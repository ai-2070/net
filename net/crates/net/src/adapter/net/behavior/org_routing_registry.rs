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
///
/// Handles, not capabilities. One warmed capability costs one handle per
/// AUTHORITY-SCOPED contributor it needs — Owner plus every DISCOVER audience
/// the family actually leased for it — so a family holding two leased DISCOVER
/// grants per capability warms ~21 capabilities, not 64. The plan's older
/// "64 warmed capabilities per clone family" wording described a bound this
/// registry has never enforced; OLB-2B.3b corrects it there to
/// "64 retained authority-scoped route demands".
pub(crate) const MAX_HANDLES_PER_FAMILY: usize = 64;
/// Max distinct retained node slots.
pub(crate) const MAX_NODE_SLOTS: usize = 256;
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
    /// Where this artifact sits in the consumer-Grant transition order
    /// (OLB-2B.3c-pre step 3). See [`ScopedSourceFacts::grant_fence`].
    pub grant_fence: GrantArtifactFence,
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
    /// Where this reconstruction sits in the consumer-Grant transition order.
    ///
    /// Ordering, not authority — `authority` above is the authority. This exists
    /// so a delayed movement notification can tell whether the artifact it is
    /// about to clear predates or postdates the transition it carries.
    ///
    /// Present on EVERY reconstruction, including `Unserved` ones, which is the
    /// whole point: an absent-Grant reconstruction is stamped `Owner` and so
    /// carries no installation identity, yet it can be the exact successor a
    /// later removal produced.
    pub grant_fence: GrantArtifactFence,
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

/// What giving up one family reference did.
///
/// Three outcomes rather than a bool, because "nothing was given back" and
/// "something was given back and the slot survives" must not collapse: the first
/// is an invariant violation the caller has to refuse on, and the second is the
/// ordinary case.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReleaseOutcome {
    /// The reference was given back and the slot still has owners.
    Released,
    /// The reference was given back and it was the LAST one.
    Retire,
    /// This family holds no such reference. NOTHING was decremented.
    NotHeld,
}

impl RegistryInner {
    #[allow(dead_code)]
    fn family_handles(&self, family: FamilyId) -> usize {
        self.families
            .get(&family)
            .map_or(0, |keys| keys.values().sum())
    }

    /// Give up ONE of `family`'s references to `key`.
    ///
    /// Bookkeeping only: the caller commits the retirement, because retiring
    /// touches the metrics and the capacity generation that live on the registry
    /// rather than on this struct. Shared by the single-key release, the
    /// whole-set release and the replacement transaction, so the three cannot
    /// drift in how a reference is given up.
    ///
    /// **The family reference is what authorizes the global decrement**, and
    /// this helper enforces that itself rather than trusting every present and
    /// future caller. A family that does not hold `key` has nothing to give
    /// back, and decrementing `slot.refs` on its behalf would retire a slot some
    /// OTHER family still owns — leaving that family's map claiming a handle it
    /// can never release, and its set unable to release it. The check is here,
    /// not at the call sites, because a call site can be added; this cannot be
    /// bypassed by adding one.
    fn release_one(&mut self, family: FamilyId, key: &SlotKey) -> ReleaseOutcome {
        let held = self
            .families
            .get(&family)
            .and_then(|keys| keys.get(key))
            .copied()
            .unwrap_or(0);
        if held == 0 {
            return ReleaseOutcome::NotHeld;
        }
        if let Some(keys) = self.families.get_mut(&family) {
            if let Some(count) = keys.get_mut(key) {
                *count -= 1;
                if *count == 0 {
                    keys.remove(key);
                }
            }
            if keys.is_empty() {
                self.families.remove(&family);
            }
        }
        match self.slots.get_mut(key) {
            Some(slot) => {
                slot.refs -= 1;
                if slot.refs == 0 {
                    ReleaseOutcome::Retire
                } else {
                    ReleaseOutcome::Released
                }
            }
            // The family held a reference the slot map does not know about.
            // Unreachable, and NOT silently absorbed: the family entry has been
            // given back above, so the two are consistent again, but the caller
            // is told nothing retired because nothing did.
            None => ReleaseOutcome::Released,
        }
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
    /// Advances every time a retained slot is RETIRED, and only then
    /// (OLB-2B.3b §9).
    ///
    /// `NodeAtCapacity` is the one retryable refusal, and this is what a
    /// refused family gates its retry on: node capacity can only have become
    /// available by a slot going away. Without it a family that missed at the
    /// node bound would re-derive its demand set, re-sort it and re-take the
    /// registry lock on EVERY subsequent cold call for that capability — the
    /// bound is node-wide, so a node at 256 slots would have every family
    /// hammering the one lock the warmed path exists to avoid.
    ///
    /// Deliberately NOT `slots.len()`: a retire-then-demand pair leaves the
    /// length equal while capacity genuinely moved, and a family comparing
    /// lengths would conclude nothing had changed and never retry. It is also
    /// NOT advanced on demand — growth cannot free capacity, so advancing there
    /// would only manufacture pointless retries.
    ///
    /// Read lock-free, mutated under the registry lock. Publishing it AFTER the
    /// retirement is committed (the `Release` half of the `AcqRel`) is what
    /// makes an observer that sees the new generation able to see the freed
    /// slot too.
    node_capacity_generation: AtomicU64,
    /// Advances on EVERY reference given back, retirement or not.
    ///
    /// The node capacity generation is the wake condition for a FRESH
    /// acquisition, and it is exact for that: only a retirement can make room
    /// for a new slot. It is NOT exact for a REPLACEMENT, which can become
    /// satisfiable when another family merely stops SHARING a slot the
    /// replacement wants to give up — the slot survives, no retirement happens,
    /// and the capacity generation stands still while the replacement's
    /// projection has genuinely changed. Gating replacement on the capacity
    /// generation therefore deadlocks it against its own wake condition.
    ///
    /// Read lock-free, mutated under the registry lock, published after the
    /// bookkeeping it describes.
    ref_release_generation: AtomicU64,
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
// OLB-2B.3b moved the boundary one layer, and no further: `org_routing_state`
// now owns this surface, so `demand_set` and `RoutingFamily` have an in-crate
// consumer. That consumer is itself dark, because what reaches it is
// `MeshNode::call` in OLB-2B.3d. The allows below therefore stay, and they stay
// for a reason that is now one hop away rather than two.
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

    /// Acquire a COMPLETE demand set atomically, or nothing (OLB-2B.3b §4.1).
    pub(crate) fn demand_set(&self, keys: Vec<SlotKey>) -> Result<DemandSet, DemandRefused> {
        self.registry.demand_set(self.id, keys)
    }

    /// Handles this family currently holds.
    pub(crate) fn handles(&self) -> usize {
        self.registry.inner.lock().family_handles(self.id)
    }

    /// The node capacity generation a FRESH acquisition's refusals gate on.
    pub(crate) fn node_capacity_generation(&self) -> u64 {
        self.registry.node_capacity_generation()
    }

    /// The reference-release generation a REPLACEMENT's refusals gate on.
    pub(crate) fn ref_release_generation(&self) -> u64 {
        self.registry.ref_release_generation()
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
    /// The allow is scoped to this ONE method and names its consumer: the
    /// FAMILY PROJECTION, which reads the contributing artifacts and narrows
    /// them into one route set. That is OLB-2B.3d, not 2B.3b — 2B.3b's
    /// `CapabilityRouteHandle` OWNS the demand set without reading a single
    /// artifact through it, which is exactly why the allow survived that slice.
    /// Per the E3c discipline, a leftover allow here after 2B.3d lands means
    /// that consumer never arrived.
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

/// A family's COMPLETE demand set for one capability, owned as ONE object.
///
/// The unit of ownership is the SET, not the handle, and that is what makes an
/// atomic replacement expressible: a replacement compares two key sets in one
/// registry transaction and moves the release responsibility across, so the
/// intersection is never released and re-acquired and nothing is ever charged
/// twice. A `Vec<DemandHandle>` cannot express it — each handle releases itself
/// on drop, so transferring one means defusing its `Drop`, and a per-handle
/// defuse is exactly the double-release hazard this type removes.
#[allow(dead_code)] // consumer: the family projection (OLB-2B.3d).
pub(crate) struct DemandSet {
    registry: Arc<NodeOrgRoutingRegistry>,
    family: FamilyId,
    /// What this set NAMES, in deterministic key order.
    ///
    /// Immutable, and read on the lock-free path. Distinct from `held`, which is
    /// what it still OWES a release for; after a replacement transfers the
    /// references away the two differ, and that difference is the whole point.
    keys: Vec<SlotKey>,
    /// Publication cells, parallel to `keys`.
    ///
    /// Immutable and lock-free readable. Holding a cell is sound whatever
    /// happens to the slot: a retired slot's cell simply reads empty, which is
    /// the correct answer for a superseded set.
    cells: Vec<Arc<ArcSwapOption<SlotBaseFacts>>>,
    /// The RELEASE RESPONSIBILITY — the keys this set must still give back.
    ///
    /// Interior-mutable because the responsibility can move to a replacement set
    /// while this one is still shared behind an `Arc`. There is no flag and no
    /// `mem::forget`: the invariant is that a set releases precisely the keys it
    /// contains at drop, and a transfer MOVES them, so at every instant exactly
    /// one set owes each reference.
    held: parking_lot::Mutex<Vec<SlotKey>>,
}

#[allow(dead_code)] // consumer: the family projection (OLB-2B.3d).
impl DemandSet {
    /// The scopes this set names, in deterministic key order.
    pub(crate) fn keys(&self) -> &[SlotKey] {
        &self.keys
    }

    pub(crate) fn len(&self) -> usize {
        self.keys.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// The published artifact for the `index`-th contributor, UNVALIDATED —
    /// exactly like its registry-side twin. Authority revalidation is the NODE
    /// seam's job (`MeshNode::org_routing_base_facts`); the name says so.
    pub(crate) fn base_facts_unvalidated(&self, index: usize) -> Option<Arc<SlotBaseFacts>> {
        self.cells.get(index)?.load_full()
    }

    /// Test-only: what this set still owes a release for. A replacement moves
    /// this to the successor, and a witness that could not see it would have to
    /// infer the transfer from a handle count that a double-release also
    /// produces.
    #[cfg(test)]
    pub(crate) fn held_for_test(&self) -> Vec<SlotKey> {
        self.held.lock().clone()
    }

    /// Atomically REPLACE this set with the one for `new_keys`, charged on the
    /// projected final footprint (§4.3).
    ///
    /// **Rooted in this set's OWN registry and family identity.** It takes no
    /// `RoutingFamily` argument, and that is the security property, not an
    /// ergonomic one: an earlier surface took the replacement authority from the
    /// CALLER, so family B could pass family A's set and have the registry
    /// release A's references under B's id. B's family map holds no such key, so
    /// nothing was given back on B's side while A's slot reference was still
    /// decremented — retiring a slot A owned, stranding a handle in A's map that
    /// A's set could no longer release, and handing B the successor. Cross-family
    /// and cross-registry transfer are now unrepresentable rather than rejected,
    /// because there is no argument through which to express them.
    ///
    /// A set whose responsibility has ALREADY transferred cannot be replaced
    /// again: `held` no longer matches what the set names, and a second
    /// replacement would compute `old_only = ∅` and charge the successor's whole
    /// footprint gross. That is refused as [`ReplaceRefused::Superseded`] with
    /// total no effect.
    pub(crate) fn replace(&self, new_keys: Vec<SlotKey>) -> Result<DemandSet, ReplaceRefused> {
        self.registry
            .clone()
            .replace_demand_set(self.family, self, new_keys)
    }
}

/// Why an atomic replacement did not happen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // consumer: `OrgRoutingState` (OLB-2B.3b).
pub(crate) enum ReplaceRefused {
    /// The registry refused the projected footprint.
    Demand(DemandRefused),
    /// This set has already given its references away, so it is not the current
    /// owner of anything and cannot be the basis of a replacement.
    Superseded,
}

impl Drop for DemandSet {
    fn drop(&mut self) {
        // Whatever is still owed, released as one transaction. After a
        // replacement transferred them this is empty and the drop is a no-op —
        // not because a flag says so, but because the keys are gone.
        let owed = std::mem::take(&mut *self.held.lock());
        if !owed.is_empty() {
            self.registry.release_keys(self.family, &owed);
        }
    }
}

/// Where a reconstruction sits in the consumer-Grant transition order.
///
/// The ARTIFACT side of the fence, distinct from the movement side below.
///
/// `TerminalAbsence` is not "publication `u64::MAX`". A single global terminal
/// generation would be wrong the moment a second Grant scope is still installed:
/// scope A withdraws terminally and sets the global marker, scope B's
/// reconstruction observes the same marker, and B's own later withdrawal can no
/// longer order B's pre-terminal `Served` artifact. Terminal absence is a
/// property of ONE scope's reconstruction, so it lives on the artifact, and
/// still-installed scopes keep an ordinary `Publication` (Kyra, review of
/// `46af3d625`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GrantArtifactFence {
    /// Reconstructed while the publication-identity space was still live, under
    /// the generation it observed.
    Publication(u64),
    /// Reconstructed ABSENT for this exact scope, with the publication-identity
    /// space exhausted. Terminal: no installation can follow, so nothing can
    /// supersede it.
    TerminalAbsence,
}

/// How a consumer-Grant transition orders itself against retained artifacts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GrantMovementFence {
    /// The ordinary case: this transition's publication generation.
    Publication(u64),
    /// The publication-identity space is EXHAUSTED (OLB-2B.3c-pre step 3).
    ///
    /// Reached only by a withdrawal, because an installation at exhaustion is
    /// refused before it publishes anything. With no further installation
    /// publishable, absence is TERMINAL for this scope, so no `Publication`
    /// artifact under it has a successor left to protect — and ordering would be
    /// meaningless anyway, since the generation that would express it cannot be
    /// allocated.
    ///
    /// **This clears every `Publication` artifact in the exact scope; it is NOT
    /// "clear everything".** A terminal movement still preserves
    /// [`GrantArtifactFence::TerminalAbsence`]. Nothing can supersede a terminal
    /// absence — but the artifact THIS withdrawal's own publication produced is
    /// one, and retiring it is the same self-inflicted churn `<=` would cause on
    /// the ordinary row. "Unconditional" describes the comparison against
    /// generations, not the set of artifacts (Kyra, review of `010c718ea`).
    ///
    /// Revocation is never refused for want of an identity. Withdrawing
    /// authority must always be possible; it is the direction that fails closed.
    Terminal,
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
    /// How this transition orders itself against retained artifacts.
    ///
    /// Ordinarily a consumer-Grant PUBLICATION generation: a reconstruction that
    /// observed generation `g` is obsolete with respect to this movement iff
    /// `g < publication`. **Strictly less than** — an artifact from THIS
    /// publication already reflects the transition and must survive it, which is
    /// exactly the "a demand arriving after publication is safe" case design
    /// §2A.2 names.
    ///
    /// **It must order absences as well as installations.** An earlier shape
    /// carried `superseded_through`, derived from `install_seq`, and treated an
    /// `Owner`-stamped artifact on a Grant slot as never-a-successor. That was
    /// the first repair, HELD at `7348529fb`: it is false, and Kyra's
    /// production-path probe demonstrated it — a delayed INSTALL notification
    /// destroyed the newer `Unserved` artifact a later REMOVAL had produced.
    /// `install_seq` totally orders installations, not transitions, and an
    /// absence has no installation identity to be ordered by.
    ///
    /// ```text
    /// install N     publish, [stall before notifying]
    /// (warm)        artifact reconstructed under N
    /// remove N      publish absence, notify -> clears it, re-queues
    /// (rebuild)     newer artifact: Unserved, `Owner`-stamped
    /// install N     [resumes] -> destroyed that newer artifact
    /// ```
    pub fence: GrantMovementFence,
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
            node_capacity_generation: AtomicU64::new(0),
            ref_release_generation: AtomicU64::new(0),
        })
    }

    /// The node capacity generation (OLB-2B.3b §9). See the field.
    pub(crate) fn node_capacity_generation(&self) -> u64 {
        self.node_capacity_generation.load(Ordering::Acquire)
    }

    pub(crate) fn ref_release_generation(&self) -> u64 {
        self.ref_release_generation.load(Ordering::Acquire)
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

    /// Acquire a family's COMPLETE demand set for one capability entry, or
    /// nothing at all (OLB-2B.3b §4.1).
    ///
    /// The all-or-none property is the whole point, and it is a CORRECTNESS
    /// property rather than a tidiness one. A capability's demand set is
    /// `Owner + every DISCOVER audience the family actually leased`, and each
    /// member contributes discovery provenance the final route is built from. A
    /// kept prefix would warm a capability whose Owner plane is retained and
    /// whose Grant plane is not — which does not read as a failure anywhere
    /// downstream. It reads as a route that legitimately found no granted
    /// provider, i.e. a silent authority narrowing presenting as a routing
    /// preference (§4.1).
    ///
    /// So every refusal is decided BEFORE anything is mutated, and after the
    /// last refusal the loop cannot fail:
    ///
    /// ```text
    /// sort + deduplicate
    /// → ONE registry acquisition
    ///     → family capacity for the WHOLE set
    ///     → node capacity for every NEW distinct slot
    ///     → reserve every required incarnation, contiguously, no wrap
    ///     → retain all references
    /// → mark work once
    /// ```
    ///
    /// Deduplicating first is not cosmetic either: a repeated key would reserve
    /// two handles against the family bound and count two new slots against the
    /// node bound, so a set that fits could be refused for capacity it never
    /// needed.
    ///
    /// This is NOT `demand` in a loop. That version consumes an identity per
    /// new slot before it discovers the refusal, retires those slots when the
    /// caller drops the prefix, and leaves the node's monotone id space
    /// permanently advanced by a call that retained nothing.
    #[allow(dead_code)] // consumer: `OrgRoutingState` (OLB-2B.3b), still dark.
    fn demand_set(
        self: &Arc<Self>,
        family: FamilyId,
        keys: Vec<SlotKey>,
    ) -> Result<DemandSet, DemandRefused> {
        let mut keys = keys;
        keys.sort();
        keys.dedup();

        let mut queued = false;
        let cells = {
            let mut inner = self.inner.lock();

            // Family capacity, for the whole set at once.
            if inner.family_handles(family) + keys.len() > MAX_HANDLES_PER_FAMILY {
                self.metrics
                    .refused_family_at_capacity
                    .fetch_add(1, Ordering::AcqRel);
                return Err(DemandRefused::FamilyAtCapacity);
            }

            // Node capacity, counting only slots this set would CREATE. Keys
            // already retained by some family cost no node capacity, exactly as
            // in the single-key path — a live slot is never evicted.
            let new_slots = keys
                .iter()
                .filter(|key| !inner.slots.contains_key(key))
                .count();
            if inner.slots.len() + new_slots > MAX_NODE_SLOTS {
                self.metrics
                    .refused_node_at_capacity
                    .fetch_add(1, Ordering::AcqRel);
                return Err(DemandRefused::NodeAtCapacity);
            }

            // Reserve every incarnation this set needs as ONE contiguous block,
            // checked, before the first mutation. `allocate_id` per slot inside
            // the loop would be equivalent only if the loop could not fail —
            // and the reason it cannot fail is precisely this reservation.
            let Some(reserved_through) = inner.next_id.checked_add(new_slots as u64) else {
                self.metrics
                    .refused_id_space_exhausted
                    .fetch_add(1, Ordering::AcqRel);
                return Err(DemandRefused::IdSpaceExhausted);
            };
            let mut next_incarnation = inner.next_id;
            inner.next_id = reserved_through;

            // Every refusal is above this line. Nothing below can fail, so
            // nothing below needs an unwind path.
            let mut cells = Vec::with_capacity(keys.len());
            for key in &keys {
                let cell = match inner.slots.get_mut(key) {
                    Some(slot) => {
                        slot.refs += 1;
                        slot.facts.clone()
                    }
                    None => {
                        next_incarnation += 1;
                        let cell = Arc::new(ArcSwapOption::empty());
                        inner.slots.insert(
                            key.clone(),
                            Slot {
                                incarnation: next_incarnation,
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
                    }
                };
                *inner
                    .families
                    .entry(family)
                    .or_default()
                    .entry(key.clone())
                    .or_insert(0) += 1;
                cells.push(cell);
            }
            debug_assert_eq!(next_incarnation, reserved_through, "reservation is exact");
            cells
        };

        if queued {
            // ONE wake for the whole set: the actor's queue is the authoritative
            // `pending`, and it already holds every new identity.
            self.work.mark();
        }
        Ok(DemandSet {
            registry: self.clone(),
            family,
            keys: keys.clone(),
            cells,
            held: parking_lot::Mutex::new(keys),
        })
    }

    /// Release one handle. The LAST reference retires the slot; a re-demand then
    /// allocates a FRESH incarnation, so work in flight cannot resurrect it.
    #[allow(dead_code)]
    fn release(&self, family: FamilyId, key: &SlotKey) {
        let mut inner = self.inner.lock();
        match inner.release_one(family, key) {
            ReleaseOutcome::Retire => {
                self.retire_committed(&mut inner, key);
                self.note_ref_released();
            }
            ReleaseOutcome::Released => self.note_ref_released(),
            ReleaseOutcome::NotHeld => {}
        }
    }

    /// Release EVERY key of a set in ONE registry transaction.
    ///
    /// One acquisition, not one per key: a set is released as a unit, so no
    /// observer can see a partially-released set and the capacity generation
    /// moves once for the whole retirement rather than interleaving with another
    /// family's demand.
    fn release_keys(&self, family: FamilyId, keys: &[SlotKey]) {
        let mut inner = self.inner.lock();
        let mut released = false;
        for key in keys {
            match inner.release_one(family, key) {
                ReleaseOutcome::Retire => {
                    self.retire_committed(&mut inner, key);
                    released = true;
                }
                ReleaseOutcome::Released => released = true,
                ReleaseOutcome::NotHeld => {}
            }
        }
        if released {
            self.note_ref_released();
        }
    }

    /// Publish that a reference was given back, so a refused REPLACEMENT knows
    /// its projection may have changed. Advances on every release, including one
    /// that leaves the slot alive with other owners.
    fn note_ref_released(&self) {
        self.ref_release_generation.fetch_add(1, Ordering::AcqRel);
    }

    /// Commit the retirement of a slot whose last reference just went away.
    ///
    /// **Clears the publication cell before dropping the slot.** The cell is an
    /// `Arc` that outstanding readers may still hold — a superseded `DemandSet`,
    /// or any holder of an `Arc<CapabilityRouteHandle>` the index no longer
    /// names — and removing the slot from `slots` does NOT reach them. Leaving
    /// the artifact in place would let a reader that owns no registry reference
    /// go on reading a dead incarnation's facts indefinitely, which is retention
    /// without ownership: exactly the property `DemandSet` exists to make
    /// impossible.
    ///
    /// Clearing it here is what makes "a retired slot's cell reads empty" TRUE
    /// rather than merely documented. It is safe against a concurrent
    /// reconstruction because the apply path re-looks the slot up under this
    /// same lock and refuses twice over: a removed slot is skipped outright, and
    /// a re-demanded one fails the `slot.incarnation == slot_incarnation` fence.
    /// A re-demand also mints a FRESH cell, so the cleared one is unreachable
    /// from the registry and nothing can ever republish into it.
    fn retire_committed(&self, inner: &mut RegistryInner, key: &SlotKey) {
        if let Some(slot) = inner.slots.get(key) {
            // Before the removal, so no window exists in which the slot is gone
            // from the index while its artifact is still readable.
            slot.facts.store(None);
        }
        inner.slots.remove(key);
        inner.pending.remove(key);
        if let Some(bucket) = inner.slots_by_capability.get_mut(&key.capability) {
            bucket.remove(key);
            if bucket.is_empty() {
                inner.slots_by_capability.remove(&key.capability);
            }
        }
        self.metrics.slots_retired.fetch_add(1, Ordering::AcqRel);
        // Node capacity genuinely moved. Published while the registry lock
        // still covers the removal, so an observer that reads the new
        // generation cannot then find the slot still there.
        self.node_capacity_generation.fetch_add(1, Ordering::AcqRel);
    }

    /// Atomically REPLACE one family's demand set with another (OLB-2B.3b §4.3).
    ///
    /// Not "acquire the new set, then drop the old one". That charges the family
    /// and the node at the TRANSIENT GROSS peak — every replacement handle while
    /// every superseded handle is still charged — so a replacement that plainly
    /// fits its final footprint is refused at the bound. A family holding 64 of
    /// 64 whose width-2 capability loses an audience projects to
    /// `64 - 2 + 1 = 63` and MUST succeed; charged gross it asks for 66.
    ///
    /// Nor "release the old set, then acquire the new one". That answers the
    /// bound correctly and breaks the no-effect property instead: a refusal
    /// would have already destroyed retained authority, and the re-acquisition
    /// that was supposed to restore it can fail on its own.
    ///
    /// So: ONE transaction, charged on the PROJECTED FINAL footprint.
    ///
    /// ```text
    /// common     = old ∩ new    reference kept EXACTLY as it is — no churn
    /// old_only   = old \ new    released; credits node capacity only if LAST
    /// new_only   = new \ old    charged; costs a node slot only if absent
    ///
    /// family:  held - |old_only| + |new_only|   <= MAX_HANDLES_PER_FAMILY
    /// node:    slots - credited  + created      <= MAX_NODE_SLOTS
    /// ```
    ///
    /// The node credit is deliberately conditional. An `old_only` slot another
    /// family still demands does NOT retire, so it frees nothing and crediting
    /// it would admit a 257th slot. The shared-old and last-reference cases are
    /// separately witnessed for exactly that reason.
    ///
    /// Every refusal is decided before the first mutation, so a refused
    /// replacement leaves handles, slots, refs, pending work, identities and
    /// generations exactly as they were — and, because the caller keeps its
    /// `DemandSet`, leaves the superseded entry fully live and owned.
    fn replace_demand_set(
        self: &Arc<Self>,
        family: FamilyId,
        old: &DemandSet,
        new_keys: Vec<SlotKey>,
    ) -> Result<DemandSet, ReplaceRefused> {
        let mut new_keys = new_keys;
        new_keys.sort();
        new_keys.dedup();

        // Lock order: the set's release responsibility, then the registry. The
        // set's own `Drop` takes them in the same order, so the two cannot
        // deadlock against each other.
        let mut held = old.held.lock();

        // A set that has already transferred owns nothing. Replacing it would
        // compute `old_only = ∅` and charge the successor's whole footprint
        // gross — the HOLD-2 defect, arriving through a stale basis instead of a
        // stale accounting rule. Refused before anything is read or written.
        if *held != old.keys {
            return Err(ReplaceRefused::Superseded);
        }

        let mut queued = false;
        let mut released = false;
        let cells = {
            let mut inner = self.inner.lock();

            let old_only: Vec<SlotKey> = held
                .iter()
                .filter(|key| new_keys.binary_search(key).is_err())
                .cloned()
                .collect();
            let new_only: Vec<SlotKey> = new_keys
                .iter()
                .filter(|key| held.binary_search(key).is_err())
                .cloned()
                .collect();

            // FAMILY, on the projected final count.
            let projected =
                inner.family_handles(family).saturating_sub(old_only.len()) + new_only.len();
            if projected > MAX_HANDLES_PER_FAMILY {
                self.metrics
                    .refused_family_at_capacity
                    .fetch_add(1, Ordering::AcqRel);
                return Err(ReplaceRefused::Demand(DemandRefused::FamilyAtCapacity));
            }

            // NODE, on the projected final retained set. A slot is credited only
            // when THIS reference is its last.
            let credited = old_only
                .iter()
                .filter(|key| inner.slots.get(key).is_some_and(|slot| slot.refs == 1))
                .count();
            let created = new_only
                .iter()
                .filter(|key| !inner.slots.contains_key(key))
                .count();
            let projected_slots = inner.slots.len().saturating_sub(credited) + created;
            if projected_slots > MAX_NODE_SLOTS {
                self.metrics
                    .refused_node_at_capacity
                    .fetch_add(1, Ordering::AcqRel);
                return Err(ReplaceRefused::Demand(DemandRefused::NodeAtCapacity));
            }

            // IDENTITIES for genuinely new slots, reserved as one checked block.
            let Some(reserved_through) = inner.next_id.checked_add(created as u64) else {
                self.metrics
                    .refused_id_space_exhausted
                    .fetch_add(1, Ordering::AcqRel);
                return Err(ReplaceRefused::Demand(DemandRefused::IdSpaceExhausted));
            };
            let mut next_incarnation = inner.next_id;
            inner.next_id = reserved_through;

            // Every refusal is above this line. Nothing below can fail.

            // Released FIRST, so the retained-slot count never rises above the
            // projected final figure even inside the transaction.
            for key in &old_only {
                // `NotHeld` is unreachable: `held` was verified to equal this
                // set's named keys above, and a set names only keys its own
                // family holds. Retiring on it anyway would be the cross-family
                // corruption in a different shape, so it is simply not a retire.
                match inner.release_one(family, key) {
                    ReleaseOutcome::Retire => {
                        self.retire_committed(&mut inner, key);
                        released = true;
                    }
                    ReleaseOutcome::Released => released = true,
                    ReleaseOutcome::NotHeld => {}
                }
            }
            for key in &new_only {
                match inner.slots.get_mut(key) {
                    Some(slot) => slot.refs += 1,
                    None => {
                        next_incarnation += 1;
                        let cell = Arc::new(ArcSwapOption::empty());
                        inner.slots.insert(
                            key.clone(),
                            Slot {
                                incarnation: next_incarnation,
                                refs: 1,
                                facts: cell,
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
                }
                *inner
                    .families
                    .entry(family)
                    .or_default()
                    .entry(key.clone())
                    .or_insert(0) += 1;
            }
            debug_assert_eq!(next_incarnation, reserved_through, "reservation is exact");

            // Cells for the WHOLE new set. A `common` key yields the very cell it
            // already had, because its reference was never touched.
            new_keys
                .iter()
                .map(|key| {
                    // `expect_used` lint guard: a `common` key kept its
                    // reference untouched and a `new_only` key was inserted or
                    // bumped just above, so the lookup cannot miss; suppress
                    // locally rather than `filter_map`, which would silently
                    // misalign the `keys[i]` <-> `cells[i]` pairing.
                    #[allow(clippy::expect_used)]
                    let slot = inner
                        .slots
                        .get(key)
                        .expect("every key of the new set is retained above");
                    slot.facts.clone()
                })
                .collect::<Vec<_>>()
        };

        // The TRANSFER, and the only place it happens: the superseded set now
        // owes nothing, because everything it owed is either still held (common)
        // or has just been released (old_only). No flag decides this — the keys
        // themselves moved, so exactly one set owes a release at every instant.
        held.clear();
        drop(held);

        if released {
            self.note_ref_released();
        }
        if queued {
            self.work.mark();
        }
        Ok(DemandSet {
            registry: self.clone(),
            family,
            keys: new_keys.clone(),
            cells,
            held: parking_lot::Mutex::new(new_keys),
        })
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
    /// the whole install direction, and it is harmless coalescing besides.
    /// Everything else is judged by the publication generation the artifact was
    /// reconstructed under.
    ///
    /// Three kinds of survivor, and the wording matters because two earlier
    /// versions of this comment erased one of them each:
    ///
    /// - **this publication's own artifact** — equal generation. A demand that
    ///   arrived after the publication and before its notification produces one,
    ///   and it already reflects the transition;
    /// - a LATER installation's artifact;
    /// - an equal-or-later ABSENCE artifact. Treating absence as
    ///   never-a-successor is the defect Kyra's probe found at `7348529fb`.
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
                // Ordered by the PUBLICATION generation the reconstruction
                // observed, not by the authority it stamped — that is what makes
                // the fence total over installations AND absences. A slot with no
                // artifact has nothing to preserve and still owes work, so it is
                // re-queued: harmless coalescing, since `pending` is a set and a
                // cold slot needs rebuilding regardless.
                //
                // STRICTLY less than. An artifact from THIS publication already
                // reflects the transition and must survive it — that is the
                // "a demand arriving after publication is safe" case of §2A.2,
                // and `<=` would have the notification destroy its own
                // publication's current artifact.
                //             artifact:  Publication(a)      TerminalAbsence
                //  movement:
                //  Publication(p)         clear iff a < p     preserve
                //  Terminal               clear               preserve
                //
                // The `Terminal` row is NOT "clear everything". Terminal
                // withdrawal must still preserve the absence artifact its OWN
                // publication produced — the same equality property W-W9/W-W10
                // pin for ordinary publications, and it does not come for free
                // just because nothing can supersede a terminal state.
                //
                // `Publication(_)` under a `Terminal` movement always clears: at
                // exhaustion the last live identity may equal the artifact's, so
                // an identity comparison could not distinguish "before the
                // withdrawal" from "after" — which is exactly the alias this
                // arm exists to avoid.
                //
                // FOUR arms for four cells, and the two `TerminalAbsence` ones
                // are written out separately even though they agree. Collapsed
                // to `(TerminalAbsence, _)` they could not be mutated apart, and
                // that is not hypothetical: the whole gate stayed green with
                // ordinary movement clearing terminal absence, because the only
                // witness in that column tested the `Terminal` row (Kyra, review
                // of `010c718ea`). A cell that cannot be mutated alone cannot be
                // witnessed alone.
                let superseded = match cell.load().as_ref() {
                    None => true,
                    Some(facts) => match (facts.grant_fence, movement.fence) {
                        // Terminal absence survives its own ORDINARY publication.
                        // The removal that SPENDS the space is an ordinary
                        // transition — it reserves the last live identity — yet
                        // the absence it causes reconstructs terminal. W-W15.
                        (
                            GrantArtifactFence::TerminalAbsence,
                            GrantMovementFence::Publication(_),
                        ) => false,
                        // And survives a terminal one. W-W14.
                        (GrantArtifactFence::TerminalAbsence, GrantMovementFence::Terminal) => {
                            false
                        }
                        (GrantArtifactFence::Publication(_), GrantMovementFence::Terminal) => true,
                        (
                            GrantArtifactFence::Publication(artifact),
                            GrantMovementFence::Publication(publication),
                        ) => artifact < publication,
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

    /// Test-only: give back ONE of `family`'s references to `key`, exactly as an
    /// internal release would.
    ///
    /// Exists so the ownership invariant can be witnessed DIRECTLY — that a
    /// family which holds no such reference cannot decrement the global one —
    /// rather than inferred from a cross-family transfer the crate-private
    /// surface no longer lets anyone express.
    #[cfg(test)]
    pub(crate) fn release_one_for_test(&self, family: &RoutingFamily, key: &SlotKey) {
        let mut inner = self.inner.lock();
        match inner.release_one(family.id, key) {
            ReleaseOutcome::Retire => {
                self.retire_committed(&mut inner, key);
                drop(inner);
                self.note_ref_released();
            }
            ReleaseOutcome::Released => {
                drop(inner);
                self.note_ref_released();
            }
            ReleaseOutcome::NotHeld => {}
        }
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

    /// Test-only: the high-water mark of the monotone identity allocator.
    ///
    /// The no-effect assertion for a refused demand set has to be TOTAL —
    /// {slots, pending, handles, identities} — and the identity component is
    /// the one a naive `demand`-in-a-loop implementation silently fails while
    /// every other component looks clean, because the prefix it retained is
    /// released again on the error path (OLB-2B.3b, W-A5; the same lesson as
    /// step 1's `300e80f6c`).
    #[cfg(test)]
    pub(crate) fn allocated_ids_for_test(&self) -> u64 {
        self.inner.lock().next_id
    }

    /// Test-only: drive the identity allocator to exhaustion, so a witness can
    /// reach the TERMINAL refusal without minting 2^64 slots.
    #[cfg(test)]
    pub(crate) fn exhaust_ids_for_test(&self) {
        self.inner.lock().next_id = u64::MAX;
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
                    grant_fence,
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
                    grant_fence,
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
                grant_fence: GrantArtifactFence::Publication(0),
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

    /// A minimal published artifact, for witnesses about cell LIFETIME rather
    /// than cell content.
    fn facts() -> Arc<SlotBaseFacts> {
        Arc::new(SlotBaseFacts {
            providers: SourceFacts::Unserved,
            epoch: SourceEpoch::default(),
            authority: ScopedDiscoveryAuthorityStamp::Owner,
            actor_incarnation: 1,
            slot_incarnation: 1,
            grant_fence: GrantArtifactFence::Publication(0),
            earliest_expiry: u64::MAX,
        })
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

    // ------------------------------- OLB-2B.3b: atomic demand-set acquisition

    /// The TOTAL state a refused acquisition must leave untouched.
    ///
    /// Total on purpose (step 1's `300e80f6c` lesson): a witness that asserts a
    /// SUBSET of {slots, pending, handles, identities, retirements} passes
    /// against the naive `demand`-in-a-loop implementation, whose prefix IS
    /// released on the error path — so slots and handles come back to their
    /// starting values and only the identity space and the retirement counter
    /// still show what happened.
    #[derive(Debug, PartialEq, Eq)]
    struct NoEffect {
        slots: usize,
        pending: usize,
        handles: usize,
        ids: u64,
        retired: u64,
    }

    fn no_effect(f: &Fixture, family: &RoutingFamily) -> NoEffect {
        NoEffect {
            slots: f.registry.retained_slots(),
            pending: f.registry.pending_slots(),
            handles: family.handles(),
            ids: f.registry.allocated_ids_for_test(),
            retired: f.metrics.slots_retired(),
        }
    }

    fn set(seeds: &[u8], tag: &str) -> Vec<SlotKey> {
        seeds.iter().map(|s| key(*s, tag)).collect()
    }

    /// W-A1 — the family bound is checked for the WHOLE set, and a refusal
    /// retains nothing.
    ///
    /// The set is deliberately larger than the remaining budget by ONE, with
    /// several NEW keys ahead of the one that does not fit. That is the schedule
    /// a per-key loop gets wrong: it admits the prefix, mints an incarnation for
    /// each, and only then discovers the bound.
    #[test]
    fn a_demand_set_past_the_family_bound_retains_nothing() {
        let f = fixture();
        let family = f.family();
        let mut held = Vec::new();
        for i in 0..62u32 {
            held.push(
                family
                    .demand(key(1, &format!("nrpc:fill-{i}")))
                    .expect("fill"),
            );
        }
        assert_eq!(family.handles(), 62);
        let before = no_effect(&f, &family);

        assert_eq!(
            family.demand_set(set(&[10, 11, 12], "nrpc:wide")).err(),
            Some(DemandRefused::FamilyAtCapacity),
            "62 + 3 exceeds the 64-handle family budget"
        );
        assert_eq!(
            no_effect(&f, &family),
            before,
            "a refused set retains no slot, queues no work, spends no handle, \
             consumes no identity and retires nothing"
        );
        assert_eq!(f.metrics.refused_family_at_capacity(), 1);

        // The bound is exact, not conservative: a set that FITS still goes
        // through, so the refusal above was the budget and not the batching.
        let fits = family
            .demand_set(set(&[10, 11], "nrpc:wide"))
            .expect("62 + 2 fits exactly");
        assert_eq!(fits.len(), 2);
        assert_eq!(family.handles(), 64);
    }

    /// W-A2 — the node bound is checked for every NEW distinct slot, and a
    /// refusal retains nothing.
    #[test]
    fn a_demand_set_past_the_node_bound_retains_nothing() {
        let f = fixture();
        // Fill the node to 255 with four families, none of which may exceed 64.
        let mut fillers = Vec::new();
        let mut held = Vec::new();
        for chunk in 0..4u32 {
            let filler = f.family();
            for i in 0..64u32 {
                let n = chunk * 64 + i;
                if n == 255 {
                    break;
                }
                held.push(
                    filler
                        .demand(key(2, &format!("nrpc:node-{n}")))
                        .expect("fill"),
                );
            }
            fillers.push(filler);
        }
        assert_eq!(f.registry.retained_slots(), 255);

        let family = f.family();
        let before = no_effect(&f, &family);
        assert_eq!(
            family.demand_set(set(&[20, 21], "nrpc:pair")).err(),
            Some(DemandRefused::NodeAtCapacity),
            "255 + 2 new slots exceeds the 256-slot node bound"
        );
        assert_eq!(
            no_effect(&f, &family),
            before,
            "the FIRST of the two must not be retained"
        );
        assert_eq!(f.metrics.refused_node_at_capacity(), 1);

        // One new slot still fits — so the refusal counted the set, not the call.
        let one = family
            .demand_set(set(&[20], "nrpc:pair"))
            .expect("255 + 1 fits");
        assert_eq!(one.len(), 1);
        assert_eq!(f.registry.retained_slots(), 256);
    }

    /// W-A2b — an ALREADY-RETAINED key costs no node capacity, so a set that
    /// shares slots with another family is admitted at the bound.
    ///
    /// The control for W-A2: without it, "count every key" and "count every NEW
    /// key" both refuse the case above, and the witness would not distinguish
    /// the implementation from a strictly-worse one that never shares.
    #[test]
    fn a_demand_set_over_retained_slots_costs_no_node_capacity() {
        let f = fixture();
        let mut fillers = Vec::new();
        let mut held = Vec::new();
        for chunk in 0..4u32 {
            let filler = f.family();
            for i in 0..64u32 {
                held.push(
                    filler
                        .demand(key(2, &format!("nrpc:node-{}", chunk * 64 + i)))
                        .expect("fill"),
                );
            }
            fillers.push(filler);
        }
        assert_eq!(f.registry.retained_slots(), 256, "the node is FULL");

        let family = f.family();
        let shared = family
            .demand_set(vec![key(2, "nrpc:node-0"), key(2, "nrpc:node-1")])
            .expect("sharing retained slots needs no node capacity");
        assert_eq!(shared.len(), 2);
        assert_eq!(f.registry.retained_slots(), 256, "and created nothing");
        assert_eq!(f.metrics.refused_node_at_capacity(), 0);
    }

    /// W-A3 — exhaustion refuses the whole set and consumes no identity.
    #[test]
    fn an_exhausted_identity_space_refuses_a_whole_demand_set() {
        let f = fixture();
        let family = f.family();
        let _live = family.demand(key(1, "nrpc:a")).expect("a");
        f.registry.exhaust_ids_for_test();
        let before = no_effect(&f, &family);

        assert_eq!(
            family.demand_set(set(&[30, 31], "nrpc:fresh")).err(),
            Some(DemandRefused::IdSpaceExhausted),
            "two new slots need two incarnations and there are none"
        );
        assert_eq!(no_effect(&f, &family), before, "and nothing moved");
        assert_eq!(f.metrics.refused_id_space_exhausted(), 1);

        // A set over ONLY retained slots needs no incarnation, so exhaustion
        // does not refuse it. The control that stops "refuse everything once
        // exhausted" from passing this witness.
        let shared = family
            .demand_set(vec![key(1, "nrpc:a")])
            .expect("a retained slot needs no fresh identity");
        assert_eq!(shared.len(), 1);
    }

    /// A superseded reader must not keep reading a RETIRED incarnation's facts.
    ///
    /// Dies to: retiring a slot without clearing its publication cell. The cell is
    /// an `Arc` outstanding readers still hold, so removing the slot from the
    /// registry map does not reach them — the artifact stays readable forever
    /// through a set that owns no reference at all. That is retention without
    /// ownership, which is the one thing `DemandSet` exists to make impossible.
    #[test]
    fn a_superseded_reader_cannot_read_a_retired_incarnations_facts() {
        let f = fixture();
        let family = f.family();
        let old_key = key(1, "nrpc:retire");
        let old = family.demand_set(vec![old_key.clone()]).expect("acquired");

        f.registry.install_facts_for_test(old_key.clone(), facts());
        assert!(
            old.base_facts_unvalidated(0).is_some(),
            "precondition: the reader can see its own incarnation's facts"
        );

        // Replace with a DISJOINT key: the old slot's last reference goes away.
        let new = old.replace(vec![key(2, "nrpc:retire")]).expect("replaced");
        assert_eq!(f.registry.retained_slots(), 1, "the old slot retired");

        assert!(
            old.base_facts_unvalidated(0).is_none(),
            "an old reader must not retain facts from the retired incarnation"
        );
        drop(new);
    }

    /// A COMMON key stays readable while the successor owns it, and goes empty when
    /// the successor gives up the last reference.
    ///
    /// The control for the witness above: clearing on retirement must not clear on
    /// TRANSFER. The old reader shares the successor's cell, and the scope is still
    /// genuinely retained — by the successor — so it must still read.
    #[test]
    fn a_common_key_reader_follows_the_successors_ownership() {
        let f = fixture();
        let family = f.family();
        let shared = key(1, "nrpc:common");
        let old = family
            .demand_set(vec![shared.clone(), key(2, "nrpc:common")])
            .expect("acquired");
        f.registry.install_facts_for_test(shared.clone(), facts());
        assert!(old.base_facts_unvalidated(0).is_some());

        // The successor keeps the common key and sheds the other.
        let new = old.replace(vec![shared.clone()]).expect("replaced");
        assert!(
            old.base_facts_unvalidated(0).is_some(),
            "the common scope is still retained — by the successor — so it still reads"
        );

        drop(new);
        assert!(
            old.base_facts_unvalidated(0).is_none(),
            "and goes empty the moment the successor releases the last reference"
        );
        assert_eq!(f.registry.retained_slots(), 0);
    }

    /// A fresh demand for a retired `SlotKey` gets a FRESH cell and incarnation, and
    /// cannot repopulate the stale one.
    #[test]
    fn a_fresh_demand_after_retirement_gets_a_fresh_cell() {
        let f = fixture();
        let family = f.family();
        let reused = key(1, "nrpc:reused");
        let old = family.demand_set(vec![reused.clone()]).expect("acquired");
        f.registry.install_facts_for_test(reused.clone(), facts());
        assert!(old.base_facts_unvalidated(0).is_some());

        let successor = old.replace(vec![key(2, "nrpc:reused")]).expect("replaced");
        assert!(old.base_facts_unvalidated(0).is_none());

        // Demand the SAME key again. It is a new slot with a new incarnation.
        let fresh = family
            .demand_set(vec![reused.clone()])
            .expect("re-demanded");
        f.registry.install_facts_for_test(reused.clone(), facts());
        assert!(
            fresh.base_facts_unvalidated(0).is_some(),
            "the live incarnation publishes into its own cell"
        );
        assert!(
            old.base_facts_unvalidated(0).is_none(),
            "and the stale cell stays empty — a fresh publication cannot reach it"
        );
        drop((successor, fresh));
    }

    /// An in-flight apply for a DEAD incarnation cannot republish into the cleared
    /// cell, even when the same key has been re-demanded.
    #[test]
    fn a_stale_incarnation_apply_cannot_republish_a_cleared_cell() {
        let f = fixture();
        let family = f.family();
        let contested = key(1, "nrpc:race");
        let old = family
            .demand_set(vec![contested.clone()])
            .expect("acquired");
        f.registry
            .install_facts_for_test(contested.clone(), facts());

        // Retire it, then re-demand the same key: a new incarnation, a new cell.
        let successor = old.replace(vec![key(2, "nrpc:race")]).expect("replaced");
        let fresh = family
            .demand_set(vec![contested.clone()])
            .expect("re-demanded");

        // Drive a real quantum. The reconstruction reaches the LIVE slot only.
        let outcome = f.registry.apply(1, request(true, DirtyCapabilities::Clean));
        assert!(
            matches!(outcome, ApplyOutcome::Current { .. }),
            "{outcome:?}"
        );
        assert!(
            old.base_facts_unvalidated(0).is_none(),
            "the dead incarnation's cell is unreachable from the registry"
        );
        drop((successor, fresh));
    }

    /// `release_one` cannot decrement a global slot reference for a family that
    /// holds none — the bookkeeping invariant, asserted directly.
    ///
    /// Dies to: dropping the `NotHeld` guard. Without it, a caller that names the
    /// wrong family retires a slot some OTHER family still owns, stranding a handle
    /// in that family's map that its set can never give back.
    #[test]
    fn release_one_cannot_decrement_a_reference_a_family_does_not_hold() {
        let f = fixture();
        let owner = f.family();
        let stranger = f.family();
        let owned = key(1, "nrpc:owned");
        let held = owner.demand_set(vec![owned.clone()]).expect("acquired");

        let before = f.registry.retained_slots();
        let generation = f.registry.node_capacity_generation();
        f.registry.release_one_for_test(&stranger, &owned);

        assert_eq!(
            f.registry.retained_slots(),
            before,
            "a family that holds nothing gives nothing back"
        );
        assert_eq!(f.registry.node_capacity_generation(), generation);
        assert_eq!(owner.handles(), 1, "the owner still holds its handle");
        assert!(
            held.base_facts_unvalidated(0).is_some() || f.registry.retained_slots() == 1,
            "and its slot survives"
        );
        drop(held);
        assert_eq!(
            f.registry.retained_slots(),
            0,
            "the owner can still release"
        );
    }

    /// A set whose responsibility has already transferred cannot be replaced again.
    ///
    /// Dies to: dropping the `held != keys` guard. A second replacement through the
    /// spent set computes `old_only = ∅` and charges the successor's whole footprint
    /// gross — the HOLD-2 defect, reached through a stale basis instead of a stale
    /// accounting rule.
    #[test]
    fn an_already_transferred_set_cannot_be_replaced_again() {
        let f = fixture();
        let family = f.family();
        let old = family
            .demand_set(vec![key(1, "nrpc:once")])
            .expect("acquired");
        let successor = old
            .replace(vec![key(2, "nrpc:once")])
            .expect("first replacement");

        let handles = family.handles();
        let slots = f.registry.retained_slots();
        assert_eq!(
            old.replace(vec![key(3, "nrpc:once")]).err(),
            Some(ReplaceRefused::Superseded),
            "a spent set is not the basis of anything"
        );
        assert_eq!(family.handles(), handles, "and nothing moved");
        assert_eq!(f.registry.retained_slots(), slots);
        drop(successor);
    }

    /// W-A4 — duplicate keys collapse to one slot and one handle each.
    ///
    /// The single-key `demand` deliberately counts a repeat as a second handle
    /// (re-demanding must not bypass the bound). A demand SET is a set: the same
    /// scope named twice is one contributor, and charging it twice would refuse
    /// families for capacity they never needed.
    #[test]
    fn duplicate_keys_in_a_demand_set_collapse() {
        let f = fixture();
        let family = f.family();
        let handles = family
            .demand_set(vec![
                key(1, "nrpc:dup"),
                key(1, "nrpc:dup"),
                key(2, "nrpc:dup"),
                key(1, "nrpc:dup"),
            ])
            .expect("acquired");

        assert_eq!(handles.len(), 2, "four keys, two distinct scopes");
        assert_eq!(family.handles(), 2, "and two handles against the budget");
        assert_eq!(f.registry.retained_slots(), 2);
        assert_eq!(
            f.registry.allocated_ids_for_test(),
            3,
            "one identity for the family and one per distinct slot — not per key"
        );
    }

    /// W-A5 — a refusal reached AFTER the identity space was consulted still
    /// consumes no identity, and the set is ordered so a per-key loop provably
    /// would.
    ///
    /// The family holds 63 of 64 handles and asks for three NEW keys. A loop
    /// admits the first (63 < 64), minting an incarnation, and refuses the
    /// second. The prefix handle is then dropped, the slot retires, and every
    /// count returns to its starting value EXCEPT the identity high-water mark
    /// and the retirement counter — which is exactly why the assertion is total.
    #[test]
    fn a_refusal_after_identities_are_considered_consumes_none() {
        let f = fixture();
        let family = f.family();
        let mut held = Vec::new();
        for i in 0..63u32 {
            held.push(
                family
                    .demand(key(1, &format!("nrpc:fill-{i}")))
                    .expect("fill"),
            );
        }
        let before = no_effect(&f, &family);
        assert_eq!(before.handles, 63);

        assert_eq!(
            family.demand_set(set(&[40, 41, 42], "nrpc:late")).err(),
            Some(DemandRefused::FamilyAtCapacity)
        );
        let after = no_effect(&f, &family);
        assert_eq!(
            after.ids, before.ids,
            "the identity space is untouched — the component a loop leaks"
        );
        assert_eq!(
            after.retired, before.retired,
            "and nothing was created only to be retired again"
        );
        assert_eq!(after, before, "totally, not just in those two components");
    }

    /// W-A6 — a successful acquisition marks the actor ONCE and queues every new
    /// identity, so the set is warmed by one pass rather than one pass per key.
    #[test]
    fn a_successful_demand_set_queues_every_key_and_marks_once() {
        let f = fixture();
        let family = f.family();
        let handles = family
            .demand_set(set(&[50, 51, 52], "nrpc:batch"))
            .expect("acquired");
        assert_eq!(handles.len(), 3);
        assert_eq!(f.registry.pending_slots(), 3, "every key owes work");

        let outcome = f.registry.apply(1, request(true, DirtyCapabilities::Clean));
        assert!(
            matches!(outcome, ApplyOutcome::Current { .. }),
            "{outcome:?}"
        );
        for seed in [50u8, 51, 52] {
            assert!(
                f.registry
                    .base_facts_unvalidated(&key(seed, "nrpc:batch"))
                    .is_some(),
                "the whole set is warmed by ONE pass"
            );
        }
    }

    /// The demand set arrives in deterministic key order, Owner scopes first.
    ///
    /// §3.1 projects Owner pool first and then Grant pools; a handle vector in
    /// call order would make that ordering the caller's problem.
    #[test]
    fn a_demand_set_is_ordered_owner_scopes_first() {
        let f = fixture();
        let family = f.family();
        let grant = PrivateAudienceScope::new(CapabilityAudienceScope::Grant {
            grant_id: [9u8; 32],
            audience_handle: [9u8; 32],
        })
        .expect("grant scopes are private");
        let cap = CapabilityAuthorityId::for_tag("nrpc:order");
        let handles = family
            .demand_set(vec![
                SlotKey {
                    scope: grant.clone(),
                    capability: cap,
                },
                key(1, "nrpc:order"),
            ])
            .expect("acquired");

        assert_eq!(handles.keys()[0], key(1, "nrpc:order"), "Owner first");
        assert_eq!(handles.keys()[1].scope, grant, "Grant after");
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
