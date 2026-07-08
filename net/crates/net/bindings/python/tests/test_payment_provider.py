"""Provider-side payment surface (supply): authoring net.pricing.terms@1.

Build the extension with the default (payments) features::

    maturin develop

The pricing/settlement logic is pinned in Rust (net-payments + the binding's
payment_provider unit tests); these assert the Python surface — that
build_pricing_terms authors a canonical, decodable terms string and rejects
bad input. Skips cleanly if the module was built without payments.
"""

from __future__ import annotations

import json

import pytest

_net = pytest.importorskip("net")
build_pricing_terms = _net.__dict__.get("build_pricing_terms")
if build_pricing_terms is None:
    pytest.skip("build_pricing_terms not built (needs the payments feature)", allow_module_level=True)

ENTITY_ID = bytes(range(32))  # a fixed 32-byte fixture id
MOCK_REQS = [
    {
        "scheme": "mock",
        "network": "mock:net",
        "amount": "2500",
        "asset": "musd",
        "payTo": "mock-provider-settle-addr",
        "maxTimeoutSeconds": 60,
    }
]


def test_authors_canonical_pricing_terms():
    terms = build_pricing_terms(ENTITY_ID, "prov/echo", json.dumps(MOCK_REQS))
    parsed = json.loads(terms)
    assert parsed["object"] == "net.pricing.terms@1"
    assert parsed["capability"] == "prov/echo"
    assert len(parsed["accepts"]) == 1
    # Canonical: sorted keys, compact separators, raw UTF-8 — re-emitting is a
    # fixed point (the byte-preservation regime the golden vectors pin).
    reemit = json.dumps(parsed, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
    assert reemit == terms


def test_multiple_accepts_are_preserved():
    reqs = MOCK_REQS + [{**MOCK_REQS[0], "amount": "5000"}]
    parsed = json.loads(build_pricing_terms(ENTITY_ID, "prov/echo", json.dumps(reqs)))
    assert len(parsed["accepts"]) == 2


def test_bad_input_raises_value_error():
    with pytest.raises(ValueError):
        build_pricing_terms(ENTITY_ID, "prov/echo", "[]")  # empty prices nothing
    with pytest.raises(ValueError):
        build_pricing_terms(ENTITY_ID, "prov/echo", "not json")
    with pytest.raises(ValueError):
        build_pricing_terms(b"\x00" * 31, "prov/echo", json.dumps(MOCK_REQS))  # not 32 bytes
