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
    RequestStream, RpcCallEvent, RpcCallStatus, RpcClientStreamingHandler, RpcContext,
    RpcDirection, RpcDuplexHandler, RpcHandler, RpcHandlerError, RpcObserver, RpcObserverHandle,
    RpcResponsePayload, RpcResponseSink, RpcStatus, RpcStreamingContext, RpcStreamingHandler,
    StreamItem,
};
pub use net::adapter::net::mesh_rpc::{
    CallOptions, ClientStreamCallRaw, CodecDirection, DuplexCallRaw, DuplexSink, DuplexStream,
    RoutingPolicy, RpcError, RpcReply, RpcStream, ServeError, ServeHandle,
};
pub use net::adapter::net::mesh_rpc_metrics::{
    RpcMetricsSnapshot, ServiceMetrics, DEFAULT_LATENCY_BUCKETS_SECS,
};

use crate::error::{Result, SdkError};
use crate::mesh::Mesh;

// ============================================================================
// Application-status code reservations for the typed wrappers.
//
// These sit in the application-defined band (0x8000..=0xFFFF) per
// the wire-format spec — callers can pattern-match on them via
// `RpcError::ServerError { status, .. }` to distinguish a typed-
// handler reject from an arbitrary application error.
//
// Pre-fix the typed wrappers used 0x4000 / 0x4001, which sit in
// the reserved-for-future-canonical-status band (0x0008..=0x7FFF).
// Moved to the application range so a future canonical status can
// safely take 0x4000+ without colliding with the typed-wrapper
// SDK contract.
// ============================================================================

/// Surfaced when the typed handler's `Codec::decode(request_body)`
/// fails — the request reached the server but its body couldn't
/// be deserialized into the handler's `Req` type. The caller's
/// typed `RpcError::ServerError` carries this status and a UTF-8
/// diagnostic in the `message` field.
pub const NRPC_TYPED_BAD_REQUEST: u16 = 0x8000;

/// Surfaced when the typed handler's user closure returns
/// `Err(String)`. The string is round-tripped as the
/// `RpcError::ServerError::message`. Distinguishable from
/// `NRPC_TYPED_BAD_REQUEST` so callers can route validation
/// errors vs. handler errors to different fall-back paths.
pub const NRPC_TYPED_HANDLER_ERROR: u16 = 0x8001;

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
// Phase 9b — predicate-pushdown convenience.
//
// `with_where(p)` encodes a `Predicate` to JSON via the substrate's
// `predicate_to_rpc_header` and pushes it into `CallOptions::request_headers`
// under the `net-where` header name. Servers that opt in read
// it back via `RpcContextExt::where_predicate()`.
// ============================================================================

/// Extension methods for [`CallOptions`] adding caller-side
/// predicate-pushdown helpers (Phase 9b of
/// `CAPABILITY_SYSTEM_SDK_PLAN.md`).
pub trait CallOptionsExt: Sized {
    /// Append a raw `(name, value_bytes)` request header. Names
    /// follow the lowercase `cyberdeck-*` / `nrpc-*` convention.
    fn with_request_header(self, name: impl Into<String>, value: impl Into<Vec<u8>>) -> Self;

    /// Attach a [`net::adapter::net::behavior::Predicate`] as the
    /// `net-where` request header. The predicate rides as
    /// JSON-encoded `PredicateWire` bytes per the substrate's
    /// `predicate_to_rpc_header` contract;
    /// services opting into predicate-pushdown decode via
    /// [`RpcContextExt::where_predicate`].
    ///
    /// Returns `Err` if either:
    ///
    ///   - the predicate's JSON encoding fails
    ///     (`PredicateRpcEncodeError::Encode`) — should not happen
    ///     for predicates built via the `pred!` macro / `Predicate`
    ///     constructors, but is exposed defensively for forward-
    ///     compat in case a future variant carries non-finite
    ///     numerics or other serde-incompatible fields, OR
    ///   - the encoded payload exceeds
    ///     `MAX_PREDICATE_RPC_HEADER_VALUE_LEN` (currently
    ///     **4 KiB**) — `PredicateRpcEncodeError::TooLarge`.
    ///
    /// Don't blindly `.unwrap()` the result; even predicates built
    /// from typical `pred!` macro use can exceed 4 KiB once they
    /// fan out (e.g. an Or-of-many StringPrefix clauses, an
    /// And of large StringMatches patterns).
    fn with_where(
        self,
        pred: &net::adapter::net::behavior::Predicate,
    ) -> std::result::Result<Self, net::adapter::net::behavior::PredicateRpcEncodeError>;
}

impl CallOptionsExt for CallOptions {
    fn with_request_header(mut self, name: impl Into<String>, value: impl Into<Vec<u8>>) -> Self {
        self.request_headers.push((name.into(), value.into()));
        self
    }

    fn with_where(
        mut self,
        pred: &net::adapter::net::behavior::Predicate,
    ) -> std::result::Result<Self, net::adapter::net::behavior::PredicateRpcEncodeError> {
        let (name, bytes) = net::adapter::net::behavior::predicate_to_rpc_header(pred)?;
        self.request_headers.push((name, bytes));
        Ok(self)
    }
}

impl CallOptionsExt for CallOptionsTyped {
    fn with_request_header(mut self, name: impl Into<String>, value: impl Into<Vec<u8>>) -> Self {
        self.raw = self.raw.with_request_header(name, value);
        self
    }

    fn with_where(
        mut self,
        pred: &net::adapter::net::behavior::Predicate,
    ) -> std::result::Result<Self, net::adapter::net::behavior::PredicateRpcEncodeError> {
        self.raw = self.raw.with_where(pred)?;
        Ok(self)
    }
}

/// Extension methods for [`RpcContext`] adding server-side
/// predicate-pushdown helpers (Phase 9b of
/// `CAPABILITY_SYSTEM_SDK_PLAN.md`).
pub trait RpcContextExt {
    /// Decode the caller's [`net::adapter::net::behavior::Predicate`]
    /// from the `net-where` request header, if present.
    /// Returns `None` when the header is absent (the common case
    /// for callers that don't issue predicate-pushdown queries)
    /// or `Some(Err(_))` if the header is malformed.
    fn where_predicate(
        &self,
    ) -> Option<
        std::result::Result<
            net::adapter::net::behavior::Predicate,
            net::adapter::net::behavior::PredicateRpcDecodeError,
        >,
    >;
}

impl RpcContextExt for RpcContext {
    fn where_predicate(
        &self,
    ) -> Option<
        std::result::Result<
            net::adapter::net::behavior::Predicate,
            net::adapter::net::behavior::PredicateRpcDecodeError,
        >,
    > {
        net::adapter::net::behavior::predicate_from_rpc_headers(&self.payload.headers)
    }
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
    /// with `RpcStatus::Application(NRPC_TYPED_HANDLER_ERROR)` and
    /// the message in the terminal frame's body.
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

    // ---- Client-streaming (raw + typed) — Phase C / E ----

    /// Register a raw-bytes client-streaming RPC handler on
    /// `service`. The handler receives the request stream (raw
    /// chunk bodies) and returns one terminal response payload.
    /// Wire codec is the user's concern; for typed handlers use
    /// [`Self::serve_rpc_client_stream_typed`].
    pub fn serve_rpc_client_stream<H: RpcClientStreamingHandler>(
        &self,
        service: &str,
        handler: Arc<H>,
    ) -> std::result::Result<ServeHandle, ServeError> {
        self.auto_register_rpc_channels(service);
        self.node().serve_rpc_client_stream(service, handler)
    }

    /// Direct-addressed raw client-streaming call. Returns a
    /// [`ClientStreamCallRaw`] handle. Push chunks via `send`,
    /// then `finish` to await the terminal RESPONSE.
    pub async fn call_client_stream(
        &self,
        target_node_id: u64,
        service: &str,
        opts: CallOptions,
    ) -> std::result::Result<ClientStreamCallRaw, RpcError> {
        self.node()
            .call_client_stream(target_node_id, service, opts)
            .await
    }

    /// Register a typed client-streaming handler. Mirror of
    /// [`Self::serve_rpc_typed`] for the multi-request shape.
    /// Receives a [`RequestStreamTyped<Req>`] (auto-decodes each
    /// inbound chunk via `codec`), returns one terminal `Resp`
    /// (auto-encoded). `Err(String)` surfaces as
    /// `RpcError::ServerError(Application(NRPC_TYPED_HANDLER_ERROR))`.
    pub fn serve_rpc_client_stream_typed<Req, Resp, F, Fut>(
        &self,
        service: &str,
        codec: Codec,
        handler: F,
    ) -> std::result::Result<ServeHandle, ServeError>
    where
        Req: DeserializeOwned + Send + Sync + Unpin + 'static,
        Resp: Serialize + Send + Sync + 'static,
        F: Fn(RequestStreamTyped<Req>) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = std::result::Result<Resp, String>> + Send + 'static,
    {
        let typed = TypedClientStreamingRpcHandler {
            codec,
            inner: Arc::new(handler),
            _req: std::marker::PhantomData::<Req>,
            _resp: std::marker::PhantomData::<Resp>,
        };
        self.auto_register_rpc_channels(service);
        self.node()
            .serve_rpc_client_stream(service, Arc::new(typed))
    }

    /// Direct-addressed typed client-streaming call. Returns a
    /// [`ClientStreamCallTyped<Req, Resp>`] handle.
    pub async fn call_client_stream_typed<Req, Resp>(
        &self,
        target_node_id: u64,
        service: &str,
        opts: CallOptionsTyped,
    ) -> std::result::Result<ClientStreamCallTyped<Req, Resp>, RpcError>
    where
        Req: Serialize,
        Resp: DeserializeOwned,
    {
        let inner = self
            .call_client_stream(target_node_id, service, opts.raw)
            .await?;
        Ok(ClientStreamCallTyped {
            inner,
            codec: opts.codec,
            _req: std::marker::PhantomData,
            _resp: std::marker::PhantomData,
        })
    }

    // ---- Duplex (raw + typed) — Phase D / E ----

    /// Register a raw-bytes duplex RPC handler on `service`. The
    /// handler receives both a request stream AND a response sink
    /// for emitting multi-fire response chunks. Returns `Ok(())`
    /// to close cleanly; `Err(RpcHandlerError)` for failure
    /// mapping. For typed handlers use [`Self::serve_rpc_duplex_typed`].
    pub fn serve_rpc_duplex<H: RpcDuplexHandler>(
        &self,
        service: &str,
        handler: Arc<H>,
    ) -> std::result::Result<ServeHandle, ServeError> {
        self.auto_register_rpc_channels(service);
        self.node().serve_rpc_duplex(service, handler)
    }

    /// Direct-addressed raw duplex call. Returns a
    /// [`DuplexCallRaw`] handle with both send and receive
    /// surfaces. Use `into_split` to peel off the two halves.
    pub async fn call_duplex(
        &self,
        target_node_id: u64,
        service: &str,
        opts: CallOptions,
    ) -> std::result::Result<DuplexCallRaw, RpcError> {
        self.node().call_duplex(target_node_id, service, opts).await
    }

    /// Register a typed duplex handler. Receives a
    /// [`RequestStreamTyped<Req>`] (auto-decodes inbound chunks)
    /// and a [`ResponseSinkTyped<Resp>`] (auto-encodes outbound
    /// chunks). Returns `Ok(())` for clean close.
    pub fn serve_rpc_duplex_typed<Req, Resp, F, Fut>(
        &self,
        service: &str,
        codec: Codec,
        handler: F,
    ) -> std::result::Result<ServeHandle, ServeError>
    where
        Req: DeserializeOwned + Send + Sync + Unpin + 'static,
        Resp: Serialize + Send + Sync + 'static,
        F: Fn(RequestStreamTyped<Req>, ResponseSinkTyped<Resp>) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = std::result::Result<(), String>> + Send + 'static,
    {
        let typed = TypedDuplexRpcHandler {
            codec,
            inner: Arc::new(handler),
            _req: std::marker::PhantomData::<Req>,
            _resp: std::marker::PhantomData::<Resp>,
        };
        self.auto_register_rpc_channels(service);
        self.node().serve_rpc_duplex(service, Arc::new(typed))
    }

    /// Direct-addressed typed duplex call. Returns a
    /// [`DuplexCallTyped<Req, Resp>`] handle.
    pub async fn call_duplex_typed<Req, Resp>(
        &self,
        target_node_id: u64,
        service: &str,
        opts: CallOptionsTyped,
    ) -> std::result::Result<DuplexCallTyped<Req, Resp>, RpcError>
    where
        Req: Serialize,
        Resp: DeserializeOwned,
    {
        let inner = self.call_duplex(target_node_id, service, opts.raw).await?;
        Ok(DuplexCallTyped {
            inner,
            codec: opts.codec,
            done: false,
            _req: std::marker::PhantomData,
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
                    code: NRPC_TYPED_BAD_REQUEST,
                    message: format!("typed handler: bad request body: {e}"),
                })
            }
        };
        // Run the user's closure.
        let resp = (self.inner)(req)
            .await
            .map_err(|message| RpcHandlerError::Application {
                code: NRPC_TYPED_HANDLER_ERROR,
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
                    code: NRPC_TYPED_BAD_REQUEST,
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
                code: NRPC_TYPED_HANDLER_ERROR,
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

// ============================================================================
// Phase E — SDK veneer for client-streaming / duplex.
//
// Substrate types (RequestStream, ClientStreamCallRaw,
// DuplexCallRaw, etc.) yield raw `Bytes`. The veneer wraps them in
// typed primitives that auto-encode on send and auto-decode on
// poll via a captured `Codec`. Adds zero new wire bits — every
// frame on the wire is exactly what the substrate emits.
//
// `Chunk<T>` is the SDK-internal classification of inbound
// request frames: `Init` for the first item (whose body came
// from the initial REQUEST), `Data` for subsequent items (whose
// bodies came from REQUEST_CHUNKs). Pure SDK abstraction — the
// wire only knows flag bits.
// ============================================================================

/// SDK-internal classification of an inbound typed request frame.
/// NOT a wire encoding — the wire stays flag-bit-tagged. The
/// veneer constructs `Chunk<T>` values by tracking "have I seen
/// the first item yet" on the substrate's `RequestStream`.
///
/// Users typically don't see this type — [`RequestStreamTyped<Req>`]
/// yields the bare `Req` flattened. The opt-in
/// [`ChunkedRequestStream<Req>`] (via
/// [`RequestStreamTyped::into_chunked`]) exposes the
/// distinction for callers that need it.
///
/// Bidi streaming plan (Phase E).
#[derive(Debug, Clone)]
pub enum Chunk<T> {
    /// First decoded item on the request stream — corresponds to
    /// the initial REQUEST's body.
    Init(T),
    /// Subsequent decoded item — corresponds to a REQUEST_CHUNK
    /// body.
    Data(T),
}

/// Typed counterpart of [`RequestStream`] for the **flattened**
/// API. Yields `Req` (both `Init` and `Data` collapse to a bare
/// `Req`); EOF when the substrate stream closes (REQUEST_END or
/// CANCEL).
///
/// Decode failure terminates the stream with a single
/// `Err(RpcError::Codec)` then closes — mirror of
/// [`RpcStreamTyped`]'s contract on the response side.
///
/// For callers that need to distinguish "first request from this
/// upload session" from "subsequent chunks" (sessions with
/// explicit init handshake, rolling-window aggregation, etc.),
/// call [`Self::into_chunked`] to get a [`ChunkedRequestStream<Req>`]
/// instead.
///
/// Bidi streaming plan (Phase E).
pub struct RequestStreamTyped<Req> {
    inner: RequestStream,
    codec: Codec,
    done: bool,
    /// Tracks whether at least one decoded request has already been
    /// yielded from this handle. Carried into `ChunkedRequestStream`
    /// by [`Self::into_chunked`] so a conversion AFTER partial
    /// consumption does not misclassify the next chunk as
    /// [`Chunk::Init`].
    seen_first: bool,
    _req: std::marker::PhantomData<fn() -> Req>,
}

impl<Req> RequestStreamTyped<Req> {
    /// Convert this flattened stream into a [`ChunkedRequestStream<Req>`]
    /// that distinguishes [`Chunk::Init`] from [`Chunk::Data`].
    /// Same underlying substrate stream — no extra wire traffic,
    /// no replay.
    ///
    /// `seen_first` is carried over from the source: if this handle
    /// has already yielded at least one item, the next chunk from
    /// the converted stream is [`Chunk::Data`], not [`Chunk::Init`].
    pub fn into_chunked(self) -> ChunkedRequestStream<Req> {
        ChunkedRequestStream {
            inner: self.inner,
            codec: self.codec,
            done: self.done,
            seen_first: self.seen_first,
            _req: std::marker::PhantomData,
        }
    }
}

impl<Req: DeserializeOwned + Unpin> futures::Stream for RequestStreamTyped<Req> {
    type Item = std::result::Result<Req, RpcError>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        if self.done {
            return std::task::Poll::Ready(None);
        }
        let codec = self.codec;
        match std::pin::Pin::new(&mut self.inner).poll_next(cx) {
            std::task::Poll::Ready(Some(bytes)) => match codec.decode::<Req>(&bytes) {
                Ok(value) => {
                    self.seen_first = true;
                    std::task::Poll::Ready(Some(Ok(value)))
                }
                Err(e) => {
                    self.done = true;
                    std::task::Poll::Ready(Some(Err(RpcError::Codec {
                        direction: CodecDirection::Decode,
                        message: format!("typed request stream decode: {e}"),
                    })))
                }
            },
            std::task::Poll::Ready(None) => {
                self.done = true;
                std::task::Poll::Ready(None)
            }
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

/// Opt-in variant of [`RequestStreamTyped`] that yields
/// [`Chunk<Req>`] values, distinguishing the first request item
/// ([`Chunk::Init`]) from subsequent items ([`Chunk::Data`]).
/// EOF is signaled by the stream returning `None`, same as the
/// flattened variant.
///
/// Bidi streaming plan (Phase E).
pub struct ChunkedRequestStream<Req> {
    inner: RequestStream,
    codec: Codec,
    done: bool,
    seen_first: bool,
    _req: std::marker::PhantomData<fn() -> Req>,
}

impl<Req: DeserializeOwned + Unpin> futures::Stream for ChunkedRequestStream<Req> {
    type Item = std::result::Result<Chunk<Req>, RpcError>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        if self.done {
            return std::task::Poll::Ready(None);
        }
        let codec = self.codec;
        match std::pin::Pin::new(&mut self.inner).poll_next(cx) {
            std::task::Poll::Ready(Some(bytes)) => match codec.decode::<Req>(&bytes) {
                Ok(value) => {
                    let chunk = if self.seen_first {
                        Chunk::Data(value)
                    } else {
                        self.seen_first = true;
                        Chunk::Init(value)
                    };
                    std::task::Poll::Ready(Some(Ok(chunk)))
                }
                Err(e) => {
                    self.done = true;
                    std::task::Poll::Ready(Some(Err(RpcError::Codec {
                        direction: CodecDirection::Decode,
                        message: format!("typed request stream decode: {e}"),
                    })))
                }
            },
            std::task::Poll::Ready(None) => {
                self.done = true;
                std::task::Poll::Ready(None)
            }
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

/// Typed caller-side handle for a client-streaming call. Encodes
/// each `send(&Req)` via the captured `Codec`; decodes the
/// terminal RESPONSE body into `Resp` on `finish`.
///
/// Bidi streaming plan (Phase E).
pub struct ClientStreamCallTyped<Req, Resp> {
    inner: ClientStreamCallRaw,
    codec: Codec,
    _req: std::marker::PhantomData<fn(Req)>,
    _resp: std::marker::PhantomData<fn() -> Resp>,
}

impl<Req: Serialize, Resp: DeserializeOwned> ClientStreamCallTyped<Req, Resp> {
    /// Encode `value` via the captured codec and publish it as
    /// the next REQUEST / REQUEST_CHUNK frame.
    pub async fn send(&mut self, value: &Req) -> std::result::Result<(), RpcError> {
        let bytes = self.codec.encode(value).map_err(|e| RpcError::Codec {
            direction: CodecDirection::Encode,
            message: format!("client stream typed encode: {e}"),
        })?;
        self.inner.send(Bytes::from(bytes)).await
    }

    /// Close the upload and await the typed terminal response.
    pub async fn finish(self) -> std::result::Result<Resp, RpcError> {
        let reply = self.inner.finish().await?;
        self.codec.decode(&reply.body).map_err(|e| RpcError::Codec {
            direction: CodecDirection::Decode,
            message: format!("client stream typed decode: {e}"),
        })
    }

    /// Server-assigned `call_id` of the underlying call.
    pub fn call_id(&self) -> u64 {
        self.inner.call_id()
    }

    /// Whether the upload side is flow-controlled.
    pub fn flow_controlled(&self) -> bool {
        self.inner.flow_controlled()
    }
}

/// Typed caller-side handle for a duplex call. Combines a
/// [`DuplexSinkTyped<Req>`] (upload) and [`DuplexStreamTyped<Resp>`]
/// (download). For the "encoder task + decoder task" use case,
/// call [`Self::into_split`] to peel off the two halves.
///
/// Bidi streaming plan (Phase E).
pub struct DuplexCallTyped<Req, Resp> {
    inner: DuplexCallRaw,
    codec: Codec,
    /// Latched true after the response stream surfaces a terminal
    /// state (decode error, raw error, EOF). Subsequent `poll_next`
    /// calls return `Ready(None)` — matches the contract that
    /// [`DuplexStreamTyped`] enforces.
    done: bool,
    _req: std::marker::PhantomData<fn(Req)>,
    _resp: std::marker::PhantomData<fn() -> Resp>,
}

impl<Req: Serialize, Resp: DeserializeOwned + Unpin> DuplexCallTyped<Req, Resp> {
    /// Encode and publish one request frame.
    pub async fn send(&mut self, value: &Req) -> std::result::Result<(), RpcError> {
        let bytes = self.codec.encode(value).map_err(|e| RpcError::Codec {
            direction: CodecDirection::Encode,
            message: format!("duplex typed encode: {e}"),
        })?;
        self.inner.send(Bytes::from(bytes)).await
    }

    /// Close the upload direction. Response stream stays open.
    pub async fn finish_sending(&mut self) -> std::result::Result<(), RpcError> {
        self.inner.finish_sending().await
    }

    /// Server-assigned `call_id`.
    pub fn call_id(&self) -> u64 {
        self.inner.call_id()
    }

    /// Whether the upload side is flow-controlled.
    pub fn flow_controlled(&self) -> bool {
        self.inner.flow_controlled()
    }

    /// Split into independent typed halves. Both halves hold an
    /// `Arc<DuplexInner>` (via the substrate types); CANCEL fires
    /// only when BOTH halves drop without a clean close.
    pub fn into_split(self) -> (DuplexSinkTyped<Req>, DuplexStreamTyped<Resp>) {
        let (sink, stream) = self.inner.into_split();
        (
            DuplexSinkTyped {
                inner: sink,
                codec: self.codec,
                _req: std::marker::PhantomData,
            },
            DuplexStreamTyped {
                inner: stream,
                codec: self.codec,
                done: false,
                _resp: std::marker::PhantomData,
            },
        )
    }
}

impl<Req, Resp: DeserializeOwned + Unpin> futures::Stream for DuplexCallTyped<Req, Resp> {
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
                        message: format!("duplex typed decode: {e}"),
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

/// Typed send-half of a split duplex call.
pub struct DuplexSinkTyped<Req> {
    inner: DuplexSink,
    codec: Codec,
    _req: std::marker::PhantomData<fn(Req)>,
}

impl<Req: Serialize> DuplexSinkTyped<Req> {
    /// Encode + send one request frame.
    pub async fn send(&mut self, value: &Req) -> std::result::Result<(), RpcError> {
        let bytes = self.codec.encode(value).map_err(|e| RpcError::Codec {
            direction: CodecDirection::Encode,
            message: format!("duplex typed encode: {e}"),
        })?;
        self.inner.send(Bytes::from(bytes)).await
    }

    /// Close the upload direction.
    pub async fn finish_sending(self) -> std::result::Result<(), RpcError> {
        self.inner.finish_sending().await
    }

    /// Server-assigned `call_id`.
    pub fn call_id(&self) -> u64 {
        self.inner.call_id()
    }
}

/// Typed receive-half of a split duplex call. Implements
/// `futures::Stream<Item = Result<Resp, RpcError>>`. Decode
/// failure surfaces one `Err(RpcError::Codec)` then closes.
pub struct DuplexStreamTyped<Resp> {
    inner: DuplexStream,
    codec: Codec,
    done: bool,
    _resp: std::marker::PhantomData<fn() -> Resp>,
}

impl<Resp> DuplexStreamTyped<Resp> {
    /// Server-assigned `call_id`.
    pub fn call_id(&self) -> u64 {
        self.inner.call_id()
    }
}

impl<Resp: DeserializeOwned + Unpin> futures::Stream for DuplexStreamTyped<Resp> {
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
                        message: format!("duplex typed decode: {e}"),
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
// Internal: typed client-streaming-handler adapter.
//
// Bridges `Fn(RequestStreamTyped<Req>) -> Future<Result<Resp, String>>`
// to the raw `RpcClientStreamingHandler` trait.
// ============================================================================

struct TypedClientStreamingRpcHandler<Req, Resp, F> {
    codec: Codec,
    inner: Arc<F>,
    _req: std::marker::PhantomData<Req>,
    _resp: std::marker::PhantomData<Resp>,
}

#[async_trait]
impl<Req, Resp, F, Fut> RpcClientStreamingHandler for TypedClientStreamingRpcHandler<Req, Resp, F>
where
    Req: DeserializeOwned + Send + Sync + Unpin + 'static,
    Resp: Serialize + Send + Sync + 'static,
    F: Fn(RequestStreamTyped<Req>) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = std::result::Result<Resp, String>> + Send + 'static,
{
    async fn call(
        &self,
        _ctx: RpcStreamingContext,
        requests: RequestStream,
    ) -> std::result::Result<RpcResponsePayload, RpcHandlerError> {
        let typed_requests = RequestStreamTyped {
            inner: requests,
            codec: self.codec,
            done: false,
            seen_first: false,
            _req: std::marker::PhantomData,
        };
        let resp =
            (self.inner)(typed_requests)
                .await
                .map_err(|message| RpcHandlerError::Application {
                    code: NRPC_TYPED_HANDLER_ERROR,
                    message,
                })?;
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
// Internal: typed duplex-handler adapter.
//
// Bridges `Fn(RequestStreamTyped<Req>, ResponseSinkTyped<Resp>) ->
// Future<Result<(), String>>` to the raw `RpcDuplexHandler` trait.
// ============================================================================

struct TypedDuplexRpcHandler<Req, Resp, F> {
    codec: Codec,
    inner: Arc<F>,
    _req: std::marker::PhantomData<Req>,
    _resp: std::marker::PhantomData<Resp>,
}

#[async_trait]
impl<Req, Resp, F, Fut> RpcDuplexHandler for TypedDuplexRpcHandler<Req, Resp, F>
where
    Req: DeserializeOwned + Send + Sync + Unpin + 'static,
    Resp: Serialize + Send + Sync + 'static,
    F: Fn(RequestStreamTyped<Req>, ResponseSinkTyped<Resp>) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = std::result::Result<(), String>> + Send + 'static,
{
    async fn call(
        &self,
        _ctx: RpcStreamingContext,
        requests: RequestStream,
        responses: RpcResponseSink,
    ) -> std::result::Result<(), RpcHandlerError> {
        let typed_requests = RequestStreamTyped {
            inner: requests,
            codec: self.codec,
            done: false,
            seen_first: false,
            _req: std::marker::PhantomData,
        };
        let typed_sink = ResponseSinkTyped {
            inner: responses,
            codec: self.codec,
            _resp: std::marker::PhantomData,
        };
        (self.inner)(typed_requests, typed_sink)
            .await
            .map_err(|message| RpcHandlerError::Application {
                code: NRPC_TYPED_HANDLER_ERROR,
                message,
            })
    }
}
