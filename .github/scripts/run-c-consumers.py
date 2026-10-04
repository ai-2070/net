#!/usr/bin/env python3
"""Build C consumer programs against a staged C SDK bundle, run them, and
prove which library each one actually loaded.

WHY THIS EXISTS. A C consumer's evidence is only as good as the pair it ran
against. Compiling with `-I bundle/include -L bundle/lib` says nothing about
what the loader picked at run time: Windows searches the application's
directory before PATH, and ELF loading follows DT_RPATH/DT_RUNPATH, explicit
dependency paths and LD_PRELOAD, not only LD_LIBRARY_PATH. A log could print
the production bundle's PROVENANCE while the program ran against another
libnet. See docs/internal/plans/C_SDK_CONSUMER_VERIFICATION_PLAN.md,
"Loaded-library identity".

WHAT A RUN PROVES. For each program in `net/crates/net/examples/c/`:

  1. It compiles against the bundle only: headers by name from
     `bundle/include`, the library from `bundle/lib`, `-Werror`.
  2. It runs in an environment this script builds, whose loader path is
     `bundle/lib` alone. If that environment (or the program's own
     directory) can still reach another Net library, the run is refused
     before it starts. That is setup, not proof.
  3. The proof: the program's first act (support/loaded_module.c) finds the
     module its Net imports resolved to and prints it. This script requires
     that path to be the staged library and its SHA-256 to equal the one in
     the bundle's PROVENANCE. The program also refuses a wrong module itself,
     before calling into it, from NET_EXPECTED_MODULE.
  4. It exits 0 with at least one named check (`NET-CHECKS: <n>`).

NEGATIVE CONTROLS (`--negative-controls --shadow-library <other libnet>`).
Each must fail the identity check by name, before any Net call:

  shadowing      the other library where the loader prefers it: beside the
                 executable on Windows, a DT_RPATH entry on Linux. Must fail
                 on the loaded path.
  interposition  (Linux) an LD_PRELOAD library defining `net_version`. Must
                 fail with the function resolving outside the staged module;
                 the interposed function is never called.

  python3 .github/scripts/run-c-consumers.py --bundle <dir>
  python3 .github/scripts/run-c-consumers.py --bundle <dir> --negative-controls \\
      --shadow-library <other bundle>/lib/libnet.so
  python3 .github/scripts/run-c-consumers.py --self-test

No retries: a flake is a defect to fix, and every run is bounded by
`--timeout`.
"""

from __future__ import annotations

import argparse
import hashlib
import os
import re
import shutil
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path

if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")

ROOT = Path(__file__).resolve().parents[2]
EXAMPLES = ROOT / "net" / "crates" / "net" / "examples" / "c"
SUPPORT = EXAMPLES / "support"
FLOORS = EXAMPLES / "FLOORS"
LIB_NAMES = ("libnet.so", "libnet.dylib", "net.dll")
IS_WINDOWS = os.name == "nt"

MODULE_RE = re.compile(r"^NET-LOADED-MODULE: (.+?)\s*$", re.M)
MODULE_ERR_RE = re.compile(r"^NET-LOADED-MODULE-ERROR: (.+?)\s*$", re.M)
CHECKS_RE = re.compile(r"^NET-CHECKS: (\d+)\s*$", re.M)
OK_LINE_RE = re.compile(r"^ok ", re.M)


# ---------------------------------------------------------------- bundle ----


@dataclass
class Bundle:
    root: Path
    library: Path
    sha256: str
    profile: str

    @property
    def include(self) -> Path:
        return self.root / "include"

    @property
    def lib(self) -> Path:
        return self.root / "lib"


def read_provenance(root: Path) -> dict[str, str]:
    out = {}
    for line in (root / "PROVENANCE").read_text(encoding="utf-8").splitlines():
        if ": " in line:
            k, v = line.split(": ", 1)
            out[k.strip()] = v.strip()
    return out


def load_bundle(root: Path) -> Bundle:
    root = root.resolve()
    prov = read_provenance(root)
    for key in ("library", "library-sha256", "profile"):
        if key not in prov:
            raise SystemExit(f"FAIL  {root}/PROVENANCE has no `{key}`")
    lib = root / prov["library"]
    if not lib.exists():
        raise SystemExit(f"FAIL  {lib} (named by PROVENANCE) does not exist")
    return Bundle(root, lib, prov["library-sha256"], prov["profile"])


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def same_path(a: str | Path, b: str | Path) -> bool:
    return os.path.normcase(os.path.realpath(a)) == os.path.normcase(os.path.realpath(b))


# ----------------------------------------------------------- environment ----


def child_env(bundle: Bundle, extra: dict[str, str] | None = None) -> dict[str, str]:
    """The environment a consumer runs in: loader path = bundle/lib alone."""
    env = {k: v for k, v in os.environ.items() if k not in ("LD_PRELOAD", "LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH")}
    if IS_WINDOWS:
        sysroot = os.environ.get("SystemRoot", r"C:\Windows")
        env["PATH"] = os.pathsep.join(
            [str(bundle.lib), os.path.join(sysroot, "System32"), sysroot]
        )
    else:
        env["LD_LIBRARY_PATH"] = str(bundle.lib)
    env["NET_EXPECTED_MODULE"] = os.path.realpath(bundle.library)
    if extra:
        env.update(extra)
    return env


def loader_dirs(env: dict[str, str]) -> list[str]:
    key = "PATH" if IS_WINDOWS else "LD_LIBRARY_PATH"
    return [d for d in env.get(key, "").split(os.pathsep) if d]


def stray_libraries(dirs: list[str], allowed: Path) -> list[str]:
    """Net libraries reachable from `dirs` other than in `allowed`."""
    found = []
    for d in dirs:
        if same_path(d, allowed):
            continue
        for name in LIB_NAMES:
            p = Path(d) / name
            if p.is_file():
                found.append(str(p))
    return found


# --------------------------------------------------------------- compile ----


def _vcvars() -> Path | None:
    vswhere = Path(os.environ.get("ProgramFiles(x86)", r"C:\Program Files (x86)")) / (
        "Microsoft Visual Studio/Installer/vswhere.exe"
    )
    if not vswhere.exists():
        return None
    out = subprocess.run(
        [str(vswhere), "-latest", "-products", "*", "-property", "installationPath"],
        capture_output=True, text=True,
    ).stdout.strip()
    bat = Path(out) / "VC" / "Auxiliary" / "Build" / "vcvars64.bat"
    return bat if out and bat.exists() else None


def pick_compiler(requested: str) -> str:
    if requested != "auto":
        return requested
    if not IS_WINDOWS:
        return "gcc"
    return "msvc" if _vcvars() else "mingw"


def compile_program(
    compiler: str, bundle: Bundle, src: Path, out_dir: Path, extra_link: list[str] | None = None
) -> Path:
    out_dir.mkdir(parents=True, exist_ok=True)
    exe = out_dir / (src.stem + (".exe" if IS_WINDOWS else ""))
    # Every support file: the loaded-module check and the shared utilities.
    support = [str(p) for p in sorted(SUPPORT.glob("*.c"))]
    extra_link = extra_link or []
    if compiler == "gcc":
        cc = os.environ.get("CC", "cc")
        cmd = [
            cc, "-std=c11", "-Wall", "-Wextra", "-Werror", "-fPIE", "-pie",
            "-I", str(bundle.include), "-I", str(SUPPORT),
            str(src), *support,
            "-L", str(bundle.lib), "-lnet", "-ldl", "-lpthread", "-lm",
            *extra_link, "-o", str(exe),
        ]
        run = subprocess.run(cmd, capture_output=True, text=True)
    elif compiler == "mingw":
        cc = os.environ.get("CC", "gcc")
        # Link the DLL itself (GNU ld supports it), and link the GCC runtime
        # statically so the minimal child PATH needs nothing but bundle/lib.
        cmd = [
            cc, "-std=c11", "-Wall", "-Wextra", "-Werror",
            "-I", str(bundle.include), "-I", str(SUPPORT),
            str(src), *support, str(bundle.library),
            "-static", "-lws2_32", *extra_link, "-o", str(exe),
        ]
        run = subprocess.run(cmd, capture_output=True, text=True)
    elif compiler == "msvc":
        bat = _vcvars()
        if bat is None:
            raise SystemExit("FAIL  MSVC requested but vcvars64.bat was not found")
        implib = bundle.lib / "net.dll.lib"
        quoted_support = " ".join('"' + x + '"' for x in support)
        cl = (
            f'cl /nologo /W4 /WX /std:c11 /MD /I "{bundle.include}" /I "{SUPPORT}" '
            f'"{src}" {quoted_support} /Fo"{out_dir}\\\\" /Fe"{exe}" '
            f'/link "{implib}" ws2_32.lib {" ".join(extra_link)}'
        )
        run = subprocess.run(
            f'call "{bat}" >nul && {cl}', shell=True, capture_output=True, text=True
        )
        cmd = [cl]
    else:
        raise SystemExit(f"FAIL  unknown compiler {compiler}")
    if run.returncode != 0:
        print(f"✗ {src.name}: compile failed ({compiler})")
        print("    " + " ".join(cmd))
        sys.stdout.write(_indent(run.stdout + run.stderr))
        raise SystemExit(1)
    return exe


def _indent(text: str) -> str:
    return "".join(f"      {line}\n" for line in text.splitlines())


# ------------------------------------------------------------------- run ----


@dataclass
class Verdict:
    kind: str  # "ok" | "identity" | "failed"
    reason: str
    module: str | None = None
    checks: int = 0
    calls_made: bool = False


def classify(stdout: str, rc: int, expected_lib: Path, expected_sha: str, hash_of=sha256) -> Verdict:
    """Judge one run. Identity failures are their own verdict, so a negative
    control can require exactly that and not merely any failure."""
    m = MODULE_RE.search(stdout)
    module = m.group(1) if m else None
    calls = bool(OK_LINE_RE.search(stdout))
    err = MODULE_ERR_RE.search(stdout)
    if err:
        return Verdict("identity", err.group(1), module, 0, calls)
    if module is None:
        return Verdict("failed", f"no NET-LOADED-MODULE line (exit {rc})", None, 0, calls)
    if not same_path(module, expected_lib):
        return Verdict("identity", f"loaded {module}, expected {expected_lib}", module, 0, calls)
    actual = hash_of(Path(module))
    if actual != expected_sha:
        return Verdict("identity", f"{module} SHA-256 {actual[:12]}… != PROVENANCE {expected_sha[:12]}…", module, 0, calls)
    if rc != 0:
        return Verdict("failed", f"exit {rc}", module, 0, calls)
    c = CHECKS_RE.search(stdout)
    if not c or int(c.group(1)) < 1:
        return Verdict("failed", "no NET-CHECKS line, or zero checks", module, 0, calls)
    return Verdict("ok", "", module, int(c.group(1)), calls)


def run_exe(exe: Path, env: dict[str, str], timeout: int) -> tuple[int, str]:
    try:
        p = subprocess.run(
            [str(exe)], env=env, capture_output=True, text=True, timeout=timeout,
            cwd=str(exe.parent),
        )
    except subprocess.TimeoutExpired as e:
        out = (e.stdout or b"").decode() if isinstance(e.stdout, bytes) else (e.stdout or "")
        return 124, out + f"\n(timed out after {timeout}s)\n"
    return p.returncode, p.stdout + p.stderr


def refuse_strays(bundle: Bundle, env: dict[str, str], app_dir: Path) -> None:
    strays = stray_libraries(loader_dirs(env), bundle.lib)
    if IS_WINDOWS:
        strays += stray_libraries([str(app_dir)], bundle.lib)
    if strays:
        print("✗ refusing to run: another Net library is reachable from the run environment:")
        for s in strays:
            print(f"    {s}")
        raise SystemExit(1)


def read_floors(path: Path = FLOORS) -> dict[str, int]:
    """program -> minimum named checks (the roster: a check that silently
    stops running shows as a lower count)."""
    out = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.split("#", 1)[0].strip()
        if line:
            name, n = line.split()
            out[name] = int(n)
    return out


BUNDLE_MARK = re.compile(r"NET-BUNDLE:\s*(\w+)")


def required_profile(src: Path) -> str | None:
    """The bundle profile a program needs, from a `NET-BUNDLE: <profile>`
    line in its header comment (repair.c needs the helper build's seams).
    None: any bundle."""
    m = BUNDLE_MARK.search(src.read_text(encoding="utf-8")[:4000])
    return m.group(1) if m else None


def floor_problems(programs: list[str], floors: dict[str, int]) -> list[str]:
    problems = [f"{p} has no line in examples/c/FLOORS" for p in programs if p not in floors]
    problems += [f"examples/c/FLOORS names {p}, which is not a program" for p in floors if p not in programs]
    return problems


def run_programs(args, bundle: Bundle, compiler: str, work: Path) -> int:
    every = sorted(p.stem for p in EXAMPLES.glob("*.c"))
    floors = read_floors()
    problems = floor_problems(every, floors)
    for p in problems:
        print(f"✗ {p}")
    names = args.program or every
    failures = len(problems)
    for name in names:
        src = EXAMPLES / f"{name}.c"
        if not src.exists():
            print(f"✗ {name}: no such program {src}")
            failures += 1
            continue
        need = required_profile(src)
        if need is not None and need != bundle.profile:
            print(f"  – {name}: needs the {need} bundle; not run against the {bundle.profile} bundle")
            continue
        exe = compile_program(compiler, bundle, src, work / name)
        env = child_env(bundle)
        refuse_strays(bundle, env, exe.parent)
        rc, out = run_exe(exe, env, args.timeout)
        v = classify(out, rc, bundle.library, bundle.sha256)
        if v.kind == "ok" and v.checks < floors.get(name, 0):
            print(f"✗ {name}: {v.checks} named checks, below its floor of {floors[name]} "
                  "(examples/c/FLOORS) — a check stopped running")
            sys.stdout.write(_indent(out))
            failures += 1
        elif v.kind == "ok":
            print(f"  ▶ {name}: {v.checks} named checks (floor {floors.get(name, 0)}); "
                  f"loaded {v.module} (SHA-256 matches PROVENANCE)")
        else:
            print(f"✗ {name}: {v.kind}: {v.reason}")
            sys.stdout.write(_indent(out))
            failures += 1
    return failures


def negative_controls(args, bundle: Bundle, compiler: str, work: Path) -> int:
    shadow = args.shadow_library.resolve()
    if not shadow.is_file():
        print(f"✗ --shadow-library {shadow} does not exist")
        return 1
    if same_path(shadow, bundle.library) or sha256(shadow) == bundle.sha256:
        print(f"✗ --shadow-library must be a different library from the bundle's ({shadow})")
        return 1
    src = EXAMPLES / "smoke.c"
    failures = 0

    def expect_identity(label: str, rc: int, out: str, must_mention: str) -> None:
        nonlocal failures
        v = classify(out, rc, bundle.library, bundle.sha256)
        if v.kind == "identity" and must_mention in v.reason and not v.calls_made:
            print(f"  ▶ negative control: {label}: refused before any Net call ({v.reason})")
        else:
            print(f"✗ negative control: {label}: expected an identity refusal naming "
                  f"{must_mention!r} before any call; got {v.kind} ({v.reason}), calls_made={v.calls_made}")
            sys.stdout.write(_indent(out))
            failures += 1

    # Shadowing: the other library where the loader prefers it.
    shadow_dir = work / "neg-shadow"
    if shadow_dir.exists():
        shutil.rmtree(shadow_dir)
    if IS_WINDOWS:
        exe = compile_program(compiler, bundle, src, shadow_dir)
        planted = shadow_dir / "net.dll"
        shutil.copy2(shadow, planted)
    else:
        libdir = shadow_dir / "rpath"
        libdir.mkdir(parents=True)
        planted = libdir / "libnet.so"
        shutil.copy2(shadow, planted)
        exe = compile_program(
            compiler, bundle, src, shadow_dir,
            ["-Wl,--disable-new-dtags", f"-Wl,-rpath,{libdir}"],
        )
    rc, out = run_exe(exe, child_env(bundle), args.timeout)
    expect_identity("shadowing library beside the program" if IS_WINDOWS else "DT_RPATH shadowing library",
                    rc, out, "expected")

    # Interposition (ELF only): LD_PRELOAD a definition of net_version.
    if not IS_WINDOWS:
        idir = work / "neg-interpose"
        idir.mkdir(parents=True, exist_ok=True)
        isrc = idir / "interposer.c"
        isrc.write_text(
            "const char* net_version(void);\n"
            'const char* net_version(void) { return "interposed"; }\n',
            encoding="utf-8",
        )
        iso = idir / "libinterposer.so"
        cc = os.environ.get("CC", "cc")
        subprocess.run([cc, "-shared", "-fPIC", "-o", str(iso), str(isrc)], check=True)
        exe = compile_program(compiler, bundle, src, idir / "bin")
        rc, out = run_exe(exe, child_env(bundle, {"LD_PRELOAD": str(iso)}), args.timeout)
        expect_identity("LD_PRELOAD interposer of net_version", rc, out, "libinterposer.so")
        if "net_version: interposed" in out:
            print("✗ negative control: the interposed net_version was called")
            failures += 1
    return failures


# ------------------------------------------------------------- self-test ----


def self_test() -> int:
    failures = 0
    with tempfile.TemporaryDirectory() as tmp:
        t = Path(tmp)
        good = t / "bundle" / "lib" / LIB_NAMES[2 if IS_WINDOWS else 0]
        good.parent.mkdir(parents=True)
        good.write_bytes(b"good")
        other = t / "elsewhere" / good.name
        other.parent.mkdir()
        other.write_bytes(b"other")
        gsha = sha256(good)

        def case(label, verdict, kind, mention=None, calls=None):
            nonlocal failures
            ok = verdict.kind == kind and (mention is None or mention in verdict.reason) and (
                calls is None or verdict.calls_made == calls)
            print(f"{'✓' if ok else '✗'} self-test: {label}" + ("" if ok else f" (got {verdict})"))
            failures += 0 if ok else 1

        case("a correct run passes",
             classify(f"NET-LOADED-MODULE: {good}\nok a\nNET-CHECKS: 1\n", 0, good, gsha), "ok")
        case("another module's path is an identity failure",
             classify(f"NET-LOADED-MODULE: {other}\nok a\nNET-CHECKS: 1\n", 0, good, gsha), "identity", "expected")
        case("the right path with the wrong content is an identity failure",
             classify(f"NET-LOADED-MODULE: {good}\nNET-CHECKS: 1\n", 0, good, "0" * 64), "identity", "SHA-256")
        case("the program's own refusal is an identity failure, before calls",
             classify(f"NET-LOADED-MODULE: {other}\nNET-LOADED-MODULE-ERROR: -: loaded x, expected y\n", 1, good, gsha),
             "identity", "expected", calls=False)
        case("a missing module line fails (not ok)",
             classify("ok a\nNET-CHECKS: 1\n", 0, good, gsha), "failed")
        case("zero named checks fails",
             classify(f"NET-LOADED-MODULE: {good}\nNET-CHECKS: 0\n", 0, good, gsha), "failed")
        case("a non-zero exit fails",
             classify(f"NET-LOADED-MODULE: {good}\nNET-CHECKS: 3\n", 1, good, gsha), "failed")
        case("a call after a wrong module is recorded",
             classify(f"NET-LOADED-MODULE: {other}\nok a\n", 0, good, gsha), "identity", calls=True)

        ok = floor_problems(["a", "b"], {"a": 1}) == ["b has no line in examples/c/FLOORS"] and             floor_problems(["a"], {"a": 1, "gone": 3}) == ["examples/c/FLOORS names gone, which is not a program"]
        print(f"{'✓' if ok else '✗'} self-test: a program without a floor, and a floor without a program, are refused")
        failures += 0 if ok else 1

        strays = stray_libraries([str(other.parent), str(good.parent)], good.parent)
        ok = strays == [str(other)]
        print(f"{'✓' if ok else '✗'} self-test: a Net library outside bundle/lib on the loader path is found")
        failures += 0 if ok else 1
        ok = stray_libraries([str(good.parent)], good.parent) == []
        print(f"{'✓' if ok else '✗'} self-test: bundle/lib itself is not a stray")
        failures += 0 if ok else 1
    return 1 if failures else 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--bundle", type=Path, help="staged bundle directory (from make-c-bundle.py)")
    ap.add_argument("--program", action="append", help="program name in examples/c (default: all)")
    ap.add_argument("--compiler", default="auto", choices=["auto", "gcc", "msvc", "mingw"])
    ap.add_argument("--work", type=Path, help="build directory (default: <bundle>/../consumers-<compiler>)")
    ap.add_argument("--timeout", type=int, default=120, help="seconds per run")
    ap.add_argument("--negative-controls", action="store_true", help="also run the identity negative controls")
    ap.add_argument("--shadow-library", type=Path, help="a different libnet, for the shadowing control")
    ap.add_argument("--self-test", action="store_true")
    args = ap.parse_args()

    if args.self_test:
        return self_test()
    if not args.bundle:
        ap.error("--bundle is required")
    if args.negative_controls and not args.shadow_library:
        ap.error("--negative-controls needs --shadow-library")

    bundle = load_bundle(args.bundle)
    if sha256(bundle.library) != bundle.sha256:
        print(f"✗ {bundle.library} does not match its own PROVENANCE SHA-256")
        return 1
    compiler = pick_compiler(args.compiler)
    work = (args.work or bundle.root.parent / f"consumers-{compiler}").resolve()
    print(f"==> {bundle.profile} bundle {bundle.root.name}, compiler {compiler}")

    failures = run_programs(args, bundle, compiler, work)
    if args.negative_controls:
        failures += negative_controls(args, bundle, compiler, work)
    if failures:
        print(f"✗ {failures} failure(s)")
        return 1
    print("✓ all consumer runs passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
