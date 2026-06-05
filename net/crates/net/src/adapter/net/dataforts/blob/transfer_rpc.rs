//! `blob.transfers` RPC service + client — operator-introspection surface
//! over a node's
//! [`BlobTransferEngine`](crate::adapter::net::dataforts::blob::transfer::BlobTransferEngine).
//!
//! Mirrors the `aggregator.registry` pattern (postcard-encoded request /
//! response enums, an
//! [`RpcHandler`](crate::adapter::net::cortex::rpc::RpcHandler) holding the
//! live engine, a pure-fn `answer` for unit testing, and a typed client
//! wrapping [`typed_call`](crate::adapter::net::mesh_rpc::typed_call)). It
//! powers `net transfer ls / status / cancel`: a remote-attached client
//! asks a daemon's engine what it is currently fetching and can cancel a
//! specific transfer.
//!
//! # Receiver-side only
//!
//! The engine tracks **requester-side** in-flight transfers (what this
//! node is fetching). Serving tasks are fire-and-forget and not tracked,
//! so `ls` reflects fetches, not what the node serves. See
//! [`BlobTransferEngine::list_pending`](crate::adapter::net::dataforts::blob::transfer::BlobTransferEngine::list_pending).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use serde::{Deserialize, Serialize};

use super::transfer::{BlobTransferEngine, TransferStatus};
use crate::adapter::net::cortex::rpc::{
    RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus,
};
use crate::adapter::net::mesh_rpc::{typed_call, RpcError, TypedCallError};
use crate::adapter::net::MeshNode;

/// Canonical service name (routes to `blob.transfers.requests` /
/// `blob.transfers.replies.<origin>`).
pub const TRANSFER_SERVICE: &str = "blob.transfers";

/// Default RPC deadline — long enough to absorb cross-subnet latency,
/// short enough that a wedged daemon surfaces quickly. Mirrors the
/// registry client's default.
pub const DEFAULT_TRANSFER_DEADLINE: Duration = Duration::from_secs(3);

/// Wire-shaped request. Postcard-encoded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransferRpcRequest {
    /// Enumerate every requester-side in-flight transfer. Read-only.
    List,
    /// Snapshot one transfer by stream id. Read-only.
    Get {
        /// Transfer stream id.
        stream_id: u64,
    },
    /// Cancel one transfer by stream id. Dropping the pending entry fails
    /// the awaiting fetch on the target node.
    Cancel {
        /// Transfer stream id.
        stream_id: u64,
    },
}

/// Wire-shaped response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransferRpcResponse {
    /// Successful `List` reply.
    Transfers(Vec<TransferStatus>),
    /// Successful `Get` reply — `None` when no such transfer is pending.
    Transfer(Option<TransferStatus>),
    /// `Cancel` reply: `true` when a transfer was removed, `false` when no
    /// transfer with that id was pending.
    Cancelled {
        /// True iff a pending transfer by that id was present.
        existed: bool,
    },
    /// Handler-level error (request decode failure, engine not installed).
    Error(TransferRpcError),
}

/// Server-side handler error, carried in [`TransferRpcResponse::Error`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum TransferRpcError {
    /// The request body failed to decode.
    #[error("request decode failed: {0}")]
    DecodeFailed(String),
    /// The target node has no blob-transfer engine installed, so it has no
    /// transfer registry to query. (Only surfaced if the service is
    /// somehow registered without an engine — the install path installs
    /// both together.)
    #[error("blob-transfer engine not installed on target")]
    EngineNotInstalled,
}

/// Pure-function answer logic, broken out for direct unit testing without
/// the RPC plumbing.
pub(crate) fn answer(
    engine: &BlobTransferEngine,
    request: &TransferRpcRequest,
) -> TransferRpcResponse {
    match request {
        TransferRpcRequest::List => TransferRpcResponse::Transfers(engine.list_pending()),
        TransferRpcRequest::Get { stream_id } => {
            TransferRpcResponse::Transfer(engine.get_pending(*stream_id))
        }
        TransferRpcRequest::Cancel { stream_id } => TransferRpcResponse::Cancelled {
            existed: engine.cancel_pending_reporting(*stream_id),
        },
    }
}

/// RPC handler holding the live engine. Registered under
/// [`TRANSFER_SERVICE`] — the SDK's `transport::serve_blob_transfer_rpc`
/// installs the engine and serves this handler through `Mesh::serve_rpc`
/// (which auto-registers the service's channels).
pub struct TransferRpcHandler {
    engine: Arc<BlobTransferEngine>,
}

impl TransferRpcHandler {
    /// Build a handler over `engine`.
    pub fn new(engine: Arc<BlobTransferEngine>) -> Self {
        Self { engine }
    }
}

#[async_trait]
impl RpcHandler for TransferRpcHandler {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        let request: TransferRpcRequest = match postcard::from_bytes(&ctx.payload.body) {
            Ok(req) => req,
            Err(e) => {
                return Ok(encode_response(&TransferRpcResponse::Error(
                    TransferRpcError::DecodeFailed(e.to_string()),
                )));
            }
        };
        Ok(encode_response(&answer(&self.engine, &request)))
    }
}

fn encode_response(response: &TransferRpcResponse) -> RpcResponsePayload {
    let body = match postcard::to_allocvec(response) {
        Ok(b) => Bytes::from(b),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "blob.transfers: response encode failed; replying with empty body",
            );
            Bytes::new()
        }
    };
    RpcResponsePayload {
        status: RpcStatus::Ok,
        headers: Vec::new(),
        body,
    }
}

/// Client-side errors. Distinct variants for transport, codec, and
/// server-handler failures so callers can match the shape they care
/// about. Mirrors `RegistryClientError`.
#[derive(Debug, thiserror::Error)]
pub enum TransferClientError {
    /// Transport-level failure (no route, timeout, non-Ok status).
    #[error("transport: {0}")]
    Transport(RpcError),
    /// Request serialization or response deserialization failed.
    #[error("codec: {0}")]
    Codec(String),
    /// Server handler rejected the request.
    #[error("server: {0}")]
    Server(TransferRpcError),
}

impl From<RpcError> for TransferClientError {
    fn from(e: RpcError) -> Self {
        Self::Transport(e)
    }
}

impl From<TypedCallError> for TransferClientError {
    fn from(e: TypedCallError) -> Self {
        match e {
            TypedCallError::Transport(t) => Self::Transport(t),
            TypedCallError::Codec(c) => Self::Codec(c),
        }
    }
}

/// Typed `blob.transfers` client. Cheap to clone.
#[derive(Clone)]
pub struct BlobTransferClient {
    mesh: Arc<MeshNode>,
    deadline: Duration,
}

impl BlobTransferClient {
    /// Build a client backed by `mesh` with the default deadline.
    pub fn new(mesh: Arc<MeshNode>) -> Self {
        Self {
            mesh,
            deadline: DEFAULT_TRANSFER_DEADLINE,
        }
    }

    /// Override the per-call deadline (fluent).
    pub fn with_deadline(mut self, deadline: Duration) -> Self {
        self.deadline = deadline;
        self
    }

    /// List every in-flight (requester-side) transfer on the target node.
    pub async fn list(
        &self,
        target_node_id: u64,
    ) -> Result<Vec<TransferStatus>, TransferClientError> {
        match self.send(target_node_id, TransferRpcRequest::List).await? {
            TransferRpcResponse::Transfers(v) => Ok(v),
            TransferRpcResponse::Error(e) => Err(TransferClientError::Server(e)),
            other => Err(TransferClientError::Codec(format!(
                "unexpected response for List: {other:?}"
            ))),
        }
    }

    /// Snapshot one transfer by stream id; `None` when not pending.
    pub async fn get(
        &self,
        target_node_id: u64,
        stream_id: u64,
    ) -> Result<Option<TransferStatus>, TransferClientError> {
        match self
            .send(target_node_id, TransferRpcRequest::Get { stream_id })
            .await?
        {
            TransferRpcResponse::Transfer(t) => Ok(t),
            TransferRpcResponse::Error(e) => Err(TransferClientError::Server(e)),
            other => Err(TransferClientError::Codec(format!(
                "unexpected response for Get: {other:?}"
            ))),
        }
    }

    /// Cancel one transfer by stream id; `true` when one was removed.
    pub async fn cancel(
        &self,
        target_node_id: u64,
        stream_id: u64,
    ) -> Result<bool, TransferClientError> {
        match self
            .send(target_node_id, TransferRpcRequest::Cancel { stream_id })
            .await?
        {
            TransferRpcResponse::Cancelled { existed } => Ok(existed),
            TransferRpcResponse::Error(e) => Err(TransferClientError::Server(e)),
            other => Err(TransferClientError::Codec(format!(
                "unexpected response for Cancel: {other:?}"
            ))),
        }
    }

    /// Shared marshalling helper — encode request, fire, decode response.
    async fn send(
        &self,
        target_node_id: u64,
        request: TransferRpcRequest,
    ) -> Result<TransferRpcResponse, TransferClientError> {
        Ok(typed_call::<TransferRpcRequest, TransferRpcResponse>(
            &self.mesh,
            target_node_id,
            TRANSFER_SERVICE,
            &request,
            self.deadline,
        )
        .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answer_list_get_cancel_round_trip_through_postcard() {
        // The wire enums must postcard round-trip (the client decodes what
        // the handler encodes). Exercise each variant.
        for resp in [
            TransferRpcResponse::Transfers(vec![TransferStatus {
                stream_id: 7,
                holder: 42,
                expected_hash: [9u8; 32],
                bytes_received: 1024,
                total_bytes: Some(4096),
            }]),
            TransferRpcResponse::Transfer(None),
            TransferRpcResponse::Cancelled { existed: true },
            TransferRpcResponse::Error(TransferRpcError::DecodeFailed("x".into())),
        ] {
            let bytes = postcard::to_allocvec(&resp).expect("encode");
            let back: TransferRpcResponse = postcard::from_bytes(&bytes).expect("decode");
            assert_eq!(resp, back);
        }
    }
}
