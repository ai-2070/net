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

Variant citations are also checked for *shape*: `LoadBand { target }` for a
tuple variant `LoadBand(f32)` is wrong in a way membership alone cannot see.

Some identifiers are legitimately absent: prose that names a thing precisely to
say it must not exist, or a rename note listing the old name so stale code is
recognisable. Those go in ABSENT_OK rather than weakening the pattern.

TWO BOUNDARIES, both real and both worth knowing before trusting a green run:

  * Only **backticked, qualified** citations are seen — `Enum::Variant`. A bare
    `Variant` in a bullet list names no owner (the Phase 3 problem), and code
    inside fenced blocks is not scanned here at all; that is the snippet
    compiler's job.
  * Membership and shape are checked, **completeness is not**. A page listing
    two of four variants and calling them "the three options" passes. That
    exact defect existed in `guides/gang-scheduler.md` and had to be found by
    reading.

Prints one line per mismatch and exits 1; silent exit 0 when clean.
"""

import argparse
import pathlib
import re
import sys

SKILLS = pathlib.Path(".claude/skills")
SOURCE_ROOTS = [pathlib.Path("net/crates"), pathlib.Path("go")]
SOURCE_EXT = {".rs", ".ts", ".py", ".go", ".toml", ".h"}

# Identifiers a corpus names on purpose despite their absence from source.
# Keyed by corpus so one document set's deliberate absence cannot silence
# another's genuine defect.
ABSENT_OK_BY_CORPUS = {
    ".claude/skills": {
        # org.md names this to say it must never exist.
        "audience_secret_bytes",
        # scheduler.md's rename note, so an agent recognises stale `*Gpu*` code.
        "match_gpu_islands",
        "claim_gpu_island",
        "warm_models",
    },
    "web/src/content/docs": {
        # A local binding in the fleet-telemetry tutorial's example code, not
        # an API name.
        "fleet_root_entity_id",
    },
}

# Trailing `[^`]*` so struct-like variants written with their fields
# (`RpcError::NoRoute { target, reason }`) are checked too, not skipped.
VARIANT_REF = re.compile(r"`([A-Z][A-Za-z]+)::([A-Z][A-Za-z]+)([^`]*)`")
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
                # Capture the delimiter after each variant name so a
                # documented shape can be checked, not just the name:
                # `LoadBand { target }` for a tuple variant `LoadBand(f32)` is
                # wrong in a way membership alone cannot see.
                found = {}
                for vm in re.finditer(
                    r"^\s{4}([A-Z]\w*)\s*([({])?", src[m.end() : i], re.M
                ):
                    shape = {"(": "tuple", "{": "struct"}.get(vm.group(2), "unit")
                    found[vm.group(1)] = shape
                enums.setdefault(m.group(1), [{}, str(path)])[0].update(found)
    return "\n".join(corpus), enums


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument(
        "--corpus",
        default=str(SKILLS),
        help="directory of markdown to check (default: .claude/skills)",
    )
    ap.add_argument(
        "--exclude",
        action="append",
        default=[],
        help="path substring to skip; repeatable. Use for dated artifacts such "
        "as release notes, which record what was true at the time and must not "
        "be rewritten to match today's tree.",
    )
    args = ap.parse_args()

    root = pathlib.Path(args.corpus)
    if not root.exists():
        print(f"corpus does not exist: {root}")
        return 1
    absent_ok = ABSENT_OK_BY_CORPUS.get(args.corpus, set())

    corpus, enums = read_sources()
    files = [
        p
        for p in sorted(root.rglob("*.md*"))
        if not any(x in str(p) for x in args.exclude)
    ]
    if not files:
        print(f"no markdown found under {root} after exclusions — check the args")
        return 1

    bad = 0
    variants, idents = set(), set()
    for p in files:
        text = p.read_text()
        variants |= set(VARIANT_REF.findall(text))
        idents |= set(IDENT_REF.findall(text))

    for enum, var, trailing in sorted(variants):
        # An unknown name is a struct, trait, or non-Rust type — not our business.
        if enum not in enums:
            continue
        if var not in enums[enum][0]:
            have = ", ".join(sorted(enums[enum][0])) or "(none)"
            print(
                f"{enum}::{var} is not a variant of {enum} "
                f"({enums[enum][1]}) — actual: {have}"
            )
            bad += 1
            continue
        # Shape, when the citation shows one. A bare `Enum::Variant` says
        # nothing about shape and is left alone.
        want = enums[enum][0][var]
        shown = (
            "struct" if trailing.lstrip().startswith("{")
            else "tuple" if trailing.lstrip().startswith("(")
            else None
        )
        if shown and shown != want:
            print(
                f"{enum}::{var} is a {want} variant but is written as a "
                f"{shown} variant ({enums[enum][1]})"
            )
            bad += 1

    for ident in sorted(idents):
        if len(ident) < 12 or ident in absent_ok:
            continue
        if ident not in corpus:
            print(f"identifier appears nowhere in source: {ident}")
            bad += 1

    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
