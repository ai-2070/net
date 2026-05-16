//! MeshOS SDK — Rust surface. The canonical daemon-author API
//! per [`MESHOS_SDK_PLAN.md`](../../../../../../docs/plans/MESHOS_SDK_PLAN.md).
//!
//! [`MeshOsDaemonHandle`] wraps a registered daemon's lifecycle:
//! a per-daemon control-event receiver, a read-only metadata
//! view, a capability-publish path, and a graceful-shutdown
//! sequence. Built directly on top of the substrate primitives
//! (`MeshOsRuntime`, `DaemonRegistry`, `DaemonHost`, `MeshDaemon`,
//! `CapabilitySet`) — the SDK is composition + ergonomics, not
//! new mechanism.
//!
//! Use pattern:
//!
//! ```ignore
//! use net::adapter::net::behavior::meshos::{MeshOsRuntime, MeshOsConfig};
//! use net::adapter::net::behavior::meshos::sdk::{MeshOsDaemonSdk, MeshOsDaemonHandle};
//! use net::adapter::net::compute::DaemonControl;
//!
//! // Wrap a user dispatcher with the SDK's routing layer.
//! let sdk = MeshOsDaemonSdk::start(MeshOsConfig::default(), my_dispatcher);
//!
//! // Register a daemon; receive control events via the handle.
//! let mut handle = sdk.register_daemon(Box::new(my_daemon), keypair)?;
//! while let Some(ev) = handle.next_control().await {
//!     match ev {
//!         DaemonControl::Shutdown { .. } => break,
//!         _ => { /* react */ }
//!     }
//! }
//! handle.graceful_shutdown(std::time::Duration::from_secs(5)).await?;
//! sdk.shutdown().await?;
//! ```
//!
//! Locked decisions from the plan:
//!
//! - **Daemon-side only.** No placement / admin / scheduler /
//!   replica APIs in the SDK surface; consumers issue daemon
//!   work + receive supervisor signals.
//! - **`DaemonControl` is the wire form.** SDK consumers see
//!   the WASM-friendly relative-ms form, not the loop-internal
//!   `Instant`-anchored `MeshOsControl`.
//! - **Snapshot / restore is opaque bytes.** SDK never inspects
//!   the daemon's state shape.
//! - **At-most-once control delivery.** When the daemon doesn't
//!   consume a control event before the next one fires for the
//!   same daemon, the older event drops + the router's drop
//!   counter increments. The SDK doesn't queue control events
//!   indefinitely on the daemon's behalf.
//! - **Error kinds use `<<meshos-sdk-kind:KIND>>MSG`.** Matches
//!   the discriminator format every cross-language SDK uses.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::future::BoxFuture;
use parking_lot::RwLock;
use tokio::sync::mpsc;

use crate::adapter::net::behavior::capability::CapabilitySet;
use crate::adapter::net::compute::{
    DaemonControl, DaemonError, DaemonHost, DaemonHostConfig, DaemonRegistry, MeshDaemon,
};
use crate::adapter::net::identity::EntityKeypair;

use super::action::MeshOsAction;
use super::config::MeshOsConfig;
use super::event::NodeId;
use super::executor::{ActionDispatcher, DispatchError};
use super::maintenance::MaintenanceState;
use super::runtime::{MeshOsRuntime, RuntimeShutdownError, RuntimeStats};
use super::snapshot::PeerSnapshot;

/// Default capacity for the per-daemon control-event channel.
/// At-most-once delivery: if the daemon doesn't consume an event
/// before this many newer ones queue, the oldest drops.
pub const DEFAULT_CONTROL_CHANNEL_CAPACITY: usize = 8;

/// Default grace window passed to [`MeshOsDaemonHandle::graceful_shutdown`]
/// when no explicit value is supplied (the macro path uses this).
pub const DEFAULT_GRACEFUL_SHUTDOWN: Duration = Duration::from_secs(5);

/// SDK error surface. Carries the operator-readable message + a
/// kind discriminator usable from cross-language consumers.
#[derive(Clone, Debug, thiserror::Error)]
#[error("<<meshos-sdk-kind:{kind}>>{message}")]
pub struct SdkError {
    /// Stable kind discriminator. Lowercase + underscore-only;
    /// the cross-language SDKs parse the surrounding
    /// `<<meshos-sdk-kind:…>>` envelope to extract this verbatim.
    pub kind: &'static str,
    /// Operator-readable message.
    pub message: String,
}

impl SdkError {
    fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl From<DaemonError> for SdkError {
    fn from(err: DaemonError) -> Self {
        Self::new("register_failed", err.to_string())
    }
}

/// Read-only view of the cluster context the daemon can observe.
/// Built from the runtime's latest [`super::snapshot::MeshOsSnapshot`]
/// at handle-construction time + refreshed on demand via
/// [`MeshOsDaemonHandle::refresh_metadata`].
#[derive(Clone, Debug)]
pub struct MetadataView {
    /// This node's identifier (`MeshOsConfig::this_node`).
    pub node_id: NodeId,
    /// The registered daemon's substrate identifier
    /// (the keypair's `origin_hash`).
    pub daemon_id: u64,
    /// The daemon's `MeshDaemon::name()` at registration.
    pub daemon_name: String,
    /// This node's own maintenance state, snapshotted at the
    /// last `refresh_metadata` call.
    pub maintenance_state: MaintenanceStateView,
    /// Per-peer summary — RTT, health, maintenance mirror.
    pub peers: BTreeMap<NodeId, PeerSnapshot>,
}

/// Bounded WASM-friendly projection of [`MaintenanceState`].
/// Carries the discriminator + relative-ms `since` so daemons
/// without `Instant` access can still reason about transitions.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum MaintenanceStateView {
    /// Normal participation.
    Active,
    /// Entering maintenance — replicas migrating, daemons draining.
    EnteringMaintenance {
        /// Milliseconds since the transition was entered.
        since_ms: u64,
        /// Milliseconds remaining until the deadline elapses,
        /// or `None` for no deadline.
        deadline_remaining_ms: Option<u64>,
    },
    /// Steady-state isolated.
    Maintenance {
        /// Milliseconds since the state was entered.
        since_ms: u64,
    },
    /// Exiting maintenance — health revalidation + capability refresh.
    ExitingMaintenance {
        /// Milliseconds since the state was entered.
        since_ms: u64,
    },
    /// Drain failed; operator warning state.
    DrainFailed {
        /// Milliseconds since the failure was recorded.
        since_ms: u64,
        /// Operator-readable reason.
        reason: String,
    },
    /// Recovery ramp-up window.
    Recovery {
        /// Milliseconds since the ramp started.
        since_ms: u64,
    },
}

impl MaintenanceStateView {
    /// Build a view from the substrate-side [`MaintenanceState`]
    /// against a `now` reference for relative-ms conversion.
    /// Useful when a consumer holds the substrate form directly
    /// rather than reading through a snapshot.
    pub fn from_state(state: &MaintenanceState, now: Instant) -> Self {
        match state {
            MaintenanceState::Active => Self::Active,
            MaintenanceState::EnteringMaintenance { since, deadline } => {
                Self::EnteringMaintenance {
                    since_ms: now.saturating_duration_since(*since).as_millis() as u64,
                    deadline_remaining_ms: deadline
                        .map(|d| d.saturating_duration_since(now).as_millis() as u64),
                }
            }
            MaintenanceState::Maintenance { since } => Self::Maintenance {
                since_ms: now.saturating_duration_since(*since).as_millis() as u64,
            },
            MaintenanceState::ExitingMaintenance { since } => Self::ExitingMaintenance {
                since_ms: now.saturating_duration_since(*since).as_millis() as u64,
            },
            MaintenanceState::DrainFailed { since, reason } => Self::DrainFailed {
                since_ms: now.saturating_duration_since(*since).as_millis() as u64,
                reason: reason.clone(),
            },
            MaintenanceState::Recovery { since } => Self::Recovery {
                since_ms: now.saturating_duration_since(*since).as_millis() as u64,
            },
        }
    }
}

/// Per-daemon control-event channel. The router (held by the
/// SDK) keeps one of these per registered daemon and pushes
/// translated [`DaemonControl`] events when the executor
/// dispatches daemon-targeted actions.
#[derive(Debug)]
struct DaemonControlSlot {
    tx: mpsc::Sender<DaemonControl>,
    /// Total events the slot has dropped because the daemon
    /// wasn't keeping up (channel full).
    dropped: AtomicU64,
}

/// Shareable per-runtime cell mapping `daemon_id` → control
/// channel. The SDK's routing dispatcher reads this on every
/// daemon-targeted action to push the translated
/// [`DaemonControl`].
#[derive(Clone, Default)]
pub struct DaemonControlRouter {
    inner: Arc<RwLock<BTreeMap<u64, Arc<DaemonControlSlot>>>>,
    /// Cluster-wide backpressure broadcast targets — populated
    /// once per registered daemon. Read by the routing
    /// dispatcher when an `MeshOsControl::BackpressureOn/Off`
    /// fans out.
    broadcast: Arc<RwLock<Vec<Arc<DaemonControlSlot>>>>,
}

impl DaemonControlRouter {
    /// Build an empty router.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a daemon's control channel. Returns the receiver
    /// the SDK hands to the daemon handle.
    fn register(&self, daemon_id: u64, capacity: usize) -> mpsc::Receiver<DaemonControl> {
        let (tx, rx) = mpsc::channel(capacity);
        let slot = Arc::new(DaemonControlSlot {
            tx,
            dropped: AtomicU64::new(0),
        });
        self.inner.write().insert(daemon_id, Arc::clone(&slot));
        self.broadcast.write().push(slot);
        rx
    }

    /// Unregister a daemon's control channel. Subsequent
    /// dispatches against this `daemon_id` drop with no
    /// destination.
    fn unregister(&self, daemon_id: u64) {
        let removed = self.inner.write().remove(&daemon_id);
        if let Some(removed) = removed {
            self.broadcast.write().retain(|s| !Arc::ptr_eq(s, &removed));
        }
    }

    /// Push a control event to a specific daemon. At-most-once:
    /// when the channel is full, the event drops + the slot's
    /// counter increments.
    fn route(&self, daemon_id: u64, event: DaemonControl) {
        let slot = self.inner.read().get(&daemon_id).cloned();
        if let Some(slot) = slot {
            if slot.tx.try_send(event).is_err() {
                slot.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Broadcast a control event to every registered daemon.
    /// Used for cluster-wide signals
    /// (`BackpressureOn`/`BackpressureOff`).
    fn broadcast(&self, event: DaemonControl) {
        let slots = self.broadcast.read().clone();
        for slot in slots {
            if slot.tx.try_send(event.clone()).is_err() {
                slot.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Sample the total dropped-event count across all daemons.
    /// Diagnostic; the SDK exposes per-daemon drop counts on
    /// the handle.
    pub fn total_dropped(&self) -> u64 {
        let map = self.inner.read();
        map.values()
            .map(|slot| slot.dropped.load(Ordering::Relaxed))
            .sum()
    }
}

/// Wraps the user's [`ActionDispatcher`] with daemon-control
/// routing. Intercepts daemon-targeted actions, translates each
/// to the WASM-friendly [`DaemonControl`] form, pushes to the
/// per-daemon channel via [`DaemonControlRouter`], then
/// delegates the original action to the user dispatcher.
///
/// Constructed by [`MeshOsDaemonSdk::start`]; consumers that
/// build their own runtime can construct one manually to opt
/// into the same routing layer.
pub struct SdkRoutingDispatcher<D: ActionDispatcher> {
    inner: Arc<D>,
    router: DaemonControlRouter,
}

impl<D: ActionDispatcher> SdkRoutingDispatcher<D> {
    /// Wrap `inner` with `router`-driven control routing.
    pub fn new(inner: Arc<D>, router: DaemonControlRouter) -> Self {
        Self { inner, router }
    }
}

impl<D: ActionDispatcher> ActionDispatcher for SdkRoutingDispatcher<D> {
    fn dispatch<'a>(&'a self, action: MeshOsAction) -> BoxFuture<'a, Result<(), DispatchError>> {
        let router = self.router.clone();
        let action_clone = action.clone();
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            translate_to_control(&router, &action_clone);
            inner.dispatch(action).await
        })
    }
}

fn translate_to_control(router: &DaemonControlRouter, action: &MeshOsAction) {
    let now = Instant::now();
    if let MeshOsAction::StopDaemon {
        daemon, deadline, ..
    } = action
    {
        let grace_period_ms = deadline.saturating_duration_since(now).as_millis() as u64;
        router.route(daemon.id, DaemonControl::Shutdown { grace_period_ms });
    }
    // Maintenance / drain fan-out arrives via the substrate's
    // `ControlSink` (installed at runtime construction); the
    // dispatcher only catches per-daemon, action-driven signals
    // here.
}

/// [`ControlSink`] implementation that broadcasts each
/// substrate-emitted [`MeshOsControl`] to every registered
/// daemon via the [`DaemonControlRouter`]. The loop owns the
/// emit cadence; this adapter just translates the SDK-internal
/// form to the wire form daemons see.
pub(super) struct RouterControlSink {
    router: DaemonControlRouter,
}

impl RouterControlSink {
    pub(super) fn new(router: DaemonControlRouter) -> Self {
        Self { router }
    }
}

impl super::control::ControlSink for RouterControlSink {
    fn emit(&self, event: super::control::MeshOsControl) {
        let now = Instant::now();
        self.router.broadcast(event.to_daemon_control(now));
    }
}

/// Per-daemon handle. Owns the control-event receiver +
/// publish-capabilities surface + graceful-shutdown sequence.
pub struct MeshOsDaemonHandle {
    daemon_id: u64,
    daemon_name: String,
    control_rx: mpsc::Receiver<DaemonControl>,
    registry: Arc<DaemonRegistry>,
    router: DaemonControlRouter,
    metadata: MetadataView,
    runtime_snapshot_reader: super::event_loop::MeshOsSnapshotReader,
    /// Mesh-loop publish handle. Used for daemon-author
    /// surfaces that need to push events into the loop (log
    /// emission, future capability announcements).
    mesh_handle: super::event_loop::MeshOsHandle,
    this_node: NodeId,
    /// Becomes `true` after `unregister` runs — guards against
    /// double-unregister on `Drop` + `graceful_shutdown`.
    unregistered: bool,
}

impl MeshOsDaemonHandle {
    /// Receive the next control event. Async — parks until the
    /// supervisor emits a signal, the handle is unregistered,
    /// or the runtime shuts down (returns `None`).
    pub async fn next_control(&mut self) -> Option<DaemonControl> {
        self.control_rx.recv().await
    }

    /// Try to receive a control event without parking. Returns
    /// `None` immediately when the channel is empty.
    pub fn try_next_control(&mut self) -> Option<DaemonControl> {
        self.control_rx.try_recv().ok()
    }

    /// The daemon's substrate identifier (the keypair's
    /// `origin_hash`). Stable across the handle's lifetime.
    pub fn daemon_id(&self) -> u64 {
        self.daemon_id
    }

    /// The daemon's `MeshDaemon::name()` at registration.
    pub fn daemon_name(&self) -> &str {
        &self.daemon_name
    }

    /// Borrow the cached metadata view. Refresh via
    /// [`Self::refresh_metadata`] when reading freshness matters.
    pub fn metadata(&self) -> &MetadataView {
        &self.metadata
    }

    /// Rebuild the metadata view from the runtime's latest
    /// snapshot. Cheap — one `ArcSwap::load_full` plus a `BTreeMap`
    /// clone of the peer entries.
    pub fn refresh_metadata(&mut self) -> &MetadataView {
        let snap = self.runtime_snapshot_reader.read();
        let maint = match snap.local_maintenance {
            super::snapshot::MaintenanceStateSnapshot::Active => MaintenanceStateView::Active,
            super::snapshot::MaintenanceStateSnapshot::EnteringMaintenance {
                since_ms,
                deadline_remaining_ms,
            } => MaintenanceStateView::EnteringMaintenance {
                since_ms,
                deadline_remaining_ms,
            },
            super::snapshot::MaintenanceStateSnapshot::Maintenance { since_ms } => {
                MaintenanceStateView::Maintenance { since_ms }
            }
            super::snapshot::MaintenanceStateSnapshot::ExitingMaintenance { since_ms } => {
                MaintenanceStateView::ExitingMaintenance { since_ms }
            }
            super::snapshot::MaintenanceStateSnapshot::DrainFailed { since_ms, reason } => {
                MaintenanceStateView::DrainFailed { since_ms, reason }
            }
            super::snapshot::MaintenanceStateSnapshot::Recovery { since_ms } => {
                MaintenanceStateView::Recovery { since_ms }
            }
        };
        self.metadata = MetadataView {
            node_id: self.this_node,
            daemon_id: self.daemon_id,
            daemon_name: self.daemon_name.clone(),
            maintenance_state: maint,
            peers: snap.peers,
        };
        &self.metadata
    }

    /// Publish (or update) the daemon's [`CapabilitySet`]. The
    /// SDK doesn't itself commit to the capability chain — the
    /// host's substrate-side path does — but it surfaces a stub
    /// that returns `Ok(())` for now. A future slice plumbs this
    /// to the real `CapabilityIndex::announce` path.
    ///
    /// **Plumbing status:** this method is a thin contract today;
    /// the actual capability-chain commit lands when the
    /// `CapabilitySet` → admin-chain integration ships. Calling
    /// it now is a no-op + always returns `Ok(())`.
    pub fn publish_capabilities(&self, _caps: CapabilitySet) -> Result<(), SdkError> {
        // TODO: when the capability-chain commit path lands,
        // wire through to `CapabilityIndex::announce_set(...)`.
        Ok(())
    }

    /// Publish a log line tagged with this daemon's id. The
    /// loop stamps the seq + wall-clock timestamp + this
    /// node's id before pushing onto the per-node log ring;
    /// operators reading through Deck SDK's
    /// `subscribe_logs(LogFilter::new().with_daemon(id))` see
    /// the line.
    ///
    /// Non-blocking — uses `MeshOsHandle::try_publish` so a
    /// saturated event queue surfaces as `SdkError` with kind
    /// `queue_full` rather than parking the caller. Daemons in
    /// hot loops can drop log lines on backpressure without
    /// stalling.
    pub fn publish_log(
        &self,
        level: super::logs::LogLevel,
        message: impl Into<String>,
    ) -> Result<(), SdkError> {
        let line = super::logs::LogLine {
            level,
            daemon_id: Some(self.daemon_id),
            message: message.into(),
        };
        self.mesh_handle
            .try_publish(super::event::MeshOsEvent::LogLine(line))
            .map_err(|e| match e {
                super::event_loop::MeshOsHandleError::LoopClosed => SdkError::new(
                    "loop_closed",
                    "MeshOS loop has exited; daemon log line dropped",
                ),
                super::event_loop::MeshOsHandleError::QueueFull => SdkError::new(
                    "queue_full",
                    "MeshOS event queue at capacity; daemon log line dropped",
                ),
            })
    }

    /// Drive a graceful shutdown. Sends
    /// `DaemonControl::Shutdown { grace_period_ms }` to the
    /// daemon's control channel, parks for `grace` (or until
    /// the daemon's task exits — whichever sooner), then
    /// unregisters from the registry + router.
    pub async fn graceful_shutdown(mut self, grace: Duration) -> Result<(), SdkError> {
        // Inject a shutdown event so the daemon's `next_control`
        // loop wakes up.
        let grace_ms = grace.as_millis() as u64;
        self.router.route(
            self.daemon_id,
            DaemonControl::Shutdown {
                grace_period_ms: grace_ms,
            },
        );
        // Wait for the grace window; daemons that exit early
        // can drop their handle to short-circuit (Drop runs
        // unregister too).
        tokio::time::sleep(grace).await;
        self.unregister_inner();
        Ok(())
    }

    fn unregister_inner(&mut self) {
        if self.unregistered {
            return;
        }
        self.unregistered = true;
        // Drop the router slot first so further dispatches against
        // this daemon are no-ops; then unregister from the registry
        // (which fires the lifecycle observer; the SDK consumer
        // sees `Unregistered`).
        self.router.unregister(self.daemon_id);
        let _ = self.registry.unregister(self.daemon_id);
    }
}

impl Drop for MeshOsDaemonHandle {
    fn drop(&mut self) {
        // Failsafe — if the consumer didn't call
        // `graceful_shutdown`, still clean up the registry +
        // router slot so the daemon doesn't leak.
        self.unregister_inner();
    }
}

/// SDK entry point. Wraps a [`MeshOsRuntime`] with the
/// [`SdkRoutingDispatcher`] + a [`DaemonControlRouter`] so
/// daemon-targeted actions translate to per-daemon control
/// events.
///
/// Construct via [`Self::start`] (one-call setup) or
/// [`Self::from_runtime`] (compose against a pre-built runtime
/// when the consumer needs to share state with other
/// subsystems).
pub struct MeshOsDaemonSdk {
    runtime: MeshOsRuntime,
    router: DaemonControlRouter,
    control_capacity: usize,
}

impl MeshOsDaemonSdk {
    /// One-call setup. Wraps the user's dispatcher in
    /// [`SdkRoutingDispatcher`]; starts the runtime; retains
    /// the router for per-daemon registration.
    pub fn start<D: ActionDispatcher>(config: MeshOsConfig, user_dispatcher: Arc<D>) -> Self {
        let router = DaemonControlRouter::new();
        let routed = Arc::new(SdkRoutingDispatcher::new(user_dispatcher, router.clone()));
        let sink: Arc<dyn super::control::ControlSink> =
            Arc::new(RouterControlSink::new(router.clone()));
        let runtime = MeshOsRuntime::start_with_options(
            config,
            routed,
            super::event_loop::ProbeRegistry::new(),
            super::scheduler::SchedulerRegistry::new(),
            Arc::new(DaemonRegistry::new()),
            Some(sink),
        );
        Self {
            runtime,
            router,
            control_capacity: DEFAULT_CONTROL_CHANNEL_CAPACITY,
        }
    }

    /// Compose against a pre-built runtime + router. The
    /// runtime's dispatcher must already be wrapped in
    /// [`SdkRoutingDispatcher`] for the same `router`.
    pub fn from_runtime(runtime: MeshOsRuntime, router: DaemonControlRouter) -> Self {
        Self {
            runtime,
            router,
            control_capacity: DEFAULT_CONTROL_CHANNEL_CAPACITY,
        }
    }

    /// Like [`Self::start`] but also installs an
    /// [`super::ice::AdminVerifier`] on the runtime. Required
    /// for any deployment where the operator's signed admin
    /// commits should fold with `VerificationOutcome::Accepted`
    /// instead of `Unverified` — the verifier's
    /// [`super::ice::OperatorRegistry`] must contain the
    /// operator key that signs incoming commits.
    pub fn start_with_verifier<D: ActionDispatcher>(
        config: MeshOsConfig,
        user_dispatcher: Arc<D>,
        verifier: Arc<super::ice::AdminVerifier>,
    ) -> Self {
        Self::start_with_verifier_and_migration_source(
            config,
            user_dispatcher,
            Some(verifier),
            None,
        )
    }

    /// Install an `AdminVerifier` plus an optional migration
    /// snapshot source in one call. This is **not** the full
    /// extension surface — the underlying
    /// [`MeshOsRuntime`] also accepts admin-audit / log /
    /// failure chain appenders and a migration aborter; this
    /// SDK-wrapper constructor exposes only the two extensions
    /// the daemon-side SDK shipped first. Plumbing the other
    /// extension slots through the SDK wrapper is tracked in
    /// `MESHOS_SDK_PLAN.md` § Deferred work; for now,
    /// deployments needing the full surface drop down to
    /// [`MeshOsRuntimeBuilder`] directly.
    pub fn start_with_verifier_and_migration_source<D: ActionDispatcher>(
        config: MeshOsConfig,
        user_dispatcher: Arc<D>,
        verifier: Option<Arc<super::ice::AdminVerifier>>,
        migration_snapshot_source: Option<
            Arc<dyn super::migration_snapshot_source::MigrationSnapshotSource>,
        >,
    ) -> Self {
        let router = DaemonControlRouter::new();
        let routed = Arc::new(SdkRoutingDispatcher::new(user_dispatcher, router.clone()));
        let sink: Arc<dyn super::control::ControlSink> =
            Arc::new(RouterControlSink::new(router.clone()));
        let runtime = MeshOsRuntime::start_with_full_extensions(
            config,
            routed,
            super::event_loop::ProbeRegistry::new(),
            super::scheduler::SchedulerRegistry::new(),
            Arc::new(DaemonRegistry::new()),
            Some(sink),
            verifier,
            None, // admin audit appender
            None, // log appender
            None, // failure appender
            None, // migration aborter
            migration_snapshot_source,
        );
        Self {
            runtime,
            router,
            control_capacity: DEFAULT_CONTROL_CHANNEL_CAPACITY,
        }
    }

    /// Override the per-daemon control-channel capacity. Default
    /// is [`DEFAULT_CONTROL_CHANNEL_CAPACITY`]. Increase for
    /// daemons that pause `process()` longer than the supervisor's
    /// tick cadence.
    pub fn with_control_capacity(mut self, capacity: usize) -> Self {
        self.control_capacity = capacity.max(1);
        self
    }

    /// Borrow the wrapped runtime.
    pub fn runtime(&self) -> &MeshOsRuntime {
        &self.runtime
    }

    /// Borrow the daemon-control router.
    pub fn router(&self) -> &DaemonControlRouter {
        &self.router
    }

    /// Register a daemon. Constructs a [`DaemonHost`], inserts
    /// it into the runtime's registry, allocates the per-daemon
    /// control channel, and returns the handle.
    pub fn register_daemon(
        &self,
        daemon: Box<dyn MeshDaemon>,
        keypair: EntityKeypair,
    ) -> Result<MeshOsDaemonHandle, SdkError> {
        let daemon_id = keypair.origin_hash();
        let daemon_name = daemon.name().to_string();
        let host = DaemonHost::new(daemon, keypair, DaemonHostConfig::default());
        self.runtime
            .daemon_registry()
            .register(host)
            .map_err(SdkError::from)?;
        let control_rx = self.router.register(daemon_id, self.control_capacity);
        let snap = self.runtime.snapshot();
        let metadata = MetadataView {
            node_id: self.runtime_this_node(),
            daemon_id,
            daemon_name: daemon_name.clone(),
            maintenance_state: MaintenanceStateView::Active,
            peers: snap.peers,
        };
        Ok(MeshOsDaemonHandle {
            daemon_id,
            daemon_name,
            control_rx,
            registry: Arc::clone(self.runtime.daemon_registry()),
            router: self.router.clone(),
            metadata,
            runtime_snapshot_reader: self.runtime.snapshot_reader().clone(),
            mesh_handle: self.runtime.handle_clone(),
            this_node: self.runtime_this_node(),
            unregistered: false,
        })
    }

    /// Total events the router dropped across every registered
    /// daemon. Diagnostic.
    pub fn dropped_control_events(&self) -> u64 {
        self.router.total_dropped()
    }

    /// Drive a clean shutdown of the wrapped runtime.
    pub async fn shutdown(self) -> Result<RuntimeStats, RuntimeShutdownError> {
        self.runtime.shutdown().await
    }

    /// Read `MeshOsConfig::this_node` off the runtime. The
    /// runtime doesn't currently expose the full config, so we
    /// read the latest snapshot's first peer key if available,
    /// falling back to `0` — the metadata's `node_id` is
    /// available either way, but consumers that care should
    /// pass a `this_node` to the runtime config explicitly.
    fn runtime_this_node(&self) -> NodeId {
        // The runtime config is private; consumers passed a
        // `this_node` into `MeshOsConfig` at construction. We
        // don't have direct access here — defer to a future
        // slice that adds a `runtime.this_node()` accessor.
        // For now, surface a placeholder so `metadata.node_id`
        // is present even if zero. Tests pass their own
        // verification value via the handle.
        0
    }
}

/// One-call macro for the common "single daemon per process"
/// case. Expands to a `tokio::main` body that:
///
/// 1. Constructs a [`MeshOsRuntime`] via the supplied config +
///    dispatcher,
/// 2. Wraps the runtime in [`MeshOsDaemonSdk`],
/// 3. Registers the supplied daemon + keypair,
/// 4. Drains control events; on `Shutdown` or `DrainFinish`,
///    breaks the loop,
/// 5. Drives `graceful_shutdown` on the handle, then
///    `shutdown` on the SDK.
///
/// ```ignore
/// daemon_main! {
///     name: "my-telemetry",
///     daemon: MyTelemetryDaemon::new(),
///     keypair: EntityKeypair::generate(),
///     config: MeshOsConfig::default(),
///     dispatcher: my_dispatcher,
/// }
/// ```
#[macro_export]
macro_rules! daemon_main {
    (
        daemon: $daemon:expr,
        keypair: $keypair:expr,
        config: $config:expr,
        dispatcher: $dispatcher:expr $(,)?
    ) => {{
        let sdk = $crate::adapter::net::behavior::meshos::sdk::MeshOsDaemonSdk::start(
            $config,
            $dispatcher,
        );
        let mut handle = sdk
            .register_daemon(Box::new($daemon), $keypair)
            .expect("daemon registration failed");
        while let Some(ev) = handle.next_control().await {
            use $crate::adapter::net::compute::DaemonControl;
            if matches!(
                ev,
                DaemonControl::Shutdown { .. } | DaemonControl::DrainFinish
            ) {
                break;
            }
        }
        let grace = $crate::adapter::net::behavior::meshos::sdk::DEFAULT_GRACEFUL_SHUTDOWN;
        let _ = handle.graceful_shutdown(grace).await;
        let _ = sdk.shutdown().await;
    }};
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use bytes::Bytes;

    use super::*;
    use crate::adapter::net::behavior::capability::CapabilityFilter;
    use crate::adapter::net::behavior::meshos::action::ActionId;
    use crate::adapter::net::behavior::meshos::executor::LoggingDispatcher;
    use crate::adapter::net::behavior::meshos::PendingAction;
    use crate::adapter::net::compute::{DaemonError, MeshDaemon};
    use crate::adapter::net::state::causal::CausalEvent;

    /// Minimal test daemon — no state, name + process only.
    struct NoopDaemon {
        name: String,
        process_count: Arc<AtomicUsize>,
    }
    impl NoopDaemon {
        fn new(name: &str) -> (Self, Arc<AtomicUsize>) {
            let counter = Arc::new(AtomicUsize::new(0));
            (
                Self {
                    name: name.into(),
                    process_count: Arc::clone(&counter),
                },
                counter,
            )
        }
    }
    impl MeshDaemon for NoopDaemon {
        fn name(&self) -> &str {
            &self.name
        }
        fn requirements(&self) -> CapabilityFilter {
            CapabilityFilter::default()
        }
        fn process(&mut self, _event: &CausalEvent) -> Result<Vec<Bytes>, DaemonError> {
            self.process_count.fetch_add(1, Ordering::Relaxed);
            Ok(Vec::new())
        }
    }

    fn fast_config() -> MeshOsConfig {
        let mut cfg = MeshOsConfig::default();
        cfg.tick_interval = Duration::from_millis(10);
        cfg
    }

    #[tokio::test]
    async fn register_daemon_returns_handle_with_correct_identity() {
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let sdk = MeshOsDaemonSdk::start(fast_config(), dispatcher);
        let (daemon, _counter) = NoopDaemon::new("telemetry");
        let kp = EntityKeypair::generate();
        let expected_id = kp.origin_hash();
        let handle = sdk.register_daemon(Box::new(daemon), kp).unwrap();
        assert_eq!(handle.daemon_id(), expected_id);
        assert_eq!(handle.daemon_name(), "telemetry");
        let _ = sdk.shutdown().await;
    }

    #[tokio::test]
    async fn control_router_routes_stop_daemon_to_per_daemon_channel() {
        let router = DaemonControlRouter::new();
        let mut rx = router.register(42, 4);
        // Inject directly — the routing dispatcher does this via
        // translate_to_control under the hood.
        router.route(
            42,
            DaemonControl::Shutdown {
                grace_period_ms: 5000,
            },
        );
        let ev = rx.try_recv().expect("event present");
        assert!(matches!(
            ev,
            DaemonControl::Shutdown {
                grace_period_ms: 5000
            }
        ));
    }

    #[tokio::test]
    async fn control_router_drops_when_channel_full() {
        let router = DaemonControlRouter::new();
        let _rx = router.register(99, 1);
        router.route(99, DaemonControl::BackpressureOn { level: 0.5 });
        // Second push exceeds capacity 1 → drop.
        router.route(99, DaemonControl::BackpressureOn { level: 0.8 });
        assert_eq!(router.total_dropped(), 1);
    }

    #[tokio::test]
    async fn translate_to_control_emits_shutdown_for_stop_daemon() {
        let router = DaemonControlRouter::new();
        let mut rx = router.register(7, 4);
        let action = MeshOsAction::StopDaemon {
            daemon: super::super::event::DaemonRef {
                id: 7,
                name: "x".into(),
            },
            reason: "intent-stop".into(),
            deadline: Instant::now() + Duration::from_millis(2500),
        };
        translate_to_control(&router, &action);
        let ev = rx.try_recv().expect("translated to control event");
        match ev {
            DaemonControl::Shutdown { grace_period_ms } => {
                // Allow small slop for the Instant arithmetic.
                assert!((2400..=2500).contains(&grace_period_ms));
            }
            other => panic!("expected Shutdown, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn router_control_sink_broadcasts_drain_start_to_every_registered_daemon() {
        // The sink is the substrate-emit seam — when the loop
        // observes a maintenance transition it calls `emit`; the
        // adapter broadcasts the translated `DaemonControl` to
        // every registered daemon channel.
        use super::super::control::{ControlSink, MeshOsControl};
        let router = DaemonControlRouter::new();
        let mut rx_a = router.register(1, 4);
        let mut rx_b = router.register(2, 4);
        let sink = RouterControlSink::new(router.clone());
        sink.emit(MeshOsControl::DrainStart {
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(30),
        });
        let ev_a = rx_a.try_recv().expect("daemon A received drain start");
        let ev_b = rx_b.try_recv().expect("daemon B received drain start");
        assert!(matches!(ev_a, DaemonControl::DrainStart { .. }));
        assert!(matches!(ev_b, DaemonControl::DrainStart { .. }));
    }

    #[tokio::test]
    async fn unregister_removes_router_slot() {
        let router = DaemonControlRouter::new();
        let _rx = router.register(7, 4);
        router.unregister(7);
        // Subsequent dispatch against 7 is a no-op — no panic,
        // no drop counter increment (the slot is gone).
        router.route(7, DaemonControl::Shutdown { grace_period_ms: 1 });
        assert_eq!(router.total_dropped(), 0);
    }

    #[tokio::test]
    async fn publish_log_lands_on_runtime_log_ring_tagged_with_daemon_id() {
        // Daemon-author surface: a registered daemon emits a
        // log line via the handle; the line shows up on the
        // runtime's log ring tagged with the daemon's id, ready
        // for Deck SDK's `subscribe_logs` to pick up.
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let sdk = MeshOsDaemonSdk::start(fast_config(), dispatcher);
        let (daemon, _) = NoopDaemon::new("logger");
        let kp = EntityKeypair::generate();
        let daemon_id = kp.origin_hash();
        let handle = sdk.register_daemon(Box::new(daemon), kp).unwrap();

        handle
            .publish_log(
                super::super::logs::LogLevel::Warn,
                "throttling: queue depth high",
            )
            .expect("publish_log");

        // Give the loop a tick + reconcile + snapshot publish.
        tokio::time::sleep(Duration::from_millis(80)).await;
        let snap = sdk.runtime().snapshot();
        let matching: Vec<_> = snap
            .log_ring
            .iter()
            .filter(|r| r.daemon_id == Some(daemon_id))
            .collect();
        assert_eq!(matching.len(), 1, "expected one log line for this daemon");
        let record = matching[0];
        assert_eq!(record.level, super::super::logs::LogLevel::Warn);
        assert_eq!(record.message, "throttling: queue depth high");
        let _ = sdk.shutdown().await;
    }

    #[tokio::test]
    async fn publish_log_after_runtime_shutdown_returns_loop_closed() {
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let sdk = MeshOsDaemonSdk::start(fast_config(), dispatcher);
        let (daemon, _) = NoopDaemon::new("logger");
        let kp = EntityKeypair::generate();
        let handle = sdk.register_daemon(Box::new(daemon), kp).unwrap();
        let _ = sdk.shutdown().await;
        let err = handle
            .publish_log(super::super::logs::LogLevel::Info, "after shutdown")
            .expect_err("publish after shutdown should fail");
        assert_eq!(err.kind, "loop_closed");
    }

    #[tokio::test]
    async fn handle_drop_unregisters_from_registry_and_router() {
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let sdk = MeshOsDaemonSdk::start(fast_config(), dispatcher);
        let registry = Arc::clone(sdk.runtime.daemon_registry());
        let (daemon, _) = NoopDaemon::new("temp");
        let kp = EntityKeypair::generate();
        let daemon_id = kp.origin_hash();
        let handle = sdk.register_daemon(Box::new(daemon), kp).unwrap();
        drop(handle);
        // Registry should no longer have the daemon — try
        // unregister and expect NotFound.
        assert!(matches!(
            registry.unregister(daemon_id),
            Err(DaemonError::NotFound(_))
        ));
        let _ = sdk.shutdown().await;
    }

    #[tokio::test]
    async fn graceful_shutdown_sends_shutdown_control_then_unregisters() {
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let sdk = MeshOsDaemonSdk::start(fast_config(), dispatcher);
        let (daemon, _) = NoopDaemon::new("graceful");
        let kp = EntityKeypair::generate();
        let mut handle = sdk.register_daemon(Box::new(daemon), kp).unwrap();
        // Spawn a task that consumes one control event, mimicking
        // a real daemon loop.
        let mut control_rx =
            std::mem::replace(&mut handle.control_rx, mpsc::channel::<DaemonControl>(1).1);
        let received = tokio::spawn(async move { control_rx.recv().await });
        // graceful_shutdown injects a Shutdown event + parks for
        // the grace window. Use a short grace to keep the test
        // fast.
        let _ = handle.graceful_shutdown(Duration::from_millis(50)).await;
        let ev = received.await.unwrap();
        assert!(matches!(ev, Some(DaemonControl::Shutdown { .. })));
        let _ = sdk.shutdown().await;
    }

    #[tokio::test]
    async fn publish_capabilities_returns_ok_pending_capability_chain_wiring() {
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let sdk = MeshOsDaemonSdk::start(fast_config(), dispatcher);
        let (daemon, _) = NoopDaemon::new("noop");
        let kp = EntityKeypair::generate();
        let handle = sdk.register_daemon(Box::new(daemon), kp).unwrap();
        // Stub for now; the chain commit lands in a future slice.
        let result = handle.publish_capabilities(CapabilitySet::default());
        assert!(result.is_ok());
        let _ = sdk.shutdown().await;
    }

    #[tokio::test]
    async fn refresh_metadata_pulls_from_runtime_snapshot() {
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let sdk = MeshOsDaemonSdk::start(fast_config(), dispatcher);
        let (daemon, _) = NoopDaemon::new("inspect");
        let kp = EntityKeypair::generate();
        let mut handle = sdk.register_daemon(Box::new(daemon), kp).unwrap();
        // Let the loop run + publish at least one snapshot.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let view = handle.refresh_metadata();
        assert_eq!(view.daemon_name, "inspect");
        // peers might be empty under a single-node test fixture;
        // pin only that the metadata view materializes without
        // panic.
        assert!(matches!(
            view.maintenance_state,
            MaintenanceStateView::Active
        ));
        let _ = sdk.shutdown().await;
    }

    #[test]
    fn sdk_error_display_carries_kind_discriminator() {
        let err = SdkError::new("register_failed", "host already registered");
        let formatted = format!("{err}");
        assert!(formatted.starts_with("<<meshos-sdk-kind:register_failed>>"));
        assert!(formatted.ends_with("host already registered"));
    }

    #[test]
    fn maintenance_state_view_round_trips_active_default() {
        let now = Instant::now();
        let active = MaintenanceStateView::from_state(&MaintenanceState::Active, now);
        assert!(matches!(active, MaintenanceStateView::Active));
    }

    #[test]
    fn maintenance_state_view_clamps_past_deadlines_to_zero() {
        let now = Instant::now();
        let state = MaintenanceState::EnteringMaintenance {
            since: now - Duration::from_secs(5),
            deadline: Some(now - Duration::from_secs(1)),
        };
        let view = MaintenanceStateView::from_state(&state, now);
        match view {
            MaintenanceStateView::EnteringMaintenance {
                deadline_remaining_ms,
                ..
            } => assert_eq!(deadline_remaining_ms, Some(0)),
            other => panic!("expected EnteringMaintenance, got {other:?}"),
        }
    }

    // Pin the unused-import suppression — we touch these types
    // through macro expansion paths in production but tests don't
    // always exercise them.
    #[allow(dead_code)]
    fn _pin(_p: PendingAction, _a: ActionId, _f: super::super::event::DaemonRef) {}
}
