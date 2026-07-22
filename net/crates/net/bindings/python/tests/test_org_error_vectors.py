"""OSDK-L X1/P — Python's consumer of the shared org error vocabulary.

Loads ``tests/cross_lang_org/error_vectors.json`` — the SAME fixture Rust
generates and consumes — and asserts this binding's ``parse_org_error``
recovers the identical domain, kind, and local/remote verdict.

Pure-Python: ``net.org.parse_org_error`` needs no compiled extension, so this
runs even on a partial or unbuilt wheel, following ``test_abi_stability.py``. A
vocabulary rename fails here immediately, not after a rebuild.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

# The classifier is pure Python and importable without the native module.
org = pytest.importorskip("net.org", reason="net.org module not importable")
parse_org_error = org.parse_org_error

_FIXTURE = (
    Path(__file__).resolve().parents[3]
    / "tests"
    / "cross_lang_org"
    / "error_vectors.json"
)


def _load() -> dict:
    return json.loads(_FIXTURE.read_text())


def test_fixture_has_the_expected_shape() -> None:
    doc = _load()
    assert doc["version"] == 1
    assert doc["prefix"] == "org:"
    assert len(doc["vectors"]) > 0
    assert len(doc["unclassified_cases"]) > 0


def test_every_vector_parses_back_to_its_declared_domain_and_kind() -> None:
    for v in _load()["vectors"]:
        parsed = parse_org_error(v["wire"])
        assert parsed.domain == v["domain"], v["wire"]
        assert parsed.kind == v["kind"], v["wire"]
        assert parsed.is_local == v["is_local"], v["wire"]


def test_unclassified_cases_never_impersonate_a_canonical_domain() -> None:
    """The property a misclassification would destroy: an unparseable or
    unknown-vocabulary string must classify as ``unknown``, never one of the
    four canonical domains — that would assert a request reached a provider."""
    for c in _load()["unclassified_cases"]:
        parsed = parse_org_error(c["wire"])
        assert parsed.domain == "unknown", c["wire"]
        assert parsed.domain == c["expect_domain"], c["wire"]
        assert parsed.kind is None, c["wire"]
        assert parsed.is_local is False, c["wire"]


def test_agrees_with_the_fixture_on_which_domains_are_local() -> None:
    doc = _load()
    by_domain = {d["token"]: d["is_local"] for d in doc["domains"]}
    for v in doc["vectors"]:
        parsed = parse_org_error(v["wire"])
        assert parsed.is_local == by_domain[v["domain"]], v["wire"]
    # Spot-pin the split — the fact a misclassification would move.
    assert parse_org_error("org:credentials:x").is_local is True
    assert parse_org_error("org:discovery:x").is_local is True
    assert parse_org_error("org:admission_denied:denied").is_local is False
    assert parse_org_error("org:rpc:timeout").is_local is False


def test_admission_denials_carry_only_the_coarse_bucket() -> None:
    doc = _load()
    denials = [v for v in doc["vectors"] if v["domain"] == "admission_denied"]
    assert len(denials) == 3
    for v in denials:
        parsed = parse_org_error(v["wire"])
        assert parsed.kind == v["kind"]
        assert parsed.kind in ("denied", "not_supported", "unavailable")
        # org:<domain>:<bucket> — no trailing detail.
        assert v["wire"].count(":") == 2, v["wire"]


def test_a_non_org_string_classifies_as_unknown() -> None:
    parsed = parse_org_error("nrpc:timeout: elapsed_ms=5000")
    assert parsed.domain == "unknown"
    assert parsed.kind is None
    assert parsed.is_local is False
