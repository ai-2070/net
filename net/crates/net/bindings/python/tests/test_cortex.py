"""Smoke tests for the CortEX Python bindings (tasks + memories).

Mirrors the Node vitest suite. Exercises CRUD, filter queries, and
the sync Python iterator protocol for watchers.
"""

import threading
import time

import pytest

from net._net import (
    CortexError,
    MemoriesAdapter,
    Redex,
    TasksAdapter,
    WriteToken,
)

ORIGIN = 0xABCDEF01


def now_ns() -> int:
    return time.time_ns()


# =========================================================================
# Tasks
# =========================================================================


def test_tasks_full_lifecycle() -> None:
    redex = Redex()
    tasks = TasksAdapter.open(redex, ORIGIN)

    t0 = now_ns()
    tasks.create(1, "write plan", t0)
    tasks.create(2, "ship adapter", t0 + 1)
    tasks.rename(1, "write better plan", t0 + 2)
    seq = tasks.complete(2, t0 + 3)
    tasks.wait_for_seq(seq)

    all_tasks = tasks.list_tasks()
    assert len(all_tasks) == 2

    by_id = {t.id: t for t in all_tasks}
    assert by_id[1].title == "write better plan"
    assert by_id[1].status == "pending"
    assert by_id[2].title == "ship adapter"
    assert by_id[2].status == "completed"


def test_tasks_filter_and_order() -> None:
    redex = Redex()
    tasks = TasksAdapter.open(redex, ORIGIN)

    for i in range(1, 6):
        tasks.create(i, f"t-{i}", 100 * i)
    seq = tasks.complete(1, 999)
    tasks.wait_for_seq(seq)

    # Status filter.
    pending = tasks.list_tasks(status="pending")
    assert sorted(t.id for t in pending) == [2, 3, 4, 5]

    # Order + limit.
    newest = tasks.list_tasks(order_by="created_desc", limit=2)
    assert [t.id for t in newest] == [5, 4]


def test_tasks_delete_removes_task() -> None:
    redex = Redex()
    tasks = TasksAdapter.open(redex, ORIGIN)
    tasks.create(1, "ephemeral", 100)
    seq = tasks.delete(1)
    tasks.wait_for_seq(seq)
    assert tasks.count() == 0


def test_tasks_ingest_after_close_errors() -> None:
    redex = Redex()
    tasks = TasksAdapter.open(redex, ORIGIN)
    tasks.create(1, "before", 100)
    tasks.close()
    with pytest.raises(CortexError, match="closed"):
        tasks.create(2, "after", 200)


def test_tasks_invalid_status_raises() -> None:
    redex = Redex()
    tasks = TasksAdapter.open(redex, ORIGIN)
    with pytest.raises(ValueError):
        tasks.list_tasks(status="nonsense")


# =========================================================================
# Memories
# =========================================================================


def test_memories_full_lifecycle() -> None:
    redex = Redex()
    memories = MemoriesAdapter.open(redex, ORIGIN)

    memories.store(1, "meeting notes", ["work", "notes"], "alice", 100)
    memories.store(2, "grocery list", ["personal", "todo"], "alice", 200)
    memories.store(3, "api design", ["work", "design"], "bob", 300)
    memories.retag(1, ["work", "meetings"], 310)
    memories.pin(3, 320)
    seq = memories.pin(1, 330)
    memories.wait_for_seq(seq)

    assert memories.count() == 3

    work = memories.list_memories(tag="work")
    assert sorted(m.id for m in work) == [1, 3]

    any_match = memories.list_memories(any_tag=["design", "todo"])
    assert sorted(m.id for m in any_match) == [2, 3]

    all_match = memories.list_memories(all_tags=["work", "meetings"])
    assert [m.id for m in all_match] == [1]

    pinned = memories.list_memories(pinned=True)
    assert sorted(m.id for m in pinned) == [1, 3]

    bob = memories.list_memories(source="bob")
    assert [m.id for m in bob] == [3]


def test_memories_content_search_case_insensitive() -> None:
    redex = Redex()
    memories = MemoriesAdapter.open(redex, ORIGIN)
    seq = memories.store(1, "Fire in the datacenter", [], "alice", 100)
    memories.wait_for_seq(seq)

    hit = memories.list_memories(content_contains="DATACENTER")
    assert [m.id for m in hit] == [1]

    miss = memories.list_memories(content_contains="unicorn")
    assert miss == []


# =========================================================================
# Watch (sync iterator protocol)
# =========================================================================


def test_watch_tasks_initial_emission() -> None:
    redex = Redex()
    tasks = TasksAdapter.open(redex, ORIGIN)

    tasks.create(1, "alpha", 100)
    seq = tasks.create(2, "beta", 200)
    tasks.wait_for_seq(seq)

    it = tasks.watch_tasks(status="pending", order_by="id_asc")
    try:
        initial = next(it)
        assert [t.id for t in initial] == [1, 2]
    finally:
        it.close()


def test_watch_tasks_close_stops_iteration() -> None:
    redex = Redex()
    tasks = TasksAdapter.open(redex, ORIGIN)

    it = tasks.watch_tasks()
    # Drain initial (empty).
    initial = next(it)
    assert initial == []

    # Close, then subsequent next raises StopIteration.
    it.close()
    with pytest.raises(StopIteration):
        next(it)


def test_watch_tasks_for_loop_exits_on_close() -> None:
    redex = Redex()
    tasks = TasksAdapter.open(redex, ORIGIN)

    emissions = []
    # Race condition: same as the Node test — fast fires can coalesce.
    # Check final state, not exact emission count.
    seen_states: set = set()

    def state_key(lst):
        return tuple(sorted(t.id for t in lst))

    it = tasks.watch_tasks(status="pending")

    def consume():
        for batch in it:
            emissions.append(batch)
            seen_states.add(state_key(batch))
            if state_key(batch) == (1, 2):
                it.close()

    reader = threading.Thread(target=consume)
    reader.start()

    tasks.create(1, "a", 100)
    tasks.create(2, "b", 200)

    reader.join(timeout=5)
    assert not reader.is_alive(), "watcher thread did not exit after close()"

    # Initial empty AND final (1, 2) state must both have been observed.
    assert () in seen_states
    assert (1, 2) in seen_states


def test_watch_memories_tag_filter() -> None:
    redex = Redex()
    memories = MemoriesAdapter.open(redex, ORIGIN)

    it = memories.watch_memories(tag="urgent")
    try:
        initial = next(it)
        assert initial == []

        # Store non-matching memory → no new emission yet.
        memories.store(1, "routine", ["later"], "alice", 100)

        # Store matching memory → emission [2].
        memories.store(2, "fire", ["urgent"], "alice", 200)
        next_batch = next(it)
        assert [m.id for m in next_batch] == [2]

        # Retag #1 to include urgent → emission grows.
        memories.retag(1, ["urgent", "later"], 300)
        next_batch = next(it)
        assert sorted(m.id for m in next_batch) == [1, 2]
    finally:
        it.close()


def test_persistent_tasks_round_trip(tmp_path) -> None:
    dir = str(tmp_path / "tasks")
    # First process: create + persist.
    redex1 = Redex(persistent_dir=dir)
    tasks1 = TasksAdapter.open(redex1, ORIGIN, persistent=True)
    tasks1.create(1, "durable", 100)
    tasks1.create(2, "also durable", 101)
    seq = tasks1.complete(1, 102)
    tasks1.wait_for_seq(seq)
    tasks1.close()
    del redex1, tasks1

    # Second process: reopen same dir, state replays from disk.
    redex2 = Redex(persistent_dir=dir)
    tasks2 = TasksAdapter.open(redex2, ORIGIN, persistent=True)
    tasks2.wait_for_seq(2)
    all_tasks = tasks2.list_tasks()
    assert len(all_tasks) == 2
    by_id = {t.id: t for t in all_tasks}
    assert by_id[1].status == "completed"
    assert by_id[2].status == "pending"
    assert by_id[2].title == "also durable"


def test_persistent_memories_round_trip(tmp_path) -> None:
    dir = str(tmp_path / "mem")
    redex1 = Redex(persistent_dir=dir)
    memories1 = MemoriesAdapter.open(redex1, ORIGIN, persistent=True)
    memories1.store(1, "alpha", ["x"], "alice", 100)
    memories1.pin(1, 110)
    memories1.store(2, "beta", ["y"], "alice", 200)
    seq = memories1.retag(2, ["y", "z"], 210)
    memories1.wait_for_seq(seq)
    memories1.close()
    del redex1, memories1

    redex2 = Redex(persistent_dir=dir)
    memories2 = MemoriesAdapter.open(redex2, ORIGIN, persistent=True)
    memories2.wait_for_seq(3)
    all_m = memories2.list_memories()
    assert len(all_m) == 2
    by_id = {m.id: m for m in all_m}
    assert by_id[1].pinned is True
    assert sorted(by_id[2].tags) == ["y", "z"]


def test_snapshot_and_restore_tasks() -> None:
    redex = Redex()
    tasks = TasksAdapter.open(redex, ORIGIN)

    tasks.create(1, "alpha", 100)
    tasks.create(2, "beta", 200)
    tasks.complete(1, 150)
    seq = tasks.rename(2, "beta-v2", 250)
    tasks.wait_for_seq(seq)

    state_bytes, last_seq = tasks.snapshot()
    assert last_seq == 3
    assert len(state_bytes) > 0
    tasks.close()

    # Restore on the same Redex — file keeps seqs 0..=3; adapter
    # tails at FromSeq(4). State comes from state_bytes.
    tasks2 = TasksAdapter.open_from_snapshot(
        redex, ORIGIN, state_bytes, last_seq=last_seq
    )
    all_tasks = tasks2.list_tasks()
    assert len(all_tasks) == 2
    by_id = {t.id: t for t in all_tasks}
    assert by_id[1].status == "completed"
    assert by_id[2].title == "beta-v2"

    # Continuing ingest works; next seq is 4.
    next_seq = tasks2.create(3, "gamma", 300)
    assert next_seq == 4
    tasks2.wait_for_seq(next_seq)
    assert tasks2.count() == 3


def test_snapshot_and_restore_memories() -> None:
    redex = Redex()
    memories = MemoriesAdapter.open(redex, ORIGIN)

    memories.store(1, "alpha", ["x"], "alice", 100)
    memories.pin(1, 110)
    memories.store(2, "beta", ["y"], "alice", 200)
    seq = memories.retag(2, ["y", "z"], 210)
    memories.wait_for_seq(seq)

    state_bytes, last_seq = memories.snapshot()
    assert last_seq == 3
    memories.close()

    memories2 = MemoriesAdapter.open_from_snapshot(
        redex, ORIGIN, state_bytes, last_seq=last_seq
    )
    all_m = memories2.list_memories()
    assert len(all_m) == 2
    by_id = {m.id: m for m in all_m}
    assert by_id[1].pinned is True
    assert sorted(by_id[2].tags) == ["y", "z"]


def test_empty_state_snapshot_has_none_last_seq() -> None:
    redex = Redex()
    tasks = TasksAdapter.open(redex, ORIGIN)
    state_bytes, last_seq = tasks.snapshot()
    assert last_seq is None
    assert len(state_bytes) > 0


def test_persistent_without_dir_errors() -> None:
    redex = Redex()  # heap-only, no persistent_dir
    with pytest.raises(CortexError, match="persistent"):
        TasksAdapter.open(redex, ORIGIN, persistent=True)


def test_closed_adapter_raises_cortex_error() -> None:
    redex = Redex()
    tasks = TasksAdapter.open(redex, ORIGIN)
    tasks.close()
    with pytest.raises(CortexError, match="closed"):
        tasks.create(1, "x", 100)
    # Still an Exception subclass, so broad catches keep working.
    assert issubclass(CortexError, Exception)


def test_cortex_error_memories_closed() -> None:
    redex = Redex()
    memories = MemoriesAdapter.open(redex, ORIGIN)
    memories.close()
    with pytest.raises(CortexError, match="closed"):
        memories.store(1, "x", ["tag"], "src", 100)


def test_multi_model_coexistence() -> None:
    redex = Redex()
    tasks = TasksAdapter.open(redex, ORIGIN)
    memories = MemoriesAdapter.open(redex, ORIGIN)

    tasks.create(1, "task-1", 100)
    memories.store(1, "mem-1", ["x"], "alice", 100)
    memories.store(2, "mem-2", ["x"], "alice", 200)
    ts = tasks.complete(1, 150)
    ms = memories.pin(1, 250)
    tasks.wait_for_seq(ts)
    memories.wait_for_seq(ms)

    assert tasks.count() == 1
    assert memories.count() == 2
    assert [m.id for m in memories.list_memories(pinned=True)] == [1]


# =========================================================================
# Write tokens carry their channel
# =========================================================================


def test_write_token_names_origin_channel_and_seq() -> None:
    redex = Redex()
    tasks = TasksAdapter.open(redex, ORIGIN)
    seq = tasks.create(1, "token me", 100)
    tok = tasks.token(seq)
    assert (tok.origin_hash, tok.channel_hash, tok.seq) == (ORIGIN, tasks.channel_hash(), seq)
    assert WriteToken.from_string(str(tok)) == tok
    tasks.wait_for_token(tok, deadline_ms=2000)
    assert tasks.count() == 1
    # No public constructor, and the old two-part string form is refused.
    with pytest.raises(TypeError):
        WriteToken(ORIGIN, seq)  # type: ignore[call-arg]
    with pytest.raises(ValueError):
        WriteToken.from_string(f"{ORIGIN:016x}:{seq}")


def test_a_tasks_token_is_refused_by_memories_with_the_same_origin() -> None:
    # Tasks and Memories number their writes independently. Memories is
    # past the task's seq below, so a channel-less token passed here.
    redex = Redex()
    tasks = TasksAdapter.open(redex, ORIGIN)
    memories = MemoriesAdapter.open(redex, ORIGIN)
    assert tasks.channel_hash() != memories.channel_hash()
    mem_seq = 0
    for i in range(3):
        mem_seq = memories.store(i, "m", [], "src", 100 + i)
    memories.wait_for_token(memories.token(mem_seq), deadline_ms=2000)
    task_seq = tasks.create(1, "t", 100)
    assert task_seq <= mem_seq

    tok = tasks.token(task_seq)
    for deadline in (0, 2000):
        with pytest.raises(CortexError, match="channel"):
            memories.wait_for_token(tok, deadline_ms=deadline)
    tasks.wait_for_token(tok, deadline_ms=2000)
