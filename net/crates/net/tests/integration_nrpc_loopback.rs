//! End-to-end nRPC integration test (in-process loopback).
//!
//! Proves the server-side + client-side folds compose into a working
//! request/response round trip, without going through the real
//! mesh/cortex publish path. The "loopback" routes synthesized
//! `RedexEvent`s between the two folds directly:
//!
//! - The CALLER builds a `RpcRequestPayload`, registers a oneshot
//!   in the client `RpcClientPending`, and synthesizes a REQUEST
//!   `RedexEvent` that gets fed into the server's fold.
//! - The SERVER's fold dispatches the handler in tokio. When the
//!   handler completes, the emit callback synthesizes a RESPONSE
//!   `RedexEvent` and feeds it into the client's fold.
//! - The CALLER awaits the oneshot.
//!
//! What this DOESN'T test (left for the Mesh glue follow-up):
//!
//! - Real channel subscription / dispatch via `Mesh::serve_rpc` /
//!   `Mesh::call`.
//! - Queue-group dispatch across multiple server replicas.
//! - Cross-process / cross-network routing.
//! - Persistence / replay (the loopback uses synthesized events,
//!   not events stored in a real RedEX file).
//!
//! What it DOES test (the load-bearing protocol-level shape):
//!
//! - REQUEST → handler → RESPONSE round-trip.
//! - Caller-side cancellation flowing into the handler.
//! - Concurrent calls multiplexed via `seq_or_ts` correlation.
//! - Server panic surfaces as `RpcStatus::Internal`.
//! - Application error surfaces as `RpcStatus::Application(code)`.

#![cfg(feature = "cortex")]

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use net::adapter::net::cortex::{
    EventMeta, RpcClientFold, RpcClientPending, RpcContext, RpcHandler, RpcHandlerError,
    RpcRequestPayload, RpcResponseEmitter, RpcResponsePayload, RpcServerFold, RpcStatus,
    DISPATCH_RPC_CANCEL, DISPATCH_RPC_REQUEST, DISPATCH_RPC_RESPONSE, EVENT_META_SIZE,
};
use net::adapter::net::redex::{RedexEntry, RedexEvent, RedexFold};
use parking_lot::Mutex;

// ============================================================================
// Loopback harness — synthesizes the publish path between the two folds.
// ============================================================================

fn make_event(meta: EventMeta, payload_tail: &[u8]) -> RedexEvent {
    let mut buf = Vec::with_capacity(EVENT_META_SIZE + payload_tail.len());
    buf.extend_from_slice(&meta.to_bytes());
    buf.extend_from_slice(payload_tail);
    RedexEvent {
        entry: RedexEntry::new_heap(0, 0, buf.len() as u32, 0, 0),
        payload: Bytes::from(buf),
    }
}

fn request_event(caller_origin: u64, call_id: u64, payload: &RpcRequestPayload) -> RedexEvent {
    let meta = EventMeta::new(DISPATCH_RPC_REQUEST, 0, caller_origin, call_id, 0);
    make_event(meta, &payload.encode())
}

fn cancel_event(caller_origin: u64, call_id: u64) -> RedexEvent {
    let meta = EventMeta::new(DISPATCH_RPC_CANCEL, 0, caller_origin, call_id, 0);
    make_event(meta, &[])
}

fn response_event(caller_origin: u64, call_id: u64, payload: &RpcResponsePayload) -> RedexEvent {
    let meta = EventMeta::new(DISPATCH_RPC_RESPONSE, 0, caller_origin, call_id, 0);
    make_event(meta, &payload.encode())
}

/// One end-to-end loopback: a server fold whose emit callback feeds
/// directly into the client fold's `apply`. Both folds share their
/// own state (in-flight set / pending senders); the harness owns
/// both folds and exposes ergonomic `call(...)` / `cancel(...)`
/// methods that mimic the eventual `Mesh::call` API.
struct Loopback<H: RpcHandler> {
    server_fold: Arc<Mutex<RpcServerFold>>,
    pending: Arc<RpcClientPending>,
    next_call_id: AtomicU64,
    caller_origin: u64,
    _handler: std::marker::PhantomData<H>,
}

impl<H: RpcHandler> Loopback<H> {
    fn new(handler: Arc<H>, caller_origin: u64) -> Self {
        let pending = Arc::new(RpcClientPending::new());
        // The client fold is owned exclusively by the emit
        // closure: every emitted RESPONSE event flows through
        // `client_fold.apply(...)`, which routes the response to
        // the matching pending oneshot. The harness itself doesn't
        // need a separate handle on the fold — `pending` is the
        // shared state that both sides observe.
        let client_fold = Arc::new(Mutex::new(RpcClientFold::new(pending.clone())));
        let emit: RpcResponseEmitter = Arc::new(move |origin, call_id, resp| {
            let ev = response_event(origin, call_id, &resp);
            // Drive the client fold synchronously. In the real
            // Mesh wire-up the emit closure publishes the RESPONSE
            // event onto the reply channel; the bus routes it
            // through the network to the caller's local cortex
            // adapter, which folds it via the same client fold.
            // The synchronous in-process path here is the
            // loopback's stand-in for that round-trip.
            let mut fold = client_fold.lock();
            fold.apply(&ev, &mut ()).expect("client fold apply");
        });
        let server_fold = Arc::new(Mutex::new(RpcServerFold::new(handler, emit)));
        Self {
            server_fold,
            pending,
            next_call_id: AtomicU64::new(1),
            caller_origin,
            _handler: std::marker::PhantomData,
        }
    }

    /// Mimic `Mesh::call(service, payload, opts)`. Allocates a
    /// fresh call_id, registers a oneshot, "publishes" the REQUEST
    /// directly into the server fold, awaits the response.
    async fn call(
        &self,
        payload: RpcRequestPayload,
    ) -> Result<RpcResponsePayload, tokio::sync::oneshot::error::RecvError> {
        let call_id = self.next_call_id.fetch_add(1, Ordering::Relaxed);
        // Loopback: no wire session peer; register with
        // target_node=0 so the fold's deliver gate accepts the
        // loopback `RedexFold::apply` path (from_node=0).
        let rx = self.pending.register(call_id, 0);
        let ev = request_event(self.caller_origin, call_id, &payload);
        // Drive the server fold; the spawned handler will eventually
        // call our emit closure, which feeds the RESPONSE through
        // the client fold, which completes the oneshot.
        self.server_fold
            .lock()
            .apply(&ev, &mut ())
            .expect("server fold apply");
        rx.await
    }

    /// Send a CANCEL event for `call_id` to the server fold,
    /// keeping the pending entry alive so the handler's
    /// cancellation-driven response still reaches the caller. This
    /// matches the "cancel and observe" semantic — the caller
    /// wants to know whether the server actually observed the
    /// cancel and what status it returned.
    ///
    /// The "cancel and forget" semantic (caller drops the future
    /// and stops caring) is exercised by the production
    /// `Mesh::call` Drop impl, which sends CANCEL AND clears the
    /// pending entry. Testing that path requires the real Mesh
    /// glue and is left for the integration test that follows.
    fn request_cancel(&self, call_id: u64) {
        let ev = cancel_event(self.caller_origin, call_id);
        self.server_fold
            .lock()
            .apply(&ev, &mut ())
            .expect("server fold apply (cancel)");
    }
}

// ============================================================================
// Test handlers.
// ============================================================================

struct EchoHandler;

#[async_trait::async_trait]
impl RpcHandler for EchoHandler {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: ctx.payload.body,
        })
    }
}

/// Counts how many times the handler ran. Used to confirm that
/// concurrent calls each invoked the handler exactly once.
struct CountingEchoHandler {
    count: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl RpcHandler for CountingEchoHandler {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        self.count.fetch_add(1, Ordering::Relaxed);
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: ctx.payload.body,
        })
    }
}

// ============================================================================
// Tests.
// ============================================================================

#[tokio::test]
async fn nrpc_loopback_round_trip() {
    let loopback = Loopback::new(Arc::new(EchoHandler), 0xCAFE);
    let req = RpcRequestPayload {
        service: "echo".to_string(),
        deadline_ns: 0,
        flags: 0,
        headers: vec![],
        body: bytes::Bytes::from_static(b"hello world"),
    };
    let resp = tokio::time::timeout(Duration::from_secs(2), loopback.call(req))
        .await
        .expect("call must complete within 2s")
        .expect("oneshot delivers");
    assert_eq!(resp.status, RpcStatus::Ok);
    assert_eq!(resp.body.as_ref(), b"hello world");
}

/// Multiple concurrent calls must each get their own correctly-
/// correlated response. This is the core "stream id is the
/// correlation id" property of the protocol — out-of-order
/// completions at the server don't corrupt caller-side routing.
#[tokio::test]
async fn nrpc_loopback_multiplexes_concurrent_calls() {
    let loopback = Arc::new(Loopback::new(Arc::new(EchoHandler), 0xBEEF));
    let mut futures = Vec::new();
    for i in 0..50u32 {
        let lb = loopback.clone();
        let body = format!("call-{i}").into_bytes();
        futures.push(tokio::spawn(async move {
            let req = RpcRequestPayload {
                service: "echo".to_string(),
                deadline_ns: 0,
                flags: 0,
                headers: vec![],
                body: body.clone().into(),
            };
            let resp = lb.call(req).await.expect("oneshot delivers");
            (body, resp.body)
        }));
    }
    for fut in futures {
        let (sent, received) = tokio::time::timeout(Duration::from_secs(5), fut)
            .await
            .expect("call must complete within 5s")
            .expect("task must not panic");
        assert_eq!(
            sent, received,
            "each call must receive its own body back, not another call's"
        );
    }
}

/// Concurrent calls each invoke the handler exactly once. The
/// counter rules out any silent deduplication that would skip
/// genuine duplicate-id collisions in the loopback.
#[tokio::test]
async fn nrpc_loopback_each_call_invokes_handler_exactly_once() {
    let count = Arc::new(AtomicUsize::new(0));
    let handler = Arc::new(CountingEchoHandler {
        count: count.clone(),
    });
    let loopback = Arc::new(Loopback::new(handler, 1));
    let mut futures = Vec::new();
    for _ in 0..100 {
        let lb = loopback.clone();
        futures.push(tokio::spawn(async move {
            let req = RpcRequestPayload {
                service: "x".to_string(),
                deadline_ns: 0,
                flags: 0,
                headers: vec![],
                body: bytes::Bytes::new(),
            };
            lb.call(req).await
        }));
    }
    for fut in futures {
        tokio::time::timeout(Duration::from_secs(5), fut)
            .await
            .expect("must complete")
            .expect("task must not panic")
            .expect("oneshot delivers");
    }
    assert_eq!(count.load(Ordering::Relaxed), 100);
}

/// Cancellation: caller emits a CANCEL after the handler is parked.
/// Handler observes `cancellation.cancelled().await` firing,
/// short-circuits, the response carries `RpcStatus::Internal` (the
/// handler returned an internal error in our test setup; production
/// handlers would emit a more specific status).
#[tokio::test]
async fn nrpc_loopback_cancellation_flows_to_handler() {
    struct CancelObserver;
    #[async_trait::async_trait]
    impl RpcHandler for CancelObserver {
        async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
            tokio::select! {
                _ = ctx.cancellation.cancelled() => {
                    Err(RpcHandlerError::Internal("cancelled by caller".to_string()))
                }
                _ = tokio::time::sleep(Duration::from_secs(10)) => {
                    Ok(RpcResponsePayload {
                        status: RpcStatus::Ok,
                        headers: vec![],
                        body: bytes::Bytes::from_static(b"completed without cancel"),
                    })
                }
            }
        }
    }
    let loopback = Arc::new(Loopback::new(Arc::new(CancelObserver), 1));

    // Issue the call from one task; cancel from the harness.
    let lb = loopback.clone();
    let call_handle = tokio::spawn(async move {
        let req = RpcRequestPayload {
            service: "x".to_string(),
            deadline_ns: 0,
            flags: 0,
            headers: vec![],
            body: bytes::Bytes::new(),
        };
        lb.call(req).await
    });

    // Wait briefly for the call to register. The first allocated
    // call_id is 1 (next_call_id starts at 1, fetch_add).
    tokio::time::sleep(Duration::from_millis(50)).await;
    loopback.request_cancel(1);

    let resp = tokio::time::timeout(Duration::from_secs(5), call_handle)
        .await
        .expect("call must complete after cancel within 5s")
        .expect("task must not panic")
        .expect("oneshot delivers");
    // CANCEL-wins: even though the handler returns
    // `Internal("cancelled by caller")`, the server fold overrides
    // the response with `RpcStatus::Cancelled` so the caller sees
    // the documented status code.
    assert_eq!(resp.status, RpcStatus::Cancelled);
}

/// Application errors surface end-to-end. Handler returns
/// `RpcHandlerError::Application { code, message }` → caller's
/// receiver sees `RpcResponsePayload { status: Application(code),
/// body: message_bytes, .. }`.
#[tokio::test]
async fn nrpc_loopback_application_error_round_trips() {
    struct AppErrHandler;
    #[async_trait::async_trait]
    impl RpcHandler for AppErrHandler {
        async fn call(&self, _ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
            Err(RpcHandlerError::Application {
                code: 0xBEEF,
                message: "validation failed: missing field 'id'".to_string(),
            })
        }
    }
    let loopback = Loopback::new(Arc::new(AppErrHandler), 1);
    let req = RpcRequestPayload {
        service: "validate".to_string(),
        deadline_ns: 0,
        flags: 0,
        headers: vec![],
        body: bytes::Bytes::from_static(b"{}"),
    };
    let resp = tokio::time::timeout(Duration::from_secs(2), loopback.call(req))
        .await
        .expect("call must complete within 2s")
        .expect("oneshot delivers");
    assert_eq!(resp.status, RpcStatus::Application(0xBEEF));
    assert_eq!(resp.body.as_ref(), b"validation failed: missing field 'id'");
}

/// Server panic surfaces as `Internal` on the caller side rather
/// than hanging forever. Pre-fix a handler panic would propagate
/// up the spawned task, log a tokio uncaught-panic, and silently
/// leave the caller waiting for a response that would never come.
#[tokio::test]
async fn nrpc_loopback_handler_panic_surfaces_as_internal() {
    struct PanicHandler;
    #[async_trait::async_trait]
    impl RpcHandler for PanicHandler {
        async fn call(&self, _ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
            panic!("explosion in the handler");
        }
    }
    let loopback = Loopback::new(Arc::new(PanicHandler), 1);
    let req = RpcRequestPayload {
        service: "boom".to_string(),
        deadline_ns: 0,
        flags: 0,
        headers: vec![],
        body: bytes::Bytes::new(),
    };
    let resp = tokio::time::timeout(Duration::from_secs(2), loopback.call(req))
        .await
        .expect("call must complete within 2s")
        .expect("oneshot delivers — panic must NOT hang the caller");
    assert_eq!(resp.status, RpcStatus::Internal);
    assert!(
        String::from_utf8_lossy(&resp.body).contains("explosion in the handler"),
        "panic message must surface in response body, got {:?}",
        String::from_utf8_lossy(&resp.body),
    );
}
