//! MeshDaemon trait and supporting types.
//!
//! A daemon is a stateful or stateless event processor that runs on the mesh.
//! It consumes causal events and produces output events. The runtime handles
//! chain building, horizon tracking, and snapshot packaging.

use bytes::Bytes;

use crate::adapter::net::behavior::capability::{CapabilityFilter, CapabilitySet};
use crate::adapter::net::state::causal::CausalEvent;

/// A daemon that runs on the mesh.
///
/// Daemons consume inbound causal events via `process()` and return zero or
/// more output payloads. The runtime wraps outputs in `CausalLink`s
/// automatically — the daemon only produces raw payloads.
///
/// # Performance
///
/// `process()` must complete in microseconds. Heavy work should be deferred
/// to a background task and emitted as a later event.
///
/// # WASM compatibility
///
/// All methods are synchronous — no async. Input/output are `Bytes` — maps
/// cleanly to WASM linear memory. No generics or associated types.
pub trait MeshDaemon: Send + Sync {
    /// Human-readable name (for logging, placement ads).
    fn name(&self) -> &str;

    /// Capability requirements for placement.
    ///
    /// The scheduler uses this to find nodes whose `CapabilitySet` matches.
    /// Return `CapabilityFilter::default()` to run anywhere.
    fn requirements(&self) -> CapabilityFilter;

    /// Hard capability requirements for Phase F-aware placement.
    ///
    /// Returns the set of tags + metadata the candidate node MUST
    /// have for the daemon to run there. Tag-set inclusion is the
    /// hard-constraint check (`StandardPlacement` returns `None`
    /// when a required tag is absent — see
    /// [`crate::adapter::net::behavior::placement`]).
    ///
    /// Default: empty set. Daemons that care about specific
    /// hardware / software override:
    ///
    /// ```ignore
    /// fn required_capabilities(&self) -> CapabilitySet {
    ///     CapabilitySet::new().add_tag("hardware.gpu")
    /// }
    /// ```
    ///
    /// Phase G slice 2 of `CAPABILITY_SYSTEM_PLAN.md`. Coexists
    /// with the legacy `requirements()` method until the
    /// `mikoshi-placement-v2` feature flag flips: `requirements()`
    /// drives the legacy `CapabilityFilter`-based path; this
    /// method drives the `Artifact::Daemon { required, .. }`
    /// payload that `PlacementFilter` impls consume.
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::default()
    }

    /// Soft capability preferences for Phase F-aware placement.
    ///
    /// Returns the set of tags + metadata the daemon prefers but
    /// does NOT require. The scheduler factors satisfaction of
    /// these into per-axis scoring; missing optional capabilities
    /// don't veto placement (unlike `required_capabilities`).
    ///
    /// Default: empty set. Daemons with a strict required floor
    /// but additional preferences (e.g. "must have GPU; prefer
    /// 80GB+ VRAM") populate this via per-tag adds.
    ///
    /// Phase G slice 2 of `CAPABILITY_SYSTEM_PLAN.md`. Slice 5's
    /// per-axis scorers consume the optional set when scoring
    /// candidates; slice 2's stub axes return `1.0` regardless.
    fn optional_capabilities(&self) -> CapabilitySet {
        CapabilitySet::default()
    }

    /// Process one inbound causal event, returning zero or more output payloads.
    ///
    /// The output `Bytes` values become payloads in the daemon's own causal
    /// chain (the runtime wraps them in CausalLinks automatically).
    fn process(&mut self, event: &CausalEvent) -> Result<Vec<Bytes>, DaemonError>;

    /// Serialize current state for migration/checkpoint.
    ///
    /// Returns `None` for stateless daemons. Stateful daemons must return
    /// opaque bytes that `restore()` can accept.
    fn snapshot(&self) -> Option<Bytes> {
        None
    }

    /// Whether this daemon carries persistent state that
    /// migration / restart paths must preserve.
    ///
    /// The default `restore` previously accepted any bytes silently
    /// for daemons that didn't override it, including ones that
    /// *should* have been stateful but forgot to provide a `restore`
    /// impl. The new default restores correctly: it matches
    /// `is_stateful()`'s answer. Stateless daemons leave
    /// `is_stateful` at `false` (matches `snapshot() = None`);
    /// stateful daemons override `is_stateful` to `true` AND
    /// `snapshot` / `restore`.
    ///
    /// The migration path can use this to refuse to migrate a
    /// stateful daemon's snapshot bytes into a stateless target,
    /// surfacing the misconfiguration rather than silently
    /// dropping state.
    fn is_stateful(&self) -> bool {
        false
    }

    /// Restore from a previous snapshot.
    ///
    /// Called before any `process()` calls after migration.
    ///
    /// The default implementation now refuses non-empty state on
    /// stateless daemons (`is_stateful() == false`) — silently
    /// discarding a stateful source's snapshot into a stateless
    /// target loses every byte of state with no signal. Stateful
    /// daemons must override both `is_stateful` and `restore`. An
    /// empty `state` is still accepted (it's what
    /// `snapshot() -> None` produces under the migration adapter),
    /// so genuine stateless-to-stateless migrations
    /// continue to work.
    fn restore(&mut self, state: Bytes) -> Result<(), DaemonError> {
        if !self.is_stateful() && !state.is_empty() {
            return Err(DaemonError::RestoreFailed(format!(
                "stateless daemon (is_stateful=false) cannot restore \
                 {}-byte snapshot — override is_stateful() + restore() \
                 if this daemon is actually stateful",
                state.len()
            )));
        }
        Ok(())
    }

    /// Self-reported health. Polled by the MeshOS supervisor on
    /// each tick. Default `Healthy`.
    ///
    /// Daemons with a real health surface (queue-depth probes,
    /// internal cache freshness, dependency readiness, etc.)
    /// override to return a richer value. The supervisor surfaces
    /// the latest sample on the behavior snapshot for Deck.
    ///
    /// Must complete in microseconds — same constraint as
    /// `process()`. Heavy probes belong in a side task whose
    /// result the daemon caches.
    fn health(&self) -> DaemonHealth {
        DaemonHealth::Healthy
    }

    /// Self-reported saturation, `0.0` (idle) to `1.0` (fully
    /// loaded). Used by Phase D-1's mesh scheduler to decide
    /// whether a daemon's host is a good candidate for new
    /// work. Default `0.0`.
    ///
    /// Daemons without a meaningful saturation surface should
    /// leave the default. The value is informational under the
    /// current scheduler; a poor estimate doesn't cause
    /// migrations to thrash.
    fn saturation(&self) -> f32 {
        0.0
    }

    /// Receive a control event from the supervisor. Default:
    /// no-op (the daemon proceeds as normal regardless of the
    /// control signal).
    ///
    /// Daemons that participate in graceful shutdown / drain /
    /// backpressure override to react. The dispatch is sync —
    /// the supervisor calls this between `process()` events on
    /// the daemon's main task, so long-running work in
    /// `on_control` blocks subsequent event processing.
    fn on_control(&mut self, _event: DaemonControl) {}
}

/// Self-reported daemon health. Default trait impl returns
/// `Healthy`; daemons with a real health surface override
/// `MeshDaemon::health` to return a richer value.
///
/// Compiled into the substrate (not gated on the `meshos`
/// feature) so daemons that compile against `MeshDaemon` can
/// return this type without conditional compilation.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum DaemonHealth {
    /// Daemon is fully operational.
    Healthy,
    /// Daemon is running but degraded. `reason` rides into the
    /// behavior snapshot's recent-failures ring buffer + Deck
    /// render.
    Degraded {
        /// Operator-readable reason.
        reason: String,
    },
    /// Daemon is non-functional but hasn't crashed. The
    /// supervisor records this and may emit a `StopDaemon`
    /// action if the desired-state intent flips to `Stop`.
    Unhealthy,
}

/// Supervisor → daemon control event. Delivered via
/// `MeshDaemon::on_control`. Carries relative-duration
/// deadlines (no `Instant`) so a daemon running under any
/// clock source can react.
///
/// The MeshOS-side richer form `MeshOsControl` carries
/// `Instant` deadlines for SDK scheduling; the supervisor
/// integration layer converts via `MeshOsControl::to_daemon_control(now)`.
///
/// `#[non_exhaustive]` so later phases add control variants
/// without breaking daemon implementations.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum DaemonControl {
    /// Graceful shutdown. The daemon should finish in-flight
    /// work and exit before `grace_period_ms` elapses. Past the
    /// deadline the supervisor force-terminates.
    Shutdown {
        /// Milliseconds the daemon has before the supervisor
        /// force-terminates.
        grace_period_ms: u64,
    },

    /// Drain start. Stop accepting new work; in-flight work
    /// continues until `grace_period_ms` elapses or `DrainFinish`
    /// arrives.
    DrainStart {
        /// Milliseconds the drain has before forced cutoff.
        grace_period_ms: u64,
    },

    /// Drain done. The daemon should exit immediately;
    /// in-flight work may be abandoned.
    DrainFinish,

    /// Cluster-wide backpressure is asserted. The daemon should
    /// reduce optional work (cache warmup, background indexing,
    /// etc.) proportional to `level ∈ [0.0, 1.0]`. 1.0 means
    /// "pause optional work entirely".
    BackpressureOn {
        /// Severity in `[0.0, 1.0]`. 0 means just-barely
        /// triggered, 1 means catastrophic queue depth.
        level: f32,
    },

    /// Cluster-wide backpressure cleared. Resume normal work.
    BackpressureOff,
}

/// Lifecycle event a [`DaemonLifecycleObserver`] receives when
/// a daemon's state on this node changes. Plain-data (no
/// references) so observers can buffer / async-forward without
/// lifetime issues.
///
/// The integration with MeshOS lives in `behavior::meshos::sources` —
/// a `MeshOsDaemonLifecycleSink` impls this trait and translates
/// each event to the matching `MeshOsEvent::DaemonLifecycle`.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum DaemonLifecycleEvent {
    /// Daemon registered on this node.
    Registered {
        /// `MeshDaemon::origin_hash`.
        id: u64,
        /// `MeshDaemon::name`.
        name: String,
        /// Wall time of the registration.
        at: std::time::Instant,
    },
    /// Daemon unregistered (either via cleanup or migration
    /// source-side teardown).
    Unregistered {
        /// `MeshDaemon::origin_hash`.
        id: u64,
        /// Last known name (carried so observers don't need to
        /// look it up post-unregister).
        name: String,
        /// Wall time of the unregistration.
        at: std::time::Instant,
    },
    /// Daemon crashed during `process()`.
    Crashed {
        /// `MeshDaemon::origin_hash`.
        id: u64,
        /// `MeshDaemon::name`.
        name: String,
        /// Wall time of the crash.
        at: std::time::Instant,
        /// Operator-readable reason from the daemon-side error.
        reason: String,
    },
    /// Daemon's self-reported health changed (poller observed a
    /// transition from the previous sample).
    HealthChanged {
        /// `MeshDaemon::origin_hash`.
        id: u64,
        /// `MeshDaemon::name`.
        name: String,
        /// Wall time of the observation.
        at: std::time::Instant,
        /// New health value.
        health: DaemonHealth,
    },
    /// Daemon's self-reported saturation changed (poller
    /// observed a transition exceeding the configured noise
    /// floor — see `behavior::meshos` for the threshold).
    SaturationChanged {
        /// `MeshDaemon::origin_hash`.
        id: u64,
        /// `MeshDaemon::name`.
        name: String,
        /// Wall time of the observation.
        at: std::time::Instant,
        /// New saturation value, `[0.0, 1.0]`.
        saturation: f32,
    },
}

/// Observer hook for daemon lifecycle events. Implementations
/// fan the events out to whichever consumer wants them — the
/// MeshOS event loop being the canonical near-term consumer.
///
/// Methods are sync + non-blocking: observers must not block in
/// `observe`. The `DaemonRegistry` calls `observe` while
/// holding (briefly) per-call references; a slow observer
/// would stall every other lifecycle path.
pub trait DaemonLifecycleObserver: Send + Sync + 'static {
    /// Receive one lifecycle event. Must not block.
    fn observe(&self, event: DaemonLifecycleEvent);
}

/// Errors from daemon operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonError {
    /// Daemon processing logic failed.
    ProcessFailed(String),
    /// Snapshot serialization failed.
    SnapshotFailed(String),
    /// Restore from snapshot failed.
    RestoreFailed(String),
    /// Daemon not found in registry.
    NotFound(u64),
    /// The daemon at this origin_hash was concurrently swapped
    /// (`replace`d) or `unregister`ed while this caller was
    /// preparing to mutate it. The caller's mutation did not
    /// land — the registry detected the orphaned `Arc` after
    /// acquiring the inner lock and bailed before invoking the
    /// host. Retry the operation against the current
    /// registered host (if any).
    Stale(u64),
}

impl std::fmt::Display for DaemonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProcessFailed(msg) => write!(f, "daemon process failed: {}", msg),
            Self::SnapshotFailed(msg) => write!(f, "snapshot failed: {}", msg),
            Self::RestoreFailed(msg) => write!(f, "restore failed: {}", msg),
            Self::NotFound(id) => write!(f, "daemon not found: {:#x}", id),
            Self::Stale(id) => write!(
                f,
                "daemon {:#x} was swapped or unregistered concurrently; mutation did not land",
                id
            ),
        }
    }
}

impl std::error::Error for DaemonError {}

/// Configuration for a daemon host.
#[derive(Debug, Clone)]
pub struct DaemonHostConfig {
    /// How often to auto-snapshot (in events processed). 0 = manual only.
    pub auto_snapshot_interval: u64,
    /// Maximum events to buffer before forcing a snapshot.
    pub max_log_entries: u32,
}

impl Default for DaemonHostConfig {
    fn default() -> Self {
        Self {
            auto_snapshot_interval: 0,
            max_log_entries: 10_000,
        }
    }
}

/// Runtime statistics for a daemon.
#[derive(Debug, Clone, Default)]
pub struct DaemonStats {
    /// Total events processed.
    pub events_processed: u64,
    /// Total output events emitted.
    pub events_emitted: u64,
    /// Total processing errors.
    pub errors: u64,
    /// Number of snapshots taken.
    pub snapshots_taken: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal daemon that doesn't override the new
    /// `required_capabilities` / `optional_capabilities` methods.
    /// Pins backward compatibility: existing daemon impls that
    /// were written pre-Phase-G compile + run unchanged.
    struct BareDaemon;

    impl MeshDaemon for BareDaemon {
        fn name(&self) -> &str {
            "bare"
        }
        fn requirements(&self) -> CapabilityFilter {
            CapabilityFilter::default()
        }
        fn process(&mut self, _event: &CausalEvent) -> Result<Vec<Bytes>, DaemonError> {
            Ok(Vec::new())
        }
    }

    /// Daemon that overrides both new methods. Pins the surface
    /// daemon authors target when declaring placement requirements.
    struct GpuDaemon;

    impl MeshDaemon for GpuDaemon {
        fn name(&self) -> &str {
            "gpu"
        }
        fn requirements(&self) -> CapabilityFilter {
            CapabilityFilter::default()
        }
        fn required_capabilities(&self) -> CapabilitySet {
            CapabilitySet::new().add_tag("hardware.gpu")
        }
        fn optional_capabilities(&self) -> CapabilitySet {
            CapabilitySet::new().add_tag("hardware.gpu.vram_gb=80")
        }
        fn process(&mut self, _event: &CausalEvent) -> Result<Vec<Bytes>, DaemonError> {
            Ok(Vec::new())
        }
    }

    /// Default `required_capabilities()` returns an empty set —
    /// daemon runs anywhere. Pin so changing the default to a
    /// non-empty value (which would break backward-compat for
    /// existing impls) fails build.
    #[test]
    fn required_capabilities_default_is_empty() {
        let d = BareDaemon;
        let req = d.required_capabilities();
        assert!(req.tags.is_empty());
        assert!(req.metadata.is_empty());
    }

    /// Same for `optional_capabilities`.
    #[test]
    fn optional_capabilities_default_is_empty() {
        let d = BareDaemon;
        let opt = d.optional_capabilities();
        assert!(opt.tags.is_empty());
        assert!(opt.metadata.is_empty());
    }

    /// An override populates the returned set as expected — pins
    /// the daemon-author-facing surface.
    #[test]
    fn override_populates_required_and_optional() {
        let d = GpuDaemon;
        let req = d.required_capabilities();
        let opt = d.optional_capabilities();
        assert_eq!(req.tags.len(), 1);
        assert!(req.tags.iter().any(|t| t.to_string() == "hardware.gpu"));
        assert_eq!(opt.tags.len(), 1);
        assert!(opt
            .tags
            .iter()
            .any(|t| t.to_string() == "hardware.gpu.vram_gb=80"));
    }

    /// The new methods plug into `PlacementFilter` via the
    /// `Artifact::Daemon { required, optional, .. }` payload.
    /// Pin the integration shape so a refactor of either side
    /// surfaces in this test.
    #[test]
    fn required_capabilities_drive_artifact_daemon() {
        use crate::adapter::net::behavior::placement::Artifact;
        let d = GpuDaemon;
        let req = d.required_capabilities();
        let opt = d.optional_capabilities();
        let _artifact = Artifact::Daemon {
            daemon_id: [0u8; 32],
            required: &req,
            optional: &opt,
        };
        // If the artifact type's Daemon variant changes shape,
        // this construction fails compile.
    }

    // MeshOS-supervision extension: health / saturation / on_control.
    // Pin the defaults so a daemon written against the older trait
    // continues to compile + behave under the new supervisor.

    #[test]
    fn health_default_is_healthy() {
        let d = BareDaemon;
        assert_eq!(d.health(), DaemonHealth::Healthy);
    }

    #[test]
    fn saturation_default_is_zero() {
        let d = BareDaemon;
        assert_eq!(d.saturation(), 0.0);
    }

    /// Daemon that overrides the new MeshOS-supervision methods.
    /// Pins the override surface daemon authors target when they
    /// participate in graceful shutdown / drain / health reporting.
    struct WatchedDaemon {
        last_control: Option<DaemonControl>,
        health: DaemonHealth,
        saturation: f32,
    }

    impl MeshDaemon for WatchedDaemon {
        fn name(&self) -> &str {
            "watched"
        }
        fn requirements(&self) -> CapabilityFilter {
            CapabilityFilter::default()
        }
        fn process(&mut self, _event: &CausalEvent) -> Result<Vec<Bytes>, DaemonError> {
            Ok(Vec::new())
        }
        fn health(&self) -> DaemonHealth {
            self.health.clone()
        }
        fn saturation(&self) -> f32 {
            self.saturation
        }
        fn on_control(&mut self, event: DaemonControl) {
            self.last_control = Some(event);
        }
    }

    #[test]
    fn override_surfaces_for_health_and_saturation() {
        let d = WatchedDaemon {
            last_control: None,
            health: DaemonHealth::Degraded {
                reason: "queue depth".into(),
            },
            saturation: 0.42,
        };
        assert!(matches!(d.health(), DaemonHealth::Degraded { .. }));
        assert!((d.saturation() - 0.42).abs() < 1e-6);
    }

    #[test]
    fn on_control_receives_supervisor_events() {
        let mut d = WatchedDaemon {
            last_control: None,
            health: DaemonHealth::Healthy,
            saturation: 0.0,
        };
        d.on_control(DaemonControl::Shutdown {
            grace_period_ms: 5_000,
        });
        assert!(matches!(
            d.last_control,
            Some(DaemonControl::Shutdown {
                grace_period_ms: 5_000
            })
        ));
        d.on_control(DaemonControl::BackpressureOn { level: 0.5 });
        assert!(matches!(
            d.last_control,
            Some(DaemonControl::BackpressureOn { level }) if (level - 0.5).abs() < 1e-6
        ));
    }

    #[test]
    fn bare_daemon_ignores_control_events_silently() {
        // Default `on_control` is a no-op — the daemon
        // proceeds as normal. Critical for backward
        // compatibility: existing daemons don't suddenly
        // change behavior under the new supervisor.
        let mut d = BareDaemon;
        d.on_control(DaemonControl::DrainFinish);
        d.on_control(DaemonControl::BackpressureOff);
        // No state to assert — the contract is just "no panic,
        // no side effect."
    }
}
