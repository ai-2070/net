//! Revocation of organization grants before they expire.
//!
//! A capability grant (B → A) and a dispatcher grant (A → S) are
//! otherwise bounded only by their `not_after` (at most
//! [`MAX_ORG_GRANT_TTL_SECS`]). This module adds the signed object an
//! issuing org uses to withdraw one early:
//!
//! - [`OrgGrantRevocationBundle`] — "org X revokes these grants it
//!   issued", root-signed by X under its own domain
//!   ([`ORG_GRANT_REVOCATION_SIG_DOMAIN`]), so its bytes can never be
//!   confused with a floor bundle, certificate or grant.
//! - [`RevokedGrant`] — one entry: the grant's kind, its revocation id
//!   (a capability grant's `grant_id`; for a dispatcher grant, which has
//!   no id field, [`OrgDispatcherGrant::revocation_id`]) and the
//!   revoked grant's own `not_after`.
//!
//! Carrying `not_after` is what keeps the denylist bounded: an entry
//! is needed only while the grant it names could still verify, so a
//! store drops it once `not_after` (plus the maximum clock skew) has
//! passed. Grants live at most 30 days, so no entry outlives that.
//!
//! An entry is honoured only for grants its signer issued: the store
//! keys every entry by the bundle's `org_id`, and admission looks up a
//! capability grant under its `issuer_org` and a dispatcher grant under
//! its `org_id`. A bundle from org X therefore cannot revoke anything
//! org Y issued.
//!
//! Like floors, revocation is monotone: a merged entry is never
//! removed before it expires, and a lower or older bundle cannot
//! re-admit a revoked grant.

use ed25519_dalek::Signature;

use super::org::{current_timestamp, OrgError, OrgId, OrgKeypair};
use super::org_grant::{OrgCapabilityGrant, OrgDispatcherGrant, MAX_ORG_GRANT_TTL_SECS};

/// Signature domain for [`OrgGrantRevocationBundle`].
pub const ORG_GRANT_REVOCATION_SIG_DOMAIN: &[u8] = b"net-org-grant-revocation-v1";

/// blake3 `derive_key` context naming a dispatcher grant for
/// revocation. Dispatcher grants carry no id field, so their id is
/// derived from the full canonical wire form (signature included;
/// ed25519 signatures are deterministic, and the random nonce makes
/// every issue byte-distinct).
pub const DISPATCHER_GRANT_ID_CONTEXT: &str = "net-org-dispatcher-grant-id-v1";

/// Most entries one bundle may carry. Checked before any allocation
/// on decode.
pub const MAX_GRANT_REVOCATIONS_PER_BUNDLE: usize = 4_096;

/// Which grant family a [`RevokedGrant`] names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum RevokedGrantKind {
    /// An [`OrgCapabilityGrant`] (provider org → grantee org), named by
    /// its `grant_id`.
    Capability = 0x01,
    /// An [`OrgDispatcherGrant`] (org → dispatcher entity), named by
    /// [`OrgDispatcherGrant::revocation_id`].
    Dispatcher = 0x02,
}

impl RevokedGrantKind {
    fn from_tag(tag: u8) -> Result<Self, OrgError> {
        match tag {
            0x01 => Ok(Self::Capability),
            0x02 => Ok(Self::Dispatcher),
            _ => Err(OrgError::InvalidFormat),
        }
    }

    /// Stable lowercase name (`"capability"` / `"dispatcher"`), used in
    /// persisted state and CLI output.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Capability => "capability",
            Self::Dispatcher => "dispatcher",
        }
    }
}

/// One revoked grant inside an [`OrgGrantRevocationBundle`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct RevokedGrant {
    /// The grant family.
    pub kind: RevokedGrantKind,
    /// The grant's revocation id.
    pub grant_id: [u8; 32],
    /// The revoked grant's own `not_after` (unix seconds). Bounds how
    /// long the entry must be kept; it does not shorten anything.
    pub not_after: u64,
}

impl RevokedGrant {
    /// The entry revoking `grant`.
    pub fn capability(grant: &OrgCapabilityGrant) -> Self {
        Self {
            kind: RevokedGrantKind::Capability,
            grant_id: grant.grant_id,
            not_after: grant.not_after,
        }
    }

    /// The entry revoking `grant`.
    pub fn dispatcher(grant: &OrgDispatcherGrant) -> Self {
        Self {
            kind: RevokedGrantKind::Dispatcher,
            grant_id: grant.revocation_id(),
            not_after: grant.not_after,
        }
    }

    /// The canonical ordering key.
    fn key(&self) -> (RevokedGrantKind, [u8; 32]) {
        (self.kind, self.grant_id)
    }
}

impl std::fmt::Debug for RevokedGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RevokedGrant")
            .field("kind", &self.kind)
            .field("grant_id", &hex::encode(&self.grant_id[..8]))
            .field("not_after", &self.not_after)
            .finish()
    }
}

impl OrgDispatcherGrant {
    /// This grant's revocation id:
    /// `blake3::derive_key(DISPATCHER_GRANT_ID_CONTEXT, wire bytes)`.
    pub fn revocation_id(&self) -> [u8; 32] {
        blake3::derive_key(DISPATCHER_GRANT_ID_CONTEXT, &self.to_bytes())
    }
}

/// A signed set of grant revocations issued by one org: "every grant
/// listed here that I issued is revoked."
///
/// Wire format (`108 + 41·n` bytes):
/// ```text
/// org_id:       32 bytes (OrgId — the issuing org, the verifying key)
/// issued_at:     8 bytes (u64 unix seconds)
/// entry_count:   4 bytes (u32 LE)
/// entries:      41 bytes each — kind (1) ‖ grant_id (32) ‖ not_after
///               (8, u64 LE) — strictly ascending by (kind, grant_id),
///               no duplicates
/// --- signed above (ORG_GRANT_REVOCATION_SIG_DOMAIN prefixed) ---
/// signature:    64 bytes (ed25519 by org_id)
/// ```
///
/// A bundle with no entries is valid. It revokes nothing but still
/// carries a signed `issued_at`, which a provider can use as evidence
/// that it has heard from the org recently.
#[derive(Clone, PartialEq, Eq)]
pub struct OrgGrantRevocationBundle {
    /// The org that issued the revoked grants (also the verifying key).
    pub org_id: OrgId,
    /// When the bundle was issued (unix seconds).
    pub issued_at: u64,
    /// Entries in canonical order. Private so every constructed value
    /// upholds the ordering invariant.
    entries: Vec<RevokedGrant>,
    /// ed25519 signature over
    /// `ORG_GRANT_REVOCATION_SIG_DOMAIN ‖ signed payload`.
    pub signature: [u8; 64],
}

impl OrgGrantRevocationBundle {
    /// org_id (32) + issued_at (8) + entry_count (4).
    const HEADER_SIZE: usize = 32 + 8 + 4;
    /// kind (1) + grant_id (32) + not_after (8).
    const ENTRY_SIZE: usize = 1 + 32 + 8;

    /// Issue a signed bundle revoking `entries`. Duplicate
    /// `(kind, grant_id)` pairs are refused rather than merged: two
    /// entries for one grant usually mean an operator mix-up.
    pub fn try_issue(
        org: &OrgKeypair,
        entries: impl IntoIterator<Item = RevokedGrant>,
    ) -> Result<Self, OrgError> {
        Self::issue_at(org, entries, current_timestamp())
    }

    /// [`Self::try_issue`] with an explicit `issued_at`, for
    /// deterministic tests.
    #[doc(hidden)]
    pub fn issue_at(
        org: &OrgKeypair,
        entries: impl IntoIterator<Item = RevokedGrant>,
        issued_at: u64,
    ) -> Result<Self, OrgError> {
        let mut entries: Vec<RevokedGrant> = entries.into_iter().collect();
        if entries.len() > MAX_GRANT_REVOCATIONS_PER_BUNDLE {
            return Err(OrgError::TooManyGrantRevocations);
        }
        entries.sort_by_key(RevokedGrant::key);
        if !entries_strictly_ascending(&entries) {
            return Err(OrgError::NonCanonicalGrantRevocations);
        }
        let mut bundle = Self {
            org_id: org.org_id(),
            issued_at,
            entries,
            signature: [0u8; 64],
        };
        bundle.signature = org.sign(&bundle.signing_input()).to_bytes();
        Ok(bundle)
    }

    /// The entries, in canonical order.
    pub fn entries(&self) -> &[RevokedGrant] {
        &self.entries
    }

    fn signed_payload(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(Self::HEADER_SIZE + self.entries.len() * Self::ENTRY_SIZE);
        buf.extend_from_slice(self.org_id.as_bytes());
        buf.extend_from_slice(&self.issued_at.to_le_bytes());
        buf.extend_from_slice(&(self.entries.len() as u32).to_le_bytes());
        for entry in &self.entries {
            buf.push(entry.kind as u8);
            buf.extend_from_slice(&entry.grant_id);
            buf.extend_from_slice(&entry.not_after.to_le_bytes());
        }
        buf
    }

    fn signing_input(&self) -> Vec<u8> {
        let payload = self.signed_payload();
        let mut buf = Vec::with_capacity(ORG_GRANT_REVOCATION_SIG_DOMAIN.len() + payload.len());
        buf.extend_from_slice(ORG_GRANT_REVOCATION_SIG_DOMAIN);
        buf.extend_from_slice(&payload);
        buf
    }

    /// Verify structural canonicality, then the org signature.
    pub fn verify(&self) -> Result<(), OrgError> {
        if self.entries.len() > MAX_GRANT_REVOCATIONS_PER_BUNDLE {
            return Err(OrgError::TooManyGrantRevocations);
        }
        if !entries_strictly_ascending(&self.entries) {
            return Err(OrgError::NonCanonicalGrantRevocations);
        }
        let sig = Signature::from_bytes(&self.signature);
        self.org_id.verify(&self.signing_input(), &sig)
    }

    /// Serialize to the canonical wire form.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = self.signed_payload();
        buf.extend_from_slice(&self.signature);
        buf
    }

    /// Strict decode: the entry count is checked against
    /// [`MAX_GRANT_REVOCATIONS_PER_BUNDLE`] before any allocation, the
    /// total length must match exactly, every kind tag must be known,
    /// and entries must be strictly ascending. Does NOT verify the
    /// signature.
    #[expect(
        clippy::unwrap_used,
        reason = "lengths are checked against the exact expected size first; fixed-offset slices convert infallibly"
    )]
    pub fn from_bytes(data: &[u8]) -> Result<Self, OrgError> {
        if data.len() < Self::HEADER_SIZE + 64 {
            return Err(OrgError::InvalidFormat);
        }
        let org_id = OrgId::from_bytes(data[0..32].try_into().unwrap());
        let issued_at = u64::from_le_bytes(data[32..40].try_into().unwrap());
        let count = u32::from_le_bytes(data[40..44].try_into().unwrap()) as usize;
        if count > MAX_GRANT_REVOCATIONS_PER_BUNDLE {
            return Err(OrgError::TooManyGrantRevocations);
        }
        if data.len() != Self::HEADER_SIZE + count * Self::ENTRY_SIZE + 64 {
            return Err(OrgError::InvalidFormat);
        }
        let mut entries = Vec::with_capacity(count);
        let mut off = Self::HEADER_SIZE;
        for _ in 0..count {
            let kind = RevokedGrantKind::from_tag(data[off])?;
            let grant_id: [u8; 32] = data[off + 1..off + 33].try_into().unwrap();
            let not_after = u64::from_le_bytes(data[off + 33..off + 41].try_into().unwrap());
            entries.push(RevokedGrant {
                kind,
                grant_id,
                not_after,
            });
            off += Self::ENTRY_SIZE;
        }
        if !entries_strictly_ascending(&entries) {
            return Err(OrgError::NonCanonicalGrantRevocations);
        }
        let mut signature = [0u8; 64];
        signature.copy_from_slice(&data[off..off + 64]);
        Ok(Self {
            org_id,
            issued_at,
            entries,
            signature,
        })
    }
}

impl std::fmt::Debug for OrgGrantRevocationBundle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OrgGrantRevocationBundle")
            .field("org_id", &self.org_id)
            .field("issued_at", &self.issued_at)
            .field("entries", &self.entries.len())
            .finish()
    }
}

// Same serde-over-wire-bytes discipline as the other org objects.
impl serde::Serialize for OrgGrantRevocationBundle {
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

impl<'de> serde::Deserialize<'de> for OrgGrantRevocationBundle {
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

fn entries_strictly_ascending(entries: &[RevokedGrant]) -> bool {
    entries.windows(2).all(|w| w[0].key() < w[1].key())
}

/// `true` once an entry with this `not_after` can no longer matter:
/// the grant it names fails its own validity check at every skew a
/// verifier may apply.
pub(crate) fn revocation_entry_expired(not_after: u64, now_secs: u64) -> bool {
    now_secs > not_after.saturating_add(crate::adapter::net::identity::MAX_TOKEN_CLOCK_SKEW_SECS)
}

/// Upper bound on how far ahead of `issued_at` an entry's `not_after`
/// may sensibly sit. Not enforced on decode (the signer is trusted for
/// its own grants), only used to sanity-check operator input.
pub const MAX_REVOCATION_ENTRY_HORIZON_SECS: u64 = MAX_ORG_GRANT_TTL_SECS;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::org_grant::{
        CapabilityAuthorityId, DispatcherScope, GrantRights, GrantTargetScope,
    };
    use crate::adapter::net::identity::EntityId;

    fn org() -> OrgKeypair {
        OrgKeypair::from_bytes([0x5au8; 32])
    }

    fn entry(kind: RevokedGrantKind, byte: u8) -> RevokedGrant {
        RevokedGrant {
            kind,
            grant_id: [byte; 32],
            not_after: 1_000 + byte as u64,
        }
    }

    /// The cross-implementation pin. The browser leaf's port
    /// (`leaf/src/org/grant_revocation.rs`) issues the same bundle from
    /// the same inputs and asserts the same bytes, so the two encoders
    /// cannot drift apart silently.
    const GOLDEN_HEX: &str = "0d7550754e0800a5d237eef5826035766b9b3e5a15868a940ab289958788e3b040420f00000000000200000001010101010101010101010101010101010101010101010101010101010101010141420f000000000002020202020202020202020202020202020202020202020202020202020202020242420f0000000000a5fb6fb64634ad91c43c1256e0c89954a086cdd713a9dd2a88674c3d62793bf18ee458d38ae9a35f98a70c00cb44c53f5bca543d0e33aafe2ab87876423e7902";

    fn golden() -> OrgGrantRevocationBundle {
        OrgGrantRevocationBundle::issue_at(
            &OrgKeypair::from_bytes([0x5au8; 32]),
            [
                RevokedGrant {
                    kind: RevokedGrantKind::Dispatcher,
                    grant_id: [0x02u8; 32],
                    not_after: 1_000_002,
                },
                RevokedGrant {
                    kind: RevokedGrantKind::Capability,
                    grant_id: [0x01u8; 32],
                    not_after: 1_000_001,
                },
            ],
            1_000_000,
        )
        .expect("golden bundle")
    }

    #[test]
    fn golden_bytes_are_pinned() {
        let bundle = golden();
        assert_eq!(hex::encode(bundle.to_bytes()), GOLDEN_HEX);
        let decoded = OrgGrantRevocationBundle::from_bytes(&hex::decode(GOLDEN_HEX).expect("hex"))
            .expect("decode");
        decoded.verify().expect("the pinned bytes verify");
    }

    #[test]
    fn issue_sorts_and_round_trips() {
        let bundle = OrgGrantRevocationBundle::issue_at(
            &org(),
            [
                entry(RevokedGrantKind::Dispatcher, 1),
                entry(RevokedGrantKind::Capability, 9),
                entry(RevokedGrantKind::Capability, 2),
            ],
            77,
        )
        .expect("issue");
        let kinds: Vec<_> = bundle
            .entries()
            .iter()
            .map(|e| (e.kind, e.grant_id[0]))
            .collect();
        assert_eq!(
            kinds,
            vec![
                (RevokedGrantKind::Capability, 2),
                (RevokedGrantKind::Capability, 9),
                (RevokedGrantKind::Dispatcher, 1),
            ]
        );
        bundle.verify().expect("verifies");
        let bytes = bundle.to_bytes();
        assert_eq!(bytes.len(), 44 + 3 * 41 + 64);
        let decoded = OrgGrantRevocationBundle::from_bytes(&bytes).expect("decode");
        assert_eq!(decoded, bundle);
        decoded.verify().expect("decoded verifies");
    }

    #[test]
    fn an_empty_bundle_is_valid() {
        let bundle = OrgGrantRevocationBundle::issue_at(&org(), [], 5).expect("issue");
        bundle.verify().expect("verifies");
        let decoded = OrgGrantRevocationBundle::from_bytes(&bundle.to_bytes()).expect("decode");
        assert!(decoded.entries().is_empty());
        assert_eq!(decoded.issued_at, 5);
    }

    #[test]
    fn duplicate_entries_are_refused_at_issue() {
        let e = entry(RevokedGrantKind::Capability, 3);
        assert_eq!(
            OrgGrantRevocationBundle::issue_at(&org(), [e, e], 1).unwrap_err(),
            OrgError::NonCanonicalGrantRevocations
        );
    }

    #[test]
    fn tampering_breaks_the_signature() {
        let bundle =
            OrgGrantRevocationBundle::issue_at(&org(), [entry(RevokedGrantKind::Capability, 4)], 1)
                .expect("issue");
        let mut bytes = bundle.to_bytes();
        // Flip a byte of the grant id.
        bytes[44 + 1] ^= 0xff;
        let tampered = OrgGrantRevocationBundle::from_bytes(&bytes).expect("still decodes");
        assert!(tampered.verify().is_err());
    }

    #[test]
    fn decode_refuses_noncanonical_unknown_and_truncated_input() {
        let bundle = OrgGrantRevocationBundle::issue_at(
            &org(),
            [
                entry(RevokedGrantKind::Capability, 1),
                entry(RevokedGrantKind::Capability, 2),
            ],
            1,
        )
        .expect("issue");
        let bytes = bundle.to_bytes();

        // Swap the two entries: still the right length, wrong order.
        let mut swapped = bytes.clone();
        let (a, b) = (44, 44 + 41);
        let first: Vec<u8> = swapped[a..a + 41].to_vec();
        let second: Vec<u8> = swapped[b..b + 41].to_vec();
        swapped[a..a + 41].copy_from_slice(&second);
        swapped[b..b + 41].copy_from_slice(&first);
        assert_eq!(
            OrgGrantRevocationBundle::from_bytes(&swapped).unwrap_err(),
            OrgError::NonCanonicalGrantRevocations
        );

        let mut unknown = bytes.clone();
        unknown[44] = 0x07;
        assert_eq!(
            OrgGrantRevocationBundle::from_bytes(&unknown).unwrap_err(),
            OrgError::InvalidFormat
        );

        assert_eq!(
            OrgGrantRevocationBundle::from_bytes(&bytes[..bytes.len() - 1]).unwrap_err(),
            OrgError::InvalidFormat
        );
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert_eq!(
            OrgGrantRevocationBundle::from_bytes(&trailing).unwrap_err(),
            OrgError::InvalidFormat
        );

        let mut huge = bytes;
        huge[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(
            OrgGrantRevocationBundle::from_bytes(&huge).unwrap_err(),
            OrgError::TooManyGrantRevocations
        );
    }

    /// A grant-revocation signature must never verify as any other org
    /// object's signature, and vice versa: the domain is distinct.
    #[test]
    fn the_signing_domain_is_distinct() {
        use crate::adapter::net::behavior::org::ORG_FLOORS_SIG_DOMAIN;
        use crate::adapter::net::behavior::org_grant::{
            ORG_CAPABILITY_GRANT_SIG_DOMAIN, ORG_DISPATCHER_GRANT_SIG_DOMAIN,
        };
        for other in [
            ORG_FLOORS_SIG_DOMAIN,
            ORG_CAPABILITY_GRANT_SIG_DOMAIN,
            ORG_DISPATCHER_GRANT_SIG_DOMAIN,
        ] {
            assert_ne!(ORG_GRANT_REVOCATION_SIG_DOMAIN, other);
            assert!(!ORG_GRANT_REVOCATION_SIG_DOMAIN.starts_with(other));
            assert!(!other.starts_with(ORG_GRANT_REVOCATION_SIG_DOMAIN));
        }
    }

    #[test]
    fn entries_name_the_grants_they_revoke() {
        let issuer = org();
        let dispatcher = OrgDispatcherGrant::try_issue(
            &issuer,
            EntityId::from_bytes([0x11u8; 32]),
            DispatcherScope::Any,
            3_600,
        )
        .expect("dispatcher grant");
        let again = OrgDispatcherGrant::try_issue(
            &issuer,
            EntityId::from_bytes([0x11u8; 32]),
            DispatcherScope::Any,
            3_600,
        )
        .expect("second dispatcher grant");
        let d = RevokedGrant::dispatcher(&dispatcher);
        assert_eq!(d.kind, RevokedGrantKind::Dispatcher);
        assert_eq!(d.not_after, dispatcher.not_after);
        assert_eq!(d.grant_id, dispatcher.revocation_id());
        assert_ne!(
            dispatcher.revocation_id(),
            again.revocation_id(),
            "two issues of the same grant shape must be distinguishable, or \
             revoking one would revoke the other"
        );

        let grantee = OrgKeypair::from_bytes([0x6bu8; 32]).org_id();
        let cap = OrgCapabilityGrant::try_issue(
            &issuer,
            grantee,
            CapabilityAuthorityId::for_tag("nrpc:revoke-me"),
            GrantRights::INVOKE,
            GrantTargetScope::AnyNodeOwnedBy(issuer.org_id()),
            3_600,
        )
        .expect("capability grant")
        .0;
        let c = RevokedGrant::capability(&cap);
        assert_eq!(c.kind, RevokedGrantKind::Capability);
        assert_eq!(c.grant_id, cap.grant_id);
        assert_eq!(c.not_after, cap.not_after);
    }

    #[test]
    fn entries_expire_only_past_the_skew_ceiling() {
        let skew = crate::adapter::net::identity::MAX_TOKEN_CLOCK_SKEW_SECS;
        assert!(!revocation_entry_expired(100, 100));
        assert!(!revocation_entry_expired(100, 100 + skew));
        assert!(revocation_entry_expired(100, 101 + skew));
    }
}
