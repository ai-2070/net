#!/usr/bin/env python3
"""Fail BEFORE a suite runs if a pinned witness name does not exist.

Every roster in this workflow pins witnesses by name against a log
produced by the run. That catches a witness that vanished, but not a
pin that never named anything: a typo'd or invented name simply fails
at verification time, and reads exactly like a regression. That cost a
red CI round once already, with a roster of twenty-four pins of which
eighteen named nothing in the file (see the `wasm_leader` roster).

So a roster is checked against its SOURCE first. A pinned name that no
source declares is a bad pin, reported as such, before a single test
runs. This is the complement of the by-name log check, not a
replacement for it:

    pinned but absent from source  -> this script, up front
    present in source, did not run -> the log loop, afterwards

Usage:
    check-roster.py --label wasm_leader --mode fn \\
        --source net/crates/net/leaf/tests/wasm_leader.rs \\
        name_one name_two ...

Modes:
    fn       the name is declared as `fn <name>(` in some source file.
    literal  the name appears as a quoted string in some source file
             (harness witnesses, which are emitted by name at runtime).
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--label", required=True, help="roster name, for the error text")
    ap.add_argument("--mode", choices=("fn", "literal"), default="fn")
    ap.add_argument(
        "--source",
        action="append",
        required=True,
        help="source file the roster is declared in; repeatable",
    )
    ap.add_argument("names", nargs="+")
    args = ap.parse_args()

    texts: dict[str, str] = {}
    missing_sources = []
    for source in args.source:
        path = Path(source)
        if not path.is_file():
            missing_sources.append(source)
            continue
        texts[source] = path.read_text(encoding="utf-8", errors="replace")

    if missing_sources:
        for source in missing_sources:
            print(f"::error::roster {args.label}: source file {source} does not exist")
        return 1

    def declared(name: str) -> bool:
        if args.mode == "fn":
            pattern = re.compile(rf"\bfn\s+{re.escape(name)}\s*\(")
        else:
            # Rust uses `"name"`, JavaScript uses `'name'` or a
            # template literal. Accepting only one of them would fail
            # a roster whose witnesses are perfectly fine, which is
            # the same false alarm this script exists to prevent.
            quoted = re.escape(name)
            pattern = re.compile(rf"""(?:"{quoted}"|'{quoted}'|`{quoted}`)""")
        return any(pattern.search(text) for text in texts.values())

    bad = [name for name in args.names if not declared(name)]
    for name in bad:
        print(
            f"::error::roster {args.label}: pinned witness {name} is not declared in "
            f"{', '.join(args.source)} — the pin names nothing, so it can never be "
            f"satisfied. Fix the pin or restore the witness."
        )

    if bad:
        return 1

    print(f"roster {args.label}: {len(args.names)} pinned names all exist in source")
    return 0


if __name__ == "__main__":
    sys.exit(main())
