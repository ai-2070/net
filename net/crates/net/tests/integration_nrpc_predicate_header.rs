//! End-to-end: client encodes a `Predicate` as the canonical
//! `net-where:` request header (Phase 9b), the server-side
//! handler decodes it and filters a candidate list with the
//! stateless evaluator (Phase 9c). Pins that the two SDK surfaces
//! actually compose over a real mesh — fixture-level wire tests
//! and per-piece unit tests cover the parts in isolation, but
//! nothing else proves the round-trip.
//!
//! Mirrors `integration_nrpc_mesh.rs`'s two-node setup; cortex
//! feature pulls in nRPC + the predicate evaluator.

#![cfg(all(feature = "net", feature = "cortex"))]

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use net::adapter::net::behavior::predicate::{
    predicate_from_rpc_headers, predicate_to_rpc_header, EvalContext, RPC_WHERE_HEADER,
};
use net::adapter::net::behavior::tag::{Tag, TaxonomyAxis};
use net::adapter::net::cortex::{
    RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus,
};
use net::adapter::net::mesh_rpc::{CallOptions, RpcError};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};
use net::pred;

const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x42u8; 32];

fn test_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(3, Duration::from_secs(2))
        .with_capability_gc_interval(Duration::from_millis(250));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: TEST_BUFFER_SIZE,
        recv_buffer_size: TEST_BUFFER_SIZE,
    };
    cfg
}

async fn build_node() -> Arc<MeshNode> {
    let cfg = test_config();
    let keypair = EntityKeypair::generate();
    Arc::new(MeshNode::new(keypair, cfg).await.expect("MeshNode::new"))
}

async fn handshake_pair(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    let a_id = a.node_id();
    let b_id = b.node_id();
    let b_pub = *b.public_key();
    let b_addr = b.local_addr();
    let b_clone = b.clone();
    let accept = tokio::spawn(async move { b_clone.accept(a_id).await });
    a.connect(b_addr, &b_pub, b_id)
        .await
        .expect("connect failed");
    accept
        .await
        .expect("accept task panicked")
        .expect("accept failed");
    a.start();
    b.start();
}

// ============================================================================
// Server side — `WhereFilterHandler` reads `net-where:` and
// returns the indices of candidates that match.
// ============================================================================

/// Synthetic candidate set the server filters through. In a real
/// deployment these would come from `CapabilityIndex` queries; the
/// integration we're pinning here is "header → predicate → eval",
/// not the index itself.
fn synthetic_corpus() -> Vec<(Vec<Tag>, BTreeMap<String, String>)> {
    fn tag_present(axis: TaxonomyAxis, key: &str) -> Tag {
        Tag::AxisPresent {
            axis,
            key: key.to_string(),
        }
    }
    fn tag_value(axis: TaxonomyAxis, key: &str, value: &str) -> Tag {
        Tag::AxisValue {
            axis,
            key: key.to_string(),
            value: value.to_string(),
            separator: net::adapter::net::behavior::tag::AxisSeparator::Eq,
        }
    }
    fn meta(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }
    vec![
        // 0: GPU, 64GB, ml-training intent — should match the test predicate.
        (
            vec![
                tag_present(TaxonomyAxis::Hardware, "gpu"),
                tag_value(TaxonomyAxis::Hardware, "memory_gb", "64"),
            ],
            meta(&[("intent", "ml-training")]),
        ),
        // 1: GPU but only 16GB — fails memory clause.
        (
            vec![
                tag_present(TaxonomyAxis::Hardware, "gpu"),
                tag_value(TaxonomyAxis::Hardware, "memory_gb", "16"),
            ],
            meta(&[("intent", "ml-training")]),
        ),
        // 2: 64GB but no GPU — fails Exists clause.
        (
            vec![tag_value(TaxonomyAxis::Hardware, "memory_gb", "64")],
            meta(&[("intent", "ml-training")]),
        ),
        // 3: GPU + 64GB but wrong intent — fails metadata clause.
        (
            vec![
                tag_present(TaxonomyAxis::Hardware, "gpu"),
                tag_value(TaxonomyAxis::Hardware, "memory_gb", "64"),
            ],
            meta(&[("intent", "billing")]),
        ),
    ]
}

/// Server-side handler. Returns a JSON-encoded `Vec<u32>` of
/// matching candidate indices. No header → empty result (caller
/// must opt in to filtering).
struct WhereFilterHandler;

#[async_trait::async_trait]
impl RpcHandler for WhereFilterHandler {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        let pred = match predicate_from_rpc_headers(&ctx.payload.headers) {
            Some(Ok(p)) => p,
            Some(Err(e)) => {
                // Malformed header — surface as application error so
                // the caller sees a typed status, not an internal panic.
                return Err(RpcHandlerError::Application {
                    code: 0xC000,
                    message: format!("predicate header decode failed: {e}"),
                });
            }
            None => {
                // No filter → empty matches. The caller must pass a
                // header to opt in. (Don't return all candidates —
                // that would mask a missing-header bug as a passing test.)
                return Ok(RpcResponsePayload {
                    status: RpcStatus::Ok,
                    headers: vec![],
                    body: serde_json::to_vec::<Vec<u32>>(&vec![])
                        .expect("empty json")
                        .into(),
                });
            }
        };
        let corpus = synthetic_corpus();
        let mut matches: Vec<u32> = Vec::new();
        for (idx, (tags, metadata)) in corpus.iter().enumerate() {
            let ctx = EvalContext::new(tags, metadata);
            if pred.evaluate(&ctx) {
                matches.push(idx as u32);
            }
        }
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: serde_json::to_vec(&matches).expect("matches json").into(),
        })
    }
}

// ============================================================================
// Tests
// ============================================================================

/// Round-trip: caller encodes a 3-clause AND predicate, server
/// decodes + evaluates against the synthetic corpus, returns the
/// indices that match. Exactly candidate 0 should pass.
#[tokio::test]
async fn predicate_header_round_trip_filters_corpus() {
    let server = build_node().await;
    let caller = build_node().await;
    handshake_pair(&caller, &server).await;

    let _serve = server
        .serve_rpc("where", Arc::new(WhereFilterHandler))
        .expect("serve_rpc");

    // Predicate: needs GPU AND memory ≥ 32 GB AND intent =
    // "ml-training". Only candidate 0 satisfies all three.
    let p = pred!(and [
        pred!(exists "hardware.gpu"),
        pred!(num_at_least "hardware.memory_gb", 32.0),
        pred!(metadata_equals "intent", "ml-training"),
    ]);
    let header = predicate_to_rpc_header(&p).expect("encode");
    assert_eq!(header.0, RPC_WHERE_HEADER);

    let opts = CallOptions {
        request_headers: vec![header],
        ..CallOptions::default()
    };
    let reply = caller
        .call(server.node_id(), "where", Bytes::new(), opts)
        .await
        .expect("call must succeed (Ok status surfaces as Ok(RpcReply))");

    let matches: Vec<u32> = serde_json::from_slice(&reply.body).expect("body decodes as Vec<u32>");
    assert_eq!(
        matches,
        vec![0],
        "only candidate 0 (GPU + 64 GB + ml-training) should match the AND predicate",
    );
}

/// Looser predicate: just needs the GPU. Candidates 0, 1, 3 match.
/// Pins that the predicate AST round-trips losslessly across the
/// header encode/decode boundary.
#[tokio::test]
async fn predicate_header_admits_multiple_candidates() {
    let server = build_node().await;
    let caller = build_node().await;
    handshake_pair(&caller, &server).await;

    let _serve = server
        .serve_rpc("where", Arc::new(WhereFilterHandler))
        .expect("serve_rpc");

    let p = pred!(exists "hardware.gpu");
    let header = predicate_to_rpc_header(&p).expect("encode");
    let opts = CallOptions {
        request_headers: vec![header],
        ..CallOptions::default()
    };
    let reply = caller
        .call(server.node_id(), "where", Bytes::new(), opts)
        .await
        .expect("call");
    let matches: Vec<u32> = serde_json::from_slice(&reply.body).expect("body");
    assert_eq!(
        matches,
        vec![0, 1, 3],
        "every GPU-bearing candidate matches"
    );
}

/// Caller omits the header — server returns empty matches. Pins
/// that the no-header path reaches the handler (rather than e.g.
/// the request being rejected upstream).
#[tokio::test]
async fn predicate_header_absent_returns_empty_matches() {
    let server = build_node().await;
    let caller = build_node().await;
    handshake_pair(&caller, &server).await;

    let _serve = server
        .serve_rpc("where", Arc::new(WhereFilterHandler))
        .expect("serve_rpc");

    let reply = caller
        .call(
            server.node_id(),
            "where",
            Bytes::new(),
            CallOptions::default(),
        )
        .await
        .expect("call");
    let matches: Vec<u32> = serde_json::from_slice(&reply.body).expect("body");
    assert!(matches.is_empty(), "no header → empty matches");
}

/// Malformed header — server surfaces it as an application error
/// rather than panicking or returning a misleading "no matches".
#[tokio::test]
async fn predicate_header_malformed_surfaces_typed_error() {
    let server = build_node().await;
    let caller = build_node().await;
    handshake_pair(&caller, &server).await;

    let _serve = server
        .serve_rpc("where", Arc::new(WhereFilterHandler))
        .expect("serve_rpc");

    let opts = CallOptions {
        request_headers: vec![(RPC_WHERE_HEADER.to_string(), b"{not-json".to_vec())],
        ..CallOptions::default()
    };
    // Application errors surface as `Err(RpcError::ServerError)` —
    // not a successful `RpcReply` with a non-Ok status. Pin both
    // the error variant and the diagnostic string.
    let err = caller
        .call(server.node_id(), "where", Bytes::new(), opts)
        .await
        .expect_err("malformed header must surface as a server error");
    match err {
        RpcError::ServerError {
            status, message, ..
        } => {
            assert_ne!(status, 0, "non-Ok wire status");
            assert!(
                message.contains("predicate header decode failed"),
                "diagnostic must carry the decode-failure message, got: {message}",
            );
        }
        other => panic!("expected ServerError, got: {other:?}"),
    }
}
