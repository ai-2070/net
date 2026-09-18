//! Admission **identity** — the properties that make a decision land on
//! the record it was taken against, and only on that record
//! (`A2A_PAID_ADMISSION_PLAN.md` D2/D6; review findings R2, R3, R9, R10).
//!
//! The suites next door witness the state table, the retention classes
//! and the handler paths. What is witnessed here is the seam between
//! them and time:
//!
//! - a published `max_in_flight` is enforced **where the write happens**,
//!   so distinct tasks cannot overbook one slot, and the slot is held
//!   across the awaited preflight/redemption rather than evaporating
//!   mid-decision;
//! - a decision names an **incarnation**, not a state tag, so a
//!   replacement record can never be marked paid by a decision taken
//!   against its predecessor — and a redemption that was refused is
//!   retained where an operator can see it, because a payment that
//!   happened is a fact;
//! - an incarnation number is never reused, across a prune and across a
//!   restart;
//! - the journal's write completes under its own guards even when the
//!   future that started it is aborted, never shares a temp path with a
//!   same-stem sibling, and reports a post-rename durability failure as
//!   **ambiguous** rather than as "the file is untouched";
//! - concurrent submitters of one task converge on the **decider's own
//!   verdict**, so a retryable refusal reaches every one of them as
//!   retryable.
//!
//! Each witness names the inverse it was checked against, because
//! "capacity is enforced" and "capacity is counted" pass the same happy
//! path.

#![cfg(all(feature = "net", feature = "cortex", feature = "testing"))]

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::{ChannelConfigRegistry, MeshNode, MeshNodeConfig};
use net_sdk::a2a::{
    purchase_hash, task_commitment, A2aBounds, A2aOffer, AdmissionReservation, CancelToken,
    PrepareReply, PreparedTask, TaskBrief, TaskExecutor, TaskOwner, TaskRegistry, TaskState,
};
use net_sdk::a2a_journal::{
    now_secs, A2aAdmissionJournal, A2aJournalError, AdmissionRecord, AdmissionState,
    AdmissionStore, AdmitOutcome, DecisionOutcome, StateTag,
};
use net_sdk::a2a_payment::{
    TaskAdmissionGate, TaskPaymentClaim, TaskPaymentEvidence, TaskPaymentProof,
};
use net_sdk::identity::Identity;
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::mesh_a2a::{
    A2aFlowError, A2aPrincipal, A2aServiceConfig, A2aServicePolicy, A2aServing, TaskPreflight,
};
use net_sdk::org::types::{
    DispatcherScope, NodeAuthority, OrgDispatcherGrant, OrgKeypair, OrgMembershipCert,
    OwnerAudienceCredential,
};
use net_sdk::org::{OrgAccess, OrgCredentials};
use net_sdk::tool_payment::{
    failure_vocab, FailureSchematic, GateDenial, Recovery, TAG_PAYMENT_FAILURE,
};

const PSK: [u8; 32] = [0x71u8; 32];
const SERVICE: &str = "svc.summarize";
const TERMS: &str = r#"{"object":"net.pricing.terms@1"}"#;
const PAYER: [u8; 32] = [0xABu8; 32];
const BINDING: [u8; 64] = [7u8; 64];
const RESULT: &str = "blob://summary-42";

// ---------------------------------------------------------------------------
// Offers, briefs, records
// ---------------------------------------------------------------------------

fn offer(paid: bool) -> A2aOffer {
    A2aOffer {
        service_id: SERVICE.to_string(),
        revision: "r1".to_string(),
        description: None,
        pricing_terms: paid.then(|| TERMS.to_string()),
        bounds: A2aBounds {
            max_prompt_bytes: 1024,
            max_context_refs: 8,
            max_tags: 8,
            max_tag_bytes: 32,
            max_in_flight: 4,
        },
        reservation_ttl_secs: 300,
        reservation_retention_secs: 7 * 24 * 60 * 60,
        retention_secs: 60 * 60,
    }
}

fn brief_id(task_id: &str, prompt: &str) -> TaskBrief {
    TaskBrief {
        task_id: task_id.to_string(),
        prompt: prompt.to_string(),
        context_refs: vec!["blob://ctx".to_string()],
        tags: vec!["finance".to_string()],
        service: Some(SERVICE.to_string()),
        revision: Some("r1".to_string()),
    }
}

fn brief(task_id: &str) -> TaskBrief {
    brief_id(task_id, "summarize the quarterly filings")
}

/// The owner every store-level witness uses: an `Entity`, so each round
/// trip also exercises the 32-byte owner encoding.
fn owner() -> TaskOwner {
    TaskOwner::Entity([9u8; 32])
}

/// A candidate reservation, as prepare would build one — the store mints
/// the incarnation.
fn candidate_for(
    who: TaskOwner,
    task_id: &str,
    admission_id: &str,
    offer: &A2aOffer,
    now: u64,
) -> AdmissionRecord {
    let b = brief(task_id);
    AdmissionRecord::reserved(
        who,
        b.clone(),
        offer,
        Some(admission_id.to_string()),
        task_commitment(offer, &b),
        now,
    )
}

fn candidate(task_id: &str, admission_id: &str, offer: &A2aOffer, now: u64) -> AdmissionRecord {
    candidate_for(owner(), task_id, admission_id, offer, now)
}

async fn journal_at(path: &std::path::Path) -> A2aAdmissionJournal {
    A2aAdmissionJournal::open(path).await.expect("open journal")
}

/// Reopen a journal after its previous owner was dropped, waiting for the
/// ownership lock to actually be released.
///
/// Dropping the `ServeHandle`s is not the same instant as releasing the
/// owner, and that is by design: a launched executor future and the
/// terminal hook each hold an `Arc<JournalOwner>` so a running task can
/// still write after the handles are gone. A completed task's future is
/// therefore dropped by the runtime a little *after* the work finishes,
/// so a successor that opens immediately can legitimately lose the race
/// and see `OwnedElsewhere`.
///
/// This is a bounded precondition on the observable being asserted, not
/// a sleep that makes a flaky thing pass: the restart property is about
/// what the successor INHERITS, and it cannot be observed until there is
/// a successor. It fails loudly, naming the condition, rather than
/// timing out somewhere less legible. (Found by Linux CI; on Windows the
/// runtime happened to reap the task first, so it passed locally — the
/// race was always there.)
async fn journal_after_release(path: &std::path::Path) -> A2aAdmissionJournal {
    for _ in 0..200 {
        match A2aAdmissionJournal::open(path).await {
            Ok(journal) => return journal,
            Err(A2aJournalError::OwnedElsewhere { .. }) => {
                tokio::task::yield_now().await;
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            Err(e) => panic!("open journal: {e:?}"),
        }
    }
    panic!(
        "the previous owner never released {} — a dropped ServeHandle should not keep \
         the journal owned once its tasks are done",
        path.display()
    )
}

/// Admit `record`, asserting it was admitted, and return the stored row
/// (with its minted generation).
async fn admit(
    store: &dyn AdmissionStore,
    record: AdmissionRecord,
    max_in_flight: u64,
    now: u64,
) -> AdmissionRecord {
    match store.admit_reserved(record, max_in_flight, now).await {
        Ok(AdmitOutcome::Admitted(stored)) => *stored,
        other => panic!("expected an admission, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Scripted host: a counting executor, and a preflight and gate that can
// each be held on a barrier
// ---------------------------------------------------------------------------

/// A barrier plus a call counter. `entered` counts calls **with
/// multiplicity** — never a set: whether a second submitter decided at
/// all is the discrimination two witnesses here turn on.
struct Barrier {
    entered: AtomicU64,
    gate: tokio::sync::Semaphore,
    held: bool,
}

impl Barrier {
    fn new(held: bool) -> Self {
        Self {
            entered: AtomicU64::new(0),
            gate: tokio::sync::Semaphore::new(0),
            held,
        }
    }

    async fn pass(&self) {
        self.entered.fetch_add(1, Ordering::SeqCst);
        if self.held {
            if let Ok(permit) = self.gate.acquire().await {
                permit.forget();
            }
        }
    }

    fn entered(&self) -> u64 {
        self.entered.load(Ordering::SeqCst)
    }

    fn release(&self) {
        self.gate.add_permits(64);
    }

    /// Wait until at least `n` calls have entered — the precondition a
    /// witness asserts from, never a sleep.
    async fn await_entered(&self, n: u64) {
        for _ in 0..500 {
            if self.entered() >= n {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!(
            "only {} of {n} calls ever entered the barrier",
            self.entered()
        );
    }
}

struct ExecProbe {
    runs: Arc<AtomicU64>,
}

#[async_trait::async_trait]
impl TaskExecutor for ExecProbe {
    async fn run(&self, _brief: TaskBrief, _cancel: CancelToken) -> Result<String, String> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        Ok(RESULT.to_string())
    }
}

/// The application preflight, optionally held on a barrier.
struct HeldPreflight {
    barrier: Arc<Barrier>,
}

#[async_trait::async_trait]
impl TaskPreflight for HeldPreflight {
    async fn preflight(
        &self,
        _owner: TaskOwner,
        _offer: &A2aOffer,
        _brief: &TaskBrief,
    ) -> Result<(), String> {
        self.barrier.pass().await;
        Ok(())
    }
}

/// The payment gate, optionally held on a barrier: quotes are issued by
/// committed input hash, exactly as the engine's are.
struct HeldGate {
    barrier: Arc<Barrier>,
    quotes: parking_lot::Mutex<HashMap<String, String>>,
    calls: AtomicU64,
}

impl HeldGate {
    fn new(held: bool) -> Arc<Self> {
        Arc::new(Self {
            barrier: Arc::new(Barrier::new(held)),
            quotes: parking_lot::Mutex::new(HashMap::new()),
            calls: AtomicU64::new(0),
        })
    }

    fn issue(&self, quote_id: &str, input_hash: &str) {
        self.quotes
            .lock()
            .insert(quote_id.to_string(), input_hash.to_string());
    }

    fn calls(&self) -> u64 {
        self.calls.load(Ordering::SeqCst)
    }
}

fn denial(reason: &str, quote_id: &str, tool_id: &str) -> GateDenial {
    GateDenial {
        schematic: FailureSchematic {
            object: TAG_PAYMENT_FAILURE.to_string(),
            code: failure_vocab::CODE_PAYMENT.to_string(),
            stage: failure_vocab::STAGE_REDEEM.to_string(),
            reason: reason.to_string(),
            message: reason.to_string(),
            retryable: false,
            recovery: Recovery {
                class: failure_vocab::CLASS_SECURITY_VIOLATION.to_string(),
                actor: failure_vocab::ACTOR_CALLER_OPERATOR.to_string(),
                safe_to_retry: false,
                safe_to_requote: false,
                next_action: None,
            },
            handler_executed: false,
            funds_moved: failure_vocab::FUNDS_UNKNOWN.to_string(),
            prior_payment: failure_vocab::PRIOR_UNKNOWN.to_string(),
            quote_id: Some(quote_id.to_string()),
            tool_id: Some(tool_id.to_string()),
            extra: Default::default(),
        },
        message: reason.to_string(),
    }
}

#[async_trait::async_trait]
impl TaskAdmissionGate for HeldGate {
    async fn redeem(&self, claim: TaskPaymentClaim<'_>) -> Result<TaskPaymentEvidence, GateDenial> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.barrier.pass().await;
        let issued = self.quotes.lock().get(claim.quote_id).cloned();
        match issued {
            Some(hash) if hash == claim.expected_input_hash => Ok(TaskPaymentEvidence {
                quote_id: claim.quote_id.to_string(),
                payer: PAYER,
            }),
            Some(_) => Err(denial(
                "input_binding_mismatch",
                claim.quote_id,
                claim.tool_id,
            )),
            None => Err(denial("unknown_quote", claim.quote_id, claim.tool_id)),
        }
    }
}

// ---------------------------------------------------------------------------
// The served fixture
// ---------------------------------------------------------------------------

async fn mesh() -> Mesh {
    MeshBuilder::new("127.0.0.1:0", &PSK)
        .expect("builder")
        .build()
        .await
        .expect("build")
}

/// Handshake `caller` to `provider`, then start both dispatch loops
/// (accepts must precede `start`, the `a2a_task_ownership` idiom).
async fn connect(provider: &Mesh, caller: &Mesh) {
    let addr = provider.inner().local_addr();
    let pubkey = *provider.inner().public_key();
    let nid_provider = provider.inner().node_id();
    let nid_caller = caller.inner().node_id();
    let (accepted, connected) = tokio::join!(provider.inner().accept(nid_caller), async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        caller.inner().connect(addr, &pubkey, nid_provider).await
    });
    accepted.expect("accept");
    connected.expect("connect");
    caller.inner().start();
    provider.inner().start();
}

/// What a witness wants of its provider before the first request lands.
///
/// Plain data rather than a callback: the two things that have to happen
/// **before** the journal is handed to the serving path — seeding a
/// previous owner's row, and arming a write to fail — both need the
/// session-peer owner, which is only known once the meshes exist.
struct ServeOpts {
    offer: A2aOffer,
    hold_gate: bool,
    hold_preflight: bool,
    /// Seed a `Paid` admission for this brief under `(admission_id,
    /// quote_id)`, exactly as a provider that crashed between redeem and
    /// claim would have left it.
    seed_paid: Option<(TaskBrief, String, String)>,
    /// Arm the n-th durable write **after** the seed to fail.
    fail_nth_write: Option<u64>,
}

impl ServeOpts {
    fn paid() -> Self {
        Self {
            offer: offer(true),
            hold_gate: false,
            hold_preflight: false,
            seed_paid: None,
            fail_nth_write: None,
        }
    }
}

struct Fixture {
    _provider: Mesh,
    caller: Arc<Mesh>,
    target: u64,
    serving: Option<A2aServing>,
    gate: Arc<HeldGate>,
    preflight: Arc<Barrier>,
    runs: Arc<AtomicU64>,
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
}

/// Serve one offer over a real two-node wire.
async fn serve(opts: ServeOpts) -> Fixture {
    let provider = mesh().await;
    let caller = Arc::new(mesh().await);
    let owner = TaskOwner::Peer(caller.inner().node_id());
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("admissions.json");

    let journal = journal_at(&path).await;
    if let Some((brief, admission_id, quote_id)) = opts.seed_paid.as_ref() {
        let now = now_secs();
        let record = admit(
            &journal,
            AdmissionRecord::reserved(
                owner,
                brief.clone(),
                &opts.offer,
                Some(admission_id.clone()),
                task_commitment(&opts.offer, brief),
                now,
            ),
            opts.offer.bounds.max_in_flight,
            now,
        )
        .await;
        journal
            .redeem(&record, quote_id.clone(), PAYER, now)
            .await
            .expect("seed the paid state");
    }
    if let Some(n) = opts.fail_nth_write {
        journal.fail_nth_write(n);
    }

    let gate = HeldGate::new(opts.hold_gate);
    let preflight = Arc::new(Barrier::new(opts.hold_preflight));
    let runs = Arc::new(AtomicU64::new(0));
    let policy = if opts.offer.pricing_terms.is_some() {
        A2aServicePolicy::Paid(opts.offer.clone())
    } else {
        A2aServicePolicy::Free(opts.offer.clone())
    };
    let catalog: BTreeMap<String, A2aServicePolicy> =
        [(SERVICE.to_string(), policy)].into_iter().collect();
    let config = A2aServiceConfig::new(catalog)
        .with_payment(Arc::clone(&gate) as Arc<dyn TaskAdmissionGate>)
        .with_preflight(Arc::new(HeldPreflight {
            barrier: Arc::clone(&preflight),
        }) as Arc<dyn TaskPreflight>)
        .with_journal(journal);

    let serving = provider
        .serve_a2a_configured(
            TaskRegistry::new(),
            Arc::new(ExecProbe {
                runs: Arc::clone(&runs),
            }),
            config,
        )
        .expect("serve the configured catalog");

    connect(&provider, &caller).await;
    let target = provider.inner().node_id();
    // Readiness precondition, not a sleep: describe is the cheapest
    // uncharged verb, and it answering means the dispatch path is up.
    let mut ready = false;
    for _ in 0..50 {
        if caller.describe_a2a(target).await.is_ok() {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(ready, "the provider never answered describe");

    Fixture {
        _provider: provider,
        caller,
        target,
        serving: Some(serving),
        gate,
        preflight,
        runs,
        _dir: dir,
        path,
    }
}

impl Fixture {
    fn store(&self) -> &dyn AdmissionStore {
        self.serving.as_ref().expect("serving").store.as_ref()
    }

    fn owner(&self) -> TaskOwner {
        TaskOwner::Peer(self.caller.inner().node_id())
    }

    async fn record(&self, task_id: &str) -> Option<AdmissionRecord> {
        self.store()
            .lookup(self.owner(), task_id)
            .await
            .expect("lookup")
    }

    async fn prepare(&self, brief: &TaskBrief) -> PrepareReply {
        self.caller
            .prepare_a2a(self.target, brief)
            .await
            .expect("prepare")
    }

    /// Wait until the durable record reaches `tag` — the precondition a
    /// terminal-hook write establishes, never a fixed sleep.
    async fn await_tag(&self, task_id: &str, tag: StateTag) {
        for _ in 0..500 {
            if self.record(task_id).await.map(|r| r.state.tag()) == Some(tag) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!(
            "task {task_id:?} never reached {}; it is {:?}",
            tag.as_str(),
            self.record(task_id).await.map(|r| r.state)
        );
    }

    fn runs(&self) -> u64 {
        self.runs.load(Ordering::SeqCst)
    }
}

/// The caller-side document a paid submit needs.
fn prepared(target: u64, brief: &TaskBrief, offer: &A2aOffer, reply: PrepareReply) -> PreparedTask {
    match reply {
        PrepareReply::Reservation(reservation) => PreparedTask {
            provider_node: target,
            brief: brief.clone(),
            offer_hash: offer.hash(),
            reservation,
        },
        other => panic!("expected a reservation, got {other:?}"),
    }
}

fn proof(quote_id: &str) -> TaskPaymentProof {
    TaskPaymentProof {
        quote_id: quote_id.to_string(),
        binding_sig: BINDING.to_vec(),
    }
}

/// The schematic of a payment refusal, or a panic naming what came back
/// instead — a witness about retryability must never pass on the wrong
/// failure.
fn refusal_schematic(e: &A2aFlowError) -> &FailureSchematic {
    match e {
        A2aFlowError::PaymentRefused { schematic, .. } => {
            schematic.as_deref().expect("a decoded schematic")
        }
        other => panic!("expected a payment refusal, got {other:?}"),
    }
}

// ===========================================================================
// R2 — capacity is held across the awaited decision
// ===========================================================================

/// A reservation that lapses **while its decision is running** keeps its
/// capacity slot, so a competing task cannot take it and leave the
/// decision landing against a stale time sample.
///
/// The schedule the reviewer traced in the source, driven: A prepares
/// against `max_in_flight = 1` with a one-second reservation, submits,
/// and parks in the gate. Its reservation lapses while it is in there.
/// B then prepares a **different** task id — the case registry
/// deduplication does not cover.
///
/// Inverse: drop `|| self.deciding` from
/// `AdmissionRecord::holds_capacity` — B is admitted and the assertion
/// below fires.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_decision_holds_its_capacity_slot_across_an_expiring_reservation() {
    let mut narrow = offer(true);
    narrow.bounds.max_in_flight = 1;
    narrow.reservation_ttl_secs = 1;
    let f = serve(ServeOpts {
        offer: narrow.clone(),
        hold_gate: true,
        ..ServeOpts::paid()
    })
    .await;

    let a = brief("hold-a");
    let ready = prepared(f.target, &a, &narrow, f.prepare(&a).await);
    f.gate.issue("q-a", &ready.reservation.purchase_hash);

    let submit = tokio::spawn({
        let caller = Arc::clone(&f.caller);
        let ready = ready.clone();
        async move { caller.submit_task_paid(&ready, &proof("q-a")).await }
    });
    // Precondition, not a window: the decision is demonstrably inside the
    // gate before anything is asserted about its slot.
    f.gate.barrier.await_entered(1).await;

    // Past the reservation's own expiry, so the ONLY thing that can be
    // holding the slot is the open decision.
    let lapse_at = now_secs() + narrow.reservation_ttl_secs + 1;
    while now_secs() < lapse_at {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let held = f.record("hold-a").await.expect("A's record");
    assert!(
        matches!(held.state, AdmissionState::Reserved { expires_at } if expires_at <= now_secs()),
        "A's reservation must actually have lapsed, or this witness is vacuous: {:?}",
        held.state
    );
    assert!(held.deciding, "A's decision must still be open");

    let b = brief("hold-b");
    assert!(
        matches!(f.prepare(&b).await, PrepareReply::Busy),
        "a distinct task took the slot an awaited redemption was still using"
    );

    // The control: release the decision, let it resolve, and the slot is
    // available again — so `Busy` above was a held slot and not a wedged
    // service.
    f.gate.barrier.release();
    let ack = submit.await.expect("join").expect("A's submit");
    assert!(ack.accepted, "{:?}", ack.reason);
    assert_eq!(f.gate.calls(), 1, "exactly one redemption");
    f.await_tag("hold-a", StateTag::Terminal).await;
    assert_eq!(f.runs(), 1, "A ran exactly once");
    assert!(
        !f.record("hold-a").await.expect("A's record").deciding,
        "the decision must be closed once it resolved"
    );
    assert!(
        matches!(f.prepare(&b).await, PrepareReply::Reservation(_)),
        "B must be admitted once A's admission resolved — otherwise `Busy` above \
         proved nothing about capacity"
    );
}

// ===========================================================================
// R3 — a decision names an incarnation
// ===========================================================================

/// The reviewer's gate-barrier / prune / reprepare / restart schedule,
/// driven end to end through the serving path.
///
/// While A's redemption is outstanding, an operator prune **cannot**
/// remove its reservation and a reprepare **cannot** mint a replacement,
/// so A's decision has nothing stale to land on: the payment is recorded
/// against A's own admission, the launch claim follows, and a restart
/// inherits exactly that.
///
/// Inverse: restore the pruning of a deciding reservation (drop the
/// `r.deciding` arm from `StoreState::prune`) — the prune below removes
/// the row, the reprepare mints a new admission id, and the identity
/// assertions fire.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_gate_barrier_prune_and_reprepare_cannot_replace_a_deciding_admission() {
    let terms = offer(true);
    let mut f = serve(ServeOpts {
        hold_gate: true,
        ..ServeOpts::paid()
    })
    .await;

    let work = brief_id("aba", "work A");
    let ready = prepared(f.target, &work, &terms, f.prepare(&work).await);
    let admission_a = ready.reservation.admission_id.clone();
    f.gate
        .issue("quote-for-A", &ready.reservation.purchase_hash);

    let submit = tokio::spawn({
        let caller = Arc::clone(&f.caller);
        let ready = ready.clone();
        async move { caller.submit_task_paid(&ready, &proof("quote-for-A")).await }
    });
    f.gate.barrier.await_entered(1).await;

    let generation_a = f.record("aba").await.expect("A's record").generation;

    // Retention maintenance, on the public store's explicit clock, while
    // the redemption is outstanding.
    let pruned = f
        .store()
        .prune(now_secs() + terms.reservation_retention_secs + 1)
        .await
        .expect("prune");
    assert_eq!(
        pruned, 0,
        "an actively deciding admission was aged out from under its own decision"
    );

    // A reprepare of the same key cannot mint a replacement: the
    // incumbent reservation is still there and still this decision's.
    match f.prepare(&work).await {
        PrepareReply::Reservation(res) => assert_eq!(
            res.admission_id, admission_a,
            "a reprepare minted a replacement admission while a decision was open"
        ),
        other => panic!("expected the same reservation, got {other:?}"),
    }
    assert_eq!(
        f.record("aba").await.expect("A's record").generation,
        generation_a,
        "the incarnation under the key changed while its decision was open"
    );

    f.gate.barrier.release();
    let ack = submit.await.expect("join").expect("A's submit");
    assert!(ack.accepted, "{:?}", ack.reason);
    f.await_tag("aba", StateTag::Terminal).await;

    let landed = f.record("aba").await.expect("A's record");
    assert_eq!(
        landed.generation, generation_a,
        "the payment landed on a different incarnation"
    );
    assert_eq!(landed.admission_id, Some(admission_a));
    assert!(
        matches!(&landed.state, AdmissionState::Terminal { quote_id: Some(q), .. } if q == "quote-for-A"),
        "A's own admission must carry A's quote: {:?}",
        landed.state
    );
    assert_eq!(f.runs(), 1, "the work ran exactly once");

    // Restart: a successor owner inherits that record and rewrites
    // nothing about its identity.
    drop(f.serving.take());
    let successor = journal_after_release(&f.path).await;
    let after = successor
        .lookup(f.owner(), "aba")
        .await
        .expect("lookup")
        .expect("the record survived the restart");
    assert_eq!(
        after.generation, generation_a,
        "a restart must not rewrite an incarnation"
    );
    assert!(
        !after.deciding,
        "a successor owner must clear an inherited decision hold"
    );
}

/// A redemption whose admission was replaced is refused **and retained**
/// against the admission it was taken for.
///
/// Refusing protects the replacement; retaining is what keeps the
/// provider honest about a payment the gate already granted. The
/// reviewer's own probe covers the identity-unbound `transition`; this is
/// the identity-bound verb, and the half that says where the money went.
///
/// Inverse: make `StoreState::redeem`'s mismatch arm return
/// `Err(identity.superseded())` without writing the detached record — the
/// `unresolved()` assertions fire while the refusal still passes, which
/// is exactly the difference between "refused" and "accounted for".
#[tokio::test]
async fn a_superseded_redemption_is_retained_against_its_own_admission() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("admissions.json");
    let journal = journal_at(&path).await;
    let terms = offer(true);
    let now = now_secs();

    let a = admit(&journal, candidate("aba", "adm-A", &terms, now), 4, now).await;
    // Retention maintenance removes A's reservation. No decision is open
    // on it, so this is legitimate.
    assert_eq!(
        journal
            .prune(now + terms.reservation_retention_secs + 1)
            .await
            .expect("prune"),
        1
    );
    let b = admit(&journal, candidate("aba", "adm-B", &terms, now), 4, now).await;
    assert_ne!(
        a.generation, b.generation,
        "a replacement must be a new incarnation"
    );

    // A's decision resolves: the gate granted the payment, and A's row is
    // gone.
    let refused = journal
        .redeem(&a, "quote-for-A".to_string(), PAYER, now)
        .await;
    assert!(
        matches!(
            &refused,
            Err(A2aJournalError::Superseded { generation, admission_id, .. })
                if *generation == a.generation && admission_id.as_deref() == Some("adm-A")
        ),
        "the refusal must name the admission it was taken against: {refused:?}"
    );

    let live = journal
        .lookup(owner(), "aba")
        .await
        .expect("lookup")
        .expect("B's record");
    assert_eq!(
        live.generation, b.generation,
        "B was replaced by A's decision"
    );
    assert_eq!(live.admission_id.as_deref(), Some("adm-B"));
    assert_eq!(
        live.state.tag(),
        StateTag::Reserved,
        "B's record must be untouched, not paid: {:?}",
        live.state
    );

    let unresolved = journal.unresolved().await.expect("unresolved");
    assert_eq!(
        unresolved.len(),
        1,
        "a redeemed payment must leave exactly one unaccounted record: {unresolved:?}"
    );
    assert_eq!(unresolved[0].generation, a.generation);
    assert_eq!(unresolved[0].admission_id.as_deref(), Some("adm-A"));
    assert_eq!(
        unresolved[0].state,
        AdmissionState::Paid {
            quote_id: "quote-for-A".to_string(),
            payer: PAYER,
        },
        "the retained record must carry A's quote"
    );

    // The live row is an ordinary unpaid reservation and ages out; the
    // retained redemption is unresolved financial evidence and survives
    // every retention timer, which is the whole point of retaining it.
    assert_eq!(
        journal
            .prune(now + 10 * 365 * 24 * 60 * 60)
            .await
            .expect("prune"),
        1,
        "the live reservation ages out — the control that prune ran at all"
    );
    assert!(
        journal
            .lookup(owner(), "aba")
            .await
            .expect("lookup")
            .is_none(),
        "B's reservation was the only prunable row"
    );
    let survivors = journal.unresolved().await.expect("unresolved");
    assert_eq!(
        survivors.len(),
        1,
        "a retained redemption must survive every retention timer: {survivors:?}"
    );
    assert_eq!(survivors[0].generation, a.generation);
    assert_eq!(survivors[0].admission_id.as_deref(), Some("adm-A"));

    // The operator path is the only exit.
    journal
        .resolve(
            owner(),
            "aba",
            TaskState::Failed {
                error: "refunded out of band".to_string(),
            },
            now,
        )
        .await
        .expect("an operator closes the retained redemption");
    assert!(
        journal.unresolved().await.expect("unresolved").is_empty(),
        "resolve is the exit from the unresolved class"
    );
}

/// An incarnation number is never reused — not after a prune, and not
/// across a restart.
///
/// A per-key counter that restarts at zero rebuilds the very ABA defect
/// the generation exists to close, one process lifetime later: the
/// replacement row would carry the number an old decision still names.
///
/// Inverse: drop `next_generation` from `JournalFile` (and the
/// `records`/`detached` maximum in `into_state`) so the counter is
/// rebuilt from nothing — the reopened journal mints the same number
/// again and both assertions below fire.
#[tokio::test]
async fn a_generation_is_never_reused_after_a_prune_and_a_restart() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("admissions.json");
    let terms = offer(true);
    let now = now_secs();

    let first = {
        let journal = journal_at(&path).await;
        let a = admit(&journal, candidate("gen", "adm-A", &terms, now), 4, now).await;
        assert_eq!(
            journal
                .prune(now + terms.reservation_retention_secs + 1)
                .await
                .expect("prune"),
            1
        );
        a
    };

    // Leave NOTHING on disk that could reconstruct the counter.
    //
    // The reviewer's qualification, and she is right: `prune` RETAINS a
    // purchasable reservation as a detached record (round 1's C3
    // repair), so a journal that rebuilt `next_generation` from the
    // highest generation it could still see would pass this test while
    // persisting nothing. Closing and pruning every retained row first
    // is what isolates genuine counter persistence from
    // max-retained-generation reconstruction.
    {
        let journal = journal_at(&path).await;
        for row in journal.unresolved().await.expect("unresolved") {
            journal
                .resolve_exact(
                    &row.identity(),
                    TaskState::Failed {
                        error: "closed so nothing retained can seed the counter".to_string(),
                    },
                    now,
                )
                .await
                .expect("close the retained row");
        }
        journal
            .prune(now + terms.retention_secs + terms.reservation_retention_secs + 1)
            .await
            .expect("prune the closed rows");
        assert!(
            journal.unresolved().await.expect("unresolved").is_empty(),
            "the isolation failed: something unresolved is still on disk"
        );
        assert!(
            journal
                .lookup(owner(), "gen")
                .await
                .expect("lookup")
                .is_none(),
            "the isolation failed: the pruned key is still readable"
        );
    }
    // Belt and braces, from outside the API: no record carrying a
    // generation remains in the file the successor is about to read, so
    // `into_state`'s `max(rows, persisted)` has nothing but the persisted
    // counter to work from.
    //
    // Structural, not a substring search: `next_generation` is a small
    // integer and the first draft of this check asserted on
    // `!raw.contains("1")`, which the persisted counter itself matched.
    // That would have failed a correct journal.
    let raw = tokio::fs::read_to_string(&path)
        .await
        .expect("read journal");
    let file: serde_json::Value = serde_json::from_str(&raw).expect("journal is json");
    for table in ["records", "detached"] {
        assert_eq!(
            file[table].as_array().map(Vec::len),
            Some(0),
            "the isolation failed: `{table}` still carries a row, so the successor \
             could rebuild the counter from it instead of from the persisted value: {raw}"
        );
    }

    // A genuinely new owner of the same file.
    let journal = journal_at(&path).await;
    let second = admit(&journal, candidate("gen", "adm-B", &terms, now), 4, now).await;
    assert!(
        second.generation > first.generation,
        "a restart rewound the incarnation counter with nothing on disk to \
         reconstruct it from: {} -> {}",
        first.generation,
        second.generation
    );

    // And the decision taken before the restart cannot match the new
    // incarnation.
    let refused = journal
        .redeem(&first, "quote-for-A".to_string(), PAYER, now)
        .await;
    assert!(
        matches!(refused, Err(A2aJournalError::Superseded { .. })),
        "a decision from before the restart matched the replacement: {refused:?}"
    );
    assert_eq!(
        journal
            .lookup(owner(), "gen")
            .await
            .expect("lookup")
            .expect("B")
            .state
            .tag(),
        StateTag::Reserved
    );
}

// ===========================================================================
// R10 — concurrent submitters converge on the decider's verdict
// ===========================================================================

/// Two concurrent submits of one task: one decides, the other parks —
/// and when the decider's **launch-claim write** fails, both callers
/// receive the same retryable schematic.
///
/// A waiter handed a generic rejected ack cannot tell a retryable
/// journal failure from a permanent refusal, and a caller that reads
/// "retry the same proof" as "this purchase can never execute" abandons
/// paid work. `preflight.entered() == 1` is what proves the duplicate
/// parked rather than deciding for itself — without it this witness
/// could pass having exercised no waiter at all.
///
/// Inverse: answer the waiter with `ack_refused(refusal.message())`
/// instead of `refusal_payload(&refusal)` in `submit`'s
/// `Admission::Pending` arm — the waiter's call returns `Ok(TaskAck {
/// accepted: false })`, `refusal_schematic` panics naming what came back
/// instead, and the row is red.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_submits_converge_on_the_deciders_retryable_verdict() {
    let terms = offer(true);
    let work = brief("converge");
    // Seeded `Paid`, exactly as a provider that crashed between redeem
    // and claim would have left it, so the decision makes exactly two
    // writes: the decision hold, then the launch claim. The second is
    // the one armed to fail.
    let f = serve(ServeOpts {
        hold_preflight: true,
        seed_paid: Some((work.clone(), "adm-seed".to_string(), "q-seed".to_string())),
        fail_nth_write: Some(2),
        ..ServeOpts::paid()
    })
    .await;
    assert_eq!(
        f.record("converge").await.map(|r| r.state.tag()),
        Some(StateTag::Paid),
        "the seed must leave a paid admission"
    );

    let commitment = task_commitment(&terms, &work);
    let ready = PreparedTask {
        provider_node: f.target,
        brief: work.clone(),
        offer_hash: terms.hash(),
        reservation: AdmissionReservation {
            task_id: work.task_id.clone(),
            admission_id: "adm-seed".to_string(),
            purchase_hash: purchase_hash("adm-seed", &commitment),
            commitment,
            capability: String::new(),
            pricing_terms: Some(TERMS.to_string()),
            expires_at: now_secs() + 300,
        },
    };

    // The decider parks in the preflight; its next write is the claim.
    let first = tokio::spawn({
        let caller = Arc::clone(&f.caller);
        let ready = ready.clone();
        async move { caller.submit_task_paid(&ready, &proof("q-seed")).await }
    });
    f.preflight.await_entered(1).await;

    let second = tokio::spawn({
        let caller = Arc::clone(&f.caller);
        let ready = ready.clone();
        async move { caller.submit_task_paid(&ready, &proof("q-seed")).await }
    });
    // The duplicate needs a chance to park before the decision ends.
    // Whether it actually parked is asserted below, never assumed: if it
    // raced past, it decided for itself and the preflight counter says
    // so.
    tokio::time::sleep(Duration::from_millis(300)).await;
    f.preflight.release();

    let a = first.await.expect("join");
    let b = second.await.expect("join");
    assert_eq!(
        f.preflight.entered(),
        1,
        "the duplicate decided for itself instead of parking, so this run never \
         exercised the waiter path"
    );

    let a_err = a.expect_err("the decider's claim write was armed to fail");
    let b_err = b.expect_err("the waiter must receive the decider's refusal");
    let a_schematic = refusal_schematic(&a_err);
    let b_schematic = refusal_schematic(&b_err);
    assert_eq!(
        a_schematic.reason, "journal_unavailable",
        "the decider's own verdict changed: {a_schematic:?}"
    );
    assert!(
        a_schematic.recovery.safe_to_retry,
        "a claim-write failure is retryable: {:?}",
        a_schematic.recovery
    );
    assert_eq!(
        (
            b_schematic.reason.as_str(),
            b_schematic.recovery.safe_to_retry
        ),
        (
            a_schematic.reason.as_str(),
            a_schematic.recovery.safe_to_retry
        ),
        "the waiter read a different verdict from the decider's"
    );
    assert_eq!(
        b_schematic.funds_moved, a_schematic.funds_moved,
        "the waiter read different money facts from the decider's"
    );
    assert_eq!(f.runs(), 0, "nothing ran on a failed claim");
    assert_eq!(
        f.record("converge").await.map(|r| r.state.tag()),
        Some(StateTag::Paid),
        "a failed claim leaves the admission paid, so the retry re-enters at S6′"
    );
}

// ===========================================================================
// R9 — the write completes, and publishes durably
// ===========================================================================

/// Aborting the future that started a store operation does not abandon
/// its write half-done: the I/O worker owns the transaction locks and
/// the lifetime handle until the rename has landed.
///
/// A `tokio::fs` write path continues on the blocking pool after its
/// awaiting future is dropped, while the guards it was holding are
/// released — so a successor can take the locks, reuse the temp path and
/// race the rename.
///
/// Inverse: publish from the awaiting future instead of the worker (have
/// `mutate_if`'s worker return the mutated state and call
/// `files.publish(&state)` after `blocking(..)`) — the aborted write
/// never lands and the "completed anyway" lookup fires.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_aborted_mutation_completes_its_write_under_its_own_guards() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("admissions.json");
    let journal = Arc::new(journal_at(&path).await);
    let terms = offer(true);
    let now = now_secs();

    let release = journal.hold_next_write();
    let task = tokio::spawn({
        let journal = Arc::clone(&journal);
        let candidate = candidate("aborted", "adm-A", &terms, now);
        async move { journal.admit_reserved(candidate, 4, now).await }
    });

    // Precondition: the write is demonstrably inside the worker.
    for _ in 0..500 {
        if journal.writers_in_flight() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        journal.writers_in_flight(),
        1,
        "the write never reached the I/O worker"
    );

    // The future that started it is gone.
    task.abort();
    assert!(
        task.await.is_err(),
        "the task must actually have been aborted"
    );

    // A successor's write cannot start while the worker still holds the
    // guards: it is queued, not interleaved.
    let second = tokio::spawn({
        let journal = Arc::clone(&journal);
        async move { journal.prune(now).await }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !second.is_finished(),
        "a second writer ran while the aborted write still held the transaction"
    );

    let _ = release.send(());
    second.await.expect("join").expect("the successor's prune");

    let landed = journal
        .lookup(owner(), "aborted")
        .await
        .expect("lookup")
        .expect("the aborted mutation's write completed anyway");
    assert_eq!(landed.admission_id.as_deref(), Some("adm-A"));
    assert_eq!(
        journal.durable_writes(),
        2,
        "both the aborted mutation's write and the successor's prune published, \
         in that order"
    );

    // The file is a whole journal, not a torn one — read by a successor
    // owner, so this is the bytes and not an in-memory artifact.
    drop(
        Arc::try_unwrap(journal)
            .map_err(|_| ())
            .expect("sole owner"),
    );
    let reopened = journal_at(&path).await;
    assert!(reopened
        .lookup(owner(), "aborted")
        .await
        .expect("lookup")
        .is_some());
}

/// Two journals whose filenames share a stem — `admissions.json` and
/// `admissions.backup` — never share a temp file.
///
/// `with_extension("tmp.<pid>")` replaces the extension instead of
/// extending the name, collapsing both onto `admissions.tmp.<pid>`,
/// while their `.owner` and `.lock` sidecars keep the full filename and
/// so never serialize the two against each other. One writer then
/// truncates the other's temp and renames a file it did not write.
///
/// Inverse: restore `self.path.with_extension(format!("tmp.{}",
/// std::process::id()))` with `create(true).truncate(true)` in
/// `JournalFiles::create_temp` — a round below surfaces a journal
/// carrying the sibling's records (or an unparseable one).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_same_stem_journals_never_share_a_temp_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let primary = dir.path().join("admissions.json");
    let backup = dir.path().join("admissions.backup");
    let a = Arc::new(journal_at(&primary).await);
    let b = Arc::new(journal_at(&backup).await);
    let terms = offer(true);
    let now = now_secs();

    // Enough records that a write is a real one: the window between
    // creating a temp file and renaming it is where the collision lives.
    for i in 0..40u64 {
        admit(
            a.as_ref(),
            candidate(&format!("a-{i}"), "adm-a", &terms, now),
            4096,
            now,
        )
        .await;
        admit(
            b.as_ref(),
            candidate(&format!("b-{i}"), "adm-b", &terms, now),
            4096,
            now,
        )
        .await;
    }

    for round in 0..24u64 {
        let (ra, rb) = tokio::join!(
            tokio::spawn({
                let journal = Arc::clone(&a);
                let candidate = candidate(&format!("a-x{round}"), "adm-a", &terms, now);
                async move { journal.admit_reserved(candidate, 4096, now).await }
            }),
            tokio::spawn({
                let journal = Arc::clone(&b);
                let candidate = candidate(&format!("b-x{round}"), "adm-b", &terms, now);
                async move { journal.admit_reserved(candidate, 4096, now).await }
            }),
        );
        ra.expect("join").expect("A's write");
        rb.expect("join").expect("B's write");

        // Each journal must contain its OWN records and none of the
        // sibling's: a shared temp file publishes the wrong bytes.
        for (journal, mine, theirs) in [(a.as_ref(), "a-x", "b-x"), (b.as_ref(), "b-x", "a-x")] {
            assert!(
                journal
                    .lookup(owner(), &format!("{mine}{round}"))
                    .await
                    .expect("lookup")
                    .is_some(),
                "round {round}: the {mine} journal lost its own write"
            );
            assert!(
                journal
                    .lookup(owner(), &format!("{theirs}{round}"))
                    .await
                    .expect("lookup")
                    .is_none(),
                "round {round}: the {mine} journal published the sibling's records"
            );
        }
    }
    assert_eq!(a.durable_writes(), 64, "A published every write");
    assert_eq!(b.durable_writes(), 64, "B published every write");
}

/// A durability barrier that fails **after** the rename is
/// `Ambiguous` — never `Io`, whose contract is "the file is untouched".
///
/// An `fsync` of the temp file commits its bytes; the directory entry
/// that makes it the journal is separate metadata. A caller told "I/O
/// error" concludes nothing happened, and acts on a store that already
/// moved.
///
/// Inverse: return `self.io_err(..)` from `JournalFiles::barrier`
/// instead of `A2aJournalError::Ambiguous` — the error-shape assertion
/// fires while the visibility assertion still passes, which is exactly
/// the misclassification.
#[tokio::test]
async fn a_failed_durability_barrier_is_ambiguous_not_untouched() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("admissions.json");
    let journal = journal_at(&path).await;
    let terms = offer(true);
    let now = now_secs();

    // The control: an armed *write* failure really does leave the file
    // untouched, so the two classifications are distinguishable.
    journal.fail_next_write();
    let untouched = journal
        .admit_reserved(candidate("io", "adm-io", &terms, now), 4, now)
        .await;
    assert!(
        matches!(untouched, Err(A2aJournalError::Io { .. })),
        "a failed write is Io: {untouched:?}"
    );
    assert!(
        journal
            .lookup(owner(), "io")
            .await
            .expect("lookup")
            .is_none(),
        "an Io failure must leave the store exactly as it was"
    );

    journal.fail_next_barrier();
    let ambiguous = journal
        .admit_reserved(candidate("amb", "adm-amb", &terms, now), 4, now)
        .await;
    assert!(
        matches!(ambiguous, Err(A2aJournalError::Ambiguous { .. })),
        "a post-rename durability failure must be Ambiguous, not {ambiguous:?}"
    );
    assert!(
        journal
            .lookup(owner(), "amb")
            .await
            .expect("lookup")
            .is_some(),
        "the write WAS published — an Ambiguous refusal never means untouched"
    );
    // And a successor owner reading the file sees it, which is what makes
    // "published" a fact about the bytes.
    drop(journal);
    let reopened = journal_at(&path).await;
    assert!(reopened
        .lookup(owner(), "amb")
        .await
        .expect("lookup")
        .is_some());
}

// ===========================================================================
// The atomic admit, at store level
// ===========================================================================

/// The ceiling is enforced in the transaction that writes, so concurrent
/// admissions of distinct task ids cannot both pass a ceiling of one.
///
/// The store-level half of the reviewer's real-wire overbooking probe:
/// same defect, no transport, and it names the verb.
///
/// Inverse: implement `admit_reserved` as `in_flight(..)` followed by
/// `insert_reserved(..)` in two `mutate` calls — the admitted count
/// rises above one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_admissions_of_distinct_tasks_cannot_pass_a_ceiling_of_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("admissions.json");
    let journal = Arc::new(journal_at(&path).await);
    let terms = offer(true);
    let now = now_secs();

    let mut tasks = Vec::new();
    for i in 0..8u64 {
        tasks.push(tokio::spawn({
            let journal = Arc::clone(&journal);
            let candidate = candidate(&format!("cap-{i}"), "adm", &terms, now);
            async move { journal.admit_reserved(candidate, 1, now).await }
        }));
    }
    let mut admitted = 0;
    let mut busy = 0;
    for t in tasks {
        match t.await.expect("join").expect("admit") {
            AdmitOutcome::Admitted(_) => admitted += 1,
            AdmitOutcome::Busy => busy += 1,
            AdmitOutcome::Existing(r) => panic!("distinct ids share no key: {r:?}"),
        }
    }
    assert_eq!(
        admitted, 1,
        "max_in_flight=1 admitted {admitted} reservations ({busy} busy)"
    );
    assert_eq!(busy, 7, "the other seven must be told the service is busy");
    assert_eq!(
        journal.in_flight(SERVICE, now).await.expect("in flight"),
        1,
        "exactly one admission holds capacity"
    );
}

/// A decision cannot be opened on an incarnation that is no longer under
/// the key; and while one is open, the slot is held whatever the
/// reservation clock says.
///
/// Inverse: replace `matching_mut(&identity)` with a plain key lookup in
/// `StoreState::open_decision` — the superseded assertion fires.
#[tokio::test]
async fn a_decision_opens_only_on_the_incarnation_it_names() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("admissions.json");
    let journal = journal_at(&path).await;
    let mut terms = offer(true);
    terms.bounds.max_in_flight = 1;
    terms.reservation_ttl_secs = 10;
    let now = now_secs();

    let a = admit(&journal, candidate("open", "adm-A", &terms, now), 1, now).await;
    assert_eq!(
        journal
            .prune(now + terms.reservation_retention_secs + 1)
            .await
            .expect("prune"),
        1
    );
    let b = admit(&journal, candidate("open", "adm-B", &terms, now), 1, now).await;

    let stale = journal.open_decision(&a, 1, now).await;
    assert!(
        matches!(stale, Err(A2aJournalError::Superseded { .. })),
        "a decision opened on a replacement record: {stale:?}"
    );

    match journal.open_decision(&b, 1, now).await.expect("open") {
        DecisionOutcome::Open(open) => {
            assert_eq!(open.generation, b.generation);
            assert!(open.deciding);
        }
        other => panic!("expected an open decision, got {other:?}"),
    }
    // A competing task finds the slot held...
    assert!(
        matches!(
            journal
                .admit_reserved(candidate("open-2", "adm-C", &terms, now), 1, now)
                .await
                .expect("admit"),
            AdmitOutcome::Busy
        ),
        "the open decision's slot was handed to a competitor"
    );
    // ...and still finds it held past the reservation's own expiry, which
    // is the R2 schedule at store level.
    let later = now + terms.reservation_ttl_secs + 1;
    assert!(
        matches!(
            journal
                .admit_reserved(candidate("open-2", "adm-C", &terms, later), 1, later)
                .await
                .expect("admit"),
            AdmitOutcome::Busy
        ),
        "an expiring reservation released the slot its decision was using"
    );
    // The control: once the decision closes, the slot is available —
    // otherwise the two `Busy` rows above proved nothing.
    journal
        .close_decision(&b, later)
        .await
        .expect("close the decision");
    assert!(
        matches!(
            journal
                .admit_reserved(candidate("open-2", "adm-C", &terms, later), 1, later)
                .await
                .expect("admit"),
            AdmitOutcome::Admitted(_)
        ),
        "a closed decision must release its slot"
    );
}

// ---------------------------------------------------------------------------
// R11 — a protected catalog is reachable by an authorized caller
// ---------------------------------------------------------------------------

/// A mesh with an adopted node authority owned by `owner`, announcing
/// fast enough for a test to converge.
///
/// Built through `MeshNode::new` + `Mesh::from_node_arc` rather than
/// `MeshBuilder` for exactly one reason: the default
/// `min_announce_interval` is 10 s, and owner-scoped discovery ships on
/// the announce path. `shared_audience` models the out-of-band
/// pre-staging owner-scoped discovery requires — it is keyed on ONE
/// per-organization audience, so two independently adopted nodes each
/// minting their own could never open each other's envelopes.
async fn org_mesh(
    tag: &str,
    owner: &OrgKeypair,
    shared_audience: Option<&OwnerAudienceCredential>,
) -> (Mesh, Identity, std::path::PathBuf) {
    let identity = Identity::generate();
    let mut cfg = MeshNodeConfig::new("127.0.0.1:0".parse().expect("addr"), PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5));
    cfg.min_announce_interval = Duration::from_millis(50);
    cfg.configured_identity = true;

    let mut node = MeshNode::new((**identity.keypair()).clone(), cfg)
        .await
        .expect("MeshNode::new");
    let channel_configs = Arc::new(ChannelConfigRegistry::new());
    node.set_channel_configs(channel_configs.clone());
    let node = Arc::new(node);

    let entity = identity.entity_id().clone();
    let cert = OrgMembershipCert::try_issue(owner, entity.clone(), 1, 3600).expect("cert");
    let dir = std::env::temp_dir().join(format!(
        "net-a2a-org-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let authority = NodeAuthority::adopt(&dir, cert, &entity, 0, None).expect("adopt");
    let authority = match shared_audience {
        None => authority,
        Some(shared) => NodeAuthority {
            config: authority.config.clone(),
            audience: OwnerAudienceCredential::decode_config(&shared.encode_config())
                .expect("decode shared owner audience"),
            revocation: authority.revocation.clone(),
        },
    };
    node.install_node_authority(Arc::new(authority))
        .expect("install authority");
    node.set_owner_cert_emission(true)
        .expect("enable owner-cert emission");

    (
        Mesh::from_node_arc(node, channel_configs, Some(identity.clone())),
        identity,
        dir,
    )
}

/// Handshake `caller` to `provider`, start both, and wait for entity
/// pins in both directions — the core refuses to publish an admission
/// proof to a target it cannot prove is the provider.
async fn bring_up(caller: &Mesh, provider: &Mesh) {
    let provider_pub = *provider.public_key();
    let provider_addr = provider.local_addr();
    let caller_id = caller.node_id();
    let p = provider.node_arc();
    let accept = tokio::spawn(async move { p.accept(caller_id).await });
    caller
        .connect(
            &provider_addr.to_string(),
            &provider_pub,
            provider.node_id(),
        )
        .await
        .expect("connect");
    accept.await.expect("accept task").expect("accept");
    caller.start();
    provider.start();
    for m in [caller, provider] {
        m.inner()
            .announce_capabilities(CapabilitySet::new())
            .await
            .expect("announce");
    }
    for _ in 0..200 {
        if caller.inner().peer_entity_id(provider.node_id()).is_some()
            && provider.inner().peer_entity_id(caller_id).is_some()
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("entity pins were not established in both directions");
}

/// Membership + dispatcher for `member` acting for `org`.
fn belonging_for(
    org: &OrgKeypair,
    member: net::adapter::net::identity::EntityId,
) -> (OrgMembershipCert, OrgDispatcherGrant) {
    let cert = OrgMembershipCert::try_issue(org, member.clone(), 1, 3600).expect("cert");
    let grant =
        OrgDispatcherGrant::try_issue(org, member, DispatcherScope::Any, 3600).expect("dispatcher");
    (cert, grant)
}

/// Completes with the shared `RESULT`, counting runs with multiplicity.
struct CountingExecutor {
    runs: Arc<AtomicU64>,
}

#[async_trait::async_trait]
impl TaskExecutor for CountingExecutor {
    async fn run(&self, _brief: TaskBrief, _cancel: CancelToken) -> Result<String, String> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        Ok(RESULT.to_string())
    }
}

/// **R11.** A provider serving its catalog PROTECTED is reachable by an
/// authorized organization caller, through the whole lifecycle.
///
/// The finding this closes is not an authorization bypass — it is the
/// opposite. `A2aPrincipal::OrgAdmitted` registers all five services
/// PROTECTED, and every A2A requester verb built its call options with a
/// deadline and payment headers and **no proof intent**, while core
/// defaults `org_proof_intent` to `None` and signs only when one is
/// supplied. So an operator could configure a protected principal and
/// then have no native way to call it: correctly refused, permanently.
///
/// The pre-state control runs first and is what makes the rest mean
/// anything: the same caller, the same target, the same verb, with no
/// identity installed, is **refused**. If the service were not really
/// protected, that row would pass and the success below would prove
/// nothing.
///
/// A registration-failure test does not prove invocation, so this drives
/// describe → prepare → submit → status → cancel over a real two-node
/// wire and reads the provider's own record back.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_installed_org_identity_reaches_a_protected_a2a_lifecycle() {
    let org = OrgKeypair::from_bytes([0xA1u8; 32]);
    let (provider, _p_identity, p_dir) = org_mesh("prov", &org, None).await;
    // The out-of-band pre-staging step: one owner audience per
    // organization, so the caller can open the provider's scoped
    // announcement at all.
    let shared = OwnerAudienceCredential::decode_config(
        &provider
            .inner()
            .node_authority()
            .expect("authority")
            .audience
            .encode_config(),
    )
    .expect("decode");
    let (caller, c_identity, c_dir) = org_mesh("call", &org, Some(&shared)).await;

    let runs = Arc::new(AtomicU64::new(0));
    let serving = provider
        .serve_a2a_configured(
            TaskRegistry::new(),
            Arc::new(CountingExecutor {
                runs: Arc::clone(&runs),
            }),
            A2aServiceConfig::new(BTreeMap::from([(
                SERVICE.to_string(),
                A2aServicePolicy::Free(offer(false)),
            )]))
            .with_principal(A2aPrincipal::OrgAdmitted(OrgAccess::SameOrg)),
        )
        .expect("a protected free catalog serves");
    bring_up(&caller, &provider).await;
    let target = provider.node_id();

    // ---- pre-state control: PROTECTED really is protected ----
    let unadmitted = caller
        .describe_a2a(target)
        .await
        .expect_err("a session-peer call must not be admitted to a protected catalog");
    assert!(
        matches!(unadmitted, A2aFlowError::Transport(_)),
        "expected the provider's admission denial, got {unadmitted:?}"
    );

    // ---- install the identity ----
    let (cert, dispatcher) = belonging_for(&org, c_identity.entity_id().clone());
    let credentials =
        OrgCredentials::new(cert, dispatcher, vec![], vec![]).expect("credentials assemble");
    let org_client = Arc::new(caller.org(credentials).expect("bind"));
    caller.set_a2a_org_caller(Some(Arc::clone(&org_client)));

    // Owner-scoped discovery ships on the announce path, so converge by
    // OUTCOME: re-announce and retry the verb until the caller's own
    // authority view resolves the provider.
    let mut offers = None;
    for _ in 0..200 {
        provider
            .inner()
            .announce_capabilities(CapabilitySet::new())
            .await
            .ok();
        if let Ok(found) = caller.describe_a2a(target).await {
            offers = Some(found);
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let offers = offers.expect("an authorized org caller must reach the protected catalog");
    assert_eq!(offers.len(), 1, "{offers:?}");
    assert_eq!(offers[0].service_id, SERVICE);

    // ---- prepare ----
    let b = brief("org-task-1");
    let reply = caller
        .prepare_a2a(target, &b)
        .await
        .expect("prepare over a protected service");
    let reservation = match reply {
        PrepareReply::Reservation(r) => r,
        other => panic!("expected a reservation, got {other:?}"),
    };
    assert_eq!(reservation.task_id, "org-task-1");

    // ---- submit (free catalog: no payment evidence) ----
    let ack = caller
        .submit_task(target, &b)
        .await
        .expect("submit over a protected service");
    assert!(ack.accepted, "{ack:?}");

    // ---- status: attributed to the ENTITY the proof names ----
    let mut record = None;
    for _ in 0..200 {
        if let Ok(Some(found)) = caller.task_status(target, "org-task-1").await {
            if found.state.is_terminal() {
                record = Some(found);
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let record = record.expect("the admitted caller reads its own task");
    assert_eq!(
        record.state,
        TaskState::Completed {
            result_ref: RESULT.to_string()
        }
    );
    assert_eq!(runs.load(Ordering::SeqCst), 1, "the work ran exactly once");
    // The durable record is keyed on the entity the admission proof
    // names, never on the delivering peer — which is the whole point of
    // the `OrgAdmitted` principal.
    let stored = serving
        .store
        .lookup(
            TaskOwner::Entity(*c_identity.entity_id().as_bytes()),
            "org-task-1",
        )
        .await
        .expect("lookup")
        .expect("the admission is stored under the admitted entity");
    assert_eq!(stored.task_id, "org-task-1");

    // ---- cancel: reachable too, and terminal work answers false ----
    let cancelled = caller
        .cancel_task(target, "org-task-1")
        .await
        .expect("cancel over a protected service");
    assert!(!cancelled, "a completed task has nothing to stop");

    // ---- control: clearing the identity restores the refusal, so the
    // success above was the identity and not a provider that opened up
    // ----
    caller.set_a2a_org_caller(None);
    let cleared = caller
        .describe_a2a(target)
        .await
        .expect_err("without an identity the protected catalog is unreachable again");
    assert!(
        matches!(cleared, A2aFlowError::Transport(_)),
        "expected the provider's admission denial, got {cleared:?}"
    );

    drop(serving);
    let _ = std::fs::remove_dir_all(&p_dir);
    let _ = std::fs::remove_dir_all(&c_dir);
}

/// **R11, fail-loud.** With an identity installed, a target that is not
/// an authorized provider of the service is a **local** refusal naming
/// the organization problem — never a silent downgrade to a session-peer
/// call.
///
/// The alternative would turn a credential problem into a remote
/// admission denial (or, against a public provider, into an unprotected
/// call the operator did not ask for). Nothing is sent: the assertion is
/// that the verb returns before any round trip could complete.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_installed_org_identity_refuses_an_unauthorized_target_locally() {
    let org = OrgKeypair::from_bytes([0xA1u8; 32]);
    let (caller, c_identity, c_dir) = org_mesh("solo", &org, None).await;
    let (cert, dispatcher) = belonging_for(&org, c_identity.entity_id().clone());
    let credentials = OrgCredentials::new(cert, dispatcher, vec![], vec![]).expect("credentials");
    let org_client = Arc::new(caller.org(credentials).expect("bind"));
    caller.set_a2a_org_caller(Some(Arc::clone(&org_client)));

    // Node 7 is nobody: no pinned entity, no discovered capability.
    let started = std::time::Instant::now();
    let err = caller
        .describe_a2a(7)
        .await
        .expect_err("an unauthorized target must be refused locally");
    match err {
        A2aFlowError::OrgAdmission(detail) => assert!(
            detail.contains("no authorized provider"),
            "the refusal must name the authority problem so an operator knows where to \
             look: {detail}"
        ),
        other => panic!(
            "expected a local OrgAdmission refusal; anything else means the call was \
             issued as an ordinary session peer: {other:?}"
        ),
    }
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "the refusal waited on the network instead of refusing locally"
    );

    // Control: with the identity cleared, the SAME target fails as
    // transport instead — so the arm above is about the missing
    // authorization, not about node 7 being unreachable.
    caller.set_a2a_org_caller(None);
    let cleared = caller
        .describe_a2a(7)
        .await
        .expect_err("node 7 is unreachable either way");
    assert!(
        matches!(cleared, A2aFlowError::Transport(_) | A2aFlowError::Timeout),
        "expected a transport failure once the identity is cleared, got {cleared:?}"
    );

    let _ = std::fs::remove_dir_all(&c_dir);
}

// ---------------------------------------------------------------------------
// The free inline path, and exact historical identity
// ---------------------------------------------------------------------------

/// A free inline admission, as S5a builds one: no `admission_id`,
/// because nothing can be purchased against a free offer.
fn inline_candidate(task_id: &str, offer: &A2aOffer, now: u64) -> AdmissionRecord {
    let b = brief(task_id);
    AdmissionRecord::reserved(
        owner(),
        b.clone(),
        offer,
        None,
        task_commitment(offer, &b),
        now,
    )
}

/// A free offer with one slot and a one-second reservation.
fn narrow_free() -> A2aOffer {
    let mut offer = offer(false);
    offer.bounds.max_in_flight = 1;
    offer.reservation_ttl_secs = 1;
    offer
}

/// **A launch claim cannot use a pre-await clock on a slot somebody else
/// now holds** — the free inline path's half of the reacquisition
/// finding.
///
/// The submit path samples its clock, then awaits a lookup, a ledger
/// read and the application preflight before it inserts the inline
/// reservation and claims the launch. By the time the claim lands, that
/// sample can be older than the whole reservation window it is arguing
/// from — and a competitor can have taken the one slot in between. The
/// claim checked `holds_capacity` against the sample it was given, so it
/// started work on a slot it did not hold: two admissions inside a
/// published ceiling of one.
///
/// Refused instead, and retryably: the store decides the slot against a
/// floor of its own (no live row of this service can have been stamped
/// in the future), so a stale sample can never revive a released
/// window.
///
/// Inverse checked: with the claim deciding on the caller's raw sample
/// again, the claim returns `Ok` and `in_flight` reads **2** against a
/// ceiling of 1.
#[tokio::test]
async fn a_free_inline_claim_cannot_launch_on_a_slot_a_competitor_took() {
    let dir = tempfile::tempdir().expect("tempdir");
    let journal = journal_at(&dir.path().join("inline.json")).await;
    let free = narrow_free();
    let t = now_secs();

    // S5a, with the clock the submit sampled before its awaits.
    let a = admit(&journal, inline_candidate("inline-A", &free, t), 1, t).await;
    assert!(
        a.admission_id.is_none(),
        "a free inline admission mints none"
    );

    // Real time moved: A's window lapsed and the only slot went to a
    // competitor, through the same transactional admission.
    let b = admit(
        &journal,
        inline_candidate("inline-B", &free, t + 2),
        1,
        t + 2,
    )
    .await;
    assert_eq!(
        journal.in_flight(SERVICE, t + 2).await.expect("in flight"),
        1,
        "B holds the one slot"
    );

    let claimed = journal.claim_launch(owner(), "inline-A", t).await;
    let err = claimed.expect_err("a claim on a slot B holds must be refused");
    assert!(
        matches!(&err, A2aJournalError::Conflict { reason, .. } if reason.contains("capacity slot")),
        "the refusal must name the slot, so a retry knows it is retryable: {err:?}"
    );
    assert_eq!(
        journal.in_flight(SERVICE, t + 2).await.expect("in flight"),
        1,
        "the ceiling of one still holds exactly one admission"
    );
    assert_eq!(
        journal.lookup(owner(), "inline-A").await.expect("lookup"),
        Some(a),
        "a refused claim writes nothing: A is exactly as it was"
    );
    assert!(
        !journal
            .ledger_has(owner(), "inline-A")
            .await
            .expect("ledger"),
        "and nothing entered the never-pruned launch ledger"
    );
    // B is untouched too, so the refusal was about the slot and not
    // about this store refusing every claim.
    assert_eq!(
        journal
            .claim_launch_exact(&b, t + 2)
            .await
            .expect("B may launch on the slot it holds")
            .task_id,
        "inline-B"
    );
}

/// **The free inline admission holds its slot from insert to claim**,
/// in one continuous hold rather than two transactions with a gap.
///
/// The same schedule as above, with the decision the submit path opens
/// on the record it just inserted. The hold is what carries the slot, so
/// the competitor is told `Busy` — nothing was written for it — and the
/// claim lands even though the reservation clock lapsed underneath it.
/// Either way exactly one admission is inside the ceiling; what changes
/// is *which*, and that a caller is never handed a hold the service
/// cannot honour.
///
/// Inverse checked: without the hold, the competitor is admitted (the
/// first leg's schedule) and the ceiling depends on the claim refusing.
#[tokio::test]
async fn a_free_inline_hold_keeps_its_slot_from_insert_to_claim() {
    let dir = tempfile::tempdir().expect("tempdir");
    let journal = journal_at(&dir.path().join("inline-hold.json")).await;
    let free = narrow_free();
    let t = now_secs();

    let a = admit(&journal, inline_candidate("inline-A", &free, t), 1, t).await;
    let held = match journal.open_decision(&a, 1, t).await.expect("open") {
        DecisionOutcome::Open(open) => *open,
        DecisionOutcome::Busy => panic!("nothing else holds the slot"),
    };
    assert!(held.deciding, "the inline admission holds its own slot");

    assert!(
        matches!(
            journal
                .admit_reserved(inline_candidate("inline-B", &free, t + 2), 1, t + 2)
                .await,
            Ok(AdmitOutcome::Busy)
        ),
        "a lapsed reservation under an open decision still owns its slot"
    );
    assert_eq!(
        journal.lookup(owner(), "inline-B").await.expect("lookup"),
        None,
        "a Busy admission writes nothing"
    );

    journal
        .claim_launch_exact(&held, t)
        .await
        .expect("the hold carries the claim across the lapsed clock");
    assert_eq!(
        journal.in_flight(SERVICE, t + 2).await.expect("in flight"),
        1,
        "one launched admission, inside the ceiling of one"
    );
}

/// **An operator resolution must name the incarnation it closes**, and a
/// refusal must leave every charge exactly where it was.
///
/// One `(owner, task id)` can carry two financial records: a redemption
/// retained against a superseded admission, and the live replacement
/// that took its place. They are two charges. A call that names only the
/// key cannot say which one the operator established an outcome for, and
/// picking either one closes a charge nobody asked about — so it is
/// refused, naming both candidates with their generations.
///
/// The exact selector is then the usable exit: the identity the
/// unresolved queue reports for the **historical** record closes that
/// record, and the live replacement is byte-identical afterwards.
///
/// Inverse checked: with the ambiguity refusal removed, the key-only
/// call returns `Ok` and terminalizes the **live** row, quote and payer
/// included.
#[tokio::test]
async fn an_ambiguous_key_resolves_only_the_incarnation_it_names() {
    let dir = tempfile::tempdir().expect("tempdir");
    let journal = journal_at(&dir.path().join("resolve.json")).await;
    let terms = offer(true);
    let t = now_secs();

    // A is paid, then its row ages out of reservation retention and is
    // retained as detached evidence; B replaces it at the same key and
    // is paid in its own right.
    let a = admit(&journal, candidate("same-key", "adm-A", &terms, t), 4, t).await;
    let retained = t + terms.reservation_retention_secs + 1;
    assert_eq!(journal.prune(retained).await.expect("prune"), 1);
    let b = admit(&journal, candidate("same-key", "adm-B", &terms, t), 4, t).await;
    assert!(matches!(
        journal.redeem(&a, "quote-A".into(), PAYER, t).await,
        Err(A2aJournalError::Superseded { .. })
    ));
    journal
        .redeem(&b, "quote-B".into(), PAYER, t)
        .await
        .expect("B is paid against its own admission");

    let queue = journal.unresolved().await.expect("unresolved");
    assert_eq!(queue.len(), 2, "both charges are on the operator's queue");
    let live_before = journal
        .lookup(owner(), "same-key")
        .await
        .expect("lookup")
        .expect("the live replacement");

    let refused = journal
        .resolve(
            owner(),
            "same-key",
            TaskState::Failed {
                error: "refunded the quote-A charge".into(),
            },
            t,
        )
        .await
        .expect_err("a key names two charges here");
    let text = refused.to_string();
    assert!(
        text.contains("adm-A") && text.contains("adm-B"),
        "the refusal must name both candidates so the operator can choose: {text}"
    );
    assert_eq!(
        journal
            .lookup(owner(), "same-key")
            .await
            .expect("lookup")
            .as_ref(),
        Some(&live_before),
        "the refusal left the live charge byte-identical"
    );
    assert_eq!(
        journal.unresolved().await.expect("unresolved").len(),
        2,
        "and closed neither"
    );

    // The historical incarnation, addressed by the identity the queue
    // reported for it.
    let historical = queue
        .iter()
        .find(|r| r.admission_id.as_deref() == Some("adm-A"))
        .expect("A is on the queue");
    journal
        .resolve_exact(
            &historical.identity(),
            TaskState::Failed {
                error: "refunded the quote-A charge".into(),
            },
            t,
        )
        .await
        .expect("the exact historical charge closes");

    assert_eq!(
        journal
            .lookup(owner(), "same-key")
            .await
            .expect("lookup")
            .as_ref(),
        Some(&live_before),
        "closing the historical charge must not touch the live replacement"
    );
    let left = journal.unresolved().await.expect("unresolved");
    assert_eq!(left.len(), 1, "exactly one charge was closed");
    assert_eq!(
        left[0].admission_id.as_deref(),
        Some("adm-B"),
        "and it was the one that was named"
    );
    // The same identity a second time is refused rather than rewriting
    // an operator's own disposition.
    assert!(
        journal
            .resolve_exact(&historical.identity(), TaskState::Cancelled, t)
            .await
            .is_err(),
        "a resolved charge is not resolvable again"
    );
}
