"""Tests for silent device-grant renewal (`HERMES_INTEGRATION_PLAN_V2.md` B5c):
the :class:`renewal.RenewalService` background loop + the node device-mode
wiring.

The renewal handshake / crypto / persistence are the Rust SDK's (proven in the
binding tests); here we prove the plugin *scheduler* renews when the grant is
within its window, persists the refreshed enrollment, and starts/stops cleanly.
"""

from __future__ import annotations

import time

import pytest

pytest.importorskip("net")
pytest.importorskip("net._net")

import net  # noqa: E402


def _operator(tmp_path, psk):
    root = net.Identity.generate()
    op_mesh = net.NetMesh("127.0.0.1:0", psk, permissive_channels=True)
    op_mesh.start()
    op = net.OperatorEnrollment(
        root, str(tmp_path / "devices.json"), str(tmp_path / "revocations.json")
    )
    handle = op_mesh.serve_enrollment_auto(op, 3600)
    return root, op_mesh, op, handle


def test_renewal_service_renews_when_within_window(plugin, tmp_path):
    psk = "5a" * 32
    root, op_mesh, op, handle = _operator(tmp_path, psk)
    rendezvous = op_mesh.rendezvous_string()
    invite = op.invite(rendezvous, 300)

    device = net.Identity.generate()
    dev_mesh = net.NetMesh("127.0.0.1:0", psk, permissive_channels=True)
    dev_mesh.start()
    try:
        chain = dev_mesh.join(device, invite.encode(), "pc", [])
        path = str(tmp_path / "device-enrollment.json")
        enrollment = net.DeviceEnrollment(device, chain, rendezvous, int(time.time()))
        enrollment.save(path)

        # A huge window → the grant is always "within" it → renew now.
        svc = plugin.renewal.RenewalService(
            dev_mesh, enrollment, path, renewal_window=10**9, check_interval=10**9
        )
        assert svc.maybe_renew() is True

        # The refreshed enrollment is persisted, still valid, and reflected in
        # the service's in-memory copy.
        reloaded = net.DeviceEnrollment.load(path)
        assert reloaded is not None
        assert reloaded.device.entity_id == device.entity_id
        assert reloaded.root == root.entity_id
        assert net.DeviceEnrollment.load(path).expires_at == svc.enrollment.expires_at
        assert reloaded.is_valid(net.RevocationRegistry()) is True

        # With a tiny window the grant isn't near expiry → no renewal.
        fresh = plugin.renewal.RenewalService(dev_mesh, svc.enrollment, path, renewal_window=1)
        assert fresh.maybe_renew() is False
    finally:
        handle.stop()
        for m in (dev_mesh, op_mesh):
            try:
                m.shutdown()
            except Exception:  # noqa: BLE001 - best-effort teardown
                pass


def test_renewal_service_start_stop_is_idempotent(plugin, tmp_path):
    # No live mesh: a tiny window means the loop never renews, so a mesh whose
    # `renew` would fail the test is never called.
    root = net.Identity.generate()
    op = net.OperatorEnrollment(
        root, str(tmp_path / "d.json"), str(tmp_path / "r.json")
    )
    invite = op.invite("relay://rv", 300)
    device = net.Identity.generate()
    chain = op.approve(net.JoinRequest.create(device, "pc", [], invite), 3600)
    enrollment = net.DeviceEnrollment(device, chain, "relay://rv", int(time.time()))

    class _NoRenewMesh:
        def renew(self, _enrollment):  # pragma: no cover - must never be called
            raise AssertionError("no renewal expected with a tiny window")

    svc = plugin.renewal.RenewalService(
        _NoRenewMesh(), enrollment, str(tmp_path / "e.json"), renewal_window=1, check_interval=100
    )
    svc.start()
    svc.start()  # idempotent — no second thread
    svc.stop()
    svc.stop()  # idempotent
    assert svc.maybe_renew() is False


def test_start_device_renewal_is_idle_without_config(plugin, monkeypatch):
    # No NET_MESH_DEVICE_ENROLLMENT → device-mode is a no-op (mesh unused).
    monkeypatch.delenv("NET_MESH_DEVICE_ENROLLMENT", raising=False)
    node = plugin.node
    node._renewal = None
    node._start_device_renewal(None)
    assert node._renewal is None
