//! Cross-binding nRPC wire-format compat — Phase B7.
//!
//! Loads `tests/cross_lang_nrpc/golden_vectors.json` and asserts
//! that the canonical `cross_lang_echo_sum` service round-trips
//! correctly through the Rust runtime. The same fixture drives
//! the Node + Python binding compat tests at:
//!
//!   - `bindings/node/test/cross_lang_compat.test.ts`
//!   - `bindings/python/tests/test_cross_lang_compat.py`
//!
//! See `net/crates/net/README.md#nrpc` for the canonical service
//! contract spec.
//!
//! What this test proves:
//!
//!   - JSON-encoded request bodies decode to the canonical
//!     `EchoSumRequest` shape on the Rust side.
//!   - The handler implements the documented behavior (echo +
//!     left-to-right sum).
//!   - Response encodes back to JSON in a shape semantically equal
//!     to the fixture's `expected_response_json`.
//!   - Malformed requests surface as `RpcStatus::Application(0x4001)`
//!     — the canonical "typed bad request" status that bindings
//!     map to `nrpc:server_error: status=0x4001`.

#![cfg(feature = "cortex")]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use net::adapter::net::cortex::{
    EventMeta, RpcClientFold, RpcClientPending, RpcContext, RpcHandler, RpcHandlerError,
    RpcRequestPayload, RpcResponseEmitter, RpcResponsePayload, RpcServerFold, RpcStatus,
    DISPATCH_RPC_REQUEST, DISPATCH_RPC_RESPONSE, EVENT_META_SIZE,
};
use net::adapter::net::redex::{RedexEntry, RedexEvent, RedexFold};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

const SERVICE_NAME: &str = "cross_lang_echo_sum";
/// Status code emitted by the handler when the request payload
/// can't be decoded into the canonical shape. Matches
/// `NRPC_TYPED_BAD_REQUEST` declared at `sdk/mesh_rpc.rs:61` —
/// in the application-defined range `0x8000..=0xFFFF`.
const NRPC_TYPED_BAD_REQUEST: u16 = 0x8000;

// =====================================================================
// Canonical service shape.
// =====================================================================

#[derive(Deserialize)]
struct EchoSumRequest {
    text: String,
    numbers: Vec<i64>,
}

#[derive(Serialize)]
struct EchoSumResponse {
    echo: String,
    sum: i64,
}

/// Reference handler implementing the canonical contract. Errors
/// from request decoding surface as `RpcStatus::Application(...)`
/// with the typed-bad-request code so the wire response matches
/// what the Node + Python typed handlers emit on the same input.
struct EchoSumHandler;

#[async_trait::async_trait]
impl RpcHandler for EchoSumHandler {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        let req: EchoSumRequest = match serde_json::from_slice(&ctx.payload.body) {
            Ok(v) => v,
            Err(e) => {
                let body = serde_json::json!({
                    "error": "invalid_request",
                    "detail": e.to_string(),
                });
                return Ok(RpcResponsePayload {
                    status: RpcStatus::Application(NRPC_TYPED_BAD_REQUEST),
                    headers: vec![],
                    body: serde_json::to_vec(&body).unwrap(),
                });
            }
        };
        // Saturating sum so a malicious caller can't crash the
        // handler with an i64 overflow. The fixture doesn't
        // exercise overflow; this is defensive.
        let sum = req
            .numbers
            .iter()
            .fold(0i64, |acc, n| acc.saturating_add(*n));
        let resp = EchoSumResponse {
            echo: req.text,
            sum,
        };
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: serde_json::to_vec(&resp).unwrap(),
        })
    }
}

// =====================================================================
// Loopback harness — identical pattern to integration_nrpc_loopback.rs
// (intentionally inlined here so this test file is self-contained
// for downstream readers tracing the canonical contract).
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

fn request_event(caller_origin: u64, call_id: u64, payload: &RpcRequestPayload) -> RedexEvent {
    let meta = EventMeta::new(DISPATCH_RPC_REQUEST, 0, caller_origin, call_id, 0);
    make_event(meta, &payload.encode())
}

fn response_event(caller_origin: u64, call_id: u64, payload: &RpcResponsePayload) -> RedexEvent {
    let meta = EventMeta::new(DISPATCH_RPC_RESPONSE, 0, caller_origin, call_id, 0);
    make_event(meta, &payload.encode())
}

struct Loopback {
    server_fold: Arc<Mutex<RpcServerFold>>,
    pending: Arc<RpcClientPending>,
    next_call_id: AtomicU64,
    caller_origin: u64,
}

impl Loopback {
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
        let server_fold = Arc::new(Mutex::new(RpcServerFold::new(
            Arc::new(EchoSumHandler),
            emit,
        )));
        Self {
            server_fold,
            pending,
            next_call_id: AtomicU64::new(1),
            caller_origin: 0xC1055,
        }
    }

    async fn call_json(&self, body: Vec<u8>) -> RpcResponsePayload {
        let call_id = self.next_call_id.fetch_add(1, Ordering::Relaxed);
        let rx = self.pending.register(call_id);
        let req = RpcRequestPayload {
            service: SERVICE_NAME.to_string(),
            deadline_ns: 0,
            flags: 0,
            headers: vec![],
            body,
        };
        let ev = request_event(self.caller_origin, call_id, &req);
        self.server_fold
            .lock()
            .apply(&ev, &mut ())
            .expect("server fold apply");
        tokio::time::timeout(Duration::from_secs(2), rx)
            .await
            .expect("response within 2s")
            .expect("oneshot delivers")
    }
}

// =====================================================================
// Fixture loader.
// =====================================================================

#[derive(Deserialize)]
struct GoldenFixture {
    service: String,
    abi_version_expected: u32,
    ok_cases: Vec<OkCase>,
    error_cases: Vec<ErrorCase>,
}

#[derive(Deserialize)]
struct OkCase {
    name: String,
    request_json: JsonValue,
    expected_response_json: JsonValue,
}

#[derive(Deserialize)]
struct ErrorCase {
    name: String,
    request_json: JsonValue,
    expected_status: u16,
}

fn load_fixture() -> GoldenFixture {
    let raw = include_str!("cross_lang_nrpc/golden_vectors.json");
    serde_json::from_str(raw).expect("golden_vectors.json is valid JSON")
}

// =====================================================================
// Tests.
// =====================================================================

/// Sanity-check the fixture's metadata up front. If this drifts
/// every other assertion is suspect.
#[test]
fn fixture_metadata_matches_canonical_contract() {
    let fx = load_fixture();
    assert_eq!(
        fx.service, SERVICE_NAME,
        "fixture's service name must match the constant in this file",
    );
    assert_eq!(
        fx.abi_version_expected, 0x0001,
        "fixture ABI version must track NET_RPC_ABI_VERSION (0x0001)",
    );
    assert!(!fx.ok_cases.is_empty(), "ok_cases must not be empty");
    assert!(!fx.error_cases.is_empty(), "error_cases must not be empty");
}

#[tokio::test]
async fn cross_lang_ok_cases_round_trip() {
    let fx = load_fixture();
    let lb = Loopback::new();
    for case in &fx.ok_cases {
        let req_bytes = serde_json::to_vec(&case.request_json).expect("request_json -> bytes");
        let resp = lb.call_json(req_bytes).await;
        assert_eq!(
            resp.status,
            RpcStatus::Ok,
            "ok-case '{}' must return Ok status, got {:?}",
            case.name,
            resp.status,
        );
        let actual: JsonValue = serde_json::from_slice(&resp.body)
            .unwrap_or_else(|_| panic!("ok-case '{}' response is not valid JSON", case.name));
        assert_eq!(
            actual, case.expected_response_json,
            "ok-case '{}' response shape mismatch",
            case.name,
        );
    }
}

#[tokio::test]
async fn cross_lang_error_cases_surface_typed_bad_request() {
    let fx = load_fixture();
    let lb = Loopback::new();
    for case in &fx.error_cases {
        let req_bytes = serde_json::to_vec(&case.request_json).expect("request_json -> bytes");
        let resp = lb.call_json(req_bytes).await;
        match resp.status {
            RpcStatus::Application(code) => assert_eq!(
                code, case.expected_status,
                "error-case '{}' status code mismatch",
                case.name,
            ),
            other => panic!(
                "error-case '{}' expected Application({:#06x}), got {:?}",
                case.name, case.expected_status, other,
            ),
        }
        // Body MUST be JSON-decodable so binding-side decoders don't
        // choke trying to surface the error detail. Shape is
        // intentionally not part of the contract — bindings may
        // inspect or ignore it.
        let _: JsonValue = serde_json::from_slice(&resp.body)
            .unwrap_or_else(|_| panic!("error-case '{}' body must be JSON", case.name));
    }
}

/// Sanity-check that `Application(0x8000)` is the documented
/// typed-bad-request code. If this constant drifts, the binding-
/// side error-mapping tables go out of sync.
#[test]
fn typed_bad_request_code_is_stable() {
    assert_eq!(NRPC_TYPED_BAD_REQUEST, 0x8000);
}
