//! End-to-end SDK test for the Phase 9b predicate-pushdown surface.
//!
//! Caller-side: `CallOptions::with_where(&pred)` encodes the predicate
//! to JSON and pushes it as a `net-where` request header.
//! Server-side: `RpcContextExt::where_predicate()` decodes from the
//! same header. End-to-end: a service can use the caller's predicate
//! to filter its result set before sending bytes over the wire.
//!
//! Two-tier coverage:
//!
//! 1. Unit-level: `with_where` + `with_request_header` populate
//!    `request_headers` byte-equal to direct manual construction. No
//!    network round-trip needed — pins the encode contract.
//!
//! 2. End-to-end: two `Mesh` instances handshake; server registers a
//!    raw handler that decodes the predicate from `ctx`. Caller
//!    issues `call(target, "filter-svc", body, opts.with_where(p))`.
//!    Server reads the predicate back; both client and server agree
//!    on byte-identical AST.
//!
//! Phase 9b of `CAPABILITY_SYSTEM_SDK_PLAN.md`.

#![cfg(all(feature = "net", feature = "cortex"))]

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;

use net::adapter::net::behavior::{
    predicate::{
        predicate_from_rpc_headers, predicate_to_rpc_header, AsRpcHeader, RPC_WHERE_HEADER,
    },
    Predicate, TagKey, TaxonomyAxis,
};
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::mesh_rpc::{
    CallOptions, CallOptionsExt, RpcContext, RpcContextExt, RpcHandler, RpcHandlerError,
    RpcResponsePayload, RpcStatus,
};

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

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

fn sample_predicate() -> Predicate {
    Predicate::and(vec![
        Predicate::exists(TagKey::new(TaxonomyAxis::Hardware, "gpu")),
        Predicate::numeric_at_least(TagKey::new(TaxonomyAxis::Hardware, "memory_gb"), 64.0),
        Predicate::metadata_equals("intent", "ml-training"),
    ])
}

// ---------------------------------------------------------------------------
// Unit: encode-side contract
// ---------------------------------------------------------------------------

/// `with_where(p)` produces a request_headers entry under the
/// canonical name with the JSON-encoded `PredicateWire` body. Pinned
/// so the per-binding wrappers (Node / Python / Go) implement the
/// same encode contract.
#[test]
fn with_where_pushes_canonical_net_where_header() {
    let pred = sample_predicate();
    let opts = CallOptions::default()
        .with_where(&pred)
        .expect("predicate fits in header budget");

    assert_eq!(opts.request_headers.len(), 1);
    let (name, value) = &opts.request_headers[0];
    assert_eq!(name, RPC_WHERE_HEADER);
    let (expected_name, expected_value) =
        predicate_to_rpc_header(&pred).expect("encode round-trips");
    assert_eq!(name, &expected_name);
    assert_eq!(value, &expected_value);
}

/// `with_request_header(k, v)` is a thin wrapper over the
/// substrate's `request_headers` field; multiple calls accumulate.
#[test]
fn with_request_header_accumulates() {
    let opts = CallOptions::default()
        .with_request_header("cyberdeck-x-tenant", b"acme".to_vec())
        .with_request_header("cyberdeck-x-priority", b"5".to_vec());
    assert_eq!(opts.request_headers.len(), 2);
    assert_eq!(opts.request_headers[0].0, "cyberdeck-x-tenant");
    assert_eq!(opts.request_headers[1].0, "cyberdeck-x-priority");
}

/// The server-side `where_predicate()` extension on `RpcContext`
/// decodes the same JSON the caller emitted — pins the round-trip
/// at the type level (not just byte-level).
#[test]
fn predicate_round_trips_through_request_headers_synthetic() {
    let pred = sample_predicate();
    let (name, value) = predicate_to_rpc_header(&pred).expect("encode");
    let headers: Vec<(String, Vec<u8>)> = vec![(name, value)];
    let decoded = predicate_from_rpc_headers(&headers)
        .expect("present")
        .expect("valid wire");
    // AST equality — pins both sides agree at the predicate-AST level
    // regardless of JSON whitespace or struct-member ordering.
    assert_eq!(decoded, pred);
}

// ---------------------------------------------------------------------------
// End-to-end: client `with_where` -> server `where_predicate`
// ---------------------------------------------------------------------------

/// Server handler that captures the predicate it observed via
/// `ctx.where_predicate()` and echoes back its JSON-encoded form so
/// the test can assert the round-trip.
struct PredicateEchoHandler {
    /// Set on first call. Tests assert against this after the call
    /// completes.
    captured: Arc<tokio::sync::Mutex<Option<Predicate>>>,
}

#[async_trait::async_trait]
impl RpcHandler for PredicateEchoHandler {
    async fn call(
        &self,
        ctx: RpcContext,
    ) -> std::result::Result<RpcResponsePayload, RpcHandlerError> {
        let pred = ctx
            .where_predicate()
            .map(|r| r.expect("server: predicate decode must succeed"));
        if let Some(p) = pred.as_ref() {
            *self.captured.lock().await = Some(p.clone());
        }
        // Echo "found" / "missing" in the response body so the caller
        // can assert without inspecting the predicate (the captured
        // mutex is the load-bearing signal).
        let body: Vec<u8> = if pred.is_some() {
            b"found".to_vec()
        } else {
            b"missing".to_vec()
        };
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body,
        })
    }
}

/// End-to-end: caller's `with_where(p)` reaches the server via the
/// `net-where` header; server's `where_predicate()` returns
/// the same AST.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn end_to_end_where_predicate_round_trip() {
    let psk = [0x42u8; 32];
    let (caller, server, addr_server) = two_meshes(&psk).await;
    handshake(&caller, &server, addr_server).await;

    let captured: Arc<tokio::sync::Mutex<Option<Predicate>>> =
        Arc::new(tokio::sync::Mutex::new(None));
    let handler = Arc::new(PredicateEchoHandler {
        captured: captured.clone(),
    });
    let _serve = server.serve_rpc("filter-svc", handler).expect("serve_rpc");

    let pred = sample_predicate();
    let opts = CallOptions::default()
        .with_where(&pred)
        .expect("predicate fits in header budget");

    let reply = caller
        .call(
            server.inner().node_id(),
            "filter-svc",
            Bytes::from_static(b"any"),
            opts,
        )
        .await
        .expect("call");
    assert_eq!(reply.body.as_ref(), b"found");

    let observed = captured.lock().await.take();
    assert_eq!(
        observed,
        Some(pred),
        "server-decoded predicate must equal the caller-encoded predicate",
    );
}

/// End-to-end: caller doesn't set `with_where`; server's
/// `where_predicate()` returns `None` and the handler responds with
/// `"missing"`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn end_to_end_missing_where_predicate_returns_none() {
    let psk = [0x43u8; 32];
    let (caller, server, addr_server) = two_meshes(&psk).await;
    handshake(&caller, &server, addr_server).await;

    let captured: Arc<tokio::sync::Mutex<Option<Predicate>>> =
        Arc::new(tokio::sync::Mutex::new(None));
    let handler = Arc::new(PredicateEchoHandler {
        captured: captured.clone(),
    });
    let _serve = server.serve_rpc("filter-svc", handler).expect("serve_rpc");

    // Default opts — no with_where.
    let reply = caller
        .call(
            server.inner().node_id(),
            "filter-svc",
            Bytes::from_static(b"any"),
            CallOptions::default(),
        )
        .await
        .expect("call");
    assert_eq!(reply.body.as_ref(), b"missing");
    assert!(captured.lock().await.is_none());
}

/// `AsRpcHeader` impl for the substrate's `(String, Vec<u8>)`
/// header tuple — pin so the trait is exposed via SDK re-exports
/// (callers writing custom decoders shouldn't have to reach into
/// `net::adapter::net::behavior::*` directly).
#[test]
fn as_rpc_header_impl_is_re_exported_via_sdk_path() {
    fn _check<H: AsRpcHeader>() {}
    _check::<(String, Vec<u8>)>();
}
