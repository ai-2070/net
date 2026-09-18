//! Agent-to-agent (A2A) task handoff — the transport-independent core
//! (`HERMES_INTEGRATION_PLAN_V2.md` Phase 3; frozen plan Phase 5).
//!
//! In-root A2A is for **parallelism**: one enrolled agent hands a long job to
//! another (which does *not* share its memory), keeps working, and can cancel
//! mid-run — and the other side **demonstrably stops**. Same-root *sequential*
//! work uses direct capabilities (Phase 2), not this — "asking the other Hermes
//! is briefing an amnesiac colleague with partial memory."
//!
//! This module is the executor side's task manager + the wire types, transport
//! independent:
//!
//! - [`TaskBrief`] — the job + the context the executor needs as **Datafort
//!   artifact refs** (the other agent doesn't share your memory, so inlining
//!   would pretend otherwise).
//! - [`TaskState`] — the lifecycle `requested → accepted → running →
//!   completed{ref} | failed | cancelled | interrupted`.
//! - [`TaskExecutor`] — the host agent's runner (the plugin wires it to
//!   Hermes's own agent loop).
//! - [`TaskRegistry`] — spawns executors, tracks their state, and routes
//!   cancellation through a [`CancelToken`] so a cancel stops the work.
//!   Admission is split from launch: [`TaskRegistry::reserve`] claims an
//!   `(owner, task id)` and hands back an [`AdmissionTicket`], and
//!   [`AdmissionTicket::launch`] spawns the executor — the window a paid
//!   serving path uses to redeem a payment before any work starts.
//!   [`TaskRegistry::submit`] is the free path: reserve + launch, one call.
//! - [`TaskAck`] / [`TaskRecord`] — the requester-facing shapes.
//! - [`A2aOffer`] / [`task_commitment`] / [`purchase_hash`] — what a
//!   service publishes and the canonical hashes that bind one purchase to
//!   one reservation of exactly one brief.
//!
//! The mesh wiring (serve + client) is `mesh_a2a` (gated `net + cortex`).
//!
//! # Where the paid path fits
//!
//! This module owns the admission window and the canonical hashes;
//! the catalog-driven serving path built on them is
//! `Mesh::serve_a2a_configured` in `mesh_a2a`, and its order is
//! **prepare → purchase → submit → claim → launch**. A service
//! publishes an [`A2aOffer`]; a prepare validates the brief, reserves
//! capacity and mints the admission id; [`purchase_hash`] binds the
//! caller's payment to *that* reservation of exactly this
//! [`TaskBrief`] (so the same proof under another owner or another
//! reservation computes a different hash); and the executor is
//! spawned only after a durable launch claim. Who a submission is
//! attributed to is `A2aPrincipal`'s decision — the session-authenticated
//! deliverer or an organization-admitted entity, documented in
//! `mesh_a2a` — and [`TaskOwner`] is what that decision produces here.
//!
//! [`TaskState::Interrupted`] is the one wire addition of that slice;
//! its cross-version note is on the variant.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::tool_payment::FailureSchematic;

/// The lifecycle state of an A2A task.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TaskState {
    /// Submitted but not yet recorded by an executor.
    Requested,
    /// The executor accepted the brief; work is queued.
    Accepted,
    /// The executor is running the job.
    Running,
    /// Done — the result is an **artifact (Datafort) ref**, promoted home
    /// explicitly rather than inlined.
    Completed {
        /// The Datafort/blob ref the result was written to.
        result_ref: String,
    },
    /// The executor failed.
    Failed {
        /// A human-readable failure reason.
        error: String,
    },
    /// Cancelled by the requester; the executor stopped.
    Cancelled,
    /// The executor's outcome is not knowable from this process: the run
    /// was interrupted by a provider restart (or resolved by an operator)
    /// rather than by the executor returning.
    ///
    /// Terminal — there is nothing left to wait for and nothing to cancel
    /// — and produced only by a serving path that keeps a durable
    /// admission record; the free registry never mints one. `detail` names
    /// *which* ambiguity (`paid_not_started`, `outcome_unknown`,
    /// `admission_revoked`).
    ///
    /// **The one wire addition of the paid-admission slice.** Because
    /// [`TaskState`] is a serde-tagged enum, a Rust requester built
    /// before this variant existed cannot decode a status reply that
    /// carries it; the Python and Node bindings return the status as a
    /// JSON string and pass the tag through untouched. Nothing on the
    /// free path produces it, so only a deployment that opts into a
    /// configured catalog can put one on the wire.
    Interrupted {
        /// Which ambiguity this is, snake_case.
        detail: String,
    },
}

impl TaskState {
    /// Whether this is an end state (no further transitions).
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            TaskState::Completed { .. }
                | TaskState::Failed { .. }
                | TaskState::Cancelled
                | TaskState::Interrupted { .. }
        )
    }

    /// A short label for logging / display.
    pub fn label(&self) -> &'static str {
        match self {
            TaskState::Requested => "requested",
            TaskState::Accepted => "accepted",
            TaskState::Running => "running",
            TaskState::Completed { .. } => "completed",
            TaskState::Failed { .. } => "failed",
            TaskState::Cancelled => "cancelled",
            TaskState::Interrupted { .. } => "interrupted",
        }
    }
}

/// A task brief: the job plus the context the executor needs, carried as
/// **Datafort artifact refs** (the other agent doesn't share your memory).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskBrief {
    /// The task id (client-generated, unique).
    pub task_id: String,
    /// The job description.
    pub prompt: String,
    /// Artifact refs the executor should read for context.
    #[serde(default)]
    pub context_refs: Vec<String>,
    /// Free-form routing / classification tags.
    #[serde(default)]
    pub tags: Vec<String>,
    /// The service this brief is for, on a catalog-driven serving path.
    /// Absent on the free path, which serves one unnamed executor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    /// The offer revision this brief was prepared against. A brief naming
    /// a revision the provider has retired is refused before anything is
    /// reserved or charged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
}

impl TaskBrief {
    /// A brief for `prompt` with a fresh random task id.
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            task_id: random_id(),
            prompt: prompt.into(),
            context_refs: Vec::new(),
            tags: Vec::new(),
            service: None,
            revision: None,
        }
    }

    /// Attach context artifact refs (builder-style).
    #[must_use]
    pub fn with_context_refs(mut self, refs: Vec<String>) -> Self {
        self.context_refs = refs;
        self
    }

    /// Attach routing tags (builder-style).
    #[must_use]
    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }

    /// Name the service and the offer revision this brief targets
    /// (builder-style). Only a catalog-driven serving path reads them; the
    /// free path ignores both.
    #[must_use]
    pub fn with_service(mut self, service: impl Into<String>, revision: impl Into<String>) -> Self {
        self.service = Some(service.into());
        self.revision = Some(revision.into());
        self
    }

    /// Use a caller-chosen task id instead of the random one
    /// [`new`](Self::new) minted (builder-style).
    ///
    /// A retained id is what makes a purchase resumable: the caller that
    /// prepared, paid and then lost the submit reply re-submits the *same*
    /// id and converges on the original admission instead of buying a
    /// second one.
    #[must_use]
    pub fn with_task_id(mut self, task_id: impl Into<String>) -> Self {
        self.task_id = task_id.into();
        self
    }

    /// Canonical JSON bytes for the wire.
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// Decode from JSON bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, A2aError> {
        serde_json::from_slice(bytes).map_err(|e| A2aError::Decode(e.to_string()))
    }
}

/// The requester's acknowledgement of a submitted task.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskAck {
    /// The task id (echoes the brief's).
    pub task_id: String,
    /// Whether the executor accepted the brief.
    pub accepted: bool,
    /// Why it was rejected, if it wasn't accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl TaskAck {
    /// Canonical JSON bytes for the wire.
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }
    /// Decode from JSON bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, A2aError> {
        serde_json::from_slice(bytes).map_err(|e| A2aError::Decode(e.to_string()))
    }
}

/// A recorded task: the brief, its current state, and when it last changed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskRecord {
    /// The submitted brief.
    pub brief: TaskBrief,
    /// The current lifecycle state.
    pub state: TaskState,
    /// Unix seconds of the last state change.
    pub updated_at: u64,
}

impl TaskRecord {
    /// Canonical JSON bytes for the wire.
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }
    /// Decode from JSON bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, A2aError> {
        serde_json::from_slice(bytes).map_err(|e| A2aError::Decode(e.to_string()))
    }
}

/// The capacity and size limits a service publishes with its offer and
/// re-checks on every brief it admits. Committed into the offer hash, so a
/// provider cannot quietly widen — or narrow — what a caller paid for.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct A2aBounds {
    /// Largest accepted `prompt`, in UTF-8 bytes.
    pub max_prompt_bytes: u64,
    /// Largest accepted number of `context_refs`.
    pub max_context_refs: u64,
    /// Largest accepted number of `tags`.
    pub max_tags: u64,
    /// Largest accepted single tag, in UTF-8 bytes.
    pub max_tag_bytes: u64,
    /// How many tasks of this service may be admitted but unfinished at
    /// once. Capacity, not a per-brief bound — [`A2aOffer::check_bounds`]
    /// deliberately does not look at it.
    pub max_in_flight: u64,
}

/// What a provider publishes for one A2A service: what it is, what it
/// costs (if anything), what it accepts, and how long each of its records
/// lives.
///
/// Every field except [`description`](Self::description) is committed by
/// [`hash`](Self::hash) — including all three retention terms, because
/// they bound how long a purchased admission stays usable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct A2aOffer {
    /// The service name a brief must carry to reach this offer.
    pub service_id: String,
    /// The offer revision. A brief prepared against a retired revision is
    /// refused before anything is reserved or charged.
    pub revision: String,
    /// Human description. **Not** committed: cosmetic, and editable
    /// without invalidating outstanding reservations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// `net.pricing.terms@1` canonical JSON — `Some` exactly for a paid
    /// service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing_terms: Option<String>,
    /// What a brief may carry, and how many may run at once.
    pub bounds: A2aBounds,
    /// How long a prepared-but-unsubmitted reservation holds capacity.
    pub reservation_ttl_secs: u64,
    /// How long an unpaid reservation *record* is kept — the window in
    /// which a caller who paid but never submitted keeps its admission.
    pub reservation_retention_secs: u64,
    /// How long a terminal *result* stays readable.
    pub retention_secs: u64,
}

impl A2aOffer {
    /// This offer's canonical hash: blake3 hex over the typed
    /// `net.a2a.offer@1` JSON encoding.
    ///
    /// A [`task_commitment`] carries it, so a commitment is only valid
    /// against the exact offer it was computed for: retire a revision,
    /// re-price, or widen the bounds and every outstanding commitment
    /// stops matching.
    pub fn hash(&self) -> String {
        hash_canonical(&OfferV1 {
            object: OBJECT_OFFER_V1,
            service_id: &self.service_id,
            revision: &self.revision,
            pricing_terms: self.pricing_terms.as_deref(),
            bounds: &self.bounds,
            reservation_ttl_secs: self.reservation_ttl_secs,
            reservation_retention_secs: self.reservation_retention_secs,
            retention_secs: self.retention_secs,
        })
    }

    /// Check `brief` against this offer's per-brief size bounds.
    ///
    /// Pure: nothing is reserved, nothing is charged. Both admission
    /// points re-run it — a prepare, and the submit that follows — so a
    /// brief that grew between them is refused rather than admitted on the
    /// strength of the earlier check. Capacity
    /// ([`max_in_flight`](A2aBounds::max_in_flight)) is not a brief
    /// property and is enforced by the serving path, not here.
    pub fn check_bounds(&self, brief: &TaskBrief) -> Result<(), SubmitRejection> {
        if brief.prompt.len() as u64 > self.bounds.max_prompt_bytes {
            return Err(SubmitRejection::BoundsExceeded {
                field: "prompt_bytes".to_string(),
                limit: self.bounds.max_prompt_bytes,
            });
        }
        if brief.context_refs.len() as u64 > self.bounds.max_context_refs {
            return Err(SubmitRejection::BoundsExceeded {
                field: "context_refs".to_string(),
                limit: self.bounds.max_context_refs,
            });
        }
        if brief.tags.len() as u64 > self.bounds.max_tags {
            return Err(SubmitRejection::BoundsExceeded {
                field: "tags".to_string(),
                limit: self.bounds.max_tags,
            });
        }
        if brief
            .tags
            .iter()
            .any(|t| t.len() as u64 > self.bounds.max_tag_bytes)
        {
            return Err(SubmitRejection::BoundsExceeded {
                field: "tag_bytes".to_string(),
                limit: self.bounds.max_tag_bytes,
            });
        }
        Ok(())
    }
}

/// A provider-minted reservation of one task: what the caller must pay
/// for, and what proves the payment belongs to *this* reservation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AdmissionReservation {
    /// The reserved task id.
    pub task_id: String,
    /// Provider-minted, per-reservation. What makes the purchase
    /// un-replayable under another owner or another reservation.
    pub admission_id: String,
    /// [`task_commitment`] of the offer + brief this reservation admits.
    pub commitment: String,
    /// [`purchase_hash`] of `admission_id` + `commitment` — the value that
    /// rides the quote as its input hash.
    pub purchase_hash: String,
    /// The capability to quote against, `"{node_id}/net.a2a.task/{service_id}"`.
    pub capability: String,
    /// `net.pricing.terms@1` canonical JSON — `Some` exactly for a paid
    /// service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing_terms: Option<String>,
    /// Unix seconds after which the reservation no longer holds capacity.
    pub expires_at: u64,
}

/// What `net.a2a.prepare` answers.
///
/// Prepare is **uncharged and read-only on the money side**: the only
/// state it can create is a bounded capacity reservation, and every
/// outcome below is an in-body reply rather than a transport failure —
/// the requester reads one shape whether it got a reservation, a task
/// that is already under way, or a refusal.
///
/// The refusal arms exist so a caller learns *before a quote exists*
/// that this work will not be admitted. That ordering is the point: an
/// invalid, oversized, unauthorized or over-capacity brief is refused
/// with nothing to reconcile, because nothing was ever paid.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum PrepareReply {
    /// Admitted: this reservation is what to quote and pay against.
    /// Idempotent — preparing the same brief under the same id again
    /// returns the same `admission_id`.
    Reservation(AdmissionReservation),
    /// This task is already launched or finished. Nothing to buy; poll
    /// status.
    Existing {
        /// The task already under way (or recorded) under this id.
        task_id: String,
    },
    /// A payment landed and then the provider could no longer admit the
    /// work. Operator reconciliation, not a purchase: prepare will not
    /// reopen it.
    Reconciliation {
        /// The task whose admission is awaiting resolution.
        task_id: String,
    },
    /// The result was retired (retention elapsed, or the provider was
    /// asked to forget it). The launch ledger still bars a relaunch, so
    /// this id can never be bought again.
    Retired {
        /// The retired task id.
        task_id: String,
    },
    /// The service is at `max_in_flight`. Nothing was reserved; retry.
    Busy,
    /// Refused before anything was reserved: a malformed brief, an
    /// unknown service or stale revision, a bound exceeded, an
    /// application preflight refusal, or this id already naming
    /// different work.
    Rejected {
        /// Why, in the provider's own words.
        reason: String,
    },
}

impl PrepareReply {
    /// Canonical JSON bytes for the wire.
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }
    /// Decode from JSON bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, A2aError> {
        serde_json::from_slice(bytes).map_err(|e| A2aError::Decode(e.to_string()))
    }
}

/// Everything a submit needs, in one document: the exact provider, the
/// exact brief, the offer it was priced against, and the reservation it
/// belongs to.
///
/// Complete by design — nothing here is resolved from a hash later, so a
/// caller that crashed between paying and submitting can reload it and
/// submit without re-deriving anything.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreparedTask {
    /// The provider node the reservation lives on. Exact-provider: a
    /// purchase is never re-routed.
    pub provider_node: u64,
    /// The brief, byte-exact — changing it invalidates the commitment.
    pub brief: TaskBrief,
    /// [`A2aOffer::hash`] of the offer this was prepared against.
    pub offer_hash: String,
    /// The provider's reservation.
    pub reservation: AdmissionReservation,
}

/// Domain tag of the canonical task-commitment encoding.
const OBJECT_COMMITMENT_V1: &str = "net.a2a.commitment@1";
/// Domain tag of the canonical offer encoding.
const OBJECT_OFFER_V1: &str = "net.a2a.offer@1";
/// Domain tag of the canonical purchase encoding.
const OBJECT_PURCHASE_V1: &str = "net.a2a.purchase@1";

/// The canonical encoding hashed by [`task_commitment`]. Serialized as
/// JSON with fields in this declaration order; arrays are JSON arrays and
/// strings are JSON strings, so framing is unambiguous by construction —
/// no separator can be smuggled inside a value to make two different
/// briefs encode alike. The `object` tag domain-separates it from the
/// offer and purchase encodings.
///
/// Fixed for `@1`: any change to the field set or their order mints `@2`.
#[derive(Serialize)]
struct TaskCommitmentV1<'a> {
    object: &'static str,
    offer_hash: &'a str,
    service_id: &'a str,
    revision: &'a str,
    task_id: &'a str,
    prompt: &'a str,
    context_refs: &'a [String],
    tags: &'a [String],
}

/// The canonical encoding hashed by [`A2aOffer::hash`]. Every term that
/// bounds how long the purchased admission stays usable is committed.
#[derive(Serialize)]
struct OfferV1<'a> {
    object: &'static str,
    service_id: &'a str,
    revision: &'a str,
    pricing_terms: Option<&'a str>,
    bounds: &'a A2aBounds,
    reservation_ttl_secs: u64,
    reservation_retention_secs: u64,
    retention_secs: u64,
}

/// The canonical encoding hashed by [`purchase_hash`].
#[derive(Serialize)]
struct PurchaseV1<'a> {
    object: &'static str,
    admission_id: &'a str,
    commitment: &'a str,
}

/// blake3 hex over the canonical JSON encoding of `value`.
///
/// Streams straight into the hasher rather than building a `Vec` first: no
/// intermediate allocation, and a serializer error (impossible for these
/// plain structs — no non-string map keys, no floats) still leaves the
/// already-written prefix hashed, so no input can collapse onto the
/// empty-input hash.
fn hash_canonical<T: Serialize>(value: &T) -> String {
    let mut hasher = blake3::Hasher::new();
    let _ = serde_json::to_writer(&mut hasher, value);
    hasher.finalize().to_hex().to_string()
}

/// The commitment to exactly this work under exactly this offer: blake3
/// hex over the typed `net.a2a.commitment@1` encoding of the offer hash,
/// the service coordinates, and the brief's id, prompt, context refs and
/// tags.
///
/// `service_id` and `revision` come from the *offer*, never from the
/// brief's optional copies: the commitment says what the provider admitted
/// it to, not what the requester claimed.
pub fn task_commitment(offer: &A2aOffer, brief: &TaskBrief) -> String {
    hash_canonical(&TaskCommitmentV1 {
        object: OBJECT_COMMITMENT_V1,
        offer_hash: &offer.hash(),
        service_id: &offer.service_id,
        revision: &offer.revision,
        task_id: &brief.task_id,
        prompt: &brief.prompt,
        context_refs: &brief.context_refs,
        tags: &brief.tags,
    })
}

/// The purchase hash: blake3 hex over the typed `net.a2a.purchase@1`
/// encoding of a provider-minted `admission_id` and a [`task_commitment`].
///
/// This is what a quote commits to as its input hash. Because
/// `admission_id` is minted per `(owner, task id)` reservation, the same
/// payment presented under another owner — or against a later reservation
/// of the same task — computes a different expected hash at the provider
/// and is refused before any idempotency arm can hand back an admission.
pub fn purchase_hash(admission_id: &str, commitment: &str) -> String {
    hash_canonical(&PurchaseV1 {
        object: OBJECT_PURCHASE_V1,
        admission_id,
        commitment,
    })
}

/// An A2A protocol error.
#[derive(Debug, thiserror::Error)]
pub enum A2aError {
    /// A wire message could not be decoded.
    #[error("a2a decode error: {0}")]
    Decode(String),
    /// The referenced task is not known to this executor.
    #[error("unknown task: {0}")]
    UnknownTask(String),
}

/// A cancellation signal handed to a running [`TaskExecutor`]. A `cancel()` from
/// the requester trips it; a cooperative executor selects on
/// [`cancelled`](Self::cancelled) (or polls [`is_cancelled`](Self::is_cancelled))
/// and returns promptly — so a cancel demonstrably stops the remote work. A
/// non-cooperative executor's future is dropped by the registry's `select!`,
/// which also stops it.
#[derive(Clone)]
pub struct CancelToken {
    tx: Arc<watch::Sender<bool>>,
}

impl CancelToken {
    fn new() -> Self {
        let (tx, _rx) = watch::channel(false);
        Self { tx: Arc::new(tx) }
    }

    /// Trip the token — request cancellation.
    pub fn cancel(&self) {
        // `send_replace` (not `send`) updates the value + notifies receivers
        // *unconditionally* — `send` fails and leaves the value unchanged when
        // there are no receivers yet (the executor subscribes lazily inside
        // `cancelled()`), which would drop the cancellation on the floor.
        self.tx.send_replace(true);
    }

    /// Whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        *self.tx.borrow()
    }

    /// Resolve once cancellation is requested. Race-free: a `watch` receiver
    /// observes the latest value, so a cancel that races the await is not lost.
    pub async fn cancelled(&self) {
        let mut rx = self.tx.subscribe();
        if *rx.borrow() {
            return;
        }
        while rx.changed().await.is_ok() {
            if *rx.borrow() {
                return;
            }
        }
    }
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

/// The host agent's task runner. [`run`](Self::run) executes `brief` and returns
/// an **artifact ref** (a Datafort/blob ref — results are promoted home
/// explicitly, never inlined past a size threshold). A cooperative executor
/// watches `cancel` and returns promptly when it trips.
#[async_trait::async_trait]
pub trait TaskExecutor: Send + Sync {
    /// Run the task, returning the result's artifact ref, or an error string.
    async fn run(&self, brief: TaskBrief, cancel: CancelToken) -> Result<String, String>;
}

/// How long a terminal record outlives its last state change before
/// [`TaskRegistry::reserve`]'s housekeeping evicts it: long enough for a
/// requester to poll the outcome (and retry a few times), short enough that a
/// long-lived executor's table doesn't grow without bound. `forget()` remains
/// the immediate path once the result is retrieved.
///
/// The default, not the law: [`AdmissionTicket::with_retention`] overrides it
/// per entry (a catalog-driven service publishes its own `retention_secs`).
pub const TERMINAL_RECORD_TTL_SECS: u64 = 60 * 60;

struct Entry {
    brief: TaskBrief,
    state: TaskState,
    updated_at: u64,
    cancel: CancelToken,
    /// `Some` while this entry is a reservation no ticket has resolved yet:
    /// the verdict channel every concurrent identical admission waits on.
    /// Taken at launch (the task id is published to the waiters); the whole
    /// entry is removed on release (the refusal is published instead).
    reservation: Option<watch::Sender<ReservationVerdict>>,
    /// Per-entry terminal-record retention, overriding
    /// [`TERMINAL_RECORD_TTL_SECS`]. `None` uses the global default.
    retention_secs: Option<u64>,
}

/// How long `e`'s terminal record is kept: its own override, else `default`.
fn retention_of(e: &Entry, default: u64) -> u64 {
    e.retention_secs.unwrap_or(default)
}

/// Who submitted a task.
///
/// A task id is a **name, not a bearer capability**: it is
/// client-generated, it travels through logs, dashboards and polling
/// loops as an ordinary identifier, and [`TaskRecord`] carries the
/// complete prompt and context refs. Learning one must not confer the
/// right to read or cancel the work it names.
///
/// Registry entries are therefore keyed by `(owner, task_id)`, not by
/// `task_id` alone. Keying rather than storing-and-checking also means
/// two agents that independently pick the same id — `"task-1"` is not
/// far-fetched for client-generated ids — never collide, so one peer
/// cannot squat obvious ids to deny another.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TaskOwner {
    /// Submitted in-process, with no mesh peer involved.
    Local,
    /// Submitted by an authenticated mesh peer, identified by the
    /// AEAD-authenticated session peer that delivered the request —
    /// never by anything the request body claimed.
    ///
    /// The deliverer, not an end-to-end origin: under nRPC relaying
    /// the relay owns everything it forwards. See the module docs on
    /// [`crate::mesh_a2a`].
    Peer(u64),
    /// Submitted by a caller whose end-to-end identity was verified by
    /// organization admission — the entity the admission proof names, not
    /// whichever peer delivered the request. The principal a paid
    /// admission can be matched against.
    Entity([u8; 32]),
}

/// Why [`TaskRegistry::submit`] refused a brief.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SubmitRejection {
    /// This owner already has a *different* brief under this task id.
    ///
    /// Silently returning the existing task would be worse than
    /// refusing: the submitter would poll a task that is not the one
    /// they described and read someone else's — or their own earlier —
    /// result as the answer to this request.
    #[error(
        "task id {task_id:?} already names a different brief for this submitter; \
         re-submitting an id is idempotent only for an identical brief"
    )]
    IdReusedForDifferentBrief {
        /// The contested id.
        task_id: String,
    },
    /// The brief names a service this executor does not serve.
    #[error("no service named {service:?} is served here")]
    UnknownService {
        /// The service the brief named.
        service: String,
    },
    /// The brief was prepared against an offer revision that is no longer
    /// current. Refused before anything is reserved or charged — admitting
    /// it would run work under terms neither side currently publishes.
    #[error("service revision {got:?} is retired; this executor serves {expected:?}")]
    StaleRevision {
        /// The revision this executor serves now.
        expected: String,
        /// The revision the brief carried.
        got: String,
    },
    /// The brief exceeds one of the offered [`A2aBounds`].
    #[error("brief exceeds the offered limit for {field}: at most {limit}")]
    BoundsExceeded {
        /// Which bound was exceeded (`prompt_bytes`, `context_refs`,
        /// `tags`, `tag_bytes`).
        field: String,
        /// The published limit.
        limit: u64,
    },
    /// A paid submission arrived with no admission reservation behind it:
    /// the caller never prepared, or the reservation was retained out.
    /// Never an inference that no money moved — the provider has no
    /// record, so it knows nothing either way.
    #[error("no admission reservation exists for this task; prepare before submitting")]
    NoReservation,
    /// The task ran once and its result has since been retired. Refused
    /// rather than re-run: the launch ledger outlives the result, so a
    /// retry after retention can never buy a second execution.
    #[error("task {task_id:?} already ran and its result has been retired")]
    Retired {
        /// The retired id.
        task_id: String,
    },
    /// The service is at its in-flight capacity. Retryable, and refused
    /// before any payment is redeemed.
    #[error("this service is at capacity; retry once an in-flight task finishes")]
    Busy,
}

/// Notified once when a registry entry reaches a terminal [`TaskState`]:
/// `(owner, task id, terminal state)`. Installed with
/// [`TaskRegistry::with_terminal_hook`].
///
/// Exactly the three values a durable admission record needs to write its
/// terminal row — the id and state are borrowed, so the ordinary
/// no-hook path allocates nothing.
///
/// **The hook must not block.** It runs on whatever task recorded the
/// terminal state (a spawned executor task, or the thread unwinding a
/// panicking one) with the registry lock already released — so it may call
/// back into the registry — but a slow hook stalls that task. Anything
/// that can take real time belongs behind a channel.
pub type TerminalHook = Arc<dyn Fn(TaskOwner, &str, &TaskState) + Send + Sync>;

/// Published to waiters when a reservation resolves without launching.
const RESERVATION_RELEASED: &str = "the reservation was released without launching";

/// Why a reservation resolved without launching — **in the shape the
/// decider answered with**, so a waiter can reproduce it exactly.
///
/// A concurrent duplicate parks on [`Admission::Pending`] instead of
/// deciding (and paying for) the same task twice, which means the
/// decider's verdict is the waiter's verdict. A refusal flattened to
/// prose loses the one thing the waiter has to act on: whether the
/// refusal is retryable. A deciding submit that fails a journal write
/// answers a retryable payment schematic; a waiter handed a bare string
/// cannot tell that from a permanent refusal, and treating "retry the
/// same proof" as "this purchase can never execute" is how a paid task
/// is abandoned.
#[derive(Debug, Clone, PartialEq)]
pub enum ReservationRefusal {
    /// A plain in-body rejection: the reason a [`TaskAck`] carries.
    Rejected(String),
    /// A payment or admission refusal: the human message plus the
    /// structured [`FailureSchematic`] the decider authored, which is
    /// what says whether the refusal is retryable, what it cost, and
    /// whether a re-quote is safe.
    Payment {
        /// The message the decider answered with.
        message: String,
        /// The decider's schematic, verbatim. Boxed: a schematic is far
        /// larger than the string arm, and this enum travels through a
        /// watch channel by value.
        schematic: Box<FailureSchematic>,
    },
}

impl ReservationRefusal {
    /// The human-readable reason, for a consumer that has no use for the
    /// structured half.
    pub fn message(&self) -> &str {
        match self {
            ReservationRefusal::Rejected(reason) => reason,
            ReservationRefusal::Payment { message, .. } => message,
        }
    }

    /// The structured refusal, when the decider authored one.
    pub fn schematic(&self) -> Option<&FailureSchematic> {
        match self {
            ReservationRefusal::Rejected(_) => None,
            ReservationRefusal::Payment { schematic, .. } => Some(schematic),
        }
    }
}

/// What a reservation resolved to: `None` while it is still being
/// decided, then the launched task id or the decider's complete refusal.
pub type ReservationVerdict = Option<Result<String, ReservationRefusal>>;

/// What [`TaskRegistry::reserve`] found for an `(owner, task id)`.
///
/// The three arms are the admission split: work that is already under way,
/// an identical admission somebody else is still deciding, and a fresh
/// reservation this caller now owns.
pub enum Admission {
    /// This exact brief is already launched — answer with this id. Nothing
    /// was reserved and no executor was spawned.
    Existing(String),
    /// An identical brief is reserved but not yet launched: a concurrent
    /// admission is deciding it (redeeming a payment, writing a journal
    /// record). The receiver resolves to `Some(Ok(task id))` if that
    /// admission launches and `Some(Err(refusal))` if it is refused or
    /// released — [`await_admission_verdict`] does the waiting.
    ///
    /// Converging here rather than reserving a second time is what keeps
    /// two racing retransmits to one payment and one run.
    Pending(watch::Receiver<ReservationVerdict>),
    /// A fresh reservation: the entry exists in [`TaskState::Requested`]
    /// and this ticket is the only thing that can launch or release it.
    Reserved(AdmissionTicket),
}

/// Await an [`Admission::Pending`] reservation's verdict: the launched task
/// id, or the decider's complete refusal.
///
/// Never hangs on a lost decider: a reservation whose entry disappears
/// without a verdict (a `forget` racing the decision) resolves as a
/// released reservation, because the sender is gone and no verdict can
/// ever arrive.
pub async fn await_admission_verdict(
    mut rx: watch::Receiver<ReservationVerdict>,
) -> Result<String, ReservationRefusal> {
    loop {
        if let Some(verdict) = rx.borrow_and_update().clone() {
            return verdict;
        }
        if rx.changed().await.is_err() {
            return rx.borrow().clone().unwrap_or_else(|| {
                Err(ReservationRefusal::Rejected(
                    RESERVATION_RELEASED.to_string(),
                ))
            });
        }
    }
}

/// The exclusive right to launch — or release — one reserved
/// `(owner, task id)`.
///
/// The ticket *is* the window between admission and work: while it is
/// held, the entry sits in [`TaskState::Requested`], every identical
/// submission parks on [`Admission::Pending`] instead of racing, and a
/// different brief under the same id is refused. A paid serving path
/// spends that window redeeming the payment and claiming the launch
/// durably, so nothing runs before the money is accounted for and nothing
/// runs twice.
///
/// **Dropping an unconsumed ticket releases the reservation.** A handler
/// that returns early — `?` on a decode error, an in-body refusal, a panic
/// — never strands a `Requested` entry that would then refuse every later
/// submission of the same id.
pub struct AdmissionTicket {
    inner: Arc<Mutex<HashMap<(TaskOwner, String), Entry>>>,
    hook: Option<TerminalHook>,
    key: (TaskOwner, String),
    /// Boxed: a `TaskBrief` is the bulk of this struct, and
    /// [`Admission`] is returned by value on every admission — an inline
    /// copy would make the common `Existing` / `Pending` arms carry a
    /// reservation's worth of stack for nothing.
    brief: Box<TaskBrief>,
    cancel: CancelToken,
    /// Cleared by `launch` / `release`; a still-armed ticket releases on drop.
    armed: bool,
}

impl AdmissionTicket {
    /// Who this reservation belongs to.
    pub fn owner(&self) -> TaskOwner {
        self.key.0
    }

    /// The reserved task id.
    pub fn task_id(&self) -> &str {
        &self.key.1
    }

    /// The brief this reservation admits.
    pub fn brief(&self) -> &TaskBrief {
        self.brief.as_ref()
    }

    /// Keep this task's terminal record for `secs` instead of the global
    /// [`TERMINAL_RECORD_TTL_SECS`] (builder-style) — a catalog-driven
    /// service sets it from the offer's published `retention_secs`, so what
    /// the caller paid for is what the registry honors.
    #[must_use]
    pub fn with_retention(self, secs: u64) -> Self {
        if let Some(e) = self.inner.lock().get_mut(&self.key) {
            e.retention_secs = Some(secs);
        }
        self
    }

    /// Consume the ticket: spawn `executor` on the reserved entry and
    /// return the task id.
    ///
    /// The spawned task drives `running → completed{ref} | failed |
    /// cancelled`, racing the executor against the cancel token so a cancel
    /// stops the work (a cooperative executor also watches the token).
    /// Waiters parked on [`Admission::Pending`] are handed this id before
    /// the entry leaves [`TaskState::Requested`], so a racing identical
    /// submission converges on this run instead of seeing a refusal.
    ///
    /// Requires a tokio runtime context (the serve handler / a
    /// `#[tokio::test]` provides it).
    pub fn launch(mut self, executor: Arc<dyn TaskExecutor>) -> String {
        self.armed = false;
        let inner = Arc::clone(&self.inner);
        let hook = self.hook.clone();
        let id_run = self.key.clone();
        let id = self.key.1.clone();
        let brief = self.brief.as_ref().clone();
        let cancel = self.cancel.clone();
        {
            let mut map = inner.lock();
            if let Some(e) = map.get_mut(&id_run) {
                if let Some(tx) = e.reservation.take() {
                    tx.send_replace(Some(Ok(id.clone())));
                }
                if !e.state.is_terminal() {
                    e.state = TaskState::Accepted;
                    e.updated_at = now_secs();
                }
            }
        }
        tokio::spawn(async move {
            set_state(&inner, hook.as_ref(), &id_run, TaskState::Running);
            // Panic containment: an executor panic unwinds this task and
            // would otherwise skip the final set_state, stranding the entry in
            // `Running` forever (status/wait_terminal poll indefinitely, the
            // entry leaks). The guard's Drop records a terminal state on
            // unwind; the normal path disarms it before recording its own.
            let mut panic_guard = PanicGuard {
                inner: Arc::clone(&inner),
                hook: hook.clone(),
                id: id_run.clone(),
                cancel: cancel.clone(),
                armed: true,
            };
            // Cooperative cancellation: the executor watches the token and
            // returns promptly when it trips (the Hermes agent loop is wired to
            // its interrupt machinery). The select! is the backstop for a
            // NON-cooperative executor — one that ignores the token — whose
            // future is dropped when the token trips, so a cancel stops the
            // work either way ([`CancelToken`]'s documented guarantee).
            // `biased` polls the executor first so a result that's already in
            // wins over a simultaneous cancel. Whatever it returns, a requested
            // cancel makes the outcome `Cancelled` — the requester asked to
            // stop, so a partial result isn't promoted.
            let r = tokio::select! {
                biased;
                r = executor.run(brief, cancel.clone()) => r,
                _ = cancel.cancelled() => Err("cancelled".to_string()),
            };
            panic_guard.armed = false;
            let final_state = if cancel.is_cancelled() {
                TaskState::Cancelled
            } else {
                match r {
                    Ok(result_ref) => TaskState::Completed { result_ref },
                    Err(error) => TaskState::Failed { error },
                }
            };
            set_state(&inner, hook.as_ref(), &id_run, final_state);
        });
        id
    }

    /// Consume the ticket without launching: delete the reserved entry and
    /// tell every parked waiter the reservation is gone. The id is free
    /// again — a later submission of the same brief reserves afresh.
    pub fn release(mut self) {
        self.armed = false;
        release_reservation(&self.inner, &self.key, None);
    }

    /// Consume the ticket and publish `refusal` — **the decider's own
    /// answer** — to every waiter, instead of the generic "released"
    /// note.
    ///
    /// This is what makes concurrent submitters converge: the waiter
    /// receives the verdict the decider produced, retryability and all,
    /// rather than a summary of it. The entry is deleted, so the id is
    /// free for a retry.
    pub fn refuse(mut self, refusal: ReservationRefusal) {
        self.armed = false;
        release_reservation(&self.inner, &self.key, Some(refusal));
    }

    /// Consume the ticket and publish `task_id` to every waiter without
    /// launching anything: the work this submission names is already
    /// under way (a launched or recorded admission the store answered
    /// from), so the waiters converge on it exactly as they would on a
    /// launch.
    pub fn resolve_existing(mut self, task_id: String) {
        self.armed = false;
        let mut map = self.inner.lock();
        let reserved = map.get(&self.key).is_some_and(|e| e.reservation.is_some());
        if !reserved {
            return;
        }
        if let Some(tx) = map.remove(&self.key).and_then(|e| e.reservation) {
            tx.send_replace(Some(Ok(task_id)));
        }
    }
}

impl Drop for AdmissionTicket {
    fn drop(&mut self) {
        if self.armed {
            release_reservation(&self.inner, &self.key, None);
        }
    }
}

/// Delete an unlaunched reservation and publish the refusal to its waiters:
/// `refusal` when the decider authored one, else the generic released note.
/// Checks the entry is still a reservation first: a launched entry is a
/// running task, never something a stale ticket may delete.
fn release_reservation(
    inner: &Arc<Mutex<HashMap<(TaskOwner, String), Entry>>>,
    key: &(TaskOwner, String),
    refusal: Option<ReservationRefusal>,
) {
    let mut map = inner.lock();
    let reserved = map.get(key).is_some_and(|e| e.reservation.is_some());
    if !reserved {
        return;
    }
    if let Some(tx) = map.remove(key).and_then(|e| e.reservation) {
        tx.send_replace(Some(Err(refusal.unwrap_or_else(|| {
            ReservationRefusal::Rejected(RESERVATION_RELEASED.to_string())
        }))));
    }
}

/// The executor side's live task table: spawns [`TaskExecutor`]s, tracks their
/// [`TaskState`], and routes cancellation. Cheap to clone (shared inner).
#[derive(Clone, Default)]
pub struct TaskRegistry {
    inner: Arc<Mutex<HashMap<(TaskOwner, String), Entry>>>,
    hook: Option<TerminalHook>,
}

impl TaskRegistry {
    /// A fresh, empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Install a [`TerminalHook`] (builder-style): called once, with the
    /// owner, task id and final state, when an entry reaches a terminal
    /// [`TaskState`] — from the executor's own completion, from a cancel,
    /// and from the panic guard's unwind path alike, because all three
    /// record through one writer.
    ///
    /// This is how a durable admission record learns an outcome it did not
    /// start: the registry stays free of storage, the hook writes the
    /// terminal row. It must not block — see [`TerminalHook`].
    #[must_use]
    pub fn with_terminal_hook(mut self, hook: TerminalHook) -> Self {
        self.hook = Some(hook);
        self
    }

    /// Claim `(owner, brief.task_id)` for admission, **without** starting
    /// any work: the caller decides whether the task may run and then
    /// either [`launches`](AdmissionTicket::launch) or
    /// [`releases`](AdmissionTicket::release) the returned ticket.
    ///
    /// Separating the claim from the launch is what makes a paid — or
    /// otherwise gated — admission possible: the reservation blocks a
    /// second admission of the same id while the payment is redeemed and
    /// the launch is recorded durably, and nothing has run if either step
    /// refuses.
    ///
    /// Dispatch on what this owner already has under the id:
    ///
    /// - a **launched** entry with an identical brief →
    ///   [`Admission::Existing`]: the retransmit is answered with the
    ///   original id. Re-inserting would orphan the first entry's cancel
    ///   token (making the original run uncancellable) and race two
    ///   executors on the final state.
    /// - a **reserved** entry with an identical brief →
    ///   [`Admission::Pending`]: another admission is mid-decision, so this
    ///   one waits for its verdict rather than deciding the same task
    ///   twice.
    /// - a **different** brief →
    ///   [`SubmitRejection::IdReusedForDifferentBrief`], never a silent
    ///   hand-back of the earlier task.
    /// - **nothing** → the entry is inserted in [`TaskState::Requested`]
    ///   and [`Admission::Reserved`] carries the only ticket for it.
    ///
    /// Two different owners may use the same task id without interfering:
    /// entries are keyed by the pair.
    ///
    /// Also evicts terminal records past their retention (per-entry
    /// override, else [`TERMINAL_RECORD_TTL_SECS`]; see
    /// [`Self::evict_terminal`]) so a long-lived executor's table doesn't
    /// grow without bound; [`Self::forget`] remains the immediate path.
    pub fn reserve(
        &self,
        owner: TaskOwner,
        brief: TaskBrief,
    ) -> Result<Admission, SubmitRejection> {
        let id = brief.task_id.clone();
        let key = (owner, id.clone());
        let mut map = self.inner.lock();
        let now = now_secs();
        map.retain(|_, e| {
            !e.state.is_terminal()
                || now.saturating_sub(e.updated_at) <= retention_of(e, TERMINAL_RECORD_TTL_SECS)
        });
        if let Some(existing) = map.get(&key) {
            if existing.brief != brief {
                return Err(SubmitRejection::IdReusedForDifferentBrief { task_id: id });
            }
            return Ok(match &existing.reservation {
                Some(tx) => Admission::Pending(tx.subscribe()),
                None => Admission::Existing(id),
            });
        }
        let cancel = CancelToken::new();
        let (verdict, _rx) = watch::channel(None);
        map.insert(
            key.clone(),
            Entry {
                brief: brief.clone(),
                state: TaskState::Requested,
                updated_at: now,
                cancel: cancel.clone(),
                reservation: Some(verdict),
                retention_secs: None,
            },
        );
        drop(map);
        Ok(Admission::Reserved(AdmissionTicket {
            inner: Arc::clone(&self.inner),
            hook: self.hook.clone(),
            key,
            brief: Box::new(brief),
            cancel,
            armed: true,
        }))
    }

    /// Accept `brief`, spawn `executor` to run it, and return the task id —
    /// [`reserve`](Self::reserve) followed immediately by
    /// [`launch`](AdmissionTicket::launch). The free path: no gate, no
    /// durable record, nothing between admission and work.
    ///
    /// Idempotent per `(owner, task id)` for an **identical** brief, and
    /// [`SubmitRejection::IdReusedForDifferentBrief`] for a different one;
    /// see [`reserve`](Self::reserve) for the full dispatch.
    ///
    /// A [`Admission::Pending`] verdict returns the id **without awaiting
    /// it**, and that is deliberate: this verb is synchronous, the entry
    /// demonstrably exists, and the only thing that can be mid-decision on
    /// this path is another `submit` of the identical brief — which is
    /// about to launch the very id being returned. (A gated serving path
    /// awaits the verdict, because there a reservation can also end in a
    /// refusal.)
    ///
    /// Requires a tokio runtime context (the serve handler / a `#[tokio::test]`
    /// provides it).
    pub fn submit(
        &self,
        owner: TaskOwner,
        brief: TaskBrief,
        executor: Arc<dyn TaskExecutor>,
    ) -> Result<String, SubmitRejection> {
        let id = brief.task_id.clone();
        match self.reserve(owner, brief)? {
            Admission::Existing(existing) => Ok(existing),
            Admission::Pending(_) => Ok(id),
            Admission::Reserved(ticket) => Ok(ticket.launch(executor)),
        }
    }

    /// The current state of `owner`'s `task_id`, if known.
    ///
    /// A task belonging to a *different* owner reads as unknown. That is
    /// deliberate: distinguishing "not yours" from "no such task" would
    /// make this an existence oracle for a caller who is not entitled to
    /// know the task exists.
    pub fn status(&self, owner: TaskOwner, task_id: &str) -> Option<TaskState> {
        self.inner
            .lock()
            .get(&(owner, task_id.to_string()))
            .map(|e| e.state.clone())
    }

    /// The full record of `owner`'s `task_id`, if known. Another owner's
    /// task reads as unknown — see [`Self::status`].
    pub fn record(&self, owner: TaskOwner, task_id: &str) -> Option<TaskRecord> {
        self.inner
            .lock()
            .get(&(owner, task_id.to_string()))
            .map(|e| TaskRecord {
                brief: e.brief.clone(),
                state: e.state.clone(),
                updated_at: e.updated_at,
            })
    }

    /// Request cancellation of `owner`'s `task_id`. Returns `true` if it
    /// existed and was still in flight (a terminal, unknown, or
    /// differently-owned task returns `false`). Trips the token; the
    /// spawned task transitions to `Cancelled` once the executor stops.
    pub fn cancel(&self, owner: TaskOwner, task_id: &str) -> bool {
        let map = self.inner.lock();
        match map.get(&(owner, task_id.to_string())) {
            Some(e) if !e.state.is_terminal() => {
                e.cancel.cancel();
                true
            }
            _ => false,
        }
    }

    /// Every recorded task with the owner that submitted it,
    /// newest-updated first.
    ///
    /// **Local only.** No nRPC service exposes this — [`Mesh::serve_a2a`]
    /// serves submit, status and cancel, all of which are owner-scoped.
    /// A caller holding the registry is the process hosting it, and
    /// giving the executor's own operator a full view is the point.
    ///
    /// The owner rides alongside rather than inside [`TaskRecord`]:
    /// that type is the status reply's wire shape, spoken by the Node
    /// and Python bindings and by peers on older builds, and a
    /// cross-version break is not worth an operator convenience.
    ///
    /// Use [`Self::list_for`] for one submitter's tasks.
    ///
    /// [`Mesh::serve_a2a`]: crate::mesh::Mesh::serve_a2a
    pub fn list(&self) -> Vec<(TaskOwner, TaskRecord)> {
        let mut recs: Vec<(TaskOwner, TaskRecord)> = self
            .inner
            .lock()
            .iter()
            .map(|((owner, _id), e)| {
                (
                    *owner,
                    TaskRecord {
                        brief: e.brief.clone(),
                        state: e.state.clone(),
                        updated_at: e.updated_at,
                    },
                )
            })
            .collect();
        recs.sort_by_key(|(_, r)| std::cmp::Reverse(r.updated_at));
        recs
    }

    /// One owner's tasks, newest-updated first.
    pub fn list_for(&self, owner: TaskOwner) -> Vec<TaskRecord> {
        let mut recs: Vec<TaskRecord> = self
            .inner
            .lock()
            .iter()
            .filter(|((o, _), _)| *o == owner)
            .map(|(_, e)| TaskRecord {
                brief: e.brief.clone(),
                state: e.state.clone(),
                updated_at: e.updated_at,
            })
            .collect();
        recs.sort_by_key(|r| std::cmp::Reverse(r.updated_at));
        recs
    }

    /// Drop `owner`'s record for `task_id` (housekeeping once the
    /// requester has the result). Returns whether a record existed.
    pub fn forget(&self, owner: TaskOwner, task_id: &str) -> bool {
        self.inner
            .lock()
            .remove(&(owner, task_id.to_string()))
            .is_some()
    }

    /// Evict terminal records whose last state change is more than their
    /// retention before `now`, returning how many were dropped. An entry
    /// with a [`AdmissionTicket::with_retention`] override uses it;
    /// everything else uses `ttl_secs`. In-flight tasks (including
    /// unlaunched reservations) are never touched.
    /// [`Self::reserve`] runs this automatically with
    /// [`TERMINAL_RECORD_TTL_SECS`]; exposed for callers that want a tighter
    /// housekeeping schedule than "on the next submission".
    pub fn evict_terminal(&self, ttl_secs: u64, now: u64) -> usize {
        let mut map = self.inner.lock();
        let before = map.len();
        map.retain(|_, e| {
            !e.state.is_terminal() || now.saturating_sub(e.updated_at) <= retention_of(e, ttl_secs)
        });
        before - map.len()
    }
}

/// Drop-guard armed while [`AdmissionTicket::launch`]'s spawned task awaits
/// the executor: a panicking executor unwinds the task, and without this the
/// final `set_state` never runs — the entry is stranded non-terminal (`cancel`
/// returns `true` but nothing transitions, `status`/`wait_terminal` poll
/// `Running` forever) and leaks. On an armed drop the task is recorded
/// `Failed` — or `Cancelled` when the token was tripped (the requester asked
/// to stop; the panic is incidental). The normal completion path disarms it.
struct PanicGuard {
    inner: Arc<Mutex<HashMap<(TaskOwner, String), Entry>>>,
    hook: Option<TerminalHook>,
    id: (TaskOwner, String),
    cancel: CancelToken,
    armed: bool,
}

impl Drop for PanicGuard {
    fn drop(&mut self) {
        if self.armed {
            let state = if self.cancel.is_cancelled() {
                TaskState::Cancelled
            } else {
                TaskState::Failed {
                    error: "executor panicked".to_string(),
                }
            };
            set_state(&self.inner, self.hook.as_ref(), &self.id, state);
        }
    }
}

/// Set a task's state, never overwriting a terminal state (so a late `Running`
/// can't clobber a `Cancelled` recorded by a racing cancel).
///
/// The single writer, so it is also the single place a terminal state is
/// announced: `hook` fires exactly once per entry, for the transition that
/// actually landed, and **after** the map lock is released so the hook may
/// read or write the registry itself.
fn set_state(
    inner: &Arc<Mutex<HashMap<(TaskOwner, String), Entry>>>,
    hook: Option<&TerminalHook>,
    id: &(TaskOwner, String),
    state: TaskState,
) {
    let mut map = inner.lock();
    let landed = match map.get_mut(id) {
        Some(e) if !e.state.is_terminal() => {
            e.state = state;
            e.updated_at = now_secs();
            e.state.is_terminal().then(|| e.state.clone())
        }
        _ => None,
    };
    drop(map);
    if let (Some(hook), Some(state)) = (hook, landed) {
        hook(id.0, &id.1, &state);
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A random 16-byte hex id. A `getrandom` failure aborts the process (mirroring
/// the identity layer): these helpers are reachable from FFI and a predictable
/// task id is worse than a crash.
///
/// `pub(crate)` because the configured serving path mints admission ids from
/// the same primitive: an `admission_id` a caller could predict would let it
/// compute another reservation's purchase hash before that reservation exists.
pub(crate) fn random_id() -> String {
    let mut b = [0u8; 16];
    if let Err(e) = getrandom::fill(&mut b) {
        eprintln!("FATAL: A2A task-id getrandom failure ({e:?}); aborting");
        std::process::abort();
    }
    let mut s = String::with_capacity(32);
    use std::fmt::Write as _;
    for x in b {
        let _ = write!(s, "{x:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    /// An executor that returns a fixed result ref after an optional pause,
    /// recording whether it saw the cancel.
    struct MockExecutor {
        result: String,
        saw_cancel: Arc<AtomicBool>,
        /// If true, wait for cancellation instead of completing.
        wait_for_cancel: bool,
    }

    #[async_trait::async_trait]
    impl TaskExecutor for MockExecutor {
        async fn run(&self, _brief: TaskBrief, cancel: CancelToken) -> Result<String, String> {
            if self.wait_for_cancel {
                cancel.cancelled().await;
                self.saw_cancel.store(true, Ordering::SeqCst);
                return Err("cancelled".to_string());
            }
            Ok(self.result.clone())
        }
    }

    async fn wait_terminal(reg: &TaskRegistry, id: &str) -> TaskState {
        for _ in 0..200 {
            if let Some(s) = reg.status(TaskOwner::Local, id) {
                if s.is_terminal() {
                    return s;
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("task {id} did not reach a terminal state");
    }

    #[tokio::test]
    async fn a_task_runs_to_completion_with_a_result_ref() {
        let reg = TaskRegistry::new();
        let brief = TaskBrief::new("summarize the logs");
        let id = reg
            .submit(
                TaskOwner::Local,
                brief,
                Arc::new(MockExecutor {
                    result: "blob://result-123".to_string(),
                    saw_cancel: Arc::new(AtomicBool::new(false)),
                    wait_for_cancel: false,
                }),
            )
            .expect("a fresh brief is accepted");
        let state = wait_terminal(&reg, &id).await;
        assert_eq!(
            state,
            TaskState::Completed {
                result_ref: "blob://result-123".to_string()
            }
        );
    }

    #[tokio::test]
    async fn cancel_stops_a_running_task() {
        let reg = TaskRegistry::new();
        let saw = Arc::new(AtomicBool::new(false));
        let id = reg
            .submit(
                TaskOwner::Local,
                TaskBrief::new("grind forever"),
                Arc::new(MockExecutor {
                    result: String::new(),
                    saw_cancel: Arc::clone(&saw),
                    wait_for_cancel: true,
                }),
            )
            .expect("submit");
        // Let it reach Running, then cancel.
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            reg.cancel(TaskOwner::Local, &id),
            "an in-flight task cancels"
        );
        let state = wait_terminal(&reg, &id).await;
        assert_eq!(state, TaskState::Cancelled);
        assert!(
            saw.load(Ordering::SeqCst),
            "the executor observed the cancel"
        );
        // Cancelling a terminal task is a no-op.
        assert!(!reg.cancel(TaskOwner::Local, &id));
    }

    /// An executor that IGNORES the cancel token — it just sleeps. Records
    /// whether its future was dropped (`dropped`, via a guard) and whether it
    /// ever ran to completion (`completed`).
    struct StubbornExecutor {
        dropped: Arc<AtomicBool>,
        completed: Arc<AtomicBool>,
    }

    struct SetOnDrop(Arc<AtomicBool>);
    impl Drop for SetOnDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[async_trait::async_trait]
    impl TaskExecutor for StubbornExecutor {
        async fn run(&self, _brief: TaskBrief, _cancel: CancelToken) -> Result<String, String> {
            let _guard = SetOnDrop(Arc::clone(&self.dropped));
            tokio::time::sleep(Duration::from_secs(3600)).await;
            self.completed.store(true, Ordering::SeqCst);
            Ok("blob://too-late".to_string())
        }
    }

    #[tokio::test]
    async fn cancel_stops_a_non_cooperative_executor() {
        // The registry's select! must drop an executor that ignores the token —
        // the CancelToken doc's "a non-cooperative executor's future is dropped
        // by the registry's select!" guarantee.
        let reg = TaskRegistry::new();
        let dropped = Arc::new(AtomicBool::new(false));
        let completed = Arc::new(AtomicBool::new(false));
        let id = reg
            .submit(
                TaskOwner::Local,
                TaskBrief::new("ignore the token"),
                Arc::new(StubbornExecutor {
                    dropped: Arc::clone(&dropped),
                    completed: Arc::clone(&completed),
                }),
            )
            .expect("submit");
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(reg.cancel(TaskOwner::Local, &id));
        // Terminal promptly — not after the executor's hour-long sleep.
        let state = wait_terminal(&reg, &id).await;
        assert_eq!(state, TaskState::Cancelled);
        assert!(
            dropped.load(Ordering::SeqCst),
            "the non-cooperative executor's future was dropped"
        );
        assert!(!completed.load(Ordering::SeqCst));
    }

    /// An executor that counts how many times it was started, then waits for
    /// the cancel token.
    struct CountingExecutor {
        runs: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl TaskExecutor for CountingExecutor {
        async fn run(&self, _brief: TaskBrief, cancel: CancelToken) -> Result<String, String> {
            self.runs.fetch_add(1, Ordering::SeqCst);
            cancel.cancelled().await;
            Err("cancelled".to_string())
        }
    }

    #[tokio::test]
    async fn duplicate_submit_is_idempotent() {
        // An nRPC retransmit of an accepted brief must not spawn a second
        // executor or replace the entry (which would orphan the first run's
        // cancel token, leaving it uncancellable).
        let reg = TaskRegistry::new();
        let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let brief = TaskBrief::new("retransmitted job");
        let exec = Arc::new(CountingExecutor {
            runs: Arc::clone(&runs),
        });
        let id = reg
            .submit(TaskOwner::Local, brief.clone(), exec.clone())
            .expect("first submit");
        let id2 = reg
            .submit(TaskOwner::Local, brief, exec)
            .expect("identical re-submit is idempotent");
        assert_eq!(id, id2, "the retransmit acks the same id");

        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(runs.load(Ordering::SeqCst), 1, "exactly one executor ran");

        // The (single, original) run is still wired to the entry's token.
        assert!(reg.cancel(TaskOwner::Local, &id));
        let state = wait_terminal(&reg, &id).await;
        assert_eq!(state, TaskState::Cancelled);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn terminal_records_evict_after_ttl_running_ones_never() {
        let reg = TaskRegistry::new();
        let done = reg
            .submit(
                TaskOwner::Local,
                TaskBrief::new("quick"),
                Arc::new(MockExecutor {
                    result: "blob://r".to_string(),
                    saw_cancel: Arc::new(AtomicBool::new(false)),
                    wait_for_cancel: false,
                }),
            )
            .expect("submit");
        wait_terminal(&reg, &done).await;
        // Within the TTL the record survives housekeeping.
        assert_eq!(reg.evict_terminal(TERMINAL_RECORD_TTL_SECS, now_secs()), 0);
        assert!(reg.status(TaskOwner::Local, &done).is_some());

        let live = reg
            .submit(
                TaskOwner::Local,
                TaskBrief::new("still running"),
                Arc::new(MockExecutor {
                    result: String::new(),
                    saw_cancel: Arc::new(AtomicBool::new(false)),
                    wait_for_cancel: true,
                }),
            )
            .expect("submit");
        tokio::time::sleep(Duration::from_millis(20)).await;

        // Past the TTL the terminal record evicts; the in-flight one never.
        let future = now_secs() + TERMINAL_RECORD_TTL_SECS + 10;
        assert_eq!(reg.evict_terminal(TERMINAL_RECORD_TTL_SECS, future), 1);
        assert!(reg.status(TaskOwner::Local, &done).is_none());
        assert!(reg.status(TaskOwner::Local, &live).is_some());
        reg.cancel(TaskOwner::Local, &live);
    }

    /// An executor that panics mid-run.
    struct PanickingExecutor;

    #[async_trait::async_trait]
    impl TaskExecutor for PanickingExecutor {
        async fn run(&self, _brief: TaskBrief, _cancel: CancelToken) -> Result<String, String> {
            panic!("executor blew up");
        }
    }

    #[tokio::test]
    async fn a_panicking_executor_marks_the_task_failed() {
        // An executor panic must not strand the task in `Running` — the guard
        // records `Failed` so pollers terminate and the entry can be forgotten.
        let reg = TaskRegistry::new();
        let id = reg
            .submit(
                TaskOwner::Local,
                TaskBrief::new("kaboom"),
                Arc::new(PanickingExecutor),
            )
            .expect("submit");
        let state = wait_terminal(&reg, &id).await;
        assert_eq!(
            state,
            TaskState::Failed {
                error: "executor panicked".to_string()
            }
        );
        // Terminal: cancel is a no-op, the record can be forgotten.
        assert!(!reg.cancel(TaskOwner::Local, &id));
        assert!(reg.forget(TaskOwner::Local, &id));
    }

    #[tokio::test]
    async fn cancel_of_unknown_task_is_false() {
        let reg = TaskRegistry::new();
        assert!(!reg.cancel(TaskOwner::Local, "nope"));
        assert!(reg.status(TaskOwner::Local, "nope").is_none());
    }

    /// `list` was the one accessor left owner-blind after tasks became
    /// owner-keyed: it flattened every submitter's records into an
    /// undifferentiated `Vec<TaskRecord>`.
    ///
    /// Not a disclosure — nothing serves it remotely — but it made the
    /// local operator view useless for the question the ownership work
    /// exists to answer, and it silently merged two submitters' tasks
    /// that happen to share an id, which is the collision
    /// `(owner, task_id)` keying was introduced to prevent.
    #[tokio::test]
    async fn list_attributes_tasks_and_list_for_scopes_them() {
        let reg = TaskRegistry::new();
        let alice = TaskOwner::Peer(0xA11CE);
        let bob = TaskOwner::Peer(0xB0B);
        let exec = || {
            Arc::new(MockExecutor {
                result: "blob://r".to_string(),
                saw_cancel: Arc::new(AtomicBool::new(false)),
                wait_for_cancel: true,
            })
        };

        // The same client-chosen id from two submitters — "task-1" is
        // not a far-fetched collision — plus a second task for Alice.
        let mut shared = TaskBrief::new("alice's work");
        shared.task_id = "task-1".to_string();
        reg.submit(alice, shared, exec()).expect("alice submit");

        let mut collide = TaskBrief::new("bob's work");
        collide.task_id = "task-1".to_string();
        reg.submit(bob, collide, exec()).expect("bob submit");

        let mut second = TaskBrief::new("alice's other work");
        second.task_id = "task-2".to_string();
        reg.submit(alice, second, exec()).expect("alice submit 2");

        let all = reg.list();
        assert_eq!(all.len(), 3, "all three tasks are recorded");

        // Both `task-1`s survive, and each is attributable.
        let task_1_owners: Vec<TaskOwner> = all
            .iter()
            .filter(|(_, r)| r.brief.task_id == "task-1")
            .map(|(o, _)| *o)
            .collect();
        assert_eq!(task_1_owners.len(), 2, "one submitter's task-1 was lost");
        assert!(task_1_owners.contains(&alice) && task_1_owners.contains(&bob));

        // The brief that comes back with each owner is that owner's.
        let alices_first = all
            .iter()
            .find(|(o, r)| *o == alice && r.brief.task_id == "task-1")
            .expect("alice's task-1");
        assert_eq!(alices_first.1.brief.prompt, "alice's work");

        // `list_for` is the scoped view.
        let hers = reg.list_for(alice);
        assert_eq!(hers.len(), 2, "alice has two tasks");
        assert!(
            hers.iter().all(|r| r.brief.prompt.starts_with("alice's")),
            "list_for leaked another submitter's task: {hers:?}"
        );
        assert_eq!(reg.list_for(bob).len(), 1);
        assert!(reg.list_for(TaskOwner::Local).is_empty());

        for (owner, rec) in all {
            reg.cancel(owner, &rec.brief.task_id);
        }
    }

    #[tokio::test]
    async fn cancel_token_is_race_free() {
        let token = CancelToken::new();
        token.cancel(); // trip BEFORE awaiting
        assert!(token.is_cancelled());
        // cancelled() must still resolve promptly (the watch retains the value).
        tokio::time::timeout(Duration::from_millis(100), token.cancelled())
            .await
            .expect("cancelled() resolves after a pre-await cancel");
    }

    #[test]
    fn wire_types_round_trip() {
        let brief = TaskBrief::new("do a thing")
            .with_context_refs(vec!["blob://ctx".to_string()])
            .with_tags(vec!["region:office".to_string()]);
        assert_eq!(TaskBrief::decode(&brief.encode()).unwrap(), brief);

        let ack = TaskAck {
            task_id: brief.task_id.clone(),
            accepted: true,
            reason: None,
        };
        assert_eq!(TaskAck::decode(&ack.encode()).unwrap(), ack);

        let rec = TaskRecord {
            brief,
            state: TaskState::Completed {
                result_ref: "blob://r".to_string(),
            },
            updated_at: 42,
        };
        let back = TaskRecord::decode(&rec.encode()).unwrap();
        assert_eq!(back, rec);
        assert!(back.state.is_terminal());
        assert_eq!(back.state.label(), "completed");
    }

    #[test]
    fn task_ids_are_unique() {
        let a = TaskBrief::new("x");
        let b = TaskBrief::new("x");
        assert_ne!(a.task_id, b.task_id);
        assert_eq!(a.task_id.len(), 32);
    }

    // ---- admission split -------------------------------------------------

    /// An executor that announces every start on a channel before waiting
    /// for the cancel token: a test can then observe how many runs began
    /// (including one that should never have) without sleeping for it.
    struct SignallingExecutor {
        runs: Arc<AtomicUsize>,
        started: tokio::sync::mpsc::UnboundedSender<()>,
    }

    #[async_trait::async_trait]
    impl TaskExecutor for SignallingExecutor {
        async fn run(&self, _brief: TaskBrief, cancel: CancelToken) -> Result<String, String> {
            self.runs.fetch_add(1, Ordering::SeqCst);
            let _ = self.started.send(());
            cancel.cancelled().await;
            Err("cancelled".to_string())
        }
    }

    fn completing_executor() -> Arc<dyn TaskExecutor> {
        Arc::new(MockExecutor {
            result: "blob://r".to_string(),
            saw_cancel: Arc::new(AtomicBool::new(false)),
            wait_for_cancel: false,
        })
    }

    #[tokio::test]
    async fn reserve_then_release_leaves_no_entry() {
        let reg = TaskRegistry::new();
        let brief = TaskBrief::new("admitted, then refused");
        let id = brief.task_id.clone();
        let Admission::Reserved(ticket) = reg
            .reserve(TaskOwner::Local, brief.clone())
            .expect("a fresh id reserves")
        else {
            panic!("a fresh id must hand back a ticket");
        };
        assert_eq!(ticket.task_id(), id);
        assert_eq!(ticket.brief(), &brief);
        assert_eq!(ticket.owner(), TaskOwner::Local);
        assert_eq!(
            reg.status(TaskOwner::Local, &id),
            Some(TaskState::Requested),
            "a reservation is visible as `requested` while it is being decided"
        );

        ticket.release();

        assert!(
            reg.status(TaskOwner::Local, &id).is_none(),
            "a released reservation must leave no entry behind"
        );
        assert!(reg.record(TaskOwner::Local, &id).is_none());
        assert!(reg.list().is_empty());
        assert!(!reg.cancel(TaskOwner::Local, &id));
        // The id is free again: a refused admission must not squat it.
        assert!(matches!(
            reg.reserve(TaskOwner::Local, brief),
            Ok(Admission::Reserved(_))
        ));
    }

    #[tokio::test]
    async fn a_dropped_ticket_releases_the_reservation() {
        let reg = TaskRegistry::new();
        let brief = TaskBrief::new("handler returned early");
        let id = brief.task_id.clone();
        let waiter = {
            let Admission::Reserved(ticket) = reg
                .reserve(TaskOwner::Local, brief.clone())
                .expect("a fresh id reserves")
            else {
                panic!("a fresh id must hand back a ticket");
            };
            let Admission::Pending(rx) = reg
                .reserve(TaskOwner::Local, brief.clone())
                .expect("an identical brief never conflicts")
            else {
                panic!("an undecided reservation must park the second admission");
            };
            // The handler returns without deciding — `?` on a decode error,
            // an in-body refusal, an early return.
            drop(ticket);
            rx
        };
        assert!(
            reg.status(TaskOwner::Local, &id).is_none(),
            "an unconsumed ticket stranded a `requested` entry that would refuse every later submission"
        );
        assert!(
            await_admission_verdict(waiter).await.is_err(),
            "the parked admission must learn the reservation is gone rather than wait forever"
        );
        assert!(matches!(
            reg.reserve(TaskOwner::Local, brief),
            Ok(Admission::Reserved(_))
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_identical_reserves_share_one_verdict() {
        const N: usize = 8;
        let reg = TaskRegistry::new();
        let brief = TaskBrief::new("one job, eight retransmits");
        let gate = Arc::new(tokio::sync::Barrier::new(N));

        let mut racers = Vec::with_capacity(N);
        for _ in 0..N {
            let reg = reg.clone();
            let brief = brief.clone();
            let gate = Arc::clone(&gate);
            racers.push(tokio::spawn(async move {
                gate.wait().await;
                reg.reserve(TaskOwner::Local, brief)
            }));
        }

        let mut ticket = None;
        let mut parked = Vec::new();
        for racer in racers {
            match racer
                .await
                .expect("reserver task")
                .expect("identical briefs never conflict")
            {
                Admission::Reserved(t) => {
                    assert!(ticket.is_none(), "two tickets were issued for one task id");
                    ticket = Some(t);
                }
                Admission::Pending(rx) => parked.push(rx),
                Admission::Existing(_) => panic!("nothing has launched yet"),
            }
        }
        assert_eq!(
            parked.len(),
            N - 1,
            "exactly one admission decides; the rest wait on its verdict"
        );

        let runs = Arc::new(AtomicUsize::new(0));
        let (started_tx, mut started) = tokio::sync::mpsc::unbounded_channel();
        let id = ticket
            .expect("one reserver")
            .launch(Arc::new(SignallingExecutor {
                runs: Arc::clone(&runs),
                started: started_tx,
            }));
        started.recv().await.expect("the launched executor started");

        for rx in parked {
            assert_eq!(
                await_admission_verdict(rx).await,
                Ok(id.clone()),
                "every parked admission converges on the one launched task"
            );
        }
        assert!(reg.cancel(TaskOwner::Local, &id));
        assert_eq!(wait_terminal(&reg, &id).await, TaskState::Cancelled);
        assert!(started.try_recv().is_err(), "a second executor started");
        assert_eq!(runs.load(Ordering::SeqCst), 1, "eight admissions, one run");
    }

    #[tokio::test]
    async fn a_ticket_launches_exactly_once() {
        let reg = TaskRegistry::new();
        let runs = Arc::new(AtomicUsize::new(0));
        let (started_tx, mut started) = tokio::sync::mpsc::unbounded_channel();
        let exec: Arc<dyn TaskExecutor> = Arc::new(SignallingExecutor {
            runs: Arc::clone(&runs),
            started: started_tx,
        });
        let brief = TaskBrief::new("launch me once");
        let Admission::Reserved(ticket) = reg
            .reserve(TaskOwner::Local, brief.clone())
            .expect("a fresh id reserves")
        else {
            panic!("a fresh id must hand back a ticket");
        };
        let id = ticket.launch(Arc::clone(&exec));
        started.recv().await.expect("the launched executor started");

        // A launched entry answers with its own id. It must never hand out a
        // second ticket for work that is already running — that is the only
        // way the same reservation could be launched twice.
        match reg
            .reserve(TaskOwner::Local, brief)
            .expect("an identical brief never conflicts")
        {
            Admission::Existing(again) => assert_eq!(again, id),
            Admission::Pending(_) => panic!("a launched entry is not still pending"),
            Admission::Reserved(second) => {
                second.launch(Arc::clone(&exec));
            }
        }

        assert!(reg.cancel(TaskOwner::Local, &id));
        assert_eq!(wait_terminal(&reg, &id).await, TaskState::Cancelled);
        assert!(
            started.try_recv().is_err(),
            "a second executor was started for one task"
        );
        assert_eq!(
            runs.load(Ordering::SeqCst),
            1,
            "the reservation launched its work exactly once"
        );
    }

    #[tokio::test]
    async fn interrupted_is_terminal_and_uncancellable() {
        let reg = TaskRegistry::new();
        let brief = TaskBrief::new("owner restarted mid-run");
        let id = brief.task_id.clone();
        let key = (TaskOwner::Local, id.clone());
        let Admission::Reserved(ticket) = reg
            .reserve(TaskOwner::Local, brief)
            .expect("a fresh id reserves")
        else {
            panic!("a fresh id must hand back a ticket");
        };

        // What a successor owner records for a run whose outcome it cannot know.
        set_state(
            &reg.inner,
            None,
            &key,
            TaskState::Interrupted {
                detail: "outcome_unknown".to_string(),
            },
        );
        let state = reg.status(TaskOwner::Local, &id).expect("recorded");
        assert!(state.is_terminal(), "an interrupted task is over");
        assert_eq!(state.label(), "interrupted");
        assert!(
            !reg.cancel(TaskOwner::Local, &id),
            "there is nothing left to cancel"
        );

        // And nothing may overwrite it — a late writer cannot resurrect it.
        set_state(&reg.inner, None, &key, TaskState::Running);
        assert_eq!(reg.status(TaskOwner::Local, &id), Some(state));
        drop(ticket);
    }

    #[tokio::test]
    async fn the_terminal_hook_fires_once_per_entry() {
        let (rows_tx, mut rows) = tokio::sync::mpsc::unbounded_channel();
        let reg = TaskRegistry::new().with_terminal_hook(Arc::new(
            move |owner: TaskOwner, id: &str, state: &TaskState| {
                let _ = rows_tx.send((owner, id.to_string(), state.clone()));
            },
        ));

        let done = reg
            .submit(
                TaskOwner::Local,
                TaskBrief::new("quick"),
                completing_executor(),
            )
            .expect("submit");
        assert_eq!(
            rows.recv().await.expect("the terminal hook fired"),
            (
                TaskOwner::Local,
                done.clone(),
                TaskState::Completed {
                    result_ref: "blob://r".to_string()
                }
            ),
            "the hook sees the owner, the id and the state that actually landed"
        );

        // The unwind path reaches the same writer, and the completed entry
        // never announces itself a second time (the next row is the panic).
        let boom = reg
            .submit(
                TaskOwner::Local,
                TaskBrief::new("kaboom"),
                Arc::new(PanickingExecutor),
            )
            .expect("submit");
        assert_eq!(
            rows.recv().await.expect("the panic guard notified too"),
            (
                TaskOwner::Local,
                boom,
                TaskState::Failed {
                    error: "executor panicked".to_string()
                }
            )
        );
        assert!(
            rows.try_recv().is_err(),
            "a terminal state was announced more than once"
        );
        assert!(reg.forget(TaskOwner::Local, &done));
    }

    #[tokio::test]
    async fn a_per_entry_retention_override_outlives_the_global_ttl() {
        let reg = TaskRegistry::new();
        let long = TERMINAL_RECORD_TTL_SECS * 10;

        let Admission::Reserved(default_ticket) = reg
            .reserve(TaskOwner::Local, TaskBrief::new("default retention"))
            .expect("reserve")
        else {
            panic!("a fresh id must hand back a ticket");
        };
        let ordinary = default_ticket.launch(completing_executor());

        let Admission::Reserved(kept_ticket) = reg
            .reserve(TaskOwner::Local, TaskBrief::new("published retention"))
            .expect("reserve")
        else {
            panic!("a fresh id must hand back a ticket");
        };
        let kept = kept_ticket
            .with_retention(long)
            .launch(completing_executor());

        wait_terminal(&reg, &ordinary).await;
        wait_terminal(&reg, &kept).await;

        let past_global = now_secs() + TERMINAL_RECORD_TTL_SECS + 10;
        assert_eq!(reg.evict_terminal(TERMINAL_RECORD_TTL_SECS, past_global), 1);
        assert!(reg.status(TaskOwner::Local, &ordinary).is_none());
        assert!(
            reg.status(TaskOwner::Local, &kept).is_some(),
            "the per-entry override must outlive the global TTL"
        );

        let past_override = now_secs() + long + 10;
        assert_eq!(
            reg.evict_terminal(TERMINAL_RECORD_TTL_SECS, past_override),
            1,
            "and it is still a deadline, not immortality"
        );
    }

    // ---- offer, commitment, purchase hash --------------------------------

    fn golden_offer() -> A2aOffer {
        A2aOffer {
            service_id: "summarize".to_string(),
            revision: "2026-09-17".to_string(),
            description: Some("summarize a log bundle".to_string()),
            pricing_terms: Some(r#"{"object":"net.pricing.terms@1","amount":"10"}"#.to_string()),
            bounds: A2aBounds {
                max_prompt_bytes: 4096,
                max_context_refs: 8,
                max_tags: 4,
                max_tag_bytes: 64,
                max_in_flight: 2,
            },
            reservation_ttl_secs: 900,
            reservation_retention_secs: 604_800,
            retention_secs: 3600,
        }
    }

    fn golden_brief() -> TaskBrief {
        TaskBrief::new("summarize the logs")
            .with_task_id("task-1")
            .with_context_refs(vec!["artifact:a".to_string()])
            .with_tags(vec!["tag:b".to_string()])
            .with_service("summarize", "2026-09-17")
    }

    /// Every hash in `rows` must be unique; a collision names both sides.
    fn assert_all_distinct(what: &str, rows: &[(&str, String)]) {
        let mut by_hash: HashMap<&str, &str> = HashMap::new();
        for (label, hash) in rows {
            if let Some(prev) = by_hash.insert(hash.as_str(), label) {
                panic!("{what}: {prev} and {label} hash alike ({hash})");
            }
        }
    }

    #[test]
    fn commitment_golden_vector() {
        let offer = golden_offer();
        let brief = golden_brief();
        let offer_hash = offer.hash();
        let commitment = task_commitment(&offer, &brief);
        let purchase = purchase_hash("0123456789abcdef0123456789abcdef", &commitment);

        // Pinned: the encoding is fixed for `@1`. A change here is a wire
        // break that must mint `@2`, not a test to update.
        assert_eq!(
            offer_hash,
            "eff1f7be82c607329eb3bd0ac4156a0047b4758181001d02fd88c52c95a2393a"
        );
        assert_eq!(
            commitment,
            "e768377cca93da23ec320954f510f71dd8c1fb97393e7a20ba556075fcfb0e87"
        );
        assert_eq!(
            purchase,
            "a802789ac1856f5dd79ffcbb7df1e73db9b8567483f67a97d0a1abb2bc1f6eed"
        );

        // The description is cosmetic: re-wording it must not invalidate an
        // outstanding reservation.
        let mut relabelled = offer.clone();
        relabelled.description = Some("a different blurb".to_string());
        assert_eq!(relabelled.hash(), offer_hash);

        // The commitment's service coordinates come from the offer, never
        // from the brief's optional copies of them.
        let mut mislabelled = brief;
        mislabelled.service = Some("something-else".to_string());
        assert_eq!(task_commitment(&offer, &mislabelled), commitment);
    }

    #[test]
    fn commitment_flips_for_every_bound_field() {
        let offer = golden_offer();
        let brief = golden_brief();
        let mut commitments: Vec<(&str, String)> = vec![("base", task_commitment(&offer, &brief))];
        let mut offer_hashes: Vec<(&str, String)> = vec![("base", offer.hash())];

        // Every field of the offer encoding. `service_id` and `revision` are
        // also commitment fields (the commitment takes them from the offer),
        // so these rows carry both flips.
        let offer_flips: Vec<(&str, A2aOffer)> = vec![
            (
                "service_id",
                A2aOffer {
                    service_id: "summarise".to_string(),
                    ..offer.clone()
                },
            ),
            (
                "revision",
                A2aOffer {
                    revision: "2026-09-18".to_string(),
                    ..offer.clone()
                },
            ),
            (
                "pricing_terms",
                A2aOffer {
                    pricing_terms: None,
                    ..offer.clone()
                },
            ),
            (
                "bounds.max_prompt_bytes",
                A2aOffer {
                    bounds: A2aBounds {
                        max_prompt_bytes: 4097,
                        ..offer.bounds.clone()
                    },
                    ..offer.clone()
                },
            ),
            (
                "bounds.max_context_refs",
                A2aOffer {
                    bounds: A2aBounds {
                        max_context_refs: 9,
                        ..offer.bounds.clone()
                    },
                    ..offer.clone()
                },
            ),
            (
                "bounds.max_tags",
                A2aOffer {
                    bounds: A2aBounds {
                        max_tags: 5,
                        ..offer.bounds.clone()
                    },
                    ..offer.clone()
                },
            ),
            (
                "bounds.max_tag_bytes",
                A2aOffer {
                    bounds: A2aBounds {
                        max_tag_bytes: 65,
                        ..offer.bounds.clone()
                    },
                    ..offer.clone()
                },
            ),
            (
                "bounds.max_in_flight",
                A2aOffer {
                    bounds: A2aBounds {
                        max_in_flight: 3,
                        ..offer.bounds.clone()
                    },
                    ..offer.clone()
                },
            ),
            (
                "reservation_ttl_secs",
                A2aOffer {
                    reservation_ttl_secs: 901,
                    ..offer.clone()
                },
            ),
            (
                "reservation_retention_secs",
                A2aOffer {
                    reservation_retention_secs: 604_801,
                    ..offer.clone()
                },
            ),
            (
                "retention_secs",
                A2aOffer {
                    retention_secs: 3601,
                    ..offer.clone()
                },
            ),
        ];
        for (label, flipped) in &offer_flips {
            offer_hashes.push((label, flipped.hash()));
            commitments.push((label, task_commitment(flipped, &brief)));
        }

        // Every remaining field of the commitment encoding.
        let brief_flips: Vec<(&str, TaskBrief)> = vec![
            (
                "task_id",
                TaskBrief {
                    task_id: "task-2".to_string(),
                    ..brief.clone()
                },
            ),
            (
                "prompt",
                TaskBrief {
                    prompt: "summarize the logs.".to_string(),
                    ..brief.clone()
                },
            ),
            (
                "context_refs",
                TaskBrief {
                    context_refs: vec!["artifact:c".to_string()],
                    ..brief.clone()
                },
            ),
            (
                "tags",
                TaskBrief {
                    tags: vec!["tag:c".to_string()],
                    ..brief.clone()
                },
            ),
        ];
        for (label, flipped) in &brief_flips {
            commitments.push((label, task_commitment(&offer, flipped)));
        }

        assert_eq!(commitments.len(), 1 + offer_flips.len() + brief_flips.len());
        assert_all_distinct("commitment", &commitments);
        assert_all_distinct("offer hash", &offer_hashes);
    }

    #[test]
    fn commitment_frames_array_boundaries() {
        let offer = golden_offer();
        let commit = |prompt: &str, refs: &[&str], tags: &[&str]| {
            task_commitment(
                &offer,
                &TaskBrief::new(prompt)
                    .with_task_id("task-1")
                    .with_context_refs(refs.iter().map(|s| s.to_string()).collect())
                    .with_tags(tags.iter().map(|s| s.to_string()).collect()),
            )
        };

        // A value moved across the refs/tags boundary is different work.
        assert_ne!(
            commit("p", &["artifact:a"], &["tag:b"]),
            commit("p", &[], &["artifact:a", "tag:b"])
        );
        // Two elements are not one element that contains a separator.
        assert_ne!(commit("p", &["a", "b"], &[]), commit("p", &["a,b"], &[]));
        // A quote inside a value cannot imitate a field break.
        assert_ne!(commit("x", &[], &[]), commit("x\"", &[], &[]));
        // An empty list is not a list holding an empty string.
        assert_ne!(commit("p", &[], &[]), commit("p", &[""], &[]));
        // And "missing" is exactly "empty": a legacy brief that omits the
        // field is the same brief, so it commits to the same work.
        let omitted = TaskBrief::decode(br#"{"task_id":"task-1","prompt":"p"}"#)
            .expect("a brief may omit context_refs");
        assert_eq!(task_commitment(&offer, &omitted), commit("p", &[], &[]));
    }

    #[test]
    fn purchase_hash_flips_with_admission_id() {
        let commitment = task_commitment(&golden_offer(), &golden_brief());
        let first = purchase_hash("a1a1a1a1", &commitment);
        assert_ne!(
            first,
            purchase_hash("b2b2b2b2", &commitment),
            "the same work under a second reservation must not share its purchase hash"
        );
        let other_work = task_commitment(&golden_offer(), &golden_brief().with_task_id("task-2"));
        assert_ne!(first, purchase_hash("a1a1a1a1", &other_work));
        // The two inputs are framed, not concatenated.
        assert_ne!(purchase_hash("x", "y"), purchase_hash("y", "x"));
    }

    #[test]
    fn a_legacy_brief_is_wire_unchanged_and_interrupted_round_trips() {
        let brief = TaskBrief::new("do a thing")
            .with_task_id("task-1")
            .with_context_refs(vec!["artifact:a".to_string()])
            .with_tags(vec!["tag:b".to_string()]);
        assert_eq!(
            String::from_utf8(brief.encode()).expect("utf-8"),
            r#"{"task_id":"task-1","prompt":"do a thing","context_refs":["artifact:a"],"tags":["tag:b"]}"#,
            "a brief that names no service must serialize exactly as it did before the service fields existed"
        );
        assert_eq!(
            String::from_utf8(
                brief
                    .clone()
                    .with_service("summarize", "2026-09-17")
                    .encode()
            )
            .expect("utf-8"),
            r#"{"task_id":"task-1","prompt":"do a thing","context_refs":["artifact:a"],"tags":["tag:b"],"service":"summarize","revision":"2026-09-17"}"#,
            "the new fields ride after the old ones, additively"
        );
        let legacy = TaskBrief::decode(br#"{"task_id":"t","prompt":"p"}"#).expect("legacy brief");
        assert_eq!(legacy.service, None);
        assert_eq!(legacy.revision, None);

        let rec = TaskRecord {
            brief,
            state: TaskState::Interrupted {
                detail: "paid_not_started".to_string(),
            },
            updated_at: 42,
        };
        let bytes = rec.encode();
        assert!(
            String::from_utf8(bytes.clone())
                .expect("utf-8")
                .contains(r#""state":"interrupted","detail":"paid_not_started""#),
            "the new terminal state is a tagged variant like every other"
        );
        assert_eq!(TaskRecord::decode(&bytes).expect("round trip"), rec);
    }
}
