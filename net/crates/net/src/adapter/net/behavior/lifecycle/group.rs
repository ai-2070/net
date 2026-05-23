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

use std::sync::Arc;

use super::daemon::{LifecycleDaemon, LifecycleError, LifecycleHandle};
use crate::adapter::net::compute::replica_group::derive_replica_keypair;
use crate::adapter::net::identity::EntityKeypair;

/// Group-spawn failure shape. Distinguishes config-time errors
/// (rejected on the caller side before any on_start fires) from
/// per-replica `on_start` failures (carry the failing index for
/// operator diagnosis).
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
}

impl std::fmt::Display for LifecycleGroupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidConfig(msg) => write!(f, "invalid lifecycle group config: {msg}"),
            Self::StartFailed { index, error } => {
                write!(f, "replica {index} failed to start: {error}")
            }
        }
    }
}

impl std::error::Error for LifecycleGroupError {}

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
        // Materialize concrete + dyn Arcs in lockstep so the
        // group retains typed access while LifecycleHandle takes
        // the erased trait object.
        let mut replicas: Vec<Arc<L>> = Vec::with_capacity(replica_count as usize);
        let mut starts = Vec::with_capacity(replica_count as usize);
        for index in 0..replica_count {
            let daemon = factory(index);
            replicas.push(daemon.clone());
            let trait_obj: Arc<dyn LifecycleDaemon> = daemon;
            starts.push((index, LifecycleHandle::start(trait_obj)));
        }
        // Drive starts concurrently. Map each to (index,
        // Result) so a failure carries the offending index for
        // diagnosis.
        let started: Vec<_> = futures::future::join_all(
            starts.into_iter().map(|(i, fut)| async move { (i, fut.await) }),
        )
        .await;
        let mut handles = Vec::with_capacity(replica_count as usize);
        for (index, result) in started {
            match result {
                Ok(h) => handles.push(h),
                Err(error) => {
                    // Drop the handles collected so far + the
                    // concrete replica Arcs. Each handle's Drop
                    // schedules an `on_stop` cleanup; the
                    // detached task handles teardown.
                    drop(handles);
                    drop(replicas);
                    return Err(LifecycleGroupError::StartFailed { index, error });
                }
            }
        }
        Ok(Self {
            handles,
            replicas,
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
