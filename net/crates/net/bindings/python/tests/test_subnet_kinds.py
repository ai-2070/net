"""SSDK §7.3 — Python's consumer of the shared subnet stable-kind fixture.

Loads ``tests/cross_lang_subnet/stable_kinds.json`` — the SAME file Rust
generates and every binding consumes — and asserts ``net.subnet``'s classifier
recovers each pinned kind. Pure-Python: ``net.subnet.parse_subnet_kind`` needs no
compiled extension, so a kind rename fails here immediately, not after a rebuild.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

subnet = pytest.importorskip("net.subnet", reason="net.subnet not importable")
parse_subnet_kind = subnet.parse_subnet_kind

_FIXTURE = (
    Path(__file__).resolve().parents[3]
    / "tests"
    / "cross_lang_subnet"
    / "stable_kinds.json"
)


def _load() -> dict:
    return json.loads(_FIXTURE.read_text())


def test_fixture_has_the_frozen_shape() -> None:
    f = _load()
    assert f["version"] == 1
    assert f["prefix"] == "subnet:"
    assert len(f["auth_kinds"]) > 0
    assert "unknown_export_name" in f["local_kinds"]
    assert f["fact_kinds"] == [
        "descriptor",
        "gateway_advertisement",
        "export_policy",
        "revocation_floor",
    ]
    assert f["access"] == ["sameOrg", "granted"]


def test_every_pinned_kind_parses_verbatim() -> None:
    f = _load()
    for kind in [*f["auth_kinds"], *f["local_kinds"]]:
        assert parse_subnet_kind(f"subnet:{kind}") == kind


def test_non_subnet_and_empty_kinds_do_not_parse() -> None:
    assert parse_subnet_kind("org:credentials:signature_invalid") is None
    assert parse_subnet_kind("subnet:") is None
    assert parse_subnet_kind("subnet-exported serve registration failed: x") is None


def test_unknown_kind_still_parses_as_data() -> None:
    assert parse_subnet_kind("subnet:kind_from_the_future") == "kind_from_the_future"


def test_kinds_are_globally_unique() -> None:
    f = _load()
    seen: set[str] = set()
    for kind in [*f["auth_kinds"], *f["local_kinds"]]:
        assert kind not in seen, f"duplicate stable kind: {kind}"
        seen.add(kind)
