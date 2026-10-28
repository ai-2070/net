"""Streamed calls must not lose chunks silently
(APP_INTEGRATION_BLOCKERS_PLAN.md Phase 3).

``sink.send`` never waits and drops when the per-call queue is full — now
reporting the drop as ``False``. ``sink.send_wait`` waits for room instead,
so a handler that produces as fast as it can still delivers every chunk,
in order, to a flow-controlled caller.
"""

from __future__ import annotations

import time

import pytest

net = pytest.importorskip("net", reason="net wheel not built")

N = 3000


def _drain(stream) -> list[int]:
    return [int.from_bytes(chunk[:4], "big") for chunk in stream]


def test_send_wait_delivers_every_chunk_in_order(mesh_pair) -> None:
    a, b = mesh_pair
    server = net.MeshRpc(b)
    client = net.MeshRpc(a)

    def producer(_req: bytes, sink) -> None:
        for i in range(N):
            chunk = i.to_bytes(4, "big") + bytes(1020)
            assert sink.send_wait(chunk), f"call ended early at chunk {i}"

    handle = server.serve_streaming("bp.send_wait", producer, 60_000)
    try:
        time.sleep(0.3)
        stream = client.call_streaming(
            b.node_id, "bp.send_wait", b"go", {"stream_window_initial": 64}
        )
        assert _drain(stream) == list(range(N))
    finally:
        handle.close()


def test_send_reports_a_dropped_chunk_as_false(mesh_pair) -> None:
    a, b = mesh_pair
    server = net.MeshRpc(b)
    client = net.MeshRpc(a)
    refused = []

    def burst(_req: bytes, sink) -> None:
        # Well past the 1024-chunk queue with the caller granting 8.
        refused.append(sum(not sink.send(bytes(16)) for _ in range(4096)))

    handle = server.serve_streaming("bp.send", burst, 60_000)
    try:
        time.sleep(0.3)
        stream = client.call_streaming(
            b.node_id, "bp.send", b"go", {"stream_window_initial": 8}
        )
        got = sum(1 for _ in stream)
        assert refused and refused[0] > 0, "a burst past the queue must report drops"
        assert got <= 4096 - refused[0]
    finally:
        handle.close()


def test_asend_wait_delivers_every_chunk_from_an_async_handler(mesh_pair) -> None:
    import asyncio

    from net import AsyncMeshRpc

    a, b = mesh_pair
    n = 1500

    async def producer(stream, sink) -> None:
        for _ in stream:  # the caller's single request chunk
            pass
        for i in range(n):
            ok = await sink.asend_wait(i.to_bytes(4, "big") + bytes(1020))
            assert ok, f"call ended early at chunk {i}"

    async def run() -> list[int]:
        server = AsyncMeshRpc(b)
        client = AsyncMeshRpc(a)
        handle = server.serve_duplex("bp.asend_wait", producer)
        try:
            await asyncio.sleep(0.5)
            call = await client.call_duplex(b.node_id, "bp.asend_wait")
            await call.send(b"go")
            await call.finish_sending()
            return [int.from_bytes(chunk[:4], "big") async for chunk in call]
        finally:
            handle.close()

    assert asyncio.run(run()) == list(range(n))
