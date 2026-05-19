//! `BlobRef` — typed event-payload that points at content stored
//! out-of-band in a [`super::BlobAdapter`] backend.
//!
//! # Wire encoding (v0.15 Small + v0.2 Manifest)
//!
//! Every encoded form starts with the four-byte magic
//! `[0xB0, 0xB1, 0xB2, 0xB3]` followed by a one-byte version
//! discriminant:
//!
//! | Version | Variant | Body layout |
//! |---|---|---|
//! | `0x01` | [`BlobRef::Small`] | `[hash 32][size 8][uri …]` — hand-rolled byte layout, v0.15-compatible. |
//! | `0x02` | [`BlobRef::Manifest`] | `[postcard manifest body …]` — chunked content. See [`BLOB_MANIFEST_BODY_VERSION`]. |
//!
//! No length prefix on the Small URI — the encoded form lives inside
//! an event payload whose length is already framed by the substrate.
//! The Manifest body is postcard-encoded with its own 1-byte version
//! prefix (`BLOB_MANIFEST_BODY_VERSION`) so the manifest schema can
//! evolve independently of the outer wire discriminant.
//!
//! Inline event payloads carry no magic (back-compat); the substrate
//! distinguishes by peeking at the first four bytes. The magic is
//! four bytes (rather than one) because a single discriminator byte
//! (`0xB0`) collides with arbitrary binary payloads — protobuf wire
//! bytes, MessagePack, compressed data — and a false match would
//! silently re-interpret an inline payload as a `BlobRef` whose
//! decoded URI gets fetched against the channel's adapter. A
//! four-byte magic with three high-bit bytes is statistically
//! unreachable in valid UTF-8 text and rare enough in binary that
//! decode-then-verify catches the rest.
//!
//! # Chunking
//!
//! Payloads above [`BLOB_CHUNK_SIZE_BYTES`] (4 MiB) split into
//! fixed-size chunks; the resulting [`BlobRef::Manifest`] carries the
//! chunk list. Below the threshold, payloads ride as a single
//! [`BlobRef::Small`]. Chunk size is fixed across versions for
//! determinism: two callers chunking the same N-byte payload produce
//! identical [`ChunkRef`] lists, which deduplicates at the
//! replication layer for free. See [`chunk_payload`] for the
//! algorithm + [`byte_range_to_chunks`] for the inverse (resolving a
//! byte range to chunk indices for partial fetches).

use serde::{Deserialize, Serialize};

use super::error::BlobError;

/// 4-byte magic at offset 0 of an encoded [`BlobRef`].
/// Distinguishes blob-ref payloads from inline event payloads on
/// every `read_range` / `tail` output. Single-byte discriminators
/// collide too readily with arbitrary binary payloads; four
/// high-bit bytes are improbable enough that decode-then-verify
/// handles the residual cases without misinterpreting attacker-
/// controlled bytes as a `BlobRef`.
pub const BLOB_REF_MAGIC: [u8; 4] = [0xB0, 0xB1, 0xB2, 0xB3];

/// Backwards-compatible single-byte discriminator alias for code
/// paths that just need to peek byte 0 (e.g. the bindings'
/// `EventPayload` classification). Equal to `BLOB_REF_MAGIC[0]`.
/// The decoder still requires the full four-byte magic, so this
/// alias is only useful for a cheap "might be a blob" pre-check.
pub const BLOB_REF_DISCRIMINATOR: u8 = BLOB_REF_MAGIC[0];

/// `BlobRef::Small` wire-encoding version. v1 is the only Small
/// version this build encodes; the version byte is reserved so
/// future migrations (e.g. BLAKE3-256 → BLAKE3-512, or a multi-hash
/// format) can land without breaking the decoder.
pub const BLOB_REF_VERSION_V1: u8 = 0x01;

/// `BlobRef::Manifest` wire-encoding version. Lands in v0.2 alongside
/// the mesh-native blob storage track. Manifest body schema evolves
/// independently via [`BLOB_MANIFEST_BODY_VERSION`].
pub const BLOB_REF_VERSION_V2_MANIFEST: u8 = 0x02;

/// `BlobRef::Tree` wire-encoding version. Lands in v0.3 alongside
/// the hierarchical-manifest terabyte-scale track. Tree body
/// schema evolves independently via [`BLOB_TREE_BODY_VERSION`].
pub const BLOB_REF_VERSION_V3_TREE: u8 = 0x03;

/// Inner-version prefix on the postcard-encoded tree body. Bumps
/// independently of the outer wire discriminator
/// ([`BLOB_REF_VERSION_V3_TREE`]) so the tree body schema can
/// evolve without re-cutting the outer version space.
pub const BLOB_TREE_BODY_VERSION: u8 = 0x01;

/// Hard ceiling on the postcard-encoded `BlobRef::Tree` body.
/// Tree bodies are tiny by design (a few hashes + ints), so a
/// 1 KiB cap is generous and bounds the decoder's allocator
/// before per-field validation runs.
pub const BLOB_REF_TREE_BODY_MAX_BYTES: usize = 1024;

/// Hard ceiling on `BlobRef::Tree::total_size`. Equals the
/// fanout 128 + depth 4 + 4 MiB chunk maximum: 128 × 128 × 128
/// × 128 × 4 MiB = 128 PiB = 2^57 bytes. Bounded so a malicious
/// or buggy publisher can't stamp `total_size = u64::MAX` and
/// propagate it into `Vec::with_capacity` allocations downstream.
pub const BLOB_TREE_MAX_TOTAL_SIZE: u64 = 128 * (1u64 << 50);

/// Inner-version prefix on the postcard-encoded manifest body. Bumps
/// independently of the outer wire discriminator
/// ([`BLOB_REF_VERSION_V2_MANIFEST`]) so the manifest schema can
/// evolve (extra fields, new encodings, etc.) without re-cutting the
/// outer version space.
pub const BLOB_MANIFEST_BODY_VERSION: u8 = 0x01;

/// Minimum encoded length for a [`BlobRef::Small`]: magic + version
/// + hash + size. URI may be empty.
pub const BLOB_REF_SMALL_HEADER_LEN: usize = 4 + 1 + 32 + 8;

/// Hard ceiling on any single blob payload — applies to both the
/// `size` field on a [`BlobRef::Small`] and the `total_size` field on
/// a [`BlobRef::Manifest`]. A malicious or buggy publisher could
/// otherwise stamp `size = u64::MAX` which then propagates into
/// `vec![0u8; len as usize]` allocations on the fetch path — OOMs on
/// 64-bit targets, silent truncation to short reads on 32-bit. 16
/// GiB is generous enough for legitimate multi-GB blobs while still
/// bounded; sites that need higher should validate on construction
/// and consider streaming (the BlobAdapter trait's streaming hooks
/// are the right escape valve).
pub const BLOB_REF_MAX_SIZE: u64 = 16 * 1024 * 1024 * 1024;

/// Fixed chunk size for chunked storage. 4 MiB is the locked
/// threshold per [`DATAFORTS_BLOB_STORAGE_PLAN.md`] — fixed across
/// versions for determinism (two callers chunking the same N-byte
/// payload produce identical [`ChunkRef`] lists, which deduplicates
/// at the replication layer for free). Payloads at or below this
/// threshold ride as a single [`BlobRef::Small`]; above it, the
/// chunker emits a [`BlobRef::Manifest`].
///
/// [`DATAFORTS_BLOB_STORAGE_PLAN.md`]: ../../../../../docs/plans/DATAFORTS_BLOB_STORAGE_PLAN.md
pub const BLOB_CHUNK_SIZE_BYTES: u64 = 4 * 1024 * 1024;

/// Hard ceiling on the number of chunks a single
/// [`BlobRef::Manifest`] may carry. 4 GiB / 4 MiB = 1024 chunks at
/// the typical max-blob size; 16 GiB / 4 MiB = 4096 chunks at the
/// `BLOB_REF_MAX_SIZE` cap. The cap protects the decoder from a
/// malicious peer stamping `chunks: Vec<…>` with tens of millions of
/// entries (the postcard varint length prefix would otherwise admit
/// up to `u32::MAX` and OOM the decoder).
pub const BLOB_MANIFEST_MAX_CHUNKS: usize = 8192;

/// Replication encoding for a chunked blob. v0.2 only supports
/// `Replicated`; `ReedSolomon { k, m }` is reserved on the wire so
/// v0.3 can land erasure coding without a manifest format change.
///
/// Wire-encoded via postcard; the unit-variant `Replicated`
/// occupies 1 byte (varint discriminant 0), `ReedSolomon { k, m }`
/// occupies 3 bytes (varint 1 + two `u8`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Encoding {
    /// N identical replicas of every chunk; the only encoding
    /// supported in v0.2. Survives loss of `replication_factor - 1`
    /// nodes per chunk; correlated failures depend on placement
    /// tags. See `DATAFORTS_BLOB_STORAGE_PLAN.md` § W-2.
    Replicated,
    /// Reed–Solomon `(k, m)` erasure coding. **Reserved for v0.3**;
    /// constructing this variant is allowed for forward-compat
    /// testing, but the v0.2 store / fetch paths reject it with a
    /// `BlobError::UnsupportedEncoding` variant added in PR-2.
    ReedSolomon {
        /// Data chunks per group.
        k: u8,
        /// Parity chunks per group.
        m: u8,
    },
}

/// Reference to a single chunk within a [`BlobRef::Manifest`].
/// Each chunk is a content-addressed RedEX file in the mesh-native
/// storage path (v0.2). The hash is BLAKE3-256 of the chunk's raw
/// bytes; `size` is the chunk's payload length in bytes (≤
/// [`BLOB_CHUNK_SIZE_BYTES`]; only the last chunk may be smaller).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChunkRef {
    /// BLAKE3-256 of the chunk's canonical bytes.
    pub hash: [u8; 32],
    /// Chunk payload length in bytes. Bounded above by
    /// [`BLOB_CHUNK_SIZE_BYTES`]; only the last chunk in a manifest
    /// may be strictly smaller.
    pub size: u32,
}

/// Postcard-encoded tree body. Lives inside the
/// [`BlobRef::Tree`] wire form after the four-byte magic +
/// version discriminator. The body itself is tiny — fixed-size
/// fields only; no embedded chunk list (the chunks live at the
/// referenced [`TreeNode`](super::blob_tree::TreeNode) leaves).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct TreeBody {
    /// Body schema version; bumps independently of the outer
    /// `BlobRef::Tree` discriminant.
    body_version: u8,
    /// Adapter-routed URI. For the mesh-native path this is
    /// `mesh://<hex-of-root_hash>`; external adapters use their
    /// own scheme.
    uri: String,
    /// Replication / erasure encoding for the chunks. Tree
    /// inherits the same enum surface as Manifest.
    encoding: Encoding,
    /// BLAKE3 hash of the root
    /// [`TreeNode`](super::blob_tree::TreeNode) body. The
    /// substrate fetches the root, verifies its bytes hash to
    /// this value, then walks down.
    root_hash: [u8; 32],
    /// Total reconstructed payload size in bytes. The decoder
    /// trusts this value (same trust model as Manifest's
    /// `total_size`); the tree walk cross-checks against the
    /// sum of leaf chunk sizes at the bottom of each descent.
    total_size: u64,
    /// Tree depth — `0` is a single-leaf tree (root IS the leaf,
    /// degenerate), `1` is root + leaves, `2` is root + internals +
    /// leaves, etc. Capped at [`super::blob_tree::MAX_TREE_DEPTH`]
    /// (currently 4).
    depth: u8,
}

/// Postcard-encoded manifest body. Lives inside the
/// [`BlobRef::Manifest`] wire form after the four-byte magic +
/// version discriminator.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct ManifestBody {
    /// Body schema version; bumps independently of the outer
    /// `BlobRef::Manifest` discriminant.
    body_version: u8,
    /// Adapter-routed URI — e.g. `mesh://<hex>`, `s3://bucket/key`.
    /// The scheme picks the adapter; the rest is passed through
    /// opaque.
    uri: String,
    /// Replication / erasure encoding for the chunks.
    encoding: Encoding,
    /// Ordered chunk list. Position N in the vector corresponds to
    /// the byte range `[N * BLOB_CHUNK_SIZE_BYTES, …)`.
    chunks: Vec<ChunkRef>,
    /// Sum of every chunk's `size`. Cached for cheap `BlobRef::size`
    /// without iterating the vector; validated on decode to match
    /// the iterated sum.
    total_size: u64,
}

/// Pointer to content stored out-of-band. Round-trips through every
/// binding as a typed value via the public fields; the substrate
/// uses [`Self::encode`] / [`Self::decode`] for the wire form.
///
/// Two variants:
///
/// - [`BlobRef::Small`] — payload ≤ [`BLOB_CHUNK_SIZE_BYTES`]; a
///   single content-addressed blob. Wire-compatible with v0.15.
/// - [`BlobRef::Manifest`] — payload > [`BLOB_CHUNK_SIZE_BYTES`];
///   carries an ordered [`ChunkRef`] list plus an [`Encoding`]
///   discriminant. Each chunk is itself a content-addressed Small
///   blob stored independently via the adapter; the manifest exists
///   only as the routing structure that ties them together.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum BlobRef {
    /// Single-blob path. Wire-compatible with v0.15.
    Small {
        /// Encoding version byte. Always [`BLOB_REF_VERSION_V1`] on
        /// fresh constructions; decode preserves the on-wire value so
        /// upstream code can detect forward-compat scenarios.
        version: u8,
        /// Adapter-routed URI — e.g. `s3://bucket/key`,
        /// `ipfs://<cid>`, `file:///abs/path`, `mesh://<hex>`. The
        /// scheme picks the adapter; the rest is passed through
        /// opaque.
        uri: String,
        /// BLAKE3-256 hash of the canonical bytes the URI resolves
        /// to. The substrate verifies this on every successful
        /// fetch; an adversarial adapter cannot fake-verify because
        /// the check runs in the substrate, not the adapter.
        hash: [u8; 32],
        /// Size of the resolved content in bytes. Range-fetch
        /// callers use this to bound their reads; the verification
        /// path uses it to short-circuit obviously-wrong payloads.
        size: u64,
    },
    /// Chunked-blob path (v0.2). Wire version
    /// [`BLOB_REF_VERSION_V2_MANIFEST`]; body schema version
    /// [`BLOB_MANIFEST_BODY_VERSION`].
    Manifest {
        /// Outer wire discriminator (always
        /// [`BLOB_REF_VERSION_V2_MANIFEST`] on fresh constructions).
        version: u8,
        /// Adapter-routed URI.
        uri: String,
        /// Replication / erasure encoding for the chunks.
        encoding: Encoding,
        /// Ordered chunk list. Empty manifests are rejected on
        /// decode (use [`BlobRef::Small`] for zero-byte payloads).
        chunks: Vec<ChunkRef>,
        /// Total payload size = sum of every chunk's `size`. Cached
        /// for cheap `BlobRef::size`; validated on decode against
        /// the iterated sum.
        total_size: u64,
    },
    /// Tree-manifest path (v0.3). Wire version
    /// [`BLOB_REF_VERSION_V3_TREE`]; body schema version
    /// [`BLOB_TREE_BODY_VERSION`]. Lifts the addressable size
    /// from the v0.2 16 GiB cap to 128 PiB at fanout 128 + depth
    /// 4 + 4 MiB chunks. The blob's actual chunk references live
    /// at the [`TreeNode`](super::blob_tree::TreeNode) leaves,
    /// reachable via the tree walk starting from `root_hash`.
    Tree {
        /// Outer wire discriminator (always
        /// [`BLOB_REF_VERSION_V3_TREE`] on fresh constructions).
        version: u8,
        /// Adapter-routed URI. For the mesh-native path this is
        /// `mesh://<hex-of-root_hash>`; external adapters use
        /// their own scheme.
        uri: String,
        /// Replication / erasure encoding (inherits the same
        /// enum surface as `Manifest`).
        encoding: Encoding,
        /// BLAKE3 hash of the root
        /// [`TreeNode`](super::blob_tree::TreeNode) body — the
        /// substrate fetches this hash to start the tree walk.
        root_hash: [u8; 32],
        /// Total payload size in bytes (sum of every leaf
        /// chunk's `size` across the whole tree). Cached for
        /// cheap [`Self::size`].
        total_size: u64,
        /// Tree depth — `1` for root-as-leaf, up to
        /// [`super::blob_tree::MAX_TREE_DEPTH`].
        depth: u8,
    },
}

impl BlobRef {
    // -----------------------------------------------------------
    // Construction
    // -----------------------------------------------------------

    /// Construct a v1 [`BlobRef::Small`]. The caller is responsible
    /// for the `hash` matching the content at `uri` — the substrate
    /// verifies on fetch, not on construction.
    pub fn small(uri: impl Into<String>, hash: [u8; 32], size: u64) -> Self {
        Self::Small {
            version: BLOB_REF_VERSION_V1,
            uri: uri.into(),
            hash,
            size,
        }
    }

    /// Backwards-compatible alias for [`Self::small`]. Pre-v0.2
    /// callers used `BlobRef::new(uri, hash, size)` which produced
    /// the single-blob shape; the new enum surface uses
    /// [`Self::small`] for the same shape.
    #[deprecated(
        since = "0.18.0",
        note = "use `BlobRef::small` for explicit-variant construction"
    )]
    pub fn new(uri: impl Into<String>, hash: [u8; 32], size: u64) -> Self {
        Self::small(uri, hash, size)
    }

    /// Construct a v2 [`BlobRef::Manifest`] from a chunk list. The
    /// caller is responsible for each chunk's hash matching the
    /// stored chunk; the substrate verifies on fetch.
    pub fn manifest(
        uri: impl Into<String>,
        encoding: Encoding,
        chunks: Vec<ChunkRef>,
    ) -> Result<Self, BlobError> {
        if chunks.is_empty() {
            return Err(BlobError::Decode(
                "manifest must carry at least one chunk".to_owned(),
            ));
        }
        if chunks.len() > BLOB_MANIFEST_MAX_CHUNKS {
            return Err(BlobError::Decode(format!(
                "manifest chunk count {} exceeds cap {}",
                chunks.len(),
                BLOB_MANIFEST_MAX_CHUNKS
            )));
        }
        validate_chunk_sizes(&chunks)?;
        let total_size: u64 = chunks.iter().map(|c| c.size as u64).sum();
        if total_size > BLOB_REF_MAX_SIZE {
            return Err(BlobError::Decode(format!(
                "manifest total_size {} exceeds cap {}",
                total_size, BLOB_REF_MAX_SIZE
            )));
        }
        Ok(Self::Manifest {
            version: BLOB_REF_VERSION_V2_MANIFEST,
            uri: uri.into(),
            encoding,
            chunks,
            total_size,
        })
    }

    /// Construct a v3 [`BlobRef::Tree`]. The caller is responsible
    /// for `root_hash` matching the BLAKE3 of the root
    /// [`TreeNode`](super::blob_tree::TreeNode)'s encoded bytes,
    /// and for `total_size` matching the sum of every leaf
    /// chunk's `size` across the tree — the substrate verifies the
    /// hash on tree-walk descent and cross-checks total_size at
    /// the leaves.
    ///
    /// Validates:
    /// - `total_size > 0` (use [`BlobRef::Small`] for zero-byte payloads).
    /// - `total_size <= BLOB_TREE_MAX_TOTAL_SIZE` (~128 PiB ceiling).
    /// - `depth` in `1..=MAX_TREE_DEPTH`.
    pub fn tree(
        uri: impl Into<String>,
        encoding: Encoding,
        root_hash: [u8; 32],
        total_size: u64,
        depth: u8,
    ) -> Result<Self, BlobError> {
        if total_size == 0 {
            return Err(BlobError::Decode(
                "tree total_size must be > 0; use BlobRef::Small for empty payloads".to_owned(),
            ));
        }
        if total_size > BLOB_TREE_MAX_TOTAL_SIZE {
            return Err(BlobError::Decode(format!(
                "tree total_size {} exceeds cap {}",
                total_size, BLOB_TREE_MAX_TOTAL_SIZE
            )));
        }
        if depth == 0 || depth > super::blob_tree::MAX_TREE_DEPTH {
            return Err(BlobError::Decode(format!(
                "tree depth {} out of range 1..={}",
                depth,
                super::blob_tree::MAX_TREE_DEPTH
            )));
        }
        Ok(Self::Tree {
            version: BLOB_REF_VERSION_V3_TREE,
            uri: uri.into(),
            encoding,
            root_hash,
            total_size,
            depth,
        })
    }

    // -----------------------------------------------------------
    // Accessors (uniform across variants)
    // -----------------------------------------------------------

    /// Outer wire version discriminator —
    /// [`BLOB_REF_VERSION_V1`] for Small, [`BLOB_REF_VERSION_V2_MANIFEST`]
    /// for Manifest, [`BLOB_REF_VERSION_V3_TREE`] for Tree.
    pub fn version(&self) -> u8 {
        match self {
            Self::Small { version, .. }
            | Self::Manifest { version, .. }
            | Self::Tree { version, .. } => *version,
        }
    }

    /// Adapter-routed URI. The scheme picks the adapter; the rest is
    /// passed through opaque.
    pub fn uri(&self) -> &str {
        match self {
            Self::Small { uri, .. } | Self::Manifest { uri, .. } | Self::Tree { uri, .. } => {
                uri.as_str()
            }
        }
    }

    /// Total payload size in bytes — `size` for Small,
    /// `total_size` for Manifest, `total_size` for Tree.
    pub fn size(&self) -> u64 {
        match self {
            Self::Small { size, .. } => *size,
            Self::Manifest { total_size, .. } | Self::Tree { total_size, .. } => *total_size,
        }
    }

    /// `true` if this is a chunked-blob manifest (flat
    /// [`Self::Manifest`] or hierarchical [`Self::Tree`]).
    pub fn is_chunked(&self) -> bool {
        matches!(self, Self::Manifest { .. } | Self::Tree { .. })
    }

    /// `true` if this is a hierarchical-manifest tree.
    pub fn is_tree(&self) -> bool {
        matches!(self, Self::Tree { .. })
    }

    /// The single content hash for a Small blob; `None` for a
    /// Manifest or Tree (manifests reference many chunks, each
    /// with its own hash — use [`Self::chunks`] for Manifest or
    /// [`Self::tree_root_hash`] for Tree).
    pub fn small_hash(&self) -> Option<&[u8; 32]> {
        match self {
            Self::Small { hash, .. } => Some(hash),
            Self::Manifest { .. } | Self::Tree { .. } => None,
        }
    }

    /// The root [`TreeNode`](super::blob_tree::TreeNode) hash for
    /// a [`Self::Tree`]; `None` for [`Self::Small`] or
    /// [`Self::Manifest`].
    pub fn tree_root_hash(&self) -> Option<&[u8; 32]> {
        match self {
            Self::Tree { root_hash, .. } => Some(root_hash),
            Self::Small { .. } | Self::Manifest { .. } => None,
        }
    }

    /// The tree depth for a [`Self::Tree`]; `None` for
    /// [`Self::Small`] or [`Self::Manifest`].
    pub fn tree_depth(&self) -> Option<u8> {
        match self {
            Self::Tree { depth, .. } => Some(*depth),
            Self::Small { .. } | Self::Manifest { .. } => None,
        }
    }

    /// The chunk list for a Manifest; empty slice for a Small or
    /// Tree (Tree chunks live at the leaf [`TreeNode`](super::blob_tree::TreeNode)s,
    /// reachable via tree walk — not flattened here).
    pub fn chunks(&self) -> &[ChunkRef] {
        match self {
            Self::Small { .. } | Self::Tree { .. } => &[],
            Self::Manifest { chunks, .. } => chunks,
        }
    }

    /// The encoding tag for a Manifest or Tree; `None` for a
    /// Small (Small has no encoding because the bytes are stored
    /// directly).
    pub fn encoding(&self) -> Option<Encoding> {
        match self {
            Self::Small { .. } => None,
            Self::Manifest { encoding, .. } | Self::Tree { encoding, .. } => Some(*encoding),
        }
    }

    // -----------------------------------------------------------
    // Wire format
    // -----------------------------------------------------------

    /// Encoded length in bytes. The `Small` variant is O(1) —
    /// header size plus URI length. The `Manifest` variant pays
    /// the full postcard serialization cost (the length is taken
    /// from a temporary buffer) because postcard's leb128
    /// length-prefixes make a closed-form size hard to predict.
    /// Callers in a hot path that already need the bytes should
    /// reuse [`Self::encode`] directly instead of pairing
    /// `encoded_len` + `encode`.
    pub fn encoded_len(&self) -> usize {
        match self {
            Self::Small { uri, .. } => BLOB_REF_SMALL_HEADER_LEN + uri.len(),
            Self::Manifest { .. } | Self::Tree { .. } => self.encode().len(),
        }
    }

    /// Emit the wire form. See the module-level table for the
    /// byte layout per variant.
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::Small {
                version,
                uri,
                hash,
                size,
            } => {
                let mut buf = Vec::with_capacity(BLOB_REF_SMALL_HEADER_LEN + uri.len());
                buf.extend_from_slice(&BLOB_REF_MAGIC);
                buf.push(*version);
                buf.extend_from_slice(hash);
                buf.extend_from_slice(&size.to_le_bytes());
                buf.extend_from_slice(uri.as_bytes());
                buf
            }
            Self::Manifest {
                version,
                uri,
                encoding,
                chunks,
                total_size,
            } => {
                let body = ManifestBody {
                    body_version: BLOB_MANIFEST_BODY_VERSION,
                    uri: uri.clone(),
                    encoding: *encoding,
                    chunks: chunks.clone(),
                    total_size: *total_size,
                };
                // Postcard alloc-encode is infallible against
                // `Serialize` types whose subobjects are all sized;
                // every field here is sized. The Result-bearing
                // signature is for fallible writers (e.g. fixed-size
                // buffers); we use the heap allocator.
                let body_bytes = postcard::to_allocvec(&body)
                    .expect("manifest body postcard-encodes infallibly");
                let mut buf = Vec::with_capacity(5 + body_bytes.len());
                buf.extend_from_slice(&BLOB_REF_MAGIC);
                buf.push(*version);
                buf.extend_from_slice(&body_bytes);
                buf
            }
            Self::Tree {
                version,
                uri,
                encoding,
                root_hash,
                total_size,
                depth,
            } => {
                let body = TreeBody {
                    body_version: BLOB_TREE_BODY_VERSION,
                    uri: uri.clone(),
                    encoding: *encoding,
                    root_hash: *root_hash,
                    total_size: *total_size,
                    depth: *depth,
                };
                let body_bytes =
                    postcard::to_allocvec(&body).expect("tree body postcard-encodes infallibly");
                let mut buf = Vec::with_capacity(5 + body_bytes.len());
                buf.extend_from_slice(&BLOB_REF_MAGIC);
                buf.push(*version);
                buf.extend_from_slice(&body_bytes);
                buf
            }
        }
    }

    /// Decode a wire form. Returns `Ok(None)` when the first four
    /// bytes are not [`BLOB_REF_MAGIC`] (caller should treat the
    /// payload as inline). Returns `Err` only when the magic matches
    /// but the rest of the frame is malformed.
    pub fn decode(bytes: &[u8]) -> Result<Option<Self>, BlobError> {
        if bytes.len() < BLOB_REF_MAGIC.len() || bytes[..BLOB_REF_MAGIC.len()] != BLOB_REF_MAGIC {
            return Ok(None);
        }
        if bytes.len() < 5 {
            return Err(BlobError::Decode(format!(
                "frame too short for version byte: {} bytes",
                bytes.len()
            )));
        }
        let version = bytes[4];
        match version {
            BLOB_REF_VERSION_V1 => Self::decode_small(version, &bytes[5..]).map(Some),
            BLOB_REF_VERSION_V2_MANIFEST => Self::decode_manifest(version, &bytes[5..]).map(Some),
            BLOB_REF_VERSION_V3_TREE => Self::decode_tree(version, &bytes[5..]).map(Some),
            other => Err(BlobError::UnsupportedVersion(other)),
        }
    }

    fn decode_small(version: u8, rest: &[u8]) -> Result<Self, BlobError> {
        // rest layout: [hash 32][size 8][uri …]
        if rest.len() < 40 {
            return Err(BlobError::Decode(format!(
                "small frame too short: {} bytes after version, need at least 40",
                rest.len()
            )));
        }
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&rest[0..32]);
        let mut size_bytes = [0u8; 8];
        size_bytes.copy_from_slice(&rest[32..40]);
        let size = u64::from_le_bytes(size_bytes);
        if size > BLOB_REF_MAX_SIZE {
            return Err(BlobError::Decode(format!(
                "blob size {} exceeds cap {}",
                size, BLOB_REF_MAX_SIZE
            )));
        }
        let uri = std::str::from_utf8(&rest[40..])
            .map_err(|e| BlobError::Decode(format!("URI not UTF-8: {}", e)))?
            .to_owned();
        Ok(Self::Small {
            version,
            uri,
            hash,
            size,
        })
    }

    fn decode_manifest(version: u8, rest: &[u8]) -> Result<Self, BlobError> {
        // Bound the wire size BEFORE postcard allocates the
        // `Vec<ChunkRef>`. Otherwise a malicious peer can stamp
        // the chunks-length varint up to ~u32::MAX, forcing a
        // multi-MB Vec allocation before our post-decode cap
        // check at line ~25 below fires. The legitimate upper
        // bound for a well-formed manifest body is:
        //
        //   uri (≤ 8 KiB after the substrate's outer length cap)
        //   + 1 byte encoding discriminant
        //   + 1 byte body_version
        //   + ≤ 10 bytes total_size varint
        //   + ≤ 5 bytes chunks-len varint (covers u32::MAX, far above our cap)
        //   + BLOB_MANIFEST_MAX_CHUNKS chunks × ≤ 50 bytes max
        //     each (32 hash + 5 size varint + 10 offset varint +
        //     framing slack)
        //
        // Round up generously to a static upper bound. Anything
        // past this is by construction malformed; reject without
        // touching the allocator.
        const MAX_MANIFEST_WIRE_BYTES: usize = 8192 + 32 + BLOB_MANIFEST_MAX_CHUNKS * 50;
        if rest.len() > MAX_MANIFEST_WIRE_BYTES {
            return Err(BlobError::Decode(format!(
                "manifest body {} bytes exceeds legitimate upper bound {}",
                rest.len(),
                MAX_MANIFEST_WIRE_BYTES
            )));
        }
        let body: ManifestBody = postcard::from_bytes(rest)
            .map_err(|e| BlobError::Decode(format!("manifest body decode failed: {}", e)))?;
        if body.body_version != BLOB_MANIFEST_BODY_VERSION {
            return Err(BlobError::UnsupportedVersion(body.body_version));
        }
        if body.chunks.is_empty() {
            return Err(BlobError::Decode(
                "manifest must carry at least one chunk".to_owned(),
            ));
        }
        if body.chunks.len() > BLOB_MANIFEST_MAX_CHUNKS {
            return Err(BlobError::Decode(format!(
                "manifest chunk count {} exceeds cap {}",
                body.chunks.len(),
                BLOB_MANIFEST_MAX_CHUNKS
            )));
        }
        validate_chunk_sizes(&body.chunks)?;
        // Validate the cached total_size matches the iterated sum —
        // a malicious peer could otherwise lie about total_size to
        // mislead range math without flipping any chunk's hash.
        let iterated_sum: u64 = body.chunks.iter().map(|c| c.size as u64).sum();
        if iterated_sum != body.total_size {
            return Err(BlobError::Decode(format!(
                "manifest total_size mismatch: declared {}, iterated {}",
                body.total_size, iterated_sum
            )));
        }
        if body.total_size > BLOB_REF_MAX_SIZE {
            return Err(BlobError::Decode(format!(
                "manifest total_size {} exceeds cap {}",
                body.total_size, BLOB_REF_MAX_SIZE
            )));
        }
        Ok(Self::Manifest {
            version,
            uri: body.uri,
            encoding: body.encoding,
            chunks: body.chunks,
            total_size: body.total_size,
        })
    }

    fn decode_tree(version: u8, rest: &[u8]) -> Result<Self, BlobError> {
        // Bound the wire size BEFORE postcard allocates. The Tree
        // body carries only fixed-size fields (root_hash, sizes,
        // depth) plus a URI string — 1 KiB is generous for the
        // legitimate shape and bounds malicious oversize payloads
        // before the URI's String allocation runs.
        if rest.len() > BLOB_REF_TREE_BODY_MAX_BYTES {
            return Err(BlobError::Decode(format!(
                "tree body {} bytes exceeds cap {}",
                rest.len(),
                BLOB_REF_TREE_BODY_MAX_BYTES
            )));
        }
        let body: TreeBody = postcard::from_bytes(rest)
            .map_err(|e| BlobError::Decode(format!("tree body decode failed: {}", e)))?;
        if body.body_version != BLOB_TREE_BODY_VERSION {
            return Err(BlobError::UnsupportedVersion(body.body_version));
        }
        if body.total_size == 0 {
            return Err(BlobError::Decode(
                "tree total_size must be > 0; empty payloads use BlobRef::Small".to_owned(),
            ));
        }
        if body.total_size > BLOB_TREE_MAX_TOTAL_SIZE {
            return Err(BlobError::Decode(format!(
                "tree total_size {} exceeds cap {}",
                body.total_size, BLOB_TREE_MAX_TOTAL_SIZE
            )));
        }
        if body.depth == 0 || body.depth > super::blob_tree::MAX_TREE_DEPTH {
            return Err(BlobError::Decode(format!(
                "tree depth {} out of range 1..={}",
                body.depth,
                super::blob_tree::MAX_TREE_DEPTH
            )));
        }
        // Defensive depth-vs-size lower bound. A well-formed depth=N
        // tree (N >= 2) requires AT LEAST TREE_FANOUT^(N-1) bytes
        // to be productive — depth=2 needs > FANOUT (128) bytes
        // for an Internal root to be useful, depth=3 needs >
        // FANOUT^2 = 16 384, depth=4 needs > FANOUT^3 ≈ 2 M. A
        // manifest claiming depth=4 + total_size=1 is structurally
        // malformed (a single chunk can't justify three internal
        // levels) — reject before any walk traffic happens. The
        // walker's depth-shortening check would catch this too,
        // but at the cost of a round trip to fetch the root.
        if body.depth >= 2 {
            let exp = body.depth as u32 - 1;
            // Compute FANOUT^exp using checked_pow; on overflow
            // the depth is at the cap and the lower bound is
            // satisfied by any reasonable total_size, so skip the
            // check in that direction.
            if let Some(min_size) = (super::blob_tree::TREE_FANOUT as u64).checked_pow(exp) {
                if body.total_size < min_size {
                    return Err(BlobError::Decode(format!(
                        "tree depth {} requires total_size >= {} (TREE_FANOUT^(depth-1)); got {}",
                        body.depth, min_size, body.total_size
                    )));
                }
            }
        }
        Ok(Self::Tree {
            version,
            uri: body.uri,
            encoding: body.encoding,
            root_hash: body.root_hash,
            total_size: body.total_size,
            depth: body.depth,
        })
    }

    /// Verify `bytes` resolves to this `BlobRef`'s hash. Only
    /// defined for [`BlobRef::Small`] — call sites holding a
    /// Manifest verify chunk-by-chunk via [`Self::chunks`]; call
    /// sites holding a Tree verify via tree-walk descent (each
    /// [`TreeNode`](super::blob_tree::TreeNode)'s bytes hash to
    /// the parent's stored child-hash entry).
    /// Returns `Ok(())` on match,
    /// `Err(BlobError::HashMismatch)` otherwise, `Err(BlobError::Decode)`
    /// on a Manifest / Tree. Runs inside the substrate, not the
    /// adapter, so an adversarial adapter cannot fake-verify.
    pub fn verify(&self, bytes: &[u8]) -> Result<(), BlobError> {
        match self {
            Self::Small { hash, .. } => {
                let actual: [u8; 32] = blake3::hash(bytes).into();
                if actual == *hash {
                    Ok(())
                } else {
                    Err(BlobError::HashMismatch {
                        expected: *hash,
                        actual,
                    })
                }
            }
            Self::Manifest { .. } => Err(BlobError::Decode(
                "verify is undefined on a Manifest variant; verify chunks individually".to_owned(),
            )),
            Self::Tree { .. } => Err(BlobError::Decode(
                "verify is undefined on a Tree variant; verify chunks individually via tree walk"
                    .to_owned(),
            )),
        }
    }
}

// -------------------------------------------------------------------
// Chunking + range math (pure logic — no I/O)
// -------------------------------------------------------------------

/// Reject manifests where any chunk size disagrees with the substrate's
/// fixed [`BLOB_CHUNK_SIZE_BYTES`] stride. Every non-last chunk MUST
/// be exactly `BLOB_CHUNK_SIZE_BYTES`; the last chunk MAY be smaller
/// but must be at least one byte. `byte_range_to_chunks` and the
/// adapter's range slicer rely on the fixed stride; an attacker-stamped
/// `{size: u32::MAX}` chunk would otherwise either return wrong-window
/// bytes silently or trip a panicking slice in the consumer.
fn validate_chunk_sizes(chunks: &[ChunkRef]) -> Result<(), BlobError> {
    let last = chunks.len() - 1;
    for (i, chunk) in chunks.iter().enumerate() {
        let size = chunk.size as u64;
        if i < last {
            if size != BLOB_CHUNK_SIZE_BYTES {
                return Err(BlobError::Decode(format!(
                    "manifest non-last chunk {} has size {} (expected {})",
                    i, size, BLOB_CHUNK_SIZE_BYTES
                )));
            }
        } else {
            if size == 0 || size > BLOB_CHUNK_SIZE_BYTES {
                return Err(BlobError::Decode(format!(
                    "manifest last chunk {} has size {} (expected 1..={})",
                    i, size, BLOB_CHUNK_SIZE_BYTES
                )));
            }
        }
    }
    Ok(())
}

/// Outcome of [`chunk_payload`] — either the payload fit below the
/// threshold (single Small blob shape) or it split into N chunks
/// plus a manifest.
#[derive(Clone, Debug)]
pub enum ChunkedPayload<'a> {
    /// Payload size ≤ [`BLOB_CHUNK_SIZE_BYTES`]; ride as a single
    /// content-addressed blob. The caller stores `payload` against
    /// the resulting hash; the [`BlobRef`] returned by
    /// [`Self::into_blob_ref`] points at that single content.
    Inline {
        /// BLAKE3 of the whole payload.
        hash: [u8; 32],
        /// Payload bytes (zero-copy slice into the caller's buffer).
        payload: &'a [u8],
    },
    /// Payload size > [`BLOB_CHUNK_SIZE_BYTES`]; split into N
    /// 4-MiB chunks (last chunk may be smaller). The caller stores
    /// each chunk independently against its hash; the
    /// [`BlobRef::Manifest`] returned by [`Self::into_blob_ref`]
    /// references all of them.
    Chunked {
        /// Each chunk's `(hash, byte-slice)`. Slices are zero-copy
        /// views into the caller's buffer.
        chunks: Vec<(ChunkRef, &'a [u8])>,
        /// Total payload length = sum of chunk lengths.
        total_size: u64,
    },
}

impl<'a> ChunkedPayload<'a> {
    /// Total payload size — `payload.len()` for Inline, sum of chunk
    /// sizes for Chunked.
    pub fn size(&self) -> u64 {
        match self {
            Self::Inline { payload, .. } => payload.len() as u64,
            Self::Chunked { total_size, .. } => *total_size,
        }
    }

    /// Convert into the corresponding [`BlobRef`] given the
    /// adapter-routed URI. Inline produces [`BlobRef::Small`];
    /// Chunked produces [`BlobRef::Manifest`] with the supplied
    /// encoding. Returns `Err` only when the chunked variant exceeds
    /// [`BLOB_MANIFEST_MAX_CHUNKS`] (defense-in-depth — the chunker
    /// already enforces the cap).
    pub fn into_blob_ref(
        self,
        uri: impl Into<String>,
        encoding: Encoding,
    ) -> Result<BlobRef, BlobError> {
        match self {
            Self::Inline { hash, payload } => Ok(BlobRef::small(uri, hash, payload.len() as u64)),
            Self::Chunked { chunks, .. } => {
                let chunk_refs: Vec<ChunkRef> = chunks.into_iter().map(|(r, _)| r).collect();
                BlobRef::manifest(uri, encoding, chunk_refs)
            }
        }
    }
}

/// Split a byte payload into either a single Inline blob or N
/// fixed-size chunks, content-addressing each part. Locked decisions:
///
/// - Threshold is a hard `≤` comparison: payload at exactly
///   [`BLOB_CHUNK_SIZE_BYTES`] rides as Inline (the chunker
///   wouldn't have anything to split into), payloads strictly larger
///   split into N = `ceil(len / BLOB_CHUNK_SIZE_BYTES)` chunks.
/// - Chunk size is fixed at [`BLOB_CHUNK_SIZE_BYTES`]; the algorithm
///   is deterministic — two callers chunking the same `bytes`
///   produce identical hash lists.
/// - Empty payload produces an Inline result with `payload = &[]`
///   and the BLAKE3-of-empty hash.
///
/// Rejects payloads larger than [`BLOB_REF_MAX_SIZE`] or whose chunk
/// count would exceed [`BLOB_MANIFEST_MAX_CHUNKS`].
pub fn chunk_payload(bytes: &[u8]) -> Result<ChunkedPayload<'_>, BlobError> {
    let len = bytes.len() as u64;
    if len > BLOB_REF_MAX_SIZE {
        return Err(BlobError::Decode(format!(
            "payload size {} exceeds cap {}",
            len, BLOB_REF_MAX_SIZE
        )));
    }
    if len <= BLOB_CHUNK_SIZE_BYTES {
        let hash: [u8; 32] = blake3::hash(bytes).into();
        return Ok(ChunkedPayload::Inline {
            hash,
            payload: bytes,
        });
    }
    let chunk_size = BLOB_CHUNK_SIZE_BYTES as usize;
    let chunk_count = bytes.len().div_ceil(chunk_size);
    if chunk_count > BLOB_MANIFEST_MAX_CHUNKS {
        return Err(BlobError::Decode(format!(
            "payload requires {} chunks, exceeds cap {}",
            chunk_count, BLOB_MANIFEST_MAX_CHUNKS
        )));
    }
    let mut chunks = Vec::with_capacity(chunk_count);
    for slice in bytes.chunks(chunk_size) {
        let hash: [u8; 32] = blake3::hash(slice).into();
        chunks.push((
            ChunkRef {
                hash,
                size: slice.len() as u32,
            },
            slice,
        ));
    }
    Ok(ChunkedPayload::Chunked {
        chunks,
        total_size: len,
    })
}

/// One chunk-range request emitted by [`byte_range_to_chunks`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkRangeRequest {
    /// Index into the manifest's chunk list.
    pub chunk_index: usize,
    /// Byte offset *within the chunk* where the requested range
    /// starts (always 0 for non-boundary chunks; non-zero only for
    /// the first chunk of a partial fetch).
    pub start_in_chunk: u32,
    /// Byte offset *within the chunk* where the requested range
    /// ends (exclusive). Equals the chunk's `size` for non-boundary
    /// chunks; smaller only for the last chunk of a partial fetch.
    pub end_in_chunk: u32,
}

impl ChunkRangeRequest {
    /// Length of the requested slice within this chunk.
    pub fn len(&self) -> u32 {
        self.end_in_chunk - self.start_in_chunk
    }

    /// `true` if the requested slice is empty.
    pub fn is_empty(&self) -> bool {
        self.start_in_chunk >= self.end_in_chunk
    }
}

/// Translate a global byte range `[start, end)` over a chunked blob
/// into the per-chunk fetch requests needed to satisfy it. Returns
/// the requests in chunk-index order so the caller can concatenate
/// the returned slices in iteration order. The math:
///
/// - `chunk_index` walks `[start / CHUNK, ceil(end / CHUNK))`.
/// - The first chunk's `start_in_chunk` is `start % CHUNK`; every
///   later chunk's `start_in_chunk` is `0`.
/// - The last chunk's `end_in_chunk` is `((end - 1) % CHUNK) + 1`
///   capped at the chunk's actual `size`; every earlier chunk's
///   `end_in_chunk` is the chunk's full `size`.
///
/// Returns an empty `Vec` for empty ranges (`start == end`) or when
/// `start >= total_size`. Errors when `end > total_size` or
/// `start > end` (callers should range-check before invoking, but
/// we surface a typed error to ease use as a defensive backstop).
///
/// Pure-logic; no chunk fetches happen here.
pub fn byte_range_to_chunks(
    manifest: &BlobRef,
    start: u64,
    end: u64,
) -> Result<Vec<ChunkRangeRequest>, BlobError> {
    let (chunks, total_size) = match manifest {
        BlobRef::Manifest {
            chunks, total_size, ..
        } => (chunks.as_slice(), *total_size),
        BlobRef::Small { .. } => {
            return Err(BlobError::Decode(
                "byte_range_to_chunks called on a Small BlobRef".to_owned(),
            ));
        }
        BlobRef::Tree { .. } => {
            // Tree blobs resolve ranges via tree walk
            // (A4 `TreeWalker`), not via the flat-manifest
            // helper. Callers holding a Tree BlobRef route
            // through `MeshBlobAdapter::fetch_range`'s tree
            // path directly.
            return Err(BlobError::Decode(
                "byte_range_to_chunks called on a Tree BlobRef — \
                 use the tree-walker path instead"
                    .to_owned(),
            ));
        }
    };
    if start > end {
        return Err(BlobError::Decode(format!(
            "range start {} > end {}",
            start, end
        )));
    }
    if end > total_size {
        return Err(BlobError::Decode(format!(
            "range end {} exceeds total_size {}",
            end, total_size
        )));
    }
    if start == end || start >= total_size {
        return Ok(Vec::new());
    }
    let chunk_size = BLOB_CHUNK_SIZE_BYTES;
    let first_chunk = (start / chunk_size) as usize;
    let last_chunk_inclusive = ((end - 1) / chunk_size) as usize;
    let mut out = Vec::with_capacity(last_chunk_inclusive - first_chunk + 1);
    for (chunk_index, chunk) in chunks
        .iter()
        .enumerate()
        .skip(first_chunk)
        .take(last_chunk_inclusive - first_chunk + 1)
    {
        let chunk_start_in_blob = chunk_index as u64 * chunk_size;
        // Clamp [start, end) against this chunk's
        // [chunk_start_in_blob, chunk_start_in_blob + chunk.size).
        let local_start = start.saturating_sub(chunk_start_in_blob);
        let local_end = (end - chunk_start_in_blob).min(chunk.size as u64);
        out.push(ChunkRangeRequest {
            chunk_index,
            start_in_chunk: local_start as u32,
            end_in_chunk: local_end as u32,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------
    // Small variant — round-trip + decode-edge tests
    // (preserved from v0.15 for back-compat coverage)
    // -----------------------------------------------------------

    fn small_fixture() -> BlobRef {
        BlobRef::small("s3://bucket/key", [0xAB; 32], 12345)
    }

    #[test]
    fn small_round_trip_encode_decode() {
        let original = small_fixture();
        let bytes = original.encode();
        let decoded = BlobRef::decode(&bytes).unwrap().unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn decode_returns_none_when_magic_missing() {
        let bytes = vec![0x00, 0x01, 0x02, 0x03, 0x04];
        assert!(BlobRef::decode(&bytes).unwrap().is_none());
    }

    #[test]
    fn decode_returns_none_for_payloads_starting_with_old_discriminator_only() {
        let bytes = vec![0xB0, 0x00, 0x00, 0x00];
        assert!(BlobRef::decode(&bytes).unwrap().is_none());
        let bytes = vec![0xB0, 0xB1, 0x00, 0x00];
        assert!(BlobRef::decode(&bytes).unwrap().is_none());
        let bytes = vec![0xB0, 0xB1, 0xB2, 0x00];
        assert!(BlobRef::decode(&bytes).unwrap().is_none());
    }

    #[test]
    fn decode_rejects_short_small_frame() {
        let mut bytes = BLOB_REF_MAGIC.to_vec();
        bytes.push(BLOB_REF_VERSION_V1);
        bytes.push(0x00); // truncated mid-hash
        let err = BlobRef::decode(&bytes).unwrap_err();
        assert!(matches!(err, BlobError::Decode(_)));
    }

    #[test]
    fn decode_rejects_unknown_outer_version() {
        let blob = small_fixture();
        let mut bytes = blob.encode();
        bytes[4] = 0xFE;
        let err = BlobRef::decode(&bytes).unwrap_err();
        assert!(matches!(err, BlobError::UnsupportedVersion(0xFE)));
    }

    #[test]
    fn encoded_len_matches_real_encoding_small() {
        let blob = small_fixture();
        assert_eq!(blob.encoded_len(), blob.encode().len());
    }

    #[test]
    fn small_verify_accepts_matching_bytes() {
        let payload = b"the lazy dog";
        let hash: [u8; 32] = blake3::hash(payload).into();
        let blob = BlobRef::small("file:///x", hash, payload.len() as u64);
        blob.verify(payload).unwrap();
    }

    #[test]
    fn small_verify_rejects_mismatching_bytes() {
        let blob = BlobRef::small("file:///x", [0xCC; 32], 0);
        let err = blob.verify(b"different content").unwrap_err();
        match err {
            BlobError::HashMismatch { expected, actual } => {
                assert_eq!(expected, [0xCC; 32]);
                assert_ne!(actual, expected);
            }
            other => panic!("expected HashMismatch, got {:?}", other),
        }
    }

    #[test]
    fn small_decode_rejects_oversize_size_field() {
        let mut bytes = BLOB_REF_MAGIC.to_vec();
        bytes.push(BLOB_REF_VERSION_V1);
        bytes.extend_from_slice(&[0u8; 32]);
        bytes.extend_from_slice(&u64::MAX.to_le_bytes());
        let err = BlobRef::decode(&bytes).unwrap_err();
        assert!(matches!(err, BlobError::Decode(_)));
    }

    #[test]
    fn empty_uri_round_trips_small() {
        let blob = BlobRef::small("", [0x00; 32], 0);
        let bytes = blob.encode();
        let decoded = BlobRef::decode(&bytes).unwrap().unwrap();
        assert_eq!(decoded.uri(), "");
        assert_eq!(decoded.size(), 0);
    }

    // -----------------------------------------------------------
    // Manifest variant — round-trip + decode-edge tests
    // -----------------------------------------------------------

    fn manifest_fixture(chunk_count: usize) -> BlobRef {
        let chunks: Vec<ChunkRef> = (0..chunk_count)
            .map(|i| ChunkRef {
                hash: [i as u8; 32],
                size: BLOB_CHUNK_SIZE_BYTES as u32,
            })
            .collect();
        BlobRef::manifest("mesh://abc", Encoding::Replicated, chunks).unwrap()
    }

    #[test]
    fn manifest_round_trip_encode_decode() {
        let original = manifest_fixture(8);
        let bytes = original.encode();
        let decoded = BlobRef::decode(&bytes).unwrap().unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn manifest_round_trip_with_reed_solomon_reserved() {
        let chunks = vec![ChunkRef {
            hash: [0xAA; 32],
            size: 1024,
        }];
        let blob =
            BlobRef::manifest("mesh://rs", Encoding::ReedSolomon { k: 4, m: 2 }, chunks).unwrap();
        let bytes = blob.encode();
        let decoded = BlobRef::decode(&bytes).unwrap().unwrap();
        assert_eq!(
            decoded.encoding(),
            Some(Encoding::ReedSolomon { k: 4, m: 2 })
        );
    }

    #[test]
    fn manifest_rejects_empty_chunk_list() {
        let err = BlobRef::manifest("mesh://", Encoding::Replicated, Vec::new()).unwrap_err();
        assert!(matches!(err, BlobError::Decode(_)));
    }

    #[test]
    fn manifest_rejects_too_many_chunks() {
        let chunks: Vec<ChunkRef> = (0..BLOB_MANIFEST_MAX_CHUNKS + 1)
            .map(|_| ChunkRef {
                hash: [0; 32],
                size: 1,
            })
            .collect();
        let err = BlobRef::manifest("mesh://", Encoding::Replicated, chunks).unwrap_err();
        assert!(matches!(err, BlobError::Decode(_)));
    }

    #[test]
    fn manifest_rejects_total_size_over_cap() {
        let chunks = vec![
            ChunkRef {
                hash: [0; 32],
                size: u32::MAX,
            };
            5
        ];
        // 5 × 4 GiB ≈ 20 GiB > 16 GiB cap (also fails chunk-size validator)
        let err = BlobRef::manifest("mesh://", Encoding::Replicated, chunks).unwrap_err();
        assert!(matches!(err, BlobError::Decode(_)));
    }

    /// `byte_range_to_chunks` and the adapter's range slicer rely on
    /// the substrate's fixed `BLOB_CHUNK_SIZE_BYTES` stride. A
    /// peer-crafted manifest with non-stride chunk sizes makes the
    /// position math return wrong-window bytes silently, so both
    /// `manifest()` and `decode_manifest()` must reject those shapes.
    #[test]
    fn manifest_rejects_non_last_chunk_smaller_than_stride() {
        let chunks = vec![
            ChunkRef {
                hash: [1; 32],
                size: 1, // first chunk must be exactly BLOB_CHUNK_SIZE_BYTES
            },
            ChunkRef {
                hash: [2; 32],
                size: BLOB_CHUNK_SIZE_BYTES as u32,
            },
        ];
        let err = BlobRef::manifest("mesh://", Encoding::Replicated, chunks).unwrap_err();
        assert!(matches!(err, BlobError::Decode(_)));
    }

    #[test]
    fn manifest_rejects_non_last_chunk_larger_than_stride() {
        let chunks = vec![
            ChunkRef {
                hash: [1; 32],
                size: (BLOB_CHUNK_SIZE_BYTES as u32) + 1,
            },
            ChunkRef {
                hash: [2; 32],
                size: BLOB_CHUNK_SIZE_BYTES as u32,
            },
        ];
        let err = BlobRef::manifest("mesh://", Encoding::Replicated, chunks).unwrap_err();
        assert!(matches!(err, BlobError::Decode(_)));
    }

    #[test]
    fn manifest_rejects_last_chunk_above_stride() {
        let chunks = vec![ChunkRef {
            hash: [1; 32],
            size: (BLOB_CHUNK_SIZE_BYTES as u32) + 1,
        }];
        let err = BlobRef::manifest("mesh://", Encoding::Replicated, chunks).unwrap_err();
        assert!(matches!(err, BlobError::Decode(_)));
    }

    #[test]
    fn manifest_rejects_zero_size_chunk() {
        let chunks = vec![ChunkRef {
            hash: [1; 32],
            size: 0,
        }];
        let err = BlobRef::manifest("mesh://", Encoding::Replicated, chunks).unwrap_err();
        assert!(matches!(err, BlobError::Decode(_)));
    }

    #[test]
    fn manifest_accepts_single_short_chunk_as_last() {
        // A single chunk smaller than the stride is the valid
        // single-chunk last-chunk case (a payload less than 4 MiB
        // would normally ride as Small, but Manifest with one short
        // chunk is structurally legal).
        let chunks = vec![ChunkRef {
            hash: [1; 32],
            size: 1024,
        }];
        let blob = BlobRef::manifest("mesh://", Encoding::Replicated, chunks).unwrap();
        assert_eq!(blob.size(), 1024);
    }

    #[test]
    fn manifest_accepts_multichunk_with_short_last() {
        let chunks = vec![
            ChunkRef {
                hash: [1; 32],
                size: BLOB_CHUNK_SIZE_BYTES as u32,
            },
            ChunkRef {
                hash: [2; 32],
                size: 1024,
            },
        ];
        let blob = BlobRef::manifest("mesh://", Encoding::Replicated, chunks).unwrap();
        assert_eq!(blob.size(), BLOB_CHUNK_SIZE_BYTES + 1024);
    }

    #[test]
    fn manifest_decode_detects_total_size_lie() {
        // Hand-craft a manifest body whose declared total_size is
        // wrong vs. the iterated sum — a malicious peer could
        // otherwise mislead range math by under-reporting the
        // total. Decode must reject.
        use serde::Serialize;
        #[derive(Serialize)]
        struct LyingBody {
            body_version: u8,
            uri: String,
            encoding: Encoding,
            chunks: Vec<ChunkRef>,
            total_size: u64,
        }
        let lying = LyingBody {
            body_version: BLOB_MANIFEST_BODY_VERSION,
            uri: "mesh://lie".to_owned(),
            encoding: Encoding::Replicated,
            chunks: vec![ChunkRef {
                hash: [0; 32],
                size: 100,
            }],
            total_size: 200, // declared 200 but iterated sum is 100
        };
        let body = postcard::to_allocvec(&lying).unwrap();
        let mut bytes = BLOB_REF_MAGIC.to_vec();
        bytes.push(BLOB_REF_VERSION_V2_MANIFEST);
        bytes.extend_from_slice(&body);
        let err = BlobRef::decode(&bytes).unwrap_err();
        assert!(matches!(err, BlobError::Decode(_)));
    }

    #[test]
    fn manifest_decode_rejects_unknown_body_version() {
        use serde::Serialize;
        #[derive(Serialize)]
        struct FutureBody {
            body_version: u8,
            uri: String,
            encoding: Encoding,
            chunks: Vec<ChunkRef>,
            total_size: u64,
        }
        let body = FutureBody {
            body_version: 0xFE,
            uri: "mesh://".to_owned(),
            encoding: Encoding::Replicated,
            chunks: vec![ChunkRef {
                hash: [0; 32],
                size: 1,
            }],
            total_size: 1,
        };
        let body_bytes = postcard::to_allocvec(&body).unwrap();
        let mut bytes = BLOB_REF_MAGIC.to_vec();
        bytes.push(BLOB_REF_VERSION_V2_MANIFEST);
        bytes.extend_from_slice(&body_bytes);
        let err = BlobRef::decode(&bytes).unwrap_err();
        assert!(matches!(err, BlobError::UnsupportedVersion(0xFE)));
    }

    #[test]
    fn manifest_size_matches_iterated_chunk_sum() {
        let blob = manifest_fixture(10);
        let iterated: u64 = blob.chunks().iter().map(|c| c.size as u64).sum();
        assert_eq!(blob.size(), iterated);
    }

    #[test]
    fn accessors_uniform_across_variants() {
        let small = BlobRef::small("file:///s", [0; 32], 99);
        assert_eq!(small.uri(), "file:///s");
        assert_eq!(small.size(), 99);
        assert!(!small.is_chunked());
        assert!(small.small_hash().is_some());
        assert!(small.chunks().is_empty());
        assert_eq!(small.encoding(), None);

        let m = manifest_fixture(3);
        assert_eq!(m.uri(), "mesh://abc");
        assert!(m.is_chunked());
        assert!(m.small_hash().is_none());
        assert_eq!(m.chunks().len(), 3);
        assert_eq!(m.encoding(), Some(Encoding::Replicated));
    }

    // -----------------------------------------------------------
    // Chunking algorithm — idempotency + edge cases
    // -----------------------------------------------------------

    #[test]
    fn chunk_payload_inline_under_threshold() {
        let payload = vec![0x42u8; 1024]; // 1 KiB
        match chunk_payload(&payload).unwrap() {
            ChunkedPayload::Inline { payload: p, hash } => {
                assert_eq!(p.len(), 1024);
                let expected_hash: [u8; 32] = blake3::hash(&payload).into();
                assert_eq!(hash, expected_hash);
            }
            ChunkedPayload::Chunked { .. } => panic!("expected Inline for 1 KiB payload"),
        }
    }

    #[test]
    fn chunk_payload_inline_at_exact_threshold() {
        let payload = vec![0x42u8; BLOB_CHUNK_SIZE_BYTES as usize]; // exactly 4 MiB
        assert!(matches!(
            chunk_payload(&payload).unwrap(),
            ChunkedPayload::Inline { .. }
        ));
    }

    #[test]
    fn chunk_payload_chunks_above_threshold() {
        let payload = vec![0x42u8; (BLOB_CHUNK_SIZE_BYTES as usize) + 1]; // 4 MiB + 1
        match chunk_payload(&payload).unwrap() {
            ChunkedPayload::Chunked { chunks, total_size } => {
                assert_eq!(chunks.len(), 2);
                assert_eq!(chunks[0].0.size, BLOB_CHUNK_SIZE_BYTES as u32);
                assert_eq!(chunks[1].0.size, 1);
                assert_eq!(total_size, payload.len() as u64);
            }
            ChunkedPayload::Inline { .. } => panic!("expected Chunked for 4MiB+1 payload"),
        }
    }

    #[test]
    fn chunk_payload_idempotent_same_bytes_same_hashes() {
        // Two callers chunking the same payload must produce
        // identical ChunkRef lists — the dedup property the
        // replication layer relies on.
        let payload: Vec<u8> = (0..(8 * 1024 * 1024 + 17))
            .map(|i| (i % 251) as u8)
            .collect();
        let first = match chunk_payload(&payload).unwrap() {
            ChunkedPayload::Chunked { chunks, .. } => {
                chunks.iter().map(|(c, _)| *c).collect::<Vec<_>>()
            }
            _ => panic!("expected Chunked"),
        };
        let second = match chunk_payload(&payload).unwrap() {
            ChunkedPayload::Chunked { chunks, .. } => {
                chunks.iter().map(|(c, _)| *c).collect::<Vec<_>>()
            }
            _ => panic!("expected Chunked"),
        };
        assert_eq!(first, second);
    }

    #[test]
    fn chunk_payload_empty_is_inline() {
        let payload: Vec<u8> = Vec::new();
        match chunk_payload(&payload).unwrap() {
            ChunkedPayload::Inline { payload, hash } => {
                assert!(payload.is_empty());
                let expected: [u8; 32] = blake3::hash(b"").into();
                assert_eq!(hash, expected);
            }
            _ => panic!("empty payload must be Inline"),
        }
    }

    #[test]
    fn chunk_payload_rejects_oversize() {
        // Construct a fake "len" by lying via slice — but we can't
        // actually allocate 16 GiB. Instead, test the cap-check
        // arithmetic via a payload sized 4 GiB + 1 against a smaller
        // synthetic cap. The production cap is BLOB_REF_MAX_SIZE so
        // we test the chunk-count cap path here.
        // (chunk-count cap fires at MAX_CHUNKS * 4 MiB = 32 GiB,
        // before BLOB_REF_MAX_SIZE — verified below.)
        assert!(BLOB_MANIFEST_MAX_CHUNKS as u64 * BLOB_CHUNK_SIZE_BYTES > BLOB_REF_MAX_SIZE);
    }

    // -----------------------------------------------------------
    // byte_range_to_chunks — range math
    // -----------------------------------------------------------

    fn five_chunk_manifest() -> BlobRef {
        // Five 4 MiB chunks (20 MiB total).
        let chunks: Vec<ChunkRef> = (0..5)
            .map(|i| ChunkRef {
                hash: [i as u8; 32],
                size: BLOB_CHUNK_SIZE_BYTES as u32,
            })
            .collect();
        BlobRef::manifest("mesh://x", Encoding::Replicated, chunks).unwrap()
    }

    #[test]
    fn range_aligned_single_chunk() {
        let m = five_chunk_manifest();
        let req = byte_range_to_chunks(&m, 0, BLOB_CHUNK_SIZE_BYTES).unwrap();
        assert_eq!(req.len(), 1);
        assert_eq!(req[0].chunk_index, 0);
        assert_eq!(req[0].start_in_chunk, 0);
        assert_eq!(req[0].end_in_chunk, BLOB_CHUNK_SIZE_BYTES as u32);
    }

    #[test]
    fn range_unaligned_within_one_chunk() {
        let m = five_chunk_manifest();
        let req = byte_range_to_chunks(&m, 100, 200).unwrap();
        assert_eq!(req.len(), 1);
        assert_eq!(req[0].chunk_index, 0);
        assert_eq!(req[0].start_in_chunk, 100);
        assert_eq!(req[0].end_in_chunk, 200);
        assert_eq!(req[0].len(), 100);
    }

    #[test]
    fn range_spans_two_chunks() {
        let m = five_chunk_manifest();
        let chunk = BLOB_CHUNK_SIZE_BYTES;
        // Last 1 KiB of chunk 0, first 1 KiB of chunk 1.
        let req = byte_range_to_chunks(&m, chunk - 1024, chunk + 1024).unwrap();
        assert_eq!(req.len(), 2);
        assert_eq!(req[0].chunk_index, 0);
        assert_eq!(req[0].start_in_chunk, (chunk - 1024) as u32);
        assert_eq!(req[0].end_in_chunk, chunk as u32);
        assert_eq!(req[1].chunk_index, 1);
        assert_eq!(req[1].start_in_chunk, 0);
        assert_eq!(req[1].end_in_chunk, 1024);
    }

    #[test]
    fn range_spans_all_chunks() {
        let m = five_chunk_manifest();
        let req = byte_range_to_chunks(&m, 0, m.size()).unwrap();
        assert_eq!(req.len(), 5);
        for (i, r) in req.iter().enumerate() {
            assert_eq!(r.chunk_index, i);
            assert_eq!(r.start_in_chunk, 0);
            assert_eq!(r.end_in_chunk, BLOB_CHUNK_SIZE_BYTES as u32);
        }
    }

    #[test]
    fn range_with_partial_last_chunk() {
        // Manifest where the last chunk is smaller than the chunk
        // size — exercises the per-chunk clamp on `end_in_chunk`.
        let chunks = vec![
            ChunkRef {
                hash: [0; 32],
                size: BLOB_CHUNK_SIZE_BYTES as u32,
            },
            ChunkRef {
                hash: [1; 32],
                size: 1024, // last chunk is 1 KiB
            },
        ];
        let m = BlobRef::manifest("mesh://", Encoding::Replicated, chunks).unwrap();
        // Range covers all of chunk 0 + first 100 bytes of chunk 1.
        let req = byte_range_to_chunks(&m, 0, BLOB_CHUNK_SIZE_BYTES + 100).unwrap();
        assert_eq!(req.len(), 2);
        assert_eq!(req[1].chunk_index, 1);
        assert_eq!(req[1].start_in_chunk, 0);
        assert_eq!(req[1].end_in_chunk, 100);
    }

    #[test]
    fn range_empty_is_empty_request_list() {
        let m = five_chunk_manifest();
        assert!(byte_range_to_chunks(&m, 100, 100).unwrap().is_empty());
        // start past end-of-blob → empty too.
        assert!(byte_range_to_chunks(&m, m.size(), m.size())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn range_rejects_end_past_total_size() {
        let m = five_chunk_manifest();
        let err = byte_range_to_chunks(&m, 0, m.size() + 1).unwrap_err();
        assert!(matches!(err, BlobError::Decode(_)));
    }

    #[test]
    fn range_rejects_start_after_end() {
        let m = five_chunk_manifest();
        let err = byte_range_to_chunks(&m, 200, 100).unwrap_err();
        assert!(matches!(err, BlobError::Decode(_)));
    }

    #[test]
    fn range_rejects_call_against_small() {
        let s = BlobRef::small("file:///x", [0; 32], 100);
        let err = byte_range_to_chunks(&s, 0, 50).unwrap_err();
        assert!(matches!(err, BlobError::Decode(_)));
    }

    #[test]
    fn range_math_reassembles_exact_payload() {
        // End-to-end sanity: chunk a payload, then for several
        // sub-ranges, reconstruct the byte slice by walking the
        // chunk-range requests and assembling.
        let payload: Vec<u8> = (0..(BLOB_CHUNK_SIZE_BYTES as usize * 3 + 1000))
            .map(|i| (i % 251) as u8)
            .collect();
        let chunked = chunk_payload(&payload).unwrap();
        let (chunks_owned, total_size) = match chunked {
            ChunkedPayload::Chunked { chunks, total_size } => (chunks, total_size),
            _ => panic!("expected Chunked"),
        };
        let chunk_refs: Vec<ChunkRef> = chunks_owned.iter().map(|(r, _)| *r).collect();
        let chunk_bytes: Vec<&[u8]> = chunks_owned.iter().map(|(_, b)| *b).collect();
        let m = BlobRef::manifest("mesh://x", Encoding::Replicated, chunk_refs).unwrap();
        assert_eq!(m.size(), total_size);

        let cases = [
            (0u64, total_size),
            (10, 5_000_000),
            (BLOB_CHUNK_SIZE_BYTES, BLOB_CHUNK_SIZE_BYTES + 1),
            (total_size - 100, total_size),
        ];
        for (start, end) in cases {
            let requests = byte_range_to_chunks(&m, start, end).unwrap();
            let mut assembled = Vec::with_capacity((end - start) as usize);
            for r in requests {
                let chunk = chunk_bytes[r.chunk_index];
                assembled
                    .extend_from_slice(&chunk[r.start_in_chunk as usize..r.end_in_chunk as usize]);
            }
            assert_eq!(
                assembled,
                payload[start as usize..end as usize],
                "range [{}, {}) reassembly mismatch",
                start,
                end
            );
        }
    }

    // -----------------------------------------------------------
    // BlobRef::Tree (v0.3) constructor + wire round-trip
    // -----------------------------------------------------------

    fn tree_root() -> [u8; 32] {
        [0xAB; 32]
    }

    #[test]
    fn tree_constructor_sets_version_and_fields() {
        let r = BlobRef::tree(
            "mesh://ab".to_string(),
            Encoding::Replicated,
            tree_root(),
            1024 * 1024 * 1024 * 64, // 64 GiB
            2,
        )
        .unwrap();
        assert_eq!(r.version(), BLOB_REF_VERSION_V3_TREE);
        assert_eq!(r.uri(), "mesh://ab");
        assert_eq!(r.size(), 1024 * 1024 * 1024 * 64);
        assert_eq!(r.tree_depth(), Some(2));
        assert_eq!(r.tree_root_hash(), Some(&tree_root()));
        assert_eq!(r.encoding(), Some(Encoding::Replicated));
        assert!(r.is_chunked());
        assert!(r.is_tree());
        assert!(r.small_hash().is_none());
        assert!(r.chunks().is_empty());
    }

    #[test]
    fn tree_constructor_rejects_zero_total_size() {
        let err = BlobRef::tree("mesh://aa", Encoding::Replicated, tree_root(), 0, 1).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("must be > 0"), "got: {msg}");
    }

    #[test]
    fn tree_constructor_rejects_total_size_above_cap() {
        let err = BlobRef::tree(
            "mesh://aa",
            Encoding::Replicated,
            tree_root(),
            BLOB_TREE_MAX_TOTAL_SIZE + 1,
            4,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("exceeds cap"), "got: {msg}");
    }

    #[test]
    fn tree_constructor_rejects_zero_depth() {
        let err =
            BlobRef::tree("mesh://aa", Encoding::Replicated, tree_root(), 1024, 0).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("depth"), "got: {msg}");
    }

    #[test]
    fn tree_constructor_rejects_depth_above_cap() {
        let err = BlobRef::tree(
            "mesh://aa",
            Encoding::Replicated,
            tree_root(),
            1024,
            super::super::blob_tree::MAX_TREE_DEPTH + 1,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("depth"), "got: {msg}");
    }

    #[test]
    fn tree_encode_decode_round_trips() {
        let original = BlobRef::tree(
            "mesh://cafe".to_string(),
            Encoding::Replicated,
            tree_root(),
            1024 * 1024 * 1024, // 1 GiB
            1,
        )
        .unwrap();
        let bytes = original.encode();
        let decoded = BlobRef::decode(&bytes).unwrap().unwrap();
        assert_eq!(original, decoded);
        match decoded {
            BlobRef::Tree {
                version,
                uri,
                encoding,
                root_hash,
                total_size,
                depth,
            } => {
                assert_eq!(version, BLOB_REF_VERSION_V3_TREE);
                assert_eq!(uri, "mesh://cafe");
                assert_eq!(encoding, Encoding::Replicated);
                assert_eq!(root_hash, tree_root());
                assert_eq!(total_size, 1024 * 1024 * 1024);
                assert_eq!(depth, 1);
            }
            other => panic!("expected Tree, got {:?}", other),
        }
    }

    #[test]
    fn tree_decode_preserves_reedsolomon_encoding_tag() {
        let original = BlobRef::tree(
            "mesh://ff",
            Encoding::ReedSolomon { k: 10, m: 4 },
            tree_root(),
            1u64 << 40, // 1 TiB
            3,
        )
        .unwrap();
        let bytes = original.encode();
        let decoded = BlobRef::decode(&bytes).unwrap().unwrap();
        assert_eq!(
            decoded.encoding(),
            Some(Encoding::ReedSolomon { k: 10, m: 4 })
        );
    }

    #[test]
    fn tree_decode_rejects_unknown_outer_version() {
        // Hand-craft magic + an unknown version byte + arbitrary
        // postcard body bytes. Must surface UnsupportedVersion
        // rather than mis-decode as Small or Manifest.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&BLOB_REF_MAGIC);
        bytes.push(0xFE); // not 0x01/0x02/0x03
        bytes.extend_from_slice(&[0u8; 64]);
        let err = BlobRef::decode(&bytes).unwrap_err();
        assert!(
            matches!(err, BlobError::UnsupportedVersion(0xFE)),
            "expected UnsupportedVersion(0xFE), got {err:?}"
        );
    }

    #[test]
    fn tree_decode_rejects_unknown_body_version() {
        // Encode a tree, then hand-mutate the body_version field
        // (first byte after magic+outer-version) to an unknown
        // value. Decoder must surface UnsupportedVersion for the
        // body, not silently accept.
        let original =
            BlobRef::tree("mesh://aa", Encoding::Replicated, tree_root(), 1024, 1).unwrap();
        let mut bytes = original.encode();
        // The postcard body starts at offset 5. The body's first
        // field is `body_version: u8`, which postcard emits as a
        // single byte (no leading length prefix on `u8`). Mutate
        // it to an unknown value.
        bytes[5] = 0xEF;
        let err = BlobRef::decode(&bytes).unwrap_err();
        assert!(
            matches!(err, BlobError::UnsupportedVersion(0xEF)),
            "expected UnsupportedVersion(0xEF), got {err:?}"
        );
    }

    #[test]
    fn tree_decode_rejects_oversize_body() {
        // Hand-construct magic + outer version + a body whose
        // length exceeds BLOB_REF_TREE_BODY_MAX_BYTES. Decoder
        // must reject BEFORE postcard allocates so a malicious
        // peer can't force a large allocation.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&BLOB_REF_MAGIC);
        bytes.push(BLOB_REF_VERSION_V3_TREE);
        bytes.extend(std::iter::repeat_n(0u8, BLOB_REF_TREE_BODY_MAX_BYTES + 1));
        let err = BlobRef::decode(&bytes).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("exceeds cap"), "got: {msg}");
    }

    #[test]
    fn tree_decode_rejects_total_size_above_cap() {
        // Hand-encode a TreeBody with a u64 total_size past
        // BLOB_TREE_MAX_TOTAL_SIZE. Decoder catches it via the
        // post-decode validation, not via the constructor.
        let body = TreeBody {
            body_version: BLOB_TREE_BODY_VERSION,
            uri: "mesh://x".to_string(),
            encoding: Encoding::Replicated,
            root_hash: tree_root(),
            total_size: BLOB_TREE_MAX_TOTAL_SIZE + 1,
            depth: 4,
        };
        let body_bytes = postcard::to_allocvec(&body).unwrap();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&BLOB_REF_MAGIC);
        bytes.push(BLOB_REF_VERSION_V3_TREE);
        bytes.extend_from_slice(&body_bytes);
        let err = BlobRef::decode(&bytes).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("exceeds cap"), "got: {msg}");
    }

    #[test]
    fn tree_decode_rejects_depth_inconsistent_with_total_size() {
        // depth=4 against a 1-byte total_size is structurally
        // malformed — TREE_FANOUT^3 ≈ 2 M bytes is the lower bound
        // for a productive depth-4 tree. Pre-fix the walker
        // would still catch the mismatch at fetch time, but the
        // decode-side check short-circuits before any walk traffic.
        let body = TreeBody {
            body_version: BLOB_TREE_BODY_VERSION,
            uri: "mesh://x".to_string(),
            encoding: Encoding::Replicated,
            root_hash: tree_root(),
            total_size: 1,
            depth: 4,
        };
        let body_bytes = postcard::to_allocvec(&body).unwrap();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&BLOB_REF_MAGIC);
        bytes.push(BLOB_REF_VERSION_V3_TREE);
        bytes.extend_from_slice(&body_bytes);
        let err = BlobRef::decode(&bytes).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("requires total_size >="),
            "expected depth-vs-size lower-bound error; got: {msg}",
        );
    }

    #[test]
    fn tree_decode_rejects_depth_above_cap() {
        let body = TreeBody {
            body_version: BLOB_TREE_BODY_VERSION,
            uri: "mesh://x".to_string(),
            encoding: Encoding::Replicated,
            root_hash: tree_root(),
            total_size: 1024,
            depth: super::super::blob_tree::MAX_TREE_DEPTH + 1,
        };
        let body_bytes = postcard::to_allocvec(&body).unwrap();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&BLOB_REF_MAGIC);
        bytes.push(BLOB_REF_VERSION_V3_TREE);
        bytes.extend_from_slice(&body_bytes);
        let err = BlobRef::decode(&bytes).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("depth"), "got: {msg}");
    }

    #[test]
    fn verify_on_tree_returns_typed_error() {
        let r = BlobRef::tree("mesh://aa", Encoding::Replicated, tree_root(), 1024, 1).unwrap();
        let err = r.verify(b"any bytes").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Tree variant"),
            "Tree verify should surface a typed Decode error pointing at tree-walk; got: {msg}",
        );
    }

    #[test]
    fn tree_does_not_alias_small_or_manifest_via_decode() {
        // Round-trip three variants and assert each decodes back
        // to its own shape. Pre-fix the version-byte gate ensures
        // a Tree wire form is never mis-decoded as Small/Manifest.
        let small = BlobRef::small("mesh://aa", [0xAA; 32], 100);
        let manifest = BlobRef::manifest(
            "mesh://bb",
            Encoding::Replicated,
            vec![ChunkRef {
                hash: [0xBB; 32],
                size: 1024,
            }],
        )
        .unwrap();
        let tree = BlobRef::tree(
            "mesh://cc",
            Encoding::Replicated,
            [0xCC; 32],
            1024 * 1024 * 1024,
            1,
        )
        .unwrap();

        let s_decoded = BlobRef::decode(&small.encode()).unwrap().unwrap();
        let m_decoded = BlobRef::decode(&manifest.encode()).unwrap().unwrap();
        let t_decoded = BlobRef::decode(&tree.encode()).unwrap().unwrap();

        assert!(matches!(s_decoded, BlobRef::Small { .. }));
        assert!(matches!(m_decoded, BlobRef::Manifest { .. }));
        assert!(matches!(t_decoded, BlobRef::Tree { .. }));
        assert_eq!(s_decoded.version(), BLOB_REF_VERSION_V1);
        assert_eq!(m_decoded.version(), BLOB_REF_VERSION_V2_MANIFEST);
        assert_eq!(t_decoded.version(), BLOB_REF_VERSION_V3_TREE);
    }
}
