#!/usr/bin/env python3
"""Check the identifiers the skills cite against the source tree.

Two passes over one read of the tree, because walking it is the expensive part
and doing it per-identifier took minutes.

1. **Enum-variant membership.** Symbol existence is not enough: a variant can
   be plausible, correctly spelled, and belong to no enum at all. The nRPC
   table claimed `NoServer`, `NoMatchingServer`, `Canceled`, and `Panic` —
   `RpcError` has none of those (it carries `NoRoute`, `Timeout`, `ServerError`,
   `Transport`, `Codec`, `CapabilityDenied`, and `Cancelled`, with two l's). An
   agent matching on that table writes code that does not compile.

2. **Metric and config identifiers.** The other thing readers copy verbatim,
   into alerts and TOML, and invisible to every other check. This caught
   `dataforts_greedy_admit_throttled_bandwidth_total` (the real metric is one
   counter with a `reason` label) and three sensing knobs documented with a
   `candidate_` prefix they do not carry.

Some identifiers are legitimately absent: prose that names a thing precisely to
say it must not exist, or a rename note listing the old name so stale code is
recognisable. Those go in ABSENT_OK rather than weakening the pattern.

Prints one line per mismatch and exits 1; silent exit 0 when clean.
"""

import pathlib
import re
import sys

SKILLS = pathlib.Path(".claude/skills")
SOURCE_ROOTS = [pathlib.Path("net/crates"), pathlib.Path("go")]
SOURCE_EXT = {".rs", ".ts", ".py", ".go", ".toml", ".h"}

# Identifiers the skills name on purpose despite their absence from source.
ABSENT_OK = {
    # org.md names this to say it must never exist.
    "audience_secret_bytes",
    # scheduler.md's rename note, so an agent recognises stale `*Gpu*` code.
    "match_gpu_islands",
    "claim_gpu_island",
    "warm_models",
}

# Trailing `[^`]*` so struct-like variants written with their fields
# (`RpcError::NoRoute { target, reason }`) are checked too, not skipped.
VARIANT_REF = re.compile(r"`([A-Z][A-Za-z]+)::([A-Z][A-Za-z]+)[^`]*`")
# snake_case, >=3 segments and >=12 chars so ordinary prose does not qualify.
IDENT_REF = re.compile(r"`([a-z][a-z0-9]*(?:_[a-z0-9]+){2,})`")


def read_sources():
    """One read of the tree: (joined corpus, {enum name: (variants, path)})."""
    corpus, enums = [], {}
    for root in SOURCE_ROOTS:
        if not root.exists():
            continue
        for path in root.rglob("*"):
            if path.suffix not in SOURCE_EXT or "target" in path.parts:
                continue
            try:
                src = path.read_text()
            except (UnicodeDecodeError, OSError):
                continue
            corpus.append(src)
            if path.suffix != ".rs":
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
                found = set(re.findall(r"^\s{4}([A-Z]\w*)", src[m.end() : i], re.M))
                enums.setdefault(m.group(1), [set(), str(path)])[0].update(found)
    return "\n".join(corpus), enums


def main():
    corpus, enums = read_sources()
    skill_text = {p: p.read_text() for p in SKILLS.rglob("*.md")}
    bad = 0

    variants = set()
    idents = set()
    for text in skill_text.values():
        variants |= set(VARIANT_REF.findall(text))
        idents |= set(IDENT_REF.findall(text))

    for enum, var in sorted(variants):
        # An unknown name is a struct, trait, or non-Rust type — not our business.
        if enum in enums and var not in enums[enum][0]:
            have = ", ".join(sorted(enums[enum][0])) or "(none)"
            print(
                f"{enum}::{var} is not a variant of {enum} "
                f"({enums[enum][1]}) — actual: {have}"
            )
            bad += 1

    for ident in sorted(idents):
        if len(ident) < 12 or ident in ABSENT_OK:
            continue
        if ident not in corpus:
            print(f"identifier appears nowhere in source: {ident}")
            bad += 1

    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
