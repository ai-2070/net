//! Live agent-to-agent task handoff over the mesh — the networked half of
//! Hermes V2 Phase 3.
//!
//! The wire is **direct-addressed nRPC**, reusing the mesh's own handshake and
//! request/response path (the same idiom as `mesh_enroll`):
//!
//! * **Executor** — [`Mesh::serve_a2a`] registers three services backed by a
//!   [`TaskRegistry`] + a host [`TaskExecutor`]: [`A2A_TASK_SERVICE`] accepts a
//!   [`TaskBrief`] and spawns the executor (returning a [`TaskAck`]),
//!   [`A2A_STATUS_SERVICE`] answers a task's [`TaskRecord`], and
//!   [`A2A_CANCEL_SERVICE`] trips its cancel token. Its running dispatch loop
//!   answers routed handshakes from any in-root peer — zero pairing ceremony.
//!
//! # Ownership
//!
//! **Submission is open to every in-root peer; inspection and
//! cancellation are not.** Each task is bound at submission to the
//! AEAD-authenticated peer that submitted it, and status and cancel
//! only ever see that peer's own tasks.
//!
//! "The peer that submitted it" means the peer whose session delivered
//! the frame — the **deliverer**, not an end-to-end origin. Under a
//! deployment that relays nRPC through an intermediary, the relay is
//! the owner and everything it forwards shares that ownership. That is
//! the documented limit of public nRPC attribution rather than a gap in
//! this binding; end-to-end provenance needs a PROTECTED service or an
//! application-level signature. See
//! [`RpcContext::session_peer`](net::adapter::net::cortex::RpcContext::session_peer).
//!
//! A task id is a name, not a bearer capability. It is client-generated,
//! it travels through logs, dashboards and polling loops as an ordinary
//! identifier, and [`TaskRecord`] carries the complete prompt and
//! context refs — so learning an id must not confer the right to read
//! the brief or stop the work. Same-root reachability means permission
//! to *submit* work, not permission to inspect and cancel every other
//! agent's.
//!
//! Third-party observation, if it is ever wanted, needs an explicit
//! delegated capability rather than a leaked id.
//! * **Requester** — [`Mesh::submit_task`] / [`Mesh::task_status`] /
//!   [`Mesh::cancel_task`] `call` those services by the executor's node id. The
//!   requester submits, keeps working, polls, and cancels — and the executor
//!   **demonstrably stops** (cooperative cancellation, `a2a`).
//!
//! In-root reachability is assumed (both agents are enrolled on the same mesh);
//! the caller connects to the executor node the usual way (`connect_via` for a
//! routed handshake) before submitting. A2A is for **parallelism** — briefs
//! carry Datafort context refs and results come back as artifact refs, because
//! the executor doesn't share the requester's memory.
//!
//! # Configured services (the catalog-driven path)
//!
//! [`Mesh::serve_a2a_configured`] is the strict sibling of
//! [`Mesh::serve_a2a`]: a brief must name a service in an
//! [`A2aServiceConfig`] catalog, each entry is *explicitly*
//! [`Free`](A2aServicePolicy::Free) or [`Paid`](A2aServicePolicy::Paid),
//! and a paid service is admitted through a payment gate against a
//! durable admission record before any work starts. It serves five
//! services: `net.a2a.describe` (offers), `net.a2a.prepare` (validate +
//! reserve + mint the admission id a purchase binds to), and the three
//! legacy verbs — submit, status, cancel.
//!
//! The ordering is the whole point: **prepare → purchase → submit →
//! claim → launch**. Validation, the application
//! [`TaskPreflight`] and capacity admission all run *before a quote
//! exists*, so an invalid, oversized, unauthorized or over-capacity
//! brief is refused with nothing to reconcile; and the launch is claimed
//! durably (with a ledger entry that outlives the result) before the
//! executor is spawned, so nothing runs twice and nothing runs unpaid.
//!
//! # Principal
//!
//! [`A2aPrincipal`] decides who a submission is attributed to:
//!
//! | Variant | Owner | Topology |
//! |---|---|---|
//! | [`SessionPeer`](A2aPrincipal::SessionPeer) | `TaskOwner::Peer(ctx.session_peer)` — the AEAD-authenticated deliverer | **direct sessions only**; a relay is the deliverer and owns everything it forwards |
//! | [`OrgAdmitted`](A2aPrincipal::OrgAdmitted) | `TaskOwner::Entity(admitted.caller)` — the entity an organization admission proof names | end-to-end; the five services register as PROTECTED |
//!
//! Under `SessionPeer` a paid admission records the payer the gate
//! attributed the payment to and binds it to the reservation, but the
//! requester is only ever the delivering peer — which is why
//! `OrgAdmitted` is the variant that additionally *enforces*
//! `payer == admitted.caller`.
//!
//! # The one wire addition
//!
//! [`TaskState::Interrupted`](crate::a2a::TaskState::Interrupted) is
//! the only new state this slice puts on the status wire, and **only
//! the configured path mints it** — the free registry never does, so
//! a deployment that configures no catalog keeps seeing exactly the
//! states it always saw.
//!
//! Where it does appear, the cost is stated rather than hidden:
//! `TaskState` is a serde-tagged enum, so a **Rust requester built
//! before this slice cannot decode a status reply carrying it** and
//! gets [`A2aFlowError::Decode`] instead of a record. The Python and
//! Node bindings hand the status back as a JSON string and pass the
//! new tag through untouched, so they need no upgrade to read one.
//!
//! Everything else here is additive and decodes on an old build:
//! the two uncharged services ([`A2A_DESCRIBE_SERVICE`],
//! [`A2A_PREPARE_SERVICE`]), the two optional brief fields
//! ([`TaskBrief::service`], [`TaskBrief::revision`]) that a legacy
//! free server ignores, the two payment request headers already
//! spoken by paid tools ([`HDR_PAYMENT_QUOTE`],
//! [`HDR_PAYMENT_BINDING`]), and the [`ERR_PAYMENT`] +
//! [`HDR_FAILURE_SCHEMATIC`] refusal shape.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::a2a::{
    await_admission_verdict, purchase_hash, random_id, task_commitment, A2aOffer, Admission,
    AdmissionReservation, CancelToken, PrepareReply, PreparedTask, SubmitRejection, TaskAck,
    TaskBrief, TaskExecutor, TaskOwner, TaskRecord, TaskRegistry, TerminalHook,
};
use crate::a2a_journal::{
    now_secs, A2aAdmissionJournal, A2aAdmissions, AdmissionRecord, AdmissionState, AttemptNote,
    InsertOutcome, JournalOwner, SharedAdmissionStore, StateTag,
};
use crate::a2a_payment::{
    TaskAdmissionGate, TaskPaymentClaim, TaskPaymentProof, A2A_DESCRIBE_SERVICE,
    A2A_PREPARE_SERVICE,
};
use crate::mesh::Mesh;
use crate::mesh_rpc::{
    CallOptions, CallOptionsExt, CallOptionsTyped, RpcContext, RpcError, RpcHandler,
    RpcHandlerError, RpcResponsePayload, RpcStatus, ServeError, ServeHandle,
    NRPC_TYPED_HANDLER_ERROR,
};
use crate::org::OrgAccess;
use crate::tool_payment::{
    failure_vocab, FailureSchematic, Recovery, ERR_PAYMENT, HDR_FAILURE_SCHEMATIC,
    HDR_PAYMENT_BINDING, HDR_PAYMENT_QUOTE, TAG_PAYMENT_FAILURE,
};

/// The nRPC service an executor serves to accept a [`TaskBrief`] (submit).
pub const A2A_TASK_SERVICE: &str = "net.a2a.task";
/// The nRPC service an executor serves to answer a task's [`TaskRecord`].
pub const A2A_STATUS_SERVICE: &str = "net.a2a.status";
/// The nRPC service an executor serves to cancel a task.
pub const A2A_CANCEL_SERVICE: &str = "net.a2a.cancel";

/// Errors from the requester-side A2A flow.
#[derive(Debug, thiserror::Error)]
pub enum A2aFlowError {
    /// Dialing the executor or calling a service failed.
    #[error("a2a transport failed: {0}")]
    Transport(String),
    /// A response could not be decoded.
    #[error("a2a decode error: {0}")]
    Decode(String),
    /// The provider refused the submission on payment or admission
    /// grounds: the `ERR_PAYMENT` application error, carrying the
    /// provider's human message and — for a schematic-aware provider —
    /// the structured verdict that says which invariant refused, who can
    /// fix it, and what recovery is safe.
    ///
    /// `schematic` is `None` when the reply carried no schematic header,
    /// carried more than one, or carried bytes that did not parse as a
    /// `net.payment.failure@1` object: the discipline is exactly one
    /// valid header or the human message alone.
    #[error("a2a payment refused: {message}")]
    PaymentRefused {
        /// The provider's human-readable refusal.
        message: String,
        /// The structured verdict, when the provider sent exactly one.
        schematic: Option<Box<FailureSchematic>>,
    },
}

/// Encode a task id as a request body (a JSON string). One place so the status
/// and cancel services agree with their callers.
fn task_ref_bytes(task_id: &str) -> Vec<u8> {
    serde_json::to_vec(task_id).unwrap_or_default()
}

/// The owner a request is attributed to.
///
/// Always the AEAD-authenticated session peer that delivered the frame —
/// never `caller_origin`, which is routing metadata the sender chooses,
/// and never anything in the request body.
fn owner_of(ctx: &RpcContext) -> TaskOwner {
    TaskOwner::Peer(ctx.session_peer)
}

/// The requester side calls these services through `call_typed` with
/// `Req = Resp = Vec<u8>`, so the transport JSON-encodes the payload
/// bytes as an array of numbers. Moving the handlers onto the
/// context-bearing `serve_rpc` path means unwrapping and re-wrapping
/// that envelope here, where `serve_rpc_typed` used to do it.
///
/// Kept exactly as it was rather than switching to a raw body: the
/// wire is spoken by the Node and Python bindings and by any peer on
/// an older build, and ownership is a server-side property. A
/// cross-version A2A break would be an odd thing to buy with an
/// authorization fix.
fn typed_body(raw: &[u8]) -> Vec<u8> {
    serde_json::from_slice(raw).unwrap_or_default()
}

/// Wrap `body` in the same envelope, in an `Ok` response. A2A answers
/// application outcomes in the body (`TaskAck { accepted: false }`,
/// `null`, `false`) rather than through transport status, so the
/// requester reads one shape.
fn json_ok(body: Vec<u8>) -> RpcResponsePayload {
    RpcResponsePayload {
        status: RpcStatus::Ok,
        headers: Vec::new(),
        body: bytes::Bytes::from(serde_json::to_vec(&body).unwrap_or_default()),
    }
}

/// The failure schematic on a refusal's reply headers, if the provider
/// sent exactly one valid header.
///
/// The producer/consumer discipline, verbatim from the paid-tool path:
/// zero headers, duplicates, or bytes that are not a
/// `net.payment.failure@1` object all read as absent, and the caller
/// falls back to the human message. Never guessing a verdict from a
/// malformed one is the point — a fabricated `funds_moved` is worse
/// than none.
fn schematic_of(headers: &[(String, Vec<u8>)]) -> Option<FailureSchematic> {
    let entries: Vec<&Vec<u8>> = headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case(HDR_FAILURE_SCHEMATIC))
        .map(|(_, value)| value)
        .collect();
    match entries.as_slice() {
        [bytes] => FailureSchematic::from_header_bytes(bytes),
        _ => None,
    }
}

/// `net.a2a.task` — accept a brief, attributed to the authenticated
/// submitter.
struct SubmitHandler {
    registry: TaskRegistry,
    executor: Arc<dyn TaskExecutor>,
}

#[async_trait::async_trait]
impl RpcHandler for SubmitHandler {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        // Never fails out of band: a malformed or refused brief answers a
        // `TaskAck { accepted: false }` the requester reads.
        let ack = match TaskBrief::decode(&typed_body(&ctx.payload.body)) {
            Ok(brief) => {
                match self
                    .registry
                    .submit(owner_of(&ctx), brief, Arc::clone(&self.executor))
                {
                    Ok(task_id) => TaskAck {
                        task_id,
                        accepted: true,
                        reason: None,
                    },
                    Err(rejection) => TaskAck {
                        task_id: String::new(),
                        accepted: false,
                        reason: Some(rejection.to_string()),
                    },
                }
            }
            Err(e) => TaskAck {
                task_id: String::new(),
                accepted: false,
                reason: Some(e.to_string()),
            },
        };
        Ok(json_ok(ack.encode()))
    }
}

/// `net.a2a.status` — answer the caller's own task, or `null`.
struct StatusHandler {
    registry: TaskRegistry,
}

#[async_trait::async_trait]
impl RpcHandler for StatusHandler {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        let task_id: String =
            serde_json::from_slice(&typed_body(&ctx.payload.body)).unwrap_or_default();
        // Another submitter's task reads as unknown rather than
        // forbidden — see `TaskRegistry::status`. `TaskRecord` carries
        // the full prompt and context refs, so "not yours" and "no such
        // task" must be indistinguishable.
        let record: Option<TaskRecord> = self.registry.record(owner_of(&ctx), &task_id);
        Ok(json_ok(serde_json::to_vec(&record).unwrap_or_default()))
    }
}

/// `net.a2a.cancel` — cancel the caller's own task.
struct CancelHandler {
    registry: TaskRegistry,
}

#[async_trait::async_trait]
impl RpcHandler for CancelHandler {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        let task_id: String =
            serde_json::from_slice(&typed_body(&ctx.payload.body)).unwrap_or_default();
        let cancelled = self.registry.cancel(owner_of(&ctx), &task_id);
        Ok(json_ok(serde_json::to_vec(&cancelled).unwrap_or_default()))
    }
}

// ===========================================================================
// The configured serving path: catalog, preflight, principal (D1 / D5)
// ===========================================================================

/// One catalog entry: an [`A2aOffer`] plus the single decision a
/// provider makes about it.
///
/// Free versus paid is **provider configuration** — a caller never
/// selects it, and a configured paid service never degrades to free
/// (the serve-time invariants refuse to start instead).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum A2aServicePolicy {
    /// Served for nothing. The offer must carry **no**
    /// `pricing_terms` — an announced price with no gate behind it
    /// would be unenforceable, so it is refused at serve time
    /// (`ServeError::UnenforceablePricing`).
    ///
    /// Free means no *payment*, never no *policy*: a free service still
    /// validates bounds, runs the application [`TaskPreflight`], and
    /// admits against `max_in_flight`.
    Free(A2aOffer),
    /// Served against a redeemed payment. The offer must carry
    /// `pricing_terms` (`ServeError::MissingPricingTerms` otherwise),
    /// and the configuration must carry both a
    /// [`TaskAdmissionGate`] and an admission journal
    /// (`ServeError::A2aPaidMisconfigured` otherwise).
    Paid(A2aOffer),
}

impl A2aServicePolicy {
    /// The offer, whichever arm this is.
    pub fn offer(&self) -> &A2aOffer {
        match self {
            A2aServicePolicy::Free(offer) | A2aServicePolicy::Paid(offer) => offer,
        }
    }

    /// Whether this service charges.
    pub fn is_paid(&self) -> bool {
        matches!(self, A2aServicePolicy::Paid(_))
    }
}

/// The application's own admission check, run at prepare **and again**
/// at submit.
///
/// This is where schema, size beyond [`A2aOffer::check_bounds`],
/// resource availability and provider authority live — everything the
/// protocol cannot know. It must be **pure**: it runs twice per task by
/// design (authority can change between a prepare and the submit that
/// follows), and a refusal at prepare reserves nothing.
///
/// A refusal at submit is *not* symmetric with one at prepare: for an
/// admission that may already have been paid for, it is post-payment
/// revocation and becomes a `Reconcile` record with an
/// `admission_revoked` refusal, never an unpaid rejection.
#[async_trait::async_trait]
pub trait TaskPreflight: Send + Sync {
    /// `Ok(())` admits; `Err(reason)` refuses, with `reason` travelling
    /// to the caller verbatim.
    async fn preflight(
        &self,
        owner: TaskOwner,
        offer: &A2aOffer,
        brief: &TaskBrief,
    ) -> Result<(), String>;
}

/// Who a submission is attributed to — the `(owner, task id)` key every
/// registry entry, admission record and ledger entry is stored under.
///
/// See the module docs for the table and the topology each variant
/// supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum A2aPrincipal {
    /// `TaskOwner::Peer(ctx.session_peer)`: the AEAD-authenticated peer
    /// whose session delivered the frame.
    ///
    /// **Supported topology: direct sessions only.** Under a deployment
    /// that relays nRPC, the relay is the deliverer and owns everything
    /// it forwards — the documented limit of public nRPC attribution,
    /// not a gap in this binding. A provider that needs an end-to-end
    /// principal uses [`OrgAdmitted`](Self::OrgAdmitted).
    #[default]
    SessionPeer,
    /// `TaskOwner::Entity(admitted.caller)`: the entity an organization
    /// admission proof names. All five services register as PROTECTED
    /// (owner-scoped or grant-audience per the [`OrgAccess`] arm), so
    /// `ctx.org_admission` is present and verified before a handler
    /// runs.
    ///
    /// The variant that additionally **enforces** `payer ==
    /// admitted.caller` on every paid admission: a payment made by one
    /// entity cannot admit another's task.
    OrgAdmitted(OrgAccess),
}

/// The configuration [`Mesh::serve_a2a_configured`] validates and then
/// serves.
///
/// Fail-closed by construction: every invariant that could make a paid
/// service serve work for free is checked before a single service is
/// registered.
pub struct A2aServiceConfig {
    /// `service_id` → policy. A brief must name a service in this map;
    /// anything else is `SubmitRejection::UnknownService`. The key and
    /// the offer's own `service_id` must agree — a commitment is
    /// computed against the offer, so a mismatch would admit work under
    /// terms the caller never saw.
    pub services: BTreeMap<String, A2aServicePolicy>,
    /// The payment gate. **Required** if any service is
    /// [`Paid`](A2aServicePolicy::Paid); unused otherwise (a free
    /// catalog links no payment crate at all).
    pub payment: Option<Arc<dyn TaskAdmissionGate>>,
    /// The application preflight. Optional — absent means "the offer's
    /// bounds and capacity are the whole policy".
    pub preflight: Option<Arc<dyn TaskPreflight>>,
    /// Who a submission is attributed to.
    pub principal: A2aPrincipal,
    /// The durable admission store. **Required** if any service is
    /// [`Paid`](A2aServicePolicy::Paid); opening it took
    /// lifetime-exclusive ownership of the journal, and serving clones
    /// that ownership into every handler and every launched task.
    ///
    /// `None` with an all-free catalog runs on the in-memory
    /// [`A2aAdmissions`] — the same state table, the same capacity
    /// accounting, the same ledger, for the process lifetime.
    pub journal: Option<A2aAdmissionJournal>,
}

impl A2aServiceConfig {
    /// A configuration serving `services` to the session peer, with no
    /// gate, no preflight and no journal.
    pub fn new(services: BTreeMap<String, A2aServicePolicy>) -> Self {
        Self {
            services,
            payment: None,
            preflight: None,
            principal: A2aPrincipal::SessionPeer,
            journal: None,
        }
    }

    /// Install the payment gate (builder-style).
    #[must_use]
    pub fn with_payment(mut self, gate: Arc<dyn TaskAdmissionGate>) -> Self {
        self.payment = Some(gate);
        self
    }

    /// Install the application preflight (builder-style).
    #[must_use]
    pub fn with_preflight(mut self, preflight: Arc<dyn TaskPreflight>) -> Self {
        self.preflight = Some(preflight);
        self
    }

    /// Choose the principal (builder-style).
    #[must_use]
    pub fn with_principal(mut self, principal: A2aPrincipal) -> Self {
        self.principal = principal;
        self
    }

    /// Install the durable admission journal (builder-style).
    #[must_use]
    pub fn with_journal(mut self, journal: A2aAdmissionJournal) -> Self {
        self.journal = Some(journal);
        self
    }

    /// Whether any configured service charges.
    fn has_paid(&self) -> bool {
        self.services.values().any(A2aServicePolicy::is_paid)
    }
}

/// What [`Mesh::serve_a2a_configured`] returns: the five serve handles
/// and the admission store they were wired to.
///
/// Hold it for as long as this node should accept tasks — dropping it
/// unregisters the services.
///
/// The store rides along because it is the **operator's queue**: a
/// provider needs
/// [`unresolved`](crate::a2a_journal::AdmissionStore::unresolved) to see
/// admissions whose money is unaccounted for,
/// [`resolve`](crate::a2a_journal::AdmissionStore::resolve) to close
/// them, and [`prune`](crate::a2a_journal::AdmissionStore::prune) to
/// apply retention — and the journal
/// was consumed by the configuration, so this is the only place that
/// handle can come from. It is also the only way to observe the
/// in-memory store an all-free catalog runs on.
pub struct A2aServing {
    /// The five registrations: submit, status, cancel, prepare,
    /// describe.
    pub handles: Vec<ServeHandle>,
    /// The admission store the handlers write through.
    pub store: SharedAdmissionStore,
    /// The journal's exclusive-ownership handle, held for as long as
    /// these handles are — so dropping the *journal* before the serving
    /// path cannot release the lock under a live handler. `None` for the
    /// in-memory store, which has no ownership contract.
    _owner: Option<Arc<JournalOwner>>,
}

// ---------------------------------------------------------------------------
// Handler-authored refusals
// ---------------------------------------------------------------------------

/// The shape every handler-authored refusal shares: the object tag, the
/// payment code, `handler_executed: false` (nothing ran, by
/// construction), and the most conservative money facts. Each
/// constructor below overrides exactly the fields its row of the
/// [`FailureSchematic`] table differs in.
fn base_schematic(stage: &str, reason: &str, message: String, tool_id: &str) -> FailureSchematic {
    FailureSchematic {
        object: TAG_PAYMENT_FAILURE.to_string(),
        code: failure_vocab::CODE_PAYMENT.to_string(),
        stage: stage.to_string(),
        reason: reason.to_string(),
        message,
        retryable: false,
        recovery: Recovery {
            class: failure_vocab::CLASS_NON_RECOVERABLE.to_string(),
            actor: failure_vocab::ACTOR_PROVIDER_OPERATOR.to_string(),
            safe_to_retry: false,
            safe_to_requote: false,
            next_action: None,
        },
        handler_executed: false,
        funds_moved: failure_vocab::FUNDS_UNKNOWN.to_string(),
        prior_payment: failure_vocab::PRIOR_UNKNOWN.to_string(),
        quote_id: None,
        tool_id: Some(tool_id.to_string()),
        extra: Default::default(),
    }
}

/// `missing_quote`: a paid task submitted with no quote header. The gate
/// was never consulted, so nothing was consumed.
fn schematic_missing_quote(tool_id: &str) -> FailureSchematic {
    let mut s = base_schematic(
        failure_vocab::STAGE_ADMISSION,
        "missing_quote",
        "paid A2A task submitted without a payment quote header".to_string(),
        tool_id,
    );
    s.recovery.class = failure_vocab::CLASS_NEW_QUOTE_REQUIRED.to_string();
    s.recovery.actor = failure_vocab::ACTOR_CALLER_AGENT.to_string();
    s.recovery.safe_to_requote = true;
    s.recovery.next_action = Some("request_new_quote".to_string());
    s.funds_moved = failure_vocab::FUNDS_NO.to_string();
    s.prior_payment = failure_vocab::PRIOR_NONE.to_string();
    s
}

/// `binding_required`: bearer presentation is never enough for a task.
/// A task is a long-running side effect, so possession of the quote id
/// is not evidence that the payer authorized *this* submission.
fn schematic_binding_required(tool_id: &str, quote_id: &str) -> FailureSchematic {
    let mut s = base_schematic(
        failure_vocab::STAGE_REDEEM,
        "binding_required",
        "paid A2A task submitted without the payment binding signature; \
         a task admission always requires it"
            .to_string(),
        tool_id,
    );
    s.recovery.class = failure_vocab::CLASS_CALLER_CONFIGURATION_ERROR.to_string();
    s.recovery.actor = failure_vocab::ACTOR_CALLER_OPERATOR.to_string();
    s.recovery.next_action = Some("fix_payment_client".to_string());
    s.quote_id = Some(quote_id.to_string());
    s
}

/// `binding_rejected`: the proof does not belong to the purchase it was
/// presented for. A mismatch to report, never something to retry or
/// re-buy.
fn schematic_binding_rejected(
    tool_id: &str,
    quote_id: Option<&str>,
    message: String,
) -> FailureSchematic {
    let mut s = base_schematic(
        failure_vocab::STAGE_REDEEM,
        "binding_rejected",
        message,
        tool_id,
    );
    s.recovery.class = failure_vocab::CLASS_SECURITY_VIOLATION.to_string();
    s.recovery.actor = failure_vocab::ACTOR_CALLER_OPERATOR.to_string();
    s.quote_id = quote_id.map(str::to_string);
    s
}

/// `no_reservation`: the provider has no admission record for this task.
///
/// The money facts are `unknown`/`unknown` and that is load-bearing: a
/// provider with no record cannot claim no money moved. This is exactly
/// the state a caller who paid and then submitted after the
/// reservation's retention window arrives in, and the caller's own
/// purchase record is the reconciliation evidence.
fn schematic_no_reservation(tool_id: &str) -> FailureSchematic {
    let mut s = base_schematic(
        failure_vocab::STAGE_ADMISSION,
        "no_reservation",
        "no admission reservation exists for this task; prepare before submitting".to_string(),
        tool_id,
    );
    s.recovery.actor = failure_vocab::ACTOR_CALLER_OPERATOR.to_string();
    s.recovery.next_action = Some("contact_provider_operator".to_string());
    s
}

/// `admission_revoked`: a payment may already have landed and the
/// provider can no longer admit the work. Operator reconciliation, never
/// an unpaid rejection — and never retryable or re-quotable, because a
/// second purchase would be a second charge for work the provider has
/// already refused.
fn schematic_admission_revoked(tool_id: &str, quote_id: Option<&str>) -> FailureSchematic {
    let mut s = base_schematic(
        failure_vocab::STAGE_ADMISSION,
        "admission_revoked",
        "this task's admission was revoked after payment; it is held for operator \
         reconciliation and will not run"
            .to_string(),
        tool_id,
    );
    s.recovery.next_action = Some("contact_provider_operator".to_string());
    s.quote_id = quote_id.map(str::to_string);
    s
}

/// `journal_unavailable`: the durable admission store refused a write.
/// The one retryable row of the A2A set — nothing ran, the store is
/// unchanged, and the *same* proof resubmitted succeeds once the store
/// recovers.
fn schematic_journal_unavailable(tool_id: &str, detail: &str) -> FailureSchematic {
    let mut s = base_schematic(
        failure_vocab::STAGE_ADMISSION,
        "journal_unavailable",
        format!("the admission store refused a write, so nothing was started: {detail}"),
        tool_id,
    );
    s.retryable = true;
    s.recovery.class = failure_vocab::CLASS_PROVIDER_CONFIGURATION_ERROR.to_string();
    s.recovery.safe_to_retry = true;
    s.recovery.safe_to_requote = true;
    s.recovery.next_action = Some("retry_later".to_string());
    s
}

/// A payment or admission refusal on the full-fidelity reply channel:
/// the human message stays the body (byte-identical to what the wire has
/// always carried for a paid refusal) and the schematic rides exactly
/// one reply header.
///
/// Returned as `Ok(payload)` because the `RpcHandlerError` convenience
/// channel flattens headers away — the same reason
/// `PaidToolHandler` does it.
fn payment_refusal(schematic: &FailureSchematic) -> RpcResponsePayload {
    RpcResponsePayload {
        status: RpcStatus::Application(ERR_PAYMENT),
        headers: schematic.header_entry().into_iter().collect(),
        body: schematic.message.clone().into(),
    }
}

/// A gate denial, passed through untouched: the gate's own message and
/// its own schematic.
fn gate_refusal(message: String, schematic: &FailureSchematic) -> RpcResponsePayload {
    RpcResponsePayload {
        status: RpcStatus::Application(ERR_PAYMENT),
        headers: schematic.header_entry().into_iter().collect(),
        body: message.into(),
    }
}

/// An accepted submission.
fn ack_accepted(task_id: String) -> RpcResponsePayload {
    json_ok(
        TaskAck {
            task_id,
            accepted: true,
            reason: None,
        }
        .encode(),
    )
}

/// A non-payment rejection: in-body, exactly like the free path, so a
/// requester reads one shape for everything that is not a payment
/// verdict.
fn ack_refused(reason: impl std::fmt::Display) -> RpcResponsePayload {
    json_ok(
        TaskAck {
            task_id: String::new(),
            accepted: false,
            reason: Some(reason.to_string()),
        }
        .encode(),
    )
}

/// One request header's raw bytes, if present.
fn request_header<'a>(headers: &'a [(String, Vec<u8>)], name: &str) -> Option<&'a [u8]> {
    headers
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.as_slice())
}

// ---------------------------------------------------------------------------
// Catalog + shared handler state
// ---------------------------------------------------------------------------

/// The configured services, and the one place a brief is resolved
/// against them.
struct Catalog {
    services: BTreeMap<String, A2aServicePolicy>,
}

impl Catalog {
    /// Resolve `brief` to the offer that admits it: named service,
    /// current revision, per-brief bounds — in that order, and all of it
    /// before anything is reserved or charged.
    fn resolve(&self, brief: &TaskBrief) -> Result<&A2aOffer, SubmitRejection> {
        let named = brief.service.as_deref().unwrap_or_default();
        let offer = self
            .services
            .get(named)
            .map(A2aServicePolicy::offer)
            .ok_or_else(|| SubmitRejection::UnknownService {
                service: named.to_string(),
            })?;
        let got = brief.revision.as_deref().unwrap_or_default();
        if got != offer.revision {
            return Err(SubmitRejection::StaleRevision {
                expected: offer.revision.clone(),
                got: got.to_string(),
            });
        }
        offer.check_bounds(brief)?;
        Ok(offer)
    }

    /// Every offer, in catalog order.
    fn offers(&self) -> Vec<A2aOffer> {
        self.services.values().map(|p| p.offer().clone()).collect()
    }
}

/// Everything the five configured handlers share. One `Arc`, cloned into
/// each of them.
struct ConfiguredA2a {
    registry: TaskRegistry,
    executor: Arc<dyn TaskExecutor>,
    catalog: Catalog,
    store: SharedAdmissionStore,
    gate: Option<Arc<dyn TaskAdmissionGate>>,
    preflight: Option<Arc<dyn TaskPreflight>>,
    principal: A2aPrincipal,
    /// This node's id — the provider half of a quote's capability.
    node_id: u64,
    /// Held so the journal's exclusive owner outlives every handler:
    /// while a handler can still write, the lock must not be released.
    _owner: Option<Arc<JournalOwner>>,
}

/// `tool_id` for a service's quotes: `net.a2a.task/{service_id}`. The
/// engine's tool-binding check reads exactly this from the quote's
/// capability tail.
fn tool_id_of(service_id: &str) -> String {
    format!("{A2A_TASK_SERVICE}/{service_id}")
}

/// The request-side facts of one submission, gathered once at S4 and
/// read by every step after it: who it is attributed to, what it names,
/// and what payment evidence (if any) it carried.
struct Submission<'a> {
    owner: TaskOwner,
    task_id: &'a str,
    /// `net.a2a.task/{service_id}` — what a quote must be bound to.
    tool_id: &'a str,
    quote: Option<&'a str>,
    binding: Option<&'a [u8]>,
}

impl Submission<'_> {
    /// Whether this submission presented **any** payment evidence.
    ///
    /// The distinction S5 turns on: a preflight failure with nothing
    /// presented is an ordinary unpaid rejection, while the same failure
    /// with a quote or a binding on the request may be refusing work
    /// somebody already paid for, and is reconciliation instead.
    fn carries_payment(&self) -> bool {
        self.quote.is_some() || self.binding.is_some()
    }
}

impl ConfiguredA2a {
    /// D5: who this request is attributed to.
    ///
    /// Never `caller_origin` (routing metadata the sender chooses) and
    /// never anything in the request body. Under
    /// [`A2aPrincipal::OrgAdmitted`] the org-admission gate dispatches
    /// only after `verify_org_admission` returned `Admitted`, so a
    /// missing admission is an invariant violation — refused loudly
    /// rather than attributed to the delivering peer, which would
    /// silently downgrade the principal.
    fn owner_of(&self, ctx: &RpcContext) -> Result<TaskOwner, RpcHandlerError> {
        match self.principal {
            A2aPrincipal::SessionPeer => Ok(TaskOwner::Peer(ctx.session_peer)),
            A2aPrincipal::OrgAdmitted(_) => match ctx.org_admission.as_ref() {
                Some(admitted) => Ok(TaskOwner::Entity(admitted.caller.0)),
                None => Err(RpcHandlerError::Application {
                    code: NRPC_TYPED_HANDLER_ERROR,
                    message: "configured A2A handler reached without verified org admission"
                        .to_string(),
                }),
            },
        }
    }

    /// The capability a quote for `service_id` is issued against.
    fn capability(&self, service_id: &str) -> String {
        format!("{}/{}", self.node_id, tool_id_of(service_id))
    }

    /// The application preflight, or `Ok(())` when none is configured.
    async fn run_preflight(
        &self,
        owner: TaskOwner,
        offer: &A2aOffer,
        brief: &TaskBrief,
    ) -> Result<(), String> {
        match self.preflight.as_ref() {
            Some(p) => p.preflight(owner, offer, brief).await,
            None => Ok(()),
        }
    }

    /// Append an audit note. A failed note never changes the refusal the
    /// caller gets: the note is evidence, not part of the verdict.
    async fn note(&self, owner: TaskOwner, task_id: &str, reason: &str, quote_id: Option<&str>) {
        let _ = self
            .store
            .note(
                owner,
                task_id,
                AttemptNote {
                    at: now_secs(),
                    reason: reason.to_string(),
                    claimed_quote_id: quote_id.map(str::to_string),
                },
            )
            .await;
    }

    /// The reservation reply for `record`.
    fn reservation_of(
        &self,
        record: &AdmissionRecord,
        offer: &A2aOffer,
        expires_at: u64,
    ) -> AdmissionReservation {
        // A free inline admission (D2 S5a) carries no admission id: it
        // never went through prepare, and a free offer publishes no
        // pricing, so there is no purchase for one to bind.
        let admission_id = record.admission_id.clone().unwrap_or_default();
        AdmissionReservation {
            task_id: record.task_id.clone(),
            purchase_hash: purchase_hash(&admission_id, &record.commitment),
            admission_id,
            commitment: record.commitment.clone(),
            capability: self.capability(&record.service_id),
            pricing_terms: offer.pricing_terms.clone(),
            expires_at,
        }
    }

    // -- P1..P5: prepare -------------------------------------------------

    /// `net.a2a.prepare`. Uncharged, and side-effect-free except for a
    /// bounded capacity reservation.
    async fn prepare(&self, owner: TaskOwner, body: &[u8]) -> PrepareReply {
        // P2 — decode, then service / revision / bounds.
        let brief = match TaskBrief::decode(&typed_body(body)) {
            Ok(brief) => brief,
            Err(e) => return rejected(e),
        };
        let offer = match self.catalog.resolve(&brief) {
            Ok(offer) => offer.clone(),
            Err(rejection) => return rejected(rejection),
        };

        // P3 — the application's own admission. A refusal here reserves
        // nothing, which is the point of running it before a quote can
        // exist.
        if let Err(reason) = self.run_preflight(owner, &offer, &brief).await {
            return PrepareReply::Rejected { reason };
        }

        // P4 — resolve against the store. Nothing else mutates.
        let commitment = task_commitment(&offer, &brief);
        let now = now_secs();
        let task_id = brief.task_id.clone();
        let existing = match self.store.lookup(owner, &task_id).await {
            Ok(existing) => existing,
            Err(e) => return rejected(e),
        };
        if let Some(record) = existing {
            if record.commitment != commitment {
                return rejected(SubmitRejection::IdReusedForDifferentBrief { task_id });
            }
            return self.prepare_reply_for(&record, &offer, now).await;
        }
        // A launch that happened and whose result has since been retired:
        // the ledger outlives the record, so this id can never be bought
        // again.
        match self.store.ledger_has(owner, &task_id).await {
            Ok(true) => return PrepareReply::Retired { task_id },
            Ok(false) => {}
            Err(e) => return rejected(e),
        }
        match self.store.in_flight(&offer.service_id, now).await {
            Ok(in_flight) if in_flight >= offer.bounds.max_in_flight => return PrepareReply::Busy,
            Ok(_) => {}
            Err(e) => return rejected(e),
        }
        // The admission id is minted here, by the provider, per
        // reservation — never derived from anything the caller sent.
        let record = AdmissionRecord::reserved(
            owner,
            brief,
            &offer,
            Some(random_id()),
            commitment.clone(),
            now,
        );
        match self.store.insert_reserved(record.clone()).await {
            Ok(InsertOutcome::Inserted) => PrepareReply::Reservation(self.reservation_of(
                &record,
                &offer,
                now.saturating_add(offer.reservation_ttl_secs),
            )),
            // Lost the CAS to a concurrent prepare of the same task: the
            // stored record is authoritative, so answer from it rather
            // than from the reservation this call would have made.
            Ok(InsertOutcome::Existing(found)) if found.commitment == commitment => {
                self.prepare_reply_for(&found, &offer, now).await
            }
            Ok(InsertOutcome::Existing(_)) => {
                rejected(SubmitRejection::IdReusedForDifferentBrief {
                    task_id: record.task_id.clone(),
                })
            }
            Err(e) => rejected(e),
        }
    }

    /// P4's dispatch on an existing record with this brief's commitment.
    async fn prepare_reply_for(
        &self,
        record: &AdmissionRecord,
        offer: &A2aOffer,
        now: u64,
    ) -> PrepareReply {
        let task_id = record.task_id.clone();
        match &record.state {
            // Live reservation: idempotent, same admission id.
            AdmissionState::Reserved { expires_at } if *expires_at > now => {
                PrepareReply::Reservation(self.reservation_of(record, offer, *expires_at))
            }
            // Lapsed: the record kept its admission id, but the capacity
            // it held was released. Re-acquire it or report Busy — never
            // hand back a reservation that does not hold capacity.
            AdmissionState::Reserved { .. } => {
                match self.store.in_flight(&offer.service_id, now).await {
                    Ok(in_flight) if in_flight >= offer.bounds.max_in_flight => {
                        return PrepareReply::Busy
                    }
                    Ok(_) => {}
                    Err(e) => return rejected(e),
                }
                let expires_at = now.saturating_add(offer.reservation_ttl_secs);
                if let Err(e) = self
                    .store
                    .transition(
                        record.owner,
                        &task_id,
                        &[StateTag::Reserved],
                        AdmissionState::Reserved { expires_at },
                        now,
                    )
                    .await
                {
                    return rejected(e);
                }
                PrepareReply::Reservation(self.reservation_of(record, offer, expires_at))
            }
            // Already paid for: the caller may go straight to submit. No
            // new quote is needed, and a paid admission holds capacity
            // until it resolves, so the expiry is advisory here.
            AdmissionState::Paid { .. } => PrepareReply::Reservation(self.reservation_of(
                record,
                offer,
                record.updated_at.saturating_add(offer.reservation_ttl_secs),
            )),
            AdmissionState::Launched { .. } | AdmissionState::Terminal { .. } => {
                PrepareReply::Existing { task_id }
            }
            // Prepare never reopens a revoked admission.
            AdmissionState::Reconcile { .. } => PrepareReply::Reconciliation { task_id },
        }
    }

    // -- S1..S8: submit --------------------------------------------------

    /// `net.a2a.task`, configured. The D2 sequence, in order, with the
    /// per-state dispatch at S4 as the only branch point.
    ///
    /// Every early return still holding the [`AdmissionTicket`] releases
    /// the reservation on drop, so no refusal path can strand a
    /// `Requested` entry that would then refuse every later submission
    /// of the same id.
    ///
    /// [`AdmissionTicket`]: crate::a2a::AdmissionTicket
    async fn submit(
        &self,
        owner: TaskOwner,
        body: &[u8],
        headers: &[(String, Vec<u8>)],
    ) -> RpcResponsePayload {
        // S2 — decode; service / revision / bounds (cheap re-check).
        let brief = match TaskBrief::decode(&typed_body(body)) {
            Ok(brief) => brief,
            Err(e) => return ack_refused(e),
        };
        let offer = match self.catalog.resolve(&brief) {
            Ok(offer) => offer.clone(),
            Err(rejection) => return ack_refused(rejection),
        };
        let paid = offer.pricing_terms.is_some();
        let tool_id = tool_id_of(&offer.service_id);
        let task_id = brief.task_id.clone();
        let commitment = task_commitment(&offer, &brief);

        // S3 — claim the registry slot without starting work.
        let ticket = match self.registry.reserve(owner, brief.clone()) {
            Err(rejection) => return ack_refused(rejection),
            // Already launched under this id: answered with no gate call
            // and no second spawn.
            Ok(Admission::Existing(id)) => return ack_accepted(id),
            // A concurrent identical submission is mid-decision. Await
            // ITS verdict rather than deciding (and paying for) the same
            // task twice — the configured path always awaits, because
            // here a reservation can also end in a refusal.
            Ok(Admission::Pending(rx)) => {
                return match await_admission_verdict(rx).await {
                    Ok(id) => ack_accepted(id),
                    Err(reason) => ack_refused(reason),
                }
            }
            Ok(Admission::Reserved(ticket)) => ticket,
        };

        let now = now_secs();
        let sub = Submission {
            owner,
            task_id: &task_id,
            tool_id: &tool_id,
            quote: request_header(headers, HDR_PAYMENT_QUOTE)
                .and_then(|raw| std::str::from_utf8(raw).ok()),
            binding: request_header(headers, HDR_PAYMENT_BINDING),
        };
        let quote = sub.quote;
        let binding = sub.binding;

        // S4 — per-state dispatch. The ONLY branch point.
        let found = match self.store.lookup(owner, &task_id).await {
            Ok(found) => found,
            Err(e) => {
                return payment_refusal(&schematic_journal_unavailable(&tool_id, &e.to_string()))
            }
        };
        let mut record = match found {
            Some(record) if record.commitment != commitment => {
                return ack_refused(SubmitRejection::IdReusedForDifferentBrief { task_id })
            }
            Some(record) => match &record.state {
                // Store-side "already under way": status shows the
                // recorded (or interrupted) state; never relaunched.
                AdmissionState::Launched { .. } | AdmissionState::Terminal { .. } => {
                    return ack_accepted(task_id)
                }
                // Post-payment revocation: the same refusal, every time,
                // with no state change. An operator's `resolve` is the
                // only exit.
                AdmissionState::Reconcile {
                    claimed_quote_id, ..
                } => {
                    let schematic =
                        schematic_admission_revoked(&tool_id, claimed_quote_id.as_deref());
                    return payment_refusal(&schematic);
                }
                _ => Some(record),
            },
            None => {
                match self.store.ledger_has(owner, &task_id).await {
                    // Ran once, result retired: the ledger bars a
                    // relaunch, and the redeem step is never reached.
                    Ok(true) => return ack_refused(SubmitRejection::Retired { task_id }),
                    Ok(false) => {}
                    Err(e) => {
                        return payment_refusal(&schematic_journal_unavailable(
                            &tool_id,
                            &e.to_string(),
                        ))
                    }
                }
                if paid {
                    // A paid service requires a prior reservation — and
                    // this is a payment/admission verdict, so it is
                    // refused structurally, before the gate is touched.
                    return payment_refusal(&schematic_no_reservation(&tool_id));
                }
                None
            }
        };

        // S4, continued: a lapsed reservation re-acquires capacity before
        // anything else. Over capacity is a retryable in-body Busy with
        // the gate NOT called — never an unpaid rejection of a purchase
        // that is still good.
        if let Some(current) = record.as_ref() {
            if matches!(&current.state, AdmissionState::Reserved { expires_at } if *expires_at <= now)
            {
                match self.store.in_flight(&offer.service_id, now).await {
                    Ok(in_flight) if in_flight >= offer.bounds.max_in_flight => {
                        return ack_refused(SubmitRejection::Busy)
                    }
                    Ok(_) => {}
                    Err(e) => {
                        return payment_refusal(&schematic_journal_unavailable(
                            &tool_id,
                            &e.to_string(),
                        ))
                    }
                }
                let expires_at = now.saturating_add(offer.reservation_ttl_secs);
                if let Err(e) = self
                    .store
                    .transition(
                        owner,
                        &task_id,
                        &[StateTag::Reserved],
                        AdmissionState::Reserved { expires_at },
                        now,
                    )
                    .await
                {
                    return payment_refusal(&schematic_journal_unavailable(
                        &tool_id,
                        &e.to_string(),
                    ));
                }
                if let Some(r) = record.as_mut() {
                    r.state = AdmissionState::Reserved { expires_at };
                }
            }
        }

        // S5 — the application preflight runs again, on every path:
        // authority may have changed since prepare.
        if let Err(reason) = self.run_preflight(owner, &offer, &brief).await {
            return self.revoked(&sub, record.as_ref(), reason).await;
        }

        // S5a — free inline admission: the same capacity rule prepare
        // applies, for a caller that never prepared.
        if record.is_none() {
            match self.store.in_flight(&offer.service_id, now).await {
                Ok(in_flight) if in_flight >= offer.bounds.max_in_flight => {
                    return ack_refused(SubmitRejection::Busy)
                }
                Ok(_) => {}
                Err(e) => {
                    return payment_refusal(&schematic_journal_unavailable(
                        &tool_id,
                        &e.to_string(),
                    ))
                }
            }
            let inline = AdmissionRecord::reserved(
                owner,
                brief.clone(),
                &offer,
                // No admission id: nothing was minted, because nothing
                // can be purchased against a free offer.
                None,
                commitment.clone(),
                now,
            );
            match self.store.insert_reserved(inline.clone()).await {
                Ok(InsertOutcome::Inserted) => record = Some(inline),
                // A prepare of the same task landed between the lookup
                // and this insert. Its reservation is authoritative;
                // this submit claims the launch against it.
                Ok(InsertOutcome::Existing(found))
                    if found.commitment == commitment
                        && matches!(found.state, AdmissionState::Reserved { .. }) =>
                {
                    record = Some(*found)
                }
                Ok(InsertOutcome::Existing(found)) => {
                    return ack_refused(format!(
                        "this task is already admitted as {}",
                        found.state.tag().as_str()
                    ))
                }
                Err(e) => {
                    return payment_refusal(&schematic_journal_unavailable(
                        &tool_id,
                        &e.to_string(),
                    ))
                }
            }
        }

        // S6 / S6′ — payment. Free services skip both entirely.
        if paid {
            let Some(current) = record.as_ref() else {
                // Unreachable: a paid service with no record answered
                // `no_reservation` at S4. Fail closed rather than launch.
                return payment_refusal(&schematic_no_reservation(&tool_id));
            };
            match &current.state {
                AdmissionState::Reserved { .. } => {
                    if let Some(refusal) = self.redeem(&sub, current, now).await {
                        return refusal;
                    }
                }
                AdmissionState::Paid { quote_id, .. } => {
                    // S6′ — the recorded evidence is authoritative and
                    // the gate is NOT called: this admission was already
                    // paid for, and redeeming again is how a retry turns
                    // into a second charge.
                    let matches_record = quote == Some(quote_id.as_str()) && binding.is_some();
                    if !matches_record {
                        self.note(owner, &task_id, "binding_rejected", quote).await;
                        return payment_refusal(&schematic_binding_rejected(
                            &tool_id,
                            Some(quote_id),
                            "this admission is already paid for by another quote, or the \
                             submission carried no binding signature"
                                .to_string(),
                        ));
                    }
                }
                other => {
                    return payment_refusal(&schematic_journal_unavailable(
                        &tool_id,
                        &format!("admission is {}", other.tag().as_str()),
                    ))
                }
            }
        }

        // S7 — the launch claim is durable BEFORE the spawn. A write
        // failure leaves the store exactly as it was and runs nothing;
        // the retry re-enters S4 with the same state.
        if let Err(e) = self.store.claim_launch(owner, &task_id, now).await {
            return payment_refusal(&schematic_journal_unavailable(&tool_id, &e.to_string()));
        }

        // S8 — launch, retained for exactly as long as the offer
        // published.
        let id = ticket
            .with_retention(offer.retention_secs)
            .launch(Arc::clone(&self.executor));
        ack_accepted(id)
    }

    /// S5's refusal dispatch: what a preflight failure at submit means
    /// depends entirely on whether money may already have moved.
    async fn revoked(
        &self,
        sub: &Submission<'_>,
        record: Option<&AdmissionRecord>,
        reason: String,
    ) -> RpcResponsePayload {
        let (owner, task_id, tool_id) = (sub.owner, sub.task_id, sub.tool_id);
        let Some(record) = record else {
            // Free, never prepared: nothing was reserved.
            return ack_refused(reason);
        };
        if !record.paid {
            // Free, prepared: delete the reservation — nothing financial
            // exists, so leaving it would hold capacity for work that
            // will not run.
            if let Err(e) = self.store.delete_reservation(owner, task_id).await {
                return payment_refusal(&schematic_journal_unavailable(tool_id, &e.to_string()));
            }
            return ack_refused(reason);
        }
        let (from, claimed, payer) = match &record.state {
            // Paid service, reserved, no payment presented: nothing has
            // been claimed paid, so this is an ordinary unpaid rejection
            // and the reservation stays exactly as it was.
            AdmissionState::Reserved { .. } if !sub.carries_payment() => {
                return ack_refused(reason)
            }
            AdmissionState::Reserved { .. } => {
                ([StateTag::Reserved], sub.quote.map(str::to_string), None)
            }
            AdmissionState::Paid { quote_id, payer } => {
                ([StateTag::Paid], Some(quote_id.clone()), Some(*payer))
            }
            other => {
                return payment_refusal(&schematic_journal_unavailable(
                    tool_id,
                    &format!("admission is {}", other.tag().as_str()),
                ))
            }
        };
        // A payment may already have landed. This is reconciliation, not
        // a rejection: the record is retained until an operator resolves
        // it, and the gate is never called on this path.
        if let Err(e) = self
            .store
            .transition(
                owner,
                task_id,
                &from,
                AdmissionState::Reconcile {
                    reason,
                    claimed_quote_id: claimed.clone(),
                    payer,
                },
                now_secs(),
            )
            .await
        {
            return payment_refusal(&schematic_journal_unavailable(tool_id, &e.to_string()));
        }
        payment_refusal(&schematic_admission_revoked(tool_id, claimed.as_deref()))
    }

    /// S6 — redeem a payment for exactly this reservation. `Some` is the
    /// refusal to answer; `None` means the record is now `Paid`.
    async fn redeem(
        &self,
        sub: &Submission<'_>,
        record: &AdmissionRecord,
        now: u64,
    ) -> Option<RpcResponsePayload> {
        let (owner, task_id, tool_id) = (sub.owner, sub.task_id, sub.tool_id);
        let Some(quote_id) = sub.quote else {
            self.note(owner, task_id, "missing_quote", None).await;
            return Some(payment_refusal(&schematic_missing_quote(tool_id)));
        };
        // Mandatory, unlike a paid tool's optional bearer fallback: a
        // task is a long-running side effect, so possession of a quote id
        // is not evidence that the payer authorized this submission.
        let Some(binding) = sub.binding else {
            self.note(owner, task_id, "binding_required", Some(quote_id))
                .await;
            return Some(payment_refusal(&schematic_binding_required(
                tool_id, quote_id,
            )));
        };
        let Some(gate) = self.gate.as_ref() else {
            // Serve-time invariants make this unreachable; fail closed
            // rather than serve paid work for nothing.
            return Some(payment_refusal(&FailureSchematic::gate_missing(tool_id)));
        };
        // The expected hash is computed from the PROVIDER's own record,
        // never read off the request: that is what makes a valid payment
        // for one reservation worthless against another.
        let expected = purchase_hash(
            record.admission_id.as_deref().unwrap_or_default(),
            &record.commitment,
        );
        let evidence = match gate
            .redeem(TaskPaymentClaim {
                tool_id,
                quote_id,
                binding,
                expected_input_hash: &expected,
            })
            .await
        {
            Ok(evidence) => evidence,
            Err(denial) => {
                // A denial is an attempt note, NOT a state change: the
                // reservation survives with its admission id, so a
                // caller that fixes its payment retries the same
                // admission instead of buying a second one.
                self.note(owner, task_id, &denial.schematic.reason, Some(quote_id))
                    .await;
                return Some(gate_refusal(denial.message, &denial.schematic));
            }
        };
        // A verified end-to-end principal is matched against the payer:
        // a payment made by one entity cannot admit another's task.
        if matches!(self.principal, A2aPrincipal::OrgAdmitted(_))
            && owner != TaskOwner::Entity(evidence.payer)
        {
            self.note(owner, task_id, "binding_rejected", Some(&evidence.quote_id))
                .await;
            return Some(payment_refusal(&schematic_binding_rejected(
                tool_id,
                Some(&evidence.quote_id),
                "the payer this quote was issued to is not the admitted caller".to_string(),
            )));
        }
        if let Err(e) = self
            .store
            .transition(
                owner,
                task_id,
                &[StateTag::Reserved],
                AdmissionState::Paid {
                    quote_id: evidence.quote_id,
                    payer: evidence.payer,
                },
                now,
            )
            .await
        {
            // The redeem was idempotent per purchase hash, so the retry
            // re-enters S4 as `Reserved`, redeems again without a second
            // charge, and proceeds.
            return Some(payment_refusal(&schematic_journal_unavailable(
                tool_id,
                &e.to_string(),
            )));
        }
        None
    }
}

/// A prepare refusal from anything that can render itself.
fn rejected(reason: impl std::fmt::Display) -> PrepareReply {
    PrepareReply::Rejected {
        reason: reason.to_string(),
    }
}

// ---------------------------------------------------------------------------
// The five configured handlers
// ---------------------------------------------------------------------------

/// `net.a2a.prepare` — uncharged: validate, reserve capacity, mint the
/// admission id a purchase binds to.
struct PrepareHandler {
    cfg: Arc<ConfiguredA2a>,
}

#[async_trait::async_trait]
impl RpcHandler for PrepareHandler {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        let owner = self.cfg.owner_of(&ctx)?;
        let reply = self.cfg.prepare(owner, &ctx.payload.body).await;
        Ok(json_ok(reply.encode()))
    }
}

/// `net.a2a.task`, configured — the S1..S8 sequence.
struct ConfiguredSubmitHandler {
    cfg: Arc<ConfiguredA2a>,
}

#[async_trait::async_trait]
impl RpcHandler for ConfiguredSubmitHandler {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        let owner = self.cfg.owner_of(&ctx)?;
        Ok(self
            .cfg
            .submit(owner, &ctx.payload.body, &ctx.payload.headers)
            .await)
    }
}

/// `net.a2a.status`, configured — the live registry first, then the
/// durable record, then `null`.
struct ConfiguredStatusHandler {
    cfg: Arc<ConfiguredA2a>,
}

#[async_trait::async_trait]
impl RpcHandler for ConfiguredStatusHandler {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        let owner = self.cfg.owner_of(&ctx)?;
        let task_id: String =
            serde_json::from_slice(&typed_body(&ctx.payload.body)).unwrap_or_default();
        // Another submitter's task reads as unknown rather than
        // forbidden, on both paths — `TaskRecord` carries the full
        // prompt and context refs, so "not yours" and "no such task"
        // must be indistinguishable.
        let mut record = self.cfg.registry.record(owner, &task_id);
        if record.is_none() {
            // The durable fallback: a task whose registry entry was
            // evicted, or whose process is gone, still answers what the
            // admission record knows — including the interrupted states a
            // successor owner inherits.
            if let Ok(Some(found)) = self.cfg.store.lookup(owner, &task_id).await {
                record = found.state.status().map(|state| TaskRecord {
                    brief: found.brief,
                    state,
                    updated_at: found.updated_at,
                });
            }
        }
        Ok(json_ok(serde_json::to_vec(&record).unwrap_or_default()))
    }
}

/// `net.a2a.cancel`, configured. A durable-only record is terminal
/// (`Interrupted`) or unstarted, so there is nothing to stop: `false`.
struct ConfiguredCancelHandler {
    cfg: Arc<ConfiguredA2a>,
}

#[async_trait::async_trait]
impl RpcHandler for ConfiguredCancelHandler {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        let owner = self.cfg.owner_of(&ctx)?;
        let task_id: String =
            serde_json::from_slice(&typed_body(&ctx.payload.body)).unwrap_or_default();
        let cancelled = self.cfg.registry.cancel(owner, &task_id);
        Ok(json_ok(serde_json::to_vec(&cancelled).unwrap_or_default()))
    }
}

/// `net.a2a.describe` — uncharged discovery: every offer, with its
/// bounds, its retention terms and (for a paid service) its pricing.
struct DescribeHandler {
    cfg: Arc<ConfiguredA2a>,
}

#[async_trait::async_trait]
impl RpcHandler for DescribeHandler {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        // Authenticated like the rest (and org-admitted under
        // `OrgAdmitted`), even though the answer is the same for every
        // caller: a catalog is not public data on a PROTECTED service.
        let _owner = self.cfg.owner_of(&ctx)?;
        Ok(json_ok(
            serde_json::to_vec(&self.cfg.catalog.offers()).unwrap_or_default(),
        ))
    }
}

/// The executor as the configured path spawns it: the host's runner plus
/// a clone of the journal's ownership handle.
///
/// [`AdmissionTicket::launch`](crate::a2a::AdmissionTicket::launch) moves
/// this `Arc` into the spawned task, so the owner cannot be released
/// while a launched task is still live — which is what lets the terminal
/// hook write its row even after every [`ServeHandle`] has been dropped.
struct OwnedExecutor {
    inner: Arc<dyn TaskExecutor>,
    _owner: Option<Arc<JournalOwner>>,
}

#[async_trait::async_trait]
impl TaskExecutor for OwnedExecutor {
    async fn run(&self, brief: TaskBrief, cancel: CancelToken) -> Result<String, String> {
        self.inner.run(brief, cancel).await
    }
}

impl Mesh {
    /// **Executor side.** Serve the three A2A services backed by `registry` +
    /// `executor`: accept briefs (spawning the executor), answer status, and
    /// cancel. Returns the [`ServeHandle`]s — hold them for as long as this
    /// agent should accept tasks; dropping them unregisters the services. This
    /// node must be `start()`ed.
    ///
    /// Rollback is automatic: if serving a later service fails, the already-
    /// registered handles drop (unregistering) as the error returns.
    pub fn serve_a2a(
        &self,
        registry: TaskRegistry,
        executor: Arc<dyn TaskExecutor>,
    ) -> Result<Vec<ServeHandle>, ServeError> {
        // The context-bearing `serve_rpc` path, not `serve_rpc_typed`:
        // the typed helper hands the closure only the request bytes, so
        // the authenticated submitter was structurally unavailable to
        // these handlers. That is why status and cancel keyed on the
        // task id alone.
        let submit = self.serve_rpc(
            A2A_TASK_SERVICE,
            Arc::new(SubmitHandler {
                registry: registry.clone(),
                executor,
            }),
        )?;
        let status = self.serve_rpc(
            A2A_STATUS_SERVICE,
            Arc::new(StatusHandler {
                registry: registry.clone(),
            }),
        )?;
        let cancel = self.serve_rpc(A2A_CANCEL_SERVICE, Arc::new(CancelHandler { registry }))?;

        Ok(vec![submit, status, cancel])
    }

    /// **Executor side, catalog-driven.** Serve the five configured A2A
    /// services — describe, prepare, submit, status, cancel — against
    /// `config`'s catalog, admitting paid work through its gate and its
    /// durable admission store.
    ///
    /// The strict sibling of [`serve_a2a`](Self::serve_a2a), which stays
    /// exactly as it was: free, catalog-free, journal-free. A node serves
    /// one A2A handler set, so "both free and paid" is two catalog
    /// entries here, never two serving paths.
    ///
    /// # Serve-time invariants (fail closed)
    ///
    /// A configured paid service must never degrade to free, so every
    /// way that could happen is refused before a single service is
    /// registered:
    ///
    /// | Configuration | Error |
    /// |---|---|
    /// | [`Paid`](A2aServicePolicy::Paid) with no `pricing_terms` | [`ServeError::MissingPricingTerms`] |
    /// | [`Free`](A2aServicePolicy::Free) with `pricing_terms` | [`ServeError::UnenforceablePricing`] |
    /// | any `Paid` with no gate, or no journal | [`ServeError::A2aPaidMisconfigured`] |
    /// | a catalog key that disagrees with its offer's `service_id` | [`ServeError::A2aPaidMisconfigured`] |
    ///
    /// An all-`Free` catalog with no journal needs neither a gate nor a
    /// journal and runs on the in-memory
    /// [`A2aAdmissions`] — the same
    /// state table, for the process lifetime.
    ///
    /// # Ownership
    ///
    /// Opening the journal took lifetime-exclusive ownership of it. That
    /// handle is cloned into the returned [`A2aServing`], into every
    /// handler, and into every launched task (through the executor
    /// wrapper), so the lock cannot be released while anything that can
    /// still write is alive — including a task whose
    /// [`ServeHandle`]s have already been dropped.
    ///
    /// The clones are explicit rather than incidental. A journal store
    /// happens to hold its own owner, so anything holding the store
    /// holds the lock transitively today — but that is a property of
    /// one store implementation, and the guarantee here is about the
    /// serving path. Stating it directly is what keeps it true if the
    /// configured store ever stops being the journal itself.
    ///
    /// Rollback is automatic: if a later registration fails, the
    /// already-registered handles drop (unregistering) as the error
    /// returns.
    ///
    /// This node must be `start()`ed.
    pub fn serve_a2a_configured(
        &self,
        registry: TaskRegistry,
        executor: Arc<dyn TaskExecutor>,
        config: A2aServiceConfig,
    ) -> Result<A2aServing, ServeError> {
        let paid_any = config.has_paid();
        for (id, policy) in &config.services {
            let offer = policy.offer();
            if offer.service_id != *id {
                return Err(ServeError::A2aPaidMisconfigured(format!(
                    "catalog key {id:?} holds an offer for service {:?}; a commitment is \
                     computed against the offer, so a caller would be admitted under terms \
                     it never saw",
                    offer.service_id
                )));
            }
            match policy {
                A2aServicePolicy::Paid(_) if offer.pricing_terms.is_none() => {
                    return Err(ServeError::MissingPricingTerms(id.clone()))
                }
                A2aServicePolicy::Free(_) if offer.pricing_terms.is_some() => {
                    return Err(ServeError::UnenforceablePricing(id.clone()))
                }
                _ => {}
            }
        }
        if paid_any && config.payment.is_none() {
            return Err(ServeError::A2aPaidMisconfigured(
                "the catalog prices at least one service but no TaskAdmissionGate is \
                 configured; a paid service is never served free"
                    .to_string(),
            ));
        }
        if paid_any && config.journal.is_none() {
            return Err(ServeError::A2aPaidMisconfigured(
                "the catalog prices at least one service but no admission journal is \
                 configured; a paid launch must be claimed durably before it runs"
                    .to_string(),
            ));
        }

        let A2aServiceConfig {
            services,
            payment,
            preflight,
            principal,
            journal,
        } = config;

        let (store, owner) = match journal {
            Some(journal) => {
                let owner = journal.owner();
                (journal.shared(), Some(owner))
            }
            None => (A2aAdmissions::new().shared(), None),
        };

        // The registry learns terminal outcomes it did not start through
        // this hook — the one place the durable record picks up an
        // executor's verdict. The hook must not block, so it bridges to
        // the store through a spawn, and that spawned task holds the
        // ownership handle across its write.
        let hook_store = Arc::clone(&store);
        let hook_owner = owner.clone();
        let hook: TerminalHook = Arc::new(move |task_owner, task_id, state| {
            let Ok(handle) = tokio::runtime::Handle::try_current() else {
                // Nothing records a terminal state outside the launched
                // task's runtime; a record left `Launched` is the
                // designed degradation (status answers
                // `Interrupted { outcome_unknown }`), never a relaunch.
                return;
            };
            let store = Arc::clone(&hook_store);
            let keep_owner = hook_owner.clone();
            let task_id = task_id.to_string();
            let state = state.clone();
            handle.spawn(async move {
                let _owner = keep_owner;
                let _ = store
                    .record_terminal(task_owner, &task_id, state, now_secs())
                    .await;
            });
        });

        let cfg = Arc::new(ConfiguredA2a {
            registry: registry.with_terminal_hook(hook),
            executor: Arc::new(OwnedExecutor {
                inner: executor,
                _owner: owner.clone(),
            }),
            catalog: Catalog { services },
            store: Arc::clone(&store),
            gate: payment,
            preflight,
            principal,
            node_id: self.node().node_id(),
            _owner: owner.clone(),
        });

        let handles = vec![
            self.serve_configured(
                A2A_TASK_SERVICE,
                Arc::new(ConfiguredSubmitHandler {
                    cfg: Arc::clone(&cfg),
                }),
                principal,
            )?,
            self.serve_configured(
                A2A_STATUS_SERVICE,
                Arc::new(ConfiguredStatusHandler {
                    cfg: Arc::clone(&cfg),
                }),
                principal,
            )?,
            self.serve_configured(
                A2A_CANCEL_SERVICE,
                Arc::new(ConfiguredCancelHandler {
                    cfg: Arc::clone(&cfg),
                }),
                principal,
            )?,
            self.serve_configured(
                A2A_PREPARE_SERVICE,
                Arc::new(PrepareHandler {
                    cfg: Arc::clone(&cfg),
                }),
                principal,
            )?,
            self.serve_configured(
                A2A_DESCRIBE_SERVICE,
                Arc::new(DescribeHandler { cfg }),
                principal,
            )?,
        ];

        Ok(A2aServing {
            handles,
            store,
            _owner: owner,
        })
    }

    /// Register one configured service under the chosen principal.
    ///
    /// [`A2aPrincipal::SessionPeer`] uses the public context-bearing
    /// `serve_rpc` path; [`A2aPrincipal::OrgAdmitted`] registers
    /// PROTECTED through the same two core seams the org facade uses, so
    /// `ctx.org_admission` is present and verified before any handler
    /// runs. The trivial proof policy is installed deliberately: these
    /// handlers decide with the verified facts in hand, exactly like
    /// `serve_org`.
    fn serve_configured<H: RpcHandler>(
        &self,
        service: &str,
        handler: Arc<H>,
        principal: A2aPrincipal,
    ) -> Result<ServeHandle, ServeError> {
        match principal {
            A2aPrincipal::SessionPeer => self.serve_rpc(service, handler),
            A2aPrincipal::OrgAdmitted(access) => {
                let policy: net::adapter::net::org_admission_gate::OrgProviderPolicy =
                    Arc::new(|_| true);
                match access {
                    OrgAccess::SameOrg => {
                        self.node().serve_rpc_owner_scoped(service, handler, policy)
                    }
                    OrgAccess::Granted => self.node().serve_rpc_granted(service, handler, policy),
                }
            }
        }
    }

    /// **Requester side.** Hand `brief` to the executor at `target_node_id` and
    /// return its [`TaskAck`]. Non-blocking on the executor: the task runs
    /// async on the far side while this agent keeps working. The caller must
    /// already be connected to `target_node_id` (an in-root peer — dial it with
    /// `connect_via` first if needed).
    pub async fn submit_task(
        &self,
        target_node_id: u64,
        brief: &TaskBrief,
    ) -> Result<TaskAck, A2aFlowError> {
        let response: Vec<u8> = self
            .call_typed(
                target_node_id,
                A2A_TASK_SERVICE,
                &brief.encode(),
                CallOptionsTyped::default(),
            )
            .await
            .map_err(|e| A2aFlowError::Transport(format!("call: {e}")))?;
        TaskAck::decode(&response).map_err(|e| A2aFlowError::Decode(e.to_string()))
    }

    /// **Requester side.** The executor's current [`TaskRecord`] for `task_id`
    /// (state + brief + last-update time), or `None` if the executor doesn't
    /// know it.
    pub async fn task_status(
        &self,
        target_node_id: u64,
        task_id: &str,
    ) -> Result<Option<TaskRecord>, A2aFlowError> {
        let response: Vec<u8> = self
            .call_typed(
                target_node_id,
                A2A_STATUS_SERVICE,
                &task_ref_bytes(task_id),
                CallOptionsTyped::default(),
            )
            .await
            .map_err(|e| A2aFlowError::Transport(format!("call: {e}")))?;
        serde_json::from_slice(&response).map_err(|e| A2aFlowError::Decode(e.to_string()))
    }

    /// **Requester side.** Cancel `task_id` on the executor. Returns whether the
    /// executor had it in flight (a terminal / unknown task returns `false`).
    /// The executor's cooperative cancellation stops the work; poll
    /// [`task_status`](Self::task_status) to observe the `Cancelled` state.
    pub async fn cancel_task(
        &self,
        target_node_id: u64,
        task_id: &str,
    ) -> Result<bool, A2aFlowError> {
        let response: Vec<u8> = self
            .call_typed(
                target_node_id,
                A2A_CANCEL_SERVICE,
                &task_ref_bytes(task_id),
                CallOptionsTyped::default(),
            )
            .await
            .map_err(|e| A2aFlowError::Transport(format!("call: {e}")))?;
        serde_json::from_slice(&response).map_err(|e| A2aFlowError::Decode(e.to_string()))
    }

    /// **Requester side.** What `target_node_id` serves: one
    /// [`A2aOffer`] per configured service, with its bounds, its
    /// retention terms and — for a paid service — its
    /// `net.pricing.terms@1`.
    ///
    /// Uncharged, and the only sanctioned way to learn a price: the
    /// offer's [`hash`](A2aOffer::hash) is what a commitment is computed
    /// against, so a caller that paid against an offer can prove which
    /// offer it paid against. A node serving the legacy free path
    /// (`serve_a2a`) has no describe service and answers a transport
    /// error — free-by-omission is not an offer.
    pub async fn describe_a2a(&self, target_node_id: u64) -> Result<Vec<A2aOffer>, A2aFlowError> {
        let response: Vec<u8> = self
            .call_typed(
                target_node_id,
                A2A_DESCRIBE_SERVICE,
                &Vec::<u8>::new(),
                CallOptionsTyped::default(),
            )
            .await
            .map_err(|e| A2aFlowError::Transport(format!("call: {e}")))?;
        serde_json::from_slice(&response).map_err(|e| A2aFlowError::Decode(e.to_string()))
    }

    /// **Requester side.** Ask `target_node_id` to validate `brief` and
    /// reserve capacity for it, returning the provider's
    /// [`PrepareReply`].
    ///
    /// **Uncharged, and it must run before any money moves**: the
    /// reservation carries the provider-minted `admission_id`, and the
    /// purchase hash a quote commits to is computed from it. A caller
    /// that skips prepare cannot hold a quote this provider will accept.
    ///
    /// Idempotent per `(this caller, brief.task_id)` for an identical
    /// brief — a retransmit returns the same reservation, so a lost
    /// reply costs nothing. A *different* brief under the same id is
    /// [`PrepareReply::Rejected`], never a silent re-reservation.
    ///
    /// The raw verb. `net-payments`' A2A caller flow composes it with a
    /// quote and a durable purchase attempt; use this directly only when
    /// keeping those records yourself.
    pub async fn prepare_a2a(
        &self,
        target_node_id: u64,
        brief: &TaskBrief,
    ) -> Result<PrepareReply, A2aFlowError> {
        let response: Vec<u8> = self
            .call_typed(
                target_node_id,
                A2A_PREPARE_SERVICE,
                &brief.encode(),
                CallOptionsTyped::default(),
            )
            .await
            .map_err(|e| A2aFlowError::Transport(format!("call: {e}")))?;
        PrepareReply::decode(&response).map_err(|e| A2aFlowError::Decode(e.to_string()))
    }

    /// **Requester side.** Submit a prepared task with its payment
    /// evidence: `prepared.brief` on the body, the quote id and the
    /// caller's binding signature on the headers.
    ///
    /// Sends to `prepared.provider_node` — exact-provider by
    /// construction, because the payment is bound to that node's
    /// reservation and is worth nothing anywhere else.
    ///
    /// Safe to re-send: the provider's admission is idempotent per
    /// purchase, so a lost reply is recovered by submitting the *same*
    /// proof again rather than by buying a second one. A payment or
    /// admission refusal is [`A2aFlowError::PaymentRefused`] with the
    /// provider's schematic; every other rejection stays in the body as
    /// `TaskAck { accepted: false, reason }`.
    ///
    /// Uses the raw call path rather than `call_typed` because the
    /// schematic rides a **reply header**, which the typed helper drops.
    pub async fn submit_task_paid(
        &self,
        prepared: &PreparedTask,
        proof: &TaskPaymentProof,
    ) -> Result<TaskAck, A2aFlowError> {
        // The A2A wire has always carried its payload inside a JSON
        // array-of-bytes envelope (`call_typed` with `Req = Resp =
        // Vec<u8>`), and it is spoken by the Node and Python bindings and
        // by peers on older builds. Dropping to the raw path to read a
        // reply header must not change the bytes on the wire, so the
        // envelope is applied here by hand.
        let body = serde_json::to_vec(&prepared.brief.encode())
            .map_err(|e| A2aFlowError::Decode(format!("encode brief: {e}")))?;
        let opts = CallOptions::default()
            .with_request_header(HDR_PAYMENT_QUOTE, proof.quote_id.clone().into_bytes())
            .with_request_header(HDR_PAYMENT_BINDING, proof.binding_sig.clone());
        let reply = match self
            .call(
                prepared.provider_node,
                A2A_TASK_SERVICE,
                bytes::Bytes::from(body),
                opts,
            )
            .await
        {
            Ok(reply) => reply,
            Err(RpcError::ServerError {
                status,
                message,
                headers,
            }) if status == ERR_PAYMENT => {
                return Err(A2aFlowError::PaymentRefused {
                    message,
                    schematic: schematic_of(&headers).map(Box::new),
                });
            }
            Err(e) => return Err(A2aFlowError::Transport(format!("call: {e}"))),
        };
        let envelope: Vec<u8> = serde_json::from_slice(&reply.body)
            .map_err(|e| A2aFlowError::Decode(format!("reply envelope: {e}")))?;
        TaskAck::decode(&envelope).map_err(|e| A2aFlowError::Decode(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a2a::{CancelToken, TaskState};
    use crate::mesh::MeshBuilder;
    use crate::Identity;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    /// A test executor: either completes with a fixed ref, or waits for cancel.
    struct TestExecutor {
        result: String,
        wait_for_cancel: bool,
        saw_cancel: Arc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl TaskExecutor for TestExecutor {
        async fn run(&self, _brief: TaskBrief, cancel: CancelToken) -> Result<String, String> {
            if self.wait_for_cancel {
                cancel.cancelled().await;
                self.saw_cancel.store(true, Ordering::SeqCst);
                return Err("stopped".to_string());
            }
            Ok(self.result.clone())
        }
    }

    async fn build_started(psk: &[u8; 32]) -> Mesh {
        let mesh = MeshBuilder::new("127.0.0.1:0", psk)
            .unwrap()
            .identity(Identity::generate())
            .build()
            .await
            .unwrap();
        mesh.start();
        mesh
    }

    /// Poll the executor's status for `task_id` until it satisfies `pred`.
    async fn wait_status(
        requester: &Mesh,
        executor_id: u64,
        task_id: &str,
        pred: impl Fn(&TaskState) -> bool,
    ) -> TaskState {
        for _ in 0..100 {
            if let Ok(Some(rec)) = requester.task_status(executor_id, task_id).await {
                if pred(&rec.state) {
                    return rec.state;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("task {task_id} never satisfied the status predicate");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_requester_submits_a_task_and_cancels_it_mid_run() {
        let psk = [0x61u8; 32];
        let executor_mesh = build_started(&psk).await;
        let saw = Arc::new(AtomicBool::new(false));
        let _handles = executor_mesh
            .serve_a2a(
                TaskRegistry::new(),
                Arc::new(TestExecutor {
                    result: String::new(),
                    wait_for_cancel: true,
                    saw_cancel: Arc::clone(&saw),
                }),
            )
            .expect("serve a2a");

        let requester = build_started(&psk).await;
        requester
            .connect_via(
                &executor_mesh.local_addr().to_string(),
                executor_mesh.public_key(),
                executor_mesh.node_id(),
            )
            .await
            .expect("connect to the executor");
        let exec_id = executor_mesh.node_id();

        // Hand off a long job — context rides as a Datafort ref.
        let brief = TaskBrief::new("grind a long job").with_context_refs(vec!["blob://ctx".into()]);
        let ack = requester
            .submit_task(exec_id, &brief)
            .await
            .expect("submit");
        assert!(ack.accepted);
        assert_eq!(ack.task_id, brief.task_id);

        // It reaches Running (the requester keeps working meanwhile).
        wait_status(&requester, exec_id, &brief.task_id, |s| {
            matches!(s, TaskState::Running)
        })
        .await;

        // Cancel mid-run → the executor demonstrably stops.
        assert!(requester
            .cancel_task(exec_id, &brief.task_id)
            .await
            .expect("cancel"));
        let state = wait_status(&requester, exec_id, &brief.task_id, |s| s.is_terminal()).await;
        assert_eq!(state, TaskState::Cancelled);
        assert!(
            saw.load(Ordering::SeqCst),
            "the remote executor observed the cancel"
        );

        requester.shutdown().await.ok();
        executor_mesh.shutdown().await.ok();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_task_completes_with_an_artifact_ref_over_the_wire() {
        let psk = [0x62u8; 32];
        let executor_mesh = build_started(&psk).await;
        let _handles = executor_mesh
            .serve_a2a(
                TaskRegistry::new(),
                Arc::new(TestExecutor {
                    result: "blob://summary-42".to_string(),
                    wait_for_cancel: false,
                    saw_cancel: Arc::new(AtomicBool::new(false)),
                }),
            )
            .expect("serve a2a");

        let requester = build_started(&psk).await;
        requester
            .connect_via(
                &executor_mesh.local_addr().to_string(),
                executor_mesh.public_key(),
                executor_mesh.node_id(),
            )
            .await
            .expect("connect");
        let exec_id = executor_mesh.node_id();

        let brief = TaskBrief::new("summarize");
        requester
            .submit_task(exec_id, &brief)
            .await
            .expect("submit");

        // The result lands as an artifact ref.
        let state = wait_status(&requester, exec_id, &brief.task_id, |s| s.is_terminal()).await;
        assert_eq!(
            state,
            TaskState::Completed {
                result_ref: "blob://summary-42".to_string()
            }
        );

        // Cancelling a finished task is a no-op; an unknown task is None.
        assert!(!requester
            .cancel_task(exec_id, &brief.task_id)
            .await
            .unwrap());
        assert!(requester
            .task_status(exec_id, "nope")
            .await
            .unwrap()
            .is_none());

        requester.shutdown().await.ok();
        executor_mesh.shutdown().await.ok();
    }
}
