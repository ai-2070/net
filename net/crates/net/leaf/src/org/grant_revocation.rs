//! Grant revocation: core's `behavior/org_grant_revocation.rs`, ported
//! byte-for-byte.
//!
//! An issuing org withdraws a capability grant or dispatcher grant before
//! its `not_after` by signing an [`OrgGrantRevocationBundle`] under its own
//! domain ([`ORG_GRANT_REVOCATION_SIG_DOMAIN`]). The wire layout, the
//! canonical `(kind, grant_id)` order and the dispatcher revocation id are
//! frozen against core by a shared golden vector (the same hex is pinned
//! in both crates). Core's clock read becomes a caller-supplied
//! `issued_at`.
//!
//! On the leaf the verified entries merge into
//! [`crate::org::revocation::RevocationFacts`], keyed by the bundle's org
//! so an org can only revoke grants it issued.

use crate::org::cert::{OrgError, OrgId, OrgKeypair};
use crate::org::grant::{OrgCapabilityGrant, OrgDispatcherGrant};

/// Signature domain for [`OrgGrantRevocationBundle`].
pub const ORG_GRANT_REVOCATION_SIG_DOMAIN: &[u8] = b"net-org-grant-revocation-v1";

/// blake3 `derive_key` context naming a dispatcher grant for revocation
/// (dispatcher grants carry no id field; the id is derived from the full
/// wire form).
pub const DISPATCHER_GRANT_ID_CONTEXT: &str = "net-org-dispatcher-grant-id-v1";

/// Most entries one bundle may carry.
pub const MAX_GRANT_REVOCATIONS_PER_BUNDLE: usize = 4_096;

/// Which grant family a [`RevokedGrant`] names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum RevokedGrantKind {
    /// An [`OrgCapabilityGrant`], named by its `grant_id`.
    Capability = 0x01,
    /// An [`OrgDispatcherGrant`], named by [`dispatcher_revocation_id`].
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
}

/// One revoked grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RevokedGrant {
    /// The grant family.
    pub kind: RevokedGrantKind,
    /// The grant's revocation id.
    pub grant_id: [u8; 32],
    /// The revoked grant's own `not_after` (unix seconds).
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
            grant_id: dispatcher_revocation_id(grant),
            not_after: grant.not_after,
        }
    }

    fn key(&self) -> (RevokedGrantKind, [u8; 32]) {
        (self.kind, self.grant_id)
    }
}

/// A dispatcher grant's revocation id:
/// `blake3::derive_key(DISPATCHER_GRANT_ID_CONTEXT, wire bytes)`.
pub fn dispatcher_revocation_id(grant: &OrgDispatcherGrant) -> [u8; 32] {
    blake3::derive_key(DISPATCHER_GRANT_ID_CONTEXT, &grant.to_bytes())
}

/// `true` once an entry with this `not_after` can no longer matter: the
/// grant it names fails its own validity check at every allowed skew.
pub fn revocation_entry_expired(not_after: u64, now_secs: u64) -> bool {
    now_secs > not_after.saturating_add(crate::org::MAX_TOKEN_CLOCK_SKEW_SECS)
}

/// A signed set of grant revocations issued by one org.
///
/// Wire format (`108 + 41·n` bytes): `org_id (32) ‖ issued_at (8, LE) ‖
/// entry_count (4, LE) ‖ entries (kind (1) ‖ grant_id (32) ‖ not_after
/// (8, LE)), strictly ascending by (kind, grant_id) ‖ signature (64)`,
/// signed over `ORG_GRANT_REVOCATION_SIG_DOMAIN ‖ everything before the
/// signature`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgGrantRevocationBundle {
    /// The org that issued the revoked grants (also the verifying key).
    pub org_id: OrgId,
    /// When the bundle was issued (unix seconds).
    pub issued_at: u64,
    entries: Vec<RevokedGrant>,
    /// ed25519 signature over the domain-prefixed payload.
    pub signature: [u8; 64],
}

impl OrgGrantRevocationBundle {
    const HEADER_SIZE: usize = 32 + 8 + 4;
    const ENTRY_SIZE: usize = 1 + 32 + 8;

    /// Issue a signed bundle. Duplicate `(kind, grant_id)` pairs are
    /// refused.
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
        if !strictly_ascending(&entries) {
            return Err(OrgError::NonCanonicalGrantRevocations);
        }
        let mut bundle = Self {
            org_id: org.org_id(),
            issued_at,
            entries,
            signature: [0u8; 64],
        };
        bundle.signature = org.sign(&bundle.signing_input());
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
        if !strictly_ascending(&self.entries) {
            return Err(OrgError::NonCanonicalGrantRevocations);
        }
        self.org_id.verify(&self.signing_input(), &self.signature)
    }

    /// Serialize to the canonical wire form.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = self.signed_payload();
        buf.extend_from_slice(&self.signature);
        buf
    }

    /// Strict decode; does NOT verify the signature.
    pub fn from_bytes(data: &[u8]) -> Result<Self, OrgError> {
        let fixed = |range: core::ops::Range<usize>| -> Result<&[u8], OrgError> {
            data.get(range).ok_or(OrgError::InvalidFormat)
        };
        if data.len() < Self::HEADER_SIZE + 64 {
            return Err(OrgError::InvalidFormat);
        }
        let mut org = [0u8; 32];
        org.copy_from_slice(fixed(0..32)?);
        let mut issued = [0u8; 8];
        issued.copy_from_slice(fixed(32..40)?);
        let mut count_bytes = [0u8; 4];
        count_bytes.copy_from_slice(fixed(40..44)?);
        let count = u32::from_le_bytes(count_bytes) as usize;
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
            let mut grant_id = [0u8; 32];
            grant_id.copy_from_slice(fixed(off + 1..off + 33)?);
            let mut not_after = [0u8; 8];
            not_after.copy_from_slice(fixed(off + 33..off + 41)?);
            entries.push(RevokedGrant {
                kind,
                grant_id,
                not_after: u64::from_le_bytes(not_after),
            });
            off += Self::ENTRY_SIZE;
        }
        if !strictly_ascending(&entries) {
            return Err(OrgError::NonCanonicalGrantRevocations);
        }
        let mut signature = [0u8; 64];
        signature.copy_from_slice(fixed(off..off + 64)?);
        Ok(Self {
            org_id: OrgId(org),
            issued_at: u64::from_le_bytes(issued),
            entries,
            signature,
        })
    }
}

fn strictly_ascending(entries: &[RevokedGrant]) -> bool {
    entries.windows(2).all(|w| w[0].key() < w[1].key())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Core pins the same hex in `behavior/org_grant_revocation.rs`.
    const GOLDEN_HEX: &str = "0d7550754e0800a5d237eef5826035766b9b3e5a15868a940ab289958788e3b040420f00000000000200000001010101010101010101010101010101010101010101010101010101010101010141420f000000000002020202020202020202020202020202020202020202020202020202020202020242420f0000000000a5fb6fb64634ad91c43c1256e0c89954a086cdd713a9dd2a88674c3d62793bf18ee458d38ae9a35f98a70c00cb44c53f5bca543d0e33aafe2ab87876423e7902";

    #[test]
    fn the_port_reproduces_cores_golden_bytes() {
        let bundle = OrgGrantRevocationBundle::issue_at(
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
        .expect("golden bundle");
        assert_eq!(crate::identity::hex_lower(&bundle.to_bytes()), GOLDEN_HEX);
        let decoded =
            OrgGrantRevocationBundle::from_bytes(&crate::identity::unhex(GOLDEN_HEX).expect("hex"))
                .expect("decode");
        decoded.verify().expect("core's bytes verify in the leaf");
        assert_eq!(decoded, bundle);
    }

    #[test]
    fn decode_refuses_tampering_and_bad_shapes() {
        let bytes = crate::identity::unhex(GOLDEN_HEX).expect("hex");
        let mut tampered = bytes.clone();
        tampered[45] ^= 0xff;
        let decoded = OrgGrantRevocationBundle::from_bytes(&tampered).expect("still decodes");
        assert!(decoded.verify().is_err());
        let mut unknown = bytes.clone();
        unknown[44] = 0x09;
        assert_eq!(
            OrgGrantRevocationBundle::from_bytes(&unknown).unwrap_err(),
            OrgError::InvalidFormat
        );
        assert_eq!(
            OrgGrantRevocationBundle::from_bytes(&bytes[..bytes.len() - 1]).unwrap_err(),
            OrgError::InvalidFormat
        );
    }
}
