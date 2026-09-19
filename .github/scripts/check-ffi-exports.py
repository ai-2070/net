#!/usr/bin/env python3
"""The single libnet cdylib exports exactly the pinned symbol set.

WHY THIS EXISTS. The browser-native WebRTC plan
(`docs/internal/plans/BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md`, Stage 1)
generalizes every peer endpoint in the core from `SocketAddr` to `PeerAddr`
and threads a submission seam under ~50 send sites. Its compatibility
guarantee is that the default build's exported C-ABI symbol set is
unchanged — every Go, Python and C consumer links `libnet` and nothing
else, so a symbol that appears or disappears there is a break for all of
them at once. That guarantee was unfalsifiable until this check existed:
nothing compared the artifact against anything. The plan's authorization
made this job the first Stage 1 commit, pinned to the pre-refactor head,
so the criterion can actually fail.

WHAT IT PROVES, AND WHAT IT DOES NOT. The set of names the built cdylib
exports equals the set recorded in the baseline file. It says nothing about
signatures, calling convention, struct layout or behaviour — the Go
`abi_stability_*_test.go` suite and the binding tests own those. An
unchanged symbol set complements them; it does not replace them.

WHAT IT READS. The real export table of the real artifact — `libnet.so`
(ELF dynamic symbols), `net.dll` (COFF export directory) or `libnet.dylib`
(Mach-O external symbols, leading underscore stripped) — never a Rust
library or a source scan. A Rust cdylib exports exactly its `#[no_mangle]`
surface on every platform, so one baseline of bare names serves all three;
if a platform ever adds a runtime export the diff names it and the baseline
header says where it was generated.

  python3 .github/scripts/check-ffi-exports.py --artifact <path>   # check
  python3 .github/scripts/check-ffi-exports.py --artifact <path> --update
  python3 .github/scripts/check-ffi-exports.py --self-test          # plant defects

The baseline lives beside the crate it describes:
`net/crates/net/bindings/go/net-ffi/exports.baseline`. Regenerate it with
`--update` only when an export change is intended, and say so in the commit
that does it.
"""

from __future__ import annotations

import argparse
import os
import platform
import re
import shutil
import subprocess
import sys
from pathlib import Path

if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")

ROOT = Path(__file__).resolve().parents[2]
CRATE = ROOT / "net" / "crates" / "net"
BASELINE = CRATE / "bindings" / "go" / "net-ffi" / "exports.baseline"
CANDIDATES = [
    CRATE / "target" / "release" / "libnet.so",
    CRATE / "target" / "release" / "net.dll",
    CRATE / "target" / "release" / "libnet.dylib",
]


def _run(cmd: list[str], cwd: Path | None = None) -> str:
    return subprocess.run(cmd, check=True, capture_output=True, text=True, cwd=cwd).stdout


def _llvm_tool(name: str) -> str | None:
    """`llvm-nm` / `llvm-readobj` from the pinned toolchain's llvm-tools, else PATH.

    `rustc` is resolved from the crate directory so `rust-toolchain.toml`'s
    pin (with its `llvm-tools-preview` component) applies, not whatever
    toolchain the repository root happens to default to.
    """
    try:
        sysroot = _run(["rustc", "--print", "sysroot"], cwd=CRATE).strip()
        host = _run(["rustc", "-vV"], cwd=CRATE)
        target = re.search(r"^host: (\S+)", host, re.M).group(1)
        exe = Path(sysroot) / "lib" / "rustlib" / target / "bin" / name
        for cand in (exe, exe.with_suffix(".exe")):
            if cand.exists():
                return str(cand)
    except (subprocess.CalledProcessError, FileNotFoundError, AttributeError):
        pass
    return shutil.which(name)


def _magic(path: Path) -> str:
    head = path.read_bytes()[:4]
    if head.startswith(b"\x7fELF"):
        return "elf"
    if head.startswith(b"MZ"):
        return "pe"
    if head[:4] in (b"\xcf\xfa\xed\xfe", b"\xfe\xed\xfa\xcf", b"\xca\xfe\xba\xbe"):
        return "macho"
    raise SystemExit(f"FAIL  {path}: not an ELF, PE or Mach-O artifact")


def exports_of(path: Path) -> set[str]:
    """Names the artifact exports, read from its real export table."""
    kind = _magic(path)
    if kind == "elf":
        # `nm -D --defined-only`: defined symbols in the dynamic table — exactly
        # what a consumer can link against. Same command ci.yml already uses to
        # count `net_<surface>_` exports.
        tool = shutil.which("nm") or _llvm_tool("llvm-nm")
        if tool is None:
            raise SystemExit("FAIL  neither `nm` nor `llvm-nm` is available")
        flag = "-D" if os.path.basename(tool).startswith("nm") else "--dynamic"
        out = _run([tool, flag, "--defined-only", str(path)])
        names = set()
        for line in out.splitlines():
            parts = line.split()
            if len(parts) >= 3 and parts[-2] in "TtDdRrBbWwVvi":
                names.add(parts[-1])
        return names
    if kind == "pe":
        tool = _llvm_tool("llvm-readobj")
        if tool is not None:
            out = _run([tool, "--coff-exports", str(path)])
            return {m.group(1) for m in re.finditer(r"^\s*Name:\s+(\S+)$", out, re.M)}
        dumpbin = shutil.which("dumpbin")
        if dumpbin is None:
            raise SystemExit("FAIL  neither `llvm-readobj` nor `dumpbin` is available")
        out = _run([dumpbin, "/EXPORTS", str(path)])
        # `ordinal hint RVA name` rows follow the header; name is the 4th column.
        return {
            m.group(1)
            for m in re.finditer(r"^\s+\d+\s+[0-9A-Fa-f]+\s+[0-9A-Fa-f]{8}\s+(\S+)", out, re.M)
        }
    tool = shutil.which("nm") or _llvm_tool("llvm-nm")
    if tool is None:
        raise SystemExit("FAIL  neither `nm` nor `llvm-nm` is available")
    out = _run([tool, "-gU", str(path)])
    names = set()
    for line in out.splitlines():
        parts = line.split()
        if len(parts) >= 3:
            names.add(parts[-1][1:] if parts[-1].startswith("_") else parts[-1])
    return names


def read_baseline(path: Path) -> tuple[list[str], set[str]]:
    header, names = [], set()
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line:
            continue
        if line.startswith("#"):
            header.append(line)
        else:
            names.add(line)
    return header, names


def compare(baseline: set[str], current: set[str]) -> tuple[list[str], list[str]]:
    """(added, removed) relative to the baseline, each sorted."""
    return sorted(current - baseline), sorted(baseline - current)


def report(added: list[str], removed: list[str], artifact: str) -> bool:
    if not added and not removed:
        print(f"✓ {artifact}: export set matches the baseline")
        return True
    if added:
        print(f"✗ {artifact}: {len(added)} export(s) NOT in the baseline (added):")
        for n in added:
            print(f"    + {n}")
    if removed:
        print(f"✗ {artifact}: {len(removed)} baseline export(s) missing (removed):")
        for n in removed:
            print(f"    - {n}")
    print(
        "\n  A C-ABI export changed. If that is intended, regenerate the baseline\n"
        "  with `--update` in the same commit and say why; if not, the change\n"
        "  broke every libnet consumer at once."
    )
    return False


def write_baseline(path: Path, names: set[str], artifact: Path, pin: str | None) -> None:
    sha = "unknown"
    try:
        sha = _run(["git", "-C", str(ROOT), "rev-parse", "HEAD"]).strip()
        if pin:
            # The baseline may only claim a pin if the C-ABI sources at HEAD are
            # the pinned ones: anything under net/ differing means the artifact
            # was built from something else and the header would lie.
            pin = _run(["git", "-C", str(ROOT), "rev-parse", pin]).strip()
            drift = _run(["git", "-C", str(ROOT), "diff", "--stat", f"{pin}..HEAD", "--", "net/"]).strip()
            if drift:
                raise SystemExit(
                    f"FAIL  --pin {pin[:9]}: net/ differs between the pin and HEAD; "
                    f"build at the pin or drop --pin\n{drift}"
                )
    except (subprocess.CalledProcessError, FileNotFoundError):
        pass
    header = [
        "# Exported symbol set of the single libnet cdylib (bindings/go/net-ffi).",
        "# Checked by .github/scripts/check-ffi-exports.py; regenerate with --update",
        "# ONLY for an intended C-ABI change, and say so in that commit.",
        f"# generated-at: {sha}",
        *( [f"# pinned-to: {pin} (net/ identical between pin and generated-at)"] if pin else [] ),
        f"# artifact: {artifact.name} ({_magic(artifact)}, {platform.system()} {platform.machine()})",
        "# build: cargo build --release -p net-ffi --features net-ffi/test-helpers",
        f"# count: {len(names)}",
    ]
    path.write_text("\n".join(header + sorted(names)) + "\n", encoding="utf-8")
    print(f"✓ wrote {path.relative_to(ROOT)} ({len(names)} exports) at {sha[:9]}")


def self_test(baseline_path: Path) -> int:
    """Plant an added and a removed export; the checker must reject each."""
    _, names = read_baseline(baseline_path)
    if len(names) < 2:
        print("✗ self-test: baseline has fewer than two names; cannot plant defects")
        return 1
    failures = 0

    added, removed = compare(names, names)
    if added or removed:
        print("✗ self-test: identical sets were reported as different")
        failures += 1
    else:
        print("✓ self-test: identical sets pass")

    planted = set(names) | {"net_stage1_planted_export"}
    added, removed = compare(names, planted)
    if added == ["net_stage1_planted_export"] and not removed:
        print("✓ self-test: an added export is rejected")
    else:
        print(f"✗ self-test: added export not caught (added={added}, removed={removed})")
        failures += 1

    victim = sorted(names)[0]
    added, removed = compare(names, set(names) - {victim})
    if removed == [victim] and not added:
        print(f"✓ self-test: a removed export is rejected ({victim})")
    else:
        print(f"✗ self-test: removed export not caught (added={added}, removed={removed})")
        failures += 1

    both = (set(names) - {victim}) | {"net_stage1_planted_export"}
    added, removed = compare(names, both)
    if added and removed:
        print("✓ self-test: a rename (one added, one removed) is rejected")
    else:
        print("✗ self-test: rename not caught")
        failures += 1

    return 1 if failures else 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--artifact", type=Path, help="built cdylib; default: first of target/release/{libnet.so,net.dll,libnet.dylib}")
    ap.add_argument("--baseline", type=Path, default=BASELINE)
    ap.add_argument("--update", action="store_true", help="rewrite the baseline from the artifact")
    ap.add_argument("--pin", help="with --update: record this SHA as the baseline's pin; refused if net/ differs from HEAD")
    ap.add_argument("--self-test", action="store_true", help="plant an added and a removed export and require rejection")
    args = ap.parse_args()

    if args.self_test:
        return self_test(args.baseline)

    artifact = args.artifact or next((c for c in CANDIDATES if c.exists()), None)
    if artifact is None or not artifact.exists():
        print("✗ no libnet artifact found; build it first:\n"
              "    (cd net/crates/net && cargo build --release -p net-ffi --features net-ffi/test-helpers)")
        return 1

    current = exports_of(artifact)
    if not current:
        print(f"✗ {artifact}: no exports read — wrong artifact or unsupported tool output")
        return 1

    if args.update:
        write_baseline(args.baseline, current, artifact, args.pin)
        return 0

    if not args.baseline.exists():
        print(f"✗ baseline missing: {args.baseline}; create it with --update at the pinned SHA")
        return 1
    header, names = read_baseline(args.baseline)
    for line in header:
        if line.split(":")[0] in ("# generated-at", "# pinned-to", "# count"):
            print(f"  baseline {line[2:]}")
    added, removed = compare(names, current)
    return 0 if report(added, removed, artifact.name) else 1


if __name__ == "__main__":
    sys.exit(main())
