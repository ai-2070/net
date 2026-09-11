#!/usr/bin/env python3
"""S0d scratch helper — print, for each cited line, the enclosing `fn`
and a context window, so every inventory row can be verified against
the file rather than remembered.

    python spikes/tools/sites.py <file> <line> [<line> ...]
    python spikes/tools/sites.py --scan <file>     # list send sites

Read-only; no repository state is changed.
"""

import re
import sys
from pathlib import Path

SEND = re.compile(r"\.(send_to|try_send_to|send)\(|bound_datagram_send\(")
FN = re.compile(r"^(\s*)(pub(\([a-z:]+\))?\s+)?(async\s+)?fn\s+(\w+)")
TESTMOD = re.compile(r"^#\[cfg\(test\)\]")


def first_test_mod(lines):
    for i, l in enumerate(lines):
        if TESTMOD.match(l) and i + 1 < len(lines) and lines[i + 1].startswith("mod "):
            return i + 1
    return len(lines) + 1


def enclosing_fn(lines, lineno):
    """Innermost preceding `fn` whose indentation is strictly smaller
    than the statement's. Closures declare no `fn`, so the first such
    match walking backwards is the host function."""
    stmt_indent = len(lines[lineno - 1]) - len(lines[lineno - 1].lstrip())
    for i in range(lineno - 1, -1, -1):
        m = FN.match(lines[i])
        if m and len(m.group(1)) < stmt_indent:
            return (i + 1, m.group(5), len(m.group(1)))
    return None


def main():
    args = sys.argv[1:]
    if not args:
        print(__doc__)
        return 1
    if args[0] == "--scan":
        path = Path(args[1])
        lines = path.read_text(encoding="utf-8", errors="replace").split("\n")
        cut = first_test_mod(lines)
        for i, l in enumerate(lines, 1):
            if i >= cut:
                break
            if SEND.search(l) and "//" not in l.split(".send")[0]:
                fn = enclosing_fn(lines, i)
                print(f"{path.name}:{i}\t{fn[1] if fn else '?'}@{fn[0] if fn else '?'}\t{l.strip()[:110]}")
        print(f"# first column-0 test mod at line {cut}", file=sys.stderr)
        return 0

    path = Path(args[0])
    lines = path.read_text(encoding="utf-8", errors="replace").split("\n")
    for a in args[1:]:
        n = int(a)
        fn = enclosing_fn(lines, n)
        print(f"===== {path.name}:{n}  fn {fn[1] if fn else '?'} (declared {fn[0] if fn else '?'})")
        for i in range(max(1, n - 8), min(len(lines), n + 8) + 1):
            print(f"{i:6d}{'>' if i == n else ' '} {lines[i - 1]}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
