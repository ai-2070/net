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


# ---------------------------------------------------------------------------
# PaymentProvider (WS-A2): a Python node that prices + charges for its tools.
# The paid serve/settle composition is pinned in Rust (net-payments
# mcp_wrap_paid_e2e / mesh_paid_capability_e2e — the same PaymentEngine +
# serve_payments + EnginePaymentAdmission this binding constructs); these
# assert the Python surface: construction, the identity it prices under, the
# priced-publish path, and the fail-closed guard. Needs net + mcp + payments +
# publish (the default wheel); skips otherwise.
# ---------------------------------------------------------------------------

NetMesh = _net.__dict__.get("NetMesh")
PaymentProvider = _net.__dict__.get("PaymentProvider")
_HAVE_PROVIDER = NetMesh is not None and PaymentProvider is not None
PSK = "42" * 32


@pytest.fixture()
def mesh():
    if not _HAVE_PROVIDER:
        pytest.skip("PaymentProvider not built (needs net+mcp+payments+publish)")
    m = NetMesh("127.0.0.1:0", PSK)
    m.start()
    try:
        yield m
    finally:
        m.shutdown()


def test_provider_prices_under_the_node_identity(mesh, tmp_path):
    provider = PaymentProvider(mesh, str(tmp_path / "engine.json"))
    # The provider prices + quotes under the node's own mesh identity.
    assert provider.provider_entity_id == mesh.entity_id
    assert len(provider.provider_entity_id) == 32


def test_publish_paid_tools_requires_pricing(mesh, tmp_path):
    provider = PaymentProvider(mesh, str(tmp_path / "engine.json"))

    async def cb(_name, _args_json):
        return "ok"

    # An empty pricing map is fail-closed (use publish_tools for free tools).
    with pytest.raises(ValueError):
        provider.publish_paid_tools([("echo", None, "{}")], cb, {})


def test_publish_paid_tools_serves_a_priced_tool(mesh, tmp_path):
    provider = PaymentProvider(
        mesh,
        str(tmp_path / "engine.json"),
        billing_log_path=str(tmp_path / "billing.jsonl"),
    )
    # The announced price for the "echo" tool, under this node's capability id.
    capability = f"{mesh.node_id}/echo"
    terms = build_pricing_terms(provider.provider_entity_id, capability, json.dumps(MOCK_REQS))

    async def cb(_name, _args_json):
        return "echoed"

    handle = provider.publish_paid_tools(
        [("echo", "Echo a value.", '{"type":"object"}')],
        cb,
        {"echo": terms},
    )
    try:
        # The priced tool is served (its channel-safe id is in the handle).
        assert handle.tools, "the priced tool is served"
    finally:
        handle.stop()


def test_read_billing(mesh, tmp_path):
    # A fresh provider with a billing log reads an empty stream (no serves yet).
    provider = PaymentProvider(
        mesh,
        str(tmp_path / "engine.json"),
        billing_log_path=str(tmp_path / "billing.jsonl"),
    )
    assert provider.read_billing() == []


def test_read_billing_without_a_log_is_a_structured_error(mesh, tmp_path):
    # A SEPARATE mesh fixture (function scope) — each PaymentProvider registers
    # the net.payments.quote/pay services, and a second on the same node is
    # rejected (ServeError::AlreadyServing), so the no-log provider needs its own
    # node. Without a billing_log_path, reading is a structured error, not a crash.
    no_log = PaymentProvider(mesh, str(tmp_path / "engine.json"))
    with pytest.raises(ValueError):
        no_log.read_billing()


def test_publish_paid_tools_fails_closed_on_a_missing_price(mesh, tmp_path):
    # Every tool must be priced — a forgotten entry would publish that tool FREE,
    # so it is a fail-closed ValueError, not a silent free leak.
    provider = PaymentProvider(mesh, str(tmp_path / "engine.json"))
    terms = build_pricing_terms(
        provider.provider_entity_id, f"{mesh.node_id}/echo", json.dumps(MOCK_REQS)
    )

    async def cb(_name, _args_json):
        return "echoed"

    with pytest.raises(ValueError):
        # Two tools, only one priced → the unpriced `other` would go out free.
        provider.publish_paid_tools(
            [
                ("echo", None, '{"type":"object"}'),
                ("other", None, '{"type":"object"}'),
            ],
            cb,
            {"echo": terms},
        )
