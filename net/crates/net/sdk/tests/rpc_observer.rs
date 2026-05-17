//! Smoke tests for `Mesh::set_rpc_observer` — Item D of
//! `DECK_DEMO_HARNESS_PLAN.md`. v1 covers caller-side firing
//! only.
//!
//! Mirrors the setup pattern in `mesh_rpc_typed.rs`: two
//! `Mesh` instances on loopback, real Noise handshake, a typed
//! `add` handler on one side. The observer collects every
//! fired event into a shared `Mutex<Vec<...>>`; the test asserts
//! the per-call fields (caller, callee, method, status, byte
//! counts) line up with the call shape.

#![cfg(all(feature = "net", feature = "cortex"))]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::mesh_rpc::{
    CallOptionsTyped, Codec, RpcCallEvent, RpcCallStatus, RpcDirection, RpcError, RpcObserver,
};
use serde::{Deserialize, Serialize};

#[derive(Default)]
struct CollectingObserver {
    events: Mutex<Vec<RpcCallEvent>>,
}

impl RpcObserver for CollectingObserver {
    fn on_call(&self, evt: RpcCallEvent) {
        self.events.lock().unwrap().push(evt);
    }
}

impl CollectingObserver {
    fn snapshot(&self) -> Vec<RpcCallEvent> {
        self.events.lock().unwrap().clone()
    }
}

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
struct AddRequest {
    a: i64,
    b: i64,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct AddResponse {
    sum: i64,
}

/// Successful call fires the observer with `RpcCallStatus::Ok`
/// and non-zero request + response byte counts.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn observer_fires_on_successful_call() {
    let psk = [0xA1u8; 32];
    let (caller, server, addr_server) = two_meshes(&psk).await;
    handshake(&caller, &server, addr_server).await;

    let observer: Arc<CollectingObserver> = Arc::new(CollectingObserver::default());
    caller.set_rpc_observer(Some(observer.clone()));

    let _serve = server
        .serve_rpc_typed("add", Codec::Json, |req: AddRequest| async move {
            Ok(AddResponse { sum: req.a + req.b })
        })
        .expect("serve_rpc_typed");

    let resp: AddResponse = caller
        .call_typed(
            server.inner().node_id(),
            "add",
            &AddRequest { a: 2, b: 3 },
            CallOptionsTyped::default(),
        )
        .await
        .expect("call_typed must succeed");
    assert_eq!(resp.sum, 5);

    let events = observer.snapshot();
    assert_eq!(
        events.len(),
        1,
        "expected one observer event, got {events:?}"
    );
    let evt = &events[0];
    assert_eq!(evt.caller, caller.node_id());
    assert_eq!(evt.callee, server.node_id());
    assert_eq!(evt.method, "add");
    assert_eq!(evt.direction, RpcDirection::Outbound);
    assert_eq!(evt.status, RpcCallStatus::Ok);
    assert!(evt.request_bytes > 0, "request bytes should be non-zero");
    assert!(evt.response_bytes > 0, "response bytes should be non-zero");
    assert!(evt.ts_unix_ms > 0, "ts_unix_ms should be set");
}

/// Server-returned error surfaces as `RpcCallStatus::Error`,
/// with the error message round-tripped through the event.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn observer_fires_on_server_error() {
    let psk = [0xA2u8; 32];
    let (caller, server, addr_server) = two_meshes(&psk).await;
    handshake(&caller, &server, addr_server).await;

    let observer: Arc<CollectingObserver> = Arc::new(CollectingObserver::default());
    caller.set_rpc_observer(Some(observer.clone()));

    let _serve = server
        .serve_rpc_typed("validate", Codec::Json, |_req: AddRequest| async move {
            Err::<AddResponse, _>("negative a not allowed".to_string())
        })
        .expect("serve_rpc_typed");

    let err = caller
        .call_typed::<AddRequest, AddResponse>(
            server.inner().node_id(),
            "validate",
            &AddRequest { a: -1, b: 2 },
            CallOptionsTyped::default(),
        )
        .await
        .expect_err("validation failure must surface");
    match err {
        RpcError::ServerError { .. } => {}
        other => panic!("expected ServerError, got {other:?}"),
    }

    let events = observer.snapshot();
    assert_eq!(events.len(), 1);
    let evt = &events[0];
    assert_eq!(evt.method, "validate");
    match &evt.status {
        RpcCallStatus::Error(msg) => {
            assert!(
                msg.contains("negative a"),
                "diagnostic should round-trip; got {msg:?}"
            );
        }
        other => panic!("expected Error status, got {other:?}"),
    }
}

/// Clearing the observer (`set_rpc_observer(None)`) stops
/// further events from being recorded.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cleared_observer_does_not_fire() {
    let psk = [0xA3u8; 32];
    let (caller, server, addr_server) = two_meshes(&psk).await;
    handshake(&caller, &server, addr_server).await;

    let observer: Arc<CollectingObserver> = Arc::new(CollectingObserver::default());
    caller.set_rpc_observer(Some(observer.clone()));

    let _serve = server
        .serve_rpc_typed("add", Codec::Json, |req: AddRequest| async move {
            Ok(AddResponse { sum: req.a + req.b })
        })
        .expect("serve_rpc_typed");

    let _: AddResponse = caller
        .call_typed(
            server.inner().node_id(),
            "add",
            &AddRequest { a: 1, b: 1 },
            CallOptionsTyped::default(),
        )
        .await
        .unwrap();
    assert_eq!(observer.snapshot().len(), 1);

    caller.set_rpc_observer(None);

    let _: AddResponse = caller
        .call_typed(
            server.inner().node_id(),
            "add",
            &AddRequest { a: 4, b: 4 },
            CallOptionsTyped::default(),
        )
        .await
        .unwrap();
    // Same snapshot length — the second call's observer fire
    // was suppressed.
    assert_eq!(observer.snapshot().len(), 1);
}

/// When no observer is installed, the call path runs without
/// regression. (Smoke test that the new firing branches don't
/// break the existing surface.)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn call_works_with_no_observer_installed() {
    let psk = [0xA4u8; 32];
    let (caller, server, addr_server) = two_meshes(&psk).await;
    handshake(&caller, &server, addr_server).await;

    let _serve = server
        .serve_rpc_typed("add", Codec::Json, |req: AddRequest| async move {
            Ok(AddResponse { sum: req.a + req.b })
        })
        .expect("serve_rpc_typed");

    let resp: AddResponse = caller
        .call_typed(
            server.inner().node_id(),
            "add",
            &AddRequest { a: 10, b: 20 },
            CallOptionsTyped::default(),
        )
        .await
        .expect("call_typed must succeed even without observer");
    assert_eq!(resp.sum, 30);
}
