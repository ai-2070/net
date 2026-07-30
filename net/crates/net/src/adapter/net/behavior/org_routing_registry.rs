//! OLB-2B-E3b: the bounded node routing registry — the actor's real consumer.
//!
//! Retains one node slot per AUTHORITY-SCOPED key, shared by every clone family
//! demanding that same key, and rebuilds slots from the private-discovery source
//! in bounded quanta. It implements `DirtyApply`, so the supervised actor from
//! E2 drives it and nothing else does.
//!
//! Three authoritative structures, deliberately separate:
//!
//! - `RegistryWork` answers only "is SOME work owed?" — a coalescing wake hint
//!   (Kyra OLB-2B-E3a). The Boolean must never become the work queue;
//! - `RegistryInner::pending` holds the exact slot identities that owe work;
//! - `RegistryInner::live_actor` holds the ONE incarnation currently permitted
//!   to consume that work. It is bound to the actor LIFECYCLE via
//!   `DirtyApply::activate_incarnation` / `DirtyApply::deactivate_incarnation`,
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

use arc_swap::ArcSwapOption;
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
    #[allow(dead_code)] // demand surface; see the note above `RoutingFamily`.
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
    #[allow(dead_code)]
    FamilyAtCapacity,
    /// The node already retains `MAX_NODE_SLOTS` distinct slots. A live slot is
    /// NEVER evicted to satisfy new demand.
    #[allow(dead_code)]
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
    #[allow(dead_code)] // read by the warmed-call consumer.
    pub providers: SourceFacts,
    /// The WHOLE source epoch these facts were built and committed under.
    pub epoch: SourceEpoch,
    /// The EXACT scope authority these facts were reconstructed under
    /// (OLB-2B.3c-pre). Per-key: never the batch-wide Grant vector, or unrelated
    /// Grant movement would invalidate Owner and unrelated Grant facts.
    pub authority: ScopedDiscoveryAuthorityStamp,
    pub actor_incarnation: u64,
    pub slot_incarnation: u64,
    /// The earliest `expires_at` across the retained providers, or `u64::MAX`
    /// when nothing here can expire.
    ///
    /// Expiry filtering at capture is not enough: reconstruction and the commit
    /// can both cross a deadline, and the exact-expiry timer is asynchronous and
    /// may itself be waiting on the publication gate. Carrying the bound lets the
    /// READ seam reject the whole object the moment it is crossed, so the timer
    /// governs promptness rather than correctness — which is what the scoped
    /// store's uncached reads already guarantee (Kyra OLB-2B-E3c).
    pub earliest_expiry: u64,
}

/// The whole authoritative source epoch a fact was built under.
///
/// NOT merely the scoped revision. Routing material is filtered by revocation
/// authority, so two facts built under different revocation authorities are from
/// different epochs even when the scoped revision is identical — a store swap
/// that retracts nothing moves no scoped revision at all. Stamping only the
/// scoped half let a multi-quantum recapture combine facts from two authorities
/// and settle `Current` over them (Kyra OLB-2B-E3c).
///
/// Every use of "generation" in this module means THIS: what facts are stamped
/// with, what recapture coherence compares, and what settlement reports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SourceEpoch {
    /// The scoped query-visible revision.
    pub generation: u64,
    /// The routing AUTHORITY epoch — monotone across revocation store installs
    /// and floor movement, allocated by the node under its authority gate. Never
    /// reused, so it cannot alias a previous authority.
    pub authority: u64,
    /// The revocation store's barriered floor generation.
    ///
    /// Separate from `authority` because it moves INDEPENDENTLY: a floor
    /// publication becomes authoritative inside the revocation store before the
    /// subscriber that advances `authority` is even notified. Retaining it means
    /// facts built against floors F0 are detectably stale once F1 is live, even
    /// during that notification gap (Kyra OLB-2B-E3c).
    pub floor_generation: u64,
    /// Whether the revocation authority was DURABILITY-POISONED when the facts
    /// were built.
    ///
    /// Carried rather than re-read live, because both directions matter and only
    /// the comparison distinguishes them. Poison rising invalidates facts built
    /// without it. Poison CLEARING invalidates facts built with it — a recovery
    /// republishes the same durable view, so it raises no floor, and facts
    /// reconstructed as `Unserved` under poison would otherwise stay reconciled
    /// forever. Reading live poison as an unconditional staleness trigger would
    /// instead churn every read under STEADY poison, which is why it is an
    /// epoch field and not a predicate (Kyra OLB-2B-E3c closure).
    pub poisoned: bool,
}

/// The EXACT authority under which one scope's rows are query-visible
/// (OLB-2B.3c-pre).
///
/// Per-SLOT, never per-batch. The batch `SourceToken` protects the
/// capture/commit transaction; this protects one cached artifact. Copying a
/// batch-wide value into every `SlotBaseFacts` would make unrelated Grant
/// movement invalidate Owner and unrelated Grant facts — exactly what W-G8
/// forbids.
///
/// For the Grant plane the installed consumer Grant IS authority, not a
/// decryption convenience: the live query admits a row only while the stored
/// grant signature and audience handle equal the CURRENTLY INSTALLED ones, so a
/// cached artifact must compare the same things. All four components are bound:
///
/// - `grant_id` alone is insufficient — remove-then-reinstall, and a DIFFERENT
///   signed grant reusing the id, both leave it equal;
/// - `install_seq` distinguishes remove/reinstall of the same grant. It is the
///   checked, terminal, non-aliasing identity signed at `300e80f6c`; a wrapping
///   one would let an old artifact match a later installation;
/// - `grant_signature` distinguishes a different signed grant under the same id —
///   the signature binds the whole canonical grant;
/// - `audience_handle` mirrors the live query's defense-in-depth check, so the
///   cached path is never weaker than the uncached one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScopedDiscoveryAuthorityStamp {
    /// The owner plane: authority is the node's own org, already covered by the
    /// routing authority epoch and revocation view in [`SourceEpoch`].
    Owner,
    /// The grant plane: exactly one installed consumer Grant.
    Grant {
        grant_id: [u8; 32],
        install_seq: u64,
        grant_signature: [u8; 64],
        audience_handle: [u8; 32],
    },
}

/// One scope's reconstructed facts TOGETHER with the exact authority that
/// produced them (OLB-2B.3c-pre).
///
/// They travel as one value so a caller cannot stamp facts with an authority
/// that did not produce them.
#[derive(Clone, Debug)]
pub(crate) struct ScopedSourceFacts {
    pub facts: SourceFacts,
    pub authority: ScopedDiscoveryAuthorityStamp,
    /// The instant this scope's AUTHORITY stops authorizing anything, or
    /// `u64::MAX` if it carries no deadline of its own (scope item 12).
    ///
    /// Separate from the provider rows' deadlines and folded in beside them,
    /// because the two can disagree in the direction that matters: an installed
    /// Grant with ZERO visible providers yields a row deadline of `u64::MAX`,
    /// so an `earliest_expiry` derived from rows alone would say the artifact
    /// never expires while its authority expires in an hour.
    ///
    /// Boundary convention matches the read seam's `now >= earliest_expiry` and
    /// the grant family's `now >= not_after + skew ⇒ expired`, so this is
    /// exactly `not_after + MAX_TOKEN_CLOCK_SKEW_SECS` — the first instant the
    /// authority is no longer valid, not the last instant it is.
    pub authority_deadline: u64,
}

/// An opaque, comparable summary of EVERY authority input a snapshot was taken
/// under.
///
/// The registry never interprets it — it only carries it from the snapshot to
/// the commit pin and lets the SOURCE decide whether anything moved. That is
/// deliberate: a source's authority is not just its own state. The production
/// source's routing material is filtered by revocation floors, so its token
/// covers the scoped revision AND the revocation store's identity, barriered
/// generation and poison state. A scoped-revision-only check would miss a floor
/// that became authoritative before its retraction reached scoped state, and
/// install facts the very next callback invalidates (Kyra OLB-2B-E3c).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SourceToken(Vec<u64>);

impl SourceToken {
    pub(crate) fn new(words: Vec<u64>) -> Self {
        Self(words)
    }
}

/// Whether the source can speak for a slot's authority scope AT ALL.
///
/// `Unserved` is NOT "zero providers". A source that cannot serve a scope has no
/// evidence about it, and an empty provider list is exact fresh negative
/// evidence — conflating them would let an unsupported scope masquerade as a
/// proven-empty one and drive a caller to a NonViable decision it never earned
/// (Kyra OLB-2B-E3c). A slot with `Unserved` facts is fully reconciled (it owes
/// no work) but reads COLD.
#[derive(Clone, Debug)]
pub(crate) enum SourceFacts {
    Served(Arc<[PrivateCapabilityProvider]>),
    Unserved,
}

/// Bounded material captured from the source for ONE quantum.
///
/// Owns everything reconstruction needs, so decode, sort and projection run with
/// NO source lock and NO registry lock held. That is the whole point of the
/// split: the source-side capture is brief, and the expensive part of a quantum
/// blocks nothing (Kyra OLB-2B-E3b).
pub(crate) trait SourceSnapshot {
    /// The full authority token this material was captured under.
    ///
    /// There is deliberately no separate generation accessor: the generation
    /// facts are STAMPED with comes from the commit pin, which is the only thing
    /// that proved it current.
    fn token(&self) -> SourceToken;

    /// Reconstruct `key`'s authority-scoped facts, with the exact scope
    /// authority that produced them.
    fn providers(&self, key: &SlotKey) -> ScopedSourceFacts;
}

/// A SHORT currentness pin, held only across final validation and installation.
///
/// Its existence proves NO authority input covered by the token has moved since
/// the snapshot, which is what makes a conditional install safe without a
/// two-sample before/after comparison — that comparison leaves a window between
/// the second read and the installation in which the source may move, and facts
/// installed in that window are stale while carrying a generation that says
/// otherwise.
pub(crate) trait SourceCommitPin {
    /// The epoch this pin proves is still current, and holds still through the
    /// conditional installation beneath it.
    fn epoch(&self) -> SourceEpoch;

    /// Re-verify EVERY authority input this pin claims to hold still.
    ///
    /// The pin excludes what the node serializes, but a source may have
    /// authority it publishes through its own synchronization — the production
    /// source's revocation floors and poison publish inside the revocation
    /// store, which no routing gate can exclude. So the pin's guarantee is
    /// completed here rather than assumed: called under the registry lock after
    /// the conditional installation and BEFORE settlement, so an authority that
    /// moved underneath cannot produce a `Current` (Kyra OLB-2B-E3c).
    ///
    /// Run `settle` IF every authority input is still current, as ONE operation
    /// relative to source publication.
    ///
    /// Checking and then settling are two independently interleavable steps: a
    /// publication landing between them makes the settlement false with nothing
    /// left to detect it. That matters here specifically because `Current` is not
    /// a historical observation — it is what causes the supervisor to publish
    /// `Healthy` (Kyra OLB-2B-E3c). The implementation holds the source's
    /// publication barrier across both, so no publication can interleave.
    ///
    /// `None` means authority moved and nothing was settled.
    fn settle_if_current(&self, settle: &mut dyn FnMut() -> ApplyOutcome) -> Option<ApplyOutcome>;
}

/// Supplies authority-scoped provider facts.
///
/// E3c binds this to the scoped discovery state, the mutation-publication gate
/// and the revocation authority. The two-method shape is deliberate: it makes
/// "the gate is a COMMIT pin, never a reconstruction lock" a property of the seam
/// rather than a rule an implementation has to remember. Holding the publication
/// gate across a quantum would block all private-discovery ingest, expiry sweeps
/// and floor mutation behind up to [`APPLY_QUANTUM`] authority-scoped store
/// queries plus their decoding.
pub(crate) trait SlotSource: Send + Sync + 'static {
    /// Briefly capture the exact authority-scoped material for `keys`, plus the
    /// token it was captured under. Every source-side lock is released before
    /// this returns. Called with NO registry lock held.
    fn snapshot(&self, keys: &[SlotKey]) -> Box<dyn SourceSnapshot>;

    /// Acquire the commit pin IF every authority input still matches `expected`.
    ///
    /// `None` means something the reconstruction depended on moved, and the
    /// caller must install nothing. ALWAYS called before the registry lock is
    /// taken, never while holding it — see the module's frozen lock order.
    /// `keys` is the SAME batch the snapshot was taken over.
    ///
    /// The token's Grant half is a vector of the exact installation identities
    /// THIS batch selected, so re-deriving it requires knowing which grants
    /// those were. Passing the keys lets the pin compare like with like instead
    /// of falling back to a global "some Grant moved" bit, which would defeat
    /// every commit pin in flight on unrelated Grant movement (OLB-2B.3c-pre).
    fn pin_if_current(
        &self,
        keys: &[SlotKey],
        expected: &SourceToken,
    ) -> Option<Box<dyn SourceCommitPin + '_>>;

    /// Whether this source can still settle a commit, and if it cannot, whether
    /// that is recoverable (E3c blockers §2, review-pass-3 §1).
    ///
    /// [`SourceLiveness::Live`] for every ORDINARY refusal: poison, floor
    /// movement and store swaps all own a later wake, so a failed pin correctly
    /// re-queues AND re-marks against them.
    fn liveness(&self) -> SourceLiveness {
        SourceLiveness::Live
    }
}

/// Whether a [`SlotSource`] can settle, and — when it cannot — who owns the wake
/// that ends the condition (E3c blockers §2, review-pass-3 §1).
///
/// The distinction exists because the two unsettleable states differ in exactly
/// one way that matters to the work queue: whether a re-queued identity is a
/// promise that can eventually be kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SourceLiveness {
    /// Ordinary operation. A pin can be taken and a settlement can succeed; a
    /// refusal is movement, and movement owns a wake.
    Live,
    /// No pin can settle until an EXTERNAL authority movement retires the
    /// condition — the exhausted publication generation of an installed
    /// revocation store, which a replacement install retires by swapping the
    /// store and advancing the routing epoch.
    ///
    /// Retained work stays OWED in `pending`, because the movement that clears
    /// the condition can and will complete it. Nothing is marked: the actor
    /// parks, and that same movement supplies the wake. Marking here instead is
    /// precisely the yield-paced livelock review-pass-3 §1 names — the folded
    /// `(poisoned, floor_generation)` view is STABLE under exhaustion, so the
    /// pin succeeds on every pass and the settlement refuses on every pass, with
    /// no source movement required at all.
    Fenced,
    /// The identity space itself is spent: no future pin can EVER settle.
    ///
    /// Queued work is a promise that cannot be kept, so it is discarded rather
    /// than re-queued, and the retained facts were already retired at the
    /// transition by [`NodeOrgRoutingRegistry::retire_terminal`].
    Terminal,
}

impl SourceLiveness {
    /// May a refusal against this source re-arm the actor?
    ///
    /// Only ordinary movement owns a wake. Both unsettleable states must park.
    fn may_self_wake(self) -> bool {
        matches!(self, Self::Live)
    }

    /// May a refusal against this source keep the selected identities owed?
    ///
    /// `Fenced` is recoverable, so the queue is preserved across it — losing it
    /// would leave the slots cold with nothing left to rebuild them once the
    /// replacement store lands.
    fn may_requeue(self) -> bool {
        !matches!(self, Self::Terminal)
    }
}

#[derive(Debug)]
struct Slot {
    /// Allocated fresh from the node-wide monotone id space at creation, so a
    /// retired-and-re-demanded slot never reuses an identity and work in flight
    /// for a previous incarnation can never resurrect it.
    incarnation: u64,
    /// Live demand handles across ALL families.
    #[allow(dead_code)]
    refs: usize,
    /// `None` until a recapture installs facts, and cleared the moment anything
    /// invalidates them. `None` IS the deterministic cold outcome.
    ///
    /// An `ArcSwapOption` CELL rather than a plain field (OLB-2B.3): every
    /// MUTATION still happens under the registry lock exactly as before, so the
    /// install/invalidate ordering the E3c closure signed is unchanged. What the
    /// cell adds is a lock-free READ — a `DemandHandle` clones this `Arc` at
    /// demand time, so the warmed path loads the artifact with one atomic and
    /// never contends with the actor's quantum. That is the hot-path contract
    /// (plan pins 7/8: the warmed call is an ArcSwap load, not a lock).
    ///
    /// Cell identity is per SLOT INCARNATION: a retired slot's entry is dropped
    /// with the slot, so a handle from a dead incarnation cannot be revived. It
    /// cannot be stale either — a live handle is precisely what stops the slot
    /// being retired.
    facts: Arc<ArcSwapOption<SlotBaseFacts>>,
}

#[derive(Default)]
struct RegistryInner {
    slots: BTreeMap<SlotKey, Slot>,
    /// Capability → retained slot identities, so `Caps(C)` touches only C's
    /// buckets. One capability can back several slots (one per audience scope).
    slots_by_capability: BTreeMap<CapabilityAuthorityId, BTreeSet<SlotKey>>,
    /// Per family, its handle count per key. Bounds HANDLES, not distinct keys, so
    /// duplicate demand cannot bypass the bound.
    #[allow(dead_code)]
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
    /// The last slot identity a quantum served, so the next one resumes strictly
    /// after it and wraps (review-pass-3 §9). Fair rotation over `pending`
    /// rather than "the first `APPLY_QUANTUM` in key order, every pass".
    select_cursor: Option<SlotKey>,
}

impl RegistryInner {
    #[allow(dead_code)]
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
            .is_some_and(|slot| slot.facts.swap(None).is_some())
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
    fn incoherent_with(&self, incarnation: u64, epoch: SourceEpoch) -> Vec<SlotKey> {
        self.slots
            .iter()
            .filter(|(_, slot)| {
                !slot.facts.load().as_ref().is_some_and(|facts| {
                    facts.actor_incarnation == incarnation
                        && facts.slot_incarnation == slot.incarnation
                        && facts.epoch == epoch
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
    /// Passes whose commit pin was held by a LIVE actor and whose settlement was
    /// still refused, because the source's own publication moved underneath it
    /// (review-pass-3 §8). Distinct from `stale_actor_rejections`, which is actor
    /// LIFECYCLE churn: conflating them steers an operator at supervisor restarts
    /// during what is actually revocation-publication pressure.
    settlements_refused: AtomicU64,
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
    pub(crate) fn settlements_refused(&self) -> u64 {
        self.settlements_refused.load(Ordering::Acquire)
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
// ---------------------------------------------------------------------------
// The DEMAND surface.
//
// Everything below has no in-crate production caller, and that is a reviewed
// SCOPE decision rather than a missing consumer: the path that holds demand
// handles is the warmed-call consumer, which is deliberately outside the OLB-2B
// entry boundary (Kyra). The registry's REAL consumer — the supervised actor
// driving `DirtyApply` — is fully live and node-wired in E3c.
//
// These allows are therefore permanent-until-warmed-calls and item-scoped. They
// are NOT the E1/E2/E3a/E3b module-wide allowances, which are gone.
// ---------------------------------------------------------------------------
#[allow(dead_code)]
#[derive(Clone)]
pub(crate) struct RoutingFamily {
    registry: Arc<NodeOrgRoutingRegistry>,
    id: FamilyId,
}

#[allow(dead_code)]
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
#[allow(dead_code)]
pub(crate) struct DemandHandle {
    registry: Arc<NodeOrgRoutingRegistry>,
    family: FamilyId,
    key: SlotKey,
    /// The slot's publication cell, cloned once at demand time (OLB-2B.3).
    ///
    /// This is what makes the warmed read lock-free: the handle already names
    /// exactly one slot, so the hot path needs no map lookup and no registry
    /// lock — one atomic load and it holds the immutable artifact. Holding the
    /// cell is sound precisely because holding the handle is what prevents the
    /// slot being retired, so the cell can never be a dead incarnation's.
    facts: Arc<ArcSwapOption<SlotBaseFacts>>,
}

impl DemandHandle {
    /// The slot's currently published artifact, or `None` for the cold outcome.
    ///
    /// UNVALIDATED, exactly like its registry-side twin: this returns whatever
    /// is published, and authority revalidation is the NODE seam's job
    /// (`MeshNode::org_routing_base_facts`). Named to say so — the review that
    /// renamed `base_facts` to `base_facts_unvalidated` did it because an
    /// accessor that looks authoritative and is not is a trap, and adding a
    /// lock-free twin would re-lay it under a friendlier name.
    ///
    /// The allow is scoped to this ONE method and names its consumer: the warmed
    /// call path (OLB-2B.3b), which is the next slice. Per the E3c discipline, a
    /// leftover allow here after 2B.3b lands means that consumer never arrived.
    #[allow(dead_code)]
    pub(crate) fn base_facts_unvalidated(&self) -> Option<Arc<SlotBaseFacts>> {
        self.facts.load_full()
    }
}

impl Drop for DemandHandle {
    fn drop(&mut self) {
        self.registry.release(self.family, &self.key);
    }
}

/// One consumer-Grant transition, carried to routing with enough identity to
/// invalidate CONDITIONALLY (OLB-2B.3c-pre items 10/11).
///
/// A bare `grant_id` is not enough, and the gap is a real race rather than a
/// tidiness point: a notification can be delayed arbitrarily between its
/// publication and its registry work, so an OBSOLETE transition can arrive after
/// a newer installation has already been published, rebuilt and warmed. Clearing
/// by id alone destroys that successor.
///
/// ```text
/// A: remove N    publish absence, release gate, [stall]
/// B: install N+1                 publish, notify, actor warms the N+1 artifact
/// A: [resumes]   clear by grant_id  ->  destroys the N+1 artifact
/// ```
///
/// It never resurrects withdrawn authority — the read seam stays fail-closed —
/// but it lets an obsolete transition retire current work, which is the defect
/// class `invalidate_if_stale` already guards one layer up: **a delayed
/// invalidator must not delete a successor.**
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GrantScopeMovement {
    pub grant_id: [u8; 32],
    /// The EXACT audience scope this transition is about. A grant id can name two
    /// live scopes across an audience rotation, and the one that did not move
    /// must not be churned.
    pub audience_handle: [u8; 32],
    /// Every artifact built under an installation identity `<=` this is obsolete;
    /// anything stamped higher is a SUCCESSOR and must survive.
    ///
    /// One number rather than an identity plus a kind, because the two transition
    /// kinds differ only in where the boundary sits:
    ///
    /// - install of `k` supersedes everything BEFORE it — `k - 1`;
    /// - removal of `k` supersedes `k` itself as well — `k`.
    ///
    /// `install_seq` is the checked, terminal, strictly monotone identity signed
    /// at `300e80f6c`, so this comparison cannot alias.
    pub superseded_through: u64,
}

impl GrantScopeMovement {
    /// Does this transition cover `key`'s scope?
    ///
    /// Exact on `(grant_id, audience_handle)`. Deliberately NOT narrowed by
    /// capability: the Grant source answers ANY capability under an installed
    /// `(grant_id, audience_handle)` with `Served(empty)` rather than `Unserved`,
    /// regardless of the grant's own capability scope — verified directly, a slot
    /// for a capability the grant does not cover reconstructs as
    /// `Served(0 providers)` and is stamped `Grant`. So the grant's movement
    /// genuinely affects every capability under its audience scope, and narrowing
    /// here would leave those slots holding a Grant-stamped artifact after
    /// removal and stuck `Unserved` after install — the exact two defects this
    /// edge exists to close, reintroduced for a subset. Narrowing becomes correct
    /// only once the source refuses uncovered capabilities structurally, which is
    /// not in this slice's scope.
    fn covers(&self, key: &SlotKey) -> bool {
        matches!(
            key.scope.scope(),
            CapabilityAudienceScope::Grant {
                grant_id,
                audience_handle,
            } if grant_id == &self.grant_id && audience_handle == &self.audience_handle
        )
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
    #[allow(dead_code)]
    fn demand(
        self: &Arc<Self>,
        family: FamilyId,
        key: SlotKey,
    ) -> Result<DemandHandle, DemandRefused> {
        let mut queued = false;
        let cell = {
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
            // Captured under the SAME lock acquisition that takes the reference,
            // so the cell can only be the one belonging to the incarnation this
            // handle is now keeping alive.
            //
            // The fresh path keeps the cell it INSERTED rather than looking the
            // slot back up, so there is no post-mutation lookup left to fail
            // (review 2026-07-29 §4). What remains is the pre-existing path,
            // where `new_slot` was false under this same lock hold and nothing
            // above removes a slot — and where, unlike before, the impossible
            // branch is reached having mutated NOTHING: no reserved slot with no
            // owner, no orphaned `pending` entry, no `queued` flag whose
            // `work.mark()` the early return would skip.
            let cell = if new_slot {
                // Allocate BEFORE mutating anything, so exhaustion retains nothing.
                let Some(incarnation) = inner.allocate_id() else {
                    self.metrics
                        .refused_id_space_exhausted
                        .fetch_add(1, Ordering::AcqRel);
                    return Err(DemandRefused::IdSpaceExhausted);
                };
                let cell = Arc::new(ArcSwapOption::empty());
                inner.slots.insert(
                    key.clone(),
                    Slot {
                        incarnation,
                        refs: 1,
                        facts: cell.clone(),
                    },
                );
                inner
                    .slots_by_capability
                    .entry(key.capability)
                    .or_default()
                    .insert(key.clone());
                inner.pending.insert(key.clone());
                queued = true;
                cell
            } else {
                let Some(slot) = inner.slots.get_mut(&key) else {
                    // Unreachable. Fail closed rather than fabricating a detached
                    // cell that no install would ever write to — a handle reading
                    // a cell the registry does not own would be permanently,
                    // silently cold. `NodeAtCapacity` is the most conservative of
                    // the three refusals: it retains nothing and tells the caller
                    // to take the cold path.
                    return Err(DemandRefused::NodeAtCapacity);
                };
                slot.refs += 1;
                slot.facts.clone()
            };
            *inner
                .families
                .entry(family)
                .or_default()
                .entry(key.clone())
                .or_insert(0) += 1;
            cell
        };
        if queued {
            // Private discovery did not move, so this is the ONLY thing that will
            // wake the actor for it.
            self.work.mark();
        }
        Ok(DemandHandle {
            registry: self.clone(),
            family,
            key,
            facts: cell,
        })
    }

    /// Release one handle. The LAST reference retires the slot; a re-demand then
    /// allocates a FRESH incarnation, so work in flight cannot resurrect it.
    #[allow(dead_code)]
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

    /// The RAW retained base facts for `key` — retention only, with NO
    /// revalidation whatsoever. `None` is the deterministic UNRETAINED outcome.
    ///
    /// **Not a read seam. Production callers want `MeshNode::org_routing_base_facts`**
    /// (`mesh.rs`), which is the validated twin: it re-checks the live authority
    /// epoch, the revocation floor generation, the poison bit, terminal
    /// exhaustion, `Unserved`, wall-clock expiry and the incarnation fence, and
    /// retires the artifact when any of them says no. NONE of that lives here.
    ///
    /// Named for what it is rather than left as the more discoverable of two
    /// similarly-named accessors (review-pass-3 §7): the code that holds a
    /// registry handle would otherwise reach for this one by default, and every
    /// check above is a security property the warmed-call consumer must not be
    /// able to skip by accident. Crate-private, and its remaining callers are
    /// witnesses asserting on RETENTION specifically — several of them precisely
    /// to prove that the seam fences something the registry still holds.
    pub(crate) fn base_facts_unvalidated(&self, key: &SlotKey) -> Option<Arc<SlotBaseFacts>> {
        self.inner.lock().slots.get(key)?.facts.load_full()
    }

    /// Authority moved to `live`: drop every fact stamped with an OLDER
    /// authority, re-queue those slots and wake.
    ///
    /// Called by the node when revocation authority changes. Authority movement
    /// need not touch scoped state at all — a store swap that retracts nothing
    /// advances no scoped revision and publishes no scoped wake — so without this
    /// the actor would never learn that everything it holds was built under an
    /// authority that no longer applies (Kyra OLB-2B-E3c).
    ///
    /// CONDITIONAL on the stamped epoch rather than unconditional. The publisher
    /// releases the authority gate before calling this, so a reconciliation under
    /// the NEW authority can install valid facts in between; wiping everything
    /// would delete them and re-queue work that was already done, turning a
    /// returned `Current` into immediately-owed work. Epochs are monotone, so
    /// `< live` is exact and cannot match a successor.
    pub(crate) fn invalidate_authority_older_than(&self, live: u64) {
        let owed = {
            let mut inner = self.inner.lock();
            let stale: Vec<SlotKey> = inner
                .slots
                .iter()
                .filter(|(_, slot)| {
                    slot.facts
                        .load()
                        .as_ref()
                        .is_some_and(|facts| facts.epoch.authority < live)
                })
                .map(|(key, _)| key.clone())
                .collect();
            let mut invalidated = 0;
            for key in stale {
                if inner.invalidate(&key) {
                    invalidated += 1;
                }
                inner.pending.insert(key);
            }
            self.metrics
                .facts_invalidated
                .fetch_add(invalidated, Ordering::AcqRel);
            !inner.pending.is_empty()
        };
        if owed {
            self.work.mark();
        }
    }

    /// One slot proved unusable at READ time: drop THOSE facts, re-queue, wake.
    ///
    /// Conditional on the exact artifact the reader observed. A reader can be
    /// arbitrarily delayed between loading facts and deciding they are stale, and
    /// reconciliation may have installed a current replacement in the meantime —
    /// an unconditional removal would delete that replacement and re-queue work
    /// that was already done (Kyra OLB-2B-E3c). `Arc::ptr_eq` is exact here
    /// because installation always allocates a fresh `Arc`.
    pub(crate) fn invalidate_if_stale(&self, key: &SlotKey, observed: &Arc<SlotBaseFacts>) {
        let owed = {
            let mut inner = self.inner.lock();
            let Some(slot) = inner.slots.get_mut(key) else {
                return;
            };
            let still_observed = slot
                .facts
                .load()
                .as_ref()
                .is_some_and(|live| Arc::ptr_eq(live, observed));
            if !still_observed {
                // Someone installed a newer artifact. Leave it alone.
                return;
            }
            slot.facts.store(None);
            self.metrics
                .facts_invalidated
                .fetch_add(1, Ordering::AcqRel);
            inner.pending.insert(key.clone());
            true
        };
        if owed {
            self.work.mark();
        }
    }

    /// A consumer Grant moved: retire every retained artifact of that exact
    /// audience scope which the transition SUPERSEDES, re-queue exactly those
    /// slots, and wake the actor (items 10/11).
    ///
    /// Returns the number of artifacts retired.
    ///
    /// **Conditional, and decided under the registry lock.** Each slot is judged
    /// against the artifact it actually holds, at the mutation boundary — not
    /// against a snapshot read before the lock. A pre-lock "is this still
    /// current?" check would be insufficient on its own: a newer publication can
    /// land between the check and the clear, and the decision to preserve a
    /// successor has to still be true when the clear happens.
    ///
    /// A slot that is retained but holds NO artifact is still re-queued — that is
    /// the whole install direction. A Grant-scoped slot reconstructed `Unserved`
    /// holds an `Owner`-stamped artifact, which names no installation at all and
    /// therefore cannot be a successor; it is retired and rebuilt. Only a
    /// `Grant`-stamped artifact from a LATER installation is preserved.
    ///
    /// **Not keyed on `ScopedDiscoveryState::revision`** (item 11, design §2A.2).
    /// A consumer-Grant transition mutates the grant registry, not the scoped
    /// store, so the scoped revision does not move and cannot be the trigger.
    ///
    /// The caller must NOT hold the consumer-Grant gate: the commit pin takes
    /// that gate and then this lock, so acquiring this beneath it would invert
    /// the frozen order. Item 10 states the same thing as an ordering rule.
    pub(crate) fn invalidate_grant_scope(&self, movement: &GrantScopeMovement) -> u64 {
        let (retired, owed) = {
            let mut inner = self.inner.lock();
            let affected: Vec<SlotKey> = inner
                .slots
                .keys()
                .filter(|key| movement.covers(key))
                .cloned()
                .collect();
            let mut retired = 0u64;
            for key in affected {
                let Some(cell) = inner.slots.get(&key).map(|slot| slot.facts.clone()) else {
                    continue;
                };
                let superseded = match cell.load().as_ref() {
                    // Nothing to preserve, and the slot still owes work.
                    None => true,
                    Some(facts) => match facts.authority {
                        ScopedDiscoveryAuthorityStamp::Grant { install_seq, .. } => {
                            install_seq <= movement.superseded_through
                        }
                        // An `Unserved` reconstruction names NO installation, so
                        // it cannot be the successor this transition must spare.
                        ScopedDiscoveryAuthorityStamp::Owner => true,
                    },
                };
                if !superseded {
                    // A LATER installation already published here. Leave it
                    // entirely alone — no clear, and no re-queue either.
                    continue;
                }
                if cell.swap(None).is_some() {
                    retired += 1;
                }
                inner.pending.insert(key);
            }
            self.metrics
                .facts_invalidated
                .fetch_add(retired, Ordering::AcqRel);
            (retired, !inner.pending.is_empty())
        };
        if owed {
            self.work.mark();
        }
        retired
    }

    /// The authority epoch space is terminally exhausted: retire every retained
    /// fact and every queued identity, and do NOT wake the actor (E3c blockers
    /// §2). Called exactly once, at the `NewlyExhausted` transition.
    ///
    /// Two ways this deliberately differs from
    /// [`Self::invalidate_authority_older_than`]:
    ///
    /// - it is UNCONDITIONAL on the stamped epoch. Facts stamped
    ///   `authority == u64::MAX` are exactly the ones a strictly-older
    ///   comparison structurally spares — yet their stamp is the identity that
    ///   just became unable to prove currentness, so leaving them retained
    ///   keeps them readable for as long as no reader happens by;
    /// - NOTHING is re-queued and no work is marked. The source refuses every
    ///   commit pin from now on, so a rebuilt fact could never be installed —
    ///   a re-queue is a promise of work that cannot complete, and the wake
    ///   that re-arms the actor would have it spin on `Superseded` until
    ///   shutdown.
    pub(crate) fn retire_terminal(&self) {
        let mut inner = self.inner.lock();
        let mut invalidated = 0u64;
        for slot in inner.slots.values_mut() {
            if slot.facts.swap(None).is_some() {
                invalidated += 1;
            }
        }
        inner.pending.clear();
        inner.recapture_open = false;
        self.metrics
            .facts_invalidated
            .fetch_add(invalidated, Ordering::AcqRel);
    }

    /// The EARLIEST deadline any retained artifact carries, or `None` if none
    /// carries one (scope item 12).
    ///
    /// This is what the actor arms its expiry wake to. `u64::MAX` means "no
    /// deadline" and is deliberately excluded rather than returned — an
    /// artifact with no deadline must not produce a wake at the end of time,
    /// and `Option` says that where a sentinel would invite arithmetic on it.
    pub(crate) fn next_artifact_deadline(&self) -> Option<u64> {
        self.inner
            .lock()
            .slots
            .values()
            .filter_map(|slot| slot.facts.load().as_ref().map(|f| f.earliest_expiry))
            .filter(|deadline| *deadline != u64::MAX)
            .min()
    }

    /// Retire every retained artifact whose deadline `now_secs` has reached, and
    /// RE-QUEUE its slot so the actor rebuilds it (scope item 12).
    ///
    /// The read seam already refuses an expired artifact, so this is not what
    /// makes expiry safe — it is what makes it PROMPT, and what makes the
    /// registry's retained set agree with what a reader would be told. Without
    /// it, an artifact whose authority expired sits retained until some reader
    /// happens by; with zero provider rows that reader is the only thing that
    /// could ever retire it, because nothing else in the artifact carries a
    /// deadline at all.
    ///
    /// Re-queuing is what closes the loop: the rebuilt reconstruction finds the
    /// authority gone and installs `Unserved`, whose deadline is `u64::MAX`, so
    /// the next arm finds nothing and the actor parks. That is also why this
    /// cannot spin — a retired deadline is not reinstalled.
    pub(crate) fn retire_expired(&self, now_secs: u64) -> u64 {
        let mut inner = self.inner.lock();
        let expired: Vec<SlotKey> = inner
            .slots
            .iter()
            .filter(|(_, slot)| {
                slot.facts
                    .load()
                    .as_ref()
                    .is_some_and(|f| now_secs >= f.earliest_expiry)
            })
            .map(|(key, _)| key.clone())
            .collect();
        let mut retired = 0u64;
        for key in expired {
            if let Some(slot) = inner.slots.get_mut(&key) {
                if slot.facts.swap(None).is_some() {
                    retired += 1;
                }
            }
            inner.pending.insert(key);
        }
        self.metrics
            .facts_invalidated
            .fetch_add(retired, Ordering::AcqRel);
        retired
    }

    /// Test-only: drop `key`'s facts and re-queue it, so a witness can drive
    /// another quantum over the same slot.
    #[cfg(test)]
    pub(crate) fn invalidate_for_test(&self, key: &SlotKey) {
        let mut inner = self.inner.lock();
        inner.invalidate(key);
        inner.pending.insert(key.clone());
    }

    /// Test-only: install `facts` for `key`, creating the slot if needed.
    ///
    /// Lets a witness reproduce a quantum that raced a deadline — facts valid at
    /// capture, expired by the time they are read — without having to win that
    /// race against the wall clock.
    #[cfg(test)]
    pub(crate) fn install_facts_for_test(&self, key: SlotKey, facts: Arc<SlotBaseFacts>) {
        let mut inner = self.inner.lock();
        let slot = inner.slots.entry(key).or_insert(Slot {
            incarnation: 1,
            refs: 1,
            facts: Arc::new(ArcSwapOption::empty()),
        });
        slot.facts.store(Some(facts));
    }

    pub(crate) fn retained_slots(&self) -> usize {
        self.inner.lock().slots.len()
    }

    pub(crate) fn pending_slots(&self) -> usize {
        self.inner.lock().pending.len()
    }
}

impl NodeOrgRoutingRegistry {
    /// Re-arm the actor for a refusal that was genuine source MOVEMENT, and only
    /// then (E3c blockers §3, review-pass-3 §2).
    ///
    /// The suppression is not an optimisation. A `Fenced` or `Terminal` source
    /// refuses without anything having moved and will refuse the next pass
    /// identically, so marking against it is a self-sustaining wake — the exact
    /// livelock the exhaustion fences exist to prevent. Movement, by contrast,
    /// is finite and the pass that follows it can succeed.
    fn mark_if_movement(&self) {
        if self.source.liveness().may_self_wake() {
            self.work.mark();
        }
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

    fn next_deadline(&self) -> Option<u64> {
        self.next_artifact_deadline()
    }

    fn retire_expired(&self, now_secs: u64) -> u64 {
        NodeOrgRoutingRegistry::retire_expired(self, now_secs)
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

            // ROTATED, not key-ordered (review-pass-3 §9). Taking the first
            // `APPLY_QUANTUM` in `(scope, capability)` order every pass starves a
            // subset under sustained churn: if each pass re-dirties at least a
            // quantum's worth of slots whose keys sort BELOW a victim's, the
            // victim is re-outsorted every pass and never rebuilds — cold reads
            // and permanent `pending` occupancy for as long as the churn lasts,
            // even though every pass settles. Distinct from the whole-recapture
            // starvation in review-pass-2 §2, which is why the epoch fix does not
            // cover it.
            //
            // The cursor resumes strictly AFTER the last key served, wrapping
            // once. That is fair by construction — a slot the cursor has passed
            // cannot be re-served until it comes round again — and it costs
            // nothing on the common path, where `named` fits in one quantum.
            let rotated: Vec<SlotKey> = match &inner.select_cursor {
                Some(cursor) => named
                    .range((
                        std::ops::Bound::Excluded(cursor.clone()),
                        std::ops::Bound::Unbounded,
                    ))
                    .chain(named.range((
                        std::ops::Bound::Unbounded,
                        std::ops::Bound::Included(cursor.clone()),
                    )))
                    .take(APPLY_QUANTUM)
                    .cloned()
                    .collect(),
                None => named.iter().take(APPLY_QUANTUM).cloned().collect(),
            };
            inner.select_cursor = rotated.last().cloned().or(inner.select_cursor.take());
            let mut selected = Vec::with_capacity(rotated.len());
            for key in rotated {
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
            let probe = self.source.snapshot(&[]);
            let token = probe.token();
            drop(probe);
            let Some(commit) = self.source.pin_if_current(&[], &token) else {
                // The refusal is REGISTRY-visible movement, exactly as it is on
                // the non-empty path, and it must be marked for exactly the same
                // reason (E3c blockers §3, review-pass-3 §2). What makes the
                // EMPTY path the dangerous one is that its usual compensator is
                // structurally absent: `invalidate_authority_older_than` marks
                // only when `pending` ends non-empty, which is a guaranteed
                // no-op with zero retained slots — and every production node
                // retains zero slots until the warmed-call consumer lands. An
                // authority-only movement (a store install that retracts
                // nothing, a floor publication raising no scoped floor, a poison
                // mark) advances no scoped watch either, so without this mark the
                // actor parks with `owed_recapture` set and health strands at
                // `Rebuilding` indefinitely on an otherwise idle node.
                self.mark_if_movement();
                return ApplyOutcome::Superseded;
            };
            let epoch = commit.epoch();
            let mut inner = self.inner.lock();

            // Authority is revalidated HERE too, not only on the installing path
            // (Kyra OLB-2B-E3b). The phase-1 check is stale by now: the snapshot
            // above ran off-lock, and the actor can be fenced in that window.
            // Installing nothing is not sufficient grounds to settle — `Current`
            // on a full request is what lets the supervisor publish `Healthy`, so
            // a revoked actor reporting it advertises routes on the authority of
            // an incarnation that no longer exists.
            if inner.live_actor != Some(incarnation) {
                self.metrics
                    .stale_actor_rejections
                    .fetch_add(1, Ordering::AcqRel);
                let owed = !inner.pending.is_empty();
                drop(inner);
                if owed {
                    self.work.mark();
                }
                return ApplyOutcome::Superseded;
            }

            let Some(outcome) = commit
                .settle_if_current(&mut || settle(&mut inner, incarnation, epoch, &self.metrics))
            else {
                drop(inner);
                // Counted, and counted as what it IS (review-pass-3 §8): a
                // settlement refused because the source's own publication moved
                // under the pin. This path used to count nothing at all.
                self.metrics
                    .settlements_refused
                    .fetch_add(1, Ordering::AcqRel);
                // Same obligation as the pin refusal above: authority moved
                // inside the probe→settle window, and with no retained slot the
                // movement's own invalidation cannot wake anyone.
                self.mark_if_movement();
                return ApplyOutcome::Superseded;
            };
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
        let snapshot_token = snapshot.token();

        // --- phase 3: reconstruct holding NOTHING ---
        // Not the registry lock, and not the source's publication gate: this is the
        // expensive part of a quantum, and blocking private-discovery ingest,
        // expiry and floor mutation behind it is exactly what the snapshot/commit
        // split exists to prevent (Kyra OLB-2B-E3b).
        let built: Vec<(SlotKey, u64, ScopedSourceFacts)> = selected
            .iter()
            .map(|(key, slot_incarnation)| {
                (key.clone(), *slot_incarnation, snapshot.providers(key))
            })
            .collect();
        drop(snapshot);

        // --- phase 4: the COMMIT pin, before the registry lock ---
        // The SAME `keys` the snapshot was taken over, not an independently
        // rebuilt copy: `pin_if_current`'s contract is that the two batches are
        // identical, and two separately-constructed vectors is the shape in
        // which that quietly stops being true (review 2026-07-29 §3).
        let Some(commit) = self.source.pin_if_current(&keys, &snapshot_token) else {
            // An UNSETTLEABLE refusal is not movement (E3c blockers §2,
            // review-pass-3 §1): the mark below would spin the actor on
            // `Superseded` until shutdown, because nothing has to move for the
            // next pass to refuse identically. `Terminal` additionally discards
            // the queue — a rebuild could never install again, and
            // `retire_terminal` already cleared everything retained at the
            // transition.
            let liveness = self.source.liveness();
            let mut inner = self.inner.lock();
            if liveness.may_requeue() {
                // Install nothing, and put every still-live selected slot back.
                for (key, slot_incarnation, _) in &built {
                    if inner
                        .slots
                        .get(key)
                        .is_some_and(|slot| slot.incarnation == *slot_incarnation)
                    {
                        inner.pending.insert(key.clone());
                    }
                }
            }
            self.metrics
                .discarded_obsolete
                .fetch_add(built.len() as u64, Ordering::AcqRel);
            drop(inner);
            if liveness.may_self_wake() {
                // The movement itself owns a source-watch wake, but that wake
                // alone carries no `registry_work` flag — and without it the
                // requeued identities would not be unioned into the next pass's
                // targets. Mark.
                self.work.mark();
            }
            return ApplyOutcome::Superseded;
        };
        let epoch = commit.epoch();

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
            for (key, slot_incarnation, facts) in built {
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
                let ScopedSourceFacts {
                    facts,
                    authority,
                    authority_deadline,
                } = facts;
                let row_expiry = match &facts {
                    SourceFacts::Served(providers) => providers
                        .iter()
                        .map(|p| p.expires_at)
                        .min()
                        .unwrap_or(u64::MAX),
                    // Cold at every read anyway; nothing to bound.
                    SourceFacts::Unserved => u64::MAX,
                };
                // The artifact expires at the EARLIER of what its rows say and
                // what its authority says (scope item 12). Rows alone are not
                // enough, and the zero-provider case is why: an installed Grant
                // with no visible providers gives `row_expiry == u64::MAX`, so a
                // rows-only deadline would claim the artifact never expires
                // while the authority behind it expires in an hour — and nothing
                // else in the artifact could ever retire it.
                let earliest_expiry = row_expiry.min(authority_deadline);
                slot.facts.store(Some(Arc::new(SlotBaseFacts {
                    providers: facts,
                    epoch,
                    authority,
                    actor_incarnation: incarnation,
                    slot_incarnation,
                    earliest_expiry,
                })));
                self.metrics.installs.fetch_add(1, Ordering::AcqRel);
            }

            // FINAL complete-vector validation, still under the pin and the
            // registry lock. Authority the source publishes through its own
            // synchronization (revocation floors, poison) can move underneath a
            // held pin, and the facts just written would then be stale. They are
            // already unreadable — the read seam compares the whole stamped
            // epoch — but `Current` is itself load-bearing: it is what lets the
            // supervisor publish `Healthy`. So settle only if nothing moved
            // (Kyra OLB-2B-E3c).
            let settled = commit
                .settle_if_current(&mut || settle(&mut inner, incarnation, epoch, &self.metrics));
            let Some(settled) = settled else {
                // The settlement — not the pin — is where an exhausted store
                // publication generation refuses (review-pass-3 §1). The folded
                // `(poisoned = true, floor_generation = 0)` view is SELF-CONSISTENT
                // across passes, so `pin_if_current` accepts every time and only
                // `matches()` can say no. Re-queueing plus a mark here is therefore
                // a spin that requires no source movement at all: park instead, and
                // let the replacement store install supply the wake.
                let liveness = self.source.liveness();
                if liveness.may_requeue() {
                    for (key, slot_incarnation) in &selected {
                        if inner
                            .slots
                            .get(key)
                            .is_some_and(|slot| slot.incarnation == *slot_incarnation)
                        {
                            inner.pending.insert(key.clone());
                        }
                    }
                }
                // NOT `stale_actor_rejections` (review-pass-3 §8). This branch is
                // reachable only with a LIVE actor — revalidated a few lines above
                // under this same lock — so attributing it to actor lifecycle
                // churn steered operators at supervisor restarts during what is
                // really revocation-publication pressure.
                self.metrics
                    .settlements_refused
                    .fetch_add(1, Ordering::AcqRel);
                drop(inner);
                if liveness.may_self_wake() {
                    self.work.mark();
                }
                return ApplyOutcome::Superseded;
            };

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
    epoch: SourceEpoch,
    metrics: &RegistryMetrics,
) -> ApplyOutcome {
    if inner.recapture_open {
        let mut displaced = 0;
        for key in inner.incoherent_with(incarnation, epoch) {
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
            source_generation: epoch.generation,
        }
    } else {
        ApplyOutcome::Progress {
            source_generation: epoch.generation,
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
        /// Set to make a HELD commit pin report that authority moved underneath
        /// it — the transition no gate can exclude.
        authority_moved: AtomicBool,
        /// Runs during reconstruction, so a witness can move the world mid-build.
        during_build: parking_lot::Mutex<Option<Hook>>,
        /// Runs inside the off-lock snapshot — the window in which the phase-1
        /// authority check goes stale.
        on_snapshot: parking_lot::Mutex<Option<Hook>>,
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
                authority_moved: AtomicBool::new(false),
                during_build: parking_lot::Mutex::new(None),
                on_snapshot: parking_lot::Mutex::new(None),
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
        fn token(&self) -> SourceToken {
            SourceToken::new(vec![self.generation])
        }
        fn providers(&self, key: &SlotKey) -> ScopedSourceFacts {
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
            ScopedSourceFacts {
                facts: SourceFacts::Served(Arc::from(Vec::new())),
                authority: ScopedDiscoveryAuthorityStamp::Owner,
                authority_deadline: u64::MAX,
            }
        }
    }

    struct TestCommitPin<'a> {
        state: Arc<SourceState>,
        generation: parking_lot::MutexGuard<'a, u64>,
    }

    impl SourceCommitPin for TestCommitPin<'_> {
        fn settle_if_current(
            &self,
            settle: &mut dyn FnMut() -> ApplyOutcome,
        ) -> Option<ApplyOutcome> {
            (!self.state.authority_moved.load(Ordering::Acquire)).then(settle)
        }
        fn epoch(&self) -> SourceEpoch {
            SourceEpoch {
                generation: *self.generation,
                authority: 0,
                floor_generation: 0,
                poisoned: false,
            }
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
            let hook = self.0.on_snapshot.lock().take();
            if let Some(hook) = hook {
                hook();
            }
            Box::new(TestSnapshot {
                state: self.0.clone(),
                generation,
                _captured: keys.to_vec(),
            })
        }

        fn pin_if_current(
            &self,
            _keys: &[SlotKey],
            expected: &SourceToken,
        ) -> Option<Box<dyn SourceCommitPin + '_>> {
            self.0.assert_no_registry_lock(
                "the commit pin must be acquired BEFORE the registry lock",
            );
            let generation = self.0.gate.lock();
            if SourceToken::new(vec![*generation]) != *expected {
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
        let facts = f
            .registry
            .base_facts_unvalidated(&key(1, "nrpc:a"))
            .expect("built");
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
            f.registry.base_facts_unvalidated(&beyond).is_none(),
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
        assert!(f
            .registry
            .base_facts_unvalidated(&key(1, "nrpc:a"))
            .is_some());

        drop(a);
        assert_eq!(f.registry.retained_slots(), 1, "one reference remains");
        assert!(f
            .registry
            .base_facts_unvalidated(&key(1, "nrpc:a"))
            .is_some());

        drop(b);
        assert_eq!(
            f.registry.retained_slots(),
            0,
            "the last reference retired it"
        );
        assert_eq!(f.metrics.slots_retired(), 1);
        assert!(f
            .registry
            .base_facts_unvalidated(&key(1, "nrpc:a"))
            .is_none());
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
            f.registry.base_facts_unvalidated(&target).is_none(),
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
        assert!(f
            .registry
            .base_facts_unvalidated(&key(1, "nrpc:a"))
            .is_some());
        assert_eq!(f.registry.pending_slots(), 0);

        f.registry.deactivate_incarnation(1);
        f.registry.activate_incarnation(2);

        assert!(
            f.registry
                .base_facts_unvalidated(&key(1, "nrpc:a"))
                .is_none(),
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

    /// The EMPTY-SELECTION settlement path revalidates authority too.
    ///
    /// Nothing is installed on that path, which is exactly why it is easy to miss —
    /// but `Current` on a full request is what lets the supervisor publish
    /// `Healthy`, so an actor revoked after phase 1 must not reach it. The
    /// revocation lands during the off-lock snapshot, the window in which the
    /// phase-1 check goes stale (Kyra OLB-2B-E3b).
    #[test]
    fn an_actor_revoked_after_selection_cannot_settle_an_empty_pass_as_current() {
        // Baseline: with the actor live, this exact pass settles Current.
        let f = fixture();
        assert_eq!(
            f.registry
                .apply(1, request(false, DirtyCapabilities::RebuildAll)),
            ApplyOutcome::Current {
                source_generation: f.source.generation()
            },
            "a full request over no retained slots completes"
        );

        // The same pass, with the actor fenced inside the off-lock snapshot.
        let f = fixture();
        {
            let registry = f.registry.clone();
            *f.source.on_snapshot.lock() = Some(Box::new(move || {
                registry.deactivate_incarnation(1);
            }));
        }
        let outcome = f
            .registry
            .apply(1, request(false, DirtyCapabilities::RebuildAll));
        assert_eq!(
            outcome,
            ApplyOutcome::Superseded,
            "an actor revoked after phase 1 cannot settle Current"
        );
        assert_eq!(f.metrics.stale_actor_rejections(), 1);

        // And the same holds for an ordinary quiet registry-work pass.
        let f = fixture();
        let target = key(1, "nrpc:a");
        let _held = f.family().demand(target.clone()).expect("a");
        f.registry.apply(1, request(true, DirtyCapabilities::Clean));
        assert_eq!(f.registry.pending_slots(), 0, "nothing left to select");
        {
            let registry = f.registry.clone();
            *f.source.on_snapshot.lock() = Some(Box::new(move || {
                registry.deactivate_incarnation(1);
            }));
        }
        assert_eq!(
            f.registry.apply(1, request(true, DirtyCapabilities::Clean)),
            ApplyOutcome::Superseded
        );
        assert_eq!(f.metrics.stale_actor_rejections(), 1);
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
        assert!(f.registry.base_facts_unvalidated(&c).is_some());
        assert!(f.registry.base_facts_unvalidated(&d).is_some());

        let observed = Arc::new(AtomicBool::new(false));
        {
            let registry = f.registry.clone();
            let observed = observed.clone();
            let c = c.clone();
            let d = d.clone();
            *f.source.during_build.lock() = Some(Box::new(move || {
                assert!(
                    registry.base_facts_unvalidated(&c).is_none(),
                    "C's stale facts must already be gone while C rebuilds"
                );
                assert!(
                    registry.base_facts_unvalidated(&d).is_some(),
                    "D was not named by the delta and keeps its facts"
                );
                observed.store(true, Ordering::Release);
            }));
        }

        f.source.reset();
        f.registry.apply(1, request(false, caps(&["nrpc:c"])));
        assert!(observed.load(Ordering::Acquire), "the hook must have run");
        assert_eq!(f.source.queries(), vec![c.clone()]);
        assert!(
            f.registry.base_facts_unvalidated(&c).is_some(),
            "and is rebuilt after"
        );
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
        assert!(keys
            .iter()
            .all(|k| f.registry.base_facts_unvalidated(k).is_some()));

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
                .filter(|k| f.registry.base_facts_unvalidated(k).is_some())
                .count(),
            APPLY_QUANTUM,
            "EVERY affected slot was invalidated; only the quantum was rebuilt"
        );
        assert_eq!(f.registry.pending_slots(), 8);
    }

    /// review-pass-2 §2 (the >`APPLY_QUANTUM` witness its disposition required) —
    /// a multi-quantum recapture under SUSTAINED source movement loses no
    /// identity, and converges the moment the source settles.
    ///
    /// With more than one quantum's worth of retained slots, a complete recapture
    /// needs ceil(N/64) CONSECUTIVE quanta at one epoch, and a single ingest,
    /// expiry sweep or floor raise between them re-queues what was already built.
    /// The failure this must exclude is not the retrying — that is the design —
    /// but SILENT LOSS: an identity dropped by a refused pass would leave its slot
    /// cold with nothing owed to rebuild it, and the recapture would report
    /// completion over a hole. The actor-side rate control for the same scenario
    /// is `a_sustained_superseded_streak_backs_off_and_reports_degraded`.
    #[test]
    fn sustained_source_movement_across_a_multi_quantum_recapture_loses_no_identity() {
        let f = fixture();
        let mut family = f.family();
        let mut held = Vec::new();
        let mut keys = Vec::new();
        let total = APPLY_QUANTUM + 1;
        for index in 0..total {
            if index > 0 && index % MAX_HANDLES_PER_FAMILY == 0 {
                family = f.family();
            }
            let slot = key(1, &format!("nrpc:m{index:03}"));
            held.push(family.demand(slot.clone()).expect("demanded"));
            keys.push(slot);
        }
        assert_eq!(f.registry.pending_slots(), total);

        // Sustained movement: every pass has the source advance mid-build, so
        // every commit pin refuses.
        for pass in 0..6 {
            let state = f.source.clone();
            *f.source.during_build.lock() = Some(Box::new(move || state.advance()));
            assert_eq!(
                f.registry
                    .apply(1, request(true, DirtyCapabilities::RebuildAll)),
                ApplyOutcome::Superseded,
                "pass {pass}: a pin that refused installs nothing"
            );
            assert_eq!(
                f.registry.pending_slots(),
                total,
                "pass {pass}: EVERY identity stays owed — a dropped one would leave \
                 its slot cold with nothing to rebuild it"
            );
            assert!(
                keys.iter()
                    .all(|k| f.registry.base_facts_unvalidated(k).is_none()),
                "pass {pass}: and nothing obsolete was installed"
            );
        }

        // The source settles: the preserved queue is what lets the recapture
        // finish, across as many quanta as it takes.
        let mut outcome = ApplyOutcome::Superseded;
        for _ in 0..8 {
            outcome = f
                .registry
                .apply(1, request(true, DirtyCapabilities::RebuildAll));
            if matches!(outcome, ApplyOutcome::Current { .. }) {
                break;
            }
        }
        assert!(
            matches!(outcome, ApplyOutcome::Current { .. }),
            "a settled source converges (last outcome {outcome:?})"
        );
        assert_eq!(f.registry.pending_slots(), 0);
        assert!(
            keys.iter()
                .all(|k| f.registry.base_facts_unvalidated(k).is_some()),
            "and every slot the recapture covered is warm — no hole under a Current"
        );
    }

    /// review-pass-3 §9 — sustained low-sorting churn must not starve a
    /// high-sorting pending slot.
    ///
    /// Selection used to take the first `APPLY_QUANTUM` in `(scope, capability)`
    /// order on EVERY pass, with no cursor, FIFO or aging. So if each pass
    /// re-dirties a full quantum of slots whose keys sort below a victim's, the
    /// victim is re-outsorted every pass and never rebuilds — cold reads and
    /// permanent `pending` occupancy for as long as the churn lasts. This is not
    /// review-pass-2 §2's whole-recapture starvation: every pass here settles,
    /// and the loss is confined to a subset.
    #[test]
    fn sustained_low_sorting_churn_cannot_starve_a_high_sorting_slot() {
        let f = fixture();
        let mut family = f.family();
        let mut held = Vec::new();
        let mut entries: Vec<(SlotKey, String)> = Vec::new();
        for index in 0..=APPLY_QUANTUM {
            if index > 0 && index % MAX_HANDLES_PER_FAMILY == 0 {
                family = f.family();
            }
            let tag = format!("nrpc:q{index:03}");
            let slot = key(1, &tag);
            held.push(family.demand(slot.clone()).expect("demanded"));
            entries.push((slot, tag));
        }
        // Capability ids are hashes, so SELECTION order is not tag order — pick
        // the victim by the order the registry actually sorts in.
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        let victim = entries.last().expect("victim").0.clone();
        let churning: Vec<&str> = entries[..APPLY_QUANTUM]
            .iter()
            .map(|(_, tag)| tag.as_str())
            .collect();

        // Warm everything, then cold the victim so it is the one identity owed
        // besides the churn.
        f.registry.apply(1, request(true, DirtyCapabilities::Clean));
        f.registry.apply(1, request(true, DirtyCapabilities::Clean));
        assert!(f.registry.base_facts_unvalidated(&victim).is_some());
        f.registry.invalidate_for_test(&victim);

        let mut served_on = None;
        for pass in 0..4 {
            f.source.reset();
            f.registry.apply(1, request(true, caps(&churning)));
            assert_eq!(
                f.source.queries().len(),
                APPLY_QUANTUM,
                "pass {pass}: the quantum is saturated, so this is genuine contention \
                 rather than a pass with room to spare"
            );
            if f.registry.base_facts_unvalidated(&victim).is_some() {
                served_on = Some(pass);
                break;
            }
        }
        assert!(
            served_on.is_some(),
            "the highest-sorting slot must come round: without rotation the churn \
             below it wins every pass, forever"
        );
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
            let facts = f.registry.base_facts_unvalidated(k).expect("built");
            assert_eq!(facts.actor_incarnation, 1);
            assert_eq!(facts.epoch.generation, generation);
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
                .load()
                .as_ref()
                .is_some_and(|facts| facts.epoch.generation == generation)),
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
                .base_facts_unvalidated(&c)
                .expect("c rebuilt")
                .epoch
                .generation,
            second
        );
        assert_eq!(
            f.registry
                .base_facts_unvalidated(&d)
                .expect("d untouched")
                .epoch
                .generation,
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
                .base_facts_unvalidated(&fresh)
                .expect("built")
                .epoch
                .generation,
            second
        );
        assert_eq!(
            f.registry
                .base_facts_unvalidated(&established)
                .expect("retained")
                .epoch
                .generation,
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
        assert!(f.registry.base_facts_unvalidated(&target).is_none());
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
                assert!(registry.base_facts_unvalidated(&target).is_some());
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
                .base_facts_unvalidated(&target)
                .expect("built")
                .epoch
                .generation,
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
