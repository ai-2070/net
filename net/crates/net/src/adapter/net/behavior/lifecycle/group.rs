//! [`LifecycleGroup`] — N interchangeable replicas of an
//! `L: LifecycleDaemon`, managed as a unit via
//! [`LifecycleHandle`]s and a shared deterministic identity
//! seed.
//!
//! Parallel to
//! [`ReplicaGroup`](crate::adapter::net::compute::replica_group::ReplicaGroup)
//! (which targets sync [`MeshDaemon`](crate::adapter::net::compute::MeshDaemon)s).
//! Direction B of `docs/plans/AGGREGATOR_LIFECYCLE_DEFERRED_2026_05_23.md`:
//! we keep `LifecycleDaemon` separate from `MeshDaemon` and
//! share the underlying placement / capability primitives
//! rather than the trait. Layered slices add:
//!
//! - Step 2 (this file later): per-daemon `requirements()` →
//!   `Scheduler::place_with_spread` integration for cross-node
//!   placement.
//! - Step 3 (`MeshNode::aggregator_registry`): process-level
//!   registry of live groups for operator CLI / Deck.
//! - Step 4 (this file later): per-replica health snapshot +
//!   auto-replace via factory respawn at the same index.
//!
//! # Shape
//!
//! - [`LifecycleGroup::spawn`] — accept a `replica_count`, a
//!   32-byte `group_seed`, and a factory that produces an
//!   `Arc<L>` per replica index. Each daemon is wrapped in a
//!   [`LifecycleHandle`] (which runs `on_start` synchronously).
//!   Errors surface per-replica via [`LifecycleGroupError`];
//!   partially-started handles drop cleanly via their RAII
//!   `Drop` impl.
//! - [`LifecycleGroup::stop`] — `stop()` each handle in order
//!   and await teardown.
//! - [`LifecycleGroup::replica_keypair`] — derive the
//!   deterministic per-replica `EntityKeypair` via
//!   [`derive_replica_keypair`]. The group itself doesn't
//!   currently install these into each `MeshNode` (single-mesh
//!   deployments share one identity); the accessor exists so
//!   future cross-node placement can read the same derivation
//!   `ReplicaGroup` uses.

use std::collections::HashSet;
use std::sync::Arc;

use super::daemon::{LifecycleDaemon, LifecycleError, LifecycleHandle};
use crate::adapter::net::behavior::capability::CapabilityFilter;
use crate::adapter::net::compute::group_coord::GroupCoordinator;
use crate::adapter::net::compute::replica_group::derive_replica_keypair;
use crate::adapter::net::compute::{PlacementDecision, Scheduler};
use crate::adapter::net::identity::EntityKeypair;

/// Group-spawn failure shape. Distinguishes config-time errors
/// (rejected on the caller side before any on_start fires) from
/// per-replica `on_start` failures (carry the failing index for
/// operator diagnosis) and from placement failures (no candidate
/// node satisfied the daemon's capability requirements + spread
/// constraint).
#[derive(Debug)]
pub enum LifecycleGroupError {
    /// `replica_count == 0` or other up-front validation
    /// rejected the spawn.
    InvalidConfig(String),
    /// A specific replica's `on_start` failed. The other
    /// already-started replicas drop cleanly via their handles'
    /// `Drop` impl when the partially-built group goes out of
    /// scope.
    StartFailed {
        /// Index of the replica whose `on_start` failed.
        index: u8,
        /// The underlying lifecycle error.
        error: LifecycleError,
    },
    /// `Scheduler::place_with_spread` could not find a candidate
    /// node satisfying the daemon's `CapabilityFilter` outside
    /// the already-used set (spread invariant).
    PlacementFailed {
        /// Index of the replica that could not be placed.
        index: u8,
        /// Operator-facing diagnostic string.
        reason: String,
    },
}

impl std::fmt::Display for LifecycleGroupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidConfig(msg) => write!(f, "invalid lifecycle group config: {msg}"),
            Self::StartFailed { index, error } => {
                write!(f, "replica {index} failed to start: {error}")
            }
            Self::PlacementFailed { index, reason } => {
                write!(f, "replica {index} placement failed: {reason}")
            }
        }
    }
}

impl std::error::Error for LifecycleGroupError {}

/// Per-replica context the factory receives during
/// [`LifecycleGroup::spawn_with_placement`]. Carries the
/// replica index + the scheduler's placement decision so
/// factories that need to bind a daemon to a specific node can.
/// Factories that don't care about placement can ignore the
/// `placement` field (or use the simpler
/// [`LifecycleGroup::spawn`] which doesn't run placement at
/// all).
#[derive(Debug, Clone)]
pub struct ReplicaContext {
    /// Replica index in `0..replica_count`.
    pub index: u8,
    /// Placement decision from the scheduler. `None` only on
    /// the placement-free [`LifecycleGroup::spawn`] path —
    /// always `Some` under `spawn_with_placement`.
    pub placement: Option<PlacementDecision>,
}

/// N interchangeable replicas of a single `LifecycleDaemon` type
/// with a shared `group_seed` for deterministic identity
/// derivation.
///
/// `L` is the concrete daemon type — generic so callers retain
/// typed access to each replica's state without dyn-erasure.
pub struct LifecycleGroup<L: LifecycleDaemon> {
    handles: Vec<LifecycleHandle>,
    /// Concrete-typed Arcs to each replica, in declaration
    /// order. Mirrors `handles` 1-to-1; lets callers read
    /// daemon state without going through the type-erased
    /// `LifecycleHandle::daemon()`.
    replicas: Vec<Arc<L>>,
    /// Per-replica placement decisions in declaration order.
    /// Populated by `spawn_with_placement`; left empty by the
    /// placement-free `spawn`. The accessor [`Self::placement`]
    /// returns `Some` only when this Vec is non-empty.
    placements: Vec<PlacementDecision>,
    group_seed: [u8; 32],
}

impl<L: LifecycleDaemon> LifecycleGroup<L> {
    /// Spawn `replica_count` replicas of `L`. The factory is
    /// called once per index `0..replica_count` and must return
    /// a fully-configured `Arc<L>` — the group wraps each in a
    /// [`LifecycleHandle`] (which runs `on_start` synchronously).
    ///
    /// Starts run **concurrently** via `try_join_all`. If any
    /// `on_start` fails, every other in-flight start cancels and
    /// the partially-started replicas drop their handles cleanly
    /// via `Drop` (which schedules `on_stop` on a detached
    /// task).
    pub async fn spawn<F>(
        replica_count: u8,
        group_seed: [u8; 32],
        mut factory: F,
    ) -> Result<Self, LifecycleGroupError>
    where
        F: FnMut(u8) -> Arc<L>,
    {
        if replica_count == 0 {
            return Err(LifecycleGroupError::InvalidConfig(
                "replica_count must be > 0".into(),
            ));
        }
        let (replicas, handles) =
            start_replicas(replica_count, |idx| factory(idx)).await?;
        Ok(Self {
            handles,
            replicas,
            placements: Vec::new(),
            group_seed,
        })
    }

    /// Spawn `replica_count` replicas with cross-node placement
    /// via [`Scheduler::place`] /
    /// [`GroupCoordinator::place_with_spread`].
    ///
    /// Differences from [`Self::spawn`]:
    /// - Caller supplies a `Scheduler` + a `CapabilityFilter`
    ///   the scheduler uses to find candidate nodes for each
    ///   replica.
    /// - Replicas are spread across distinct nodes (spread
    ///   invariant) — failing if fewer than `replica_count`
    ///   candidates match the filter.
    /// - The factory receives a [`ReplicaContext`] carrying the
    ///   placement decision so daemons that bind to a specific
    ///   node can read it.
    ///
    /// Daemon construction happens **after** placement so a
    /// factory can use `ctx.placement.node_id` to configure the
    /// daemon for its target node. The placement decision is
    /// recorded on the group for inspection.
    ///
    /// # Note on single-process semantics
    ///
    /// In a single-process deployment the scheduler may pick
    /// the local node for every replica — `place_with_spread`
    /// errors with `PlacementFailed` when fewer candidate nodes
    /// than replicas match the filter. The group does not
    /// actually move daemons across nodes; that is the
    /// substrate's remote-spawn responsibility, not the group
    /// helper's. Recording the placement decisions here lets a
    /// future cross-node integration consume them without
    /// re-deriving them.
    pub async fn spawn_with_placement<F>(
        replica_count: u8,
        group_seed: [u8; 32],
        requirements: CapabilityFilter,
        scheduler: &Scheduler,
        mut factory: F,
    ) -> Result<Self, LifecycleGroupError>
    where
        F: FnMut(ReplicaContext) -> Arc<L>,
    {
        if replica_count == 0 {
            return Err(LifecycleGroupError::InvalidConfig(
                "replica_count must be > 0".into(),
            ));
        }
        // Walk placements first so factory invocations see a
        // populated `ReplicaContext`. Spread invariant: each
        // placement excludes prior ones.
        let mut placements: Vec<PlacementDecision> =
            Vec::with_capacity(replica_count as usize);
        let mut used_nodes: HashSet<u64> = HashSet::new();
        for index in 0..replica_count {
            match GroupCoordinator::place_with_spread(scheduler, &requirements, &used_nodes) {
                Ok(decision) => {
                    used_nodes.insert(decision.node_id);
                    placements.push(decision);
                }
                Err(e) => {
                    return Err(LifecycleGroupError::PlacementFailed {
                        index,
                        reason: format!("{e}"),
                    });
                }
            }
        }
        let placements_for_factory = placements.clone();
        let (replicas, handles) = start_replicas(replica_count, move |index| {
            let ctx = ReplicaContext {
                index,
                placement: Some(placements_for_factory[index as usize].clone()),
            };
            factory(ctx)
        })
        .await?;
        Ok(Self {
            handles,
            replicas,
            placements,
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
    /// Same derivation
    /// [`ReplicaGroup`](crate::adapter::net::compute::replica_group::ReplicaGroup)
    /// uses for sync MeshDaemon replicas — so a future
    /// cross-node lifecycle-daemon deployment can reuse this id.
    pub fn replica_keypair(&self, index: u8) -> EntityKeypair {
        derive_replica_keypair(&self.group_seed, index)
    }

    /// Concrete, typed access to each replica's daemon. Mirrors
    /// `replicas[index].clone()` — preserves the underlying
    /// `L`'s state surface so callers don't have to downcast
    /// from a trait object.
    pub fn replica(&self, index: usize) -> Option<Arc<L>> {
        self.replicas.get(index).cloned()
    }

    /// All replicas in declaration order. Cheap O(n) Arc clones.
    pub fn replicas(&self) -> Vec<Arc<L>> {
        self.replicas.clone()
    }

    /// Placement decision recorded for `index`, or `None` when
    /// the group was created via the placement-free
    /// [`Self::spawn`].
    pub fn placement(&self, index: usize) -> Option<&PlacementDecision> {
        self.placements.get(index)
    }

    /// All recorded placement decisions in declaration order.
    /// Empty when the group was created via the placement-free
    /// [`Self::spawn`].
    pub fn placements(&self) -> &[PlacementDecision] {
        &self.placements
    }

    /// Borrow the underlying lifecycle handles. Operator
    /// tooling that wants type-erased access (e.g. iterating
    /// `daemon().name()` across heterogeneous groups in a
    /// future registry) reaches in here.
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

/// Shared spawn helper: invoke `factory(index)` for each
/// replica, wrap each in a `LifecycleHandle` concurrently via
/// `join_all`, and return the parallel `(replicas, handles)`
/// Vecs in declaration order. Partial failure drops the
/// already-collected Arcs cleanly — each handle's RAII Drop
/// schedules `on_stop`.
async fn start_replicas<L, F>(
    replica_count: u8,
    mut factory: F,
) -> Result<(Vec<Arc<L>>, Vec<LifecycleHandle>), LifecycleGroupError>
where
    L: LifecycleDaemon,
    F: FnMut(u8) -> Arc<L>,
{
    let mut replicas: Vec<Arc<L>> = Vec::with_capacity(replica_count as usize);
    let mut starts = Vec::with_capacity(replica_count as usize);
    for index in 0..replica_count {
        let daemon = factory(index);
        replicas.push(daemon.clone());
        let trait_obj: Arc<dyn LifecycleDaemon> = daemon;
        starts.push((index, LifecycleHandle::start(trait_obj)));
    }
    let started: Vec<_> = futures::future::join_all(
        starts.into_iter().map(|(i, fut)| async move { (i, fut.await) }),
    )
    .await;
    let mut handles = Vec::with_capacity(replica_count as usize);
    for (index, result) in started {
        match result {
            Ok(h) => handles.push(h),
            Err(error) => {
                drop(handles);
                drop(replicas);
                return Err(LifecycleGroupError::StartFailed { index, error });
            }
        }
    }
    Ok((replicas, handles))
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    /// Bare-minimum LifecycleDaemon for group testing — no
    /// background work, just bumps a counter on each lifecycle
    /// callback so tests can pin start/stop semantics without
    /// pulling in the aggregator stack.
    struct CountingDaemon {
        starts: AtomicU64,
        stops: AtomicU64,
        fail_start: AtomicBool,
    }

    impl CountingDaemon {
        fn new() -> Self {
            Self {
                starts: AtomicU64::new(0),
                stops: AtomicU64::new(0),
                fail_start: AtomicBool::new(false),
            }
        }
    }

    #[async_trait]
    impl LifecycleDaemon for CountingDaemon {
        fn name(&self) -> &str {
            "counting"
        }
        async fn on_start(self: Arc<Self>) -> Result<(), LifecycleError> {
            if self.fail_start.load(Ordering::Acquire) {
                return Err(LifecycleError::StartFailed("intentional".into()));
            }
            self.starts.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }
        async fn on_stop(&self) {
            self.stops.fetch_add(1, Ordering::AcqRel);
        }
    }

    #[tokio::test]
    async fn spawn_zero_replicas_is_rejected_as_config_error() {
        let result = LifecycleGroup::<CountingDaemon>::spawn(0, [0u8; 32], |_| {
            panic!("factory must not be called when replica_count == 0")
        })
        .await;
        match result {
            Err(LifecycleGroupError::InvalidConfig(msg)) => {
                assert!(msg.contains("replica_count"), "msg was: {msg}");
            }
            Err(other) => panic!("expected InvalidConfig, got {other:?}"),
            Ok(_) => panic!("expected InvalidConfig, got Ok"),
        }
    }

    #[tokio::test]
    async fn spawn_three_replicas_runs_each_lifecycle_then_stops_all() {
        let factory_calls = Arc::new(parking_lot::Mutex::new(Vec::<u8>::new()));
        let factory_calls_clone = factory_calls.clone();
        let daemons: Arc<parking_lot::Mutex<Vec<Arc<CountingDaemon>>>> =
            Arc::new(parking_lot::Mutex::new(Vec::new()));
        let daemons_clone = daemons.clone();

        let group = LifecycleGroup::<CountingDaemon>::spawn(3, [0xABu8; 32], move |idx| {
            factory_calls_clone.lock().push(idx);
            let d = Arc::new(CountingDaemon::new());
            daemons_clone.lock().push(d.clone());
            d
        })
        .await
        .expect("group spawn");

        assert_eq!(group.replica_count(), 3);
        assert_eq!(*factory_calls.lock(), vec![0u8, 1, 2]);
        for d in daemons.lock().iter() {
            assert_eq!(d.starts.load(Ordering::Acquire), 1);
            assert_eq!(d.stops.load(Ordering::Acquire), 0);
        }

        // Typed access to each replica.
        let r0 = group.replica(0).expect("replica 0");
        assert_eq!(r0.starts.load(Ordering::Acquire), 1);
        assert!(group.replica(3).is_none());

        group.stop().await;
        for d in daemons.lock().iter() {
            assert_eq!(d.stops.load(Ordering::Acquire), 1);
        }
    }

    #[tokio::test]
    async fn replica_keypair_is_deterministic_for_a_given_index() {
        let seed = [0x42u8; 32];
        let group = LifecycleGroup::<CountingDaemon>::spawn(2, seed, |_idx| {
            Arc::new(CountingDaemon::new())
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
        assert_ne!(
            group.replica_keypair(0).entity_id(),
            group.replica_keypair(1).entity_id()
        );
        assert_eq!(group.group_seed(), &seed);
        group.stop().await;
    }

    fn make_scheduler(node_ids: &[u64]) -> Scheduler {
        use crate::adapter::net::behavior::capability::{
            CapabilityAnnouncement, CapabilitySet,
        };
        use crate::adapter::net::behavior::fold::{capability_bridge, CapabilityFold, Fold};
        let fold: Arc<Fold<CapabilityFold>> =
            Arc::new(Fold::with_sweep_interval(std::time::Duration::ZERO));
        let eid = crate::adapter::net::identity::EntityId::from_bytes([0u8; 32]);
        for &id in node_ids {
            capability_bridge::apply_legacy_announcement(
                &fold,
                CapabilityAnnouncement::new(id, eid.clone(), 1, CapabilitySet::new()),
            )
            .expect("apply legacy announcement in fixture");
        }
        let local = node_ids.first().copied().unwrap_or(0xFFFF);
        Scheduler::new(fold, local, CapabilitySet::new())
    }

    #[tokio::test]
    async fn spawn_with_placement_records_distinct_node_per_replica() {
        let scheduler = make_scheduler(&[0x1111, 0x2222, 0x3333]);
        let seen_placements = Arc::new(parking_lot::Mutex::new(Vec::<u64>::new()));
        let seen_placements_clone = seen_placements.clone();

        let group = LifecycleGroup::<CountingDaemon>::spawn_with_placement(
            3,
            [0u8; 32],
            CapabilityFilter::default(),
            &scheduler,
            move |ctx| {
                // Factory observes the placement decision for
                // its index — record it for assertion.
                let node_id = ctx
                    .placement
                    .as_ref()
                    .expect("placement set under spawn_with_placement")
                    .node_id;
                seen_placements_clone.lock().push(node_id);
                Arc::new(CountingDaemon::new())
            },
        )
        .await
        .expect("spawn_with_placement");

        // Spread invariant: three replicas → three distinct nodes.
        let recorded: Vec<u64> = group.placements().iter().map(|p| p.node_id).collect();
        assert_eq!(recorded.len(), 3);
        let unique: std::collections::HashSet<u64> = recorded.iter().copied().collect();
        assert_eq!(unique.len(), 3, "placements must be on distinct nodes");
        assert_eq!(*seen_placements.lock(), recorded);
        for i in 0..3 {
            assert!(group.placement(i).is_some());
        }
        assert!(group.placement(3).is_none());

        group.stop().await;
    }

    #[tokio::test]
    async fn spawn_with_placement_fails_when_fewer_nodes_than_replicas() {
        // Two candidate nodes, three replicas requested — spread
        // invariant rejects the third.
        let scheduler = make_scheduler(&[0xAA, 0xBB]);
        let result = LifecycleGroup::<CountingDaemon>::spawn_with_placement(
            3,
            [0u8; 32],
            CapabilityFilter::default(),
            &scheduler,
            |_ctx| Arc::new(CountingDaemon::new()),
        )
        .await;
        match result {
            Err(LifecycleGroupError::PlacementFailed { index, .. }) => {
                assert_eq!(index, 2);
            }
            Err(other) => panic!("expected PlacementFailed, got {other:?}"),
            Ok(_) => panic!("expected PlacementFailed, got Ok"),
        }
    }

    #[tokio::test]
    async fn spawn_path_leaves_placements_empty() {
        // Placement-free path returns no recorded placements;
        // `placement(0)` is None.
        let group = LifecycleGroup::<CountingDaemon>::spawn(2, [0u8; 32], |_idx| {
            Arc::new(CountingDaemon::new())
        })
        .await
        .expect("spawn");
        assert!(group.placements().is_empty());
        assert!(group.placement(0).is_none());
        group.stop().await;
    }

    #[tokio::test]
    async fn start_failure_at_index_two_returns_typed_error_with_index() {
        let result = LifecycleGroup::<CountingDaemon>::spawn(3, [0u8; 32], |idx| {
            let d = Arc::new(CountingDaemon::new());
            if idx == 2 {
                d.fail_start.store(true, Ordering::Release);
            }
            d
        })
        .await;
        match result {
            Err(LifecycleGroupError::StartFailed { index, error }) => {
                assert_eq!(index, 2);
                match error {
                    LifecycleError::StartFailed(msg) => assert_eq!(msg, "intentional"),
                }
            }
            Err(other) => panic!("expected StartFailed, got {other:?}"),
            Ok(_) => panic!("expected StartFailed, got Ok"),
        }
    }
}
