//! P1 WS1 conformance: the HTTP facilitator client against an
//! in-process fixture server speaking the pinned x402 v2 facilitator
//! API — request shape, byte-preservation across the wire, the spec
//! error vocabulary riding inside responses, transport-failure mapping,
//! `/supported` validation, and the unchanged P0 engine settling through
//! the HTTP client (the "zero interface changes" acceptance).
#![cfg(feature = "http-facilitator")]

use std::sync::Arc;

use net::adapter::net::identity::EntityKeypair;
use net_payments::core::registry::default_mock_registry;
use net_payments::core::verification::VerificationTier;
use net_payments::engine::{AdmitAll, PaymentDecision, PaymentEngine, RejectReason};
use net_payments::facilitator::client::{HttpFacilitator, NoAuth};
use net_payments::facilitator::{Facilitator, FacilitatorErrorKind};
use net_payments::x402::payload::PaymentPayload;
use net_payments::x402::requirements::PaymentRequirements;
use net_payments::x402::X402Carry;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

// ---------------------------------------------------------------------
// The fixture server: minimal HTTP/1.1, spec-shaped answers, scripted
// behavior, and capture of the raw request bodies for byte-preservation
// assertions.
// ---------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum FixtureMode {
    Success,
    InsufficientFunds,
    ServerError,
    Forbidden,
}

/// Captured `(method path, raw body)` pairs, newest last.
type CapturedBodies = Arc<parking_lot::Mutex<Vec<(String, Vec<u8>)>>>;

struct Fixture {
    addr: String,
    mode: Arc<parking_lot::Mutex<FixtureMode>>,
    bodies: CapturedBodies,
}

impl Fixture {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = format!("http://{}", listener.local_addr().expect("addr"));
        let mode = Arc::new(parking_lot::Mutex::new(FixtureMode::Success));
        let bodies = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let mode_task = mode.clone();
        let bodies_task = bodies.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else { return };
                let mode = mode_task.clone();
                let bodies = bodies_task.clone();
                tokio::spawn(async move {
                    let Some((method_path, body)) = read_request(&mut stream).await else {
                        return;
                    };
                    bodies.lock().push((method_path.clone(), body));
                    let current = *mode.lock();
                    let (status, response) = respond(&method_path, current);
                    let head = format!(
                        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        response.len()
                    );
                    let _ = stream.write_all(head.as_bytes()).await;
                    let _ = stream.write_all(response.as_bytes()).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        Self { addr, mode, bodies }
    }

    fn set_mode(&self, mode: FixtureMode) {
        *self.mode.lock() = mode;
    }

    fn last_body_for(&self, method_path: &str) -> Option<Vec<u8>> {
        self.bodies
            .lock()
            .iter()
            .rev()
            .find(|(mp, _)| mp == method_path)
            .map(|(_, b)| b.clone())
    }
}

async fn read_request(stream: &mut tokio::net::TcpStream) -> Option<(String, Vec<u8>)> {
    let mut buf = Vec::with_capacity(1024);
    let mut tmp = [0u8; 1024];
    let header_end = loop {
        let n = stream.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        if buf.len() > 64 * 1024 {
            return None;
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let mut lines = head.lines();
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?;
    let path = parts.next()?;
    let content_length: usize = lines
        .filter_map(|l| l.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse().ok())
        .unwrap_or(0);
    let mut body = buf[header_end..].to_vec();
    while body.len() < content_length {
        let n = stream.read(&mut tmp).await.ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    Some((format!("{method} {path}"), body))
}

fn respond(method_path: &str, mode: FixtureMode) -> (&'static str, String) {
    match (method_path, mode) {
        (_, FixtureMode::ServerError) => {
            ("500 Internal Server Error", r#"{"error":"facilitator degraded"}"#.to_string())
        }
        (_, FixtureMode::Forbidden) => {
            ("403 Forbidden", r#"{"error":"missing api key"}"#.to_string())
        }
        ("GET /supported", _) => (
            "200 OK",
            r#"{"kinds":[{"x402Version":2,"scheme":"mock","network":"mock:net"},{"x402Version":2,"scheme":"exact","network":"eip155:84532"}],"extensions":[],"signers":{"eip155:*":["0x1234"]}}"#
                .to_string(),
        ),
        ("POST /verify", FixtureMode::Success) => {
            ("200 OK", r#"{"isValid":true,"payer":"0xPayerAddress"}"#.to_string())
        }
        ("POST /verify", FixtureMode::InsufficientFunds) => (
            "200 OK",
            r#"{"isValid":false,"invalidReason":"insufficient_funds","payer":"0xPayerAddress"}"#
                .to_string(),
        ),
        ("POST /settle", FixtureMode::Success) => (
            "200 OK",
            r#"{"success":true,"payer":"0xPayerAddress","transaction":"0xfeedbead","network":"mock:net","amount":"2500"}"#
                .to_string(),
        ),
        ("POST /settle", FixtureMode::InsufficientFunds) => (
            "200 OK",
            r#"{"success":false,"errorReason":"insufficient_funds","transaction":"","network":"mock:net"}"#
                .to_string(),
        ),
        _ => ("404 Not Found", r#"{"error":"no such route"}"#.to_string()),
    }
}

// ---------------------------------------------------------------------
// Payment fixtures (mock scheme — the client is scheme/network
// agnostic; the engine's registry knows the mock asset).
// ---------------------------------------------------------------------

fn requirements() -> X402Carry<PaymentRequirements> {
    // Deliberately quirky formatting: byte-preservation across HTTP is
    // only meaningful if canonicalization would have changed the bytes.
    let json = "{ \"scheme\": \"mock\",\n  \"network\": \"mock:net\", \"amount\": \"2500\", \"asset\": \"musd\", \"payTo\": \"mock-provider-settle-addr\", \"maxTimeoutSeconds\": 60 }";
    X402Carry::from_bytes(json.as_bytes().to_vec()).expect("requirements")
}

fn payload_for(requirements: &X402Carry<PaymentRequirements>) -> X402Carry<PaymentPayload> {
    X402Carry::author(&PaymentPayload {
        x402_version: 2,
        resource: None,
        accepted: requirements.view().clone(),
        payload: serde_json::json!({ "mock_authorization": "payer-http" }),
        extensions: None,
    })
    .expect("payload")
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_bodies_embed_the_carry_bytes_verbatim() {
    let fixture = Fixture::start().await;
    let client = HttpFacilitator::new(&fixture.addr, Arc::new(NoAuth)).expect("client");
    let reqs = requirements();
    let pay = payload_for(&reqs);

    let outcome = client.verify(&pay, &reqs).await.expect("verify");
    assert!(outcome.response.view().is_valid);
    assert_eq!(outcome.tier, VerificationTier::Observed);

    // The preserved bytes appear inside the request body EXACTLY —
    // whitespace quirks and all. Re-serialization would have normalized
    // them and broken this.
    let body = fixture.last_body_for("POST /verify").expect("captured body");
    let body_str = String::from_utf8(body).expect("utf8");
    assert!(
        body_str.contains(reqs.as_json_str()),
        "requirements bytes were re-serialized on the way to the facilitator"
    );
    assert!(
        body_str.contains(pay.as_json_str()),
        "payload bytes were re-serialized on the way to the facilitator"
    );
    assert!(body_str.contains("\"x402Version\":2"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spec_vocabulary_rides_inside_responses_not_as_transport_errors() {
    let fixture = Fixture::start().await;
    fixture.set_mode(FixtureMode::InsufficientFunds);
    let client = HttpFacilitator::new(&fixture.addr, Arc::new(NoAuth)).expect("client");
    let reqs = requirements();
    let pay = payload_for(&reqs);

    // isValid: false is the facilitator's ANSWER — Ok(...), engine judges.
    let verify = client.verify(&pay, &reqs).await.expect("verify transport ok");
    assert!(!verify.response.view().is_valid);
    assert_eq!(
        verify.response.view().invalid_reason.as_deref(),
        Some("insufficient_funds"),
        "the spec's verbatim reason is preserved"
    );

    let settle = client.settle(&pay, &reqs).await.expect("settle transport ok");
    assert!(!settle.response.view().success);
    assert_eq!(settle.response.view().error_reason.as_deref(), Some("insufficient_funds"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transport_failures_map_to_structured_retryability() {
    let fixture = Fixture::start().await;
    let client = HttpFacilitator::new(&fixture.addr, Arc::new(NoAuth)).expect("client");
    let reqs = requirements();
    let pay = payload_for(&reqs);

    // 5xx: the facilitator is degraded — retryable, policy decides.
    fixture.set_mode(FixtureMode::ServerError);
    let err = client.verify(&pay, &reqs).await.expect_err("5xx is an error");
    assert_eq!(err.kind, FacilitatorErrorKind::Unavailable);
    assert!(err.retryable);

    // 4xx: a terminal answer about this request — never retried.
    fixture.set_mode(FixtureMode::Forbidden);
    let err = client.settle(&pay, &reqs).await.expect_err("4xx is an error");
    assert_eq!(err.kind, FacilitatorErrorKind::Rejected);
    assert!(!err.retryable);

    // Unreachable endpoint: retryable unavailability.
    let dead = HttpFacilitator::new("http://127.0.0.1:1", Arc::new(NoAuth)).expect("client");
    let err = dead.verify(&pay, &reqs).await.expect_err("no listener");
    assert!(err.retryable, "connect failure must be retryable: {err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supported_validation_passes_offered_pairs_and_refuses_others() {
    let fixture = Fixture::start().await;
    let client = HttpFacilitator::new(&fixture.addr, Arc::new(NoAuth)).expect("client");

    let supported = client
        .validate_pairs(&[("mock".into(), "mock:net".into()), ("exact".into(), "eip155:84532".into())])
        .await
        .expect("offered pairs validate");
    assert_eq!(supported.kinds.len(), 2);
    assert!(supported.signers.contains_key("eip155:*"));

    let err = client
        .validate_pairs(&[("exact".into(), "eip155:8453".into())])
        .await
        .expect_err("unoffered pair must refuse the configuration");
    assert_eq!(err.kind, FacilitatorErrorKind::Rejected);
    assert!(err.to_string().contains("eip155:8453"), "{err}");
}

/// The P0 acceptance comes due: the unchanged engine settles through the
/// HTTP client — same lifecycle, same events, same billing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_unchanged_engine_settles_through_the_http_client() {
    let fixture = Fixture::start().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let provider = Arc::new(EntityKeypair::generate());
    let caller = EntityKeypair::generate();
    let engine = PaymentEngine::new(
        provider.clone(),
        Arc::new(HttpFacilitator::new(&fixture.addr, Arc::new(NoAuth)).expect("client")),
        Arc::new(AdmitAll),
        default_mock_registry(provider.entity_id().clone()),
        dir.path().join("engine.json"),
    )
    .expect("engine");

    let quote = engine
        .issue_quote(
            caller.entity_id().clone(),
            "fixture-provider/fixture-tool",
            requirements(),
            1_000_000_000_000_000,
            60_000_000_000,
        )
        .expect("quote");
    let payload = payload_for(&quote.requirements);

    let decision = engine
        .accept_payment(&quote, &payload, VerificationTier::Observed, 1_000_000_000_000_001)
        .await
        .expect("accept");
    let PaymentDecision::Served { billing, tier } = decision else {
        panic!("expected Served through HTTP, got {decision:?}");
    };
    assert_eq!(tier, VerificationTier::Observed, "a receipt is observed, never more");
    assert_eq!(billing.transaction.as_deref(), Some("0xfeedbead"));
    assert_eq!(billing.amount.to_canonical_string(), "2500");

    // And a facilitator-rejected payment through the same path is the
    // engine's structured rejection, with the spec's verbatim reason.
    fixture.set_mode(FixtureMode::InsufficientFunds);
    let quote2 = engine
        .issue_quote(
            caller.entity_id().clone(),
            "fixture-provider/fixture-tool",
            requirements(),
            1_000_000_000_000_100,
            60_000_000_000,
        )
        .expect("quote2");
    let payload2 = X402Carry::author(&PaymentPayload {
        x402_version: 2,
        resource: None,
        accepted: quote2.requirements.view().clone(),
        payload: serde_json::json!({ "mock_authorization": "payer-http-2" }),
        extensions: None,
    })
    .expect("payload2");
    let rejected = engine
        .accept_payment(&quote2, &payload2, VerificationTier::Observed, 1_000_000_000_000_101)
        .await
        .expect("accept2");
    match rejected {
        PaymentDecision::Rejected { reason: RejectReason::VerifyRejected(r) } => {
            assert_eq!(r, "insufficient_funds")
        }
        other => panic!("expected VerifyRejected(insufficient_funds), got {other:?}"),
    }
}
