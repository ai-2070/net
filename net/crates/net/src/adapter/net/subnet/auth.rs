//! Subnet transport credentials — S2 of
//! `docs/internal/plans/SUBNET_AUTH_PLAN.md`.
//!
//! Fixed, domain-separated wire artifacts granting the three
//! transport rights (`ATTACH` / `ROUTE` / `EXPORT`) over an
//! authority-qualified subtree of one installation's compact
//! hierarchy:
//!
//! - [`SubnetGrant`] — the terminal leaf credential, subject-bound to
//!   a full 32-byte [`EntityId`] (never the 64-bit derived `NodeId`);
//! - [`SubnetIssuerGrant`] — the single typed provisioning hop: an
//!   authority root names one issuer that may sign leaves within a
//!   scope/rights/window envelope. There is no recursive chain and no
//!   leaf delegation bit;
//! - [`SubnetRevocationFloor`] — a root-signed, subtree-scoped,
//!   monotonic generation floor;
//! - [`SubnetFloorRegistry`] — per-authority floor state plus the
//!   `subnet_auth_epoch` counter compiled contexts compare against;
//! - [`verify_credential_set`] — the fail-closed verifier producing a
//!   [`VerifiedSubnetAuthority`] for session compilation (S3).
//!
//! Everything here is deliberately independent from
//! `identity/token.rs`: `PermissionToken`'s 105-byte positional
//! transcript has no domain prefix and must stay byte-identical, so
//! the subnet family gets its own domain-prefixed envelopes
//! (cross-domain presentation fails signature verification by
//! construction, not by hash-space luck). Low-level discipline —
//! fixed-offset little-endian payloads, strict `from_bytes`,
//! issue-AND-verify TTL enforcement, saturating skew arithmetic —
//! mirrors the org credential family (`behavior/org.rs`).

use blake2::{
    digest::{consts::U32, Mac},
    Blake2sMac,
};
use dashmap::DashMap;
use ed25519_dalek::Signature;

use super::id::TopologySubnetId;
use crate::adapter::net::identity::{EntityId, EntityKeypair};

/// Domain prefix for the leaf grant's ed25519 transcript.
pub const SUBNET_GRANT_SIG_DOMAIN: &[u8] = b"net.subnet.grant.v1";
/// Domain prefix for the issuer grant's ed25519 transcript.
pub const SUBNET_ISSUER_GRANT_SIG_DOMAIN: &[u8] = b"net.subnet.issuer-grant.v1";
/// Domain prefix for the revocation floor's ed25519 transcript.
pub const SUBNET_FLOOR_SIG_DOMAIN: &[u8] = b"net.subnet.floor.v1";
/// Domain label for [`SubnetCredentialSet::credential_set_hash`].
const SUBNET_CREDSET_HASH_DOMAIN: &[u8] = b"net.subnet.credset.v1";

/// Hard ceiling on any subnet credential's validity-window width,
/// enforced at issue AND at verify (org-grant precedent: a receiver
/// must not honor a remote issuer's immortal credential). Authorities
/// tighten further via
/// [`SubnetAuthorityConfig::maximum_grant_lifetime_secs`].
pub const MAX_SUBNET_GRANT_LIFETIME_SECS: u64 = 30 * 24 * 60 * 60;

/// Maximum tolerated clock skew, shared with the token/org families.
pub use crate::adapter::net::identity::MAX_TOKEN_CLOCK_SKEW_SECS;

// ---------------------------------------------------------------------------
// Rights
// ---------------------------------------------------------------------------

/// The three transport rights. Strict decode: any other bit —
/// including bit 3, the previously sketched `DELEGATE` — is
/// [`SubnetAuthError::InvalidRights`], and an empty mask is rejected
/// at issue and decode (a grant of nothing is a mistake, not a
/// credential). Issuance authority is not a rights bit; it exists
/// only as the typed [`SubnetIssuerGrant`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubnetRights(u8);

impl SubnetRights {
    /// Establish subnet-scoped transport sessions in the subtree.
    pub const ATTACH: Self = Self(1 << 0);
    /// Forward traffic with both endpoints inside the subtree.
    pub const ROUTE: Self = Self(1 << 1);
    /// Cross the subtree boundary (either direction).
    pub const EXPORT: Self = Self(1 << 2);
    /// Every defined bit.
    pub const ALL: Self = Self(0b0111);

    const KNOWN_MASK: u8 = 0b0111;

    /// Strict constructor: rejects unknown bits and the empty mask.
    pub fn try_from_bits(bits: u8) -> Result<Self, SubnetAuthError> {
        if bits == 0 || bits & !Self::KNOWN_MASK != 0 {
            return Err(SubnetAuthError::InvalidRights);
        }
        Ok(Self(bits))
    }

    /// Raw bits.
    #[inline]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// `true` iff every bit of `other` is present in `self`.
    #[inline]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Bitwise union (for constructing grants; decode stays strict).
    #[inline]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

// ---------------------------------------------------------------------------
// SubnetRef
// ---------------------------------------------------------------------------

/// The security target: an authority-qualified topology path. Equal
/// path bits under two authorities are unrelated.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SubnetRef {
    /// The installation/product root this hierarchy belongs to.
    pub authority: EntityId,
    /// Compact path inside that authority's hierarchy. Path `0` is
    /// the authority-local root (whole-installation scope), not an
    /// absent or wildcard value.
    pub path: TopologySubnetId,
}

impl SubnetRef {
    /// `true` iff `self` covers `target`: same authority and
    /// `self.path` is `target.path` or an ancestor of it. The path
    /// half is the canonical fixed-width prefix operation
    /// ([`TopologySubnetId::is_ancestor_or_self_of`]) whose truth
    /// table — including the path-`0` rows — is pinned in
    /// `subnet/id.rs`.
    #[inline]
    pub fn contains(&self, target: &SubnetRef) -> bool {
        self.authority == target.authority && self.path.is_ancestor_or_self_of(target.path)
    }
}

// ---------------------------------------------------------------------------
// Errors — stable reason codes (SUBNET_AUTH_PLAN.md D9 subset)
// ---------------------------------------------------------------------------

/// Typed verification failures. `Display` renders the stable
/// snake_case reason code from the plan's D9 list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubnetAuthError {
    /// No [`SubnetAuthorityConfig`] (or an empty root set) for the
    /// credential's authority — fail closed.
    UnknownAuthority,
    /// Leaf subject is not the expected (session-proven) entity.
    WrongSubject,
    /// Credential names a different authority than the verifying
    /// config, or the set's artifacts disagree with each other.
    WrongAuthority,
    /// Credential minted under a different topology epoch.
    WrongTopologyEpoch,
    /// Leaf scope escapes the issuer-grant scope.
    ScopeNotAncestor,
    /// Unknown rights bits or an empty rights mask.
    InvalidRights,
    /// Validity window has passed (with skew tolerance).
    Expired,
    /// Validity window has not begun (with skew tolerance).
    NotYetValid,
    /// Window wider than the wire ceiling or the authority's
    /// configured maximum.
    LifetimeTooWide,
    /// Generation is below the maximum applicable subtree floor.
    Revoked,
    /// Direct leaf not signed by a configured root, or a one-hop
    /// issuer grant not signed by a configured root.
    IssuerNotAuthorized,
    /// Leaf rights exceed the issuer grant's `maximum_rights`, or
    /// the leaf window escapes the issuer window.
    IssuerAttenuationBroadened,
    /// Requested rights exceed the granted rights.
    RightNotGranted,
    /// Presentation bound to a different session incarnation.
    WrongSession,
    /// Presentation bound to a different verifier.
    WrongVerifier,
    /// Presentation carries an unknown, stale, or already-consumed
    /// challenge, or a credential-set hash that does not match the
    /// presented artifacts.
    WrongChallenge,
    /// A different full `EntityId` is already pinned to this routing
    /// `NodeId` — refused, never overwritten.
    IdentityPinConflict,
    /// ed25519 verification failed.
    InvalidSignature,
    /// Structural decode failure: wrong length, unknown version,
    /// trailing bytes, or a malformed field.
    InvalidFormat,
    /// `not_after <= not_before`.
    InvalidValidityWindow,
    /// Caller-supplied skew exceeds [`MAX_TOKEN_CLOCK_SKEW_SECS`].
    ClockSkewTooLarge,
}

impl std::fmt::Display for SubnetAuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let code = match self {
            Self::UnknownAuthority => "unknown_authority",
            Self::WrongSubject => "wrong_subject",
            Self::WrongAuthority => "wrong_authority",
            Self::WrongTopologyEpoch => "wrong_topology_epoch",
            Self::ScopeNotAncestor => "scope_not_ancestor",
            Self::InvalidRights => "invalid_rights",
            Self::Expired => "expired",
            Self::NotYetValid => "not_yet_valid",
            Self::LifetimeTooWide => "lifetime_too_wide",
            Self::Revoked => "revoked",
            Self::IssuerNotAuthorized => "issuer_not_authorized",
            Self::IssuerAttenuationBroadened => "issuer_attenuation_broadened",
            Self::RightNotGranted => "right_not_granted",
            Self::WrongSession => "wrong_session",
            Self::WrongVerifier => "wrong_verifier",
            Self::WrongChallenge => "wrong_challenge",
            Self::IdentityPinConflict => "identity_pin_conflict",
            Self::InvalidSignature => "invalid_signature",
            Self::InvalidFormat => "invalid_format",
            Self::InvalidValidityWindow => "invalid_validity_window",
            Self::ClockSkewTooLarge => "clock_skew_too_large",
        };
        f.write_str(code)
    }
}

impl std::error::Error for SubnetAuthError {}

// ---------------------------------------------------------------------------
// SubnetGrant — the terminal leaf credential
// ---------------------------------------------------------------------------

/// Leaf transport credential. Always terminal: a leaf cannot issue,
/// and bit 3 (`DELEGATE`) is a decode error, so no implementation can
/// mistake an inert bit for usable authority.
///
/// `subject` is the full 32-byte Ed25519 [`EntityId`]. The 64-bit
/// `NodeId` is a truncated derivation and is never a
/// security-strength credential subject; derive
/// `subject.node_id()` for routing/display only after verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubnetGrant {
    /// Wire version; only `1` decodes.
    pub version: u8,
    /// Authority whose hierarchy `scope` addresses.
    pub authority: EntityId,
    /// Granted subtree root within the authority's hierarchy.
    pub scope: TopologySubnetId,
    /// Topology epoch the grant was minted under.
    pub topology_epoch: u32,
    /// Signing identity: a configured authority root (direct) or the
    /// issuer named by a verified [`SubnetIssuerGrant`] (one hop).
    pub issuer: EntityId,
    /// The full entity this grant is bound to.
    pub subject: EntityId,
    /// Granted transport rights.
    pub rights: SubnetRights,
    /// Revocation generation; compared against subtree floors.
    pub generation: u32,
    /// Validity start (unix seconds, inclusive).
    pub not_before: u64,
    /// Validity end (unix seconds, exclusive).
    pub not_after: u64,
    /// Uniqueness nonce — makes re-issues distinct artifacts. Not
    /// packet replay protection (the transport sequence window is).
    pub nonce: u64,
    /// ed25519 over [`SUBNET_GRANT_SIG_DOMAIN`] ‖ payload.
    pub signature: [u8; 64],
}

impl SubnetGrant {
    /// Fixed payload size: version 1 + authority 32 + scope 4 +
    /// epoch 4 + issuer 32 + subject 32 + rights 1 + generation 4 +
    /// not_before 8 + not_after 8 + nonce 8.
    pub const SIGNED_PAYLOAD_SIZE: usize = 134;
    /// Payload + 64-byte signature.
    pub const WIRE_SIZE: usize = Self::SIGNED_PAYLOAD_SIZE + 64;
    const SIGNING_INPUT_SIZE: usize = SUBNET_GRANT_SIG_DOMAIN.len() + Self::SIGNED_PAYLOAD_SIZE;

    /// Issue a leaf grant signed by `issuer_keypair`. Whether that
    /// key actually holds authority (root membership or a valid
    /// issuer grant) is the verifier's decision, not the signer's.
    #[expect(
        clippy::too_many_arguments,
        reason = "explicit wire fields; a params struct would only rename them"
    )]
    pub fn try_issue(
        issuer_keypair: &EntityKeypair,
        authority: EntityId,
        scope: TopologySubnetId,
        topology_epoch: u32,
        subject: EntityId,
        rights: SubnetRights,
        generation: u32,
        not_before: u64,
        duration_secs: u64,
    ) -> Result<Self, SubnetAuthError> {
        if duration_secs == 0 {
            return Err(SubnetAuthError::InvalidValidityWindow);
        }
        if duration_secs > MAX_SUBNET_GRANT_LIFETIME_SECS {
            return Err(SubnetAuthError::LifetimeTooWide);
        }
        // Abort on getrandom failure rather than unwind with a
        // predictable nonce (token-module precedent).
        let mut nonce_bytes = [0u8; 8];
        if let Err(e) = getrandom::fill(&mut nonce_bytes) {
            eprintln!(
                "FATAL: SubnetGrant nonce getrandom failure ({e:?}); aborting to avoid predictable nonce"
            );
            std::process::abort();
        }
        let mut grant = Self {
            version: 1,
            authority,
            scope,
            topology_epoch,
            issuer: issuer_keypair.entity_id().clone(),
            subject,
            rights,
            generation,
            not_before,
            not_after: not_before.saturating_add(duration_secs),
            nonce: u64::from_le_bytes(nonce_bytes),
            signature: [0u8; 64],
        };
        let sig = issuer_keypair
            .try_sign(&grant.signing_input())
            .map_err(|_| SubnetAuthError::InvalidSignature)?;
        grant.signature = sig.to_bytes();
        Ok(grant)
    }

    /// Canonical fixed-offset little-endian payload. `pub(crate)` so
    /// signed bytes can only be minted through the invariant-checking
    /// issue path.
    pub(crate) fn signed_payload(&self) -> [u8; Self::SIGNED_PAYLOAD_SIZE] {
        let mut buf = [0u8; Self::SIGNED_PAYLOAD_SIZE];
        let mut off = 0;
        buf[off] = self.version;
        off += 1;
        buf[off..off + 32].copy_from_slice(self.authority.as_bytes());
        off += 32;
        buf[off..off + 4].copy_from_slice(&self.scope.raw().to_le_bytes());
        off += 4;
        buf[off..off + 4].copy_from_slice(&self.topology_epoch.to_le_bytes());
        off += 4;
        buf[off..off + 32].copy_from_slice(self.issuer.as_bytes());
        off += 32;
        buf[off..off + 32].copy_from_slice(self.subject.as_bytes());
        off += 32;
        buf[off] = self.rights.bits();
        off += 1;
        buf[off..off + 4].copy_from_slice(&self.generation.to_le_bytes());
        off += 4;
        buf[off..off + 8].copy_from_slice(&self.not_before.to_le_bytes());
        off += 8;
        buf[off..off + 8].copy_from_slice(&self.not_after.to_le_bytes());
        off += 8;
        buf[off..off + 8].copy_from_slice(&self.nonce.to_le_bytes());
        buf
    }

    fn signing_input(&self) -> [u8; Self::SIGNING_INPUT_SIZE] {
        let mut buf = [0u8; Self::SIGNING_INPUT_SIZE];
        buf[..SUBNET_GRANT_SIG_DOMAIN.len()].copy_from_slice(SUBNET_GRANT_SIG_DOMAIN);
        buf[SUBNET_GRANT_SIG_DOMAIN.len()..].copy_from_slice(&self.signed_payload());
        buf
    }

    /// Wire form: payload ‖ signature, exactly [`Self::WIRE_SIZE`].
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(Self::WIRE_SIZE);
        out.extend_from_slice(&self.signed_payload());
        out.extend_from_slice(&self.signature);
        out
    }

    /// Strict decode: exact length, version 1, known non-empty
    /// rights, well-formed window. Signature is NOT verified here —
    /// call [`Self::verify`] / [`verify_credential_set`].
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, SubnetAuthError> {
        if bytes.len() != Self::WIRE_SIZE {
            return Err(SubnetAuthError::InvalidFormat);
        }
        let mut off = 0;
        let version = bytes[off];
        off += 1;
        if version != 1 {
            return Err(SubnetAuthError::InvalidFormat);
        }
        let authority = EntityId::from_bytes(read_32(bytes, &mut off));
        let scope = TopologySubnetId::from_raw(read_u32(bytes, &mut off));
        let topology_epoch = read_u32(bytes, &mut off);
        let issuer = EntityId::from_bytes(read_32(bytes, &mut off));
        let subject = EntityId::from_bytes(read_32(bytes, &mut off));
        let rights = SubnetRights::try_from_bits(bytes[off])?;
        off += 1;
        let generation = read_u32(bytes, &mut off);
        let not_before = read_u64(bytes, &mut off);
        let not_after = read_u64(bytes, &mut off);
        let nonce = read_u64(bytes, &mut off);
        let mut signature = [0u8; 64];
        signature.copy_from_slice(&bytes[off..off + 64]);
        let grant = Self {
            version,
            authority,
            scope,
            topology_epoch,
            issuer,
            subject,
            rights,
            generation,
            not_before,
            not_after,
            nonce,
            signature,
        };
        if grant.not_after <= grant.not_before {
            return Err(SubnetAuthError::InvalidValidityWindow);
        }
        Ok(grant)
    }

    /// Structural + signature verification against `self.issuer`.
    /// Window width is re-enforced here (receive side must not honor
    /// a remote issuer's immortal credential).
    pub fn verify(&self) -> Result<(), SubnetAuthError> {
        if self.not_after <= self.not_before {
            return Err(SubnetAuthError::InvalidValidityWindow);
        }
        if self.not_after - self.not_before > MAX_SUBNET_GRANT_LIFETIME_SECS {
            return Err(SubnetAuthError::LifetimeTooWide);
        }
        let sig = Signature::from_bytes(&self.signature);
        self.issuer
            .verify(&self.signing_input(), &sig)
            .map_err(|_| SubnetAuthError::InvalidSignature)
    }

    /// Wall-clock window check at explicit `now` with saturating
    /// skew, token/org boundary convention (`now >= not_after + skew`
    /// ⇒ expired).
    pub fn check_time_bounds_at(&self, now: u64, skew_secs: u64) -> Result<(), SubnetAuthError> {
        if skew_secs > MAX_TOKEN_CLOCK_SKEW_SECS {
            return Err(SubnetAuthError::ClockSkewTooLarge);
        }
        if now < self.not_before.saturating_sub(skew_secs) {
            return Err(SubnetAuthError::NotYetValid);
        }
        if now >= self.not_after.saturating_add(skew_secs) {
            return Err(SubnetAuthError::Expired);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// SubnetIssuerGrant — the single typed provisioning hop
// ---------------------------------------------------------------------------

/// Root-signed authorization for one named `issuer` to sign leaf
/// grants within this scope/rights/window envelope. This artifact IS
/// the issuance authority — there is no rights bit for it, no second
/// hop, and no chain: a `SubnetIssuerGrant` cannot authorize another
/// `SubnetIssuerGrant`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubnetIssuerGrant {
    /// Wire version; only `1` decodes.
    pub version: u8,
    /// Authority whose hierarchy `scope` addresses.
    pub authority: EntityId,
    /// Subtree the issuer may grant within.
    pub scope: TopologySubnetId,
    /// Topology epoch the grant was minted under.
    pub topology_epoch: u32,
    /// The provisioning issuer being empowered (NOT the signer; the
    /// signer is an authority root, checked by the verifier).
    pub issuer: EntityId,
    /// Ceiling on the rights a leaf may carry.
    pub maximum_rights: SubnetRights,
    /// Revocation generation; compared against subtree floors.
    pub generation: u32,
    /// Validity start (unix seconds, inclusive).
    pub not_before: u64,
    /// Validity end (unix seconds, exclusive).
    pub not_after: u64,
    /// ed25519 over [`SUBNET_ISSUER_GRANT_SIG_DOMAIN`] ‖ payload, by
    /// an authority root.
    pub signature: [u8; 64],
}

impl SubnetIssuerGrant {
    /// version 1 + authority 32 + scope 4 + epoch 4 + issuer 32 +
    /// maximum_rights 1 + generation 4 + not_before 8 + not_after 8.
    pub const SIGNED_PAYLOAD_SIZE: usize = 94;
    /// Payload + 64-byte signature.
    pub const WIRE_SIZE: usize = Self::SIGNED_PAYLOAD_SIZE + 64;
    const SIGNING_INPUT_SIZE: usize =
        SUBNET_ISSUER_GRANT_SIG_DOMAIN.len() + Self::SIGNED_PAYLOAD_SIZE;

    /// Issue signed by `root_keypair` (whether that key is a
    /// configured root for `authority` is the verifier's decision).
    #[expect(
        clippy::too_many_arguments,
        reason = "explicit wire fields; a params struct would only rename them"
    )]
    pub fn try_issue(
        root_keypair: &EntityKeypair,
        authority: EntityId,
        scope: TopologySubnetId,
        topology_epoch: u32,
        issuer: EntityId,
        maximum_rights: SubnetRights,
        generation: u32,
        not_before: u64,
        duration_secs: u64,
    ) -> Result<Self, SubnetAuthError> {
        if duration_secs == 0 {
            return Err(SubnetAuthError::InvalidValidityWindow);
        }
        if duration_secs > MAX_SUBNET_GRANT_LIFETIME_SECS {
            return Err(SubnetAuthError::LifetimeTooWide);
        }
        let mut grant = Self {
            version: 1,
            authority,
            scope,
            topology_epoch,
            issuer,
            maximum_rights,
            generation,
            not_before,
            not_after: not_before.saturating_add(duration_secs),
            signature: [0u8; 64],
        };
        let sig = root_keypair
            .try_sign(&grant.signing_input())
            .map_err(|_| SubnetAuthError::InvalidSignature)?;
        grant.signature = sig.to_bytes();
        Ok(grant)
    }

    pub(crate) fn signed_payload(&self) -> [u8; Self::SIGNED_PAYLOAD_SIZE] {
        let mut buf = [0u8; Self::SIGNED_PAYLOAD_SIZE];
        let mut off = 0;
        buf[off] = self.version;
        off += 1;
        buf[off..off + 32].copy_from_slice(self.authority.as_bytes());
        off += 32;
        buf[off..off + 4].copy_from_slice(&self.scope.raw().to_le_bytes());
        off += 4;
        buf[off..off + 4].copy_from_slice(&self.topology_epoch.to_le_bytes());
        off += 4;
        buf[off..off + 32].copy_from_slice(self.issuer.as_bytes());
        off += 32;
        buf[off] = self.maximum_rights.bits();
        off += 1;
        buf[off..off + 4].copy_from_slice(&self.generation.to_le_bytes());
        off += 4;
        buf[off..off + 8].copy_from_slice(&self.not_before.to_le_bytes());
        off += 8;
        buf[off..off + 8].copy_from_slice(&self.not_after.to_le_bytes());
        buf
    }

    fn signing_input(&self) -> [u8; Self::SIGNING_INPUT_SIZE] {
        let mut buf = [0u8; Self::SIGNING_INPUT_SIZE];
        buf[..SUBNET_ISSUER_GRANT_SIG_DOMAIN.len()].copy_from_slice(SUBNET_ISSUER_GRANT_SIG_DOMAIN);
        buf[SUBNET_ISSUER_GRANT_SIG_DOMAIN.len()..].copy_from_slice(&self.signed_payload());
        buf
    }

    /// Wire form: payload ‖ signature.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(Self::WIRE_SIZE);
        out.extend_from_slice(&self.signed_payload());
        out.extend_from_slice(&self.signature);
        out
    }

    /// Strict decode; signature NOT verified here.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, SubnetAuthError> {
        if bytes.len() != Self::WIRE_SIZE {
            return Err(SubnetAuthError::InvalidFormat);
        }
        let mut off = 0;
        let version = bytes[off];
        off += 1;
        if version != 1 {
            return Err(SubnetAuthError::InvalidFormat);
        }
        let authority = EntityId::from_bytes(read_32(bytes, &mut off));
        let scope = TopologySubnetId::from_raw(read_u32(bytes, &mut off));
        let topology_epoch = read_u32(bytes, &mut off);
        let issuer = EntityId::from_bytes(read_32(bytes, &mut off));
        let maximum_rights = SubnetRights::try_from_bits(bytes[off])?;
        off += 1;
        let generation = read_u32(bytes, &mut off);
        let not_before = read_u64(bytes, &mut off);
        let not_after = read_u64(bytes, &mut off);
        let mut signature = [0u8; 64];
        signature.copy_from_slice(&bytes[off..off + 64]);
        let grant = Self {
            version,
            authority,
            scope,
            topology_epoch,
            issuer,
            maximum_rights,
            generation,
            not_before,
            not_after,
            signature,
        };
        if grant.not_after <= grant.not_before {
            return Err(SubnetAuthError::InvalidValidityWindow);
        }
        Ok(grant)
    }

    /// Structural check + signature verification against `root`.
    pub fn verify_signed_by(&self, root: &EntityId) -> Result<(), SubnetAuthError> {
        if self.not_after <= self.not_before {
            return Err(SubnetAuthError::InvalidValidityWindow);
        }
        if self.not_after - self.not_before > MAX_SUBNET_GRANT_LIFETIME_SECS {
            return Err(SubnetAuthError::LifetimeTooWide);
        }
        let sig = Signature::from_bytes(&self.signature);
        root.verify(&self.signing_input(), &sig)
            .map_err(|_| SubnetAuthError::InvalidSignature)
    }

    /// Wall-clock window check (same convention as the leaf).
    pub fn check_time_bounds_at(&self, now: u64, skew_secs: u64) -> Result<(), SubnetAuthError> {
        if skew_secs > MAX_TOKEN_CLOCK_SKEW_SECS {
            return Err(SubnetAuthError::ClockSkewTooLarge);
        }
        if now < self.not_before.saturating_sub(skew_secs) {
            return Err(SubnetAuthError::NotYetValid);
        }
        if now >= self.not_after.saturating_add(skew_secs) {
            return Err(SubnetAuthError::Expired);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Revocation floors
// ---------------------------------------------------------------------------

/// Root-signed, subtree-scoped, monotonic revocation floor: every
/// credential whose scope lies under `scope` and whose `generation`
/// is below `minimum_generation` is dead. A child floor does not
/// revoke a structurally dominant parent-scoped grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubnetRevocationFloor {
    /// Wire version; only `1` decodes.
    pub version: u8,
    /// Authority-qualified subtree the floor applies to.
    pub scope: SubnetRef,
    /// Topology epoch the floor belongs to; floors are monotonic per
    /// `(scope, topology_epoch)`.
    pub topology_epoch: u32,
    /// Signing root.
    pub issuer: EntityId,
    /// Grants below this generation (within scope) are revoked.
    pub minimum_generation: u32,
    /// Per-`(scope, epoch)` ordering revision; replay/reorder safe.
    pub revision: u64,
    /// Advisory issue timestamp (unix seconds).
    pub issued_at: u64,
    /// ed25519 over [`SUBNET_FLOOR_SIG_DOMAIN`] ‖ payload.
    pub signature: [u8; 64],
}

impl SubnetRevocationFloor {
    /// version 1 + authority 32 + path 4 + epoch 4 + issuer 32 +
    /// minimum_generation 4 + revision 8 + issued_at 8.
    pub const SIGNED_PAYLOAD_SIZE: usize = 93;
    /// Payload + 64-byte signature.
    pub const WIRE_SIZE: usize = Self::SIGNED_PAYLOAD_SIZE + 64;
    const SIGNING_INPUT_SIZE: usize = SUBNET_FLOOR_SIG_DOMAIN.len() + Self::SIGNED_PAYLOAD_SIZE;

    /// Issue signed by `root_keypair` (`issuer` is set from it).
    pub fn try_issue(
        root_keypair: &EntityKeypair,
        scope: SubnetRef,
        topology_epoch: u32,
        minimum_generation: u32,
        revision: u64,
        issued_at: u64,
    ) -> Result<Self, SubnetAuthError> {
        let mut floor = Self {
            version: 1,
            scope,
            topology_epoch,
            issuer: root_keypair.entity_id().clone(),
            minimum_generation,
            revision,
            issued_at,
            signature: [0u8; 64],
        };
        let sig = root_keypair
            .try_sign(&floor.signing_input())
            .map_err(|_| SubnetAuthError::InvalidSignature)?;
        floor.signature = sig.to_bytes();
        Ok(floor)
    }

    pub(crate) fn signed_payload(&self) -> [u8; Self::SIGNED_PAYLOAD_SIZE] {
        let mut buf = [0u8; Self::SIGNED_PAYLOAD_SIZE];
        let mut off = 0;
        buf[off] = self.version;
        off += 1;
        buf[off..off + 32].copy_from_slice(self.scope.authority.as_bytes());
        off += 32;
        buf[off..off + 4].copy_from_slice(&self.scope.path.raw().to_le_bytes());
        off += 4;
        buf[off..off + 4].copy_from_slice(&self.topology_epoch.to_le_bytes());
        off += 4;
        buf[off..off + 32].copy_from_slice(self.issuer.as_bytes());
        off += 32;
        buf[off..off + 4].copy_from_slice(&self.minimum_generation.to_le_bytes());
        off += 4;
        buf[off..off + 8].copy_from_slice(&self.revision.to_le_bytes());
        off += 8;
        buf[off..off + 8].copy_from_slice(&self.issued_at.to_le_bytes());
        buf
    }

    fn signing_input(&self) -> [u8; Self::SIGNING_INPUT_SIZE] {
        let mut buf = [0u8; Self::SIGNING_INPUT_SIZE];
        buf[..SUBNET_FLOOR_SIG_DOMAIN.len()].copy_from_slice(SUBNET_FLOOR_SIG_DOMAIN);
        buf[SUBNET_FLOOR_SIG_DOMAIN.len()..].copy_from_slice(&self.signed_payload());
        buf
    }

    /// Wire form: payload ‖ signature.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(Self::WIRE_SIZE);
        out.extend_from_slice(&self.signed_payload());
        out.extend_from_slice(&self.signature);
        out
    }

    /// Strict decode; signature NOT verified here.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, SubnetAuthError> {
        if bytes.len() != Self::WIRE_SIZE {
            return Err(SubnetAuthError::InvalidFormat);
        }
        let mut off = 0;
        let version = bytes[off];
        off += 1;
        if version != 1 {
            return Err(SubnetAuthError::InvalidFormat);
        }
        let authority = EntityId::from_bytes(read_32(bytes, &mut off));
        let path = TopologySubnetId::from_raw(read_u32(bytes, &mut off));
        let topology_epoch = read_u32(bytes, &mut off);
        let issuer = EntityId::from_bytes(read_32(bytes, &mut off));
        let minimum_generation = read_u32(bytes, &mut off);
        let revision = read_u64(bytes, &mut off);
        let issued_at = read_u64(bytes, &mut off);
        let mut signature = [0u8; 64];
        signature.copy_from_slice(&bytes[off..off + 64]);
        Ok(Self {
            version,
            scope: SubnetRef { authority, path },
            topology_epoch,
            issuer,
            minimum_generation,
            revision,
            issued_at,
            signature,
        })
    }

    /// Signature verification against `self.issuer` (whether that
    /// issuer is a configured root is the registry's decision).
    pub fn verify(&self) -> Result<(), SubnetAuthError> {
        let sig = Signature::from_bytes(&self.signature);
        self.issuer
            .verify(&self.signing_input(), &sig)
            .map_err(|_| SubnetAuthError::InvalidSignature)
    }
}

/// Per-authority floor state + the `subnet_auth_epoch` counter.
///
/// Floors are keyed `(authority, topology_epoch, scope path)` and
/// applied monotonically by `revision` (a replayed or reordered floor
/// can never roll state backward). Accepting a floor that CHANGES
/// state increments the authority's auth epoch; compiled session
/// contexts pin the epoch they verified against and fail one integer
/// comparison when it moves (S3), keeping revocation out of
/// per-packet maps.
#[derive(Debug, Default)]
pub struct SubnetFloorRegistry {
    floors: DashMap<([u8; 32], u32, u32), FloorEntry>,
    auth_epochs: DashMap<[u8; 32], u64>,
}

#[derive(Debug, Clone, Copy)]
struct FloorEntry {
    minimum_generation: u32,
    revision: u64,
}

impl SubnetFloorRegistry {
    /// Empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Verify and apply a floor under `config`'s trust: the floor's
    /// authority must equal the config's, and its issuer must be a
    /// configured root. Returns `Ok(true)` iff registry state
    /// changed (and the auth epoch advanced). Stale revisions and
    /// non-raising floors are `Ok(false)` no-ops — replay/reorder
    /// can never roll backward.
    pub fn apply(
        &self,
        floor: &SubnetRevocationFloor,
        config: &SubnetAuthorityConfig,
    ) -> Result<bool, SubnetAuthError> {
        if floor.scope.authority != config.authority {
            return Err(SubnetAuthError::WrongAuthority);
        }
        if config.roots.is_empty() {
            return Err(SubnetAuthError::UnknownAuthority);
        }
        if !config.roots.contains(&floor.issuer) {
            return Err(SubnetAuthError::IssuerNotAuthorized);
        }
        floor.verify()?;

        let key = (
            *floor.scope.authority.as_bytes(),
            floor.topology_epoch,
            floor.scope.path.raw(),
        );
        let mut changed = false;
        self.floors
            .entry(key)
            .and_modify(|e| {
                if floor.revision > e.revision && floor.minimum_generation > e.minimum_generation {
                    e.revision = floor.revision;
                    e.minimum_generation = floor.minimum_generation;
                    changed = true;
                }
            })
            .or_insert_with(|| {
                changed = true;
                FloorEntry {
                    minimum_generation: floor.minimum_generation,
                    revision: floor.revision,
                }
            });
        if changed {
            self.auth_epochs
                .entry(*floor.scope.authority.as_bytes())
                .and_modify(|e| *e += 1)
                .or_insert(1);
        }
        Ok(changed)
    }

    /// Maximum applicable floor for a credential scoped at
    /// `scope_path`: the max `minimum_generation` over the scope's
    /// fixed-depth ancestor chain (itself, each parent, and the
    /// authority root `0`). ≤ 5 lookups, no allocation.
    pub fn max_floor(
        &self,
        authority: &EntityId,
        topology_epoch: u32,
        scope_path: TopologySubnetId,
    ) -> u32 {
        let auth = *authority.as_bytes();
        let mut max = 0u32;
        let mut cursor = scope_path;
        loop {
            if let Some(e) = self.floors.get(&(auth, topology_epoch, cursor.raw())) {
                max = max.max(e.minimum_generation);
            }
            if cursor.is_global() {
                break;
            }
            cursor = cursor.parent();
        }
        max
    }

    /// Current auth epoch for `authority` (0 until a floor applies).
    pub fn auth_epoch(&self, authority: &EntityId) -> u64 {
        self.auth_epochs
            .get(authority.as_bytes())
            .map(|e| *e)
            .unwrap_or(0)
    }
}

// ---------------------------------------------------------------------------
// Trust anchoring
// ---------------------------------------------------------------------------

/// Per-authority trust configuration. Empty `roots` fail closed for
/// every protected subnet assertion under that authority.
#[derive(Debug, Clone)]
pub struct SubnetAuthorityConfig {
    /// The authority this config anchors. A root configured here can
    /// verify only grants whose authority equals this — it cannot
    /// mint a different authority.
    pub authority: EntityId,
    /// Root entities whose signatures anchor grants, issuer grants,
    /// and floors for this authority.
    pub roots: Vec<EntityId>,
    /// Authority-local ceiling on credential window width, enforced
    /// at verification in addition to the wire-level
    /// [`MAX_SUBNET_GRANT_LIFETIME_SECS`].
    pub maximum_grant_lifetime_secs: u64,
}

// ---------------------------------------------------------------------------
// Credential set + verifier
// ---------------------------------------------------------------------------

/// What a subject presents: a root-signed leaf, or one typed
/// provisioning hop. There is no deeper shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubnetCredentialSet {
    /// `authority root → subject`.
    Direct(SubnetGrant),
    /// `authority root → delegated issuer → subject`.
    OneHop {
        /// The root-signed provisioning credential.
        issuer_grant: SubnetIssuerGrant,
        /// The leaf it authorizes.
        leaf: SubnetGrant,
    },
}

impl SubnetCredentialSet {
    const TAG_DIRECT: u8 = 1;
    const TAG_ONE_HOP: u8 = 2;

    /// The leaf grant of either shape.
    pub fn leaf(&self) -> &SubnetGrant {
        match self {
            Self::Direct(leaf) => leaf,
            Self::OneHop { leaf, .. } => leaf,
        }
    }

    /// Framed wire form: 1-byte shape tag ‖ artifacts.
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            Self::Direct(leaf) => {
                let mut out = Vec::with_capacity(1 + SubnetGrant::WIRE_SIZE);
                out.push(Self::TAG_DIRECT);
                out.extend_from_slice(&leaf.to_bytes());
                out
            }
            Self::OneHop { issuer_grant, leaf } => {
                let mut out =
                    Vec::with_capacity(1 + SubnetIssuerGrant::WIRE_SIZE + SubnetGrant::WIRE_SIZE);
                out.push(Self::TAG_ONE_HOP);
                out.extend_from_slice(&issuer_grant.to_bytes());
                out.extend_from_slice(&leaf.to_bytes());
                out
            }
        }
    }

    /// Strict decode of the framed form (exact lengths, no trailing
    /// bytes).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, SubnetAuthError> {
        let (&tag, rest) = bytes.split_first().ok_or(SubnetAuthError::InvalidFormat)?;
        match tag {
            Self::TAG_DIRECT => Ok(Self::Direct(SubnetGrant::from_bytes(rest)?)),
            Self::TAG_ONE_HOP => {
                if rest.len() != SubnetIssuerGrant::WIRE_SIZE + SubnetGrant::WIRE_SIZE {
                    return Err(SubnetAuthError::InvalidFormat);
                }
                let (ig, leaf) = rest.split_at(SubnetIssuerGrant::WIRE_SIZE);
                Ok(Self::OneHop {
                    issuer_grant: SubnetIssuerGrant::from_bytes(ig)?,
                    leaf: SubnetGrant::from_bytes(leaf)?,
                })
            }
            _ => Err(SubnetAuthError::InvalidFormat),
        }
    }

    /// Domain-separated BLAKE2s over the framed wire form — the
    /// `credential_set_hash` bound into the S3 session presentation.
    #[expect(
        clippy::expect_used,
        reason = "Blake2sMac::new_from_slice rejects only keys longer than 32 bytes; the domain label is a short compile-time constant"
    )]
    pub fn credential_set_hash(&self) -> [u8; 32] {
        let mut mac = <Blake2sMac<U32> as Mac>::new_from_slice(SUBNET_CREDSET_HASH_DOMAIN)
            .expect("BLAKE2s accepts variable-length keys");
        Mac::update(&mut mac, &self.to_bytes());
        let mut out = [0u8; 32];
        out.copy_from_slice(&mac.finalize().into_bytes());
        out
    }
}

/// Successful verification summary — the input S3 compiles into an
/// immutable per-session `VerifiedSubnetContext`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedSubnetAuthority {
    /// Authority the credentials verified under.
    pub authority: EntityId,
    /// Granted subtree root.
    pub scope: TopologySubnetId,
    /// Topology epoch verified against.
    pub topology_epoch: u32,
    /// The full subject entity the leaf is bound to.
    pub subject: EntityId,
    /// Granted transport rights.
    pub rights: SubnetRights,
    /// Leaf generation (for later floor comparisons).
    pub generation: u32,
    /// Leaf expiry (unix seconds, exclusive).
    pub expires_at: u64,
    /// The authority's auth epoch at verification time.
    pub subnet_auth_epoch: u64,
    /// Hash binding the exact presented artifacts.
    pub credential_set_hash: [u8; 32],
}

/// Fail-closed verification of a presented credential set
/// (SUBNET_AUTH_PLAN.md D3/D4; the session-proof steps around it are
/// S3). Checks, in order: authority match against `config`;
/// non-empty roots; root anchoring (direct leaf issuer ∈ roots, or
/// issuer grant signed by a root and naming the leaf's issuer);
/// subject binding to `expected_subject` (full `EntityId` equality);
/// topology epoch; artifact signatures and structural bounds;
/// wall-clock windows at `now_secs ± skew`; authority lifetime
/// ceiling; one-hop scope containment, rights attenuation, and
/// window nesting; revocation floors for every artifact at its own
/// scope.
pub fn verify_credential_set(
    set: &SubnetCredentialSet,
    expected_subject: &EntityId,
    config: &SubnetAuthorityConfig,
    current_topology_epoch: u32,
    floors: &SubnetFloorRegistry,
    now_secs: u64,
    skew_secs: u64,
) -> Result<VerifiedSubnetAuthority, SubnetAuthError> {
    let leaf = set.leaf();
    if leaf.authority != config.authority {
        return Err(SubnetAuthError::WrongAuthority);
    }
    if config.roots.is_empty() {
        return Err(SubnetAuthError::UnknownAuthority);
    }
    if &leaf.subject != expected_subject {
        return Err(SubnetAuthError::WrongSubject);
    }
    if leaf.topology_epoch != current_topology_epoch {
        return Err(SubnetAuthError::WrongTopologyEpoch);
    }

    match set {
        SubnetCredentialSet::Direct(leaf) => {
            if !config.roots.contains(&leaf.issuer) {
                return Err(SubnetAuthError::IssuerNotAuthorized);
            }
            leaf.verify()?;
        }
        SubnetCredentialSet::OneHop { issuer_grant, leaf } => {
            if issuer_grant.authority != config.authority {
                return Err(SubnetAuthError::WrongAuthority);
            }
            if issuer_grant.topology_epoch != current_topology_epoch {
                return Err(SubnetAuthError::WrongTopologyEpoch);
            }
            // The issuer grant must be signed by SOME configured
            // root; try each. (Root sets are operator-small.)
            if !config
                .roots
                .iter()
                .any(|root| issuer_grant.verify_signed_by(root).is_ok())
            {
                return Err(SubnetAuthError::IssuerNotAuthorized);
            }
            // A leaf may not itself act as an issuer, and the leaf's
            // signer must be exactly the empowered issuer.
            if leaf.issuer != issuer_grant.issuer {
                return Err(SubnetAuthError::IssuerNotAuthorized);
            }
            leaf.verify()?;
            // Envelope: scope containment, rights ceiling, window
            // nesting.
            if !issuer_grant.scope.is_ancestor_or_self_of(leaf.scope) {
                return Err(SubnetAuthError::ScopeNotAncestor);
            }
            if !issuer_grant.maximum_rights.contains(leaf.rights) {
                return Err(SubnetAuthError::IssuerAttenuationBroadened);
            }
            if leaf.not_before < issuer_grant.not_before || leaf.not_after > issuer_grant.not_after
            {
                return Err(SubnetAuthError::IssuerAttenuationBroadened);
            }
            issuer_grant.check_time_bounds_at(now_secs, skew_secs)?;
            // Issuer grant revocation at ITS scope.
            let issuer_floor = floors.max_floor(
                &config.authority,
                current_topology_epoch,
                issuer_grant.scope,
            );
            if issuer_grant.generation < issuer_floor {
                return Err(SubnetAuthError::Revoked);
            }
        }
    }

    leaf.check_time_bounds_at(now_secs, skew_secs)?;
    if leaf.not_after - leaf.not_before > config.maximum_grant_lifetime_secs {
        return Err(SubnetAuthError::LifetimeTooWide);
    }
    let floor = floors.max_floor(&config.authority, current_topology_epoch, leaf.scope);
    if leaf.generation < floor {
        return Err(SubnetAuthError::Revoked);
    }

    Ok(VerifiedSubnetAuthority {
        authority: leaf.authority.clone(),
        scope: leaf.scope,
        topology_epoch: leaf.topology_epoch,
        subject: leaf.subject.clone(),
        rights: leaf.rights,
        generation: leaf.generation,
        expires_at: leaf.not_after,
        subnet_auth_epoch: floors.auth_epoch(&config.authority),
        credential_set_hash: set.credential_set_hash(),
    })
}

// ---------------------------------------------------------------------------
// Session proof (S3)
// ---------------------------------------------------------------------------

/// Domain prefix for the session-proof transcript.
pub const SUBNET_PRESENTATION_SIG_DOMAIN: &[u8] = b"net.subnet.presentation.v1";

/// Proof of possession of the leaf grant's subject key, bound to one
/// admission attempt (SUBNET_AUTH_PLAN.md D5).
///
/// The AEAD session alone does not prove the leaf `EntityId`: Noise
/// `NKpsk0` authenticates the responder's X25519 static while the
/// initiator is anonymous, and the `peer_entity_ids` TOFU pin can
/// corroborate identity but cannot substitute for proof-of-possession
/// on a protected credential. The subject therefore signs a fresh
/// verifier challenge with its Ed25519 key.
///
/// Binding the credential-set hash, session id, verifier identity,
/// nonce, target, and requested rights makes the signature
/// non-transferable to another credential set, session, verifier,
/// scope, or operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubnetAuthPresentation {
    /// Wire version; only `1` decodes.
    pub version: u8,
    /// The signing entity — must equal the leaf grant's subject.
    pub subject: EntityId,
    /// Hash of the exact credential set being presented.
    pub credential_set_hash: [u8; 32],
    /// Session incarnation this proof is valid on.
    pub session_id: u64,
    /// The verifier that issued the challenge.
    pub verifier: EntityId,
    /// One-use 32-byte challenge from that verifier.
    pub verifier_nonce: [u8; 32],
    /// Subnet the subject is asking to operate in.
    pub target: SubnetRef,
    /// Rights being requested for this session.
    pub requested_rights: SubnetRights,
    /// ed25519 over [`SUBNET_PRESENTATION_SIG_DOMAIN`] ‖ payload.
    pub signature: [u8; 64],
}

impl SubnetAuthPresentation {
    /// Field widths in order: version 1, subject 32,
    /// credential_set_hash 32, session_id 8, verifier 32,
    /// verifier_nonce 32, target authority 32, target path 4,
    /// requested_rights 1.
    pub const SIGNED_PAYLOAD_SIZE: usize = 174;
    /// Payload + 64-byte signature.
    pub const WIRE_SIZE: usize = Self::SIGNED_PAYLOAD_SIZE + 64;
    const SIGNING_INPUT_SIZE: usize =
        SUBNET_PRESENTATION_SIG_DOMAIN.len() + Self::SIGNED_PAYLOAD_SIZE;

    /// Sign a presentation with the subject's keypair.
    pub fn try_issue(
        subject_keypair: &EntityKeypair,
        credential_set_hash: [u8; 32],
        session_id: u64,
        verifier: EntityId,
        verifier_nonce: [u8; 32],
        target: SubnetRef,
        requested_rights: SubnetRights,
    ) -> Result<Self, SubnetAuthError> {
        let mut p = Self {
            version: 1,
            subject: subject_keypair.entity_id().clone(),
            credential_set_hash,
            session_id,
            verifier,
            verifier_nonce,
            target,
            requested_rights,
            signature: [0u8; 64],
        };
        let sig = subject_keypair
            .try_sign(&p.signing_input())
            .map_err(|_| SubnetAuthError::InvalidSignature)?;
        p.signature = sig.to_bytes();
        Ok(p)
    }

    pub(crate) fn signed_payload(&self) -> [u8; Self::SIGNED_PAYLOAD_SIZE] {
        let mut buf = [0u8; Self::SIGNED_PAYLOAD_SIZE];
        let mut off = 0;
        buf[off] = self.version;
        off += 1;
        buf[off..off + 32].copy_from_slice(self.subject.as_bytes());
        off += 32;
        buf[off..off + 32].copy_from_slice(&self.credential_set_hash);
        off += 32;
        buf[off..off + 8].copy_from_slice(&self.session_id.to_le_bytes());
        off += 8;
        buf[off..off + 32].copy_from_slice(self.verifier.as_bytes());
        off += 32;
        buf[off..off + 32].copy_from_slice(&self.verifier_nonce);
        off += 32;
        buf[off..off + 32].copy_from_slice(self.target.authority.as_bytes());
        off += 32;
        buf[off..off + 4].copy_from_slice(&self.target.path.raw().to_le_bytes());
        off += 4;
        buf[off] = self.requested_rights.bits();
        buf
    }

    fn signing_input(&self) -> [u8; Self::SIGNING_INPUT_SIZE] {
        let mut buf = [0u8; Self::SIGNING_INPUT_SIZE];
        buf[..SUBNET_PRESENTATION_SIG_DOMAIN.len()].copy_from_slice(SUBNET_PRESENTATION_SIG_DOMAIN);
        buf[SUBNET_PRESENTATION_SIG_DOMAIN.len()..].copy_from_slice(&self.signed_payload());
        buf
    }

    /// Wire form: payload ‖ signature.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(Self::WIRE_SIZE);
        out.extend_from_slice(&self.signed_payload());
        out.extend_from_slice(&self.signature);
        out
    }

    /// Strict decode; signature NOT verified here.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, SubnetAuthError> {
        if bytes.len() != Self::WIRE_SIZE {
            return Err(SubnetAuthError::InvalidFormat);
        }
        let mut off = 0;
        let version = bytes[off];
        off += 1;
        if version != 1 {
            return Err(SubnetAuthError::InvalidFormat);
        }
        let subject = EntityId::from_bytes(read_32(bytes, &mut off));
        let credential_set_hash = read_32(bytes, &mut off);
        let session_id = read_u64(bytes, &mut off);
        let verifier = EntityId::from_bytes(read_32(bytes, &mut off));
        let verifier_nonce = read_32(bytes, &mut off);
        let target_authority = EntityId::from_bytes(read_32(bytes, &mut off));
        let target_path = TopologySubnetId::from_raw(read_u32(bytes, &mut off));
        let requested_rights = SubnetRights::try_from_bits(bytes[off])?;
        off += 1;
        let mut signature = [0u8; 64];
        signature.copy_from_slice(&bytes[off..off + 64]);
        Ok(Self {
            version,
            subject,
            credential_set_hash,
            session_id,
            verifier,
            verifier_nonce,
            target: SubnetRef {
                authority: target_authority,
                path: target_path,
            },
            requested_rights,
            signature,
        })
    }

    /// Verify the signature against `self.subject`.
    pub fn verify(&self) -> Result<(), SubnetAuthError> {
        let sig = Signature::from_bytes(&self.signature);
        self.subject
            .verify(&self.signing_input(), &sig)
            .map_err(|_| SubnetAuthError::InvalidSignature)
    }
}

/// What the verifier expects a presentation to be bound to. Every
/// field is verifier-owned state, never taken from the wire.
#[derive(Debug, Clone)]
pub struct ExpectedBinding {
    /// Session incarnation the challenge was issued on.
    pub session_id: u64,
    /// This node's identity.
    pub verifier: EntityId,
    /// The one-use challenge this node minted and has not yet
    /// consumed.
    pub verifier_nonce: [u8; 32],
}

/// Immutable per-session authority compiled once at admission and
/// read by the packet path (SUBNET_AUTH_PLAN.md D5/D6).
///
/// Forwarding consumes only this: authority equality, one fixed-width
/// prefix comparison, two epoch comparisons, an expiry check, and a
/// rights-bit test. No signature verification, chain walk, string
/// parse, allocation, or online lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedSubnetContext {
    /// Authority the credentials verified under.
    pub authority: EntityId,
    /// Granted subtree root.
    pub scope: TopologySubnetId,
    /// Topology epoch verified against.
    pub topology_epoch: u32,
    /// Full subject entity, proven by signature this session.
    pub subject: EntityId,
    /// Routing derivative of `subject`, derived once after
    /// verification (never used to establish identity).
    pub subject_node: u64,
    /// Session incarnation this context belongs to.
    pub session_id: u64,
    /// Rights granted for this session.
    pub rights: SubnetRights,
    /// Leaf generation at verification time.
    pub generation: u32,
    /// Authority auth epoch at verification time.
    pub subnet_auth_epoch: u64,
    /// Leaf expiry (unix seconds, exclusive).
    pub expires_at: u64,
    /// Hash binding the exact credentials that produced this context.
    pub credential_set_hash: [u8; 32],
}

impl VerifiedSubnetContext {
    /// The D6 hot-path check: is `right` authorized for `target`
    /// right now? Pure integer work over immutable state.
    #[inline]
    pub fn allows(
        &self,
        current_topology_epoch: u32,
        current_subnet_auth_epoch: u64,
        now_secs: u64,
        target: TopologySubnetId,
        right: SubnetRights,
    ) -> bool {
        self.topology_epoch == current_topology_epoch
            && self.subnet_auth_epoch == current_subnet_auth_epoch
            && now_secs < self.expires_at
            && self.scope.is_ancestor_or_self_of(target)
            && self.rights.contains(right)
    }

    /// Forwarding boundary rule (D6): traffic with both endpoints
    /// inside the scope needs `ROUTE`; traffic crossing the boundary
    /// needs `EXPORT`; traffic wholly outside is not this context's
    /// business. Returns the required right, or `None` when the
    /// context is irrelevant to the transition.
    #[inline]
    pub fn required_forwarding_right(
        &self,
        source: TopologySubnetId,
        target: TopologySubnetId,
    ) -> Option<SubnetRights> {
        let inside_source = self.scope.is_ancestor_or_self_of(source);
        let inside_target = self.scope.is_ancestor_or_self_of(target);
        match (inside_source, inside_target) {
            (true, true) => Some(SubnetRights::ROUTE),
            (true, false) | (false, true) => Some(SubnetRights::EXPORT),
            (false, false) => None,
        }
    }
}

/// Verify a presentation + credential set and compile the session
/// context (SUBNET_AUTH_PLAN.md D5 steps 2–7, 9, 10).
///
/// The caller owns the surrounding steps that need node state: minting
/// and consuming the one-use challenge (step 1/4), and the atomic
/// `NodeId → EntityId` pin compare/install (step 8), which must happen
/// only after every check here succeeds.
///
/// Order matters: the presentation is checked against verifier-owned
/// expectations BEFORE any credential work, so a replayed or
/// misdirected proof cannot spend credential verification effort.
#[expect(
    clippy::too_many_arguments,
    reason = "every parameter is a distinct verifier-owned input; bundling them into a struct \
              would hide which ones the caller must supply from trusted state"
)]
pub fn verify_admission(
    presentation: &SubnetAuthPresentation,
    set: &SubnetCredentialSet,
    expected: &ExpectedBinding,
    config: &SubnetAuthorityConfig,
    current_topology_epoch: u32,
    floors: &SubnetFloorRegistry,
    now_secs: u64,
    skew_secs: u64,
) -> Result<VerifiedSubnetContext, SubnetAuthError> {
    // Verifier-owned bindings first — cheap, and they reject a proof
    // aimed at another session, node, or challenge before any
    // signature work on the credential chain.
    if presentation.session_id != expected.session_id {
        return Err(SubnetAuthError::WrongSession);
    }
    if presentation.verifier != expected.verifier {
        return Err(SubnetAuthError::WrongVerifier);
    }
    // Constant-time nonce comparison: the challenge is a one-use
    // secret until consumed.
    if !bool::from(subtle::ConstantTimeEq::ct_eq(
        &presentation.verifier_nonce[..],
        &expected.verifier_nonce[..],
    )) {
        return Err(SubnetAuthError::WrongChallenge);
    }
    if presentation.credential_set_hash != set.credential_set_hash() {
        return Err(SubnetAuthError::WrongChallenge);
    }
    if presentation.target.authority != config.authority {
        return Err(SubnetAuthError::WrongAuthority);
    }

    // The leaf subject is the identity the proof must come from.
    let leaf_subject = set.leaf().subject.clone();
    if presentation.subject != leaf_subject {
        return Err(SubnetAuthError::WrongSubject);
    }
    presentation.verify()?;

    let verified = verify_credential_set(
        set,
        &leaf_subject,
        config,
        current_topology_epoch,
        floors,
        now_secs,
        skew_secs,
    )?;

    // The requested target must lie inside the granted scope, and the
    // requested rights inside the granted rights: a session is
    // admitted for what it asked for, never more.
    if !verified
        .scope
        .is_ancestor_or_self_of(presentation.target.path)
    {
        return Err(SubnetAuthError::ScopeNotAncestor);
    }
    if !verified.rights.contains(presentation.requested_rights) {
        return Err(SubnetAuthError::RightNotGranted);
    }

    Ok(VerifiedSubnetContext {
        authority: verified.authority,
        scope: verified.scope,
        topology_epoch: verified.topology_epoch,
        subject_node: verified.subject.node_id(),
        subject: verified.subject,
        session_id: expected.session_id,
        rights: presentation.requested_rights,
        generation: verified.generation,
        subnet_auth_epoch: verified.subnet_auth_epoch,
        expires_at: verified.expires_at,
        credential_set_hash: verified.credential_set_hash,
    })
}

// ---------------------------------------------------------------------------
// Wire helpers
// ---------------------------------------------------------------------------

#[expect(
    clippy::unwrap_used,
    reason = "callers pre-check exact buffer length; fixed-width slicing is statically infallible"
)]
fn read_32(bytes: &[u8], off: &mut usize) -> [u8; 32] {
    let out: [u8; 32] = bytes[*off..*off + 32].try_into().unwrap();
    *off += 32;
    out
}

#[expect(
    clippy::unwrap_used,
    reason = "callers pre-check exact buffer length; fixed-width slicing is statically infallible"
)]
fn read_u32(bytes: &[u8], off: &mut usize) -> u32 {
    let out = u32::from_le_bytes(bytes[*off..*off + 4].try_into().unwrap());
    *off += 4;
    out
}

#[expect(
    clippy::unwrap_used,
    reason = "callers pre-check exact buffer length; fixed-width slicing is statically infallible"
)]
fn read_u64(bytes: &[u8], off: &mut usize) -> u64 {
    let out = u64::from_le_bytes(bytes[*off..*off + 8].try_into().unwrap());
    *off += 8;
    out
}
