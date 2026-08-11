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

THE SOURCE OF TRUTH IS THE UNIFIED RELEASE TAG.

    Candidate manifests may lead publication; install pages name only the
    newest stable unified `vX.Y.Z` tag reachable from the deployed docs
    commit.

Everything else that looked like an answer is wrong in a different direction:

  * Release-note FILES were the previous answer, and they are a record of
    intent, not of publication. v0.35.0 shipped — tagged, published to
    crates.io, npm and PyPI — with no `RELEASE_v0.35*.md` in the tree, so this
    checker went on certifying pages that named 0.34 as correct. A missing
    release note is a release-process gap; it must not also silently pin the
    documentation a minor behind.
  * The workspace `Cargo.toml` version is the NEXT candidate — 0.35.0 while
    0.34 was the newest release — so telling readers to install it is the same
    defect pointed the other way.
  * Registries are the real evidence and are deliberately not consulted.
    Ordinary CI does not get to depend on network calls to crates.io, npm and
    PyPI; a flaky registry must not fail a docs PR.

The tag is the one artifact that is created BY publishing and is local to the
checkout. Reachability from `HEAD` is what makes it an answer for THIS commit:
a tag on a branch nobody merged describes a release these docs do not
document. `cli-v*` and `deck-v*` are separately-versioned binaries, and
`-rc` / `-beta` tags are not shipped releases — none of them are unified
releases and all are ignored.

The consequence to accept: a future release updates "current release"
documentation only once the unified release tag exists. A candidate PR must
not call an unpublished version current.

Usage: check-install-version.py [--self-test]
Exits 0 when every install page names the newest released minor.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from collections.abc import Container, Iterable
from pathlib import Path

_ROOT = Path(__file__).resolve().parents[2]
_INSTALL = _ROOT / "web" / "src" / "content" / "docs" / "start" / "install"

# A unified release tag and nothing else. Anchored at both ends, so
# `cli-v0.35.0` fails at the front and `v0.35.0-rc.1` at the back.
_UNIFIED_TAG = re.compile(r"^v(\d+)\.(\d+)\.(\d+)$")

# Any `0.NN` that is not part of a longer number. Patch suffixes are allowed
# in prose (`0.34.0`) but the minor is what must match.
_VERSION = re.compile(r"(?<![\d.])(\d+)\.(\d+)(?:\.\d+)?(?![\d.])")

# Net is pre-1.0 and every published artifact is `0.x`, so a leading `0` is
# what separates this product's version from the other numbers that
# legitimately appear on an install page — Go 1.26, Node 24, Python 3.10.
#
# There used to be a `(page stem, text)` allowlist here for exactly those.
# Every entry had a non-zero major, so the `major != 0` test below already
# excluded all of them: the allowlist never rejected a single match and could
# not have. It read like a maintained list, which is worse than no list —
# adding "Go 1.27" to it when the toolchain moved would have felt like doing
# the work, while the test that actually matters is the one below.
#
# The consequence to know: a genuinely non-Net `0.x` on an install page (some
# dependency at 0.9, say) WOULD be flagged. That is not a hypothetical worth
# a mechanism yet — no install page has one — and when it happens the honest
# fix is an inline marker on that line, not a table in this file that drifts
# away from the pages it describes.


def parse_unified_tag(tag: str) -> tuple[int, int, int] | None:
    """`(major, minor, patch)` for a unified release tag, else `None`.

    Pure: no Git, no filesystem. Everything the doctrine excludes — the
    per-binary `cli-` / `deck-` prefixes, prerelease suffixes, two-component
    tags, anything not matching at all — comes back `None` here rather than
    being filtered somewhere downstream where the reason is easy to lose.
    """
    m = _UNIFIED_TAG.match(tag.strip())
    if not m:
        return None
    return (int(m.group(1)), int(m.group(2)), int(m.group(3)))


def newest_unified(
    tags: Iterable[str], reachable: Container[str]
) -> tuple[int, int, int] | None:
    """The highest unified release among `tags` that is also `reachable`.

    Split out from the Git call so the selection rules are testable without a
    repository: the ordering, the exclusions, and — the one that is easy to
    get wrong — that an unreachable tag is not a release for THIS checkout,
    however new it is.
    """
    best: tuple[int, int, int] | None = None
    for tag in tags:
        version = parse_unified_tag(tag)
        if version is None:
            continue
        if tag not in reachable:
            continue
        if best is None or version > best:
            best = version
    return best


def _git(*args: str) -> str:
    return subprocess.run(
        ["git", *args],
        capture_output=True,
        text=True,
        cwd=_ROOT,
        check=True,
    ).stdout


def latest_released_version() -> tuple[int, int, int]:
    """The newest stable unified release tag reachable from `HEAD`.

    Fails closed. Every way this can come back empty is a prerequisite the
    caller can fix, and each says which one — a checkout with no tags is a
    CI configuration problem, not evidence that nothing has been released.
    """
    if _git("rev-parse", "--is-shallow-repository").strip() == "true":
        raise SystemExit(
            "FAIL  this is a shallow checkout, so tag reachability cannot be "
            "decided.\n"
            "      The install pages name the newest unified release tag "
            "reachable from HEAD;\n"
            "      a shallow clone can neither see the tags nor the history "
            "that orders them.\n"
            "      Check out with `fetch-depth: 0` (actions/checkout) or "
            "`git fetch --tags --unshallow`."
        )

    all_tags = _git("tag", "--list").splitlines()
    if not all_tags:
        raise SystemExit(
            "FAIL  no tags in this checkout.\n"
            "      The newest unified `vX.Y.Z` tag reachable from HEAD is the "
            "source of truth for\n"
            "      the install pages. Fetch tags (`fetch-depth: 0`) rather "
            "than assuming a version."
        )

    reachable = set(_git("tag", "--list", "--merged", "HEAD").splitlines())
    newest = newest_unified(all_tags, reachable)
    if newest is None:
        raise SystemExit(
            "FAIL  no stable unified `vX.Y.Z` tag is reachable from HEAD.\n"
            f"      {len(all_tags)} tag(s) are present, but none that "
            "qualifies: `cli-v*` / `deck-v*`\n"
            "      version separate binaries, and `-rc` / `-beta` tags are "
            "not shipped releases.\n"
            "      If a release really has shipped, its tag is not merged "
            "into this branch yet."
        )

    if newest[0] != 0:
        # Say this here rather than let it surface as "found no version
        # references at all; the matcher is broken", which is what the
        # `major != 0` filter in `net_versions_in` would produce and which
        # sends the reader after the wrong thing.
        raise SystemExit(
            f"FAIL  newest release is v{newest[0]}.{newest[1]}, but this "
            "checker distinguishes Net versions from Go/Node/Python versions "
            "by their leading `0`.\n"
            "      Post-1.0 that no longer works. Replace the `major != 0` "
            "test in `net_versions_in` with an explicit marker on the lines "
            "that name a Net version."
        )
    return newest


def net_versions_in(path: Path) -> list[tuple[int, int, str]]:
    """Every product-version-looking number on an install page."""
    out: list[tuple[int, int, str]] = []
    for line_no, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        for m in _VERSION.finditer(line):
            # Only 0.x numbers are Net releases; Node 24, Go 1.26 etc. are not.
            if m.group(1) != "0":
                continue
            out.append((int(m.group(1)), int(m.group(2)), f"{path.name}:{line_no}"))
    return out


def check() -> int:
    major, minor, patch = latest_released_version()
    print(f"==> newest reachable unified release tag: v{major}.{minor}.{patch}")

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


def _self_test_tag_parser() -> list[str]:
    """The doctrine, as assertions. Returns a list of failures."""
    failures: list[str] = []

    def expect(got: object, want: object, what: str) -> None:
        if got != want:
            failures.append(f"FAIL  {what}: got {got!r}, expected {want!r}")

    expect(parse_unified_tag("v0.35.0"), (0, 35, 0), "a unified tag is accepted")
    # Everything the doctrine excludes, one reason at a time.
    for rejected, why in [
        ("v0.35.0-rc.1", "a release candidate is not a shipped release"),
        ("v0.35.0-beta.2", "a beta is not a shipped release"),
        ("cli-v0.35.0", "the CLI is versioned separately"),
        ("deck-v0.35.0", "Deck is versioned separately"),
        ("v0.35", "a two-component tag is not a release tag"),
        ("0.35.0", "a tag without the `v` prefix is not one of ours"),
        ("vX.Y.Z", "malformed"),
        ("", "empty"),
    ]:
        expect(parse_unified_tag(rejected), None, f"`{rejected}` ignored — {why}")

    # Ordering is numeric, not lexicographic: `v0.35.10` must outrank
    # `v0.35.9`, which string comparison gets backwards.
    expect(
        newest_unified(["v0.35.0", "v0.35.1"], {"v0.35.0", "v0.35.1"}),
        (0, 35, 1),
        "a later patch outranks an earlier one",
    )
    expect(
        newest_unified(["v0.35.9", "v0.35.10"], {"v0.35.9", "v0.35.10"}),
        (0, 35, 10),
        "patch ordering is numeric, not lexicographic",
    )

    # The whole selection at once, including the case that has no local
    # spelling difference at all: `v0.36.0` is a perfectly well-formed
    # unified tag and is still not a release for THIS checkout, because it is
    # not reachable from HEAD. Nothing about the string says so.
    tags = [
        "v0.34.0",
        "v0.35.0",
        "v0.35.1",
        "v0.35.2-rc.1",
        "cli-v0.36.0",
        "deck-v0.36.0",
        "v0.36.0",
        "not-a-tag",
    ]
    reachable = {"v0.34.0", "v0.35.0", "v0.35.1", "v0.35.2-rc.1", "cli-v0.36.0"}
    expect(
        newest_unified(tags, reachable),
        (0, 35, 1),
        "an unreachable tag is not shipped for this checkout",
    )
    expect(
        newest_unified(["v0.36.0"], set()),
        None,
        "nothing reachable means no answer, not a guess",
    )
    return failures


def self_test() -> int:
    """The tag parser must obey the doctrine, and the page matcher must
    catch a stale number while ignoring a non-Net one."""
    print("==> self-test")

    failures = _self_test_tag_parser()
    if failures:
        print("\n".join(failures))
        return 1
    print("  ok    unified tags parsed; rc/beta, cli-, deck- and malformed ignored")
    print("  ok    newest wins numerically; an unreachable tag is not shipped")

    major, minor, _patch = latest_released_version()

    # The stale number has to be a real, differently-spelled version. Going
    # DOWN a minor breaks at `x.0` — `0.0 - 1` renders as `0.-1`, which the
    # version regex does not match at all, so the test would report "flagged 0
    # stale versions" and blame the matcher for arithmetic. Going up is always
    # well-formed, and "names a version newer than anything released" is the
    # same defect pointed the other way: the install pages must name the
    # NEWEST RELEASED minor, not merely a different one.
    stale_minor = minor + 1

    sample = (
        f"The current published release is **{major}.{minor}**.\n"
        f'net-mesh = "{major}.{stale_minor}"\n'  # wrong — must be caught
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
