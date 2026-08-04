//! Signed subnet control facts — S5 of
//! `docs/internal/plans/SUBNET_AUTH_PLAN.md` (D8).
//!
//! V1 defines exactly four independently signed facts:
//!
//! - [`SubnetDescriptor`] — "this authority-qualified path exists
//!   under this topology epoch";
//! - [`GatewayAdvertisement`] — "this entity (at this routing id)
//!   serves as a gateway for this subtree";
//! - [`SubnetExportPolicy`] — "exactly these channels are exported at
//!   this subtree's boundary";
//! - [`SubnetRevocationFloor`] — the S2 revocation floor, distributed
//!   unchanged: a floor that arrives as a control fact is the same
//!   bytes, same domain, same verification as one an operator
//!   provisions locally.
//!
//! Every fact carries its [`SubnetRef`] scope, `topology_epoch`, a
//! `revision` scoped per `(SubnetRef, fact kind)`, an issuer, and a
//! domain-separated ed25519 signature. Unknown versions, unknown kind
//! tags, wrong lengths, and trailing bytes all fail closed.
//!
//! **Arrival path changes no verification rule.** A fact may arrive
//! through a configured channel, local provisioning, or a
//! configuration bundle; each path hands the same bytes to the same
//! verifier. Channel membership and publication NEVER establish fact
//! authority — only a signature by a configured root of the fact's
//! own authority does. A hostile publisher on the control channel can
//! therefore inject bytes, and those bytes are inert.
//!
//! **Replay and reorder never roll state backward.** The
//! [`SubnetControlStore`] applies each fact kind monotonically by
//! revision within `(authority, topology_epoch, path)`; a replayed or
//! reordered fact is a no-op, and the revision streams of different
//! kinds are independent — a newer [`GatewayAdvertisement`] cannot
//! suppress a current [`SubnetExportPolicy`].
//!
//! Floors are deliberately NOT stored here: they flow into the S2
//! [`SubnetFloorRegistry`](super::auth::SubnetFloorRegistry), whose
//! `(scope, topology_epoch)`-monotonic application and
//! `subnet_auth_epoch` bump are already the revocation contract
//! (bounded-stale: a verifier may honor an older grant until the
//! newer floor ARRIVES — this module is the arrival machinery).

use dashmap::DashMap;
use ed25519_dalek::Signature;

use super::auth::{
    read_32, read_u32, read_u64, SubnetAuthError, SubnetAuthorityConfig, SubnetRef,
    SubnetRevocationFloor, MAX_TOKEN_CLOCK_SKEW_SECS,
};
use super::id::TopologySubnetId;
use crate::adapter::net::channel::ChannelHash;
use crate::adapter::net::identity::{EntityId, EntityKeypair};

/// Domain prefix for the descriptor's ed25519 transcript.
pub const SUBNET_DESCRIPTOR_SIG_DOMAIN: &[u8] = b"net.subnet.descriptor.v1";
/// Domain prefix for the gateway advertisement's ed25519 transcript.
pub const SUBNET_GATEWAY_AD_SIG_DOMAIN: &[u8] = b"net.subnet.gateway-ad.v1";
/// Domain prefix for the export policy's ed25519 transcript.
pub const SUBNET_EXPORT_POLICY_SIG_DOMAIN: &[u8] = b"net.subnet.export-policy.v1";

/// Ceiling on the number of exported channels one policy fact may
/// name. The policy replaces wholesale (like gateway credential
/// sets), so the bound is per-fact, and decode fails closed above it.
pub const MAX_EXPORTED_CHANNELS: usize = 16;

// ---------------------------------------------------------------------------
// Fact kinds
// ---------------------------------------------------------------------------

/// The four V1 fact kinds, as wire tags. Strict: any other tag is
/// [`SubnetAuthError::InvalidFormat`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum SubnetFactKind {
    /// [`SubnetDescriptor`].
    Descriptor = 1,
    /// [`GatewayAdvertisement`].
    GatewayAdvertisement = 2,
    /// [`SubnetExportPolicy`].
    ExportPolicy = 3,
    /// [`SubnetRevocationFloor`], distributed as a fact.
    RevocationFloor = 4,
}

impl SubnetFactKind {
    /// Strict tag decode; unknown tags fail closed.
    pub fn try_from_tag(tag: u8) -> Result<Self, SubnetAuthError> {
        match tag {
            1 => Ok(Self::Descriptor),
            2 => Ok(Self::GatewayAdvertisement),
            3 => Ok(Self::ExportPolicy),
            4 => Ok(Self::RevocationFloor),
            _ => Err(SubnetAuthError::InvalidFormat),
        }
    }
}

// ---------------------------------------------------------------------------
// SubnetDescriptor
// ---------------------------------------------------------------------------

/// Root-signed declaration that an authority-qualified path exists
/// under a topology epoch (D1: reparenting or reinterpreting a path
/// creates a NEW epoch; adding a fresh descendant under a stable
/// parent does not).
///
/// Deliberately carries no name string or metadata: the descriptor is
/// the "this path is live under epoch E" fact, and the packet path's
/// zero-string-parse budget (D9) starts with the facts themselves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubnetDescriptor {
    /// Wire version; only `1` decodes.
    pub version: u8,
    /// The declared authority-qualified path.
    pub scope: SubnetRef,
    /// Topology epoch the declaration belongs to.
    pub topology_epoch: u32,
    /// Signing root.
    pub issuer: EntityId,
    /// Per-`(SubnetRef, kind)` ordering revision; replay/reorder safe.
    pub revision: u64,
    /// Advisory issue timestamp (unix seconds). A descriptor is
    /// superseded by revision, not by expiry.
    pub issued_at: u64,
    /// ed25519 over [`SUBNET_DESCRIPTOR_SIG_DOMAIN`] ‖ payload.
    pub signature: [u8; 64],
}

impl SubnetDescriptor {
    /// version 1 + authority 32 + path 4 + epoch 4 + issuer 32 +
    /// revision 8 + issued_at 8.
    pub const SIGNED_PAYLOAD_SIZE: usize = 89;
    /// Payload + 64-byte signature.
    pub const WIRE_SIZE: usize = Self::SIGNED_PAYLOAD_SIZE + 64;
    const SIGNING_INPUT_SIZE: usize =
        SUBNET_DESCRIPTOR_SIG_DOMAIN.len() + Self::SIGNED_PAYLOAD_SIZE;

    /// Issue signed by `root_keypair` (`issuer` is set from it).
    pub fn try_issue(
        root_keypair: &EntityKeypair,
        scope: SubnetRef,
        topology_epoch: u32,
        revision: u64,
        issued_at: u64,
    ) -> Result<Self, SubnetAuthError> {
        let mut fact = Self {
            version: 1,
            scope,
            topology_epoch,
            issuer: root_keypair.entity_id().clone(),
            revision,
            issued_at,
            signature: [0u8; 64],
        };
        let sig = root_keypair
            .try_sign(&fact.signing_input())
            .map_err(|_| SubnetAuthError::InvalidSignature)?;
        fact.signature = sig.to_bytes();
        Ok(fact)
    }

    fn signed_payload(&self) -> [u8; Self::SIGNED_PAYLOAD_SIZE] {
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
        buf[off..off + 8].copy_from_slice(&self.revision.to_le_bytes());
        off += 8;
        buf[off..off + 8].copy_from_slice(&self.issued_at.to_le_bytes());
        buf
    }

    fn signing_input(&self) -> [u8; Self::SIGNING_INPUT_SIZE] {
        let mut buf = [0u8; Self::SIGNING_INPUT_SIZE];
        buf[..SUBNET_DESCRIPTOR_SIG_DOMAIN.len()].copy_from_slice(SUBNET_DESCRIPTOR_SIG_DOMAIN);
        buf[SUBNET_DESCRIPTOR_SIG_DOMAIN.len()..].copy_from_slice(&self.signed_payload());
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
        let revision = read_u64(bytes, &mut off);
        let issued_at = read_u64(bytes, &mut off);
        let mut signature = [0u8; 64];
        signature.copy_from_slice(&bytes[off..off + 64]);
        Ok(Self {
            version,
            scope: SubnetRef { authority, path },
            topology_epoch,
            issuer,
            revision,
            issued_at,
            signature,
        })
    }

    /// Signature verification against `self.issuer` (whether that
    /// issuer is a configured root is the store's decision).
    pub fn verify(&self) -> Result<(), SubnetAuthError> {
        let sig = Signature::from_bytes(&self.signature);
        self.issuer
            .verify(&self.signing_input(), &sig)
            .map_err(|_| SubnetAuthError::InvalidSignature)
    }
}

// ---------------------------------------------------------------------------
// GatewayAdvertisement
// ---------------------------------------------------------------------------

/// Root-signed advertisement that one entity serves as a gateway for
/// a subtree.
///
/// An advertisement is DISCOVERY, not authority: it tells members
/// where a gateway is, and nothing more. The advertised entity still
/// proves its own forwarding rights from self-held credentials
/// (`install_subnet_gateway_credentials`, D6) — an advertisement for
/// an entity holding no `ROUTE`/`EXPORT` grant advertises a gateway
/// that can forward nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayAdvertisement {
    /// Wire version; only `1` decodes.
    pub version: u8,
    /// Subtree the gateway serves.
    pub scope: SubnetRef,
    /// Topology epoch the advertisement belongs to.
    pub topology_epoch: u32,
    /// Signing root.
    pub issuer: EntityId,
    /// The advertised gateway's full entity identity.
    pub gateway: EntityId,
    /// The advertised gateway's routing id (derived `NodeId`). A
    /// convenience for reaching it; never an identity claim — the
    /// full `EntityId` above is the identity.
    pub gateway_node: u64,
    /// Per-`(SubnetRef, kind)` ordering revision; replay/reorder safe.
    pub revision: u64,
    /// Validity window start (unix seconds).
    pub not_before: u64,
    /// Validity window end (unix seconds, exclusive).
    pub not_after: u64,
    /// ed25519 over [`SUBNET_GATEWAY_AD_SIG_DOMAIN`] ‖ payload.
    pub signature: [u8; 64],
}

impl GatewayAdvertisement {
    /// version 1 + authority 32 + path 4 + epoch 4 + issuer 32 +
    /// gateway 32 + gateway_node 8 + revision 8 + not_before 8 +
    /// not_after 8.
    pub const SIGNED_PAYLOAD_SIZE: usize = 137;
    /// Payload + 64-byte signature.
    pub const WIRE_SIZE: usize = Self::SIGNED_PAYLOAD_SIZE + 64;
    const SIGNING_INPUT_SIZE: usize =
        SUBNET_GATEWAY_AD_SIG_DOMAIN.len() + Self::SIGNED_PAYLOAD_SIZE;

    /// Issue signed by `root_keypair` (`issuer` is set from it).
    /// `not_after <= not_before` is refused at issue as at decode.
    #[expect(
        clippy::too_many_arguments,
        reason = "explicit wire fields; a params struct would only rename them"
    )]
    pub fn try_issue(
        root_keypair: &EntityKeypair,
        scope: SubnetRef,
        topology_epoch: u32,
        gateway: EntityId,
        gateway_node: u64,
        revision: u64,
        not_before: u64,
        not_after: u64,
    ) -> Result<Self, SubnetAuthError> {
        if not_after <= not_before {
            return Err(SubnetAuthError::InvalidValidityWindow);
        }
        let mut fact = Self {
            version: 1,
            scope,
            topology_epoch,
            issuer: root_keypair.entity_id().clone(),
            gateway,
            gateway_node,
            revision,
            not_before,
            not_after,
            signature: [0u8; 64],
        };
        let sig = root_keypair
            .try_sign(&fact.signing_input())
            .map_err(|_| SubnetAuthError::InvalidSignature)?;
        fact.signature = sig.to_bytes();
        Ok(fact)
    }

    fn signed_payload(&self) -> [u8; Self::SIGNED_PAYLOAD_SIZE] {
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
        buf[off..off + 32].copy_from_slice(self.gateway.as_bytes());
        off += 32;
        buf[off..off + 8].copy_from_slice(&self.gateway_node.to_le_bytes());
        off += 8;
        buf[off..off + 8].copy_from_slice(&self.revision.to_le_bytes());
        off += 8;
        buf[off..off + 8].copy_from_slice(&self.not_before.to_le_bytes());
        off += 8;
        buf[off..off + 8].copy_from_slice(&self.not_after.to_le_bytes());
        buf
    }

    fn signing_input(&self) -> [u8; Self::SIGNING_INPUT_SIZE] {
        let mut buf = [0u8; Self::SIGNING_INPUT_SIZE];
        buf[..SUBNET_GATEWAY_AD_SIG_DOMAIN.len()].copy_from_slice(SUBNET_GATEWAY_AD_SIG_DOMAIN);
        buf[SUBNET_GATEWAY_AD_SIG_DOMAIN.len()..].copy_from_slice(&self.signed_payload());
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
        let gateway = EntityId::from_bytes(read_32(bytes, &mut off));
        let gateway_node = read_u64(bytes, &mut off);
        let revision = read_u64(bytes, &mut off);
        let not_before = read_u64(bytes, &mut off);
        let not_after = read_u64(bytes, &mut off);
        if not_after <= not_before {
            return Err(SubnetAuthError::InvalidValidityWindow);
        }
        let mut signature = [0u8; 64];
        signature.copy_from_slice(&bytes[off..off + 64]);
        Ok(Self {
            version,
            scope: SubnetRef { authority, path },
            topology_epoch,
            issuer,
            gateway,
            gateway_node,
            revision,
            not_before,
            not_after,
            signature,
        })
    }

    /// Signature verification against `self.issuer`.
    pub fn verify(&self) -> Result<(), SubnetAuthError> {
        let sig = Signature::from_bytes(&self.signature);
        self.issuer
            .verify(&self.signing_input(), &sig)
            .map_err(|_| SubnetAuthError::InvalidSignature)
    }

    /// Window check with saturating skew, the family discipline.
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
// SubnetExportPolicy
// ---------------------------------------------------------------------------

/// Root-signed statement of EXACTLY which channels are exported at a
/// subtree's boundary.
///
/// The set replaces wholesale — like a gateway credential set — so a
/// revoked export cannot survive inside a merged remainder. An empty
/// set is meaningful: "nothing is exported here".
///
/// Like the advertisement, this is policy DISTRIBUTION, not export
/// authority: the boundary gateway still needs its own `EXPORT`
/// credential at the boundary scope (D6). The fact tells a gateway
/// what the authority wants exported; the credential is what lets it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubnetExportPolicy {
    /// Wire version; only `1` decodes.
    pub version: u8,
    /// The boundary subtree the policy applies to.
    pub scope: SubnetRef,
    /// Topology epoch the policy belongs to.
    pub topology_epoch: u32,
    /// Signing root.
    pub issuer: EntityId,
    /// Canonical 64-bit channel hashes exported at this boundary.
    /// At most [`MAX_EXPORTED_CHANNELS`]; order is not significant
    /// but IS signed, so decode preserves it.
    pub exported_channels: Vec<ChannelHash>,
    /// Per-`(SubnetRef, kind)` ordering revision; replay/reorder safe.
    pub revision: u64,
    /// Validity window start (unix seconds).
    pub not_before: u64,
    /// Validity window end (unix seconds, exclusive).
    pub not_after: u64,
    /// ed25519 over [`SUBNET_EXPORT_POLICY_SIG_DOMAIN`] ‖ payload.
    pub signature: [u8; 64],
}

impl SubnetExportPolicy {
    /// version 1 + authority 32 + path 4 + epoch 4 + issuer 32 +
    /// count 1, before the per-channel hashes and the fixed tail.
    const FIXED_HEAD_SIZE: usize = 74;
    /// revision 8 + not_before 8 + not_after 8.
    const FIXED_TAIL_SIZE: usize = 24;

    /// Wire size for a policy naming `count` channels.
    pub const fn wire_size(count: usize) -> usize {
        Self::FIXED_HEAD_SIZE + count * 8 + Self::FIXED_TAIL_SIZE + 64
    }

    /// Issue signed by `root_keypair` (`issuer` is set from it).
    pub fn try_issue(
        root_keypair: &EntityKeypair,
        scope: SubnetRef,
        topology_epoch: u32,
        exported_channels: Vec<ChannelHash>,
        revision: u64,
        not_before: u64,
        not_after: u64,
    ) -> Result<Self, SubnetAuthError> {
        if exported_channels.len() > MAX_EXPORTED_CHANNELS {
            return Err(SubnetAuthError::InvalidFormat);
        }
        if not_after <= not_before {
            return Err(SubnetAuthError::InvalidValidityWindow);
        }
        let mut fact = Self {
            version: 1,
            scope,
            topology_epoch,
            issuer: root_keypair.entity_id().clone(),
            exported_channels,
            revision,
            not_before,
            not_after,
            signature: [0u8; 64],
        };
        let sig = root_keypair
            .try_sign(&fact.signing_input())
            .map_err(|_| SubnetAuthError::InvalidSignature)?;
        fact.signature = sig.to_bytes();
        Ok(fact)
    }

    /// Variable-width payload (the channel list is length-prefixed by
    /// a single strict count byte).
    fn signed_payload(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(Self::wire_size(self.exported_channels.len()) - 64);
        buf.push(self.version);
        buf.extend_from_slice(self.scope.authority.as_bytes());
        buf.extend_from_slice(&self.scope.path.raw().to_le_bytes());
        buf.extend_from_slice(&self.topology_epoch.to_le_bytes());
        buf.extend_from_slice(self.issuer.as_bytes());
        buf.push(self.exported_channels.len() as u8);
        for hash in &self.exported_channels {
            buf.extend_from_slice(&hash.to_le_bytes());
        }
        buf.extend_from_slice(&self.revision.to_le_bytes());
        buf.extend_from_slice(&self.not_before.to_le_bytes());
        buf.extend_from_slice(&self.not_after.to_le_bytes());
        buf
    }

    fn signing_input(&self) -> Vec<u8> {
        let payload = self.signed_payload();
        let mut buf = Vec::with_capacity(SUBNET_EXPORT_POLICY_SIG_DOMAIN.len() + payload.len());
        buf.extend_from_slice(SUBNET_EXPORT_POLICY_SIG_DOMAIN);
        buf.extend_from_slice(&payload);
        buf
    }

    /// Wire form: payload ‖ signature.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = self.signed_payload();
        out.extend_from_slice(&self.signature);
        out
    }

    /// Strict decode; signature NOT verified here. The count byte
    /// must match the exact remaining length — a count that disagrees
    /// with the buffer is a forgery attempt or corruption either way.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, SubnetAuthError> {
        if bytes.len() < Self::wire_size(0) {
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
        let count = bytes[off] as usize;
        off += 1;
        if count > MAX_EXPORTED_CHANNELS || bytes.len() != Self::wire_size(count) {
            return Err(SubnetAuthError::InvalidFormat);
        }
        let mut exported_channels = Vec::with_capacity(count);
        for _ in 0..count {
            exported_channels.push(read_u64(bytes, &mut off));
        }
        let revision = read_u64(bytes, &mut off);
        let not_before = read_u64(bytes, &mut off);
        let not_after = read_u64(bytes, &mut off);
        if not_after <= not_before {
            return Err(SubnetAuthError::InvalidValidityWindow);
        }
        let mut signature = [0u8; 64];
        signature.copy_from_slice(&bytes[off..off + 64]);
        Ok(Self {
            version,
            scope: SubnetRef { authority, path },
            topology_epoch,
            issuer,
            exported_channels,
            revision,
            not_before,
            not_after,
            signature,
        })
    }

    /// Signature verification against `self.issuer`.
    pub fn verify(&self) -> Result<(), SubnetAuthError> {
        let sig = Signature::from_bytes(&self.signature);
        self.issuer
            .verify(&self.signing_input(), &sig)
            .map_err(|_| SubnetAuthError::InvalidSignature)
    }

    /// Window check with saturating skew, the family discipline.
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
// The tagged wire envelope
// ---------------------------------------------------------------------------

/// One decoded control fact, any kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubnetControlFact {
    /// A [`SubnetDescriptor`].
    Descriptor(SubnetDescriptor),
    /// A [`GatewayAdvertisement`].
    GatewayAdvertisement(GatewayAdvertisement),
    /// A [`SubnetExportPolicy`].
    ExportPolicy(SubnetExportPolicy),
    /// A [`SubnetRevocationFloor`] — same bytes and domain as the S2
    /// artifact, so distribution and local provisioning verify
    /// identically.
    RevocationFloor(SubnetRevocationFloor),
}

impl SubnetControlFact {
    /// This fact's wire kind.
    pub fn kind(&self) -> SubnetFactKind {
        match self {
            Self::Descriptor(_) => SubnetFactKind::Descriptor,
            Self::GatewayAdvertisement(_) => SubnetFactKind::GatewayAdvertisement,
            Self::ExportPolicy(_) => SubnetFactKind::ExportPolicy,
            Self::RevocationFloor(_) => SubnetFactKind::RevocationFloor,
        }
    }

    /// The fact's authority-qualified scope.
    pub fn scope(&self) -> &SubnetRef {
        match self {
            Self::Descriptor(f) => &f.scope,
            Self::GatewayAdvertisement(f) => &f.scope,
            Self::ExportPolicy(f) => &f.scope,
            Self::RevocationFloor(f) => &f.scope,
        }
    }

    /// Wire form: one kind tag byte ‖ the fact's own wire bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let body = match self {
            Self::Descriptor(f) => f.to_bytes(),
            Self::GatewayAdvertisement(f) => f.to_bytes(),
            Self::ExportPolicy(f) => f.to_bytes(),
            Self::RevocationFloor(f) => f.to_bytes(),
        };
        let mut out = Vec::with_capacity(1 + body.len());
        out.push(self.kind() as u8);
        out.extend_from_slice(&body);
        out
    }

    /// Strict decode: unknown tag, wrong body length, or a malformed
    /// body all fail closed. Signatures are NOT verified here.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, SubnetAuthError> {
        let (&tag, body) = bytes.split_first().ok_or(SubnetAuthError::InvalidFormat)?;
        match SubnetFactKind::try_from_tag(tag)? {
            SubnetFactKind::Descriptor => SubnetDescriptor::from_bytes(body).map(Self::Descriptor),
            SubnetFactKind::GatewayAdvertisement => {
                GatewayAdvertisement::from_bytes(body).map(Self::GatewayAdvertisement)
            }
            SubnetFactKind::ExportPolicy => {
                SubnetExportPolicy::from_bytes(body).map(Self::ExportPolicy)
            }
            SubnetFactKind::RevocationFloor => {
                SubnetRevocationFloor::from_bytes(body).map(Self::RevocationFloor)
            }
        }
    }
}

/// What applying one control fact did — the kind it decoded to, and
/// whether state changed (`false` = the designed replay/reorder
/// no-op).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubnetControlOutcome {
    /// The decoded fact kind.
    pub kind: SubnetFactKind,
    /// Whether any state changed.
    pub applied: bool,
}

// ---------------------------------------------------------------------------
// The monotonic store
// ---------------------------------------------------------------------------

/// Key: (authority bytes, topology epoch, path). Facts for different
/// epochs never interact — an epoch is a reinterpretation boundary
/// (D1), so revision streams restart cleanly on the far side of one
/// and a lagging node's facts for a future epoch sit inert until it
/// advances.
type FactKey = ([u8; 32], u32, u32);

/// Verified control-fact state, applied monotonically by revision per
/// `(SubnetRef, fact kind)`.
///
/// Floors are NOT held here — they flow into the S2 floor registry.
/// This store carries the three descriptive kinds, each in its own
/// map so their revision streams cannot interact: a newer gateway
/// advertisement can never suppress a current export policy.
#[derive(Debug, Default)]
pub struct SubnetControlStore {
    descriptors: DashMap<FactKey, SubnetDescriptor>,
    gateways: DashMap<FactKey, GatewayAdvertisement>,
    exports: DashMap<FactKey, SubnetExportPolicy>,
}

impl SubnetControlStore {
    /// Empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Verify and apply one non-floor fact under `config`'s trust.
    ///
    /// The checks are the floor-registry discipline, applied
    /// uniformly regardless of how the bytes arrived:
    ///
    /// 1. the fact's authority equals the config's ([`SubnetAuthError::WrongAuthority`]);
    /// 2. the config anchors at least one root ([`SubnetAuthError::UnknownAuthority`]);
    /// 3. the issuer is a configured root ([`SubnetAuthError::IssuerNotAuthorized`])
    ///    — channel membership, publication, or any other arrival
    ///    privilege establishes nothing here;
    /// 4. the domain-separated signature verifies;
    /// 5. kinds with a validity window are inside it (with skew);
    /// 6. the revision strictly exceeds the stored one for this
    ///    `(SubnetRef, kind)` — otherwise `Ok(false)`: a replayed or
    ///    reordered fact is a no-op, never a rollback.
    ///
    /// Returns `Ok(true)` iff state changed. Floors are refused here
    /// ([`SubnetAuthError::InvalidFormat`]) — route them through the
    /// floor registry, which owns revocation ordering and the auth
    /// epoch.
    pub fn apply(
        &self,
        fact: &SubnetControlFact,
        config: &SubnetAuthorityConfig,
        now: u64,
        skew_secs: u64,
    ) -> Result<bool, SubnetAuthError> {
        if fact.scope().authority != config.authority {
            return Err(SubnetAuthError::WrongAuthority);
        }
        if config.roots.is_empty() {
            return Err(SubnetAuthError::UnknownAuthority);
        }
        match fact {
            SubnetControlFact::Descriptor(f) => {
                if !config.roots.contains(&f.issuer) {
                    return Err(SubnetAuthError::IssuerNotAuthorized);
                }
                f.verify()?;
                Ok(Self::apply_monotonic(
                    &self.descriptors,
                    key_of(&f.scope, f.topology_epoch),
                    f,
                    |s| s.revision,
                ))
            }
            SubnetControlFact::GatewayAdvertisement(f) => {
                if !config.roots.contains(&f.issuer) {
                    return Err(SubnetAuthError::IssuerNotAuthorized);
                }
                f.verify()?;
                f.check_time_bounds_at(now, skew_secs)?;
                Ok(Self::apply_monotonic(
                    &self.gateways,
                    key_of(&f.scope, f.topology_epoch),
                    f,
                    |s| s.revision,
                ))
            }
            SubnetControlFact::ExportPolicy(f) => {
                if !config.roots.contains(&f.issuer) {
                    return Err(SubnetAuthError::IssuerNotAuthorized);
                }
                f.verify()?;
                f.check_time_bounds_at(now, skew_secs)?;
                Ok(Self::apply_monotonic(
                    &self.exports,
                    key_of(&f.scope, f.topology_epoch),
                    f,
                    |s| s.revision,
                ))
            }
            SubnetControlFact::RevocationFloor(_) => Err(SubnetAuthError::InvalidFormat),
        }
    }

    /// The one write shape: install iff vacant or strictly newer by
    /// revision, under the entry guard, so two concurrent arrivals
    /// cannot interleave a rollback.
    fn apply_monotonic<T: Clone>(
        map: &DashMap<FactKey, T>,
        key: FactKey,
        fact: &T,
        revision_of: impl Fn(&T) -> u64,
    ) -> bool {
        let mut changed = false;
        map.entry(key)
            .and_modify(|stored| {
                if revision_of(fact) > revision_of(stored) {
                    *stored = fact.clone();
                    changed = true;
                }
            })
            .or_insert_with(|| {
                changed = true;
                fact.clone()
            });
        changed
    }

    /// The current descriptor for a scope under an epoch.
    pub fn descriptor_for(
        &self,
        authority: &EntityId,
        topology_epoch: u32,
        path: TopologySubnetId,
    ) -> Option<SubnetDescriptor> {
        self.descriptors
            .get(&(*authority.as_bytes(), topology_epoch, path.raw()))
            .map(|e| e.clone())
    }

    /// The current, unexpired gateway advertisement for a scope under
    /// an epoch. Expiry is enforced at read as well as at apply: an
    /// advertisement that aged out while stored stops being served
    /// without needing a tombstoning write.
    pub fn gateway_for(
        &self,
        authority: &EntityId,
        topology_epoch: u32,
        path: TopologySubnetId,
        now: u64,
        skew_secs: u64,
    ) -> Option<GatewayAdvertisement> {
        self.gateways
            .get(&(*authority.as_bytes(), topology_epoch, path.raw()))
            .filter(|e| e.check_time_bounds_at(now, skew_secs).is_ok())
            .map(|e| e.clone())
    }

    /// The current, unexpired export policy for a scope under an
    /// epoch.
    pub fn export_policy_for(
        &self,
        authority: &EntityId,
        topology_epoch: u32,
        path: TopologySubnetId,
        now: u64,
        skew_secs: u64,
    ) -> Option<SubnetExportPolicy> {
        self.exports
            .get(&(*authority.as_bytes(), topology_epoch, path.raw()))
            .filter(|e| e.check_time_bounds_at(now, skew_secs).is_ok())
            .map(|e| e.clone())
    }

    /// Drop facts for epochs BELOW `current_epoch` — they can never
    /// be read again (reads are epoch-exact and epochs only advance).
    /// Facts for the current or a future epoch are kept: a lagging
    /// node may hold facts it cannot yet see.
    pub fn purge_stale_epochs(&self, current_epoch: u32) -> usize {
        let before = self.descriptors.len() + self.gateways.len() + self.exports.len();
        self.descriptors.retain(|k, _| k.1 >= current_epoch);
        self.gateways.retain(|k, _| k.1 >= current_epoch);
        self.exports.retain(|k, _| k.1 >= current_epoch);
        before - (self.descriptors.len() + self.gateways.len() + self.exports.len())
    }
}

fn key_of(scope: &SubnetRef, topology_epoch: u32) -> FactKey {
    (
        *scope.authority.as_bytes(),
        topology_epoch,
        scope.path.raw(),
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn root() -> EntityKeypair {
        EntityKeypair::generate()
    }

    fn scope_of(authority: &EntityKeypair, path: u32) -> SubnetRef {
        SubnetRef {
            authority: authority.entity_id().clone(),
            path: TopologySubnetId::from_raw(path),
        }
    }

    fn config_of(authority: &EntityKeypair, roots: &[&EntityKeypair]) -> SubnetAuthorityConfig {
        SubnetAuthorityConfig {
            authority: authority.entity_id().clone(),
            roots: roots.iter().map(|r| r.entity_id().clone()).collect(),
            maximum_grant_lifetime_secs: 3600,
        }
    }

    const NOW: u64 = 1_700_000_000;
    const SKEW: u64 = 30;

    fn descriptor(root: &EntityKeypair, path: u32, revision: u64) -> SubnetControlFact {
        SubnetControlFact::Descriptor(
            SubnetDescriptor::try_issue(root, scope_of(root, path), 1, revision, NOW).unwrap(),
        )
    }

    fn gateway_ad(root: &EntityKeypair, path: u32, revision: u64) -> SubnetControlFact {
        SubnetControlFact::GatewayAdvertisement(
            GatewayAdvertisement::try_issue(
                root,
                scope_of(root, path),
                1,
                EntityKeypair::generate().entity_id().clone(),
                0xBEEF,
                revision,
                NOW - 10,
                NOW + 3600,
            )
            .unwrap(),
        )
    }

    fn export_policy(
        root: &EntityKeypair,
        path: u32,
        revision: u64,
        channels: Vec<ChannelHash>,
    ) -> SubnetControlFact {
        SubnetControlFact::ExportPolicy(
            SubnetExportPolicy::try_issue(
                root,
                scope_of(root, path),
                1,
                channels,
                revision,
                NOW - 10,
                NOW + 3600,
            )
            .unwrap(),
        )
    }

    #[test]
    fn every_kind_round_trips_through_the_tagged_wire() {
        let root = root();
        let facts = [
            descriptor(&root, 0x0101, 7),
            gateway_ad(&root, 0x0101, 7),
            export_policy(&root, 0x0101, 7, vec![0xAAAA, 0xBBBB]),
            SubnetControlFact::RevocationFloor(
                SubnetRevocationFloor::try_issue(&root, scope_of(&root, 0x0101), 1, 3, 7, NOW)
                    .unwrap(),
            ),
        ];
        for fact in &facts {
            let bytes = fact.to_bytes();
            let decoded = SubnetControlFact::from_bytes(&bytes).unwrap();
            assert_eq!(&decoded, fact);
        }
    }

    #[test]
    fn unknown_tags_versions_and_lengths_fail_closed() {
        let root = root();
        let good = descriptor(&root, 1, 1).to_bytes();

        // Unknown kind tag.
        let mut bad_tag = good.clone();
        bad_tag[0] = 9;
        assert_eq!(
            SubnetControlFact::from_bytes(&bad_tag),
            Err(SubnetAuthError::InvalidFormat)
        );
        // Unknown version inside the body.
        let mut bad_version = good.clone();
        bad_version[1] = 2;
        assert_eq!(
            SubnetControlFact::from_bytes(&bad_version),
            Err(SubnetAuthError::InvalidFormat)
        );
        // Truncation and trailing bytes.
        assert_eq!(
            SubnetControlFact::from_bytes(&good[..good.len() - 1]),
            Err(SubnetAuthError::InvalidFormat)
        );
        let mut trailing = good.clone();
        trailing.push(0);
        assert_eq!(
            SubnetControlFact::from_bytes(&trailing),
            Err(SubnetAuthError::InvalidFormat)
        );
        // Empty input.
        assert_eq!(
            SubnetControlFact::from_bytes(&[]),
            Err(SubnetAuthError::InvalidFormat)
        );
    }

    #[test]
    fn an_export_count_disagreeing_with_the_buffer_fails_closed() {
        let root = root();
        let bytes = export_policy(&root, 1, 1, vec![0xAAAA, 0xBBBB]).to_bytes();
        // Lower the count byte: total length no longer matches.
        let mut shrunk = bytes.clone();
        shrunk[1 + SubnetExportPolicy::FIXED_HEAD_SIZE - 1] = 1;
        assert_eq!(
            SubnetControlFact::from_bytes(&shrunk),
            Err(SubnetAuthError::InvalidFormat)
        );
        // A count beyond the ceiling.
        let mut oversized = bytes;
        oversized[1 + SubnetExportPolicy::FIXED_HEAD_SIZE - 1] = (MAX_EXPORTED_CHANNELS + 1) as u8;
        assert_eq!(
            SubnetControlFact::from_bytes(&oversized),
            Err(SubnetAuthError::InvalidFormat)
        );
    }

    #[test]
    fn an_unsigned_or_tampered_fact_changes_no_state() {
        let root = root();
        let store = SubnetControlStore::new();
        let config = config_of(&root, &[&root]);

        // Zeroed signature.
        let SubnetControlFact::Descriptor(mut plain) = descriptor(&root, 1, 1) else {
            unreachable!()
        };
        plain.signature = [0u8; 64];
        assert_eq!(
            store.apply(&SubnetControlFact::Descriptor(plain), &config, NOW, SKEW),
            Err(SubnetAuthError::InvalidSignature)
        );

        // Payload tampered after signing (revision inflated).
        let SubnetControlFact::Descriptor(mut tampered) = descriptor(&root, 1, 1) else {
            unreachable!()
        };
        tampered.revision = 99;
        assert_eq!(
            store.apply(&SubnetControlFact::Descriptor(tampered), &config, NOW, SKEW),
            Err(SubnetAuthError::InvalidSignature)
        );

        assert!(store
            .descriptor_for(&config.authority, 1, TopologySubnetId::from_raw(1))
            .is_none());
    }

    #[test]
    fn a_wrong_authority_or_non_root_issuer_is_inert() {
        let root_a = root();
        let root_b = root();
        let store = SubnetControlStore::new();
        let config_a = config_of(&root_a, &[&root_a]);

        // Authority B's fact against authority A's config.
        assert_eq!(
            store.apply(&descriptor(&root_b, 1, 1), &config_a, NOW, SKEW),
            Err(SubnetAuthError::WrongAuthority)
        );

        // Correct authority, but the issuer is not a configured root:
        // a valid signature by ANYONE ELSE — however privileged on
        // the arrival path — establishes nothing.
        let outsider = root();
        let fact = SubnetDescriptor::try_issue(&outsider, scope_of(&root_a, 1), 1, 1, NOW).unwrap();
        assert_eq!(
            store.apply(&SubnetControlFact::Descriptor(fact), &config_a, NOW, SKEW),
            Err(SubnetAuthError::IssuerNotAuthorized)
        );

        // Empty roots fail closed even for the authority's own fact.
        let empty = SubnetAuthorityConfig {
            authority: root_a.entity_id().clone(),
            roots: vec![],
            maximum_grant_lifetime_secs: 3600,
        };
        assert_eq!(
            store.apply(&descriptor(&root_a, 1, 1), &empty, NOW, SKEW),
            Err(SubnetAuthError::UnknownAuthority)
        );

        assert!(store
            .descriptor_for(root_a.entity_id(), 1, TopologySubnetId::from_raw(1))
            .is_none());
    }

    #[test]
    fn revisions_are_monotonic_per_scope_and_kind() {
        let root = root();
        let store = SubnetControlStore::new();
        let config = config_of(&root, &[&root]);

        assert!(store
            .apply(&descriptor(&root, 1, 5), &config, NOW, SKEW)
            .unwrap());
        // Replay and regression are no-ops, not errors and not writes.
        assert!(!store
            .apply(&descriptor(&root, 1, 5), &config, NOW, SKEW)
            .unwrap());
        assert!(!store
            .apply(&descriptor(&root, 1, 4), &config, NOW, SKEW)
            .unwrap());
        // Strictly newer applies.
        assert!(store
            .apply(&descriptor(&root, 1, 6), &config, NOW, SKEW)
            .unwrap());
        assert_eq!(
            store
                .descriptor_for(&config.authority, 1, TopologySubnetId::from_raw(1))
                .unwrap()
                .revision,
            6
        );
        // A different path is an independent stream.
        assert!(store
            .apply(&descriptor(&root, 2, 1), &config, NOW, SKEW)
            .unwrap());
    }

    #[test]
    fn a_newer_gateway_fact_does_not_suppress_an_export_policy() {
        let root = root();
        let store = SubnetControlStore::new();
        let config = config_of(&root, &[&root]);

        assert!(store
            .apply(
                &export_policy(&root, 1, 1, vec![0xAAAA]),
                &config,
                NOW,
                SKEW
            )
            .unwrap());
        // A gateway fact at a far higher revision, same scope.
        assert!(store
            .apply(&gateway_ad(&root, 1, 99), &config, NOW, SKEW)
            .unwrap());

        // The export policy is still served…
        let policy = store
            .export_policy_for(
                &config.authority,
                1,
                TopologySubnetId::from_raw(1),
                NOW,
                SKEW,
            )
            .unwrap();
        assert_eq!(policy.exported_channels, vec![0xAAAA]);
        // …and its OWN revision stream still advances from 1, not 99.
        assert!(store
            .apply(
                &export_policy(&root, 1, 2, vec![0xBBBB]),
                &config,
                NOW,
                SKEW
            )
            .unwrap());
    }

    #[test]
    fn replay_and_reorder_converge_to_max_revision_state() {
        let root = root();
        let config = config_of(&root, &[&root]);
        let facts = [
            descriptor(&root, 1, 3),
            descriptor(&root, 1, 1),
            descriptor(&root, 1, 2),
            gateway_ad(&root, 1, 2),
            gateway_ad(&root, 1, 1),
            export_policy(&root, 1, 2, vec![0xCC]),
            export_policy(&root, 1, 1, vec![0xDD]),
        ];
        // Two adversarial orders, then a full replay of everything.
        for order in [[0usize, 1, 2, 3, 4, 5, 6], [6, 5, 4, 3, 2, 1, 0]] {
            let store = SubnetControlStore::new();
            for &i in &order {
                let _ = store.apply(&facts[i], &config, NOW, SKEW).unwrap();
            }
            for &i in &order {
                assert!(
                    !store.apply(&facts[i], &config, NOW, SKEW).unwrap(),
                    "a full replay must change nothing"
                );
            }
            let path = TopologySubnetId::from_raw(1);
            assert_eq!(
                store
                    .descriptor_for(&config.authority, 1, path)
                    .unwrap()
                    .revision,
                3
            );
            assert_eq!(
                store
                    .gateway_for(&config.authority, 1, path, NOW, SKEW)
                    .unwrap()
                    .revision,
                2
            );
            assert_eq!(
                store
                    .export_policy_for(&config.authority, 1, path, NOW, SKEW)
                    .unwrap()
                    .exported_channels,
                vec![0xCC]
            );
        }
    }

    #[test]
    fn windowed_kinds_expire_at_read_and_refuse_at_apply() {
        let root = root();
        let store = SubnetControlStore::new();
        let config = config_of(&root, &[&root]);

        assert!(store
            .apply(&gateway_ad(&root, 1, 1), &config, NOW, SKEW)
            .unwrap());
        let path = TopologySubnetId::from_raw(1);
        assert!(store
            .gateway_for(&config.authority, 1, path, NOW, SKEW)
            .is_some());
        // Read after the window: served no longer.
        assert!(store
            .gateway_for(&config.authority, 1, path, NOW + 7200, SKEW)
            .is_none());
        // Apply outside the window: refused.
        assert_eq!(
            store.apply(&gateway_ad(&root, 2, 1), &config, NOW + 7200, SKEW),
            Err(SubnetAuthError::Expired)
        );
    }

    #[test]
    fn floors_are_routed_to_the_registry_not_stored_here() {
        let root = root();
        let store = SubnetControlStore::new();
        let config = config_of(&root, &[&root]);
        let floor = SubnetControlFact::RevocationFloor(
            SubnetRevocationFloor::try_issue(&root, scope_of(&root, 1), 1, 3, 1, NOW).unwrap(),
        );
        assert_eq!(
            store.apply(&floor, &config, NOW, SKEW),
            Err(SubnetAuthError::InvalidFormat),
            "the store must not become a second revocation authority"
        );
    }

    #[test]
    fn purging_stale_epochs_keeps_current_and_future_facts() {
        let root = root();
        let store = SubnetControlStore::new();
        let config = config_of(&root, &[&root]);

        let at_epoch = |epoch: u32, path: u32| {
            SubnetControlFact::Descriptor(
                SubnetDescriptor::try_issue(&root, scope_of(&root, path), epoch, 1, NOW).unwrap(),
            )
        };
        assert!(store.apply(&at_epoch(1, 1), &config, NOW, SKEW).unwrap());
        assert!(store.apply(&at_epoch(2, 2), &config, NOW, SKEW).unwrap());
        assert!(store.apply(&at_epoch(3, 3), &config, NOW, SKEW).unwrap());

        assert_eq!(store.purge_stale_epochs(2), 1);
        assert!(store
            .descriptor_for(&config.authority, 1, TopologySubnetId::from_raw(1))
            .is_none());
        assert!(store
            .descriptor_for(&config.authority, 2, TopologySubnetId::from_raw(2))
            .is_some());
        assert!(store
            .descriptor_for(&config.authority, 3, TopologySubnetId::from_raw(3))
            .is_some());
    }
}
