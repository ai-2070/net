//! The ordered admission engine (§2.4): the one place a decoded
//! [`OrgCallProof`] becomes an admit-or-typed-refusal decision.
//!
//! Core's `behavior/org_admission.rs`, ported byte-for-byte. The
//! decision ORDER is the security property — cheap plaintext checks
//! before signatures, the TOFU channel binding before any credential
//! work, replay insert before the provider policy — so the body
//! mirrors core's step for step and the witnesses pin the refusals
//! by exact [`AdmissionDenied`] variant.
//!
//! Core's `ClockSample { wall_ns, monotonic: Instant }` becomes two
//! integers (`now_unix_ns`, `now_mono_ms`) passed beside the guard:
//! every wall-clock check inside one admission reads the ONE
//! captured sample, and the replay retention derives from the same
//! sample's monotonic projection, so a wall-clock jump cannot make
//! checks disagree.

use crate::org::cert::OrgId;
use crate::org::entity::EntityId;
use crate::org::grant::CapabilityAuthorityId;
use crate::org::proof::{OrgCallProof, OrgStreamCallProof, RpcCallShape, MAX_ORG_CALL_PROOF_BYTES};
use crate::org::replay::{AdmissionReplayGuard, ReplayOutcome, ReplayPrincipal};
use crate::org::revocation::RevocationFacts;

/// The admission mode a provider registered for one capability
/// (Locked #6). Bound at registration (§2.4a); resolved BEFORE gate
/// selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrgAdmission {
    /// v0.4 behavior — allow-list axes + transport auth only. NOT
    /// verified by this engine; the seam routes it through the
    /// capability-auth path.
    PublicAuthenticated,
    /// The caller acts for the provider's own owner org.
    OwnerDelegated,
    /// The caller's org holds a cross-org capability grant issued by
    /// the provider's owner org.
    CrossOrgGranted,
}

/// A distinguishable admission-denial reason (§2.4). The gate maps
/// every variant to the RPC `AdmissionDenied` status while
/// preserving the reason for audit — a caller bug (e.g.
/// [`Self::CallIdCollision`]) must read differently from an attack
/// (e.g. [`Self::Replay`] or [`Self::BindingInvalid`]).
///
/// `#[non_exhaustive]` (C4/Q7): new variants are a named source
/// break for downstream exhaustive `match`es, never a silent
/// reclassification. The [`Self::coarse`] mapping stays exhaustive
/// IN-CRATE (compile-forced), so a new variant can never escape the
/// caller-facing `{Denied, NotSupported, Unavailable}` byte set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AdmissionDenied {
    /// The engine was invoked for a non-org-protected mode
    /// ([`OrgAdmission::PublicAuthenticated`]) — a caller logic
    /// error; the seam should have routed it elsewhere.
    NotOrgProtected,
    /// No `net-org-admission` header was present.
    MissingHeader,
    /// More than one `net-org-admission` header — exactly one or deny
    /// (§2.3 header discipline).
    MultipleHeaders,
    /// The proof header value did not decode (bad bytes, over cap).
    MalformedProof,
    /// A streaming (non-unary) call on a unary registration — a
    /// unary registration never admits streaming flags (§1.5 step
    /// 4a). Distinct from [`Self::ShapeMismatch`] so "this service
    /// does not stream" reads differently from "your proof does not
    /// match your flags".
    StreamingUnsupported,
    /// The call's shape is incoherent with its proof or its
    /// registration (§1.5 step 4): streaming flags on a streaming
    /// registration of a DIFFERENT shape, or `proof.kind` naming a
    /// different shape than the flags (C4, coarse `Denied`).
    ShapeMismatch,
    /// The opening proof's `session_binding` does not equal the Noise
    /// handshake hash of the RECEIVING session (§1.3) — a replayed or
    /// transplanted opening, or a session carrying no binding at all
    /// (hand-built test sessions can never admit a protected stream).
    SessionBindingMismatch,
    /// The explicitly requested deadline exceeds the provider's
    /// maximum protected lifetime (§2.1 bound 2: refused, never
    /// clamped; C4, coarse `Denied`).
    DeadlineExceedsPolicy,
    /// The `(caller, call_id)` key is already live in the protected
    /// call registry (§3 `reserve`): one admitted handler owns the
    /// stream (C4, coarse `Denied`).
    ActiveCallOwned,
    /// The node/caller/org active-stream budget is exhausted at
    /// `reserve` (§3; C4, coarse `Unavailable` — a later retry may
    /// succeed).
    ActiveStreamCapacity,
    /// Credentials were revoked mid-call and the call is being
    /// retired (C4, coarse `Denied` — plan §4.3: midstream retirement
    /// arrives as a merits denial).
    Revoked,
    /// The byte budget refused an item and retired the call (C4,
    /// coarse `Unavailable`).
    ResourceExhausted,
    /// The proof's `member` is not the TOFU-authenticated channel
    /// peer — a captured proof replayed by a different peer fails
    /// here before any signature work (§1.5 step 5).
    MemberBindingMismatch,
    /// The dispatcher grant's org disagrees with the membership
    /// cert's org — the acting org is named by the membership and the
    /// grant must agree.
    ActingOrgMismatch,
    /// `OwnerDelegated` admission carrying a cross-org capability
    /// grant — same-org calls have none.
    UnexpectedCapabilityGrant,
    /// `CrossOrgGranted` admission carrying NO capability grant.
    MissingCapabilityGrant,
    /// The capability grant is issued by someone other than the
    /// provider's owner org — authority ONLY if my owner issued it.
    ForeignIssuer,
    /// The capability grant names a grantee org other than the
    /// caller's verified acting org.
    GranteeMismatch,
    /// The capability grant does not carry `INVOKE`.
    InsufficientRights,
    /// The capability grant is for a different capability than the
    /// one invoked.
    CapabilityMismatch,
    /// The capability grant's target scope does not cover THIS
    /// provider ("covering exactly me (owned by my owner org)").
    TargetNotCovered,
    /// The dispatcher grant does not empower THIS caller to act for
    /// the acting org over the invoked capability (wrong dispatcher
    /// entity, or an `Exact` scope naming a different capability).
    DispatcherGrantScope,
    /// The dispatcher grant failed its signature/structural/window
    /// validation.
    DispatcherGrantInvalid,
    /// The membership certificate failed its
    /// signature/structural/window validation.
    MembershipInvalid,
    /// The membership certificate's `generation` is below the fed
    /// revocation floor for `(acting_org, member)` — the cert is
    /// revoked. (A cert AT the floor is alive.)
    MembershipRevoked,
    /// The capability grant failed its
    /// signature/structural/window validation.
    CapabilityGrantInvalid,
    /// The proof is expired, or claims an expiry beyond
    /// `now + MAX_ORG_PROOF_TTL_SECS` — both surface here (the TTL
    /// ceiling refusing a standing credential is a merits denial).
    ProofExpired,
    /// The call-binding signature did not verify against the caller
    /// entity over the reconstructed transcript — a tampered request
    /// digest/body/headers/order, or a proof re-pointed at other
    /// credentials.
    BindingInvalid,
    /// The provider cannot admit right now: its revocation view or
    /// installed authority is unavailable (`poisoned` store facts).
    ProviderAuthorityUnavailable,
    /// The §9.5 stability recheck failed: the provider's security
    /// view (floors/authority/store health) changed mid-admission.
    /// Consumes NO replay slot, so the gate may retry from a fresh
    /// view.
    AuthorityChanged,
    /// The SAME proof (identical binding digest) was re-presented
    /// before its retention window closed — a replay.
    Replay,
    /// The same `(caller, call_id)` with a DIFFERENT binding digest
    /// is already live — a correlation-id collision (caller bug or
    /// forged reuse of an id).
    CallIdCollision,
    /// The GLOBAL replay guard is full of still-live entries; the
    /// call is denied fail-closed rather than evicting one.
    ReplayCapacity,
    /// THIS caller already holds its maximum simultaneously-retained
    /// replay entries.
    PerCallerReplayCapacity,
    /// THIS external acting ORG has consumed its aggregate replay
    /// allocation across all of its member identities.
    PerOrganizationReplayCapacity,
    /// The EXTERNAL replay pool is full with no single org over
    /// quota — many active external orgs; the owner org is
    /// unaffected.
    ExternalPoolReplayCapacity,
    /// The provider-local policy closure vetoed the verified proof.
    ProviderPolicyRejected,
}

/// The COARSE wire reason (E2.2): the three buckets a caller sees
/// when only retry-behavior matters, `Denied` (will never succeed as
/// is) / `NotSupported` (shape) / `Unavailable` (transient).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoarseAdmissionReason {
    /// Rejected on the merits — a credential, binding, replay, or
    /// provider-policy failure. Retrying the SAME proof will not
    /// succeed.
    Denied,
    /// The provider does not support this call shape (a streaming
    /// frame on a protected unary service). Not retryable as-is.
    NotSupported,
    /// The provider cannot admit right now — its own authority is
    /// unavailable, its security view changed mid-admission, or a
    /// replay allocation is full. A later retry may succeed.
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

    /// Decode a coarse reason from its wire byte (`None` on an
    /// unknown byte).
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
    /// Map this detailed reason to the COARSE wire reason. The match
    /// is EXHAUSTIVE (no wildcard) BY DESIGN: a newly added
    /// [`AdmissionDenied`] variant forces a compile error here, so it
    /// can never silently fall into a default bucket and escape the
    /// caller-facing classification.
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
            | D::ExternalPoolReplayCapacity
            | D::ActiveStreamCapacity
            | D::ResourceExhausted => C::Unavailable,
            // The call shape is unsupported on a protected unary service.
            D::StreamingUnsupported => C::NotSupported,
            // Everything else is a denial on the merits.
            D::NotOrgProtected
            | D::MissingHeader
            | D::MultipleHeaders
            | D::MalformedProof
            | D::ShapeMismatch
            | D::SessionBindingMismatch
            | D::DeadlineExceedsPolicy
            | D::ActiveCallOwned
            | D::Revoked
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
///
/// `#[non_exhaustive]` + [`Self::new`] (C3/Q7): external literal
/// construction is a named source break; the stable constructor is
/// the migration path. There is deliberately NO second derivable
/// `is_unary`-style field beside [`Self::shape`].
pub struct AdmissionContext<'a> {
    /// The registered admission mode for the invoked capability.
    pub mode: OrgAdmission,
    /// The TOFU-authenticated identity of the channel peer — who is
    /// ACTUALLY on the wire, independent of what the proof claims.
    pub authenticated_caller: &'a EntityId,
    /// This provider's entity (P).
    pub provider: &'a EntityId,
    /// This provider's PROVEN owner org (from its installed authority
    /// scaffold — never fold state).
    pub provider_owner_org: OrgId,
    /// The authority id of the invoked service — the provider
    /// computes this from the service tag it is about to dispatch.
    pub invoked_capability: CapabilityAuthorityId,
    /// The nRPC correlation id of this call.
    pub call_id: u64,
    /// blake3 of the canonical request with the admission header
    /// removed (computed via
    /// [`crate::org::digest::org_request_digest`]).
    pub request_digest: [u8; 32],
    /// The SHAPE TERM (C3): the call's shape as derived from
    /// (registration shape, payload flags) at the gate — the exact
    /// replacement for the old flags-derived `is_unary`.
    pub shape: RpcCallShape,
    /// The shape the invoked REGISTRATION serves (§1.5 step 4's
    /// "registration shape"): [`RpcCallShape::Unary`] for a unary
    /// serve seam, the matching streaming shape for a streaming one.
    pub registered_shape: RpcCallShape,
    /// The Noise handshake hash of the RECEIVING session (§1.3);
    /// `None` for a hand-built/test session, which can never admit a
    /// protected STREAM (unary wire semantics are unchanged and do
    /// not read this).
    pub session_binding: Option<[u8; 32]>,
    /// The provider's current revocation floor view (core's
    /// `&OrgRevocationState`, fed to the leaf as
    /// [`RevocationFacts`]).
    pub floors: &'a RevocationFacts,
    /// Clock-skew tolerance for every wall-clock check.
    pub skew_secs: u64,
}

impl<'a> AdmissionContext<'a> {
    /// The stable external constructor (C3). `client_streaming` /
    /// `streaming_response` are the two nRPC streaming-flag bits of
    /// the REQUEST payload, resolved to plain bools at the caller;
    /// [`Self::shape`] is derived from them here, and
    /// [`Self::registered_shape`] is carried for the §1.5 step-4
    /// coherence checks.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        mode: OrgAdmission,
        authenticated_caller: &'a EntityId,
        provider: &'a EntityId,
        provider_owner_org: OrgId,
        invoked_capability: CapabilityAuthorityId,
        call_id: u64,
        request_digest: [u8; 32],
        registered_shape: RpcCallShape,
        client_streaming: bool,
        streaming_response: bool,
        session_binding: Option<[u8; 32]>,
        floors: &'a RevocationFacts,
        skew_secs: u64,
    ) -> Self {
        Self {
            mode,
            authenticated_caller,
            provider,
            provider_owner_org,
            invoked_capability,
            call_id,
            request_digest,
            shape: RpcCallShape::from_streaming_flags(client_streaming, streaming_response),
            registered_shape,
            session_binding,
            floors,
            skew_secs,
        }
    }
}

/// Verify one admission proof against `ctx` in the §2.4 order.
///
/// `admission_headers` is every value carried under
/// [`ORG_ADMISSION_HEADER`](crate::org::proof::ORG_ADMISSION_HEADER)
/// (exactly one is required). `replay` is the provider's replay
/// guard. `now_unix_ns` + `now_mono_ms` are ONE paired wall +
/// monotonic sample for the whole admission (core's `ClockSample`):
/// every credential/proof freshness check reads `now_unix_ns` and
/// the replay retention derives from the SAME sample's monotonic
/// projection, so no clock is read inside a single admission and a
/// wall-clock jump cannot make checks disagree.
///
/// `stability_recheck` is the §9.5 linearization hook (E1.4): it
/// runs AFTER all credential/binding verification but BEFORE the
/// replay insert, and returns `true` iff the provider's security
/// view (the floor snapshot + installed authority + store health
/// captured by the gate before verification) is STILL current. A
/// `false` return denies [`AdmissionDenied::AuthorityChanged`]
/// WITHOUT consuming a `(caller, call_id)` replay slot, so a stale
/// decision can neither run the handler nor burn the correlation id.
///
/// `provider_policy` is the application veto, run LAST — it sees the
/// verified proof (for a stream: its [`OrgStreamCallProof::unary_prefix`])
/// and returns `true` to admit.
///
/// Returns the four-party [`Admitted`] attribution on success, or a
/// distinguishable [`AdmissionDenied`] reason.
#[allow(clippy::too_many_arguments)]
pub fn verify_org_admission(
    ctx: &AdmissionContext,
    admission_headers: &[&[u8]],
    replay: &AdmissionReplayGuard,
    now_unix_ns: u64,
    now_mono_ms: u64,
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

    // 3. Decode the proof — under the REGISTERED shape's decoder
    //    (§1.4: unary prefix-tolerant, streaming strict).
    enum DecodedProof {
        Unary(OrgCallProof),
        Stream(OrgStreamCallProof),
    }
    let proof = match ctx.registered_shape {
        RpcCallShape::Unary => DecodedProof::Unary(
            OrgCallProof::decode(header).map_err(|_| AdmissionDenied::MalformedProof)?,
        ),
        RpcCallShape::ServerStreaming | RpcCallShape::ClientStreaming | RpcCallShape::Duplex => {
            DecodedProof::Stream(
                OrgStreamCallProof::decode(header).map_err(|_| AdmissionDenied::MalformedProof)?,
            )
        }
    };
    let (membership, dispatcher, cap_grant, expires_ns, call_binding_sig) = match &proof {
        DecodedProof::Unary(p) => (
            &p.caller_membership,
            &p.dispatcher_grant,
            &p.capability_grant,
            p.proof_expires_at_unix_ns,
            &p.call_binding_sig,
        ),
        DecodedProof::Stream(p) => (
            &p.caller_membership,
            &p.dispatcher_grant,
            &p.capability_grant,
            p.proof_expires_at_unix_ns,
            &p.call_binding_sig,
        ),
    };

    // 4. Shape coherence (§1.5).
    //    (a) unary registration + streaming flags ⇒ StreamingUnsupported
    //    (b) streaming registration + flags ≠ registered shape ⇒ ShapeMismatch
    //    (c) streaming registration + proof.kind ≠ shape ⇒ ShapeMismatch
    match ctx.registered_shape {
        RpcCallShape::Unary => {
            if ctx.shape != RpcCallShape::Unary {
                return Err(AdmissionDenied::StreamingUnsupported);
            }
        }
        RpcCallShape::ServerStreaming | RpcCallShape::ClientStreaming | RpcCallShape::Duplex => {
            if ctx.shape != ctx.registered_shape {
                return Err(AdmissionDenied::ShapeMismatch);
            }
            match &proof {
                DecodedProof::Stream(p)
                    if RpcCallShape::from_stream_kind(p.kind) == Some(ctx.shape) => {}
                DecodedProof::Stream(_) => return Err(AdmissionDenied::ShapeMismatch),
                DecodedProof::Unary(_) => {
                    unreachable!("streaming registrations decode the streaming proof")
                }
            }
        }
    }

    // 5. TOFU member binding: the proof's caller must be who is
    //    actually on the channel — a captured proof replayed by a
    //    different peer fails here before any signature work.
    if &membership.member != ctx.authenticated_caller {
        return Err(AdmissionDenied::MemberBindingMismatch);
    }

    // The acting org is named by the membership; the dispatcher
    // grant must agree.
    let acting_org = membership.org_id;
    if dispatcher.org_id != acting_org {
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
            if cap_grant.is_some() {
                return Err(AdmissionDenied::UnexpectedCapabilityGrant);
            }
        }
        OrgAdmission::CrossOrgGranted => {
            let grant = cap_grant
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
    if dispatcher.dispatcher != *ctx.authenticated_caller
        || !dispatcher.covers_capability(&ctx.invoked_capability)
    {
        return Err(AdmissionDenied::DispatcherGrantScope);
    }

    // 8. Credentials: signatures + windows + floors + freshness.
    //    EVERY wall-clock check reads the ONE captured sample.
    let now_secs = now_unix_ns / 1_000_000_000;
    membership
        .is_valid_at_with_skew(now_secs, ctx.skew_secs)
        .map_err(|_| AdmissionDenied::MembershipInvalid)?;
    let floor = ctx.floors.floor_for(&acting_org, &membership.member);
    if membership.generation < floor {
        return Err(AdmissionDenied::MembershipRevoked);
    }
    dispatcher
        .is_valid_at_with_skew(now_secs, ctx.skew_secs)
        .map_err(|_| AdmissionDenied::DispatcherGrantInvalid)?;
    if let Some(grant) = &cap_grant {
        grant
            .is_valid_at_with_skew(now_secs, ctx.skew_secs)
            .map_err(|_| AdmissionDenied::CapabilityGrantInvalid)?;
    }
    match &proof {
        DecodedProof::Unary(p) => p.check_expiry_at(now_unix_ns, ctx.skew_secs),
        DecodedProof::Stream(p) => p.check_expiry_at(now_unix_ns, ctx.skew_secs),
    }
    .map_err(|_| AdmissionDenied::ProofExpired)?;

    // 9. Call binding: the caller ENTITY signed THIS exact call.
    match &proof {
        DecodedProof::Unary(p) => {
            let binding = p.binding_for_verify(
                ctx.provider_owner_org,
                ctx.provider.clone(),
                ctx.call_id,
                ctx.invoked_capability,
                ctx.request_digest,
            );
            binding
                .verify(call_binding_sig)
                .map_err(|_| AdmissionDenied::BindingInvalid)?;
        }
        DecodedProof::Stream(p) => {
            let binding = p.binding_for_stream_verify(
                ctx.provider_owner_org,
                ctx.provider.clone(),
                ctx.call_id,
                ctx.invoked_capability,
                ctx.request_digest,
            );
            binding
                .verify(call_binding_sig)
                .map_err(|_| AdmissionDenied::BindingInvalid)?;
            // 9b. Session binding (§1.3): the opening is admitted
            //     only on the ONE session its proof binds — the
            //     RECEIVING session's full Noise handshake hash must
            //     equal `proof.session_binding`. A hand-built session
            //     (`None`) can never admit a protected stream.
            match ctx.session_binding {
                Some(resolved) if resolved == p.session_binding => {}
                _ => return Err(AdmissionDenied::SessionBindingMismatch),
            }
        }
    }

    // 9.5. Stability linearization (E1.4 §9.5) — BEFORE the replay
    //      insert.
    if !stability_recheck() {
        return Err(AdmissionDenied::AuthorityChanged);
    }

    // 10. Replay guard: atomic insert-or-deny BEFORE the handler.
    //     Retention = proof expiry + MAX_TOKEN_CLOCK_SKEW_SECS (the
    //     HARD ceiling, NOT ctx.skew_secs) on the SAME sample's
    //     monotonic timeline.
    let binding_digest: [u8; 32] = blake3::hash(call_binding_sig).into();
    let skew_ns = crate::org::MAX_TOKEN_CLOCK_SKEW_SECS.saturating_mul(1_000_000_000);
    let retain_until_wall_ns = expires_ns.saturating_add(skew_ns);
    // `ClockSample::monotonic_deadline_for`, as the pair of integers
    // this port passes instead of an `Instant`: project the wall
    // retention horizon onto the monotonic sample, clamped at the
    // sample itself (an already-past horizon retains nothing).
    let expires_at = if retain_until_wall_ns <= now_unix_ns {
        now_mono_ms
    } else {
        now_mono_ms.saturating_add((retain_until_wall_ns - now_unix_ns) / 1_000_000)
    };
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
        now_mono_ms,
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

    // 11. Provider-local policy LAST (Locked #6). For a streaming
    //     call the closure sees the five unary prefix fields as an
    //     `OrgCallProof`.
    let policy_view;
    let policy_proof: &OrgCallProof = match &proof {
        DecodedProof::Unary(p) => p,
        DecodedProof::Stream(p) => {
            policy_view = p.unary_prefix();
            &policy_view
        }
    };
    if !provider_policy(policy_proof) {
        return Err(AdmissionDenied::ProviderPolicyRejected);
    }

    Ok(Admitted {
        caller: membership.member.clone(),
        acting_org,
        provider_org: ctx.provider_owner_org,
        provider: ctx.provider.clone(),
        capability: ctx.invoked_capability,
    })
}
