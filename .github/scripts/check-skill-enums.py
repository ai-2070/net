#!/usr/bin/env python3
"""Verify every `Enum::Variant` the skills document actually exists on that enum.

Symbol existence is not enough. The skills tabulate error variants for agents to
pattern-match on, and a variant can be plausible, correctly spelled, and belong
to no enum at all. The nRPC table claimed `NoServer`, `NoMatchingServer`,
`Canceled`, and `Panic` — `RpcError` has none of those (it carries `NoRoute`,
`Timeout`, `ServerError`, `Transport`, `Codec`, `CapabilityDenied`, and
`Cancelled`, with two l's). An agent matching on that table writes code that
does not compile.

Prints one line per mismatch and exits 1; silent exit 0 when clean.
"""

import pathlib
import re
import sys

SKILLS = pathlib.Path(".claude/skills")
CRATES = pathlib.Path("net/crates")

# Trailing `[^`]*` so struct-like variants written with their fields
# (`RpcError::NoRoute { target, reason }`) are checked too, not skipped.
REF = re.compile(r"`([A-Z][A-Za-z]+)::([A-Z][A-Za-z]+)[^`]*`")


def documented():
    refs = set()
    for md in SKILLS.rglob("*.md"):
        refs |= set(REF.findall(md.read_text()))
    return refs


def defined():
    """Map enum name -> (variants, defining file). Brace-matched, so nested
    types inside a variant body don't truncate the scan early."""
    enums = {}
    for rs in CRATES.rglob("*.rs"):
        if "target" in rs.parts:
            continue
        try:
            src = rs.read_text()
        except (UnicodeDecodeError, OSError):
            continue
        for m in re.finditer(r"pub enum (\w+)[^{]*\{", src):
            depth, i = 0, m.end() - 1
            while i < len(src):
                if src[i] == "{":
                    depth += 1
                elif src[i] == "}":
                    depth -= 1
                    if depth == 0:
                        break
                i += 1
            variants = set(re.findall(r"^\s{4}([A-Z]\w*)", src[m.end() : i], re.M))
            enums.setdefault(m.group(1), [set(), str(rs)])[0].update(variants)
    return enums


def main():
    enums = defined()
    bad = 0
    for enum, var in sorted(documented()):
        # An unknown name is a struct, trait, or non-Rust type — not our business.
        if enum in enums and var not in enums[enum][0]:
            have = ", ".join(sorted(enums[enum][0])) or "(none)"
            print(
                f"{enum}::{var} is not a variant of {enum} "
                f"({enums[enum][1]}) — actual: {have}"
            )
            bad += 1
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
