//! Phase 2 wire codec for [`SignedAnnouncement<P>`].
//!
//! The on-wire form is postcard. The signing form — the bytes the
//! Ed25519 signature commits to — is a separate postcard
//! serialization of every field EXCEPT the signature itself, in
//! the field-declared order. Keeping the two forms distinct lets
//! the verifier reconstruct the signing bytes from a received
//! envelope without re-encoding the signature into them.
//!
//! ## Why postcard
//!
//! - **Field-deterministic.** The struct's `#[derive(Serialize)]`
//!   emits fields in declaration order; the codec is stable across
//!   builds as long as the struct field order doesn't change. The
//!   plan locks the field order at v1 — see the
//!   `SignedAnnouncement` struct doc.
//! - **No length-prefix tax on the cold path.** Fixed-width
//!   fields (u16, u64, NodeId, etc.) encode with no length prefix;
//!   variable-width fields (`Option<u32>`, `Vec<u8>`, `payload: P`)
//!   carry a one-byte (varint) length. The dispatch hot path
//!   doesn't allocate beyond the payload itself.
//! - **Already a workspace dependency.** Same codec the meshdb
//!   plan-byte hashing and the RedEX disk format use.
//!
//! Phase 2's verifier rejects:
//! - Signatures whose length is not [`super::announcement::SIGNATURE_LEN`].
//! - The all-zero [`super::announcement::placeholder_signature`]
//!   sentinel — that's the Phase-1 unsigned envelope, which has
//!   no business reaching the dispatch path.
//! - Tampered envelopes (the signature won't verify against the
//!   recomputed signing bytes).

use serde::{de::DeserializeOwned, Serialize};

use super::announcement::{placeholder_signature, EnvelopeMeta, SignedAnnouncement, SIGNATURE_LEN};
use super::state::NodeId;
use super::FoldError;

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

    /// Decoded envelope carries the Phase-1
    /// [`placeholder_signature`] sentinel. The Phase-2 dispatch
    /// layer rejects this — the envelope was constructed without
    /// signing, so verification would be vacuous and the
    /// `node_id` claim is unauthenticated.
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
    /// [`signing_bytes`].
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

    /// Verify the signature against a publisher's [`EntityId`].
    ///
    /// Rejects:
    /// - Wrong-length signatures
    ///   ([`WireError::BadSignatureLength`])
    /// - The Phase-1 placeholder sentinel
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

        publisher
            .verify(&bytes, &sig)
            .map_err(|e| match e {
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
