//! Caller-side attempt **identity**: what a decision taken *before* an
//! await is allowed to publish *after* it, and where a financial outcome
//! goes when the record it was decided against is no longer there.
//!
//! The defect these witnesses are written against is one shape repeated:
//! a compare-and-swap that checks a state *tag* where the invariant needs
//! an immutable *identity*. A signer that finally returns, a refusal that
//! finally lands, a duplicate that publishes an ambiguity — each of them
//! wrote onto whatever record happened to occupy the intent key.
//!
//! Every test here drives the **real** `PaymentEngine` over the real
//! mock facilitator behind a scripted `ProviderChannel`, so the
//! idempotency and the in-flight claim these witnesses lean on are the
//! production ones. Three powers the harness adds, none of which a wire
//! can be asked for on demand: the facilitator can be **parked** (so a
//! payment is genuinely mid-settlement while the test does something
//! else), the provider's submit can be parked until *n* callers are
//! inside it (so "concurrent" is staged, not slept on), and the quote
//! channel can answer with a deliberately misbound quote.
//!
//! No test in this file sleeps, and none of them retries.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use net::adapter::net::identity::EntityKeypair;
use net_payments::billing::BillingLog;
use net_payments::core::canonical::canonical_bytes;
use net_payments::core::registry::{default_mock_registry, AssetRegistry};
use net_payments::core::terms::PricingTerms;
use net_payments::engine::{AdmitAll, PaymentEngine};
use net_payments::facilitator::mock::{MockFacilitator, MOCK_NETWORK, MOCK_SCHEME};
use net_payments::facilitator::{Facilitator, FacilitatorError, SettleOutcome, VerifyOutcome};
use net_payments::flow::a2a::{
    A2aCallerFlow, A2aPrepareError, A2aProviderChannel, A2aPurchase, A2aPurchaseFile,
    A2aPurchaseStore, A2aSubmit, AttemptGeneration, AttemptResolution, PurchaseAttempt,
    PurchaseError, PurchaseState, StateTag, PREPARE_LEASE_NS, SUPERSEDED_REFUSAL_REASON,
};
use net_payments::flow::mesh::EngineTaskAdmissionGate;
use net_payments::flow::{Clock, InProcessProvider, ProviderChannel};
use net_payments::policy::spend::{SpendPolicyEngine, SpendProfile};
use net_payments::policy::store::mutate_json;
use net_payments::x402::payload::PaymentPayload;
use net_payments::x402::requirements::PaymentRequirements;
use net_payments::x402::X402Carry;
use net_sdk::a2a::{
    purchase_hash, task_commitment, A2aBounds, A2aOffer, AdmissionReservation, PrepareReply,
    PreparedTask, SubmitRejection, TaskAck, TaskBrief,
};
use net_sdk::a2a_payment::{TaskAdmissionGate, TaskPaymentClaim, TaskPaymentProof};
use net_sdk::mesh_a2a::A2aFlowError;
use net_sdk::tool_payment::FailureSchematic;

const NOW: u64 = 1_740_672_000_000_000_000;
const NODE: u64 = 7;
const SERVICE: &str = "summarize";
const REVISION: &str = "r1";
const CAPABILITY: &str = "7/net.a2a.task/summarize";
const AMOUNT: u128 = 2_500;
/// Long enough that a clock advance past the *caller's* lease window
/// cannot expire the quote: this file stages claim staleness, never
/// pricing staleness.
const QUOTE_TTL_NS: u64 = 3_600_000_000_000;

/// The engine's in-flight reclaim window, **set by this file** (see
/// `world_with_quote_ttl`) rather than inherited from the engine's
/// default. The live-claim row below is the one schedule whose meaning
/// is decided by this number, and a default the engine is free to
/// retune would decide it silently: the engine answers a duplicate
/// `InProgress` both while the claim is fresh *and* after it has gone
/// stale under an expired quote, so nothing in the test's outcome would
/// report the drift.
const IN_FLIGHT_TTL_NS: u64 = 300_000_000_000;
/// A quote priced to lapse well inside [`IN_FLIGHT_TTL_NS`] — the only
/// way a quote can expire while its claim is still live.
const LAPSING_QUOTE_TTL_NS: u64 = IN_FLIGHT_TTL_NS / 5;
/// How far the live-claim row advances its clock: past the quote's
/// death, and far short of the reclaim window.
const PAST_LAPSED_QUOTE_NS: u64 = LAPSING_QUOTE_TTL_NS + 1_000_000_000;
/// The staging is a compile-time fact, not a comment.
const _: () = assert!(LAPSING_QUOTE_TTL_NS < PAST_LAPSED_QUOTE_NS);
const _: () = assert!(PAST_LAPSED_QUOTE_NS < IN_FLIGHT_TTL_NS);

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A clock the test moves on purpose; never auto-advancing.
struct TestClock(AtomicU64);

impl TestClock {
    fn new() -> Self {
        Self(AtomicU64::new(NOW))
    }
    fn advance(&self, ns: u64) {
        self.0.fetch_add(ns, Ordering::SeqCst);
    }
}

impl Clock for TestClock {
    fn now_ns(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// A staging gate: a call parks here until the test opens it. Registers
/// for the wake *before* re-reading the flag, so a gate opened between
/// the check and the wait cannot leave a caller parked forever.
#[derive(Default)]
struct Gate {
    open: AtomicBool,
    entered: AtomicUsize,
    notify: tokio::sync::Notify,
}

impl Gate {
    fn wide_open() -> Self {
        Self {
            open: AtomicBool::new(true),
            ..Default::default()
        }
    }

    async fn pass(&self) {
        self.entered.fetch_add(1, Ordering::SeqCst);
        self.notify.notify_waiters();
        loop {
            let waited = self.notify.notified();
            tokio::pin!(waited);
            waited.as_mut().enable();
            if self.open.load(Ordering::SeqCst) {
                return;
            }
            waited.await;
        }
    }

    fn release(&self) {
        self.open.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    fn entered(&self) -> usize {
        self.entered.load(Ordering::SeqCst)
    }

    /// Park until at least `n` calls are inside the gate — the only
    /// cross-task ordering primitive in this file.
    async fn await_entered(&self, n: usize) {
        loop {
            let waited = self.notify.notified();
            tokio::pin!(waited);
            waited.as_mut().enable();
            if self.entered() >= n {
                return;
            }
            waited.await;
        }
    }
}

/// Parks a settlement that the engine has already claimed `in_flight`,
/// so a concurrent duplicate meets the production in-flight answer rather
/// than a fabricated one.
struct GatedFacilitator {
    inner: MockFacilitator,
    gate: Arc<Gate>,
}

#[async_trait::async_trait]
impl Facilitator for GatedFacilitator {
    fn reference(&self) -> net_payments::core::verification::VerifierRef {
        self.inner.reference()
    }

    async fn verify(
        &self,
        payload: &X402Carry<PaymentPayload>,
        requirements: &X402Carry<PaymentRequirements>,
    ) -> Result<VerifyOutcome, FacilitatorError> {
        self.inner.verify(payload, requirements).await
    }

    async fn settle(
        &self,
        payload: &X402Carry<PaymentPayload>,
        requirements: &X402Carry<PaymentRequirements>,
    ) -> Result<SettleOutcome, FacilitatorError> {
        self.gate.pass().await;
        self.inner.settle(payload, requirements).await
    }

    async fn supported_pairs(&self) -> Result<Option<Vec<(String, String)>>, FacilitatorError> {
        self.inner.supported_pairs().await
    }
}

/// The provider's prepare/submit half, scripted.
struct ScriptedTasks {
    offer: A2aOffer,
    admissions: parking_lot::Mutex<std::collections::HashMap<String, String>>,
    submit_gate: Arc<Gate>,
    submit_calls: AtomicUsize,
    submit_replies: parking_lot::Mutex<std::collections::VecDeque<Result<TaskAck, A2aFlowError>>>,
}

impl ScriptedTasks {
    fn new(offer: A2aOffer, submit_gate: Arc<Gate>) -> Self {
        Self {
            offer,
            admissions: parking_lot::Mutex::new(std::collections::HashMap::new()),
            submit_gate,
            submit_calls: AtomicUsize::new(0),
            submit_replies: parking_lot::Mutex::new(std::collections::VecDeque::new()),
        }
    }

    fn admission_id(&self, task_id: &str) -> String {
        let mut minted = self.admissions.lock();
        let next = minted.len();
        minted
            .entry(task_id.to_string())
            .or_insert_with(|| format!("adm-{task_id}-{next}"))
            .clone()
    }

    fn push_submit(&self, reply: Result<TaskAck, A2aFlowError>) {
        self.submit_replies.lock().push_back(reply);
    }
}

#[async_trait::async_trait]
impl A2aProviderChannel for ScriptedTasks {
    async fn prepare(&self, node: u64, brief: &TaskBrief) -> Result<PrepareReply, A2aFlowError> {
        // The provider computes the commitment from its OWN offer copy
        // and mints the admission id, exactly as the journal does; the
        // purchase hash is what those two imply.
        let commitment = task_commitment(&self.offer, brief);
        let admission_id = self.admission_id(&brief.task_id);
        Ok(PrepareReply::Reservation(AdmissionReservation {
            task_id: brief.task_id.clone(),
            admission_id: admission_id.clone(),
            purchase_hash: purchase_hash(&admission_id, &commitment),
            commitment,
            capability: format!("{node}/net.a2a.task/{}", self.offer.service_id),
            pricing_terms: self.offer.pricing_terms.clone(),
            expires_at: 2_000_000_000,
        }))
    }

    async fn submit(
        &self,
        prepared: &PreparedTask,
        proof: &TaskPaymentProof,
    ) -> Result<TaskAck, A2aFlowError> {
        self.submit_calls.fetch_add(1, Ordering::SeqCst);
        assert!(
            !proof.quote_id.is_empty(),
            "a paid submission must name the quote that paid"
        );
        self.submit_gate.pass().await;
        match self.submit_replies.lock().pop_front() {
            Some(reply) => reply,
            None => Ok(TaskAck {
                task_id: prepared.brief.task_id.clone(),
                accepted: true,
                reason: None,
            }),
        }
    }
}

/// What the scripted quote channel binds the quote it returns to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HashMode {
    /// The purchase hash the caller asked for — the honest provider.
    Exact,
    /// Another purchase's hash: a genuine quote, for other work.
    Foreign,
}

/// The payment channel, scripted: counts what crossed, and can misbind
/// the quote it hands back.
struct ScriptedChannel {
    inner: InProcessProvider,
    quote_calls: AtomicUsize,
    payloads: parking_lot::Mutex<Vec<Vec<u8>>>,
    hash_mode: parking_lot::Mutex<HashMode>,
}

impl ScriptedChannel {
    fn new(inner: InProcessProvider) -> Self {
        Self {
            inner,
            quote_calls: AtomicUsize::new(0),
            payloads: parking_lot::Mutex::new(Vec::new()),
            hash_mode: parking_lot::Mutex::new(HashMode::Exact),
        }
    }

    fn quote_calls(&self) -> usize {
        self.quote_calls.load(Ordering::SeqCst)
    }

    /// Every payload that crossed, **with multiplicity** — a set could
    /// not tell one send from two.
    fn pay_sends(&self) -> usize {
        self.payloads.lock().len()
    }

    fn distinct_payloads(&self) -> usize {
        let mut seen: Vec<Vec<u8>> = self.payloads.lock().clone();
        seen.sort();
        seen.dedup();
        seen.len()
    }

    fn set_hash_mode(&self, mode: HashMode) {
        *self.hash_mode.lock() = mode;
    }
}

#[async_trait::async_trait]
impl ProviderChannel for ScriptedChannel {
    async fn quote(
        &self,
        caller: &net::adapter::net::identity::EntityId,
        provider: &net::adapter::net::identity::EntityId,
        capability: &str,
        template: &X402Carry<PaymentRequirements>,
        input_hash: Option<&str>,
    ) -> Result<Vec<u8>, net_payments::flow::ChannelError> {
        self.quote_calls.fetch_add(1, Ordering::SeqCst);
        // The misbinding is applied where a dishonest (or confused)
        // provider would apply it: the quote it signs, not the request.
        let foreign = purchase_hash("adm-somebody-else", "commitment-for-other-work");
        let bound = match *self.hash_mode.lock() {
            HashMode::Exact => input_hash.map(str::to_string),
            HashMode::Foreign => Some(foreign),
        };
        self.inner
            .quote(caller, provider, capability, template, bound.as_deref())
            .await
    }

    async fn pay(
        &self,
        quote_bytes: &[u8],
        payload: &X402Carry<PaymentPayload>,
    ) -> Result<net_payments::flow::PayResponse, net_payments::flow::ChannelError> {
        self.payloads.lock().push(payload.bytes().to_vec());
        self.inner.pay(quote_bytes, payload).await
    }
}

struct World {
    flow: Arc<A2aCallerFlow>,
    tasks: Arc<ScriptedTasks>,
    channel: Arc<ScriptedChannel>,
    store: Arc<A2aPurchaseStore>,
    billing: Arc<BillingLog>,
    clock: Arc<TestClock>,
    engine: Arc<PaymentEngine>,
    offer: A2aOffer,
    settle_gate: Arc<Gate>,
    submit_gate: Arc<Gate>,
    _dir: tempfile::TempDir,
}

impl World {
    async fn state(&self, task_id: &str) -> StateTag {
        self.attempt(task_id).await.state.tag()
    }

    async fn attempt(&self, task_id: &str) -> net_payments::flow::a2a::PurchaseAttempt {
        self.flow
            .stored_attempt(NODE, task_id)
            .await
            .expect("store read")
            .unwrap_or_else(|| panic!("no attempt for {task_id}"))
    }

    async fn billed(&self) -> usize {
        self.billing.read_all().await.expect("read billing").len()
    }
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
    TaskBrief::new(format!("summarize {task_id}"))
        .with_task_id(task_id)
        .with_service(SERVICE, REVISION)
}

fn mock_terms(provider: &EntityKeypair, registry: &AssetRegistry) -> String {
    let template = X402Carry::author(&PaymentRequirements {
        scheme: MOCK_SCHEME.into(),
        network: MOCK_NETWORK.into(),
        amount: AMOUNT.to_string(),
        asset: "musd".into(),
        pay_to: "mock-provider-settle-addr".into(),
        max_timeout_seconds: 60,
        extra: None,
    })
    .expect("template");
    let terms = PricingTerms::new(
        provider.entity_id().clone(),
        CAPABILITY,
        vec![template],
        registry.reference().expect("registry ref"),
    );
    String::from_utf8(canonical_bytes(&terms).expect("canonicalize")).expect("utf8")
}

fn schematic(reason: &str, safe_to_retry: bool, safe_to_requote: bool) -> FailureSchematic {
    // Built from a real schematic rather than field by field, so a
    // change to the object's shape cannot leave this fixture behind.
    let mut s = FailureSchematic::missing_quote("net.a2a.task/summarize");
    s.reason = reason.to_string();
    s.message = reason.to_string();
    s.retryable = safe_to_retry;
    s.recovery.safe_to_retry = safe_to_retry;
    s.recovery.safe_to_requote = safe_to_requote;
    s
}

/// `park_settlement`: the facilitator holds every settlement until the
/// test releases it. `park_submit`: the provider holds every submit.
async fn world(park_settlement: bool, park_submit: bool) -> World {
    world_with_quote_ttl(park_settlement, park_submit, QUOTE_TTL_NS).await
}

/// [`world`], with the provider's quote lifetime set by the caller — for
/// the one schedule that needs a quote to lapse *inside* the engine's
/// in-flight window rather than long past it.
///
/// That window is [`IN_FLIGHT_TTL_NS`], set here rather than inherited:
/// the engine reads the test clock (`InProcessProvider::pay` hands it
/// `clock.now_ns()`), so the relation between a quote's life and the
/// reclaim window is entirely this file's to state.
async fn world_with_quote_ttl(
    park_settlement: bool,
    park_submit: bool,
    quote_ttl_ns: u64,
) -> World {
    let dir = tempfile::tempdir().expect("tempdir");
    let clock = Arc::new(TestClock::new());
    let provider_keys = Arc::new(EntityKeypair::generate());
    let registry = default_mock_registry(provider_keys.entity_id().clone());
    let billing = Arc::new(BillingLog::new(dir.path().join("billing.jsonl")));
    let settle_gate = Arc::new(if park_settlement {
        Gate::default()
    } else {
        Gate::wide_open()
    });
    let submit_gate = Arc::new(if park_submit {
        Gate::default()
    } else {
        Gate::wide_open()
    });
    let engine = Arc::new(
        PaymentEngine::new(
            provider_keys.clone(),
            Arc::new(GatedFacilitator {
                inner: MockFacilitator::new(),
                gate: settle_gate.clone(),
            }),
            Arc::new(AdmitAll),
            registry.clone(),
            dir.path().join("engine.json"),
        )
        .expect("engine")
        .with_billing_log(billing.clone())
        .with_in_flight_ttl_ns(IN_FLIGHT_TTL_NS),
    );
    let channel = Arc::new(ScriptedChannel::new(
        InProcessProvider::new(engine.clone(), clock.clone()).with_quote_ttl_ns(quote_ttl_ns),
    ));
    let offer = A2aOffer {
        service_id: SERVICE.to_string(),
        revision: REVISION.to_string(),
        description: None,
        pricing_terms: Some(mock_terms(&provider_keys, &registry)),
        bounds: bounds(),
        reservation_ttl_secs: 600,
        reservation_retention_secs: 604_800,
        retention_secs: 3_600,
    };
    let caller = Arc::new(EntityKeypair::generate());
    let payments = Arc::new(net_payments::flow::CallerPaymentFlow::new(
        caller,
        SpendPolicyEngine::new(
            dir.path().join("payment-policy.json"),
            SpendProfile::DevTest,
        ),
        registry,
        channel.clone(),
        clock.clone(),
    ));
    let tasks = Arc::new(ScriptedTasks::new(offer.clone(), submit_gate.clone()));
    let store = Arc::new(A2aPurchaseStore::new(dir.path().join("a2a-purchases.json")));
    let flow = Arc::new(A2aCallerFlow::new(
        payments,
        tasks.clone(),
        store.clone(),
        clock.clone(),
    ));
    World {
        flow,
        tasks,
        channel,
        store,
        billing,
        clock,
        engine,
        offer,
        settle_gate,
        submit_gate,
        _dir: dir,
    }
}

// ---------------------------------------------------------------------------
// The quote must price the work that was asked about (finding R4)
// ---------------------------------------------------------------------------

/// A provider-signed quote bound to **another** purchase is refused
/// before a budget is reserved, before a payload is authored, and before
/// anything is sent. The provider's later redemption refusal is not the
/// backstop: by then the caller has been charged.
#[tokio::test]
async fn a_quote_bound_to_another_purchase_is_refused_before_payment() {
    let w = world(false, false).await;
    w.channel.set_hash_mode(HashMode::Foreign);

    let refused = w
        .flow
        .prepare_task(NODE, &w.offer, &brief("t-misbound"))
        .await
        .expect_err("a quote for other work must not be accepted");
    assert!(
        matches!(&refused, A2aPrepareError::Quote { retryable: false, message }
            if message.contains("bound to input")),
        "expected a permanent quote-binding refusal, got {refused:?}"
    );
    assert_eq!(w.channel.quote_calls(), 1, "the quote was requested once");
    assert_eq!(w.channel.pay_sends(), 0, "nothing was ever sent");
    assert_eq!(w.billed().await, 0, "and nothing was charged");
    assert!(
        w.flow
            .stored_attempt(NODE, "t-misbound")
            .await
            .expect("store read")
            .is_none(),
        "a prepare that never got a usable quote leaves no attempt behind"
    );
}

/// The positive control for the refusal above: the same path, the same
/// engine, an honestly bound quote — bought, and redeemed by the
/// provider's own gate against the reservation it was bought for.
///
/// Without this row the refusal witness would pass with the whole
/// purchase path broken.
#[tokio::test]
async fn a_correctly_bound_quote_is_purchased_and_redeemed() {
    let w = world(false, false).await;
    let prepared = w
        .flow
        .prepare_task(NODE, &w.offer, &brief("t-bound"))
        .await
        .expect("prepare");
    let paid = w.flow.purchase_task(NODE, "t-bound").await;
    let A2aPurchase::Paid { proof, .. } = &paid else {
        panic!("expected Paid, got {paid:?}");
    };
    assert_eq!(w.billed().await, 1, "exactly one charge");
    let gate = EngineTaskAdmissionGate::new(w.engine.clone());
    let evidence = gate
        .redeem(TaskPaymentClaim {
            tool_id: "net.a2a.task/summarize",
            quote_id: &proof.quote_id,
            binding: &proof.binding_sig,
            expected_input_hash: &prepared.reservation.purchase_hash,
        })
        .await
        .expect("the provider's gate admits a correctly bound purchase");
    assert_eq!(evidence.quote_id, proof.quote_id);
}

// ---------------------------------------------------------------------------
// What a defaulted generation can and cannot match (finding R1)
// ---------------------------------------------------------------------------

/// `AttemptGeneration::default()` — `gen 0/` — is the identity of a row
/// written by a build from **before** generations existed, and it is the
/// only way to obtain one: every production mint carries `seq >= 1` and
/// a unique incarnation token.
///
/// Two such rows share that identity *value*, which is exactly the thing
/// that would be dangerous if the guard compared identities across
/// records. It does not: it resolves the record by key first and then
/// compares, so a shared value grants nothing. This pins all three
/// halves — a pre-generation decision cannot publish onto a minted
/// record, a minted decision cannot publish onto a pre-generation row,
/// and a pre-generation row is still writable by a decision taken
/// against it (or legacy attempts would become unrecoverable) while its
/// identity-sharing sibling is untouched.
#[tokio::test]
async fn a_defaulted_generation_matches_only_the_row_it_was_read_from() {
    let w = world(false, false).await;
    let legacy = w.flow.key(NODE, "t-legacy");
    let sibling = w.flow.key(NODE, "t-legacy-twin");
    for key in [&legacy, &sibling] {
        // Exactly what `serde(default)` produces for a file written
        // before the field existed: no generation, and a quote.
        let attempt = PurchaseAttempt {
            key: key.clone(),
            commitment: task_commitment(&w.offer, &brief(&key.task_id)),
            generation: AttemptGeneration::default(),
            prepared: None,
            quote_bytes: None,
            quote_id: Some("q-pre-generation".to_string()),
            quote_expires_at_ns: None,
            payload_bytes: None,
            state: PurchaseState::Quoted,
            updated_at_ns: NOW,
        };
        let id = key.id();
        mutate_json::<A2aPurchaseFile, _, _>(w.store.path(), move |file| {
            file.attempts.insert(id, attempt);
        })
        .await
        .expect("seed a pre-generation row");
    }
    let old = w.attempt("t-legacy").await;
    let twin = w.attempt("t-legacy-twin").await;
    assert_eq!(
        old.identity(),
        twin.identity(),
        "two pre-generation rows really do share an identity value"
    );

    w.flow
        .prepare_task(NODE, &w.offer, &brief("t-minted"))
        .await
        .expect("prepare");
    let minted = w.attempt("t-minted").await;
    assert_eq!(minted.generation.seq, 1);
    assert!(
        !minted.generation.incarnation.is_empty(),
        "production never mints a defaulted generation"
    );

    let onto_minted = w
        .store
        .transition_exact(
            &w.flow.key(NODE, "t-minted"),
            &[StateTag::Quoted],
            &old.identity(),
            PurchaseState::AwaitingApproval,
            NOW,
        )
        .await
        .expect_err("a pre-generation decision must not publish onto a minted record");
    assert!(
        matches!(onto_minted, PurchaseError::Superseded { .. }),
        "{onto_minted:?}"
    );
    let onto_legacy = w
        .store
        .transition_exact(
            &legacy,
            &[StateTag::Quoted],
            &minted.identity(),
            PurchaseState::AwaitingApproval,
            NOW,
        )
        .await
        .expect_err("nor a minted decision onto a pre-generation row");
    assert!(
        matches!(onto_legacy, PurchaseError::Superseded { .. }),
        "{onto_legacy:?}"
    );

    let recovered = w
        .store
        .transition_exact(
            &legacy,
            &[StateTag::Quoted],
            &old.identity(),
            PurchaseState::AwaitingApproval,
            NOW,
        )
        .await
        .expect("a pre-generation row stays writable by a decision taken against it");
    assert_eq!(recovered.state.tag(), StateTag::AwaitingApproval);
    assert_eq!(
        w.state("t-legacy-twin").await,
        StateTag::Quoted,
        "and the sibling that shares its identity value is untouched"
    );
    assert_eq!(
        w.state("t-minted").await,
        StateTag::Quoted,
        "as is the minted record"
    );
}

// ---------------------------------------------------------------------------
// A settled purchase whose key moved on (finding R1, retention half)
// ---------------------------------------------------------------------------

/// Refusing a stale write protects the replacement; it must not erase a
/// charge that already happened.
///
/// Staged without a sleep: the winner is parked **inside the
/// facilitator**, so the engine has claimed the quote and money is
/// genuinely in motion. The key is then pruned and prepared again, and
/// the replacement is driven to the point where the old decision's write
/// would be *accepted* on every other axis — deliberately minted at the
/// same caller clock instant against the same reservation, so it carries
/// the **same generation counter (1)**, the **same quote id**, and a
/// state tag the success write's `from` set contains. Nothing but the
/// incarnation token minted at creation can tell the two apart. A
/// generation that reset into reuse, or an identity built from the quote
/// id or the state tag, cannot pass this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_settled_purchase_whose_key_was_recreated_is_retained_for_reconciliation() {
    let w = world(true, false).await;
    w.flow
        .prepare_task(NODE, &w.offer, &brief("t-recreated"))
        .await
        .expect("prepare");
    let first_quote = w.attempt("t-recreated").await.quote_id.expect("quoted");

    let flow = w.flow.clone();
    let paying = tokio::spawn(async move { flow.purchase_task(NODE, "t-recreated").await });
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        w.settle_gate.await_entered(1),
    )
    .await
    .expect("the payment reached the facilitator");
    let superseded = w.attempt("t-recreated").await;
    assert_eq!(
        superseded.state.tag(),
        StateTag::Paying,
        "the payment is in flight under a persisted payload"
    );

    // The record is removed out of band, and a fresh prepare recreates
    // the key. Round 1 staged this through `A2aPurchaseStore::prune`,
    // which used to delete an in-flight `Paying` row — reviewer finding
    // R5, now repaired, so retention keeps it and that route is gone.
    // What replaces it is the same public recovery surface, used the way
    // an operator tool or a restored-from-backup file reaches it: a
    // locked mutation of `a2a-purchases.json` that drops this record.
    // The staging is *verified* rather than assumed (the row is read
    // back as absent), and the property under test is untouched: it is
    // about what a decision taken against one incarnation may write to
    // the next, not about how the key came to be recreated. No clock
    // moves, which is what keeps the replacement quote identical.
    let removed_id = superseded.key.id();
    mutate_json::<A2aPurchaseFile, _, _>(w.store.path(), move |file| {
        file.attempts.remove(&removed_id);
    })
    .await
    .expect("remove the in-flight record out from under the payment");
    assert!(
        w.flow
            .stored_attempt(NODE, "t-recreated")
            .await
            .expect("store read")
            .is_none(),
        "the in-flight attempt is gone, so the next prepare recreates the key"
    );
    w.flow
        .prepare_task(NODE, &w.offer, &brief("t-recreated"))
        .await
        .expect("reprepare");
    let live = w.attempt("t-recreated").await;
    assert_eq!(
        live.quote_id.as_deref(),
        Some(first_quote.as_str()),
        "the replacement carries the SAME quote id — so the quote id alone \
         cannot tell the two incarnations apart"
    );
    assert_eq!(
        (live.generation.seq, superseded.generation.seq),
        (1, 1),
        "and the SAME generation counter — a per-key counter alone cannot either"
    );

    // Drive the replacement to where the old decision's write would
    // otherwise land: it re-sends its (byte-identical) payload, meets the
    // engine's in-flight claim from the payment still parked in the
    // facilitator, and records that ambiguity. `Unknown` is in the
    // success write's `from` set, so after this the state tag cannot
    // refuse the stale write either.
    let replacement = w.flow.purchase_task(NODE, "t-recreated").await;
    assert!(
        matches!(replacement, A2aPurchase::Unknown { .. }),
        "the replacement meets the in-flight payment: {replacement:?}"
    );
    assert_eq!(w.state("t-recreated").await, StateTag::Unknown);

    w.settle_gate.release();
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), paying)
        .await
        .expect("the parked payment finished")
        .expect("no panic");

    // The replacement is untouched: nobody authorized a payment against
    // *its* incarnation, and its own recovery is still ahead of it.
    assert_eq!(
        w.state("t-recreated").await,
        StateTag::Unknown,
        "the recreated attempt must not be marked paid by the old decision"
    );
    assert!(
        !matches!(result, A2aPurchase::Paid { .. }),
        "the old decision must not report the replacement as bought: {result:?}"
    );
    // Every other axis matched, so the token minted at creation is the
    // only thing that refused the stale write.
    assert_ne!(
        live.generation.incarnation, superseded.generation.incarnation,
        "the two incarnations must not share an identity"
    );

    // And the charge that really happened is still reachable.
    let retained = w
        .flow
        .superseded_attempt(NODE, "t-recreated", &superseded.generation)
        .await
        .expect("store read")
        .expect("the settled purchase is retained against its own incarnation");
    let PurchaseState::PaidUnexecutable { proof, refusal, .. } = &retained.state else {
        panic!(
            "expected retained payment evidence, got {:?}",
            retained.state
        );
    };
    assert_eq!(
        proof.quote_id, first_quote,
        "the evidence names what was paid"
    );
    assert_eq!(refusal.reason.as_deref(), Some(SUPERSEDED_REFUSAL_REASON));
    assert!(!refusal.safe_to_requote, "buying again is not the answer");
    assert!(
        retained.is_unresolved_financial(),
        "so retention can never delete it"
    );
    assert_eq!(w.billed().await, 1, "one charge happened, and only one");

    // The operator's queue shows both, and the operator can close the
    // retained charge without touching the live attempt.
    let queue = w.flow.attempts().await.expect("attempts");
    assert_eq!(queue.len(), 2, "one live attempt, one retained charge");

    // Retention keeps both: the replacement's ambiguity and the retained
    // charge are each the caller's only evidence about money.
    assert_eq!(
        w.store
            .prune(w.clock.now_ns() + PREPARE_LEASE_NS, 0)
            .await
            .expect("prune"),
        0,
        "an aggressive sweep deletes neither"
    );
    let survived = w
        .flow
        .superseded_attempt(NODE, "t-recreated", &superseded.generation)
        .await
        .expect("store read")
        .expect("the retained charge survives retention");
    assert_eq!(survived.state, retained.state, "unchanged by the sweep");

    let closed = w
        .flow
        .resolve_superseded_attempt(
            NODE,
            "t-recreated",
            &superseded.generation,
            AttemptResolution::Closed {
                outcome: "refunded by the provider operator".to_string(),
                evidence: serde_json::json!({ "ticket": "OPS-31" }),
            },
        )
        .await
        .expect("operator resolution");
    assert_eq!(closed.state.tag(), StateTag::Resolved);
}

// ---------------------------------------------------------------------------
// An exposed payment outlives the caller that sent it (finding R5)
// ---------------------------------------------------------------------------

/// A caller that dies with a payment in flight must still find **that
/// exact purchase** afterwards, retention included — and must recover it
/// through the stored payload rather than buying again.
///
/// Staged without a sleep and without a seeded state: the payment is
/// parked **inside the facilitator**, so the engine has claimed the quote
/// and the money is genuinely in motion, and then the task driving it is
/// **aborted**. That is the caller-termination window — the payload is
/// exposed, and no outcome is recorded in the caller's store, in the
/// engine, or anywhere else. An aggressive sweep then runs (zero
/// retention, a clock far past every lease window) and must keep the row:
/// `Paying` is the state that holds the byte-exact payload, and
/// re-sending it is the only way to learn what became of the charge.
/// Deleting it would destroy that evidence *and* let the next prepare
/// mint a second quote for work that may already be paid for.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_exposed_payment_survives_caller_death_and_retention() {
    let w = world(true, false).await;
    w.flow
        .prepare_task(NODE, &w.offer, &brief("t-died"))
        .await
        .expect("prepare");

    let flow = w.flow.clone();
    let paying = tokio::spawn(async move { flow.purchase_task(NODE, "t-died").await });
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        w.settle_gate.await_entered(1),
    )
    .await
    .expect("the payment reached the facilitator");
    let exposed = w.attempt("t-died").await;
    assert_eq!(
        exposed.state.tag(),
        StateTag::Paying,
        "the claim is durable before the payment goes out"
    );
    let sent = exposed
        .payload_bytes
        .clone()
        .expect("the exact payload is persisted before the send");
    assert_eq!(
        w.channel.pay_sends(),
        1,
        "the payload really did leave this caller"
    );

    // The caller dies here: the future holding the payment is dropped
    // while the engine's claim is durable and nothing has been recorded.
    paying.abort();
    assert!(
        paying
            .await
            .expect_err("the purchase was cancelled")
            .is_cancelled(),
        "the caller terminated mid-payment"
    );

    // Zero retention, and a clock well past every lease window: the most
    // aggressive sweep the public API can be asked for.
    assert_eq!(
        w.store
            .prune(w.clock.now_ns() + 10 * PREPARE_LEASE_NS, 0)
            .await
            .expect("prune"),
        0,
        "an exposed payment is not finished work and must not be swept"
    );
    let kept = w.attempt("t-died").await;
    assert_eq!(kept.state.tag(), StateTag::Paying);
    assert_eq!(
        kept.payload_bytes.as_deref(),
        Some(sent.as_slice()),
        "the byte-exact payload survived, which is what makes recovery possible"
    );
    assert_eq!(
        kept.generation, exposed.generation,
        "and it is the same incarnation, not a replacement"
    );

    // Recovery: the parked settlement is let go, and the clock moves past
    // the caller's lease window and the engine's in-flight TTL (both well
    // inside the quote's own hour) so the abandoned claim is reclaimable.
    w.settle_gate.release();
    w.clock.advance(600_000_000_000);
    let resumed = w.flow.purchase_task(NODE, "t-died").await;
    assert!(
        matches!(resumed, A2aPurchase::Paid { .. }),
        "the exact purchase must recover after caller death and retention: {resumed:?}"
    );
    assert_eq!(
        (w.channel.pay_sends(), w.channel.distinct_payloads()),
        (2, 1),
        "recovery re-sent the stored payload verbatim; it never authored a second one"
    );
    assert_eq!(
        w.channel.quote_calls(),
        1,
        "and never bought a second quote"
    );
    assert_eq!(w.billed().await, 1, "the charge happened exactly once");
}

// ---------------------------------------------------------------------------
// Convergence on one recoverable verdict (finding R10)
// ---------------------------------------------------------------------------

/// A duplicate that published an ambiguity must not make the original's
/// authoritative success unrepresentable — and must not cost a third
/// round trip to recover.
///
/// Staged, not slept: the winner is parked inside the facilitator with
/// the engine's in-flight claim held, so the duplicate's re-send of the
/// **identical** payload meets the production `InProgress` answer and
/// records the ambiguity that actually arises. Then the winner's
/// settlement completes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_later_authoritative_success_survives_a_duplicates_ambiguity() {
    let w = world(true, false).await;
    w.flow
        .prepare_task(NODE, &w.offer, &brief("t-ambiguous"))
        .await
        .expect("prepare");

    let flow = w.flow.clone();
    let winner = tokio::spawn(async move { flow.purchase_task(NODE, "t-ambiguous").await });
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        w.settle_gate.await_entered(1),
    )
    .await
    .expect("the winner's payment reached the facilitator");
    assert_eq!(w.state("t-ambiguous").await, StateTag::Paying);

    // The winner's claim goes stale (it is stuck in settlement), so the
    // duplicate re-sends the stored payload rather than waiting.
    w.clock.advance(PREPARE_LEASE_NS + 1);
    let duplicate = w.flow.purchase_task(NODE, "t-ambiguous").await;
    assert!(
        matches!(duplicate, A2aPurchase::Unknown { .. }),
        "the duplicate meets an in-flight payment and publishes ambiguity: {duplicate:?}"
    );
    assert_eq!(
        w.state("t-ambiguous").await,
        StateTag::Unknown,
        "which is what the record says while the original is still settling"
    );
    assert_eq!(w.channel.pay_sends(), 2, "two sends");
    assert_eq!(w.channel.distinct_payloads(), 1, "of one payload");

    w.settle_gate.release();
    let settled = tokio::time::timeout(std::time::Duration::from_secs(5), winner)
        .await
        .expect("the parked payment finished")
        .expect("no panic");
    let A2aPurchase::Paid { proof, .. } = &settled else {
        panic!("the original's authoritative success must be publishable: {settled:?}");
    };
    assert_eq!(
        w.state("t-ambiguous").await,
        StateTag::Paid,
        "and it is what the record ends up saying"
    );
    assert_eq!(
        w.channel.pay_sends(),
        2,
        "no third send was needed to reach a resolved state"
    );
    assert_eq!(w.billed().await, 1, "one charge");
    assert_eq!(w.channel.quote_calls(), 1, "and never a second quote");

    // The proof the success published is the one the provider admits.
    let prepared = w.attempt("t-ambiguous").await.prepared.expect("prepared");
    let gate = EngineTaskAdmissionGate::new(w.engine.clone());
    let evidence = gate
        .redeem(TaskPaymentClaim {
            tool_id: "net.a2a.task/summarize",
            quote_id: &proof.quote_id,
            binding: &proof.binding_sig,
            expected_input_hash: &prepared.reservation.purchase_hash,
        })
        .await
        .expect("the gate admits the recovered proof");
    assert_eq!(evidence.quote_id, proof.quote_id);
}

/// Two submitters of one paid purchase, both answered with the same
/// **retryable** structured refusal, both keep the purchase `Paid`.
///
/// Staged: neither submit returns until both are inside the provider, so
/// this is the concurrent claim-write failure — the deciding submitter
/// and the waiting duplicate — not two sequential retries.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_submitters_of_a_retryable_refusal_both_keep_the_purchase_paid() {
    let w = world(false, true).await;
    w.flow
        .prepare_task(NODE, &w.offer, &brief("t-journal"))
        .await
        .expect("prepare");
    let paid = w.flow.purchase_task(NODE, "t-journal").await;
    let A2aPurchase::Paid { proof, .. } = &paid else {
        panic!("expected Paid, got {paid:?}");
    };

    // The provider's admission journal is unavailable: the decider and
    // every waiter receive that same verdict, verbatim.
    for _ in 0..2 {
        w.tasks.push_submit(Err(A2aFlowError::PaymentRefused {
            message: "the admission journal is unavailable".to_string(),
            schematic: Some(Box::new(schematic("journal_unavailable", true, false))),
        }));
    }
    let a = {
        let flow = w.flow.clone();
        tokio::spawn(async move { flow.submit_task(NODE, "t-journal").await })
    };
    let b = {
        let flow = w.flow.clone();
        tokio::spawn(async move { flow.submit_task(NODE, "t-journal").await })
    };
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        w.submit_gate.await_entered(2),
    )
    .await
    .expect("both submitters are inside the provider at once");
    w.submit_gate.release();
    let outcomes = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        (a.await.expect("no panic"), b.await.expect("no panic"))
    })
    .await
    .expect("both submitters returned");

    for outcome in [&outcomes.0, &outcomes.1] {
        assert_eq!(
            outcome,
            &A2aSubmit::Retry {
                message: "the admission journal is unavailable".to_string()
            },
            "a retryable refusal is a retry for both submitters, not a verdict about money"
        );
    }
    assert_eq!(
        w.state("t-journal").await,
        StateTag::Paid,
        "and the purchase keeps its evidence, so the SAME proof is resubmitted"
    );
    assert_eq!(w.billed().await, 1, "no second charge");
    assert_eq!(w.channel.quote_calls(), 1, "no second quote");

    // And the same proof, resubmitted once the journal recovers, is
    // accepted — the retryable reading was the correct one.
    let accepted = w.flow.submit_task(NODE, "t-journal").await;
    assert_eq!(
        accepted,
        A2aSubmit::Accepted {
            task_id: "t-journal".to_string()
        }
    );
    assert_eq!(w.tasks.submit_calls.load(Ordering::SeqCst), 3);
    let attempt = w.attempt("t-journal").await;
    assert_eq!(
        attempt.state,
        PurchaseState::Submitted {
            task_id: "t-journal".to_string()
        }
    );
    assert_eq!(
        proof.quote_id,
        attempt.quote_id.expect("the quote that paid"),
        "the accepted submission is the purchase that was paid for"
    );
}

/// A rejected ack carries no recovery posture, so it cannot establish
/// that a **paid** purchase will never execute. The attempt keeps its
/// evidence and the same proof is resubmitted.
///
/// The refusal used here is a capacity refusal whose wording is not the
/// exact sentence `SubmitRejection::Busy` renders — which is the whole
/// point: a classification that reads one variant's prose and treats
/// every other sentence as permanent strands a paid purchase the moment
/// that wording drifts or the taxonomy grows.
#[tokio::test]
async fn an_unstructured_rejected_ack_does_not_strand_a_paid_purchase() {
    let w = world(false, false).await;
    w.flow
        .prepare_task(NODE, &w.offer, &brief("t-prose"))
        .await
        .expect("prepare");
    let paid = w.flow.purchase_task(NODE, "t-prose").await;
    let A2aPurchase::Paid { proof, .. } = &paid else {
        panic!("expected Paid, got {paid:?}");
    };
    let drifted = "this service is at capacity right now; retry once a slot frees up";
    assert_ne!(
        drifted,
        SubmitRejection::Busy.to_string(),
        "the fixture is only meaningful if the wording differs from the pinned sentence"
    );
    w.tasks.push_submit(Ok(TaskAck {
        task_id: "t-prose".to_string(),
        accepted: false,
        reason: Some(drifted.to_string()),
    }));

    let outcome = w.flow.submit_task(NODE, "t-prose").await;
    assert_eq!(
        outcome,
        A2aSubmit::Retry {
            message: drifted.to_string()
        },
        "an unstructured refusal of a paid purchase is retryable"
    );
    assert_eq!(
        w.state("t-prose").await,
        StateTag::Paid,
        "the payment evidence stays on the attempt"
    );

    // Terminality is still reachable — through the structured verdict,
    // which is the only evidence that can carry it. (Positive control:
    // without this row the test above would pass with every refusal
    // collapsed into a retry.)
    w.tasks.push_submit(Err(A2aFlowError::PaymentRefused {
        message: SubmitRejection::NoReservation.to_string(),
        schematic: Some(Box::new(schematic("no_reservation", false, false))),
    }));
    let terminal = w.flow.submit_task(NODE, "t-prose").await;
    let A2aSubmit::Unexecutable { refusal } = &terminal else {
        panic!("a structured terminal refusal must still be terminal: {terminal:?}");
    };
    assert_eq!(refusal.reason.as_deref(), Some("no_reservation"));
    let attempt = w.attempt("t-prose").await;
    let PurchaseState::PaidUnexecutable {
        proof: kept_proof, ..
    } = &attempt.state
    else {
        panic!("expected PaidUnexecutable, got {:?}", attempt.state);
    };
    assert_eq!(kept_proof, proof, "with the payment evidence retained");
    assert_eq!(w.billed().await, 1, "and one charge throughout");
}

/// A **retired** task is the case the provider's paid-path split exists
/// for: the caller holds a proof for work that will never run again, so
/// the refusal has to be terminal and machine-readable or the charge is
/// stranded as `Paid` forever with no operator exit.
///
/// Its own attempt on purpose — a `PaidUnexecutable` record is already
/// terminal and answers later submits from its retained refusal, so this
/// cannot share the control above's task without measuring that instead.
#[tokio::test]
async fn a_retired_paid_submit_is_terminal_with_its_evidence_kept() {
    let w = world(false, false).await;
    w.flow
        .prepare_task(NODE, &w.offer, &brief("t-retired"))
        .await
        .expect("prepare");
    let paid = w.flow.purchase_task(NODE, "t-retired").await;
    let A2aPurchase::Paid { proof, .. } = &paid else {
        panic!("expected Paid, got {paid:?}");
    };

    w.tasks.push_submit(Err(A2aFlowError::PaymentRefused {
        message: "this task already ran and its result has been retired".to_string(),
        schematic: Some(Box::new(schematic("retired", false, false))),
    }));
    let retired = w.flow.submit_task(NODE, "t-retired").await;
    let A2aSubmit::Unexecutable { refusal } = &retired else {
        panic!("a retired paid submit must be terminal, not a retry: {retired:?}");
    };
    assert_eq!(refusal.reason.as_deref(), Some("retired"));

    let attempt = w.attempt("t-retired").await;
    let PurchaseState::PaidUnexecutable {
        proof: kept_proof, ..
    } = &attempt.state
    else {
        panic!("expected PaidUnexecutable, got {:?}", attempt.state);
    };
    assert_eq!(
        kept_proof, proof,
        "the payment evidence is retained for the operator"
    );
    assert!(
        attempt.is_unresolved_financial(),
        "and it is never pruned while unresolved"
    );
    assert_eq!(
        w.billed().await,
        1,
        "still exactly one charge — a retired refusal must never buy again"
    );
}

// ---------------------------------------------------------------------------
// An expired quote and a settlement that is still running
// ---------------------------------------------------------------------------

/// A quote that lapses while its settlement is **still inside the
/// engine's in-flight window** is the sharp case of the claim order: the
/// attempt holding the quote is demonstrably alive (its money is parked
/// at the facilitator, and the record's claim is nowhere near the
/// reclaim TTL), so a concurrent re-send of the identical payload must
/// be told *ambiguity*, never `quote expired`. Expiry bounds taking a
/// **new** settlement; it cannot retroactively describe one already
/// admitted.
///
/// Staged by pricing the quote's life ([`LAPSING_QUOTE_TTL_NS`]) below
/// the engine's in-flight window ([`IN_FLIGHT_TTL_NS`], which this file
/// sets), which is the only way a quote can expire while its claim is
/// still fresh. The reviewer's own expiry probe advances far past both,
/// so it exercises the lapsed-claim arm; this row is the live-claim one,
/// and only the order of the two guards answers it.
///
/// Both halves of the staging are *checked*, not assumed, because the
/// engine's answer cannot report either: a duplicate under a stale claim
/// and an expired quote is also told `InProgress`, and so is one under
/// an unexpired quote. So the clock relation is a compile-time assert on
/// the constants, and the quote's death is read back off the record
/// before the duplicate is sent.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_expired_quote_with_a_live_settlement_answers_ambiguity() {
    let w = world_with_quote_ttl(true, false, LAPSING_QUOTE_TTL_NS).await;
    w.flow
        .prepare_task(NODE, &w.offer, &brief("t-live-expiry"))
        .await
        .expect("prepare");
    let flow = w.flow.clone();
    let winner = tokio::spawn(async move { flow.purchase_task(NODE, "t-live-expiry").await });
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        w.settle_gate.await_entered(1),
    )
    .await
    .expect("the payment reached the facilitator");
    assert_eq!(w.state("t-live-expiry").await, StateTag::Paying);

    // Past the quote's expiry, well short of the in-flight reclaim TTL
    // (`PAST_LAPSED_QUOTE_NS` is pinned between the two).
    w.clock.advance(PAST_LAPSED_QUOTE_NS);
    let lapsed_at = w
        .attempt("t-live-expiry")
        .await
        .quote_expires_at_ns
        .expect("a quote with a life");
    assert!(
        w.clock.now_ns() > lapsed_at,
        "the quote this duplicate re-sends against really has lapsed: \
         now {} vs expiry {lapsed_at}",
        w.clock.now_ns()
    );
    let duplicate = w.flow.purchase_task(NODE, "t-live-expiry").await;
    assert!(
        matches!(duplicate, A2aPurchase::Unknown { .. }),
        "a settlement already in flight is not proven unpaid by the clock: {duplicate:?}"
    );
    assert_eq!(
        w.state("t-live-expiry").await,
        StateTag::Unknown,
        "the record keeps the payload and the reservation for recovery"
    );

    w.settle_gate.release();
    let settled = tokio::time::timeout(std::time::Duration::from_secs(5), winner)
        .await
        .expect("the parked payment finished")
        .expect("no panic");
    assert!(
        matches!(settled, A2aPurchase::Paid { .. }),
        "and the real outcome still publishes over the ambiguity: {settled:?}"
    );
    assert_eq!(w.state("t-live-expiry").await, StateTag::Paid);
    assert_eq!(w.billed().await, 1, "one charge, once");
}

// ---------------------------------------------------------------------------
// Evidence precedence: what a late or weaker outcome may overwrite
// ---------------------------------------------------------------------------

/// The settlement evidence of one attempt, shaped the way the flow
/// retains a charge whose intent key would not accept it.
fn paid_evidence(attempt: &PurchaseAttempt) -> PurchaseState {
    let PurchaseState::Paid { proof, billing } = &attempt.state else {
        panic!("expected a paid attempt, got {:?}", attempt.state);
    };
    PurchaseState::PaidUnexecutable {
        proof: proof.clone(),
        billing: billing.clone(),
        refusal: net_payments::flow::a2a::RefusalRecord {
            at_ns: NOW,
            message: "this purchase settled and its key moved on".to_string(),
            reason: Some(SUPERSEDED_REFUSAL_REASON.to_string()),
            safe_to_retry: false,
            safe_to_requote: false,
        },
    }
}

/// A sibling's **terminal** verdict cannot make an authoritative
/// settlement disappear.
///
/// The ambiguity case has its own row above (`Unknown` is in the success
/// write's `from` set, so the success converges onto the live record).
/// This is the harder one: the sibling published something the table has
/// no transition out of, so the success write is refused — and a refused
/// write used to hand the charge back inside a retryable error, leaving
/// the only record of it in the provider's billing log.
///
/// Reachable in production whenever a concurrent re-send of the stored
/// payload gets a terminal answer while the original settlement is still
/// at the rail: a frozen quote, an `Invalidated` verdict, a
/// non-retryable transport failure. Staged here through the same store
/// verb `refuse_exposed` itself uses, under the *same* identity, so what
/// is measured is the success write meeting that verdict — not a
/// fabricated record.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_settlement_is_retained_over_a_siblings_exposed_refusal() {
    let w = world(true, false).await;
    w.flow
        .prepare_task(NODE, &w.offer, &brief("t-sibling-refusal"))
        .await
        .expect("prepare");
    let quote_id = w
        .attempt("t-sibling-refusal")
        .await
        .quote_id
        .expect("quoted");
    let flow = w.flow.clone();
    let winner = tokio::spawn(async move { flow.purchase_task(NODE, "t-sibling-refusal").await });
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        w.settle_gate.await_entered(1),
    )
    .await
    .expect("the payment reached the facilitator");
    let in_flight = w.attempt("t-sibling-refusal").await;
    assert_eq!(
        in_flight.state.tag(),
        StateTag::Paying,
        "the payment is in flight under a persisted payload"
    );

    w.store
        .transition_exact(
            &in_flight.key,
            &[StateTag::Paying],
            &in_flight.identity(),
            PurchaseState::RefusedExposed {
                reason: "provider rejected the payment: quote is frozen".to_string(),
                reservation_kept: true,
            },
            w.clock.now_ns(),
        )
        .await
        .expect("the sibling's exposed refusal lands on the record");

    w.settle_gate.release();
    let settled = tokio::time::timeout(std::time::Duration::from_secs(5), winner)
        .await
        .expect("the parked payment finished")
        .expect("no panic");

    // The sibling's verdict is not unmade — it was decided against this
    // incarnation and an operator closes it — but it is not allowed to
    // be the only thing the store remembers.
    assert_eq!(
        w.state("t-sibling-refusal").await,
        StateTag::RefusedExposed,
        "the live record keeps the verdict it was given"
    );
    assert!(
        matches!(
            settled,
            A2aPurchase::Failed {
                retryable: false,
                ..
            }
        ),
        "a settled payment whose record refused it is not a retryable failure: {settled:?}"
    );
    let retained = w
        .flow
        .superseded_attempt(NODE, "t-sibling-refusal", &in_flight.generation)
        .await
        .expect("store read")
        .expect("the settlement is retained against the incarnation that bought it");
    let PurchaseState::PaidUnexecutable {
        proof,
        billing,
        refusal,
    } = &retained.state
    else {
        panic!(
            "expected retained settlement evidence, got {:?}",
            retained.state
        );
    };
    assert_eq!(
        proof.quote_id, quote_id,
        "the retained evidence names the quote that was paid"
    );
    assert!(
        !billing.is_null(),
        "with the billing proof, which is the half reconciliation cannot rebuild"
    );
    assert_eq!(refusal.reason.as_deref(), Some(SUPERSEDED_REFUSAL_REASON));
    assert_eq!(w.billed().await, 1, "one charge happened, and only one");
    assert_eq!(
        w.store
            .prune(w.clock.now_ns() + PREPARE_LEASE_NS, 0)
            .await
            .expect("prune"),
        0,
        "an aggressive sweep deletes neither class"
    );
}

/// A live task id that *spells* an archive record id is a legitimate
/// task id, and retaining a charge must not land on its purchase.
///
/// Both insertion orders are witnessed, because a shared namespace
/// breaks in both directions: this one retains the archive row second
/// (the write lands on an existing live record), the next buys the
/// colliding task second (the live insert lands on an existing archive
/// record). Neither may touch the other, and each keeps its own
/// evidence.
#[tokio::test]
async fn retaining_a_charge_cannot_overwrite_a_live_task_that_spells_its_archive_id() {
    let w = world(false, false).await;
    w.flow
        .prepare_task(NODE, &w.offer, &brief("t-collide-late"))
        .await
        .expect("prepare the charge");
    assert!(matches!(
        w.flow.purchase_task(NODE, "t-collide-late").await,
        A2aPurchase::Paid { .. }
    ));
    let charged = w.attempt("t-collide-late").await;
    let colliding = format!(
        "t-collide-late#superseded/{}-{}",
        charged.generation.seq, charged.generation.incarnation
    );
    w.flow
        .prepare_task(NODE, &w.offer, &brief(&colliding))
        .await
        .expect("prepare the legitimate task whose id spells the archive key");
    assert!(matches!(
        w.flow.purchase_task(NODE, &colliding).await,
        A2aPurchase::Paid { .. }
    ));
    let live_before = w.attempt(&colliding).await;
    assert_ne!(
        live_before.quote_id, charged.quote_id,
        "the two purchases are genuinely different work"
    );

    w.store
        .retain_superseded(&charged, paid_evidence(&charged), NOW + 1)
        .await
        .expect("retain the charge");

    assert_eq!(
        w.attempt(&colliding).await,
        live_before,
        "the live purchase is byte-identical after the archive write"
    );
    let retained = w
        .flow
        .superseded_attempt(NODE, "t-collide-late", &charged.generation)
        .await
        .expect("store read")
        .expect("the charge is retained");
    assert_eq!(
        retained.state,
        paid_evidence(&charged),
        "and the archive holds the charge's own evidence, not the live purchase's"
    );
    assert_eq!(
        retained.quote_id, charged.quote_id,
        "addressed by the incarnation that paid it"
    );
}

/// The other order: the archive row exists first, and a legitimate task
/// whose id spells it is purchased afterwards.
#[tokio::test]
async fn a_live_task_that_spells_an_archive_id_cannot_overwrite_the_retained_charge() {
    let w = world(false, false).await;
    w.flow
        .prepare_task(NODE, &w.offer, &brief("t-collide-early"))
        .await
        .expect("prepare the charge");
    assert!(matches!(
        w.flow.purchase_task(NODE, "t-collide-early").await,
        A2aPurchase::Paid { .. }
    ));
    let charged = w.attempt("t-collide-early").await;
    let evidence = paid_evidence(&charged);
    w.store
        .retain_superseded(&charged, evidence.clone(), NOW + 1)
        .await
        .expect("retain the charge");

    let colliding = format!(
        "t-collide-early#superseded/{}-{}",
        charged.generation.seq, charged.generation.incarnation
    );
    w.flow
        .prepare_task(NODE, &w.offer, &brief(&colliding))
        .await
        .expect("prepare the legitimate task whose id spells the archive key");
    let bought = w.flow.purchase_task(NODE, &colliding).await;
    assert!(
        matches!(bought, A2aPurchase::Paid { .. }),
        "a task id is not reserved by an archive record: {bought:?}"
    );

    let retained = w
        .flow
        .superseded_attempt(NODE, "t-collide-early", &charged.generation)
        .await
        .expect("store read")
        .expect("the retained charge is still there");
    assert_eq!(
        retained.state, evidence,
        "buying the colliding task did not overwrite the retained charge"
    );
    assert_eq!(
        retained.quote_id, charged.quote_id,
        "nor its identity: the archive row still names the payment it holds"
    );
    assert_eq!(
        w.flow.attempts().await.expect("attempts").len(),
        3,
        "the queue shows two live purchases and one retained charge"
    );
    assert_eq!(
        w.flow.retained_attempts().await.expect("retained").len(),
        1,
        "exactly one of them closes through the superseded exit"
    );
}

/// An operator's disposition is not un-decided by a result that was
/// already in flight when they decided it — and a settlement that
/// arrives afterwards does not vanish either.
///
/// Two schedules, because the archive takes writes from both sides:
/// resolved-first/settlement-second (the sharp one — the late outcome
/// carries proof and billing, the two facts reconciliation cannot
/// rebuild from the caller's store), and resolved-first/ambiguity-second
/// (which knows strictly less than the disposition and is dropped).
///
/// The operator's document is a colliding one — it spells
/// `late_settlement` itself — because the generated evidence must be
/// retained *beside* an opaque document, never written into it: the
/// caller-side store reserves no field inside what a human recorded.
#[tokio::test]
async fn a_resolved_disposition_keeps_a_late_settlement_findable() {
    let w = world(false, false).await;
    w.flow
        .prepare_task(NODE, &w.offer, &brief("t-late-settlement"))
        .await
        .expect("prepare");
    assert!(matches!(
        w.flow.purchase_task(NODE, "t-late-settlement").await,
        A2aPurchase::Paid { .. }
    ));
    let charged = w.attempt("t-late-settlement").await;
    let quote_id = charged.quote_id.clone().expect("quoted");

    // An exposed refusal was retained against this incarnation, and the
    // operator closed it: as far as the books are concerned, this charge
    // is accounted for.
    w.store
        .retain_superseded(
            &charged,
            PurchaseState::RefusedExposed {
                reason: "the provider refused after the authorization was exposed".to_string(),
                reservation_kept: true,
            },
            NOW + 1,
        )
        .await
        .expect("retain the exposed refusal");
    // Operator evidence is unrestricted JSON, and this document
    // already spells the property the generated evidence is named by.
    // Staged that way on purpose: it is the case where reserving a
    // field inside the operator's document destroys the only copy of
    // something a human recorded deliberately.
    let recorded = serde_json::json!({
        "ticket": "OPS-77",
        "late_settlement": "reconciled by hand against invoice 41",
    });
    let closed = w
        .flow
        .resolve_superseded_attempt(
            NODE,
            "t-late-settlement",
            &charged.generation,
            AttemptResolution::Closed {
                outcome: "written off".to_string(),
                evidence: recorded.clone(),
            },
        )
        .await
        .expect("operator resolution");
    assert_eq!(closed.state.tag(), StateTag::Resolved);
    // Where that disposition lives is the assumption the rest of this
    // schedule rests on, so it is asserted rather than inferred: the
    // archive row holds it, and the live purchase under the same key
    // and generation is untouched. Without these two, an operator exit
    // that resolved the LIVE row instead would read identically — the
    // late evidence below would simply inherit the disposition from
    // there (`retain_superseded` seeds the archive from a live
    // `Resolved` row of the same incarnation) and every assertion after
    // it would still hold.
    assert_eq!(
        w.flow
            .superseded_attempt(NODE, "t-late-settlement", &charged.generation)
            .await
            .expect("archive read")
            .expect("the archive holds this incarnation")
            .state
            .tag(),
        StateTag::Resolved,
        "the operator's disposition is the archive row's"
    );
    assert_eq!(
        w.attempt("t-late-settlement").await.state,
        charged.state,
        "and the live purchase is exactly as it was: the superseded exit \
         cannot reach it"
    );

    // The settlement the operator never saw finally lands.
    let after = w
        .store
        .retain_superseded(&charged, paid_evidence(&charged), NOW + 2)
        .await
        .expect("late settlement");
    let PurchaseState::Resolved { outcome, evidence } = &after.state else {
        panic!(
            "the operator's disposition must stand, got {:?}",
            after.state
        );
    };
    assert_eq!(outcome, "written off", "nothing reopened it");
    let preserved = evidence
        .pointer("/net.payments.a2a.late_settlement@1/operator_evidence")
        .expect("the operator's document is kept whole under the envelope");
    assert_eq!(
        preserved, &recorded,
        "what the operator recorded is preserved byte for byte, including the \
         property the generated evidence shares a name with: {evidence}"
    );
    let late = evidence
        .pointer("/net.payments.a2a.late_settlement@1/late_settlement")
        .expect("the late charge is retained beside the disposition");
    assert_eq!(
        late.pointer("/proof/quote_id").and_then(|q| q.as_str()),
        Some(quote_id.as_str()),
        "naming the quote that was actually paid"
    );
    assert!(
        late.get("billing").is_some_and(|b| !b.is_null()),
        "with the billing event: {late}"
    );
    assert_eq!(
        w.attempt("t-late-settlement").await.state,
        charged.state,
        "the late settlement landed in the archive and nowhere else — the \
         live row is still the charge it always was"
    );

    // A weaker late outcome knows less than the disposition and is
    // dropped rather than recorded as a second story.
    let unchanged = w
        .store
        .retain_superseded(
            &charged,
            PurchaseState::Unknown {
                last_error: "a sibling timed out long ago".to_string(),
            },
            NOW + 3,
        )
        .await
        .expect("late ambiguity");
    assert_eq!(
        unchanged.state, after.state,
        "a delayed ambiguity neither reopens the disposition nor displaces the late charge"
    );
}

/// One key can hold both a live purchase and the retained evidence of an
/// incarnation — and an operator must be able to close either without
/// touching the other.
///
/// Staged at the hardest point: the two rows share the *same* key and
/// the *same* generation, so nothing in the identity distinguishes them.
/// Only the class does, and the class is a different map, not a decorated
/// id — so the generation-addressed exit reaches exactly the historical
/// row, and the key-only exit cannot reach it at all.
#[tokio::test]
async fn a_retained_charge_and_a_live_attempt_are_closed_independently() {
    let w = world(false, false).await;
    w.flow
        .prepare_task(NODE, &w.offer, &brief("t-two-classes"))
        .await
        .expect("prepare");
    assert!(matches!(
        w.flow.purchase_task(NODE, "t-two-classes").await,
        A2aPurchase::Paid { .. }
    ));
    let live_before = w.attempt("t-two-classes").await;
    w.store
        .retain_superseded(&live_before, paid_evidence(&live_before), NOW + 1)
        .await
        .expect("retain a charge against this very incarnation");

    // The queue lists both, and each row carries the pair that addresses
    // it — which is what an operator (or a binding) reads back.
    let queue = w.flow.attempts().await.expect("attempts");
    assert_eq!(queue.len(), 2, "one live purchase, one retained charge");
    assert!(
        queue
            .iter()
            .all(|a| a.key == live_before.key && a.generation == live_before.generation),
        "the two rows are indistinguishable by identity: {queue:?}"
    );
    let retained = w.flow.retained_attempts().await.expect("retained");
    assert_eq!(retained.len(), 1, "and the classes are distinguishable");
    assert_eq!(retained[0].state.tag(), StateTag::PaidUnexecutable);

    let closed = w
        .flow
        .resolve_superseded_attempt(
            NODE,
            "t-two-classes",
            &live_before.generation,
            AttemptResolution::Closed {
                outcome: "refunded".to_string(),
                evidence: serde_json::json!({ "ticket": "OPS-91" }),
            },
        )
        .await
        .expect("the historical identity is addressable");
    assert_eq!(closed.state.tag(), StateTag::Resolved);
    assert_eq!(
        w.attempt("t-two-classes").await,
        live_before,
        "closing the historical charge left the live purchase byte-identical"
    );

    // And the key-only exit cannot close the live charge in its place:
    // a paid purchase is not an operator's to write off.
    let refused = w
        .flow
        .resolve_attempt(
            NODE,
            "t-two-classes",
            AttemptResolution::Closed {
                outcome: "refunded".to_string(),
                evidence: serde_json::json!({ "ticket": "OPS-91" }),
            },
        )
        .await;
    assert!(
        matches!(
            refused,
            Err(PurchaseError::Conflict {
                found: StateTag::Paid,
                ..
            })
        ),
        "the live paid purchase must not be closable by the key-only verb: {refused:?}"
    );
}
