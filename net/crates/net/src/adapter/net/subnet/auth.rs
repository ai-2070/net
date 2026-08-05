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

use super::id::{TopologySubnetId, MAX_DEPTH};
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
    /// More distinct local gateway scopes than
    /// [`MAX_GATEWAY_CONTEXTS_PER_AUTHORITY`].
    TooManyGatewayContexts,
    /// Entries compiled against different topology or auth epochs were
    /// offered as one gateway set.
    ///
    /// The set folds those epochs into one value at publication so the
    /// packet path can check currency in constant time; that fold is
    /// only sound if every entry agrees. A mixed set means the caller
    /// merged compilations from either side of an epoch bump, and the
    /// honest answer is to recompile, not to pick a winner.
    MixedGatewayEpochs,
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

impl SubnetAuthError {
    /// Every reason code, in canonical order. `ALL` and
    /// [`Self::wire_kind`] are the ONE source of the `subnet:` kind
    /// vocabulary: `Display` renders through `wire_kind`, and the
    /// cross-language fixture generator enumerates `ALL` and calls
    /// `wire_kind` on each — so a renamed token moves everywhere at
    /// once, and there is no second spelling table to drift.
    ///
    /// Completeness is compiler-enforced by the wildcard-free match in
    /// `wire_kind` plus the `all_is_complete` test below, which walks
    /// `ALL` and asserts the length against a locally exhaustive
    /// destructuring — a new variant fails to compile until it is added
    /// to both.
    pub const ALL: &'static [SubnetAuthError] = &[
        Self::UnknownAuthority,
        Self::WrongSubject,
        Self::WrongAuthority,
        Self::WrongTopologyEpoch,
        Self::ScopeNotAncestor,
        Self::InvalidRights,
        Self::Expired,
        Self::NotYetValid,
        Self::LifetimeTooWide,
        Self::Revoked,
        Self::IssuerNotAuthorized,
        Self::IssuerAttenuationBroadened,
        Self::RightNotGranted,
        Self::WrongSession,
        Self::WrongVerifier,
        Self::WrongChallenge,
        Self::IdentityPinConflict,
        Self::TooManyGatewayContexts,
        Self::MixedGatewayEpochs,
        Self::InvalidSignature,
        Self::InvalidFormat,
        Self::InvalidValidityWindow,
        Self::ClockSkewTooLarge,
    ];

    /// The stable wire token for this reason code — the `<kind>` in a
    /// `subnet:<kind>` envelope.
    ///
    /// Returns `&'static str` rather than going through `Display` so
    /// callers that need the token (the fixture generator, the binding
    /// error mappers) do not allocate, and so the tokens can be
    /// enumerated without formatting.
    pub const fn wire_kind(&self) -> &'static str {
        match self {
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
            Self::TooManyGatewayContexts => "too_many_gateway_contexts",
            Self::MixedGatewayEpochs => "mixed_gateway_epochs",
            Self::InvalidSignature => "invalid_signature",
            Self::InvalidFormat => "invalid_format",
            Self::InvalidValidityWindow => "invalid_validity_window",
            Self::ClockSkewTooLarge => "clock_skew_too_large",
        }
    }
}

impl std::fmt::Display for SubnetAuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.wire_kind())
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
/// can never roll state backward). Accepting a floor that changes
/// ENFORCEABLE state increments the authority's auth epoch; compiled
/// session contexts pin the epoch they verified against and fail one
/// integer comparison when it moves (S3), keeping revocation out of
/// per-packet maps. See [`Self::apply`] for what an epoch advance
/// costs and why a floor that revokes nothing is charged none of it.
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
    /// configured root. Returns `Ok(true)` iff the floor changed
    /// *enforceable* state — and the auth epoch advanced with it.
    /// Stale revisions and non-raising floors are `Ok(false)` no-ops
    /// — replay/reorder can never roll backward.
    ///
    /// The epoch advance is the expensive half of acceptance: the
    /// epoch is per AUTHORITY while floors are keyed per
    /// `(authority, topology_epoch, path)`, so one advance
    /// invalidates every compiled context under the authority, and
    /// every one of those peers must re-present against a fresh
    /// challenge. That blast radius is the designed cost of a real
    /// revocation, so it is charged only for one: a floor with
    /// `minimum_generation` `0` — revoking nothing, by definition —
    /// is stored (its revision still anchors the stream's
    /// monotonicity) but returns `Ok(false)` and leaves the epoch
    /// alone. A provisioning run that lays down one placeholder
    /// floor per scope therefore churns nobody.
    ///
    /// `topology_epoch` is used as signed, never compared against
    /// the node's current epoch: a floor minted for a future or
    /// stale epoch is stored state that enforces nothing until
    /// [`Self::max_floor`] is queried at that epoch, and costs the
    /// invalidation up front only when materially restrictive.
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
                // A first floor for a never-seen key is a revocation
                // only if it kills some credential; at generation 0
                // it kills none, so it must not cost the
                // authority-wide invalidation an epoch advance
                // triggers. The entry is stored either way.
                changed = floor.minimum_generation > 0;
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
    /// The exact admitted topology point — `presentation.target.path`,
    /// verified to lie inside `scope`.
    ///
    /// This, not [`Self::scope`], is where the peer *is*. A grant
    /// scoped at `vehicle` and presented for
    /// `vehicle/perception/camera-domain` admits the camera domain
    /// only; treating `scope` as the peer's location would place that
    /// one peer everywhere beneath `vehicle` at once and let a
    /// gateway mistake an internal transition for a boundary
    /// crossing (or the reverse). Forwarding reads `attachment`.
    pub attachment: TopologySubnetId,
    /// Ceiling: the root of the subtree the credential permits. Bounds
    /// what `attachment` may be and what a re-presentation may claim;
    /// it is never itself a location.
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

    // A peer context deliberately exposes NO forwarding decision.
    // Forwarding authority belongs to the gateway itself
    // ([`VerifiedGatewayContextSet::authorize_transition`]); a peer
    // proving it may attach somewhere says nothing about whether THIS
    // node may relay for it. See SUBNET_AUTH_PLAN.md D6.
}

// ---------------------------------------------------------------------------
// Local gateway authority (S4A / D6)
// ---------------------------------------------------------------------------

/// One locally-held forwarding authority, compiled from a credential
/// whose subject is THIS process (SUBNET_AUTH_PLAN.md D6).
///
/// Deliberately a distinct type from [`VerifiedSubnetContext`]: a
/// remote peer's admitted session may never be silently reused as
/// local gateway authority. The peer context answers "where is that
/// peer, and what did it prove"; this answers "what may this node do
/// with traffic between two such peers".
///
/// No challenge is involved. A self-challenge adds nothing because the
/// process already holds the private key; what is required instead is
/// `leaf.subject == local EntityKeypair.entity_id()`, checked at
/// compile time in [`compile_gateway_context`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedGatewayContext {
    /// Authority the credential verified under.
    pub authority: EntityId,
    /// This node's own attachment point within `scope`.
    pub attachment: TopologySubnetId,
    /// The subtree this entry's rights apply to. Boundary decisions
    /// test containment against THIS.
    pub scope: TopologySubnetId,
    /// Topology epoch verified against.
    pub topology_epoch: u32,
    /// The local entity — equal to this process's `EntityId`.
    pub subject: EntityId,
    /// Rights held over `scope`.
    pub rights: SubnetRights,
    /// Leaf generation at verification time.
    pub generation: u32,
    /// Authority auth epoch at verification time.
    pub subnet_auth_epoch: u64,
    /// Leaf expiry (unix seconds, exclusive).
    pub expires_at: u64,
    /// Hash of the credentials that produced this entry.
    pub credential_set_hash: [u8; 32],
}

/// Operator cap on locally-held gateway entries per authority. A
/// gateway legitimately holds a few (e.g. `ROUTE(vehicle)` plus
/// `EXPORT(world-model)`); an unbounded set would turn the
/// per-transition evaluation into an operator-sized scan.
pub const MAX_GATEWAY_CONTEXTS_PER_AUTHORITY: usize = 32;

/// Immutable scope→rights index, compiled once at publication.
///
/// Sorted by raw scope so a lookup is a binary search over one small
/// contiguous array. The point is not that the array is short — with a
/// single local attachment the entry scopes already form one ancestor
/// chain — but that the packet path asks *"what do I hold at exactly
/// this path?"* instead of *"which of my entries contains this
/// path?"*. The first question is answered by lookup at a bounded set
/// of candidate paths; the second is a walk of the context collection,
/// and its cost is whatever an operator provisioned.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GatewayScopeIndex {
    /// `(scope.raw(), rights)`, ascending by scope, one entry per
    /// distinct scope.
    by_scope: Box<[(u32, SubnetRights)]>,
}

impl GatewayScopeIndex {
    /// Build from already-deduplicated entries. Off the packet path by
    /// construction: publication is the only caller.
    fn build(entries: &[VerifiedGatewayContext]) -> Self {
        let mut by_scope: Vec<(u32, SubnetRights)> =
            entries.iter().map(|e| (e.scope.raw(), e.rights)).collect();
        by_scope.sort_unstable_by_key(|(scope, _)| *scope);
        Self {
            by_scope: by_scope.into_boxed_slice(),
        }
    }

    /// Whether this index grants anything at all.
    #[inline]
    fn is_empty(&self) -> bool {
        self.by_scope.is_empty()
    }

    /// Rights held at **exactly** `scope`, if any. One probe.
    ///
    /// Deliberately exact rather than containment-aware: containment
    /// is the caller's ancestor walk, and folding it in here would put
    /// the scan back.
    #[inline]
    fn rights_at(&self, scope: TopologySubnetId) -> Option<SubnetRights> {
        self.by_scope
            .binary_search_by_key(&scope.raw(), |(candidate, _)| *candidate)
            .ok()
            .map(|i| self.by_scope[i].1)
    }
}

/// The immutable, authority-local set of this node's forwarding
/// authorities, deduplicated by scope and capped by
/// [`MAX_GATEWAY_CONTEXTS_PER_AUTHORITY`].
///
/// Everything the packet path needs is precomputed here: the scope
/// index, and the epochs and earliest expiry folded across every entry
/// so currency is a constant-time comparison rather than a walk.
///
/// # Every field is private, deliberately
///
/// The index is derived from `entries`, and the epochs and expiry are
/// folded from them. Any field left publicly writable lets those views
/// disagree — and the failure is not symmetric. Assigning an empty
/// `entries` used to leave the compiled index still granting `ROUTE`
/// while the emptiness shortcut skipped the currency check, so
/// *removing* authority left authority active. Publication replaces the
/// whole value; nothing here is independently assignable, and the
/// accessors are read-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedGatewayContextSet {
    /// Authority every entry belongs to.
    authority: EntityId,
    /// Compiled entries. Immutable once published; refresh and
    /// revocation recompute the whole set atomically rather than
    /// mutating entries, so stale rights can never accumulate.
    entries: Box<[VerifiedGatewayContext]>,
    /// Topology epoch shared by every entry.
    topology_epoch: u32,
    /// Authority auth epoch shared by every entry.
    subnet_auth_epoch: u64,
    /// Tightest expiry across every entry. The whole set stops
    /// authorizing when the shortest-lived credential in it does —
    /// checking the minimum is exactly checking all of them.
    earliest_expiry: u64,
    /// Off-path scope index, and the thing that actually authorizes.
    index: GatewayScopeIndex,
}

/// Ceiling on indexed lookups for one protected transition.
///
/// A boundary can only separate the two attachments if it lies on one
/// of their ancestor chains strictly below their common ancestor, so
/// there are at most `MAX_DEPTH` boundary lookups per endpoint, plus at
/// most one `EXPORT` lookup per boundary actually crossed:
/// `4 × MAX_DEPTH`. The internal-`ROUTE` path is strictly cheaper —
/// `2 × MAX_DEPTH` boundary lookups and at most `MAX_ANCESTOR_PATH`
/// `ROUTE` lookups — so this bounds both branches.
///
/// # What this does and does not claim
///
/// It counts **lookup calls**, not comparisons or memory accesses.
/// Each call is a binary search, so the real work is about
/// `MAX_DEPTH × log(boundary_count)` plus
/// `MAX_DEPTH × log(grant_count)`. The honest claim is therefore: a
/// depth-bounded number of indexed lookups, with no linear credential
/// or boundary scan anywhere on the packet path. It is *not* a claim
/// of literal inventory-independent CPU cost — the grant count is
/// capped by [`MAX_GATEWAY_CONTEXTS_PER_AUTHORITY`], but the boundary
/// inventory is currently uncapped, so its logarithm is real.
pub const MAX_TRANSITION_LOOKUPS: u32 = 4 * MAX_DEPTH as u32;

/// The mandatory inventory of protected boundaries for one authority.
///
/// Boundaries and the rights that satisfy them must come from
/// **different** surfaces. S4A discovered boundaries from the gateway's
/// own credential set, which inverted revocation: dropping an
/// `EXPORT(world-model)` credential removed the world-model entry, so
/// the crossing stopped being a crossing and a broader
/// `ROUTE(vehicle)` silently carried the same traffic. Removing a
/// credential widened authority.
///
/// Here the boundary exists independently of any credential. Removing
/// `EXPORT(world-model)` leaves the boundary standing and unsatisfied,
/// which denies — the fail-closed direction. An absent boundary set
/// denies every protected transition outright, so a gateway cannot
/// gain authority by forgetting to configure one.
///
/// V1 sources this from operator configuration; S5's signed
/// `ExportPolicy` fact is the distribution mechanism, and it replaces
/// the whole set atomically for the same reason the credential set is
/// republished whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubnetBoundarySet {
    /// Authority these boundaries belong to.
    authority: EntityId,
    /// Topology epoch they were declared under. A boundary means
    /// nothing once paths are reinterpreted.
    topology_epoch: u32,
    /// Subtree roots whose edge is a protected boundary, ascending by
    /// raw path and deduplicated. Crossing one requires `EXPORT` held
    /// at exactly that scope.
    ///
    /// Private because the sort order is load-bearing: the packet path
    /// asks "is this exact path a boundary?" by binary search, so an
    /// unsorted set built by struct literal would silently answer
    /// "no" and turn a declared boundary into an internal transition.
    boundaries: Box<[TopologySubnetId]>,
}

impl SubnetBoundarySet {
    /// Build a boundary set, deduplicating and ordering scopes.
    pub fn new(
        authority: EntityId,
        topology_epoch: u32,
        boundaries: impl IntoIterator<Item = TopologySubnetId>,
    ) -> Self {
        let mut sorted: Vec<TopologySubnetId> = boundaries.into_iter().collect();
        sorted.sort_unstable_by_key(|b| b.raw());
        sorted.dedup();
        Self {
            authority,
            topology_epoch,
            boundaries: sorted.into_boxed_slice(),
        }
    }

    /// Authority these boundaries belong to.
    pub fn authority(&self) -> &EntityId {
        &self.authority
    }

    /// Topology epoch they were declared under.
    pub fn topology_epoch(&self) -> u32 {
        self.topology_epoch
    }

    /// The declared boundaries, ascending by path.
    pub fn boundaries(&self) -> &[TopologySubnetId] {
        &self.boundaries
    }

    /// Is `scope` **exactly** a declared boundary? One probe.
    ///
    /// Exact, not containment: the packet path supplies the candidate
    /// paths from the endpoints' ancestor chains, which is what keeps
    /// this from becoming a walk of the inventory.
    #[inline]
    fn is_boundary(&self, scope: TopologySubnetId) -> bool {
        self.boundaries
            .binary_search_by_key(&scope.raw(), |b| b.raw())
            .is_ok()
    }
}

/// The immutable binding of one exported nRPC service to one exact
/// protected crossing (SUBNET_AUTH_PLAN.md D7).
///
/// Captured by a `serve_rpc_subnet_exported` registration and never
/// mutated afterwards: the service exports through exactly this
/// authority-qualified path, declared under exactly this topology
/// epoch. The epoch is load-bearing — if paths are reinterpreted
/// under a newer epoch, an old registration must stay dark until
/// explicitly re-registered; it must not silently transfer to the
/// same path bits with a new meaning.
///
/// Deliberately NO target organization and NO caller subnet:
/// organization admission already controls who may ask, and the
/// external caller never joins the provider's subnet at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubnetExportBinding {
    /// The exact crossing the service exports through.
    subnet: SubnetRef,
    /// The topology epoch the binding was declared under.
    topology_epoch: u32,
}

impl SubnetExportBinding {
    /// Bind to exactly `subnet` under exactly `topology_epoch`.
    pub fn new(subnet: SubnetRef, topology_epoch: u32) -> Self {
        Self {
            subnet,
            topology_epoch,
        }
    }

    /// The bound authority-qualified crossing.
    pub fn subnet(&self) -> &SubnetRef {
        &self.subnet
    }

    /// The topology epoch the binding was declared under.
    pub fn topology_epoch(&self) -> u32 {
        self.topology_epoch
    }
}

/// Why a protected forwarding transition was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForwardDenial {
    /// An ingress/egress context is missing, expired, or minted under
    /// a different authority or topology epoch than the local set.
    ContextNotCurrent,
    /// A hop peer's admitted context does not carry `ATTACH`.
    AttachMissing,
    /// The transition crosses a configured boundary and some crossed
    /// entry lacks `EXPORT`.
    ExportMissing,
    /// The transition is wholly internal but no local entry contains
    /// both attachments with `ROUTE`.
    RouteMissing,
}

/// A transition verdict together with the number of indexed lookups it
/// cost.
///
/// The count exists so the bound in [`MAX_TRANSITION_LOOKUPS`] is
/// *observable* rather than asserted in prose: a test can pin that
/// evaluating the same transition against a two-entry gateway and a
/// thirty-two-entry gateway issues the same number of lookups. It is a
/// `u32` counter on a path that already does keyed hashing per packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransitionDecision {
    /// Authorized, or why not.
    pub verdict: Result<(), ForwardDenial>,
    /// Index lookup calls performed — never more than
    /// [`MAX_TRANSITION_LOOKUPS`]. Each is a binary search, so this is
    /// a count of calls, not of comparisons.
    pub lookup_calls: u32,
}

impl VerifiedGatewayContextSet {
    /// Authority every entry belongs to.
    pub fn authority(&self) -> &EntityId {
        &self.authority
    }

    /// The compiled entries, for diagnostics and operator display.
    ///
    /// Read-only on purpose: the forwarding decision consults the
    /// derived index, not this slice, and letting a caller replace it
    /// would let the two disagree.
    pub fn entries(&self) -> &[VerifiedGatewayContext] {
        &self.entries
    }

    /// Topology epoch shared by every entry.
    pub fn topology_epoch(&self) -> u32 {
        self.topology_epoch
    }

    /// Authority auth epoch shared by every entry.
    pub fn subnet_auth_epoch(&self) -> u64 {
        self.subnet_auth_epoch
    }

    /// Tightest expiry across every entry.
    pub fn earliest_expiry(&self) -> u64 {
        self.earliest_expiry
    }
}

impl VerifiedGatewayContextSet {
    /// The D6 forwarding decision, evaluated over hop-local
    /// **attachments** — never grant scopes, never a wire-claimed
    /// subnet.
    ///
    /// ```text
    /// crossed = BOUNDARIES where b.contains(source) != b.contains(target)
    /// if crossed is non-empty: require EXPORT held at EVERY crossed boundary
    /// else:                    require one entry containing both, with ROUTE
    /// ```
    ///
    /// `boundaries` is a separate mandatory surface, not this set —
    /// see [`SubnetBoundarySet`] for why inferring boundaries from the
    /// credentials that satisfy them let revocation widen authority.
    /// The crossed test runs first, so a broad `ROUTE(vehicle)` cannot
    /// carry traffic out through a declared `world-model` boundary:
    /// that boundary is crossed, so `EXPORT` at exactly its scope is
    /// demanded regardless of what any wider entry permits. Crossing
    /// two declared sibling boundaries needs `EXPORT` for both.
    ///
    /// Cost is bounded by the hierarchy depth rather than by a linear
    /// scan — see [`Self::authorize_transition_counted`], which is this
    /// method plus the lookup count that makes the bound checkable.
    pub fn authorize_transition(
        &self,
        ingress: &VerifiedSubnetContext,
        egress: &VerifiedSubnetContext,
        boundaries: &SubnetBoundarySet,
        current_topology_epoch: u32,
        current_subnet_auth_epoch: u64,
        now_secs: u64,
    ) -> Result<(), ForwardDenial> {
        self.authorize_transition_counted(
            ingress,
            egress,
            boundaries,
            current_topology_epoch,
            current_subnet_auth_epoch,
            now_secs,
        )
        .verdict
    }

    /// May a locally-registered service export through the exact
    /// crossing `binding` names, RIGHT NOW? (SUBNET_AUTH_PLAN.md D7 —
    /// the exported-nRPC composition decision.)
    ///
    /// Deliberately NOT [`Self::authorize_transition`]: that decision
    /// orders two admitted subnet contexts across a hop, and the
    /// externally org-authorized nRPC caller intentionally holds no
    /// such context. This one asks only about the PROVIDER's side —
    /// whether this gateway currently has the authority to expose a
    /// result through the declared boundary:
    ///
    /// - one authority across binding, boundary set, and this set;
    /// - one topology epoch across binding, boundary set, this set,
    ///   AND the node's current epoch — a binding declared under a
    ///   reinterpreted hierarchy stays dark until re-registered;
    /// - this set's auth epoch equals the CURRENT floor-registry
    ///   epoch (a signed floor kills it), and it is unexpired;
    /// - the binding path is EXACTLY a declared boundary;
    /// - `EXPORT` is held at EXACTLY that path. Exact means exact:
    ///   `EXPORT` at `vehicle` does not satisfy a service bound to
    ///   `world-model` — no ancestor inheritance, per the same rule
    ///   the packet path applies at crossed boundaries.
    ///
    /// Missing boundary state, epoch drift, revocation, expiry, and
    /// absent exact `EXPORT` all deny. Two probes, no inventory walk.
    pub fn authorize_service_export(
        &self,
        binding: &SubnetExportBinding,
        boundaries: &SubnetBoundarySet,
        current_topology_epoch: u32,
        current_subnet_auth_epoch: u64,
        now_secs: u64,
    ) -> Result<(), ForwardDenial> {
        let scope = binding.subnet();
        if scope.authority != self.authority || scope.authority != *boundaries.authority() {
            return Err(ForwardDenial::ContextNotCurrent);
        }
        if binding.topology_epoch() != current_topology_epoch
            || self.topology_epoch != current_topology_epoch
            || boundaries.topology_epoch() != current_topology_epoch
        {
            return Err(ForwardDenial::ContextNotCurrent);
        }
        if self.subnet_auth_epoch != current_subnet_auth_epoch {
            return Err(ForwardDenial::ContextNotCurrent);
        }
        if now_secs >= self.earliest_expiry {
            return Err(ForwardDenial::ContextNotCurrent);
        }
        if !boundaries.is_boundary(scope.path) {
            return Err(ForwardDenial::ExportMissing);
        }
        match self.index.rights_at(scope.path) {
            Some(rights) if rights.contains(SubnetRights::EXPORT) => Ok(()),
            _ => Err(ForwardDenial::ExportMissing),
        }
    }

    /// [`Self::authorize_transition`], reporting how many indexed
    /// lookups the decision cost.
    ///
    /// Neither the entry array nor the boundary inventory is walked.
    /// Both are keyed by exact scope and consulted only at the paths
    /// that can possibly matter: a boundary separates the two
    /// attachments only if it lies on one of their ancestor chains
    /// strictly below their common ancestor, and a scope contains both
    /// attachments only if it contains their common ancestor. Those two
    /// facts turn "search what I hold" into "look up what I hold here".
    ///
    /// Both facts rest on [`TopologySubnetId::common_ancestor`] being
    /// the true meet of the containment order over the **raw** path
    /// domain, interior zeros included — when it was not, a transition
    /// between two identical attachments could appear to cross a
    /// boundary and let `EXPORT` stand in for `ROUTE`.
    pub fn authorize_transition_counted(
        &self,
        ingress: &VerifiedSubnetContext,
        egress: &VerifiedSubnetContext,
        boundaries: &SubnetBoundarySet,
        current_topology_epoch: u32,
        current_subnet_auth_epoch: u64,
        now_secs: u64,
    ) -> TransitionDecision {
        let refuse = |denial| TransitionDecision {
            verdict: Err(denial),
            lookup_calls: 0,
        };

        if boundaries.authority != self.authority
            || boundaries.topology_epoch != current_topology_epoch
        {
            return refuse(ForwardDenial::ContextNotCurrent);
        }
        // Both hop peers must be admitted under the same authority and
        // epoch this node's own credentials were verified against, and
        // both must actually be attached (ATTACH), not merely known.
        for peer in [ingress, egress] {
            if peer.authority != self.authority
                || peer.topology_epoch != current_topology_epoch
                || peer.subnet_auth_epoch != current_subnet_auth_epoch
                || now_secs >= peer.expires_at
            {
                return refuse(ForwardDenial::ContextNotCurrent);
            }
            if !peer.rights.contains(SubnetRights::ATTACH) {
                return refuse(ForwardDenial::AttachMissing);
            }
        }
        // Local currency, in constant time. The epochs and the tightest
        // expiry were folded across every entry at publication, so a
        // stale credential is caught by three comparisons rather than
        // by walking the set looking for one.
        //
        // The emptiness test reads the INDEX, not `entries`: the index
        // is what grants rights below, so the thing that authorizes and
        // the thing that gates currency are the same object. (Both are
        // private and built together, so they cannot disagree — this is
        // belt and braces for the direction that fails open.)
        if !self.index.is_empty()
            && (self.topology_epoch != current_topology_epoch
                || self.subnet_auth_epoch != current_subnet_auth_epoch
                || now_secs >= self.earliest_expiry)
        {
            return refuse(ForwardDenial::ContextNotCurrent);
        }

        let source = ingress.attachment;
        let target = egress.attachment;
        let common = source.common_ancestor(target);
        let mut lookup_calls = 0u32;

        // Boundaries come from the declared inventory, so a crossing
        // stays a crossing whether or not a credential satisfies it.
        //
        // Candidates are only the two strict chains below `common`: a
        // path containing both endpoints contains `common` and so is
        // at or above it, and a path containing neither is on no chain
        // at all. Either way it is not crossed, so it is not worth a
        // lookup.
        //
        // When the two attachments are equal, `common` is the
        // attachment itself and both loops terminate immediately —
        // there is no such thing as crossing a boundary to reach
        // yourself, and treating one as crossed is exactly how EXPORT
        // came to substitute for ROUTE.
        let mut crossed_any = false;
        for endpoint in [source, target] {
            for scope in endpoint.ancestor_path() {
                if scope == common {
                    break;
                }
                lookup_calls += 1;
                if !boundaries.is_boundary(scope) {
                    continue;
                }
                crossed_any = true;
                // EXPORT must be held at exactly the crossed scope. A
                // wider EXPORT elsewhere is authority over a different
                // boundary, not this one.
                lookup_calls += 1;
                let exports = self
                    .index
                    .rights_at(scope)
                    .is_some_and(|r| r.contains(SubnetRights::EXPORT));
                if !exports {
                    return TransitionDecision {
                        verdict: Err(ForwardDenial::ExportMissing),
                        lookup_calls,
                    };
                }
            }
        }
        if crossed_any {
            return TransitionDecision {
                verdict: Ok(()),
                lookup_calls,
            };
        }

        // Wholly internal: some held scope must contain both endpoints
        // and carry ROUTE. A scope contains both iff it contains their
        // common ancestor, so the candidates are one chain, not two.
        for scope in common.ancestor_path() {
            lookup_calls += 1;
            let routes = self
                .index
                .rights_at(scope)
                .is_some_and(|r| r.contains(SubnetRights::ROUTE));
            if routes {
                return TransitionDecision {
                    verdict: Ok(()),
                    lookup_calls,
                };
            }
        }
        TransitionDecision {
            verdict: Err(ForwardDenial::RouteMissing),
            lookup_calls,
        }
    }
}

/// Compile one local gateway entry from a self-held credential set.
///
/// Beyond the ordinary D4/D5 credential checks this requires
/// `leaf.subject == local_keypair_entity`, so a credential issued to
/// some other node can never become this node's forwarding authority.
///
/// # Scope relation to `local_attachment`
///
/// **Any credential containing [`SubnetRights::ATTACH`] uses PLACEMENT
/// containment, regardless of its other rights.** `ATTACH` is a claim
/// about where this node sits, so the scope must be an ancestor of (or
/// equal to) `local_attachment`. Adding `ROUTE` or `EXPORT` alongside
/// it does not relax that: otherwise a descendant-scoped
/// `ATTACH | EXPORT` would let a vehicle-attached gateway assert it
/// belongs at `WORLD_MODEL`.
///
/// **Delegated forwarding rights that do NOT include `ATTACH`** may
/// name a scope on either side of the attachment — a gateway forwards
/// across boundaries below where it sits. The scope must still be
/// chain-related to `local_attachment` (one an ancestor of the other);
/// an unrelated branch is always [`SubnetAuthError::ScopeNotAncestor`].
///
/// So descendant `ROUTE`/`EXPORT` delegation requires a SEPARATE,
/// forwarding-only credential:
///
/// ```text
/// attachment: [3]
/// scope:      [3,7,1]
/// rights:     ATTACH | EXPORT      → ScopeNotAncestor
///
/// attachment: [3]
/// scope:      [3]
/// rights:     ATTACH | ROUTE       → compiles
/// attachment: [3]
/// scope:      [3,7,1]
/// rights:     EXPORT               → compiles
/// ```
///
/// The split form is the supported way to hold both; the two
/// credentials publish together. All three rows are pinned by
/// `gateway_compiler_accepts_delegated_descendant_export_but_not_attach`
/// in `tests/subnet_auth_e2e.rs`.
///
/// [`SubnetAuthError::ScopeNotAncestor`] is the correct verdict for
/// the rejected row — the scope genuinely is not an ancestor of the
/// attachment — so no distinct error variant exists for it.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors verify_credential_set's explicit trusted-input list; a struct would hide \
              which inputs the caller must supply from local state"
)]
pub fn compile_gateway_context(
    set: &SubnetCredentialSet,
    local_entity: &EntityId,
    local_attachment: TopologySubnetId,
    config: &SubnetAuthorityConfig,
    current_topology_epoch: u32,
    floors: &SubnetFloorRegistry,
    now_secs: u64,
    skew_secs: u64,
) -> Result<VerifiedGatewayContext, SubnetAuthError> {
    if &set.leaf().subject != local_entity {
        return Err(SubnetAuthError::WrongSubject);
    }
    let verified = verify_credential_set(
        set,
        local_entity,
        config,
        current_topology_epoch,
        floors,
        now_secs,
        skew_secs,
    )?;
    // The scope/attachment relation differs by what the credential
    // CLAIMS:
    //
    // - ATTACH says where this node BELONGS, so its scope must
    //   contain the actual attachment — placement cannot be claimed
    //   for a subtree the node is not in.
    // - A ROUTE- or EXPORT-only credential is DELEGATED forwarding
    //   authority. The plan's own provisioning ("EXPORT at
    //   world-model" on a vehicle-attached gateway) names a scope
    //   the gateway is not attached under, and the signed credential
    //   naming this gateway as subject is what authorizes that
    //   delegation — demanding containment there conflated "where I
    //   am" with "what I may forward". But delegation is still
    //   HIERARCHY-CHAINED: the scope and the attachment must lie on
    //   one ancestor chain (either may contain the other). A scope
    //   on an unrelated branch has no path through this gateway at
    //   all, and compiling it would let a root mint forwarding
    //   authority that placement can never exercise.
    let scope_contains_attachment = verified.scope.is_ancestor_or_self_of(local_attachment);
    if verified.rights.contains(SubnetRights::ATTACH) {
        if !scope_contains_attachment {
            return Err(SubnetAuthError::ScopeNotAncestor);
        }
    } else if !scope_contains_attachment && !local_attachment.is_ancestor_or_self_of(verified.scope)
    {
        return Err(SubnetAuthError::ScopeNotAncestor);
    }
    Ok(VerifiedGatewayContext {
        authority: verified.authority,
        attachment: local_attachment,
        scope: verified.scope,
        topology_epoch: verified.topology_epoch,
        subject: verified.subject,
        rights: verified.rights,
        generation: verified.generation,
        subnet_auth_epoch: verified.subnet_auth_epoch,
        expires_at: verified.expires_at,
        credential_set_hash: verified.credential_set_hash,
    })
}

/// Build the immutable published set from compiled entries.
///
/// Entries with the same scope are combined by rights union — but only
/// across simultaneously-current credentials, because the caller
/// recompiles the whole set on refresh or revocation rather than
/// merging into a live one. That is what keeps a revoked credential's
/// rights from surviving inside a surviving entry.
///
/// Rejects a set exceeding [`MAX_GATEWAY_CONTEXTS_PER_AUTHORITY`]
/// distinct scopes, any entry from a different authority, or entries
/// compiled against different epochs
/// ([`SubnetAuthError::MixedGatewayEpochs`]).
///
/// This is where the packet path's off-path state is built: the scope
/// index, and the epochs and earliest expiry folded across every
/// entry. All of it is derived from `compiled` and none of it is
/// reachable for mutation afterwards, so what forwarding consults can
/// never drift from what was verified.
pub fn build_gateway_context_set(
    authority: &EntityId,
    compiled: Vec<VerifiedGatewayContext>,
) -> Result<VerifiedGatewayContextSet, SubnetAuthError> {
    let mut entries: Vec<VerifiedGatewayContext> = Vec::with_capacity(compiled.len());
    for entry in compiled {
        if &entry.authority != authority {
            return Err(SubnetAuthError::WrongAuthority);
        }
        // The set publishes one topology/auth epoch pair for the whole
        // collection; entries straddling an epoch bump would make that
        // fold a lie, so they are refused rather than reconciled.
        if let Some(first) = entries.first() {
            if entry.topology_epoch != first.topology_epoch
                || entry.subnet_auth_epoch != first.subnet_auth_epoch
            {
                return Err(SubnetAuthError::MixedGatewayEpochs);
            }
        }
        match entries.iter_mut().find(|e| e.scope == entry.scope) {
            Some(existing) => {
                existing.rights = existing.rights.union(entry.rights);
                // Keep the tightest expiry so the merged entry cannot
                // outlive the shorter-lived credential behind it.
                existing.expires_at = existing.expires_at.min(entry.expires_at);
            }
            None => {
                entries.push(entry);
                // Enforced at each push, not after the loop: the cap
                // is on DISTINCT scopes (an input merging down to few
                // scopes is fine however long it is), and checking
                // here bounds the linear probe above to the cap
                // instead of letting a caller-supplied Vec make the
                // merge quadratic in its own length. Same accept set:
                // the distinct count only grows.
                if entries.len() > MAX_GATEWAY_CONTEXTS_PER_AUTHORITY {
                    return Err(SubnetAuthError::TooManyGatewayContexts);
                }
            }
        }
    }
    let topology_epoch = entries.first().map_or(0, |e| e.topology_epoch);
    let subnet_auth_epoch = entries.first().map_or(0, |e| e.subnet_auth_epoch);
    // An empty set never authorizes anything, so its expiry is
    // vacuous; u64::MAX keeps the currency check from firing ahead of
    // the rights check that is the real reason it denies.
    let earliest_expiry = entries
        .iter()
        .map(|e| e.expires_at)
        .min()
        .unwrap_or(u64::MAX);
    let index = GatewayScopeIndex::build(&entries);
    Ok(VerifiedGatewayContextSet {
        authority: authority.clone(),
        entries: entries.into_boxed_slice(),
        topology_epoch,
        subnet_auth_epoch,
        earliest_expiry,
        index,
    })
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
    // Defence in depth, NOT the one-use enforcement: `expected` is
    // built from the challenge the store already matched against this
    // same nonce, so through the production call path this comparison
    // cannot fail — the single-use property is `consume`'s
    // unconditional removal. Kept (and constant-time) so a caller
    // that assembles an `ExpectedBinding` some other way still cannot
    // pass a foreign nonce through.
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
        // The exact point admitted, checked above to lie inside the
        // credential scope. Never widened to `scope`.
        attachment: presentation.target.path,
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
// Wire helpers (shared with the S5 control-fact family in `control.rs`)
// ---------------------------------------------------------------------------

#[expect(
    clippy::unwrap_used,
    reason = "callers pre-check exact buffer length; fixed-width slicing is statically infallible"
)]
pub(super) fn read_32(bytes: &[u8], off: &mut usize) -> [u8; 32] {
    let out: [u8; 32] = bytes[*off..*off + 32].try_into().unwrap();
    *off += 32;
    out
}

#[expect(
    clippy::unwrap_used,
    reason = "callers pre-check exact buffer length; fixed-width slicing is statically infallible"
)]
pub(super) fn read_u32(bytes: &[u8], off: &mut usize) -> u32 {
    let out = u32::from_le_bytes(bytes[*off..*off + 4].try_into().unwrap());
    *off += 4;
    out
}

#[expect(
    clippy::unwrap_used,
    reason = "callers pre-check exact buffer length; fixed-width slicing is statically infallible"
)]
pub(super) fn read_u64(bytes: &[u8], off: &mut usize) -> u64 {
    let out = u64::from_le_bytes(bytes[*off..*off + 8].try_into().unwrap());
    *off += 8;
    out
}

#[cfg(test)]
mod wire_kind_tests {
    use super::SubnetAuthError as E;

    /// `ALL` really is every variant, and every token is distinct and
    /// non-empty.
    ///
    /// The destructuring below is wildcard-free, so adding a variant
    /// fails THIS compile until it is listed here; the length assertion
    /// then fails until it is added to `ALL` too. Together they close
    /// the gap a plain array cannot: an array can be short, and a match
    /// can be exhaustive, but only both can prove they agree.
    #[test]
    fn all_is_complete_and_tokens_are_distinct() {
        fn assert_exhaustive(e: E) -> u32 {
            match e {
                E::UnknownAuthority => 0,
                E::WrongSubject => 1,
                E::WrongAuthority => 2,
                E::WrongTopologyEpoch => 3,
                E::ScopeNotAncestor => 4,
                E::InvalidRights => 5,
                E::Expired => 6,
                E::NotYetValid => 7,
                E::LifetimeTooWide => 8,
                E::Revoked => 9,
                E::IssuerNotAuthorized => 10,
                E::IssuerAttenuationBroadened => 11,
                E::RightNotGranted => 12,
                E::WrongSession => 13,
                E::WrongVerifier => 14,
                E::WrongChallenge => 15,
                E::IdentityPinConflict => 16,
                E::TooManyGatewayContexts => 17,
                E::MixedGatewayEpochs => 18,
                E::InvalidSignature => 19,
                E::InvalidFormat => 20,
                E::InvalidValidityWindow => 21,
                E::ClockSkewTooLarge => 22,
            }
        }
        const EXPECTED: usize = 23;
        assert_eq!(
            E::ALL.len(),
            EXPECTED,
            "a variant was added to the enum but not to SubnetAuthError::ALL",
        );

        // Every ordinal appears exactly once — catches a duplicate or a
        // transposition in `ALL`, which a length check alone would miss.
        let mut ordinals: Vec<u32> = E::ALL.iter().map(|e| assert_exhaustive(*e)).collect();
        ordinals.sort_unstable();
        assert_eq!(ordinals, (0..EXPECTED as u32).collect::<Vec<_>>());

        let tokens: std::collections::BTreeSet<&str> =
            E::ALL.iter().map(|e| e.wire_kind()).collect();
        assert_eq!(
            tokens.len(),
            EXPECTED,
            "reason-code tokens must be distinct"
        );
        assert!(E::ALL.iter().all(|e| !e.wire_kind().is_empty()));
    }

    /// `Display` renders exactly `wire_kind` — for EVERY variant, not
    /// just the ends of the list. This is what stops an intermediate
    /// token from being renamed in one place and not the other.
    #[test]
    fn display_is_wire_kind_for_every_variant() {
        for e in E::ALL {
            assert_eq!(e.to_string(), e.wire_kind());
        }
    }
}
