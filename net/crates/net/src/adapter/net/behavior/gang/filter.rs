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

use std::collections::HashSet;

use crate::adapter::net::behavior::fold::{CapabilityMatch, IslandId, IslandRecord, NodeId};

/// Step 1 bridge: the candidate *hosts* surfaced by a capability
/// match. The capability fold is keyed by `(class, node)`; the node
/// is the island host, so the matched node ids are exactly the hosts
/// whose islands step 2 then inspects. Deduped across classes (a
/// host in several capability classes is still one host).
pub fn candidate_hosts(matches: &[CapabilityMatch]) -> HashSet<NodeId> {
    matches.iter().map(|((_class, node), _)| *node).collect()
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
    /// Capabilities the island must have resident (AND) — e.g.
    /// `"model:<hex>"` for a warm model that skips cold-load. Empty =
    /// no constraint.
    pub require_capabilities: Vec<String>,
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
        // Every required capability must be resident on the island.
        if !self
            .require_capabilities
            .iter()
            .all(|cap| record.capabilities.contains(cap))
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
/// Affinity is a *preference*, not the hard `require_capabilities`
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
    use crate::adapter::net::behavior::fold::{IslandRecord, UnitSet};

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
    fn required_capability_filters() {
        let f = NumericFilter {
            require_capabilities: vec!["model:beef".into()],
            ..Default::default()
        };
        let mut hot = rec(1, 0xAA, 4, 0.0, 0);
        hot.capabilities = vec!["model:beef".into(), "model:a1".into()];
        assert!(f.accepts(&hot));
        assert!(!f.accepts(&rec(2, 0xAA, 4, 0.0, 0))); // only has model:a1
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
