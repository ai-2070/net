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
//! Between steps 2 and 3, on the provider-addressed leg only, the interest
//! selector must name EXACTLY the frame's own `target`
//! (`spec.providers == ProviderSelector::Node(target)`, design D2.6).
//!
//! # Two phases, and why the boundary is where it is
//!
//! Steps 1, 2 and that selector check need no authority, no revocation store
//! and no lock — they are pure functions of bytes the frame already carries.
//! They live in `validate_org_frame_shape`, which yields an
//! `OrgFrameShape`. Steps 3–8 need the installed authority and the pinned
//! revocation snapshot, and live in `verify_org_admission`, which can only
//! be called WITH an `OrgFrameShape`.
//!
//! [`verify_org_sensing_registration`] is the single-call composition of both
//! and is unchanged for callers that already hold an authority snapshot. The
//! dispatch layer instead calls the two halves in order, so a frame whose own
//! bytes are internally inconsistent is refused as protocol-invalid input
//! WITHOUT that node taking an `org_install` snapshot — which would otherwise
//! report the sender's malformation as a local `org_authority_unavailable`
//! problem on an unadopted or poisoned node. `OrgFrameShape` has private
//! fields and no public constructor, so that ordering is structural rather
//! than a convention a future edit could quietly invert.
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

use super::super::org::{OrgError, OrgId, OrgMembershipCert};
use super::super::org_authority::{NodeAuthority, OrgAuthorityError};
use super::super::org_revocation::{BarrieredGeneration, OrgRevocationState, OrgRevocationStore};
use super::frames::{FrameSpecError, SensingInterestFrame};
use super::identity::{AudienceScopeCommitment, InterestSpec, ProviderSelector};
use super::SensingCounters;
use crate::adapter::net::identity::EntityId;
use arc_swap::ArcSwapOption;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// The two values the gate needs from the installed
/// [`NodeAuthority`]: the owner
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

/// A zero-size witness that a [`ValidatedOrgSensingRegistration`] was minted by
/// [`verify_org_sensing_registration`] and nowhere else. Every variant of the
/// validated object carries one. The type is nameable but its field is PRIVATE
/// to this module, so a variant cannot be CONSTRUCTED without a `GateProof`,
/// which no code outside `org_gate` can produce. The validated object is
/// therefore impossible to fabricate, not merely documented as gate-produced
/// (review §7). Pattern matches (`from_validated_org`, tests) ignore it via `..`,
/// and the derived impls read it, so it is not dead.
#[derive(Debug, PartialEq, Eq)]
pub struct GateProof(());

/// The narrow, validated result of the org-sensing authority gate — the ONLY
/// value permitted to drive a sensing-table mutation for an org registration.
/// It carries the re-derived spec and the leg parameters, plus the verified
/// subscriber/organization identity for attribution; it never lends the caller
/// a reason to re-read the untrusted frame. SEALED: each variant carries a
/// private [`GateProof`], so only [`verify_org_sensing_registration`] (or, under
/// `#[cfg(test)]`, [`Self::capability_for_test`]) can construct it — a future
/// leader intake cannot mint an org-authority row by literal-constructing this.
#[derive(Debug, PartialEq, Eq)]
enum ValidatedInner {
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
        /// Gate-only construction proof (review §7).
        gate_proof: GateProof,
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
        /// Gate-only construction proof (review §7).
        gate_proof: GateProof,
    },
}

/// The SEALED validated object (2026-07-23 §7 residual).
///
/// A newtype whose field is PRIVATE to this module, wrapping the variant data.
/// The `GateProof` inside made a variant impossible to CONSTRUCT from nothing;
/// it did not stop code that legitimately holds a validated object from
/// DESTRUCTURING it, keeping the proof, and re-assembling a variant around a
/// different spec or leg — and `GateProof: Clone` made that free and repeatable
/// rather than costing the original.
///
/// With the payload behind a private field, neither is expressible outside
/// `org_gate`: a holder can move the value and hand it to
/// `AdmittedSensingRegistration::from_validated_org`, and nothing else. The
/// proof and the non-`Clone` bound remain as defence in depth for code inside
/// this module.
#[derive(Debug, PartialEq, Eq)]
pub struct ValidatedOrgSensingRegistration(ValidatedInner);

impl Clone for ValidatedOrgSensingRegistration {
    /// Hand-written because `GateProof` is deliberately NOT `Clone`: only this
    /// module may mint one, so only this module may duplicate a validated
    /// object. A derived impl would have required making the proof cloneable,
    /// which is exactly the residual being closed.
    fn clone(&self) -> Self {
        Self(match &self.0 {
            ValidatedInner::Capability {
                spec,
                consumer,
                requested_sample_interval,
                soft_state_ttl,
                subscriber,
                org_id,
                ..
            } => ValidatedInner::Capability {
                spec: spec.clone(),
                consumer: *consumer,
                requested_sample_interval: *requested_sample_interval,
                soft_state_ttl: *soft_state_ttl,
                subscriber: subscriber.clone(),
                org_id: *org_id,
                gate_proof: GateProof(()),
            },
            ValidatedInner::Provider {
                spec,
                target,
                requested_sample_interval,
                soft_state_ttl,
                subscriber,
                org_id,
                ..
            } => ValidatedInner::Provider {
                spec: spec.clone(),
                target: *target,
                requested_sample_interval: *requested_sample_interval,
                soft_state_ttl: *soft_state_ttl,
                subscriber: subscriber.clone(),
                org_id: *org_id,
                gate_proof: GateProof(()),
            },
        })
    }
}

#[cfg(test)]
impl ValidatedOrgSensingRegistration {
    /// Test-only sanctioned constructor for a Capability-leg validated object —
    /// the sole way in-crate tests may fabricate one without running the full
    /// gate. Not available in production builds, so the seal holds there.
    pub(crate) fn capability_for_test(
        spec: InterestSpec,
        consumer: u64,
        requested_sample_interval: Duration,
        soft_state_ttl: Duration,
        subscriber: EntityId,
        org_id: OrgId,
    ) -> Self {
        Self(ValidatedInner::Capability {
            spec,
            consumer,
            requested_sample_interval,
            soft_state_ttl,
            subscriber,
            org_id,
            gate_proof: GateProof(()),
        })
    }
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
    /// The provider-addressed leg's interest selector does not name EXACTLY
    /// the frame's `target`: `spec.providers != ProviderSelector::Node(target)`
    /// (design D2.6). An exact-provider interest is one interest per provider,
    /// so `AnyAuthorized`, a `Group`, a `Tags` match, and even a one-element
    /// `Nodes([target])` are all incoherent here — `Nodes` is canonically
    /// sorted and deduplicated, so accepting it would split one semantic
    /// interest across two digests.
    ///
    /// Appended after the eight original variants. `OrgSensingRejection` is
    /// public and not `#[non_exhaustive]`, so this is a deliberate semver
    /// break for any downstream exhaustive matcher; in-tree the cost is the
    /// single wildcard-free match in this file.
    SelectorTargetMismatch,
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
///
/// Every refusal bumps a per-reason [`SensingCounters`] tally (review §4) so a
/// forged-cert flood or revocation-evasion attempt is operator-visible — the
/// `Semantic` arm is already counted by [`SensingInterestFrame::validated_spec`],
/// so it is not double-counted here.
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
    let result = validate_org_frame_shape(frame, counters).and_then(|shape| {
        verify_org_admission_with_shape(
            &shape,
            from_node,
            sender_entity,
            node_authority,
            revocation,
            now_secs,
        )
    });
    if let Err(rejection) = &result {
        count_org_rejection(rejection, counters);
    }
    result
}

/// The ONE counter mapping for an org-gate refusal. Both the full verifier and
/// the dispatch-layer shape pre-gate route through this, so a refusal can never
/// be counted in two different classes depending on which entry point saw it.
pub(crate) fn count_org_rejection(rejection: &OrgSensingRejection, counters: &SensingCounters) {
    // One counter per reason; `Semantic` already counted upstream by
    // `validated_spec`.
    let counter = match rejection {
        OrgSensingRejection::CertInvalid(_) => Some(&counters.org_cert_invalid),
        OrgSensingRejection::BelowFloor => Some(&counters.org_below_floor),
        OrgSensingRejection::ForeignOrg => Some(&counters.org_foreign_org),
        OrgSensingRejection::SenderMemberMismatch => Some(&counters.org_sender_member_mismatch),
        OrgSensingRejection::AudienceMismatch => Some(&counters.org_audience_mismatch),
        OrgSensingRejection::MissingAuthority => Some(&counters.org_authority_unavailable),
        // Routed-origin / frame-shape violations are protocol-invalid
        // input. A selector that does not name the frame's own target is
        // the same class: the sender's own bytes are internally
        // inconsistent, so it is a protocol violation, not merely an
        // authorization refusal (design D2.6).
        OrgSensingRejection::ConsumerBindingMismatch
        | OrgSensingRejection::NotOrgRegistration
        | OrgSensingRejection::SelectorTargetMismatch => Some(&counters.protocol_invalid),
        OrgSensingRejection::Semantic(_) => None,
    };
    if let Some(counter) = counter {
        counter.fetch_add(1, Ordering::Relaxed);
    }
}

/// The AUTHORITY phase (steps 3–8) with counter dispatch, for a caller that has
/// already validated the frame shape. This is the dispatch layer's entry point:
/// it pairs with [`validate_org_frame_shape`] to give the same total validation
/// as [`verify_org_sensing_registration`], in two explicitly ordered halves, so
/// the authority-free refusals can be taken before any `org_install` work.
pub(crate) fn verify_org_admission(
    shape: &OrgFrameShape<'_>,
    from_node: u64,
    sender_entity: &EntityId,
    node_authority: Option<OrgAuthorityView>,
    revocation: &OrgRevocationState,
    now_secs: u64,
    counters: &SensingCounters,
) -> Result<ValidatedOrgSensingRegistration, OrgSensingRejection> {
    let result = verify_org_admission_with_shape(
        shape,
        from_node,
        sender_entity,
        node_authority,
        revocation,
        now_secs,
    );
    if let Err(rejection) = &result {
        count_org_rejection(rejection, counters);
    }
    result
}

/// The AUTHORITY-FREE prefix of the org gate, and an unforgeable proof that it
/// ran: frame-kind discrimination (step 1), semantic spec reconstruction +
/// interest-digest cross-check (step 2), and the selector ↔ target exactness
/// check (step 2a).
///
/// None of those three needs an installed authority, a revocation store, or any
/// lock — they are pure functions of bytes the frame already carries. Holding
/// them in their own phase is what lets the dispatch layer reject a malformed
/// frame BEFORE it pays for an `org_install` snapshot, and what makes that
/// ordering structural rather than a convention: [`OrgFrameShape`] has private
/// fields and no public constructor, so the authority phase literally cannot be
/// entered without one.
///
/// The fields are the products the authority phase needs, so nothing is
/// recomputed and the two phases cannot drift apart.
pub(crate) struct OrgFrameShape<'a> {
    spec: InterestSpec,
    leg: Leg,
    membership: &'a OrgMembershipCert,
}

impl OrgFrameShape<'_> {
    /// The provider-addressed target, when this is the provider leg.
    #[cfg(test)]
    fn provider_target(&self) -> Option<u64> {
        match self.leg {
            Leg::Provider { target, .. } => Some(target),
            Leg::Capability { .. } => None,
        }
    }
}

/// Steps 1, 2 and 2a. Authority-free, lock-free, and the mandatory entry point
/// to the gate — see [`OrgFrameShape`].
pub(crate) fn validate_org_frame_shape<'a>(
    frame: &'a SensingInterestFrame,
    counters: &SensingCounters,
) -> Result<OrgFrameShape<'a>, OrgSensingRejection> {
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

    // Step 2a (design D2.6): on the provider-addressed leg the interest
    // selector must name EXACTLY the frame's own `target`. This runs
    // immediately after semantic reconstruction — before the membership,
    // authority, signature, floor and audience work below, and therefore
    // before ANY table mutation, evaluator feed, relay planning, cache
    // publication or onward byte. It is a pure comparison of two values the
    // frame already carries, so it needs no authority and no lock.
    //
    // `Node(target)` is the only coherent shape: an exact-provider interest is
    // one interest per provider, and `ProviderSelector` is part of the interest
    // digest, so a broader selector would either corrupt the merge-miss
    // denominator (`AnyAuthorized` makes `is_provider_free()` true) or split one
    // semantic interest across two digests (`Nodes` is canonically sorted and
    // deduplicated). The capability leg carries a `consumer`, not a `target`,
    // and is checked by the routed-origin binding in step 3 instead.
    if let Leg::Provider { target, .. } = &leg {
        if !selector_names_target(&spec, *target) {
            return Err(OrgSensingRejection::SelectorTargetMismatch);
        }
    }

    Ok(OrgFrameShape {
        spec,
        leg,
        membership,
    })
}

/// Steps 3–8: everything that needs the installed authority and the pinned
/// revocation snapshot. Reachable only with an [`OrgFrameShape`], so steps 1, 2
/// and 2a have provably already passed.
fn verify_org_admission_with_shape(
    shape: &OrgFrameShape<'_>,
    from_node: u64,
    sender_entity: &EntityId,
    node_authority: Option<OrgAuthorityView>,
    revocation: &OrgRevocationState,
    now_secs: u64,
) -> Result<ValidatedOrgSensingRegistration, OrgSensingRejection> {
    let OrgFrameShape {
        spec,
        leg,
        membership,
    } = shape;

    // Step 3: the authenticated hop is the certificate's member. The
    // certificate binds an EntityId; it does NOT replace the routed-origin
    // cross-check, which the leader leg still enforces (consumer == from_node).
    if *sender_entity != membership.member {
        return Err(OrgSensingRejection::SenderMemberMismatch);
    }
    if let Leg::Capability { consumer, .. } = leg {
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
    let spec = spec.clone();
    Ok(ValidatedOrgSensingRegistration(match *leg {
        Leg::Capability {
            consumer,
            requested_sample_interval,
            soft_state_ttl,
        } => ValidatedInner::Capability {
            spec,
            consumer,
            requested_sample_interval,
            soft_state_ttl,
            subscriber,
            org_id,
            gate_proof: GateProof(()),
        },
        Leg::Provider {
            target,
            requested_sample_interval,
            soft_state_ttl,
        } => ValidatedInner::Provider {
            spec,
            target,
            requested_sample_interval,
            soft_state_ttl,
            subscriber,
            org_id,
            gate_proof: GateProof(()),
        },
    }))
}

/// The ONE selector <-> target exactness rule (design D2.6), shared by the
/// inbound intake gate and the local-origin egress planner so the two can never
/// disagree about what "exact provider" means.
///
/// `Node(target)` is the only coherent shape: an exact-provider interest is one
/// interest per provider, and `ProviderSelector` is part of the interest digest,
/// so a broader selector would either corrupt the merge-miss denominator
/// (`AnyAuthorized` makes `is_provider_free()` true) or split one semantic
/// interest across two digests (`Nodes` is canonically sorted and deduplicated).
pub(crate) fn selector_names_target(spec: &InterestSpec, target: u64) -> bool {
    spec.providers == ProviderSelector::Node(target)
}

/// Why a LOCAL-ORIGIN organization egress was refused. Every variant is a hard
/// refusal: nothing is minted, installed or emitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LocalOrgEgressRefusal {
    /// The spec's audience is not the canonical sensing commitment of the
    /// organization the captured membership proves. The audience is DERIVED
    /// from installed authority; a caller cannot name one.
    AudienceNotDerived,
    /// The interest selector does not name the lease's own target
    /// (design D2.6) — the same rule the inbound gate enforces.
    SelectorTargetMismatch,
}

/// A validated local-origin organization egress plan, and an unforgeable proof
/// that a LIVE local membership was captured for it.
///
/// Constructible only from a [`LiveOrgRelayMembership`], which is itself
/// constructible only inside this module by
/// [`capture_live_org_relay_membership`] — so a caller cannot inject an
/// audience commitment, an owner root, a certificate, or an organization id,
/// and cannot fabricate this token to reach the egress planner.
///
/// The certificate is cloned OUT of the live token deliberately: the token is
/// deliberately not `Clone`, and the frame needs an owned certificate. Cloning
/// the cert copies signed bytes only — it confers no authority and does not
/// extend the token's lifetime.
pub(crate) struct LocalOrgEgress {
    proven_root: AudienceScopeCommitment,
    membership: OrgMembershipCert,
}

impl LocalOrgEgress {
    /// Derive the egress plan from a live local membership capture. The proven
    /// root is `canonical_org_sensing_commitment(org)` — the SAME derivation
    /// the admitted-wrapper uses, never a caller value and never
    /// `validate_subscriber_scope`'s legacy entity root.
    pub(crate) fn from_live_membership(membership: &LiveOrgRelayMembership) -> Self {
        Self {
            proven_root: canonical_org_sensing_commitment(&membership.org_id()),
            membership: membership.owner_cert().clone(),
        }
    }

    /// The organization-derived root the interest table row registers under.
    pub(crate) fn proven_root(&self) -> AudienceScopeCommitment {
        self.proven_root
    }
}

/// Plan the LOCAL-ORIGIN organization provider registration frame.
///
/// There is deliberately **no `Legacy` arm**: this function is reachable only
/// with a [`LocalOrgEgress`], which only a live organization membership capture
/// can produce, so a legacy authority cannot reach it and it can emit nothing
/// but [`SensingInterestFrame::OrgProviderRegistration`]. That is the structural
/// counterpart to `plan_provider_continuation`'s exhaustive authority match on
/// the inbound relay path — and unlike that planner, this one has no legacy
/// frame to fall back to even in principle.
///
/// Both refusals are checked BEFORE the frame is built, so a refused plan emits
/// nothing.
pub(crate) fn plan_local_org_provider_registration(
    egress: &LocalOrgEgress,
    spec: &InterestSpec,
    target: u64,
    requested_sample_interval: Duration,
    soft_state_ttl: Duration,
) -> Result<SensingInterestFrame, LocalOrgEgressRefusal> {
    // The audience must be the one DERIVED from the captured membership's
    // organization. This is the local mirror of intake step 8, and it is what
    // makes "no caller-supplied audience" checkable rather than merely intended.
    if spec.audience != egress.proven_root {
        return Err(LocalOrgEgressRefusal::AudienceNotDerived);
    }
    if !selector_names_target(spec, target) {
        return Err(LocalOrgEgressRefusal::SelectorTargetMismatch);
    }
    Ok(SensingInterestFrame::org_provider_registration(
        spec,
        target,
        requested_sample_interval,
        soft_state_ttl,
        egress.membership.clone(),
    ))
}

/// The leg-specific parameters extracted from the frame once (step 1). `Copy`
/// because [`OrgFrameShape`] hands it to the authority phase by reference.
#[derive(Clone, Copy)]
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

/// An admitted sensing registration that CARRIES its authority evidence, so
/// invalid combinations are unrepresentable: an org registration can never
/// carry an independently-supplied entity root, and the legacy arm can never
/// carry a relay/organization certificate. This is the only value the shared
/// semantic operations act on after admission — the original frame is never
/// read again, and the upstream continuation frame kind is chosen exhaustively
/// from [`Self::authority`] (no org → legacy fallback).
///
/// Consumed by the signed-off exact-provider path: [`Self::from_validated_org`],
/// [`Self::leg`], [`Self::proven_root`], [`Self::authority`], and the provider
/// continuation ([`plan_provider_continuation`]) all drive the live
/// `OrgProviderRegistration` re-authoring seam. The residual
/// `#[allow(dead_code)]` now covers only the capability-leg surface the
/// still-dark organization LEADER path will consume when it lights up.
///
/// **Immutable after construction.** All fields are private and exposed only
/// through by-value/by-ref accessors: the complete spec AND the addressing leg
/// were part of the admission decision, so no crate code may mutate the spec
/// audience, retarget the leg, or otherwise desync the payload from the
/// verified `authority`. There is deliberately no `*_mut`, no `into_parts`, and
/// no field is `pub`/`pub(crate)`.
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AdmittedSensingRegistration {
    spec: InterestSpec,
    leg: RegistrationLeg,
    authority: RegistrationAuthority,
}

/// The authority under which a sensing registration was admitted.
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RegistrationAuthority {
    /// Legacy entity/fleet-root admission — the session-proven root the legacy
    /// dispatch computed.
    Legacy {
        /// The session-proven owner root.
        proven_root: AudienceScopeCommitment,
    },
    /// Organization-authenticated admission — the verified organization. The
    /// proven root is DERIVED from this id, never supplied.
    Org {
        /// The verified organization id.
        org_id: OrgId,
    },
}

/// The leg-specific parameters of an admitted registration.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RegistrationLeg {
    /// A leader-addressed (provider-free) registration.
    Capability {
        /// The registering consumer's node id (the authenticated origin).
        consumer: u64,
        /// The delivery-continuity interval.
        requested_sample_interval: Duration,
        /// The per-downstream soft-state lifetime.
        soft_state_ttl: Duration,
    },
    /// A provider-addressed registration.
    Provider {
        /// The provider this branch targets.
        target: u64,
        /// The (strictest) delivery-continuity interval.
        requested_sample_interval: Duration,
        /// The per-downstream soft-state lifetime.
        soft_state_ttl: Duration,
    },
}

#[allow(dead_code)]
impl AdmittedSensingRegistration {
    /// Admit a legacy registration (the legacy dispatch validated + converted
    /// it once).
    ///
    /// The proven root is DERIVED from the spec, not accepted beside it
    /// (2026-07-23 §7 residual). The old signature took an arbitrary
    /// `(spec, proven_root)` pair, so nothing in the type stopped a caller from
    /// admitting one interest's spec under a different interest's proven root —
    /// the pairing was correct only because every caller happened to be passing
    /// the value `validate_subscriber_scope` had just returned FOR THAT SPEC.
    ///
    /// That correctness is now structural, and it costs nothing, because
    /// `validate_subscriber_scope` only ever returns `Ok` when
    /// `interest_audience == session_root == local_root`. Its `Ok` value IS
    /// `spec.audience`; taking it as a parameter was re-stating a value the spec
    /// already carried. This mirrors `from_validated_org`, which has always
    /// derived its root rather than borrowing one.
    ///
    /// The caller must still have validated the scope — that is what makes the
    /// audience PROVEN rather than merely claimed. What can no longer happen is
    /// the two halves disagreeing.
    pub(crate) fn from_validated_legacy(spec: InterestSpec, leg: RegistrationLeg) -> Self {
        let proven_root = spec.audience;
        Self {
            spec,
            leg,
            authority: RegistrationAuthority::Legacy { proven_root },
        }
    }

    /// Admit an organization registration from the authority gate's output.
    /// The proven root is NOT accepted here — it is derived from the verified
    /// organization id, so an org registration can never borrow a foreign or
    /// entity root.
    pub(crate) fn from_validated_org(value: ValidatedOrgSensingRegistration) -> Self {
        match value.0 {
            ValidatedInner::Capability {
                spec,
                consumer,
                requested_sample_interval,
                soft_state_ttl,
                org_id,
                ..
            } => Self {
                spec,
                leg: RegistrationLeg::Capability {
                    consumer,
                    requested_sample_interval,
                    soft_state_ttl,
                },
                authority: RegistrationAuthority::Org { org_id },
            },
            ValidatedInner::Provider {
                spec,
                target,
                requested_sample_interval,
                soft_state_ttl,
                org_id,
                ..
            } => Self {
                spec,
                leg: RegistrationLeg::Provider {
                    target,
                    requested_sample_interval,
                    soft_state_ttl,
                },
                authority: RegistrationAuthority::Org { org_id },
            },
        }
    }

    /// The re-derived, digest-validated interest spec (immutable).
    pub(crate) fn spec(&self) -> &InterestSpec {
        &self.spec
    }

    /// The leg-specific parameters (immutable; `Copy`, so a caller mutates only
    /// its own copy).
    pub(crate) fn leg(&self) -> RegistrationLeg {
        self.leg
    }

    /// The audience commitment this registration was proven under. For the org
    /// arm it is DERIVED from the verified organization id — never a supplied
    /// value — so the semantic operation cannot be handed a mismatched root.
    pub(crate) fn proven_root(&self) -> AudienceScopeCommitment {
        match &self.authority {
            RegistrationAuthority::Legacy { proven_root } => *proven_root,
            RegistrationAuthority::Org { org_id } => canonical_org_sensing_commitment(org_id),
        }
    }

    /// The admitting authority — the dispatch matches this EXHAUSTIVELY to pick
    /// the upstream continuation frame kind (org → a fresh org frame with the
    /// relay's own membership; legacy → a legacy frame). There is no fallback.
    pub(crate) fn authority(&self) -> &RegistrationAuthority {
        &self.authority
    }

    /// Derive a PROVIDER-leg continuation of this admitted registration for the
    /// upstream `target`, preserving the validated spec AND the admitted
    /// authority provenance unchanged. This is the ONLY sanctioned way to obtain
    /// a provider-leg wrapper from a cached capability seed: it re-targets the
    /// leg but can never desync the spec/authority pairing or fabricate an
    /// authority the seed was not admitted under — there is deliberately no
    /// `new(spec, leg, authority)` that would let a caller assemble an arbitrary
    /// combination. The leader caches the capability seed a consumer's admission
    /// produced and derives one of these per resolved provider branch, so a
    /// reconciliation-added or refusal-survivor re-registration authored after
    /// the original wrapper has left scope still carries the org-vs-legacy mode
    /// from admitted evidence, never re-inferred from `spec.audience`.
    pub(crate) fn provider_continuation(
        &self,
        target: u64,
        requested_sample_interval: Duration,
        soft_state_ttl: Duration,
    ) -> Self {
        Self {
            spec: self.spec.clone(),
            leg: RegistrationLeg::Provider {
                target,
                requested_sample_interval,
                soft_state_ttl,
            },
            authority: self.authority.clone(),
        }
    }
}

/// A monotonic, allocator-reuse-safe stamp of the node's sensing authority
/// security view, captured under the `org_install` publication lock. Equality
/// is the admission linearization currency check: the store publish generation
/// catches a floor raise on the same store, the pointers catch identity swaps,
/// and the **installation generation** catches an `A → B → exact-Arc-A`
/// rotation the pointer alone cannot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SensingAuthorityStamp {
    authority_ptr: usize,
    store_ptr: usize,
    /// `None` once the publication generation space is EXHAUSTED — a frozen
    /// counter can no longer witness that a view is still live, so it never
    /// compares current (Kyra OLB-2B-E3c).
    store_generation: Option<BarrieredGeneration>,
    installation_generation: u64,
    poisoned: bool,
}

impl SensingAuthorityStamp {
    /// `true` iff `self` (captured before validation) still equals the live
    /// `current` stamp AND the store is not now poisoned — i.e. the security
    /// view the registration was validated against is still live. A poison
    /// transition makes the stamp non-current even with every numeric field
    /// equal.
    pub(crate) fn is_current(&self, current: &SensingAuthorityStamp) -> bool {
        // An exhausted generation is unusable authority on EITHER side, exactly
        // as in `AdmissionStamp::is_current`: two `None` stamps would otherwise
        // compare equal-and-current, and a frozen counter can witness neither
        // that the captured view is still live nor that the live view moved.
        //
        // Unreachable through the current caller —
        // `capture_sensing_authority_snapshot` refuses with
        // `SensingAuthorityUnavailable::GenerationExhausted` before it builds a
        // stamp, so the captured side is always `Some`. That invariant lives in
        // a DIFFERENT function from this check, so it is made structural here
        // rather than left as a property a future capture path could break
        // silently (review-pass-2 §4).
        // The INSTALLATION generation is fenced on the same principle
        // (review-pass-3 §12). It is advanced `checked`-ly and LATCHES at
        // `u64::MAX` rather than wrapping, so the ceiling is a terminal sentinel:
        // past it every installation would receive the same identity and an
        // `A -> B -> A` rotation — the exact case this field exists to catch —
        // would compare unchanged. Refused on both sides for the same reason the
        // store generation is.
        self.store_generation.is_some()
            && current.store_generation.is_some()
            && self.installation_generation != u64::MAX
            && current.installation_generation != u64::MAX
            && self == current
            && !current.poisoned
    }
}

/// A pinned snapshot of the sensing authority view for one org-registration
/// admission. The retained `Arc`s prevent allocator-address reuse between
/// validation and the pre-mutation recheck; the installation generation
/// separately closes exact-`Arc` `A → B → A`.
pub(crate) struct SensingAuthoritySnapshot {
    stamp: SensingAuthorityStamp,
    authority_view: OrgAuthorityView,
    floors: Arc<OrgRevocationState>,
    // Pins — held only to keep the exact objects alive across validation.
    _authority: Arc<NodeAuthority>,
    _store: Arc<OrgRevocationStore>,
}

#[allow(dead_code)]
impl SensingAuthoritySnapshot {
    /// The stamp to recheck against live state before mutation.
    pub(crate) fn stamp(&self) -> &SensingAuthorityStamp {
        &self.stamp
    }

    /// The pinned authority view (owner org + skew) the gate validates against.
    pub(crate) fn authority_view(&self) -> OrgAuthorityView {
        self.authority_view
    }

    /// The pinned, coherent revocation floors the gate validates against.
    pub(crate) fn floors(&self) -> &OrgRevocationState {
        &self.floors
    }
}

/// Why the sensing authority view could not be captured — each leaves the
/// registration `Unknown`/`Potential` (advisory), never a legacy downgrade.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SensingAuthorityUnavailable {
    /// No `NodeAuthority` is installed.
    NoAuthority,
    /// No `OrgRevocationStore` is installed.
    NoStore,
    /// The installed store is poisoned.
    Poisoned,
    /// The store's publication generation space is exhausted, so its currentness
    /// can no longer be witnessed — unusable authority, exactly like poison.
    GenerationExhausted,
}

/// Capture a coherent, pinned snapshot of the sensing authority view under the
/// `org_install` publication lock (so no reader observes a published store
/// before its matching installation generation). Free-standing over the raw
/// node fields so both `MeshNode` and the dispatch context can call it.
pub(crate) fn capture_sensing_authority_snapshot(
    org_install: &Mutex<()>,
    node_authority: &ArcSwapOption<NodeAuthority>,
    org_revocation: &ArcSwapOption<OrgRevocationStore>,
    org_install_generation: &AtomicU64,
) -> Result<SensingAuthoritySnapshot, SensingAuthorityUnavailable> {
    let _install = org_install.lock();
    let authority = node_authority
        .load_full()
        .ok_or(SensingAuthorityUnavailable::NoAuthority)?;
    let store = org_revocation
        .load_full()
        .ok_or(SensingAuthorityUnavailable::NoStore)?;
    if store.is_poisoned() {
        return Err(SensingAuthorityUnavailable::Poisoned);
    }
    // Coherent floors + barriered store generation as one pair. An exhausted
    // generation is unusable authority, exactly like poison.
    let (floors, store_generation) = store
        .snapshot_with_generation()
        .map_err(|_| SensingAuthorityUnavailable::GenerationExhausted)?;
    let store_generation = Some(store_generation);
    let stamp = SensingAuthorityStamp {
        authority_ptr: Arc::as_ptr(&authority) as *const () as usize,
        store_ptr: Arc::as_ptr(&store) as *const () as usize,
        store_generation,
        installation_generation: org_install_generation.load(Ordering::Acquire),
        poisoned: false,
    };
    let authority_view = OrgAuthorityView {
        owner_org: authority.owner_org(),
        verification_skew_secs: authority.config.verification_skew_secs,
    };
    Ok(SensingAuthoritySnapshot {
        stamp,
        authority_view,
        floors,
        _authority: authority,
        _store: store,
    })
}

/// Capture just the current stamp under `org_install` for the pre-mutation
/// recheck — it crosses the store publication barrier (`barriered_generation`),
/// never a bare `publish_generation`. `None` when authority/store is absent
/// (also a stale view relative to any prior successful capture).
pub(crate) fn capture_current_sensing_stamp(
    org_install: &Mutex<()>,
    node_authority: &ArcSwapOption<NodeAuthority>,
    org_revocation: &ArcSwapOption<OrgRevocationStore>,
    org_install_generation: &AtomicU64,
) -> Option<SensingAuthorityStamp> {
    let _install = org_install.lock();
    let authority = node_authority.load_full()?;
    let store = org_revocation.load_full()?;
    Some(SensingAuthorityStamp {
        authority_ptr: Arc::as_ptr(&authority) as *const () as usize,
        store_ptr: Arc::as_ptr(&store) as *const () as usize,
        store_generation: store.barriered_generation().ok(),
        installation_generation: org_install_generation.load(Ordering::Acquire),
        poisoned: store.is_poisoned(),
    })
}

/// Run `mutate` if and only if `expected` is still the current authority stamp,
/// with `org_install` HELD across both the comparison and the mutation.
///
/// This closes a real window. [`capture_current_sensing_stamp`] releases
/// `org_install` before it returns, so a caller that compared its result and
/// then mutated a table was SAMPLING authority, not excluding it: an authority
/// swap, a store swap, an `A -> B -> exact-A` rotation, a floor raise
/// (`apply_bundle` -> `StoreCore::publish`) or a poison mark could all land in
/// between and the row would be created anyway — a local row and a ticket for a
/// frame the provider is now guaranteed to reject.
///
/// The shape mirrors [`capture_live_org_relay_membership_seamed`], which
/// already performs its currency recheck under the same lock rather than
/// afterwards.
///
/// # What may run inside
///
/// `mutate` MUST be a bounded, pure state mutation — the interest-table
/// register/deregister call and nothing else. No cryptography, no frame
/// authoring, no encoding, no I/O, no callbacks, no channel sends, no lock
/// acquisition that is not already ordered below `org_install`. The authority
/// installation path holds this same lock across Ed25519 verification and a
/// fold sweep, so anything slow here directly stalls installs.
///
/// # Lock order
///
/// `sensing_interest_table` -> `org_install`, matching the two existing
/// comparison sites. No authority-installation or floor-publication path
/// acquires any sensing lock, so the reverse edge does not exist and this
/// cannot deadlock.
///
/// `fence_seam` fires after a SUCCESSFUL comparison and before `mutate`, with
/// `org_install` held. It exists so a witness can prove an authority
/// installation or floor publication attempted in that instant cannot complete
/// ahead of the mutation.
pub(crate) fn with_fenced_current_authority<R>(
    org_install: &Mutex<()>,
    node_authority: &ArcSwapOption<NodeAuthority>,
    org_revocation: &ArcSwapOption<OrgRevocationStore>,
    org_install_generation: &AtomicU64,
    expected: &SensingAuthorityStamp,
    fence_seam: Option<&(dyn Fn() + Send + Sync)>,
    mutate: impl FnOnce() -> R,
) -> Option<R> {
    let _install = org_install.lock();
    let authority = node_authority.load_full()?;
    let store = org_revocation.load_full()?;
    let current = SensingAuthorityStamp {
        authority_ptr: Arc::as_ptr(&authority) as *const () as usize,
        store_ptr: Arc::as_ptr(&store) as *const () as usize,
        store_generation: store.barriered_generation().ok(),
        installation_generation: org_install_generation.load(Ordering::Acquire),
        poisoned: store.is_poisoned(),
    };
    if !expected.is_current(&current) {
        return None;
    }
    if let Some(seam) = fence_seam {
        seam();
    }
    // STILL HOLDING `org_install`: no install, swap, rotation, floor
    // publication or poison mark can be observed between the verdict above and
    // the mutation below.
    Some(mutate())
}

/// A pinned, live proof of THIS node's own organization membership, captured so
/// a relay may re-author an organization sensing registration upstream under
/// its OWN certificate.
///
/// The org sensing plane forwards a downstream interest as a FRESH
/// `OrgProviderRegistration` at every hop, and each hop must vouch with the
/// membership it can prove RIGHT NOW — never the downstream consumer's
/// certificate, and never one that verified only at startup. This is the
/// late-bound relay authority: [`capture_live_org_relay_membership`] re-runs the
/// EXACT startup ownership check (`NodeAuthorityConfig::self_verify_at`) against
/// the LIVE revocation floors at an explicit `now_secs`, so a relay whose own
/// membership has since expired or been revoked re-authors NOTHING — the branch
/// stays advisory (`Unknown`/`Potential`) rather than forwarding under a stale
/// certificate.
///
/// **Independent of the OA `owner_cert_emission_enabled` toggle.** That flag
/// governs whether this node advertises its owner certificate on the OA
/// discovery/announcement surface. Sensing relay re-authoring is a DISTINCT
/// authorization: the sensing plane runs its own organization-authority gate
/// ([`verify_org_sensing_registration`]), so a relay forwarding an org sensing
/// interest vouches under its live membership regardless of the announcement
/// surface's emission policy. The two must not be coupled — silencing OA
/// announcements must not silence in-mesh sensing relay, and vice versa.
///
/// The retained `Arc`s pin the exact authority + store the membership was
/// proven against, alive from capture through the upstream re-authoring emit.
///
/// The accessors are consumed by the live provider re-authoring emit
/// ([`plan_provider_continuation`] captures this at the relay hop); the residual
/// `#[allow(dead_code)]` now covers only the `_authority`/`_store` pins, held
/// solely to keep the exact authority + store alive and never read directly.
#[allow(dead_code)]
pub(crate) struct LiveOrgRelayMembership {
    owner_cert: OrgMembershipCert,
    org_id: OrgId,
    // Pins — held to keep the exact authority + store alive from capture
    // through the upstream re-authoring emit.
    _authority: Arc<NodeAuthority>,
    _store: Arc<OrgRevocationStore>,
}

#[allow(dead_code)]
impl LiveOrgRelayMembership {
    /// This relay's OWN membership certificate — the value attached to the fresh
    /// `OrgProviderRegistration` emitted upstream. It is the node's live,
    /// self-verified owner certificate, never a value copied from the downstream
    /// frame.
    pub(crate) fn owner_cert(&self) -> &OrgMembershipCert {
        &self.owner_cert
    }

    /// The relay's verified organization — equal to the incoming registration's
    /// org (the capture refuses a foreign org) and to the certificate's issuer.
    pub(crate) fn org_id(&self) -> OrgId {
        self.org_id
    }
}

/// Why THIS relay cannot vouch for an organization sensing re-authoring right
/// now. Every variant leaves the branch advisory (`Unknown`/`Potential`): a
/// relay that cannot prove its OWN live membership forwards nothing — it never
/// downgrades to a legacy frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RelayMembershipUnavailable {
    /// No [`NodeAuthority`] is installed — this node has no membership to vouch
    /// with.
    NoAuthority,
    /// No [`OrgRevocationStore`] is installed — the revocation floor cannot be
    /// evaluated, so membership cannot be proven current.
    NoStore,
    /// The installed store is poisoned.
    Poisoned,
    /// The store's publication generation space is exhausted, so a floor raise
    /// between the snapshot and the linearization point can no longer be
    /// detected — refuse rather than vouch against an unwitnessable view.
    GenerationExhausted,
    /// The incoming registration's organization is not this node's owner
    /// organization — a relay only re-authors within its OWN organization. This
    /// is also the late-bound guard against the authority having rotated to a
    /// different org between the incoming validation and this capture.
    ForeignOrg,
    /// This node's own certificate names a different entity than the supplied
    /// local entity — a mis-installed authority (defense in depth; a loaded
    /// [`NodeAuthority`] proved this binding at `open()`).
    NotForThisNode,
    /// This node's own certificate failed signature or time-window validation at
    /// `now_secs` under the persisted skew.
    CertInvalid,
    /// This node's own certificate generation is below the current revocation
    /// floor for its `(org, member)` — its membership has been revoked.
    BelowFloor,
    /// A revocation published — moving the store's floor generation — between
    /// the coherent floor snapshot the verdict was computed against and the final
    /// currency recheck, so the verdict is stale. Refused advisorily rather than
    /// returning a membership (or a floor verdict) proven against a floor view
    /// that is no longer live; the soft-state refresh retries naturally. (A store
    /// that POISONS mid-capture is caught first as `Poisoned` — poison does not
    /// move the generation, so the final live poison check is what catches it.)
    ViewChanged,
}

/// Capture a pinned, live proof of THIS relay's own organization membership for
/// re-authoring an org sensing registration whose (already-validated)
/// organization is `expected_org`, at an explicit `now_secs`.
///
/// `org_install` is held throughout so the authority and store IDENTITY cannot
/// be replaced mid-gate (the same lock the piece-1 captures use). It does NOT,
/// however, gate floor publication: a concurrent `apply_bundle` raises the floor
/// through the store's OWN publication path, so a coherent floor snapshot alone
/// is not a currency proof. The gate therefore captures the floor snapshot
/// paired with its publication generation, runs the membership self-verify with
/// no store publish guard held across the signature check, and makes the END of
/// the gate the explicit linearization point: it crosses the store publication
/// barrier and re-checks poison, and gates BOTH the success and the
/// snapshot-dependent failure result behind that currency check. A floor raise
/// or poison that publishes between the snapshot and the recheck yields an
/// advisory `ViewChanged` — never a membership (or a specific floor verdict)
/// computed against a floor view that is no longer live.
///
/// The relay's membership bar is IDENTICAL to the startup ownership bar
/// (`NodeAuthorityConfig::self_verify_at`) — binding, signature, window at
/// `now_secs` + persisted skew, and revocation floor — so an expired or revoked
/// relay proves nothing. On success the relay's own `owner_cert` is returned for
/// the caller to attach to the fresh upstream `OrgProviderRegistration`; the
/// exact authority + store are pinned so the emit runs against the objects the
/// proof was taken against.
///
/// Free-standing over the raw node fields (mirroring the piece-1 captures) so
/// both `MeshNode` and the dispatch context can call it.
pub(crate) fn capture_live_org_relay_membership(
    org_install: &Mutex<()>,
    node_authority: &ArcSwapOption<NodeAuthority>,
    org_revocation: &ArcSwapOption<OrgRevocationStore>,
    local_entity: &EntityId,
    expected_org: OrgId,
    now_secs: u64,
) -> Result<LiveOrgRelayMembership, RelayMembershipUnavailable> {
    capture_live_org_relay_membership_seamed(
        org_install,
        node_authority,
        org_revocation,
        local_entity,
        expected_org,
        now_secs,
        || {},
    )
}

/// [`capture_live_org_relay_membership`] with a test seam invoked exactly once
/// AFTER the coherent floor snapshot and BEFORE the final currency recheck. In
/// production the seam is `|| {}` (inlined away); the in-crate race witnesses
/// pass a pause closure to publish a floor raise / poison the store while the
/// gate is parked between the snapshot and the recheck.
fn capture_live_org_relay_membership_seamed(
    org_install: &Mutex<()>,
    node_authority: &ArcSwapOption<NodeAuthority>,
    org_revocation: &ArcSwapOption<OrgRevocationStore>,
    local_entity: &EntityId,
    expected_org: OrgId,
    now_secs: u64,
    after_floor_snapshot: impl FnOnce(),
) -> Result<LiveOrgRelayMembership, RelayMembershipUnavailable> {
    let _install = org_install.lock();
    let authority = node_authority
        .load_full()
        .ok_or(RelayMembershipUnavailable::NoAuthority)?;
    let store = org_revocation
        .load_full()
        .ok_or(RelayMembershipUnavailable::NoStore)?;
    if store.is_poisoned() {
        return Err(RelayMembershipUnavailable::Poisoned);
    }
    // A relay only re-authors within its OWN organization. Checked before the
    // floor snapshot — so ForeignOrg ordering is preserved — and doubling as the
    // late-bound guard against an authority rotation to a different org between
    // the incoming validation and this capture.
    if authority.owner_org() != expected_org {
        return Err(RelayMembershipUnavailable::ForeignOrg);
    }

    // Capture a coherent floor snapshot paired with its publication generation,
    // then run the membership self-verify WITHOUT holding any store publish guard
    // across the signature check.
    let (floors, captured_generation) = store
        .snapshot_with_generation()
        .map_err(|_| RelayMembershipUnavailable::GenerationExhausted)?;
    after_floor_snapshot();
    let verification = authority
        .config
        .self_verify_at(local_entity, &floors, now_secs);

    // The END of the gate is the explicit linearization point. Cross the store
    // publication barrier and re-check poison, then gate BOTH the success and the
    // (snapshot-dependent) verification verdict behind currency: a floor raise or
    // poison that published between the snapshot and here makes the verdict
    // stale, so refuse with an advisory `ViewChanged` rather than returning a
    // membership — or a specific floor verdict — proven against a floor view that
    // is no longer live. An early `?` on `verification` would bypass this, so the
    // Result is held, not propagated, until currency is established.
    let current_generation = store
        .barriered_generation()
        .map_err(|_| RelayMembershipUnavailable::GenerationExhausted)?;
    if store.is_poisoned() {
        return Err(RelayMembershipUnavailable::Poisoned);
    }
    if current_generation != captured_generation {
        return Err(RelayMembershipUnavailable::ViewChanged);
    }
    verification.map_err(map_self_verify_error)?;

    Ok(LiveOrgRelayMembership {
        owner_cert: authority.config.owner_cert.clone(),
        org_id: authority.owner_org(),
        _authority: authority,
        _store: store,
    })
}

/// Narrow the startup self-verify error onto the relay-membership refusal. A
/// loaded [`NodeAuthority`] already passed the structural binding and version
/// checks at `open()`, so the reachable LIVE failures are a signature/window
/// lapse (`CertInvalid`) and a revocation floor raised since boot (`BelowFloor`);
/// a supplied local entity that does not match the certificate surfaces as
/// `NotForThisNode`. Any residual structural variant a loaded authority cannot
/// exhibit collapses to `CertInvalid`.
fn map_self_verify_error(e: OrgAuthorityError) -> RelayMembershipUnavailable {
    match e {
        OrgAuthorityError::CertBelowFloor { .. } => RelayMembershipUnavailable::BelowFloor,
        OrgAuthorityError::CertNotForThisNode { .. } => RelayMembershipUnavailable::NotForThisNode,
        _ => RelayMembershipUnavailable::CertInvalid,
    }
}

/// The authority-aware sensing PROVIDER continuation: given an admitted
/// registration whose leg is a [`RegistrationLeg::Provider`], produce the fresh
/// upstream frame — or `None` (emit nothing). Every field of the emitted frame
/// comes from the admitted wrapper: the spec, the authority, AND the target +
/// interval + ttl (from the leg). A caller cannot admit one leg and then supply
/// a different target/interval/ttl at planning time — the ONLY way to set the
/// upstream target and the post-aggregate strictest interval is to derive a
/// provider leg first via [`AdmittedSensingRegistration::provider_continuation`],
/// so the leg is the sole, immutable, load-bearing source. A non-provider (e.g.
/// capability) seed reaching here is a caller error and yields `None`.
///
/// The split matches EXHAUSTIVELY on [`AdmittedSensingRegistration::authority`]
/// with NO wildcard, so adding a future authority mode breaks compilation here
/// rather than silently defaulting to a legacy egress:
///
/// - Legacy → the existing [`SensingInterestFrame::provider_registration`] shape.
/// - Org → a FRESH [`SensingInterestFrame::org_provider_registration`] carrying
///   THIS relay's own certificate, captured late via `capture_membership`. The
///   certificate is sourced ONLY from the captured membership — never from the
///   downstream frame (the admitted wrapper does not even retain a downstream
///   cert) — and if the relay's live membership is unavailable
///   (`capture_membership` returns `None`) the result is `None`: no frame, and
///   specifically NO legacy fallback. A relay that cannot vouch under its own
///   live org membership forwards nothing.
///
/// `capture_membership` is invoked ONLY in the org arm, at the point of frame
/// construction, so it is the latest synchronous capture point — no work
/// intervenes between the membership capture and the frame it is bound into.
/// Legacy callers pass a fail-closed `|_| None`: it is never invoked for a legacy
/// admission, and an org admission arriving at a not-yet-live legacy seam emits
/// nothing rather than downgrading.
///
/// The org/legacy split is safe to drive off `authority()` alone because org and
/// legacy interests cannot coalesce under one `ProviderInterestKey` (the interest
/// digest binds the audience, and the org-commitment and entity-owner-root
/// audience families are cryptographically disjoint under BLAKE3/Ed25519
/// preimage assumptions). Authority mode is taken from the admitted evidence,
/// never inferred from the audience bytes.
pub(crate) fn plan_provider_continuation(
    admitted: &AdmittedSensingRegistration,
    capture_membership: impl FnOnce(OrgId) -> Option<LiveOrgRelayMembership>,
) -> Option<SensingInterestFrame> {
    // The upstream target and the post-aggregate strictest interval + ttl are
    // the admitted PROVIDER leg — never independently supplied. A non-provider
    // seed cannot be emitted as a provider continuation.
    let RegistrationLeg::Provider {
        target,
        requested_sample_interval: strictest,
        soft_state_ttl: ttl,
    } = admitted.leg()
    else {
        return None;
    };

    match admitted.authority() {
        RegistrationAuthority::Legacy { .. } => Some(SensingInterestFrame::provider_registration(
            admitted.spec(),
            target,
            strictest,
            ttl,
        )),
        RegistrationAuthority::Org { org_id } => {
            let membership = capture_membership(*org_id)?;
            Some(SensingInterestFrame::org_provider_registration(
                admitted.spec(),
                target,
                strictest,
                ttl,
                membership.owner_cert().clone(),
            ))
        }
    }
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
        match validated.0 {
            ValidatedInner::Capability {
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
            validated.0,
            ValidatedInner::Provider { target: 0x77, .. }
        ));
    }

    // ---- review §8: org-gate rejection arms without direct witnesses -----

    /// A non-organization frame is refused at step 1 as `NotOrgRegistration`,
    /// before any membership work.
    #[test]
    fn a_non_org_frame_is_refused_as_not_org_registration() {
        let frame =
            SensingInterestFrame::provider_registration(&spec_with(org_commit()), 0x77, D, TTL);
        let err = run(&frame, &member(), Some(authority()), &empty_floors()).unwrap_err();
        assert!(
            matches!(err, OrgSensingRejection::NotOrgRegistration),
            "got {err:?}"
        );
    }

    /// A digest-inconsistent AUDIENCE on an ORG frame fails at step 2 (Semantic),
    /// before the step-8 audience check — the embedded `interest_digest` commits
    /// to the org-commitment audience, so a swapped `audience_scope` no longer
    /// reconstructs to that digest. (The `frames.rs` red matrix mutates only the
    /// legacy leg; this is the org-leg analog.)
    #[test]
    fn a_digest_inconsistent_audience_on_an_org_frame_fails_at_step_2() {
        let mut frame = SensingInterestFrame::org_provider_registration(
            &spec_with(org_commit()),
            0x77,
            D,
            TTL,
            cert_gen(5),
        );
        if let SensingInterestFrame::OrgProviderRegistration { audience_scope, .. } = &mut frame {
            *audience_scope = AudienceScopeCommitment::from_bytes([0xABu8; 32]);
        }
        let err = run(&frame, &member(), Some(authority()), &empty_floors()).unwrap_err();
        assert!(
            matches!(err, OrgSensingRejection::Semantic(_)),
            "a digest-inconsistent org audience must fail at step 2, got {err:?}"
        );
    }

    /// Review §4: an org-gate refusal that was previously silent now bumps its
    /// per-reason counter.
    #[test]
    fn a_foreign_org_refusal_bumps_its_counter() {
        let counters = SensingCounters::default();
        let foreign_kp = OrgKeypair::from_bytes([0x99u8; 32]);
        let foreign_cert =
            OrgMembershipCert::try_issue(&foreign_kp, member(), 5, ORG_CERT_TTL_SECS_RECOMMENDED)
                .expect("foreign cert");
        let frame = SensingInterestFrame::org_provider_registration(
            &spec_with(canonical_org_sensing_commitment(&foreign_kp.org_id())),
            0x77,
            D,
            TTL,
            foreign_cert,
        );
        let err = verify_org_sensing_registration(
            &frame,
            FROM_NODE,
            &member(),
            Some(authority()),
            &empty_floors(),
            now_secs(),
            &counters,
        )
        .unwrap_err();
        assert!(
            matches!(err, OrgSensingRejection::ForeignOrg),
            "got {err:?}"
        );
        assert_eq!(
            SensingCounters::get(&counters.org_foreign_org),
            1,
            "the foreign-org refusal is counted (previously silent)"
        );
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

    // ---- admitted wrapper provenance ----------------------------------
    #[test]
    fn org_admitted_wrapper_derives_proven_root_from_org_id() {
        let validated = ValidatedOrgSensingRegistration::capability_for_test(
            spec_with(org_commit()),
            FROM_NODE,
            D,
            TTL,
            member(),
            org_kp().org_id(),
        );
        let admitted = AdmittedSensingRegistration::from_validated_org(validated);
        // Derived from the verified org id, never supplied.
        assert_eq!(
            admitted.proven_root(),
            canonical_org_sensing_commitment(&org_kp().org_id())
        );
        // And never an entity root.
        assert_ne!(
            admitted.proven_root(),
            AudienceScopeCommitment::owner_root(&member())
        );
        assert!(matches!(
            admitted.authority(),
            RegistrationAuthority::Org { .. }
        ));
    }

    /// 2026-07-23 §7 residual: the legacy wrapper DERIVES its proven root from
    /// the spec it admits, so an admission cannot pair one interest's spec with
    /// another interest's proven root. The old signature took the two
    /// independently and nothing in the type related them.
    #[test]
    fn legacy_admitted_wrapper_derives_the_proven_root_from_its_own_spec() {
        let root = AudienceScopeCommitment::owner_root(&member());
        let admitted = AdmittedSensingRegistration::from_validated_legacy(
            spec_with(root),
            RegistrationLeg::Capability {
                consumer: FROM_NODE,
                requested_sample_interval: D,
                soft_state_ttl: TTL,
            },
        );
        assert_eq!(
            admitted.proven_root(),
            root,
            "the admitted root is the admitted spec's audience, by construction"
        );
        assert_eq!(
            admitted.proven_root(),
            admitted.spec().audience,
            "and they cannot diverge — there is no second parameter to disagree with"
        );
        assert!(matches!(
            admitted.authority(),
            RegistrationAuthority::Legacy { .. }
        ));
    }

    // ---- authority stamp currency discrimination ----------------------
    fn stamp(
        authority_ptr: usize,
        store_ptr: usize,
        store_generation: u64,
        installation_generation: u64,
        poisoned: bool,
    ) -> SensingAuthorityStamp {
        SensingAuthorityStamp {
            authority_ptr,
            store_ptr,
            store_generation: Some(BarrieredGeneration::from_raw_for_test(store_generation)),
            installation_generation,
            poisoned,
        }
    }

    #[test]
    fn identical_stamp_is_current() {
        let captured = stamp(1, 2, 3, 4, false);
        assert!(captured.is_current(&stamp(1, 2, 3, 4, false)));
    }

    #[test]
    fn a_b_a_rotation_is_stale_via_installation_generation() {
        // The authority Arc pointer returns to its original value (authority_ptr
        // and store_ptr unchanged) but two installs advanced the installation
        // generation — the pointer alone would falsely match.
        let captured = stamp(1, 2, 3, 4, false);
        let after_a_b_a = stamp(1, 2, 3, 6, false);
        assert!(!captured.is_current(&after_a_b_a));
    }

    #[test]
    fn floor_raise_on_same_store_is_stale() {
        let captured = stamp(1, 2, 3, 4, false);
        assert!(!captured.is_current(&stamp(1, 2, 4, 4, false)));
    }

    #[test]
    fn authority_or_store_pointer_change_is_stale() {
        let captured = stamp(1, 2, 3, 4, false);
        assert!(!captured.is_current(&stamp(9, 2, 3, 4, false)));
        assert!(!captured.is_current(&stamp(1, 9, 3, 4, false)));
    }

    #[test]
    fn poison_transition_is_stale_even_with_equal_numeric_fields() {
        // A store may poison without changing any pointer or generation.
        let captured = stamp(1, 2, 3, 4, false);
        assert!(!captured.is_current(&stamp(1, 2, 3, 4, true)));
    }

    /// The mirror of `admission_stamp_currency`'s exhaustion cell
    /// (review-pass-2 §4). `capture_sensing_authority_snapshot` refuses before
    /// it ever builds an exhausted stamp, so this is currently unreachable
    /// through the production caller — which is exactly why the guard belongs in
    /// the type rather than in that one function.
    #[test]
    fn an_exhausted_generation_is_never_current_in_either_position() {
        let base = stamp(1, 2, 3, 4, false);
        let mut exhausted = base;
        exhausted.store_generation = None;

        assert!(
            !base.is_current(&exhausted),
            "a live view whose generation is exhausted cannot be shown current"
        );
        assert!(
            !exhausted.is_current(&base),
            "a captured view sampled at exhaustion cannot be shown still live"
        );
        assert!(
            !exhausted.is_current(&exhausted),
            "and two exhausted samples must NOT compare equal-and-current"
        );
    }

    // ---- piece-2: live relay re-authoring membership ------------------
    //
    // `capture_live_org_relay_membership` against a REAL adopted authority + its
    // live store. Two cells are behavioral RED-couplings, not mere refusals:
    // `relay_expired_at_future_now_is_cert_invalid` fails if the gate reads
    // `current_timestamp()` instead of the explicit `now_secs`, and
    // `relay_below_live_floor_is_refused` fails if it verifies against the
    // authority's startup floors instead of the LIVE store's floors.
    use crate::adapter::net::behavior::org::OrgRevocationBundle;
    use crate::adapter::net::behavior::org_revocation::ProvisioningExpectation;
    use crate::adapter::net::identity::EntityKeypair;
    use std::sync::atomic::AtomicUsize;

    fn scratch(tag: &str) -> std::path::PathBuf {
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "net-relay-membership-{tag}-{}-{seq}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn foreign_org() -> OrgId {
        OrgKeypair::from_bytes([0x99u8; 32]).org_id()
    }

    /// Adopt a real authority for a freshly-generated entity under `org_kp()` at
    /// cert generation `generation`, returning the entity, the authority, and
    /// the live revocation store the ceremony created.
    fn adopt_relay(
        tag: &str,
        generation: u32,
    ) -> (EntityId, Arc<NodeAuthority>, Arc<OrgRevocationStore>) {
        let kp = EntityKeypair::generate();
        let entity = kp.entity_id().clone();
        let cert = OrgMembershipCert::try_issue(
            &org_kp(),
            entity.clone(),
            generation,
            ORG_CERT_TTL_SECS_RECOMMENDED,
        )
        .expect("issue relay cert");
        let authority = Arc::new(
            NodeAuthority::adopt(&scratch(tag), cert, &entity, 60, None)
                .expect("adopt relay authority"),
        );
        let store = authority.revocation.clone();
        (entity, authority, store)
    }

    fn capture_relay(
        authority: Option<Arc<NodeAuthority>>,
        store: Option<Arc<OrgRevocationStore>>,
        local_entity: &EntityId,
        expected_org: OrgId,
        now: u64,
    ) -> Result<LiveOrgRelayMembership, RelayMembershipUnavailable> {
        let na = ArcSwapOption::from(authority);
        let rev = ArcSwapOption::from(store);
        let lock = Mutex::new(());
        capture_live_org_relay_membership(&lock, &na, &rev, local_entity, expected_org, now)
    }

    #[test]
    fn live_relay_membership_returns_this_nodes_own_cert() {
        let (entity, authority, store) = adopt_relay("ok", 3);
        let membership = capture_relay(
            Some(authority),
            Some(store),
            &entity,
            org_kp().org_id(),
            now_secs(),
        )
        .expect("relay membership");
        // The returned cert is THIS node's own membership — its own entity, its
        // own org, the adopted generation — never a downstream value.
        assert_eq!(membership.owner_cert().member, entity);
        assert_eq!(membership.owner_cert().org_id, org_kp().org_id());
        assert_eq!(membership.owner_cert().generation, 3);
        assert_eq!(membership.org_id(), org_kp().org_id());
    }

    #[test]
    fn no_authority_installed_is_refused() {
        let (entity, _authority, store) = adopt_relay("noauth", 1);
        assert_eq!(
            capture_relay(None, Some(store), &entity, org_kp().org_id(), now_secs()).err(),
            Some(RelayMembershipUnavailable::NoAuthority)
        );
    }

    #[test]
    fn no_store_installed_is_refused() {
        let (entity, authority, _store) = adopt_relay("nostore", 1);
        assert_eq!(
            capture_relay(
                Some(authority),
                None,
                &entity,
                org_kp().org_id(),
                now_secs()
            )
            .err(),
            Some(RelayMembershipUnavailable::NoStore)
        );
    }

    #[test]
    fn poisoned_store_is_refused() {
        let (entity, authority, store) = adopt_relay("poison", 1);
        store.mark_poisoned_for_test();
        assert_eq!(
            capture_relay(
                Some(authority),
                Some(store),
                &entity,
                org_kp().org_id(),
                now_secs()
            )
            .err(),
            Some(RelayMembershipUnavailable::Poisoned)
        );
    }

    #[test]
    fn foreign_org_registration_is_refused() {
        // A perfectly valid relay membership, but the incoming registration
        // names a different organization — a relay never re-authors outside its
        // own org.
        let (entity, authority, store) = adopt_relay("foreign", 1);
        assert_eq!(
            capture_relay(
                Some(authority),
                Some(store),
                &entity,
                foreign_org(),
                now_secs()
            )
            .err(),
            Some(RelayMembershipUnavailable::ForeignOrg)
        );
    }

    #[test]
    fn wrong_local_entity_is_not_for_this_node() {
        // Defense in depth: the supplied local entity is not the one the
        // authority's certificate names.
        let (_entity, authority, store) = adopt_relay("wrongentity", 1);
        let other = EntityId::from_bytes([0xEEu8; 32]);
        assert_eq!(
            capture_relay(
                Some(authority),
                Some(store),
                &other,
                org_kp().org_id(),
                now_secs()
            )
            .err(),
            Some(RelayMembershipUnavailable::NotForThisNode)
        );
    }

    #[test]
    fn relay_expired_at_future_now_is_cert_invalid() {
        // RED coupling for the explicit-time thread: the SAME authority is
        // admitted at the present instant and refused at a `now_secs` past its
        // window + skew. A gate that read `current_timestamp()` instead of
        // `now_secs` would admit both.
        let (entity, authority, store) = adopt_relay("expired", 1);
        assert!(capture_relay(
            Some(authority.clone()),
            Some(store.clone()),
            &entity,
            org_kp().org_id(),
            now_secs(),
        )
        .is_ok());
        let far_future = now_secs() + ORG_CERT_TTL_SECS_RECOMMENDED + 1_000;
        assert_eq!(
            capture_relay(
                Some(authority),
                Some(store),
                &entity,
                org_kp().org_id(),
                far_future
            )
            .err(),
            Some(RelayMembershipUnavailable::CertInvalid)
        );
    }

    #[test]
    fn relay_below_live_floor_is_refused() {
        // RED coupling for the LIVE-store floor read: the relay's own
        // generation-1 cert is verified against the store PASSED to the gate
        // (the live installed store), NOT the authority's embedded startup
        // store. A DISTINCT live store carrying a floor that revokes generation 1
        // refuses the cert; a gate reading `authority.revocation` (empty) would
        // wrongly admit it. Using the authority's own store here would make the
        // two indistinguishable — they must be different objects.
        let (entity, authority, _embedded) = adopt_relay("floored", 1);
        let live = Arc::new(
            OrgRevocationStore::init(scratch("floored-live"), ProvisioningExpectation::MayBeFresh)
                .expect("init live store"),
        );
        let mut floors = BTreeMap::new();
        floors.insert(entity.clone(), 2u32);
        let bundle = OrgRevocationBundle::try_issue(&org_kp(), &floors).expect("bundle");
        live.apply_bundle(&bundle).expect("apply floor raise");
        assert_eq!(
            capture_relay(
                Some(authority),
                Some(live),
                &entity,
                org_kp().org_id(),
                now_secs()
            )
            .err(),
            Some(RelayMembershipUnavailable::BelowFloor)
        );
    }

    #[test]
    fn relay_at_live_floor_is_admitted() {
        // The floor check is `>=`: a generation exactly at the live floor still
        // vouches.
        let (entity, authority, store) = adopt_relay("atfloor", 2);
        let mut floors = BTreeMap::new();
        floors.insert(entity.clone(), 2u32);
        let bundle = OrgRevocationBundle::try_issue(&org_kp(), &floors).expect("bundle");
        store.apply_bundle(&bundle).expect("apply floor raise");
        assert!(capture_relay(
            Some(authority),
            Some(store),
            &entity,
            org_kp().org_id(),
            now_secs(),
        )
        .is_ok());
    }

    #[test]
    fn foreign_org_precedes_cert_validity() {
        // Ordering: a foreign-org interest is refused as ForeignOrg even when
        // this node's own certificate is ALSO invalid at the supplied instant —
        // the relay simply does not serve that org.
        let (entity, authority, store) = adopt_relay("orderfirst", 1);
        let far_future = now_secs() + ORG_CERT_TTL_SECS_RECOMMENDED + 1_000;
        assert_eq!(
            capture_relay(
                Some(authority),
                Some(store),
                &entity,
                foreign_org(),
                far_future
            )
            .err(),
            Some(RelayMembershipUnavailable::ForeignOrg)
        );
    }

    #[test]
    fn floor_raise_during_capture_is_view_changed() {
        // Deterministic TOCTOU witness: the gate captures the coherent floor
        // snapshot, then parks (the test seam) while another thread publishes a
        // real signed floor raise through `apply_bundle`. On resume the gate
        // crosses the publication barrier, observes the generation moved, and
        // returns ViewChanged — never a membership proven against the stale
        // floor. `org_install` is held throughout and floor publication does NOT
        // take it, so the raise proceeds concurrently (no deadlock).
        //
        // RED coupling (verified, not committed): removing the final
        // `current_generation != captured_generation` comparison makes this
        // return Ok — the generation-1 cert verifies against the captured floor.
        use std::sync::Barrier;

        let (entity, authority, store) = adopt_relay("viewchange", 1);
        let na = ArcSwapOption::from(Some(authority));
        let rev = ArcSwapOption::from(Some(store.clone()));
        let lock = Mutex::new(());
        let paused = Barrier::new(2);
        let release = Barrier::new(2);

        std::thread::scope(|s| {
            let gate = s.spawn(|| {
                capture_live_org_relay_membership_seamed(
                    &lock,
                    &na,
                    &rev,
                    &entity,
                    org_kp().org_id(),
                    now_secs(),
                    || {
                        paused.wait();
                        release.wait();
                    },
                )
            });
            paused.wait(); // the gate is parked after the coherent floor snapshot
            let mut floors = BTreeMap::new();
            floors.insert(entity.clone(), 2u32);
            let bundle = OrgRevocationBundle::try_issue(&org_kp(), &floors).expect("bundle");
            store.apply_bundle(&bundle).expect("apply floor raise");
            release.wait(); // let the gate cross the barrier and recheck currency
            let result = gate.join().expect("gate thread");
            assert_eq!(result.err(), Some(RelayMembershipUnavailable::ViewChanged));
        });
    }

    #[test]
    fn poison_during_capture_is_refused() {
        // Companion to the floor-raise witness, pinning the FINAL poison check
        // (not just the initial refusal branch): the gate snapshots a clean
        // store, parks, the store is poisoned, and on resume the gate refuses
        // with Poisoned. Poison does not move the generation, so the final live
        // poison check is what catches it.
        //
        // RED coupling (verified, not committed): removing the final
        // `store.is_poisoned()` check makes this return Ok.
        use std::sync::Barrier;

        let (entity, authority, store) = adopt_relay("poison-mid", 1);
        let na = ArcSwapOption::from(Some(authority));
        let rev = ArcSwapOption::from(Some(store.clone()));
        let lock = Mutex::new(());
        let paused = Barrier::new(2);
        let release = Barrier::new(2);

        std::thread::scope(|s| {
            let gate = s.spawn(|| {
                capture_live_org_relay_membership_seamed(
                    &lock,
                    &na,
                    &rev,
                    &entity,
                    org_kp().org_id(),
                    now_secs(),
                    || {
                        paused.wait();
                        release.wait();
                    },
                )
            });
            paused.wait(); // parked after the (clean) floor snapshot
            store.mark_poisoned_for_test();
            release.wait();
            let result = gate.join().expect("gate thread");
            assert_eq!(result.err(), Some(RelayMembershipUnavailable::Poisoned));
        });
    }

    // ---- piece-3: authority-aware provider continuation ---------------
    //
    // The `plan_provider_continuation` semantic operation, exercised in-process.
    // The match is wildcard-free, so both authority variants are covered here by
    // construction (a future mode would fail to compile). Dispatch stays dark.
    use crate::adapter::net::behavior::sensing::identity::ProviderInterestKey;

    /// A legacy admitted registration (authority = Legacy) with the given
    /// audience — the provider leg parameters are placeholders the planner
    /// ignores (it takes target/interval/ttl as explicit arguments).
    fn legacy_admitted(audience: AudienceScopeCommitment) -> AdmittedSensingRegistration {
        AdmittedSensingRegistration::from_validated_legacy(
            spec_with(audience),
            RegistrationLeg::Provider {
                target: 0x77,
                requested_sample_interval: D,
                soft_state_ttl: TTL,
            },
        )
    }

    /// An org admitted registration (authority = Org), conceptually derived from
    /// consumer `consumer`'s org registration. `from_validated_org` DROPS the
    /// subscriber cert and keeps only the verified org id, so no downstream cert
    /// can survive into the continuation.
    fn org_admitted(consumer: EntityId) -> AdmittedSensingRegistration {
        AdmittedSensingRegistration::from_validated_org(
            ValidatedOrgSensingRegistration::capability_for_test(
                spec_with(org_commit()),
                FROM_NODE,
                D,
                TTL,
                consumer,
                org_kp().org_id(),
            ),
        )
    }

    #[test]
    fn legacy_provider_continuation_emits_legacy_frame() {
        // `legacy_admitted` carries a Provider leg (target 0x77, D, TTL); the
        // planner emits from that leg, and the fail-closed capture is never
        // invoked for a legacy admission.
        let admitted = legacy_admitted(AudienceScopeCommitment::owner_root(&member()));
        let frame = plan_provider_continuation(&admitted, |_| {
            panic!("capture must not run for a legacy admission")
        })
        .expect("legacy continuation frame");
        // Exact equality with the established constructor — "legacy frame shape
        // preserved" is a real witness, not a nearby variant check.
        assert_eq!(
            frame,
            SensingInterestFrame::provider_registration(admitted.spec(), 0x77, D, TTL)
        );
    }

    #[test]
    fn org_provider_continuation_carries_the_relays_own_cert() {
        // Cert-source witness: consumer entity C, relay entity R. The emitted org
        // frame's certificate MUST be R's (from the captured membership), never C.
        // The capability seed passes through the sanctioned provider-leg
        // derivation first — a capability seed cannot enter the provider planner.
        let consumer = EntityId::from_bytes([0xCCu8; 32]);
        let seed = org_admitted(consumer.clone());
        let admitted = seed.provider_continuation(0x77, D, TTL);
        let (relay_entity, authority, store) = adopt_relay("continuation", 1);
        let membership = capture_relay(
            Some(authority),
            Some(store),
            &relay_entity,
            org_kp().org_id(),
            now_secs(),
        )
        .expect("relay membership");
        let frame = plan_provider_continuation(&admitted, |org| {
            assert_eq!(org, org_kp().org_id(), "captured for the admitted org");
            Some(membership)
        })
        .expect("org continuation frame");
        match frame {
            SensingInterestFrame::OrgProviderRegistration {
                subscriber_membership,
                target,
                ..
            } => {
                assert_eq!(target, 0x77);
                assert_eq!(
                    subscriber_membership.member, relay_entity,
                    "the continuation carries the relay's own cert"
                );
                assert_ne!(
                    subscriber_membership.member, consumer,
                    "never the downstream consumer's cert"
                );
            }
            _ => panic!("expected OrgProviderRegistration"),
        }
    }

    #[test]
    fn org_continuation_without_membership_emits_nothing_and_no_fallback() {
        // Relay membership unavailable → NO frame, and specifically NO legacy
        // ProviderRegistration fallback. A proper provider leg is derived first,
        // so the `None` is the membership gate — not the leg guard.
        let admitted =
            org_admitted(EntityId::from_bytes([0xCCu8; 32])).provider_continuation(0x77, D, TTL);
        let planned = plan_provider_continuation(&admitted, |_| None);
        assert!(
            planned.is_none(),
            "an org admission with no live membership emits nothing (no legacy fallback)"
        );
    }

    #[test]
    fn capability_leg_cannot_enter_provider_planner() {
        // The leg is load-bearing: a capability seed must pass through
        // `provider_continuation` before it can be a provider continuation.
        // Passing a capability-leg admission directly emits NOTHING and never
        // even reaches the capture. This RED-fails a planner that reads
        // independently supplied fields instead of the admitted leg.
        let org_cap = org_admitted(EntityId::from_bytes([0xCCu8; 32])); // Capability leg
        assert!(
            plan_provider_continuation(&org_cap, |_| {
                panic!("capture must not run for a non-provider leg")
            })
            .is_none(),
            "an org capability seed is not a provider continuation"
        );
        let legacy_cap = AdmittedSensingRegistration::from_validated_legacy(
            spec_with(AudienceScopeCommitment::owner_root(&member())),
            RegistrationLeg::Capability {
                consumer: FROM_NODE,
                requested_sample_interval: D,
                soft_state_ttl: TTL,
            },
        );
        assert!(
            plan_provider_continuation(&legacy_cap, |_| None).is_none(),
            "a legacy capability seed is not a provider continuation"
        );
    }

    #[test]
    fn entity_legacy_root_and_org_commitment_are_cryptographically_separated() {
        // Scoped key-separation proof (Kyra amended verdict). The honest claim:
        // ENTITY-derived legacy roots and canonical organization commitments are
        // cryptographically separated — two specs identical in every predicate
        // field but scoped to an org audience vs an entity owner-root audience
        // derive DIFFERENT ProviderInterestKeys under BLAKE3/Ed25519 preimage
        // assumptions (both audiences share one untagged [u8; 32]).
        //
        // This does NOT hold for an EXPLICIT operator fleet root
        // (`MeshNodeConfig::sensing_owner_root`), which is an arbitrary commitment
        // that can be set equal to a known org commitment by copy — no collision
        // attack needed. That case is closed at ADMISSION, not by these bytes:
        // legacy intake rejects an organization-derived audience when authority is
        // installed, and authority installation is refused over a colliding fleet
        // root (see the mesh dispatch witnesses
        // `org_derived_legacy_audience_refused_while_org_row_lands` and
        // `fleet_root_equal_to_org_commitment_refuses_authority_install`). Authority
        // mode is taken from admitted evidence, never inferred from these bytes.
        let org_spec = spec_with(org_commit());
        let legacy_spec = spec_with(AudienceScopeCommitment::owner_root(&member()));
        let org_key = ProviderInterestKey::new(org_spec.key(), 0x77);
        let legacy_key = ProviderInterestKey::new(legacy_spec.key(), 0x77);
        assert_ne!(
            org_key, legacy_key,
            "an entity owner-root audience and an org commitment must not collide on the key"
        );
    }

    #[test]
    fn provider_continuation_preserves_spec_and_authority() {
        // The sanctioned derivation the leader seams will use: re-target the leg
        // while preserving the validated spec AND the admitted authority
        // provenance — org stays org, legacy stays legacy, never re-inferred from
        // the audience bytes.
        let legacy = legacy_admitted(AudienceScopeCommitment::owner_root(&member()));
        let legacy_cont = legacy.provider_continuation(0x99, D, TTL);
        assert_eq!(legacy_cont.spec(), legacy.spec(), "spec preserved");
        assert_eq!(
            legacy_cont.proven_root(),
            legacy.proven_root(),
            "legacy authority preserved"
        );
        assert!(matches!(
            legacy_cont.authority(),
            RegistrationAuthority::Legacy { .. }
        ));
        assert_eq!(
            legacy_cont.leg(),
            RegistrationLeg::Provider {
                target: 0x99,
                requested_sample_interval: D,
                soft_state_ttl: TTL,
            },
            "leg re-targeted to the provider"
        );

        let org = org_admitted(EntityId::from_bytes([0xCCu8; 32]));
        let org_cont = org.provider_continuation(0x99, D, TTL);
        assert_eq!(org_cont.spec(), org.spec(), "spec preserved");
        assert!(matches!(
            org_cont.authority(),
            RegistrationAuthority::Org { .. }
        ));
        // The org proven root stays the canonical org commitment — derived from
        // the verified org id, never re-inferred from the audience.
        assert_eq!(
            org_cont.proven_root(),
            canonical_org_sensing_commitment(&org_kp().org_id()),
            "org authority preserved"
        );
    }

    // ---- D2.6: the selector <-> target intake invariant -----------------
    //
    // The provider-addressed leg carries BOTH a `target` and, inside the
    // digest-bound spec, a `providers` selector. Before this slice the gate
    // never compared them, so a frame naming provider A while selecting
    // provider B was admitted and produced a real registration. These
    // witnesses pin the comparison, its position in the locked order, its
    // counter class, and the fact that the coherent shape still passes.

    /// Build a provider frame whose selector and target are chosen
    /// independently, so an incoherent pair can be constructed on purpose.
    fn provider_frame_with(
        selector: ProviderSelector,
        target: u64,
        audience: AudienceScopeCommitment,
        cert: OrgMembershipCert,
    ) -> SensingInterestFrame {
        let mut spec = spec_with(audience);
        spec.providers = selector;
        SensingInterestFrame::org_provider_registration(&spec, target, D, TTL, cert)
    }

    /// The local-origin admission refuses a selector that names a provider
    /// other than the leg's own target. Inverse mutation: drop the
    /// `spec.providers == Node(target)` comparison — the frame is admitted and
    /// a registration for a provider nobody selected is produced.
    #[test]
    fn local_org_admission_refuses_a_selector_that_does_not_name_the_target() {
        let frame = provider_frame_with(
            ProviderSelector::Node(0x77),
            0x88,
            org_commit(),
            cert_gen(5),
        );
        assert_eq!(
            run(&frame, &member(), Some(authority()), &empty_floors()),
            Err(OrgSensingRejection::SelectorTargetMismatch),
            "selector names 0x77 while the leg targets 0x88"
        );
        // The mirror image is refused too: the selector must not merely
        // *contain* the target's neighbourhood, it must BE `Node(target)`.
        let swapped = provider_frame_with(
            ProviderSelector::Node(0x88),
            0x77,
            org_commit(),
            cert_gen(5),
        );
        assert_eq!(
            run(&swapped, &member(), Some(authority()), &empty_floors()),
            Err(OrgSensingRejection::SelectorTargetMismatch)
        );
    }

    /// Only `Node(target)` is exact. Every other selector shape is refused —
    /// including a one-element `Nodes([target])`, which "contains" the target
    /// but hashes to a different interest digest, and the three provider-free
    /// shapes, which would make `is_provider_free()` true on a leg that
    /// carries an explicit destination.
    #[test]
    fn every_non_exact_selector_shape_is_refused_at_intake() {
        const TARGET: u64 = 0x77;
        let shapes = [
            ("AnyAuthorized", ProviderSelector::AnyAuthorized),
            ("Nodes([target])", ProviderSelector::Nodes(vec![TARGET])),
            (
                "Nodes([target, other])",
                ProviderSelector::Nodes(vec![TARGET, 0x88]),
            ),
            (
                "Group",
                ProviderSelector::Group(super::super::identity::GroupRef::from_bytes([0x5Au8; 32])),
            ),
            (
                "Tags",
                ProviderSelector::Tags(vec![super::super::identity::TagMatch {
                    key: "rack".to_string(),
                    value: "a1".to_string(),
                }]),
            ),
        ];
        for (label, selector) in shapes {
            let frame = provider_frame_with(selector, TARGET, org_commit(), cert_gen(5));
            assert_eq!(
                run(&frame, &member(), Some(authority()), &empty_floors()),
                Err(OrgSensingRejection::SelectorTargetMismatch),
                "{label} is not an exact provider selector"
            );
        }
    }

    /// The refusal is protocol-invalid input — the sender's own bytes are
    /// internally inconsistent — and it yields no validated registration, so
    /// nothing downstream can mutate a table from it.
    #[test]
    fn the_selector_target_refusal_bumps_protocol_invalid_and_creates_no_row() {
        let counters = SensingCounters::default();
        let frame = provider_frame_with(
            ProviderSelector::AnyAuthorized,
            0x77,
            org_commit(),
            cert_gen(5),
        );
        let before = SensingCounters::get(&counters.protocol_invalid);
        let outcome = verify_org_sensing_registration(
            &frame,
            FROM_NODE,
            &member(),
            Some(authority()),
            &empty_floors(),
            now_secs(),
            &counters,
        );
        assert_eq!(outcome, Err(OrgSensingRejection::SelectorTargetMismatch));
        assert_eq!(
            SensingCounters::get(&counters.protocol_invalid),
            before + 1,
            "a selector/target mismatch is counted as protocol-invalid input"
        );
        // No other refusal class is touched — this is not an authority failure.
        assert_eq!(SensingCounters::get(&counters.org_audience_mismatch), 0);
        assert_eq!(SensingCounters::get(&counters.org_foreign_org), 0);
        assert_eq!(SensingCounters::get(&counters.org_cert_invalid), 0);
        assert_eq!(SensingCounters::get(&counters.org_authority_unavailable), 0);
    }

    /// Position in the locked order: the comparison is a pure function of two
    /// values the frame already carries, so it must run BEFORE any authority,
    /// signature, floor or audience work — and therefore before any effect. A
    /// frame that is simultaneously selector-incoherent and unauthorized is
    /// refused for the selector, proving nothing authority-shaped ran first.
    #[test]
    fn the_selector_target_check_precedes_every_other_authority_refusal() {
        // (a) no authority installed at all: without step 2a this is
        //     `MissingAuthority`.
        let frame = provider_frame_with(
            ProviderSelector::Node(0x77),
            0x88,
            org_commit(),
            cert_gen(5),
        );
        assert_eq!(
            run(&frame, &member(), None, &empty_floors()),
            Err(OrgSensingRejection::SelectorTargetMismatch),
            "step 2a precedes the installed-authority requirement"
        );
        // (b) a foreign organization's certificate: without step 2a this is
        //     `ForeignOrg`.
        let foreign_kp = OrgKeypair::from_bytes([0x77u8; 32]);
        let foreign_cert =
            OrgMembershipCert::try_issue(&foreign_kp, member(), 1, ORG_CERT_TTL_SECS_RECOMMENDED)
                .expect("issue foreign cert");
        let foreign = provider_frame_with(
            ProviderSelector::Node(0x77),
            0x88,
            canonical_org_sensing_commitment(&foreign_kp.org_id()),
            foreign_cert,
        );
        assert_eq!(
            run(&foreign, &member(), Some(authority()), &empty_floors()),
            Err(OrgSensingRejection::SelectorTargetMismatch),
            "step 2a precedes the owner-org comparison"
        );
        // (c) a floored certificate: without step 2a this is `BelowFloor`.
        let floors = floors_at(org_kp().org_id(), member(), 9);
        assert_eq!(
            run(&frame, &member(), Some(authority()), &floors),
            Err(OrgSensingRejection::SelectorTargetMismatch),
            "step 2a precedes the revocation floor"
        );
        // Control: with the selector coherent, each of those refusals is the
        // one that actually fires — step 2a adds no false positives.
        let ok = provider_frame_with(
            ProviderSelector::Node(0x77),
            0x77,
            org_commit(),
            cert_gen(5),
        );
        assert_eq!(
            run(&ok, &member(), None, &empty_floors()),
            Err(OrgSensingRejection::MissingAuthority)
        );
        assert_eq!(
            run(&ok, &member(), Some(authority()), &floors),
            Err(OrgSensingRejection::BelowFloor)
        );
    }

    /// The coherent shape is still admitted, and the spec survives the gate
    /// unchanged — step 2a is a refusal, never a rewrite.
    #[test]
    fn an_exact_selector_naming_the_target_is_admitted_unchanged() {
        const TARGET: u64 = 0x5EED;
        let frame = provider_frame_with(
            ProviderSelector::Node(TARGET),
            TARGET,
            org_commit(),
            cert_gen(5),
        );
        let validated =
            run(&frame, &member(), Some(authority()), &empty_floors()).expect("admitted");
        match &validated.0 {
            ValidatedInner::Provider { spec, target, .. } => {
                assert_eq!(*target, TARGET);
                assert!(
                    matches!(spec.providers, ProviderSelector::Node(n) if n == TARGET),
                    "the selector reaches the admitted object verbatim"
                );
            }
            _ => panic!("expected a provider registration"),
        }
    }

    /// Round trip: a frame the org planner itself emits is admitted by the
    /// gate with no modification, and specifically satisfies the new step 2a.
    /// This is the coupling guard — the producer and the checker agree on the
    /// exact shape, so tightening intake cannot orphan our own egress.
    #[test]
    fn an_emitted_local_org_frame_passes_the_intake_gate_unmodified() {
        const TARGET: u64 = 0x77;
        let seed = org_admitted(EntityId::from_bytes([0xCCu8; 32]));
        let admitted = seed.provider_continuation(TARGET, D, TTL);
        let (relay_entity, relay_authority, store) = adopt_relay("gate-round-trip", 1);
        let membership = capture_relay(
            Some(relay_authority),
            Some(store),
            &relay_entity,
            org_kp().org_id(),
            now_secs(),
        )
        .expect("relay membership");
        let frame = plan_provider_continuation(&admitted, |_| Some(membership))
            .expect("org continuation frame");
        // The emitted frame is fed to the gate byte-for-byte as planned.
        let validated = run(&frame, &relay_entity, Some(authority()), &empty_floors())
            .expect("the planner's own frame must be admitted by the gate");
        match &validated.0 {
            ValidatedInner::Provider { spec, target, .. } => {
                assert_eq!(*target, TARGET);
                assert_eq!(
                    spec,
                    admitted.spec(),
                    "the admitted spec equals the planned spec"
                );
            }
            _ => panic!("expected a provider registration"),
        }
    }

    /// The phase split is real, not cosmetic: `validate_org_frame_shape` takes
    /// NO authority view, NO revocation state and NO `now_secs`, so the
    /// selector/target refusal is structurally incapable of consulting — or
    /// waiting on — any authority. This is the unit-level twin of the
    /// production-path witness in `mesh::sensing_authority_witness_tests`.
    #[test]
    fn the_shape_phase_refuses_a_malformed_selector_with_no_authority_input() {
        let counters = SensingCounters::default();
        let malformed = provider_frame_with(
            ProviderSelector::Node(0x77),
            0x88,
            org_commit(),
            cert_gen(5),
        );
        assert_eq!(
            validate_org_frame_shape(&malformed, &counters).err(),
            Some(OrgSensingRejection::SelectorTargetMismatch),
            "the authority-free phase refuses the malformation on its own"
        );
        // The coherent shape passes the phase and carries its products forward.
        let coherent = provider_frame_with(
            ProviderSelector::Node(0x77),
            0x77,
            org_commit(),
            cert_gen(5),
        );
        let shape = validate_org_frame_shape(&coherent, &counters).expect("shape accepted");
        assert_eq!(shape.provider_target(), Some(0x77));
        // And the two phases compose to exactly the public verifier's verdict.
        assert_eq!(
            verify_org_admission(
                &shape,
                FROM_NODE,
                &member(),
                Some(authority()),
                &empty_floors(),
                now_secs(),
                &counters,
            )
            .is_ok(),
            run(&coherent, &member(), Some(authority()), &empty_floors()).is_ok(),
            "shape + admission agrees with the single-call verifier"
        );
    }

    // ---- LOCAL-ORIGIN ORG EGRESS PLANNER ---------------------------------

    /// Build a live local membership capture for a freshly adopted relay, and
    /// derive the egress plan from it. This is the ONLY way to obtain a
    /// `LocalOrgEgress` — the plan cannot be constructed from caller-supplied
    /// audience/root/certificate material.
    fn local_egress(tag: &str) -> (EntityId, LocalOrgEgress) {
        let (relay_entity, authority, store) = adopt_relay(tag, 1);
        let membership = capture_relay(
            Some(authority),
            Some(store),
            &relay_entity,
            org_kp().org_id(),
            now_secs(),
        )
        .expect("relay membership");
        let plan = LocalOrgEgress::from_live_membership(&membership);
        (relay_entity, plan)
    }

    /// The planner emits ONLY the org variant, carrying THIS node's own live
    /// membership certificate. There is no `Legacy` arm to reach: a
    /// `LocalOrgEgress` can only come from an organization membership capture,
    /// so a legacy authority cannot produce a frame here even in principle.
    #[test]
    fn the_local_org_planner_emits_only_the_org_variant_with_its_own_membership() {
        const TARGET: u64 = 0x77;
        let (relay_entity, plan) = local_egress("local-egress-emit");
        let spec = spec_with(plan.proven_root());
        let frame = plan_local_org_provider_registration(&plan, &spec, TARGET, D, TTL)
            .expect("a coherent own-org plan emits a frame");
        match frame {
            SensingInterestFrame::OrgProviderRegistration {
                target,
                subscriber_membership,
                audience_scope,
                ..
            } => {
                assert_eq!(target, TARGET);
                assert_eq!(
                    subscriber_membership.member, relay_entity,
                    "the frame carries THIS node's own live membership"
                );
                assert_eq!(
                    audience_scope,
                    canonical_org_sensing_commitment(&org_kp().org_id()),
                    "the audience is derived from installed authority"
                );
            }
            other => panic!("expected OrgProviderRegistration, got {other:?}"),
        }
    }

    /// The audience is DERIVED, not accepted: a caller-chosen audience that is
    /// not the captured organization's canonical commitment is refused before
    /// any frame is built.
    #[test]
    fn the_local_org_planner_refuses_an_audience_it_did_not_derive() {
        let (_, plan) = local_egress("local-egress-audience");
        // A legacy entity root, and a foreign organization's commitment.
        for audience in [
            AudienceScopeCommitment::owner_root(&member()),
            canonical_org_sensing_commitment(&OrgKeypair::from_bytes([0x99u8; 32]).org_id()),
        ] {
            let spec = spec_with(audience);
            assert_eq!(
                plan_local_org_provider_registration(&plan, &spec, 0x77, D, TTL).err(),
                Some(LocalOrgEgressRefusal::AudienceNotDerived),
                "only the derived audience may be spoken for"
            );
        }
    }

    /// The planner enforces the SAME exactness rule as the inbound gate, via
    /// the one shared predicate.
    #[test]
    fn the_local_org_planner_refuses_a_selector_that_does_not_name_the_target() {
        let (_, plan) = local_egress("local-egress-selector");
        let mut spec = spec_with(plan.proven_root());
        for selector in [
            ProviderSelector::Node(0x88),
            ProviderSelector::AnyAuthorized,
            ProviderSelector::Nodes(vec![0x77]),
        ] {
            spec.providers = selector;
            assert_eq!(
                plan_local_org_provider_registration(&plan, &spec, 0x77, D, TTL).err(),
                Some(LocalOrgEgressRefusal::SelectorTargetMismatch)
            );
        }
        // And the one shared predicate is the same rule the gate uses.
        let coherent = spec_with(plan.proven_root());
        assert!(selector_names_target(&coherent, 0x77));
        assert!(!selector_names_target(&coherent, 0x88));
    }

    /// A membership captured for ANOTHER organization cannot be obtained at all:
    /// the capture itself refuses, so no egress plan exists to speak for it.
    #[test]
    fn a_membership_for_another_org_yields_no_local_egress_plan() {
        let (relay_entity, authority, store) = adopt_relay("local-egress-foreign", 1);
        let outcome = capture_relay(
            Some(authority),
            Some(store),
            &relay_entity,
            foreign_org(),
            now_secs(),
        );
        // `LiveOrgRelayMembership` is deliberately not `Debug` (it is an
        // authority proof, not a printable value), so match rather than unwrap.
        match outcome {
            Err(RelayMembershipUnavailable::ForeignOrg) => {}
            Err(other) => panic!("expected ForeignOrg, got {other:?}"),
            Ok(_) => panic!("a capture for a foreign org must be refused"),
        }
    }

    /// The emitted local frame passes the REAL intake gate unchanged — the
    /// producer and the checker agree byte for byte, including the new
    /// selector/target step.
    #[test]
    fn an_emitted_local_org_lease_frame_passes_the_real_intake_gate() {
        const TARGET: u64 = 0x77;
        let (relay_entity, plan) = local_egress("local-egress-roundtrip");
        let spec = spec_with(plan.proven_root());
        let frame = plan_local_org_provider_registration(&plan, &spec, TARGET, D, TTL)
            .expect("frame planned");
        let validated = run(&frame, &relay_entity, Some(authority()), &empty_floors())
            .expect("the planner's own frame must be admitted by the intake gate");
        match &validated.0 {
            ValidatedInner::Provider {
                target, spec: got, ..
            } => {
                assert_eq!(*target, TARGET);
                assert_eq!(got, &spec, "the admitted spec equals the planned spec");
            }
            _ => panic!("expected a provider registration"),
        }
    }
}
