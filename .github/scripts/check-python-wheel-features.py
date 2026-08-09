#!/usr/bin/env python3
"""The wheel CI tests must be the wheel users install.

`net-mesh`'s crate defaults omit `org` and `deck`. The release workflow
built with `--features redis,extension-module`, so published wheels had
neither — while the capability record marked Python organization auth
and Deck `supported`, the v0.34 release notes claimed the authority
verbs in five languages, and the SDK README said PyPI wheels ship every
feature enabled.

Nothing caught it, because CI's own `maturin develop` enabled both
explicitly. The tested artifact and the published artifact had
different surfaces, and every test was green against the one nobody
installs.

This compares the two feature lists and fails when the release build
would omit something CI tested. CI may enable *more* — it builds
`--no-default-features` and has to name defaults explicitly — so the
check is a subset test, not equality, computed over resolved features.

Run locally:  .github/scripts/check-python-wheel-features.py
"""

from __future__ import annotations

import re
import sys
import tomllib
from pathlib import Path

CARGO = Path("net/crates/net/bindings/python/Cargo.toml")
RELEASE_WF = Path(".github/workflows/release-python.yml")
CI_WF = Path(".github/workflows/ci.yml")

# `PY_RELEASE_FEATURES: redis,org,deck,extension-module`
RELEASE_RE = re.compile(r"^\s*PY_RELEASE_FEATURES:\s*(\S+)\s*$", re.M)
# `maturin develop --no-default-features --features a,b,c`
CI_RE = re.compile(r"maturin develop\s+(?P<flags>[^\n]*)")

# Not a capability; it controls whether the cdylib links libpython.
BUILD_ONLY = {"extension-module"}


def crate_features() -> tuple[dict[str, list[str]], set[str]]:
    data = tomllib.loads(CARGO.read_text(encoding="utf-8"))
    table = data.get("features", {})
    return table, set(table.get("default", []))


def resolve(names: set[str], table: dict[str, list[str]]) -> set[str]:
    """Expand feature names through their transitive `features` deps."""
    seen: set[str] = set()
    stack = list(names)
    while stack:
        name = stack.pop()
        if name in seen:
            continue
        seen.add(name)
        for dep in table.get(name, []):
            # `dep:foo` and `pkg?/feat` are dependency activations, not
            # features of this crate.
            if "/" in dep or dep.startswith("dep:"):
                continue
            stack.append(dep)
    return seen


def main() -> int:
    table, defaults = crate_features()

    m = RELEASE_RE.search(RELEASE_WF.read_text(encoding="utf-8"))
    if not m:
        print(f"{RELEASE_WF}: no PY_RELEASE_FEATURES — the canonical set moved")
        return 1
    release_named = {f.strip() for f in m.group(1).split(",") if f.strip()}
    # The release build keeps crate defaults; CI does not.
    release = resolve(release_named | defaults, table) - BUILD_ONLY

    ci_text = CI_WF.read_text(encoding="utf-8")
    ci_named: set[str] = set()
    for match in CI_RE.finditer(ci_text):
        flags = match.group("flags")
        feat = re.search(r"--features\s+(\S+)", flags)
        if not feat:
            continue
        named = {f.strip() for f in feat.group(1).split(",") if f.strip()}
        if "--no-default-features" not in flags:
            named |= defaults
        ci_named |= named
    if not ci_named:
        print(f"{CI_WF}: no `maturin develop --features` found — the job moved")
        return 1
    ci = resolve(ci_named, table) - BUILD_ONLY

    missing = sorted(ci - release)
    if missing:
        print(
            f"{RELEASE_WF}: the published wheel would omit "
            f"{', '.join(missing)}, which CI builds and tests. Add them to "
            f"PY_RELEASE_FEATURES, or stop testing them."
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
