//! [`AggregatorGroup`] — N interchangeable `AggregatorDaemon`
//! replicas managed as a unit via [`LifecycleHandle`].
//!
//! Phase B slice 4 of `SCALING_SUBNET_SPEC.md`. Parallel to
//! [`ReplicaGroup`](crate::adapter::net::compute::replica_group::ReplicaGroup)
//! for sync [`MeshDaemon`](crate::adapter::net::compute::MeshDaemon)s,
//! but built on the async [`LifecycleDaemon`] sibling trait so
//! aggregator lifetimes (`mesh.publish().await`, tokio
//! intervals) can run without contorting the sync MeshDaemon
//! contract.
//!
//! # Shape
//!
//! - [`AggregatorGroup::spawn`] — accept a `replica_count`, a
//!   32-byte `group_seed`, and a factory that produces an
//!   `Arc<AggregatorDaemon>` per replica index. Each daemon is
//!   wrapped in a [`LifecycleHandle`], which runs its
//!   `on_start` synchronously. Errors abort the group; any
//!   handles already started in the loop are dropped (their
//!   Drop fires a best-effort `on_stop` on a detached task).
//! - [`AggregatorGroup::stop`] — `stop()` each handle in order
//!   and await the teardown.
//! - [`AggregatorGroup::replica_keypair`] — derive the
//!   deterministic per-replica `EntityKeypair` via
//!   [`derive_replica_keypair`]. The group itself doesn't
//!   currently install these into each `MeshNode` (single-mesh
//!   deployments share one identity); the accessor exists so
//!   future cross-node placement can read the same derivation
//!   the [`ReplicaGroup`](crate::adapter::net::compute::replica_group::ReplicaGroup) uses.
//!
//! # What this is NOT (yet)
//!
//! - No distributed placement. Single-process; the caller
//!   owns the `MeshNode`(s) and threads them into the factory.
//! - No load-balanced routing. Aggregators publish summaries
//!   on visibility-scoped channels — subscribers receive from
//!   *any* live aggregator naturally via the existing channel
//!   plumbing.
//! - No auto-replacement / failure detection. A handle's
//!   background loop stops only on shutdown.

use std::sync::Arc;

use super::daemon::AggregatorDaemon;
use super::lifecycle::{LifecycleDaemon, LifecycleError, LifecycleHandle};
use crate::adapter::net::compute::replica_group::derive_replica_keypair;
use crate::adapter::net::identity::EntityKeypair;

/// N interchangeable `AggregatorDaemon` replicas with a shared
/// `group_seed` for deterministic identity derivation.
pub struct AggregatorGroup {
    handles: Vec<LifecycleHandle>,
    group_seed: [u8; 32],
}

impl AggregatorGroup {
    /// Spawn `replica_count` aggregator replicas. The factory
    /// is called once per index `0..replica_count` and should
    /// return a fully configured `Arc<AggregatorDaemon>` — the
    /// group takes care of wrapping each in a [`LifecycleHandle`]
    /// (which calls `on_start` synchronously).
    ///
    /// `replica_count == 0` is rejected as a config error.
    pub async fn spawn<F>(
        replica_count: u8,
        group_seed: [u8; 32],
        mut factory: F,
    ) -> Result<Self, LifecycleError>
    where
        F: FnMut(u8) -> Arc<AggregatorDaemon>,
    {
        if replica_count == 0 {
            return Err(LifecycleError::StartFailed(
                "replica_count must be > 0".into(),
            ));
        }
        // Build the per-replica daemons synchronously (factory is
        // sync — preserving the existing ordering invariant the
        // tests pin), then drive each `on_start` in parallel via
        // `try_join_all`. If any start fails, every other in-flight
        // start cancels and the partially-started replicas drop
        // their LifecycleHandles cleanly via Drop.
        let starts: Vec<_> = (0..replica_count)
            .map(|index| {
                let daemon: Arc<dyn LifecycleDaemon> = factory(index);
                LifecycleHandle::start(daemon)
            })
            .collect();
        let handles = futures::future::try_join_all(starts).await?;
        Ok(Self {
            handles,
            group_seed,
        })
    }

    /// Number of live replicas managed by the group.
    pub fn replica_count(&self) -> usize {
        self.handles.len()
    }

    /// The 32-byte seed used to derive per-replica identities.
    pub fn group_seed(&self) -> &[u8; 32] {
        &self.group_seed
    }

    /// Derive the deterministic per-replica keypair for `index`.
    /// Same derivation [`ReplicaGroup`](crate::adapter::net::compute::replica_group::ReplicaGroup)
    /// uses for sync MeshDaemon replicas — so a future
    /// cross-node aggregator deployment can reuse this id.
    pub fn replica_keypair(&self, index: u8) -> EntityKeypair {
        derive_replica_keypair(&self.group_seed, index)
    }

    /// Borrow the underlying lifecycle handles. Operator
    /// tooling that wants to introspect per-replica state
    /// reaches in here.
    pub fn handles(&self) -> &[LifecycleHandle] {
        &self.handles
    }

    /// Stop every replica in declaration order and await the
    /// teardown. Consumes the group.
    pub async fn stop(self) {
        for h in self.handles {
            h.stop().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::aggregator::AggregatorConfig;
    use crate::adapter::net::behavior::fold::capability::CapabilityFold;
    use crate::adapter::net::behavior::fold::FoldKind;
    use crate::adapter::net::identity::EntityKeypair;
    use crate::adapter::net::{MeshNode, MeshNodeConfig, SubnetId};
    use std::net::SocketAddr;
    use std::time::Duration;

    async fn build_mesh() -> Arc<MeshNode> {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let cfg = MeshNodeConfig::new(addr, [0x17u8; 32]);
        Arc::new(
            MeshNode::new(EntityKeypair::generate(), cfg)
                .await
                .expect("MeshNode::new"),
        )
    }

    #[tokio::test]
    async fn spawn_zero_replicas_is_rejected_as_config_error() {
        let result = AggregatorGroup::spawn(0, [0u8; 32], |_| {
            panic!("factory must not be called when replica_count == 0")
        })
        .await;
        match result {
            Err(LifecycleError::StartFailed(msg)) => {
                assert!(msg.contains("replica_count"), "msg was: {msg}");
            }
            Ok(_) => panic!("expected StartFailed, got Ok"),
        }
    }

    #[tokio::test]
    async fn spawn_three_replicas_runs_each_lifecycle_then_stops_all() {
        // Build a single mesh; factory clones the Arc per replica.
        let mesh = build_mesh().await;
        let agg_cfg = AggregatorConfig::new(SubnetId::GLOBAL)
            .with_fold_kind(CapabilityFold::KIND_ID)
            .with_interval(Duration::from_millis(20));
        let factory_cfg = agg_cfg.clone();
        let factory_mesh = mesh.clone();
        let replicas_seen = Arc::new(parking_lot::Mutex::new(Vec::<u8>::new()));
        let replicas_seen_for_factory = replicas_seen.clone();

        let daemons: Arc<parking_lot::Mutex<Vec<Arc<AggregatorDaemon>>>> =
            Arc::new(parking_lot::Mutex::new(Vec::new()));
        let daemons_for_factory = daemons.clone();

        let group = AggregatorGroup::spawn(3, [0xABu8; 32], move |idx| {
            replicas_seen_for_factory.lock().push(idx);
            let d = Arc::new(
                AggregatorDaemon::new(factory_cfg.clone(), factory_mesh.clone()).expect("new"),
            );
            daemons_for_factory.lock().push(d.clone());
            d
        })
        .await
        .expect("group spawn");

        assert_eq!(group.replica_count(), 3);
        assert_eq!(*replicas_seen.lock(), vec![0u8, 1, 2]);

        // Let the loops tick. Each daemon has its own loop, so
        // each generation counter should advance independently.
        tokio::time::sleep(Duration::from_millis(90)).await;
        for d in daemons.lock().iter() {
            assert!(
                d.generation() >= 1,
                "every replica should have ticked at least once"
            );
        }

        group.stop().await;
        // After stop, capture each generation and ensure no
        // further advance.
        let snapshot: Vec<u64> = daemons.lock().iter().map(|d| d.generation()).collect();
        tokio::time::sleep(Duration::from_millis(80)).await;
        for (d, snap) in daemons.lock().iter().zip(snapshot.iter()) {
            assert_eq!(
                d.generation(),
                *snap,
                "no further ticks may land after group.stop()"
            );
        }
    }

    #[tokio::test]
    async fn replica_keypair_is_deterministic_for_a_given_index() {
        // The group's keypair derivation must match
        // `derive_replica_keypair` exactly — operator tooling
        // and future cross-node placement rely on it.
        let mesh = build_mesh().await;
        let agg_cfg = AggregatorConfig::new(SubnetId::GLOBAL)
            .with_fold_kind(CapabilityFold::KIND_ID)
            .with_interval(Duration::from_millis(100));
        let mesh_for_factory = mesh.clone();
        let cfg_for_factory = agg_cfg.clone();
        let seed = [0x42u8; 32];

        let group = AggregatorGroup::spawn(2, seed, move |_idx| {
            Arc::new(
                AggregatorDaemon::new(cfg_for_factory.clone(), mesh_for_factory.clone())
                    .expect("new"),
            )
        })
        .await
        .expect("group spawn");

        let expected_kp_0 = derive_replica_keypair(&seed, 0);
        let expected_kp_1 = derive_replica_keypair(&seed, 1);
        assert_eq!(
            group.replica_keypair(0).entity_id(),
            expected_kp_0.entity_id()
        );
        assert_eq!(
            group.replica_keypair(1).entity_id(),
            expected_kp_1.entity_id()
        );
        // Two distinct indices produce distinct identities.
        assert_ne!(
            group.replica_keypair(0).entity_id(),
            group.replica_keypair(1).entity_id()
        );
        assert_eq!(group.group_seed(), &seed);

        group.stop().await;
    }
}
