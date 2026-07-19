//! OA-3 §3.1 of `docs/plans/ORG_CAPABILITY_AUTH_PLAN.md` — the
//! cryptographic foundation of grant-scoped private discovery.
//!
//! A [`ScopedCapabilityAnnouncement`] (the wire object itself lands in a later
//! slice) privately carries a capability descriptor to exactly one audience:
//! the descriptor plaintext is sealed with **XChaCha20-Poly1305** under the
//! per-audience `discovery_key`, and the cleartext framing that routes the
//! envelope is bound into the AEAD as associated data so a forwarder can neither
//! read the descriptor nor transplant it onto a different framing.
//!
//! This module is that key layer only:
//!
//! - [`seal_descriptor`] / [`open_descriptor`] — the AEAD seal/open, following
//!   the in-tree sealed-box idiom (`identity/envelope.rs`): a random 24-byte
//!   nonce, `Payload { msg, aad }`, no key material on any error path we own.
//! - [`scoped_ann_associated_data`] — the canonical AD binding
//!   `provider ‖ owner_org ‖ audience_handle ‖ grant_id ‖ generation ‖
//!   expires_at`. For the **owner** audience `grant_id` is the reserved
//!   [`OWNER_AUDIENCE_GRANT_SENTINEL`] (all-zero), so an owner and a granted
//!   envelope can never be confused under one AD — the same zero grant id that
//!   `OrgCapabilityGrant` issuance/decode already reject
//!   ([`OrgError::ReservedGrantId`](super::org::OrgError::ReservedGrantId)).
//! - Dual size bounds ([`MAX_SCOPED_ANN_CIPHERTEXT_BYTES`],
//!   [`MAX_SCOPED_ANN_ENCODED_BYTES`]) enforced here at the seal/open boundary
//!   and, in the later slice, again at the envelope builder/decoder — an
//!   oversized descriptor is the typed [`ScopedAnnouncementError::DescriptorTooLarge`],
//!   never silently trimmed (trimming changes capability semantics).
//!
//! The raw `discovery_key` is never a member of any wire object: it arrives via
//! a borrowing accessor on the non-serializable secret types
//! ([`OrgAudienceSecret`](super::org_grant::OrgAudienceSecret),
//! [`OwnerAudienceCredential`](super::org_authority::OwnerAudienceCredential))
//! and this module only ever borrows it.

use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    XChaCha20Poly1305,
};

use super::org::{OrgId, OrgMembershipCert};
use crate::adapter::net::identity::EntityId;
use crate::adapter::net::protocol::MAX_PACKET_SIZE;

/// Signing domain for the scoped announcement's OUTER signature by the
/// publishing provider P (§3.1). The `-v1` suffix is load-bearing: a field-list
/// change needs a NEW domain string, never a silent reinterpretation (same
/// discipline as `ORG_CERT_SIG_DOMAIN`). The outer signature itself is applied
/// by the envelope builder in a later slice; the domain is pinned here so the
/// whole OA-3 family shares one constant.
pub const SCOPED_ANN_SIG_DOMAIN: &[u8] = b"net-org-scoped-ann-v1";

/// The reserved all-zero grant id used as the OWNER-audience sentinel in the
/// associated data (§3.1). `OrgCapabilityGrant` issuance AND decode reject this
/// value ([`OrgError::ReservedGrantId`](super::org::OrgError::ReservedGrantId)),
/// so it can only ever appear as the owner-audience marker — a granted envelope
/// always carries a real random grant id, and the two AD domains are therefore
/// disjoint.
pub const OWNER_AUDIENCE_GRANT_SENTINEL: [u8; 32] = [0u8; 32];

/// XChaCha20-Poly1305 nonce size (192-bit / 24-byte extended nonce). Large
/// enough that a per-envelope random nonce has negligible collision risk, so we
/// avoid a stateful counter.
pub const SCOPED_ANN_NONCE_SIZE: usize = 24;

/// Poly1305 authentication tag size appended to every sealed ciphertext.
pub const SCOPED_ANN_AEAD_TAG_SIZE: usize = 16;

/// Conservative reservation carved out of the 8 KiB transport packet
/// ([`MAX_PACKET_SIZE`]) for the packet header, the packet-level AEAD tag, the
/// subprotocol + per-event framing that will carry a scoped announcement (wired
/// in a later slice), and safety headroom. Deliberately generous so the
/// envelope never rides the edge of the frame.
const SCOPED_ANN_TRANSPORT_RESERVE: usize = 2048;

/// Whole-envelope byte cap (§3.1 "dual size bounds"): what a scoped-announcement
/// envelope may occupy on the wire. Enforced at BOTH the builder and the decoder
/// once the envelope lands — an oversized envelope is a typed error, never
/// silently trimmed.
pub const MAX_SCOPED_ANN_ENCODED_BYTES: usize = MAX_PACKET_SIZE - SCOPED_ANN_TRANSPORT_RESERVE;

/// Fixed outer-envelope overhead: every field except the ciphertext —
/// `version(1) ‖ provider(32) ‖ owner_org(32) ‖ owner_cert(WIRE_SIZE) ‖
/// audience_handle(32) ‖ grant_id(32) ‖ generation(8) ‖ expires_at(8) ‖
/// nonce(24) ‖ ciphertext_len(2) ‖ signature(64)`. The plaintext cap is derived
/// from this so a maximum-size descriptor still fits the whole-envelope cap.
pub const SCOPED_ANN_FIXED_OVERHEAD: usize =
    1 + 32 + 32 + OrgMembershipCert::WIRE_SIZE + 32 + 32 + 8 + 8 + SCOPED_ANN_NONCE_SIZE + 2 + 64;

/// Plaintext descriptor cap (§3.1 "plaintext-side cap"): the maximum number of
/// descriptor bytes that may be sealed. The sealed ciphertext is this plus
/// [`SCOPED_ANN_AEAD_TAG_SIZE`]; the whole envelope is that plus
/// [`SCOPED_ANN_FIXED_OVERHEAD`], staying within [`MAX_SCOPED_ANN_ENCODED_BYTES`].
pub const MAX_SCOPED_ANN_CIPHERTEXT_BYTES: usize =
    MAX_SCOPED_ANN_ENCODED_BYTES - SCOPED_ANN_FIXED_OVERHEAD - SCOPED_ANN_AEAD_TAG_SIZE;

// The three bounds partition the packet budget exactly, and the plaintext cap
// leaves ample room for a real capability descriptor.
const _: () = assert!(
    SCOPED_ANN_FIXED_OVERHEAD + SCOPED_ANN_AEAD_TAG_SIZE + MAX_SCOPED_ANN_CIPHERTEXT_BYTES
        == MAX_SCOPED_ANN_ENCODED_BYTES,
    "envelope budget must partition into fixed overhead + AEAD tag + plaintext exactly"
);
const _: () = assert!(
    MAX_SCOPED_ANN_ENCODED_BYTES + SCOPED_ANN_TRANSPORT_RESERVE == MAX_PACKET_SIZE,
    "encoded-envelope cap plus transport reserve must equal the packet size"
);
const _: () = assert!(
    MAX_SCOPED_ANN_CIPHERTEXT_BYTES > 512,
    "plaintext budget must leave room for a real capability descriptor"
);

/// Errors from sealing/opening or bounding a scoped announcement. Manual
/// `Display` + `Error` (org-family house style — no `thiserror` in this module
/// tree).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopedAnnouncementError {
    /// A descriptor plaintext (or the ciphertext it would produce) exceeds its
    /// size bound. Reports the offending size and the maximum; the descriptor is
    /// NEVER trimmed, because trimming would change capability semantics.
    DescriptorTooLarge {
        /// The plaintext-descriptor byte count that was rejected.
        encoded: usize,
        /// The maximum permitted ([`MAX_SCOPED_ANN_CIPHERTEXT_BYTES`]).
        maximum: usize,
    },
    /// AEAD open failed: wrong key, wrong associated data (framing transplant),
    /// or tampered ciphertext. Deliberately one opaque variant — the failure
    /// reason is not a decryption oracle.
    SealOpenFailed,
    /// A structurally malformed input (e.g. a ciphertext shorter than the AEAD
    /// tag).
    InvalidFormat,
}

impl std::fmt::Display for ScopedAnnouncementError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScopedAnnouncementError::DescriptorTooLarge { encoded, maximum } => write!(
                f,
                "scoped-announcement descriptor is {encoded} bytes, exceeds the {maximum}-byte maximum"
            ),
            ScopedAnnouncementError::SealOpenFailed => {
                f.write_str("scoped-announcement AEAD open failed")
            }
            ScopedAnnouncementError::InvalidFormat => {
                f.write_str("scoped-announcement ciphertext is malformed")
            }
        }
    }
}

impl std::error::Error for ScopedAnnouncementError {}

/// Build the AEAD associated data (§3.1): the cleartext framing that routes the
/// envelope, bound into the seal so a forwarder cannot transplant a ciphertext
/// onto a different provider / owner / audience / grant / generation / expiry.
///
/// Layout: `provider(32) ‖ owner_org(32) ‖ audience_handle(32) ‖ grant_id(32) ‖
/// generation(LE u64, 8) ‖ expires_at(LE u64, 8)` = 144 bytes. For the OWNER
/// audience pass [`OWNER_AUDIENCE_GRANT_SENTINEL`] as `grant_id`.
pub fn scoped_ann_associated_data(
    provider: &EntityId,
    owner_org: &OrgId,
    audience_handle: &[u8; 32],
    grant_id: &[u8; 32],
    generation: u64,
    expires_at: u64,
) -> Vec<u8> {
    let mut aad = Vec::with_capacity(32 * 4 + 8 + 8);
    aad.extend_from_slice(provider.as_bytes());
    aad.extend_from_slice(owner_org.as_bytes());
    aad.extend_from_slice(audience_handle);
    aad.extend_from_slice(grant_id);
    aad.extend_from_slice(&generation.to_le_bytes());
    aad.extend_from_slice(&expires_at.to_le_bytes());
    aad
}

/// A fresh random 24-byte XChaCha nonce. `getrandom` failure aborts — a
/// predictable or reused nonce under a fixed key voids XChaCha's
/// confidentiality (same fail-closed discipline as
/// [`OrgAudienceSecret::mint`](super::org_grant::OrgAudienceSecret)).
fn random_scoped_ann_nonce() -> [u8; SCOPED_ANN_NONCE_SIZE] {
    let mut nonce = [0u8; SCOPED_ANN_NONCE_SIZE];
    if let Err(e) = getrandom::fill(&mut nonce) {
        eprintln!(
            "FATAL: scoped-announcement nonce getrandom failure ({e:?}); aborting to avoid nonce reuse"
        );
        std::process::abort();
    }
    nonce
}

/// Seal a descriptor `plaintext` under the audience `discovery_key` with a fresh
/// random nonce and the given associated data. Returns `(nonce, ciphertext)`
/// where `ciphertext` includes the 16-byte AEAD tag. Rejects a plaintext larger
/// than [`MAX_SCOPED_ANN_CIPHERTEXT_BYTES`] with
/// [`ScopedAnnouncementError::DescriptorTooLarge`].
pub fn seal_descriptor(
    discovery_key: &[u8; 32],
    aad: &[u8],
    plaintext: &[u8],
) -> Result<([u8; SCOPED_ANN_NONCE_SIZE], Vec<u8>), ScopedAnnouncementError> {
    let nonce = random_scoped_ann_nonce();
    let ciphertext = seal_descriptor_with_nonce(discovery_key, &nonce, aad, plaintext)?;
    Ok((nonce, ciphertext))
}

/// Deterministic seal with a caller-supplied nonce. Real publication uses
/// [`seal_descriptor`] (fresh random nonce); this exists so golden vectors can
/// pin a fixed nonce. NEVER reuse a `(discovery_key, nonce)` pair across two
/// distinct plaintexts.
pub fn seal_descriptor_with_nonce(
    discovery_key: &[u8; 32],
    nonce: &[u8; SCOPED_ANN_NONCE_SIZE],
    aad: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>, ScopedAnnouncementError> {
    if plaintext.len() > MAX_SCOPED_ANN_CIPHERTEXT_BYTES {
        return Err(ScopedAnnouncementError::DescriptorTooLarge {
            encoded: plaintext.len(),
            maximum: MAX_SCOPED_ANN_CIPHERTEXT_BYTES,
        });
    }
    let aead = XChaCha20Poly1305::new(discovery_key.into());
    aead.encrypt(
        nonce.into(),
        Payload {
            msg: plaintext,
            aad,
        },
    )
    // Encryption with a valid key+nonce does not fail for an in-bounds
    // plaintext; map defensively rather than panic.
    .map_err(|_| ScopedAnnouncementError::SealOpenFailed)
}

/// Open a sealed descriptor. Returns the descriptor plaintext, or
/// [`ScopedAnnouncementError::SealOpenFailed`] on a wrong key / wrong AD
/// (framing transplant) / tampered ciphertext — a single opaque failure so the
/// error is not a decryption oracle. The ciphertext is bounded to the plaintext
/// cap (+ tag) BEFORE any AEAD work so a peer cannot push the decoder past the
/// size bound.
pub fn open_descriptor(
    discovery_key: &[u8; 32],
    nonce: &[u8; SCOPED_ANN_NONCE_SIZE],
    aad: &[u8],
    ciphertext: &[u8],
) -> Result<Vec<u8>, ScopedAnnouncementError> {
    if ciphertext.len() < SCOPED_ANN_AEAD_TAG_SIZE {
        return Err(ScopedAnnouncementError::InvalidFormat);
    }
    let plaintext_len = ciphertext.len() - SCOPED_ANN_AEAD_TAG_SIZE;
    if plaintext_len > MAX_SCOPED_ANN_CIPHERTEXT_BYTES {
        return Err(ScopedAnnouncementError::DescriptorTooLarge {
            encoded: plaintext_len,
            maximum: MAX_SCOPED_ANN_CIPHERTEXT_BYTES,
        });
    }
    let aead = XChaCha20Poly1305::new(discovery_key.into());
    aead.decrypt(
        nonce.into(),
        Payload {
            msg: ciphertext,
            aad,
        },
    )
    .map_err(|_| ScopedAnnouncementError::SealOpenFailed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider() -> EntityId {
        // Any 32 bytes decode as an EntityId; the AD only reads its bytes.
        EntityId::from_bytes([7u8; 32])
    }

    fn owner_org() -> OrgId {
        OrgId::from_bytes([9u8; 32])
    }

    const KEY: [u8; 32] = [42u8; 32];
    const HANDLE: [u8; 32] = [1u8; 32];
    const GRANT: [u8; 32] = [2u8; 32];
    const NONCE: [u8; SCOPED_ANN_NONCE_SIZE] = [3u8; SCOPED_ANN_NONCE_SIZE];

    #[test]
    fn associated_data_layout_is_stable() {
        let aad = scoped_ann_associated_data(&provider(), &owner_org(), &HANDLE, &GRANT, 5, 9);
        // 4 * 32 (provider, owner, handle, grant) + 8 (gen) + 8 (expiry).
        assert_eq!(aad.len(), 144);
        assert_eq!(&aad[0..32], provider().as_bytes());
        assert_eq!(&aad[32..64], owner_org().as_bytes());
        assert_eq!(&aad[64..96], &HANDLE);
        assert_eq!(&aad[96..128], &GRANT);
        assert_eq!(&aad[128..136], &5u64.to_le_bytes());
        assert_eq!(&aad[136..144], &9u64.to_le_bytes());
    }

    #[test]
    fn owner_audience_ad_uses_the_reserved_zero_sentinel_and_is_disjoint_from_granted() {
        let owner = scoped_ann_associated_data(
            &provider(),
            &owner_org(),
            &HANDLE,
            &OWNER_AUDIENCE_GRANT_SENTINEL,
            5,
            9,
        );
        assert_eq!(
            &owner[96..128],
            &[0u8; 32],
            "owner AD carries the zero grant sentinel"
        );
        // A granted envelope with the same everything-else but a real grant id
        // produces a DIFFERENT AD, so a ciphertext cannot be replayed across the
        // owner/granted boundary under one key.
        let granted = scoped_ann_associated_data(&provider(), &owner_org(), &HANDLE, &GRANT, 5, 9);
        assert_ne!(owner, granted);
    }

    #[test]
    fn seal_open_round_trips() {
        let aad = scoped_ann_associated_data(&provider(), &owner_org(), &HANDLE, &GRANT, 5, 9);
        let plaintext = b"capability-descriptor-fragment";
        let ciphertext = seal_descriptor_with_nonce(&KEY, &NONCE, &aad, plaintext).expect("seal");
        assert_eq!(ciphertext.len(), plaintext.len() + SCOPED_ANN_AEAD_TAG_SIZE);
        assert_ne!(
            &ciphertext[..plaintext.len()],
            &plaintext[..],
            "plaintext is not in the clear"
        );
        let opened = open_descriptor(&KEY, &NONCE, &aad, &ciphertext).expect("open");
        assert_eq!(opened, plaintext);
    }

    #[test]
    fn open_with_wrong_key_fails() {
        let aad = scoped_ann_associated_data(&provider(), &owner_org(), &HANDLE, &GRANT, 5, 9);
        let ciphertext = seal_descriptor_with_nonce(&KEY, &NONCE, &aad, b"secret").expect("seal");
        let wrong_key = [43u8; 32];
        assert_eq!(
            open_descriptor(&wrong_key, &NONCE, &aad, &ciphertext),
            Err(ScopedAnnouncementError::SealOpenFailed)
        );
    }

    #[test]
    fn open_with_transplanted_aad_fails() {
        // Seal under a grant AD, try to open under a DIFFERENT grant id in the AD
        // (framing transplant) — the AEAD tag binds the framing, so it fails.
        let aad = scoped_ann_associated_data(&provider(), &owner_org(), &HANDLE, &GRANT, 5, 9);
        let ciphertext = seal_descriptor_with_nonce(&KEY, &NONCE, &aad, b"secret").expect("seal");
        let other_grant = [8u8; 32];
        let transplanted =
            scoped_ann_associated_data(&provider(), &owner_org(), &HANDLE, &other_grant, 5, 9);
        assert_eq!(
            open_descriptor(&KEY, &NONCE, &transplanted, &ciphertext),
            Err(ScopedAnnouncementError::SealOpenFailed)
        );
        // A generation bump in the AD also breaks the seal.
        let bumped = scoped_ann_associated_data(&provider(), &owner_org(), &HANDLE, &GRANT, 6, 9);
        assert_eq!(
            open_descriptor(&KEY, &NONCE, &bumped, &ciphertext),
            Err(ScopedAnnouncementError::SealOpenFailed)
        );
    }

    #[test]
    fn seal_rejects_oversized_descriptor() {
        let aad = scoped_ann_associated_data(&provider(), &owner_org(), &HANDLE, &GRANT, 5, 9);
        let oversized = vec![0u8; MAX_SCOPED_ANN_CIPHERTEXT_BYTES + 1];
        assert_eq!(
            seal_descriptor_with_nonce(&KEY, &NONCE, &aad, &oversized),
            Err(ScopedAnnouncementError::DescriptorTooLarge {
                encoded: MAX_SCOPED_ANN_CIPHERTEXT_BYTES + 1,
                maximum: MAX_SCOPED_ANN_CIPHERTEXT_BYTES,
            })
        );
        // Exactly at the cap seals fine.
        let at_cap = vec![0u8; MAX_SCOPED_ANN_CIPHERTEXT_BYTES];
        assert!(seal_descriptor_with_nonce(&KEY, &NONCE, &aad, &at_cap).is_ok());
    }

    #[test]
    fn open_rejects_oversized_and_truncated_ciphertext() {
        // A ciphertext implying a plaintext past the cap is refused before AEAD.
        let oversized = vec![0u8; MAX_SCOPED_ANN_CIPHERTEXT_BYTES + SCOPED_ANN_AEAD_TAG_SIZE + 1];
        assert!(matches!(
            open_descriptor(&KEY, &NONCE, b"", &oversized),
            Err(ScopedAnnouncementError::DescriptorTooLarge { .. })
        ));
        // A ciphertext shorter than the tag is malformed.
        assert_eq!(
            open_descriptor(&KEY, &NONCE, b"", &[0u8; SCOPED_ANN_AEAD_TAG_SIZE - 1]),
            Err(ScopedAnnouncementError::InvalidFormat)
        );
    }

    #[test]
    fn random_nonces_do_not_repeat() {
        let (n1, _) = seal_descriptor(&KEY, b"", b"x").expect("seal");
        let (n2, _) = seal_descriptor(&KEY, b"", b"x").expect("seal");
        assert_ne!(n1, n2, "fresh random nonce per seal");
    }
}
