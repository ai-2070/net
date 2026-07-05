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

import json

import pytest

pytest.importorskip("net._net")

from net import NetMesh  # noqa: E402

# The gateway is only present when the module was built with both `net` and
# `mcp` (the default wheel is). Skip the whole module cleanly otherwise.
CapabilityGateway = pytest.importorskip("net").__dict__.get("CapabilityGateway")
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
