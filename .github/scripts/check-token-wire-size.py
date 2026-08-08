#!/usr/bin/env python3
"""Every documented `PermissionToken` size must equal the real one.

`PermissionToken::WIRE_SIZE` has moved twice (159 -> 161 at v0.15,
161 -> 169 at v0.19). Each time, the hand-written size in the binding
docstrings and the docs site was left behind: five surfaces still said
161 and Go still said 159 long after the core said 169. Nothing caught
it, because the bindings pass opaque byte buffers — the stale number is
caller-contract drift, not a compile error, and it surfaces only when
someone sizes a buffer or rejects a valid credential against it.

This derives the canonical size from the source of truth and fails on
any disagreeing claim.

RELEASE NOTES ARE EXCLUDED, for the same reason `check-docs.sh` excludes
them: "the wire form grows from 159 to 161 bytes" is a dated record, and
rewriting it corrupts the record rather than fixing anything.

A `TokenChain` is a DIFFERENT shape — `1 + count * WIRE_SIZE` — so
lines expressing a count are left alone. Conflating the two is the other
half of this drift.

Run locally:  .github/scripts/check-token-wire-size.py
Exit 0 = every claim agrees with the tree.
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

TOKEN_RS = Path("net/crates/net/src/adapter/net/identity/token.rs")

# Files whose text is scanned. Everything git tracks, minus vendored
# trees and the dated release records.
EXCLUDE_DIRS = (
    "node_modules/",
    "target/",
    "/releases/",
    "docs/releases/",
    # Dated records, like release notes: internal plans and audits quote
    # the size that was true when they were written, and an audit that
    # reports "Go still says 159" must keep saying so.
    "docs/internal/",
    "docs/misc/",
)
INCLUDE_SUFFIXES = (".rs", ".go", ".h", ".py", ".pyi", ".ts", ".d.ts", ".md")

# A size claim: "169-byte", "169 byte", "(169 bytes)".
SIZE_RE = re.compile(r"\b(\d{3})[- ]bytes?\b")

# Only claims sitting next to a token mention are token claims. Keeps
# the 32-byte entity ids and 64-byte signatures in the same comment
# block from being misread as token sizes — those are two digits and
# already outside SIZE_RE's three-digit range, but the context test is
# what makes the check safe to widen later.
TOKEN_CONTEXT_RE = re.compile(
    r"PermissionToken|Permission\s+token|serialized token|"
    r"token as a Buffer|IssueToken|issue_token|token bytes",
    re.IGNORECASE,
)

# The signed payload is a different (smaller) number by construction —
# WIRE_SIZE is SIGNED_PAYLOAD_SIZE + 64. Claims about it are not claims
# about the wire size.
PAYLOAD_RE = re.compile(
    r"signed[ _-]payload|SIGNED_PAYLOAD_SIZE|positional", re.IGNORECASE
)

# A chain is `1 + count * WIRE_SIZE`, a different shape. Lines that are
# talking about a chain are not making a single-token claim.
CHAIN_RE = re.compile(r"chain|count\s*[*x×]|per link", re.IGNORECASE)

CONTEXT_LINES = 3


def canonical_wire_size() -> int:
    src = TOKEN_RS.read_text(encoding="utf-8")
    signed = re.search(r"SIGNED_PAYLOAD_SIZE:\s*usize\s*=\s*([^;]+);", src)
    wire = re.search(
        r"WIRE_SIZE:\s*usize\s*=\s*Self::SIGNED_PAYLOAD_SIZE\s*\+\s*(\d+)", src
    )
    if not signed or not wire:
        sys.exit(
            f"could not derive WIRE_SIZE from {TOKEN_RS} — the constants moved"
        )
    # The signed payload is itself a sum of literals.
    parts = [int(n) for n in re.findall(r"\b(\d+)\b", signed.group(1))]
    return sum(parts) + int(wire.group(1))


def tracked_files() -> list[Path]:
    out = subprocess.run(
        ["git", "ls-files"], capture_output=True, text=True, check=True
    ).stdout.splitlines()
    return [
        Path(p)
        for p in out
        if p.endswith(INCLUDE_SUFFIXES)
        and not any(d in f"/{p}" for d in EXCLUDE_DIRS)
    ]


def main() -> int:
    expected = canonical_wire_size()
    problems: list[str] = []

    for path in tracked_files():
        try:
            lines = path.read_text(encoding="utf-8").splitlines()
        except (UnicodeDecodeError, OSError):
            continue
        for i, line in enumerate(lines):
            window = "\n".join(
                lines[max(0, i - CONTEXT_LINES) : i + CONTEXT_LINES + 1]
            )
            if not TOKEN_CONTEXT_RE.search(window):
                continue
            if CHAIN_RE.search(line) or PAYLOAD_RE.search(line):
                continue
            for match in SIZE_RE.finditer(line):
                claimed = int(match.group(1))
                if claimed != expected:
                    problems.append(
                        f"{path}:{i + 1}: claims {claimed} bytes, "
                        f"PermissionToken::WIRE_SIZE is {expected}\n"
                        f"    {line.strip()}"
                    )

    # One finding per line on stdout, nothing at all when clean —
    # `run_checker` in `lib/checker.sh` treats any output as a finding.
    for problem in problems:
        print(problem)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
