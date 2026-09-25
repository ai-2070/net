//! VERBATIM vendoring of `net/crates/net/src/adapter/net/behavior/org_call.rs`
//! **lines 50–359 at revision `85ecc77c9`** — the frozen old-provider
//! [`OrgCallProof`] (with its prefix-tolerant `postcard::from_bytes` decode),
//! [`CallBinding`], the `"net-org-call-v1"` transcript, and the codec consts.
//!
//! Provenance (byte-identical; the appended body below is this extraction):
//! `git show 85ecc77c9:net/crates/net/src/adapter/net/behavior/org_call.rs |
//! sed -n '50,359p'` → sha256
//! `64adefbc5b30b10ba75186572efab97a51aefcd4ce23d8b8541835d1d72f4d2c`.
//!
//! Adaptations (imports ONLY, zero body lines changed): the original `use`
//! block (`org_call.rs:43-48` at that revision) is retargeted from
//! `super::org`/`super::org_grant`/`crate::adapter::net::identity` to the
//! test crate's `net::…` paths — EXCEPT `org::current_timestamp`, which is
//! `pub(crate)` in the library and is therefore supplied below as the
//! byte-identical one-liner its definition carries (`org.rs:963-967`).

#![allow(
    dead_code,
    reason = "verbatim vendoring of the frozen old-provider surface: items beyond the \
              witnessed decode/verify path are retained intact, never pruned"
)]

use ed25519_dalek::Signature;
use serde::{Deserialize, Serialize};

use net::adapter::net::behavior::org::{OrgError, OrgId, OrgMembershipCert};
use net::adapter::net::behavior::org_grant::{
    CapabilityAuthorityId, OrgCapabilityGrant, OrgDispatcherGrant,
};
use net::adapter::net::identity::{EntityId, EntityKeypair, MAX_TOKEN_CLOCK_SKEW_SECS};

/// Adaptation (above): the frozen `org::current_timestamp` body, verbatim
/// from `org.rs:963-967` (`pub(crate)` there, so it cannot be imported).
fn current_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
/// blake3 `derive_key` context for the call-binding transcript
/// hash (plan §2.3). A signing domain: bytes hashed under this
/// context can never be confused with a grant, cert, or floor
/// bundle transcript.
pub const ORG_CALL_BINDING_CONTEXT: &str = "net-org-call-v1";

/// The well-known RPC header the proof rides on (§2.4 enforces
/// EXACTLY ONE or deny).
pub const ORG_ADMISSION_HEADER: &str = "net-org-admission";

/// Ceiling on how far in the future a proof may claim to expire,
/// measured from now at verify time (§2.3: "FINITE, always"). A
/// proof that could FIRST be admitted arbitrarily far in the
/// future is a standing replayable credential; this bounds the
/// window independently of the RPC deadline. 30 s provisional
/// (plan Q3 — the callee `AdmissionReplayConfig` is authoritative
/// and this freezes only after OA-2 measurement).
pub const MAX_ORG_PROOF_TTL_SECS: u64 = 30;

/// Upper bound on the postcard-encoded proof, asserted against the
/// RPC header value cap (`MAX_RPC_HEADER_VALUE_LEN = 4096`).
/// membership 156 + dispatcher grant 185 + capability grant 318 +
/// expiry 8 + sig 64 = 731 raw; postcard length prefixes and the
/// option tag add a handful of bytes. 1024 is a comfortable pin
/// well under 4096.
pub const MAX_ORG_CALL_PROOF_BYTES: usize = 1024;

/// The digest of a credential's canonical wire bytes, bound into
/// the transcript so a proof cannot be re-pointed at a different
/// (but individually valid) credential without breaking the
/// signature.
fn credential_digest(bytes: &[u8]) -> [u8; 32] {
    blake3::hash(bytes).into()
}

/// The four-party call binding: everything the caller ENTITY signs
/// to authorize exactly one call (§2.3). Built identically by the
/// caller (to sign) and the provider (to verify) — any field the
/// two disagree on breaks the signature.
///
/// Digests, not the credentials themselves, so the transcript is
/// fixed-width apart from the request digest; the credentials
/// travel in the [`OrgCallProof`] alongside the signature.
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
    /// The exact callee P. The call always names an exact
    /// provider (§2.2), and the binding pins it so a proof for one
    /// provider cannot be replayed against another it happens to
    /// also cover.
    pub callee: EntityId,
    /// nRPC correlation id for this call.
    pub call_id: u64,
    /// The capability being invoked, by authority id.
    pub capability: CapabilityAuthorityId,
    /// Absolute proof expiry (unit-explicit unix NANOSECONDS —
    /// §2.12 wire-honesty: the transcript carries the unit in the
    /// field name, never a bare integer).
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
    /// header removed — the "whole canonical request minus the
    /// proof header" (§2.3), digested at the cortex layer.
    pub request_digest: [u8; 32],
}

impl CallBinding {
    /// Domain-separated transcript hash. Fixed-width throughout
    /// (every field is a fixed-size integer or 32-byte value), so
    /// the concatenation is unambiguous without length prefixes —
    /// there is no variable-length member. The caller's origin
    /// hash is derived from `caller` and bound implicitly (it is a
    /// pure function of the entity key); binding the full 32-byte
    /// key is strictly stronger than binding the 64-bit origin.
    fn transcript_hash(&self) -> [u8; 32] {
        let mut buf = Vec::with_capacity(32 * 6 + 8 + 8 + 32);
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
        caller_keypair.sign(&self.transcript_hash()).to_bytes()
    }

    /// Verify `signature` against the caller entity over this
    /// binding.
    pub fn verify(&self, signature: &[u8; 64]) -> Result<(), OrgError> {
        let sig = Signature::from_bytes(signature);
        self.caller
            .verify(&self.transcript_hash(), &sig)
            .map_err(|_| OrgError::InvalidSignature)
    }
}

/// The per-call admission proof (§2.3). Carries the caller's
/// credentials, a finite expiry, and the call-binding signature.
/// The SIGNED capability grant is carried when present (with its
/// key commitment — the raw discovery key never rides a call).
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
    /// credential digests, builds the binding, and signs it with
    /// the caller key. `request_digest` is the canonical request
    /// (proof header omitted) already digested by the caller.
    ///
    /// Does not itself enforce grant validity or the expiry
    /// ceiling — issuing is the caller's, verification is
    /// [`CallBinding::verify`]'s, and the full admission order is §2.4's.
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
            caller: caller_keypair.entity_id().clone(),
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

    /// Recompute the binding for verification against the values
    /// the PROVIDER independently knows — its own owner org and
    /// identity, the call_id it received, the `capability` id of
    /// the service it is about to dispatch, and the digest of the
    /// canonical request (proof header omitted) — digesting the
    /// credentials carried in this proof. A mismatch on any bound
    /// field surfaces as [`CallBinding::verify`] failing.
    ///
    /// `capability` is supplied by the provider (the id of the
    /// invoked service), NOT read from the proof: binding the
    /// capability the provider actually serves is what stops a
    /// proof minted for capability X being replayed against
    /// capability Y. `acting_org` comes from the caller's
    /// dispatcher grant but is bound, so tampering fails the
    /// signature.
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

    /// Wall-clock expiry check with skew (§2.3): the proof must
    /// not be expired, and its claimed expiry must not exceed
    /// `now + MAX_ORG_PROOF_TTL_SECS` — a proof that could first
    /// be admitted too far out is refused as a standing
    /// credential. Skew ceiling enforced.
    pub fn check_expiry(&self, skew_secs: u64) -> Result<(), OrgError> {
        self.check_expiry_at(current_timestamp().saturating_mul(1_000_000_000), skew_secs)
    }

    /// Explicit-time variant (Kyra E1 audit): check expiry + the TTL
    /// ceiling against a caller-supplied `now_ns` (unix nanoseconds)
    /// instead of re-reading the wall clock, so one admission uses a
    /// single clock sample.
    pub fn check_expiry_at(&self, now_ns: u64, skew_secs: u64) -> Result<(), OrgError> {
        if skew_secs > MAX_TOKEN_CLOCK_SKEW_SECS {
            return Err(OrgError::ClockSkewTooLarge);
        }
        let skew_ns = skew_secs.saturating_mul(1_000_000_000);
        if now_ns >= self.proof_expires_at_unix_ns.saturating_add(skew_ns) {
            return Err(OrgError::Expired);
        }
        let ceiling_ns = now_ns
            .saturating_add(MAX_ORG_PROOF_TTL_SECS.saturating_mul(1_000_000_000))
            .saturating_add(skew_ns);
        if self.proof_expires_at_unix_ns > ceiling_ns {
            return Err(OrgError::TtlTooLong);
        }
        Ok(())
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

    /// Strict decode from header bytes. Over-cap input is refused
    /// before allocation-heavy parsing.
    pub fn decode(bytes: &[u8]) -> Result<Self, OrgError> {
        if bytes.len() > MAX_ORG_CALL_PROOF_BYTES {
            return Err(OrgError::InvalidFormat);
        }
        postcard::from_bytes(bytes).map_err(|_| OrgError::InvalidFormat)
    }
}

impl std::fmt::Debug for OrgCallProof {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OrgCallProof")
            .field("caller", &self.caller_membership.member)
            .field("acting_org", &self.dispatcher_grant.org_id)
            .field("has_capability_grant", &self.capability_grant.is_some())
            .field("proof_expires_at_unix_ns", &self.proof_expires_at_unix_ns)
            .finish()
    }
}

/// postcard codec for the 64-byte signature (serde has no default
/// for `[u8; 64]`).
mod sig_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(sig: &[u8; 64], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(sig)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 64], D::Error> {
        let v = <Vec<u8>>::deserialize(d)?;
        v.as_slice()
            .try_into()
            .map_err(|_| serde::de::Error::custom("signature must be 64 bytes"))
    }
}
