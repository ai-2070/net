//! Tunables for the NAT-traversal subsystem.
//!
//! Defaults match the values locked in `docs/NAT_TRAVERSAL_PLAN.md`.
//! Callers override via `MeshBuilder`-exposed setters once the
//! per-stage wiring lands; defaults are sized so a caller who
//! enables `nat-traversal` without further configuration gets
//! sensible behavior out of the box.

use std::time::Duration;

/// Configuration for the NAT-traversal subsystem. Every value has
/// a default matching the plan; non-default values are accepted
/// via the SDK's `MeshBuilder` once stage 5 lands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraversalConfig {
    /// Per-probe timeout on a reflex request. The requester waits
    /// at most this long for the peer's reflex response before
    /// giving up on that probe and consulting the classification
    /// state machine with fewer data points.
    ///
    /// Default: 3 s — long enough for a routed multi-hop probe
    /// over a lossy link, short enough that a dead peer doesn't
    /// stall classification for noticeable time.
    pub reflex_timeout: Duration,

    /// Aggregate budget for the startup classification sweep.
    /// Multiple reflex probes run in parallel; classification
    /// records an `Unknown` and moves on after this deadline even
    /// if fewer than two responses have arrived.
    ///
    /// Default: 5 s.
    pub classify_deadline: Duration,

    /// Delay between `PunchIntroduce` receipt and the first
    /// keep-alive send. The 500 ms window gives both sides time
    /// to arm their timers + firewalls time to install state for
    /// the outbound connection-tracking row, while staying well
    /// under NTP-level clock skew.
    ///
    /// Default: 500 ms.
    pub punch_fire_lead: Duration,

    /// How long the receiver waits for inbound traffic on the
    /// punched path before declaring the single-shot punch
    /// failed (plan decision 10 — no internal retry).
    ///
    /// Default: 5 s.
    pub punch_deadline: Duration,

    /// How often the port-mapping renewal task re-issues the
    /// router mapping. Active only when the `port-mapping`
    /// cargo feature is on AND
    /// `MeshBuilder::try_port_mapping(true)` opted in.
    ///
    /// Default: 30 min — much shorter than the 3600 s TTL we
    /// request, matches plan decision 12.
    pub port_mapping_renewal: Duration,

    /// Fixed window for the rendezvous abuse budgets
    /// (`NAT_TRAVERSAL_V2_PLAN.md` Stage 2, closing review
    /// Finding 5). Both the coordinator per-requester budget and the
    /// responder per-source keep-alive-train budget reset on this
    /// cadence.
    ///
    /// Default: 10 s.
    pub punch_budget_window: Duration,

    /// Coordinator budget: max `PunchRequest`s a single requester may
    /// have mediated per [`Self::punch_budget_window`]. Beyond this
    /// the coordinator answers with a `PunchReject { RateLimited }`
    /// instead of fanning out an introduce.
    ///
    /// Default: 4.
    pub punch_requests_per_window: u32,

    /// Responder budget: max keep-alive trains a single introducing
    /// peer may trigger via *unsolicited* `PunchIntroduce`s per
    /// [`Self::punch_budget_window`]. Beyond this the introduce is
    /// dropped (no requester is waiting, so there is nothing to
    /// reject). Replaces Stage 1's temporary per-source cap.
    ///
    /// Default: 4.
    pub punch_trains_per_window: u32,

    /// Responder budget: global ceiling on outstanding unsolicited
    /// keep-alive trains, so a Sybil set of session peers each under
    /// their per-source budget can't multiply the aggregate. A train
    /// holds a slot for its scheduled lifetime (≤ `punch_deadline`).
    ///
    /// Default: 8.
    pub punch_trains_concurrent_max: usize,
}

impl Default for TraversalConfig {
    fn default() -> Self {
        Self {
            reflex_timeout: Duration::from_secs(3),
            classify_deadline: Duration::from_secs(5),
            punch_fire_lead: Duration::from_millis(500),
            punch_deadline: Duration::from_secs(5),
            port_mapping_renewal: Duration::from_secs(30 * 60),
            punch_budget_window: Duration::from_secs(10),
            punch_requests_per_window: 4,
            punch_trains_per_window: 4,
            punch_trains_concurrent_max: 8,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_plan() {
        // Guard against accidental tuning drift. Any change here
        // needs a matching update to docs/NAT_TRAVERSAL_PLAN.md.
        let cfg = TraversalConfig::default();
        assert_eq!(cfg.reflex_timeout, Duration::from_secs(3));
        assert_eq!(cfg.classify_deadline, Duration::from_secs(5));
        assert_eq!(cfg.punch_fire_lead, Duration::from_millis(500));
        assert_eq!(cfg.punch_deadline, Duration::from_secs(5));
        assert_eq!(cfg.port_mapping_renewal, Duration::from_secs(1800));
        assert_eq!(cfg.punch_budget_window, Duration::from_secs(10));
        assert_eq!(cfg.punch_requests_per_window, 4);
        assert_eq!(cfg.punch_trains_per_window, 4);
        assert_eq!(cfg.punch_trains_concurrent_max, 8);
    }
}
