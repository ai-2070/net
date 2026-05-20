//! SDK wrapper around `adapter::net::compute::StandbyGroup`.
//!
//! Active-passive replication. One member processes events; N−1
//! standbys hold snapshots and catch up via `sync_standbys`. On
//! active failure, `promote` picks the standby with the highest
//! `synced_through` sequence, re-keys it to active, and replays
//! any events that arrived after the last sync.
//!
//! # Usage pattern
//!
//! ```ignore
//! let group = StandbyGroup::spawn(&rt, "counter", cfg)?;
//!
//! // Every event deliver:
//! rt.deliver(group.active_origin(), &event)?;
//! group.on_event_delivered(event);    // buffer for replay
//!
//! // Periodic catchup:
//! group.sync_standbys()?;
//!
//! // On active node failure:
//! group.on_node_failure(failed_node)?;  // may auto-promote
//! ```
//!
//! The `on_event_delivered` call is manual — SDK doesn't
//! auto-hook into `DaemonRuntime::deliver`. See the plan doc's
//! "Open questions" section for the rationale.

use parking_lot::Mutex;
use std::sync::Arc;

use ::net::adapter::net::compute::DaemonHostConfig;
use ::net::adapter::net::compute::{
    standby_group::StandbyGroup as CoreStandbyGroup,
    standby_group::StandbyGroupConfig as CoreStandbyGroupConfig,
};
use ::net::adapter::net::state::causal::CausalEvent;

use crate::compute::{DaemonRuntime, ObserverHandle};
use crate::groups::common::{GroupHealth, MemberInfo, MemberRole};
use crate::groups::error::GroupError;

/// Configuration for a standby group.
#[derive(Debug, Clone)]
pub struct StandbyGroupConfig {
    /// Total members (1 active + N−1 standbys). Must be ≥ 2 or
    /// the core rejects with `InvalidConfig`.
    pub member_count: u8,
    /// 32-byte seed for deterministic per-member keypair derivation.
    pub group_seed: [u8; 32],
    /// Daemon host configuration applied to every member.
    pub host_config: DaemonHostConfig,
}

impl From<StandbyGroupConfig> for CoreStandbyGroupConfig {
    fn from(cfg: StandbyGroupConfig) -> Self {
        CoreStandbyGroupConfig {
            member_count: cfg.member_count,
            group_seed: cfg.group_seed,
            host_config: cfg.host_config,
        }
    }
}

/// A standby group. See module docs for the usage pattern.
pub struct StandbyGroup {
    /// Observer registered against the current active's origin
    /// hash. Populated on `spawn`; replaced on `promote` /
    /// `on_node_failure` when the active changes; cleared on drop.
    ///
    /// Declared first so it drops first on `StandbyGroup::drop`,
    /// unregistering from the runtime's observer map BEFORE `inner`
    /// drops. This eliminates any chance of a concurrent
    /// `deliver` seeing the observer and attempting to upgrade a
    /// `Weak` to an already-dead `Arc` (it'd no-op anyway, but
    /// cleaner).
    observer_handle: Mutex<Option<ObserverHandle>>,
    inner: Arc<Mutex<CoreStandbyGroup>>,
    runtime: DaemonRuntime,
    /// Kind the group was spawned under. Pinned at spawn so
    /// `promote` / `on_node_failure` can't be passed a different
    /// kind that would reconstruct the newly-active member from
    /// a different implementation than the original active +
    /// standbys were built from.
    kind: String,
}

/// Build a post-delivery observer closure that pushes the delivered
/// event into the standby group's replay buffer. Captures a `Weak`
/// to the core group so the observer outliving the group is a
/// no-op rather than a leak.
fn make_buffer_observer(inner: &Arc<Mutex<CoreStandbyGroup>>) -> crate::compute::DeliverObserver {
    let weak = Arc::downgrade(inner);
    Arc::new(move |event: &CausalEvent| {
        if let Some(core) = weak.upgrade() {
            let mut guard = core.lock();
            guard.on_event_delivered(event.clone());
        }
    })
}

impl StandbyGroup {
    /// Spawn a standby group. Member 0 starts as active; the rest
    /// start as standbys with no snapshot (`synced_through == 0`).
    /// The kind is stored and reused by every subsequent mutator.
    pub fn spawn(
        runtime: &DaemonRuntime,
        kind: &str,
        config: StandbyGroupConfig,
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
            CoreStandbyGroup::spawn(config.into(), move || (factory)(), &scheduler, &registry)?;
        let inner = Arc::new(Mutex::new(core));

        // Install the replay-buffer observer on the active's origin
        // so every `DaemonRuntime::deliver` to the active
        // automatically feeds the buffer — no caller-side pairing
        // required.
        let active_origin = inner.lock().active_origin();
        let observer = make_buffer_observer(&inner);
        let handle = runtime.register_deliver_observer(active_origin, observer);

        Ok(Self {
            observer_handle: Mutex::new(Some(handle)),
            inner,
            runtime: runtime.clone(),
            kind: kind.to_string(),
        })
    }

    /// Replace the observer so it now fires for `new_origin`. Used
    /// on promote / on_node_failure when the active member changes.
    fn rebind_observer(&self, new_origin: u64) {
        let observer = make_buffer_observer(&self.inner);
        let new_handle = self.runtime.register_deliver_observer(new_origin, observer);
        // Swap in the new handle; the old one's `Drop` unregisters
        // its entry from the runtime's observer map.
        let mut slot = self.observer_handle.lock();
        *slot = Some(new_handle);
    }

    /// The kind this group was spawned with.
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// `origin_hash` of the current active member. Events always
    /// go through the active; standbys don't process inputs.
    pub fn active_origin(&self) -> u64 {
        self.inner.lock().active_origin()
    }

    /// **Test-only.** Manually push an event into the replay
    /// buffer. Production code does NOT need to call this — a
    /// post-delivery observer installed at `spawn` / `promote`
    /// automatically feeds the buffer on every
    /// `DaemonRuntime::deliver(active_origin, event)`. The method
    /// stays public (and `#[doc(hidden)]`) so tests can simulate
    /// a gap between the last sync and a failure without a live
    /// runtime, but it's not part of the stable public API.
    #[doc(hidden)]
    pub fn on_event_delivered(&self, event: CausalEvent) {
        self.inner.lock().on_event_delivered(event);
    }

    /// Snapshot the active and push to every standby. Returns the
    /// sequence number through which the sync caught up.
    pub fn sync_standbys(&self) -> Result<u64, GroupError> {
        let registry = self.runtime.registry_arc();
        let mut guard = self.inner.lock();
        Ok(guard.sync_standbys(&registry)?)
    }

    /// Promote the most-synced standby to active. Used
    /// automatically by `on_node_failure` when the active fails;
    /// can also be called manually for planned failover.
    /// Returns the promoted member's new `origin_hash` (stays
    /// the same as before — keypair is re-derived deterministically).
    /// Reuses the group's spawn kind; no external parameter.
    pub fn promote(&self) -> Result<u64, GroupError> {
        let factory = self
            .runtime
            .factory_for_kind_pub(&self.kind)
            .map_err(|_| GroupError::FactoryNotFound(self.kind.clone()))?;
        let scheduler = self.runtime.scheduler_arc();
        let registry = self.runtime.registry_arc();
        let new_origin = {
            let mut guard = self.inner.lock();
            guard.promote(move || (factory)(), &registry, &scheduler)?
        };
        // Re-point the post-delivery observer at the new active so
        // future `DaemonRuntime::deliver(new_origin, ...)` calls
        // populate the replay buffer without caller cooperation.
        self.rebind_observer(new_origin);
        Ok(new_origin)
    }

    /// Handle node failure. If the active was on `failed_node_id`,
    /// auto-promotes the most-synced standby and returns its
    /// `origin_hash`. If only standbys were affected, returns
    /// `None` — the caller can re-sync those standbys later.
    pub fn on_node_failure(&self, failed_node_id: u64) -> Result<Option<u64>, GroupError> {
        let factory = self
            .runtime
            .factory_for_kind_pub(&self.kind)
            .map_err(|_| GroupError::FactoryNotFound(self.kind.clone()))?;
        let scheduler = self.runtime.scheduler_arc();
        let registry = self.runtime.registry_arc();
        let result = {
            let mut guard = self.inner.lock();
            guard.on_node_failure(failed_node_id, move || (factory)(), &scheduler, &registry)?
        };
        // If the active was the one that failed, the core returns
        // the promoted member's new origin — rebind the observer so
        // replay buffering follows the new active.
        if let Some(new_origin) = result {
            self.rebind_observer(new_origin);
        }
        Ok(result)
    }

    pub fn on_node_recovery(&self, recovered_node_id: u64) {
        let registry = self.runtime.registry_arc();
        let mut guard = self.inner.lock();
        guard.on_node_recovery(recovered_node_id, &registry);
    }

    pub fn health(&self) -> GroupHealth {
        self.inner.lock().health()
    }

    pub fn active_healthy(&self) -> bool {
        self.inner.lock().active_healthy()
    }

    pub fn active_index(&self) -> u8 {
        self.inner.lock().active_index()
    }

    pub fn member_role(&self, index: u8) -> Option<MemberRole> {
        self.inner.lock().member_role(index)
    }

    pub fn synced_through(&self, index: u8) -> Option<u64> {
        self.inner.lock().synced_through(index)
    }

    pub fn buffered_event_count(&self) -> usize {
        self.inner.lock().buffered_event_count()
    }

    pub fn group_id(&self) -> u32 {
        self.inner.lock().group_id()
    }

    pub fn members(&self) -> Vec<MemberInfo> {
        self.inner.lock().members().to_vec()
    }

    pub fn member_count(&self) -> u8 {
        self.inner.lock().member_count()
    }

    pub fn standby_count(&self) -> u8 {
        self.inner.lock().standby_count()
    }
}

impl std::fmt::Debug for StandbyGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let guard = self.inner.lock();
        f.debug_struct("StandbyGroup")
            .field("group_id", &format_args!("{:#x}", guard.group_id()))
            .field("active_index", &guard.active_index())
            .field("member_count", &guard.member_count())
            .field("buffered_events", &guard.buffered_event_count())
            .finish()
    }
}
