//! Shared in-process two-node harness for the nRPC benchmark
//! suite. Every bench file under `benches/nrpc_*.rs` uses these
//! helpers via `#[path = "nrpc_common/mod.rs"] mod nrpc_common;`.
//!
//! Why a shared module: setting up a real `Mesh` peer pair
//! (build + handshake + start + capability announce + discovery
//! wait) is ~30 lines and identical across every bench. Doing it
//! once per file means six near-identical copies that drift out
//! of sync; once here, every bench sees the same setup cost.
//!
//! The harness intentionally lives behind `benches/nrpc_common/`
//! (as a directory) rather than `benches/nrpc_common.rs` so
//! Cargo's bench auto-discovery doesn't pick it up as a bench
//! target. `autobenches = false` in `sdk/Cargo.toml` reinforces
//! this — each bench is registered explicitly with `[[bench]]`.

#![allow(dead_code)] // each bench uses only a subset of helpers

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use net_sdk::capabilities::CapabilitySet;
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::mesh_rpc::{
    CallOptions, CallOptionsTyped, Codec, RpcContext, RpcHandler, RpcHandlerError,
    RpcResponsePayload, RpcStatus,
};
use serde::{Deserialize, Serialize};
use tokio::runtime::{Builder as RtBuilder, Runtime};

// ============================================================================
// Service names. Each codec gets its own service so the same
// `Pair` can serve all three side by side and a bench can pick
// whichever it's measuring without re-registering.
// ============================================================================

pub const SVC_JSON: &str = "bench_echo_json";
pub const SVC_POSTCARD: &str = "bench_echo_postcard";
pub const SVC_RAW: &str = "bench_echo_raw";
pub const SVC_JSON_STREAM: &str = "bench_stream_json";
pub const SVC_JSON_CLIENT_STREAM: &str = "bench_client_stream_json";
pub const SVC_JSON_DUPLEX: &str = "bench_duplex_json";

// ============================================================================
// Echo wire types — the same logical `String` body across all
// three codecs so the comparison reflects codec cost, not payload
// shape. ASCII content keeps JSON honest (no Vec<u8> → JSON-array
// blow-up) while still letting postcard / raw deliver the same
// bytes.
// ============================================================================

#[derive(Serialize, Deserialize, Clone)]
pub struct EchoReq {
    pub body: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct EchoResp {
    pub body: String,
}

/// One ASCII byte ('A') repeated `n` times. Cheap to allocate,
/// stable across runs, no random-source overhead during the bench
/// loop.
pub fn payload(n: usize) -> String {
    "A".repeat(n)
}

// ============================================================================
// Pair — two `Mesh` nodes in one process, fully handshaken,
// every echo service pre-registered, discovery primed.
// ============================================================================

pub struct Pair {
    pub server: Mesh,
    pub caller: Mesh,
    pub server_node_id: u64,
    // Keep ServeHandles alive for the lifetime of the Pair. The
    // RPC dispatcher unregisters on Drop, so binding to `_` would
    // tear the service down immediately (see nrpc_echo.rs:98).
    _handles: Vec<net_sdk::mesh_rpc::ServeHandle>,
}

impl Pair {
    /// Build two `Mesh` instances on `127.0.0.1:0`, handshake
    /// them, register the three echo services, announce
    /// capabilities, and wait for the caller's capability index
    /// to learn about the JSON service (sentinel — all three
    /// land in the same announce).
    pub async fn new() -> Self {
        let psk = [0x42u8; 32];
        let server = MeshBuilder::new("127.0.0.1:0", &psk)
            .expect("builder")
            .build()
            .await
            .expect("server build");
        let caller = MeshBuilder::new("127.0.0.1:0", &psk)
            .expect("builder")
            .build()
            .await
            .expect("caller build");

        let server_addr = server.local_addr().to_string();
        let server_pub = *server.public_key();
        let server_id = server.node_id();
        let caller_id = caller.node_id();

        // Concurrent accept + connect — matches nrpc_echo.rs:81.
        let (accept_res, connect_res) = tokio::join!(server.accept(caller_id), async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            caller.connect(&server_addr, &server_pub, server_id).await
        });
        accept_res.expect("accept");
        connect_res.expect("connect");
        server.start();
        caller.start();

        // Three unary echo services — one per codec.
        let h_json = server
            .serve_rpc_typed(SVC_JSON, Codec::Json, |req: EchoReq| async move {
                Ok::<_, String>(EchoResp { body: req.body })
            })
            .expect("serve json");

        let h_post = server
            .serve_rpc(SVC_POSTCARD, Arc::new(PostcardEchoHandler))
            .expect("serve postcard");

        let h_raw = server
            .serve_rpc(SVC_RAW, Arc::new(RawEchoHandler))
            .expect("serve raw");

        // Server-streaming echo — emits the same body N times,
        // N from the request. Used by `nrpc_streaming.rs`.
        let h_stream = server
            .serve_rpc_streaming_typed(
                SVC_JSON_STREAM,
                Codec::Json,
                |req: StreamReq, sink| async move {
                    let item = EchoResp { body: req.body };
                    for _ in 0..req.count {
                        if sink.send(&item).is_err() {
                            break;
                        }
                    }
                    Ok::<_, String>(())
                },
            )
            .expect("serve stream");

        // Client-streaming echo — collects N typed requests and
        // returns a count. Used by the Phase F client-streaming
        // bench.
        let h_client_stream = server
            .serve_rpc_client_stream_typed(
                SVC_JSON_CLIENT_STREAM,
                Codec::Json,
                |mut requests: net_sdk::mesh_rpc::RequestStreamTyped<EchoReq>| async move {
                    use futures::StreamExt;
                    let mut count = 0u64;
                    while let Some(item) = requests.next().await {
                        std::hint::black_box(item.map_err(|e| format!("decode: {e}"))?);
                        count += 1;
                    }
                    Ok::<_, String>(EchoResp {
                        body: count.to_string(),
                    })
                },
            )
            .expect("serve client_stream");

        // Duplex echo — emits one Resp per inbound Req. Used by
        // the Phase F duplex bench.
        let h_duplex = server
            .serve_rpc_duplex_typed(
                SVC_JSON_DUPLEX,
                Codec::Json,
                |mut requests: net_sdk::mesh_rpc::RequestStreamTyped<EchoReq>, sink| async move {
                    use futures::StreamExt;
                    while let Some(item) = requests.next().await {
                        let item: EchoReq = item.map_err(|e| format!("decode: {e}"))?;
                        sink.send(&EchoResp { body: item.body })?;
                    }
                    Ok::<_, String>(())
                },
            )
            .expect("serve duplex");

        // Announce + wait for discovery — required for the
        // `call_service_typed` (discovery) path.
        server
            .inner()
            .announce_capabilities(CapabilitySet::new())
            .await
            .expect("announce");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            if !caller.find_service_nodes(SVC_JSON).is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            !caller.find_service_nodes(SVC_JSON).is_empty(),
            "discovery did not converge within 2s"
        );

        Self {
            server,
            caller,
            server_node_id: server_id,
            _handles: vec![h_json, h_post, h_raw, h_stream, h_client_stream, h_duplex],
        }
    }
}

// ============================================================================
// Raw handlers — postcard + identity. Postcard goes through the
// raw `serve_rpc` path because `Codec` only exposes Json /
// JsonPretty (mesh_rpc.rs:79-89); the bench manually encodes
// the same `EchoReq` struct with postcard on both ends.
// ============================================================================

struct PostcardEchoHandler;

#[async_trait::async_trait]
impl RpcHandler for PostcardEchoHandler {
    async fn call(
        &self,
        ctx: RpcContext,
    ) -> std::result::Result<RpcResponsePayload, RpcHandlerError> {
        let req: EchoReq = postcard::from_bytes(&ctx.payload.body)
            .map_err(|e| RpcHandlerError::Internal(format!("postcard decode: {e}")))?;
        let resp = EchoResp { body: req.body };
        let bytes = postcard::to_allocvec(&resp)
            .map_err(|e| RpcHandlerError::Internal(format!("postcard encode: {e}")))?;
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: bytes.into(),
        })
    }
}

struct RawEchoHandler;

#[async_trait::async_trait]
impl RpcHandler for RawEchoHandler {
    async fn call(
        &self,
        ctx: RpcContext,
    ) -> std::result::Result<RpcResponsePayload, RpcHandlerError> {
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: ctx.payload.body,
        })
    }
}

// ============================================================================
// Streaming request shape — payload + count of emissions. Lives
// here so both `nrpc_common` (handler registration) and
// `nrpc_streaming.rs` (call site) see the same type.
// ============================================================================

#[derive(Serialize, Deserialize, Clone)]
pub struct StreamReq {
    pub body: String,
    pub count: u32,
}

// ============================================================================
// Caller-side helpers. Each routing path × codec gets one fn that
// the bench loop can call without re-stating the encode/decode
// dance.
// ============================================================================

/// Direct `call_typed` with `Codec::Json`. Routing path: known
/// target node id, no capability-index lookup.
pub async fn call_json_direct(pair: &Pair, req: &EchoReq) -> EchoResp {
    pair.caller
        .call_typed(
            pair.server_node_id,
            SVC_JSON,
            req,
            CallOptionsTyped::default(),
        )
        .await
        .expect("call json direct")
}

/// Same as [`call_json_direct`] but retries on transient transport
/// backpressure (`RpcError::Transport(...)`). The per-stream publish
/// budget fills up either at high concurrency (`nrpc_qps` /
/// `nrpc_tail`) or when a single large unary body chunks past the
/// budget (`nrpc_payload`) — mesh_rpc.rs:1158 classifies these as
/// retriable on purpose. The bench yields after each backpressure
/// hit so the transport flushes and other in-flight callers make
/// progress; the resulting wall-clock latency reflects real
/// saturation behavior rather than masking it as a panic.
pub async fn call_json_direct_retrying(pair: &Pair, req: &EchoReq) -> EchoResp {
    use net_sdk::mesh_rpc::RpcError;
    loop {
        match pair
            .caller
            .call_typed::<_, EchoResp>(
                pair.server_node_id,
                SVC_JSON,
                req,
                CallOptionsTyped::default(),
            )
            .await
        {
            Ok(resp) => return resp,
            Err(RpcError::Transport(_)) => {
                tokio::task::yield_now().await;
            }
            Err(e) => panic!("call json direct (retrying): {e}"),
        }
    }
}

/// Discovery `call_service_typed` with `Codec::Json`. Routing
/// path: capability-index lookup picks the server.
pub async fn call_json_discovery(pair: &Pair, req: &EchoReq) -> EchoResp {
    pair.caller
        .call_service_typed(SVC_JSON, req, CallOptionsTyped::default())
        .await
        .expect("call json discovery")
}

/// Direct raw `call` with postcard encode/decode applied by the
/// bench. Skips the typed wrapper to dodge the `Codec` enum's
/// JSON-only scope.
pub async fn call_postcard_direct(pair: &Pair, req: &EchoReq) -> EchoResp {
    let body = postcard::to_allocvec(req).expect("postcard encode");
    let reply = pair
        .caller
        .call(
            pair.server_node_id,
            SVC_POSTCARD,
            Bytes::from(body),
            CallOptions::default(),
        )
        .await
        .expect("call postcard");
    postcard::from_bytes(&reply.body).expect("postcard decode")
}

/// Same as [`call_postcard_direct`] but retries on transient
/// transport backpressure, mirroring [`call_json_direct_retrying`].
/// Used by `nrpc_payload` where large bodies chunk past the
/// per-stream publish budget. Re-encoding per attempt is intentional:
/// a backpressured `call` published nothing the server accepted, so
/// the retry is a fresh call, not a duplicate.
pub async fn call_postcard_direct_retrying(pair: &Pair, req: &EchoReq) -> EchoResp {
    use net_sdk::mesh_rpc::RpcError;
    let body = postcard::to_allocvec(req).expect("postcard encode");
    loop {
        match pair
            .caller
            .call(
                pair.server_node_id,
                SVC_POSTCARD,
                Bytes::from(body.clone()),
                CallOptions::default(),
            )
            .await
        {
            Ok(reply) => return postcard::from_bytes(&reply.body).expect("postcard decode"),
            Err(RpcError::Transport(_)) => tokio::task::yield_now().await,
            Err(e) => panic!("call postcard (retrying): {e}"),
        }
    }
}

/// Direct raw `call` with no codec — body bytes round-trip
/// verbatim. The theoretical floor: every byte the bench
/// measures is genuine transport cost.
pub async fn call_raw_direct(pair: &Pair, body: Bytes) -> Bytes {
    pair.caller
        .call(pair.server_node_id, SVC_RAW, body, CallOptions::default())
        .await
        .expect("call raw")
        .body
}

/// Same as [`call_raw_direct`] but retries on transient transport
/// backpressure, mirroring [`call_json_direct_retrying`]. Used by
/// `nrpc_payload` where large bodies chunk past the per-stream
/// publish budget.
pub async fn call_raw_direct_retrying(pair: &Pair, body: Bytes) -> Bytes {
    use net_sdk::mesh_rpc::RpcError;
    loop {
        match pair
            .caller
            .call(
                pair.server_node_id,
                SVC_RAW,
                body.clone(),
                CallOptions::default(),
            )
            .await
        {
            Ok(reply) => return reply.body,
            Err(RpcError::Transport(_)) => tokio::task::yield_now().await,
            Err(e) => panic!("call raw (retrying): {e}"),
        }
    }
}

// ============================================================================
// Runtime constructor — multi-threaded tokio runtime used by
// every bench. 4 workers matches the existing test/example
// setup (nrpc_echo.rs:60).
//
// The worker-thread count is overridable via the
// `NRPC_BENCH_WORKER_THREADS` env var so the concurrency-scaling
// sweep (Phase 0a of NRPC_QPS_CONCURRENCY_SCALING_PLAN.md) can run
// 4 / 8 / 16 workers without a recompile. Both nodes of a `Pair`
// share this single runtime, so the count caps the cores available
// to client + server combined. Unset / unparseable → 4 (the
// baseline every committed bench number was taken at).
// ============================================================================

pub fn worker_threads() -> usize {
    std::env::var("NRPC_BENCH_WORKER_THREADS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(4)
}

pub fn runtime() -> Runtime {
    RtBuilder::new_multi_thread()
        .worker_threads(worker_threads())
        .enable_all()
        .build()
        .expect("tokio runtime")
}
