"""Tests for the gang-claim scheduler + task-lifecycle (WorkflowAdapter)
surface. Real-extension contract tests; mirror the Rust surface tests in
`sdk/tests/{gang_surface,workflow_surface}.rs` and the napi/vitest suite.

Each node self-indexes its own capability + island announcements, so the
single-node round-trip fully exercises the criteria marshaling + claim
pipeline. Multi-node propagation is covered by the Rust integration suite.
"""

from __future__ import annotations

from net import NetMesh, Redex, ShardGroup, TriggerEngine, WorkflowAdapter

PSK = "5b" * 32
ORIGIN = 0x0F10_5D01
UNTIL = 10**18


def _port(seed: int) -> str:
    return f"127.0.0.1:{28700 + seed}"


# ---- Task lifecycle (WorkflowAdapter) -------------------------------------


def test_workflow_lifecycle_round_trip() -> None:
    wf = WorkflowAdapter.open(Redex(), ORIGIN)
    wf.submit(1)
    wf.start(1)
    wf.advance(1)  # step 0 -> 1, attempts reset
    wf.wait_for_seq(wf.complete(1))
    st = wf.get(1)
    assert st is not None
    assert st.status == "done"
    assert st.step == 1
    assert wf.status_counts().done == 1


def test_workflow_terminal_state_is_not_resurrected() -> None:
    wf = WorkflowAdapter.open(Redex(), ORIGIN)
    wf.submit(1)
    wf.complete(1)
    wf.start(1)  # no-op: Done is terminal
    wf.wait_for_seq(wf.retry(1))  # no-op: Done is terminal
    assert wf.get(1).status == "done"


def test_workflow_delete_reclaims_task() -> None:
    wf = WorkflowAdapter.open(Redex(), ORIGIN)
    wf.submit(7)
    wf.wait_for_seq(wf.delete(7))
    assert wf.get(7) is None


# ---- Gang scheduler (NetMesh) --------------------------------------------


def test_reserve_release_and_unheld_release_is_lost() -> None:
    m = NetMesh(_port(1), PSK)
    try:
        assert m.reserve_island(0xA0, UNTIL) == "won"
        assert m.release_island(0xA0) == "won"
        # Releasing an island this node never held -> lost (not a false won).
        assert m.release_island(0xBEEF) == "lost"
    finally:
        m.shutdown()


def test_single_node_publish_match_claim() -> None:
    m = NetMesh(_port(2), PSK)
    try:
        m.announce_capabilities({"tags": ["gpu:h100"]})
        m.publish_island_topology(0xD0, [0, 1, 2, 3, 4, 5, 6, 7], [0xA1], 0.1, 800)
        assert (
            m.match_gpu_islands(["gpu:h100"], min_gpus=8, selection="least_loaded")
            == [0xD0]
        )
        assert m.claim_gpu_island(["gpu:h100"], UNTIL, min_gpus=8) == 0xD0
    finally:
        m.shutdown()


# ---- Tier 2: shards + triggers -------------------------------------------


def test_shards_fan_out_then_join() -> None:
    wf = WorkflowAdapter.open(Redex(), ORIGIN)
    group = ShardGroup([10, 11, 12], 99)
    seq = wf.fan_out(group)
    wf.wait_for_seq(seq)
    assert wf.try_join(group).kind == "pending"

    last = 0
    for s in (10, 11, 12):
        last = wf.complete(s)
    wf.wait_for_seq(last)

    j = wf.try_join(group)
    assert j.kind == "submitted"
    if j.seq is not None:
        wf.wait_for_seq(j.seq)
    assert wf.get(99) is not None
    assert wf.try_join(group).kind == "already_submitted"


def test_trigger_fires_dependent_on_done() -> None:
    wf = WorkflowAdapter.open(Redex(), ORIGIN)
    eng = TriggerEngine(wf)
    eng.arm_after_task(1, "submit", 2)  # B depends on A

    wf.submit(1)
    wf.start(1)
    wf.wait_for_seq(wf.complete(1))

    actions = eng.on_task_change(1)
    assert len(actions) == 1
    assert actions[0].kind == "submit"
    assert actions[0].id == 2
    assert eng.armed_count() == 0
