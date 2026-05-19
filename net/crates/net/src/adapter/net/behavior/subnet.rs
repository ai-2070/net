//! Subnet identifier — capability-auth-plan Phase 1.
//!
//! A `SubnetId` is a 16-byte opaque identifier for a topology
//! partition. Operators pick the value (random 16 bytes, or a
//! blake2s-of-name truncated to 16, or any operator-stable
//! convention); the substrate doesn't interpret the bytes.
//!
//! # Membership
//!
//! Peers self-declare subnet membership via a `subnet:<hex32>` tag
//! on their own [`CapabilityAnnouncement`](super::capability::CapabilityAnnouncement).
//! The capability index parses the tag at fold time and stores the
//! `NodeId → SubnetId` mapping on the peer view, where the v0.4
//! capability-auth execute-gate consults it.
//!
//! Self-declaration is safe because the announcement is signed +
//! TOFU-bound to the entity's ed25519 key — a peer can only lie
//! about its own subnet, not someone else's. Operators who want
//! stricter membership use a random `SubnetId` value that's hard
//! to guess; the value-as-secret pattern matches the substrate's
//! existing channel-auth-token idiom.

use serde::{Deserialize, Serialize};

/// Wire-format tag prefix for self-declared subnet membership.
/// Operators emit `subnet:<32-hex-char>` as a capability tag on
/// their announcement; the substrate parses it via
/// [`SubnetId::from_tag`] at fold time.
pub const SUBNET_TAG_PREFIX: &str = "subnet:";

/// 16-byte stable subnet identifier. Opaque to the substrate —
/// operators choose the value (random, blake2s-of-name, etc.).
/// Pairs of nodes with the same `SubnetId` are treated as
/// subnet-mates by the v0.4 capability-auth execute-gate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SubnetId(pub [u8; 16]);

impl SubnetId {
    /// Construct from raw bytes.
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Borrow the 16-byte representation.
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// Parse a `subnet:<hex32>` capability-tag value into a
    /// `SubnetId`. Returns `None` on:
    /// - missing prefix,
    /// - wrong hex length (must be exactly 32 chars),
    /// - non-hex characters.
    pub fn from_tag(tag: &str) -> Option<Self> {
        let hex_part = tag.strip_prefix(SUBNET_TAG_PREFIX)?;
        if hex_part.len() != 32 {
            return None;
        }
        let mut out = [0u8; 16];
        for (i, chunk) in hex_part.as_bytes().chunks(2).enumerate() {
            let hi = hex_nibble(chunk[0])?;
            let lo = hex_nibble(chunk[1])?;
            out[i] = (hi << 4) | lo;
        }
        Some(Self(out))
    }

    /// Render as the canonical `subnet:<hex32>` tag form.
    pub fn to_tag(self) -> String {
        let mut s = String::with_capacity(SUBNET_TAG_PREFIX.len() + 32);
        s.push_str(SUBNET_TAG_PREFIX);
        for b in &self.0 {
            use std::fmt::Write;
            let _ = write!(s, "{:02x}", b);
        }
        s
    }
}

impl std::fmt::Display for SubnetId {
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
        let original = SubnetId([
            0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0, 0x0F, 0xED, 0xCB, 0xA9, 0x87, 0x65,
            0x43, 0x21,
        ]);
        let tag = original.to_tag();
        assert_eq!(tag, "subnet:123456789abcdef00fedcba987654321");
        let decoded = SubnetId::from_tag(&tag).expect("round trip");
        assert_eq!(decoded, original);
    }

    #[test]
    fn parse_rejects_missing_prefix() {
        // No `subnet:` prefix — not a subnet tag.
        assert!(SubnetId::from_tag("123456789abcdef00fedcba987654321").is_none());
    }

    #[test]
    fn parse_rejects_wrong_length() {
        // 31 hex chars instead of 32.
        assert!(SubnetId::from_tag("subnet:123456789abcdef00fedcba98765432").is_none());
        // 33 hex chars.
        assert!(SubnetId::from_tag("subnet:123456789abcdef00fedcba9876543211").is_none());
    }

    #[test]
    fn parse_rejects_non_hex_chars() {
        assert!(SubnetId::from_tag("subnet:gg3456789abcdef00fedcba987654321").is_none());
        assert!(SubnetId::from_tag("subnet:                                ").is_none());
    }

    #[test]
    fn display_matches_tag_hex() {
        let s = SubnetId([0xAB; 16]);
        let displayed = format!("{}", s);
        assert_eq!(displayed, "abababababababababababababababab");
        assert_eq!(s.to_tag(), format!("{}{}", SUBNET_TAG_PREFIX, displayed));
    }

    #[test]
    fn serde_round_trip_postcard() {
        let s = SubnetId([0xCC; 16]);
        let bytes = postcard::to_allocvec(&s).unwrap();
        let decoded: SubnetId = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, s);
    }
}
