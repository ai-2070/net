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

Deliberately narrow. Only two forms are flagged, because only two are
unambiguously wrong wherever they appear:

  1. a per-surface LINK FLAG   — `-lnet_rpc`, `-lnet_compute`
  2. a per-surface BUILD TARGET — `cargo build ... -p net-rpc-ffi`

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

_PATTERNS = {
    # `-lnet_rpc`, `-lnet_mcp_ffi`. A space after `-l` is not valid here and
    # is not matched.
    "link-flag": re.compile(rf"-lnet_(?:{_SURFACES})(?:_ffi)?(?![\w])"),
    # `-p net-rpc-ffi`, as passed to cargo.
    "build-target": re.compile(rf"-p\s+net-(?:{_SURFACES})-ffi(?![\w-])"),
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
        for kind, pattern in _PATTERNS.items():
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
        print(f"{len(problems)} instruction(s) naming a library that is not built:\n")
        print("\n".join(problems))
        print(
            f"\nIf a mention is a deliberate negation or history, put "
            f"`{_OPT_OUT}` in a comment on the line or the line before it."
        )
        return 1

    print(f"All {len(files)} consumer-facing files build and link one library.")
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
"""


def self_test() -> int:
    """The matcher must catch both forms, and nothing else."""
    print("==> self-test")
    got = sorted((line, matched) for line, _kind, matched in scan(_SELF_TEST))
    expected = sorted([(1, "-lnet_compute"), (2, "-p net-mcp-ffi")])

    if got != expected:
        print(f"FAIL  matched {got}, expected {expected}")
        return 1

    print("  ok    per-surface link flag and build target caught")
    print("  ok    opt-out honoured; headers, symbols and `libnet_rpc` prose ignored")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    return self_test() if args.self_test else check()


if __name__ == "__main__":
    sys.exit(main())
