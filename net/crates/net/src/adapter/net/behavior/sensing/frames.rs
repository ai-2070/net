//! `SensingInterestFrame` — the SI-0 **semantic form** of the
//! 0x0C02 `SUBPROTOCOL_SENSING_INTEREST` wire objects (plan §4.2,
//! v4.3 review 7).
//!
//! The v4.2 routing has two legs, so the frame family has two
//! registration shapes:
//!
//! - [`SensingInterestFrame::CapabilityRegistration`] — the
//!   **leader-addressed** leg (consumer → elected sensing leader).
//!   The interest digest COMMITS to selector + result mode but does
//!   not REVEAL them, so this leg must carry the full canonical
//!   interest: the leader RE-DERIVES the digest from the carried
//!   predicate + selector + mode + scope and cross-checks the
//!   carried `interest_digest` — a mismatch is protocol-invalid
//!   input, and the RE-DERIVED digest is the coalescing identity,
//!   never the claimed one
//!   (`SensingLeader::register_from_frame`, gate (r)).
//! - [`SensingInterestFrame::ProviderRegistration`] — the
//!   **provider-addressed** leg (leader → provider). Selector and
//!   result mode are deliberately absent: the provider evaluates the
//!   predicate, not the population. The provider re-derives and
//!   validates the predicate binding
//!   (`CanonicalConstraints::validate_inline`) exactly as before.
//! - [`SensingInterestFrame::Deregister`] — withdraw an interest at
//!   either stage.
//!
//! **`ConsumerLatencyBudget` appears in NO variant** — it is local
//! by definition (plan §3.3): a provider cannot know a consumer's
//! path cost, so the end-to-end budget never rides the wire and is
//! never provider-signed.
//!
//! **SI-0 boundary.** These frames are in-process objects with a
//! serde (JSON-friendly) round-trip so the gate (s) real-path test
//! can carry them over the existing routed transport. NO subprotocol
//! id is consumed here and the serde encoding is NOT the wire codec:
//! SI-1 freezes the codec + signing and commits the 0x0C02/0x0C03
//! ids only after gates (a)–(s) hold (plan §6). 32-byte identities
//! serialize as hex strings; `Duration` as serde's default
//! `{secs, nanos}` shape.

use std::time::Duration;

use super::identity::{
    AudienceScopeCommitment, CapabilityId, Digest256, InterestSpec, ProviderSelector, ResultMode,
    WorkLatencyEnvelope,
};

/// One frame of the sensing-interest subprotocol family (plan §4.2).
/// See the module docs for the two-leg shape and the SI-0/SI-1
/// boundary.
#[derive(Clone, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum SensingInterestFrame {
    /// Consumer → leader: the provider-free capability interest,
    /// carrying the full canonical predicate + selector + mode so
    /// the leader can re-derive the digest, coalesce BEFORE provider
    /// selection, and resolve bounded candidates.
    CapabilityRegistration {
        /// Capability the predicate targets.
        capability_id: CapabilityId,
        /// Inline canonical constraint bytes C
        /// ([`super::identity::CanonicalConstraints::canonical_bytes`],
        /// ≤ `max_constraint_bytes`).
        constraints: Vec<u8>,
        /// Digest the inline bytes must hash to
        /// (truncation/tampering detection, plan §4.2).
        constraints_digest: Digest256,
        /// Provider-evaluated latency envelope L.
        work_latency: WorkLatencyEnvelope,
        /// The provider population — the leader needs it to resolve.
        providers: ProviderSelector,
        /// The result cardinality — the leader needs it to bound
        /// exploration.
        result_mode: ResultMode,
        /// The sender's claimed interest identity. Cross-checked by
        /// re-derivation at the leader; never the coalescing
        /// identity by itself.
        interest_digest: Digest256,
        /// D — the delivery-continuity interval (min-dominance
        /// upstream; not identity).
        requested_sample_interval: Duration,
        /// Per-downstream soft-state lifetime.
        soft_state_ttl: Duration,
        /// Wire scope claim (v1: the owner-root commitment).
        /// Cross-checked against the session-proven root, never
        /// load-bearing (plan §4.10).
        audience_scope: AudienceScopeCommitment,
        /// The registering consumer's node id. Bound to the
        /// authenticated routed origin at the leader — NEVER trusted
        /// alone (plan §4.10, review 7).
        consumer: u64,
    },
    /// Leader → provider: the provider-targeted readiness interest.
    /// Selector and result mode are deliberately omitted (the
    /// provider evaluates the predicate, not the population).
    ProviderRegistration {
        /// The provider this branch targets (routes via
        /// `next_hop(target)`).
        target: u64,
        /// Capability the predicate targets.
        capability_id: CapabilityId,
        /// Inline canonical constraint bytes C.
        constraints: Vec<u8>,
        /// Digest the inline bytes must hash to.
        constraints_digest: Digest256,
        /// Provider-evaluated latency envelope L.
        work_latency: WorkLatencyEnvelope,
        /// The capability-interest identity this branch serves —
        /// re-derived and validated against the predicate binding at
        /// the provider.
        interest_digest: Digest256,
        /// Aggregated (strictest) D for the branch.
        requested_sample_interval: Duration,
        /// Soft-state lifetime of the branch registration.
        soft_state_ttl: Duration,
        /// Wire scope claim (cross-checked, never load-bearing).
        audience_scope: AudienceScopeCommitment,
    },
    /// Withdraw an interest: leader-addressed when `target` is
    /// `None`, provider-addressed (one branch) when `Some`.
    Deregister {
        /// The interest identity to withdraw.
        interest_digest: Digest256,
        /// Provider branch to withdraw, or `None` for the
        /// leader-addressed (whole-interest) withdrawal.
        target: Option<u64>,
    },
}

impl SensingInterestFrame {
    /// Build the leader-addressed registration for a spec: inline
    /// constraint bytes, both digests, and the consumer binding all
    /// derived from the same source, so an honest sender cannot
    /// produce an internally inconsistent frame.
    pub fn capability_registration(
        spec: &InterestSpec,
        requested_sample_interval: Duration,
        soft_state_ttl: Duration,
        consumer: u64,
    ) -> Self {
        Self::CapabilityRegistration {
            capability_id: spec.capability_id.clone(),
            constraints: spec.constraints.canonical_bytes(),
            constraints_digest: spec.constraints.constraints_digest(),
            work_latency: spec.work_latency,
            providers: spec.providers.clone(),
            result_mode: spec.result_mode,
            interest_digest: spec.interest_digest(),
            requested_sample_interval,
            soft_state_ttl,
            audience_scope: spec.audience,
            consumer,
        }
    }

    /// Build the provider-addressed registration for a resolved
    /// branch of a spec (selector/mode intentionally dropped; the
    /// digest still commits to them).
    pub fn provider_registration(
        spec: &InterestSpec,
        target: u64,
        requested_sample_interval: Duration,
        soft_state_ttl: Duration,
    ) -> Self {
        Self::ProviderRegistration {
            target,
            capability_id: spec.capability_id.clone(),
            constraints: spec.constraints.canonical_bytes(),
            constraints_digest: spec.constraints.constraints_digest(),
            work_latency: spec.work_latency,
            interest_digest: spec.interest_digest(),
            requested_sample_interval,
            soft_state_ttl,
            audience_scope: spec.audience,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::identity::{CanonicalConstraints, DisclosureClass};
    use super::*;

    fn spec() -> InterestSpec {
        InterestSpec {
            capability_id: CapabilityId::new("print.document"),
            constraints: CanonicalConstraints::from_entries([("color", "true"), ("media", "a4")])
                .unwrap(),
            work_latency: WorkLatencyEnvelope::start_within(Duration::from_secs(5)),
            providers: ProviderSelector::AnyAuthorized,
            result_mode: ResultMode::Any,
            disclosure_class: DisclosureClass::Owner,
            audience: AudienceScopeCommitment::from_bytes([0xAA; 32]),
        }
    }

    #[test]
    fn capability_registration_round_trips_through_json() {
        let frame = SensingInterestFrame::capability_registration(
            &spec(),
            Duration::from_millis(100),
            Duration::from_secs(30),
            0xA11CE,
        );
        let json = serde_json::to_string(&frame).unwrap();
        let back: SensingInterestFrame = serde_json::from_str(&json).unwrap();
        assert_eq!(back, frame);
    }

    #[test]
    fn provider_registration_round_trips_and_omits_selector_and_mode() {
        let frame = SensingInterestFrame::provider_registration(
            &spec(),
            0x77,
            Duration::from_millis(100),
            Duration::from_secs(30),
        );
        let json = serde_json::to_value(&frame).unwrap();
        let body = &json["ProviderRegistration"];
        assert!(body.is_object());
        // §4.2: the provider evaluates the predicate, not the
        // population — selector and mode never reach it.
        assert!(body.get("providers").is_none());
        assert!(body.get("result_mode").is_none());
        // §3.3: no variant carries a consumer budget — the field
        // name must not exist anywhere in the frame family.
        assert!(body.get("consumer_budget").is_none());
        let back: SensingInterestFrame = serde_json::from_value(json).unwrap();
        assert_eq!(back, frame);
    }

    #[test]
    fn deregister_round_trips_both_addressing_modes() {
        for target in [None, Some(0x77u64)] {
            let frame = SensingInterestFrame::Deregister {
                interest_digest: spec().interest_digest(),
                target,
            };
            let json = serde_json::to_string(&frame).unwrap();
            let back: SensingInterestFrame = serde_json::from_str(&json).unwrap();
            assert_eq!(back, frame);
        }
    }

    #[test]
    fn helper_builds_internally_consistent_frames() {
        let spec = spec();
        let frame = SensingInterestFrame::capability_registration(
            &spec,
            Duration::from_millis(100),
            Duration::from_secs(30),
            0xA,
        );
        let SensingInterestFrame::CapabilityRegistration {
            constraints,
            constraints_digest,
            interest_digest,
            audience_scope,
            ..
        } = &frame
        else {
            panic!("helper must build the leader-addressed variant");
        };
        // The inline bytes validate against the carried digest, and
        // the claimed interest digest matches what the leader will
        // re-derive.
        let parsed = CanonicalConstraints::validate_inline(constraints, constraints_digest)
            .expect("inline bytes must match the carried digest");
        assert_eq!(parsed, spec.constraints);
        assert_eq!(*interest_digest, spec.interest_digest());
        assert_eq!(*audience_scope, spec.audience);
    }

    #[test]
    fn digest_fields_serialize_as_hex_strings() {
        // Pin the JSON-friendly identity encoding: 64 lowercase hex
        // chars, exactly the Debug rendering's payload.
        let frame = SensingInterestFrame::Deregister {
            interest_digest: Digest256::from_bytes([0x0F; 32]),
            target: None,
        };
        let json = serde_json::to_value(&frame).unwrap();
        assert_eq!(
            json["Deregister"]["interest_digest"],
            serde_json::Value::String("0f".repeat(32)),
        );
    }
}
