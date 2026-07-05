"""Plugin delegated-identity tests (Phase 3, Slice A).

Covers ``delegation.GatewayDelegation`` (derive / verify / subagent / revoke /
per-machine isolation) and the ``node.check_net_available`` gating on chain
validity. The crypto invariants are pinned in Rust + the binding tests; here we
prove the plugin orchestrates them correctly and that a revoked delegation
removes the tools (check_fn False).
"""

from __future__ import annotations

import pytest

pytest.importorskip("net")
pytest.importorskip("net_sdk")

_SEED = bytes(range(32))  # any 32 bytes is a valid ed25519 seed


def test_gateway_delegation_derives_and_verifies(plugin):
    gd = plugin.delegation.GatewayDelegation(_SEED, machine_label="hostA")
    assert gd.verify() is True
    assert len(gd.chain) == 2
    assert len(gd.gateway_id) == 32
    assert len(gd.root_id) == 32
    assert len(gd.machine_id) == 32
    # The leaf the chain attributes to is the gateway.
    assert gd.chain.leaf == gd.gateway_id
    assert gd.chain.root == gd.root_id


def test_revoke_gateway_makes_verify_false(plugin):
    gd = plugin.delegation.GatewayDelegation(_SEED, machine_label="hostA")
    assert gd.verify() is True
    gd.revoke_gateway()
    assert gd.verify() is False


def test_subagent_extension_verifies(plugin):
    import net

    gd = plugin.delegation.GatewayDelegation(_SEED, machine_label="hostA")
    subagent = net.Identity.generate()
    sub_chain = gd.delegate_subagent(subagent.entity_id)
    assert len(sub_chain) == 3
    assert sub_chain.leaf == subagent.entity_id
    assert sub_chain.verify(subagent.entity_id, gd.root_id, gd.registry) is True


def test_two_machines_under_one_root_are_isolated(plugin):
    # Same user root, two machines: revoking one gateway must not touch the
    # other — the Phase-3 acceptance.
    gd1 = plugin.delegation.GatewayDelegation(_SEED, machine_label="host1")
    gd2 = plugin.delegation.GatewayDelegation(_SEED, machine_label="host2")
    assert gd1.verify() is True
    assert gd2.verify() is True

    gd1.revoke_gateway()
    assert gd1.verify() is False
    assert gd2.verify() is True


def test_check_net_available_gates_on_delegation_validity(plugin, monkeypatch):
    # Inject a state whose delegation is a real GatewayDelegation (the mesh /
    # gateway / path slots are unused by the delegation-gating branch), so we
    # exercise node's check_fn without building a seeded mesh.
    node = plugin.node
    gd = plugin.delegation.GatewayDelegation(_SEED, machine_label="gate-test")
    monkeypatch.setattr(node, "_state", (object(), object(), "unused", gd))

    assert node.check_net_available() is True
    gd.revoke_gateway()
    # A revoked gateway delegation removes the tools (never invoke under an
    # invalid chain).
    assert node.check_net_available() is False


def test_isolated_node_runs_undelegated(node_ready):
    # The session node is built with no NET_MESH_IDENTITY_SEED ⇒ un-delegated;
    # its tools still load (delegation is opt-in via the root seed).
    assert node_ready.delegation() is None
    assert node_ready.check_net_available() is True


def test_gateway_delegation_exposes_the_signer_inputs(plugin):
    # The gateway leaf Identity + chain bytes node.py hands to the
    # AsyncCapabilityGateway (Slice B2 auto-attach) must be self-consistent.
    import net

    gd = plugin.delegation.GatewayDelegation(_SEED, machine_label="hostA")
    # The leaf handle's entity-id is the chain leaf.
    assert gd.gateway_identity.entity_id == gd.gateway_id
    cb = gd.chain_bytes()
    assert isinstance(cb, (bytes, bytearray)) and len(cb) > 0
    assert net.DelegationChain.from_bytes(cb).leaf == gd.gateway_id
