//! OLB-2B-E3b: the bounded node routing registry — the actor's real consumer.
//!
//! Retains one node slot per AUTHORITY-SCOPED key, shared by every clone family
//! demanding that same key, and rebuilds slots from the private-discovery source
//! in bounded quanta. It implements [`DirtyApply`], so the supervised actor from
//! E2 drives it and nothing else does.
//!
//! Two authoritative structures, deliberately separate (Kyra OLB-2B-E3a):
//!
//! - [`RegistryWork`] answers only "is SOME work owed?" — a coalescing wake hint;
//! - [`RegistryInner::pending`] holds the exact slot identities that owe work. The
//!   Boolean must never become the work queue.

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

/// A clone family. Independent families share node slots but hold their own
/// handles and their own bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct FamilyId(pub u64);

/// The AUTHORITY-SCOPED identity of a retained slot.
///
/// The audience scope is part of the key, never elided: two demands under
/// different acting identities/audiences are different slots even for the same
/// capability. Sharing a slot across scopes would let indexing or reuse broaden
/// authority, which no amount of downstream filtering can undo.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SlotKey {
    pub scope: CapabilityAudienceScope,
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

/// Supplies authority-scoped provider facts. Implemented in E3c over the scoped
/// state plus the live floor view; a seam here so the registry can be reviewed
/// without the node's authority plumbing, and so "no registry lock across the
/// source query" is structurally checkable.
pub(crate) trait SlotSource: Send + Sync + 'static {
    /// Authority-scoped providers for `key`. Called with NO registry lock held.
    fn providers(&self, key: &SlotKey) -> Vec<PrivateCapabilityProvider>;
    /// The source's current query-visible generation.
    fn source_generation(&self) -> u64;
}

#[derive(Debug)]
struct Slot {
    /// Replaced whenever the slot is retired, so work in flight for a previous
    /// incarnation can never resurrect it.
    incarnation: u64,
    /// Live demand handles across ALL families.
    refs: usize,
    /// `None` until a recapture installs facts.
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
    next_incarnation: u64,
}

impl RegistryInner {
    fn family_handles(&self, family: FamilyId) -> usize {
        self.families
            .get(&family)
            .map_or(0, |keys| keys.values().sum())
    }
}

/// Observable registry counters.
#[derive(Default)]
pub(crate) struct RegistryMetrics {
    refused_family_at_capacity: AtomicU64,
    refused_node_at_capacity: AtomicU64,
    slots_retired: AtomicU64,
    installs: AtomicU64,
    discarded_obsolete: AtomicU64,
}

impl RegistryMetrics {
    pub(crate) fn refused_family_at_capacity(&self) -> u64 {
        self.refused_family_at_capacity.load(Ordering::Acquire)
    }
    pub(crate) fn refused_node_at_capacity(&self) -> u64 {
        self.refused_node_at_capacity.load(Ordering::Acquire)
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
}

/// The node's bounded routing registry.
pub(crate) struct NodeOrgRoutingRegistry {
    inner: parking_lot::Mutex<RegistryInner>,
    source: Arc<dyn SlotSource>,
    work: Arc<RegistryWork>,
    metrics: Arc<RegistryMetrics>,
    /// Highest actor incarnation seen. An install from an older incarnation is
    /// obsolete by definition.
    live_actor: AtomicU64,
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
            live_actor: AtomicU64::new(0),
        })
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
    pub(crate) fn demand(
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
                inner.next_incarnation += 1;
                let incarnation = inner.next_incarnation;
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

    /// Release one handle. The LAST reference retires the slot and REPLACES its
    /// incarnation, so work in flight cannot resurrect it.
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
            inner.next_incarnation += 1;
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

    /// The retained base facts for `key`, if the slot exists AND has been built.
    /// `None` is the deterministic UNRETAINED/cold outcome.
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
    /// One bounded reconciliation quantum.
    ///
    /// The three phases exist for the lock discipline: the registry lock is held
    /// ONLY to select targets and later to install. The source query, decoding,
    /// sorting and projection all happen with NO lock held.
    fn apply(&self, incarnation: u64, request: ApplyRequest) -> ApplyOutcome {
        self.live_actor.fetch_max(incarnation, Ordering::AcqRel);

        // --- phase 1: select under the lock, bounded to one quantum ---
        let selected: Vec<(SlotKey, u64)> = {
            let mut inner = self.inner.lock();

            let mut targets: BTreeSet<SlotKey> = BTreeSet::new();
            match &request.batch.dirty {
                // Every retained slot — bounded by MAX_NODE_SLOTS.
                DirtyCapabilities::RebuildAll => targets.extend(inner.slots.keys().cloned()),
                // ONLY the capability-indexed buckets.
                DirtyCapabilities::Caps(caps) => {
                    for capability in caps {
                        if let Some(bucket) = inner.slots_by_capability.get(capability) {
                            targets.extend(bucket.iter().cloned());
                        }
                    }
                }
                DirtyCapabilities::Clean => {}
            }
            if request.registry_work {
                // The registry's OWN authoritative pending identities. Union with
                // the source selection; the set dedups by slot identity.
                targets.extend(inner.pending.iter().cloned());
            }

            let mut selected = Vec::with_capacity(targets.len().min(APPLY_QUANTUM));
            let mut overflow = Vec::new();
            for (index, key) in targets.into_iter().enumerate() {
                if index < APPLY_QUANTUM {
                    if let Some(slot) = inner.slots.get(&key) {
                        selected.push((key, slot.incarnation));
                    }
                } else {
                    overflow.push(key);
                }
            }
            for (key, _) in &selected {
                inner.pending.remove(key);
            }
            // The remainder stays AUTHORITATIVE here, not in the wake flag.
            for key in overflow {
                inner.pending.insert(key);
            }
            selected
        };

        if selected.is_empty() {
            return ApplyOutcome::Current {
                source_generation: self.source.source_generation(),
            };
        }

        // --- phase 2: build OFF-LOCK ---
        let before = self.source.source_generation();
        let built: Vec<(SlotKey, u64, Vec<PrivateCapabilityProvider>)> = selected
            .iter()
            .map(|(key, slot_incarnation)| {
                (key.clone(), *slot_incarnation, self.source.providers(key))
            })
            .collect();
        let after = self.source.source_generation();
        if before != after {
            // The source moved under this build. Re-queue and wake: the
            // `Superseded` contract requires a pending or eventual wake, and here
            // the private-discovery movement supplies one as well.
            {
                let mut inner = self.inner.lock();
                for (key, _) in &selected {
                    if inner.slots.contains_key(key) {
                        inner.pending.insert(key.clone());
                    }
                }
            }
            self.work.mark();
            return ApplyOutcome::Superseded;
        }

        // --- phase 3: reacquire and install CONDITIONALLY ---
        // All four conditions must hold: the actor incarnation is still live, the
        // slot is still retained, its incarnation is unchanged, and the source
        // generation is still the one we built from.
        let mut slot_moved = false;
        let remaining = {
            let mut inner = self.inner.lock();
            let actor_live = self.live_actor.load(Ordering::Acquire) == incarnation;
            for (key, slot_incarnation, providers) in built {
                if !actor_live {
                    self.metrics
                        .discarded_obsolete
                        .fetch_add(1, Ordering::AcqRel);
                    continue;
                }
                let Some(slot) = inner.slots.get_mut(&key) else {
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
                    inner.pending.insert(key);
                    slot_moved = true;
                    continue;
                }
                slot.facts = Some(Arc::new(SlotBaseFacts {
                    key: key.clone(),
                    providers: providers.into(),
                    source_generation: after,
                    actor_incarnation: incarnation,
                    slot_incarnation,
                }));
                self.metrics.installs.fetch_add(1, Ordering::AcqRel);
            }
            inner.pending.len()
        };

        if remaining > 0 {
            // More quanta owed, or slot movement re-queued work: re-arm rather
            // than looping inside this synchronous call.
            self.work.mark();
        }
        if slot_moved {
            // Slot/demand-origin rejection: its wake is OURS to provide.
            return ApplyOutcome::Superseded;
        }
        ApplyOutcome::Current {
            source_generation: after,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::org::OrgId;
    use std::sync::atomic::AtomicBool;

    fn scope(seed: u8) -> CapabilityAudienceScope {
        CapabilityAudienceScope::Owner {
            org_id: OrgId::from_bytes([seed; 32]),
            audience_handle: [seed; 32],
        }
    }

    fn key(seed: u8, tag: &str) -> SlotKey {
        SlotKey {
            scope: scope(seed),
            capability: CapabilityAuthorityId::for_tag(tag),
        }
    }

    /// A source the witnesses drive, which asserts it is never queried while the
    /// registry lock is held.
    struct TestSource {
        generation: AtomicU64,
        /// Bumps the generation DURING a query, forcing a source supersede.
        move_during_query: AtomicBool,
        /// Runs during a query, so a witness can retire a slot mid-build.
        #[allow(clippy::type_complexity)]
        during_query: parking_lot::Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
        queried: parking_lot::Mutex<Vec<SlotKey>>,
        registry: parking_lot::Mutex<Option<Arc<NodeOrgRoutingRegistry>>>,
    }

    impl TestSource {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                generation: AtomicU64::new(1),
                move_during_query: AtomicBool::new(false),
                during_query: parking_lot::Mutex::new(None),
                queried: parking_lot::Mutex::new(Vec::new()),
                registry: parking_lot::Mutex::new(None),
            })
        }
        fn queries(&self) -> Vec<SlotKey> {
            self.queried.lock().clone()
        }
        fn reset(&self) {
            self.queried.lock().clear();
        }
    }

    impl SlotSource for TestSource {
        fn providers(&self, key: &SlotKey) -> Vec<PrivateCapabilityProvider> {
            self.queried.lock().push(key.clone());
            // The registry lock MUST NOT be held here.
            if let Some(registry) = self.registry.lock().clone() {
                assert!(
                    registry.inner.try_lock().is_some(),
                    "no registry lock may be held across the source query"
                );
            }
            if let Some(hook) = self.during_query.lock().take() {
                hook();
            }
            if self.move_during_query.load(Ordering::Acquire) {
                self.generation.fetch_add(1, Ordering::AcqRel);
            }
            Vec::new()
        }
        fn source_generation(&self) -> u64 {
            self.generation.load(Ordering::Acquire)
        }
    }

    struct Fixture {
        registry: Arc<NodeOrgRoutingRegistry>,
        source: Arc<TestSource>,
        metrics: Arc<RegistryMetrics>,
    }

    fn fixture() -> Fixture {
        let source = TestSource::new();
        let metrics: Arc<RegistryMetrics> = Arc::default();
        let registry = NodeOrgRoutingRegistry::new(source.clone(), Arc::default(), metrics.clone());
        *source.registry.lock() = Some(registry.clone());
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

        let _held = f
            .registry
            .demand(FamilyId(1), key(1, "nrpc:a"))
            .expect("demand");
        assert_eq!(f.registry.pending_slots(), 1);

        // A CLEAN source pass still rebuilds it, from registry work alone.
        f.registry.apply(1, request(true, DirtyCapabilities::Clean));
        assert_eq!(f.source.queries(), vec![key(1, "nrpc:a")]);
        let facts = f.registry.base_facts(&key(1, "nrpc:a")).expect("built");
        assert_eq!(facts.slot_incarnation, 1);
        assert_eq!(f.registry.pending_slots(), 0);
    }

    /// Two families converge on ONE node slot and ONE reconstruction.
    #[test]
    fn two_families_share_one_slot_and_one_reconstruction() {
        let f = fixture();
        let a = f.registry.demand(FamilyId(1), key(1, "nrpc:a")).expect("a");
        let b = f.registry.demand(FamilyId(2), key(1, "nrpc:a")).expect("b");
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
        let _a = f.registry.demand(FamilyId(1), key(1, "nrpc:a")).expect("a");
        let _b = f.registry.demand(FamilyId(1), key(2, "nrpc:a")).expect("b");
        assert_eq!(
            f.registry.retained_slots(),
            2,
            "same capability, different scope => distinct slots"
        );

        f.source.reset();
        f.registry.apply(
            1,
            request(
                false,
                DirtyCapabilities::Caps(
                    [CapabilityAuthorityId::for_tag("nrpc:a")]
                        .into_iter()
                        .collect(),
                ),
            ),
        );
        let mut queried = f.source.queries();
        queried.sort();
        let mut expected = vec![key(1, "nrpc:a"), key(2, "nrpc:a")];
        expected.sort();
        assert_eq!(
            queried, expected,
            "the capability bucket holds both scoped slots, each rebuilt separately"
        );
    }

    /// The 65th family handle is refused without corrupting the first 64, and a
    /// DUPLICATE demand counts toward the bound rather than bypassing it.
    #[test]
    fn the_sixty_fifth_family_handle_is_refused_without_corrupting_the_first_64() {
        let f = fixture();
        let mut held = Vec::new();
        for index in 0..MAX_HANDLES_PER_FAMILY {
            held.push(
                f.registry
                    .demand(FamilyId(1), key(1, &format!("nrpc:f{index}")))
                    .expect("within the bound"),
            );
        }
        assert_eq!(
            f.registry.demand(FamilyId(1), key(1, "nrpc:over")).err(),
            Some(DemandRefused::FamilyAtCapacity)
        );
        // A duplicate of an EXISTING key is still a handle, so it is refused too.
        assert_eq!(
            f.registry.demand(FamilyId(1), key(1, "nrpc:f0")).err(),
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
        let mut family = 1u64;
        for index in 0..MAX_NODE_SLOTS {
            held.push(
                f.registry
                    .demand(FamilyId(family), key(1, &format!("nrpc:n{index}")))
                    .expect("within the node bound"),
            );
            if held.len() % MAX_HANDLES_PER_FAMILY == 0 {
                family += 1;
            }
        }
        assert_eq!(f.registry.retained_slots(), MAX_NODE_SLOTS);

        let beyond = key(1, "nrpc:beyond");
        assert_eq!(
            f.registry.demand(FamilyId(9999), beyond.clone()).err(),
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

    /// Dropping one of several references preserves the slot; dropping the LAST
    /// retires it.
    #[test]
    fn only_the_last_reference_retires_the_slot() {
        let f = fixture();
        let a = f.registry.demand(FamilyId(1), key(1, "nrpc:a")).expect("a");
        let b = f.registry.demand(FamilyId(2), key(1, "nrpc:a")).expect("b");
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
    /// it: the install is discarded, the LIVE incarnation is re-queued, and the
    /// pass reports `Superseded` so its wake is owed.
    #[test]
    fn a_late_build_cannot_resurrect_a_replaced_slot_incarnation() {
        let f = fixture();
        let target = key(1, "nrpc:a");
        let held = f.registry.demand(FamilyId(1), target.clone()).expect("a");

        // Mid-build: retire and immediately re-demand, replacing the incarnation.
        let registry = f.registry.clone();
        let retarget = target.clone();
        *f.source.during_query.lock() = Some(Box::new(move || {
            drop(std::mem::take(&mut *registry.inner.lock()));
            // Rebuild the slot under a NEW incarnation, as retire+re-demand would.
            let mut inner = registry.inner.lock();
            inner.next_incarnation = 99;
            inner.slots.insert(
                retarget.clone(),
                Slot {
                    incarnation: 99,
                    refs: 1,
                    facts: None,
                },
            );
        }));

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
        assert_eq!(
            f.registry.pending_slots(),
            1,
            "the live incarnation still owes work, queued authoritatively"
        );
        drop(held);
    }

    /// An install from a SUPERSEDED actor incarnation is discarded.
    #[test]
    fn an_install_from_a_dead_actor_incarnation_is_discarded() {
        let f = fixture();
        let target = key(1, "nrpc:a");
        let _held = f.registry.demand(FamilyId(1), target.clone()).expect("a");

        // A newer actor takes over while the old one is building.
        let registry = f.registry.clone();
        *f.source.during_query.lock() = Some(Box::new(move || {
            registry.live_actor.store(2, Ordering::Release);
        }));

        f.registry.apply(1, request(true, DirtyCapabilities::Clean));
        assert!(
            f.registry.base_facts(&target).is_none(),
            "a dead incarnation's work is never installed"
        );
        assert_eq!(f.metrics.discarded_obsolete(), 1);
    }

    /// `Caps(C)` touches only C's indexed slots; `RebuildAll` touches every
    /// retained slot; an undemanded capability is never projected.
    #[test]
    fn caps_touches_only_its_bucket_and_rebuild_all_touches_every_slot() {
        let f = fixture();
        let _a = f.registry.demand(FamilyId(1), key(1, "nrpc:a")).expect("a");
        let _b = f.registry.demand(FamilyId(1), key(1, "nrpc:b")).expect("b");
        f.registry.apply(1, request(true, DirtyCapabilities::Clean));

        f.source.reset();
        f.registry.apply(
            1,
            request(
                false,
                DirtyCapabilities::Caps(
                    [CapabilityAuthorityId::for_tag("nrpc:b")]
                        .into_iter()
                        .collect(),
                ),
            ),
        );
        assert_eq!(f.source.queries(), vec![key(1, "nrpc:b")]);

        f.source.reset();
        f.registry
            .apply(1, request(false, DirtyCapabilities::RebuildAll));
        assert_eq!(f.source.queries().len(), 2, "every retained slot");

        f.source.reset();
        f.registry.apply(
            1,
            request(
                false,
                DirtyCapabilities::Caps(
                    [CapabilityAuthorityId::for_tag("nrpc:absent")]
                        .into_iter()
                        .collect(),
                ),
            ),
        );
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
        let _held = f.registry.demand(FamilyId(1), target.clone()).expect("a");
        assert_eq!(
            f.registry.pending_slots(),
            1,
            "registry work owes this slot"
        );

        f.source.reset();
        f.registry.apply(
            1,
            request(
                true,
                DirtyCapabilities::Caps(
                    [CapabilityAuthorityId::for_tag("nrpc:a")]
                        .into_iter()
                        .collect(),
                ),
            ),
        );
        assert_eq!(
            f.source.queries(),
            vec![target],
            "named by both domains, built once"
        );
    }

    /// Source movement during the build rejects the stale installation and
    /// re-queues.
    #[test]
    fn source_movement_during_a_build_rejects_the_stale_installation() {
        let f = fixture();
        let target = key(1, "nrpc:a");
        let _held = f.registry.demand(FamilyId(1), target.clone()).expect("a");
        f.source.move_during_query.store(true, Ordering::Release);

        let outcome = f.registry.apply(1, request(true, DirtyCapabilities::Clean));
        assert_eq!(outcome, ApplyOutcome::Superseded);
        assert!(f.registry.base_facts(&target).is_none());
        assert_eq!(f.registry.pending_slots(), 1, "re-queued authoritatively");

        f.source.move_during_query.store(false, Ordering::Release);
        let outcome = f.registry.apply(1, request(true, DirtyCapabilities::Clean));
        assert!(matches!(outcome, ApplyOutcome::Current { .. }));
        assert!(f.registry.base_facts(&target).is_some());
    }

    /// One quantum leaves the remainder queued AUTHORITATIVELY, and a further pass
    /// finishes it.
    #[test]
    fn one_quantum_leaves_the_remainder_queued() {
        let f = fixture();
        let mut held = Vec::new();
        let mut family = 1u64;
        for index in 0..(APPLY_QUANTUM + 10) {
            held.push(
                f.registry
                    .demand(FamilyId(family), key(1, &format!("nrpc:q{index}")))
                    .expect("demanded"),
            );
            if held.len() % MAX_HANDLES_PER_FAMILY == 0 {
                family += 1;
            }
        }

        f.source.reset();
        f.registry
            .apply(1, request(false, DirtyCapabilities::RebuildAll));
        assert_eq!(
            f.source.queries().len(),
            APPLY_QUANTUM,
            "at most one quantum per synchronous application"
        );
        assert_eq!(
            f.registry.pending_slots(),
            10,
            "the remainder lives in the registry, not in the wake flag"
        );

        f.source.reset();
        f.registry.apply(1, request(true, DirtyCapabilities::Clean));
        assert_eq!(f.source.queries().len(), 10);
        assert_eq!(f.registry.pending_slots(), 0);
    }

    /// No registry lock is held while the source query/build seam executes —
    /// asserted from inside the query, which would deadlock if one were.
    #[test]
    fn no_registry_lock_is_held_across_the_source_query() {
        let f = fixture();
        let _held = f.registry.demand(FamilyId(1), key(1, "nrpc:a")).expect("a");
        f.registry.apply(1, request(true, DirtyCapabilities::Clean));
        assert_eq!(f.source.queries(), vec![key(1, "nrpc:a")]);
    }
}
