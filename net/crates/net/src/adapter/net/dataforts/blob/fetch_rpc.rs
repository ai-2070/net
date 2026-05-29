//! `blob.fetch_chunk` nRPC service (federation S-1).
//!
//! Cross-peer, content-addressed chunk fetch over the substrate's
//! UDP-multiplexed transport. A node running a [`MeshBlobAdapter`]
//! registers [`BlobFetchChunkHandler`] under
//! [`BLOB_FETCH_CHUNK_SERVICE`] via
//! [`crate::adapter::net::MeshNode::serve_blob_fetch_chunk`]; a peer
//! that misses a chunk locally calls the service (see S-2's
//! [`MeshBlobAdapter::fetch`] fallback) to pull the bytes.
//!
//! # Why unary + range paging rather than whole-chunk streaming
//!
//! A blob chunk can be a full [`super::blob_ref::BLOB_CHUNK_SIZE_BYTES`]
//! (4 MiB) — exactly the nRPC body cap
//! ([`crate::adapter::net::cortex::MAX_RPC_BODY_LEN`]). A whole-chunk
//! response plus postcard framing would overflow that cap, and the
//! streaming sink ([`crate::adapter::net::cortex::RpcResponseSink`])
//! drops chunks on overflow — a dropped frame mid-chunk corrupts the
//! reassembled blob (caught by the fetch-side BLAKE3 verify, but only
//! after the round trip is wasted). So `blob.fetch_chunk` is a unary
//! request/response that serves a bounded
//! [`FETCH_CHUNK_SEGMENT_BYTES`] window; the caller pages a larger
//! chunk across several requests. Unary RPC carries substrate-managed
//! per-call reliability with no silent-drop failure mode. Whole-chunk
//! streaming is a documented follow-up (the plan's "probably 256KB"
//! threshold) once flow-controlled streaming is the better trade.
//!
//! Each call rides the shared `UdpSocket` in `router.rs`, multiplexed
//! with every other in-flight operation — no per-call connection
//! setup, no per-call handshake. The substrate's session-level
//! channel-auth + capability-auth (`nrpc:dataforts.blob.fetch_chunk`)
//! gate the call; peer selection is done caller-side against the
//! `causal:<hex>` advertisement (S-3), so a node only ever calls peers
//! that advertise the chunk.
//!
//! [`MeshBlobAdapter`]: super::mesh::MeshBlobAdapter
//! [`MeshBlobAdapter::fetch`]: super::mesh::MeshBlobAdapter::fetch

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::error::BlobError;
use super::mesh::MeshBlobAdapter;

/// Service-name token for the blob fetch-chunk nRPC channel. The
/// caller constructs a request on `"{BLOB_FETCH_CHUNK_SERVICE}.requests"`
/// and listens on `"{BLOB_FETCH_CHUNK_SERVICE}.replies.<origin>"`; the
/// server registers a handler under the same name via
/// [`crate::adapter::net::MeshNode::serve_blob_fetch_chunk`].
///
/// Held as a const so a typo on either side surfaces at compile time.
/// The wire form is the literal string — no version suffix (payload-
/// level versioning lives inside the postcard body, not the channel
/// name). Mirrors [`super::overflow::OVERFLOW_PUSH_SERVICE`].
pub const BLOB_FETCH_CHUNK_SERVICE: &str = "dataforts.blob.fetch_chunk";

/// Maximum bytes a single [`FetchChunkResponse::Segment`] carries.
/// 1 MiB sits well under the 4 MiB
/// [`crate::adapter::net::cortex::MAX_RPC_BODY_LEN`] so a segment plus
/// postcard framing never overflows the unary body cap, regardless of
/// chunk size. Callers fetching a chunk larger than this page the
/// remainder with successive range requests.
pub const FETCH_CHUNK_SEGMENT_BYTES: u64 = 1024 * 1024;

/// Wire request body for a chunk fetch. Encoded via postcard.
///
/// `range` is an optional half-open `[start, end)` byte window into
/// the chunk. `None` requests the window `[0, FETCH_CHUNK_SEGMENT_BYTES)`
/// (clamped to the chunk length). Any requested window wider than
/// [`FETCH_CHUNK_SEGMENT_BYTES`] is clamped server-side so a caller
/// cannot force the server to materialize an over-cap response.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchChunkRequest {
    /// 32-byte BLAKE3 content address of the chunk.
    pub hash: [u8; 32],
    /// Optional half-open `[start, end)` byte window into the chunk.
    pub range: Option<(u64, u64)>,
}

/// Wire response body. Encoded via postcard.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FetchChunkResponse {
    /// A (possibly partial) segment of the chunk. `total_len` is the
    /// full chunk length so the caller knows how many more segments
    /// to page; `offset` is where `data` begins within the chunk.
    /// `data.len()` never exceeds [`FETCH_CHUNK_SEGMENT_BYTES`].
    Segment {
        /// Full byte length of the chunk on the serving node.
        total_len: u64,
        /// Byte offset of `data` within the chunk.
        offset: u64,
        /// The served bytes.
        data: Vec<u8>,
    },
    /// The serving node does not hold this chunk locally (absent /
    /// empty file), or its local copy failed verification. Either
    /// way the caller should try the next advertised holder. Never
    /// serves bytes that fail the local BLAKE3 check.
    NotFound,
}

impl FetchChunkRequest {
    /// Resolve the effective `[start, end)` window for this request
    /// against a chunk of `total_len` bytes, clamping to the chunk
    /// bounds and to [`FETCH_CHUNK_SEGMENT_BYTES`].
    fn effective_window(&self, total_len: u64) -> (u64, u64) {
        let (start, end) = match self.range {
            Some((s, e)) => (s, e),
            None => (0, total_len),
        };
        let start = start.min(total_len);
        let end = end.clamp(start, total_len);
        // Never serve more than one segment per call.
        let end = end.min(start.saturating_add(FETCH_CHUNK_SEGMENT_BYTES));
        (start, end)
    }
}

/// Receive-side handler for the `blob.fetch_chunk` nRPC. Implements
/// [`crate::adapter::net::cortex::RpcHandler`] so it slots into
/// [`crate::adapter::net::MeshNode::serve_rpc`] under
/// [`BLOB_FETCH_CHUNK_SERVICE`].
///
/// Reads strictly from the local store via
/// [`MeshBlobAdapter::fetch_chunk_local`] — never the peer-aware
/// [`MeshBlobAdapter::fetch_chunk`] — so a serving node answers from
/// its own Redex and can never fan a request back out to the mesh.
///
/// Holds `Arc<MeshBlobAdapter>` because the handler is owned by the
/// nRPC fold (`Arc<dyn RpcHandler>`) and outlives any single call;
/// the adapter is cheap to clone (Arc-internal throughout).
pub struct BlobFetchChunkHandler {
    adapter: Arc<MeshBlobAdapter>,
}

impl BlobFetchChunkHandler {
    /// Construct a handler over `adapter`. Operators wire this in via
    /// [`crate::adapter::net::MeshNode::serve_blob_fetch_chunk`].
    pub fn new(adapter: Arc<MeshBlobAdapter>) -> Self {
        Self { adapter }
    }

    /// Pure typed handler logic: decoded request in, typed response
    /// out. Split from the [`crate::adapter::net::cortex::RpcHandler`]
    /// impl so tests drive the read path without constructing an
    /// [`crate::adapter::net::cortex::RpcContext`].
    pub async fn handle(&self, request: FetchChunkRequest) -> FetchChunkResponse {
        match self.adapter.fetch_chunk_local(&request.hash).await {
            Ok(bytes) => {
                let total_len = bytes.len() as u64;
                let (start, end) = request.effective_window(total_len);
                // `start`/`end` are bounded by `total_len` above, so
                // the slice indices are in range by construction.
                let data = bytes[start as usize..end as usize].to_vec();
                FetchChunkResponse::Segment {
                    total_len,
                    offset: start,
                    data,
                }
            }
            // Absent locally → the caller tries the next holder.
            Err(BlobError::NotFound(_)) => FetchChunkResponse::NotFound,
            // A local read error (corrupted on-disk bytes surfacing as
            // HashMismatch, a transient backend fault) must NOT serve
            // suspect bytes — answer NotFound so the caller falls
            // through to another advertised holder. Log so the local
            // corruption is still visible to operators.
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    hash = %super::hex32(&request.hash),
                    "blob fetch_chunk: local read failed; answering NotFound",
                );
                FetchChunkResponse::NotFound
            }
        }
    }
}

#[async_trait]
impl crate::adapter::net::cortex::RpcHandler for BlobFetchChunkHandler {
    async fn call(
        &self,
        ctx: crate::adapter::net::cortex::RpcContext,
    ) -> Result<
        crate::adapter::net::cortex::RpcResponsePayload,
        crate::adapter::net::cortex::RpcHandlerError,
    > {
        use crate::adapter::net::cortex::{RpcHandlerError, RpcResponsePayload, RpcStatus};

        // Malformed bytes surface as Internal — distinct from a typed
        // NotFound, which is an ordinary negative answer carried in
        // an Ok response body.
        let request: FetchChunkRequest = postcard::from_bytes(&ctx.payload.body).map_err(|e| {
            RpcHandlerError::Internal(format!("blob fetch_chunk: decode request failed: {e}"))
        })?;

        let response = self.handle(request).await;

        let body = postcard::to_allocvec(&response).map_err(|e| {
            RpcHandlerError::Internal(format!("blob fetch_chunk: encode response failed: {e}"))
        })?;
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: Vec::new(),
            body: bytes::Bytes::from(body),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_window_defaults_to_first_segment() {
        let req = FetchChunkRequest {
            hash: [0u8; 32],
            range: None,
        };
        // A chunk smaller than a segment: whole chunk.
        assert_eq!(req.effective_window(1024), (0, 1024));
        // A chunk larger than a segment: first segment only.
        assert_eq!(
            req.effective_window(4 * 1024 * 1024),
            (0, FETCH_CHUNK_SEGMENT_BYTES)
        );
    }

    #[test]
    fn effective_window_clamps_explicit_range_to_chunk_and_segment() {
        let req = FetchChunkRequest {
            hash: [0u8; 32],
            range: Some((100, 100 + 4 * 1024 * 1024)),
        };
        // end clamped to total_len, then to one segment from start.
        let (start, end) = req.effective_window(2 * 1024 * 1024);
        assert_eq!(start, 100);
        assert_eq!(end, 100 + FETCH_CHUNK_SEGMENT_BYTES);
    }

    #[test]
    fn effective_window_clamps_out_of_range_start() {
        let req = FetchChunkRequest {
            hash: [0u8; 32],
            range: Some((9_000, 10_000)),
        };
        // start past EOF clamps to total_len; empty window.
        assert_eq!(req.effective_window(4_096), (4_096, 4_096));
    }

    #[test]
    fn request_response_round_trip_postcard() {
        let req = FetchChunkRequest {
            hash: [7u8; 32],
            range: Some((0, 16)),
        };
        let encoded = postcard::to_allocvec(&req).unwrap();
        let decoded: FetchChunkRequest = postcard::from_bytes(&encoded).unwrap();
        assert_eq!(req, decoded);

        let resp = FetchChunkResponse::Segment {
            total_len: 16,
            offset: 0,
            data: vec![1, 2, 3, 4],
        };
        let encoded = postcard::to_allocvec(&resp).unwrap();
        let decoded: FetchChunkResponse = postcard::from_bytes(&encoded).unwrap();
        assert_eq!(resp, decoded);

        let nf = FetchChunkResponse::NotFound;
        let encoded = postcard::to_allocvec(&nf).unwrap();
        let decoded: FetchChunkResponse = postcard::from_bytes(&encoded).unwrap();
        assert_eq!(nf, decoded);
    }
}
