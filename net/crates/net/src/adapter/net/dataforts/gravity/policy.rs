//! `DataGravityPolicy` — per-channel tuning for the heat-counter
//! emission cycle + the pure emission-decision function the
//! runtime hooks call after every read.
//!
//! Decision shape:
//!
//! - **Emit a new tag** when `(current_rate / last_emitted) ≥
//!   emit_threshold_ratio` (default `2.0`) — hot enough to
//!   surface to peers.
//! - **Withdraw** (emit rate=0) when the rate decays to zero —
//!   peers drop the heat annotation.
//! - **Otherwise** suppress emission. Re-emission floods the
//!   capability-announcement bus; the throttle keeps wire
//!   traffic bounded.
//!
//! Locked defaults from `DATAFORTS_PLAN.md` § Phase 4.

use std::time::Duration;

/// Default emission threshold ratio. `2.0` means a heat tag
/// re-emits only when the read rate doubles (or halves) since
/// the last emission. Chosen to keep capability-announcement
/// traffic bounded under steady-state read patterns; a workload
/// that fluctuates uniformly across the threshold emits at most
/// `log2(peak / baseline)` tags per channel per lifetime.
pub const DEFAULT_EMIT_THRESHOLD_RATIO: f32 = 2.0;

/// Default decay half-life. `30 min` — fast enough that read
/// patterns flowing over operator-relevant timescales (minutes,
/// not hours) surface as heat changes, slow enough that
/// transient bursts don't churn the emission path.
pub const DEFAULT_DECAY_HALF_LIFE_SECS: u64 = 30 * 60;

/// Minimum emission ratio. `1.01` lets operators bias toward
/// "emit aggressively" without permitting 1.0 (which would
/// re-emit on every bump — pathological). Below this fires
/// the validator at construction.
pub const MIN_EMIT_THRESHOLD_RATIO: f32 = 1.01;

/// Maximum emission ratio. `10.0` is a sanity ceiling — above
/// this the policy approaches "never emit," which is equivalent
/// to disabling the feature.
pub const MAX_EMIT_THRESHOLD_RATIO: f32 = 10.0;

/// Per-channel configuration for the data-gravity heat-counter
/// emission cycle. Carried on
/// [`crate::adapter::net::redex::RedexFileConfig::data_gravity`]
/// (added in a subsequent slice); `None` keeps the channel
/// gravity-free (no heat tags emitted, no counter maintained).
///
/// Validation rules (enforced by [`Self::validate`]):
///
/// - `emit_threshold_ratio` is finite,
///   `>= MIN_EMIT_THRESHOLD_RATIO`, `<= MAX_EMIT_THRESHOLD_RATIO`.
/// - `decay_half_life` is non-zero.
#[derive(Debug, Clone)]
pub struct DataGravityPolicy {
    /// Whether the counter + emission cycle is active for the
    /// channel. `false` keeps the policy carried through config
    /// surfaces without spinning up the per-channel state.
    pub enabled: bool,
    /// Re-emission threshold. Default
    /// [`DEFAULT_EMIT_THRESHOLD_RATIO`].
    pub emit_threshold_ratio: f32,
    /// Exponential-decay half-life for the read rate. Default
    /// [`DEFAULT_DECAY_HALF_LIFE_SECS`].
    pub decay_half_life: Duration,
}

impl Default for DataGravityPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            emit_threshold_ratio: DEFAULT_EMIT_THRESHOLD_RATIO,
            decay_half_life: Duration::from_secs(DEFAULT_DECAY_HALF_LIFE_SECS),
        }
    }
}

impl DataGravityPolicy {
    /// Construct with the locked Phase-4 defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder: set the enabled flag.
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Builder: set the emission threshold ratio.
    pub fn with_emit_threshold_ratio(mut self, ratio: f32) -> Self {
        self.emit_threshold_ratio = ratio;
        self
    }

    /// Builder: set the decay half-life.
    pub fn with_decay_half_life(mut self, half_life: Duration) -> Self {
        self.decay_half_life = half_life;
        self
    }

    /// Validate the locked invariants. Returns a typed error
    /// naming the offending field so binding-layer callers can
    /// surface operator-friendly diagnostics.
    pub fn validate(&self) -> Result<(), DataGravityPolicyError> {
        if !self.emit_threshold_ratio.is_finite()
            || self.emit_threshold_ratio < MIN_EMIT_THRESHOLD_RATIO
            || self.emit_threshold_ratio > MAX_EMIT_THRESHOLD_RATIO
        {
            return Err(DataGravityPolicyError::EmitThresholdOutOfRange {
                got: self.emit_threshold_ratio,
                min: MIN_EMIT_THRESHOLD_RATIO,
                max: MAX_EMIT_THRESHOLD_RATIO,
            });
        }
        if self.decay_half_life.is_zero() {
            return Err(DataGravityPolicyError::DecayHalfLifeZero);
        }
        Ok(())
    }
}

/// Outcome of the pure emission-decision function. Names the
/// path so the runtime can route the right metric bump.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EmissionDecision {
    /// No emission — suppress. Rate hasn't crossed the
    /// threshold; the last-emitted value is still
    /// representative.
    Suppress,
    /// Emit a new heat tag with the supplied rate.
    Emit {
        /// Decayed read-rate to carry on the wire. The substrate
        /// clamps to `[0.0, 1.0]` at emission time.
        rate: f64,
    },
    /// Withdraw — emit `heat:<chain>=0`. Peers drop the
    /// annotation.
    Withdraw,
}

/// Pure-function emission gate. Caller passes the current
/// (decayed) read rate, the last-emitted value (or `None` if
/// no prior emission), and the configured policy. Returns the
/// `EmissionDecision` the runtime acts on.
///
/// Locked semantics:
///
/// - `current_rate == 0.0` + `last_emitted == Some(>0)` → withdraw.
/// - `current_rate > 0` + `last_emitted == None` → emit (first
///   announcement).
/// - `current_rate > 0` + `last_emitted == Some(prev)` and
///   `current_rate / prev >= ratio` (or
///   `prev / current_rate >= ratio` when rate fell) → emit.
/// - Otherwise → suppress.
pub fn should_emit_heat(
    current_rate: f64,
    last_emitted: Option<f64>,
    policy: &DataGravityPolicy,
) -> EmissionDecision {
    if !policy.enabled {
        return EmissionDecision::Suppress;
    }
    // Reject non-finite / negative rates defensively — the
    // caller's decay loop should never produce these but a
    // misuse should not corrupt the emission path.
    if !current_rate.is_finite() || current_rate < 0.0 {
        return EmissionDecision::Suppress;
    }
    let ratio = policy.emit_threshold_ratio as f64;
    match (last_emitted, current_rate) {
        // No prior emission — emit if there's any heat to surface.
        (None, r) if r > 0.0 => EmissionDecision::Emit { rate: r },
        // No prior, no heat — nothing to do.
        (None, _) => EmissionDecision::Suppress,
        // Withdrawn-to-zero — emit a withdrawal tag so peers
        // drop the annotation.
        (Some(prev), 0.0) if prev > 0.0 => EmissionDecision::Withdraw,
        // Already withdrawn + still zero — suppress.
        (Some(_), 0.0) => EmissionDecision::Suppress,
        // Rate moved meaningfully — emit. Symmetric:
        // rate doubled (current/prev >= ratio) OR
        // rate halved (prev/current >= ratio).
        (Some(prev), r) => {
            if prev <= 0.0 {
                // Defensive: a prior emission with non-positive
                // value shouldn't happen but treat as "emit fresh."
                EmissionDecision::Emit { rate: r }
            } else if (r / prev) >= ratio || (prev / r) >= ratio {
                EmissionDecision::Emit { rate: r }
            } else {
                EmissionDecision::Suppress
            }
        }
    }
}

/// Validation errors for [`DataGravityPolicy`].
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum DataGravityPolicyError {
    /// `emit_threshold_ratio` outside `[1.01, 10.0]` or non-finite.
    #[error(
        "data-gravity emit_threshold_ratio {got} outside [{min}, {max}] or non-finite"
    )]
    EmitThresholdOutOfRange {
        /// Configured value.
        got: f32,
        /// Minimum permitted value.
        min: f32,
        /// Maximum permitted value.
        max: f32,
    },
    /// `decay_half_life` is zero. A zero half-life decays
    /// instantly and produces a flapping emission cycle.
    #[error("data-gravity decay_half_life must be non-zero")]
    DecayHalfLifeZero,
    /// `enable_gravity_for_greedy` was called before greedy
    /// itself was installed. Operators must call
    /// `enable_greedy_dataforts` first.
    #[error("data-gravity requires greedy to be enabled first")]
    GreedyNotEnabled,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_validates() {
        DataGravityPolicy::default()
            .validate()
            .expect("defaults must validate");
    }

    #[test]
    fn emit_threshold_below_min_rejected() {
        let p = DataGravityPolicy::default().with_emit_threshold_ratio(1.0);
        let err = p.validate().expect_err("ratio 1.0 must reject");
        assert!(matches!(
            err,
            DataGravityPolicyError::EmitThresholdOutOfRange { .. }
        ));
    }

    #[test]
    fn emit_threshold_above_max_rejected() {
        let p = DataGravityPolicy::default().with_emit_threshold_ratio(20.0);
        let err = p.validate().expect_err("ratio 20.0 must reject");
        assert!(matches!(
            err,
            DataGravityPolicyError::EmitThresholdOutOfRange { .. }
        ));
    }

    #[test]
    fn emit_threshold_nan_rejected() {
        let p = DataGravityPolicy::default().with_emit_threshold_ratio(f32::NAN);
        let err = p.validate().expect_err("NaN ratio must reject");
        assert!(matches!(
            err,
            DataGravityPolicyError::EmitThresholdOutOfRange { .. }
        ));
    }

    #[test]
    fn decay_half_life_zero_rejected() {
        let p = DataGravityPolicy::default().with_decay_half_life(Duration::ZERO);
        let err = p.validate().expect_err("zero half-life must reject");
        assert!(matches!(err, DataGravityPolicyError::DecayHalfLifeZero));
    }

    // ---- should_emit_heat ----

    fn policy() -> DataGravityPolicy {
        DataGravityPolicy::default()
    }

    #[test]
    fn disabled_policy_always_suppresses() {
        let p = policy().with_enabled(false);
        assert_eq!(
            should_emit_heat(10.0, Some(1.0), &p),
            EmissionDecision::Suppress
        );
    }

    #[test]
    fn first_emission_fires_when_heat_present() {
        let p = policy();
        match should_emit_heat(5.0, None, &p) {
            EmissionDecision::Emit { rate } => assert_eq!(rate, 5.0),
            other => panic!("expected Emit, got {other:?}"),
        }
    }

    #[test]
    fn first_emission_suppressed_with_zero_rate() {
        let p = policy();
        assert_eq!(
            should_emit_heat(0.0, None, &p),
            EmissionDecision::Suppress
        );
    }

    #[test]
    fn doubled_rate_emits() {
        let p = policy();
        match should_emit_heat(20.0, Some(10.0), &p) {
            EmissionDecision::Emit { rate } => assert_eq!(rate, 20.0),
            other => panic!("expected Emit, got {other:?}"),
        }
    }

    #[test]
    fn halved_rate_emits() {
        let p = policy();
        match should_emit_heat(5.0, Some(10.0), &p) {
            EmissionDecision::Emit { rate } => assert_eq!(rate, 5.0),
            other => panic!("expected Emit, got {other:?}"),
        }
    }

    #[test]
    fn sub_threshold_change_suppresses() {
        let p = policy();
        // Rate moved from 10 → 15 (ratio 1.5 < 2.0 default).
        assert_eq!(
            should_emit_heat(15.0, Some(10.0), &p),
            EmissionDecision::Suppress
        );
    }

    #[test]
    fn decay_to_zero_emits_withdrawal() {
        let p = policy();
        assert_eq!(
            should_emit_heat(0.0, Some(5.0), &p),
            EmissionDecision::Withdraw
        );
    }

    #[test]
    fn already_withdrawn_suppresses() {
        let p = policy();
        // Last emission was 0.0; current is 0.0 — no change.
        assert_eq!(
            should_emit_heat(0.0, Some(0.0), &p),
            EmissionDecision::Suppress
        );
    }

    #[test]
    fn negative_rate_suppresses_defensively() {
        let p = policy();
        assert_eq!(
            should_emit_heat(-1.0, Some(1.0), &p),
            EmissionDecision::Suppress
        );
    }

    #[test]
    fn non_finite_rate_suppresses_defensively() {
        let p = policy();
        assert_eq!(
            should_emit_heat(f64::NAN, Some(1.0), &p),
            EmissionDecision::Suppress
        );
        assert_eq!(
            should_emit_heat(f64::INFINITY, Some(1.0), &p),
            EmissionDecision::Suppress
        );
    }

    #[test]
    fn higher_threshold_suppresses_doubled_rate() {
        // ratio = 3.0: doubling (2×) is no longer enough.
        let p = policy().with_emit_threshold_ratio(3.0);
        assert_eq!(
            should_emit_heat(20.0, Some(10.0), &p),
            EmissionDecision::Suppress
        );
        // tripling DOES fire.
        match should_emit_heat(30.0, Some(10.0), &p) {
            EmissionDecision::Emit { rate } => assert_eq!(rate, 30.0),
            other => panic!("expected Emit, got {other:?}"),
        }
    }
}
