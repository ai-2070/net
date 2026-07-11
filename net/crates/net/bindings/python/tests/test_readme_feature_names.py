"""README-vs-Cargo.toml feature-name drift test.

Every ``--features "..."`` incantation in this binding's README must
name features that actually exist in ``bindings/python/Cargo.toml``.
Regression for a documented dev command that silently rotted:
``maturin develop --features "cortex netdb redex-disk meshdb meshos"``
named the *underlying crate's* ``netdb`` / ``redex-disk`` features,
which this package folds into ``cortex`` — so the command failed with
``the package 'net-python' does not contain these features``.

Pure text parsing — no cargo, no maturin, no built wheel required.
"""

from __future__ import annotations

import re
from pathlib import Path

THIS_DIR = Path(__file__).parent
PKG_ROOT = THIS_DIR.parent
README = PKG_ROOT / "README.md"
CARGO_TOML = PKG_ROOT / "Cargo.toml"


def _declared_features() -> set[str]:
    """Feature names declared in ``[features]``: every ``name =``
    key between the section header and the next section."""
    text = CARGO_TOML.read_text()
    match = re.search(r"^\[features\]\n(.*?)(?=^\[)", text, re.M | re.S)
    assert match, "Cargo.toml must have a [features] section"
    names = set(re.findall(r"^([A-Za-z0-9_-]+)\s*=", match.group(1), re.M))
    assert names, "no features parsed — the parser regressed"
    return names


def _readme_feature_lists() -> list[tuple[str, list[str]]]:
    """Every ``--features <list>`` occurrence in the README, as
    ``(raw_occurrence, [feature, ...])``. Handles quoted
    space-separated lists and bare comma/space-separated ones."""
    text = README.read_text()
    out: list[tuple[str, list[str]]] = []
    for m in re.finditer(r'--features[= ]+(?:"([^"]+)"|\'([^\']+)\'|([A-Za-z0-9_,-]+))', text):
        raw = m.group(0)
        blob = next(g for g in m.groups() if g)
        # `--features "<list>"`-style placeholders document the flag's
        # shape, not concrete names — skip them.
        if "<" in blob:
            continue
        feats = [f for f in re.split(r"[,\s]+", blob) if f]
        out.append((raw, feats))
    return out


def test_readme_feature_lists_name_real_features() -> None:
    declared = _declared_features()
    occurrences = _readme_feature_lists()
    assert occurrences, "README no longer documents any --features command"
    for raw, feats in occurrences:
        unknown = [f for f in feats if f not in declared]
        assert not unknown, (
            f"README documents {raw!r}, but {unknown} are not features of "
            f"net-python (see [features] in bindings/python/Cargo.toml). "
            f"Crate-level features must be reached through a binding "
            f"feature that forwards them (e.g. cortex = ['net/netdb', ...])."
        )
