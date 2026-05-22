//! End-to-end SDK test for the retry helper.
//!
//! Builds a real mesh + handshake and exercises:
//! - **Happy path retry** — server fails the first N attempts then
//!   succeeds; the wrapper observes the eventual success.
//! - **Non-retryable short-circuit** — handler returns an
//!   application error (`Err(String)`); the wrapper does NOT retry
//!   and surfaces the error after exactly one attempt.
//! - **Exhaustion** — server fails every attempt; the wrapper
//!   surfaces the last `Err` after `max_attempts`.
//! - **`compute_backoff` semantics** — pure-function unit test,
//!   no network needed.

#![cfg(all(feature = "net", feature = "cortex"))]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::mesh_rpc::{CallOptionsTyped, Codec, RpcError};
use net_sdk::mesh_rpc_resilience::{default_retryable, RetryPolicy};
use serde::{Deserialize, Serialize};

async fn two_meshes(psk: &[u8; 32]) -> (Mesh, Mesh, std::net::SocketAddr) {
    let a = MeshBuilder::new("127.0.0.1:0", psk)
        .unwrap()
        .build()
        .await
        .unwrap();
    let b = MeshBuilder::new("127.0.0.1:0", psk)
        .unwrap()
        .build()
        .await
        .unwrap();
    let addr_b = b.inner().local_addr();
    (a, b, addr_b)
}

async fn handshake(a: &Mesh, b: &Mesh, addr_b: std::net::SocketAddr) {
    let pub_b = *b.inner().public_key();
    let nid_b = b.inner().node_id();
    let nid_a = a.inner().node_id();
    let (r1, r2) = tokio::join!(b.inner().accept(nid_a), async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        a.inner().connect(addr_b, &pub_b, nid_b).await
    });
    r1.expect("accept");
    r2.expect("connect");
    a.inner().start();
    b.inner().start();
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone)]
struct PingRequest {
    seq: u64,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone)]
struct PingResponse {
    pong: u64,
}

/// Server returns Internal for the first 2 calls, then Ok. The
/// retry wrapper must absorb the first 2 failures and surface the
/// 3rd attempt's success. Confirms (a) retry actually re-issues,
/// (b) `Internal` is in the default retryable set, (c) backoff
/// doesn't blow past a reasonable wall-clock window.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retry_eventually_succeeds_after_transient_failures() {
    let psk = [0x42u8; 32];
    let (caller, server, addr_server) = two_meshes(&psk).await;
    handshake(&caller, &server, addr_server).await;

    // Server: fail first 2 attempts with a string the default
    // predicate translates to `Internal`, then succeed on the 3rd.
    // Typed `Err(String)` from the handler maps to
    // `RpcStatus::Application(0x4001)`, which is NOT in the default
    // retryable set — so we use the raw `serve_rpc` path with
    // `RpcHandlerError::Internal` to get a retryable error.
    use async_trait::async_trait;
    use net_sdk::mesh_rpc::{
        RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus,
    };
    struct FailThenOk {
        calls: Arc<AtomicUsize>,
        fail_count: usize,
    }
    #[async_trait]
    impl RpcHandler for FailThenOk {
        async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n < self.fail_count {
                Err(RpcHandlerError::Internal(format!("transient failure #{n}")))
            } else {
                Ok(RpcResponsePayload {
                    status: RpcStatus::Ok,
                    headers: vec![],
                    body: ctx.payload.body,
                })
            }
        }
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let _serve = server
        .serve_rpc(
            "flaky",
            Arc::new(FailThenOk {
                calls: calls.clone(),
                fail_count: 2,
            }),
        )
        .expect("serve_rpc");

    let policy = RetryPolicy {
        max_attempts: 5,
        initial_backoff: Duration::from_millis(10),
        max_backoff: Duration::from_millis(80),
        backoff_multiplier: 2.0,
        jitter: true,
        ..Default::default()
    };

    let started = std::time::Instant::now();
    let reply = caller
        .call_with_retry(
            server.inner().node_id(),
            "flaky",
            bytes::Bytes::from_static(b"hello"),
            Default::default(),
            &policy,
        )
        .await
        .expect("retry must eventually succeed");
    let elapsed = started.elapsed();

    assert_eq!(reply.body.as_ref(), b"hello");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "handler must run exactly 3 times (2 failures + 1 success)",
    );
    // Sanity: total wall-clock should reflect ~the sum of backoffs
    // (10ms + 20ms ≈ 30ms minimum, with jitter ≥15ms), but bounded
    // well under any human-noticeable threshold.
    assert!(
        elapsed < Duration::from_secs(3),
        "elapsed wall-clock too large: {elapsed:?}",
    );
}

/// Typed handler returns `Err(String)` (= `Application(0x4001)`).
/// The default predicate does NOT retry application errors —
/// surfaces immediately after one attempt.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retry_does_not_retry_application_errors() {
    let psk = [0x42u8; 32];
    let (caller, server, addr_server) = two_meshes(&psk).await;
    handshake(&caller, &server, addr_server).await;

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_handler = calls.clone();
    let _serve = server
        .serve_rpc_typed("validate", Codec::Json, move |req: PingRequest| {
            let calls = calls_for_handler.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Err::<PingResponse, _>(format!("rejected seq {}", req.seq))
            }
        })
        .expect("serve_rpc_typed");

    let policy = RetryPolicy::default();
    let err = caller
        .call_typed_with_retry::<PingRequest, PingResponse>(
            server.inner().node_id(),
            "validate",
            &PingRequest { seq: 7 },
            CallOptionsTyped::default(),
            &policy,
        )
        .await
        .expect_err("validation failure must surface");
    match err {
        RpcError::ServerError { message, .. } => {
            assert!(
                message.contains("rejected seq 7"),
                "diagnostic must round-trip, got {message:?}",
            );
        }
        other => panic!("expected ServerError, got {other:?}"),
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "application error must NOT trigger retry",
    );
}

/// Server fails every attempt. After `max_attempts`, the wrapper
/// surfaces the last error (not a synthetic "exhausted" wrapper —
/// the underlying `RpcError` round-trips so the caller can pattern-
/// match on it).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retry_exhaustion_surfaces_last_error() {
    let psk = [0x42u8; 32];
    let (caller, server, addr_server) = two_meshes(&psk).await;
    handshake(&caller, &server, addr_server).await;

    use async_trait::async_trait;
    use net_sdk::mesh_rpc::{RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload};
    struct AlwaysInternal {
        calls: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl RpcHandler for AlwaysInternal {
        async fn call(&self, _ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(RpcHandlerError::Internal("always".into()))
        }
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let _serve = server
        .serve_rpc(
            "doomed",
            Arc::new(AlwaysInternal {
                calls: calls.clone(),
            }),
        )
        .expect("serve_rpc");

    let policy = RetryPolicy {
        max_attempts: 3,
        initial_backoff: Duration::from_millis(5),
        max_backoff: Duration::from_millis(20),
        backoff_multiplier: 2.0,
        jitter: false,
        ..Default::default()
    };

    let err = caller
        .call_with_retry(
            server.inner().node_id(),
            "doomed",
            bytes::Bytes::from_static(b""),
            Default::default(),
            &policy,
        )
        .await
        .expect_err("must exhaust");
    assert!(
        matches!(
            err,
            RpcError::ServerError { ref message, .. } if message.contains("always"),
        ),
        "must surface the underlying server error, got {err:?}",
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "handler must run exactly max_attempts times",
    );
}

/// Regression: a typed call whose response body cannot be decoded
/// into the caller's `Resp` type surfaces as `RpcError::Codec` and
/// is NOT retried by the default predicate. Without this guarantee,
/// a permanent local schema-drift bug burns the full retry budget
/// (and trips the circuit breaker) on a deterministic local fault.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn codec_decode_failure_is_not_retried() {
    let psk = [0x42u8; 32];
    let (caller, server, addr_server) = two_meshes(&psk).await;
    handshake(&caller, &server, addr_server).await;

    // Server returns a response body that does NOT round-trip as
    // `PingResponse`. Each call increments the handler counter so
    // we can assert the retry layer issued exactly one wire call.
    use async_trait::async_trait;
    use net_sdk::mesh_rpc::{
        RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus,
    };
    struct ReturnsBadShape {
        calls: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl RpcHandler for ReturnsBadShape {
        async fn call(&self, _ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            // Valid JSON, but a shape `PingResponse { pong: u64 }`
            // can't decode (wrong field, wrong type).
            Ok(RpcResponsePayload {
                status: RpcStatus::Ok,
                headers: vec![],
                body: bytes::Bytes::from_static(br#"{"unexpected":"string"}"#),
            })
        }
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let _serve = server
        .serve_rpc(
            "bad_shape",
            Arc::new(ReturnsBadShape {
                calls: calls.clone(),
            }),
        )
        .expect("serve_rpc");

    let policy = RetryPolicy {
        max_attempts: 5,
        initial_backoff: Duration::from_millis(5),
        max_backoff: Duration::from_millis(20),
        backoff_multiplier: 2.0,
        jitter: false,
        ..Default::default()
    };
    let err = caller
        .call_typed_with_retry::<PingRequest, PingResponse>(
            server.inner().node_id(),
            "bad_shape",
            &PingRequest { seq: 1 },
            CallOptionsTyped::default(),
            &policy,
        )
        .await
        .expect_err("decode must fail");
    assert!(
        matches!(err, RpcError::Codec { .. }),
        "decode failure must surface as RpcError::Codec, got {err:?}",
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "codec failures must NOT trigger retry — got {} attempts",
        calls.load(Ordering::SeqCst),
    );
}

/// `default_retryable` correctly classifies each `RpcError` shape.
#[test]
fn default_retryable_classifies_canonical_errors() {
    use net_sdk::mesh_rpc::RpcStatus;

    // No-route → never retry (target is gone or never existed).
    assert!(!default_retryable(&RpcError::NoRoute {
        target: 0,
        reason: "x".into(),
    }));
    // Timeout / Transport → always retry.
    assert!(default_retryable(&RpcError::Timeout { elapsed_ms: 100 }));
    // ServerError(Internal / Backpressure / Timeout) → retry.
    assert!(default_retryable(&RpcError::ServerError {
        status: RpcStatus::Internal.to_wire(),
        message: "x".into(),
    }));
    assert!(default_retryable(&RpcError::ServerError {
        status: RpcStatus::Backpressure.to_wire(),
        message: "x".into(),
    }));
    assert!(default_retryable(&RpcError::ServerError {
        status: RpcStatus::Timeout.to_wire(),
        message: "x".into(),
    }));
    // ServerError(Application / NotFound / Unauthorized / etc.) → not retry.
    assert!(!default_retryable(&RpcError::ServerError {
        status: RpcStatus::Application(net_sdk::mesh_rpc::NRPC_TYPED_HANDLER_ERROR).to_wire(),
        message: "x".into(),
    }));
    assert!(!default_retryable(&RpcError::ServerError {
        status: RpcStatus::NotFound.to_wire(),
        message: "x".into(),
    }));
    assert!(!default_retryable(&RpcError::ServerError {
        status: RpcStatus::Unauthorized.to_wire(),
        message: "x".into(),
    }));
    assert!(!default_retryable(&RpcError::ServerError {
        status: RpcStatus::UnknownVersion.to_wire(),
        message: "x".into(),
    }));
    // Codec failures are caller-fixable bugs (wrong codec, schema
    // drift, malformed Serialize/Deserialize impl). Retrying just
    // burns the backoff budget on the same deterministic failure.
    assert!(!default_retryable(&RpcError::Codec {
        direction: net_sdk::mesh_rpc::CodecDirection::Encode,
        message: "non-finite f64".into(),
    }));
    assert!(!default_retryable(&RpcError::Codec {
        direction: net_sdk::mesh_rpc::CodecDirection::Decode,
        message: "trailing garbage".into(),
    }));
}
