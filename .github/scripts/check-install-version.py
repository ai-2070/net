#!/usr/bin/env python3
"""The install pages must name the release that actually shipped.

`start/install/_shared.md` says "The current published release is X" and uses
that number as a cross-layer compatibility instruction — pin the same version
everywhere, because a core at one version and an SDK at another is a
combination nobody built. The per-language pages repeat it, and rust.md puts
it in copy-and-paste `Cargo.toml` snippets.

It said 0.33 while every registry — crates.io, npm, PyPI — served 0.34. A
hard-coded "current" version is a fact with an expiry date, and nothing was
watching it, so it silently became instructions to install a superseded
release and then pin the rest of the stack to match it.

This derives the answer from the release notes in the tree (the newest
`RELEASE_v<major>.<minor>*.md`) and requires the install pages to agree. It
deliberately does NOT read the workspace Cargo version: that is the *next*
candidate — 0.35.0 while 0.34 is the newest release — and telling readers to
install an unpublished version is the same defect pointed the other way.

Usage: check-install-version.py [--self-test]
Exits 0 when every install page names the newest released minor.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

_ROOT = Path(__file__).resolve().parents[2]
_RELEASES = _ROOT / "web" / "src" / "content" / "docs" / "releases"
_INSTALL = _ROOT / "web" / "src" / "content" / "docs" / "start" / "install"

_RELEASE_FILE = re.compile(r"^RELEASE_v(\d+)\.(\d+)")

# Any `0.NN` that is not part of a longer number. Patch suffixes are allowed
# in prose (`0.34.0`) but the minor is what must match.
_VERSION = re.compile(r"(?<![\d.])(\d+)\.(\d+)(?:\.\d+)?(?![\d.])")

# Versions on these pages describe THIS product's release. Other numbers that
# legitimately appear (a Node major, a Python minor, a TypeScript version) are
# excluded by the allowlist below rather than by guessing from context.
_NOT_A_NET_VERSION = {
    # (page stem, matched text) pairs that name something else entirely.
    ("go", "1.26"),
    ("go", "1.25"),
    ("typescript", "24.0"),
}


def latest_released_minor() -> tuple[int, int]:
    """The newest (major, minor) with release notes in the tree."""
    found: list[tuple[int, int]] = []
    for path in _RELEASES.glob("RELEASE_v*.md"):
        m = _RELEASE_FILE.match(path.name)
        if m:
            found.append((int(m.group(1)), int(m.group(2))))
    if not found:
        raise SystemExit(f"FAIL  no release notes found under {_RELEASES}")
    return max(found)


def net_versions_in(path: Path) -> list[tuple[int, int, str]]:
    """Every product-version-looking number on an install page."""
    stem = path.stem
    out: list[tuple[int, int, str]] = []
    for line_no, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        for m in _VERSION.finditer(line):
            text = m.group(0)
            if (stem, f"{m.group(1)}.{m.group(2)}") in _NOT_A_NET_VERSION:
                continue
            if (stem, text) in _NOT_A_NET_VERSION:
                continue
            # Only 0.x numbers are Net releases; Node 24, Go 1.26 etc. are not.
            if m.group(1) != "0":
                continue
            out.append((int(m.group(1)), int(m.group(2)), f"{path.name}:{line_no}"))
    return out


def check() -> int:
    major, minor = latest_released_minor()
    print(f"==> newest release notes: v{major}.{minor}")

    pages = sorted(_INSTALL.glob("*.md"))
    if not pages:
        print(f"FAIL  no install pages under {_INSTALL}")
        return 1

    problems: list[str] = []
    checked = 0
    for page in pages:
        for found_major, found_minor, where in net_versions_in(page):
            checked += 1
            if (found_major, found_minor) != (major, minor):
                problems.append(
                    f"  {where}: names {found_major}.{found_minor}, "
                    f"but the newest release is {major}.{minor}"
                )

    if checked == 0:
        print("FAIL  found no version references at all; the matcher is broken")
        return 1

    if problems:
        print(f"\n{len(problems)} stale version reference(s):")
        print("\n".join(problems))
        print(
            "\nThe install pages tell readers to pin every layer to this number. "
            "Naming a superseded release makes that instruction actively wrong."
        )
        return 1

    print(f"All {checked} version reference(s) across {len(pages)} pages name {major}.{minor}.")
    return 0


def self_test() -> int:
    """The matcher must catch a stale number and ignore a non-Net one."""
    print("==> self-test")
    major, minor = latest_released_minor()

    sample = (
        f"The current published release is **{major}.{minor}**.\n"
        f'net-mesh = "{major}.{minor - 1}"\n'  # stale — must be caught
        "Requires Node 24 and Go 1.26.\n"  # not Net versions — must be ignored
    )
    import tempfile

    with tempfile.TemporaryDirectory() as tmp:
        page = Path(tmp) / "sample.md"
        page.write_text(sample, encoding="utf-8")
        found = net_versions_in(page)

    stale = [f for f in found if (f[0], f[1]) != (major, minor)]
    if len(found) != 2:
        print(f"FAIL  matched {len(found)} version(s), expected 2 (Node/Go must be ignored)")
        return 1
    if len(stale) != 1:
        print(f"FAIL  flagged {len(stale)} stale version(s), expected 1")
        return 1

    print("  ok    stale version caught, non-Net versions ignored")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    return self_test() if args.self_test else check()


if __name__ == "__main__":
    sys.exit(main())
