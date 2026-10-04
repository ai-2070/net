#!/usr/bin/env python3
"""Stage the C SDK bundle: the headers and the one libnet built with them.

WHY THIS EXISTS. A C consumer builds against a header set and a library,
and until this script nothing produced that pair the way a consumer gets it.
Every CI build of libnet passed `--features net-ffi/test-helpers` and left the
result in the ordinary `target/release`, so the library a consumer builds
from the documented `cargo build --release -p net-ffi` was never built,
staged or checked. See docs/internal/plans/C_SDK_CONSUMER_VERIFICATION_PLAN.md
(C0).

TWO PROFILES, KEPT APART.

  production  `cargo build --release -p net-ffi` (default features), the
              documented command. Eleven shipped headers, no test seams.
  helper      the same plus `--features net-ffi/test-helpers`, and one extra
              header, `net_test_helpers.h`, from outside `include/`. Used by
              `repair.c` alone; its results are helper-build evidence.

Each profile builds in its OWN target directory (`target/c-bundle`,
`target/c-bundle-helper`) and stages under it, so nothing in the ordinary
`target/release` — where CI's helper library lives — can be picked up.

THE BUNDLE.

  net-c-sdk-<version>-<target>[-helper]/
    include/     headers, by name
    lib/         libnet.so | libnet.dylib | net.dll (+ net.dll.lib)
    EXPORTS      the staged library's export set, one symbol per line
    PROVENANCE   profile, commit, cargo command, features, toolchain, SHA-256

WHAT IT CHECKS, after staging: the header inventory is exactly the profile's;
`lib/` holds one Net implementation library (plus its import library on
Windows); the production export set carries no test seam, and the helper
export set carries the repair seams (so the profile really is the helper
build). `PROVENANCE` records the library's SHA-256, which the consumer
runner compares with the library the program actually loaded.

  python3 .github/scripts/make-c-bundle.py --profile production --build
  python3 .github/scripts/make-c-bundle.py --profile helper --build
  python3 .github/scripts/make-c-bundle.py --self-test

Without `--build` it stages an existing build from the profile's target
directory. It prints the bundle directory on its last line.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")

ROOT = Path(__file__).resolve().parents[2]
CRATE = ROOT / "net" / "crates" / "net"
INCLUDE = "net/crates/net/include"
HELPER_HEADER = CRATE / "tests" / "c_abi" / "helpers" / "net_test_helpers.h"

PROFILES = {
    "production": {
        "target_dir": "c-bundle",
        "features": [],
        "suffix": "",
    },
    "helper": {
        "target_dir": "c-bundle-helper",
        "features": ["net-ffi/test-helpers"],
        "suffix": "-helper",
    },
}

# Exports that exist only for test suites. None may ship.
SEAM_PATTERNS = [re.compile(r"_test_"), re.compile(r"^net_blob_test_barrier_")]
# The helper build must carry these, or it is not the helper build.
HELPER_REQUIRED = {
    "net_mesh_blob_adapter_test_drop_data_chunk",
    "net_mesh_blob_adapter_test_chunk_present",
}
LIB_NAMES = ("libnet.so", "libnet.dylib", "net.dll")


def _load_exports_module():
    path = Path(__file__).with_name("check-ffi-exports.py")
    spec = importlib.util.spec_from_file_location("check_ffi_exports", path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def _run(cmd: list[str], cwd: Path | None = None, env: dict | None = None) -> str:
    return subprocess.run(
        cmd, check=True, capture_output=True, text=True, cwd=cwd, env=env
    ).stdout


def cargo_command(profile: str) -> list[str]:
    cmd = ["cargo", "build", "--release", "-p", "net-ffi"]
    feats = PROFILES[profile]["features"]
    if feats:
        cmd += ["--features", ",".join(feats)]
    return cmd


def target_dir(profile: str) -> Path:
    return CRATE / "target" / PROFILES[profile]["target_dir"]


def shipped_headers() -> list[Path]:
    """The shipped header set, from git: what `include/` really ships."""
    out = _run(["git", "-C", str(ROOT), "ls-files", "--", f"{INCLUDE}/*.h"])
    return [ROOT / line for line in out.splitlines() if line.strip()]


def workspace_version() -> str:
    text = (CRATE / "Cargo.toml").read_text(encoding="utf-8")
    m = re.search(r'^\[workspace\.package\][^\[]*?^version\s*=\s*"([^"]+)"', text, re.M | re.S)
    if not m:
        m = re.search(r'^version\s*=\s*"([^"]+)"', text, re.M)
    return m.group(1) if m else "unknown"


def host_target() -> str:
    out = _run(["rustc", "-vV"], cwd=CRATE)
    return re.search(r"^host: (\S+)", out, re.M).group(1)


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def built_library(profile: str) -> Path:
    release = target_dir(profile) / "release"
    for name in LIB_NAMES:
        cand = release / name
        if cand.exists():
            return cand
    raise SystemExit(
        f"FAIL  no libnet in {release}; build it with --build "
        f"(or: CARGO_TARGET_DIR={release.parent} {' '.join(cargo_command(profile))})"
    )


def validate(
    profile: str,
    header_names: list[str],
    expected_headers: list[str],
    lib_names: list[str],
    exports: set[str],
) -> list[str]:
    """Problems with a staged bundle; empty when it is what the profile says."""
    problems: list[str] = []
    if sorted(header_names) != sorted(expected_headers):
        extra = sorted(set(header_names) - set(expected_headers))
        missing = sorted(set(expected_headers) - set(header_names))
        problems.append(f"header inventory differs: extra={extra} missing={missing}")
    impl = [n for n in lib_names if n in LIB_NAMES]
    others = [n for n in lib_names if n not in LIB_NAMES and n != "net.dll.lib"]
    if len(impl) != 1:
        problems.append(f"lib/ holds {len(impl)} Net implementation libraries: {impl}")
    if others:
        problems.append(f"lib/ holds files that are not the Net library: {others}")
    if "net.dll" in impl and "net.dll.lib" not in lib_names:
        problems.append("lib/ has net.dll but not its import library net.dll.lib")
    if not exports:
        problems.append("the staged library exports nothing")
    seams = sorted(n for n in exports if any(p.search(n) for p in SEAM_PATTERNS))
    if profile == "production" and seams:
        problems.append(f"test seams in the production export set: {seams}")
    if profile == "helper":
        missing = sorted(HELPER_REQUIRED - exports)
        if missing:
            problems.append(f"helper build lacks its seams: {missing}")
    return problems


def stage(profile: str, out: Path | None) -> Path:
    exports_mod = _load_exports_module()
    lib = built_library(profile)
    version, target = workspace_version(), host_target()
    name = f"net-c-sdk-{version}-{target}{PROFILES[profile]['suffix']}"
    bundle = out or (target_dir(profile) / "stage" / name)
    if bundle.exists():
        shutil.rmtree(bundle)
    (bundle / "include").mkdir(parents=True)
    (bundle / "lib").mkdir()

    headers = shipped_headers()
    if profile == "helper":
        headers = headers + [HELPER_HEADER]
    for h in headers:
        shutil.copy2(h, bundle / "include" / h.name)

    shutil.copy2(lib, bundle / "lib" / lib.name)
    if lib.name == "net.dll":
        implib = lib.with_name("net.dll.lib")
        if implib.exists():
            shutil.copy2(implib, bundle / "lib" / implib.name)

    staged = bundle / "lib" / lib.name
    exports = exports_mod.exports_of(staged)
    (bundle / "EXPORTS").write_text("\n".join(sorted(exports)) + "\n", encoding="utf-8")

    commit = _run(["git", "-C", str(ROOT), "rev-parse", "HEAD"]).strip()
    dirty = bool(_run(["git", "-C", str(ROOT), "status", "--porcelain", "--", "net/"]).strip())
    rustc = _run(["rustc", "-V"], cwd=CRATE).strip()
    lines = [
        f"profile: {profile}",
        f"commit: {commit}{' (net/ dirty)' if dirty else ''}",
        f"cargo: CARGO_TARGET_DIR=target/{PROFILES[profile]['target_dir']} {' '.join(cargo_command(profile))}",
        f"features: {','.join(PROFILES[profile]['features']) or '(default)'}",
        f"rustc: {rustc}",
        f"target: {target}",
        f"library: lib/{lib.name}",
        f"library-sha256: {sha256(staged)}",
        f"exports: {len(exports)}",
        f"headers: {len(headers)}",
    ]
    (bundle / "PROVENANCE").write_text("\n".join(lines) + "\n", encoding="utf-8")

    # The expected inventory is derived again from git, not from `headers`,
    # so a staging bug cannot make the bundle agree with itself.
    expected = [h.name for h in shipped_headers()]
    if profile == "helper":
        expected.append(HELPER_HEADER.name)
    problems = validate(
        profile,
        [p.name for p in (bundle / "include").iterdir()],
        expected,
        [p.name for p in (bundle / "lib").iterdir()],
        exports,
    )
    if problems:
        for p in problems:
            print(f"✗ {p}")
        raise SystemExit(1)
    print(f"✓ {profile} bundle: {len(headers)} headers, lib/{lib.name}, {len(exports)} exports")
    return bundle


def build(profile: str) -> None:
    env = dict(os.environ, CARGO_TARGET_DIR=str(target_dir(profile)))
    cmd = cargo_command(profile)
    print(f"→ CARGO_TARGET_DIR={target_dir(profile)} {' '.join(cmd)}", flush=True)
    subprocess.run(cmd, check=True, cwd=CRATE, env=env)


def self_test() -> int:
    hdrs = [f"h{i}.h" for i in range(11)]
    good = {"net_version", "net_free_string"}
    helper_exports = good | HELPER_REQUIRED
    cases = [
        ("a correct production bundle passes",
         validate("production", hdrs, hdrs, ["libnet.so"], good), False),
        ("a correct Windows bundle with its import library passes",
         validate("production", hdrs, hdrs, ["net.dll", "net.dll.lib"], good), False),
        ("a correct helper bundle passes",
         validate("helper", hdrs + ["net_test_helpers.h"], hdrs + ["net_test_helpers.h"],
                  ["libnet.so"], helper_exports), False),
        ("an extra header in the production bundle is rejected",
         validate("production", hdrs + ["net_test_helpers.h"], hdrs, ["libnet.so"], good), True),
        ("a missing header is rejected",
         validate("production", hdrs[:-1], hdrs, ["libnet.so"], good), True),
        ("a test seam in the production export set is rejected",
         validate("production", hdrs, hdrs, ["libnet.so"],
                  good | {"net_mesh_blob_adapter_test_drop_data_chunk"}), True),
        ("a barrier seam in the production export set is rejected",
         validate("production", hdrs, hdrs, ["libnet.so"],
                  good | {"net_blob_test_barrier_arm"}), True),
        ("a helper bundle without its seams is rejected",
         validate("helper", hdrs, hdrs, ["libnet.so"], good), True),
        ("two Net libraries are rejected",
         validate("production", hdrs, hdrs, ["libnet.so", "net.dll", "net.dll.lib"], good), True),
        ("a stray file in lib/ is rejected",
         validate("production", hdrs, hdrs, ["libnet.so", "libnet_org.so"], good), True),
        ("net.dll without its import library is rejected",
         validate("production", hdrs, hdrs, ["net.dll"], good), True),
        ("an empty export set is rejected",
         validate("production", hdrs, hdrs, ["libnet.so"], set()), True),
    ]
    failures = 0
    for label, problems, want_problems in cases:
        if bool(problems) == want_problems:
            print(f"✓ self-test: {label}")
        else:
            print(f"✗ self-test: {label} (problems={problems})")
            failures += 1
    with tempfile.TemporaryDirectory() as tmp:
        p = Path(tmp) / "f"
        p.write_bytes(b"abc")
        if sha256(p) == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad":
            print("✓ self-test: SHA-256 of a known input")
        else:
            print("✗ self-test: SHA-256 of a known input")
            failures += 1
    return 1 if failures else 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--profile", choices=sorted(PROFILES), default="production")
    ap.add_argument("--build", action="store_true", help="build the profile's library first")
    ap.add_argument("--out", type=Path, help="bundle directory (default: under the profile's target dir)")
    ap.add_argument("--self-test", action="store_true", help="plant bundle defects and require rejection")
    args = ap.parse_args()
    if args.self_test:
        return self_test()
    if args.build:
        build(args.profile)
    bundle = stage(args.profile, args.out)
    print(bundle)
    return 0


if __name__ == "__main__":
    sys.exit(main())
