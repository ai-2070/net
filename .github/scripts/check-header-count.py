#!/usr/bin/env python3
"""No doc may name a header count that the include directory disagrees with.

WHY THIS EXISTS. `net/crates/net/include/` ships eleven `.h` files. Eleven of
the places that say so were right; two skill pages said "ten headers", and the
C skill's header table was ten rows — `net_subnet.h`, which both
`coverage.md` and the docs' `headers-and-linking.md` document, had no row at
all. A reader working from that table would conclude the subnet authority
surface has no C binding.

Nobody typed a wrong number. `net_subnet.h` was added, the pages that
enumerate headers were updated one at a time, and two copies were missed. That
is the shape of every defect in this family: prose duplicated across a docs
tree and a skills tree drifts one copy at a time, and the copy nobody opened
this quarter is the one a reader lands on.

The count is derivable, so it should not be typed and trusted. This derives it
from `git ls-files` over the include directory and requires the prose to agree.

TWO RULES, AND WHAT EACH IS FOR.

  1. COUNTED CLAIM — "eleven headers", "ten headers", "11 headers". Must equal
     the real count. This is the one that catches a stale copy: ship a twelfth
     header and every page still saying eleven goes red at once, which is the
     opposite of finding out one page at a time.

  2. NEAR-COMPLETE ENUMERATION — a file that names all but a couple of the
     shipped headers is a header INDEX, and an index that is missing a row is
     worse than one that does not exist, because it reads as complete. A file
     naming most of them must name all of them.

     Detected by counting, not by looking for a table: a table is one way to
     enumerate and the rule should not care which way a page chose. The
     threshold is deliberately near the top — `net_org.h` legitimately
     mentions six of its neighbours in prose and is not an index.

WHAT IS NOT CHECKED. Uncounted plurals ("the headers", "several headers") are
left alone, and so is the singular: "include one header per translation unit"
is a true sentence about a different thing, and a checker that argued with it
would be arguing with English. `the two headers that share NET_SDK_H` is
anaphoric — it points at two already-named headers rather than claiming the
set has two members — so a count behind a definite or possessive determiner is
skipped. Anything left over has the opt-out.

Usage: check-header-count.py [--self-test]
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

_ROOT = Path(__file__).resolve().parents[2]
_INCLUDE = "net/crates/net/include"

_OPT_OUT = "header-count: allow"

# Spelled either way. `one` / `1` are absent on purpose — see the docstring.
_WORDS = {
    "two": 2,
    "three": 3,
    "four": 4,
    "five": 5,
    "six": 6,
    "seven": 7,
    "eight": 8,
    "nine": 9,
    "ten": 10,
    "eleven": 11,
    "twelve": 12,
    "thirteen": 13,
    "fourteen": 14,
    "fifteen": 15,
    "sixteen": 16,
}

# A count immediately before `header(s)`, with the adjectives that routinely
# sit between them allowed through. The lookbehinds drop anaphoric counts —
# "the two headers", "those three headers" — which point at headers already
# named rather than claiming a total. Python allows several lookbehinds in a
# row as long as each is individually fixed-width, which these are.
_COUNTED = re.compile(
    r"(?<!the )(?<!its )(?<!both )(?<!those )(?<!these )"
    r"\b(" + "|".join(_WORDS) + r"|[2-9]|\d{2,})\s+"
    r"(?:public\s+|generated\s+|shipped\s+|C\s+)*headers?\b",
    re.IGNORECASE,
)

# Where a consumer actually looks. Same roots as `check-one-library-docs.py`,
# and for the same reason: the FFI crates' own sources and the internal plans
# are out of scope, and release notes are a dated record of what was true then.
_SEARCH_ROOTS = ["go", _INCLUDE, ".claude/skills", "web/src/content/docs"]
_SUFFIXES = {".go", ".h", ".c", ".md", ".rs"}
_EXEMPT_DIRS = [Path("web/src/content/docs/releases")]

# A file naming at least this many of the shipped headers is enumerating the
# set. Two below the total: `net_org.h` names six of its neighbours in prose
# and must not be dragged in, while a table that dropped a row is caught.
_INDEX_SLACK = 2


def _git(*args: str) -> str:
    return subprocess.run(
        ["git", *args], capture_output=True, text=True, cwd=_ROOT, check=True
    ).stdout


def shipped_headers() -> list[str]:
    """Every `.h` checked in under the include directory.

    `git ls-files`, not `glob`: a header generated into the working tree but
    not committed is not something a consumer receives, and a stale one left
    behind by an old build must not inflate the count either.
    """
    names = sorted(
        Path(p).name
        for p in _git("ls-files", f"{_INCLUDE}/*.h").splitlines()
        if p.strip()
    )
    if not names:
        raise SystemExit(
            f"FAIL  no `.h` files tracked under {_INCLUDE}.\n"
            "      That is a prerequisite failure, not a count of zero — "
            "check the path and that this is a full checkout."
        )
    return names


def _tracked_files() -> list[Path]:
    """Checked-in files under the consumer-facing roots."""
    out = []
    for name in _git("ls-files", "-z", *_SEARCH_ROOTS).split("\0"):
        if not name:
            continue
        rel = Path(name)
        if rel.suffix not in _SUFFIXES:
            continue
        if any(exempt in rel.parents for exempt in _EXEMPT_DIRS):
            continue
        out.append(rel)
    return out


def scan_counts(text: str, expected: int) -> list[tuple[int, str, int]]:
    """(line, matched text, claimed count) for every count that is not `expected`."""
    lines = text.splitlines()
    found: list[tuple[int, str, int]] = []
    for i, line in enumerate(lines, 1):
        previous = lines[i - 2] if i >= 2 else ""
        if _OPT_OUT in line or _OPT_OUT in previous:
            continue
        for m in _COUNTED.finditer(line):
            token = m.group(1).lower()
            claimed = _WORDS.get(token) or int(token)
            if claimed != expected:
                found.append((i, m.group(0), claimed))
    return found


def missing_from_index(text: str, headers: list[str]) -> list[str]:
    """Headers absent from a file that enumerates nearly all of them.

    Empty for a file that is not an index — which is most of them.
    """
    named = [h for h in headers if h in text]
    if len(named) < len(headers) - _INDEX_SLACK:
        return []
    return [h for h in headers if h not in named]


def check() -> int:
    headers = shipped_headers()
    expected = len(headers)
    print(f"==> {expected} headers tracked under {_INCLUDE}")

    files = _tracked_files()
    if not files:
        print("FAIL  matched no files under the consumer-facing roots")
        return 1

    problems: list[str] = []
    for rel in files:
        text = (_ROOT / rel).read_text(encoding="utf-8", errors="replace")
        for line_no, matched, claimed in scan_counts(text, expected):
            problems.append(
                f"  {rel.as_posix()}:{line_no}: `{matched}` — {expected} "
                f"headers ship, not {claimed}. If this counts something other "
                f"than the shipped set, put `{_OPT_OUT}` on the line or the "
                f"line before."
            )
        for missing in missing_from_index(text, headers):
            problems.append(
                f"  {rel.as_posix()}: enumerates the headers but never names "
                f"`{missing}` — an index missing a row reads as complete, "
                f"which is worse than no index."
            )

    if problems:
        print(f"{len(problems)} header-count problem(s):\n")
        print("\n".join(problems))
        print(f"\nThe shipped set is: {', '.join(headers)}")
        return 1

    print(
        f"All {len(files)} consumer-facing files agree there are {expected} "
        f"headers, and every file that enumerates them names all of them."
    )
    return 0


_SELF_TEST_HEADERS = ["net.h", "net_a.h", "net_b.h", "net_c.h", "net_d.h"]

_SELF_TEST = f"""\
The ABI is split across four headers, all compiled into one library.
It is one of five headers, NOT the entire C SDK.
C is 5 headers over one shared library.
Include one header per translation unit.
The two headers that share NET_SDK_H are `net.h` and `net.go.h`.
// {_OPT_OUT}
This paragraph is about six headers from some other project.
Twelve headers would be a lot.
"""


def _self_test_counts() -> list[str]:
    """The count matcher, against a pretend five-header world."""
    failures: list[str] = []
    got = sorted(
        (line, matched.lower(), claimed)
        for line, matched, claimed in scan_counts(_SELF_TEST, len(_SELF_TEST_HEADERS))
    )
    expected = sorted(
        [
            # A stale total, spelled as a word...
            (1, "four headers", 4),
            # ...and one spelled as a digit is caught the same way (line 3 is
            # correct at 5 and must NOT appear). Line 2's "one of five" is the
            # right number in the phrasing the header banners use.
            (8, "twelve headers", 12),
        ]
    )
    if got != expected:
        failures.append(f"FAIL  counts: matched {got}, expected {expected}")
    return failures


def _self_test_index() -> list[str]:
    """The enumeration rule, at and either side of its threshold."""
    failures: list[str] = []
    hdrs = _SELF_TEST_HEADERS

    def expect(got: object, want: object, what: str) -> None:
        if got != want:
            failures.append(f"FAIL  {what}: got {got!r}, expected {want!r}")

    expect(missing_from_index(" ".join(hdrs), hdrs), [], "a complete index passes")
    # One row short of complete: an index, and an incomplete one.
    expect(
        missing_from_index(" ".join(hdrs[:-1]), hdrs),
        ["net_d.h"],
        "an index missing one header is caught",
    )
    # Two short is still within slack — the threshold has to sit somewhere,
    # and a page mentioning a handful of neighbours is not an index.
    expect(
        missing_from_index(" ".join(hdrs[:2]), hdrs),
        [],
        "a page naming a couple of headers is not an index",
    )
    expect(missing_from_index("no headers here", hdrs), [], "unrelated prose passes")
    return failures


def self_test() -> int:
    print("==> self-test")
    failures = _self_test_counts() + _self_test_index()
    if failures:
        print("\n".join(failures))
        return 1
    print("  ok    a stale count is caught, as a word and as a digit")
    print("  ok    the right count, `one header`, `the two headers` and the")
    print("        opt-out all pass")
    print("  ok    an enumeration missing a header is caught; a passing")
    print("        mention of a few is not")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    return self_test() if args.self_test else check()


if __name__ == "__main__":
    sys.exit(main())
