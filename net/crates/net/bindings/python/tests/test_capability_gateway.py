"""Native consent-gated CapabilityGateway binding tests
(`HERMES_INTEGRATION_PLAN.md` Phase 1 enabler).

Build the extension with the default (net + mcp) features::

    maturin develop

These exercise the *binding contract*: that a first-class SDK node can call
``search`` / ``describe`` / ``invoke`` through the shared consent gate and get a
structured ``status`` result rather than an exception. The gate *logic* (the
requires-approval / validation / denied decisions) is pinned in Rust by
``net-mesh-mcp``'s ``serve::gated`` unit tests and the cross-node
``serve_end_to_end.rs`` gateway test — the native gateway drives the exact same
``MeshGateway`` + ``gated_invoke``, so those decisions are covered transitively.
A full live search -> approve -> invoke against a wrapped provider needs a
second node running ``net wrap`` (see ``tools/hermes-gate/probe_full_gate.py``).
"""

from __future__ import annotations

import asyncio
import json

import pytest

pytest.importorskip("net._net")

from net import NetMesh  # noqa: E402

# The gateway is only present when the module was built with both `net` and
# `mcp` (the default wheel is). Skip the whole module cleanly otherwise.
_net = pytest.importorskip("net")
CapabilityGateway = _net.__dict__.get("CapabilityGateway")
AsyncCapabilityGateway = _net.__dict__.get("AsyncCapabilityGateway")
if CapabilityGateway is None:
    pytest.skip("CapabilityGateway not built (needs net+mcp features)", allow_module_level=True)

PSK = "42" * 32


@pytest.fixture()
def mesh():
    """A single isolated mesh node (ephemeral port), torn down after the test."""
    m = NetMesh("127.0.0.1:0", PSK)
    try:
        yield m
    finally:
        m.shutdown()


@pytest.fixture()
def gateway(mesh, tmp_path):
    return CapabilityGateway(mesh, pin_store_path=str(tmp_path / "pins.json"))


def test_pin_store_path_round_trips(gateway, tmp_path):
    assert gateway.pin_store_path == str(tmp_path / "pins.json")
    assert "CapabilityGateway" in repr(gateway)


def test_search_on_an_empty_mesh_is_ok_and_empty(gateway):
    # No providers reachable -> an empty index is a success, never an error.
    result = json.loads(gateway.search("anything"))
    assert result == {"status": "ok", "capabilities": []}


def test_gateway_without_a_pin_store_still_searches(mesh):
    gw = CapabilityGateway(mesh)
    assert gw.pin_store_path is None
    assert json.loads(gw.search(""))["status"] == "ok"


def test_describe_of_an_unreachable_provider_is_structured(gateway):
    # Provider node 42 isn't connected: a structured transport/not-found error,
    # not a raised exception.
    result = json.loads(gateway.describe("42/echo"))
    assert result["status"] in {"transport_error", "not_found"}
    assert "error" in result


def test_invoke_of_an_unreachable_provider_is_structured(gateway):
    result = json.loads(gateway.invoke("42/echo", json.dumps({"message": "hi"})))
    assert result["status"] in {"transport_error", "not_found"}
    assert "error" in result


def test_invoke_defaults_to_empty_arguments(gateway):
    # arguments_json defaults to "{}" — a no-arg invoke is well-formed and only
    # fails at the (unreachable) provider, not on argument parsing.
    result = json.loads(gateway.invoke("42/echo"))
    assert result["status"] in {"transport_error", "not_found"}


def test_malformed_capability_id_is_a_structured_error(gateway):
    for method in (gateway.describe, lambda cid: gateway.invoke(cid, "{}")):
        result = json.loads(method("bareword"))
        assert result["status"] == "invalid_capability_id", result


def test_malformed_arguments_are_a_structured_error(gateway):
    result = json.loads(gateway.invoke("42/echo", "not json"))
    assert result["status"] == "invalid_arguments"
    assert "error" in result


def test_every_surface_returns_valid_json(gateway):
    # The whole contract: every method hands back a JSON string with a `status`.
    for raw in (
        gateway.search("x"),
        gateway.describe("42/echo"),
        gateway.invoke("42/echo", "{}"),
    ):
        parsed = json.loads(raw)
        assert isinstance(parsed, dict)
        assert "status" in parsed


# ---------------------------------------------------------------------------
# Awaitable dual — same contract, driven by the event loop. Each method spawns
# the gateway op on the mesh's own runtime, so mesh I/O stays on the right
# reactor; asserting an unreachable provider resolves to a structured error
# (rather than hanging or raising a reactor-affinity error) is the real check.
# ---------------------------------------------------------------------------


@pytest.fixture()
def async_gateway(mesh, tmp_path):
    return AsyncCapabilityGateway(mesh, pin_store_path=str(tmp_path / "pins.json"))


def test_async_pin_store_path_round_trips(async_gateway, tmp_path):
    assert async_gateway.pin_store_path == str(tmp_path / "pins.json")
    assert "AsyncCapabilityGateway" in repr(async_gateway)


def test_async_search_on_an_empty_mesh_is_ok_and_empty(async_gateway):
    async def body():
        return json.loads(await async_gateway.search("anything"))

    result = asyncio.run(body())
    assert result == {"status": "ok", "capabilities": []}


def test_async_describe_and_invoke_of_unreachable_are_structured(async_gateway):
    async def body():
        d = json.loads(await async_gateway.describe("42/echo"))
        i = json.loads(await async_gateway.invoke("42/echo", json.dumps({"m": 1})))
        return d, i

    described, invoked = asyncio.run(body())
    assert described["status"] in {"transport_error", "not_found"}
    assert invoked["status"] in {"transport_error", "not_found"}


def test_async_concurrent_invokes_all_resolve(async_gateway):
    async def body():
        raws = await asyncio.gather(
            *(async_gateway.invoke("42/echo", "{}") for _ in range(5))
        )
        return [json.loads(r) for r in raws]

    results = asyncio.run(body())
    assert len(results) == 5
    assert all(r["status"] in {"transport_error", "not_found"} for r in results)


def test_async_malformed_id_and_arguments_are_structured(async_gateway):
    async def body():
        bad_id = json.loads(await async_gateway.invoke("bareword", "{}"))
        bad_args = json.loads(await async_gateway.invoke("42/echo", "not json"))
        return bad_id, bad_args

    bad_id, bad_args = asyncio.run(body())
    assert bad_id["status"] == "invalid_capability_id"
    assert bad_args["status"] == "invalid_arguments"


# ---------------------------------------------------------------------------
# Payments (PAYMENTS_SDK_PLAN.md P0): the gateway accepts the payment kwargs
# and keeps every result structured. The payment *decisions* are pinned in
# Rust (net-payments flow_end_to_end / mcp_gate_composition tests + the
# binding-level outcome_to_json contract test); these assert the Python
# surface: construction, validation, and unchanged behavior for free tools.
# ---------------------------------------------------------------------------


def test_payment_kwargs_construct_a_gateway(mesh, tmp_path):
    try:
        gw = CapabilityGateway(
            mesh,
            pin_store_path=str(tmp_path / "pins.json"),
            payment_policy_path=str(tmp_path / "payment-policy.json"),
            payment_profile="dev_test",
        )
    except ValueError as e:
        pytest.skip(f"build lacks the payments feature: {e}")
    # Free-tool behavior is unchanged: structured results, never exceptions.
    assert json.loads(gw.search(""))["status"] == "ok"
    result = json.loads(gw.invoke("42/echo", "{}"))
    assert result["status"] in {"transport_error", "not_found"}


def test_payment_profile_without_policy_path_is_a_config_error(mesh):
    with pytest.raises(ValueError):
        CapabilityGateway(mesh, payment_profile="dev_test")


def test_unknown_payment_profile_is_a_config_error(mesh, tmp_path):
    with pytest.raises(ValueError):
        CapabilityGateway(
            mesh,
            payment_policy_path=str(tmp_path / "payment-policy.json"),
            payment_profile="yolo",
        )


def test_async_gateway_accepts_payment_kwargs(mesh, tmp_path):
    try:
        gw = AsyncCapabilityGateway(
            mesh,
            pin_store_path=str(tmp_path / "pins.json"),
            payment_policy_path=str(tmp_path / "payment-policy.json"),
            payment_profile="production",
        )
    except ValueError as e:
        pytest.skip(f"build lacks the payments feature: {e}")

    async def roundtrip():
        return json.loads(await gw.search(""))

    assert asyncio.run(roundtrip())["status"] == "ok"


# ---------------------------------------------------------------------------
# Operator approval verbs (PAYMENTS_LANGUAGE_SDKS_PLAN WS-P1): approve /
# reject / pending / spent_today over the shared spend-policy store, so an
# agent can resolve its own `requires_payment_approval` under operator
# policy. The store, lock protocol, and Pending->Approved transition are
# pinned in Rust (SpendPolicyEngine + the binding's do_* driven tests);
# these assert the Python surface marshals the store round-trip.
# ---------------------------------------------------------------------------


@pytest.fixture()
def paid_gateway(mesh, tmp_path):
    try:
        return CapabilityGateway(
            mesh,
            pin_store_path=str(tmp_path / "pins.json"),
            payment_policy_path=str(tmp_path / "payment-policy.json"),
            payment_profile="dev_test",
        )
    except ValueError as e:  # noqa: F841
        pytest.skip("build lacks the payments feature")


def test_approval_verbs_round_trip_on_the_shared_store(paid_gateway):
    # Fresh store: nothing pending, nothing spent today.
    pending = json.loads(paid_gateway.pending_payments())
    assert pending["status"] == "ok"
    assert pending["pending"] == []

    spent = json.loads(paid_gateway.spent_today("mock:net", "musd"))
    assert spent["status"] == "ok"
    assert spent["spent"] == "0"

    # Approve a quote id: the record moves to approved (changed), and a
    # second approve is idempotent.
    approved = json.loads(paid_gateway.approve_payment("q-1"))
    assert approved["status"] == "ok"
    assert approved["quote_id"] == "q-1"
    assert approved["changed"] is True
    assert json.loads(paid_gateway.approve_payment("q-1"))["changed"] is False

    # Reject removes it (changed), then a second reject is a no-op.
    assert json.loads(paid_gateway.reject_payment("q-1"))["changed"] is True
    assert json.loads(paid_gateway.reject_payment("q-1"))["changed"] is False


def test_approval_verbs_without_policy_path_are_structured(gateway):
    # A gateway built without payment_policy_path can't approve anything —
    # a structured `no_payment_policy` (payments build) / `unsupported`
    # (feature absent), never a raised exception.
    for raw in (
        gateway.pending_payments(),
        gateway.approve_payment("q"),
        gateway.reject_payment("q"),
        gateway.spent_today("mock:net", "musd"),
    ):
        parsed = json.loads(raw)
        assert parsed["status"] in {"no_payment_policy", "unsupported"}


def test_async_approval_verbs_round_trip(mesh, tmp_path):
    try:
        gw = AsyncCapabilityGateway(
            mesh,
            payment_policy_path=str(tmp_path / "payment-policy.json"),
            payment_profile="dev_test",
        )
    except ValueError:
        pytest.skip("build lacks the payments feature")

    async def body():
        approved = json.loads(await gw.approve_payment("q-async"))
        pending = json.loads(await gw.pending_payments())
        rejected = json.loads(await gw.reject_payment("q-async"))
        return approved, pending, rejected

    approved, pending, rejected = asyncio.run(body())
    assert approved["status"] == "ok" and approved["changed"] is True
    # The approved quote is not *pending* (it's approved), so the list is empty.
    assert pending["status"] == "ok" and pending["pending"] == []
    assert rejected["changed"] is True


# ---------------------------------------------------------------------------
# The settlement signer *reference* surface (P1 WS2): the payer address plus
# a callable that signs EIP-712 typed data. The contract pinned here: the
# pair is both-or-neither, needs the policy store, must be callable — and
# there is NO kwarg that accepts key material (the negative test is that
# these four are the entire payment surface).
# ---------------------------------------------------------------------------


def test_payment_signer_reference_constructs_a_gateway(mesh, tmp_path):
    def signer(typed_data_json: str) -> str:
        raise AssertionError("never invoked at construction")

    try:
        gw = CapabilityGateway(
            mesh,
            payment_policy_path=str(tmp_path / "payment-policy.json"),
            payment_signer_address="0x209693Bc6afc0C5328bA36FaF03C514EF312287C",
            payment_signer=signer,
        )
    except ValueError as e:
        pytest.skip(f"build lacks the payments feature: {e}")
    # Construction wires the signer but never calls it; free tools unchanged.
    assert json.loads(gw.search(""))["status"] == "ok"


def test_payment_signer_pair_is_both_or_neither(mesh, tmp_path):
    policy = str(tmp_path / "payment-policy.json")
    with pytest.raises(ValueError):
        CapabilityGateway(
            mesh, payment_policy_path=policy, payment_signer_address="0xpayer"
        )
    with pytest.raises(ValueError):
        CapabilityGateway(
            mesh, payment_policy_path=policy, payment_signer=lambda t: "0x"
        )


def test_payment_signer_requires_the_policy_path(mesh):
    with pytest.raises(ValueError):
        CapabilityGateway(
            mesh,
            payment_signer_address="0xpayer",
            payment_signer=lambda t: "0x",
        )


def test_payment_signer_must_be_callable(mesh, tmp_path):
    with pytest.raises(ValueError):
        CapabilityGateway(
            mesh,
            payment_policy_path=str(tmp_path / "payment-policy.json"),
            payment_signer_address="0xpayer",
            payment_signer="not-a-callable",
        )


def test_no_payment_kwarg_accepts_key_material(mesh, tmp_path):
    """The key-invariant negative test, binding-level: a private key is
    unrepresentable on this surface. Any kwarg smelling of key bytes is
    rejected as an unexpected argument — the signer *reference* (address +
    callable) is the only way in, and the callable only ever sees typed
    data."""
    for kwarg in (
        "payment_private_key",
        "payment_secret",
        "payment_key_bytes",
        # …extended to the svm/xrpl seams: still no key kwarg anywhere.
        "payment_signer_svm_key",
        "payment_signer_xrpl_secret",
    ):
        with pytest.raises(TypeError):
            CapabilityGateway(
                mesh,
                payment_policy_path=str(tmp_path / "payment-policy.json"),
                **{kwarg: b"\x11" * 32},
            )


# ---------------------------------------------------------------------------
# The svm/xrpl signer seams (PAYMENTS_LANGUAGE_SDKS_PLAN WS-P3): the same
# reference shape as eip155 under their own namespaces — a payer address plus
# a `(intent_json: str) -> str` callable (base64 SVM tx / hex XRPL blob out).
# The contract: each pair is both-or-neither, the callable only ever sees a
# typed intent (never key material), and all three schemes coexist.
# ---------------------------------------------------------------------------


def test_svm_signer_reference_constructs_a_gateway(mesh, tmp_path):
    def signer(intent_json: str) -> str:
        raise AssertionError("never invoked at construction")

    try:
        gw = CapabilityGateway(
            mesh,
            payment_policy_path=str(tmp_path / "payment-policy.json"),
            payment_signer_svm_address="So11111111111111111111111111111111111111112",
            payment_signer_svm=signer,
        )
    except ValueError as e:
        pytest.skip(f"build lacks the payments feature: {e}")
    assert json.loads(gw.search(""))["status"] == "ok"


def test_xrpl_signer_reference_constructs_a_gateway(mesh, tmp_path):
    def signer(intent_json: str) -> str:
        raise AssertionError("never invoked at construction")

    try:
        gw = CapabilityGateway(
            mesh,
            payment_policy_path=str(tmp_path / "payment-policy.json"),
            payment_signer_xrpl_address="rPT1Sjq2YGrBMTttX4GZHjKu9dyfzbpAYe",
            payment_signer_xrpl=signer,
        )
    except ValueError as e:
        pytest.skip(f"build lacks the payments feature: {e}")
    assert json.loads(gw.search(""))["status"] == "ok"


def test_svm_and_xrpl_signer_pairs_are_both_or_neither(mesh, tmp_path):
    policy = str(tmp_path / "payment-policy.json")
    for addr_kw, call_kw in (
        ("payment_signer_svm_address", "payment_signer_svm"),
        ("payment_signer_xrpl_address", "payment_signer_xrpl"),
    ):
        with pytest.raises(ValueError):
            CapabilityGateway(mesh, payment_policy_path=policy, **{addr_kw: "addr"})
        with pytest.raises(ValueError):
            CapabilityGateway(
                mesh, payment_policy_path=policy, **{call_kw: lambda t: "sig"}
            )


def test_all_three_signer_schemes_coexist(mesh, tmp_path):
    try:
        gw = CapabilityGateway(
            mesh,
            payment_policy_path=str(tmp_path / "payment-policy.json"),
            payment_signer_address="0x209693Bc6afc0C5328bA36FaF03C514EF312287C",
            payment_signer=lambda t: "0x",
            payment_signer_svm_address="So11111111111111111111111111111111111111112",
            payment_signer_svm=lambda t: "base64tx",
            payment_signer_xrpl_address="rPT1Sjq2YGrBMTttX4GZHjKu9dyfzbpAYe",
            payment_signer_xrpl=lambda t: "hexblob",
        )
    except ValueError as e:
        pytest.skip(f"build lacks the payments feature: {e}")
    assert json.loads(gw.search(""))["status"] == "ok"
