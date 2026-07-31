#!/usr/bin/env python3
"""Interleaved N-arm ICB-1 runner.

Implements the measurement contract in
docs/internal/misc/PERF_AUDIT_2026_07_31_GANG_SCHEDULER.md.

Two properties the contract asks for and this enforces:

* **Interleaving, not batching.** Every repetition runs every arm before
  moving on, and the arm ORDER rotates between repetitions, so machine
  drift is charged to all arms roughly equally instead of being
  attributed to whichever change happened to run last.
* **Attribution.** Arms are per-slice commits, not just
  before-and-after, because a single aggregate delta cannot say which
  slice produced it — the contract calls that out explicitly.

Reports each arm's per-cell p50 as a median across repetitions plus the
observed range, so no single number is ever the claim. This is not a
threshold check: the acceptance rule is "the targeted mechanism improves
outside run-to-run noise and unrelated rows do not regress", which a
human reads off the table by comparing deltas against the ranges.

Usage: icb1_interleave.py <reps> <warmups> <name>=<exe> [<name>=<exe> ...]
"""

import os
import re
import statistics
import subprocess
import sys

# Deliberately does NOT match the box-drawing/interpunct glyphs the row
# header uses -- those are UTF-8 and this has to parse identically
# regardless of console codepage.
CELL = re.compile(r"ICB-1 .* islands=(\d+) .* units=(\d+) .* (sparse|dense)")
P50 = re.compile(r"p50=([\d.]+)us")
POP = re.compile(r"island_pop=.*viable_returned=(\d+)")


def run_once(exe):
    """One full ICB-1 pass -> ({cell: p50_us}, [viable_returned...])."""
    out = subprocess.run(
        [exe],
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        timeout=600,
    )
    if out.returncode != 0:
        raise SystemExit(f"{exe} exited {out.returncode}:\n{out.stderr[-2000:]}")
    cells, pops, cur = {}, [], None
    for line in out.stdout.splitlines():
        m = CELL.search(line)
        if m:
            # Zero-padded so the label sorts numerically as plain text.
            cur = f"islands={int(m.group(1)):04d} units={int(m.group(2)):02d} {m.group(3)}"
            continue
        m = POP.search(line)
        if m:
            pops.append(int(m.group(1)))
            continue
        m = P50.search(line)
        if m and cur:
            cells[cur] = float(m.group(1))
            cur = None
    if not cells:
        raise SystemExit(f"{exe}: parsed no cells -- output format changed?")
    return cells, pops


def main():
    reps, warmups = int(sys.argv[1]), int(sys.argv[2])
    arms = [a.split("=", 1) for a in sys.argv[3:]]

    # Fail on a missing arm up front, by name. Cargo evicts release
    # artifacts when a clippy run reuses the same target dir, so an arm
    # binary can vanish between building it and measuring with it —
    # which otherwise surfaces as a bare WinError 2 from deep inside
    # subprocess, with no indication of WHICH arm is missing.
    missing = [(n, p) for n, p in arms if not os.path.exists(p)]
    if missing:
        raise SystemExit(
            "missing arm binaries (rebuild with "
            "`cargo build --release --bench island_claim_match --features net`):\n"
            + "\n".join(f"  {n}: {p}" for n, p in missing)
        )

    for _ in range(warmups):
        for _, exe in arms:
            run_once(exe)

    runs = {name: [] for name, _ in arms}
    pops = {}
    for i in range(reps):
        # Rotate arm order each repetition so no arm systematically runs
        # on a warmer machine than the others.
        for name, exe in arms[i % len(arms):] + arms[: i % len(arms)]:
            cells, pop = run_once(exe)
            runs[name].append(cells)
            pops.setdefault(name, pop)

    # Populations are an output-equality check: if any arm returns a
    # different result set, the timing comparison is meaningless.
    ref_name, ref_pop = next(iter(pops.items()))
    for name, pop in pops.items():
        if pop != ref_pop:
            raise SystemExit(
                f"OUTPUT MISMATCH: arm '{name}' returned different viable counts "
                f"than '{ref_name}'\n  {ref_name}: {ref_pop}\n  {name}: {pop}"
            )
    print(f"\noutput equality: all {len(arms)} arms returned identical "
          f"viable_returned across all cells ({len(ref_pop)} cells)")

    names = [n for n, _ in arms]
    cells = sorted(runs[names[0]][0])
    print(f"\nICB-1 interleaved: reps={reps} warmups={warmups} "
          f"(450 samples/cell/rep, 4 workers)")
    print("p50 median across reps, with [min-max] range across reps; "
          "delta vs first arm\n")

    w = 26
    hdr = f"{'cell':<{w}}" + "".join(f"{n:>22}" for n in names)
    print(hdr)
    print("-" * len(hdr))
    for c in cells:
        row = f"{c:<{w}}"
        base_med = None
        for n in names:
            vals = [r[c] for r in runs[n] if c in r]
            med = statistics.median(vals)
            if base_med is None:
                base_med = med
                row += f"{med:>8.2f}u [{min(vals):.1f}-{max(vals):.1f}]".rjust(22)
            else:
                pct = ((med - base_med) / base_med * 100) if base_med else 0.0
                row += f"{med:>7.2f}u {pct:>+6.1f}%".rjust(22)
        print(row)
    print(f"\ndelta is vs '{names[0]}'. Negative = faster. Compare each delta")
    print("against that arm's own [min-max] range before reading into it.")


if __name__ == "__main__":
    main()
