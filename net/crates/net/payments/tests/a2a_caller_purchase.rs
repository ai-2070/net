//! The caller half of paid A2A admission
//! (`docs/internal/plans/A2A_PAID_ADMISSION_PLAN.md` §D4): one
//! authoritative purchase attempt per intent key, resumed rather than
//! re-quoted.
//!
//! Every test here runs the **real** `PaymentEngine` over the mock
//! facilitator behind a scripted `ProviderChannel`, so the idempotency
//! these witnesses lean on (the engine's consumed-payload index) is the
//! production one, not a stub's imitation. The scripted channel adds
//! exactly three powers a wire cannot be asked for on demand: it counts
//! what crossed it, it can swallow or downgrade a reply the engine
//! already processed (a lost reply is not a lost payment), and it can
//! park a call on a gate so a concurrency claim is staged rather than
//! slept on.
//!
//! The A2A provider verbs are scripted the same way: `ScriptedTasks`
//! stands in for the configured serving path's prepare/submit, minting
//! reservations exactly as the provider's journal does (its own
//! commitment, its own `admission_id`, the purchase hash those imply).
//!
//! `run_is_byte_for_byte_equivalent_after_the_split` from the plan's
//! WS-B list is deliberately **not** a test in this file: it is
//! discharged by the pre-existing guard suites — `flow_end_to_end`,
//! `mcp_gate_composition`, `lifecycle_modes`, `spend_policy`, and
//! `http402_outbound`'s `a_solana_reject_keeps_the_reservation` — staying
//! green **unedited**. A test asserting "`run` still works" written
//! beside those would be a tautology, not a witness.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use net::adapter::net::identity::EntityKeypair;
use net_payments::billing::BillingLog;
use net_payments::core::canonical::canonical_bytes;
use net_payments::core::quote::PaymentQuote;
use net_payments::core::registry::{default_mock_registry, default_registry_v1, AssetRegistry};
use net_payments::core::terms::PricingTerms;
use net_payments::core::units::AtomicAmount;
use net_payments::engine::{AdmitAll, PaymentEngine, RedeemDecision};
use net_payments::facilitator::mock::{MockFacilitator, MOCK_NETWORK, MOCK_SCHEME};
use net_payments::flow::a2a::{
    A2aCallerFlow, A2aPrepareError, A2aProviderChannel, A2aPurchase, A2aPurchaseFile,
    A2aPurchaseStore, A2aSubmit, AttemptResolution, PurchaseAttempt, PurchaseError, PurchaseKey,
    PurchaseState, StateTag, PREPARE_LEASE_NS,
};
use net_payments::flow::mesh::EngineTaskAdmissionGate;
use net_payments::flow::signer::ExternalSvmSigner;
use net_payments::flow::{ChannelError, Clock, InProcessProvider, PayResponse, ProviderChannel};
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
/// The provider node every reservation in this file lives on.
const NODE: u64 = 7;
const SERVICE: &str = "summarize";
const REVISION: &str = "r1";
/// `"{node}/net.a2a.task/{service}"` — the capability the configured A2A
/// path quotes under.
const CAPABILITY: &str = "7/net.a2a.task/summarize";
const AMOUNT: u128 = 2_500;

const SOLANA_MAINNET: &str = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp";
const SPL_USDC: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
const SVM_PAY_TO: &str = "9xQeWvG816bUx9EPjHmaT23yvVM2ZWbrrpZb9PusVFin";
const SVM_AMOUNT: u128 = 10_000;
const SVM_FEE_PAYER: &str = "FaciLitator111111111111111111111111111111111";

/// Env switch + payload for the cross-process probe.
const CHILD_ENV: &str = "NET_A2A_PURCHASE_CHILD";
const CHILD_STORE_ENV: &str = "NET_A2A_PURCHASE_CHILD_STORE";
const CHILD_CALLER_ENV: &str = "NET_A2A_PURCHASE_CHILD_CALLER";
const CHILD_TASK_ENV: &str = "NET_A2A_PURCHASE_CHILD_TASK";
const CHILD_COMMITMENT_ENV: &str = "NET_A2A_PURCHASE_CHILD_COMMITMENT";
const CHILD_QUOTE_ENV: &str = "NET_A2A_PURCHASE_CHILD_QUOTE";
const CHILD_NOW_ENV: &str = "NET_A2A_PURCHASE_CHILD_NOW";
const EXIT_CONVERGED: i32 = 21;
const EXIT_WRONG: i32 = 22;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A clock the test moves on purpose. Never auto-advancing: quote expiry
/// and lease staleness are the subject of several witnesses here, and a
/// clock that drifts on its own would make them about luck.
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
/// the check and the wait cannot leave a caller parked forever — the
/// same discipline the flow's own waiters use. No sleeps anywhere.
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

    /// Park until at least `n` calls have entered the gate. The only
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

/// The provider's prepare/submit half, scripted.
struct ScriptedTasks {
    offer: A2aOffer,
    /// Minted once per task id and reused, exactly as an idempotent
    /// provider prepare does — which is what makes "re-prepare keeps the
    /// same admission id" observable.
    admissions: parking_lot::Mutex<std::collections::HashMap<String, String>>,
    prepare_gate: Gate,
    prepare_calls: AtomicUsize,
    submit_calls: AtomicUsize,
    submit_replies: parking_lot::Mutex<std::collections::VecDeque<Result<TaskAck, A2aFlowError>>>,
}

impl ScriptedTasks {
    fn new(offer: A2aOffer) -> Self {
        Self {
            offer,
            admissions: parking_lot::Mutex::new(std::collections::HashMap::new()),
            prepare_gate: Gate::wide_open(),
            prepare_calls: AtomicUsize::new(0),
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
        self.prepare_calls.fetch_add(1, Ordering::SeqCst);
        self.prepare_gate.pass().await;
        // The provider computes the commitment from its OWN offer copy
        // and mints the admission id; the purchase hash is what those
        // two imply. Nothing here is read off the request.
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
            !proof.binding_sig.is_empty(),
            "a paid submission must carry the caller's binding signature"
        );
        assert!(
            !proof.quote_id.is_empty(),
            "a paid submission must name the quote that paid"
        );
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

/// What the scripted payment channel does with the next pay call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PayMode {
    /// Straight through to the engine.
    Forward,
    /// The engine processes it — the money moves — and the reply is lost
    /// on the way back. The caller learns nothing.
    SwallowOnce,
    /// The engine processes it and the provider answers "settled, but
    /// confidence pending".
    PendingOnce,
    /// The provider claims a rejection while holding the authorization
    /// it was handed. The engine is never told.
    RejectAlways,
}

/// The payment channel, scripted: counts, fault injection, and a gate.
struct ScriptedChannel {
    inner: InProcessProvider,
    quote_calls: AtomicUsize,
    /// Every payload that crossed, **with multiplicity** — a set could
    /// not tell one send from three.
    payloads: parking_lot::Mutex<Vec<Vec<u8>>>,
    pay_gate: Gate,
    mode: parking_lot::Mutex<PayMode>,
}

impl ScriptedChannel {
    fn new(inner: InProcessProvider) -> Self {
        Self {
            inner,
            quote_calls: AtomicUsize::new(0),
            payloads: parking_lot::Mutex::new(Vec::new()),
            pay_gate: Gate::wide_open(),
            mode: parking_lot::Mutex::new(PayMode::Forward),
        }
    }

    fn quote_calls(&self) -> usize {
        self.quote_calls.load(Ordering::SeqCst)
    }

    fn pay_sends(&self) -> usize {
        self.payloads.lock().len()
    }

    fn distinct_payloads(&self) -> usize {
        let mut seen: Vec<Vec<u8>> = self.payloads.lock().clone();
        seen.sort();
        seen.dedup();
        seen.len()
    }

    fn set_mode(&self, mode: PayMode) {
        *self.mode.lock() = mode;
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
        self.pay_gate.pass().await;
        let mode = *self.mode.lock();
        match mode {
            PayMode::Forward => self.inner.pay(quote_bytes, payload).await,
            PayMode::SwallowOnce => {
                let _landed = self.inner.pay(quote_bytes, payload).await;
                self.set_mode(PayMode::Forward);
                Err(ChannelError {
                    message: "the pay reply was lost in transit".to_string(),
                    retryable: true,
                })
            }
            PayMode::PendingOnce => {
                let _landed = self.inner.pay(quote_bytes, payload).await;
                self.set_mode(PayMode::Forward);
                Ok(PayResponse::PendingTier {
                    reached: "observed".to_string(),
                    required: "confirmed(3)".to_string(),
                })
            }
            PayMode::RejectAlways => Ok(PayResponse::Rejected {
                reason: "provider says no".to_string(),
            }),
        }
    }
}

/// Settles the exact-SVM scheme the mock facilitator does not speak, so
/// the real-network world can complete a purchase as well as refuse one.
/// Mirrors `exact_svm_scheme_flow.rs`'s stub — same pinned payload shape
/// across the boundary.
struct SvmFacilitator;

#[async_trait::async_trait]
impl net_payments::facilitator::Facilitator for SvmFacilitator {
    fn reference(&self) -> net_payments::core::verification::VerifierRef {
        net_payments::core::verification::VerifierRef {
            identity: None,
            endpoint: "test-exact-svm".into(),
        }
    }

    async fn verify(
        &self,
        _payload: &X402Carry<PaymentPayload>,
        requirements: &X402Carry<PaymentRequirements>,
    ) -> Result<net_payments::facilitator::VerifyOutcome, net_payments::facilitator::FacilitatorError>
    {
        assert_eq!(requirements.view().scheme, "exact");
        Ok(net_payments::facilitator::VerifyOutcome {
            response: X402Carry::author(&net_payments::x402::settlement::VerifyResponse {
                is_valid: true,
                invalid_reason: None,
                payer: None,
                extra: None,
            })
            .expect("verify response"),
        })
    }

    async fn settle(
        &self,
        _payload: &X402Carry<PaymentPayload>,
        requirements: &X402Carry<PaymentRequirements>,
    ) -> Result<net_payments::facilitator::SettleOutcome, net_payments::facilitator::FacilitatorError>
    {
        Ok(net_payments::facilitator::SettleOutcome {
            response: X402Carry::author(&net_payments::x402::settlement::SettlementResponse {
                success: true,
                error_reason: None,
                payer: None,
                transaction: "5VERYrealSVMsignature1111111111111111111111".into(),
                network: requirements.view().network.clone(),
                amount: Some(requirements.view().amount.clone()),
                extensions: None,
            })
            .expect("settlement response"),
        })
    }
}

struct World {
    flow: A2aCallerFlow,
    payments: Arc<net_payments::flow::CallerPaymentFlow>,
    tasks: Arc<ScriptedTasks>,
    channel: Arc<ScriptedChannel>,
    store: Arc<A2aPurchaseStore>,
    store_path: std::path::PathBuf,
    spend: SpendPolicyEngine,
    spend_path: std::path::PathBuf,
    billing: Arc<BillingLog>,
    clock: Arc<TestClock>,
    caller: Arc<EntityKeypair>,
    engine: Arc<PaymentEngine>,
    offer: A2aOffer,
    network: String,
    asset: String,
    _dir: tempfile::TempDir,
}

impl World {
    fn caller_hex(&self) -> String {
        hex::encode(self.caller.entity_id().as_bytes())
    }

    fn key(&self, task_id: &str) -> PurchaseKey {
        PurchaseKey::new(self.caller.entity_id(), NODE, task_id)
    }

    async fn attempt(&self, task_id: &str) -> PurchaseAttempt {
        self.flow
            .stored_attempt(NODE, task_id)
            .await
            .expect("store read")
            .unwrap_or_else(|| panic!("no attempt for {task_id}"))
    }

    async fn state(&self, task_id: &str) -> StateTag {
        self.attempt(task_id).await.state.tag()
    }

    async fn billed(&self) -> usize {
        self.billing.read_all().await.expect("read billing").len()
    }

    async fn reserved_today(&self) -> u128 {
        self.spend
            .spent_today(&self.network, &self.asset, self.clock.now_ns())
            .await
            .expect("spent today")
            .to_canonical_string()
            .parse()
            .expect("amount")
    }

    /// A second `A2aCallerFlow` over the *same* store, spend policy and
    /// provider — a caller restart, with nothing but the files carried
    /// across.
    fn restart(&self) -> A2aCallerFlow {
        let payments = Arc::new(net_payments::flow::CallerPaymentFlow::new(
            self.caller.clone(),
            SpendPolicyEngine::new(&self.spend_path, SpendProfile::DevTest),
            default_mock_registry(self.engine.provider_id().clone()),
            self.channel.clone(),
            self.clock.clone(),
        ));
        A2aCallerFlow::new(
            payments,
            self.tasks.clone(),
            Arc::new(A2aPurchaseStore::new(&self.store_path)),
            self.clock.clone(),
        )
    }
}

fn bounds() -> A2aBounds {
    A2aBounds {
        max_prompt_bytes: 4096,
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

fn svm_terms(provider: &EntityKeypair, registry: &AssetRegistry) -> String {
    let template = X402Carry::author(&PaymentRequirements {
        scheme: "exact".into(),
        network: SOLANA_MAINNET.into(),
        amount: SVM_AMOUNT.to_string(),
        asset: SPL_USDC.into(),
        pay_to: SVM_PAY_TO.into(),
        max_timeout_seconds: 60,
        // The SPL transfer intent the wallet is shown needs a fee payer,
        // exactly as the solana pack's announced terms carry one.
        extra: Some(serde_json::json!({ "feePayer": SVM_FEE_PAYER })),
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

async fn world() -> World {
    build_world(SpendProfile::DevTest, false).await
}

async fn world_requiring_approval() -> World {
    build_world(SpendProfile::Production, false).await
}

/// The exact-SVM world: a real network, a wallet that authors a
/// self-contained transfer authorization, and therefore a provider whose
/// claimed rejection is not proof of non-settlement.
async fn svm_world() -> World {
    build_world(SpendProfile::Production, true).await
}

async fn build_world(profile: SpendProfile, svm: bool) -> World {
    let dir = tempfile::tempdir().expect("tempdir");
    let clock = Arc::new(TestClock::new());
    let provider_keys = Arc::new(EntityKeypair::generate());
    let registry = if svm {
        default_registry_v1(provider_keys.entity_id().clone())
    } else {
        default_mock_registry(provider_keys.entity_id().clone())
    };
    let billing = Arc::new(BillingLog::new(dir.path().join("billing.jsonl")));
    let engine = Arc::new(
        PaymentEngine::new(
            provider_keys.clone(),
            if svm {
                Arc::new(SvmFacilitator) as Arc<dyn net_payments::facilitator::Facilitator>
            } else {
                Arc::new(MockFacilitator::new())
            },
            Arc::new(AdmitAll),
            registry.clone(),
            dir.path().join("engine.json"),
        )
        .expect("engine")
        .with_billing_log(billing.clone()),
    );
    let channel = Arc::new(ScriptedChannel::new(InProcessProvider::new(
        engine.clone(),
        clock.clone(),
    )));

    let spend_path = dir.path().join("payment-policy.json");
    if svm {
        SpendPolicyEngine::new(&spend_path, profile)
            .configure(|defaults, _| {
                defaults.allowed_networks = vec![SOLANA_MAINNET.to_string()];
                defaults.max_per_call = Some(AtomicAmount::from_u128(50_000));
            })
            .await
            .expect("configure");
    }

    let terms = if svm {
        svm_terms(&provider_keys, &registry)
    } else {
        mock_terms(&provider_keys, &registry)
    };
    let offer = A2aOffer {
        service_id: SERVICE.to_string(),
        revision: REVISION.to_string(),
        description: None,
        pricing_terms: Some(terms),
        bounds: bounds(),
        reservation_ttl_secs: 600,
        reservation_retention_secs: 604_800,
        retention_secs: 3_600,
    };

    let caller = Arc::new(EntityKeypair::generate());
    let mut payments = net_payments::flow::CallerPaymentFlow::new(
        caller.clone(),
        SpendPolicyEngine::new(&spend_path, profile),
        registry,
        channel.clone(),
        clock.clone(),
    );
    if svm {
        payments = payments.with_signer(
            "solana",
            Arc::new(ExternalSvmSigner::new(SVM_PAY_TO, move |_intent| {
                Box::pin(async move { Ok("cGFydGlhbGx5LXNpZ25lZC1zdm0=".to_string()) })
            })),
        );
    }
    let payments = Arc::new(payments);
    let tasks = Arc::new(ScriptedTasks::new(offer.clone()));
    let store_path = dir.path().join("a2a-purchases.json");
    let store = Arc::new(A2aPurchaseStore::new(&store_path));
    let flow = A2aCallerFlow::new(
        payments.clone(),
        tasks.clone(),
        store.clone(),
        clock.clone(),
    );

    World {
        flow,
        payments,
        tasks,
        channel,
        store,
        store_path,
        spend: SpendPolicyEngine::new(&spend_path, profile),
        spend_path,
        billing,
        clock,
        caller,
        engine,
        offer,
        network: if svm {
            SOLANA_MAINNET.to_string()
        } else {
            MOCK_NETWORK.to_string()
        },
        asset: if svm {
            SPL_USDC.to_string()
        } else {
            "musd".to_string()
        },
        _dir: dir,
    }
}

/// Seed one attempt in whatever state a table-driven test needs, through
/// the same locked store helpers production uses.
async fn seed(world: &World, task_id: &str, state: PurchaseState) {
    let key = world.key(task_id);
    let attempt = PurchaseAttempt {
        key: key.clone(),
        commitment: task_commitment(&world.offer, &brief(task_id)),
        prepared: None,
        quote_bytes: None,
        quote_id: None,
        quote_expires_at_ns: None,
        payload_bytes: None,
        state,
        updated_at_ns: world.clock.now_ns(),
    };
    mutate_json::<A2aPurchaseFile, _, _>(&world.store_path, move |file| {
        file.attempts.insert(key.id(), attempt);
    })
    .await
    .expect("seed the attempt");
}

fn sample_state(tag: StateTag) -> PurchaseState {
    let proof = TaskPaymentProof {
        quote_id: "q-seed".to_string(),
        binding_sig: vec![7u8; 64],
    };
    match tag {
        StateTag::Preparing => PurchaseState::Preparing {
            lease_id: "lease-seed".to_string(),
            since_ns: NOW,
        },
        StateTag::Quoted => PurchaseState::Quoted,
        StateTag::AwaitingApproval => PurchaseState::AwaitingApproval,
        StateTag::Paying => PurchaseState::Paying {
            lease_id: "lease-seed".to_string(),
            since_ns: NOW,
        },
        StateTag::Paid => PurchaseState::Paid {
            proof,
            billing: serde_json::Value::Null,
        },
        StateTag::Unknown => PurchaseState::Unknown {
            last_error: "lost".to_string(),
        },
        StateTag::RefusedUnexposed => PurchaseState::RefusedUnexposed {
            reason: "denied".to_string(),
        },
        StateTag::RefusedExposed => PurchaseState::RefusedExposed {
            reason: "rejected".to_string(),
            reservation_kept: true,
        },
        StateTag::Submitted => PurchaseState::Submitted {
            task_id: "t-seed".to_string(),
        },
        StateTag::PaidUnexecutable => PurchaseState::PaidUnexecutable {
            proof,
            billing: serde_json::Value::Null,
            refusal: net_payments::flow::a2a::RefusalRecord {
                at_ns: NOW,
                message: "no reservation".to_string(),
                reason: Some("no_reservation".to_string()),
                safe_to_retry: false,
                safe_to_requote: false,
            },
        },
        StateTag::Resolved => PurchaseState::Resolved {
            outcome: "written off".to_string(),
            evidence: serde_json::Value::Null,
        },
    }
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

// ---------------------------------------------------------------------------
// Sequential recovery
// ---------------------------------------------------------------------------

/// Prepare is read-only on the money side: a price can be displayed
/// without spending anything.
#[tokio::test]
async fn prepare_task_moves_no_money_and_reserves_no_spend() {
    let w = world().await;
    let prepared = w
        .flow
        .prepare_task(NODE, &w.offer, &brief("t-quote-only"))
        .await
        .expect("prepare");

    // The quote exists and is bound to THIS reservation.
    assert_eq!(w.channel.quote_calls(), 1, "exactly one quote was issued");
    let attempt = w.attempt("t-quote-only").await;
    assert_eq!(attempt.state.tag(), StateTag::Quoted);
    let quote = PaymentQuote::from_json_bytes(&attempt.quote_bytes.expect("quote bytes"))
        .expect("stored quote verifies");
    assert_eq!(
        quote.input_hash.as_deref(),
        Some(prepared.reservation.purchase_hash.as_str()),
        "the quote commits to the reservation's purchase hash"
    );

    // And nothing financial happened: no payload, no spend reservation,
    // no billing event.
    assert_eq!(w.channel.pay_sends(), 0, "no payment was delivered");
    assert_eq!(w.reserved_today().await, 0, "no budget was reserved");
    assert_eq!(w.billed().await, 0, "nothing was billed");
    assert!(
        !w.payments
            .spend_reservation_held(&quote.quote_id)
            .await
            .expect("reservation read"),
        "prepare must not hold a spend reservation"
    );
}

/// The purchase spends the quote prepare obtained — it does not go and
/// get a fresh one.
#[tokio::test]
async fn purchase_consumes_the_prepared_quote_not_a_fresh_one() {
    let w = world().await;
    let prepared = w
        .flow
        .prepare_task(NODE, &w.offer, &brief("t-consume"))
        .await
        .expect("prepare");
    let quoted = w.attempt("t-consume").await.quote_id.expect("quote id");

    let outcome = w.flow.purchase_task(NODE, "t-consume").await;
    let A2aPurchase::Paid { proof, .. } = &outcome else {
        panic!("expected Paid, got {outcome:?}");
    };
    assert_eq!(
        proof.quote_id, quoted,
        "the proof is for the prepared quote"
    );
    assert_eq!(
        w.channel.quote_calls(),
        1,
        "the purchase must not request a second quote"
    );
    assert_eq!(
        w.channel.pay_sends(),
        1,
        "exactly one payment was delivered"
    );
    assert_eq!(w.billed().await, 1, "exactly one billing event");
    assert_eq!(w.state("t-consume").await, StateTag::Paid);
    assert_eq!(
        w.reserved_today().await,
        AMOUNT,
        "the spend reservation holds the quoted amount"
    );

    // The binding the provider will verify is over this quote and this
    // task's tool id — the engine agrees, which is the whole point of
    // the purchase hash.
    let gate = EngineTaskAdmissionGate::new(w.engine.clone());
    let evidence = gate
        .redeem(TaskPaymentClaim {
            tool_id: "net.a2a.task/summarize",
            quote_id: &proof.quote_id,
            binding: &proof.binding_sig,
            expected_input_hash: &prepared.reservation.purchase_hash,
        })
        .await
        .expect("the provider gate admits the purchase it was paid for");
    assert_eq!(evidence.quote_id, proof.quote_id);
    assert_eq!(
        evidence.payer,
        *w.caller.entity_id().as_bytes(),
        "the evidence names the paying entity"
    );
}

/// A lost pay reply is recovered by re-sending the **identical** payload,
/// never by re-quoting. The engine resolves the duplicate to the original
/// verdict, so the caller ends up paid, once.
#[tokio::test]
async fn a_lost_pay_reply_is_resumed_by_resending_the_identical_payload() {
    let w = world().await;
    w.flow
        .prepare_task(NODE, &w.offer, &brief("t-lost"))
        .await
        .expect("prepare");

    // The engine processes the payment; the reply never arrives.
    w.channel.set_mode(PayMode::SwallowOnce);
    let first = w.flow.purchase_task(NODE, "t-lost").await;
    assert!(
        matches!(first, A2aPurchase::Unknown { .. }),
        "a lost reply is ambiguous, not a failure: {first:?}"
    );
    assert_eq!(w.state("t-lost").await, StateTag::Unknown);
    assert_eq!(w.billed().await, 1, "the payment did land");

    // The resume: same quote, same payload, byte for byte.
    let second = w.flow.purchase_task(NODE, "t-lost").await;
    let A2aPurchase::Paid { proof, billing, .. } = &second else {
        panic!("expected Paid on resume, got {second:?}");
    };
    assert_eq!(w.state("t-lost").await, StateTag::Paid);
    assert_eq!(
        w.channel.pay_sends(),
        2,
        "the payload was delivered twice — the resume really re-sent it"
    );
    assert_eq!(
        w.channel.distinct_payloads(),
        1,
        "and both deliveries were the same bytes"
    );
    assert_eq!(
        w.channel.quote_calls(),
        1,
        "the resume must never mint a second quote"
    );
    assert_eq!(w.billed().await, 1, "exactly one charge for one purchase");
    assert_eq!(
        w.reserved_today().await,
        AMOUNT,
        "one purchase, one reservation against the day budget"
    );
    assert!(
        billing["billing_event"].is_string(),
        "the recovered proof carries the provider's signed billing event"
    );
    assert!(!proof.binding_sig.is_empty());
}

/// A settlement the provider will not yet vouch for leaves the attempt
/// `Unknown` — and the same stored payment resolves it once confidence
/// arrives.
#[tokio::test]
async fn a_pending_settlement_purchase_stays_unknown_then_resolves_to_paid() {
    let w = world().await;
    w.flow
        .prepare_task(NODE, &w.offer, &brief("t-pending"))
        .await
        .expect("prepare");

    w.channel.set_mode(PayMode::PendingOnce);
    let first = w.flow.purchase_task(NODE, "t-pending").await;
    assert!(
        matches!(first, A2aPurchase::Unknown { .. }),
        "pending confidence is ambiguous: {first:?}"
    );
    assert_eq!(w.state("t-pending").await, StateTag::Unknown);

    let second = w.flow.purchase_task(NODE, "t-pending").await;
    assert!(
        matches!(second, A2aPurchase::Paid { .. }),
        "the stored payment resolves once the provider serves: {second:?}"
    );
    assert_eq!(w.state("t-pending").await, StateTag::Paid);
    assert_eq!(w.channel.distinct_payloads(), 1);
    assert_eq!(w.channel.pay_sends(), 2);
    assert_eq!(w.channel.quote_calls(), 1);
    assert_eq!(w.billed().await, 1);
}

/// A caller that dies mid-purchase resumes from the store: the reopened
/// flow finds the claimed attempt and sends the payload it left behind.
#[tokio::test]
async fn a_caller_restart_mid_purchase_resumes_the_stored_attempt() {
    let w = world().await;
    w.flow
        .prepare_task(NODE, &w.offer, &brief("t-restart"))
        .await
        .expect("prepare");
    w.channel.set_mode(PayMode::SwallowOnce);
    let _ambiguous = w.flow.purchase_task(NODE, "t-restart").await;
    let before = w.attempt("t-restart").await;
    assert_eq!(before.state.tag(), StateTag::Unknown);
    let stored_payload = before.payload_bytes.clone().expect("payload persisted");

    // Nothing but the files survive: a brand new flow over the same
    // store path, the same spend policy, the same provider.
    let reopened = w.restart();
    let resumed = reopened.purchase_task(NODE, "t-restart").await;
    let A2aPurchase::Paid { proof, .. } = &resumed else {
        panic!("expected the restarted caller to resume to Paid, got {resumed:?}");
    };
    assert_eq!(
        proof.quote_id,
        before.quote_id.clone().expect("quote id"),
        "the restarted caller paid with the stored quote"
    );
    assert_eq!(
        w.channel.payloads.lock().last().expect("a second send"),
        &stored_payload,
        "the restarted caller re-sent the stored payload verbatim"
    );
    assert_eq!(w.channel.distinct_payloads(), 1);
    assert_eq!(w.channel.quote_calls(), 1, "no re-quote across the restart");
    assert_eq!(w.billed().await, 1);
}

/// While an attempt is unresolved, no number of retries produces a second
/// quote. The quote channel's count is the oracle.
#[tokio::test]
async fn purchase_never_requotes_while_an_attempt_is_unresolved() {
    let w = world().await;
    w.flow
        .prepare_task(NODE, &w.offer, &brief("t-noreq"))
        .await
        .expect("prepare");
    w.channel.set_mode(PayMode::SwallowOnce);
    assert!(matches!(
        w.flow.purchase_task(NODE, "t-noreq").await,
        A2aPurchase::Unknown { .. }
    ));

    // Re-preparing an unresolved attempt is not a re-quote either: it
    // returns the stored reservation.
    for _ in 0..3 {
        let prepared = w
            .flow
            .prepare_task(NODE, &w.offer, &brief("t-noreq"))
            .await
            .expect("re-prepare returns the stored reservation");
        assert_eq!(prepared.brief.task_id, "t-noreq");
        assert_eq!(
            w.channel.quote_calls(),
            1,
            "the quote channel was called again while an attempt was unresolved"
        );
    }
    // And the purchase resumes through the stored payment.
    assert!(matches!(
        w.flow.purchase_task(NODE, "t-noreq").await,
        A2aPurchase::Paid { .. }
    ));
    assert_eq!(w.channel.quote_calls(), 1);
    assert_eq!(w.billed().await, 1);
}

/// An unpaid quote that expired is replaced only through `prepare_task`
/// — and the provider's admission id survives, so the new quote buys the
/// same reservation.
#[tokio::test]
async fn an_expired_unpaid_quote_is_requoted_only_through_prepare_with_the_same_admission_id() {
    let w = world().await;
    let first = w
        .flow
        .prepare_task(NODE, &w.offer, &brief("t-expiry"))
        .await
        .expect("prepare");
    let first_quote = w.attempt("t-expiry").await.quote_id.expect("quote id");

    // Past the quote's authoritative expiry, nothing has been exposed.
    w.clock.advance(120_000_000_000);

    // Purchasing does not re-quote; it is the prepare verb that does.
    let purchase = w.flow.purchase_task(NODE, "t-expiry").await;
    assert!(
        matches!(
            purchase,
            A2aPurchase::Denied { .. } | A2aPurchase::Failed { .. }
        ),
        "an expired quote cannot be paid: {purchase:?}"
    );
    assert_eq!(
        w.channel.quote_calls(),
        1,
        "purchase_task must never mint a quote"
    );

    let second = w
        .flow
        .prepare_task(NODE, &w.offer, &brief("t-expiry"))
        .await
        .expect("re-prepare");
    assert_eq!(
        w.channel.quote_calls(),
        2,
        "prepare_task mints the replacement quote"
    );
    let second_quote = w.attempt("t-expiry").await.quote_id.expect("quote id");
    assert_ne!(first_quote, second_quote, "it is a different quote");
    assert_eq!(
        second.reservation.admission_id, first.reservation.admission_id,
        "under the same provider-minted admission id"
    );
    assert_eq!(
        second.reservation.purchase_hash, first.reservation.purchase_hash,
        "so the purchase hash is unchanged"
    );
    assert!(matches!(
        w.flow.purchase_task(NODE, "t-expiry").await,
        A2aPurchase::Paid { .. }
    ));
    assert_eq!(w.billed().await, 1);
}

// ---------------------------------------------------------------------------
// Atomic attempt selection (finding r3-4)
// ---------------------------------------------------------------------------

/// Concurrent prepares converge on one attempt and one quote.
///
/// Staged, not slept: the first caller is parked **inside** the provider's
/// prepare, so the second caller provably meets a live `Preparing` lease
/// (asserted from the store before it is released) and has to await the
/// outcome rather than quote for itself.
#[tokio::test]
async fn concurrent_prepares_converge_on_one_quote() {
    let w = Arc::new(world().await);
    w.tasks.prepare_gate.open.store(false, Ordering::SeqCst);

    let first = {
        let w = w.clone();
        tokio::spawn(async move { w.flow.prepare_task(NODE, &w.offer, &brief("t-race")).await })
    };
    // The lease is taken and the provider call is in flight.
    w.tasks.prepare_gate.await_entered(1).await;
    assert_eq!(
        w.attempt("t-race").await.state.tag(),
        StateTag::Preparing,
        "the first caller's lease is on file while it is inside prepare"
    );

    let mut racers = Vec::new();
    for _ in 0..7 {
        let w = w.clone();
        racers.push(tokio::spawn(async move {
            w.flow.prepare_task(NODE, &w.offer, &brief("t-race")).await
        }));
    }
    w.tasks.prepare_gate.release();

    let leader = first.await.expect("join").expect("prepare");
    let mut ids = vec![leader.reservation.admission_id.clone()];
    for racer in racers {
        let prepared = racer.await.expect("join").expect("racer prepare");
        ids.push(prepared.reservation.admission_id.clone());
    }

    assert_eq!(
        w.channel.quote_calls(),
        1,
        "eight concurrent prepares must produce exactly one quote"
    );
    assert_eq!(
        w.tasks.prepare_calls.load(Ordering::SeqCst),
        1,
        "and exactly one provider prepare"
    );
    let quote_id = w.attempt("t-race").await.quote_id.expect("quote id");
    assert!(!quote_id.is_empty());
    assert!(
        ids.iter().all(|id| *id == ids[0]),
        "every caller received the same reservation: {ids:?}"
    );
    let file: A2aPurchaseFile =
        serde_json::from_slice(&tokio::fs::read(&w.store_path).await.expect("read store"))
            .expect("parse store");
    assert_eq!(file.attempts.len(), 1, "one authoritative attempt on file");
}

/// Concurrent purchases send one payload, incur one charge, and every
/// caller gets the same proof.
///
/// Staged: the winner is parked inside `pay` with its payload already
/// persisted, so the racers meet a live `Paying` claim (asserted from the
/// store) — they converge on it instead of authoring a second
/// authorization.
#[tokio::test]
async fn concurrent_purchases_send_one_payload() {
    let w = Arc::new(world().await);
    w.flow
        .prepare_task(NODE, &w.offer, &brief("t-pay-race"))
        .await
        .expect("prepare");
    w.channel.pay_gate.open.store(false, Ordering::SeqCst);

    let winner = {
        let w = w.clone();
        tokio::spawn(async move { w.flow.purchase_task(NODE, "t-pay-race").await })
    };
    w.channel.pay_gate.await_entered(1).await;
    let claimed = w.attempt("t-pay-race").await;
    assert_eq!(
        claimed.state.tag(),
        StateTag::Paying,
        "the claim is durable before the payment goes out"
    );
    assert!(
        claimed.payload_bytes.is_some(),
        "and so is the exact payload it sent"
    );

    let mut racers = Vec::new();
    for _ in 0..7 {
        let w = w.clone();
        racers.push(tokio::spawn(async move {
            w.flow.purchase_task(NODE, "t-pay-race").await
        }));
    }
    w.channel.pay_gate.release();

    let lead = winner.await.expect("join");
    let A2aPurchase::Paid {
        proof: lead_proof,
        billing: lead_billing,
        ..
    } = &lead
    else {
        panic!("expected the winner to be Paid, got {lead:?}");
    };
    for racer in racers {
        let outcome = racer.await.expect("join");
        let A2aPurchase::Paid { proof, billing, .. } = &outcome else {
            panic!("every concurrent purchase must resolve Paid, got {outcome:?}");
        };
        assert_eq!(proof, lead_proof, "same proof");
        assert_eq!(billing, lead_billing, "same billing evidence");
    }

    assert!(
        w.channel.pay_sends() >= 1,
        "the payment really was delivered"
    );
    assert_eq!(
        w.channel.distinct_payloads(),
        1,
        "eight concurrent purchases authored exactly one payload"
    );
    assert_eq!(w.billed().await, 1, "and were charged exactly once");
    assert_eq!(
        w.reserved_today().await,
        AMOUNT,
        "one purchase, one reservation against the day budget"
    );
    assert_eq!(w.channel.quote_calls(), 1);
    assert_eq!(w.state("t-pay-race").await, StateTag::Paid);
}

/// Two different pieces of work cannot share one intent key.
#[tokio::test]
async fn a_conflicting_commitment_under_the_same_key_is_rejected() {
    let w = world().await;
    w.flow
        .prepare_task(NODE, &w.offer, &brief("t-conflict"))
        .await
        .expect("prepare");

    // Same task id, different prompt ⇒ different commitment.
    let altered = TaskBrief::new("something else entirely")
        .with_task_id("t-conflict")
        .with_service(SERVICE, REVISION);
    let refused = w
        .flow
        .prepare_task(NODE, &w.offer, &altered)
        .await
        .expect_err("a different brief under the same id must be refused");
    assert!(
        matches!(
            refused,
            A2aPrepareError::Attempt(PurchaseError::CommitmentConflict { .. })
        ),
        "expected a commitment conflict, got {refused:?}"
    );
    assert_eq!(
        w.channel.quote_calls(),
        1,
        "the refused brief must not have been quoted"
    );
    assert_eq!(
        w.attempt("t-conflict").await.commitment,
        task_commitment(&w.offer, &brief("t-conflict")),
        "the original attempt is untouched"
    );
}

/// A stale prepare lease is taken over; a live one is not.
#[tokio::test]
async fn a_stale_preparing_lease_is_taken_over_and_a_live_one_is_not() {
    let w = world().await;
    let key = w.key("t-lease");
    seed(
        &w,
        "t-lease",
        PurchaseState::Preparing {
            lease_id: "someone-elses-lease".to_string(),
            since_ns: w.clock.now_ns(),
        },
    )
    .await;

    // Live lease: refused, and nothing was quoted.
    let refused = w
        .flow
        .prepare_task(NODE, &w.offer, &brief("t-lease"))
        .await
        .expect_err("a live lease must not be stolen");
    assert!(
        matches!(refused, A2aPrepareError::InFlight { .. }),
        "expected InFlight, got {refused:?}"
    );
    assert_eq!(w.channel.quote_calls(), 0);
    assert_eq!(
        w.store
            .attempt(&key)
            .await
            .expect("read")
            .expect("attempt")
            .state
            .lease()
            .map(str::to_string),
        Some("someone-elses-lease".to_string()),
        "the incumbent's lease is still on file"
    );

    // Past the horizon: taken over, and the prepare completes.
    w.clock.advance(PREPARE_LEASE_NS + 1);
    let prepared = w
        .flow
        .prepare_task(NODE, &w.offer, &brief("t-lease"))
        .await
        .expect("a stale lease is taken over");
    assert_eq!(prepared.brief.task_id, "t-lease");
    assert_eq!(w.channel.quote_calls(), 1);
    assert_eq!(w.state("t-lease").await, StateTag::Quoted);
}

/// Two **processes** share one attempt: the child converges on the
/// parent's stored quote and is refused a conflicting commitment, against
/// the same file.
#[tokio::test]
async fn two_processes_share_one_attempt() {
    if std::env::var(CHILD_ENV).is_ok() {
        child_probe().await;
    }
    let w = world().await;
    w.flow
        .prepare_task(NODE, &w.offer, &brief("t-cross"))
        .await
        .expect("prepare");
    let attempt = w.attempt("t-cross").await;
    let quote_id = attempt.quote_id.clone().expect("quote id");

    let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args([
            "--exact",
            "two_processes_share_one_attempt",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ENV, "1")
        .env(CHILD_STORE_ENV, &w.store_path)
        .env(CHILD_CALLER_ENV, w.caller_hex())
        .env(CHILD_TASK_ENV, "t-cross")
        .env(CHILD_COMMITMENT_ENV, &attempt.commitment)
        .env(CHILD_QUOTE_ENV, &quote_id)
        .env(CHILD_NOW_ENV, w.clock.now_ns().to_string())
        .output()
        .expect("spawn the second-process probe");
    let verdict = format!(
        "exit={:?} stdout={} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        out.status.code(),
        Some(EXIT_CONVERGED),
        "the second process must converge on the one stored attempt: {verdict}"
    );

    // And the parent's attempt is exactly as it was: one attempt, one
    // quote, no second lease.
    let after = w.attempt("t-cross").await;
    assert_eq!(after.state.tag(), StateTag::Quoted);
    assert_eq!(after.quote_id.as_deref(), Some(quote_id.as_str()));
    assert_eq!(w.channel.quote_calls(), 1);
    assert_eq!(w.flow.attempts().await.expect("attempts").len(), 1);
}

/// The child half of [`two_processes_share_one_attempt`]: a genuinely
/// separate process over the same store file.
async fn child_probe() -> ! {
    let path = std::env::var(CHILD_STORE_ENV).expect("child store path");
    let caller_hex = std::env::var(CHILD_CALLER_ENV).expect("child caller");
    let task_id = std::env::var(CHILD_TASK_ENV).expect("child task");
    let commitment = std::env::var(CHILD_COMMITMENT_ENV).expect("child commitment");
    let expected_quote = std::env::var(CHILD_QUOTE_ENV).expect("child quote");
    let store = A2aPurchaseStore::new(&path);
    let key = PurchaseKey {
        caller_hex,
        provider_node: NODE,
        task_id,
    };
    // The parent's instant, not the wall clock: whether the stored
    // quote is live is a fact about the caller's clock, and a child
    // reading its own would "discover" an expired quote and take the
    // lease over, which is the opposite of the claim.
    let now: u64 = std::env::var(CHILD_NOW_ENV)
        .expect("child clock")
        .parse()
        .expect("child clock parses");

    // A conflicting commitment under the same key is refused across the
    // process boundary too.
    match store
        .begin_prepare(&key, "a-different-commitment", now)
        .await
    {
        Err(PurchaseError::CommitmentConflict { .. }) => {}
        other => {
            eprintln!("child: conflicting commitment was not refused: {other:?}");
            std::process::exit(EXIT_WRONG);
        }
    }
    // And the matching one resolves to the incumbent's attempt — no
    // second lease, no second quote.
    match store.begin_prepare(&key, &commitment, now).await {
        Ok(net_payments::flow::a2a::PrepareClaim::Ready(attempt))
            if attempt.quote_id.as_deref() == Some(expected_quote.as_str()) =>
        {
            eprintln!("child: converged on quote {expected_quote}");
            std::process::exit(EXIT_CONVERGED);
        }
        other => {
            eprintln!("child: did not converge: {other:?}");
            std::process::exit(EXIT_WRONG);
        }
    }
}

// ---------------------------------------------------------------------------
// Refusal classes (finding r3-5)
// ---------------------------------------------------------------------------

/// A provider's claimed rejection **after** it was handed a
/// self-contained transfer authorization is not proof the money stayed
/// put. The reservation stands, the attempt is ambiguous, and nothing
/// re-quotes.
#[tokio::test]
async fn an_exposed_bearer_refusal_keeps_the_spend_reservation_and_marks_the_attempt_ambiguous() {
    let w = svm_world().await;
    w.flow
        .prepare_task(NODE, &w.offer, &brief("t-exposed"))
        .await
        .expect("prepare");
    w.channel.set_mode(PayMode::RejectAlways);

    let denied = w.flow.purchase_task(NODE, "t-exposed").await;
    let A2aPurchase::Denied {
        funds_ambiguous,
        quote_id,
        ..
    } = &denied
    else {
        panic!("expected Denied, got {denied:?}");
    };
    assert!(
        *funds_ambiguous,
        "an exposed bearer refusal is financially ambiguous"
    );
    assert_eq!(
        w.channel.pay_sends(),
        1,
        "the authorization really was exposed"
    );
    assert_eq!(
        w.reserved_today().await,
        SVM_AMOUNT,
        "the spend reservation is KEPT — the provider's claim is not proof"
    );
    let attempt = w.attempt("t-exposed").await;
    assert_eq!(
        attempt.state,
        PurchaseState::RefusedExposed {
            reason: "provider rejected the payment: provider says no".to_string(),
            reservation_kept: true,
        }
    );

    // Neither verb re-quotes or re-pays an ambiguous attempt.
    let again = w.flow.purchase_task(NODE, "t-exposed").await;
    assert_eq!(again, denied, "the ambiguous verdict is stable");
    assert_eq!(w.channel.quote_calls(), 1);
    assert_eq!(w.channel.pay_sends(), 1);
    let reprepare = w
        .flow
        .prepare_task(NODE, &w.offer, &brief("t-exposed"))
        .await
        .expect_err("an ambiguous attempt cannot be prepared again");
    assert!(
        matches!(
            reprepare,
            A2aPrepareError::Attempt(PurchaseError::Conflict {
                found: StateTag::RefusedExposed,
                ..
            })
        ),
        "expected a RefusedExposed conflict, got {reprepare:?}"
    );

    // The operator is the only exit, and the evidence survives it.
    let resolved = w
        .flow
        .resolve_attempt(
            NODE,
            "t-exposed",
            AttemptResolution::Closed {
                outcome: "written off after chain review".to_string(),
                evidence: serde_json::json!({ "checked": "solana" }),
            },
        )
        .await
        .expect("operator resolution");
    assert_eq!(
        resolved.state,
        PurchaseState::Resolved {
            outcome: "written off after chain review".to_string(),
            evidence: serde_json::json!({ "checked": "solana" }),
        }
    );
    assert_eq!(
        resolved.quote_id.as_deref(),
        quote_id.as_deref(),
        "the quote it was about is retained"
    );
}

/// A refusal that happened before anything left the process releases the
/// budget and may be prepared again — the positive control for the
/// ambiguity above.
#[tokio::test]
async fn an_unexposed_refusal_releases_and_may_prepare_again() {
    // The same exact-SVM world as the ambiguity witness above, so the
    // *only* difference between the two is where the refusal happened.
    let w = svm_world().await;
    let first = w
        .flow
        .prepare_task(NODE, &w.offer, &brief("t-unexposed"))
        .await
        .expect("prepare");
    let first_quote = w.attempt("t-unexposed").await.quote_id.expect("quote id");

    // Policy denies the spend outright: nothing is authored, nothing is
    // sent, and the provider never hears about it.
    w.spend
        .configure(|defaults, _| defaults.allowed_networks = Vec::new())
        .await
        .expect("disable the network");
    let denied = w.flow.purchase_task(NODE, "t-unexposed").await;
    let A2aPurchase::Denied {
        funds_ambiguous, ..
    } = &denied
    else {
        panic!("expected Denied, got {denied:?}");
    };
    assert!(
        !funds_ambiguous,
        "nothing was exposed, so nothing is ambiguous"
    );
    assert_eq!(w.channel.pay_sends(), 0, "no payment was ever delivered");
    assert_eq!(w.reserved_today().await, 0, "and no budget is held");
    assert_eq!(w.state("t-unexposed").await, StateTag::RefusedUnexposed);

    // Time passes before the retry — a quote id commits its issuance
    // instant, so a re-prepare on a frozen clock would re-derive the
    // same id and prove nothing about freshness.
    w.clock.advance(1_000_000);
    // Prepared again — a new quote under the same admission id, and the
    // purchase then goes through: the key was genuinely re-opened.
    w.spend
        .configure(|defaults, _| {
            defaults.allowed_networks = vec![SOLANA_MAINNET.to_string()];
        })
        .await
        .expect("re-enable the network");
    let second = w
        .flow
        .prepare_task(NODE, &w.offer, &brief("t-unexposed"))
        .await
        .expect("an unexposed refusal may be prepared again");
    assert_eq!(
        w.channel.quote_calls(),
        2,
        "the re-prepare minted a fresh quote"
    );
    assert_ne!(
        w.attempt("t-unexposed").await.quote_id.expect("quote id"),
        first_quote
    );
    assert_eq!(
        second.reservation.admission_id, first.reservation.admission_id,
        "under the same reservation"
    );
    let paid = w.flow.purchase_task(NODE, "t-unexposed").await;
    assert!(
        matches!(paid, A2aPurchase::Paid { .. }),
        "expected Paid after the re-prepare, got {paid:?}"
    );
    assert_eq!(w.channel.pay_sends(), 1);
    assert_eq!(w.billed().await, 1);
    assert_eq!(
        w.reserved_today().await,
        SVM_AMOUNT,
        "and now the budget is held — for the purchase that happened"
    );
}

// ---------------------------------------------------------------------------
// Table consistency (finding r4)
// ---------------------------------------------------------------------------

/// Every state pair the caller table does not name is a conflict, and
/// every pair it does name is accepted. Table-driven over all 121 pairs.
#[tokio::test]
async fn every_caller_transition_outside_the_table_is_a_conflict() {
    let w = world().await;
    let mut allowed = 0usize;
    let mut refused = 0usize;
    for from in StateTag::ALL {
        for to in StateTag::ALL {
            let task_id = format!("t-{from}-{to}");
            seed(&w, &task_id, sample_state(from)).await;
            let key = w.key(&task_id);
            let result = w
                .store
                .transition(&key, &[from], sample_state(to), w.clock.now_ns())
                .await;
            let expected = net_payments::flow::a2a::is_table_transition(from, to);
            match (&result, expected) {
                (Ok(attempt), true) => {
                    assert_eq!(attempt.state.tag(), to, "{from} → {to} landed elsewhere");
                    allowed += 1;
                }
                (Err(PurchaseError::NotATransition { .. }), false) => {
                    let after = w.store.attempt(&key).await.expect("read").expect("attempt");
                    assert_eq!(
                        after.state.tag(),
                        from,
                        "{from} → {to} was refused but still mutated the record"
                    );
                    refused += 1;
                }
                (result, expected) => {
                    panic!("{from} → {to}: expected in-table={expected}, got {result:?}")
                }
            }
        }
    }
    assert_eq!(
        allowed + refused,
        StateTag::ALL.len() * StateTag::ALL.len(),
        "every pair was exercised"
    );
    assert_eq!(allowed, 23, "the table has exactly the rows §D4 names");
    assert!(refused > 0, "and the rest are conflicts");
}

/// An approval is for one exact quote. Let it expire, re-prepare, and the
/// operator must approve again — the stale hold cannot authorize the new
/// purchase.
#[tokio::test]
async fn an_expired_awaiting_approval_quote_requires_fresh_approval_after_reprepare() {
    let w = world_requiring_approval().await;
    w.flow
        .prepare_task(NODE, &w.offer, &brief("t-approve"))
        .await
        .expect("prepare");
    let first_quote = w.attempt("t-approve").await.quote_id.expect("quote id");

    let held = w.flow.purchase_task(NODE, "t-approve").await;
    let A2aPurchase::RequiresPaymentApproval { quote_id, .. } = &held else {
        panic!("the production profile must hold a mock spend, got {held:?}");
    };
    assert_eq!(quote_id, &first_quote);
    assert_eq!(w.state("t-approve").await, StateTag::AwaitingApproval);
    assert_eq!(w.channel.pay_sends(), 0);

    // The operator approves the quote they were shown...
    assert!(w.spend.approve(&first_quote).await.expect("approve"));
    // ...and it expires before the purchase runs.
    w.clock.advance(120_000_000_000);

    let reprepared = w
        .flow
        .prepare_task(NODE, &w.offer, &brief("t-approve"))
        .await
        .expect("re-prepare after expiry");
    let second_quote = w.attempt("t-approve").await.quote_id.expect("quote id");
    assert_ne!(second_quote, first_quote, "a new quote was minted");
    assert!(!reprepared.reservation.admission_id.is_empty());
    assert!(
        !w.spend
            .pending()
            .await
            .expect("pending")
            .contains(&first_quote),
        "the stale hold was cleared"
    );
    assert_eq!(
        w.spend.approval_state(&first_quote).await.expect("state"),
        net_payments::policy::spend::ApprovalOutcome::Gone,
        "the old approval cannot authorize anything"
    );

    let held_again = w.flow.purchase_task(NODE, "t-approve").await;
    let A2aPurchase::RequiresPaymentApproval { quote_id, .. } = &held_again else {
        panic!("the new quote must need its own approval, got {held_again:?}");
    };
    assert_eq!(quote_id, &second_quote);
    assert_eq!(w.channel.pay_sends(), 0, "still nothing paid");

    // Approving the new quote is what lets it through.
    assert!(w.spend.approve(&second_quote).await.expect("approve"));
    let paid = w.flow.purchase_task(NODE, "t-approve").await;
    assert!(
        matches!(paid, A2aPurchase::Paid { .. }),
        "expected Paid after the fresh approval, got {paid:?}"
    );
    assert_eq!(w.channel.pay_sends(), 1);
    assert_eq!(w.billed().await, 1);
}

/// An operator rejection of a held quote is a proven non-payment, and it
/// re-opens the key.
#[tokio::test]
async fn an_operator_rejection_of_a_held_quote_is_an_unexposed_refusal() {
    let w = world_requiring_approval().await;
    w.flow
        .prepare_task(NODE, &w.offer, &brief("t-reject"))
        .await
        .expect("prepare");
    assert!(matches!(
        w.flow.purchase_task(NODE, "t-reject").await,
        A2aPurchase::RequiresPaymentApproval { .. }
    ));
    let quote_id = w.attempt("t-reject").await.quote_id.expect("quote id");

    assert!(w.spend.reject(&quote_id).await.expect("reject"));
    let denied = w.flow.purchase_task(NODE, "t-reject").await;
    let A2aPurchase::Denied {
        funds_ambiguous, ..
    } = &denied
    else {
        panic!("expected Denied after the operator said no, got {denied:?}");
    };
    assert!(!funds_ambiguous);
    assert_eq!(w.state("t-reject").await, StateTag::RefusedUnexposed);
    assert_eq!(w.channel.pay_sends(), 0);
}

/// `Unknown` recovers through the stored payment — automatically, and
/// never through a new quote. The operator's resolution is additive.
#[tokio::test]
async fn unknown_recovers_automatically_through_the_stored_payment_never_a_new_quote() {
    let w = world().await;
    w.flow
        .prepare_task(NODE, &w.offer, &brief("t-unknown"))
        .await
        .expect("prepare");
    w.channel.set_mode(PayMode::SwallowOnce);
    assert!(matches!(
        w.flow.purchase_task(NODE, "t-unknown").await,
        A2aPurchase::Unknown { .. }
    ));
    let quotes_before = w.channel.quote_calls();

    let recovered = w.flow.purchase_task(NODE, "t-unknown").await;
    assert!(
        matches!(recovered, A2aPurchase::Paid { .. }),
        "the provider came back and the stored payment resolved it: {recovered:?}"
    );
    assert_eq!(
        w.channel.quote_calls(),
        quotes_before,
        "recovery must not touch the quote channel"
    );
    assert_eq!(w.channel.distinct_payloads(), 1);
    assert_eq!(w.billed().await, 1);

    // The operator path is the additive alternative, for a provider that
    // never comes back.
    let w2 = world().await;
    w2.flow
        .prepare_task(NODE, &w2.offer, &brief("t-gone"))
        .await
        .expect("prepare");
    w2.channel.set_mode(PayMode::SwallowOnce);
    assert!(matches!(
        w2.flow.purchase_task(NODE, "t-gone").await,
        A2aPurchase::Unknown { .. }
    ));
    let resolved = w2
        .flow
        .resolve_attempt(
            NODE,
            "t-gone",
            AttemptResolution::NotPaid {
                reason: "chain shows no transfer".to_string(),
            },
        )
        .await
        .expect("operator resolution");
    assert_eq!(resolved.state.tag(), StateTag::RefusedUnexposed);
}

/// A paid purchase the provider will not execute keeps its evidence in
/// its own state, and the operator's resolution is the only exit.
#[tokio::test]
async fn a_late_paid_submit_becomes_paid_unexecutable_and_keeps_its_evidence() {
    let w = world().await;
    w.flow
        .prepare_task(NODE, &w.offer, &brief("t-late"))
        .await
        .expect("prepare");
    let paid = w.flow.purchase_task(NODE, "t-late").await;
    let A2aPurchase::Paid { proof, billing, .. } = &paid else {
        panic!("expected Paid, got {paid:?}");
    };

    // The reservation was retained out from under the purchase.
    w.tasks.push_submit(Err(A2aFlowError::PaymentRefused {
        message: SubmitRejection::NoReservation.to_string(),
        schematic: Some(Box::new(schematic("no_reservation", false, false))),
    }));
    let submitted = w.flow.submit_task(NODE, "t-late").await;
    let A2aSubmit::Unexecutable { refusal } = &submitted else {
        panic!("expected Unexecutable, got {submitted:?}");
    };
    assert_eq!(refusal.reason.as_deref(), Some("no_reservation"));
    assert!(!refusal.safe_to_retry && !refusal.safe_to_requote);

    let attempt = w.attempt("t-late").await;
    let PurchaseState::PaidUnexecutable {
        proof: kept_proof,
        billing: kept_billing,
        refusal: kept_refusal,
    } = &attempt.state
    else {
        panic!("expected PaidUnexecutable, got {:?}", attempt.state);
    };
    assert_eq!(kept_proof, proof, "the payment evidence is retained");
    assert_eq!(kept_billing, billing, "and so is the billing evidence");
    assert_eq!(kept_refusal, refusal);

    // Both purchase verbs refuse it.
    let repurchase = w.flow.purchase_task(NODE, "t-late").await;
    assert!(
        matches!(
            repurchase,
            A2aPurchase::Failed {
                retryable: false,
                ..
            }
        ),
        "a paid-unexecutable attempt cannot be re-purchased: {repurchase:?}"
    );
    let reprepare = w
        .flow
        .prepare_task(NODE, &w.offer, &brief("t-late"))
        .await
        .expect_err("nor prepared again");
    assert!(matches!(
        reprepare,
        A2aPrepareError::Attempt(PurchaseError::Conflict {
            found: StateTag::PaidUnexecutable,
            ..
        })
    ));
    assert_eq!(w.channel.quote_calls(), 1, "and never re-quoted");
    assert_eq!(w.billed().await, 1, "and never charged twice");

    let resolved = w
        .flow
        .resolve_attempt(
            NODE,
            "t-late",
            AttemptResolution::Closed {
                outcome: "refunded by the provider operator".to_string(),
                evidence: serde_json::json!({ "ticket": "OPS-12" }),
            },
        )
        .await
        .expect("operator resolution");
    assert_eq!(resolved.state.tag(), StateTag::Resolved);
}

/// A retryable submit refusal keeps the attempt `Paid`, so the *same*
/// proof is what gets resubmitted — never a second purchase.
#[tokio::test]
async fn a_retryable_submit_refusal_keeps_the_attempt_paid() {
    let w = world().await;
    w.flow
        .prepare_task(NODE, &w.offer, &brief("t-busy"))
        .await
        .expect("prepare");
    let paid = w.flow.purchase_task(NODE, "t-busy").await;
    let A2aPurchase::Paid { proof, .. } = &paid else {
        panic!("expected Paid, got {paid:?}");
    };

    // Capacity, in-body — retryable.
    w.tasks.push_submit(Ok(TaskAck {
        task_id: "t-busy".to_string(),
        accepted: false,
        reason: Some(SubmitRejection::Busy.to_string()),
    }));
    // Then the provider's journal wobbles — also retryable.
    w.tasks.push_submit(Err(A2aFlowError::PaymentRefused {
        message: "the admission journal is unavailable".to_string(),
        schematic: Some(Box::new(schematic("journal_unavailable", true, true))),
    }));
    // Then transport.
    w.tasks
        .push_submit(Err(A2aFlowError::Transport("dial failed".to_string())));

    for expected in ["capacity", "journal", "transport"] {
        let outcome = w.flow.submit_task(NODE, "t-busy").await;
        assert!(
            matches!(outcome, A2aSubmit::Retry { .. }),
            "{expected} refusal must be a retry, got {outcome:?}"
        );
        assert_eq!(
            w.state("t-busy").await,
            StateTag::Paid,
            "{expected}: the attempt stays Paid"
        );
    }

    // And the same proof, resubmitted, is accepted.
    let accepted = w.flow.submit_task(NODE, "t-busy").await;
    assert_eq!(
        accepted,
        A2aSubmit::Accepted {
            task_id: "t-busy".to_string()
        }
    );
    assert_eq!(
        w.tasks.submit_calls.load(Ordering::SeqCst),
        4,
        "four submissions were attempted"
    );
    assert_eq!(
        w.attempt("t-busy").await.state,
        PurchaseState::Submitted {
            task_id: "t-busy".to_string()
        }
    );
    assert_eq!(w.channel.quote_calls(), 1, "no re-quote");
    assert_eq!(w.billed().await, 1, "no second charge");
    // The proof that was accepted is the one that was paid for.
    let gate = EngineTaskAdmissionGate::new(w.engine.clone());
    let prepared = w
        .attempt("t-busy")
        .await
        .prepared
        .expect("prepared retained");
    let evidence = gate
        .redeem(TaskPaymentClaim {
            tool_id: "net.a2a.task/summarize",
            quote_id: &proof.quote_id,
            binding: &proof.binding_sig,
            expected_input_hash: &prepared.reservation.purchase_hash,
        })
        .await
        .expect("the gate admits the resubmitted proof");
    assert_eq!(evidence.quote_id, proof.quote_id);
}

/// Retention classes: only finished attempts age out. Anything whose
/// money is unresolved is the caller's only evidence and is never pruned.
#[tokio::test]
async fn unresolved_financial_attempts_are_never_pruned() {
    let w = world().await;
    let retention_ns = 1_000_000_000u64;
    let prunable = [
        StateTag::Submitted,
        StateTag::Resolved,
        StateTag::RefusedUnexposed,
        StateTag::Preparing,
        StateTag::Quoted,
    ];
    let never = [
        StateTag::Paid,
        StateTag::Unknown,
        StateTag::RefusedExposed,
        StateTag::PaidUnexecutable,
    ];
    for tag in prunable.iter().chain(never.iter()) {
        seed(&w, &format!("t-ret-{tag}"), sample_state(*tag)).await;
    }
    assert_eq!(w.flow.attempts().await.expect("attempts").len(), 9);

    // Not yet aged out: nothing goes.
    assert_eq!(
        w.store
            .prune(w.clock.now_ns(), retention_ns)
            .await
            .expect("prune"),
        0
    );
    w.clock.advance(retention_ns * 10);
    assert_eq!(
        w.store
            .prune(w.clock.now_ns(), retention_ns)
            .await
            .expect("prune"),
        prunable.len(),
        "exactly the finished attempts were pruned"
    );
    let left: Vec<StateTag> = w
        .flow
        .attempts()
        .await
        .expect("attempts")
        .iter()
        .map(|a| a.state.tag())
        .collect();
    for tag in never {
        assert!(
            left.contains(&tag),
            "{tag} is unresolved money and must survive retention; left: {left:?}"
        );
    }
    assert_eq!(left.len(), never.len());
}

/// The provider-side gate refuses a proof that was bought for a different
/// reservation, and the engine is not consumed by the attempt.
#[tokio::test]
async fn the_task_gate_refuses_a_proof_bought_for_another_reservation() {
    let w = world().await;
    let prepared = w
        .flow
        .prepare_task(NODE, &w.offer, &brief("t-gate"))
        .await
        .expect("prepare");
    let paid = w.flow.purchase_task(NODE, "t-gate").await;
    let A2aPurchase::Paid { proof, .. } = &paid else {
        panic!("expected Paid, got {paid:?}");
    };
    let gate = EngineTaskAdmissionGate::new(w.engine.clone());

    // Another reservation's expected hash: same payment, wrong purchase.
    let foreign = purchase_hash("adm-somebody-else", &prepared.reservation.commitment);
    let denial = gate
        .redeem(TaskPaymentClaim {
            tool_id: "net.a2a.task/summarize",
            quote_id: &proof.quote_id,
            binding: &proof.binding_sig,
            expected_input_hash: &foreign,
        })
        .await
        .expect_err("a proof for another reservation must be refused");
    assert_eq!(denial.schematic.reason, "input_binding_mismatch");
    assert!(!denial.schematic.recovery.safe_to_retry);
    assert!(!denial.schematic.recovery.safe_to_requote);

    // The real reservation still redeems — the refusal consumed nothing.
    let evidence = gate
        .redeem(TaskPaymentClaim {
            tool_id: "net.a2a.task/summarize",
            quote_id: &proof.quote_id,
            binding: &proof.binding_sig,
            expected_input_hash: &prepared.reservation.purchase_hash,
        })
        .await
        .expect("the right reservation is still admissible");
    assert_eq!(evidence.payer, *w.caller.entity_id().as_bytes());
    // And the engine's own decision agrees, idempotently.
    assert!(matches!(
        w.engine
            .redeem_for_task(
                "net.a2a.task/summarize",
                &proof.quote_id,
                &proof.binding_sig,
                &prepared.reservation.purchase_hash,
            )
            .await
            .expect("engine redeem"),
        RedeemDecision::Admitted { .. }
    ));
}
