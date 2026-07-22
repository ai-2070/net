//! Organization-authenticated sensing registration admission (OLB org-auth
//! slice, commit 2).
//!
//! The single authority transaction that turns an attacker-controlled
//! `OrgCapabilityRegistration` / `OrgProviderRegistration` frame into a narrow
//! [`ValidatedOrgSensingRegistration`]. Only that validated object may reach a
//! sensing-table mutation; after the gate succeeds the caller must NOT resume
//! reading security-relevant values from the original frame.
//!
//! # Locked validation order (each check before any table mutation)
//!
//! 1. frame is an org registration variant;
//! 2. semantic spec reconstruction + interest-digest cross-check
//!    ([`SensingInterestFrame::validated_spec`]) — the digest binds the
//!    audience, so a tampered audience fails here;
//! 3. authenticated hop/session `EntityId` == `cert.member` (and, for the
//!    leader leg, the existing routed-origin binding `consumer == from_node`);
//! 4. an installed [`NodeAuthority`] exists;
//! 5. `cert.org_id` == `authority.owner_org`;
//! 6. ONE explicit-time signature + window validation
//!    ([`OrgMembershipCert::is_valid_at_with_skew`] — it already calls
//!    `verify()`, so the signature is not re-checked separately);
//! 7. `cert.generation` >= the current revocation floor for
//!    `(cert.org_id, cert.member)`;
//! 8. the interest audience == the canonical organization sensing commitment
//!    for `cert.org_id`.
//!
//! Step 9 (the authority/store stability recheck immediately before mutation)
//! and step 10 (the mutation itself) are the dispatch layer's job — the gate
//! is validated against a pinned revocation snapshot the caller captured, and
//! the caller rechecks the security stamp between this gate returning and the
//! mutation. See the dispatch wiring (commit 2 part 2).
//!
//! Membership proves belonging only; it never enters `may_execute` and grants
//! no invocation authority. This gate authorizes *sensing registration*, an
//! advisory optimization — a refusal leaves the caller `Unknown`/`Potential`.

use super::super::org::{OrgError, OrgId};
use super::super::org_revocation::OrgRevocationState;
use super::frames::{FrameSpecError, SensingInterestFrame};
use super::identity::{AudienceScopeCommitment, InterestSpec};
use super::SensingCounters;
use crate::adapter::net::identity::EntityId;
use std::time::Duration;

/// The two values the gate needs from the installed
/// [`NodeAuthority`](super::super::org_authority::NodeAuthority): the owner
/// organization and the persisted verification skew. Extracted by the dispatch
/// layer so the gate stays decoupled from authority construction/storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OrgAuthorityView {
    /// The organization this node's authority belongs to (`owner_org()`).
    pub owner_org: OrgId,
    /// The persisted, ceiling-enforced verification clock skew (seconds).
    pub verification_skew_secs: u64,
}

/// Domain separation for the canonical organization sensing audience
/// commitment — a distinct BLAKE3 derive-key domain from the entity
/// `owner_root` commitment, so an organization audience colliding with a
/// single entity's root is cryptographically infeasible (domain-separated, not
/// literally injective).
const ORG_SENSING_AUDIENCE_DOMAIN: &str = "net.sensing.org-audience.v1";

/// The canonical sensing audience commitment for an organization: a
/// domain-separated BLAKE3 derivation over the `OrgId`. Every same-org member
/// derives the identical 32-byte commitment, and it is bound into the interest
/// digest, so two different organizations' interests coalescing is
/// cryptographically infeasible (domain-separated, not literal injectivity).
pub fn canonical_org_sensing_commitment(org_id: &OrgId) -> AudienceScopeCommitment {
    let mut hasher = blake3::Hasher::new_derive_key(ORG_SENSING_AUDIENCE_DOMAIN);
    hasher.update(org_id.as_bytes());
    AudienceScopeCommitment::from_bytes(*hasher.finalize().as_bytes())
}

/// The narrow, validated result of the org-sensing authority gate — the ONLY
/// value permitted to drive a sensing-table mutation for an org registration.
/// It carries the re-derived spec and the leg parameters, plus the verified
/// subscriber/organization identity for attribution; it never lends the caller
/// a reason to re-read the untrusted frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValidatedOrgSensingRegistration {
    /// A leader-addressed (provider-free) org registration.
    Capability {
        /// The re-derived, digest-validated interest spec.
        spec: InterestSpec,
        /// The registering consumer's node id (== the authenticated origin).
        consumer: u64,
        /// The delivery-continuity interval.
        requested_sample_interval: Duration,
        /// The per-downstream soft-state lifetime.
        soft_state_ttl: Duration,
        /// The verified subscriber entity (== `cert.member`).
        subscriber: EntityId,
        /// The verified organization.
        org_id: OrgId,
    },
    /// A provider-addressed org registration (a relay's re-authoring).
    Provider {
        /// The re-derived, digest-validated interest spec.
        spec: InterestSpec,
        /// The provider this branch targets.
        target: u64,
        /// The (strictest) delivery-continuity interval.
        requested_sample_interval: Duration,
        /// The per-downstream soft-state lifetime.
        soft_state_ttl: Duration,
        /// The verified subscriber entity (== `cert.member`).
        subscriber: EntityId,
        /// The verified organization.
        org_id: OrgId,
    },
}

/// Why an org-sensing registration was refused. Every variant is a hard
/// refusal — the caller mutates nothing and the observation stays
/// `Unknown`/`Potential`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OrgSensingRejection {
    /// The frame is not an organization registration variant.
    NotOrgRegistration,
    /// Semantic reconstruction / interest-digest cross-check failed.
    Semantic(FrameSpecError),
    /// The leader-leg routed-origin binding failed: `consumer != from_node`.
    ConsumerBindingMismatch,
    /// The authenticated sender is not the certificate's member.
    SenderMemberMismatch,
    /// No `NodeAuthority` is installed — this node cannot verify membership.
    MissingAuthority,
    /// The certificate's organization is not this node's owner organization.
    ForeignOrg,
    /// The certificate failed signature or time-window validation.
    CertInvalid(OrgError),
    /// The certificate generation is below the current revocation floor.
    BelowFloor,
    /// The interest audience is not the canonical commitment for the org.
    AudienceMismatch,
}

/// Steps 1–8 of the org-sensing authority gate against a PINNED revocation
/// snapshot and the installed node authority, at a single captured
/// `now_secs`. Returns the narrow validated object, or a typed refusal.
///
/// The caller (the dispatch layer) captures one wall-clock sample and pins the
/// authority + revocation view BEFORE calling this, and rechecks the security
/// stamp (step 9) immediately before the table mutation (step 10) — so a floor
/// raised or an authority swapped mid-validation cannot admit a stale
/// registration.
#[allow(clippy::too_many_arguments)]
pub fn verify_org_sensing_registration(
    frame: &SensingInterestFrame,
    from_node: u64,
    sender_entity: &EntityId,
    node_authority: Option<OrgAuthorityView>,
    revocation: &OrgRevocationState,
    now_secs: u64,
    counters: &SensingCounters,
) -> Result<ValidatedOrgSensingRegistration, OrgSensingRejection> {
    // Step 1: the frame must be an organization registration variant, and we
    // extract the leg parameters + the membership certificate exactly once.
    let (membership, leg) = match frame {
        SensingInterestFrame::OrgCapabilityRegistration {
            subscriber_membership,
            consumer,
            requested_sample_interval,
            soft_state_ttl,
            ..
        } => (
            subscriber_membership,
            Leg::Capability {
                consumer: *consumer,
                requested_sample_interval: *requested_sample_interval,
                soft_state_ttl: *soft_state_ttl,
            },
        ),
        SensingInterestFrame::OrgProviderRegistration {
            subscriber_membership,
            target,
            requested_sample_interval,
            soft_state_ttl,
            ..
        } => (
            subscriber_membership,
            Leg::Provider {
                target: *target,
                requested_sample_interval: *requested_sample_interval,
                soft_state_ttl: *soft_state_ttl,
            },
        ),
        _ => return Err(OrgSensingRejection::NotOrgRegistration),
    };

    // Step 2: semantic reconstruction + interest-digest cross-check. The
    // digest binds the audience, so a tampered audience is rejected HERE, not
    // as a plausible-but-wrong org membership.
    let spec = frame
        .validated_spec(counters)
        .map_err(OrgSensingRejection::Semantic)?;

    // Step 3: the authenticated hop is the certificate's member. The
    // certificate binds an EntityId; it does NOT replace the routed-origin
    // cross-check, which the leader leg still enforces (consumer == from_node).
    if *sender_entity != membership.member {
        return Err(OrgSensingRejection::SenderMemberMismatch);
    }
    if let Leg::Capability { consumer, .. } = &leg {
        if *consumer != from_node {
            return Err(OrgSensingRejection::ConsumerBindingMismatch);
        }
    }

    // Step 4: an installed authority is required to verify membership at all.
    let authority = node_authority.ok_or(OrgSensingRejection::MissingAuthority)?;

    // Step 5: the certificate's organization is this node's owner org.
    if membership.org_id != authority.owner_org {
        return Err(OrgSensingRejection::ForeignOrg);
    }

    // Step 6: ONE explicit-time signature + window validation. This calls
    // `verify()` internally, so the signature is validated exactly once.
    membership
        .is_valid_at_with_skew(now_secs, authority.verification_skew_secs)
        .map_err(OrgSensingRejection::CertInvalid)?;

    // Step 7: the certificate is not floored — its generation meets the
    // current revocation floor for (org, member).
    if membership.generation < revocation.floor_for(&membership.org_id, &membership.member) {
        return Err(OrgSensingRejection::BelowFloor);
    }

    // Step 8: the interest audience is the canonical organization sensing
    // commitment. `spec.audience` is digest-bound (step 2), so this pins the
    // registration to the organization scope authoritatively — a legacy sender
    // cannot self-declare org scope, and org scope cannot borrow a foreign
    // organization's commitment.
    if spec.audience != canonical_org_sensing_commitment(&membership.org_id) {
        return Err(OrgSensingRejection::AudienceMismatch);
    }

    // Steps 9 (stability recheck) and 10 (mutation) are the dispatch layer's.
    let subscriber = membership.member.clone();
    let org_id = membership.org_id;
    Ok(match leg {
        Leg::Capability {
            consumer,
            requested_sample_interval,
            soft_state_ttl,
        } => ValidatedOrgSensingRegistration::Capability {
            spec,
            consumer,
            requested_sample_interval,
            soft_state_ttl,
            subscriber,
            org_id,
        },
        Leg::Provider {
            target,
            requested_sample_interval,
            soft_state_ttl,
        } => ValidatedOrgSensingRegistration::Provider {
            spec,
            target,
            requested_sample_interval,
            soft_state_ttl,
            subscriber,
            org_id,
        },
    })
}

/// The leg-specific parameters extracted from the frame once (step 1).
enum Leg {
    Capability {
        consumer: u64,
        requested_sample_interval: Duration,
        soft_state_ttl: Duration,
    },
    Provider {
        target: u64,
        requested_sample_interval: Duration,
        soft_state_ttl: Duration,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::org::{
        OrgKeypair, OrgMembershipCert, ORG_CERT_TTL_SECS_RECOMMENDED,
    };
    use crate::adapter::net::behavior::sensing::identity::{
        CanonicalConstraints, CapabilityId, DisclosureClass, ProviderSelector, ResultMode,
        WorkLatencyEnvelope,
    };
    use std::collections::BTreeMap;

    const FROM_NODE: u64 = 0xA11CE;
    const D: Duration = Duration::from_millis(100);
    const TTL: Duration = Duration::from_secs(30);
    // A fixed "now" comfortably inside a freshly-issued cert's window.
    fn now_secs() -> u64 {
        crate::adapter::net::behavior::org::current_timestamp()
    }

    fn org_kp() -> OrgKeypair {
        OrgKeypair::from_bytes([0x42u8; 32])
    }

    fn member() -> EntityId {
        EntityId::from_bytes([0x24u8; 32])
    }

    fn cert_gen(generation: u32) -> OrgMembershipCert {
        OrgMembershipCert::try_issue(
            &org_kp(),
            member(),
            generation,
            ORG_CERT_TTL_SECS_RECOMMENDED,
        )
        .expect("issue cert")
    }

    fn authority() -> OrgAuthorityView {
        OrgAuthorityView {
            owner_org: org_kp().org_id(),
            verification_skew_secs: 60,
        }
    }

    fn spec_with(audience: AudienceScopeCommitment) -> InterestSpec {
        InterestSpec {
            capability_id: CapabilityId::new("gpu.infer"),
            constraints: CanonicalConstraints::from_entries([("model", "llama-70b")]).unwrap(),
            work_latency: WorkLatencyEnvelope::start_within(Duration::from_secs(2)),
            providers: ProviderSelector::Node(0x77),
            result_mode: ResultMode::Any,
            disclosure_class: DisclosureClass::Owner,
            audience,
        }
    }

    fn org_commit() -> AudienceScopeCommitment {
        canonical_org_sensing_commitment(&org_kp().org_id())
    }

    fn cap_frame_with(
        cert: OrgMembershipCert,
        audience: AudienceScopeCommitment,
    ) -> SensingInterestFrame {
        SensingInterestFrame::org_capability_registration(
            &spec_with(audience),
            D,
            TTL,
            FROM_NODE,
            cert,
        )
    }

    fn empty_floors() -> OrgRevocationState {
        OrgRevocationState::default()
    }

    fn floors_at(org: OrgId, member: EntityId, floor: u32) -> OrgRevocationState {
        let mut map = BTreeMap::new();
        map.insert((org, member), floor);
        OrgRevocationState::from_floors_for_test(map)
    }

    fn run(
        frame: &SensingInterestFrame,
        sender: &EntityId,
        authority: Option<OrgAuthorityView>,
        floors: &OrgRevocationState,
    ) -> Result<ValidatedOrgSensingRegistration, OrgSensingRejection> {
        verify_org_sensing_registration(
            frame,
            FROM_NODE,
            sender,
            authority,
            floors,
            now_secs(),
            &SensingCounters::default(),
        )
    }

    #[test]
    fn distinct_orgs_derive_distinct_commitments() {
        let a = canonical_org_sensing_commitment(&OrgId([1u8; 32]));
        let b = canonical_org_sensing_commitment(&OrgId([2u8; 32]));
        assert_ne!(a, b);
        // And it is not the raw org bytes (domain separated).
        assert_ne!(a.as_bytes(), &[1u8; 32]);
    }

    // ---- positive controls --------------------------------------------
    #[test]
    fn a_valid_org_capability_registration_is_admitted() {
        let frame = cap_frame_with(cert_gen(5), org_commit());
        let validated =
            run(&frame, &member(), Some(authority()), &empty_floors()).expect("admitted");
        match validated {
            ValidatedOrgSensingRegistration::Capability {
                consumer,
                subscriber,
                org_id,
                ..
            } => {
                assert_eq!(consumer, FROM_NODE);
                assert_eq!(subscriber, member());
                assert_eq!(org_id, org_kp().org_id());
            }
            other => panic!("expected Capability, got {other:?}"),
        }
    }

    #[test]
    fn a_valid_org_provider_registration_is_admitted() {
        let frame = SensingInterestFrame::org_provider_registration(
            &spec_with(org_commit()),
            0x77,
            D,
            TTL,
            cert_gen(5),
        );
        let validated =
            run(&frame, &member(), Some(authority()), &empty_floors()).expect("admitted");
        assert!(matches!(
            validated,
            ValidatedOrgSensingRegistration::Provider { target: 0x77, .. }
        ));
    }

    // ---- red matrix: each negative varies ONLY its named predicate -----
    #[test]
    fn foreign_organization_is_refused() {
        // A currently-valid cert with a valid signature and legal generation,
        // whose audience matches ITS OWN org commitment — only the org is
        // foreign relative to the installed authority.
        let foreign_kp = OrgKeypair::from_bytes([0x99u8; 32]);
        let foreign_cert =
            OrgMembershipCert::try_issue(&foreign_kp, member(), 5, ORG_CERT_TTL_SECS_RECOMMENDED)
                .expect("foreign cert");
        let foreign_audience = canonical_org_sensing_commitment(&foreign_kp.org_id());
        let frame = cap_frame_with(foreign_cert, foreign_audience);
        assert_eq!(
            run(&frame, &member(), Some(authority()), &empty_floors()),
            Err(OrgSensingRejection::ForeignOrg)
        );
    }

    #[test]
    fn sender_member_mismatch_is_refused() {
        let frame = cap_frame_with(cert_gen(5), org_commit());
        let other_sender = EntityId::from_bytes([0xEEu8; 32]);
        assert_eq!(
            run(&frame, &other_sender, Some(authority()), &empty_floors()),
            Err(OrgSensingRejection::SenderMemberMismatch)
        );
    }

    #[test]
    fn consumer_from_node_mismatch_is_refused() {
        // Same authenticated sender/member/org/audience — only the frame's
        // consumer disagrees with the routed origin.
        let frame = SensingInterestFrame::org_capability_registration(
            &spec_with(org_commit()),
            D,
            TTL,
            FROM_NODE ^ 0x1, // consumer != from_node
            cert_gen(5),
        );
        assert_eq!(
            run(&frame, &member(), Some(authority()), &empty_floors()),
            Err(OrgSensingRejection::ConsumerBindingMismatch)
        );
    }

    #[test]
    fn missing_authority_is_refused() {
        let frame = cap_frame_with(cert_gen(5), org_commit());
        assert_eq!(
            run(&frame, &member(), None, &empty_floors()),
            Err(OrgSensingRejection::MissingAuthority)
        );
    }

    #[test]
    fn generation_below_floor_is_refused() {
        // Everything valid; only the floor is raised above the cert generation.
        let frame = cap_frame_with(cert_gen(5), org_commit());
        let floors = floors_at(org_kp().org_id(), member(), 6);
        assert_eq!(
            run(&frame, &member(), Some(authority()), &floors),
            Err(OrgSensingRejection::BelowFloor)
        );
    }

    #[test]
    fn generation_at_floor_is_admitted() {
        let frame = cap_frame_with(cert_gen(6), org_commit());
        let floors = floors_at(org_kp().org_id(), member(), 6);
        assert!(run(&frame, &member(), Some(authority()), &floors).is_ok());
    }

    #[test]
    fn audience_org_mismatch_is_refused() {
        // Valid cert/sender/org; only the audience is not the org commitment.
        // The digest binds the audience, so validated_spec still passes (the
        // frame is internally consistent) and step 8 catches it.
        let frame = cap_frame_with(
            cert_gen(5),
            AudienceScopeCommitment::from_bytes([0x55u8; 32]),
        );
        assert_eq!(
            run(&frame, &member(), Some(authority()), &empty_floors()),
            Err(OrgSensingRejection::AudienceMismatch)
        );
    }

    #[test]
    fn a_forged_signature_is_refused() {
        // Everything valid except one flipped signature byte.
        let mut cert = cert_gen(5);
        cert.signature[0] ^= 0xFF;
        let frame = cap_frame_with(cert, org_commit());
        assert!(matches!(
            run(&frame, &member(), Some(authority()), &empty_floors()),
            Err(OrgSensingRejection::CertInvalid(_))
        ));
    }

    #[test]
    fn an_expired_certificate_is_refused() {
        // Valid signature/org/audience; only the window is in the past
        // (beyond the 60 s skew).
        let now = now_secs();
        let expired = OrgMembershipCert::issue_at(&org_kp(), member(), 5, now - 200, now - 100, 1);
        let frame = cap_frame_with(expired, org_commit());
        assert!(matches!(
            run(&frame, &member(), Some(authority()), &empty_floors()),
            Err(OrgSensingRejection::CertInvalid(_))
        ));
    }

    #[test]
    fn a_not_yet_valid_certificate_is_refused() {
        let now = now_secs();
        let future = OrgMembershipCert::issue_at(&org_kp(), member(), 5, now + 100, now + 200, 1);
        let frame = cap_frame_with(future, org_commit());
        assert!(matches!(
            run(&frame, &member(), Some(authority()), &empty_floors()),
            Err(OrgSensingRejection::CertInvalid(_))
        ));
    }
}
