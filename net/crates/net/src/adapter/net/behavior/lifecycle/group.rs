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

use super::daemon::{LifecycleDaemon, LifecycleError, LifecycleHandle, ReplicaHealth};
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
        factory: F,
    ) -> Result<Self, LifecycleGroupError>
    where
        F: FnMut(u8) -> Arc<L>,
    {
        if replica_count == 0 {
            return Err(LifecycleGroupError::InvalidConfig(
                "replica_count must be > 0".into(),
            ));
        }
        let (replicas, handles) = start_replicas(replica_count, factory).await?;
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
        let mut placements: Vec<PlacementDecision> = Vec::with_capacity(replica_count as usize);
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

    /// Per-replica health snapshot in declaration order.
    /// Polls each replica's
    /// [`LifecycleDaemon::health`] in parallel via
    /// `join_all` — the typical impl is cheap (atomic load or
    /// short-RwLock read), so the parallelism is mostly future-
    /// proofing for impls that need to await a lock.
    pub async fn health(&self) -> Vec<ReplicaHealth> {
        let futures = self.replicas.iter().map(|r| {
            let r = r.clone();
            async move { r.health().await }
        });
        futures::future::join_all(futures).await
    }

    /// Replace the daemon at `index` with `new_daemon`. The old
    /// handle is stopped + awaited before the new one is
    /// installed, so the slot is briefly empty during the
    /// transition. Returns the stopped handle's underlying
    /// daemon Arc — callers wanting to inspect the old state
    /// (e.g. for forensics on what caused the unhealthy flip)
    /// can hold onto it.
    ///
    /// Errors:
    /// - `InvalidConfig` if `index >= replica_count`.
    /// - `StartFailed { index, error }` if the new handle's
    ///   `on_start` fails. The slot is left empty in this case
    ///   — caller must retry or shrink the group.
    pub async fn replace(
        &mut self,
        index: usize,
        new_daemon: Arc<L>,
    ) -> Result<Arc<L>, LifecycleGroupError> {
        if index >= self.replicas.len() {
            return Err(LifecycleGroupError::InvalidConfig(format!(
                "replace index {index} out of bounds for {} replicas",
                self.replicas.len()
            )));
        }
        // Drain the old handle out of the Vec and stop it. Use
        // `Vec::remove` shift-cost is bounded by `replica_count`
        // which is u8-bounded; cheap.
        let old_handle = self.handles.remove(index);
        old_handle.stop().await;
        let old_replica = std::mem::replace(&mut self.replicas[index], new_daemon.clone());

        // Start the replacement.
        let trait_obj: Arc<dyn LifecycleDaemon> = new_daemon;
        let new_handle = match LifecycleHandle::start(trait_obj).await {
            Ok(h) => h,
            Err(error) => {
                // Slot is now empty for handles but replicas Vec
                // still has the new Arc. Leave it that way and
                // surface the error — `health()` will report
                // unhealthy for the missing-handle index.
                return Err(LifecycleGroupError::StartFailed {
                    index: u8::try_from(index).unwrap_or(u8::MAX),
                    error,
                });
            }
        };
        self.handles.insert(index, new_handle);
        Ok(old_replica)
    }

    /// Append one replica to the group, growing it in place. The
    /// factory receives the new replica's index (= current
    /// `replica_count`). Existing replicas keep their identities
    /// and their handles — neither stops nor restarts. This is the
    /// scale-up primitive for [`AggregatorRegistry::scale_group`]
    /// and the `Scale` RPC.
    ///
    /// Errors:
    /// - `InvalidConfig` when `replica_count == u8::MAX`
    ///   (group-size hard cap; the index field is `u8`).
    /// - `StartFailed { index, error }` when the new handle's
    ///   `on_start` fails. The factory's `Arc<L>` is dropped
    ///   before the error returns, so no zombie replica leaks.
    ///
    /// # Placement
    ///
    /// `add_replica` does **not** engage the scheduler. A group
    /// originally created via [`Self::spawn_with_placement`] still
    /// has its placement Vec — the new replica gets no placement
    /// entry and runs on the local node. Operators who need
    /// placement-aware scale-up wait for a future
    /// `add_replica_with_placement` sibling; the single-process /
    /// single-host deployment shipping today doesn't engage that
    /// surface.
    pub async fn add_replica<F>(&mut self, factory: F) -> Result<u8, LifecycleGroupError>
    where
        F: FnOnce(u8) -> Arc<L>,
    {
        if self.replicas.len() >= u8::MAX as usize {
            return Err(LifecycleGroupError::InvalidConfig(format!(
                "cannot grow past u8::MAX replicas (current: {})",
                self.replicas.len()
            )));
        }
        // u8 cast is safe by the guard above (len < 255).
        let new_idx = self.replicas.len() as u8;
        let daemon = factory(new_idx);
        let trait_obj: Arc<dyn LifecycleDaemon> = daemon.clone();
        let handle = LifecycleHandle::start(trait_obj).await.map_err(|error| {
            LifecycleGroupError::StartFailed {
                index: new_idx,
                error,
            }
        })?;
        self.replicas.push(daemon);
        self.handles.push(handle);
        Ok(new_idx)
    }

    /// Bulk version of [`Self::add_replica`]. Constructs `count`
    /// new daemons via the factory, then runs their `on_start`
    /// handlers **concurrently** via `try_join_all` (same shape
    /// as the initial-spawn path in `start_replicas`). If any
    /// `on_start` fails, every successfully-started replica's
    /// handle is dropped — its `LifecycleHandle::Drop` schedules
    /// `on_stop` on a detached task, so partial-start cleanup is
    /// automatic. The group itself stays at its pre-call size on
    /// error.
    ///
    /// Used by [`super::super::aggregator::AggregatorRegistry::scale_group`]
    /// so a 1→N grow doesn't serialize N `on_start`s under the
    /// entry mutex (which would block `List` / `health` /
    /// `HealthMonitor` for the duration).
    pub async fn add_replicas<F>(
        &mut self,
        count: u8,
        mut factory: F,
    ) -> Result<(), LifecycleGroupError>
    where
        F: FnMut(u8) -> Arc<L>,
    {
        if count == 0 {
            return Ok(());
        }
        let new_total = (self.replicas.len() as u32) + (count as u32);
        if new_total > u8::MAX as u32 {
            return Err(LifecycleGroupError::InvalidConfig(format!(
                "cannot grow past u8::MAX replicas (current: {}, requested +{})",
                self.replicas.len(),
                count
            )));
        }
        // Pre-allocate everything synchronously so the FnMut
        // closure runs serially (factory is operator-defined; we
        // don't get to thread-balance it). The starts get
        // collected as futures and awaited concurrently.
        let base_idx = self.replicas.len() as u8;
        let mut new_daemons: Vec<Arc<L>> = Vec::with_capacity(count as usize);
        let mut starts = Vec::with_capacity(count as usize);
        for offset in 0..count {
            let idx = base_idx + offset;
            let daemon = factory(idx);
            new_daemons.push(daemon.clone());
            let trait_obj: Arc<dyn LifecycleDaemon> = daemon;
            starts.push((idx, LifecycleHandle::start(trait_obj)));
        }
        // Await every on_start concurrently. join_all preserves
        // order so we can map the index back when any fails.
        let started: Vec<_> = futures::future::join_all(
            starts
                .into_iter()
                .map(|(idx, fut)| async move { (idx, fut.await) }),
        )
        .await;
        let mut handles = Vec::with_capacity(count as usize);
        for (idx, result) in started {
            match result {
                Ok(h) => handles.push(h),
                Err(error) => {
                    // Drop everything started so far — their RAII
                    // `LifecycleHandle::Drop` schedules `on_stop`.
                    // Drop `new_daemons` too so we don't leak the
                    // Arc<L>s that never made it to the group.
                    drop(handles);
                    drop(new_daemons);
                    return Err(LifecycleGroupError::StartFailed { index: idx, error });
                }
            }
        }
        // All starts succeeded — commit the daemons + handles.
        self.replicas.extend(new_daemons);
        self.handles.extend(handles);
        Ok(())
    }

    /// Stop and pop the last replica. Returns the stopped
    /// replica's Arc so callers can inspect post-stop state (e.g.
    /// for forensic logging). The other replicas' handles are
    /// untouched — neither stopped nor signalled — preserving
    /// their identity, generation counters, and any in-memory
    /// state.
    ///
    /// Refuses to drop below one replica: callers that want to
    /// dismantle the whole group should call [`Self::stop`]
    /// instead. Returning an error rather than completing as a
    /// no-op surfaces the typo at the caller (e.g. operator who
    /// meant `--replica-count 1` and wrote `--replica-count 0`).
    ///
    /// If the group was created via
    /// [`Self::spawn_with_placement`], the last placement entry
    /// is also popped so the parallel-Vec invariant
    /// (`placements.len() == replicas.len()` when populated) is
    /// preserved.
    pub async fn remove_last(&mut self) -> Result<Arc<L>, LifecycleGroupError> {
        if self.replicas.len() <= 1 {
            return Err(LifecycleGroupError::InvalidConfig(format!(
                "cannot remove last replica below count 1 (current: {}); \
                 call stop() to dismantle the whole group instead",
                self.replicas.len()
            )));
        }
        // `expect_used` lint guard: the `len <= 1` check above
        // guarantees both pops succeed; suppress lint locally
        // rather than fall back to `unwrap_or_else` panic shapes
        // that would obscure the invariant.
        #[allow(clippy::expect_used)]
        let handle = self
            .handles
            .pop()
            .expect("replica_count > 1 above; handles parallel to replicas");
        handle.stop().await;
        #[allow(clippy::expect_used)]
        let replica = self
            .replicas
            .pop()
            .expect("replica_count > 1 above; pop after handle.stop succeeded");
        if !self.placements.is_empty() {
            // Parallel-Vec invariant: when placements is
            // populated, it tracks replicas 1-to-1. Pop the last
            // so a subsequent `placement(replicas.len()-1)` still
            // resolves.
            self.placements.pop();
        }
        Ok(replica)
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

    /// Surrender the group's parts to the caller. Used by
    /// process-level registries (e.g.
    /// `AggregatorRegistry::register`) that take ownership of
    /// the handles for shutdown but still want concrete-typed
    /// access to the replicas + placement records.
    ///
    /// Returns `(replicas, placements, handles, group_seed)` in
    /// declaration order. After this call the group no longer
    /// exists; lifecycle shutdown becomes the caller's
    /// responsibility (via the returned `LifecycleHandle`s).
    pub fn into_parts(
        self,
    ) -> (
        Vec<Arc<L>>,
        Vec<PlacementDecision>,
        Vec<LifecycleHandle>,
        [u8; 32],
    ) {
        (
            self.replicas,
            self.placements,
            self.handles,
            self.group_seed,
        )
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
        starts
            .into_iter()
            .map(|(i, fut)| async move { (i, fut.await) }),
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
        let group =
            LifecycleGroup::<CountingDaemon>::spawn(
                2,
                seed,
                |_idx| Arc::new(CountingDaemon::new()),
            )
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
        use crate::adapter::net::behavior::capability::{CapabilityAnnouncement, CapabilitySet};
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

    /// Daemon variant that reports unhealthy when `force_unhealthy`
    /// is set — lets tests pin LifecycleGroup::health snapshot
    /// + replace() behavior without dragging in AggregatorDaemon.
    struct HealthControlDaemon {
        force_unhealthy: AtomicBool,
        starts: AtomicU64,
        stops: AtomicU64,
    }

    impl HealthControlDaemon {
        fn new() -> Self {
            Self {
                force_unhealthy: AtomicBool::new(false),
                starts: AtomicU64::new(0),
                stops: AtomicU64::new(0),
            }
        }
    }

    #[async_trait]
    impl LifecycleDaemon for HealthControlDaemon {
        fn name(&self) -> &str {
            "health-control"
        }
        async fn on_start(self: Arc<Self>) -> Result<(), LifecycleError> {
            self.starts.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }
        async fn on_stop(&self) {
            self.stops.fetch_add(1, Ordering::AcqRel);
        }
        async fn health(&self) -> ReplicaHealth {
            if self.force_unhealthy.load(Ordering::Acquire) {
                ReplicaHealth::unhealthy("test-forced")
            } else {
                ReplicaHealth::healthy()
            }
        }
    }

    #[tokio::test]
    async fn health_returns_per_replica_snapshot_in_declaration_order() {
        let daemons: Arc<parking_lot::Mutex<Vec<Arc<HealthControlDaemon>>>> =
            Arc::new(parking_lot::Mutex::new(Vec::new()));
        let daemons_clone = daemons.clone();
        let group = LifecycleGroup::<HealthControlDaemon>::spawn(3, [0u8; 32], move |_idx| {
            let d = Arc::new(HealthControlDaemon::new());
            daemons_clone.lock().push(d.clone());
            d
        })
        .await
        .expect("spawn");

        // All three healthy initially.
        let snapshot = group.health().await;
        assert_eq!(snapshot.len(), 3);
        for h in &snapshot {
            assert!(h.healthy);
            assert!(h.diagnostic.is_none());
        }

        // Flip replica 1 to unhealthy.
        daemons.lock()[1]
            .force_unhealthy
            .store(true, Ordering::Release);
        let snapshot = group.health().await;
        assert!(snapshot[0].healthy);
        assert!(!snapshot[1].healthy);
        assert_eq!(snapshot[1].diagnostic.as_deref(), Some("test-forced"));
        assert!(snapshot[2].healthy);

        group.stop().await;
    }

    #[tokio::test]
    async fn replace_stops_old_handle_and_installs_new_daemon() {
        let daemons: Arc<parking_lot::Mutex<Vec<Arc<HealthControlDaemon>>>> =
            Arc::new(parking_lot::Mutex::new(Vec::new()));
        let daemons_clone = daemons.clone();
        let mut group = LifecycleGroup::<HealthControlDaemon>::spawn(2, [0u8; 32], move |_idx| {
            let d = Arc::new(HealthControlDaemon::new());
            daemons_clone.lock().push(d.clone());
            d
        })
        .await
        .expect("spawn");

        let original_idx_1 = daemons.lock()[1].clone();
        assert_eq!(original_idx_1.stops.load(Ordering::Acquire), 0);

        // Build a replacement.
        let replacement = Arc::new(HealthControlDaemon::new());
        let returned = group
            .replace(1, replacement.clone())
            .await
            .expect("replace");
        // The returned Arc is the original replica.
        assert!(Arc::ptr_eq(&returned, &original_idx_1));
        // The old daemon was stopped.
        assert_eq!(original_idx_1.stops.load(Ordering::Acquire), 1);
        // The replacement was started.
        assert_eq!(replacement.starts.load(Ordering::Acquire), 1);
        // The group's typed accessor reflects the swap.
        let now_at_1 = group.replica(1).expect("replica 1");
        assert!(Arc::ptr_eq(&now_at_1, &replacement));

        group.stop().await;
        // The replacement's on_stop fires on group.stop.
        assert_eq!(replacement.stops.load(Ordering::Acquire), 1);
    }

    #[tokio::test]
    async fn replace_rejects_out_of_bounds_index() {
        let mut group = LifecycleGroup::<HealthControlDaemon>::spawn(2, [0u8; 32], |_idx| {
            Arc::new(HealthControlDaemon::new())
        })
        .await
        .expect("spawn");
        let replacement = Arc::new(HealthControlDaemon::new());
        match group.replace(5, replacement).await {
            Err(LifecycleGroupError::InvalidConfig(msg)) => {
                assert!(msg.contains("out of bounds"), "msg was: {msg}");
            }
            Err(other) => panic!("expected InvalidConfig, got {other:?}"),
            Ok(_) => panic!("expected InvalidConfig, got Ok"),
        }
        group.stop().await;
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

    #[tokio::test]
    async fn add_replica_grows_in_place_preserving_existing_replicas() {
        let daemons: Arc<parking_lot::Mutex<Vec<Arc<CountingDaemon>>>> =
            Arc::new(parking_lot::Mutex::new(Vec::new()));
        let daemons_clone = daemons.clone();
        let mut group = LifecycleGroup::<CountingDaemon>::spawn(2, [0u8; 32], move |_idx| {
            let d = Arc::new(CountingDaemon::new());
            daemons_clone.lock().push(d.clone());
            d
        })
        .await
        .expect("initial spawn");
        // Existing replicas each ran on_start exactly once.
        for d in daemons.lock().iter() {
            assert_eq!(d.starts.load(Ordering::Acquire), 1);
        }

        let new_replica = Arc::new(CountingDaemon::new());
        let new_replica_clone = new_replica.clone();
        let new_idx = group
            .add_replica(move |_idx| new_replica_clone)
            .await
            .expect("add_replica");
        assert_eq!(new_idx, 2, "new index = old replica_count");
        assert_eq!(group.replica_count(), 3);
        assert_eq!(new_replica.starts.load(Ordering::Acquire), 1);
        // Critical: existing replicas did NOT restart — their
        // start counters stay at 1 (no respawn).
        for d in daemons.lock().iter() {
            assert_eq!(
                d.starts.load(Ordering::Acquire),
                1,
                "existing replica restarted"
            );
            assert_eq!(
                d.stops.load(Ordering::Acquire),
                0,
                "existing replica stopped"
            );
        }

        group.stop().await;
    }

    #[tokio::test]
    async fn remove_last_stops_only_the_last_replica() {
        let daemons: Arc<parking_lot::Mutex<Vec<Arc<CountingDaemon>>>> =
            Arc::new(parking_lot::Mutex::new(Vec::new()));
        let daemons_clone = daemons.clone();
        let mut group = LifecycleGroup::<CountingDaemon>::spawn(3, [0u8; 32], move |_idx| {
            let d = Arc::new(CountingDaemon::new());
            daemons_clone.lock().push(d.clone());
            d
        })
        .await
        .expect("spawn");

        let removed = group.remove_last().await.expect("remove_last");
        assert_eq!(group.replica_count(), 2);
        // Returned Arc is the original index-2 replica.
        let last_original = daemons.lock()[2].clone();
        assert!(Arc::ptr_eq(&removed, &last_original));
        // The dropped replica's stop counter incremented exactly once.
        assert_eq!(removed.stops.load(Ordering::Acquire), 1);
        // Indices 0 and 1 untouched.
        let kept = daemons.lock();
        assert_eq!(kept[0].stops.load(Ordering::Acquire), 0);
        assert_eq!(kept[1].stops.load(Ordering::Acquire), 0);
        drop(kept);

        group.stop().await;
    }

    #[tokio::test]
    async fn remove_last_refuses_to_drop_below_one() {
        let mut group = LifecycleGroup::<CountingDaemon>::spawn(1, [0u8; 32], |_idx| {
            Arc::new(CountingDaemon::new())
        })
        .await
        .expect("spawn");
        match group.remove_last().await {
            Ok(_) => panic!("expected InvalidConfig, got Ok"),
            Err(LifecycleGroupError::InvalidConfig(msg)) => {
                assert!(msg.contains("cannot remove last replica"), "msg was: {msg}");
            }
            Err(other) => panic!("expected InvalidConfig, got {other:?}"),
        }
        // Replica still there, can still stop the group cleanly.
        assert_eq!(group.replica_count(), 1);
        group.stop().await;
    }

    #[tokio::test]
    async fn add_replicas_bulk_runs_starts_concurrently() {
        use std::time::Duration;
        // Each daemon's on_start sleeps for SLEEP; if `add_replicas`
        // serialized them, total wall-clock would be N×SLEEP. With
        // try_join_all the bound is ~1×SLEEP (plus scheduling).
        const SLEEP: Duration = Duration::from_millis(120);
        const N: u8 = 8;

        struct SleepyDaemon {
            stops: AtomicU64,
        }
        #[async_trait]
        impl LifecycleDaemon for SleepyDaemon {
            fn name(&self) -> &str {
                "sleepy"
            }
            async fn on_start(self: Arc<Self>) -> Result<(), LifecycleError> {
                tokio::time::sleep(SLEEP).await;
                Ok(())
            }
            async fn on_stop(&self) {
                self.stops.fetch_add(1, Ordering::AcqRel);
            }
        }

        let mut group = LifecycleGroup::<SleepyDaemon>::spawn(1, [0u8; 32], |_idx| {
            Arc::new(SleepyDaemon {
                stops: AtomicU64::new(0),
            })
        })
        .await
        .expect("initial spawn");

        let started = std::time::Instant::now();
        group
            .add_replicas(N, |_idx| {
                Arc::new(SleepyDaemon {
                    stops: AtomicU64::new(0),
                })
            })
            .await
            .expect("add_replicas");
        let elapsed = started.elapsed();
        assert_eq!(group.replica_count(), 1 + N as usize);
        // Serial bound is N×SLEEP. Allow a generous margin
        // (2.5×SLEEP) for CI scheduling noise.
        assert!(
            elapsed < SLEEP * 5 / 2,
            "add_replicas took {elapsed:?} — likely serialized (serial bound {}ms)",
            (SLEEP * N as u32).as_millis()
        );

        group.stop().await;
    }

    #[tokio::test]
    async fn add_replicas_propagates_first_failure_and_leaves_group_unchanged() {
        let mut group = LifecycleGroup::<CountingDaemon>::spawn(1, [0u8; 32], |_idx| {
            Arc::new(CountingDaemon::new())
        })
        .await
        .expect("spawn");

        let mut call = 0u8;
        let result = group
            .add_replicas(3, |_idx| {
                let d = Arc::new(CountingDaemon::new());
                // Second of the three new replicas fails on_start.
                if call == 1 {
                    d.fail_start.store(true, Ordering::Release);
                }
                call += 1;
                d
            })
            .await;
        match result {
            Ok(_) => panic!("expected StartFailed, got Ok"),
            Err(LifecycleGroupError::StartFailed { index, .. }) => {
                // 0-indexed; the original replica occupies idx 0,
                // so the failing slot is index 1+1 = 2.
                assert_eq!(index, 2);
            }
            Err(other) => panic!("expected StartFailed, got {other:?}"),
        }
        // Group stayed at its pre-call size — no zombie additions.
        assert_eq!(group.replica_count(), 1);
        group.stop().await;
    }

    #[tokio::test]
    async fn add_replica_propagates_on_start_failure() {
        let mut group = LifecycleGroup::<CountingDaemon>::spawn(1, [0u8; 32], |_idx| {
            Arc::new(CountingDaemon::new())
        })
        .await
        .expect("spawn");
        let result = group
            .add_replica(|_idx| {
                let d = Arc::new(CountingDaemon::new());
                d.fail_start.store(true, Ordering::Release);
                d
            })
            .await;
        match result {
            Ok(_) => panic!("expected StartFailed, got Ok"),
            Err(LifecycleGroupError::StartFailed { index, .. }) => {
                assert_eq!(index, 1);
            }
            Err(other) => panic!("expected StartFailed, got {other:?}"),
        }
        // Group still has the original replica; no zombie added.
        assert_eq!(group.replica_count(), 1);
        group.stop().await;
    }
}
