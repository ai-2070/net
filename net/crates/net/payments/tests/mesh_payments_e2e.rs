//! The payment lifecycle over the real mesh wire: two nodes, real UDP
//! loopback, real handshake — the provider serves the quote/pay nRPC
//! services over its `PaymentEngine`; the caller's flow crosses the wire
//! through `MeshPaymentChannel`. This is the P0 demo's shape: the same
//! code on two hosts is the recorded cross-machine run.
#![cfg(feature = "mesh")]

use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::identity::EntityKeypair;
use net_payments::billing::BillingLog;
use net_payments::core::canonical::{canonical_bytes, SignedEnvelope as _};
use net_payments::core::registry::default_mock_registry;
use net_payments::core::terms::PricingTerms;
use net_payments::core::units::AtomicAmount;
use net_payments::engine::{AdmitAll, PaymentEngine};
use net_payments::facilitator::mock::{MockFacilitator, MOCK_NETWORK, MOCK_SCHEME};
use net_payments::flow::mesh::{serve_payments, MeshPaymentChannel};
use net_payments::flow::{CallerDecision, CallerPaymentFlow, Clock, InProcessProvider};
use net_payments::policy::spend::{SpendPolicyEngine, SpendProfile};
use net_payments::x402::requirements::PaymentRequirements;
use net_payments::x402::X402Carry;
use net_sdk::mesh::{Mesh, MeshBuilder};

struct TestClock(std::sync::atomic::AtomicU64);
impl Clock for TestClock {
    fn now_ns(&self) -> u64 {
        self.0.fetch_add(1_000, std::sync::atomic::Ordering::SeqCst)
    }
}

async fn handshake(server: &Mesh, caller: &Mesh) {
    let server_addr = server.inner().local_addr();
    let server_pub = *server.inner().public_key();
    let server_id = server.inner().node_id();
    let caller_id = caller.inner().node_id();
    let (accept, connect) = tokio::join!(server.inner().accept(caller_id), async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        caller.inner().connect(server_addr, &server_pub, server_id).await
    });
    accept.expect("accept");
    connect.expect("connect");
    server.inner().start();
    caller.inner().start();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_paid_lifecycle_crosses_the_wire() {
    let psk = [0x42u8; 32];
    let dir = tempfile::tempdir().expect("tempdir");
    let clock: Arc<dyn Clock> =
        Arc::new(TestClock(std::sync::atomic::AtomicU64::new(1_000_000_000_000_000)));

    // ── machine B: the provider ────────────────────────────────────
    let provider_mesh = MeshBuilder::new("127.0.0.1:0", &psk)
        .expect("builder")
        .build()
        .await
        .expect("provider mesh");
    let caller_mesh = MeshBuilder::new("127.0.0.1:0", &psk)
        .expect("builder")
        .build()
        .await
        .expect("caller mesh");
    handshake(&provider_mesh, &caller_mesh).await;

    let provider_keys = Arc::new(EntityKeypair::generate());
    let registry = default_mock_registry(provider_keys.entity_id().clone());
    let provider_log = Arc::new(BillingLog::new(dir.path().join("provider-billing.jsonl")));
    let engine = Arc::new(
        PaymentEngine::new(
            provider_keys.clone(),
            Arc::new(MockFacilitator::new()),
            Arc::new(AdmitAll),
            registry.clone(),
            dir.path().join("engine.json"),
        )
        .expect("engine")
        .with_billing_log(provider_log.clone()),
    );
    let in_process = Arc::new(InProcessProvider::new(engine, clock.clone()));
    let _serving = serve_payments(&provider_mesh, in_process).expect("serve payments");

    // The capability id the mesh channel routes by: `<node_id>/<tool>`.
    let capability = format!("{}/fixture-tool", provider_mesh.inner().node_id());

    // The announced pricing (what publish would attach).
    let template = X402Carry::author(&PaymentRequirements {
        scheme: MOCK_SCHEME.into(),
        network: MOCK_NETWORK.into(),
        amount: "2500".into(),
        asset: "musd".into(),
        pay_to: "mock-provider-settle-addr".into(),
        max_timeout_seconds: 60,
        extra: None,
    })
    .expect("author");
    let terms = PricingTerms::new(
        provider_keys.entity_id().clone(),
        capability.clone(),
        vec![template],
        registry.reference().expect("ref"),
    );
    let terms_json =
        String::from_utf8(canonical_bytes(&terms).expect("canonicalize")).expect("utf8");

    // ── machine A: the caller ──────────────────────────────────────
    let caller_keys = Arc::new(EntityKeypair::generate());
    let spend_path = dir.path().join("spend-policy.json");
    let flow = CallerPaymentFlow::new(
        caller_keys,
        SpendPolicyEngine::new(&spend_path, SpendProfile::DevTest),
        registry,
        Arc::new(MeshPaymentChannel::new(Arc::new(caller_mesh))),
        clock,
    );

    // Auto-allow: quote, payload, and settlement all cross the wire;
    // the proof carries the provider-signed billing event back.
    let decision = flow.run(&capability, &terms_json).await;
    let CallerDecision::Paid { quote_id: _, proof } = decision else {
        panic!("expected Paid over the wire, got {decision:?}");
    };
    let billing_json = proof["billing_event"].as_str().expect("billing event");
    let billing =
        net_payments::core::billing_event::BillingEvent::from_json_bytes(billing_json.as_bytes())
            .expect("caller-side verification of the provider-signed event");
    assert_eq!(billing.amount, AtomicAmount::from_u128(2500));
    assert_eq!(billing.capability, capability);

    // Provider side persisted the same single event.
    let recorded = provider_log.read_all().await.expect("read");
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].billing_event_id, billing.billing_event_id);
    recorded[0].verify_signature().expect("signed");

    // ── over-cap → structured hold → approve → redeem, over the wire ─
    let configurer = SpendPolicyEngine::new(&spend_path, SpendProfile::DevTest);
    configurer
        .configure(|defaults, _| {
            defaults.max_per_call = Some(AtomicAmount::from_u128(1000));
        })
        .await
        .expect("configure");

    let held = flow.run(&capability, &terms_json).await;
    let CallerDecision::RequiresPaymentApproval { quote_id, .. } = held else {
        panic!("expected RequiresPaymentApproval, got {held:?}");
    };
    assert_eq!(provider_log.read_all().await.unwrap().len(), 1, "no charge while held");

    configurer.approve(&quote_id).await.expect("approve");
    let redeemed = flow.run(&capability, &terms_json).await;
    let CallerDecision::Paid { quote_id: _, proof } = redeemed else {
        panic!("approval must unblock over the wire, got {redeemed:?}");
    };
    assert_eq!(proof["quote_id"].as_str(), Some(quote_id.as_str()));
    assert_eq!(provider_log.read_all().await.unwrap().len(), 2, "exactly one new charge");
}
