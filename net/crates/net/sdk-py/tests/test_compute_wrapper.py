"""Forwarding tests for ``net_sdk.compute`` (`PYTHON_SDK_WRAPPER_PARITY_PLAN.md`
S2). Runs against the stubbed extension (``conftest.py``), so it proves the
wrapper's plumbing only; behaviour is witnessed live in
``bindings/python/tests/test_sdk_compute.py``.
"""

from __future__ import annotations

from unittest.mock import MagicMock

import pytest

import net_sdk
import net_sdk.compute as compute
import net_sdk.mesh as mesh_mod


class FakeNativeRuntime:
    """Stands in for ``net.DaemonRuntime``. A real class, not a ``MagicMock``
    instance: ``_native_runtime`` does ``isinstance(x, _NativeDaemonRuntime)``,
    and ``isinstance`` against a mock *instance* raises ``TypeError`` by
    itself, which would let a type-check test pass for the wrong reason.
    Method calls go to a recording mock."""

    def __init__(self, mesh) -> None:
        self.mesh = mesh
        self.calls = MagicMock(name="DaemonRuntime-methods")

    def __getattr__(self, name):
        return getattr(self.calls, name)


@pytest.fixture
def rt(monkeypatch: pytest.MonkeyPatch):
    native_mesh = MagicMock(name="NetMesh-instance")
    monkeypatch.setattr(mesh_mod, "_NetMesh", MagicMock(return_value=native_mesh))
    node = mesh_mod.MeshNode("127.0.0.1:0", "00" * 32)
    monkeypatch.setattr(compute, "_NativeDaemonRuntime", FakeNativeRuntime)
    wrapper = compute.DaemonRuntime(node)
    return wrapper, wrapper.native, FakeNativeRuntime, native_mesh


def test_runtime_is_built_from_the_nodes_native_mesh(rt) -> None:
    wrapper, native_rt, _, native_mesh = rt
    assert isinstance(native_rt, FakeNativeRuntime)
    assert native_rt.mesh is native_mesh


def test_a_non_mesh_argument_is_a_type_error() -> None:
    with pytest.raises(TypeError, match="MeshNode"):
        compute.DaemonRuntime(object())


def test_methods_forward_unchanged(rt) -> None:
    wrapper, native_rt, _, _ = rt
    factory = object()
    wrapper.register_factory("k", factory)
    native_rt.register_factory.assert_called_once_with("k", factory)

    ident, cfg = object(), {"auto_snapshot_interval": 5}
    wrapper.spawn("k", ident, cfg)
    native_rt.spawn.assert_called_once_with("k", ident, cfg)
    wrapper.spawn_from_snapshot("k", ident, b"s")
    native_rt.spawn_from_snapshot.assert_called_once_with("k", ident, b"s", None)

    event = object()
    wrapper.deliver(7, event)
    native_rt.deliver.assert_called_once_with(7, event)
    wrapper.snapshot(7)
    native_rt.snapshot.assert_called_once_with(7)
    wrapper.stop(7)
    native_rt.stop.assert_called_once_with(7)

    wrapper.start_migration(7, 1, 2)
    native_rt.start_migration.assert_called_once_with(7, 1, 2)
    opts = {"transport_identity": False}
    wrapper.start_migration_with(7, 1, 2, opts)
    native_rt.start_migration_with.assert_called_once_with(7, 1, 2, opts)
    wrapper.expect_migration("k", 7)
    native_rt.expect_migration.assert_called_once_with("k", 7, None)
    wrapper.register_migration_target_identity("k", ident)
    native_rt.register_migration_target_identity.assert_called_once_with("k", ident, None)
    wrapper.migration_phase(7)
    native_rt.migration_phase.assert_called_once_with(7)


def test_native_runtime_unwraps_both_forms(rt) -> None:
    wrapper, native_rt, _, _ = rt
    assert compute._native_runtime(wrapper) is native_rt  # the SDK wrapper
    assert compute._native_runtime(native_rt) is native_rt  # a raw native runtime
    with pytest.raises(TypeError, match="expected a net_sdk.compute.DaemonRuntime"):
        compute._native_runtime(object())


def test_compute_names_are_exported() -> None:
    for name in compute.__all__:
        assert hasattr(compute, name), name
    for name in ("DaemonRuntime", "MeshDaemon", "MigrationPhase", "MigrationErrorKind"):
        assert name in net_sdk.__all__, name
