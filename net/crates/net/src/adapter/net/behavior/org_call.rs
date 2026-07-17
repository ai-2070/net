//! OA-2 §2.3 of `docs/plans/ORG_CAPABILITY_AUTH_PLAN.md` — the
//! per-call admission proof and its four-party call binding.
//!
//! An [`OrgCallProof`] rides exactly one `net-org-admission` RPC
//! header (§2.4 enforces the exactly-one rule). It carries the
//! caller's membership certificate, the dispatcher grant proving
//! the caller acts FOR its org, an optional cross-org capability
//! grant, a FINITE proof expiry, and a signature over the
//! [`CallBinding`] transcript.
//!
//! # What the binding proves
//!
//! The transcript (`"net-org-call-v1"`) is the caller ENTITY's
//! ed25519 signature over the full four-party attribution plus the
//! exact call it authorizes:
//!
//! ```text
//! acting org A ‖ caller S + origin ‖ provider org B ‖ exact callee P
//!   ‖ call_id ‖ capability C ‖ proof expiry (ns)
//!   ‖ digest(membership) ‖ digest(dispatcher grant)
//!   ‖ digest(capability grant)   (zero when absent)
//!   ‖ digest(canonical request, proof header omitted)
//! ```
//!
//! A relay, or a previously-authorized-now-revoked peer, replaying
//! a captured proof against a DIFFERENT call, callee, capability,
//! acting org, or request body fails the binding — the signature
//! covers all of them. The proof expiry bounds the window for a
//! byte-identical resend; the replay guard (§2.5) closes that.
//!
//! # Layering
//!
//! This module lives at the behavior layer and never imports the
//! cortex RPC types. The "canonical request minus the proof
//! header" is passed in as a caller-computed `request_digest`
//! ([`CallBinding::request_digest`]) — the §2.4 admission engine,
//! which sits at the cortex/mesh layer where `RpcRequestPayload`
//! is available, computes that digest with the `net-org-admission`
//! header removed and hands it here. The raw discovery key
//! (`OrgAudienceSecret`) is NEVER a member of the proof — grants
//! carry only commitments (§2.2).

use ed25519_dalek::Signature;
use serde::{Deserialize, Serialize};

use super::org::{current_timestamp, OrgError, OrgId, OrgMembershipCert};
use super::org_grant::{CapabilityAuthorityId, OrgCapabilityGrant, OrgDispatcherGrant};
use crate::adapter::net::identity::{EntityId, EntityKeypair, MAX_TOKEN_CLOCK_SKEW_SECS};

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
    /// [`Self::verify`]'s, and the full admission order is §2.4's.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::org::OrgKeypair;
    use crate::adapter::net::behavior::org_grant::{
        DispatcherScope, GrantRights, GrantTargetScope,
    };

    fn org_a() -> OrgKeypair {
        OrgKeypair::from_bytes([0x77u8; 32])
    }

    fn org_b() -> OrgKeypair {
        OrgKeypair::from_bytes([0x42u8; 32])
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

    /// A live cross-org proof: membership (A), dispatcher grant
    /// (A→caller, exact cap), capability grant (B→A, INVOKE).
    fn build_cross_org_proof(call_id: u64, request_digest: [u8; 32]) -> (OrgCallProof, u64) {
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
        let (capability_grant, _secret) = OrgCapabilityGrant::try_issue(
            &org_b(),
            org_a().org_id(),
            cap(),
            GrantRights::INVOKE,
            GrantTargetScope::ExactNode(provider()),
            3600,
        )
        .expect("cap grant");
        let expiry = (current_timestamp() + 20) * 1_000_000_000;
        let proof = OrgCallProof::sign_for_call(
            &caller,
            membership,
            dispatcher,
            Some(capability_grant),
            org_a().org_id(),
            org_b().org_id(),
            provider(),
            call_id,
            cap(),
            expiry,
            request_digest,
        );
        (proof, expiry)
    }

    #[test]
    fn proof_binds_and_verifies_against_the_exact_call() {
        let digest = [0x11u8; 32];
        let (proof, _) = build_cross_org_proof(42, digest);
        let binding = proof.binding_for_verify(org_b().org_id(), provider(), 42, cap(), digest);
        binding
            .verify(&proof.call_binding_sig)
            .expect("binding verifies for the exact call");
        proof.check_expiry(0).expect("live");
    }

    #[test]
    fn binding_transplant_matrix_every_bound_field_is_load_bearing() {
        let digest = [0x11u8; 32];
        let (proof, _) = build_cross_org_proof(42, digest);

        // Wrong call_id.
        assert!(proof
            .binding_for_verify(org_b().org_id(), provider(), 43, cap(), digest)
            .verify(&proof.call_binding_sig)
            .is_err());
        // Wrong callee.
        assert!(proof
            .binding_for_verify(
                org_b().org_id(),
                EntityId::from_bytes([7u8; 32]),
                42,
                cap(),
                digest
            )
            .verify(&proof.call_binding_sig)
            .is_err());
        // Wrong provider org.
        assert!(proof
            .binding_for_verify(org_a().org_id(), provider(), 42, cap(), digest)
            .verify(&proof.call_binding_sig)
            .is_err());
        // Wrong request digest (different request body / headers).
        assert!(proof
            .binding_for_verify(org_b().org_id(), provider(), 42, cap(), [0x22u8; 32])
            .verify(&proof.call_binding_sig)
            .is_err());
    }

    #[test]
    fn tampering_a_carried_credential_breaks_the_binding() {
        let digest = [0x11u8; 32];
        let (mut proof, _) = build_cross_org_proof(42, digest);
        // Swap in a different (individually valid) capability
        // grant: the digest bound into the signature no longer
        // matches.
        let (other_grant, _) = OrgCapabilityGrant::try_issue(
            &org_b(),
            org_a().org_id(),
            cap(),
            GrantRights::INVOKE,
            GrantTargetScope::AnyNodeOwnedBy(org_b().org_id()),
            3600,
        )
        .expect("other grant");
        proof.capability_grant = Some(other_grant);
        assert!(proof
            .binding_for_verify(org_b().org_id(), provider(), 42, cap(), digest)
            .verify(&proof.call_binding_sig)
            .is_err());
    }

    #[test]
    fn wrong_caller_key_never_verifies() {
        let digest = [0x11u8; 32];
        let (proof, expiry) = build_cross_org_proof(42, digest);
        // A different caller signs the same binding fields; the
        // proof's `caller` (from membership.member) is the real
        // caller, so verification uses the real key and the forged
        // signature fails.
        let attacker = EntityKeypair::from_bytes([0xEEu8; 32]);
        let forged_binding = CallBinding {
            acting_org: org_a().org_id(),
            caller: proof.caller_membership.member.clone(),
            provider_org: org_b().org_id(),
            callee: provider(),
            call_id: 42,
            capability: cap(),
            proof_expires_at_unix_ns: expiry,
            membership_digest: credential_digest(&proof.caller_membership.to_bytes()),
            dispatcher_grant_digest: credential_digest(&proof.dispatcher_grant.to_bytes()),
            capability_grant_digest: proof
                .capability_grant
                .as_ref()
                .map(|g| credential_digest(&g.to_bytes()))
                .unwrap(),
            request_digest: digest,
        };
        let forged_sig = attacker.sign(&forged_binding.transcript_hash()).to_bytes();
        assert!(forged_binding.verify(&forged_sig).is_err(), "wrong key");
    }

    #[test]
    fn expiry_ceiling_and_expired_are_refused() {
        let caller = caller();
        let membership =
            OrgMembershipCert::try_issue(&org_a(), caller.entity_id().clone(), 1, 3600)
                .expect("cert");
        let dispatcher = OrgDispatcherGrant::try_issue(
            &org_a(),
            caller.entity_id().clone(),
            DispatcherScope::Any,
            3600,
        )
        .expect("dispatcher");
        let mk = |expiry_ns: u64| {
            OrgCallProof::sign_for_call(
                &caller,
                membership.clone(),
                dispatcher.clone(),
                None,
                org_a().org_id(),
                org_b().org_id(),
                provider(),
                1,
                cap(),
                expiry_ns,
                [0u8; 32],
            )
        };
        // Already expired.
        let past = (current_timestamp().saturating_sub(10)) * 1_000_000_000;
        assert!(matches!(mk(past).check_expiry(0), Err(OrgError::Expired)));
        // Too far in the future (> MAX_ORG_PROOF_TTL_SECS).
        let far = (current_timestamp() + MAX_ORG_PROOF_TTL_SECS + 60) * 1_000_000_000;
        assert!(matches!(mk(far).check_expiry(0), Err(OrgError::TtlTooLong)));
        // Just inside the ceiling.
        let ok = (current_timestamp() + 5) * 1_000_000_000;
        mk(ok).check_expiry(0).expect("live");
        // Skew ceiling.
        assert!(matches!(
            mk(ok).check_expiry(MAX_TOKEN_CLOCK_SKEW_SECS + 1),
            Err(OrgError::ClockSkewTooLarge)
        ));
    }

    #[test]
    fn proof_codec_roundtrips_and_stays_under_the_header_cap() {
        let (proof, _) = build_cross_org_proof(42, [0x11u8; 32]);
        let bytes = proof.encode().expect("encode");
        assert!(
            bytes.len() <= MAX_ORG_CALL_PROOF_BYTES,
            "encoded proof {} exceeds cap",
            bytes.len()
        );
        // Comfortably under the RPC header value cap too.
        assert!(bytes.len() < 4096);
        let decoded = OrgCallProof::decode(&bytes).expect("decode");
        assert_eq!(decoded, proof);
        // The decoded proof still verifies against the same call.
        decoded
            .binding_for_verify(org_b().org_id(), provider(), 42, cap(), [0x11u8; 32])
            .verify(&decoded.call_binding_sig)
            .expect("decoded verifies");
    }

    #[test]
    fn same_org_proof_carries_no_capability_grant() {
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
        let expiry = (current_timestamp() + 10) * 1_000_000_000;
        let proof = OrgCallProof::sign_for_call(
            &caller,
            membership,
            dispatcher,
            None,
            org_a().org_id(),
            org_a().org_id(), // same org: provider owned by A
            provider(),
            7,
            cap(),
            expiry,
            [0u8; 32],
        );
        assert!(proof.capability_grant.is_none());
        // The zero capability-grant digest is bound; verification
        // reconstructs it identically.
        proof
            .binding_for_verify(org_a().org_id(), provider(), 7, cap(), [0u8; 32])
            .verify(&proof.call_binding_sig)
            .expect("same-org binding verifies");
        let bytes = proof.encode().expect("encode");
        assert_eq!(OrgCallProof::decode(&bytes).expect("decode"), proof);
    }

    #[test]
    fn decode_refuses_oversized_input() {
        assert!(matches!(
            OrgCallProof::decode(&vec![0u8; MAX_ORG_CALL_PROOF_BYTES + 1]),
            Err(OrgError::InvalidFormat)
        ));
    }
}
