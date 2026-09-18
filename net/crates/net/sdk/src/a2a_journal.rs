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
//! # Identity, not a state tag
//!
//! Every record carries an immutable, monotonic
//! [`generation`](AdmissionRecord::generation) minted by the store when
//! the row is created, and its [`identity`](AdmissionRecord::identity)
//! is that generation plus the `admission_id` a purchase hash commits
//! to and the `commitment` of the brief. **Every write that lands after
//! an await re-presents the record it decided against**
//! ([`AdmissionStore::redeem`], [`AdmissionStore::open_decision`],
//! [`AdmissionStore::close_decision`],
//! [`AdmissionStore::delete_reservation`]) and the store refuses it
//! unless the row under that key is still the same incarnation — not
//! merely because the state tag still matches. A replacement row
//! (reprepare, or prune-then-reprepare) mints a **new** generation, so a
//! decision taken against an older one can never land on it. The
//! generation counter is persisted and never rewound by pruning, a
//! recreated row or a restart, so an incarnation number is never reused.
//!
//! A refused post-await write is **retryable**, never a verdict about
//! money — and when the refused write carries a redemption the gate
//! already granted, that evidence is retained against the **original**
//! admission as a detached record (see below) rather than dropped or
//! attached to the replacement.
//!
//! # Capacity is held across the decision, and decided on the store's clock
//!
//! [`AdmissionStore::admit_reserved`] counts what holds capacity and
//! inserts (or re-acquires) the reservation **in one transaction**, so
//! two prepares of distinct tasks cannot both pass a ceiling of one.
//! [`AdmissionStore::open_decision`] then marks the record
//! [`deciding`](AdmissionRecord::deciding): it holds its slot for as
//! long as the decision runs, whatever its reservation clock says, and
//! it is excluded from pruning and from a sibling's
//! `delete_reservation`. So an awaited preflight or redemption cannot
//! have its slot taken by a competitor and then land against a stale
//! time sample.
//!
//! Every caller passes its own `now`, and a submit samples that clock
//! *before* it awaits a lookup and a decision acquisition — so the
//! sample can be older than a competitor's whole reservation by the time
//! it is used. Each capacity/expiry decision therefore raises the given
//! sample to a **floor the store can prove**: no live row of that
//! service can have been stamped in the future, so the latest
//! `updated_at` among them is a lower bound on the real clock. Whoever
//! took a released slot stamped the row it took, which is exactly the
//! evidence that the previous holder's window ended. Retention is left
//! on the caller's clock, where a stale sample only ever keeps a record
//! longer.
//!
//! The hold belongs to the owner that took it — liveness *is* the lock —
//! so a successor clears every inherited hold at
//! [`open`](A2aAdmissionJournal::open).
//!
//! # Detached admissions, and naming one
//!
//! An admission whose row left its key — aged out of reservation
//! retention, or replaced — is kept as a **detached** record when it was
//! purchasable (a quote may have committed to its purchase hash and can
//! still be presented). A redemption that arrives for one is recorded
//! *there*, in the unresolved-financial class: reported by
//! [`AdmissionStore::unresolved`], never pruned automatically, and
//! closable only by an operator.
//!
//! One key can therefore carry **two charges** — a retained redemption
//! and the live replacement that took its place. So the exits come in
//! pairs: [`AdmissionStore::resolve`] and
//! [`AdmissionStore::claim_launch`] name a record by key and refuse the
//! moment that key is ambiguous, while
//! [`AdmissionStore::resolve_exact`] and
//! [`AdmissionStore::claim_launch_exact`] name the
//! [`identity`](AdmissionRecord::identity) the queue reported and touch
//! nothing else. Being pinned against pruning is not the same as being
//! unreplaceable: an operator's resolve-then-forget mid-decision, and a
//! re-prepare landing after it, leave a different admission under that
//! key — which is why the launch claim re-presents its incarnation
//! rather than trusting the key.
//!
//! # Writes complete, then publish durably
//!
//! **Every** writer — the ordinary mutation and the recovery
//! publication at [`open`](A2aAdmissionJournal::open), which are the
//! only two in this file — runs its whole publish (temp file,
//! `sync_all`, rename, directory barrier) in a
//! **cancellation-independent worker that owns the serialization guard,
//! the transaction lock and the ownership handle** until its I/O has
//! actually completed. Aborting the future that started a write
//! therefore cannot release a guard while filesystem work is still in
//! flight, and a successor's write cannot reuse the temp path (created
//! exclusively, named after the full destination filename) or race the
//! rename.
//!
//! Holding the **owner** matters most for recovery, which publishes a
//! whole snapshot: an abandoned one would otherwise land on top of
//! everything a successor durably added in the meantime and read back as
//! if that work had never happened. A unique temp name is no defence
//! there, because the destination is the journal itself — so a
//! successor's `open` is refused with
//! [`OwnedElsewhere`](A2aJournalError::OwnedElsewhere) until the
//! abandoned writer has finished.
//!
//! An [`A2aJournalError::Io`] leaves the file **untouched** (the rename
//! is what publishes a write), which is what lets the serving path keep
//! its rule: *nothing runs unless the claim write returned `Ok`*. An
//! [`A2aJournalError::Ambiguous`] is the one outcome that does **not**
//! promise that: the rename landed and only its durability is
//! unconfirmed, so the write may survive a power loss. It is retryable
//! and must never be read as "the file is unchanged".

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
    /// The record this decision was taken against is no longer the one
    /// under its key: it was replaced by a later incarnation, or it left
    /// the key entirely. The write was **not** applied to the
    /// replacement.
    ///
    /// Distinct from [`Conflict`](Self::Conflict) because it is not a
    /// programming error and not a state-tag mismatch — the tag may
    /// match perfectly. If the refused write carried a redemption, that
    /// evidence was retained as a detached record of the original
    /// admission and is reported by [`AdmissionStore::unresolved`].
    #[error(
        "admission {admission_id:?} of task {task_id:?} (generation {generation}) was \
         superseded; the decision taken against it was not applied"
    )]
    Superseded {
        /// The task id whose row was replaced.
        task_id: String,
        /// The admission id the decision was taken against.
        admission_id: Option<String>,
        /// The incarnation the decision was taken against.
        generation: u64,
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
    /// The write was **published** — the rename landed and a reader sees
    /// it — but its durability could not be confirmed, so a power loss
    /// may or may not recover it.
    ///
    /// The one refusal that must never be read as "the file is
    /// unchanged": a caller that treats this like [`Io`](Self::Io) would
    /// conclude nothing happened while the store already moved.
    /// Retryable — a retry re-reads the published state and either
    /// confirms it or applies the write again.
    #[error("admission journal at {path} was published but not confirmed durable: {reason}")]
    Ambiguous {
        /// The path involved.
        path: String,
        /// Why durability could not be confirmed.
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
    /// The immutable incarnation number of this row, minted by the store
    /// when it was created and never rewritten. A replacement row under
    /// the same key always gets a higher one, and the counter is
    /// persisted, so an incarnation is never reused after a prune, a
    /// recreated row, or a restart.
    ///
    /// `0` on a record a caller built for insertion (the store mints the
    /// real one) and on a row written before this field existed — in
    /// which case the store's counter is raised past every generation it
    /// finds, so the next mint is still unique.
    #[serde(default)]
    pub generation: u64,
    /// Whether a decision (an awaited preflight, a redemption) is open
    /// against this exact incarnation. A deciding record holds its
    /// capacity slot whatever its reservation clock says, and neither
    /// [`prune`](AdmissionStore::prune) nor a sibling's
    /// [`delete_reservation`](AdmissionStore::delete_reservation) may
    /// remove it.
    ///
    /// Cleared by whatever ends the decision: every state change, an
    /// explicit [`close_decision`](AdmissionStore::close_decision), or a
    /// successor owner at [`open`](A2aAdmissionJournal::open) — the hold
    /// belongs to the owner that took it.
    #[serde(default)]
    pub deciding: bool,
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
            // Minted by the store on insert: a caller cannot choose its
            // own incarnation number.
            generation: 0,
            deciding: false,
        }
    }

    /// What a post-await write must re-present: this exact incarnation,
    /// the admission id a purchase hash commits to, and the brief's
    /// commitment.
    pub fn identity(&self) -> AdmissionIdentity {
        AdmissionIdentity {
            owner: self.owner,
            task_id: self.task_id.clone(),
            generation: self.generation,
            admission_id: self.admission_id.clone(),
            commitment: self.commitment.clone(),
        }
    }

    /// Whether this record still holds capacity at `now`: every state
    /// except a lapsed reservation, a recorded terminal, and a revoked
    /// admission — and a lapsed reservation **does** still hold it while
    /// a decision is open against it, which is what keeps a competitor
    /// from taking the slot an awaited redemption is still using.
    pub fn holds_capacity(&self, now: u64) -> bool {
        match &self.state {
            AdmissionState::Reserved { expires_at } => *expires_at > now || self.deciding,
            AdmissionState::Paid { .. } | AdmissionState::Launched { .. } => true,
            AdmissionState::Terminal { .. } | AdmissionState::Reconcile { .. } => false,
        }
    }

    /// Whether a purchase could still be presented against this
    /// admission: it is priced and carries the provider-minted
    /// `admission_id` a quote's input hash commits to. Such a record is
    /// retained as a detached admission rather than dropped when its row
    /// leaves its key.
    fn purchasable(&self) -> bool {
        self.paid && self.admission_id.is_some()
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

/// The immutable identity of one admission: which incarnation of which
/// `(owner, task id)`, the provider-minted `admission_id` a purchase
/// hash commits to, and the commitment of the brief.
///
/// This — not the state tag — is what a write landing after an await
/// must re-present, and what the store compares before applying it.
/// Built with [`AdmissionRecord::identity`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionIdentity {
    /// The submitter.
    pub owner: TaskOwner,
    /// The task id.
    pub task_id: String,
    /// The incarnation this decision was taken against.
    pub generation: u64,
    /// The admission id it was taken against.
    pub admission_id: Option<String>,
    /// The commitment it was taken against.
    pub commitment: String,
}

impl AdmissionIdentity {
    /// Whether `record` is still the incarnation this identity names.
    ///
    /// All three fields, not just the generation: the generation is what
    /// distinguishes two incarnations of the same key, and the admission
    /// id and commitment are what a purchase hash was computed over, so
    /// a mismatch in any of them means the decision was taken against
    /// something else.
    pub fn matches(&self, record: &AdmissionRecord) -> bool {
        self.generation == record.generation
            && self.admission_id == record.admission_id
            && self.commitment == record.commitment
    }

    fn superseded(&self) -> A2aJournalError {
        A2aJournalError::Superseded {
            task_id: self.task_id.clone(),
            admission_id: self.admission_id.clone(),
            generation: self.generation,
        }
    }
}

/// What [`AdmissionStore::admit_reserved`] decided, in one transaction
/// with the capacity count that decided it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmitOutcome {
    /// The reservation holds capacity: either freshly inserted (with a
    /// newly minted generation) or a lapsed one whose slot was
    /// re-acquired, keeping its generation and admission id. Carries the
    /// stored record.
    Admitted(Box<AdmissionRecord>),
    /// A record is already there and no capacity decision was needed — a
    /// live reservation, an admission past `Reserved`, or a different
    /// brief under the same id. Nothing was written; the caller
    /// dispatches on it exactly as D2 P4/S4 prescribe.
    Existing(Box<AdmissionRecord>),
    /// The service is at `max_in_flight` and nothing was written.
    Busy,
}

/// What [`AdmissionStore::open_decision`] decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecisionOutcome {
    /// The decision is open: this record holds its capacity slot until
    /// the decision ends, whatever its reservation clock says. Carries
    /// the stored record, whose identity every later write of this
    /// decision must re-present.
    Open(Box<AdmissionRecord>),
    /// The reservation had lapsed and the service is at
    /// `max_in_flight`, so its slot could not be re-acquired. Nothing
    /// was written and no decision is open.
    Busy,
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
///
/// `transition` is **identity-unbound** — it names a record by key and a
/// state tag — so the serving path does not use it for the financial
/// row: [`AdmissionStore::redeem`] is the identity-bound verb that
/// writes `Paid`. What is left here for an operator or a test fixture
/// refuses the financial row outright once the key is known to have
/// held a different purchasable admission, because a payment presented
/// with no identity cannot then be attributed to an incarnation.
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

    /// Admit a reservation **under a capacity ceiling, in one
    /// transaction**: count what holds capacity for `record`'s service
    /// at `now` and only then insert the fresh row — or re-acquire a
    /// lapsed one that is already there.
    ///
    /// This is what makes a published `max_in_flight` mean something.
    /// Counting with [`in_flight`](Self::in_flight) and then inserting
    /// with [`insert_reserved`](Self::insert_reserved) is two
    /// transactions, and concurrent prepares of **distinct** task ids
    /// each see a count taken before any of them wrote: registry
    /// deduplication protects identical ids and nothing protects
    /// competing ones. The ceiling has to be enforced where the write
    /// happens.
    ///
    /// A live reservation, or any record past `Reserved`, needs no
    /// capacity decision and comes back as
    /// [`Existing`](AdmitOutcome::Existing) with nothing written — so a
    /// re-prepare cannot extend a reservation that is still good.
    async fn admit_reserved(
        &self,
        record: AdmissionRecord,
        max_in_flight: u64,
        now: u64,
    ) -> Result<AdmitOutcome, A2aJournalError>;

    /// Open a decision against exactly `admission`: the awaited window
    /// in which a preflight runs and a payment is redeemed.
    ///
    /// Identity-bound: refuses with
    /// [`Superseded`](A2aJournalError::Superseded) unless the row under
    /// the key is still that incarnation. While the decision is open the
    /// record holds its capacity slot whatever its reservation clock
    /// says, and neither [`prune`](Self::prune) nor
    /// [`delete_reservation`](Self::delete_reservation) may remove it —
    /// so an expiry mid-decision cannot hand the slot to a competitor
    /// and leave this decision landing against a stale time sample.
    ///
    /// A reservation that has **already** lapsed re-acquires its slot
    /// here, in the same transaction, or the call answers
    /// [`Busy`](DecisionOutcome::Busy) with nothing written.
    async fn open_decision(
        &self,
        admission: &AdmissionRecord,
        max_in_flight: u64,
        now: u64,
    ) -> Result<DecisionOutcome, A2aJournalError>;

    /// End a decision without changing the state: release the capacity
    /// hold [`open_decision`](Self::open_decision) took.
    ///
    /// Idempotent and forgiving by design — it is called on every
    /// refusal path, including those whose own write already cleared the
    /// hold, and a record that has since been superseded or removed has
    /// no hold of this decision's to release. `Ok(())` either way;
    /// nothing is published when there is nothing to clear.
    async fn close_decision(
        &self,
        admission: &AdmissionRecord,
        now: u64,
    ) -> Result<(), A2aJournalError>;

    /// Record a redeemed payment against exactly `admission`:
    /// `Reserved → Paid`, identity-bound.
    ///
    /// This is the **only** verb the serving path writes a financial
    /// state with, because it is the only one that re-presents what the
    /// decision was taken against. Three outcomes, not two:
    ///
    /// | Found under the key | Result |
    /// |---|---|
    /// | the same incarnation, `Reserved` and holding capacity | `Paid` is written |
    /// | a different incarnation, or nothing | [`Superseded`](A2aJournalError::Superseded) — **and the redemption is retained as a detached record of the original admission**, in the unresolved-financial class |
    /// | the same incarnation in a state that cannot be paid | [`Conflict`](A2aJournalError::Conflict) |
    ///
    /// The middle row is the one that matters: refusing protects the
    /// replacement, but a payment the gate already granted is a fact.
    /// It is never attached to the replacement and never dropped behind
    /// a retryable error — it is retained where an operator can see it
    /// through [`unresolved`](Self::unresolved) and close it with
    /// [`resolve`](Self::resolve).
    async fn redeem(
        &self,
        admission: &AdmissionRecord,
        quote_id: String,
        payer: [u8; 32],
        now: u64,
    ) -> Result<(), A2aJournalError>;

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
    ///
    /// Identity-bound, like every other post-await write: the decision
    /// to abandon the work was taken against one incarnation, and it may
    /// not delete a replacement. It **may** delete its own deciding
    /// record — this is the decision ending — but never a sibling's.
    async fn delete_reservation(&self, admission: &AdmissionRecord) -> Result<(), A2aJournalError>;

    /// Claim the launch of exactly `admission`: `Paid → Launched` (paid)
    /// or `Reserved → Launched` (free), **and** the ledger entry, in one
    /// atomic replace.
    ///
    /// Either both land or neither does; there is no instant at which
    /// the store holds a launched admission with no ledger entry. The
    /// serving path spawns only after this returns `Ok`.
    ///
    /// Identity-bound, like every other post-await write: the row under
    /// the key must still be the incarnation the decision was opened on,
    /// or the claim is
    /// [`Superseded`](A2aJournalError::Superseded) with nothing written.
    /// Being excluded from pruning is *not* the same as being
    /// unreplaceable — an operator's `resolve` + [`forget`](Self::forget)
    /// mid-decision, and a re-prepare landing after them, leave a
    /// different admission under that key. Launching it would run one
    /// brief and write another's identity into the never-pruned ledger.
    ///
    /// Refuses a record that no longer holds capacity: a launch claim
    /// that arrives after its reservation lapsed would be running work
    /// whose slot may already belong to somebody else. A `Paid` record
    /// always holds capacity, and an open decision holds it for a free
    /// one, so this only ever refuses a claim nothing was holding open.
    async fn claim_launch_exact(
        &self,
        admission: &AdmissionRecord,
        now: u64,
    ) -> Result<LaunchLedgerEntry, A2aJournalError>;

    /// The same claim named by key alone, for an **operator or a
    /// fixture** recording a launch it established out of band.
    ///
    /// Identity-unbound, and bounded the same way
    /// [`transition`](Self::transition) is: once the key is known to have
    /// held a different purchasable admission, no launch presented
    /// without an identity can be attributed to an incarnation, so it is
    /// refused. The serving path uses
    /// [`claim_launch_exact`](Self::claim_launch_exact) instead.
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

    /// **Operator only**: resolve the one unresolved financial record of
    /// `(owner, task_id)` — `Paid`, `Launched` or `Reconcile` →
    /// `Terminal`. The one exit from the never-pruned class; after it,
    /// the resolved-result retention rule applies.
    ///
    /// Resolves the live row under the key when that row is unresolved;
    /// otherwise the unresolved **detached** admission of the same key —
    /// a redemption retained by [`redeem`](Self::redeem) after its
    /// admission was superseded has no other exit, and the live row (a
    /// later incarnation) is not an operator's to close.
    ///
    /// **Refuses when the key carries more than one**: a retained
    /// redemption and the live replacement that took its place are two
    /// charges, and a call that names only the key cannot say which one
    /// the operator meant. The refusal lists every candidate with its
    /// generation, each of which
    /// [`resolve_exact`](Self::resolve_exact) closes on its own.
    async fn resolve(
        &self,
        owner: TaskOwner,
        task_id: &str,
        state: TaskState,
        now: u64,
    ) -> Result<(), A2aJournalError>;

    /// **Operator only**: resolve the **exact** incarnation `identity`
    /// names, live or retained.
    ///
    /// This is what makes [`unresolved`](Self::unresolved) a usable
    /// queue: every row it reports carries its own
    /// [`identity`](AdmissionRecord::identity), and handing that back
    /// closes that record and nothing else — never a neighbour under the
    /// same key, and never a live replacement while a historical charge
    /// was the one being closed. A generation that is gone, or whose
    /// admission id or commitment does not match, is
    /// [`Superseded`](A2aJournalError::Superseded).
    async fn resolve_exact(
        &self,
        identity: &AdmissionIdentity,
        state: TaskState,
        now: u64,
    ) -> Result<(), A2aJournalError>;

    /// Whether `(owner, task_id)` ever launched. Consulted at D2 P4/S4:
    /// a ledger hit with no record answers `Retired`, and the redeem
    /// step is never reached, so a retired result can never make its
    /// payment reusable.
    async fn ledger_has(&self, owner: TaskOwner, task_id: &str) -> Result<bool, A2aJournalError>;

    /// Every record in the unresolved financial class, for an operator
    /// view — live rows and retained **detached** admissions alike,
    /// because a redemption whose admission was superseded is exactly
    /// the thing an operator must see. Deterministically ordered: live
    /// rows by key, then detached ones by key and incarnation.
    async fn unresolved(&self) -> Result<Vec<AdmissionRecord>, A2aJournalError>;

    /// Drop a **terminal** result immediately, once the requester has
    /// read it (the `forget` / `evict_terminal` path). Refuses every
    /// other state and never touches the ledger. Returns whether a
    /// record was removed.
    async fn forget(&self, owner: TaskOwner, task_id: &str) -> Result<bool, A2aJournalError>;

    /// Apply the retention classes at `now`, returning how many rows
    /// left the live table:
    ///
    /// | Class | States | Pruned |
    /// |---|---|---|
    /// | Resolved result | `Terminal` | `retention_secs` after `updated_at` |
    /// | Unpaid reservation | `Reserved`, notes included | `reservation_retention_secs` after `updated_at` |
    /// | Deciding reservation | `Reserved` with a decision open | **never** |
    /// | Unresolved financial | `Paid`, `Launched`, `Reconcile` | **never** |
    ///
    /// A pruned reservation that was **purchasable** (priced, with a
    /// provider-minted admission id a quote may have committed to) is
    /// retained as a detached admission rather than forgotten, so a
    /// payment that arrives for it is attributable; it is dropped once
    /// its own resolved-result window has passed. Either way it is gone
    /// from the live table and counted here.
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
    /// Admissions whose row left its key — aged out of reservation
    /// retention, or superseded by a later incarnation — kept because a
    /// quote committed to their purchase hash can still be presented.
    /// Keyed by `(key, generation)`, so two incarnations of one task are
    /// distinct rows and neither can be mistaken for the live one.
    detached: BTreeMap<(Key, u64), AdmissionRecord>,
    /// The next incarnation number to mint. Persisted and monotonic: a
    /// prune, a recreated row or a restart never rewinds it, so an
    /// incarnation is never reused and a decision taken against an old
    /// one can never match a new one.
    next_generation: u64,
    /// The instant capacity for a service was last **acquired**, by any
    /// route — a fresh insertion, or an existing reservation taking its
    /// slot back through [`open_decision`].
    ///
    /// This exists because `updated_at` cannot carry it. Reacquisition
    /// deliberately does not rewrite `updated_at`: that field drives
    /// retention, and advancing it there would keep rows past the
    /// window the operator published. So a floor derived from
    /// `max(updated_at)` alone is blind to exactly the event that
    /// matters here, and a caller holding a pre-expiry sample could
    /// walk back in and find its own lapsed reservation still live
    /// while the slot had already been taken — two rows over a
    /// `max_in_flight` of one.
    ///
    /// Per service, because capacity is per service. Monotonic within a
    /// service, and only ever raises the clock a capacity decision is
    /// taken against; retention still uses the caller's own clock,
    /// where a stale sample can only keep a row longer, which is the
    /// safe direction.
    slot_floor: BTreeMap<String, u64>,
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

    /// The next incarnation number, advancing the counter.
    fn mint_generation(&mut self) -> u64 {
        // 1-based: `0` is what a caller-built record and a pre-generation
        // row carry, and neither may ever compare equal to a minted one.
        self.next_generation = self.next_generation.saturating_add(1);
        self.next_generation
    }

    fn insert_reserved(
        &mut self,
        mut record: AdmissionRecord,
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
        record.generation = self.mint_generation();
        record.deciding = false;
        self.records.insert(k, record);
        Ok(InsertOutcome::Inserted)
    }

    /// A floor under `now`, proved by the store's own durable rows.
    ///
    /// Every record of `service_id` was stamped at a moment the clock had
    /// already reached its `updated_at`, and time does not run backwards,
    /// so the latest of those stamps is a lower bound on the real clock.
    ///
    /// This is what makes a **pre-await** clock sample safe. A submit
    /// samples `now`, then awaits a lookup and a decision acquisition;
    /// by the time the decision is taken, the reservation that sample
    /// called live may have lapsed and a competitor may already hold the
    /// slot it released. Whoever acquired that slot stamped the row it
    /// acquired it on — which is exactly the evidence that our window
    /// ended — so a stale sample can never revive a released slot, and
    /// the caller is told `Busy` (retryable) instead of being handed a
    /// hold that puts the service over its published ceiling.
    ///
    /// Read **only** by the capacity/expiry decisions. Retention keeps
    /// using the caller's own clock: a floor there would drop rows
    /// earlier than the operator asked for, and a stale sample can only
    /// ever keep a row longer, which is the safe direction.
    fn slot_now(&self, service_id: &str, now: u64) -> u64 {
        // Three sources, all of which are evidence that the service's
        // clock has moved past the caller's sample:
        //   * the caller's own `now`;
        //   * the last write to any of this service's live rows;
        //   * the last time capacity was ACQUIRED for this service.
        // The third is not implied by the second. A reacquisition
        // through `open_decision` takes a slot without writing
        // `updated_at`, by design, so it is invisible to the second.
        let written = self
            .records
            .values()
            .filter(|r| r.service_id == service_id)
            .fold(now, |floor, r| floor.max(r.updated_at));
        written.max(self.slot_floor.get(service_id).copied().unwrap_or(0))
    }

    /// Record that capacity for `service_id` was acquired at `at`.
    ///
    /// Called on every acquisition, not merely on fresh insertion —
    /// that asymmetry is exactly what let a reacquisition go unrecorded.
    fn note_acquisition(&mut self, service_id: &str, at: u64) {
        let slot = self.slot_floor.entry(service_id.to_string()).or_insert(0);
        *slot = (*slot).max(at);
    }

    /// Capacity + write, one critical section. `exclude` is the key of a
    /// record the caller is about to change and whose own slot therefore
    /// must not count against it.
    fn at_capacity(&self, service_id: &str, max_in_flight: u64, now: u64, exclude: &Key) -> bool {
        let held = self
            .records
            .iter()
            .filter(|(k, r)| *k != exclude && r.service_id == service_id && r.holds_capacity(now))
            .count() as u64;
        held >= max_in_flight
    }

    fn admit_reserved(
        &mut self,
        candidate: AdmissionRecord,
        max_in_flight: u64,
        now: u64,
    ) -> Result<AdmitOutcome, A2aJournalError> {
        if candidate.state.tag() != StateTag::Reserved {
            return Err(conflict(format!(
                "an admission may only be admitted as reserved, not {}",
                candidate.state.tag().as_str()
            )));
        }
        let k = key(candidate.owner, &candidate.task_id);
        // Capacity is decided against the floor, never the raw sample:
        // a prepare that sampled its clock before this transaction must
        // not be able to call a lapsed reservation live, nor undercount
        // the holders it is competing with.
        let slot_now = self.slot_now(&candidate.service_id, now);
        let expires_at = match candidate.state {
            AdmissionState::Reserved { expires_at } => expires_at,
            // Guarded above.
            _ => return Err(conflict("unreachable admission state")),
        };
        match self.records.get(&k) {
            // A different brief under the same id, or an admission past
            // `Reserved`: no capacity decision to make here.
            Some(found)
                if found.commitment != candidate.commitment
                    || found.state.tag() != StateTag::Reserved =>
            {
                Ok(AdmitOutcome::Existing(Box::new(found.clone())))
            }
            // Still holding its slot: idempotent, and deliberately NOT
            // refreshed — re-preparing must not be a way to hold capacity
            // indefinitely.
            Some(found) if found.holds_capacity(slot_now) => {
                Ok(AdmitOutcome::Existing(Box::new(found.clone())))
            }
            // Lapsed: re-acquire the slot in this same transaction,
            // keeping the incarnation and the admission id a quote may
            // already commit to.
            Some(_) => {
                if self.at_capacity(&candidate.service_id, max_in_flight, slot_now, &k) {
                    return Ok(AdmitOutcome::Busy);
                }
                let service_id = candidate.service_id.clone();
                let found = self.record_mut(candidate.owner, &candidate.task_id)?;
                found.state = AdmissionState::Reserved { expires_at };
                found.updated_at = now;
                let admitted = Box::new(found.clone());
                self.note_acquisition(&service_id, slot_now);
                Ok(AdmitOutcome::Admitted(admitted))
            }
            None => {
                if self.at_capacity(&candidate.service_id, max_in_flight, slot_now, &k) {
                    return Ok(AdmitOutcome::Busy);
                }
                let mut record = candidate;
                record.generation = self.mint_generation();
                record.deciding = false;
                self.note_acquisition(&record.service_id.clone(), slot_now);
                self.records.insert(k, record.clone());
                Ok(AdmitOutcome::Admitted(Box::new(record)))
            }
        }
    }

    /// The live row, if it is still the incarnation `identity` names.
    fn matching_mut(
        &mut self,
        identity: &AdmissionIdentity,
    ) -> Result<&mut AdmissionRecord, A2aJournalError> {
        let k = key(identity.owner, &identity.task_id);
        match self.records.get_mut(&k) {
            Some(found) if identity.matches(found) => Ok(found),
            // Superseded, not a conflict: the tag may match perfectly and
            // the caller did nothing wrong.
            _ => Err(identity.superseded()),
        }
    }

    fn open_decision(
        &mut self,
        admission: &AdmissionRecord,
        max_in_flight: u64,
        now: u64,
    ) -> Result<DecisionOutcome, A2aJournalError> {
        let identity = admission.identity();
        let k = key(identity.owner, &identity.task_id);
        // Whether this reservation still owns its slot is decided against
        // the store's own floor, under the same lock as the acquisition
        // below. The caller sampled `now` before it awaited its way here,
        // and a released slot may already have been taken: a sample from
        // before that must not be allowed to revive it.
        let slot_now = self.slot_now(&admission.service_id, now);
        let lapsed = {
            let found = self.matching_mut(&identity)?;
            match &found.state {
                AdmissionState::Reserved { .. } | AdmissionState::Paid { .. } => {
                    !found.holds_capacity(slot_now)
                }
                other => {
                    return Err(conflict(format!(
                        "task {:?} is {}, which is not an admission a decision can open on",
                        identity.task_id,
                        other.tag().as_str()
                    )))
                }
            }
        };
        if lapsed && self.at_capacity(&admission.service_id, max_in_flight, slot_now, &k) {
            return Ok(DecisionOutcome::Busy);
        }
        let found = self.matching_mut(&identity)?;
        // The hold itself is what carries the slot: `holds_capacity`
        // honours a decision over the reservation clock, so nothing here
        // rewrites `expires_at` or `updated_at`. A reservation that
        // lapses mid-decision keeps its slot until the decision ends,
        // and one that ends without a financial state is lapsed again —
        // its next submit re-acquires through this same check.
        found.deciding = true;
        let opened = Box::new(found.clone());
        // This IS an acquisition, whether the row was still live or had
        // lapsed and just took its slot back, so the service's floor
        // moves. Recorded here rather than by writing `updated_at`,
        // which belongs to retention: see `slot_floor`.
        self.note_acquisition(&admission.service_id, slot_now);
        Ok(DecisionOutcome::Open(opened))
    }

    fn close_decision(&mut self, admission: &AdmissionRecord) -> bool {
        let identity = admission.identity();
        match self
            .records
            .get_mut(&key(identity.owner, &identity.task_id))
        {
            Some(found) if identity.matches(found) && found.deciding => {
                found.deciding = false;
                true
            }
            // Nothing of this decision's to release: the write that ended
            // it already cleared the hold, or the row is no longer this
            // incarnation and the hold (if any) is not ours.
            _ => false,
        }
    }

    fn redeem(
        &mut self,
        admission: &AdmissionRecord,
        quote_id: String,
        payer: [u8; 32],
        now: u64,
    ) -> Result<(), A2aJournalError> {
        let identity = admission.identity();
        let k = key(identity.owner, &identity.task_id);
        // Same floor as the acquisition: a redemption that sampled its
        // clock before the gate call must not record itself against a
        // reservation whose slot somebody else now holds.
        let slot_now = self.slot_now(&admission.service_id, now);
        let current = self.records.get(&k);
        if !current.is_some_and(|found| identity.matches(found)) {
            // The admission this payment was redeemed for is gone or
            // replaced. Refuse — and retain the redemption where it
            // belongs: against the original admission, in the
            // unresolved-financial class.
            let detached = self
                .detached
                .entry((k, identity.generation))
                .or_insert_with(|| admission.clone());
            detached.deciding = false;
            detached.state = AdmissionState::Paid { quote_id, payer };
            detached.updated_at = now;
            return Err(identity.superseded());
        }
        let found = self.matching_mut(&identity)?;
        match &found.state {
            AdmissionState::Reserved { .. } if found.holds_capacity(slot_now) => {
                found.state = AdmissionState::Paid { quote_id, payer };
                found.updated_at = now;
                found.deciding = false;
                Ok(())
            }
            AdmissionState::Reserved { .. } => Err(conflict(format!(
                "task {:?} no longer holds its capacity slot; its redemption cannot be recorded \
                 against a reservation that lapsed with no decision open",
                identity.task_id
            ))),
            other => Err(conflict(format!(
                "task {:?} is {}, and only a reservation records a redemption",
                identity.task_id,
                other.tag().as_str()
            ))),
        }
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
        // This verb names a record by key and a tag, so it has no
        // identity to check — and once the key is known to have held a
        // different purchasable admission, a payment presented with no
        // identity cannot be attributed to an incarnation. Refuse rather
        // than guess which one it belongs to; `redeem` is the verb that
        // re-presents the admission and retains the evidence.
        if to_tag == StateTag::Paid {
            let admission_id = record.admission_id.clone();
            if let Some(other) = self
                .detached_of(owner, task_id)
                .find(|d| d.purchasable() && d.admission_id != admission_id)
            {
                return Err(conflict(format!(
                    "task {task_id:?} previously held admission {:?} (generation {}), which is \
                     retained and could still be paid; an unattributed financial write is \
                     refused — use `redeem`, which re-presents the admission it decided against",
                    other.admission_id, other.generation
                )));
            }
        }
        let record = self.record_mut(owner, task_id)?;
        record.state = to;
        record.updated_at = now;
        // The hold belongs to the decision, and the decision ended with
        // this state change.
        record.deciding = false;
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

    fn delete_reservation(&mut self, admission: &AdmissionRecord) -> Result<(), A2aJournalError> {
        let identity = admission.identity();
        let (owner, task_id) = (identity.owner, identity.task_id.as_str());
        let record = self.matching_mut(&identity)?;
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

    /// The launch claim bound to the incarnation whose decision reached
    /// it: the row under the key must still be exactly `admission`, or
    /// the claim is [`A2aJournalError::Superseded`] and nothing is
    /// written.
    ///
    /// This is the verb the serving path claims with. Being pinned
    /// against pruning is not the same as being unreplaceable: an
    /// operator resolving and forgetting a row mid-decision, and a
    /// re-prepare landing after it, leaves a *different* admission under
    /// the key — and a key-only claim would then launch one brief while
    /// writing another's identity into the never-pruned ledger.
    fn claim_launch_exact(
        &mut self,
        admission: &AdmissionRecord,
        now: u64,
    ) -> Result<LaunchLedgerEntry, A2aJournalError> {
        let identity = admission.identity();
        // Re-presented under the same lock as the write, exactly like
        // every other post-await write in this store.
        self.matching_mut(&identity)?;
        self.launch(identity.owner, &identity.task_id, now)
    }

    fn claim_launch(
        &mut self,
        owner: TaskOwner,
        task_id: &str,
        now: u64,
    ) -> Result<LaunchLedgerEntry, A2aJournalError> {
        // Identity-unbound, for an operator or a fixture recording a
        // launch by key — the same shape, and the same limit, as
        // `transition`: once the key is known to have held a different
        // purchasable admission, nothing here can attribute the launch to
        // an incarnation, so it is refused rather than guessed.
        if let Some(other) = self
            .detached_of(owner, task_id)
            .find(|d| d.purchasable())
            .map(|d| (d.admission_id.clone(), d.generation))
        {
            let live = self
                .records
                .get(&key(owner, task_id))
                .map(|r| r.admission_id.clone());
            if live.as_ref() != Some(&other.0) {
                return Err(conflict(format!(
                    "task {task_id:?} previously held admission {:?} (generation {}), which is \
                     retained and could still be paid; an unattributed launch is refused — use \
                     the identity-bound claim, which re-presents the admission its decision was \
                     opened on",
                    other.0, other.1
                )));
            }
        }
        self.launch(owner, task_id, now)
    }

    /// `Paid → Launched` (paid) or `Reserved → Launched` (free), plus the
    /// ledger entry, in one replace. The identity check is the caller's.
    fn launch(
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
        let slot_now = self
            .records
            .get(&k)
            .map(|r| self.slot_now(&r.service_id, now))
            .unwrap_or(now);
        let record = self.record_mut(owner, task_id)?;
        if !record.holds_capacity(slot_now) {
            // A claim whose slot lapsed with no decision holding it open
            // would be starting work against a ceiling somebody else may
            // already be inside. Decided against the store's own floor,
            // so a clock sampled before the awaited preflight cannot
            // claim a slot that has since been taken. Retryable: the
            // retry re-acquires or is told the service is busy.
            return Err(conflict(format!(
                "task {task_id:?} no longer holds its capacity slot and cannot claim a launch"
            )));
        }
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
        record.deciding = false;
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
        let live_is_unresolved = self
            .records
            .get(&key(owner, task_id))
            .is_some_and(|r| r.state.is_unresolved());
        // One key can carry more than one financial record: a redemption
        // retained against a superseded admission sits beside the live
        // replacement that took its place. Closing "the" record of such a
        // key means closing a charge nobody named — the operator meant
        // one incarnation and would silently terminalize the other. So
        // the key-only verb refuses, and names every candidate an
        // identity-bound `resolve_exact` can close.
        let ambiguous: Vec<(Option<String>, u64)> = self
            .detached_of(owner, task_id)
            .filter(|d| d.state.is_unresolved())
            .map(|d| (d.admission_id.clone(), d.generation))
            .collect();
        if ambiguous.len() + usize::from(live_is_unresolved) > 1 {
            let live = self
                .records
                .get(&key(owner, task_id))
                .map(|r| (r.admission_id.clone(), r.generation))
                .filter(|_| live_is_unresolved);
            let named: Vec<String> = live
                .into_iter()
                .map(|(id, generation)| format!("live {id:?} (generation {generation})"))
                .chain(
                    ambiguous
                        .iter()
                        .map(|(id, g)| format!("retained {id:?} (generation {g})")),
                )
                .collect();
            return Err(conflict(format!(
                "task {task_id:?} has {} unresolved financial records — {}; a resolution that \
                 names only the key would close one of them at random, so name the incarnation",
                named.len(),
                named.join(", ")
            )));
        }
        let record = if live_is_unresolved {
            self.record_mut(owner, task_id)?
        } else {
            // The live row is not an operator's to close (it may be a
            // later incarnation of somebody else's work); the one
            // retained detached admission is.
            match ambiguous.first().map(|(_, generation)| *generation) {
                Some(generation) => self
                    .detached
                    .get_mut(&(key(owner, task_id), generation))
                    .ok_or_else(|| conflict("detached admission vanished"))?,
                None => {
                    let found = self.record_mut(owner, task_id)?;
                    return Err(conflict(format!(
                        "task {task_id:?} is {}, which is not an unresolved financial record",
                        found.state.tag().as_str()
                    )));
                }
            }
        };
        Self::terminalize(record, state, now);
        Ok(())
    }

    /// Resolve the **exact** incarnation `identity` names — the live row
    /// under the key when it is still that incarnation, otherwise the
    /// retained detached record of that generation.
    ///
    /// What makes the unresolved queue usable: every row it reports
    /// carries its own [`AdmissionRecord::identity`], and handing that
    /// back closes that record and nothing else. A generation that is not
    /// there, or whose admission id or commitment does not match, is
    /// [`A2aJournalError::Superseded`] rather than a neighbouring charge.
    fn resolve_exact(
        &mut self,
        identity: &AdmissionIdentity,
        state: TaskState,
        now: u64,
    ) -> Result<(), A2aJournalError> {
        let k = key(identity.owner, &identity.task_id);
        let live_matches = self
            .records
            .get(&k)
            .is_some_and(|r| identity.matches(r) && r.state.is_unresolved());
        let record = if live_matches {
            self.record_mut(identity.owner, &identity.task_id)?
        } else {
            match self.detached.get_mut(&(k, identity.generation)) {
                Some(found) if identity.matches(found) && found.state.is_unresolved() => found,
                Some(found) if !identity.matches(found) => return Err(identity.superseded()),
                Some(found) => {
                    let tag = found.state.tag().as_str();
                    return Err(conflict(format!(
                        "the retained admission {:?} (generation {}) of task {:?} is {tag}, which \
                         is not an unresolved financial record",
                        identity.admission_id, identity.generation, identity.task_id
                    )));
                }
                None => return Err(identity.superseded()),
            }
        };
        Self::terminalize(record, state, now);
        Ok(())
    }

    /// Write an operator's disposition over an unresolved financial
    /// record, keeping the payment evidence it already carries.
    fn terminalize(record: &mut AdmissionRecord, state: TaskState, now: u64) {
        let (quote_id, payer) = record.state.evidence();
        record.state = AdmissionState::Terminal {
            quote_id,
            payer,
            state,
        };
        record.updated_at = now;
        record.deciding = false;
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
        let mut detach: Vec<AdmissionRecord> = Vec::new();
        self.records.retain(|_, r| match &r.state {
            // Resolved result.
            AdmissionState::Terminal { .. } => now < r.updated_at.saturating_add(r.retention_secs),
            // A decision is open against it: its slot and its evidence
            // both belong to that decision until it ends.
            AdmissionState::Reserved { .. } if r.deciding => true,
            // Unpaid reservation, attempt notes included. A purchasable
            // one is retained as a detached admission rather than
            // forgotten: a quote committed to its purchase hash can
            // still be presented, and a payment nobody can attribute is
            // worse than a row nobody reads.
            AdmissionState::Reserved { .. } => {
                let keep = now < r.updated_at.saturating_add(r.reservation_retention_secs);
                if !keep && r.purchasable() {
                    detach.push(r.clone());
                }
                keep
            }
            // Unresolved financial: never automatically.
            AdmissionState::Paid { .. }
            | AdmissionState::Launched { .. }
            | AdmissionState::Reconcile { .. } => true,
        });
        for mut record in detach {
            let k = (key(record.owner, &record.task_id), record.generation);
            // Detaching is itself a change to the row, and `updated_at`
            // is what its retention is measured from: the reservation
            // clock stays in `state`, so nothing is lost.
            record.updated_at = now;
            self.detached.entry(k).or_insert(record);
        }
        // A detached admission that was never paid for is remembered for
        // the offer's own result-retention window — long enough that a
        // quote issued against its purchase hash is attributable rather
        // than landing on a successor — and then dropped. One carrying a
        // redemption is unresolved financial evidence and is never
        // dropped automatically.
        self.detached.retain(|_, r| {
            r.state.is_unresolved() || now < r.updated_at.saturating_add(r.retention_secs)
        });
        (before - self.records.len()) as u64
    }

    /// Every detached admission of `(owner, task_id)`, oldest
    /// incarnation first.
    fn detached_of<'a>(
        &'a self,
        owner: TaskOwner,
        task_id: &str,
    ) -> impl Iterator<Item = &'a AdmissionRecord> + 'a {
        let k = key(owner, task_id);
        self.detached
            .iter()
            .filter(move |((dk, _), _)| *dk == k)
            .map(|(_, r)| r)
    }

    fn unresolved(&self) -> Vec<AdmissionRecord> {
        self.records
            .values()
            .chain(self.detached.values())
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
    /// Admissions retained after their row left its key. Flat, because
    /// the key and the incarnation are both already in the record.
    #[serde(default)]
    detached: Vec<AdmissionRecord>,
    /// The incarnation counter. Persisted so a restart cannot rewind it
    /// and hand a replacement row a number an old decision still names.
    #[serde(default)]
    next_generation: u64,
    /// Per-service capacity-acquisition floor. Persisted for the same
    /// reason as the counter: a restart must not rewind it and let a
    /// caller holding a pre-restart sample acquire a slot the store has
    /// already moved past. Absent in a file written before this field
    /// existed, which is safe — `slot_now` takes a max, so an empty map
    /// degrades to the previous behaviour rather than lowering a floor.
    #[serde(default)]
    slot_floor: BTreeMap<String, u64>,
}

impl JournalFile {
    fn into_state(self) -> StoreState {
        let records: BTreeMap<Key, AdmissionRecord> = self
            .records
            .into_iter()
            .map(|r| (key(r.owner, &r.task_id), r))
            .collect();
        let detached: BTreeMap<(Key, u64), AdmissionRecord> = self
            .detached
            .into_iter()
            .map(|r| ((key(r.owner, &r.task_id), r.generation), r))
            .collect();
        // Raised past everything on disk, not merely trusted: a file
        // written before the counter existed carries `0`, and a row
        // whose generation survived a counter that did not must never be
        // mintable again.
        let next_generation = records
            .values()
            .chain(detached.values())
            .map(|r| r.generation)
            .chain(std::iter::once(self.next_generation))
            .max()
            .unwrap_or_default();
        StoreState {
            records,
            ledger: self
                .ledger
                .into_iter()
                .map(|e| (key(e.owner, &e.task_id), e))
                .collect(),
            detached,
            next_generation,
            slot_floor: self.slot_floor,
        }
    }

    fn from_state(state: &StoreState) -> Self {
        Self {
            version: JOURNAL_VERSION,
            records: state.records.values().cloned().collect(),
            ledger: state.ledger.values().cloned().collect(),
            detached: state.detached.values().cloned().collect(),
            next_generation: state.next_generation,
            slot_floor: state.slot_floor.clone(),
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
    files: Arc<JournalFiles>,
    owner: Arc<JournalOwner>,
    /// Serializes this process's own writers before they contend for the
    /// `.lock` sidecar, so a load→mutate→replace is never interleaved
    /// in-process. `tokio`'s mutex because the acquisition awaits, and
    /// an `Arc` because the guard is **owned by the writing worker**:
    /// dropping the future that started a write must not release it
    /// while the write is still running.
    write_mu: Arc<tokio::sync::Mutex<()>>,
    recovered: Vec<RecoveredAdmission>,
}

/// The journal's file half: everything the writer needs, in an `Arc` it
/// can own outright.
///
/// The write runs on a blocking worker that is **never cancelled**
/// (`spawn_blocking` futures detach rather than abort), and that worker
/// holds the in-process guard, the `.lock` sidecar and the ownership
/// handle until the rename has landed. Aborting a store-operation future
/// therefore cannot release a guard while filesystem work is still in
/// flight — the defect a `tokio::fs` write path has, because its I/O
/// continues on the blocking pool after the awaiting future is dropped.
#[derive(Debug)]
struct JournalFiles {
    path: PathBuf,
    /// Distinguishes this journal's concurrent temp files from each
    /// other. The destination **filename** is what distinguishes them
    /// from a same-stem sibling's (`admissions.json` and
    /// `admissions.backup` used to collapse onto one temp name), and
    /// every temp is created exclusively, so a name is never shared.
    seq: std::sync::atomic::AtomicU64,
    /// Arms one durable write to fail without touching the file: the
    /// countdown decrements per write and fails the one that reaches
    /// zero, so a witness can name the write it means (the launch claim
    /// rather than whatever wrote first).
    #[cfg(feature = "testing")]
    fail_write: std::sync::atomic::AtomicU64,
    /// Arms exactly one durability barrier to fail *after* the rename
    /// landed — the ambiguous boundary.
    #[cfg(feature = "testing")]
    fail_next_barrier: std::sync::atomic::AtomicBool,
    /// How many atomic replaces this journal has published.
    #[cfg(feature = "testing")]
    writes: std::sync::atomic::AtomicU64,
    /// How many writers are inside the worker right now.
    #[cfg(feature = "testing")]
    in_worker: std::sync::atomic::AtomicU64,
    /// Parks the next worker just before it writes, until the witness
    /// releases it.
    #[cfg(feature = "testing")]
    hold: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
}

impl JournalFiles {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            seq: std::sync::atomic::AtomicU64::new(0),
            #[cfg(feature = "testing")]
            fail_write: std::sync::atomic::AtomicU64::new(0),
            #[cfg(feature = "testing")]
            fail_next_barrier: std::sync::atomic::AtomicBool::new(false),
            #[cfg(feature = "testing")]
            writes: std::sync::atomic::AtomicU64::new(0),
            #[cfg(feature = "testing")]
            in_worker: std::sync::atomic::AtomicU64::new(0),
            #[cfg(feature = "testing")]
            hold: Mutex::new(None),
        }
    }

    fn io_err(&self, e: impl std::fmt::Display) -> A2aJournalError {
        A2aJournalError::Io {
            path: self.path.display().to_string(),
            reason: e.to_string(),
        }
    }

    /// Read the journal. A missing file is an empty journal (first run);
    /// an unparseable one is [`A2aJournalError::Corrupt`]. No lock: the
    /// atomic rename is what prevents a torn read.
    fn load(&self) -> Result<StoreState, A2aJournalError> {
        load_at(&self.path)
    }

    /// Publish `state`: exclusive 0600 temp, `sync_all`, atomic rename,
    /// then the directory barrier.
    ///
    /// A failure up to and including the rename leaves the live file
    /// exactly as it was ([`Io`](A2aJournalError::Io)). A failure of the
    /// barrier **after** the rename is
    /// [`Ambiguous`](A2aJournalError::Ambiguous): the write is visible
    /// and only its survival of a power loss is unknown.
    fn publish(&self, state: &StoreState) -> Result<(), A2aJournalError> {
        // Only the fault seams and the write counter read it.
        #[cfg(feature = "testing")]
        use std::sync::atomic::Ordering::SeqCst;

        #[cfg(feature = "testing")]
        {
            let held = self.hold.lock().take();
            if let Some(rx) = held {
                // A blocking worker, so a blocking wait is the right
                // shape: this is the window a witness aborts the calling
                // future in.
                let _ = rx.recv();
            }
            let armed = self
                .fail_write
                .fetch_update(SeqCst, SeqCst, |n| (n > 0).then(|| n - 1))
                .unwrap_or(0);
            if armed == 1 {
                return Err(self.io_err("injected write failure (testing seam)"));
            }
        }

        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| self.io_err(e))?;
            }
        }

        let bytes = serde_json::to_vec_pretty(&JournalFile::from_state(state))
            .map_err(|e| self.io_err(format!("serialize admission journal: {e}")))?;

        let tmp = self.create_temp()?;
        let written = self.write_temp(&tmp.1, &bytes);
        drop(tmp.1);
        let path = &tmp.0;

        if let Err(e) = written {
            let _ = std::fs::remove_file(path);
            return Err(e);
        }
        if let Err(e) = std::fs::rename(path, &self.path) {
            // Never leave a temp sibling behind from a failed write.
            let _ = std::fs::remove_file(path);
            return Err(self.io_err(e));
        }
        #[cfg(feature = "testing")]
        self.writes.fetch_add(1, SeqCst);
        // Published. Everything from here can only be ambiguous.
        self.barrier()
    }

    /// An exclusively-created temp file beside the journal, named after
    /// the **full destination filename**.
    ///
    /// `with_extension` would replace the extension instead of extending
    /// the name, collapsing `admissions.json` and `admissions.backup`
    /// onto one `admissions.tmp.<pid>` — while their `.owner` and
    /// `.lock` sidecars keep the full filename and so never serialize
    /// the two journals against each other. `create_new` then makes
    /// "unique" a fact rather than a hope: a collision is an error to
    /// retry, never a silent truncation of somebody else's temp.
    fn create_temp(&self) -> Result<(PathBuf, std::fs::File), A2aJournalError> {
        use std::sync::atomic::Ordering::Relaxed;
        let pid = std::process::id();
        let mut last = None;
        for _ in 0..64 {
            let seq = self.seq.fetch_add(1, Relaxed);
            let candidate = sidecar(&self.path, &format!(".tmp.{pid}.{seq}"));
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create_new(true);
            // Owner-only (0600) from the start — the journal holds payer
            // keys and quote ids. The mode travels with the inode
            // through the rename. On Windows the per-user directory's
            // inherited ACLs scope access instead.
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                opts.mode(0o600);
            }
            match opts.open(&candidate) {
                Ok(file) => return Ok((candidate, file)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = Some(e),
                Err(e) => return Err(self.io_err(e)),
            }
        }
        Err(self.io_err(match last {
            Some(e) => format!("no unused temp name beside the journal: {e}"),
            None => "no unused temp name beside the journal".to_string(),
        }))
    }

    fn write_temp(&self, mut file: &std::fs::File, bytes: &[u8]) -> Result<(), A2aJournalError> {
        use std::io::Write as _;
        file.write_all(bytes).map_err(|e| self.io_err(e))?;
        file.flush().map_err(|e| self.io_err(e))?;
        // Durable before it becomes the live file, so a crash right
        // after the rename can never surface a truncated journal.
        file.sync_all().map_err(|e| self.io_err(e))
    }

    /// Make the **rename** durable, not merely the bytes it published.
    ///
    /// `sync_all` on the temp file commits its contents; the directory
    /// entry that makes it the journal is separate metadata, and without
    /// a barrier a power loss can recover the previous image — an old
    /// `Paid` row for work that has already started. On unix the barrier
    /// is an fsync of the parent directory, which is exactly what POSIX
    /// requires for rename durability.
    ///
    /// Windows exposes no portable parent-directory handle to flush;
    /// re-opening the published file for write and flushing it is the
    /// closest available barrier (NTFS commits the file's metadata with
    /// it), and the residual gap — a volume-level flush needs privileges
    /// this process does not have — is stated rather than papered over.
    fn barrier(&self) -> Result<(), A2aJournalError> {
        let ambiguous = |e: std::io::Error| A2aJournalError::Ambiguous {
            path: self.path.display().to_string(),
            reason: e.to_string(),
        };
        #[cfg(feature = "testing")]
        if self
            .fail_next_barrier
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(A2aJournalError::Ambiguous {
                path: self.path.display().to_string(),
                reason: "injected durability-barrier failure (testing seam)".to_string(),
            });
        }
        #[cfg(unix)]
        {
            let parent = match self.path.parent() {
                Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
                _ => PathBuf::from("."),
            };
            std::fs::File::open(&parent)
                .and_then(|dir| dir.sync_all())
                .map_err(ambiguous)
        }
        #[cfg(not(unix))]
        {
            std::fs::OpenOptions::new()
                .write(true)
                .open(&self.path)
                .and_then(|f| f.sync_all())
                .map_err(ambiguous)
        }
    }
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

        let files = Arc::new(JournalFiles::new(path));
        let write_mu = Arc::new(tokio::sync::Mutex::new(()));
        let mut state = {
            let files = Arc::clone(&files);
            blocking(move || files.load()).await?
        };
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

        // A decision hold belongs to the owner that took it, and this
        // handle is a successor: whatever was deciding when the previous
        // owner stopped is not deciding now, and leaving the hold would
        // consume a capacity slot no work will ever use. Cleared
        // durably, because every mutation reloads from the file.
        //
        // Nothing else about the inherited state is rewritten — the
        // recovery table on `AdmissionState::status` is what reads it.
        let stale: Vec<Key> = state
            .records
            .iter()
            .filter(|(_, r)| r.deciding)
            .map(|(k, _)| k.clone())
            .collect();
        if !stale.is_empty() {
            for k in &stale {
                if let Some(r) = state.records.get_mut(k) {
                    r.deciding = false;
                }
            }
            // Recovery publishes a WHOLE snapshot, which makes an
            // abandoned one the most destructive write in this file: it
            // would land on top of everything a successor has durably
            // added since, and read back as if that work had never
            // happened. So it runs under exactly the guards an ordinary
            // mutation does — the serialization mutex, the `.lock`
            // sidecar, and **the ownership handle** — all owned by the
            // blocking worker and released only when its I/O has
            // actually completed.
            //
            // Dropping this future (an aborted `open`, a `select!` that
            // lost) therefore cannot release the `.owner` lock while the
            // snapshot is still in flight: a successor's `open` is
            // refused with `OwnedElsewhere` until the writer is done,
            // which is the only answer that keeps "liveness is the lock"
            // true. A unique temp name does not help here — the
            // destination is the journal itself.
            let serialized = Arc::clone(&write_mu).lock_owned().await;
            let lock = WriteLock::acquire(&files.path).await?;
            let files = Arc::clone(&files);
            let owner = Arc::clone(&owner);
            blocking(move || {
                let _serialized = serialized;
                let _lock = lock;
                let _owner = owner;
                files.publish(&state)
            })
            .await?;
        }

        Ok(Self {
            files,
            owner,
            write_mu,
            recovered,
        })
    }

    /// The journal file's path.
    pub fn path(&self) -> &Path {
        &self.files.path
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
        self.fail_nth_write(1);
    }

    /// **Testing seam**: arm the `n`-th durable write from now to fail,
    /// leaving the ones before it alone.
    ///
    /// A serving path makes several writes per submission, so "the next
    /// write" cannot name the launch claim. A witness about the claim
    /// has to be able to say *which* write it means, or it is a witness
    /// about whatever wrote first.
    #[cfg(feature = "testing")]
    pub fn fail_nth_write(&self, n: u64) {
        self.files
            .fail_write
            .store(n, std::sync::atomic::Ordering::SeqCst);
    }

    /// **Testing seam**: arm the next durability barrier to fail *after*
    /// the rename has landed — the ambiguous boundary, which a witness
    /// must be able to reach without a power cut.
    #[cfg(feature = "testing")]
    pub fn fail_next_barrier(&self) {
        self.files
            .fail_next_barrier
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// **Testing seam**: park the next write inside its worker until the
    /// returned sender is used (or dropped). The window in which a
    /// witness can abort the future that started the write and observe
    /// that the write still completes under its own guards.
    #[cfg(feature = "testing")]
    pub fn hold_next_write(&self) -> std::sync::mpsc::Sender<()> {
        let (tx, rx) = std::sync::mpsc::channel();
        *self.files.hold.lock() = Some(rx);
        tx
    }

    /// **Testing seam**: how many writers are inside the I/O worker
    /// right now — the precondition a witness waits on instead of
    /// sleeping.
    #[cfg(feature = "testing")]
    pub fn writers_in_flight(&self) -> u64 {
        self.files
            .in_worker
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// **Testing seam**: how many atomic replaces this journal has
    /// published. What makes "the launched state and the ledger entry
    /// land in ONE replace" an observation rather than a claim.
    #[cfg(feature = "testing")]
    pub fn durable_writes(&self) -> u64 {
        self.files.writes.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// in-process mutex → `.lock` → load → check → temp + fsync +
    /// rename + barrier. `f` decides; a refusal from it publishes
    /// nothing.
    async fn mutate<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut StoreState) -> Result<R, A2aJournalError> + Send + 'static,
    ) -> Result<R, A2aJournalError> {
        self.mutate_if(|state| Ok((f(state)?, true))).await
    }

    /// [`mutate`](Self::mutate) for a decision that may conclude in
    /// "nothing changed": `f` returns its result plus whether the state
    /// is dirty, and a clean outcome publishes no write at all.
    ///
    /// The whole transaction — the `.lock` sidecar, the load, `f`, and
    /// the publish — runs on **one blocking worker that owns every
    /// guard**, including the in-process mutex and the ownership handle.
    /// `spawn_blocking` work is never cancelled, so dropping this future
    /// (an aborted runtime task, a `select!` that lost) cannot release a
    /// guard while the write is still running, and the next writer
    /// cannot start — or reuse a temp path — until this one has
    /// finished. That is the difference between an atomic replace and a
    /// half-published one.
    async fn mutate_if<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut StoreState) -> Result<(R, bool), A2aJournalError> + Send + 'static,
    ) -> Result<R, A2aJournalError> {
        let serialized = Arc::clone(&self.write_mu).lock_owned().await;
        let lock = WriteLock::acquire(&self.files.path).await?;
        let files = Arc::clone(&self.files);
        let owner = Arc::clone(&self.owner);
        blocking(move || {
            // Every guard is owned here, and released only when this
            // closure returns: the serialization guard, the cross-process
            // transaction lock, and the journal's lifetime owner.
            let _serialized = serialized;
            let _lock = lock;
            let _owner = owner;
            #[cfg(feature = "testing")]
            files
                .in_worker
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let out = (|| {
                let mut state = files.load()?;
                let (out, dirty) = f(&mut state)?;
                if dirty {
                    files.publish(&state)?;
                }
                Ok(out)
            })();
            #[cfg(feature = "testing")]
            files
                .in_worker
                .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            out
        })
        .await
    }
}

/// Run `f` on the blocking pool, mapping a worker panic to an I/O
/// failure rather than swallowing it.
async fn blocking<R: Send + 'static>(
    f: impl FnOnce() -> Result<R, A2aJournalError> + Send + 'static,
) -> Result<R, A2aJournalError> {
    match tokio::task::spawn_blocking(f).await {
        Ok(out) => out,
        Err(e) => Err(A2aJournalError::Io {
            path: String::new(),
            reason: format!("admission-journal I/O worker panicked: {e}"),
        }),
    }
}

/// Read the journal at `path`. A missing file is an empty journal (first
/// run); an unparseable one is [`A2aJournalError::Corrupt`]. No lock:
/// the atomic rename is what prevents a torn read.
fn load_at(path: &Path) -> Result<StoreState, A2aJournalError> {
    let bytes = match std::fs::read(path) {
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

/// Read the journal at `path` off the runtime thread.
async fn load(path: &Path) -> Result<StoreState, A2aJournalError> {
    let path = path.to_path_buf();
    blocking(move || load_at(&path)).await
}

#[async_trait::async_trait]
impl AdmissionStore for A2aAdmissionJournal {
    async fn lookup(
        &self,
        owner: TaskOwner,
        task_id: &str,
    ) -> Result<Option<AdmissionRecord>, A2aJournalError> {
        Ok(load(&self.files.path)
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

    async fn admit_reserved(
        &self,
        record: AdmissionRecord,
        max_in_flight: u64,
        now: u64,
    ) -> Result<AdmitOutcome, A2aJournalError> {
        // The count and the write are the same transaction, so two
        // prepares of distinct tasks cannot both read a count taken
        // before either wrote.
        self.mutate_if(move |state| {
            let out = state.admit_reserved(record, max_in_flight, now)?;
            let dirty = matches!(out, AdmitOutcome::Admitted(_));
            Ok((out, dirty))
        })
        .await
    }

    async fn open_decision(
        &self,
        admission: &AdmissionRecord,
        max_in_flight: u64,
        now: u64,
    ) -> Result<DecisionOutcome, A2aJournalError> {
        let admission = admission.clone();
        self.mutate_if(move |state| {
            let out = state.open_decision(&admission, max_in_flight, now)?;
            let dirty = matches!(out, DecisionOutcome::Open(_));
            Ok((out, dirty))
        })
        .await
    }

    async fn close_decision(
        &self,
        admission: &AdmissionRecord,
        _now: u64,
    ) -> Result<(), A2aJournalError> {
        let admission = admission.clone();
        // Nothing to clear publishes nothing: a refusal path must not
        // turn a clean no-op into an I/O failure of its own.
        self.mutate_if(move |state| Ok(((), state.close_decision(&admission))))
            .await
    }

    async fn redeem(
        &self,
        admission: &AdmissionRecord,
        quote_id: String,
        payer: [u8; 32],
        now: u64,
    ) -> Result<(), A2aJournalError> {
        let admission = admission.clone();
        // A superseded redemption is retained, so its refusal is a
        // *dirty* outcome: the detached record must be published before
        // the error reaches the caller, or the payment would be a fact
        // with no durable trace.
        let out = self
            .mutate_if(
                move |state| match state.redeem(&admission, quote_id, payer, now) {
                    Ok(()) => Ok((Ok(()), true)),
                    Err(e @ A2aJournalError::Superseded { .. }) => Ok((Err(e), true)),
                    Err(e) => Err(e),
                },
            )
            .await?;
        out
    }

    async fn in_flight(&self, service_id: &str, now: u64) -> Result<u64, A2aJournalError> {
        Ok(load(&self.files.path).await?.in_flight(service_id, now))
    }

    async fn transition(
        &self,
        owner: TaskOwner,
        task_id: &str,
        from: &[StateTag],
        to: AdmissionState,
        now: u64,
    ) -> Result<(), A2aJournalError> {
        let task_id = task_id.to_string();
        let from = from.to_vec();
        self.mutate(move |state| state.transition(owner, &task_id, &from, to, now))
            .await
    }

    async fn note(
        &self,
        owner: TaskOwner,
        task_id: &str,
        note: AttemptNote,
    ) -> Result<(), A2aJournalError> {
        let task_id = task_id.to_string();
        self.mutate(move |state| state.note(owner, &task_id, note))
            .await
    }

    async fn delete_reservation(&self, admission: &AdmissionRecord) -> Result<(), A2aJournalError> {
        let admission = admission.clone();
        self.mutate(move |state| state.delete_reservation(&admission))
            .await
    }

    async fn claim_launch_exact(
        &self,
        admission: &AdmissionRecord,
        now: u64,
    ) -> Result<LaunchLedgerEntry, A2aJournalError> {
        // ONE `mutate`, therefore one atomic replace: the `Launched`
        // state and the ledger entry become visible together or not at
        // all.
        let admission = admission.clone();
        self.mutate(move |state| state.claim_launch_exact(&admission, now))
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
        let task_id = task_id.to_string();
        self.mutate(move |state| state.claim_launch(owner, &task_id, now))
            .await
    }

    async fn record_terminal(
        &self,
        owner: TaskOwner,
        task_id: &str,
        state: TaskState,
        now: u64,
    ) -> Result<(), A2aJournalError> {
        let task_id = task_id.to_string();
        self.mutate(move |s| s.record_terminal(owner, &task_id, state, now))
            .await
    }

    async fn resolve(
        &self,
        owner: TaskOwner,
        task_id: &str,
        state: TaskState,
        now: u64,
    ) -> Result<(), A2aJournalError> {
        let task_id = task_id.to_string();
        self.mutate(move |s| s.resolve(owner, &task_id, state, now))
            .await
    }

    async fn resolve_exact(
        &self,
        identity: &AdmissionIdentity,
        state: TaskState,
        now: u64,
    ) -> Result<(), A2aJournalError> {
        let identity = identity.clone();
        self.mutate(move |s| s.resolve_exact(&identity, state, now))
            .await
    }

    async fn ledger_has(&self, owner: TaskOwner, task_id: &str) -> Result<bool, A2aJournalError> {
        Ok(load(&self.files.path)
            .await?
            .ledger
            .contains_key(&key(owner, task_id)))
    }

    async fn unresolved(&self) -> Result<Vec<AdmissionRecord>, A2aJournalError> {
        Ok(load(&self.files.path).await?.unresolved())
    }

    async fn forget(&self, owner: TaskOwner, task_id: &str) -> Result<bool, A2aJournalError> {
        let task_id = task_id.to_string();
        self.mutate(move |s| s.forget(owner, &task_id)).await
    }

    async fn prune(&self, now: u64) -> Result<u64, A2aJournalError> {
        self.mutate(move |s| Ok(s.prune(now))).await
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

    async fn admit_reserved(
        &self,
        record: AdmissionRecord,
        max_in_flight: u64,
        now: u64,
    ) -> Result<AdmitOutcome, A2aJournalError> {
        // One critical section under the mutex: the same atomicity the
        // journal gets from one transaction.
        self.state.lock().admit_reserved(record, max_in_flight, now)
    }

    async fn open_decision(
        &self,
        admission: &AdmissionRecord,
        max_in_flight: u64,
        now: u64,
    ) -> Result<DecisionOutcome, A2aJournalError> {
        self.state
            .lock()
            .open_decision(admission, max_in_flight, now)
    }

    async fn close_decision(
        &self,
        admission: &AdmissionRecord,
        _now: u64,
    ) -> Result<(), A2aJournalError> {
        self.state.lock().close_decision(admission);
        Ok(())
    }

    async fn redeem(
        &self,
        admission: &AdmissionRecord,
        quote_id: String,
        payer: [u8; 32],
        now: u64,
    ) -> Result<(), A2aJournalError> {
        self.state.lock().redeem(admission, quote_id, payer, now)
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

    async fn delete_reservation(&self, admission: &AdmissionRecord) -> Result<(), A2aJournalError> {
        self.state.lock().delete_reservation(admission)
    }

    async fn claim_launch_exact(
        &self,
        admission: &AdmissionRecord,
        now: u64,
    ) -> Result<LaunchLedgerEntry, A2aJournalError> {
        // One critical section under the mutex: the same atomicity the
        // journal gets from one atomic replace.
        self.state.lock().claim_launch_exact(admission, now)
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

    async fn resolve_exact(
        &self,
        identity: &AdmissionIdentity,
        state: TaskState,
        now: u64,
    ) -> Result<(), A2aJournalError> {
        self.state.lock().resolve_exact(identity, state, now)
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
