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


def _device_env(monkeypatch, path, invite):
    monkeypatch.setenv("NET_MESH_DEVICE_ENROLLMENT", str(path))
    monkeypatch.setenv("NET_MESH_INVITE", invite)
    # Keep the background loop quiet: nowhere near the (tiny) window, huge tick.
    monkeypatch.setenv("NET_MESH_RENEWAL_WINDOW", "1")
    monkeypatch.setenv("NET_MESH_RENEWAL_INTERVAL", str(10**9))


def test_first_run_enrolls_joins_and_persists(plugin, tmp_path, monkeypatch):
    psk = "5c" * 32
    _root, op_mesh, op, handle = _operator(tmp_path, psk)
    invite = op.invite(op_mesh.rendezvous_string(), 300)
    path = tmp_path / "device-enrollment.json"
    _device_env(monkeypatch, path, invite.encode())

    node = plugin.node
    node._renewal = None
    dev_mesh = net.NetMesh("127.0.0.1:0", psk, permissive_channels=True)
    dev_mesh.start()
    try:
        node._start_device_renewal(dev_mesh)
        assert node._renewal is not None
        # Persisted: a restart would load this instead of replaying the invite.
        assert net.DeviceEnrollment.load(str(path)) is not None
    finally:
        if node._renewal is not None:
            node._renewal.stop()
        node._renewal = None
        handle.stop()
        for m in (dev_mesh, op_mesh):
            try:
                m.shutdown()
            except Exception:  # noqa: BLE001 - best-effort teardown
                pass


def test_unwritable_enrollment_path_aborts_before_the_join(plugin, tmp_path, monkeypatch):
    # The single-use invite must NOT be burned when the enrollment could never
    # be persisted: the write probe fails first and join is never reached.
    import os

    ro_dir = tmp_path / "ro"
    ro_dir.mkdir()
    path = ro_dir / "enrollment.json"

    root = net.Identity.generate()
    op = net.OperatorEnrollment(
        root, str(tmp_path / "d.json"), str(tmp_path / "r.json")
    )
    invite = op.invite("relay://rv", 300)
    _device_env(monkeypatch, path, invite.encode())

    joins = []

    class _RecordingMesh:
        def join(self, *args):  # pragma: no cover - must never be called
            joins.append(args)
            raise AssertionError("join must not run when the path is unwritable")

    node = plugin.node
    node._renewal = None
    os.chmod(ro_dir, 0o500)  # readable/traversable, not writable
    try:
        with pytest.raises(OSError):
            node._start_device_renewal(_RecordingMesh())
    finally:
        os.chmod(ro_dir, 0o700)
    assert joins == [], "the invite was not spent"
    assert node._renewal is None


def test_save_failure_after_join_keeps_the_in_memory_enrollment(
    plugin, tmp_path, monkeypatch, caplog
):
    # The probe passes but the post-join save keeps failing: the failure is
    # surfaced loudly and the session continues on the in-memory enrollment —
    # the renewal loop re-attempts persistence later.
    psk = "5d" * 32
    _root, op_mesh, op, handle = _operator(tmp_path, psk)
    invite = op.invite(op_mesh.rendezvous_string(), 300)
    path = tmp_path / "device-enrollment.json"
    _device_env(monkeypatch, path, invite.encode())

    real = net.DeviceEnrollment

    class _UnsavableProxy:
        def __init__(self, inner):
            self._inner = inner

        def save(self, _path):
            raise RuntimeError("disk on fire")

        def __getattr__(self, name):
            return getattr(self._inner, name)

    class _FlakyDeviceEnrollment:
        load = staticmethod(real.load)

        def __new__(cls, *args):
            return _UnsavableProxy(real(*args))

    monkeypatch.setattr(net, "DeviceEnrollment", _FlakyDeviceEnrollment)

    node = plugin.node
    node._renewal = None
    dev_mesh = net.NetMesh("127.0.0.1:0", psk, permissive_channels=True)
    dev_mesh.start()
    try:
        with caplog.at_level("ERROR", logger=node.logger.name):
            node._start_device_renewal(dev_mesh)
        assert node._renewal is not None, "the admitted key keeps the session alive"
        assert any("only in memory" in r.message for r in caplog.records)
    finally:
        if node._renewal is not None:
            node._renewal.stop()
        node._renewal = None
        handle.stop()
        for m in (dev_mesh, op_mesh):
            try:
                m.shutdown()
            except Exception:  # noqa: BLE001 - best-effort teardown
                pass
