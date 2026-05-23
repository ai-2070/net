//! [`LifecycleDaemon`] — async lifecycle trait + RAII handle.
//!
//! See the module-level doc on [`super`] for the rationale on
//! why this lives separately from
//! [`MeshDaemon`](crate::adapter::net::compute::MeshDaemon).
//!
//! # Trait shape
//!
//! - `on_start(self: Arc<Self>)` — spawn whatever background
//!   work the daemon needs. Called exactly once per daemon
//!   before any other lifecycle method. Receives `Arc<Self>`
//!   so implementations can move the daemon into a tokio task
//!   without weak-ref gymnastics.
//! - `on_stop(&self)` — signal the background work to stop.
//!   Called exactly once before the handle drops. Idempotent
//!   in practice; [`LifecycleHandle`] only calls it once.
//!
//! The trait surface is intentionally minimal so future
//! lifecycle hooks (`on_pause`, `on_drain`, …) can land
//! without breaking existing impls.

use async_trait::async_trait;
use std::sync::Arc;

use crate::adapter::net::behavior::capability::CapabilityFilter;

/// Per-replica health snapshot reported by
/// [`LifecycleDaemon::health`]. Distinct from the substrate's
/// `DaemonHealth` so lifecycle daemons can carry typed
/// diagnostic strings without dragging in cross-module
/// dependencies. The
/// [`LifecycleGroup::health`](super::group::LifecycleGroup::health)
/// accessor returns one of these per replica in declaration
/// order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicaHealth {
    /// True when the daemon's last heartbeat was within its
    /// liveness window. Implementations that don't carry a
    /// liveness notion can leave this `true` permanently —
    /// the default [`LifecycleDaemon::health`] does exactly
    /// that.
    pub healthy: bool,
    /// Daemon-specific diagnostic when `healthy == false`.
    /// Operator surfaces render this verbatim.
    pub diagnostic: Option<String>,
}

impl ReplicaHealth {
    /// Healthy snapshot with no diagnostic. The default
    /// [`LifecycleDaemon::health`] impl returns this.
    pub fn healthy() -> Self {
        Self {
            healthy: true,
            diagnostic: None,
        }
    }

    /// Unhealthy snapshot carrying a diagnostic for operator
    /// rendering.
    pub fn unhealthy(reason: impl Into<String>) -> Self {
        Self {
            healthy: false,
            diagnostic: Some(reason.into()),
        }
    }
}

/// Async lifecycle trait for native mesh-aware daemons. See
/// module doc for the trait's intent and the
/// [`MeshDaemon`](crate::adapter::net::compute::MeshDaemon)
/// distinction.
#[async_trait]
pub trait LifecycleDaemon: Send + Sync + 'static {
    /// Human-readable name — used in tracing spans + operator
    /// surfaces (`net aggregator inspect`, the Deck panel
    /// header). Stable across the daemon's lifetime; no
    /// per-replica differentiation.
    fn name(&self) -> &str;

    /// Capability requirements for placement. Mirrors
    /// [`MeshDaemon::requirements`](crate::adapter::net::compute::MeshDaemon::requirements)
    /// so the same scheduler primitives
    /// ([`Scheduler::place`](crate::adapter::net::compute::Scheduler::place),
    /// [`GroupCoordinator::place_with_spread`](crate::adapter::net::compute::GroupCoordinator::place_with_spread))
    /// apply to lifecycle daemons without duplicating the
    /// filter type. Returns `CapabilityFilter::default()` to
    /// run anywhere.
    ///
    /// Used by
    /// [`LifecycleGroup::spawn_with_placement`](super::group::LifecycleGroup::spawn_with_placement)
    /// — invoked once before placement to read the requirements
    /// applied to every replica. Daemons whose requirements are
    /// uniform across replicas (the common case) can leave the
    /// default empty filter and pass requirements directly to
    /// `spawn_with_placement`.
    fn requirements(&self) -> CapabilityFilter {
        CapabilityFilter::default()
    }

    /// Called once when a [`LifecycleHandle`] wrapping `self`
    /// is created. Implementations spawn whatever long-running
    /// background work they need (a tokio interval loop, a
    /// subscription handler, etc.). Receives `Arc<Self>` so
    /// implementations can move the daemon into a spawned task
    /// without weak-ref gymnastics. Errors abort the lifecycle
    /// — the handle isn't created.
    async fn on_start(self: Arc<Self>) -> Result<(), LifecycleError>;

    /// Called once when a [`LifecycleHandle`] wrapping `self`
    /// is dropped. Implementations signal their background work
    /// to stop. Awaited by the handle's drop / shutdown path;
    /// implementations that need to wait for full teardown should
    /// hold a `JoinHandle` internally and await it here.
    async fn on_stop(&self);

    /// Liveness check polled by
    /// [`LifecycleGroup::health`](super::group::LifecycleGroup::health)
    /// and the auto-respawn monitor. Default: report healthy
    /// — daemons that have a heartbeat / tick / generation
    /// notion override to surface stuck loops to operators.
    ///
    /// `async` because some daemons may need to await an
    /// internal RwLock or query the runtime; most impls are
    /// fast and non-blocking.
    async fn health(&self) -> ReplicaHealth {
        ReplicaHealth::healthy()
    }
}

/// Lifecycle-trait error shape. Distinct from substrate-wide
/// `AdapterError` so trait implementors can carry typed
/// failures without pulling in cross-module dependencies.
#[derive(Debug)]
pub enum LifecycleError {
    /// `on_start` failed for a daemon-specific reason. Carries
    /// a free-form diagnostic string the lifecycle harness
    /// surfaces to the operator.
    StartFailed(String),
}

impl std::fmt::Display for LifecycleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StartFailed(msg) => write!(f, "start failed: {msg}"),
        }
    }
}

impl std::error::Error for LifecycleError {}

/// RAII handle that runs a [`LifecycleDaemon`]'s lifecycle.
/// Construction calls `on_start`; drop schedules `on_stop` on a
/// detached task so the synchronous Drop impl can fire-and-forget
/// the async shutdown.
///
/// For deterministic shutdown ordering (`net aggregator shutdown`
/// waiting on the loop to fully drain before returning), use
/// [`LifecycleHandle::stop`] instead of dropping.
pub struct LifecycleHandle {
    daemon: Arc<dyn LifecycleDaemon>,
    /// `Some` until `stop()` consumes the handle or Drop runs.
    /// Lets `stop()` move ownership without conflicting with
    /// Drop's fallback.
    daemon_for_drop: Option<Arc<dyn LifecycleDaemon>>,
}

impl LifecycleHandle {
    /// Construct a handle and run `on_start` synchronously
    /// against the async runtime. Errors abort — the handle is
    /// never created if start fails.
    pub async fn start(daemon: Arc<dyn LifecycleDaemon>) -> Result<Self, LifecycleError> {
        Arc::clone(&daemon).on_start().await?;
        Ok(Self {
            daemon: daemon.clone(),
            daemon_for_drop: Some(daemon),
        })
    }

    /// Borrow the underlying daemon for introspection. Operator
    /// tooling that wants type-erased access reads through this
    /// — the lifecycle handle alone doesn't expose concrete
    /// daemon state.
    pub fn daemon(&self) -> &Arc<dyn LifecycleDaemon> {
        &self.daemon
    }

    /// Shut the daemon down and await the teardown. Consumes
    /// the handle so a subsequent Drop doesn't double-stop.
    pub async fn stop(mut self) {
        let daemon = self.daemon_for_drop.take();
        if let Some(d) = daemon {
            d.on_stop().await;
        }
    }
}

impl Drop for LifecycleHandle {
    fn drop(&mut self) {
        if let Some(daemon) = self.daemon_for_drop.take() {
            match tokio::runtime::Handle::try_current() {
                Ok(handle) => {
                    handle.spawn(async move {
                        daemon.on_stop().await;
                    });
                }
                Err(_) => {
                    // No tokio runtime in scope (e.g. synchronous
                    // test teardown). The daemon's internal task
                    // is expected to clean itself up via its
                    // shutdown flag once its own `Arc` is dropped
                    // — but flag the skipped lifecycle hook so the
                    // contract is visible at operator log level.
                    tracing::warn!(
                        daemon = daemon.name(),
                        "LifecycleHandle dropped outside a tokio runtime; \
                         skipping on_stop. Daemon must self-clean via its \
                         shutdown flag.",
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU8, Ordering};

    struct CountingDaemon {
        starts: AtomicU8,
        stops: AtomicU8,
    }

    #[async_trait]
    impl LifecycleDaemon for CountingDaemon {
        fn name(&self) -> &str {
            "counting"
        }
        async fn on_start(self: Arc<Self>) -> Result<(), LifecycleError> {
            self.starts.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }
        async fn on_stop(&self) {
            self.stops.fetch_add(1, Ordering::AcqRel);
        }
    }

    #[tokio::test]
    async fn start_fires_on_start_exactly_once() {
        let daemon = Arc::new(CountingDaemon {
            starts: AtomicU8::new(0),
            stops: AtomicU8::new(0),
        });
        let handle = LifecycleHandle::start(daemon.clone()).await.expect("start");
        assert_eq!(daemon.starts.load(Ordering::Acquire), 1);
        assert_eq!(daemon.stops.load(Ordering::Acquire), 0);
        handle.stop().await;
        assert_eq!(daemon.stops.load(Ordering::Acquire), 1);
    }

    struct FailingStart;

    #[async_trait]
    impl LifecycleDaemon for FailingStart {
        fn name(&self) -> &str {
            "failing"
        }
        async fn on_start(self: Arc<Self>) -> Result<(), LifecycleError> {
            Err(LifecycleError::StartFailed("intentional".into()))
        }
        async fn on_stop(&self) {}
    }

    #[tokio::test]
    async fn start_failure_aborts_handle_creation() {
        let result = LifecycleHandle::start(Arc::new(FailingStart)).await;
        match result {
            Err(LifecycleError::StartFailed(msg)) => assert_eq!(msg, "intentional"),
            Ok(_) => panic!("expected StartFailed"),
        }
    }

    #[tokio::test]
    async fn drop_schedules_on_stop_under_tokio_runtime() {
        let daemon = Arc::new(CountingDaemon {
            starts: AtomicU8::new(0),
            stops: AtomicU8::new(0),
        });
        {
            let _handle = LifecycleHandle::start(daemon.clone()).await.expect("start");
            // Drop fires next.
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(daemon.stops.load(Ordering::Acquire), 1);
    }
}
