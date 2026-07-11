//! Shared chaos-harness utilities for failure-injection
//! integration tests. Supports FAILURE_PATH_HARDENING_PLAN
//! Stage 3 — the (subprotocol × phase × failure-mode) matrix.
//!
//! Five hand-authored failure-injection tests (`peer_death_*`,
//! `migration_target_failure_*`, `rendezvous_coordinator`'s
//! staleness case, etc.) each reimplement their own polling
//! loops, config builders, and setup primitives. This module
//! centralizes the patterns so new matrix cells are cheap to
//! add and the assertions are uniform: one consistent
//! `await_*` / `chaos_*` / `drive_*` vocabulary across every
//! failure-injection test.
//!
//! # Usage
//!
//! Each integration test file under `tests/` that wants the
//! harness adds:
//!
//! ```ignore
//! mod common;
//! use common::*;
//! ```
//!
//! Cargo's integration-test model treats subdirectories under
//! `tests/` as shared modules rather than separate test
//! binaries, so `tests/common/mod.rs` is compiled into each
//! test that `mod common;`-imports it but not run as its own
//! test.
//!
//! # What's here
//!
//! - **Setup**: [`fast_fd_config`], [`build_fast_node`],
//!   [`connect_pair`] — the standard 100 ms / 500 ms
//!   heartbeat / session-timeout recipe that the existing
//!   failure-injection tests all use, extracted once.
//! - **Polling**: [`await_condition`], [`await_peer_failed`],
//!   [`await_peer_recovered`], [`await_capability_index_evicts`],
//!   [`await_peer_count`].
//! - **Chaos injection**: [`chaos_partition`], [`chaos_heal`],
//!   [`drive_failure_detection`].
//!
//! # What's NOT here (Stage 3 blockers)
//!
//! The full Stage-3 matrix calls for `wire-packet-delay`,
//! `wire-packet-reorder`, `wire-packet-duplicate`, and
//! `clock-jump-*` failure modes. None are implementable with
//! the crate's current public API — they need either a
//! dispatch-layer interception hook or a mock-time substitution.
//! See `docs/FAILURE_PATH_HARDENING_PLAN.md` §Stage 3 for the
//! blocker discussion. This harness covers
//! `peer-crash-mid-phase`, `partition-split`,
//! `partition-heal-mid-phase`, and `slow-heartbeat` — the
//! cells that are reachable today.

#![allow(dead_code)]
#![cfg(feature = "net")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, NodeStatus, SocketBufferConfig};

/// Default socket buffer for all chaos-harness tests. 256 KiB
/// is comfortably above the OS defaults on Linux + macOS so
/// tests don't silently drop packets under the chaos load.
pub const CHAOS_BUFFER_SIZE: usize = 256 * 1024;

/// Shared PSK across every chaos-harness node. Identical to
/// the inline constant used in the pre-harness tests.
pub const CHAOS_PSK: [u8; 32] = [0x42u8; 32];

/// Build a `MeshNodeConfig` tuned for fast failure detection:
/// heartbeat every 100 ms, session timeout 500 ms → peer
/// transitions to Failed after ~1.5 s of silence (3×
/// session_timeout, per [`FailureDetectorConfig`]'s miss
/// threshold of 3).
///
/// The existing `peer_death_clears_capability_index`,
/// `peer_death_evicts_peer_map`, and
/// `migration_target_failure_mid_chunking` tests all
/// independently picked the same 100/500 recipe. Centralized
/// here so any tuning decision (e.g., CI running slow and
/// needing 200/1000) happens once.
pub fn fast_fd_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, CHAOS_PSK)
        .with_heartbeat_interval(Duration::from_millis(100))
        .with_session_timeout(Duration::from_millis(500))
        .with_handshake(3, Duration::from_secs(2));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: CHAOS_BUFFER_SIZE,
        recv_buffer_size: CHAOS_BUFFER_SIZE,
    };
    cfg
}

/// Build a node with [`fast_fd_config`]. Use [`build_node_with`]
/// for custom configs.
pub async fn build_fast_node() -> Arc<MeshNode> {
    build_node_with(fast_fd_config()).await
}

/// Build a node against a caller-supplied config.
pub async fn build_node_with(cfg: MeshNodeConfig) -> Arc<MeshNode> {
    Arc::new(
        MeshNode::new(EntityKeypair::generate(), cfg)
            .await
            .expect("MeshNode::new"),
    )
}

/// Connect A→B via the handshake + accept pattern every
/// failure-injection test uses. Leaves both nodes started.
pub async fn connect_pair(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    let a_id = a.node_id();
    let b_pub = *b.public_key();
    let b_addr = b.local_addr();
    let b_id = b.node_id();
    let b_clone = b.clone();
    let accept = tokio::spawn(async move { b_clone.accept(a_id).await });
    a.connect(b_addr, &b_pub, b_id)
        .await
        .expect("connect failed");
    accept
        .await
        .expect("accept task panicked")
        .expect("accept failed");
}

// ─────────────────────────────────────────────────────────────
// Polling helpers
// ─────────────────────────────────────────────────────────────

/// Poll `check` until it returns `true` or `limit` elapses.
/// On timeout, panics with a message including `description`
/// so the test failure points at the specific invariant that
/// didn't converge rather than a generic "assertion failed."
///
/// 50 ms poll interval is the same cadence the pre-harness
/// tests used; tight enough that FD transitions are caught
/// promptly, loose enough that CPU cost is negligible.
pub async fn await_condition<F: FnMut() -> bool>(limit: Duration, description: &str, mut check: F) {
    let start = tokio::time::Instant::now();
    while start.elapsed() < limit {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // Final check after the loop exits — a cooperative
    // scheduler might have held the tokio runtime so long
    // that the condition became true between the last poll
    // and the timeout. One more chance before we panic.
    if !check() {
        panic!("await_condition({description:?}) did not hold within {limit:?}",);
    }
}

/// Bool-returning variant of [`await_condition`]: poll `check` every
/// 25 ms up to `limit`, returning whether it ever held. Use this when
/// the caller wants to branch on the outcome — e.g. print diagnostics
/// before asserting, or assert the *negative* (a route that must NOT
/// appear). [`await_condition`] panics on timeout; this reports.
pub async fn poll_until<F: FnMut() -> bool>(limit: Duration, mut check: F) -> bool {
    let start = tokio::time::Instant::now();
    while start.elapsed() < limit {
        if check() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    check()
}

/// Wait for `observer.failure_detector()` to mark `target_id`
/// as `NodeStatus::Failed`. Drives the detector on each poll
/// iteration (via `check_all()`) because production-config
/// `check_all` runs on a heartbeat cadence that tests shouldn't
/// rely on firing within the window.
pub async fn await_peer_failed(observer: &Arc<MeshNode>, target_id: u64, limit: Duration) {
    let observer = observer.clone();
    await_condition(limit, &format!("peer {target_id:#x} marked Failed"), || {
        let _ = observer.failure_detector().check_all();
        observer.failure_detector().status(target_id) == NodeStatus::Failed
    })
    .await;
}

/// Wait for `observer.failure_detector()` to mark `target_id`
/// as `NodeStatus::Healthy`. Used after partition heal to pin
/// the recovery path.
pub async fn await_peer_recovered(observer: &Arc<MeshNode>, target_id: u64, limit: Duration) {
    let observer = observer.clone();
    await_condition(
        limit,
        &format!("peer {target_id:#x} marked Healthy"),
        || observer.failure_detector().status(target_id) == NodeStatus::Healthy,
    )
    .await;
}

/// Wait until `observer`'s capability index no longer has an
/// entry for `target_id`. This is the P1-5 three-way-agreement
/// invariant — a peer the FD has failed must be evicted from
/// every derived map.
pub async fn await_capability_index_evicts(
    observer: &Arc<MeshNode>,
    target_id: u64,
    limit: Duration,
) {
    let observer = observer.clone();
    await_condition(
        limit,
        &format!("capability_index evicts {target_id:#x}"),
        || !observer.test_capability_fold_has(target_id),
    )
    .await;
}

/// Wait until `observer.peer_count()` reaches `expected`.
/// Useful for teardown assertions ("after failure, the peers
/// map should have shrunk by 1").
pub async fn await_peer_count(observer: &Arc<MeshNode>, expected: usize, limit: Duration) {
    let observer = observer.clone();
    await_condition(limit, &format!("peer_count == {expected}"), || {
        observer.peer_count() == expected
    })
    .await;
}

// ─────────────────────────────────────────────────────────────
// Chaos injection
// ─────────────────────────────────────────────────────────────

/// Full bilateral partition: `a` and `b` each drop every
/// packet from the other. Neither node sees the other's
/// heartbeats; both FD's will eventually mark the other
/// Failed. Symmetric by construction — use this for the
/// "peer crashed hard" pattern.
pub fn chaos_partition(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    a.block_peer(b.local_addr());
    b.block_peer(a.local_addr());
}

/// Undo [`chaos_partition`]. Both sides must unblock — a
/// one-sided unblock leaves the partition up from the
/// still-blocking side. Heartbeats resume on the next
/// heartbeat-interval tick.
pub fn chaos_heal(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    a.unblock_peer(&b.local_addr());
    b.unblock_peer(&a.local_addr());
}

/// One-sided block: `observer` drops every packet to/from
/// `target`, but `target` is unaware and keeps sending.
/// Effectively a full partition from `observer`'s perspective;
/// `target`'s FD will also eventually mark `observer` Failed
/// because `observer` stops sending heartbeats (it's filtering
/// its own outbound traffic too).
pub fn chaos_one_sided_block(observer: &Arc<MeshNode>, target: &Arc<MeshNode>) {
    observer.block_peer(target.local_addr());
}

pub fn chaos_one_sided_heal(observer: &Arc<MeshNode>, target: &Arc<MeshNode>) {
    observer.unblock_peer(&target.local_addr());
}

/// Wait past the failure threshold, then explicitly drive the
/// detector. Use after [`chaos_partition`] when the test
/// expects the peer to be marked Failed. Returns the list of
/// newly-failed node IDs from the explicit `check_all()`.
///
/// `wait` should be `>= miss_threshold × session_timeout` —
/// with [`fast_fd_config`] that's 3 × 500 ms = 1.5 s, so
/// 2 s gives a comfortable margin.
pub async fn drive_failure_detection(observer: &Arc<MeshNode>, wait: Duration) -> Vec<u64> {
    tokio::time::sleep(wait).await;
    observer.failure_detector().check_all()
}
