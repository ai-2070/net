//! NAT traversal — reflex-address discovery, NAT-type classification,
//! hole-punch rendezvous, and (feature-gated) UPnP / NAT-PMP / PCP
//! port mapping.
//!
//! **Framing.** NAT traversal in this codebase is a
//! **latency / throughput optimization**, not a correctness
//! requirement. Connectivity between two NATed peers already works
//! via routed handshakes + relay forwarding — every message reaches
//! its destination regardless of NAT type. What this module adds is
//! a shorter path for the cases where a direct punch is feasible,
//! reducing the per-packet relay tax and the load concentrated on
//! topological relays.
//!
//! A `NatType::Symmetric` classification or a `PunchFailed` outcome
//! is **not** a connectivity failure — it just means traffic keeps
//! riding the relay. The design doc
//! (`docs/NAT_TRAVERSAL_PLAN.md`) treats this framing as
//! load-bearing; docstrings added here must not imply that any
//! NAT-traversal primitive is required for peers behind NAT to
//! talk to each other.
//!
//! # Module layout
//!
//! - [`reflex`]      — reflex probe subprotocol handler + client.
//! - [`classify`]    — `NatType` classification state machine.
//! - [`rendezvous`]  — hole-punch coordinator subprotocol.
//! - [`config`]      — [`TraversalConfig`] (probe cadence, timeouts, …).
//! - `portmap`       — UPnP / NAT-PMP / PCP client (gated behind
//!   the `port-mapping` cargo feature; lands in stage 4 of the plan).
//!
//! # Staging
//!
//! Implemented incrementally per `docs/NAT_TRAVERSAL_PLAN.md`:
//!
//! | Stage | Surface                                    | Status            |
//! |-------|--------------------------------------------|-------------------|
//! | 0     | Module scaffolding + feature gate          | **done**          |
//! | 1     | Reflex probe subprotocol                   | **done**          |
//! | 2     | NAT type classification + `reflex_addr`    | **done**          |
//! | 3     | Hole-punch rendezvous (coordinator + ack + keep-alive train) | **done** |
//! | 4a    | Reflex override (config + runtime setters) | **done**          |
//! | 4b    | UPnP / NAT-PMP / PCP port-mapping client   | deferred (needs `igd-next` + `rust-natpmp` deps + real-router testing; the `set_reflex_override` runtime setter in stage 4a is the hook point) |
//! | 5     | SDK + NAPI + PyO3 + Go binding surface     | **done**          |
//!
//! Every stage is independently shippable. Earlier stages provide
//! observability (`nat_type`, `reflex_addr`) without the later
//! stages having landed; later stages lift performance without
//! changing the correctness contract.

pub mod classify;
pub mod config;
#[cfg(feature = "port-mapping")]
pub mod portmap;
pub mod reflex;
pub mod rendezvous;

// Re-exports for the stable sub-module surface. Kept narrow on
// purpose — each sub-module owns the bulk of its public types
// and users import them from their origin rather than the root.
pub use config::TraversalConfig;

// =========================================================================
// Traversal stats
// =========================================================================

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
#[cfg(feature = "port-mapping")]
use std::sync::Arc;

use arc_swap::ArcSwapOption;

/// Counters tracking traversal decisions + outcomes. Exposed via
/// [`crate::adapter::net::MeshNode::traversal_stats`]. Every
/// counter is monotonic; resetting isn't supported because the
/// values are only meaningful cumulatively.
///
/// The three punch/fallback counters partition all
/// `connect_direct` outcomes:
///
/// - **`punches_attempted`** — the coordinator mediated a
///   punch: the `PunchRequest` went on the wire and the
///   `PunchIntroduce` came back. Fast-fails before that point
///   (coordinator unreachable, socket send failed) do NOT
///   increment — the counter reflects *actual punch activity*,
///   not just matrix decisions. Bumped whether the subsequent
///   ack / direct handshake ultimately succeeds or falls back.
/// - **`relay_fallbacks`** — connection ended up on the routed-
///   handshake path: `SkipPunch` (Symmetric-×-Symmetric), a
///   failed-and-fallen-back punch, or a failed-and-fallen-back
///   `Direct` attempt. Only incremented after the routed
///   handshake *itself* lands. Successful direct connects don't
///   contribute, and failed-and-ALSO-failed fallback attempts
///   don't either — so the counter stays a real "we're on the
///   relay right now" signal operators can use.
/// - **`punches_succeeded`** — the punch completed within the
///   deadline and produced a direct session. Always `≤
///   punches_attempted`; the difference is the punch-failure
///   rate.
///
/// Plus three port-mapping fields (stage 4b): `port_mapping_active`
/// plus a monotonic renewal counter and the current external
/// `SocketAddr`. See `docs/PORT_MAPPING_PLAN.md` §8 for the
/// derivation.
///
/// Read via [`TraversalStats::snapshot`] for a consistent
/// point-in-time view.
#[derive(Debug, Default)]
pub struct TraversalStats {
    punches_attempted: AtomicU64,
    punches_succeeded: AtomicU64,
    relay_fallbacks: AtomicU64,
    /// True when a port mapping is currently installed on the
    /// operator's router and the mesh is advertising the mapped
    /// external address via `set_reflex_override`.
    port_mapping_active: AtomicBool,
    /// The external `SocketAddr` a successful port-mapping
    /// install produced. `None` while inactive.
    port_mapping_external: ArcSwapOption<std::net::SocketAddr>,
    /// Count of successful renewal ticks since install.
    port_mapping_renewals: AtomicU64,
    /// Background direct-path upgrades started (a relay-routed
    /// session for which the pair matrix and outcome cache elected
    /// to attempt a punch). `NAT_TRAVERSAL_V2_PLAN.md` Stage 3.
    upgrades_attempted: AtomicU64,
    /// Upgrades that landed a direct session (the relay-routed
    /// session was replaced by the punched path).
    upgrades_succeeded: AtomicU64,
    /// Upgrades deferred because the session was busy (open streams
    /// / unacked in-flight data) at swap time — the C3 busy gate.
    /// Deferred upgrades retry later; they are not failures.
    upgrades_deferred_busy: AtomicU64,
    /// Punch flows that gave up on a deadline (Stage 5, decision 7):
    /// the coordinator neither introduced nor rejected within
    /// `punch_deadline`, or the introduce arrived but the
    /// counterpart's `PunchAck` never did. One increment per flow —
    /// a single punch can time out on at most one of its waits.
    punch_timeouts: AtomicU64,
    /// Punch flows refused by a typed `PunchReject` (rate-limited,
    /// unknown target reflex, no session with target, reflex
    /// mismatch). Fast failures — no deadline was consumed.
    punch_rejections: AtomicU64,
    /// Punch-needing pairs skipped because no rendezvous coordinator
    /// candidate existed (`select_punch_coordinator` tier 4). The
    /// caller stays on the routed path.
    rendezvous_no_relay: AtomicU64,
}

/// Consistent point-in-time view of the [`TraversalStats`]
/// counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraversalStatsSnapshot {
    /// Number of punches whose `PunchRequest` was successfully
    /// mediated by the coordinator. Fast-fails before the wire
    /// send (coordinator unreachable, transport error) don't
    /// contribute — the counter reflects real punch activity.
    pub punches_attempted: u64,
    /// Number of those attempts that produced a direct session.
    pub punches_succeeded: u64,
    /// Number of `connect_direct` calls that ended on the routed-
    /// handshake path: matrix-skipped (Symmetric × Symmetric),
    /// punch-failed, or Direct-handshake-failed-falling-back.
    /// Only counted after the routed handshake actually
    /// succeeds — failed-and-also-failed-fallback calls don't
    /// contribute. Successful direct connects don't contribute
    /// either.
    pub relay_fallbacks: u64,
    /// True when a port mapping is currently installed on the
    /// operator's router. Flips via the port-mapping task's
    /// install / revoke transitions.
    pub port_mapping_active: bool,
    /// Current mapped external `SocketAddr`, or `None` when no
    /// port mapping is active.
    pub port_mapping_external: Option<std::net::SocketAddr>,
    /// Cumulative count of successful renewal ticks since the
    /// current mapping was installed. Resets on a fresh install.
    pub port_mapping_renewals: u64,
    /// Background direct-path upgrades started (Stage 3). See
    /// [`TraversalStats`] for the field semantics.
    pub upgrades_attempted: u64,
    /// Upgrades that replaced a relay-routed session with a punched
    /// direct session.
    pub upgrades_succeeded: u64,
    /// Upgrades deferred by the C3 busy gate (retried later; not a
    /// failure).
    pub upgrades_deferred_busy: u64,
    /// **Derived**: `punches_attempted - punches_succeeded` — punches
    /// whose coordinator mediation succeeded but that didn't land a
    /// direct session. Computed at snapshot time; a punch in flight
    /// at the observation instant counts as failed until it resolves
    /// (the same cross-counter skew the struct doc already accepts).
    pub punches_failed: u64,
    /// Punch flows that gave up on a deadline. A **cause counter**,
    /// not a partition of [`Self::punches_failed`]: an introduce-wait
    /// timeout happens *before* the attempt is counted (no wire
    /// mediation happened), so `punch_timeouts` can exceed
    /// `punches_failed`.
    pub punch_timeouts: u64,
    /// Punch flows refused by a typed `PunchReject`. Cause counter —
    /// rejections happen before mediation, so they don't contribute
    /// to `punches_attempted` / `punches_failed`.
    pub punch_rejections: u64,
    /// Punch-needing pairs skipped for lack of any coordinator
    /// candidate. Cause counter; the caller stayed on the routed path.
    pub rendezvous_no_relay: u64,
}

impl TraversalStats {
    /// Construct a zeroed stats block. Identical to
    /// `Default::default()`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Read all counters + port-mapping flags. Reads are
    /// `Relaxed` — the stats are observability, not
    /// synchronization primitives, and cross-counter skew at
    /// observation time is meaningless.
    pub fn snapshot(&self) -> TraversalStatsSnapshot {
        let punches_attempted = self.punches_attempted.load(Ordering::Relaxed);
        let punches_succeeded = self.punches_succeeded.load(Ordering::Relaxed);
        TraversalStatsSnapshot {
            punches_attempted,
            punches_succeeded,
            relay_fallbacks: self.relay_fallbacks.load(Ordering::Relaxed),
            port_mapping_active: self.port_mapping_active.load(Ordering::Relaxed),
            port_mapping_external: self.port_mapping_external.load_full().map(|arc| *arc),
            port_mapping_renewals: self.port_mapping_renewals.load(Ordering::Relaxed),
            upgrades_attempted: self.upgrades_attempted.load(Ordering::Relaxed),
            upgrades_succeeded: self.upgrades_succeeded.load(Ordering::Relaxed),
            upgrades_deferred_busy: self.upgrades_deferred_busy.load(Ordering::Relaxed),
            // Derived, saturating: `succeeded` is bumped a beat after
            // `attempted` on the success path, so a racing read could
            // otherwise underflow.
            punches_failed: punches_attempted.saturating_sub(punches_succeeded),
            punch_timeouts: self.punch_timeouts.load(Ordering::Relaxed),
            punch_rejections: self.punch_rejections.load(Ordering::Relaxed),
            rendezvous_no_relay: self.rendezvous_no_relay.load(Ordering::Relaxed),
        }
    }

    /// Bump `punches_attempted`. Called when the pair-type matrix
    /// elects to try a hole-punch, regardless of outcome.
    pub(crate) fn record_punch_attempt(&self) {
        self.punches_attempted.fetch_add(1, Ordering::Relaxed);
    }

    /// Bump `punches_succeeded`. Called when a punch attempt
    /// completes with a direct session.
    pub(crate) fn record_punch_success(&self) {
        self.punches_succeeded.fetch_add(1, Ordering::Relaxed);
    }

    /// Bump `relay_fallbacks`. Called when `connect_direct`
    /// resolves on the routed-handshake path — matrix-skipped or
    /// punch-failed.
    pub(crate) fn record_relay_fallback(&self) {
        self.relay_fallbacks.fetch_add(1, Ordering::Relaxed);
    }

    /// Bump `upgrades_attempted`. Called when a background direct-path
    /// upgrade begins its punch (Stage 3).
    #[cfg(feature = "nat-traversal")]
    pub(crate) fn record_upgrade_attempt(&self) {
        self.upgrades_attempted.fetch_add(1, Ordering::Relaxed);
    }

    /// Bump `upgrades_succeeded`. Called when an upgrade replaces the
    /// relay-routed session with a punched direct session.
    #[cfg(feature = "nat-traversal")]
    pub(crate) fn record_upgrade_success(&self) {
        self.upgrades_succeeded.fetch_add(1, Ordering::Relaxed);
    }

    /// Bump `upgrades_deferred_busy`. Called when the C3 busy gate
    /// defers an upgrade because the session had open streams / unacked
    /// data at swap time.
    #[cfg(feature = "nat-traversal")]
    pub(crate) fn record_upgrade_deferred_busy(&self) {
        self.upgrades_deferred_busy.fetch_add(1, Ordering::Relaxed);
    }

    /// Bump `punch_timeouts`. Called when a punch flow's deadline
    /// elapses — the introduce wait in `request_punch` /
    /// `await_punch_introduce`, or the ack wait in `connect_direct`'s
    /// `SinglePunch` arm. Oneshot-cancelled waits (superseded by a
    /// concurrent call to the same target) are NOT timeouts and don't
    /// count.
    #[cfg(feature = "nat-traversal")]
    pub(crate) fn record_punch_timeout(&self) {
        self.punch_timeouts.fetch_add(1, Ordering::Relaxed);
    }

    /// Bump `punch_rejections`. Called when a pending punch waiter
    /// resolves with a typed `PunchReject` from the coordinator.
    #[cfg(feature = "nat-traversal")]
    pub(crate) fn record_punch_rejection(&self) {
        self.punch_rejections.fetch_add(1, Ordering::Relaxed);
    }

    /// Bump `rendezvous_no_relay`. Called when a punch-needing pair
    /// is skipped because `select_punch_coordinator` found no
    /// candidate (tier 4).
    #[cfg(feature = "nat-traversal")]
    pub(crate) fn record_rendezvous_no_relay(&self) {
        self.rendezvous_no_relay.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a freshly-installed port mapping. Flips
    /// `port_mapping_active` to `true`, stores the external
    /// `SocketAddr`, and resets the renewal counter to zero —
    /// a new install is treated as a distinct session from any
    /// prior one.
    #[cfg(feature = "port-mapping")]
    pub(crate) fn record_port_mapping_install(&self, external: std::net::SocketAddr) {
        self.port_mapping_external.store(Some(Arc::new(external)));
        self.port_mapping_renewals.store(0, Ordering::Release);
        self.port_mapping_active.store(true, Ordering::Release);
    }

    /// Record a successful renewal of the currently-installed
    /// mapping. Bumps the renewal counter; leaves
    /// `port_mapping_active` + `port_mapping_external` unchanged.
    #[cfg(feature = "port-mapping")]
    pub(crate) fn record_port_mapping_renewal(&self) {
        self.port_mapping_renewals.fetch_add(1, Ordering::Relaxed);
    }

    /// Overwrite the stored external `SocketAddr` without
    /// touching `port_mapping_active` or the renewal counter.
    /// Called by [`MappingSink::apply_renewal`] when the router
    /// returns a different external on a renewal tick (router
    /// reboot / WAN flap / DHCP lease change). Keeping stats
    /// and reflex in sync matters: observability reads this
    /// field, and it's what operators cross-reference against
    /// `reflex_addr` when debugging flapping reachability.
    #[cfg(feature = "port-mapping")]
    pub(crate) fn replace_port_mapping_external(&self, external: std::net::SocketAddr) {
        self.port_mapping_external.store(Some(Arc::new(external)));
    }

    /// Record a revoke — either operator-initiated (clean
    /// shutdown) or after repeated renewal failures. Flips
    /// `port_mapping_active` to `false` and clears the external
    /// address.
    #[cfg(feature = "port-mapping")]
    pub(crate) fn record_port_mapping_revoke(&self) {
        self.port_mapping_active.store(false, Ordering::Release);
        self.port_mapping_external.store(None);
    }
}

// =========================================================================
// Error surface
// =========================================================================

/// Typed failures from the NAT-traversal subsystem. Matches the
/// vocabulary locked in `docs/NAT_TRAVERSAL_PLAN.md` stage 5 — each
/// variant maps to a stable `kind` string the SDK bindings expose
/// to callers.
///
/// **Framing reminder.** Every variant here describes the failure
/// of an *optimization*, not a connectivity failure. A caller that
/// receives `TraversalError` can always proceed via routed-handshake
/// — the traversal path just didn't pan out.
#[derive(Debug, thiserror::Error)]
pub enum TraversalError {
    /// Reflex probe didn't complete in time. The requester gave
    /// up after [`TraversalConfig::reflex_timeout`] without
    /// observing a response.
    #[error("reflex-timeout")]
    ReflexTimeout,

    /// The named peer is not currently reachable from this node
    /// (no session, no cached addr). Rendezvous / reflex paths
    /// need at least a direct or relayed path to the peer; if
    /// none exists, the optimization can't run.
    #[error("peer-not-reachable")]
    PeerNotReachable,

    /// Transport-level failure while sending the probe / punch
    /// traffic (socket error, partition filter, etc.).
    #[error("transport: {0}")]
    Transport(String),

    // Reserved for stages 3–5. Left defined here so downstream
    // stages can add variants without shifting the public enum
    // discriminants.
    /// Rendezvous coordinator couldn't find a mutually-connected
    /// relay-capable peer to introduce the pair.
    #[error("rendezvous-no-relay")]
    RendezvousNoRelay,

    /// Rendezvous coordinator refused to coordinate (rate-limit
    /// / unknown target / policy reject).
    #[error("rendezvous-rejected: {0}")]
    RendezvousRejected(String),

    /// Keep-alive train didn't establish a punched path within
    /// the [`TraversalConfig::punch_deadline`] window.
    #[error("punch-failed")]
    PunchFailed,

    /// UPnP / NAT-PMP / PCP all failed to install a port mapping.
    /// Only surfaces when the `port-mapping` feature is enabled
    /// AND `MeshBuilder::try_port_mapping(true)` opted in.
    #[error("port-map-unavailable")]
    PortMapUnavailable,

    /// Peer doesn't advertise the NAT-traversal capability tag
    /// (compiled without `nat-traversal`, or opted out via
    /// `MeshBuilder::disable_nat_traversal`).
    #[error("unsupported")]
    Unsupported,
}

impl TraversalError {
    /// Stable machine-readable kind string used by the SDK
    /// bindings to expose typed catches. Never localized; never
    /// changed once a variant has shipped.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::ReflexTimeout => "reflex-timeout",
            Self::PeerNotReachable => "peer-not-reachable",
            Self::Transport(_) => "transport",
            Self::RendezvousNoRelay => "rendezvous-no-relay",
            Self::RendezvousRejected(_) => "rendezvous-rejected",
            Self::PunchFailed => "punch-failed",
            Self::PortMapUnavailable => "port-map-unavailable",
            Self::Unsupported => "unsupported",
        }
    }
}

// =========================================================================
// Subprotocol ID assignment
// =========================================================================
//
// The `0x0D00` block is the first unused range after the existing
// subprotocol allocations (`0x0400` causal, `0x0500` migration,
// `0x0A00` channel membership, `0x0B00` stream window,
// `0x0C00` capability announcement). Reserved for NAT-traversal
// primitives; ids consumed here:
//
//   0x0D00 — reflex probe (stage 1)
//   0x0D01 — rendezvous (stage 3)
//   0x0D02 — reserved for port-mapping metadata (stage 4, optional)
//
// Future traversal primitives take `0x0D0x` ids sequentially.

/// Subprotocol ID for the reflex-probe request/response exchange.
///
/// Any peer that receives a `SUBPROTOCOL_REFLEX` request replies with
/// the source `ip:port` it observed on the request's UDP envelope.
/// Two or more probes to different peers are sufficient to detect
/// symmetric NAT (the observed source port differs per destination).
///
/// See [`reflex`] for the handler / client implementation.
pub const SUBPROTOCOL_REFLEX: u16 = 0x0D00;

/// Subprotocol ID for hole-punch rendezvous coordination.
///
/// Carries the three-message dance (`PunchRequest` →
/// `PunchIntroduce` × 2 → `PunchAck`) that synchronizes a
/// simultaneous open between two NATed peers, mediated by a
/// mutually-connected relay.
///
/// See [`rendezvous`] for the state machine.
pub const SUBPROTOCOL_RENDEZVOUS: u16 = 0x0D01;

#[cfg(test)]
mod error_kind_tests {
    use super::*;

    /// The machine-readable `kind()` vocabulary is a stable API the
    /// SDK bindings map to typed catches. Pin the two rendezvous
    /// outcomes that Stage 2 (Finding 5) brought to life — they were
    /// defined-but-never-constructed before, so nothing guarded them.
    #[test]
    fn rendezvous_error_kinds_are_stable() {
        assert_eq!(
            TraversalError::RendezvousRejected("rate-limited".into()).kind(),
            "rendezvous-rejected",
        );
        assert_eq!(
            TraversalError::RendezvousNoRelay.kind(),
            "rendezvous-no-relay"
        );
    }

    /// The reason sub-kind rides in the `RendezvousRejected` payload
    /// (via `Display`), so a caller can distinguish rate-limit from
    /// anti-reflection without a second field.
    #[test]
    fn rendezvous_rejected_display_carries_reason() {
        let e = TraversalError::RendezvousRejected("reflex-mismatch".into());
        assert_eq!(e.to_string(), "rendezvous-rejected: reflex-mismatch");
    }
}

#[cfg(all(test, feature = "nat-traversal"))]
mod stats_snapshot_tests {
    use super::*;

    /// `punches_failed` is derived at snapshot time — mediated
    /// attempts minus successes — and the reason counters are
    /// independent cause counters, not a partition of it (Stage 5,
    /// decision 7).
    #[test]
    fn punches_failed_derives_and_reason_counters_are_independent() {
        let stats = TraversalStats::new();
        let zero = stats.snapshot();
        assert_eq!(zero.punches_failed, 0);
        assert_eq!(zero.punch_timeouts, 0);
        assert_eq!(zero.punch_rejections, 0);
        assert_eq!(zero.rendezvous_no_relay, 0);

        // 3 mediated attempts, 1 landed → 2 derived failures.
        stats.record_punch_attempt();
        stats.record_punch_attempt();
        stats.record_punch_attempt();
        stats.record_punch_success();
        // Cause events: one ack-wait timeout (contributes to a
        // derived failure) plus one pre-mediation rejection and one
        // no-relay skip (which do NOT — no attempt was counted).
        stats.record_punch_timeout();
        stats.record_punch_rejection();
        stats.record_rendezvous_no_relay();

        let snap = stats.snapshot();
        assert_eq!(snap.punches_attempted, 3);
        assert_eq!(snap.punches_succeeded, 1);
        assert_eq!(snap.punches_failed, 2, "derived: attempted - succeeded");
        assert_eq!(snap.punch_timeouts, 1);
        assert_eq!(snap.punch_rejections, 1);
        assert_eq!(snap.rendezvous_no_relay, 1);
    }

    /// The derivation saturates: a success recorded between the two
    /// counter loads (the racing-read window the snapshot doc
    /// documents) must not underflow to u64::MAX.
    #[test]
    fn punches_failed_saturates_instead_of_underflowing() {
        let stats = TraversalStats::new();
        // Degenerate ordering: success without a recorded attempt.
        stats.record_punch_success();
        assert_eq!(stats.snapshot().punches_failed, 0, "saturating_sub");
    }
}
