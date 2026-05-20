//! SDK wrapper around `adapter::net::compute::ForkGroup`.
//!
//! N independent daemons forked from a common parent, each with a
//! unique identity and its own causal chain but carrying a
//! verifiable `ForkRecord` that documents lineage back to the
//! parent.

use parking_lot::Mutex;
use std::sync::Arc;

use ::net::adapter::net::behavior::loadbalance::Strategy;
use ::net::adapter::net::compute::DaemonHostConfig;
use ::net::adapter::net::compute::{
    fork_group::ForkGroup as CoreForkGroup, fork_group::ForkGroupConfig as CoreForkGroupConfig,
};

use crate::compute::DaemonRuntime;
use crate::groups::common::{ForkRecord, GroupHealth, MemberInfo, RequestContext};
use crate::groups::error::GroupError;

/// Configuration for a fork group.
#[derive(Debug, Clone)]
pub struct ForkGroupConfig {
    /// Desired number of forks. Must be ≥ 1.
    pub fork_count: u8,
    /// Load-balancing strategy.
    pub lb_strategy: Strategy,
    /// Daemon host configuration applied to every fork.
    pub host_config: DaemonHostConfig,
}

impl From<ForkGroupConfig> for CoreForkGroupConfig {
    fn from(cfg: ForkGroupConfig) -> Self {
        CoreForkGroupConfig {
            fork_count: cfg.fork_count,
            lb_strategy: cfg.lb_strategy,
            host_config: cfg.host_config,
        }
    }
}

/// A fork group. See module docs for semantics.
pub struct ForkGroup {
    inner: Arc<Mutex<CoreForkGroup>>,
    runtime: DaemonRuntime,
    /// Kind the group was forked under. Pinned at `fork()` so
    /// every subsequent `scale_to` / `on_node_failure` reuses the
    /// same factory — a fork group with mixed implementations
    /// would produce forks whose `ForkRecord` lineage wouldn't
    /// correspond to the code actually running.
    kind: String,
}

impl ForkGroup {
    /// Fork N new daemons from a parent at `fork_seq`. `kind`
    /// resolves to the factory registered via
    /// [`DaemonRuntime::register_factory`](crate::compute::DaemonRuntime::register_factory).
    /// Stored and reused by every subsequent mutator — see
    /// [`ForkGroup::kind`].
    pub fn fork(
        runtime: &DaemonRuntime,
        kind: &str,
        parent_origin: u64,
        fork_seq: u64,
        config: ForkGroupConfig,
    ) -> Result<Self, GroupError> {
        if !runtime.is_ready_pub() {
            return Err(GroupError::NotReady);
        }
        let factory = runtime
            .factory_for_kind_pub(kind)
            .map_err(|_| GroupError::FactoryNotFound(kind.to_string()))?;
        let scheduler = runtime.scheduler_arc();
        let registry = runtime.registry_arc();
        let core = CoreForkGroup::fork(
            parent_origin,
            fork_seq,
            config.into(),
            move || (factory)(),
            &scheduler,
            &registry,
        )?;
        Ok(Self {
            inner: Arc::new(Mutex::new(core)),
            runtime: runtime.clone(),
            kind: kind.to_string(),
        })
    }

    /// The kind this group was forked under. Stable for the
    /// group's lifetime.
    pub fn kind(&self) -> &str {
        &self.kind
    }

    pub fn route_event(&self, ctx: &RequestContext) -> Result<u64, GroupError> {
        Ok(self.inner.lock().route_event(ctx)?)
    }

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

    pub fn on_node_failure(&self, failed_node_id: u64) -> Result<Vec<u8>, GroupError> {
        let factory = self
            .runtime
            .factory_for_kind_pub(&self.kind)
            .map_err(|_| GroupError::FactoryNotFound(self.kind.clone()))?;
        let scheduler = self.runtime.scheduler_arc();
        let registry = self.runtime.registry_arc();
        let mut guard = self.inner.lock();
        Ok(guard.on_node_failure(failed_node_id, move || (factory)(), &scheduler, &registry)?)
    }

    pub fn on_node_recovery(&self, recovered_node_id: u64) {
        let registry = self.runtime.registry_arc();
        let mut guard = self.inner.lock();
        guard.on_node_recovery(recovered_node_id, &registry);
    }

    pub fn health(&self) -> GroupHealth {
        self.inner.lock().health()
    }

    pub fn parent_origin(&self) -> u64 {
        self.inner.lock().parent_origin()
    }

    pub fn fork_seq(&self) -> u64 {
        self.inner.lock().fork_seq()
    }

    /// Owned clones of the lineage records for every fork. Cloned
    /// (not borrowed) so the caller doesn't hold the lock.
    pub fn fork_records(&self) -> Vec<ForkRecord> {
        self.inner
            .lock()
            .fork_records()
            .iter()
            .map(|r| (*r).clone())
            .collect()
    }

    /// `true` iff every fork's `ForkRecord` verifies against its
    /// parent. Core performs the signature + sentinel checks.
    pub fn verify_lineage(&self) -> bool {
        self.inner.lock().verify_lineage()
    }

    pub fn members(&self) -> Vec<MemberInfo> {
        self.inner.lock().members().to_vec()
    }

    pub fn fork_count(&self) -> u8 {
        self.inner.lock().fork_count()
    }

    pub fn healthy_count(&self) -> u8 {
        self.inner.lock().healthy_count()
    }
}

impl std::fmt::Debug for ForkGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let guard = self.inner.lock();
        f.debug_struct("ForkGroup")
            .field(
                "parent_origin",
                &format_args!("{:#x}", guard.parent_origin()),
            )
            .field("fork_seq", &guard.fork_seq())
            .field("fork_count", &guard.fork_count())
            .field("healthy_count", &guard.healthy_count())
            .finish()
    }
}
