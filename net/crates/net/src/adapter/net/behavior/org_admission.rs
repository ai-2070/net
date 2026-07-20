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

use super::admission_clock::ClockSample;
use super::org::OrgId;
use super::org_admission_replay::{AdmissionReplayGuard, ReplayOutcome};
use super::org_call::{OrgCallProof, MAX_ORG_CALL_PROOF_BYTES};
use super::org_grant::CapabilityAuthorityId;
use super::org_revocation::OrgRevocationState;
use crate::adapter::net::identity::{EntityId, MAX_TOKEN_CLOCK_SKEW_SECS};

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
    /// A revocation floor for `(acting org, caller)` has risen ABOVE
    /// the membership certificate's generation — the cert is dead.
    ///
    /// The boundary is `generation < floor`, so a cert issued AT the
    /// floor is still alive; the floor names the lowest generation that
    /// remains valid, not the first one revoked. (An earlier revision of
    /// this doc said "to or above", which would have led an operator to
    /// issue a floor EQUAL to the generation they meant to retire and
    /// get no error — the credential stays live. `org.rs` and
    /// `org_authority.rs` state the rule correctly; §D3.)
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
    /// The provider cannot admit an org-protected call right now: no
    /// node authority / revocation store is installed, the store is
    /// poisoned, or the provider's own owner certificate fails its
    /// call-time self-verification (expired, or its generation fell
    /// below a floor). Registration-time authority is NOT usable
    /// authority — an expired/revoked/unhealthy provider stays dark
    /// (E1.3, verdict §5).
    ProviderAuthorityUnavailable,
    /// The provider's security view changed BETWEEN verification and
    /// the replay insert (E1.4 §9.5) — a revocation floor rose, the
    /// installed authority was replaced, or the active store was
    /// poisoned mid-admission. The stale decision is denied WITHOUT
    /// consuming a replay slot; the gate may retry from a fresh view.
    AuthorityChanged,
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

/// The COARSE, wire-stable admission-denial reason (E2.2). The DETAILED
/// [`AdmissionDenied`] variant stays PROVIDER-SIDE audit only — surfacing it on
/// the wire would make denial a credential oracle (which check failed) and could
/// leak the provider's authority / replay state. A caller sees only one of three
/// buckets, enough to decide retry behavior without learning why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoarseAdmissionReason {
    /// Rejected on the merits — a credential, binding, replay, or provider-
    /// policy failure. Retrying the SAME proof will not succeed.
    Denied,
    /// The provider does not support this call shape (a streaming frame on a
    /// protected unary service). Not retryable as-is.
    NotSupported,
    /// The provider cannot admit right now — its own authority is unavailable,
    /// its security view changed mid-admission, or a replay allocation is full.
    /// A transient state; a later retry may succeed.
    Unavailable,
}

impl CoarseAdmissionReason {
    /// The stable wire byte for this coarse reason.
    pub fn to_wire(self) -> u8 {
        match self {
            Self::Denied => 0,
            Self::NotSupported => 1,
            Self::Unavailable => 2,
        }
    }

    /// Decode a coarse reason from its wire byte (`None` on an unknown byte).
    pub fn from_wire(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Denied),
            1 => Some(Self::NotSupported),
            2 => Some(Self::Unavailable),
            _ => None,
        }
    }
}

impl AdmissionDenied {
    /// Map this detailed reason to the COARSE wire reason (E2.2). The match is
    /// EXHAUSTIVE (no wildcard) BY DESIGN: a newly added [`AdmissionDenied`]
    /// variant forces a compile error here, so it can never silently fall into a
    /// default bucket and escape the caller-facing classification.
    pub fn coarse(self) -> CoarseAdmissionReason {
        use AdmissionDenied as D;
        use CoarseAdmissionReason as C;
        match self {
            // The provider cannot admit right now — transient / retryable.
            D::ProviderAuthorityUnavailable
            | D::AuthorityChanged
            | D::ReplayCapacity
            | D::PerCallerReplayCapacity => C::Unavailable,
            // The call shape is unsupported on a protected unary service.
            D::StreamingUnsupported => C::NotSupported,
            // Everything else is a denial on the merits.
            D::NotOrgProtected
            | D::MissingHeader
            | D::MultipleHeaders
            | D::MalformedProof
            | D::MemberBindingMismatch
            | D::ActingOrgMismatch
            | D::UnexpectedCapabilityGrant
            | D::MissingCapabilityGrant
            | D::ForeignIssuer
            | D::GranteeMismatch
            | D::InsufficientRights
            | D::CapabilityMismatch
            | D::TargetNotCovered
            | D::DispatcherGrantScope
            | D::DispatcherGrantInvalid
            | D::MembershipInvalid
            | D::MembershipRevoked
            | D::CapabilityGrantInvalid
            | D::ProofExpired
            | D::BindingInvalid
            | D::Replay
            | D::CallIdCollision
            | D::ProviderPolicyRejected => C::Denied,
        }
    }
}

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
/// guard. `clock` is ONE paired wall+monotonic sample for the whole
/// admission (Kyra E1 audit): every credential/proof freshness check
/// reads `clock.wall_ns` and the replay retention derives from the
/// SAME sample's monotonic instant, so no `current_timestamp()` is
/// read inside a single admission and a wall-clock jump cannot make
/// checks disagree.
///
/// `stability_recheck` is the §9.5 linearization hook (E1.4): it runs
/// AFTER all credential/binding verification but BEFORE the replay
/// insert, and returns `true` iff the provider's security view (the
/// floor snapshot + installed authority + store health captured by
/// the gate before verification) is STILL current. A `false` return
/// — a floor raised, the authority was swapped, or the store was
/// poisoned mid-admission — denies [`AdmissionDenied::AuthorityChanged`]
/// WITHOUT consuming a `(caller, call_id)` replay slot, so a stale
/// decision can neither run the handler nor burn the correlation id;
/// the gate is free to retry from a fresh view.
///
/// `provider_policy` is the application veto, run LAST — it sees the
/// verified proof and returns `true` to admit.
///
/// Returns the four-party [`Admitted`] attribution on success, or a
/// distinguishable [`AdmissionDenied`] reason.
pub fn verify_org_admission(
    ctx: &AdmissionContext,
    admission_headers: &[&[u8]],
    replay: &AdmissionReplayGuard,
    clock: ClockSample,
    stability_recheck: impl FnOnce() -> bool,
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
    //    the grants, then proof expiry. EVERY wall-clock check reads
    //    the ONE captured `clock` sample (E1.4/E0.4, Kyra E1 audit) —
    //    never a freshly-sampled `current_timestamp()` — so a
    //    wall-clock jump mid-admission cannot make one check disagree
    //    with another or with the replay retention below.
    let now_secs = clock.wall_secs();
    proof
        .caller_membership
        .is_valid_at_with_skew(now_secs, ctx.skew_secs)
        .map_err(|_| AdmissionDenied::MembershipInvalid)?;
    let floor = ctx
        .floors
        .floor_for(&acting_org, &proof.caller_membership.member);
    if proof.caller_membership.generation < floor {
        return Err(AdmissionDenied::MembershipRevoked);
    }
    proof
        .dispatcher_grant
        .is_valid_at_with_skew(now_secs, ctx.skew_secs)
        .map_err(|_| AdmissionDenied::DispatcherGrantInvalid)?;
    if let Some(grant) = &proof.capability_grant {
        grant
            .is_valid_at_with_skew(now_secs, ctx.skew_secs)
            .map_err(|_| AdmissionDenied::CapabilityGrantInvalid)?;
    }
    proof
        .check_expiry_at(clock.wall_ns, ctx.skew_secs)
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

    // 9.5. Stability linearization (E1.4, verdict §6). Steps 1–9
    //      verified the proof against a floor snapshot + authority
    //      captured by the gate BEFORE this call. A floor raise, an
    //      authority swap, or a store poison DURING verification
    //      would make that view stale. Recheck it HERE — before the
    //      replay insert — so a stale decision neither runs the
    //      handler nor consumes the `(caller, call_id)` slot. On a
    //      changed view the gate retries from a fresh snapshot; a
    //      persistent change denies `AuthorityChanged`.
    if !stability_recheck() {
        return Err(AdmissionDenied::AuthorityChanged);
    }

    // 10. Replay guard: atomic insert-or-deny BEFORE the handler.
    //     Keyed on (caller, call_id); the binding signature
    //     distinguishes replay from call-id collision.
    //
    //     Retention derives from the SAME `clock` sample (Kyra E1
    //     audit): the wall deadline is the proof's expiry PLUS a skew
    //     allowance (a proof admitted within skew is still live, so it
    //     must be retained that far), translated onto the sample's
    //     monotonic instant. Using `clock.monotonic` as `now` keeps
    //     insertion and expiry on one monotonic timeline — a wall-clock
    //     jump cannot evict a just-admitted proof.
    //
    //     §5 — that allowance is the HARD CEILING, not `ctx.skew_secs`.
    //     Retention must dominate every acceptance window the freshness
    //     check could ever apply, and freshness re-reads the LIVE skew
    //     on every call (`facts.skew_secs`, resolved from the installed
    //     authority in `verify_provider_authority`). Retaining to
    //     `expiry + ctx.skew_secs` ties the two to the same mutable
    //     value read at different TIMES, which is not the same thing as
    //     tying them together:
    //
    //       P runs at skew 0 (the serde default). Caller S issues a
    //       protected call at T, proof expiry T+30; the guard entry is
    //       retained to monotonic M+30. S keeps the frame and re-sends
    //       it periodically — all denied ProofExpired. An operator then
    //       runs `net node adopt --skew-secs 300` after a clock-drift
    //       incident and calls install_node_authority: a supported
    //       same-org renewal, runtime-installable, no restart, guard not
    //       cleared. S's next resend at T+200 finds an EXPIRED guard
    //       entry (so `admit` takes the reusable-key branch and returns
    //       Admitted), passes freshness (T+200 < T+30+300), passes the
    //       TTL ceiling, and passes every credential check — the grants
    //       run days-to-weeks and nothing else changed. The handler runs
    //       a SECOND time on one signed proof. The fold's duplicate
    //       -REQUEST guard covers only in-flight calls, so the first
    //       call having COMPLETED is the enabling condition.
    //
    //     The same shape has a second, non-attacker-controlled trigger:
    //     retention is anchored on `Instant` while freshness reads wall
    //     time, so a backward wall step (NTP correcting a fast clock)
    //     makes monotonic elapse more than wall and expires the entry
    //     while the proof is still fresh. `admission_clock` closes the
    //     INTRA-admission case; this closes the inter-admission one.
    //
    //     `MAX_TOKEN_CLOCK_SKEW_SECS` is the ceiling `check_expiry_at`
    //     enforces on `skew_secs` (org_call.rs), so no future skew can
    //     produce an acceptance window wider than this retention. The
    //     cost is bounded: entries live at most 5 minutes past expiry
    //     rather than `ctx.skew_secs`, and the per-caller ceiling
    //     already bounds how many a caller can hold.
    let binding_digest: [u8; 32] = blake3::hash(&proof.call_binding_sig).into();
    let skew_ns = MAX_TOKEN_CLOCK_SKEW_SECS.saturating_mul(1_000_000_000);
    let retain_until_wall_ns = proof.proof_expires_at_unix_ns.saturating_add(skew_ns);
    let expires_at = clock.monotonic_deadline_for(retain_until_wall_ns);
    match replay.admit(
        ctx.authenticated_caller,
        ctx.call_id,
        binding_digest,
        expires_at,
        clock.monotonic,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::org::{OrgKeypair, OrgMembershipCert, OrgRevocationBundle};
    use crate::adapter::net::behavior::org_grant::{
        DispatcherScope, GrantRights, GrantTargetScope, OrgCapabilityGrant, OrgDispatcherGrant,
    };
    use crate::adapter::net::identity::EntityKeypair;
    use std::collections::BTreeMap;
    use std::time::Duration;

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
        cross_org_proof_for_call(CALL_ID)
    }

    /// [`cross_org_proof`] bound to an explicit `call_id`, so a test can put
    /// two independent calls on one replay guard.
    fn cross_org_proof_for_call(call_id: u64) -> OrgCallProof {
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
            call_id,
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
        verify_org_admission(
            ctx,
            &[&bytes],
            replay,
            ClockSample::now(),
            || true,
            |_| true,
        )
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
            verify_org_admission(&ctx, &[], &replay, ClockSample::now(), || true, |_| true),
            Err(AdmissionDenied::MissingHeader)
        );
        assert_eq!(
            verify_org_admission(
                &ctx,
                &[&bytes, &bytes],
                &replay,
                ClockSample::now(),
                || true,
                |_| true
            ),
            Err(AdmissionDenied::MultipleHeaders)
        );
    }

    #[test]
    fn malformed_and_streaming_are_distinct() {
        let floors = empty_floors();
        let ctx = cross_org_ctx(&floors);
        let replay = AdmissionReplayGuard::with_defaults();
        assert_eq!(
            verify_org_admission(
                &ctx,
                &[b"garbage"],
                &replay,
                ClockSample::now(),
                || true,
                |_| true
            ),
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
        let out = verify_org_admission(
            &ctx,
            &[&bytes],
            &replay,
            ClockSample::now(),
            || true,
            |_| false,
        );
        assert_eq!(out, Err(AdmissionDenied::ProviderPolicyRejected));
    }

    /// KC2 (Kyra E1 audit) — the whole admission derives from ONE
    /// `ClockSample`: proof freshness reads the sample's `wall_ns`
    /// (never a fresh `current_timestamp()`), and the replay retention
    /// derives from the SAME sample's monotonic instant.
    #[test]
    fn admission_uses_one_clock_sample_for_freshness_and_retention() {
        let floors = empty_floors();
        let ctx = cross_org_ctx(&floors);
        let proof = cross_org_proof();
        let bytes = proof.encode().expect("encode");
        let expiry_ns = proof.proof_expires_at_unix_ns;
        // A fixed monotonic base for the retention timeline. (The
        // certs were issued against the REAL clock and stay valid at
        // every `wall_ns` used below — only the proof's own freshness
        // moves with the sample.)
        let base = ClockSample::now().monotonic;

        // Freshness reads the SAMPLE's wall: a sample whose wall is
        // PAST the proof expiry denies ProofExpired even though the
        // real wall clock is well before it (no internal clock read).
        let stale_guard = AdmissionReplayGuard::with_defaults();
        let stale = ClockSample {
            wall_ns: expiry_ns + 1_000_000_000,
            monotonic: base,
        };
        assert_eq!(
            verify_org_admission(&ctx, &[&bytes], &stale_guard, stale, || true, |_| true),
            Err(AdmissionDenied::ProofExpired),
        );
        assert_eq!(stale_guard.len(), 0, "an expired proof consumes no slot");

        // A sample 10 s before expiry admits. Retention runs to the
        // proof's expiry PLUS the hard skew ceiling — NOT plus this
        // context's `skew_secs` (§5) — so on the sample's monotonic
        // timeline the horizon is `base + 10s + MAX_TOKEN_CLOCK_SKEW_SECS`.
        let replay = AdmissionReplayGuard::with_defaults();
        let fresh = ClockSample {
            wall_ns: expiry_ns - 10_000_000_000,
            monotonic: base,
        };
        assert!(verify_org_admission(&ctx, &[&bytes], &replay, fresh, || true, |_| true).is_ok());
        let horizon = Duration::from_secs(10 + MAX_TOKEN_CLOCK_SKEW_SECS);

        // Same proof, monotonic still INSIDE retention → Replay.
        let inside = ClockSample {
            wall_ns: expiry_ns - 10_000_000_000,
            monotonic: base + Duration::from_secs(5),
        };
        assert_eq!(
            verify_org_admission(&ctx, &[&bytes], &replay, inside, || true, |_| true),
            Err(AdmissionDenied::Replay),
        );

        // Still inside at the point the PRE-§5 horizon would have ended
        // (`base + 10s`, i.e. expiry + this context's skew of 0). This is
        // the exact instant the old retention reopened the slot.
        let old_horizon = ClockSample {
            wall_ns: expiry_ns - 10_000_000_000,
            monotonic: base + Duration::from_secs(11),
        };
        assert_eq!(
            verify_org_admission(&ctx, &[&bytes], &replay, old_horizon, || true, |_| true),
            Err(AdmissionDenied::Replay),
            "retention must outlast expiry + ctx.skew_secs (§5)",
        );

        // Monotonic PAST the real retention horizon → the slot reopened,
        // so the same still-fresh proof admits again.
        let past = ClockSample {
            wall_ns: expiry_ns - 10_000_000_000,
            monotonic: base + horizon + Duration::from_secs(1),
        };
        assert!(verify_org_admission(&ctx, &[&bytes], &replay, past, || true, |_| true).is_ok());
    }

    /// §5 — widening `verification_skew_secs` at runtime must not re-admit a
    /// proof that was already used.
    ///
    /// Retention used to be `expiry + ctx.skew_secs` while freshness re-reads
    /// the LIVE skew on every call, so the two were the same mutable value
    /// read at different times. Between an admission at skew 0 and a later
    /// one at skew 300 there was a window where the guard entry had lapsed
    /// but the proof was still accepted as fresh — and `admit`'s
    /// expired-key branch returns `Admitted`, so the handler ran a second
    /// time on one signed proof.
    ///
    /// The trigger is a supported operator action, not an attack: `net node
    /// adopt --skew-secs 300` after a clock-drift incident, then
    /// `install_node_authority` — a runtime same-org renewal that does not
    /// clear the guard.
    ///
    /// Red-witness: restoring `ctx.skew_secs` in the retention computation
    /// makes the final assertion admit instead of denying Replay.
    #[test]
    fn widening_skew_does_not_reopen_an_already_used_proof() {
        let floors = empty_floors();
        let proof = cross_org_proof();
        let bytes = proof.encode().expect("encode");
        let expiry_ns = proof.proof_expires_at_unix_ns;
        let base = ClockSample::now().monotonic;
        let replay = AdmissionReplayGuard::with_defaults();

        // 1. Admit under the DEFAULT skew of 0, 10 s before expiry.
        let narrow = cross_org_ctx(&floors);
        assert_eq!(
            narrow.skew_secs, 0,
            "fixture must start at the serde default"
        );
        let admit_at = ClockSample {
            wall_ns: expiry_ns - 10_000_000_000,
            monotonic: base,
        };
        assert!(
            verify_org_admission(&narrow, &[&bytes], &replay, admit_at, || true, |_| true).is_ok(),
            "first use admits",
        );

        // 2. The operator widens skew to the ceiling and reinstalls the
        //    authority. The guard is NOT cleared by that path.
        let mut wide = cross_org_ctx(&floors);
        wide.skew_secs = MAX_TOKEN_CLOCK_SKEW_SECS;

        // 3. Replay the SAME frame 200 s later. Under the widened skew the
        //    proof is still "fresh" (200 < 30 + 300), and it clears the TTL
        //    ceiling — so nothing else in the admission order stops it. Only
        //    the replay guard can, and only if its retention outlasted the
        //    old, narrower skew.
        let resend = ClockSample {
            wall_ns: expiry_ns + 200_000_000_000,
            monotonic: base + Duration::from_secs(210),
        };
        assert_eq!(
            verify_org_admission(&wide, &[&bytes], &replay, resend, || true, |_| true),
            Err(AdmissionDenied::Replay),
            "a used proof must stay denied across a runtime skew widening",
        );

        // Positive control: the freshness/TTL checks really did pass under
        // the widened skew, so the denial above is the REPLAY GUARD's doing
        // and not an incidental ProofExpired. A distinct call_id on the same
        // timeline admits.
        let mut other = cross_org_ctx(&floors);
        other.skew_secs = MAX_TOKEN_CLOCK_SKEW_SECS;
        other.call_id = CALL_ID ^ 0xFFFF;
        let fresh_proof = cross_org_proof_for_call(other.call_id);
        let fresh_bytes = fresh_proof.encode().expect("encode");
        assert!(
            verify_org_admission(&other, &[&fresh_bytes], &replay, resend, || true, |_| true)
                .is_ok(),
            "an unused proof at the same instant admits — so §5's denial is \
             the replay guard, not expiry",
        );
    }

    /// E1.4 §9.5 — a security-view change detected AFTER binding
    /// verification but BEFORE the replay insert denies
    /// `AuthorityChanged` and, crucially, leaves NO replay record:
    /// the stale attempt neither runs the handler nor burns the
    /// `(caller, call_id)` slot, so a legitimate retry from a fresh
    /// view can still admit.
    #[test]
    fn stability_recheck_denies_without_consuming_a_replay_slot() {
        let floors = empty_floors();
        let ctx = cross_org_ctx(&floors);
        let replay = AdmissionReplayGuard::with_defaults();
        let bytes = cross_org_proof().encode().expect("encode");

        // The view changed mid-admission → recheck returns false.
        let out = verify_org_admission(
            &ctx,
            &[&bytes],
            &replay,
            ClockSample::now(),
            || false,
            |_| true,
        );
        assert_eq!(out, Err(AdmissionDenied::AuthorityChanged));
        assert_eq!(replay.len(), 0, "the stale attempt consumed no replay slot");

        // A retry from a now-stable view admits the SAME call — proof
        // the earlier denial didn't burn the correlation id.
        let out = verify_org_admission(
            &ctx,
            &[&bytes],
            &replay,
            ClockSample::now(),
            || true,
            |_| true,
        );
        assert!(out.is_ok(), "retry under a stable view admits");
        assert_eq!(replay.len(), 1);
    }

    /// The recheck runs LATE — a proof that fails an EARLIER check
    /// (here: streaming) is rejected on its own merits and never
    /// reaches the §9.5 hook (so `AuthorityChanged` cannot mask a
    /// more specific denial).
    #[test]
    fn stability_recheck_runs_after_credential_checks() {
        let floors = empty_floors();
        let mut ctx = cross_org_ctx(&floors);
        ctx.is_unary = false;
        let replay = AdmissionReplayGuard::with_defaults();
        let bytes = cross_org_proof().encode().expect("encode");
        // Even with an unstable view, the streaming rejection wins.
        let out = verify_org_admission(
            &ctx,
            &[&bytes],
            &replay,
            ClockSample::now(),
            || false,
            |_| true,
        );
        assert_eq!(out, Err(AdmissionDenied::StreamingUnsupported));
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

    /// E2.2: EVERY `AdmissionDenied` variant maps to a defined coarse reason
    /// that round-trips through its wire byte, so a denial never leaks the
    /// detailed reason yet is always classified. The `coarse()` match is
    /// exhaustive (no wildcard), so a new variant forces classification; this
    /// list enumerates them for the runtime round-trip and pins the anchors of
    /// each bucket.
    #[test]
    fn every_denial_maps_to_a_defined_coarse_reason() {
        const ALL: &[AdmissionDenied] = &[
            AdmissionDenied::NotOrgProtected,
            AdmissionDenied::MissingHeader,
            AdmissionDenied::MultipleHeaders,
            AdmissionDenied::MalformedProof,
            AdmissionDenied::StreamingUnsupported,
            AdmissionDenied::MemberBindingMismatch,
            AdmissionDenied::ActingOrgMismatch,
            AdmissionDenied::UnexpectedCapabilityGrant,
            AdmissionDenied::MissingCapabilityGrant,
            AdmissionDenied::ForeignIssuer,
            AdmissionDenied::GranteeMismatch,
            AdmissionDenied::InsufficientRights,
            AdmissionDenied::CapabilityMismatch,
            AdmissionDenied::TargetNotCovered,
            AdmissionDenied::DispatcherGrantScope,
            AdmissionDenied::DispatcherGrantInvalid,
            AdmissionDenied::MembershipInvalid,
            AdmissionDenied::MembershipRevoked,
            AdmissionDenied::CapabilityGrantInvalid,
            AdmissionDenied::ProofExpired,
            AdmissionDenied::BindingInvalid,
            AdmissionDenied::ProviderAuthorityUnavailable,
            AdmissionDenied::AuthorityChanged,
            AdmissionDenied::Replay,
            AdmissionDenied::CallIdCollision,
            AdmissionDenied::ReplayCapacity,
            AdmissionDenied::PerCallerReplayCapacity,
            AdmissionDenied::ProviderPolicyRejected,
        ];
        for &d in ALL {
            let c = d.coarse();
            assert_eq!(
                CoarseAdmissionReason::from_wire(c.to_wire()),
                Some(c),
                "coarse reason for {d:?} must round-trip through its wire byte",
            );
        }
        // Bucket anchors.
        assert_eq!(
            AdmissionDenied::StreamingUnsupported.coarse(),
            CoarseAdmissionReason::NotSupported,
        );
        for unavailable in [
            AdmissionDenied::ProviderAuthorityUnavailable,
            AdmissionDenied::AuthorityChanged,
            AdmissionDenied::ReplayCapacity,
            AdmissionDenied::PerCallerReplayCapacity,
        ] {
            assert_eq!(unavailable.coarse(), CoarseAdmissionReason::Unavailable);
        }
        for denied in [
            AdmissionDenied::BindingInvalid,
            AdmissionDenied::Replay,
            AdmissionDenied::ProviderPolicyRejected,
            AdmissionDenied::MembershipRevoked,
        ] {
            assert_eq!(denied.coarse(), CoarseAdmissionReason::Denied);
        }
        // Every coarse byte decodes; an unknown byte does not.
        assert_eq!(CoarseAdmissionReason::from_wire(3), None);
    }
}
