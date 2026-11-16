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

# Mesh handshake fixtures live in tests/conftest.py (P12):
# `mesh_pair` yields a connected (a, b) pair with auto-shutdown.


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


# `serve()` returns once the handler is registered locally, but the
# channel-membership advertisement to the peer is async (rides the
# substrate's broadcast). A `call()` issued before the peer has
# observed the new joiner is rejected with
# `RpcNoRouteError: membership request rejected: UnknownChannel`.
# Production callers retry; tests use the same pattern so we don't
# bake in a flaky fixed sleep.
_RETRY_INTERVAL_S = 0.3  # > heartbeat (200 ms); keeps the per-peer
# auth-failure budget (default 16/min) safe across the 3 s window.


def _call_until_routed(cli, target, service, body, timeout_s=3.0):
    import time

    from net import RpcNoRouteError

    deadline = time.time() + timeout_s
    while True:
        try:
            return cli.call(target, service, body)
        except RpcNoRouteError:
            if time.time() >= deadline:
                raise
            time.sleep(_RETRY_INTERVAL_S)


async def _acall_until_routed(acli, target, service, body, timeout_s=3.0):
    import time as _time

    from net import RpcNoRouteError

    deadline = _time.time() + timeout_s
    while True:
        try:
            return await acli.call(target, service, body)
        except RpcNoRouteError:
            if _time.time() >= deadline:
                raise
            await asyncio.sleep(_RETRY_INTERVAL_S)


# Cancel propagation from the substrate's CANCEL frame into an
# `async def` server handler's `await asyncio.sleep(...)` is wired
# through `async_bridge::dispatch_handler_coro`, which submits the
# handler coroutine via `asyncio.run_coroutine_threadsafe` and
# propagates Rust-future drop into `concurrent.futures.Future.cancel()`
# on the dispatcher loop. The two regression tests below pin that
# chain end-to-end.


def test_sync_caller_sync_server_unary(mesh_pair) -> None:
    """Regression: the original sync API still works after async lands."""
    a, b = mesh_pair
    srv = MeshRpc(b)
    cli = MeshRpc(a)
    h = srv.serve("echo", _sync_echo)
    try:
        reply = _call_until_routed(cli, b.node_id, "echo", b"hi")
        assert reply == b"hi"
    finally:
        h.close()


def test_sync_caller_async_server_unary(mesh_pair) -> None:
    """A sync caller reaches an `async def` server handler.

    The async handler runs as a coroutine on the substrate's tokio
    runtime; the sync caller blocks on its `block_on(call(...))`
    until the coroutine resolves and the reply lands."""
    a, b = mesh_pair
    asrv = AsyncMeshRpc(b)
    cli = MeshRpc(a)
    h = asrv.serve("echo", _async_echo)
    try:
        reply = _call_until_routed(cli, b.node_id, "echo", b"async-via-sync")
        assert reply == b"async-via-sync"
    finally:
        h.close()


def test_async_caller_sync_server_unary(mesh_pair) -> None:
    """An async caller reaches a sync handler.

    The sync handler runs on the substrate's `spawn_blocking` path;
    the async caller awaits a Python awaitable that resolves when
    the substrate reply lands."""
    a, b = mesh_pair

    async def _run() -> bytes:
        srv = MeshRpc(b)
        acli = AsyncMeshRpc(a)
        h = srv.serve("echo", _sync_echo)
        try:
            return await _acall_until_routed(
                acli, b.node_id, "echo", b"sync-via-async"
            )
        finally:
            h.close()

    reply = asyncio.run(_run())
    assert reply == b"sync-via-async"


def test_async_caller_async_server_unary(mesh_pair) -> None:
    """End-to-end async path: `async def` handler + `await call`.

    Both sides ride the same shared `MeshNode`; the reply lands on
    the async caller's awaitable without ever blocking a Python
    thread."""
    a, b = mesh_pair

    async def _run() -> bytes:
        asrv = AsyncMeshRpc(b)
        acli = AsyncMeshRpc(a)
        h = asrv.serve("echo", _async_echo)
        try:
            return await _acall_until_routed(
                acli, b.node_id, "echo", b"both-async"
            )
        finally:
            h.close()

    reply = asyncio.run(_run())
    assert reply == b"both-async"


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


def test_wait_for_timeout_propagates_to_substrate_cancel(mesh_pair) -> None:
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
    import threading

    from net import RpcCancelledError, RpcError  # noqa: F401

    a, b = mesh_pair

    # `threading.Event`, not `asyncio.Event`: the handler runs on the
    # dispatcher loop's thread, the assertion awaits on the outer
    # `asyncio.run` loop. `asyncio.Event.set()` from a foreign loop's
    # thread calls `fut.set_result()` → `loop.call_soon()` against
    # the wrong loop and doesn't wake the parked waiter, so the
    # signal would race-lose even when cancel did propagate.
    handler_was_cancelled = threading.Event()

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
            # Give the substrate's channel-membership advertisement a
            # beat to reach the caller. Without this the wait_for
            # timeout below races against route-propagation, and we
            # see NoRoute instead of the cancel we want to test.
            await asyncio.sleep(0.5)
            with pytest.raises(asyncio.TimeoutError):
                await asyncio.wait_for(
                    acli.call(b.node_id, "slow", b"never-resolves"),
                    timeout=0.2,
                )
            # Give the substrate a moment to deliver the cancel.
            # The handler-side CancelledError fires as soon as the
            # tokio cancel-watcher trips the substrate's cancel
            # registry; that latency is sub-millisecond in steady
            # state but allow a generous bound for CI. `to_thread`
            # bridges the blocking `threading.Event.wait` onto the
            # outer loop without blocking it.
            got_cancel = await asyncio.to_thread(
                handler_was_cancelled.wait, 1.0
            )
            if not got_cancel:
                pytest.fail(
                    "handler never observed CancelledError — "
                    "asyncio cancel did not propagate to substrate"
                )
        finally:
            h.close()

    asyncio.run(_run())


def test_streaming_mid_iter_cancel_terminates_stream(mesh_pair) -> None:
    """A streaming server emitting on a slow cadence; an async
    consumer breaks out of the `async for` loop after one chunk.
    The remaining substrate-side stream pulls must be dropped
    (substrate cancel-watcher fires on construction-time token)
    rather than continuing in the background.

    This pins the per-stream cancel contract: a mid-stream
    `task.cancel()` (here triggered via `wait_for` on a single
    `__anext__`) terminates the WHOLE stream, not just one pull.
    """
    # Two sides: server registers a duplex handler that sends a
    # first chunk fast, then sleeps before sending more. Client
    # opens via call_duplex, reads one chunk, then asyncio.wait_for
    # times out a `__anext__`. The substrate cancel-watcher should
    # observe the cancel and terminate the stream.
    a, b = mesh_pair

    async def _duplex_handler(stream, sink):
        # Read whatever the caller sends; emit one fast chunk + one
        # slow chunk. The slow chunk waits long enough that the
        # caller's wait_for will time out and trip cancel.
        for _ in stream:
            sink.send(b"first")
            # Sleep blocks one bridge worker — fine for a smoke
            # test. Cancel propagation is via the substrate's
            # stream-cancel watcher, not via dropping this sleep.
            import time as _time
            _time.sleep(2.0)
            sink.send(b"second")

    async def _run() -> bool:
        asrv = AsyncMeshRpc(b)
        acli = AsyncMeshRpc(a)
        h = asrv.serve_duplex("duplex-cancel", _duplex_handler)
        try:
            # Same membership-propagation beat as the cancel test —
            # `serve_duplex` returns once the channel is joined
            # locally, but the join-advertisement to the caller side
            # rides the next broadcast. Without the wait, the
            # `call_duplex` open races the propagation and gets
            # rejected with UnknownChannel.
            await asyncio.sleep(0.5)
            call = await acli.call_duplex(b.node_id, "duplex-cancel")
            await call.send(b"go")
            await call.finish_sending()
            # First chunk arrives promptly.
            chunk = await asyncio.wait_for(call.__anext__(), timeout=1.0)
            assert chunk == b"first"
            # Second pull times out — the substrate-side cancel
            # should fire on construction-time token and drop the
            # stream.
            with pytest.raises(asyncio.TimeoutError):
                await asyncio.wait_for(call.__anext__(), timeout=0.2)
            return True
        finally:
            h.close()

    # Streaming serve from Python is real now (T1-A7 completion).
    # The body above exercises the cancel propagation contract
    # end-to-end. If `serve_duplex` ever regresses to "not exposed",
    # this test fails with a clear AttributeError rather than
    # silently skipping.
    assert asyncio.run(_run())


def test_cancelling_a_ready_pull_terminates_the_stream(mesh_pair) -> None:
    """PYBR-02: cancel a pull whose chunk is already buffered.

    The bridge polls the Rust pull before its cancellation receiver, so
    the pull resolves with the buffered chunk, and the cancelled Python
    future then discards it. Before the fix nothing cancelled the
    substrate call: the chunk was silently lost and the stream carried
    on to a clean EOF. A cancelled pull must terminate the WHOLE stream
    with a cancellation error instead.
    """
    from net import RpcCancelledError

    import threading

    a, b = mesh_pair
    srv = MeshRpc(b)
    all_sent = threading.Event()

    def _producer(_req: bytes, sink) -> None:
        for i in range(3):
            sink.send(str(i).encode())
        all_sent.set()
        # Keep the stream open so the buffered chunks are not followed
        # by an EOF terminal while the client cancels.
        import time as _time

        _time.sleep(3.0)

    h = srv.serve_streaming("pull-cancel", _producer, 60_000)

    async def _run() -> list:
        acli = AsyncMeshRpc(a)
        await asyncio.sleep(0.5)
        stream = await acli.call_streaming(b.node_id, "pull-cancel", b"go")
        assert await asyncio.wait_for(stream.__anext__(), timeout=2.0) == b"0"
        # Synchronize with the producer rather than guessing: wait until
        # chunks 1 and 2 have been SENT, then give loopback delivery a
        # moment so the next pull is READY when it is cancelled (the path
        # this test exists for; a pending pull is cancelled by a different
        # path that never had the bug).
        assert await asyncio.to_thread(all_sent.wait, 5.0), "producer stalled"
        await asyncio.sleep(0.5)
        pull = stream.__anext__()
        pull.cancel()
        await asyncio.sleep(0.3)
        rest = []
        with pytest.raises(RpcCancelledError):
            while True:
                rest.append(await asyncio.wait_for(stream.__anext__(), timeout=5.0))
        return rest

    try:
        rest = asyncio.run(_run())
        # Whatever drained before the terminal, it is never the chunk the
        # cancelled pull consumed.
        assert b"1" not in rest
    finally:
        h.close()


def test_a_registration_after_shutdown_is_not_retained(next_port) -> None:
    """PYBR-01: shutdown is terminal for node-owned registrations.

    An AsyncMeshRpc owns its own reference to the node, so it can still
    register after ``NetMesh.shutdown()`` has swept the node's services.
    That registration used to be held in a fresh keepalive bucket that
    no later shutdown would sweep (a repeated shutdown returns early),
    keeping the service and node alive for the life of the process. It
    is now refused: the handle comes back already closed.
    """
    mesh = NetMesh(
        bind_addr=next_port(),
        psk="42" * 32,
        heartbeat_interval_ms=200,
        permissive_channels=True,
    )
    mesh.start()
    retained = AsyncMeshRpc(mesh)
    before = retained.serve("before-shutdown", _sync_echo)
    assert not before.is_closed()

    mesh.shutdown()
    assert before.is_closed(), "shutdown releases the services it swept"

    # Not wrapped in a broad `except RuntimeError`: the documented outcome
    # is an already-closed handle, and any error here is a real failure.
    after = retained.serve("after-shutdown", _sync_echo)
    assert after.is_closed(), (
        "a registration after a completed shutdown must not be retained"
    )
    after.close()


def test_async_netmesh_shares_handshake_with_sync_netmesh(mesh_pair) -> None:
    """`AsyncNetMesh(mesh)` doesn't re-handshake — the same peer
    connection set up by the sync `NetMesh.connect/.accept` is
    visible to the async wrapper.

    This is the "shared MeshNode" contract from the plan's locked
    decision #4 — proves an AsyncNetMesh constructed against an
    already-connected mesh sees the existing peer count without
    a re-handshake."""
    a, b = mesh_pair
    amesh_a = AsyncNetMesh(a)
    amesh_b = AsyncNetMesh(b)
    # Peer counts come from the underlying MeshNode — already
    # one peer apiece (the post-handshake state).
    assert amesh_a.peer_count() >= 1
    assert amesh_b.peer_count() >= 1
    # node_id getters also pass through.
    assert amesh_a.node_id == a.node_id
    assert amesh_b.node_id == b.node_id
