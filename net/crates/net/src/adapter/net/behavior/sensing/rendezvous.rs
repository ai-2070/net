//! Capability-interest rendezvous: the RedEX-elected sensing leader
//! (plan §4.1, review 6).
//!
//! Provider-free capability interests need a destination; Net
//! already ships the primitive. This module REUSES
//! [`elect`](super::super::super::redex::elect) — the pure,
//! deterministic, health-filtered `(key, NodeId)` ranking whose
//! "next-ranked healthy node wins on leader loss" is exactly the
//! bully fallback — with two parameter changes and zero algorithm
//! changes:
//!
//! - the ranking key is a **shared closeness-centrality score** over
//!   the shared, pingwave-flooded proximity view (RedEX ranks by
//!   self-anchored RTT: follow-the-nearest, self-bias intended);
//! - the observer id is a **non-member sentinel**, which disables
//!   `elect`'s self-RTT-zero bias, so every node in the scope
//!   computes the identical winner from the identical view.
//!
//! The leader is island-relative, never a truth oracle: partitions
//! elect one leader per island, duplicate provider streams are
//! tolerated and expire, and failover is soft-state re-registration
//! (plan §4.1). If sensing ever needs terms/epochs, that lands in
//! RedEX — never a second election subsystem (SI-0 item 31).
//!
//! [`SensingLeader`] is the leader ROLE: rendezvous, deduplicator,
//! bounded candidate resolver, and fan-out point — composed entirely
//! from the existing spike pieces (`resolve_candidates` +
//! `SensingRelay`). The provider remains the authority (it signs the
//! proofs) and each consumer remains the judge of its own path
//! viability (§3.5).
//!
//! Feature note: this module rides the `redex` feature because the
//! reuse is real, not copied. SI-2 revisits the final layering.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::super::super::redex::{elect, ElectionOutcome};
use super::controller::{
    resolve_candidates, CandidatePolicy, CandidateProvider, ResolutionRefusal,
};
use super::delivery::{Delivery, SensingRelay};
use super::identity::{
    AudienceScopeCommitment, CapabilityInterestKey, InterestSpec, ProviderInterestKey,
};
use super::table::{DownstreamId, UpstreamAction};

/// Observer id fed to [`elect`]: never a member, so the
/// self-RTT-zero bias can't fire and the ranking is identical for
/// every real observer. Members MUST NOT contain this value.
const RENDEZVOUS_OBSERVER: u64 = u64::MAX;

/// Ranking penalty for a member pair with no shared-view RTT sample:
/// large enough to push unknown-connectivity members to the back,
/// finite so sums stay comparable (saturating arithmetic caps the
/// pathological case).
const UNKNOWN_EDGE_PENALTY: Duration = Duration::from_secs(3600);

/// Closeness-centrality score for one member over the SHARED
/// proximity view: the sum of its RTTs to every other member
/// (missing samples take [`UNKNOWN_EDGE_PENALTY`]). Lower = more
/// central. Computed over the full member set — health changes
/// affect *eligibility*, never scores, so a leader loss reorders
/// nothing and the next-ranked member wins deterministically.
pub fn closeness_score<F>(node: u64, members: &[u64], rtt_between: &F) -> Duration
where
    F: Fn(u64, u64) -> Option<Duration>,
{
    let mut total = Duration::ZERO;
    for &peer in members {
        if peer == node {
            continue;
        }
        total = total.saturating_add(rtt_between(node, peer).unwrap_or(UNKNOWN_EDGE_PENALTY));
    }
    total
}

/// The scope's current sensing leader (plan §4.1): the healthy
/// member with the best (lowest) closeness score, ties broken by
/// NodeId — computed by delegating to the RedEX election with the
/// shared score as the ranking key and a non-member observer.
/// `None` when no member is healthy in the caller's view (isolated
/// island — sensing degrades to Unknown, never blocks on
/// consensus).
///
/// `rtt_between` MUST be the shared proximity view (symmetric), not
/// self-anchored measurements — that is what makes every observer
/// compute the same winner.
pub fn sensing_leader<F, H>(members: &[u64], rtt_between: F, health_of: H) -> Option<u64>
where
    F: Fn(u64, u64) -> Option<Duration>,
    H: Fn(u64) -> bool,
{
    debug_assert!(
        !members.contains(&RENDEZVOUS_OBSERVER),
        "RENDEZVOUS_OBSERVER sentinel must not be a member",
    );
    match elect(
        members,
        RENDEZVOUS_OBSERVER,
        |node| Some(closeness_score(node, members, &rtt_between)),
        health_of,
    ) {
        ElectionOutcome::PeerWins(node) => Some(node),
        // SelfWins is unreachable (the observer is never a member);
        // NoEligibleReplica means the island has no healthy
        // rendezvous candidate.
        ElectionOutcome::SelfWins | ElectionOutcome::NoEligibleReplica => None,
    }
}

struct LeaderInterest {
    active: Vec<u64>,
    standby: Vec<u64>,
}

/// One consumer registration's outcome at the leader.
#[derive(Debug)]
pub struct LeaderRegistration {
    /// The coalescing identity this registration joined.
    pub interest: CapabilityInterestKey,
    /// The providers this interest actively senses (leader-resolved,
    /// bounded).
    pub branches: Vec<u64>,
    /// Whether this registration triggered candidate resolution
    /// (first consumer) or joined an existing row (coalesced).
    pub newly_resolved: bool,
    /// Cached-proof warm-starts for the registering downstream
    /// (always provisional).
    pub warm_starts: Vec<Delivery>,
}

/// The sensing-leader role (plan §4.1): coalesce equivalent
/// capability interests BEFORE provider selection, resolve bounded
/// candidates once per distinct interest, open provider-targeted
/// branches, and fan identical signed proofs back — composed from
/// the existing resolver + relay machinery.
pub struct SensingLeader {
    owner_root: AudienceScopeCommitment,
    policy: CandidatePolicy,
    /// Branch-level machinery: per-downstream tables, caches,
    /// schedules, and the hop-by-hop continuity rule.
    pub relay: SensingRelay,
    interests: HashMap<CapabilityInterestKey, LeaderInterest>,
}

impl SensingLeader {
    /// New leader role for one owner scope.
    pub fn new(
        owner_root: AudienceScopeCommitment,
        policy: CandidatePolicy,
        continuity_factor: u32,
        max_interests_per_peer: usize,
    ) -> Self {
        Self {
            owner_root,
            policy,
            relay: SensingRelay::new(continuity_factor, max_interests_per_peer),
            interests: HashMap::new(),
        }
    }

    /// Register one consumer's provider-free interest (scope
    /// validation happens upstream, §4.10). Equivalent interests
    /// coalesce on the digest: only the FIRST registration resolves
    /// candidates (from the leader's fold/proximity snapshot); every
    /// later one joins the existing row and branches. The
    /// registering downstream is added to every active branch and
    /// warm-started from the branch caches.
    #[allow(clippy::too_many_arguments)]
    pub fn register_capability_interest(
        &mut self,
        spec: &InterestSpec,
        downstream: DownstreamId,
        requested_sample_interval: Duration,
        soft_state_ttl: Duration,
        proven_root: AudienceScopeCommitment,
        snapshot: &[CandidateProvider],
        now: Instant,
    ) -> Result<LeaderRegistration, ResolutionRefusal> {
        let key = spec.key();
        let newly_resolved = !self.interests.contains_key(&key);
        if newly_resolved {
            let resolved = resolve_candidates(
                &spec.providers,
                spec.result_mode,
                snapshot,
                &self.owner_root,
                &self.policy,
            )?;
            self.interests.insert(
                key.clone(),
                LeaderInterest {
                    active: resolved.active,
                    standby: resolved.standby,
                },
            );
        }
        let branches: Vec<u64> = self
            .interests
            .get(&key)
            .map(|entry| entry.active.clone())
            .unwrap_or_default();
        let mut warm_starts = Vec::new();
        for provider in &branches {
            let branch = ProviderInterestKey::new(key.clone(), *provider);
            // A refused registration (cached floor, cap) simply
            // yields no warm-start; the outcome surfaces through the
            // table exactly as it does for any relay.
            let (_outcome, warm) = self.relay.register_downstream(
                &branch,
                downstream,
                requested_sample_interval,
                soft_state_ttl,
                proven_root,
                now,
            );
            if let Some(delivery) = warm {
                warm_starts.push(delivery);
            }
        }
        Ok(LeaderRegistration {
            interest: key,
            branches,
            newly_resolved,
            warm_starts,
        })
    }

    /// Promote the next standby candidate to active for one interest
    /// and register the requesting downstream on it — the expansion
    /// path for a consumer whose own budget rejected the active
    /// proof (plan §4.1: the leader never claims a universal
    /// end-to-end result; SI-0 test 30). Returns the promoted
    /// provider and any warm-start.
    pub fn expand_to_standby(
        &mut self,
        key: &CapabilityInterestKey,
        downstream: DownstreamId,
        requested_sample_interval: Duration,
        soft_state_ttl: Duration,
        proven_root: AudienceScopeCommitment,
        now: Instant,
    ) -> Option<(u64, Option<Delivery>)> {
        let entry = self.interests.get_mut(key)?;
        if entry.standby.is_empty() {
            return None;
        }
        let promoted = entry.standby.remove(0);
        entry.active.push(promoted);
        let branch = ProviderInterestKey::new(key.clone(), promoted);
        let (_, warm) = self.relay.register_downstream(
            &branch,
            downstream,
            requested_sample_interval,
            soft_state_ttl,
            proven_root,
            now,
        );
        Some((promoted, warm))
    }

    /// Ingest one provider attestation and fan it to the registered
    /// downstreams (delegates the whole store/pack/down-sample +
    /// hop-rule machinery).
    pub fn on_attestation(
        &mut self,
        now: Instant,
        attestation: &super::delivery::Attestation,
        upstream_bearing: bool,
    ) -> Vec<Delivery> {
        self.relay
            .on_attestation(now, attestation, upstream_bearing)
    }

    /// Drive schedules and continuity windows.
    pub fn poll(&mut self, now: Instant) -> Vec<Delivery> {
        self.relay.poll(now)
    }

    /// Sweep soft state: expired downstream rows drop; an interest
    /// whose branches ALL lost their last downstream is removed —
    /// emitters die when the last interest dies, and an abandoned
    /// leader drains to empty (plan §4.1 failover/suppression).
    pub fn sweep(&mut self, now: Instant) {
        let actions = self.relay.table.expire(now);
        for (branch, action) in actions {
            if action != UpstreamAction::Deregister {
                continue;
            }
            let interest = branch.interest.clone();
            let any_branch_live = self
                .interests
                .get(&interest)
                .map(|entry| {
                    entry.active.iter().any(|provider| {
                        let branch = ProviderInterestKey::new(interest.clone(), *provider);
                        self.relay.table.aggregate(&branch, now).is_some()
                    })
                })
                .unwrap_or(false);
            if !any_branch_live {
                self.interests.remove(&interest);
            }
        }
    }

    /// Distinct coalesced interests currently held.
    pub fn interest_count(&self) -> usize {
        self.interests.len()
    }

    /// Active branch providers for one interest.
    pub fn branches(&self, key: &CapabilityInterestKey) -> Vec<u64> {
        self.interests
            .get(key)
            .map(|entry| entry.active.clone())
            .unwrap_or_default()
    }

    /// Whether the leader holds no interests and no branch demand —
    /// the drained state an abandoned or superseded leader converges
    /// to.
    pub fn is_drained(&self) -> bool {
        self.interests.is_empty() && self.relay.table.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::super::continuity::{AttestedStatus, ProjectedReadiness};
    use super::super::controller::{project_aggregate, AggregateView, BranchView};
    use super::super::delivery::{Attestation, SensingConsumer};
    use super::super::identity::{
        CanonicalConstraints, CapabilityId, ConsumerLatencyBudget, DisclosureClass,
        ProviderObservationKey, ProviderSelector, ResultMode, WorkLatencyEnvelope,
    };
    use super::super::incarnation::Incarnation;
    use super::*;

    const K: u32 = 3;
    const TTL: Duration = Duration::from_secs(1);

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v)
    }

    fn root() -> AudienceScopeCommitment {
        AudienceScopeCommitment::from_bytes([0xAA; 32])
    }

    /// Symmetric shared RTT view over member pairs.
    fn shared_view(edges: &[(u64, u64, u64)]) -> impl Fn(u64, u64) -> Option<Duration> + '_ {
        move |a, b| {
            edges.iter().find_map(|(x, y, rtt)| {
                ((*x == a && *y == b) || (*x == b && *y == a)).then(|| ms(*rtt))
            })
        }
    }

    fn all_alive(_: u64) -> bool {
        true
    }

    fn spec() -> InterestSpec {
        InterestSpec {
            capability_id: CapabilityId::new("print.document"),
            constraints: CanonicalConstraints::from_entries([("color", "true"), ("media", "a4")])
                .unwrap(),
            work_latency: WorkLatencyEnvelope::start_within(Duration::from_secs(5)),
            providers: ProviderSelector::AnyAuthorized,
            result_mode: ResultMode::Any,
            disclosure_class: DisclosureClass::Owner,
            audience: root(),
        }
    }

    fn provider(id: u64, route_ms: u64) -> CandidateProvider {
        CandidateProvider {
            node_id: id,
            capability_generation: 1,
            authorized: true,
            reachable: true,
            route_estimate: ms(route_ms),
            tags: Vec::new(),
            groups: Vec::new(),
        }
    }

    fn proof(
        key: &CapabilityInterestKey,
        provider: u64,
        seq: u64,
        estimated_start: Option<Duration>,
    ) -> Attestation {
        Attestation::new(
            ProviderObservationKey::new(key.clone(), provider, 1),
            Incarnation::new(1),
            AttestedStatus::Ready,
            estimated_start,
            seq,
            ms(100),
        )
    }

    /// Scope members 1/2/3: node 1 sits between 2 and 3
    /// (scores: 1 → 100, 2 → 250, 3 → 250) — the proximity center.
    const EDGES: &[(u64, u64, u64)] = &[(1, 2, 50), (1, 3, 50), (2, 3, 200)];
    const MEMBERS: &[u64] = &[1, 2, 3];

    #[test]
    fn center_rendezvous_agrees_across_observers() {
        // SI-0 test 24: A and C hold DIFFERENT local provider
        // rankings — that must not matter, because the rendezvous is
        // computed from the shared membership + proximity view, not
        // from anyone's provider preference or self-anchored RTT.
        let view = shared_view(EDGES);
        let leader_seen_by_a = sensing_leader(MEMBERS, &view, all_alive);
        let leader_seen_by_c = sensing_leader(MEMBERS, &view, all_alive);
        assert_eq!(leader_seen_by_a, Some(1), "node 1 is the proximity center");
        assert_eq!(leader_seen_by_a, leader_seen_by_c);
        // The divergent local provider rankings that broke v4's
        // cross-node coalescing (review 5) play no part in the
        // computation above — both consumers address the interest to
        // the SAME leader, where test 25 shows it coalesces before
        // provider selection.
    }

    #[test]
    fn leader_coalesces_before_provider_selection() {
        // SI-0 test 25 — the restored v4 flagship: identical digests
        // from A and C → ONE interest row, ONE bounded candidate
        // branch, ONE signed stream, the identical proof to both.
        let t0 = Instant::now();
        let (a, c) = (DownstreamId::Peer(0xA), DownstreamId::Peer(0xC));
        let mut leader = SensingLeader::new(root(), CandidatePolicy::default(), K, 512);
        let snapshot = vec![provider(7, 10), provider(8, 40)];

        let reg_a = leader
            .register_capability_interest(&spec(), a, ms(100), TTL, root(), &snapshot, t0)
            .unwrap();
        let reg_c = leader
            .register_capability_interest(&spec(), c, ms(100), TTL, root(), &snapshot, t0)
            .unwrap();
        assert!(reg_a.newly_resolved);
        assert!(!reg_c.newly_resolved, "C joined the coalesced row");
        assert_eq!(reg_a.interest, reg_c.interest);
        assert_eq!(leader.interest_count(), 1, "one interest row");
        assert_eq!(reg_a.branches, vec![7], "one bounded candidate branch");
        assert_eq!(leader.relay.table.len(), 1, "one branch entry");

        // Provider 7 signs one attestation; the leader fans the
        // identical bytes to both consumers.
        let key = reg_a.interest.clone();
        let branch = ProviderInterestKey::new(key.clone(), 7);
        let mut consumer_a = SensingConsumer::new(K);
        let mut consumer_c = SensingConsumer::new(K);
        consumer_a.register_interest(&branch, ms(100), t0);
        consumer_c.register_interest(&branch, ms(100), t0);

        let out = leader.on_attestation(t0 + ms(100), &proof(&key, 7, 1, Some(ms(300))), true);
        let to_a = out.iter().find(|d| d.to == a).expect("A gets the proof");
        let to_c = out.iter().find(|d| d.to == c).expect("C gets the proof");
        assert_eq!(to_a.attestation.fingerprint, to_c.attestation.fingerprint);
        consumer_a.on_delivery(t0 + ms(100), to_a);
        consumer_c.on_delivery(t0 + ms(100), to_c);
        assert_eq!(consumer_a.projected(&branch), ProjectedReadiness::Ready);
        assert_eq!(consumer_c.projected(&branch), ProjectedReadiness::Ready);
    }

    #[test]
    fn leader_loss_fails_over_to_next_ranked_and_recovers() {
        // SI-0 test 26: R (node 1) dies; the SAME election over the
        // health-filtered view yields the next-ranked node — the
        // bully fallback. Recovery is soft-state re-registration at
        // R₂ with NO synchronous state transfer.
        let view = shared_view(EDGES);
        assert_eq!(sensing_leader(MEMBERS, &view, all_alive), Some(1));
        // Node 1 fails: 2 and 3 tie on score (250); NodeId breaks it.
        let leader2 = sensing_leader(MEMBERS, &view, |n| n != 1);
        assert_eq!(leader2, Some(2), "next-ranked healthy member wins");

        // The consumer re-registers its (still live) interest with
        // the new leader, which starts EMPTY and rebuilds from
        // registrations.
        let t0 = Instant::now();
        let a = DownstreamId::Peer(0xA);
        let mut new_leader = SensingLeader::new(root(), CandidatePolicy::default(), K, 512);
        assert!(new_leader.is_drained());
        let reg = new_leader
            .register_capability_interest(&spec(), a, ms(100), TTL, root(), &[provider(7, 10)], t0)
            .unwrap();
        assert!(
            reg.newly_resolved,
            "candidates re-resolve at the new leader"
        );

        let key = reg.interest.clone();
        let branch = ProviderInterestKey::new(key.clone(), 7);
        let mut consumer = SensingConsumer::new(K);
        consumer.register_interest(&branch, ms(100), t0);
        let out = new_leader.on_attestation(t0 + ms(100), &proof(&key, 7, 10, None), true);
        let to_a = out.iter().find(|d| d.to == a).expect("delivery resumes");
        consumer.on_delivery(t0 + ms(100), to_a);
        assert_eq!(consumer.projected(&branch), ProjectedReadiness::Ready);
    }

    #[test]
    fn center_change_drains_the_old_leader() {
        // SI-0 tests 27 + 29: the proximity view shifts so node 2
        // becomes center; consumers accept the new election result
        // and STOP refreshing node 1. The old leader's soft state
        // drains to empty — no duplicate permanence.
        let old_view = shared_view(EDGES);
        assert_eq!(sensing_leader(MEMBERS, &old_view, all_alive), Some(1));
        // Topology change: node 2 now sits between 1 and 3.
        let new_edges: &[(u64, u64, u64)] = &[(1, 2, 50), (1, 3, 200), (2, 3, 50)];
        let new_view = shared_view(new_edges);
        assert_eq!(
            sensing_leader(MEMBERS, &new_view, all_alive),
            Some(2),
            "the center moved with the topology",
        );

        // Old leader had live demand…
        let t0 = Instant::now();
        let a = DownstreamId::Peer(0xA);
        let mut old_leader = SensingLeader::new(root(), CandidatePolicy::default(), K, 512);
        old_leader
            .register_capability_interest(&spec(), a, ms(100), TTL, root(), &[provider(7, 10)], t0)
            .unwrap();
        assert_eq!(old_leader.interest_count(), 1);

        // …but the consumer, having accepted the new result, never
        // refreshes it again. Two missed ttl/2 refreshes later the
        // rows expire, the branch deregisters, and the interest —
        // and with it the upstream emitter demand — is gone.
        old_leader.sweep(t0 + TTL);
        assert!(
            old_leader.is_drained(),
            "an unrefreshed old leader must drain to empty",
        );
    }

    #[test]
    fn partition_islands_elect_their_own_leaders_and_converge() {
        // SI-0 test 28: during a partition each island elects its own
        // leader from its own health view; both may sense the same
        // provider (duplicate streams tolerated — advisory plane,
        // origin-signed proofs). After healing both islands compute
        // the same winner again and the loser drains.
        let view = shared_view(EDGES);
        // Island 1 sees only node 1 healthy; island 2 sees 2 and 3.
        let island1 = sensing_leader(MEMBERS, &view, |n| n == 1);
        let island2 = sensing_leader(MEMBERS, &view, |n| n != 1);
        assert_eq!(island1, Some(1));
        assert_eq!(island2, Some(2));
        assert_ne!(island1, island2, "one leader per island");

        // Both islands' leaders open a branch to the SAME provider.
        let t0 = Instant::now();
        let snapshot = vec![provider(7, 10)];
        let mut leader1 = SensingLeader::new(root(), CandidatePolicy::default(), K, 512);
        let mut leader2 = SensingLeader::new(root(), CandidatePolicy::default(), K, 512);
        let reg1 = leader1
            .register_capability_interest(
                &spec(),
                DownstreamId::Peer(0xA),
                ms(100),
                TTL,
                root(),
                &snapshot,
                t0,
            )
            .unwrap();
        let reg2 = leader2
            .register_capability_interest(
                &spec(),
                DownstreamId::Peer(0xC),
                ms(100),
                TTL,
                root(),
                &snapshot,
                t0,
            )
            .unwrap();
        assert_eq!(
            reg1.branches, reg2.branches,
            "duplicate streams to provider 7"
        );
        let key = reg1.interest.clone();
        // The provider serves both streams; each island's consumers
        // get origin-signed proofs. Neither leader claims global
        // authority — aggregates stay consumer-local.
        let out1 = leader1.on_attestation(t0 + ms(100), &proof(&key, 7, 1, None), true);
        let out2 = leader2.on_attestation(t0 + ms(100), &proof(&key, 7, 1, None), true);
        assert!(!out1.is_empty() && !out2.is_empty());

        // Healing: both islands see everyone; the deterministic
        // ranking gives ONE winner; island 2's consumers re-register
        // there and leader2 drains.
        let healed1 = sensing_leader(MEMBERS, &view, all_alive);
        let healed2 = sensing_leader(MEMBERS, &view, all_alive);
        assert_eq!(healed1, healed2);
        assert_eq!(healed1, Some(1));
        leader2.sweep(t0 + TTL);
        assert!(leader2.is_drained(), "the losing island's leader drains");
    }

    #[test]
    fn latency_disagreement_expands_to_the_standby() {
        // SI-0 test 30: the leader fans ONE provider proof
        // (estimated_start = 300 ms). A (route 150 ms, budget
        // 500 ms) accepts; C (route 250 ms) rejects it under ITS
        // budget and consumes the standby candidate. The leader
        // never claims a universal end-to-end result.
        let t0 = Instant::now();
        let (a, c) = (DownstreamId::Peer(0xA), DownstreamId::Peer(0xC));
        let mut leader = SensingLeader::new(root(), CandidatePolicy::default(), K, 512);
        // P7 ranks first at the leader; P8 is the warm standby.
        let snapshot = vec![provider(7, 10), provider(8, 20)];
        let reg = leader
            .register_capability_interest(&spec(), a, ms(100), TTL, root(), &snapshot, t0)
            .unwrap();
        leader
            .register_capability_interest(&spec(), c, ms(100), TTL, root(), &snapshot, t0)
            .unwrap();
        assert_eq!(reg.branches, vec![7]);

        let key = reg.interest.clone();
        let branch7 = ProviderInterestKey::new(key.clone(), 7);
        let mut consumer_a = SensingConsumer::new(K);
        let mut consumer_c = SensingConsumer::new(K);
        consumer_a.register_interest(&branch7, ms(100), t0);
        consumer_c.register_interest(&branch7, ms(100), t0);
        let out = leader.on_attestation(t0 + ms(100), &proof(&key, 7, 1, Some(ms(300))), true);
        for delivery in &out {
            match delivery.to {
                to if to == a => consumer_a.on_delivery(t0 + ms(100), delivery),
                to if to == c => consumer_c.on_delivery(t0 + ms(100), delivery),
                _ => {}
            }
        }

        let budget = ConsumerLatencyBudget {
            end_to_end_within: Some(ms(500)),
        };
        let selector = ProviderSelector::AnyAuthorized;
        let view_for = |consumer: &SensingConsumer, route_ms: u64| {
            let branches: Vec<BranchView> = consumer
                .branch_projections(&key)
                .into_iter()
                .map(|(provider, projection, estimated_start)| BranchView {
                    provider,
                    projection,
                    estimated_start,
                    route_estimate: ms(route_ms),
                })
                .collect();
            project_aggregate(&selector, ResultMode::Any, &budget, &branches, false)
        };
        // Same proof, opposite conclusions.
        assert_eq!(
            view_for(&consumer_a, 150),
            AggregateView::Scalar {
                status: ProjectedReadiness::Ready,
                supporting: vec![7],
            },
        );
        assert_eq!(
            view_for(&consumer_c, 250),
            AggregateView::Scalar {
                status: ProjectedReadiness::Unknown,
                supporting: vec![],
            },
        );

        // C requests expansion: the leader promotes the standby and
        // registers C on it; P8's proof makes C viable via P8.
        let (promoted, _warm) = leader
            .expand_to_standby(&key, c, ms(100), TTL, root(), t0 + ms(150))
            .expect("a standby candidate exists");
        assert_eq!(promoted, 8);
        assert_eq!(leader.branches(&key), vec![7, 8]);
        let branch8 = ProviderInterestKey::new(key.clone(), 8);
        consumer_c.register_interest(&branch8, ms(100), t0 + ms(150));
        let out = leader.on_attestation(t0 + ms(200), &proof(&key, 8, 1, Some(ms(200))), true);
        for delivery in out.iter().filter(|d| d.to == c) {
            consumer_c.on_delivery(t0 + ms(200), delivery);
        }
        // C's route to P8 is short enough: viable via the standby.
        let branches: Vec<BranchView> = consumer_c
            .branch_projections(&key)
            .into_iter()
            .map(|(provider, projection, estimated_start)| BranchView {
                provider,
                projection,
                estimated_start,
                route_estimate: if provider == 8 { ms(100) } else { ms(250) },
            })
            .collect();
        let aggregate = project_aggregate(&selector, ResultMode::Any, &budget, &branches, false);
        assert_eq!(
            aggregate,
            AggregateView::Scalar {
                status: ProjectedReadiness::Ready,
                supporting: vec![8],
            },
        );
    }

    #[test]
    fn rendezvous_delegates_to_the_redex_election() {
        // SI-0 test 31: outcome-equivalence with a direct elect()
        // call across score spreads, ties, health filtering, and the
        // empty case — the rendezvous is a parameterization of the
        // existing election, not a second algorithm.
        let cases: &[(&[u64], &[(u64, u64, u64)], fn(u64) -> bool)] = &[
            (MEMBERS, EDGES, all_alive),
            // Tie on score: symmetric triangle → NodeId tiebreak.
            (MEMBERS, &[(1, 2, 100), (1, 3, 100), (2, 3, 100)], all_alive),
            // Health filtering removes the center.
            (MEMBERS, EDGES, |n| n != 1),
            // Missing edges take the penalty rank.
            (&[1, 2, 3, 4], EDGES, all_alive),
            // Nobody healthy.
            (MEMBERS, EDGES, |_| false),
        ];
        for (members, edges, health) in cases {
            let view = shared_view(edges);
            let ours = sensing_leader(members, &view, health);
            let direct = match elect(
                members,
                RENDEZVOUS_OBSERVER,
                |node| Some(closeness_score(node, members, &&view)),
                health,
            ) {
                ElectionOutcome::PeerWins(node) => Some(node),
                _ => None,
            };
            assert_eq!(ours, direct, "members={members:?}");
        }
        // And the tie case is decided exactly like elect decides
        // ties: lowest NodeId.
        let tie_view = shared_view(&[(1, 2, 100), (1, 3, 100), (2, 3, 100)]);
        assert_eq!(sensing_leader(MEMBERS, &tie_view, all_alive), Some(1));
    }
}
