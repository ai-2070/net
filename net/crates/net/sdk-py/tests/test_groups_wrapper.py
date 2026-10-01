"""Forwarding tests for ``net_sdk.groups`` (`PYTHON_SDK_WRAPPER_PARITY_PLAN.md`
S3), against the stubbed extension (``conftest.py``). Behaviour is
witnessed live in ``bindings/python/tests/test_sdk_groups.py``.
"""

from __future__ import annotations

from unittest.mock import MagicMock

import pytest

import net_sdk
import net_sdk.compute as compute
import net_sdk.groups as groups
import net_sdk.mesh as mesh_mod


@pytest.fixture
def sdk_rt(monkeypatch: pytest.MonkeyPatch):
    monkeypatch.setattr(mesh_mod, "_NetMesh", MagicMock(return_value=MagicMock()))
    native_rt = MagicMock(name="DaemonRuntime-instance")
    monkeypatch.setattr(compute, "_NativeDaemonRuntime", MagicMock(return_value=native_rt))
    return compute.DaemonRuntime(mesh_mod.MeshNode("127.0.0.1:0", "00" * 32)), native_rt


def test_constructors_unwrap_the_sdk_runtime(monkeypatch, sdk_rt) -> None:
    rt, native_rt = sdk_rt
    for cls_name, ctor_name, args in (
        ("_NativeReplicaGroup", "spawn", ("k", 3, b"s" * 32, "round-robin")),
        ("_NativeForkGroup", "fork", ("k", 1, 2, 3, "random")),
        ("_NativeStandbyGroup", "spawn", ("k", 2, b"s" * 32)),
    ):
        native_cls = MagicMock()
        monkeypatch.setattr(groups, cls_name, native_cls)
        public = getattr(groups, cls_name.replace("_Native", ""))
        group = getattr(public, ctor_name)(rt, *args)
        getattr(native_cls, ctor_name).assert_called_once_with(native_rt, *args, None)
        assert group.native is getattr(native_cls, ctor_name).return_value


def test_methods_and_properties_forward(monkeypatch, sdk_rt) -> None:
    native = MagicMock()
    group = groups.ReplicaGroup(native)
    group.route_event({"routing_key": "k"})
    native.route_event.assert_called_once_with({"routing_key": "k"})
    group.scale_to(5)
    native.scale_to.assert_called_once_with(5)
    native.replica_count = 5
    assert group.replica_count == 5

    standby = groups.StandbyGroup(native)
    standby.member_role(1)
    native.member_role.assert_called_once_with(1)
    standby.promote()
    native.promote.assert_called_once_with()


def test_a_non_runtime_is_a_type_error() -> None:
    with pytest.raises(TypeError):
        groups.ReplicaGroup.spawn(object(), "k", 1, b"s" * 32, "random")


def test_groups_names_are_exported() -> None:
    for name in groups.__all__:
        assert hasattr(groups, name), name
    for name in ("ReplicaGroup", "ForkGroup", "StandbyGroup", "GroupErrorKind"):
        assert name in net_sdk.__all__, name
