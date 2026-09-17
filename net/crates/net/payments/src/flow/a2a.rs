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
    purchase_hash, task_commitment, A2aOffer, PrepareReply, PreparedTask, SubmitRejection, TaskAck,
    TaskBrief,
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
const TERMINAL_SUBMIT_REASONS: [&str; 3] = [
    "admission_revoked",
    "no_reservation",
    "input_binding_mismatch",
];

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
    /// `Unknown`, `RefusedExposed`, `PaidUnexecutable` and a `Paid`
    /// attempt that was never submitted all describe money whose fate is
    /// unresolved; pruning them would delete the caller's only evidence.
    /// They leave only through an operator decision.
    pub fn is_unresolved_financial(&self) -> bool {
        matches!(
            self.state,
            PurchaseState::Unknown { .. }
                | PurchaseState::RefusedExposed { .. }
                | PurchaseState::PaidUnexecutable { .. }
                | PurchaseState::Paid { .. }
        )
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
    /// Not a row of the caller transition table.
    #[error("`{from}` → `{to}` is not a caller purchase transition")]
    NotATransition { from: StateTag, to: StateTag },
    #[error(transparent)]
    Store(#[from] StoreError),
}

// ---------------------------------------------------------------------------
// The durable store
// ---------------------------------------------------------------------------

/// The on-disk document of `a2a-purchases.json`: attempts by
/// [`PurchaseKey::id`].
///
/// Public because it is the recovery surface — an operator tool (or a
/// test) reads and seeds attempts through the same locked
/// [`crate::policy::store`] helpers this module uses, rather than
/// through a private shape it has to guess.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct A2aPurchaseFile {
    #[serde(default)]
    pub attempts: BTreeMap<String, PurchaseAttempt>,
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
        Ok(file.attempts.get(&key.id()).cloned())
    }

    /// Every attempt on file — the operator's queue.
    pub async fn attempts(&self) -> Result<Vec<PurchaseAttempt>, StoreError> {
        let file: A2aPurchaseFile = load_json(&self.path).await?;
        Ok(file.attempts.into_values().collect())
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
        let id_owned = id.clone();
        let claim = mutate_json_if_changed::<A2aPurchaseFile, _, _>(&self.path, move |file| {
            let id = id_owned;
            let Some(existing) = file.attempts.get_mut(&id) else {
                file.attempts.insert(
                    id.clone(),
                    PurchaseAttempt {
                        key: key_owned,
                        commitment: commitment_owned,
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
    /// The store never reads a clock: `now_ns` stamps the record, so a
    /// recovery pass can replay with the times it is reasoning about.
    pub async fn transition(
        &self,
        key: &PurchaseKey,
        from: &[StateTag],
        to: PurchaseState,
        now_ns: u64,
    ) -> Result<PurchaseAttempt, PurchaseError> {
        self.cas(key, from, None, to, now_ns, |_| {}).await
    }

    /// The CAS every verb funnels through: table check, state check,
    /// optional lease check, patch, one atomic file replace.
    async fn cas<F>(
        &self,
        key: &PurchaseKey,
        from: &[StateTag],
        lease: Option<&str>,
        to: PurchaseState,
        now_ns: u64,
        patch: F,
    ) -> Result<PurchaseAttempt, PurchaseError>
    where
        F: FnOnce(&mut PurchaseAttempt) + Send,
    {
        let id = key.id();
        let expected: Vec<StateTag> = from.to_vec();
        let lease = lease.map(str::to_string);
        let updated = mutate_json_if_changed::<A2aPurchaseFile, _, _>(&self.path, move |file| {
            let Some(attempt) = file.attempts.get_mut(&id) else {
                return (Err(PurchaseError::Missing { key: id.clone() }), false);
            };
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
            if let Some(lease) = &lease {
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
        self.wake(key.id().as_str());
        Ok(updated)
    }

    /// Drop attempts that have finished and aged out.
    ///
    /// Only `Submitted`, `Resolved`, `RefusedUnexposed`, and an
    /// abandoned `Preparing` lease follow ordinary retention. The
    /// unresolved-financial classes — `Unknown`, `RefusedExposed`,
    /// `PaidUnexecutable`, and a `Paid` attempt that was never submitted
    /// — are **never** pruned: they are the caller's only record that
    /// money may have moved. Returns how many were removed.
    pub async fn prune(&self, now_ns: u64, retention_ns: u64) -> Result<usize, StoreError> {
        mutate_json_if_changed::<A2aPurchaseFile, _, _>(&self.path, move |file| {
            let before = file.attempts.len();
            file.attempts.retain(|_, attempt| {
                if attempt.is_unresolved_financial() {
                    return true;
                }
                now_ns.saturating_sub(attempt.updated_at_ns) <= retention_ns
            });
            let removed = before - file.attempts.len();
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

/// Put the attempt back into `Preparing` under `lease_id`, clearing
/// everything the previous quote established.
///
/// Returns the quote id whose operator approval (if any) the caller must
/// clear: an approval authorizes one exact quote, and this attempt is
/// about to get a different one (boundary a).
fn take_lease(attempt: &mut PurchaseAttempt, lease_id: &str, now_ns: u64) -> Option<String> {
    let cleared = attempt.quote_id.take();
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
/// `Reconciliation` are status reads, `Retired` and `Rejected` are dead
/// ends, and `Conflict` needs an operator.
#[derive(Debug, thiserror::Error)]
pub enum A2aPrepareError {
    /// Transport or decode failure talking to the provider. Nothing was
    /// reserved and nothing was quoted.
    #[error("a2a prepare transport failed: {0}")]
    Transport(String),
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
    /// Could not get far enough to have an opinion about the money.
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

    /// Every stored attempt — the operator's queue.
    pub async fn attempts(&self) -> Result<Vec<PurchaseAttempt>, StoreError> {
        self.store.attempts().await
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
                Some(&lease_id),
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
            .map_err(|e| A2aPrepareError::Transport(e.to_string()))?;
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
                        .transition(
                            key,
                            from,
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
                None,
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
            .transition(
                key,
                from,
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
                let state = PurchaseState::Paid {
                    proof: task_proof.clone(),
                    billing: proof.clone(),
                };
                if let Err(e) = self.store.transition(key, from, state, now_ns).await {
                    return A2aPurchase::Failed {
                        quote_id: Some(quote_id),
                        message: format!(
                            "the payment landed but recording it failed ({e}); the stored \
                             attempt is still resumable"
                        ),
                        retryable: true,
                    };
                }
                // The human's approval was for this exact quote and is
                // consumed by the payment it authorized.
                self.payments.clear_approval(&quote_id).await;
                A2aPurchase::Paid {
                    task_id: key.task_id.clone(),
                    proof: task_proof,
                    billing: proof,
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
                let _ = self
                    .store
                    .transition(
                        key,
                        from,
                        PurchaseState::Unknown {
                            last_error: message.clone(),
                        },
                        now_ns,
                    )
                    .await;
                A2aPurchase::Unknown {
                    quote_id: quote_id.unwrap_or_default(),
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
        if let Err(e) = self
            .store
            .transition(
                key,
                from,
                PurchaseState::RefusedExposed {
                    reason: reason.clone(),
                    reservation_kept,
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
            funds_ambiguous: reservation_kept,
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
                    .transition(
                        &key,
                        &[StateTag::Paid],
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
                // The one retryable in-body refusal is capacity — the
                // provider's own wording for it, not a literal here.
                if message == SubmitRejection::Busy.to_string() {
                    return self.keep_paid(&key, now_ns, message).await;
                }
                self.unexecutable(
                    &key,
                    proof,
                    billing,
                    RefusalRecord {
                        at_ns: now_ns,
                        message,
                        reason: None,
                        safe_to_retry: false,
                        safe_to_requote: false,
                    },
                )
                .await
            }
            Err(A2aFlowError::PaymentRefused { message, schematic }) => {
                match classify_refusal(&message, schematic.as_deref(), now_ns) {
                    Ok(refusal) => self.unexecutable(&key, proof, billing, refusal).await,
                    Err(message) => self.keep_paid(&key, now_ns, message).await,
                }
            }
            Err(e) => self.keep_paid(&key, now_ns, e.to_string()).await,
        }
    }

    /// A retryable submit refusal: the attempt stays `Paid`, so the same
    /// proof is what gets resubmitted.
    async fn keep_paid(&self, key: &PurchaseKey, now_ns: u64, message: String) -> A2aSubmit {
        if let Ok(Some(attempt)) = self.store.attempt(key).await {
            if let PurchaseState::Paid { proof, billing } = attempt.state {
                let _ = self
                    .store
                    .transition(
                        key,
                        &[StateTag::Paid],
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
        proof: TaskPaymentProof,
        billing: serde_json::Value,
        refusal: RefusalRecord,
    ) -> A2aSubmit {
        match self
            .store
            .transition(
                key,
                &[StateTag::Paid],
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
        let now_ns = self.clock.now_ns();
        match resolution {
            AttemptResolution::Paid { proof, billing } => {
                self.store
                    .transition(
                        &key,
                        &[StateTag::Unknown],
                        PurchaseState::Paid { proof, billing },
                        now_ns,
                    )
                    .await
            }
            AttemptResolution::NotPaid { reason } => {
                self.store
                    .transition(
                        &key,
                        &[StateTag::Unknown],
                        PurchaseState::RefusedUnexposed { reason },
                        now_ns,
                    )
                    .await
            }
            AttemptResolution::Closed { outcome, evidence } => {
                self.store
                    .transition(
                        &key,
                        &[StateTag::RefusedExposed, StateTag::PaidUnexecutable],
                        PurchaseState::Resolved { outcome, evidence },
                        now_ns,
                    )
                    .await
            }
        }
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
