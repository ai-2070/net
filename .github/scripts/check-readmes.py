#!/usr/bin/env python3
"""Drift checks for the ``README.md`` files outside ``web/``.

Run from anywhere in the repository::

    python3 .github/scripts/check-readmes.py
    python3 .github/scripts/check-readmes.py --self-test  # plant defects

Checks, all offline and deterministic:

1. Every relative Markdown link resolves from the file that contains it.
2. Every README with a License section names both licenses (MIT and Apache).
3. The CLI family (``cli/``, ``cli/npm/``, ``cli/python/``) shares one
   subcommand table, and the Deck family (``deck/``, ``deck/npm/``,
   ``deck/python/``) shares one tabs table.

Exit status is non-zero when any check fails, so this can gate CI. It is
deliberately narrow: it checks facts that have a mechanical oracle, never
prose.
"""

from __future__ import annotations

import os
import re
import subprocess
import sys

LINK_RE = re.compile(r"\]\(([^)\s]+)\)")
LICENSE_SECTION_RE = re.compile(r"^#{1,3}\s*License\b", re.M | re.I)
SKIP_SCHEMES = ("http://", "https://", "mailto:", "#")

FAMILIES = [
    (
        "## Subcommand surface",
        [
            "net/crates/net/cli/README.md",
            "net/crates/net/cli/npm/README.md",
            "net/crates/net/cli/python/README.md",
        ],
    ),
    (
        "## Tabs",
        [
            "net/crates/net/deck/README.md",
            "net/crates/net/deck/npm/README.md",
            "net/crates/net/deck/python/README.md",
        ],
    ),
]


def repo_root() -> str:
    out = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        capture_output=True,
        text=True,
        check=True,
    )
    return out.stdout.strip()


def readmes(root: str) -> list[str]:
    """Tracked README.md files outside web/."""
    out = subprocess.run(
        ["git", "ls-files", "--", "*README*"],
        capture_output=True,
        text=True,
        check=True,
        cwd=root,
    )
    files: list[str] = []
    for line in out.stdout.splitlines():
        name = os.path.basename(line.strip())
        if not (name.startswith("README") and name.endswith(".md")):
            continue
        if line.split("/")[0] == "web":
            continue
        files.append(line.strip())
    return sorted(set(files))


def section_block(text: str, header: str) -> list[str]:
    """The lines of a markdown table under `header`, up to the next blank/heading."""
    out: list[str] = []
    on = False
    for line in text.splitlines():
        if not on:
            if line.strip().startswith(header):
                on = True
            continue
        if not line.strip():
            if out:
                break
            continue
        if line.startswith("## "):
            break
        out.append(line.rstrip())
    return out


def check(
    root: str, files: list[str], families: list[tuple[str, list[str]]]
) -> list[str]:
    """Every problem in `files` (paths relative to `root`) and `families`."""
    failures: list[str] = []

    for rel in files:
        path = os.path.join(root, rel)
        with open(path, encoding="utf-8", errors="replace") as fh:
            text = fh.read()
        base = os.path.dirname(path)

        for target in LINK_RE.findall(text):
            if target.startswith(SKIP_SCHEMES):
                continue
            clean = target.split("#", 1)[0]
            if not clean:
                continue
            resolved = (
                clean
                if os.path.isabs(clean)
                else os.path.normpath(os.path.join(base, clean))
            )
            if not os.path.exists(resolved):
                failures.append(f"{rel}: relative link does not resolve: {target}")

        if LICENSE_SECTION_RE.search(text) and not (
            "Apache" in text and "MIT" in text
        ):
            failures.append(
                f"{rel}: License section does not name both MIT and Apache"
            )

    for header, family in families:
        blocks: list[tuple[str, list[str]]] = []
        for rel in family:
            path = os.path.join(root, rel)
            if not os.path.exists(path):
                failures.append(f"{rel}: missing (family {header!r})")
                continue
            with open(path, encoding="utf-8") as fh:
                blocks.append((rel, section_block(fh.read(), header)))
        if len(blocks) < 2:
            continue
        _, first = blocks[0]
        for rel, block in blocks[1:]:
            if block != first:
                failures.append(
                    f"{rel}: {header} block differs from {blocks[0][0]}"
                )
    return failures


def self_test() -> int:
    """Plant one defect per rule in a scratch tree; require each reported."""
    import tempfile

    print("==> Self-test: planting defects in a scratch tree")
    table = "## Table\n\n| a | b |\n|---|---|\n| x | y |\n\n## Next\n"
    licensed = "## License\n\nMIT or Apache-2.0.\n"
    clean = {
        "README.md": "[sibling](one/README.md)\n\n" + licensed,
        "one/README.md": table + licensed,
        "two/README.md": table,
    }
    family = [("## Table", ["one/README.md", "two/README.md"])]

    def run(tree: dict[str, str], fam: list[tuple[str, list[str]]]) -> list[str]:
        with tempfile.TemporaryDirectory() as tmp:
            for rel, text in tree.items():
                path = os.path.join(tmp, rel)
                os.makedirs(os.path.dirname(path), exist_ok=True)
                with open(path, "w", encoding="utf-8") as fh:
                    fh.write(text)
            return check(tmp, sorted(tree), fam)

    cases = [
        (
            "a relative link that does not resolve",
            "relative link does not resolve",
            {**clean, "README.md": "[gone](nowhere/README.md)\n"},
            family,
        ),
        (
            "a License section naming one license",
            "does not name both MIT and Apache",
            {**clean, "README.md": "## License\n\nMIT.\n"},
            family,
        ),
        (
            "a family member whose table drifted",
            "block differs from",
            {**clean, "two/README.md": table.replace("| x |", "| z |")},
            family,
        ),
        (
            "a family member that does not exist",
            "missing (family",
            clean,
            [("## Table", ["one/README.md", "three/README.md"])],
        ),
    ]

    failed = 0
    for label, expect, tree, fam in cases:
        found = run(tree, fam)
        if any(expect in item for item in found):
            print(f"  ok      reported {label}")
        else:
            print(f"  MISSED  did NOT report {label}: {found}")
            failed += 1

    # And the clean tree passes, so each case above is caught for its
    # planted defect rather than for a fixture that was already broken.
    found = run(clean, family)
    if found:
        print(f"  MISSED  the clean scratch tree fails: {found}")
        failed += 1
    else:
        print("  ok      the clean scratch tree passes")

    if failed:
        print(f"{failed} self-test failure(s).", file=sys.stderr)
        return 1
    print("The checker reports every planted defect.")
    return 0


def main() -> int:
    if "--self-test" in sys.argv:
        return self_test()
    root = repo_root()
    files = readmes(root)
    failures = check(root, files, FAMILIES)

    if failures:
        print(f"README checks failed ({len(failures)}):", file=sys.stderr)
        for item in failures:
            print(f"  - {item}", file=sys.stderr)
        return 1

    print(f"README checks passed ({len(files)} files)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
