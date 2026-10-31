"""Dropping a serve handle no longer stops its service
(APP_INTEGRATION_BLOCKERS_PLAN.md Phase 4).

In Python the handle is freed as soon as nothing references it — so
``mesh.serve_enrollment_auto(op, ttl)`` without assigning the result used
to unregister enrollment on the very next line. The node now owns the
registration until ``stop()`` / ``close()`` or ``shutdown()``.
"""

from __future__ import annotations

import gc
import time
import weakref

import pytest

pytest.importorskip("net._net")

import net  # noqa: E402


def test_an_unreferenced_enrollment_handle_keeps_serving(tmp_path):
    psk = "6a" * 32
    root = net.Identity.generate()
    op_mesh = net.NetMesh("127.0.0.1:0", psk, permissive_channels=True)
    op_mesh.start()
    op = net.OperatorEnrollment(
        root, str(tmp_path / "devices.json"), str(tmp_path / "revocations.json")
    )

    # Serve without keeping the handle, and prove it really is gone.
    handle = op_mesh.serve_enrollment_auto(op, 3600)
    gone = weakref.ref(handle) if hasattr(handle, "__weakref__") else None
    del handle
    gc.collect()
    if gone is not None:
        assert gone() is None, "the handle should have been freed"

    rendezvous = op_mesh.rendezvous_string()
    invite = op.invite(rendezvous, 300)
    device = net.Identity.generate()
    dev_mesh = net.NetMesh("127.0.0.1:0", psk, permissive_channels=True)
    dev_mesh.start()
    try:
        chain = dev_mesh.join(device, invite.encode(), "pc", [])
        enrollment = net.DeviceEnrollment(device, chain, rendezvous, int(time.time()))
        renewed = dev_mesh.renew(enrollment)
        assert renewed.leaf == device.entity_id
    finally:
        # Shutdown releases the services the node still owns.
        dev_mesh.shutdown()
        op_mesh.shutdown()


def test_stop_still_stops_enrollment(tmp_path):
    psk = "6b" * 32
    root = net.Identity.generate()
    op_mesh = net.NetMesh("127.0.0.1:0", psk, permissive_channels=True)
    op_mesh.start()
    op = net.OperatorEnrollment(
        root, str(tmp_path / "devices.json"), str(tmp_path / "revocations.json")
    )
    handle = op_mesh.serve_enrollment_auto(op, 3600)
    assert handle.serving
    handle.stop()
    assert not handle.serving
    handle.stop()  # idempotent

    invite = op.invite(op_mesh.rendezvous_string(), 300)
    dev_mesh = net.NetMesh("127.0.0.1:0", psk, permissive_channels=True)
    dev_mesh.start()
    try:
        with pytest.raises(Exception):
            dev_mesh.join(net.Identity.generate(), invite.encode(), "pc", [])
    finally:
        dev_mesh.shutdown()
        op_mesh.shutdown()
