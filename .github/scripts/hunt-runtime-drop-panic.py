#!/usr/bin/env python3
"""Hunt the intermittent tokio runtime-drop panic and name its owner.

# What is being hunted

After every runtime the Python binding *creates* was moved to
`GuardedRuntime`, one panic was still seen during a full-suite run:

    thread '<name>' panicked at .../shutdown.rs:51:
    Cannot drop a runtime in a context where blocking is not allowed.

It has not reproduced in three subsequent runs of the exact release
wheel. That is not evidence it is gone: the trigger is a CPython GC
running at a point where the last `Arc` to some runtime is released on
a thread that is already inside a runtime, which depends on allocation
timing. Absence over three runs is weak evidence about a rare event.

The open question is **ownership**. A runtime the binding never
constructed cannot be fixed by `GuardedRuntime`; some dependency
building its own runtime (`reqwest`'s blocking client is the obvious
shape, reachable through `payments-http`) would produce exactly this
message from code no amount of binding-side guarding touches.
`payments-http` is a suspect, not a finding, and this script exists to
replace the suspicion with a backtrace.

# Why this needs a special build

The shipped `python-release` profile has `strip` at its inherited
default and `debug` off, so a backtrace from it is a list of addresses.
`python-diagnostic` inherits `python-release` — same optimisation, same
LTO, same `panic = "unwind"` — and adds `debug = 2` with `strip =
"none"`. The panic path is therefore the shipped one; only the symbols
differ.

Do not substitute a debug build. Debug changes inlining, allocation
timing, and thread scheduling — every variable this race depends on.

# Why `-s` and why the output is scanned rather than the exit code

The panic happens on a background worker thread during a drop. Nothing
propagates it to the test that was running, so:

  * the suite still exits 0 — the exit code is not the signal;
  * pytest's per-test capture may not own the thread's stderr, so
    `-s` (capture disabled) is what reliably surfaces it.

`-v` interleaves test names with the panic, which is the only way to
say which test was in flight when it fired.

Usage:
    hunt-runtime-drop-panic.py --python <venv-python> --tests <dir> \
        [--runs N] [--out <dir>]
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
from pathlib import Path

#: The panic tokio raises when a runtime is dropped inside an async
#: context. Matching the message rather than the file:line keeps this
#: working across tokio versions.
PANIC = "Cannot drop a runtime in a context where blocking is not allowed"

#: Rust's default panic hook prefixes the thread name. Capturing it
#: separates "a tokio worker dropped it" from "the main interpreter
#: thread did", which are different defects.
THREAD_LINE = re.compile(r"thread '([^']*)' panicked at ([^\n]*)")

#: A pytest `-v` progress line, used to attribute the panic to the test
#: that was in flight.
TEST_LINE = re.compile(r"^(\S+::\S+)\s")

#: pytest's own summary, used to prove the run actually ran something.
SUMMARY = re.compile(r"(\d+) passed")

#: Below this, assume the suite did not really execute. A bad `--tests`
#: path, a collection error, or an import failure all produce a run that
#: finds no panic — indistinguishable from a clean sweep unless the
#: harness insists on having seen a real suite. This is the vacuous-pass
#: guard: a hunt that proves nothing must not print "not reproduced".
MIN_TESTS = 500


def run_once(
    py: Path, tests: Path, idx: int, out_dir: Path, stress: bool
) -> tuple[bool, Path]:
    """One full-suite run. Returns (panic_seen, transcript_path).

    Raises if the run did not execute a plausible suite — see
    `MIN_TESTS`.

    With `stress`, the two free variables the panic depends on are
    perturbed instead of held fixed: test order (a different seed per
    run, so each run is a different allocation sequence) and collection
    frequency (the `gcstress` plugin). Without it, repeated runs re-run
    one trajectory — they sample the timing space once, N times.
    """
    env = dict(os.environ)
    # Symbolicated Rust backtrace, and CPython's own fault handler for
    # the case where the process dies instead of surviving.
    env["RUST_BACKTRACE"] = "full"
    env["PYTHONFAULTHANDLER"] = "1"
    # Unbuffered, so the interleaving of test names and the panic
    # reflects real ordering rather than flush timing.
    env["PYTHONUNBUFFERED"] = "1"

    args = [str(py), "-m", "pytest", str(tests), "-v", "-s"]
    if stress:
        # `gcstress` lives beside this script.
        env["PYTHONPATH"] = str(Path(__file__).parent) + os.pathsep + env.get("PYTHONPATH", "")
        # A distinct, RECORDED seed per run: a reproduction is only
        # useful if the order that produced it can be replayed.
        # pytest-randomly autoloads; it only needs the seed. Passing it
        # as an explicit `-p` names a plugin that does not exist.
        args += ["-p", "gcstress", f"--randomly-seed={idx}"]
    else:
        args += ["-p", "no:randomly"]

    transcript = out_dir / f"run-{idx:02d}.log"
    proc = subprocess.run(
        args,
        capture_output=True,
        text=True,
        env=env,
    )
    body = (proc.stdout or "") + "\n" + (proc.stderr or "")
    transcript.write_text(body, encoding="utf-8", errors="replace")

    m = SUMMARY.search(body)
    ran = int(m.group(1)) if m else 0
    if ran < MIN_TESTS:
        raise SystemExit(
            f"run {idx:02d} executed {ran} test(s), below the {MIN_TESTS} floor.\n"
            f"The suite did not really run: a bad --tests path, a collection\n"
            f"error, or a failed import. Such a run finds no panic for reasons\n"
            f"having nothing to do with the defect, so continuing would let a\n"
            f"vacuous sweep be reported as 'not reproduced'.\n"
            f"See {transcript}"
        )

    seen = PANIC in body
    print(
        f"  run {idx:02d}: exit={proc.returncode} tests={ran} "
        f"panic={'YES' if seen else 'no'}"
    )
    return seen, transcript


def report(transcript: Path) -> None:
    """Print the panic in context: which test, which thread, what stack."""
    body = transcript.read_text(encoding="utf-8", errors="replace")
    lines = body.splitlines()
    for i, line in enumerate(lines):
        if PANIC not in line:
            continue

        # The last test line before the panic is the one in flight.
        in_flight = "<unknown>"
        for prev in reversed(lines[:i]):
            m = TEST_LINE.match(prev)
            if m:
                in_flight = m.group(1)
                break

        thread, loc = "<unknown>", "<unknown>"
        for prev in reversed(lines[max(0, i - 6):i + 1]):
            m = THREAD_LINE.search(prev)
            if m:
                thread, loc = m.group(1), m.group(2)
                break

        print("\n" + "=" * 72)
        print("RUNTIME-DROP PANIC")
        print("=" * 72)
        print(f"  transcript : {transcript}")
        print(f"  test       : {in_flight}")
        print(f"  thread     : {thread}")
        print(f"  location   : {loc}")
        print("\n--- backtrace ---")
        # Everything up to the frame that leaves Rust, which is where
        # the owner shows up.
        print("\n".join(lines[i:i + 90]))
        print("=" * 72)
        return


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--python", required=True, type=Path)
    ap.add_argument("--tests", required=True, type=Path)
    ap.add_argument("--runs", type=int, default=10)
    ap.add_argument("--out", type=Path, default=Path("runtime-drop-hunt"))
    ap.add_argument(
        "--stress",
        action="store_true",
        help="perturb test order (seeded per run) and GC frequency, instead "
             "of repeating one execution trace",
    )
    args = ap.parse_args()

    args.out.mkdir(parents=True, exist_ok=True)
    print(f"hunting the runtime-drop panic over {args.runs} full-suite run(s)")
    print(f"  python: {args.python}")
    print(f"  tests : {args.tests}")
    print(f"  mode  : {'STRESS (random order + dense GC)' if args.stress else 'plain'}\n")

    hits = []
    for i in range(1, args.runs + 1):
        seen, transcript = run_once(args.python, args.tests, i, args.out, args.stress)
        if seen:
            hits.append(transcript)

    print(f"\n{len(hits)}/{args.runs} run(s) reproduced the panic")
    for t in hits:
        report(t)

    if not hits:
        print(
            "\nNot reproduced. This does NOT close the residual: the trigger is\n"
            "GC/allocation timing, so a clean sweep bounds the rate and nothing\n"
            "more. Report it as 'not reproduced in N runs', never as 'fixed'."
        )
        if not args.stress:
            print(
                "\nAnd this was PLAIN mode, so the bound is weaker than the run\n"
                "count suggests: fixed test order means every run replays one\n"
                "execution trace. N runs are N samples of a single trajectory,\n"
                "not N samples of the timing space. Re-run with --stress before\n"
                "drawing any conclusion from a clean sweep."
            )
    # Reproducing is the goal, so a hit is not a failure of this script.
    return 0


if __name__ == "__main__":
    sys.exit(main())
