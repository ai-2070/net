//! Correlated failure detection.
//!
//! Wraps `FailureDetector` with a time-windowed correlation layer.
//! Classifies failures as independent or correlated (mass failure),
//! and identifies whether failures are concentrated in a subnet
//! (likely partition) or spread broadly (likely infrastructure outage).

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use crate::adapter::net::subnet::SubnetId;

/// Configuration for correlated failure detection.
#[derive(Debug, Clone)]
pub struct CorrelatedFailureConfig {
    /// Time window for correlating failures.
    pub correlation_window: Duration,
    /// Fraction of tracked nodes failing within the window to trigger
    /// mass-failure classification (0.0 - 1.0).
    pub mass_failure_threshold: f32,
    /// If this fraction of failures in the window share a common subnet
    /// ancestor, classify as subnet-correlated (likely partition).
    pub subnet_correlation_threshold: f32,
    /// Maximum concurrent recovery actions during mass failure.
    pub max_concurrent_migrations: usize,
}

impl Default for CorrelatedFailureConfig {
    fn default() -> Self {
        Self {
            correlation_window: Duration::from_secs(2),
            mass_failure_threshold: 0.30,
            subnet_correlation_threshold: 0.80,
            max_concurrent_migrations: 3,
        }
    }
}

/// A recorded failure event within the correlation window.
#[derive(Debug, Clone)]
struct FailureEvent {
    node_id: u64,
    detected_at: Instant,
    _subnet: Option<SubnetId>,
}

/// Verdict from correlated failure analysis.
#[derive(Debug, Clone)]
pub enum CorrelationVerdict {
    /// Independent failures — handle normally via RecoveryManager.
    Independent {
        /// Nodes that failed.
        failed_nodes: Vec<u64>,
    },
    /// Mass correlated failure — throttle recovery.
    MassFailure {
        /// Nodes that failed.
        failed_nodes: Vec<u64>,
        /// Fraction of tracked nodes that failed.
        failure_ratio: f32,
        /// Suspected root cause.
        suspected_cause: FailureCause,
    },
}

impl CorrelationVerdict {
    /// Get the failed nodes regardless of verdict type.
    pub fn failed_nodes(&self) -> &[u64] {
        match self {
            Self::Independent { failed_nodes } => failed_nodes,
            Self::MassFailure { failed_nodes, .. } => failed_nodes,
        }
    }

    /// Whether this is a mass failure.
    pub fn is_mass_failure(&self) -> bool {
        matches!(self, Self::MassFailure { .. })
    }
}

/// Suspected cause of a mass failure.
#[derive(Debug, Clone, PartialEq)]
pub enum FailureCause {
    /// Failures concentrated in a single subnet (likely partition).
    SubnetFailure {
        /// The subnet ancestor where failures are concentrated.
        subnet: SubnetId,
        /// Fraction of failures in this subnet.
        affected_ratio: f32,
    },
    /// Failures spread across subnets (likely infrastructure outage).
    BroadOutage,
    /// Insufficient subnet data to determine cause.
    Unknown,
}

/// Correlated failure detector.
///
/// Sits alongside `FailureDetector` as a correlation layer. Consumes
/// failure events and classifies them as independent or correlated.
pub struct CorrelatedFailureDetector {
    config: CorrelatedFailureConfig,
    /// Recent failures within the correlation window.
    recent_failures: VecDeque<FailureEvent>,
    /// Node -> subnet mapping for correlation analysis.
    node_subnets: HashMap<u64, SubnetId>,
    /// Whether we're currently in mass-failure mode.
    in_mass_failure: bool,
}

impl CorrelatedFailureDetector {
    /// Create a new detector with the given configuration.
    pub fn new(config: CorrelatedFailureConfig) -> Self {
        Self {
            config,
            recent_failures: VecDeque::new(),
            node_subnets: HashMap::new(),
            in_mass_failure: false,
        }
    }

    /// Register a node's subnet for correlation analysis.
    pub fn register_node(&mut self, node_id: u64, subnet: SubnetId) {
        self.node_subnets.insert(node_id, subnet);
    }

    /// Record new failures and classify them.
    ///
    /// Call this after `FailureDetector::check_all()` with the newly
    /// failed nodes and the total number of tracked nodes.
    pub fn record_failures(
        &mut self,
        failed_nodes: &[u64],
        total_tracked: usize,
    ) -> CorrelationVerdict {
        let now = Instant::now();

        // Record new failures
        for &node_id in failed_nodes {
            self.recent_failures.push_back(FailureEvent {
                node_id,
                detected_at: now,
                _subnet: self.node_subnets.get(&node_id).copied(),
            });
        }

        // Prune events older than the correlation window.
        //
        // `now - duration` panics when `duration > now.elapsed()`
        // (shortly after process start with a long correlation
        // window). With the default 2s window this is fine, but
        // configurable windows of hours/days can panic on the
        // first second of process life. Saturate to the
        // process-start `Instant` (`now - now.elapsed()`) so the
        // cutoff is at most as old as the earliest possible
        // observation — equivalent to "no events to prune yet."
        let cutoff = now
            .checked_sub(self.config.correlation_window)
            .unwrap_or_else(|| now - now.elapsed());
        while self
            .recent_failures
            .front()
            .is_some_and(|e| e.detected_at < cutoff)
        {
            self.recent_failures.pop_front();
        }

        // Count unique failures in the window.
        //
        // Pre-fix this collected through a `HashSet<u64>`
        // and converted back to `Vec`, which exposed the HashSet's
        // randomized iteration order to downstream consumers.
        // `window_failures` flows verbatim into
        // `PartitionRecord::other_side` (partition.rs:160), so two
        // nodes with identical inputs produced different
        // `other_side` orderings, breaking cross-node serialization
        // / reconcile-ordering / replay validation. Sort + dedup
        // gives a canonical Vec deterministic across processes.
        let mut window_failures: Vec<u64> =
            self.recent_failures.iter().map(|e| e.node_id).collect();
        window_failures.sort_unstable();
        window_failures.dedup();

        if total_tracked == 0 {
            return CorrelationVerdict::Independent {
                failed_nodes: failed_nodes.to_vec(),
            };
        }

        let failure_ratio = window_failures.len() as f32 / total_tracked as f32;

        if failure_ratio < self.config.mass_failure_threshold {
            self.in_mass_failure = false;
            return CorrelationVerdict::Independent {
                failed_nodes: failed_nodes.to_vec(),
            };
        }

        // Mass failure detected — analyze subnet correlation
        self.in_mass_failure = true;
        let cause = self.analyze_subnet_correlation(&window_failures);

        CorrelationVerdict::MassFailure {
            failed_nodes: window_failures,
            failure_ratio,
            suspected_cause: cause,
        }
    }

    /// How many concurrent recovery actions are allowed.
    ///
    /// Throttled during mass failure to avoid overloading survivors.
    pub fn recovery_budget(&self) -> usize {
        if self.in_mass_failure {
            self.config.max_concurrent_migrations
        } else {
            usize::MAX
        }
    }

    /// Whether we're currently in mass-failure mode.
    pub fn in_mass_failure(&self) -> bool {
        self.in_mass_failure
    }

    /// Clear the failure window (e.g., when conditions normalize).
    pub fn clear_window(&mut self) {
        self.recent_failures.clear();
        self.in_mass_failure = false;
    }

    /// Number of failures in the current window.
    pub fn window_size(&self) -> usize {
        self.recent_failures.len()
    }

    /// Analyze whether failures are concentrated in a subnet subtree.
    fn analyze_subnet_correlation(&self, failed_nodes: &[u64]) -> FailureCause {
        let mut subnet_counts: HashMap<SubnetId, usize> = HashMap::new();
        let mut with_subnet = 0usize;

        for &node_id in failed_nodes {
            if let Some(&subnet) = self.node_subnets.get(&node_id) {
                with_subnet += 1;
                // Count at each hierarchy level. The break
                // conditions (`parent == current`, `parent.is_global`)
                // cover every well-formed `SubnetId::parent`
                // implementation, but a defensive depth cap
                // matches the 4-level hierarchy and forecloses
                // an infinite loop if a future regression in
                // `SubnetId::parent` ever returns a non-self,
                // non-global subnet that cycles back to an
                // ancestor (e.g., a typo in a 4→3→2→1→4 walk
                // returning to the deepest level). The cap is
                // generously above the 4-level hierarchy so
                // legitimate walks always complete inside it.
                let mut current = subnet;
                for _ in 0..8 {
                    *subnet_counts.entry(current).or_insert(0) += 1;
                    let parent = current.parent();
                    if parent == current || parent.is_global() {
                        break;
                    }
                    current = parent;
                }
            }
        }

        if with_subnet == 0 {
            return FailureCause::Unknown;
        }

        // Find the most specific subnet with the highest concentration
        // Ceiling to avoid false subnet correlation from rounding down
        let threshold =
            (with_subnet as f32 * self.config.subnet_correlation_threshold).ceil() as usize;

        // Iterate a sorted snapshot so ties resolve deterministically:
        // higher `depth` wins; on equal depth, the lower `SubnetId`
        // wins (the inner `u32` comparison is a stable total order
        // without semantic hierarchy meaning — see `SubnetId`'s
        // `Ord` rustdoc). Iterating a `HashMap` directly with `>=` on
        // depth as the tiebreaker would let hash iteration order
        // (randomized per process) pick the winner, and downstream
        // `partition.rs::detect` would brand the partition record
        // with a subnet that flips between runs given identical
        // inputs.
        let mut entries: Vec<(SubnetId, usize)> =
            subnet_counts.iter().map(|(&s, &c)| (s, c)).collect();
        entries.sort_by(|a, b| b.0.depth().cmp(&a.0.depth()).then_with(|| a.0.cmp(&b.0)));

        // Sort (above) guarantees the entries are visited deepest-
        // first within the threshold-meeting set, so the first hit
        // is the deterministic winner. We `break` immediately on
        // the first hit; no `best_depth` tracking needed.
        let mut best_subnet = None;
        for (subnet, count) in entries {
            if count >= threshold {
                best_subnet = Some(subnet);
                break;
            }
        }

        match best_subnet {
            Some(subnet) => {
                let ratio = *subnet_counts.get(&subnet).unwrap() as f32 / with_subnet as f32;
                FailureCause::SubnetFailure {
                    subnet,
                    affected_ratio: ratio,
                }
            }
            None => FailureCause::BroadOutage,
        }
    }
}

impl std::fmt::Debug for CorrelatedFailureDetector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CorrelatedFailureDetector")
            .field("window_size", &self.recent_failures.len())
            .field("tracked_nodes", &self.node_subnets.len())
            .field("in_mass_failure", &self.in_mass_failure)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_detector(threshold: f32) -> CorrelatedFailureDetector {
        CorrelatedFailureDetector::new(CorrelatedFailureConfig {
            mass_failure_threshold: threshold,
            ..Default::default()
        })
    }

    #[test]
    fn test_independent_failures() {
        let mut det = make_detector(0.30);
        for i in 0..10 {
            det.register_node(i, SubnetId::new(&[1]));
        }

        // 1 out of 10 fails = 10% < 30%
        let verdict = det.record_failures(&[0], 10);
        assert!(!verdict.is_mass_failure());
        assert!(!det.in_mass_failure());
        assert_eq!(det.recovery_budget(), usize::MAX);
    }

    #[test]
    fn test_mass_failure() {
        let mut det = make_detector(0.30);
        for i in 0..10 {
            det.register_node(i, SubnetId::new(&[1]));
        }

        // 4 out of 10 fails = 40% > 30%
        let verdict = det.record_failures(&[0, 1, 2, 3], 10);
        assert!(verdict.is_mass_failure());
        assert!(det.in_mass_failure());
        assert_eq!(det.recovery_budget(), 3); // default max_concurrent_migrations
    }

    #[test]
    fn test_subnet_correlated() {
        let mut det = make_detector(0.30);
        // 5 nodes in subnet [1, 1], 5 in subnet [1, 2]
        for i in 0..5 {
            det.register_node(i, SubnetId::new(&[1, 1]));
        }
        for i in 5..10 {
            det.register_node(i, SubnetId::new(&[1, 2]));
        }

        // All 5 nodes in subnet [1, 1] fail
        let verdict = det.record_failures(&[0, 1, 2, 3, 4], 10);
        assert!(verdict.is_mass_failure());

        if let CorrelationVerdict::MassFailure {
            suspected_cause, ..
        } = &verdict
        {
            match suspected_cause {
                FailureCause::SubnetFailure { subnet, .. } => {
                    // Should identify subnet [1, 1] as the correlated subnet
                    assert_eq!(*subnet, SubnetId::new(&[1, 1]));
                }
                other => panic!("expected SubnetFailure, got {:?}", other),
            }
        }
    }

    #[test]
    fn test_broad_outage() {
        let mut det = make_detector(0.30);
        // Nodes spread across 4 different subnets
        det.register_node(0, SubnetId::new(&[1]));
        det.register_node(1, SubnetId::new(&[2]));
        det.register_node(2, SubnetId::new(&[3]));
        det.register_node(3, SubnetId::new(&[4]));
        for i in 4..10 {
            det.register_node(i, SubnetId::new(&[(i + 1) as u8]));
        }

        // Failures spread across all subnets
        let verdict = det.record_failures(&[0, 1, 2, 3], 10);
        assert!(verdict.is_mass_failure());

        if let CorrelationVerdict::MassFailure {
            suspected_cause, ..
        } = &verdict
        {
            assert_eq!(*suspected_cause, FailureCause::BroadOutage);
        }
    }

    /// Regression for BUG_AUDIT_2026_04_30_CORE.md #91: previously
    /// `analyze_subnet_correlation` iterated `subnet_counts` (a
    /// `HashMap`) directly with `>=` on depth as the tiebreaker.
    /// On tied `best_depth`, the chosen subnet depended on hash
    /// iteration order, which `std::collections::HashMap` randomizes
    /// per process — recovery scope flipped between runs given
    /// identical inputs.
    ///
    /// We pin the deterministic-tiebreak fix by:
    ///   1. Building a scenario with two equally-deep subnets
    ///      that both meet the correlation threshold and have
    ///      equal failure counts (the pre-fix nondeterminism
    ///      window).
    ///   2. Running the analysis many times back-to-back. The
    ///      same `CorrelatedFailureDetector` is rebuilt each
    ///      iteration to maximize the chance the underlying
    ///      hasher state shifts.
    ///   3. Asserting every run picks the same subnet — the
    ///      lower `SubnetId` wins on ties (per the new sort).
    ///
    /// Pre-fix this would intermittently return `SubnetId::new(&[1, 2])`
    /// instead of `SubnetId::new(&[1, 1])`.
    #[test]
    fn ties_resolve_deterministically_across_runs() {
        for _attempt in 0..32 {
            // Two sibling subnets at depth 2, each with 3 nodes.
            // Threshold of 0.30 means a subnet needs count ≥
            // ceil(6 * 0.30) = 2 to qualify. Both [1,1] and [1,2]
            // hit count=3 — the tied case the pre-fix code
            // resolved nondeterministically. (The default
            // `subnet_correlation_threshold` of 0.80 would put
            // the threshold at 5 and select only the parent
            // rollup, so we override it explicitly.)
            let mut det = CorrelatedFailureDetector::new(CorrelatedFailureConfig {
                mass_failure_threshold: 0.30,
                subnet_correlation_threshold: 0.30,
                ..Default::default()
            });
            for i in 0..3 {
                det.register_node(i, SubnetId::new(&[1, 1]));
            }
            for i in 3..6 {
                det.register_node(i, SubnetId::new(&[1, 2]));
            }
            for i in 6..10 {
                det.register_node(i, SubnetId::new(&[2, (i as u8)]));
            }

            // Fail all 6 nodes in [1,1] + [1,2]. with_subnet=6.
            // Both [1,1] and [1,2] hit count=3 ≥ 2 at depth 2;
            // [1] hits count=6 at depth 1. Pre-fix the loop
            // would pick whichever depth-2 child the HashMap
            // visited last in iteration order.
            let verdict = det.record_failures(&[0, 1, 2, 3, 4, 5], 20);
            assert!(verdict.is_mass_failure());
            if let CorrelationVerdict::MassFailure {
                suspected_cause, ..
            } = &verdict
            {
                match suspected_cause {
                    FailureCause::SubnetFailure { subnet, .. } => {
                        // Deterministic tiebreak: lower id wins
                        // on equal depth. `SubnetId::new(&[1, 1])`
                        // < `SubnetId::new(&[1, 2])` under the
                        // derived `Ord` on the inner u32.
                        assert_eq!(
                            *subnet,
                            SubnetId::new(&[1, 1]),
                            "tied subnets at the same depth must \
                             resolve to the lower SubnetId every \
                             run — pre-fix this flipped between \
                             [1,1] and [1,2] depending on hash \
                             iteration order"
                        );
                    }
                    other => panic!("expected SubnetFailure, got {:?}", other),
                }
            }
        }
    }

    #[test]
    fn test_clear_window() {
        let mut det = make_detector(0.30);
        for i in 0..10 {
            det.register_node(i, SubnetId::new(&[1]));
        }

        det.record_failures(&[0, 1, 2, 3], 10);
        assert!(det.in_mass_failure());

        det.clear_window();
        assert!(!det.in_mass_failure());
        assert_eq!(det.window_size(), 0);
    }

    #[test]
    fn test_no_subnet_data() {
        let mut det = make_detector(0.30);
        // Don't register any subnets

        let verdict = det.record_failures(&[0, 1, 2, 3], 10);
        assert!(verdict.is_mass_failure());

        if let CorrelationVerdict::MassFailure {
            suspected_cause, ..
        } = &verdict
        {
            assert_eq!(*suspected_cause, FailureCause::Unknown);
        }
    }

    /// Pin: a correlation window longer than the time the process
    /// has been running must NOT panic in the prune path. Pre-fix
    /// `now - self.config.correlation_window` panicked when
    /// `correlation_window > now.elapsed()` — trivially reachable
    /// for any operator-tunable window of hours/days that's
    /// hit during the first second of process startup.
    #[test]
    fn record_failures_does_not_panic_with_long_window_at_startup() {
        let mut det = CorrelatedFailureDetector::new(CorrelatedFailureConfig {
            mass_failure_threshold: 0.30,
            // A correlation window much larger than any plausible
            // process-uptime-since-Instant-creation. Pre-fix this
            // panicked inside `now - duration`.
            correlation_window: std::time::Duration::from_secs(86_400 * 365),
            ..Default::default()
        });
        for i in 0..10 {
            det.register_node(i, SubnetId::new(&[1]));
        }
        // Should not panic; should still produce a valid verdict.
        let verdict = det.record_failures(&[0, 1, 2, 3], 10);
        assert!(verdict.is_mass_failure());
    }

    /// `failed_nodes` in the verdict (sourced from
    /// `window_failures` after dedup) must be sorted, not in
    /// arbitrary HashSet iteration order. Pre-fix the same input
    /// could produce different orderings on each run / process,
    /// breaking serialization parity across nodes that observe
    /// the same partition.
    ///
    /// We can't easily test "different across processes" in a
    /// single test run, but we CAN check the ordering is
    /// monotonic, which is a strong proxy: a sorted output is
    /// canonical, while a HashSet output is not.
    #[test]
    fn mass_failure_failed_nodes_are_sorted_canonically() {
        let mut det = make_detector(0.30);
        for i in 0..10 {
            det.register_node(i, SubnetId::new(&[1]));
        }

        // Record failures in a deliberately-unsorted order.
        let verdict = det.record_failures(&[7, 2, 9, 4, 0, 5, 8, 1], 10);
        assert!(verdict.is_mass_failure());
        if let CorrelationVerdict::MassFailure { failed_nodes, .. } = &verdict {
            let mut sorted = failed_nodes.clone();
            sorted.sort_unstable();
            assert_eq!(
                failed_nodes, &sorted,
                "MassFailure.failed_nodes must be in canonical \
                 (sorted) order; pre-fix it leaked HashSet iteration order \
                 and varied per process"
            );
        }
    }
}
