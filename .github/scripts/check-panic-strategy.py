#!/usr/bin/env python3
"""Prove what panic strategy a built Python extension actually has.

`cargo build -v` shows what was *requested*. This shows what the loaded
`.pyd`/`.so` *does*, which is the thing that matters — and the thing
that could not be established otherwise when a runtime-drop panic was
seen on a `--release` wheel and the process survived, which an effective
abort profile cannot do. That observation is still unexplained; this
script exists so the question is answerable by construction rather than
by inference.

Two artifacts, each built with the `panic-probe` feature, which compiles
one deliberately panicking export:

    abort control    maturin build --release ...
      the child must terminate abnormally, having reached the probe

    unwind control   maturin build --profile python-release ...
      pyo3 must raise `PanicException` carrying the probe's message, the
      child must catch it, print a sentinel, and exit 0

The probe is always invoked in a **child** process. Under abort it takes
the interpreter with it, so a harness calling it in-process could not
report its own result.

# Failing closed

The oracle is deliberately narrow, because the failure mode of a witness
is proving nothing while printing `ok`. Every acceptance requires the
child to print an ARMED marker first, which it can only do after it has
imported the intended extension AND found the export. Without that, a
child that died from a missing DLL, a loader error, or an interpreter
startup failure would exit nonzero and look exactly like abort.

`--expect abort` additionally requires an *abnormal* termination, not
merely a nonzero one: a signal on POSIX, an NTSTATUS-range status on
Windows. `exit 1` is what a Python traceback produces and must never be
read as evidence of abort.

`--expect unwind` requires the exception to be pyo3's `PanicException`
carrying the probe's own message. An unrelated `ImportError` caught by a
broad `except` would otherwise prove "Rust unwound" while proving
nothing of the kind.

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

#: Printed after the extension imports and the export is found, and
#: immediately before the deliberate panic. Its absence means the child
#: never reached the probe, whatever its exit status says.
ARMED = "PANIC_PROBE_IMPORTED_AND_ARMED"

#: Printed by the child only if it survived the probe and caught it.
SENTINEL = "PROBE_SURVIVED_AND_CAUGHT"

#: Printed if the probe returned instead of panicking.
RETURNED = "PROBE_RETURNED_NORMALLY"

#: The panic message the probe raises. Requiring it rules out an
#: unrelated exception masquerading as containment.
PANIC_TEXT = "net-python deliberate panic-strategy probe"

#: pyo3's panic-carrying exception type.
PANIC_EXC = "PanicException"

CHILD = f"""
import sys
import net._net as native

if not hasattr(native, "_panic_strategy_probe"):
    print("NO_PROBE", flush=True)
    sys.exit(3)

# Everything above can fail for reasons that have nothing to do with
# panic strategy. Past this line the child has the right extension and
# the right symbol, so its exit status means something.
print("{ARMED}", flush=True)

try:
    native._panic_strategy_probe()
except BaseException as exc:            # noqa: BLE001 — classified below
    print("{SENTINEL}", type(exc).__name__, repr(str(exc))[:200], flush=True)
    sys.exit(0)

print("{RETURNED}", flush=True)
sys.exit(4)
"""


def build_env(wheel: Path) -> Path:
    """A throwaway venv with only this wheel installed."""
    env_dir = Path(tempfile.mkdtemp(prefix="panic-probe-"))
    venv.create(env_dir, with_pip=True)
    bindir = "Scripts" if sys.platform == "win32" else "bin"
    exe = "python.exe" if sys.platform == "win32" else "python"
    py = env_dir / bindir / exe
    subprocess.run(
        [str(py), "-m", "pip", "install", "--quiet", str(wheel)],
        check=True,
    )
    return py


def is_abnormal(code: int) -> bool:
    """Did the child die, as opposed to exiting?

    A Python traceback exits 1. `sys.exit(3)` exits 3. Neither is
    evidence of abort, so "nonzero" is not the test.

    POSIX: a negative return code is `-signum` — `abort()` raises
    SIGABRT, so this is the direct signal.

    Windows: there are no signals here. `abort()` in a Rust MSVC build
    goes through `__fastfail`, which surfaces as an NTSTATUS in the
    error range (`0xC0000409` for the fail-fast). Requiring that range
    rejects ordinary positive exits.
    """
    if code < 0:
        return True
    if sys.platform == "win32":
        return (code & 0xF0000000) == 0xC0000000
    return False


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

    def fail(msg: str) -> int:
        print(f"FAIL  {msg}")
        if err:
            print(f"      child stderr: {err[:800]}")
        return 1

    if "NO_PROBE" in out:
        return fail(
            "the wheel has no `_panic_strategy_probe`. Build it with the "
            "`panic-probe` feature; this script cannot test a wheel that has "
            "nothing to trigger."
        )

    # The gate that makes every branch below mean something.
    if ARMED not in out:
        return fail(
            "the child never reached the probe. It did not import the "
            "extension, or died before arming — a missing dependent library, "
            "a loader error, or an interpreter startup failure all look like "
            "this. Whatever the exit status is, it is not evidence about "
            "panic strategy."
        )

    if RETURNED in out:
        return fail("the probe returned instead of panicking — it is not a probe")

    if args.expect == "abort":
        if SENTINEL in out:
            return fail(
                "expected abort, but the panic was caught and the child "
                "survived. This artifact is NOT abort-strategy: a `--release` "
                "build is not producing the panic behaviour the profile asks "
                "for."
            )
        if not is_abnormal(code):
            return fail(
                f"expected abort: the child reached the probe but exited "
                f"normally with {code}. Abort must terminate the process "
                f"abnormally (a signal on POSIX, an NTSTATUS-range status on "
                f"Windows), not merely exit nonzero."
            )
        print(f"ok    abort: the child armed, then died abnormally ({code})")
        return 0

    # expect == "unwind"
    if not is_abnormal(code) and code != 0:
        return fail(f"expected unwind: the child exited {code} rather than 0")
    if is_abnormal(code):
        return fail(
            "expected unwind, but the child terminated abnormally — pyo3 did "
            "not contain the panic, so this artifact is abort-strategy despite "
            "being built --profile python-release."
        )
    if SENTINEL not in out:
        return fail("expected unwind: the child never reported catching anything")
    if PANIC_EXC not in out:
        return fail(
            f"expected unwind: the child caught something, but not pyo3's "
            f"`{PANIC_EXC}`. An unrelated Python exception does not "
            f"demonstrate that a Rust panic was contained."
        )
    if PANIC_TEXT not in out:
        return fail(
            f"expected unwind: caught a {PANIC_EXC} that does not carry the "
            f"probe's message ({PANIC_TEXT!r}). Something other than the probe "
            f"panicked."
        )
    print("ok    unwind: pyo3 raised PanicException with the probe's message, "
          "the child caught it and survived")
    return 0


if __name__ == "__main__":
    sys.exit(main())
