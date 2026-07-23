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
//!   **provider-addressed** leg (leader → provider). The provider
//!   evaluates the predicate, not the population — but selector,
//!   result mode, and disclosure class are CARRIED anyway (review 7
//!   sign-off, plan §4.2 amendment): the provider must reconstruct
//!   the COMPLETE interest identity, re-derive `interest_digest`,
//!   and reject any mismatch as protocol-invalid BEFORE it evaluates
//!   or signs — it must never sign an attestation against an opaque,
//!   unvalidated interest-digest claim
//!   ([`SensingInterestFrame::validate_provider_registration`]).
//! - [`SensingInterestFrame::Deregister`] — withdraw an interest at
//!   either stage.
//!
//! Both registration legs share ONE intake pipeline
//! ([`SensingInterestFrame::validated_spec`]): canonicalize +
//! digest-validate the inline constraints, reconstruct the COMPLETE
//! [`InterestSpec`] from the carried fields, re-derive the interest
//! digest, and cross-check the claim — the re-derived identity is
//! the only one that ever coalesces or gets signed.
//!
//! **`ConsumerLatencyBudget` appears in NO variant** — it is local
//! by definition (plan §3.3): a provider cannot know a consumer's
//! path cost, so the end-to-end budget never rides the wire and is
//! never provider-signed.
//!
//! **SI-1 boundary.** These are the semantic frame shapes; the
//! committed wire form lives in [`super::wire`] — postcard over
//! these serde derives, strict-decoded under the 0x0C02 id
//! ([`super::wire::SUBPROTOCOL_SENSING_INTEREST`]). The
//! human-readable serde form (32-byte identities as hex strings,
//! `Duration` as serde's default `{secs, nanos}` shape) remains for
//! the SI-0 real-path tests and diagnostics.

use std::fmt;
use std::sync::atomic::Ordering;
use std::time::Duration;

use super::super::org::OrgMembershipCert;
use super::evaluator::{validate_interest_constraints, SensingCounters};
use super::identity::{
    AudienceScopeCommitment, CanonicalConstraints, CapabilityId, ConstraintError, Digest256,
    DisclosureClass, InterestSpec, ProviderSelector, ResultMode, WorkLatencyEnvelope,
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
    /// The provider evaluates the predicate, not the population —
    /// but selector, result mode, and disclosure class ride along
    /// for COMPLETE digest verification (review 7 sign-off, plan
    /// §4.2): the provider re-derives the full interest identity and
    /// signs only the VALIDATED digest, never an opaque claim.
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
        /// The provider population. Carried for digest verification
        /// only — it never affects provider-side predicate
        /// evaluation (plan §4.2, review 7).
        providers: ProviderSelector,
        /// The result cardinality. Carried for digest verification
        /// only.
        result_mode: ResultMode,
        /// The disclosure class. Carried for digest verification
        /// only.
        disclosure_class: DisclosureClass,
        /// Wire scope claim (cross-checked, never load-bearing);
        /// also digest-bound as the interest audience.
        audience_scope: AudienceScopeCommitment,
        /// The capability-interest identity this branch serves —
        /// re-derived from the COMPLETE carried fields and validated
        /// at the provider before anything is evaluated or signed
        /// ([`Self::validate_provider_registration`]).
        interest_digest: Digest256,
        /// Aggregated (strictest) D for the branch.
        requested_sample_interval: Duration,
        /// Soft-state lifetime of the branch registration.
        soft_state_ttl: Duration,
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
    /// **Organization-authenticated** leader-addressed registration
    /// (OLB org-auth slice) — the [`Self::CapabilityRegistration`]
    /// semantic fields plus the registering hop's membership
    /// certificate. Postcard variant index **3** (appended; the
    /// legacy indices 0/1/2 are frozen). The membership is validated
    /// at every receiving hop BEFORE any table mutation
    /// (`verify_org_sensing_registration`, commit 2); this variant is
    /// structurally dark until that gate exists.
    OrgCapabilityRegistration {
        /// Capability the predicate targets.
        capability_id: CapabilityId,
        /// Inline canonical constraint bytes C.
        constraints: Vec<u8>,
        /// Digest the inline bytes must hash to.
        constraints_digest: Digest256,
        /// Provider-evaluated latency envelope L.
        work_latency: WorkLatencyEnvelope,
        /// The provider population — the leader needs it to resolve.
        providers: ProviderSelector,
        /// The result cardinality — the leader needs it to bound
        /// exploration.
        result_mode: ResultMode,
        /// The sender's claimed interest identity (re-derived + cross
        /// checked; never the coalescing identity by itself).
        interest_digest: Digest256,
        /// D — the delivery-continuity interval (not identity).
        requested_sample_interval: Duration,
        /// Per-downstream soft-state lifetime.
        soft_state_ttl: Duration,
        /// Wire scope claim — the owner-root/organization commitment;
        /// cross-checked against `subscriber_membership.org_id`'s
        /// canonical sensing commitment, never load-bearing alone.
        audience_scope: AudienceScopeCommitment,
        /// The registering consumer's node id (bound to the
        /// authenticated origin at intake, never trusted alone).
        consumer: u64,
        /// The registering hop's organization membership certificate.
        /// Rides the wire as its canonical 156-byte encoding (the
        /// type's manual serde); verified at every receiving hop.
        subscriber_membership: OrgMembershipCert,
    },
    /// **Organization-authenticated** provider-addressed registration
    /// (OLB org-auth slice) — the [`Self::ProviderRegistration`]
    /// semantic fields plus the re-registering hop's membership
    /// certificate. Postcard variant index **4** (appended). A relay
    /// re-authors this with its OWN membership; it never forwards the
    /// downstream consumer's certificate (commit 3). Structurally
    /// dark until the membership gate exists (commit 2).
    OrgProviderRegistration {
        /// The provider this branch targets.
        target: u64,
        /// Capability the predicate targets.
        capability_id: CapabilityId,
        /// Inline canonical constraint bytes C.
        constraints: Vec<u8>,
        /// Digest the inline bytes must hash to.
        constraints_digest: Digest256,
        /// Provider-evaluated latency envelope L.
        work_latency: WorkLatencyEnvelope,
        /// The provider population (carried for digest verification).
        providers: ProviderSelector,
        /// The result cardinality (carried for digest verification).
        result_mode: ResultMode,
        /// The disclosure class (carried for digest verification).
        disclosure_class: DisclosureClass,
        /// Wire scope claim (cross-checked; also digest-bound).
        audience_scope: AudienceScopeCommitment,
        /// The capability-interest identity this branch serves.
        interest_digest: Digest256,
        /// Aggregated (strictest) D for the branch.
        requested_sample_interval: Duration,
        /// Soft-state lifetime of the branch registration.
        soft_state_ttl: Duration,
        /// The re-registering hop's own organization membership
        /// certificate (never the consumer's). Canonical 156-byte
        /// encoding; verified at every receiving hop.
        subscriber_membership: OrgMembershipCert,
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
    /// branch of a spec. Selector, mode, and disclosure class are
    /// carried so the provider can verify the COMPLETE digest it
    /// will sign (review 7 sign-off, plan §4.2).
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
            providers: spec.providers.clone(),
            result_mode: spec.result_mode,
            disclosure_class: spec.disclosure_class,
            audience_scope: spec.audience,
            interest_digest: spec.interest_digest(),
            requested_sample_interval,
            soft_state_ttl,
        }
    }

    /// Build the organization-authenticated leader-addressed
    /// registration for a spec, carrying the registering hop's
    /// membership certificate (OLB org-auth slice). Same semantic
    /// derivation as [`Self::capability_registration`].
    pub fn org_capability_registration(
        spec: &InterestSpec,
        requested_sample_interval: Duration,
        soft_state_ttl: Duration,
        consumer: u64,
        subscriber_membership: OrgMembershipCert,
    ) -> Self {
        Self::OrgCapabilityRegistration {
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
            subscriber_membership,
        }
    }

    /// Build the organization-authenticated provider-addressed
    /// registration for a resolved branch, carrying the re-registering
    /// hop's OWN membership certificate (OLB org-auth slice). Same
    /// semantic derivation as [`Self::provider_registration`].
    pub fn org_provider_registration(
        spec: &InterestSpec,
        target: u64,
        requested_sample_interval: Duration,
        soft_state_ttl: Duration,
        subscriber_membership: OrgMembershipCert,
    ) -> Self {
        Self::OrgProviderRegistration {
            target,
            capability_id: spec.capability_id.clone(),
            constraints: spec.constraints.canonical_bytes(),
            constraints_digest: spec.constraints.constraints_digest(),
            work_latency: spec.work_latency,
            providers: spec.providers.clone(),
            result_mode: spec.result_mode,
            disclosure_class: spec.disclosure_class,
            audience_scope: spec.audience,
            interest_digest: spec.interest_digest(),
            requested_sample_interval,
            soft_state_ttl,
            subscriber_membership,
        }
    }

    /// Rebuild the COMPLETE [`InterestSpec`] a registration frame
    /// carries, given the already-validated parse of its inline
    /// constraint bytes. `None` for [`Self::Deregister`] (it carries
    /// no spec).
    ///
    /// The leader-addressed leg does not carry a disclosure class on
    /// the wire; v1 is owner-root-only (plan §4.10), so it
    /// reconstructs as [`DisclosureClass::Owner`] — exactly what
    /// every v1 sender digested.
    ///
    /// This is the single reconstruction BOTH legs share; callers
    /// almost always want [`Self::validated_spec`], which also
    /// validates the constraints and cross-checks the re-derived
    /// digest against the frame's claim.
    pub fn reconstruct_spec(&self, constraints: CanonicalConstraints) -> Option<InterestSpec> {
        match self {
            Self::CapabilityRegistration {
                capability_id,
                work_latency,
                providers,
                result_mode,
                audience_scope,
                ..
            } => Some(InterestSpec {
                capability_id: capability_id.clone(),
                constraints,
                work_latency: *work_latency,
                providers: providers.clone(),
                result_mode: *result_mode,
                disclosure_class: DisclosureClass::Owner,
                audience: *audience_scope,
            }),
            Self::ProviderRegistration {
                capability_id,
                work_latency,
                providers,
                result_mode,
                disclosure_class,
                audience_scope,
                ..
            }
            | Self::OrgProviderRegistration {
                capability_id,
                work_latency,
                providers,
                result_mode,
                disclosure_class,
                audience_scope,
                ..
            } => Some(InterestSpec {
                capability_id: capability_id.clone(),
                constraints,
                work_latency: *work_latency,
                providers: providers.clone(),
                result_mode: *result_mode,
                disclosure_class: *disclosure_class,
                audience: *audience_scope,
            }),
            // The org leader-addressed leg reconstructs like the legacy
            // leader leg (owner-root disclosure class); its membership is
            // validated at intake, not here.
            Self::OrgCapabilityRegistration {
                capability_id,
                work_latency,
                providers,
                result_mode,
                audience_scope,
                ..
            } => Some(InterestSpec {
                capability_id: capability_id.clone(),
                constraints,
                work_latency: *work_latency,
                providers: providers.clone(),
                result_mode: *result_mode,
                disclosure_class: DisclosureClass::Owner,
                audience: *audience_scope,
            }),
            Self::Deregister { .. } => None,
        }
    }

    /// The shared registration-intake pipeline (plan §4.2, review 7
    /// — used by BOTH legs: the leader's gate (r) intake and the
    /// provider's transcript invariant):
    ///
    /// 1. canonicalize + digest-validate the inline constraint bytes
    ///    ([`validate_interest_constraints`], which owns the
    ///    invalid-constraints/security counting);
    /// 2. reconstruct the COMPLETE [`InterestSpec`] from the carried
    ///    fields ([`Self::reconstruct_spec`]);
    /// 3. re-derive `interest_digest` and cross-check the frame's
    ///    claim — a mismatch is protocol-invalid input
    ///    ([`SensingCounters::protocol_invalid`]);
    /// 4. only then hand back the validated spec. The RE-DERIVED
    ///    identity — never the claim — is what coalesces at the
    ///    leader and what the provider signs.
    pub fn validated_spec(
        &self,
        counters: &SensingCounters,
    ) -> Result<InterestSpec, FrameSpecError> {
        let (constraint_bytes, constraints_digest, claimed_digest) = match self {
            Self::CapabilityRegistration {
                constraints,
                constraints_digest,
                interest_digest,
                ..
            }
            | Self::ProviderRegistration {
                constraints,
                constraints_digest,
                interest_digest,
                ..
            }
            | Self::OrgCapabilityRegistration {
                constraints,
                constraints_digest,
                interest_digest,
                ..
            }
            | Self::OrgProviderRegistration {
                constraints,
                constraints_digest,
                interest_digest,
                ..
            } => (constraints, constraints_digest, interest_digest),
            Self::Deregister { .. } => return Err(FrameSpecError::NotARegistration),
        };
        let constraints =
            validate_interest_constraints(constraint_bytes, constraints_digest, counters)
                .map_err(FrameSpecError::Constraints)?;
        // The variant was matched above, so a spec always exists.
        let spec = self
            .reconstruct_spec(constraints)
            .ok_or(FrameSpecError::NotARegistration)?;
        if spec.interest_digest() != *claimed_digest {
            counters.protocol_invalid.fetch_add(1, Ordering::Relaxed);
            return Err(FrameSpecError::InterestDigestMismatch);
        }
        Ok(spec)
    }

    /// Provider-side intake for the provider-addressed leg (the SI-1
    /// transcript invariant, review 7 sign-off): the provider must
    /// never evaluate — let alone sign — against an opaque,
    /// unvalidated interest-digest claim. Runs
    /// [`Self::validated_spec`] and hands back the validated spec
    /// together with the branch parameters the provider needs.
    ///
    /// Checking that `target` names this node, and that the frame
    /// arrived from an authenticated upstream, is the dispatch
    /// layer's job (SI-2) — exactly as the leader's consumer/origin
    /// cross-check lives at ITS intake.
    pub fn validate_provider_registration(
        &self,
        counters: &SensingCounters,
    ) -> Result<ValidatedProviderRegistration, FrameSpecError> {
        let Self::ProviderRegistration {
            target,
            requested_sample_interval,
            soft_state_ttl,
            ..
        } = self
        else {
            return Err(FrameSpecError::NotProviderAddressed);
        };
        let spec = self.validated_spec(counters)?;
        Ok(ValidatedProviderRegistration {
            target: *target,
            spec,
            requested_sample_interval: *requested_sample_interval,
            soft_state_ttl: *soft_state_ttl,
        })
    }
}

/// A provider-addressed registration that survived the full intake
/// pipeline ([`SensingInterestFrame::validate_provider_registration`]):
/// the spec's re-derived digest matches the frame's claim, so an
/// attestation signed against `spec.interest_digest()` commits to the
/// complete predicate + selector + mode + disclosure + audience
/// identity (plan §4.2, review 7).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ValidatedProviderRegistration {
    /// The provider the branch targets (this node, once SI-2 wires
    /// dispatch).
    pub target: u64,
    /// The validated, COMPLETE interest spec.
    pub spec: InterestSpec,
    /// Aggregated (strictest) D for the branch.
    pub requested_sample_interval: Duration,
    /// Soft-state lifetime of the branch registration.
    pub soft_state_ttl: Duration,
}

/// Why a registration frame's carried predicate failed intake
/// validation ([`SensingInterestFrame::validated_spec`]). Counter
/// discipline mirrors the leader's gate (r) intake: constraint
/// rejections are counted by [`validate_interest_constraints`]; an
/// interest-digest mismatch bumps
/// [`SensingCounters::protocol_invalid`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FrameSpecError {
    /// The frame is a `Deregister` — it carries no interest spec.
    NotARegistration,
    /// The frame is not the provider-addressed leg
    /// ([`SensingInterestFrame::validate_provider_registration`]
    /// only).
    NotProviderAddressed,
    /// The inline constraint bytes failed parse or digest validation
    /// (already counted).
    Constraints(ConstraintError),
    /// The re-derived interest digest does not match the frame's
    /// claim: the sender's bytes don't hash to the identity it
    /// asserted — protocol-invalid input (already counted). Nothing
    /// may coalesce under, or be signed against, the claimed digest.
    InterestDigestMismatch,
}

impl FrameSpecError {
    /// Whether this rejection incremented the protocol-invalid/
    /// security counter (forged or malformed protocol input, as
    /// opposed to an addressing or plain-decode problem).
    pub const fn is_security_relevant(self) -> bool {
        match self {
            Self::InterestDigestMismatch => true,
            Self::Constraints(error) => error.is_security_relevant(),
            Self::NotARegistration | Self::NotProviderAddressed => false,
        }
    }
}

impl fmt::Display for FrameSpecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotARegistration => f.write_str("deregister frames carry no interest spec"),
            Self::NotProviderAddressed => {
                f.write_str("frame is not a provider-addressed ProviderRegistration")
            }
            Self::Constraints(error) => write!(f, "constraint intake refused: {error}"),
            Self::InterestDigestMismatch => {
                f.write_str("re-derived interest digest does not match the frame's claim")
            }
        }
    }
}

impl std::error::Error for FrameSpecError {}

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

    // ---- Frozen-wire golden fixtures (OLB org-auth slice) --------------
    // The exact postcard encoding of each existing variant, captured BEFORE
    // the organization-authenticated variants are appended at indices 3/4.
    // Appending must not perturb these bytes (postcard encodes the variant
    // index; index 0/1/2 must stay 0/1/2). Regenerate ONLY with a deliberate,
    // reviewed wire change.
    const CAP_HEX: &str = "000e7072696e742e646f63756d656e74240200000005000000636f6c6f720400000074727565050000006d6564696102000000613420d02d423654096a867b66a506b433528db701e41818066eec51e186c3724be398010500000000204f9d6f145f2df01fa70c8155e7e9c55fe5571d6df47b631749ce35edc0b250fd0080c2d72f1e0020aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaacea328";
    const PROV_HEX: &str = "01770e7072696e742e646f63756d656e74240200000005000000636f6c6f720400000074727565050000006d6564696102000000613420d02d423654096a867b66a506b433528db701e41818066eec51e186c3724be3980105000000000020aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa204f9d6f145f2df01fa70c8155e7e9c55fe5571d6df47b631749ce35edc0b250fd0080c2d72f1e00";
    const DEREG_HEX: &str =
        "0220bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb019901";

    fn golden_dereg() -> SensingInterestFrame {
        SensingInterestFrame::Deregister {
            interest_digest: Digest256::from_bytes([0xBB; 32]),
            target: Some(0x99),
        }
    }

    #[test]
    fn existing_variants_have_frozen_postcard_encodings() {
        use crate::adapter::net::behavior::sensing::encode_interest_frame;
        let cap = SensingInterestFrame::capability_registration(
            &spec(),
            Duration::from_millis(100),
            Duration::from_secs(30),
            0xA11CE,
        );
        let prov = SensingInterestFrame::provider_registration(
            &spec(),
            0x77,
            Duration::from_millis(100),
            Duration::from_secs(30),
        );
        let dereg = golden_dereg();
        assert_eq!(hex::encode(encode_interest_frame(&cap).unwrap()), CAP_HEX);
        assert_eq!(hex::encode(encode_interest_frame(&prov).unwrap()), PROV_HEX);
        assert_eq!(
            hex::encode(encode_interest_frame(&dereg).unwrap()),
            DEREG_HEX
        );
        // Postcard variant indices: CapabilityRegistration=0, Provider=1,
        // Deregister=2 — the first byte is the variant discriminant.
        assert_eq!(encode_interest_frame(&cap).unwrap()[0], 0);
        assert_eq!(encode_interest_frame(&prov).unwrap()[0], 1);
        assert_eq!(encode_interest_frame(&dereg).unwrap()[0], 2);
    }

    // ---- Organization-authenticated variant composition (org-auth) -----
    fn cert() -> OrgMembershipCert {
        OrgMembershipCert::try_issue(
            &crate::adapter::net::behavior::org::OrgKeypair::from_bytes([0x42u8; 32]),
            crate::adapter::net::identity::EntityId::from_bytes([0x24u8; 32]),
            5,
            crate::adapter::net::behavior::org::ORG_CERT_TTL_SECS_RECOMMENDED,
        )
        .expect("issue cert")
    }

    fn org_cap_frame() -> SensingInterestFrame {
        SensingInterestFrame::org_capability_registration(
            &spec(),
            Duration::from_millis(100),
            Duration::from_secs(30),
            0xA11CE,
            cert(),
        )
    }

    fn org_prov_frame() -> SensingInterestFrame {
        SensingInterestFrame::org_provider_registration(
            &spec(),
            0x77,
            Duration::from_millis(100),
            Duration::from_secs(30),
            cert(),
        )
    }

    #[test]
    fn org_variants_land_at_postcard_indices_3_and_4() {
        use crate::adapter::net::behavior::sensing::encode_interest_frame;
        // Appended AFTER the frozen 0/1/2; the discriminant is the first byte.
        assert_eq!(encode_interest_frame(&org_cap_frame()).unwrap()[0], 3);
        assert_eq!(encode_interest_frame(&org_prov_frame()).unwrap()[0], 4);
    }

    #[test]
    fn org_frames_round_trip_and_preserve_the_embedded_cert() {
        use crate::adapter::net::behavior::sensing::{
            decode_interest_frame, encode_interest_frame,
        };
        for frame in [org_cap_frame(), org_prov_frame()] {
            let bytes = encode_interest_frame(&frame).unwrap();
            let back = decode_interest_frame(&bytes).expect("strict decode");
            // Full equality includes subscriber_membership — the embedded
            // 156-byte canonical cert survived the postcard round-trip.
            assert_eq!(back, frame);
        }
    }

    #[test]
    fn a_truncated_embedded_cert_fails_frame_decode() {
        use crate::adapter::net::behavior::sensing::{
            decode_interest_frame, encode_interest_frame,
        };
        let mut bytes = encode_interest_frame(&org_cap_frame()).unwrap();
        // Cut into the trailing certificate bytes: the cert's manual
        // Deserialize requires exactly WIRE_SIZE, so the frame fails to decode
        // rather than surviving as an unvalidated byte bag.
        bytes.truncate(bytes.len() - 10);
        assert!(decode_interest_frame(&bytes).is_err());
    }

    #[test]
    fn org_frame_trailing_bytes_fail_strict_decode() {
        use crate::adapter::net::behavior::sensing::{
            decode_interest_frame, encode_interest_frame,
        };
        let mut bytes = encode_interest_frame(&org_prov_frame()).unwrap();
        bytes.push(0x00);
        assert!(decode_interest_frame(&bytes).is_err());
    }

    #[test]
    fn validated_spec_reconstructs_org_variants() {
        let counters = SensingCounters::default();
        // Semantic reconstruction works (digest cross-check passes) — this is
        // NOT organization-authority validation, which the intake gate owns.
        assert_eq!(org_cap_frame().validated_spec(&counters).unwrap(), spec());
        assert_eq!(org_prov_frame().validated_spec(&counters).unwrap(), spec());
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
    fn provider_registration_round_trips_and_carries_population_fields() {
        let frame = SensingInterestFrame::provider_registration(
            &spec(),
            0x77,
            Duration::from_millis(100),
            Duration::from_secs(30),
        );
        let json = serde_json::to_value(&frame).unwrap();
        let body = &json["ProviderRegistration"];
        assert!(body.is_object());
        // §4.2 review-7 amendment: selector, mode, and disclosure
        // class ride the provider leg so the provider can verify the
        // COMPLETE digest it signs — never sign an opaque claim.
        assert!(body.get("providers").is_some());
        assert!(body.get("result_mode").is_some());
        assert!(body.get("disclosure_class").is_some());
        // §3.3: no variant carries a consumer budget — the field
        // name must not exist anywhere in the frame family.
        assert!(body.get("consumer_budget").is_none());
        let back: SensingInterestFrame = serde_json::from_value(json).unwrap();
        assert_eq!(back, frame);
    }

    #[test]
    fn validated_spec_reconstructs_the_complete_spec_on_both_legs() {
        let spec = spec();
        let counters = SensingCounters::default();
        let leader_leg = SensingInterestFrame::capability_registration(
            &spec,
            Duration::from_millis(100),
            Duration::from_secs(30),
            0xA,
        );
        let provider_leg = SensingInterestFrame::provider_registration(
            &spec,
            0x77,
            Duration::from_millis(100),
            Duration::from_secs(30),
        );
        for frame in [&leader_leg, &provider_leg] {
            let validated = frame.validated_spec(&counters).unwrap();
            assert_eq!(validated, spec);
            assert_eq!(validated.interest_digest(), spec.interest_digest());
        }
        assert_eq!(SensingCounters::get(&counters.invalid_constraints), 0);
        assert_eq!(SensingCounters::get(&counters.protocol_invalid), 0);
    }

    #[test]
    fn validate_provider_registration_returns_the_branch_parameters() {
        let spec = spec();
        let counters = SensingCounters::default();
        let frame = SensingInterestFrame::provider_registration(
            &spec,
            0x77,
            Duration::from_millis(100),
            Duration::from_secs(30),
        );
        let validated = frame.validate_provider_registration(&counters).unwrap();
        assert_eq!(validated.target, 0x77);
        assert_eq!(validated.spec, spec);
        assert_eq!(
            validated.requested_sample_interval,
            Duration::from_millis(100)
        );
        assert_eq!(validated.soft_state_ttl, Duration::from_secs(30));

        // The leader-addressed leg has no business at provider
        // intake.
        let leader_leg = SensingInterestFrame::capability_registration(
            &spec,
            Duration::from_millis(100),
            Duration::from_secs(30),
            0xA,
        );
        assert_eq!(
            leader_leg.validate_provider_registration(&counters),
            Err(FrameSpecError::NotProviderAddressed),
        );
        assert_eq!(SensingCounters::get(&counters.protocol_invalid), 0);
    }

    #[test]
    fn tampered_population_fields_fail_provider_digest_validation() {
        // The review-7 point of carrying selector/mode/class: a
        // tampered population field must be caught by the provider's
        // COMPLETE re-derivation, not silently signed under the old
        // digest claim.
        let base = || {
            SensingInterestFrame::provider_registration(
                &spec(),
                0x77,
                Duration::from_millis(100),
                Duration::from_secs(30),
            )
        };
        type FrameMutation = fn(&mut SensingInterestFrame);
        let mutations: [(&str, FrameMutation); 3] = [
            ("providers", |frame| {
                let SensingInterestFrame::ProviderRegistration { providers, .. } = frame else {
                    panic!("helper builds the provider leg");
                };
                *providers = ProviderSelector::Node(0x77);
            }),
            ("result_mode", |frame| {
                let SensingInterestFrame::ProviderRegistration { result_mode, .. } = frame else {
                    panic!("helper builds the provider leg");
                };
                *result_mode = ResultMode::Each;
            }),
            ("work_latency", |frame| {
                let SensingInterestFrame::ProviderRegistration { work_latency, .. } = frame else {
                    panic!("helper builds the provider leg");
                };
                *work_latency = WorkLatencyEnvelope::start_within(Duration::from_secs(6));
            }),
        ];
        for (field, mutate) in mutations {
            let counters = SensingCounters::default();
            let mut frame = base();
            mutate(&mut frame);
            let rejection = frame.validate_provider_registration(&counters).unwrap_err();
            assert_eq!(
                rejection,
                FrameSpecError::InterestDigestMismatch,
                "tampered {field} must fail digest re-derivation",
            );
            assert!(rejection.is_security_relevant());
            assert_eq!(SensingCounters::get(&counters.protocol_invalid), 1);
            // Constraint bytes were untouched — only the identity
            // cross-check fired.
            assert_eq!(SensingCounters::get(&counters.invalid_constraints), 0);
        }
    }

    #[test]
    fn corrupted_constraints_fail_intake_before_digest_re_derivation() {
        let counters = SensingCounters::default();
        let mut frame = SensingInterestFrame::provider_registration(
            &spec(),
            0x77,
            Duration::from_millis(100),
            Duration::from_secs(30),
        );
        let SensingInterestFrame::ProviderRegistration { constraints, .. } = &mut frame else {
            panic!("helper builds the provider leg");
        };
        constraints[0] ^= 1;
        let rejection = frame.validated_spec(&counters).unwrap_err();
        assert!(matches!(rejection, FrameSpecError::Constraints(_)));
        assert_eq!(SensingCounters::get(&counters.invalid_constraints), 1);
    }

    #[test]
    fn deregister_carries_no_spec() {
        let counters = SensingCounters::default();
        let frame = SensingInterestFrame::Deregister {
            interest_digest: spec().interest_digest(),
            target: None,
        };
        assert_eq!(
            frame.validated_spec(&counters),
            Err(FrameSpecError::NotARegistration),
        );
        assert_eq!(
            frame.validate_provider_registration(&counters),
            Err(FrameSpecError::NotProviderAddressed),
        );
        assert!(!FrameSpecError::NotARegistration.is_security_relevant());
        assert_eq!(SensingCounters::get(&counters.protocol_invalid), 0);
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
