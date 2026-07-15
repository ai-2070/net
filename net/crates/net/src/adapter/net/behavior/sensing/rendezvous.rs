//! Capability-interest rendezvous: the RedEX-elected sensing leader
//! (plan §4.1, review 6).
//!
//! Provider-free capability interests need a destination; Net
//! already ships the primitive. This module REUSES
//! [`elect`] — the pure,
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
use std::fmt;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use super::super::super::redex::{elect, ElectionOutcome};
use super::controller::{
    resolve_candidates, CandidatePolicy, CandidateProvider, ResolutionRefusal,
};
use super::delivery::{Delivery, SensingRelay};
use super::evaluator::SensingCounters;
use super::frames::{FrameSpecError, SensingInterestFrame};
use super::identity::{
    AudienceScopeCommitment, CapabilityInterestKey, ConstraintError, InterestSpec,
    ProviderInterestKey,
};
use super::scope::{validate_subscriber_scope, ScopeError};
use super::table::{DownstreamId, RegisterOutcome, UpstreamAction};

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
/// (missing samples take `UNKNOWN_EDGE_PENALTY`). Lower = more
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
    /// The validated spec this interest coalesced under — cached so
    /// a refusal-survivor re-registration can be authored HERE (the
    /// mesh hop keeps no spec cache; the leader, as first-class
    /// subscriber, must — SI-4 re-review item 4).
    spec: InterestSpec,
    active: Vec<u64>,
    standby: Vec<u64>,
}

/// Why the leader refused one [`SensingInterestFrame`] at intake
/// (gate (r), plan §4.2/§4.10). Wraps the existing failure classes;
/// each keeps its own counter discipline (see
/// [`SensingLeader::register_from_frame`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FrameRejection {
    /// The frame is not a `CapabilityRegistration` — provider- or
    /// deregister-addressed frames have no business at the leader's
    /// registration intake.
    NotLeaderAddressed,
    /// The frame's `consumer` field does not name the authenticated
    /// routed origin (plan §4.10, review 7): an honest consumer's
    /// stack always binds its own id, so the mismatch is malformed
    /// or forged protocol input — protocol-invalid, exactly like a
    /// wire scope claim the session does not back.
    ConsumerMismatch {
        /// What the frame claimed.
        claimed: u64,
        /// What the routed session actually authenticated.
        authenticated: u64,
    },
    /// The inline constraint bytes failed parse or digest validation
    /// ([`super::evaluator::validate_interest_constraints`] — which
    /// already counted the rejection).
    Constraints(ConstraintError),
    /// The RE-DERIVED interest digest does not match the frame's
    /// claim (plan §4.2, review 7): the sender's bytes don't hash to
    /// the identity it asserted — protocol-invalid input. The
    /// claimed digest is never the coalescing identity, so nothing
    /// was registered.
    DigestMismatch,
    /// Scope validation from the session identity refused the
    /// registration (plan §4.10; counted by
    /// [`validate_subscriber_scope`]).
    Scope(ScopeError),
    /// The predicate was authentic and in-scope but candidate
    /// resolution refused to activate any stream (e.g. a broad
    /// `Each` selector, plan §4.7).
    Resolution(ResolutionRefusal),
}

impl FrameRejection {
    /// Whether this rejection incremented the protocol-invalid/
    /// security counter: forged or malformed protocol input, as
    /// opposed to an honest authorization or policy refusal.
    pub const fn is_security_relevant(self) -> bool {
        match self {
            Self::ConsumerMismatch { .. } | Self::DigestMismatch => true,
            Self::Constraints(error) => error.is_security_relevant(),
            Self::Scope(error) => error.is_security_relevant(),
            Self::NotLeaderAddressed | Self::Resolution(_) => false,
        }
    }
}

impl fmt::Display for FrameRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotLeaderAddressed => {
                f.write_str("frame is not a leader-addressed CapabilityRegistration")
            }
            Self::ConsumerMismatch {
                claimed,
                authenticated,
            } => write!(
                f,
                "frame consumer {claimed:#x} is not the authenticated routed origin \
                 {authenticated:#x}"
            ),
            Self::Constraints(error) => write!(f, "constraint intake refused: {error}"),
            Self::DigestMismatch => {
                f.write_str("re-derived interest digest does not match the frame's claim")
            }
            Self::Scope(error) => write!(f, "scope validation refused: {error}"),
            Self::Resolution(ResolutionRefusal::SelectorTooBroad { matched, cap }) => write!(
                f,
                "candidate resolution refused: selector matched {matched} providers (cap {cap})"
            ),
            Self::Resolution(ResolutionRefusal::AllBranchesRefused) => f.write_str(
                "every resolved branch refused the registration — interest not admitted",
            ),
            Self::Resolution(ResolutionRefusal::QuorumExceedsFanout { required, cap }) => write!(
                f,
                "candidate resolution refused: quorum of {required} exceeds the fanout cap {cap}"
            ),
        }
    }
}

impl std::error::Error for FrameRejection {}

/// Result of a leader-level refusal partition (SI-4 re-review
/// item 4): which REAL consumer rows fell below the provider floor
/// (forward the provider's exact signed refusal bytes to each), the
/// pending surviving transition for the mesh Leader row, and the
/// interest's cached spec for authoring the re-registration (`None`
/// once the interest itself is gone — nothing left to re-register).
#[derive(Debug)]
pub struct LeaderRefusalPartition {
    /// Leader-relay consumer rows with `D < M`, now removed — the
    /// refusal propagates to exactly these.
    pub refused: Vec<DownstreamId>,
    /// `Register { strictest }` = the surviving consumers' aggregate
    /// to re-register the mesh Leader row (and upstream demand) at;
    /// `Deregister` = every consumer was refused — the branch died
    /// at this hop; `None` = nothing changed against what was last
    /// advertised.
    pub upstream: UpstreamAction,
    /// The interest's cached spec (the leader is the spec-holding
    /// subscriber; the mesh hop caches none).
    pub spec: Option<InterestSpec>,
}

/// Outcome of one SI-6.1 fold-membership reconciliation pass — the
/// caller (the mesh hop hosting the leader) owns the mesh-table and
/// upstream consequences for both lists, and bumps the unified
/// scheduler-input generation when `changed`.
#[derive(Debug, Default)]
pub struct LeaderReconciliation {
    /// Branches torn down (provider no longer eligible): retire the
    /// mesh Leader row and the upstream demand.
    pub torn_down: Vec<ProviderInterestKey>,
    /// Branches newly opened (with the interest's cached spec):
    /// register the mesh Leader row and the upstream demand.
    pub added: Vec<(ProviderInterestKey, InterestSpec)>,
    /// Whether anything scheduler-relevant moved (including
    /// standby-list refreshes).
    pub changed: bool,
}

/// One consumer registration's outcome at the leader.
#[derive(Debug)]
pub struct LeaderRegistration {
    /// The coalescing identity this registration joined.
    pub interest: CapabilityInterestKey,
    /// The providers this interest actively senses (leader-resolved,
    /// bounded) — the SEMANTIC branch set, which other consumers may
    /// be keeping alive.
    pub branches: Vec<u64>,
    /// The branches on which THIS registration was actually admitted
    /// (`RegisterOutcome::Registered`) — the partial-admission
    /// distinction from the SI-3 sign-off residual: "at least one
    /// branch exists globally" is not "this registration was
    /// admitted on a branch". Never empty (an all-refused
    /// registration errs with
    /// [`ResolutionRefusal::AllBranchesRefused`] instead); demand
    /// derivation must use THIS set, never `branches`.
    pub admitted_branches: Vec<u64>,
    /// Whether this registration triggered candidate resolution
    /// (first consumer) or joined an existing row (coalesced).
    pub newly_resolved: bool,
    /// Cached-proof warm-starts for the registering downstream
    /// (always provisional).
    pub warm_starts: Vec<Delivery>,
}

/// SI-7 leader-load snapshot ([`SensingLeader::load`]). All three
/// grow with the scope's demand the leader concentrates; a
/// per-digest leader spread is a possible later refinement, not v1
/// (plan §7 leader-hotspot note).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SensingLeaderLoad {
    /// Distinct coalesced interests (one row per `(Y,C,L,selector,
    /// mode)`).
    pub interests: usize,
    /// Distinct active branches with at least one live downstream.
    pub branches: usize,
    /// Total live per-consumer downstream rows across all branches —
    /// the pre-coalescing demand the leader absorbs.
    pub downstream_rows: usize,
}

/// The sensing-leader role (plan §4.1): coalesce equivalent
/// capability interests BEFORE provider selection, resolve bounded
/// candidates once per distinct interest, open provider-targeted
/// branches, and fan identical signed proofs back — composed from
/// the existing resolver + relay machinery.
pub struct SensingLeader {
    owner_root: AudienceScopeCommitment,
    policy: CandidatePolicy,
    /// The node's soft-state lifetime bound (`sensing_interest_ttl`).
    /// A wire `soft_state_ttl` is clamped to this at intake so no
    /// remote value ever reaches `Instant + Duration` scheduling
    /// unbounded (an over-long ttl would otherwise panic the leader
    /// on overflow, or pin a row past the configured lifetime) —
    /// the same cap every local registration path already applies.
    max_soft_state_ttl: Duration,
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
        max_soft_state_ttl: Duration,
    ) -> Self {
        Self {
            owner_root,
            policy,
            max_soft_state_ttl,
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
                    spec: spec.clone(),
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
        let mut admitted_branches = Vec::new();
        for provider in &branches {
            let branch = ProviderInterestKey::new(key.clone(), *provider);
            let (outcome, warm) = self.relay.register_downstream(
                &branch,
                downstream,
                requested_sample_interval,
                soft_state_ttl,
                proven_root,
                now,
            );
            // SI-3 sign-off residual: record which branches admitted
            // THIS registration — a refused branch (cached floor,
            // per-downstream cap) yields no warm-start AND no
            // admitted entry, so the caller can never reconstruct
            // demand for it.
            if matches!(outcome, RegisterOutcome::Registered(_)) {
                admitted_branches.push(*provider);
            }
            if let Some(delivery) = warm {
                warm_starts.push(delivery);
            }
        }
        // A registration admitted on NO branch is refused — even
        // when other consumers keep the interest's branches live
        // (the sign-off residual's second manifestation: a ghost
        // registration would look successful while owning no
        // downstream row, visible the moment SI-4 delivers proofs).
        // Soft state: the consumer's refresh retries.
        if admitted_branches.is_empty() {
            // Standing SI-2 orphan-cap rule: the interest itself is
            // removed only when NO branch row is live globally —
            // the sweep drains interests only through branch-table
            // expiry, so a fully rowless interest would sit outside
            // expiry forever. Other consumers' live rows keep it.
            let any_branch_live = branches.iter().any(|provider| {
                let branch = ProviderInterestKey::new(key.clone(), *provider);
                self.relay.table.aggregate(&branch, now).is_some()
            });
            if !any_branch_live {
                self.interests.remove(&key);
            }
            return Err(ResolutionRefusal::AllBranchesRefused);
        }
        Ok(LeaderRegistration {
            interest: key,
            branches,
            admitted_branches,
            newly_resolved,
            warm_starts,
        })
    }

    /// Leader-side frame intake (gate (r), plan §4.2/§4.10 review
    /// 7): validate one routed [`SensingInterestFrame`] against the
    /// AUTHENTICATED transport identity, re-derive the coalescing
    /// identity, and only then delegate to
    /// [`Self::register_capability_interest`]. Order matters:
    ///
    /// 1. **Consumer binding** — `frame.consumer` must name
    ///    `authenticated_origin` (the routed end-to-end session
    ///    identity, NEVER the ingress relay). A mismatch is
    ///    protocol-invalid input: `protocol_invalid` +
    ///    `scope_refusals` both bump, mirroring the wire-scope-claim
    ///    rule.
    /// 2. **Predicate reconstruction** — the inline constraint bytes
    ///    parse and must hash to the carried `constraints_digest`
    ///    (via [`SensingInterestFrame::validated_spec`], whose
    ///    constraint intake owns the invalid-constraints/security
    ///    counting).
    /// 3. **Digest re-derivation** — the [`InterestSpec`] rebuilt
    ///    from the carried predicate + selector + mode + scope must
    ///    hash to the frame's `interest_digest`; a mismatch is
    ///    protocol-invalid. **The RE-DERIVED identity is what
    ///    coalesces** — the claim is only ever a cross-check. Steps
    ///    2–3 are [`SensingInterestFrame::validated_spec`] — the
    ///    SAME intake pipeline the provider leg runs before signing
    ///    (plan §4.2, review 7).
    /// 4. **Scope validation** — [`validate_subscriber_scope`]
    ///    against the session-proven root; the frame's
    ///    `audience_scope` is the wire claim AND the digest-bound
    ///    interest audience (v1 owner-root scope), cross-checked and
    ///    never load-bearing.
    /// 5. **Coalesce/resolve** — the existing registration path,
    ///    keyed under `DownstreamId::Peer(authenticated_origin)` with
    ///    the PROVEN root.
    ///
    /// `session_root` is derived from the authenticated routed
    /// session identity (v1:
    /// [`AudienceScopeCommitment::owner_root`] of the session's
    /// entity); `local_root` is this leader's own owner root (the
    /// one it was constructed with).
    #[allow(clippy::too_many_arguments)]
    pub fn register_from_frame(
        &mut self,
        frame: &SensingInterestFrame,
        authenticated_origin: u64,
        session_root: &AudienceScopeCommitment,
        local_root: &AudienceScopeCommitment,
        counters: &SensingCounters,
        snapshot: &[CandidateProvider],
        now: Instant,
    ) -> Result<LeaderRegistration, FrameRejection> {
        let SensingInterestFrame::CapabilityRegistration {
            requested_sample_interval,
            soft_state_ttl,
            audience_scope,
            consumer,
            ..
        } = frame
        else {
            return Err(FrameRejection::NotLeaderAddressed);
        };

        // (1) The consumer field is bound to the authenticated
        // routed origin — never trusted alone (§4.10, review 7).
        if *consumer != authenticated_origin {
            counters.scope_refusals.fetch_add(1, Ordering::Relaxed);
            counters.protocol_invalid.fetch_add(1, Ordering::Relaxed);
            return Err(FrameRejection::ConsumerMismatch {
                claimed: *consumer,
                authenticated: authenticated_origin,
            });
        }

        // (2)+(3) The shared intake pipeline (frames.rs — used by
        // BOTH legs): parse + digest-validate the inline
        // constraints, reconstruct the COMPLETE spec, RE-DERIVE the
        // interest digest, and cross-check the claim. The re-derived
        // key — spec.key(), inside register_capability_interest — is
        // the ONLY identity that ever coalesces.
        let spec = frame
            .validated_spec(counters)
            .map_err(|error| match error {
                FrameSpecError::Constraints(error) => FrameRejection::Constraints(error),
                FrameSpecError::InterestDigestMismatch => FrameRejection::DigestMismatch,
                // Unreachable: the CapabilityRegistration match above
                // already excluded the spec-free variants.
                FrameSpecError::NotARegistration | FrameSpecError::NotProviderAddressed => {
                    FrameRejection::NotLeaderAddressed
                }
            })?;

        // (4) Owner-root scope from the SESSION identity; the frame
        // field is the cross-checked wire claim.
        let proven_root = validate_subscriber_scope(
            session_root,
            audience_scope,
            local_root,
            &spec.audience,
            counters,
        )
        .map_err(FrameRejection::Scope)?;

        // (5) Clamp the wire `soft_state_ttl` to this node's
        // soft-state lifetime bound BEFORE it reaches any
        // `Instant + Duration` scheduling — a near-`u64::MAX` ttl is
        // not part of `interest_digest`, so it rides the frame
        // unvalidated, and an unclamped value would overflow-panic
        // the leader (or pin a row past the configured lifetime).
        // This mirrors the cap every local registration path applies
        // (`MeshNode::register_*`); the interest interval is already
        // range-checked at the 0x0C02 dispatch gate.
        let soft_state_ttl = (*soft_state_ttl).min(self.max_soft_state_ttl);

        // The validated registration joins the ordinary coalescing
        // path under the authenticated origin.
        self.register_capability_interest(
            &spec,
            DownstreamId::Peer(authenticated_origin),
            *requested_sample_interval,
            soft_state_ttl,
            proven_root,
            snapshot,
            now,
        )
        .map_err(FrameRejection::Resolution)
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
        // Promote the first standby provider that is not ALREADY
        // active. reconcile_with_snapshot keeps active and standby
        // disjoint, so this skip is defence-in-depth against a stale
        // overlap — promoting an already-active provider would push a
        // duplicate into `active` (double-counted load, a redundant
        // branch re-registration).
        let promoted = loop {
            let candidate = *entry.standby.first()?;
            entry.standby.remove(0);
            if !entry.active.contains(&candidate) {
                break candidate;
            }
        };
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

    /// SI-4 re-review item 4 (leader refusal partitioning): apply a
    /// provider floor M to the leader relay's REAL consumer rows.
    /// The mesh table holds ONE aggregate Leader row per branch;
    /// when the provider refuses it, the per-consumer partition must
    /// happen here, where the actual cadences live: consumers with
    /// D < M are refused (the caller forwards the provider's EXACT
    /// signed refusal bytes to each), D ≥ M survive, and the
    /// returned transition carries the surviving aggregate for the
    /// mesh Leader row's re-registration (with the interest's cached
    /// spec, so the caller can actually author it). A branch whose
    /// consumers were ALL refused dies at this hop: its private
    /// relay state reclaims and, if no other branch keeps it, the
    /// interest drops — the sweep-death semantics on the refusal
    /// path.
    pub fn on_refusal(
        &mut self,
        branch: &ProviderInterestKey,
        minimum_supported: Duration,
        now: Instant,
    ) -> LeaderRefusalPartition {
        let spec = self
            .interests
            .get(&branch.interest)
            .map(|entry| entry.spec.clone());
        let partition = self.relay.table.on_refusal(branch, minimum_supported, now);
        if partition.upstream == UpstreamAction::Deregister {
            self.relay.reclaim_branch(branch);
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
        } else {
            // Partitioned-out rows leave inert slots — GC on the
            // same event (the sweep's all-slot rule) — and the
            // surviving aggregate re-anchors the relay's continuity
            // window immediately (item 5).
            self.relay.gc_dead_slots(now);
            if let Some(aggregate) = self.relay.table.aggregate(branch, now) {
                self.relay.update_branch_interval(branch, aggregate);
            }
        }
        LeaderRefusalPartition {
            refused: partition.refused,
            upstream: partition.upstream,
            spec,
        }
    }

    /// SI-5 (§4.8 downstream loss): a consumer session died — drop
    /// every leader-relay row it held, event-driven, never waiting
    /// for the ttl sweep. A branch that lost its LAST row reclaims
    /// the relay's private state (a stale cache must not warm-start
    /// a later lifecycle) and drops its interest when no other
    /// branch keeps it; a merely-loosened branch re-anchors its
    /// continuity window. Returns the branch consequences so the
    /// caller can retire or re-scope its mesh Leader-row demand.
    pub fn remove_downstream(
        &mut self,
        downstream: DownstreamId,
        now: Instant,
    ) -> Vec<(ProviderInterestKey, UpstreamAction)> {
        let actions = self.relay.table.remove_downstream(downstream, now);
        for (branch, action) in &actions {
            match action {
                UpstreamAction::Register { strictest } => {
                    self.relay.update_branch_interval(branch, *strictest);
                }
                UpstreamAction::Deregister => {
                    self.relay.reclaim_branch(branch);
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
                UpstreamAction::None => {}
            }
        }
        self.relay.gc_dead_slots(now);
        actions
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

    /// SI-6.1 (§4.7 membership dynamics): reconcile every interest
    /// on `capability_id` against a FRESH candidate snapshot — a
    /// fold-membership change can alter the resolved set, the
    /// active branches, the scheduler's population, and the
    /// selected provider, so it joins the same reconciliation seam
    /// as the failure plane instead of waiting for TTL.
    ///
    /// Per interest: providers no longer ELIGIBLE (absent from the
    /// fresh resolution's active ∪ standby) are torn down — relay
    /// branch state reclaims exactly as on branch death, and the
    /// returned keys let the caller retire its mesh Leader rows and
    /// upstream demand. When the active set is left UNDER the fresh
    /// resolution's size, newly eligible providers fill it in
    /// resolution order; the returned additions let the caller open
    /// mesh demand for them. Standby lists refresh unconditionally.
    /// A fresh resolution that REFUSES (e.g. a now-too-broad
    /// selector) leaves the interest untouched — soft state drains
    /// it if consumers stop.
    ///
    /// SI-6.1 closure (review): consumer demand is snapshotted as a
    /// DEDUPLICATED union across ALL old live branches BEFORE any
    /// teardown, and every replacement branch inherits that
    /// surviving union — deriving rows from the first KEPT branch
    /// handed a full replacement (old \[A\] → fresh \[B\]) zero rows,
    /// and lost consumers present only on another old branch
    /// (partial refusals make branch populations non-identical). A
    /// branch is reported `added` only when it actually acquired
    /// live downstream demand; if NO branch holds live demand the
    /// interest DRAINS (the sweep's rule) instead of recording a
    /// ghost active set the mesh caller skips.
    pub fn reconcile_with_snapshot(
        &mut self,
        capability_id: &super::identity::CapabilityId,
        snapshot: &[CandidateProvider],
        now: Instant,
    ) -> LeaderReconciliation {
        let mut reconciliation = LeaderReconciliation::default();
        let keys: Vec<CapabilityInterestKey> = self
            .interests
            .iter()
            .filter(|(_, entry)| entry.spec.capability_id == *capability_id)
            .map(|(key, _)| key.clone())
            .collect();
        for key in keys {
            let Some(entry) = self.interests.get(&key) else {
                continue;
            };
            let Ok(resolved) = resolve_candidates(
                &entry.spec.providers,
                entry.spec.result_mode,
                snapshot,
                &self.owner_root,
                &self.policy,
            ) else {
                continue;
            };
            let eligible: Vec<u64> = resolved
                .active
                .iter()
                .chain(resolved.standby.iter())
                .copied()
                .collect();
            let spec = entry.spec.clone();
            let old_active = entry.active.clone();
            let old_standby = entry.standby.clone();
            // SI-6.1 closure: the surviving consumer demand,
            // deduplicated across ALL old live branches, captured
            // BEFORE any teardown drops their rows. A consumer
            // holding rows on several branches (with divergent D/ttl
            // via partial refusals) keeps its strictest interval and
            // longest ttl — the same direction the aggregate and the
            // sweep already resolve toward.
            let mut consumer_rows: Vec<(DownstreamId, Duration, Duration)> = Vec::new();
            for provider in &old_active {
                let branch = ProviderInterestKey::new(key.clone(), *provider);
                for downstream in self.relay.table.downstreams(&branch, now) {
                    let Some(row) = self.relay.table.downstream_entry(&branch, downstream) else {
                        continue;
                    };
                    match consumer_rows
                        .iter_mut()
                        .find(|(existing, _, _)| *existing == downstream)
                    {
                        Some((_, interval, ttl)) => {
                            *interval = (*interval).min(row.requested_sample_interval);
                            *ttl = (*ttl).max(row.soft_state_ttl);
                        }
                        None => consumer_rows.push((
                            downstream,
                            row.requested_sample_interval,
                            row.soft_state_ttl,
                        )),
                    }
                }
            }
            // Tear down branches whose provider fell out of the
            // eligible set.
            let mut kept: Vec<u64> = Vec::new();
            for provider in &old_active {
                if eligible.contains(provider) {
                    kept.push(*provider);
                    continue;
                }
                let branch = ProviderInterestKey::new(key.clone(), *provider);
                self.relay.table.remove_branch(&branch);
                self.relay.reclaim_branch(&branch);
                reconciliation.torn_down.push(branch);
            }
            // Fill an under-filled active set from the fresh
            // resolution, registering the surviving union on the
            // new branch at its captured D/ttl.
            for provider in &resolved.active {
                if kept.len() >= resolved.active.len() {
                    break;
                }
                if kept.contains(provider) {
                    continue;
                }
                let branch = ProviderInterestKey::new(key.clone(), *provider);
                for (downstream, interval, ttl) in &consumer_rows {
                    let _ = self.relay.register_downstream(
                        &branch,
                        *downstream,
                        *interval,
                        *ttl,
                        self.owner_root,
                        now,
                    );
                }
                // `added` means "acquired live downstream demand" —
                // a demandless replacement would be a ghost active
                // branch the mesh caller skips (`aggregate` None),
                // recorded as coverage sensing never re-establishes.
                if self.relay.table.aggregate(&branch, now).is_none() {
                    self.relay.table.remove_branch(&branch);
                    self.relay.reclaim_branch(&branch);
                    continue;
                }
                kept.push(*provider);
                reconciliation.added.push((branch, spec.clone()));
            }
            // 2026-07-15 review §6: a consumer that lived ONLY on a
            // torn-down branch (partial admission makes branch
            // populations non-identical) can end up with a row on NO
            // surviving branch — the fill loop adds the union only to
            // NEW branches, so when `kept` already fills
            // `resolved.active` no replacement is opened and the
            // orphan receives no proofs until its own ttl/2 refresh.
            // Re-register any such orphan onto a surviving branch NOW,
            // trying each until one admits (cap/floor may refuse some;
            // soft state retries the rest). Consumers still covered by
            // a surviving branch — e.g. those the fill loop just placed
            // on a replacement — are skipped, so this never diverts a
            // consumer off the branch it already reached.
            if !kept.is_empty() {
                for (downstream, interval, ttl) in &consumer_rows {
                    let still_covered = kept.iter().any(|provider| {
                        let branch = ProviderInterestKey::new(key.clone(), *provider);
                        self.relay
                            .table
                            .downstream_entry(&branch, *downstream)
                            .is_some()
                    });
                    if still_covered {
                        continue;
                    }
                    for provider in &kept {
                        let branch = ProviderInterestKey::new(key.clone(), *provider);
                        let (outcome, _) = self.relay.register_downstream(
                            &branch,
                            *downstream,
                            *interval,
                            *ttl,
                            self.owner_root,
                            now,
                        );
                        if matches!(outcome, RegisterOutcome::Registered(_)) {
                            break;
                        }
                    }
                }
            }
            // A provider retained in `active` (`kept`) must never also
            // sit in `standby`: `resolved.standby` is the fresh
            // resolution's standby, computed independently of the
            // incumbents we kept, so the two CAN overlap (e.g. a fold
            // shift re-ranks an active provider into standby while it
            // stays eligible). Filter `kept` out — otherwise
            // expand_to_standby would later promote an already-active
            // provider and duplicate the branch in `active`.
            let new_standby: Vec<u64> = resolved
                .standby
                .into_iter()
                .filter(|provider| !kept.contains(provider))
                .collect();
            reconciliation.changed |= kept != old_active || old_standby != new_standby;
            if kept.is_empty() {
                // No branch holds live demand: the interest DRAINS —
                // the sweep's own rule — never a ghost active set.
                reconciliation.changed = true;
                self.interests.remove(&key);
                continue;
            }
            if let Some(entry) = self.interests.get_mut(&key) {
                entry.active = kept;
                entry.standby = new_standby;
            }
        }
        reconciliation
    }

    /// Sweep soft state: expired downstream rows drop; an interest
    /// whose branches ALL lost their last downstream is removed —
    /// emitters die when the last interest dies, and an abandoned
    /// leader drains to empty (plan §4.1 failover/suppression).
    ///
    /// SI-4 re-review (leader relay reclamation): a branch whose
    /// LAST row died reclaims the relay's private cache + schedule
    /// state with it (a stale cache must never warm-start a later
    /// same-key lifecycle, and distinct-key churn must stay
    /// bounded); rows that died while their branch survives leave
    /// inert slots, GC'd on the same clock — the mesh sweep's
    /// all-slot rule.
    pub fn sweep(&mut self, now: Instant) {
        let actions = self.relay.table.expire(now);
        for (branch, action) in actions {
            // SI-4 re-review item 5: an expiry-loosened aggregate
            // re-anchors the surviving branch's continuity window
            // immediately, never at the next beat.
            if let UpstreamAction::Register { strictest } = action {
                self.relay.update_branch_interval(&branch, strictest);
            }
            if action != UpstreamAction::Deregister {
                continue;
            }
            self.relay.reclaim_branch(&branch);
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
        self.relay.gc_dead_slots(now);
    }

    /// Distinct coalesced interests currently held.
    pub fn interest_count(&self) -> usize {
        self.interests.len()
    }

    /// SI-7 (plan §7 "SI-7 must expose leader load"): a compact load
    /// snapshot for the leader hotspot — distinct coalesced
    /// interests, distinct active branches across them, and total
    /// live per-consumer downstream rows the relay carries. Demand
    /// concentrates here (bounded by scope size, per-downstream caps,
    /// and coalescing), so operators watch these three to spot a hot
    /// leader before it is a problem.
    pub fn load(&self, now: Instant) -> SensingLeaderLoad {
        let mut branches = 0usize;
        let mut downstream_rows = 0usize;
        for (interest, entry) in &self.interests {
            for provider in &entry.active {
                let branch = ProviderInterestKey::new(interest.clone(), *provider);
                let rows = self.relay.table.downstreams(&branch, now).len();
                if rows > 0 {
                    branches += 1;
                    downstream_rows += rows;
                }
            }
        }
        SensingLeaderLoad {
            interests: self.interests.len(),
            branches,
            downstream_rows,
        }
    }

    /// Distinct capability ids with live interests — the SI-6.1
    /// fold-reconciliation scan input.
    pub fn interest_capability_ids(&self) -> Vec<super::identity::CapabilityId> {
        let mut ids: Vec<super::identity::CapabilityId> = Vec::new();
        for entry in self.interests.values() {
            if !ids.contains(&entry.spec.capability_id) {
                ids.push(entry.spec.capability_id.clone());
            }
        }
        ids
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
    /// to. HONEST about the relay's private state (SI-4 re-review):
    /// retained caches or schedules mean not-drained, table or no
    /// table.
    pub fn is_drained(&self) -> bool {
        self.interests.is_empty() && self.relay.is_drained()
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

    /// SI-7: the leader-load snapshot counts distinct interests,
    /// distinct branches with live rows, and total downstream rows —
    /// the demand a hot leader concentrates. Two consumers coalescing
    /// on one interest is one interest, one branch, two rows.
    #[test]
    fn leader_load_snapshots_interest_branch_and_row_concentration() {
        let now = Instant::now();
        let mut leader = SensingLeader::new(root(), CandidatePolicy::default(), K, 4, TTL);
        assert_eq!(
            leader.load(now),
            SensingLeaderLoad {
                interests: 0,
                branches: 0,
                downstream_rows: 0,
            },
            "an idle leader has zero load",
        );

        let snapshot = [provider(0xB1, 5)];
        leader
            .register_capability_interest(
                &spec(),
                DownstreamId::Peer(0xC1),
                ms(100),
                TTL,
                root(),
                &snapshot,
                now,
            )
            .expect("first consumer admits");
        // A second consumer on the SAME interest coalesces: still one
        // interest, one branch, now two downstream rows.
        leader
            .register_capability_interest(
                &spec(),
                DownstreamId::Peer(0xC2),
                ms(100),
                TTL,
                root(),
                &snapshot,
                now,
            )
            .expect("second consumer coalesces");
        assert_eq!(
            leader.load(now),
            SensingLeaderLoad {
                interests: 1,
                branches: 1,
                downstream_rows: 2,
            },
        );
    }

    /// Standing SI-2 orphan-cap finding, closed in the SI-3 second
    /// closure round: an interest whose EVERY branch registration
    /// is refused must not be admitted — before the fix the
    /// `LeaderInterest` stayed behind with zero branch rows,
    /// invisible to the sweep (which drains interests only through
    /// branch-table expiry) forever.
    #[test]
    fn fully_cap_refused_interest_never_becomes_orphan_leader_state() {
        let now = Instant::now();
        // Per-downstream cap of ONE row: the second distinct
        // interest from the same consumer has nowhere to register.
        let mut leader = SensingLeader::new(root(), CandidatePolicy::default(), K, 1, TTL);
        let snapshot = [provider(0xB1, 5)];
        let consumer = DownstreamId::Peer(0xC1);

        let first = spec();
        leader
            .register_capability_interest(&first, consumer, ms(100), TTL, root(), &snapshot, now)
            .expect("first interest admits");
        assert_eq!(leader.interest_count(), 1);

        // Distinct digest, same downstream: the only branch
        // registration is OverCap — the interest must be refused
        // and leave NO leader state behind.
        let mut second = spec();
        second.constraints = CanonicalConstraints::from_entries([("media", "letter")]).unwrap();
        let refused = leader.register_capability_interest(
            &second,
            consumer,
            ms(100),
            TTL,
            root(),
            &snapshot,
            now,
        );
        assert!(
            matches!(refused, Err(ResolutionRefusal::AllBranchesRefused)),
            "expected AllBranchesRefused, got {refused:?}",
        );
        assert_eq!(
            leader.interest_count(),
            1,
            "no orphan interest outside branch-table expiry",
        );

        // A downstream with headroom admits the same spec normally.
        let other = DownstreamId::Peer(0xC2);
        leader
            .register_capability_interest(&second, other, ms(100), TTL, root(), &snapshot, now)
            .expect("fresh downstream admits");
        assert_eq!(leader.interest_count(), 2);
    }

    /// SI-3 sign-off residual: a PARTIAL admission must report
    /// exactly the branches this registration was admitted on —
    /// `branches` is the semantic set, `admitted_branches` is what
    /// the caller may derive demand from. Before the fix the caller
    /// saw all branches and reconstructed demand for the refused
    /// ones from the request interval.
    #[test]
    fn partial_admission_records_only_admitted_branches() {
        let now = Instant::now();
        // Fanout 2 → two active branches per interest; a per-
        // downstream cap of 3 leaves room for exactly ONE more row
        // after the first interest's two.
        let policy = CandidatePolicy {
            initial_fanout: 2,
            standby_count: 0,
            maximum_fanout: 3,
            each_mode_max_providers: 32,
        };
        let mut leader = SensingLeader::new(root(), policy, K, 3, TTL);
        let snapshot = [provider(0xB1, 5), provider(0xB2, 7)];
        let consumer = DownstreamId::Peer(0xC1);

        let first = spec();
        let full = leader
            .register_capability_interest(&first, consumer, ms(100), TTL, root(), &snapshot, now)
            .expect("both branches admit under the cap");
        assert_eq!(full.branches, vec![0xB1, 0xB2]);
        assert_eq!(full.admitted_branches, vec![0xB1, 0xB2]);

        // Second interest: the third row admits, the fourth is
        // OverCap — a PARTIAL admission.
        let mut second = spec();
        second.constraints = CanonicalConstraints::from_entries([("media", "letter")]).unwrap();
        let partial = leader
            .register_capability_interest(&second, consumer, ms(100), TTL, root(), &snapshot, now)
            .expect("a partially admitted registration still succeeds");
        assert_eq!(partial.branches, vec![0xB1, 0xB2], "semantic set intact");
        assert_eq!(
            partial.admitted_branches,
            vec![0xB1],
            "demand may derive only from the admitted branch",
        );
        // The refused branch really has no row behind it — the old
        // fallback would have re-manufactured its demand.
        let refused_branch = ProviderInterestKey::new(second.key(), 0xB2);
        assert_eq!(leader.relay.table.aggregate(&refused_branch, now), None);
    }

    /// SI-3 sign-off residual, second manifestation: a consumer
    /// refused on EVERY branch of an interest that OTHER consumers
    /// keep live must be refused — not ghost-registered ("at least
    /// one branch exists globally" is not "this registration was
    /// admitted on a branch"). The interest itself stays: the live
    /// consumer owns it.
    #[test]
    fn refused_joiner_on_live_interest_is_refused_not_ghosted() {
        let now = Instant::now();
        let mut leader = SensingLeader::new(root(), CandidatePolicy::default(), K, 1, TTL);
        let snapshot = [provider(0xB1, 5)];
        let live_consumer = DownstreamId::Peer(0xC1);
        let full_consumer = DownstreamId::Peer(0xC2);

        let shared = spec();
        leader
            .register_capability_interest(
                &shared,
                live_consumer,
                ms(100),
                TTL,
                root(),
                &snapshot,
                now,
            )
            .expect("the live consumer admits");

        // Fill the joiner's per-downstream cap elsewhere …
        let mut other = spec();
        other.constraints = CanonicalConstraints::from_entries([("media", "letter")]).unwrap();
        leader
            .register_capability_interest(
                &other,
                full_consumer,
                ms(100),
                TTL,
                root(),
                &snapshot,
                now,
            )
            .expect("the joiner's own interest admits");

        // … then join the LIVE shared interest: every branch refuses
        // (cap), so the registration must err even though the
        // interest's branch is globally live.
        let ghost = leader.register_capability_interest(
            &shared,
            full_consumer,
            ms(100),
            TTL,
            root(),
            &snapshot,
            now,
        );
        assert!(
            matches!(ghost, Err(ResolutionRefusal::AllBranchesRefused)),
            "expected AllBranchesRefused, got {ghost:?}",
        );
        // The live consumer's interest and row are untouched.
        assert_eq!(leader.interest_count(), 2);
        let branch = ProviderInterestKey::new(shared.key(), 0xB1);
        assert_eq!(
            leader.relay.table.aggregate(&branch, now),
            Some(ms(100)),
            "the live consumer's demand stands",
        );
    }

    /// SI-4 re-review (leader relay reclamation): the branch's last
    /// row death must reclaim the relay's PRIVATE state with the
    /// table — before the fix `is_drained` lied (empty table,
    /// retained cache + slot) and the dead lifecycle's cache
    /// warm-started the next same-key registration.
    #[test]
    fn sweep_reclaims_relay_state_and_re_registration_gets_no_stale_warm_start() {
        let t0 = Instant::now();
        let a = DownstreamId::Peer(0xA);
        let mut leader = SensingLeader::new(root(), CandidatePolicy::default(), K, 512, TTL);
        let snapshot = vec![provider(7, 10)];
        let reg = leader
            .register_capability_interest(&spec(), a, ms(100), TTL, root(), &snapshot, t0)
            .unwrap();
        let key = reg.interest.clone();
        // A proof lands: the relay holds a branch cache + slot.
        let out = leader.on_attestation(t0 + ms(100), &proof(&key, 7, 1, None), true);
        assert!(!out.is_empty());
        assert_eq!(leader.relay.retained_branches(), 1);
        assert_eq!(leader.relay.retained_slots(), 1);

        // The consumer stops refreshing; the row lapses.
        leader.sweep(t0 + TTL + ms(1));
        assert_eq!(leader.interest_count(), 0);
        assert_eq!(leader.relay.retained_branches(), 0, "cache reclaimed");
        assert_eq!(leader.relay.retained_slots(), 0, "slot reclaimed");
        assert!(
            leader.is_drained(),
            "drained means EMPTY, private state included",
        );

        // A same-key re-registration is a FRESH lifecycle: nothing
        // warm-starts it from the dead branch's cache.
        let reg = leader
            .register_capability_interest(
                &spec(),
                a,
                ms(100),
                TTL,
                root(),
                &snapshot,
                t0 + TTL + ms(2),
            )
            .unwrap();
        assert!(reg.newly_resolved, "candidates re-resolve from scratch");
        assert!(
            reg.warm_starts.is_empty(),
            "no warm-start from a previous lifecycle",
        );
    }

    /// SI-4 re-review item 5: an expiry-LOOSENED aggregate must push
    /// the relay's continuity deadline outward immediately — on a
    /// quiet stream the stale strict deadline would otherwise expire
    /// continuity the surviving loose consumer never demanded.
    #[test]
    fn expiry_loosened_aggregate_re_anchors_the_window_without_a_beat() {
        use super::super::continuity::Continuity;
        let t0 = Instant::now();
        let (a, c) = (DownstreamId::Peer(0xA), DownstreamId::Peer(0xC));
        let mut leader = SensingLeader::new(root(), CandidatePolicy::default(), K, 512, TTL);
        let snapshot = vec![provider(7, 10)];
        // Strict A carries a short ttl; loose C holds the branch.
        leader
            .register_capability_interest(&spec(), a, ms(100), ms(300), root(), &snapshot, t0)
            .unwrap();
        let reg = leader
            .register_capability_interest(&spec(), c, ms(400), TTL, root(), &snapshot, t0)
            .unwrap();
        let key = reg.interest.clone();
        let branch = ProviderInterestKey::new(key.clone(), 7);

        // One live beat establishes at aggregate 100: window =
        // 3 × max(promised 100, aggregate 100) = 300 → deadline
        // t1 + 300.
        let t1 = t0 + ms(100);
        leader.on_attestation(t1, &proof(&key, 7, 1, None), true);
        assert_eq!(
            leader.relay.upstream_continuity(&branch),
            Some(Continuity::Established),
        );

        // A's row lapses; the sweep loosens the aggregate to 400 →
        // window 1200 → the deadline shifts outward NOW.
        leader.sweep(t0 + ms(350));

        // Past the stale strict deadline, inside the loosened one:
        // continuity must hold.
        let _ = leader.poll(t1 + ms(400));
        assert_eq!(
            leader.relay.upstream_continuity(&branch),
            Some(Continuity::Established),
            "a loosened aggregate must move the deadline outward immediately",
        );
        // And the loosened window still expires honestly.
        let _ = leader.poll(t1 + ms(1250));
        assert_eq!(
            leader.relay.upstream_continuity(&branch),
            Some(Continuity::Expired),
        );
    }

    /// SI-4 re-review: distinct expired-branch churn must not grow
    /// retained relay state.
    #[test]
    fn distinct_branch_churn_leaves_no_retained_relay_state() {
        let t0 = Instant::now();
        let a = DownstreamId::Peer(0xA);
        let mut leader = SensingLeader::new(root(), CandidatePolicy::default(), K, 512, TTL);
        let snapshot = vec![provider(7, 10)];
        for i in 0..50u64 {
            let media = format!("size-{i}");
            let mut varied = spec();
            varied.constraints =
                CanonicalConstraints::from_entries([("media", media.as_str())]).unwrap();
            let reg = leader
                .register_capability_interest(&varied, a, ms(100), TTL, root(), &snapshot, t0)
                .unwrap();
            let out = leader.on_attestation(t0 + ms(1), &proof(&reg.interest, 7, 1, None), true);
            assert!(!out.is_empty());
        }
        assert_eq!(leader.relay.retained_branches(), 50);
        leader.sweep(t0 + TTL + ms(1));
        assert!(leader.is_drained(), "expired churn must not accumulate");
        assert_eq!(leader.relay.retained_branches(), 0);
        assert_eq!(leader.relay.retained_slots(), 0);
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
        let mut leader = SensingLeader::new(root(), CandidatePolicy::default(), K, 512, TTL);
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
        let mut new_leader = SensingLeader::new(root(), CandidatePolicy::default(), K, 512, TTL);
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
        let mut old_leader = SensingLeader::new(root(), CandidatePolicy::default(), K, 512, TTL);
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
        let mut leader1 = SensingLeader::new(root(), CandidatePolicy::default(), K, 512, TTL);
        let mut leader2 = SensingLeader::new(root(), CandidatePolicy::default(), K, 512, TTL);
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
    fn reconcile_keeps_active_and_standby_disjoint_so_expansion_never_duplicates() {
        // 2026-07-15 review §4: when a fold re-ranks an active provider
        // below a standby one while the incumbent stays eligible,
        // reconcile keeps the incumbent active BUT `resolved.standby`
        // (computed independently) names that same incumbent. Assigning
        // it unfiltered left the provider in BOTH sets, and a later
        // expand_to_standby then promoted it into `active` a second
        // time (active = [A, A]). The sets must stay disjoint.
        let t0 = Instant::now();
        let policy = CandidatePolicy {
            initial_fanout: 1,
            standby_count: 2,
            maximum_fanout: 1,
            each_mode_max_providers: 32,
        };
        let mut leader = SensingLeader::new(root(), policy, K, 3, TTL);
        let c = DownstreamId::Peer(1);
        let spec = spec();
        let key = spec.key();

        // A is closest → active=[A]; B waits in standby.
        leader
            .register_capability_interest(
                &spec,
                c,
                ms(100),
                TTL,
                root(),
                &[provider(0xA, 10), provider(0xB, 20)],
                t0,
            )
            .expect("A resolves");
        assert_eq!(leader.branches(&key), vec![0xA]);

        // The fold re-ranks: B is now closest, but A stays eligible,
        // so incumbency keeps A active and resolution offers A back as
        // standby — exactly the overlap the fix must filter out.
        leader.reconcile_with_snapshot(
            &spec.capability_id,
            &[provider(0xA, 30), provider(0xB, 5)],
            t0 + ms(10),
        );
        assert_eq!(
            leader.branches(&key),
            vec![0xA],
            "incumbent A stays active exactly once",
        );

        // A was filtered out of standby, so there is nothing to
        // promote — and, crucially, A is never duplicated into active.
        assert!(
            leader
                .expand_to_standby(&key, c, ms(100), TTL, root(), t0 + ms(20))
                .is_none(),
            "the re-ranked incumbent is not a promotable standby",
        );
        assert_eq!(
            leader.branches(&key),
            vec![0xA],
            "active still holds A exactly once — no standby re-promotion duplicated it",
        );
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
        let mut leader = SensingLeader::new(root(), CandidatePolicy::default(), K, 512, TTL);
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
        type ElectionCase<'a> = (&'a [u64], &'a [(u64, u64, u64)], fn(u64) -> bool);
        let cases: &[ElectionCase] = &[
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

    // ── gate (r): leader frame intake ───────────────────────────

    use super::super::evaluator::SensingCounters;
    use super::super::frames::SensingInterestFrame;
    use super::super::identity::{ConstraintError, Digest256};
    use super::super::scope::ScopeError;

    fn other_root() -> AudienceScopeCommitment {
        AudienceScopeCommitment::from_bytes([0xBB; 32])
    }

    fn frame_for(spec: &InterestSpec, consumer: u64, d: Duration) -> SensingInterestFrame {
        SensingInterestFrame::capability_registration(spec, d, TTL, consumer)
    }

    fn count(counter: &std::sync::atomic::AtomicU64) -> u64 {
        SensingCounters::get(counter)
    }

    #[test]
    fn frame_intake_coalesces_two_authenticated_origins_into_one_row() {
        // Gate (r) happy path: two REAL frames from two authenticated
        // origins, same predicate/selector/mode, different D — the
        // leader re-derives the digest from each and coalesces them
        // into ONE row on the RE-DERIVED identity.
        let t0 = Instant::now();
        let counters = SensingCounters::default();
        let mut leader = SensingLeader::new(root(), CandidatePolicy::default(), K, 512, TTL);
        let snapshot = vec![provider(7, 10), provider(8, 40)];

        let reg_a = leader
            .register_from_frame(
                &frame_for(&spec(), 0xA, ms(100)),
                0xA,
                &root(),
                &root(),
                &counters,
                &snapshot,
                t0,
            )
            .expect("A's frame is valid");
        let reg_c = leader
            .register_from_frame(
                &frame_for(&spec(), 0xC, ms(250)),
                0xC,
                &root(),
                &root(),
                &counters,
                &snapshot,
                t0,
            )
            .expect("C's frame is valid");

        assert!(reg_a.newly_resolved);
        assert!(!reg_c.newly_resolved, "C joined the coalesced row");
        assert_eq!(reg_a.interest, reg_c.interest);
        assert_eq!(
            reg_a.interest.interest_digest,
            spec().interest_digest(),
            "the registered identity is the re-derived digest",
        );
        assert_eq!(leader.interest_count(), 1, "one coalesced row");
        assert_eq!(reg_a.branches, vec![7], "one bounded branch");
        // Both AUTHENTICATED origins — and only they — hold table
        // rows on the branch.
        let branch = ProviderInterestKey::new(reg_a.interest.clone(), 7);
        let mut downstreams = leader.relay.table.downstreams(&branch, t0);
        downstreams.sort_by_key(|d| match d {
            DownstreamId::Local | DownstreamId::Leader => 0,
            DownstreamId::Peer(id) => *id,
        });
        assert_eq!(
            downstreams,
            vec![DownstreamId::Peer(0xA), DownstreamId::Peer(0xC)],
        );
        // A clean intake moves no security or refusal counters.
        assert_eq!(count(&counters.protocol_invalid), 0);
        assert_eq!(count(&counters.scope_refusals), 0);
        assert_eq!(count(&counters.invalid_constraints), 0);
    }

    #[test]
    fn frame_intake_clamps_an_over_long_soft_state_ttl() {
        // 2026-07-15 review §1 (leader-crash DoS): `soft_state_ttl` is
        // NOT bound by `interest_digest`, so a peer can ride any value
        // on the frame unvalidated. An unclamped near-`u64::MAX` ttl
        // reaches `now + soft_state_ttl` (InterestTable::register) and
        // overflow-panics the leader. Intake must clamp the wire value
        // to the node's soft-state ceiling — here `TTL` — exactly as
        // every local registration path does; the frame is admitted,
        // never rejected, and never panics.
        let t0 = Instant::now();
        let counters = SensingCounters::default();
        let mut leader = SensingLeader::new(root(), CandidatePolicy::default(), K, 512, TTL);
        let snapshot = vec![provider(7, 10)];

        let frame = SensingInterestFrame::capability_registration(
            &spec(),
            ms(100),
            Duration::from_secs(u64::MAX),
            0xA,
        );
        let reg = leader
            .register_from_frame(&frame, 0xA, &root(), &root(), &counters, &snapshot, t0)
            .expect("an over-long ttl is clamped, not rejected — and never panics");

        // The stored row reflects the CLAMPED lifetime, so
        // `expires_at = now + TTL` is representable and the row
        // expires on the configured schedule rather than never.
        let branch = ProviderInterestKey::new(reg.interest.clone(), 7);
        let row = leader
            .relay
            .table
            .downstream_entry(&branch, DownstreamId::Peer(0xA))
            .expect("the authenticated origin holds a row on the branch");
        assert_eq!(
            row.soft_state_ttl, TTL,
            "the wire ttl was clamped to the ceiling",
        );
        assert_eq!(row.expires_at, t0 + TTL);
    }

    #[test]
    fn frame_intake_rejects_a_consumer_field_mismatch() {
        // §4.10 review 7: the frame claims a consumer the routed
        // session did not authenticate — protocol-invalid, refused
        // before any predicate work.
        let counters = SensingCounters::default();
        let mut leader = SensingLeader::new(root(), CandidatePolicy::default(), K, 512, TTL);
        let rejection = leader
            .register_from_frame(
                &frame_for(&spec(), 0xBAD, ms(100)),
                0xA,
                &root(),
                &root(),
                &counters,
                &[provider(7, 10)],
                Instant::now(),
            )
            .unwrap_err();
        assert_eq!(
            rejection,
            FrameRejection::ConsumerMismatch {
                claimed: 0xBAD,
                authenticated: 0xA,
            },
        );
        assert!(rejection.is_security_relevant());
        assert_eq!(count(&counters.protocol_invalid), 1);
        assert_eq!(count(&counters.scope_refusals), 1);
        assert_eq!(leader.interest_count(), 0, "nothing registered");
    }

    #[test]
    fn frame_intake_rejects_a_forged_interest_digest_claim() {
        // §4.2 review 7: the leader re-derives; a claim the carried
        // fields don't hash to is protocol-invalid, and the claimed
        // digest never becomes an identity.
        let t0 = Instant::now();
        let counters = SensingCounters::default();
        let mut leader = SensingLeader::new(root(), CandidatePolicy::default(), K, 512, TTL);
        let snapshot = vec![provider(7, 10)];

        let forged_claim = Digest256::from_bytes([0xEE; 32]);
        let mut frame = frame_for(&spec(), 0xA, ms(100));
        let SensingInterestFrame::CapabilityRegistration {
            interest_digest, ..
        } = &mut frame
        else {
            unreachable!("helper builds the leader-addressed variant");
        };
        *interest_digest = forged_claim;

        let rejection = leader
            .register_from_frame(&frame, 0xA, &root(), &root(), &counters, &snapshot, t0)
            .unwrap_err();
        assert_eq!(rejection, FrameRejection::DigestMismatch);
        assert!(rejection.is_security_relevant());
        assert_eq!(count(&counters.protocol_invalid), 1);
        assert_eq!(leader.interest_count(), 0, "the claim registered nothing");

        // The claimed digest is IGNORED as identity: a subsequent
        // honest frame registers under the re-derived digest, which
        // is not the forged claim.
        let reg = leader
            .register_from_frame(
                &frame_for(&spec(), 0xA, ms(100)),
                0xA,
                &root(),
                &root(),
                &counters,
                &snapshot,
                t0,
            )
            .expect("honest frame registers");
        assert_eq!(reg.interest.interest_digest, spec().interest_digest());
        assert_ne!(reg.interest.interest_digest, forged_claim);
    }

    #[test]
    fn frame_intake_refuses_a_foreign_session_root() {
        // §4.10 v1 boundary: an HONEST foreign subscriber (its wire
        // claim matches its own proven root) is refused — an
        // authorization outcome, not a security event.
        let counters = SensingCounters::default();
        let mut leader = SensingLeader::new(root(), CandidatePolicy::default(), K, 512, TTL);
        let mut foreign_spec = spec();
        foreign_spec.audience = other_root();
        let rejection = leader
            .register_from_frame(
                &frame_for(&foreign_spec, 0xA, ms(100)),
                0xA,
                &other_root(), // the session proves the foreign root
                &root(),       // this leader's owner root
                &counters,
                &[provider(7, 10)],
                Instant::now(),
            )
            .unwrap_err();
        assert_eq!(
            rejection,
            FrameRejection::Scope(ScopeError::CrossRootRefused),
        );
        assert!(!rejection.is_security_relevant());
        assert_eq!(count(&counters.scope_refusals), 1);
        assert_eq!(count(&counters.protocol_invalid), 0);
        assert_eq!(leader.interest_count(), 0);
    }

    #[test]
    fn frame_intake_rejects_a_constraints_digest_mismatch() {
        // §4.2: inline bytes that don't hash to the carried
        // constraints digest are tampered/malformed protocol input —
        // both the invalid-constraints and security counters move.
        let counters = SensingCounters::default();
        let mut leader = SensingLeader::new(root(), CandidatePolicy::default(), K, 512, TTL);
        let mut frame = frame_for(&spec(), 0xA, ms(100));
        let SensingInterestFrame::CapabilityRegistration {
            constraints_digest, ..
        } = &mut frame
        else {
            unreachable!("helper builds the leader-addressed variant");
        };
        *constraints_digest = Digest256::from_bytes([0u8; 32]);

        let rejection = leader
            .register_from_frame(
                &frame,
                0xA,
                &root(),
                &root(),
                &counters,
                &[provider(7, 10)],
                Instant::now(),
            )
            .unwrap_err();
        assert_eq!(
            rejection,
            FrameRejection::Constraints(ConstraintError::DigestMismatch),
        );
        assert!(rejection.is_security_relevant());
        assert_eq!(count(&counters.invalid_constraints), 1);
        assert_eq!(count(&counters.protocol_invalid), 1);
        assert_eq!(leader.interest_count(), 0);
    }

    #[test]
    fn frame_intake_rejects_frames_that_are_not_leader_addressed() {
        // Provider-addressed and deregister frames have no business
        // at the registration intake.
        let counters = SensingCounters::default();
        let mut leader = SensingLeader::new(root(), CandidatePolicy::default(), K, 512, TTL);
        let not_leader_addressed = [
            SensingInterestFrame::provider_registration(&spec(), 7, ms(100), TTL),
            SensingInterestFrame::Deregister {
                interest_digest: spec().interest_digest(),
                target: None,
            },
        ];
        for frame in not_leader_addressed {
            let rejection = leader
                .register_from_frame(
                    &frame,
                    0xA,
                    &root(),
                    &root(),
                    &counters,
                    &[provider(7, 10)],
                    Instant::now(),
                )
                .unwrap_err();
            assert_eq!(rejection, FrameRejection::NotLeaderAddressed);
            assert!(!rejection.is_security_relevant());
        }
        assert_eq!(leader.interest_count(), 0);
    }

    /// SI-6.1 closure P1 (reviewer's exact sequence): when EVERY old
    /// active provider disappears but a replacement is available,
    /// the replacement branch must inherit the surviving consumer
    /// rows — before the fix `consumer_rows` derived from
    /// `kept.first()` AFTER teardown, so a full replacement handed
    /// the new branch zero rows: the leader recorded B as active
    /// while no aggregate, mesh Leader row, or upstream demand
    /// existed behind it.
    #[test]
    fn full_active_replacement_inherits_the_surviving_consumer_rows() {
        let now = Instant::now();
        let mut leader = SensingLeader::new(root(), CandidatePolicy::default(), K, 4, TTL);
        let consumer = DownstreamId::Peer(193);
        let shared = spec();

        leader
            .register_capability_interest(
                &shared,
                consumer,
                ms(100),
                TTL,
                root(),
                &[provider(0xA1, 5)],
                now,
            )
            .expect("registration admits");
        assert_eq!(leader.branches(&shared.key()), vec![0xA1]);

        // Fresh fold: A is gone entirely, B is the only eligible
        // provider — old active [A], fresh active [B], kept [].
        let reconciliation =
            leader.reconcile_with_snapshot(&shared.capability_id, &[provider(0xB1, 5)], now);

        let replacement = ProviderInterestKey::new(shared.key(), 0xB1);
        assert_eq!(
            leader.relay.table.downstreams(&replacement, now),
            vec![consumer],
            "the replacement branch must inherit the surviving consumer row",
        );
        assert_eq!(
            reconciliation
                .torn_down
                .iter()
                .map(|branch| branch.provider)
                .collect::<Vec<_>>(),
            vec![0xA1],
        );
        // `added` is backed by REAL demand the mesh caller can open
        // a Leader row + upstream registration from.
        assert_eq!(
            reconciliation
                .added
                .iter()
                .map(|(branch, _)| branch.provider)
                .collect::<Vec<_>>(),
            vec![0xB1],
        );
        assert_eq!(
            leader.relay.table.aggregate(&replacement, now),
            Some(ms(100)),
        );
        assert_eq!(leader.branches(&shared.key()), vec![0xB1]);
        assert!(reconciliation.changed);
    }

    /// SI-6.1 closure P1, second witness: branch populations are
    /// NON-IDENTICAL under partial refusals — a consumer whose row
    /// lives only on a torn-down branch must still reach the
    /// replacement. Old active [B1, B2] where only B1 carries C2
    /// (C2's B2 registration was cap-refused); the fold drops B1
    /// and offers B3 — the replacement must receive the surviving
    /// UNION {C1, C2}, not the first kept branch's rows {C1}.
    #[test]
    fn replacement_receives_the_surviving_union_across_old_branches() {
        let now = Instant::now();
        let policy = CandidatePolicy {
            initial_fanout: 2,
            standby_count: 0,
            maximum_fanout: 3,
            each_mode_max_providers: 32,
        };
        let mut leader = SensingLeader::new(root(), policy, K, 3, TTL);
        let snapshot = [provider(0xB1, 5), provider(0xB2, 7)];
        let c1 = DownstreamId::Peer(1);
        let c2 = DownstreamId::Peer(2);

        let shared = spec();
        leader
            .register_capability_interest(&shared, c1, ms(100), TTL, root(), &snapshot, now)
            .expect("C1 admits on both branches");
        // Fill two of C2's three row slots elsewhere …
        let mut filler = spec();
        filler.constraints = CanonicalConstraints::from_entries([("media", "letter")]).unwrap();
        leader
            .register_capability_interest(&filler, c2, ms(100), TTL, root(), &snapshot, now)
            .expect("C2's filler admits on both branches");
        // … so C2's join lands on B1 only (B2 cap-refused): the
        // shared interest's populations are B1{C1,C2}, B2{C1}.
        let join = leader
            .register_capability_interest(&shared, c2, ms(50), TTL, root(), &snapshot, now)
            .expect("a partially admitted join still succeeds");
        assert_eq!(join.admitted_branches, vec![0xB1]);

        // The fold drops B1 (the ONLY branch carrying C2) and
        // offers B3; B2 — the branch that lacks C2 — is kept.
        let reconciliation = leader.reconcile_with_snapshot(
            &shared.capability_id,
            &[provider(0xB2, 7), provider(0xB3, 9)],
            now,
        );

        let replacement = ProviderInterestKey::new(shared.key(), 0xB3);
        let mut downstreams = leader.relay.table.downstreams(&replacement, now);
        downstreams.sort_by_key(|downstream| match downstream {
            DownstreamId::Peer(node) => *node,
            _ => 0,
        });
        assert_eq!(
            downstreams,
            vec![c1, c2],
            "the replacement must receive the surviving union across \
             ALL old branches, not the first kept branch's rows",
        );
        // C2's stricter D survives onto the replacement aggregate.
        assert_eq!(
            leader.relay.table.aggregate(&replacement, now),
            Some(ms(50)),
        );
        assert_eq!(leader.branches(&shared.key()), vec![0xB2, 0xB3]);
        assert!(reconciliation.changed);
    }

    #[test]
    fn reconcile_re_registers_a_torn_down_only_consumer_onto_a_kept_branch() {
        // 2026-07-15 review §6: a consumer present ONLY on a torn-down
        // branch (partial admission makes branch populations
        // non-identical) must not go dark until its own ttl/2 refresh.
        // When the fold keeps a branch and opens no replacement, the
        // orphan is re-registered onto the surviving branch NOW.
        let now = Instant::now();
        let policy = CandidatePolicy {
            initial_fanout: 2,
            standby_count: 0,
            maximum_fanout: 2,
            each_mode_max_providers: 32,
        };
        // Per-downstream cap 3.
        let mut leader = SensingLeader::new(root(), policy, K, 3, TTL);
        let c1 = DownstreamId::Peer(1);
        let c2 = DownstreamId::Peer(2);

        // B is closer than A → active order [B, A].
        let snapshot = [provider(0xA, 10), provider(0xB, 5)];
        let shared = spec();

        leader
            .register_capability_interest(&shared, c1, ms(100), TTL, root(), &snapshot, now)
            .expect("C1 admits on both branches");
        // Consume two of C2's three slots on a DIFFERENT capability, so
        // reconcile (filtered by the shared capability) never touches
        // them while the per-downstream cap still counts them.
        let mut filler = spec();
        filler.capability_id = CapabilityId::new("scan.document");
        leader
            .register_capability_interest(&filler, c2, ms(100), TTL, root(), &snapshot, now)
            .expect("C2's filler admits on both branches");
        // C2's shared join lands on B only (active[0]); A is cap-refused.
        let join = leader
            .register_capability_interest(&shared, c2, ms(50), TTL, root(), &snapshot, now)
            .expect("a partially admitted join still succeeds");
        assert_eq!(join.admitted_branches, vec![0xB]);

        let branch_a = ProviderInterestKey::new(shared.key(), 0xA);
        assert!(
            leader.relay.table.downstream_entry(&branch_a, c2).is_none(),
            "precondition: C2 holds no row on branch A",
        );

        // The fold drops B (C2's ONLY shared branch) and keeps A; no
        // replacement is opened (kept already fills resolved.active).
        leader.reconcile_with_snapshot(&shared.capability_id, &[provider(0xA, 10)], now);

        assert_eq!(
            leader.branches(&shared.key()),
            vec![0xA],
            "A is kept and no replacement is opened",
        );
        assert!(
            leader.relay.table.downstream_entry(&branch_a, c2).is_some(),
            "the orphaned C2 was re-registered onto the surviving branch immediately",
        );
        // C1, already on A, is untouched (it stayed covered).
        assert!(leader.relay.table.downstream_entry(&branch_a, c1).is_some());
    }

    /// SI-6.1 closure P1, drain arm: a replacement that acquires NO
    /// live downstream demand (every surviving row already expired)
    /// must not be reported `added`, and an interest left with no
    /// demand-bearing branch DRAINS — the sweep's rule — instead of
    /// recording a ghost active set the mesh caller skips.
    #[test]
    fn replacement_without_surviving_demand_drains_instead_of_ghosting() {
        let now = Instant::now();
        let mut leader = SensingLeader::new(root(), CandidatePolicy::default(), K, 4, TTL);
        let shared = spec();
        leader
            .register_capability_interest(
                &shared,
                DownstreamId::Peer(0xC1),
                ms(100),
                TTL,
                root(),
                &[provider(0xA1, 5)],
                now,
            )
            .expect("registration admits");

        // Every consumer row is expired by reconciliation time.
        let later = now + TTL * 2;
        let reconciliation =
            leader.reconcile_with_snapshot(&shared.capability_id, &[provider(0xB1, 5)], later);

        assert!(
            reconciliation.added.is_empty(),
            "a replacement with no surviving demand is never reported added",
        );
        assert_eq!(
            leader.interest_count(),
            0,
            "the interest drains rather than retaining a ghost active branch",
        );
        assert!(reconciliation.changed);
        assert!(leader.is_drained());
    }
}
