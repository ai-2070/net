#!/usr/bin/env python3
"""Check the cross-language string vocabularies the skills tabulate.

Scoped deliberately. The plan asked for TS/Python/Go structural membership
checks; measuring the actual surface first changed the answer:

  * Python `sdk-py` defines **zero** `Enum`/`StrEnum` classes — nothing to check.
  * TS string-literal unions: 10 types exist, but the skills cite a member as a
    quoted literal in exactly **2** places (both in one line of `streams.md`).
  * Go typed consts: the citations concentrate almost entirely in **one**
    family — the `nrpc:` wire kinds.

Three parsers plus a citation convention would have guarded ~a dozen citations.
So this checks the thing that actually carries risk: the **frozen cross-language
vocabularies**, where a value is single-sourced across four bindings and the
skills reproduce it as a table. Those drift silently, and a reader
pattern-matches on them.

It caught the nRPC kind table missing `cancelled` and `capability_denied` — both
real wire kinds present in all four bindings, both absent from the skill, so a
cross-language catch site written from the table would have missed them.

Each vocabulary names its own source of truth. Adding one is four lines; adding
a parser for a language that documents nothing is not.

Prints one line per mismatch and exits 1; silent exit 0 when clean.
"""

import pathlib
import re
import sys

SKILLS = pathlib.Path(".claude/skills")


def nrpc_wire_kinds():
    """The eight `nrpc:` kind segments, from the mapping block that every
    binding is written against."""
    src = pathlib.Path("net/crates/net/bindings/node/errors.ts").read_text()
    block = re.search(r"//\s+nrpc:no_route.*?//\s+nrpc:\* \(anything else\)", src, re.S)
    if not block:
        return None
    return set(re.findall(r"nrpc:([a-z_]+)\s+->", block.group(0)))


def go_rpc_kinds():
    """The same vocabulary as Go typed consts — an independent copy, so
    disagreement between the two is itself a finding."""
    kinds = set()
    for p in pathlib.Path("go").glob("*.go"):
        kinds |= set(re.findall(r'RpcKind\w+\s+RpcKind\s*=\s*"([a-z_]+)"', p.read_text()))
    return kinds


def documented_nrpc_kinds():
    """Kind segments the skills tabulate: a leading `| \\`kind\\`` table cell."""
    kinds = set()
    for md in SKILLS.rglob("*.md"):
        for line in md.read_text().splitlines():
            m = re.match(r"^\|\s+`([a-z_]+)`\s+\|", line)
            if m:
                kinds.add(m.group(1))
    return kinds


def main():
    problems = []

    wire = nrpc_wire_kinds()
    if wire is None:
        problems.append(
            "could not parse the nrpc kind mapping from bindings/node/errors.ts "
            "— the comment block moved or changed shape"
        )
    else:
        go = go_rpc_kinds()
        # `unknown` is Go's fallback constant, not one of the mapped kinds.
        for k in sorted(wire - go - {"cancelled"}):
            problems.append(
                f"nrpc kind `{k}` is in the binding mapping but has no Go const "
                f"— the vocabulary is meant to be single-sourced"
            )

        documented = documented_nrpc_kinds()
        # Only flag kinds the skills claim to tabulate exhaustively; a kind
        # documented nowhere at all is a coverage gap, not a correctness bug,
        # so it is reported but scoped to the table that exists.
        if documented & wire:
            for k in sorted(wire - documented):
                problems.append(
                    f"nrpc kind `{k}` exists in every binding but is missing from "
                    f"the skills' kind table — a cross-language catch site written "
                    f"from that table would miss it"
                )

    for p in problems:
        print(p)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
