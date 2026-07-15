//! The Layer-1 capability sensing controller (plan §3.5, §4.7) —
//! LOCAL by definition.
//!
//! This layer owns everything the wire must not: candidate
//! resolution (fold ∩ selector ∩ authority ∩ reachability,
//! proximity-ranked), bounded exploration per result mode, the
//! consumer's end-to-end budget check, and the result-mode
//! aggregate over provider proofs. Relays distribute *proofs*,
//! never verdicts — budgets make viability consumer-relative, so no
//! relay may resolve `Any` for its downstreams (plan §3.5, risk
//! list's fifth near-tripwire).
//!
//! Conservative projection (plan §3.5): Ready needs one established,
//! budget-passing proof; NotReady needs a COMPLETE bounded
//! authoritative population whose members all explicitly attest
//! NotReady — and `AnyAuthorized`/`Tags` populations are never
//! complete in v1 (proving absence is harder than proving
//! presence). Completeness is baked into the projection here, not
//! left to caller discipline.

use std::time::Duration;

use super::continuity::ProjectedReadiness;
use super::identity::{
    AudienceScopeCommitment, ConsumerLatencyBudget, GroupRef, ProviderSelector, ResultMode,
};

/// Bounded exploration policy (plan §4.7). Exact values are
/// configuration/application policy, never protocol semantics.
#[derive(Clone, Copy, Debug)]
pub struct CandidatePolicy {
    /// Branches an `Any` interest starts with.
    pub initial_fanout: usize,
    /// Warm standbys kept beyond the satisfying candidate.
    pub standby_count: usize,
    /// Hard per-interest exploration bound.
    pub maximum_fanout: usize,
    /// `Each` refuses selectors matching more than this BEFORE any
    /// stream activates (plan §5, `each_mode_max_providers`).
    pub each_mode_max_providers: usize,
}

impl Default for CandidatePolicy {
    fn default() -> Self {
        Self {
            initial_fanout: 1,
            standby_count: 1,
            maximum_fanout: 3,
            each_mode_max_providers: 32,
        }
    }
}

/// One tag assertion with its provenance (plan §4.10). Tags are not
/// equivalent self-assertions: `calibrated=true` means nothing
/// unless the asserting authority satisfies the selector's policy.
#[derive(Clone, Debug)]
pub struct TagAssertion {
    /// Tag key.
    pub key: String,
    /// Tag value.
    pub value: String,
    /// Who signed the assertion — v1 policy accepts owner-root
    /// authored assertions.
    pub asserted_by: AudienceScopeCommitment,
}

/// One provider as seen through the local fold + proximity planes —
/// the resolver's input snapshot. Structural capability/constraint
/// matching is the fold's job (SI-2 wires it); this snapshot is
/// already scoped to declarers of the capability.
#[derive(Clone, Debug)]
pub struct CandidateProvider {
    /// Provider node id.
    pub node_id: u64,
    /// Its current announce generation.
    pub capability_generation: u64,
    /// Whether the authority scope admits it (plan §4.10).
    pub authorized: bool,
    /// Whether the routing plane currently reaches it.
    pub reachable: bool,
    /// Route estimate from this consumer (proximity plane) — also
    /// the ranking key and the budget-check input.
    pub route_estimate: Duration,
    /// Tag assertions with provenance.
    pub tags: Vec<TagAssertion>,
    /// Owner-scoped groups this provider is a member of.
    pub groups: Vec<GroupRef>,
}

/// Why resolution refused to activate any stream.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResolutionRefusal {
    /// An `Each` selector matched more providers than the policy cap
    /// — refused BEFORE activation, with the count (plan §4.7).
    SelectorTooBroad {
        /// How many providers matched.
        matched: usize,
        /// The configured cap.
        cap: usize,
    },
    /// Every resolved branch refused this downstream's registration
    /// (per-downstream cap or cached floor) and no other consumer
    /// holds live branch rows — admitting the interest would create
    /// leader state the branch-table sweep can never expire (the
    /// standing SI-2 orphan-cap finding, closed in the SI-3 second
    /// closure round). Soft state: the consumer's refresh retries.
    AllBranchesRefused,
    /// A `Quorum(k)` requires k providers ready SIMULTANEOUSLY, but k
    /// exceeds the policy's `maximum_fanout`, so the active set can
    /// never hold k branches and the quorum is *permanently*
    /// unsatisfiable. Refused at resolution — the same discipline as
    /// `SelectorTooBroad` — rather than silently under-sensing and
    /// pinning the interest at Unknown/NotReady forever (plan §4.7).
    /// (A merely-transient shortfall — fewer than k *eligible*
    /// providers right now — is NOT refused: the population may grow,
    /// and it correctly resolves once k providers appear.)
    QuorumExceedsFanout {
        /// The quorum threshold k.
        required: usize,
        /// The configured `maximum_fanout`.
        cap: usize,
    },
}

/// The bounded outcome of one resolution pass.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ResolvedCandidates {
    /// Providers to actively sense now, proximity-ranked.
    pub active: Vec<u64>,
    /// Next-in-line providers (warm standby / expansion order).
    pub standby: Vec<u64>,
}

/// Whether a selector names a population whose completeness a
/// consumer can in principle establish (plan §3.5): explicit
/// node sets and owner groups are boundable; `AnyAuthorized` and
/// `Tags` are open-world in v1 and NEVER complete.
pub const fn population_is_boundable(selector: &ProviderSelector) -> bool {
    matches!(
        selector,
        ProviderSelector::Node(_) | ProviderSelector::Nodes(_) | ProviderSelector::Group(_)
    )
}

/// Candidate resolution (plan §4.7):
/// `fold ∩ selector ∩ authority ∩ reachability`, proximity-ranked,
/// bounded by the result mode and policy.
///
/// `Node`/`Nodes` selectors are operator-explicit: they use the
/// named ids directly (no fold consultation beyond ranking data),
/// which is exactly the v3 provider-targeted path. Tag matching
/// accepts an assertion only when its provenance passes the v1
/// owner-root policy (`asserted_by == owner_root`) — a provider
/// cannot enter a candidate set by self-labeling.
pub fn resolve_candidates(
    selector: &ProviderSelector,
    result_mode: ResultMode,
    snapshot: &[CandidateProvider],
    owner_root: &AudienceScopeCommitment,
    policy: &CandidatePolicy,
) -> Result<ResolvedCandidates, ResolutionRefusal> {
    // Node(X) is the operator naming a provider: no resolution, one
    // branch — final admission and scope checks still apply
    // downstream.
    if let ProviderSelector::Node(id) = selector {
        return Ok(ResolvedCandidates {
            active: vec![*id],
            standby: Vec::new(),
        });
    }

    let mut eligible: Vec<&CandidateProvider> = snapshot
        .iter()
        .filter(|candidate| candidate.authorized && candidate.reachable)
        .filter(|candidate| match selector {
            ProviderSelector::AnyAuthorized => true,
            ProviderSelector::Node(_) => unreachable!("handled above"),
            ProviderSelector::Nodes(ids) => ids.contains(&candidate.node_id),
            ProviderSelector::Group(group) => candidate.groups.contains(group),
            ProviderSelector::Tags(matches) => matches.iter().all(|wanted| {
                candidate.tags.iter().any(|assertion| {
                    assertion.key == wanted.key
                        && assertion.value == wanted.value
                        && assertion.asserted_by == *owner_root
                })
            }),
        })
        .collect();
    eligible.sort_by_key(|candidate| candidate.route_estimate);

    let matched = eligible.len();
    let active_bound = match result_mode {
        ResultMode::Any => policy.initial_fanout,
        // `TopK` is best-effort ("up to k"): capping the active set at
        // `maximum_fanout` yields fewer than k results, which is a
        // valid degradation — no refusal.
        ResultMode::TopK(k) => (k as usize)
            .max(policy.initial_fanout)
            .min(policy.maximum_fanout),
        // `Quorum` is a hard threshold: `project_aggregate` requires k
        // viable branches, so the active set must be able to hold k.
        // If k exceeds the static `maximum_fanout`, the quorum can
        // never be met — refuse the misconfiguration explicitly
        // instead of silently sensing too few branches (which pins the
        // interest at Unknown/NotReady with no signal).
        ResultMode::Quorum(k) => {
            let required = k as usize;
            if required > policy.maximum_fanout {
                return Err(ResolutionRefusal::QuorumExceedsFanout {
                    required,
                    cap: policy.maximum_fanout,
                });
            }
            required
                .max(policy.initial_fanout)
                .min(policy.maximum_fanout)
        }
        ResultMode::Each => {
            if matched > policy.each_mode_max_providers {
                return Err(ResolutionRefusal::SelectorTooBroad {
                    matched,
                    cap: policy.each_mode_max_providers,
                });
            }
            matched
        }
    };

    let active: Vec<u64> = eligible
        .iter()
        .take(active_bound)
        .map(|candidate| candidate.node_id)
        .collect();
    let standby: Vec<u64> = eligible
        .iter()
        .skip(active.len())
        .take(policy.standby_count)
        .map(|candidate| candidate.node_id)
        .collect();
    Ok(ResolvedCandidates { active, standby })
}

/// One branch's inputs to the local aggregate: the provider proof
/// (as projected through gate + continuity) plus this consumer's
/// route estimate toward that provider.
#[derive(Clone, Copy, Debug)]
pub struct BranchView {
    /// The provider behind the proof.
    pub provider: u64,
    /// Continuity-gated projection of the latest admitted proof.
    pub projection: ProjectedReadiness,
    /// The provider-signed start estimate.
    pub estimated_start: Option<Duration>,
    /// This consumer's current route estimate to the provider.
    pub route_estimate: Duration,
}

/// The local result-mode view (plan §3.5).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum AggregateView {
    /// `Any` / `TopK` / `Quorum`: one scalar status plus the
    /// supporting (viable) providers, ranked as given.
    Scalar {
        /// The projected capability-level status.
        status: ProjectedReadiness,
        /// Providers whose proofs support it (viable ones).
        supporting: Vec<u64>,
    },
    /// `Each`: the provider-indexed map — never flattened.
    PerProvider(Vec<(u64, ProjectedReadiness)>),
}

/// One branch's viability under a consumer budget — the SINGLE
/// source of the rule [`project_aggregate`] applies, exposed for
/// SI-6 so the scheduler-bridge candidate projection can never
/// drift from the aggregate's own economics.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BranchViability {
    /// Projected Ready AND within the budget; carries the
    /// consumer-local ranking cost (route + provider start
    /// estimate).
    Viable(Duration),
    /// Unknown — or Ready OUTSIDE the budget (a route change could
    /// still make it viable): never prune on it, never count it
    /// toward NotReady.
    Potential,
    /// Explicitly NotReady.
    NonViable,
}

/// Classify one branch view against this consumer's budget (SI-6;
/// the §3.5 viability rule).
pub fn classify_branch(branch: &BranchView, budget: &ConsumerLatencyBudget) -> BranchViability {
    match branch.projection {
        ProjectedReadiness::Ready
            if budget.admits(branch.route_estimate, branch.estimated_start) =>
        {
            BranchViability::Viable(
                branch
                    .route_estimate
                    .saturating_add(branch.estimated_start.unwrap_or(Duration::ZERO)),
            )
        }
        ProjectedReadiness::NotReady => BranchViability::NonViable,
        _ => BranchViability::Potential,
    }
}

/// The LOCAL result-mode aggregate (plan §3.5). `viable` means
/// projected Ready AND passing this consumer's budget over its
/// current route estimate — which is why the same proofs can
/// legitimately aggregate differently at two consumers, and why no
/// relay may compute this for anyone else.
///
/// NotReady requires a complete population: `search_complete` is
/// ANDed with [`population_is_boundable`], so open-world selectors
/// (`AnyAuthorized`, `Tags`) can never project NotReady in v1 —
/// completeness is baked in, not caller discipline. A Ready proof
/// that fails the budget counts toward neither `viable` nor
/// `NotReady` (a route change could make it viable), so it holds
/// the aggregate at Unknown rather than pushing it pessimistic.
pub fn project_aggregate(
    selector: &ProviderSelector,
    result_mode: ResultMode,
    budget: &ConsumerLatencyBudget,
    branches: &[BranchView],
    search_complete: bool,
) -> AggregateView {
    if result_mode == ResultMode::Each {
        // SI-4 review P1: `Each` obeys the SAME viability contract
        // as every other mode — a Ready proof that fails THIS
        // consumer's budget is locally non-viable and projects
        // Unknown, never Ready (a route change could still make it
        // viable, so Unknown, not NotReady).
        return AggregateView::PerProvider(
            branches
                .iter()
                .map(|branch| {
                    let projection = if branch.projection == ProjectedReadiness::Ready
                        && !matches!(classify_branch(branch, budget), BranchViability::Viable(_))
                    {
                        ProjectedReadiness::Unknown
                    } else {
                        branch.projection
                    };
                    (branch.provider, projection)
                })
                .collect(),
        );
    }

    let required = match result_mode {
        ResultMode::Quorum(k) => k as usize,
        _ => 1,
    };
    // SI-4 review P1: viable branches are ranked by the
    // consumer-LOCAL economics — the same quantity the budget
    // checks (route + provider start estimate) — with provider id
    // as a stable tie-break, so TopK is deterministic and locally
    // ranked instead of map-order. SI-6: the classification is the
    // shared [`classify_branch`] rule.
    let mut viable_ranked: Vec<(Duration, u64)> = branches
        .iter()
        .filter_map(|branch| match classify_branch(branch, budget) {
            BranchViability::Viable(cost) => Some((cost, branch.provider)),
            _ => None,
        })
        .collect();
    viable_ranked.sort();
    let viable: Vec<u64> = viable_ranked.into_iter().map(|(_, id)| id).collect();
    let explicit_not_ready = branches
        .iter()
        .filter(|branch| classify_branch(branch, budget) == BranchViability::NonViable)
        .count();
    let complete = search_complete && population_is_boundable(selector);

    // Everything not explicitly NotReady (Unknowns, over-budget
    // Readys) could still become viable; NotReady fires only when
    // even counting all of them as future successes cannot reach
    // the bar.
    let potential = branches.len().saturating_sub(explicit_not_ready);
    let status = if viable.len() >= required {
        ProjectedReadiness::Ready
    } else if complete && potential < required {
        ProjectedReadiness::NotReady
    } else {
        ProjectedReadiness::Unknown
    };
    let supporting = match result_mode {
        ResultMode::TopK(k) => viable.into_iter().take(k as usize).collect(),
        _ => viable,
    };
    AggregateView::Scalar { status, supporting }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::super::continuity::AttestedStatus;
    use super::super::delivery::{Attestation, SensingConsumer, SensingRelay};
    use super::super::identity::{
        CanonicalConstraints, CapabilityId, DisclosureClass, InterestRegistration, InterestSpec,
        ProviderInterestKey, ProviderObservationKey, TagMatch, WorkLatencyEnvelope,
    };
    use super::super::incarnation::Incarnation;
    use super::super::table::DownstreamId;
    use super::*;

    fn root() -> AudienceScopeCommitment {
        AudienceScopeCommitment::from_bytes([0xAA; 32])
    }

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v)
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

    fn view(provider: u64, projection: ProjectedReadiness) -> BranchView {
        BranchView {
            provider,
            projection,
            estimated_start: Some(ms(50)),
            route_estimate: ms(10),
        }
    }

    fn costed(
        provider: u64,
        projection: ProjectedReadiness,
        route_ms: u64,
        start_ms: Option<u64>,
    ) -> BranchView {
        BranchView {
            provider,
            projection,
            estimated_start: start_ms.map(ms),
            route_estimate: ms(route_ms),
        }
    }

    /// SI-4 review P1: TopK is deterministic and LOCALLY ranked —
    /// viable branches sort by this consumer's route + start
    /// economics (the budget's own quantity), provider id breaking
    /// ties — never by map iteration order.
    #[test]
    fn topk_ranks_viable_branches_by_local_economics() {
        use ProjectedReadiness::Ready;
        // Scrambled input: totals are 50 ms (p7), 15 ms (p5, tie),
        // 15 ms (p3, tie), 100 ms (p9).
        let branches = [
            costed(7, Ready, 20, Some(30)),
            costed(5, Ready, 15, None),
            costed(9, Ready, 60, Some(40)),
            costed(3, Ready, 10, Some(5)),
        ];
        let view = project_aggregate(
            &ProviderSelector::AnyAuthorized,
            ResultMode::TopK(2),
            &ConsumerLatencyBudget::default(),
            &branches,
            false,
        );
        match view {
            AggregateView::Scalar { status, supporting } => {
                assert_eq!(status, Ready);
                assert_eq!(
                    supporting,
                    vec![3, 5],
                    "the two cheapest by local economics, id-tie-broken",
                );
            }
            other => panic!("TopK aggregates to Scalar, got {other:?}"),
        }
        // Any mode reports the FULL ranked viable list.
        let view = project_aggregate(
            &ProviderSelector::AnyAuthorized,
            ResultMode::Any,
            &ConsumerLatencyBudget::default(),
            &branches,
            false,
        );
        match view {
            AggregateView::Scalar { supporting, .. } => {
                assert_eq!(supporting, vec![3, 5, 7, 9], "fully ranked");
            }
            other => panic!("Any aggregates to Scalar, got {other:?}"),
        }
    }

    /// SI-4 review P1: `Each` obeys the same viability contract —
    /// a Ready proof that fails THIS consumer's budget is locally
    /// Unknown (a route change could revive it), never surfaced as
    /// Ready.
    #[test]
    fn each_mode_budget_gates_ready_projections() {
        use ProjectedReadiness::{NotReady, Ready, Unknown};
        let budget = ConsumerLatencyBudget {
            end_to_end_within: Some(ms(50)),
        };
        let branches = [
            costed(1, Ready, 10, Some(10)),  // within budget
            costed(2, Ready, 100, Some(10)), // over budget
            costed(3, NotReady, 10, None),
        ];
        let view = project_aggregate(
            &ProviderSelector::nodes(vec![1, 2, 3]),
            ResultMode::Each,
            &budget,
            &branches,
            true,
        );
        assert_eq!(
            view,
            AggregateView::PerProvider(vec![(1, Ready), (2, Unknown), (3, NotReady)]),
            "over-budget Ready is locally non-viable, never Ready",
        );
    }

    #[test]
    fn any_mode_exploration_is_bounded_by_policy() {
        // "Any provider of Y?" must never become "probe every
        // provider of Y": ten matches, one active + one standby.
        let snapshot: Vec<CandidateProvider> =
            (1..=10).map(|id| provider(id, 100 - id * 5)).collect();
        let resolved = resolve_candidates(
            &ProviderSelector::AnyAuthorized,
            ResultMode::Any,
            &snapshot,
            &root(),
            &CandidatePolicy::default(),
        )
        .unwrap();
        // Proximity-ranked: id 10 has the cheapest route.
        assert_eq!(resolved.active, vec![10]);
        assert_eq!(resolved.standby, vec![9]);
    }

    #[test]
    fn node_selector_resolves_exactly_the_named_provider() {
        // SI-0 test 17: the operator named P1; a closer P2 in the
        // fold must not be substituted — this is the v3
        // provider-targeted path, no fold consultation.
        let snapshot = vec![provider(2, 1), provider(1, 500)];
        let resolved = resolve_candidates(
            &ProviderSelector::Node(1),
            ResultMode::Each,
            &snapshot,
            &root(),
            &CandidatePolicy::default(),
        )
        .unwrap();
        assert_eq!(resolved.active, vec![1]);
        assert!(resolved.standby.is_empty());
    }

    #[test]
    fn tags_require_authorized_assertions() {
        // SI-0 test 19: a structurally matching provider with a
        // SELF-asserted authority-implying tag is excluded; the
        // owner-root-asserted one enters.
        let calibrated = |by: AudienceScopeCommitment| TagAssertion {
            key: "calibrated".into(),
            value: "true".into(),
            asserted_by: by,
        };
        let mut legit = provider(1, 10);
        legit.tags = vec![calibrated(root())];
        let mut imposter = provider(2, 1);
        imposter.tags = vec![calibrated(AudienceScopeCommitment::from_bytes([0xEE; 32]))];

        let selector = ProviderSelector::tags(vec![TagMatch {
            key: "calibrated".into(),
            value: "true".into(),
        }]);
        let resolved = resolve_candidates(
            &selector,
            ResultMode::Any,
            &[legit, imposter],
            &root(),
            &CandidatePolicy::default(),
        )
        .unwrap();
        assert_eq!(
            resolved.active,
            vec![1],
            "self-asserted authority tags must not admit a candidate",
        );
    }

    #[test]
    fn broad_each_selector_is_refused_before_activation() {
        // SI-0 test 21: Each × a selector matching 40 providers ×
        // cap 32 → structured refusal carrying the count, and NO
        // active set.
        let snapshot: Vec<CandidateProvider> = (1..=40).map(|id| provider(id, id)).collect();
        let result = resolve_candidates(
            &ProviderSelector::AnyAuthorized,
            ResultMode::Each,
            &snapshot,
            &root(),
            &CandidatePolicy::default(),
        );
        assert_eq!(
            result,
            Err(ResolutionRefusal::SelectorTooBroad {
                matched: 40,
                cap: 32,
            }),
        );
    }

    #[test]
    fn quorum_exceeding_max_fanout_is_refused_not_silently_unsatisfiable() {
        // 2026-07-15 review §3: Quorum(k) is a hard threshold —
        // project_aggregate needs k viable branches. maximum_fanout
        // (default 3) capped the active set below k, so a Quorum(5)
        // could never reach Ready even with an ample ready population
        // and — unlike Each — produced NO refusal, pinning the
        // interest at Unknown/NotReady forever. It must be refused at
        // resolution, carrying the required count and the cap.
        let snapshot: Vec<CandidateProvider> = (1..=8).map(|id| provider(id, id)).collect();
        let policy = CandidatePolicy::default(); // maximum_fanout = 3
        let refusal = resolve_candidates(
            &ProviderSelector::AnyAuthorized,
            ResultMode::Quorum(5),
            &snapshot,
            &root(),
            &policy,
        );
        assert_eq!(
            refusal,
            Err(ResolutionRefusal::QuorumExceedsFanout {
                required: 5,
                cap: policy.maximum_fanout,
            }),
        );

        // A quorum WITHIN the fanout budget resolves and activates at
        // least k branches, so it can actually reach the threshold.
        let resolved = resolve_candidates(
            &ProviderSelector::AnyAuthorized,
            ResultMode::Quorum(3),
            &snapshot,
            &root(),
            &policy,
        )
        .expect("a quorum within the fanout budget resolves");
        assert!(
            resolved.active.len() >= 3,
            "the active set must be able to hold the full quorum",
        );
    }

    #[test]
    fn group_each_yields_the_unflattened_map() {
        // SI-0 test 18: three group members, three independent
        // observations — one NotReady never flattens the group.
        let branches = [
            view(1, ProjectedReadiness::Ready),
            view(2, ProjectedReadiness::NotReady),
            view(3, ProjectedReadiness::Unknown),
        ];
        let group = ProviderSelector::Group(GroupRef::from_bytes([7; 32]));
        let aggregate = project_aggregate(
            &group,
            ResultMode::Each,
            &ConsumerLatencyBudget::default(),
            &branches,
            true,
        );
        assert_eq!(
            aggregate,
            AggregateView::PerProvider(vec![
                (1, ProjectedReadiness::Ready),
                (2, ProjectedReadiness::NotReady),
                (3, ProjectedReadiness::Unknown),
            ]),
        );
    }

    #[test]
    fn quorum_flips_on_viable_count_and_respects_completeness() {
        // SI-0 test 20.
        let quorum = ResultMode::Quorum(2);
        let nodes = ProviderSelector::nodes(vec![1, 2, 3]);
        let budget = ConsumerLatencyBudget::default();

        // One viable of three, third Unknown → Unknown (it could
        // still become viable).
        let one_ready = [
            view(1, ProjectedReadiness::Ready),
            view(2, ProjectedReadiness::NotReady),
            view(3, ProjectedReadiness::Unknown),
        ];
        let aggregate = project_aggregate(&nodes, quorum, &budget, &one_ready, true);
        assert_eq!(
            aggregate,
            AggregateView::Scalar {
                status: ProjectedReadiness::Unknown,
                supporting: vec![1],
            },
        );

        // Two viable → Ready with both proofs.
        let two_ready = [
            view(1, ProjectedReadiness::Ready),
            view(2, ProjectedReadiness::Ready),
            view(3, ProjectedReadiness::NotReady),
        ];
        let aggregate = project_aggregate(&nodes, quorum, &budget, &two_ready, true);
        assert_eq!(
            aggregate,
            AggregateView::Scalar {
                status: ProjectedReadiness::Ready,
                supporting: vec![1, 2],
            },
        );

        // Complete bounded set, one viable, the REST explicitly
        // NotReady: even optimism cannot reach 2 → NotReady.
        let starved = [
            view(1, ProjectedReadiness::Ready),
            view(2, ProjectedReadiness::NotReady),
            view(3, ProjectedReadiness::NotReady),
        ];
        let aggregate = project_aggregate(&nodes, quorum, &budget, &starved, true);
        assert_eq!(
            aggregate,
            AggregateView::Scalar {
                status: ProjectedReadiness::NotReady,
                supporting: vec![1],
            },
        );

        // Same picture but the search is NOT complete → Unknown.
        let aggregate = project_aggregate(&nodes, quorum, &budget, &starved, false);
        assert_eq!(
            aggregate,
            AggregateView::Scalar {
                status: ProjectedReadiness::Unknown,
                supporting: vec![1],
            },
        );
    }

    #[test]
    fn open_world_selectors_never_project_not_ready() {
        // SI-0 item 22, the fourth permanent tripwire: proving
        // absence needs a bounded authoritative population.
        // AnyAuthorized/Tags are open-world in v1 — all-NotReady
        // with search_complete=true still projects Unknown; the same
        // picture under an explicit node set projects NotReady.
        let branches = [
            view(1, ProjectedReadiness::NotReady),
            view(2, ProjectedReadiness::NotReady),
        ];
        let budget = ConsumerLatencyBudget::default();
        for selector in [
            ProviderSelector::AnyAuthorized,
            ProviderSelector::tags(vec![TagMatch {
                key: "site".into(),
                value: "factory-7".into(),
            }]),
        ] {
            let aggregate = project_aggregate(&selector, ResultMode::Any, &budget, &branches, true);
            assert_eq!(
                aggregate,
                AggregateView::Scalar {
                    status: ProjectedReadiness::Unknown,
                    supporting: vec![],
                },
                "open-world population projected NotReady",
            );
        }
        let bounded = project_aggregate(
            &ProviderSelector::nodes(vec![1, 2]),
            ResultMode::Any,
            &budget,
            &branches,
            true,
        );
        assert_eq!(
            bounded,
            AggregateView::Scalar {
                status: ProjectedReadiness::NotReady,
                supporting: vec![],
            },
        );
    }

    #[test]
    fn flagship_coalescing_surfaces_and_the_honest_limitation() {
        // SI-0 test 16 (v4.1): the two honest coalescing surfaces,
        // and the divergent-resolution miss pinned as a conscious
        // cost.
        let any_color_a4_printer = || InterestSpec {
            capability_id: CapabilityId::new("print.document"),
            constraints: CanonicalConstraints::from_entries([
                ("color", "true"),
                ("duplex", "true"),
                ("media", "a4"),
            ])
            .unwrap(),
            work_latency: WorkLatencyEnvelope::start_within(Duration::from_secs(5)),
            providers: ProviderSelector::AnyAuthorized,
            result_mode: ResultMode::Any,
            disclosure_class: DisclosureClass::Owner,
            audience: root(),
        };

        // (a) LOCAL pre-selection coalescing: Hermes and a desktop
        // UI on ONE node, different D and budgets — one
        // CapabilityInterestKey, therefore one resolution and one
        // branch set for both.
        let hermes = InterestRegistration {
            spec: any_color_a4_printer(),
            requested_sample_interval: ms(100),
            soft_state_ttl: Duration::from_secs(30),
            consumer_budget: ConsumerLatencyBudget::default(),
        };
        let desktop_ui = InterestRegistration {
            spec: any_color_a4_printer(),
            requested_sample_interval: Duration::from_secs(1),
            soft_state_ttl: Duration::from_secs(300),
            consumer_budget: ConsumerLatencyBudget {
                end_to_end_within: Some(Duration::from_secs(2)),
            },
        };
        let interest = hermes.spec.key();
        assert_eq!(interest, desktop_ui.spec.key());

        // (b) Cross-node post-resolution coalescing: node A and
        // node C see similar fold/proximity facts and both resolve
        // printer P1 — their provider interests merge at the shared
        // relay: ONE table entry, one provider stream, and both
        // receive the SAME signed proof.
        let p1 = 1u64;
        let shared_snapshot = vec![provider(p1, 10), provider(2, 40)];
        for _node in ["A", "C"] {
            let resolved = resolve_candidates(
                &ProviderSelector::AnyAuthorized,
                ResultMode::Any,
                &shared_snapshot,
                &root(),
                &CandidatePolicy::default(),
            )
            .unwrap();
            assert_eq!(resolved.active, vec![p1]);
        }
        let t0 = Instant::now();
        let branch = ProviderInterestKey::new(interest.clone(), p1);
        let (a, c) = (DownstreamId::Peer(0xA), DownstreamId::Peer(0xC));
        let mut relay = SensingRelay::new(3, 512);
        let mut consumer_a = SensingConsumer::new(3);
        let mut consumer_c = SensingConsumer::new(3);
        consumer_a.register_interest(&branch, ms(100), t0);
        consumer_c.register_interest(&branch, ms(100), t0);
        relay.register_downstream(&branch, a, ms(100), Duration::from_secs(30), root(), t0);
        relay.register_downstream(&branch, c, ms(100), Duration::from_secs(30), root(), t0);
        assert_eq!(
            relay.table.len(),
            1,
            "equivalent demand must share one entry"
        );

        let proof = Attestation::new(
            ProviderObservationKey::new(interest.clone(), p1, 42),
            Incarnation::new(1),
            AttestedStatus::Ready,
            Some(ms(800)),
            1,
            ms(100),
        );
        let out = relay.on_attestation(t0 + ms(100), &proof, true);
        let to_a = out
            .iter()
            .find(|d| d.to == a)
            .expect("A receives the proof");
        let to_c = out
            .iter()
            .find(|d| d.to == c)
            .expect("C receives the proof");
        assert_eq!(
            to_a.attestation.fingerprint, to_c.attestation.fingerprint,
            "one provider stream serves both consumers with identical signed bytes",
        );
        consumer_a.on_delivery(t0 + ms(100), to_a);
        consumer_c.on_delivery(t0 + ms(100), to_c);
        assert_eq!(consumer_a.projected(&branch), ProjectedReadiness::Ready);
        assert_eq!(consumer_c.projected(&branch), ProjectedReadiness::Ready);

        // (c) Divergent resolution: A's proximity view ranks P1
        // first, C's ranks P2 — two branches at the relay, no
        // merge. THE stated v1 limitation (plan §4.1), pinned here
        // so it stays a conscious, measured cost rather than a
        // silent assumption.
        let snapshot_a = vec![provider(1, 10), provider(2, 40)];
        let snapshot_c = vec![provider(1, 40), provider(2, 10)];
        let resolve = |snapshot: &[CandidateProvider]| {
            resolve_candidates(
                &ProviderSelector::AnyAuthorized,
                ResultMode::Any,
                snapshot,
                &root(),
                &CandidatePolicy::default(),
            )
            .unwrap()
            .active
        };
        assert_eq!(resolve(&snapshot_a), vec![1]);
        assert_eq!(resolve(&snapshot_c), vec![2]);
        let mut divergent_relay = SensingRelay::new(3, 512);
        divergent_relay.register_downstream(
            &ProviderInterestKey::new(interest.clone(), 1),
            a,
            ms(100),
            Duration::from_secs(30),
            root(),
            t0,
        );
        divergent_relay.register_downstream(
            &ProviderInterestKey::new(interest, 2),
            c,
            ms(100),
            Duration::from_secs(30),
            root(),
            t0,
        );
        assert_eq!(
            divergent_relay.table.len(),
            2,
            "divergent provider resolution does not merge — the honest v1 cost",
        );
    }

    #[test]
    fn budget_makes_viability_consumer_relative() {
        // SI-0 test 23 (review 5): the SAME signed proof
        // (estimated_start = 300 ms) with a 500 ms end-to-end
        // budget — viable at 150 ms of route, not viable at 250 ms.
        // The aggregate is local by definition.
        let proof = |route_ms: u64| BranchView {
            provider: 1,
            projection: ProjectedReadiness::Ready,
            estimated_start: Some(ms(300)),
            route_estimate: ms(route_ms),
        };
        let budget = ConsumerLatencyBudget {
            end_to_end_within: Some(ms(500)),
        };
        let selector = ProviderSelector::AnyAuthorized;

        let near = project_aggregate(&selector, ResultMode::Any, &budget, &[proof(150)], false);
        assert_eq!(
            near,
            AggregateView::Scalar {
                status: ProjectedReadiness::Ready,
                supporting: vec![1],
            },
        );
        let far = project_aggregate(&selector, ResultMode::Any, &budget, &[proof(250)], false);
        assert_eq!(
            far,
            AggregateView::Scalar {
                status: ProjectedReadiness::Unknown,
                supporting: vec![],
            },
            "an over-budget Ready is not viable — and not NotReady either",
        );
    }
}
