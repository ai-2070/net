//! Workstream 5 acceptance: the billing stream surface. Subscribers see
//! events as the engine emits them; the log is verified canonical JSONL;
//! export re-emits verifiable lines; idempotent retries never duplicate a
//! record; and a poisoned log line fails loudly on read.

use std::sync::Arc;

use net::adapter::net::identity::EntityKeypair;
use net_payments::billing::{BillingError, BillingLog};
use net_payments::core::canonical::SignedEnvelope as _;
use net_payments::core::registry::default_mock_registry;
use net_payments::core::verification::VerificationTier;
use net_payments::engine::{AdmitAll, PaymentDecision, PaymentEngine};
use net_payments::facilitator::mock::{MockFacilitator, MOCK_NETWORK, MOCK_SCHEME};
use net_payments::x402::payload::PaymentPayload;
use net_payments::x402::requirements::PaymentRequirements;
use net_payments::x402::X402Carry;

const NOW: u64 = 1_000_000_000_000_000;
const CAPABILITY: &str = "fixture-provider/fixture-tool";

fn requirements() -> X402Carry<PaymentRequirements> {
    X402Carry::author(&PaymentRequirements {
        scheme: MOCK_SCHEME.into(),
        network: MOCK_NETWORK.into(),
        amount: "2500".into(),
        asset: "musd".into(),
        pay_to: "mock-provider-settle-addr".into(),
        max_timeout_seconds: 60,
        extra: None,
    })
    .expect("author")
}

struct Setup {
    engine: PaymentEngine,
    log: Arc<BillingLog>,
    caller: EntityKeypair,
    _dir: tempfile::TempDir,
}

fn setup() -> Setup {
    let provider = Arc::new(EntityKeypair::generate());
    let dir = tempfile::tempdir().expect("tempdir");
    let log = Arc::new(BillingLog::new(dir.path().join("billing.jsonl")));
    let engine = PaymentEngine::new(
        provider.clone(),
        Arc::new(MockFacilitator::new()),
        Arc::new(AdmitAll),
        default_mock_registry(provider.entity_id().clone()),
        dir.path().join("engine.json"),
    )
    .expect("engine")
    .with_billing_log(log.clone());
    Setup { engine, log, caller: EntityKeypair::generate(), _dir: dir }
}

async fn pay_once(s: &Setup, nonce: &str, issued_ns: u64) -> PaymentDecision {
    let quote = s
        .engine
        .issue_quote(
            s.caller.entity_id().clone(),
            CAPABILITY,
            requirements(),
            issued_ns,
            60_000_000_000,
        )
        .expect("quote");
    let payload = X402Carry::author(&PaymentPayload {
        x402_version: 2,
        resource: None,
        accepted: quote.requirements.view().clone(),
        payload: serde_json::json!({ "mock_authorization": nonce }),
        extensions: None,
    })
    .expect("payload");
    s.engine
        .accept_payment(&quote, &payload, VerificationTier::Observed, issued_ns + 1)
        .await
        .expect("accept")
}

#[tokio::test]
async fn subscribers_receive_what_the_log_records() {
    let s = setup();
    let mut rx = s.log.subscribe();

    let decision = pay_once(&s, "payer-1", NOW).await;
    let PaymentDecision::Served { billing, .. } = decision else {
        panic!("expected Served");
    };

    // The stream delivered the same signed fact...
    let streamed = rx.try_recv().expect("subscriber got the event");
    assert_eq!(streamed.billing_event_id, billing.billing_event_id);
    streamed.verify_signature().expect("streamed event verifies");

    // ...and the log holds it durably, verified on read.
    let recorded = s.log.read_all().await.unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].billing_event_id, billing.billing_event_id);
}

#[tokio::test]
async fn idempotent_retries_do_not_duplicate_log_records() {
    let s = setup();
    let quote = s
        .engine
        .issue_quote(s.caller.entity_id().clone(), CAPABILITY, requirements(), NOW, 60_000_000_000)
        .unwrap();
    let payload = X402Carry::author(&PaymentPayload {
        x402_version: 2,
        resource: None,
        accepted: quote.requirements.view().clone(),
        payload: serde_json::json!({ "mock_authorization": "payer-1" }),
        extensions: None,
    })
    .unwrap();

    for i in 0..3 {
        let d = s
            .engine
            .accept_payment(&quote, &payload, VerificationTier::Observed, NOW + 1 + i)
            .await
            .unwrap();
        assert!(matches!(d, PaymentDecision::Served { .. }));
    }
    assert_eq!(
        s.log.read_all().await.unwrap().len(),
        1,
        "one idempotency key, one billing record — retries republish nothing"
    );
}

#[tokio::test]
async fn export_reemits_verifiable_jsonl() {
    let s = setup();
    // Distinct quotes (different issuance instants) → distinct events.
    for i in 0..3u64 {
        let d = pay_once(&s, &format!("payer-{i}"), NOW + i * 1_000).await;
        assert!(matches!(d, PaymentDecision::Served { .. }));
    }

    let dest = s._dir.path().join("export.jsonl");
    let count = s.log.export_jsonl(&dest).await.unwrap();
    assert_eq!(count, 3);

    // The export is itself a valid, verifiable log.
    let reread = BillingLog::new(&dest).read_all().await.unwrap();
    assert_eq!(reread.len(), 3);
    let ids: std::collections::BTreeSet<_> =
        reread.iter().map(|e| e.billing_event_id.clone()).collect();
    assert_eq!(ids.len(), 3, "three distinct billing event ids");
}

#[tokio::test]
async fn a_tampered_log_line_fails_loudly_on_read() {
    let s = setup();
    let d = pay_once(&s, "payer-1", NOW).await;
    assert!(matches!(d, PaymentDecision::Served { .. }));

    // Flip a digit inside the recorded amount: the line still parses as
    // JSON but the signature no longer covers it.
    let path = s.log.path().to_path_buf();
    let text = tokio::fs::read_to_string(&path).await.unwrap();
    let tampered = text.replace("\"2500\"", "\"2501\"");
    assert_ne!(text, tampered, "tamper target present");
    tokio::fs::write(&path, tampered).await.unwrap();

    let err = s.log.read_all().await.unwrap_err();
    assert!(matches!(err, BillingError::BadRecord { line: 1, .. }), "got {err:?}");
}
