//! Fold-layer wire envelope + codec.
//!
//! Defines the on-the-wire [`SignedAnnouncement<P>`] shape (one
//! per fold-channel emission, generic over the per-fold
//! [`FoldKind::Payload`](super::FoldKind::Payload)) and the
//! postcard codec that encodes / decodes / signs / verifies it.
//!
//! The on-wire form is postcard. The signing form — the bytes the
//! Ed25519 signature commits to — is a separate postcard
//! serialization of every field EXCEPT the signature itself, in
//! field-declared order. Keeping the two forms distinct lets the
//! verifier reconstruct the signing bytes from a received
//! envelope without re-encoding the signature into them.
//!
//! Postcard is chosen because it's field-deterministic (struct
//! `#[derive(Serialize)]` emits fields in declaration order,
//! stable across builds), imposes no length-prefix tax on
//! fixed-width fields, and is already a workspace dependency
//! (meshdb, RedEX disk format).
//!
//! The verifier rejects: signatures whose length is not
//! [`SIGNATURE_LEN`]; the all-zero [`placeholder_signature`]
//! sentinel (placeholder envelopes have no business reaching
//! dispatch); and tampered envelopes (the signature won't verify
//! against the recomputed signing bytes). See
//! `docs/plans/SCALING_MULTIFOLD_PLAN.md` § Wire format for the
//! authoritative field semantics and on-wire ordering.

use serde::{de::DeserializeOwned, Deserialize, Serialize};

use super::state::NodeId;
use super::FoldError;

/// Ed25519 signature size in bytes (64). The on-wire signature
/// is a fixed-length slice stored as a `Vec<u8>` so the derived
/// `Serialize`/`Deserialize` impls work without a
/// `serde-big-array` dependency.
pub const SIGNATURE_LEN: usize = 64;

/// Per-envelope metadata grouped to keep the `sign` /
/// `placeholder` constructor signatures narrow. All three fields
/// are wire-envelope members; defaults match the most common
/// publisher pattern (current wall-clock micros, default TTL via
/// `FoldKind::DEFAULT_TTL`, no flag bits set).
#[derive(Debug, Clone, Copy, Default)]
pub struct EnvelopeMeta {
    /// Publisher's wall-clock micros-since-epoch at emission.
    /// Receivers use this for diagnostics, not for ordering —
    /// `generation` is the load-bearing anti-reorder signal.
    pub announced_at: u64,
    /// Per-announcement TTL override. `None` falls through to
    /// [`super::FoldKind::DEFAULT_TTL`].
    pub ttl_secs: Option<u32>,
    /// Bit flags. See [`SignedAnnouncement::flags`] for the
    /// reserved layout.
    pub flags: u8,
}

/// Sentinel signature bytes — all-zero, [`SIGNATURE_LEN`] wide.
/// The verifier rejects this unconditionally; it carries the
/// "envelope is well-formed, signature is a placeholder" marker
/// through tests and synthetic in-process producers that don't
/// have a keypair handy.
pub fn placeholder_signature() -> Vec<u8> {
    vec![0u8; SIGNATURE_LEN]
}

/// One signed announcement on a fold channel. The `P` parameter
/// is the per-fold payload type
/// ([`super::FoldKind::Payload`]).
///
/// Postcard-encoded with field-ordered structs; the signature is
/// Ed25519 over the canonical encoding of every other field.
/// `subnet_id` is intentionally NOT a member here — it lives on
/// the underlying `NetHeader.subnet_id` so the wire envelope and
/// the header don't carry duplicate scoping state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedAnnouncement<P> {
    /// Fold this announcement targets — [`super::FoldKind::KIND_ID`].
    /// The dispatch layer routes on this and rejects announcements
    /// whose `kind` is unregistered.
    pub kind: u16,
    /// Class within the fold. For capability, this is the
    /// capability-class hash; for routing, a tier identifier;
    /// for reservation, a pool identifier. The fold's channel
    /// name is derived as
    /// `format!("{}{}", FoldKind::CHANNEL_PREFIX, class)` —
    /// subscribers either subscribe to a per-class channel
    /// (default) or to a fold-wide channel and filter on
    /// `class` at the matcher.
    pub class: u64,
    /// Publisher of this announcement. The
    /// [`SignedAnnouncement::signature`] commits to the
    /// publisher's cryptographic identity; the routing-layer
    /// `node_id` here is what folds index against.
    pub node_id: NodeId,
    /// Monotonic per-`(node_id, kind, class)` counter. The
    /// default [`super::FoldKind::merge`] orders applies on
    /// this; the publisher persists it across restarts so the
    /// sequence never goes backward. `0` is reserved as an
    /// "uninitialized" sentinel — see
    /// [`super::state::FoldError::InvalidGeneration`].
    pub generation: u64,
    /// Publisher's local micros-since-epoch at emission. Used by
    /// metrics + diagnostics; NOT consulted for ordering — the
    /// `generation` field is the load-bearing ordering signal.
    pub announced_at: u64,
    /// Per-announcement TTL override. `None` falls through to
    /// [`super::FoldKind::DEFAULT_TTL`].
    pub ttl_secs: Option<u32>,
    /// Bit flags. The reserved layout is:
    ///
    /// - bit 0: join (new membership).
    /// - bit 1: leave (publisher is voluntarily releasing the key).
    /// - bit 2: update (in-place mutation of existing entry).
    /// - bits 3..7: reserved for future use, must be zero.
    ///
    /// Folds free to ignore flags they don't recognize.
    pub flags: u8,
    /// Domain-specific payload — the actual data the fold cares
    /// about. Owned, not borrowed: the runtime moves it into the
    /// [`super::state::FoldEntry::payload`] field on accept.
    pub payload: P,
    /// Ed25519 signature over the canonical encoding of every
    /// other field. Stored as a `Vec<u8>` of length
    /// [`SIGNATURE_LEN`] so the derived serde impls work without
    /// a fixed-array codec dependency.
    pub signature: Vec<u8>,
}

impl<P> SignedAnnouncement<P> {
    /// Construct an announcement with the
    /// [`placeholder_signature`] sentinel. Tests and in-process
    /// producers that don't have a keypair handy use this; the
    /// dispatch layer rejects placeholder-stamped envelopes on
    /// the slow path.
    pub fn placeholder(
        kind: u16,
        class: u64,
        node_id: NodeId,
        generation: u64,
        meta: EnvelopeMeta,
        payload: P,
    ) -> Self {
        Self {
            kind,
            class,
            node_id,
            generation,
            announced_at: meta.announced_at,
            ttl_secs: meta.ttl_secs,
            flags: meta.flags,
            payload,
            signature: placeholder_signature(),
        }
    }
}

/// Errors the wire codec surfaces. The dispatch layer routes them
/// to logs + metrics; the caller of [`SignedAnnouncement::decode`]
/// sees them via `Result`.
#[derive(Debug, thiserror::Error)]
pub enum WireError {
    /// Postcard refused to decode the byte buffer (truncated,
    /// schema-incompatible, etc.).
    #[error("wire decode failed: {0}")]
    Decode(#[from] postcard::Error),

    /// The signature on a decoded envelope has the wrong length.
    /// Production signatures are exactly [`SIGNATURE_LEN`] bytes
    /// (Ed25519); anything else is malformed by construction.
    #[error("signature length {0} != expected {expected}", expected = SIGNATURE_LEN)]
    BadSignatureLength(usize),

    /// Decoded envelope carries the [`placeholder_signature`]
    /// sentinel. Dispatch rejects this — the envelope was
    /// constructed without signing, so verification would be
    /// vacuous and the `node_id` claim is unauthenticated.
    #[error("placeholder (all-zero) signature reached the dispatch path")]
    PlaceholderSignature,

    /// Underlying Ed25519 verifier rejected the signature: the
    /// envelope was tampered with, or claims a publisher whose
    /// public key doesn't match the signing key.
    #[error("signature verification failed")]
    InvalidSignature,

    /// The publisher's `EntityId` bytes aren't a valid Ed25519
    /// public key (not on-curve / malformed encoding). Returned
    /// when the dispatch layer is handed an `EntityId` that
    /// didn't round-trip through a known publisher.
    #[error("publisher public key bytes are not a valid Ed25519 point")]
    InvalidPublicKey,

    /// The decoded envelope's `kind` field doesn't match the
    /// fold it was dispatched into. The dispatch layer catches
    /// this BEFORE handing the envelope to `Fold::apply` so a
    /// crossed-channel publish doesn't pollute the wrong fold.
    #[error("envelope kind {got:#06x} does not match expected {expected:#06x}")]
    KindMismatch {
        /// Kind field decoded from the envelope.
        got: u16,
        /// Kind the dispatch path was expecting.
        expected: u16,
    },

    /// An [`FoldError`] surfaced during the post-verify apply.
    /// Wraps the underlying error so callers can pattern-match
    /// on the apply-side failure modes.
    #[error("apply rejected: {0}")]
    Apply(#[from] FoldError),
}

/// Canonical bytes the Ed25519 signature commits to.
///
/// Postcard-encodes every field EXCEPT `signature` in the order
/// they appear on [`SignedAnnouncement`]. Field order is wire-
/// load-bearing: any future field addition appends to the end
/// and bumps the `kind` reservation (a new `KIND_ID` means a
/// new fold, which gets a fresh canonical ordering). Existing
/// folds never reorder.
///
/// The borrow on `payload` avoids cloning the (potentially large)
/// per-fold payload — the canonical bytes are computed inside
/// `sign` and `verify`, both of which can hold the reference for
/// the duration of the postcard call.
pub(super) fn signing_bytes<P: Serialize>(
    kind: u16,
    class: u64,
    node_id: NodeId,
    generation: u64,
    meta: &EnvelopeMeta,
    payload: &P,
) -> Result<Vec<u8>, postcard::Error> {
    // A separate struct rather than a tuple so postcard's serde
    // derive emits length-tagged fields in the right order. Field
    // ORDER here is load-bearing — it MUST match the
    // `SignedAnnouncement` field declaration order.
    #[derive(Serialize)]
    struct ToSign<'a, P: Serialize> {
        kind: u16,
        class: u64,
        node_id: NodeId,
        generation: u64,
        announced_at: u64,
        ttl_secs: Option<u32>,
        flags: u8,
        payload: &'a P,
    }
    postcard::to_allocvec(&ToSign {
        kind,
        class,
        node_id,
        generation,
        announced_at: meta.announced_at,
        ttl_secs: meta.ttl_secs,
        flags: meta.flags,
        payload,
    })
}

impl<P: Serialize + DeserializeOwned> SignedAnnouncement<P> {
    /// Construct + sign an announcement with the supplied
    /// keypair. The signature commits to every other field via
    /// the canonical `signing_bytes` byte layout (private to
    /// this module).
    pub fn sign(
        keypair: &crate::adapter::net::identity::EntityKeypair,
        kind: u16,
        class: u64,
        node_id: NodeId,
        generation: u64,
        meta: EnvelopeMeta,
        payload: P,
    ) -> Result<Self, WireError> {
        let bytes = signing_bytes(kind, class, node_id, generation, &meta, &payload)?;
        let sig = keypair.sign(&bytes);
        Ok(Self {
            kind,
            class,
            node_id,
            generation,
            announced_at: meta.announced_at,
            ttl_secs: meta.ttl_secs,
            flags: meta.flags,
            payload,
            signature: sig.to_bytes().to_vec(),
        })
    }

    /// Verify the signature against a publisher's
    /// [`EntityId`](crate::adapter::net::identity::EntityId).
    ///
    /// Rejects:
    /// - Wrong-length signatures
    ///   ([`WireError::BadSignatureLength`])
    /// - The placeholder sentinel
    ///   ([`WireError::PlaceholderSignature`])
    /// - Invalid publisher public keys
    ///   ([`WireError::InvalidPublicKey`])
    /// - Tampered envelopes
    ///   ([`WireError::InvalidSignature`])
    pub fn verify(
        &self,
        publisher: &crate::adapter::net::identity::EntityId,
    ) -> Result<(), WireError> {
        if self.signature.len() != SIGNATURE_LEN {
            return Err(WireError::BadSignatureLength(self.signature.len()));
        }
        if self.signature == placeholder_signature() {
            return Err(WireError::PlaceholderSignature);
        }

        let mut sig_bytes = [0u8; SIGNATURE_LEN];
        sig_bytes.copy_from_slice(&self.signature);
        let sig = ed25519_dalek::Signature::from_bytes(&sig_bytes);

        let meta = EnvelopeMeta {
            announced_at: self.announced_at,
            ttl_secs: self.ttl_secs,
            flags: self.flags,
        };
        let bytes = signing_bytes(
            self.kind,
            self.class,
            self.node_id,
            self.generation,
            &meta,
            &self.payload,
        )?;

        publisher.verify(&bytes, &sig).map_err(|e| match e {
            crate::adapter::net::identity::EntityError::InvalidPublicKey => {
                WireError::InvalidPublicKey
            }
            _ => WireError::InvalidSignature,
        })
    }

    /// Encode the full envelope to wire bytes via postcard.
    pub fn encode(&self) -> Result<Vec<u8>, WireError> {
        postcard::to_allocvec(self).map_err(WireError::Decode)
    }

    /// Decode an envelope from wire bytes. Does NOT verify the
    /// signature — callers route through
    /// [`Self::decode_and_verify`] when they have the publisher's
    /// public key, or call [`Self::verify`] separately. Pure-
    /// decode is exposed for diagnostic tooling that wants to
    /// inspect malformed envelopes.
    pub fn decode(bytes: &[u8]) -> Result<Self, WireError> {
        postcard::from_bytes(bytes).map_err(WireError::Decode)
    }

    /// One-shot decode + verify. The dispatch layer's hot path
    /// goes through here.
    pub fn decode_and_verify(
        bytes: &[u8],
        publisher: &crate::adapter::net::identity::EntityId,
    ) -> Result<Self, WireError> {
        let ann = Self::decode(bytes)?;
        ann.verify(publisher)?;
        Ok(ann)
    }
}
