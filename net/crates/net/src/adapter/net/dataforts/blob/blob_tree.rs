//! Hierarchical manifest tree primitives for `BlobRef::Tree`
//! (v0.3, terabyte-scale).
//!
//! v0.2 [`BlobRef::Manifest`](super::blob_ref::BlobRef::Manifest)
//! carries a flat `Vec<ChunkRef>` and caps at 16 GiB. v0.3 layers
//! a tree of [`TreeNode`] above the chunk layer: leaves carry
//! [`ChunkRefV3`] lists, internal nodes carry
//! `(child_hash, subtree_size)` pairs. The outer
//! [`BlobRef::Tree`](super::blob_ref::BlobRef::Tree) carries the
//! root hash + total size + depth; the substrate walks the tree
//! lazily on `fetch_range`, fetching only the manifest path + the
//! spanning chunks.
//!
//! # Wire format
//!
//! Each [`TreeNode`] is itself stored as a v0.15
//! [`BlobRef::Small`](super::blob_ref::BlobRef::Small) at
//! `dataforts/blob/<hex32>` (same channel naming as v0.2 chunks).
//! The node's bytes are postcard-encoded; the parent
//! ([`TreeNode::Internal`] or [`BlobRef::Tree::root_hash`])
//! carries the hash that authenticates the node. Tree-walk
//! verification (BLAKE3 of fetched bytes == parent's stored hash)
//! extends the v0.2 single-chunk verification model up the tree.
//!
//! # Fanout + depth
//!
//! Fanout is pinned at [`TREE_FANOUT`] = 128 in v0.3 — a leaf
//! addresses 512 MiB at fixed 4 MiB chunks; depth 4 addresses
//! 128 PiB. The depth cap [`MAX_TREE_DEPTH`] = 4 is enforced
//! at encode and decode time; no tree chaining
//! (`Tree`-pointing-at-`Tree`).
//!
//! # Producer hint
//!
//! Producers SHOULD emit [`BlobRef::Tree`] above
//! [`TREE_THRESHOLD_BYTES`] (32 GiB) — the breakeven where the
//! flat manifest body exceeds ~1 MB. Below threshold, emit the
//! v0.2 [`BlobRef::Manifest`] for round-trip efficiency. The
//! threshold is policy, not a wire requirement; well-formed
//! `Tree`s of any size decode cleanly.

use serde::{Deserialize, Serialize};

use super::error::BlobError;

#[cfg(test)]
use super::blob_ref::BLOB_CHUNK_SIZE_BYTES;

/// Per-level fanout. A leaf carries up to [`TREE_FANOUT`] chunks;
/// an internal node carries up to [`TREE_FANOUT`] children.
///
/// 128 is a balance between leaf size (~5 KiB postcard-encoded
/// for a fixed-chunk leaf), range-fetch read amplification
/// (smaller leaves → less wasted manifest per cross-leaf range
/// query), and chunk-count variance under CDC (where chunks may
/// be up to 4× the average size).
pub const TREE_FANOUT: usize = 128;

/// Hard maximum tree depth.
///
/// At fanout 128 + 4 MiB fixed chunks: depth 1 = 64 GiB, depth 2
/// = 8 TiB, depth 3 = 1 PiB, depth 4 = 128 PiB. Beyond any
/// plausible workload; future lift is non-breaking on the wire.
/// No tree chaining (`Tree`-pointing-at-`Tree`) is permitted —
/// producers that would need depth > 4 hit the size-limit error.
pub const MAX_TREE_DEPTH: u8 = 4;

/// Producer-hint threshold: emit
/// [`BlobRef::Tree`](super::blob_ref::BlobRef::Tree) above this
/// size, [`BlobRef::Manifest`](super::blob_ref::BlobRef::Manifest)
/// below.
///
/// Tree wins decisively above ~25 GiB (where the flat manifest
/// body crosses ~1 MB); 32 GiB rounds up with margin. Below the
/// threshold, the Manifest path's single-round-trip simplicity
/// beats the Tree path's two-round-trip walk.
pub const TREE_THRESHOLD_BYTES: u64 = 32 * 1024 * 1024 * 1024;

/// Hard ceiling on a single [`TreeNode`]'s postcard-encoded
/// wire bytes. Bounds the decoder's allocator before the per-
/// variant invariants run.
///
/// At fanout 128 + 32-byte hashes + 8-byte subtree sizes +
/// postcard varint slack, an `Internal` node lands at ~5–6 KiB.
/// A `Leaf` of 128 ChunkRefV3 entries (32 hash + 5 size varint +
/// 2 role discriminant ≈ 40 bytes each) lands at ~5–6 KiB. 64
/// KiB gives ~10× headroom for forward-compat fields.
pub const TREE_NODE_MAX_WIRE_BYTES: usize = 64 * 1024;

/// Maximum permissible chunk size under v0.3 CDC. The fixed-
/// chunk path uses [`BLOB_CHUNK_SIZE_BYTES`] (4 MiB); CDC pins
/// `max = 16 MiB`. Leaf decoding rejects any chunk past this so
/// a malicious peer can't stamp a `size = u32::MAX` chunk that
/// then overflows a per-chunk buffer on fetch.
pub const TREE_LEAF_CHUNK_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// Role of a chunk within a v0.3 leaf. Replicated-encoded blobs
/// carry all [`ChunkRole::Data`]; Reed–Solomon-encoded blobs
/// mix [`ChunkRole::Data`] and [`ChunkRole::Parity`].
///
/// v0.3a (Phase A) emits only `Data`. `Parity` lands in Phase C
/// with the RS implementation; reserving the variant now keeps
/// the wire format stable across the phase boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ChunkRole {
    /// Plain data chunk.
    Data,
    /// Reed–Solomon parity chunk. `stripe_index` identifies which
    /// stripe within the leaf the chunk belongs to (multiple
    /// stripes may co-exist in one leaf for large RS configurations).
    Parity {
        /// Per-leaf stripe identifier (`0..N` where `N` = number
        /// of stripes in the leaf).
        stripe_index: u8,
    },
}

impl Default for ChunkRole {
    fn default() -> Self {
        Self::Data
    }
}

/// v0.3 chunk reference. Adds [`ChunkRole`] to v0.2's
/// (`hash`, `size`) shape so Reed–Solomon-encoded leaves can
/// label data vs parity members at the chunk level.
///
/// v0.2 [`ChunkRef`](super::blob_ref::ChunkRef) keeps its wire
/// shape unchanged inside `ManifestBody` — `ChunkRefV3` is
/// leaf-only and only appears inside a [`TreeNode::Leaf`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChunkRefV3 {
    /// BLAKE3-256 of the chunk's canonical bytes.
    pub hash: [u8; 32],
    /// Chunk payload length in bytes. Bounded above by
    /// [`TREE_LEAF_CHUNK_MAX_BYTES`].
    pub size: u32,
    /// Data / parity classification for RS-encoded leaves.
    /// [`ChunkRole::Data`] for Replicated-encoded blobs.
    pub role: ChunkRole,
}

impl ChunkRefV3 {
    /// Construct a data-role chunk reference. The most common
    /// case (every chunk in a Replicated-encoded blob).
    pub fn data(hash: [u8; 32], size: u32) -> Self {
        Self {
            hash,
            size,
            role: ChunkRole::Data,
        }
    }

    /// Construct a parity-role chunk reference. Used by the
    /// Phase C Reed–Solomon striper.
    pub fn parity(hash: [u8; 32], size: u32, stripe_index: u8) -> Self {
        Self {
            hash,
            size,
            role: ChunkRole::Parity { stripe_index },
        }
    }

    /// `true` if this is a data chunk (vs parity).
    pub fn is_data(&self) -> bool {
        matches!(self.role, ChunkRole::Data)
    }

    /// `true` if this is a parity chunk.
    pub fn is_parity(&self) -> bool {
        matches!(self.role, ChunkRole::Parity { .. })
    }
}

/// A node in the manifest tree. Stored as a v0.15
/// [`BlobRef::Small`](super::blob_ref::BlobRef::Small) at
/// `dataforts/blob/<hex32>` — same channel naming + GC lifecycle
/// as v0.2 chunks.
///
/// The node's depth in the tree comes from the outer
/// [`BlobRef::Tree::depth`](super::blob_ref::BlobRef::Tree) +
/// the walk position; the node itself does NOT carry its own
/// depth (that would let a peer-supplied node lie about its
/// position). Tree-walk verification (BLAKE3 of fetched bytes
/// == parent's stored hash) rejects any peer-supplied node
/// whose bytes don't match.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TreeNode {
    /// Internal node — carries up to [`TREE_FANOUT`] children.
    /// Each entry is `(child_hash, subtree_total_size)`; the hash
    /// references either another [`TreeNode::Internal`] (when
    /// the parent's residual depth > 1) or a [`TreeNode::Leaf`]
    /// (when residual depth == 1).
    Internal {
        /// `(child_hash, subtree_size)` pairs in left-to-right
        /// order. The subtree size enables O(depth) prefix-sum
        /// range lookup without descending every child.
        children: Vec<([u8; 32], u64)>,
    },
    /// Leaf node — carries the actual chunk references that the
    /// fetch path resolves into bytes.
    Leaf {
        /// Chunk references in byte-order. Position N corresponds
        /// to the byte range starting at `sum(chunks[0..N].size)`.
        chunks: Vec<ChunkRefV3>,
    },
}

impl TreeNode {
    /// Construct an [`Internal`](TreeNode::Internal) node from
    /// `(child_hash, subtree_size)` pairs. Validates fanout and
    /// non-empty + non-zero invariants at construction.
    pub fn internal(children: Vec<([u8; 32], u64)>) -> Result<Self, BlobError> {
        let node = TreeNode::Internal { children };
        node.validate()?;
        Ok(node)
    }

    /// Construct a [`Leaf`](TreeNode::Leaf) node from
    /// [`ChunkRefV3`] entries. Validates fanout + per-chunk-size
    /// invariants at construction.
    pub fn leaf(chunks: Vec<ChunkRefV3>) -> Result<Self, BlobError> {
        let node = TreeNode::Leaf { chunks };
        node.validate()?;
        Ok(node)
    }

    /// Postcard-encode for storage as a Small blob's body.
    /// Returns the bytes that the substrate then hashes (via
    /// BLAKE3) to derive this node's address.
    pub fn encode(&self) -> Result<Vec<u8>, BlobError> {
        self.validate()?;
        let bytes = postcard::to_allocvec(self)
            .map_err(|e| BlobError::Decode(format!("TreeNode encode failed: {}", e)))?;
        if bytes.len() > TREE_NODE_MAX_WIRE_BYTES {
            return Err(BlobError::Decode(format!(
                "TreeNode encoded length {} exceeds cap {}",
                bytes.len(),
                TREE_NODE_MAX_WIRE_BYTES
            )));
        }
        Ok(bytes)
    }

    /// Decode a [`TreeNode`] from a Small blob's body. Enforces
    /// the wire-size cap before postcard allocates, then runs
    /// per-variant invariants ([`validate`](Self::validate)).
    pub fn decode(bytes: &[u8]) -> Result<Self, BlobError> {
        if bytes.len() > TREE_NODE_MAX_WIRE_BYTES {
            return Err(BlobError::Decode(format!(
                "TreeNode wire length {} exceeds cap {}",
                bytes.len(),
                TREE_NODE_MAX_WIRE_BYTES
            )));
        }
        let node: TreeNode = postcard::from_bytes(bytes)
            .map_err(|e| BlobError::Decode(format!("TreeNode decode failed: {}", e)))?;
        node.validate()?;
        Ok(node)
    }

    /// Per-variant invariants: non-empty, fanout cap, per-entry
    /// size sanity. Run at construction (in `internal` / `leaf`)
    /// and on decode.
    pub fn validate(&self) -> Result<(), BlobError> {
        match self {
            TreeNode::Internal { children } => {
                if children.is_empty() {
                    return Err(BlobError::Decode(
                        "TreeNode::Internal must have at least one child".to_owned(),
                    ));
                }
                if children.len() > TREE_FANOUT {
                    return Err(BlobError::Decode(format!(
                        "TreeNode::Internal child count {} exceeds fanout cap {}",
                        children.len(),
                        TREE_FANOUT
                    )));
                }
                // Subtree sizes must be strictly positive — a zero-byte
                // subtree is a sign of a malicious or buggy producer and
                // would break the prefix-sum range-lookup invariant.
                // Sum into a saturating u64; reject overflow as
                // structurally invalid.
                let mut sum: u64 = 0;
                for (i, (_, sz)) in children.iter().enumerate() {
                    if *sz == 0 {
                        return Err(BlobError::Decode(format!(
                            "TreeNode::Internal child {} has zero subtree_size",
                            i
                        )));
                    }
                    sum = sum.checked_add(*sz).ok_or_else(|| {
                        BlobError::Decode(
                            "TreeNode::Internal subtree_size sum overflowed u64".to_owned(),
                        )
                    })?;
                }
                let _ = sum; // future cross-check vs BlobRef::Tree::total_size.
            }
            TreeNode::Leaf { chunks } => {
                if chunks.is_empty() {
                    return Err(BlobError::Decode(
                        "TreeNode::Leaf must have at least one chunk".to_owned(),
                    ));
                }
                if chunks.len() > TREE_FANOUT {
                    return Err(BlobError::Decode(format!(
                        "TreeNode::Leaf chunk count {} exceeds fanout cap {}",
                        chunks.len(),
                        TREE_FANOUT
                    )));
                }
                for (i, chunk) in chunks.iter().enumerate() {
                    if chunk.size == 0 {
                        return Err(BlobError::Decode(format!(
                            "TreeNode::Leaf chunk {} has zero size",
                            i
                        )));
                    }
                    if (chunk.size as u64) > TREE_LEAF_CHUNK_MAX_BYTES {
                        return Err(BlobError::Decode(format!(
                            "TreeNode::Leaf chunk {} size {} exceeds cap {}",
                            i, chunk.size, TREE_LEAF_CHUNK_MAX_BYTES
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    /// `true` if this is an internal node.
    pub fn is_internal(&self) -> bool {
        matches!(self, TreeNode::Internal { .. })
    }

    /// `true` if this is a leaf node.
    pub fn is_leaf(&self) -> bool {
        matches!(self, TreeNode::Leaf { .. })
    }

    /// Sum of subtree sizes for [`Internal`](TreeNode::Internal),
    /// sum of chunk sizes for [`Leaf`](TreeNode::Leaf). Used by
    /// the walker to cross-check a child's reported total against
    /// the parent's `subtree_size` entry.
    pub fn covered_bytes(&self) -> u64 {
        match self {
            TreeNode::Internal { children } => {
                children.iter().map(|(_, sz)| *sz).sum::<u64>()
            }
            TreeNode::Leaf { chunks } => chunks.iter().map(|c| c.size as u64).sum::<u64>(),
        }
    }

    /// Number of direct children (for internal) or chunks (for
    /// leaf). Always in `1..=TREE_FANOUT` for a valid node.
    pub fn arity(&self) -> usize {
        match self {
            TreeNode::Internal { children } => children.len(),
            TreeNode::Leaf { chunks } => chunks.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    // -----------------------------------------------------------
    // ChunkRefV3
    // -----------------------------------------------------------

    #[test]
    fn chunk_ref_data_helper_sets_role() {
        let c = ChunkRefV3::data(h(0x11), 4 * 1024 * 1024);
        assert!(c.is_data());
        assert!(!c.is_parity());
        assert_eq!(c.role, ChunkRole::Data);
    }

    #[test]
    fn chunk_ref_parity_helper_sets_role_with_stripe_index() {
        let c = ChunkRefV3::parity(h(0x22), 4 * 1024 * 1024, 3);
        assert!(!c.is_data());
        assert!(c.is_parity());
        match c.role {
            ChunkRole::Parity { stripe_index } => assert_eq!(stripe_index, 3),
            ChunkRole::Data => panic!("expected Parity role"),
        }
    }

    #[test]
    fn chunk_role_default_is_data() {
        assert_eq!(ChunkRole::default(), ChunkRole::Data);
    }

    // -----------------------------------------------------------
    // TreeNode constructors validate at construction time
    // -----------------------------------------------------------

    #[test]
    fn internal_requires_at_least_one_child() {
        let err = TreeNode::internal(Vec::new()).unwrap_err();
        assert!(matches!(err, BlobError::Decode(_)));
    }

    #[test]
    fn internal_rejects_zero_subtree_size() {
        let err = TreeNode::internal(vec![(h(0x01), 0)]).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("zero subtree_size"), "got: {msg}");
    }

    #[test]
    fn internal_rejects_overflowing_subtree_sum() {
        // Two children whose sizes sum > u64::MAX.
        let err = TreeNode::internal(vec![(h(0x01), u64::MAX), (h(0x02), 1)]).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("overflowed"), "got: {msg}");
    }

    #[test]
    fn internal_rejects_over_fanout_cap() {
        let too_many: Vec<_> = (0..(TREE_FANOUT + 1) as u8)
            .map(|i| (h(i), 1024u64))
            .collect();
        let err = TreeNode::internal(too_many).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("exceeds fanout cap"),
            "got: {msg}"
        );
    }

    #[test]
    fn internal_accepts_at_fanout_cap() {
        let exactly_cap: Vec<_> = (0..TREE_FANOUT as u8).map(|i| (h(i), 4 * 1024 * 1024u64)).collect();
        let node = TreeNode::internal(exactly_cap).expect("at-cap construction succeeds");
        assert_eq!(node.arity(), TREE_FANOUT);
    }

    #[test]
    fn leaf_requires_at_least_one_chunk() {
        let err = TreeNode::leaf(Vec::new()).unwrap_err();
        assert!(matches!(err, BlobError::Decode(_)));
    }

    #[test]
    fn leaf_rejects_zero_size_chunk() {
        let err = TreeNode::leaf(vec![ChunkRefV3::data(h(0x33), 0)]).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("zero size"), "got: {msg}");
    }

    #[test]
    fn leaf_rejects_oversize_chunk() {
        let over = (TREE_LEAF_CHUNK_MAX_BYTES + 1) as u32;
        let err = TreeNode::leaf(vec![ChunkRefV3::data(h(0x44), over)]).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("exceeds cap"), "got: {msg}");
    }

    #[test]
    fn leaf_accepts_fixed_chunk_size() {
        let node = TreeNode::leaf(vec![ChunkRefV3::data(h(0x55), BLOB_CHUNK_SIZE_BYTES as u32)])
            .expect("fixed-size chunk is valid");
        assert_eq!(node.arity(), 1);
        assert_eq!(node.covered_bytes(), BLOB_CHUNK_SIZE_BYTES);
    }

    #[test]
    fn leaf_accepts_at_cdc_max_chunk_size() {
        let max = TREE_LEAF_CHUNK_MAX_BYTES as u32;
        let node = TreeNode::leaf(vec![ChunkRefV3::data(h(0x66), max)])
            .expect("at-cap chunk is valid (CDC max boundary)");
        assert_eq!(node.covered_bytes(), TREE_LEAF_CHUNK_MAX_BYTES);
    }

    // -----------------------------------------------------------
    // Wire encode/decode round-trip
    // -----------------------------------------------------------

    #[test]
    fn internal_round_trips_through_wire() {
        let children = vec![
            (h(0x01), 4 * 1024 * 1024u64),
            (h(0x02), 4 * 1024 * 1024),
            (h(0x03), 1024 * 1024),
        ];
        let node = TreeNode::internal(children.clone()).unwrap();
        let bytes = node.encode().unwrap();
        let decoded = TreeNode::decode(&bytes).unwrap();
        assert_eq!(node, decoded);
        match decoded {
            TreeNode::Internal { children: c } => assert_eq!(c, children),
            TreeNode::Leaf { .. } => panic!("expected Internal"),
        }
    }

    #[test]
    fn leaf_round_trips_data_chunks_through_wire() {
        let chunks = vec![
            ChunkRefV3::data(h(0x10), 4 * 1024 * 1024),
            ChunkRefV3::data(h(0x11), 4 * 1024 * 1024),
            ChunkRefV3::data(h(0x12), 1024),
        ];
        let node = TreeNode::leaf(chunks.clone()).unwrap();
        let bytes = node.encode().unwrap();
        let decoded = TreeNode::decode(&bytes).unwrap();
        assert_eq!(node, decoded);
        if let TreeNode::Leaf { chunks: c } = decoded {
            assert_eq!(c, chunks);
            assert!(c.iter().all(ChunkRefV3::is_data));
        } else {
            panic!("expected Leaf");
        }
    }

    #[test]
    fn leaf_round_trips_mixed_data_parity_chunks() {
        let chunks = vec![
            ChunkRefV3::data(h(0x20), 4 * 1024 * 1024),
            ChunkRefV3::data(h(0x21), 4 * 1024 * 1024),
            ChunkRefV3::parity(h(0x22), 4 * 1024 * 1024, 0),
            ChunkRefV3::parity(h(0x23), 4 * 1024 * 1024, 0),
        ];
        let node = TreeNode::leaf(chunks.clone()).unwrap();
        let bytes = node.encode().unwrap();
        let decoded = TreeNode::decode(&bytes).unwrap();
        assert_eq!(node, decoded);
        if let TreeNode::Leaf { chunks: c } = decoded {
            assert_eq!(c.iter().filter(|x| x.is_data()).count(), 2);
            assert_eq!(c.iter().filter(|x| x.is_parity()).count(), 2);
        } else {
            panic!("expected Leaf");
        }
    }

    /// Decoder must reject a postcard body longer than the wire
    /// cap BEFORE postcard allocates — a malicious peer could
    /// otherwise stamp a multi-MB body to force a large allocation
    /// before the per-variant cap fires.
    #[test]
    fn decode_rejects_oversize_wire_bytes() {
        let bytes = vec![0u8; TREE_NODE_MAX_WIRE_BYTES + 1];
        let err = TreeNode::decode(&bytes).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("exceeds cap"), "got: {msg}");
    }

    /// Encode must reject a node whose serialized form would
    /// exceed the wire cap. With fanout 128 the only realistic
    /// way to trigger this is a future-format extension that adds
    /// significant per-entry overhead; today's serialized internal
    /// node lands at < 6 KiB. The check is defense-in-depth.
    #[test]
    fn encode_cap_is_reachable_only_under_pathological_inputs() {
        // A maxed-out Internal node lands well under the cap.
        let children: Vec<_> = (0..TREE_FANOUT as u8).map(|i| (h(i), 1u64 << 32)).collect();
        let node = TreeNode::internal(children).unwrap();
        let bytes = node.encode().unwrap();
        assert!(
            bytes.len() < TREE_NODE_MAX_WIRE_BYTES,
            "max-fanout internal node should fit comfortably; got {} bytes",
            bytes.len()
        );
    }

    /// Decoder must reject malformed postcard bytes with the same
    /// magic prefix — postcard's own error is wrapped into
    /// `BlobError::Decode` so callers see a uniform error surface.
    #[test]
    fn decode_rejects_malformed_postcard_bytes() {
        // Random bytes that aren't valid postcard for the TreeNode
        // schema (would be interpreted as an enum-discriminant 0xFF
        // which is past both variants).
        let garbage = vec![0xFFu8; 32];
        let err = TreeNode::decode(&garbage).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("decode failed"), "got: {msg}");
    }

    /// Decoder validates per-variant invariants even when postcard
    /// successfully reconstructs the type. Smuggling an invalid
    /// node onto the wire (e.g. empty Internal) is the attack
    /// surface this guards.
    #[test]
    fn decode_re_validates_invariants() {
        // Hand-construct an Internal { children: [] } bypassing the
        // constructor and serialize it. Decode must reject.
        let bad = TreeNode::Internal { children: Vec::new() };
        // Skip `encode`'s pre-check by going through postcard
        // directly.
        let bytes = postcard::to_allocvec(&bad).unwrap();
        let err = TreeNode::decode(&bytes).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("must have at least one child"),
            "got: {msg}"
        );
    }

    // -----------------------------------------------------------
    // covered_bytes + arity helpers
    // -----------------------------------------------------------

    #[test]
    fn covered_bytes_internal_sums_subtree_sizes() {
        let node = TreeNode::internal(vec![
            (h(0x01), 100),
            (h(0x02), 200),
            (h(0x03), 50),
        ])
        .unwrap();
        assert_eq!(node.covered_bytes(), 350);
        assert_eq!(node.arity(), 3);
        assert!(node.is_internal());
        assert!(!node.is_leaf());
    }

    #[test]
    fn covered_bytes_leaf_sums_chunk_sizes() {
        let node = TreeNode::leaf(vec![
            ChunkRefV3::data(h(0x10), 1000),
            ChunkRefV3::data(h(0x11), 2000),
        ])
        .unwrap();
        assert_eq!(node.covered_bytes(), 3000);
        assert_eq!(node.arity(), 2);
        assert!(node.is_leaf());
        assert!(!node.is_internal());
    }

    // -----------------------------------------------------------
    // Constant sanity
    // -----------------------------------------------------------

    #[test]
    fn fanout_and_depth_yield_expected_max_address() {
        // fanout 128, depth 4, chunk 4 MiB.
        // depth-1 = one layer of internals over a leaf-of-128-chunks:
        //   = 128 × (128 × 4 MiB) = 64 GiB
        // depth-2 = 128 × depth-1 = 8 TiB
        // depth-3 = 128 × depth-2 = 1 PiB
        // depth-4 = 128 × depth-3 = 128 PiB
        let leaf_bytes = TREE_FANOUT as u128 * BLOB_CHUNK_SIZE_BYTES as u128;
        let depth_1 = leaf_bytes * TREE_FANOUT as u128;
        let depth_2 = depth_1 * TREE_FANOUT as u128;
        let depth_3 = depth_2 * TREE_FANOUT as u128;
        let depth_4 = depth_3 * TREE_FANOUT as u128;
        // 128 PiB = 128 * 2^50 bytes
        let pib_128 = 128u128 * (1u128 << 50);
        assert_eq!(
            depth_4, pib_128,
            "fanout {} + depth {} + chunk {} should address 128 PiB",
            TREE_FANOUT, MAX_TREE_DEPTH, BLOB_CHUNK_SIZE_BYTES
        );
        // Per-depth sanity values pinned for the doc-comment claims.
        assert_eq!(leaf_bytes, 512u128 * 1024 * 1024, "leaf addresses 512 MiB");
        assert_eq!(depth_1, 64u128 * 1024 * 1024 * 1024, "depth-1 addresses 64 GiB");
        assert_eq!(depth_2, 8u128 * (1u128 << 40), "depth-2 addresses 8 TiB");
        assert_eq!(depth_3, 1u128 << 50, "depth-3 addresses 1 PiB");
    }

    #[test]
    fn threshold_matches_documented_break_even() {
        // 32 GiB
        assert_eq!(TREE_THRESHOLD_BYTES, 32u64 * 1024 * 1024 * 1024);
    }

    #[test]
    fn max_tree_depth_pinned_at_four() {
        assert_eq!(MAX_TREE_DEPTH, 4);
    }
}
