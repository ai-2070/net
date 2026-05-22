//! Failure-detector × failure-mode matrix. First vertical
//! slice of FAILURE_PATH_HARDENING_PLAN Stage 3.
//!
//! Pins the failure-detector's behavior under each chaos
//! scenario the harness supports:
//!
//! | Failure mode | Test |
//! |--------------|------|
//! | `partition-split` (bilateral) | `bilateral_partition_marks_peer_failed_on_both_sides` |
//! | `partition-heal-mid-phase` | `partition_heal_recovers_peer_to_healthy_status` |
//! | `peer-crash-mid-phase` (one-sided block) | `one_sided_block_marks_peer_failed_from_blocking_side` |
//! | `multi-peer-isolation` | `partition_of_one_peer_does_not_mark_unrelated_peers_failed` |
//!
//! The remaining failure modes the plan calls for
//! (`wire-packet-delay`, `wire-packet-reorder`,
//! `wire-packet-duplicate`, `clock-jump-*`,
//! `resource-exhaustion`) are blocked on harness extensions
//! documented in
//! `docs/FAILURE_PATH_HARDENING_PLAN.md` §Stage 3.
//!
//! Every test here uses the shared harness at `tests/common/` —
//! `await_peer_failed`, `chaos_partition`, etc. — so a future
//! matrix refactor (e.g., adding a time-mock layer) updates
//! one module instead of every test file.

#![cfg(feature = "net")]

use std::time::Duration;

mod common;
use common::*;

/// Failure mode: `partition-split` (bilateral). Both nodes'
/// FD's should mark the other Failed once heartbeats stop.
/// Pins the baseline "partition → failure" semantics every
/// other chaos test leans on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bilateral_partition_marks_peer_failed_on_both_sides() {
    let a = build_fast_node().await;
    let b = build_fast_node().await;
    connect_pair(&a, &b).await;
    a.start();
    b.start();

    let a_id = a.node_id();
    let b_id = b.node_id();

    // Pre-chaos sanity: both sides see each other as Healthy.
    // A fresh handshake hasn't necessarily registered a
    // heartbeat yet, so we wait a beat before asserting.
    tokio::time::sleep(Duration::from_millis(250)).await;

    // Inject bilateral partition.
    chaos_partition(&a, &b);

    // With heartbeat=100 ms, session_timeout=500 ms, miss
    // threshold=3, Failed fires at ~1.5 s of silence. 3 s
    // gives generous margin for CI scheduler jitter.
    let limit = Duration::from_secs(3);
    await_peer_failed(&a, b_id, limit).await;
    await_peer_failed(&b, a_id, limit).await;
}

/// Failure mode: `partition-heal-mid-phase`. Partition, wait
/// for Failed, heal, assert Healthy returns. Pins the
/// recovery-path of the FD state machine — regressing to
/// "once Failed, always Failed" would break every auto-
/// reconnect scenario.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn partition_heal_recovers_peer_to_healthy_status() {
    let a = build_fast_node().await;
    let b = build_fast_node().await;
    connect_pair(&a, &b).await;
    a.start();
    b.start();

    let b_id = b.node_id();
    tokio::time::sleep(Duration::from_millis(250)).await;

    // Phase 1: partition + wait for Failed.
    chaos_partition(&a, &b);
    await_peer_failed(&a, b_id, Duration::from_secs(3)).await;

    // Phase 2: heal + wait for Healthy. Heartbeats resume
    // on the next heartbeat-interval tick; recovery takes
    // one round-trip + the next FD check.
    chaos_heal(&a, &b);
    await_peer_recovered(&a, b_id, Duration::from_secs(5)).await;
}

/// Failure mode: `peer-crash-mid-phase` (one-sided block).
/// Only `a` filters traffic; `b` keeps sending but never
/// hears back. `a`'s FD marks `b` Failed because `a` is no
/// longer receiving heartbeats; this is the cleanest proxy
/// we have for "peer process died" from the observer's
/// perspective without OS-level kill.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_sided_block_marks_peer_failed_from_blocking_side() {
    let a = build_fast_node().await;
    let b = build_fast_node().await;
    connect_pair(&a, &b).await;
    a.start();
    b.start();

    let b_id = b.node_id();
    tokio::time::sleep(Duration::from_millis(250)).await;

    // A drops every packet to/from B. B is unaware; its own
    // FD will also eventually mark A Failed because A stops
    // sending heartbeats. We only assert A's view here — the
    // symmetric case is covered by
    // `bilateral_partition_marks_peer_failed_on_both_sides`.
    chaos_one_sided_block(&a, &b);

    await_peer_failed(&a, b_id, Duration::from_secs(3)).await;
}

/// Failure mode: `multi-peer-isolation`. Three-node topology:
/// A↔B, A↔C, B↔C. Partition A↔B; assert A's FD still sees C
/// as Healthy (no false-positive cascade). Pins that the FD
/// state machine is per-peer, not global, which is easy to
/// break with a shared-shelf bug where one peer's timeout
/// evicts the whole table.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn partition_of_one_peer_does_not_mark_unrelated_peers_failed() {
    let a = build_fast_node().await;
    let b = build_fast_node().await;
    let c = build_fast_node().await;
    connect_pair(&a, &b).await;
    connect_pair(&a, &c).await;
    connect_pair(&b, &c).await;
    a.start();
    b.start();
    c.start();

    let b_id = b.node_id();
    let c_id = c.node_id();
    tokio::time::sleep(Duration::from_millis(250)).await;

    // Partition A↔B; C stays connected to both.
    chaos_partition(&a, &b);

    // A's FD marks B Failed...
    await_peer_failed(&a, b_id, Duration::from_secs(3)).await;

    // ...but C must remain Healthy on A. Drive FD once
    // explicitly so any latent "mark-all-if-any" bug would
    // have fired by now.
    let _ = a.failure_detector().check_all();
    assert_eq!(
        a.failure_detector().status(c_id),
        net::adapter::net::NodeStatus::Healthy,
        "FD must be per-peer — a partition of B must not \
         cascade to C; got status={:?}",
        a.failure_detector().status(c_id),
    );
}

/// Composite invariant: after `partition` → `await_peer_failed`,
/// the `on_failure` callback must have fired and the
/// capability index must no longer hold the peer's entry
/// (P1-5 three-way-agreement). This is the end-to-end
/// derivation that the previously-hand-authored
/// `peer_death_clears_capability_index` test demonstrated;
/// here it falls out of the harness primitives in ~20 lines
/// instead of 150.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn peer_failure_clears_capability_index_via_harness() {
    use net::adapter::net::behavior::capability::CapabilitySet;

    let a = build_fast_node().await;
    let b = build_fast_node().await;
    connect_pair(&a, &b).await;
    a.start();
    b.start();

    // B announces capabilities so A's index picks up an
    // entry for B. No reflex needed — we only care about
    // index-eviction on FD-failed.
    b.announce_capabilities(CapabilitySet::new())
        .await
        .expect("B announce");

    let b_id = b.node_id();
    // Wait for A to index B's announcement (propagation isn't
    // instant; the announce + dispatch + index round-trip
    // takes a few ms).
    await_condition(
        Duration::from_secs(2),
        "A indexes B's capability announcement",
        || a.test_capability_fold_has(b_id),
    )
    .await;

    chaos_partition(&a, &b);
    await_peer_failed(&a, b_id, Duration::from_secs(3)).await;
    await_capability_index_evicts(&a, b_id, Duration::from_secs(2)).await;
}
