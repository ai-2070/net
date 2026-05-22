//! Fold-layer wire envelope.
//!
//! Phase 1 ships the in-memory shape only — signature verification
//! and the wire codec land in Phase 2. The shape is held stable
//! here so the runtime, the snapshot serializer, and (Phase 2)
//! the dispatch layer agree on what an "announcement" looks like
//! before any actual bytes flow.
//!
//! The plan's wire-format spec — see
//! `docs/plans/SCALING_MULTIFOLD_PLAN.md` § Wire format — is the
//! authority for field semantics and on-wire ordering.

use serde::{Deserialize, Serialize};

use super::state::NodeId;

/// Ed25519 signature size in bytes (64). The on-wire signature
/// is a fixed-length slice; Phase 1 stores it as a `Vec<u8>` so
/// the derived `Serialize`/`Deserialize` impls work without a
/// `serde-big-array` dependency, and Phase 2 narrows it back to
/// `[u8; SIGNATURE_LEN]` when the wire codec lands.
pub const SIGNATURE_LEN: usize = 64;

/// Sentinel signature bytes used in Phase 1 tests and any
/// dispatch path that hasn't been routed through Phase 2's
/// signing pipeline yet. Phase 2's verifier rejects this value
/// unconditionally; until then it carries the "envelope is
/// well-formed, signature is a placeholder" marker through the
/// in-memory shape.
pub fn placeholder_signature() -> Vec<u8> {
    vec![0u8; SIGNATURE_LEN]
}

/// One signed announcement on a fold channel. The `P` parameter
/// is the per-fold payload type (`FoldKind::Payload`).
///
/// On the wire (Phase 2): postcard-encoded with field-ordered
/// structs; the signature is Ed25519 over the canonical encoding
/// of every other field. The `subnet_id` is intentionally NOT a
/// member of this struct — it lives on the underlying
/// `NetHeader.subnet_id` per the plan's
/// "Composition with the existing subnet system" section, which
/// avoids dual-source-of-truth between the wire envelope and
/// the header.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedAnnouncement<P> {
    /// Fold this announcement targets — [`super::FoldKind::KIND_ID`].
    /// The dispatch layer (Phase 2) routes on this and rejects
    /// announcements whose `kind` is unregistered.
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
    /// other field. Phase 1 holds this as a `Vec<u8>` of length
    /// [`SIGNATURE_LEN`] so the derived serde impls work
    /// without a fixed-array codec dependency; Phase 2 narrows
    /// it to `[u8; SIGNATURE_LEN]` when the wire codec lands
    /// and plumbs in the real verifier. Until then producers
    /// stamp [`placeholder_signature`] and the (forthcoming)
    /// dispatch layer rejects any signature whose bytes are
    /// all-zero on the slow path.
    pub signature: Vec<u8>,
}

impl<P> SignedAnnouncement<P> {
    /// Construct an announcement with the
    /// [`placeholder_signature`] sentinel. Phase 1 callers
    /// (tests, in-process producers) use this; Phase 2's signer
    /// will provide a real `sign(...)` constructor.
    ///
    /// The arg list mirrors the wire envelope's fields in
    /// declaration order. The wire format pins the field set,
    /// so the constructor signature is wide by design;
    /// clippy's `too_many_arguments` lint is suppressed here
    /// because Phase 2's `sign(...)` will inherit the same shape.
    #[allow(clippy::too_many_arguments)]
    pub fn placeholder(
        kind: u16,
        class: u64,
        node_id: NodeId,
        generation: u64,
        announced_at: u64,
        ttl_secs: Option<u32>,
        flags: u8,
        payload: P,
    ) -> Self {
        Self {
            kind,
            class,
            node_id,
            generation,
            announced_at,
            ttl_secs,
            flags,
            payload,
            signature: placeholder_signature(),
        }
    }
}
