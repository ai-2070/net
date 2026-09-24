#!/usr/bin/env python3
"""A LEXICAL PREFLIGHT: fail a roster whose pinned name no source mentions.

Every roster in this workflow pins witnesses by name against a log
produced by the run. That catches a witness that vanished, but not a
pin that never named anything: a typo'd or invented name simply fails
at verification time, and reads exactly like a regression. That cost a
red CI round once already, with a roster of twenty-four pins of which
eighteen named nothing in the file (see the `wasm_leader` roster).

So a roster is checked against its SOURCE. A pinned name that no
source mentions is a bad pin, reported as such. This is the complement
of the by-name log check, NOT a replacement for it:

    pinned but absent from source  -> this script
    present in source, did not run -> the log loop, afterwards

WHAT THIS IS NOT. Read the two limits below before citing this script
as evidence of anything; both have been overstated in a report.

1. It is LEXICAL, not a test-inventory parser. `declared()` searches
   raw file text for `fn <name>(` or for the name in quotes. It does
   not parse Rust or JavaScript, does not evaluate `cfg`, and does not
   know whether a match is a live test. A commented-out declaration,
   an ordinary helper function, a declaration behind a `cfg` that is
   off in this build, or a witness literal that nothing ever emits all
   satisfy it. It also does not deduplicate the roster and does not
   assert set EQUALITY between source and pins — a witness present in
   source but absent from the roster is invisible here, which is what
   the separate cardinality floors are for. The downstream "this exact
   name appeared as passed in the log" loops are therefore load-
   bearing, not belt-and-braces: they are the only check that a pinned
   name corresponds to a test that RAN.

2. It does NOT run before every suite. Its callers, exhaustively:
   the leaf review-probe roster and the `wasm_leaf` / `wasm_leader`
   rosters are checked before their `cargo test` invocations, and the
   browser-package ABI roster before `node abi_real_package.mjs` — for
   those four, "before the suite runs" is accurate. The browser-matrix
   roster is checked in the post-run inventory step: before the ENGINE
   LOGS are read, but after both engines have already executed, so it
   saves the inventory from a bad pin and saves nothing else. The
   native RTC rosters have no preflight here at all; they use the
   existing JUnit checker. Any claim that every roster in this
   workflow is checked before its suite runs is false.

Usage:
    check-roster.py --label wasm_leader --mode fn \\
        --source net/crates/net/leaf/tests/wasm_leader.rs \\
        name_one name_two ...

Modes:
    fn       the name appears as `fn <name>(` in some source file.
    decl     the name appears as a `fn` / `func` / `def` declaration
             (`fn <name>(`, `func <name>(`, `def <name>(`) or as a
             quoted string. The Go and Python rosters need both in one
             pass: the test functions are pinned by declaration, and
             the parametrize ids (`"same_org"`, `"granted"`) only ever
             appear as literals.
    literal  the name appears as a quoted string in some source file
             (harness witnesses, which are emitted by name at runtime).
    text     the name appears VERBATIM anywhere in the source. For pins
             that are code shapes rather than names — `it.each(byteRows)`
             in a vitest file, the unquoted keys of a scenario table —
             this is the only lexical form there is. The cost, stated
             plainly: a mention in a comment satisfies it too. Prefer
             `fn` / `decl` / `literal` wherever the source offers one of
             those forms.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--label", required=True, help="roster name, for the error text")
    ap.add_argument("--mode", choices=("fn", "decl", "literal", "text"), default="fn")
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
        elif args.mode == "text":
            # Verbatim, wherever it appears — see the mode's note in the
            # module docstring for what that costs.
            return any(name in text for text in texts.values())
        elif args.mode == "decl":
            # `fn` (Rust), `func` (Go), `def` (Python) declarations, or the
            # name as a quoted literal — see the mode's note in the module
            # docstring for why the Go/Python rosters pin both.
            decl = re.compile(rf"\b(?:fn|func|def)\s+{re.escape(name)}\s*\(")
            quoted = re.compile(
                rf"""(?:"{re.escape(name)}"|'{re.escape(name)}'|`{re.escape(name)}`)"""
            )
            return any(decl.search(text) or quoted.search(text) for text in texts.values())
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
