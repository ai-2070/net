//! `GreedyConfig` — per-node tuning surface for the greedy-LRU
//! dataforts subsystem. Locked defaults match
//! `docs/misc/DATAFORTS_PLAN.md` § Phase 1.

use std::time::Duration;

use crate::adapter::net::behavior::placement::{ColocationPolicy, IntentMatchPolicy, ScopeLabel};

/// Default per-channel cache cap. 100 MiB — large enough for
/// typical chain working sets, small enough that a 10 GiB total
/// budget covers ~100 distinct channels before eviction kicks in.
pub const DEFAULT_PER_CHANNEL_CAP_BYTES: u64 = 100 * 1024 * 1024;

/// Floor on `per_channel_cap_bytes`. Channels smaller than 1 MiB
/// thrash on the per-event append path; reject the config at
/// construction rather than letting the runtime fight the LRU.
pub const MIN_PER_CHANNEL_CAP_BYTES: u64 = 1024 * 1024;

/// Default total cache cap across every channel. 10 GiB — sized
/// to fit comfortably on a small-disk edge node and large enough
/// to materially absorb working-set reads at gigabit-class link
/// rates.
pub const DEFAULT_TOTAL_CAP_BYTES: u64 = 10 * 1024 * 1024 * 1024;

/// Default proximity bound — chains whose home is more than 200 ms
/// away from the local node don't admit, on the theory that the
/// catch-up bandwidth required isn't worth the cache cost.
pub const DEFAULT_PROXIMITY_MAX_RTT_MS: u64 = 200;

/// Default I/O budget as a fraction of measured NIC peak. `0.25`
/// leaves three-quarters of the link for foreground publish
/// traffic.
pub const DEFAULT_BANDWIDTH_BUDGET_FRACTION: f32 = 0.25;

/// Default NIC peak the bandwidth budget is computed against, when
/// no override is supplied: 1 Gbps in bytes/sec. A measured probe
/// is intentionally still deferred (see `DATAFORTS_PLAN.md`
/// § Phase 1); operators on faster NICs should set
/// `nic_peak_bytes_per_s` explicitly to avoid proportional
/// under-utilization.
pub const DEFAULT_NIC_PEAK_BYTES_PER_S: u64 = 125_000_000;

/// Per-node configuration for [`crate::adapter::net::dataforts::greedy`].
///
/// Validation rules (enforced by [`Self::validate`]):
///
/// - `per_channel_cap_bytes >= MIN_PER_CHANNEL_CAP_BYTES`
/// - `total_cap_bytes >= per_channel_cap_bytes`
/// - `bandwidth_budget_fraction` is finite, `> 0.0`, `<= 1.0`
/// - `proximity_max_rtt` is non-zero
///
/// `scopes` may be empty — an empty scope set admits chains
/// regardless of `scope:` tags (greedy with no scope filter). To
/// reject all scope-tagged chains, leave scopes empty and configure
/// `intent_match: IntentMatchPolicy::Strict` so admission still
/// gates on intent.
#[derive(Debug, Clone)]
pub struct GreedyConfig {
    /// Local node's interesting scopes — chains whose `scope:` tag
    /// matches any of these are eligible for admission.
    pub scopes: Vec<ScopeLabel>,
    /// Maximum acceptable RTT to the chain's home node before
    /// admission rejects (proximity gate).
    pub proximity_max_rtt: Duration,
    /// Per-channel byte cap on the cache substrate. Reuses
    /// `RedexFileConfig::with_retention_max_bytes` once the cache
    /// runtime lands.
    pub per_channel_cap_bytes: u64,
    /// Total byte cap across every channel the greedy runtime is
    /// holding. LRU eviction drives toward this bound.
    pub total_cap_bytes: u64,
    /// I/O budget for greedy cache writes, expressed as a fraction
    /// of the measured NIC peak. Backpressures cache writes when
    /// the budget is exhausted so application traffic isn't
    /// crowded out.
    pub bandwidth_budget_fraction: f32,
    /// Override for the NIC peak (bytes/sec) the bandwidth budget
    /// computes against. `None` falls back to
    /// [`DEFAULT_NIC_PEAK_BYTES_PER_S`] (1 Gbps). Set this on
    /// deployments with > 1 Gbps NICs — otherwise greedy throttles
    /// at gigabit-class rates and the operator sees what looks
    /// like an admission-reject storm in
    /// `dataforts_greedy_admit_rejected_total{reason="bandwidth"}`.
    pub nic_peak_bytes_per_s: Option<u64>,
    /// Intent-axis admission policy. Reuses the substrate's
    /// `IntentMatchPolicy` so greedy uses the same eligibility
    /// shape as `StandardPlacement`.
    pub intent_match: IntentMatchPolicy,
    /// Colocation-axis admission policy. Soft preference by
    /// default — colocation tilts admission toward affinity but
    /// doesn't override capacity constraints.
    pub colocation_policy: ColocationPolicy,
}

impl Default for GreedyConfig {
    fn default() -> Self {
        Self {
            scopes: Vec::new(),
            proximity_max_rtt: Duration::from_millis(DEFAULT_PROXIMITY_MAX_RTT_MS),
            per_channel_cap_bytes: DEFAULT_PER_CHANNEL_CAP_BYTES,
            total_cap_bytes: DEFAULT_TOTAL_CAP_BYTES,
            bandwidth_budget_fraction: DEFAULT_BANDWIDTH_BUDGET_FRACTION,
            nic_peak_bytes_per_s: None,
            intent_match: IntentMatchPolicy::AnyOfLocalCapabilities,
            colocation_policy: ColocationPolicy::SoftPreference,
        }
    }
}

impl GreedyConfig {
    /// Construct a config with the locked defaults from
    /// `DATAFORTS_PLAN.md` § Phase 1.
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder: replace the scope set.
    pub fn with_scopes(mut self, scopes: Vec<ScopeLabel>) -> Self {
        self.scopes = scopes;
        self
    }

    /// Builder: set the proximity bound.
    pub fn with_proximity_max_rtt(mut self, rtt: Duration) -> Self {
        self.proximity_max_rtt = rtt;
        self
    }

    /// Builder: set the per-channel cap.
    pub fn with_per_channel_cap_bytes(mut self, cap: u64) -> Self {
        self.per_channel_cap_bytes = cap;
        self
    }

    /// Builder: set the total cap.
    pub fn with_total_cap_bytes(mut self, cap: u64) -> Self {
        self.total_cap_bytes = cap;
        self
    }

    /// Builder: set the bandwidth budget fraction.
    pub fn with_bandwidth_budget_fraction(mut self, fraction: f32) -> Self {
        self.bandwidth_budget_fraction = fraction;
        self
    }

    /// Builder: override the NIC peak (bytes/sec). `None` reverts
    /// to the [`DEFAULT_NIC_PEAK_BYTES_PER_S`] fallback.
    pub fn with_nic_peak_bytes_per_s(mut self, peak: Option<u64>) -> Self {
        self.nic_peak_bytes_per_s = peak;
        self
    }

    /// The effective NIC peak after applying the override-or-default
    /// rule. Saturates to [`DEFAULT_NIC_PEAK_BYTES_PER_S`] when
    /// `nic_peak_bytes_per_s` is `None` or `Some(0)`.
    pub fn effective_nic_peak_bytes_per_s(&self) -> u64 {
        match self.nic_peak_bytes_per_s {
            Some(v) if v > 0 => v,
            _ => DEFAULT_NIC_PEAK_BYTES_PER_S,
        }
    }

    /// Builder: set the intent-match policy.
    pub fn with_intent_match(mut self, policy: IntentMatchPolicy) -> Self {
        self.intent_match = policy;
        self
    }

    /// Builder: set the colocation policy.
    pub fn with_colocation_policy(mut self, policy: ColocationPolicy) -> Self {
        self.colocation_policy = policy;
        self
    }

    /// Validate the locked invariants. Returns a typed error
    /// naming the offending field so binding-layer callers can
    /// surface operator-friendly diagnostics.
    pub fn validate(&self) -> Result<(), GreedyConfigError> {
        if self.per_channel_cap_bytes < MIN_PER_CHANNEL_CAP_BYTES {
            return Err(GreedyConfigError::PerChannelCapTooLow {
                got: self.per_channel_cap_bytes,
                min: MIN_PER_CHANNEL_CAP_BYTES,
            });
        }
        if self.total_cap_bytes < self.per_channel_cap_bytes {
            return Err(GreedyConfigError::TotalCapBelowPerChannel {
                total: self.total_cap_bytes,
                per_channel: self.per_channel_cap_bytes,
            });
        }
        if !self.bandwidth_budget_fraction.is_finite()
            || self.bandwidth_budget_fraction <= 0.0
            || self.bandwidth_budget_fraction > 1.0
        {
            return Err(GreedyConfigError::BudgetFractionOutOfRange {
                got: self.bandwidth_budget_fraction,
            });
        }
        if self.proximity_max_rtt.is_zero() {
            return Err(GreedyConfigError::ProximityRttZero);
        }
        Ok(())
    }
}

/// Typed validation errors. Distinct variants per invariant so
/// the binding layer can route to language-idiomatic error
/// classes without parsing strings.
// `Eq` intentionally omitted — `BudgetFractionOutOfRange` carries
// an `f32`, which has NaN asymmetry. `PartialEq` is sufficient for
// the typical "compare against an expected error" pattern in tests.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum GreedyConfigError {
    /// `per_channel_cap_bytes` is below the floor.
    #[error("greedy per_channel_cap_bytes {got} below minimum {min}")]
    PerChannelCapTooLow {
        /// Configured value.
        got: u64,
        /// Minimum permitted value.
        min: u64,
    },
    /// `total_cap_bytes < per_channel_cap_bytes`. A total budget
    /// smaller than a single channel's cap can't admit any
    /// channel.
    #[error("greedy total_cap_bytes {total} must be ≥ per_channel_cap_bytes {per_channel}")]
    TotalCapBelowPerChannel {
        /// Configured total.
        total: u64,
        /// Configured per-channel cap.
        per_channel: u64,
    },
    /// `bandwidth_budget_fraction` outside `(0.0, 1.0]` or
    /// non-finite (NaN / ±inf).
    #[error("greedy bandwidth_budget_fraction {got} outside (0.0, 1.0] or non-finite")]
    BudgetFractionOutOfRange {
        /// Configured value.
        got: f32,
    },
    /// `proximity_max_rtt` is zero. A zero RTT bound excludes every
    /// non-local peer and produces a single-node cache — almost
    /// certainly a misconfig.
    #[error("greedy proximity_max_rtt must be non-zero")]
    ProximityRttZero,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_valid() {
        GreedyConfig::default()
            .validate()
            .expect("defaults must validate");
    }

    #[test]
    fn per_channel_cap_below_floor_rejected() {
        let cfg = GreedyConfig::default().with_per_channel_cap_bytes(1024);
        let err = cfg.validate().expect_err("1 KiB cap must reject");
        assert!(matches!(
            err,
            GreedyConfigError::PerChannelCapTooLow { got: 1024, .. }
        ));
    }

    #[test]
    fn total_cap_below_per_channel_rejected() {
        let cfg = GreedyConfig::default()
            .with_per_channel_cap_bytes(200 * 1024 * 1024)
            .with_total_cap_bytes(100 * 1024 * 1024);
        let err = cfg
            .validate()
            .expect_err("total below per-channel must reject");
        assert!(matches!(
            err,
            GreedyConfigError::TotalCapBelowPerChannel { .. }
        ));
    }

    #[test]
    fn budget_fraction_zero_rejected() {
        let cfg = GreedyConfig::default().with_bandwidth_budget_fraction(0.0);
        let err = cfg.validate().expect_err("zero fraction must reject");
        assert!(matches!(
            err,
            GreedyConfigError::BudgetFractionOutOfRange { .. }
        ));
    }

    #[test]
    fn budget_fraction_above_one_rejected() {
        let cfg = GreedyConfig::default().with_bandwidth_budget_fraction(1.5);
        let err = cfg.validate().expect_err("fraction above 1.0 must reject");
        assert!(matches!(
            err,
            GreedyConfigError::BudgetFractionOutOfRange { .. }
        ));
    }

    #[test]
    fn budget_fraction_nan_rejected() {
        let cfg = GreedyConfig::default().with_bandwidth_budget_fraction(f32::NAN);
        let err = cfg.validate().expect_err("NaN fraction must reject");
        assert!(matches!(
            err,
            GreedyConfigError::BudgetFractionOutOfRange { .. }
        ));
    }

    #[test]
    fn budget_fraction_inf_rejected() {
        let cfg = GreedyConfig::default().with_bandwidth_budget_fraction(f32::INFINITY);
        let err = cfg.validate().expect_err("inf fraction must reject");
        assert!(matches!(
            err,
            GreedyConfigError::BudgetFractionOutOfRange { .. }
        ));
    }

    #[test]
    fn proximity_rtt_zero_rejected() {
        let cfg = GreedyConfig::default().with_proximity_max_rtt(Duration::ZERO);
        let err = cfg.validate().expect_err("zero RTT must reject");
        assert!(matches!(err, GreedyConfigError::ProximityRttZero));
    }

    #[test]
    fn boundary_values_admitted() {
        // Floor values for each axis — should all validate.
        let cfg = GreedyConfig::default()
            .with_per_channel_cap_bytes(MIN_PER_CHANNEL_CAP_BYTES)
            .with_total_cap_bytes(MIN_PER_CHANNEL_CAP_BYTES)
            .with_bandwidth_budget_fraction(1.0)
            .with_proximity_max_rtt(Duration::from_nanos(1));
        cfg.validate().expect("boundary values are admissible");
    }
}
