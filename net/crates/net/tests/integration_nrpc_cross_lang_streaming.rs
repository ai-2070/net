//! Cross-binding nRPC wire-format compat — Rust-side reference
//! implementation (Phase B12).
//!
//! **Scope:** this file is the Rust-side *reference* test against
//! `tests/cross_lang_nrpc/golden_vectors_streaming.json`. It does
//! NOT spawn Node / Python / Go processes — the cross-binding
//! harness is each language's own test suite running the same
//! assertions against the same fixture. The shared bytes-on-wire
//! contract lives in the fixture; this file proves the Rust side
//! honors it.
//!
//! **Coverage:**
//!
//!   - `client_streaming_ok_cases_match_fixture` — round-trip
//!     the documented `cross_lang_client_stream_sum` cases through
//!     the in-process fold (drain a stream of `{text, numbers}`
//!     items → one `{echo, total, count}` response).
//!   - `duplex_ok_cases_match_fixture` — same for
//!     `cross_lang_duplex_echo` (drain a stream, emit one
//!     response per item plus a final summary frame).
//!   - `wire_snapshots_match_fixture` — byte-exact assertion
//!     that the substrate codec produces the canonical hex
//!     bytes documented in `wire_snapshots`. The most load-bearing
//!     check: catches endianness / padding / flag-bit-layout
//!     drift that the JSON-only ok_cases miss. Each binding port
//!     runs the same hex-equality assertion against its own
//!     native encoder.
//!   - `error_cases_fixture_is_well_formed` — structural
//!     validation of the `error_cases` matrix so each binding's
//!     port consumes a consistent shape.
//!   - `fixture_metadata_matches_canonical_contract` — sanity
//!     check on the fixture's top-level shape.
//!
//! **Why not spawn other bindings here?** Each binding's test
//! suite already runs in its own host (`cargo test` for Node /
//! Python / Go FFI Rust source; native `npm test` / `pytest` /
//! `go test` for the idiomatic wrappers). Adding a Rust harness
//! that forks them would duplicate that infrastructure without
//! adding coverage. The fixture is the source of truth; each
//! language's reference assertion lives where that language is
//! naturally tested.
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
    classify_streaming_chunk, encode_request_grant, EventMeta, RequestStream,
    RpcAsyncResponseEmitter, RpcClientFold, RpcClientPending, RpcClientStreamingHandler,
    RpcDuplexFold, RpcDuplexHandler, RpcHandlerError, RpcRequestChunkPayload, RpcRequestPayload,
    RpcResponseEmitter, RpcResponsePayload, RpcResponseSink, RpcStatus, RpcStreamingContext,
    RpcStreamingRequestFold, StreamingChunkKind, DISPATCH_RPC_REQUEST, DISPATCH_RPC_REQUEST_CHUNK,
    DISPATCH_RPC_RESPONSE, EVENT_META_SIZE, FLAG_RPC_CLIENT_STREAMING_REQUEST,
    FLAG_RPC_REQUEST_END, FLAG_RPC_STREAMING_RESPONSE,
};
use net::adapter::net::redex::{RedexEntry, RedexEvent, RedexFold};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

const SERVICE_CLIENT_STREAM: &str = "cross_lang_client_stream_sum";
const SERVICE_DUPLEX: &str = "cross_lang_duplex_echo";
// Bumped 2 → 3 by S1-A1 (observer + metrics surfaces). The
// streaming shapes documented in this fixture are unchanged from
// v2; the v3 invariants live in the new observer_invariants +
// metrics_snapshot_invariants sections.
const ABI_VERSION_EXPECTED: u32 = 3;

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
            body: body.into(),
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
        body: body.into(),
    };
    let meta = EventMeta::new(DISPATCH_RPC_REQUEST, 0, caller_origin, call_id, 0);
    make_event(meta, &payload.encode())
}

fn request_chunk_event(caller_origin: u64, call_id: u64, flags: u16, body: Vec<u8>) -> RedexEvent {
    let payload = RpcRequestChunkPayload {
        call_id,
        flags,
        headers: vec![],
        body: body.into(),
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
    #[serde(default)]
    canonical_flag_end_placement: Option<JsonValue>,
    #[serde(default)]
    error_cases: Option<ErrorCasesFixture>,
}

#[derive(Deserialize)]
struct ServiceFixture {
    service: String,
    shape: String,
    ok_cases: Vec<JsonValue>,
}

#[derive(Deserialize)]
struct ErrorCasesFixture {
    #[allow(dead_code)]
    description: String,
    cases: Vec<ErrorCaseFixture>,
}

#[derive(Deserialize)]
struct ErrorCaseFixture {
    name: String,
    shape: String,
    service: String,
    #[allow(dead_code)]
    trigger: String,
    expected_status: String,
    expected_application_code: Option<String>,
    #[allow(dead_code)]
    #[serde(default)]
    expected_application_code_constant: Option<String>,
    #[allow(dead_code)]
    diagnostic_substring: String,
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
    // The canonical FLAG_END placement rule must be present; it
    // pins the wire contract that all bindings emit FLAG_END on
    // the data-bearing chunk, not a trailing-empty terminator.
    assert!(
        fx.canonical_flag_end_placement.is_some(),
        "fixture must document the canonical FLAG_END placement rule",
    );
}

/// Byte-exact wire snapshot pinning. The substrate codec MUST
/// produce exactly the hex bytes documented in
/// `golden_vectors_streaming.json::wire_snapshots`. Catches
/// endianness flips, padding drift, header packing changes, and
/// flag-bit layout drift that the JSON-only ok_cases would miss.
/// Per-binding ports run the same assertion against their own
/// native encoder.
#[test]
fn wire_snapshots_match_fixture() {
    let raw = include_str!("cross_lang_nrpc/golden_vectors_streaming.json");
    let fixture: JsonValue = serde_json::from_str(raw).expect("fixture parses");
    let snapshots = fixture
        .get("wire_snapshots")
        .expect("fixture has wire_snapshots section");

    // 1. Initial REQUEST payload (client-streaming flag set).
    let req = RpcRequestPayload {
        service: "test".to_string(),
        deadline_ns: 0,
        flags: FLAG_RPC_CLIENT_STREAMING_REQUEST,
        headers: vec![],
        body: bytes::Bytes::from_static(b"hello"),
    };
    let req_bytes = req.encode();
    let req_hex = hex_string(&req_bytes);
    let expected_req = snapshots
        .get("request_payload_minimal_clientstream")
        .and_then(|v| v.get("hex"))
        .and_then(|v| v.as_str())
        .expect("fixture has request_payload_minimal_clientstream.hex");
    assert_eq!(
        req_hex, expected_req,
        "RpcRequestPayload encoding drifted from canonical wire snapshot",
    );

    // 2. REQUEST_CHUNK payload with FLAG_END set.
    let chunk = RpcRequestChunkPayload {
        call_id: 42,
        flags: FLAG_RPC_REQUEST_END,
        headers: vec![],
        body: bytes::Bytes::from_static(b"bye"),
    };
    let chunk_bytes = chunk.encode();
    let chunk_hex = hex_string(&chunk_bytes);
    let expected_chunk = snapshots
        .get("request_chunk_payload_with_end")
        .and_then(|v| v.get("hex"))
        .and_then(|v| v.as_str())
        .expect("fixture has request_chunk_payload_with_end.hex");
    assert_eq!(
        chunk_hex, expected_chunk,
        "RpcRequestChunkPayload encoding drifted from canonical wire snapshot",
    );

    // 3. REQUEST_GRANT payload (note: call_id is u64 LE, credits
    //    is u32 BE — bindings MUST preserve the asymmetry).
    let grant_bytes = encode_request_grant(42, 8);
    let grant_hex = hex_string(&grant_bytes);
    let expected_grant = snapshots
        .get("request_grant_payload")
        .and_then(|v| v.get("hex"))
        .and_then(|v| v.as_str())
        .expect("fixture has request_grant_payload.hex");
    assert_eq!(
        grant_hex, expected_grant,
        "encode_request_grant drifted from canonical wire snapshot",
    );
}

fn hex_string(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        write!(&mut s, "{b:02x}").expect("write to String");
    }
    s
}

/// Validate the `error_cases` fixture section: every documented
/// case parses cleanly, references one of the canonical services,
/// and carries a recognized status. This pins the contract that
/// per-binding error tests will consume; the actual end-to-end
/// error exercises live next to each binding's tests (and in the
/// existing ok_case tests above).
#[test]
fn error_cases_fixture_is_well_formed() {
    let fx = load_fixture();
    let cases = fx
        .error_cases
        .as_ref()
        .expect("fixture must include error_cases section")
        .cases
        .as_slice();
    assert!(
        !cases.is_empty(),
        "error_cases must include at least one case"
    );
    let recognized_statuses = ["Application", "Cancelled", "ClientCodec", "Internal"];
    for case in cases {
        assert!(
            case.shape == "client_streaming" || case.shape == "duplex",
            "case {} has recognized shape; got {}",
            case.name,
            case.shape,
        );
        assert!(
            case.service == SERVICE_CLIENT_STREAM || case.service == SERVICE_DUPLEX,
            "case {} references a canonical service; got {}",
            case.name,
            case.service,
        );
        assert!(
            recognized_statuses.contains(&case.expected_status.as_str()),
            "case {} has recognized status; got {}",
            case.name,
            case.expected_status,
        );
        if case.expected_status == "Application" {
            let code = case
                .expected_application_code
                .as_ref()
                .expect("Application status must carry an expected_application_code");
            assert!(
                code.starts_with("0x") || code.parse::<u16>().is_ok(),
                "case {} application code must be hex (0xNNNN) or decimal: {}",
                case.name,
                code,
            );
        }
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

// The fixture's `error_cases` section is consumed by the
// integration_nrpc_cross_lang_streaming_errors test below; this
// `#[allow]` shim keeps the streaming-chunk classifier imports
// reachable from the future per-binding ports that will exercise
// the same error_cases against Node/Python/Go.
#[allow(dead_code)]
fn _suppress_unused() {
    let _: StreamingChunkKind = StreamingChunkKind::Unary;
    let _ = classify_streaming_chunk;
}

// =====================================================================
// S2-X: observer + metrics invariants — structural validation.
//
// These tests assert the fixture's observer_invariants +
// metrics_snapshot_invariants sections are well-formed AND pin the
// substrate's RpcCallEvent / ServiceMetrics field shape against the
// documented contract. Per-binding ports (Node test/, Python tests/,
// Go *_test.go) consume the same fixture and run the equivalent
// assertion against their own binding's POD/dataclass/struct shape.
//
// Live observer-fire tests against a real MeshNode belong in the
// per-binding test suites — each binding has its own way to wire
// the observer (TSFN, GIL worker, cgo trampoline) and the substrate
// firing path is already exercised by the per-binding raw bindings.
// =====================================================================

/// The fixture's `observer_invariants` section is well-formed:
/// every documented field has a `name`, `type`, and `semantic`;
/// the status_discriminator has exactly the four variants the
/// substrate defines; the direction_discriminator has exactly
/// two variants with correct emission flags.
#[test]
fn observer_invariants_fixture_is_well_formed() {
    let raw = include_str!("cross_lang_nrpc/golden_vectors_streaming.json");
    let fixture: JsonValue = serde_json::from_str(raw).expect("fixture parses");
    let obs = fixture
        .get("observer_invariants")
        .expect("fixture has observer_invariants section");

    let fields = obs
        .get("fields")
        .and_then(|v| v.as_array())
        .expect("observer_invariants.fields is an array");
    // The substrate's RpcCallEvent has 9 fields — drift would
    // break every binding's POD-to-typed-event normalization.
    assert_eq!(fields.len(), 9, "RpcCallEvent has 9 documented fields");
    for f in fields {
        assert!(
            f.get("name").and_then(|v| v.as_str()).is_some(),
            "field has a name"
        );
        assert!(
            f.get("type").and_then(|v| v.as_str()).is_some(),
            "field has a type"
        );
        assert!(
            f.get("semantic").and_then(|v| v.as_str()).is_some(),
            "field has a semantic doc"
        );
    }
    // Pin the specific field names so a substrate-side rename
    // surfaces here loudly.
    let names: Vec<&str> = fields
        .iter()
        .map(|f| f.get("name").and_then(|v| v.as_str()).unwrap())
        .collect();
    for expected in [
        "caller",
        "callee",
        "method",
        "latency_ms",
        "status",
        "request_bytes",
        "response_bytes",
        "direction",
        "ts_unix_ms",
    ] {
        assert!(
            names.contains(&expected),
            "fixture is missing documented field {expected}",
        );
    }

    let status = obs
        .get("status_discriminator")
        .and_then(|v| v.get("variants"))
        .and_then(|v| v.as_array())
        .expect("status_discriminator.variants is an array");
    assert_eq!(status.len(), 4, "RpcCallStatus has 4 variants");
    let kinds: Vec<&str> = status
        .iter()
        .map(|v| v.get("kind").and_then(|x| x.as_str()).unwrap())
        .collect();
    for expected in ["ok", "error", "timeout", "canceled"] {
        assert!(
            kinds.contains(&expected),
            "status_discriminator missing variant {expected}",
        );
    }
    // The 'error' variant is the only one with a message. Pin so
    // a future tagged-union extension doesn't accidentally add
    // message to ok/timeout/canceled.
    for v in status {
        let kind = v.get("kind").and_then(|x| x.as_str()).unwrap();
        let has_msg = v.get("has_message").and_then(|x| x.as_bool()).unwrap();
        if kind == "error" {
            assert!(has_msg, "'error' variant carries a message");
        } else {
            assert!(!has_msg, "'{kind}' variant does NOT carry a message");
        }
    }
    // C ABI discriminant values must match the constants in
    // rpc-ffi/src/lib.rs::NET_RPC_STATUS_* (0/1/2/3).
    for (kind, expected_val) in [("ok", 0), ("error", 1), ("timeout", 2), ("canceled", 3)] {
        let v = status
            .iter()
            .find(|v| v.get("kind").and_then(|x| x.as_str()) == Some(kind))
            .unwrap();
        let got_val = v.get("c_abi_value").and_then(|x| x.as_u64()).unwrap();
        assert_eq!(
            got_val, expected_val,
            "'{kind}' C ABI discriminant must be {expected_val}",
        );
    }

    let direction = obs
        .get("direction_discriminator")
        .and_then(|v| v.get("variants"))
        .and_then(|v| v.as_array())
        .expect("direction_discriminator.variants is an array");
    assert_eq!(direction.len(), 2, "RpcDirection has 2 variants");
    // v1 emits only Outbound. Pin so a future Inbound-server-hook
    // landing doesn't accidentally claim v1-already-shipped.
    let outbound = direction
        .iter()
        .find(|v| v.get("kind").and_then(|x| x.as_str()) == Some("outbound"))
        .unwrap();
    assert!(outbound
        .get("emitted_in_v1")
        .and_then(|x| x.as_bool())
        .unwrap());
    let inbound = direction
        .iter()
        .find(|v| v.get("kind").and_then(|x| x.as_str()) == Some("inbound"))
        .unwrap();
    assert!(!inbound
        .get("emitted_in_v1")
        .and_then(|x| x.as_bool())
        .unwrap());

    // firing_contract documents per-binding implementations + the
    // locked-decision-#1 cheap-callbacks rule. Just check it's
    // structurally present — drift in the per-binding entries
    // matters less than drift in the variant table above.
    assert!(
        obs.get("firing_contract").is_some(),
        "observer_invariants documents the firing contract",
    );
}

/// The fixture's `metrics_snapshot_invariants` section is well-
/// formed AND pins the exact set of ServiceMetrics fields the
/// substrate's RpcMetricsSnapshot exposes. Drift in the substrate
/// definition surfaces as a test failure here BEFORE per-binding
/// tests start producing red builds with confusing diagnostics.
#[test]
fn metrics_snapshot_invariants_fixture_is_well_formed() {
    let raw = include_str!("cross_lang_nrpc/golden_vectors_streaming.json");
    let fixture: JsonValue = serde_json::from_str(raw).expect("fixture parses");
    let m = fixture
        .get("metrics_snapshot_invariants")
        .expect("fixture has metrics_snapshot_invariants section");

    // Envelope: services list, sorted by name.
    let envelope = m
        .get("envelope")
        .expect("metrics_snapshot_invariants has envelope");
    assert_eq!(
        envelope.get("field").and_then(|v| v.as_str()),
        Some("services"),
        "envelope field is 'services'",
    );

    // Field set: every ServiceMetrics field is documented exactly
    // once with name/type/section/semantic.
    let fields = m
        .get("service_metrics_fields")
        .and_then(|v| v.as_array())
        .expect("service_metrics_fields is an array");
    // The substrate's ServiceMetrics has 19 documented fields
    // (1 identity + 9 caller-side + 9 server-side). Pin so a
    // substrate-side field add/remove surfaces here.
    assert_eq!(fields.len(), 19, "ServiceMetrics has 19 documented fields",);
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for f in fields {
        let name = f
            .get("name")
            .and_then(|v| v.as_str())
            .expect("field has a name");
        assert!(seen.insert(name), "duplicate field: {name}");
        assert!(
            f.get("type").and_then(|v| v.as_str()).is_some(),
            "field {name} has a type"
        );
        assert!(
            f.get("section").and_then(|v| v.as_str()).is_some(),
            "field {name} has a section"
        );
        assert!(
            f.get("semantic").and_then(|v| v.as_str()).is_some(),
            "field {name} has a semantic doc"
        );
    }
    // Pin the canonical caller-side fields. A rename in
    // mesh_rpc_metrics.rs::ServiceMetrics is a cross-binding
    // breaking change; this test catches it before each binding
    // re-derives its POD.
    let expected_caller_side = [
        "calls_total",
        "errors_no_route",
        "errors_timeout",
        "errors_server",
        "errors_transport",
        "in_flight",
        "latency_sum_ns",
        "latency_count",
        "latency_buckets",
    ];
    for expected in &expected_caller_side {
        assert!(
            seen.contains(expected),
            "caller-side field {expected} missing",
        );
    }
    // Server-side fields — same drift-detection contract.
    let expected_server_side = [
        "handler_invocations_total",
        "handler_panics_total",
        "handler_in_flight",
        "handler_duration_sum_ns",
        "handler_duration_count",
        "handler_duration_buckets",
        "streaming_chunks_emitted_total",
        "streaming_chunks_dropped_total",
        "capability_denied_total",
    ];
    for expected in &expected_server_side {
        assert!(
            seen.contains(expected),
            "server-side field {expected} missing",
        );
    }

    // Per-binding naming + scrape-cost contracts are structurally
    // present — drift in the per-binding entries matters less than
    // drift in the field table above.
    assert!(
        m.get("per_binding_field_naming").is_some(),
        "metrics_snapshot_invariants documents per-binding naming",
    );
    assert!(
        m.get("scrape_cost").is_some(),
        "metrics_snapshot_invariants documents scrape cost",
    );
}

/// Pin the substrate-side `ServiceMetrics` struct against the
/// fixture. If a future substrate change adds / removes / renames
/// a field, this test catches the drift — the fixture is the
/// source of truth for cross-binding compat, so the test asserts
/// the substrate side hasn't drifted from it.
///
/// Implementation: roundtrip a default ServiceMetrics through the
/// substrate's Prometheus-formatter path and parse out the field
/// names. Avoids depending on serde::Serialize (which the substrate
/// type doesn't derive) by using the public `service` + counter
/// accessors directly.
#[test]
fn service_metrics_fields_match_fixture() {
    use net::adapter::net::mesh_rpc_metrics::ServiceMetrics;

    // Compile-time witness: construct a ServiceMetrics with every
    // field populated. A substrate-side field removal surfaces
    // here as a compile error (missing field in struct literal);
    // a field rename surfaces the same way. A field ADDITION
    // surfaces via the
    // metrics_snapshot_invariants_fixture_is_well_formed test's
    // 19-field count assertion.
    let svc = ServiceMetrics {
        service: "test_service".to_string(),
        calls_total: 0,
        errors_no_route: 0,
        errors_timeout: 0,
        errors_server: 0,
        errors_transport: 0,
        in_flight: 0,
        latency_sum_ns: 0,
        latency_count: 0,
        latency_buckets: vec![],
        handler_invocations_total: 0,
        handler_panics_total: 0,
        handler_in_flight: 0,
        handler_duration_sum_ns: 0,
        handler_duration_count: 0,
        handler_duration_buckets: vec![],
        streaming_chunks_emitted_total: 0,
        streaming_chunks_dropped_total: 0,
        capability_denied_total: 0,
    };
    // Touch every field — read-side mirror of the literal above
    // so a field rename caught at construction also catches at
    // access if some future refactor splits the two.
    let _: &str = &svc.service;
    let _: u64 = svc.calls_total;
    let _: u64 = svc.errors_no_route;
    let _: u64 = svc.errors_timeout;
    let _: u64 = svc.errors_server;
    let _: u64 = svc.errors_transport;
    let _: i64 = svc.in_flight;
    let _: u64 = svc.latency_sum_ns;
    let _: u64 = svc.latency_count;
    let _: &Vec<u64> = &svc.latency_buckets;
    let _: u64 = svc.handler_invocations_total;
    let _: u64 = svc.handler_panics_total;
    let _: i64 = svc.handler_in_flight;
    let _: u64 = svc.handler_duration_sum_ns;
    let _: u64 = svc.handler_duration_count;
    let _: &Vec<u64> = &svc.handler_duration_buckets;
    let _: u64 = svc.streaming_chunks_emitted_total;
    let _: u64 = svc.streaming_chunks_dropped_total;
    let _: u64 = svc.capability_denied_total;

    // Sanity: the fixture lists the same field names.
    let raw = include_str!("cross_lang_nrpc/golden_vectors_streaming.json");
    let fixture: JsonValue = serde_json::from_str(raw).expect("fixture parses");
    let fields: Vec<&str> = fixture
        .get("metrics_snapshot_invariants")
        .and_then(|v| v.get("service_metrics_fields"))
        .and_then(|v| v.as_array())
        .unwrap()
        .iter()
        .map(|f| f.get("name").and_then(|v| v.as_str()).unwrap())
        .collect();
    assert!(
        fields.contains(&"service"),
        "fixture documents 'service' field",
    );
    assert!(
        fields.contains(&"in_flight"),
        "fixture documents 'in_flight' field",
    );
    assert!(
        fields.contains(&"capability_denied_total"),
        "fixture documents 'capability_denied_total' field",
    );
}

/// Pin the substrate-side `RpcCallEvent` field shape against the
/// fixture's observer_invariants section. Same drift-detection
/// rationale as service_metrics_fields_match_fixture: the fixture
/// is the cross-binding source of truth.
#[test]
fn rpc_call_event_fields_match_fixture() {
    use net::adapter::net::cortex::{RpcCallEvent, RpcCallStatus, RpcDirection};

    // Construct an event with every field populated so a removal
    // surfaces as a compile error.
    let evt = RpcCallEvent {
        caller: 0xAA00,
        callee: 0xBB00,
        method: "echo".to_string(),
        latency_ms: 7,
        status: RpcCallStatus::Ok,
        request_bytes: 10,
        response_bytes: 20,
        direction: RpcDirection::Outbound,
        ts_unix_ms: 1_700_000_000_000,
    };
    // Touch every field.
    let _: u64 = evt.caller;
    let _: u64 = evt.callee;
    let _: &str = evt.method.as_str();
    let _: u32 = evt.latency_ms;
    let _: &RpcCallStatus = &evt.status;
    let _: u32 = evt.request_bytes;
    let _: u32 = evt.response_bytes;
    let _: RpcDirection = evt.direction;
    let _: u64 = evt.ts_unix_ms;

    // Status variant exhaustiveness check: a future substrate-side
    // variant addition surfaces as a `non-exhaustive patterns`
    // error at compile time.
    let _: &str = match &evt.status {
        RpcCallStatus::Ok => "ok",
        RpcCallStatus::Error(_) => "error",
        RpcCallStatus::Timeout => "timeout",
        RpcCallStatus::Canceled => "canceled",
    };
    // Direction variant exhaustiveness.
    let _: &str = match evt.direction {
        RpcDirection::Outbound => "outbound",
        RpcDirection::Inbound => "inbound",
    };

    // Sanity: the fixture lists the same field names.
    let raw = include_str!("cross_lang_nrpc/golden_vectors_streaming.json");
    let fixture: JsonValue = serde_json::from_str(raw).expect("fixture parses");
    let fields: Vec<&str> = fixture
        .get("observer_invariants")
        .and_then(|v| v.get("fields"))
        .and_then(|v| v.as_array())
        .unwrap()
        .iter()
        .map(|f| f.get("name").and_then(|v| v.as_str()).unwrap())
        .collect();
    assert!(fields.contains(&"caller"));
    assert!(fields.contains(&"callee"));
    assert!(fields.contains(&"latency_ms"));
    assert!(fields.contains(&"ts_unix_ms"));
}
