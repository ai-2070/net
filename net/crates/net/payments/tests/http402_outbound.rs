//! P1 WS6: the two-way door, outbound — a Net agent pays an external
//! x402 HTTP API through the v2 header transport (`PAYMENT-REQUIRED` /
//! `PAYMENT-SIGNATURE` / `PAYMENT-RESPONSE`), under the same spend
//! policy as every other payment. Free resources pass through; policy
//! holds happen BEFORE anything is signed or sent; a server that
//! refuses the payment gets its reservation released.
#![cfg(feature = "http-facilitator")]

use std::sync::Arc;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use net::adapter::net::identity::EntityKeypair;
use net_payments::core::registry::default_mock_registry;
use net_payments::core::units::AtomicAmount;
use net_payments::flow::http402::{X402HttpFlow, X402HttpOutcome};
use net_payments::flow::Clock;
use net_payments::policy::spend::{SpendPolicyEngine, SpendProfile};
use net_payments::x402::payload::PaymentPayload;
use net_payments::x402::X402Carry;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const NOW: u64 = 1_000_000_000_000_000;

struct TestClock;
impl Clock for TestClock {
    fn now_ns(&self) -> u64 {
        NOW
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ServerMode {
    Free,
    PaidAccept,
    PaidReject,
    /// Answer the unpaid probe with a cross-origin 302 — an open
    /// redirect / compromised host trying to draw the signed payment to
    /// another origin.
    Redirect,
}

/// A paid-resource fixture speaking the v2 header transport.
struct PaidServer {
    url: String,
    mode: Arc<parking_lot::Mutex<ServerMode>>,
    received_payloads: Arc<parking_lot::Mutex<Vec<Vec<u8>>>>,
}

impl PaidServer {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let url = format!("http://{}/resource", listener.local_addr().expect("addr"));
        let mode = Arc::new(parking_lot::Mutex::new(ServerMode::PaidAccept));
        let received = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let mode_task = mode.clone();
        let received_task = received.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let mode = mode_task.clone();
                let received = received_task.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 4096];
                    let header_end = loop {
                        let Ok(n) = stream.read(&mut tmp).await else {
                            return;
                        };
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break pos + 4;
                        }
                    };
                    let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
                    let payment_signature = head
                        .lines()
                        .filter_map(|l| l.split_once(':'))
                        .find(|(k, _)| k.eq_ignore_ascii_case("payment-signature"))
                        .map(|(_, v)| v.trim().to_string());

                    let current = *mode.lock();
                    let response = match (current, payment_signature) {
                        (ServerMode::Free, _) => http_response("200 OK", &[], b"free lunch"),
                        (ServerMode::Redirect, _) => http_response(
                            "302 Found",
                            &[("location", "http://evil.example.test/resource")],
                            b"",
                        ),
                        (_, None) => {
                            // Demand payment: mock-network requirements so
                            // the test settles without a chain.
                            let required = serde_json::json!({
                                "x402Version": 2,
                                "error": "payment required",
                                "resource": { "url": "/resource" },
                                "accepts": [{
                                    "scheme": "mock",
                                    "network": "mock:net",
                                    "amount": "2500",
                                    "asset": "musd",
                                    "payTo": "external-server-account",
                                    "maxTimeoutSeconds": 60
                                }]
                            });
                            let header = BASE64.encode(required.to_string());
                            http_response(
                                "402 Payment Required",
                                &[("payment-required", &header)],
                                b"",
                            )
                        }
                        (ServerMode::PaidAccept, Some(b64)) => {
                            if let Ok(bytes) = BASE64.decode(b64.as_bytes()) {
                                received.lock().push(bytes);
                            }
                            let settlement = serde_json::json!({
                                "success": true,
                                "transaction": "ext:settled-1",
                                "network": "mock:net",
                                "amount": "2500"
                            });
                            let header = BASE64.encode(settlement.to_string());
                            http_response(
                                "200 OK",
                                &[("payment-response", &header)],
                                b"the paid content",
                            )
                        }
                        (ServerMode::PaidReject, Some(_)) => http_response(
                            "402 Payment Required",
                            &[],
                            b"payment verification failed",
                        ),
                    };
                    let _ = stream.write_all(response.as_bytes()).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        Self {
            url,
            mode,
            received_payloads: received,
        }
    }

    fn set_mode(&self, mode: ServerMode) {
        *self.mode.lock() = mode;
    }
}

fn http_response(status: &str, headers: &[(&str, &str)], body: &[u8]) -> String {
    let mut out = format!(
        "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n",
        body.len()
    );
    for (name, value) in headers {
        out.push_str(&format!("{name}: {value}\r\n"));
    }
    out.push_str("\r\n");
    out.push_str(&String::from_utf8_lossy(body));
    out
}

fn flow(profile: SpendProfile, dir: &tempfile::TempDir) -> X402HttpFlow {
    let caller = Arc::new(EntityKeypair::generate());
    let registry = default_mock_registry(caller.entity_id().clone());
    X402HttpFlow::new(
        caller,
        SpendPolicyEngine::new(dir.path().join("spend.json"), profile),
        registry,
        Arc::new(TestClock),
    )
    .expect("flow")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn free_resources_pass_through_untouched() {
    let server = PaidServer::start().await;
    server.set_mode(ServerMode::Free);
    let dir = tempfile::tempdir().expect("tempdir");
    let outcome = flow(SpendProfile::DevTest, &dir)
        .fetch_paid(&server.url)
        .await;
    let X402HttpOutcome::Ok { status, body } = outcome else {
        panic!("expected Ok passthrough, got {outcome:?}");
    };
    assert_eq!(status, 200);
    assert_eq!(body, b"free lunch");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_paid_fetch_settles_under_policy_and_lands_the_settlement_header() {
    let server = PaidServer::start().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let f = flow(SpendProfile::DevTest, &dir);

    let outcome = f.fetch_paid(&server.url).await;
    let X402HttpOutcome::Paid {
        status,
        body,
        settlement,
    } = outcome
    else {
        panic!("expected Paid, got {outcome:?}");
    };
    assert_eq!(status, 200);
    assert_eq!(body, b"the paid content");
    let settlement = settlement.expect("PAYMENT-RESPONSE parsed");
    assert_eq!(settlement.view().transaction, "ext:settled-1");

    // What the server received: a valid x402 v2 payload accepting its
    // exact requirements. (Scoped: the guard must not live across the
    // spend-engine await below.)
    {
        let received = server.received_payloads.lock();
        assert_eq!(received.len(), 1);
        let payload: X402Carry<PaymentPayload> =
            X402Carry::from_bytes(received[0].clone()).expect("server got a valid payload");
        assert_eq!(payload.view().accepted.pay_to, "external-server-account");
        assert_eq!(payload.view().accepted.amount, "2500");
    }

    // The spend landed in the day counter, keyed by the external host's
    // demand.
    let spend = SpendPolicyEngine::new(dir.path().join("spend.json"), SpendProfile::DevTest);
    assert_eq!(
        spend.spent_today("mock:net", "musd", NOW).await.unwrap(),
        AtomicAmount::from_u128(2500)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn policy_holds_before_anything_is_signed_or_sent() {
    let server = PaidServer::start().await;
    let dir = tempfile::tempdir().expect("tempdir");
    // Production profile: mock spends hold for approval.
    let f = flow(SpendProfile::Production, &dir);

    let outcome = f.fetch_paid(&server.url).await;
    let X402HttpOutcome::RequiresPaymentApproval {
        quote_id,
        policy_reason,
        ..
    } = outcome
    else {
        panic!("expected the approval hold, got {outcome:?}");
    };
    assert!(!quote_id.is_empty());
    assert!(
        policy_reason.contains("dev/test profile"),
        "{policy_reason}"
    );
    assert!(
        server.received_payloads.lock().is_empty(),
        "no payment left the machine while policy holds"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rejected_payment_releases_the_reservation() {
    let server = PaidServer::start().await;
    server.set_mode(ServerMode::PaidReject);
    let dir = tempfile::tempdir().expect("tempdir");
    let f = flow(SpendProfile::DevTest, &dir);

    let outcome = f.fetch_paid(&server.url).await;
    let X402HttpOutcome::PaymentRejected { status, .. } = outcome else {
        panic!("expected PaymentRejected, got {outcome:?}");
    };
    assert_eq!(status, 402);

    // Nothing settled per the transport; the day budget is whole again.
    let spend = SpendPolicyEngine::new(dir.path().join("spend.json"), SpendProfile::DevTest);
    assert_eq!(
        spend.spent_today("mock:net", "musd", NOW).await.unwrap(),
        AtomicAmount::from_u128(0)
    );
}

/// H2 regression: a cross-origin redirect on the fetch must be refused,
/// never followed. Following it would (a) let the demand — and the
/// pay_to/amount it dictates — be authored by the redirect target while
/// the capability key still reads the original host, and (b) hand the
/// signed PAYMENT-SIGNATURE to that target on the paid retry. Nothing is
/// signed, nothing is sent, no reservation is taken.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cross_origin_redirect_is_refused_and_nothing_is_signed() {
    let server = PaidServer::start().await;
    server.set_mode(ServerMode::Redirect);
    let dir = tempfile::tempdir().expect("tempdir");
    let f = flow(SpendProfile::DevTest, &dir);

    let outcome = f.fetch_paid(&server.url).await;
    let X402HttpOutcome::Failed { message, .. } = outcome else {
        panic!("expected Failed on a redirect, got {outcome:?}");
    };
    assert!(
        message.contains("redirect"),
        "failure must name the refused redirect: {message}"
    );
    assert!(
        server.received_payloads.lock().is_empty(),
        "no payment left the machine on a refused redirect"
    );

    // No reservation was ever taken, so the day budget is untouched.
    let spend = SpendPolicyEngine::new(dir.path().join("spend.json"), SpendProfile::DevTest);
    assert_eq!(
        spend.spent_today("mock:net", "musd", NOW).await.unwrap(),
        AtomicAmount::from_u128(0)
    );
}
