//! Cross-binding nRPC wire-format compat — Phase B12.
//!
//! Loads `tests/cross_lang_nrpc/golden_vectors_streaming.json`
//! and asserts the two canonical Phase-2 services round-trip
//! correctly through the Rust runtime:
//!
//!   - `cross_lang_client_stream_sum` — drain a stream of
//!     `{text, numbers}` items, return one `{echo, total, count}`
//!     response.
//!   - `cross_lang_duplex_echo` — drain a stream, emit one
//!     response per item plus a final `{echo: "summary", sum: N}`
//!     frame.
//!
//! The same fixture is the source-of-truth for the Node, Python,
//! and Go binding compat tests (B9 / B10 / B11 ports — separate
//! commits).
//!
//! The contract is documented in `golden_vectors_streaming.json`
//! itself; this file is the load-bearing Rust-side reference
//! implementation that defines what "correct" looks like.
//!
//! Same direct-fold-dispatch pattern as the existing
//! `integration_nrpc_cross_lang.rs` (no real network — the
//! caller-side `RpcClientPending::register_*` is wired straight
//! to the server-side fold's emit closure, so this test exercises
//! the encode/dispatch/decode loop without paying handshake
//! latency).

#![cfg(feature = "cortex")]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use net::adapter::net::cortex::{
    classify_streaming_chunk, EventMeta, RequestStream, RpcAsyncResponseEmitter, RpcClientFold,
    RpcClientPending, RpcClientStreamingHandler, RpcDuplexFold, RpcDuplexHandler, RpcHandlerError,
    RpcRequestChunkPayload, RpcRequestPayload, RpcResponseEmitter, RpcResponsePayload,
    RpcResponseSink, RpcStatus, RpcStreamingContext, RpcStreamingRequestFold, StreamingChunkKind,
    DISPATCH_RPC_REQUEST, DISPATCH_RPC_REQUEST_CHUNK, DISPATCH_RPC_RESPONSE, EVENT_META_SIZE,
    FLAG_RPC_CLIENT_STREAMING_REQUEST, FLAG_RPC_REQUEST_END, FLAG_RPC_STREAMING_RESPONSE,
};
use net::adapter::net::redex::{RedexEntry, RedexEvent, RedexFold};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

const SERVICE_CLIENT_STREAM: &str = "cross_lang_client_stream_sum";
const SERVICE_DUPLEX: &str = "cross_lang_duplex_echo";
const ABI_VERSION_EXPECTED: u32 = 2;

// =====================================================================
// Canonical wire shapes.
// =====================================================================

#[derive(Deserialize, Clone)]
struct StreamItem {
    text: String,
    numbers: Vec<i64>,
}

#[derive(Serialize)]
struct ClientStreamResponse {
    echo: String,
    total: i64,
    count: u64,
}

#[derive(Serialize)]
struct DuplexFrame {
    echo: String,
    sum: i64,
}

// =====================================================================
// Reference handlers.
// =====================================================================

/// Reference handler for `cross_lang_client_stream_sum`. Drains
/// the request stream, accumulates text + numbers + count, returns
/// one terminal JSON response.
struct ClientStreamSumHandler;

#[async_trait::async_trait]
impl RpcClientStreamingHandler for ClientStreamSumHandler {
    async fn call(
        &self,
        _ctx: RpcStreamingContext,
        mut requests: RequestStream,
    ) -> Result<RpcResponsePayload, RpcHandlerError> {
        let mut texts: Vec<String> = Vec::new();
        let mut total: i64 = 0;
        let mut count: u64 = 0;
        while let Some(bytes) = requests.next().await {
            let item: StreamItem = serde_json::from_slice(&bytes)
                .map_err(|e| RpcHandlerError::Internal(format!("decode item: {e}")))?;
            total = total.saturating_add(item.numbers.iter().copied().sum::<i64>());
            texts.push(item.text);
            count += 1;
        }
        let echo = texts.join(" ");
        let resp = ClientStreamResponse { echo, total, count };
        let body = serde_json::to_vec(&resp)
            .map_err(|e| RpcHandlerError::Internal(format!("encode resp: {e}")))?;
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body,
        })
    }
}

/// Reference handler for `cross_lang_duplex_echo`. Drains the
/// request stream, emits one response per item, then a final
/// `{echo: "summary", sum: count_of_items}` frame.
struct DuplexEchoHandler;

#[async_trait::async_trait]
impl RpcDuplexHandler for DuplexEchoHandler {
    async fn call(
        &self,
        _ctx: RpcStreamingContext,
        mut requests: RequestStream,
        responses: RpcResponseSink,
    ) -> Result<(), RpcHandlerError> {
        let mut count: i64 = 0;
        while let Some(bytes) = requests.next().await {
            let item: StreamItem = serde_json::from_slice(&bytes)
                .map_err(|e| RpcHandlerError::Internal(format!("decode item: {e}")))?;
            let frame = DuplexFrame {
                echo: item.text,
                sum: item.numbers.iter().copied().sum(),
            };
            let body = serde_json::to_vec(&frame)
                .map_err(|e| RpcHandlerError::Internal(format!("encode frame: {e}")))?;
            responses.send(body);
            count += 1;
        }
        let summary = DuplexFrame {
            echo: "summary".to_string(),
            sum: count,
        };
        let body = serde_json::to_vec(&summary)
            .map_err(|e| RpcHandlerError::Internal(format!("encode summary: {e}")))?;
        responses.send(body);
        Ok(())
    }
}

// =====================================================================
// Event helpers.
// =====================================================================

fn make_event(meta: EventMeta, payload_tail: &[u8]) -> RedexEvent {
    let mut buf = Vec::with_capacity(EVENT_META_SIZE + payload_tail.len());
    buf.extend_from_slice(&meta.to_bytes());
    buf.extend_from_slice(payload_tail);
    RedexEvent {
        entry: RedexEntry::new_heap(0, 0, buf.len() as u32, 0, 0),
        payload: Bytes::from(buf),
    }
}

fn initial_request_event(
    caller_origin: u64,
    call_id: u64,
    service: &str,
    flags: u16,
    body: Vec<u8>,
) -> RedexEvent {
    let payload = RpcRequestPayload {
        service: service.to_string(),
        deadline_ns: 0,
        flags,
        headers: vec![],
        body,
    };
    let meta = EventMeta::new(DISPATCH_RPC_REQUEST, 0, caller_origin, call_id, 0);
    make_event(meta, &payload.encode())
}

fn request_chunk_event(caller_origin: u64, call_id: u64, flags: u16, body: Vec<u8>) -> RedexEvent {
    let payload = RpcRequestChunkPayload {
        call_id,
        flags,
        headers: vec![],
        body,
    };
    let meta = EventMeta::new(DISPATCH_RPC_REQUEST_CHUNK, 0, caller_origin, call_id, 0);
    make_event(meta, &payload.encode())
}

fn response_event(caller_origin: u64, call_id: u64, payload: &RpcResponsePayload) -> RedexEvent {
    let meta = EventMeta::new(DISPATCH_RPC_RESPONSE, 0, caller_origin, call_id, 0);
    make_event(meta, &payload.encode())
}

// =====================================================================
// Client-streaming loopback.
// =====================================================================

struct ClientStreamLoopback {
    server_fold: Arc<Mutex<RpcStreamingRequestFold>>,
    pending: Arc<RpcClientPending>,
    next_call_id: AtomicU64,
    caller_origin: u64,
}

impl ClientStreamLoopback {
    fn new() -> Self {
        let pending = Arc::new(RpcClientPending::new());
        let client_fold = Arc::new(Mutex::new(RpcClientFold::new(pending.clone())));
        let emit: RpcResponseEmitter = Arc::new(move |origin, call_id, resp| {
            let ev = response_event(origin, call_id, &resp);
            client_fold
                .lock()
                .apply(&ev, &mut ())
                .expect("client fold apply");
        });
        let server_fold = Arc::new(Mutex::new(RpcStreamingRequestFold::new(
            Arc::new(ClientStreamSumHandler),
            emit,
        )));
        Self {
            server_fold,
            pending,
            next_call_id: AtomicU64::new(1),
            caller_origin: 0xC1055,
        }
    }

    async fn run(&self, items: &[StreamItem]) -> RpcResponsePayload {
        let call_id = self.next_call_id.fetch_add(1, Ordering::Relaxed);
        let (terminal_rx, _grant_rx) = self.pending.register_client_streaming(call_id, 0);
        // Initial REQUEST — empty body, FLAG_CLIENT_STREAMING set.
        // Empty body is the convention when the first send hasn't
        // happened yet; the streaming-request fold's terminator-
        // semantics rule (empty body + FLAG_END = pure terminator)
        // means a zero-item upload sends ONE frame with both flags.
        let initial_flags = FLAG_RPC_CLIENT_STREAMING_REQUEST;
        if items.is_empty() {
            // Degenerate path — initial REQUEST with FLAG_END set.
            // Empty body so the fold emits zero stream items.
            let ev = initial_request_event(
                self.caller_origin,
                call_id,
                SERVICE_CLIENT_STREAM,
                initial_flags | FLAG_RPC_REQUEST_END,
                vec![],
            );
            self.server_fold
                .lock()
                .apply(&ev, &mut ())
                .expect("server fold apply");
        } else {
            // First item rides on the initial REQUEST body. This
            // matches what ClientStreamCallRaw does in production
            // (see mesh_rpc.rs::ClientStreamCallRaw::send).
            let first_body = serde_json::to_vec(&serde_json::json!({
                "text": items[0].text,
                "numbers": items[0].numbers,
            }))
            .expect("encode item");
            let ev = initial_request_event(
                self.caller_origin,
                call_id,
                SERVICE_CLIENT_STREAM,
                initial_flags,
                first_body,
            );
            self.server_fold
                .lock()
                .apply(&ev, &mut ())
                .expect("server fold apply");
            // Remaining items as REQUEST_CHUNKs; last has FLAG_END.
            for (i, item) in items.iter().enumerate().skip(1) {
                let body = serde_json::to_vec(&serde_json::json!({
                    "text": item.text,
                    "numbers": item.numbers,
                }))
                .expect("encode item");
                let flags = if i == items.len() - 1 {
                    FLAG_RPC_REQUEST_END
                } else {
                    0
                };
                let ev = request_chunk_event(self.caller_origin, call_id, flags, body);
                self.server_fold
                    .lock()
                    .apply(&ev, &mut ())
                    .expect("server fold apply chunk");
            }
            // If items had only 1 entry, the initial REQUEST didn't
            // get FLAG_END; emit a trailing empty-body END chunk.
            if items.len() == 1 {
                let ev =
                    request_chunk_event(self.caller_origin, call_id, FLAG_RPC_REQUEST_END, vec![]);
                self.server_fold
                    .lock()
                    .apply(&ev, &mut ())
                    .expect("server fold apply terminator");
            }
        }
        tokio::time::timeout(Duration::from_secs(2), terminal_rx)
            .await
            .expect("terminal RESPONSE within 2s")
            .expect("oneshot delivers")
    }
}

// =====================================================================
// Duplex loopback.
// =====================================================================

struct DuplexLoopback {
    server_fold: Arc<Mutex<RpcDuplexFold>>,
    pending: Arc<RpcClientPending>,
    next_call_id: AtomicU64,
    caller_origin: u64,
}

impl DuplexLoopback {
    fn new() -> Self {
        let pending = Arc::new(RpcClientPending::new());
        let client_fold = Arc::new(Mutex::new(RpcClientFold::new(pending.clone())));
        let emit: RpcAsyncResponseEmitter = Arc::new(move |origin, call_id, resp| {
            let client_fold = client_fold.clone();
            Box::pin(async move {
                let ev = response_event(origin, call_id, &resp);
                client_fold
                    .lock()
                    .apply(&ev, &mut ())
                    .expect("client fold apply");
            })
        });
        let server_fold = Arc::new(Mutex::new(RpcDuplexFold::new(
            Arc::new(DuplexEchoHandler),
            emit,
        )));
        Self {
            server_fold,
            pending,
            next_call_id: AtomicU64::new(1),
            caller_origin: 0xD007E,
        }
    }

    /// Run a duplex call with `items` as the upload stream.
    /// Returns the collected response chunk bodies (each entry is
    /// one Resp JSON body) — terminator absent from the return.
    async fn run(&self, items: &[StreamItem]) -> Vec<Vec<u8>> {
        let call_id = self.next_call_id.fetch_add(1, Ordering::Relaxed);
        let (mut chunks_rx, _grant_rx) = self.pending.register_duplex(call_id, 0);
        let initial_flags = FLAG_RPC_CLIENT_STREAMING_REQUEST | FLAG_RPC_STREAMING_RESPONSE;
        if items.is_empty() {
            let ev = initial_request_event(
                self.caller_origin,
                call_id,
                SERVICE_DUPLEX,
                initial_flags | FLAG_RPC_REQUEST_END,
                vec![],
            );
            self.server_fold
                .lock()
                .apply(&ev, &mut ())
                .expect("server fold apply");
        } else {
            let first_body = serde_json::to_vec(&serde_json::json!({
                "text": items[0].text,
                "numbers": items[0].numbers,
            }))
            .expect("encode item");
            let ev = initial_request_event(
                self.caller_origin,
                call_id,
                SERVICE_DUPLEX,
                initial_flags,
                first_body,
            );
            self.server_fold
                .lock()
                .apply(&ev, &mut ())
                .expect("server fold apply");
            for (i, item) in items.iter().enumerate().skip(1) {
                let body = serde_json::to_vec(&serde_json::json!({
                    "text": item.text,
                    "numbers": item.numbers,
                }))
                .expect("encode item");
                let flags = if i == items.len() - 1 {
                    FLAG_RPC_REQUEST_END
                } else {
                    0
                };
                let ev = request_chunk_event(self.caller_origin, call_id, flags, body);
                self.server_fold
                    .lock()
                    .apply(&ev, &mut ())
                    .expect("server fold apply chunk");
            }
            if items.len() == 1 {
                let ev =
                    request_chunk_event(self.caller_origin, call_id, FLAG_RPC_REQUEST_END, vec![]);
                self.server_fold
                    .lock()
                    .apply(&ev, &mut ())
                    .expect("server fold apply terminator");
            }
        }
        // Collect chunks until we observe a terminal frame.
        let mut bodies: Vec<Vec<u8>> = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let recv = tokio::time::timeout_at(deadline, chunks_rx.recv()).await;
            let item = recv.expect("chunk within 2s");
            match item {
                Some(net::adapter::net::cortex::StreamItem::Chunk(bytes)) => {
                    bodies.push(bytes.to_vec());
                }
                Some(net::adapter::net::cortex::StreamItem::End)
                | Some(net::adapter::net::cortex::StreamItem::Error(_)) => break,
                None => break,
            }
        }
        bodies
    }
}

// =====================================================================
// Fixture types.
// =====================================================================

#[derive(Deserialize)]
struct GoldenFixture {
    abi_version_expected: u32,
    services: Vec<ServiceFixture>,
}

#[derive(Deserialize)]
struct ServiceFixture {
    service: String,
    shape: String,
    ok_cases: Vec<JsonValue>,
}

fn load_fixture() -> GoldenFixture {
    let raw = include_str!("cross_lang_nrpc/golden_vectors_streaming.json");
    serde_json::from_str(raw).expect("golden_vectors_streaming.json is valid JSON")
}

fn items_from_case(case: &JsonValue) -> Vec<StreamItem> {
    let arr = case
        .get("request_items")
        .and_then(|v| v.as_array())
        .expect("case has request_items array");
    arr.iter()
        .map(|v| StreamItem {
            text: v
                .get("text")
                .and_then(|t| t.as_str())
                .expect("item.text")
                .to_string(),
            numbers: v
                .get("numbers")
                .and_then(|n| n.as_array())
                .map(|arr| arr.iter().filter_map(|x| x.as_i64()).collect())
                .unwrap_or_default(),
        })
        .collect()
}

// =====================================================================
// Tests.
// =====================================================================

/// Sanity-check fixture metadata before exercising the cases.
#[test]
fn fixture_metadata_matches_canonical_contract() {
    let fx = load_fixture();
    assert_eq!(
        fx.abi_version_expected, ABI_VERSION_EXPECTED,
        "fixture's expected ABI version must match the constant in this file"
    );
    assert_eq!(fx.services.len(), 2);
    let names: Vec<&str> = fx.services.iter().map(|s| s.service.as_str()).collect();
    assert!(names.contains(&SERVICE_CLIENT_STREAM));
    assert!(names.contains(&SERVICE_DUPLEX));
    for svc in &fx.services {
        assert!(
            !svc.ok_cases.is_empty(),
            "service {} has at least one ok_case",
            svc.service
        );
        assert!(
            svc.shape == "client_streaming" || svc.shape == "duplex",
            "shape {} on {} is one of the two we recognize",
            svc.shape,
            svc.service
        );
    }
}

/// Client-streaming ok_cases — every case round-trips through
/// the canonical handler and produces the expected JSON.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_streaming_ok_cases_match_fixture() {
    let fx = load_fixture();
    let svc = fx
        .services
        .iter()
        .find(|s| s.service == SERVICE_CLIENT_STREAM)
        .expect("client-stream service in fixture");
    let loopback = ClientStreamLoopback::new();
    for case in &svc.ok_cases {
        let name = case
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("<unnamed>");
        let items = items_from_case(case);
        let resp = loopback.run(&items).await;
        assert_eq!(
            resp.status,
            RpcStatus::Ok,
            "case {name}: expected Ok status, got {:?}",
            resp.status
        );
        let got: JsonValue = serde_json::from_slice(&resp.body).expect("response body is JSON");
        let expected = case
            .get("expected_response_json")
            .expect("case has expected_response_json");
        assert_eq!(&got, expected, "case {name}: response mismatch");
    }
}

/// Duplex ok_cases — every case round-trips through the canonical
/// handler and produces the expected sequence of response frames.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duplex_ok_cases_match_fixture() {
    let fx = load_fixture();
    let svc = fx
        .services
        .iter()
        .find(|s| s.service == SERVICE_DUPLEX)
        .expect("duplex service in fixture");
    let loopback = DuplexLoopback::new();
    for case in &svc.ok_cases {
        let name = case
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("<unnamed>");
        let items = items_from_case(case);
        let bodies = loopback.run(&items).await;
        let expected_items = case
            .get("expected_response_items")
            .and_then(|v| v.as_array())
            .expect("case has expected_response_items array");
        assert_eq!(
            bodies.len(),
            expected_items.len(),
            "case {name}: expected {} response frames, got {}",
            expected_items.len(),
            bodies.len()
        );
        for (i, (got_bytes, expected)) in bodies.iter().zip(expected_items.iter()).enumerate() {
            let got: JsonValue = serde_json::from_slice(got_bytes).expect("response chunk is JSON");
            assert_eq!(&got, expected, "case {name}: frame {i} mismatch");
        }
    }
}

// Suppress unused-import warning for the streaming-chunk classifier
// (only relevant if a future error_cases section is added).
#[allow(dead_code)]
fn _suppress_unused() {
    let _: StreamingChunkKind = StreamingChunkKind::Unary;
    let _ = classify_streaming_chunk;
}
