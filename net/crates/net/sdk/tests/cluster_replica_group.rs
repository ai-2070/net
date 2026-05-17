//! Smoke tests for `net_sdk::testing::ClusterHarness::spawn_replica_group` —
//! Item B of `DECK_DEMO_HARNESS_PLAN.md`. Validates that a real
//! `ReplicaGroup` can be spawned through a node's
//! `DaemonRuntime`, that `Scheduler::place_with_spread` finds
//! candidates against the live (post-`announce_capabilities`)
//! capability index, and that the resulting group reports the
//! requested replica count.

#![cfg(feature = "testing")]

use bytes::Bytes;
use net_sdk::capabilities::CapabilityFilter;
use net_sdk::compute::{CausalEvent, DaemonHostConfig};
use net_sdk::groups::{GroupHealth, ReplicaGroupConfig};
use net_sdk::meshos::{DaemonError, MeshDaemon};
use net_sdk::testing::ClusterHarness;

// Compute-layer MeshDaemon — same shape as the substrate's own
// `NoopDaemon` test fixture (groups_surface.rs:28-40). Stateless,
// returns no outputs.
struct NoopDaemon;

impl MeshDaemon for NoopDaemon {
    fn name(&self) -> &str {
        "noop"
    }
    fn requirements(&self) -> CapabilityFilter {
        CapabilityFilter::default()
    }
    fn process(&mut self, _event: &CausalEvent) -> Result<Vec<Bytes>, DaemonError> {
        Ok(vec![])
    }
}

fn replica_config(n: u8, seed: u8) -> ReplicaGroupConfig {
    ReplicaGroupConfig {
        replica_count: n,
        group_seed: [seed; 32],
        lb_strategy: net_sdk::groups::common::Strategy::RoundRobin,
        host_config: DaemonHostConfig::default(),
    }
}

/// 3-node cluster + 3-replica group. After the harness boots,
/// every Mesh has announced an empty capability set, so each
/// node's CapabilityIndex carries an entry for every peer. The
/// scheduler's `place_with_spread` then has N candidates and
/// can place 3 replicas successfully.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn replica_group_spawns_against_live_placement() {
    let harness = ClusterHarness::new(3).await.expect("3-node cluster boot");
    let group = harness
        .spawn_replica_group(0, "noop", || NoopDaemon, replica_config(3, 0x11))
        .expect("spawn_replica_group");
    assert_eq!(group.replica_count(), 3);
    assert_eq!(group.healthy_count(), 3);
    assert_eq!(group.health(), GroupHealth::Healthy);
    drop(group);
    harness.shutdown().await.expect("shutdown");
}

/// Anchor-index out of range is an invariant violation; the
/// harness should reject it instead of silently picking a
/// different node.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replica_group_rejects_anchor_out_of_range() {
    let harness = ClusterHarness::new(2).await.expect("2-node cluster boot");
    let res = harness.spawn_replica_group(5, "noop", || NoopDaemon, replica_config(2, 0x22));
    assert!(res.is_err(), "spawn with out-of-range anchor must fail");
    harness.shutdown().await.expect("shutdown");
}

/// Distinct kinds on the same anchor coexist — the runtime
/// allows multiple factory registrations as long as kinds
/// don't collide.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_groups_with_distinct_kinds_coexist() {
    let harness = ClusterHarness::new(3).await.expect("3-node cluster boot");
    let g1 = harness
        .spawn_replica_group(0, "noop-a", || NoopDaemon, replica_config(2, 0x01))
        .expect("spawn group A");
    let g2 = harness
        .spawn_replica_group(0, "noop-b", || NoopDaemon, replica_config(2, 0x02))
        .expect("spawn group B");
    assert_eq!(g1.replica_count(), 2);
    assert_eq!(g2.replica_count(), 2);
    drop(g1);
    drop(g2);
    harness.shutdown().await.expect("shutdown");
}
