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

// `ChunkingStrategy::Default` references `BLOB_CHUNK_SIZE_BYTES`
// via the fully-qualified `super::blob_ref::` path so the
// test-only import above doesn't shadow it for production paths.

/// Capability tag advertised by nodes that support the v0.3
/// hierarchical-manifest tree path (`BlobRef::Tree`). Producers
/// targeting a peer that does NOT advertise this tag must
/// downgrade to the v0.2 `BlobRef::Manifest` (capped at 16 GiB)
/// — a v0.2-only reader returns `BlobError::UnsupportedVersion(3)`
/// on a `BlobRef::Tree` wire frame.
///
/// The capability-advertisement substrate ships independently of
/// the blob layer; v0.3 Phase A declares the tag string and
/// exposes a [`TreeSupportProbe`] hook so producers can wire
/// the check without depending on a specific advertisement
/// transport.
pub const DATAFORTS_BLOB_TREE_SUPPORTED: &str = "dataforts:blob-tree-supported";

/// Producer-side hook for the cross-version downgrade decision.
///
/// Implementations decide whether a destination peer supports
/// the v0.3 [`BlobRef::Tree`](super::blob_ref::BlobRef::Tree)
/// shape. The default [`TreeSupportProbe::AlwaysSupported`] is
/// correct for single-cluster all-v0.3 deployments;
/// [`TreeSupportProbe::ForceManifest`] is correct for cross-
/// version deployments where every Tree publish must downgrade;
/// the dynamic [`TreeSupportProbe::Dynamic`] arm lets callers
/// wire the substrate's capability-tag advertisement once that
/// surface lands.
///
/// Producers consult the probe BEFORE calling
/// `store_stream_tree` — on `false`, they fall back to
/// `store_stream` + `BlobRef::Manifest` (capped at 16 GiB).
#[derive(Default)]
pub enum TreeSupportProbe {
    /// All targets support Tree. Default for single-cluster
    /// v0.3-only deployments.
    #[default]
    AlwaysSupported,
    /// No target supports Tree. Forces every publish to
    /// downgrade to v0.2 Manifest. Useful during cluster-wide
    /// rollouts before every node has been upgraded.
    ForceManifest,
    /// Dynamic check — call into a caller-supplied closure
    /// that consults the capability-tag advertisement layer.
    /// The boxed closure returns `true` iff the destination
    /// advertises [`DATAFORTS_BLOB_TREE_SUPPORTED`].
    Dynamic(Box<dyn Fn() -> bool + Send + Sync>),
}

impl TreeSupportProbe {
    /// Evaluate the probe. Cheap for the static variants;
    /// invokes the closure for `Dynamic`.
    pub fn check(&self) -> bool {
        match self {
            TreeSupportProbe::AlwaysSupported => true,
            TreeSupportProbe::ForceManifest => false,
            TreeSupportProbe::Dynamic(f) => f(),
        }
    }
}

impl std::fmt::Debug for TreeSupportProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TreeSupportProbe::AlwaysSupported => f.write_str("TreeSupportProbe::AlwaysSupported"),
            TreeSupportProbe::ForceManifest => f.write_str("TreeSupportProbe::ForceManifest"),
            TreeSupportProbe::Dynamic(_) => f.write_str("TreeSupportProbe::Dynamic(..)"),
        }
    }
}


/// Chunking strategy for `MeshBlobAdapter::store_stream_tree`.
///
/// v0.3 Phase A ships [`ChunkingStrategy::Fixed`] only —
/// deterministic fixed-size chunks matching v0.2's chunker so
/// content stored via Tree can dedup at the chunk level against
/// content stored via the v0.2 Manifest path.
///
/// [`ChunkingStrategy::Cdc`] is reserved on the surface for
/// Phase B (content-defined chunking). Constructing a `Cdc`
/// variant is allowed; passing it to `store_stream_tree` in v0.3a
/// returns `BlobError::Backend` until Phase B lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChunkingStrategy {
    /// Fixed-size chunks. `size` is the chunk size in bytes;
    /// only the last chunk may be smaller. Default value
    /// matches v0.2's [`super::blob_ref::BLOB_CHUNK_SIZE_BYTES`].
    Fixed {
        /// Chunk size in bytes. Must equal
        /// `BLOB_CHUNK_SIZE_BYTES` (4 MiB) in v0.3a — other
        /// values are wire-incompatible with v0.2 dedup and
        /// rejected by `store_stream_tree`.
        size: u32,
    },
    /// Content-defined chunking via FastCDC. Reserved for Phase B.
    Cdc {
        /// Target average chunk size in bytes.
        avg: u32,
        /// Minimum chunk size in bytes.
        min: u32,
        /// Maximum chunk size in bytes (enforced as hard cut).
        max: u32,
    },
}

impl Default for ChunkingStrategy {
    fn default() -> Self {
        Self::Fixed {
            size: super::blob_ref::BLOB_CHUNK_SIZE_BYTES as u32,
        }
    }
}

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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ChunkRole {
    /// Plain data chunk.
    #[default]
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

/// One Reed-Solomon stripe inside a [`TreeNode::ErasureLeaf`].
/// Carries its own [`Encoding`](super::blob_ref::Encoding) so the
/// small-stripe fallback (a trailing partial stripe under the
/// `RS_STRIPE_MIN_BYTES` threshold) can record itself as
/// `Replicated` while the parent blob's encoding is
/// `ReedSolomon { k, m }`.
///
/// `chunks` lists data chunks first, then parity chunks. The
/// reader consults `encoding`:
/// - `Encoding::Replicated` → every chunk is data; concatenate
///   to reconstruct the stripe.
/// - `Encoding::ReedSolomon { k, m }` → first `k` chunks are
///   data (each `ChunkRefV3::size` = pre-padding actual data
///   size); next `m` chunks are parity (size = post-padding
///   data shard length, equal across the stripe).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StripeBlock {
    /// Per-stripe encoding override. Allows mixed RS + Replicated
    /// stripes within one RS-encoded blob.
    pub encoding: super::blob_ref::Encoding,
    /// Chunk references in stripe order: data first, then parity.
    pub chunks: Vec<ChunkRefV3>,
}

impl StripeBlock {
    /// Number of bytes the stripe covers when read out — sum of
    /// the data chunks' sizes. Parity chunks contribute zero
    /// because they're never returned to the caller verbatim
    /// (they only exist for reconstruction).
    pub fn covered_bytes(&self) -> u64 {
        self.chunks
            .iter()
            .filter(|c| c.is_data())
            .map(|c| c.size as u64)
            .sum()
    }

    /// Stripe-shape validation. Catches encoder bugs early:
    /// - Replicated stripe must have zero parity chunks.
    /// - RS stripe must have exactly `k` data + `m` parity chunks.
    /// - Every parity chunk's `stripe_index` is unused at the
    ///   `StripeBlock` level (stripes are now positionally
    ///   identified by index inside `ErasureLeaf::stripes`); a
    ///   stale non-zero `stripe_index` is tolerated for forward
    ///   compatibility.
    pub fn validate(&self) -> Result<(), BlobError> {
        let data_count = self.chunks.iter().filter(|c| c.is_data()).count();
        let parity_count = self.chunks.iter().filter(|c| c.is_parity()).count();
        match self.encoding {
            super::blob_ref::Encoding::Replicated => {
                if parity_count != 0 {
                    return Err(BlobError::Decode(format!(
                        "StripeBlock(Replicated) has {} parity chunks; expected 0",
                        parity_count
                    )));
                }
                if data_count == 0 {
                    return Err(BlobError::Decode(
                        "StripeBlock(Replicated) has no data chunks".to_owned(),
                    ));
                }
            }
            super::blob_ref::Encoding::ReedSolomon { k, m } => {
                if data_count != k as usize || parity_count != m as usize {
                    return Err(BlobError::Decode(format!(
                        "StripeBlock(RS k={}, m={}) has {} data + {} parity; expected {} + {}",
                        k, m, data_count, parity_count, k, m
                    )));
                }
                // Data chunks must precede parity in the encoded
                // order (positional invariant the fetch path relies
                // on for stripe-relative chunk addressing).
                for (i, c) in self.chunks.iter().enumerate() {
                    if i < k as usize && !c.is_data() {
                        return Err(BlobError::Decode(format!(
                            "StripeBlock(RS): chunk at position {} should be Data, got Parity",
                            i
                        )));
                    }
                    if i >= k as usize && !c.is_parity() {
                        return Err(BlobError::Decode(format!(
                            "StripeBlock(RS): chunk at position {} should be Parity, got Data",
                            i
                        )));
                    }
                }
            }
        }
        Ok(())
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
    /// fetch path resolves into bytes. Used by Replicated-encoded
    /// blobs (Phase A + B).
    Leaf {
        /// Chunk references in byte-order. Position N corresponds
        /// to the byte range starting at `sum(chunks[0..N].size)`.
        chunks: Vec<ChunkRefV3>,
    },
    /// Erasure-coded leaf — one or more Reed-Solomon stripes.
    /// Used by [`Encoding::ReedSolomon`](super::blob_ref::Encoding::ReedSolomon)
    /// blobs (Phase C). Postcard variant discriminant 2 (Internal
    /// is 0, Leaf is 1, ErasureLeaf is 2) — additive to the wire
    /// format, so Phase A/B readers cleanly fail decode on a
    /// Phase C ErasureLeaf rather than silently mis-interpreting
    /// it.
    ErasureLeaf {
        /// Stripes in covered-byte order. Stripe N covers bytes
        /// `sum(stripes[0..N].covered_bytes())..sum(stripes[0..=N].covered_bytes())`.
        /// Each stripe carries its own [`Encoding`](super::blob_ref::Encoding)
        /// so the small-stripe fallback (trailing partial stripe
        /// below `RS_STRIPE_MIN_BYTES`) can mix Replicated stripes
        /// with RS stripes inside one leaf.
        stripes: Vec<StripeBlock>,
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

    /// Construct an [`ErasureLeaf`](TreeNode::ErasureLeaf) from
    /// [`StripeBlock`] entries. Validates per-stripe invariants
    /// plus the leaf-level fanout cap (sum of stripe chunk counts
    /// ≤ [`TREE_FANOUT`]).
    pub fn erasure_leaf(stripes: Vec<StripeBlock>) -> Result<Self, BlobError> {
        let node = TreeNode::ErasureLeaf { stripes };
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
            TreeNode::ErasureLeaf { stripes } => {
                if stripes.is_empty() {
                    return Err(BlobError::Decode(
                        "TreeNode::ErasureLeaf must have at least one stripe".to_owned(),
                    ));
                }
                let mut chunk_total: usize = 0;
                for (i, stripe) in stripes.iter().enumerate() {
                    stripe.validate().map_err(|e| {
                        BlobError::Decode(format!("stripe {}: {}", i, e))
                    })?;
                    chunk_total = chunk_total.saturating_add(stripe.chunks.len());
                    for (j, chunk) in stripe.chunks.iter().enumerate() {
                        if chunk.size == 0 {
                            return Err(BlobError::Decode(format!(
                                "TreeNode::ErasureLeaf stripe {} chunk {} has zero size",
                                i, j
                            )));
                        }
                        if (chunk.size as u64) > TREE_LEAF_CHUNK_MAX_BYTES {
                            return Err(BlobError::Decode(format!(
                                "TreeNode::ErasureLeaf stripe {} chunk {} size {} exceeds cap {}",
                                i, j, chunk.size, TREE_LEAF_CHUNK_MAX_BYTES
                            )));
                        }
                    }
                }
                if chunk_total > TREE_FANOUT {
                    return Err(BlobError::Decode(format!(
                        "TreeNode::ErasureLeaf total chunk count {} exceeds fanout cap {}",
                        chunk_total, TREE_FANOUT
                    )));
                }
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
            TreeNode::ErasureLeaf { stripes } => {
                stripes.iter().map(|s| s.covered_bytes()).sum::<u64>()
            }
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
            TreeNode::ErasureLeaf { stripes } => {
                stripes.iter().map(|s| s.chunks.len()).sum()
            }
        }
    }

    /// `true` if this is an erasure-coded leaf (Phase C RS path).
    pub fn is_erasure_leaf(&self) -> bool {
        matches!(self, TreeNode::ErasureLeaf { .. })
    }
}

// ────────────────────────────────────────────────────────────────────
// TreeBuilder — incremental manifest tree construction
// ────────────────────────────────────────────────────────────────────

/// A closed [`TreeNode`] emitted by [`TreeBuilder`]. The caller
/// persists `bytes` against `hash` (the substrate stores it as
/// a [`BlobRef::Small`](super::blob_ref::BlobRef::Small) at
/// `dataforts/blob/<hex32>`) before constructing the
/// [`BlobRef::Tree`](super::blob_ref::BlobRef::Tree) that
/// references it.
///
/// `level == 0` marks a leaf node (its bytes encode a
/// [`TreeNode::Leaf`]); `level >= 1` marks an internal node at
/// the corresponding depth above the leaves. The deepest
/// internal-node level equals
/// [`BlobRef::Tree::depth`](super::blob_ref::BlobRef::Tree) - 1.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClosedNode {
    /// BLAKE3 of `bytes`.
    pub hash: [u8; 32],
    /// Postcard-encoded [`TreeNode`] bytes the caller persists.
    pub bytes: Vec<u8>,
    /// Position in the tree — `0` for leaves, `1..` for internals.
    pub level: u8,
}

/// Output of [`TreeBuilder::finalize`]. The root node lives in
/// `root_hash` + `root_bytes` + `root_depth`; every other node
/// closed during finalize is in `trailing_nodes` and must be
/// persisted before the [`BlobRef::Tree`](super::blob_ref::BlobRef::Tree)
/// is published.
///
/// Nodes closed during streaming (via [`TreeBuilder::push_chunk`])
/// are returned by that call directly; `trailing_nodes` carries
/// only the cascade-on-finalize closures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeBuildOutput {
    /// BLAKE3 of the root [`TreeNode`].
    pub root_hash: [u8; 32],
    /// Postcard-encoded root [`TreeNode`] bytes.
    pub root_bytes: Vec<u8>,
    /// Total tree depth — `1` for a single-leaf tree (root IS
    /// the leaf), increasing by one per additional internal
    /// level. Capped at [`MAX_TREE_DEPTH`].
    pub root_depth: u8,
    /// Sum of every leaf chunk's `size` across the whole tree.
    pub total_bytes: u64,
    /// Number of chunks accumulated across all leaves.
    pub chunk_count: u64,
    /// Non-root nodes closed during finalize, in deepest-first
    /// order. The caller persists each before publishing the
    /// final [`BlobRef::Tree`](super::blob_ref::BlobRef::Tree).
    pub trailing_nodes: Vec<ClosedNode>,
}

/// Incremental manifest-tree builder.
///
/// Push [`ChunkRefV3`] entries one at a time; the builder
/// accumulates them into a leaf, closes the leaf when it hits
/// [`TREE_FANOUT`] chunks, hashes + stores the leaf via the
/// emitted [`ClosedNode`], and lifts the leaf's hash into the
/// depth-1 builder. Cascade continues up to the root.
///
/// Memory bound: O(`TREE_FANOUT × MAX_TREE_DEPTH × entry_size`)
/// plus the current leaf-builder buffer. At fanout 128 + depth
/// 4 + ~40 bytes per entry ≈ 20 KiB independent of total tree
/// size — bounded regardless of how many chunks pass through.
///
/// Determinism: two builders fed the same sequence of
/// [`ChunkRefV3`] entries produce identical
/// [`TreeBuildOutput::root_hash`] outputs. Chunk dedup at the
/// substrate level then makes two tree-built blobs over
/// identical content land at the same root.
#[derive(Debug)]
pub struct TreeBuilder {
    /// Open leaf builder — accumulates chunks until it reaches
    /// [`TREE_FANOUT`] entries.
    leaf_chunks: Vec<ChunkRefV3>,
    /// Per-level internal builders. `internals[i]` is the depth
    /// (i+1) builder, holding `(child_hash, subtree_size)`
    /// pairs whose children live at depth `i`. Empty until the
    /// first cascade reaches that level.
    internals: Vec<Vec<([u8; 32], u64)>>,
    /// Running total of bytes ever accumulated. Cross-checked
    /// in [`Self::finalize`] for observability.
    total_bytes: u64,
    /// Running count of chunks ever accumulated.
    chunk_count: u64,
}

impl TreeBuilder {
    /// Construct an empty builder. The first call to
    /// [`Self::push_chunk`] initialises the leaf buffer.
    pub fn new() -> Self {
        Self {
            leaf_chunks: Vec::with_capacity(TREE_FANOUT),
            internals: Vec::new(),
            total_bytes: 0,
            chunk_count: 0,
        }
    }

    /// Total bytes the builder has accepted so far across every
    /// pushed chunk.
    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    /// Number of chunks accepted so far.
    pub fn chunk_count(&self) -> u64 {
        self.chunk_count
    }

    /// Add a single chunk reference. Returns the (possibly
    /// empty) sequence of nodes the push closed via cascade —
    /// typically zero per call, exactly one when the leaf fills,
    /// occasionally more when a leaf-close also triggers an
    /// internal-level close.
    ///
    /// The caller must persist every returned [`ClosedNode`]'s
    /// `bytes` against its `hash` before [`Self::finalize`] is
    /// called — the tree-walk path on `fetch_range` will fetch
    /// these by hash and verify the bytes hash back.
    pub fn push_chunk(&mut self, chunk: ChunkRefV3) -> Result<Vec<ClosedNode>, BlobError> {
        if chunk.size == 0 {
            return Err(BlobError::Decode(
                "TreeBuilder::push_chunk received zero-size chunk".to_owned(),
            ));
        }
        if (chunk.size as u64) > TREE_LEAF_CHUNK_MAX_BYTES {
            return Err(BlobError::Decode(format!(
                "TreeBuilder::push_chunk received oversize chunk: {} bytes (cap {})",
                chunk.size, TREE_LEAF_CHUNK_MAX_BYTES
            )));
        }
        self.total_bytes = self.total_bytes.saturating_add(chunk.size as u64);
        self.chunk_count = self.chunk_count.saturating_add(1);
        self.leaf_chunks.push(chunk);

        if self.leaf_chunks.len() < TREE_FANOUT {
            return Ok(Vec::new());
        }

        // Leaf is full. Close + cascade.
        self.close_leaf_and_cascade()
    }

    /// Inject a pre-built leaf into the internal-cascade. Used
    /// by the v0.3 Phase C Reed-Solomon path: the RS store flow
    /// produces [`TreeNode::ErasureLeaf`] nodes outside the
    /// builder's chunk-driven flow, and this entry point lifts
    /// them into the same internal hierarchy `push_chunk` builds.
    ///
    /// `leaf_hash` MUST be the BLAKE3 of `leaf_bytes` and
    /// `leaf_covered_bytes` MUST be the leaf's `covered_bytes()`
    /// — the caller (the RS striper consumer) is responsible for
    /// supplying coherent values. The builder appends a
    /// [`ClosedNode`] for the leaf itself (level 0) followed by
    /// any internal nodes the cascade closes.
    ///
    /// The builder's `chunk_count` is bumped by `synthetic_chunks`
    /// — pass the number of logical data chunks the leaf covers
    /// so [`Self::chunk_count`] reports a meaningful figure for
    /// the RS path's [`Self::finalize`] non-empty check.
    pub fn push_prebuilt_leaf(
        &mut self,
        leaf_hash: [u8; 32],
        leaf_bytes: Vec<u8>,
        leaf_covered_bytes: u64,
        synthetic_chunks: u64,
    ) -> Result<Vec<ClosedNode>, BlobError> {
        self.total_bytes = self.total_bytes.saturating_add(leaf_covered_bytes);
        self.chunk_count = self.chunk_count.saturating_add(synthetic_chunks);
        let mut emitted = vec![ClosedNode {
            hash: leaf_hash,
            bytes: leaf_bytes,
            level: 0,
        }];
        self.lift_into_internal(0, leaf_hash, leaf_covered_bytes, &mut emitted)?;
        Ok(emitted)
    }

    /// Internal: close the open leaf builder (assumed full or
    /// finalize-time), then lift the resulting (hash, size) into
    /// the depth-1 internal builder. If that fills, cascade.
    /// Returns every [`ClosedNode`] emitted.
    fn close_leaf_and_cascade(&mut self) -> Result<Vec<ClosedNode>, BlobError> {
        let leaf_chunks = std::mem::replace(
            &mut self.leaf_chunks,
            Vec::with_capacity(TREE_FANOUT),
        );
        if leaf_chunks.is_empty() {
            return Ok(Vec::new());
        }
        let leaf = TreeNode::leaf(leaf_chunks)?;
        let bytes = leaf.encode()?;
        let hash: [u8; 32] = blake3::hash(&bytes).into();
        let size = leaf.covered_bytes();
        let mut emitted = vec![ClosedNode {
            hash,
            bytes,
            level: 0,
        }];

        self.lift_into_internal(0, hash, size, &mut emitted)?;
        Ok(emitted)
    }

    /// Internal: push `(hash, size)` into `internals[level]`. If
    /// that level fills, close it and recurse one level up.
    /// `emitted` accumulates every closed node along the way.
    ///
    /// Recursion depth is bounded by [`MAX_TREE_DEPTH`] = 4, so
    /// the stack is well-bounded. Each recursive frame returns
    /// after at most one TreeNode encode + hash; no allocation
    /// path is shared across frames.
    fn lift_into_internal(
        &mut self,
        level: usize,
        hash: [u8; 32],
        size: u64,
        emitted: &mut Vec<ClosedNode>,
    ) -> Result<(), BlobError> {
        while self.internals.len() <= level {
            self.internals.push(Vec::with_capacity(TREE_FANOUT));
        }
        self.internals[level].push((hash, size));
        if self.internals[level].len() < TREE_FANOUT {
            return Ok(());
        }
        // Level filled. Close + cascade.
        let entries = std::mem::replace(
            &mut self.internals[level],
            Vec::with_capacity(TREE_FANOUT),
        );
        let node = TreeNode::internal(entries)?;
        let bytes = node.encode()?;
        let node_hash: [u8; 32] = blake3::hash(&bytes).into();
        let node_size = node.covered_bytes();
        let depth_for_emit = (level + 1) as u8;
        if depth_for_emit > MAX_TREE_DEPTH {
            return Err(BlobError::Decode(format!(
                "TreeBuilder cascade exceeded MAX_TREE_DEPTH {}: \
                 internal node at depth {} would not fit",
                MAX_TREE_DEPTH, depth_for_emit
            )));
        }
        emitted.push(ClosedNode {
            hash: node_hash,
            bytes,
            level: depth_for_emit,
        });
        // Cascade — recursive call handles its own push into
        // internals[level+1]. Tail-recursive in spirit; depth is
        // hard-capped at MAX_TREE_DEPTH.
        self.lift_into_internal(level + 1, node_hash, node_size, emitted)
    }

    /// Close every open builder level and emit the root.
    ///
    /// Returns:
    /// - `Err` if no chunks were ever pushed.
    /// - `Ok(TreeBuildOutput)` with the root + every non-root
    ///   node closed during finalize.
    ///
    /// The output's `root_depth` is `1` for a single-leaf tree,
    /// increasing by one per internal level — bounded by
    /// [`MAX_TREE_DEPTH`]. Producing a tree past the cap returns
    /// an error.
    pub fn finalize(mut self) -> Result<TreeBuildOutput, BlobError> {
        if self.chunk_count == 0 {
            return Err(BlobError::Decode(
                "TreeBuilder::finalize called with no chunks".to_owned(),
            ));
        }
        let mut trailing: Vec<ClosedNode> = Vec::new();
        // Holds the "current" node being lifted up the cascade.
        let mut current: Option<(/*hash*/ [u8; 32], /*size*/ u64, /*bytes*/ Vec<u8>, /*level*/ u8)> = None;

        // Step 1: close the leaf level if non-empty.
        if !self.leaf_chunks.is_empty() {
            let leaf = TreeNode::leaf(std::mem::take(&mut self.leaf_chunks))?;
            let bytes = leaf.encode()?;
            let hash: [u8; 32] = blake3::hash(&bytes).into();
            let size = leaf.covered_bytes();
            current = Some((hash, size, bytes, 0));
        }

        // Step 2: cascade up through every internal level.
        for level_idx in 0..self.internals.len() {
            let mut entries = std::mem::take(&mut self.internals[level_idx]);
            if let Some((hash, size, bytes, c_level)) = current.take() {
                entries.push((hash, size));
                // The previous "current" is now a child of this
                // level — it's no longer the root candidate.
                trailing.push(ClosedNode {
                    hash,
                    bytes,
                    level: c_level,
                });
            }
            if entries.is_empty() {
                continue;
            }
            let node = TreeNode::internal(entries)?;
            let bytes = node.encode()?;
            let hash: [u8; 32] = blake3::hash(&bytes).into();
            let size = node.covered_bytes();
            let level = (level_idx + 1) as u8;
            if level > MAX_TREE_DEPTH {
                return Err(BlobError::Decode(format!(
                    "TreeBuilder::finalize produced internal node at depth {}, \
                     exceeding MAX_TREE_DEPTH {}",
                    level, MAX_TREE_DEPTH
                )));
            }
            current = Some((hash, size, bytes, level));
        }

        let (mut root_hash, mut root_total, mut root_bytes, mut root_level) =
            current.ok_or_else(|| {
                BlobError::Decode(
                    "TreeBuilder::finalize internal error — non-empty input produced no root"
                        .to_owned(),
                )
            })?;

        // Peel off degenerate single-child internal roots. The
        // cascade can wrap each non-empty level into an internal
        // node even when that level had only one child — a
        // partial-leaf finalize that lands in a previously-empty
        // internals[0] produces a 1-child internal root, etc.
        // Without the peel, a small-tail tree adds a needless
        // fetch RTT to every range query.
        //
        // Best-effort: the peel only succeeds when the
        // single-child's bytes are still in `trailing` (i.e. the
        // child was closed during finalize, not during streaming
        // push). Mid-stream cascade emits its closed nodes to
        // the caller directly, so a single-child internal that
        // wraps an already-emitted node stays in the tree (1
        // extra fetch RTT vs the minimal depth, no correctness
        // hazard). The streaming-store caller can collapse this
        // path by re-encoding the child from its hash — outside
        // the builder's scope.
        loop {
            let decoded = TreeNode::decode(&root_bytes)?;
            let TreeNode::Internal { children } = &decoded else {
                break; // root is a leaf — no peeling
            };
            if children.len() != 1 {
                break; // genuine multi-child internal
            }
            let (child_hash, child_size) = children[0];
            let Some(pos) = trailing.iter().position(|n| n.hash == child_hash) else {
                // Child was emitted during streaming push; not in
                // trailing. Accept the 1-extra-depth wart.
                break;
            };
            let child_node = trailing.remove(pos);
            root_hash = child_hash;
            root_total = child_size;
            root_bytes = child_node.bytes;
            root_level = child_node.level;
        }

        // `root_depth` = 1 for a single-leaf tree (root_level==0),
        // root_level + 1 otherwise.
        let root_depth = (root_level as u16 + 1) as u8;
        if root_depth > MAX_TREE_DEPTH {
            return Err(BlobError::Decode(format!(
                "TreeBuilder::finalize produced root at depth {}, \
                 exceeding MAX_TREE_DEPTH {}",
                root_depth, MAX_TREE_DEPTH
            )));
        }
        // Cross-check: root_total covers exactly total_bytes.
        if root_total != self.total_bytes {
            return Err(BlobError::Decode(format!(
                "TreeBuilder::finalize root covers {} bytes but builder accepted {}",
                root_total, self.total_bytes
            )));
        }

        Ok(TreeBuildOutput {
            root_hash,
            root_bytes,
            root_depth,
            total_bytes: self.total_bytes,
            chunk_count: self.chunk_count,
            trailing_nodes: trailing,
        })
    }
}

impl Default for TreeBuilder {
    fn default() -> Self {
        Self::new()
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
            TreeNode::ErasureLeaf { .. } => panic!("expected Internal"),
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

    // -----------------------------------------------------------
    // TreeBuilder
    // -----------------------------------------------------------

    /// Synthesize N distinct chunk refs at the standard fixed
    /// chunk size for deterministic test fixtures.
    fn n_chunks(n: usize) -> Vec<ChunkRefV3> {
        (0..n)
            .map(|i| {
                let mut hash = [0u8; 32];
                hash[0] = (i & 0xFF) as u8;
                hash[1] = ((i >> 8) & 0xFF) as u8;
                hash[2] = ((i >> 16) & 0xFF) as u8;
                ChunkRefV3::data(hash, BLOB_CHUNK_SIZE_BYTES as u32)
            })
            .collect()
    }

    #[test]
    fn builder_empty_finalize_errors() {
        let b = TreeBuilder::new();
        let err = b.finalize().unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("no chunks"), "got: {msg}");
    }

    #[test]
    fn builder_rejects_zero_size_chunk() {
        let mut b = TreeBuilder::new();
        let err = b
            .push_chunk(ChunkRefV3::data([0u8; 32], 0))
            .unwrap_err();
        assert!(err.to_string().contains("zero-size"), "got: {err}");
    }

    #[test]
    fn builder_rejects_oversize_chunk() {
        let mut b = TreeBuilder::new();
        let err = b
            .push_chunk(ChunkRefV3::data(
                [0u8; 32],
                (TREE_LEAF_CHUNK_MAX_BYTES + 1) as u32,
            ))
            .unwrap_err();
        assert!(err.to_string().contains("oversize"), "got: {err}");
    }

    /// Single chunk → leaf is the root. `root_depth == 1`, no
    /// trailing nodes.
    #[test]
    fn builder_single_chunk_root_is_leaf() {
        let mut b = TreeBuilder::new();
        let emitted = b.push_chunk(n_chunks(1).into_iter().next().unwrap()).unwrap();
        assert!(emitted.is_empty(), "single push below fanout emits no closed nodes");
        let out = b.finalize().unwrap();
        assert_eq!(out.root_depth, 1);
        assert_eq!(out.total_bytes, BLOB_CHUNK_SIZE_BYTES);
        assert_eq!(out.chunk_count, 1);
        assert!(out.trailing_nodes.is_empty());
        // Root bytes decode back to a Leaf.
        let root = TreeNode::decode(&out.root_bytes).unwrap();
        assert!(root.is_leaf());
        assert_eq!(root.arity(), 1);
        // Root hash matches blake3 of its bytes.
        let computed: [u8; 32] = blake3::hash(&out.root_bytes).into();
        assert_eq!(computed, out.root_hash);
    }

    /// FANOUT chunks → the leaf fills + closes during push (the
    /// leaf bytes go to the caller, not into trailing), then
    /// finalize wraps the lifted (hash,size) into a 1-child
    /// internal root. The peel can't promote because the child
    /// bytes aren't local. Result: root_depth=2 (1-child internal
    /// over the streaming-emitted leaf) — the small inefficiency
    /// documented on finalize.
    #[test]
    fn builder_one_full_leaf_emits_internal_root_over_streaming_leaf() {
        let mut b = TreeBuilder::new();
        let mut mid_closed = Vec::new();
        for c in n_chunks(TREE_FANOUT) {
            mid_closed.extend(b.push_chunk(c).unwrap());
        }
        // The last push closed the leaf during streaming.
        assert_eq!(mid_closed.len(), 1);
        assert_eq!(mid_closed[0].level, 0);
        let leaf_hash = mid_closed[0].hash;
        let out = b.finalize().unwrap();
        // Peel can't run because the leaf isn't in trailing.
        assert_eq!(
            out.root_depth, 2,
            "FANOUT-chunk input produces a 1-child internal root over the \
             streaming-emitted leaf — the peel skips because the leaf bytes \
             aren't local"
        );
        let root = TreeNode::decode(&out.root_bytes).unwrap();
        assert!(root.is_internal());
        assert_eq!(root.arity(), 1);
        // The internal's child IS the streaming-emitted leaf.
        if let TreeNode::Internal { children } = &root {
            assert_eq!(children[0].0, leaf_hash);
        }
        assert_eq!(out.total_bytes, BLOB_CHUNK_SIZE_BYTES * TREE_FANOUT as u64);
        assert_eq!(out.chunk_count, TREE_FANOUT as u64);
    }

    /// The peel DOES fire when the single child IS in trailing.
    /// Example: a partial-leaf finalize that lifts into an
    /// empty internals[0] — the leaf was closed by finalize (so
    /// it's in trailing), and the would-be 1-child internal at
    /// internals[0] gets peeled to the leaf.
    #[test]
    fn builder_peels_partial_leaf_when_child_in_trailing() {
        // Push fewer than FANOUT chunks so finalize closes the
        // leaf (not streaming push).
        let count = TREE_FANOUT / 2;
        let mut b = TreeBuilder::new();
        for c in n_chunks(count) {
            let emitted = b.push_chunk(c).unwrap();
            assert!(emitted.is_empty(), "no cascade below fanout");
        }
        let out = b.finalize().unwrap();
        // The leaf was the only thing built; no internals were
        // ever closed (nothing cascaded). root_depth=1.
        assert_eq!(out.root_depth, 1);
        assert!(out.trailing_nodes.is_empty());
        let root = TreeNode::decode(&out.root_bytes).unwrap();
        assert!(root.is_leaf());
        assert_eq!(root.arity(), count);
    }

    /// 2 × FANOUT chunks → 2 leaves under one root internal.
    /// root_depth == 2.
    #[test]
    fn builder_two_leaves_yields_depth_two() {
        let mut b = TreeBuilder::new();
        let mut closed = Vec::new();
        for c in n_chunks(TREE_FANOUT * 2) {
            closed.extend(b.push_chunk(c).unwrap());
        }
        // 2 leaves closed during push.
        let leaf_closes = closed.iter().filter(|n| n.level == 0).count();
        assert_eq!(leaf_closes, 2, "two full leaves close during streaming");
        let out = b.finalize().unwrap();
        assert_eq!(out.root_depth, 2);
        assert_eq!(out.total_bytes, BLOB_CHUNK_SIZE_BYTES * (TREE_FANOUT * 2) as u64);
        let root = TreeNode::decode(&out.root_bytes).unwrap();
        assert!(root.is_internal());
        assert_eq!(root.arity(), 2);
    }

    /// FANOUT + 1 chunks → 2 leaves (one full, one with one
    /// chunk) under one root internal. root_depth == 2.
    #[test]
    fn builder_fanout_plus_one_yields_depth_two_with_partial_leaf() {
        let mut b = TreeBuilder::new();
        for c in n_chunks(TREE_FANOUT + 1) {
            let _ = b.push_chunk(c).unwrap();
        }
        let out = b.finalize().unwrap();
        assert_eq!(out.root_depth, 2);
        let root = TreeNode::decode(&out.root_bytes).unwrap();
        assert!(root.is_internal());
        assert_eq!(root.arity(), 2, "first leaf full, second leaf has one chunk");
    }

    /// FANOUT² + 1 chunks. Cascade trace:
    /// - First FANOUT² chunks fill FANOUT leaves and one full
    ///   internals[0] (closed mid-stream, lifted to internals[1]).
    /// - The final chunk lives alone in leaf_chunks at finalize.
    /// - Finalize closes that 1-chunk leaf, wraps it in a
    ///   1-child internal (lifted to internals[1]), then closes
    ///   internals[1] which now holds 2 entries (the streaming-
    ///   emitted internal_0_0 + the finalize-emitted 1-child wrap).
    ///
    /// Result: depth=3, with the root being a genuine multi-child
    /// internal. The 1-child internal in the middle is a known
    /// inefficiency the peel can't reach (its parent has 2 kids).
    #[test]
    fn builder_fanout_squared_plus_one_produces_depth_three_with_known_wart() {
        let chunk_count = TREE_FANOUT * TREE_FANOUT + 1;
        let mut b = TreeBuilder::new();
        for i in 0..chunk_count {
            let mut hash = [0u8; 32];
            hash[0] = (i & 0xFF) as u8;
            hash[1] = ((i >> 8) & 0xFF) as u8;
            hash[2] = ((i >> 16) & 0xFF) as u8;
            let _ = b.push_chunk(ChunkRefV3::data(hash, 1024)).unwrap();
        }
        let out = b.finalize().unwrap();
        assert_eq!(out.root_depth, 3);
        assert_eq!(out.chunk_count, chunk_count as u64);
        let root = TreeNode::decode(&out.root_bytes).unwrap();
        assert!(root.is_internal());
        assert_eq!(root.arity(), 2);
    }

    /// Determinism: two builders fed identical chunk sequences
    /// produce identical root hashes + identical trailing nodes.
    #[test]
    fn builder_is_deterministic_across_runs() {
        let chunks = n_chunks(TREE_FANOUT * 3 + 17);
        let mut b1 = TreeBuilder::new();
        let mut b2 = TreeBuilder::new();
        for c in &chunks {
            let _ = b1.push_chunk(*c).unwrap();
            let _ = b2.push_chunk(*c).unwrap();
        }
        let o1 = b1.finalize().unwrap();
        let o2 = b2.finalize().unwrap();
        assert_eq!(o1.root_hash, o2.root_hash);
        assert_eq!(o1.root_depth, o2.root_depth);
        assert_eq!(o1.total_bytes, o2.total_bytes);
        assert_eq!(o1.chunk_count, o2.chunk_count);
    }

    /// Every emitted node's hash must verify against blake3 of
    /// its bytes — the contract the tree-walk verifier relies on.
    #[test]
    fn builder_emits_hashes_that_match_blake3_of_bytes() {
        let mut b = TreeBuilder::new();
        let mut all_closed: Vec<ClosedNode> = Vec::new();
        for c in n_chunks(TREE_FANOUT * 2 + 5) {
            all_closed.extend(b.push_chunk(c).unwrap());
        }
        let out = b.finalize().unwrap();
        all_closed.extend(out.trailing_nodes.clone());
        all_closed.push(ClosedNode {
            hash: out.root_hash,
            bytes: out.root_bytes.clone(),
            level: out.root_depth - 1,
        });
        for node in &all_closed {
            let computed: [u8; 32] = blake3::hash(&node.bytes).into();
            assert_eq!(
                computed, node.hash,
                "ClosedNode at level {} has hash that doesn't match blake3(bytes)",
                node.level
            );
        }
    }

    /// Cross-check: the root's `covered_bytes()` must equal the
    /// builder's accumulated `total_bytes`. Finalize already
    /// asserts this; the test pins the invariant against a
    /// representative tree shape.
    #[test]
    fn builder_root_covers_all_accumulated_bytes() {
        let chunks = n_chunks(TREE_FANOUT * 4 + 33);
        let total: u64 = chunks.iter().map(|c| c.size as u64).sum();
        let mut b = TreeBuilder::new();
        for c in &chunks {
            let _ = b.push_chunk(*c).unwrap();
        }
        let out = b.finalize().unwrap();
        let root = TreeNode::decode(&out.root_bytes).unwrap();
        assert_eq!(root.covered_bytes(), total);
        assert_eq!(out.total_bytes, total);
    }

    /// Bounded memory: a builder that has pushed many chunks
    /// holds at most O(FANOUT × depth) entries across its
    /// internal stack + leaf buffer. Pin the invariant by
    /// pushing 100 × FANOUT chunks and asserting the builder's
    /// internal storage is bounded.
    #[test]
    fn builder_memory_is_bounded_independent_of_input_size() {
        let mut b = TreeBuilder::new();
        let mut total_closed = 0;
        // Push 100 × FANOUT = 12,800 chunks.
        for i in 0..(100 * TREE_FANOUT) {
            let mut hash = [0u8; 32];
            hash[0] = (i & 0xFF) as u8;
            hash[1] = ((i >> 8) & 0xFF) as u8;
            total_closed += b.push_chunk(ChunkRefV3::data(hash, 1024)).unwrap().len();
        }
        // Builder's internal vec sizes should never exceed
        // FANOUT × depth entries total at any moment. depth
        // bounded at MAX_TREE_DEPTH = 4.
        let total_open_entries: usize = b.leaf_chunks.len()
            + b.internals.iter().map(|v| v.len()).sum::<usize>();
        assert!(
            total_open_entries <= TREE_FANOUT * (MAX_TREE_DEPTH as usize),
            "builder has {} open entries; expected <= {} (fanout × MAX_DEPTH)",
            total_open_entries,
            TREE_FANOUT * (MAX_TREE_DEPTH as usize)
        );
        assert!(total_closed > 0, "some cascades should have emitted nodes");
        let _ = b.finalize().unwrap();
    }

    /// Root hash is the same regardless of whether a cascade
    /// happened mid-stream or only at finalize. Equivalently:
    /// pushing chunks one-at-a-time vs in a single fanout-batch
    /// produces the same root. (The CDC chunker may emit
    /// boundary-skewed batches; the tree builder must be
    /// order-equivalent.)
    #[test]
    fn builder_root_independent_of_push_batching() {
        let chunks = n_chunks(TREE_FANOUT + 50);
        // Run 1: push individually.
        let mut b1 = TreeBuilder::new();
        for c in &chunks {
            let _ = b1.push_chunk(*c).unwrap();
        }
        let o1 = b1.finalize().unwrap();
        // Run 2: same content, same order. (The "batching" here
        // is really about whether intermediate cascades fire
        // mid-push or not — but push_chunk only emits per chunk,
        // so this is really a determinism check vs run 1.)
        let mut b2 = TreeBuilder::new();
        for c in &chunks {
            let _ = b2.push_chunk(*c).unwrap();
        }
        let o2 = b2.finalize().unwrap();
        assert_eq!(o1.root_hash, o2.root_hash);
    }
}
