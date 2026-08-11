#!/usr/bin/env python3
"""No consumer-facing text may tell someone to build or link a per-surface library.

Net used to ship one cdylib per surface — `libnet_rpc`, `libnet_org`,
`libnet_meshdb`, and five more — each embedding its own copy of `net::ffi`.
Link order unified the FUNCTIONS, which is why it looked fine for months. It
does not unify `static`s, and `parking_lot_core` keeps its parked-thread
registry in one: with two copies loaded, a lock could be released without
waking its waiter. The consolidation into a single `libnet` removed that.

`ci.yml` already guards the CODE side — it fails if a sibling FFI crate emits
a cdylib again, or if a `go/*.go` file reintroduces a second `-l`. Nothing
guarded the TEXT, and text is what a consumer follows. The sweep that
consolidated the docs updated `net/crates/net/include/net.go.h` but not
`go/net.h` — the copy cgo actually compiles — so the header a Go user
actually has kept saying `Link with -lnet -lnet_compute`. Two test files and
a published skill kept saying `cargo build -p net-<x>-ffi`, which is now an
rlib and produces no library at all.

Deliberately narrow. Only three forms are flagged, because only three are
unambiguously wrong wherever they appear:

  1. a per-surface LINK FLAG    — `-lnet_rpc`, `-lnet_compute`
  2. a per-surface BUILD TARGET — `cargo build ... -p net-rpc-ffi`
  3. a COUNTED CLAIM of more than one shared library — "five shared libraries"

The third was added after the code side had been clean for a release: the C
overview still opened with "the ABI is split across eleven headers and five
shared libraries", and the two flag/target patterns cannot see a sentence that
names no flag and no target. A reader who believes that sentence goes looking
for four libraries that do not exist, which is the same wasted afternoon the
flag patterns were written to prevent — reached through prose instead.

It requires a count AND an explicit library kind (`shared` / `static` /
`dynamic` / `native`). Plain plural "libraries" is not enough: pages
legitimately discuss other projects' libraries, and a checker that flagged
those would report false positives and get switched off, which is how the real
defect came back the first time. "One shared library" and any singular form
pass, which is the sentence these pages should be saying.

Everything else that mentions the old names is legitimate: `net_rpc.h` is a
real header, `net_org_call` is a real symbol, `NET_ORG_ERR_*` are real
constants, and the headers themselves explain at length that there is no
`libnet_deck` to link. A checker that flagged those would report a hundred
false positives and get switched off, which is how the real one came back.

Release notes are exempt as a directory: they are a dated record of what
shipped, and v0.12 really did ship `libnet_rpc`.

A negated mention ("there is no `-p net-deck-ffi` to build") is still a
match, because the string alone cannot be told apart from an instruction.
Mark those with `one-library-docs: allow` on the line or the line before.

Usage: check-one-library-docs.py [--self-test]
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

_ROOT = Path(__file__).resolve().parents[2]

# The seven consolidated surfaces. `net-ffi` itself is the real target and is
# deliberately absent.
_SURFACES = "rpc|org|meshdb|meshos|deck|mcp|compute"

_OPT_OUT = "one-library-docs: allow"

# Counts greater than one, spelled either way. `one` / `a` / `1` are absent on
# purpose: "one shared library" is the true sentence and must pass. `both` is
# here because "Link both libraries" is a count, and was a live instruction.
_PLURAL_COUNT = (
    r"both|two|three|four|five|six|seven|eight|nine|ten|eleven|twelve|"
    r"several|multiple|[2-9]|\d{2,}"
)
# `shared` and friends. When one is present the phrase is unambiguous wherever
# it appears; when it is absent, `library-count-net` below asks for context
# instead, because bare "N libraries" is a sentence about somebody else's
# dependencies as often as it is about ours.
_LIB_KIND = r"shared|static|dynamic|native"
# What a linkable artifact gets called around here. `cdylib` is ours by
# construction — it is a Rust crate-type, not a word anyone uses for an
# upstream dependency — which is why it needs no KIND word.
_LIB_NOUN = r"librar(?:y|ies)|cdylibs?|dylibs?|shared objects?"
# Words that make a line a sentence about the NET C surface rather than about
# software in general. `headers` earns its place: "eleven headers over six
# libraries" names neither `libnet` nor a header file, and was the live claim.
_NET_CONTEXT = re.compile(
    r"\bnet[\w.]*\.h\b|\blibnet\b|\bheaders?\b|\bC SDK\b|\bC ABI\b",
    re.IGNORECASE,
)

# kind -> (pattern, needs_net_context)
_PATTERNS = {
    # `-lnet_rpc`, `-lnet_mcp_ffi`. A space after `-l` is not valid here and
    # is not matched.
    "link-flag": (re.compile(rf"-lnet_(?:{_SURFACES})(?:_ffi)?(?![\w])"), False),
    # `-p net-rpc-ffi`, as passed to cargo.
    "build-target": (re.compile(rf"-p\s+net-(?:{_SURFACES})-ffi(?![\w-])"), False),
    # "five shared libraries", "3 dynamic libraries". Singular "library" is
    # matched too so a mistyped "two shared library" is not a hole.
    "library-count": (
        re.compile(
            rf"\b(?:{_PLURAL_COUNT})\s+(?:{_LIB_KIND})\s+librar(?:y|ies)\b",
            re.IGNORECASE,
        ),
        False,
    ),
    # The same claim with the KIND word left out — "eleven headers over six
    # libraries", "ten headers over six cdylibs", "Link both libraries". All
    # three were live in the skills and docs trees while the KIND-word rule
    # above was green, because none of them says "shared". Gated on the line
    # also being about the C surface, which is what keeps "two libraries from
    # upstream" out of it.
    # The negative lookahead keeps this disjoint from `library-count` above,
    # so a kinded claim on a line about headers is reported once with the
    # advice that fits it, not twice.
    "library-count-net": (
        re.compile(
            rf"\b(?:{_PLURAL_COUNT})\s+(?!(?:{_LIB_KIND})\s)(?:{_LIB_NOUN})\b",
            re.IGNORECASE,
        ),
        True,
    ),
    # "the `libnet_meshdb` cdylib", "`libnet_org` cdylib" — a per-surface
    # library presented as a thing that exists to link against. The bare name
    # in prose is deliberately NOT matched: "symbols lived in `libnet_rpc`
    # historically" is true, and so is the list in the consolidation story.
    # It is the following noun that turns a memory into an instruction.
    "surface-library": (
        re.compile(
            rf"libnet_(?:{_SURFACES})(?:_ffi)?`?\s+(?:{_LIB_NOUN})\b",
            re.IGNORECASE,
        ),
        False,
    ),
}

_ADVICE = {
    "link-flag": (
        "there is no such library — every surface is compiled into `libnet`. "
        "Link `-lnet` and nothing else."
    ),
    "build-target": (
        "that crate is an rlib linked into `libnet`; building it produces "
        "nothing to link. Say `cargo build --release -p net-ffi`."
    ),
    "library-count": (
        "there is one shared library, `libnet`. The headers are many; the "
        "library is not. Say `one shared library`, or name `libnet`."
    ),
    "library-count-net": (
        "there is one shared library, `libnet`. If this line is about "
        "somebody else's dependencies, say so away from `headers` / `libnet` "
        "or use the opt-out; if it is history, say it in the past tense."
    ),
    "surface-library": (
        "no such library is built — that name was a per-surface cdylib the "
        "consolidation removed. The header is real; the library is `libnet`."
    ),
}

# Where a consumer actually looks. The FFI crates' own sources and the
# internal plans are out of scope; release notes are a dated record.
_SEARCH_ROOTS = [
    Path("go"),
    Path("net/crates/net/include"),
    Path(".claude/skills"),
    Path("web/src/content/docs"),
]
_SUFFIXES = {".go", ".h", ".c", ".md", ".rs"}
_EXEMPT_DIRS = [Path("web/src/content/docs/releases")]


def _tracked_files() -> list[Path]:
    """Checked-in files under the consumer-facing roots."""
    proc = subprocess.run(
        ["git", "ls-files", "-z", *[str(p) for p in _SEARCH_ROOTS]],
        capture_output=True,
        text=True,
        cwd=_ROOT,
        check=True,
    )
    out = []
    for name in proc.stdout.split("\0"):
        if not name:
            continue
        rel = Path(name)
        if rel.suffix not in _SUFFIXES:
            continue
        if any(exempt in rel.parents for exempt in _EXEMPT_DIRS):
            continue
        out.append(rel)
    return out


def scan(text: str) -> list[tuple[int, str, str]]:
    """(line number, kind, matched text) for every unexempted hit."""
    lines = text.splitlines()
    found: list[tuple[int, str, str]] = []
    for i, line in enumerate(lines, 1):
        previous = lines[i - 2] if i >= 2 else ""
        if _OPT_OUT in line or _OPT_OUT in previous:
            continue
        # Context is decided per LINE, not per file. A page may talk about
        # upstream dependencies in one paragraph and `libnet` in the next,
        # and the sentence is where the claim is.
        has_context = _NET_CONTEXT.search(line) is not None
        for kind, (pattern, needs_context) in _PATTERNS.items():
            if needs_context and not has_context:
                continue
            for m in pattern.finditer(line):
                found.append((i, kind, m.group(0)))
    return found


def check() -> int:
    files = _tracked_files()
    if not files:
        print("FAIL  matched no files under the consumer-facing roots")
        return 1

    problems: list[str] = []
    for rel in files:
        text = (_ROOT / rel).read_text(encoding="utf-8", errors="replace")
        for line_no, kind, matched in scan(text):
            problems.append(
                f"  {rel.as_posix()}:{line_no}: `{matched}` — {_ADVICE[kind]}"
            )

    if problems:
        print(f"{len(problems)} claim(s) about a library that is not built:\n")
        print("\n".join(problems))
        print(
            f"\nIf a mention is a deliberate negation or history, put "
            f"`{_OPT_OUT}` in a comment on the line or the line before it."
        )
        return 1

    print(
        f"All {len(files)} consumer-facing files: no per-surface build or link "
        f"instruction, no counted claim of more than one library, and no "
        f"`libnet_<surface>` named as something to link."
    )
    return 0


_SELF_TEST = f"""\
gcc -o app app.c -L target/release -lnet -lnet_compute -lpthread
cargo build --release -p net-mcp-ffi
// {_OPT_OUT}
There is no `-p net-deck-ffi` to build, and no `-lnet_deck` to link.
See `net_rpc.h` for the nRPC surface; call `net_org_call`.
Symbols live in `libnet_rpc` historically; the header explains why.
cargo build --release -p net-ffi
gcc -o app app.c -L target/release -lnet -lpthread -ldl -lm
The ABI is split across eleven headers and five shared libraries.
Link the three shared libraries required by your application.
Eleven headers, one shared library.
Everything is compiled into libnet.
Bundled with two libraries from upstream, plus the C runtime.
C is eleven headers over six libraries, not one SDK.
The C SDK is ten headers over six cdylibs; nRPC lives in its own pair.
`net_org.h` has its own ABI stamp. Link both libraries.
The same surface through `net_meshdb.h` against the `libnet_meshdb` cdylib.
Net used to ship a cdylib per surface — `libnet_rpc`, `libnet_org`.
Every symbol resolves out of the one `libnet` cdylib.
`-lpthread -ldl -lm` are not Windows libraries; leave them off.
"""


def self_test() -> int:
    """The matcher must catch all five forms, and nothing else."""
    print("==> self-test")
    got = sorted((line, matched) for line, _kind, matched in scan(_SELF_TEST))
    expected = sorted(
        [
            (1, "-lnet_compute"),
            (2, "-p net-mcp-ffi"),
            # The sentence the C overview actually shipped, and the same
            # claim in imperative form. Line 11 proves the true sentence
            # ("one shared library") is not collateral damage, line 12 that
            # naming `libnet` is fine, and line 13 that a plural with no
            # library KIND — somebody else's dependencies — is left alone.
            (9, "five shared libraries"),
            (10, "three shared libraries"),
            # The three shapes that were LIVE in the tree while the two
            # patterns above were green, one per line, exactly as they were
            # written: a bare plural (14), a crate-type noun (15), and a
            # count spelled as a word nobody thinks of as a number (16).
            (14, "six libraries"),
            (15, "six cdylibs"),
            (16, "both libraries"),
            # A per-surface library named as a thing to link against (17) —
            # while the same names in the consolidation story (18) and the
            # one real cdylib (19) are left alone. Line 20 is the linker's
            # own error message, which is about Windows, not about us.
            (17, "libnet_meshdb` cdylib"),
        ]
    )

    if got != expected:
        missing = sorted(set(expected) - set(got))
        extra = sorted(set(got) - set(expected))
        print(f"FAIL  matched {got}, expected {expected}")
        if missing:
            print(f"      missed: {missing}")
        if extra:
            print(f"      false positives: {extra}")
        return 1

    print("  ok    per-surface link flag and build target caught")
    print("  ok    counted multi-shared-library prose caught, both word and count-first")
    print("  ok    uncounted-KIND claims caught on C-surface lines: `six libraries`,")
    print("        `six cdylibs`, `both libraries` — the three that were live")
    print("  ok    `libnet_<surface>` named as a linkable artifact caught")
    print("  ok    `one shared library`, `libnet`, upstream plurals and the")
    print("        consolidation story all pass")
    print("  ok    opt-out honoured; headers, symbols and `libnet_rpc` prose ignored")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    return self_test() if args.self_test else check()


if __name__ == "__main__":
    sys.exit(main())
