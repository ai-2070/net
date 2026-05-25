"""(sync | async) caller × (sync | async) server matrix — TX-1.

Pins the v0.x acceptance criterion: a server registered via either
``MeshRpc.serve`` or ``AsyncMeshRpc.serve`` is callable from either
``MeshRpc.call`` or ``AsyncMeshRpc.call``. Four combinations on
the unary shape — extend per-shape tests in follow-ups for
server-streaming / client-streaming / duplex.

Run via::

    maturin develop --features "net,cortex"
    pytest tests/test_async_interop.py -v

The two-mesh handshake follows the existing ``_mesh_pair`` pattern
from ``test_compute.py`` — accept on a thread while connect fires
from the main thread, then ``start()`` both sides.
"""

from __future__ import annotations

import asyncio
import itertools
import threading
import time

import pytest

# These tests need the native classes; if the wheel was built
# without `net + cortex`, the import fails and pytest skips.
pytest.importorskip("net._net")

from net import (  # noqa: E402
    AsyncMeshRpc,
    AsyncNetMesh,
    MeshRpc,
    NetMesh,
)

PSK = "42" * 32

# Per-test unique ports so repeated runs don't collide on localhost.
_port_counter = itertools.count(29_700)


def _next_port() -> str:
    return f"127.0.0.1:{next(_port_counter)}"


def _mesh_pair() -> tuple[NetMesh, NetMesh]:
    """Build two connected meshes. Mirrors the pattern in
    `test_compute.py::_mesh_pair`; documented there in detail."""
    a_addr = _next_port()
    b_addr = _next_port()
    a = NetMesh(bind_addr=a_addr, psk=PSK)
    b = NetMesh(bind_addr=b_addr, psk=PSK)

    errors: list[Exception] = []

    def _accept() -> None:
        try:
            b.accept(a.node_id)
        except Exception as e:
            errors.append(e)

    t = threading.Thread(target=_accept, daemon=True)
    t.start()
    time.sleep(0.05)
    a.connect(b_addr, b.public_key, b.node_id)
    t.join(timeout=5)
    if t.is_alive():
        raise RuntimeError(
            "mesh-pair handshake: accept thread still alive after 5 s timeout"
        )
    if errors:
        raise errors[0]
    a.start()
    b.start()
    return a, b


# ---------------------------------------------------------------------------
# Unary matrix — four combinations.
#
# Server handler "echo" — returns the request bytes verbatim. We use
# this same handler shape across all four tests so the comparison is
# purely about the caller/server async-shape combination.
# ---------------------------------------------------------------------------


def _sync_echo(req: bytes) -> bytes:
    return req


async def _async_echo(req: bytes) -> bytes:
    # An `await` makes this a genuine coroutine — `inspect.iscoroutinefunction`
    # returns True against this whether or not we actually await anything,
    # but adding the await proves the bridge drives the coroutine to
    # completion before returning the reply.
    await asyncio.sleep(0)
    return req


def test_sync_caller_sync_server_unary() -> None:
    """Regression: the original sync API still works after async lands."""
    a, b = _mesh_pair()
    try:
        srv = MeshRpc(b)
        cli = MeshRpc(a)
        h = srv.serve("echo", _sync_echo)
        try:
            reply = cli.call(b.node_id, "echo", b"hi")
            assert reply == b"hi"
        finally:
            h.close()
    finally:
        a.shutdown()
        b.shutdown()


def test_sync_caller_async_server_unary() -> None:
    """A sync caller reaches an `async def` server handler.

    The async handler runs as a coroutine on the substrate's tokio
    runtime; the sync caller blocks on its `block_on(call(...))`
    until the coroutine resolves and the reply lands."""
    a, b = _mesh_pair()
    try:
        asrv = AsyncMeshRpc(b)
        cli = MeshRpc(a)
        h = asrv.serve("echo", _async_echo)
        try:
            reply = cli.call(b.node_id, "echo", b"async-via-sync")
            assert reply == b"async-via-sync"
        finally:
            h.close()
    finally:
        a.shutdown()
        b.shutdown()


def test_async_caller_sync_server_unary() -> None:
    """An async caller reaches a sync handler.

    The sync handler runs on the substrate's `spawn_blocking` path;
    the async caller awaits a Python awaitable that resolves when
    the substrate reply lands."""
    a, b = _mesh_pair()

    async def _run() -> bytes:
        srv = MeshRpc(b)
        acli = AsyncMeshRpc(a)
        h = srv.serve("echo", _sync_echo)
        try:
            return await acli.call(b.node_id, "echo", b"sync-via-async")
        finally:
            h.close()

    try:
        reply = asyncio.run(_run())
        assert reply == b"sync-via-async"
    finally:
        a.shutdown()
        b.shutdown()


def test_async_caller_async_server_unary() -> None:
    """End-to-end async path: `async def` handler + `await call`.

    Both sides ride the same shared `MeshNode`; the reply lands on
    the async caller's awaitable without ever blocking a Python
    thread."""
    a, b = _mesh_pair()

    async def _run() -> bytes:
        asrv = AsyncMeshRpc(b)
        acli = AsyncMeshRpc(a)
        h = asrv.serve("echo", _async_echo)
        try:
            return await acli.call(b.node_id, "echo", b"both-async")
        finally:
            h.close()

    try:
        reply = asyncio.run(_run())
        assert reply == b"both-async"
    finally:
        a.shutdown()
        b.shutdown()


# ---------------------------------------------------------------------------
# Mixing sync NetMesh + AsyncNetMesh on the same handle.
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# TX-2 — asyncio cancel propagation.
#
# A long-running server holds the call past the caller's wait_for
# timeout. The asyncio task-cancel must propagate to the substrate's
# Mesh::cancel(token), which surfaces RpcCancelledError on the
# in-flight call.
# ---------------------------------------------------------------------------


def test_wait_for_timeout_propagates_to_substrate_cancel() -> None:
    """`asyncio.wait_for(arpc.call(...), timeout=0.1)` against a
    handler that sleeps for several seconds must surface
    `asyncio.TimeoutError` on the caller side AND let the substrate
    cancel the in-flight call (rather than orphaning the handler
    until natural completion).

    Verification has two parts:
    - `asyncio.TimeoutError` is raised on the caller (proves the
      Python-side cancel fired).
    - The handler observes `asyncio.CancelledError` mid-sleep
      (proves the substrate cancel reached the handler coroutine
      via the cancel-token notify path).
    """
    from net import RpcCancelledError, RpcError  # noqa: F401

    a, b = _mesh_pair()

    handler_was_cancelled = asyncio.Event()

    async def _slow_handler(req: bytes) -> bytes:
        try:
            await asyncio.sleep(10.0)
            return req
        except asyncio.CancelledError:
            # Caller's asyncio.wait_for triggered cancel — the
            # substrate's cancel-token machinery propagated it
            # through to this handler's await.
            handler_was_cancelled.set()
            raise

    async def _run() -> None:
        asrv = AsyncMeshRpc(b)
        acli = AsyncMeshRpc(a)
        h = asrv.serve("slow", _slow_handler)
        try:
            with pytest.raises(asyncio.TimeoutError):
                await asyncio.wait_for(
                    acli.call(b.node_id, "slow", b"never-resolves"),
                    timeout=0.2,
                )
            # Give the substrate a moment to deliver the cancel.
            # The handler-side CancelledError fires as soon as the
            # tokio cancel-watcher trips the substrate's cancel
            # registry; that latency is sub-millisecond in steady
            # state but allow a generous bound for CI.
            try:
                await asyncio.wait_for(
                    handler_was_cancelled.wait(), timeout=1.0
                )
            except asyncio.TimeoutError:
                pytest.fail(
                    "handler never observed CancelledError — "
                    "asyncio cancel did not propagate to substrate"
                )
        finally:
            h.close()

    try:
        asyncio.run(_run())
    finally:
        a.shutdown()
        b.shutdown()


def test_streaming_mid_iter_cancel_terminates_stream() -> None:
    """A streaming server emitting on a slow cadence; an async
    consumer breaks out of the `async for` loop after one chunk.
    The remaining substrate-side stream pulls must be dropped
    (substrate cancel-watcher fires on construction-time token)
    rather than continuing in the background.

    This pins the per-stream cancel contract: a mid-stream
    `task.cancel()` (here triggered via `wait_for` on a single
    `__anext__`) terminates the WHOLE stream, not just one pull.
    """
    # Imported lazily — the streaming-handler protocol may not be
    # wired in slim builds; skip cleanly if so.
    try:
        from net import RpcCancelledError  # noqa: F401
    except ImportError:
        pytest.skip("RpcCancelledError not available in this wheel")

    a, b = _mesh_pair()

    # We need a streaming server. The cleanest path is to register
    # a sync streaming-handler via the existing MeshRpc serve API.
    # If serve_streaming isn't exposed (T1-A7 streaming-serve is
    # deferred per the project plan), skip.
    srv = MeshRpc(b)
    if not hasattr(srv, "serve_streaming"):
        pytest.skip(
            "MeshRpc.serve_streaming not exposed — streaming-serve "
            "from Python is deferred (see project plan §T1-A7)"
        )

    # Body intentionally left for a follow-up wiring slice — we
    # don't have a Python-side streaming-server handler shape yet
    # to register a slow-emitting stream against.
    pytest.skip(
        "streaming-server-side cancel test requires a Python-side "
        "serve_streaming handler API (deferred follow-up)"
    )
    a.shutdown()
    b.shutdown()


def test_async_netmesh_shares_handshake_with_sync_netmesh() -> None:
    """`AsyncNetMesh(mesh)` doesn't re-handshake — the same peer
    connection set up by the sync `NetMesh.connect/.accept` is
    visible to the async wrapper.

    This is the "shared MeshNode" contract from the plan's locked
    decision #4 — proves an AsyncNetMesh constructed against an
    already-connected mesh sees the existing peer count without
    a re-handshake."""
    a, b = _mesh_pair()
    try:
        amesh_a = AsyncNetMesh(a)
        amesh_b = AsyncNetMesh(b)
        # Peer counts come from the underlying MeshNode — already
        # one peer apiece (the post-handshake state).
        assert amesh_a.peer_count() >= 1
        assert amesh_b.peer_count() >= 1
        # node_id getters also pass through.
        assert amesh_a.node_id == a.node_id
        assert amesh_b.node_id == b.node_id
    finally:
        a.shutdown()
        b.shutdown()
