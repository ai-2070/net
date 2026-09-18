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
    AdmissionReservation, CancelToken, PrepareReply, PreparedTask, ReservationRefusal,
    SubmitRejection, TaskAck, TaskBrief, TaskExecutor, TaskOwner, TaskRecord, TaskRegistry,
    TerminalHook,
};
use crate::a2a_journal::{
    now_secs, A2aAdmissionJournal, A2aAdmissions, A2aJournalError, AdmissionRecord, AdmissionState,
    AdmitOutcome, AttemptNote, DecisionOutcome, JournalOwner, SharedAdmissionStore, StateTag,
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

/// Hard deadline on every A2A round trip.
///
/// `CallOptions::deadline` defaults to `None`, which means *wait
/// forever*. Every verb here is a short control call — submit returns an
/// ack, not a result; status and cancel are lookups — so "forever" is
/// never the right answer for any of them.
///
/// What actually parks a caller, measured rather than assumed: a request
/// that is **delivered to a live service whose handler never answers**
/// (a wedged executor, a handler blocked on a dead dependency), or one
/// never delivered at all because it exceeded a packet — the defect
/// [`A2A_MAX_BRIEF_BYTES`] now prevents. A peer that serves no A2A
/// service at all is *not* this case: it fails fast with a no-route
/// error, because the reply channel is unknown.
///
/// The *task* is still unbounded — that is the point of A2A. Only the
/// control round trip is bounded, and an expired one is
/// [`A2aFlowError::Timeout`], which a caller can retry: every verb here
/// is idempotent per `(owner, task id)`.
pub const A2A_CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// The largest [`TaskBrief::encode`] output that can actually cross the
/// wire, in bytes.
///
/// **Why a brief has a limit far below the nRPC body cap.** The A2A wire
/// carries its payload inside a JSON *array-of-bytes* envelope (the
/// `call_typed` shape with `Req = Resp = Vec<u8>`, spoken by the Node and
/// Python bindings and by every peer on an older build). That encoding
/// costs up to **four bytes per payload byte** — `255,` — so an encoded
/// brief is quadrupled before it is framed. nRPC would accept 4 MiB, but
/// one mesh packet is [`MAX_PACKET_SIZE`], and a request that does not
/// fit is never delivered: before this limit existed, a brief with a
/// ~2 KB prompt simply vanished and the caller — with no deadline — waited
/// for a reply that could never come.
///
/// Derived from the transport, not copied from it: the per-packet payload
/// budget [`MAX_PAYLOAD_SIZE`] (itself `MAX_PACKET_SIZE - HEADER_SIZE -
/// TAG_SIZE`), less a reserve for the nRPC request framing that shares
/// the packet (service name, call id, and the payment headers a paid
/// submit carries), divided by the envelope's worst-case expansion. A
/// change to the packet budget therefore moves this limit at compile
/// time rather than leaving a stale number behind — and
/// `a_brief_at_the_wire_limit_round_trips` sends a brief of exactly this
/// size over a real two-node wire, so the arithmetic is checked against
/// the transport as well as against itself.
///
/// **A brief over this limit is refused locally**
/// ([`A2aFlowError::BriefTooLarge`]) and a configured service may not
/// announce a `max_prompt_bytes` above it
/// (`ServeError::A2aUndeliverableBounds`). Work that needs more room
/// belongs in a context artifact ref, which is what briefs carry refs
/// for.
///
/// [`MAX_PACKET_SIZE`]: net::adapter::net::MAX_PACKET_SIZE
/// [`MAX_PAYLOAD_SIZE`]: net::adapter::net::MAX_PAYLOAD_SIZE
pub const A2A_MAX_BRIEF_BYTES: usize =
    (net::adapter::net::MAX_PAYLOAD_SIZE - A2A_FRAMING_RESERVE) / JSON_BYTE_ARRAY_COST;

/// Worst-case bytes the array-of-bytes envelope spends per payload byte:
/// three digits and a separator (`255,`).
const JSON_BYTE_ARRAY_COST: usize = 4;

/// Packet budget held back for everything that rides beside the body in
/// one request: the nRPC frame's own fields, the longest A2A service
/// name, and a paid submit's two payment headers (a quote id and a
/// 64-byte signature). Generous on purpose — the cost of reserving too
/// much is a slightly shorter prompt, and the cost of reserving too
/// little is a request that disappears.
const A2A_FRAMING_RESERVE: usize = 1024;

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
    /// The round trip outran [`A2A_CALL_TIMEOUT`]. Distinct from
    /// [`Transport`](Self::Transport) because it says something
    /// different: the request may or may not have arrived, so the
    /// outcome is *unknown* rather than failed. Every A2A verb is
    /// idempotent per `(owner, task id)`, so retrying is safe — a submit
    /// that did land answers `Existing` rather than starting a second
    /// run.
    #[error("a2a call timed out after {}s", A2A_CALL_TIMEOUT.as_secs())]
    Timeout,
    /// The encoded brief is too large to cross the wire, refused
    /// **locally** before any packet was sent.
    ///
    /// Failing here rather than on the wire is the whole point: an
    /// over-large request is not delivered and not refused, it simply
    /// disappears, which is indistinguishable from a peer that never
    /// answers. See [`A2A_MAX_BRIEF_BYTES`] for why the ceiling is what
    /// it is.
    #[error(
        "a2a brief is {encoded} bytes encoded, over the {limit}-byte wire limit — \
         shorten the prompt or move the bulk into a context artifact ref"
    )]
    BriefTooLarge {
        /// `TaskBrief::encode().len()`.
        encoded: usize,
        /// [`A2A_MAX_BRIEF_BYTES`].
        limit: usize,
    },
}

/// Encode a task id as a request body (a JSON string). One place so the status
/// and cancel services agree with their callers.
fn task_ref_bytes(task_id: &str) -> Vec<u8> {
    serde_json::to_vec(task_id).unwrap_or_default()
}

/// Typed call options carrying [`A2A_CALL_TIMEOUT`] as a hard deadline,
/// stamped fresh per call. One place, so no A2A verb can be written
/// without a bound by forgetting to add one.
fn bounded_typed() -> CallOptionsTyped {
    let mut opts = CallOptionsTyped::default();
    opts.raw.deadline = Some(std::time::Instant::now() + A2A_CALL_TIMEOUT);
    opts
}

/// Map a client-side RPC failure, keeping a timeout distinguishable from
/// a hard transport error: the first leaves the outcome unknown and is
/// safe to retry, the second does not.
fn map_call_err(e: RpcError) -> A2aFlowError {
    match e {
        RpcError::Timeout { .. } => A2aFlowError::Timeout,
        other => A2aFlowError::Transport(format!("call: {other}")),
    }
}

/// Refuse a brief that cannot cross the wire, before a packet is sent.
///
/// The encoded brief — not the prompt — is what gets quadrupled by the
/// array-of-bytes envelope, so the check is on `encode()`: context refs,
/// tags and the service fields all count, and a caller cannot evade the
/// limit by splitting a long prompt into many refs.
fn check_brief(brief: &TaskBrief) -> Result<Vec<u8>, A2aFlowError> {
    let encoded = brief.encode();
    if encoded.len() > A2A_MAX_BRIEF_BYTES {
        return Err(A2aFlowError::BriefTooLarge {
            encoded: encoded.len(),
            limit: A2A_MAX_BRIEF_BYTES,
        });
    }
    Ok(encoded)
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

/// A payment or admission refusal, as a **structured verdict** rather
/// than a rendered reply.
///
/// The configured submit decides first and renders last, because the
/// same verdict goes to two places: the reply this caller gets and the
/// reservation channel every concurrent duplicate is parked on. A
/// verdict that existed only as bytes could not reach the second.
fn refuse_payment(schematic: &FailureSchematic) -> ReservationRefusal {
    ReservationRefusal::Payment {
        message: schematic.message.clone(),
        schematic: Box::new(schematic.clone()),
    }
}

/// A gate denial, passed through untouched: the gate's own message and
/// its own schematic.
fn refuse_gate(message: String, schematic: &FailureSchematic) -> ReservationRefusal {
    ReservationRefusal::Payment {
        message,
        schematic: Box::new(schematic.clone()),
    }
}

/// A non-payment rejection, in-body exactly like the free path.
fn refuse(reason: impl std::fmt::Display) -> ReservationRefusal {
    ReservationRefusal::Rejected(reason.to_string())
}

/// Render a verdict for the wire.
///
/// A payment refusal goes out on the full-fidelity reply channel: the
/// human message stays the body (byte-identical to what the wire has
/// always carried for a paid refusal) and the schematic rides exactly
/// one reply header. Returned as `Ok(payload)` because the
/// `RpcHandlerError` convenience channel flattens headers away — the
/// same reason `PaidToolHandler` does it.
///
/// One function, so a waiter's reply is **byte-identical** to the
/// decider's.
fn refusal_payload(refusal: &ReservationRefusal) -> RpcResponsePayload {
    match refusal {
        ReservationRefusal::Rejected(reason) => ack_refused(reason),
        ReservationRefusal::Payment { message, schematic } => RpcResponsePayload {
            status: RpcStatus::Application(ERR_PAYMENT),
            headers: schematic.header_entry().into_iter().collect(),
            body: message.clone().into(),
        },
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
        // Capacity and the write are ONE transaction. Counting first and
        // inserting second is what let four concurrent prepares of
        // distinct task ids all pass a ceiling of one: each read a count
        // taken before any of them had written.
        match self
            .store
            .admit_reserved(record.clone(), offer.bounds.max_in_flight, now)
            .await
        {
            Ok(AdmitOutcome::Admitted(stored)) => PrepareReply::Reservation(self.reservation_of(
                &stored,
                &offer,
                now.saturating_add(offer.reservation_ttl_secs),
            )),
            Ok(AdmitOutcome::Busy) => PrepareReply::Busy,
            // Lost the CAS to a concurrent prepare of the same task: the
            // stored record is authoritative, so answer from it rather
            // than from the reservation this call would have made.
            Ok(AdmitOutcome::Existing(found)) if found.commitment == commitment => {
                self.prepare_reply_for(&found, &offer, now).await
            }
            Ok(AdmitOutcome::Existing(_)) => rejected(SubmitRejection::IdReusedForDifferentBrief {
                task_id: record.task_id.clone(),
            }),
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
                // Re-acquire under the ceiling in ONE transaction,
                // keeping this record's admission id and incarnation —
                // never hand back a reservation that does not hold
                // capacity, and never check the ceiling in a
                // transaction other than the one that writes.
                let candidate = AdmissionRecord::reserved(
                    record.owner,
                    record.brief.clone(),
                    offer,
                    record.admission_id.clone(),
                    record.commitment.clone(),
                    now,
                );
                match self
                    .store
                    .admit_reserved(candidate, offer.bounds.max_in_flight, now)
                    .await
                {
                    Ok(AdmitOutcome::Admitted(stored)) => {
                        let expires_at = match stored.state {
                            AdmissionState::Reserved { expires_at } => expires_at,
                            _ => now.saturating_add(offer.reservation_ttl_secs),
                        };
                        PrepareReply::Reservation(self.reservation_of(&stored, offer, expires_at))
                    }
                    Ok(AdmitOutcome::Busy) => PrepareReply::Busy,
                    // A concurrent decision got there first; its record is
                    // authoritative.
                    Ok(AdmitOutcome::Existing(found)) => match found.state {
                        AdmissionState::Reserved { expires_at } => PrepareReply::Reservation(
                            self.reservation_of(&found, offer, expires_at),
                        ),
                        _ => PrepareReply::Existing { task_id },
                    },
                    Err(e) => rejected(e),
                }
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
    /// **Decide, publish, then render.** Everything from S4 to S7 is
    /// [`decide`](Self::decide), which returns a verdict rather than
    /// reply bytes; this function publishes that verdict to every
    /// concurrent duplicate parked on the reservation and only then
    /// renders it for this caller. A waiter therefore receives the
    /// decider's complete answer — the same status, the same schematic,
    /// the same body — instead of a generic rejection that reads as
    /// permanent when the decider's own refusal was retryable.
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
        let tool_id = tool_id_of(&offer.service_id);
        let task_id = brief.task_id.clone();

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
                    // The decider's verdict, rendered by the one function
                    // that rendered its own reply.
                    Err(refusal) => refusal_payload(&refusal),
                };
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

        // S4..S7. `deciding` is the record whose capacity hold has to be
        // released on every outcome but a launch, which is what makes
        // "held across the awaited preflight and redemption" bounded
        // rather than permanent.
        let mut deciding: Option<AdmissionRecord> = None;
        let outcome = self.decide(&sub, &offer, &brief, now, &mut deciding).await;
        if !matches!(outcome, Ok(Admitted::Launch)) {
            if let Some(record) = deciding.as_ref() {
                let _ = self.store.close_decision(record, now_secs()).await;
            }
        }
        match outcome {
            // S8 — launch, retained for exactly as long as the offer
            // published. `launch` publishes the id to the waiters itself.
            Ok(Admitted::Launch) => {
                let id = ticket
                    .with_retention(offer.retention_secs)
                    .launch(Arc::clone(&self.executor));
                ack_accepted(id)
            }
            Ok(Admitted::Underway(id)) => {
                ticket.resolve_existing(id.clone());
                ack_accepted(id)
            }
            Err(refusal) => {
                let payload = refusal_payload(&refusal);
                ticket.refuse(refusal);
                payload
            }
        }
    }

    /// S4..S7: everything between the registry claim and the spawn.
    ///
    /// Returns the decision, never reply bytes — see
    /// [`submit`](Self::submit) for why. `deciding` is set to the record
    /// a capacity hold was opened on, so the caller can release it.
    async fn decide(
        &self,
        sub: &Submission<'_>,
        offer: &A2aOffer,
        brief: &TaskBrief,
        now: u64,
        deciding: &mut Option<AdmissionRecord>,
    ) -> Result<Admitted, ReservationRefusal> {
        let (owner, task_id, tool_id) = (sub.owner, sub.task_id, sub.tool_id);
        let paid = offer.pricing_terms.is_some();
        let commitment = task_commitment(offer, brief);
        let unavailable =
            |detail: String| refuse_payment(&schematic_journal_unavailable(tool_id, &detail));

        // S4 — per-state dispatch. The ONLY branch point.
        let found = self
            .store
            .lookup(owner, task_id)
            .await
            .map_err(|e| unavailable(e.to_string()))?;
        let mut record = match found {
            Some(record) if record.commitment != commitment => {
                return Err(refuse(SubmitRejection::IdReusedForDifferentBrief {
                    task_id: task_id.to_string(),
                }))
            }
            Some(record) => match &record.state {
                // Store-side "already under way": status shows the
                // recorded (or interrupted) state; never relaunched.
                AdmissionState::Launched { .. } | AdmissionState::Terminal { .. } => {
                    return Ok(Admitted::Underway(task_id.to_string()))
                }
                // Post-payment revocation: the same refusal, every time,
                // with no state change. An operator's `resolve` is the
                // only exit.
                AdmissionState::Reconcile {
                    claimed_quote_id, ..
                } => {
                    return Err(refuse_payment(&schematic_admission_revoked(
                        tool_id,
                        claimed_quote_id.as_deref(),
                    )))
                }
                _ => Some(record),
            },
            None => {
                // Ran once, result retired: the ledger bars a relaunch,
                // and the redeem step is never reached.
                if self
                    .store
                    .ledger_has(owner, task_id)
                    .await
                    .map_err(|e| unavailable(e.to_string()))?
                {
                    return Err(refuse(SubmitRejection::Retired {
                        task_id: task_id.to_string(),
                    }));
                }
                if paid {
                    // A paid service requires a prior reservation — and
                    // this is a payment/admission verdict, so it is
                    // refused structurally, before the gate is touched.
                    return Err(refuse_payment(&schematic_no_reservation(tool_id)));
                }
                None
            }
        };

        // S4b — open the decision. Identity-bound, so nothing that
        // follows can land on a replacement record; and the record holds
        // its capacity slot for the whole awaited window, so a
        // reservation that lapses mid-decision cannot have its slot
        // taken by a competitor and then be transitioned against this
        // stale time sample. A lapsed reservation re-acquires here, in
        // the same transaction as the check — over capacity is a
        // retryable in-body Busy with the gate NOT called, never an
        // unpaid rejection of a purchase that is still good.
        if let Some(current) = record.as_ref() {
            match self
                .store
                .open_decision(current, offer.bounds.max_in_flight, now)
                .await
            {
                Ok(DecisionOutcome::Open(open)) => {
                    *deciding = Some((*open).clone());
                    record = Some(*open);
                }
                Ok(DecisionOutcome::Busy) => return Err(refuse(SubmitRejection::Busy)),
                Err(e) => return Err(unavailable(e.to_string())),
            }
        }

        // S5 — the application preflight runs again, on every path:
        // authority may have changed since prepare.
        if let Err(reason) = self.run_preflight(owner, offer, brief).await {
            return Err(self.revoked(sub, record.as_ref(), reason).await);
        }

        // S5a — free inline admission: the same capacity rule prepare
        // applies, for a caller that never prepared, and in the same one
        // transaction.
        if record.is_none() {
            let inline = AdmissionRecord::reserved(
                owner,
                brief.clone(),
                offer,
                // No admission id: nothing was minted, because nothing
                // can be purchased against a free offer.
                None,
                commitment.clone(),
                now,
            );
            match self
                .store
                .admit_reserved(inline, offer.bounds.max_in_flight, now)
                .await
            {
                Ok(AdmitOutcome::Admitted(stored)) => record = Some(*stored),
                Ok(AdmitOutcome::Busy) => return Err(refuse(SubmitRejection::Busy)),
                // A prepare of the same task landed between the lookup
                // and this insert. Its reservation is authoritative;
                // this submit claims the launch against it.
                Ok(AdmitOutcome::Existing(found))
                    if found.commitment == commitment
                        && matches!(found.state, AdmissionState::Reserved { .. }) =>
                {
                    record = Some(*found)
                }
                Ok(AdmitOutcome::Existing(found)) => {
                    return Err(refuse(format!(
                        "this task is already admitted as {}",
                        found.state.tag().as_str()
                    )))
                }
                Err(e) => return Err(unavailable(e.to_string())),
            }
        }

        // S6 / S6′ — payment. Free services skip both entirely.
        if paid {
            let Some(current) = record.as_ref() else {
                // Unreachable: a paid service with no record answered
                // `no_reservation` at S4. Fail closed rather than launch.
                return Err(refuse_payment(&schematic_no_reservation(tool_id)));
            };
            match &current.state {
                AdmissionState::Reserved { .. } => {
                    if let Some(refusal) = self.redeem(sub, current, now).await {
                        return Err(refusal);
                    }
                }
                AdmissionState::Paid { quote_id, .. } => {
                    // S6′ — the recorded evidence is authoritative and
                    // the gate is NOT called: this admission was already
                    // paid for, and redeeming again is how a retry turns
                    // into a second charge.
                    let matches_record =
                        sub.quote == Some(quote_id.as_str()) && sub.binding.is_some();
                    if !matches_record {
                        self.note(owner, task_id, "binding_rejected", sub.quote)
                            .await;
                        return Err(refuse_payment(&schematic_binding_rejected(
                            tool_id,
                            Some(quote_id),
                            "this admission is already paid for by another quote, or the \
                             submission carried no binding signature"
                                .to_string(),
                        )));
                    }
                }
                other => {
                    return Err(unavailable(format!(
                        "admission is {}",
                        other.tag().as_str()
                    )))
                }
            }
        }

        // S7 — the launch claim is durable BEFORE the spawn. A write
        // failure leaves the store exactly as it was and runs nothing;
        // the retry re-enters S4 with the same state.
        self.store
            .claim_launch(owner, task_id, now)
            .await
            .map_err(|e| unavailable(e.to_string()))?;
        Ok(Admitted::Launch)
    }

    /// S5's refusal dispatch: what a preflight failure at submit means
    /// depends entirely on whether money may already have moved.
    async fn revoked(
        &self,
        sub: &Submission<'_>,
        record: Option<&AdmissionRecord>,
        reason: String,
    ) -> ReservationRefusal {
        let (owner, task_id, tool_id) = (sub.owner, sub.task_id, sub.tool_id);
        let unavailable =
            |detail: String| refuse_payment(&schematic_journal_unavailable(tool_id, &detail));
        let Some(record) = record else {
            // Free, never prepared: nothing was reserved.
            return refuse(reason);
        };
        if !record.paid {
            // Free, prepared: delete the reservation — nothing financial
            // exists, so leaving it would hold capacity for work that
            // will not run. Identity-bound: the decision to abandon this
            // work may not delete a replacement, and it deletes its own
            // deciding record because this *is* the decision ending.
            if let Err(e) = self.store.delete_reservation(record).await {
                return unavailable(e.to_string());
            }
            return refuse(reason);
        }
        let (from, claimed, payer) = match &record.state {
            // Paid service, reserved, no payment presented: nothing has
            // been claimed paid, so this is an ordinary unpaid rejection
            // and the reservation stays exactly as it was.
            AdmissionState::Reserved { .. } if !sub.carries_payment() => return refuse(reason),
            AdmissionState::Reserved { .. } => {
                ([StateTag::Reserved], sub.quote.map(str::to_string), None)
            }
            AdmissionState::Paid { quote_id, payer } => {
                ([StateTag::Paid], Some(quote_id.clone()), Some(*payer))
            }
            other => return unavailable(format!("admission is {}", other.tag().as_str())),
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
            return unavailable(e.to_string());
        }
        refuse_payment(&schematic_admission_revoked(tool_id, claimed.as_deref()))
    }

    /// S6 — redeem a payment for exactly this reservation. `Some` is the
    /// refusal to answer; `None` means the record is now `Paid`.
    async fn redeem(
        &self,
        sub: &Submission<'_>,
        record: &AdmissionRecord,
        now: u64,
    ) -> Option<ReservationRefusal> {
        let (owner, task_id, tool_id) = (sub.owner, sub.task_id, sub.tool_id);
        let Some(quote_id) = sub.quote else {
            self.note(owner, task_id, "missing_quote", None).await;
            return Some(refuse_payment(&schematic_missing_quote(tool_id)));
        };
        // Mandatory, unlike a paid tool's optional bearer fallback: a
        // task is a long-running side effect, so possession of a quote id
        // is not evidence that the payer authorized this submission.
        let Some(binding) = sub.binding else {
            self.note(owner, task_id, "binding_required", Some(quote_id))
                .await;
            return Some(refuse_payment(&schematic_binding_required(
                tool_id, quote_id,
            )));
        };
        let Some(gate) = self.gate.as_ref() else {
            // Serve-time invariants make this unreachable; fail closed
            // rather than serve paid work for nothing.
            return Some(refuse_payment(&FailureSchematic::gate_missing(tool_id)));
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
                return Some(refuse_gate(denial.message, &denial.schematic));
            }
        };
        // A verified end-to-end principal is matched against the payer:
        // a payment made by one entity cannot admit another's task.
        if matches!(self.principal, A2aPrincipal::OrgAdmitted(_))
            && owner != TaskOwner::Entity(evidence.payer)
        {
            self.note(owner, task_id, "binding_rejected", Some(&evidence.quote_id))
                .await;
            return Some(refuse_payment(&schematic_binding_rejected(
                tool_id,
                Some(&evidence.quote_id),
                "the payer this quote was issued to is not the admitted caller".to_string(),
            )));
        }
        // The financial write re-presents the admission the gate was
        // asked about: the generation, the admission id the purchase hash
        // above was computed over, and the commitment. A record that is
        // no longer that incarnation refuses — and the redemption is
        // retained against the original admission, because the payment
        // happened whether or not the provider can still use it. That is
        // reconciliation, not a retry: a second purchase would be a
        // second charge for work this provider will not admit.
        match self
            .store
            .redeem(record, evidence.quote_id.clone(), evidence.payer, now)
            .await
        {
            Ok(()) => None,
            Err(A2aJournalError::Superseded { .. }) => Some(refuse_payment(
                &schematic_admission_revoked(tool_id, Some(&evidence.quote_id)),
            )),
            Err(e) => {
                // The redeem was idempotent per purchase hash, so the
                // retry re-enters S4 as `Reserved`, redeems again without
                // a second charge, and proceeds.
                Some(refuse_payment(&schematic_journal_unavailable(
                    tool_id,
                    &e.to_string(),
                )))
            }
        }
    }
}

/// What [`ConfiguredA2a::decide`] concluded when it did not refuse.
enum Admitted {
    /// The launch claim is durable: spawn the executor.
    Launch,
    /// The work this submission names is already under way — a launched
    /// or recorded admission. Answer with its id and spawn nothing.
    Underway(String),
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
            if offer.bounds.max_prompt_bytes > A2A_MAX_BRIEF_BYTES as u64 {
                return Err(ServeError::A2aUndeliverableBounds(format!(
                    "service {id:?} announces max_prompt_bytes {} but a brief larger than \
                     {A2A_MAX_BRIEF_BYTES} bytes encoded cannot cross the wire — a caller \
                     sizing its work against that bound would send a request that is never \
                     delivered rather than one that is refused",
                    offer.bounds.max_prompt_bytes
                )));
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
                &check_brief(brief)?,
                bounded_typed(),
            )
            .await
            .map_err(map_call_err)?;
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
                bounded_typed(),
            )
            .await
            .map_err(map_call_err)?;
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
                bounded_typed(),
            )
            .await
            .map_err(map_call_err)?;
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
                bounded_typed(),
            )
            .await
            .map_err(map_call_err)?;
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
                &check_brief(brief)?,
                bounded_typed(),
            )
            .await
            .map_err(map_call_err)?;
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
        let body = serde_json::to_vec(&check_brief(&prepared.brief)?)
            .map_err(|e| A2aFlowError::Decode(format!("encode brief: {e}")))?;
        let opts = CallOptions {
            deadline: Some(std::time::Instant::now() + A2A_CALL_TIMEOUT),
            ..CallOptions::default()
        }
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
            Err(e) => return Err(map_call_err(e)),
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
