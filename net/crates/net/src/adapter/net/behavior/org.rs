//! Organization identity, membership, and revocation floors — OA-1
//! of `docs/plans/ORG_CAPABILITY_AUTH_PLAN.md` (scaffolded
//! ownership).
//!
//! An organization is identified by its ed25519 verifying key
//! ([`OrgId`]) — self-certifying, no registry. The org root key is
//! held offline by an operator; issuance is occasional (roughly
//! yearly certificate renewal), never per-call. Three authority
//! objects ship in OA-1:
//!
//! - [`OrgMembershipCert`] — "node S belongs to org A", signed by
//!   A's root under [`ORG_CERT_SIG_DOMAIN`]. Carried optionally on
//!   the capability announcement and projected into the fold as
//!   `owner_org` **only after ingest verification**.
//! - [`OrgRevocationBundle`] — a signed set of per-member
//!   generation floors under [`ORG_FLOORS_SIG_DOMAIN`]. Merged
//!   monotonically into the node-local persisted
//!   revocation state (see `org_revocation.rs`): a lower floor
//!   never rolls back a higher one, including across restart.
//! - [`OrgKeypair`] — the offline root key. CLI-side only; the
//!   secret never rides the mesh and there is deliberately no
//!   "public-only" variant (contrast
//!   [`EntityKeypair`](crate::adapter::net::identity::EntityKeypair)):
//!   a node never holds an org root, so there is no migration path
//!   that could strand a verifying half here.
//!
//! # Membership is never invocation authority
//!
//! Nothing in this module participates in `may_execute` or any
//! execute-authorization axis. OA-1 is authority-dark scaffolding:
//! a verified certificate proves *belonging* and feeds discovery
//! projections only. Admission (OA-2) is a separate, per-call
//! proof.
//!
//! # `OrgId` equality is deliberately NOT constant-time
//!
//! [`OrgId`] uses the derived `PartialEq`, unlike
//! [`GroupId`](super::group::GroupId) / [`SubnetId`](super::subnet::SubnetId)
//! which fold through `subtle::ConstantTimeEq`. Those are **bearer
//! secrets** — knowing the 32 bytes *is* membership, so a
//! data-dependent early-exit compare leaks the secret through
//! timing. An `OrgId` is a *public key*: knowing it grants nothing,
//! every announcement that carries a cert names it in plaintext,
//! and timing reveals only what the wire already says. Do not
//! "fix" this into `ct_eq` — and do not conclude from this that
//! `GroupId`'s constant-time compare is optional; the two types
//! answer different threat models.
//!
//! # Domain separation
//!
//! Signatures cover a domain-prefixed transcript
//! (`domain ‖ canonical payload bytes`). The `-v1` suffix IS the
//! transcript version: any change to a signed field list requires
//! a NEW domain string, never a silent reinterpretation of the old
//! one.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

use crate::adapter::net::identity::{EntityId, MAX_TOKEN_CLOCK_SKEW_SECS};

/// Signature domain for [`OrgMembershipCert`]. Prefixed to the
/// 92-byte canonical payload before signing so a certificate
/// signature can never be replayed as any other org object (and
/// vice versa).
pub const ORG_CERT_SIG_DOMAIN: &[u8] = b"net-org-cert-v1";

/// Signature domain for [`OrgRevocationBundle`].
pub const ORG_FLOORS_SIG_DOMAIN: &[u8] = b"net-org-floors-v1";

/// Recommended membership-certificate TTL (~1 year). Renewal is
/// silent re-issue; see the plan's cert discipline. Mirrors the
/// shape of `MAX_TOKEN_TTL_SECS` in the token module but is a
/// distinct constant — org certs are long-lived belonging
/// statements, not per-channel grants.
pub const ORG_CERT_TTL_SECS_RECOMMENDED: u64 = 365 * 24 * 60 * 60;

/// Hard upper bound on a membership certificate's validity window
/// (2 years). Enforced at issue (`try_issue` rejects a longer
/// `duration_secs`) AND at verify (`verify` rejects a foreign cert
/// whose `not_after - not_before` exceeds it) — a peer must not be
/// able to mint an effectively immortal belonging statement that
/// only its own issuer would have refused.
pub const MAX_ORG_CERT_TTL_SECS: u64 = 2 * 365 * 24 * 60 * 60;

/// Hard cap on floors carried by one [`OrgRevocationBundle`].
/// Bundles are operator-distributed local files, not mesh frames,
/// so the bound exists to keep hostile-input decode allocation
/// proportionate rather than to fit a packet ceiling. 65 536
/// members with simultaneously bumped floors is far past any real
/// fleet; the check runs BEFORE any allocation.
pub const MAX_REVOCATION_FLOORS_PER_BUNDLE: usize = 65_536;

/// Organization identity — a 32-byte ed25519 verifying key.
///
/// Self-certifying: the id IS the key; there is no registry. The
/// derived (non-constant-time) `PartialEq` is deliberate — see the
/// module docs before changing it.
///
/// `Ord` is the lexicographic byte order; it doubles as the
/// canonical ordering for persisted floor maps
/// (`BTreeMap<(OrgId, EntityId), u32>` in the revocation state).
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

    /// Interpret as an ed25519 verifying key.
    pub fn verifying_key(&self) -> Result<VerifyingKey, OrgError> {
        VerifyingKey::from_bytes(&self.0).map_err(|_| OrgError::InvalidPublicKey)
    }

    /// Verify a signature against this org's root key.
    ///
    /// Uses `verify_strict` — same malleability rationale as
    /// [`EntityId::verify`]: certs and bundles are compared and
    /// cached on their signed bytes, so the `(R, S + L)` malleated
    /// variant of a signature must not verify as a second encoding
    /// of the same logical object.
    pub fn verify(&self, message: &[u8], signature: &Signature) -> Result<(), OrgError> {
        let vk = self.verifying_key()?;
        vk.verify_strict(message, signature)
            .map_err(|_| OrgError::InvalidSignature)
    }
}

impl std::fmt::Debug for OrgId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "OrgId({})", hex_short(&self.0))
    }
}

impl std::fmt::Display for OrgId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&hex::encode(self.0))
    }
}

// Hex string when human-readable (JSON config files / announcement
// envelope), raw bytes otherwise — mirror `EntityId`'s impls so the
// two identity kinds read identically in every serialized form.
impl serde::Serialize for OrgId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        if serializer.is_human_readable() {
            serializer.serialize_str(&hex::encode(self.0))
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
            hex::decode(&hex_str).map_err(serde::de::Error::custom)?
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

/// Offline organization root keypair.
///
/// Lives on an operator machine and signs certificates and
/// revocation bundles through the CLI; it is never loaded by a mesh
/// node and never rides the wire. Deliberately simpler than
/// [`EntityKeypair`](crate::adapter::net::identity::EntityKeypair):
/// no public-only mode (no migration path ever strands a verifying
/// half here) and therefore no fallible `try_sign` split.
pub struct OrgKeypair {
    signing_key: SigningKey,
    org_id: OrgId,
}

impl OrgKeypair {
    /// Generate a new random org root keypair.
    ///
    /// `getrandom::fill` failure aborts rather than panicking —
    /// identical rationale to `EntityKeypair::generate`: predictable
    /// bytes produce a forgeable ed25519 secret, and `abort` cannot
    /// unwind across a future FFI frame.
    pub fn generate() -> Self {
        let mut rng_bytes = [0u8; 32];
        if let Err(e) = getrandom::fill(&mut rng_bytes) {
            eprintln!(
                "FATAL: OrgKeypair::generate getrandom failure ({e:?}); aborting to avoid weak ed25519 secret"
            );
            std::process::abort();
        }
        let signing_key = SigningKey::from_bytes(&rng_bytes);
        // Zeroize secret material — volatile write prevents optimizer elision
        for byte in rng_bytes.iter_mut() {
            // SAFETY: `byte` is a valid mutable reference into `rng_bytes`
            // for the lifetime of this loop iteration, which is all
            // `ptr::write_volatile` requires.
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
        Self::from_signing_key(signing_key)
    }

    /// Create from an existing ed25519 signing key.
    pub fn from_signing_key(signing_key: SigningKey) -> Self {
        let org_id = OrgId::from_bytes(signing_key.verifying_key().to_bytes());
        Self {
            signing_key,
            org_id,
        }
    }

    /// Create from raw secret key bytes (32-byte seed).
    pub fn from_bytes(secret: [u8; 32]) -> Self {
        Self::from_signing_key(SigningKey::from_bytes(&secret))
    }

    /// The public organization identity for this root key.
    #[inline]
    pub fn org_id(&self) -> OrgId {
        self.org_id
    }

    /// Raw 32-byte secret seed. Handle with care — this is the org
    /// root secret; it belongs in an operator-side key file, never
    /// in node config or on the wire.
    pub fn secret_bytes(&self) -> &[u8; 32] {
        self.signing_key.as_bytes()
    }

    /// Sign a message with the org root key. `pub(crate)`: the
    /// OA-2 grant family (`org_grant.rs`) signs through the same
    /// root; the public issuing surfaces remain the typed
    /// `try_issue` paths, never raw signing.
    pub(crate) fn sign(&self, message: &[u8]) -> Signature {
        self.signing_key.sign(message)
    }
}

impl std::fmt::Debug for OrgKeypair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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
    /// Org id bytes are not a valid ed25519 point.
    InvalidPublicKey,
    /// Signature verification failed.
    InvalidSignature,
    /// `duration_secs == 0` passed at issue time. The signature
    /// would verify but the window is born empty — reject with a
    /// typed error instead of minting an unusable credential
    /// (mirrors `TokenError::ZeroTtl`).
    ZeroTtl,
    /// Validity window exceeds [`MAX_ORG_CERT_TTL_SECS`]. Raised at
    /// issue AND at verify — see the constant's docs.
    TtlTooLong,
    /// `not_after <= not_before` — a zero or reversed validity
    /// window is structurally invalid (review-8). A reversed
    /// window is not merely unusable: with ordinary allowed clock
    /// skew, `now >= not_before - skew` and `now < not_after +
    /// skew` can BOTH hold for a short reversed window, admitting
    /// a certificate that was never live. Rejected at verify
    /// before any TTL or signature work.
    InvalidValidityWindow,
    /// The caller passed a clock-skew tolerance above
    /// [`MAX_TOKEN_CLOCK_SKEW_SECS`] (the token-module ceiling).
    /// Enforced inside [`OrgMembershipCert::is_valid_with_skew`]
    /// rather than documented at the call sites — with saturating
    /// window arithmetic, an unbounded skew admits any expired
    /// certificate.
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
    /// Bundle floors are not in strictly-ascending member order
    /// (out of order, or duplicate member keys). Canonical order is
    /// part of the signed transcript's injectivity contract, so a
    /// non-canonical bundle is rejected before its signature is
    /// even examined.
    NonCanonicalFloors,
    /// A capability grant carries the all-zero `grant_id` (OA-2).
    /// Zero is RESERVED — it is the owner-audience envelope
    /// sentinel in OA-3's associated data — so issuance and decode
    /// both reject it (plan v1.3 carry-forward).
    ReservedGrantId,
    /// A capability grant violates the structural rule
    /// `rights ⊇ DISCOVER ⇔ discovery binding present` (OA-2
    /// §2.2): DISCOVER without a binding grants a right with no
    /// audience, a binding without DISCOVER smuggles audience
    /// material into a grant that confers no discovery. Enforced
    /// at issue AND decode.
    DiscoveryBindingMismatch,
    /// A grant's rights bitset carries bits this build does not
    /// know (OA-2). Unknown rights could widen authority under an
    /// old verifier — wire evolution is honest, so they refuse
    /// loudly instead of being masked off.
    UnknownRights,
    /// A grant's rights bitset is empty (OA-2). A credential that
    /// permits nothing is structurally meaningless — minting or
    /// accepting it can only hide a caller bug.
    EmptyRights,
    /// A capability grant's `AnyNodeOwnedBy(org)` target names an org
    /// OTHER than the issuer (OA-2, Kyra OA2-F). B grants access to
    /// nodes IT owns; `AnyNodeOwnedBy(C != B)` names providers owned
    /// by a foreign org C and can NEVER admit (admission requires the
    /// provider's owner == issuer), so it is refused at issue AND
    /// decode rather than minting a permanently-unusable credential.
    TargetOrgNotIssuer,
}

impl std::fmt::Display for OrgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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
/// not_after:     8 bytes (u64 unix seconds)
/// generation:    4 bytes (u32; floor-checked against the persisted
///                         revocation maxima — a cert below its
///                         org's floor for this member is dead)
/// nonce:         8 bytes (u64; makes re-issues byte-distinct)
/// --- signed above (with ORG_CERT_SIG_DOMAIN prefixed) ---
/// signature:    64 bytes (ed25519 by org_id)
/// ```
///
/// Both bounds are **inclusive-expiry** like `PermissionToken`:
/// live while `not_before <= now < not_after`.
///
/// A verified certificate proves *belonging only*. It never enters
/// `may_execute`, and holding one grants no invocation authority.
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
    /// `generation < n`; re-issuing at a higher generation is how
    /// an org retires a member's outstanding certs without waiting
    /// for expiry.
    pub generation: u32,
    /// Random per-issue nonce so silent renewals are byte-distinct.
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

    /// Issue a certificate valid from now for `duration_secs`.
    ///
    /// Rejects `duration_secs == 0` ([`OrgError::ZeroTtl`]) and
    /// `duration_secs > MAX_ORG_CERT_TTL_SECS`
    /// ([`OrgError::TtlTooLong`]) so misuse surfaces as a typed
    /// error at the issuing CLI instead of as silent rejection on
    /// every receiver.
    pub fn try_issue(
        org: &OrgKeypair,
        member: EntityId,
        generation: u32,
        duration_secs: u64,
    ) -> Result<Self, OrgError> {
        if duration_secs == 0 {
            return Err(OrgError::ZeroTtl);
        }
        if duration_secs > MAX_ORG_CERT_TTL_SECS {
            return Err(OrgError::TtlTooLong);
        }
        // Abort on getrandom failure rather than panic-unwinding —
        // a predictable nonce would let re-issued certs collide
        // byte-for-byte, breaking the byte-distinct-renewal
        // contract. Same rationale as PermissionToken::try_issue.
        let mut nonce_bytes = [0u8; 8];
        if let Err(e) = getrandom::fill(&mut nonce_bytes) {
            eprintln!(
                "FATAL: OrgMembershipCert nonce getrandom failure ({e:?}); aborting to avoid predictable cert nonce"
            );
            std::process::abort();
        }
        let nonce = u64::from_le_bytes(nonce_bytes);
        let now = current_timestamp();
        Ok(Self::issue_at(
            org,
            member,
            generation,
            now,
            now.saturating_add(duration_secs),
            nonce,
        ))
    }

    /// Build and sign a certificate with fully explicit fields.
    ///
    /// `pub(crate)` — the public issuing surface is [`Self::try_issue`],
    /// which enforces the TTL discipline. Golden-vector tests and
    /// in-crate tooling use this to pin deterministic bytes.
    pub(crate) fn issue_at(
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
        cert.signature = org.sign(&cert.signing_input()).to_bytes();
        cert
    }

    /// Canonical signed payload — fixed offsets, little-endian.
    /// `pub(crate)` for the same reason as
    /// `PermissionToken::signed_payload`: a `pub` transcript builder
    /// would let any key holder mint signed bytes outside the
    /// invariant-enforcing issue path.
    pub(crate) fn signed_payload(&self) -> [u8; Self::SIGNED_PAYLOAD_SIZE] {
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
    /// (`not_after > not_before` — zero or reversed windows are
    /// structurally invalid, [`OrgError::InvalidValidityWindow`]);
    /// the window length is within [`MAX_ORG_CERT_TTL_SECS`]
    /// (enforced at verify as well as at issue — see the
    /// constant); then `verify_strict` of the domain-prefixed
    /// payload against `org_id`.
    ///
    /// Does NOT check wall-clock bounds or revocation floors —
    /// those are contextual; use [`Self::is_valid_with_skew`] for
    /// the time window and the persisted revocation state for
    /// floors.
    pub fn verify(&self) -> Result<(), OrgError> {
        if self.not_after <= self.not_before {
            return Err(OrgError::InvalidValidityWindow);
        }
        if self.not_after - self.not_before > MAX_ORG_CERT_TTL_SECS {
            return Err(OrgError::TtlTooLong);
        }
        let sig = Signature::from_bytes(&self.signature);
        self.org_id.verify(&self.signing_input(), &sig)
    }

    /// Signature + wall-clock validity with `skew_secs` of clock
    /// tolerance on both bounds — accepted while
    /// `now >= not_before - skew` AND `now < not_after + skew`.
    ///
    /// Skew semantics are identical to the token module's
    /// (`PermissionToken::is_valid_with_skew`), and the token
    /// ceiling is ENFORCED here (review-8): a tolerance above
    /// [`MAX_TOKEN_CLOCK_SKEW_SECS`] is rejected with
    /// [`OrgError::ClockSkewTooLarge`] rather than trusted to a
    /// documentation clause — saturating window arithmetic would
    /// otherwise let an unbounded skew admit any expired
    /// certificate.
    pub fn is_valid_with_skew(&self, skew_secs: u64) -> Result<(), OrgError> {
        self.is_valid_at_with_skew(current_timestamp(), skew_secs)
    }

    /// Explicit-time variant (Kyra E1 audit): validate against a
    /// caller-supplied `now_secs` (unix seconds) instead of re-reading
    /// the wall clock, so one admission decision uses a SINGLE clock
    /// sample for every credential/proof freshness check.
    pub fn is_valid_at_with_skew(&self, now_secs: u64, skew_secs: u64) -> Result<(), OrgError> {
        if skew_secs > MAX_TOKEN_CLOCK_SKEW_SECS {
            return Err(OrgError::ClockSkewTooLarge);
        }
        self.verify()?;
        self.check_time_bounds_at(now_secs, skew_secs)
    }

    /// Wall-clock window check at an explicit `now` (unix seconds),
    /// without signature verification — same split as the token
    /// module: signatures are immutable, expiry must be re-evaluated
    /// per use.
    fn check_time_bounds_at(&self, now: u64, skew_secs: u64) -> Result<(), OrgError> {
        // `saturating_sub` pins the lower comparison at 0 when
        // `not_before < skew`; `saturating_add` keeps a saturated
        // `not_after` forever-valid instead of wrapping.
        if now < self.not_before.saturating_sub(skew_secs) {
            return Err(OrgError::NotYetValid);
        }
        if now >= self.not_after.saturating_add(skew_secs) {
            return Err(OrgError::Expired);
        }
        Ok(())
    }

    /// Pure time check: `true` iff wall clock has reached
    /// `not_after`. Boundary convention matches the token module:
    /// `now == not_after` ⇒ expired.
    pub fn is_expired(&self) -> bool {
        current_timestamp() >= self.not_after
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
    #[expect(
        clippy::unwrap_used,
        reason = "data.len() == WIRE_SIZE checked above; fixed-offset slices convert infallibly to fixed-size arrays"
    )]
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

impl std::fmt::Debug for OrgMembershipCert {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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

// Serde rides the canonical wire bytes: hex string when
// human-readable (the announcement's JSON codec, config files), raw
// bytes otherwise. Decode goes through `from_bytes`, so the strict
// exact-length contract holds in every serialized form and there is
// exactly one byte layout to keep canonical.
impl serde::Serialize for OrgMembershipCert {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let bytes = self.to_bytes();
        if serializer.is_human_readable() {
            serializer.serialize_str(&hex::encode(&bytes))
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
            hex::decode(&hex_str).map_err(serde::de::Error::custom)?
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
/// before looking at the signature (mirroring the sensing module's
/// refuse-to-recanonicalize discipline).
///
/// Distribution is operator-side (plain local files); a node merges
/// verified bundles into its persisted [`OrgRevocationState`]
/// maxima, where a lower floor never rolls back a higher one — see
/// `org_revocation.rs`.
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

    /// Issue a signed bundle from a floor map. The `BTreeMap`
    /// iterates in ascending member order, so canonical ordering
    /// holds by construction.
    pub fn try_issue(org: &OrgKeypair, floors: &BTreeMap<EntityId, u32>) -> Result<Self, OrgError> {
        Self::issue_at(org, floors, current_timestamp())
    }

    /// [`Self::try_issue`] with an explicit `issued_at`;
    /// `pub(crate)` for deterministic golden-vector tests.
    pub(crate) fn issue_at(
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
        bundle.signature = org.sign(&bundle.signing_input()).to_bytes();
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
    /// order (an in-memory value could have been built outside
    /// `try_issue`/`from_bytes`), then `verify_strict` of the
    /// domain-prefixed payload against `org_id`.
    pub fn verify(&self) -> Result<(), OrgError> {
        if self.floors.len() > MAX_REVOCATION_FLOORS_PER_BUNDLE {
            return Err(OrgError::TooManyFloors);
        }
        if !floors_strictly_ascending(&self.floors) {
            return Err(OrgError::NonCanonicalFloors);
        }
        let sig = Signature::from_bytes(&self.signature);
        self.org_id.verify(&self.signing_input(), &sig)
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
    /// ascending (no duplicates). Decoding does NOT verify the
    /// signature.
    #[expect(
        clippy::unwrap_used,
        reason = "lengths are checked against the exact expected size above; fixed-offset slices convert infallibly to fixed-size arrays"
    )]
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

impl std::fmt::Debug for OrgRevocationBundle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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
            serializer.serialize_str(&hex::encode(&bytes))
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
            hex::decode(&hex_str).map_err(serde::de::Error::custom)?
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

/// Format first 8 bytes as hex for debug display (module-local
/// mirror of the private helper in `identity/entity.rs`).
fn hex_short(bytes: &[u8]) -> String {
    bytes
        .iter()
        .take(8)
        .map(|b| format!("{:02x}", b))
        .collect::<String>()
        + "..."
}

/// Unix seconds. Matches the token module's private helper —
/// pre-epoch clocks collapse to 0 rather than panicking.
/// Unix seconds now. `pub(crate)`: the OA-2 grant family
/// (`org_grant.rs`) shares the org module's clock discipline.
pub(crate) fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn org() -> OrgKeypair {
        OrgKeypair::from_bytes([0x42u8; 32])
    }

    fn member() -> EntityId {
        EntityId::from_bytes([0x24u8; 32])
    }

    fn valid_cert() -> OrgMembershipCert {
        OrgMembershipCert::try_issue(&org(), member(), 5, ORG_CERT_TTL_SECS_RECOMMENDED)
            .expect("issue")
    }

    // ---------------------------------------------------------- OrgId

    #[test]
    fn org_id_round_trips_serde_json_and_postcard() {
        let id = org().org_id();
        let json = serde_json::to_string(&id).expect("json");
        // Human-readable form is the bare hex string.
        assert_eq!(json, format!("\"{}\"", hex::encode(id.as_bytes())));
        let back: OrgId = serde_json::from_str(&json).expect("json back");
        assert_eq!(back, id);

        let bin = postcard::to_allocvec(&id).expect("postcard");
        let back: OrgId = postcard::from_bytes(&bin).expect("postcard back");
        assert_eq!(back, id);
    }

    #[test]
    fn org_id_rejects_wrong_length_serde() {
        let short: Result<OrgId, _> = serde_json::from_str("\"5a5a\"");
        assert!(short.is_err());
    }

    #[test]
    fn org_keypair_deterministic_from_seed_and_distinct_on_generate() {
        assert_eq!(
            OrgKeypair::from_bytes([7u8; 32]).org_id(),
            OrgKeypair::from_bytes([7u8; 32]).org_id()
        );
        assert_ne!(
            OrgKeypair::generate().org_id(),
            OrgKeypair::generate().org_id()
        );
    }

    // ------------------------------------------- OrgMembershipCert

    #[test]
    fn cert_issue_verify_and_wire_round_trip() {
        let cert = valid_cert();
        cert.verify().expect("fresh cert verifies");
        cert.is_valid_with_skew(0).expect("fresh cert in window");

        let bytes = cert.to_bytes();
        assert_eq!(bytes.len(), OrgMembershipCert::WIRE_SIZE);
        let back = OrgMembershipCert::from_bytes(&bytes).expect("decode");
        assert_eq!(back, cert);
        back.verify().expect("decoded cert verifies");
    }

    #[test]
    fn cert_wire_size_is_pinned() {
        // 156 bytes — the plan's announced cert size. A change here
        // is a wire-format break and needs a new signature domain.
        assert_eq!(OrgMembershipCert::WIRE_SIZE, 156);
    }

    #[test]
    fn cert_from_bytes_rejects_truncation_and_trailing() {
        let bytes = valid_cert().to_bytes();
        for cut in 0..bytes.len() {
            assert_eq!(
                OrgMembershipCert::from_bytes(&bytes[..cut]),
                Err(OrgError::InvalidFormat),
                "truncation at {cut} must not decode"
            );
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert_eq!(
            OrgMembershipCert::from_bytes(&trailing),
            Err(OrgError::InvalidFormat)
        );
    }

    #[test]
    fn cert_every_signed_field_is_tamper_evident() {
        type CertMutation = (&'static str, fn(&mut OrgMembershipCert));
        let mutations: [CertMutation; 6] = [
            ("org_id", |c| c.org_id.0[0] ^= 1),
            ("member", |c| c.member.0[0] ^= 1),
            ("not_before", |c| c.not_before ^= 1),
            ("not_after", |c| c.not_after ^= 1),
            ("generation", |c| c.generation ^= 1),
            ("nonce", |c| c.nonce ^= 1),
        ];
        for (field, mutate) in mutations {
            let mut tampered = valid_cert();
            mutate(&mut tampered);
            assert!(
                tampered.verify().is_err(),
                "tampered {field} must fail verification"
            );
        }
        // And the signature itself.
        let mut tampered = valid_cert();
        tampered.signature[0] ^= 1;
        assert_eq!(tampered.verify(), Err(OrgError::InvalidSignature));
    }

    #[test]
    fn cert_signature_domain_is_load_bearing() {
        // Sign the raw payload WITHOUT the domain prefix — a
        // would-be cross-protocol transplant. Verification must
        // fail because the domain is part of the signing input.
        let mut cert = valid_cert();
        cert.signature = org().sign(&cert.signed_payload()).to_bytes();
        assert_eq!(cert.verify(), Err(OrgError::InvalidSignature));

        // And under the WRONG domain (the floors domain).
        let mut input = Vec::new();
        input.extend_from_slice(ORG_FLOORS_SIG_DOMAIN);
        input.extend_from_slice(&cert.signed_payload());
        cert.signature = org().sign(&input).to_bytes();
        assert_eq!(cert.verify(), Err(OrgError::InvalidSignature));
    }

    #[test]
    fn cert_wrong_org_key_fails() {
        let mut cert = valid_cert();
        // Re-sign with a different key while claiming the same org_id.
        let other = OrgKeypair::from_bytes([9u8; 32]);
        cert.signature = other.sign(&cert.signing_input()).to_bytes();
        assert_eq!(cert.verify(), Err(OrgError::InvalidSignature));
    }

    #[test]
    fn cert_ttl_enforced_at_issue_and_verify() {
        assert_eq!(
            OrgMembershipCert::try_issue(&org(), member(), 0, 0),
            Err(OrgError::ZeroTtl)
        );
        assert_eq!(
            OrgMembershipCert::try_issue(&org(), member(), 0, MAX_ORG_CERT_TTL_SECS + 1),
            Err(OrgError::TtlTooLong)
        );
        // A properly-signed cert with a 3-year window must fail
        // VERIFY too — the receiver enforces the ceiling even when
        // the issuer didn't.
        let now = current_timestamp();
        let immortal = OrgMembershipCert::issue_at(
            &org(),
            member(),
            0,
            now,
            now + MAX_ORG_CERT_TTL_SECS + 1,
            7,
        );
        assert_eq!(immortal.verify(), Err(OrgError::TtlTooLong));
        // At exactly the ceiling it is accepted.
        let at_max =
            OrgMembershipCert::issue_at(&org(), member(), 0, now, now + MAX_ORG_CERT_TTL_SECS, 7);
        at_max.verify().expect("window of exactly MAX is legal");
    }

    #[test]
    fn cert_time_bounds_and_skew() {
        let now = current_timestamp();
        // Not yet valid: opens 100s in the future.
        let future = OrgMembershipCert::issue_at(&org(), member(), 0, now + 100, now + 200, 1);
        assert_eq!(future.is_valid_with_skew(0), Err(OrgError::NotYetValid));
        // 120s of skew tolerance admits it.
        future.is_valid_with_skew(120).expect("skew admits future");

        // Expired: closed 100s ago.
        let past = OrgMembershipCert::issue_at(&org(), member(), 0, now - 200, now - 100, 1);
        assert_eq!(past.is_valid_with_skew(0), Err(OrgError::Expired));
        assert!(past.is_expired());
        // 120s of skew tolerance admits it.
        past.is_valid_with_skew(120).expect("skew admits recent");

        // Boundary: now == not_after is already expired (inclusive
        // expiry, matching the token module).
        let boundary = OrgMembershipCert::issue_at(&org(), member(), 0, now - 100, now, 1);
        assert_eq!(boundary.is_valid_with_skew(0), Err(OrgError::Expired));
    }

    #[test]
    fn skew_ceiling_enforced_at_and_above_max() {
        let now = current_timestamp();
        // Expired 100 s ago: the full ceiling tolerance legally
        // admits it (the documented skew semantics)...
        let past = OrgMembershipCert::issue_at(&org(), member(), 0, now - 200, now - 100, 1);
        past.is_valid_with_skew(MAX_TOKEN_CLOCK_SKEW_SECS)
            .expect("skew of exactly MAX is legal");
        // ...one past the ceiling is refused as caller misuse, not
        // applied to the window.
        assert_eq!(
            past.is_valid_with_skew(MAX_TOKEN_CLOCK_SKEW_SECS + 1),
            Err(OrgError::ClockSkewTooLarge)
        );
        assert_eq!(
            past.is_valid_with_skew(u64::MAX),
            Err(OrgError::ClockSkewTooLarge)
        );
        // Even a currently-valid cert refuses an over-ceiling
        // tolerance — the ceiling gates the CALL, not the outcome.
        assert_eq!(
            valid_cert().is_valid_with_skew(MAX_TOKEN_CLOCK_SKEW_SECS + 1),
            Err(OrgError::ClockSkewTooLarge)
        );
    }

    #[test]
    fn zero_and_reversed_windows_are_structurally_invalid() {
        let now = current_timestamp();
        // not_after == not_before → reject.
        let zero = OrgMembershipCert::issue_at(&org(), member(), 0, now, now, 1);
        assert_eq!(zero.verify(), Err(OrgError::InvalidValidityWindow));
        // not_after < not_before → reject. The review-8 scenario: a
        // short reversed window straddling `now` passes BOTH
        // saturating time comparisons under ordinary legal skew, so
        // structural verification must kill it first.
        let reversed = OrgMembershipCert::issue_at(&org(), member(), 0, now + 100, now - 100, 1);
        assert_eq!(reversed.verify(), Err(OrgError::InvalidValidityWindow));
        assert_eq!(
            reversed.is_valid_with_skew(120),
            Err(OrgError::InvalidValidityWindow)
        );
        // A positive window proceeds to TTL/signature checks.
        valid_cert().verify().expect("positive window verifies");
    }

    #[test]
    fn cert_serde_json_and_postcard_round_trip() {
        let cert = valid_cert();
        let json = serde_json::to_string(&cert).expect("json");
        // Human-readable form is the hex of the canonical wire bytes.
        assert_eq!(json, format!("\"{}\"", hex::encode(cert.to_bytes())));
        let back: OrgMembershipCert = serde_json::from_str(&json).expect("json back");
        assert_eq!(back, cert);

        let bin = postcard::to_allocvec(&cert).expect("postcard");
        let back: OrgMembershipCert = postcard::from_bytes(&bin).expect("postcard back");
        assert_eq!(back, cert);
    }

    #[test]
    fn cert_serde_rejects_malformed() {
        // Wrong length hex.
        let short = format!("\"{}\"", hex::encode([0u8; 10]));
        assert!(serde_json::from_str::<OrgMembershipCert>(&short).is_err());
        // Non-hex.
        assert!(serde_json::from_str::<OrgMembershipCert>("\"zz\"").is_err());
    }

    // ---------------------------------------- OrgRevocationBundle

    fn floors_fixture() -> BTreeMap<EntityId, u32> {
        let mut floors = BTreeMap::new();
        floors.insert(EntityId::from_bytes([1u8; 32]), 3);
        floors.insert(EntityId::from_bytes([2u8; 32]), 7);
        floors.insert(EntityId::from_bytes([3u8; 32]), 1);
        floors
    }

    #[test]
    fn bundle_issue_verify_and_wire_round_trip() {
        let bundle = OrgRevocationBundle::try_issue(&org(), &floors_fixture()).expect("issue");
        bundle.verify().expect("fresh bundle verifies");
        assert_eq!(bundle.floors().len(), 3);

        let bytes = bundle.to_bytes();
        assert_eq!(bytes.len(), 32 + 8 + 4 + 3 * 36 + 64);
        let back = OrgRevocationBundle::from_bytes(&bytes).expect("decode");
        assert_eq!(back, bundle);
        back.verify().expect("decoded bundle verifies");
    }

    #[test]
    fn bundle_empty_floors_is_legal() {
        let bundle = OrgRevocationBundle::try_issue(&org(), &BTreeMap::new()).expect("issue");
        bundle.verify().expect("empty bundle verifies");
        let back = OrgRevocationBundle::from_bytes(&bundle.to_bytes()).expect("decode");
        assert_eq!(back.floors().len(), 0);
    }

    #[test]
    fn bundle_from_bytes_rejects_truncation_and_trailing() {
        let bytes = OrgRevocationBundle::try_issue(&org(), &floors_fixture())
            .expect("issue")
            .to_bytes();
        for cut in 0..bytes.len() {
            assert!(
                OrgRevocationBundle::from_bytes(&bytes[..cut]).is_err(),
                "truncation at {cut} must not decode"
            );
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert_eq!(
            OrgRevocationBundle::from_bytes(&trailing),
            Err(OrgError::InvalidFormat)
        );
    }

    #[test]
    fn bundle_rejects_non_canonical_floor_order() {
        let bundle = OrgRevocationBundle::try_issue(&org(), &floors_fixture()).expect("issue");
        let mut bytes = bundle.to_bytes();
        // Swap the first two 36-byte floor entries in place. The
        // byte string still parses structurally but violates the
        // canonical ascending order.
        let header = 32 + 8 + 4;
        let (a, b) = (header, header + 36);
        let first: Vec<u8> = bytes[a..a + 36].to_vec();
        let second: Vec<u8> = bytes[b..b + 36].to_vec();
        bytes[a..a + 36].copy_from_slice(&second);
        bytes[b..b + 36].copy_from_slice(&first);
        assert_eq!(
            OrgRevocationBundle::from_bytes(&bytes),
            Err(OrgError::NonCanonicalFloors)
        );

        // Duplicate member keys are equally non-canonical.
        let mut dup = bundle.to_bytes();
        let entry: Vec<u8> = dup[a..a + 36].to_vec();
        dup[b..b + 36].copy_from_slice(&entry);
        assert_eq!(
            OrgRevocationBundle::from_bytes(&dup),
            Err(OrgError::NonCanonicalFloors)
        );
    }

    #[test]
    fn bundle_count_cap_checked_before_allocation() {
        // Craft a header claiming MAX+1 floors with no body. The
        // decoder must reject on the count BEFORE trusting it for
        // allocation — TooManyFloors, not a huge Vec::with_capacity.
        let mut data = Vec::new();
        data.extend_from_slice(&[0u8; 32]); // org_id
        data.extend_from_slice(&0u64.to_le_bytes()); // issued_at
        data.extend_from_slice(&((MAX_REVOCATION_FLOORS_PER_BUNDLE as u32) + 1).to_le_bytes());
        data.extend_from_slice(&[0u8; 64]); // signature
        assert_eq!(
            OrgRevocationBundle::from_bytes(&data),
            Err(OrgError::TooManyFloors)
        );
    }

    #[test]
    fn bundle_tamper_evident() {
        let bundle = OrgRevocationBundle::try_issue(&org(), &floors_fixture()).expect("issue");
        let bytes = bundle.to_bytes();

        // Flip one byte at every position of the signed payload;
        // each mutation must fail decode (structure) or verify
        // (signature) — never pass both.
        for pos in 0..bytes.len() - 64 {
            let mut tampered = bytes.clone();
            tampered[pos] ^= 1;
            let ok = OrgRevocationBundle::from_bytes(&tampered)
                .and_then(|b| b.verify())
                .is_ok();
            assert!(!ok, "flipped byte {pos} must not survive decode+verify");
        }
    }

    #[test]
    fn bundle_signature_domain_is_load_bearing() {
        let mut bundle = OrgRevocationBundle::try_issue(&org(), &floors_fixture()).expect("issue");
        // Re-sign the raw payload without the domain prefix.
        bundle.signature = org().sign(&bundle.signed_payload()).to_bytes();
        assert_eq!(bundle.verify(), Err(OrgError::InvalidSignature));
        // Re-sign under the cert domain.
        let mut input = Vec::new();
        input.extend_from_slice(ORG_CERT_SIG_DOMAIN);
        input.extend_from_slice(&bundle.signed_payload());
        bundle.signature = org().sign(&input).to_bytes();
        assert_eq!(bundle.verify(), Err(OrgError::InvalidSignature));
    }

    #[test]
    fn bundle_too_many_floors_rejected_at_issue() {
        // Build a map one past the cap. 65 537 tiny inserts is fast
        // enough for a unit test and exercises the real path.
        let mut floors = BTreeMap::new();
        for i in 0..=MAX_REVOCATION_FLOORS_PER_BUNDLE {
            let mut m = [0u8; 32];
            m[0..8].copy_from_slice(&(i as u64).to_le_bytes());
            floors.insert(EntityId::from_bytes(m), 1);
        }
        assert_eq!(
            OrgRevocationBundle::try_issue(&org(), &floors),
            Err(OrgError::TooManyFloors)
        );
    }

    #[test]
    fn bundle_serde_json_round_trip() {
        let bundle = OrgRevocationBundle::try_issue(&org(), &floors_fixture()).expect("issue");
        let json = serde_json::to_string(&bundle).expect("json");
        let back: OrgRevocationBundle = serde_json::from_str(&json).expect("json back");
        assert_eq!(back, bundle);
    }

    // ------------------------------------------------ golden vectors

    // Deterministic bytes pinned end-to-end: fixed seed, fixed
    // fields, deterministic ed25519 ⇒ the exact wire hex below. A
    // failure here means the canonical layout, the domain string,
    // or the signing input changed — all wire-format breaks that
    // require a new `-v2` domain, never a silent edit.
    #[test]
    fn golden_vector_cert() {
        let cert = OrgMembershipCert::issue_at(
            &org(),
            member(),
            5,
            1_700_000_000,
            1_731_536_000,
            0x1122_3344_5566_7788,
        );
        assert_eq!(hex::encode(cert.to_bytes()), GOLDEN_CERT_HEX);
        // And the golden bytes decode + verify.
        let decoded = OrgMembershipCert::from_bytes(
            &hex::decode(GOLDEN_CERT_HEX).expect("golden hex decodes"),
        )
        .expect("golden bytes decode");
        decoded.verify().expect("golden cert verifies");
    }

    #[test]
    fn golden_vector_bundle() {
        let bundle =
            OrgRevocationBundle::issue_at(&org(), &floors_fixture(), 1_700_000_000).expect("issue");
        assert_eq!(hex::encode(bundle.to_bytes()), GOLDEN_BUNDLE_HEX);
        let decoded = OrgRevocationBundle::from_bytes(
            &hex::decode(GOLDEN_BUNDLE_HEX).expect("golden hex decodes"),
        )
        .expect("golden bytes decode");
        decoded.verify().expect("golden bundle verifies");
    }

    const GOLDEN_CERT_HEX: &str = "2152f8d19b791d24453242e15f2eab6cb7cffa7b6a5ed30097960e069881db12242424242424242424242424242424242424242424242424242424242424242400f15365000000008024356700000000050000008877665544332211b94f91f5cac0026eb101b68e5eed16d9e3f8d516d9b81add1de32ccf7508fe40f597be8370ba6ee871154348a5d2ea86335714277a60e8146de3b5576acd300c";
    const GOLDEN_BUNDLE_HEX: &str = "2152f8d19b791d24453242e15f2eab6cb7cffa7b6a5ed30097960e069881db1200f153650000000003000000010101010101010101010101010101010101010101010101010101010101010103000000020202020202020202020202020202020202020202020202020202020202020207000000030303030303030303030303030303030303030303030303030303030303030301000000e9da4985e5d915871c1713e5c6ec815bf97c5c69ac943595f51c9c9faf7d26231220f9aaa5b5f42db19b0cf60a05502549c486a08b7c2495d3f38e7a453b0803";

    // -------------------------------------- seeded round-trip sweep

    /// Dependency-free deterministic PRNG (same discipline as
    /// `behavior/gang/proptest.rs`) — no proptest crate in-tree.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0
        }
        fn bytes32(&mut self) -> [u8; 32] {
            let mut out = [0u8; 32];
            for chunk in out.chunks_mut(8) {
                chunk.copy_from_slice(&self.next().to_le_bytes()[..chunk.len()]);
            }
            out
        }
    }

    #[test]
    fn random_certs_and_bundles_round_trip() {
        let mut rng = Lcg(0xDEAD_BEEF);
        for _ in 0..64 {
            let org = OrgKeypair::from_bytes(rng.bytes32());
            let cert = OrgMembershipCert::issue_at(
                &org,
                EntityId::from_bytes(rng.bytes32()),
                rng.next() as u32,
                rng.next(),
                rng.next(),
                rng.next(),
            );
            let back = OrgMembershipCert::from_bytes(&cert.to_bytes()).expect("cert decode");
            assert_eq!(back, cert);
            // Signature always verifies regardless of window sanity
            // — window checks are separate typed errors.
            let sig = Signature::from_bytes(&cert.signature);
            cert.org_id
                .verify(&cert.signing_input(), &sig)
                .expect("sig verifies");

            let mut floors = BTreeMap::new();
            for _ in 0..(rng.next() % 8) {
                floors.insert(EntityId::from_bytes(rng.bytes32()), rng.next() as u32);
            }
            let bundle = OrgRevocationBundle::issue_at(&org, &floors, rng.next()).expect("issue");
            let back = OrgRevocationBundle::from_bytes(&bundle.to_bytes()).expect("decode");
            assert_eq!(back, bundle);
            back.verify().expect("bundle verifies");
        }
    }
}
