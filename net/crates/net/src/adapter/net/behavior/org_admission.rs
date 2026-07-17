//! OA-2 §2.4 of `docs/plans/ORG_CAPABILITY_AUTH_PLAN.md` — the
//! provider-local admission engine.
//!
//! Admission is provider-local, per-service, bound at registration,
//! and ALWAYS the last authority consulted (Locked #6). This module
//! is the ordered verification of one [`OrgCallProof`] against the
//! provider's own knowledge — its identity, its PROVEN owner org
//! (from its installed authority scaffold, never fold state), the
//! service being invoked, and the call it actually received.
//!
//! # The three modes
//!
//! - [`OrgAdmission::PublicAuthenticated`] — v0.4 behavior; NOT
//!   handled here (the §2.4a seam routes it through `may_execute`).
//! - [`OrgAdmission::OwnerDelegated`] — the caller acts for the
//!   provider's OWN owner org; membership + dispatcher grant, and
//!   NO cross-org capability grant (an unexpected one is malformed).
//! - [`OrgAdmission::CrossOrgGranted`] — the caller's org holds a
//!   capability grant the provider's owner issued; the grant's
//!   issuer must be my owner, its grantee the caller's org, its
//!   rights ⊇ INVOKE, its capability the invoked one, and its
//!   target must cover exactly me.
//!
//! # The ordered checks
//!
//! ```text
//! 1. mode is org-protected            (Public routes elsewhere)
//! 2. exactly one admission header     (0 or >1 → deny)
//! 3. proof decodes                    (malformed → deny)
//! 4. call is unary                    (streaming → distinct deny)
//! 5. TOFU member binding              (proof caller == channel peer)
//! 6. mode checks                      (owner/cross-org shape)
//! 7. dispatcher grant checks          (acts-for org, capability)
//! 8. credentials: signatures, windows, floors, proof freshness
//! 9. call binding                     (caller signed THIS call)
//! 10. replay guard                    (atomic insert-or-deny)
//! 11. provider-local policy           (LAST — application veto)
//! ```
//!
//! Every failure is a typed [`AdmissionDenied`] with a
//! distinguishable reason. Fold state, decrypted announcements, and
//! discovery responses are never admission evidence — the engine
//! reads only the proof and the provider's own facts. `may_execute`
//! is never touched.
//!
//! # Layering
//!
//! Behavior-layer, like [`org_call`](super::org_call): the "whole
//! canonical request minus the proof header" arrives as a
//! `request_digest`, and the admission header value(s) arrive
//! pre-extracted, so this module never imports the cortex RPC
//! types. The cortex/mesh gate (§2.4a, a later step) wires it in
//! and maps [`AdmissionDenied`] to `RpcStatus::AdmissionDenied`
//! (0x0009).

use std::time::Instant;

use super::org::OrgId;
use super::org_admission_replay::{AdmissionReplayGuard, ReplayOutcome};
use super::org_call::{OrgCallProof, MAX_ORG_CALL_PROOF_BYTES};
use super::org_grant::CapabilityAuthorityId;
use super::org_revocation::OrgRevocationState;
use crate::adapter::net::identity::EntityId;

/// The admission mode a provider registered for one capability
/// (Locked #6; the model's `OrgAdmission`). Bound at registration
/// (§2.4a); resolved BEFORE gate selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrgAdmission {
    /// v0.4 behavior — allow-list axes + transport auth only. NOT
    /// verified by this engine; the §2.4a seam routes it through
    /// `may_execute`.
    PublicAuthenticated,
    /// The caller acts for the provider's own owner org.
    OwnerDelegated,
    /// The caller's org holds a cross-org capability grant issued
    /// by the provider's owner org.
    CrossOrgGranted,
}

/// A distinguishable admission-denial reason (§2.4). The cortex
/// gate maps every variant to `RpcStatus::AdmissionDenied`
/// (0x0009) while preserving the reason for audit — a caller bug
/// (e.g. [`Self::CallIdCollision`]) must read differently from an
/// attack (e.g. [`Self::Replay`] or [`Self::BindingInvalid`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionDenied {
    /// The engine was invoked for a non-org-protected mode
    /// ([`OrgAdmission::PublicAuthenticated`]) — a caller logic
    /// error; the seam should have routed it elsewhere.
    NotOrgProtected,
    /// No `net-org-admission` header was present.
    MissingHeader,
    /// More than one `net-org-admission` header — exactly one or
    /// deny (§2.3 header discipline).
    MultipleHeaders,
    /// The proof header value did not decode (bad bytes, over cap).
    MalformedProof,
    /// A streaming (non-unary) call — org admission covers unary
    /// only in v1 (Locked #9); rejected with THIS distinct reason
    /// rather than admitted under a binding that covers only the
    /// initial payload.
    StreamingUnsupported,
    /// The proof's caller does not match the TOFU-authenticated
    /// channel peer — a relayed or transplanted proof.
    MemberBindingMismatch,
    /// The membership certificate's org and the dispatcher grant's
    /// org disagree — the proof does not name one coherent acting
    /// org.
    ActingOrgMismatch,
    /// `OwnerDelegated` carried a capability grant (malformed — the
    /// same-org mode confers no cross-org grant).
    UnexpectedCapabilityGrant,
    /// `CrossOrgGranted` carried no capability grant.
    MissingCapabilityGrant,
    /// The capability grant's issuer is not the provider's owner
    /// org (a signed receipt from a foreign org is not authority
    /// here).
    ForeignIssuer,
    /// The capability grant's grantee is not the caller's acting
    /// org.
    GranteeMismatch,
    /// The capability grant does not carry INVOKE.
    InsufficientRights,
    /// The capability grant is for a different capability than the
    /// one invoked.
    CapabilityMismatch,
    /// The grant's target scope does not cover this exact provider.
    TargetNotCovered,
    /// The dispatcher grant does not empower this caller to act for
    /// the acting org (wrong org, wrong subject, or capability out
    /// of scope).
    DispatcherGrantScope,
    /// The dispatcher grant failed signature/structure/window
    /// verification.
    DispatcherGrantInvalid,
    /// The membership certificate failed signature/structure/window
    /// verification.
    MembershipInvalid,
    /// A revocation floor for `(acting org, caller)` has risen to
    /// or above the membership certificate's generation — the cert
    /// is dead.
    MembershipRevoked,
    /// The capability grant failed signature/structure/window
    /// verification.
    CapabilityGrantInvalid,
    /// The proof's finite expiry has passed, or exceeds the TTL
    /// ceiling.
    ProofExpired,
    /// The call-binding signature does not verify against the
    /// caller over THIS exact call (call_id, callee, capability,
    /// provider org, request digest, credential digests).
    BindingInvalid,
    /// The same `(caller, call_id)` proof was already admitted —
    /// a replay.
    Replay,
    /// The same `(caller, call_id)` reused with a different binding
    /// — a correlation-id collision.
    CallIdCollision,
    /// The GLOBAL replay guard is at capacity — denied fail-closed.
    ReplayCapacity,
    /// THIS caller has filled its per-caller replay allocation
    /// (E1.5) — denied fail-closed, but only for this caller; other
    /// callers are unaffected.
    PerCallerReplayCapacity,
    /// The provider-local policy (application veto, run LAST)
    /// rejected the call.
    ProviderPolicyRejected,
}

impl std::fmt::Display for AdmissionDenied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "admission denied: {self:?}")
    }
}

impl std::error::Error for AdmissionDenied {}

/// The full four-party attribution of an admitted call (audit
/// identity, Locked #11): actor S, acting for org A, under a grant
/// from provider org B, invoking capability C on exact provider P.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Admitted {
    /// The acting entity S (the caller).
    pub caller: EntityId,
    /// The org A the caller acted for.
    pub acting_org: OrgId,
    /// The provider org B (this node's owner).
    pub provider_org: OrgId,
    /// The exact provider P (this node).
    pub provider: EntityId,
    /// The invoked capability C.
    pub capability: CapabilityAuthorityId,
}

/// The provider's own facts for one admission decision. Everything
/// here is the provider's knowledge — never the caller's claims and
/// never fold state.
pub struct AdmissionContext<'a> {
    /// The registered admission mode for the invoked capability.
    pub mode: OrgAdmission,
    /// The TOFU-authenticated identity of the channel peer — who is
    /// ACTUALLY on the wire (from `peer_entity_ids`), independent
    /// of what the proof claims.
    pub authenticated_caller: &'a EntityId,
    /// This provider's entity (P).
    pub provider: &'a EntityId,
    /// This provider's PROVEN owner org (from its installed
    /// authority scaffold — never fold state).
    pub provider_owner_org: OrgId,
    /// The authority id of the invoked service — the provider
    /// computes this from the service tag it is about to dispatch.
    pub invoked_capability: CapabilityAuthorityId,
    /// The nRPC correlation id of this call.
    pub call_id: u64,
    /// blake3 of the canonical request with the admission header
    /// removed (computed at the cortex layer).
    pub request_digest: [u8; 32],
    /// `true` iff this is a unary call; streaming is rejected.
    pub is_unary: bool,
    /// The provider's current revocation floor view.
    pub floors: &'a OrgRevocationState,
    /// Clock-skew tolerance for every wall-clock check.
    pub skew_secs: u64,
}

/// Verify one admission proof against `ctx` in the §2.4 order.
/// `admission_headers` is every value carried under
/// [`ORG_ADMISSION_HEADER`](super::org_call::ORG_ADMISSION_HEADER)
/// (exactly one is required). `replay` is the provider's replay
/// guard; `now` its monotonic clock. `provider_policy` is the
/// application veto, run LAST — it sees the verified proof and
/// returns `true` to admit.
///
/// Returns the four-party [`Admitted`] attribution on success, or a
/// distinguishable [`AdmissionDenied`] reason.
pub fn verify_org_admission(
    ctx: &AdmissionContext,
    admission_headers: &[&[u8]],
    replay: &AdmissionReplayGuard,
    now: Instant,
    provider_policy: impl FnOnce(&OrgCallProof) -> bool,
) -> Result<Admitted, AdmissionDenied> {
    // 1. Only org-protected modes reach the engine.
    match ctx.mode {
        OrgAdmission::OwnerDelegated | OrgAdmission::CrossOrgGranted => {}
        OrgAdmission::PublicAuthenticated => return Err(AdmissionDenied::NotOrgProtected),
    }

    // 2. Exactly one admission header, or deny.
    let header = match admission_headers {
        [] => return Err(AdmissionDenied::MissingHeader),
        [one] => *one,
        _ => return Err(AdmissionDenied::MultipleHeaders),
    };
    if header.len() > MAX_ORG_CALL_PROOF_BYTES {
        return Err(AdmissionDenied::MalformedProof);
    }

    // 3. Decode the proof.
    let proof = OrgCallProof::decode(header).map_err(|_| AdmissionDenied::MalformedProof)?;

    // 4. Unary only (Locked #9). A distinct reason so a caller can
    //    tell "not supported" from "rejected".
    if !ctx.is_unary {
        return Err(AdmissionDenied::StreamingUnsupported);
    }

    // 5. TOFU member binding: the proof's caller must be who is
    //    actually on the channel — a captured proof replayed by a
    //    different peer fails here before any signature work.
    if &proof.caller_membership.member != ctx.authenticated_caller {
        return Err(AdmissionDenied::MemberBindingMismatch);
    }

    // The acting org is named by the membership; the dispatcher
    // grant must agree.
    let acting_org = proof.caller_membership.org_id;
    if proof.dispatcher_grant.org_id != acting_org {
        return Err(AdmissionDenied::ActingOrgMismatch);
    }

    // 6. Mode checks.
    match ctx.mode {
        OrgAdmission::OwnerDelegated => {
            // Same-org: the caller acts for MY owner, and there is
            // no cross-org capability grant.
            if acting_org != ctx.provider_owner_org {
                return Err(AdmissionDenied::GranteeMismatch);
            }
            if proof.capability_grant.is_some() {
                return Err(AdmissionDenied::UnexpectedCapabilityGrant);
            }
        }
        OrgAdmission::CrossOrgGranted => {
            let grant = proof
                .capability_grant
                .as_ref()
                .ok_or(AdmissionDenied::MissingCapabilityGrant)?;
            // The grant is authority ONLY if my owner issued it…
            if grant.issuer_org != ctx.provider_owner_org {
                return Err(AdmissionDenied::ForeignIssuer);
            }
            // …to the caller's acting org…
            if grant.grantee_org != acting_org {
                return Err(AdmissionDenied::GranteeMismatch);
            }
            // …carrying INVOKE…
            if !grant.permits_invoke() {
                return Err(AdmissionDenied::InsufficientRights);
            }
            // …for the invoked capability…
            if grant.capability != ctx.invoked_capability {
                return Err(AdmissionDenied::CapabilityMismatch);
            }
            // …covering exactly me (owned by my owner org).
            if !grant
                .target_scope
                .covers(ctx.provider, Some(&ctx.provider_owner_org))
            {
                return Err(AdmissionDenied::TargetNotCovered);
            }
        }
        OrgAdmission::PublicAuthenticated => unreachable!("filtered in step 1"),
    }

    // 7. Dispatcher grant scope: it must empower THIS caller to act
    //    for the acting org over the invoked capability.
    if proof.dispatcher_grant.dispatcher != *ctx.authenticated_caller
        || !proof
            .dispatcher_grant
            .covers_capability(&ctx.invoked_capability)
    {
        return Err(AdmissionDenied::DispatcherGrantScope);
    }

    // 8. Credentials: signatures + windows + floors + freshness.
    //    Membership first (belonging), then revocation floor, then
    //    the grants, then proof expiry.
    proof
        .caller_membership
        .is_valid_with_skew(ctx.skew_secs)
        .map_err(|_| AdmissionDenied::MembershipInvalid)?;
    let floor = ctx
        .floors
        .floor_for(&acting_org, &proof.caller_membership.member);
    if proof.caller_membership.generation < floor {
        return Err(AdmissionDenied::MembershipRevoked);
    }
    proof
        .dispatcher_grant
        .is_valid_with_skew(ctx.skew_secs)
        .map_err(|_| AdmissionDenied::DispatcherGrantInvalid)?;
    if let Some(grant) = &proof.capability_grant {
        grant
            .is_valid_with_skew(ctx.skew_secs)
            .map_err(|_| AdmissionDenied::CapabilityGrantInvalid)?;
    }
    proof
        .check_expiry(ctx.skew_secs)
        .map_err(|_| AdmissionDenied::ProofExpired)?;

    // 9. Call binding: the caller ENTITY signed THIS exact call.
    //    The provider supplies its own owner org, identity,
    //    call_id, the invoked capability, and the request digest —
    //    a proof minted for another call/callee/capability fails.
    let binding = proof.binding_for_verify(
        ctx.provider_owner_org,
        ctx.provider.clone(),
        ctx.call_id,
        ctx.invoked_capability,
        ctx.request_digest,
    );
    binding
        .verify(&proof.call_binding_sig)
        .map_err(|_| AdmissionDenied::BindingInvalid)?;

    // 10. Replay guard: atomic insert-or-deny BEFORE the handler.
    //     Keyed on (caller, call_id); the binding signature
    //     distinguishes replay from call-id collision.
    let binding_digest: [u8; 32] = blake3::hash(&proof.call_binding_sig).into();
    let expires_at = replay_deadline(now, proof.proof_expires_at_unix_ns, ctx.skew_secs);
    match replay.admit(
        ctx.authenticated_caller,
        ctx.call_id,
        binding_digest,
        expires_at,
        now,
    ) {
        ReplayOutcome::Admitted => {}
        ReplayOutcome::Replay => return Err(AdmissionDenied::Replay),
        ReplayOutcome::CallIdCollision => return Err(AdmissionDenied::CallIdCollision),
        ReplayOutcome::CapacityExhausted => return Err(AdmissionDenied::ReplayCapacity),
        ReplayOutcome::PerCallerCapacityExhausted => {
            return Err(AdmissionDenied::PerCallerReplayCapacity)
        }
    }

    // 11. Provider-local policy LAST (Locked #6): the application
    //     veto. Fold state / decrypted announcements are never
    //     consulted here — the closure gets only the verified proof.
    if !provider_policy(&proof) {
        return Err(AdmissionDenied::ProviderPolicyRejected);
    }

    Ok(Admitted {
        caller: proof.caller_membership.member.clone(),
        acting_org,
        provider_org: ctx.provider_owner_org,
        provider: ctx.provider.clone(),
        capability: ctx.invoked_capability,
    })
}

/// The replay-guard deadline for a proof: the proof's wall-clock
/// expiry (plus skew) translated onto the monotonic clock. A proof
/// already past expiry (which step 8 would have rejected) yields
/// `now`, so it is never retained beyond its own life.
fn replay_deadline(now: Instant, proof_expires_at_unix_ns: u64, skew_secs: u64) -> Instant {
    let now_ns = super::org::current_timestamp().saturating_mul(1_000_000_000);
    let skew_ns = skew_secs.saturating_mul(1_000_000_000);
    let remaining_ns = proof_expires_at_unix_ns
        .saturating_add(skew_ns)
        .saturating_sub(now_ns);
    now + std::time::Duration::from_nanos(remaining_ns)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::org::{OrgKeypair, OrgMembershipCert, OrgRevocationBundle};
    use crate::adapter::net::behavior::org_grant::{
        DispatcherScope, GrantRights, GrantTargetScope, OrgCapabilityGrant, OrgDispatcherGrant,
    };
    use crate::adapter::net::identity::EntityKeypair;
    use std::collections::BTreeMap;

    fn org_b() -> OrgKeypair {
        // Provider's owner org.
        OrgKeypair::from_bytes([0x42u8; 32])
    }

    fn org_a() -> OrgKeypair {
        // Caller's org (cross-org grantee).
        OrgKeypair::from_bytes([0x77u8; 32])
    }

    fn caller() -> EntityKeypair {
        EntityKeypair::from_bytes([0x24u8; 32])
    }

    fn provider() -> EntityId {
        EntityId::from_bytes([0x99u8; 32])
    }

    fn cap() -> CapabilityAuthorityId {
        CapabilityAuthorityId::for_tag("nrpc:oa2-echo")
    }

    fn empty_floors() -> OrgRevocationState {
        OrgRevocationState::empty()
    }

    const REQ: [u8; 32] = [0x11u8; 32];
    const CALL_ID: u64 = 42;

    /// Build a valid cross-org proof (caller ∈ A, dispatcher A→caller,
    /// capability grant B→A INVOKE covering the exact provider).
    fn cross_org_proof() -> OrgCallProof {
        let caller = caller();
        let membership =
            OrgMembershipCert::try_issue(&org_a(), caller.entity_id().clone(), 1, 3600)
                .expect("cert");
        let dispatcher = OrgDispatcherGrant::try_issue(
            &org_a(),
            caller.entity_id().clone(),
            DispatcherScope::Exact(cap()),
            3600,
        )
        .expect("dispatcher");
        let (grant, _) = OrgCapabilityGrant::try_issue(
            &org_b(),
            org_a().org_id(),
            cap(),
            GrantRights::INVOKE,
            GrantTargetScope::ExactNode(provider()),
            3600,
        )
        .expect("grant");
        let expiry = (crate::adapter::net::behavior::org::current_timestamp() + 20) * 1_000_000_000;
        OrgCallProof::sign_for_call(
            &caller,
            membership,
            dispatcher,
            Some(grant),
            org_a().org_id(),
            org_b().org_id(),
            provider(),
            CALL_ID,
            cap(),
            expiry,
            REQ,
        )
    }

    /// Build a valid owner-delegated proof (caller ∈ B = my owner,
    /// dispatcher B→caller, NO capability grant).
    fn owner_delegated_proof() -> OrgCallProof {
        let caller = caller();
        let membership =
            OrgMembershipCert::try_issue(&org_b(), caller.entity_id().clone(), 1, 3600)
                .expect("cert");
        let dispatcher = OrgDispatcherGrant::try_issue(
            &org_b(),
            caller.entity_id().clone(),
            DispatcherScope::Exact(cap()),
            3600,
        )
        .expect("dispatcher");
        let expiry = (crate::adapter::net::behavior::org::current_timestamp() + 20) * 1_000_000_000;
        OrgCallProof::sign_for_call(
            &caller,
            membership,
            dispatcher,
            None,
            org_b().org_id(),
            org_b().org_id(),
            provider(),
            CALL_ID,
            cap(),
            expiry,
            REQ,
        )
    }

    fn cross_org_ctx(floors: &OrgRevocationState) -> AdmissionContext<'_> {
        AdmissionContext {
            mode: OrgAdmission::CrossOrgGranted,
            authenticated_caller: Box::leak(Box::new(caller().entity_id().clone())),
            provider: Box::leak(Box::new(provider())),
            provider_owner_org: org_b().org_id(),
            invoked_capability: cap(),
            call_id: CALL_ID,
            request_digest: REQ,
            is_unary: true,
            floors,
            skew_secs: 0,
        }
    }

    fn owner_ctx(floors: &OrgRevocationState) -> AdmissionContext<'_> {
        AdmissionContext {
            mode: OrgAdmission::OwnerDelegated,
            authenticated_caller: Box::leak(Box::new(caller().entity_id().clone())),
            provider: Box::leak(Box::new(provider())),
            provider_owner_org: org_b().org_id(),
            invoked_capability: cap(),
            call_id: CALL_ID,
            request_digest: REQ,
            is_unary: true,
            floors,
            skew_secs: 0,
        }
    }

    fn admit(
        ctx: &AdmissionContext,
        proof: &OrgCallProof,
        replay: &AdmissionReplayGuard,
    ) -> Result<Admitted, AdmissionDenied> {
        let bytes = proof.encode().expect("encode");
        verify_org_admission(ctx, &[&bytes], replay, Instant::now(), |_| true)
    }

    #[test]
    fn cross_org_happy_path_admits_with_four_party_attribution() {
        let floors = empty_floors();
        let ctx = cross_org_ctx(&floors);
        let replay = AdmissionReplayGuard::with_defaults();
        let admitted = admit(&ctx, &cross_org_proof(), &replay).expect("admit");
        assert_eq!(admitted.caller, *caller().entity_id());
        assert_eq!(admitted.acting_org, org_a().org_id());
        assert_eq!(admitted.provider_org, org_b().org_id());
        assert_eq!(admitted.provider, provider());
        assert_eq!(admitted.capability, cap());
    }

    #[test]
    fn owner_delegated_happy_path_admits() {
        let floors = empty_floors();
        let ctx = owner_ctx(&floors);
        let replay = AdmissionReplayGuard::with_defaults();
        let admitted = admit(&ctx, &owner_delegated_proof(), &replay).expect("admit");
        assert_eq!(admitted.acting_org, org_b().org_id());
        assert!(admitted.provider_org == admitted.acting_org, "same-org");
    }

    #[test]
    fn public_mode_is_not_handled_here() {
        let floors = empty_floors();
        let mut ctx = cross_org_ctx(&floors);
        ctx.mode = OrgAdmission::PublicAuthenticated;
        let replay = AdmissionReplayGuard::with_defaults();
        assert_eq!(
            admit(&ctx, &cross_org_proof(), &replay),
            Err(AdmissionDenied::NotOrgProtected)
        );
    }

    #[test]
    fn header_discipline_exactly_one() {
        let floors = empty_floors();
        let ctx = cross_org_ctx(&floors);
        let replay = AdmissionReplayGuard::with_defaults();
        let bytes = cross_org_proof().encode().expect("encode");
        assert_eq!(
            verify_org_admission(&ctx, &[], &replay, Instant::now(), |_| true),
            Err(AdmissionDenied::MissingHeader)
        );
        assert_eq!(
            verify_org_admission(&ctx, &[&bytes, &bytes], &replay, Instant::now(), |_| true),
            Err(AdmissionDenied::MultipleHeaders)
        );
    }

    #[test]
    fn malformed_and_streaming_are_distinct() {
        let floors = empty_floors();
        let ctx = cross_org_ctx(&floors);
        let replay = AdmissionReplayGuard::with_defaults();
        assert_eq!(
            verify_org_admission(&ctx, &[b"garbage"], &replay, Instant::now(), |_| true),
            Err(AdmissionDenied::MalformedProof)
        );

        let mut streaming = cross_org_ctx(&floors);
        streaming.is_unary = false;
        assert_eq!(
            admit(&streaming, &cross_org_proof(), &replay),
            Err(AdmissionDenied::StreamingUnsupported)
        );
    }

    #[test]
    fn tofu_member_binding_rejects_a_relayed_proof() {
        let floors = empty_floors();
        let mut ctx = cross_org_ctx(&floors);
        // A different peer on the channel than the proof's member.
        let other = EntityId::from_bytes([0xEEu8; 32]);
        ctx.authenticated_caller = &other;
        let replay = AdmissionReplayGuard::with_defaults();
        assert_eq!(
            admit(&ctx, &cross_org_proof(), &replay),
            Err(AdmissionDenied::MemberBindingMismatch)
        );
    }

    #[test]
    fn owner_delegated_rejects_an_unexpected_capability_grant() {
        // A proof shaped for cross-org (carries a grant) presented
        // under OwnerDelegated: the caller is in B, so build a B
        // membership but attach a capability grant.
        let caller = caller();
        let membership =
            OrgMembershipCert::try_issue(&org_b(), caller.entity_id().clone(), 1, 3600)
                .expect("cert");
        let dispatcher = OrgDispatcherGrant::try_issue(
            &org_b(),
            caller.entity_id().clone(),
            DispatcherScope::Exact(cap()),
            3600,
        )
        .expect("dispatcher");
        let (grant, _) = OrgCapabilityGrant::try_issue(
            &org_b(),
            org_b().org_id(),
            cap(),
            GrantRights::INVOKE,
            GrantTargetScope::ExactNode(provider()),
            3600,
        )
        .expect("grant");
        let expiry = (crate::adapter::net::behavior::org::current_timestamp() + 20) * 1_000_000_000;
        let proof = OrgCallProof::sign_for_call(
            &caller,
            membership,
            dispatcher,
            Some(grant),
            org_b().org_id(),
            org_b().org_id(),
            provider(),
            CALL_ID,
            cap(),
            expiry,
            REQ,
        );
        let floors = empty_floors();
        let ctx = owner_ctx(&floors);
        let replay = AdmissionReplayGuard::with_defaults();
        assert_eq!(
            admit(&ctx, &proof, &replay),
            Err(AdmissionDenied::UnexpectedCapabilityGrant)
        );
    }

    #[test]
    fn cross_org_mode_check_matrix() {
        let floors = empty_floors();
        let replay = AdmissionReplayGuard::with_defaults();

        // Missing capability grant.
        let owner_proof = owner_delegated_proof();
        assert_eq!(
            admit(&cross_org_ctx(&floors), &owner_proof, &replay),
            Err(AdmissionDenied::MissingCapabilityGrant)
        );

        // Foreign issuer: grant issued by A (not my owner B).
        let caller = caller();
        let membership =
            OrgMembershipCert::try_issue(&org_a(), caller.entity_id().clone(), 1, 3600)
                .expect("cert");
        let dispatcher = OrgDispatcherGrant::try_issue(
            &org_a(),
            caller.entity_id().clone(),
            DispatcherScope::Exact(cap()),
            3600,
        )
        .expect("dispatcher");
        let (foreign_grant, _) = OrgCapabilityGrant::try_issue(
            &org_a(), // WRONG issuer
            org_a().org_id(),
            cap(),
            GrantRights::INVOKE,
            GrantTargetScope::ExactNode(provider()),
            3600,
        )
        .expect("grant");
        let expiry = (crate::adapter::net::behavior::org::current_timestamp() + 20) * 1_000_000_000;
        let foreign = OrgCallProof::sign_for_call(
            &caller,
            membership.clone(),
            dispatcher.clone(),
            Some(foreign_grant),
            org_a().org_id(),
            org_b().org_id(),
            provider(),
            CALL_ID,
            cap(),
            expiry,
            REQ,
        );
        assert_eq!(
            admit(&cross_org_ctx(&floors), &foreign, &replay),
            Err(AdmissionDenied::ForeignIssuer)
        );

        // Wrong target: grant covers a DIFFERENT provider.
        let (wrong_target, _) = OrgCapabilityGrant::try_issue(
            &org_b(),
            org_a().org_id(),
            cap(),
            GrantRights::INVOKE,
            GrantTargetScope::ExactNode(EntityId::from_bytes([0x55u8; 32])),
            3600,
        )
        .expect("grant");
        let mistargeted = OrgCallProof::sign_for_call(
            &caller,
            membership.clone(),
            dispatcher.clone(),
            Some(wrong_target),
            org_a().org_id(),
            org_b().org_id(),
            provider(),
            CALL_ID,
            cap(),
            expiry,
            REQ,
        );
        assert_eq!(
            admit(&cross_org_ctx(&floors), &mistargeted, &replay),
            Err(AdmissionDenied::TargetNotCovered)
        );

        // DISCOVER-only grant: no INVOKE.
        let (discover_only, _) = OrgCapabilityGrant::try_issue(
            &org_b(),
            org_a().org_id(),
            cap(),
            GrantRights::DISCOVER,
            GrantTargetScope::ExactNode(provider()),
            3600,
        )
        .expect("grant");
        let no_invoke = OrgCallProof::sign_for_call(
            &caller,
            membership,
            dispatcher,
            Some(discover_only),
            org_a().org_id(),
            org_b().org_id(),
            provider(),
            CALL_ID,
            cap(),
            expiry,
            REQ,
        );
        assert_eq!(
            admit(&cross_org_ctx(&floors), &no_invoke, &replay),
            Err(AdmissionDenied::InsufficientRights)
        );
    }

    #[test]
    fn capability_mismatch_when_invoked_tag_differs() {
        let floors = empty_floors();
        // The proof is bound to cap(); the provider is dispatching a
        // DIFFERENT service.
        let mut ctx = cross_org_ctx(&floors);
        ctx.invoked_capability = CapabilityAuthorityId::for_tag("nrpc:other");
        let replay = AdmissionReplayGuard::with_defaults();
        // The grant's capability no longer matches the invoked one.
        assert_eq!(
            admit(&ctx, &cross_org_proof(), &replay),
            Err(AdmissionDenied::CapabilityMismatch)
        );
    }

    #[test]
    fn revocation_floor_kills_the_membership() {
        // Floor for (A, caller) rises to 2; the cert is generation 1.
        let mut floors = OrgRevocationState::empty();
        let mut map = BTreeMap::new();
        map.insert(caller().entity_id().clone(), 2u32);
        floors.merge_bundle(&OrgRevocationBundle::try_issue(&org_a(), &map).expect("bundle"));
        let ctx = cross_org_ctx(&floors);
        let replay = AdmissionReplayGuard::with_defaults();
        assert_eq!(
            admit(&ctx, &cross_org_proof(), &replay),
            Err(AdmissionDenied::MembershipRevoked)
        );
    }

    #[test]
    fn binding_rejects_a_transplanted_call() {
        let floors = empty_floors();
        let replay = AdmissionReplayGuard::with_defaults();
        // The provider received the proof under a DIFFERENT call_id
        // than the caller signed.
        let mut ctx = cross_org_ctx(&floors);
        ctx.call_id = CALL_ID + 1;
        assert_eq!(
            admit(&ctx, &cross_org_proof(), &replay),
            Err(AdmissionDenied::BindingInvalid)
        );
        // …or a different request body (digest).
        let mut ctx = cross_org_ctx(&floors);
        ctx.request_digest = [0x22u8; 32];
        assert_eq!(
            admit(&ctx, &cross_org_proof(), &replay),
            Err(AdmissionDenied::BindingInvalid)
        );
    }

    #[test]
    fn replay_then_collision_are_distinguished() {
        let floors = empty_floors();
        let ctx = cross_org_ctx(&floors);
        let replay = AdmissionReplayGuard::with_defaults();
        let proof = cross_org_proof();
        admit(&ctx, &proof, &replay).expect("first admit");
        // Same proof again → replay.
        assert_eq!(admit(&ctx, &proof, &replay), Err(AdmissionDenied::Replay));
        // Same (caller, call_id), different binding (new proof, fresh
        // nonce in the grant/cert → different sig) → collision.
        let other = cross_org_proof();
        assert_eq!(
            admit(&ctx, &other, &replay),
            Err(AdmissionDenied::CallIdCollision)
        );
    }

    #[test]
    fn provider_policy_runs_last_and_can_veto() {
        let floors = empty_floors();
        let ctx = cross_org_ctx(&floors);
        let replay = AdmissionReplayGuard::with_defaults();
        let bytes = cross_org_proof().encode().expect("encode");
        // Everything verifies, but the application vetoes.
        let out = verify_org_admission(&ctx, &[&bytes], &replay, Instant::now(), |_| false);
        assert_eq!(out, Err(AdmissionDenied::ProviderPolicyRejected));
    }

    #[test]
    fn expired_proof_is_refused() {
        let caller = caller();
        let membership =
            OrgMembershipCert::try_issue(&org_a(), caller.entity_id().clone(), 1, 3600)
                .expect("cert");
        let dispatcher = OrgDispatcherGrant::try_issue(
            &org_a(),
            caller.entity_id().clone(),
            DispatcherScope::Exact(cap()),
            3600,
        )
        .expect("dispatcher");
        let (grant, _) = OrgCapabilityGrant::try_issue(
            &org_b(),
            org_a().org_id(),
            cap(),
            GrantRights::INVOKE,
            GrantTargetScope::ExactNode(provider()),
            3600,
        )
        .expect("grant");
        // Expiry already in the past.
        let past = (crate::adapter::net::behavior::org::current_timestamp().saturating_sub(10))
            * 1_000_000_000;
        let proof = OrgCallProof::sign_for_call(
            &caller,
            membership,
            dispatcher,
            Some(grant),
            org_a().org_id(),
            org_b().org_id(),
            provider(),
            CALL_ID,
            cap(),
            past,
            REQ,
        );
        let floors = empty_floors();
        let ctx = cross_org_ctx(&floors);
        let replay = AdmissionReplayGuard::with_defaults();
        assert_eq!(
            admit(&ctx, &proof, &replay),
            Err(AdmissionDenied::ProofExpired)
        );
    }
}
