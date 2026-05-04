//! Origin binding for Net packets.
//!
//! The `OriginStamp` holds the entity identity and provides the cached
//! `origin_hash` value that gets written into every outbound packet header.
//! This is computed once at session creation — zero per-packet crypto.
//!
//! # Threat model — origin spoofability inside an authenticated channel
//!
//! `origin_hash` is written verbatim into the wire header
//! and protected only by the channel's AEAD seal. There is **no
//! per-packet signature** binding the payload to the originator's
//! keypair. Any peer with the session key (i.e., any node admitted
//! to the channel via the handshake) can mint packets claiming an
//! arbitrary `origin_hash` value.
//!
//! This is a **deliberate design trade-off**: per-packet signatures
//! would add ~64 bytes of overhead and a signature verification per
//! packet, both load-bearing on the "wire-speed forwarding" promise.
//! The mitigation is at the membership layer:
//!
//! - Channels can require capability tokens for join (`ChannelConfig::with_require_token`).
//! - Tokens are scoped + signed by the issuer.
//! - Once a peer is admitted, it's trusted to act under any
//!   `origin_hash` within the channel — including spoofing
//!   another channel member's origin.
//!
//! Callers needing **end-to-end origin authentication** must layer
//! a signed envelope inside the encrypted payload (e.g., via
//! `PermissionToken`'s signing primitives or an application-level
//! signature scheme). The bus deliberately does not enforce this.

use super::entity::{EntityId, EntityKeypair};

/// Cached origin binding for packet building.
///
/// Created once per session from the entity keypair. The `origin_hash`
/// is a truncated BLAKE2s of the entity's public key, suitable for
/// wire-speed filtering by forwarding nodes.
#[derive(Debug, Clone)]
pub struct OriginStamp {
    entity_id: EntityId,
    origin_hash: u64,
    node_id: u64,
}

impl OriginStamp {
    /// Create an origin stamp from an entity keypair.
    pub fn from_keypair(keypair: &EntityKeypair) -> Self {
        Self {
            entity_id: keypair.entity_id().clone(),
            origin_hash: keypair.origin_hash(),
            node_id: keypair.node_id(),
        }
    }

    /// Create an origin stamp from an entity ID (no signing capability).
    pub fn from_entity_id(entity_id: EntityId) -> Self {
        let origin_hash = entity_id.origin_hash();
        let node_id = entity_id.node_id();
        Self {
            entity_id,
            origin_hash,
            node_id,
        }
    }

    /// Get the full 8-byte origin hash for application-layer
    /// accounting. The per-packet `NetHeader::origin_hash` (still
    /// 4 bytes) downcasts via `as u32` — the low 32 bits are
    /// what feed routing.
    #[inline]
    pub fn origin_hash(&self) -> u64 {
        self.origin_hash
    }

    /// Get the node ID for swarm/routing (8 bytes).
    #[inline]
    pub fn node_id(&self) -> u64 {
        self.node_id
    }

    /// Get the full entity identity.
    #[inline]
    pub fn entity_id(&self) -> &EntityId {
        &self.entity_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_origin_stamp_from_keypair() {
        let kp = EntityKeypair::generate();
        let stamp = OriginStamp::from_keypair(&kp);

        assert_eq!(stamp.origin_hash(), kp.origin_hash());
        assert_eq!(stamp.node_id(), kp.node_id());
        assert_eq!(stamp.entity_id(), kp.entity_id());
    }

    #[test]
    fn test_origin_stamp_from_entity_id() {
        let kp = EntityKeypair::generate();
        let stamp = OriginStamp::from_entity_id(kp.entity_id().clone());

        assert_eq!(stamp.origin_hash(), kp.origin_hash());
        assert_eq!(stamp.node_id(), kp.node_id());
    }
}
