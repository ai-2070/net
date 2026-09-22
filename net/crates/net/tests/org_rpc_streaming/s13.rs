//! Helpers for the slice 1.3 (fold ownership + lifetime) witnesses.
//!
//! The unit-witness idiom: the witnesses drive the production fold seams
//! (`RpcServerStreamingFold::apply_inbound*`, `RpcStreamingRequestFold::
//! apply_inbound`) directly with hand-built `RpcInboundEvent`s, and observe
//! `in_flight_keys()` / `sender_keys()` shape, the protected records'
//! `ProtectedStreamCall` handles, and every emitted frame at a capturing
//! emitter seam. Timing is real-time with generous bounds (the nextest
//! `terminate-after` timeout is the hang catcher).

#![allow(dead_code)]

use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use net::adapter::net::behavior::admission_clock::ClockSample;
use net::adapter::net::behavior::org::OrgId;
use net::adapter::net::behavior::org_admission::Admitted;
use net::adapter::net::behavior::org_grant::CapabilityAuthorityId;
use net::adapter::net::cortex::rpc::{
    encode_rpc_route, encode_stream_grant, RequestStream, RpcAsyncResponseEmitter,
    RpcClientStreamingHandler, RpcContext, RpcHandlerError, RpcInboundEvent,
    RpcRequestChunkPayload, RpcRequestPayload, RpcResponseEmitter, RpcResponsePayload,
    RpcResponseSink, RpcStatus, RpcStreamingContext, RpcStreamingHandler, StreamCallLifetime,
    StreamLifetimePolicy, DISPATCH_RPC_CANCEL, DISPATCH_RPC_REQUEST, DISPATCH_RPC_REQUEST_CHUNK,
    DISPATCH_RPC_STREAM_GRANT, FLAG_RPC_CLIENT_STREAMING_REQUEST, FLAG_RPC_REQUEST_END,
    FLAG_RPC_STREAMING_RESPONSE, HEADER_NRPC_STREAM_WINDOW_INITIAL,
};
use net::adapter::net::cortex::EventMeta;
use net::adapter::net::identity::EntityId;

pub(crate) const SEC: u64 = 1_000_000_000;
/// The key every witness observes: `(from_node, session_id, origin, call_id)` (C5).
pub(crate) type CallKey = (u64, u64, u64, u64);

/// The captured response frames at the emitter seam, in emission order.
pub(crate) type CapturedFrames = Arc<Mutex<Vec<RpcResponsePayload>>>;

/// A capturing async emitter (`RpcAsyncResponseEmitter`) — the response
/// emitter seam the SS fold's supervisor emits through.
pub(crate) fn capturing_async_emitter() -> (RpcAsyncResponseEmitter, CapturedFrames) {
    let captured: CapturedFrames = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&captured);
    let emit: RpcAsyncResponseEmitter = Arc::new(move |_from, _origin, _call, resp| {
        sink.lock().push(resp);
        Box::pin(async move {})
    });
    (emit, captured)
}

/// A capturing sync emitter (`RpcResponseEmitter`, the CS fold's
/// terminal-only emitter).
pub(crate) fn capturing_emitter() -> (RpcResponseEmitter, CapturedFrames) {
    let captured: CapturedFrames = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&captured);
    let emit: RpcResponseEmitter = Arc::new(move |_from, _session, _origin, _call, resp| {
        sink.lock().push(resp);
    });
    (emit, captured)
}

/// One synthetic `Admitted` fact set (the fold seam takes the verified
/// facts; minting them is the verifier's job and slice 1.5's bridge).
pub(crate) fn synthetic_admitted() -> Admitted {
    Admitted {
        caller: EntityId::from_bytes([0x24u8; 32]),
        acting_org: OrgId::from_bytes([0x42u8; 32]),
        provider_org: OrgId::from_bytes([0x42u8; 32]),
        provider: EntityId::from_bytes([0x99u8; 32]),
        capability: CapabilityAuthorityId::for_tag("nrpc:svc"),
    }
}

/// The §2.1 lifetime inputs for one opening: `policy`, the credential
/// validity ends, and one clock sample (the same `clock` the witness
/// asserts against, so resolved ends are exact).
pub(crate) fn lifetime<'a>(
    policy: StreamLifetimePolicy,
    credential_ends_ns: &'a [Option<u64>],
    clock: ClockSample,
) -> StreamCallLifetime<'a> {
    StreamCallLifetime {
        policy,
        credential_ends_ns,
        clock,
    }
}

/// A tiny injectable policy (default 400 ms / max 30 s) so expiry
/// witnesses run in real time with generous observation bounds.
pub(crate) fn tiny_policy() -> StreamLifetimePolicy {
    StreamLifetimePolicy {
        default_live_ns: 400 * 1_000_000,
        max_live_ns: 30 * SEC,
    }
}

/// Frame builders — the folds decode at `RPC_FRAME_BODY_OFFSET`, so the
/// frames carry `EventMeta` + the RpcRouteV1 placeholder + payload,
/// exactly as the ingress path delivers them.
pub(crate) fn request_frame(origin: u64, call_id: u64, req: &RpcRequestPayload) -> Bytes {
    let meta = EventMeta::new(DISPATCH_RPC_REQUEST, 0, origin, call_id, 0);
    let mut buf = Vec::new();
    buf.extend_from_slice(&meta.to_bytes());
    encode_rpc_route(&mut buf, 0);
    req.encode_into(&mut buf);
    Bytes::from(buf)
}

pub(crate) fn chunk_frame(origin: u64, call_id: u64, chunk: &RpcRequestChunkPayload) -> Bytes {
    let meta = EventMeta::new(DISPATCH_RPC_REQUEST_CHUNK, 0, origin, call_id, 0);
    let mut buf = Vec::new();
    buf.extend_from_slice(&meta.to_bytes());
    encode_rpc_route(&mut buf, 0);
    chunk.encode_into(&mut buf);
    Bytes::from(buf)
}

pub(crate) fn cancel_frame(origin: u64, call_id: u64) -> Bytes {
    let meta = EventMeta::new(DISPATCH_RPC_CANCEL, 0, origin, call_id, 0);
    let mut buf = Vec::new();
    buf.extend_from_slice(&meta.to_bytes());
    encode_rpc_route(&mut buf, 0);
    Bytes::from(buf)
}

pub(crate) fn grant_frame(origin: u64, call_id: u64, n: u32) -> Bytes {
    let meta = EventMeta::new(DISPATCH_RPC_STREAM_GRANT, 0, origin, call_id, 0);
    let mut buf = Vec::new();
    buf.extend_from_slice(&meta.to_bytes());
    encode_rpc_route(&mut buf, 0);
    buf.extend_from_slice(&encode_stream_grant(n));
    Bytes::from(buf)
}

/// The receiving-end event: `session_id` is the AEAD-verified receiving
/// incarnation (the C5 key term this slice's witnesses are about).
pub(crate) fn inbound(
    session_id: u64,
    from_node: u64,
    origin: u64,
    payload: Bytes,
) -> RpcInboundEvent {
    RpcInboundEvent {
        session_id,
        channel_hash: 0,
        origin_hash: origin,
        from_node,
        payload,
    }
}

/// A server-streaming opening payload with SS flags.
pub(crate) fn ss_request(service: &str, deadline_ns: u64, body: &[u8]) -> RpcRequestPayload {
    RpcRequestPayload {
        service: service.to_string(),
        deadline_ns,
        flags: FLAG_RPC_STREAMING_RESPONSE,
        headers: vec![],
        body: Bytes::copy_from_slice(body),
    }
}

/// A server-streaming opening payload opting into flow control with
/// `window` (0 = zero initial credit: the pump parks until a grant).
pub(crate) fn ss_request_windowed(
    service: &str,
    deadline_ns: u64,
    window: u32,
) -> RpcRequestPayload {
    let mut req = ss_request(service, deadline_ns, b"");
    req.headers.push((
        HEADER_NRPC_STREAM_WINDOW_INITIAL.to_string(),
        window.to_string().into_bytes(),
    ));
    req
}

/// A client-streaming opening payload with CS flags.
pub(crate) fn cs_request(service: &str, deadline_ns: u64, body: &[u8]) -> RpcRequestPayload {
    RpcRequestPayload {
        service: service.to_string(),
        deadline_ns,
        flags: FLAG_RPC_CLIENT_STREAMING_REQUEST,
        headers: vec![],
        body: Bytes::copy_from_slice(body),
    }
}

/// A REQUEST_CHUNK carrying `body`; `end` sets FLAG_RPC_REQUEST_END.
pub(crate) fn chunk_payload(call_id: u64, body: &[u8], end: bool) -> RpcRequestChunkPayload {
    RpcRequestChunkPayload {
        call_id,
        flags: if end { FLAG_RPC_REQUEST_END } else { 0 },
        headers: vec![],
        body: Bytes::copy_from_slice(body),
    }
}

/// A streaming handler that emits `chunks`, flips `returned` just
/// before returning `Ok`, and emits nothing else.
pub(crate) struct EmitAndReturn {
    pub(crate) chunks: Vec<&'static [u8]>,
    pub(crate) returned: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl RpcStreamingHandler for EmitAndReturn {
    async fn call(&self, _ctx: RpcContext, sink: RpcResponseSink) -> Result<(), RpcHandlerError> {
        for chunk in &self.chunks {
            sink.send(Bytes::from_static(chunk));
        }
        self.returned.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// A streaming handler that counts its invocation and parks (the
/// zero-effects witnesses' darkness probe).
pub(crate) struct CountingStream {
    pub(crate) calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl RpcStreamingHandler for CountingStream {
    async fn call(&self, _ctx: RpcContext, _sink: RpcResponseSink) -> Result<(), RpcHandlerError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        std::future::pending::<()>().await;
        Ok(())
    }
}

/// A streaming handler that parks forever ("idle"), flips `dropped`
/// when its future is dropped (retirement reached it), and bumps
/// `started` on first poll (a retire that wins BEFORE the supervisor's
/// first poll never enters the handler at all — witnesses wait for
/// `started` before retiring).
pub(crate) struct ParkForever {
    pub(crate) dropped: Arc<AtomicBool>,
    pub(crate) started: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl RpcStreamingHandler for ParkForever {
    async fn call(&self, _ctx: RpcContext, _sink: RpcResponseSink) -> Result<(), RpcHandlerError> {
        struct DropFlag(Arc<AtomicBool>);
        impl Drop for DropFlag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        self.started.fetch_add(1, Ordering::SeqCst);
        let _flag = DropFlag(Arc::clone(&self.dropped));
        std::future::pending::<()>().await;
        Ok(())
    }
}

/// A streaming handler that emits one chunk, records its drops through
/// `dropped`, and parks until `release` fires (used by the
/// serve-handle-drop witness's two siblings).
pub(crate) struct ParkUntilReleased {
    pub(crate) release: Arc<tokio::sync::Notify>,
    pub(crate) dropped: Arc<AtomicBool>,
    pub(crate) ran: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl RpcStreamingHandler for ParkUntilReleased {
    async fn call(&self, _ctx: RpcContext, sink: RpcResponseSink) -> Result<(), RpcHandlerError> {
        struct DropFlag(Arc<AtomicBool>);
        impl Drop for DropFlag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let _flag = DropFlag(Arc::clone(&self.dropped));
        self.ran.fetch_add(1, Ordering::SeqCst);
        sink.send(Bytes::from_static(b"live"));
        self.release.notified().await;
        Ok(())
    }
}

/// A client-streaming handler that collects every request body and
/// returns them joined in its terminal response body — so the witness
/// observes EXACTLY which chunks reached the handler's stream.
pub(crate) struct CollectBodies {
    pub(crate) collected: Arc<Mutex<Vec<Bytes>>>,
}

#[async_trait::async_trait]
impl RpcClientStreamingHandler for CollectBodies {
    async fn call(
        &self,
        _ctx: RpcStreamingContext,
        mut requests: RequestStream,
    ) -> Result<RpcResponsePayload, RpcHandlerError> {
        use futures::StreamExt;
        while let Some(body) = requests.next().await {
            self.collected.lock().push(body);
        }
        let joined: Vec<u8> = self
            .collected
            .lock()
            .iter()
            .flat_map(|b| b.to_vec())
            .collect();
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: Bytes::from(joined),
        })
    }
}

/// A client-streaming handler that parks forever (the C7 witness).
pub(crate) struct CsParkForever {
    pub(crate) dropped: Arc<AtomicBool>,
    pub(crate) started: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl RpcClientStreamingHandler for CsParkForever {
    async fn call(
        &self,
        _ctx: RpcStreamingContext,
        _requests: RequestStream,
    ) -> Result<RpcResponsePayload, RpcHandlerError> {
        struct DropFlag(Arc<AtomicBool>);
        impl Drop for DropFlag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        self.started.fetch_add(1, Ordering::SeqCst);
        let _flag = DropFlag(Arc::clone(&self.dropped));
        std::future::pending::<()>().await;
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: Bytes::new(),
        })
    }
}

/// Bounded polling (`fixture::wait_until`'s shape) for outcome
/// assertions that must hold within a generous real-time bound.
pub(crate) async fn wait_for<F: Fn() -> bool>(limit: Duration, cond: F) -> bool {
    crate::fixture::wait_until(limit, cond).await
}
