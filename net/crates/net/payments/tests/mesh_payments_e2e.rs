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
        caller
            .inner()
            .connect(server_addr, &server_pub, server_id)
            .await
    });
    accept.expect("accept");
    connect.expect("connect");
    server.inner().start();
    caller.inner().start();
}

/// A provider serving the payment wire over real loopback UDP, and
/// everything a caller needs to transact with it: a whole
/// [`CallerPaymentFlow`] for the lifecycle test, or the pieces to
/// hand-build a quote request for the H1 wire tests below.
///
/// One fixture rather than two so the wire tests exercise the same
/// provider the lifecycle test proves out — a divergence between the two
/// setups would let an H1 refusal pass against a provider configured
/// differently from the one that actually serves money.
struct World {
    caller_mesh: Arc<Mesh>,
    caller_keys: Arc<EntityKeypair>,
    provider_node: u64,
    provider_id: net::adapter::net::identity::EntityId,
    provider_log: Arc<BillingLog>,
    registry: net_payments::core::registry::AssetRegistry,
    capability: String,
    template: X402Carry<PaymentRequirements>,
    terms_json: String,
    clock: Arc<dyn Clock>,
    dir: tempfile::TempDir,
    _serving: net_payments::flow::mesh::PaymentServeHandle,
}

impl World {
    async fn start() -> Self {
        let psk = [0x42u8; 32];
        let dir = tempfile::tempdir().expect("tempdir");
        let clock: Arc<dyn Clock> = Arc::new(TestClock(std::sync::atomic::AtomicU64::new(
            1_000_000_000_000_000,
        )));

        // ── machine B: the provider ───────────────────────────────
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
        let serving = serve_payments(&provider_mesh, in_process).expect("serve payments");

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
            vec![template.clone()],
            registry.reference().expect("ref"),
        );
        let terms_json =
            String::from_utf8(canonical_bytes(&terms).expect("canonicalize")).expect("utf8");

        Self {
            provider_node: provider_mesh.inner().node_id(),
            provider_id: provider_keys.entity_id().clone(),
            provider_log,
            registry,
            caller_mesh: Arc::new(caller_mesh),
            caller_keys: Arc::new(EntityKeypair::generate()),
            capability,
            template,
            terms_json,
            clock,
            dir,
            _serving: serving,
        }
    }

    /// ── machine A: the caller ── a flow whose spend policy lives at
    /// `spend_path`, so a test can reopen the same policy to reconfigure
    /// or approve against it.
    fn caller_flow(&self, spend_path: &std::path::Path) -> CallerPaymentFlow {
        CallerPaymentFlow::new(
            self.caller_keys.clone(),
            SpendPolicyEngine::new(spend_path, SpendProfile::DevTest),
            self.registry.clone(),
            Arc::new(MeshPaymentChannel::new(
                self.caller_mesh.clone(),
                self.caller_keys.clone(),
                self.clock.clone(),
            )),
            self.clock.clone(),
        )
    }

    /// Send a hand-built quote request over the real wire, returning the
    /// provider's refusal text on rejection.
    ///
    /// The text matters. Every negative test here asserts a *specific*
    /// refusal — the transport collapses the typed `QuoteRequestError` to
    /// a string, so `is_err()` alone would also be satisfied by a decode
    /// failure, a dead peer, or a handler panic, and the test would keep
    /// passing after the check it names had been removed.
    async fn request_quote(
        &self,
        request: &net_payments::core::quote_request::QuoteRequest,
    ) -> Result<(), String> {
        use base64::engine::general_purpose::STANDARD as BASE64;
        use base64::Engine as _;
        let bytes = canonical_bytes(request).expect("canonical");
        let reply: Result<QuoteWireReply, _> = self
            .caller_mesh
            .call_typed(
                self.provider_node,
                "net.payments.quote.v1",
                &QuoteWire {
                    request_b64: BASE64.encode(bytes),
                    template_b64: BASE64.encode(self.template.bytes()),
                },
                Default::default(),
            )
            .await;
        reply.map(|_| ()).map_err(|e| e.to_string())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_paid_lifecycle_crosses_the_wire() {
    let w = World::start().await;
    let (capability, terms_json) = (w.capability.clone(), w.terms_json.clone());
    let provider_log = w.provider_log.clone();
    let spend_path = w.dir.path().join("spend-policy.json");
    let flow = w.caller_flow(&spend_path);
    // Auto-allow: quote, payload, and settlement all cross the wire;
    // the proof carries the provider-signed billing event back.
    let decision = flow.run(&capability, &terms_json).await;
    let CallerDecision::Paid {
        quote_id: _,
        binding_sig: _,
        proof,
    } = decision
    else {
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
    assert_eq!(
        provider_log.read_all().await.unwrap().len(),
        1,
        "no charge while held"
    );

    configurer.approve(&quote_id).await.expect("approve");
    let redeemed = flow.run(&capability, &terms_json).await;
    let CallerDecision::Paid {
        quote_id: _,
        binding_sig: _,
        proof,
    } = redeemed
    else {
        panic!("approval must unblock over the wire, got {redeemed:?}");
    };
    assert_eq!(proof["quote_id"].as_str(), Some(quote_id.as_str()));
    assert_eq!(
        provider_log.read_all().await.unwrap().len(),
        2,
        "exactly one new charge"
    );
}

// ---------------------------------------------------------------------
// H1: the caller identity on a quote request is proven, not claimed
// ---------------------------------------------------------------------

/// The wire shape of a quote request, mirrored here on purpose.
///
/// `QuoteWireRequest` is private to the flow, so these tests re-declare
/// it — which makes them a check on the *wire contract* rather than on an
/// internal type, and lets them send requests the real channel would
/// never build.
///
/// The mirror cannot drift silently: every test below finishes by sending
/// a well-formed request and asserting it is *accepted*, so a rename on
/// either side turns into a decode failure on the honest path rather than
/// a refusal test that passes for the wrong reason.
#[derive(serde::Serialize)]
struct QuoteWire {
    request_b64: String,
    template_b64: String,
}

#[derive(serde::Deserialize)]
struct QuoteWireReply {
    #[allow(dead_code)]
    quote_b64: String,
}

/// A forged quote request — naming a victim the attacker holds no key
/// for — is refused over the real wire.
///
/// This is the finding the signed request exists for. The service used to
/// take the caller identity from a body field, and `EntityId` is a public
/// key, so naming an admitted caller cleared provider admission and
/// naming a victim put their identity on the provider's signed billing
/// event. `RpcContext::caller_origin` could not fix it: it is routing
/// metadata carried on the packet header, so comparing it against a body
/// field compares two claims from the same untrusted source.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_forged_caller_identity_is_refused_over_the_wire() {
    use net_payments::core::quote_request::QuoteRequest;

    let w = World::start().await;

    let victim = EntityKeypair::generate();
    let attacker = EntityKeypair::generate();
    let now_ns = w.clock.now_ns();

    // The attacker builds a request naming the victim and signs it with
    // its own key — the exact impersonation the old wire permitted.
    let mut forged = QuoteRequest::new(
        w.provider_id.clone(),
        victim.entity_id().clone(),
        &w.capability,
        w.template.bytes(),
        now_ns,
        30_000_000_000,
        "forged-nonce",
    );
    let payload = net_payments::core::canonical::signed_payload_bytes(&forged).expect("payload");
    let sig = attacker.try_sign(&payload).expect("sign");
    forged.signature = Some(net_payments::core::canonical::SignatureHex(sig.to_bytes()));

    let refusal = w
        .request_quote(&forged)
        .await
        .expect_err("a request signed by anyone but the identity it names must be refused");
    assert!(
        refusal.contains("signature"),
        "the refusal must be the signature check, not some other failure: {refusal}"
    );

    // And the honest path still works, so the refusal is the signature
    // check rather than the wire being broken.
    let mut honest = QuoteRequest::new(
        w.provider_id.clone(),
        victim.entity_id().clone(),
        &w.capability,
        w.template.bytes(),
        w.clock.now_ns(),
        30_000_000_000,
        "honest-nonce",
    );
    honest.sign_with(&victim).expect("sign");
    assert!(
        w.request_quote(&honest).await.is_ok(),
        "the identity that holds the key still gets a quote"
    );
}

/// A verified request cannot be replayed: the nonce is remembered for as
/// long as the request remains presentable.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_replayed_quote_request_is_refused() {
    use net_payments::core::quote_request::QuoteRequest;

    let w = World::start().await;
    let caller = EntityKeypair::generate();
    let mut request = QuoteRequest::new(
        w.provider_id.clone(),
        caller.entity_id().clone(),
        &w.capability,
        w.template.bytes(),
        w.clock.now_ns(),
        30_000_000_000,
        "replay-me",
    );
    request.sign_with(&caller).expect("sign");

    assert!(w.request_quote(&request).await.is_ok(), "first use");
    let refusal = w
        .request_quote(&request)
        .await
        .expect_err("the same signed request must not be presentable twice");
    assert!(
        refusal.contains("nonce was already used"),
        "the refusal must be the replay guard: {refusal}"
    );
}

/// A request signed for one provider cannot be relayed to another, even
/// though it is perfectly well signed — the destination bind.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_request_for_another_provider_is_refused() {
    use net_payments::core::quote_request::QuoteRequest;

    let w = World::start().await;
    let caller = EntityKeypair::generate();
    let elsewhere = EntityKeypair::generate().entity_id().clone();

    let mut request = QuoteRequest::new(
        elsewhere,
        caller.entity_id().clone(),
        &w.capability,
        w.template.bytes(),
        w.clock.now_ns(),
        30_000_000_000,
        "wrong-destination",
    );
    request.sign_with(&caller).expect("sign");
    let refusal = w
        .request_quote(&request)
        .await
        .expect_err("a request addressed to another provider must be refused");
    assert!(
        refusal.contains("addressed to a different provider"),
        "the refusal must be the destination bind: {refusal}"
    );

    // The same request, correctly addressed, is taken — so the refusal
    // above is the bind and not the wire.
    let mut addressed = QuoteRequest::new(
        w.provider_id.clone(),
        caller.entity_id().clone(),
        &w.capability,
        w.template.bytes(),
        w.clock.now_ns(),
        30_000_000_000,
        "right-destination",
    );
    addressed.sign_with(&caller).expect("sign");
    assert!(w.request_quote(&addressed).await.is_ok());
}
