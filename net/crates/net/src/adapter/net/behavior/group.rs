//! Capability group identifier — capability-auth-plan Phase 1.
//!
//! A `GroupId` is a 32-byte opaque identifier for an operator-
//! defined named collection of peers. Mirrors [`super::subnet::SubnetId`]
//! one-for-one but at 32 bytes (the wider value-as-secret space
//! lets operators use a random `GroupId` that's effectively
//! unguessable, matching the substrate's channel-auth-token
//! pattern).
//!
//! # Membership
//!
//! Peers self-declare group membership via `group:<hex64>` tags on
//! their own [`CapabilityAnnouncement`](super::capability::CapabilityAnnouncement).
//! A peer may emit multiple group tags to claim membership in
//! multiple groups. The capability index parses every group tag
//! and stores the `NodeId → Vec<GroupId>` mapping on the peer view.
//!
//! Self-declaration is safe in the same sense as
//! [`super::subnet::SubnetId`]: the announcement is signed +
//! TOFU-bound to the entity's ed25519 key, so a peer can only
//! claim membership for itself. Group ids that act as secrets
//! (random 32 bytes) prevent unauthorised claims; group ids that
//! are public (e.g. blake2s-of-name) accept any claimant and are
//! suitable for advisory routing rather than strict gating.
//!
//! This is a separate concept from the compute-layer
//! `replica_group` / `standby_group` — those are about replica
//! placement, this is about access control. No relationship.

use serde::{Deserialize, Serialize};

/// Wire-format tag prefix for self-declared group membership.
/// Operators emit `group:<64-hex-char>` as a capability tag on
/// their announcement; the substrate parses it via
/// [`GroupId::from_tag`] at fold time.
pub const GROUP_TAG_PREFIX: &str = "group:";

/// 32-byte stable group identifier. Opaque to the substrate.
/// Operators choose the value; values that double as secrets
/// (random 32 bytes) prevent unauthorised membership claims.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GroupId(pub [u8; 32]);

impl GroupId {
    /// Construct from raw bytes.
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Borrow the 32-byte representation.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Parse a `group:<hex64>` capability-tag value into a
    /// `GroupId`. Returns `None` on missing prefix, wrong hex
    /// length (must be exactly 64 chars), or non-hex characters.
    pub fn from_tag(tag: &str) -> Option<Self> {
        let hex_part = tag.strip_prefix(GROUP_TAG_PREFIX)?;
        if hex_part.len() != 64 {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, chunk) in hex_part.as_bytes().chunks(2).enumerate() {
            let hi = hex_nibble(chunk[0])?;
            let lo = hex_nibble(chunk[1])?;
            out[i] = (hi << 4) | lo;
        }
        Some(Self(out))
    }

    /// Render as the canonical `group:<hex64>` tag form.
    pub fn to_tag(self) -> String {
        let mut s = String::with_capacity(GROUP_TAG_PREFIX.len() + 64);
        s.push_str(GROUP_TAG_PREFIX);
        for b in &self.0 {
            use std::fmt::Write;
            let _ = write!(s, "{:02x}", b);
        }
        s
    }
}

impl std::fmt::Display for GroupId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for b in &self.0 {
            write!(f, "{:02x}", b)?;
        }
        Ok(())
    }
}

fn hex_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_via_tag_form() {
        let original = GroupId([0x5A; 32]);
        let tag = original.to_tag();
        assert_eq!(
            tag,
            "group:5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a"
        );
        let decoded = GroupId::from_tag(&tag).expect("round trip");
        assert_eq!(decoded, original);
    }

    #[test]
    fn parse_rejects_missing_prefix() {
        let no_prefix = "5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a";
        assert!(GroupId::from_tag(no_prefix).is_none());
    }

    #[test]
    fn parse_rejects_wrong_length() {
        // 63 hex chars instead of 64.
        let short = "group:5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5";
        assert!(GroupId::from_tag(short).is_none());
        // 65 hex chars.
        let long = "group:5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5aa";
        assert!(GroupId::from_tag(long).is_none());
    }

    #[test]
    fn parse_rejects_non_hex_chars() {
        let bad = "group:zz5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a";
        assert!(GroupId::from_tag(bad).is_none());
    }

    #[test]
    fn distinct_groups_differ() {
        let a = GroupId([0x11; 32]);
        let b = GroupId([0x22; 32]);
        assert_ne!(a, b);
    }

    #[test]
    fn serde_round_trip_postcard() {
        let g = GroupId([0xAA; 32]);
        let bytes = postcard::to_allocvec(&g).unwrap();
        let decoded: GroupId = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, g);
    }
}
