//! nRPC quickstart — echo + add, two `Mesh` instances in one
//! process, real network handshake.
//!
//! ```text
//! cargo run --example nrpc_echo --features net,cortex
//! ```
//!
//! What this demonstrates:
//!
//! - `Mesh::serve_rpc_typed` — register a typed handler on
//!   `<service>` with auto-deserde of the request payload.
//! - `Mesh::call_typed` — direct call to a known target node id.
//! - `Mesh::call_service_typed` — discovery-driven call (the
//!   caller doesn't need to know the server's node id; the
//!   capability index resolves it via the `nrpc:<service>` tag).
//! - Per-call routing policy (`Sticky` / `RoundRobin` / `Random` /
//!   `LowestLatency`).
//! - Server returning a structured `Err` — surfaces as
//!   `RpcError::ServerError` with the message in the body.
//!
//! Two nodes run in this process for the demo, but the same code
//! shape works for nodes on different hosts. Replace the
//! `127.0.0.1:0` bind addresses with real network addresses + the
//! shared PSK and you have a multi-host RPC mesh.

use std::time::Duration;

use net_sdk::capabilities::CapabilitySet;
use net_sdk::mesh::MeshBuilder;
use net_sdk::mesh_rpc::{CallOptions, CallOptionsTyped, Codec, RoutingPolicy, RpcError};
use serde::{Deserialize, Serialize};

/// Request type for the `echo` service. Could be any
/// `Serialize + Deserialize` type — JSON over the wire.
#[derive(Debug, Serialize, Deserialize, Clone)]
struct EchoRequest {
    message: String,
}

/// Response type for the `echo` service.
#[derive(Debug, Serialize, Deserialize)]
struct EchoResponse {
    echoed: String,
    server_label: String,
}

/// Request type for the `add` service.
#[derive(Debug, Serialize, Deserialize)]
struct AddRequest {
    a: i64,
    b: i64,
}

/// Response type for the `add` service.
#[derive(Debug, Serialize, Deserialize)]
struct AddResponse {
    sum: i64,
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> net_sdk::error::Result<()> {
    let psk = [0x42u8; 32];

    // ──────────────────────────────────────────────────────────────
    // Set up two mesh nodes — server + caller — and handshake them.
    // In a real deployment each node would be a separate process or
    // host; the API shape is identical.
    // ──────────────────────────────────────────────────────────────
    let server = MeshBuilder::new("127.0.0.1:0", &psk)?.build().await?;
    let caller = MeshBuilder::new("127.0.0.1:0", &psk)?.build().await?;
    println!("server: node_id={:#x}", server.inner().node_id());
    println!("caller: node_id={:#x}", caller.inner().node_id());

    // Direct handshake. `tokio::join!` lets the two halves of the
    // handshake run concurrently without requiring `'static`
    // lifetimes (which `tokio::spawn` would).
    let server_addr = server.inner().local_addr();
    let server_pub = *server.inner().public_key();
    let server_id = server.inner().node_id();
    let caller_id = caller.inner().node_id();
    let (accept_res, connect_res) = tokio::join!(server.inner().accept(caller_id), async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        caller
            .inner()
            .connect(server_addr, &server_pub, server_id)
            .await
    });
    accept_res.map_err(|e| net_sdk::error::SdkError::Config(format!("accept: {e}")))?;
    connect_res.map_err(|e| net_sdk::error::SdkError::Config(format!("connect: {e}")))?;
    server.inner().start();
    caller.inner().start();

    // ──────────────────────────────────────────────────────────────
    // Register two typed RPC services on the server. The handler
    // closure takes a typed Req and returns Result<Resp, String>;
    // the SDK auto serde via the JSON codec.
    // ──────────────────────────────────────────────────────────────
    let _serve_echo = server
        .serve_rpc_typed("echo", Codec::Json, |req: EchoRequest| async move {
            Ok(EchoResponse {
                echoed: req.message,
                server_label: "primary".to_string(),
            })
        })
        .map_err(|e| net_sdk::error::SdkError::Config(format!("serve echo: {e}")))?;

    let _serve_add = server
        .serve_rpc_typed("add", Codec::Json, |req: AddRequest| async move {
            if req.a < 0 || req.b < 0 {
                Err(format!(
                    "this demo refuses negative inputs: a={}, b={}",
                    req.a, req.b
                ))
            } else {
                Ok(AddResponse { sum: req.a + req.b })
            }
        })
        .map_err(|e| net_sdk::error::SdkError::Config(format!("serve add: {e}")))?;

    // Announce capabilities so the caller can discover the
    // services via `find_service_nodes` / `call_service_typed`.
    // The SDK auto-merges the registered services as
    // `nrpc:<service>` tags on the announced CapabilitySet.
    server
        .inner()
        .announce_capabilities(CapabilitySet::new())
        .await
        .map_err(|e| net_sdk::error::SdkError::Config(format!("announce: {e}")))?;

    // Wait for the caller's capability index to learn about both
    // services.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        if !caller.find_service_nodes("echo").is_empty()
            && !caller.find_service_nodes("add").is_empty()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    println!(
        "discovered: echo on {} node(s), add on {} node(s)",
        caller.find_service_nodes("echo").len(),
        caller.find_service_nodes("add").len(),
    );

    // ──────────────────────────────────────────────────────────────
    // Demo 1: direct-addressed `call_typed`. Caller specifies the
    // target node id explicitly — useful when the topology is
    // known ahead of time (e.g. dedicated server with a static
    // address).
    // ──────────────────────────────────────────────────────────────
    let resp: EchoResponse = caller
        .call_typed(
            server.inner().node_id(),
            "echo",
            &EchoRequest {
                message: "hello, mesh".to_string(),
            },
            CallOptionsTyped::default(),
        )
        .await
        .map_err(|e| net_sdk::error::SdkError::Config(format!("call echo: {e}")))?;
    println!(
        "direct call -> echo: {} (from {})",
        resp.echoed, resp.server_label
    );

    // ──────────────────────────────────────────────────────────────
    // Demo 2: discovery-driven `call_service_typed`. Caller just
    // names the service; the capability index picks a server.
    // ──────────────────────────────────────────────────────────────
    let resp: AddResponse = caller
        .call_service_typed(
            "add",
            &AddRequest { a: 5, b: 7 },
            CallOptionsTyped {
                raw: CallOptions {
                    routing_policy: RoutingPolicy::RoundRobin,
                    ..Default::default()
                },
                codec: Codec::Json,
            },
        )
        .await
        .map_err(|e| net_sdk::error::SdkError::Config(format!("call add: {e}")))?;
    println!("discovery call -> add(5, 7) = {}", resp.sum);

    // ──────────────────────────────────────────────────────────────
    // Demo 3: structured handler error. The server's `Err(String)`
    // surfaces to the caller as `RpcError::ServerError` with the
    // diagnostic in the body.
    // ──────────────────────────────────────────────────────────────
    let err = caller
        .call_service_typed::<AddRequest, AddResponse>(
            "add",
            &AddRequest { a: -1, b: 7 },
            CallOptionsTyped::default(),
        )
        .await
        .expect_err("negative input must fail");
    match err {
        RpcError::ServerError { message, .. } => {
            println!("expected error caught: {message}");
        }
        other => println!("unexpected error shape: {other:?}"),
    }

    println!("done.");
    Ok(())
}
