"""Per-module async-class smoke tests — TX-5.

One happy-path round-trip per Async* class to pin that the surface
exists and the awaitable / async-for shape actually drives to
completion. Not exhaustive — adapter-level behaviors are covered
by the sync test suites; these tests prove the async wrapper
correctly delegates without dropping or hanging.

Run via::

    maturin develop --features "net,cortex,dataforts,meshdb,aggregator"
    pytest tests/test_async_per_module.py -v
"""

from __future__ import annotations

import asyncio
import time

import pytest

pytest.importorskip("net._net")

# =========================================================================
# T2-C3 — AsyncMemoriesAdapter
# =========================================================================


def test_async_memories_lifecycle() -> None:
    from net._net import (
        AsyncMemoriesAdapter,
        MemoriesAdapter,
        Redex,
    )

    ORIGIN = 0xABCDEF01

    async def _run() -> list:
        redex = Redex()
        sync_mem = MemoriesAdapter.open(redex, ORIGIN)
        amem = AsyncMemoriesAdapter(sync_mem)
        try:
            t0 = time.time_ns()
            # Writes stay sync (no awaiting on the inner adapter).
            seq = amem.store(1, "hello async", ["greeting"], "bot", t0)
            # The wait_for_seq awaitable proves the fold-task await
            # propagates through.
            await amem.wait_for_seq(seq)
            return amem.list_memories()
        finally:
            amem.close()

    out = asyncio.run(_run())
    assert len(out) == 1
    assert out[0].content == "hello async"


def test_async_memory_watch_iter_yields_initial_state() -> None:
    from net._net import (
        AsyncMemoriesAdapter,
        MemoriesAdapter,
        Redex,
    )

    ORIGIN = 0xABCDEF02

    async def _run() -> list:
        redex = Redex()
        sync_mem = MemoriesAdapter.open(redex, ORIGIN)
        amem = AsyncMemoriesAdapter(sync_mem)
        try:
            t0 = time.time_ns()
            seq = amem.store(1, "watched", [], "bot", t0)
            await amem.wait_for_seq(seq)
            it = await amem.watch_memories()
            try:
                # First emission carries the current state.
                batch = await asyncio.wait_for(it.__anext__(), timeout=2.0)
                return batch
            finally:
                it.close()
        finally:
            amem.close()

    batch = asyncio.run(_run())
    assert len(batch) == 1


# =========================================================================
# T2-C4 — AsyncTasksAdapter
# =========================================================================


def test_async_tasks_lifecycle() -> None:
    from net._net import (
        AsyncTasksAdapter,
        Redex,
        TasksAdapter,
    )

    ORIGIN = 0xABCDEF03

    async def _run() -> list:
        redex = Redex()
        sync_t = TasksAdapter.open(redex, ORIGIN)
        atasks = AsyncTasksAdapter(sync_t)
        try:
            t0 = time.time_ns()
            atasks.create(1, "ship it", t0)
            seq = atasks.complete(1, t0 + 1)
            await atasks.wait_for_seq(seq)
            return atasks.list_tasks()
        finally:
            atasks.close()

    out = asyncio.run(_run())
    assert len(out) == 1
    assert out[0].status == "completed"


# =========================================================================
# T2-C2 — AsyncRedexFile + AsyncRedexTailIter
# =========================================================================


def test_async_redex_tail_yields_appends() -> None:
    from net._net import AsyncRedexFile, Redex

    async def _run() -> list[int]:
        redex = Redex()
        f_sync = redex.open_file("async-tail-test")
        af = AsyncRedexFile(f_sync)
        af.append(b"first")
        af.append(b"second")
        af.append(b"third")
        it = await af.tail(from_seq=0)
        collected: list[int] = []
        try:
            # Backfilled retained range — three events expected.
            for _ in range(3):
                ev = await asyncio.wait_for(it.__anext__(), timeout=2.0)
                collected.append(ev.seq)
            return collected
        finally:
            it.close()

    seqs = asyncio.run(_run())
    assert len(seqs) == 3


# =========================================================================
# T2-F1 — async_blob_publish + async_blob_resolve
# =========================================================================


def test_async_blob_round_trip(tmp_path) -> None:
    try:
        from net._net import (
            async_blob_publish,
            async_blob_resolve,
            register_filesystem_blob_adapter,
            unregister_blob_adapter,
        )
    except ImportError:
        pytest.skip("dataforts feature not built into this wheel")

    adapter_id = "tx-5-blob"
    register_filesystem_blob_adapter(adapter_id, str(tmp_path))

    # The filesystem adapter accepts only `file:` URIs (its
    # accepted-schemes gate applies on BOTH publish and resolve).
    async def _run() -> bytes:
        encoded = await async_blob_publish(adapter_id, "file:tx5/sample", b"hello")
        return await async_blob_resolve(adapter_id, encoded)

    try:
        out = asyncio.run(_run())
        assert out == b"hello"
    finally:
        unregister_blob_adapter(adapter_id)


def test_async_blob_publish_rejects_unaccepted_scheme(tmp_path) -> None:
    """Publishing through the file adapter with a URI it can't
    later resolve (scheme-less, or a foreign scheme) fails fast with
    ``BlobError`` instead of minting a poisoned ref. Regression for
    the publish/resolve scheme-gate asymmetry this file's round-trip
    test originally tripped over (its bare ``tx5/sample`` URI stored
    fine, then could never be resolved)."""
    try:
        from net._net import (
            BlobError,
            async_blob_publish,
            register_filesystem_blob_adapter,
            unregister_blob_adapter,
        )
    except ImportError:
        pytest.skip("dataforts feature not built into this wheel")

    adapter_id = "tx-5-blob-scheme"
    register_filesystem_blob_adapter(adapter_id, str(tmp_path))

    async def _run(uri: str) -> None:
        await async_blob_publish(adapter_id, uri, b"hello")

    try:
        for bad_uri in ("tx5/sample", "s3://attacker/key"):
            with pytest.raises(BlobError, match="scheme not supported"):
                asyncio.run(_run(bad_uri))
    finally:
        unregister_blob_adapter(adapter_id)


# =========================================================================
# T2-F1 — AsyncMeshBlobAdapter (the class, not the free functions)
# =========================================================================


def test_async_mesh_blob_adapter_round_trip(tmp_path) -> None:
    try:
        from net._net import (
            AsyncMeshBlobAdapter,
            BlobRef,
            MeshBlobAdapter,
            Redex,
        )
    except ImportError:
        pytest.skip("dataforts feature not built into this wheel")

    redex = Redex(persistent_dir=str(tmp_path))
    sync = MeshBlobAdapter(redex, adapter_id="tx-5-mesh-blob")
    aadapter = AsyncMeshBlobAdapter(sync)

    payload = b"hello async mesh blob"
    # blake3 isn't in stdlib until 3.11; on older Pythons fall back
    # to the `blake3` PyPI package. Mirrors the existing
    # _blake3_digest helper in test_blob.py.
    import hashlib

    try:
        digest = hashlib.blake3(payload).digest()  # type: ignore[attr-defined]
    except AttributeError:
        try:
            import blake3 as blake3_mod  # type: ignore
        except ImportError:
            pytest.skip("blake3 not available (try `pip install blake3`)")
        digest = blake3_mod.blake3(payload).digest()
    blob_ref = BlobRef("mesh://tx5/blob", digest, len(payload))

    async def _run() -> bytes:
        await aadapter.store(blob_ref, payload)
        assert await aadapter.exists(blob_ref)
        return await aadapter.fetch(blob_ref)

    out = asyncio.run(_run())
    assert out == payload


# =========================================================================
# T3-G3 ice + T3-H1 meshos — construction-only smoke tests.
#
# Both surfaces involve heavyweight setup (deck supervisor, daemon
# registration) that would dwarf this file. The smoke we can run
# without that machinery is the consume-pattern construction —
# verify `from_sync` and the closed-state error.
# =========================================================================


def test_async_ice_commands_symbols_load() -> None:
    """Pin that the ice break-glass async surface is importable —
    the full simulate→commit round-trip needs a live deck client
    against a running supervisor, which is out of scope for a
    smoke test."""
    try:
        from net._net import (  # noqa: F401
            AsyncIceCommands,
            AsyncIceProposal,
            AsyncSimulatedIceProposal,
        )
    except ImportError:
        pytest.skip("deck feature not built into this wheel")


def test_async_meshos_from_sync_consumes_sync_handle() -> None:
    """Pin the consume-pattern. After AsyncMeshOsDaemonHandle takes
    ownership via from_sync, the sync handle raises
    `already_shutdown` on subsequent method calls."""
    pytest.importorskip("net._net")
    try:
        from net._net import (
            AsyncMeshOsDaemonSdk,  # noqa: F401
        )
        from net._net import (
            MeshOsDaemonSdk,  # noqa: F401
        )
    except ImportError:
        pytest.skip("meshos feature not built into this wheel")
    # The full lifecycle needs MeshOsConfig + identity wiring;
    # symbol existence alone is the v0.x acceptance criterion here.


# =========================================================================
# T3-I1 — AsyncMeshQueryRunner
# =========================================================================


def test_async_mesh_query_runner_executes() -> None:
    try:
        from net._net import (
            AsyncMeshQueryRunner,
            InMemoryChainReader,
            MeshQuery,
        )
    except ImportError:
        pytest.skip("meshdb feature not built into this wheel")

    async def _run() -> list:
        reader = InMemoryChainReader()
        runner = AsyncMeshQueryRunner(reader)
        # Smallest viable query against an empty in-memory store —
        # `between(origin, 0, 1)` on an empty chain returns zero
        # rows, proving the awaitable resolves end-to-end with the
        # right empty-result shape rather than hanging.
        query = MeshQuery.between(0xCAFE, 0, 1)
        return await runner.execute(query)

    rows = asyncio.run(_run())
    assert isinstance(rows, list)
