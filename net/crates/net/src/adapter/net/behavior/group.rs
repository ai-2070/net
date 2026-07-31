//! Capability group identifier — capability-auth-plan Phase 1.
//!
//! A `GroupId` is a 32-byte opaque identifier for an operator-
//! defined named collection of peers. Mirrors [`super::subnet::SubnetId`]
//! one-for-one but at 32 bytes.
//!
//! # ⚠ Advisory only — NOT an access-control boundary
//!
//! This module previously documented a value-as-secret model: pick a
//! random 32-byte `GroupId` and its unguessability prevents
//! unauthorised membership claims. **That model does not hold on the
//! deployed transport, and `allowed_groups` must not be relied on to
//! keep anyone out.** Two independent reasons:
//!
//! 1. **The id is broadcast.** A provider restricting a capability
//!    publishes `allowed_groups` in its own
//!    [`CapabilityAnnouncement`](super::capability::CapabilityAnnouncement),
//!    which fans out to every directly-connected peer and forwards up
//!    to `MAX_CAPABILITY_HOPS` (16). Every node that receives it — and
//!    every node that relays it — learns the full set of authorised
//!    ids. The "secret" is published by the very announcement it is
//!    supposed to protect. Session AEAD is hop-by-hop, so it does not
//!    help.
//! 2. **The claim is unverified.** `group:` is not a reserved tag
//!    prefix, so `CapabilitySet::add_tag("group:<hex>")` is accepted by
//!    the public API in every binding. A signature proves only that the
//!    announcer holds its own key — never that any authority granted it
//!    that membership.
//!
//! Together those make the `allowed_groups` axis of
//! [`may_execute`](super::fold::capability_bridge::may_execute) bypassable
//! by any peer that has observed one provider announcement. The same
//! reasoning applies to `allowed_subnets`, whose values are both
//! published and only 32 bits wide.
//!
//! `allowed_nodes` is unaffected: `ann.node_id` is a blake2s derivation
//! over the announcing key and is checked against `entity_id` at
//! dispatch, so it cannot be claimed.
//!
//! Closing this needs an entitlement primitive the substrate does not
//! have yet: an issuer-signed assertion binding
//! `(subject, axis, value, validity)`, verified at ingest against the
//! issuer's key with revocation semantics fit for an execution gate —
//! i.e. the ORG vouches for the membership rather than the subject
//! asserting it. Note that
//! [`VerifiedOwner`](super::fold::capability::VerifiedOwner) is NOT that
//! primitive: it proves org belonging only and is authority-dark by
//! explicit invariant (OA-1), carrying no group, subnet, or invocation
//! entitlement.
//!
//! Until then, treat groups as advisory routing / grouping metadata.
//! See `docs/internal/misc/SECURITY_AUDIT_2026_07_31_SCOPED_CAPABILITIES.md`.
//!
//! # Membership
//!
//! Peers self-declare group membership via `group:<hex64>` tags on
//! their own `CapabilityAnnouncement`. A peer may emit multiple group
//! tags to claim membership in multiple groups. The capability index
//! parses every group tag and stores the `NodeId → Vec<GroupId>`
//! mapping on the peer view.
//!
//! The signature + TOFU pin do guarantee one narrow thing: a peer can
//! only make claims *about itself*, never about another node. That is
//! integrity of attribution, not authorisation.
//!
//! This is a separate concept from the compute-layer
//! `replica_group` / `standby_group` — those are about replica
//! placement, this is about access control. No relationship.

use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;

/// Wire-format tag prefix for self-declared group membership.
/// Operators emit `group:<64-hex-char>` as a capability tag on
/// their announcement; the substrate parses it via
/// [`GroupId::from_tag`] at fold time.
pub const GROUP_TAG_PREFIX: &str = "group:";

/// 32-byte stable group identifier. Opaque to the substrate.
///
/// ⚠ A random value does NOT prevent unauthorised membership claims —
/// providers publish their `allowed_groups` in a broadcast
/// announcement, so the value reaches every node in the mesh. See the
/// module docs; treat group membership as advisory.
///
/// The inner array is `pub(crate)` rather than `pub` — external
/// callers go through [`Self::from_bytes`] / [`Self::as_bytes`]
/// so the substrate keeps the option of changing the internal
/// representation without breaking the public surface.
#[expect(
    clippy::derived_hash_with_manual_eq,
    reason = "manual PartialEq is constant-time but byte-identical to the \
              derived one; the Hash/Eq invariant (equal values hash equal) \
              holds because both operate on the same 32 bytes"
)]
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Serialize, Deserialize)]
pub struct GroupId(pub(crate) [u8; 32]);

impl PartialEq for GroupId {
    /// Constant-time equality. A `GroupId` is a bearer secret —
    /// knowing the 32 random bytes *is* membership — so a
    /// data-dependent early-exit compare (the derived `PartialEq`)
    /// leaks the secret through timing. Fold every byte difference
    /// into one accumulator. Delegates to `subtle`'s audited,
    /// optimizer-resistant `ConstantTimeEq` rather than a hand-rolled
    /// `black_box` fold.
    ///
    /// Consistent with the derived `Hash`/`Eq` (equal bytes compare
    /// equal and hash equal), so use as a map key is unaffected.
    fn eq(&self, other: &Self) -> bool {
        self.0.ct_eq(&other.0).into()
    }
}

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
        let mut out = [0u8; 32];
        // `decode_to_slice` requires hex_part.len() == 2 *
        // out.len() (=64) and only ASCII hex digits — both length
        // and charset failures collapse to `Err`, mirroring the
        // hand-rolled predecessor's reject set exactly.
        hex::decode_to_slice(hex_part, &mut out).ok()?;
        Some(Self(out))
    }

    /// Render as the canonical `group:<hex64>` tag form.
    pub fn to_tag(self) -> String {
        format!("{GROUP_TAG_PREFIX}{}", hex::encode(self.0))
    }
}

impl std::fmt::Display for GroupId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&hex::encode(self.0))
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
    fn constant_time_eq_preserves_equality_semantics() {
        // The constant-time PartialEq must agree with byte equality
        // on every shape: identical, fully different, and a single
        // differing byte at the start or end (the cases an early-exit
        // compare would short-circuit on).
        assert_eq!(GroupId([0x11; 32]), GroupId([0x11; 32]));
        assert_ne!(GroupId([0x00; 32]), GroupId([0xFF; 32]));
        let mut first_byte = [0x11; 32];
        first_byte[0] = 0x12;
        assert_ne!(GroupId([0x11; 32]), GroupId(first_byte));
        let mut last_byte = [0x11; 32];
        last_byte[31] = 0x12;
        assert_ne!(GroupId([0x11; 32]), GroupId(last_byte));
        // Hash/Eq stay consistent: equal ids usable as map keys.
        let mut set = std::collections::HashSet::new();
        set.insert(GroupId([0x11; 32]));
        assert!(set.contains(&GroupId([0x11; 32])));
    }

    #[test]
    fn serde_round_trip_postcard() {
        let g = GroupId([0xAA; 32]);
        let bytes = postcard::to_allocvec(&g).unwrap();
        let decoded: GroupId = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, g);
    }
}
