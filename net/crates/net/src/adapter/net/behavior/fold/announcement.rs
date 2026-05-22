//! Fold-layer wire envelope. See
//! `docs/plans/SCALING_MULTIFOLD_PLAN.md` § Wire format for the
//! authoritative field semantics and on-wire ordering.

use serde::{Deserialize, Serialize};

use super::state::NodeId;

/// Ed25519 signature size in bytes (64). The on-wire signature
/// is a fixed-length slice stored here as a `Vec<u8>` so the
/// derived `Serialize`/`Deserialize` impls work without a
/// `serde-big-array` dependency.
pub const SIGNATURE_LEN: usize = 64;

/// Per-envelope metadata grouped to keep the `sign` /
/// `placeholder` constructor signatures narrow. All three fields
/// are wire-envelope members; defaults match the most common
/// publisher pattern (current wall-clock micros, default TTL
/// via `FoldKind::DEFAULT_TTL`, no flag bits set).
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
/// is the per-fold payload type ([`super::FoldKind::Payload`]).
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
