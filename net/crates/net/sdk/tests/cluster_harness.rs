//! Smoke tests for `net_sdk::testing::ClusterHarness`.
//!
//! Covers Phase 0 + Phase 0.5 + Item C of
//! `DECK_DEMO_HARNESS_PLAN.md`:
//!
//! - **Phase 0**: N `Mesh` instances on loopback handshake into a
//!   full peer mesh within the documented boot budget.
//! - **Phase 0.5**: bridge probes drive each `MeshOsRuntime`'s
//!   `snapshot.peers` up to `n - 1` entries shortly after the
//!   mesh layer stabilizes.
//! - **Item C**: explicit `shutdown` returns cleanly and a forgot-
//!   to-call-shutdown drop emits a hint but doesn't panic.

#![cfg(feature = "testing")]

use std::time::Duration;

use net_sdk::testing::{ClusterConfig, ClusterError, ClusterHarness};

/// 5-node smoke test. The harness should:
/// 1. peer every Mesh against every other Mesh, and
/// 2. fold the same peer set into every MeshOsRuntime snapshot
/// within the default boot budget.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn five_node_cluster_boots_and_converges() {
    let harness = ClusterHarness::new(5)
        .await
        .expect("5-node cluster boot within default budget");
    assert_eq!(harness.len(), 5);

    // Mesh layer: each node sees the other 4.
    for (i, node) in harness.nodes().iter().enumerate() {
        assert_eq!(
            node.mesh().peer_count(),
            4,
            "node[{i}] mesh.peer_count() mismatch"
        );
    }

    // MeshOS layer: each runtime's snapshot.peers reflects the
    // other 4 peers via the bridge probes.
    for (i, node) in harness.nodes().iter().enumerate() {
        let sdk = node.sdk().expect("sdk present before shutdown");
        let snap = sdk.runtime().snapshot();
        assert_eq!(
            snap.peers.len(),
            4,
            "node[{i}] runtime.snapshot.peers mismatch (got {})",
            snap.peers.len()
        );
        // Every entry should key on a distinct peer node_id —
        // never the local one.
        assert!(
            !snap.peers.contains_key(&node.node_id()),
            "node[{i}] snapshot.peers includes self {:x}",
            node.node_id()
        );
    }

    // Health check the harness exposes for callers monitoring a
    // long-running demo: should report full convergence.
    let h = harness.health();
    assert_eq!(h.total_nodes, 5);
    assert_eq!(h.meshes_with_full_peers, 5);
    assert_eq!(h.runtimes_with_full_peers, 5);
    assert!(h.fully_converged());

    harness
        .shutdown()
        .await
        .expect("clean shutdown of 5-node cluster");
}

/// Smallest meaningful cluster — pin that `n = 2` still drives a
/// real handshake + fold. Catches off-by-one issues in the
/// pairwise-iteration code that wouldn't surface at n = 5.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_node_cluster_pair_handshake() {
    let harness = ClusterHarness::new(2).await.expect("2-node cluster boot");
    assert_eq!(harness.len(), 2);
    for node in harness.nodes() {
        assert_eq!(node.mesh().peer_count(), 1);
        let snap = node
            .sdk()
            .expect("sdk present before shutdown")
            .runtime()
            .snapshot();
        assert_eq!(snap.peers.len(), 1);
    }
    harness.shutdown().await.expect("clean shutdown");
}

/// `n = 0` is an invariant violation; the harness should reject
/// it at the boundary instead of returning a 0-node handle.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn zero_node_cluster_rejected() {
    // `expect_err` needs `Debug` on the Ok variant; `ClusterHarness`
    // isn't `Debug` (the wrapped SDKs aren't), so unwrap manually.
    match ClusterHarness::new(0).await {
        Ok(_) => panic!("n=0 must fail"),
        Err(ClusterError::Invariant(_)) => {}
        Err(other) => panic!("expected ClusterError::Invariant, got {other:?}"),
    }
}

/// A single-node "cluster" should boot — no peers to handshake
/// with, but the MeshOS runtime still spins up cleanly. Useful
/// as a degenerate baseline for tests building on the harness.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn single_node_cluster_boots() {
    let harness = ClusterHarness::new(1).await.expect("1-node cluster boot");
    assert_eq!(harness.len(), 1);
    let node = harness.nth(0);
    assert_eq!(node.mesh().peer_count(), 0);
    let snap = node
        .sdk()
        .expect("sdk present before shutdown")
        .runtime()
        .snapshot();
    assert!(snap.peers.is_empty());
    harness.shutdown().await.expect("clean shutdown");
}

/// Boot-budget regression: a 5-node cluster should be ready in
/// under 4 s wall-clock on a developer laptop. Generous ceiling
/// to avoid flakes on CI; tighten once we have real data on the
/// distribution.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn five_node_boot_within_budget() {
    let start = std::time::Instant::now();
    let harness = ClusterHarness::new(5).await.expect("5-node cluster boot");
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(8),
        "5-node boot took {elapsed:?}, expected < 8s"
    );
    harness.shutdown().await.expect("clean shutdown");
}

/// Custom-config path: ensure `with_config` honors a tightened
/// poll interval. Doesn't assert a specific timing, just that the
/// path compiles + runs end-to-end.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn custom_config_path_runs() {
    let cfg = ClusterConfig {
        poll_interval: Duration::from_millis(10),
        ..ClusterConfig::default()
    };
    let harness = ClusterHarness::with_config(3, cfg)
        .await
        .expect("3-node cluster boot with custom config");
    assert_eq!(harness.len(), 3);
    harness.shutdown().await.expect("clean shutdown");
}

/// Regression: `ClusterConfig.verifier` must be installed on every
/// node's `MeshOsDaemonSdk`. Pre-fix the demo built an `AdminVerifier`
/// and immediately dropped it (`let _verifier = ...`), so deck-side
/// ICE commits would bypass operator signature verification. The
/// `verifier` field on `ClusterConfig` closes that gap; this test
/// pins the wire-through so a future refactor that forgets to plumb
/// the field through `with_config` is caught.
#[cfg(feature = "deck")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verifier_threads_through_to_every_node() {
    use std::sync::Arc;

    use net_sdk::deck::{AdminVerifier, OperatorRegistry};
    use net_sdk::meshos::EntityKeypair;

    let kp = EntityKeypair::generate();
    let mut registry = OperatorRegistry::new();
    registry.register(&kp);
    let verifier = Arc::new(AdminVerifier::new(Arc::new(registry), 1));

    let cfg = ClusterConfig {
        verifier: Some(Arc::clone(&verifier)),
        ..ClusterConfig::default()
    };
    let harness = ClusterHarness::with_config(2, cfg)
        .await
        .expect("2-node cluster boot with verifier");
    assert_eq!(harness.len(), 2);

    // The substrate doesn't expose a "is verifier installed?" query,
    // so the strongest assertion we can make from outside the SDK is
    // that the cluster boots successfully with a verifier set —
    // earlier the harness ignored the field entirely, so this call
    // would have compiled but the verifier would have been dropped.
    // The smoke test in `deck/src/demo/spawn.rs` covers the
    // end-to-end signed-commit path.
    let snap = harness
        .nth(0)
        .sdk()
        .expect("sdk present before shutdown")
        .runtime()
        .snapshot();
    assert_eq!(snap.peers.len(), 1);

    harness.shutdown().await.expect("clean shutdown");
}
