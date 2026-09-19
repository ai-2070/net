//! Paid agent-to-agent admission, end to end in one process (Rust).
//!
//! Run: `cargo run --example a2a_paid`
//!
//! What it proves: a provider that is **explicitly paid** by configuration
//! refuses free work, quotes before any money moves, and runs the task
//! exactly once after the caller pays for that exact reservation. Both halves
//! are real — a `PaymentEngine` behind both the quote/pay wire and the
//! admission gate, a durable admission journal on disk, and an
//! `A2aCallerFlow` with a spend policy and a durable purchase store.
//!
//! The two nodes are in one process only so the example is a single file.
//! Nothing about the flow depends on that: they are separate mesh nodes that
//! complete a real noise handshake over loopback UDP and speak the same wire
//! two machines would.
//!
//! WHAT IT DELIBERATELY DOES NOT DO: settle on a real chain. The facilitator
//! is the mock, which is the only honest choice for an example — a runnable
//! rail needs funded keys and a testnet. The *lifecycle* is real; the
//! settlement is not. See `payments.md`.
//!
//! The order is the whole design: everything that can refuse the work happens
//! BEFORE a quote exists, and the launch is claimed durably BEFORE the
//! executor is spawned.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use net_payments::billing::BillingLog;
use net_payments::core::canonical::canonical_bytes;
use net_payments::core::registry::default_mock_registry;
use net_payments::core::terms::PricingTerms;
use net_payments::engine::{AdmitAll, PaymentEngine};
use net_payments::facilitator::mock::{MockFacilitator, MOCK_NETWORK, MOCK_SCHEME};
use net_payments::flow::a2a::{
    A2aCallerFlow, A2aPrepareError, A2aPurchase, A2aPurchaseStore, A2aSubmit, MeshA2aChannel,
};
use net_payments::flow::mesh::{serve_payments, EngineTaskAdmissionGate, MeshPaymentChannel};
use net_payments::flow::{CallerPaymentFlow, Clock, InProcessProvider, SystemClock};
use net_payments::policy::spend::{SpendPolicyEngine, SpendProfile};
use net_payments::x402::requirements::PaymentRequirements;
use net_payments::x402::X402Carry;
use net_sdk::a2a::{
    A2aBounds, A2aOffer, CancelToken, TaskBrief, TaskExecutor, TaskRegistry, TaskState,
};
use net_sdk::a2a_journal::A2aAdmissionJournal;
use net_sdk::a2a_payment::{TaskAdmissionGate, TaskPaymentProof};
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::mesh_a2a::{A2aFlowError, A2aServiceConfig, A2aServicePolicy, A2A_TASK_SERVICE};
use net_sdk::meshos::EntityKeypair;
use net_sdk::tool_payment::TAG_PAYMENT_FAILURE;

const PSK: [u8; 32] = [0x5au8; 32];
const SERVICE: &str = "summarize";
const REV: &str = "r1";
const AMOUNT: &str = "2500";
const ASSET: &str = "musd";

/// The host's runner. `runs` counts executions **with multiplicity** —
/// exactly-once is the claim, and a set could not tell one run from three.
#[derive(Clone)]
struct Executor {
    runs: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl TaskExecutor for Executor {
    async fn run(&self, brief: TaskBrief, _cancel: CancelToken) -> Result<String, String> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        Ok(format!("blob://summary/{}", brief.task_id))
    }
}

async fn node() -> Mesh {
    MeshBuilder::new("127.0.0.1:0", &PSK)
        .expect("builder")
        .build()
        .await
        .expect("mesh")
}

/// The prepare outcomes worth re-sending: they reserved nothing and quoted
/// nothing, so a second attempt costs nothing and may land. A round trip
/// that never arrived, a provider at capacity, a contended prepare lease,
/// a retryable quote failure. Every other arm is a decision — re-sending it
/// buys the same answer again.
fn prepare_is_retryable(e: &A2aPrepareError) -> bool {
    matches!(
        e,
        A2aPrepareError::Transport(_)
            | A2aPrepareError::Busy
            | A2aPrepareError::InFlight { .. }
            | A2aPrepareError::Quote { retryable: true, .. }
    )
}

/// Complete a real noise handshake, then start both nodes.
///
/// THE CROSS-NODE IDIOM, AND IT IS EASY TO GET WRONG: handshake while both
/// nodes are still unstarted, then start them. A started node's receive loop
/// auto-accepts and races the manual `accept`, which times the handshake out
/// — and the symptom is not a handshake error, it is every later call failing
/// with "no route to target".
async fn handshake(server: &Mesh, caller: &Mesh) {
    let addr = server.inner().local_addr();
    let key = *server.inner().public_key();
    let server_id = server.inner().node_id();
    let (accept, connect) = tokio::join!(server.inner().accept(caller.inner().node_id()), async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        caller.inner().connect(addr, &key, server_id).await
    });
    accept.expect("accept");
    connect.expect("connect");
    server.start();
    caller.start();
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let dir = tempfile::tempdir().expect("tempdir");
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);

    let provider_mesh = Arc::new(node().await);
    let caller_mesh = Arc::new(node().await);
    handshake(&provider_mesh, &caller_mesh).await;
    let provider_node = provider_mesh.inner().node_id();

    // ── provider: ONE engine behind BOTH seams ────────────────────────────
    // `serve_payments` issues the quote and accepts the payment;
    // `EngineTaskAdmissionGate` redeems it. Same engine, same
    // `payment-engine.json`, so the purchase the caller made is the purchase
    // the admission gate reads.
    let keys = Arc::new(EntityKeypair::generate());
    let registry = default_mock_registry(keys.entity_id().clone());
    let billing = Arc::new(BillingLog::new(dir.path().join("billing.jsonl")));
    let engine = Arc::new(
        PaymentEngine::new(
            Arc::clone(&keys),
            Arc::new(MockFacilitator::new()),
            Arc::new(AdmitAll),
            registry.clone(),
            dir.path().join("payment-engine.json"),
        )
        .expect("engine")
        .with_billing_log(Arc::clone(&billing)),
    );
    let _payments = serve_payments(
        &provider_mesh,
        Arc::new(InProcessProvider::new(Arc::clone(&engine), clock.clone())),
    )
    .expect("serve payments");

    // Pricing terms are bound to the exact capability being sold, so a quote
    // for one service can never admit work on another.
    let capability = format!("{provider_node}/{A2A_TASK_SERVICE}/{SERVICE}");
    let template = X402Carry::author(&PaymentRequirements {
        scheme: MOCK_SCHEME.into(),
        network: MOCK_NETWORK.into(),
        amount: AMOUNT.into(),
        asset: ASSET.into(),
        pay_to: "mock-provider-settle-addr".into(),
        max_timeout_seconds: 60,
        extra: None,
    })
    .expect("template");
    let terms = PricingTerms::new(
        keys.entity_id().clone(),
        &capability,
        vec![template],
        registry.reference().expect("registry reference"),
    );
    let offer = A2aOffer {
        service_id: SERVICE.to_string(),
        revision: REV.to_string(),
        description: Some("summarize a filing, for money".to_string()),
        pricing_terms: Some(
            String::from_utf8(canonical_bytes(&terms).expect("canonicalize")).expect("utf8"),
        ),
        bounds: A2aBounds {
            max_prompt_bytes: 1024,
            max_context_refs: 8,
            max_tags: 8,
            max_tag_bytes: 64,
            max_in_flight: 4,
        },
        reservation_ttl_secs: 600,
        reservation_retention_secs: 7 * 24 * 60 * 60,
        retention_secs: 3_600,
    };

    // A paid service MUST carry pricing terms, a payment gate and a journal.
    // Omit any one and `serve_a2a_configured` refuses to start rather than
    // quietly serving the work for free.
    let executor = Executor {
        runs: Arc::new(AtomicUsize::new(0)),
    };
    let journal = A2aAdmissionJournal::open(&dir.path().join("admissions.json"))
        .await
        .expect("open the admission journal");
    let mut services = BTreeMap::new();
    services.insert(SERVICE.to_string(), A2aServicePolicy::Paid(offer.clone()));
    let task_registry = TaskRegistry::new();
    let _serving = provider_mesh
        .serve_a2a_configured(
            task_registry.clone(),
            Arc::new(executor.clone()) as Arc<dyn TaskExecutor>,
            A2aServiceConfig::new(services)
                .with_payment(
                    Arc::new(EngineTaskAdmissionGate::new(Arc::clone(&engine)))
                        as Arc<dyn TaskAdmissionGate>,
                )
                .with_journal(journal),
        )
        .expect("serve the paid catalog");

    // ── caller: the real flow over the real wire ──────────────────────────
    let caller_keys = Arc::new(EntityKeypair::generate());
    let flow = A2aCallerFlow::new(
        Arc::new(CallerPaymentFlow::new(
            Arc::clone(&caller_keys),
            SpendPolicyEngine::new(&dir.path().join("payment-policy.json"), SpendProfile::DevTest),
            registry.clone(),
            Arc::new(MeshPaymentChannel::new(
                Arc::clone(&caller_mesh),
                Arc::clone(&caller_keys),
                clock.clone(),
            )),
            clock.clone(),
        )),
        Arc::new(MeshA2aChannel::new(Arc::clone(&caller_mesh))),
        Arc::new(A2aPurchaseStore::new(&dir.path().join("a2a-purchases.json"))),
        clock.clone(),
    );

    // Discovery doubles as the readiness precondition: describe answering
    // means the dispatch path is up and this caller's reply subscription has
    // propagated. A first call to a fresh peer is genuinely not routable yet.
    let mut discovered = Vec::new();
    for _ in 0..50 {
        if let Ok(offers) = caller_mesh.describe_a2a(provider_node).await {
            discovered = offers;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let offer = discovered.into_iter().next().expect("the announced offer");
    assert!(
        offer.pricing_terms.is_some(),
        "the catalog must publish its price before anyone commits to it"
    );
    println!("discovered: {} rev {} (priced)", offer.service_id, offer.revision);

    let task_id = "filing-42";
    let brief = TaskBrief::new("summarize the quarterly filing")
        .with_task_id(task_id)
        .with_service(SERVICE, REV);

    // 1. PREPARE — validate, preflight, reserve capacity, mint the admission
    //    id, quote. No money moves here. Bounded retry, and only on the
    //    outcomes that left no record behind: a peer still propagating this
    //    caller's reply route answers a first call with exactly those.
    let mut prepared = None;
    let mut last = String::new();
    for _ in 0..40 {
        match flow.prepare_task(provider_node, &offer, &brief).await {
            Ok(p) => {
                prepared = Some(p);
                break;
            }
            Err(e) if prepare_is_retryable(&e) => last = e.to_string(),
            Err(fatal) => panic!("prepare refused, and a re-send cannot change it: {fatal}"),
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let prepared = prepared.unwrap_or_else(|| panic!("prepare never reached the provider: {last}"));
    println!("prepared: no money moved yet");

    // A RESERVATION IS NOT AN ADMISSION. That prepare reserved capacity and
    // minted an admission id for this exact work, and a submit carrying no
    // payment is STILL refused before the executor — with the provider's
    // machine-actionable failure schematic on the reply rather than a prose
    // error string. This is what "didn't pay" looks like on the wire: the
    // real handle, an empty proof.
    let unpaid = TaskPaymentProof {
        quote_id: String::new(),
        binding_sig: Vec::new(),
    };
    match caller_mesh.submit_task_paid(&prepared, &unpaid).await {
        // Optional by type, and the assertion is the point: a refusal MAY
        // arrive with no schematic, and one that does is a much weaker
        // answer — prose a caller has to parse.
        Err(A2aFlowError::PaymentRefused {
            schematic: Some(schematic),
            ..
        }) => {
            assert_eq!(schematic.object, TAG_PAYMENT_FAILURE, "{schematic:?}");
            assert!(!schematic.handler_executed, "{schematic:?}");
            println!(
                "unpaid submit refused at {}: {} (executor never ran)",
                schematic.stage, schematic.reason
            );
        }
        Ok(ack) => panic!("a paid service must refuse unpaid work, answered {ack:?}"),
        Err(other) => panic!("the provider's failure schematic did not survive: {other}"),
    }
    let before = executor.runs.load(Ordering::SeqCst);
    assert_eq!(before, 0, "the executor ran before anyone paid: {before} runs");

    // 2. PURCHASE — consumes THAT quote, against that one reservation. Never
    //    re-quote a live attempt: a fresh quote for the same work is a second
    //    charge.
    match flow.purchase_task(provider_node, task_id).await {
        A2aPurchase::Paid { .. } => println!("purchased: the attempt is durable and resumable"),
        other => panic!("purchase did not complete: {other:?}"),
    }

    // 3. SUBMIT — sends the brief with the stored proof, byte-identical on
    //    every retry. The provider redeems, claims the launch, runs.
    match flow.submit_task(provider_node, task_id).await {
        A2aSubmit::Accepted { .. } => {}
        other => panic!("submit was not accepted: {other:?}"),
    }

    // Poll the observable the claim is about, never a fixed sleep.
    let mut completed = false;
    for _ in 0..200 {
        if let Ok(Some(record)) = caller_mesh.task_status(provider_node, task_id).await {
            if matches!(record.state, TaskState::Completed { .. }) {
                completed = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(completed, "the paid task never completed");

    // The two properties the whole design exists for: the work ran once, and
    // it was charged once.
    let runs = executor.runs.load(Ordering::SeqCst);
    assert_eq!(runs, 1, "expected exactly one run, got {runs}");
    let charges = billing.read_all().await.expect("billing log").len();
    assert_eq!(charges, 1, "expected exactly one charge, got {charges}");

    println!("paid task ran once: task={task_id} charges={charges}");
}
