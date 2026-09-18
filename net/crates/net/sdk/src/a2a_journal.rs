//! Durable admission records for paid agent-to-agent tasks — the
//! provider's own evidence that a payment bought exactly one execution
//! of exactly one brief (`A2A_PAID_ADMISSION_PLAN.md` D6).
//!
//! Two tables live in one file because they have different lifetimes:
//!
//! - the **result** table ([`AdmissionRecord`]) — one row per
//!   `(owner, task id)`, carrying the reservation, the payment
//!   evidence, and eventually the outcome. Rows age out under the three
//!   retention classes below.
//! - the **ledger** ([`LaunchLedgerEntry`]) — one row per launch that
//!   ever happened, **never pruned automatically**. It is what makes a
//!   retry after the result was retired answer `Retired` instead of
//!   buying a second run.
//!
//! # One owner, for the journal's whole lifetime
//!
//! [`A2aAdmissionJournal::open`] takes an fs2 **exclusive lock on a
//! stable sidecar `<path>.owner`**, created once and never replaced,
//! and holds it in an [`Arc<JournalOwner>`](JournalOwner) for as long as
//! the journal lives. The journal file itself is replaced on every write
//! (temp → fsync → rename), so a lock taken on *it* would guard the old
//! inode the moment the first write landed — the same reason
//! [`crate::pins::PinStore`]'s mutate path locks a sidecar rather than
//! the store. The per-write path additionally takes the short
//! `PinStore`-style `<path>.lock`, so a read-only inspector can
//! coordinate with a writer; the `.owner` lock is the **liveness** lock
//! and the `.lock` sidecar is the **transaction** lock.
//!
//! A second `open` — in this process (an in-process registry) or in
//! another (the OS lock) — fails with
//! [`A2aJournalError::OwnedElsewhere`], so a serving path configured
//! against an already-owned journal refuses at serve time rather than
//! running two writers over one set of admissions. Hand
//! [`A2aAdmissionJournal::owner`] clones to every serve handle, handler
//! and spawned executor future: while any of them can still write, the
//! owner must not be released.
//!
//! OS advisory locks release on process death, so a crashed owner's
//! successor acquires cleanly and treats what it finds conservatively —
//! see the recovery table on [`AdmissionState::status`]. There is no
//! epoch field; liveness *is* the lock.
//!
//! **Limits, stated rather than papered over:**
//!
//! - **Local filesystems only.** Advisory locking over NFS/SMB is not
//!   dependable, which is the same caveat the payment engine's
//!   `payment-engine.json` carries. A journal on a network share is not
//!   protected by this contract.
//! - **An operator who deletes `<path>.owner` by hand defeats the
//!   exclusion** — a new owner then creates a fresh sidecar and locks
//!   that one, while the incumbent still holds a lock on an unlinked
//!   inode. The sidecar is part of the store; back it up and delete it
//!   with the journal, never on its own.
//!
//! # Two implementations, one state machine
//!
//! Every mutation — journal or memory — runs the same check-then-mutate
//! body ([`AdmissionStore`]); only the durability differs. The journal
//! writes through in-process mutex → `.lock` → load → check → temp +
//! fsync + rename; the in-memory [`A2aAdmissions`] applies it under a
//! `parking_lot` mutex and forgets everything at process exit. An
//! all-free catalog therefore needs no journal, and a handler written
//! against the trait never learns which one it has.
//!
//! An [`A2aJournalError::Io`] leaves the file **untouched** (the rename
//! is what publishes a write), which is what lets the serving path keep
//! its rule: *nothing runs unless the claim write returned `Ok`*.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::a2a::{A2aOffer, TaskBrief, TaskOwner, TaskState};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Why an admission-store operation refused.
#[derive(Debug, thiserror::Error)]
pub enum A2aJournalError {
    /// Another live owner already holds this journal — in this process
    /// or in another one. The serving path turns this into
    /// `ServeError::A2aPaidMisconfigured`: two writers over one set of
    /// admissions is exactly the situation the ownership contract
    /// exists to prevent.
    #[error("admission journal at {path} is owned by another live holder")]
    OwnedElsewhere {
        /// The journal path whose `.owner` sidecar is locked.
        path: String,
    },
    /// The requested state change is not one this store will make: the
    /// record is missing, its current state is not in the caller's
    /// `from` set, or the pair is absent from the D2 transition table.
    ///
    /// A programming error on the serving path, surfaced to the caller
    /// as the retryable `journal_unavailable` posture — never as a
    /// verdict about money.
    #[error("admission conflict: {reason}")]
    Conflict {
        /// What was refused, and what the record actually holds.
        reason: String,
    },
    /// An I/O failure reading or writing the journal. The file is
    /// unchanged.
    #[error("admission journal I/O error at {path}: {reason}")]
    Io {
        /// The path involved.
        path: String,
        /// The stringified underlying error.
        reason: String,
    },
    /// The journal file exists but does not parse. Refused rather than
    /// silently treated as empty — an empty journal would relaunch paid
    /// work and forget a ledger.
    #[error("admission journal at {path} is corrupt: {reason}")]
    Corrupt {
        /// The path involved.
        path: String,
        /// Why it failed to parse.
        reason: String,
    },
}

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

/// Default terminal-result retention for a record whose file predates
/// the retention terms being recorded: one hour, matching
/// [`crate::a2a::TERMINAL_RECORD_TTL_SECS`].
const DEFAULT_RETENTION_SECS: u64 = 60 * 60;
/// Default unpaid-reservation retention for such a record: seven days,
/// the plan's published default.
const DEFAULT_RESERVATION_RETENTION_SECS: u64 = 7 * 24 * 60 * 60;

fn default_retention_secs() -> u64 {
    DEFAULT_RETENTION_SECS
}
fn default_reservation_retention_secs() -> u64 {
    DEFAULT_RESERVATION_RETENTION_SECS
}

/// One gate denial, payer mismatch, or header mismatch — audit only.
///
/// A note **never** changes the record's state and never moves its
/// `updated_at`: a caller that keeps presenting a bad quote must not be
/// able to hold a reservation open past its retention by retrying.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptNote {
    /// Unix seconds when the attempt was refused.
    pub at: u64,
    /// The refusal reason, in the payment schematic's vocabulary.
    pub reason: String,
    /// The quote id the attempt claimed, when it carried one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimed_quote_id: Option<String>,
}

/// The state machine of one admission. The discriminants are
/// [`StateTag`]; the transitions are the D2 table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
// `admission`, not `state`: the terminal variant carries a `state`
// field of its own (the executor's `TaskState`), and an internal tag
// may not collide with a field name.
#[serde(tag = "admission", rename_all = "snake_case")]
pub enum AdmissionState {
    /// Capacity is held and (for a paid service) a purchase may be made
    /// against this reservation. Nothing financial has been claimed.
    Reserved {
        /// Unix seconds after which the reservation no longer holds
        /// capacity. Re-acquired, not re-minted, by a later prepare.
        expires_at: u64,
    },
    /// A payment was redeemed for this admission and the work has not
    /// been claimed yet. The window a crash can land in — recovery
    /// answers `paid_not_started` and a retry launches once.
    Paid {
        /// The redeemed quote.
        quote_id: String,
        /// The payer the gate attributed the payment to.
        payer: [u8; 32],
    },
    /// The launch was claimed durably; the executor may be running,
    /// finished, or gone with the process. Never relaunched.
    Launched {
        /// The redeemed quote, absent for a free service.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        quote_id: Option<String>,
        /// The payer, absent for a free service.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payer: Option<[u8; 32]>,
    },
    /// The outcome is recorded. The only state the automatic
    /// result-retention rule prunes.
    Terminal {
        /// The redeemed quote, absent for a free service.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        quote_id: Option<String>,
        /// The payer, absent for a free service.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payer: Option<[u8; 32]>,
        /// What the executor (or an operator) recorded.
        state: TaskState,
    },
    /// Post-payment revocation: the provider refused after money may
    /// already have moved. Never pruned automatically, and the only
    /// exit is an operator [`AdmissionStore::resolve`].
    Reconcile {
        /// Why admission was revoked.
        reason: String,
        /// The quote the caller claimed, or the one on record.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        claimed_quote_id: Option<String>,
        /// The payer, when one was established.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payer: Option<[u8; 32]>,
    },
}

/// The discriminant of an [`AdmissionState`] — what
/// [`AdmissionStore::transition`]'s `from` set is built out of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateTag {
    /// [`AdmissionState::Reserved`].
    Reserved,
    /// [`AdmissionState::Paid`].
    Paid,
    /// [`AdmissionState::Launched`].
    Launched,
    /// [`AdmissionState::Terminal`].
    Terminal,
    /// [`AdmissionState::Reconcile`].
    Reconcile,
}

impl StateTag {
    /// The stable label used in refusal messages and operator output.
    pub fn as_str(self) -> &'static str {
        match self {
            StateTag::Reserved => "reserved",
            StateTag::Paid => "paid",
            StateTag::Launched => "launched",
            StateTag::Terminal => "terminal",
            StateTag::Reconcile => "reconcile",
        }
    }

    /// Every tag, in declaration order — so an exhaustive table-driven
    /// check cannot silently miss one a later revision adds.
    pub const ALL: [StateTag; 5] = [
        StateTag::Reserved,
        StateTag::Paid,
        StateTag::Launched,
        StateTag::Terminal,
        StateTag::Reconcile,
    ];
}

impl AdmissionState {
    /// This state's discriminant.
    pub fn tag(&self) -> StateTag {
        match self {
            AdmissionState::Reserved { .. } => StateTag::Reserved,
            AdmissionState::Paid { .. } => StateTag::Paid,
            AdmissionState::Launched { .. } => StateTag::Launched,
            AdmissionState::Terminal { .. } => StateTag::Terminal,
            AdmissionState::Reconcile { .. } => StateTag::Reconcile,
        }
    }

    /// What a status query answers for a record in this state when no
    /// live registry entry backs it — the D6 recovery table:
    ///
    /// | Found | Status |
    /// |---|---|
    /// | `Reserved` | `None` (unknown; prepare returns the same reservation) |
    /// | `Paid` | `Interrupted { detail: "paid_not_started" }` |
    /// | `Launched` without a terminal | `Interrupted { detail: "outcome_unknown" }` |
    /// | `Reconcile` | `Interrupted { detail: "admission_revoked" }` |
    /// | `Terminal` | exactly what was recorded |
    ///
    /// Reading this answer changes nothing: a successor owner never
    /// relaunches and never rewrites `Paid` / `Launched` / `Reconcile`.
    pub fn status(&self) -> Option<TaskState> {
        match self {
            AdmissionState::Reserved { .. } => None,
            AdmissionState::Paid { .. } => Some(TaskState::Interrupted {
                detail: DETAIL_PAID_NOT_STARTED.to_string(),
            }),
            AdmissionState::Launched { .. } => Some(TaskState::Interrupted {
                detail: DETAIL_OUTCOME_UNKNOWN.to_string(),
            }),
            AdmissionState::Reconcile { .. } => Some(TaskState::Interrupted {
                detail: DETAIL_ADMISSION_REVOKED.to_string(),
            }),
            AdmissionState::Terminal { state, .. } => Some(state.clone()),
        }
    }

    /// Whether this state belongs to the **unresolved financial** class:
    /// money may have moved and the outcome is not recorded. Never
    /// pruned automatically; only [`AdmissionStore::resolve`] moves one
    /// to `Terminal`.
    pub fn is_unresolved(&self) -> bool {
        matches!(
            self,
            AdmissionState::Paid { .. }
                | AdmissionState::Launched { .. }
                | AdmissionState::Reconcile { .. }
        )
    }

    /// The quote and payer this state carries, if any.
    fn evidence(&self) -> (Option<String>, Option<[u8; 32]>) {
        match self {
            AdmissionState::Reserved { .. } => (None, None),
            AdmissionState::Paid { quote_id, payer } => (Some(quote_id.clone()), Some(*payer)),
            AdmissionState::Launched { quote_id, payer }
            | AdmissionState::Terminal {
                quote_id, payer, ..
            } => (quote_id.clone(), *payer),
            AdmissionState::Reconcile {
                claimed_quote_id,
                payer,
                ..
            } => (claimed_quote_id.clone(), *payer),
        }
    }
}

/// `detail` of the status a crash between redeem and claim produces.
pub const DETAIL_PAID_NOT_STARTED: &str = "paid_not_started";
/// `detail` of the status a launch whose outcome was never recorded
/// produces.
pub const DETAIL_OUTCOME_UNKNOWN: &str = "outcome_unknown";
/// `detail` of the status a post-payment revocation produces.
pub const DETAIL_ADMISSION_REVOKED: &str = "admission_revoked";

/// One admission: who asked for what, under which offer terms, and how
/// far it got.
///
/// The record carries the retention terms it was **admitted under**
/// (copied from the offer at insert time), so [`AdmissionStore::prune`]
/// needs no catalog and a re-priced or retired offer never changes how
/// long an outstanding admission is kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmissionRecord {
    /// The authenticated submitter this admission belongs to.
    #[serde(with = "owner_repr")]
    pub owner: TaskOwner,
    /// The task id, unique per owner.
    pub task_id: String,
    /// The catalog service this admission is against.
    pub service_id: String,
    /// The offer revision it was admitted under.
    pub revision: String,
    /// Provider-minted per reservation; `None` for a free inline
    /// admission that never went through prepare.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission_id: Option<String>,
    /// [`crate::a2a::task_commitment`] of the offer + brief.
    pub commitment: String,
    /// The brief, byte-exact as admitted.
    pub brief: TaskBrief,
    /// Whether the offer priced this service. A free reservation may be
    /// deleted on a failed preflight and may claim a launch without a
    /// payment; a paid one may do neither.
    #[serde(default)]
    pub paid: bool,
    /// How long a terminal result is kept, from the offer.
    #[serde(default = "default_retention_secs")]
    pub retention_secs: u64,
    /// How long an unpaid reservation is kept, from the offer.
    #[serde(default = "default_reservation_retention_secs")]
    pub reservation_retention_secs: u64,
    /// Where this admission is in the state machine.
    pub state: AdmissionState,
    /// Unix seconds of the last **state** change. Attempt notes do not
    /// move it.
    pub updated_at: u64,
    /// Gate denials and header mismatches: audit, never a state change.
    #[serde(default)]
    pub attempts: Vec<AttemptNote>,
}

impl AdmissionRecord {
    /// A fresh `Reserved` record for `brief` under `offer`.
    ///
    /// Everything the store later needs from the catalog — the service
    /// and revision, whether the service is priced, and both retention
    /// terms — is copied here, at the one moment the offer is known to
    /// be current.
    ///
    /// `admission_id` is `Some` for a prepared reservation and `None`
    /// for a free inline admission (D2 S5a). `expires_at` is
    /// `now + offer.reservation_ttl_secs`.
    pub fn reserved(
        owner: TaskOwner,
        brief: TaskBrief,
        offer: &A2aOffer,
        admission_id: Option<String>,
        commitment: impl Into<String>,
        now: u64,
    ) -> Self {
        Self {
            owner,
            task_id: brief.task_id.clone(),
            service_id: offer.service_id.clone(),
            revision: offer.revision.clone(),
            admission_id,
            commitment: commitment.into(),
            brief,
            paid: offer.pricing_terms.is_some(),
            retention_secs: offer.retention_secs,
            reservation_retention_secs: offer.reservation_retention_secs,
            state: AdmissionState::Reserved {
                expires_at: now.saturating_add(offer.reservation_ttl_secs),
            },
            updated_at: now,
            attempts: Vec::new(),
        }
    }

    /// Whether this record still holds capacity at `now`: every state
    /// except a lapsed reservation, a recorded terminal, and a revoked
    /// admission.
    pub fn holds_capacity(&self, now: u64) -> bool {
        match &self.state {
            AdmissionState::Reserved { expires_at } => *expires_at > now,
            AdmissionState::Paid { .. } | AdmissionState::Launched { .. } => true,
            AdmissionState::Terminal { .. } | AdmissionState::Reconcile { .. } => false,
        }
    }
}

/// One launch that happened. Written in the same atomic replace as the
/// `Launched` state and **never pruned automatically**: it outlives the
/// result so a retry after retention answers `Retired` rather than
/// buying a second execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchLedgerEntry {
    /// The submitter.
    #[serde(with = "owner_repr")]
    pub owner: TaskOwner,
    /// The task id.
    pub task_id: String,
    /// The reservation this launch was claimed under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission_id: Option<String>,
    /// The commitment that was launched.
    pub commitment: String,
    /// The quote that paid for it, absent for a free service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote_id: Option<String>,
    /// Unix seconds the claim was made durable.
    pub launched_at: u64,
}

/// What a successor owner inherited for one admission, sampled once at
/// [`A2aAdmissionJournal::open`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveredAdmission {
    /// The submitter.
    pub owner: TaskOwner,
    /// The task id.
    pub task_id: String,
    /// The state found on disk — unchanged by recovery.
    pub found: AdmissionState,
    /// What status answers for it ([`AdmissionState::status`]).
    pub status: TaskState,
}

/// What a CAS insert found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InsertOutcome {
    /// Nothing was there; the record is now this caller's reservation.
    Inserted,
    /// A record already existed, so nothing was written. The caller
    /// dispatches on it exactly as D2 P4/S4 prescribe.
    Existing(Box<AdmissionRecord>),
}

/// Unix seconds now — the clock every store call is given explicitly,
/// so no store ever guesses its own time.
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// The transition table
// ---------------------------------------------------------------------------

/// The **only** `(from, to)` pairs [`AdmissionStore::transition`] will
/// write. Everything else is an [`A2aJournalError::Conflict`].
///
/// The D2 table's remaining rows each carry a second, inseparable
/// effect and therefore have their own verb rather than being reachable
/// from here: `→ Launched` writes the ledger in the same replace
/// ([`AdmissionStore::claim_launch`]), and `→ Terminal` reclassifies the
/// record for retention ([`AdmissionStore::record_terminal`] for the
/// executor's outcome, [`AdmissionStore::resolve`] for an operator's).
/// A general `transition` that could reach them would be a way to
/// launch without a ledger entry.
const TRANSITIONS: &[(StateTag, StateTag)] = &[
    // P4 / S4: an expired reservation re-acquires capacity — same
    // admission id, refreshed `expires_at`.
    (StateTag::Reserved, StateTag::Reserved),
    // S6: the gate admitted.
    (StateTag::Reserved, StateTag::Paid),
    // S5: preflight failed at submit with payment headers present.
    (StateTag::Reserved, StateTag::Reconcile),
    // S5: preflight failed at submit on an already-paid admission.
    (StateTag::Paid, StateTag::Reconcile),
];

// ---------------------------------------------------------------------------
// The store trait
// ---------------------------------------------------------------------------

/// A durable or in-memory admission store: the same state machine, the
/// same capacity accounting, the same ledger.
///
/// Handlers are written against this trait so an all-free catalog can
/// run on [`A2aAdmissions`] with no journal, and a catalog with one paid
/// entry can run on [`A2aAdmissionJournal`] with no handler change. The
/// selector is the constructor plus [`SharedAdmissionStore`].
///
/// Every mutating call takes `now` (Unix seconds) rather than reading a
/// clock, so retention and expiry are exercised by tests instead of
/// waited out.
#[async_trait::async_trait]
pub trait AdmissionStore: Send + Sync {
    /// The record for `(owner, task_id)`, if one exists.
    async fn lookup(
        &self,
        owner: TaskOwner,
        task_id: &str,
    ) -> Result<Option<AdmissionRecord>, A2aJournalError>;

    /// CAS-insert a fresh `Reserved` record: written only if nothing is
    /// there, otherwise the existing record comes back untouched.
    ///
    /// Refuses a `record` that is not [`AdmissionState::Reserved`] —
    /// every other state is reached through a transition.
    async fn insert_reserved(
        &self,
        record: AdmissionRecord,
    ) -> Result<InsertOutcome, A2aJournalError>;

    /// How many admissions of `service_id` hold capacity at `now`: live
    /// reservations, paid-not-launched, and launched-not-terminal. A
    /// lapsed reservation, a recorded terminal and a revoked admission
    /// hold none.
    async fn in_flight(&self, service_id: &str, now: u64) -> Result<u64, A2aJournalError>;

    /// Move the record's state, refusing anything the D2 table does not
    /// allow.
    ///
    /// Both guards apply: the record's current tag must be in `from`,
    /// **and** `(current, to)` must be one of the four pairs this verb
    /// writes:
    ///
    /// | From | To | Where |
    /// |---|---|---|
    /// | `Reserved` | `Reserved` | P4 / S4 — an expired reservation re-acquires capacity |
    /// | `Reserved` | `Paid` | S6 — the gate admitted |
    /// | `Reserved` | `Reconcile` | S5 — preflight failed with payment headers present |
    /// | `Paid` | `Reconcile` | S5 — preflight failed on an already-paid admission |
    ///
    /// The table's remaining rows each carry a second, inseparable
    /// effect and have their own verb:
    /// [`claim_launch`](Self::claim_launch) writes `Launched` *and* the
    /// ledger, [`record_terminal`](Self::record_terminal) and
    /// [`resolve`](Self::resolve) write `Terminal`, and
    /// [`delete_reservation`](Self::delete_reservation) removes a free
    /// reservation. A general `transition` that could reach `Launched`
    /// would be a way to launch with no ledger entry.
    ///
    /// Only the state and `updated_at` change — the admission id,
    /// commitment and brief are immutable once inserted.
    async fn transition(
        &self,
        owner: TaskOwner,
        task_id: &str,
        from: &[StateTag],
        to: AdmissionState,
        now: u64,
    ) -> Result<(), A2aJournalError>;

    /// Append an audit note. The state and `updated_at` are unchanged —
    /// a denied gate attempt reopens nothing and extends nothing.
    async fn note(
        &self,
        owner: TaskOwner,
        task_id: &str,
        note: AttemptNote,
    ) -> Result<(), A2aJournalError>;

    /// Delete an unlaunched **free** reservation (D2 S5: preflight
    /// failed and nothing financial exists). Refuses a paid record and
    /// any state other than `Reserved`; the ledger is never touched.
    async fn delete_reservation(
        &self,
        owner: TaskOwner,
        task_id: &str,
    ) -> Result<(), A2aJournalError>;

    /// Claim the launch: `Paid → Launched` (paid) or `Reserved →
    /// Launched` (free), **and** the ledger entry, in one atomic
    /// replace.
    ///
    /// Either both land or neither does; there is no instant at which
    /// the store holds a launched admission with no ledger entry. The
    /// serving path spawns only after this returns `Ok`.
    async fn claim_launch(
        &self,
        owner: TaskOwner,
        task_id: &str,
        now: u64,
    ) -> Result<LaunchLedgerEntry, A2aJournalError>;

    /// Record the executor's outcome: `Launched → Terminal`. This is
    /// what a `TerminalHook` calls.
    ///
    /// The hook itself must not block, so bridge to this call through a
    /// spawn or a channel rather than awaiting it inside the hook.
    async fn record_terminal(
        &self,
        owner: TaskOwner,
        task_id: &str,
        state: TaskState,
        now: u64,
    ) -> Result<(), A2aJournalError>;

    /// **Operator only**: resolve an unresolved financial record —
    /// `Paid`, `Launched` or `Reconcile` → `Terminal`. The one exit from
    /// the never-pruned class; after it, the resolved-result retention
    /// rule applies.
    async fn resolve(
        &self,
        owner: TaskOwner,
        task_id: &str,
        state: TaskState,
        now: u64,
    ) -> Result<(), A2aJournalError>;

    /// Whether `(owner, task_id)` ever launched. Consulted at D2 P4/S4:
    /// a ledger hit with no record answers `Retired`, and the redeem
    /// step is never reached, so a retired result can never make its
    /// payment reusable.
    async fn ledger_has(&self, owner: TaskOwner, task_id: &str) -> Result<bool, A2aJournalError>;

    /// Every record in the unresolved financial class, for an operator
    /// view. Deterministically ordered.
    async fn unresolved(&self) -> Result<Vec<AdmissionRecord>, A2aJournalError>;

    /// Drop a **terminal** result immediately, once the requester has
    /// read it (the `forget` / `evict_terminal` path). Refuses every
    /// other state and never touches the ledger. Returns whether a
    /// record was removed.
    async fn forget(&self, owner: TaskOwner, task_id: &str) -> Result<bool, A2aJournalError>;

    /// Apply the three retention classes at `now`, returning how many
    /// records were removed:
    ///
    /// | Class | States | Pruned |
    /// |---|---|---|
    /// | Resolved result | `Terminal` | `retention_secs` after `updated_at` |
    /// | Unpaid reservation | `Reserved`, notes included | `reservation_retention_secs` after `updated_at` |
    /// | Unresolved financial | `Paid`, `Launched`, `Reconcile` | **never** |
    ///
    /// The ledger is never pruned.
    async fn prune(&self, now: u64) -> Result<u64, A2aJournalError>;
}

/// The configured admission store, chosen once at serve time: a
/// [`A2aAdmissionJournal`] when any catalog entry is paid or an operator
/// configured durability, an [`A2aAdmissions`] when the catalog is
/// entirely free.
///
/// A trait object rather than a two-arm enum: selection is the whole job
/// (nothing downstream matches on which one it got), and an enum would
/// need a third copy of the method list to forward through — one that
/// drifts from the trait silently the first time a verb is added.
/// `Arc` is the shape the serving path needs anyway, since handlers and
/// spawned executor futures share it.
pub type SharedAdmissionStore = Arc<dyn AdmissionStore>;

// ---------------------------------------------------------------------------
// The shared state machine
// ---------------------------------------------------------------------------

/// The record key: the owner in its stable string form plus the task id.
/// `TaskOwner` is deliberately not `Ord`; this is the one place a total
/// order over it is needed.
type Key = (String, String);

fn owner_key(owner: TaskOwner) -> String {
    match owner {
        TaskOwner::Local => "local".to_string(),
        TaskOwner::Peer(node) => format!("peer:{node}"),
        TaskOwner::Entity(id) => format!("entity:{}", to_hex(&id)),
    }
}

fn key(owner: TaskOwner, task_id: &str) -> Key {
    (owner_key(owner), task_id.to_string())
}

fn conflict(reason: impl Into<String>) -> A2aJournalError {
    A2aJournalError::Conflict {
        reason: reason.into(),
    }
}

/// Both stores' entire decision logic, independent of where the bytes
/// live. The journal runs it between a load and an atomic replace; the
/// in-memory store runs it under its mutex.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct StoreState {
    records: BTreeMap<Key, AdmissionRecord>,
    ledger: BTreeMap<Key, LaunchLedgerEntry>,
}

impl StoreState {
    fn record_mut(
        &mut self,
        owner: TaskOwner,
        task_id: &str,
    ) -> Result<&mut AdmissionRecord, A2aJournalError> {
        self.records
            .get_mut(&key(owner, task_id))
            .ok_or_else(|| conflict(format!("no admission record for task {task_id:?}")))
    }

    fn insert_reserved(
        &mut self,
        record: AdmissionRecord,
    ) -> Result<InsertOutcome, A2aJournalError> {
        if record.state.tag() != StateTag::Reserved {
            return Err(conflict(format!(
                "an insert may only create a reserved record, not {}",
                record.state.tag().as_str()
            )));
        }
        let k = key(record.owner, &record.task_id);
        if let Some(existing) = self.records.get(&k) {
            return Ok(InsertOutcome::Existing(Box::new(existing.clone())));
        }
        self.records.insert(k, record);
        Ok(InsertOutcome::Inserted)
    }

    fn in_flight(&self, service_id: &str, now: u64) -> u64 {
        self.records
            .values()
            .filter(|r| r.service_id == service_id && r.holds_capacity(now))
            .count() as u64
    }

    fn transition(
        &mut self,
        owner: TaskOwner,
        task_id: &str,
        from: &[StateTag],
        to: AdmissionState,
        now: u64,
    ) -> Result<(), A2aJournalError> {
        let to_tag = to.tag();
        let record = self.record_mut(owner, task_id)?;
        let current = record.state.tag();
        if !from.contains(&current) {
            return Err(conflict(format!(
                "task {task_id:?} is {}, not one of the expected {:?}",
                current.as_str(),
                from.iter().map(|t| t.as_str()).collect::<Vec<_>>()
            )));
        }
        if !TRANSITIONS.contains(&(current, to_tag)) {
            return Err(conflict(format!(
                "no transition {} -> {} exists for task {task_id:?}",
                current.as_str(),
                to_tag.as_str()
            )));
        }
        record.state = to;
        record.updated_at = now;
        Ok(())
    }

    fn note(
        &mut self,
        owner: TaskOwner,
        task_id: &str,
        note: AttemptNote,
    ) -> Result<(), A2aJournalError> {
        // Deliberately does not touch `updated_at`: the retention clock
        // belongs to the state, not to how often the caller retried.
        self.record_mut(owner, task_id)?.attempts.push(note);
        Ok(())
    }

    fn delete_reservation(
        &mut self,
        owner: TaskOwner,
        task_id: &str,
    ) -> Result<(), A2aJournalError> {
        let record = self.record_mut(owner, task_id)?;
        if record.state.tag() != StateTag::Reserved {
            return Err(conflict(format!(
                "task {task_id:?} is {}, and only a reservation may be deleted",
                record.state.tag().as_str()
            )));
        }
        if record.paid {
            return Err(conflict(format!(
                "task {task_id:?} reserves a paid service; its record is evidence and is never deleted"
            )));
        }
        self.records.remove(&key(owner, task_id));
        Ok(())
    }

    fn claim_launch(
        &mut self,
        owner: TaskOwner,
        task_id: &str,
        now: u64,
    ) -> Result<LaunchLedgerEntry, A2aJournalError> {
        let k = key(owner, task_id);
        if self.ledger.contains_key(&k) {
            return Err(conflict(format!(
                "task {task_id:?} already has a launch ledger entry; it is never launched twice"
            )));
        }
        let record = self.record_mut(owner, task_id)?;
        let (quote_id, payer) = match &record.state {
            AdmissionState::Paid { quote_id, payer } => (Some(quote_id.clone()), Some(*payer)),
            AdmissionState::Reserved { .. } if !record.paid => (None, None),
            AdmissionState::Reserved { .. } => {
                return Err(conflict(format!(
                    "task {task_id:?} reserves a paid service and has redeemed nothing"
                )))
            }
            other => {
                return Err(conflict(format!(
                    "task {task_id:?} is {}, which cannot claim a launch",
                    other.tag().as_str()
                )))
            }
        };
        let entry = LaunchLedgerEntry {
            owner,
            task_id: task_id.to_string(),
            admission_id: record.admission_id.clone(),
            commitment: record.commitment.clone(),
            quote_id: quote_id.clone(),
            launched_at: now,
        };
        record.state = AdmissionState::Launched { quote_id, payer };
        record.updated_at = now;
        self.ledger.insert(k, entry.clone());
        Ok(entry)
    }

    fn record_terminal(
        &mut self,
        owner: TaskOwner,
        task_id: &str,
        state: TaskState,
        now: u64,
    ) -> Result<(), A2aJournalError> {
        let record = self.record_mut(owner, task_id)?;
        if record.state.tag() != StateTag::Launched {
            return Err(conflict(format!(
                "task {task_id:?} is {}, and only a launched admission records an outcome",
                record.state.tag().as_str()
            )));
        }
        let (quote_id, payer) = record.state.evidence();
        record.state = AdmissionState::Terminal {
            quote_id,
            payer,
            state,
        };
        record.updated_at = now;
        Ok(())
    }

    fn resolve(
        &mut self,
        owner: TaskOwner,
        task_id: &str,
        state: TaskState,
        now: u64,
    ) -> Result<(), A2aJournalError> {
        let record = self.record_mut(owner, task_id)?;
        if !record.state.is_unresolved() {
            return Err(conflict(format!(
                "task {task_id:?} is {}, which is not an unresolved financial record",
                record.state.tag().as_str()
            )));
        }
        let (quote_id, payer) = record.state.evidence();
        record.state = AdmissionState::Terminal {
            quote_id,
            payer,
            state,
        };
        record.updated_at = now;
        Ok(())
    }

    fn forget(&mut self, owner: TaskOwner, task_id: &str) -> Result<bool, A2aJournalError> {
        let k = key(owner, task_id);
        match self.records.get(&k) {
            None => Ok(false),
            Some(r) if r.state.tag() == StateTag::Terminal => {
                self.records.remove(&k);
                Ok(true)
            }
            Some(r) => Err(conflict(format!(
                "task {task_id:?} is {}, and only a terminal result may be forgotten",
                r.state.tag().as_str()
            ))),
        }
    }

    fn prune(&mut self, now: u64) -> u64 {
        let before = self.records.len();
        self.records.retain(|_, r| match &r.state {
            // Resolved result.
            AdmissionState::Terminal { .. } => now < r.updated_at.saturating_add(r.retention_secs),
            // Unpaid reservation, attempt notes included.
            AdmissionState::Reserved { .. } => {
                now < r.updated_at.saturating_add(r.reservation_retention_secs)
            }
            // Unresolved financial: never automatically.
            AdmissionState::Paid { .. }
            | AdmissionState::Launched { .. }
            | AdmissionState::Reconcile { .. } => true,
        });
        (before - self.records.len()) as u64
    }

    fn unresolved(&self) -> Vec<AdmissionRecord> {
        self.records
            .values()
            .filter(|r| r.state.is_unresolved())
            .cloned()
            .collect()
    }
}

// ---------------------------------------------------------------------------
// On-disk shape
// ---------------------------------------------------------------------------

/// The journal format version. Bumped only for a change a reader of the
/// previous version could misinterpret; additive fields carry
/// `#[serde(default)]` instead.
const JOURNAL_VERSION: u32 = 1;

#[derive(Debug, Default, Serialize, Deserialize)]
struct JournalFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    records: Vec<AdmissionRecord>,
    #[serde(default)]
    ledger: Vec<LaunchLedgerEntry>,
}

impl JournalFile {
    fn into_state(self) -> StoreState {
        StoreState {
            records: self
                .records
                .into_iter()
                .map(|r| (key(r.owner, &r.task_id), r))
                .collect(),
            ledger: self
                .ledger
                .into_iter()
                .map(|e| (key(e.owner, &e.task_id), e))
                .collect(),
        }
    }

    fn from_state(state: &StoreState) -> Self {
        Self {
            version: JOURNAL_VERSION,
            records: state.records.values().cloned().collect(),
            ledger: state.ledger.values().cloned().collect(),
        }
    }
}

// ---------------------------------------------------------------------------
// Ownership
// ---------------------------------------------------------------------------

/// Journals owned by *this* process, keyed by the canonical `.owner`
/// sidecar path.
///
/// The OS lock alone is not quite enough: POSIX `fcntl` locks are
/// per-process, and a second `open` in one process could otherwise
/// succeed on some platforms and silently run two writers. Checking an
/// in-process registry first makes "a second open is refused" mean the
/// same thing everywhere.
static OWNED: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());

/// The exclusive-ownership handle for one journal: the locked `.owner`
/// sidecar plus this process's claim on it.
///
/// Held inside an `Arc` for the journal's whole lifetime and cloned into
/// every serve handle, handler and spawned executor future, so the owner
/// cannot be released while anything that can still write is alive.
/// Dropping the last clone closes the sidecar handle (releasing the OS
/// lock) and clears the in-process claim.
#[derive(Debug)]
pub struct JournalOwner {
    /// Canonical `.owner` path — the in-process registry key.
    sidecar: PathBuf,
    /// The locked handle. Dropping it releases the advisory lock.
    _lock: std::fs::File,
}

impl JournalOwner {
    /// The `.owner` sidecar this holder has locked.
    pub fn sidecar_path(&self) -> &Path {
        &self.sidecar
    }
}

impl Drop for JournalOwner {
    fn drop(&mut self) {
        OWNED.lock().remove(&self.sidecar);
    }
}

/// Holds the short cross-process transaction lock on `<path>.lock` for
/// one journal write, so a read-only inspector (or a second tool) can
/// coordinate with the owner. Dropping it releases the lock.
///
/// Distinct from the `.owner` lock on purpose: this one is taken and
/// released per write, that one is held for the journal's life.
struct WriteLock {
    _file: std::fs::File,
}

impl WriteLock {
    async fn acquire(journal_path: &Path) -> Result<Self, A2aJournalError> {
        let lock_path = sidecar(journal_path, ".lock");
        let display = lock_path.display().to_string();
        let file = open_sidecar(lock_path).await?;
        // Poll `try_lock_exclusive` with async backoff rather than
        // blocking a pool thread on `lock_exclusive`: the holder's own
        // `tokio::fs` load/save runs on that same blocking pool, so a
        // parked waiter can starve the very holder it is waiting for.
        let contended = fs2::lock_contended_error().kind();
        let mut backoff = std::time::Duration::from_millis(1);
        const MAX_BACKOFF: std::time::Duration = std::time::Duration::from_millis(50);
        loop {
            match file.try_lock_exclusive() {
                Ok(()) => return Ok(Self { _file: file }),
                Err(e) if e.kind() == contended => {
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
                Err(e) => {
                    return Err(A2aJournalError::Io {
                        path: display,
                        reason: e.to_string(),
                    })
                }
            }
        }
    }
}

/// `<path><suffix>`, built from the raw OS path bytes so the sidecar
/// sits beside the journal even for a non-UTF-8 path.
fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    os.push(suffix);
    PathBuf::from(os)
}

/// Open (creating, never truncating) a sidecar lock file on the blocking
/// pool. A lock file's content is irrelevant — only its advisory lock is.
async fn open_sidecar(path: PathBuf) -> Result<std::fs::File, A2aJournalError> {
    let display = path.display().to_string();
    tokio::task::spawn_blocking(move || -> std::io::Result<std::fs::File> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&path)
    })
    .await
    .map_err(|e| A2aJournalError::Io {
        path: display.clone(),
        reason: format!("admission-journal lock task panicked: {e}"),
    })?
    .map_err(|e| A2aJournalError::Io {
        path: display,
        reason: e.to_string(),
    })
}

// ---------------------------------------------------------------------------
// The journal
// ---------------------------------------------------------------------------

/// The durable admission store: one JSON file, one live owner, atomic
/// replaces.
///
/// See the module docs for the ownership contract and its limits.
#[derive(Debug)]
pub struct A2aAdmissionJournal {
    path: PathBuf,
    owner: Arc<JournalOwner>,
    /// Serializes this process's own writers before they contend for the
    /// `.lock` sidecar, so a load→mutate→replace is never interleaved
    /// in-process. `tokio`'s mutex because the body awaits file I/O.
    write_mu: tokio::sync::Mutex<()>,
    recovered: Vec<RecoveredAdmission>,
    /// Arms exactly one durable write to fail without touching the file.
    #[cfg(feature = "testing")]
    fail_next: std::sync::atomic::AtomicBool,
    /// How many atomic replaces this journal has published.
    #[cfg(feature = "testing")]
    writes: std::sync::atomic::AtomicU64,
}

impl A2aAdmissionJournal {
    /// Open (or create) the journal at `path`, taking exclusive
    /// ownership of it for this handle's lifetime.
    ///
    /// Fails with [`A2aJournalError::OwnedElsewhere`] if any live holder
    /// — in this process or another — already owns it, and with
    /// [`A2aJournalError::Corrupt`] if the file exists but does not
    /// parse (an unreadable journal must never be mistaken for an empty
    /// one: that would relaunch paid work and forget a ledger).
    ///
    /// Opening runs the recovery pass: what the previous owner left
    /// unresolved is read into [`recovered`](Self::recovered) and
    /// **nothing is relaunched or rewritten**.
    pub async fn open(path: impl Into<PathBuf>) -> Result<Self, A2aJournalError> {
        let path = path.into();
        let owner_path = sidecar(&path, ".owner");
        let display = path.display().to_string();

        // Create the sidecar once, then never replace it: the journal
        // file is renamed over on every write, so only a stable sidecar
        // can carry a lifetime lock.
        let file = open_sidecar(owner_path.clone()).await?;
        let canonical = tokio::fs::canonicalize(&owner_path)
            .await
            .unwrap_or(owner_path);

        // In-process claim first — see `OWNED`.
        if !OWNED.lock().insert(canonical.clone()) {
            return Err(A2aJournalError::OwnedElsewhere { path: display });
        }
        // One attempt, not a retry loop: a contended owner lock is a
        // live sibling owner, not a transient.
        if file.try_lock_exclusive().is_err() {
            OWNED.lock().remove(&canonical);
            return Err(A2aJournalError::OwnedElsewhere { path: display });
        }
        let owner = Arc::new(JournalOwner {
            sidecar: canonical,
            _lock: file,
        });

        let state = load(&path).await?;
        let recovered = state
            .records
            .values()
            .filter_map(|r| {
                r.state.status().map(|status| RecoveredAdmission {
                    owner: r.owner,
                    task_id: r.task_id.clone(),
                    found: r.state.clone(),
                    status,
                })
            })
            .filter(|r| r.found.is_unresolved())
            .collect();

        Ok(Self {
            path,
            owner,
            write_mu: tokio::sync::Mutex::new(()),
            recovered,
            #[cfg(feature = "testing")]
            fail_next: std::sync::atomic::AtomicBool::new(false),
            #[cfg(feature = "testing")]
            writes: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// The journal file's path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A clone of the ownership handle. Give one to every serve handle,
    /// handler and spawned executor future: while a clone is alive the
    /// owner cannot be released, so nothing that can still write ever
    /// outlives the lock.
    pub fn owner(&self) -> Arc<JournalOwner> {
        Arc::clone(&self.owner)
    }

    /// What the previous owner left unresolved, sampled at
    /// [`open`](Self::open) — `Paid`, `Launched`-without-`Terminal`, and
    /// `Reconcile` records with the status each answers.
    ///
    /// Empty for a fresh journal and for a clean shutdown. Reading it
    /// mutates nothing.
    pub fn recovered(&self) -> &[RecoveredAdmission] {
        &self.recovered
    }

    /// This journal as the configured store.
    pub fn shared(self) -> SharedAdmissionStore {
        Arc::new(self)
    }

    /// **Testing seam**: arm the next durable write to fail with
    /// [`A2aJournalError::Io`] **without modifying the file**, so a
    /// witness can prove the serving path's rule — nothing runs unless
    /// the claim write returned `Ok` — against a real failure rather
    /// than a description of one.
    #[cfg(feature = "testing")]
    pub fn fail_next_write(&self) {
        self.fail_next
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// **Testing seam**: how many atomic replaces this journal has
    /// published. What makes "the launched state and the ledger entry
    /// land in ONE replace" an observation rather than a claim.
    #[cfg(feature = "testing")]
    pub fn durable_writes(&self) -> u64 {
        self.writes.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// in-process mutex → `.lock` → load → check → temp + fsync +
    /// rename. `f` decides; a refusal from it publishes nothing.
    async fn mutate<R>(
        &self,
        f: impl FnOnce(&mut StoreState) -> Result<R, A2aJournalError>,
    ) -> Result<R, A2aJournalError> {
        self.mutate_if(|state| Ok((f(state)?, true))).await
    }

    /// [`mutate`](Self::mutate) for a decision that may conclude in
    /// "nothing changed": `f` returns its result plus whether the state
    /// is dirty, and a clean outcome publishes no write at all.
    async fn mutate_if<R>(
        &self,
        f: impl FnOnce(&mut StoreState) -> Result<(R, bool), A2aJournalError>,
    ) -> Result<R, A2aJournalError> {
        let _serialized = self.write_mu.lock().await;
        let _lock = WriteLock::acquire(&self.path).await?;
        let mut state = load(&self.path).await?;
        let (out, dirty) = f(&mut state)?;
        if dirty {
            self.save(&state).await?;
        }
        Ok(out)
    }

    /// Publish `state`: 0600 temp, `sync_all`, atomic rename. A failure
    /// anywhere leaves the live file exactly as it was.
    async fn save(&self, state: &StoreState) -> Result<(), A2aJournalError> {
        #[cfg(feature = "testing")]
        if self
            .fail_next
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(A2aJournalError::Io {
                path: self.path.display().to_string(),
                reason: "injected write failure (testing seam)".to_string(),
            });
        }

        let io_err = |e: std::io::Error| A2aJournalError::Io {
            path: self.path.display().to_string(),
            reason: e.to_string(),
        };

        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent).await.map_err(io_err)?;
            }
        }

        let bytes = serde_json::to_vec_pretty(&JournalFile::from_state(state)).map_err(|e| {
            A2aJournalError::Io {
                path: self.path.display().to_string(),
                reason: format!("serialize admission journal: {e}"),
            }
        })?;

        // A per-process-unique temp name so two writers never clobber
        // each other's partial file; the rename is still last-wins.
        let tmp = self
            .path
            .with_extension(format!("tmp.{}", std::process::id()));

        // Owner-only (0600) from the start — the journal holds payer
        // keys and quote ids — and truncated, so a stale same-pid temp
        // from a prior crash cannot leave trailing bytes. The mode
        // travels with the inode through the rename. On Windows the
        // per-user directory's inherited ACLs scope access and the rest
        // of the path is identical.
        use tokio::io::AsyncWriteExt;
        let mut opts = tokio::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        // `tokio::fs::OpenOptions::mode` is tokio's own unix-gated
        // inherent method — importing `std::os::unix::fs::OpenOptionsExt`
        // to reach it is redundant, and the unused import is a hard error
        // under the strict lint profile on every unix job. `pins.rs` is
        // the precedent: same call, no import.
        #[cfg(unix)]
        opts.mode(0o600);
        let mut f = opts.open(&tmp).await.map_err(io_err)?;
        let written: Result<(), A2aJournalError> = async {
            f.write_all(&bytes).await.map_err(io_err)?;
            f.flush().await.map_err(io_err)?;
            // Durable before it becomes the live file, so a crash right
            // after the rename can never surface a truncated journal.
            f.sync_all().await.map_err(io_err)?;
            Ok(())
        }
        .await;
        drop(f);

        let result = match written {
            Ok(()) => tokio::fs::rename(&tmp, &self.path).await.map_err(io_err),
            Err(e) => Err(e),
        };
        if result.is_err() {
            // Never leak a `.tmp.<pid>` sibling from a failed write.
            let _ = tokio::fs::remove_file(&tmp).await;
        }
        #[cfg(feature = "testing")]
        if result.is_ok() {
            self.writes
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        result
    }
}

/// Read the journal. A missing file is an empty journal (first run);
/// an unparseable one is [`A2aJournalError::Corrupt`]. No lock: the
/// atomic rename is what prevents a torn read.
async fn load(path: &Path) -> Result<StoreState, A2aJournalError> {
    let bytes = match tokio::fs::read(path).await {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(StoreState::default()),
        Err(e) => {
            return Err(A2aJournalError::Io {
                path: path.display().to_string(),
                reason: e.to_string(),
            })
        }
    };
    let file: JournalFile =
        serde_json::from_slice(&bytes).map_err(|e| A2aJournalError::Corrupt {
            path: path.display().to_string(),
            reason: e.to_string(),
        })?;
    Ok(file.into_state())
}

#[async_trait::async_trait]
impl AdmissionStore for A2aAdmissionJournal {
    async fn lookup(
        &self,
        owner: TaskOwner,
        task_id: &str,
    ) -> Result<Option<AdmissionRecord>, A2aJournalError> {
        Ok(load(&self.path)
            .await?
            .records
            .get(&key(owner, task_id))
            .cloned())
    }

    async fn insert_reserved(
        &self,
        record: AdmissionRecord,
    ) -> Result<InsertOutcome, A2aJournalError> {
        // A lost CAS publishes nothing: the record on disk is already
        // what a write would produce, and a durable replace for a
        // no-op would be an I/O failure this caller cannot act on.
        self.mutate_if(|state| {
            let out = state.insert_reserved(record)?;
            let dirty = matches!(out, InsertOutcome::Inserted);
            Ok((out, dirty))
        })
        .await
    }

    async fn in_flight(&self, service_id: &str, now: u64) -> Result<u64, A2aJournalError> {
        Ok(load(&self.path).await?.in_flight(service_id, now))
    }

    async fn transition(
        &self,
        owner: TaskOwner,
        task_id: &str,
        from: &[StateTag],
        to: AdmissionState,
        now: u64,
    ) -> Result<(), A2aJournalError> {
        self.mutate(|state| state.transition(owner, task_id, from, to, now))
            .await
    }

    async fn note(
        &self,
        owner: TaskOwner,
        task_id: &str,
        note: AttemptNote,
    ) -> Result<(), A2aJournalError> {
        self.mutate(|state| state.note(owner, task_id, note)).await
    }

    async fn delete_reservation(
        &self,
        owner: TaskOwner,
        task_id: &str,
    ) -> Result<(), A2aJournalError> {
        self.mutate(|state| state.delete_reservation(owner, task_id))
            .await
    }

    async fn claim_launch(
        &self,
        owner: TaskOwner,
        task_id: &str,
        now: u64,
    ) -> Result<LaunchLedgerEntry, A2aJournalError> {
        // ONE `mutate`, therefore one atomic replace: the `Launched`
        // state and the ledger entry become visible together or not at
        // all.
        self.mutate(|state| state.claim_launch(owner, task_id, now))
            .await
    }

    async fn record_terminal(
        &self,
        owner: TaskOwner,
        task_id: &str,
        state: TaskState,
        now: u64,
    ) -> Result<(), A2aJournalError> {
        self.mutate(|s| s.record_terminal(owner, task_id, state, now))
            .await
    }

    async fn resolve(
        &self,
        owner: TaskOwner,
        task_id: &str,
        state: TaskState,
        now: u64,
    ) -> Result<(), A2aJournalError> {
        self.mutate(|s| s.resolve(owner, task_id, state, now)).await
    }

    async fn ledger_has(&self, owner: TaskOwner, task_id: &str) -> Result<bool, A2aJournalError> {
        Ok(load(&self.path)
            .await?
            .ledger
            .contains_key(&key(owner, task_id)))
    }

    async fn unresolved(&self) -> Result<Vec<AdmissionRecord>, A2aJournalError> {
        Ok(load(&self.path).await?.unresolved())
    }

    async fn forget(&self, owner: TaskOwner, task_id: &str) -> Result<bool, A2aJournalError> {
        self.mutate(|s| s.forget(owner, task_id)).await
    }

    async fn prune(&self, now: u64) -> Result<u64, A2aJournalError> {
        self.mutate(|s| Ok(s.prune(now))).await
    }
}

// ---------------------------------------------------------------------------
// The in-memory sibling
// ---------------------------------------------------------------------------

/// The process-lifetime admission store: the same state table, the same
/// transition restrictions, the same capacity accounting and ledger —
/// with no file, no lock, and no ownership contract.
///
/// What an all-free catalog runs on. A free admission's evidence is
/// worth exactly what the process it lives in is worth: nothing survives
/// a restart, and nothing needs to, because no money was involved. A
/// paid catalog entry requires the journal (the serving path refuses to
/// configure otherwise).
#[derive(Debug, Default)]
pub struct A2aAdmissions {
    state: Mutex<StoreState>,
}

impl A2aAdmissions {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// This store as the configured store.
    pub fn shared(self) -> SharedAdmissionStore {
        Arc::new(self)
    }
}

#[async_trait::async_trait]
impl AdmissionStore for A2aAdmissions {
    async fn lookup(
        &self,
        owner: TaskOwner,
        task_id: &str,
    ) -> Result<Option<AdmissionRecord>, A2aJournalError> {
        Ok(self.state.lock().records.get(&key(owner, task_id)).cloned())
    }

    async fn insert_reserved(
        &self,
        record: AdmissionRecord,
    ) -> Result<InsertOutcome, A2aJournalError> {
        self.state.lock().insert_reserved(record)
    }

    async fn in_flight(&self, service_id: &str, now: u64) -> Result<u64, A2aJournalError> {
        Ok(self.state.lock().in_flight(service_id, now))
    }

    async fn transition(
        &self,
        owner: TaskOwner,
        task_id: &str,
        from: &[StateTag],
        to: AdmissionState,
        now: u64,
    ) -> Result<(), A2aJournalError> {
        self.state.lock().transition(owner, task_id, from, to, now)
    }

    async fn note(
        &self,
        owner: TaskOwner,
        task_id: &str,
        note: AttemptNote,
    ) -> Result<(), A2aJournalError> {
        self.state.lock().note(owner, task_id, note)
    }

    async fn delete_reservation(
        &self,
        owner: TaskOwner,
        task_id: &str,
    ) -> Result<(), A2aJournalError> {
        self.state.lock().delete_reservation(owner, task_id)
    }

    async fn claim_launch(
        &self,
        owner: TaskOwner,
        task_id: &str,
        now: u64,
    ) -> Result<LaunchLedgerEntry, A2aJournalError> {
        // One critical section under the mutex: the same atomicity the
        // journal gets from one atomic replace.
        self.state.lock().claim_launch(owner, task_id, now)
    }

    async fn record_terminal(
        &self,
        owner: TaskOwner,
        task_id: &str,
        state: TaskState,
        now: u64,
    ) -> Result<(), A2aJournalError> {
        self.state
            .lock()
            .record_terminal(owner, task_id, state, now)
    }

    async fn resolve(
        &self,
        owner: TaskOwner,
        task_id: &str,
        state: TaskState,
        now: u64,
    ) -> Result<(), A2aJournalError> {
        self.state.lock().resolve(owner, task_id, state, now)
    }

    async fn ledger_has(&self, owner: TaskOwner, task_id: &str) -> Result<bool, A2aJournalError> {
        Ok(self.state.lock().ledger.contains_key(&key(owner, task_id)))
    }

    async fn unresolved(&self) -> Result<Vec<AdmissionRecord>, A2aJournalError> {
        Ok(self.state.lock().unresolved())
    }

    async fn forget(&self, owner: TaskOwner, task_id: &str) -> Result<bool, A2aJournalError> {
        self.state.lock().forget(owner, task_id)
    }

    async fn prune(&self, now: u64) -> Result<u64, A2aJournalError> {
        Ok(self.state.lock().prune(now))
    }
}

// ---------------------------------------------------------------------------
// TaskOwner on disk
// ---------------------------------------------------------------------------

fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn from_hex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// `TaskOwner` is a registry key in `a2a`, not a persisted type, so its
/// on-disk encoding lives here: a tagged object rather than a bare
/// index, so an entity owner stays readable in an operator's journal and
/// a future owner kind is an additive tag.
mod owner_repr {
    use super::{from_hex32, to_hex, TaskOwner};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Serialize, Deserialize)]
    #[serde(tag = "kind", rename_all = "snake_case")]
    enum Repr {
        Local,
        Peer { node: u64 },
        Entity { entity: String },
    }

    pub(super) fn serialize<S: Serializer>(owner: &TaskOwner, s: S) -> Result<S::Ok, S::Error> {
        match owner {
            TaskOwner::Local => Repr::Local,
            TaskOwner::Peer(node) => Repr::Peer { node: *node },
            TaskOwner::Entity(id) => Repr::Entity { entity: to_hex(id) },
        }
        .serialize(s)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<TaskOwner, D::Error> {
        Ok(match Repr::deserialize(d)? {
            Repr::Local => TaskOwner::Local,
            Repr::Peer { node } => TaskOwner::Peer(node),
            Repr::Entity { entity } => TaskOwner::Entity(
                from_hex32(&entity)
                    .ok_or_else(|| serde::de::Error::custom("entity is not 32 hex bytes"))?,
            ),
        })
    }
}
