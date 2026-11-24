//! Coalesced (leader-backed) organization sensing at the CONSUMER — the
//! election that names a capability's org sensing leader, and the node-owned
//! demand registry that keeps a provider-free org interest registered with it
//! (LS-7a; `docs/internal/plans/ORG_SENSING_LEADER_SDK_PLAN.md` D-LS15/D-LS16).
//!
//! # The election (D-LS15)
//!
//! Every observer computes the same leader from its own view, so no consensus
//! and no second election subsystem exist:
//!
//! - the ELECTORATE is the owner-private announcers this node's verified
//!   discovery holds (plus this node when it announces owner-scoped services),
//!   MINUS the providers of the watched capability. A leader excludes itself
//!   from its own candidate projection (leader plan Q2, v1), so electing a
//!   provider of the capability would hide that provider from every consumer —
//!   in a small org, all of them;
//! - it is bounded to [`MAX_ORG_LEADER_ELECTORATE`] members by a rendezvous
//!   hash of `(capability, node_id)`: deterministic for every observer, and it
//!   spreads leaders across capabilities;
//! - the rank is [`sensing_leader`] — the
//!   existing RedEX election over shared proximity closeness — with this
//!   node's reachability as the health filter;
//! - a consumer never registers with ITSELF: if it wins, it uses the
//!   next-ranked member. Other consumers still send to it, and it serves them,
//!   because every coalescing node holds the org leader role (D-LS14).
//!
//! # The demand registry (D-LS16)
//!
//! One entry per interest key, its holders counted by token. The node's
//! refresh task re-elects and re-registers every entry at `ttl/2`; a changed
//! leader is re-registered at once (failover), and the old leader's row
//! expires by soft-state TTL. The last release removes the entry; the caller
//! then drops the consumer expectation. Nothing here touches the wire, a lock
//! other than its own, or the exact-provider lease registry.

use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

use super::identity::{CapabilityInterestKey, InterestSpec};
use super::rendezvous::sensing_leader;

/// The most members one capability's election ranks (D-LS15). Bounds the
/// closeness computation at `32 × 31` edge reads per election.
pub const MAX_ORG_LEADER_ELECTORATE: usize = 32;

/// The largest `TopK` a coalesced interest may ask (owner decision 3): the
/// exact watch's own population cap, so a coalesced watch never reports more
/// rows than an exact one could.
pub const MAX_COALESCED_TOP_K: u16 = 32;

/// One electorate member: a node id and the verified entity it derives from.
/// The entity is what the health filter's pin check compares against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ElectorateMember<E> {
    pub node_id: u64,
    pub entity: E,
}

/// The rendezvous weight of `node_id` for `capability`: lower wins a place in
/// the bounded electorate. Keyed on the capability so different capabilities
/// draw different (deterministic) electorates.
fn electorate_weight(capability: &str, node_id: u64) -> u64 {
    let mut hasher = blake3::Hasher::new_derive_key("net.sensing.org_leader_electorate.v1");
    hasher.update(&(capability.len() as u64).to_le_bytes());
    hasher.update(capability.as_bytes());
    hasher.update(&node_id.to_le_bytes());
    let digest = hasher.finalize();
    let mut weight = [0u8; 8];
    weight.copy_from_slice(&digest.as_bytes()[..8]);
    u64::from_le_bytes(weight)
}

/// Bound an electorate to [`MAX_ORG_LEADER_ELECTORATE`] members by rendezvous
/// weight, deduplicated by node id, in ascending node-id order. Pure: every
/// observer holding the same member set derives the same bounded set.
pub(crate) fn bound_electorate<E: Clone>(
    capability: &str,
    members: impl IntoIterator<Item = ElectorateMember<E>>,
) -> Vec<ElectorateMember<E>> {
    let mut by_node: HashMap<u64, ElectorateMember<E>> = HashMap::new();
    for member in members {
        by_node.entry(member.node_id).or_insert(member);
    }
    let mut weighted: Vec<(u64, ElectorateMember<E>)> = by_node
        .into_values()
        .map(|member| (electorate_weight(capability, member.node_id), member))
        .collect();
    weighted.sort_by_key(|(weight, member)| (*weight, member.node_id));
    weighted.truncate(MAX_ORG_LEADER_ELECTORATE);
    let mut bounded: Vec<ElectorateMember<E>> =
        weighted.into_iter().map(|(_, member)| member).collect();
    bounded.sort_by_key(|member| member.node_id);
    bounded
}

/// Elect the org sensing leader THIS node should register with, over an
/// already-bounded electorate (D-LS15).
///
/// `rtt_between` is the SHARED proximity view (symmetric), `healthy` is this
/// node's reachability judgement (`self_id` is always healthy). If this node
/// wins, the next-ranked healthy member is returned instead: a consumer never
/// registers with itself. `None` when no other member is healthy — there is no
/// leader to register with.
pub(crate) fn elect_org_leader<F, H>(
    electorate: &[u64],
    self_id: u64,
    rtt_between: F,
    healthy: H,
) -> Option<u64>
where
    F: Fn(u64, u64) -> Option<Duration>,
    H: Fn(u64) -> bool,
{
    let health = |node: u64| node == self_id || healthy(node);
    let winner = sensing_leader(electorate, &rtt_between, health)?;
    if winner != self_id {
        return Some(winner);
    }
    // The next-ranked member: the same election with this node ineligible.
    // Scores are computed over the FULL electorate (closeness_score), so
    // removing self from eligibility alone reorders nothing else.
    sensing_leader(electorate, &rtt_between, |node| {
        node != self_id && healthy(node)
    })
}

/// A consumer's hold on one coalesced org interest. Released exactly once
/// through the node; a released or foreign ticket releases nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrgCoalescedTicket {
    key: CapabilityInterestKey,
    token: u64,
}

impl OrgCoalescedTicket {
    /// The interest this ticket holds — the identity its leader-delivered
    /// consumer cells are keyed under.
    pub fn interest(&self) -> &CapabilityInterestKey {
        &self.key
    }
}

/// Why a coalesced interest could not be held.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrgCoalescedRefused {
    /// The sensing plane is off on this node.
    Disabled,
    /// No live organization authority (or a poisoned/exhausted store): there is
    /// no audience to derive and no membership to register under.
    NoAuthority,
    /// No member of this node's organization can lead this capability right
    /// now: the electorate (announcers minus the capability's providers) has no
    /// healthy member other than this node.
    NoLeader,
    /// The coalesced registry, or the consumer expectation map it feeds, is
    /// full. Nothing is evicted.
    AtCapacity,
    /// The result mode is not one a coalesced interest supports
    /// (`Any`, or `TopK(1..=32)`).
    UnsupportedMode,
}

/// One held interest.
#[derive(Debug)]
pub(crate) struct OrgCoalescedEntry {
    pub spec: InterestSpec,
    pub interval: Duration,
    holders: BTreeSet<u64>,
    /// The leader the last registration went to.
    pub leader: Option<u64>,
    /// When the last registration was sent; `None` until the first.
    pub registered_at: Option<Instant>,
}

/// What a release did.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum OrgCoalescedRelease {
    /// The ticket held nothing (already released, or never this registry's).
    NotHeld,
    /// Other holders remain; the interest stays registered.
    Survives,
    /// This was the last holder: the entry is gone, and the caller drops the
    /// consumer expectation for `key`.
    Last(CapabilityInterestKey),
}

/// The node-owned registry of coalesced org interests (D-LS16). Guarded by its
/// own mutex in the node; every operation is map work only.
#[derive(Debug, Default)]
pub(crate) struct OrgCoalescedRegistry {
    entries: HashMap<CapabilityInterestKey, OrgCoalescedEntry>,
    next_token: u64,
}

impl OrgCoalescedRegistry {
    /// Hold `spec`. Returns the ticket and whether this hold CREATED the entry
    /// (the caller then registers it). A new entry over `capacity` is refused.
    pub(crate) fn hold(
        &mut self,
        spec: &InterestSpec,
        interval: Duration,
        capacity: usize,
    ) -> Result<(OrgCoalescedTicket, bool), OrgCoalescedRefused> {
        let key = spec.key();
        let created = !self.entries.contains_key(&key);
        if created && self.entries.len() >= capacity {
            return Err(OrgCoalescedRefused::AtCapacity);
        }
        self.next_token = self.next_token.wrapping_add(1);
        let token = self.next_token;
        let entry = self
            .entries
            .entry(key.clone())
            .or_insert_with(|| OrgCoalescedEntry {
                spec: spec.clone(),
                interval,
                holders: BTreeSet::new(),
                leader: None,
                registered_at: None,
            });
        entry.holders.insert(token);
        Ok((OrgCoalescedTicket { key, token }, created))
    }

    /// Undo a hold that the caller could not complete (its first registration
    /// was refused). Same effect as [`Self::release`].
    pub(crate) fn release(&mut self, ticket: &OrgCoalescedTicket) -> OrgCoalescedRelease {
        let Some(entry) = self.entries.get_mut(&ticket.key) else {
            return OrgCoalescedRelease::NotHeld;
        };
        if !entry.holders.remove(&ticket.token) {
            return OrgCoalescedRelease::NotHeld;
        }
        if !entry.holders.is_empty() {
            return OrgCoalescedRelease::Survives;
        }
        self.entries.remove(&ticket.key);
        OrgCoalescedRelease::Last(ticket.key.clone())
    }

    /// Whether `ticket` is a live holder.
    pub(crate) fn holds(&self, ticket: &OrgCoalescedTicket) -> bool {
        self.entries
            .get(&ticket.key)
            .is_some_and(|entry| entry.holders.contains(&ticket.token))
    }

    /// The leader `ticket`'s interest last registered with.
    pub(crate) fn leader_of(&self, ticket: &OrgCoalescedTicket) -> Option<u64> {
        self.entries
            .get(&ticket.key)
            .filter(|entry| entry.holders.contains(&ticket.token))
            .and_then(|entry| entry.leader)
    }

    /// Every held interest as `(key, spec, interval, leader, registered_at)`,
    /// for one refresh pass. Cloned out so the pass registers with the
    /// registry lock released.
    #[allow(clippy::type_complexity)]
    pub(crate) fn snapshot(
        &self,
    ) -> Vec<(
        CapabilityInterestKey,
        InterestSpec,
        Duration,
        Option<u64>,
        Option<Instant>,
    )> {
        self.entries
            .iter()
            .map(|(key, entry)| {
                (
                    key.clone(),
                    entry.spec.clone(),
                    entry.interval,
                    entry.leader,
                    entry.registered_at,
                )
            })
            .collect()
    }

    /// Record a registration sent to `leader` (or `None`: no leader was
    /// electable, nothing was sent). A key released meanwhile is ignored.
    pub(crate) fn note_registered(
        &mut self,
        key: &CapabilityInterestKey,
        leader: Option<u64>,
        at: Option<Instant>,
    ) {
        if let Some(entry) = self.entries.get_mut(key) {
            entry.leader = leader;
            entry.registered_at = at;
        }
    }

    /// The number of held interests.
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is held.
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Whether one held interest is due a registration at `now`: never sent, its
/// leader changed, or `refresh_every` has elapsed since the last send.
pub(crate) fn registration_due(
    elected: Option<u64>,
    registered_with: Option<u64>,
    registered_at: Option<Instant>,
    now: Instant,
    refresh_every: Duration,
) -> bool {
    if elected.is_none() {
        return false;
    }
    if elected != registered_with {
        return true;
    }
    registered_at.is_none_or(|at| now.saturating_duration_since(at) >= refresh_every)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::sensing::identity::{
        AudienceScopeCommitment, CanonicalConstraints, CapabilityId, DisclosureClass,
        ProviderSelector, ResultMode, WorkLatencyEnvelope,
    };

    fn member(node_id: u64) -> ElectorateMember<u64> {
        ElectorateMember {
            node_id,
            entity: node_id,
        }
    }

    fn ids(members: &[ElectorateMember<u64>]) -> Vec<u64> {
        members.iter().map(|member| member.node_id).collect()
    }

    /// A symmetric proximity view from an explicit edge list (µs).
    fn view(edges: &[(u64, u64, u64)]) -> impl Fn(u64, u64) -> Option<Duration> + '_ {
        move |a, b| {
            edges
                .iter()
                .find(|(x, y, _)| (*x == a && *y == b) || (*x == b && *y == a))
                .map(|(_, _, micros)| Duration::from_micros(*micros))
        }
    }

    #[test]
    fn the_electorate_is_bounded_deterministically_and_deduplicated() {
        let many: Vec<ElectorateMember<u64>> = (1..=100).map(member).collect();
        let bounded = bound_electorate("nrpc:infer", many.clone());
        assert_eq!(bounded.len(), MAX_ORG_LEADER_ELECTORATE);
        // The same members in any order, with duplicates, bound identically —
        // which is what lets every observer agree.
        let mut shuffled = many.clone();
        shuffled.reverse();
        shuffled.extend(many.iter().take(10).cloned());
        assert_eq!(
            ids(&bound_electorate("nrpc:infer", shuffled)),
            ids(&bounded)
        );
        // Ascending node order, so the election input is canonical.
        let order = ids(&bounded);
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(order, sorted);
    }

    #[test]
    fn different_capabilities_draw_different_electorates() {
        let many: Vec<ElectorateMember<u64>> = (1..=200).map(member).collect();
        let a = ids(&bound_electorate("nrpc:a", many.clone()));
        let b = ids(&bound_electorate("nrpc:b", many));
        assert_ne!(a, b, "leaders spread across capabilities");
    }

    #[test]
    fn a_small_electorate_is_kept_whole() {
        let bounded = bound_electorate("nrpc:infer", [member(9), member(3), member(5)]);
        assert_eq!(ids(&bounded), vec![3, 5, 9]);
    }

    #[test]
    fn every_observer_of_one_view_elects_the_same_leader() {
        // 2 is the centre of a star.
        let edges = [(1, 2, 100), (2, 3, 100), (2, 4, 100), (1, 3, 900)];
        let electorate = [1, 2, 3, 4];
        for observer in [10, 11, 12] {
            assert_eq!(
                elect_org_leader(&electorate, observer, view(&edges), |_| true),
                Some(2),
                "observer {observer} agrees"
            );
        }
    }

    #[test]
    fn a_consumer_never_elects_itself() {
        let edges = [(1, 2, 100), (2, 3, 100), (2, 4, 100)];
        let electorate = [1, 2, 3, 4];
        // Node 2 wins for everyone else; node 2 itself registers with the
        // next-ranked member instead.
        let own = elect_org_leader(&electorate, 2, view(&edges), |_| true);
        assert!(own.is_some());
        assert_ne!(own, Some(2));
        assert_eq!(
            own,
            elect_org_leader(&electorate, 99, view(&edges), |node| node != 2),
            "self's fallback is exactly the next-ranked member"
        );
    }

    #[test]
    fn an_unreachable_winner_yields_to_the_next_member_and_none_is_no_leader() {
        let edges = [(1, 2, 100), (2, 3, 100)];
        let electorate = [1, 2, 3];
        let next = elect_org_leader(&electorate, 99, view(&edges), |node| node != 2);
        assert!(matches!(next, Some(1) | Some(3)));
        assert_eq!(
            elect_org_leader(&electorate, 99, view(&edges), |_| false),
            None,
            "nobody healthy: no leader"
        );
        assert_eq!(
            elect_org_leader(&[7], 7, view(&edges), |_| true),
            None,
            "an electorate of only this node has no leader for it"
        );
        assert_eq!(elect_org_leader(&[], 7, view(&edges), |_| true), None);
    }

    fn spec(capability: &str) -> InterestSpec {
        InterestSpec {
            capability_id: CapabilityId::new(capability),
            constraints: CanonicalConstraints::default(),
            work_latency: WorkLatencyEnvelope::start_within(Duration::from_secs(2)),
            providers: ProviderSelector::AnyAuthorized,
            result_mode: ResultMode::Any,
            disclosure_class: DisclosureClass::Owner,
            audience: AudienceScopeCommitment::from_bytes([7; 32]),
        }
    }

    #[test]
    fn holders_share_one_entry_and_only_the_last_release_retires_it() {
        let mut registry = OrgCoalescedRegistry::default();
        let interval = Duration::from_secs(2);
        let (a, created_a) = registry.hold(&spec("nrpc:x"), interval, 8).unwrap();
        let (b, created_b) = registry.hold(&spec("nrpc:x"), interval, 8).unwrap();
        assert!(created_a && !created_b);
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.release(&a), OrgCoalescedRelease::Survives);
        assert_eq!(
            registry.release(&a),
            OrgCoalescedRelease::NotHeld,
            "idempotent"
        );
        assert!(registry.holds(&b));
        assert_eq!(
            registry.release(&b),
            OrgCoalescedRelease::Last(spec("nrpc:x").key())
        );
        assert!(registry.is_empty());
        assert_eq!(registry.release(&b), OrgCoalescedRelease::NotHeld);
    }

    #[test]
    fn a_new_interest_over_capacity_is_refused_but_an_existing_one_joins() {
        let mut registry = OrgCoalescedRegistry::default();
        let interval = Duration::from_secs(2);
        registry.hold(&spec("nrpc:x"), interval, 1).unwrap();
        assert_eq!(
            registry.hold(&spec("nrpc:y"), interval, 1).unwrap_err(),
            OrgCoalescedRefused::AtCapacity
        );
        assert!(registry.hold(&spec("nrpc:x"), interval, 1).is_ok());
    }

    #[test]
    fn a_registration_is_due_on_first_send_leader_change_or_refresh() {
        let now = Instant::now();
        let every = Duration::from_secs(5);
        assert!(
            registration_due(Some(1), None, None, now, every),
            "first send"
        );
        assert!(
            registration_due(Some(2), Some(1), Some(now), now, every),
            "failover is immediate"
        );
        assert!(!registration_due(Some(1), Some(1), Some(now), now, every));
        assert!(registration_due(
            Some(1),
            Some(1),
            Some(now - every),
            now,
            every
        ));
        assert!(
            !registration_due(None, Some(1), None, now, every),
            "no leader, nothing to send"
        );
    }
}
