//! OLB-2B.3b: the clone-family routing state.
//!
//! One `OrgRoutingState` per clone family. It owns three things and nothing
//! else: a lock-free capability index, the `CapabilityRouteHandle`s that index
//! points at, and the refusal bookkeeping that decides whether a miss may try
//! to acquire demand again.
//!
//! ```text
//! OrgRoutingState
//!   index    ArcSwap<CapabilityIndex>      immutable, bounded; the READ path
//!   mutate   parking_lot::Mutex<Refusal>   miss / insert / drop ONLY
//! ```
//!
//! Design §8 sketches `mutate` as `Mutex<()>`. It guards the refusal
//! bookkeeping here instead of nothing, because that state is read and written
//! on exactly the miss path the lock already delimits — a separate `Mutex<()>`
//! beside separately-synchronized refusal state would be two things to keep in
//! step where one will do, and would let a refusal be recorded against a
//! capacity generation sampled outside the section that acted on it. What §8
//! actually pins is preserved exactly: the READ path takes no lock, and the
//! mutation path takes exactly one.
//!
//! **The read path takes no lock.** `index.load()` is one atomic; the handle it
//! yields is an `Arc` clone. `mutate` is never touched by a hit. It is not a
//! `DashMap` on purpose (design §8): internally-sharded locking would leave the
//! zero-lock contract unproven while appearing to satisfy it, because the
//! sharded lock is usually uncontended and a witness would pass by luck.
//!
//! ## What this slice does NOT do
//!
//! A `CapabilityRouteHandle` here owns a demand set and nothing more. There is
//! no `OrgRouteSet`, no route-set cell, no scoped pool, no candidate, no
//! provider list, no selection, and no projection — those are 2B.3c and 2B.3d,
//! and this module deliberately cannot express them. `route_handle` returns
//! ownership of retained demand, never a route.
//!
//! ## The demand set (design §4)
//!
//! ```text
//! Owner
//! + each Grant audience THIS FAMILY ACTUALLY LEASED FOR DISCOVER
//! ```
//!
//! Both qualifiers are load-bearing and each has its own witness. A DISCOVER
//! grant whose audience secret was never installed yields no consumer audience,
//! so demanding it would retain a slot that can never be anything but
//! `Unserved` — permanently consuming node and family budget for a contributor
//! that can never contribute (W-N1). And an INVOKE-only grant is not a
//! discovery authority at all; it stays in the family's credential set for the
//! projection to match providers against, but it is not a source demand (W-N2).
//!
//! ```text
//! DISCOVER ≠ INVOKE ≠ SENSE
//! ```
//!
//! ## The demand set is CURRENT, never first-seen (§4.2)
//!
//! `route_handle` takes a [`ConsumerGrantSnapshot`] that CHANGES under it: an
//! audience secret can be installed, removed or rotated long after a capability
//! first warmed. "Actually leased" therefore means leased NOW — never "leased on
//! whichever miss happened to warm this entry". An entry frozen at first-miss
//! silently omits an authority the family has since been given, which reads
//! downstream as a route that legitimately found no granted provider: exactly
//! the failure §4.1 refuses to let a kept prefix produce.
//!
//! The currency check runs on the WARMED read and takes NO lock:
//!
//! ```text
//! the scopes this entry retains   (CapabilityRouteHandle::demanded)
//! vs
//! the scopes THIS capability's grants are leased for right now
//! ```
//!
//! Both directions are checked, because they fail differently: a retained scope
//! that is no longer leased is an OBSOLETE demand (removal, rotation), and a
//! leased scope the entry does not retain is a MISSING one (installation). Each
//! comparison is on the whole `(grant_id, audience_handle)` scope, so a rotation
//! — same id, new handle — is both at once and cannot alias.
//!
//! It is bounded by ONE capability's grants, never the family's whole credential
//! set and never the provider set, because [`OrgRoutingState`] indexes the
//! credentials by capability once at construction. That index is a filter of the
//! same credential set the pure derivation walks, and both go through
//! `demand_set_from`, so the warmed check and the miss path cannot disagree
//! about what "leased" means.
//!
//! ## Re-derivation REPLACES, charged on the projected footprint (§4.3)
//!
//! A stale entry is not "acquire the new set, then drop the old one". That
//! charges the family and the node at the transient GROSS peak — every
//! replacement handle while every superseded handle is still charged — so a
//! replacement whose FINAL footprint fits is refused at the bound:
//!
//! ```text
//! family at 64/64, capability loses an audience
//! projected   64 - 2 + 1 = 63     must succeed
//! gross       64 + 1     = 65     refused
//! ```
//!
//! An entry that cannot shed an obsolete scope without capacity that shedding it
//! would itself free is stuck forever, so the bound would preserve exactly the
//! stale authority §4.2 exists to correct.
//!
//! It is not "release the old set, then acquire the new one" either: that
//! answers the bound and breaks no-effect instead, destroying retained authority
//! before an acquisition that may fail.
//!
//! So re-derivation goes through ONE registry transaction that compares the two
//! key sets and charges the projected final footprint —
//! `DemandSet::replace` — rooted in the superseded set's OWN registry and family
//! identity, so no caller can offer another family's set as the basis. The
//! intersection is never released and
//! re-taken, the state asks its own refusal gate only for the MARGINAL cost
//! (see `marginal_charge`), and a refusal leaves the superseded set owning
//! everything it owned. A refused re-derivation reports the refusal rather than
//! the stale entry: answering `Warm` with a Grant plane known to be incomplete
//! is the silent authority narrowing this slice exists to prevent.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use arc_swap::ArcSwap;

use super::org::OrgId;
use super::org_grant::{CapabilityAuthorityId, OrgCapabilityGrant};
use super::org_grant_registry::ConsumerGrantSnapshot;
use super::org_routing_registry::{
    DemandRefused, DemandSet, GrantMovementFence, PrivateAudienceScope, ReplaceRefused,
    RoutingFamily, SlotKey, MAX_HANDLES_PER_FAMILY,
};
use super::org_scoped_ingest::CapabilityAudienceScope;

/// Max capability entries ONE clone family may retain (design §4).
///
/// STRUCTURAL rather than separately enforced: every entry retains at least the
/// Owner demand, and the family's demand budget is
/// [`MAX_HANDLES_PER_FAMILY`], so the index can never hold more entries than
/// that however cheap the entries are. Stating it as a `<=` bound derived from
/// the demand bound — rather than as a second independent counter — is what
/// keeps the two from disagreeing.
///
/// It is a CEILING that is normally not reached. A family with two leased
/// DISCOVER grants per capability spends three demands per entry and warms 21
/// capabilities, not 64.
#[allow(dead_code)] // documents the structural ceiling; nothing enforces it twice.
pub(crate) const MAX_CAPABILITY_ENTRIES_PER_FAMILY: usize = MAX_HANDLES_PER_FAMILY;

/// The family's discovery credentials, as the demand derivation sees them.
///
/// Deliberately plain data. The derivation is a pure function of
/// (credentials, capability, installed consumer audiences), which is what lets
/// a witness drive it without a node and what keeps this slice from acquiring
/// a dependency on the SDK's credential type.
#[derive(Clone, Debug)]
#[allow(dead_code)] // consumer: `MeshNode::call` (OLB-2B.3d).
pub(crate) struct FamilyDiscoveryCredentials {
    /// The acting org — the family's own organization.
    pub acting_org: OrgId,
    /// The org's owner audience routing handle. With `acting_org` this is the
    /// family's Owner discovery scope, which is a demand for EVERY capability
    /// the family warms.
    pub owner_audience_handle: [u8; 32],
    /// The family's COMPLETE grant set, in exact credential order.
    ///
    /// Complete on purpose: INVOKE-only grants belong here even though they are
    /// never source demands, because the projection matches INVOKE against this
    /// set (§4). A derivation that could only see DISCOVER grants would make
    /// W-N2 unwitnessable — there would be nothing left to wrongly demand.
    pub grants: Vec<Arc<OrgCapabilityGrant>>,
}

impl FamilyDiscoveryCredentials {
    /// The family's Owner discovery scope.
    fn owner_scope(&self) -> Option<PrivateAudienceScope> {
        PrivateAudienceScope::new(CapabilityAudienceScope::Owner {
            org_id: self.acting_org,
            audience_handle: self.owner_audience_handle,
        })
    }
}

/// Why one grant is or is not a source demand for a capability.
///
/// A named classification rather than a filter chain, because "which of the
/// family's credentials are source demands" is the single decision W-N1 and
/// W-N2 pin, and a decision that lives in a `.filter(..).filter(..)` cannot be
/// mutated one reason at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GrantDemand {
    /// A DISCOVER grant whose exact audience the family has leased.
    Leased([u8; 32]),
    /// Right for another verb. INVOKE-only grants stay available to the
    /// projection and consume no source budget.
    NotDiscovery,
    /// DISCOVER, but no installed audience secret — so no consumer audience
    /// exists and the source can never serve the scope.
    NotLeased,
}

/// Classify one grant of the family's credential set for `capability`.
///
/// `None` when the grant is for a different capability or a different grantee;
/// otherwise the exact reason it is or is not a demand.
fn classify(
    grant: &OrgCapabilityGrant,
    capability: &CapabilityAuthorityId,
    acting_org: &OrgId,
    leased: &ConsumerGrantSnapshot,
) -> Option<GrantDemand> {
    if &grant.capability != capability || &grant.grantee_org != acting_org {
        return None;
    }
    // W-N2. DISCOVER ≠ INVOKE: only a DISCOVER grant carries the discovery
    // binding that names an audience, and only an audience can be a source
    // scope. Demanding an INVOKE-only grant would key a slot on an audience
    // that does not exist, which the source can only ever answer `Unserved`.
    if !grant.permits_discover() {
        return Some(GrantDemand::NotDiscovery);
    }
    // Both halves of the structural rule, checked rather than assumed. `verify`
    // enforces `rights ⊇ DISCOVER ⇔ binding present`, so for a verified grant
    // these two cannot disagree — which is exactly why neither is load-bearing
    // on its own and why §17.6a records that this gate is not independently
    // observable further downstream.
    let Some(binding) = grant.discovery.as_ref() else {
        return Some(GrantDemand::NotDiscovery);
    };
    // W-N1. The right to discover is not the audience. Without the out-of-band
    // secret installed there is no consumer audience, the live query admits no
    // row, and the slot is a permanently-`Unserved` required contributor that
    // consumes budget forever (§4).
    //
    // Compared on the WHOLE scope — `(grant_id, audience_handle)` — never on
    // `grant_id` alone. A rotated audience leaves the id installed under a
    // DIFFERENT handle, and an id-keyed check would call the rotated-away scope
    // leased (the aliasing the source seam already closed at `cbbd448b3`).
    let installed = leased
        .get(&grant.grant_id)
        .is_some_and(|record| record.audience_handle() == &binding.audience_handle);
    if installed {
        Some(GrantDemand::Leased(binding.audience_handle))
    } else {
        Some(GrantDemand::NotLeased)
    }
}

/// The exact demand set for `capability`: Owner plus every leased DISCOVER
/// audience, deduplicated and in deterministic key order.
///
/// `None` when the family has no representable Owner scope, which is the one
/// input that makes the whole set meaningless rather than merely smaller.
///
/// A pure function of (credentials, capability, installed consumer audiences),
/// which is what lets a witness drive it without a node.
#[allow(dead_code)] // consumer: `MeshNode::call` (OLB-2B.3d).
pub(crate) fn demand_set_for(
    credentials: &FamilyDiscoveryCredentials,
    capability: &CapabilityAuthorityId,
    leased: &ConsumerGrantSnapshot,
) -> Option<Vec<SlotKey>> {
    demand_set_from(credentials, credentials.grants.iter(), capability, leased)
}

/// The derivation itself, over an arbitrary source of candidate grants.
///
/// ONE body, two sources: [`demand_set_for`] walks the family's whole credential
/// set, and [`OrgRoutingState`] walks only the grants it indexed for this
/// capability. `classify` re-checks the capability either way, so the index is a
/// pure filter and the two cannot derive different sets — which is a property a
/// witness pins rather than a comment asserts
/// (`the_capability_index_derives_exactly_what_the_full_scan_derives`).
fn demand_set_from<'a>(
    credentials: &FamilyDiscoveryCredentials,
    grants: impl Iterator<Item = &'a Arc<OrgCapabilityGrant>>,
    capability: &CapabilityAuthorityId,
    leased: &ConsumerGrantSnapshot,
) -> Option<Vec<SlotKey>> {
    let mut keys = vec![SlotKey {
        scope: credentials.owner_scope()?,
        capability: *capability,
    }];
    for grant in grants {
        let Some(GrantDemand::Leased(audience_handle)) =
            classify(grant, capability, &credentials.acting_org, leased)
        else {
            continue;
        };
        let scope = CapabilityAudienceScope::Grant {
            grant_id: grant.grant_id,
            audience_handle,
        };
        // `Public` is the only rejection, and a Grant scope is never public.
        if let Some(scope) = PrivateAudienceScope::new(scope) {
            keys.push(SlotKey {
                scope,
                capability: *capability,
            });
        }
    }
    // The registry deduplicates too; doing it here as well keeps the count this
    // function reports equal to the budget the acquisition actually spends.
    keys.sort();
    keys.dedup();
    Some(keys)
}

/// What replacing `old` with `new` asks the FAMILY budget for.
///
/// The marginal cost, never the gross width: the intersection is kept in place
/// by the replacement transaction, so it is neither released nor re-charged.
/// Saturating, because a replacement can be net-negative — a capability that
/// loses an audience gives a handle back — and a negative charge is simply
/// zero, which no width record can block.
///
/// This is the STATE's gate estimate. The registry recomputes the projection
/// authoritatively inside the transaction and is what actually refuses; this
/// exists only so a family that is provably at its bound stops asking. Both
/// sides are witnessed against the same schedules, so a drift between them
/// shows up as a refusal that the registry does not agree with.
///
/// Both slices are sorted and deduplicated (`demand_set_for` sorts; `demanded`
/// stores what it was acquired from), so membership is a binary search.
fn marginal_charge(old: &[SlotKey], new: &[SlotKey]) -> usize {
    let new_only = new
        .iter()
        .filter(|key| old.binary_search(key).is_err())
        .count();
    let old_only = old
        .iter()
        .filter(|key| new.binary_search(key).is_err())
        .count();
    new_only.saturating_sub(old_only)
}

/// One warmed capability entry: the family's COMPLETE retained demand for it.
///
/// Ownership is the whole content of this type in 2B.3b. Dropping it releases
/// every handle it holds, and the last handle for a slot retires that slot —
/// so an entry is exactly as alive as the demand behind it, with no separate
/// lifecycle to keep in step.
///
/// The demand set is never partial: it is acquired all-or-none (§4.1), so a
/// handle either names every contributor its capability needs or does not
/// exist.
#[allow(dead_code)] // consumer: the family projection (OLB-2B.3d).
pub(crate) struct CapabilityRouteHandle {
    capability: CapabilityAuthorityId,
    /// The publication transition this entry was derived under (§4.4).
    ///
    /// Two different snapshots claiming ONE transition identity is an invariant
    /// breach — the publication seam allocates each identity exactly once — and
    /// this is what makes it detectable rather than silently acted on.
    derived_at: u64,
    /// The EXACT keys `demands` was acquired for, in the same order.
    ///
    /// Retained rather than re-derived because the currency check (§4.2) needs
    /// to know what this entry actually holds, and the only alternative — asking
    /// the registry — is the lock the warmed read exists to avoid. It is the
    /// same `Vec` the acquisition was made from, so the two cannot describe
    /// different sets.
    demanded: Vec<SlotKey>,
    /// The complete demand set, owned as ONE object, in deterministic key order.
    /// Owner first — `SlotKey` sorts by scope, and
    /// `CapabilityAudienceScope::Owner` precedes `Grant` — which is the order
    /// §3.1 projects in.
    demands: DemandSet,
}

#[allow(dead_code)] // consumer: the family projection (OLB-2B.3d).
impl CapabilityRouteHandle {
    pub(crate) fn capability(&self) -> &CapabilityAuthorityId {
        &self.capability
    }

    /// The retained contributors, in projection order.
    pub(crate) fn demands(&self) -> &DemandSet {
        &self.demands
    }

    /// The scopes this entry retains, in the same order as [`Self::demands`].
    pub(crate) fn demanded(&self) -> &[SlotKey] {
        &self.demanded
    }

    /// Whether this entry retains the EXACT `(grant_id, audience_handle)` scope.
    ///
    /// Whole-scope, never `grant_id` alone: an id-keyed check would call a
    /// rotated-away scope retained, which is the aliasing §4 closes on the
    /// derivation side and this closes on the lifecycle side.
    fn retains_grant(&self, grant_id: &[u8; 32], audience_handle: &[u8; 32]) -> bool {
        self.demanded.iter().any(|key| {
            matches!(
                key.scope.scope(),
                CapabilityAudienceScope::Grant {
                    grant_id: retained_id,
                    audience_handle: retained_handle,
                } if retained_id == grant_id && retained_handle == audience_handle
            )
        })
    }
}

/// The family's immutable capability index.
///
/// Rebuilt by copy-on-write under `mutate` and published with one `ArcSwap`
/// store. Readers never see a partially-built index and never take a lock.
#[derive(Default)]
pub(crate) struct CapabilityIndex {
    entries: BTreeMap<CapabilityAuthorityId, Arc<CapabilityRouteHandle>>,
}

impl CapabilityIndex {
    fn get(&self, capability: &CapabilityAuthorityId) -> Option<&Arc<CapabilityRouteHandle>> {
        self.entries.get(capability)
    }

    #[allow(dead_code)] // consumer: the family projection (OLB-2B.3d).
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    fn with(&self, handle: Arc<CapabilityRouteHandle>) -> Self {
        let mut entries = self.entries.clone();
        entries.insert(handle.capability, handle);
        Self { entries }
    }
}

/// What a lookup found, and — when it found nothing — whether acquiring demand
/// is still worth attempting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // consumer: `MeshNode::call` (OLB-2B.3d).
pub(crate) enum RouteLookup {
    /// A warmed entry. The caller holds retained demand for the capability.
    Warm,
    /// No entry, and none was attempted or acquired. The call takes the
    /// current-authority cold path; this is bounded degradation, never failure.
    Cold(ColdReason),
}

/// Where one lease snapshot sits against what this family has already acted on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SnapshotOrder {
    /// Strictly older than the family's high-water. Must neither be served nor
    /// allowed to mutate.
    Older,
    /// The same transition the family last acted on.
    Current,
    /// A later transition; the high-water has been advanced to it.
    Newer,
}

/// Total order over publication transitions, as one comparable integer.
///
/// `Terminal` is the last word the publication-identity space can produce, so it
/// sorts above every ordinary generation. `u64::MAX` is unreachable as an
/// ordinary generation for the same reason it is terminal: the allocator refuses
/// before it could ever be handed out.
fn encode_revision(revision: GrantMovementFence) -> u64 {
    match revision {
        GrantMovementFence::Publication(generation) => generation,
        GrantMovementFence::Terminal => u64::MAX,
    }
}

/// Why a lookup is cold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // consumer: `MeshNode::call` (OLB-2B.3d).
pub(crate) enum ColdReason {
    /// The family has no representable Owner discovery scope.
    NoOwnerScope,
    /// The caller's lease snapshot is OLDER than one this family has already
    /// acted on, or claims a transition identity that a different snapshot
    /// already used (§4.4).
    ///
    /// Never served and never acted on. The caller takes the current-authority
    /// cold path, which is what it would have done had it not raced at all.
    SnapshotSuperseded,
    /// Demand was refused, with the class deciding whether a later miss retries
    /// (§9).
    Refused(DemandRefused),
}

/// Whether a refusal class may ever be attempted again, and on what.
///
/// Three classes, three policies, and they are genuinely different — collapsing
/// them would break in a different direction each time (§9):
///
/// ```text
/// FamilyAtCapacity   sticky FROM THE REFUSED WIDTH UP
/// NodeAtCapacity     retryable, gated on the node capacity generation
/// IdSpaceExhausted   terminal — never retry
/// ```
#[derive(Debug, Default)]
pub(crate) struct RefusalState {
    /// The NARROWEST demand-set width this family has been refused
    /// `FamilyAtCapacity` at, or `None` if it never has.
    ///
    /// A width, not a flag, and the difference is a correctness one. Demand sets
    /// have VARIABLE width — Owner alone is one, Owner plus two leased DISCOVER
    /// audiences is three — and the registry refuses on
    /// `held + width > MAX_HANDLES_PER_FAMILY`. A family holding 62 of 64 that
    /// is refused a width-3 capability still has room for a width-1 one, so a
    /// single flag set by the wide refusal would suppress every narrow set that
    /// still fits, for the family's lifetime, having never asked the registry.
    /// That is not conservatism; the family is simply not at capacity.
    ///
    /// What IS exact is the implication from the refused width UP. The refusal
    /// established `held + refused > 64`, and this state only ever grows its
    /// handle count except when it supersedes an entry (see
    /// [`Self::on_supersession`]), so for any `width >= refused` the same
    /// inequality holds and asking again is provably pointless.
    ///
    /// It converges, so it is not a licence to hammer the registry: an attempt
    /// only reaches the lock when its width is strictly BELOW the recorded one,
    /// and a refusal there lowers the record. Widths are bounded by
    /// [`MAX_HANDLES_PER_FAMILY`], so a family can be refused at most that many
    /// times before `Some(1)` — the old flag's exact behaviour — blocks
    /// everything, since every demand set contains at least the Owner scope.
    family_at_capacity_from: Option<usize>,
    /// Set once the identity space is exhausted.
    ///
    /// TERMINAL, and terminal outranks the retryable class. Exhaustion is
    /// irreversible by construction — the allocator is checked and never wraps —
    /// so a node capacity generation that moves afterwards says nothing about
    /// it. A family that retried on that signal would attempt acquisition
    /// forever on a node that can never satisfy it.
    id_space_exhausted: bool,
    /// The reference-release generation observed at the last REPLACEMENT
    /// refusal, with the class it was refused as.
    ///
    /// A REPLACEMENT gets its own cache, and must not inherit the fresh path's
    /// (§4.5). The fresh caches answer "can this family take MORE", and a
    /// replacement often asks the opposite question:
    ///
    /// ```text
    /// terminal id space  + a replacement creating NO slot   → needs no identity
    /// node full at G     + a net-negative replacement       → frees capacity
    /// node full at G     + a self-crediting rotation        → moves capacity
    /// ```
    ///
    /// Each of those is suppressed by a fresh-path cache that is not about it,
    /// and the suppression is self-sustaining: the very transaction that would
    /// free or move the capacity is the one being refused, so the wake condition
    /// can never arrive. The entry then keeps an obsolete scope forever.
    ///
    /// The gate is the REFERENCE-RELEASE generation, not the node capacity one,
    /// because a replacement also becomes satisfiable when another family stops
    /// SHARING a slot without retiring it — no retirement, no capacity movement,
    /// and yet the projection genuinely changed.
    replace_refused: Option<(u64, DemandRefused)>,
    /// The node capacity generation observed at the last `NodeAtCapacity`
    /// refusal, or `None` if there has not been one.
    ///
    /// A later miss retries only if the generation has MOVED. Equality means no
    /// slot has retired since the refusal, so the node is provably still full
    /// and the attempt would take the registry lock only to be refused again.
    node_at_capacity_at: Option<u64>,
}

impl RefusalState {
    /// Whether the family budget is provably spent for a set of exactly `width`.
    fn family_spent_at(&self, width: usize) -> bool {
        self.family_at_capacity_from
            .is_some_and(|refused| width >= refused)
    }

    /// Whether a FRESH acquisition may be attempted for a set of exactly
    /// `width`, given the CURRENT node capacity generation.
    ///
    /// Unchanged from the signed fresh-path semantics: sticky from the refused
    /// width up, retryable on the node capacity generation, terminal on
    /// exhaustion.
    fn may_attempt(&self, node_capacity_generation: u64, width: usize) -> bool {
        if self.id_space_exhausted || self.family_spent_at(width) {
            return false;
        }
        self.node_at_capacity_at
            .is_none_or(|at| at != node_capacity_generation)
    }

    /// Whether a REPLACEMENT may be attempted, given the current
    /// reference-release generation.
    ///
    /// Deliberately consults NONE of the fresh-path caches — see
    /// [`Self::replace_refused`]. Its own cache is exact for what it gates: a
    /// replacement refused with nothing released since is refused for the same
    /// reason, and any release at all may have changed the projection.
    fn may_attempt_replacement(&self, ref_release_generation: u64) -> bool {
        self.replace_refused
            .is_none_or(|(at, _)| at != ref_release_generation)
    }

    fn record_replacement_refusal(&mut self, refusal: DemandRefused, ref_release_generation: u64) {
        self.replace_refused = Some((ref_release_generation, refusal));
    }

    fn record(&mut self, refusal: DemandRefused, node_capacity_generation: u64, width: usize) {
        match refusal {
            DemandRefused::FamilyAtCapacity => {
                // Keep the NARROWEST. A later, wider refusal says nothing the
                // narrower one did not already say, and taking the wider value
                // would re-open sets already proven not to fit.
                self.family_at_capacity_from = Some(match self.family_at_capacity_from {
                    Some(recorded) => recorded.min(width),
                    None => width,
                });
            }
            DemandRefused::IdSpaceExhausted => self.id_space_exhausted = true,
            DemandRefused::NodeAtCapacity => {
                self.node_at_capacity_at = Some(node_capacity_generation);
            }
        }
    }

    /// Forget the family-capacity record because an entry was SUPERSEDED.
    ///
    /// The width record is sound only while the family's handle count never
    /// falls, which is true of every path except re-derivation: that one
    /// publishes a new demand set and releases the set it replaced, so handles
    /// the earlier refusal counted against may now be free. Terminal exhaustion
    /// and the node generation are untouched — neither is a statement about this
    /// family's budget, and clearing them here would manufacture retries against
    /// a node that is still full or an id space that can never recover.
    fn on_supersession(&mut self) {
        self.family_at_capacity_from = None;
    }
}

/// The clone-family routing state (design §8).
#[allow(dead_code)] // consumer: `OrgClient` (OLB-2B.3d).
pub(crate) struct OrgRoutingState {
    family: RoutingFamily,
    credentials: FamilyDiscoveryCredentials,
    /// The family's credentials indexed by capability, built ONCE at
    /// construction (§4.2).
    ///
    /// This is what keeps the warmed currency check off the family's whole
    /// credential set: a warmed lookup consults only the grants for the one
    /// capability it is asking about. Credentials are fixed for the family's
    /// lifetime, so the index can be built once and never invalidated — the
    /// snapshot that moves is the LEASE state, which is an argument, not a
    /// field.
    grants_by_capability: BTreeMap<CapabilityAuthorityId, Vec<Arc<OrgCapabilityGrant>>>,
    /// The READ path. One atomic load, no lock, ever.
    index: ArcSwap<CapabilityIndex>,
    /// Miss / insert / drop ONLY. Never taken by a hit.
    mutate: parking_lot::Mutex<RefusalState>,
    /// The newest publication transition this family has acted on (§4.4),
    /// encoded by `encode_revision`.
    ///
    /// Node-family-wide rather than per-entry, because an older snapshot must be
    /// refused even for a capability it has never touched: authority it still
    /// believes in may already have been withdrawn. Per-entry freshness is
    /// carried separately by `CapabilityRouteHandle::derived_at`, which is what
    /// detects two DIFFERENT snapshots claiming one transition identity.
    snapshot_high_water: AtomicU64,
    /// Every acquisition of `mutate`, so a witness can assert that a warmed
    /// lookup takes it ZERO times rather than inferring it from timing.
    ///
    /// An end-to-end counter and a real contention witness prove different
    /// things and the design asks for both: the counter cannot distinguish "no
    /// lock" from "an uncontended lock", and the contention witness cannot see
    /// a lock taken on a path it did not exercise.
    mutate_acquisitions: AtomicU64,
}

#[allow(dead_code)] // consumer: `OrgClient` (OLB-2B.3d).
impl OrgRoutingState {
    pub(crate) fn new(family: RoutingFamily, credentials: FamilyDiscoveryCredentials) -> Self {
        let mut grants_by_capability: BTreeMap<
            CapabilityAuthorityId,
            Vec<Arc<OrgCapabilityGrant>>,
        > = BTreeMap::new();
        for grant in &credentials.grants {
            // Indexed on capability ONLY. `classify` still decides grantee,
            // rights and lease, so the index cannot quietly become a second
            // admission rule that disagrees with the derivation.
            grants_by_capability
                .entry(grant.capability)
                .or_default()
                .push(grant.clone());
        }
        Self {
            family,
            credentials,
            grants_by_capability,
            index: ArcSwap::from_pointee(CapabilityIndex::default()),
            snapshot_high_water: AtomicU64::new(0),
            mutate: parking_lot::Mutex::new(RefusalState::default()),
            mutate_acquisitions: AtomicU64::new(0),
        }
    }

    /// The family's credentials for exactly `capability`, in credential order.
    fn grants_for(
        &self,
        capability: &CapabilityAuthorityId,
    ) -> impl Iterator<Item = &Arc<OrgCapabilityGrant>> {
        self.grants_by_capability
            .get(capability)
            .into_iter()
            .flatten()
    }

    /// This family's demand set for `capability` under `leased`, derived from
    /// the capability index.
    fn demand_set(
        &self,
        capability: &CapabilityAuthorityId,
        leased: &ConsumerGrantSnapshot,
    ) -> Option<Vec<SlotKey>> {
        demand_set_from(
            &self.credentials,
            self.grants_for(capability),
            capability,
            leased,
        )
    }

    /// Whether `entry` still names the family's COMPLETE current demand set
    /// (§4.2). Lock-free, and bounded by this one capability's grants.
    ///
    /// Both directions, because they fail differently and a check that ran only
    /// one of them would miss half the lifecycle:
    ///
    /// ```text
    /// retained but no longer leased   →  OBSOLETE  (removal, rotation)
    /// leased but not retained         →  MISSING   (installation, rotation)
    /// ```
    ///
    /// Counting is deliberately avoided in favour of membership in both
    /// directions: two credential entries naming the same scope deduplicate into
    /// ONE key, so a count comparison would report a permanent mismatch and
    /// re-derive on every call forever.
    fn leases_current(
        &self,
        entry: &CapabilityRouteHandle,
        capability: &CapabilityAuthorityId,
        leased: &ConsumerGrantSnapshot,
    ) -> bool {
        // Nothing retained has since been removed or rotated away. Compared on
        // the whole scope, so an id still installed under a DIFFERENT handle is
        // correctly not this scope.
        for key in &entry.demanded {
            let CapabilityAudienceScope::Grant {
                grant_id,
                audience_handle,
            } = key.scope.scope()
            else {
                continue;
            };
            let still_leased = leased
                .get(grant_id)
                .is_some_and(|record| record.audience_handle() == audience_handle);
            if !still_leased {
                return false;
            }
        }
        // And nothing newly leased is missing from it.
        for grant in self.grants_for(capability) {
            let Some(GrantDemand::Leased(audience_handle)) =
                classify(grant, capability, &self.credentials.acting_org, leased)
            else {
                continue;
            };
            if !entry.retains_grant(&grant.grant_id, &audience_handle) {
                return false;
            }
        }
        true
    }

    /// The RETAINED read: one atomic load, one map lookup, one `Arc` clone.
    ///
    /// Takes no lock — not this state's, not the registry's, not the SDK's.
    /// This is the path a warmed call runs, and it is the reason the index is
    /// an immutable snapshot behind an `ArcSwap` rather than a map with a lock
    /// in front of it.
    ///
    /// Retention ONLY: it answers "what does this family hold for `capability`",
    /// not "is that still the complete current demand set". Currency is
    /// [`Self::route_handle`]'s, because currency is a question about the lease
    /// snapshot and this method is not given one. Named for what it does — an
    /// accessor that looked current and was not is the trap
    /// `base_facts_unvalidated` was renamed to avoid.
    pub(crate) fn warm(
        &self,
        capability: &CapabilityAuthorityId,
    ) -> Option<Arc<CapabilityRouteHandle>> {
        self.index.load().get(capability).cloned()
    }

    /// Look the capability up and, on a miss OR a stale lease view, acquire its
    /// COMPLETE current demand set.
    ///
    /// The hit is checked twice by design. Once lock-free, which is the whole
    /// hot path; then again under `mutate`, because two threads can miss
    /// concurrently and only one of them may spend the family's budget — the
    /// loser must adopt the winner's entry rather than acquire a second,
    /// duplicate demand set for the same capability.
    ///
    /// A warmed entry is returned only while it is CURRENT (§4.2). The lease
    /// state is an argument, not a field, so it can move between two calls for
    /// the same capability; an entry whose Grant plane no longer matches what
    /// the family has leased is re-derived rather than answered from.
    pub(crate) fn route_handle(
        &self,
        capability: &CapabilityAuthorityId,
        leased: &ConsumerGrantSnapshot,
    ) -> RouteLookup {
        // SNAPSHOT ORDERING FIRST (§4.4). A caller whose view is older than one
        // this family has already acted on is answered `SnapshotSuperseded` and
        // nothing else: it must not be served from a newer entry, because a
        // downstream projection would then read the newer retained authority as
        // if it were this older snapshot's; and it must not re-derive, because
        // that would reinstate authority a later transition already withdrew.
        match self.note_snapshot(leased) {
            SnapshotOrder::Older => return RouteLookup::Cold(ColdReason::SnapshotSuperseded),
            SnapshotOrder::Current | SnapshotOrder::Newer => {}
        }
        if let Some(entry) = self.warm(capability) {
            if self.leases_current(&entry, capability, leased) {
                return RouteLookup::Warm;
            }
        }
        self.acquire(capability, leased)
    }

    /// Where `leased` sits against everything this family has already acted on,
    /// advancing the high-water when it is newer.
    ///
    /// Lock-free, and free in the steady state: a repeat call under the same
    /// snapshot is one `Acquire` load and no write. Freshness advances even when
    /// the movement is IRRELEVANT to this capability and no re-derivation
    /// follows — otherwise an unrelated newer publication would leave the
    /// high-water behind and let a stalled older snapshot overwrite it later.
    fn note_snapshot(&self, leased: &ConsumerGrantSnapshot) -> SnapshotOrder {
        let revision = encode_revision(leased.revision());
        let high_water = self.snapshot_high_water.load(Ordering::Acquire);
        if revision < high_water {
            return SnapshotOrder::Older;
        }
        if revision > high_water {
            self.snapshot_high_water
                .fetch_max(revision, Ordering::AcqRel);
            return SnapshotOrder::Newer;
        }
        SnapshotOrder::Current
    }

    /// The MUTATION path: a miss, or a warmed entry that is stale for THIS
    /// caller's snapshot.
    ///
    /// One path, not two. An earlier shape had the miss path re-check only
    /// `is_some()` under the lock and return `Warm` — so a thread that lost the
    /// race adopted the winner's entry without ever asking whether that entry
    /// was current for ITS OWN snapshot, and reported `Warm` for a set missing a
    /// newly installed audience or still holding a removed one (§4.6).
    fn acquire(
        &self,
        capability: &CapabilityAuthorityId,
        leased: &ConsumerGrantSnapshot,
    ) -> RouteLookup {
        let mut refusals = self.mutate.lock();
        self.mutate_acquisitions.fetch_add(1, Ordering::AcqRel);

        // Re-check the SNAPSHOT ORDER under the lock (§4.4). `route_handle`
        // checked it too, but the high-water can advance in the window between
        // that check and this lock: a newer caller can publish while this one is
        // still on its way in. Ordering it only outside the lock would make the
        // gate exactly as weak as the serialization it exists to correct.
        if encode_revision(leased.revision()) < self.snapshot_high_water.load(Ordering::Acquire) {
            return RouteLookup::Cold(ColdReason::SnapshotSuperseded);
        }

        // Re-check under the mutation lock, against the entry the index holds
        // NOW — never one loaded before taking it, which may already have
        // transferred its references away. A concurrent caller may have inserted
        // or re-derived this capability, and if what it published is current for
        // this caller too, there is nothing to do and no budget to spend twice.
        let current = self.index.load().get(capability).cloned();
        match current {
            Some(current) => {
                if self.leases_current(&current, capability, leased) {
                    return RouteLookup::Warm;
                }
                self.acquire_under_mutate(&mut refusals, capability, leased, Some(&current))
            }
            None => self.acquire_under_mutate(&mut refusals, capability, leased, None),
        }
    }

    /// Derive, gate, acquire and publish, under a held `mutate`.
    ///
    /// `superseded` is the entry this acquisition REPLACES, if any. A
    /// replacement goes through the registry's atomic replacement rather than
    /// acquiring a second set beside the first: the family and the node are
    /// charged on the PROJECTED FINAL footprint, so a capability that loses an
    /// audience at 64 of 64 projects to `64 - 2 + 1 = 63` and succeeds, where
    /// charging it gross would ask for 66 and refuse a replacement that plainly
    /// fits (§4.3). Refusal is still total no-effect, because the superseded set
    /// is borrowed and keeps everything it owns unless the transaction commits.
    fn acquire_under_mutate(
        &self,
        refusals: &mut RefusalState,
        capability: &CapabilityAuthorityId,
        leased: &ConsumerGrantSnapshot,
        superseded: Option<&Arc<CapabilityRouteHandle>>,
    ) -> RouteLookup {
        // Derived BEFORE the refusal gate, because the gate is on what the set
        // COSTS and that is not knowable without deriving. The derivation is
        // pure and takes no registry lock, so a settled family pays a bounded
        // walk of one capability's grants and never reaches the lock itself.
        let Some(keys) = self.demand_set(capability, leased) else {
            return RouteLookup::Cold(ColdReason::NoOwnerScope);
        };

        // What this attempt asks the family budget for. A fresh acquisition asks
        // for its whole width; a replacement asks only for its MARGINAL cost,
        // because the intersection is never given up and re-taken. A replacement
        // that is net-neutral or net-negative charges zero and is therefore
        // never blocked by a width record — which is exactly right, since it
        // cannot make the family's footprint grow.
        let charge = match superseded {
            Some(stale) => marginal_charge(stale.demanded(), &keys),
            None => keys.len(),
        };

        // The node capacity generation is read ONCE, before the attempt, and
        // the same value is what a refusal records. Reading it after a refusal
        // would record a generation that may already have moved because of the
        // very retirement that would have made the attempt succeed, and the
        // family would then never retry.
        let revision = encode_revision(leased.revision());

        // Two DIFFERENT snapshots claiming ONE transition identity is an
        // invariant breach, not a race: the publication seam allocates each
        // identity exactly once, under the consumer-Grant gate. If this entry
        // was derived under the very transition the caller names and the derived
        // set has nevertheless changed, the two disagree about what that
        // transition published. Fail closed rather than pick one.
        if superseded.is_some_and(|stale| stale.derived_at == revision) {
            return RouteLookup::Cold(ColdReason::SnapshotSuperseded);
        }

        // A REPLACEMENT is gated on the reference-release generation, a FRESH
        // acquisition on the node capacity generation and the fresh caches
        // (§4.5). Applying the fresh caches to a replacement deadlocks it
        // against its own wake condition: the transaction being suppressed is
        // the one that would free or move the very capacity the cache is
        // waiting on.
        let release_generation = self.family.ref_release_generation();
        if superseded.is_some() {
            if !refusals.may_attempt_replacement(release_generation) {
                let refusal = refusals
                    .replace_refused
                    .map_or(DemandRefused::NodeAtCapacity, |(_, refusal)| refusal);
                return RouteLookup::Cold(ColdReason::Refused(refusal));
            }
        } else {
            let generation = self.family.node_capacity_generation();
            if !refusals.may_attempt(generation, charge) {
                return RouteLookup::Cold(self.settled_reason(refusals, charge));
            }
        }
        let generation = self.family.node_capacity_generation();

        // A replacement is driven through the SUPERSEDED SET ITSELF, which
        // carries its own registry and family identity. No argument exists
        // through which one family could offer another's set as the basis, so
        // the private ownership boundary is structural here, not checked.
        let acquired = match superseded {
            Some(stale) => stale
                .demands()
                .replace(keys.clone())
                .map_err(|refusal| match refusal {
                    ReplaceRefused::Demand(refusal) => refusal,
                    // The set gave its references away between the index read
                    // and here. Unreachable while `mutate` serializes every
                    // replacement, and answered as the most conservative
                    // capacity refusal rather than a panic: it retains nothing,
                    // and the caller's next attempt re-reads the index.
                    ReplaceRefused::Superseded => DemandRefused::NodeAtCapacity,
                }),
            None => self.family.demand_set(keys.clone()),
        };
        match acquired {
            Ok(demands) => {
                let handle = Arc::new(CapabilityRouteHandle {
                    capability: *capability,
                    derived_at: revision,
                    demanded: keys,
                    demands,
                });
                // Copy-on-write publication: readers hold the previous snapshot
                // until this store, and the new one the moment after it. There
                // is no window in which the index is half-built, because the
                // index a reader can reach was never mutated in place.
                self.index.store(Arc::new(self.index.load().with(handle)));
                // The family has now ACTED on this transition. Advanced here as
                // well as on the read path, because acting is what the ordering
                // invariant is about: a later caller carrying an older view must
                // be refused even if no read ever observed the newer one.
                self.snapshot_high_water
                    .fetch_max(revision, Ordering::AcqRel);
                if superseded.is_some() {
                    // The superseded set gave up its references INSIDE the
                    // replacement transaction, so the family's footprint may
                    // have fallen and a width record taken against the old one
                    // no longer describes it.
                    refusals.on_supersession();
                }
                RouteLookup::Warm
            }
            Err(refusal) => {
                if superseded.is_some() {
                    // A replacement refusal records against the release
                    // generation and NEVER against the fresh caches: a
                    // replacement that could not fit says nothing about whether
                    // this family may take more, and writing it into the sticky
                    // width or the terminal flag would suppress fresh
                    // acquisitions that are still perfectly serviceable.
                    refusals.record_replacement_refusal(refusal, release_generation);
                } else {
                    refusals.record(refusal, generation, charge);
                }
                RouteLookup::Cold(ColdReason::Refused(refusal))
            }
        }
    }

    /// The refusal a settled family reports without attempting acquisition.
    ///
    /// Terminal before spent-at-this-width before retryable, so the reported
    /// reason is the one that actually stops the attempt.
    fn settled_reason(&self, refusals: &RefusalState, width: usize) -> ColdReason {
        if refusals.id_space_exhausted {
            ColdReason::Refused(DemandRefused::IdSpaceExhausted)
        } else if refusals.family_spent_at(width) {
            ColdReason::Refused(DemandRefused::FamilyAtCapacity)
        } else {
            ColdReason::Refused(DemandRefused::NodeAtCapacity)
        }
    }

    /// Capability entries currently retained.
    pub(crate) fn entries(&self) -> usize {
        self.index.load().len()
    }

    /// Demand handles this family currently holds, across all entries.
    pub(crate) fn handles(&self) -> usize {
        self.family.handles()
    }

    /// How many times `mutate` has been acquired.
    pub(crate) fn mutate_acquisitions(&self) -> u64 {
        self.mutate_acquisitions.load(Ordering::Acquire)
    }

    /// Test-only: the mutation lock itself, so a contention witness can hold
    /// the REAL lock rather than a stand-in and prove a contender's `try_lock`
    /// fails while a warmed lookup completes.
    #[cfg(test)]
    pub(crate) fn mutate_lock_for_test(&self) -> &parking_lot::Mutex<RefusalState> {
        &self.mutate
    }
}

#[cfg(test)]
#[path = "org_routing_state_tests.rs"]
mod tests;
