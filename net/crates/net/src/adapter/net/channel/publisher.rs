//! Thin fan-out helper: publish the same payload to every subscriber of a
//! channel by doing N per-peer sends.
//!
//! A `ChannelPublisher` is a *recipe* — `(channel, config)`. The work is
//! done by `MeshNode::publish` / `publish_many`, which iterate the
//! [`SubscriberRoster`](super::SubscriberRoster) and call `send_routed`
//! once per subscriber. There is no multicast packet primitive, no group
//! cryptography, and no implicit "send to everyone" — fan-out targets
//! only nodes that explicitly `subscribe_channel`'d.
//!
//! Per-publish behavior is controlled by `PublishConfig.on_failure`:
//!
//! * `BestEffort` — log and return `Ok(PublishReport)` as long as at
//!   least one subscriber received the payload (or the roster is empty).
//! * `FailFast` — return `Err` on the first per-peer error, without
//!   attempting the rest.
//! * `Collect` — never short-circuit; always return a full per-peer
//!   `PublishReport` so the caller can decide what "success" means.
//!
//! The design plan this was built from was removed in `a6beba551`; the
//! module docs above are the surviving statement of intent.

use super::name::{ChannelId, ChannelName};
use crate::adapter::net::stream::Reliability;
use crate::error::AdapterError;

/// What to do when one of the per-peer fan-out sends fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnFailure {
    /// Log per-peer errors and keep going; return Ok if at least one
    /// subscriber received the payload (or the roster was empty).
    #[default]
    BestEffort,
    /// Abort on the first per-peer error, without attempting the rest.
    FailFast,
    /// Never short-circuit; always return a full per-peer report.
    Collect,
}

/// Builder-style configuration for a [`ChannelPublisher`].
#[derive(Debug, Clone)]
pub struct PublishConfig {
    /// Per-peer reliability mode (applies to each unicast leg of the
    /// fan-out, not to the fan-out as a whole).
    pub reliability: Reliability,
    /// Failure policy.
    pub on_failure: OnFailure,
    /// Maximum concurrent per-peer sends. Defaults to 32. Larger rosters
    /// can bump this; smaller numbers are safer under socket contention.
    pub max_inflight: usize,
}

impl Default for PublishConfig {
    fn default() -> Self {
        Self {
            reliability: Reliability::FireAndForget,
            on_failure: OnFailure::BestEffort,
            max_inflight: 32,
        }
    }
}

impl PublishConfig {
    /// New builder-style config with defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set per-peer reliability.
    pub fn with_reliability(mut self, reliability: Reliability) -> Self {
        self.reliability = reliability;
        self
    }

    /// Set the failure policy.
    pub fn with_on_failure(mut self, on_failure: OnFailure) -> Self {
        self.on_failure = on_failure;
        self
    }

    /// Set the max concurrent per-peer sends. Clamped to `>= 1`.
    pub fn with_max_inflight(mut self, n: usize) -> Self {
        self.max_inflight = n.max(1);
        self
    }
}

/// Recipe for a fan-out to `channel`'s subscriber set. Pass to
/// `MeshNode::publish` / `publish_many`.
#[derive(Debug, Clone)]
pub struct ChannelPublisher {
    channel: ChannelId,
    config: PublishConfig,
}

impl ChannelPublisher {
    /// Build a publisher for `channel` with `config`.
    pub fn new(channel: ChannelName, config: PublishConfig) -> Self {
        Self {
            channel: ChannelId::new(channel),
            config,
        }
    }

    /// The channel being published to.
    pub fn channel(&self) -> &ChannelId {
        &self.channel
    }

    /// The publish config.
    pub fn config(&self) -> &PublishConfig {
        &self.config
    }
}

/// Outcome of one `publish` / `publish_many` call.
#[derive(Debug)]
pub struct PublishReport {
    /// Number of subscribers attempted.
    pub attempted: usize,
    /// Number of per-peer sends that succeeded.
    pub delivered: usize,
    /// Per-peer errors, keyed by `node_id`.
    pub errors: Vec<(u64, AdapterError)>,
}

impl PublishReport {
    /// True if every attempted peer saw the payload — including
    /// the vacuous case where the roster was empty (zero peers
    /// attempted, zero failures, every attempt succeeded).
    ///
    /// Pre-fix this required `attempted > 0`, so a
    /// publish with zero subscribers was simultaneously
    /// `is_empty() == true` AND `all_delivered() == false`.
    /// Callers branching on `all_delivered()` for the "everything
    /// went fine" path treated an empty roster as failure — a
    /// real-world hazard right after a `remove_peer` GC. Pair
    /// with [`Self::is_empty`] when the caller actually wants to
    /// distinguish "roster was empty" from "roster was full and
    /// delivered to all".
    pub fn all_delivered(&self) -> bool {
        self.delivered == self.attempted && self.errors.is_empty()
    }

    /// True if no subscribers were attempted (empty roster).
    pub fn is_empty(&self) -> bool {
        self.attempted == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults() {
        let c = PublishConfig::default();
        assert_eq!(c.reliability, Reliability::FireAndForget);
        assert_eq!(c.on_failure, OnFailure::BestEffort);
        assert_eq!(c.max_inflight, 32);
    }

    #[test]
    fn test_config_builder() {
        let c = PublishConfig::new()
            .with_reliability(Reliability::Reliable)
            .with_on_failure(OnFailure::FailFast)
            .with_max_inflight(8);
        assert_eq!(c.reliability, Reliability::Reliable);
        assert_eq!(c.on_failure, OnFailure::FailFast);
        assert_eq!(c.max_inflight, 8);
    }

    #[test]
    fn test_max_inflight_clamp() {
        let c = PublishConfig::new().with_max_inflight(0);
        assert_eq!(c.max_inflight, 1);
    }

    #[test]
    fn test_publisher_new() {
        let name = ChannelName::new("sensors/lidar").unwrap();
        let p = ChannelPublisher::new(name.clone(), PublishConfig::default());
        assert_eq!(p.channel().name().as_str(), "sensors/lidar");
        assert_eq!(p.config().reliability, Reliability::FireAndForget);
    }

    #[test]
    fn test_report_helpers() {
        // Empty roster is `all_delivered()` (vacuously
        // true). Pre-fix this returned false, which made callers
        // branching on `all_delivered()` for "all good" treat
        // empty rosters as failure — a real hazard after
        // `remove_peer` GC. Pair with `is_empty()` when the
        // caller cares about the distinction.
        let empty = PublishReport {
            attempted: 0,
            delivered: 0,
            errors: vec![],
        };
        assert!(empty.is_empty());
        assert!(
            empty.all_delivered(),
            "empty roster must be vacuously all_delivered"
        );

        let full = PublishReport {
            attempted: 3,
            delivered: 3,
            errors: vec![],
        };
        assert!(!full.is_empty());
        assert!(full.all_delivered());

        let partial = PublishReport {
            attempted: 3,
            delivered: 2,
            errors: vec![(42, AdapterError::Connection("boom".into()))],
        };
        assert!(!partial.all_delivered());
    }
}
