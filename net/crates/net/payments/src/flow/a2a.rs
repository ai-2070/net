//! The caller side of paid agent-to-agent tasks: prepare (read-only) →
//! purchase (durable, resumable) → submit (evidence).
//!
//! **The invariant this module owns: one authoritative purchase attempt
//! per intent key.** The key is `(caller entity, provider node, task
//! id)`; the record is a [`PurchaseAttempt`] in `a2a-purchases.json`
//! (the [`crate::policy::store`] JSON + `fs2` sidecar-lock idiom, beside
//! `payment-policy.json`); every state change is a compare-and-set under
//! that lock. Two callers in one process, two processes on one machine,
//! or one process either side of a crash all converge on the same
//! attempt — the same reservation, the same quote, and the same authored
//! payload bytes — rather than minting a second purchase of the same
//! work.
//!
//! Why the record exists at all: a lost pay reply is not a lost payment.
//! The provider's acceptance is payload-idempotent, so re-sending the
//! *identical* payload resolves to the original verdict; re-quoting
//! would buy the work twice. That is only possible if the quote and the
//! payload were durable **before** the pay call, which is what
//! [`A2aCallerFlow::purchase_task`] guarantees.
//!
//! **What a compare-and-set here actually compares.** A state tag is not
//! an identity: re-preparing replaces the record under the same key, so
//! an awaited decision — a wallet still signing, a refusal still in
//! flight — would otherwise publish its result onto whatever now
//! occupies that key and buy a quote nobody authorized. Every record
//! therefore carries an immutable [`AttemptGeneration`], minted at
//! creation and re-minted on replacement, and every write that lands
//! *after* an await re-presents the [`AttemptIdentity`] it was decided
//! against — that generation and that exact quote id. A refused write is
//! [`PurchaseError::Superseded`]: retryable, and never a claim about
//! money.
//!
//! Refusing the stale write protects the replacement; it cannot unmake a
//! charge. So when the awaited operation *did* produce a financial
//! result — a settled payment, or an exposed payload of unknown fate —
//! that result is retained beside the live attempt under the incarnation
//! it belongs to, in the unresolved-financial class, and closes through
//! [`A2aCallerFlow::resolve_superseded_attempt`]. Discarding it behind a
//! retryable error would be a lost payment.
//!
//! Three boundaries hold throughout, and each one is a state, not a
//! convention:
//!
//! - **(a) Re-preparing after an *unexposed* outcome mints a new quote**
//!   — so it passes spend policy again and needs a fresh operator
//!   approval where policy demands one. Approvals key on `quote_id`, so
//!   the stale hold is cleared as part of the re-prepare.
//! - **(b) [`PurchaseState::Unknown`] recovers automatically only
//!   through the stored payment** — never through a new quote. The only
//!   other exit is an operator's [`A2aCallerFlow::resolve_attempt`], for
//!   a provider that is gone or has purged the quote.
//! - **(c) A paid purchase the provider will not execute keeps its
//!   evidence** in [`PurchaseState::PaidUnexecutable`], never a relabel
//!   to "refused".
//!
//! And the distinction that makes the accounting honest: an **exposed**
//! refusal is not proof of non-settlement. Every real scheme authors a
//! self-contained bearer pull authorization the counterparty could
//! settle regardless of what it reports back, so
//! [`PurchaseState::RefusedExposed`] records that the spend reservation
//! was *kept* and [`A2aPurchase::Denied`] renders `funds_ambiguous:
//! true`. The per-scheme decision is not re-implemented here: it belongs
//! to [`CallerPaymentFlow::pay_exact`], and this module records what it
//! did by reading the reservation back.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use net::adapter::net::identity::EntityId;
use net_sdk::a2a::{
    purchase_hash, task_commitment, A2aOffer, PrepareReply, PreparedTask, TaskAck, TaskBrief,
};
use net_sdk::a2a_payment::TaskPaymentProof;
use net_sdk::mesh::Mesh;
use net_sdk::mesh_a2a::{A2aFlowError, A2A_TASK_SERVICE};
use net_sdk::tool_payment::FailureSchematic;
use serde::{Deserialize, Serialize};

use super::mesh::MeshPaymentChannel;
use super::{reject_releases_reservation, CallerDecision, CallerPaymentFlow, Clock};
use crate::core::quote::PaymentQuote;
use crate::policy::spend::{ApprovalOutcome, SpendDecision};
use crate::policy::store::{load_json, mutate_json_if_changed, StoreError};

/// How long a `Preparing` or `Paying` lease is honored before another
/// caller may take it over. Two minutes: long enough that no live
/// prepare round trip (or pay round trip whose payload never got
/// persisted) is stolen from under a working caller, short enough that a
/// crashed one does not strand the intent until an operator notices.
pub const PREPARE_LEASE_NS: u64 = 120_000_000_000;

/// How long one wait on a per-key [`tokio::sync::Notify`] parks before
/// the waiter re-reads the store. The notify is the fast path (an
/// in-process sibling wakes it the instant it commits); this bound is
/// what keeps a **cross-process** sibling — which cannot be notified —
/// from waiting on a wake that will never come.
const WAIT_STEP_MS: u64 = 250;
/// How many wait steps a caller takes before handing the decision back
/// as retryable. Deliberately far below [`PREPARE_LEASE_NS`]: waiting is
/// a convergence convenience, and the caller's retry is the contract.
const MAX_WAIT_STEPS: u32 = 8;

/// The handler-authored admission reasons this flow classifies a paid
/// submission's refusal by. The posture table lives in
/// [`FailureSchematic`]'s docs; these three are named because a refusal
/// that says the provider will never execute this purchase must move the
/// attempt to [`PurchaseState::PaidUnexecutable`] even if a future
/// schematic revision loosened its recovery flags.
const TERMINAL_SUBMIT_REASONS: [&str; 4] = [
    "admission_revoked",
    "no_reservation",
    "input_binding_mismatch",
    // The task already ran and its result was retired: the provider's
    // launch ledger bars a relaunch, so no retry and no fresh quote can
    // ever make this proof execute. Listed explicitly as well as being
    // covered by the no-retry/no-requote posture, because the vocabulary
    // is the contract a caller branches on.
    "retired",
];

/// The `reason` a retained superseded attempt's refusal carries: the
/// purchase is paid (or exposed) and its intent key moved on, so no
/// retry and no re-quote can close it — only an operator can.
pub const SUPERSEDED_REFUSAL_REASON: &str = "superseded_attempt";

// ---------------------------------------------------------------------------
// Keys, records, states
// ---------------------------------------------------------------------------

/// The intent this caller is buying: who is paying, which provider, and
/// which task id. Exactly one authoritative [`PurchaseAttempt`] exists
/// per key.
///
/// The task id is part of the key rather than derived from it, because a
/// retained task id is what makes a purchase resumable: the caller that
/// prepared, paid, and then lost the submit reply re-submits the *same*
/// id and converges on the original admission.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PurchaseKey {
    /// The paying entity, lowercase hex.
    pub caller_hex: String,
    /// The provider node the reservation lives on.
    pub provider_node: u64,
    /// The task id.
    pub task_id: String,
}

impl PurchaseKey {
    pub fn new(caller: &EntityId, provider_node: u64, task_id: impl Into<String>) -> Self {
        Self {
            caller_hex: hex::encode(caller.as_bytes()),
            provider_node,
            task_id: task_id.into(),
        }
    }

    /// The store's map key. Unambiguous by construction: the caller hex
    /// is fixed-width and the node id is decimal, so a task id
    /// containing `/` cannot collide with another key.
    pub fn id(&self) -> String {
        format!(
            "{}/{}/{}",
            self.caller_hex, self.provider_node, self.task_id
        )
    }
}

impl std::fmt::Display for PurchaseKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.id())
    }
}

/// What a provider said when it refused to execute a purchase that was
/// already paid for. Retained on the attempt — it is the caller's half
/// of the reconciliation evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RefusalRecord {
    /// When the refusal was recorded (caller clock, ns).
    pub at_ns: u64,
    /// The provider's human-readable refusal.
    pub message: String,
    /// The schematic's `reason`, when the provider sent one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// From the schematic's recovery block; `false` when absent.
    pub safe_to_retry: bool,
    /// From the schematic's recovery block; `false` when absent.
    pub safe_to_requote: bool,
}

/// The immutable identity of one **incarnation** of a
/// [`PurchaseAttempt`].
///
/// A compare-and-swap that checks only the state *tag* cannot tell an
/// attempt from its replacement, so an awaited decision — a signer that
/// finally returned, a refusal that finally landed — publishes its
/// result onto whatever record now occupies the key. The generation is
/// what makes that impossible: it is minted when the record is created,
/// never mutated afterwards, and every post-await write re-presents it
/// (see [`AttemptIdentity`]).
///
/// Two fields, because each answers a question the other cannot:
///
/// - `seq` is monotonic within the key, so "older decision" is a
///   readable fact in the store and in an error message.
/// - `incarnation` is unique per creation, because `seq` **restarts** at
///   1 when a key is pruned or discarded and prepared again. A pure
///   counter would let a decision taken against the first incarnation
///   match the third — the same ABA the generation exists to close.
///
/// `Default` — `gen 0/` — exists for exactly one reason: the field is
/// `#[serde(default)]`, so a file written before generations existed
/// still loads. **No production path mints it**; every mint goes through
/// `mint_generation` with `seq >= 1` and a unique incarnation. Two
/// pre-generation rows therefore share this identity *value*, which is
/// harmless because a guarded write resolves its record by key and only
/// then compares: a shared value lets a decision write to the row it was
/// read from, and to nothing else.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptGeneration {
    /// 1 for a fresh record; +1 on every replacement (re-prepare, or a
    /// stale-lease takeover). `0` only on a record written before
    /// generations existed.
    pub seq: u64,
    /// Minted at creation from the pid, a per-process sequence, the
    /// caller's clock and the key. Not a secret and not a nonce: it only
    /// has to be unique, so that no incarnation of a key can be mistaken
    /// for another one. Empty only on a pre-generation record.
    pub incarnation: String,
}

impl std::fmt::Display for AttemptGeneration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "gen {}/{}", self.seq, self.incarnation)
    }
}

/// What a write decided **before** an await must re-present to be
/// allowed to land afterwards: the exact record incarnation, and the
/// exact quote the decision was taken against.
///
/// Both halves are load-bearing. The generation refuses a write aimed at
/// a record that has been replaced; the quote id refuses one aimed at a
/// quote that has been replaced *within* a record. A write the store
/// refuses on this check is [`PurchaseError::Superseded`] — retryable,
/// and never a statement about money.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptIdentity {
    /// The incarnation the decision was taken against.
    pub generation: AttemptGeneration,
    /// The quote the decision was taken against, if the record had one.
    pub quote_id: Option<String>,
}

impl std::fmt::Display for AttemptIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} quote {}",
            self.generation,
            self.quote_id.as_deref().unwrap_or("-")
        )
    }
}

/// One caller-side purchase attempt: the durable record of everything
/// needed to finish, or to reconcile, exactly one purchase.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PurchaseAttempt {
    /// The intent key. Present in the record as well as the map key so a
    /// record read out of the file stands alone.
    pub key: PurchaseKey,
    /// `task_commitment(offer, brief)` — what this attempt is buying. A
    /// different commitment under the same key is
    /// [`PurchaseError::CommitmentConflict`], never a silent re-purchase.
    pub commitment: String,
    /// This incarnation's immutable identity. Minted when the record is
    /// created, re-minted when the record is *replaced*, and never
    /// touched by an ordinary transition — which is what lets a write
    /// decided before an await prove it is still writing to the record
    /// it decided about. `#[serde(default)]` so a file written before
    /// generations existed still loads (as `gen 0/`).
    #[serde(default)]
    pub generation: AttemptGeneration,
    /// The provider's reservation and the exact brief it admits. `None`
    /// only while `Preparing`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepared: Option<PreparedTask>,
    /// The provider-signed quote's canonical envelope bytes, exactly as
    /// signed — never a re-serialization.
    ///
    /// Base64 on disk, like the spend store's held quote: a JSON array
    /// of byte integers is ~4× the bytes, on a file that is re-serialized
    /// and fsynced under a lock on every transition.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "b64_bytes")]
    pub quote_bytes: Option<Vec<u8>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote_id: Option<String>,
    /// The quote's authoritative expiry, for the "expired and never
    /// paid, so re-prepare" decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote_expires_at_ns: Option<u64>,
    /// The authored x402 payload, byte-exact. Persisted **before** the
    /// pay call; re-sent verbatim on every resume. Base64 on disk.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "b64_bytes")]
    pub payload_bytes: Option<Vec<u8>>,
    pub state: PurchaseState,
    pub updated_at_ns: u64,
}

/// Base64 for the attempt's byte-exact fields. The same choice, for the
/// same reason, as the spend store's held quote (`ApprovalRecord`): these
/// are preserved signed/authored bytes that must survive a round trip
/// untouched, and a JSON integer array would cost roughly four bytes of
/// file per byte of payload on every locked write.
mod b64_bytes {
    use base64::engine::general_purpose::STANDARD as BASE64;
    use base64::Engine as _;
    use serde::{Deserialize as _, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(
        value: &Option<Vec<u8>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(bytes) => serializer.serialize_str(&BASE64.encode(bytes)),
            None => serializer.serialize_none(),
        }
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Vec<u8>>, D::Error> {
        let encoded = Option::<String>::deserialize(deserializer)?;
        match encoded {
            Some(encoded) => BASE64
                .decode(encoded.as_bytes())
                .map(Some)
                .map_err(serde::de::Error::custom),
            None => Ok(None),
        }
    }
}

impl PurchaseAttempt {
    /// Is this attempt exempt from ordinary retention?
    ///
    /// `Paying`, `Unknown`, `RefusedExposed`, `PaidUnexecutable` and a
    /// `Paid` attempt that was never submitted all describe money whose
    /// fate is unresolved; pruning them would delete the caller's only
    /// evidence. They leave only through a resolved disposition.
    ///
    /// `Paying` is in this class because it is the **exposed** state, not
    /// merely the busy one: the CAS that writes it persists the authored
    /// payload in the same transaction, and from that moment the bytes
    /// that authorize the charge may be on the wire. A row whose reply
    /// was lost is indistinguishable from one whose payment settled, and
    /// it holds the byte-exact payload that is the only way to ask the
    /// provider which. Deleting it destroys that evidence *and* lets
    /// `begin_prepare` mint a second quote for work that may already be
    /// paid for. The disposition is always reachable — resuming the
    /// stored payload resolves it to `Paid`, `Unknown` or
    /// `RefusedExposed`, and `Unknown` has the operator exit — so this
    /// retains an in-flight attempt, never an immortal one.
    pub fn is_unresolved_financial(&self) -> bool {
        matches!(
            self.state,
            PurchaseState::Paying { .. }
                | PurchaseState::Unknown { .. }
                | PurchaseState::RefusedExposed { .. }
                | PurchaseState::PaidUnexecutable { .. }
                | PurchaseState::Paid { .. }
        )
    }

    /// What a decision taken against this record must re-present to
    /// publish its result: this incarnation, and this quote.
    ///
    /// Read **before** the await, compared **after** it. A colliding
    /// pair is not reachable by accident: `incarnation` is unique per
    /// creation, so two records with the same identity are the same
    /// record.
    pub fn identity(&self) -> AttemptIdentity {
        AttemptIdentity {
            generation: self.generation.clone(),
            quote_id: self.quote_id.clone(),
        }
    }
}

/// Where one purchase attempt stands.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PurchaseState {
    /// CAS-inserted **before any network call**, so a crash mid-prepare
    /// leaves a record rather than nothing. Stale after
    /// [`PREPARE_LEASE_NS`], after which another caller may take it over.
    Preparing { lease_id: String, since_ns: u64 },
    /// Reservation and provider-signed quote persisted. Nothing is
    /// reserved on the spend side and no money has moved — this is the
    /// state a price can be displayed from.
    Quoted,
    /// Spend policy held the quote for an operator. Approvals key on
    /// `quote_id`, so this hold is for this exact purchase.
    AwaitingApproval,
    /// The payload is authored and persisted; a pay request may be in
    /// flight. Any caller may re-send the stored payload; only the lease
    /// holder (or a taker-over after [`PREPARE_LEASE_NS`]) may author.
    /// **Exposed**, therefore unresolved-financial: the authorizing
    /// bytes exist and may have reached the provider, so the row is
    /// never pruned (see [`PurchaseAttempt::is_unresolved_financial`]).
    Paying { lease_id: String, since_ns: u64 },
    /// Paid. `billing` is the payment proof (settlement ref + signed
    /// billing event) the caller keeps as its own evidence.
    Paid {
        proof: TaskPaymentProof,
        billing: serde_json::Value,
    },
    /// The pay reply was lost, or settlement is still pending. **Not** a
    /// failure: recovered automatically by re-sending the stored payload
    /// (boundary b). The spend reservation is kept, exactly as
    /// [`CallerPaymentFlow::pay_exact`] keeps it on transport ambiguity.
    Unknown { last_error: String },
    /// Refused **before any authorization left this process** — spend
    /// denied, quote refused, authoring failed. Proven that no money
    /// moved, so this attempt may be prepared again (boundary a) and
    /// follows ordinary retention.
    RefusedUnexposed { reason: String },
    /// The provider answered `Rejected` / `Failure` / `Invalidated` /
    /// `Exception` **after** a bearer authorization was exposed. Not
    /// proof of non-settlement: financially ambiguous, never re-quoted,
    /// and an operator's `resolve_attempt` is the only exit.
    RefusedExposed {
        reason: String,
        reservation_kept: bool,
    },
    /// Submitted and accepted. Terminal for the *attempt*; the task's own
    /// lifecycle continues through status/cancel.
    Submitted { task_id: String },
    /// Paid, and the provider will not execute it (boundary c). The
    /// payment evidence is retained beside the refusal.
    PaidUnexecutable {
        proof: TaskPaymentProof,
        billing: serde_json::Value,
        refusal: RefusalRecord,
    },
    /// An operator closed an ambiguous attempt: refunded, written off, or
    /// executed elsewhere. The evidence is kept in `evidence`.
    Resolved {
        outcome: String,
        evidence: serde_json::Value,
    },
}

impl PurchaseState {
    pub fn tag(&self) -> StateTag {
        match self {
            Self::Preparing { .. } => StateTag::Preparing,
            Self::Quoted => StateTag::Quoted,
            Self::AwaitingApproval => StateTag::AwaitingApproval,
            Self::Paying { .. } => StateTag::Paying,
            Self::Paid { .. } => StateTag::Paid,
            Self::Unknown { .. } => StateTag::Unknown,
            Self::RefusedUnexposed { .. } => StateTag::RefusedUnexposed,
            Self::RefusedExposed { .. } => StateTag::RefusedExposed,
            Self::Submitted { .. } => StateTag::Submitted,
            Self::PaidUnexecutable { .. } => StateTag::PaidUnexecutable,
            Self::Resolved { .. } => StateTag::Resolved,
        }
    }

    /// The lease this state carries, if it is a leased state — which
    /// caller's claim the record is currently under.
    pub fn lease(&self) -> Option<&str> {
        match self {
            Self::Preparing { lease_id, .. } | Self::Paying { lease_id, .. } => Some(lease_id),
            _ => None,
        }
    }

    /// When the lease was taken, if it is a leased state.
    fn since_ns(&self) -> Option<u64> {
        match self {
            Self::Preparing { since_ns, .. } | Self::Paying { since_ns, .. } => Some(*since_ns),
            _ => None,
        }
    }
}

/// A [`PurchaseState`] without its payload — what the transition table is
/// written in terms of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateTag {
    Preparing,
    Quoted,
    AwaitingApproval,
    Paying,
    Paid,
    Unknown,
    RefusedUnexposed,
    RefusedExposed,
    Submitted,
    PaidUnexecutable,
    Resolved,
}

impl StateTag {
    /// Every tag, so the transition table can be enumerated exhaustively
    /// (a test that drives all `ALL × ALL` pairs is how "no other
    /// transition exists" is checked rather than asserted).
    pub const ALL: [StateTag; 11] = [
        StateTag::Preparing,
        StateTag::Quoted,
        StateTag::AwaitingApproval,
        StateTag::Paying,
        StateTag::Paid,
        StateTag::Unknown,
        StateTag::RefusedUnexposed,
        StateTag::RefusedExposed,
        StateTag::Submitted,
        StateTag::PaidUnexecutable,
        StateTag::Resolved,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Preparing => "preparing",
            Self::Quoted => "quoted",
            Self::AwaitingApproval => "awaiting_approval",
            Self::Paying => "paying",
            Self::Paid => "paid",
            Self::Unknown => "unknown",
            Self::RefusedUnexposed => "refused_unexposed",
            Self::RefusedExposed => "refused_exposed",
            Self::Submitted => "submitted",
            Self::PaidUnexecutable => "paid_unexecutable",
            Self::Resolved => "resolved",
        }
    }
}

impl std::fmt::Display for StateTag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// **The caller transition table.** Every state change this flow can
/// make is one of these pairs; [`A2aPurchaseStore::transition`] refuses
/// everything else as [`PurchaseError::NotATransition`], so a new path
/// through the flow cannot quietly invent a transition.
///
/// The self-pairs are not padding: `Preparing → Preparing` is the stale
/// lease takeover, `Paying → Paying` writes the authored payload under
/// the same lease, `Unknown → Unknown` records a second ambiguous
/// attempt, and `Paid → Paid` is a retryable submit refusal (the proof
/// stays valid, the attempt is resubmitted).
pub fn is_table_transition(from: StateTag, to: StateTag) -> bool {
    use StateTag as T;
    matches!(
        (from, to),
        // prepare
        (T::Preparing, T::Quoted)
            | (T::Preparing, T::Preparing)
            | (T::Quoted, T::Preparing)
            | (T::AwaitingApproval, T::Preparing)
            | (T::RefusedUnexposed, T::Preparing)
            // purchase, pre-exposure
            | (T::Quoted, T::AwaitingApproval)
            | (T::Quoted, T::Paying)
            | (T::AwaitingApproval, T::Paying)
            | (T::Quoted, T::RefusedUnexposed)
            | (T::AwaitingApproval, T::RefusedUnexposed)
            | (T::Paying, T::Paying)
            // purchase, post-exposure
            | (T::Paying, T::Paid)
            | (T::Unknown, T::Paid)
            | (T::Paying, T::Unknown)
            | (T::Unknown, T::Unknown)
            | (T::Paying, T::RefusedExposed)
            | (T::Unknown, T::RefusedExposed)
            // operator exits
            | (T::Unknown, T::RefusedUnexposed)
            | (T::RefusedExposed, T::Resolved)
            | (T::PaidUnexecutable, T::Resolved)
            // submit
            | (T::Paid, T::Paid)
            | (T::Paid, T::Submitted)
            | (T::Paid, T::PaidUnexecutable)
    )
}

/// Failures of the purchase store's ownership contract.
#[derive(Debug, thiserror::Error)]
pub enum PurchaseError {
    /// No attempt exists under this key.
    #[error("no purchase attempt for {key}")]
    Missing { key: String },
    /// The attempt exists but is in a state this verb cannot act from.
    #[error("purchase attempt for {key} is `{found}`, expected one of [{expected}]")]
    Conflict {
        key: String,
        found: StateTag,
        expected: String,
    },
    /// Another caller holds the lease this verb needs.
    #[error("purchase attempt for {key} is leased by another caller")]
    LeaseLost { key: String },
    /// A different piece of work is already being purchased under this
    /// key — the one refusal that must never be a silent re-purchase.
    #[error("purchase attempt for {key} commits to different work")]
    CommitmentConflict { key: String },
    /// This write was decided against a record incarnation (or a quote)
    /// the key no longer holds — a replacement was minted while the
    /// decision was in flight.
    ///
    /// **Retryable, and never a financial fact.** It says one thing
    /// only: this result may not be published *here*. Whether money
    /// moved is the awaited operation's answer, and an authoritative one
    /// is retained against the original incarnation rather than
    /// discarded (see [`A2aCallerFlow::resolve_superseded_attempt`]).
    #[error(
        "purchase attempt for {key} is no longer the incarnation this decision was taken \
         against (decided for {expected}, key now holds {found})"
    )]
    Superseded {
        key: String,
        expected: String,
        found: String,
    },
    /// Not a row of the caller transition table.
    #[error("`{from}` → `{to}` is not a caller purchase transition")]
    NotATransition { from: StateTag, to: StateTag },
    #[error(transparent)]
    Store(#[from] StoreError),
}

// ---------------------------------------------------------------------------
// The durable store
// ---------------------------------------------------------------------------

/// The on-disk document of `a2a-purchases.json`: the **live** attempt
/// per [`PurchaseKey::id`], and — in its own map — the retained
/// evidence of incarnations the live key no longer holds.
///
/// Public because it is the recovery surface — an operator tool (or a
/// test) reads and seeds attempts through the same locked
/// [`crate::policy::store`] helpers this module uses, rather than
/// through a private shape it has to guess.
///
/// **Two maps, not one namespace with decorated ids.** A task id is
/// unrestricted, so any archive id built by decorating a live key id is
/// an id some legitimate task can also spell — and a retained charge
/// written under it would land on that task's live purchase (and an
/// operator closing the archive row would close the live charge).
/// Separating the classes structurally is what makes the two
/// unreachable from each other; see `RecordClass`.
///
/// **A store written before the split still loads, and its archives
/// stay addressable.** The previous writer kept retained evidence in
/// `attempts` under a decorated id, so such a document has no
/// `superseded` map at all; deserializing it as an ordinary store would
/// accept it and list its rows while making every one of those charges
/// unreachable through the key-and-incarnation exit that closes them.
/// [`A2aPurchaseFile`]'s `Deserialize` therefore classifies what it
/// reads by each record's **own embedded `key` and `generation`** — a
/// live record is the one whose map id *is* `key.id()` — never by a
/// suffix on the map id, which a legitimate task id can spell.
#[derive(Debug, Clone, Default, Serialize)]
pub struct A2aPurchaseFile {
    #[serde(default)]
    pub attempts: BTreeMap<String, PurchaseAttempt>,
    /// Retained evidence of superseded incarnations, by
    /// `superseded_record_id`. Never addressable by a live
    /// [`PurchaseKey`], and never pruned while it is financially
    /// unresolved.
    #[serde(default)]
    pub superseded: BTreeMap<String, PurchaseAttempt>,
}

impl<'de> Deserialize<'de> for A2aPurchaseFile {
    /// Read the document, then place every record in the class its own
    /// identity says it belongs to.
    ///
    /// The migration is a pure function of the record's embedded fields:
    /// an `attempts` entry filed under anything other than its own
    /// `key.id()` is not the live purchase of that key — it is the
    /// previous writer's retained evidence — so it moves to the archive
    /// under `superseded_record_id` of the key and generation it
    /// carries, and merges by `merge_retained` if that incarnation is
    /// already archived. Nothing is inferred from the shape of the map
    /// id, so a live task whose id spells the old decoration stays live.
    ///
    /// Applied on every read, and persisted by the next write that
    /// changes the file — so a prior-format store is exactly addressable
    /// from the first reopen, without a migration pass an operator has
    /// to remember to run.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Document {
            #[serde(default)]
            attempts: BTreeMap<String, PurchaseAttempt>,
            #[serde(default)]
            superseded: BTreeMap<String, PurchaseAttempt>,
        }
        let Document {
            attempts,
            superseded,
        } = Document::deserialize(deserializer)?;
        let mut file = Self {
            attempts: BTreeMap::new(),
            superseded,
        };
        for (id, record) in attempts {
            if id == record.key.id() {
                file.attempts.insert(id, record);
                continue;
            }
            let archived = superseded_record_id(&record.key, &record.generation);
            match file.superseded.get_mut(&archived) {
                Some(existing) => {
                    merge_retained(existing, record);
                }
                None => {
                    file.superseded.insert(archived, record);
                }
            }
        }
        Ok(file)
    }
}

/// Which of the file's two record classes a store verb addresses.
///
/// The live class is the current purchase under an intent key; the
/// retained class is historical financial evidence, addressed by key
/// *and* incarnation. A verb names its class, so no id a caller can
/// influence decides which map a write reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecordClass {
    Live,
    Retained,
}

impl RecordClass {
    /// The only place a class becomes a map. Every verb goes through
    /// here, so "live and historical are separate namespaces" is one
    /// fact in one function rather than a convention each call site
    /// re-implements.
    fn records(self, file: &A2aPurchaseFile) -> &BTreeMap<String, PurchaseAttempt> {
        match self {
            Self::Live => &file.attempts,
            Self::Retained => &file.superseded,
        }
    }

    fn map(self, file: &mut A2aPurchaseFile) -> &mut BTreeMap<String, PurchaseAttempt> {
        match self {
            Self::Live => &mut file.attempts,
            Self::Retained => &mut file.superseded,
        }
    }

    /// Address one record in this class.
    fn at(self, id: String) -> RecordRef {
        RecordRef { class: self, id }
    }
}

/// One addressed record: which class it lives in, and its id there.
///
/// The pair travels together because neither half addresses a record
/// alone — an id without a class is the ambiguity C6 was.
struct RecordRef {
    class: RecordClass,
    id: String,
}

/// The durable purchase store: one authoritative attempt per key, every
/// change a CAS under the cross-process lock.
///
/// Cross-process callers converge because the file is the authority.
/// In-process callers additionally wait on a per-key
/// [`tokio::sync::Notify`], so a sibling that is mid-prepare or
/// mid-payment is *awaited* rather than polled.
pub struct A2aPurchaseStore {
    path: PathBuf,
    waiters: parking_lot::Mutex<HashMap<String, Arc<tokio::sync::Notify>>>,
}

/// What [`A2aPurchaseStore::begin_prepare`] resolved the key to.
#[derive(Debug)]
pub enum PrepareClaim {
    /// This caller owns the `Preparing` lease and must drive the prepare.
    /// `cleared_approval` names an approval hold on the *previous* quote
    /// that the caller must clear — a new quote needs a new approval
    /// (boundary a).
    Leased {
        lease_id: String,
        cleared_approval: Option<String>,
    },
    /// A usable attempt already exists; its stored reservation is the
    /// answer and no quote is requested.
    Ready(Box<PurchaseAttempt>),
    /// Another caller holds a live `Preparing` lease.
    Awaiting,
}

/// What an [`A2aPurchaseStore`] CAS checks *besides* the state tag.
///
/// Exactly one guard, never a pair, because the two are the same thing
/// at two ages of the record: before a quote exists, the caller's lease
/// id is the record's identity (it is minted per claim, so a replacement
/// cannot accept a write under it); once a quote exists, the
/// [`AttemptIdentity`] is.
enum CasGuard<'a> {
    /// The state table only — for a write decided and published without
    /// an await in between.
    None,
    /// The record must still be leased by this exact claim.
    Lease(&'a str),
    /// The record must still be the incarnation, and carry the quote,
    /// that this write was decided against.
    Identity(&'a AttemptIdentity),
}

/// [`CasGuard`] with its borrows resolved, so it can cross into the
/// `'static` locked-mutation closure.
enum OwnedGuard {
    None,
    Lease(String),
    Identity(AttemptIdentity),
}

impl CasGuard<'_> {
    fn owned(self) -> OwnedGuard {
        match self {
            Self::None => OwnedGuard::None,
            Self::Lease(lease) => OwnedGuard::Lease(lease.to_string()),
            Self::Identity(identity) => OwnedGuard::Identity(identity.clone()),
        }
    }
}

impl A2aPurchaseStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            waiters: parking_lot::Mutex::new(HashMap::new()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The per-key notify handle. In-process only by construction — a
    /// cross-process sibling is served by the file and the caller's
    /// bounded re-read.
    fn waiter(&self, key_id: &str) -> Arc<tokio::sync::Notify> {
        let mut waiters = self.waiters.lock();
        waiters.entry(key_id.to_string()).or_default().clone()
    }

    fn wake(&self, key_id: &str) {
        let waiter = {
            let waiters = self.waiters.lock();
            waiters.get(key_id).cloned()
        };
        if let Some(waiter) = waiter {
            waiter.notify_waiters();
        }
    }

    /// The single attempt under `key`, if any.
    pub async fn attempt(&self, key: &PurchaseKey) -> Result<Option<PurchaseAttempt>, StoreError> {
        let file: A2aPurchaseFile = load_json(&self.path).await?;
        Ok(RecordClass::Live.records(&file).get(&key.id()).cloned())
    }

    /// Every record on file — the operator's queue: the live attempts
    /// and the retained evidence of superseded incarnations, each
    /// carrying the `key` and `generation` that address it.
    pub async fn attempts(&self) -> Result<Vec<PurchaseAttempt>, StoreError> {
        let file: A2aPurchaseFile = load_json(&self.path).await?;
        Ok(file
            .attempts
            .into_values()
            .chain(file.superseded.into_values())
            .collect())
    }

    /// Only the retained evidence of superseded incarnations — which
    /// rows close through [`Self::transition_superseded`] rather than
    /// [`Self::transition`]. A key can hold both classes at once, and
    /// the two are closed by different verbs.
    pub async fn retained_attempts(&self) -> Result<Vec<PurchaseAttempt>, StoreError> {
        let file: A2aPurchaseFile = load_json(&self.path).await?;
        Ok(RecordClass::Retained
            .records(&file)
            .values()
            .cloned()
            .collect())
    }

    /// Claim the right to prepare `key` for `commitment`, atomically.
    ///
    /// The insert happens **before** any network call, which is what
    /// makes the attempt recoverable: a crash between here and the quote
    /// leaves a `Preparing` record whose lease goes stale and is taken
    /// over, never an orphaned payment.
    pub async fn begin_prepare(
        &self,
        key: &PurchaseKey,
        commitment: &str,
        now_ns: u64,
    ) -> Result<PrepareClaim, PurchaseError> {
        let id = key.id();
        let key_owned = key.clone();
        let commitment_owned = commitment.to_string();
        let lease_id = mint_lease(&id, now_ns);
        let fresh_generation = mint_generation(&id, now_ns, 1);
        let id_owned = id.clone();
        let claim = mutate_json_if_changed::<A2aPurchaseFile, _, _>(&self.path, move |file| {
            let id = id_owned;
            let Some(existing) = file.attempts.get_mut(&id) else {
                file.attempts.insert(
                    id.clone(),
                    PurchaseAttempt {
                        key: key_owned,
                        commitment: commitment_owned,
                        generation: fresh_generation,
                        prepared: None,
                        quote_bytes: None,
                        quote_id: None,
                        quote_expires_at_ns: None,
                        payload_bytes: None,
                        state: PurchaseState::Preparing {
                            lease_id: lease_id.clone(),
                            since_ns: now_ns,
                        },
                        updated_at_ns: now_ns,
                    },
                );
                return (
                    Ok(PrepareClaim::Leased {
                        lease_id,
                        cleared_approval: None,
                    }),
                    true,
                );
            };
            if existing.commitment != commitment_owned {
                return (
                    Err(PurchaseError::CommitmentConflict { key: id.clone() }),
                    false,
                );
            }
            // The tag and the lease age are read out first so the arms
            // below can take `existing` mutably.
            let found = existing.state.tag();
            let lease_age = existing
                .state
                .since_ns()
                .map(|since| now_ns.saturating_sub(since));
            let quote_live = existing
                .quote_expires_at_ns
                .is_some_and(|expiry| expiry > now_ns);
            match found {
                StateTag::Preparing => {
                    if lease_age.is_some_and(|age| age > PREPARE_LEASE_NS) {
                        let cleared = take_lease(existing, &lease_id, now_ns);
                        (
                            Ok(PrepareClaim::Leased {
                                lease_id,
                                cleared_approval: cleared,
                            }),
                            true,
                        )
                    } else {
                        (Ok(PrepareClaim::Awaiting), false)
                    }
                }
                // A live quote is the answer; an expired one that was
                // never paid is re-prepared under the same admission id,
                // and its approval hold goes with it (boundary a).
                StateTag::Quoted | StateTag::AwaitingApproval => {
                    if quote_live {
                        (Ok(PrepareClaim::Ready(Box::new(existing.clone()))), false)
                    } else {
                        let cleared = take_lease(existing, &lease_id, now_ns);
                        (
                            Ok(PrepareClaim::Leased {
                                lease_id,
                                cleared_approval: cleared,
                            }),
                            true,
                        )
                    }
                }
                // Nothing was exposed, so a fresh quote is legitimate —
                // and it re-runs spend policy and approval.
                StateTag::RefusedUnexposed => {
                    let cleared = take_lease(existing, &lease_id, now_ns);
                    (
                        Ok(PrepareClaim::Leased {
                            lease_id,
                            cleared_approval: cleared,
                        }),
                        true,
                    )
                }
                // Mid-purchase or paid: the stored reservation is the
                // answer, and re-quoting is exactly what must not happen.
                StateTag::Paying | StateTag::Paid | StateTag::Unknown | StateTag::Submitted => {
                    (Ok(PrepareClaim::Ready(Box::new(existing.clone()))), false)
                }
                // Financially unresolved or closed: only an operator
                // moves these, so preparing again is refused rather than
                // silently starting a second purchase.
                StateTag::RefusedExposed | StateTag::PaidUnexecutable | StateTag::Resolved => (
                    Err(PurchaseError::Conflict {
                        key: id.clone(),
                        found,
                        expected: "quoted, refused_unexposed or a live reservation".to_string(),
                    }),
                    false,
                ),
            }
        })
        .await??;
        if matches!(claim, PrepareClaim::Leased { .. }) {
            self.wake(&id);
        }
        Ok(claim)
    }

    /// Abandon a `Preparing` lease this caller owns, deleting the record.
    ///
    /// The prepare failed before anything financial existed, so the
    /// attempt leaves no trace — the next `prepare_task` starts clean.
    pub async fn discard(&self, key: &PurchaseKey, lease_id: &str) -> Result<(), PurchaseError> {
        let id = key.id();
        let lease = lease_id.to_string();
        mutate_json_if_changed::<A2aPurchaseFile, _, _>(&self.path, move |file| {
            match file.attempts.get(&id) {
                Some(attempt)
                    if attempt.state.tag() == StateTag::Preparing
                        && attempt.state.lease() == Some(lease.as_str()) =>
                {
                    file.attempts.remove(&id);
                    (Ok(()), true)
                }
                Some(attempt) => (
                    Err(PurchaseError::LeaseLost {
                        key: format!("{id} ({})", attempt.state.tag()),
                    }),
                    false,
                ),
                None => (Err(PurchaseError::Missing { key: id.clone() }), false),
            }
        })
        .await??;
        self.wake(key.id().as_str());
        Ok(())
    }

    /// Move the attempt from one of `from` to `to`, refusing any pair
    /// outside the caller transition table.
    ///
    /// No identity check: for writes taken and published without an
    /// await in between (an operator's resolution, a state change
    /// decided from the record just read under the same lock-free
    /// re-read), the state check *is* the whole contract. Anything that
    /// awaits between its decision and its write MUST use
    /// [`Self::transition_exact`] instead.
    ///
    /// The store never reads a clock: `now_ns` stamps the record, so a
    /// recovery pass can replay with the times it is reasoning about.
    pub async fn transition(
        &self,
        key: &PurchaseKey,
        from: &[StateTag],
        to: PurchaseState,
        now_ns: u64,
    ) -> Result<PurchaseAttempt, PurchaseError> {
        self.cas(key, from, CasGuard::None, to, now_ns, |_| {})
            .await
    }

    /// [`Self::transition`], plus the check that makes a post-await write
    /// safe: the record must still be the incarnation (and carry the
    /// quote) the decision was taken against, or the write is refused as
    /// [`PurchaseError::Superseded`] rather than landing on whatever
    /// replaced it.
    pub async fn transition_exact(
        &self,
        key: &PurchaseKey,
        from: &[StateTag],
        identity: &AttemptIdentity,
        to: PurchaseState,
        now_ns: u64,
    ) -> Result<PurchaseAttempt, PurchaseError> {
        self.cas(key, from, CasGuard::Identity(identity), to, now_ns, |_| {})
            .await
    }

    /// The CAS every verb funnels through: table check, state check,
    /// guard check, patch, one atomic file replace.
    async fn cas<F>(
        &self,
        key: &PurchaseKey,
        from: &[StateTag],
        guard: CasGuard<'_>,
        to: PurchaseState,
        now_ns: u64,
        patch: F,
    ) -> Result<PurchaseAttempt, PurchaseError>
    where
        F: FnOnce(&mut PurchaseAttempt) + Send,
    {
        self.cas_at(
            RecordClass::Live.at(key.id()),
            from,
            guard,
            to,
            now_ns,
            patch,
        )
        .await
    }

    /// [`Self::cas`] against an explicitly addressed record, so a
    /// retained superseded record — which lives in the archive map,
    /// under its own id — is resolved through the same table and the
    /// same lock, and no live record can be reached by addressing it.
    async fn cas_at<F>(
        &self,
        target: RecordRef,
        from: &[StateTag],
        guard: CasGuard<'_>,
        to: PurchaseState,
        now_ns: u64,
        patch: F,
    ) -> Result<PurchaseAttempt, PurchaseError>
    where
        F: FnOnce(&mut PurchaseAttempt) + Send,
    {
        let RecordRef { class, id } = target;
        let wake_id = id.clone();
        let expected: Vec<StateTag> = from.to_vec();
        let guard = guard.owned();
        let updated = mutate_json_if_changed::<A2aPurchaseFile, _, _>(&self.path, move |file| {
            let Some(attempt) = class.map(file).get_mut(&id) else {
                return (Err(PurchaseError::Missing { key: id.clone() }), false);
            };
            // Identity before state: a write aimed at a record that no
            // longer exists under this key must read as *superseded*,
            // not as a state conflict — the two mean different things to
            // the caller, and only one of them is about this attempt.
            if let OwnedGuard::Identity(identity) = &guard {
                let found = attempt.identity();
                if found != *identity {
                    return (
                        Err(PurchaseError::Superseded {
                            key: id.clone(),
                            expected: identity.to_string(),
                            found: found.to_string(),
                        }),
                        false,
                    );
                }
            }
            let found = attempt.state.tag();
            if !expected.contains(&found) {
                return (
                    Err(PurchaseError::Conflict {
                        key: id.clone(),
                        found,
                        expected: expected
                            .iter()
                            .map(|t| t.label())
                            .collect::<Vec<_>>()
                            .join(", "),
                    }),
                    false,
                );
            }
            if !is_table_transition(found, to.tag()) {
                return (
                    Err(PurchaseError::NotATransition {
                        from: found,
                        to: to.tag(),
                    }),
                    false,
                );
            }
            if let OwnedGuard::Lease(lease) = &guard {
                if attempt.state.lease() != Some(lease.as_str()) {
                    return (Err(PurchaseError::LeaseLost { key: id.clone() }), false);
                }
            }
            attempt.state = to;
            attempt.updated_at_ns = now_ns;
            patch(attempt);
            (Ok(attempt.clone()), true)
        })
        .await??;
        self.wake(&wake_id);
        Ok(updated)
    }

    /// Retain the financial outcome of a decision whose record the live
    /// key no longer accepts.
    ///
    /// Refusing a stale write protects the *replacement*; it does not
    /// unmake a payment. So when the awaited operation came back with
    /// something financial — a settled payment, or an exposed payload
    /// whose fate is unknown — the evidence is written into the archive
    /// map under the **original** incarnation's own record id, in the
    /// unresolved-financial class, where [`Self::attempts`] shows it and
    /// [`A2aCallerFlow::resolve_superseded_attempt`] closes it. The live
    /// key is untouched, and cannot be reached from here at all.
    ///
    /// **Merged under the lock by evidence precedence, never
    /// last-write-wins** (see `evidence_rank`). Outcomes about one
    /// incarnation arrive in whatever order the network and the operator
    /// produce them: a sibling's delayed `Unknown`, its claimed refusal,
    /// or a settlement that lost a race. A write that knows *less* than
    /// what is already retained is dropped, so ambiguity cannot erase
    /// settlement proof; a resolved disposition is never overwritten,
    /// and a settlement arriving after one is retained *beside* it (an
    /// operator's accounting decision stands, but a real charge it never
    /// saw stays findable). Re-publishing the same outcome is a no-op,
    /// so a retry of one decision never mints a second record of one
    /// payment.
    ///
    /// **The disposition that wins is the one belonging to this exact
    /// incarnation, whichever map holds it.** An operator can close a
    /// charge while it is still the live record under its key — that is
    /// the ordinary exit for an exposed refusal — and a settlement of
    /// *that same incarnation* landing afterwards must not become a
    /// second, unresolved reconciliation item for one charge an operator
    /// already accounted for. So the archive entry this verb creates is
    /// seeded from that live disposition and the late evidence merged
    /// into it by the same precedence an archive-resident disposition
    /// obeys; the live record is never touched from here.
    ///
    /// Returns the record as the archive now holds it — which is not
    /// necessarily the state just offered.
    pub async fn retain_superseded(
        &self,
        attempt: &PurchaseAttempt,
        state: PurchaseState,
        now_ns: u64,
    ) -> Result<PurchaseAttempt, StoreError> {
        let id = superseded_record_id(&attempt.key, &attempt.generation);
        let mut incoming = attempt.clone();
        incoming.state = state;
        incoming.updated_at_ns = now_ns;
        let id_owned = id.clone();
        let live_id = attempt.key.id();
        let incarnation = attempt.generation.clone();
        let stored = mutate_json_if_changed::<A2aPurchaseFile, _, _>(&self.path, move |file| {
            // Read out before the archive is borrowed mutably: the
            // disposition of this exact incarnation, when it is the
            // live record under the key rather than an archived one.
            let disposition = file
                .attempts
                .get(&live_id)
                .filter(|live| {
                    live.generation == incarnation && live.state.tag() == StateTag::Resolved
                })
                .cloned();
            let archive = RecordClass::Retained.map(file);
            if let Some(existing) = archive.get_mut(&id_owned) {
                // An archive row already exists for this incarnation. It
                // does NOT follow that the archive is where the decision
                // lives: the operator may have resolved the live row under
                // the same key and generation, and which of the two the
                // late evidence meets is only an ordering accident —
                // whether archive creation or live resolution happened
                // first. So the disposition governs from whichever map
                // holds it, and this branch consumes it exactly as the
                // archive-absent one below does.
                //
                // An archive that has its OWN terminal disposition is left
                // to `merge_retained`, which already applies archive-local
                // precedence — a separately resolved archive is not
                // overwritten by a live one.
                let inherit = existing.state.tag() != StateTag::Resolved;
                if let (true, Some(disposition)) = (inherit, disposition) {
                    let mut retained = disposition;
                    // The archive's own evidence first, so proof and
                    // billing already retained there survive the move, then
                    // the late outcome. Both go through the same merge, so
                    // precedence and operator-document preservation are the
                    // one implementation.
                    merge_retained(&mut retained, existing.clone());
                    merge_retained(&mut retained, incoming);
                    let changed = *existing != retained;
                    *existing = retained;
                    return (existing.clone(), changed);
                }
                let changed = merge_retained(existing, incoming);
                return (existing.clone(), changed);
            }
            let Some(disposition) = disposition else {
                let stored = incoming.clone();
                archive.insert(id_owned, incoming);
                return (stored, true);
            };
            let mut retained = disposition;
            if !merge_retained(&mut retained, incoming) {
                // This outcome knows nothing the disposition has not
                // already accounted for: nothing to retain, and no
                // second row to mint.
                return (retained, false);
            }
            let stored = retained.clone();
            archive.insert(id_owned, retained);
            (stored, true)
        })
        .await?;
        self.wake(&id);
        Ok(stored)
    }

    /// The retained superseded record for one incarnation of a key, if
    /// one was ever written.
    pub async fn superseded(
        &self,
        key: &PurchaseKey,
        generation: &AttemptGeneration,
    ) -> Result<Option<PurchaseAttempt>, StoreError> {
        let file: A2aPurchaseFile = load_json(&self.path).await?;
        Ok(RecordClass::Retained
            .records(&file)
            .get(&superseded_record_id(key, generation))
            .cloned())
    }

    /// Move a retained superseded record, by the same table every other
    /// transition obeys.
    pub async fn transition_superseded(
        &self,
        key: &PurchaseKey,
        generation: &AttemptGeneration,
        from: &[StateTag],
        to: PurchaseState,
        now_ns: u64,
    ) -> Result<PurchaseAttempt, PurchaseError> {
        self.cas_at(
            RecordClass::Retained.at(superseded_record_id(key, generation)),
            from,
            CasGuard::None,
            to,
            now_ns,
            |_| {},
        )
        .await
    }

    /// Drop records that have finished and aged out, in both classes.
    ///
    /// Only `Submitted`, `Resolved`, `RefusedUnexposed`, and an
    /// abandoned `Preparing` lease follow ordinary retention. The
    /// unresolved-financial classes — `Paying`, `Unknown`,
    /// `RefusedExposed`, `PaidUnexecutable`, and a `Paid` attempt that
    /// was never submitted — are **never** pruned: they are the caller's
    /// only record that money may have moved, and `Paying` is the state
    /// that holds the exact payload the provider was (or is being) asked
    /// to charge. Returns how many were removed.
    pub async fn prune(&self, now_ns: u64, retention_ns: u64) -> Result<usize, StoreError> {
        mutate_json_if_changed::<A2aPurchaseFile, _, _>(&self.path, move |file| {
            let keep = |attempt: &PurchaseAttempt| {
                attempt.is_unresolved_financial()
                    || now_ns.saturating_sub(attempt.updated_at_ns) <= retention_ns
            };
            let before = file.attempts.len() + file.superseded.len();
            file.attempts.retain(|_, attempt| keep(attempt));
            file.superseded.retain(|_, attempt| keep(attempt));
            let removed = before - file.attempts.len() - file.superseded.len();
            (removed, removed > 0)
        })
        .await
    }
}

/// A process-unique lease id. Not a secret and not a nonce: it only has
/// to distinguish this caller's claim from a concurrent or crashed one,
/// so it commits the pid, a per-process sequence, the clock, and the key.
fn mint_lease(key_id: &str, now_ns: u64) -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"net.payments.a2a.lease@1");
    hasher.update(&std::process::id().to_le_bytes());
    hasher.update(&SEQ.fetch_add(1, Ordering::Relaxed).to_le_bytes());
    hasher.update(&now_ns.to_le_bytes());
    hasher.update(key_id.as_bytes());
    hex::encode(&hasher.finalize().as_bytes()[..16])
}

/// Mint the identity of a newly created — or newly *replaced* — record.
///
/// `seq` is supplied by the caller (1 for a fresh record, one more than
/// the record being replaced otherwise) so the ordering stays readable;
/// the incarnation is unique by the same construction as
/// [`mint_lease`], which is what keeps a pruned-and-recreated key from
/// handing incarnation *n*'s identity back to a decision taken against
/// incarnation 1.
fn mint_generation(key_id: &str, now_ns: u64, seq: u64) -> AttemptGeneration {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"net.payments.a2a.generation@1");
    hasher.update(&std::process::id().to_le_bytes());
    hasher.update(&SEQ.fetch_add(1, Ordering::Relaxed).to_le_bytes());
    hasher.update(&now_ns.to_le_bytes());
    hasher.update(&seq.to_le_bytes());
    hasher.update(key_id.as_bytes());
    AttemptGeneration {
        seq,
        incarnation: hex::encode(&hasher.finalize().as_bytes()[..16]),
    }
}

/// Where the retained evidence of one superseded incarnation lives
/// **inside the archive map**.
///
/// Beside the live attempt rather than inside it: the live record is the
/// *current* purchase and must stay readable as such, while the
/// superseded charge is its own reconciliation item. Keyed by the
/// incarnation as well as the key, so two supersessions of one key never
/// overwrite each other, and unambiguously so: the incarnation is hex
/// and `seq` is decimal, so the last `#` in the id is always the
/// separator this function appended, whatever the task id spells.
///
/// It carries no isolation from the live class — an id is not a
/// namespace when part of it is an unrestricted task id. That
/// separation is [`RecordClass`]'s, and this id is only ever looked up
/// in the archive map.
fn superseded_record_id(key: &PurchaseKey, generation: &AttemptGeneration) -> String {
    format!(
        "{}#superseded/{}-{}",
        key.id(),
        generation.seq,
        generation.incarnation
    )
}

/// How far one outcome settles the question "what happened to this
/// money" — the precedence a merge into the retained-evidence archive
/// obeys.
///
/// Not an ordering of the transition table: it ranks *evidence*, which
/// is what a store has to compare when two answers about one
/// incarnation arrive out of order. A retained outcome is only replaced
/// by one that knows at least as much, so the three losses a
/// last-write-wins insert allowed are closed at the same place: a
/// sibling's ambiguity cannot erase settlement proof, a claimed refusal
/// cannot erase it either, and an operator's disposition is not
/// overwritten by a result that was already in flight when they decided.
fn evidence_rank(tag: StateTag) -> u8 {
    match tag {
        // An operator decided how this charge is accounted for. Only
        // another operator decision moves it.
        StateTag::Resolved => 3,
        // Settlement: a payment proof and a billing event exist.
        StateTag::Paid | StateTag::PaidUnexecutable | StateTag::Submitted => 2,
        // Exposed and unresolved: the authorization is out, and no
        // authoritative answer came back.
        StateTag::Paying | StateTag::Unknown | StateTag::RefusedExposed => 1,
        // Nothing was ever exposed, so nothing is claimed about money.
        StateTag::Preparing
        | StateTag::Quoted
        | StateTag::AwaitingApproval
        | StateTag::RefusedUnexposed => 0,
    }
}

/// The settlement evidence a late outcome carries, if it carries any.
///
/// `None` for an ambiguity or a refusal: those add nothing a
/// disposition has not already accounted for. `Some` only for a
/// payment proof and its billing event — the facts reconciliation
/// cannot reconstruct from the caller's store if they are dropped.
fn late_settlement_evidence(attempt: &PurchaseAttempt) -> Option<serde_json::Value> {
    let (proof, billing) = match &attempt.state {
        PurchaseState::Paid { proof, billing }
        | PurchaseState::PaidUnexecutable { proof, billing, .. } => (proof, billing),
        _ => return None,
    };
    Some(serde_json::json!({
        "at_ns": attempt.updated_at_ns,
        "generation": attempt.generation,
        "quote_id": attempt.quote_id,
        "proof": proof,
        "billing": billing,
    }))
}

/// The schema tag of the envelope a resolution's evidence grows when a
/// settlement lands after the disposition.
///
/// Versioned and namespaced because it is a read contract: a
/// reconciliation tool looks for exactly this key to find the charge an
/// operator never saw.
const LATE_SETTLEMENT_ENVELOPE: &str = "net.payments.a2a.late_settlement@1";

/// Is `needle` somewhere inside `haystack`, as an exact value?
///
/// The only question `with_late_settlement` needs to ask about a
/// document it must not otherwise interpret: has this settlement
/// already been retained here? Structural, so it finds the evidence at
/// whatever envelope depth a previous merge put it.
fn json_retains(haystack: &serde_json::Value, needle: &serde_json::Value) -> bool {
    if haystack == needle {
        return true;
    }
    match haystack {
        serde_json::Value::Object(fields) => fields.values().any(|v| json_retains(v, needle)),
        serde_json::Value::Array(items) => items.iter().any(|v| json_retains(v, needle)),
        _ => false,
    }
}

/// Put late settlement evidence *beside* what an operator recorded, in
/// an outer envelope that preserves their document byte for byte.
///
/// Operator evidence is unrestricted JSON, so there is no field this
/// function may reserve inside it: a document that already spells
/// `late_settlement` — or the envelope's own shape — owns that value,
/// and writing the generated one over it destroys the only copy of
/// something a human recorded on purpose. The operator's document is
/// therefore never inspected or edited; it moves whole into
/// `operator_evidence` beneath the envelope tag, and the generated
/// facts go beside it.
///
/// Idempotent without interpreting the document: if this exact
/// settlement value is already retained anywhere inside the evidence,
/// there is nothing to add and the evidence is returned unchanged — so
/// a re-published decision neither grows the envelope nor is mistaken
/// for new evidence.
fn with_late_settlement(
    evidence: &serde_json::Value,
    late: serde_json::Value,
) -> serde_json::Value {
    if json_retains(evidence, &late) {
        return evidence.clone();
    }
    serde_json::json!({
        LATE_SETTLEMENT_ENVELOPE: {
            "operator_evidence": evidence,
            "late_settlement": late,
        }
    })
}

/// Merge one outcome into the archived record of the same incarnation,
/// under the store lock. Returns whether the file changed.
///
/// The whole point is that this is **not** an assignment. See
/// [`evidence_rank`] for the precedence; the one case that is neither
/// "replace" nor "drop" is a settlement that arrives after an operator
/// has already resolved the record. The disposition stands — a later
/// fact does not un-decide how a human chose to account for it — but
/// the charge is real and stays findable, so the proof and billing are
/// retained in the evidence the disposition carries.
fn merge_retained(existing: &mut PurchaseAttempt, incoming: PurchaseAttempt) -> bool {
    if existing.state.tag() == StateTag::Resolved {
        let Some(late) = late_settlement_evidence(&incoming) else {
            return false;
        };
        let changed = {
            let PurchaseState::Resolved { evidence, .. } = &mut existing.state else {
                unreachable!("the tag was just matched")
            };
            let merged = with_late_settlement(evidence, late);
            if merged == *evidence {
                false
            } else {
                *evidence = merged;
                true
            }
        };
        if changed {
            existing.updated_at_ns = incoming.updated_at_ns;
        }
        return changed;
    }
    if evidence_rank(incoming.state.tag()) < evidence_rank(existing.state.tag())
        || *existing == incoming
    {
        return false;
    }
    *existing = incoming;
    true
}

/// Put the attempt back into `Preparing` under `lease_id`, clearing
/// everything the previous quote established.
///
/// Returns the quote id whose operator approval (if any) the caller must
/// clear: an approval authorizes one exact quote, and this attempt is
/// about to get a different one (boundary a).
fn take_lease(attempt: &mut PurchaseAttempt, lease_id: &str, now_ns: u64) -> Option<String> {
    let cleared = attempt.quote_id.take();
    // Replacing the record mints a new incarnation: every decision taken
    // against the old quote — a signer still running, a refusal still in
    // flight — is now unpublishable here, by identity rather than by
    // whatever state tag it happens to find.
    attempt.generation = mint_generation(
        &attempt.key.id(),
        now_ns,
        attempt.generation.seq.saturating_add(1),
    );
    attempt.prepared = None;
    attempt.quote_bytes = None;
    attempt.quote_expires_at_ns = None;
    attempt.payload_bytes = None;
    attempt.state = PurchaseState::Preparing {
        lease_id: lease_id.to_string(),
        since_ns: now_ns,
    };
    attempt.updated_at_ns = now_ns;
    cleared
}

// ---------------------------------------------------------------------------
// The provider boundary for task admission
// ---------------------------------------------------------------------------

/// The A2A provider boundary: prepare a task, and submit a paid one.
///
/// The same seam [`super::ProviderChannel`] is for payments — the mesh
/// implementation ([`MeshA2aChannel`]) is the only production one, and
/// it is a thin delegation to the SDK's requester verbs. Tests script
/// this trait to drive fault injection and barriers without a mesh.
#[async_trait::async_trait]
pub trait A2aProviderChannel: Send + Sync {
    /// `net.a2a.prepare` — uncharged, and it must run before any money
    /// moves: the reservation carries the provider-minted `admission_id`
    /// the purchase hash is computed from.
    async fn prepare(&self, node: u64, brief: &TaskBrief) -> Result<PrepareReply, A2aFlowError>;

    /// `net.a2a.task` with the payment evidence on the headers.
    async fn submit(
        &self,
        prepared: &PreparedTask,
        proof: &TaskPaymentProof,
    ) -> Result<TaskAck, A2aFlowError>;
}

/// The mesh implementation: `Mesh::prepare_a2a` and
/// `Mesh::submit_task_paid`, composed, never re-implemented.
pub struct MeshA2aChannel {
    mesh: Arc<Mesh>,
}

impl MeshA2aChannel {
    pub fn new(mesh: Arc<Mesh>) -> Self {
        Self { mesh }
    }
}

#[async_trait::async_trait]
impl A2aProviderChannel for MeshA2aChannel {
    async fn prepare(&self, node: u64, brief: &TaskBrief) -> Result<PrepareReply, A2aFlowError> {
        self.mesh.prepare_a2a(node, brief).await
    }

    async fn submit(
        &self,
        prepared: &PreparedTask,
        proof: &TaskPaymentProof,
    ) -> Result<TaskAck, A2aFlowError> {
        self.mesh.submit_task_paid(prepared, proof).await
    }
}

// ---------------------------------------------------------------------------
// Flow outcomes
// ---------------------------------------------------------------------------

/// Why a prepare did not produce a reservation this caller can buy.
///
/// Every arm is a distinct thing to do next, which is why they are not
/// one string: `Busy` and `InFlight` are retries, `Existing` and
/// `Reconciliation` are status reads, `Retired`, `Rejected` and
/// `BriefTooLarge` are dead ends, and `Conflict` needs an operator.
#[derive(Debug, thiserror::Error)]
pub enum A2aPrepareError {
    /// Transport or decode failure talking to the provider. Nothing was
    /// reserved and nothing was quoted.
    ///
    /// **Ambiguous by nature and therefore retryable** — which is why a
    /// local validation refusal must never be folded in here (see
    /// [`Self::BriefTooLarge`]).
    #[error("a2a prepare transport failed: {0}")]
    Transport(String),
    /// The brief exceeds the A2A envelope bound and was refused
    /// **locally**: no packet left this process, nothing was reserved,
    /// nothing was quoted.
    ///
    /// Its own arm rather than [`Self::Transport`] because the two have
    /// opposite recovery postures and a caller (or a binding) can only
    /// tell them apart by type. This is a permanent local validation
    /// error: the same brief will be refused identically forever, so it
    /// is neither retryable nor re-quotable — the work has to be moved
    /// into a context artifact ref and the brief rebuilt.
    #[error(
        "brief is {encoded} bytes, over the {limit}-byte A2A envelope bound — refused locally"
    )]
    BriefTooLarge {
        /// `TaskBrief::encode().len()`.
        encoded: usize,
        /// `net_sdk::mesh_a2a::A2A_MAX_BRIEF_BYTES`.
        limit: usize,
    },
    /// The provider refused the brief before reserving anything: unknown
    /// service, stale revision, a bound exceeded, an application
    /// preflight refusal, or this id already naming different work.
    #[error("provider rejected the brief: {reason}")]
    Rejected { reason: String },
    /// The provider is at capacity. Nothing reserved; retry.
    #[error("provider is at capacity for this service")]
    Busy,
    /// This task is already launched or finished — poll status.
    #[error("task {task_id} already exists on the provider")]
    Existing { task_id: String },
    /// A payment landed and the provider could no longer admit the work;
    /// prepare will not reopen it.
    #[error("task {task_id} is awaiting provider reconciliation")]
    Reconciliation { task_id: String },
    /// The result was retired; this id can never be bought again.
    #[error("task {task_id} has been retired by the provider")]
    Retired { task_id: String },
    /// The service is free. A free task is submitted directly — there is
    /// nothing to purchase, and minting a purchase attempt with no quote
    /// would be a record that can never be resolved.
    #[error("service {service_id} is free — submit it directly, there is nothing to purchase")]
    Unpriced { service_id: String },
    /// The provider's reservation does not describe the work this caller
    /// asked to buy (a different commitment, a foreign capability, or a
    /// purchase hash that is not the one this reservation implies).
    #[error("provider reservation does not match this purchase: {detail}")]
    ReservationMismatch { detail: String },
    /// No quote could be obtained for the reservation.
    #[error("quoting the reservation failed: {message}")]
    Quote { message: String, retryable: bool },
    /// Another caller — in this process or another — holds the prepare
    /// lease for this key. Retry.
    #[error("another caller holds the prepare lease for {key}")]
    InFlight { key: String },
    /// The stored attempt cannot be prepared again (financially
    /// unresolved or operator-closed), or it commits to different work.
    #[error(transparent)]
    Attempt(#[from] PurchaseError),
}

/// What one purchase attempt resolved to.
#[derive(Debug, Clone, PartialEq)]
pub enum A2aPurchase {
    /// Paid. `proof` is what submit presents; `billing` is the caller's
    /// own evidence (settlement ref + signed billing event).
    Paid {
        task_id: String,
        proof: TaskPaymentProof,
        billing: serde_json::Value,
    },
    /// Spend policy is holding this exact quote for an operator.
    RequiresPaymentApproval {
        quote_id: String,
        policy_reason: String,
        approve_hint: String,
    },
    /// Refused. `funds_ambiguous` is the whole point of the distinction:
    /// `false` means the refusal happened before any authorization left
    /// this process (proven no money moved, and a new prepare is
    /// allowed); `true` means a bearer authorization was exposed first,
    /// the spend reservation is held, and an operator's
    /// `resolve_attempt` is the only exit.
    Denied {
        quote_id: Option<String>,
        policy_reason: String,
        funds_ambiguous: bool,
    },
    /// The payment may or may not have landed. Recovered by calling
    /// `purchase_task` again — it re-sends the stored payload.
    Unknown { quote_id: String },
    /// Could not get far enough to have an opinion about the money —
    /// with one exception the `message` always names: a purchase whose
    /// intent key was replaced while it was in flight *did* have an
    /// outcome, and that outcome is retained as a superseded attempt for
    /// an operator to close. Such a failure is never `retryable`,
    /// because re-sending cannot help and buying again would be a second
    /// charge.
    Failed {
        quote_id: Option<String>,
        message: String,
        retryable: bool,
    },
}

/// What one submit of a paid attempt resolved to.
#[derive(Debug, Clone, PartialEq)]
pub enum A2aSubmit {
    /// The provider accepted the brief; the task is running.
    Accepted { task_id: String },
    /// A retryable refusal — the attempt stays `Paid` and the *same*
    /// proof is resubmitted. Never re-purchase on this.
    Retry { message: String },
    /// The provider will not execute this purchase. The attempt is
    /// `PaidUnexecutable` with its evidence retained; an operator's
    /// `resolve_attempt` is the exit.
    Unexecutable { refusal: RefusalRecord },
}

/// How an operator closes an attempt the automatic path cannot.
#[derive(Debug, Clone)]
pub enum AttemptResolution {
    /// The operator established that the payment landed (`Unknown` →
    /// `Paid`), supplying the evidence they established it from.
    Paid {
        proof: TaskPaymentProof,
        billing: serde_json::Value,
    },
    /// The operator established that no money moved (`Unknown` →
    /// `RefusedUnexposed`), which re-opens the key for a fresh prepare.
    NotPaid { reason: String },
    /// The operator closed an ambiguous or unexecutable attempt —
    /// refunded, written off, or executed elsewhere. The evidence is
    /// kept.
    Closed {
        outcome: String,
        evidence: serde_json::Value,
    },
}

// ---------------------------------------------------------------------------
// The flow
// ---------------------------------------------------------------------------

/// The caller-side paid-A2A flow: prepare, purchase, submit — over one
/// durable attempt per intent key.
///
/// Composed, not re-implemented: the payment lifecycle is
/// [`CallerPaymentFlow`]'s staged verbs, the provider verbs are the
/// SDK's, and this type owns exactly one thing — which attempt is
/// authoritative and what may happen to it next.
pub struct A2aCallerFlow {
    payments: Arc<CallerPaymentFlow>,
    tasks: Arc<dyn A2aProviderChannel>,
    store: Arc<A2aPurchaseStore>,
    clock: Arc<dyn Clock>,
}

impl A2aCallerFlow {
    pub fn new(
        payments: Arc<CallerPaymentFlow>,
        tasks: Arc<dyn A2aProviderChannel>,
        store: Arc<A2aPurchaseStore>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            payments,
            tasks,
            store,
            clock,
        }
    }

    /// The intent key for one task on one provider, under this caller.
    pub fn key(&self, provider_node: u64, task_id: &str) -> PurchaseKey {
        PurchaseKey::new(self.payments.caller(), provider_node, task_id)
    }

    /// The stored attempt for one task, if any.
    pub async fn stored_attempt(
        &self,
        provider_node: u64,
        task_id: &str,
    ) -> Result<Option<PurchaseAttempt>, StoreError> {
        self.store.attempt(&self.key(provider_node, task_id)).await
    }

    /// Every stored record — the operator's queue: the live attempts and
    /// the retained evidence of superseded incarnations. Each carries
    /// the `key` and `generation` that address it, which is what an
    /// operator (or a binding) reads back to name one exactly.
    pub async fn attempts(&self) -> Result<Vec<PurchaseAttempt>, StoreError> {
        self.store.attempts().await
    }

    /// Only the retained evidence of superseded incarnations — the rows
    /// that close through [`Self::resolve_superseded_attempt`] rather
    /// than [`Self::resolve_attempt`]. One key can hold both a live
    /// attempt and retained charges; this is how a queue tells them
    /// apart without guessing.
    pub async fn retained_attempts(&self) -> Result<Vec<PurchaseAttempt>, StoreError> {
        self.store.retained_attempts().await
    }

    /// **Read-only on the money side.** Validate the brief with the
    /// provider, reserve capacity, and obtain a quote bound to that exact
    /// reservation. No spend reservation, no payment — this is what makes
    /// it safe to call merely to display a price.
    ///
    /// Exactly one attempt per key does the work: a concurrent caller
    /// that finds a live `Preparing` lease awaits it and returns the same
    /// [`PreparedTask`], and one that finds a live quote returns it
    /// without asking for another. The quote channel is called **once
    /// per key** for as long as the quote lives.
    ///
    /// Re-preparing is legitimate only from a state where nothing was
    /// exposed: a quote that expired unpaid, or a `RefusedUnexposed`
    /// attempt. Both mint a new quote under the *same* `admission_id`
    /// (the provider's prepare is idempotent), so spend policy and
    /// operator approval run again — the stale approval hold is cleared
    /// here, not carried over (boundary a).
    pub async fn prepare_task(
        &self,
        provider_node: u64,
        offer: &A2aOffer,
        brief: &TaskBrief,
    ) -> Result<PreparedTask, A2aPrepareError> {
        if brief.service.as_deref() != Some(offer.service_id.as_str())
            || brief.revision.as_deref() != Some(offer.revision.as_str())
        {
            return Err(A2aPrepareError::Rejected {
                reason: format!(
                    "brief names service {:?}/{:?} but this offer is {:?}/{:?}",
                    brief.service, brief.revision, offer.service_id, offer.revision
                ),
            });
        }
        let Some(pricing_terms) = offer.pricing_terms.clone() else {
            return Err(A2aPrepareError::Unpriced {
                service_id: offer.service_id.clone(),
            });
        };
        let commitment = task_commitment(offer, brief);
        let key = self.key(provider_node, &brief.task_id);
        let key_id = key.id();
        let waiter = self.store.waiter(&key_id);

        let mut steps = 0u32;
        let (lease_id, cleared_approval) = loop {
            // Register for the wake BEFORE reading the store, so a
            // sibling that commits between the read and the wait cannot
            // leave this caller parked on a notification already sent.
            let notified = waiter.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            match self
                .store
                .begin_prepare(&key, &commitment, self.clock.now_ns())
                .await?
            {
                PrepareClaim::Leased {
                    lease_id,
                    cleared_approval,
                } => break (lease_id, cleared_approval),
                PrepareClaim::Ready(attempt) => {
                    return attempt.prepared.ok_or(A2aPrepareError::InFlight {
                        key: key_id.clone(),
                    })
                }
                PrepareClaim::Awaiting => {
                    if steps >= MAX_WAIT_STEPS {
                        return Err(A2aPrepareError::InFlight { key: key_id });
                    }
                    steps += 1;
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_millis(WAIT_STEP_MS),
                        notified,
                    )
                    .await;
                }
            }
        };

        // An approval is for one exact quote. This prepare is minting a
        // new one, so the hold the previous quote was carrying is dead
        // and must not be able to authorize the next purchase.
        if let Some(stale) = cleared_approval {
            self.payments.clear_approval(&stale).await;
        }

        let prepared = match self
            .drive_prepare(provider_node, offer, brief, &commitment)
            .await
        {
            Ok(prepared) => prepared,
            Err(e) => {
                // Nothing financial exists yet, so the record goes away
                // entirely rather than lingering as a half-attempt.
                let _ = self.store.discard(&key, &lease_id).await;
                return Err(e);
            }
        };

        let input_hash = prepared.reservation.purchase_hash.clone();
        let bound = match self
            .payments
            .quote_bound(
                &prepared.reservation.capability,
                &pricing_terms,
                Some(&input_hash),
            )
            .await
        {
            Ok(bound) => bound,
            Err(decision) => {
                let _ = self.store.discard(&key, &lease_id).await;
                let (message, retryable) = match decision {
                    CallerDecision::Denied { policy_reason } => (policy_reason, false),
                    CallerDecision::Failed {
                        message, retryable, ..
                    } => (message, retryable),
                    other => (format!("unexpected quote outcome: {other:?}"), false),
                };
                return Err(A2aPrepareError::Quote { message, retryable });
            }
        };

        let quote_id = bound.quote.quote_id.clone();
        let expires_at_ns = bound.quote.expires_at_ns;
        let quote_bytes = bound.quote_bytes.clone();
        let stored = prepared.clone();
        self.store
            .cas(
                &key,
                &[StateTag::Preparing],
                // The lease id *is* this phase's identity: it is minted
                // per claim, so a replacement record (which mints its
                // own) cannot accept this write. Nothing financial
                // exists to bind a quote id to yet.
                CasGuard::Lease(&lease_id),
                PurchaseState::Quoted,
                self.clock.now_ns(),
                move |attempt| {
                    attempt.prepared = Some(stored);
                    attempt.quote_bytes = Some(quote_bytes);
                    attempt.quote_id = Some(quote_id);
                    attempt.quote_expires_at_ns = Some(expires_at_ns);
                },
            )
            .await?;
        Ok(prepared)
    }

    /// P1–P5 at the provider, plus the checks that make the reply
    /// trustworthy: the reservation must admit *this* commitment, its
    /// capability must be this node's `net.a2a.task/{service}`, and its
    /// purchase hash must be the one this reservation implies — computed
    /// here, never taken from the reply.
    async fn drive_prepare(
        &self,
        provider_node: u64,
        offer: &A2aOffer,
        brief: &TaskBrief,
        commitment: &str,
    ) -> Result<PreparedTask, A2aPrepareError> {
        let reply = self
            .tasks
            .prepare(provider_node, brief)
            .await
            // A local bound refusal is not a transport outcome: nothing
            // was sent, and calling it `Transport` told every caller
            // above this one to retry a brief that can never fit.
            .map_err(|e| match e {
                A2aFlowError::BriefTooLarge { encoded, limit } => {
                    A2aPrepareError::BriefTooLarge { encoded, limit }
                }
                other => A2aPrepareError::Transport(other.to_string()),
            })?;
        let reservation = match reply {
            PrepareReply::Reservation(reservation) => reservation,
            PrepareReply::Existing { task_id } => {
                return Err(A2aPrepareError::Existing { task_id })
            }
            PrepareReply::Reconciliation { task_id } => {
                return Err(A2aPrepareError::Reconciliation { task_id })
            }
            PrepareReply::Retired { task_id } => return Err(A2aPrepareError::Retired { task_id }),
            PrepareReply::Busy => return Err(A2aPrepareError::Busy),
            PrepareReply::Rejected { reason } => return Err(A2aPrepareError::Rejected { reason }),
        };
        if reservation.task_id != brief.task_id {
            return Err(A2aPrepareError::ReservationMismatch {
                detail: format!(
                    "reservation is for task {:?}, this brief is {:?}",
                    reservation.task_id, brief.task_id
                ),
            });
        }
        if reservation.commitment != commitment {
            return Err(A2aPrepareError::ReservationMismatch {
                detail: "the provider admitted a different commitment than this offer + brief"
                    .to_string(),
            });
        }
        let expected_hash = purchase_hash(&reservation.admission_id, commitment);
        if reservation.purchase_hash != expected_hash {
            return Err(A2aPrepareError::ReservationMismatch {
                detail: "the reservation's purchase hash is not the one its admission id and \
                         commitment imply"
                    .to_string(),
            });
        }
        let expected_tail = format!("/{A2A_TASK_SERVICE}/{}", offer.service_id);
        let capability_node = MeshPaymentChannel::provider_node(&reservation.capability)
            .map_err(|e| A2aPrepareError::ReservationMismatch { detail: e.message })?;
        if capability_node != provider_node || !reservation.capability.ends_with(&expected_tail) {
            return Err(A2aPrepareError::ReservationMismatch {
                detail: format!(
                    "reservation capability {:?} is not node {provider_node}'s \
                     {A2A_TASK_SERVICE}/{}",
                    reservation.capability, offer.service_id
                ),
            });
        }
        Ok(PreparedTask {
            provider_node,
            brief: brief.clone(),
            offer_hash: offer.hash(),
            reservation,
        })
    }

    /// Buy the prepared task, using **the stored attempt's quote and
    /// payload** — never fresh ones.
    ///
    /// The first caller to CAS `Quoted → Paying` authors the payload and
    /// persists it in that same write, *before* the pay call. A
    /// concurrent caller that finds `Paying` re-sends the byte-identical
    /// stored payload: the provider's acceptance is payload-idempotent,
    /// so it returns the original verdict rather than charging again. An
    /// attempt left `Unknown` by a lost reply recovers the same way —
    /// automatically, and only through the stored payment (boundary b).
    pub async fn purchase_task(&self, provider_node: u64, task_id: &str) -> A2aPurchase {
        let key = self.key(provider_node, task_id);
        let key_id = key.id();
        let waiter = self.store.waiter(&key_id);
        let mut steps = 0u32;
        loop {
            let notified = waiter.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let attempt = match self.store.attempt(&key).await {
                Ok(Some(attempt)) => attempt,
                Ok(None) => {
                    return A2aPurchase::Failed {
                        quote_id: None,
                        message: format!("no prepared purchase for task {task_id}; prepare first"),
                        retryable: false,
                    }
                }
                Err(e) => {
                    return A2aPurchase::Failed {
                        quote_id: None,
                        message: e.to_string(),
                        retryable: true,
                    }
                }
            };
            match &attempt.state {
                PurchaseState::Quoted => return self.buy(&key, attempt, &[StateTag::Quoted]).await,
                PurchaseState::AwaitingApproval => {
                    return self.resume_approval(&key, attempt).await
                }
                PurchaseState::Paying { since_ns, .. } => {
                    let claim_live =
                        self.clock.now_ns().saturating_sub(*since_ns) <= PREPARE_LEASE_NS;
                    // A live claim is somebody's payment in flight. Wait
                    // for its verdict rather than re-sending underneath
                    // it: the engine would answer a concurrent duplicate
                    // `in_flight` and this caller would record an
                    // ambiguity that never existed. Only once the claim
                    // is stale (or the wait is spent — the holder died
                    // without updating it) is re-sending the stored
                    // payload the right move, and then it is the
                    // lost-reply recovery, byte for byte.
                    if claim_live && steps < MAX_WAIT_STEPS {
                        steps += 1;
                        let _ = tokio::time::timeout(
                            std::time::Duration::from_millis(WAIT_STEP_MS),
                            notified,
                        )
                        .await;
                        continue;
                    }
                    if attempt.payload_bytes.is_some() {
                        return self.settle(&key, attempt, &[StateTag::Paying]).await;
                    }
                    // Claimed, stale, and the payload never landed:
                    // authoring again is safe because nothing was ever
                    // sent under this claim.
                    if !claim_live {
                        return self.buy(&key, attempt, &[StateTag::Paying]).await;
                    }
                    return A2aPurchase::Failed {
                        quote_id: attempt.quote_id.clone(),
                        message: "another caller is authoring this purchase".to_string(),
                        retryable: true,
                    };
                }
                PurchaseState::Unknown { .. } => {
                    if attempt.payload_bytes.is_some() {
                        return self.settle(&key, attempt, &[StateTag::Unknown]).await;
                    }
                    return A2aPurchase::Unknown {
                        quote_id: attempt.quote_id.clone().unwrap_or_default(),
                    };
                }
                PurchaseState::Paid { proof, billing } => {
                    return A2aPurchase::Paid {
                        task_id: task_id.to_string(),
                        proof: proof.clone(),
                        billing: billing.clone(),
                    }
                }
                PurchaseState::Submitted { task_id: submitted } => {
                    return A2aPurchase::Failed {
                        quote_id: attempt.quote_id.clone(),
                        message: format!("task {submitted} was already submitted"),
                        retryable: false,
                    }
                }
                PurchaseState::RefusedUnexposed { reason } => {
                    return A2aPurchase::Denied {
                        quote_id: attempt.quote_id.clone(),
                        policy_reason: reason.clone(),
                        funds_ambiguous: false,
                    }
                }
                PurchaseState::RefusedExposed {
                    reason,
                    reservation_kept,
                } => {
                    return A2aPurchase::Denied {
                        quote_id: attempt.quote_id.clone(),
                        policy_reason: reason.clone(),
                        funds_ambiguous: *reservation_kept,
                    }
                }
                PurchaseState::PaidUnexecutable { refusal, .. } => {
                    return A2aPurchase::Failed {
                        quote_id: attempt.quote_id.clone(),
                        message: format!(
                            "this purchase was paid and the provider will not execute it \
                             ({}); operator resolution is the exit",
                            refusal.message
                        ),
                        retryable: false,
                    }
                }
                PurchaseState::Resolved { outcome, .. } => {
                    return A2aPurchase::Failed {
                        quote_id: attempt.quote_id.clone(),
                        message: format!("this attempt was resolved: {outcome}"),
                        retryable: false,
                    }
                }
                PurchaseState::Preparing { .. } => {
                    if steps >= MAX_WAIT_STEPS {
                        return A2aPurchase::Failed {
                            quote_id: None,
                            message: "the purchase is still being prepared".to_string(),
                            retryable: true,
                        };
                    }
                    steps += 1;
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_millis(WAIT_STEP_MS),
                        notified,
                    )
                    .await;
                }
            }
        }
    }

    /// `Quoted` (or a stale `Paying`) → spend policy → author → persist
    /// the payload → pay. The spend decision is taken *before* the claim,
    /// so a loser of the claim race gives its holder back rather than
    /// leaving budget reserved twice for one purchase.
    async fn buy(
        &self,
        key: &PurchaseKey,
        attempt: PurchaseAttempt,
        from: &[StateTag],
    ) -> A2aPurchase {
        let Some(quote_bytes) = attempt.quote_bytes.clone() else {
            return A2aPurchase::Failed {
                quote_id: attempt.quote_id.clone(),
                message: "the stored attempt carries no quote; prepare again".to_string(),
                retryable: false,
            };
        };
        let quote = match PaymentQuote::from_json_bytes(&quote_bytes) {
            Ok(quote) => quote,
            Err(e) => {
                return A2aPurchase::Failed {
                    quote_id: attempt.quote_id.clone(),
                    message: format!("stored quote failed verification: {e}"),
                    retryable: false,
                }
            }
        };
        // Everything below this line happens *after* an await — spend
        // policy, an operator's approval, a wallet signing a bearer
        // authorization — and every one of those writes re-presents this
        // identity. A state tag cannot tell this attempt from its
        // replacement; the incarnation and the exact quote can.
        let identity = attempt.identity();
        // Expiry is checked here, before anything is authored: an
        // expired quote cannot be paid, and refusing it *unexposed* is
        // what leaves the key open for a re-prepare (the D4 row) instead
        // of stranding it as financially ambiguous.
        if quote.is_expired_at(self.clock.now_ns()) {
            return self
                .refuse_unexposed(
                    key,
                    &attempt,
                    from,
                    "the stored quote expired before it was paid; prepare again".to_string(),
                )
                .await;
        }

        match self.payments.reserve_spend(&quote).await {
            Ok(SpendDecision::Allowed) => {}
            Ok(SpendDecision::RequiresPaymentApproval {
                quote_id,
                policy_reason,
                approve_hint,
            }) => {
                // Already parked for this exact approval: the state is
                // where it belongs and `AwaitingApproval → AwaitingApproval`
                // is deliberately not a table row.
                if !from.contains(&StateTag::AwaitingApproval) {
                    if let Err(e) = self
                        .store
                        .transition_exact(
                            key,
                            from,
                            &identity,
                            PurchaseState::AwaitingApproval,
                            self.clock.now_ns(),
                        )
                        .await
                    {
                        return A2aPurchase::Failed {
                            quote_id: Some(quote_id),
                            message: e.to_string(),
                            retryable: true,
                        };
                    }
                }
                return A2aPurchase::RequiresPaymentApproval {
                    quote_id,
                    policy_reason,
                    approve_hint,
                };
            }
            Ok(SpendDecision::Denied { policy_reason }) => {
                return self
                    .refuse_unexposed(key, &attempt, from, policy_reason)
                    .await
            }
            Err(e) => {
                return A2aPurchase::Failed {
                    quote_id: attempt.quote_id.clone(),
                    message: e.to_string(),
                    retryable: false,
                }
            }
        }

        let payload = match self.payments.author(&quote).await {
            Ok(payload) => payload,
            Err(message) => {
                // The payload never left this process; give the
                // reservation back and record a proven-unexposed refusal.
                self.payments.release_spend(&quote).await;
                return self.refuse_unexposed(key, &attempt, from, message).await;
            }
        };
        let payload_bytes = payload.bytes().to_vec();
        let lease_id = mint_lease(&key.id(), self.clock.now_ns());
        let stored_payload = payload_bytes.clone();
        let claimed = self
            .store
            .cas(
                key,
                from,
                CasGuard::Identity(&identity),
                PurchaseState::Paying {
                    lease_id,
                    since_ns: self.clock.now_ns(),
                },
                self.clock.now_ns(),
                move |attempt| attempt.payload_bytes = Some(stored_payload),
            )
            .await;
        match claimed {
            Ok(claimed) => self.settle(key, claimed, &[StateTag::Paying]).await,
            Err(PurchaseError::Superseded {
                expected, found, ..
            }) => {
                // The record this authorization was decided against is
                // gone: a replacement was minted while the wallet was
                // signing. Nothing was sent, so nothing is ambiguous —
                // hand the reservation back and let the caller decide
                // again against whatever the key holds now. The one
                // thing that must not happen is this payload landing on
                // the replacement, whose quote nobody authorized.
                self.payments.release_spend(&quote).await;
                A2aPurchase::Failed {
                    quote_id: attempt.quote_id.clone(),
                    message: format!(
                        "this purchase was authorized for {expected}, and the key now holds \
                         {found}; a replacement quote needs its own authorization"
                    ),
                    retryable: true,
                }
            }
            Err(PurchaseError::Conflict { .. }) | Err(PurchaseError::LeaseLost { .. }) => {
                // A sibling claimed it first. Hand back this attempt's
                // holder of the reservation and converge on the stored
                // payload — never send a second distinct one.
                self.payments.release_spend(&quote).await;
                match self.store.attempt(key).await {
                    Ok(Some(current)) if current.payload_bytes.is_some() => {
                        self.settle(key, current, &[StateTag::Paying, StateTag::Unknown])
                            .await
                    }
                    _ => A2aPurchase::Failed {
                        quote_id: attempt.quote_id.clone(),
                        message: "another caller claimed this purchase".to_string(),
                        retryable: true,
                    },
                }
            }
            Err(e) => A2aPurchase::Failed {
                quote_id: attempt.quote_id.clone(),
                message: e.to_string(),
                retryable: true,
            },
        }
    }

    /// `AwaitingApproval` → did the operator decide?
    ///
    /// An approval that is gone was *rejected* — that is a proven
    /// non-payment, so the attempt becomes `RefusedUnexposed` and the key
    /// may be prepared again. A still-pending hold is reported back
    /// unchanged; nothing re-quotes.
    async fn resume_approval(&self, key: &PurchaseKey, attempt: PurchaseAttempt) -> A2aPurchase {
        let Some(quote_id) = attempt.quote_id.clone() else {
            return A2aPurchase::Failed {
                quote_id: None,
                message: "the stored attempt carries no quote id".to_string(),
                retryable: false,
            };
        };
        match self.payments.approval_state(&quote_id).await {
            Ok(ApprovalOutcome::Approved) => {
                self.buy(key, attempt, &[StateTag::AwaitingApproval]).await
            }
            Ok(ApprovalOutcome::Pending) => A2aPurchase::RequiresPaymentApproval {
                quote_id: quote_id.clone(),
                policy_reason: "this purchase is held for operator approval".to_string(),
                approve_hint: format!(
                    "approve quote {quote_id} via the payments consent API (operator surface)"
                ),
            },
            Ok(ApprovalOutcome::Gone) => {
                self.refuse_unexposed(
                    key,
                    &attempt,
                    &[StateTag::AwaitingApproval],
                    "the operator rejected this payment".to_string(),
                )
                .await
            }
            Err(e) => A2aPurchase::Failed {
                quote_id: Some(quote_id),
                message: e.to_string(),
                retryable: true,
            },
        }
    }

    /// Record a refusal that happened before any authorization left this
    /// process. Proven non-payment: ordinary retention, and the key may
    /// be prepared again.
    async fn refuse_unexposed(
        &self,
        key: &PurchaseKey,
        attempt: &PurchaseAttempt,
        from: &[StateTag],
        reason: String,
    ) -> A2aPurchase {
        if let Err(e) = self
            .store
            .transition_exact(
                key,
                from,
                // Nothing left the process, so there is no financial
                // evidence to retain — but this refusal still may not
                // land on a record it was not decided about.
                &attempt.identity(),
                PurchaseState::RefusedUnexposed {
                    reason: reason.clone(),
                },
                self.clock.now_ns(),
            )
            .await
        {
            return A2aPurchase::Failed {
                quote_id: attempt.quote_id.clone(),
                message: e.to_string(),
                retryable: true,
            };
        }
        A2aPurchase::Denied {
            quote_id: attempt.quote_id.clone(),
            policy_reason: reason,
            funds_ambiguous: false,
        }
    }

    /// Send the stored payload for the stored quote and record what came
    /// back. The only place a payment leaves this flow.
    async fn settle(
        &self,
        key: &PurchaseKey,
        attempt: PurchaseAttempt,
        from: &[StateTag],
    ) -> A2aPurchase {
        let (Some(quote_bytes), Some(payload_bytes)) =
            (attempt.quote_bytes.clone(), attempt.payload_bytes.clone())
        else {
            return A2aPurchase::Failed {
                quote_id: attempt.quote_id.clone(),
                message: "the stored attempt is missing its quote or payload".to_string(),
                retryable: false,
            };
        };
        let quote = match PaymentQuote::from_json_bytes(&quote_bytes) {
            Ok(quote) => quote,
            Err(e) => {
                return A2aPurchase::Failed {
                    quote_id: attempt.quote_id.clone(),
                    message: format!("stored quote failed verification: {e}"),
                    retryable: false,
                }
            }
        };
        let decision = self.payments.pay_exact(&quote_bytes, &payload_bytes).await;
        let now_ns = self.clock.now_ns();
        // The payload is exposed now, so every write below publishes the
        // result of an await and must prove it is writing to the record
        // the payment was sent for.
        let identity = attempt.identity();
        match decision {
            CallerDecision::Paid {
                quote_id,
                binding_sig,
                proof,
            } => {
                // A public-only caller identity cannot sign the binding.
                // The payment still landed, so the attempt is Paid — and
                // submit will be refused `binding_required`, which is
                // exactly the `PaidUnexecutable` path, not a relabel of
                // the payment.
                let task_proof = TaskPaymentProof {
                    quote_id: quote_id.clone(),
                    binding_sig: binding_sig.unwrap_or_default(),
                };
                let landed = self
                    .store
                    .cas(
                        key,
                        // Authoritative success is publishable from the
                        // claim it was sent under *or* from an ambiguity
                        // a sibling published while it was in flight:
                        // evidence of this purchase only ever improves.
                        // What makes that safe is the identity — same
                        // incarnation, same quote — not the tag, and it
                        // is what keeps a duplicate's `Unknown` from
                        // making a later success unrepresentable or
                        // costing a third round trip to recover.
                        &[StateTag::Paying, StateTag::Unknown],
                        CasGuard::Identity(&identity),
                        PurchaseState::Paid {
                            proof: task_proof.clone(),
                            billing: proof.clone(),
                        },
                        now_ns,
                        |_| {},
                    )
                    .await;
                match landed {
                    Ok(_) => {
                        // The human's approval was for this exact quote
                        // and is consumed by the payment it authorized.
                        self.payments.clear_approval(&quote_id).await;
                        A2aPurchase::Paid {
                            task_id: key.task_id.clone(),
                            proof: task_proof,
                            billing: proof,
                        }
                    }
                    // A store I/O failure decided nothing: the record is
                    // untouched and the stored payload still recovers
                    // this purchase, so this is the one arm that stays a
                    // retryable error.
                    Err(PurchaseError::Store(e)) => A2aPurchase::Failed {
                        quote_id: Some(quote_id),
                        message: format!(
                            "the payment landed but recording it failed ({e}); the stored \
                             attempt is still resumable"
                        ),
                        retryable: true,
                    },
                    Err(e) => {
                        // The live record will not accept this success:
                        // the incarnation was replaced, the row is gone,
                        // or a sibling published a verdict — an exposed
                        // refusal, an operator's closure — that a
                        // settlement is not a table transition from.
                        // None of those unmakes a charge, and a weaker
                        // sibling verdict does not outrank settlement,
                        // so the proof is retained against the
                        // incarnation that bought it instead of being
                        // handed back inside a retryable error that
                        // leaves it nowhere.
                        self.payments.clear_approval(&quote_id).await;
                        // Unless the live record already holds this very
                        // purchase's own outcome — same incarnation,
                        // same quote, and the engine answers one payload
                        // once — in which case the success is recorded
                        // there and a retained copy would invent a
                        // second reconciliation item for one payment.
                        if let Ok(Some(live)) = self.store.attempt(key).await {
                            if live.identity() == identity {
                                match live.state {
                                    PurchaseState::Paid { proof, billing } => {
                                        return A2aPurchase::Paid {
                                            task_id: key.task_id.clone(),
                                            proof,
                                            billing,
                                        };
                                    }
                                    PurchaseState::Submitted { .. }
                                    | PurchaseState::PaidUnexecutable { .. } => {
                                        return A2aPurchase::Failed {
                                            quote_id: Some(quote_id),
                                            message: format!(
                                                "this payment settled and its attempt has \
                                                 already moved on ({})",
                                                live.state.tag()
                                            ),
                                            retryable: false,
                                        };
                                    }
                                    _ => {}
                                }
                            }
                        }
                        self.publish_superseded(
                            &attempt,
                            PurchaseState::PaidUnexecutable {
                                proof: task_proof,
                                billing: proof,
                                refusal: RefusalRecord {
                                    at_ns: now_ns,
                                    message: format!(
                                        "this purchase settled and the live attempt under its \
                                         key did not accept the result ({e})"
                                    ),
                                    reason: Some(SUPERSEDED_REFUSAL_REASON.to_string()),
                                    safe_to_retry: false,
                                    safe_to_requote: false,
                                },
                            },
                            "this payment settled",
                            now_ns,
                        )
                        .await
                    }
                }
            }
            CallerDecision::Failed {
                quote_id,
                message,
                retryable: true,
            } => {
                // Ambiguous: the payment MAY have landed. The reservation
                // stays, the payload stays, and the next purchase_task
                // re-sends it.
                let published = self
                    .store
                    .transition_exact(
                        key,
                        from,
                        &identity,
                        PurchaseState::Unknown {
                            last_error: message.clone(),
                        },
                        now_ns,
                    )
                    .await;
                match published {
                    Err(PurchaseError::Superseded { .. } | PurchaseError::Missing { .. }) => {
                        // Exposed and unresolved, against a record that
                        // is gone: the ambiguity is evidence too.
                        self.publish_superseded(
                            &attempt,
                            PurchaseState::Unknown {
                                last_error: message,
                            },
                            "this payment may have landed",
                            now_ns,
                        )
                        .await
                    }
                    _ => A2aPurchase::Unknown {
                        quote_id: quote_id.unwrap_or_default(),
                    },
                }
            }
            CallerDecision::Denied { policy_reason } => {
                self.refuse_exposed(key, &attempt, from, &quote, policy_reason)
                    .await
            }
            CallerDecision::Failed {
                message,
                retryable: false,
                ..
            } => {
                self.refuse_exposed(key, &attempt, from, &quote, message)
                    .await
            }
            CallerDecision::RequiresPaymentApproval { quote_id, .. } => A2aPurchase::Failed {
                quote_id: Some(quote_id),
                message: "the payment stage asked for approval after the payload was sent"
                    .to_string(),
                retryable: false,
            },
        }
    }

    /// Record a refusal that arrived **after** a bearer authorization was
    /// exposed. Not proof of non-settlement — the reservation is read
    /// back rather than inferred, so the record says what actually
    /// happened to the budget.
    async fn refuse_exposed(
        &self,
        key: &PurchaseKey,
        attempt: &PurchaseAttempt,
        from: &[StateTag],
        quote: &PaymentQuote,
        reason: String,
    ) -> A2aPurchase {
        let reservation_kept = self
            .payments
            .spend_reservation_held(&quote.quote_id)
            .await
            .unwrap_or(!reject_releases_reservation(quote));
        let exposed = PurchaseState::RefusedExposed {
            reason: reason.clone(),
            reservation_kept,
        };
        if let Err(e) = self
            .store
            .transition_exact(
                key,
                from,
                &attempt.identity(),
                exposed.clone(),
                self.clock.now_ns(),
            )
            .await
        {
            if matches!(
                e,
                PurchaseError::Superseded { .. } | PurchaseError::Missing { .. }
            ) {
                // The authorization was exposed and the record it was
                // exposed for is gone: a claimed refusal is not proof of
                // non-settlement, so this ambiguity is retained against
                // the incarnation that took the risk.
                return self
                    .publish_superseded(
                        attempt,
                        exposed,
                        &format!("this payment was refused after exposure ({reason})"),
                        self.clock.now_ns(),
                    )
                    .await;
            }
            return A2aPurchase::Failed {
                quote_id: attempt.quote_id.clone(),
                message: e.to_string(),
                retryable: true,
            };
        }
        A2aPurchase::Denied {
            quote_id: attempt.quote_id.clone(),
            policy_reason: reason,
            funds_ambiguous: reservation_kept,
        }
    }

    /// Publish a financial outcome whose record was replaced — pruned,
    /// re-prepared — while the payment was in flight.
    ///
    /// The result is retained beside the live attempt under the
    /// incarnation it belongs to, in the unresolved-financial class, so
    /// it survives retention, shows up in
    /// [`A2aCallerFlow::attempts`], and closes through
    /// [`A2aCallerFlow::resolve_superseded_attempt`]. The live key is
    /// left exactly as it is: whatever occupies it was authorized on its
    /// own terms and this outcome is not about it.
    ///
    /// The answer is deliberately **not** retryable: re-sending cannot
    /// help, and the caller must not read a retained charge as a reason
    /// to buy again.
    async fn publish_superseded(
        &self,
        attempt: &PurchaseAttempt,
        state: PurchaseState,
        known: &str,
        now_ns: u64,
    ) -> A2aPurchase {
        let generation = attempt.generation.clone();
        match self.store.retain_superseded(attempt, state, now_ns).await {
            Ok(_) => A2aPurchase::Failed {
                quote_id: attempt.quote_id.clone(),
                message: format!(
                    "{known}, and its intent key no longer holds the attempt it was bought \
                     for; the evidence is retained as superseded attempt {generation} — close \
                     it with resolve_superseded_attempt, never by buying again"
                ),
                retryable: false,
            },
            Err(e) => A2aPurchase::Failed {
                quote_id: attempt.quote_id.clone(),
                message: format!(
                    "{known}, its intent key no longer holds the attempt it was bought for, \
                     and retaining that evidence failed ({e}); superseded attempt \
                     {generation} must be reconciled from the billing log"
                ),
                retryable: true,
            },
        }
    }

    /// Submit a paid attempt with its stored proof, and record the
    /// outcome on the attempt.
    ///
    /// A retryable refusal keeps the attempt `Paid` — the *same* proof is
    /// resubmitted, never a second purchase. A refusal that says the
    /// provider will never execute this purchase moves it to
    /// `PaidUnexecutable` with the payment evidence retained (boundary
    /// c).
    ///
    /// `Mesh::submit_task_paid` remains available for callers that keep
    /// no purchase store; this verb is that call plus the record.
    pub async fn submit_task(&self, provider_node: u64, task_id: &str) -> A2aSubmit {
        let key = self.key(provider_node, task_id);
        let attempt = match self.store.attempt(&key).await {
            Ok(Some(attempt)) => attempt,
            Ok(None) => {
                return A2aSubmit::Retry {
                    message: format!(
                        "no purchase attempt for task {task_id}; prepare and purchase first"
                    ),
                }
            }
            Err(e) => {
                return A2aSubmit::Retry {
                    message: e.to_string(),
                }
            }
        };
        let (proof, billing) = match &attempt.state {
            PurchaseState::Paid { proof, billing } => (proof.clone(), billing.clone()),
            PurchaseState::Submitted { task_id } => {
                return A2aSubmit::Accepted {
                    task_id: task_id.clone(),
                }
            }
            PurchaseState::PaidUnexecutable { refusal, .. } => {
                return A2aSubmit::Unexecutable {
                    refusal: refusal.clone(),
                }
            }
            PurchaseState::RefusedExposed { reason, .. }
            | PurchaseState::Resolved {
                outcome: reason, ..
            } => {
                return A2aSubmit::Unexecutable {
                    refusal: RefusalRecord {
                        at_ns: self.clock.now_ns(),
                        message: format!("this attempt was never paid for: {reason}"),
                        reason: None,
                        safe_to_retry: false,
                        safe_to_requote: false,
                    },
                }
            }
            other => {
                return A2aSubmit::Retry {
                    message: format!(
                        "task {task_id} is `{}`, not paid — purchase it before submitting",
                        other.tag()
                    ),
                }
            }
        };
        let Some(prepared) = attempt.prepared.clone() else {
            return A2aSubmit::Retry {
                message: "the stored attempt carries no prepared task".to_string(),
            };
        };

        // Read before the provider round trip, presented after it.
        let identity = attempt.identity();
        let outcome = self.tasks.submit(&prepared, &proof).await;
        let now_ns = self.clock.now_ns();
        match outcome {
            Ok(TaskAck {
                task_id: acked,
                accepted: true,
                ..
            }) => {
                let _ = self
                    .store
                    .transition_exact(
                        &key,
                        &[StateTag::Paid],
                        &identity,
                        PurchaseState::Submitted {
                            task_id: acked.clone(),
                        },
                        now_ns,
                    )
                    .await;
                A2aSubmit::Accepted { task_id: acked }
            }
            Ok(TaskAck {
                accepted: false,
                reason,
                ..
            }) => {
                let message =
                    reason.unwrap_or_else(|| "the provider refused the brief".to_string());
                // A rejected ack is prose. It carries no recovery
                // posture, and a `String` cannot be turned back into the
                // `SubmitRejection` it was rendered from — so on this
                // path there is no structured verdict to interpret. For
                // an attempt that is already **paid**, "the provider
                // will never execute this" is a claim about money and
                // needs positive evidence: a schematic that says so.
                // Without one the purchase keeps its evidence and the
                // same proof is resubmitted. Matching one variant's
                // sentence and reading every other sentence as permanent
                // is exactly what stranded a retryable refusal.
                self.keep_paid(&key, &identity, now_ns, message).await
            }
            Err(A2aFlowError::PaymentRefused { message, schematic }) => {
                // The structured path: the provider's own recovery
                // posture decides, never this flow's reading of its
                // wording.
                match classify_refusal(&message, schematic.as_deref(), now_ns) {
                    Ok(refusal) => {
                        self.unexecutable(&key, &identity, proof, billing, refusal)
                            .await
                    }
                    Err(message) => self.keep_paid(&key, &identity, now_ns, message).await,
                }
            }
            Err(e) => self.keep_paid(&key, &identity, now_ns, e.to_string()).await,
        }
    }

    /// A retryable submit refusal: the attempt stays `Paid`, so the same
    /// proof is what gets resubmitted.
    async fn keep_paid(
        &self,
        key: &PurchaseKey,
        identity: &AttemptIdentity,
        now_ns: u64,
        message: String,
    ) -> A2aSubmit {
        if let Ok(Some(attempt)) = self.store.attempt(key).await {
            if let PurchaseState::Paid { proof, billing } = attempt.state {
                let _ = self
                    .store
                    .transition_exact(
                        key,
                        &[StateTag::Paid],
                        identity,
                        PurchaseState::Paid { proof, billing },
                        now_ns,
                    )
                    .await;
            }
        }
        A2aSubmit::Retry { message }
    }

    async fn unexecutable(
        &self,
        key: &PurchaseKey,
        identity: &AttemptIdentity,
        proof: TaskPaymentProof,
        billing: serde_json::Value,
        refusal: RefusalRecord,
    ) -> A2aSubmit {
        match self
            .store
            .transition_exact(
                key,
                &[StateTag::Paid],
                identity,
                PurchaseState::PaidUnexecutable {
                    proof,
                    billing,
                    refusal: refusal.clone(),
                },
                self.clock.now_ns(),
            )
            .await
        {
            Ok(_) => A2aSubmit::Unexecutable { refusal },
            Err(e) => A2aSubmit::Retry {
                message: format!(
                    "the provider refused to execute this paid purchase ({}), and recording \
                     that failed ({e}) — the attempt is still Paid",
                    refusal.message
                ),
            },
        }
    }

    /// The operator's exit for an attempt the automatic path cannot
    /// close: `Unknown` (the provider is gone or purged the quote),
    /// `RefusedExposed`, or `PaidUnexecutable`.
    pub async fn resolve_attempt(
        &self,
        provider_node: u64,
        task_id: &str,
        resolution: AttemptResolution,
    ) -> Result<PurchaseAttempt, PurchaseError> {
        let key = self.key(provider_node, task_id);
        let (from, to) = resolution_target(resolution);
        self.store
            .transition(&key, from, to, self.clock.now_ns())
            .await
    }

    /// The same exit, for the retained evidence of a **superseded**
    /// incarnation: a purchase that settled (or was exposed) against a
    /// record the intent key no longer holds.
    ///
    /// Addressed by the pair that identifies it — the key and the
    /// generation — both of which every record returned by
    /// [`Self::attempts`] carries, so an operator never has to guess a
    /// store id. The live attempt under the same key is untouched.
    pub async fn resolve_superseded_attempt(
        &self,
        provider_node: u64,
        task_id: &str,
        generation: &AttemptGeneration,
        resolution: AttemptResolution,
    ) -> Result<PurchaseAttempt, PurchaseError> {
        let key = self.key(provider_node, task_id);
        let (from, to) = resolution_target(resolution);
        self.store
            .transition_superseded(&key, generation, from, to, self.clock.now_ns())
            .await
    }

    /// The retained evidence of one superseded incarnation, if any was
    /// written.
    pub async fn superseded_attempt(
        &self,
        provider_node: u64,
        task_id: &str,
        generation: &AttemptGeneration,
    ) -> Result<Option<PurchaseAttempt>, StoreError> {
        self.store
            .superseded(&self.key(provider_node, task_id), generation)
            .await
    }
}

/// Which states an [`AttemptResolution`] may be applied from, and what
/// it writes — shared by the live and superseded operator exits so the
/// two can never disagree about what an operator's decision means.
fn resolution_target(resolution: AttemptResolution) -> (&'static [StateTag], PurchaseState) {
    match resolution {
        AttemptResolution::Paid { proof, billing } => {
            (&[StateTag::Unknown], PurchaseState::Paid { proof, billing })
        }
        AttemptResolution::NotPaid { reason } => (
            &[StateTag::Unknown],
            PurchaseState::RefusedUnexposed { reason },
        ),
        AttemptResolution::Closed { outcome, evidence } => (
            &[StateTag::RefusedExposed, StateTag::PaidUnexecutable],
            PurchaseState::Resolved { outcome, evidence },
        ),
    }
}

/// Is a paid submission's refusal terminal for the purchase?
///
/// `Ok(refusal)` means the provider will never execute it — the attempt
/// keeps its evidence in `PaidUnexecutable`. `Err(message)` means retry
/// with the same proof.
fn classify_refusal(
    message: &str,
    schematic: Option<&FailureSchematic>,
    now_ns: u64,
) -> Result<RefusalRecord, String> {
    let Some(schematic) = schematic else {
        // No structured verdict: ambiguous, so the safe reading is that
        // the purchase is still good and the submission can be retried.
        return Err(message.to_string());
    };
    let terminal = TERMINAL_SUBMIT_REASONS.contains(&schematic.reason.as_str())
        || (!schematic.recovery.safe_to_retry && !schematic.recovery.safe_to_requote);
    if terminal {
        Ok(RefusalRecord {
            at_ns: now_ns,
            message: message.to_string(),
            reason: Some(schematic.reason.clone()),
            safe_to_retry: schematic.recovery.safe_to_retry,
            safe_to_requote: schematic.recovery.safe_to_requote,
        })
    } else {
        Err(message.to_string())
    }
}
