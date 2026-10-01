"""``net_sdk.AsyncMeshNode``, live (`PYTHON_SDK_WRAPPER_PARITY_PLAN.md` S7):
the S1 channel scenario and the S2 compute scenario under asyncio, plus
cancellation of the SDK's own ``events()`` loop.

Imports ``net_sdk`` from the in-repo source when the wrapper isn't
installed (CI's main run); it never skips for that.
"""

from __future__ import annotations

import asyncio
import importlib
import sys
from pathlib import Path

import pytest

pytest.importorskip("net._net")

from net import Identity  # noqa: E402

SDK_SRC = Path(__file__).resolve().parents[3] / "sdk-py" / "src"
PSK = "42" * 32


def _sdk(module: str):
    try:
        return importlib.import_module(module)
    except ImportError:
        sys.path.insert(0, str(SDK_SRC))
        return importlib.import_module(module)


async def _async_pair():
    net_sdk = _sdk("net_sdk")
    sub = net_sdk.AsyncMeshNode("127.0.0.1:0", PSK, heartbeat_interval_ms=200)
    pub = net_sdk.AsyncMeshNode("127.0.0.1:0", PSK, heartbeat_interval_ms=200)

    async def _connect():
        await asyncio.sleep(0.05)
        await sub.connect(pub.local_addr, pub.public_key, pub.node_id)

    await asyncio.wait_for(asyncio.gather(pub.accept(sub.node_id), _connect()), 10)
    sub.start()
    pub.start()
    return sub, pub


def test_channel_round_trip_under_asyncio() -> None:
    async def run() -> None:
        sub, pub = await _async_pair()
        try:
            pub.register_channel("sdk/async", visibility="global", reliable=True)
            await sub.subscribe_channel(pub.node_id, "sdk/async")
            report = await pub.publish("sdk/async", b'{"a": 1}', reliability="reliable")
            assert report["attempted"] == 1 and report["delivered"] == 1, report

            async def first_event():
                async for event in sub.events():
                    return event

            event = await asyncio.wait_for(first_event(), 5)
            raw = event.raw
            assert (raw.encode() if isinstance(raw, str) else bytes(raw)) == b'{"a": 1}'
            await sub.unsubscribe_channel(pub.node_id, "sdk/async")
        finally:
            await sub.shutdown()
            await pub.shutdown()

    asyncio.run(run())


def test_cancelling_an_events_loop_stops_it_cleanly() -> None:
    async def run() -> None:
        net_sdk = _sdk("net_sdk")
        node = net_sdk.AsyncMeshNode("127.0.0.1:0", PSK)
        node.start()
        seen: list = []

        async def consume():
            async for event in node.events(idle_sleep=0.005):
                seen.append(event)

        task = asyncio.create_task(consume())
        await asyncio.sleep(0.1)  # several idle drains
        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await task
        assert task.cancelled()
        assert seen == []
        # The node is still usable after its loop was cancelled.
        assert await node.recv(8) == []
        await node.shutdown()

    asyncio.run(run())


def test_compute_runs_on_an_async_node() -> None:
    async def run() -> None:
        net_sdk = _sdk("net_sdk")
        compute = _sdk("net_sdk.compute")
        node = net_sdk.AsyncMeshNode("127.0.0.1:0", PSK)

        class Echo:
            def process(self, event):
                return [event.payload]

        rt = compute.DaemonRuntime(node)  # accepted via _native_mesh
        rt.register_factory("echo", Echo)
        art = compute.AsyncDaemonRuntime(rt.native)
        await art.start()
        try:
            ident = Identity.generate()
            handle = await art.spawn("echo", ident)
            out = await art.deliver(
                handle.origin_hash, compute.CausalEvent(ident.origin_hash, 1, b"async")
            )
            assert out == [b"async"]
        finally:
            await art.shutdown()
            await node.shutdown()

    asyncio.run(run())


def test_from_node_shares_the_sync_node() -> None:
    net_sdk = _sdk("net_sdk")
    sync = net_sdk.MeshNode("127.0.0.1:0", PSK)
    node = net_sdk.AsyncMeshNode.from_node(sync)
    try:
        assert node.sync is sync
        assert node.node_id == sync.node_id
        assert node.entity_id == sync.entity_id
        assert node.num_shards() == sync.num_shards()
    finally:
        sync.shutdown()
