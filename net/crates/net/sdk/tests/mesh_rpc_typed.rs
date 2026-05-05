//! End-to-end SDK test for the typed nRPC surface.
//!
//! Two `Mesh` instances built via `MeshBuilder` (the public SDK
//! entry point), connected via direct handshake. Server registers
//! a typed handler via `serve_rpc_typed`; caller invokes via
//! `call_typed` and `call_service_typed`. Pinned: round-trip,
//! handler error mapping, malformed body short-circuit.
//!
//! Verifies the SDK's auto-registration of the request channel +
//! reply-channel prefix unblocks the dynamic per-caller reply
//! subscriptions.

#![cfg(all(feature = "net", feature = "cortex"))]

use std::time::Duration;

use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::mesh_rpc::{
    CallOptionsTyped, Codec, RoutingPolicy, RpcError,
};
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
struct AddRequest {
    a: i64,
    b: i64,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct AddResponse {
    sum: i64,
}

/// Round-trip through the network. Server registers typed handler
/// via `serve_rpc_typed`; caller invokes via `call_typed`. Both
/// sides go through `MeshBuilder` (the public SDK entry point).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn typed_rpc_round_trip_via_call_typed() {
    let psk = [0x42u8; 32];
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
            &AddRequest { a: 5, b: 7 },
            CallOptionsTyped::default(),
        )
        .await
        .expect("call_typed must succeed");
    assert_eq!(resp.sum, 12);
}

/// Server returns `Err(message)` from the typed handler — the SDK
/// surfaces it as `RpcError::ServerError` with the message in the
/// body. Pin: structured error reaches the caller-side future,
/// not a panic / hang.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn typed_rpc_handler_error_surfaces_to_caller() {
    let psk = [0x42u8; 32];
    let (caller, server, addr_server) = two_meshes(&psk).await;
    handshake(&caller, &server, addr_server).await;

    let _serve = server
        .serve_rpc_typed("validate", Codec::Json, |req: AddRequest| async move {
            if req.a < 0 {
                Err(format!("negative a not allowed: {}", req.a))
            } else {
                Ok(AddResponse { sum: req.a + req.b })
            }
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
        .expect_err("validation failure must surface as Err");
    match err {
        RpcError::ServerError { message, .. } => {
            assert!(
                message.contains("negative a"),
                "diagnostic must round-trip; got {message:?}",
            );
        }
        other => panic!("expected ServerError, got {other:?}"),
    }
}

/// `call_service_typed` consults the capability index to find a
/// server advertising `nrpc:multiply`. Both sides go through the
/// SDK; the auto-registered prefix admits the dynamic reply
/// subscription.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn typed_call_service_uses_capability_announcements() {
    use net_sdk::capabilities::CapabilitySet;

    let psk = [0x42u8; 32];
    let (caller, server, addr_server) = two_meshes(&psk).await;
    handshake(&caller, &server, addr_server).await;

    let _serve = server
        .serve_rpc_typed(
            "multiply",
            Codec::Json,
            |req: AddRequest| async move { Ok(AddResponse { sum: req.a * req.b }) },
        )
        .expect("serve_rpc_typed");

    server
        .inner()
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("announce");

    // Wait for the caller's index to learn about the service.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        if !caller.find_service_nodes("multiply").is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        !caller.find_service_nodes("multiply").is_empty(),
        "caller must discover the service via announcement"
    );

    let resp: AddResponse = caller
        .call_service_typed(
            "multiply",
            &AddRequest { a: 6, b: 7 },
            CallOptionsTyped {
                raw: net_sdk::mesh_rpc::CallOptions {
                    routing_policy: RoutingPolicy::RoundRobin,
                    ..Default::default()
                },
                codec: Codec::Json,
            },
        )
        .await
        .expect("call_service_typed");
    assert_eq!(resp.sum, 42);
}

/// `Codec::Json` round-trips primitive values without surprises.
#[test]
fn codec_round_trip() {
    let bytes = Codec::Json.encode(&42u32).unwrap();
    let back: u32 = Codec::Json.decode(&bytes).unwrap();
    assert_eq!(back, 42);

    let bytes = Codec::Json.encode(&"hello").unwrap();
    let back: String = Codec::Json.decode(&bytes).unwrap();
    assert_eq!(back, "hello");

    // Pretty round-trips identically (same wire format, just
    // formatted differently on encode).
    let pretty = Codec::JsonPretty.encode(&AddRequest { a: 1, b: 2 }).unwrap();
    let back: AddRequest = Codec::JsonPretty.decode(&pretty).unwrap();
    assert_eq!(back, AddRequest { a: 1, b: 2 });
}
