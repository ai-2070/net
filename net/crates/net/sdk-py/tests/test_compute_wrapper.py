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


@pytest.fixture
def rt(monkeypatch: pytest.MonkeyPatch):
    native_mesh = MagicMock(name="NetMesh-instance")
    monkeypatch.setattr(mesh_mod, "_NetMesh", MagicMock(return_value=native_mesh))
    node = mesh_mod.MeshNode("127.0.0.1:0", "00" * 32)
    native_rt = MagicMock(name="DaemonRuntime-instance")
    ctor = MagicMock(return_value=native_rt)
    monkeypatch.setattr(compute, "_NativeDaemonRuntime", ctor)
    return compute.DaemonRuntime(node), native_rt, ctor, native_mesh


def test_runtime_is_built_from_the_nodes_native_mesh(rt) -> None:
    wrapper, native_rt, ctor, native_mesh = rt
    ctor.assert_called_once_with(native_mesh)
    assert wrapper.native is native_rt


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
    wrapper, native_rt, ctor, _ = rt
    assert compute._native_runtime(wrapper) is native_rt
    with pytest.raises(TypeError):
        compute._native_runtime(object())


def test_compute_names_are_exported() -> None:
    for name in compute.__all__:
        assert hasattr(compute, name), name
    for name in ("DaemonRuntime", "MeshDaemon", "MigrationPhase", "MigrationErrorKind"):
        assert name in net_sdk.__all__, name
