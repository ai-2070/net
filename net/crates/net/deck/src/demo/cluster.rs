//! Thin wrapper that builds the `net_sdk::testing::ClusterHarness`
//! the demo runs on top of. Keeps the node-count + boot
//! configuration in one place so spawn.rs reads top-down.

use net_sdk::testing::{ClusterError, ClusterHarness};

/// Hardcoded per `DECK_DEMO_PLAN.md` locked decisions: 5 nodes,
/// enough room for the replica trio + fork trio + standby
/// triad without contention.
pub const DEMO_NODE_COUNT: usize = 5;

/// Build the underlying multi-node harness. Returns once every
/// `Mesh` has handshook with every other Mesh and every
/// `MeshOsRuntime`'s `snapshot.peers` has folded `N - 1` peers
/// via the bridge probes.
pub async fn build_cluster() -> Result<ClusterHarness, ClusterError> {
    ClusterHarness::new(DEMO_NODE_COUNT).await
}
