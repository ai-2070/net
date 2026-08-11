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

use std::collections::HashSet;
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
    /// The calling session peer is not an operator of this node's
    /// transfer engine. See [`TransferAdminPolicy`].
    #[error("caller is not authorized to administer this node's transfers")]
    Unauthorized,
}

/// Who may inspect and cancel this node's in-flight transfers.
///
/// # Why this exists
///
/// `blob.transfers` is described in this module's own docs as an
/// "operator-introspection surface", but it was registered as an
/// ordinary public nRPC service and the handler never looked at who
/// was calling. Any admitted mesh peer could therefore:
///
/// - enumerate in-flight stream ids, holder identities, expected
///   content hashes, transferred byte counts and sizes; and
/// - `Cancel` an arbitrary stream id, which drops the pending entry
///   and fails the *owner's* fetch.
///
/// Cancellation is the sharper end — repeatedly cancelling makes blob
/// and directory retrieval fail on a node the caller does not own —
/// but `List` is not obviously the safer half: it is a map of what
/// this node is fetching and from whom.
///
/// # Read and write are not separately configurable
///
/// Deliberately. Splitting them would invite "reads are harmless, open
/// those up", and the disclosure above is the reason that reading
/// isn't harmless. An operator who genuinely wants public visibility
/// can say so with [`AnyAdmittedPeer`](Self::AnyAdmittedPeer).
///
/// # What the decision is made on
///
/// [`RpcContext::session_peer`] — the AEAD-authenticated node that
/// delivered the request. `caller_origin` is unusable here: it is
/// routing metadata the sender chooses, and this crate's own docs say
/// not to authorize on it. Note the limit `session_peer` carries: it
/// is the last hop, so under nRPC relaying an allowlist authorizes the
/// relay and whatever it forwards.
#[derive(Debug, Clone, Default)]
pub enum TransferAdminPolicy {
    /// Refuse every remote request. The default: a node that has not
    /// named its operators has no way to tell one from an attacker.
    ///
    /// The service still answers — with
    /// [`TransferRpcError::Unauthorized`] — so a misconfigured
    /// operator gets a diagnosable reply rather than a timeout.
    #[default]
    Closed,
    /// Only the named nodes may list, inspect, or cancel.
    ///
    /// An empty set is equivalent to [`Closed`](Self::Closed); the
    /// unconfigured case never widens access.
    Operators(Arc<HashSet<u64>>),
    /// Any peer that completed the mesh handshake, i.e. the pre-0.35
    /// behaviour that this type exists to close. Defensible only where
    /// mesh admission *is* the operator boundary.
    AnyAdmittedPeer,
}

impl TransferAdminPolicy {
    /// Build an [`Operators`](Self::Operators) set from anything iterable.
    pub fn operators<I: IntoIterator<Item = u64>>(nodes: I) -> Self {
        Self::Operators(Arc::new(nodes.into_iter().collect()))
    }

    /// Whether `session_peer` may administer this node's transfers.
    pub fn permits(&self, session_peer: u64) -> bool {
        match self {
            Self::Closed => false,
            Self::Operators(nodes) => nodes.contains(&session_peer),
            Self::AnyAdmittedPeer => true,
        }
    }
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
    policy: TransferAdminPolicy,
}

impl TransferRpcHandler {
    /// Build a handler over `engine` that refuses every remote
    /// request — [`TransferAdminPolicy::Closed`].
    ///
    /// Use [`Self::with_policy`] to name the operator nodes that may
    /// administer this node's transfers.
    pub fn new(engine: Arc<BlobTransferEngine>) -> Self {
        Self::with_policy(engine, TransferAdminPolicy::default())
    }

    /// Build a handler over `engine` with an explicit authority policy.
    pub fn with_policy(engine: Arc<BlobTransferEngine>, policy: TransferAdminPolicy) -> Self {
        Self { engine, policy }
    }
}

#[async_trait]
impl RpcHandler for TransferRpcHandler {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        // Authorize before decoding. The body is attacker-controlled
        // and decoding it for an unauthorized caller buys nothing but
        // a wider surface; it would also let the refusal reply differ
        // between a malformed and a well-formed body, which is a free
        // oracle for probing the wire format.
        if !self.policy.permits(ctx.session_peer) {
            tracing::warn!(
                session_peer = format!("{:#x}", ctx.session_peer),
                caller_origin = format!("{:#x}", ctx.caller_origin),
                policy = ?self.policy,
                "refused an unauthorized blob.transfers request",
            );
            return Ok(encode_response(&TransferRpcResponse::Error(
                TransferRpcError::Unauthorized,
            )));
        }
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

    /// Build a node + an `Arc`-wrapped engine over it, for exercising
    /// `answer` and the handler against real engine state. Mirrors the
    /// `transfer.rs` engine-accessor test setup.
    async fn build_engine() -> (Arc<MeshNode>, Arc<BlobTransferEngine>) {
        use crate::adapter::net::dataforts::blob::MeshBlobAdapter;
        use crate::adapter::net::identity::EntityKeypair;
        use crate::adapter::net::redex::Redex;
        use crate::adapter::net::MeshNodeConfig;

        let addr = "127.0.0.1:0".parse().expect("addr");
        let node = Arc::new(
            MeshNode::new(
                EntityKeypair::generate(),
                MeshNodeConfig::new(addr, [0x17u8; 32]),
            )
            .await
            .expect("node"),
        );
        let adapter = Arc::new(MeshBlobAdapter::new("rpc-test", Arc::new(Redex::new())));
        let engine = Arc::new(BlobTransferEngine::new(&node, adapter));
        (node, engine)
    }

    /// `answer` is the pure request → response logic; it must reflect the
    /// engine's live state for each verb (empty, populated, post-cancel).
    #[tokio::test]
    async fn answer_reflects_engine_state_for_each_verb() {
        let (_node, engine) = build_engine().await;
        let sid = super::super::transfer_stream_id(99);

        // Empty registry.
        assert_eq!(
            answer(&engine, &TransferRpcRequest::List),
            TransferRpcResponse::Transfers(vec![])
        );
        assert_eq!(
            answer(&engine, &TransferRpcRequest::Get { stream_id: sid }),
            TransferRpcResponse::Transfer(None)
        );

        // Register a pending transfer; List + Get now surface it.
        let (tx, _rx) = tokio::sync::oneshot::channel();
        engine.register_pending(sid, 7, [0xABu8; 32], tx);
        match answer(&engine, &TransferRpcRequest::List) {
            TransferRpcResponse::Transfers(v) => {
                assert_eq!(v.len(), 1);
                assert_eq!(v[0].stream_id, sid);
                assert_eq!(v[0].holder, 7);
                assert_eq!(v[0].expected_hash, [0xABu8; 32]);
            }
            other => panic!("expected Transfers, got {other:?}"),
        }
        match answer(&engine, &TransferRpcRequest::Get { stream_id: sid }) {
            TransferRpcResponse::Transfer(Some(s)) => assert_eq!(s.holder, 7),
            other => panic!("expected Transfer(Some), got {other:?}"),
        }

        // Cancel reports existence once, then false; the registry empties.
        assert_eq!(
            answer(&engine, &TransferRpcRequest::Cancel { stream_id: sid }),
            TransferRpcResponse::Cancelled { existed: true }
        );
        assert_eq!(
            answer(&engine, &TransferRpcRequest::Cancel { stream_id: sid }),
            TransferRpcResponse::Cancelled { existed: false }
        );
        assert_eq!(
            answer(&engine, &TransferRpcRequest::List),
            TransferRpcResponse::Transfers(vec![])
        );
    }

    /// The `RpcHandler` decodes the request, answers it, and encodes the
    /// reply — and an undecodable body must come back as a `DecodeFailed`
    /// application error inside an `Ok` envelope (never an `Err`, so the
    /// transport stays healthy).
    #[tokio::test]
    async fn handler_answers_valid_request_and_wraps_bad_body() {
        use crate::adapter::net::cortex::rpc::{RpcCancellationToken, RpcRequestPayload};

        let (_node, engine) = build_engine().await;
        // Named operator: this test is about decode behaviour, not
        // authority, so the caller has to get past the gate. The
        // authority cases are covered below.
        let handler =
            TransferRpcHandler::with_policy(engine, TransferAdminPolicy::operators([OPERATOR]));

        let make_ctx = |body: Vec<u8>| RpcContext {
            caller_origin: 1,
            session_peer: OPERATOR,
            call_id: 2,
            payload: RpcRequestPayload {
                service: TRANSFER_SERVICE.to_string(),
                deadline_ns: 0,
                flags: 0,
                headers: Vec::new(),
                body: Bytes::from(body),
            },
            cancellation: RpcCancellationToken::new(),
            trace_context: None,
            org_admission: None,
        };

        // Valid List → Ok status, body decodes to an (empty) Transfers reply.
        let body = postcard::to_allocvec(&TransferRpcRequest::List).expect("encode req");
        let resp = handler.call(make_ctx(body)).await.expect("handler ok");
        assert_eq!(resp.status, RpcStatus::Ok);
        match postcard::from_bytes::<TransferRpcResponse>(&resp.body).expect("decode resp") {
            TransferRpcResponse::Transfers(v) => assert!(v.is_empty()),
            other => panic!("expected Transfers, got {other:?}"),
        }

        // Undecodable body (a discriminant past the last variant) → Ok
        // envelope carrying DecodeFailed.
        let resp = handler
            .call(make_ctx(vec![0x7F]))
            .await
            .expect("handler ok");
        assert_eq!(resp.status, RpcStatus::Ok);
        match postcard::from_bytes::<TransferRpcResponse>(&resp.body).expect("decode resp") {
            TransferRpcResponse::Error(TransferRpcError::DecodeFailed(_)) => {}
            other => panic!("expected Error(DecodeFailed), got {other:?}"),
        }
    }

    /// Node id used as the legitimate operator across the authority
    /// tests below.
    const OPERATOR: u64 = 0x0FE8;
    /// An admitted mesh peer that is not an operator.
    const OUTSIDER: u64 = 0xBAD5;

    /// SEC-03 / AUTH-05 RED witness. An admitted peer that is not an
    /// operator must learn nothing about this node's transfers and
    /// must not be able to cancel one.
    ///
    /// The cancel leg is the one that matters most: pre-fix it
    /// removed the pending entry and failed the *owner's* fetch, so a
    /// peer with no relationship to the transfer could break blob and
    /// directory retrieval on another node at will.
    #[tokio::test]
    async fn unauthorized_peer_can_neither_inspect_nor_cancel_transfers() {
        use crate::adapter::net::cortex::rpc::{RpcCancellationToken, RpcRequestPayload};

        let (_node, engine) = build_engine().await;
        // One live transfer to disclose or destroy.
        let sid = super::super::transfer_stream_id(99);
        let (tx, _rx) = tokio::sync::oneshot::channel();
        engine.register_pending(sid, 7, [0xABu8; 32], tx);
        let handler = TransferRpcHandler::with_policy(
            engine.clone(),
            TransferAdminPolicy::operators([OPERATOR]),
        );

        let call = |req: TransferRpcRequest, peer: u64| {
            let handler = &handler;
            async move {
                let ctx = RpcContext {
                    caller_origin: 1,
                    session_peer: peer,
                    call_id: 2,
                    payload: RpcRequestPayload {
                        service: TRANSFER_SERVICE.to_string(),
                        deadline_ns: 0,
                        flags: 0,
                        headers: Vec::new(),
                        body: Bytes::from(postcard::to_allocvec(&req).expect("encode")),
                    },
                    cancellation: RpcCancellationToken::new(),
                    trace_context: None,
                    org_admission: None,
                };
                let resp = handler.call(ctx).await.expect("handler ok");
                postcard::from_bytes::<TransferRpcResponse>(&resp.body).expect("decode resp")
            }
        };

        for req in [
            TransferRpcRequest::List,
            TransferRpcRequest::Get { stream_id: sid },
            TransferRpcRequest::Cancel { stream_id: sid },
        ] {
            let label = format!("{req:?}");
            match call(req, OUTSIDER).await {
                TransferRpcResponse::Error(TransferRpcError::Unauthorized) => {}
                other => panic!("{label} from a non-operator was answered with {other:?}"),
            }
        }

        // The transfer survived the refused Cancel. Asserting on the
        // reply alone would miss a gate that refused *after* mutating.
        match answer(&engine, &TransferRpcRequest::Get { stream_id: sid }) {
            TransferRpcResponse::Transfer(Some(s)) => assert_eq!(s.holder, 7),
            other => panic!("refused Cancel still dropped the pending transfer: {other:?}"),
        }

        // Positive control: the operator can still do all three, and
        // the Cancel actually takes effect.
        match call(TransferRpcRequest::List, OPERATOR).await {
            TransferRpcResponse::Transfers(v) => assert_eq!(v.len(), 1),
            other => panic!("operator List refused: {other:?}"),
        }
        match call(TransferRpcRequest::Cancel { stream_id: sid }, OPERATOR).await {
            TransferRpcResponse::Cancelled { existed: true } => {}
            other => panic!("operator Cancel refused: {other:?}"),
        }
        match answer(&engine, &TransferRpcRequest::Get { stream_id: sid }) {
            TransferRpcResponse::Transfer(None) => {}
            other => panic!("operator Cancel did not take effect: {other:?}"),
        }
    }

    /// The policy's truth table. `Closed` is the default, and an empty
    /// operator set must not read as "everyone".
    #[test]
    fn transfer_admin_policy_permits_only_what_it_names() {
        assert!(!TransferAdminPolicy::Closed.permits(OPERATOR));
        assert!(!TransferAdminPolicy::default().permits(OPERATOR));

        let named = TransferAdminPolicy::operators([OPERATOR]);
        assert!(named.permits(OPERATOR));
        assert!(!named.permits(OUTSIDER));

        // The unconfigured-list case: writing the config key but
        // leaving it empty must not widen access.
        assert!(!TransferAdminPolicy::operators([]).permits(OPERATOR));

        assert!(TransferAdminPolicy::AnyAdmittedPeer.permits(OUTSIDER));
    }

    /// `TransferClientError` must preserve the failure category across the
    /// `From` conversions the client relies on (`?` from `typed_call`).
    #[test]
    fn client_error_conversions_preserve_category() {
        let e: TransferClientError = RpcError::Timeout { elapsed_ms: 5 }.into();
        assert!(matches!(e, TransferClientError::Transport(_)));

        let e: TransferClientError =
            TypedCallError::Transport(RpcError::Timeout { elapsed_ms: 1 }).into();
        assert!(matches!(e, TransferClientError::Transport(_)));

        let e: TransferClientError = TypedCallError::Codec("boom".into()).into();
        assert!(matches!(e, TransferClientError::Codec(m) if m == "boom"));
    }

    /// `new` adopts the default deadline; `with_deadline` overrides it.
    #[tokio::test]
    async fn client_new_defaults_deadline_and_with_deadline_overrides() {
        let (node, _engine) = build_engine().await;
        let client = BlobTransferClient::new(node);
        assert_eq!(client.deadline, DEFAULT_TRANSFER_DEADLINE);

        let custom = Duration::from_millis(250);
        let client = client.with_deadline(custom);
        assert_eq!(client.deadline, custom);
    }
}
