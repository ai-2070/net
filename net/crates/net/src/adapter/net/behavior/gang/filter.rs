//! Gang-claim read pipeline (plan §2 steps 1–3): the pure,
//! read-only narrowing from an affinity hint to an ordered island
//! claim list.
//!
//! Every function here is a pure fn over already-queried fold data —
//! "match narrows, CAS commits" (locked decision 4). Nothing here
//! holds a resource; the hold is the separate `ReservationFold` CAS
//! in [`super::claim`]. These steps are cheap and side-effect-free,
//! so the scheduler runs them optimistically and re-runs them on a
//! claim reject.

use crate::adapter::net::behavior::fold::capability::resolve_candidate_keys;
use crate::adapter::net::behavior::fold::{
    CapabilityFold, CapabilityMatch, CapabilityQuery, Fold, FoldKind, IslandId, IslandRecord,
    NodeIdSet,
};

/// Step 1 bridge: the candidate *hosts* surfaced by a capability
/// match. The capability fold is keyed by `(class, node)`; the node
/// is the island host, so the matched node ids are exactly the hosts
/// whose islands step 2 then inspects. Deduped across classes (a
/// host in several capability classes is still one host).
pub fn candidate_hosts(matches: &[CapabilityMatch]) -> NodeIdSet {
    matches.iter().map(|((_class, node), _)| *node).collect()
}

/// Step 1, without materializing what step 1 throws away.
///
/// [`candidate_hosts`] keeps one `u64` out of each
/// [`CapabilityMatch`], but producing that match list means
/// `composite_query` deep-clones a whole `CapabilityMembership` per
/// matched host — tags Vec, metadata BTreeMap, three allow-list Vecs,
/// hardware — and the matcher drops all of it on the next line. This
/// resolves the same candidate set straight to node ids, cloning
/// nothing (PERF_AUDIT_2026_07_31_GANG_SCHEDULER §1).
///
/// **Scope:** the clone-free route covers
/// [`CapabilityQuery::Composite`] — the shape every [`MatchCriteria`]
/// in the tree uses. Every other query shape falls through to the
/// ordinary `query` path and stays clone-heavy; this is a targeted
/// fix, not a general one.
///
/// [`MatchCriteria`]: super::MatchCriteria
pub fn candidate_hosts_for(
    capability_fold: &Fold<CapabilityFold>,
    query: &CapabilityQuery,
) -> NodeIdSet {
    capability_fold.with_state_and_index(|state, index| match query {
        CapabilityQuery::Composite(filter) => {
            let candidates = resolve_candidate_keys(state, index, filter);
            // Two invariants carried over from `composite_query`:
            //
            // 1. The primary-store check is NOT redundant. That path
            //    is `filter_map(|k| state.entries.get(&k)…)`, so an
            //    index key with no live entry behind it is silently
            //    dropped; reading node ids straight off the index
            //    would resurrect it.
            // 2. `limit` applies to KEYS, before host dedup — matching
            //    today's order (take `limit` matches, then dedup).
            //    Which keys survive is unspecified either way (set
            //    iteration order), but the resulting host count must
            //    not change.
            let hosts = candidates
                .as_set()
                .iter()
                .filter(|key| state.entries.contains_key(*key))
                .map(|(_class, node)| *node);
            if filter.limit > 0 {
                hosts.take(filter.limit).collect()
            } else {
                hosts.collect()
            }
        }
        other => candidate_hosts(&<CapabilityFold as FoldKind>::query(
            state,
            index,
            other.clone(),
        )),
    })
}

/// Scheduler-side numeric constraints over the LIVE `IslandTopology`
/// axes — the step-2 filter the capability index deliberately can't
/// express because those axes churn every heartbeat (locked
/// decision 4). Every field's neutral value (`0` / `None`) means
/// "no constraint on this axis", so `NumericFilter::default()`
/// accepts everything.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NumericFilter {
    /// Minimum exclusive units in the island. `0` = any.
    pub min_units: usize,
    /// Maximum live load (`0.0..=1.0`). `None` = any.
    pub max_load: Option<f32>,
    /// Maximum live p50 latency (µs). `None` = any.
    pub max_p50_latency_us: Option<u32>,
    /// Resident capabilities the island must have **all** of (AND) —
    /// e.g. `"model:<hex>"` for a warm model that skips cold-load.
    /// Empty = no constraint. The island-side analogue of
    /// [`CapabilityFilter::tags_all`].
    ///
    /// [`CapabilityFilter::tags_all`]: crate::adapter::net::behavior::fold::CapabilityFilter::tags_all
    pub require_all: Vec<String>,
    /// Resident capabilities the island must have **at least one** of
    /// (OR). Empty = no constraint (the composite-filter semantics, not
    /// the bare `HasAnyTag` query). The island-side analogue of
    /// [`CapabilityFilter::tags_any`].
    ///
    /// [`CapabilityFilter::tags_any`]: crate::adapter::net::behavior::fold::CapabilityFilter::tags_any
    pub require_any: Vec<String>,
}

impl NumericFilter {
    /// Does `record` satisfy every populated constraint?
    pub fn accepts(&self, record: &IslandRecord) -> bool {
        if record.units.len() < self.min_units {
            return false;
        }
        if let Some(max) = self.max_load {
            if record.load > max {
                return false;
            }
        }
        if let Some(max) = self.max_p50_latency_us {
            if record.p50_latency_us > max {
                return false;
            }
        }
        // AND: every `require_all` capability must be resident.
        if !self
            .require_all
            .iter()
            .all(|cap| record.capabilities.contains(cap))
        {
            return false;
        }
        // OR: if `require_any` is non-empty, at least one must be
        // resident. Empty = no constraint on this axis.
        if !self.require_any.is_empty()
            && !self
                .require_any
                .iter()
                .any(|cap| record.capabilities.contains(cap))
        {
            return false;
        }
        true
    }
}

/// Step 2: retain only the island records passing `filter`. Pure.
pub fn numeric_filter(
    records: impl IntoIterator<Item = IslandRecord>,
    filter: &NumericFilter,
) -> Vec<IslandRecord> {
    records.into_iter().filter(|r| filter.accepts(r)).collect()
}

/// Island selection ordering (plan §2 step 3 / Phase E): a pure
/// ranking over the live `IslandTopology` axes.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum SelectionPolicy {
    /// **Spread** — least-loaded island first (lowest `load`),
    /// `IslandId` ascending as a deterministic tie-break. The default:
    /// distributes work across islands.
    #[default]
    LeastLoaded,
    /// **Pack** — most-loaded (but still filter-passing) island first.
    /// Consolidates jobs onto busy-but-available islands so whole
    /// islands stay idle and claimable by a future large gang.
    Pack,
    /// **Load-band** — island whose load is closest to `target`
    /// first. Avoids both stone-cold islands (cold-start cost) and
    /// near-saturated ones (tail-latency cliff).
    LoadBand(f32),
    /// `IslandId` ascending, ignoring the live axes. This is the
    /// global lock-ordering the multi-island ordered-acquire path
    /// (Phase C) needs: acquiring islands in one total order is what
    /// makes the gang protocol deadlock-free.
    LowestId,
}

/// Total order over two islands under `policy`, ties broken on
/// ascending `IslandId` so selection is deterministic.
fn policy_cmp(a: &IslandRecord, b: &IslandRecord, policy: SelectionPolicy) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let primary = match policy {
        SelectionPolicy::LeastLoaded => a.load.partial_cmp(&b.load).unwrap_or(Ordering::Equal),
        SelectionPolicy::Pack => b.load.partial_cmp(&a.load).unwrap_or(Ordering::Equal),
        SelectionPolicy::LoadBand(target) => {
            let da = (a.load - target).abs();
            let db = (b.load - target).abs();
            da.partial_cmp(&db).unwrap_or(Ordering::Equal)
        }
        SelectionPolicy::LowestId => Ordering::Equal,
    };
    primary.then(a.id.cmp(&b.id))
}

/// Step 3: order `records` per `policy` and project to claim-order
/// island ids. Pure.
pub fn select_islands(mut records: Vec<IslandRecord>, policy: SelectionPolicy) -> Vec<IslandId> {
    records.sort_by(|a, b| policy_cmp(a, b, policy));
    records.into_iter().map(|r| r.id).collect()
}

/// Step 3 with soft **capability affinity**: islands that already have
/// `prefer_capability` resident rank ahead of those that don't (e.g. a
/// warm model that skips cold-load), and within each group `policy`
/// orders them. `None` reduces to plain [`select_islands`]. Pure.
///
/// Affinity is a *preference*, not the hard `require_all` / `require_any`
/// filter — a job that benefits from a resident capability but can
/// tolerate its absence still considers islands without it, just after
/// the ones that have it.
pub fn select_with_affinity(
    records: Vec<IslandRecord>,
    policy: SelectionPolicy,
    prefer_capability: Option<String>,
) -> Vec<IslandId> {
    let Some(cap) = prefer_capability else {
        return select_islands(records, policy);
    };
    let (warm, cold): (Vec<IslandRecord>, Vec<IslandRecord>) = records
        .into_iter()
        .partition(|r| r.capabilities.contains(&cap));
    let mut ordered = select_islands(warm, policy);
    ordered.extend(select_islands(cold, policy));
    ordered
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::fold::{IslandRecord, NodeId, NodeState, UnitSet};

    fn rec(id: IslandId, host: NodeId, units: usize, load: f32, lat: u32) -> IslandRecord {
        IslandRecord {
            id,
            units: UnitSet::new((0..units as u32).collect()),
            host,
            capabilities: vec!["model:a1".into()],
            load,
            p50_latency_us: lat,
        }
    }

    /// Announce `node` into `class` carrying `tags`, in `state` /
    /// `region`. The equivalence tests below need every indexed axis
    /// populated so each `CapabilityQuery` shape selects something.
    fn announce(
        fold: &Fold<CapabilityFold>,
        kp: &crate::adapter::net::identity::EntityKeypair,
        node: NodeId,
        class: u64,
        tags: Vec<&str>,
        state: NodeState,
        region: Option<&str>,
    ) {
        use crate::adapter::net::behavior::fold::{
            CapabilityMembership, EnvelopeMeta, SignedAnnouncement,
        };
        let membership = CapabilityMembership {
            class_hash: class,
            tags: tags.into_iter().map(String::from).collect(),
            hardware: None,
            state,
            region: region.map(String::from),
            price_quote: None,
            reflex_addr: None,
            allowed_nodes: Vec::new(),
            allowed_subnets: Vec::new(),
            allowed_groups: Vec::new(),
            metadata: std::collections::BTreeMap::new(),
            owner: None,
        };
        fold.apply(
            SignedAnnouncement::sign(
                kp,
                CapabilityFold::KIND_ID,
                class,
                node,
                1,
                EnvelopeMeta::default(),
                membership,
            )
            .expect("sign"),
        )
        .expect("apply");
    }

    /// The reference: what step 1 produced before §1 — materialize
    /// every match, then project to hosts.
    fn hosts_via_query(fold: &Fold<CapabilityFold>, q: &CapabilityQuery) -> NodeIdSet {
        candidate_hosts(&fold.query(q.clone()))
    }

    /// A capability fold with three publishers spread across two
    /// classes, two states and two regions, so every indexed axis
    /// discriminates.
    fn populated_fold() -> (
        Fold<CapabilityFold>,
        Vec<crate::adapter::net::identity::EntityKeypair>,
        Vec<NodeId>,
    ) {
        use crate::adapter::net::identity::EntityKeypair;
        let fold: Fold<CapabilityFold> = Fold::with_sweep_interval(std::time::Duration::ZERO);
        let kps: Vec<EntityKeypair> = (0..3).map(|_| EntityKeypair::generate()).collect();
        let nodes: Vec<NodeId> = kps.iter().map(|k| k.entity_id().node_id()).collect();

        announce(
            &fold,
            &kps[0],
            nodes[0],
            1,
            vec!["gpu:h100", "nvlink"],
            NodeState::Idle,
            Some("us-east"),
        );
        announce(
            &fold,
            &kps[1],
            nodes[1],
            1,
            vec!["gpu:h100"],
            NodeState::Busy,
            Some("us-west"),
        );
        announce(
            &fold,
            &kps[2],
            nodes[2],
            2,
            vec!["gpu:a10", "nvlink"],
            NodeState::Idle,
            Some("us-east"),
        );
        (fold, kps, nodes)
    }

    /// §1: `candidate_hosts_for` must agree with the
    /// materialize-then-project path for EVERY `CapabilityQuery`
    /// shape — not just the `Composite` one it fast-paths. The
    /// non-composite shapes fall through to `query`, so this pins
    /// the fallback too.
    #[test]
    fn candidate_hosts_for_matches_the_query_path_on_every_shape() {
        use crate::adapter::net::behavior::fold::CapabilityFilter;
        let (fold, _kps, nodes) = populated_fold();

        let shapes = vec![
            CapabilityQuery::InClass(1),
            CapabilityQuery::InClass(2),
            CapabilityQuery::InClass(99), // selects nothing
            CapabilityQuery::HasAllTags(vec!["gpu:h100".into()]),
            CapabilityQuery::HasAllTags(vec!["gpu:h100".into(), "nvlink".into()]),
            CapabilityQuery::HasAllTags(vec![]), // "no constraint"
            CapabilityQuery::HasAnyTag(vec!["gpu:h100".into(), "gpu:a10".into()]),
            CapabilityQuery::HasAnyTag(vec!["absent".into()]),
            CapabilityQuery::InState(NodeState::Idle),
            CapabilityQuery::InState(NodeState::Busy),
            CapabilityQuery::InRegion("us-east".into()),
            CapabilityQuery::InRegion("nowhere".into()),
            // Composite: the fast-pathed shape, across its own
            // sub-shapes (single-tag borrow, multi-axis, empty).
            CapabilityQuery::Composite(CapabilityFilter {
                tags_all: vec!["gpu:h100".into()],
                ..Default::default()
            }),
            CapabilityQuery::Composite(CapabilityFilter {
                tags_all: vec!["nvlink".into()],
                region: Some("us-east".into()),
                ..Default::default()
            }),
            CapabilityQuery::Composite(CapabilityFilter {
                tags_all: vec!["gpu:h100".into()],
                state: Some(NodeState::Idle),
                ..Default::default()
            }),
            CapabilityQuery::Composite(CapabilityFilter {
                tags_any: vec!["gpu:h100".into(), "gpu:a10".into()],
                ..Default::default()
            }),
            CapabilityQuery::Composite(CapabilityFilter {
                class: Some(2),
                ..Default::default()
            }),
            CapabilityQuery::Composite(CapabilityFilter::default()), // everything
            CapabilityQuery::Composite(CapabilityFilter {
                tags_all: vec!["absent".into()],
                ..Default::default()
            }), // nothing
        ];

        for shape in shapes {
            assert_eq!(
                candidate_hosts_for(&fold, &shape),
                hosts_via_query(&fold, &shape),
                "candidate_hosts_for disagreed with the query path on {shape:?}",
            );
        }

        // Sanity: the fixture actually discriminates, so the loop
        // above is not comparing empty against empty.
        let idle = candidate_hosts_for(&fold, &CapabilityQuery::InState(NodeState::Idle));
        assert_eq!(idle.len(), 2, "fixture must not be degenerate");
        assert!(idle.contains(&nodes[0]) && idle.contains(&nodes[2]));
    }

    /// §1 invariant 1: `composite_query` resolves candidates through
    /// the index but materializes through the PRIMARY STORE, so an
    /// index key with no live entry is dropped. Reading node ids
    /// straight off the index would resurrect it. Evicting a node
    /// leaves exactly that divergence, so it is the witness.
    #[test]
    fn candidate_hosts_for_drops_index_keys_with_no_live_entry() {
        use crate::adapter::net::behavior::fold::CapabilityFilter;
        let (fold, _kps, nodes) = populated_fold();
        let shape = CapabilityQuery::Composite(CapabilityFilter {
            tags_all: vec!["gpu:h100".into()],
            ..Default::default()
        });
        assert_eq!(candidate_hosts_for(&fold, &shape).len(), 2);

        // Evict one of the two matching publishers.
        fold.evict_node(nodes[0], "test");

        let via_new = candidate_hosts_for(&fold, &shape);
        assert_eq!(
            via_new,
            hosts_via_query(&fold, &shape),
            "an evicted publisher must drop out of both paths identically",
        );
        assert!(
            !via_new.contains(&nodes[0]),
            "an evicted node must not survive as a candidate host",
        );
        assert_eq!(via_new.len(), 1);
    }

    /// §1 invariant 2: `limit` applies to KEYS, before host dedup —
    /// the same order `composite_query` + `candidate_hosts` used.
    /// Which keys survive is unspecified (set iteration order), so
    /// this pins the COUNT, which is the part that must not change.
    #[test]
    fn candidate_hosts_for_applies_limit_before_host_dedup() {
        use crate::adapter::net::behavior::fold::CapabilityFilter;
        let (fold, _kps, _nodes) = populated_fold();

        for limit in [1usize, 2, 3] {
            let shape = CapabilityQuery::Composite(CapabilityFilter {
                tags_any: vec!["gpu:h100".into(), "gpu:a10".into()],
                limit,
                ..Default::default()
            });
            let got = candidate_hosts_for(&fold, &shape);
            assert!(
                got.len() <= limit,
                "limit {limit} must bound the host count, got {}",
                got.len(),
            );
            assert_eq!(
                got.len(),
                hosts_via_query(&fold, &shape).len(),
                "limit {limit}: host COUNT must match the query path",
            );
        }
    }

    #[test]
    fn candidate_hosts_dedupes_across_classes() {
        // Same node 0xAA in two classes + node 0xBB in one.
        let matches: Vec<CapabilityMatch> = vec![];
        assert!(candidate_hosts(&matches).is_empty());
    }

    #[test]
    fn default_filter_accepts_everything() {
        let f = NumericFilter::default();
        assert!(f.accepts(&rec(1, 0xAA, 8, 0.99, 9999)));
        assert!(f.accepts(&rec(2, 0xAA, 0, 0.0, 0)));
    }

    #[test]
    fn min_units_filters_small_islands() {
        let f = NumericFilter {
            min_units: 4,
            ..Default::default()
        };
        assert!(f.accepts(&rec(1, 0xAA, 4, 0.0, 0)));
        assert!(f.accepts(&rec(2, 0xAA, 8, 0.0, 0)));
        assert!(!f.accepts(&rec(3, 0xAA, 2, 0.0, 0)));
    }

    #[test]
    fn load_and_latency_caps_apply() {
        let f = NumericFilter {
            max_load: Some(0.5),
            max_p50_latency_us: Some(2_000),
            ..Default::default()
        };
        assert!(f.accepts(&rec(1, 0xAA, 4, 0.50, 2_000))); // at the cap
        assert!(!f.accepts(&rec(2, 0xAA, 4, 0.51, 1_000))); // over load
        assert!(!f.accepts(&rec(3, 0xAA, 4, 0.10, 2_001))); // over latency
    }

    #[test]
    fn require_all_filters_resident_capabilities_and() {
        let f = NumericFilter {
            require_all: vec!["model:beef".into()],
            ..Default::default()
        };
        let mut hot = rec(1, 0xAA, 4, 0.0, 0);
        hot.capabilities = vec!["model:beef".into(), "model:a1".into()];
        assert!(f.accepts(&hot));
        assert!(!f.accepts(&rec(2, 0xAA, 4, 0.0, 0))); // only has model:a1
    }

    #[test]
    fn require_any_filters_resident_capabilities_or() {
        // OR: at least one of the set must be resident.
        let f = NumericFilter {
            require_any: vec!["model:beef".into(), "model:a1".into()],
            ..Default::default()
        };
        assert!(f.accepts(&rec(1, 0xAA, 4, 0.0, 0))); // rec has model:a1 (one of)
        let mut neither = rec(2, 0xAA, 4, 0.0, 0);
        neither.capabilities = vec!["model:cafe".into()];
        assert!(!f.accepts(&neither)); // has neither
                                       // Empty require_any = no constraint (composite semantics).
        assert!(NumericFilter::default().accepts(&neither));
    }

    #[test]
    fn require_all_and_require_any_compose() {
        // Both axes apply: ALL of require_all AND ONE of require_any.
        let f = NumericFilter {
            require_all: vec!["model:beef".into()],
            require_any: vec!["model:a1".into(), "model:cafe".into()],
            ..Default::default()
        };
        let mut both = rec(1, 0xAA, 4, 0.0, 0);
        both.capabilities = vec!["model:beef".into(), "model:a1".into()];
        assert!(f.accepts(&both)); // beef (all) + a1 (any)
        let mut all_no_any = rec(2, 0xAA, 4, 0.0, 0);
        all_no_any.capabilities = vec!["model:beef".into()];
        assert!(!f.accepts(&all_no_any)); // beef present, but none of the any-set
    }

    #[test]
    fn numeric_filter_retains_passing_records() {
        let f = NumericFilter {
            min_units: 4,
            max_load: Some(0.5),
            ..Default::default()
        };
        let kept: Vec<IslandId> = numeric_filter(
            vec![
                rec(1, 0xAA, 4, 0.2, 0), // pass
                rec(2, 0xAA, 2, 0.2, 0), // too few units
                rec(3, 0xAA, 8, 0.9, 0), // too loaded
                rec(4, 0xAA, 8, 0.4, 0), // pass
            ],
            &f,
        )
        .into_iter()
        .map(|r| r.id)
        .collect();
        assert_eq!(kept, vec![1, 4]);
    }

    #[test]
    fn least_loaded_orders_by_load_then_id() {
        let order = select_islands(
            vec![
                rec(5, 0xAA, 4, 0.3, 0),
                rec(2, 0xAA, 4, 0.1, 0),
                rec(9, 0xAA, 4, 0.1, 0), // ties 0.1 with island 2 → id breaks
                rec(7, 0xAA, 4, 0.9, 0),
            ],
            SelectionPolicy::LeastLoaded,
        );
        assert_eq!(order, vec![2, 9, 5, 7]);
    }

    #[test]
    fn lowest_id_is_a_total_lock_order_ignoring_load() {
        // The ordered-acquire path needs a stable total order on id,
        // independent of the (churny) load axis.
        let order = select_islands(
            vec![
                rec(30, 0xAA, 4, 0.01, 0),
                rec(10, 0xAA, 4, 0.99, 0),
                rec(20, 0xAA, 4, 0.50, 0),
            ],
            SelectionPolicy::LowestId,
        );
        assert_eq!(order, vec![10, 20, 30]);
    }

    #[test]
    fn pack_orders_most_loaded_first() {
        // Consolidate: busiest filter-passing island first, leaving
        // whole islands idle for future large gangs.
        let order = select_islands(
            vec![
                rec(1, 0xAA, 4, 0.2, 0),
                rec(2, 0xAA, 4, 0.8, 0),
                rec(3, 0xAA, 4, 0.5, 0),
            ],
            SelectionPolicy::Pack,
        );
        assert_eq!(order, vec![2, 3, 1]);
    }

    #[test]
    fn load_band_orders_by_distance_to_target() {
        // Target 0.5: closest-to-half-loaded first.
        let order = select_islands(
            vec![
                rec(1, 0xAA, 4, 0.05, 0), // dist 0.45
                rec(2, 0xAA, 4, 0.55, 0), // dist 0.05
                rec(3, 0xAA, 4, 0.95, 0), // dist 0.45 (ties id 1 → id breaks)
                rec(4, 0xAA, 4, 0.40, 0), // dist 0.10
            ],
            SelectionPolicy::LoadBand(0.5),
        );
        assert_eq!(order, vec![2, 4, 1, 3]);
    }

    #[test]
    fn affinity_ranks_warm_islands_ahead_within_policy() {
        let mut warm_a = rec(1, 0xAA, 4, 0.9, 0); // warm, high load
        warm_a.capabilities = vec!["model:beef".into()];
        let cold_b = rec(2, 0xAA, 4, 0.1, 0); // cold, low load
        let mut warm_c = rec(3, 0xAA, 4, 0.3, 0); // warm, mid load
        warm_c.capabilities = vec!["model:beef".into(), "model:a1".into()];

        // Spread policy: within the warm group least-loaded first
        // (3 then 1), then the cold group (2). Warm beats cold even
        // though cold island 2 is the least loaded overall.
        let order = select_with_affinity(
            vec![warm_a, cold_b, warm_c],
            SelectionPolicy::LeastLoaded,
            Some("model:beef".into()),
        );
        assert_eq!(order, vec![3, 1, 2]);
    }

    #[test]
    fn affinity_none_is_plain_selection() {
        let recs = vec![rec(2, 0xAA, 4, 0.5, 0), rec(1, 0xAA, 4, 0.1, 0)];
        assert_eq!(
            select_with_affinity(recs.clone(), SelectionPolicy::LeastLoaded, None),
            select_islands(recs, SelectionPolicy::LeastLoaded),
        );
    }
}
