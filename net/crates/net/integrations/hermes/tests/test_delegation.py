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


def test_store_revocation_makes_verify_false(plugin, tmp_path):
    # Provider-parity: an operator's `net identity revoke` written to the
    # machine-shared store is observed by the caller-side self-check, so a revoked
    # gateway's chain fails verify() (and thus check_fn) — not only rejected at
    # the provider.
    import json

    store = tmp_path / "delegation-revocations.json"
    gd = plugin.delegation.GatewayDelegation(
        _SEED, machine_label="hostA", revocation_store_path=str(store)
    )
    assert gd.verify() is True  # no store file yet → empty → still valid

    # An operator revokes this machine's gateway delegation in the shared store.
    store.write_text(
        json.dumps({"floors": [{"issuer": gd.machine_id.hex(), "generation": 1}]})
    )
    # verify() reloads the store on each call and now observes the revocation.
    assert gd.verify() is False


def test_verify_survives_a_corrupt_revocation_store(plugin, tmp_path):
    # A store that can't be parsed must not crash verify() or open a hole: keep
    # the last-known floors (here none), so a still-valid chain stays valid rather
    # than failing shut or raising out of check_fn.
    store = tmp_path / "delegation-revocations.json"
    store.write_text("{ not valid json")
    gd = plugin.delegation.GatewayDelegation(
        _SEED, machine_label="hostA", revocation_store_path=str(store)
    )
    assert gd.verify() is True


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


def test_machine_label_is_unique_and_persistent(plugin, monkeypatch, tmp_path):
    # The default machine label must be unique + persistent (not the unstable
    # hostname), so two machines sharing a hostname don't derive identical
    # machine/gateway identities from the same root.
    delegation = plugin.delegation
    monkeypatch.setenv("NET_MESH_PIN_STORE", str(tmp_path / "pins.json"))
    monkeypatch.delenv("NET_MESH_MACHINE_ID", raising=False)

    label1 = delegation._machine_label()
    label2 = delegation._machine_label()
    assert label1 == label2, "label must be stable across calls"
    assert label1 and label1 != "unknown-host"
    # It embeds a persistent random id, not just the bare hostname.
    pid = delegation._persistent_machine_id()
    assert pid and pid in label1


def test_explicit_machine_id_overrides_the_default(plugin, monkeypatch):
    delegation = plugin.delegation
    monkeypatch.setenv("NET_MESH_MACHINE_ID", "my-unique-box-42")
    assert delegation._machine_label() == "my-unique-box-42"


def test_invoke_refuses_when_the_delegation_is_invalid(plugin, monkeypatch):
    # A revoked/expired delegation must short-circuit the invoke BEFORE the
    # gateway is touched — proving the invoke path enforces validity, not just
    # check_fn. (The mesh/gateway slots are bare objects with no .invoke, so a
    # non-short-circuit would AttributeError.)
    import asyncio
    import json

    tools = plugin.tools
    node = plugin.node

    class _InvalidDelegation:
        def verify(self):
            return False

    monkeypatch.setattr(node, "_state", (object(), object(), "unused", _InvalidDelegation()))
    res = json.loads(asyncio.run(tools.handle_net_invoke({"cap_id": "42/x"})))
    assert res["status"] == "denied"
    assert "revoked or expired" in res["error"]
