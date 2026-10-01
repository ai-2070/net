"""Forwarding tests for the mesh-channel methods on ``net_sdk.MeshNode``
(`PYTHON_SDK_WRAPPER_PARITY_PLAN.md` S1).

This suite runs against a stub of the native extension (see
``conftest.py``), so it proves only that every argument reaches the
native call unchanged. Behaviour is witnessed live in
``bindings/python/tests/test_sdk_mesh_channels.py``.
"""

from __future__ import annotations

from unittest.mock import MagicMock

import pytest

import net_sdk
import net_sdk.mesh as mesh_mod


@pytest.fixture
def node(monkeypatch: pytest.MonkeyPatch):
    native = MagicMock(name="NetMesh-instance")
    monkeypatch.setattr(mesh_mod, "_NetMesh", MagicMock(return_value=native))
    return mesh_mod.MeshNode("127.0.0.1:0", "00" * 32), native


def test_register_channel_forwards_every_option(node) -> None:
    mesh, native = node
    caps = {"require_tags": ["reader"]}
    mesh.register_channel(
        "a/b",
        visibility="subnet-local",
        reliable=True,
        require_token=True,
        token_roots=[b"\x01" * 32],
        priority=3,
        max_rate_pps=100,
        publish_caps={"require_tags": ["admin"]},
        subscribe_caps=caps,
    )
    native.register_channel.assert_called_once_with(
        "a/b",
        visibility="subnet-local",
        reliable=True,
        require_token=True,
        token_roots=[b"\x01" * 32],
        priority=3,
        max_rate_pps=100,
        publish_caps={"require_tags": ["admin"]},
        subscribe_caps=caps,
    )


def test_register_channel_accepts_a_splatted_channel_config(node) -> None:
    mesh, native = node
    cfg: net_sdk.ChannelConfig = {"visibility": "global", "priority": 1}
    mesh.register_channel("a/b", **cfg)
    kwargs = native.register_channel.call_args.kwargs
    assert kwargs["visibility"] == "global"
    assert kwargs["priority"] == 1
    assert kwargs["reliable"] is None  # unset options reach native as None


def test_subscribe_and_unsubscribe_forward(node) -> None:
    mesh, native = node
    mesh.subscribe_channel(7, "a/b", b"token")
    native.subscribe_channel.assert_called_once_with(7, "a/b", b"token")
    mesh.subscribe_channel(7, "a/c")
    native.subscribe_channel.assert_called_with(7, "a/c", None)
    mesh.unsubscribe_channel(7, "a/b")
    native.unsubscribe_channel.assert_called_once_with(7, "a/b")


def test_publish_forwards_and_returns_the_native_report(node) -> None:
    mesh, native = node
    native.publish.return_value = {"attempted": 2, "delivered": 2, "errors": []}
    report = mesh.publish(
        "a/b", b"payload", reliability="reliable", on_failure="collect", max_inflight=8
    )
    native.publish.assert_called_once_with(
        "a/b",
        b"payload",
        reliability="reliable",
        on_failure="collect",
        max_inflight=8,
    )
    assert report == {"attempted": 2, "delivered": 2, "errors": []}


def test_receive_side_forwards(node) -> None:
    mesh, native = node
    native.poll.return_value = ["e1"]
    assert mesh.recv(10) == ["e1"]
    native.poll.assert_called_once_with(10)

    native.num_shards.return_value = 4
    assert mesh.num_shards() == 4
    native.num_shards.assert_called_once_with()
    native.shard_for_stream.return_value = 3
    assert mesh.shard_for_stream(7) == 3
    native.shard_for_stream.assert_called_once_with(7)

    mesh.poll_shard(2, 5)
    native.poll_shard.assert_called_once_with(2, 5)

    mesh.open_stream_inbox(9)
    native.open_stream_inbox.assert_called_with(9, 4096)  # native default
    mesh.open_stream_inbox(9, capacity=16)
    native.open_stream_inbox.assert_called_with(9, 16)


def test_channel_names_are_exported_from_the_package() -> None:
    for name in (
        "ChannelError",
        "ChannelAuthError",
        "ChannelConfig",
        "PublishConfig",
        "PublishError",
        "PublishReport",
        "Visibility",
        "OnFailure",
    ):
        assert name in net_sdk.__all__, name
        assert hasattr(net_sdk, name), name
