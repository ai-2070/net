#!/usr/bin/env python3
"""Remove `skill-check: compile` markers from snippets that do not compile.

The ratchet's maintenance tool, not part of CI. Marking is a claim that a
snippet is self-contained enough to type-check; most are not, and that is fine —
they are documentation fragments referring to variables the surrounding prose
introduces. What is *not* fine is a marker on a snippet that cannot compile,
because then the job is red for a reason that is not a defect.

Usage:
    .github/scripts/check-skill-snippets.py > /tmp/out.log 2>&1
    .github/scripts/ratchet-skill-snippets.py /tmp/out.log     # unmark failures
    .github/scripts/check-skill-snippets.py                    # confirm green

Deliberately manual. Running it automatically in CI would let the coverage
number silently fall whenever someone broke a snippet — the opposite of a
ratchet.
"""

import pathlib
import re
import sys

# Every check in this suite prints its verdict with U+2713 / U+2717, and some
# of the identifiers it echoes carry em-dashes. Python picks stdout's encoding
# from the platform, so on a cp1252 console those characters raise
# UnicodeEncodeError mid-report — the checker dies partway through and its
# caller sees a truncated run rather than a verdict. Force UTF-8 so the output
# is the same everywhere the checker runs.
if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")


def main():
    if len(sys.argv) < 2:
        print(__doc__)
        return 2

    log = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8")
    # Lines look like: `  .claude/skills/x/y.md:123  (snippet 4, generated line 261)`
    failing = {}
    for m in re.finditer(r"^\s+(\.claude/skills/\S+\.md):(\d+)\s+\(snippet", log, re.M):
        failing.setdefault(m.group(1), set()).add(int(m.group(2)))

    if not failing:
        print("no failing snippets in that log — nothing to unmark")
        return 0

    removed = 0
    for path, lines in sorted(failing.items()):
        p = pathlib.Path(path)
        text = p.read_text(encoding="utf-8").splitlines(keepends=True)
        # `line` is the marker's own 1-based line. Delete highest first so
        # earlier indices stay valid.
        for line in sorted(lines, reverse=True):
            idx = line - 1
            if 0 <= idx < len(text) and "skill-check: compile" in text[idx]:
                del text[idx]
                removed += 1
            else:
                print(f"  warning: {path}:{line} is not a marker line — skipped")
        p.write_text("".join(text))
        print(f"  {path}: unmarked {len(lines)}")

    print(f"\nremoved {removed} marker(s). Re-run the checker to confirm green.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
