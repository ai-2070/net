"""Forwarding tests for ``net_sdk.AsyncMeshNode`` (`PYTHON_SDK_WRAPPER_PARITY_PLAN.md`
S7), against the stubbed extension (``conftest.py``). Behaviour is witnessed
live in ``bindings/python/tests/test_sdk_async_mesh.py``.
"""

from __future__ import annotations

import asyncio
from unittest.mock import AsyncMock, MagicMock

import pytest

import net
import net_sdk
import net_sdk.mesh as mesh_mod

AWAITED = (
    "connect",
    "accept",
    "shutdown",
    "subscribe_channel",
    "unsubscribe_channel",
    "publish",
    "poll",
    "announce_capabilities",
    "push_to",
)


@pytest.fixture
def anode(monkeypatch: pytest.MonkeyPatch):
    sync_native = MagicMock(name="NetMesh-instance")
    monkeypatch.setattr(mesh_mod, "_NetMesh", MagicMock(return_value=sync_native))
    async_native = MagicMock(name="AsyncNetMesh-instance")
    for name in AWAITED:
        setattr(async_native, name, AsyncMock(name=name))
    async_ctor = MagicMock(return_value=async_native)
    monkeypatch.setattr(net, "AsyncNetMesh", async_ctor, raising=False)
    node = mesh_mod.AsyncMeshNode("127.0.0.1:0", "00" * 32, num_shards=2)
    return node, sync_native, async_native, async_ctor


def test_it_wraps_the_sync_nodes_native_mesh(anode) -> None:
    node, sync_native, _, async_ctor = anode
    async_ctor.assert_called_once_with(sync_native)
    assert isinstance(node.sync, mesh_mod.MeshNode)
    assert mesh_mod._native_mesh(node) is sync_native


def test_async_verbs_are_awaited_with_their_arguments(anode) -> None:
    node, _, async_native, _ = anode

    async def run() -> None:
        await node.connect("1.2.3.4:5", "ab", 7)
        await node.accept(7)
        await node.subscribe_channel(7, "c", b"tok")
        await node.unsubscribe_channel(7, "c")
        await node.publish("c", b"p", reliability="reliable", max_inflight=2)
        await node.recv(9)
        await node.announce_capabilities({"tags": []})
        await node.push_to("1.2.3.4:5", "{}")
        await node.shutdown()

    asyncio.run(run())
    async_native.connect.assert_awaited_once_with("1.2.3.4:5", "ab", 7)
    async_native.accept.assert_awaited_once_with(7)
    async_native.subscribe_channel.assert_awaited_once_with(7, "c", b"tok")
    async_native.unsubscribe_channel.assert_awaited_once_with(7, "c")
    async_native.publish.assert_awaited_once_with(
        "c", b"p", reliability="reliable", on_failure=None, max_inflight=2
    )
    async_native.poll.assert_awaited_once_with(9)
    async_native.announce_capabilities.assert_awaited_once_with({"tags": []})
    async_native.push_to.assert_awaited_once_with("1.2.3.4:5", "{}")
    async_native.shutdown.assert_awaited_once_with()


def test_sync_only_verbs_go_to_the_sync_node(anode) -> None:
    node, sync_native, _, _ = anode
    node.register_channel("c", visibility="global")
    assert sync_native.register_channel.call_args.args == ("c",)
    assert sync_native.register_channel.call_args.kwargs["visibility"] == "global"
    node.open_stream_inbox(5)
    sync_native.open_stream_inbox.assert_called_once_with(5, 4096)
    node.num_shards()
    sync_native.num_shards.assert_called_once_with()


def test_events_yields_batches_and_sleeps_when_idle(anode, monkeypatch) -> None:
    node, _, async_native, _ = anode
    async_native.poll.side_effect = [["e1", "e2"], [], ["e3"]]
    # Without the idle sleep, `events()` would spin on an empty queue. Pin
    # that it backs off exactly once, after the one empty batch, for the
    # requested interval.
    sleep = AsyncMock()
    monkeypatch.setattr(mesh_mod.asyncio, "sleep", sleep)

    async def run() -> list:
        got = []
        async for event in node.events(limit=4, idle_sleep=0.25):
            got.append(event)
            if len(got) == 3:
                break
        return got

    assert asyncio.run(run()) == ["e1", "e2", "e3"]
    assert async_native.poll.await_count == 3
    sleep.assert_awaited_once_with(0.25)


def test_exported_from_the_root() -> None:
    assert "AsyncMeshNode" in net_sdk.__all__
    assert net_sdk.AsyncMeshNode is mesh_mod.AsyncMeshNode
