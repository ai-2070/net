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
    provider = PaymentProvider(
        mesh, str(tmp_path / "engine.json"), unsafe_dev_mock_facilitator=True
    )
    # The provider prices + quotes under the node's own mesh identity.
    assert provider.provider_entity_id == mesh.entity_id
    assert len(provider.provider_entity_id) == 32


def test_publish_paid_tools_requires_pricing(mesh, tmp_path):
    provider = PaymentProvider(
        mesh, str(tmp_path / "engine.json"), unsafe_dev_mock_facilitator=True
    )

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
        unsafe_dev_mock_facilitator=True,
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
        unsafe_dev_mock_facilitator=True,
    )
    assert provider.read_billing() == []


def test_read_billing_without_a_log_is_a_structured_error(mesh, tmp_path):
    # A SEPARATE mesh fixture (function scope) — each PaymentProvider registers
    # the net.payments.quote/pay services, and a second on the same node is
    # rejected (ServeError::AlreadyServing), so the no-log provider needs its own
    # node. Without a billing_log_path, reading is a structured error, not a crash.
    no_log = PaymentProvider(
        mesh, str(tmp_path / "engine.json"), unsafe_dev_mock_facilitator=True
    )
    with pytest.raises(ValueError):
        no_log.read_billing()


def test_publish_paid_tools_fails_closed_on_a_missing_price(mesh, tmp_path):
    # Every tool must be priced — a forgotten entry would publish that tool FREE,
    # so it is a fail-closed ValueError, not a silent free leak.
    provider = PaymentProvider(
        mesh, str(tmp_path / "engine.json"), unsafe_dev_mock_facilitator=True
    )
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


# ---------------------------------------------------------------------------
# H2: a settlement backend must be chosen explicitly
# ---------------------------------------------------------------------------


def test_provider_requires_an_explicit_settlement_backend(mesh, tmp_path):
    """No default backend: constructing without one is an error.

    This constructor used to build a MockFacilitator unconditionally, with
    no way to reach a real one — so a provider could publish priced tools,
    sign quotes with its real mesh identity, emit signed billing events,
    and serve, while settlement moved nothing. Guessing "mock" for an
    operator who has not decided is how a simulator ends up in front of
    real customers.
    """
    with pytest.raises(ValueError) as excinfo:
        PaymentProvider(mesh, str(tmp_path / "engine.json"))
    message = str(excinfo.value)
    assert "no settlement backend" in message
    # The error must name both ways out, not just fail.
    assert "facilitator_url" in message
    assert "unsafe_dev_mock_facilitator" in message


def test_provider_refuses_both_backends_at_once(mesh, tmp_path):
    """A real URL and the mock together is ambiguous, not a precedence rule."""
    with pytest.raises(ValueError) as excinfo:
        PaymentProvider(
            mesh,
            str(tmp_path / "engine.json"),
            facilitator_url="https://facilitator.example.com",
            unsafe_dev_mock_facilitator=True,
        )
    assert "not both" in str(excinfo.value)


def test_a_real_facilitator_url_is_never_silently_downgraded(mesh, tmp_path):
    """Without the payments-http feature, a real URL is a build error.

    The one outcome that must not happen is quietly falling back to the
    mock: an operator who configured a facilitator URL has stated they
    want real settlement. That has to be *asserted* on the success path —
    a silent downgrade constructs successfully too, so "it built" proves
    nothing.

    ``registry_version`` is the observable difference: a real backend puts
    the engine on the production revision, the mock on the dev one (which
    carries the valueless ``mock:net`` asset).
    """
    try:
        provider = PaymentProvider(
            mesh,
            str(tmp_path / "engine.json"),
            facilitator_url="https://facilitator.example.com",
        )
    except ValueError as e:
        # Built without payments-http: must say so, and must not mention
        # having used the mock.
        assert "payments-http" in str(e)
        return
    # Built with payments-http (which the published wheels enable).
    assert provider.registry_version == "net-production-1"


def test_provider_authored_terms_follow_the_provider_registry(mesh, tmp_path):
    """The provider-bound authoring cannot announce a revision it will not
    quote under.

    The free ``build_pricing_terms`` takes the provider id and the registry
    choice as separate arguments, so both can disagree with the provider
    that actually serves the quotes. ``provider.pricing_terms`` takes both
    from the engine, which is why it is the one to reach for.
    """
    provider = PaymentProvider(
        mesh,
        str(tmp_path / "engine.json"),
        unsafe_dev_mock_facilitator=True,
    )
    reqs = json.dumps(
        [
            {
                "scheme": "mock",
                "network": "mock:net",
                "amount": "2500",
                "asset": "musd",
                "payTo": "mock-provider-settle-addr",
                "maxTimeoutSeconds": 60,
            }
        ]
    )
    terms = json.loads(provider.pricing_terms("prov/echo", reqs))
    assert terms["object"] == "net.pricing.terms@1"
    assert terms["capability"] == "prov/echo"

    # Byte-identical to the free function told the truth about this
    # provider — which is the point: the method is the version that cannot
    # be told a lie.
    free = _net.build_pricing_terms(
        bytes(provider.provider_entity_id),
        "prov/echo",
        reqs,
        provider.registry_version == "net-production-1",
    )
    assert json.loads(free) == terms

    # And an asset this provider's registry does not carry is refused at
    # authoring rather than announced and refused later at quote time.
    absent = json.dumps(
        [
            {
                "scheme": "exact",
                "network": "eip155:1",
                "amount": "2500",
                "asset": "0x0000000000000000000000000000000000000001",
                "payTo": "0x0000000000000000000000000000000000000002",
                "maxTimeoutSeconds": 60,
            }
        ]
    )
    with pytest.raises(ValueError):
        provider.pricing_terms("prov/echo", absent)


def test_the_mock_backend_says_so_in_the_registry_revision(mesh, tmp_path):
    """The other side of the same guarantee.

    Asking for the mock gets the mock, so ``registry_version`` genuinely
    discriminates rather than always reading "production".
    """
    provider = PaymentProvider(
        mesh,
        str(tmp_path / "engine.json"),
        unsafe_dev_mock_facilitator=True,
    )
    assert provider.registry_version == "net-default-1"
