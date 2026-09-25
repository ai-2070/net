//! Call bindings and per-call admission proofs.
//!
//! Core's `behavior/org_call.rs`, ported byte-for-byte. Everything a
//! caller entity signs to authorize exactly ONE call lives here: the
//! fixed-width [`CallBinding`] / [`StreamCallBinding`] transcripts
//! (304 B / 337 B, `blake3::derive_key` under their own contexts so
//! a signature in one domain can never verify in the other), the
//! [`OrgCallProof`] / [`OrgStreamCallProof`] wire objects, and the
//! expiry + TTL-ceiling check.
//!
//! The mixed-version contract (§1.4 of the streaming plan) is in the
//! decoders: [`OrgCallProof::decode`] is **prefix-tolerant** (it
//! consumes the five leading fields and ignores any suffix — the
//! frozen old-provider behavior), while
//! [`OrgStreamCallProof::decode`] is **strict**: it consumes the
//! entire bounded value and refuses unknown `kind`s, truncated
//! suffixes and trailing bytes. Historical tolerance lives only on
//! the unary side.

use serde::{Deserialize, Serialize};

use crate::identity::EntityKeypair;
use crate::org::cert::{OrgError, OrgId, OrgMembershipCert};
use crate::org::entity::EntityId;
use crate::org::grant::{CapabilityAuthorityId, OrgCapabilityGrant, OrgDispatcherGrant};

/// blake3 `derive_key` context for the call-binding transcript hash
/// (plan §2.3). A signing domain: bytes hashed under this context
/// can never be confused with a grant, cert, or floor bundle
/// transcript.
pub const ORG_CALL_BINDING_CONTEXT: &str = "net-org-call-v1";

/// blake3 `derive_key` context for the STREAMING call-binding
/// transcript (streaming plan §1.2). A separate signing domain from
/// [`ORG_CALL_BINDING_CONTEXT`]: a signature over the 11 unary
/// fields can never verify as a streaming binding or vice versa,
/// whatever subset of fields the two transcripts share.
pub const ORG_STREAM_CALL_BINDING_CONTEXT: &str = "net-org-stream-call-v1";

/// Streaming call kind: server-streaming (1 = SS / 2 = CS / 3 = DX;
/// 0 is never emitted and the streaming decoder rejects it).
pub const STREAM_CALL_KIND_SERVER_STREAMING: u8 = 1;
/// Streaming call kind: client-streaming.
pub const STREAM_CALL_KIND_CLIENT_STREAMING: u8 = 2;
/// Streaming call kind: duplex.
pub const STREAM_CALL_KIND_DUPLEX: u8 = 3;

/// The shape of one RPC call (C3) — the streaming term that replaces
/// `AdmissionContext.is_unary`. `Unary` is the zero-streaming-flags
/// call; the three streaming shapes map 1:1 onto the wire `kind` of
/// an [`OrgStreamCallProof`] ([`Self::stream_kind`]).
/// `#[non_exhaustive]`: a future shape must not break downstream
/// exhaustive matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RpcCallShape {
    /// One request, one response — no streaming flags.
    Unary,
    /// One request, many responses (streaming-response flag).
    ServerStreaming,
    /// Many requests, one response (client-streaming flag).
    ClientStreaming,
    /// Many requests, many responses (both flags).
    Duplex,
}

impl RpcCallShape {
    /// Derive the call's shape from the two nRPC streaming-flag bits,
    /// resolved to plain bools at the caller (this module never
    /// imports the wire flag constants).
    pub fn from_streaming_flags(client_streaming: bool, streaming_response: bool) -> Self {
        match (client_streaming, streaming_response) {
            (false, false) => Self::Unary,
            (false, true) => Self::ServerStreaming,
            (true, false) => Self::ClientStreaming,
            (true, true) => Self::Duplex,
        }
    }

    /// The proof `kind` this shape rides, or `None` for
    /// [`Self::Unary`] (which rides the unary [`OrgCallProof`] and
    /// emits no kind).
    pub fn stream_kind(self) -> Option<u8> {
        match self {
            Self::Unary => None,
            Self::ServerStreaming => Some(STREAM_CALL_KIND_SERVER_STREAMING),
            Self::ClientStreaming => Some(STREAM_CALL_KIND_CLIENT_STREAMING),
            Self::Duplex => Some(STREAM_CALL_KIND_DUPLEX),
        }
    }

    /// The shape a wire `kind` names (`None` for 0 or any unknown
    /// kind).
    pub fn from_stream_kind(kind: u8) -> Option<Self> {
        match kind {
            STREAM_CALL_KIND_SERVER_STREAMING => Some(Self::ServerStreaming),
            STREAM_CALL_KIND_CLIENT_STREAMING => Some(Self::ClientStreaming),
            STREAM_CALL_KIND_DUPLEX => Some(Self::Duplex),
            _ => None,
        }
    }
}

/// The well-known RPC header the proof rides on (the engine enforces
/// EXACTLY ONE or deny).
pub const ORG_ADMISSION_HEADER: &str = "net-org-admission";

/// Ceiling on how far in the future a proof may claim to expire,
/// measured from now at verify time (§2.3: "FINITE, always"). A
/// proof that could FIRST be admitted arbitrarily far in the future
/// is a standing replayable credential; this bounds the window
/// independently of the RPC deadline. 30 s provisional — the callee's
/// `AdmissionReplayConfig` is authoritative and this freezes only
/// after measurement.
pub const MAX_ORG_PROOF_TTL_SECS: u64 = 30;

/// Upper bound on the postcard-encoded proof, asserted against the
/// RPC header value cap (4096). membership 156 + dispatcher grant
/// 185 + capability grant 318 + expiry 8 + sig 64 = 731 raw; postcard
/// length prefixes and the option tag add a handful of bytes. 1024
/// is a comfortable pin well under 4096.
pub const MAX_ORG_CALL_PROOF_BYTES: usize = 1024;

/// The digest of a credential's canonical wire bytes, bound into the
/// transcript so a proof cannot be re-pointed at a different (but
/// individually valid) credential without breaking the signature.
fn credential_digest(bytes: &[u8]) -> [u8; 32] {
    blake3::hash(bytes).into()
}

/// The four-party call binding: everything the caller ENTITY signs to
/// authorize exactly one call (§2.3). Built identically by the caller
/// (to sign) and the provider (to verify) — any field the two
/// disagree on breaks the signature.
///
/// Digests, not the credentials themselves, so the transcript is
/// fixed-width apart from the request digest; the credentials travel
/// in the [`OrgCallProof`] alongside the signature.
pub struct CallBinding {
    /// Org A — whom the caller acts FOR (the dispatcher grant's
    /// org). Binding this stops a proof minted to act for one org
    /// being replayed as acting for another.
    pub acting_org: OrgId,
    /// The caller entity S (the signing key).
    pub caller: EntityId,
    /// Provider org B — the callee's PROVEN owner (from the
    /// provider's installed authority scaffold; never fold state).
    pub provider_org: OrgId,
    /// The exact callee P. The call always names an exact provider
    /// (§2.2), and the binding pins it so a proof for one provider
    /// cannot be replayed against another it happens to also cover.
    pub callee: EntityId,
    /// nRPC correlation id for this call.
    pub call_id: u64,
    /// The capability being invoked, by authority id.
    pub capability: CapabilityAuthorityId,
    /// Absolute proof expiry (unit-explicit unix NANOSECONDS — the
    /// transcript carries the unit in the field name, never a bare
    /// integer).
    pub proof_expires_at_unix_ns: u64,
    /// blake3 of the caller's membership cert canonical bytes.
    pub membership_digest: [u8; 32],
    /// blake3 of the dispatcher grant canonical bytes.
    pub dispatcher_grant_digest: [u8; 32],
    /// blake3 of the capability grant canonical bytes, or all-zero
    /// when the call carries no capability grant (same-org
    /// `OwnerDelegated` admission).
    pub capability_grant_digest: [u8; 32],
    /// blake3 of the canonical request with the `net-org-admission`
    /// header removed — the "whole canonical request minus the proof
    /// header" (§2.3), digested by
    /// [`crate::org::digest::org_request_digest`].
    pub request_digest: [u8; 32],
}

impl CallBinding {
    /// Domain-separated transcript hash. Fixed-width throughout
    /// (every field is a fixed-size integer or 32-byte value), so the
    /// concatenation is unambiguous without length prefixes. The
    /// caller's origin hash is derived from `caller` and bound
    /// implicitly (it is a pure function of the entity key); binding
    /// the full 32-byte key is strictly stronger than binding the
    /// 64-bit origin.
    fn transcript_hash(&self) -> [u8; 32] {
        // Exact 304 B (9 × 32-byte fields + two u64 LE).
        let mut buf = Vec::with_capacity(32 * 9 + 8 + 8);
        buf.extend_from_slice(self.acting_org.as_bytes());
        buf.extend_from_slice(self.caller.as_bytes());
        buf.extend_from_slice(self.provider_org.as_bytes());
        buf.extend_from_slice(self.callee.as_bytes());
        buf.extend_from_slice(&self.call_id.to_le_bytes());
        buf.extend_from_slice(self.capability.as_bytes());
        buf.extend_from_slice(&self.proof_expires_at_unix_ns.to_le_bytes());
        buf.extend_from_slice(&self.membership_digest);
        buf.extend_from_slice(&self.dispatcher_grant_digest);
        buf.extend_from_slice(&self.capability_grant_digest);
        buf.extend_from_slice(&self.request_digest);
        blake3::derive_key(ORG_CALL_BINDING_CONTEXT, &buf)
    }

    /// Sign this binding with the caller entity key, producing the
    /// [`OrgCallProof::call_binding_sig`].
    pub fn sign(&self, caller_keypair: &EntityKeypair) -> [u8; 64] {
        caller_keypair.sign(&self.transcript_hash())
    }

    /// Verify `signature` against the caller entity over this
    /// binding.
    pub fn verify(&self, signature: &[u8; 64]) -> Result<(), OrgError> {
        self.caller
            .verify(&self.transcript_hash(), signature)
            .map_err(|_| OrgError::InvalidSignature)
    }
}

/// The per-call admission proof (§2.3). Carries the caller's
/// credentials, a finite expiry, and the call-binding signature. The
/// SIGNED capability grant is carried when present (with its key
/// commitment — the raw discovery key never rides a call).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgCallProof {
    /// The caller's membership certificate (proves belonging; the
    /// binding's `caller` must equal `member`).
    pub caller_membership: OrgMembershipCert,
    /// The grant proving the caller may dispatch FOR its org.
    pub dispatcher_grant: OrgDispatcherGrant,
    /// The cross-org capability grant, when the call crosses orgs
    /// (`CrossOrgGranted`); `None` for same-org `OwnerDelegated`
    /// calls.
    pub capability_grant: Option<OrgCapabilityGrant>,
    /// Absolute proof expiry (unit-explicit unix nanoseconds).
    pub proof_expires_at_unix_ns: u64,
    /// The caller entity's signature over the [`CallBinding`].
    #[serde(with = "sig_bytes")]
    pub call_binding_sig: [u8; 64],
}

impl OrgCallProof {
    /// Assemble and sign a proof for one call. Computes the
    /// credential digests, builds the binding, and signs it with the
    /// caller key. `request_digest` is the canonical request (proof
    /// header omitted) already digested by the caller.
    ///
    /// Does not itself enforce grant validity or the expiry ceiling
    /// — issuing is the caller's, verification is
    /// [`CallBinding::verify`]'s, and the full admission order is
    /// [`crate::org::admission::verify_org_admission`]'s. (Core
    /// reads no clock here either — the expiry is signed input, so
    /// this signature needs no `now` parameter.)
    #[allow(clippy::too_many_arguments)]
    pub fn sign_for_call(
        caller_keypair: &EntityKeypair,
        caller_membership: OrgMembershipCert,
        dispatcher_grant: OrgDispatcherGrant,
        capability_grant: Option<OrgCapabilityGrant>,
        acting_org: OrgId,
        provider_org: OrgId,
        callee: EntityId,
        call_id: u64,
        capability: CapabilityAuthorityId,
        proof_expires_at_unix_ns: u64,
        request_digest: [u8; 32],
    ) -> Self {
        let binding = CallBinding {
            acting_org,
            caller: EntityId::from_bytes(*caller_keypair.entity_id()),
            provider_org,
            callee,
            call_id,
            capability,
            proof_expires_at_unix_ns,
            membership_digest: credential_digest(&caller_membership.to_bytes()),
            dispatcher_grant_digest: credential_digest(&dispatcher_grant.to_bytes()),
            capability_grant_digest: capability_grant
                .as_ref()
                .map(|g| credential_digest(&g.to_bytes()))
                .unwrap_or([0u8; 32]),
            request_digest,
        };
        let call_binding_sig = binding.sign(caller_keypair);
        Self {
            caller_membership,
            dispatcher_grant,
            capability_grant,
            proof_expires_at_unix_ns,
            call_binding_sig,
        }
    }

    /// Recompute the binding for verification against the values the
    /// PROVIDER independently knows — its own owner org and identity,
    /// the call_id it received, the `capability` id of the service it
    /// is about to dispatch, and the digest of the canonical request
    /// (proof header omitted) — digesting the credentials carried in
    /// this proof. A mismatch on any bound field surfaces as
    /// [`CallBinding::verify`] failing.
    ///
    /// `capability` is supplied by the provider (the id of the
    /// invoked service), NOT read from the proof: binding the
    /// capability the provider actually serves is what stops a proof
    /// minted for capability X being replayed against capability Y.
    /// `acting_org` comes from the caller's dispatcher grant but is
    /// bound, so tampering fails the signature.
    #[allow(clippy::too_many_arguments)]
    pub fn binding_for_verify(
        &self,
        provider_org: OrgId,
        callee: EntityId,
        call_id: u64,
        capability: CapabilityAuthorityId,
        request_digest: [u8; 32],
    ) -> CallBinding {
        CallBinding {
            acting_org: self.dispatcher_grant.org_id,
            caller: self.caller_membership.member.clone(),
            provider_org,
            callee,
            call_id,
            capability,
            proof_expires_at_unix_ns: self.proof_expires_at_unix_ns,
            membership_digest: credential_digest(&self.caller_membership.to_bytes()),
            dispatcher_grant_digest: credential_digest(&self.dispatcher_grant.to_bytes()),
            capability_grant_digest: self
                .capability_grant
                .as_ref()
                .map(|g| credential_digest(&g.to_bytes()))
                .unwrap_or([0u8; 32]),
            request_digest,
        }
    }

    /// Explicit-time expiry check (§2.3): the proof must not be
    /// expired at `now_ns`, and its claimed expiry must not exceed
    /// `now_ns + MAX_ORG_PROOF_TTL_SECS` — a proof that could first
    /// be admitted too far out is refused as a standing credential.
    /// Skew ceiling enforced. (Core's wall-clock `check_expiry`
    /// wrapper is left behind; this `_at` form is the port target.)
    pub fn check_expiry_at(&self, now_ns: u64, skew_secs: u64) -> Result<(), OrgError> {
        check_proof_expiry_at(self.proof_expires_at_unix_ns, now_ns, skew_secs)
    }

    /// Encode to the postcard bytes the `net-org-admission` header
    /// carries. Refuses to emit over the pinned ceiling.
    pub fn encode(&self) -> Result<Vec<u8>, OrgError> {
        let bytes = postcard::to_allocvec(self).map_err(|_| OrgError::InvalidFormat)?;
        if bytes.len() > MAX_ORG_CALL_PROOF_BYTES {
            return Err(OrgError::InvalidFormat);
        }
        Ok(bytes)
    }

    /// PREFIX-TOLERANT decode from header bytes (the frozen
    /// old-provider behavior — a streaming proof's five leading
    /// fields decode here and its suffix is ignored). Over-cap input
    /// is refused before parsing.
    pub fn decode(bytes: &[u8]) -> Result<Self, OrgError> {
        if bytes.len() > MAX_ORG_CALL_PROOF_BYTES {
            return Err(OrgError::InvalidFormat);
        }
        postcard::from_bytes(bytes).map_err(|_| OrgError::InvalidFormat)
    }
}

impl core::fmt::Debug for OrgCallProof {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("OrgCallProof")
            .field("caller", &self.caller_membership.member)
            .field("acting_org", &self.dispatcher_grant.org_id)
            .field("has_capability_grant", &self.capability_grant.is_some())
            .field("proof_expires_at_unix_ns", &self.proof_expires_at_unix_ns)
            .finish()
    }
}

/// Expiry + TTL-ceiling check with skew, shared by the unary and
/// streaming proofs (§2.3): the proof must not be expired, and its
/// claimed expiry must not exceed `now + MAX_ORG_PROOF_TTL_SECS`.
/// `pub` as the shared pin surface both proofs and the engine name.
pub fn check_proof_expiry_at(
    proof_expires_at_unix_ns: u64,
    now_ns: u64,
    skew_secs: u64,
) -> Result<(), OrgError> {
    if skew_secs > crate::org::MAX_TOKEN_CLOCK_SKEW_SECS {
        return Err(OrgError::ClockSkewTooLarge);
    }
    let skew_ns = skew_secs.saturating_mul(1_000_000_000);
    if now_ns >= proof_expires_at_unix_ns.saturating_add(skew_ns) {
        return Err(OrgError::Expired);
    }
    let ceiling_ns = now_ns
        .saturating_add(MAX_ORG_PROOF_TTL_SECS.saturating_mul(1_000_000_000))
        .saturating_add(skew_ns);
    if proof_expires_at_unix_ns > ceiling_ns {
        return Err(OrgError::TtlTooLong);
    }
    Ok(())
}

/// The streaming four-party call binding (streaming plan §1.2): the
/// 11 [`CallBinding`] fields PLUS `kind` and `session_binding`, fixed
/// width, 337 B, hashed under
/// [`ORG_STREAM_CALL_BINDING_CONTEXT`].
///
/// Binding `kind` makes the signed shape un-repointable (a
/// server-streaming opening cannot be replayed as a duplex call);
/// binding `session_binding` ties the opening to ONE exact Noise
/// session, so a captured opening replayed on a later session fails
/// the signature AND the session comparison.
pub struct StreamCallBinding {
    /// Org A — whom the caller acts FOR (as
    /// [`CallBinding::acting_org`]).
    pub acting_org: OrgId,
    /// The caller entity S (the signing key).
    pub caller: EntityId,
    /// Provider org B — the callee's PROVEN owner.
    pub provider_org: OrgId,
    /// The exact callee P.
    pub callee: EntityId,
    /// nRPC correlation id for this call.
    pub call_id: u64,
    /// The capability being invoked, by authority id.
    pub capability: CapabilityAuthorityId,
    /// Absolute proof expiry (unix NANOSECONDS).
    pub proof_expires_at_unix_ns: u64,
    /// blake3 of the caller's membership cert canonical bytes.
    pub membership_digest: [u8; 32],
    /// blake3 of the dispatcher grant canonical bytes.
    pub dispatcher_grant_digest: [u8; 32],
    /// blake3 of the capability grant canonical bytes, or all-zero
    /// when absent (same-org `OwnerDelegated`).
    pub capability_grant_digest: [u8; 32],
    /// blake3 of the canonical request with the `net-org-admission`
    /// header removed.
    pub request_digest: [u8; 32],
    /// The streaming call kind (1 SS / 2 CS / 3 DX; 0 is never
    /// emitted and [`OrgStreamCallProof::decode`] rejects it).
    pub kind: u8,
    /// The full Noise handshake hash of the ONE session this opening
    /// rides (streaming plan §1.3) — raw hash, no HKDF label (the
    /// derive_key context string is the domain separation).
    pub session_binding: [u8; 32],
}

impl StreamCallBinding {
    /// Domain-separated transcript hash. Fixed width (337 B: the
    /// 304-B unary field set + 1-byte kind + 32-byte session
    /// binding), so the concatenation is unambiguous without length
    /// prefixes.
    fn transcript_hash(&self) -> [u8; 32] {
        let mut buf = Vec::with_capacity(32 * 9 + 8 + 8 + 1 + 32);
        buf.extend_from_slice(self.acting_org.as_bytes());
        buf.extend_from_slice(self.caller.as_bytes());
        buf.extend_from_slice(self.provider_org.as_bytes());
        buf.extend_from_slice(self.callee.as_bytes());
        buf.extend_from_slice(&self.call_id.to_le_bytes());
        buf.extend_from_slice(self.capability.as_bytes());
        buf.extend_from_slice(&self.proof_expires_at_unix_ns.to_le_bytes());
        buf.extend_from_slice(&self.membership_digest);
        buf.extend_from_slice(&self.dispatcher_grant_digest);
        buf.extend_from_slice(&self.capability_grant_digest);
        buf.extend_from_slice(&self.request_digest);
        buf.push(self.kind);
        buf.extend_from_slice(&self.session_binding);
        blake3::derive_key(ORG_STREAM_CALL_BINDING_CONTEXT, &buf)
    }

    /// Sign this binding with the caller entity key, producing the
    /// [`OrgStreamCallProof::call_binding_sig`].
    pub fn sign(&self, caller_keypair: &EntityKeypair) -> [u8; 64] {
        caller_keypair.sign(&self.transcript_hash())
    }

    /// Verify `signature` against the caller entity over this
    /// binding.
    pub fn verify(&self, signature: &[u8; 64]) -> Result<(), OrgError> {
        self.caller
            .verify(&self.transcript_hash(), signature)
            .map_err(|_| OrgError::InvalidSignature)
    }
}

/// The per-call admission proof for a STREAMING call (streaming plan
/// §1.1). Wire-**prefix-compatible** with [`OrgCallProof`]: the same
/// five leading fields in the same order (so the frozen old decoder
/// consumes the prefix and ignores the suffix — the §1.4
/// mixed-version argument), followed by `kind: u8` and
/// `session_binding: [u8; 32]`.
///
/// Unlike [`OrgCallProof::decode`], [`Self::decode`] consumes the
/// ENTIRE bounded value: unknown kinds, truncated suffixes and extra
/// trailing bytes are all refused. Historical unary prefix tolerance
/// exists only on the frozen old-provider side.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgStreamCallProof {
    /// The caller's membership certificate (as [`OrgCallProof`]).
    pub caller_membership: OrgMembershipCert,
    /// The grant proving the caller may dispatch FOR its org.
    pub dispatcher_grant: OrgDispatcherGrant,
    /// The cross-org capability grant, when the call crosses orgs.
    pub capability_grant: Option<OrgCapabilityGrant>,
    /// Absolute proof expiry (unit-explicit unix nanoseconds).
    pub proof_expires_at_unix_ns: u64,
    /// The caller entity's signature over the [`StreamCallBinding`].
    #[serde(with = "sig_bytes")]
    pub call_binding_sig: [u8; 64],
    /// The streaming call kind: 1 SS / 2 CS / 3 DX (0 is never
    /// emitted).
    pub kind: u8,
    /// The Noise handshake hash of the ONE session this opening
    /// rides.
    pub session_binding: [u8; 32],
}

impl OrgStreamCallProof {
    /// Assemble and sign a streaming proof for one call (the
    /// streaming counterpart of [`OrgCallProof::sign_for_call`]).
    #[allow(clippy::too_many_arguments)]
    pub fn sign_for_stream_call(
        caller_keypair: &EntityKeypair,
        caller_membership: OrgMembershipCert,
        dispatcher_grant: OrgDispatcherGrant,
        capability_grant: Option<OrgCapabilityGrant>,
        acting_org: OrgId,
        provider_org: OrgId,
        callee: EntityId,
        call_id: u64,
        capability: CapabilityAuthorityId,
        proof_expires_at_unix_ns: u64,
        request_digest: [u8; 32],
        kind: u8,
        session_binding: [u8; 32],
    ) -> Self {
        let binding = StreamCallBinding {
            acting_org,
            caller: EntityId::from_bytes(*caller_keypair.entity_id()),
            provider_org,
            callee,
            call_id,
            capability,
            proof_expires_at_unix_ns,
            membership_digest: credential_digest(&caller_membership.to_bytes()),
            dispatcher_grant_digest: credential_digest(&dispatcher_grant.to_bytes()),
            capability_grant_digest: capability_grant
                .as_ref()
                .map(|g| credential_digest(&g.to_bytes()))
                .unwrap_or([0u8; 32]),
            request_digest,
            kind,
            session_binding,
        };
        let call_binding_sig = binding.sign(caller_keypair);
        Self {
            caller_membership,
            dispatcher_grant,
            capability_grant,
            proof_expires_at_unix_ns,
            call_binding_sig,
            kind,
            session_binding,
        }
    }

    /// Recompute the streaming binding for verification against the
    /// values the PROVIDER independently knows (as
    /// [`OrgCallProof::binding_for_verify`], plus the proof's own
    /// `kind`/`session_binding`, which the provider separately
    /// checks against the call's flags and the RECEIVING session).
    #[allow(clippy::too_many_arguments)]
    pub fn binding_for_stream_verify(
        &self,
        provider_org: OrgId,
        callee: EntityId,
        call_id: u64,
        capability: CapabilityAuthorityId,
        request_digest: [u8; 32],
    ) -> StreamCallBinding {
        StreamCallBinding {
            acting_org: self.dispatcher_grant.org_id,
            caller: self.caller_membership.member.clone(),
            provider_org,
            callee,
            call_id,
            capability,
            proof_expires_at_unix_ns: self.proof_expires_at_unix_ns,
            membership_digest: credential_digest(&self.caller_membership.to_bytes()),
            dispatcher_grant_digest: credential_digest(&self.dispatcher_grant.to_bytes()),
            capability_grant_digest: self
                .capability_grant
                .as_ref()
                .map(|g| credential_digest(&g.to_bytes()))
                .unwrap_or([0u8; 32]),
            request_digest,
            kind: self.kind,
            session_binding: self.session_binding,
        }
    }

    /// The five unary prefix fields as an [`OrgCallProof`] — the view
    /// the provider-local policy closure sees for a streaming proof
    /// (the policy keeps its unary signature; `kind` and
    /// `session_binding` are shape/session facts, not credentials).
    pub fn unary_prefix(&self) -> OrgCallProof {
        OrgCallProof {
            caller_membership: self.caller_membership.clone(),
            dispatcher_grant: self.dispatcher_grant.clone(),
            capability_grant: self.capability_grant.clone(),
            proof_expires_at_unix_ns: self.proof_expires_at_unix_ns,
            call_binding_sig: self.call_binding_sig,
        }
    }

    /// Explicit-time expiry check (§2.3), as
    /// [`OrgCallProof::check_expiry_at`].
    pub fn check_expiry_at(&self, now_ns: u64, skew_secs: u64) -> Result<(), OrgError> {
        check_proof_expiry_at(self.proof_expires_at_unix_ns, now_ns, skew_secs)
    }

    /// Encode to the postcard bytes the `net-org-admission` header
    /// carries. Refuses to emit over the pinned ceiling.
    pub fn encode(&self) -> Result<Vec<u8>, OrgError> {
        let bytes = postcard::to_allocvec(self).map_err(|_| OrgError::InvalidFormat)?;
        if bytes.len() > MAX_ORG_CALL_PROOF_BYTES {
            return Err(OrgError::InvalidFormat);
        }
        Ok(bytes)
    }

    /// STRICT streaming decode (§1.1): over-cap input is refused
    /// before parsing; truncation fails to decode; unknown `kind` (0
    /// or > 3) is refused; and the decoder consumes the ENTIRE
    /// bounded value — trailing bytes after the suffix are refused,
    /// never ignored.
    pub fn decode(bytes: &[u8]) -> Result<Self, OrgError> {
        if bytes.len() > MAX_ORG_CALL_PROOF_BYTES {
            return Err(OrgError::InvalidFormat);
        }
        let (proof, rest) =
            postcard::take_from_bytes::<Self>(bytes).map_err(|_| OrgError::InvalidFormat)?;
        if !rest.is_empty() {
            return Err(OrgError::InvalidFormat);
        }
        if RpcCallShape::from_stream_kind(proof.kind).is_none() {
            return Err(OrgError::InvalidFormat);
        }
        Ok(proof)
    }
}

impl core::fmt::Debug for OrgStreamCallProof {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("OrgStreamCallProof")
            .field("caller", &self.caller_membership.member)
            .field("acting_org", &self.dispatcher_grant.org_id)
            .field("has_capability_grant", &self.capability_grant.is_some())
            .field("proof_expires_at_unix_ns", &self.proof_expires_at_unix_ns)
            .field("kind", &self.kind)
            .finish()
    }
}

/// postcard codec for the 64-byte signature (serde has no default
/// for `[u8; 64]`): `serialize_bytes` = varint length + 64 raw
/// bytes (65 on the wire), decoded through `Vec<u8>` with an exact
/// length check.
mod sig_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    /// Serialize as length-prefixed bytes.
    pub fn serialize<S: Serializer>(sig: &[u8; 64], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(sig)
    }

    /// Deserialize a length-prefixed 64-byte value.
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 64], D::Error> {
        let v = <Vec<u8>>::deserialize(d)?;
        v.as_slice()
            .try_into()
            .map_err(|_| serde::de::Error::custom("signature must be 64 bytes"))
    }
}
