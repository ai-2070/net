//! [`AggregatorConfig`] — operator-facing configuration for a
//! [`super::Summarizer`]-driven tier-bridging daemon.
//!
//! The configuration is plain-data: no live runtime handles, no
//! `Arc`s of substrate state. That keeps it `Clone + Send + Sync`
//! and lets `ReplicaGroup`'s deterministic-seed derivation
//! (`derive_aggregator_seed`) hash it without reaching into
//! interior mutable state.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::adapter::net::channel::Visibility;
use crate::adapter::net::subnet::SubnetId;

use super::Summarizer;

/// Aggregator daemon configuration.
///
/// The substrate consumes this at daemon construction time;
/// fields are not mutated post-construction. Operators producing
/// a config for `ReplicaGroup::spawn` can hash the fields with
/// `derive_aggregator_seed` to produce a stable group identity
/// across re-placements.
#[derive(Clone)]
pub struct AggregatorConfig {
    /// Source subnet the aggregator covers. Replicas of this
    /// daemon are placed in this subnet via the
    /// `SubnetPolicy`-driven placement gate.
    pub source_subnet: SubnetId,
    /// Visibility config for the summary channels this daemon
    /// publishes. Typically [`Visibility::ParentVisible`] (one tier
    /// up), [`Visibility::Exported`] with explicit targets, or
    /// [`Visibility::Global`].
    pub summary_visibility: Visibility,
    /// Explicit destination subnets for [`Visibility::Exported`]
    /// summary channels. Ignored when `summary_visibility` is not
    /// `Exported`.
    pub summary_targets: Vec<SubnetId>,
    /// Fold kinds (the `KIND_ID` u16 each fold defines as a
    /// const) the daemon aggregates. Each fold kind must have
    /// either a built-in summarizer or an entry in
    /// `custom_summarizers`.
    pub fold_kinds: Vec<u16>,
    /// How often to recompute + republish summaries. Per-replica
    /// — every replica in the group publishes on its own
    /// schedule (the plan's "all replicas publish" decision).
    pub summary_interval: Duration,
    /// Optional override summarizers, keyed on fold kind.
    /// Built-in summarizers (capability, reservation) are used
    /// for any fold-kind not present in this map.
    pub custom_summarizers: HashMap<u16, Arc<dyn Summarizer>>,
}

impl AggregatorConfig {
    /// Build a default-ish config bound to a source subnet.
    /// Operator tooling tightens it via builder-style setters.
    pub fn new(source_subnet: SubnetId) -> Self {
        Self {
            source_subnet,
            summary_visibility: Visibility::ParentVisible,
            summary_targets: Vec::new(),
            fold_kinds: Vec::new(),
            summary_interval: Duration::from_secs(30),
            custom_summarizers: HashMap::new(),
        }
    }

    /// Builder: replace the summary-visibility config.
    pub fn with_visibility(mut self, visibility: Visibility) -> Self {
        self.summary_visibility = visibility;
        self
    }

    /// Builder: replace the explicit export targets. Only
    /// honoured when `summary_visibility` is
    /// [`Visibility::Exported`]; otherwise stored but unused.
    pub fn with_targets(mut self, targets: Vec<SubnetId>) -> Self {
        self.summary_targets = targets;
        self
    }

    /// Builder: extend the fold-kind list. Re-adding the same
    /// kind is a no-op.
    pub fn with_fold_kind(mut self, kind: u16) -> Self {
        if !self.fold_kinds.contains(&kind) {
            self.fold_kinds.push(kind);
        }
        self
    }

    /// Builder: replace the summary cadence.
    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.summary_interval = interval;
        self
    }

    /// Builder: register a custom summarizer for `kind`. Replaces
    /// any prior registration on the same kind.
    pub fn with_custom_summarizer(mut self, kind: u16, summarizer: Arc<dyn Summarizer>) -> Self {
        self.custom_summarizers.insert(kind, summarizer);
        self
    }
}

impl std::fmt::Debug for AggregatorConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AggregatorConfig")
            .field("source_subnet", &self.source_subnet)
            .field("summary_visibility", &self.summary_visibility)
            .field("summary_targets", &self.summary_targets)
            .field("fold_kinds", &self.fold_kinds)
            .field("summary_interval", &self.summary_interval)
            .field("custom_summarizers_count", &self.custom_summarizers.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_chain_round_trips_fields() {
        let cfg = AggregatorConfig::new(SubnetId::new(&[3, 7]))
            .with_visibility(Visibility::Exported)
            .with_targets(vec![SubnetId::new(&[3]), SubnetId::new(&[4])])
            .with_fold_kind(0x0001)
            .with_fold_kind(0x0003)
            .with_fold_kind(0x0001) // duplicate — no-op
            .with_interval(Duration::from_secs(15));

        assert_eq!(cfg.source_subnet, SubnetId::new(&[3, 7]));
        assert_eq!(cfg.summary_visibility, Visibility::Exported);
        assert_eq!(
            cfg.summary_targets,
            vec![SubnetId::new(&[3]), SubnetId::new(&[4])]
        );
        assert_eq!(cfg.fold_kinds, vec![0x0001u16, 0x0003u16]);
        assert_eq!(cfg.summary_interval, Duration::from_secs(15));
        assert!(cfg.custom_summarizers.is_empty());
    }

    #[test]
    fn defaults_to_parent_visible_with_no_targets_or_folds() {
        let cfg = AggregatorConfig::new(SubnetId::new(&[1]));
        assert_eq!(cfg.summary_visibility, Visibility::ParentVisible);
        assert!(cfg.summary_targets.is_empty());
        assert!(cfg.fold_kinds.is_empty());
        assert_eq!(cfg.summary_interval, Duration::from_secs(30));
    }
}
