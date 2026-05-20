//! SDK wrapper around `adapter::net::compute::ReplicaGroup`.
//!
//! N interchangeable copies of a daemon with deterministic
//! per-replica identity, load-balanced routing, and
//! auto-replacement on node failure.
//!
//! The SDK wrapper:
//! - Takes a `&DaemonRuntime` + a `kind` string; resolves the
//!   kind to the factory closure registered via
//!   `runtime.register_factory`.
//! - Hides the core `Scheduler` / `DaemonRegistry` behind
//!   `pub(crate)` accessors on `DaemonRuntime`.
//! - Wraps the core group in `Arc<Mutex<_>>` so all methods are
//!   `&self`; contention is negligible because group operations
//!   happen on seconds-to-minutes timescales.

use parking_lot::Mutex;
use std::sync::Arc;

use ::net::adapter::net::behavior::loadbalance::Strategy;
use ::net::adapter::net::compute::DaemonHostConfig;
use ::net::adapter::net::compute::{
    replica_group::ReplicaGroup as CoreReplicaGroup,
    replica_group::ReplicaGroupConfig as CoreReplicaGroupConfig,
};

use crate::compute::DaemonRuntime;
use crate::groups::common::{GroupHealth, MemberInfo, RequestContext};
use crate::groups::error::GroupError;

/// Configuration for a replica group.
#[derive(Debug, Clone)]
pub struct ReplicaGroupConfig {
    /// Desired number of replicas. Must be ≥ 1 or
    /// [`GroupError::Core`] wrapping `InvalidConfig` is returned.
    pub replica_count: u8,
    /// 32-byte seed for deterministic keypair derivation. Same
    /// `group_seed` always produces the same per-replica identity,
    /// so a replica destroyed and recreated by any caller with
    /// access to the seed has a stable `origin_hash`.
    pub group_seed: [u8; 32],
    /// Load-balancing strategy for inbound event routing.
    pub lb_strategy: Strategy,
    /// Daemon host configuration applied to every replica.
    pub host_config: DaemonHostConfig,
}

impl From<ReplicaGroupConfig> for CoreReplicaGroupConfig {
    fn from(cfg: ReplicaGroupConfig) -> Self {
        CoreReplicaGroupConfig {
            replica_count: cfg.replica_count,
            group_seed: cfg.group_seed,
            lb_strategy: cfg.lb_strategy,
            host_config: cfg.host_config,
        }
    }
}

/// A replica group. See module docs for the high-level semantics.
pub struct ReplicaGroup {
    inner: Arc<Mutex<CoreReplicaGroup>>,
    runtime: DaemonRuntime,
    /// Kind the group was spawned with. Pinned at spawn time so that
    /// `scale_to` / `on_node_failure` reuse the *same* factory that
    /// produced the existing replicas. Allowing a caller to pass a
    /// different kind on these methods would silently grow the group
    /// with a different daemon implementation — a replica-4 of an
    /// "echo" group running "counter" code — which violates the
    /// interchangeable-members contract.
    kind: String,
}

impl ReplicaGroup {
    /// Spawn a replica group of `config.replica_count` members,
    /// each constructed by the factory previously registered under
    /// `kind` via [`DaemonRuntime::register_factory`]. The kind is
    /// stored and reused by every subsequent `scale_to` /
    /// `on_node_failure`.
    ///
    /// Errors:
    /// - [`GroupError::NotReady`] if the runtime hasn't started.
    /// - [`GroupError::FactoryNotFound`] if `kind` was never
    ///   registered.
    /// - [`GroupError::Core`] wrapping `InvalidConfig` /
    ///   `PlacementFailed` / `RegistryFailed` for the core's
    ///   failure paths (e.g., `replica_count == 0`, no candidate
    ///   nodes, registry collision on the first member).
    pub fn spawn(
        runtime: &DaemonRuntime,
        kind: &str,
        config: ReplicaGroupConfig,
    ) -> Result<Self, GroupError> {
        if !runtime.is_ready_pub() {
            return Err(GroupError::NotReady);
        }
        let factory = runtime
            .factory_for_kind_pub(kind)
            .map_err(|_| GroupError::FactoryNotFound(kind.to_string()))?;
        let scheduler = runtime.scheduler_arc();
        let registry = runtime.registry_arc();
        let core =
            CoreReplicaGroup::spawn(config.into(), move || (factory)(), &scheduler, &registry)?;
        Ok(Self {
            inner: Arc::new(Mutex::new(core)),
            runtime: runtime.clone(),
            kind: kind.to_string(),
        })
    }

    /// The kind this group was spawned with. Stable for the
    /// group's lifetime.
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// Route an inbound event to the best available replica,
    /// returning the target `origin_hash`. The caller then calls
    /// [`DaemonRuntime::deliver`](crate::compute::DaemonRuntime::deliver)
    /// with the returned hash.
    pub fn route_event(&self, ctx: &RequestContext) -> Result<u64, GroupError> {
        let guard = self.inner.lock();
        Ok(guard.route_event(ctx)?)
    }

    /// Resize the group to `n` replicas. Growing re-uses the
    /// factory registered under the group's spawn kind; shrinking
    /// unregisters the trailing members in reverse index order.
    /// The kind is fixed at spawn time and not accepted as a
    /// parameter — see [`ReplicaGroup::kind`] for the rationale.
    pub fn scale_to(&self, n: u8) -> Result<(), GroupError> {
        let factory = self
            .runtime
            .factory_for_kind_pub(&self.kind)
            .map_err(|_| GroupError::FactoryNotFound(self.kind.clone()))?;
        let scheduler = self.runtime.scheduler_arc();
        let registry = self.runtime.registry_arc();
        let mut guard = self.inner.lock();
        guard.scale_to(n, move || (factory)(), &scheduler, &registry)?;
        Ok(())
    }

    /// Handle failure of a node hosting one or more replicas.
    /// Re-derives each affected replica's deterministic keypair
    /// and re-spawns on a new node (excluding `failed_node_id`).
    /// Returns the list of replica indices that were replaced.
    /// Reuses the group's spawn kind; no external parameter.
    pub fn on_node_failure(&self, failed_node_id: u64) -> Result<Vec<u8>, GroupError> {
        let factory = self
            .runtime
            .factory_for_kind_pub(&self.kind)
            .map_err(|_| GroupError::FactoryNotFound(self.kind.clone()))?;
        let scheduler = self.runtime.scheduler_arc();
        let registry = self.runtime.registry_arc();
        let mut guard = self.inner.lock();
        let replaced =
            guard.on_node_failure(failed_node_id, move || (factory)(), &scheduler, &registry)?;
        Ok(replaced)
    }

    /// Handle recovery of a previously-failed node. Re-marks
    /// members that are still live in the registry as healthy;
    /// members whose respawn completed on another node stay on
    /// their new home.
    pub fn on_node_recovery(&self, recovered_node_id: u64) {
        let registry = self.runtime.registry_arc();
        let mut guard = self.inner.lock();
        guard.on_node_recovery(recovered_node_id, &registry);
    }

    /// Aggregate health of the group.
    pub fn health(&self) -> GroupHealth {
        self.inner.lock().health()
    }

    /// Unique group identifier (hash of `group_seed`).
    pub fn group_id(&self) -> u32 {
        self.inner.lock().group_id()
    }

    /// Owned snapshot of the current member roster. Owned
    /// (cloned) rather than borrowed so the caller doesn't hold
    /// the lock across await points.
    pub fn replicas(&self) -> Vec<MemberInfo> {
        self.inner.lock().replicas().to_vec()
    }

    /// Current replica count (both healthy and unhealthy).
    pub fn replica_count(&self) -> u8 {
        self.inner.lock().replica_count()
    }

    /// Number of replicas currently healthy.
    pub fn healthy_count(&self) -> u8 {
        self.inner.lock().healthy_count()
    }
}

impl std::fmt::Debug for ReplicaGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let guard = self.inner.lock();
        f.debug_struct("ReplicaGroup")
            .field("group_id", &format_args!("{:#x}", guard.group_id()))
            .field("replica_count", &guard.replica_count())
            .field("healthy_count", &guard.healthy_count())
            .finish()
    }
}
