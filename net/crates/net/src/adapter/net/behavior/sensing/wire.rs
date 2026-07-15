//! SI-1 wire layer: committed subprotocol ids, the frozen postcard
//! codec for the 0x0C02/0x0C03 payloads, and attestation signing +
//! verification honoring the §4.2 transcript invariant
//! (`docs/plans/SENSING_INTEREST_COALESCING_PLAN.md`, v4.3).
//!
//! # Codec (postcard, strict)
//!
//! postcard is the tree's canonical wire codec (fold, group, subnet,
//! RedEX); both sensing payloads encode through the serde derives on
//! the semantic types. Decoding is **strict**: the payload must be
//! exactly one object — trailing bytes are rejected
//! ([`WireError::TrailingBytes`], via `postcard::take_from_bytes`
//! with an empty-rest requirement), and payloads over
//! [`MAX_SENSING_FRAME_BYTES`] (4 KiB) are rejected before parsing.
//! 4 KiB comfortably bounds every legal frame: inline constraints
//! are capped at 1 KiB (plan §5) and everything else is small;
//! anything larger is malformed or hostile.
//!
//! Encoding note: the 32-byte identity newtypes ([`Digest256`],
//! [`AudienceScopeCommitment`]) split on `is_human_readable` — raw
//! 33-byte `serialize_bytes` under postcard (this codec), lowercase
//! hex under JSON-style encodings (see `impl_hex32_serde` in
//! `identity.rs`). The split landed BEFORE any deployment existed
//! (the SI-1 as-built note records the hex-first history), so it was
//! an encoding choice, not a wire break.
//!
//! # Signature transcript (encoding-independent)
//!
//! The [`ReadinessAttestation`] signature never signs postcard
//! bytes: like the digest preimages in `identity.rs`, the transcript
//! is a hand-rolled, domain-separated, injective byte string —
//! length-prefixed where variable, fixed-width everywhere else — so
//! a codec migration can never invalidate old signatures or let two
//! field tuples collide. It binds EXACTLY the §4.2 list: protocol
//! domain/version (the [`ATTESTATION_SIG_DOMAIN`] derive-key
//! context), interest digest, origin NodeId, origin incarnation,
//! capability id, capability generation, status + reason, estimated
//! start, sequence, promised cadence, audience scope. Because the
//! provider validated the interest digest first
//! ([`SensingInterestFrame::validate_provider_registration`]),
//! signing it commits the attestation to the complete predicate +
//! selector + mode + disclosure + audience identity.
//!
//! **What is signed:** ed25519 over the 32-byte
//! `blake3::derive_key(ATTESTATION_SIG_DOMAIN, transcript)` digest —
//! not over the raw transcript. Chosen so the signing input is
//! constant-size at any fan-out, the domain separation rides the
//! derive-key context (with the `v1` version inside it), and the
//! same 32 bytes double as the attestation's semantic fingerprint
//! ([`semantic_attestation`]): the SI-0 stand-in fingerprint is now
//! the real signed-bytes digest, so equivocation detection at the
//! seq gate keys on exactly what the origin signed.

use std::fmt;
use std::time::Duration;

use super::super::super::identity::{EntityError, EntityId, EntityKeypair};
use super::super::capability::Signature64;
use super::continuity::AttestedStatus;
use super::delivery::Attestation;
use super::evaluator::StatusReason;
use super::frames::SensingInterestFrame;
use super::identity::{
    AudienceScopeCommitment, CapabilityId, CapabilityInterestKey, Digest256, ProviderObservationKey,
};
use super::incarnation::Incarnation;

/// Subprotocol id for [`SensingInterestFrame`] payloads —
/// **committed** per the review-7 sign-off (plan v4.3 Status block:
/// gates (a)–(s) verified, 0x0C02 MAY be committed). Sensing-owned;
/// cross-referenced beside 0x0C00/0x0C01 in `behavior::broadcast`.
///
/// Mixed-version caveat (as 0x0C01): a node that does not know this
/// id drops the packet at the dispatch loop's unknown-subprotocol
/// guard and keeps pre-sensing behavior (per-branch fallback, plan
/// §4.11) — but binaries older than that guard itself would
/// mis-handle the frame as an opaque application event, so a true
/// mixed-version deployment needs peers new enough to have the
/// guard.
pub const SUBPROTOCOL_SENSING_INTEREST: u16 = 0x0C02;

/// Subprotocol id for [`ReadinessAttestation`] payloads —
/// **committed** per the review-7 sign-off, exactly as
/// [`SUBPROTOCOL_SENSING_INTEREST`] (same mixed-version caveat).
pub const SUBPROTOCOL_READINESS_ATTESTATION: u16 = 0x0C03;

/// SI-4: the session stream a PROVISIONAL attestation forward rides
/// (plan §4.2/§4.4 — "the continuity-bearing flag is relay-authored
/// envelope metadata, never signed content"). The flag's wire
/// encoding is the hop-authored session ENVELOPE itself: a live,
/// continuity-bearing forward travels on the standard stream
/// (`SUBPROTOCOL_READINESS_ATTESTATION as u64`); a provisional one —
/// a warm-start re-send, or any forward while the relay's own
/// upstream continuity is not Established (the §4.4 hop rule) —
/// travels on THIS stream. Same subprotocol id, byte-identical
/// committed codec: the payload the origin signed is never touched,
/// and the flag is authenticated by the hop session exactly like
/// every other envelope field. A hostile relay could lie about the
/// flag on either encoding — the §4.5 stated v1 trust assumption
/// inside the owner-root boundary.
pub const SENSING_PROVISIONAL_STREAM: u64 = 0x0001_0C03;

/// Hard cap on one encoded sensing payload (either subprotocol):
/// 4 KiB. Inline constraints are capped at 1 KiB (plan §5) and every
/// other field is bounded and small, so any larger payload is
/// malformed or hostile — enforced on encode AND before decode.
pub const MAX_SENSING_FRAME_BYTES: usize = 4096;

/// Domain-separation context for the attestation signature
/// transcript: fed to `blake3::Hasher::new_derive_key`, so the
/// signed digest can never collide with the interest/constraints
/// digests or any other blake3 use in the tree. The `v1` is the
/// transcript version — a field-list change means a new domain,
/// never a silent re-interpretation.
pub const ATTESTATION_SIG_DOMAIN: &str = "net.sensing.attestation.sig.v1";

/// Why a sensing payload failed the wire codec.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum WireError {
    /// The payload exceeds [`MAX_SENSING_FRAME_BYTES`] (checked on
    /// encode and before decode).
    Oversize {
        /// The offending payload length.
        len: usize,
    },
    /// postcard could not encode/decode the payload.
    Codec(postcard::Error),
    /// Bytes remained after exactly one object was decoded — a
    /// sensing payload is never a concatenation (strict decode).
    TrailingBytes {
        /// How many undecoded bytes trailed the object.
        remaining: usize,
    },
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Oversize { len } => {
                write!(
                    f,
                    "sensing payload {len} B > {MAX_SENSING_FRAME_BYTES} B cap"
                )
            }
            Self::Codec(error) => write!(f, "sensing payload codec failure: {error}"),
            Self::TrailingBytes { remaining } => {
                write!(f, "{remaining} trailing bytes after sensing payload")
            }
        }
    }
}

impl std::error::Error for WireError {}

fn encode_capped<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, WireError> {
    let bytes = postcard::to_allocvec(value).map_err(WireError::Codec)?;
    if bytes.len() > MAX_SENSING_FRAME_BYTES {
        return Err(WireError::Oversize { len: bytes.len() });
    }
    Ok(bytes)
}

fn decode_strict<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, WireError> {
    if bytes.len() > MAX_SENSING_FRAME_BYTES {
        return Err(WireError::Oversize { len: bytes.len() });
    }
    let (value, rest) = postcard::take_from_bytes::<T>(bytes).map_err(WireError::Codec)?;
    if !rest.is_empty() {
        return Err(WireError::TrailingBytes {
            remaining: rest.len(),
        });
    }
    Ok(value)
}

/// Encode one [`SensingInterestFrame`] as the 0x0C02 payload.
pub fn encode_interest_frame(frame: &SensingInterestFrame) -> Result<Vec<u8>, WireError> {
    encode_capped(frame)
}

/// Strict-decode one [`SensingInterestFrame`] from a 0x0C02 payload
/// (size-capped; trailing bytes rejected). Decoding says nothing
/// about authenticity or identity — intake validation
/// ([`SensingInterestFrame::validated_spec`]) still applies.
pub fn decode_interest_frame(bytes: &[u8]) -> Result<SensingInterestFrame, WireError> {
    decode_strict(bytes)
}

/// Encode one [`ReadinessAttestation`] as the 0x0C03 payload.
pub fn encode_attestation(attestation: &ReadinessAttestation) -> Result<Vec<u8>, WireError> {
    encode_capped(attestation)
}

/// Strict-decode one [`ReadinessAttestation`] from a 0x0C03 payload
/// (size-capped; trailing bytes rejected). Decoding says nothing
/// about authenticity — [`verify_attestation`] still applies.
pub fn decode_attestation(bytes: &[u8]) -> Result<ReadinessAttestation, WireError> {
    decode_strict(bytes)
}

/// The 0x0C03 wire attestation (plan §4.2): one origin-signed
/// readiness proof. Relays forward these bytes identically —
/// suppress or delay, never alter; the continuity-bearing flag is
/// relay-authored envelope metadata, never a field here (§4.4). The
/// signature binds the §4.2 transcript (module docs) — it proves
/// authorship, not recency (§4.5).
#[derive(Clone, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub struct ReadinessAttestation {
    /// The VALIDATED capability-interest identity this proof
    /// answers (the provider re-derived it before signing —
    /// [`SensingInterestFrame::validate_provider_registration`]).
    pub interest_digest: Digest256,
    /// The signing provider's NodeId (derived from its entity
    /// identity; cross-checked against the verifying [`EntityId`]).
    pub origin: u64,
    /// The provider's signed boot epoch (§4.6 ordering scope).
    pub origin_incarnation: Incarnation,
    /// Capability the predicate targets.
    pub capability_id: CapabilityId,
    /// The provider's OWN announce generation at evaluation time —
    /// attested content, bound one level down in the observation key
    /// (§3.2/§3.4).
    pub capability_generation: u64,
    /// The provider-signed status.
    pub status: AttestedStatus,
    /// Compact reason code beside the status (§4.4 projection).
    pub status_reason: StatusReason,
    /// Provider-side time-to-start estimate when Ready — each
    /// consumer adds its own route estimate against its own budget
    /// (§3.3); never an end-to-end claim.
    pub estimated_start: Option<Duration>,
    /// Signed per-(origin, incarnation, interest) sequence number
    /// (strictly-newer admission, §4.6).
    pub seq: u64,
    /// The emission cadence the provider signed for this branch
    /// (continuity-window input, §4.5).
    pub promised_cadence: Duration,
    /// The audience commitment the interest was validated under
    /// (v1: canonical owner-root id) — signed, so a proof can never
    /// be re-homed across audiences.
    pub audience_scope: AudienceScopeCommitment,
    /// ed25519 signature over the 32-byte transcript digest (module
    /// docs).
    pub signature: Signature64,
}

impl ReadinessAttestation {
    /// The signable fields of this attestation (everything except
    /// the signature).
    pub fn unsigned(&self) -> UnsignedAttestation {
        UnsignedAttestation {
            interest_digest: self.interest_digest,
            origin: self.origin,
            origin_incarnation: self.origin_incarnation,
            capability_id: self.capability_id.clone(),
            capability_generation: self.capability_generation,
            status: self.status,
            status_reason: self.status_reason,
            estimated_start: self.estimated_start,
            seq: self.seq,
            promised_cadence: self.promised_cadence,
            audience_scope: self.audience_scope,
        }
    }

    /// The 32-byte domain-separated digest of this attestation's
    /// signature transcript — the exact bytes the origin signed, and
    /// the semantic fingerprint ([`semantic_attestation`]).
    pub fn transcript_digest(&self) -> [u8; 32] {
        self.unsigned().transcript_digest()
    }
}

/// The unsigned content of a [`ReadinessAttestation`] — exactly the
/// fields the §4.2 signature transcript binds, as named fields so
/// the three adjacent `u64`s (origin, generation, seq) can never be
/// swapped silently at a call site. [`sign_attestation`] seals it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct UnsignedAttestation {
    /// See [`ReadinessAttestation::interest_digest`].
    pub interest_digest: Digest256,
    /// See [`ReadinessAttestation::origin`]; must match the signing
    /// keypair's node id.
    pub origin: u64,
    /// See [`ReadinessAttestation::origin_incarnation`].
    pub origin_incarnation: Incarnation,
    /// See [`ReadinessAttestation::capability_id`].
    pub capability_id: CapabilityId,
    /// See [`ReadinessAttestation::capability_generation`].
    pub capability_generation: u64,
    /// See [`ReadinessAttestation::status`].
    pub status: AttestedStatus,
    /// See [`ReadinessAttestation::status_reason`].
    pub status_reason: StatusReason,
    /// See [`ReadinessAttestation::estimated_start`].
    pub estimated_start: Option<Duration>,
    /// See [`ReadinessAttestation::seq`].
    pub seq: u64,
    /// See [`ReadinessAttestation::promised_cadence`].
    pub promised_cadence: Duration,
    /// See [`ReadinessAttestation::audience_scope`].
    pub audience_scope: AudienceScopeCommitment,
}

impl UnsignedAttestation {
    /// The hand-rolled signature transcript (module docs) —
    /// injective by construction: the single variable-width field
    /// (`capability_id`) is length-prefixed; everything else is
    /// fixed-width:
    ///
    /// ```text
    /// interest_digest         32 B
    /// origin                  u64 LE
    /// origin_incarnation      u64 LE
    /// len(capability_id)      u64 LE, then the UTF-8 bytes
    /// capability_generation   u64 LE
    /// status                  1 B canonical tag
    /// status_reason           3 B (tag + u16 LE parameter)
    /// estimated_start         1 B presence + u128 LE nanos
    /// seq                     u64 LE
    /// promised_cadence        u128 LE nanos
    /// audience_scope          32 B
    /// ```
    ///
    /// The protocol domain + version bind via the
    /// [`ATTESTATION_SIG_DOMAIN`] derive-key context in
    /// [`Self::transcript_digest`], not as transcript bytes.
    pub fn transcript(&self) -> Vec<u8> {
        let id_bytes = self.capability_id.as_str().as_bytes();
        let mut out = Vec::with_capacity(128 + id_bytes.len());
        out.extend_from_slice(self.interest_digest.as_bytes());
        out.extend_from_slice(&self.origin.to_le_bytes());
        out.extend_from_slice(&self.origin_incarnation.get().to_le_bytes());
        out.extend_from_slice(&(id_bytes.len() as u64).to_le_bytes());
        out.extend_from_slice(id_bytes);
        out.extend_from_slice(&self.capability_generation.to_le_bytes());
        out.push(status_tag(self.status));
        out.extend_from_slice(&reason_bytes(self.status_reason));
        match self.estimated_start {
            None => out.extend_from_slice(&[0u8; 17]),
            Some(estimate) => {
                out.push(1);
                out.extend_from_slice(&estimate.as_nanos().to_le_bytes());
            }
        }
        out.extend_from_slice(&self.seq.to_le_bytes());
        out.extend_from_slice(&self.promised_cadence.as_nanos().to_le_bytes());
        out.extend_from_slice(self.audience_scope.as_bytes());
        out
    }

    /// `blake3::derive_key(ATTESTATION_SIG_DOMAIN, transcript)` —
    /// the 32 bytes the origin actually signs (module docs), and the
    /// semantic fingerprint of the attestation.
    pub fn transcript_digest(&self) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new_derive_key(ATTESTATION_SIG_DOMAIN);
        hasher.update(&self.transcript());
        *hasher.finalize().as_bytes()
    }
}

/// Canonical 1-byte transcript tag for [`AttestedStatus`]
/// (append-only; never a serde encoding).
const fn status_tag(status: AttestedStatus) -> u8 {
    match status {
        AttestedStatus::Ready => 0,
        AttestedStatus::NotReady => 1,
        AttestedStatus::ProviderUnknown => 2,
    }
}

/// Canonical fixed-width transcript encoding for [`StatusReason`]:
/// variant tag + u16 LE parameter (zero where unused; append-only).
const fn reason_bytes(reason: StatusReason) -> [u8; 3] {
    match reason {
        StatusReason::None => [0, 0, 0],
        StatusReason::Provider(code) => {
            let le = code.to_le_bytes();
            [1, le[0], le[1]]
        }
        StatusReason::UnsupportedPredicate => [2, 0, 0],
        StatusReason::TemporarilyUnevaluable => [3, 0, 0],
        StatusReason::InvalidConstraints => [4, 0, 0],
        StatusReason::SamplingIntervalUnsupported => [5, 0, 0],
    }
}

/// Why an attestation could not be signed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum AttestationSignError {
    /// The unsigned `origin` does not name the signing keypair's
    /// node id — an origin must only ever sign as itself.
    OriginMismatch {
        /// What the unsigned fields claimed.
        claimed: u64,
        /// The signing keypair's actual node id.
        keypair: u64,
    },
    /// The keypair refused to sign (public-only keypairs return
    /// [`EntityError::ReadOnly`]).
    Signing(EntityError),
}

impl fmt::Display for AttestationSignError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OriginMismatch { claimed, keypair } => write!(
                f,
                "attestation origin {claimed:#x} is not the signing keypair's node id \
                 {keypair:#x}"
            ),
            Self::Signing(error) => write!(f, "attestation signing failed: {error}"),
        }
    }
}

impl std::error::Error for AttestationSignError {}

/// Why an attestation failed verification.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum AttestationVerifyError {
    /// The attested `origin` does not name the verifying entity's
    /// node id — the proof is not this entity's, whatever the
    /// signature says.
    OriginMismatch {
        /// What the attestation claimed.
        attested: u64,
        /// The verifying entity's actual node id.
        entity: u64,
    },
    /// The signature does not verify over the re-built transcript
    /// (or the entity's public key is invalid).
    Signature(EntityError),
}

impl fmt::Display for AttestationVerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OriginMismatch { attested, entity } => write!(
                f,
                "attestation origin {attested:#x} is not the verifying entity's node id \
                 {entity:#x}"
            ),
            Self::Signature(error) => write!(f, "attestation signature invalid: {error}"),
        }
    }
}

impl std::error::Error for AttestationVerifyError {}

/// Sign an attestation: `origin` must be the keypair's own node id
/// (an origin only ever signs as itself), and the keypair must hold
/// its signing half (`try_sign` — public-only keypairs fail closed,
/// never panic). The signature covers the 32-byte transcript digest
/// (module docs).
pub fn sign_attestation(
    keypair: &EntityKeypair,
    unsigned: UnsignedAttestation,
) -> Result<ReadinessAttestation, AttestationSignError> {
    let keypair_node = keypair.node_id();
    if unsigned.origin != keypair_node {
        return Err(AttestationSignError::OriginMismatch {
            claimed: unsigned.origin,
            keypair: keypair_node,
        });
    }
    let digest = unsigned.transcript_digest();
    let signature = keypair
        .try_sign(&digest)
        .map_err(AttestationSignError::Signing)?;
    let UnsignedAttestation {
        interest_digest,
        origin,
        origin_incarnation,
        capability_id,
        capability_generation,
        status,
        status_reason,
        estimated_start,
        seq,
        promised_cadence,
        audience_scope,
    } = unsigned;
    Ok(ReadinessAttestation {
        interest_digest,
        origin,
        origin_incarnation,
        capability_id,
        capability_generation,
        status,
        status_reason,
        estimated_start,
        seq,
        promised_cadence,
        audience_scope,
        signature: Signature64(signature.to_bytes()),
    })
}

/// Verify an attestation against the claimed origin's entity
/// identity: the attested `origin` must be `origin_entity`'s node id
/// AND the signature must verify (strict ed25519) over the re-built
/// transcript digest. Any tampered transcript field changes the
/// digest and fails here.
pub fn verify_attestation(
    attestation: &ReadinessAttestation,
    origin_entity: &EntityId,
) -> Result<(), AttestationVerifyError> {
    let entity_node = origin_entity.node_id();
    if attestation.origin != entity_node {
        return Err(AttestationVerifyError::OriginMismatch {
            attested: attestation.origin,
            entity: entity_node,
        });
    }
    origin_entity
        .verify_bytes(&attestation.transcript_digest(), &attestation.signature.0)
        .map_err(AttestationVerifyError::Signature)
}

/// Why a wire attestation could not be bridged onto a validated
/// interest ([`semantic_attestation`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AttestationBridgeError {
    /// The attestation names a different capability than the
    /// validated interest key.
    CapabilityMismatch,
    /// The attestation answers a different interest digest than the
    /// validated interest key.
    InterestDigestMismatch,
}

impl fmt::Display for AttestationBridgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CapabilityMismatch => {
                f.write_str("attestation capability does not match the validated interest")
            }
            Self::InterestDigestMismatch => {
                f.write_str("attestation interest digest does not match the validated interest")
            }
        }
    }
}

impl std::error::Error for AttestationBridgeError {}

/// Bridge a wire [`ReadinessAttestation`] into the semantic layer's
/// [`Attestation`]. The caller supplies the VALIDATED
/// [`CapabilityInterestKey`] — the one re-derived at frame intake
/// ([`SensingInterestFrame::validated_spec`]), never one rebuilt
/// from the attestation's own claims — and this helper cross-checks
/// the attestation against it before minting the
/// [`ProviderObservationKey`]. Verify authorship first
/// ([`verify_attestation`]); the bridge maps identity, not trust.
///
/// The semantic `fingerprint` is the transcript digest — the SI-0
/// stand-in (a hash of the semantic fields) is now the REAL
/// signed-bytes digest, so seq-gate equivocation detection keys on
/// exactly what the origin signed.
pub fn semantic_attestation(
    interest: &CapabilityInterestKey,
    wire: &ReadinessAttestation,
) -> Result<Attestation, AttestationBridgeError> {
    if wire.capability_id != interest.capability_id {
        return Err(AttestationBridgeError::CapabilityMismatch);
    }
    if wire.interest_digest != interest.interest_digest {
        return Err(AttestationBridgeError::InterestDigestMismatch);
    }
    Ok(Attestation {
        key: ProviderObservationKey::new(interest.clone(), wire.origin, wire.capability_generation),
        origin_incarnation: wire.origin_incarnation,
        status: wire.status,
        estimated_start: wire.estimated_start,
        seq: wire.seq,
        promised_cadence: wire.promised_cadence,
        fingerprint: Digest256::from_bytes(wire.transcript_digest()),
    })
}

#[cfg(test)]
mod tests {
    use super::super::super::broadcast::{SUBPROTOCOL_CAPABILITY_ANN, SUBPROTOCOL_ROUTE_WITHDRAW};
    use super::super::identity::{
        CanonicalConstraints, DisclosureClass, InterestSpec, ProviderSelector, ResultMode,
        WorkLatencyEnvelope,
    };
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

    fn keypair() -> EntityKeypair {
        EntityKeypair::from_bytes([7u8; 32])
    }

    fn unsigned(origin: u64) -> UnsignedAttestation {
        UnsignedAttestation {
            interest_digest: spec().interest_digest(),
            origin,
            origin_incarnation: Incarnation::new(3),
            capability_id: CapabilityId::new("print.document"),
            capability_generation: 12,
            status: AttestedStatus::Ready,
            status_reason: StatusReason::None,
            estimated_start: Some(Duration::from_millis(800)),
            seq: 41,
            promised_cadence: Duration::from_millis(150),
            audience_scope: AudienceScopeCommitment::from_bytes([0xAA; 32]),
        }
    }

    fn signed() -> ReadinessAttestation {
        let keypair = keypair();
        sign_attestation(&keypair, unsigned(keypair.node_id())).unwrap()
    }

    #[test]
    fn subprotocol_ids_are_committed_in_the_0x0c_family() {
        // Committed per review-7 sign-off — moving either after this
        // point is a wire break.
        assert_eq!(SUBPROTOCOL_SENSING_INTEREST, 0x0C02);
        assert_eq!(SUBPROTOCOL_READINESS_ATTESTATION, 0x0C03);
        // Contiguous with, and distinct from, the existing family.
        assert_eq!(SUBPROTOCOL_CAPABILITY_ANN, 0x0C00);
        assert_eq!(SUBPROTOCOL_ROUTE_WITHDRAW, 0x0C01);
    }

    #[test]
    fn interest_frames_round_trip_through_postcard() {
        let spec = spec();
        let frames = [
            SensingInterestFrame::capability_registration(
                &spec,
                Duration::from_millis(100),
                Duration::from_secs(30),
                0xA11CE,
            ),
            SensingInterestFrame::provider_registration(
                &spec,
                0x77,
                Duration::from_millis(100),
                Duration::from_secs(30),
            ),
            SensingInterestFrame::Deregister {
                interest_digest: spec.interest_digest(),
                target: Some(0x77),
            },
            SensingInterestFrame::Deregister {
                interest_digest: spec.interest_digest(),
                target: None,
            },
        ];
        for frame in frames {
            let bytes = encode_interest_frame(&frame).unwrap();
            assert!(bytes.len() <= MAX_SENSING_FRAME_BYTES);
            assert_eq!(decode_interest_frame(&bytes).unwrap(), frame);
        }
    }

    #[test]
    fn strict_decode_rejects_trailing_truncated_and_oversize_frames() {
        let frame = SensingInterestFrame::capability_registration(
            &spec(),
            Duration::from_millis(100),
            Duration::from_secs(30),
            0xA,
        );
        let bytes = encode_interest_frame(&frame).unwrap();

        // Trailing bytes: exactly one object per payload.
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert_eq!(
            decode_interest_frame(&trailing),
            Err(WireError::TrailingBytes { remaining: 1 }),
        );

        // Every strict prefix is invalid (all fields mandatory).
        for cut in 0..bytes.len() {
            assert!(
                decode_interest_frame(&bytes[..cut]).is_err(),
                "truncation at {cut} must not decode",
            );
        }

        // Oversize input is refused before parsing.
        let oversize = vec![0u8; MAX_SENSING_FRAME_BYTES + 1];
        assert_eq!(
            decode_interest_frame(&oversize),
            Err(WireError::Oversize {
                len: MAX_SENSING_FRAME_BYTES + 1,
            }),
        );
    }

    #[test]
    fn oversize_frames_are_refused_on_encode() {
        // A Nodes selector big enough to blow the 4 KiB cap — the
        // one unbounded field family the constraint cap does not
        // already bound.
        let mut huge = spec();
        huge.providers =
            ProviderSelector::nodes((0..600).map(|i| u64::MAX - i as u64).collect::<Vec<_>>());
        let frame = SensingInterestFrame::capability_registration(
            &huge,
            Duration::from_millis(100),
            Duration::from_secs(30),
            0xA,
        );
        assert!(matches!(
            encode_interest_frame(&frame),
            Err(WireError::Oversize { .. }),
        ));
    }

    #[test]
    fn attestations_round_trip_and_still_verify() {
        let attestation = signed();
        let bytes = encode_attestation(&attestation).unwrap();
        assert!(bytes.len() <= MAX_SENSING_FRAME_BYTES);
        let back = decode_attestation(&bytes).unwrap();
        assert_eq!(back, attestation);
        // The codec never disturbs the transcript: verification
        // still holds on the decoded copy.
        verify_attestation(&back, keypair().entity_id()).unwrap();
    }

    #[test]
    fn attestation_decode_rejects_trailing_and_truncation() {
        let bytes = encode_attestation(&signed()).unwrap();
        let mut trailing = bytes.clone();
        trailing.push(0xFF);
        assert_eq!(
            decode_attestation(&trailing),
            Err(WireError::TrailingBytes { remaining: 1 }),
        );
        for cut in 0..bytes.len() {
            assert!(
                decode_attestation(&bytes[..cut]).is_err(),
                "truncation at {cut} must not decode",
            );
        }
    }

    #[test]
    fn sign_and_verify_round_trip() {
        let keypair = keypair();
        let attestation = sign_attestation(&keypair, unsigned(keypair.node_id())).unwrap();
        verify_attestation(&attestation, keypair.entity_id()).unwrap();
        // The fingerprint surface: deterministic and equal between
        // the unsigned and signed views of one attestation.
        assert_eq!(
            attestation.transcript_digest(),
            unsigned(keypair.node_id()).transcript_digest(),
        );
    }

    #[test]
    fn signing_rejects_a_foreign_origin_and_a_public_only_keypair() {
        let keypair = keypair();
        // An origin must only ever sign as itself.
        let foreign = unsigned(keypair.node_id() ^ 1);
        assert_eq!(
            sign_attestation(&keypair, foreign),
            Err(AttestationSignError::OriginMismatch {
                claimed: keypair.node_id() ^ 1,
                keypair: keypair.node_id(),
            }),
        );
        // Public-only keypairs fail closed via try_sign — no panic.
        let public_only = EntityKeypair::public_only(keypair.entity_id().clone());
        assert_eq!(
            sign_attestation(&public_only, unsigned(keypair.node_id())),
            Err(AttestationSignError::Signing(EntityError::ReadOnly)),
        );
    }

    #[test]
    fn verification_rejects_the_wrong_entity() {
        let attestation = signed();
        let other = EntityKeypair::from_bytes([9u8; 32]);
        // The attested origin is not this entity's node id.
        assert!(matches!(
            verify_attestation(&attestation, other.entity_id()),
            Err(AttestationVerifyError::OriginMismatch { .. }),
        ));
        // And an entity whose node id was forged to match still
        // fails on the signature itself.
        let mut forged = attestation.clone();
        forged.origin = other.node_id();
        assert!(matches!(
            verify_attestation(&forged, other.entity_id()),
            Err(AttestationVerifyError::Signature(_)),
        ));
    }

    #[test]
    fn every_transcript_field_is_tamper_evident() {
        // Flip each signed field (and the signature) one at a time:
        // verification must fail for every mutation — the transcript
        // binds EXACTLY the §4.2 list, so nothing here is malleable.
        type AttestationMutation = fn(&mut ReadinessAttestation);
        let mutations: [(&str, AttestationMutation); 12] = [
            ("interest_digest", |a| {
                a.interest_digest = Digest256::from_bytes([0xFF; 32]);
            }),
            ("origin", |a| a.origin ^= 1),
            ("origin_incarnation", |a| {
                a.origin_incarnation = Incarnation::new(a.origin_incarnation.get() + 1);
            }),
            ("capability_id", |a| {
                a.capability_id = CapabilityId::new("print.documenu");
            }),
            ("capability_generation", |a| a.capability_generation += 1),
            ("status", |a| a.status = AttestedStatus::NotReady),
            ("status_reason", |a| {
                a.status_reason = StatusReason::Provider(7);
            }),
            ("estimated_start", |a| a.estimated_start = None),
            ("seq", |a| a.seq += 1),
            ("promised_cadence", |a| {
                a.promised_cadence = Duration::from_millis(151);
            }),
            ("audience_scope", |a| {
                a.audience_scope = AudienceScopeCommitment::from_bytes([0xBB; 32]);
            }),
            ("signature", |a| a.signature.0[0] ^= 1),
        ];
        let entity = keypair().entity_id().clone();
        for (field, mutate) in mutations {
            let mut tampered = signed();
            mutate(&mut tampered);
            assert!(
                verify_attestation(&tampered, &entity).is_err(),
                "tampered {field} must fail verification",
            );
        }
        // Control: the untampered attestation verifies.
        verify_attestation(&signed(), &entity).unwrap();
    }

    #[test]
    fn transcript_encoding_is_injective_at_the_option_boundary() {
        // Presence is transcript-bearing: `None` and `Some(0)` are
        // different signed statements.
        let keypair = keypair();
        let mut none = unsigned(keypair.node_id());
        none.estimated_start = None;
        let mut zero = unsigned(keypair.node_id());
        zero.estimated_start = Some(Duration::ZERO);
        assert_ne!(none.transcript_digest(), zero.transcript_digest());
        // And the digest is domain-separated from the interest
        // digest machinery.
        assert_ne!(
            none.transcript_digest(),
            *spec().interest_digest().as_bytes(),
        );
    }

    #[test]
    fn bridge_mints_the_semantic_attestation_from_the_validated_key() {
        let attestation = signed();
        let key = spec().key();
        let semantic = semantic_attestation(&key, &attestation).unwrap();
        assert_eq!(semantic.key.interest, key);
        assert_eq!(semantic.key.provider, attestation.origin);
        assert_eq!(
            semantic.key.capability_generation,
            attestation.capability_generation,
        );
        assert_eq!(semantic.origin_incarnation, attestation.origin_incarnation);
        assert_eq!(semantic.status, attestation.status);
        assert_eq!(semantic.estimated_start, attestation.estimated_start);
        assert_eq!(semantic.seq, attestation.seq);
        assert_eq!(semantic.promised_cadence, attestation.promised_cadence);
        // The SI-0 fingerprint stand-in is now the REAL signed-bytes
        // digest.
        assert_eq!(
            semantic.fingerprint,
            Digest256::from_bytes(attestation.transcript_digest()),
        );
    }

    #[test]
    fn bridge_rejects_a_mismatched_interest() {
        let attestation = signed();
        // Different digest, same capability.
        let mut other = spec();
        other.result_mode = ResultMode::Each;
        assert_eq!(
            semantic_attestation(&other.key(), &attestation).unwrap_err(),
            AttestationBridgeError::InterestDigestMismatch,
        );
        // Different capability entirely.
        let mut foreign = spec();
        foreign.capability_id = CapabilityId::new("scan.document");
        assert_eq!(
            semantic_attestation(&foreign.key(), &attestation).unwrap_err(),
            AttestationBridgeError::CapabilityMismatch,
        );
    }
}
