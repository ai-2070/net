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
use subtle::ConstantTimeEq;

/// Wire-format tag prefix for self-declared subnet membership.
/// Operators emit `subnet:<32-hex-char>` as a capability tag on
/// their announcement; the substrate parses it via
/// [`SubnetId::from_tag`] at fold time.
pub const SUBNET_TAG_PREFIX: &str = "subnet:";

/// 16-byte stable subnet identifier. Opaque to the substrate —
/// operators choose the value (random, blake2s-of-name, etc.).
/// Pairs of nodes with the same `SubnetId` are treated as
/// subnet-mates by the v0.4 capability-auth execute-gate.
///
/// The inner array is `pub(crate)` rather than `pub` — external
/// callers go through [`Self::from_bytes`] / [`Self::as_bytes`]
/// so the substrate keeps the option of changing the internal
/// representation (e.g. a typed length tag) without breaking
/// the public surface.
#[expect(
    clippy::derived_hash_with_manual_eq,
    reason = "manual PartialEq is constant-time but byte-identical to the \
              derived one; the Hash/Eq invariant (equal values hash equal) \
              holds because both operate on the same 16 bytes"
)]
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Serialize, Deserialize)]
pub struct SubnetId(pub(crate) [u8; 16]);

impl PartialEq for SubnetId {
    /// Constant-time equality. In the stricter-membership mode
    /// documented on this module a `SubnetId` is a bearer secret
    /// ("hard to guess"), so a data-dependent early-exit compare
    /// leaks it through timing. Delegate to `subtle`'s audited,
    /// optimizer-resistant `ConstantTimeEq` rather than a hand-rolled
    /// `black_box` fold.
    ///
    /// Consistent with the derived `Hash`/`Eq`, so map-key use is
    /// unaffected (well-known public values like `GLOBAL` compare
    /// correctly too — constant time is simply harmless for them).
    fn eq(&self, other: &Self) -> bool {
        self.0.ct_eq(&other.0).into()
    }
}

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
    /// `SubnetId`. Returns `None` on missing prefix, wrong hex
    /// length (must be exactly 32 chars), or non-hex characters.
    pub fn from_tag(tag: &str) -> Option<Self> {
        let hex_part = tag.strip_prefix(SUBNET_TAG_PREFIX)?;
        let mut out = [0u8; 16];
        // `decode_to_slice` requires hex_part.len() == 2 *
        // out.len() (=32) and only ASCII hex digits — both length
        // and charset failures collapse to `Err`, mirroring the
        // hand-rolled predecessor's reject set exactly.
        hex::decode_to_slice(hex_part, &mut out).ok()?;
        Some(Self(out))
    }

    /// Render as the canonical `subnet:<hex32>` tag form.
    pub fn to_tag(self) -> String {
        format!("{SUBNET_TAG_PREFIX}{}", hex::encode(self.0))
    }
}

impl std::fmt::Display for SubnetId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&hex::encode(self.0))
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

    #[test]
    fn constant_time_eq_preserves_equality_semantics() {
        assert_eq!(SubnetId([0x11; 16]), SubnetId([0x11; 16]));
        assert_ne!(SubnetId([0x00; 16]), SubnetId([0xFF; 16]));
        let mut first_byte = [0x11; 16];
        first_byte[0] = 0x12;
        assert_ne!(SubnetId([0x11; 16]), SubnetId(first_byte));
        let mut last_byte = [0x11; 16];
        last_byte[15] = 0x12;
        assert_ne!(SubnetId([0x11; 16]), SubnetId(last_byte));
        let mut set = std::collections::HashSet::new();
        set.insert(SubnetId([0x11; 16]));
        assert!(set.contains(&SubnetId([0x11; 16])));
    }
}
