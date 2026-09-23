//! Org identity, membership certificates and revocation-floor
//! bundles — the OA-1 credential layer.
//!
//! Core's `behavior/org.rs`, ported byte-for-byte: the wire layouts
//! (cert 156 B, bundle `108 + 36·n` B), the domain-prefixed signing
//! transcripts (`net-org-cert-v1`, `net-org-floors-v1`) and the
//! acceptance check order are all frozen against core's golden
//! vectors. What is left behind is core's clock reads and its
//! `getrandom`+`abort` nonce/seed draws: every issue function takes
//! `now_unix_secs` and a caller-supplied `nonce` instead, and
//! [`OrgKeypair::from_bytes`] is the only constructor — seed custody
//! is the host's job.
//!
//! A verified [`OrgMembershipCert`] proves *belonging only*. It never
//! grants invocation authority: that is the grant family
//! ([`crate::org::grant`]) plus the per-call proof
//! ([`crate::org::proof`]).

use std::collections::BTreeMap;

use crate::identity::{hex_lower, unhex, verify_entity_signature, EntityKeypair};
use crate::org::entity::EntityId;

/// Signature domain for [`OrgMembershipCert`]. Prefixed to the
/// 92-byte canonical payload before signing so a certificate
/// signature can never be replayed as any other org object (and
/// vice versa).
pub const ORG_CERT_SIG_DOMAIN: &[u8] = b"net-org-cert-v1";

/// Signature domain for [`OrgRevocationBundle`].
pub const ORG_FLOORS_SIG_DOMAIN: &[u8] = b"net-org-floors-v1";

/// Recommended membership-certificate TTL (~1 year). Renewal is
/// silent re-issue; see the plan's cert discipline.
pub const ORG_CERT_TTL_SECS_RECOMMENDED: u64 = 365 * 24 * 60 * 60;

/// Hard upper bound on a membership certificate's validity window
/// (2 years). Enforced at issue (`try_issue` rejects a longer
/// `duration_secs`) AND at verify (`verify` rejects a foreign cert
/// whose `not_after - not_before` exceeds it) — a peer must not be
/// able to mint an effectively immortal belonging statement that
/// only its own issuer would have refused.
pub const MAX_ORG_CERT_TTL_SECS: u64 = 2 * 365 * 24 * 60 * 60;

/// Hard cap on floors carried by one [`OrgRevocationBundle`].
/// The check runs BEFORE any decode allocation, so hostile input
/// cannot make the parser allocate proportionately to a claimed
/// count. 65 536 members with simultaneously bumped floors is far
/// past any real fleet.
pub const MAX_REVOCATION_FLOORS_PER_BUNDLE: usize = 65_536;

/// Organization identity — a 32-byte Ed25519 verifying key.
///
/// Self-certifying: the id IS the key; there is no registry. The
/// derived (non-constant-time) `PartialEq` is deliberate — a public
/// key is not a bearer secret.
///
/// `Ord` is the lexicographic byte order; it doubles as the
/// canonical ordering for persisted floor maps
/// (`BTreeMap<(OrgId, EntityId), u32>` in the revocation facts).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OrgId(pub [u8; 32]);

impl OrgId {
    /// Construct from raw verifying-key bytes.
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Borrow the 32-byte representation.
    #[inline]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Verify a signature against this org's root key.
    ///
    /// `verify_strict` through
    /// [`crate::identity::verify_entity_signature`] — one Ed25519
    /// verify implementation in the crate, and the same
    /// malleability rationale as [`EntityId::verify`]: certs and
    /// bundles are compared and cached on their signed bytes, so the
    /// `(R, S + L)` malleated variant must not verify as a second
    /// encoding of the same logical object. A malformed key is
    /// [`OrgError::InvalidPublicKey`], a bad signature
    /// [`OrgError::InvalidSignature`] — core's classification.
    pub fn verify(&self, message: &[u8], signature: &[u8; 64]) -> Result<(), OrgError> {
        // Classify a malformed key distinctly from a bad signature,
        // as core's `verifying_key()` step does.
        ed25519_dalek::VerifyingKey::from_bytes(&self.0)
            .map_err(|_| OrgError::InvalidPublicKey)?;
        verify_entity_signature(&self.0, message, signature).map_err(|_| OrgError::InvalidSignature)
    }
}

impl core::fmt::Debug for OrgId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "OrgId({}...)", hex_lower(&self.0[..8]))
    }
}

impl core::fmt::Display for OrgId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&hex_lower(&self.0))
    }
}

// Hex string when human-readable, raw bytes otherwise — mirror
// `EntityId`'s impls so the two identity kinds read identically in
// every serialized form.
impl serde::Serialize for OrgId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        if serializer.is_human_readable() {
            serializer.serialize_str(&hex_lower(&self.0))
        } else {
            serializer.serialize_bytes(&self.0)
        }
    }
}

impl<'de> serde::Deserialize<'de> for OrgId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let bytes = if deserializer.is_human_readable() {
            let hex_str = String::deserialize(deserializer)?;
            unhex(&hex_str).map_err(serde::de::Error::custom)?
        } else {
            <Vec<u8>>::deserialize(deserializer)?
        };
        if bytes.len() != 32 {
            return Err(serde::de::Error::custom("org_id must be 32 bytes"));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        Ok(OrgId(arr))
    }
}

/// Offline organization root keypair: signs membership certificates,
/// grants and revocation bundles.
///
/// Core's type minus its native-only surface: `generate()` (the
/// `getrandom` + `abort` constructor) and `from_signing_key`/
/// `secret_bytes` (the dalek-typed seed round-trip) stay behind — the
/// host owns seed custody and injects it through
/// [`Self::from_bytes`]. Signing rides
/// [`crate::identity::EntityKeypair`], so there is exactly one
/// Ed25519 key implementation in this crate.
pub struct OrgKeypair {
    entity: EntityKeypair,
    org_id: OrgId,
}

impl OrgKeypair {
    /// Create from raw secret key bytes (a 32-byte Ed25519 seed).
    pub fn from_bytes(secret: [u8; 32]) -> Self {
        let entity = EntityKeypair::from_secret(secret);
        let org_id = OrgId::from_bytes(*entity.entity_id());
        Self { entity, org_id }
    }

    /// The public organization identity for this root key.
    #[inline]
    pub fn org_id(&self) -> OrgId {
        self.org_id
    }

    /// Sign a message with the org root key. Crate-internal: the
    /// public issuing surfaces are the typed `try_issue`/`issue_at`
    /// paths, never raw signing.
    pub(crate) fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.entity.sign(message)
    }
}

impl core::fmt::Debug for OrgKeypair {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("OrgKeypair")
            .field("org_id", &self.org_id)
            .field("secret", &"[REDACTED]")
            .finish()
    }
}

/// Errors from organization authority operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrgError {
    /// Wire bytes are the wrong length or structurally malformed
    /// (truncated, trailing garbage, inconsistent floor count).
    InvalidFormat,
    /// Org id bytes are not a valid Ed25519 point.
    InvalidPublicKey,
    /// Signature verification failed.
    InvalidSignature,
    /// `duration_secs == 0` passed at issue time — the window is
    /// born empty, so misuse surfaces as a typed error instead of an
    /// unusable credential.
    ZeroTtl,
    /// Validity window exceeds [`MAX_ORG_CERT_TTL_SECS`] (or the
    /// grant ceiling). Raised at issue AND at verify.
    TtlTooLong,
    /// `not_after <= not_before` — a zero or reversed validity window
    /// is structurally invalid. A reversed window is not merely
    /// unusable: with allowed clock skew both bounds can hold at
    /// once, admitting a certificate that was never live.
    InvalidValidityWindow,
    /// A clock-skew tolerance above
    /// [`crate::org::MAX_TOKEN_CLOCK_SKEW_SECS`] was passed. With
    /// saturating window arithmetic, an unbounded skew admits any
    /// expired certificate, so the ceiling is enforced, not
    /// documented.
    ClockSkewTooLarge,
    /// Certificate window has not opened yet (`now < not_before`,
    /// after skew).
    NotYetValid,
    /// Certificate window has closed (`now >= not_after`, after
    /// skew).
    Expired,
    /// Bundle carries more than
    /// [`MAX_REVOCATION_FLOORS_PER_BUNDLE`] floors.
    TooManyFloors,
    /// Bundle floors are not in strictly-ascending member order (out
    /// of order, or duplicate member keys). Canonical order is part
    /// of the signed transcript's injectivity contract, so a
    /// non-canonical bundle is rejected before its signature is
    /// examined.
    NonCanonicalFloors,
    /// A capability grant carries the all-zero `grant_id`. Zero is
    /// RESERVED — the owner-audience envelope sentinel — so issuance
    /// and decode both reject it.
    ReservedGrantId,
    /// A capability grant violates the structural rule
    /// `rights ⊇ DISCOVER ⇔ discovery binding present`: DISCOVER
    /// without a binding grants a right with no audience; a binding
    /// without DISCOVER smuggles audience material into a grant that
    /// confers no discovery.
    DiscoveryBindingMismatch,
    /// A grant's rights bitset carries bits this build does not know.
    /// Unknown rights could widen authority under an old verifier —
    /// wire evolution is honest, so they refuse loudly instead of
    /// being masked off.
    UnknownRights,
    /// A grant's rights bitset is empty — a credential that permits
    /// nothing is structurally meaningless.
    EmptyRights,
    /// A capability grant's `AnyNodeOwnedBy(org)` target names an org
    /// OTHER than the issuer — a permanently-unusable credential (it
    /// can never admit), refused at issue and decode/verify.
    TargetOrgNotIssuer,
}

impl core::fmt::Display for OrgError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidFormat => write!(f, "invalid wire format"),
            Self::InvalidPublicKey => write!(f, "invalid org public key"),
            Self::InvalidSignature => write!(f, "invalid signature"),
            Self::ZeroTtl => write!(f, "certificate TTL is zero"),
            Self::TtlTooLong => write!(
                f,
                "certificate validity window exceeds MAX_ORG_CERT_TTL_SECS"
            ),
            Self::InvalidValidityWindow => write!(
                f,
                "certificate validity window is zero or reversed (not_after <= not_before)"
            ),
            Self::ClockSkewTooLarge => {
                write!(f, "clock-skew tolerance exceeds MAX_TOKEN_CLOCK_SKEW_SECS")
            }
            Self::NotYetValid => write!(f, "certificate not yet valid"),
            Self::Expired => write!(f, "certificate expired"),
            Self::TooManyFloors => write!(
                f,
                "revocation bundle exceeds MAX_REVOCATION_FLOORS_PER_BUNDLE"
            ),
            Self::NonCanonicalFloors => {
                write!(f, "revocation bundle floors are not in canonical order")
            }
            Self::ReservedGrantId => {
                write!(f, "grant_id zero is reserved (owner-audience sentinel)")
            }
            Self::DiscoveryBindingMismatch => write!(
                f,
                "grant violates rights ⊇ DISCOVER ⇔ discovery binding present"
            ),
            Self::UnknownRights => write!(f, "grant rights carry unknown bits"),
            Self::EmptyRights => write!(f, "grant rights are empty"),
            Self::TargetOrgNotIssuer => write!(
                f,
                "grant AnyNodeOwnedBy target org is not the issuer org (permanently unusable)"
            ),
        }
    }
}

impl std::error::Error for OrgError {}

/// A signed organization membership certificate: "entity `member`
/// belongs to org `org_id`", valid `[not_before, not_after)` at
/// revocation generation `generation`.
///
/// Wire format (156 bytes):
/// ```text
/// org_id:       32 bytes (OrgId — issuing org's root key)
/// member:       32 bytes (EntityId)
/// not_before:    8 bytes (u64 unix seconds)
/// not_after:     8 bytes (u64 unix seconds, EXCLUSIVE)
/// generation:    4 bytes (u32; floor-checked against the fed
///                         revocation maxima — a cert below its
///                         org's floor for this member is dead)
/// nonce:         8 bytes (u64; makes re-issues byte-distinct)
/// --- signed above (with ORG_CERT_SIG_DOMAIN prefixed) ---
/// signature:    64 bytes (ed25519 by org_id)
/// ```
///
/// Both bounds are **inclusive-expiry** like the core's tokens: live
/// while `not_before <= now < not_after`.
#[derive(Clone, PartialEq, Eq)]
pub struct OrgMembershipCert {
    /// The organization asserting membership (also the verifying
    /// key for `signature`).
    pub org_id: OrgId,
    /// The entity the org vouches for.
    pub member: EntityId,
    /// Valid from (unix seconds).
    pub not_before: u64,
    /// Valid until (unix seconds, exclusive).
    pub not_after: u64,
    /// Revocation generation. An [`OrgRevocationBundle`] floor of
    /// `n` for `(org_id, member)` kills every cert with
    /// `generation < n`; re-issuing at a higher generation is how an
    /// org retires a member's outstanding certs without waiting for
    /// expiry.
    pub generation: u32,
    /// Per-issue nonce so silent renewals are byte-distinct
    /// (caller-supplied randomness).
    pub nonce: u64,
    /// ed25519 signature over `ORG_CERT_SIG_DOMAIN ‖ signed payload`.
    pub signature: [u8; 64],
}

impl OrgMembershipCert {
    /// Size of the signed payload (everything before the signature).
    const SIGNED_PAYLOAD_SIZE: usize = 32 + 32 + 8 + 8 + 4 + 8; // 92 bytes

    /// Size of the domain-prefixed signing input.
    const SIGNING_INPUT_SIZE: usize = ORG_CERT_SIG_DOMAIN.len() + Self::SIGNED_PAYLOAD_SIZE;

    /// Total serialized size.
    pub const WIRE_SIZE: usize = Self::SIGNED_PAYLOAD_SIZE + 64; // 156 bytes

    /// Issue a certificate valid from `now_unix_secs` for
    /// `duration_secs`.
    ///
    /// Rejects `duration_secs == 0` ([`OrgError::ZeroTtl`]) and
    /// `duration_secs > MAX_ORG_CERT_TTL_SECS`
    /// ([`OrgError::TtlTooLong`]) so misuse surfaces as a typed
    /// error at issue instead of as silent rejection on every
    /// receiver. `nonce` is the caller's randomness — core draws it
    /// from `getrandom` (and aborts the process on failure); the
    /// host must supply an unpredictable value so re-issues stay
    /// byte-distinct.
    pub fn try_issue(
        org: &OrgKeypair,
        member: EntityId,
        generation: u32,
        duration_secs: u64,
        now_unix_secs: u64,
        nonce: u64,
    ) -> Result<Self, OrgError> {
        if duration_secs == 0 {
            return Err(OrgError::ZeroTtl);
        }
        if duration_secs > MAX_ORG_CERT_TTL_SECS {
            return Err(OrgError::TtlTooLong);
        }
        Ok(Self::issue_at(
            org,
            member,
            generation,
            now_unix_secs,
            now_unix_secs.saturating_add(duration_secs),
            nonce,
        ))
    }

    /// Build and sign a certificate with fully explicit fields.
    ///
    /// The deterministic pin surface (golden vectors, in-crate
    /// tooling) — `pub` here where core keeps it `pub(crate)` because
    /// the cross-implementation conformance tests live outside the
    /// crate. It does NOT enforce the TTL discipline; that is
    /// [`Self::try_issue`]'s job and [`Self::verify`]'s.
    pub fn issue_at(
        org: &OrgKeypair,
        member: EntityId,
        generation: u32,
        not_before: u64,
        not_after: u64,
        nonce: u64,
    ) -> Self {
        let mut cert = Self {
            org_id: org.org_id(),
            member,
            not_before,
            not_after,
            generation,
            nonce,
            signature: [0u8; 64],
        };
        cert.signature = org.sign(&cert.signing_input());
        cert
    }

    /// Canonical signed payload — fixed offsets, little-endian.
    fn signed_payload(&self) -> [u8; Self::SIGNED_PAYLOAD_SIZE] {
        let mut buf = [0u8; Self::SIGNED_PAYLOAD_SIZE];
        let mut off = 0;
        buf[off..off + 32].copy_from_slice(self.org_id.as_bytes());
        off += 32;
        buf[off..off + 32].copy_from_slice(self.member.as_bytes());
        off += 32;
        buf[off..off + 8].copy_from_slice(&self.not_before.to_le_bytes());
        off += 8;
        buf[off..off + 8].copy_from_slice(&self.not_after.to_le_bytes());
        off += 8;
        buf[off..off + 4].copy_from_slice(&self.generation.to_le_bytes());
        off += 4;
        buf[off..off + 8].copy_from_slice(&self.nonce.to_le_bytes());
        buf
    }

    /// Domain-prefixed signing input:
    /// `ORG_CERT_SIG_DOMAIN ‖ signed_payload`.
    fn signing_input(&self) -> [u8; Self::SIGNING_INPUT_SIZE] {
        let mut buf = [0u8; Self::SIGNING_INPUT_SIZE];
        buf[..ORG_CERT_SIG_DOMAIN.len()].copy_from_slice(ORG_CERT_SIG_DOMAIN);
        buf[ORG_CERT_SIG_DOMAIN.len()..].copy_from_slice(&self.signed_payload());
        buf
    }

    /// Verify the certificate's signature and structural validity.
    ///
    /// Checks, in order: the validity window is well-formed
    /// (`not_after > not_before`), the window length is within
    /// [`MAX_ORG_CERT_TTL_SECS`], then `verify_strict` of the
    /// domain-prefixed payload against `org_id`. No wall-clock or
    /// revocation-floor checks — those are contextual
    /// ([`Self::is_valid_at_with_skew`]; floors live in
    /// [`crate::org::revocation::RevocationFacts`]).
    pub fn verify(&self) -> Result<(), OrgError> {
        if self.not_after <= self.not_before {
            return Err(OrgError::InvalidValidityWindow);
        }
        if self.not_after - self.not_before > MAX_ORG_CERT_TTL_SECS {
            return Err(OrgError::TtlTooLong);
        }
        self.org_id.verify(&self.signing_input(), &self.signature)
    }

    /// Signature + wall-clock validity against a caller-supplied
    /// `now_secs` (unix seconds), with `skew_secs` of tolerance on
    /// both bounds — accepted while `now >= not_before - skew` AND
    /// `now < not_after + skew` (saturating on both bounds).
    ///
    /// One admission passes ONE `now_secs` here (core's
    /// `ClockSample::wall_secs`) so every freshness check reads a
    /// single clock sample. The skew ceiling is ENFORCED:
    /// `skew_secs > MAX_TOKEN_CLOCK_SKEW_SECS` is
    /// [`OrgError::ClockSkewTooLarge`].
    pub fn is_valid_at_with_skew(&self, now_secs: u64, skew_secs: u64) -> Result<(), OrgError> {
        if skew_secs > crate::org::MAX_TOKEN_CLOCK_SKEW_SECS {
            return Err(OrgError::ClockSkewTooLarge);
        }
        self.verify()?;
        self.check_time_bounds_at(now_secs, skew_secs)
    }

    /// Wall-clock window check at an explicit `now` (unix seconds),
    /// without signature verification — signatures are immutable,
    /// expiry must be re-evaluated per use. `now == not_after` is
    /// expired.
    fn check_time_bounds_at(&self, now: u64, skew_secs: u64) -> Result<(), OrgError> {
        if now < self.not_before.saturating_sub(skew_secs) {
            return Err(OrgError::NotYetValid);
        }
        if now >= self.not_after.saturating_add(skew_secs) {
            return Err(OrgError::Expired);
        }
        Ok(())
    }

    /// `true` iff `now_unix_secs` has reached `not_after`. The
    /// explicit-time form of core's clock-reading `is_expired`.
    pub fn is_expired_at(&self, now_unix_secs: u64) -> bool {
        now_unix_secs >= self.not_after
    }

    /// Serialize to canonical wire format.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(Self::WIRE_SIZE);
        buf.extend_from_slice(&self.signed_payload());
        buf.extend_from_slice(&self.signature);
        buf
    }

    /// Deserialize from wire format. Rejects any length other than
    /// exactly [`Self::WIRE_SIZE`] — truncation and trailing bytes
    /// are both format errors, never partial reads. Decoding does
    /// NOT verify the signature.
    pub fn from_bytes(data: &[u8]) -> Result<Self, OrgError> {
        if data.len() != Self::WIRE_SIZE {
            return Err(OrgError::InvalidFormat);
        }
        let org_id = OrgId::from_bytes(data[0..32].try_into().unwrap());
        let member = EntityId::from_bytes(data[32..64].try_into().unwrap());
        let not_before = u64::from_le_bytes(data[64..72].try_into().unwrap());
        let not_after = u64::from_le_bytes(data[72..80].try_into().unwrap());
        let generation = u32::from_le_bytes(data[80..84].try_into().unwrap());
        let nonce = u64::from_le_bytes(data[84..92].try_into().unwrap());
        let mut signature = [0u8; 64];
        signature.copy_from_slice(&data[92..156]);
        Ok(Self {
            org_id,
            member,
            not_before,
            not_after,
            generation,
            nonce,
            signature,
        })
    }
}

impl core::fmt::Debug for OrgMembershipCert {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("OrgMembershipCert")
            .field("org_id", &self.org_id)
            .field("member", &self.member)
            .field("not_before", &self.not_before)
            .field("not_after", &self.not_after)
            .field("generation", &self.generation)
            .field("nonce", &self.nonce)
            .finish()
    }
}

// Serde rides the canonical wire bytes: hex when human-readable (the
// announcement's JSON codec, config files), raw bytes otherwise.
// Decode goes through `from_bytes`, so the strict exact-length
// contract holds in every serialized form and there is exactly one
// byte layout to keep canonical.
impl serde::Serialize for OrgMembershipCert {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let bytes = self.to_bytes();
        if serializer.is_human_readable() {
            serializer.serialize_str(&hex_lower(&bytes))
        } else {
            serializer.serialize_bytes(&bytes)
        }
    }
}

impl<'de> serde::Deserialize<'de> for OrgMembershipCert {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let bytes = if deserializer.is_human_readable() {
            let hex_str = String::deserialize(deserializer)?;
            unhex(&hex_str).map_err(serde::de::Error::custom)?
        } else {
            <Vec<u8>>::deserialize(deserializer)?
        };
        Self::from_bytes(&bytes).map_err(serde::de::Error::custom)
    }
}

/// One `(member, minimum_generation)` floor inside an
/// [`OrgRevocationBundle`].
pub type OrgFloor = (EntityId, u32);

/// A signed set of per-member revocation floors for one org:
/// "every certificate I issued to `member` with
/// `generation < minimum_generation` is revoked."
///
/// Wire format (`108 + 36·n` bytes):
/// ```text
/// org_id:       32 bytes (OrgId — issuing org's root key)
/// issued_at:     8 bytes (u64 unix seconds; audit/ordering hint
///                         only — merge is generation-monotone and
///                         never trusts wall clocks)
/// floor_count:   4 bytes (u32 LE)
/// floors:       36 bytes each — member (32) ‖ minimum_generation
///               (4, u32 LE) — in strictly-ascending member byte
///               order, no duplicates
/// --- signed above (with ORG_FLOORS_SIG_DOMAIN prefixed) ---
/// signature:    64 bytes (ed25519 by org_id)
/// ```
///
/// Canonical member ordering is part of the transcript's
/// injectivity contract: a permuted floor list is a different byte
/// string claiming the same logical content, so decode rejects it
/// before looking at the signature. On the leaf the verified floors
/// merge into [`crate::org::revocation::RevocationFacts`], where a
/// lower floor never rolls back a higher one.
#[derive(Clone, PartialEq, Eq)]
pub struct OrgRevocationBundle {
    /// The organization whose certificate floors these are (also
    /// the verifying key for `signature`).
    pub org_id: OrgId,
    /// When the operator issued the bundle (unix seconds).
    /// Informational: merge ordering is by generation maxima only.
    pub issued_at: u64,
    /// `(member, minimum_generation)` in strictly-ascending member
    /// byte order. Private so every constructed value upholds the
    /// canonical-order invariant.
    floors: Vec<OrgFloor>,
    /// ed25519 signature over
    /// `ORG_FLOORS_SIG_DOMAIN ‖ signed payload`.
    pub signature: [u8; 64],
}

impl OrgRevocationBundle {
    /// Fixed header size: org_id (32) + issued_at (8) + floor_count (4).
    const HEADER_SIZE: usize = 32 + 8 + 4;

    /// Encoded size of one floor entry.
    const FLOOR_ENTRY_SIZE: usize = 32 + 4;

    /// Issue a signed bundle from a floor map, stamped
    /// `now_unix_secs`. The `BTreeMap` iterates in ascending member
    /// order, so canonical ordering holds by construction.
    pub fn try_issue(
        org: &OrgKeypair,
        floors: &BTreeMap<EntityId, u32>,
        now_unix_secs: u64,
    ) -> Result<Self, OrgError> {
        Self::issue_at(org, floors, now_unix_secs)
    }

    /// [`Self::try_issue`] with an explicit `issued_at`; `pub` for
    /// deterministic golden-vector pinning.
    pub fn issue_at(
        org: &OrgKeypair,
        floors: &BTreeMap<EntityId, u32>,
        issued_at: u64,
    ) -> Result<Self, OrgError> {
        if floors.len() > MAX_REVOCATION_FLOORS_PER_BUNDLE {
            return Err(OrgError::TooManyFloors);
        }
        let mut bundle = Self {
            org_id: org.org_id(),
            issued_at,
            floors: floors.iter().map(|(m, g)| (m.clone(), *g)).collect(),
            signature: [0u8; 64],
        };
        bundle.signature = org.sign(&bundle.signing_input());
        Ok(bundle)
    }

    /// The floors, in canonical (ascending member) order.
    pub fn floors(&self) -> &[OrgFloor] {
        &self.floors
    }

    /// Canonical signed payload:
    /// `org_id ‖ issued_at ‖ floor_count ‖ floors…`.
    fn signed_payload(&self) -> Vec<u8> {
        let mut buf =
            Vec::with_capacity(Self::HEADER_SIZE + self.floors.len() * Self::FLOOR_ENTRY_SIZE);
        buf.extend_from_slice(self.org_id.as_bytes());
        buf.extend_from_slice(&self.issued_at.to_le_bytes());
        buf.extend_from_slice(&(self.floors.len() as u32).to_le_bytes());
        for (member, floor) in &self.floors {
            buf.extend_from_slice(member.as_bytes());
            buf.extend_from_slice(&floor.to_le_bytes());
        }
        buf
    }

    /// Domain-prefixed signing input:
    /// `ORG_FLOORS_SIG_DOMAIN ‖ signed_payload`.
    fn signing_input(&self) -> Vec<u8> {
        let payload = self.signed_payload();
        let mut buf = Vec::with_capacity(ORG_FLOORS_SIG_DOMAIN.len() + payload.len());
        buf.extend_from_slice(ORG_FLOORS_SIG_DOMAIN);
        buf.extend_from_slice(&payload);
        buf
    }

    /// Verify structural canonicality and the org signature.
    ///
    /// Order of checks: floor-count cap, strictly-ascending member
    /// order (an in-memory value could have been built outside the
    /// issue path), then `verify_strict` of the domain-prefixed
    /// payload against `org_id`.
    pub fn verify(&self) -> Result<(), OrgError> {
        if self.floors.len() > MAX_REVOCATION_FLOORS_PER_BUNDLE {
            return Err(OrgError::TooManyFloors);
        }
        if !floors_strictly_ascending(&self.floors) {
            return Err(OrgError::NonCanonicalFloors);
        }
        self.org_id.verify(&self.signing_input(), &self.signature)
    }

    /// Serialize to canonical wire format.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = self.signed_payload();
        buf.extend_from_slice(&self.signature);
        buf
    }

    /// Deserialize from wire format.
    ///
    /// Strict: the floor count is validated against
    /// [`MAX_REVOCATION_FLOORS_PER_BUNDLE`] BEFORE any allocation;
    /// the total length must then equal exactly
    /// `header + count·entry + signature` (no truncation, no
    /// trailing bytes); and the member keys must be strictly
    /// ascending. Decoding does NOT verify the signature.
    pub fn from_bytes(data: &[u8]) -> Result<Self, OrgError> {
        if data.len() < Self::HEADER_SIZE + 64 {
            return Err(OrgError::InvalidFormat);
        }
        let org_id = OrgId::from_bytes(data[0..32].try_into().unwrap());
        let issued_at = u64::from_le_bytes(data[32..40].try_into().unwrap());
        let count = u32::from_le_bytes(data[40..44].try_into().unwrap()) as usize;
        if count > MAX_REVOCATION_FLOORS_PER_BUNDLE {
            return Err(OrgError::TooManyFloors);
        }
        let expected = Self::HEADER_SIZE + count * Self::FLOOR_ENTRY_SIZE + 64;
        if data.len() != expected {
            return Err(OrgError::InvalidFormat);
        }
        let mut floors = Vec::with_capacity(count);
        let mut off = Self::HEADER_SIZE;
        for _ in 0..count {
            let member = EntityId::from_bytes(data[off..off + 32].try_into().unwrap());
            let floor = u32::from_le_bytes(data[off + 32..off + 36].try_into().unwrap());
            floors.push((member, floor));
            off += Self::FLOOR_ENTRY_SIZE;
        }
        if !floors_strictly_ascending(&floors) {
            return Err(OrgError::NonCanonicalFloors);
        }
        let mut signature = [0u8; 64];
        signature.copy_from_slice(&data[off..off + 64]);
        Ok(Self {
            org_id,
            issued_at,
            floors,
            signature,
        })
    }
}

impl core::fmt::Debug for OrgRevocationBundle {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("OrgRevocationBundle")
            .field("org_id", &self.org_id)
            .field("issued_at", &self.issued_at)
            .field("floors", &self.floors.len())
            .finish()
    }
}

// Same serde-over-wire-bytes discipline as `OrgMembershipCert`.
impl serde::Serialize for OrgRevocationBundle {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let bytes = self.to_bytes();
        if serializer.is_human_readable() {
            serializer.serialize_str(&hex_lower(&bytes))
        } else {
            serializer.serialize_bytes(&bytes)
        }
    }
}

impl<'de> serde::Deserialize<'de> for OrgRevocationBundle {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let bytes = if deserializer.is_human_readable() {
            let hex_str = String::deserialize(deserializer)?;
            unhex(&hex_str).map_err(serde::de::Error::custom)?
        } else {
            <Vec<u8>>::deserialize(deserializer)?
        };
        Self::from_bytes(&bytes).map_err(serde::de::Error::custom)
    }
}

/// `true` iff members are strictly ascending in byte order (implies
/// no duplicates). Relies on `EntityId`'s derived lexicographic
/// `Ord`.
fn floors_strictly_ascending(floors: &[OrgFloor]) -> bool {
    floors.windows(2).all(|w| w[0].0 < w[1].0)
}
