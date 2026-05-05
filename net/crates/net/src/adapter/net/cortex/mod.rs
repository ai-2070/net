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
mod watermark;

#[cfg(feature = "cortex")]
pub mod memories;
#[cfg(feature = "cortex")]
pub mod tasks;

pub use adapter::{ChangeEvent, CortexAdapter};
pub use config::{CortexAdapterConfig, FoldErrorPolicy, StartPosition};
pub use envelope::{EventEnvelope, IntoRedexPayload};
pub use error::CortexAdapterError;
pub use meta::{
    compute_checksum, compute_checksum_with_meta, EventMeta, DISPATCH_RAW, EVENT_META_SIZE,
    FLAG_CAUSAL, FLAG_CONTINUITY_PROOF,
};
pub use rpc::{
    build_trace_headers, extract_trace_context, request_wire_size, response_wire_size,
    RpcCancellationToken, RpcClientFold, RpcClientPending, RpcCodecError, RpcContext, RpcHandler,
    RpcHandlerError, RpcHeader, RpcInboundDispatcher, RpcInboundEvent, RpcRequestPayload,
    RpcResponseEmitter, RpcResponsePayload, RpcServerFold, RpcStatus, TraceContext,
    DISPATCH_RPC_CANCEL, DISPATCH_RPC_DEADLINE_EXCEEDED, DISPATCH_RPC_REQUEST,
    DISPATCH_RPC_RESPONSE, FLAG_RPC_IDEMPOTENT, FLAG_RPC_PROPAGATE_TRACE,
    FLAG_RPC_STREAMING_RESPONSE, MAX_RPC_BODY_LEN, MAX_RPC_HEADERS, MAX_RPC_HEADER_NAME_LEN,
    MAX_RPC_HEADER_VALUE_LEN, MAX_RPC_SERVICE_NAME_LEN,
};
