"""Tests for the device-enrollment surface (`HERMES_INTEGRATION_PLAN_V2.md`
Phase 1): `InviteToken`, `JoinRequest`, `JoinOutcome`, `OperatorEnrollment`,
`DeviceRecord`, and `fingerprint`.

Thin PyO3 wrappers over `net_sdk::{enrollment,operator,devices}` — the Rust unit
tests own the crypto invariants; here we prove the Python surface mints,
signs, verifies, approves, revokes, and round-trips correctly, and that the H8
boundary holds (opaque `Identity` handles + public ids only).
"""

from __future__ import annotations

import pytest

pytest.importorskip("net")

import net  # noqa: E402


def _operator(tmp_path, root=None):
    root = root or net.Identity.generate()
    op = net.OperatorEnrollment(
        root,
        str(tmp_path / "devices.json"),
        str(tmp_path / "rev.json"),
    )
    return op, root


def test_invite_string_round_trips_and_carries_the_root(tmp_path):
    op, root = _operator(tmp_path)
    invite = op.invite("relay://rv", 300)
    s = invite.encode()
    assert s.startswith("net-invite:")

    parsed = net.InviteToken.decode(s)
    assert parsed.root == root.entity_id == op.root_id
    assert parsed.rendezvous == "relay://rv"
    # The displayed fingerprint matches the free function on the same id.
    assert parsed.root_fingerprint() == net.fingerprint(root.entity_id)


def test_join_request_signs_and_verifies(tmp_path):
    op, _root = _operator(tmp_path)
    invite = op.invite("relay://rv", 300)
    device = net.Identity.generate()
    req = net.JoinRequest.create(device, "pc", ["region:office"], invite)

    assert req.device == device.entity_id
    assert req.name == "pc"
    assert req.tags == ["region:office"]
    assert req.verify_self_signature() is True
    # Round-trips through bytes and still verifies.
    assert net.JoinRequest.from_bytes(req.to_bytes()).verify_self_signature() is True


def test_approve_mints_a_chain_and_records_the_device(tmp_path):
    op, root = _operator(tmp_path)
    invite = op.invite("relay://rv", 300)
    device = net.Identity.generate()
    req = net.JoinRequest.create(device, "pc", ["gpu:true"], invite)

    chain = op.approve(req, 3600)
    assert chain.leaf == device.entity_id
    assert chain.root == root.entity_id

    devices = op.devices()
    assert len(devices) == 1
    assert devices[0].name == "pc"
    assert devices[0].device == device.entity_id
    assert devices[0].is_revoked is False

    # Single-use: a replay of the same request is rejected.
    with pytest.raises(RuntimeError):
        op.approve(req, 3600)


def test_handle_join_request_round_trip_and_device_verifies(tmp_path):
    # The wire shape: request bytes -> handler -> outcome bytes -> device
    # verifies the grant anchors at the invited root + binds to itself.
    op, root = _operator(tmp_path)
    invite = op.invite("relay://rv", 300)
    device = net.Identity.generate()
    req = net.JoinRequest.create(device, "pc", [], invite)

    outcome_bytes = op.handle_join_request(req.to_bytes(), 3600)
    outcome = net.JoinOutcome.from_bytes(outcome_bytes)
    assert outcome.is_admitted is True
    assert outcome.reject_code is None

    chain = outcome.into_chain(device.entity_id, invite.root)
    assert chain.leaf == device.entity_id
    assert chain.root == root.entity_id

    # A grant for a different root/device is refused (rogue-operator defense).
    stranger = net.Identity.generate()
    with pytest.raises(RuntimeError):
        outcome2 = net.JoinOutcome.from_bytes(outcome_bytes)
        outcome2.into_chain(stranger.entity_id, invite.root)


def test_handle_join_request_rejects_are_coded(tmp_path):
    op, _root = _operator(tmp_path)
    # A request against an invite this operator never minted.
    stray_root = net.Identity.generate()
    stray_op = net.OperatorEnrollment(
        stray_root, str(tmp_path / "d2.json"), str(tmp_path / "r2.json")
    )
    stray_invite = stray_op.invite("relay://rv", 300)
    device = net.Identity.generate()
    # Point the request at *our* op's root won't help — the nonce is unknown.
    req = net.JoinRequest.create(device, "pc", [], stray_invite)

    outcome = net.JoinOutcome.from_bytes(op.handle_join_request(req.to_bytes(), 3600))
    assert outcome.is_admitted is False
    assert outcome.reject_code is not None
    assert outcome.reject_message


def test_revoke_marks_the_inventory(tmp_path):
    op, _root = _operator(tmp_path)
    invite = op.invite("relay://rv", 300)
    device = net.Identity.generate()
    req = net.JoinRequest.create(device, "pc", [], invite)
    op.approve(req, 3600)

    op.revoke(device.entity_id)
    rec = op.devices()[0]
    assert rec.is_revoked is True
    assert rec.revoked_at is not None


def test_forget_prunes_the_inventory(tmp_path):
    op, _root = _operator(tmp_path)
    invite = op.invite("relay://rv", 300)
    device = net.Identity.generate()
    op.approve(net.JoinRequest.create(device, "pc", [], invite), 3600)

    assert op.forget(device.entity_id) is True
    assert op.devices() == []
    assert op.forget(device.entity_id) is False


def test_pending_invites_lists_unredeemed(tmp_path):
    op, _root = _operator(tmp_path)
    op.invite("relay://a", 300)
    op.invite("relay://b", 300)
    # now=0 is before any expiry, so both are listed.
    assert len(op.pending_invites(0)) == 2


def test_fingerprint_is_stable_and_grouped(tmp_path):
    a = net.Identity.generate()
    b = net.Identity.generate()
    fa = net.fingerprint(a.entity_id)
    assert fa == net.fingerprint(a.entity_id)
    assert fa != net.fingerprint(b.entity_id)
    assert len(fa) == 19 and fa.count("-") == 3


def test_live_enrollment_over_the_mesh(tmp_path):
    # End-to-end over real UDP loopback: an operator node serves enrollment; a
    # fresh device node joins over the wire and gets its root -> device chain.
    pytest.importorskip("net._net")

    psk = "37" * 32  # 32-byte PSK as hex

    # nRPC reply channels are dynamic per-caller-origin, so enrollment needs
    # permissive_channels (the Rust default; the Python default is strict).
    root = net.Identity.generate()
    op_mesh = net.NetMesh("127.0.0.1:0", psk, permissive_channels=True)
    op_mesh.start()
    op = net.OperatorEnrollment(
        root, str(tmp_path / "d.json"), str(tmp_path / "r.json")
    )
    handle = op_mesh.serve_enrollment_auto(op, 3600)
    assert handle.serving is True

    invite = op.invite(op_mesh.rendezvous_string(), 300)

    device = net.Identity.generate()
    dev_mesh = net.NetMesh("127.0.0.1:0", psk, permissive_channels=True)
    dev_mesh.start()
    try:
        chain = dev_mesh.join(device, invite.encode(), "pc", ["region:office"])
        assert chain.leaf == device.entity_id
        assert chain.root == root.entity_id

        devs = op.devices()
        assert len(devs) == 1
        assert devs[0].name == "pc"
        assert devs[0].device == device.entity_id

        handle.stop()
        assert handle.serving is False
    finally:
        for m in (dev_mesh, op_mesh):
            try:
                m.shutdown()
            except Exception:  # noqa: BLE001 - best-effort teardown
                pass
