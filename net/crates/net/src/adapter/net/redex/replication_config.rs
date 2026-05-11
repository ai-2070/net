//! Per-channel replication configuration — Phase B opt-in for
//! `docs/plans/REDEX_DISTRIBUTED_PLAN.md` §1.
//!
//! `ReplicationConfig` is the opt-in surface that turns a v1 / v2
//! single-node `RedexFile` into a replicated channel. It lives on
//! [`RedexFileConfig::replication`](super::RedexFileConfig) as an
//! `Option`; the default `None` keeps every existing channel single-
//! node (no observable change). Phase C wires the `ReplicationCoordinator`
//! daemon's spawn path to consult this field on `Redex::open_file`.
//!
//! Validation is fail-fast at config-build time — pin invariants here
//! so a malformed config can't escape into the coordinator's hot
//! loop. The `validate()` method returns a typed `ReplicationConfigError`
//! covering every reject path; the `with_*` builder methods are
//! validation-free for ergonomic chaining and pair with a final
//! `validate()` call before the config is committed to a `Redex`.

use crate::adapter::net::behavior::placement::NodeId;

/// Replication factor lower bound. `1` collapses to single-node-with-
/// coordinator (the daemon runs but there's only one replica) — useful
/// for testing and the brief moment between channel-open and the first
/// peer joining; below `1` is meaningless.
pub const REPLICATION_FACTOR_MIN: u8 = 1;
/// Replication factor upper bound. The protocol allows up to 255
/// replicas per channel (u8), but anything beyond ~16 stops scaling
/// (heartbeat fanout dominates) and we clamp here so a misconfig
/// can't accidentally fanout to hundreds. Operators with genuine
/// 16+-replica workloads can plumb the override; v1 keeps the
/// ceiling conservative.
pub const REPLICATION_FACTOR_MAX: u8 = 16;

/// Default replication factor — three is the conventional minimum
/// for a single-leader log to tolerate one replica failure while
/// staying in single-partition quorum-irrelevant configuration.
pub const REPLICATION_FACTOR_DEFAULT: u8 = 3;

/// Minimum heartbeat cadence. Below 100 ms heartbeat traffic
/// dominates the channel's effective throughput; pin a floor here so
/// a misconfig can't accidentally turn the heartbeat path into a
/// busy-loop.
pub const HEARTBEAT_MS_MIN: u64 = 100;

/// Default heartbeat cadence — 500 ms matches the plan §1 default and
/// the §6 "3 × heartbeat" lag bound; with three-missed hysteresis the
/// effective failover detection window is ~1.5 s, well under the
/// activation-gate's "< 5 s RTO" target.
pub const HEARTBEAT_MS_DEFAULT: u64 = 500;

/// Default replication-sync I/O budget as a fraction of measured NIC
/// peak. `0.5` lets replication consume half the link without
/// starving foreground publish traffic; tune per channel via
/// [`ReplicationConfig::with_replication_budget_fraction`].
pub const REPLICATION_BUDGET_FRACTION_DEFAULT: f32 = 0.5;

/// Where replicas live and how they're chosen at channel-open time
/// and on roster change. Mirrors the four-axis intent the plan §1
/// pins:
///
/// - [`PlacementStrategy::Standard`] — let `PlacementFilter` decide
///   (default for new channels). Reads `metadata.intent`,
///   `metadata.colocate-with`, `scope:`, proximity, and resource-
///   availability axes.
/// - [`PlacementStrategy::Pinned`] — manual placement on a fixed
///   `NodeId` set. Used for special-case topologies and tests.
/// - [`PlacementStrategy::ColocationStrict`] — every replica must
///   live on a node already holding the chain referenced by
///   `metadata.colocate-with-strict`. Refuses placement on
///   insufficient-coverage nodes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum PlacementStrategy {
    /// Spread across nodes per the `PlacementFilter` primitive shipped
    /// in The Warriors. Reads `metadata.intent`, `metadata.colocate-
    /// with`, `scope:`, proximity, and resource-availability axes.
    /// Default for new channels.
    #[default]
    Standard,
    /// Manual pinning. Used for special-case topologies and tests.
    /// The vector lists the exact `NodeId` set that should run
    /// replicas — its length pins the effective replication factor
    /// regardless of [`ReplicationConfig::factor`].
    Pinned(Vec<NodeId>),
    /// Strict colocation — all replicas must be on nodes already
    /// holding the chain referenced by `metadata.colocate-with-
    /// strict`. Refuses placement on insufficient-coverage nodes.
    ColocationStrict,
}

/// Behavior when a replica falls below the channel's retention
/// requirement due to local disk pressure. The leader's replication
/// factor is a hard guarantee; replicas are best-effort under
/// pressure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnderCapacity {
    /// Drop the replica role; fall through to greedy LRU if also
    /// enabled. The channel's capability tag for this node is
    /// withdrawn and reads re-route to the leader. **Default.**
    #[default]
    Withdraw,
    /// Aggressively evict the oldest local data to maintain the
    /// channel's retention even if total data exceeds disk. Trades
    /// older data for keeping the replication factor intact.
    EvictOldest,
}

/// Per-channel replication opt-in. The default-when-set
/// [`ReplicationConfig::new`] gives sensible values (factor 3,
/// 500 ms heartbeat, standard placement, withdraw-under-capacity,
/// 0.5 NIC peak budget); the `with_*` builders adjust individual
/// fields.
///
/// `validate()` runs the full set of invariants pinned at module top
/// — callers should invoke it before committing the config to a
/// `Redex` so a malformed config can't leak into the coordinator's
/// hot loop.
#[derive(Debug, Clone, PartialEq)]
pub struct ReplicationConfig {
    /// Replication factor — number of replicas (including the leader)
    /// maintained. Must satisfy
    /// `REPLICATION_FACTOR_MIN <= factor <= REPLICATION_FACTOR_MAX`.
    /// Default [`REPLICATION_FACTOR_DEFAULT`].
    pub factor: u8,
    /// How replicas are chosen when first instantiated and on roster
    /// change. Default [`PlacementStrategy::Standard`].
    pub placement: PlacementStrategy,
    /// Heartbeat interval between leader and replicas, in
    /// milliseconds. Must satisfy
    /// `heartbeat_ms >= HEARTBEAT_MS_MIN`. Default
    /// [`HEARTBEAT_MS_DEFAULT`].
    pub heartbeat_ms: u64,
    /// Optional override pinning the leader to a specific node.
    /// `None` = leader is the channel's natural publisher (the
    /// `ChannelPublisher` home). When `Some(node)`, the override
    /// applies on every leader election; the deterministic
    /// nearest-RTT election picks `node` whenever it's healthy.
    pub leader_pinned: Option<NodeId>,
    /// Behavior when a replica falls below the channel's retention
    /// requirement due to local disk pressure. Default
    /// [`UnderCapacity::Withdraw`].
    pub on_under_capacity: UnderCapacity,
    /// Bandwidth budget for replication-sync I/O, as a fraction of
    /// measured NIC peak. Must lie in `(0.0, 1.0]` and be finite.
    /// Default [`REPLICATION_BUDGET_FRACTION_DEFAULT`].
    pub replication_budget_fraction: f32,
}

impl Default for ReplicationConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl ReplicationConfig {
    /// Construct a [`ReplicationConfig`] with all defaults — factor
    /// 3, 500 ms heartbeat, standard placement, withdraw-under-
    /// capacity, 0.5 NIC peak budget, leader = natural publisher.
    pub fn new() -> Self {
        Self {
            factor: REPLICATION_FACTOR_DEFAULT,
            placement: PlacementStrategy::default(),
            heartbeat_ms: HEARTBEAT_MS_DEFAULT,
            leader_pinned: None,
            on_under_capacity: UnderCapacity::default(),
            replication_budget_fraction: REPLICATION_BUDGET_FRACTION_DEFAULT,
        }
    }

    /// Set the replication factor. Validate via [`Self::validate`]
    /// before committing the config to a `Redex`.
    pub fn with_factor(mut self, factor: u8) -> Self {
        self.factor = factor;
        self
    }

    /// Set the placement strategy.
    pub fn with_placement(mut self, placement: PlacementStrategy) -> Self {
        self.placement = placement;
        self
    }

    /// Set the heartbeat cadence in milliseconds.
    pub fn with_heartbeat_ms(mut self, heartbeat_ms: u64) -> Self {
        self.heartbeat_ms = heartbeat_ms;
        self
    }

    /// Pin the leader to a specific `NodeId`. Pass `None` to fall
    /// back to "leader = natural publisher."
    pub fn with_leader_pinned(mut self, leader: Option<NodeId>) -> Self {
        self.leader_pinned = leader;
        self
    }

    /// Set the under-capacity policy.
    pub fn with_on_under_capacity(mut self, on_under_capacity: UnderCapacity) -> Self {
        self.on_under_capacity = on_under_capacity;
        self
    }

    /// Set the replication-sync I/O budget as a fraction of measured
    /// NIC peak.
    pub fn with_replication_budget_fraction(mut self, fraction: f32) -> Self {
        self.replication_budget_fraction = fraction;
        self
    }

    /// Effective replica count — `placement` may override `factor`:
    /// [`PlacementStrategy::Pinned`] pins the count to the length of
    /// its `Vec<NodeId>` regardless of the configured factor (the
    /// operator's explicit list wins over the numeric hint). All
    /// other strategies honor `factor`.
    pub fn effective_factor(&self) -> u8 {
        match &self.placement {
            PlacementStrategy::Pinned(nodes) => {
                u8::try_from(nodes.len()).unwrap_or(REPLICATION_FACTOR_MAX)
            }
            _ => self.factor,
        }
    }

    /// Run every documented invariant. Returns `Ok(())` when the
    /// config is safe to commit; otherwise a typed
    /// [`ReplicationConfigError`] naming the first violation. Pin
    /// in tests; surface to operators on `Redex::open_file`.
    pub fn validate(&self) -> Result<(), ReplicationConfigError> {
        if self.factor < REPLICATION_FACTOR_MIN {
            return Err(ReplicationConfigError::FactorBelowMin {
                got: self.factor,
                min: REPLICATION_FACTOR_MIN,
            });
        }
        if self.factor > REPLICATION_FACTOR_MAX {
            return Err(ReplicationConfigError::FactorAboveMax {
                got: self.factor,
                max: REPLICATION_FACTOR_MAX,
            });
        }
        if self.heartbeat_ms < HEARTBEAT_MS_MIN {
            return Err(ReplicationConfigError::HeartbeatTooLow {
                got: self.heartbeat_ms,
                min: HEARTBEAT_MS_MIN,
            });
        }
        if !self.replication_budget_fraction.is_finite()
            || self.replication_budget_fraction <= 0.0
            || self.replication_budget_fraction > 1.0
        {
            return Err(ReplicationConfigError::BudgetFractionOutOfRange {
                got: self.replication_budget_fraction,
            });
        }
        if let PlacementStrategy::Pinned(nodes) = &self.placement {
            if nodes.is_empty() {
                return Err(ReplicationConfigError::PinnedSetEmpty);
            }
            if nodes.len() > REPLICATION_FACTOR_MAX as usize {
                return Err(ReplicationConfigError::PinnedSetTooLarge {
                    got: nodes.len(),
                    max: REPLICATION_FACTOR_MAX as usize,
                });
            }
            // Reject duplicate `NodeId`s — pinning a node twice is
            // a misconfig (would the coordinator try to run two
            // local instances? Worse: half the count is silent).
            let mut sorted = nodes.clone();
            sorted.sort_unstable();
            for w in sorted.windows(2) {
                if w[0] == w[1] {
                    return Err(ReplicationConfigError::PinnedDuplicate { node_id: w[0] });
                }
            }
            // If `leader_pinned` is set, it must live in the pinned
            // set — otherwise the operator pinned a leader on a
            // node that won't even run a replica.
            if let Some(leader) = self.leader_pinned {
                if !nodes.contains(&leader) {
                    return Err(ReplicationConfigError::LeaderPinnedOutsidePinnedSet {
                        leader,
                    });
                }
            }
        }
        Ok(())
    }
}

/// Reject reasons surfaced by [`ReplicationConfig::validate`].
///
/// Not `Eq` — the [`Self::BudgetFractionOutOfRange`] variant carries
/// an `f32`, which is `PartialEq` but not `Eq`.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ReplicationConfigError {
    /// `factor` is below the [`REPLICATION_FACTOR_MIN`] floor.
    #[error("replication factor {got} below minimum {min}")]
    FactorBelowMin {
        /// Configured factor value.
        got: u8,
        /// Minimum permitted factor.
        min: u8,
    },
    /// `factor` is above the [`REPLICATION_FACTOR_MAX`] ceiling.
    #[error("replication factor {got} above maximum {max}")]
    FactorAboveMax {
        /// Configured factor value.
        got: u8,
        /// Maximum permitted factor.
        max: u8,
    },
    /// `heartbeat_ms` is below the [`HEARTBEAT_MS_MIN`] floor.
    #[error("heartbeat_ms {got} below minimum {min} ms")]
    HeartbeatTooLow {
        /// Configured heartbeat cadence.
        got: u64,
        /// Minimum permitted heartbeat cadence.
        min: u64,
    },
    /// `replication_budget_fraction` is outside `(0.0, 1.0]` or
    /// non-finite (NaN / ±inf).
    #[error("replication_budget_fraction {got} outside (0.0, 1.0] or non-finite")]
    BudgetFractionOutOfRange {
        /// Configured budget fraction.
        got: f32,
    },
    /// [`PlacementStrategy::Pinned`] supplied an empty `Vec<NodeId>`.
    /// Pinned placement needs at least one node; if the operator
    /// wanted "no replication" they should leave
    /// `RedexFileConfig::replication` at `None` instead.
    #[error("PlacementStrategy::Pinned must list at least one NodeId")]
    PinnedSetEmpty,
    /// [`PlacementStrategy::Pinned`] supplied more than
    /// `REPLICATION_FACTOR_MAX` nodes.
    #[error("PlacementStrategy::Pinned has {got} nodes; ceiling is {max}")]
    PinnedSetTooLarge {
        /// Number of nodes in the pinned set.
        got: usize,
        /// Maximum permitted pinned-set size.
        max: usize,
    },
    /// A `NodeId` appears more than once in the pinned set.
    #[error("PlacementStrategy::Pinned contains duplicate NodeId {node_id:#x}")]
    PinnedDuplicate {
        /// The duplicated `NodeId`.
        node_id: NodeId,
    },
    /// `leader_pinned` names a node that isn't in the pinned set —
    /// the operator pinned a leader on a node that won't even run a
    /// replica.
    #[error(
        "leader_pinned {leader:#x} is not in the PlacementStrategy::Pinned set"
    )]
    LeaderPinnedOutsidePinnedSet {
        /// The leader `NodeId` that lies outside the pinned set.
        leader: NodeId,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_validates() {
        let cfg = ReplicationConfig::new();
        assert_eq!(cfg.factor, REPLICATION_FACTOR_DEFAULT);
        assert_eq!(cfg.heartbeat_ms, HEARTBEAT_MS_DEFAULT);
        assert_eq!(cfg.placement, PlacementStrategy::Standard);
        assert_eq!(cfg.on_under_capacity, UnderCapacity::Withdraw);
        assert!(cfg.leader_pinned.is_none());
        assert!((cfg.replication_budget_fraction - 0.5).abs() < f32::EPSILON);
        cfg.validate().expect("defaults must validate");
    }

    #[test]
    fn builder_chain_threads_through() {
        let cfg = ReplicationConfig::new()
            .with_factor(5)
            .with_heartbeat_ms(250)
            .with_placement(PlacementStrategy::ColocationStrict)
            .with_on_under_capacity(UnderCapacity::EvictOldest)
            .with_leader_pinned(Some(0xDEAD_BEEF))
            .with_replication_budget_fraction(0.75);
        assert_eq!(cfg.factor, 5);
        assert_eq!(cfg.heartbeat_ms, 250);
        assert_eq!(cfg.placement, PlacementStrategy::ColocationStrict);
        assert_eq!(cfg.on_under_capacity, UnderCapacity::EvictOldest);
        assert_eq!(cfg.leader_pinned, Some(0xDEAD_BEEF));
        assert!((cfg.replication_budget_fraction - 0.75).abs() < f32::EPSILON);
        cfg.validate().expect("built config must validate");
    }

    #[test]
    fn factor_below_min_rejected() {
        let cfg = ReplicationConfig::new().with_factor(0);
        let err = cfg.validate().expect_err("factor=0 must fail");
        assert!(matches!(
            err,
            ReplicationConfigError::FactorBelowMin { got: 0, min: 1 }
        ));
    }

    #[test]
    fn factor_above_max_rejected() {
        let cfg = ReplicationConfig::new().with_factor(REPLICATION_FACTOR_MAX + 1);
        let err = cfg.validate().expect_err("factor>max must fail");
        assert!(matches!(
            err,
            ReplicationConfigError::FactorAboveMax { got: 17, max: 16 }
        ));
    }

    #[test]
    fn heartbeat_below_min_rejected() {
        let cfg = ReplicationConfig::new().with_heartbeat_ms(50);
        let err = cfg.validate().expect_err("heartbeat=50ms must fail");
        assert!(matches!(
            err,
            ReplicationConfigError::HeartbeatTooLow { got: 50, min: 100 }
        ));
    }

    #[test]
    fn budget_fraction_out_of_range_rejected() {
        for bad in [0.0, -0.5, 1.5, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let cfg = ReplicationConfig::new().with_replication_budget_fraction(bad);
            let err = cfg
                .validate()
                .expect_err(&format!("budget={bad} must fail but didn't"));
            assert!(
                matches!(err, ReplicationConfigError::BudgetFractionOutOfRange { .. }),
                "budget={bad} produced wrong error: {err:?}"
            );
        }

        // Inverse: 1.0 (the inclusive upper) is fine.
        ReplicationConfig::new()
            .with_replication_budget_fraction(1.0)
            .validate()
            .expect("budget=1.0 is the inclusive upper bound");
    }

    #[test]
    fn pinned_empty_rejected() {
        let cfg = ReplicationConfig::new().with_placement(PlacementStrategy::Pinned(vec![]));
        let err = cfg.validate().expect_err("empty pinned set must fail");
        assert_eq!(err, ReplicationConfigError::PinnedSetEmpty);
    }

    #[test]
    fn pinned_too_large_rejected() {
        let nodes = (0..(REPLICATION_FACTOR_MAX as u64 + 1)).collect();
        let cfg = ReplicationConfig::new().with_placement(PlacementStrategy::Pinned(nodes));
        let err = cfg.validate().expect_err("oversized pinned set must fail");
        assert!(matches!(
            err,
            ReplicationConfigError::PinnedSetTooLarge { got: 17, max: 16 }
        ));
    }

    #[test]
    fn pinned_duplicate_rejected() {
        let cfg = ReplicationConfig::new()
            .with_placement(PlacementStrategy::Pinned(vec![0xAA, 0xBB, 0xAA]));
        let err = cfg.validate().expect_err("duplicate NodeId must fail");
        assert!(matches!(
            err,
            ReplicationConfigError::PinnedDuplicate { node_id: 0xAA }
        ));
    }

    #[test]
    fn pinned_leader_outside_set_rejected() {
        let cfg = ReplicationConfig::new()
            .with_placement(PlacementStrategy::Pinned(vec![0xAA, 0xBB, 0xCC]))
            .with_leader_pinned(Some(0xDD));
        let err = cfg.validate().expect_err("leader outside set must fail");
        assert!(matches!(
            err,
            ReplicationConfigError::LeaderPinnedOutsidePinnedSet { leader: 0xDD }
        ));
    }

    #[test]
    fn pinned_leader_inside_set_validates() {
        let cfg = ReplicationConfig::new()
            .with_placement(PlacementStrategy::Pinned(vec![0xAA, 0xBB, 0xCC]))
            .with_leader_pinned(Some(0xBB));
        cfg.validate().expect("leader in set must validate");
    }

    #[test]
    fn pinned_leader_with_standard_placement_validates() {
        // `leader_pinned` with `PlacementStrategy::Standard` is the
        // explicit-publisher-elsewhere shape — leader runs on a
        // specific node while replicas spread via the standard
        // filter. Plan §10 calls this out as a supported topology.
        let cfg = ReplicationConfig::new().with_leader_pinned(Some(0x1234_5678));
        cfg.validate().expect("standard + leader_pinned must validate");
    }

    #[test]
    fn effective_factor_honors_pinned_length() {
        let cfg = ReplicationConfig::new()
            .with_factor(7) // ignored
            .with_placement(PlacementStrategy::Pinned(vec![1, 2, 3, 4]));
        assert_eq!(cfg.effective_factor(), 4);
    }

    #[test]
    fn effective_factor_falls_back_to_factor_for_non_pinned() {
        let cfg = ReplicationConfig::new()
            .with_factor(7)
            .with_placement(PlacementStrategy::Standard);
        assert_eq!(cfg.effective_factor(), 7);
        let cfg = ReplicationConfig::new()
            .with_factor(5)
            .with_placement(PlacementStrategy::ColocationStrict);
        assert_eq!(cfg.effective_factor(), 5);
    }

    #[test]
    fn factor_boundary_min_and_max_validate() {
        ReplicationConfig::new()
            .with_factor(REPLICATION_FACTOR_MIN)
            .validate()
            .expect("factor=min must validate");
        ReplicationConfig::new()
            .with_factor(REPLICATION_FACTOR_MAX)
            .validate()
            .expect("factor=max must validate");
    }

    #[test]
    fn heartbeat_at_min_validates() {
        ReplicationConfig::new()
            .with_heartbeat_ms(HEARTBEAT_MS_MIN)
            .validate()
            .expect("heartbeat=min must validate");
    }
}
