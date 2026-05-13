//! [`MeshOsConfig`] — tunables for the canonical event loop.
//! Defaults match the locked decisions in the plan (tick cadence
//! aligned with heartbeat, backpressure thresholds, drain rate
//! limits). Operators override per-node via the same MeshConfig
//! surface that already carries `heartbeat_interval`.

use std::time::Duration;

/// Tunable parameters for [`super::event_loop::MeshOsLoop`].
/// `Default::default()` produces the plan's stated defaults.
#[derive(Clone, Debug)]
pub struct MeshOsConfig {
    /// Identity of the node this loop runs on. Used by Phase C
    /// reconcile to decide whether the leader-only `Request*`
    /// actions are admissible here. Default `0`; production
    /// callers must override.
    pub this_node: super::event::NodeId,

    /// How often the loop fires a [`super::event::MeshOsEvent::Tick`]
    /// to drive a reconcile pass. Default 500 ms — matches
    /// `MeshConfig::heartbeat_interval`. The reconcile pass
    /// never runs more often than this regardless of event
    /// arrival rate.
    pub tick_interval: Duration,

    /// Capacity of the event-source mpsc channel. Sources that
    /// produce faster than the loop can consume block on send;
    /// this bound is the safety valve against unbounded growth.
    /// Default 1024.
    pub event_queue_capacity: usize,

    /// Capacity of the action-executor mpsc channel. The
    /// reconcile pass enqueues actions here; the executor drains
    /// under the backpressure layer (Phase G). Default 1024.
    pub action_queue_capacity: usize,

    /// Phase G — backpressure / safety knobs. Phase A wires the
    /// fields in; the executor honors them once Phase G's
    /// `admit()` check lands.
    pub backpressure: BackpressureConfig,

    /// Phase D — locality tuning (RTT thresholds, avoid-list
    /// TTLs).
    pub locality: LocalityConfig,

    /// Phase E — maintenance state machine tuning (drain
    /// deadlines, recovery ramp-up window).
    pub maintenance: MaintenanceConfig,

    /// Phase D-1 — continuous-rebalance scoring tunables.
    pub scheduler: super::scheduler::SchedulerConfig,
}

impl Default for MeshOsConfig {
    fn default() -> Self {
        Self {
            this_node: 0,
            tick_interval: Duration::from_millis(500),
            event_queue_capacity: 1024,
            action_queue_capacity: 1024,
            backpressure: BackpressureConfig::default(),
            locality: LocalityConfig::default(),
            maintenance: MaintenanceConfig::default(),
            scheduler: super::scheduler::SchedulerConfig::default(),
        }
    }
}

/// Phase G backpressure tunables — included in
/// [`MeshOsConfig`] from Phase A so the `admit()` layer can read
/// them once it lands, without breaking the config shape.
#[derive(Clone, Debug)]
pub struct BackpressureConfig {
    /// Minimum interval between admitted replica-pull actions.
    /// Default 250 ms.
    pub pull_cooldown: Duration,

    /// Maximum drain-triggered migrations per zone per second.
    /// Default 10.
    pub drain_rate_per_zone_per_sec: u32,

    /// After a replica migration completes, the chain is
    /// excluded from further migration decisions for this long.
    /// Default 60 s.
    pub replica_stabilization_window: Duration,

    /// Action-queue depth above which MeshOS broadcasts
    /// `MeshOsControl::BackpressureOn` to supervised daemons.
    /// Default 1000.
    pub cluster_backpressure_threshold: usize,

    /// Queue depth below which `BackpressureOn` is rescinded
    /// (`BackpressureOff` broadcast). Default 200. Hysteresis
    /// avoids on/off thrash near the threshold.
    pub cluster_backpressure_release: usize,
}

impl Default for BackpressureConfig {
    fn default() -> Self {
        Self {
            pull_cooldown: Duration::from_millis(250),
            drain_rate_per_zone_per_sec: 10,
            replica_stabilization_window: Duration::from_secs(60),
            cluster_backpressure_threshold: 1000,
            cluster_backpressure_release: 200,
        }
    }
}

/// Phase D locality tunables. Default values come from the
/// plan's locked decisions; operators override per-node.
#[derive(Clone, Debug)]
pub struct LocalityConfig {
    /// RTT samples above this threshold flag the peer as
    /// "degraded" and trigger a `MarkAvoid` action. Default
    /// 250 ms — twice the heartbeat interval, so a peer whose
    /// RTT is double the configured cadence is suspect.
    pub degraded_rtt_threshold: Duration,

    /// TTL applied to `MarkAvoid` actions emitted by Phase D
    /// reconcile. Default 5 min — long enough to survive a
    /// transient RTT spike, short enough that a recovered peer
    /// gets re-evaluated.
    pub avoid_ttl: Duration,
}

impl Default for LocalityConfig {
    fn default() -> Self {
        Self {
            degraded_rtt_threshold: Duration::from_millis(250),
            avoid_ttl: Duration::from_secs(5 * 60),
        }
    }
}

/// Phase E maintenance tunables.
#[derive(Clone, Debug)]
pub struct MaintenanceConfig {
    /// Default drain deadline applied when `AdminEvent::EnterMaintenance`
    /// arrives without an explicit one. Default 10 min.
    pub default_drain_deadline: Duration,

    /// Ramp-up window after `ExitingMaintenance → Recovery`.
    /// During this window the node is on the avoid list for new
    /// placement; the loop transitions to `Active` once it
    /// elapses. Default 5 min.
    pub recovery_ramp_window: Duration,
}

impl Default for MaintenanceConfig {
    fn default() -> Self {
        Self {
            default_drain_deadline: Duration::from_secs(10 * 60),
            recovery_ramp_window: Duration::from_secs(5 * 60),
        }
    }
}
