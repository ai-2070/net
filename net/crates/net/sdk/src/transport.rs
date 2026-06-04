//! Transport SDK surface — fairscheduler transport, blob transfer,
//! directory transfer.
//!
//! These are the on-demand, cross-peer *movement* primitives: pull a
//! content-addressed blob (or a whole directory tree) from a peer over
//! the substrate's reliable, fair-scheduled stream transport. They are
//! distinct from the two neighbouring surfaces:
//!
//! - `dataforts` exposes the *storage* + operator read side (the
//!   [`MeshBlobAdapter`] constructor, metrics, inventory).
//! - RedEX replication is a push/replication primitive; nRPC is a
//!   request/reply primitive. Transport is "fetch this exact content
//!   from that peer", multiplexed fairly against other traffic so a
//!   bulk pull can't starve interactive streams.
//!
//! The module re-exports the substrate primitives (the engine, the
//! `TransferControl` / `TransferHeader` wire types, the stream-id
//! helpers, and the directory types + [`store_dir`]) for advanced
//! callers, and adds thin ergonomic wrappers over a [`Mesh`] handle:
//! [`fetch_blob`] / [`fetch_blob_stream`] (and the holder-discovering
//! [`fetch_blob_discovered`]), [`fetch_dir`], and [`serve_blob_transfer`].
//!
//! # Serving is required to fetch
//!
//! The transfer engine is installed per node by [`serve_blob_transfer`].
//! A node must install it before it can *either* serve chunks to peers
//! *or* issue fetches — [`fetch_blob`] registers a pending transfer on
//! the local engine, so a node with no engine installed gets a
//! [`TransferError::Substrate`] error. Install once after the node is
//! built, against the same [`MeshBlobAdapter`] the node stores into.
//!
//! ```no_run
//! # use std::sync::Arc;
//! # async fn ex(mesh: &net_sdk::mesh::Mesh, adapter: Arc<net_sdk::dataforts::MeshBlobAdapter>, blob_ref: &net_sdk::transport::BlobRef, source: u64) -> Result<(), net_sdk::transport::TransferError> {
//! use net_sdk::transport;
//!
//! // Install the engine (idempotent; needed to serve AND to fetch).
//! transport::serve_blob_transfer(mesh, adapter);
//!
//! // Pull a blob from a known holder peer.
//! let bytes = transport::fetch_blob(mesh, source, blob_ref).await?;
//! # let _ = bytes;
//! # Ok(())
//! # }
//! ```

use std::path::Path;
use std::sync::Arc;

use bytes::{Bytes, BytesMut};
use futures::Stream;

use crate::mesh::Mesh;

// ── Re-exports ──────────────────────────────────────────────────────

// Blob-transfer engine + wire types, plus the stream-id convention
// helpers, for callers composing their own transfer-shaped operations
// on the fairscheduler transport.
pub use net::adapter::net::dataforts::blob::transfer::{
    BlobTransferEngine, TransferControl, TransferHeader,
};
pub use net::adapter::net::dataforts::blob::{
    is_transfer_stream_id, next_transfer_stream_id, transfer_stream_id, BlobError, BlobRef,
    MeshBlobAdapter, SUBPROTOCOL_BLOB_TRANSFER,
};

// Operator-introspection RPC over a node's transfer engine: the typed
// client + status shape behind `net transfer ls / status / cancel`, and
// the server-side error variants for matching. [`serve_blob_transfer_rpc`]
// (below) installs the matching handler.
pub use net::adapter::net::dataforts::blob::{
    BlobTransferClient, TransferClientError, TransferRpcError, TransferStatus,
};
// Returned by [`serve_blob_transfer_rpc`]; the caller holds it to keep the
// RPC registered (drop = stop answering).
pub use net::adapter::net::mesh_rpc::{ServeError, ServeHandle};

// Store-side helpers for building a content-addressed [`BlobRef`] from raw
// bytes without reaching into the substrate: [`chunk_payload`] splits a byte
// slice into the inline-or-chunked shape and [`ChunkedPayload::into_blob_ref`]
// finishes it into a [`BlobRef::Small`] / [`BlobRef::Manifest`] under an
// [`Encoding`]. These are the inverse of [`fetch_blob`] — what a publisher runs
// to learn the reference a peer will [`fetch_blob`] by. Re-exported so the
// `net transfer send-blob` CLI verb (and any SDK consumer staging content for
// fetch) doesn't reimplement chunk sizing / hashing.
pub use net::adapter::net::dataforts::{chunk_payload, ChunkedPayload, Encoding};

// Directory transfer: the substrate `store_dir` is usable as-is (it
// takes a `&MeshBlobAdapter`); the fetch side is wrapped below because
// the substrate function needs the internal node handle. The manifest
// types are re-exported so applications can introspect a tree before
// (or instead of) reconstructing it — the hook a future directory-sync
// composition layer builds on.
pub use net::adapter::net::dataforts::{
    store_dir, DirEntry, DirManifest, DirStats, EntryKind, DEFAULT_FETCH_CONCURRENCY,
    DIR_MANIFEST_VERSION,
};
// The substrate's directory-error type, surfaced for the `From`
// conversion into [`TransferError`] and for callers that re-export it.
pub use net::adapter::net::dataforts::DirError;

// ── Error surface ───────────────────────────────────────────────────

/// Stable SDK-facing transfer error. Translates the substrate's
/// [`BlobError`] / [`DirError`] into a small, durable shape so the SDK
/// contract doesn't churn when the substrate's internal error enums
/// grow variants.
#[derive(Debug)]
pub enum TransferError {
    /// The content was not available — a known holder didn't have it, or
    /// (for [`fetch_blob_discovered`]) the search ran out of candidates
    /// before any peer served it (see [`Self::AllPeersFailed`]).
    NotFound(String),
    /// Holder discovery exhausted every connected peer without one
    /// serving the content. Distinct from [`Self::NotFound`] so a caller
    /// can tell "this specific holder lacks it" from "nobody I'm
    /// connected to has it".
    AllPeersFailed(String),
    /// Fetched bytes did not hash to the expected content address — the
    /// substrate verifies every fetch, so this is a hard integrity
    /// failure, never silently accepted.
    HashMismatch {
        /// Hash recorded on the [`BlobRef`].
        expected: [u8; 32],
        /// Hash computed over the fetched bytes.
        actual: [u8; 32],
    },
    /// Any other substrate-level failure (engine not installed, transport
    /// error, manifest decode, unsafe path, cancellation, …). The string
    /// is the underlying error's `Display`.
    Substrate(String),
}

impl std::fmt::Display for TransferError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(m) => write!(f, "transfer: not found: {m}"),
            Self::AllPeersFailed(m) => write!(f, "transfer: all peers failed: {m}"),
            Self::HashMismatch { expected, actual } => write!(
                f,
                "transfer: hash mismatch (expected {}, got {})",
                hex32(expected),
                hex32(actual)
            ),
            Self::Substrate(m) => write!(f, "transfer: {m}"),
        }
    }
}

impl std::error::Error for TransferError {}

impl From<BlobError> for TransferError {
    fn from(e: BlobError) -> Self {
        match e {
            BlobError::NotFound(m) => Self::NotFound(m),
            BlobError::HashMismatch { expected, actual } => Self::HashMismatch { expected, actual },
            other => Self::Substrate(other.to_string()),
        }
    }
}

impl From<DirError> for TransferError {
    fn from(e: DirError) -> Self {
        // Map the blob-carrying directory error through the BlobError
        // mapping so a directory fetch reports the same NotFound /
        // HashMismatch shape as a bare blob fetch; everything else
        // (unsafe path, manifest decode, io) is opaque substrate detail.
        match e {
            DirError::Blob(b) => b.into(),
            other => Self::Substrate(other.to_string()),
        }
    }
}

// ── Serving ─────────────────────────────────────────────────────────

/// Install the blob-transfer engine on `mesh` over `adapter`. Required
/// before the node can serve chunk fetches to peers **or** issue its own
/// fetches ([`fetch_blob`] registers state on the local engine).
/// Idempotent — re-installing replaces the engine.
pub fn serve_blob_transfer(mesh: &Mesh, adapter: Arc<MeshBlobAdapter>) {
    mesh.node().serve_blob_transfer(adapter);
}

/// Like [`serve_blob_transfer`] but also registers the `blob.transfers`
/// operator-introspection RPC (list / status / cancel) over the same
/// engine, so a remote operator (`net transfer ls / status / cancel`) can
/// query and cancel this node's in-flight, requester-side transfers.
/// Returns the [`ServeHandle`]; **hold it** for as long as the RPC should
/// answer (dropping it stops the RPC; the engine itself stays installed).
pub fn serve_blob_transfer_rpc(
    mesh: &Mesh,
    adapter: Arc<MeshBlobAdapter>,
) -> Result<ServeHandle, ServeError> {
    mesh.node().serve_blob_transfer_with_rpc(adapter)
}

// ── Blob fetch ──────────────────────────────────────────────────────

/// Fetch a whole blob (every chunk of a [`BlobRef`]) from the known
/// holder `source`, returning the reassembled, BLAKE3-verified bytes.
///
/// A [`BlobRef::Small`] is one chunk; a [`BlobRef::Manifest`] is its
/// ordered chunk list, concatenated in manifest order. [`BlobRef::Tree`]
/// is not supported by this wrapper (use the substrate tree walk).
///
/// Each chunk is fetched over the reliable, fair-scheduled stream
/// transport; verification is enforced by the substrate, so a hash
/// disagreement surfaces as [`TransferError::HashMismatch`] rather than
/// returning suspect bytes.
pub async fn fetch_blob(
    mesh: &Mesh,
    source: u64,
    blob_ref: &BlobRef,
) -> Result<Bytes, TransferError> {
    let node = mesh.node();
    match blob_ref {
        BlobRef::Small { hash, .. } => Ok(node.transfer_fetch_chunk(source, *hash).await?),
        BlobRef::Manifest {
            chunks, total_size, ..
        } => {
            let mut buf = BytesMut::with_capacity(*total_size as usize);
            for chunk in chunks {
                let bytes = node.transfer_fetch_chunk(source, chunk.hash).await?;
                buf.extend_from_slice(&bytes);
            }
            Ok(buf.freeze())
        }
        BlobRef::Tree { .. } => Err(TransferError::Substrate(
            "BlobRef::Tree not supported by the transport wrapper".into(),
        )),
    }
}

/// Like [`fetch_blob`], but discovers the holder among connected peers
/// instead of the caller naming one. Probes peers in turn; the first to
/// serve the verified bytes wins. Returns [`TransferError::AllPeersFailed`]
/// if no connected peer has the content.
///
/// Per-chunk discovery is more expensive than a known-source fetch
/// (each chunk re-probes), so prefer [`fetch_blob`] when the holder is
/// known (e.g. directory transfer pulls from a single source).
pub async fn fetch_blob_discovered(
    mesh: &Mesh,
    blob_ref: &BlobRef,
) -> Result<Bytes, TransferError> {
    let node = mesh.node();
    let discovered = |hash: [u8; 32]| async move {
        node.transfer_fetch_chunk_discovered(hash)
            .await
            .map_err(|e| match e {
                // Discovery returns NotFound when no peer served it;
                // re-tag as AllPeersFailed so the caller can distinguish.
                BlobError::NotFound(m) => TransferError::AllPeersFailed(m),
                other => other.into(),
            })
    };
    match blob_ref {
        BlobRef::Small { hash, .. } => discovered(*hash).await,
        BlobRef::Manifest {
            chunks, total_size, ..
        } => {
            let mut buf = BytesMut::with_capacity(*total_size as usize);
            for chunk in chunks {
                let bytes = discovered(chunk.hash).await?;
                buf.extend_from_slice(&bytes);
            }
            Ok(buf.freeze())
        }
        BlobRef::Tree { .. } => Err(TransferError::Substrate(
            "BlobRef::Tree not supported by the transport wrapper".into(),
        )),
    }
}

/// Stream a blob from `source` chunk-by-chunk: each yielded item is one
/// verified chunk's bytes in manifest order, so a large blob is consumed
/// incrementally without buffering the whole payload in memory.
///
/// A [`BlobRef::Small`] yields a single item; a [`BlobRef::Manifest`]
/// yields one item per chunk. [`BlobRef::Tree`] yields a single
/// [`TransferError::Substrate`] error item. The first error terminates
/// the stream (no further chunks are fetched).
pub fn fetch_blob_stream(
    mesh: &Mesh,
    source: u64,
    blob_ref: &BlobRef,
) -> impl Stream<Item = Result<Bytes, TransferError>> {
    let node = mesh.node().clone();
    // Resolve the ordered per-chunk hash list (or a terminal error for
    // the unsupported Tree case) eagerly, then fetch lazily as the
    // consumer polls. `unfold` carries the remaining-chunks iterator as
    // its state so a fetch only happens on demand, and so an error can
    // be surfaced AND end the stream (a `take_while` would drop the
    // error item; `then` alone would keep fetching after it).
    let items: Vec<Result<[u8; 32], TransferError>> = match blob_ref {
        BlobRef::Small { hash, .. } => vec![Ok(*hash)],
        BlobRef::Manifest { chunks, .. } => chunks.iter().map(|c| Ok(c.hash)).collect(),
        BlobRef::Tree { .. } => vec![Err(TransferError::Substrate(
            "BlobRef::Tree not supported by the transport wrapper".into(),
        ))],
    };
    futures::stream::unfold(items.into_iter(), move |mut remaining| {
        let node = node.clone();
        async move {
            let next = remaining.next()?; // exhausted → end the stream
            let out = match next {
                Ok(hash) => node
                    .transfer_fetch_chunk(source, hash)
                    .await
                    .map_err(TransferError::from),
                Err(e) => Err(e),
            };
            // First error terminates the stream after it's surfaced:
            // once a chunk fails the blob can't be completed, so drop the
            // rest and fetch nothing further.
            let rest = if out.is_err() {
                Vec::<Result<[u8; 32], TransferError>>::new().into_iter()
            } else {
                remaining
            };
            Some((out, rest))
        }
    })
}

// ── Directory fetch ─────────────────────────────────────────────────

/// Pull the directory whose manifest is `manifest_ref` from `source` and
/// reconstruct it under `dest`, returning what was written. `concurrency`
/// bounds how many leaf files race for the transport at once
/// ([`DEFAULT_FETCH_CONCURRENCY`] when `0`).
///
/// Manifest paths are validated to stay within `dest` (a hostile sender
/// cannot escape the destination root). [`store_dir`] is the matching
/// store side and is usable directly (it takes a [`MeshBlobAdapter`]).
pub async fn fetch_dir(
    mesh: &Mesh,
    source: u64,
    manifest_ref: &BlobRef,
    dest: &Path,
    concurrency: usize,
) -> Result<DirStats, TransferError> {
    net::adapter::net::dataforts::fetch_dir(mesh.node(), source, manifest_ref, dest, concurrency)
        .await
        .map_err(TransferError::from)
}

/// Lowercase-hex render of a 32-byte hash for error messages.
fn hex32(hash: &[u8; 32]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(64);
    for b in hash {
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_error_maps_to_stable_shape() {
        // NotFound and HashMismatch get dedicated variants; everything
        // else collapses to Substrate so the SDK shape is durable.
        assert!(matches!(
            TransferError::from(BlobError::NotFound("x".into())),
            TransferError::NotFound(_)
        ));
        assert!(matches!(
            TransferError::from(BlobError::HashMismatch {
                expected: [1u8; 32],
                actual: [2u8; 32],
            }),
            TransferError::HashMismatch { .. }
        ));
        assert!(matches!(
            TransferError::from(BlobError::Backend("boom".into())),
            TransferError::Substrate(_)
        ));
        assert!(matches!(
            TransferError::from(BlobError::Cancelled),
            TransferError::Substrate(_)
        ));
    }

    #[test]
    fn dir_error_routes_blob_failures_through_blob_mapping() {
        // A blob-carrying directory error reports the same NotFound shape
        // as a bare blob fetch; other directory errors stay opaque.
        assert!(matches!(
            TransferError::from(DirError::Blob(BlobError::NotFound("x".into()))),
            TransferError::NotFound(_)
        ));
        assert!(matches!(
            TransferError::from(DirError::UnsafePath("../escape".into())),
            TransferError::Substrate(_)
        ));
    }

    #[test]
    fn hash_mismatch_display_renders_both_hashes() {
        let e = TransferError::HashMismatch {
            expected: [0xABu8; 32],
            actual: [0xCDu8; 32],
        };
        let s = e.to_string();
        assert!(s.contains(&"ab".repeat(32)));
        assert!(s.contains(&"cd".repeat(32)));
    }
}
