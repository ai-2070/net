//! Smoke tests for `net_sdk::testing::ClusterHarness`'s daemon
//! supervisor methods — `spawn_per_node` + `spawn_where` (Item A
//! of `DECK_DEMO_HARNESS_PLAN.md`).
//!
//! Each test boots a small cluster, registers a `BareDaemon`
//! across some subset of nodes, asserts the resulting handles
//! line up with the expected (node_index, node_id) pairs, then
//! cleans up.

#![cfg(feature = "testing")]

use std::sync::atomic::{AtomicUsize, Ordering};

use bytes::Bytes;
use net_sdk::capabilities::CapabilityFilter;
use net_sdk::compute::CausalEvent;
// `net_sdk::compute::DaemonError` re-exports under the alias
// `ComputeDaemonError` from the crate root — distinct symbol
// from the `DaemonError` the substrate's `MeshDaemon` trait
// returns. Use the one re-exported off `meshos` to match the
// trait's `process` signature exactly.
use net_sdk::meshos::{DaemonError, MeshDaemon};
use net_sdk::testing::ClusterHarness;

/// Minimal `MeshDaemon` impl — mirrors `BareDaemon` from the
/// substrate's own tests. Stateless, no inbound processing.
struct BareDaemon;

impl MeshDaemon for BareDaemon {
    fn name(&self) -> &str {
        "bare"
    }
    fn requirements(&self) -> CapabilityFilter {
        CapabilityFilter::default()
    }
    fn process(&mut self, _event: &CausalEvent) -> Result<Vec<Bytes>, DaemonError> {
        Ok(Vec::new())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spawn_per_node_registers_on_every_node() {
    let harness = ClusterHarness::new(3).await.expect("3-node cluster boot");
    let handles = harness
        .spawn_per_node(|| BareDaemon)
        .await
        .expect("spawn_per_node");
    assert_eq!(handles.len(), 3);
    // Every node index 0..3 must appear exactly once.
    let mut seen: Vec<usize> = handles.iter().map(|h| h.node_index).collect();
    seen.sort_unstable();
    assert_eq!(seen, vec![0, 1, 2]);
    // Every daemon_id must be distinct (factory mints a fresh
    // keypair per spawn).
    let mut daemon_ids: Vec<u64> = handles.iter().map(|h| h.daemon_id).collect();
    daemon_ids.sort_unstable();
    daemon_ids.dedup();
    assert_eq!(daemon_ids.len(), 3, "daemon ids must be unique");
    // node_id on each handle must match the corresponding
    // node's id in the harness.
    for h in &handles {
        assert_eq!(h.node_id, harness.nth(h.node_index).node_id);
    }
    drop(handles);
    harness.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spawn_where_filters_subset() {
    let harness = ClusterHarness::new(4).await.expect("4-node cluster boot");
    // Pick nodes at index 0 and 2 only.
    let handles = harness
        .spawn_where(
            || BareDaemon,
            |node| {
                // The harness owns the node order; we filter by index
                // captured via node_id since the predicate doesn't get
                // the index directly.
                node.node_id == harness.nth(0).node_id || node.node_id == harness.nth(2).node_id
            },
        )
        .await
        .expect("spawn_where");
    assert_eq!(handles.len(), 2);
    let mut seen: Vec<usize> = handles.iter().map(|h| h.node_index).collect();
    seen.sort_unstable();
    assert_eq!(seen, vec![0, 2]);
    drop(handles);
    harness.shutdown().await.expect("shutdown");
}

/// Empty predicate => empty result, no error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawn_where_empty_subset_is_ok() {
    let harness = ClusterHarness::new(2).await.expect("2-node cluster boot");
    let handles = harness
        .spawn_where(|| BareDaemon, |_| false)
        .await
        .expect("spawn_where with empty subset");
    assert!(handles.is_empty());
    harness.shutdown().await.expect("shutdown");
}

/// Factory side effects fire exactly once per registered node.
/// Catches a refactoring bug where the closure could be called
/// 0 or 2+ times per spawn.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn factory_runs_once_per_spawn() {
    let harness = ClusterHarness::new(5).await.expect("5-node cluster boot");
    let counter = std::sync::Arc::new(AtomicUsize::new(0));
    let c2 = std::sync::Arc::clone(&counter);
    let handles = harness
        .spawn_per_node(move || {
            c2.fetch_add(1, Ordering::SeqCst);
            BareDaemon
        })
        .await
        .expect("spawn_per_node");
    assert_eq!(handles.len(), 5);
    assert_eq!(counter.load(Ordering::SeqCst), 5);
    drop(handles);
    harness.shutdown().await.expect("shutdown");
}

/// Handles outlive the harness's `nodes()` borrow — sanity
/// check that the returned `NodeDaemonHandle` set is usable for
/// the duration of the cluster's life, not just within the
/// spawn call's scope.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn handles_remain_usable_after_spawn_returns() {
    let harness = ClusterHarness::new(2).await.expect("2-node cluster boot");
    let handles = harness
        .spawn_per_node(|| BareDaemon)
        .await
        .expect("spawn_per_node");
    // Hand each handle a log publish to prove the underlying
    // SDK handle is live.
    for h in &handles {
        h.handle
            .publish_log(
                net_sdk::meshos::LogLevel::Info,
                format!("hello from node[{}]", h.node_index),
            )
            .expect("publish_log");
    }
    // Explicit graceful_shutdown on every handle (drains the
    // control channel before unregister).
    for h in handles {
        h.graceful_shutdown(std::time::Duration::from_millis(50))
            .await
            .expect("graceful_shutdown");
    }
    harness.shutdown().await.expect("shutdown");
}
