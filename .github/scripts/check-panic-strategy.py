#!/usr/bin/env python3
"""Prove what panic strategy a built Python extension actually has.

`cargo build -v` shows what was *requested*. This shows what the loaded
`.pyd`/`.so` *does*, which is the thing that matters — and the thing
that could not be established otherwise when a runtime-drop panic was
seen on a `--release` wheel and the process survived, which an effective
abort profile cannot do. That observation is still unexplained; this
script exists so the question is answerable by construction next time
rather than by inference.

Two artifacts, each built with the `panic-probe` feature, which compiles
one deliberately panicking export:

    abort control    maturin build --release ...
      the child must terminate abnormally, and nothing after the probe
      call may run

    unwind control   maturin build --profile python-release ...
      pyo3 must convert the panic to an exception the child catches; the
      child prints a sentinel and exits 0

The probe is always invoked in a **child** process. Under abort it takes
the interpreter with it, so a harness calling it in-process could not
report its own result.

Deliberately not pinned to a native status code: abort surfaces as
0xC0000409 on Windows and as SIGABRT elsewhere. The assertion is
"terminated abnormally", which is the portable form of the claim.

Usage:
    check-panic-strategy.py --wheel <path> --expect abort|unwind
"""

from __future__ import annotations

import argparse
import subprocess
import sys
import tempfile
import venv
from pathlib import Path

#: Printed by the child only if it survived the probe and caught it.
SENTINEL = "PROBE_SURVIVED_AND_CAUGHT"

#: Runs in the child. Importing and calling the probe is the whole test;
#: everything else is reporting.
CHILD = f"""
import sys
import net._net as native

if not hasattr(native, "_panic_strategy_probe"):
    print("NO_PROBE", flush=True)
    sys.exit(3)

try:
    native._panic_strategy_probe()
except BaseException as exc:            # noqa: BLE001 — any containment counts
    # Reached only under unwind: pyo3 turned the panic into an
    # exception. Print AFTER catching, so the line itself is evidence
    # the process kept running.
    print("{SENTINEL}", type(exc).__name__, flush=True)
    sys.exit(0)

# Reached only if the call returned normally, which the probe must never
# do — it panics unconditionally.
print("PROBE_RETURNED_NORMALLY", flush=True)
sys.exit(4)
"""


def build_env(wheel: Path) -> Path:
    """A throwaway venv with only this wheel installed."""
    env_dir = Path(tempfile.mkdtemp(prefix="panic-probe-"))
    venv.create(env_dir, with_pip=True)
    py = env_dir / ("Scripts" if sys.platform == "win32" else "bin") / (
        "python.exe" if sys.platform == "win32" else "python"
    )
    subprocess.run(
        [str(py), "-m", "pip", "install", "--quiet", str(wheel)],
        check=True,
    )
    return py


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--wheel", required=True, type=Path)
    ap.add_argument("--expect", required=True, choices=("abort", "unwind"))
    args = ap.parse_args()

    if not args.wheel.is_file():
        print(f"FAIL  no wheel at {args.wheel}")
        return 1

    py = build_env(args.wheel)
    # `cwd` away from the repo so `import net` cannot resolve to a source
    # directory instead of the installed extension.
    proc = subprocess.run(
        [str(py), "-c", CHILD],
        capture_output=True,
        text=True,
        cwd=tempfile.gettempdir(),
    )
    out = (proc.stdout or "").strip()
    err = (proc.stderr or "").strip()
    code = proc.returncode

    print(f"child exit={code} (0x{code & 0xFFFFFFFF:08X})")
    if out:
        print(f"child stdout: {out}")

    if "NO_PROBE" in out:
        print(
            "FAIL  the wheel has no `_panic_strategy_probe`. Build it with the "
            "`panic-probe` feature; this script cannot test a wheel that has "
            "nothing to trigger."
        )
        return 1

    if args.expect == "unwind":
        if code != 0 or SENTINEL not in out:
            print(
                "FAIL  expected unwind: pyo3 should have converted the panic to "
                "an exception, the child should have caught it, printed the "
                "sentinel and exited 0.\\n"
                "      Getting an abnormal exit here means the artifact is "
                "abort-strategy despite being built --profile python-release."
            )
            if err:
                print(f"      child stderr: {err[:400]}")
            return 1
        print("ok    unwind: the panic was caught and the process survived")
        return 0

    # expect == "abort"
    if code == 0 or SENTINEL in out:
        print(
            "FAIL  expected abort: the child should have died on the probe. It "
            "survived, so this artifact is NOT abort-strategy — a `--release` "
            "build is not producing the panic behaviour the profile asks for."
        )
        return 1
    if "PROBE_RETURNED_NORMALLY" in out:
        print("FAIL  the probe returned instead of panicking — it is not a probe")
        return 1
    print(f"ok    abort: the child terminated abnormally (exit {code})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
