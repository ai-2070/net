//! Paid agent-to-agent admission, end to end over the real mesh wire
//! with the **real payment engine** on both seams
//! (`docs/internal/plans/A2A_PAID_ADMISSION_PLAN.md`, WS-D).
//!
//! Every other suite in this slice proves one half. `sdk/tests/`'s
//! `a2a_paid_admission` drives the configured serving path against a
//! *scripted* gate; `a2a_caller_purchase` drives the caller's durable
//! attempt against a *scripted* provider. This file is the composition
//! neither covers:
//!
//! * the provider is a real [`Mesh`] node serving
//!   [`Mesh::serve_a2a_configured`] with a `Paid` catalog entry, a real
//!   [`PaymentEngine`] behind **both** seams — `serve_payments` (quote
//!   and pay) and [`EngineTaskAdmissionGate`] (redeem) — and a real
//!   [`A2aAdmissionJournal`] in a tempdir;
//! * the requester is a real [`A2aCallerFlow`] over [`MeshA2aChannel`],
//!   [`MeshPaymentChannel`], a real [`SpendPolicyEngine`] and a real
//!   [`A2aPurchaseStore`] in a second tempdir.
//!
//! So the quote a caller pays for is the quote the provider's gate
//! redeems, the purchase hash the caller's quote commits to is the one
//! the provider computes from its own reservation, and every counter
//! below is read out of a production artifact: the provider's
//! [`BillingLog`] file, the engine's `payment-engine.json`, the
//! journal's launch ledger, the executor's own run counter.
//!
//! # What is scripted, and what is not
//!
//! Nothing decides anything on behalf of production code. Three test
//! types wrap production types and do not replace them:
//!
//! * [`CountingGate`] delegates every `redeem` to
//!   [`EngineTaskAdmissionGate`] and counts the calls. It is how "no
//!   second redemption" is observed — S6′ never calls the gate at all.
//! * [`WirePayments`] delegates to [`MeshPaymentChannel`], counts quote
//!   calls and records every payload **with multiplicity**, and can
//!   lose exactly one pay reply *after* the provider has processed it.
//!   A lost reply is not a lost payment; there is no other way to ask a
//!   real wire for one on demand.
//! * [`WireTasks`] does the same for [`MeshA2aChannel`]'s submit.
//!
//! The one fixture-authored provider state is the crash window in
//! `a_provider_restart_between_redeem_and_claim_reconciles_the_original_payment`,
//! and it is authored **through the production writers**: the real gate
//! redeems against the real engine, and the real store performs the
//! exact S6 transition the handler performs. See that test's own note —
//! a handler cannot be interrupted between its two journal writes from
//! outside the process, and every restart witness here genuinely drops
//! and re-opens the owning objects rather than re-calling a verb.

#![cfg(feature = "mesh")]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use net::adapter::net::identity::{EntityId, EntityKeypair};
use net_payments::billing::BillingLog;
use net_payments::core::canonical::canonical_bytes;
use net_payments::core::registry::{default_mock_registry, AssetRegistry};
use net_payments::core::terms::PricingTerms;
use net_payments::engine::{AdmitAll, PaymentEngine};
use net_payments::facilitator::mock::{MockFacilitator, MOCK_NETWORK, MOCK_SCHEME};
use net_payments::flow::a2a::{
    A2aCallerFlow, A2aProviderChannel, A2aPurchase, A2aPurchaseStore, A2aSubmit, MeshA2aChannel,
    StateTag as PurchaseTag,
};
use net_payments::flow::mesh::{
    serve_payments, EngineTaskAdmissionGate, MeshPaymentChannel, PaymentServeHandle,
};
use net_payments::flow::{
    CallerPaymentFlow, ChannelError, Clock, InProcessProvider, PayResponse, ProviderChannel,
};
use net_payments::policy::spend::{SpendPolicyEngine, SpendProfile};
use net_payments::x402::payload::PaymentPayload;
use net_payments::x402::requirements::PaymentRequirements;
use net_payments::x402::X402Carry;
use net_sdk::a2a::{
    A2aBounds, A2aOffer, CancelToken, PreparedTask, TaskAck, TaskBrief, TaskExecutor, TaskOwner,
    TaskRegistry, TaskState,
};
use net_sdk::a2a_journal::{
    now_secs, A2aAdmissionJournal, A2aJournalError, AdmissionRecord, AdmissionState,
    AdmissionStore, StateTag,
};
use net_sdk::a2a_journal::{RecoveredAdmission, DETAIL_PAID_NOT_STARTED};
use net_sdk::a2a_payment::{
    TaskAdmissionGate, TaskPaymentClaim, TaskPaymentEvidence, TaskPaymentProof,
};
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::mesh_a2a::{
    A2aFlowError, A2aServiceConfig, A2aServicePolicy, A2aServing, A2A_TASK_SERVICE,
};
use net_sdk::tool_payment::GateDenial;

const PSK: [u8; 32] = [0x5au8; 32];
const SERVICE: &str = "svc.summarize";
const REV: &str = "r1";
/// The mock scheme's amount, in atomic units of `musd`.
const AMOUNT: &str = "2500";
const ASSET: &str = "musd";
/// What the executor returns — asserted, so a run is never inferred
/// from a state label alone.
const RESULT: &str = "blob://a2a-summary-42";
/// A fixed engine clock. Nothing here is about expiry, and a clock that
/// drifts on its own would make the quote-reuse witnesses about luck.
const NOW_NS: u64 = 1_740_672_000_000_000_000;

// ---------------------------------------------------------------------------
// Probes: the executor, the gate, and the two wire decorators
// ---------------------------------------------------------------------------

struct FixedClock(AtomicU64);

impl Clock for FixedClock {
    fn now_ns(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// The host's runner, instrumented. `runs` counts executions **with
/// multiplicity** — this file's whole subject is exactly-once, and a set
/// could not tell one run from three.
#[derive(Clone)]
struct ExecProbe {
    runs: Arc<AtomicUsize>,
    ran: Arc<tokio::sync::Semaphore>,
}

impl ExecProbe {
    fn new() -> Self {
        Self {
            runs: Arc::new(AtomicUsize::new(0)),
            ran: Arc::new(tokio::sync::Semaphore::new(0)),
        }
    }

    fn runs(&self) -> usize {
        self.runs.load(Ordering::SeqCst)
    }

    fn executor(&self) -> Arc<dyn TaskExecutor> {
        Arc::new(self.clone())
    }

    /// Block until a run has returned — a precondition, never a sleep.
    async fn wait_ran(&self) {
        let permit = tokio::time::timeout(Duration::from_secs(20), self.ran.acquire())
            .await
            .expect("no executor run ever completed")
            .expect("semaphore");
        permit.forget();
    }
}

#[async_trait::async_trait]
impl TaskExecutor for ExecProbe {
    async fn run(&self, _brief: TaskBrief, _cancel: CancelToken) -> Result<String, String> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        self.ran.add_permits(1);
        Ok(RESULT.to_string())
    }
}

/// The **real** engine gate, counted. Every decision is
/// [`EngineTaskAdmissionGate`]'s; the counter is what makes "the gate
/// was not called" (S6′) and "exactly one redemption" observable.
struct CountingGate {
    inner: EngineTaskAdmissionGate,
    calls: AtomicUsize,
}

impl CountingGate {
    fn new(engine: Arc<PaymentEngine>) -> Arc<Self> {
        Arc::new(Self {
            inner: EngineTaskAdmissionGate::new(engine),
            calls: AtomicUsize::new(0),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl TaskAdmissionGate for CountingGate {
    async fn redeem(&self, claim: TaskPaymentClaim<'_>) -> Result<TaskPaymentEvidence, GateDenial> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.redeem(claim).await
    }
}

/// What the next pay call over the wire does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PayMode {
    /// Straight through to the provider.
    Forward,
    /// The provider processes it — the money moves, the billing event is
    /// written — and the reply is lost on the way back. The caller
    /// learns nothing.
    SwallowOnce,
}

/// [`MeshPaymentChannel`], counted, with one injectable lost reply.
struct WirePayments {
    inner: MeshPaymentChannel,
    quote_calls: AtomicUsize,
    payloads: parking_lot::Mutex<Vec<Vec<u8>>>,
    mode: parking_lot::Mutex<PayMode>,
}

impl WirePayments {
    fn new(inner: MeshPaymentChannel) -> Arc<Self> {
        Arc::new(Self {
            inner,
            quote_calls: AtomicUsize::new(0),
            payloads: parking_lot::Mutex::new(Vec::new()),
            mode: parking_lot::Mutex::new(PayMode::Forward),
        })
    }

    fn quote_calls(&self) -> usize {
        self.quote_calls.load(Ordering::SeqCst)
    }

    fn pay_sends(&self) -> usize {
        self.payloads.lock().len()
    }

    fn distinct_payloads(&self) -> usize {
        let mut seen = self.payloads.lock().clone();
        seen.sort();
        seen.dedup();
        seen.len()
    }

    fn lose_next_pay_reply(&self) {
        *self.mode.lock() = PayMode::SwallowOnce;
    }
}

#[async_trait::async_trait]
impl ProviderChannel for WirePayments {
    async fn quote(
        &self,
        caller: &EntityId,
        provider: &EntityId,
        capability: &str,
        template: &X402Carry<PaymentRequirements>,
        input_hash: Option<&str>,
    ) -> Result<Vec<u8>, ChannelError> {
        self.quote_calls.fetch_add(1, Ordering::SeqCst);
        assert!(
            input_hash.is_some(),
            "a task quote must be bound to the reservation's purchase hash"
        );
        self.inner
            .quote(caller, provider, capability, template, input_hash)
            .await
    }

    async fn pay(
        &self,
        quote_bytes: &[u8],
        payload: &X402Carry<PaymentPayload>,
    ) -> Result<PayResponse, ChannelError> {
        self.payloads.lock().push(payload.bytes().to_vec());
        let mode = *self.mode.lock();
        // The provider always sees the payment; only the reply is lost.
        let landed = self.inner.pay(quote_bytes, payload).await;
        match mode {
            PayMode::Forward => landed,
            PayMode::SwallowOnce => {
                *self.mode.lock() = PayMode::Forward;
                let _processed_by_the_provider = landed;
                Err(ChannelError {
                    message: "the pay reply was lost in transit".to_string(),
                    retryable: true,
                })
            }
        }
    }
}

/// [`MeshA2aChannel`], counted, with one injectable lost submit reply.
struct WireTasks {
    inner: MeshA2aChannel,
    prepare_calls: AtomicUsize,
    submit_calls: AtomicUsize,
    lose_submit_reply: AtomicBool,
}

impl WireTasks {
    fn new(inner: MeshA2aChannel) -> Arc<Self> {
        Arc::new(Self {
            inner,
            prepare_calls: AtomicUsize::new(0),
            submit_calls: AtomicUsize::new(0),
            lose_submit_reply: AtomicBool::new(false),
        })
    }

    fn prepare_calls(&self) -> usize {
        self.prepare_calls.load(Ordering::SeqCst)
    }

    fn submit_calls(&self) -> usize {
        self.submit_calls.load(Ordering::SeqCst)
    }

    fn lose_next_submit_reply(&self) {
        self.lose_submit_reply.store(true, Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl A2aProviderChannel for WireTasks {
    async fn prepare(
        &self,
        node: u64,
        brief: &TaskBrief,
    ) -> Result<net_sdk::a2a::PrepareReply, A2aFlowError> {
        self.prepare_calls.fetch_add(1, Ordering::SeqCst);
        self.inner.prepare(node, brief).await
    }

    async fn submit(
        &self,
        prepared: &PreparedTask,
        proof: &TaskPaymentProof,
    ) -> Result<TaskAck, A2aFlowError> {
        self.submit_calls.fetch_add(1, Ordering::SeqCst);
        assert!(
            !proof.binding_sig.is_empty() && !proof.quote_id.is_empty(),
            "a paid submission carries the quote it paid and the caller's binding"
        );
        let acked = self.inner.submit(prepared, proof).await;
        if self.lose_submit_reply.swap(false, Ordering::SeqCst) {
            let _processed_by_the_provider = acked;
            return Err(A2aFlowError::Transport(
                "the submit reply was lost in transit".to_string(),
            ));
        }
        acked
    }
}

// ---------------------------------------------------------------------------
// The provider: one mesh node, one engine, N serving generations
// ---------------------------------------------------------------------------

/// One serving generation: the handles, the registry and the gate a
/// restart throws away. Dropping it unregisters the five services,
/// drops the admission store and releases the journal's ownership lock.
struct Generation {
    serving: A2aServing,
    gate: Arc<CountingGate>,
    /// The live task registry. Held because a restart must drop it —
    /// the successor generation gets a fresh one.
    _registry: TaskRegistry,
}

struct Provider {
    mesh: Arc<Mesh>,
    node_id: u64,
    engine: Arc<PaymentEngine>,
    engine_path: PathBuf,
    journal_path: PathBuf,
    billing: Arc<BillingLog>,
    /// The offer as announced (what `describe_a2a` must round-trip).
    offer: A2aOffer,
    exec: ExecProbe,
    generation: Option<Generation>,
    _payments: PaymentServeHandle,
}

impl Provider {
    fn store(&self) -> &dyn AdmissionStore {
        self.generation
            .as_ref()
            .expect("a live serving generation")
            .serving
            .store
            .as_ref()
    }

    fn gate_calls(&self) -> usize {
        self.generation
            .as_ref()
            .expect("a live serving generation")
            .gate
            .calls()
    }

    /// Serve a new generation over the journal at `journal_path`,
    /// waiting (bounded) for any previous owner to release it.
    async fn serve(&mut self) {
        assert!(
            self.generation.is_none(),
            "a node serves one A2A handler set"
        );
        let journal = open_journal(&self.journal_path).await;
        self.generation = Some(serve_generation(
            &self.mesh,
            &self.offer,
            &self.exec,
            &self.engine,
            journal,
        ));
    }

    /// Drop everything the plan's restart row names: the serve handles,
    /// the registry, the admission store and the journal's ownership
    /// handle. Returns a `Weak` to the store so the caller can prove it
    /// is gone rather than assume it.
    fn crash(&mut self) -> Weak<dyn AdmissionStore> {
        let generation = self.generation.take().expect("a live generation");
        let weak = Arc::downgrade(&generation.serving.store);
        drop(generation);
        weak
    }

    async fn record(&self, owner: TaskOwner, task_id: &str) -> Option<AdmissionRecord> {
        self.store()
            .lookup(owner, task_id)
            .await
            .expect("journal lookup")
    }

    async fn tag(&self, owner: TaskOwner, task_id: &str) -> Option<StateTag> {
        self.record(owner, task_id).await.map(|r| r.state.tag())
    }

    async fn ledger_has(&self, owner: TaskOwner, task_id: &str) -> bool {
        self.store()
            .ledger_has(owner, task_id)
            .await
            .expect("ledger read")
    }

    async fn billed(&self) -> usize {
        self.billing.read_all().await.expect("billing log").len()
    }

    /// Every quote record the engine holds, straight off
    /// `payment-engine.json`.
    async fn engine_quotes(&self) -> serde_json::Map<String, serde_json::Value> {
        let bytes = match tokio::fs::read(&self.engine_path).await {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Default::default(),
            Err(e) => panic!("read the engine state: {e}"),
        };
        let json: serde_json::Value = serde_json::from_slice(&bytes).expect("engine state json");
        json.get("quotes")
            .and_then(|q| q.as_object())
            .cloned()
            .unwrap_or_default()
    }

    /// What the engine recorded this quote was redeemed for — the
    /// purchase hash, or `None` if it was never redeemed.
    async fn redeemed_for(&self, quote_id: &str) -> Option<String> {
        let quotes = self.engine_quotes().await;
        let record = quotes
            .get(quote_id)
            .unwrap_or_else(|| panic!("the engine holds no record of quote {quote_id}"));
        assert_eq!(
            record.get("redeemed").and_then(serde_json::Value::as_bool),
            Some(record.get("redeemed_for").is_some_and(|v| !v.is_null())),
            "`redeemed` and `redeemed_for` are written in one replace"
        );
        record
            .get("redeemed_for")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    }

    /// Wait until the durable record reaches `tag`. A precondition on
    /// the observable the assertion is about (the terminal hook writes
    /// from a spawned task), not a timing window.
    async fn await_tag(&self, owner: TaskOwner, task_id: &str, tag: StateTag) -> AdmissionRecord {
        for _ in 0..400 {
            if let Some(record) = self.record(owner, task_id).await {
                if record.state.tag() == tag {
                    return record;
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!(
            "admission {task_id} never reached {tag:?} (it is {:?})",
            self.tag(owner, task_id).await
        );
    }
}

/// Wait (bounded) until the last strong reference to a crashed
/// generation's admission store is gone.
///
/// The drop is not instantaneous — unregistering a service releases the
/// handler (and with it the store clone the handlers share) through the
/// node's own bookkeeping — so this is a precondition on the observable
/// the restart depends on. It still fails if the store never goes away:
/// a leaked store means a leaked journal owner, and the successor could
/// never open the journal at all.
async fn await_dropped(store: &Weak<dyn AdmissionStore>) {
    for _ in 0..400 {
        if store.strong_count() == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!(
        "the crashed generation's admission store is still held by {} owner(s)",
        store.strong_count()
    );
}

/// Open the journal, waiting (bounded) for a predecessor owner to
/// release its lifetime-exclusive lock.
async fn open_journal(path: &Path) -> A2aAdmissionJournal {
    for _ in 0..400 {
        match A2aAdmissionJournal::open(path).await {
            Ok(journal) => return journal,
            Err(A2aJournalError::OwnedElsewhere { .. }) => {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Err(e) => panic!("open the admission journal: {e}"),
        }
    }
    panic!("the previous owner never released the journal");
}

fn serve_generation(
    mesh: &Mesh,
    offer: &A2aOffer,
    exec: &ExecProbe,
    engine: &Arc<PaymentEngine>,
    journal: A2aAdmissionJournal,
) -> Generation {
    let gate = CountingGate::new(Arc::clone(engine));
    let registry = TaskRegistry::new();
    let mut services = BTreeMap::new();
    services.insert(SERVICE.to_string(), A2aServicePolicy::Paid(offer.clone()));
    let config = A2aServiceConfig::new(services)
        .with_payment(Arc::clone(&gate) as Arc<dyn TaskAdmissionGate>)
        .with_journal(journal);
    let serving = mesh
        .serve_a2a_configured(registry.clone(), exec.executor(), config)
        .expect("serve the paid catalog");
    Generation {
        serving,
        gate,
        _registry: registry,
    }
}

// ---------------------------------------------------------------------------
// The world: provider + requester, both real
// ---------------------------------------------------------------------------

struct World {
    provider: Provider,
    caller_mesh: Arc<Mesh>,
    caller_keys: Arc<EntityKeypair>,
    caller_owner: TaskOwner,
    clock: Arc<dyn Clock>,
    registry: AssetRegistry,
    channel: Arc<WirePayments>,
    tasks: Arc<WireTasks>,
    store_path: PathBuf,
    spend_path: PathBuf,
    profile: SpendProfile,
    /// The operator's view of spend policy — a separate engine over the
    /// same file, exactly as a consent surface is.
    spend: SpendPolicyEngine,
    flow: A2aCallerFlow,
    /// Proof that a caller restart really dropped the old flow.
    payments_weak: Weak<CallerPaymentFlow>,
    /// The offer the caller DISCOVERED (never a locally rebuilt copy).
    offer: A2aOffer,
    _provider_dir: tempfile::TempDir,
    _caller_dir: tempfile::TempDir,
}

async fn mesh() -> Mesh {
    MeshBuilder::new("127.0.0.1:0", &PSK)
        .expect("builder")
        .build()
        .await
        .expect("mesh")
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

fn bounds() -> A2aBounds {
    A2aBounds {
        max_prompt_bytes: 1024,
        max_context_refs: 8,
        max_tags: 8,
        max_tag_bytes: 64,
        max_in_flight: 4,
    }
}

fn brief(task_id: &str) -> TaskBrief {
    TaskBrief::new(format!("summarize the filing for {task_id}"))
        .with_task_id(task_id)
        .with_service(SERVICE, REV)
}

fn mock_terms(provider: &EntityKeypair, registry: &AssetRegistry, capability: &str) -> String {
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
        provider.entity_id().clone(),
        capability,
        vec![template],
        registry.reference().expect("registry reference"),
    );
    String::from_utf8(canonical_bytes(&terms).expect("canonicalize")).expect("utf8")
}

async fn world() -> World {
    build_world(SpendProfile::DevTest).await
}

async fn world_requiring_approval() -> World {
    build_world(SpendProfile::Production).await
}

async fn build_world(profile: SpendProfile) -> World {
    let provider_dir = tempfile::tempdir().expect("provider tempdir");
    let caller_dir = tempfile::tempdir().expect("caller tempdir");
    let clock: Arc<dyn Clock> = Arc::new(FixedClock(AtomicU64::new(NOW_NS)));

    // ── two real nodes, real UDP loopback, real handshake ──────────
    let provider_mesh = Arc::new(mesh().await);
    let caller_mesh = Arc::new(mesh().await);
    handshake(&provider_mesh, &caller_mesh).await;
    let node_id = provider_mesh.inner().node_id();
    let caller_owner = TaskOwner::Peer(caller_mesh.inner().node_id());

    // ── provider: ONE engine behind BOTH seams ─────────────────────
    // `serve_payments` issues the quote and accepts the payment;
    // `EngineTaskAdmissionGate` redeems it. Same engine, same
    // `payment-engine.json`, so the purchase the caller made is the
    // purchase the admission gate reads.
    let provider_keys = Arc::new(EntityKeypair::generate());
    let registry = default_mock_registry(provider_keys.entity_id().clone());
    let billing = Arc::new(BillingLog::new(provider_dir.path().join("billing.jsonl")));
    let engine_path = provider_dir.path().join("payment-engine.json");
    let engine = Arc::new(
        PaymentEngine::new(
            provider_keys.clone(),
            Arc::new(MockFacilitator::new()),
            Arc::new(AdmitAll),
            registry.clone(),
            engine_path.clone(),
        )
        .expect("engine")
        .with_billing_log(billing.clone()),
    );
    let payments = serve_payments(
        &provider_mesh,
        Arc::new(InProcessProvider::new(engine.clone(), clock.clone())),
    )
    .expect("serve payments");

    let capability = format!("{node_id}/{A2A_TASK_SERVICE}/{SERVICE}");
    let offer = A2aOffer {
        service_id: SERVICE.to_string(),
        revision: REV.to_string(),
        description: Some("summarize a filing, for money".to_string()),
        pricing_terms: Some(mock_terms(&provider_keys, &registry, &capability)),
        bounds: bounds(),
        reservation_ttl_secs: 600,
        reservation_retention_secs: 7 * 24 * 60 * 60,
        retention_secs: 3_600,
    };

    let mut provider = Provider {
        mesh: provider_mesh,
        node_id,
        engine,
        engine_path,
        journal_path: provider_dir.path().join("admissions.json"),
        billing,
        offer,
        exec: ExecProbe::new(),
        generation: None,
        _payments: payments,
    };
    provider.serve().await;

    // ── discovery: the offer the caller pays against is the one the
    //    provider announced, fetched over the wire. Also the readiness
    //    precondition — describe answering means the dispatch path is
    //    up and this caller's reply subscription has propagated.
    let mut discovered = Vec::new();
    for _ in 0..50 {
        if let Ok(offers) = caller_mesh.describe_a2a(node_id).await {
            discovered = offers;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        discovered,
        vec![provider.offer.clone()],
        "describe must round-trip the announced offer, pricing and bounds included"
    );
    let offer = discovered.into_iter().next().expect("one offer");

    // ── requester: the real caller flow over the real wire ─────────
    let caller_keys = Arc::new(EntityKeypair::generate());
    let spend_path = caller_dir.path().join("payment-policy.json");
    let store_path = caller_dir.path().join("a2a-purchases.json");
    let channel = WirePayments::new(MeshPaymentChannel::new(
        caller_mesh.clone(),
        caller_keys.clone(),
        clock.clone(),
    ));
    let tasks = WireTasks::new(MeshA2aChannel::new(caller_mesh.clone()));
    let payments = Arc::new(CallerPaymentFlow::new(
        caller_keys.clone(),
        SpendPolicyEngine::new(&spend_path, profile),
        registry.clone(),
        channel.clone(),
        clock.clone(),
    ));
    let payments_weak = Arc::downgrade(&payments);
    let flow = A2aCallerFlow::new(
        payments,
        tasks.clone(),
        Arc::new(A2aPurchaseStore::new(&store_path)),
        clock.clone(),
    );

    World {
        provider,
        caller_mesh,
        caller_keys,
        caller_owner,
        clock,
        registry,
        channel,
        tasks,
        store_path,
        spend_path: spend_path.clone(),
        profile,
        spend: SpendPolicyEngine::new(&spend_path, profile),
        flow,
        payments_weak,
        offer,
        _provider_dir: provider_dir,
        _caller_dir: caller_dir,
    }
}

impl World {
    fn node(&self) -> u64 {
        self.provider.node_id
    }

    fn owner(&self) -> TaskOwner {
        self.caller_owner
    }

    /// Prepare, retrying only a lost round trip (the prepare verb is
    /// uncharged and leaves no record when the transport fails).
    async fn prepare(&self, task_id: &str) -> PreparedTask {
        let brief = brief(task_id);
        let mut last = String::new();
        for _ in 0..5 {
            match self
                .flow
                .prepare_task(self.node(), &self.offer, &brief)
                .await
            {
                Ok(prepared) => return prepared,
                Err(e) => last = format!("{e}"),
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("prepare never reached the provider: {last}");
    }

    async fn billed(&self) -> usize {
        self.provider.billed().await
    }

    async fn purchase_tag(&self, task_id: &str) -> PurchaseTag {
        self.flow
            .stored_attempt(self.node(), task_id)
            .await
            .expect("purchase store read")
            .unwrap_or_else(|| panic!("no purchase attempt for {task_id}"))
            .state
            .tag()
    }

    /// The task's status, as the requester reads it, once terminal.
    async fn await_terminal_status(&self, task_id: &str) -> TaskState {
        for _ in 0..400 {
            if let Ok(Some(record)) = self.caller_mesh.task_status(self.node(), task_id).await {
                if record.state.is_terminal() {
                    return record.state;
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("task {task_id} never reached a terminal state");
    }

    /// A brand new caller: new payment flow, new spend engine, new
    /// purchase store handle over the same files. The transport
    /// decorators are kept so their counters span the restart — they
    /// are the wire, not the caller's state.
    fn restart_caller(&mut self) {
        let payments = Arc::new(CallerPaymentFlow::new(
            self.caller_keys.clone(),
            SpendPolicyEngine::new(&self.spend_path, self.profile),
            self.registry.clone(),
            self.channel.clone(),
            self.clock.clone(),
        ));
        self.flow = A2aCallerFlow::new(
            payments,
            self.tasks.clone(),
            Arc::new(A2aPurchaseStore::new(&self.store_path)),
            self.clock.clone(),
        );
    }
}

/// The whole path, once: prepare (uncharged) → purchase (one quote, one
/// charge) → submit (one redemption, one durable claim, one run).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn prepare_then_purchase_then_submit_runs_the_task_once() {
    let w = world().await;
    let owner = w.owner();

    // ── prepare: validated, reserved and quoted before any money
    //    moves. Read-only on the money side is the claim, not
    //    "quote-free": the quote is bound to the reservation's
    //    purchase hash, and nothing is reserved against the budget
    //    and nothing is paid.
    let prepared = w.prepare("t-once").await;
    assert_eq!(w.tasks.prepare_calls(), 1, "one prepare round trip");
    assert_eq!(
        prepared.offer_hash,
        w.offer.hash(),
        "the reservation is bound to the discovered offer"
    );
    assert_eq!(
        w.provider.tag(owner, "t-once").await,
        Some(StateTag::Reserved),
        "prepare reserves capacity and nothing else"
    );
    assert_eq!(
        w.channel.quote_calls(),
        1,
        "one quote, bound to this reservation"
    );
    assert_eq!(w.channel.pay_sends(), 0, "and nothing was paid");
    assert_eq!(w.billed().await, 0, "prepare is uncharged");
    assert_eq!(w.provider.gate_calls(), 0);
    assert_eq!(
        w.provider.engine_quotes().await.len(),
        0,
        "an unpaid quote is not yet a record in the engine"
    );

    // ── purchase: one quote, one payment, one billing event ───────
    let purchase = w.flow.purchase_task(w.node(), "t-once").await;
    let A2aPurchase::Paid { proof, billing, .. } = &purchase else {
        panic!("expected a completed purchase, got {purchase:?}");
    };
    assert_eq!(
        w.channel.quote_calls(),
        1,
        "the purchase paid the quote prepare obtained — it never asked for another"
    );
    assert_eq!(w.channel.pay_sends(), 1);
    assert_eq!(w.billed().await, 1, "one purchase, one billing event");
    assert!(
        billing["billing_event"].is_string(),
        "the purchase carries the provider's signed billing event"
    );
    assert_eq!(
        w.provider.gate_calls(),
        0,
        "paying is not admission — the gate is a submit-time step"
    );
    assert_eq!(
        w.provider.tag(owner, "t-once").await,
        Some(StateTag::Reserved),
        "money moved; the admission is still only reserved"
    );
    assert_eq!(
        w.provider.redeemed_for(&proof.quote_id).await,
        None,
        "nothing has redeemed the quote yet"
    );

    // ── submit: redeem once, claim durably, launch once ───────────
    let submitted = w.flow.submit_task(w.node(), "t-once").await;
    assert_eq!(
        submitted,
        A2aSubmit::Accepted {
            task_id: "t-once".to_string()
        },
        "the paid submission is accepted"
    );
    assert_eq!(
        w.provider.gate_calls(),
        1,
        "exactly one redemption for one purchase"
    );
    assert_eq!(
        w.provider.redeemed_for(&proof.quote_id).await.as_deref(),
        Some(prepared.reservation.purchase_hash.as_str()),
        "the engine recorded WHICH purchase consumed the quote"
    );
    assert_eq!(
        w.provider.engine_quotes().await.len(),
        1,
        "one quote in the engine, start to finish"
    );

    w.provider.exec.wait_ran().await;
    assert_eq!(w.provider.exec.runs(), 1, "exactly one executor run");
    assert!(
        w.provider.ledger_has(owner, "t-once").await,
        "the launch is in the ledger"
    );

    // ── the task reaches Completed, in the registry and durably ───
    assert_eq!(
        w.await_terminal_status("t-once").await,
        TaskState::Completed {
            result_ref: RESULT.to_string()
        },
        "the requester reads the executor's own result"
    );
    let terminal = w
        .provider
        .await_tag(owner, "t-once", StateTag::Terminal)
        .await;
    assert_eq!(
        terminal.state,
        AdmissionState::Terminal {
            quote_id: Some(proof.quote_id.clone()),
            payer: Some(*w.caller_keys.entity_id().as_bytes()),
            state: TaskState::Completed {
                result_ref: RESULT.to_string()
            },
        },
        "the durable record names the payer and the outcome"
    );
    assert!(
        terminal.attempts.is_empty(),
        "a clean purchase leaves no refusal notes"
    );
    assert_eq!(w.billed().await, 1, "still exactly one charge");
    assert_eq!(w.purchase_tag("t-once").await, PurchaseTag::Submitted);
}

/// A held purchase resumes when the operator approves **that** quote —
/// and an approval for a different purchase unlocks nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pending_approval_resumes_after_approve_payment_for_the_same_purchase_only() {
    let w = world_requiring_approval().await;
    let owner = w.owner();
    w.prepare("t-held").await;
    w.prepare("t-other").await;

    let held = w.flow.purchase_task(w.node(), "t-held").await;
    let A2aPurchase::RequiresPaymentApproval {
        quote_id: held_quote,
        ..
    } = &held
    else {
        panic!("the production profile must hold this spend, got {held:?}");
    };
    let other = w.flow.purchase_task(w.node(), "t-other").await;
    let A2aPurchase::RequiresPaymentApproval {
        quote_id: other_quote,
        ..
    } = &other
    else {
        panic!("the second purchase must be held too, got {other:?}");
    };
    assert_ne!(
        held_quote, other_quote,
        "two purchases, two quotes, two approvals"
    );
    assert_eq!(w.billed().await, 0, "a held purchase charges nothing");
    assert_eq!(w.channel.pay_sends(), 0, "and sends no payload");
    assert_eq!(
        w.purchase_tag("t-held").await,
        PurchaseTag::AwaitingApproval
    );

    // The operator approves the OTHER purchase.
    assert!(
        w.spend.approve(other_quote).await.expect("approve"),
        "the operator surface approved a pending hold"
    );
    let still_held = w.flow.purchase_task(w.node(), "t-held").await;
    let A2aPurchase::RequiresPaymentApproval { quote_id, .. } = &still_held else {
        panic!("an approval for another purchase must not release this one: {still_held:?}");
    };
    assert_eq!(quote_id, held_quote, "same quote, still held");
    assert_eq!(
        w.purchase_tag("t-held").await,
        PurchaseTag::AwaitingApproval
    );
    assert_eq!(
        w.billed().await,
        0,
        "no charge — approving one purchase never pays for another"
    );
    assert_eq!(w.channel.pay_sends(), 0);

    // The positive control: the approval mechanism does work, for the
    // purchase it was granted for.
    let paid_other = w.flow.purchase_task(w.node(), "t-other").await;
    assert!(
        matches!(paid_other, A2aPurchase::Paid { .. }),
        "the approved purchase goes through: {paid_other:?}"
    );
    assert_eq!(w.billed().await, 1, "exactly the approved purchase paid");

    // And then its own approval releases the held one.
    assert!(w.spend.approve(held_quote).await.expect("approve"));
    let resumed = w.flow.purchase_task(w.node(), "t-held").await;
    let A2aPurchase::Paid { proof, .. } = &resumed else {
        panic!("the approved purchase must resume, got {resumed:?}");
    };
    assert_eq!(
        &proof.quote_id, held_quote,
        "resumed on the stored quote — never re-quoted"
    );
    assert_eq!(w.channel.quote_calls(), 2, "one quote per purchase, ever");
    assert_eq!(w.billed().await, 2, "two purchases, two charges");

    // Both run, once each.
    for task_id in ["t-other", "t-held"] {
        assert_eq!(
            w.flow.submit_task(w.node(), task_id).await,
            A2aSubmit::Accepted {
                task_id: task_id.to_string()
            }
        );
        w.provider.exec.wait_ran().await;
        assert!(w.provider.ledger_has(owner, task_id).await);
        assert_eq!(
            w.await_terminal_status(task_id).await,
            TaskState::Completed {
                result_ref: RESULT.to_string()
            }
        );
    }
    assert_eq!(w.provider.exec.runs(), 2, "one run per purchase");
    assert_eq!(w.provider.gate_calls(), 2, "one redemption per purchase");
    assert_eq!(w.billed().await, 2);
}

/// A lost pay reply is not a lost payment: the caller resends the
/// identical stored payload and the provider's payload-idempotent
/// acceptance answers the original verdict. One quote, one charge.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lost_pay_reply_is_reconciled_without_a_second_quote_or_charge() {
    let w = world().await;
    let owner = w.owner();
    let prepared = w.prepare("t-lost-pay").await;

    w.channel.lose_next_pay_reply();
    let ambiguous = w.flow.purchase_task(w.node(), "t-lost-pay").await;
    assert!(
        matches!(ambiguous, A2aPurchase::Unknown { .. }),
        "a lost reply is ambiguous, not a failure: {ambiguous:?}"
    );
    assert_eq!(w.purchase_tag("t-lost-pay").await, PurchaseTag::Unknown);
    assert_eq!(
        w.billed().await,
        1,
        "the payment did land — the provider billed it"
    );

    let resumed = w.flow.purchase_task(w.node(), "t-lost-pay").await;
    let A2aPurchase::Paid { proof, .. } = &resumed else {
        panic!("the stored payment must resolve, got {resumed:?}");
    };
    assert_eq!(w.channel.pay_sends(), 2, "the payload really was re-sent");
    assert_eq!(
        w.channel.distinct_payloads(),
        1,
        "and both sends were the same bytes"
    );
    assert_eq!(
        w.channel.quote_calls(),
        1,
        "the recovery never minted a second quote"
    );
    assert_eq!(
        w.provider.engine_quotes().await.len(),
        1,
        "and the provider's engine holds exactly one"
    );
    assert_eq!(
        w.billed().await,
        1,
        "exactly one charge across the recovery"
    );

    // The recovered purchase is the one that admits the work.
    assert_eq!(
        w.flow.submit_task(w.node(), "t-lost-pay").await,
        A2aSubmit::Accepted {
            task_id: "t-lost-pay".to_string()
        }
    );
    w.provider.exec.wait_ran().await;
    assert_eq!(w.provider.exec.runs(), 1);
    assert_eq!(w.provider.gate_calls(), 1);
    assert_eq!(
        w.provider.redeemed_for(&proof.quote_id).await.as_deref(),
        Some(prepared.reservation.purchase_hash.as_str()),
        "the recovered quote is the one the gate consumed, for this purchase"
    );
    assert!(w.provider.ledger_has(owner, "t-lost-pay").await);
    assert_eq!(
        w.await_terminal_status("t-lost-pay").await,
        TaskState::Completed {
            result_ref: RESULT.to_string()
        }
    );
    assert_eq!(w.billed().await, 1);
}

/// A lost submit reply is reconciled by resubmitting the **same** proof:
/// the provider already launched it, so the retry converges on the
/// original admission — one redemption, one run.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lost_submit_reply_is_reconciled_by_resubmitting_the_same_proof() {
    let w = world().await;
    let owner = w.owner();
    w.prepare("t-lost-ack").await;
    let purchase = w.flow.purchase_task(w.node(), "t-lost-ack").await;
    let A2aPurchase::Paid { proof, .. } = &purchase else {
        panic!("expected a paid purchase, got {purchase:?}");
    };

    w.tasks.lose_next_submit_reply();
    let lost = w.flow.submit_task(w.node(), "t-lost-ack").await;
    assert!(
        matches!(lost, A2aSubmit::Retry { .. }),
        "a lost ack is retryable with the same proof: {lost:?}"
    );
    // The provider did admit and launch it — that is the precondition
    // the retry has to converge on, not a guess.
    w.provider.exec.wait_ran().await;
    assert_eq!(w.provider.exec.runs(), 1);
    assert!(w.provider.ledger_has(owner, "t-lost-ack").await);
    assert_eq!(
        w.purchase_tag("t-lost-ack").await,
        PurchaseTag::Paid,
        "the caller keeps its evidence rather than re-buying"
    );

    let resubmitted = w.flow.submit_task(w.node(), "t-lost-ack").await;
    assert_eq!(
        resubmitted,
        A2aSubmit::Accepted {
            task_id: "t-lost-ack".to_string()
        },
        "the same proof resubmitted converges on the original admission"
    );
    assert_eq!(w.tasks.submit_calls(), 2, "two submissions really crossed");
    assert_eq!(
        w.provider.gate_calls(),
        1,
        "the second submission redeemed nothing"
    );
    assert_eq!(w.provider.exec.runs(), 1, "and launched nothing");
    assert_eq!(
        w.provider.redeemed_for(&proof.quote_id).await.as_deref(),
        Some(
            w.flow
                .stored_attempt(w.node(), "t-lost-ack")
                .await
                .expect("store")
                .expect("attempt")
                .prepared
                .expect("prepared")
                .reservation
                .purchase_hash
                .as_str()
        ),
        "one quote, consumed by exactly this purchase"
    );
    assert_eq!(w.billed().await, 1, "one charge, one run");
    assert_eq!(
        w.await_terminal_status("t-lost-ack").await,
        TaskState::Completed {
            result_ref: RESULT.to_string()
        }
    );
}

/// A provider that redeems a payment, records it, and dies before it can
/// claim the launch resumes through S6′: the successor owner inherits a
/// `Paid` admission, redeems **nothing** a second time, and runs the
/// work exactly once.
///
/// **On how the crash window is authored.** The window is between the
/// handler's two journal writes (`Reserved → Paid`, then the launch
/// claim + ledger in one atomic replace) — back to back, with no seam an
/// out-of-process test can interrupt, and the SDK's own
/// `fail_next_write` seam is behind a feature this crate's test build
/// does not enable. So the pre-crash state is authored through the
/// **production writers**: the real [`EngineTaskAdmissionGate`] redeems
/// against the real engine (the money side is genuinely consumed —
/// `redeemed_for` is written, and a second redemption of a different
/// purchase would be refused), and the real [`AdmissionStore`] performs
/// the exact S6 transition with the evidence the gate returned. What
/// follows — the restart — is real in every respect: the serve handles,
/// the registry, the admission store and the journal's lifetime-
/// exclusive owner are dropped and proven gone, the journal is re-opened
/// from disk, and a fresh generation serves over the same
/// `payment-engine.json` and the same journal path.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_provider_restart_between_redeem_and_claim_reconciles_the_original_payment() {
    let mut w = world().await;
    let owner = w.owner();
    let prepared = w.prepare("t-crash").await;
    let purchase = w.flow.purchase_task(w.node(), "t-crash").await;
    let A2aPurchase::Paid { proof, .. } = &purchase else {
        panic!("expected a paid purchase, got {purchase:?}");
    };
    assert_eq!(w.billed().await, 1);

    // ── the crash window, through the production writers ──────────
    let tool_id = format!("{A2A_TASK_SERVICE}/{SERVICE}");
    let evidence = w
        .provider
        .generation
        .as_ref()
        .expect("generation")
        .gate
        .redeem(TaskPaymentClaim {
            tool_id: &tool_id,
            quote_id: &proof.quote_id,
            binding: &proof.binding_sig,
            expected_input_hash: &prepared.reservation.purchase_hash,
        })
        .await
        .expect("the real engine admits the original payment");
    assert_eq!(evidence.quote_id, proof.quote_id);
    assert_eq!(
        evidence.payer,
        *w.caller_keys.entity_id().as_bytes(),
        "the engine attributed the payment to the paying caller"
    );
    w.provider
        .store()
        .transition(
            owner,
            "t-crash",
            &[StateTag::Reserved],
            AdmissionState::Paid {
                quote_id: evidence.quote_id.clone(),
                payer: evidence.payer,
            },
            now_secs(),
        )
        .await
        .expect("S6's write");
    assert_eq!(
        w.provider.gate_calls(),
        1,
        "the dead generation redeemed once"
    );
    assert_eq!(
        w.provider.redeemed_for(&proof.quote_id).await.as_deref(),
        Some(prepared.reservation.purchase_hash.as_str()),
        "the money side is consumed, for exactly this purchase"
    );
    assert_eq!(w.provider.tag(owner, "t-crash").await, Some(StateTag::Paid));
    assert!(
        !w.provider.ledger_has(owner, "t-crash").await,
        "the launch was never claimed"
    );
    assert_eq!(w.provider.exec.runs(), 0, "and nothing ran");

    // ── the restart: drop the handles, the registry, the store and
    //    the journal owner, and prove they are gone ────────────────
    let dead_store = w.provider.crash();
    await_dropped(&dead_store).await;

    // Re-open the SAME journal path: the successor inherits the
    // unresolved admission and neither relaunches nor rewrites it.
    let successor = open_journal(&w.provider.journal_path).await;
    let recovered: Vec<&RecoveredAdmission> = successor
        .recovered()
        .iter()
        .filter(|r| r.task_id == "t-crash")
        .collect();
    assert_eq!(
        recovered.len(),
        1,
        "the successor owner inherited exactly one unresolved admission"
    );
    assert_eq!(
        recovered[0].found,
        AdmissionState::Paid {
            quote_id: proof.quote_id.clone(),
            payer: evidence.payer,
        }
    );
    assert_eq!(
        recovered[0].status,
        TaskState::Interrupted {
            detail: DETAIL_PAID_NOT_STARTED.to_string()
        },
        "a crash between redeem and claim answers paid_not_started"
    );
    drop(successor);

    // A fresh generation: new gate counter, new registry, same engine
    // state file, same journal path.
    w.provider.serve().await;
    assert_eq!(
        w.provider.gate_calls(),
        0,
        "the successor's gate starts at zero"
    );

    // ── the retry resumes through S6′ ─────────────────────────────
    let resumed = w.flow.submit_task(w.node(), "t-crash").await;
    assert_eq!(
        resumed,
        A2aSubmit::Accepted {
            task_id: "t-crash".to_string()
        },
        "the same proof, resubmitted to the successor, is admitted"
    );
    assert_eq!(
        w.provider.gate_calls(),
        0,
        "S6′ trusts the recorded evidence — NO second redemption"
    );
    w.provider.exec.wait_ran().await;
    assert_eq!(w.provider.exec.runs(), 1, "the work ran exactly once");
    assert!(
        w.provider.ledger_has(owner, "t-crash").await,
        "and the successor claimed the launch durably"
    );
    assert_eq!(w.channel.quote_calls(), 1, "no second quote");
    assert_eq!(w.channel.pay_sends(), 1, "no second payment");
    assert_eq!(w.billed().await, 1, "no second charge");
    assert_eq!(w.provider.engine_quotes().await.len(), 1);
    assert_eq!(
        w.await_terminal_status("t-crash").await,
        TaskState::Completed {
            result_ref: RESULT.to_string()
        }
    );
    let terminal = w
        .provider
        .await_tag(owner, "t-crash", StateTag::Terminal)
        .await;
    assert_eq!(
        terminal.state,
        AdmissionState::Terminal {
            quote_id: Some(proof.quote_id.clone()),
            payer: Some(evidence.payer),
            state: TaskState::Completed {
                result_ref: RESULT.to_string()
            },
        },
        "the successor recorded the outcome against the original payment"
    );
}

/// A caller that dies between paying and submitting resumes from the
/// purchase store: a brand new flow finds the stored proof and finishes
/// without re-quoting or paying again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_caller_restart_between_pay_and_submit_resumes_from_the_purchase_store() {
    let mut w = world().await;
    let owner = w.owner();
    let prepared = w.prepare("t-caller-restart").await;
    let purchase = w.flow.purchase_task(w.node(), "t-caller-restart").await;
    let A2aPurchase::Paid { proof, .. } = &purchase else {
        panic!("expected a paid purchase, got {purchase:?}");
    };
    let proof = proof.clone();
    assert_eq!(w.billed().await, 1);
    assert_eq!(w.provider.gate_calls(), 0, "nothing submitted yet");
    assert_eq!(w.provider.exec.runs(), 0);

    // Nothing but the files survive.
    w.restart_caller();
    assert_eq!(
        w.payments_weak.strong_count(),
        0,
        "the pre-restart caller flow is genuinely gone, not merely unused"
    );

    let stored = w
        .flow
        .stored_attempt(w.node(), "t-caller-restart")
        .await
        .expect("store read")
        .expect("the restarted caller reads its own durable attempt");
    assert_eq!(stored.state.tag(), PurchaseTag::Paid);

    let submitted = w.flow.submit_task(w.node(), "t-caller-restart").await;
    assert_eq!(
        submitted,
        A2aSubmit::Accepted {
            task_id: "t-caller-restart".to_string()
        },
        "the restarted caller submits the stored proof"
    );
    assert_eq!(
        w.channel.quote_calls(),
        1,
        "no re-quote across the caller restart"
    );
    assert_eq!(w.channel.pay_sends(), 1, "and no second payment");
    assert_eq!(w.billed().await, 1, "one charge for one purchase");
    assert_eq!(
        w.provider.gate_calls(),
        1,
        "the stored proof is what redeemed — exactly once"
    );
    assert_eq!(
        w.provider.redeemed_for(&proof.quote_id).await.as_deref(),
        Some(prepared.reservation.purchase_hash.as_str()),
        "against the reservation the pre-restart caller prepared"
    );
    w.provider.exec.wait_ran().await;
    assert_eq!(w.provider.exec.runs(), 1, "the work ran exactly once");
    assert!(w.provider.ledger_has(owner, "t-caller-restart").await);
    assert_eq!(
        w.await_terminal_status("t-caller-restart").await,
        TaskState::Completed {
            result_ref: RESULT.to_string()
        }
    );
    assert_eq!(
        w.purchase_tag("t-caller-restart").await,
        PurchaseTag::Submitted,
        "and the restarted caller recorded the submission on the same attempt"
    );
}
