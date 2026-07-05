"""Tests for the delegated-identity surface (`HERMES_INTEGRATION_PLAN.md`
Phase 3): `DelegationChain`, `RevocationRegistry`, and `derive_child_identity`.

These exercise the thin PyO3 wrappers over `net_sdk::delegation` — the Rust
unit tests own the crypto invariants; here we prove the Python surface derives,
verifies, extends, revokes, and round-trips correctly, and that the H8 boundary
holds (handles + public ids only).
"""

from __future__ import annotations

import pytest

pytest.importorskip("net")

import net  # noqa: E402


def _root_machine_gateway(root=None, host="hostA"):
    """A root plus a machine + gateway derived from it — exactly the shape a
    real deployment uses (deterministic children from the root seed)."""
    root = root or net.Identity.generate()
    machine = net.derive_child_identity(root, f"machine:{host}")
    gateway = net.derive_child_identity(root, f"gateway:{host}:hermes")
    return root, machine, gateway


def test_derive_gateway_and_verify():
    root, machine, gateway = _root_machine_gateway()
    chain = net.DelegationChain.derive_gateway(root, machine, gateway, 3600)
    reg = net.RevocationRegistry()

    assert len(chain) == 2
    assert chain.root == root.entity_id
    assert chain.leaf == gateway.entity_id
    assert chain.verify(gateway.entity_id, root.entity_id, reg) is True


def test_wrong_presenter_is_rejected():
    root, machine, gateway = _root_machine_gateway()
    chain = net.DelegationChain.derive_gateway(root, machine, gateway, 3600)
    reg = net.RevocationRegistry()
    # The machine can't present the gateway's chain (leaf binding fails).
    assert chain.verify(machine.entity_id, root.entity_id, reg) is False


def test_wrong_root_is_rejected():
    root, machine, gateway = _root_machine_gateway()
    chain = net.DelegationChain.derive_gateway(root, machine, gateway, 3600)
    reg = net.RevocationRegistry()
    stranger = net.Identity.generate()
    assert chain.verify(gateway.entity_id, stranger.entity_id, reg) is False


def test_subagent_extension_attributes_and_verifies():
    root, machine, gateway = _root_machine_gateway()
    chain = net.DelegationChain.derive_gateway(root, machine, gateway, 3600)
    subagent = net.Identity.generate()
    sub_chain = chain.extend_to_subagent(gateway, subagent.entity_id)
    reg = net.RevocationRegistry()

    assert len(sub_chain) == 3
    assert sub_chain.leaf == subagent.entity_id
    assert sub_chain.verify(subagent.entity_id, root.entity_id, reg) is True
    # The original chain is untouched by the extension.
    assert len(chain) == 2
    assert chain.verify(gateway.entity_id, root.entity_id, reg) is True


def test_revoke_machine_kills_gateway_and_subagents_but_not_a_sibling():
    root = net.Identity.generate()
    _, m1, g1 = _root_machine_gateway(root, host="host1")
    _, m2, g2 = _root_machine_gateway(root, host="host2")

    c1 = net.DelegationChain.derive_gateway(root, m1, g1, 3600)
    c2 = net.DelegationChain.derive_gateway(root, m2, g2, 3600)
    sub1 = net.Identity.generate()
    c1_sub = c1.extend_to_subagent(g1, sub1.entity_id)

    reg = net.RevocationRegistry()
    assert c1.verify(g1.entity_id, root.entity_id, reg) is True
    assert c1_sub.verify(sub1.entity_id, root.entity_id, reg) is True
    assert c2.verify(g2.entity_id, root.entity_id, reg) is True

    # Revoke machine 1's gateway delegation (bump the machine issuer's floor).
    reg.revoke(m1.entity_id)

    assert c1.verify(g1.entity_id, root.entity_id, reg) is False
    assert c1_sub.verify(sub1.entity_id, root.entity_id, reg) is False
    # Machine 2's chain is untouched.
    assert c2.verify(g2.entity_id, root.entity_id, reg) is True


def test_derive_child_identity_is_deterministic_and_label_separated():
    root = net.Identity.generate()
    a1 = net.derive_child_identity(root, "machine:x").entity_id
    a2 = net.derive_child_identity(root, "machine:x").entity_id
    b = net.derive_child_identity(root, "machine:y").entity_id
    assert a1 == a2  # deterministic from the parent
    assert a1 != b  # label-separated
    # A different parent yields a different child under the same label.
    other = net.Identity.generate()
    assert net.derive_child_identity(other, "machine:x").entity_id != a1


def test_chain_round_trips_through_bytes():
    root, machine, gateway = _root_machine_gateway()
    chain = net.DelegationChain.derive_gateway(root, machine, gateway, 3600)
    parsed = net.DelegationChain.from_bytes(chain.to_bytes())
    reg = net.RevocationRegistry()
    assert parsed.leaf == chain.leaf
    assert parsed.root == chain.root
    assert parsed.verify(gateway.entity_id, root.entity_id, reg) is True


def test_revocation_registry_floor_is_monotonic():
    reg = net.RevocationRegistry()
    issuer = net.Identity.generate().entity_id
    assert reg.floor(issuer) == 0
    reg.revoke_below(issuer, 3)
    assert reg.floor(issuer) == 3
    reg.revoke_below(issuer, 1)  # lower value is a no-op
    assert reg.floor(issuer) == 3


def test_gateway_delegation_channel_is_a_nonempty_string():
    assert isinstance(net.GATEWAY_DELEGATION_CHANNEL, str)
    assert net.GATEWAY_DELEGATION_CHANNEL


# --- caller-side auto-attach (Slice B2) ------------------------------------

_GW = pytest.importorskip("net")  # AsyncCapabilityGateway needs net+mcp


def _mesh():
    return net.NetMesh("127.0.0.1:0", "42" * 32)


def test_async_gateway_accepts_delegation_params(tmp_path):
    import asyncio
    import json

    AsyncCapabilityGateway = getattr(net, "AsyncCapabilityGateway", None)
    if AsyncCapabilityGateway is None:
        pytest.skip("net+mcp features not built")

    mesh = _mesh()
    mesh.start()
    try:
        root = net.Identity.generate()
        machine = net.derive_child_identity(root, "machine:h")
        gateway = net.derive_child_identity(root, "gateway:h:hermes")
        chain = net.DelegationChain.derive_gateway(root, machine, gateway, 3600)
        gw = AsyncCapabilityGateway(
            mesh,
            pin_store_path=str(tmp_path / "pins.json"),
            delegation_leaf=gateway,
            delegation_chain=chain.to_bytes(),
        )

        # On an isolated node search returns structured ok/empty — proving the
        # delegated gateway is wired and callable (no live provider needed).
        async def body():
            return await gw.search("anything")

        res = json.loads(asyncio.run(body()))
        assert res["status"] == "ok"
    finally:
        mesh.shutdown()


def test_gateway_delegation_params_are_both_or_neither(tmp_path):
    AsyncCapabilityGateway = getattr(net, "AsyncCapabilityGateway", None)
    if AsyncCapabilityGateway is None:
        pytest.skip("net+mcp features not built")

    mesh = _mesh()
    mesh.start()
    try:
        gateway = net.derive_child_identity(net.Identity.generate(), "gateway:h")
        with pytest.raises(ValueError):
            AsyncCapabilityGateway(mesh, delegation_leaf=gateway)  # chain missing
    finally:
        mesh.shutdown()
