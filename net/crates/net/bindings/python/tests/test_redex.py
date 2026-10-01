"""Smoke tests for the raw `RedexFile` surface.

Exercises append / append_batch / read_range / tail / close and the
sync `RedexTailIter` lifecycle. Mirrors the vitest and Rust SDK
redex suites.
"""

import threading
import time

import pytest

from net import Redex, RedexError, RedexEvent, RedexFile


def test_open_file_returns_handle() -> None:
    redex = Redex()
    file = redex.open_file("test/basic")
    assert isinstance(file, RedexFile)
    assert len(file) == 0


def test_append_and_read_range() -> None:
    redex = Redex()
    file = redex.open_file("test/rr")
    seq = file.append(b"hello")
    assert seq == 0
    assert len(file) == 1

    events = file.read_range(0, 10)
    assert len(events) == 1
    ev = events[0]
    assert isinstance(ev, RedexEvent)
    assert ev.seq == 0
    assert bytes(ev.payload) == b"hello"
    assert ev.is_inline is False  # 5 bytes > 8-byte inline threshold? Actually fits but not marked inline in append path


def test_append_batch_returns_first_seq() -> None:
    redex = Redex()
    file = redex.open_file("test/batch")
    first = file.append_batch([b"a", b"b", b"c"])
    assert first == 0
    assert len(file) == 3
    events = file.read_range(0, 5)
    assert [bytes(e.payload) for e in events] == [b"a", b"b", b"c"]


def test_append_batch_empty_returns_none() -> None:
    """Pin the breaking-change semantics from the Rust core:
    `append_batch([])` returns `None` rather than `0`. Pre-fix the
    Rust side returned `0` (the first seq that *would have been*
    assigned), which collided with the legitimate "first event of a
    non-empty batch is seq 0" return — callers couldn't distinguish
    "I appended nothing" from "I appended one event at seq 0". The
    Node binding pins this with its own
    `'append_batch returns null on empty input'` test in
    `bindings/node/test/redex.test.ts`; this is the Python parity.
    """
    redex = Redex()
    file = redex.open_file("test/batch-empty")
    first = file.append_batch([])
    assert first is None
    assert len(file) == 0


def test_repeat_open_returns_same_handle() -> None:
    redex = Redex()
    a = redex.open_file("test/shared")
    a.append(b"from-a")
    b = redex.open_file("test/shared")
    assert len(b) == 1


def test_invalid_channel_name_raises_redex_error() -> None:
    redex = Redex()
    with pytest.raises(RedexError):
        redex.open_file("bad name with spaces")


def test_mutually_exclusive_fsync_raises() -> None:
    redex = Redex()
    with pytest.raises(RedexError):
        redex.open_file("test/bad-fsync", fsync_every_n=10, fsync_interval_ms=100)


def test_fsync_every_n_zero_rejected() -> None:
    redex = Redex()
    with pytest.raises(RedexError):
        redex.open_file("test/zero", fsync_every_n=0)


def test_tail_backfill_and_live() -> None:
    redex = Redex()
    file = redex.open_file("test/tail")
    file.append(b"early-1")
    file.append(b"early-2")

    it = file.tail(from_seq=0)

    first = next(it)
    assert bytes(first.payload) == b"early-1"

    second = next(it)
    assert bytes(second.payload) == b"early-2"

    # Spawn a thread that appends live while the main thread waits on
    # the iterator.
    def later() -> None:
        time.sleep(0.02)
        file.append(b"live")

    t = threading.Thread(target=later, daemon=True)
    t.start()
    third = next(it)
    t.join()
    assert bytes(third.payload) == b"live"
    assert third.seq == 2

    it.close()
    with pytest.raises(StopIteration):
        next(it)


def test_tail_from_seq_skips_earlier() -> None:
    redex = Redex()
    file = redex.open_file("test/from-seq")
    for i in range(5):
        file.append(f"e{i}".encode())
    it = file.tail(from_seq=3)
    first = next(it)
    assert first.seq == 3
    it.close()


def test_close_on_file_ends_tail_iter() -> None:
    redex = Redex()
    file = redex.open_file("test/close-file")
    file.append(b"x")
    it = file.tail(from_seq=0)
    _ = next(it)   # drain backfill

    # Close the file in a background thread; the iterator's next()
    # should return promptly rather than hang.
    def close_soon() -> None:
        time.sleep(0.02)
        file.close()

    t = threading.Thread(target=close_soon, daemon=True)
    t.start()

    # Wait for StopIteration with a soft budget (the thread will pump
    # close within ~20ms, iter.next() should then return immediately).
    start = time.monotonic()
    with pytest.raises(StopIteration):
        next(it)
    elapsed = time.monotonic() - start
    t.join()
    assert elapsed < 2.0, f"tail iter hung for {elapsed:.2f}s after file.close()"


def test_retention_options_accepted() -> None:
    redex = Redex()
    # Non-error smoke — ensure retention kwargs plumb through without
    # blowing up on type coercion.
    file = redex.open_file(
        "test/retention",
        retention_max_events=100,
        retention_max_bytes=1024 * 1024,
        retention_max_age_ms=60_000,
    )
    assert len(file) == 0


def test_replicated_open_file_runs_on_the_mesh_runtime():
    """A replicated ``open_file`` spawns its replication runtime.

    That spawn ran on the Python thread, which has no tokio reactor, so
    the first replicated ``open_file`` raised ``PanicException: there is
    no reactor running``. ``enable_replication`` now records the mesh's
    runtime and ``open_file`` enters it.
    """
    net = pytest.importorskip("net")
    if not hasattr(Redex, "enable_replication"):
        pytest.fail("wheel built without the `net` feature")
    mesh = net.NetMesh("127.0.0.1:0", "7a" * 32)
    try:
        r = Redex()
        r.enable_replication(mesh)
        file = r.open_file(
            "py/repl",
            replication=True,
            replication_heartbeat_ms=150,
            replication_placement="pinned",
            replication_pinned_nodes=[mesh.node_id],
        )
        assert r.replication_runtime_count() == 1
        assert 'channel="py/repl"' in r.replication_prometheus_text()
        assert file.append(b"x") == 0
    finally:
        mesh.shutdown()
