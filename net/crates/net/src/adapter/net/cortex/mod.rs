//! CortEX adapter — the seam between CortEX events and RedEX storage.
//!
//! Takes a CortEX `EventEnvelope`, projects it into a fixed 20-byte
//! [`EventMeta`] prefix plus a type-specific payload tail, appends the
//! concatenation to a [`super::redex::RedexFile`], and drives a
//! caller-supplied [`super::redex::RedexFold`] as the tail advances.
//! Exposes the materialized state as the read-side NetDB handle.
//!
//! See `docs/CORTEX_ADAPTER_PLAN.md` for the full design.
//!
//! # Layering
//!
//! - **Net** moves events and runs daemons.
//! - **RedEX** keeps a per-node append-only log.
//! - **CortEX adapter** (this module) projects Net events → RedEX
//!   payloads, folds them into local state, exposes that state as a
//!   read handle.
//! - **CortEX / NetDB** (outside this crate) query that state.

mod adapter;
mod config;
mod envelope;
mod error;
mod meta;
pub mod rpc;
#[cfg(feature = "cortex")]
pub mod rpc_observer;
#[cfg(feature = "cortex")]
mod watermark;

#[cfg(feature = "cortex")]
pub mod memories;
#[cfg(feature = "cortex")]
pub mod tasks;

pub use adapter::{
    set_global_ryw_inflight_cap, ChangeEvent, CortexAdapter, RywMetricsSnapshot, WaitForTokenError,
};
pub use config::{CortexAdapterConfig, FoldErrorPolicy, StartPosition, RYW_INFLIGHT_CAP_DEFAULT};
pub use envelope::{EventEnvelope, IntoRedexPayload};
pub use error::CortexAdapterError;
pub use meta::{
    compute_checksum, compute_checksum_with_meta, EventMeta, DISPATCH_RAW, EVENT_META_SIZE,
    FLAG_CAUSAL, FLAG_CONTINUITY_PROOF,
};
pub use rpc::{
    build_trace_headers, classify_streaming_chunk, decode_request_grant, decode_stream_grant,
    encode_request_grant, encode_stream_grant, extract_trace_context,
    parse_request_window_initial, parse_stream_window_initial, request_wire_size,
    response_wire_size, RequestStream, RpcAsyncResponseEmitter, RpcCancellationToken,
    RpcClientFold, RpcClientPending, RpcClientStreamingHandler, RpcCodecError, RpcContext,
    RpcHandler, RpcHandlerError, RpcHeader, RpcInboundDispatcher, RpcInboundEvent,
    RpcRequestChunkPayload, RpcRequestGrantEmitter, RpcRequestGrantPayload, RpcRequestPayload,
    RpcDuplexFold, RpcDuplexHandler, RpcResponseEmitter, RpcResponsePayload, RpcResponseSink,
    RpcServerFold, RpcServerStreamingFold, RpcStatus, RpcStreamingContext, RpcStreamingHandler,
    RpcStreamingRequestFold, StreamItem, StreamingChunkKind, TraceContext, DISPATCH_RPC_CANCEL,
    DISPATCH_RPC_DEADLINE_EXCEEDED, DISPATCH_RPC_REQUEST, DISPATCH_RPC_REQUEST_CHUNK,
    DISPATCH_RPC_REQUEST_GRANT, DISPATCH_RPC_RESPONSE, DISPATCH_RPC_STREAM_GRANT,
    FLAG_RPC_CLIENT_STREAMING_REQUEST, FLAG_RPC_PROPAGATE_TRACE, FLAG_RPC_REQUEST_END,
    FLAG_RPC_STREAMING_RESPONSE, HEADER_NRPC_REQUEST_WINDOW_INITIAL, HEADER_NRPC_STREAMING,
    HEADER_NRPC_STREAMING_CONTINUE, HEADER_NRPC_STREAMING_END, HEADER_NRPC_STREAM_WINDOW_INITIAL,
    MAX_RPC_BODY_LEN, MAX_RPC_HEADERS, MAX_RPC_HEADER_NAME_LEN, MAX_RPC_HEADER_VALUE_LEN,
    MAX_RPC_SERVICE_NAME_LEN,
};
#[cfg(feature = "cortex")]
pub use rpc_observer::{
    unix_now_ms as rpc_observer_unix_now_ms, RpcCallEvent, RpcCallStatus, RpcDirection,
    RpcObserver, RpcObserverHandle,
};
