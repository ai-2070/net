//! VERBATIM vendoring of `net/crates/net/src/adapter/net/behavior/org_admission.rs`
//! **lines 64–662 at revision `85ecc77c9`** — the frozen old-provider
//! [`verify_org_admission`] (with its `is_unary` step 4), the frozen
//! [`AdmissionContext`] (`is_unary: bool`), [`AdmissionDenied`],
//! [`CoarseAdmissionReason`] (incl. the exhaustive `coarse()` mapping),
//! [`Admitted`] and [`OrgAdmission`].
//!
//! Provenance (byte-identical; the appended body below is this extraction):
//! `git show 85ecc77c9:net/crates/net/src/adapter/net/behavior/org_admission.rs |
//! sed -n '64,662p'` → sha256
//! `ef756fdb76c4ba2eaf7548a437bce8cc897eb60e45955b2cca423b2f6c3fd646`.
//!
//! Adaptations (imports ONLY, zero body lines changed): the original `use`
//! block (`org_admission.rs:57-63` at that revision) is retargeted to the
//! test crate's `net::…` paths; `super::org_call` is retargeted to the
//! vendored [`super::old_org_call`] (the frozen decoder — NEVER the new one).

#![allow(
    dead_code,
    reason = "verbatim vendoring of the frozen old-provider surface: items beyond the \
              witnessed decode/verify path are retained intact, never pruned"
)]

use net::adapter::net::behavior::admission_clock::ClockSample;
use net::adapter::net::behavior::org::OrgId;
use net::adapter::net::behavior::org_admission_replay::{
    AdmissionReplayGuard, ReplayOutcome, ReplayPrincipal,
};
use net::adapter::net::behavior::org_grant::CapabilityAuthorityId;
use net::adapter::net::behavior::org_revocation::OrgRevocationState;
use net::adapter::net::identity::{EntityId, MAX_TOKEN_CLOCK_SKEW_SECS};

use super::old_org_call::{OrgCallProof, MAX_ORG_CALL_PROOF_BYTES};
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
    /// THIS external acting ORG has filled its aggregate replay allocation
    /// across all of its member identities (§5).
    ///
    /// Distinct from [`Self::PerCallerReplayCapacity`] because a coalition is
    /// the point of the attack: identities are free to mint, so a per-caller
    /// denial names the wrong subject and reads to an operator as a single
    /// misconfigured client. This names the org, which is the thing an
    /// operator can actually act on (revoke the grant).
    PerOrganizationReplayCapacity,
    /// The EXTERNAL pool is full with no single org over quota (§5) — many
    /// distinct external orgs active at once. Notably NOT a state in which the
    /// provider's own org is affected: the reserve is untouched by
    /// construction.
    ExternalPoolReplayCapacity,
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
            | D::PerCallerReplayCapacity
            | D::PerOrganizationReplayCapacity
            | D::ExternalPoolReplayCapacity => C::Unavailable,
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
    // §14 — BOUNDARY, stated because it is invisible at this call site.
    //
    // `floor_for` returns 0 for an org this node holds no floors for, and
    // `generation < 0` is unsatisfiable for a `u32`. For an OwnerDelegated
    // call `acting_org` is this provider's own org, whose bundles it imports
    // by definition, so the check bites. For CrossOrgGranted, `acting_org` is
    // a FOREIGN org A — and floors arrive only via operator-distributed,
    // A-signed bundle files that B has chosen to merge. If B never imports
    // A's bundles (the default), this check silently evaluates to "permit"
    // for every cross-org caller.
    //
    // So a compromised A member keeps invoking B's protected capability for
    // the life of its membership certificate — up to `MAX_ORG_CERT_TTL_SECS`
    // (2 years) — and B cannot revoke the capability grant it issued either,
    // because floors apply to the membership cert and NOT to grants (v1, see
    // `org_grant.rs`). The only live provider-side kill switch is the
    // `provider_policy` closure at step 11.
    //
    // Deliberately not "fixed" here: making an absent foreign floor set DENY
    // would break every cross-org deployment that has not built bundle
    // distribution, and there is no signal at this layer to distinguish "no
    // floors because none were ever needed" from "no floors because
    // distribution is broken". Closing it properly needs a cross-org bundle
    // channel plus an explicit per-issuer "floors expected" assertion — an
    // operational surface, not a check. Until then `provider_policy` is the
    // documented mechanism, and this is the §D1 limitation made concrete at
    // the exact line where a reader would otherwise assume coverage.
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
    // §5 — the quota principal. `acting_org` here is the VERIFIED one: it was
    // taken from the org-signed membership certificate at step 5 and
    // cross-checked against the dispatcher grant, so it is not a wire claim
    // and not the (attacker-choosable) certificate issuer. That is what makes
    // the aggregate quota meaningful — identities are free to mint, a trusted
    // acting org is not.
    let principal = ReplayPrincipal {
        caller: ctx.authenticated_caller,
        acting_org: &acting_org,
        provider_owner_org: &ctx.provider_owner_org,
    };
    match replay.admit(
        principal,
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
        ReplayOutcome::PerOrganizationCapacityExhausted => {
            return Err(AdmissionDenied::PerOrganizationReplayCapacity)
        }
        ReplayOutcome::ExternalPoolCapacityExhausted => {
            return Err(AdmissionDenied::ExternalPoolReplayCapacity)
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
