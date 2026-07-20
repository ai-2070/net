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
use crate::adapter::net::identity::{EntityId, EntityKeypair};
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
    /// tag, a bad version byte, a length that does not add up, or an
    /// undecodable inline `owner_cert`).
    InvalidFormat,
    /// The envelope's OUTER provider signature did not verify over the exact
    /// encoded bytes. A single opaque variant — never a per-field oracle.
    SignatureInvalid,
    /// A GRANTED-audience envelope was asked to carry the reserved all-zero
    /// grant id (which is exclusively the OWNER-audience sentinel). Distinct
    /// builders prevent the two audiences from ever being confused.
    ReservedGrantId,
    /// The provider keypair cannot sign (it is public-only), so the outer
    /// signature could not be produced. Surfaced instead of panicking.
    SigningUnavailable,
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
                f.write_str("scoped-announcement encoding is malformed")
            }
            ScopedAnnouncementError::SignatureInvalid => {
                f.write_str("scoped-announcement outer provider signature did not verify")
            }
            ScopedAnnouncementError::ReservedGrantId => f.write_str(
                "a granted scoped announcement cannot use the reserved zero grant id (owner sentinel)",
            ),
            ScopedAnnouncementError::SigningUnavailable => {
                f.write_str("scoped-announcement provider keypair cannot sign (public-only)")
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

/// Wire-format version byte at the head of a serialized
/// [`ScopedCapabilityAnnouncement`]. Producers always emit `1`; decoders reject
/// any other value — there is no fallback probing (a format change bumps this
/// byte and [`SCOPED_ANN_SIG_DOMAIN`] together).
pub const SCOPED_ANN_WIRE_VERSION: u8 = 1;

/// Length of the encoded envelope up to and including the `ciphertext_len`
/// field — every fixed field before the variable ciphertext. The ciphertext and
/// the trailing 64-byte signature follow.
const SCOPED_ANN_PREFIX_LEN: usize = 1        // version
    + 32                                       // provider
    + 32                                       // owner_org
    + OrgMembershipCert::WIRE_SIZE             // owner_cert (156)
    + 32                                       // audience_handle
    + 32                                       // grant_id
    + 8                                        // generation (LE u64)
    + 8                                        // expires_at (LE u64)
    + SCOPED_ANN_NONCE_SIZE                    // nonce (24)
    + 2; // ciphertext_len (LE u16)

// The prefix plus the 64-byte signature is exactly the fixed overhead used in
// OA3-1b to derive the plaintext cap — the two derivations must agree.
const _: () = assert!(
    SCOPED_ANN_PREFIX_LEN + 64 == SCOPED_ANN_FIXED_OVERHEAD,
    "envelope prefix + signature must equal the fixed overhead"
);

/// An **outer-signature-authenticated** grant-scoped capability announcement
/// (§3.1). The descriptor is sealed to exactly one audience under the AEAD of
/// OA3-1b; the cleartext framing that routes the envelope is bound into that
/// seal as associated data AND signed by the publishing provider P.
///
/// # Verified-by-construction invariant
///
/// Holding a value of this type means P's ed25519 signature over the exact
/// encoded bytes (every field but the signature) verified — either we just
/// built and signed it, or [`Self::from_bytes`] verified it. There is NO public
/// constructor that skips signature verification, so an unverified value can
/// never be confused with a verified one. It does NOT mean the inline
/// `owner_cert`, revocation floors, freshness, or the sealed descriptor have
/// been checked — those are ingest-authority concerns (OA3-3), performed via the
/// accessors and [`Self::open_with`] below.
#[derive(Clone)]
pub struct ScopedCapabilityAnnouncement {
    provider: EntityId,
    owner_org: OrgId,
    owner_cert: OrgMembershipCert,
    audience_handle: [u8; 32],
    grant_id: [u8; 32],
    generation: u64,
    expires_at: u64,
    nonce: [u8; SCOPED_ANN_NONCE_SIZE],
    ciphertext: Vec<u8>,
    signature: [u8; 64],
}

impl ScopedCapabilityAnnouncement {
    /// Build, seal, and sign an **owner-audience** envelope: `grant_id` is fixed
    /// to the reserved zero sentinel ([`OWNER_AUDIENCE_GRANT_SENTINEL`]), both in
    /// the envelope and in the AEAD associated data. `provider_keypair` is P's
    /// entity key (it becomes `provider` and signs the outer signature);
    /// `owner_discovery_key` is the owner audience's decryption key.
    #[allow(clippy::too_many_arguments)]
    pub fn build_owner(
        provider_keypair: &EntityKeypair,
        owner_org: OrgId,
        owner_cert: OrgMembershipCert,
        audience_handle: [u8; 32],
        owner_discovery_key: &[u8; 32],
        generation: u64,
        expires_at: u64,
        descriptor: &[u8],
    ) -> Result<Self, ScopedAnnouncementError> {
        Self::build_sealed(
            provider_keypair,
            owner_org,
            owner_cert,
            audience_handle,
            OWNER_AUDIENCE_GRANT_SENTINEL,
            owner_discovery_key,
            generation,
            expires_at,
            descriptor,
        )
    }

    /// Build, seal, and sign a **granted-audience** envelope. Rejects the
    /// reserved zero `grant_id` with [`ScopedAnnouncementError::ReservedGrantId`]
    /// so a granted envelope can never masquerade as an owner one.
    #[allow(clippy::too_many_arguments)]
    pub fn build_granted(
        provider_keypair: &EntityKeypair,
        owner_org: OrgId,
        owner_cert: OrgMembershipCert,
        grant_id: [u8; 32],
        audience_handle: [u8; 32],
        discovery_key: &[u8; 32],
        generation: u64,
        expires_at: u64,
        descriptor: &[u8],
    ) -> Result<Self, ScopedAnnouncementError> {
        if grant_id == OWNER_AUDIENCE_GRANT_SENTINEL {
            return Err(ScopedAnnouncementError::ReservedGrantId);
        }
        Self::build_sealed(
            provider_keypair,
            owner_org,
            owner_cert,
            audience_handle,
            grant_id,
            discovery_key,
            generation,
            expires_at,
            descriptor,
        )
    }

    /// Common build path: seal (enforcing the plaintext cap) then sign.
    #[allow(clippy::too_many_arguments)]
    fn build_sealed(
        provider_keypair: &EntityKeypair,
        owner_org: OrgId,
        owner_cert: OrgMembershipCert,
        audience_handle: [u8; 32],
        grant_id: [u8; 32],
        discovery_key: &[u8; 32],
        generation: u64,
        expires_at: u64,
        descriptor: &[u8],
    ) -> Result<Self, ScopedAnnouncementError> {
        let provider = provider_keypair.entity_id().clone();
        let aad = scoped_ann_associated_data(
            &provider,
            &owner_org,
            &audience_handle,
            &grant_id,
            generation,
            expires_at,
        );
        let (nonce, ciphertext) = seal_descriptor(discovery_key, &aad, descriptor)?;
        Self::finish(
            provider_keypair,
            provider,
            owner_org,
            owner_cert,
            audience_handle,
            grant_id,
            generation,
            expires_at,
            nonce,
            ciphertext,
        )
    }

    /// Common deterministic-nonce build hook for golden vectors: seal with a
    /// caller-supplied nonce (instead of a fresh random one) and sign. Shared by
    /// [`Self::build_owner_with_nonce`] and [`Self::build_granted_with_nonce`] so
    /// the two deterministic builders differ ONLY in their `grant_id` sentinel
    /// policy, never in sealing/signing logic. Test-only — real publication always
    /// uses a fresh random nonce via [`Self::build_owner`] / [`Self::build_granted`].
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    fn build_with_nonce(
        provider_keypair: &EntityKeypair,
        owner_org: OrgId,
        owner_cert: OrgMembershipCert,
        grant_id: [u8; 32],
        audience_handle: [u8; 32],
        discovery_key: &[u8; 32],
        generation: u64,
        expires_at: u64,
        nonce: [u8; SCOPED_ANN_NONCE_SIZE],
        descriptor: &[u8],
    ) -> Result<Self, ScopedAnnouncementError> {
        let provider = provider_keypair.entity_id().clone();
        let aad = scoped_ann_associated_data(
            &provider,
            &owner_org,
            &audience_handle,
            &grant_id,
            generation,
            expires_at,
        );
        let ciphertext = seal_descriptor_with_nonce(discovery_key, &nonce, &aad, descriptor)?;
        Self::finish(
            provider_keypair,
            provider,
            owner_org,
            owner_cert,
            audience_handle,
            grant_id,
            generation,
            expires_at,
            nonce,
            ciphertext,
        )
    }

    /// Deterministic OWNER-audience build hook (golden vectors): `grant_id` is
    /// fixed to the reserved zero sentinel, mirroring [`Self::build_owner`].
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn build_owner_with_nonce(
        provider_keypair: &EntityKeypair,
        owner_org: OrgId,
        owner_cert: OrgMembershipCert,
        audience_handle: [u8; 32],
        owner_discovery_key: &[u8; 32],
        generation: u64,
        expires_at: u64,
        nonce: [u8; SCOPED_ANN_NONCE_SIZE],
        descriptor: &[u8],
    ) -> Result<Self, ScopedAnnouncementError> {
        Self::build_with_nonce(
            provider_keypair,
            owner_org,
            owner_cert,
            OWNER_AUDIENCE_GRANT_SENTINEL,
            audience_handle,
            owner_discovery_key,
            generation,
            expires_at,
            nonce,
            descriptor,
        )
    }

    /// Deterministic GRANTED-audience build hook (golden vectors): rejects the
    /// reserved zero `grant_id`, mirroring [`Self::build_granted`].
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn build_granted_with_nonce(
        provider_keypair: &EntityKeypair,
        owner_org: OrgId,
        owner_cert: OrgMembershipCert,
        grant_id: [u8; 32],
        audience_handle: [u8; 32],
        discovery_key: &[u8; 32],
        generation: u64,
        expires_at: u64,
        nonce: [u8; SCOPED_ANN_NONCE_SIZE],
        descriptor: &[u8],
    ) -> Result<Self, ScopedAnnouncementError> {
        if grant_id == OWNER_AUDIENCE_GRANT_SENTINEL {
            return Err(ScopedAnnouncementError::ReservedGrantId);
        }
        Self::build_with_nonce(
            provider_keypair,
            owner_org,
            owner_cert,
            grant_id,
            audience_handle,
            discovery_key,
            generation,
            expires_at,
            nonce,
            descriptor,
        )
    }

    /// Assemble the struct and apply the outer signature. Fallible ONLY on a
    /// public-only provider keypair: the seal already bounded the plaintext (so
    /// the ciphertext fits the `u16` length prefix and the whole envelope fits
    /// [`MAX_SCOPED_ANN_ENCODED_BYTES`] by the const-asserted budget partition,
    /// checked here with debug asserts), but `try_sign` returns
    /// [`ScopedAnnouncementError::SigningUnavailable`] rather than panic if the
    /// keypair cannot sign (Kyra OA3 closure).
    #[allow(clippy::too_many_arguments)]
    fn finish(
        provider_keypair: &EntityKeypair,
        provider: EntityId,
        owner_org: OrgId,
        owner_cert: OrgMembershipCert,
        audience_handle: [u8; 32],
        grant_id: [u8; 32],
        generation: u64,
        expires_at: u64,
        nonce: [u8; SCOPED_ANN_NONCE_SIZE],
        ciphertext: Vec<u8>,
    ) -> Result<Self, ScopedAnnouncementError> {
        debug_assert!(ciphertext.len() <= u16::MAX as usize);
        debug_assert!(
            SCOPED_ANN_PREFIX_LEN + ciphertext.len() + 64 <= MAX_SCOPED_ANN_ENCODED_BYTES
        );
        let mut env = Self {
            provider,
            owner_org,
            owner_cert,
            audience_handle,
            grant_id,
            generation,
            expires_at,
            nonce,
            ciphertext,
            signature: [0u8; 64],
        };
        let signing_input = env.signing_input();
        // `try_sign` (not `sign`): these builders return `Result`, and
        // `EntityKeypair::sign` PANICS on a public-only keypair — surface it as a
        // typed error instead.
        env.signature = provider_keypair
            .try_sign(&signing_input)
            .map_err(|_| ScopedAnnouncementError::SigningUnavailable)?
            .to_bytes();
        Ok(env)
    }

    /// The encoded envelope WITHOUT the trailing signature — the exact bytes the
    /// outer signature covers (after the domain prefix). `ciphertext_len` is
    /// encoded as a little-endian `u16`; the value fits by construction and by
    /// the decoder's bound check.
    fn encode_unsigned(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(SCOPED_ANN_PREFIX_LEN + self.ciphertext.len());
        buf.push(SCOPED_ANN_WIRE_VERSION);
        buf.extend_from_slice(self.provider.as_bytes());
        buf.extend_from_slice(self.owner_org.as_bytes());
        buf.extend_from_slice(&self.owner_cert.to_bytes());
        buf.extend_from_slice(&self.audience_handle);
        buf.extend_from_slice(&self.grant_id);
        buf.extend_from_slice(&self.generation.to_le_bytes());
        buf.extend_from_slice(&self.expires_at.to_le_bytes());
        buf.extend_from_slice(&self.nonce);
        let ct_len = self.ciphertext.len() as u16;
        buf.extend_from_slice(&ct_len.to_le_bytes());
        buf.extend_from_slice(&self.ciphertext);
        buf
    }

    /// The domain-prefixed message the outer signature signs/verifies:
    /// `SCOPED_ANN_SIG_DOMAIN ‖ encode_unsigned`.
    fn signing_input(&self) -> Vec<u8> {
        let unsigned = self.encode_unsigned();
        let mut buf = Vec::with_capacity(SCOPED_ANN_SIG_DOMAIN.len() + unsigned.len());
        buf.extend_from_slice(SCOPED_ANN_SIG_DOMAIN);
        buf.extend_from_slice(&unsigned);
        buf
    }

    /// Serialize to canonical wire bytes: `encode_unsigned ‖ signature`.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = self.encode_unsigned();
        buf.extend_from_slice(&self.signature);
        buf
    }

    /// Decode and AUTHENTICATE an envelope from wire bytes. Order (per §3.1 and
    /// review): structural decode + bounds (with checked length arithmetic —
    /// `prefix + ciphertext_len + signature == input.len()`), THEN the outer
    /// provider signature is verified LAST; only a value whose signature checks
    /// out is returned. Does NOT open the AEAD or check the cert/floors — that is
    /// ingest (OA3-3).
    #[expect(
        clippy::unwrap_used,
        reason = "every fixed-offset slice is length-checked before the infallible array conversion"
    )]
    pub fn from_bytes(data: &[u8]) -> Result<Self, ScopedAnnouncementError> {
        // Whole-envelope bound first — reject an oversized frame before parsing.
        if data.len() > MAX_SCOPED_ANN_ENCODED_BYTES {
            return Err(ScopedAnnouncementError::DescriptorTooLarge {
                encoded: data.len(),
                maximum: MAX_SCOPED_ANN_ENCODED_BYTES,
            });
        }
        if data.len() < SCOPED_ANN_PREFIX_LEN + 64 {
            return Err(ScopedAnnouncementError::InvalidFormat);
        }
        if data[0] != SCOPED_ANN_WIRE_VERSION {
            return Err(ScopedAnnouncementError::InvalidFormat);
        }
        let mut off = 1;
        let provider = EntityId::from_bytes(data[off..off + 32].try_into().unwrap());
        off += 32;
        let owner_org = OrgId::from_bytes(data[off..off + 32].try_into().unwrap());
        off += 32;
        let owner_cert =
            OrgMembershipCert::from_bytes(&data[off..off + OrgMembershipCert::WIRE_SIZE])
                .map_err(|_| ScopedAnnouncementError::InvalidFormat)?;
        off += OrgMembershipCert::WIRE_SIZE;
        let audience_handle: [u8; 32] = data[off..off + 32].try_into().unwrap();
        off += 32;
        let grant_id: [u8; 32] = data[off..off + 32].try_into().unwrap();
        off += 32;
        let generation = u64::from_le_bytes(data[off..off + 8].try_into().unwrap());
        off += 8;
        let expires_at = u64::from_le_bytes(data[off..off + 8].try_into().unwrap());
        off += 8;
        let nonce: [u8; SCOPED_ANN_NONCE_SIZE] =
            data[off..off + SCOPED_ANN_NONCE_SIZE].try_into().unwrap();
        off += SCOPED_ANN_NONCE_SIZE;
        let ct_len = u16::from_le_bytes(data[off..off + 2].try_into().unwrap()) as usize;
        off += 2;
        debug_assert_eq!(off, SCOPED_ANN_PREFIX_LEN);
        // Checked length arithmetic: prefix + ciphertext + signature must equal
        // the input EXACTLY — no trailing bytes, no truncation, no overflow.
        let expected_total = SCOPED_ANN_PREFIX_LEN
            .checked_add(ct_len)
            .and_then(|x| x.checked_add(64))
            .ok_or(ScopedAnnouncementError::InvalidFormat)?;
        if expected_total != data.len() {
            return Err(ScopedAnnouncementError::InvalidFormat);
        }
        // A valid AEAD ciphertext is at least the tag; anything shorter is
        // structurally malformed.
        if ct_len < SCOPED_ANN_AEAD_TAG_SIZE {
            return Err(ScopedAnnouncementError::InvalidFormat);
        }
        // Plaintext-side bound (mirrors the builder).
        if ct_len - SCOPED_ANN_AEAD_TAG_SIZE > MAX_SCOPED_ANN_CIPHERTEXT_BYTES {
            return Err(ScopedAnnouncementError::DescriptorTooLarge {
                encoded: ct_len - SCOPED_ANN_AEAD_TAG_SIZE,
                maximum: MAX_SCOPED_ANN_CIPHERTEXT_BYTES,
            });
        }
        let ciphertext = data[off..off + ct_len].to_vec();
        off += ct_len;
        let mut signature = [0u8; 64];
        signature.copy_from_slice(&data[off..off + 64]);
        let env = Self {
            provider,
            owner_org,
            owner_cert,
            audience_handle,
            grant_id,
            generation,
            expires_at,
            nonce,
            ciphertext,
            signature,
        };
        // Outer signature LAST — a value is returned ONLY if P's signature over
        // the exact encoded bytes verifies. A transplanted owner_cert / nonce /
        // ct_len / ciphertext all fail here.
        env.provider
            .verify_bytes(&env.signing_input(), &env.signature)
            .map_err(|_| ScopedAnnouncementError::SignatureInvalid)?;
        Ok(env)
    }

    /// The publishing provider P (also the outer-signature verifier).
    pub fn provider(&self) -> &EntityId {
        &self.provider
    }
    /// The org P claims to belong to (bound by the inline `owner_cert`, checked
    /// at ingest).
    pub fn owner_org(&self) -> &OrgId {
        &self.owner_org
    }
    /// The inline membership certificate proving P ∈ `owner_org` (validity /
    /// floors checked at ingest, OA3-3).
    pub fn owner_cert(&self) -> &OrgMembershipCert {
        &self.owner_cert
    }
    /// The audience routing handle this envelope is sealed to.
    pub fn audience_handle(&self) -> &[u8; 32] {
        &self.audience_handle
    }
    /// The grant id — the reserved zero sentinel for an owner-audience envelope,
    /// a real grant id for a granted one.
    pub fn grant_id(&self) -> &[u8; 32] {
        &self.grant_id
    }
    /// Monotonic announcement generation (freshness / dedup).
    pub fn generation(&self) -> u64 {
        self.generation
    }
    /// Envelope expiry (unix seconds).
    pub fn expires_at(&self) -> u64 {
        self.expires_at
    }
    /// The AEAD nonce.
    pub fn nonce(&self) -> &[u8; SCOPED_ANN_NONCE_SIZE] {
        &self.nonce
    }
    /// The sealed descriptor ciphertext (opaque; includes the AEAD tag).
    pub fn ciphertext(&self) -> &[u8] {
        &self.ciphertext
    }
    /// True iff this is an owner-audience envelope (grant id is the reserved zero
    /// sentinel).
    pub fn is_owner_audience(&self) -> bool {
        self.grant_id == OWNER_AUDIENCE_GRANT_SENTINEL
    }

    /// The AEAD associated data binding this envelope's framing, recomputed from
    /// the authenticated fields — passed to [`open_descriptor`] at ingest.
    pub fn associated_data(&self) -> Vec<u8> {
        scoped_ann_associated_data(
            &self.provider,
            &self.owner_org,
            &self.audience_handle,
            &self.grant_id,
            self.generation,
            self.expires_at,
        )
    }

    /// Open the sealed descriptor with an audience `discovery_key`. The caller
    /// (OA3-3) is responsible for having established that this key belongs to
    /// this envelope's audience (the owner credential, or an installed grant
    /// secret whose commitment matches the signed grant). Returns the descriptor
    /// plaintext, or an opaque failure that is not a decryption oracle.
    pub fn open_with(&self, discovery_key: &[u8; 32]) -> Result<Vec<u8>, ScopedAnnouncementError> {
        open_descriptor(
            discovery_key,
            &self.nonce,
            &self.associated_data(),
            &self.ciphertext,
        )
    }
}

impl std::fmt::Debug for ScopedCapabilityAnnouncement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScopedCapabilityAnnouncement")
            .field("provider", &self.provider)
            .field("owner_org", &self.owner_org)
            .field("grant_id", &hex::encode(&self.grant_id[..8]))
            .field("audience_handle", &hex::encode(&self.audience_handle[..8]))
            .field("generation", &self.generation)
            .field("expires_at", &self.expires_at)
            .field("ciphertext_len", &self.ciphertext.len())
            .finish()
    }
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

    // ---------------------------------------------------------------------
    // OA3-2 — ScopedCapabilityAnnouncement envelope
    // ---------------------------------------------------------------------

    use super::super::org::OrgKeypair;

    fn provider_keypair() -> EntityKeypair {
        EntityKeypair::from_bytes([11u8; 32])
    }

    fn owner_keypair() -> OrgKeypair {
        OrgKeypair::from_bytes([22u8; 32])
    }

    /// A deterministic membership cert (fixed window + nonce) binding `member`
    /// to `org` — so the whole envelope is byte-reproducible for golden vectors.
    fn deterministic_cert(org: &OrgKeypair, member: &EntityId) -> OrgMembershipCert {
        OrgMembershipCert::issue_at(org, member.clone(), 3, 1_000, 1_000_000, 0xABCD)
    }

    #[test]
    fn granted_envelope_round_trips_verifies_and_opens() {
        let pk = provider_keypair();
        let org = owner_keypair();
        let cert = deterministic_cert(&org, pk.entity_id());
        let key = [42u8; 32];
        let env = ScopedCapabilityAnnouncement::build_granted(
            &pk,
            org.org_id(),
            cert,
            GRANT,
            HANDLE,
            &key,
            7,
            1_234,
            b"descriptor-bytes",
        )
        .expect("build");
        assert!(!env.is_owner_audience());

        let bytes = env.to_bytes();
        assert_eq!(
            bytes.len(),
            SCOPED_ANN_PREFIX_LEN + env.ciphertext().len() + 64
        );

        let decoded = ScopedCapabilityAnnouncement::from_bytes(&bytes).expect("decode + verify");
        assert_eq!(decoded.provider(), pk.entity_id());
        assert_eq!(decoded.owner_org(), &org.org_id());
        assert_eq!(decoded.grant_id(), &GRANT);
        assert_eq!(decoded.generation(), 7);
        assert_eq!(decoded.expires_at(), 1_234);
        assert_eq!(decoded.open_with(&key).expect("open"), b"descriptor-bytes");
    }

    #[test]
    fn owner_envelope_uses_the_zero_sentinel_and_opens() {
        let pk = provider_keypair();
        let org = owner_keypair();
        let cert = deterministic_cert(&org, pk.entity_id());
        let key = [7u8; 32];
        let env = ScopedCapabilityAnnouncement::build_owner(
            &pk,
            org.org_id(),
            cert,
            HANDLE,
            &key,
            1,
            99,
            b"owner-descriptor",
        )
        .expect("build owner");
        assert!(env.is_owner_audience());
        assert_eq!(env.grant_id(), &OWNER_AUDIENCE_GRANT_SENTINEL);

        let decoded =
            ScopedCapabilityAnnouncement::from_bytes(&env.to_bytes()).expect("decode + verify");
        assert!(decoded.is_owner_audience());
        assert_eq!(decoded.open_with(&key).expect("open"), b"owner-descriptor");
    }

    #[test]
    fn build_granted_rejects_the_reserved_sentinel() {
        let pk = provider_keypair();
        let org = owner_keypair();
        let cert = deterministic_cert(&org, pk.entity_id());
        let err = ScopedCapabilityAnnouncement::build_granted(
            &pk,
            org.org_id(),
            cert,
            OWNER_AUDIENCE_GRANT_SENTINEL,
            HANDLE,
            &[1u8; 32],
            1,
            1,
            b"x",
        )
        .expect_err("granted with the owner sentinel must be refused");
        assert_eq!(err, ScopedAnnouncementError::ReservedGrantId);
    }

    #[test]
    fn tampering_any_signed_field_fails_the_outer_signature() {
        let pk = provider_keypair();
        let org = owner_keypair();
        let cert = deterministic_cert(&org, pk.entity_id());
        let env = ScopedCapabilityAnnouncement::build_granted(
            &pk,
            org.org_id(),
            cert,
            GRANT,
            HANDLE,
            &[42u8; 32],
            7,
            1_234,
            b"descriptor-bytes",
        )
        .expect("build");
        let good = env.to_bytes();
        assert!(ScopedCapabilityAnnouncement::from_bytes(&good).is_ok());

        // One representative byte in each signed field (per the encode layout).
        let provider_off = 1;
        let owner_cert_off = 1 + 32 + 32;
        let handle_off = owner_cert_off + OrgMembershipCert::WIRE_SIZE;
        let grant_off = handle_off + 32;
        let generation_off = grant_off + 32;
        let nonce_off = generation_off + 8 + 8;
        let ciphertext_off = SCOPED_ANN_PREFIX_LEN;
        for off in [
            provider_off,
            owner_cert_off,
            handle_off,
            grant_off,
            generation_off,
            nonce_off,
            ciphertext_off,
        ] {
            let mut tampered = good.clone();
            tampered[off] ^= 0x01;
            assert_eq!(
                ScopedCapabilityAnnouncement::from_bytes(&tampered).unwrap_err(),
                ScopedAnnouncementError::SignatureInvalid,
                "flipping a byte at offset {off} must fail the outer signature",
            );
        }
        // The signature itself is likewise load-bearing.
        let mut sig_tampered = good.clone();
        let sig_off = SCOPED_ANN_PREFIX_LEN + env.ciphertext().len();
        sig_tampered[sig_off] ^= 0x01;
        assert_eq!(
            ScopedCapabilityAnnouncement::from_bytes(&sig_tampered).unwrap_err(),
            ScopedAnnouncementError::SignatureInvalid,
        );
    }

    #[test]
    fn decode_rejects_bad_version_length_and_bounds() {
        let pk = provider_keypair();
        let org = owner_keypair();
        let cert = deterministic_cert(&org, pk.entity_id());
        let env = ScopedCapabilityAnnouncement::build_granted(
            &pk,
            org.org_id(),
            cert,
            GRANT,
            HANDLE,
            &[42u8; 32],
            1,
            1,
            b"desc",
        )
        .expect("build");
        let good = env.to_bytes();

        let mut bad_version = good.clone();
        bad_version[0] = 2;
        assert_eq!(
            ScopedCapabilityAnnouncement::from_bytes(&bad_version).unwrap_err(),
            ScopedAnnouncementError::InvalidFormat
        );

        // Truncated by one byte (length arithmetic no longer adds up).
        assert_eq!(
            ScopedCapabilityAnnouncement::from_bytes(&good[..good.len() - 1]).unwrap_err(),
            ScopedAnnouncementError::InvalidFormat
        );

        // A trailing byte is rejected (no partial reads).
        let mut trailing = good.clone();
        trailing.push(0);
        assert_eq!(
            ScopedCapabilityAnnouncement::from_bytes(&trailing).unwrap_err(),
            ScopedAnnouncementError::InvalidFormat
        );

        // Shorter than the fixed prefix + signature.
        assert_eq!(
            ScopedCapabilityAnnouncement::from_bytes(&[SCOPED_ANN_WIRE_VERSION; 10]).unwrap_err(),
            ScopedAnnouncementError::InvalidFormat
        );

        // Oversized frame is refused before parsing.
        let huge = vec![0u8; MAX_SCOPED_ANN_ENCODED_BYTES + 1];
        assert!(matches!(
            ScopedCapabilityAnnouncement::from_bytes(&huge),
            Err(ScopedAnnouncementError::DescriptorTooLarge { .. })
        ));
    }

    #[test]
    fn golden_vector_pins_the_full_encoded_envelope() {
        // Fully deterministic: fixed entity/org seeds, fixed cert window+nonce,
        // fixed AEAD nonce, deterministic ed25519 — so the ENTIRE encoded
        // envelope (framing + ciphertext + outer signature) is byte-stable.
        let pk = provider_keypair();
        let org = owner_keypair();
        let cert = deterministic_cert(&org, pk.entity_id());
        let key = [42u8; 32];
        let env = ScopedCapabilityAnnouncement::build_granted_with_nonce(
            &pk,
            org.org_id(),
            cert,
            GRANT,
            HANDLE,
            &key,
            7,
            1_234,
            NONCE,
            b"golden-descriptor",
        )
        .expect("build");
        let encoded = hex::encode(env.to_bytes());
        assert_eq!(encoded, GOLDEN_ENVELOPE_HEX, "GOLDEN={encoded}");

        // And the pinned bytes decode, verify, and open.
        let decoded = ScopedCapabilityAnnouncement::from_bytes(
            &hex::decode(GOLDEN_ENVELOPE_HEX).expect("golden hex"),
        )
        .expect("decode + verify golden");
        assert_eq!(decoded.open_with(&key).expect("open"), b"golden-descriptor");
    }

    // Pinned by the deterministic build above (captured once, then frozen). Any
    // change to the wire layout, a field width, the signing domain, or the AEAD
    // construction moves these bytes — the whole encoded envelope, including the
    // deterministic ed25519 outer signature, is locked here.
    const GOLDEN_ENVELOPE_HEX: &str = "0166be7e332c7a453332bd9d0a7f7db055f5c5ef1a06ada66d98b39fb6810c473a511c34a1a2cb521df16bb246b8de8e7997ce235c7e76b22a3d7503a24819dd8a511c34a1a2cb521df16bb246b8de8e7997ce235c7e76b22a3d7503a24819dd8a66be7e332c7a453332bd9d0a7f7db055f5c5ef1a06ada66d98b39fb6810c473ae80300000000000040420f000000000003000000cdab000000000000e86638ebfcdd62b5b94bcf3b15f78be4f33ee0a4f7cbd5713a06a88fd5df42d129c550d2076eefff949ac948407db797229f3ee0c2e116d6049eb7ea13629c04010101010101010101010101010101010101010101010101010101010101010102020202020202020202020202020202020202020202020202020202020202020700000000000000d2040000000000000303030303030303030303030303030303030303030303032100a7eee1241f24f2fa061e532b393aaf8aa5b73a76eef9afcd640df692b04e159d8740e97ea293f952f8fae84542d911648555f96846ad269115031cd46c1537ee36f78ca97f55f4d1e855708ace5e9453a9570228523ec61fa12cc50fdb8db3b409";

    /// OA3-6 exit gate (§3.5 "golden vectors incl. the zero-sentinel owner AD"):
    /// a frozen OWNER-audience envelope. `grant_id` is the all-zero sentinel,
    /// bound into the AEAD associated data; the whole encoded envelope — framing,
    /// zero-sentinel grant id, ciphertext, and deterministic ed25519 outer
    /// signature — is byte-locked here.
    #[test]
    fn owner_golden_vector_pins_the_zero_sentinel_envelope() {
        let pk = provider_keypair();
        let org = owner_keypair();
        let cert = deterministic_cert(&org, pk.entity_id());
        let owner_key = [7u8; 32];
        let env = ScopedCapabilityAnnouncement::build_owner_with_nonce(
            &pk,
            org.org_id(),
            cert,
            HANDLE,
            &owner_key,
            1,
            99,
            NONCE,
            b"owner-golden-descriptor",
        )
        .expect("build owner");
        let encoded = hex::encode(env.to_bytes());
        assert_eq!(encoded, OWNER_GOLDEN_ENVELOPE_HEX, "OWNER_GOLDEN={encoded}");

        // The pinned bytes decode + outer-verify, are the owner audience, carry
        // the all-zero grant sentinel, and open under the owner key.
        let bytes = hex::decode(OWNER_GOLDEN_ENVELOPE_HEX).expect("golden hex");
        let decoded =
            ScopedCapabilityAnnouncement::from_bytes(&bytes).expect("decode + verify owner golden");
        assert!(decoded.is_owner_audience());
        assert_eq!(decoded.grant_id(), &OWNER_AUDIENCE_GRANT_SENTINEL);
        // The grant-id field is all-zero in the ENCODED bytes too (offset after
        // version + provider + owner_org + owner_cert + audience_handle).
        let grant_id_off = 1 + 32 + 32 + OrgMembershipCert::WIRE_SIZE + 32;
        assert_eq!(&bytes[grant_id_off..grant_id_off + 32], &[0u8; 32]);
        assert_eq!(
            decoded.open_with(&owner_key).expect("open"),
            b"owner-golden-descriptor"
        );
    }

    /// Frozen by the deterministic owner build above — the OWNER counterpart to
    /// [`GOLDEN_ENVELOPE_HEX`], pinning the zero-sentinel AD encoding end to end.
    const OWNER_GOLDEN_ENVELOPE_HEX: &str = "0166be7e332c7a453332bd9d0a7f7db055f5c5ef1a06ada66d98b39fb6810c473a511c34a1a2cb521df16bb246b8de8e7997ce235c7e76b22a3d7503a24819dd8a511c34a1a2cb521df16bb246b8de8e7997ce235c7e76b22a3d7503a24819dd8a66be7e332c7a453332bd9d0a7f7db055f5c5ef1a06ada66d98b39fb6810c473ae80300000000000040420f000000000003000000cdab000000000000e86638ebfcdd62b5b94bcf3b15f78be4f33ee0a4f7cbd5713a06a88fd5df42d129c550d2076eefff949ac948407db797229f3ee0c2e116d6049eb7ea13629c0401010101010101010101010101010101010101010101010101010101010101010000000000000000000000000000000000000000000000000000000000000000010000000000000063000000000000000303030303030303030303030303030303030303030303032700eb71257e070ca19f1b7426956ab5ab3a49e81917777a448abfafb33a633f75f842f096421b62dddda47b880244aa5f99f5a4d94fbdb94b57ee7d37e026077bf6872b2a4b7ccba39d3c5f7e3dfbd79a2f819650beea2e210eb81732d189814d902bcf29005bbf00";
}
