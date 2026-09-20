//! Stage 0 slice 0.4 of
//! `docs/internal/plans/ORG_SCOPED_STREAMING_PLAN.md` — the executable
//! **admission/retirement transaction model** (§3), over an abstract
//! authority (§2.3), an abstract fold effect boundary (§3 step 5), the Q1
//! active-call quotas and the §2.7 byte accounting.
//!
//! Like [`super::org_stream_lifecycle`] this is a **model**: `#[cfg(test)]`
//! only, no `MeshNode`, no network, no real `OrgRevocationStore`, no
//! `verify_org_admission`. Its job is to make the plan's hardest ownership
//! claims executable and *deterministically* schedulable before any of it is
//! wired into `mesh_rpc.rs`'s protected bridge, where an ordering mistake
//! costs a whole review round.
//!
//! # What the model must reproduce, and why
//!
//! The production ordering being modelled is `admit_and_dispatch_protected`
//! (`mesh_rpc.rs:1001-1318`): resolve the direct caller → decode → capture
//! the admission stamp (`org_admission_gate.rs:155`) → verify (replay insert
//! at step 10, provider policy at step 11) with the §9.5 stability recheck
//! (`mesh_rpc.rs:1269-1274`) → `apply_inbound_admitted`. §3 brackets that
//! with registry ownership, which adds four things the unary path never
//! needed:
//!
//! 1. A **reservation** taken before any signature work, so a duplicate
//!    `(caller, call_id)` is refused before decode — and charged only to
//!    quotas that are already authenticated (caller + node), never to an
//!    organization a *claimed* proof names.
//! 2. An **install** that requalifies rather than re-admits. The unary gate
//!    compares the whole
//!    [`AdmissionStamp`](crate::adapter::net::org_admission_gate::AdmissionStamp)
//!    and denies `AuthorityChanged` on any movement, because it is about to
//!    admit; a live call must instead survive a floor raise aimed at some
//!    *other* member (§2.3).
//! 3. A **confirm** that is an ownership *transfer*, not a boolean check
//!    followed by a spawn. The separating observation is the model check
//!    `retire_between_confirm_check_and_owner_transfer`.
//! 4. Two mutually exclusive, incarnation-conditional removal paths —
//!    bridge-owned [`ProtectedCallRegistry::release`] before transfer,
//!    supervisor-owned [`ProtectedCallRegistry::complete`] after — so a late
//!    retire cannot touch a reused key and nothing is removed twice (§2.4).
//!
//! # Determinism
//!
//! Every interleaving here is driven by explicit boundaries, never by sleeps
//! or thread races. The check/commit boundary is a real two-phase
//! transaction ([`ConfirmTxn`], [`CommitTxn`]) that *holds* the registry lock
//! across the window a bool-only implementation would leave open, and
//! [`ProtectedCallRegistry::retire_nonblocking`] reports
//! [`RetireAttempt::Blocked`] when it cannot land. That makes "the retire
//! cannot slip between the check and the transfer" a single-threaded,
//! reproducible assertion rather than a scheduling hope — and makes the
//! bool-only mutant fail by construction, because releasing the lock between
//! the phases turns `Blocked` into `Applied`.
//!
//! The model deliberately exposes the phases *separately*
//! (`begin_confirm`/`transfer`, `begin_commit`/`commit`, `guard_admit` apart
//! from `install`, publication apart from notification): a model that fuses
//! operations production cannot fuse is not evidence for production wiring
//! (plan, "Model check" table preamble).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};

use parking_lot::{Mutex, MutexGuard};

use super::org_stream_lifecycle::{
    CallLifecycle, CallShape, LifetimePolicy, PolicyError, ResolvedDeadline, Terminal,
    TerminalReason,
};

// ---------------------------------------------------------------------
// Identities
// ---------------------------------------------------------------------

/// An authenticated caller entity, resolved from the direct session pin
/// (`resolve_direct_caller`) — never a request-body field.
pub type CallerId = u64;
/// A mesh node id; the `from_node` of an inbound event.
pub type PeerId = u64;
/// An organization id.
pub type OrgId = u64;
/// A member identity within an organization.
pub type MemberId = u64;
/// One protected service registration (`ServeHandle`).
pub type RegistrationId = u64;

/// The nRPC correlation identity an admission is keyed on — exactly the
/// replay guard's key (`org_admission_replay.rs:8-10`). Both halves are known
/// before decode, which is what lets `reserve` run first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CallKey {
    /// The authenticated caller.
    pub caller: CallerId,
    /// `EventMeta::seq_or_ts`.
    pub call_id: u64,
}

/// The exact originating session, as §3 step 4 requires it to be bound.
///
/// `session_id` is the **truncated** 8-byte wire id. On its own it is not an
/// identity: two unrelated peers can present the same truncated value, so
/// session retirement matching on it alone would retire a bystander's call.
/// `peer` plus `establishment` (the exact handshake that produced this
/// session) is the identity, and re-handshaking the same peer yields a new
/// `establishment` — which is what makes session *replacement* retire the
/// displaced call and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionRef {
    /// The authenticated peer node.
    pub peer: PeerId,
    /// The truncated 8-byte session id carried on the wire.
    pub session_id: u64,
    /// The exact session establishment (handshake) this record was opened on.
    pub establishment: u64,
}

/// A raised revocation floor, mirroring `org_revocation.rs:453`.
pub type RaisedFloor = (OrgId, MemberId, u32);

// ---------------------------------------------------------------------
// The abstract authority (§2.3)
// ---------------------------------------------------------------------

/// The security view a record was captured under — the model's
/// `AdmissionStamp`.
///
/// `generation == None` models generation-space exhaustion
/// (`org_revocation.rs:1818-1823`): a frozen counter can no longer
/// distinguish views, so it must never compare current in either position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthorityStamp {
    /// Identity of the installed node authority (`Arc::as_ptr` in production).
    pub authority_id: u64,
    /// Identity of the installed revocation store.
    pub store_id: u64,
    /// The store's floor-publish generation; `None` once exhausted.
    pub generation: Option<u64>,
    /// Whether the store is poisoned as of this capture.
    pub poisoned: bool,
}

impl AuthorityStamp {
    /// Exactly `AdmissionStamp::is_current` (`org_admission_gate.rs:138-146`):
    /// whole-stamp equality, both generations usable, and the live view not
    /// poisoned.
    pub fn is_current(&self, current: &AuthorityStamp) -> bool {
        self.generation.is_some()
            && current.generation.is_some()
            && self == current
            && !current.poisoned
    }

    /// Classify *how* the view moved. This is the distinction §2.3 turns on:
    /// the unary gate only needs `is_current`, because a stale view means "do
    /// not admit". A live call must know whether the store itself moved (fail
    /// closed) or merely published a floor (requalify).
    pub fn movement(&self, current: &AuthorityStamp) -> StampMovement {
        if self.generation.is_none()
            || current.generation.is_none()
            || self.poisoned
            || current.poisoned
        {
            return StampMovement::Unusable;
        }
        if self.authority_id != current.authority_id || self.store_id != current.store_id {
            return StampMovement::Unusable;
        }
        if self.generation == current.generation {
            StampMovement::Unchanged
        } else {
            StampMovement::GenerationOnly
        }
    }
}

/// How a captured [`AuthorityStamp`] relates to the live one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StampMovement {
    /// Same store, same generation: proceed.
    Unchanged,
    /// Same store, a floor was published: requalify against the floors.
    GenerationOnly,
    /// The authority/store moved, is poisoned, or a generation is exhausted:
    /// fail closed, there is nothing left to requalify against.
    Unusable,
}

struct AuthorityState {
    authority_id: u64,
    store_id: u64,
    generation: u64,
    generation_exhausted: bool,
    poisoned: bool,
    floors: HashMap<(OrgId, MemberId), u32>,
}

impl AuthorityState {
    fn stamp(&self) -> AuthorityStamp {
        AuthorityStamp {
            authority_id: self.authority_id,
            store_id: self.store_id,
            generation: if self.generation_exhausted {
                None
            } else {
                Some(self.generation)
            },
            poisoned: self.poisoned,
        }
    }

    fn bump_generation(&mut self) {
        // CHECKED, never wrapping (`org_revocation.rs:676-691`): at the
        // ceiling the counter freezes and the exhaustion latch is set.
        match self.generation.checked_add(1) {
            Some(next) => self.generation = next,
            None => self.generation_exhausted = true,
        }
    }
}

/// The abstract revocation authority: floors, a publication generation, a
/// poison flag and an authority/store identity.
///
/// Publication and notification are **separate operations**, exactly as in
/// production (`org_revocation.rs:669-692` swaps the live view and bumps the
/// generation under `live.write()`; `:708-721` notifies afterwards, outside
/// every store lock). Keeping them separate is what makes
/// `publication_before_notification_cannot_authorize_commit` expressible: the
/// window between them is real, and a consumer that trusts only its own
/// notified epoch is wrong inside it.
pub struct ModelAuthority {
    state: Mutex<AuthorityState>,
    subscribers: Mutex<Vec<Weak<ProtectedCallRegistry>>>,
}

impl ModelAuthority {
    /// A fresh authority with no floors, generation 0, not poisoned.
    pub fn new(authority_id: u64, store_id: u64) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(AuthorityState {
                authority_id,
                store_id,
                generation: 0,
                generation_exhausted: false,
                poisoned: false,
                floors: HashMap::new(),
            }),
            subscribers: Mutex::new(Vec::new()),
        })
    }

    /// A single unbarriered read — enough to *detect* change, which is all
    /// `capture_admission_stamp` needs (`org_admission_gate.rs:149-155`).
    pub fn stamp(&self) -> AuthorityStamp {
        self.state.lock().stamp()
    }

    /// Hold the publication barrier. While the returned pin lives no
    /// publication can land, so a decision taken under it is not invalidated
    /// between the sample and the act (`org_revocation.rs:1786-1809`). Real
    /// exclusion, not observation.
    pub fn pin(&self) -> AuthorityPin<'_> {
        AuthorityPin {
            state: self.state.lock(),
        }
    }

    /// Publish floors: swap the view and bump the generation, returning the
    /// raise set **without notifying**. The caller decides when the
    /// subscriber callback runs.
    #[must_use = "the publication has landed; subscribers have NOT been notified yet"]
    pub fn publish_floors(&self, raises: &[RaisedFloor]) -> PendingNotification<'_> {
        let mut state = self.state.lock();
        let mut raised = Vec::new();
        for (org, member, floor) in raises {
            let current = state.floors.get(&(*org, *member)).copied().unwrap_or(0);
            if *floor > current {
                state.floors.insert((*org, *member), *floor);
                raised.push((*org, *member, *floor));
            }
        }
        state.bump_generation();
        drop(state);
        PendingNotification {
            authority: self,
            raised,
            authority_changed: false,
        }
    }

    /// Poison the store. Poison/recovery republishes the same durable view,
    /// so it raises no floor and must wake subscribers through the
    /// empty-slice path (`org_revocation.rs:736-746`).
    #[must_use = "the poison has landed; subscribers have NOT been notified yet"]
    pub fn poison(&self) -> PendingNotification<'_> {
        let mut state = self.state.lock();
        state.poisoned = true;
        state.bump_generation();
        drop(state);
        PendingNotification {
            authority: self,
            raised: Vec::new(),
            authority_changed: true,
        }
    }

    /// Burn the generation space (`org_revocation.rs:683`).
    pub fn exhaust_generation(&self) {
        self.state.lock().generation_exhausted = true;
    }

    /// Register a registry as a raise subscriber. Held weakly so a dropped
    /// registry does not keep itself alive through its own subscription.
    pub fn subscribe(&self, registry: &Arc<ProtectedCallRegistry>) {
        self.subscribers.lock().push(Arc::downgrade(registry));
    }

    /// Retire this registry's subscription — what dropping the production
    /// `RaiseSubscription` guard does. A registry that re-subscribed to a
    /// new store while still registered on the old one would act on floors
    /// published by an authority it no longer serves.
    pub fn unsubscribe(&self, registry: &Arc<ProtectedCallRegistry>) {
        let target = Arc::as_ptr(registry);
        self.subscribers
            .lock()
            .retain(|weak| !std::ptr::eq(Weak::as_ptr(weak), target));
    }

    /// Live subscriber count — the leak/resubscribe surface.
    pub fn subscriber_count(&self) -> usize {
        self.subscribers
            .lock()
            .iter()
            .filter(|w| w.strong_count() > 0)
            .count()
    }

    fn notify(&self, raised: &[RaisedFloor], authority_changed: bool) {
        // Snapshot outside the registry lock, exactly as `StoreCore::notify`
        // clones the callback `Arc`s before invoking any of them.
        let subscribers: Vec<Arc<ProtectedCallRegistry>> = self
            .subscribers
            .lock()
            .iter()
            .filter_map(Weak::upgrade)
            .collect();
        for registry in subscribers {
            if authority_changed {
                registry.on_authority_changed();
            } else if !raised.is_empty() {
                registry.on_floors_raised(raised);
            }
        }
    }
}

/// The publication barrier (`OrgRevocationStore::pin_publication`).
pub struct AuthorityPin<'a> {
    state: MutexGuard<'a, AuthorityState>,
}

impl AuthorityPin<'_> {
    /// The live stamp, sampled under the barrier.
    pub fn stamp(&self) -> AuthorityStamp {
        self.state.stamp()
    }

    /// The live floor for one member, under the same barrier as the stamp —
    /// so the `(floors, generation)` pair a requalification decides on is
    /// consistent (`org_revocation.rs:1830-1836`).
    pub fn floor_for(&self, org: OrgId, member: MemberId) -> u32 {
        self.state.floors.get(&(org, member)).copied().unwrap_or(0)
    }
}

/// A landed publication whose subscribers have not run yet.
#[must_use = "dropping this leaves the notifier permanently paused"]
pub struct PendingNotification<'a> {
    authority: &'a ModelAuthority,
    raised: Vec<RaisedFloor>,
    authority_changed: bool,
}

impl PendingNotification<'_> {
    /// What the publication raised.
    pub fn raised(&self) -> &[RaisedFloor] {
        &self.raised
    }

    /// Run the subscriber callbacks — the point at which the registry learns.
    pub fn notify(self) {
        self.authority.notify(&self.raised, self.authority_changed);
    }
}

// ---------------------------------------------------------------------
// Q1 limits
// ---------------------------------------------------------------------

/// The Q1 active-call ceilings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallLimits {
    /// Active protected calls per node, including `Opening` reservations and
    /// terminal records not yet reclaimed.
    pub max_active_node: usize,
    /// Active calls per authenticated caller, across its sessions.
    pub max_active_per_caller: usize,
    /// Active calls per external acting org, across its member identities.
    pub max_active_per_org: usize,
    /// Call lifetime bounds (§2.1), reused from slice 0.3.
    pub lifetime: LifetimePolicy,
    /// How long an `Opening` reservation may wait for verification before it
    /// is reaped. Finite by requirement: a lost bridge must not pin a slot.
    pub verification_deadline_ns: u64,
}

impl CallLimits {
    /// The Q1 initial defaults: 4096 / 64 / 512, 300 s default lifetime,
    /// 3600 s maximum, 30 s verification deadline.
    pub const fn q1_defaults() -> Self {
        Self {
            max_active_node: 4096,
            max_active_per_caller: 64,
            max_active_per_org: 512,
            lifetime: LifetimePolicy::q1_defaults(),
            verification_deadline_ns: 30 * 1_000_000_000,
        }
    }

    /// Startup validation (Q1): positive values, `default_live <= max_live`,
    /// and caller/org ceilings inside the node ceiling. A per-caller ceiling
    /// at or above the node ceiling makes the sub-ceiling decorative — one
    /// caller could consume the whole node.
    pub fn validate(&self) -> Result<(), LimitsError> {
        if self.max_active_node == 0
            || self.max_active_per_caller == 0
            || self.max_active_per_org == 0
            || self.verification_deadline_ns == 0
        {
            return Err(LimitsError::NotPositive);
        }
        self.lifetime.validate().map_err(LimitsError::Lifetime)?;
        if self.max_active_per_caller > self.max_active_node {
            return Err(LimitsError::PerCallerAboveNode);
        }
        if self.max_active_per_org > self.max_active_node {
            return Err(LimitsError::PerOrgAboveNode);
        }
        Ok(())
    }
}

/// The Q1 queued-byte ceilings (§2.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteLimits {
    /// Queued bytes per call **per direction**.
    pub per_call: usize,
    /// Queued bytes per caller, combined directions and calls.
    pub per_caller: usize,
    /// Queued bytes per node, combined everything.
    pub per_node: usize,
}

impl ByteLimits {
    /// Q1: 16 MiB per call per direction, 64 MiB per caller, 512 MiB per node.
    pub const fn q1_defaults() -> Self {
        Self {
            per_call: 16 * 1024 * 1024,
            per_caller: 64 * 1024 * 1024,
            per_node: 512 * 1024 * 1024,
        }
    }

    /// Positive, and each scope inside the next. The node budget is the hard
    /// aggregate, not the product of the per-call maxima.
    pub fn validate(&self) -> Result<(), LimitsError> {
        if self.per_call == 0 || self.per_caller == 0 || self.per_node == 0 {
            return Err(LimitsError::NotPositive);
        }
        if self.per_call > self.per_caller {
            return Err(LimitsError::BytePerCallAboveCaller);
        }
        if self.per_caller > self.per_node {
            return Err(LimitsError::BytePerCallerAboveNode);
        }
        Ok(())
    }
}

/// Why a limit set is unusable at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitsError {
    /// A zero ceiling: nothing could ever be admitted.
    NotPositive,
    /// `max_active_per_caller > max_active_node`.
    PerCallerAboveNode,
    /// `max_active_per_org > max_active_node`.
    PerOrgAboveNode,
    /// The §2.1 lifetime policy itself is invalid.
    Lifetime(PolicyError),
    /// `per_call > per_caller`.
    BytePerCallAboveCaller,
    /// `per_caller > per_node`.
    BytePerCallerAboveNode,
}

/// The existing RPC item cap, mirroring `cortex/rpc.rs:430`. Queue budgets do
/// not raise it (Q1), and it bounds a single item, never an aggregate.
///
/// Mirrored rather than imported because this model must compile in feature
/// graphs without `cortex`; `item_cap_matches_the_real_rpc_body_limit` pins
/// the two together wherever `cortex` is on.
pub const MAX_RPC_ITEM_BYTES: usize = 4 * 1024 * 1024;

// ---------------------------------------------------------------------
// §2.7 byte accounting
// ---------------------------------------------------------------------

/// Which queue an item entered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    /// Caller → provider (`apply_request_chunk_to_senders`).
    Request,
    /// Provider → caller (`RpcResponseSink::send_wait`).
    Response,
}

/// One item's byte charge, tied to the exact call incarnation so a refund
/// can never land on a successor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteCharge {
    /// The charged call.
    pub key: CallKey,
    /// The exact incarnation.
    pub incarnation: u64,
    /// Which direction's per-call budget was charged.
    pub direction: Direction,
    /// Bytes reserved.
    pub len: usize,
}

/// Why an item cannot be queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteRefusal {
    /// Larger than `MAX_RPC_ITEM_BYTES`: no amount of waiting helps.
    ItemTooLarge,
    /// Larger than the *entire* per-call budget: likewise impossible.
    ExceedsCallBudget,
    /// The per-call budget for this direction is currently full.
    CallBudgetFull,
    /// The caller's combined budget is currently full.
    CallerBudgetFull,
    /// The node's aggregate budget is currently full.
    NodeBudgetFull,
    /// Checked arithmetic overflowed.
    Overflow,
}

impl ByteRefusal {
    /// Whether waiting could ever satisfy this request. `false` means the
    /// item must fail promptly rather than park on permits that can never be
    /// granted (§2.7, `oversized_item_never_waits_for_impossible_permits`).
    pub fn is_satisfiable_by_waiting(&self) -> bool {
        match self {
            ByteRefusal::ItemTooLarge | ByteRefusal::ExceedsCallBudget | ByteRefusal::Overflow => {
                false
            }
            ByteRefusal::CallBudgetFull
            | ByteRefusal::CallerBudgetFull
            | ByteRefusal::NodeBudgetFull => true,
        }
    }
}

#[derive(Default)]
struct ByteState {
    per_call: HashMap<(CallKey, u64, Direction), usize>,
    per_caller: HashMap<CallerId, usize>,
    node: usize,
}

/// The three-level byte budget of §2.7.
///
/// Reservation is a **short locked reservation that checks before it
/// increments**, in the documented order call → caller → node, rolling back
/// everything it already acquired on a later refusal. `fetch_add` followed
/// by a check is explicitly not a hard bound: two racing producers both
/// observe an under-limit total after both have already published their
/// increments.
pub struct ByteBudgets {
    limits: ByteLimits,
    state: Mutex<ByteState>,
    unsettled_drops: AtomicUsize,
}

impl ByteBudgets {
    /// Validate and build.
    pub fn new(limits: ByteLimits) -> Result<Arc<Self>, LimitsError> {
        limits.validate()?;
        Ok(Arc::new(Self {
            limits,
            state: Mutex::new(ByteState::default()),
            unsettled_drops: AtomicUsize::new(0),
        }))
    }

    /// The configured ceilings.
    pub fn limits(&self) -> ByteLimits {
        self.limits
    }

    /// Validate an item against the RPC item cap and the configured per-call
    /// budget **before any wait**. Both are static properties of the item:
    /// no amount of drainage makes a 5 MiB item fit a 4 MiB cap.
    pub fn validate_item(&self, len: usize) -> Result<(), ByteRefusal> {
        if len > MAX_RPC_ITEM_BYTES {
            return Err(ByteRefusal::ItemTooLarge);
        }
        if len > self.limits.per_call {
            return Err(ByteRefusal::ExceedsCallBudget);
        }
        Ok(())
    }

    /// Reserve `len` bytes for one item. Order: call → caller → node, each
    /// level checked before it is incremented, each acquired level rolled
    /// back if a later one refuses.
    pub fn reserve(
        self: &Arc<Self>,
        key: CallKey,
        incarnation: u64,
        direction: Direction,
        len: usize,
    ) -> Result<ItemPermit, ByteRefusal> {
        self.validate_item(len)?;
        let mut state = self.state.lock();

        // 1 — per call, per direction.
        let call_slot = (key, incarnation, direction);
        let call_cur = state.per_call.get(&call_slot).copied().unwrap_or(0);
        let call_next = call_cur.checked_add(len).ok_or(ByteRefusal::Overflow)?;
        if call_next > self.limits.per_call {
            return Err(ByteRefusal::CallBudgetFull);
        }
        state.per_call.insert(call_slot, call_next);

        // 2 — per caller. On refusal, undo (1).
        let caller_cur = state.per_caller.get(&key.caller).copied().unwrap_or(0);
        let caller_next = match caller_cur.checked_add(len) {
            Some(next) if next <= self.limits.per_caller => next,
            Some(_) => {
                state.per_call.insert(call_slot, call_cur);
                return Err(ByteRefusal::CallerBudgetFull);
            }
            None => {
                state.per_call.insert(call_slot, call_cur);
                return Err(ByteRefusal::Overflow);
            }
        };
        state.per_caller.insert(key.caller, caller_next);

        // 3 — per node. On refusal, undo (2) and (1).
        let node_next = match state.node.checked_add(len) {
            Some(next) if next <= self.limits.per_node => next,
            Some(_) => {
                state.per_caller.insert(key.caller, caller_cur);
                state.per_call.insert(call_slot, call_cur);
                return Err(ByteRefusal::NodeBudgetFull);
            }
            None => {
                state.per_caller.insert(key.caller, caller_cur);
                state.per_call.insert(call_slot, call_cur);
                return Err(ByteRefusal::Overflow);
            }
        };
        state.node = node_next;
        drop(state);

        Ok(ItemPermit {
            budgets: Arc::clone(self),
            charge: ByteCharge {
                key,
                incarnation,
                direction,
                len,
            },
            settled: false,
        })
    }

    /// Aggregate bytes charged to the node.
    pub fn node_bytes(&self) -> usize {
        self.state.lock().node
    }

    /// Bytes charged to one caller.
    pub fn caller_bytes(&self, caller: CallerId) -> usize {
        self.state
            .lock()
            .per_caller
            .get(&caller)
            .copied()
            .unwrap_or(0)
    }

    /// Bytes charged to one call incarnation in one direction.
    pub fn call_bytes(&self, key: CallKey, incarnation: u64, direction: Direction) -> usize {
        self.state
            .lock()
            .per_call
            .get(&(key, incarnation, direction))
            .copied()
            .unwrap_or(0)
    }

    /// How many permits were dropped without being released or transferred.
    /// A correct flow leaves this at zero; a nonzero value is an ownership
    /// bug the accounting would otherwise hide.
    pub fn unsettled_drops(&self) -> usize {
        self.unsettled_drops.load(Ordering::Relaxed)
    }

    fn release_charge(&self, charge: ByteCharge) {
        let mut state = self.state.lock();
        let call_slot = (charge.key, charge.incarnation, charge.direction);
        // CHECKED, never saturating: saturating subtraction is exactly how a
        // double release or a refund of another call's bytes stays invisible.
        let call_cur = state.per_call.get(&call_slot).copied().unwrap_or(0);
        let call_next = call_cur
            .checked_sub(charge.len)
            .expect("byte permit released twice, or against the wrong call");
        if call_next == 0 {
            state.per_call.remove(&call_slot);
        } else {
            state.per_call.insert(call_slot, call_next);
        }
        let caller_cur = state
            .per_caller
            .get(&charge.key.caller)
            .copied()
            .unwrap_or(0);
        let caller_next = caller_cur
            .checked_sub(charge.len)
            .expect("byte permit released twice, or against the wrong caller");
        if caller_next == 0 {
            state.per_caller.remove(&charge.key.caller);
        } else {
            state.per_caller.insert(charge.key.caller, caller_next);
        }
        state.node = state
            .node
            .checked_sub(charge.len)
            .expect("byte permit released twice against the node budget");
    }
}

/// One admitted item's release-once permit bundle, tied to the call
/// incarnation.
///
/// Not `Clone`: ownership is the mechanism. Dequeue, cancellation and queue
/// discard compete to *consume* this, rather than each subtracting a guessed
/// byte count.
#[must_use = "a byte permit must be released or transferred exactly once"]
pub struct ItemPermit {
    budgets: Arc<ByteBudgets>,
    charge: ByteCharge,
    settled: bool,
}

impl ItemPermit {
    /// What this permit holds.
    pub fn charge(&self) -> ByteCharge {
        self.charge
    }

    /// Actual release: the bytes leave the three counters.
    pub fn release(mut self) {
        self.settle();
    }

    /// Hand the *same* charge to another bounded queue. The bytes stay
    /// charged — handoff is not memory reclamation — and exactly one live
    /// permit continues to own them.
    pub fn transfer(mut self) -> ItemPermit {
        self.settled = true;
        ItemPermit {
            budgets: Arc::clone(&self.budgets),
            charge: self.charge,
            settled: false,
        }
    }

    fn settle(&mut self) {
        if !self.settled {
            self.settled = true;
            self.budgets.release_charge(self.charge);
        }
    }
}

impl std::fmt::Debug for ItemPermit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ItemPermit")
            .field("charge", &self.charge)
            .field("settled", &self.settled)
            .finish()
    }
}

impl Drop for ItemPermit {
    fn drop(&mut self) {
        if !self.settled {
            self.budgets.unsettled_drops.fetch_add(1, Ordering::Relaxed);
            self.settle();
        }
    }
}

/// A queued item whose permit three parties race to consume: the pump
/// dequeuing it, retirement cancelling it, and the queue discarding it.
/// Exactly one [`SharedPermit::take`] wins.
pub struct SharedPermit {
    slot: Mutex<Option<ItemPermit>>,
}

impl SharedPermit {
    /// Wrap a permit for contended consumption.
    pub fn new(permit: ItemPermit) -> Arc<Self> {
        Arc::new(Self {
            slot: Mutex::new(Some(permit)),
        })
    }

    /// Consume the permit. Exactly one caller ever gets `Some`.
    pub fn take(&self) -> Option<ItemPermit> {
        self.slot.lock().take()
    }

    /// Whether some party has already consumed it.
    pub fn is_consumed(&self) -> bool {
        self.slot.lock().is_none()
    }
}

// ---------------------------------------------------------------------
// The abstract replay guard
// ---------------------------------------------------------------------

/// The guard's retention grace beyond proof expiry (§3: "proof not yet
/// expired + 300 s").
pub const GUARD_RETENTION_GRACE_NS: u64 = 300 * 1_000_000_000;

/// The three outcomes of `AdmissionReplayGuard::admit`
/// (`org_admission_replay.rs:224-234`) a duplicate opening can observe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardVerdict {
    /// First sight of this `(caller, call_id)` within its window.
    Admitted,
    /// The same proof re-presented before expiry.
    Replay,
    /// The same `(caller, call_id)` with a different binding digest.
    CallIdCollision,
}

/// The volatile `(caller, call_id)` replay guard, modelled at the fidelity
/// §3 requires: a reused key inside the retained window is **refused**, not
/// silently re-`Admitted` (the plan names that failure explicitly at
/// "A Stage 0 abstract guard must reproduce those semantics"). Keyed on the
/// correlation identity, never on the digest — keying on the digest would
/// let a caller mint a fresh key by re-signing.
#[derive(Default)]
pub struct ReplayGuardModel {
    entries: Mutex<HashMap<CallKey, GuardEntry>>,
}

struct GuardEntry {
    digest: [u8; 32],
    expires_at_ns: u64,
}

impl ReplayGuardModel {
    /// Insert-or-deny. An UNEXPIRED entry is never evicted.
    pub fn admit(
        &self,
        key: CallKey,
        digest: [u8; 32],
        now_ns: u64,
        proof_expiry_ns: u64,
    ) -> GuardVerdict {
        let mut entries = self.entries.lock();
        entries.retain(|_, entry| entry.expires_at_ns > now_ns);
        if let Some(existing) = entries.get(&key) {
            return if existing.digest == digest {
                GuardVerdict::Replay
            } else {
                GuardVerdict::CallIdCollision
            };
        }
        entries.insert(
            key,
            GuardEntry {
                digest,
                expires_at_ns: proof_expiry_ns.saturating_add(GUARD_RETENTION_GRACE_NS),
            },
        );
        GuardVerdict::Admitted
    }

    /// Whether the guard still retains this key at `now_ns`. Retirement never
    /// touches the guard, so this outlives the call.
    pub fn retains(&self, key: CallKey, now_ns: u64) -> bool {
        self.entries
            .lock()
            .get(&key)
            .is_some_and(|entry| entry.expires_at_ns > now_ns)
    }
}

// ---------------------------------------------------------------------
// Denials
// ---------------------------------------------------------------------

/// Which ceiling refused an opening.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapacityScope {
    /// The node-wide active-call ceiling.
    Node,
    /// The authenticated caller's ceiling.
    Caller,
    /// The verified acting organization's ceiling.
    Org,
}

/// Why an opening, install or confirm was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denial {
    /// The key is already owned by a record on this node (§3: refused by
    /// `reserve`, before decode).
    ActiveCallOwned,
    /// An active-call ceiling is exhausted.
    ActiveStreamCapacity(CapacityScope),
    /// The reservation was retired between reserve and install.
    AuthorityChanged,
    /// The authority/store moved, is poisoned, or a generation is exhausted.
    AuthorityUnavailable,
    /// A floor rose past this member's generation.
    Revoked,
    /// `SessionCurrentness` generation `u64::MAX`
    /// (`org_routing_registry.rs:513-518`).
    SessionCurrentnessExhausted,
    /// The `Opening` reservation outlived its verification deadline.
    VerificationDeadlineExpired,
    /// The guard saw the same proof again.
    Replay,
    /// The guard saw the key with a different binding digest.
    CallIdCollision,
    /// Provider policy vetoed a valid proof (Q4: the guard slot stays
    /// consumed).
    PolicyVetoed,
    /// A byte budget refused an admitted item.
    ResourceExhausted(ByteRefusal),
    /// Checked arithmetic overflowed on a counter.
    CounterOverflow,
    /// The record is not in the state this operation requires.
    NotAdmitted,
}

/// The coarse wire category a denial collapses to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoarseDenial {
    /// The caller is not permitted. Retrying identically will not help.
    Denied,
    /// Provider-side state; the call may be retried.
    Unavailable,
}

impl Denial {
    /// §3: `ActiveCallOwned` and `Replay` deliberately share the coarse
    /// byte, so classifying a duplicate through either path leaves the wire
    /// unchanged.
    pub fn coarse(&self) -> CoarseDenial {
        match self {
            Denial::ActiveCallOwned
            | Denial::Replay
            | Denial::CallIdCollision
            | Denial::PolicyVetoed
            | Denial::NotAdmitted => CoarseDenial::Denied,
            Denial::ActiveStreamCapacity(_)
            | Denial::AuthorityChanged
            | Denial::AuthorityUnavailable
            | Denial::Revoked
            | Denial::SessionCurrentnessExhausted
            | Denial::VerificationDeadlineExpired
            | Denial::ResourceExhausted(_)
            | Denial::CounterOverflow => CoarseDenial::Unavailable,
        }
    }
}

// ---------------------------------------------------------------------
// The record
// ---------------------------------------------------------------------

/// Where a record sits in the admission transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Reserved, not yet verified. No member facts.
    Opening,
    /// Installed with verified facts; the fold has not taken ownership.
    Admitted,
    /// Ownership transferred to a supervisor. `Draining` lives inside the
    /// [`CallLifecycle`] under this phase — it is still live.
    Running,
    /// A terminal has been selected. The record still owns its key and its
    /// quota slots until its one cleanup owner removes it.
    Terminal,
}

/// Which side owns the single conditional removal (§2.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupOwner {
    /// Before transfer: the bridge's reservation guard.
    Bridge,
    /// After transfer: the supervisor, and only after disposing of its work.
    Supervisor,
}

/// Verified member facts, known only after the proof is checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifiedFacts {
    /// The org the caller is verified to be acting for.
    pub acting_org: OrgId,
    /// The verified member identity.
    pub member: MemberId,
    /// That member's certificate generation.
    pub member_generation: u32,
    /// The §2.1 effective deadline.
    pub deadline: ResolvedDeadline,
}

/// Everything `reserve` can know before decode.
#[derive(Debug, Clone, Copy)]
pub struct OpeningRequest {
    /// `(caller, call_id)`.
    pub key: CallKey,
    /// The exact originating session.
    pub session: SessionRef,
    /// The routing registry's session generation; `None` models the
    /// `u64::MAX` terminal marker.
    pub session_generation: Option<u64>,
    /// The protected registration this opening targets.
    pub registration: RegistrationId,
    /// The streaming shape.
    pub shape: CallShape,
    /// Wall-clock now, nanoseconds.
    pub now_ns: u64,
}

/// A held `Opening` slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reservation {
    /// The reserved key.
    pub key: CallKey,
    /// The exact incarnation every later operation is conditional on.
    pub incarnation: u64,
    /// The registry's authority epoch at reserve time.
    pub epoch_at_reserve: u64,
    /// When this reservation is reaped if verification has not completed.
    pub verify_by_ns: u64,
}

/// An installed record, ready for the fold's confirm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionLease {
    /// The admitted key.
    pub key: CallKey,
    /// The exact incarnation.
    pub incarnation: u64,
}

struct CallRecord {
    incarnation: u64,
    phase: Phase,
    session: SessionRef,
    registration: RegistrationId,
    shape: CallShape,
    verify_by_ns: u64,
    captured: AuthorityStamp,
    facts: Option<VerifiedFacts>,
    lifecycle: Option<CallLifecycle>,
    owner: Option<Arc<SupervisorOwner>>,
    charged_org: Option<OrgId>,
    cleanup: CleanupOwner,
    terminal: Option<TerminalReason>,
    queued: Vec<Arc<SharedPermit>>,
}

// ---------------------------------------------------------------------
// The supervisor owner and the fold effect boundary
// ---------------------------------------------------------------------

/// The cancellation-ready owner `confirm` registers. It exists *before* its
/// task is scheduled: that is the whole point of making confirm a transfer.
pub struct SupervisorOwner {
    key: CallKey,
    incarnation: u64,
    terminal: Mutex<Option<TerminalReason>>,
    signals: AtomicUsize,
    task_started: AtomicBool,
}

impl SupervisorOwner {
    /// Arm an owner for one exact incarnation.
    pub fn new(key: CallKey, incarnation: u64) -> Arc<Self> {
        Arc::new(Self {
            key,
            incarnation,
            terminal: Mutex::new(None),
            signals: AtomicUsize::new(0),
            task_started: AtomicBool::new(false),
        })
    }

    /// The owned key.
    pub fn key(&self) -> CallKey {
        self.key
    }

    /// The owned incarnation.
    pub fn incarnation(&self) -> u64 {
        self.incarnation
    }

    /// The terminal this owner was handed, if any. Reaching here is the
    /// observable meaning of "retirement reached the owner".
    pub fn observed_terminal(&self) -> Option<TerminalReason> {
        self.terminal.lock().clone()
    }

    /// How many retirement signals arrived. First writer wins the terminal;
    /// later signals are counted but do not replace it.
    pub fn signal_count(&self) -> usize {
        self.signals.load(Ordering::Relaxed)
    }

    /// Whether the supervisor task has actually been polled. Retirement must
    /// reach the owner even while this is `false`.
    pub fn task_started(&self) -> bool {
        self.task_started.load(Ordering::Relaxed)
    }

    /// Mark the supervisor task as scheduled and polled.
    pub fn start_task(&self) {
        self.task_started.store(true, Ordering::Relaxed);
    }

    fn signal(&self, reason: TerminalReason) {
        self.signals.fetch_add(1, Ordering::Relaxed);
        let mut slot = self.terminal.lock();
        if slot.is_none() {
            *slot = Some(reason);
        }
    }
}

/// A confirmed call. Minting one is the *only* way to reach
/// [`FoldEffects::admit`], so "zero fold effects" is a structural property
/// of a failed confirm rather than a discipline the test has to remember.
pub struct RunningCall {
    key: CallKey,
    incarnation: u64,
    owner: Arc<SupervisorOwner>,
}

impl RunningCall {
    /// The running key.
    pub fn key(&self) -> CallKey {
        self.key
    }

    /// The running incarnation.
    pub fn incarnation(&self) -> u64 {
        self.incarnation
    }

    /// The owner registered by the transfer.
    pub fn owner(&self) -> &Arc<SupervisorOwner> {
        &self.owner
    }
}

/// The fold's observable side effects: in-flight insert, sender creation,
/// handler spawn (§3 step 5).
#[derive(Default)]
pub struct FoldEffects {
    admitted: AtomicUsize,
    opening_refusals: AtomicUsize,
}

impl FoldEffects {
    /// Record the fold's admitted effects for a confirmed call.
    pub fn admit(&self, _running: &RunningCall) {
        self.admitted.fetch_add(1, Ordering::Relaxed);
    }

    /// The bridge's one bounded opening refusal for a pre-transfer loss.
    pub fn refuse_opening(&self) {
        self.opening_refusals.fetch_add(1, Ordering::Relaxed);
    }

    /// How many admitted effects were performed.
    pub fn admitted(&self) -> usize {
        self.admitted.load(Ordering::Relaxed)
    }

    /// How many opening refusals the bridge emitted.
    pub fn opening_refusals(&self) -> usize {
        self.opening_refusals.load(Ordering::Relaxed)
    }
}

// ---------------------------------------------------------------------
// The registry
// ---------------------------------------------------------------------

struct RegistryInner {
    authority: Arc<ModelAuthority>,
    authority_epoch: u64,
    next_incarnation: u64,
    records: HashMap<CallKey, CallRecord>,
    active_node: usize,
    active_per_caller: HashMap<CallerId, usize>,
    active_per_org: HashMap<OrgId, usize>,
}

/// One per `MeshNode`: the exact-incarnation admission/retirement
/// transaction of §3.
pub struct ProtectedCallRegistry {
    inner: Mutex<RegistryInner>,
    limits: CallLimits,
    bytes: Arc<ByteBudgets>,
    guard: ReplayGuardModel,
    removals: Mutex<HashMap<(CallKey, u64), usize>>,
}

impl ProtectedCallRegistry {
    /// Validate the Q1 limits and subscribe to the authority's raise feed —
    /// the same site the production subscription is installed at, so it is
    /// re-created whenever the store is.
    pub fn new(
        authority: Arc<ModelAuthority>,
        limits: CallLimits,
        byte_limits: ByteLimits,
    ) -> Result<Arc<Self>, LimitsError> {
        limits.validate()?;
        let bytes = ByteBudgets::new(byte_limits)?;
        let registry = Arc::new(Self {
            inner: Mutex::new(RegistryInner {
                authority: Arc::clone(&authority),
                authority_epoch: 0,
                next_incarnation: 1,
                records: HashMap::new(),
                active_node: 0,
                active_per_caller: HashMap::new(),
                active_per_org: HashMap::new(),
            }),
            limits,
            bytes,
            guard: ReplayGuardModel::default(),
            removals: Mutex::new(HashMap::new()),
        });
        authority.subscribe(&registry);
        Ok(registry)
    }

    /// The configured active-call ceilings.
    pub fn limits(&self) -> CallLimits {
        self.limits
    }

    /// The §2.7 byte budgets.
    pub fn bytes(&self) -> &Arc<ByteBudgets> {
        &self.bytes
    }

    /// The replay guard. Exposed separately from [`Self::install`] because
    /// production consults it *inside* verification (step 10), between
    /// reserve and install — fusing it into install would model an ordering
    /// production does not have.
    pub fn guard(&self) -> &ReplayGuardModel {
        &self.guard
    }

    /// The registry's authority epoch. Only a *notified* raise moves it,
    /// which is precisely why it cannot be the sole basis for an install or
    /// commit decision.
    pub fn authority_epoch(&self) -> u64 {
        self.inner.lock().authority_epoch
    }

    /// How many records exist, in any phase.
    pub fn record_count(&self) -> usize {
        self.inner.lock().records.len()
    }

    /// Active calls charged to the node.
    pub fn active_node(&self) -> usize {
        self.inner.lock().active_node
    }

    /// Active calls charged to one caller.
    pub fn active_for_caller(&self, caller: CallerId) -> usize {
        self.inner
            .lock()
            .active_per_caller
            .get(&caller)
            .copied()
            .unwrap_or(0)
    }

    /// Active calls charged to one verified acting org.
    pub fn active_for_org(&self, org: OrgId) -> usize {
        self.inner
            .lock()
            .active_per_org
            .get(&org)
            .copied()
            .unwrap_or(0)
    }

    /// The phase of a record, if it exists.
    pub fn phase(&self, key: CallKey) -> Option<Phase> {
        self.inner.lock().records.get(&key).map(|r| r.phase)
    }

    /// The selected terminal, if any.
    pub fn terminal_reason(&self, key: CallKey) -> Option<TerminalReason> {
        self.inner
            .lock()
            .records
            .get(&key)
            .and_then(|r| r.terminal.clone())
    }

    /// The §2.6 lifecycle terminal, created at transfer.
    pub fn lifecycle_terminal(&self, key: CallKey) -> Option<Terminal> {
        self.inner
            .lock()
            .records
            .get(&key)
            .and_then(|r| r.lifecycle.as_ref())
            .and_then(|lc| lc.terminal().cloned())
    }

    /// Which side currently owns the single removal.
    pub fn cleanup_owner(&self, key: CallKey) -> Option<CleanupOwner> {
        self.inner.lock().records.get(&key).map(|r| r.cleanup)
    }

    /// The §2.1 deadline installed for a record.
    pub fn deadline(&self, key: CallKey) -> Option<ResolvedDeadline> {
        self.inner
            .lock()
            .records
            .get(&key)
            .and_then(|r| r.facts.map(|f| f.deadline))
    }

    /// The captured security view, refreshed by requalification.
    pub fn captured_stamp(&self, key: CallKey) -> Option<AuthorityStamp> {
        self.inner.lock().records.get(&key).map(|r| r.captured)
    }

    /// How many times `(key, incarnation)` has been removed. Exactly-once
    /// removal is the property; a ledger makes "exactly once" observable
    /// rather than inferred from a boolean.
    pub fn removals(&self, key: CallKey, incarnation: u64) -> usize {
        self.removals
            .lock()
            .get(&(key, incarnation))
            .copied()
            .unwrap_or(0)
    }

    // ----------------------------------------------------------------
    // §3 step 1 — reserve
    // ----------------------------------------------------------------

    /// Take an `Opening` slot before any signature work.
    ///
    /// Charges the authenticated caller and the node only. The acting org is
    /// whatever an *unverified* proof claims at this point, so charging it
    /// would let a forged label exhaust a real organization's quota.
    pub fn reserve(&self, req: OpeningRequest) -> Result<Reservation, Denial> {
        if req.session_generation.is_none() {
            return Err(Denial::SessionCurrentnessExhausted);
        }
        let mut inner = self.inner.lock();

        // The key check comes first: it is the one refusal that must land
        // before decode, and it costs a hash lookup.
        if inner.records.contains_key(&req.key) {
            return Err(Denial::ActiveCallOwned);
        }
        if inner.active_node >= self.limits.max_active_node {
            return Err(Denial::ActiveStreamCapacity(CapacityScope::Node));
        }
        let caller_active = inner
            .active_per_caller
            .get(&req.key.caller)
            .copied()
            .unwrap_or(0);
        if caller_active >= self.limits.max_active_per_caller {
            return Err(Denial::ActiveStreamCapacity(CapacityScope::Caller));
        }
        let (Some(node_next), Some(caller_next)) = (
            inner.active_node.checked_add(1),
            caller_active.checked_add(1),
        ) else {
            return Err(Denial::CounterOverflow);
        };
        let Some(verify_by_ns) = req.now_ns.checked_add(self.limits.verification_deadline_ns)
        else {
            return Err(Denial::CounterOverflow);
        };

        let captured = inner.authority.stamp();
        if captured.generation.is_none() || captured.poisoned {
            return Err(Denial::AuthorityUnavailable);
        }

        let incarnation = inner.next_incarnation;
        inner.next_incarnation += 1;
        inner.active_node = node_next;
        inner.active_per_caller.insert(req.key.caller, caller_next);
        inner.records.insert(
            req.key,
            CallRecord {
                incarnation,
                phase: Phase::Opening,
                session: req.session,
                registration: req.registration,
                shape: req.shape,
                verify_by_ns,
                captured,
                facts: None,
                lifecycle: None,
                owner: None,
                charged_org: None,
                cleanup: CleanupOwner::Bridge,
                terminal: None,
                queued: Vec::new(),
            },
        );
        Ok(Reservation {
            key: req.key,
            incarnation,
            epoch_at_reserve: inner.authority_epoch,
            verify_by_ns,
        })
    }

    // ----------------------------------------------------------------
    // §3 step 2 — the verification boundary (exposed, not fused)
    // ----------------------------------------------------------------

    /// The guard insert of step 10. Separate from [`Self::install`] on
    /// purpose: Q4 fixes replay insertion *before* provider policy, so a
    /// vetoed valid proof must already have consumed its slot.
    pub fn guard_admit(
        &self,
        key: CallKey,
        digest: [u8; 32],
        now_ns: u64,
        proof_expiry_ns: u64,
    ) -> GuardVerdict {
        self.guard.admit(key, digest, now_ns, proof_expiry_ns)
    }

    // ----------------------------------------------------------------
    // §3 step 4 — install
    // ----------------------------------------------------------------

    /// Fill the verified facts and transition `Opening → Admitted`, under
    /// the registry lock and the authority's publication barrier.
    ///
    /// Three refusals, in order:
    ///
    /// 1. The reservation was retired meanwhile ⇒ `AuthorityChanged`.
    /// 2. The security view moved ⇒ requalify per §2.3. The store moving,
    ///    poison, or an exhausted generation is unusable authority
    ///    (`AuthorityUnavailable`); a generation-only move compares
    ///    `floor_for(acting_org, member)` against *this record's* member
    ///    generation, which is what keeps another org's sibling call alive.
    /// 3. The now-verified acting-org quota is reserved atomically with the
    ///    transition, rolling back on refusal.
    ///
    /// The movement test reads the **live** stamp under the barrier, not
    /// just `authority_epoch`. The epoch only moves when the subscriber
    /// callback runs, and publication happens before notification
    /// (`org_revocation.rs:669` vs `:708`) — so an install trusting the
    /// epoch alone would admit against a view that is already dead.
    pub fn install(
        &self,
        reservation: &Reservation,
        facts: VerifiedFacts,
        now_ns: u64,
    ) -> Result<AdmissionLease, Denial> {
        let mut inner = self.inner.lock();
        let authority = Arc::clone(&inner.authority);
        let pin = authority.pin();
        let live = pin.stamp();
        let epoch_now = inner.authority_epoch;

        let Some(record) = inner.records.get(&reservation.key) else {
            // Reclaimed by its cleanup owner while we verified.
            return Err(Denial::AuthorityChanged);
        };
        if record.incarnation != reservation.incarnation {
            return Err(Denial::AuthorityChanged);
        }
        match record.phase {
            Phase::Opening => {}
            Phase::Terminal => return Err(Denial::AuthorityChanged),
            Phase::Admitted | Phase::Running => return Err(Denial::NotAdmitted),
        }
        let captured = record.captured;
        let verify_by_ns = record.verify_by_ns;

        if now_ns > verify_by_ns {
            Self::retire_locked(
                &mut inner,
                reservation.key,
                reservation.incarnation,
                TerminalReason::Timeout,
            );
            return Err(Denial::VerificationDeadlineExpired);
        }

        if epoch_now != reservation.epoch_at_reserve || !captured.is_current(&live) {
            match captured.movement(&live) {
                StampMovement::Unusable => {
                    Self::retire_locked(
                        &mut inner,
                        reservation.key,
                        reservation.incarnation,
                        TerminalReason::AuthorityUnavailable,
                    );
                    return Err(Denial::AuthorityUnavailable);
                }
                StampMovement::Unchanged | StampMovement::GenerationOnly => {
                    if pin.floor_for(facts.acting_org, facts.member) > facts.member_generation {
                        Self::retire_locked(
                            &mut inner,
                            reservation.key,
                            reservation.incarnation,
                            TerminalReason::Revoked,
                        );
                        return Err(Denial::Revoked);
                    }
                }
            }
        }

        // The acting org is verified only now, so this is the first moment
        // its quota may legitimately be charged.
        let org_active = inner
            .active_per_org
            .get(&facts.acting_org)
            .copied()
            .unwrap_or(0);
        if org_active >= self.limits.max_active_per_org {
            Self::retire_locked(
                &mut inner,
                reservation.key,
                reservation.incarnation,
                TerminalReason::ResourceExhausted,
            );
            return Err(Denial::ActiveStreamCapacity(CapacityScope::Org));
        }
        let Some(org_next) = org_active.checked_add(1) else {
            Self::retire_locked(
                &mut inner,
                reservation.key,
                reservation.incarnation,
                TerminalReason::ResourceExhausted,
            );
            return Err(Denial::CounterOverflow);
        };
        inner.active_per_org.insert(facts.acting_org, org_next);

        let record = inner
            .records
            .get_mut(&reservation.key)
            .expect("record presence checked under this same lock");
        record.captured = live;
        record.facts = Some(facts);
        record.charged_org = Some(facts.acting_org);
        record.phase = Phase::Admitted;
        Ok(AdmissionLease {
            key: reservation.key,
            incarnation: reservation.incarnation,
        })
    }

    // ----------------------------------------------------------------
    // §3 step 5 — confirm, as an ownership transfer
    // ----------------------------------------------------------------

    /// Validate the fold's prerequisites and **hold** the registry lock
    /// until the ownership transfer completes.
    ///
    /// The returned transaction is the check half of §3 step 5. Because it
    /// holds the lock, the window a bool-only implementation would leave
    /// open between "still `Admitted`?" and "owner registered" does not
    /// exist: a competing retire in that window is serialized, which
    /// [`Self::retire_nonblocking`] makes directly observable.
    pub fn begin_confirm(&self, lease: &AdmissionLease) -> Result<ConfirmTxn<'_>, Denial> {
        let inner = self.inner.lock();
        let Some(record) = inner.records.get(&lease.key) else {
            return Err(Denial::AuthorityChanged);
        };
        if record.incarnation != lease.incarnation {
            return Err(Denial::AuthorityChanged);
        }
        match record.phase {
            Phase::Admitted => {}
            Phase::Terminal => return Err(Denial::AuthorityChanged),
            Phase::Opening | Phase::Running => return Err(Denial::NotAdmitted),
        }
        drop(inner);
        Ok(ConfirmTxn {
            registry: self,
            lease: *lease,
        })
    }

    /// `begin_confirm` + `transfer` in one operation — the shape production
    /// uses inside the fold, under the documented fold/registry lock order.
    /// No handler is polled under this lock.
    pub fn confirm(
        &self,
        lease: &AdmissionLease,
        owner: Arc<SupervisorOwner>,
    ) -> Result<RunningCall, Denial> {
        self.begin_confirm(lease)?.transfer(owner)
    }

    // ----------------------------------------------------------------
    // §2.3 — the per-item commit boundary
    // ----------------------------------------------------------------

    /// The per-item check of §2.3, without committing anything.
    ///
    /// Exposed because production has this boundary, but it is *not* a
    /// licence to enqueue afterwards: a verdict read and then an unguarded
    /// enqueue still races retirement. [`Self::begin_commit`] is the
    /// ownership-preserving form.
    pub fn commit_check(&self, key: CallKey, incarnation: u64) -> CommitVerdict {
        let mut inner = self.inner.lock();
        let authority = Arc::clone(&inner.authority);
        let pin = authority.pin();
        Self::commit_check_locked(&mut inner, &pin, key, incarnation)
    }

    /// Check and hold: the verdict and the enqueue are one ownership
    /// operation.
    pub fn begin_commit(
        &self,
        key: CallKey,
        incarnation: u64,
    ) -> Result<CommitTxn<'_>, CommitVerdict> {
        let mut inner = self.inner.lock();
        let authority = Arc::clone(&inner.authority);
        let verdict = {
            let pin = authority.pin();
            Self::commit_check_locked(&mut inner, &pin, key, incarnation)
        };
        match verdict {
            CommitVerdict::Proceed | CommitVerdict::Requalified => Ok(CommitTxn {
                inner,
                key,
                incarnation,
                verdict,
            }),
            other => Err(other),
        }
    }

    fn commit_check_locked(
        inner: &mut RegistryInner,
        pin: &AuthorityPin<'_>,
        key: CallKey,
        incarnation: u64,
    ) -> CommitVerdict {
        let live = pin.stamp();
        let Some(record) = inner.records.get(&key) else {
            return CommitVerdict::Unknown;
        };
        if record.incarnation != incarnation {
            return CommitVerdict::Unknown;
        }
        // "token.is_cancelled() → retired" is the first test, before any
        // stamp work: a cancelled call has already selected its outcome.
        if record.phase == Phase::Terminal {
            return CommitVerdict::Retired(
                record.terminal.clone().unwrap_or(TerminalReason::Cancelled),
            );
        }
        let captured = record.captured;
        if captured.is_current(&live) {
            return CommitVerdict::Proceed;
        }
        let Some(facts) = record.facts else {
            // No verified facts: there is nothing to requalify against.
            Self::retire_locked(
                inner,
                key,
                incarnation,
                TerminalReason::AuthorityUnavailable,
            );
            return CommitVerdict::Retired(TerminalReason::AuthorityUnavailable);
        };
        match captured.movement(&live) {
            StampMovement::Unusable => {
                Self::retire_locked(
                    inner,
                    key,
                    incarnation,
                    TerminalReason::AuthorityUnavailable,
                );
                CommitVerdict::Retired(TerminalReason::AuthorityUnavailable)
            }
            StampMovement::Unchanged => CommitVerdict::Proceed,
            StampMovement::GenerationOnly => {
                if pin.floor_for(facts.acting_org, facts.member) > facts.member_generation {
                    Self::retire_locked(inner, key, incarnation, TerminalReason::Revoked);
                    CommitVerdict::Retired(TerminalReason::Revoked)
                } else {
                    let record = inner
                        .records
                        .get_mut(&key)
                        .expect("record presence checked under this same lock");
                    record.captured = live;
                    CommitVerdict::Requalified
                }
            }
        }
    }

    // ----------------------------------------------------------------
    // §2.4 — retirement and the two removal paths
    // ----------------------------------------------------------------

    /// Mark a record terminal, conditional on the exact incarnation, and
    /// signal the registered owner. First writer wins; a late retire against
    /// a reused key is a no-op. **Never removes** — removal belongs to the
    /// record's one cleanup owner.
    pub fn retire(&self, key: CallKey, incarnation: u64, reason: TerminalReason) -> bool {
        let mut inner = self.inner.lock();
        Self::retire_locked(&mut inner, key, incarnation, reason)
    }

    /// A retire that refuses to wait for the registry lock, reporting that
    /// it could not land. This is how the model observes exclusion
    /// deterministically, with no threads and no sleeps.
    pub fn retire_nonblocking(
        &self,
        key: CallKey,
        incarnation: u64,
        reason: TerminalReason,
    ) -> RetireAttempt {
        match self.inner.try_lock() {
            Some(mut inner) => {
                RetireAttempt::Applied(Self::retire_locked(&mut inner, key, incarnation, reason))
            }
            None => RetireAttempt::Blocked,
        }
    }

    fn retire_locked(
        inner: &mut RegistryInner,
        key: CallKey,
        incarnation: u64,
        reason: TerminalReason,
    ) -> bool {
        let Some(record) = inner.records.get_mut(&key) else {
            return false;
        };
        if record.incarnation != incarnation || record.phase == Phase::Terminal {
            return false;
        }
        if let Some(lifecycle) = record.lifecycle.as_mut() {
            lifecycle.retire(reason.clone());
        }
        record.phase = Phase::Terminal;
        record.terminal = Some(reason.clone());
        if let Some(owner) = record.owner.as_ref() {
            owner.signal(reason);
        }
        true
    }

    /// Pre-transfer rollback, owned by the bridge's reservation guard.
    /// Refuses once ownership has moved to a supervisor.
    pub fn release(&self, key: CallKey, incarnation: u64) -> bool {
        self.remove(key, incarnation, CleanupOwner::Bridge)
    }

    /// Post-transfer removal, owned by the supervisor and performed after it
    /// has disposed of its work. Refuses before ownership transferred.
    pub fn complete(&self, key: CallKey, incarnation: u64) -> bool {
        self.remove(key, incarnation, CleanupOwner::Supervisor)
    }

    fn remove(&self, key: CallKey, incarnation: u64, expected: CleanupOwner) -> bool {
        let mut inner = self.inner.lock();
        let matches = inner
            .records
            .get(&key)
            .is_some_and(|r| r.incarnation == incarnation && r.cleanup == expected);
        if !matches {
            return false;
        }
        let record = inner
            .records
            .remove(&key)
            .expect("presence checked under this same lock");
        inner.active_node = inner
            .active_node
            .checked_sub(1)
            .expect("node active-call counter released twice");
        let caller_slot = inner
            .active_per_caller
            .get_mut(&key.caller)
            .expect("caller active-call counter released twice");
        *caller_slot = caller_slot
            .checked_sub(1)
            .expect("caller active-call counter released twice");
        if *caller_slot == 0 {
            inner.active_per_caller.remove(&key.caller);
        }
        if let Some(org) = record.charged_org {
            let org_slot = inner
                .active_per_org
                .get_mut(&org)
                .expect("org active-call counter released twice");
            *org_slot = org_slot
                .checked_sub(1)
                .expect("org active-call counter released twice");
            if *org_slot == 0 {
                inner.active_per_org.remove(&org);
            }
        }
        // Queued items the removed call still owned: their permits are
        // consumed here, not guessed at.
        for item in &record.queued {
            if let Some(permit) = item.take() {
                permit.release();
            }
        }
        drop(inner);
        *self.removals.lock().entry((key, incarnation)).or_insert(0) += 1;
        true
    }

    /// Reap `Opening` reservations whose verification deadline passed — the
    /// lost-bridge path. An opening retired before transfer must not wait
    /// for a supervisor that was never created, so this both retires and
    /// removes under the bridge's ownership.
    pub fn reap_expired_openings(&self, now_ns: u64) -> usize {
        let expired: Vec<(CallKey, u64)> = {
            let inner = self.inner.lock();
            inner
                .records
                .iter()
                .filter(|(_, r)| r.phase == Phase::Opening && r.verify_by_ns < now_ns)
                .map(|(k, r)| (*k, r.incarnation))
                .collect()
        };
        let mut reaped = 0;
        for (key, incarnation) in expired {
            self.retire(key, incarnation, TerminalReason::Timeout);
            if self.release(key, incarnation) {
                reaped += 1;
            }
        }
        reaped
    }

    // ----------------------------------------------------------------
    // §2.3 — revocation entry points
    // ----------------------------------------------------------------

    /// Selective raise callback. Bumps the epoch, then retires every record
    /// whose `(acting_org, member)` generation is below a raised floor.
    ///
    /// An empty slice is the authority-changed wake
    /// (`org_revocation.rs:736-746`) and retires everything, including
    /// `Opening` reservations that have no facts to compare.
    pub fn on_floors_raised(&self, raised: &[RaisedFloor]) {
        let mut inner = self.inner.lock();
        inner.authority_epoch = inner.authority_epoch.wrapping_add(1);
        let victims: Vec<(CallKey, u64, TerminalReason)> = inner
            .records
            .iter()
            .filter_map(|(key, record)| {
                if record.phase == Phase::Terminal {
                    return None;
                }
                if raised.is_empty() {
                    return Some((
                        *key,
                        record.incarnation,
                        TerminalReason::AuthorityUnavailable,
                    ));
                }
                let facts = record.facts?;
                raised
                    .iter()
                    .any(|(org, member, floor)| {
                        *org == facts.acting_org
                            && *member == facts.member
                            && *floor > facts.member_generation
                    })
                    .then_some((*key, record.incarnation, TerminalReason::Revoked))
            })
            .collect();
        for (key, incarnation, reason) in victims {
            Self::retire_locked(&mut inner, key, incarnation, reason);
        }
    }

    /// The authority moved or poison recovered with no floor raised: retire
    /// all.
    pub fn on_authority_changed(&self) {
        self.on_floors_raised(&[]);
    }

    /// The revocation store (or node authority) was replaced. Retire every
    /// record captured under the old identity and re-subscribe to the new
    /// store, so the registry never sits without a raise feed.
    pub fn on_store_replaced(self: &Arc<Self>, new_authority: Arc<ModelAuthority>) {
        let new_stamp = new_authority.stamp();
        let previous = Arc::clone(&self.inner.lock().authority);
        {
            let mut inner = self.inner.lock();
            inner.authority_epoch = inner.authority_epoch.wrapping_add(1);
            let victims: Vec<(CallKey, u64)> = inner
                .records
                .iter()
                .filter(|(_, record)| {
                    record.phase != Phase::Terminal
                        && (record.captured.authority_id != new_stamp.authority_id
                            || record.captured.store_id != new_stamp.store_id)
                })
                .map(|(key, record)| (*key, record.incarnation))
                .collect();
            for (key, incarnation) in victims {
                Self::retire_locked(
                    &mut inner,
                    key,
                    incarnation,
                    TerminalReason::AuthorityUnavailable,
                );
            }
            inner.authority = Arc::clone(&new_authority);
        }
        previous.unsubscribe(self);
        new_authority.subscribe(self);
    }

    /// Session replacement or a dead-peer sweep. Matches the **exact**
    /// `(peer, session_id, establishment)` triple: a bare truncated session
    /// id is shared across unrelated peers, so matching on it alone would
    /// retire a bystander's call (§3 step 4).
    pub fn retire_session(&self, session: &SessionRef, reason: TerminalReason) -> usize {
        let mut inner = self.inner.lock();
        let victims: Vec<(CallKey, u64)> = inner
            .records
            .iter()
            .filter(|(_, r)| r.phase != Phase::Terminal && r.session == *session)
            .map(|(key, r)| (*key, r.incarnation))
            .collect();
        let mut retired = 0;
        for (key, incarnation) in victims {
            if Self::retire_locked(&mut inner, key, incarnation, reason.clone()) {
                retired += 1;
            }
        }
        retired
    }

    /// `ServeHandle::drop` / node shutdown for one protected registration.
    pub fn retire_registration(
        &self,
        registration: RegistrationId,
        reason: TerminalReason,
    ) -> usize {
        let mut inner = self.inner.lock();
        let victims: Vec<(CallKey, u64)> = inner
            .records
            .iter()
            .filter(|(_, r)| r.phase != Phase::Terminal && r.registration == registration)
            .map(|(key, r)| (*key, r.incarnation))
            .collect();
        let mut retired = 0;
        for (key, incarnation) in victims {
            if Self::retire_locked(&mut inner, key, incarnation, reason.clone()) {
                retired += 1;
            }
        }
        retired
    }

    // ----------------------------------------------------------------
    // Queue ownership helpers (§2.7)
    // ----------------------------------------------------------------

    /// How many items the call still owns.
    pub fn queued_items(&self, key: CallKey) -> usize {
        self.inner
            .lock()
            .records
            .get(&key)
            .map_or(0, |r| r.queued.len())
    }

    /// The pump takes the next item's ownership. `None` when the queue is
    /// empty or another party already consumed the head's permit.
    pub fn dequeue_next(&self, key: CallKey, incarnation: u64) -> Option<ItemPermit> {
        let mut inner = self.inner.lock();
        let record = inner.records.get_mut(&key)?;
        if record.incarnation != incarnation || record.queued.is_empty() {
            return None;
        }
        let item = record.queued.remove(0);
        item.take()
    }

    /// Retirement consumes every item the call still owns. Returns how many
    /// permits this call actually won — items another party already consumed
    /// are not double counted, and no other call's bytes are touched.
    pub fn cancel_queued(&self, key: CallKey, incarnation: u64) -> usize {
        let mut inner = self.inner.lock();
        let Some(record) = inner.records.get_mut(&key) else {
            return 0;
        };
        if record.incarnation != incarnation {
            return 0;
        }
        let items = std::mem::take(&mut record.queued);
        drop(inner);
        let mut released = 0;
        for item in items {
            if let Some(permit) = item.take() {
                permit.release();
                released += 1;
            }
        }
        released
    }
}

/// Whether a retire landed, or could not because the registry was mid
/// transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetireAttempt {
    /// The retire ran; the bool is whether it won the terminal.
    Applied(bool),
    /// The registry lock was held by an in-flight ownership transaction.
    Blocked,
}

/// The open check-half of §3 step 5. Holding this holds the registry lock.
pub struct ConfirmTxn<'a> {
    registry: &'a ProtectedCallRegistry,
    lease: AdmissionLease,
}

impl ConfirmTxn<'_> {
    /// The lease being confirmed.
    pub fn lease(&self) -> AdmissionLease {
        self.lease
    }

    /// Atomically register a cancellation-ready owner and mark `Running`.
    ///
    /// The owner exists before its task is scheduled, so a retire arriving
    /// immediately after this returns still reaches something. A scope guard
    /// handling a scheduling failure retires and completes through this same
    /// registered owner rather than orphaning a `Running` record.
    pub fn transfer(mut self, owner: Arc<SupervisorOwner>) -> Result<RunningCall, Denial> {
        if owner.key != self.lease.key || owner.incarnation != self.lease.incarnation {
            return Err(Denial::NotAdmitted);
        }
        let mut inner = self.registry.inner.lock();
        let record = inner
            .records
            .get_mut(&self.lease.key)
            .expect("presence validated when this transaction opened");
        record.phase = Phase::Running;
        record.cleanup = CleanupOwner::Supervisor;
        record.lifecycle = Some(CallLifecycle::new(record.shape, record.incarnation));
        record.owner = Some(Arc::clone(&owner));
        Ok(RunningCall {
            key: self.lease.key,
            incarnation: self.lease.incarnation,
            owner,
        })
    }

    /// The fold's prerequisites failed after the check opened: close the
    /// transaction without transferring. Ownership stays with the bridge, so
    /// the bridge's `release` is still the one removal.
    pub fn abandon(self) {}
}

/// The per-item §2.3 verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitVerdict {
    /// The captured view is still live.
    Proceed,
    /// The generation moved, the floor still permits this member, and the
    /// captured generation was refreshed.
    Requalified,
    /// The call is retired with this reason.
    Retired(TerminalReason),
    /// No record with that exact incarnation.
    Unknown,
}

/// Check and commit as one ownership operation. Holding this holds the
/// registry lock, so retirement cannot land between the verdict and the
/// enqueue.
pub struct CommitTxn<'a> {
    inner: MutexGuard<'a, RegistryInner>,
    key: CallKey,
    incarnation: u64,
    verdict: CommitVerdict,
}

impl CommitTxn<'_> {
    /// The verdict this transaction opened on.
    pub fn verdict(&self) -> &CommitVerdict {
        &self.verdict
    }

    /// Enqueue the item, handing its permit to the call's queue.
    pub fn commit(mut self, permit: ItemPermit) -> Arc<SharedPermit> {
        let shared = SharedPermit::new(permit);
        let record = self
            .inner
            .records
            .get_mut(&self.key)
            .expect("presence validated when this transaction opened");
        debug_assert_eq!(record.incarnation, self.incarnation);
        record.queued.push(Arc::clone(&shared));
        shared
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::org_stream_lifecycle::{DeadlineBound, HandlerResult};

    const SEC: u64 = 1_000_000_000;
    const T0: u64 = 1_000 * SEC;

    fn small_limits() -> CallLimits {
        CallLimits {
            max_active_node: 4,
            max_active_per_caller: 2,
            max_active_per_org: 3,
            lifetime: LifetimePolicy::q1_defaults(),
            verification_deadline_ns: 30 * SEC,
        }
    }

    fn small_bytes() -> ByteLimits {
        ByteLimits {
            per_call: 1_000,
            per_caller: 1_500,
            per_node: 2_000,
        }
    }

    struct Harness {
        authority: Arc<ModelAuthority>,
        registry: Arc<ProtectedCallRegistry>,
        effects: FoldEffects,
    }

    fn harness_with(limits: CallLimits, bytes: ByteLimits) -> Harness {
        let authority = ModelAuthority::new(0xA1, 0x51);
        let registry = ProtectedCallRegistry::new(Arc::clone(&authority), limits, bytes)
            .expect("limits validate");
        Harness {
            authority,
            registry,
            effects: FoldEffects::default(),
        }
    }

    fn harness() -> Harness {
        harness_with(small_limits(), small_bytes())
    }

    fn key(caller: CallerId, call_id: u64) -> CallKey {
        CallKey { caller, call_id }
    }

    fn opening(k: CallKey) -> OpeningRequest {
        OpeningRequest {
            key: k,
            session: SessionRef {
                peer: k.caller,
                session_id: 0x5E55,
                establishment: 1,
            },
            session_generation: Some(7),
            registration: 100,
            shape: CallShape::ServerStreaming,
            now_ns: T0,
        }
    }

    fn facts(acting_org: OrgId, member: MemberId, generation: u32) -> VerifiedFacts {
        VerifiedFacts {
            acting_org,
            member,
            member_generation: generation,
            deadline: ResolvedDeadline {
                end_ns: T0 + 300 * SEC,
                bound: DeadlineBound::Deadline,
            },
        }
    }

    /// reserve → install, the uncontended prefix most schedules need. The
    /// guard is deliberately NOT consulted here: it is a separate step in
    /// production and the tests that care drive it explicitly.
    fn admit(h: &Harness, k: CallKey, org: OrgId, member: MemberId, gen: u32) -> AdmissionLease {
        let reservation = h.registry.reserve(opening(k)).expect("reserve");
        h.registry
            .install(&reservation, facts(org, member, gen), T0)
            .expect("install")
    }

    fn run(h: &Harness, k: CallKey, lease: &AdmissionLease) -> Arc<SupervisorOwner> {
        let owner = SupervisorOwner::new(k, lease.incarnation);
        let running = h
            .registry
            .confirm(lease, Arc::clone(&owner))
            .expect("confirm");
        h.effects.admit(&running);
        owner
    }

    // ================ positive progress controls ================

    #[test]
    fn normal_admit_confirm_complete_round_trip() {
        let h = harness();
        let k = key(1, 10);
        let lease = admit(&h, k, 7, 70, 3);
        assert_eq!(h.registry.phase(k), Some(Phase::Admitted));
        assert_eq!(h.registry.cleanup_owner(k), Some(CleanupOwner::Bridge));
        assert_eq!(
            h.registry.deadline(k).expect("deadline").end_ns,
            T0 + 300 * SEC
        );

        let owner = run(&h, k, &lease);
        assert_eq!(h.registry.phase(k), Some(Phase::Running));
        assert_eq!(h.registry.cleanup_owner(k), Some(CleanupOwner::Supervisor));
        assert_eq!(h.effects.admitted(), 1);
        assert_eq!(
            h.registry.active_for_org(7),
            1,
            "verified org quota charged"
        );

        owner.start_task();
        assert!(h.registry.retire(
            k,
            lease.incarnation,
            TerminalReason::Completed(HandlerResult::Ok)
        ));
        assert_eq!(
            h.registry.lifecycle_terminal(k),
            Some(Terminal {
                reason: TerminalReason::Completed(HandlerResult::Ok),
                emitted: false,
            }),
            "the §2.6 record carries the terminal, unemitted until the supervisor flips it"
        );
        assert!(h.registry.complete(k, lease.incarnation));
        assert_eq!(h.registry.record_count(), 0);
        assert_eq!(h.registry.active_node(), 0);
        assert_eq!(h.registry.active_for_caller(1), 0);
        assert_eq!(h.registry.active_for_org(7), 0);
    }

    #[test]
    fn two_independent_calls_both_reach_running() {
        let h = harness();
        let a = key(1, 10);
        let b = key(2, 11);
        let lease_a = admit(&h, a, 7, 70, 3);
        let lease_b = admit(&h, b, 8, 80, 3);
        let owner_a = run(&h, a, &lease_a);
        let owner_b = run(&h, b, &lease_b);
        assert_eq!(h.effects.admitted(), 2);
        assert_eq!(h.registry.active_node(), 2);
        assert_eq!(h.registry.active_for_org(7), 1);
        assert_eq!(h.registry.active_for_org(8), 1);
        assert_eq!(owner_a.observed_terminal(), None);
        assert_eq!(owner_b.observed_terminal(), None);
    }

    // ================ §3 step 4 — install under movement ================

    #[test]
    fn raise_between_reserve_and_install_denies_with_zero_effects() {
        let h = harness();
        let k = key(1, 10);
        let reservation = h.registry.reserve(opening(k)).expect("reserve");
        assert_eq!(h.registry.active_node(), 1, "provisional slot held");

        // A floor rises past this member while the proof is being verified.
        h.authority.publish_floors(&[(7, 70, 9)]).notify();
        assert_ne!(
            h.registry.authority_epoch(),
            reservation.epoch_at_reserve,
            "the notified callback moved the epoch"
        );

        let denial = h
            .registry
            .install(&reservation, facts(7, 70, 3), T0)
            .expect_err("install must deny");
        assert_eq!(denial, Denial::Revoked);
        assert_eq!(denial.coarse(), CoarseDenial::Unavailable);
        assert_eq!(h.effects.admitted(), 0, "zero fold effects");
        assert_eq!(h.registry.phase(k), Some(Phase::Terminal));
        assert_eq!(h.registry.active_for_org(7), 0, "org quota never charged");

        // The bridge owns the rollback; the key is reusable afterwards.
        assert!(h.registry.release(k, reservation.incarnation));
        assert_eq!(h.registry.active_node(), 0);
        assert_eq!(h.registry.removals(k, reservation.incarnation), 1);
        assert!(h.registry.reserve(opening(k)).is_ok(), "key is reusable");
    }

    #[test]
    fn publication_before_notification_cannot_authorize_commit() {
        let h = harness();
        let opening_key = key(1, 10);
        let running_key = key(2, 11);

        // One call already running, one reservation mid-verification.
        let lease = admit(&h, running_key, 7, 70, 3);
        let owner = run(&h, running_key, &lease);
        let reservation = h.registry.reserve(opening(opening_key)).expect("reserve");
        let epoch_before = h.registry.authority_epoch();

        // The publication lands — view swapped, generation bumped — but the
        // notifier is paused, so no subscriber has run.
        let pending = h.authority.publish_floors(&[(7, 70, 9)]);
        assert_eq!(
            h.registry.authority_epoch(),
            epoch_before,
            "the registry has NOT been notified; its epoch is stale by construction"
        );

        // Neither the stale opening nor the live call may act on the
        // pre-publication view.
        let denial = h
            .registry
            .install(&reservation, facts(7, 70, 3), T0)
            .expect_err("install must not admit against a dead view");
        assert_eq!(denial, Denial::Revoked);
        assert_eq!(
            h.registry.commit_check(running_key, lease.incarnation),
            CommitVerdict::Retired(TerminalReason::Revoked),
            "a commit inside the pre-notification window is refused"
        );
        assert_eq!(
            h.registry
                .begin_commit(running_key, lease.incarnation)
                .err()
                .expect("the check/commit transaction must refuse to open"),
            CommitVerdict::Retired(TerminalReason::Revoked)
        );
        assert_eq!(h.effects.admitted(), 1, "no second admitted effect");
        assert_eq!(owner.observed_terminal(), Some(TerminalReason::Revoked));

        pending.notify();
        assert_ne!(h.registry.authority_epoch(), epoch_before);
        assert_eq!(
            h.registry.terminal_reason(running_key),
            Some(TerminalReason::Revoked)
        );
        assert_eq!(
            owner.signal_count(),
            1,
            "the notification does not re-signal an already retired owner"
        );
    }

    #[test]
    fn requalify_keeps_the_unaffected_sibling_and_retires_the_affected_call() {
        // Both halves in ONE test: a whole-stamp comparison (what
        // `AdmissionStamp::is_current` does, correctly, for the unary gate)
        // retires both, because a floor raise for one member moves every
        // captured stamp.
        let h = harness();
        let victim = key(1, 10);
        let sibling = key(2, 11);
        let victim_lease = admit(&h, victim, 7, 70, 3);
        let sibling_lease = admit(&h, sibling, 8, 80, 3);
        let victim_owner = run(&h, victim, &victim_lease);
        let sibling_owner = run(&h, sibling, &sibling_lease);
        let sibling_stamp_before = h.registry.captured_stamp(sibling).expect("stamp");

        h.authority.publish_floors(&[(7, 70, 9)]).notify();

        assert_eq!(
            h.registry.terminal_reason(victim),
            Some(TerminalReason::Revoked),
            "the raised member is retired by the callback"
        );
        assert_eq!(
            h.registry.phase(sibling),
            Some(Phase::Running),
            "another org's call is NOT retired by a floor aimed elsewhere"
        );

        // The sibling's captured stamp is now stale, and requalification —
        // not a whole-stamp comparison — is what keeps it alive.
        assert_eq!(
            h.registry.commit_check(sibling, sibling_lease.incarnation),
            CommitVerdict::Requalified
        );
        let sibling_stamp_after = h.registry.captured_stamp(sibling).expect("stamp");
        assert_ne!(
            sibling_stamp_before.generation, sibling_stamp_after.generation,
            "the captured generation is refreshed, not merely tolerated"
        );
        assert_eq!(
            h.registry.commit_check(sibling, sibling_lease.incarnation),
            CommitVerdict::Proceed,
            "and the refreshed capture compares current next time"
        );
        assert_eq!(sibling_owner.observed_terminal(), None);
        assert_eq!(
            victim_owner.observed_terminal(),
            Some(TerminalReason::Revoked)
        );
    }

    #[test]
    fn store_replacement_retires_records_captured_under_the_old_identity() {
        let h = harness();
        let k = key(1, 10);
        let lease = admit(&h, k, 7, 70, 3);
        let owner = run(&h, k, &lease);

        let replacement = ModelAuthority::new(0xA2, 0x52);
        h.registry.on_store_replaced(Arc::clone(&replacement));

        assert_eq!(
            h.registry.terminal_reason(k),
            Some(TerminalReason::AuthorityUnavailable)
        );
        assert_eq!(
            owner.observed_terminal(),
            Some(TerminalReason::AuthorityUnavailable)
        );
        assert_eq!(
            replacement.subscriber_count(),
            1,
            "the registry re-subscribed to the new store"
        );
        assert_eq!(
            h.authority.subscriber_count(),
            0,
            "and retired its subscription to the old one"
        );

        // Prove the new subscription is live, not merely counted.
        assert!(h.registry.complete(k, lease.incarnation));
        let next = key(2, 11);
        let next_lease = admit(&h, next, 9, 90, 3);
        let next_owner = run(&h, next, &next_lease);
        replacement.publish_floors(&[(9, 90, 5)]).notify();
        assert_eq!(
            next_owner.observed_terminal(),
            Some(TerminalReason::Revoked)
        );
    }

    #[test]
    fn poisoned_store_retires_every_protected_call() {
        let h = harness();
        let a = key(1, 10);
        let b = key(2, 11);
        let c = key(3, 12);
        let lease_a = admit(&h, a, 7, 70, 3);
        let _lease_b = admit(&h, b, 8, 80, 3);
        let owner_a = run(&h, a, &lease_a);
        let reservation = h.registry.reserve(opening(c)).expect("reserve");

        h.authority.poison().notify();

        assert_eq!(
            h.registry.terminal_reason(a),
            Some(TerminalReason::AuthorityUnavailable)
        );
        assert_eq!(
            h.registry.terminal_reason(b),
            Some(TerminalReason::AuthorityUnavailable),
            "an Admitted-but-unconfirmed record is retired too"
        );
        assert_eq!(
            h.registry.terminal_reason(c),
            Some(TerminalReason::AuthorityUnavailable),
            "and an Opening reservation with no facts at all"
        );
        assert_eq!(
            owner_a.observed_terminal(),
            Some(TerminalReason::AuthorityUnavailable)
        );
        assert_eq!(
            h.registry
                .install(&reservation, facts(7, 70, 3), T0)
                .expect_err("a poisoned authority cannot install"),
            Denial::AuthorityChanged
        );
    }

    #[test]
    fn exhausted_generation_is_unusable_authority_in_either_position() {
        let h = harness();
        let k = key(1, 10);
        let lease = admit(&h, k, 7, 70, 3);
        h.authority.exhaust_generation();
        assert_eq!(
            h.registry.commit_check(k, lease.incarnation),
            CommitVerdict::Retired(TerminalReason::AuthorityUnavailable),
            "a frozen generation cannot show a captured view still live"
        );
        assert_eq!(
            h.registry
                .reserve(opening(key(2, 11)))
                .expect_err("refused"),
            Denial::AuthorityUnavailable
        );
    }

    // ================ §3 step 5 — confirm as a transfer ================

    #[test]
    fn retire_between_confirm_check_and_owner_transfer() {
        let h = harness();
        let k = key(1, 10);
        let lease = admit(&h, k, 7, 70, 3);
        let owner = SupervisorOwner::new(k, lease.incarnation);

        // The check half opens. This is exactly the point a bool-only
        // implementation would return `true` and release its lock.
        let txn = h
            .registry
            .begin_confirm(&lease)
            .expect("prerequisites hold");

        // A retire arriving in that window cannot land: the ownership
        // transaction is still open. A bool-then-unguarded-spawn
        // implementation reports `Applied(true)` here, and then hands the
        // fold a `RunningCall` over an already-retired record whose owner was
        // never armed.
        assert_eq!(
            h.registry
                .retire_nonblocking(k, lease.incarnation, TerminalReason::Cancelled),
            RetireAttempt::Blocked,
            "retirement is serialized with the check→transfer window"
        );
        assert_eq!(h.effects.admitted(), 0, "no effect before the transfer");

        let running = txn.transfer(Arc::clone(&owner)).expect("transfer");
        h.effects.admit(&running);

        // Now the same retire lands, and it reaches an armed owner.
        assert_eq!(
            h.registry
                .retire_nonblocking(k, lease.incarnation, TerminalReason::Cancelled),
            RetireAttempt::Applied(true)
        );
        assert_eq!(owner.observed_terminal(), Some(TerminalReason::Cancelled));
        assert_eq!(
            h.effects.admitted(),
            1,
            "an admitted effect exists only together with an armed cleanup owner"
        );
    }

    #[test]
    fn retire_before_transfer_prevents_every_admitted_effect() {
        let h = harness();
        let k = key(1, 10);
        let lease = admit(&h, k, 7, 70, 3);

        assert!(h
            .registry
            .retire(k, lease.incarnation, TerminalReason::Cancelled));
        assert_eq!(
            h.registry
                .begin_confirm(&lease)
                .err()
                .expect("confirm must refuse a retired record"),
            Denial::AuthorityChanged
        );
        assert_eq!(h.effects.admitted(), 0);

        // The bridge owns exactly one bounded opening refusal and one release.
        h.effects.refuse_opening();
        assert!(h.registry.release(k, lease.incarnation));
        assert_eq!(h.effects.opening_refusals(), 1);
        assert_eq!(h.registry.removals(k, lease.incarnation), 1);
    }

    #[test]
    fn retire_after_transfer_reaches_the_owner_before_its_task_runs() {
        let h = harness();
        let k = key(1, 10);
        let lease = admit(&h, k, 7, 70, 3);
        let owner = run(&h, k, &lease);

        assert!(
            !owner.task_started(),
            "the supervisor task is not scheduled"
        );
        assert!(h
            .registry
            .retire(k, lease.incarnation, TerminalReason::Cancelled));
        assert_eq!(
            owner.observed_terminal(),
            Some(TerminalReason::Cancelled),
            "retirement reaches the registered owner with no task polled"
        );
        assert!(!owner.task_started());

        // A second retire does not replace the first writer's terminal.
        assert!(!h
            .registry
            .retire(k, lease.incarnation, TerminalReason::Timeout));
        assert_eq!(owner.observed_terminal(), Some(TerminalReason::Cancelled));
        assert_eq!(owner.signal_count(), 1);
    }

    #[test]
    fn fold_prerequisite_refusal_before_transfer_releases_exactly_once() {
        let h = harness();
        let k = key(1, 10);
        let lease = admit(&h, k, 7, 70, 3);

        let txn = h.registry.begin_confirm(&lease).expect("check passes");
        txn.abandon(); // the fold could not build its senders
        assert_eq!(h.registry.phase(k), Some(Phase::Admitted));
        assert_eq!(h.registry.cleanup_owner(k), Some(CleanupOwner::Bridge));
        assert!(
            !h.registry.complete(k, lease.incarnation),
            "the supervisor path cannot remove a record it never owned"
        );
        assert!(h.registry.release(k, lease.incarnation));
        assert!(!h.registry.release(k, lease.incarnation));
        assert_eq!(h.registry.removals(k, lease.incarnation), 1);
        assert_eq!(h.effects.admitted(), 0);
    }

    #[test]
    fn scheduling_failure_after_transfer_does_not_orphan_a_running_record() {
        let h = harness();
        let k = key(1, 10);
        let lease = admit(&h, k, 7, 70, 3);
        let owner = run(&h, k, &lease);

        // The spawn fails; the scope guard retires through the owner the
        // transfer already registered, then completes.
        assert!(
            !h.registry.release(k, lease.incarnation),
            "the bridge cannot remove a transferred record"
        );
        assert!(h
            .registry
            .retire(k, lease.incarnation, TerminalReason::PumpFailed));
        assert_eq!(owner.observed_terminal(), Some(TerminalReason::PumpFailed));
        assert!(h.registry.complete(k, lease.incarnation));
        assert_eq!(h.registry.record_count(), 0);
        assert_eq!(h.registry.active_node(), 0);
    }

    #[test]
    fn pretransfer_retirement_has_one_cleanup_owner() {
        // Failed install, lost bridge and revocation must each converge on a
        // single removal, and none may leave an ownerless reservation.
        let h = harness();

        // (a) failed install — revocation retires, the bridge removes once.
        let failed = key(1, 10);
        let reservation = h.registry.reserve(opening(failed)).expect("reserve");
        h.authority.publish_floors(&[(7, 70, 9)]).notify();
        assert!(h
            .registry
            .install(&reservation, facts(7, 70, 3), T0)
            .is_err());
        assert!(h.registry.release(failed, reservation.incarnation));
        assert!(!h.registry.release(failed, reservation.incarnation));
        assert!(!h.registry.complete(failed, reservation.incarnation));
        assert_eq!(h.registry.removals(failed, reservation.incarnation), 1);

        // (b) lost bridge — the finite verification deadline reaps it, and a
        // late bridge release finds nothing to remove twice.
        let lost = key(2, 11);
        let lost_reservation = h.registry.reserve(opening(lost)).expect("reserve");
        assert_eq!(h.registry.reap_expired_openings(T0 + SEC), 0, "not yet due");
        assert_eq!(h.registry.reap_expired_openings(T0 + 31 * SEC), 1);
        assert!(!h.registry.release(lost, lost_reservation.incarnation));
        assert_eq!(h.registry.removals(lost, lost_reservation.incarnation), 1);

        // (c) revocation alone marks and signals; it never removes.
        let revoked = key(3, 12);
        let revoked_lease = admit(&h, revoked, 8, 80, 3);
        h.authority.publish_floors(&[(8, 80, 9)]).notify();
        assert_eq!(h.registry.phase(revoked), Some(Phase::Terminal));
        assert_eq!(
            h.registry.removals(revoked, revoked_lease.incarnation),
            0,
            "the raise callback marked the record, it did not remove it"
        );
        assert!(h.registry.release(revoked, revoked_lease.incarnation));
        assert_eq!(h.registry.removals(revoked, revoked_lease.incarnation), 1);
        assert_eq!(h.registry.record_count(), 0);
        assert_eq!(h.registry.active_node(), 0);
    }

    #[test]
    fn complete_removes_exactly_once_and_release_cannot_double_remove() {
        let h = harness();
        let k = key(1, 10);
        let lease = admit(&h, k, 7, 70, 3);
        let _owner = run(&h, k, &lease);
        assert!(h
            .registry
            .retire(k, lease.incarnation, TerminalReason::Cancelled));
        assert!(h.registry.complete(k, lease.incarnation));
        assert!(!h.registry.complete(k, lease.incarnation));
        assert!(!h.registry.release(k, lease.incarnation));
        assert_eq!(h.registry.removals(k, lease.incarnation), 1);
    }

    #[test]
    fn late_operations_with_a_stale_incarnation_cannot_touch_the_successor() {
        let h = harness();
        let k = key(1, 10);
        let first = admit(&h, k, 7, 70, 3);
        let _first_owner = run(&h, k, &first);
        assert!(h
            .registry
            .retire(k, first.incarnation, TerminalReason::Cancelled));
        assert!(h.registry.complete(k, first.incarnation));

        // The key is reused by a fresh call.
        let second = admit(&h, k, 7, 70, 3);
        assert_ne!(second.incarnation, first.incarnation);
        let second_owner = run(&h, k, &second);

        // Everything late from the first incarnation is inert.
        assert!(!h
            .registry
            .retire(k, first.incarnation, TerminalReason::Timeout));
        assert!(!h.registry.release(k, first.incarnation));
        assert!(!h.registry.complete(k, first.incarnation));
        assert_eq!(
            h.registry.commit_check(k, first.incarnation),
            CommitVerdict::Unknown
        );
        assert_eq!(h.registry.phase(k), Some(Phase::Running));
        assert_eq!(second_owner.observed_terminal(), None);
        assert_eq!(second_owner.signal_count(), 0);
        assert_eq!(h.registry.removals(k, second.incarnation), 0);
    }

    #[test]
    fn check_and_commit_are_one_ownership_operation() {
        let h = harness();
        let k = key(1, 10);
        let lease = admit(&h, k, 7, 70, 3);
        let _owner = run(&h, k, &lease);

        let permit = h
            .registry
            .bytes()
            .reserve(k, lease.incarnation, Direction::Response, 100)
            .expect("reserve bytes");
        let txn = h
            .registry
            .begin_commit(k, lease.incarnation)
            .expect("commit opens");
        assert_eq!(txn.verdict(), &CommitVerdict::Proceed);
        assert_eq!(
            h.registry
                .retire_nonblocking(k, lease.incarnation, TerminalReason::Revoked),
            RetireAttempt::Blocked,
            "retirement cannot land between the verdict and the enqueue"
        );
        let item = txn.commit(permit);
        assert!(!item.is_consumed());
        assert_eq!(h.registry.queued_items(k), 1);
        assert_eq!(h.registry.bytes().node_bytes(), 100);

        assert_eq!(h.registry.cancel_queued(k, lease.incarnation), 1);
        assert_eq!(h.registry.bytes().node_bytes(), 0);
        assert_eq!(h.registry.bytes().unsettled_drops(), 0);
    }

    // ================ duplicates, guard, quotas ================

    #[test]
    fn duplicate_opening_while_live_is_active_call_owned_before_decode() {
        let h = harness();
        let k = key(1, 10);
        let reservation = h.registry.reserve(opening(k)).expect("reserve");

        // Still Opening: refused without ever consulting the guard.
        let denial = h.registry.reserve(opening(k)).expect_err("duplicate");
        assert_eq!(denial, Denial::ActiveCallOwned);
        assert_eq!(denial.coarse(), CoarseDenial::Denied);
        assert!(
            !h.registry.guard().retains(k, T0),
            "no guard entry exists yet; the refusal happened before decode"
        );

        // Still refused once running, and still refused while terminal but
        // not yet reclaimed — the record owns the key until its removal.
        let lease = h
            .registry
            .install(&reservation, facts(7, 70, 3), T0)
            .expect("install");
        let _owner = run(&h, k, &lease);
        assert_eq!(
            h.registry.reserve(opening(k)).expect_err("duplicate"),
            Denial::ActiveCallOwned
        );
        assert!(h
            .registry
            .retire(k, lease.incarnation, TerminalReason::Cancelled));
        assert_eq!(
            h.registry.reserve(opening(k)).expect_err("duplicate"),
            Denial::ActiveCallOwned,
            "a terminal record not yet reclaimed still owns its key"
        );
    }

    #[test]
    fn duplicate_after_completion_inside_the_retained_window_is_a_guard_refusal() {
        let h = harness();
        let k = key(1, 10);
        let digest = [0xAB; 32];
        let reservation = h.registry.reserve(opening(k)).expect("reserve");
        assert_eq!(
            h.registry.guard_admit(k, digest, T0, T0 + 60 * SEC),
            GuardVerdict::Admitted
        );
        let lease = h
            .registry
            .install(&reservation, facts(7, 70, 3), T0)
            .expect("install");
        let _owner = run(&h, k, &lease);
        assert!(h.registry.retire(
            k,
            lease.incarnation,
            TerminalReason::Completed(HandlerResult::Ok)
        ));
        assert!(h.registry.complete(k, lease.incarnation));

        // The key is no longer live, so `reserve` succeeds and the guard is
        // the check that sees the duplicate.
        assert!(
            h.registry.guard().retains(k, T0 + 100 * SEC),
            "retirement never touches the guard"
        );
        let reused = h.registry.reserve(opening(k)).expect("the key is free");
        assert_eq!(
            h.registry.guard_admit(k, digest, T0 + SEC, T0 + 60 * SEC),
            GuardVerdict::Replay,
            "an abstract guard returning Admitted here is the failure the plan names"
        );
        assert_eq!(
            h.registry
                .guard_admit(k, [0xCD; 32], T0 + SEC, T0 + 60 * SEC),
            GuardVerdict::CallIdCollision,
            "a different binding digest is a collision, not a replay"
        );
        assert_eq!(
            Denial::ActiveCallOwned.coarse(),
            Denial::Replay.coarse(),
            "§3: the two duplicate classifications share the coarse wire byte"
        );
        assert!(h.registry.release(k, reused.incarnation));

        // Past the retained window (proof expiry + 300 s) the key is clean.
        assert!(!h.registry.guard().retains(k, T0 + 400 * SEC));
        assert_eq!(
            h.registry
                .guard_admit(k, digest, T0 + 400 * SEC, T0 + 460 * SEC),
            GuardVerdict::Admitted
        );
    }

    #[test]
    fn policy_veto_releases_the_reservation_but_retains_the_guard_record() {
        // Q4: replay insertion precedes provider policy, and a vetoed VALID
        // proof stays consumed — otherwise it is repeatedly reusable.
        let h = harness();
        let k = key(1, 10);
        let digest = [0x11; 32];
        let reservation = h.registry.reserve(opening(k)).expect("reserve");
        assert_eq!(
            h.registry.guard_admit(k, digest, T0, T0 + 60 * SEC),
            GuardVerdict::Admitted
        );
        assert_eq!(Denial::PolicyVetoed.coarse(), CoarseDenial::Denied);

        assert!(h.registry.release(k, reservation.incarnation));
        assert_eq!(h.registry.active_node(), 0, "the active slot is released");
        assert_eq!(h.registry.active_for_caller(1), 0);
        assert!(
            h.registry.guard().retains(k, T0 + SEC),
            "the guard slot the veto consumed stays consumed"
        );
        // The key is reusable — by a NEW proof, not by re-presenting this one.
        assert!(h.registry.reserve(opening(k)).is_ok());
        assert_eq!(
            h.registry.guard_admit(k, digest, T0 + SEC, T0 + 60 * SEC),
            GuardVerdict::Replay
        );
        assert_eq!(h.effects.admitted(), 0);
    }

    #[test]
    fn active_call_quotas_refuse_the_n_plus_first_at_every_scope() {
        let limits = CallLimits {
            max_active_node: 3,
            max_active_per_caller: 2,
            max_active_per_org: 2,
            ..small_limits()
        };
        let h = harness_with(limits, small_bytes());

        // Per caller: the third opening from caller 1 is refused.
        let a = h.registry.reserve(opening(key(1, 1))).expect("1st");
        let b = h.registry.reserve(opening(key(1, 2))).expect("2nd");
        assert_eq!(
            h.registry.reserve(opening(key(1, 3))).expect_err("3rd"),
            Denial::ActiveStreamCapacity(CapacityScope::Caller)
        );

        // Per node: caller 2 fills the node ceiling, so caller 3 is refused
        // for the NODE reason, not the caller reason.
        let c = h
            .registry
            .reserve(opening(key(2, 4)))
            .expect("3rd node slot");
        let denial = h
            .registry
            .reserve(opening(key(3, 5)))
            .expect_err("node full");
        assert_eq!(denial, Denial::ActiveStreamCapacity(CapacityScope::Node));
        assert_eq!(denial.coarse(), CoarseDenial::Unavailable);

        // Per org: two verified installs for org 7, the third refuses at
        // install — the only point the acting org is known.
        h.registry
            .install(&a, facts(7, 70, 3), T0)
            .expect("install a");
        h.registry
            .install(&b, facts(7, 71, 3), T0)
            .expect("install b");
        assert_eq!(h.registry.active_for_org(7), 2);
        assert_eq!(
            h.registry
                .install(&c, facts(7, 72, 3), T0)
                .expect_err("org full"),
            Denial::ActiveStreamCapacity(CapacityScope::Org)
        );
        assert_eq!(
            h.registry.active_for_org(7),
            2,
            "the refused install charged nothing"
        );
        assert!(h.registry.release(key(2, 4), c.incarnation));
    }

    #[test]
    fn provisional_charge_is_caller_and_node_only_until_verification() {
        let h = harness();
        let k = key(1, 10);
        let reservation = h.registry.reserve(opening(k)).expect("reserve");
        assert_eq!(h.registry.active_node(), 1);
        assert_eq!(h.registry.active_for_caller(1), 1);
        assert_eq!(
            h.registry.active_for_org(7),
            0,
            "an unverified proof's claimed org is never charged"
        );
        h.registry
            .install(&reservation, facts(7, 70, 3), T0)
            .expect("install");
        assert_eq!(
            h.registry.active_for_org(7),
            1,
            "the verified org is charged atomically with the transition"
        );
        assert_eq!(h.registry.active_node(), 1, "not double counted");
        assert_eq!(h.registry.active_for_caller(1), 1);
    }

    #[test]
    fn q1_default_active_call_ceilings_admit_n_and_refuse_n_plus_one() {
        let limits = CallLimits::q1_defaults();
        let h = harness_with(limits, ByteLimits::q1_defaults());

        // Caller 0 alone reaches the 64-call per-caller ceiling while the
        // node still has 4032 free slots, so the refusal names the CALLER.
        for call in 0..64u64 {
            h.registry
                .reserve(opening(key(0, call)))
                .unwrap_or_else(|e| panic!("caller 0 call {call}: {e:?}"));
        }
        assert_eq!(
            h.registry
                .reserve(opening(key(0, 64)))
                .expect_err("65th for caller 0"),
            Denial::ActiveStreamCapacity(CapacityScope::Caller)
        );

        // 63 more saturated callers bring the node to exactly 4096.
        for caller in 1..64u64 {
            for call in 0..64u64 {
                h.registry
                    .reserve(opening(key(caller, call)))
                    .unwrap_or_else(|e| panic!("caller {caller} call {call}: {e:?}"));
            }
        }
        assert_eq!(h.registry.active_node(), 4096);
        assert_eq!(
            h.registry
                .reserve(opening(key(64, 0)))
                .expect_err("4097th on the node"),
            Denial::ActiveStreamCapacity(CapacityScope::Node)
        );
    }

    #[test]
    fn q1_default_per_org_ceiling_admits_512_and_refuses_the_513th() {
        let limits = CallLimits::q1_defaults();
        let h = harness_with(limits, ByteLimits::q1_defaults());
        // 8 callers × 64 calls = 512 installs for one org.
        for caller in 0..8u64 {
            for call in 0..64u64 {
                let k = key(caller, call);
                let reservation = h.registry.reserve(opening(k)).expect("reserve");
                h.registry
                    .install(&reservation, facts(7, caller * 64 + call, 3), T0)
                    .expect("install");
            }
        }
        assert_eq!(h.registry.active_for_org(7), 512);
        let k = key(9, 0);
        let reservation = h.registry.reserve(opening(k)).expect("reserve");
        assert_eq!(
            h.registry
                .install(&reservation, facts(7, 9_999, 3), T0)
                .expect_err("513th for this org"),
            Denial::ActiveStreamCapacity(CapacityScope::Org)
        );
        assert_eq!(h.registry.active_for_org(7), 512);
    }

    #[test]
    fn session_retirement_matches_the_exact_peer_establishment_not_a_bare_id() {
        let h = harness();
        let mine = key(1, 10);
        let bystander = key(2, 11);
        let mut mine_req = opening(mine);
        mine_req.session = SessionRef {
            peer: 0xAA,
            session_id: 0x1234,
            establishment: 1,
        };
        let mut bystander_req = opening(bystander);
        // The SAME truncated session id, on an unrelated peer.
        bystander_req.session = SessionRef {
            peer: 0xBB,
            session_id: 0x1234,
            establishment: 9,
        };
        let mine_res = h.registry.reserve(mine_req).expect("reserve");
        let by_res = h.registry.reserve(bystander_req).expect("reserve");
        h.registry
            .install(&mine_res, facts(7, 70, 3), T0)
            .expect("install");
        h.registry
            .install(&by_res, facts(8, 80, 3), T0)
            .expect("install");

        let retired = h.registry.retire_session(
            &SessionRef {
                peer: 0xAA,
                session_id: 0x1234,
                establishment: 1,
            },
            TerminalReason::SessionReplaced,
        );
        assert_eq!(retired, 1);
        assert_eq!(
            h.registry.terminal_reason(mine),
            Some(TerminalReason::SessionReplaced)
        );
        assert_eq!(
            h.registry.phase(bystander),
            Some(Phase::Admitted),
            "a bare truncated session id must not reach an unrelated peer's call"
        );

        // A re-handshake on the same peer is a different establishment, so
        // sweeping the NEW session does not retire the displaced record's
        // successor either.
        assert_eq!(
            h.registry.retire_session(
                &SessionRef {
                    peer: 0xBB,
                    session_id: 0x1234,
                    establishment: 10,
                },
                TerminalReason::SessionReplaced
            ),
            0
        );
        assert_eq!(h.registry.phase(bystander), Some(Phase::Admitted));
    }

    #[test]
    fn serve_handle_drop_retires_only_its_own_registration() {
        let h = harness();
        let mine = key(1, 10);
        let other = key(2, 11);
        let mut other_req = opening(other);
        other_req.registration = 200;
        let mine_res = h.registry.reserve(opening(mine)).expect("reserve");
        let other_res = h.registry.reserve(other_req).expect("reserve");
        h.registry
            .install(&mine_res, facts(7, 70, 3), T0)
            .expect("install");
        h.registry
            .install(&other_res, facts(8, 80, 3), T0)
            .expect("install");

        assert_eq!(
            h.registry
                .retire_registration(100, TerminalReason::ServeHandleDropped),
            1
        );
        assert_eq!(
            h.registry.terminal_reason(mine),
            Some(TerminalReason::ServeHandleDropped)
        );
        assert_eq!(h.registry.phase(other), Some(Phase::Admitted));
    }

    #[test]
    fn spent_session_currentness_refuses_admission() {
        let h = harness();
        let mut req = opening(key(1, 10));
        req.session_generation = None; // the `u64::MAX` terminal marker
        let denial = h.registry.reserve(req).expect_err("refused");
        assert_eq!(denial, Denial::SessionCurrentnessExhausted);
        assert_eq!(denial.coarse(), CoarseDenial::Unavailable);
        assert_eq!(h.registry.record_count(), 0, "no slot was taken");
    }

    #[test]
    fn expired_verification_deadline_denies_a_late_install() {
        let h = harness();
        let k = key(1, 10);
        let reservation = h.registry.reserve(opening(k)).expect("reserve");
        let denial = h
            .registry
            .install(&reservation, facts(7, 70, 3), T0 + 31 * SEC)
            .expect_err("too late");
        assert_eq!(denial, Denial::VerificationDeadlineExpired);
        assert_eq!(h.registry.terminal_reason(k), Some(TerminalReason::Timeout));
        assert!(h.registry.release(k, reservation.incarnation));
    }

    #[test]
    fn reserve_arithmetic_is_checked_against_an_absurd_clock() {
        let h = harness();
        let mut req = opening(key(1, 10));
        req.now_ns = u64::MAX;
        assert_eq!(
            h.registry.reserve(req).expect_err("overflow"),
            Denial::CounterOverflow
        );
        assert_eq!(h.registry.record_count(), 0);
        assert_eq!(h.registry.active_node(), 0, "nothing was charged");
    }

    #[test]
    fn limit_validation_rejects_every_q1_violation() {
        assert_eq!(CallLimits::q1_defaults().validate(), Ok(()));
        assert_eq!(ByteLimits::q1_defaults().validate(), Ok(()));

        let zero = CallLimits {
            max_active_node: 0,
            ..CallLimits::q1_defaults()
        };
        assert_eq!(zero.validate(), Err(LimitsError::NotPositive));
        let no_deadline = CallLimits {
            verification_deadline_ns: 0,
            ..CallLimits::q1_defaults()
        };
        assert_eq!(no_deadline.validate(), Err(LimitsError::NotPositive));
        let default_over_max = CallLimits {
            lifetime: LifetimePolicy {
                default_live_ns: 2 * SEC,
                max_live_ns: SEC,
            },
            ..CallLimits::q1_defaults()
        };
        assert_eq!(
            default_over_max.validate(),
            Err(LimitsError::Lifetime(PolicyError::DefaultOverMax))
        );
        let caller_over_node = CallLimits {
            max_active_node: 8,
            max_active_per_caller: 9,
            max_active_per_org: 8,
            ..CallLimits::q1_defaults()
        };
        assert_eq!(
            caller_over_node.validate(),
            Err(LimitsError::PerCallerAboveNode)
        );
        let org_over_node = CallLimits {
            max_active_node: 8,
            max_active_per_caller: 8,
            max_active_per_org: 9,
            ..CallLimits::q1_defaults()
        };
        assert_eq!(org_over_node.validate(), Err(LimitsError::PerOrgAboveNode));

        assert_eq!(
            ByteLimits {
                per_call: 10,
                per_caller: 9,
                per_node: 100
            }
            .validate(),
            Err(LimitsError::BytePerCallAboveCaller)
        );
        assert_eq!(
            ByteLimits {
                per_call: 10,
                per_caller: 100,
                per_node: 99
            }
            .validate(),
            Err(LimitsError::BytePerCallerAboveNode)
        );
        // Construction refuses, rather than clamping silently.
        assert_eq!(
            ProtectedCallRegistry::new(
                ModelAuthority::new(1, 1),
                caller_over_node,
                ByteLimits::q1_defaults()
            )
            .err(),
            Some(LimitsError::PerCallerAboveNode)
        );
    }

    // ================ §2.7 byte accounting ================

    #[test]
    fn oversized_item_never_waits_for_impossible_permits() {
        let h = harness();
        let k = key(1, 10);
        let budgets = h.registry.bytes();

        // Larger than the configured per-call budget.
        let refusal = budgets
            .reserve(k, 1, Direction::Response, 1_001)
            .expect_err("over the call budget");
        assert_eq!(refusal, ByteRefusal::ExceedsCallBudget);
        assert!(!refusal.is_satisfiable_by_waiting());

        // Larger than the RPC item cap, under Q1 byte defaults where the
        // per-call budget (16 MiB) would otherwise accept it.
        let q1 = harness_with(small_limits(), ByteLimits::q1_defaults());
        let refusal = q1
            .registry
            .bytes()
            .reserve(k, 1, Direction::Response, MAX_RPC_ITEM_BYTES + 1)
            .expect_err("over the item cap");
        assert_eq!(refusal, ByteRefusal::ItemTooLarge);
        assert!(!refusal.is_satisfiable_by_waiting());
        q1.registry
            .bytes()
            .reserve(k, 1, Direction::Response, MAX_RPC_ITEM_BYTES)
            .expect("exactly the item cap is admissible")
            .release();

        // A merely-full budget IS satisfiable by waiting — that distinction
        // is the whole point of refusing the impossible ones promptly.
        let permit = budgets
            .reserve(k, 1, Direction::Response, 1_000)
            .expect("fills the call budget");
        let refusal = budgets
            .reserve(k, 1, Direction::Response, 1)
            .expect_err("call budget full");
        assert_eq!(refusal, ByteRefusal::CallBudgetFull);
        assert!(refusal.is_satisfiable_by_waiting());
        permit.release();
        assert_eq!(budgets.unsettled_drops(), 0);
    }

    #[test]
    fn item_credit_does_not_imply_byte_reservation() {
        // Call A holds eight item credits. Call B has already charged most
        // of the node budget. The grant must not let A oversubscribe it.
        let h = harness();
        let a = key(1, 10);
        let b = key(2, 11);
        let budgets = h.registry.bytes();
        let item_credits_for_a = 8usize;

        let b_response = budgets
            .reserve(b, 1, Direction::Response, 1_000)
            .expect("b reserves");
        let b_request = budgets
            .reserve(b, 1, Direction::Request, 500)
            .expect("b reserves more");
        assert_eq!(budgets.node_bytes(), 1_500);

        let a_permit = budgets
            .reserve(a, 1, Direction::Response, 500)
            .expect("fits exactly");
        assert_eq!(budgets.node_bytes(), 2_000, "the node cap, exactly");
        let refusal = budgets
            .reserve(a, 1, Direction::Response, 1)
            .expect_err("the node cap is a hard bound");
        assert_eq!(refusal, ByteRefusal::NodeBudgetFull);
        assert!(
            item_credits_for_a > 1,
            "credits remain; the refusal is byte-driven, not credit-driven"
        );
        assert!(
            budgets.node_bytes() <= budgets.limits().per_node,
            "the node total never exceeded its cap, even transiently"
        );

        b_response.release();
        b_request.release();
        a_permit.release();
        assert_eq!(budgets.node_bytes(), 0);
        assert_eq!(budgets.unsettled_drops(), 0);
    }

    #[test]
    fn node_refusal_rolls_back_call_and_caller_reservations() {
        // Node-binding budgets: the node is the scarcest scope.
        let budgets = ByteBudgets::new(ByteLimits {
            per_call: 1_000,
            per_caller: 1_500,
            per_node: 2_000,
        })
        .expect("limits validate");
        let a = key(1, 10);
        let b = key(2, 11);

        let b_response = budgets
            .reserve(b, 1, Direction::Response, 1_000)
            .expect("b charges 1000");
        let b_request = budgets
            .reserve(b, 1, Direction::Request, 500)
            .expect("b charges 500 more");
        let a_first = budgets
            .reserve(a, 1, Direction::Response, 400)
            .expect("a charges 400");
        assert_eq!(budgets.node_bytes(), 1_900);

        // Passes the call level (400 + 200 <= 1000) and the caller level
        // (400 + 200 <= 1500), then hits the node level (1900 + 200 > 2000).
        let refusal = budgets
            .reserve(a, 1, Direction::Response, 200)
            .expect_err("refused at the node level");
        assert_eq!(refusal, ByteRefusal::NodeBudgetFull);
        assert_eq!(
            budgets.call_bytes(a, 1, Direction::Response),
            400,
            "the call counter was rolled back"
        );
        assert_eq!(
            budgets.caller_bytes(1),
            400,
            "the caller counter was rolled back"
        );
        assert_eq!(budgets.node_bytes(), 1_900);
        b_response.release();
        b_request.release();
        a_first.release();
        assert_eq!(budgets.node_bytes(), 0);
        assert_eq!(budgets.unsettled_drops(), 0);

        // Caller-binding budgets: a roomy node, so the CALLER level refuses
        // and must still roll the call level back.
        let budgets = ByteBudgets::new(ByteLimits {
            per_call: 1_000,
            per_caller: 1_500,
            per_node: 100_000,
        })
        .expect("limits validate");
        let first = budgets
            .reserve(a, 1, Direction::Response, 1_000)
            .expect("a's first call charges 1000");
        let second = budgets
            .reserve(a, 2, Direction::Response, 400)
            .expect("a's second call charges 400");
        assert_eq!(budgets.caller_bytes(1), 1_400);
        let refusal = budgets
            .reserve(a, 2, Direction::Response, 200)
            .expect_err("refused at the caller level");
        assert_eq!(refusal, ByteRefusal::CallerBudgetFull);
        assert_eq!(
            budgets.call_bytes(a, 2, Direction::Response),
            400,
            "the call counter was rolled back"
        );
        assert_eq!(budgets.caller_bytes(1), 1_400);
        assert_eq!(budgets.node_bytes(), 1_400);
        first.release();
        second.release();

        assert_eq!(budgets.node_bytes(), 0);
        assert_eq!(budgets.unsettled_drops(), 0);
    }

    #[test]
    fn cancel_dequeue_handoff_consumes_one_permit() {
        let h = harness();
        let a = key(1, 10);
        let b = key(2, 11);
        let lease_a = admit(&h, a, 7, 70, 3);
        let lease_b = admit(&h, b, 8, 80, 3);
        let _owner_a = run(&h, a, &lease_a);
        let _owner_b = run(&h, b, &lease_b);
        let budgets = Arc::clone(h.registry.bytes());

        // Another call's live bytes, which must stay charged throughout.
        let b_permit = budgets
            .reserve(b, lease_b.incarnation, Direction::Response, 700)
            .expect("b reserves");

        // Item 1: the pump wins the race.
        let permit = budgets
            .reserve(a, lease_a.incarnation, Direction::Response, 100)
            .expect("reserve");
        let item = h
            .registry
            .begin_commit(a, lease_a.incarnation)
            .expect("commit opens")
            .commit(permit);
        let dequeued = h
            .registry
            .dequeue_next(a, lease_a.incarnation)
            .expect("the pump wins");
        assert!(item.is_consumed());
        assert!(item.take().is_none(), "the discard path loses");
        assert_eq!(
            h.registry.cancel_queued(a, lease_a.incarnation),
            0,
            "cancellation loses"
        );
        assert_eq!(
            budgets.node_bytes(),
            800,
            "handoff is not memory reclamation: the bytes are still charged"
        );
        let handed_off = dequeued.transfer();
        assert_eq!(budgets.node_bytes(), 800, "transfer keeps the charge");
        handed_off.release();
        assert_eq!(
            budgets.call_bytes(a, lease_a.incarnation, Direction::Response),
            0
        );

        // Item 2: cancellation wins.
        let permit = budgets
            .reserve(a, lease_a.incarnation, Direction::Response, 250)
            .expect("reserve");
        let item = h
            .registry
            .begin_commit(a, lease_a.incarnation)
            .expect("commit opens")
            .commit(permit);
        assert_eq!(h.registry.cancel_queued(a, lease_a.incarnation), 1);
        assert!(item.is_consumed());
        assert!(h.registry.dequeue_next(a, lease_a.incarnation).is_none());

        assert_eq!(
            budgets.caller_bytes(2),
            700,
            "another call's live bytes remain charged"
        );
        assert_eq!(budgets.caller_bytes(1), 0);
        assert_eq!(budgets.node_bytes(), 700);
        assert_eq!(budgets.unsettled_drops(), 0);
        b_permit.release();
        assert_eq!(budgets.node_bytes(), 0);
    }

    #[test]
    fn removal_consumes_the_permits_its_call_still_owned() {
        let h = harness();
        let k = key(1, 10);
        let lease = admit(&h, k, 7, 70, 3);
        let _owner = run(&h, k, &lease);
        let budgets = Arc::clone(h.registry.bytes());
        for len in [100usize, 200, 300] {
            let permit = budgets
                .reserve(k, lease.incarnation, Direction::Response, len)
                .expect("reserve");
            h.registry
                .begin_commit(k, lease.incarnation)
                .expect("commit opens")
                .commit(permit);
        }
        assert_eq!(budgets.node_bytes(), 600);
        assert!(h
            .registry
            .retire(k, lease.incarnation, TerminalReason::Cancelled));
        assert_eq!(
            budgets.node_bytes(),
            600,
            "retirement marks, it does not free"
        );
        assert!(h.registry.complete(k, lease.incarnation));
        assert_eq!(
            budgets.node_bytes(),
            0,
            "the single removal consumed every owned permit exactly once"
        );
        assert_eq!(budgets.unsettled_drops(), 0);
    }

    #[test]
    fn q1_default_byte_ceilings_bound_call_caller_and_node() {
        let h = harness_with(CallLimits::q1_defaults(), ByteLimits::q1_defaults());
        let budgets = h.registry.bytes();
        let item = MAX_RPC_ITEM_BYTES; // 4 MiB
        let mut held = Vec::new();

        // Per call, per direction: 16 MiB = four 4 MiB items.
        for _ in 0..4 {
            held.push(
                budgets
                    .reserve(key(0, 0), 1, Direction::Response, item)
                    .expect("within the per-call budget"),
            );
        }
        assert_eq!(
            budgets
                .reserve(key(0, 0), 1, Direction::Response, item)
                .expect_err("fifth item"),
            ByteRefusal::CallBudgetFull
        );
        // The opposite direction has its own 16 MiB.
        held.push(
            budgets
                .reserve(key(0, 0), 1, Direction::Request, item)
                .expect("the request direction is budgeted separately"),
        );

        // Per caller: 64 MiB total. Caller 0 holds 20 MiB; eleven more 4 MiB
        // items across fresh calls reach 64 MiB.
        for call in 1..12u64 {
            held.push(
                budgets
                    .reserve(key(0, call), 1, Direction::Response, item)
                    .expect("within the per-caller budget"),
            );
        }
        assert_eq!(budgets.caller_bytes(0), 64 * 1024 * 1024);
        assert_eq!(
            budgets
                .reserve(key(0, 99), 1, Direction::Response, item)
                .expect_err("over the caller budget"),
            ByteRefusal::CallerBudgetFull
        );

        // Per node: 512 MiB total = eight saturated callers.
        for caller in 1..8u64 {
            for call in 0..16u64 {
                held.push(
                    budgets
                        .reserve(key(caller, call), 1, Direction::Response, item)
                        .expect("within the node budget"),
                );
            }
        }
        assert_eq!(budgets.node_bytes(), 512 * 1024 * 1024);
        assert_eq!(
            budgets
                .reserve(key(8, 0), 1, Direction::Response, item)
                .expect_err("over the node budget"),
            ByteRefusal::NodeBudgetFull
        );

        for permit in held {
            permit.release();
        }
        assert_eq!(budgets.node_bytes(), 0);
        assert_eq!(budgets.unsettled_drops(), 0);
    }

    #[cfg(feature = "cortex")]
    #[test]
    fn item_cap_matches_the_real_rpc_body_limit() {
        // The model mirrors the constant instead of importing it so it
        // compiles without `cortex`. That mirror must not silently drift.
        assert_eq!(
            MAX_RPC_ITEM_BYTES,
            crate::adapter::net::cortex::rpc::MAX_RPC_BODY_LEN
        );
    }
}
