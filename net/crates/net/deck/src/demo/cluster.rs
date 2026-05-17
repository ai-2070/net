//! Thin wrapper that builds the `net_sdk::testing::ClusterHarness`
//! the demo runs on top of. Keeps the node-count + boot
//! configuration in one place so spawn.rs reads top-down.

use std::sync::Arc;

use net_sdk::deck::AdminVerifier;
use net_sdk::testing::{ClusterConfig, ClusterError, ClusterHarness};

/// 9 nodes — gives the NET.MAP a real cluster shape and lets
/// the replica trio / fork trio / standby triad each land on
/// distinct subsets of nodes with breathing room. Picked over
/// the original 5-node baseline because 5 reads as "toy" to
/// non-engineer viewers; 9 reads as "real cluster." Boot stays
/// well under the harness's 10 s ceiling on a developer
/// laptop (pairwise handshake = N²/2 = 36 pairs at ~50 ms
/// each ≈ 2 s).
pub const DEMO_NODE_COUNT: usize = 9;

/// Build the underlying multi-node harness, installing
/// `verifier` on every node's `MeshOsDaemonSdk` so ICE / admin
/// commits from the deck go through the real signed-commit
/// path. Returns once every `Mesh` has handshook with every
/// other Mesh and every `MeshOsRuntime`'s `snapshot.peers` has
/// folded `N - 1` peers via the bridge probes.
pub async fn build_cluster(
    verifier: Arc<AdminVerifier>,
) -> Result<ClusterHarness, ClusterError> {
    let cfg = ClusterConfig {
        verifier: Some(verifier),
        ..ClusterConfig::default()
    };
    ClusterHarness::with_config(DEMO_NODE_COUNT, cfg).await
}
