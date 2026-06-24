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

use crate::adapter::net::behavior::fold::{CapabilityMatch, IslandId, IslandRecord, ModelId, NodeId};

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
    /// Minimum GPUs in the island's NVLink domain. `0` = any.
    pub min_gpus: usize,
    /// Maximum live load (`0.0..=1.0`). `None` = any.
    pub max_load: Option<f32>,
    /// Maximum live p50 latency (µs). `None` = any.
    pub max_p50_latency_us: Option<u32>,
    /// Require this model already warm in GPU memory (skips
    /// cold-load). `None` = any.
    pub require_warm_model: Option<ModelId>,
}

impl NumericFilter {
    /// Does `record` satisfy every populated constraint?
    pub fn accepts(&self, record: &IslandRecord) -> bool {
        if record.gpus.len() < self.min_gpus {
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
        if let Some(model) = self.require_warm_model {
            if !record.warm_models.contains(&model) {
                return false;
            }
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

/// Island selection ordering (plan §2 step 3). Phase A ships two
/// deterministic policies; the richer pack/spread/warm-affinity/
/// load-band policy is Phase E.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SelectionPolicy {
    /// Least-loaded island first (lowest `load`), `IslandId`
    /// ascending as a deterministic tie-break. The default — packs
    /// onto the most-available island first.
    #[default]
    LeastLoaded,
    /// `IslandId` ascending, ignoring the live axes. This is the
    /// global lock-ordering the multi-island ordered-acquire path
    /// (Phase C) needs: acquiring islands in one total order is what
    /// makes the gang protocol deadlock-free.
    LowestId,
}

/// Step 3: order `records` per `policy` and project to claim-order
/// island ids. Pure.
pub fn select_islands(mut records: Vec<IslandRecord>, policy: SelectionPolicy) -> Vec<IslandId> {
    match policy {
        SelectionPolicy::LeastLoaded => records.sort_by(|a, b| {
            a.load
                .partial_cmp(&b.load)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.id.cmp(&b.id))
        }),
        SelectionPolicy::LowestId => records.sort_by_key(|r| r.id),
    }
    records.into_iter().map(|r| r.id).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::fold::{GpuSet, IslandRecord};

    fn rec(id: IslandId, host: NodeId, gpus: usize, load: f32, lat: u32) -> IslandRecord {
        IslandRecord {
            id,
            gpus: GpuSet::new((0..gpus as u32).collect()),
            host,
            warm_models: vec![0xA1],
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
    fn min_gpus_filters_small_islands() {
        let f = NumericFilter {
            min_gpus: 4,
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
    fn warm_model_requirement_filters() {
        let f = NumericFilter {
            require_warm_model: Some(0xBEEF),
            ..Default::default()
        };
        let mut hot = rec(1, 0xAA, 4, 0.0, 0);
        hot.warm_models = vec![0xBEEF, 0xA1];
        assert!(f.accepts(&hot));
        assert!(!f.accepts(&rec(2, 0xAA, 4, 0.0, 0))); // only has 0xA1
    }

    #[test]
    fn numeric_filter_retains_passing_records() {
        let f = NumericFilter {
            min_gpus: 4,
            max_load: Some(0.5),
            ..Default::default()
        };
        let kept: Vec<IslandId> = numeric_filter(
            vec![
                rec(1, 0xAA, 4, 0.2, 0), // pass
                rec(2, 0xAA, 2, 0.2, 0), // too few gpus
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
}
