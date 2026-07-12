//! The frozen `ReadinessEvaluator` contract (plan §4.4, SI-0
//! item 12).
//!
//! Capability integrations implement ONE narrow trait; without it
//! every integration invents its own meaning for `ProviderUnknown`.
//! The five-variant result model is frozen in SI-0 (SI-1 gate
//! condition (j)): the three non-Ready/NotReady variants all project
//! onto the wire as `ProviderUnknown`, but each carries a distinct
//! compact `status_reason` code — observability keeps the
//! distinction even though consumers treat all three as Unknown.
//!
//! Two provider-side refusals live here as well:
//!
//! - **Unsupported cadence is refused, not silently degraded** — a
//!   coalesced strictest D below the provider's floor produces a
//!   structured [`CadenceRefusal`] carrying `minimum_supported`, so
//!   relays can partition their downstreams on it (§4.4; the
//!   partitioning itself is the interest table's job).
//! - **A `constraints_digest` mismatch is malformed or tampered
//!   protocol input**, not merely an unevaluable predicate: it
//!   increments the protocol-invalid/security counter even though it
//!   projects publicly as `ProviderUnknown { InvalidConstraints }`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use super::continuity::AttestedStatus;
use super::identity::{
    CanonicalConstraints, CapabilityId, ConstraintError, Digest256, WorkLatencyEnvelope,
};

/// Default provider cadence floor (plan §5,
/// `attestation_cadence_floor`): a coalesced strictest D below this
/// is refused with `sampling_interval_unsupported`.
pub const DEFAULT_ATTESTATION_CADENCE_FLOOR: Duration = Duration::from_millis(50);

/// The semantic inputs of one predicate evaluation. The spike
/// freezes these parameters — SI-3 binds the fold's capability entry
/// to them when the origin emitter lands; the entry adds context,
/// never replaces a parameter.
///
/// v4: there is deliberately NO generation parameter — a
/// capability-directed interest cannot bind one provider's
/// generation (plan §3.2). The provider always evaluates against
/// its CURRENT generation and stamps that generation into the
/// signed attestation, where the observation key binds it.
#[derive(Clone, Copy, Debug)]
pub struct EvaluationRequest<'a> {
    /// Capability the predicate targets.
    pub capability_id: &'a CapabilityId,
    /// Work characteristics C (already digest-validated).
    pub constraints: &'a CanonicalConstraints,
    /// Latency envelope L.
    pub work_latency: &'a WorkLatencyEnvelope,
}

/// The frozen evaluation result model (plan §4.4).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReadinessEvaluation {
    /// The predicate holds.
    Ready {
        /// Provider's estimate of time-to-start, if it has one.
        estimated_start: Option<Duration>,
    },
    /// The predicate evaluated false.
    NotReady {
        /// Provider-defined compact detail code (queue full, model
        /// cold, disk pressure, …) — diagnostics, never semantics.
        reason: u16,
    },
    /// This capability cannot answer this (C, L) shape at all.
    UnsupportedPredicate,
    /// Transient local failure — the evaluator itself is degraded.
    TemporarilyUnevaluable,
    /// Constraints were undecodable or failed digest validation.
    InvalidConstraints,
}

/// Compact `status_reason` code carried beside the wire status
/// (plan §4.2/§4.4). Consumers treat every `ProviderUnknown` alike;
/// these codes exist for observability distributions (SI-7).
///
/// Serde exists for the SI-1 wire codec
/// (`super::wire::ReadinessAttestation`, postcard); the signature
/// transcript never hashes a serde encoding — it uses the
/// fixed-width canonical tag in `super::wire`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum StatusReason {
    /// No detail (the normal Ready case).
    None,
    /// Provider-defined NotReady detail code.
    Provider(u16),
    /// Capability cannot answer this predicate shape.
    UnsupportedPredicate,
    /// Transient evaluator failure.
    TemporarilyUnevaluable,
    /// Undecodable / digest-mismatched constraints.
    InvalidConstraints,
    /// The coalesced strictest D was below the provider floor
    /// ([`CadenceRefusal`]).
    SamplingIntervalUnsupported,
}

/// Project an evaluation onto the wire pair
/// `(attested status, status_reason)` (plan §4.4): the three
/// non-Ready/NotReady variants collapse to `ProviderUnknown` with
/// distinct reasons.
pub const fn project_evaluation(
    evaluation: &ReadinessEvaluation,
) -> (AttestedStatus, StatusReason) {
    match evaluation {
        ReadinessEvaluation::Ready { .. } => (AttestedStatus::Ready, StatusReason::None),
        ReadinessEvaluation::NotReady { reason } => {
            (AttestedStatus::NotReady, StatusReason::Provider(*reason))
        }
        ReadinessEvaluation::UnsupportedPredicate => (
            AttestedStatus::ProviderUnknown,
            StatusReason::UnsupportedPredicate,
        ),
        ReadinessEvaluation::TemporarilyUnevaluable => (
            AttestedStatus::ProviderUnknown,
            StatusReason::TemporarilyUnevaluable,
        ),
        ReadinessEvaluation::InvalidConstraints => (
            AttestedStatus::ProviderUnknown,
            StatusReason::InvalidConstraints,
        ),
    }
}

/// The one trait a capability integration implements (plan §4.4).
/// The provider compiles the predicate once per distinct
/// `interest_digest` and calls this at the aggregated cadence plus
/// on status edges; implementations must be cheap and non-blocking.
pub trait ReadinessEvaluator {
    /// Evaluate the predicate against current local state.
    fn evaluate(&self, request: &EvaluationRequest<'_>) -> ReadinessEvaluation;
}

/// Structured refusal for an unsupportable sampling interval (plan
/// §4.4): never a silently weaker stream. Relays partition their
/// downstreams on `minimum_supported` and re-register the
/// satisfiable aggregate.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CadenceRefusal {
    /// The provider's floor M — the strictest interval it will
    /// serve.
    pub minimum_supported: Duration,
}

impl CadenceRefusal {
    /// How the refusal appears on the attestation surface.
    pub const fn as_status(&self) -> (AttestedStatus, StatusReason) {
        (
            AttestedStatus::ProviderUnknown,
            StatusReason::SamplingIntervalUnsupported,
        )
    }
}

/// Provider-side cadence admission: a coalesced strictest D below
/// the floor is refused with the floor attached, so satisfiable
/// co-subscribers can be re-aggregated by the relay (§4.4).
pub const fn check_cadence(
    requested_strictest: Duration,
    floor: Duration,
) -> Result<(), CadenceRefusal> {
    // Duration lacks const PartialOrd; compare the raw parts.
    if requested_strictest.as_nanos() < floor.as_nanos() {
        Err(CadenceRefusal {
            minimum_supported: floor,
        })
    } else {
        Ok(())
    }
}

/// Sensing-plane counters (SI-0 subset — SI-7 grows the full stats
/// surface). Shared-reference friendly: relaxed atomics, monotonic,
/// diagnostics only.
#[derive(Default, Debug)]
pub struct SensingCounters {
    /// Every constraint rejection (any [`ConstraintError`]).
    pub invalid_constraints: AtomicU64,
    /// The security-relevant subset: protocol-invalid input —
    /// constraint digest mismatches (plan §4.4) and wire scope
    /// claims the session does not back (plan §4.10).
    pub protocol_invalid: AtomicU64,
    /// Structured cadence refusals issued.
    pub cadence_refusals: AtomicU64,
    /// Scope-validation refusals (plan §4.10) — every
    /// [`super::scope::ScopeError`], security-relevant or not.
    pub scope_refusals: AtomicU64,
}

impl SensingCounters {
    /// Snapshot one counter (test/observability convenience).
    pub fn get(counter: &AtomicU64) -> u64 {
        counter.load(Ordering::Relaxed)
    }
}

/// Provider-side inline-constraint intake (plan §4.2): parse +
/// digest-validate, counting rejections. A digest mismatch counts on
/// BOTH `invalid_constraints` and `protocol_invalid`; plain decode
/// failures count only on the former. The caller maps any `Err` to
/// `ReadinessEvaluation::InvalidConstraints`.
pub fn validate_interest_constraints(
    bytes: &[u8],
    claimed: &Digest256,
    counters: &SensingCounters,
) -> Result<CanonicalConstraints, ConstraintError> {
    match CanonicalConstraints::validate_inline(bytes, claimed) {
        Ok(constraints) => Ok(constraints),
        Err(error) => {
            counters.invalid_constraints.fetch_add(1, Ordering::Relaxed);
            if error.is_security_relevant() {
                counters.protocol_invalid.fetch_add(1, Ordering::Relaxed);
            }
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal integration: readiness is driven by a "load"
    /// constraint, and unrecognized constraint keys are an
    /// unsupported predicate — exactly the shape SI-3's real
    /// evaluators will take.
    struct LoadEvaluator {
        current_load: u16,
    }

    impl ReadinessEvaluator for LoadEvaluator {
        fn evaluate(&self, request: &EvaluationRequest<'_>) -> ReadinessEvaluation {
            let Some(max_load) = request.constraints.get("max_load") else {
                return ReadinessEvaluation::UnsupportedPredicate;
            };
            let Ok(max_load) = max_load.parse::<u16>() else {
                return ReadinessEvaluation::InvalidConstraints;
            };
            if self.current_load <= max_load {
                ReadinessEvaluation::Ready {
                    estimated_start: Some(Duration::from_millis(5)),
                }
            } else {
                ReadinessEvaluation::NotReady { reason: 42 }
            }
        }
    }

    fn request<'a>(
        capability_id: &'a CapabilityId,
        constraints: &'a CanonicalConstraints,
        work_latency: &'a WorkLatencyEnvelope,
    ) -> EvaluationRequest<'a> {
        EvaluationRequest {
            capability_id,
            constraints,
            work_latency,
        }
    }

    #[test]
    fn evaluator_contract_round_trips_through_a_real_impl() {
        let id = CapabilityId::new("job.run");
        let latency = WorkLatencyEnvelope::start_within(Duration::from_millis(100));
        let ok = CanonicalConstraints::from_entries([("max_load", "50")]).unwrap();
        let alien = CanonicalConstraints::from_entries([("gpu_class", "h100")]).unwrap();

        let idle = LoadEvaluator { current_load: 10 };
        let busy = LoadEvaluator { current_load: 90 };
        assert_eq!(
            idle.evaluate(&request(&id, &ok, &latency)),
            ReadinessEvaluation::Ready {
                estimated_start: Some(Duration::from_millis(5)),
            },
        );
        assert_eq!(
            busy.evaluate(&request(&id, &ok, &latency)),
            ReadinessEvaluation::NotReady { reason: 42 },
        );
        assert_eq!(
            idle.evaluate(&request(&id, &alien, &latency)),
            ReadinessEvaluation::UnsupportedPredicate,
        );
    }

    #[test]
    fn projection_collapses_to_provider_unknown_with_distinct_reasons() {
        use AttestedStatus as S;
        assert_eq!(
            project_evaluation(&ReadinessEvaluation::Ready {
                estimated_start: None,
            }),
            (S::Ready, StatusReason::None),
        );
        assert_eq!(
            project_evaluation(&ReadinessEvaluation::NotReady { reason: 7 }),
            (S::NotReady, StatusReason::Provider(7)),
        );
        // The three Unknown-projecting variants stay distinguishable
        // through status_reason even though the wire status is one
        // value.
        let unknowns = [
            (
                ReadinessEvaluation::UnsupportedPredicate,
                StatusReason::UnsupportedPredicate,
            ),
            (
                ReadinessEvaluation::TemporarilyUnevaluable,
                StatusReason::TemporarilyUnevaluable,
            ),
            (
                ReadinessEvaluation::InvalidConstraints,
                StatusReason::InvalidConstraints,
            ),
        ];
        for (evaluation, expected_reason) in unknowns {
            assert_eq!(
                project_evaluation(&evaluation),
                (S::ProviderUnknown, expected_reason),
            );
        }
    }

    #[test]
    fn cadence_below_floor_is_refused_with_the_floor_attached() {
        let floor = DEFAULT_ATTESTATION_CADENCE_FLOOR;
        assert_eq!(check_cadence(Duration::from_millis(50), floor), Ok(()));
        assert_eq!(check_cadence(Duration::from_secs(1), floor), Ok(()));
        let refusal = check_cadence(Duration::from_millis(20), floor).unwrap_err();
        assert_eq!(refusal.minimum_supported, floor);
        assert_eq!(
            refusal.as_status(),
            (
                AttestedStatus::ProviderUnknown,
                StatusReason::SamplingIntervalUnsupported,
            ),
        );
    }

    #[test]
    fn digest_mismatch_counts_as_security_plain_decode_failures_do_not() {
        let counters = SensingCounters::default();
        let constraints = CanonicalConstraints::from_entries([("a", "1")]).unwrap();
        let bytes = constraints.canonical_bytes();
        let right = constraints.constraints_digest();
        let wrong = Digest256::from_bytes([0u8; 32]);

        // Valid intake: no counters move.
        assert!(validate_interest_constraints(&bytes, &right, &counters).is_ok());
        assert_eq!(SensingCounters::get(&counters.invalid_constraints), 0);
        assert_eq!(SensingCounters::get(&counters.protocol_invalid), 0);

        // Digest mismatch: both counters move (plan §4.4 — malformed
        // or tampered protocol input, not merely unevaluable).
        assert_eq!(
            validate_interest_constraints(&bytes, &wrong, &counters),
            Err(ConstraintError::DigestMismatch),
        );
        assert_eq!(SensingCounters::get(&counters.invalid_constraints), 1);
        assert_eq!(SensingCounters::get(&counters.protocol_invalid), 1);

        // Truncation: only the invalid-constraints counter moves.
        assert!(validate_interest_constraints(&bytes[..3], &right, &counters).is_err());
        assert_eq!(SensingCounters::get(&counters.invalid_constraints), 2);
        assert_eq!(SensingCounters::get(&counters.protocol_invalid), 1);
    }
}
