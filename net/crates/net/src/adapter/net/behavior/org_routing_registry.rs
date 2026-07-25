//! OLB-2B-E3b: the bounded node routing registry — the actor's real consumer.
//!
//! Retains one node slot per AUTHORITY-SCOPED key, shared by every clone family
//! demanding that same key, and rebuilds slots from the private-discovery source
//! in bounded quanta. It implements [`DirtyApply`], so the supervised actor from
//! E2 drives it and nothing else does.
//!
//! Three authoritative structures, deliberately separate:
//!
//! - [`RegistryWork`] answers only "is SOME work owed?" — a coalescing wake hint
//!   (Kyra OLB-2B-E3a). The Boolean must never become the work queue;
//! - [`RegistryInner::pending`] holds the exact slot identities that owe work;
//! - [`RegistryInner::live_actor`] holds the ONE incarnation currently permitted
//!   to consume that work. It is bound to the actor LIFECYCLE via
//!   [`DirtyApply::activate_incarnation`] / [`DirtyApply::deactivate_incarnation`],
//!   never to a high-water counter: a high-water mark cannot distinguish "a newer
//!   actor took over" from "no actor is live", so a stale attempt could consume
//!   pending identities, be rejected at installation, and still report success.
//!
//! ## Frozen lock order
//!
//! `source commit pin` → `registry lock`.
//!
//! A quantum runs as: select/invalidate under the registry lock → release → brief
//! source snapshot → reconstruct entirely off-lock → acquire the commit pin →
//! registry lock beneath it → install → release both. No source method is called
//! while the registry lock is held; no source-side lock is held across decoding,
//! sorting or projection; and no registry method a source implementation could
//! re-enter takes the commit pin.

// E3b-ONLY: the node wiring that consumes this lands in E3c, which also removes
// this allow and those in `org_routing`/`org_scoped_store`. A leftover allow there
// means the real consumer never landed.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use super::org_grant::CapabilityAuthorityId;
use super::org_routing::{ApplyOutcome, ApplyRequest, DirtyApply, RegistryWork};
use super::org_scoped_ingest::CapabilityAudienceScope;
use super::org_scoped_store::{DirtyCapabilities, PrivateCapabilityProvider};

/// Max demand handles ONE clone family may hold.
const MAX_HANDLES_PER_FAMILY: usize = 64;
/// Max distinct retained node slots.
const MAX_NODE_SLOTS: usize = 256;
/// Max slots rebuilt in ONE synchronous application. Work beyond this stays
/// authoritative in `pending` and re-marks, so a 256-slot rebuild becomes several
/// yielding quanta rather than one unbroken burst.
const APPLY_QUANTUM: usize = 64;

/// A clone family's identity.
///
/// Deliberately UNCONSTRUCTIBLE outside this module: it is minted only by
/// [`NodeOrgRoutingRegistry::new_family`] and reaches callers wrapped in a
/// [`RoutingFamily`]. A caller cannot name another family's id, so it cannot spend
/// another family's handle budget or forge membership in one (Kyra OLB-2B-E3b).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct FamilyId(u64);

/// An audience scope in the PRIVATE partition.
///
/// `CapabilityAudienceScope::Public` is unrepresentable here: this registry is the
/// owner-private routing consumer, and a public scope reaching a retained slot
/// would mean the private plane retained a plaintext, globally-discoverable row
/// under private-authority facts. Rejecting it at construction makes that
/// structurally impossible rather than a filter someone can forget.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct PrivateAudienceScope(CapabilityAudienceScope);

impl PrivateAudienceScope {
    /// `None` for `Public` — the only rejection.
    pub(crate) fn new(scope: CapabilityAudienceScope) -> Option<Self> {
        match scope {
            CapabilityAudienceScope::Public => None,
            private => Some(Self(private)),
        }
    }

    pub(crate) fn scope(&self) -> &CapabilityAudienceScope {
        &self.0
    }
}

/// The AUTHORITY-SCOPED identity of a retained slot.
///
/// The audience scope is part of the key, never elided: two demands under
/// different acting identities/audiences are different slots even for the same
/// capability. Sharing a slot across scopes would let indexing or reuse broaden
/// authority, which no amount of downstream filtering can undo.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SlotKey {
    pub scope: PrivateAudienceScope,
    pub capability: CapabilityAuthorityId,
}

/// Why demand was refused. Deterministic and state-free: a refused demand retains
/// nothing, so the caller takes the current-authority cold path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DemandRefused {
    /// This family already holds `MAX_HANDLES_PER_FAMILY` handles.
    FamilyAtCapacity,
    /// The node already retains `MAX_NODE_SLOTS` distinct slots. A live slot is
    /// NEVER evicted to satisfy new demand.
    NodeAtCapacity,
    /// The monotone identity space backing slot incarnations and family identities
    /// is exhausted. Refusing is the only safe answer: wrapping would let a stale
    /// artifact match a live slot's incarnation and resurrect it, and aborting
    /// would take the process down over a bookkeeping limit.
    IdSpaceExhausted,
}

/// The authority-scoped base facts for one retained slot.
///
/// Stamped with all three identities, because health alone is never
/// source-currentness: the actor incarnation that built it, the slot incarnation
/// it was built for, and the source generation it was built from.
#[derive(Clone, Debug)]
pub(crate) struct SlotBaseFacts {
    pub key: SlotKey,
    pub providers: Arc<[PrivateCapabilityProvider]>,
    pub source_generation: u64,
    pub actor_incarnation: u64,
    pub slot_incarnation: u64,
}

/// Bounded material captured from the source for ONE quantum.
///
/// Owns everything reconstruction needs, so decode, sort and projection run with
/// NO source lock and NO registry lock held. That is the whole point of the split:
/// the source-side capture is brief, and the expensive part of a quantum blocks
/// nothing (Kyra OLB-2B-E3b).
pub(crate) trait SourceSnapshot {
    /// The query-visible generation this material was captured at.
    fn generation(&self) -> u64;

    /// Reconstruct `key`'s authority-scoped providers from the captured material.
    fn providers(&self, key: &SlotKey) -> Vec<PrivateCapabilityProvider>;
}

/// A SHORT currentness pin, held only across final validation and installation.
///
/// Its existence proves the source has not moved since the snapshot, which is what
/// makes a conditional install safe without a two-sample before/after comparison —
/// that comparison leaves a window between the second read and the installation in
/// which the source may move, and facts installed in that window are stale while
/// carrying a generation that says otherwise.
pub(crate) trait SourceCommitPin {
    /// The generation this pin proves is still current.
    fn generation(&self) -> u64;
}

/// Supplies authority-scoped provider facts.
///
/// E3c binds this to the scoped discovery state and the mutation-publication gate.
/// The two-method shape is deliberate: it makes "the gate is a COMMIT pin, never a
/// reconstruction lock" a property of the seam rather than a rule an implementation
/// has to remember. Holding the publication gate across a quantum would block all
/// private-discovery ingest, expiry sweeps and floor mutation behind up to
/// [`APPLY_QUANTUM`] authority-scoped store queries plus their decoding.
pub(crate) trait SlotSource: Send + Sync + 'static {
    /// Briefly capture the exact authority-scoped material for `keys`, plus the
    /// generation it was captured at. Every source-side lock is released before
    /// this returns. Called with NO registry lock held.
    fn snapshot(&self, keys: &[SlotKey]) -> Box<dyn SourceSnapshot>;

    /// Acquire the commit pin IF the source is still at `expected_generation`.
    ///
    /// `None` means the source moved while this quantum was rebuilding, and the
    /// caller must install nothing. ALWAYS called before the registry lock is
    /// taken, never while holding it — see the module's frozen lock order.
    fn pin_if_current(&self, expected_generation: u64) -> Option<Box<dyn SourceCommitPin + '_>>;
}

#[derive(Debug)]
struct Slot {
    /// Allocated fresh from the node-wide monotone id space at creation, so a
    /// retired-and-re-demanded slot never reuses an identity and work in flight
    /// for a previous incarnation can never resurrect it.
    incarnation: u64,
    /// Live demand handles across ALL families.
    refs: usize,
    /// `None` until a recapture installs facts, and cleared the moment anything
    /// invalidates them. `None` IS the deterministic cold outcome.
    facts: Option<Arc<SlotBaseFacts>>,
}

#[derive(Default)]
struct RegistryInner {
    slots: BTreeMap<SlotKey, Slot>,
    /// Capability → retained slot identities, so `Caps(C)` touches only C's
    /// buckets. One capability can back several slots (one per audience scope).
    slots_by_capability: BTreeMap<CapabilityAuthorityId, BTreeSet<SlotKey>>,
    /// Per family, its handle count per key. Bounds HANDLES, not distinct keys, so
    /// duplicate demand cannot bypass the bound.
    families: BTreeMap<FamilyId, BTreeMap<SlotKey, usize>>,
    /// AUTHORITATIVE pending slot identities — the work queue proper.
    pending: BTreeSet<SlotKey>,
    /// Monotone allocator shared by slot incarnations and family identities. Never
    /// reused, never wrapped.
    next_id: u64,
    /// The ONE actor incarnation permitted to consume pending work and install
    /// facts, or `None` when no actor is live.
    live_actor: Option<u64>,
    /// Whether a COMPLETE recapture is under way.
    ///
    /// While open, its remaining set is `pending`, and settlement additionally
    /// re-queues every retained slot that is not coherent with the commit — which
    /// is how a source generation that moved between quanta restarts the epoch. A
    /// later `RebuildAll` must NOT re-expand while it is open: re-expanding would
    /// re-select the first quantum forever and the recapture would never terminate.
    recapture_open: bool,
}

impl RegistryInner {
    fn family_handles(&self, family: FamilyId) -> usize {
        self.families
            .get(&family)
            .map_or(0, |keys| keys.values().sum())
    }

    /// Allocate the next identity, or `None` when the space is exhausted.
    fn allocate_id(&mut self) -> Option<u64> {
        let next = self.next_id.checked_add(1)?;
        self.next_id = next;
        Some(next)
    }

    /// Drop `key`'s facts, reporting whether anything was actually invalidated.
    fn invalidate(&mut self, key: &SlotKey) -> bool {
        self.slots
            .get_mut(key)
            .is_some_and(|slot| slot.facts.take().is_some())
    }

    /// Queue every retained slot and drop every fact — the expansion step of a
    /// recapture epoch and of a fresh actor incarnation.
    fn invalidate_and_queue_all(&mut self) -> u64 {
        let keys: Vec<SlotKey> = self.slots.keys().cloned().collect();
        let mut invalidated = 0;
        for key in keys {
            if self.invalidate(&key) {
                invalidated += 1;
            }
            self.pending.insert(key);
        }
        invalidated
    }

    /// Retained slots NOT coherent with this commit.
    ///
    /// Coherent means: facts present, stamped by the live actor incarnation, for
    /// the slot's current incarnation, from this source generation. Used ONLY
    /// while a complete-recapture epoch is open — ordinary `Caps` and first-demand
    /// work deliberately leaves unrelated slots at older generations, and demanding
    /// global coherence there would report `Progress` for a pass that finished
    /// everything it was asked to do.
    fn incoherent_with(&self, incarnation: u64, generation: u64) -> Vec<SlotKey> {
        self.slots
            .iter()
            .filter(|(_, slot)| {
                !slot.facts.as_ref().is_some_and(|facts| {
                    facts.actor_incarnation == incarnation
                        && facts.slot_incarnation == slot.incarnation
                        && facts.source_generation == generation
                })
            })
            .map(|(key, _)| key.clone())
            .collect()
    }
}

/// Observable registry counters.
#[derive(Default)]
pub(crate) struct RegistryMetrics {
    refused_family_at_capacity: AtomicU64,
    refused_node_at_capacity: AtomicU64,
    refused_id_space_exhausted: AtomicU64,
    slots_retired: AtomicU64,
    installs: AtomicU64,
    discarded_obsolete: AtomicU64,
    facts_invalidated: AtomicU64,
    stale_actor_rejections: AtomicU64,
    recaptures_restarted: AtomicU64,
}

impl RegistryMetrics {
    pub(crate) fn refused_family_at_capacity(&self) -> u64 {
        self.refused_family_at_capacity.load(Ordering::Acquire)
    }
    pub(crate) fn refused_node_at_capacity(&self) -> u64 {
        self.refused_node_at_capacity.load(Ordering::Acquire)
    }
    pub(crate) fn refused_id_space_exhausted(&self) -> u64 {
        self.refused_id_space_exhausted.load(Ordering::Acquire)
    }
    pub(crate) fn slots_retired(&self) -> u64 {
        self.slots_retired.load(Ordering::Acquire)
    }
    pub(crate) fn installs(&self) -> u64 {
        self.installs.load(Ordering::Acquire)
    }
    pub(crate) fn discarded_obsolete(&self) -> u64 {
        self.discarded_obsolete.load(Ordering::Acquire)
    }
    pub(crate) fn facts_invalidated(&self) -> u64 {
        self.facts_invalidated.load(Ordering::Acquire)
    }
    pub(crate) fn stale_actor_rejections(&self) -> u64 {
        self.stale_actor_rejections.load(Ordering::Acquire)
    }
    pub(crate) fn recaptures_restarted(&self) -> u64 {
        self.recaptures_restarted.load(Ordering::Acquire)
    }
}

/// The node's bounded routing registry.
pub(crate) struct NodeOrgRoutingRegistry {
    inner: parking_lot::Mutex<RegistryInner>,
    source: Arc<dyn SlotSource>,
    work: Arc<RegistryWork>,
    metrics: Arc<RegistryMetrics>,
}

/// A clone family: one private identity and one shared 64-handle budget.
///
/// Cloning shares the identity — that is the point, and it is why the identity is
/// not a caller-supplied integer. Independent families are minted by
/// [`NodeOrgRoutingRegistry::new_family`] and can neither name nor spend one
/// another's budget.
#[derive(Clone)]
pub(crate) struct RoutingFamily {
    registry: Arc<NodeOrgRoutingRegistry>,
    id: FamilyId,
}

impl RoutingFamily {
    /// Register demand for `key` under THIS family's budget.
    pub(crate) fn demand(&self, key: SlotKey) -> Result<DemandHandle, DemandRefused> {
        self.registry.demand(self.id, key)
    }

    /// Handles this family currently holds.
    pub(crate) fn handles(&self) -> usize {
        self.registry.inner.lock().family_handles(self.id)
    }
}

/// A live demand for one authority-scoped key, held by one family. Releases on
/// drop; the LAST reference retires the slot.
pub(crate) struct DemandHandle {
    registry: Arc<NodeOrgRoutingRegistry>,
    family: FamilyId,
    key: SlotKey,
}

impl Drop for DemandHandle {
    fn drop(&mut self) {
        self.registry.release(self.family, &self.key);
    }
}

impl NodeOrgRoutingRegistry {
    pub(crate) fn new(
        source: Arc<dyn SlotSource>,
        work: Arc<RegistryWork>,
        metrics: Arc<RegistryMetrics>,
    ) -> Arc<Self> {
        Arc::new(Self {
            inner: parking_lot::Mutex::new(RegistryInner::default()),
            source,
            work,
            metrics,
        })
    }

    /// Mint a new clone family with its own private identity and handle budget.
    pub(crate) fn new_family(self: &Arc<Self>) -> Result<RoutingFamily, DemandRefused> {
        let id = {
            let mut inner = self.inner.lock();
            inner.allocate_id()
        };
        match id {
            Some(id) => Ok(RoutingFamily {
                registry: self.clone(),
                id: FamilyId(id),
            }),
            None => {
                self.metrics
                    .refused_id_space_exhausted
                    .fetch_add(1, Ordering::AcqRel);
                Err(DemandRefused::IdSpaceExhausted)
            }
        }
    }

    /// Register demand for `key` from `family`.
    ///
    /// FIRST demand for a key creates the slot and queues an exact FULL recapture
    /// of THAT slot — never a node-wide rebuild, and never contingent on whether
    /// older source deltas were already destructively drained. A later family
    /// demanding the same key SHARES the one node slot.
    ///
    /// A duplicate demand from the same family is a distinct handle: it shares the
    /// slot and counts toward the family bound, so re-demanding cannot bypass it.
    fn demand(
        self: &Arc<Self>,
        family: FamilyId,
        key: SlotKey,
    ) -> Result<DemandHandle, DemandRefused> {
        let mut queued = false;
        {
            let mut inner = self.inner.lock();
            if inner.family_handles(family) >= MAX_HANDLES_PER_FAMILY {
                self.metrics
                    .refused_family_at_capacity
                    .fetch_add(1, Ordering::AcqRel);
                return Err(DemandRefused::FamilyAtCapacity);
            }
            let new_slot = !inner.slots.contains_key(&key);
            if new_slot && inner.slots.len() >= MAX_NODE_SLOTS {
                // Never evict a live retained slot to satisfy new demand.
                self.metrics
                    .refused_node_at_capacity
                    .fetch_add(1, Ordering::AcqRel);
                return Err(DemandRefused::NodeAtCapacity);
            }
            if new_slot {
                // Allocate BEFORE mutating anything, so exhaustion retains nothing.
                let Some(incarnation) = inner.allocate_id() else {
                    self.metrics
                        .refused_id_space_exhausted
                        .fetch_add(1, Ordering::AcqRel);
                    return Err(DemandRefused::IdSpaceExhausted);
                };
                inner.slots.insert(
                    key.clone(),
                    Slot {
                        incarnation,
                        refs: 0,
                        facts: None,
                    },
                );
                inner
                    .slots_by_capability
                    .entry(key.capability)
                    .or_default()
                    .insert(key.clone());
                inner.pending.insert(key.clone());
                queued = true;
            }
            if let Some(slot) = inner.slots.get_mut(&key) {
                slot.refs += 1;
            }
            *inner
                .families
                .entry(family)
                .or_default()
                .entry(key.clone())
                .or_insert(0) += 1;
        }
        if queued {
            // Private discovery did not move, so this is the ONLY thing that will
            // wake the actor for it.
            self.work.mark();
        }
        Ok(DemandHandle {
            registry: self.clone(),
            family,
            key,
        })
    }

    /// Release one handle. The LAST reference retires the slot; a re-demand then
    /// allocates a FRESH incarnation, so work in flight cannot resurrect it.
    fn release(&self, family: FamilyId, key: &SlotKey) {
        let mut inner = self.inner.lock();
        if let Some(keys) = inner.families.get_mut(&family) {
            if let Some(count) = keys.get_mut(key) {
                *count -= 1;
                if *count == 0 {
                    keys.remove(key);
                }
            }
            if keys.is_empty() {
                inner.families.remove(&family);
            }
        }
        let retire = match inner.slots.get_mut(key) {
            Some(slot) => {
                slot.refs -= 1;
                slot.refs == 0
            }
            None => false,
        };
        if retire {
            inner.slots.remove(key);
            inner.pending.remove(key);
            if let Some(bucket) = inner.slots_by_capability.get_mut(&key.capability) {
                bucket.remove(key);
                if bucket.is_empty() {
                    inner.slots_by_capability.remove(&key.capability);
                }
            }
            self.metrics.slots_retired.fetch_add(1, Ordering::AcqRel);
        }
    }

    /// The retained base facts for `key`, if the slot exists AND currently holds
    /// valid facts. `None` is the deterministic UNRETAINED/cold outcome.
    pub(crate) fn base_facts(&self, key: &SlotKey) -> Option<Arc<SlotBaseFacts>> {
        self.inner.lock().slots.get(key)?.facts.clone()
    }

    pub(crate) fn retained_slots(&self) -> usize {
        self.inner.lock().slots.len()
    }

    pub(crate) fn pending_slots(&self) -> usize {
        self.inner.lock().pending.len()
    }
}

impl DirtyApply for NodeOrgRoutingRegistry {
    /// A fresh incarnation claims work authority and invalidates EVERYTHING.
    ///
    /// Nothing a dead incarnation built is trustworthy, so rather than leaving
    /// stale facts readable until the first recapture lands, this drops them and
    /// queues every retained slot. `SlotBaseFacts::actor_incarnation` therefore
    /// records which live actor built a fact rather than merely enabling a check
    /// for one that no longer is.
    fn activate_incarnation(&self, incarnation: u64) {
        let owed = {
            let mut inner = self.inner.lock();
            inner.live_actor = Some(incarnation);
            inner.recapture_open = false;
            let invalidated = inner.invalidate_and_queue_all();
            self.metrics
                .facts_invalidated
                .fetch_add(invalidated, Ordering::AcqRel);
            !inner.pending.is_empty()
        };
        if owed {
            self.work.mark();
        }
    }

    /// This incarnation is over. Revoking authority here — from the actor's own
    /// fence — is what makes "stale actor" a state rather than a comparison
    /// against a high-water mark.
    fn deactivate_incarnation(&self, incarnation: u64) {
        let mut inner = self.inner.lock();
        if inner.live_actor == Some(incarnation) {
            inner.live_actor = None;
        }
    }

    /// One bounded reconciliation quantum.
    ///
    /// The phases exist for the lock discipline. The registry lock is held ONLY to
    /// select/invalidate and later to install. The source is touched twice, both
    /// times briefly: once to snapshot the material for this quantum, and once to
    /// pin currentness across the installation. Decoding, sorting and projection
    /// happen between those, holding NOTHING.
    fn apply(&self, incarnation: u64, request: ApplyRequest) -> ApplyOutcome {
        // --- phase 1: invalidate + select under the lock, bounded to one quantum ---
        let selected: Vec<(SlotKey, u64)> = {
            let mut inner = self.inner.lock();

            if inner.live_actor != Some(incarnation) {
                // Stale or absent authority. Consume NOTHING: taking pending
                // identities here and then failing at installation is exactly how
                // authoritative work gets lost while the caller is told it
                // succeeded (Kyra OLB-2B-E3b).
                self.metrics
                    .stale_actor_rejections
                    .fetch_add(1, Ordering::AcqRel);
                drop(inner);
                // Re-arm so the LIVE incarnation still sees the work that woke us.
                self.work.mark();
                return ApplyOutcome::Superseded;
            }

            // Slots this request names, invalidated under THIS lock — before any
            // rebuild starts and including slots beyond the quantum. Leaving stale
            // facts readable while their capability is known to have moved is a
            // silent-staleness hazard that no later install can undo (Kyra
            // OLB-2B-E3b): `Caps` deliberately leaves global health alone, so
            // per-slot invalidation is the ONLY thing fencing those routes.
            let mut invalidated = 0u64;
            let mut named: BTreeSet<SlotKey> = BTreeSet::new();
            match &request.batch.dirty {
                DirtyCapabilities::RebuildAll => {
                    if !inner.recapture_open {
                        inner.recapture_open = true;
                        invalidated += inner.invalidate_and_queue_all();
                    }
                    // An already-open epoch keeps its remaining set in `pending`;
                    // re-expanding would re-select the first quantum forever. A
                    // source generation that moves mid-epoch restarts it from
                    // `settle`, where the COMMITTED generation is known.
                    named = inner.pending.clone();
                }
                DirtyCapabilities::Caps(caps) => {
                    let affected: Vec<SlotKey> = caps
                        .iter()
                        .filter_map(|capability| inner.slots_by_capability.get(capability))
                        .flat_map(|bucket| bucket.iter().cloned())
                        .collect();
                    for key in affected {
                        if inner.invalidate(&key) {
                            invalidated += 1;
                        }
                        inner.pending.insert(key.clone());
                        named.insert(key);
                    }
                }
                DirtyCapabilities::Clean => {}
            }
            if request.registry_work {
                named.extend(inner.pending.iter().cloned());
            }
            self.metrics
                .facts_invalidated
                .fetch_add(invalidated, Ordering::AcqRel);

            let mut selected = Vec::with_capacity(named.len().min(APPLY_QUANTUM));
            for key in named.into_iter().take(APPLY_QUANTUM) {
                // Consuming the identity is safe now: authority was verified under
                // THIS lock, and the install phase re-queues anything it cannot
                // install.
                inner.pending.remove(&key);
                if let Some(slot) = inner.slots.get(&key) {
                    selected.push((key, slot.incarnation));
                }
            }
            // Everything beyond the quantum stays AUTHORITATIVE in `pending`, not
            // in the wake flag.
            selected
        };

        // A pass with nothing to build still has to settle — an epoch may have
        // become complete, or may owe a coherence re-queue. It takes the same
        // commit pin, so `Current` is never reported over an unverified generation.
        if selected.is_empty() {
            let generation = self.source.snapshot(&[]).generation();
            let Some(commit) = self.source.pin_if_current(generation) else {
                return ApplyOutcome::Superseded;
            };
            let generation = commit.generation();
            let mut inner = self.inner.lock();
            let outcome = settle(&mut inner, incarnation, generation, &self.metrics);
            let owed = !inner.pending.is_empty();
            drop(inner);
            if owed {
                self.work.mark();
            }
            return outcome;
        }

        // --- phase 2: BRIEF source capture, no registry lock held ---
        let keys: Vec<SlotKey> = selected.iter().map(|(key, _)| key.clone()).collect();
        let snapshot = self.source.snapshot(&keys);
        let snapshot_generation = snapshot.generation();

        // --- phase 3: reconstruct holding NOTHING ---
        // Not the registry lock, and not the source's publication gate: this is the
        // expensive part of a quantum, and blocking private-discovery ingest,
        // expiry and floor mutation behind it is exactly what the snapshot/commit
        // split exists to prevent (Kyra OLB-2B-E3b).
        let built: Vec<(SlotKey, u64, Vec<PrivateCapabilityProvider>)> = selected
            .iter()
            .map(|(key, slot_incarnation)| {
                (key.clone(), *slot_incarnation, snapshot.providers(key))
            })
            .collect();
        drop(snapshot);

        // --- phase 4: the COMMIT pin, before the registry lock ---
        let Some(commit) = self.source.pin_if_current(snapshot_generation) else {
            // The source moved while we rebuilt. Install nothing, and put every
            // still-live selected slot back.
            let mut inner = self.inner.lock();
            for (key, slot_incarnation, _) in &built {
                if inner
                    .slots
                    .get(key)
                    .is_some_and(|slot| slot.incarnation == *slot_incarnation)
                {
                    inner.pending.insert(key.clone());
                }
            }
            self.metrics
                .discarded_obsolete
                .fetch_add(built.len() as u64, Ordering::AcqRel);
            drop(inner);
            // The movement itself owns a source-watch wake, but that wake alone
            // carries no `registry_work` flag — and without it the requeued
            // identities would not be unioned into the next pass's targets. Mark.
            self.work.mark();
            return ApplyOutcome::Superseded;
        };
        let generation = commit.generation();

        // --- phase 5: registry lock BENEATH the commit pin; install ---
        // The source cannot move while `commit` is held. What can still have
        // changed is the actor's authority and the slots themselves.
        let mut slot_moved = false;
        let outcome = {
            let mut inner = self.inner.lock();

            if inner.live_actor != Some(incarnation) {
                // Authority was revoked while we built. Every still-live slot we
                // selected still owes work — requeue it rather than dropping it,
                // and never report a current installation.
                for (key, slot_incarnation, _) in &built {
                    if inner
                        .slots
                        .get(key)
                        .is_some_and(|slot| slot.incarnation == *slot_incarnation)
                    {
                        inner.pending.insert(key.clone());
                    }
                }
                self.metrics
                    .stale_actor_rejections
                    .fetch_add(1, Ordering::AcqRel);
                self.metrics
                    .discarded_obsolete
                    .fetch_add(built.len() as u64, Ordering::AcqRel);
                drop(inner);
                self.work.mark();
                return ApplyOutcome::Superseded;
            }

            let RegistryInner { slots, pending, .. } = &mut *inner;
            for (key, slot_incarnation, providers) in built {
                let Some(slot) = slots.get_mut(&key) else {
                    // Retired while we built: do NOT resurrect it.
                    self.metrics
                        .discarded_obsolete
                        .fetch_add(1, Ordering::AcqRel);
                    continue;
                };
                if slot.incarnation != slot_incarnation {
                    // Retired and re-demanded: this artifact belongs to a dead
                    // incarnation. The live one still owes work.
                    self.metrics
                        .discarded_obsolete
                        .fetch_add(1, Ordering::AcqRel);
                    pending.insert(key);
                    slot_moved = true;
                    continue;
                }
                slot.facts = Some(Arc::new(SlotBaseFacts {
                    key: key.clone(),
                    providers: providers.into(),
                    source_generation: generation,
                    actor_incarnation: incarnation,
                    slot_incarnation,
                }));
                self.metrics.installs.fetch_add(1, Ordering::AcqRel);
            }

            let settled = settle(&mut inner, incarnation, generation, &self.metrics);
            let owed = !inner.pending.is_empty();
            drop(inner);
            if owed {
                // More quanta owed, or slot movement re-queued work: re-arm rather
                // than looping inside this synchronous call.
                self.work.mark();
            }
            if slot_moved {
                // Slot/demand-origin rejection: its wake is OURS to provide, and
                // the re-queue above made `owed` true, so it was.
                ApplyOutcome::Superseded
            } else {
                settled
            }
        };

        // `commit` drops HERE — after the installation, never before it.
        drop(commit);
        outcome
    }
}

/// Settle the pass: `Current` only when no work is owed.
///
/// `Current` is the only outcome that may advance routing health, so a pass that
/// left work owed must not claim it.
///
/// Global coherence across every retained slot is required ONLY while a complete
/// recapture epoch is open, and it is enforced by RE-QUEUEING the slots that are
/// not coherent rather than by refusing to settle. Two consequences, both load
/// bearing:
///
/// - ordinary `Caps` and first-demand work no longer reports `Progress` merely
///   because unrelated slots legitimately sit at older generations — a pass that
///   rebuilt everything it was asked to rebuild is complete (Kyra OLB-2B-E3b);
/// - `Progress` now IMPLIES `pending` is non-empty, so the caller always marks.
///   `Progress` with no owed work and no wake — which strands the actor — is
///   unrepresentable rather than merely avoided.
///
/// Re-queueing is also what restarts a recapture whose source generation moved
/// between quanta: the slots built against the older generation stop being
/// coherent, so they go back on the queue and are rebuilt against this one.
fn settle(
    inner: &mut RegistryInner,
    incarnation: u64,
    generation: u64,
    metrics: &RegistryMetrics,
) -> ApplyOutcome {
    if inner.recapture_open {
        let mut displaced = 0;
        for key in inner.incoherent_with(incarnation, generation) {
            if inner.invalidate(&key) {
                displaced += 1;
            }
            inner.pending.insert(key);
        }
        if displaced > 0 {
            // Slots that WERE installed are no longer coherent with this commit:
            // the source moved mid-epoch and the recapture restarts over them.
            metrics
                .facts_invalidated
                .fetch_add(displaced, Ordering::AcqRel);
            metrics.recaptures_restarted.fetch_add(1, Ordering::AcqRel);
        }
    }

    if inner.pending.is_empty() {
        inner.recapture_open = false;
        ApplyOutcome::Current {
            source_generation: generation,
        }
    } else {
        ApplyOutcome::Progress {
            source_generation: generation,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::org::OrgId;
    use std::sync::atomic::AtomicBool;

    fn scope(seed: u8) -> PrivateAudienceScope {
        PrivateAudienceScope::new(CapabilityAudienceScope::Owner {
            org_id: OrgId::from_bytes([seed; 32]),
            audience_handle: [seed; 32],
        })
        .expect("owner scopes are private")
    }

    fn key(seed: u8, tag: &str) -> SlotKey {
        SlotKey {
            scope: scope(seed),
            capability: CapabilityAuthorityId::for_tag(tag),
        }
    }

    type Hook = Box<dyn Fn() + Send + Sync>;
    type SharedHook = Arc<dyn Fn() + Send + Sync>;

    /// The observable state of the source the witnesses drive.
    ///
    /// `gate` stands in for BOTH the query-visible generation and the scoped
    /// mutation-publication gate E3c will bind here: taking it is what a mutation
    /// must do to advance. That makes "the gate is not held across reconstruction"
    /// directly assertable — a rival mutation on another thread must be able to
    /// take it mid-build.
    struct SourceState {
        gate: parking_lot::Mutex<u64>,
        /// Runs during reconstruction, so a witness can move the world mid-build.
        during_build: parking_lot::Mutex<Option<Hook>>,
        /// Runs when the COMMIT pin is released — i.e. strictly after installation.
        on_commit_release: parking_lot::Mutex<Option<SharedHook>>,
        queried: parking_lot::Mutex<Vec<SlotKey>>,
        snapshots: AtomicU64,
        registry: parking_lot::Mutex<Option<Arc<NodeOrgRoutingRegistry>>>,
    }

    impl SourceState {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                gate: parking_lot::Mutex::new(1),
                during_build: parking_lot::Mutex::new(None),
                on_commit_release: parking_lot::Mutex::new(None),
                queried: parking_lot::Mutex::new(Vec::new()),
                snapshots: AtomicU64::new(0),
                registry: parking_lot::Mutex::new(None),
            })
        }
        fn queries(&self) -> Vec<SlotKey> {
            self.queried.lock().clone()
        }
        fn reset(&self) {
            self.queried.lock().clear();
        }
        fn generation(&self) -> u64 {
            *self.gate.lock()
        }
        /// Advance the query-visible generation, as a mutation+publication would.
        fn advance(&self) {
            *self.gate.lock() += 1;
        }
        fn assert_no_registry_lock(&self, context: &str) {
            if let Some(registry) = self.registry.lock().clone() {
                assert!(registry.inner.try_lock().is_some(), "{context}");
            }
        }
    }

    struct TestSource(Arc<SourceState>);

    /// Owns its bounded material: reconstruction touches no source lock.
    struct TestSnapshot {
        state: Arc<SourceState>,
        generation: u64,
        _captured: Vec<SlotKey>,
    }

    impl SourceSnapshot for TestSnapshot {
        fn generation(&self) -> u64 {
            self.generation
        }
        fn providers(&self, key: &SlotKey) -> Vec<PrivateCapabilityProvider> {
            self.state.queried.lock().push(key.clone());
            self.state
                .assert_no_registry_lock("no registry lock may be held across reconstruction");
            assert!(
                self.state.gate.try_lock().is_some(),
                "no source/publication lock may be held across decode, sort or projection"
            );
            let hook = self.state.during_build.lock().take();
            if let Some(hook) = hook {
                hook();
            }
            Vec::new()
        }
    }

    struct TestCommitPin<'a> {
        state: Arc<SourceState>,
        generation: parking_lot::MutexGuard<'a, u64>,
    }

    impl SourceCommitPin for TestCommitPin<'_> {
        fn generation(&self) -> u64 {
            *self.generation
        }
    }

    impl Drop for TestCommitPin<'_> {
        fn drop(&mut self) {
            let hook = self.state.on_commit_release.lock().clone();
            if let Some(hook) = hook {
                hook();
            }
        }
    }

    impl SlotSource for TestSource {
        fn snapshot(&self, keys: &[SlotKey]) -> Box<dyn SourceSnapshot> {
            self.0
                .assert_no_registry_lock("no registry lock may be held across the source snapshot");
            // Brief: the gate guard is released before this returns.
            let generation = *self.0.gate.lock();
            self.0.snapshots.fetch_add(1, Ordering::AcqRel);
            Box::new(TestSnapshot {
                state: self.0.clone(),
                generation,
                _captured: keys.to_vec(),
            })
        }

        fn pin_if_current(
            &self,
            expected_generation: u64,
        ) -> Option<Box<dyn SourceCommitPin + '_>> {
            self.0.assert_no_registry_lock(
                "the commit pin must be acquired BEFORE the registry lock",
            );
            let generation = self.0.gate.lock();
            if *generation != expected_generation {
                return None;
            }
            Some(Box::new(TestCommitPin {
                state: self.0.clone(),
                generation,
            }))
        }
    }

    struct Fixture {
        registry: Arc<NodeOrgRoutingRegistry>,
        source: Arc<SourceState>,
        metrics: Arc<RegistryMetrics>,
    }

    impl Fixture {
        fn family(&self) -> RoutingFamily {
            self.registry.new_family().expect("family")
        }
    }

    /// Builds a registry with actor incarnation 1 already live — the state
    /// `RoutingSupervisor` establishes before it applies anything.
    fn fixture() -> Fixture {
        let source = SourceState::new();
        let metrics: Arc<RegistryMetrics> = Arc::default();
        let registry = NodeOrgRoutingRegistry::new(
            Arc::new(TestSource(source.clone())),
            Arc::default(),
            metrics.clone(),
        );
        *source.registry.lock() = Some(registry.clone());
        registry.activate_incarnation(1);
        Fixture {
            registry,
            source,
            metrics,
        }
    }

    fn request(registry_work: bool, dirty: DirtyCapabilities) -> ApplyRequest {
        ApplyRequest {
            batch: crate::adapter::net::behavior::org_scoped_store::PrivateDiscoveryChangeBatch {
                generation: 1,
                dirty,
            },
            registry_work,
        }
    }

    fn caps(tags: &[&str]) -> DirtyCapabilities {
        DirtyCapabilities::Caps(
            tags.iter()
                .map(|t| CapabilityAuthorityId::for_tag(t))
                .collect(),
        )
    }

    // ---------------------------------------------------------------- identity

    /// The public partition cannot form a routing slot key at all.
    #[test]
    fn the_public_scope_cannot_form_a_slot_key() {
        assert!(
            PrivateAudienceScope::new(CapabilityAudienceScope::Public).is_none(),
            "this is the PRIVATE routing consumer"
        );
        assert!(PrivateAudienceScope::new(CapabilityAudienceScope::Owner {
            org_id: OrgId::from_bytes([7; 32]),
            audience_handle: [7; 32],
        })
        .is_some());
    }

    /// Clones of ONE family share its identity and its 64-handle budget; an
    /// independently minted family gets its own.
    #[test]
    fn a_clone_family_shares_one_identity_and_one_budget() {
        let f = fixture();
        let family = f.family();
        let clone = family.clone();
        let independent = f.family();

        let _a = family.demand(key(1, "nrpc:a")).expect("a");
        let _b = clone.demand(key(1, "nrpc:b")).expect("b");
        assert_eq!(family.handles(), 2, "the clone spends the SAME budget");
        assert_eq!(clone.handles(), 2);
        assert_eq!(independent.handles(), 0, "a distinct family is unaffected");

        // Exhaust the shared budget through the clone; the original is exhausted too.
        let mut held = Vec::new();
        for index in 0..(MAX_HANDLES_PER_FAMILY - 2) {
            held.push(
                clone
                    .demand(key(1, &format!("nrpc:s{index}")))
                    .expect("within the shared bound"),
            );
        }
        assert_eq!(
            family.demand(key(1, "nrpc:over")).err(),
            Some(DemandRefused::FamilyAtCapacity),
            "the original clone is bounded by what its clone spent"
        );
        assert!(
            independent.demand(key(1, "nrpc:own")).is_ok(),
            "an independent family still has its own budget"
        );
    }

    // ------------------------------------------------------------- recapture

    /// First demand gets a full CURRENT recapture even when earlier source deltas
    /// were already destructively drained — the slot's own pending identity is what
    /// drives it, not any surviving source delta.
    #[test]
    fn first_demand_after_drained_deltas_still_gets_a_full_recapture() {
        let f = fixture();
        // Historical deltas: applied and consumed before this slot ever existed.
        f.registry
            .apply(1, request(false, DirtyCapabilities::RebuildAll));
        f.source.reset();

        let family = f.family();
        let _held = family.demand(key(1, "nrpc:a")).expect("demand");
        assert_eq!(f.registry.pending_slots(), 1);

        // A CLEAN source pass still rebuilds it, from registry work alone.
        let outcome = f.registry.apply(1, request(true, DirtyCapabilities::Clean));
        assert_eq!(f.source.queries(), vec![key(1, "nrpc:a")]);
        assert!(matches!(outcome, ApplyOutcome::Current { .. }));
        let facts = f.registry.base_facts(&key(1, "nrpc:a")).expect("built");
        assert_eq!(facts.slot_incarnation, 2, "family 1, then this slot");
        assert_eq!(facts.actor_incarnation, 1);
        assert_eq!(f.registry.pending_slots(), 0);
    }

    /// Two families converge on ONE node slot and ONE reconstruction.
    #[test]
    fn two_families_share_one_slot_and_one_reconstruction() {
        let f = fixture();
        let a = f.family().demand(key(1, "nrpc:a")).expect("a");
        let b = f.family().demand(key(1, "nrpc:a")).expect("b");
        assert_eq!(f.registry.retained_slots(), 1, "one shared node slot");

        f.source.reset();
        f.registry.apply(1, request(true, DirtyCapabilities::Clean));
        assert_eq!(
            f.source.queries(),
            vec![key(1, "nrpc:a")],
            "reconstructed once per SLOT, not once per family"
        );
        drop((a, b));
    }

    /// A DIFFERENT audience scope is a different slot: indexing never broadens
    /// authority by sharing across scopes.
    #[test]
    fn a_different_audience_scope_is_a_different_slot() {
        let f = fixture();
        let family = f.family();
        let _a = family.demand(key(1, "nrpc:a")).expect("a");
        let _b = family.demand(key(2, "nrpc:a")).expect("b");
        assert_eq!(
            f.registry.retained_slots(),
            2,
            "same capability, different scope => distinct slots"
        );

        f.source.reset();
        f.registry.apply(1, request(false, caps(&["nrpc:a"])));
        let mut queried = f.source.queries();
        queried.sort();
        let mut expected = vec![key(1, "nrpc:a"), key(2, "nrpc:a")];
        expected.sort();
        assert_eq!(
            queried, expected,
            "the capability bucket holds both scoped slots, each rebuilt separately"
        );
    }

    // ---------------------------------------------------------------- bounds

    /// The 65th family handle is refused without corrupting the first 64, and a
    /// DUPLICATE demand counts toward the bound rather than bypassing it.
    #[test]
    fn the_sixty_fifth_family_handle_is_refused_without_corrupting_the_first_64() {
        let f = fixture();
        let family = f.family();
        let mut held = Vec::new();
        for index in 0..MAX_HANDLES_PER_FAMILY {
            held.push(
                family
                    .demand(key(1, &format!("nrpc:f{index}")))
                    .expect("within the bound"),
            );
        }
        assert_eq!(
            family.demand(key(1, "nrpc:over")).err(),
            Some(DemandRefused::FamilyAtCapacity)
        );
        // A duplicate of an EXISTING key is still a handle, so it is refused too.
        assert_eq!(
            family.demand(key(1, "nrpc:f0")).err(),
            Some(DemandRefused::FamilyAtCapacity),
            "duplicate demand cannot bypass the handle bound"
        );
        assert_eq!(f.metrics.refused_family_at_capacity(), 2);
        assert_eq!(
            f.registry.retained_slots(),
            MAX_HANDLES_PER_FAMILY,
            "the first 64 are intact and no slot was created for the refusals"
        );
        assert_eq!(held.len(), MAX_HANDLES_PER_FAMILY);
    }

    /// The 257th unique node slot is deterministically unretained, and no live slot
    /// is evicted for it.
    #[test]
    fn the_two_hundred_fifty_seventh_slot_is_deterministically_unretained() {
        let f = fixture();
        let mut held = Vec::new();
        let mut family = f.family();
        for index in 0..MAX_NODE_SLOTS {
            if index > 0 && index % MAX_HANDLES_PER_FAMILY == 0 {
                family = f.family();
            }
            held.push(
                family
                    .demand(key(1, &format!("nrpc:n{index}")))
                    .expect("within the node bound"),
            );
        }
        assert_eq!(f.registry.retained_slots(), MAX_NODE_SLOTS);

        let beyond = key(1, "nrpc:beyond");
        assert_eq!(
            f.family().demand(beyond.clone()).err(),
            Some(DemandRefused::NodeAtCapacity)
        );
        assert_eq!(f.metrics.refused_node_at_capacity(), 1);
        assert_eq!(
            f.registry.retained_slots(),
            MAX_NODE_SLOTS,
            "no live slot was evicted to make room"
        );
        assert!(
            f.registry.base_facts(&beyond).is_none(),
            "the refused key is cold/unretained"
        );
    }

    /// An exhausted identity space refuses deterministically: no slot retained, no
    /// wrap, no panic — and no family minted either.
    #[test]
    fn an_exhausted_identity_space_refuses_deterministically() {
        let f = fixture();
        let family = f.family();
        let _live = family.demand(key(1, "nrpc:a")).expect("a");
        let retained = f.registry.retained_slots();

        f.registry.inner.lock().next_id = u64::MAX;

        assert_eq!(
            family.demand(key(1, "nrpc:fresh")).err(),
            Some(DemandRefused::IdSpaceExhausted),
            "a NEW slot needs a fresh incarnation and cannot have one"
        );
        assert_eq!(
            f.registry.retained_slots(),
            retained,
            "the refusal retained nothing"
        );
        assert_eq!(
            f.registry.inner.lock().next_id,
            u64::MAX,
            "and mutated no counter"
        );
        assert_eq!(
            f.registry.new_family().err(),
            Some(DemandRefused::IdSpaceExhausted)
        );
        assert_eq!(f.metrics.refused_id_space_exhausted(), 2);

        // A duplicate demand for an EXISTING slot needs no new identity.
        assert!(
            family.demand(key(1, "nrpc:a")).is_ok(),
            "sharing a retained slot allocates nothing"
        );
    }

    // -------------------------------------------------------------- lifecycle

    /// Dropping one of several references preserves the slot; dropping the LAST
    /// retires it.
    #[test]
    fn only_the_last_reference_retires_the_slot() {
        let f = fixture();
        let a = f.family().demand(key(1, "nrpc:a")).expect("a");
        let b = f.family().demand(key(1, "nrpc:a")).expect("b");
        f.registry.apply(1, request(true, DirtyCapabilities::Clean));
        assert!(f.registry.base_facts(&key(1, "nrpc:a")).is_some());

        drop(a);
        assert_eq!(f.registry.retained_slots(), 1, "one reference remains");
        assert!(f.registry.base_facts(&key(1, "nrpc:a")).is_some());

        drop(b);
        assert_eq!(
            f.registry.retained_slots(),
            0,
            "the last reference retired it"
        );
        assert_eq!(f.metrics.slots_retired(), 1);
        assert!(f.registry.base_facts(&key(1, "nrpc:a")).is_none());
    }

    /// A build in flight cannot resurrect a slot retired and re-demanded beneath
    /// it — driven through the REAL lifecycle: `DemandHandle::drop` retires the
    /// slot and a real family owner re-demands it, both from inside the source
    /// query. The stale artifact is discarded, the recreated slot stays indexed and
    /// pending, and the pass reports `Superseded` so its wake is owed.
    #[test]
    fn a_late_build_cannot_resurrect_a_replaced_slot_incarnation() {
        let f = fixture();
        let target = key(1, "nrpc:a");
        let original = f.family();
        let successor = f.family();

        let held: Arc<parking_lot::Mutex<Option<DemandHandle>>> = Arc::new(
            parking_lot::Mutex::new(Some(original.demand(target.clone()).expect("first demand"))),
        );
        let first_incarnation = f
            .registry
            .inner
            .lock()
            .slots
            .get(&target)
            .expect("retained")
            .incarnation;

        let recreated: Arc<parking_lot::Mutex<Option<DemandHandle>>> =
            Arc::new(parking_lot::Mutex::new(None));
        {
            let held = held.clone();
            let recreated = recreated.clone();
            let successor = successor.clone();
            let target = target.clone();
            *f.source.during_build.lock() = Some(Box::new(move || {
                // The real RAII release: last reference retires the slot.
                drop(held.lock().take());
                // A real family owner re-demands it — a FRESH incarnation.
                *recreated.lock() = Some(successor.demand(target.clone()).expect("re-demand"));
            }));
        }

        let outcome = f.registry.apply(1, request(true, DirtyCapabilities::Clean));
        assert_eq!(
            outcome,
            ApplyOutcome::Superseded,
            "slot movement supersedes the attempt"
        );
        assert!(
            f.registry.base_facts(&target).is_none(),
            "the artifact for the dead incarnation was discarded, not installed"
        );
        assert_eq!(f.metrics.discarded_obsolete(), 1);
        assert_eq!(f.metrics.installs(), 0);
        assert_eq!(f.metrics.slots_retired(), 1);

        let inner = f.registry.inner.lock();
        let slot = inner.slots.get(&target).expect("recreated slot retained");
        assert_ne!(
            slot.incarnation, first_incarnation,
            "the recreated slot has a FRESH identity"
        );
        assert!(
            inner
                .slots_by_capability
                .get(&target.capability)
                .is_some_and(|bucket| bucket.contains(&target)),
            "and is still indexed by capability"
        );
        assert!(
            inner.pending.contains(&target),
            "the live incarnation still owes work, queued authoritatively"
        );
        drop(inner);
        assert!(recreated.lock().is_some());
    }

    /// A brand-new actor incarnation invalidates everything the previous one built
    /// and queues every retained slot.
    #[test]
    fn a_new_incarnation_invalidates_every_retained_slot() {
        let f = fixture();
        let family = f.family();
        let _a = family.demand(key(1, "nrpc:a")).expect("a");
        let _b = family.demand(key(1, "nrpc:b")).expect("b");
        f.registry.apply(1, request(true, DirtyCapabilities::Clean));
        assert!(f.registry.base_facts(&key(1, "nrpc:a")).is_some());
        assert_eq!(f.registry.pending_slots(), 0);

        f.registry.deactivate_incarnation(1);
        f.registry.activate_incarnation(2);

        assert!(
            f.registry.base_facts(&key(1, "nrpc:a")).is_none(),
            "nothing a dead incarnation built stays readable"
        );
        assert_eq!(
            f.registry.pending_slots(),
            2,
            "every retained slot re-queued"
        );
    }

    // ---------------------------------------------------------- authority

    /// An application from an actor that is NOT the live one consumes no pending
    /// work, installs nothing, and never reports `Current`.
    #[test]
    fn a_stale_actor_neither_consumes_pending_work_nor_reports_current() {
        let f = fixture();
        let _held = f.family().demand(key(1, "nrpc:a")).expect("a");
        assert_eq!(f.registry.pending_slots(), 1);

        f.source.reset();
        let outcome = f
            .registry
            .apply(2, request(true, DirtyCapabilities::RebuildAll));
        assert_eq!(outcome, ApplyOutcome::Superseded);
        assert!(
            f.source.queries().is_empty(),
            "a stale actor never even queries the source"
        );
        assert_eq!(
            f.registry.pending_slots(),
            1,
            "the authoritative work is still owed"
        );
        assert_eq!(f.metrics.stale_actor_rejections(), 1);

        // With NO actor live, the same holds.
        f.registry.deactivate_incarnation(1);
        assert_eq!(
            f.registry.apply(1, request(true, DirtyCapabilities::Clean)),
            ApplyOutcome::Superseded
        );
        assert_eq!(f.registry.pending_slots(), 1);
        assert_eq!(f.metrics.stale_actor_rejections(), 2);

        // The live successor finds the work intact.
        f.registry.activate_incarnation(3);
        let outcome = f.registry.apply(3, request(true, DirtyCapabilities::Clean));
        assert!(matches!(outcome, ApplyOutcome::Current { .. }));
        assert_eq!(f.source.queries(), vec![key(1, "nrpc:a")]);
    }

    /// Authority revoked DURING the build: every still-live selected slot is
    /// requeued, nothing is installed, and the outcome is never `Current`.
    #[test]
    fn authority_revoked_during_a_build_requeues_every_live_slot() {
        let f = fixture();
        let family = f.family();
        let a = key(1, "nrpc:a");
        let b = key(1, "nrpc:b");
        let _ha = family.demand(a.clone()).expect("a");
        let hb = family.demand(b.clone()).expect("b");

        let registry = f.registry.clone();
        let hb_cell: Arc<parking_lot::Mutex<Option<DemandHandle>>> =
            Arc::new(parking_lot::Mutex::new(Some(hb)));
        {
            let hb_cell = hb_cell.clone();
            *f.source.during_build.lock() = Some(Box::new(move || {
                // The actor's fence fires mid-build, and one slot also retires —
                // that one must NOT be requeued, because it no longer exists.
                registry.deactivate_incarnation(1);
                drop(hb_cell.lock().take());
            }));
        }

        let outcome = f.registry.apply(1, request(true, DirtyCapabilities::Clean));
        assert_eq!(outcome, ApplyOutcome::Superseded);
        assert_eq!(
            f.metrics.installs(),
            0,
            "nothing from a revoked actor lands"
        );
        assert_eq!(f.metrics.stale_actor_rejections(), 1);

        let inner = f.registry.inner.lock();
        assert!(inner.pending.contains(&a), "the live slot still owes work");
        assert!(
            !inner.pending.contains(&b),
            "a retired slot is not resurrected into the queue"
        );
    }

    // ------------------------------------------------------------ invalidation

    /// A `Caps` delta invalidates the affected slot's facts UNDER THE PHASE-1 LOCK,
    /// before any rebuild begins — asserted from inside the source query, while the
    /// rebuild is still in flight. An unrelated slot keeps its facts.
    #[test]
    fn a_caps_delta_invalidates_affected_facts_before_the_rebuild_begins() {
        let f = fixture();
        let family = f.family();
        let c = key(1, "nrpc:c");
        let d = key(1, "nrpc:d");
        let _hc = family.demand(c.clone()).expect("c");
        let _hd = family.demand(d.clone()).expect("d");
        f.registry.apply(1, request(true, DirtyCapabilities::Clean));
        assert!(f.registry.base_facts(&c).is_some());
        assert!(f.registry.base_facts(&d).is_some());

        let observed = Arc::new(AtomicBool::new(false));
        {
            let registry = f.registry.clone();
            let observed = observed.clone();
            let c = c.clone();
            let d = d.clone();
            *f.source.during_build.lock() = Some(Box::new(move || {
                assert!(
                    registry.base_facts(&c).is_none(),
                    "C's stale facts must already be gone while C rebuilds"
                );
                assert!(
                    registry.base_facts(&d).is_some(),
                    "D was not named by the delta and keeps its facts"
                );
                observed.store(true, Ordering::Release);
            }));
        }

        f.source.reset();
        f.registry.apply(1, request(false, caps(&["nrpc:c"])));
        assert!(observed.load(Ordering::Acquire), "the hook must have run");
        assert_eq!(f.source.queries(), vec![c.clone()]);
        assert!(f.registry.base_facts(&c).is_some(), "and is rebuilt after");
    }

    /// Caps invalidation covers slots BEYOND the quantum: a slot that will not be
    /// rebuilt for several passes must not keep serving stale facts in the meantime.
    #[test]
    fn caps_invalidates_affected_slots_beyond_the_quantum() {
        let f = fixture();
        let mut family = f.family();
        let mut keys = Vec::new();
        let mut held = Vec::new();
        for index in 0..(APPLY_QUANTUM + 8) {
            if index > 0 && index % MAX_HANDLES_PER_FAMILY == 0 {
                family = f.family();
            }
            let k = key(1, &format!("nrpc:q{index}"));
            held.push(family.demand(k.clone()).expect("demanded"));
            keys.push(k);
        }
        // Two passes to build them all.
        f.registry.apply(1, request(true, DirtyCapabilities::Clean));
        f.registry.apply(1, request(true, DirtyCapabilities::Clean));
        assert!(keys.iter().all(|k| f.registry.base_facts(k).is_some()));

        let tags: Vec<String> = (0..(APPLY_QUANTUM + 8))
            .map(|index| format!("nrpc:q{index}"))
            .collect();
        let all: Vec<&str> = tags.iter().map(String::as_str).collect();

        f.source.reset();
        let outcome = f.registry.apply(1, request(false, caps(&all)));
        assert_eq!(
            f.source.queries().len(),
            APPLY_QUANTUM,
            "only one quantum is rebuilt"
        );
        assert!(
            matches!(outcome, ApplyOutcome::Progress { .. }),
            "work remains, so this is not a complete installation"
        );
        assert_eq!(
            keys.iter()
                .filter(|k| f.registry.base_facts(k).is_some())
                .count(),
            APPLY_QUANTUM,
            "EVERY affected slot was invalidated; only the quantum was rebuilt"
        );
        assert_eq!(f.registry.pending_slots(), 8);
    }

    /// `Caps(C)` touches only C's indexed slots; `RebuildAll` touches every
    /// retained slot; an undemanded capability is never projected.
    #[test]
    fn caps_touches_only_its_bucket_and_rebuild_all_touches_every_slot() {
        let f = fixture();
        let family = f.family();
        let _a = family.demand(key(1, "nrpc:a")).expect("a");
        let _b = family.demand(key(1, "nrpc:b")).expect("b");
        f.registry.apply(1, request(true, DirtyCapabilities::Clean));

        f.source.reset();
        f.registry.apply(1, request(false, caps(&["nrpc:b"])));
        assert_eq!(f.source.queries(), vec![key(1, "nrpc:b")]);

        f.source.reset();
        f.registry
            .apply(1, request(false, DirtyCapabilities::RebuildAll));
        assert_eq!(f.source.queries().len(), 2, "every retained slot");

        f.source.reset();
        f.registry.apply(1, request(false, caps(&["nrpc:absent"])));
        assert!(
            f.source.queries().is_empty(),
            "an undemanded capability is never projected"
        );
    }

    /// A slot named by BOTH the source delta and the registry's pending set is
    /// reconstructed ONCE.
    #[test]
    fn combined_source_and_registry_work_deduplicates_a_slot() {
        let f = fixture();
        let target = key(1, "nrpc:a");
        let _held = f.family().demand(target.clone()).expect("a");
        assert_eq!(
            f.registry.pending_slots(),
            1,
            "registry work owes this slot"
        );

        f.source.reset();
        f.registry.apply(1, request(true, caps(&["nrpc:a"])));
        assert_eq!(
            f.source.queries(),
            vec![target],
            "named by both domains, built once"
        );
    }

    // ------------------------------------------------------------ completion

    /// A multi-quantum recapture reports `Progress` until the LAST quantum, keeps
    /// the epoch's remaining set authoritative, and only then reports `Current`
    /// with every retained slot coherently stamped.
    #[test]
    fn a_multi_quantum_recapture_reports_progress_until_it_completes() {
        let f = fixture();
        let mut family = f.family();
        let mut keys = Vec::new();
        let mut held = Vec::new();
        for index in 0..(APPLY_QUANTUM + 10) {
            if index > 0 && index % MAX_HANDLES_PER_FAMILY == 0 {
                family = f.family();
            }
            let k = key(1, &format!("nrpc:q{index}"));
            held.push(family.demand(k.clone()).expect("demanded"));
            keys.push(k);
        }

        f.source.reset();
        let first = f
            .registry
            .apply(1, request(false, DirtyCapabilities::RebuildAll));
        assert_eq!(
            f.source.queries().len(),
            APPLY_QUANTUM,
            "at most one quantum per synchronous application"
        );
        assert!(
            matches!(first, ApplyOutcome::Progress { .. }),
            "an incomplete recapture must NOT claim a current installation"
        );
        assert_eq!(
            f.registry.pending_slots(),
            10,
            "the remainder lives in the registry, not in the wake flag"
        );

        // The actor promotes the next wake to RebuildAll while a recapture is
        // owed. That must CONTINUE the epoch, not re-expand it.
        f.source.reset();
        let second = f
            .registry
            .apply(1, request(true, DirtyCapabilities::RebuildAll));
        assert_eq!(
            f.source.queries().len(),
            10,
            "the epoch resumed where it left off"
        );
        assert!(
            matches!(second, ApplyOutcome::Current { .. }),
            "the final quantum completes the recapture"
        );
        assert_eq!(f.registry.pending_slots(), 0);

        let generation = f.source.generation();
        for k in &keys {
            let facts = f.registry.base_facts(k).expect("built");
            assert_eq!(facts.actor_incarnation, 1);
            assert_eq!(facts.source_generation, generation);
        }
        assert!(!f.registry.inner.lock().recapture_open, "the epoch closed");
    }

    /// A recapture whose source generation moved between quanta RESTARTS, so
    /// `Current` is never reported over a set built against two generations.
    #[test]
    fn a_recapture_restarts_when_the_source_generation_moves_between_quanta() {
        let f = fixture();
        let mut family = f.family();
        let mut held = Vec::new();
        for index in 0..(APPLY_QUANTUM + 4) {
            if index > 0 && index % MAX_HANDLES_PER_FAMILY == 0 {
                family = f.family();
            }
            held.push(
                family
                    .demand(key(1, &format!("nrpc:q{index}")))
                    .expect("demanded"),
            );
        }

        let first = f
            .registry
            .apply(1, request(false, DirtyCapabilities::RebuildAll));
        assert!(matches!(first, ApplyOutcome::Progress { .. }));
        assert_eq!(f.registry.pending_slots(), 4);

        // The source moves between quanta — nothing is pinned right now.
        f.source.advance();

        let second = f
            .registry
            .apply(1, request(true, DirtyCapabilities::RebuildAll));
        assert!(
            matches!(second, ApplyOutcome::Progress { .. }),
            "the restart cannot complete in one quantum"
        );
        assert_eq!(f.metrics.recaptures_restarted(), 1);
        assert_eq!(
            f.registry.pending_slots(),
            APPLY_QUANTUM,
            "the 64 slots built against the OLD generation went back on the queue"
        );

        let third = f
            .registry
            .apply(1, request(true, DirtyCapabilities::RebuildAll));
        assert!(matches!(third, ApplyOutcome::Current { .. }));
        let generation = f.source.generation();
        assert!(
            f.registry.inner.lock().slots.values().all(|slot| slot
                .facts
                .as_ref()
                .is_some_and(|facts| facts.source_generation == generation)),
            "ONE coherent source generation across the whole retained set"
        );
    }

    /// Ordinary `Caps` work at a NEWER generation is COMPLETE.
    ///
    /// Rebuilding C at G+1 leaves unrelated D legitimately stamped at G. Demanding
    /// global coherence outside a recapture epoch would report `Progress` for a
    /// pass that finished everything it was asked to do — and with `pending` empty
    /// there is nothing to mark, so the actor would park owing a recapture that
    /// nothing will ever wake (Kyra OLB-2B-E3b).
    #[test]
    fn ordinary_caps_at_a_newer_generation_reports_current() {
        let f = fixture();
        let family = f.family();
        let c = key(1, "nrpc:c");
        let d = key(1, "nrpc:d");
        let _hc = family.demand(c.clone()).expect("c");
        let _hd = family.demand(d.clone()).expect("d");
        f.registry.apply(1, request(true, DirtyCapabilities::Clean));
        let first = f.source.generation();

        // The source moves, dirtying ONLY c.
        f.source.advance();
        let outcome = f.registry.apply(1, request(false, caps(&["nrpc:c"])));

        let second = f.source.generation();
        assert_ne!(first, second);
        assert_eq!(
            f.registry
                .base_facts(&c)
                .expect("c rebuilt")
                .source_generation,
            second
        );
        assert_eq!(
            f.registry
                .base_facts(&d)
                .expect("d untouched")
                .source_generation,
            first,
            "an unrelated slot legitimately keeps its older stamp"
        );
        assert_eq!(f.registry.pending_slots(), 0, "no work is owed");
        assert_eq!(
            outcome,
            ApplyOutcome::Current {
                source_generation: second
            },
            "the pass rebuilt everything it was asked to; that IS complete"
        );
    }

    /// First-demand work at a NEWER generation is likewise complete.
    #[test]
    fn first_demand_at_a_newer_generation_reports_current() {
        let f = fixture();
        let family = f.family();
        let established = key(1, "nrpc:a");
        let _ha = family.demand(established.clone()).expect("a");
        f.registry.apply(1, request(true, DirtyCapabilities::Clean));
        let first = f.source.generation();

        f.source.advance();
        let fresh = key(1, "nrpc:b");
        let _hb = family.demand(fresh.clone()).expect("b");

        let outcome = f.registry.apply(1, request(true, DirtyCapabilities::Clean));
        let second = f.source.generation();
        assert_eq!(
            f.registry
                .base_facts(&fresh)
                .expect("built")
                .source_generation,
            second
        );
        assert_eq!(
            f.registry
                .base_facts(&established)
                .expect("retained")
                .source_generation,
            first,
            "the unrelated retained slot was not rebuilt and did not need to be"
        );
        assert_eq!(f.registry.pending_slots(), 0);
        assert_eq!(
            outcome,
            ApplyOutcome::Current {
                source_generation: second
            }
        );
    }

    /// Structural: `Progress` is never returned with an empty queue, so it always
    /// carries the wake its contract promises.
    #[test]
    fn progress_always_implies_owed_work() {
        let f = fixture();
        let mut family = f.family();
        let mut held = Vec::new();
        for index in 0..(APPLY_QUANTUM + 3) {
            if index > 0 && index % MAX_HANDLES_PER_FAMILY == 0 {
                family = f.family();
            }
            held.push(
                family
                    .demand(key(1, &format!("nrpc:q{index}")))
                    .expect("demanded"),
            );
        }

        for pass in 0..4 {
            let outcome = f
                .registry
                .apply(1, request(true, DirtyCapabilities::RebuildAll));
            let owed = f.registry.pending_slots();
            match outcome {
                ApplyOutcome::Progress { .. } => assert!(
                    owed > 0,
                    "pass {pass}: Progress with nothing owed strands the actor"
                ),
                ApplyOutcome::Current { .. } => assert_eq!(owed, 0, "pass {pass}"),
                other => panic!("pass {pass}: unexpected {other:?}"),
            }
        }
    }

    // ------------------------------------------------------------- currentness

    /// A concurrent mutation lands DURING reconstruction — without waiting for it —
    /// and the commit pin for the snapshot's generation then fails. Nothing stale
    /// installs, and every still-live selected slot goes back on the queue.
    ///
    /// This is the transition the snapshot/commit split exists for: the rival
    /// mutation is performed from another thread with `try_lock`, so it FAILS
    /// outright if reconstruction were holding the publication gate.
    #[test]
    fn a_mutation_during_reconstruction_defeats_the_commit_pin_without_waiting() {
        let f = fixture();
        let target = key(1, "nrpc:a");
        let _held = f.family().demand(target.clone()).expect("a");

        let moved = Arc::new(AtomicBool::new(false));
        {
            let source = f.source.clone();
            let moved = moved.clone();
            *f.source.during_build.lock() = Some(Box::new(move || {
                let source = source.clone();
                let advanced = std::thread::spawn(move || match source.gate.try_lock() {
                    Some(mut generation) => {
                        *generation += 1;
                        true
                    }
                    None => false,
                })
                .join()
                .expect("mutation thread");
                assert!(
                    advanced,
                    "a rival mutation must not have to wait on reconstruction"
                );
                moved.store(true, Ordering::Release);
            }));
        }

        let outcome = f.registry.apply(1, request(true, DirtyCapabilities::Clean));
        assert!(moved.load(Ordering::Acquire), "the mutation must have run");
        assert_eq!(outcome, ApplyOutcome::Superseded);
        assert_eq!(f.metrics.installs(), 0, "nothing stale installed");
        assert!(f.registry.base_facts(&target).is_none());
        assert_eq!(
            f.registry.pending_slots(),
            1,
            "the live slot re-entered the authoritative queue"
        );

        // And the next pass, against the settled generation, completes.
        let outcome = f.registry.apply(1, request(true, DirtyCapabilities::Clean));
        assert_eq!(
            outcome,
            ApplyOutcome::Current {
                source_generation: f.source.generation()
            }
        );
    }

    /// The commit pin is acquired BEFORE the registry lock and is still held when
    /// the facts land: its `Drop` observes the installation already done.
    #[test]
    fn the_commit_pin_outlives_the_installation() {
        let f = fixture();
        let target = key(1, "nrpc:a");
        let _held = f.family().demand(target.clone()).expect("a");

        let observed = Arc::new(AtomicBool::new(false));
        {
            let registry = f.registry.clone();
            let metrics = f.metrics.clone();
            let observed = observed.clone();
            let target = target.clone();
            *f.source.on_commit_release.lock() = Some(Arc::new(move || {
                assert_eq!(
                    metrics.installs(),
                    1,
                    "the commit pin must still be held when the facts are installed"
                );
                assert!(registry.base_facts(&target).is_some());
                observed.store(true, Ordering::Release);
            }));
        }

        let outcome = f.registry.apply(1, request(true, DirtyCapabilities::Clean));
        assert!(
            observed.load(Ordering::Acquire),
            "the commit pin must have dropped"
        );
        let generation = f.source.generation();
        assert_eq!(
            outcome,
            ApplyOutcome::Current {
                source_generation: generation
            }
        );
        assert_eq!(
            f.registry
                .base_facts(&target)
                .expect("built")
                .source_generation,
            generation,
            "facts carry exactly the generation the commit pin proved current"
        );
    }

    /// No registry lock is held across the source snapshot or reconstruction, and
    /// no source/publication lock is held across reconstruction — all asserted from
    /// inside the seams themselves, which fail if either is held.
    #[test]
    fn neither_lock_is_held_across_the_snapshot_or_reconstruction() {
        let f = fixture();
        let _held = f.family().demand(key(1, "nrpc:a")).expect("a");
        f.registry.apply(1, request(true, DirtyCapabilities::Clean));
        assert_eq!(f.source.queries(), vec![key(1, "nrpc:a")]);
        assert_eq!(
            f.source.snapshots.load(Ordering::Acquire),
            1,
            "one brief capture per quantum"
        );
    }
}
