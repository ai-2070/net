//! The configured A2A serving path: free-or-paid by provider
//! configuration, admission before money, and exactly one execution per
//! purchase (`A2A_PAID_ADMISSION_PLAN.md` D1–D6, WS-C).
//!
//! Every witness here is about something a provider's money or a
//! caller's work depends on:
//!
//! - a paid service never degrades to free, and a free service is still
//!   governed by policy (preflight + capacity);
//! - validation and the application preflight run **before a quote can
//!   exist**, so a refusal has nothing to reconcile;
//! - a refusal *after* a payment may have landed is reconciliation, kept
//!   for an operator, never an unpaid rejection;
//! - a gate denial keeps the reservation (it is an attempt note, not a
//!   state change) so a fixed payment retries the same admission;
//! - the launch claim is durable before the spawn, so a failed write
//!   runs nothing and a retry runs exactly once;
//! - one payment admits one reservation of one owner's one brief — a
//!   proof replayed by another peer buys nothing.
//!
//! The gate and the preflight are scripted (`net-payments` tests cover
//! the engine-backed gate), the executor counts its runs and can be held
//! on a barrier, and concurrency is staged on that barrier rather than
//! on sleeps.
//!
//! Store-level properties — the transition table itself, the three
//! retention classes, the journal's ownership lock across a file
//! replacement — are witnessed in `a2a_admission_journal.rs` and are not
//! repeated here. What this file adds is that **each handler path
//! performs the table's transition and no other**.

#![cfg(all(feature = "net", feature = "cortex", feature = "testing"))]

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use net_sdk::a2a::{
    purchase_hash, task_commitment, A2aBounds, A2aOffer, AdmissionReservation, CancelToken,
    PrepareReply, PreparedTask, TaskAck, TaskBrief, TaskExecutor, TaskOwner, TaskRecord,
    TaskRegistry, TaskState, TERMINAL_RECORD_TTL_SECS,
};
use net_sdk::a2a_journal::{
    now_secs, A2aAdmissionJournal, A2aJournalError, AdmissionRecord, AdmissionState,
    AdmissionStore, StateTag, DETAIL_ADMISSION_REVOKED, DETAIL_OUTCOME_UNKNOWN,
    DETAIL_PAID_NOT_STARTED,
};
use net_sdk::a2a_payment::{
    TaskAdmissionGate, TaskPaymentClaim, TaskPaymentEvidence, TaskPaymentProof,
};
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::mesh_a2a::{
    A2aPrincipal, A2aServiceConfig, A2aServicePolicy, A2aServing, TaskPreflight, A2A_TASK_SERVICE,
};
use net_sdk::mesh_rpc::{CallOptions, RpcError, ServeError};
use net_sdk::org::OrgAccess;
use net_sdk::tool_payment::{
    failure_vocab, FailureSchematic, GateDenial, Recovery, ERR_PAYMENT, HDR_FAILURE_SCHEMATIC,
    HDR_PAYMENT_BINDING, HDR_PAYMENT_QUOTE, TAG_PAYMENT_FAILURE,
};

const PSK: [u8; 32] = [0x71u8; 32];
const SERVICE: &str = "svc.summarize";
const OTHER_SERVICE: &str = "svc.translate";
const REV: &str = "r1";
const TERMS: &str = r#"{"object":"net.pricing.terms@1"}"#;
const PAYER: [u8; 32] = [0xABu8; 32];
const RESULT: &str = "blob://summary-42";
/// A stand-in for the caller's ed25519 binding signature. The scripted
/// gate checks presence and the purchase hash; signature verification is
/// the engine's job, witnessed in `net-payments`.
const BINDING: [u8; 64] = [7u8; 64];

// ---------------------------------------------------------------------------
// Offers and briefs
// ---------------------------------------------------------------------------

fn offer_named(service_id: &str, paid: bool) -> A2aOffer {
    A2aOffer {
        service_id: service_id.to_string(),
        revision: REV.to_string(),
        description: Some("summarize a filing".to_string()),
        pricing_terms: paid.then(|| TERMS.to_string()),
        bounds: A2aBounds {
            max_prompt_bytes: 256,
            max_context_refs: 4,
            max_tags: 4,
            max_tag_bytes: 32,
            max_in_flight: 4,
        },
        reservation_ttl_secs: 300,
        reservation_retention_secs: 7 * 24 * 60 * 60,
        retention_secs: 60 * 60,
    }
}

fn offer(paid: bool) -> A2aOffer {
    offer_named(SERVICE, paid)
}

fn brief_id(task_id: &str, prompt: &str) -> TaskBrief {
    TaskBrief::new(prompt)
        .with_task_id(task_id)
        .with_service(SERVICE, REV)
}

fn brief(task_id: &str) -> TaskBrief {
    brief_id(task_id, "summarize the quarterly filings")
}

fn catalog(policies: Vec<A2aServicePolicy>) -> BTreeMap<String, A2aServicePolicy> {
    policies
        .into_iter()
        .map(|p| (p.offer().service_id.clone(), p))
        .collect()
}

// ---------------------------------------------------------------------------
// The scripted executor: a run counter plus a barrier
// ---------------------------------------------------------------------------

/// The host's runner, instrumented. `runs` counts **executions with
/// multiplicity** (never a set — this file's whole subject is
/// exactly-once), `started` is signalled as each run begins, `hold` is
/// the barrier a gated executor waits on, and `finished` is signalled
/// after it is released.
#[derive(Clone)]
struct ExecProbe {
    runs: Arc<AtomicU64>,
    started: Arc<tokio::sync::Semaphore>,
    hold: Arc<tokio::sync::Semaphore>,
    finished: Arc<tokio::sync::Semaphore>,
    gated: bool,
}

impl ExecProbe {
    fn new(gated: bool) -> Self {
        Self {
            runs: Arc::new(AtomicU64::new(0)),
            started: Arc::new(tokio::sync::Semaphore::new(0)),
            hold: Arc::new(tokio::sync::Semaphore::new(0)),
            finished: Arc::new(tokio::sync::Semaphore::new(0)),
            gated,
        }
    }

    fn runs(&self) -> u64 {
        self.runs.load(Ordering::SeqCst)
    }

    fn executor(&self) -> Arc<dyn TaskExecutor> {
        Arc::new(self.clone())
    }

    /// Block until a run has begun — a precondition, never a sleep.
    async fn wait_started(&self) {
        let permit = tokio::time::timeout(Duration::from_secs(10), self.started.acquire())
            .await
            .expect("an executor run never started")
            .expect("semaphore");
        permit.forget();
    }

    /// Release `n` held runs.
    fn release(&self, n: usize) {
        self.hold.add_permits(n);
    }

    /// Block until a released run has returned.
    async fn wait_finished(&self) {
        let permit = tokio::time::timeout(Duration::from_secs(10), self.finished.acquire())
            .await
            .expect("an executor run never finished")
            .expect("semaphore");
        permit.forget();
    }
}

#[async_trait::async_trait]
impl TaskExecutor for ExecProbe {
    async fn run(&self, _brief: TaskBrief, _cancel: CancelToken) -> Result<String, String> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        self.started.add_permits(1);
        if self.gated {
            self.hold.acquire().await.expect("barrier").forget();
        }
        self.finished.add_permits(1);
        Ok(RESULT.to_string())
    }
}

// ---------------------------------------------------------------------------
// The scripted gate
// ---------------------------------------------------------------------------

/// What one redemption saw.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GateCall {
    tool_id: String,
    quote_id: String,
    /// The purchase hash the PROVIDER expected — the value the gate is
    /// asked to match, never one read off the request.
    expected: String,
    has_binding: bool,
}

/// One issued quote, as the engine would hold it.
#[derive(Debug, Clone)]
struct IssuedQuote {
    input_hash: String,
    payer: [u8; 32],
    /// A scripted denial reason, for the gate-denial witnesses.
    deny: Option<String>,
}

/// The engine stand-in: records every call and admits only a quote whose
/// own committed input hash is the one the provider expects.
#[derive(Default)]
struct RecordingTaskGate {
    calls: parking_lot::Mutex<Vec<GateCall>>,
    quotes: parking_lot::Mutex<HashMap<String, IssuedQuote>>,
}

impl RecordingTaskGate {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Issue `quote_id` against `input_hash`, paid by `payer`.
    fn issue(&self, quote_id: &str, input_hash: &str, payer: [u8; 32]) {
        self.quotes.lock().insert(
            quote_id.to_string(),
            IssuedQuote {
                input_hash: input_hash.to_string(),
                payer,
                deny: None,
            },
        );
    }

    /// Issue a quote the gate will refuse with `reason`.
    fn issue_denied(&self, quote_id: &str, input_hash: &str, reason: &str) {
        self.quotes.lock().insert(
            quote_id.to_string(),
            IssuedQuote {
                input_hash: input_hash.to_string(),
                payer: PAYER,
                deny: Some(reason.to_string()),
            },
        );
    }

    fn calls(&self) -> Vec<GateCall> {
        self.calls.lock().clone()
    }

    fn call_count(&self) -> usize {
        self.calls.lock().len()
    }
}

fn denial(reason: &str, message: &str, quote_id: &str, tool_id: &str) -> GateDenial {
    GateDenial {
        schematic: FailureSchematic {
            object: TAG_PAYMENT_FAILURE.to_string(),
            code: failure_vocab::CODE_PAYMENT.to_string(),
            stage: failure_vocab::STAGE_REDEEM.to_string(),
            reason: reason.to_string(),
            message: message.to_string(),
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
        message: message.to_string(),
    }
}

#[async_trait::async_trait]
impl TaskAdmissionGate for RecordingTaskGate {
    async fn redeem(&self, claim: TaskPaymentClaim<'_>) -> Result<TaskPaymentEvidence, GateDenial> {
        self.calls.lock().push(GateCall {
            tool_id: claim.tool_id.to_string(),
            quote_id: claim.quote_id.to_string(),
            expected: claim.expected_input_hash.to_string(),
            has_binding: !claim.binding.is_empty(),
        });
        let issued = self.quotes.lock().get(claim.quote_id).cloned();
        let Some(issued) = issued else {
            return Err(denial(
                "unknown_quote",
                "no such quote",
                claim.quote_id,
                claim.tool_id,
            ));
        };
        if let Some(reason) = issued.deny.as_deref() {
            return Err(denial(
                reason,
                "scripted denial",
                claim.quote_id,
                claim.tool_id,
            ));
        }
        // The engine's `input_binding_mismatch` arm, which is what makes
        // one peer's proof worthless against another's reservation.
        if issued.input_hash != claim.expected_input_hash {
            return Err(denial(
                "input_binding_mismatch",
                "this quote commits to a different purchase",
                claim.quote_id,
                claim.tool_id,
            ));
        }
        Ok(TaskPaymentEvidence {
            quote_id: claim.quote_id.to_string(),
            payer: issued.payer,
        })
    }
}

// ---------------------------------------------------------------------------
// The scripted preflight
// ---------------------------------------------------------------------------

/// The application's admission check, scriptable per test.
#[derive(Default)]
struct ScriptedPreflight {
    calls: AtomicU64,
    refusal: parking_lot::Mutex<Option<String>>,
}

impl ScriptedPreflight {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    fn refuse(&self, reason: &str) {
        *self.refusal.lock() = Some(reason.to_string());
    }
    fn allow(&self) {
        *self.refusal.lock() = None;
    }
    fn calls(&self) -> u64 {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl TaskPreflight for ScriptedPreflight {
    async fn preflight(
        &self,
        _owner: TaskOwner,
        _offer: &A2aOffer,
        _brief: &TaskBrief,
    ) -> Result<(), String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match self.refusal.lock().clone() {
            Some(reason) => Err(reason),
            None => Ok(()),
        }
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

async fn mesh() -> Mesh {
    MeshBuilder::new("127.0.0.1:0", &PSK)
        .expect("builder")
        .build()
        .await
        .expect("build")
}

/// Handshake every caller to `provider`, then start all dispatch loops
/// (accepts must precede `start`, or the loop races the responder
/// handshake — the `a2a_task_ownership` idiom).
async fn connect_all(provider: &Mesh, callers: &[&Mesh]) {
    let addr = provider.inner().local_addr();
    let pubkey = *provider.inner().public_key();
    let nid_provider = provider.inner().node_id();
    for caller in callers {
        let nid_caller = caller.inner().node_id();
        let (accepted, connected) = tokio::join!(provider.inner().accept(nid_caller), async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            caller.inner().connect(addr, &pubkey, nid_provider).await
        });
        accepted.expect("accept");
        connected.expect("connect");
    }
    for caller in callers {
        caller.start();
    }
    provider.start();
}

/// A server refusal, both renderings: the wire status + human message,
/// and the schematic decoded under the producer/consumer discipline
/// (exactly one valid header, else `None`).
#[derive(Debug)]
struct Refusal {
    status: u16,
    message: String,
    /// Boxed for the same reason `A2aFlowError::PaymentRefused` boxes
    /// it: the schematic dwarfs the rest of the refusal.
    schematic: Option<Box<FailureSchematic>>,
}

impl Refusal {
    /// The schematic's reason, or a panic naming what actually came
    /// back — a refusal witness that accepted "some error" would pass on
    /// the wrong one.
    fn reason(&self) -> String {
        match self.schematic.as_ref() {
            Some(s) => s.reason.clone(),
            None => panic!("refusal carried no schematic: {self:?}"),
        }
    }

    fn schematic(&self) -> &FailureSchematic {
        self.schematic
            .as_deref()
            .unwrap_or_else(|| panic!("refusal carried no schematic: {self:?}"))
    }
}

/// The A2A wire envelope: the payload rides as a JSON array of bytes
/// (`call_typed` with `Req = Resp = Vec<u8>`), which the raw path has to
/// apply by hand to read reply headers.
fn envelope(body: &[u8]) -> Vec<u8> {
    serde_json::to_vec(&body.to_vec()).expect("envelope")
}

fn unwrap_envelope(raw: &[u8]) -> Vec<u8> {
    serde_json::from_slice(raw).expect("reply envelope")
}

/// A bounded call with request headers. A transport failure is retried
/// (the first cross-node call can lose its reply before the per-caller
/// reply subscription propagates); a **server refusal is terminal** and
/// returns both renderings.
async fn call_raw(
    caller: &Mesh,
    target: u64,
    service: &str,
    body: Vec<u8>,
    headers: Vec<(String, Vec<u8>)>,
) -> Result<Vec<u8>, Refusal> {
    let mut last = String::new();
    for _ in 0..8 {
        let opts = CallOptions {
            request_headers: headers.clone(),
            ..CallOptions::default()
        };
        match tokio::time::timeout(
            Duration::from_secs(5),
            caller.call(target, service, bytes::Bytes::from(body.clone()), opts),
        )
        .await
        {
            Ok(Ok(reply)) => return Ok(unwrap_envelope(&reply.body)),
            Ok(Err(RpcError::ServerError {
                status,
                message,
                headers,
            })) => {
                let entries: Vec<&Vec<u8>> = headers
                    .iter()
                    .filter(|(name, _)| name.eq_ignore_ascii_case(HDR_FAILURE_SCHEMATIC))
                    .map(|(_, value)| value)
                    .collect();
                let schematic = match entries.as_slice() {
                    [bytes] => FailureSchematic::from_header_bytes(bytes).map(Box::new),
                    _ => None,
                };
                return Err(Refusal {
                    status,
                    message,
                    schematic,
                });
            }
            Ok(Err(e)) => last = format!("rpc error: {e:?}"),
            Err(_) => last = "call timed out".to_string(),
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("call to {service} never reached the server: {last}");
}

/// Payment headers: quote id + binding signature.
fn paid_headers(quote_id: &str) -> Vec<(String, Vec<u8>)> {
    vec![
        (HDR_PAYMENT_QUOTE.to_string(), quote_id.as_bytes().to_vec()),
        (HDR_PAYMENT_BINDING.to_string(), BINDING.to_vec()),
    ]
}

/// Bearer presentation: the quote id alone, no possession proof.
fn bearer_headers(quote_id: &str) -> Vec<(String, Vec<u8>)> {
    vec![(HDR_PAYMENT_QUOTE.to_string(), quote_id.as_bytes().to_vec())]
}

fn no_headers() -> Vec<(String, Vec<u8>)> {
    Vec::new()
}

/// Meshes and (optionally) a journal path, before anything is served —
/// the window in which a test can seed the journal exactly as a previous
/// owner would have left it.
struct Pending {
    provider: Mesh,
    callers: Vec<Mesh>,
    dir: Option<tempfile::TempDir>,
    path: Option<PathBuf>,
}

async fn pending(durable: bool) -> Pending {
    pending_with(1, durable).await
}

async fn pending_with(callers: usize, durable: bool) -> Pending {
    let provider = mesh().await;
    let mut peers = Vec::with_capacity(callers);
    for _ in 0..callers {
        peers.push(mesh().await);
    }
    let (dir, path) = if durable {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("admissions.json");
        (Some(dir), Some(path))
    } else {
        (None, None)
    };
    Pending {
        provider,
        callers: peers,
        dir,
        path,
    }
}

impl Pending {
    fn path(&self) -> &Path {
        self.path.as_deref().expect("a durable fixture")
    }

    /// The owner a submission from caller `i` is attributed to under
    /// [`A2aPrincipal::SessionPeer`].
    fn owner(&self, i: usize) -> TaskOwner {
        TaskOwner::Peer(self.callers[i].inner().node_id())
    }

    async fn journal(&self) -> A2aAdmissionJournal {
        A2aAdmissionJournal::open(self.path())
            .await
            .expect("open the journal")
    }

    async fn serve(self, policies: Vec<A2aServicePolicy>, gated: bool) -> Fixture {
        let journal = match self.path.as_ref() {
            Some(_) => Some(self.journal().await),
            None => None,
        };
        self.serve_with(policies, gated, journal).await
    }

    /// Serve `policies`, using a caller-supplied journal (already seeded,
    /// or armed to fail its next write).
    async fn serve_with(
        self,
        policies: Vec<A2aServicePolicy>,
        gated: bool,
        journal: Option<A2aAdmissionJournal>,
    ) -> Fixture {
        let exec = ExecProbe::new(gated);
        let gate = RecordingTaskGate::new();
        let preflight = ScriptedPreflight::new();
        let registry = TaskRegistry::new();
        let mut config = A2aServiceConfig::new(catalog(policies))
            .with_payment(Arc::clone(&gate) as Arc<dyn TaskAdmissionGate>)
            .with_preflight(Arc::clone(&preflight) as Arc<dyn TaskPreflight>);
        if let Some(journal) = journal {
            config = config.with_journal(journal);
        }
        let serving = self
            .provider
            .serve_a2a_configured(registry.clone(), exec.executor(), config)
            .expect("serve the configured catalog");

        let peers: Vec<&Mesh> = self.callers.iter().collect();
        connect_all(&self.provider, &peers).await;
        let target = self.provider.inner().node_id();
        // Readiness precondition, not a sleep: describe is the cheapest
        // uncharged verb, and it answering means the dispatch path is up.
        for caller in &self.callers {
            let mut ready = false;
            for _ in 0..50 {
                if caller.describe_a2a(target).await.is_ok() {
                    ready = true;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            assert!(ready, "the provider never answered describe");
        }

        Fixture {
            _provider: self.provider,
            callers: self.callers,
            target,
            registry,
            serving: Some(serving),
            exec,
            gate,
            preflight,
            _dir: self.dir,
        }
    }
}

/// A served provider plus its callers and every probe a witness reads.
struct Fixture {
    /// Held so the served node outlives the fixture.
    _provider: Mesh,
    callers: Vec<Mesh>,
    target: u64,
    registry: TaskRegistry,
    /// `Option` so a witness can drop every serve handle (and with it
    /// the store and the journal owner) while a task is still running.
    serving: Option<A2aServing>,
    exec: ExecProbe,
    gate: Arc<RecordingTaskGate>,
    preflight: Arc<ScriptedPreflight>,
    /// Held so the journal's tempdir outlives the fixture.
    _dir: Option<tempfile::TempDir>,
}

impl Fixture {
    fn caller(&self) -> &Mesh {
        &self.callers[0]
    }

    fn owner(&self) -> TaskOwner {
        self.owner_of(0)
    }

    fn owner_of(&self, i: usize) -> TaskOwner {
        TaskOwner::Peer(self.callers[i].inner().node_id())
    }

    fn store(&self) -> &dyn AdmissionStore {
        self.serving.as_ref().expect("serving").store.as_ref()
    }

    /// The durable record for caller 0's `task_id`.
    async fn record(&self, task_id: &str) -> Option<AdmissionRecord> {
        self.record_of(0, task_id).await
    }

    async fn record_of(&self, i: usize, task_id: &str) -> Option<AdmissionRecord> {
        self.store()
            .lookup(self.owner_of(i), task_id)
            .await
            .expect("store lookup")
    }

    async fn tag(&self, task_id: &str) -> Option<StateTag> {
        self.record(task_id).await.map(|r| r.state.tag())
    }

    async fn ledger_has(&self, task_id: &str) -> bool {
        self.store()
            .ledger_has(self.owner(), task_id)
            .await
            .expect("ledger")
    }

    async fn in_flight(&self) -> u64 {
        self.store()
            .in_flight(SERVICE, now_secs())
            .await
            .expect("in flight")
    }

    async fn prepare(&self, brief: &TaskBrief) -> PrepareReply {
        self.prepare_as(0, brief).await
    }

    async fn prepare_as(&self, i: usize, brief: &TaskBrief) -> PrepareReply {
        self.callers[i]
            .prepare_a2a(self.target, brief)
            .await
            .expect("prepare")
    }

    /// Prepare and unwrap the reservation.
    async fn reserve(&self, brief: &TaskBrief) -> AdmissionReservation {
        self.reserve_as(0, brief).await
    }

    async fn reserve_as(&self, i: usize, brief: &TaskBrief) -> AdmissionReservation {
        match self.prepare_as(i, brief).await {
            PrepareReply::Reservation(res) => res,
            other => panic!("expected a reservation, got {other:?}"),
        }
    }

    async fn submit(
        &self,
        brief: &TaskBrief,
        headers: Vec<(String, Vec<u8>)>,
    ) -> Result<TaskAck, Refusal> {
        self.submit_as(0, brief, headers).await
    }

    async fn submit_as(
        &self,
        i: usize,
        brief: &TaskBrief,
        headers: Vec<(String, Vec<u8>)>,
    ) -> Result<TaskAck, Refusal> {
        let body = envelope(&brief.encode());
        let raw = call_raw(
            &self.callers[i],
            self.target,
            A2A_TASK_SERVICE,
            body,
            headers,
        )
        .await?;
        Ok(TaskAck::decode(&raw).expect("decode ack"))
    }

    async fn status(&self, task_id: &str) -> Option<TaskRecord> {
        self.status_as(0, task_id).await
    }

    async fn status_as(&self, i: usize, task_id: &str) -> Option<TaskRecord> {
        self.callers[i]
            .task_status(self.target, task_id)
            .await
            .expect("status")
    }

    /// Wait until the durable record reaches `tag` — the precondition a
    /// terminal-hook write establishes, never a fixed sleep.
    async fn await_tag(&self, task_id: &str, tag: StateTag) -> AdmissionRecord {
        for _ in 0..200 {
            if let Some(record) = self.record(task_id).await {
                if record.state.tag() == tag {
                    return record;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!(
            "task {task_id:?} never reached {}; it is {:?}",
            tag.as_str(),
            self.record(task_id).await.map(|r| r.state)
        );
    }

    /// A complete purchase for `brief`: prepare, issue a quote against
    /// the provider's own purchase hash, submit with both headers.
    async fn buy_and_submit(&self, brief: &TaskBrief, quote_id: &str) -> Result<TaskAck, Refusal> {
        let res = self.reserve(brief).await;
        self.gate.issue(quote_id, &res.purchase_hash, PAYER);
        self.submit(brief, paid_headers(quote_id)).await
    }
}

/// Seed `journal` with a `Paid` admission for `brief`, exactly as a
/// provider that crashed between redeem and claim would have left it.
async fn seed_paid(
    journal: &A2aAdmissionJournal,
    owner: TaskOwner,
    brief: &TaskBrief,
    offer: &A2aOffer,
    admission_id: &str,
    quote_id: &str,
) {
    let now = now_secs();
    journal
        .insert_reserved(AdmissionRecord::reserved(
            owner,
            brief.clone(),
            offer,
            Some(admission_id.to_string()),
            task_commitment(offer, brief),
            now,
        ))
        .await
        .expect("seed the reservation");
    journal
        .transition(
            owner,
            &brief.task_id,
            &[StateTag::Reserved],
            AdmissionState::Paid {
                quote_id: quote_id.to_string(),
                payer: PAYER,
            },
            now,
        )
        .await
        .expect("seed the payment");
}

// ===========================================================================
// Configuration (D1)
// ===========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_free_configured_service_runs_without_a_gate_or_journal() {
    let provider = mesh().await;
    let caller = mesh().await;
    let exec = ExecProbe::new(false);
    let registry = TaskRegistry::new();

    // No gate, no journal, no preflight: a service given away must not
    // need a payments crate or a durable store to serve.
    let serving = provider
        .serve_a2a_configured(
            registry.clone(),
            exec.executor(),
            A2aServiceConfig::new(catalog(vec![A2aServicePolicy::Free(offer(false))])),
        )
        .expect("a free catalog needs neither a gate nor a journal");

    connect_all(&provider, &[&caller]).await;
    let target = provider.inner().node_id();
    let owner = TaskOwner::Peer(caller.inner().node_id());
    let b = brief("t-free");

    let ack = TaskAck::decode(
        &call_raw(
            &caller,
            target,
            A2A_TASK_SERVICE,
            envelope(&b.encode()),
            no_headers(),
        )
        .await
        .expect("submit"),
    )
    .expect("ack");
    assert!(ack.accepted, "free submit refused: {:?}", ack.reason);
    exec.wait_started().await;
    assert_eq!(exec.runs(), 1, "the free task must have run exactly once");

    // The in-memory store carries the same state table: admitted,
    // claimed, and in the ledger.
    let record = serving
        .store
        .lookup(owner, "t-free")
        .await
        .expect("lookup")
        .expect("the in-memory store recorded the admission");
    assert!(
        matches!(record.state.tag(), StateTag::Launched | StateTag::Terminal),
        "free admission is {:?}",
        record.state
    );
    assert!(
        serving
            .store
            .ledger_has(owner, "t-free")
            .await
            .expect("ledger"),
        "a launch is always in the ledger, free or paid"
    );
}

#[tokio::test]
async fn a_paid_service_refuses_to_start_without_a_gate() {
    let provider = mesh().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("admissions.json");
    let exec = ExecProbe::new(false);

    let config = A2aServiceConfig::new(catalog(vec![A2aServicePolicy::Paid(offer(true))]))
        .with_journal(A2aAdmissionJournal::open(&path).await.expect("open"));
    let err = provider
        .serve_a2a_configured(TaskRegistry::new(), exec.executor(), config)
        .err()
        .expect("a paid service with no gate must refuse to start");
    assert!(
        matches!(&err, ServeError::A2aPaidMisconfigured(msg) if msg.contains("TaskAdmissionGate")),
        "{err}"
    );

    // The positive control: the same catalog with a gate starts. The
    // refusal is about the missing gate, not about the catalog.
    let config = A2aServiceConfig::new(catalog(vec![A2aServicePolicy::Paid(offer(true))]))
        .with_journal(A2aAdmissionJournal::open(&path).await.expect("reopen"))
        .with_payment(RecordingTaskGate::new() as Arc<dyn TaskAdmissionGate>);
    provider
        .serve_a2a_configured(TaskRegistry::new(), exec.executor(), config)
        .expect("a gated paid service starts");
}

#[tokio::test]
async fn a_paid_service_refuses_to_start_without_a_journal() {
    let provider = mesh().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("admissions.json");
    let exec = ExecProbe::new(false);

    let config = A2aServiceConfig::new(catalog(vec![A2aServicePolicy::Paid(offer(true))]))
        .with_payment(RecordingTaskGate::new() as Arc<dyn TaskAdmissionGate>);
    let err = provider
        .serve_a2a_configured(TaskRegistry::new(), exec.executor(), config)
        .err()
        .expect("a paid service with no journal must refuse to start");
    assert!(
        matches!(&err, ServeError::A2aPaidMisconfigured(msg) if msg.contains("journal")),
        "{err}"
    );

    let config = A2aServiceConfig::new(catalog(vec![A2aServicePolicy::Paid(offer(true))]))
        .with_payment(RecordingTaskGate::new() as Arc<dyn TaskAdmissionGate>)
        .with_journal(A2aAdmissionJournal::open(&path).await.expect("open"));
    provider
        .serve_a2a_configured(TaskRegistry::new(), exec.executor(), config)
        .expect("a journalled paid service starts");
}

#[tokio::test]
async fn a_paid_service_refuses_to_start_without_pricing() {
    let provider = mesh().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("admissions.json");
    let exec = ExecProbe::new(false);

    // Priced by policy, unpriced in the offer: a gate on an unannounced
    // price refuses every caller with no way to know why.
    let unpriced = offer(false);
    let config = A2aServiceConfig::new(catalog(vec![A2aServicePolicy::Paid(unpriced)]))
        .with_payment(RecordingTaskGate::new() as Arc<dyn TaskAdmissionGate>)
        .with_journal(A2aAdmissionJournal::open(&path).await.expect("open"));
    let err = provider
        .serve_a2a_configured(TaskRegistry::new(), exec.executor(), config)
        .err()
        .expect("a paid service with no pricing terms must refuse to start");
    assert!(
        matches!(&err, ServeError::MissingPricingTerms(id) if id == SERVICE),
        "{err}"
    );

    let config = A2aServiceConfig::new(catalog(vec![A2aServicePolicy::Paid(offer(true))]))
        .with_payment(RecordingTaskGate::new() as Arc<dyn TaskAdmissionGate>)
        .with_journal(A2aAdmissionJournal::open(&path).await.expect("reopen"));
    provider
        .serve_a2a_configured(TaskRegistry::new(), exec.executor(), config)
        .expect("a priced paid service starts");
}

#[tokio::test]
async fn a_free_service_refuses_pricing() {
    let provider = mesh().await;
    let exec = ExecProbe::new(false);

    // An announced price with no gate behind it would be served free.
    let config = A2aServiceConfig::new(catalog(vec![A2aServicePolicy::Free(offer(true))]));
    let err = provider
        .serve_a2a_configured(TaskRegistry::new(), exec.executor(), config)
        .err()
        .expect("a free service must refuse an announced price");
    assert!(
        matches!(&err, ServeError::UnenforceablePricing(id) if id == SERVICE),
        "{err}"
    );

    provider
        .serve_a2a_configured(
            TaskRegistry::new(),
            exec.executor(),
            A2aServiceConfig::new(catalog(vec![A2aServicePolicy::Free(offer(false))])),
        )
        .expect("an unpriced free service starts");
}

/// Under [`A2aPrincipal::OrgAdmitted`] the five services register
/// through the PROTECTED core seams, not the public one.
///
/// The observable difference at registration is that a protected
/// registration **requires an installed node authority** — the owner
/// audience credential the encrypted announcement consumes and the
/// admission gate verifies against. A node without one can serve the
/// same catalog to session peers and cannot serve it org-admitted,
/// which is exactly the discrimination: had these handlers gone out
/// over `serve_rpc`, `ctx.org_admission` would be `None` and the
/// principal would silently degrade to the delivering peer.
#[tokio::test]
async fn an_org_admitted_catalog_registers_as_protected() {
    let provider = mesh().await;
    let exec = ExecProbe::new(false);

    for access in [OrgAccess::SameOrg, OrgAccess::Granted] {
        let config = A2aServiceConfig::new(catalog(vec![A2aServicePolicy::Free(offer(false))]))
            .with_principal(A2aPrincipal::OrgAdmitted(access));
        let err = provider
            .serve_a2a_configured(TaskRegistry::new(), exec.executor(), config)
            .err()
            .unwrap_or_else(|| panic!("{access:?} must not register without an authority"));
        assert!(
            matches!(&err, ServeError::ProtectedAuthorityRequired(service) if service == A2A_TASK_SERVICE),
            "{access:?}: {err}"
        );
    }

    // The control: the same catalog serves fine to session peers, so
    // the refusals above are about the principal and not the catalog.
    provider
        .serve_a2a_configured(
            TaskRegistry::new(),
            exec.executor(),
            A2aServiceConfig::new(catalog(vec![A2aServicePolicy::Free(offer(false))])),
        )
        .expect("the session-peer principal needs no authority");
}

// ===========================================================================
// Free is not policy-free (finding r4-1)
// ===========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_free_service_without_a_journal_runs_preflight_on_direct_submit() {
    let fx = pending(false)
        .await
        .serve(vec![A2aServicePolicy::Free(offer(false))], false)
        .await;
    fx.preflight.refuse("this agent is not accepting work");

    // No prepare at all: the inline admission path must still run the
    // application's own policy.
    let ack = fx
        .submit(&brief("t-1"), no_headers())
        .await
        .expect("an application refusal is in-body, not a payment error");
    assert!(!ack.accepted);
    assert!(
        ack.reason
            .as_deref()
            .is_some_and(|r| r.contains("not accepting work")),
        "{:?}",
        ack.reason
    );
    assert_eq!(fx.preflight.calls(), 1, "preflight ran");
    assert_eq!(fx.exec.runs(), 0, "nothing ran");
    assert!(
        fx.record("t-1").await.is_none(),
        "a refused direct submit reserves nothing"
    );

    // The control: the same path admits once the application allows it.
    fx.preflight.allow();
    let ack = fx
        .submit(&brief("t-2"), no_headers())
        .await
        .expect("submit");
    assert!(ack.accepted, "{:?}", ack.reason);
    fx.exec.wait_started().await;
    assert_eq!(fx.exec.runs(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_free_service_without_a_journal_enforces_capacity_on_direct_submit() {
    let mut narrow = offer(false);
    narrow.bounds.max_in_flight = 1;
    let fx = pending(false)
        .await
        .serve(vec![A2aServicePolicy::Free(narrow)], true)
        .await;

    let first = fx
        .submit(&brief("t-1"), no_headers())
        .await
        .expect("first submit");
    assert!(first.accepted, "{:?}", first.reason);
    fx.exec.wait_started().await;

    // The slot is taken while the first task is held on the barrier.
    let second = fx
        .submit(&brief("t-2"), no_headers())
        .await
        .expect("a capacity refusal is in-body");
    assert!(!second.accepted);
    assert!(
        second
            .reason
            .as_deref()
            .is_some_and(|r| r.contains("capacity")),
        "{:?}",
        second.reason
    );
    assert_eq!(fx.exec.runs(), 1, "the refused submit never ran");
    assert!(fx.record("t-2").await.is_none(), "nothing was admitted");

    // Freeing the slot is the terminal record, not the passage of time.
    fx.exec.release(1);
    fx.await_tag("t-1", StateTag::Terminal).await;
    let third = fx
        .submit(&brief("t-3"), no_headers())
        .await
        .expect("third submit");
    assert!(
        third.accepted,
        "capacity must free on a terminal outcome: {:?}",
        third.reason
    );
    fx.exec.release(1);
    fx.exec.wait_started().await;
    assert_eq!(fx.exec.runs(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_free_direct_submit_that_passes_preflight_launches_once() {
    let fx = pending(false)
        .await
        .serve(vec![A2aServicePolicy::Free(offer(false))], true)
        .await;
    let b = brief("t-once");

    let first = fx.submit(&b, no_headers()).await.expect("first");
    assert!(first.accepted, "{:?}", first.reason);
    fx.exec.wait_started().await;

    // An identical retransmit while the first run is held converges on
    // the original task instead of starting a second one.
    let second = fx.submit(&b, no_headers()).await.expect("retransmit");
    assert!(second.accepted);
    assert_eq!(second.task_id, first.task_id);
    assert_eq!(fx.exec.runs(), 1, "one brief, one run");

    fx.exec.release(1);
    let record = fx.await_tag("t-once", StateTag::Terminal).await;
    assert_eq!(
        record.state,
        AdmissionState::Terminal {
            quote_id: None,
            payer: None,
            state: TaskState::Completed {
                result_ref: RESULT.to_string()
            }
        }
    );
    assert!(fx.ledger_has("t-once").await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_free_prepared_reservation_is_deleted_when_preflight_fails_at_submit() {
    let fx = pending(false)
        .await
        .serve(vec![A2aServicePolicy::Free(offer(false))], false)
        .await;
    let b = brief("t-free-prep");

    fx.reserve(&b).await;
    assert_eq!(fx.tag("t-free-prep").await, Some(StateTag::Reserved));

    fx.preflight.refuse("the corpus was withdrawn");
    let ack = fx
        .submit(&b, no_headers())
        .await
        .expect("a free refusal is in-body");
    assert!(!ack.accepted);
    assert_eq!(fx.exec.runs(), 0);
    assert!(
        fx.record("t-free-prep").await.is_none(),
        "a free reservation with nothing financial behind it is deleted, not held"
    );
    assert_eq!(fx.in_flight().await, 0, "its capacity was released too");

    // The control: the id is free again and the same brief admits.
    fx.preflight.allow();
    let ack = fx.submit(&b, no_headers()).await.expect("resubmit");
    assert!(ack.accepted, "{:?}", ack.reason);
    fx.exec.wait_started().await;
    assert_eq!(fx.exec.runs(), 1);
}

// ===========================================================================
// Ownership of the journal across a live task (finding r3-1)
// ===========================================================================

/// The journal's owner outlives the serve handles: a launched task can
/// still write its terminal row, and no successor may open the journal
/// until it has.
///
/// The store-level ownership witnesses (a second process after a file
/// replacement; a second open in one process) live in
/// `a2a_admission_journal.rs`. What is specific to the serving path is
/// that dropping every [`A2aServing`] handle while work is in flight does
/// **not** release the lock.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_owner_outlives_a_running_executor() {
    let p = pending(true).await;
    let path = p.path().to_path_buf();
    let owner = p.owner(0);
    let mut fx = p
        .serve(vec![A2aServicePolicy::Free(offer(false))], true)
        .await;

    let ack = fx
        .submit(&brief("t-live"), no_headers())
        .await
        .expect("submit");
    assert!(ack.accepted, "{:?}", ack.reason);
    fx.exec.wait_started().await;

    // Drop everything the serving path handed back: the five
    // registrations (which drop the handlers, and with them their store
    // and owner clones) and this test's own store handle.
    fx.serving = None;

    // The task is held on the barrier, so it is demonstrably live for
    // the whole window below.
    for attempt in 0..25 {
        match A2aAdmissionJournal::open(&path).await {
            Err(A2aJournalError::OwnedElsewhere { .. }) => {}
            Ok(_) => panic!(
                "attempt {attempt}: a successor opened the journal while a launched task \
                 could still write to it"
            ),
            Err(e) => panic!("attempt {attempt}: unexpected error {e}"),
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Release it, and wait for the run to return.
    fx.exec.release(1);
    fx.exec.wait_finished().await;

    // Once the last writer is gone the lock is released — and what it
    // wrote on the way out is the terminal row.
    let mut successor = None;
    for _ in 0..200 {
        match A2aAdmissionJournal::open(&path).await {
            Ok(journal) => {
                successor = Some(journal);
                break;
            }
            Err(A2aJournalError::OwnedElsewhere { .. }) => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(e) => panic!("unexpected error {e}"),
        }
    }
    let successor = successor.expect("the journal is never released");
    let record = successor
        .lookup(owner, "t-live")
        .await
        .expect("lookup")
        .expect("the record survives its owner");
    assert_eq!(
        record.state,
        AdmissionState::Terminal {
            quote_id: None,
            payer: None,
            state: TaskState::Completed {
                result_ref: RESULT.to_string()
            }
        },
        "the terminal hook wrote its row before the owner was released"
    );
}

// ===========================================================================
// Preflight before money (P1–P5)
// ===========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn prepare_rejects_invalid_oversized_or_unauthorized_briefs_without_reserving() {
    let fx = pending(true)
        .await
        .serve(vec![A2aServicePolicy::Paid(offer(true))], false)
        .await;

    // Unnamed service.
    let unnamed = TaskBrief::new("no service").with_task_id("t-unnamed");
    assert!(
        matches!(fx.prepare(&unnamed).await, PrepareReply::Rejected { .. }),
        "an unnamed service must be refused"
    );
    // Unknown service.
    let unknown = TaskBrief::new("elsewhere")
        .with_task_id("t-unknown")
        .with_service("svc.nope", REV);
    assert!(matches!(
        fx.prepare(&unknown).await,
        PrepareReply::Rejected { .. }
    ));
    // Oversized prompt (the offer publishes 256 bytes).
    let huge = brief_id("t-huge", &"x".repeat(1024));
    match fx.prepare(&huge).await {
        PrepareReply::Rejected { reason } => {
            assert!(reason.contains("prompt_bytes"), "{reason}")
        }
        other => panic!("expected a bounds rejection, got {other:?}"),
    }
    // Unauthorized by the application.
    fx.preflight.refuse("this caller may not use the corpus");
    match fx.prepare(&brief("t-unauth")).await {
        PrepareReply::Rejected { reason } => {
            assert!(reason.contains("may not use the corpus"), "{reason}")
        }
        other => panic!("expected a preflight rejection, got {other:?}"),
    }

    for id in ["t-unnamed", "t-unknown", "t-huge", "t-unauth"] {
        assert!(
            fx.record(id).await.is_none(),
            "{id} reserved something despite being refused"
        );
    }
    assert_eq!(fx.in_flight().await, 0, "no capacity was taken");
    assert_eq!(fx.gate.call_count(), 0, "no quote can exist yet");

    // The control: a valid brief does reserve, so the four refusals
    // above are about the briefs and not about a dead prepare path.
    fx.preflight.allow();
    let res = fx.reserve(&brief("t-ok")).await;
    assert_eq!(res.task_id, "t-ok");
    assert_eq!(fx.in_flight().await, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn prepare_is_idempotent_and_returns_the_same_admission_id() {
    let fx = pending(true)
        .await
        .serve(vec![A2aServicePolicy::Paid(offer(true))], false)
        .await;
    let b = brief("t-idem");

    let first = fx.reserve(&b).await;
    let second = fx.reserve(&b).await;
    assert_eq!(
        first, second,
        "a retransmitted prepare must return the identical reservation"
    );
    assert!(!first.admission_id.is_empty());
    assert_eq!(
        first.purchase_hash,
        purchase_hash(&first.admission_id, &first.commitment)
    );
    assert_eq!(first.pricing_terms.as_deref(), Some(TERMS));
    assert_eq!(
        first.capability,
        format!("{}/net.a2a.task/{SERVICE}", fx.target)
    );
    assert_eq!(fx.in_flight().await, 1, "one reservation, not two");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn prepare_refuses_a_different_brief_under_the_same_id() {
    let fx = pending(true)
        .await
        .serve(vec![A2aServicePolicy::Paid(offer(true))], false)
        .await;

    let original = brief_id("t-reuse", "summarize the filing");
    let first = fx.reserve(&original).await;

    let altered = brief_id("t-reuse", "exfiltrate the filing");
    match fx.prepare(&altered).await {
        PrepareReply::Rejected { reason } => assert!(reason.contains("t-reuse"), "{reason}"),
        other => panic!("expected an id-reuse rejection, got {other:?}"),
    }

    let record = fx.record("t-reuse").await.expect("the original survives");
    assert_eq!(record.commitment, first.commitment);
    assert_eq!(
        record.admission_id.as_deref(),
        Some(first.admission_id.as_str())
    );
    assert_eq!(record.brief.prompt, "summarize the filing");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn prepare_reports_busy_at_max_in_flight_and_frees_on_expiry() {
    let mut narrow = offer(true);
    narrow.bounds.max_in_flight = 1;
    narrow.reservation_ttl_secs = 1;
    let fx = pending(true)
        .await
        .serve(vec![A2aServicePolicy::Paid(narrow)], false)
        .await;

    let first = fx.reserve(&brief("t-a")).await;
    match fx.prepare(&brief("t-b")).await {
        PrepareReply::Busy => {}
        other => panic!("expected Busy at capacity, got {other:?}"),
    }
    assert!(
        fx.record("t-b").await.is_none(),
        "a Busy prepare writes nothing"
    );

    // Wait for the reservation to lapse — observed through the store's
    // own capacity accounting, not by sleeping a fixed amount.
    let mut lapsed = false;
    for _ in 0..200 {
        if fx.in_flight().await == 0 {
            lapsed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(lapsed, "the reservation never lapsed");

    // The lapsed reservation re-acquires capacity under its ORIGINAL
    // admission id: a caller that already holds a quote for it is not
    // forced to buy a second one.
    let reacquired = fx.reserve(&brief("t-a")).await;
    assert_eq!(reacquired.admission_id, first.admission_id);
    assert_eq!(reacquired.purchase_hash, first.purchase_hash);
    assert!(reacquired.expires_at > first.expires_at);
    assert_eq!(fx.in_flight().await, 1);

    // ...and the slot is taken again.
    match fx.prepare(&brief("t-b")).await {
        PrepareReply::Busy => {}
        other => panic!("expected Busy after the re-acquire, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn prepare_on_a_paid_record_returns_the_same_reservation() {
    let p = pending(true).await;
    let b = brief("t-paid-prep");
    let journal = p.journal().await;
    seed_paid(&journal, p.owner(0), &b, &offer(true), "adm-seed", "q-seed").await;
    let fx = p
        .serve_with(
            vec![A2aServicePolicy::Paid(offer(true))],
            false,
            Some(journal),
        )
        .await;

    let res = fx.reserve(&b).await;
    assert_eq!(res.admission_id, "adm-seed");
    assert_eq!(res.commitment, task_commitment(&offer(true), &b));
    assert_eq!(
        res.purchase_hash,
        purchase_hash("adm-seed", &res.commitment)
    );
    assert_eq!(
        fx.tag("t-paid-prep").await,
        Some(StateTag::Paid),
        "prepare must not disturb a paid admission"
    );
    assert_eq!(fx.gate.call_count(), 0, "prepare never touches the gate");
}

// ===========================================================================
// Refusals
// ===========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unpaid_submit_to_a_paid_service_is_refused_before_the_executor() {
    let fx = pending(true)
        .await
        .serve(vec![A2aServicePolicy::Paid(offer(true))], false)
        .await;
    let b = brief("t-unpaid");
    fx.reserve(&b).await;

    let refusal = fx
        .submit(&b, no_headers())
        .await
        .expect_err("an unpaid submit to a paid service must be a payment refusal");
    assert_eq!(refusal.status, ERR_PAYMENT);
    assert_eq!(refusal.reason(), "missing_quote");
    assert_eq!(refusal.schematic().funds_moved, failure_vocab::FUNDS_NO);
    assert!(!refusal.schematic().handler_executed);
    assert!(!refusal.message.is_empty());

    assert_eq!(fx.exec.runs(), 0, "the executor never ran");
    assert_eq!(fx.gate.call_count(), 0, "the gate was never consulted");
    let record = fx.record("t-unpaid").await.expect("the reservation stays");
    assert_eq!(record.state.tag(), StateTag::Reserved);
    assert_eq!(record.attempts.len(), 1, "the attempt is noted");
    assert_eq!(record.attempts[0].reason, "missing_quote");

    // The control: the same reservation admits once it is paid for.
    let res = fx.reserve(&b).await;
    fx.gate.issue("q-ok", &res.purchase_hash, PAYER);
    let ack = fx
        .submit(&b, paid_headers("q-ok"))
        .await
        .expect("the paid submit");
    assert!(ack.accepted, "{:?}", ack.reason);
    fx.exec.wait_started().await;
    assert_eq!(fx.exec.runs(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bearer_submit_is_refused_binding_required() {
    let fx = pending(true)
        .await
        .serve(vec![A2aServicePolicy::Paid(offer(true))], false)
        .await;
    let b = brief("t-bearer");
    let res = fx.reserve(&b).await;
    fx.gate.issue("q-1", &res.purchase_hash, PAYER);

    // The quote id alone is bearer presentation: possession is not proof
    // that the payer authorized THIS submission.
    let refusal = fx
        .submit(&b, bearer_headers("q-1"))
        .await
        .expect_err("a bearer task submit must be refused");
    assert_eq!(refusal.status, ERR_PAYMENT);
    assert_eq!(refusal.reason(), "binding_required");
    assert_eq!(fx.gate.call_count(), 0, "the gate is never reached");
    assert_eq!(fx.exec.runs(), 0);
    let record = fx.record("t-bearer").await.expect("record");
    assert_eq!(record.state.tag(), StateTag::Reserved);
    assert_eq!(record.attempts.len(), 1);
    assert_eq!(record.attempts[0].reason, "binding_required");

    // The control: the same quote with a binding admits.
    let ack = fx.submit(&b, paid_headers("q-1")).await.expect("submit");
    assert!(ack.accepted, "{:?}", ack.reason);
    assert_eq!(fx.gate.call_count(), 1);
    fx.exec.wait_started().await;
    assert_eq!(fx.exec.runs(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_paid_submit_without_a_reservation_is_refused_before_the_gate() {
    let fx = pending(true)
        .await
        .serve(vec![A2aServicePolicy::Paid(offer(true))], false)
        .await;
    let b = brief("t-noresv");
    // A quote that would otherwise be perfectly good.
    fx.gate.issue(
        "q-1",
        &purchase_hash("adm-elsewhere", &task_commitment(&offer(true), &b)),
        PAYER,
    );

    let refusal = fx
        .submit(&b, paid_headers("q-1"))
        .await
        .expect_err("a paid submit with no reservation must be refused");
    assert_eq!(refusal.status, ERR_PAYMENT);
    assert_eq!(refusal.reason(), "no_reservation");
    assert_eq!(
        refusal.schematic().funds_moved,
        failure_vocab::FUNDS_UNKNOWN,
        "a provider with no record cannot claim no money moved"
    );
    assert_eq!(
        fx.gate.call_count(),
        0,
        "structural refusal: the gate is never consulted"
    );
    assert_eq!(fx.exec.runs(), 0);
    assert!(fx.record("t-noresv").await.is_none());

    // The control: prepare first, and the same quote id (re-issued
    // against the real reservation) admits.
    let res = fx.reserve(&b).await;
    fx.gate.issue("q-2", &res.purchase_hash, PAYER);
    let ack = fx.submit(&b, paid_headers("q-2")).await.expect("submit");
    assert!(ack.accepted, "{:?}", ack.reason);
    assert_eq!(fx.gate.call_count(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_revoked_preflight_with_headers_enters_reconciliation_not_unpaid_rejection() {
    let fx = pending(true)
        .await
        .serve(vec![A2aServicePolicy::Paid(offer(true))], false)
        .await;
    let b = brief("t-revoked");
    let res = fx.reserve(&b).await;
    fx.gate.issue("q-1", &res.purchase_hash, PAYER);

    // Authority is withdrawn between prepare and submit, and the
    // submission carries payment evidence.
    fx.preflight.refuse("the provider lost the corpus licence");
    let refusal = fx
        .submit(&b, paid_headers("q-1"))
        .await
        .expect_err("a post-payment refusal is never an in-body rejection");
    assert_eq!(refusal.status, ERR_PAYMENT);
    assert_eq!(refusal.reason(), "admission_revoked");
    let s = refusal.schematic();
    assert_eq!(s.funds_moved, failure_vocab::FUNDS_UNKNOWN);
    assert_eq!(s.prior_payment, failure_vocab::PRIOR_UNKNOWN);
    assert!(!s.retryable);
    assert!(!s.recovery.safe_to_requote);
    assert_eq!(
        s.recovery.next_action.as_deref(),
        Some("contact_provider_operator")
    );

    assert_eq!(fx.gate.call_count(), 0, "the gate is never called here");
    assert_eq!(fx.exec.runs(), 0);
    let record = fx.record("t-revoked").await.expect("record");
    match &record.state {
        AdmissionState::Reconcile {
            claimed_quote_id, ..
        } => assert_eq!(claimed_quote_id.as_deref(), Some("q-1")),
        other => panic!("expected Reconcile, got {other:?}"),
    }
    let updated_at = record.updated_at;

    // A second submission answers the same schematic and changes
    // nothing: only an operator's resolve moves it.
    let again = fx
        .submit(&b, paid_headers("q-1"))
        .await
        .expect_err("still refused");
    assert_eq!(again.reason(), "admission_revoked");
    let record = fx.record("t-revoked").await.expect("record");
    assert_eq!(record.state.tag(), StateTag::Reconcile);
    assert_eq!(record.updated_at, updated_at, "no state change");
    assert_eq!(fx.gate.call_count(), 0);
    assert_eq!(fx.exec.runs(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_revoked_preflight_without_headers_is_an_unpaid_rejection_and_keeps_the_reservation() {
    let fx = pending(true)
        .await
        .serve(vec![A2aServicePolicy::Paid(offer(true))], false)
        .await;
    let b = brief("t-quiet");
    let res = fx.reserve(&b).await;
    let before = fx.record("t-quiet").await.expect("record").updated_at;

    fx.preflight.refuse("temporarily unavailable");
    let ack = fx
        .submit(&b, no_headers())
        .await
        .expect("with no payment presented this is an ordinary rejection");
    assert!(!ack.accepted);
    assert!(
        ack.reason
            .as_deref()
            .is_some_and(|r| r.contains("temporarily unavailable")),
        "{:?}",
        ack.reason
    );

    let record = fx.record("t-quiet").await.expect("the reservation stays");
    assert_eq!(record.state.tag(), StateTag::Reserved);
    assert_eq!(record.updated_at, before, "nothing was claimed paid");
    assert_eq!(
        record.admission_id.as_deref(),
        Some(res.admission_id.as_str())
    );
    assert_eq!(fx.exec.runs(), 0);
    assert_eq!(fx.gate.call_count(), 0);

    // The control: the same reservation is still purchasable.
    fx.preflight.allow();
    fx.gate.issue("q-1", &res.purchase_hash, PAYER);
    let ack = fx.submit(&b, paid_headers("q-1")).await.expect("submit");
    assert!(ack.accepted, "{:?}", ack.reason);
}

// ===========================================================================
// Transitions (finding r3-2)
// ===========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_gate_denial_keeps_the_reservation_and_a_valid_retry_launches_once() {
    let fx = pending(true)
        .await
        .serve(vec![A2aServicePolicy::Paid(offer(true))], false)
        .await;
    let b = brief("t-denied");
    let res = fx.reserve(&b).await;
    fx.gate
        .issue_denied("q-bad", &res.purchase_hash, "not_settled");

    let refusal = fx
        .submit(&b, paid_headers("q-bad"))
        .await
        .expect_err("a gate denial is a payment refusal");
    assert_eq!(refusal.status, ERR_PAYMENT);
    assert_eq!(
        refusal.reason(),
        "not_settled",
        "the gate's own verdict travels untouched"
    );
    assert_eq!(fx.gate.call_count(), 1);
    assert_eq!(fx.exec.runs(), 0);

    let record = fx.record("t-denied").await.expect("record");
    assert_eq!(
        record.state.tag(),
        StateTag::Reserved,
        "a denial is an attempt note, never a state change"
    );
    assert_eq!(
        record.admission_id.as_deref(),
        Some(res.admission_id.as_str())
    );
    assert_eq!(record.attempts.len(), 1);
    assert_eq!(record.attempts[0].reason, "not_settled");
    assert_eq!(
        record.attempts[0].claimed_quote_id.as_deref(),
        Some("q-bad")
    );

    // The retry pays for the SAME admission — no second prepare, no
    // second admission id.
    fx.gate.issue("q-good", &res.purchase_hash, PAYER);
    let ack = fx
        .submit(&b, paid_headers("q-good"))
        .await
        .expect("the fixed payment");
    assert!(ack.accepted, "{:?}", ack.reason);
    assert_eq!(fx.gate.call_count(), 2);
    fx.exec.wait_started().await;
    assert_eq!(fx.exec.runs(), 1, "exactly one run for one purchase");
    assert!(fx.ledger_has("t-denied").await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_paid_record_resumes_without_a_second_redeem() {
    let p = pending(true).await;
    let b = brief("t-resume");
    let journal = p.journal().await;
    seed_paid(&journal, p.owner(0), &b, &offer(true), "adm-1", "q-seed").await;
    let fx = p
        .serve_with(
            vec![A2aServicePolicy::Paid(offer(true))],
            false,
            Some(journal),
        )
        .await;

    let ack = fx
        .submit(&b, paid_headers("q-seed"))
        .await
        .expect("a paid record resumes");
    assert!(ack.accepted, "{:?}", ack.reason);
    assert_eq!(
        fx.gate.call_count(),
        0,
        "the recorded evidence is authoritative; redeeming again is how a retry becomes a \
         second charge"
    );
    fx.exec.wait_started().await;
    assert_eq!(fx.exec.runs(), 1);
    let record = fx.await_tag("t-resume", StateTag::Terminal).await;
    assert_eq!(
        record.state,
        AdmissionState::Terminal {
            quote_id: Some("q-seed".to_string()),
            payer: Some(PAYER),
            state: TaskState::Completed {
                result_ref: RESULT.to_string()
            }
        },
        "the payment evidence follows the record to its outcome"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_paid_record_refuses_a_different_quote_header() {
    let p = pending(true).await;
    let b = brief("t-mismatch");
    let journal = p.journal().await;
    seed_paid(&journal, p.owner(0), &b, &offer(true), "adm-1", "q-seed").await;
    let fx = p
        .serve_with(
            vec![A2aServicePolicy::Paid(offer(true))],
            false,
            Some(journal),
        )
        .await;
    let before = fx.record("t-mismatch").await.expect("record").updated_at;

    let refusal = fx
        .submit(&b, paid_headers("q-other"))
        .await
        .expect_err("a quote that is not the recorded one must be refused");
    assert_eq!(refusal.status, ERR_PAYMENT);
    assert_eq!(refusal.reason(), "binding_rejected");
    assert_eq!(fx.gate.call_count(), 0, "S6' never calls the gate");
    assert_eq!(fx.exec.runs(), 0);

    let record = fx.record("t-mismatch").await.expect("record");
    assert_eq!(record.state.tag(), StateTag::Paid);
    assert_eq!(record.updated_at, before, "state unchanged");
    assert_eq!(record.attempts.len(), 1);
    assert_eq!(record.attempts[0].reason, "binding_rejected");

    // A bearer presentation of the RIGHT quote is refused for the same
    // reason — the binding is never optional.
    let bearer = fx
        .submit(&b, bearer_headers("q-seed"))
        .await
        .expect_err("bearer refused");
    assert_eq!(bearer.reason(), "binding_rejected");
    assert_eq!(fx.exec.runs(), 0);

    // The control: the recorded quote with a binding resumes.
    let ack = fx.submit(&b, paid_headers("q-seed")).await.expect("submit");
    assert!(ack.accepted, "{:?}", ack.reason);
    fx.exec.wait_started().await;
    assert_eq!(fx.exec.runs(), 1);
}

/// Each handler path performs the D2 table's transition **and no
/// other**: the record is observed before and after every step, and the
/// gate/run counters are asserted alongside.
///
/// The store-level claim — that `transition` refuses every pair outside
/// the table — is `every_transition_outside_the_table_is_a_conflict` in
/// `a2a_admission_journal.rs`. This is the serving-path half.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_handler_path_performs_only_its_table_transition() {
    let fx = pending(true)
        .await
        .serve(
            vec![
                A2aServicePolicy::Paid(offer(true)),
                A2aServicePolicy::Free(offer_named(OTHER_SERVICE, false)),
            ],
            false,
        )
        .await;

    // — prepare: absent → Reserved{admission_id}
    let paid_brief = brief("t-table-paid");
    assert!(fx.record("t-table-paid").await.is_none());
    let res = fx.reserve(&paid_brief).await;
    let reserved = fx.record("t-table-paid").await.expect("reserved");
    assert_eq!(reserved.state.tag(), StateTag::Reserved);
    assert!(reserved.attempts.is_empty());
    assert_eq!(fx.gate.call_count(), 0);
    assert_eq!(fx.exec.runs(), 0);

    // — gate denial: Reserved → Reserved + AttemptNote (never a state change)
    fx.gate
        .issue_denied("q-no", &res.purchase_hash, "quote_frozen");
    fx.submit(&paid_brief, paid_headers("q-no"))
        .await
        .expect_err("denied");
    let noted = fx.record("t-table-paid").await.expect("still reserved");
    assert_eq!(noted.state, reserved.state);
    assert_eq!(noted.updated_at, reserved.updated_at);
    assert_eq!(noted.attempts.len(), 1);
    assert_eq!(fx.gate.call_count(), 1);
    assert_eq!(fx.exec.runs(), 0);

    // — gate admitted: Reserved → Paid, then claim: Paid → Launched + ledger
    fx.gate.issue("q-yes", &res.purchase_hash, PAYER);
    let ack = fx
        .submit(&paid_brief, paid_headers("q-yes"))
        .await
        .expect("admitted");
    assert!(ack.accepted, "{:?}", ack.reason);
    assert_eq!(fx.gate.call_count(), 2);
    fx.exec.wait_started().await;
    assert_eq!(fx.exec.runs(), 1);
    assert!(fx.ledger_has("t-table-paid").await, "the ledger is written");
    // — executor terminal: Launched → Terminal
    let terminal = fx.await_tag("t-table-paid", StateTag::Terminal).await;
    assert_eq!(terminal.attempts.len(), 1, "notes are never rewritten");
    assert_eq!(
        terminal.admission_id.as_deref(),
        Some(res.admission_id.as_str()),
        "the admission id is immutable once inserted"
    );

    // — free inline: absent → Reserved{admission_id: None} → Launched
    let free_brief = TaskBrief::new("translate")
        .with_task_id("t-table-free")
        .with_service(OTHER_SERVICE, REV);
    let ack = fx.submit(&free_brief, no_headers()).await.expect("free");
    assert!(ack.accepted, "{:?}", ack.reason);
    fx.exec.wait_started().await;
    assert_eq!(fx.exec.runs(), 2);
    let free_record = fx.await_tag("t-table-free", StateTag::Terminal).await;
    assert_eq!(
        free_record.admission_id, None,
        "a free inline admission mints no admission id"
    );
    assert_eq!(fx.gate.call_count(), 2, "a free path never calls the gate");

    // — preflight fails with headers: Reserved → Reconcile
    let revoked = brief("t-table-revoked");
    let res = fx.reserve(&revoked).await;
    fx.gate.issue("q-rev", &res.purchase_hash, PAYER);
    fx.preflight.refuse("withdrawn");
    fx.submit(&revoked, paid_headers("q-rev"))
        .await
        .expect_err("revoked");
    assert_eq!(
        fx.tag("t-table-revoked").await,
        Some(StateTag::Reconcile),
        "a post-payment refusal is reconciliation"
    );
    assert_eq!(fx.gate.call_count(), 2, "the gate was not called again");
    assert_eq!(fx.exec.runs(), 2, "nothing else ran");

    // — preflight fails on a free reservation: the record is deleted
    let free_prepared = TaskBrief::new("translate later")
        .with_task_id("t-table-free-2")
        .with_service(OTHER_SERVICE, REV);
    fx.preflight.allow();
    fx.reserve(&free_prepared).await;
    assert_eq!(
        fx.tag("t-table-free-2").await,
        Some(StateTag::Reserved),
        "free services prepare too"
    );
    fx.preflight.refuse("withdrawn");
    fx.submit(&free_prepared, no_headers())
        .await
        .expect("in-body");
    assert!(
        fx.record("t-table-free-2").await.is_none(),
        "a free reservation is deleted, not reconciled"
    );
    assert_eq!(fx.exec.runs(), 2);
}

// ===========================================================================
// Once
// ===========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_paid_submit_redeems_once_and_runs_once() {
    let fx = pending(true)
        .await
        .serve(vec![A2aServicePolicy::Paid(offer(true))], false)
        .await;
    let b = brief("t-once-paid");

    // The requester's own verbs: prepare, then submit with the proof.
    let res = fx.reserve(&b).await;
    fx.gate.issue("q-1", &res.purchase_hash, PAYER);
    let prepared = PreparedTask {
        provider_node: fx.target,
        brief: b.clone(),
        offer_hash: offer(true).hash(),
        reservation: res.clone(),
    };
    let proof = TaskPaymentProof {
        quote_id: "q-1".to_string(),
        binding_sig: BINDING.to_vec(),
    };
    let ack = fx
        .caller()
        .submit_task_paid(&prepared, &proof)
        .await
        .expect("the paid submit");
    assert!(ack.accepted, "{:?}", ack.reason);
    assert_eq!(ack.task_id, "t-once-paid");

    fx.exec.wait_started().await;
    assert_eq!(fx.exec.runs(), 1, "exactly one run");
    let calls = fx.gate.calls();
    assert_eq!(calls.len(), 1, "exactly one redemption");
    assert_eq!(calls[0].tool_id, format!("net.a2a.task/{SERVICE}"));
    assert_eq!(calls[0].quote_id, "q-1");
    assert!(calls[0].has_binding);
    assert_eq!(
        calls[0].expected,
        purchase_hash(&res.admission_id, &res.commitment),
        "the provider computes the expected hash from its own reservation"
    );
    let record = fx.await_tag("t-once-paid", StateTag::Terminal).await;
    assert_eq!(
        record.state.status(),
        Some(TaskState::Completed {
            result_ref: RESULT.to_string()
        })
    );
    assert!(fx.ledger_has("t-once-paid").await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_identical_retry_returns_the_original_task_without_a_second_redeem() {
    let fx = pending(true)
        .await
        .serve(vec![A2aServicePolicy::Paid(offer(true))], true)
        .await;
    let b = brief("t-retry");

    let first = fx.buy_and_submit(&b, "q-1").await.expect("first submit");
    assert!(first.accepted, "{:?}", first.reason);
    fx.exec.wait_started().await;

    // The lost-reply case: the caller re-sends the SAME proof.
    let second = fx
        .submit(&b, paid_headers("q-1"))
        .await
        .expect("the resend");
    assert!(second.accepted);
    assert_eq!(second.task_id, first.task_id);
    assert_eq!(fx.gate.call_count(), 1, "no second redemption");
    assert_eq!(fx.exec.runs(), 1, "no second run");

    // ...and again once the task has finished and left the registry's
    // live view: the durable record answers.
    fx.exec.release(1);
    fx.await_tag("t-retry", StateTag::Terminal).await;
    fx.registry.forget(fx.owner(), "t-retry");
    let third = fx
        .submit(&b, paid_headers("q-1"))
        .await
        .expect("the late resend");
    assert!(third.accepted);
    assert_eq!(third.task_id, "t-retry");
    assert_eq!(fx.gate.call_count(), 1);
    assert_eq!(fx.exec.runs(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_identical_submits_converge_on_one_admission() {
    let fx = pending(true)
        .await
        .serve(vec![A2aServicePolicy::Paid(offer(true))], true)
        .await;
    let b = brief("t-race");
    let res = fx.reserve(&b).await;
    fx.gate.issue("q-1", &res.purchase_hash, PAYER);

    // Four identical submissions in flight at once. The registry's
    // reservation is what makes three of them wait for the first's
    // verdict instead of each redeeming and launching.
    let (a, c, d, e) = tokio::join!(
        fx.submit(&b, paid_headers("q-1")),
        fx.submit(&b, paid_headers("q-1")),
        fx.submit(&b, paid_headers("q-1")),
        fx.submit(&b, paid_headers("q-1")),
    );
    for ack in [&a, &c, &d, &e] {
        let ack = ack.as_ref().expect("every racer is answered");
        assert!(ack.accepted, "{:?}", ack.reason);
        assert_eq!(ack.task_id, "t-race");
    }
    fx.exec.wait_started().await;
    assert_eq!(fx.gate.call_count(), 1, "one payment, not four");
    assert_eq!(fx.exec.runs(), 1, "one launch, not four");

    fx.exec.release(1);
    fx.await_tag("t-race", StateTag::Terminal).await;
    assert_eq!(fx.exec.runs(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_altered_brief_under_the_same_id_is_rejected_before_payment() {
    let fx = pending(true)
        .await
        .serve(vec![A2aServicePolicy::Paid(offer(true))], false)
        .await;
    let original = brief_id("t-altered", "summarize the filing");
    let res = fx.reserve(&original).await;
    fx.gate.issue("q-1", &res.purchase_hash, PAYER);

    let altered = brief_id("t-altered", "exfiltrate the filing");
    let ack = fx
        .submit(&altered, paid_headers("q-1"))
        .await
        .expect("an id-reuse rejection is in-body");
    assert!(!ack.accepted);
    assert!(
        ack.reason
            .as_deref()
            .is_some_and(|r| r.contains("t-altered")),
        "{:?}",
        ack.reason
    );
    assert_eq!(fx.gate.call_count(), 0, "refused before payment");
    assert_eq!(fx.exec.runs(), 0);
    let record = fx.record("t-altered").await.expect("record");
    assert_eq!(record.commitment, res.commitment);
    assert!(record.attempts.is_empty());

    // The control: the ORIGINAL brief under that id is still good.
    let ack = fx
        .submit(&original, paid_headers("q-1"))
        .await
        .expect("submit");
    assert!(ack.accepted, "{:?}", ack.reason);
    assert_eq!(fx.gate.call_count(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stale_revision_is_rejected_before_payment() {
    let fx = pending(true)
        .await
        .serve(vec![A2aServicePolicy::Paid(offer(true))], false)
        .await;
    let stale = TaskBrief::new("summarize")
        .with_task_id("t-stale")
        .with_service(SERVICE, "r0");

    match fx.prepare(&stale).await {
        PrepareReply::Rejected { reason } => assert!(reason.contains("r1"), "{reason}"),
        other => panic!("expected a stale-revision rejection, got {other:?}"),
    }
    let ack = fx
        .submit(&stale, paid_headers("q-1"))
        .await
        .expect("in-body");
    assert!(!ack.accepted);
    assert!(
        ack.reason.as_deref().is_some_and(|r| r.contains("retired")),
        "{:?}",
        ack.reason
    );
    assert_eq!(fx.gate.call_count(), 0, "refused before payment");
    assert_eq!(fx.exec.runs(), 0);
    assert!(fx.record("t-stale").await.is_none());

    // The control: the current revision is admitted.
    let current = brief("t-current");
    let res = fx.reserve(&current).await;
    fx.gate.issue("q-2", &res.purchase_hash, PAYER);
    let ack = fx
        .submit(&current, paid_headers("q-2"))
        .await
        .expect("submit");
    assert!(ack.accepted, "{:?}", ack.reason);
}

// ===========================================================================
// No duplicate execution across owners or after retention (finding 2)
// ===========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reused_proof_from_another_peer_is_refused_and_never_launches() {
    // Three nodes: the provider, the peer that pays, and the peer that
    // replays what it paid.
    let fx = pending_with(2, true)
        .await
        .serve(vec![A2aServicePolicy::Paid(offer(true))], false)
        .await;
    let b = brief("t-shared-id");

    // Alice prepares, buys and runs.
    let alice_res = fx.reserve_as(0, &b).await;
    fx.gate.issue("q-alice", &alice_res.purchase_hash, PAYER);
    let ack = fx
        .submit_as(0, &b, paid_headers("q-alice"))
        .await
        .expect("alice submits");
    assert!(ack.accepted, "{:?}", ack.reason);
    fx.exec.wait_started().await;
    assert_eq!(fx.exec.runs(), 1);
    let after_alice = fx.gate.call_count();
    assert_eq!(after_alice, 1);

    // Bob prepares the identical brief. His reservation is his own, so
    // the provider expects a DIFFERENT purchase hash.
    let bob_res = fx.reserve_as(1, &b).await;
    assert_ne!(bob_res.admission_id, alice_res.admission_id);
    assert_ne!(bob_res.purchase_hash, alice_res.purchase_hash);

    // Bob replays Alice's headers.
    let refusal = fx
        .submit_as(1, &b, paid_headers("q-alice"))
        .await
        .expect_err("a replayed proof must be refused");
    assert_eq!(refusal.status, ERR_PAYMENT);
    assert_eq!(refusal.reason(), "input_binding_mismatch");
    let calls = fx.gate.calls();
    assert_eq!(calls.len(), 2, "bob's attempt did reach the gate");
    assert_eq!(
        calls[1].expected, bob_res.purchase_hash,
        "the provider asked for BOB's purchase hash"
    );
    assert_eq!(fx.exec.runs(), 1, "the replay never launched");
    let bob_record = fx.record_of(1, "t-shared-id").await.expect("bob's record");
    assert_eq!(bob_record.state.tag(), StateTag::Reserved);
    assert_eq!(bob_record.attempts.len(), 1);
    assert_eq!(bob_record.attempts[0].reason, "input_binding_mismatch");

    // ...and without a reservation at all, the same headers are refused
    // before the gate is even consulted.
    let other = brief("t-bob-only");
    let refusal = fx
        .submit_as(1, &other, paid_headers("q-alice"))
        .await
        .expect_err("no reservation");
    assert_eq!(refusal.reason(), "no_reservation");
    assert_eq!(fx.gate.call_count(), 2, "the gate was not consulted");
    assert_eq!(fx.exec.runs(), 1);

    // Alice's task is untouched, and Bob never got a ledger entry.
    assert!(fx.ledger_has("t-shared-id").await, "alice's launch");
    assert!(
        !fx.store()
            .ledger_has(fx.owner_of(1), "t-shared-id")
            .await
            .expect("ledger"),
        "bob never launched anything"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_retry_after_result_retention_never_relaunches_or_redeems() {
    let fx = pending(true)
        .await
        .serve(vec![A2aServicePolicy::Paid(offer(true))], false)
        .await;
    let b = brief("t-retired");

    let ack = fx.buy_and_submit(&b, "q-1").await.expect("submit");
    assert!(ack.accepted, "{:?}", ack.reason);
    fx.exec.wait_started().await;
    fx.await_tag("t-retired", StateTag::Terminal).await;

    // Retire the result: the registry entry and the durable record both
    // go, and only the ledger remains.
    assert!(fx.registry.forget(fx.owner(), "t-retired"));
    assert!(fx
        .store()
        .forget(fx.owner(), "t-retired")
        .await
        .expect("forget"));
    assert!(fx.record("t-retired").await.is_none());
    assert!(fx.ledger_has("t-retired").await, "the ledger outlives it");

    // NOTE — the only edit to this reviewer-supplied file beyond the
    // `review_*` counterexamples, which stay byte-identical to what was
    // exported. This function is a VENDORED COPY of the repository's own
    // `a2a_paid_admission.rs::a_retry_after_result_retention_never_
    // relaunches_or_redeems`, copied here so the probe file could stand
    // alone. The repair deliberately changed its subject: a paid submit
    // meeting a retired task now gets a structured terminal refusal
    // instead of prose, because the prose shape stranded a paid attempt
    // as retryable forever with no operator exit. This retargeting
    // mirrors, line for line, the one applied to the original in
    // `a2a_paid_admission.rs`; the row's real property — never relaunch,
    // never redeem twice — is unchanged and still asserted below.
    let refusal = fx
        .submit(&b, paid_headers("q-1"))
        .await
        .expect_err("a retired paid retry must be a structured terminal refusal");
    assert_eq!(refusal.reason(), "retired");
    let schematic = refusal
        .schematic
        .as_ref()
        .expect("the refusal carries a schematic");
    assert!(
        !schematic.recovery.safe_to_retry && !schematic.recovery.safe_to_requote,
        "a retired task is terminal in both directions: {:?}",
        schematic.recovery
    );
    assert_eq!(fx.gate.call_count(), 1, "no second redemption");
    assert_eq!(fx.exec.runs(), 1, "no second run");

    // Prepare answers the same way, so a caller cannot buy it again.
    match fx.prepare(&b).await {
        PrepareReply::Retired { task_id } => assert_eq!(task_id, "t-retired"),
        other => panic!("expected Retired, got {other:?}"),
    }
}

// ===========================================================================
// The launch claim (finding 5)
// ===========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_claim_write_never_launches_and_a_retry_launches_once() {
    let p = pending(true).await;
    let b = brief("t-claim");
    let journal = p.journal().await;
    seed_paid(&journal, p.owner(0), &b, &offer(true), "adm-1", "q-seed").await;
    // Arm the NEXT durable write to fail without touching the file. The
    // seeding writes are done, and S6′ writes nothing, so the next write
    // is the launch claim itself.
    journal.fail_next_write();
    let fx = p
        .serve_with(
            vec![A2aServicePolicy::Paid(offer(true))],
            false,
            Some(journal),
        )
        .await;

    let refusal = fx
        .submit(&b, paid_headers("q-seed"))
        .await
        .expect_err("a failed claim write must refuse");
    assert_eq!(refusal.status, ERR_PAYMENT);
    assert_eq!(refusal.reason(), "journal_unavailable");
    assert!(
        refusal.schematic().retryable,
        "the same proof resubmitted must be sanctioned"
    );
    assert!(refusal.schematic().recovery.safe_to_retry);

    assert_eq!(fx.exec.runs(), 0, "nothing ran");
    let record = fx.record("t-claim").await.expect("record");
    assert_eq!(
        record.state.tag(),
        StateTag::Paid,
        "the store is exactly as it was"
    );
    assert!(!fx.ledger_has("t-claim").await, "no ledger entry");

    // The retry re-enters at `Paid`, skips the gate, claims, and runs
    // exactly once.
    let ack = fx
        .submit(&b, paid_headers("q-seed"))
        .await
        .expect("the retry");
    assert!(ack.accepted, "{:?}", ack.reason);
    fx.exec.wait_started().await;
    assert_eq!(fx.exec.runs(), 1, "one run, from the retry");
    assert_eq!(fx.gate.call_count(), 0, "and still no redemption");
    assert!(fx.ledger_has("t-claim").await);
}

// ===========================================================================
// Retention
// ===========================================================================

/// The offer's `retention_secs` — not the registry's global default —
/// governs how long a configured task's terminal record is readable.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_offers_retention_governs_the_task_record() {
    let mut brief_retention = offer(true);
    brief_retention.retention_secs = 5;
    let fx = pending(true)
        .await
        .serve(vec![A2aServicePolicy::Paid(brief_retention)], false)
        .await;
    let b = brief("t-retention");

    let ack = fx.buy_and_submit(&b, "q-1").await.expect("submit");
    assert!(ack.accepted, "{:?}", ack.reason);
    fx.exec.wait_started().await;

    // The control shares the registry but carries no per-entry override,
    // so it must survive exactly where the configured entry does not.
    let local = TaskBrief::new("local work").with_task_id("t-local");
    fx.registry
        .submit(TaskOwner::Local, local, fx.exec.executor())
        .expect("local submit");
    fx.exec.wait_started().await;

    // Wait until both are terminal before applying retention.
    for _ in 0..200 {
        let a = fx.registry.status(fx.owner(), "t-retention");
        let b = fx.registry.status(TaskOwner::Local, "t-local");
        if a.as_ref().is_some_and(TaskState::is_terminal)
            && b.as_ref().is_some_and(TaskState::is_terminal)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let evicted = fx
        .registry
        .evict_terminal(TERMINAL_RECORD_TTL_SECS, now_secs() + 6);
    assert_eq!(evicted, 1, "exactly the short-retention entry");
    assert!(
        fx.registry.status(fx.owner(), "t-retention").is_none(),
        "the offer published 5s of result retention"
    );
    assert!(
        fx.registry.status(TaskOwner::Local, "t-local").is_some(),
        "an entry with no override keeps the global default"
    );

    // The durable record is a separate lifetime: the registry entry is
    // gone and the admission record still carries the outcome.
    fx.await_tag("t-retention", StateTag::Terminal).await;
}

// ===========================================================================
// Owner-only, uncharged control
// ===========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn another_peer_cannot_read_or_cancel_a_paid_task() {
    let fx = pending_with(2, true)
        .await
        .serve(vec![A2aServicePolicy::Paid(offer(true))], true)
        .await;
    let b = brief("t-private");

    let res = fx.reserve_as(0, &b).await;
    fx.gate.issue("q-1", &res.purchase_hash, PAYER);
    let ack = fx
        .submit_as(0, &b, paid_headers("q-1"))
        .await
        .expect("alice submits");
    assert!(ack.accepted, "{:?}", ack.reason);
    fx.exec.wait_started().await;

    // Bob knows the id — from a log line, a dashboard, a shared channel.
    assert!(
        fx.status_as(1, "t-private").await.is_none(),
        "another peer read the brief and its context refs"
    );
    assert!(
        !fx.callers[1]
            .cancel_task(fx.target, "t-private")
            .await
            .expect("cancel call"),
        "another peer stopped the work"
    );
    assert_eq!(fx.exec.runs(), 1, "still running for its owner");

    // The control: the owner can do both.
    let mine = fx.status("t-private").await.expect("owner reads");
    assert_eq!(mine.brief.task_id, "t-private");
    assert!(fx
        .caller()
        .cancel_task(fx.target, "t-private")
        .await
        .expect("cancel"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn status_cancel_prepare_and_describe_are_uncharged() {
    let fx = pending(true)
        .await
        .serve(vec![A2aServicePolicy::Paid(offer(true))], false)
        .await;
    let b = brief("t-free-verbs");

    let offers = fx.caller().describe_a2a(fx.target).await.expect("describe");
    assert_eq!(offers.len(), 1);
    fx.reserve(&b).await;
    assert!(
        fx.status("t-free-verbs").await.is_none(),
        "not launched yet"
    );
    assert!(!fx
        .caller()
        .cancel_task(fx.target, "t-free-verbs")
        .await
        .expect("cancel"));
    assert_eq!(
        fx.gate.call_count(),
        0,
        "describe / prepare / status / cancel never touch the gate"
    );

    // The control: the gate IS wired, so the zero above is not a dead
    // gate — the charged verb reaches it.
    let res = fx.reserve(&b).await;
    fx.gate.issue("q-1", &res.purchase_hash, PAYER);
    let ack = fx.submit(&b, paid_headers("q-1")).await.expect("submit");
    assert!(ack.accepted, "{:?}", ack.reason);
    assert_eq!(fx.gate.call_count(), 1);
}

// ===========================================================================
// Recovery (D6)
// ===========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_after_payment_before_claim_surfaces_paid_not_started_and_resumes_once() {
    let p = pending(true).await;
    let b = brief("t-crash-paid");
    let journal = p.journal().await;
    seed_paid(&journal, p.owner(0), &b, &offer(true), "adm-1", "q-seed").await;
    drop(journal);

    // The successor owner opens the journal the crashed one left.
    let journal = p.journal().await;
    let recovered = journal.recovered().to_vec();
    assert_eq!(recovered.len(), 1, "the successor inherits the admission");
    assert_eq!(
        recovered[0].status,
        TaskState::Interrupted {
            detail: DETAIL_PAID_NOT_STARTED.to_string()
        }
    );
    let fx = p
        .serve_with(
            vec![A2aServicePolicy::Paid(offer(true))],
            false,
            Some(journal),
        )
        .await;

    let status = fx.status("t-crash-paid").await.expect("status");
    assert_eq!(
        status.state,
        TaskState::Interrupted {
            detail: DETAIL_PAID_NOT_STARTED.to_string()
        }
    );

    // The retry launches once, without a second charge.
    let ack = fx.submit(&b, paid_headers("q-seed")).await.expect("resume");
    assert!(ack.accepted, "{:?}", ack.reason);
    fx.exec.wait_started().await;
    assert_eq!(fx.exec.runs(), 1);
    assert_eq!(fx.gate.call_count(), 0, "the work was already paid for");
    fx.await_tag("t-crash-paid", StateTag::Terminal).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_after_claim_surfaces_outcome_unknown_and_never_reruns() {
    let p = pending(true).await;
    let b = brief("t-crash-launched");
    let owner = p.owner(0);
    let journal = p.journal().await;
    seed_paid(&journal, owner, &b, &offer(true), "adm-1", "q-seed").await;
    journal
        .claim_launch(owner, "t-crash-launched", now_secs())
        .await
        .expect("seed the claim");
    drop(journal);

    let fx = p
        .serve(vec![A2aServicePolicy::Paid(offer(true))], false)
        .await;

    let status = fx.status("t-crash-launched").await.expect("status");
    assert_eq!(
        status.state,
        TaskState::Interrupted {
            detail: DETAIL_OUTCOME_UNKNOWN.to_string()
        },
        "a launch whose outcome was never recorded is ambiguous, not failed"
    );

    // A retry is acknowledged with the original id and runs nothing: the
    // work may already have happened, and this is the one thing that
    // must never be guessed.
    let ack = fx
        .submit(&b, paid_headers("q-seed"))
        .await
        .expect("acknowledged");
    assert!(ack.accepted);
    assert_eq!(ack.task_id, "t-crash-launched");
    assert_eq!(fx.exec.runs(), 0, "never relaunched");
    assert_eq!(fx.gate.call_count(), 0);
    assert_eq!(fx.tag("t-crash-launched").await, Some(StateTag::Launched));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_with_a_reconcile_record_keeps_refusing_until_resolved() {
    let p = pending(true).await;
    let b = brief("t-crash-reconcile");
    let owner = p.owner(0);
    let journal = p.journal().await;
    seed_paid(&journal, owner, &b, &offer(true), "adm-1", "q-seed").await;
    journal
        .transition(
            owner,
            "t-crash-reconcile",
            &[StateTag::Paid],
            AdmissionState::Reconcile {
                reason: "the corpus licence lapsed".to_string(),
                claimed_quote_id: Some("q-seed".to_string()),
                payer: Some(PAYER),
            },
            now_secs(),
        )
        .await
        .expect("seed the reconcile");
    drop(journal);

    let fx = p
        .serve(vec![A2aServicePolicy::Paid(offer(true))], false)
        .await;

    let status = fx.status("t-crash-reconcile").await.expect("status");
    assert_eq!(
        status.state,
        TaskState::Interrupted {
            detail: DETAIL_ADMISSION_REVOKED.to_string()
        }
    );
    let refusal = fx
        .submit(&b, paid_headers("q-seed"))
        .await
        .expect_err("still revoked");
    assert_eq!(refusal.reason(), "admission_revoked");
    assert_eq!(fx.exec.runs(), 0);
    assert_eq!(fx.gate.call_count(), 0);
    assert_eq!(fx.tag("t-crash-reconcile").await, Some(StateTag::Reconcile));

    // The operator's resolve is the only exit.
    fx.store()
        .resolve(owner, "t-crash-reconcile", TaskState::Cancelled, now_secs())
        .await
        .expect("operator resolve");
    let status = fx.status("t-crash-reconcile").await.expect("status");
    assert_eq!(status.state, TaskState::Cancelled);
    let ack = fx
        .submit(&b, paid_headers("q-seed"))
        .await
        .expect("a resolved record answers its recorded outcome");
    assert!(ack.accepted);
    assert_eq!(fx.exec.runs(), 0, "resolving never runs the work");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn describe_a2a_lists_offers_with_pricing_and_bounds() {
    let paid = offer(true);
    let free = offer_named(OTHER_SERVICE, false);
    let fx = pending(true)
        .await
        .serve(
            vec![
                A2aServicePolicy::Paid(paid.clone()),
                A2aServicePolicy::Free(free.clone()),
            ],
            false,
        )
        .await;

    let mut offers = fx.caller().describe_a2a(fx.target).await.expect("describe");
    offers.sort_by(|a, b| a.service_id.cmp(&b.service_id));
    assert_eq!(offers.len(), 2);
    let listed_paid = offers
        .iter()
        .find(|o| o.service_id == SERVICE)
        .expect("the paid offer");
    assert_eq!(listed_paid.pricing_terms.as_deref(), Some(TERMS));
    assert_eq!(listed_paid.bounds, paid.bounds);
    assert_eq!(listed_paid.retention_secs, paid.retention_secs);
    assert_eq!(
        listed_paid.hash(),
        paid.hash(),
        "what a caller commits to is what the provider serves"
    );
    let listed_free = offers
        .iter()
        .find(|o| o.service_id == OTHER_SERVICE)
        .expect("the free offer");
    assert_eq!(listed_free.pricing_terms, None);
}

/// The legacy free path is untouched: it serves no describe, and it
/// ignores `service` / `revision` entirely.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn legacy_serve_a2a_ignores_the_service_and_revision_fields() {
    let provider = mesh().await;
    let caller = mesh().await;
    let exec = ExecProbe::new(false);
    let _handles = provider
        .serve_a2a(TaskRegistry::new(), exec.executor())
        .expect("legacy serve");
    connect_all(&provider, &[&caller]).await;
    let target = provider.inner().node_id();

    // A brief naming a service this node never heard of, at a revision
    // it never published.
    let b = TaskBrief::new("legacy work")
        .with_task_id("t-legacy")
        .with_service("svc.does-not-exist", "r99");
    let ack = caller.submit_task(target, &b).await.expect("submit");
    assert!(
        ack.accepted,
        "the free path must ignore the catalog fields: {:?}",
        ack.reason
    );
    exec.wait_started().await;
    assert_eq!(exec.runs(), 1);

    // ...and free-by-omission is not an offer.
    assert!(
        caller.describe_a2a(target).await.is_err(),
        "the legacy path serves no describe"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn review_distinct_prepares_cannot_overbook_one_slot() {
    let mut o = offer(true);
    o.bounds.max_in_flight = 1;
    let f = pending(true)
        .await
        .serve(vec![A2aServicePolicy::Paid(o)], false)
        .await;
    let a = brief("review-cap-a");
    let b = brief("review-cap-b");
    let c = brief("review-cap-c");
    let d = brief("review-cap-d");
    let replies = tokio::join!(f.prepare(&a), f.prepare(&b), f.prepare(&c), f.prepare(&d));
    let all = [replies.0, replies.1, replies.2, replies.3];
    let admitted = all
        .iter()
        .filter(|r| matches!(r, PrepareReply::Reservation(_)))
        .count();
    assert_eq!(
        admitted, 1,
        "max_in_flight=1 yielded {admitted} purchasable reservations: {all:?}"
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn review_stale_financial_transition_rejects_replacement_admission() {
    let o = offer(true);
    let horizon = o.reservation_retention_secs;
    let f = pending(true)
        .await
        .serve(vec![A2aServicePolicy::Paid(o)], false)
        .await;
    let first = brief_id("review-aba", "work A");
    let old = match f.prepare(&first).await {
        PrepareReply::Reservation(r) => r,
        x => panic!("{x:?}"),
    };
    assert_eq!(f.store().prune(now_secs() + horizon + 1).await.unwrap(), 1);
    let replacement = brief_id("review-aba", "work B");
    let new = match f.prepare(&replacement).await {
        PrepareReply::Reservation(r) => r,
        x => panic!("{x:?}"),
    };
    assert_ne!(old.admission_id, new.admission_id);
    f.gate.issue("quote-for-A", &old.purchase_hash, PAYER);
    let evidence = f
        .gate
        .redeem(TaskPaymentClaim {
            tool_id: "net.a2a.task/svc.summarize",
            quote_id: "quote-for-A",
            binding: &BINDING,
            expected_input_hash: &old.purchase_hash,
        })
        .await
        .unwrap();
    let changed = f
        .store()
        .transition(
            f.owner(),
            "review-aba",
            &[StateTag::Reserved],
            AdmissionState::Paid {
                quote_id: evidence.quote_id,
                payer: evidence.payer,
            },
            now_secs(),
        )
        .await;
    eprintln!(
        "new record after stale A decision: {:?}",
        f.record("review-aba").await
    );
    assert!(
        changed.is_err(),
        "financial transition from admission A modified replacement B"
    );
}
