#!/usr/bin/env python3
"""`@net-mesh/sdk` must be installable against the `@net-mesh/core` it ships with.

The two packages are released together and the README tells callers to
upgrade them together. Nothing enforced it. `sdk-ts/package.json` raised
its peer floor to `@net-mesh/core >=0.35.0` — deliberately, to require a
core that actually has `findBestNode` — while both manifests still read
`0.34.0`. Published as-is, `npm install @net-mesh/sdk@0.34.0
@net-mesh/core@0.34.0` cannot satisfy its own peer range.

CI never saw it: `sdk-ts` resolves core through a
`file:../bindings/node` devDependency, which has no version at all. The
build is green and the published pair is broken.

Two modes:

  default   A pending bump is allowed, so long as the SDK CHANGELOG
            declares it — `## Unreleased — targets 0.35.0` says the
            floor is aimed at a release that has not happened yet.
            That is the normal state of the tree between releases.

  --strict  No exceptions. Run at publish time, where the version in
            the manifest is final by definition and "we'll bump it
            later" is not available.

Run locally:  .github/scripts/check-npm-peer-range.py
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

SDK = Path("net/crates/net/sdk-ts/package.json")
CORE = Path("net/crates/net/bindings/node/package.json")
CHANGELOG = Path("net/crates/net/sdk-ts/CHANGELOG.md")

PEER = "@net-mesh/core"

# Only the range shape actually in use. Anything else fails loudly
# rather than being waved through by a parser that half-understands it.
RANGE_RE = re.compile(r"^>=\s*(\d+)\.(\d+)\.(\d+)$")
VERSION_RE = re.compile(r"^(\d+)\.(\d+)\.(\d+)")
TARGETS_RE = re.compile(r"^##\s+Unreleased\s*[—-]\s*targets\s+(\d+\.\d+\.\d+)", re.M)


def parse_version(text: str) -> tuple[int, int, int]:
    m = VERSION_RE.match(text)
    if not m:
        sys.exit(f"unparseable version: {text!r}")
    return tuple(int(g) for g in m.groups())  # type: ignore[return-value]


def main() -> int:
    strict = "--strict" in sys.argv
    problems: list[str] = []

    sdk = json.loads(SDK.read_text(encoding="utf-8"))
    core = json.loads(CORE.read_text(encoding="utf-8"))

    raw_range = sdk.get("peerDependencies", {}).get(PEER)
    if raw_range is None:
        print(f"{SDK}: no {PEER} peerDependency — the pairing is undeclared")
        return 1

    m = RANGE_RE.match(raw_range.strip())
    if not m:
        print(
            f"{SDK}: peer range {raw_range!r} is not the '>=X.Y.Z' shape this "
            f"check understands — teach it the new shape rather than relaxing it"
        )
        return 1

    floor = tuple(int(g) for g in m.groups())
    core_version = parse_version(core["version"])
    sdk_version = parse_version(sdk["version"])

    if core_version < floor:
        pending = TARGETS_RE.search(CHANGELOG.read_text(encoding="utf-8"))
        target = parse_version(pending.group(1)) if pending else None

        if strict:
            problems.append(
                f"{SDK}: peer range {raw_range} excludes the in-tree "
                f"{PEER} {core['version']} — publish the matching core "
                f"version first, or lower the floor"
            )
        elif target is None:
            problems.append(
                f"{SDK}: peer range {raw_range} excludes the in-tree "
                f"{PEER} {core['version']}, and {CHANGELOG} declares no "
                f"pending bump — add '## Unreleased — targets X.Y.Z' or "
                f"reconcile the manifests"
            )
        elif target < floor:
            problems.append(
                f"{CHANGELOG}: targets {'.'.join(map(str, target))}, which "
                f"still does not satisfy the peer range {raw_range}"
            )

    # The SDK's own version must not run ahead of the floor it demands:
    # that would mean a released SDK pinning a core newer than itself.
    if sdk_version >= floor and core_version < floor:
        problems.append(
            f"{SDK}: version {sdk['version']} is at or past its own peer "
            f"floor {raw_range} while {PEER} is {core['version']} — the "
            f"pair is unshippable"
        )

    for p in problems:
        print(p)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
