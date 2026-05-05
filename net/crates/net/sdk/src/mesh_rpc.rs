//! nRPC SDK surface — typed `serve_rpc_typed` / `call_typed` over
//! the underlying `MeshNode::serve_rpc` / `call` raw-bytes API.
//!
//! See `docs/misc/NRPC_DESIGN.md` for the architectural framing.
//! This module is the user-facing wrapper that:
//!
//! - Hides the `Bytes`-in / `Bytes`-out shape behind serde
//!   codecs (JSON by default; the codec is per-call selectable
//!   via [`Codec`]).
//! - Provides typed handlers — `Fn(Req) -> Future<Output =
//!   Result<Resp, _>>` instead of the trait-based
//!   [`net::adapter::net::cortex::RpcHandler`].
//! - Re-exports the supporting types so users don't have to dig
//!   through `net::adapter::net::*` paths.
//!
//! Raw `Bytes`-typed APIs are also exposed (`serve_rpc`, `call`,
//! `call_service`) for users who manage their own serialization
//! (e.g. protobuf via prost, postcard, or hand-rolled formats).

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use serde::{de::DeserializeOwned, Serialize};

pub use net::adapter::net::cortex::{
    RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcResponseSink, RpcStatus,
    RpcStreamingHandler, StreamItem,
};
pub use net::adapter::net::mesh_rpc::{
    CallOptions, CodecDirection, RoutingPolicy, RpcError, RpcReply, RpcStream, ServeError,
    ServeHandle,
};
pub use net::adapter::net::mesh_rpc_metrics::{
    RpcMetricsSnapshot, ServiceMetrics, DEFAULT_LATENCY_BUCKETS_SECS,
};

use crate::error::{Result, SdkError};
use crate::mesh::Mesh;

// ============================================================================
// Codec selection.
// ============================================================================

/// Application-payload encoding for typed RPC. Per-call selectable
/// via [`CallOptionsTyped::codec`]; per-handler via
/// [`Mesh::serve_rpc_typed`]'s closure choice. Caller and server
/// must agree on the codec out of band.
#[derive(Debug, Clone, Copy, Default)]
pub enum Codec {
    /// `serde_json`. The default — human-readable, ubiquitous,
    /// works across every binding language.
    #[default]
    Json,
    /// `serde_json::to_vec_pretty`. Same wire format as `Json`,
    /// just emitted with indentation. Useful for debugging /
    /// human inspection of recorded RPC traffic.
    JsonPretty,
}

impl Codec {
    /// Encode a value to bytes.
    pub fn encode<T: Serialize>(self, value: &T) -> Result<Vec<u8>> {
        let bytes = match self {
            Codec::Json => serde_json::to_vec(value),
            Codec::JsonPretty => serde_json::to_vec_pretty(value),
        };
        bytes.map_err(|e| SdkError::Config(format!("rpc codec encode: {e}")))
    }
    /// Decode bytes into a value.
    pub fn decode<T: DeserializeOwned>(self, bytes: &[u8]) -> Result<T> {
        match self {
            Codec::Json | Codec::JsonPretty => serde_json::from_slice(bytes)
                .map_err(|e| SdkError::Config(format!("rpc codec decode: {e}"))),
        }
    }
}

/// Options for the typed-call APIs ([`Mesh::call_typed`],
/// [`Mesh::call_service_typed`]). Wraps [`CallOptions`] plus the
/// per-call [`Codec`].
#[derive(Debug, Clone, Default)]
pub struct CallOptionsTyped {
    /// Underlying `CallOptions` (deadline, routing policy, etc.).
    pub raw: CallOptions,
    /// Codec used to (en/de)code request and response bodies.
    pub codec: Codec,
}

// ============================================================================
// Mesh SDK extensions — raw + typed nRPC surface.
// ============================================================================

impl Mesh {
    // ---- Raw (Bytes-in / Bytes-out) ----

    /// Register a raw-bytes RPC handler on `service`. The user
    /// handler receives the request body as `Bytes` and returns
    /// the response body as `Bytes`. Wire codec is the user's
    /// concern.
    ///
    /// **Auto-registers two `ChannelConfig` entries** so the
    /// per-caller subscribe + per-call publish work under the
    /// SDK's default `ChannelConfigRegistry` (which fail-closes
    /// on unknown channels):
    ///
    ///   1. Exact-match `<service>.requests` — the channel
    ///      callers publish REQUESTs onto.
    ///   2. Prefix-match `<service>.replies.` — admits every
    ///      `<service>.replies.<caller_origin>` subscribe that
    ///      arrives, no per-caller pre-registration needed.
    ///
    /// Both entries default to permissive (no `publish_caps`,
    /// no `require_token`) — channel-level ACLs on RPC traffic
    /// are a Phase 3 concern (alongside the per-service token
    /// allowlist). Operators who need RPC ACLs today can call
    /// `register_channel` / `register_channel_prefix` themselves
    /// before `serve_rpc` to override.
    ///
    /// For typed handlers (auto serde), use
    /// [`Self::serve_rpc_typed`].
    pub fn serve_rpc<H: RpcHandler>(
        &self,
        service: &str,
        handler: Arc<H>,
    ) -> std::result::Result<ServeHandle, ServeError> {
        self.auto_register_rpc_channels(service);
        self.node().serve_rpc(service, handler)
    }

    /// Internal helper used by `serve_rpc` / `serve_rpc_typed` to
    /// auto-register the request channel + reply prefix in the
    /// SDK's `ChannelConfigRegistry`. Idempotent — repeated calls
    /// for the same service are no-ops (DashMap insert overwrites
    /// with the same default permissive config).
    fn auto_register_rpc_channels(&self, service: &str) {
        use crate::ChannelConfig;
        use net::adapter::net::channel::{ChannelId, ChannelName};
        // Exact: `<service>.requests`.
        let req_name = format!("{service}.requests");
        if let Ok(req_channel) = ChannelName::new(&req_name) {
            self.register_channel(ChannelConfig::new(ChannelId::new(req_channel)));
        }
        // Prefix: `<service>.replies.` — admits every per-caller
        // `<service>.replies.<caller_origin>` subscribe.
        let prefix = format!("{service}.replies.");
        // Sentinel ChannelId for the prefix entry; not used for
        // hash lookups, just carried so the ChannelConfig is
        // structurally well-formed.
        if let Ok(sentinel_name) = ChannelName::new(&format!("{service}.replies.prefix")) {
            self.channel_configs_arc()
                .insert_prefix(prefix, ChannelConfig::new(ChannelId::new(sentinel_name)));
        }
    }

    /// Direct-addressed call. Caller specifies `target_node_id`;
    /// the SDK does NOT consult the capability index.
    pub async fn call(
        &self,
        target_node_id: u64,
        service: &str,
        payload: Bytes,
        opts: CallOptions,
    ) -> std::result::Result<RpcReply, RpcError> {
        self.node()
            .call(target_node_id, service, payload, opts)
            .await
    }

    /// Service-name call. Consults the capability index for nodes
    /// advertising `nrpc:<service>`, picks one per
    /// `opts.routing_policy`, calls.
    pub async fn call_service(
        &self,
        service: &str,
        payload: Bytes,
        opts: CallOptions,
    ) -> std::result::Result<RpcReply, RpcError> {
        self.node().call_service(service, payload, opts).await
    }

    /// All node ids currently advertising `nrpc:<service>` in the
    /// local capability index. Useful for diagnostics + custom
    /// caller-side routing logic.
    pub fn find_service_nodes(&self, service: &str) -> Vec<u64> {
        self.node().find_service_nodes(service)
    }

    /// Snapshot of caller-side nRPC metrics for this Mesh. Cheap
    /// (one DashMap iteration); call on every Prometheus scrape.
    /// Use [`RpcMetricsSnapshot::prometheus_text`] to format as
    /// `text/plain; version=0.0.4` for a `/metrics` endpoint.
    pub fn rpc_metrics_snapshot(&self) -> RpcMetricsSnapshot {
        self.node().rpc_metrics_snapshot()
    }

    // ---- Typed (serde) ----

    /// Register a typed RPC handler on `service`. The handler
    /// receives a deserialized `Req` and returns either an `Ok(Resp)`
    /// (encoded as the response body) or an `Err(message)`
    /// (surfaced as `RpcStatus::Internal` with the message as the
    /// body).
    ///
    /// Codec is the [`Codec`] passed to the handler factory; the
    /// same codec must be used by the caller.
    pub fn serve_rpc_typed<Req, Resp, F, Fut>(
        &self,
        service: &str,
        codec: Codec,
        handler: F,
    ) -> std::result::Result<ServeHandle, ServeError>
    where
        Req: DeserializeOwned + Send + Sync + 'static,
        Resp: Serialize + Send + Sync + 'static,
        F: Fn(Req) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = std::result::Result<Resp, String>> + Send + 'static,
    {
        let typed = TypedRpcHandler {
            codec,
            inner: Arc::new(handler),
            _req: std::marker::PhantomData::<Req>,
            _resp: std::marker::PhantomData::<Resp>,
        };
        self.auto_register_rpc_channels(service);
        self.node().serve_rpc(service, Arc::new(typed))
    }

    /// Direct-addressed typed call. Encodes `request` via
    /// `opts.codec`, calls the underlying raw `call`, decodes the
    /// reply body into `Resp`.
    pub async fn call_typed<Req, Resp>(
        &self,
        target_node_id: u64,
        service: &str,
        request: &Req,
        opts: CallOptionsTyped,
    ) -> std::result::Result<Resp, RpcError>
    where
        Req: Serialize,
        Resp: DeserializeOwned,
    {
        let body = opts.codec.encode(request).map_err(|e| RpcError::Codec {
            direction: CodecDirection::Encode,
            message: format!("client encode: {e}"),
        })?;
        let reply = self
            .call(target_node_id, service, Bytes::from(body), opts.raw)
            .await?;
        opts.codec.decode(&reply.body).map_err(|e| RpcError::Codec {
            direction: CodecDirection::Decode,
            message: format!("client decode: {e}"),
        })
    }

    /// Service-name typed call. Same as [`Self::call_typed`] but
    /// uses the capability index to pick the target.
    pub async fn call_service_typed<Req, Resp>(
        &self,
        service: &str,
        request: &Req,
        opts: CallOptionsTyped,
    ) -> std::result::Result<Resp, RpcError>
    where
        Req: Serialize,
        Resp: DeserializeOwned,
    {
        let body = opts.codec.encode(request).map_err(|e| RpcError::Codec {
            direction: CodecDirection::Encode,
            message: format!("client encode: {e}"),
        })?;
        let reply = self
            .call_service(service, Bytes::from(body), opts.raw)
            .await?;
        opts.codec.decode(&reply.body).map_err(|e| RpcError::Codec {
            direction: CodecDirection::Decode,
            message: format!("client decode: {e}"),
        })
    }

    // ---- Streaming (raw) ----

    /// Register a raw-bytes streaming RPC handler on `service`. The
    /// handler receives the request body plus an [`RpcResponseSink`]
    /// it writes raw chunks to via `sink.send(body)`. Wire codec is
    /// the user's concern.
    ///
    /// Same auto-registration as [`Self::serve_rpc`] (request channel
    /// plus reply prefix). For typed handlers (auto serde), use
    /// [`Self::serve_rpc_streaming_typed`] instead.
    pub fn serve_rpc_streaming<H: RpcStreamingHandler>(
        &self,
        service: &str,
        handler: Arc<H>,
    ) -> std::result::Result<ServeHandle, ServeError> {
        self.auto_register_rpc_channels(service);
        self.node().serve_rpc_streaming(service, handler)
    }

    /// Direct-addressed streaming call. Returns an [`RpcStream`] that
    /// yields raw chunks as `Result<Bytes, RpcError>`. Dropping the
    /// stream emits CANCEL to the server.
    pub async fn call_streaming(
        &self,
        target_node_id: u64,
        service: &str,
        payload: Bytes,
        opts: CallOptions,
    ) -> std::result::Result<RpcStream, RpcError> {
        self.node()
            .call_streaming(target_node_id, service, payload, opts)
            .await
    }

    // ---- Streaming (typed) ----

    /// Register a typed streaming RPC handler. The handler receives
    /// a deserialized `Req` plus a [`ResponseSinkTyped<Resp>`] that
    /// auto-encodes each `send(&value)` per the codec. Returning
    /// `Ok(())` closes the stream cleanly; `Err(message)` closes it
    /// with `RpcStatus::Application(0x4001)` and the message in the
    /// terminal frame's body.
    pub fn serve_rpc_streaming_typed<Req, Resp, F, Fut>(
        &self,
        service: &str,
        codec: Codec,
        handler: F,
    ) -> std::result::Result<ServeHandle, ServeError>
    where
        Req: DeserializeOwned + Send + Sync + 'static,
        Resp: Serialize + Send + Sync + 'static,
        F: Fn(Req, ResponseSinkTyped<Resp>) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = std::result::Result<(), String>> + Send + 'static,
    {
        let typed = TypedStreamingRpcHandler {
            codec,
            inner: Arc::new(handler),
            _req: std::marker::PhantomData::<Req>,
            _resp: std::marker::PhantomData::<Resp>,
        };
        self.auto_register_rpc_channels(service);
        self.node().serve_rpc_streaming(service, Arc::new(typed))
    }

    /// Direct-addressed typed streaming call. Encodes `request` via
    /// `opts.codec`, opens the streaming call, returns an
    /// [`RpcStreamTyped<Resp>`] that decodes each chunk on the fly.
    /// Decode failures terminate the stream with a single
    /// `RpcError::ServerError(Internal)` carrying the decode
    /// diagnostic.
    pub async fn call_streaming_typed<Req, Resp>(
        &self,
        target_node_id: u64,
        service: &str,
        request: &Req,
        opts: CallOptionsTyped,
    ) -> std::result::Result<RpcStreamTyped<Resp>, RpcError>
    where
        Req: Serialize,
        Resp: DeserializeOwned,
    {
        let body = opts.codec.encode(request).map_err(|e| RpcError::Codec {
            direction: CodecDirection::Encode,
            message: format!("client encode: {e}"),
        })?;
        let inner = self
            .call_streaming(target_node_id, service, Bytes::from(body), opts.raw)
            .await?;
        Ok(RpcStreamTyped {
            inner,
            codec: opts.codec,
            done: false,
            _resp: std::marker::PhantomData,
        })
    }
}

// ============================================================================
// Typed streaming sink + stream wrappers.
// ============================================================================

/// Typed counterpart of [`RpcResponseSink`]. Each `send(&value)`
/// encodes via the codec captured at handler registration, then
/// hands the bytes to the underlying raw sink.
///
/// Encode failures are surfaced as a `String` `Err` so the handler
/// can decide whether to abort the stream (return `Err`) or
/// continue. The raw sink itself never blocks and never errors
/// from a back-pressure standpoint — it discards if the caller has
/// already dropped the stream.
pub struct ResponseSinkTyped<Resp> {
    inner: RpcResponseSink,
    codec: Codec,
    _resp: std::marker::PhantomData<fn(Resp)>,
}

impl<Resp: Serialize> ResponseSinkTyped<Resp> {
    /// Encode `value` with the captured codec and emit it as one
    /// non-terminal chunk. Returns `Err(message)` if encoding fails;
    /// the chunk is NOT sent in that case.
    pub fn send(&self, value: &Resp) -> std::result::Result<(), String> {
        let bytes = self
            .codec
            .encode(value)
            .map_err(|e| format!("typed streaming sink encode: {e}"))?;
        self.inner.send(bytes);
        Ok(())
    }
}

/// Typed counterpart of [`RpcStream`]. Auto-decodes each chunk to
/// `Resp` per the codec captured at call time. Implements
/// `futures::Stream<Item = Result<Resp, RpcError>>`.
///
/// **Decode failure terminates the stream** — once a chunk fails to
/// decode, the next poll yields the decode-error `Err` and
/// subsequent polls return `None`. The underlying [`RpcStream`]'s
/// CANCEL-on-Drop semantics still apply.
pub struct RpcStreamTyped<Resp> {
    inner: RpcStream,
    codec: Codec,
    done: bool,
    _resp: std::marker::PhantomData<fn() -> Resp>,
}

impl<Resp> RpcStreamTyped<Resp> {
    /// Server-assigned `call_id` of the underlying stream — useful
    /// for trace correlation / custom logging.
    pub fn call_id(&self) -> u64 {
        self.inner.call_id()
    }
}

impl<Resp: DeserializeOwned + Unpin> futures::Stream for RpcStreamTyped<Resp> {
    type Item = std::result::Result<Resp, RpcError>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        if self.done {
            return std::task::Poll::Ready(None);
        }
        let codec = self.codec;
        match std::pin::Pin::new(&mut self.inner).poll_next(cx) {
            std::task::Poll::Ready(Some(Ok(bytes))) => match codec.decode::<Resp>(&bytes) {
                Ok(value) => std::task::Poll::Ready(Some(Ok(value))),
                Err(e) => {
                    self.done = true;
                    std::task::Poll::Ready(Some(Err(RpcError::Codec {
                        direction: CodecDirection::Decode,
                        message: format!("client decode: {e}"),
                    })))
                }
            },
            std::task::Poll::Ready(Some(Err(e))) => {
                self.done = true;
                std::task::Poll::Ready(Some(Err(e)))
            }
            std::task::Poll::Ready(None) => {
                self.done = true;
                std::task::Poll::Ready(None)
            }
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

// ============================================================================
// Internal: typed-handler adapter.
//
// Bridges the user's typed `Fn(Req) -> Future<Result<Resp, _>>`
// closure to the raw `RpcHandler` trait the underlying mesh layer
// expects.
// ============================================================================

struct TypedRpcHandler<Req, Resp, F> {
    codec: Codec,
    inner: Arc<F>,
    _req: std::marker::PhantomData<Req>,
    _resp: std::marker::PhantomData<Resp>,
}

#[async_trait]
impl<Req, Resp, F, Fut> RpcHandler for TypedRpcHandler<Req, Resp, F>
where
    Req: DeserializeOwned + Send + Sync + 'static,
    Resp: Serialize + Send + Sync + 'static,
    F: Fn(Req) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = std::result::Result<Resp, String>> + Send + 'static,
{
    async fn call(
        &self,
        ctx: RpcContext,
    ) -> std::result::Result<RpcResponsePayload, RpcHandlerError> {
        // Decode the request body. A bad body is a caller error
        // — surface as `Application(0x4000)` with the decode
        // diagnostic so the caller can distinguish "I sent
        // nonsense" from a server-internal failure.
        let req: Req = match self.codec.decode(&ctx.payload.body) {
            Ok(r) => r,
            Err(e) => {
                return Err(RpcHandlerError::Application {
                    code: 0x4000,
                    message: format!("typed handler: bad request body: {e}"),
                })
            }
        };
        // Run the user's closure.
        let resp = (self.inner)(req)
            .await
            .map_err(|message| RpcHandlerError::Application {
                code: 0x4001,
                message,
            })?;
        // Encode the response body.
        let body = self
            .codec
            .encode(&resp)
            .map_err(|e| RpcHandlerError::Internal(format!("typed handler encode: {e}")))?;
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body,
        })
    }
}

// ============================================================================
// Internal: typed streaming-handler adapter.
//
// Bridges `Fn(Req, ResponseSinkTyped<Resp>) -> Future<Result<(),
// String>>` to the raw `RpcStreamingHandler` trait.
// ============================================================================

struct TypedStreamingRpcHandler<Req, Resp, F> {
    codec: Codec,
    inner: Arc<F>,
    _req: std::marker::PhantomData<Req>,
    _resp: std::marker::PhantomData<Resp>,
}

#[async_trait]
impl<Req, Resp, F, Fut> RpcStreamingHandler for TypedStreamingRpcHandler<Req, Resp, F>
where
    Req: DeserializeOwned + Send + Sync + 'static,
    Resp: Serialize + Send + Sync + 'static,
    F: Fn(Req, ResponseSinkTyped<Resp>) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = std::result::Result<(), String>> + Send + 'static,
{
    async fn call(
        &self,
        ctx: RpcContext,
        sink: RpcResponseSink,
    ) -> std::result::Result<(), RpcHandlerError> {
        let req: Req = match self.codec.decode(&ctx.payload.body) {
            Ok(r) => r,
            Err(e) => {
                return Err(RpcHandlerError::Application {
                    code: 0x4000,
                    message: format!("typed streaming handler: bad request body: {e}"),
                })
            }
        };
        let typed_sink = ResponseSinkTyped {
            inner: sink,
            codec: self.codec,
            _resp: std::marker::PhantomData,
        };
        (self.inner)(req, typed_sink)
            .await
            .map_err(|message| RpcHandlerError::Application {
                code: 0x4001,
                message,
            })
    }
}

// `Mesh::node()` is a private accessor on `crate::mesh::Mesh` that
// returns the underlying `Arc<MeshNode>`. Add it (or expose the
// existing field) as a small `pub(crate)` shim if it isn't there
// yet.
//
// The `crate::mesh::Mesh` type holds `node: Arc<MeshNode>` (private).
// We expose a `pub(crate) fn node(&self) -> &Arc<MeshNode>` accessor
// on `Mesh` in the same commit so this module can delegate.
