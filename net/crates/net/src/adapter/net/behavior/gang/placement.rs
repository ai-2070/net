//! `ColocationStrict` island placement (plan §5) — pin each island's
//! reservation-chain replica set inside one fault domain.
//!
//! Consequence (the reason §5 is ranked *before* the §6 quorum): a
//! cross-DC partition then leaves an island's replica set wholly on
//! one side. The far side can't see the island to claim it, and the
//! §6 quorum on the near side is LAN-local — sub-millisecond, so the
//! one CP edge stays cheap. Placement is the cheap mitigation that
//! makes the quorum guarantee affordable.
//!
//! This module ties the island's RedEX replica placement to the §6
//! quorum membership: the *same* pinned node set is both where the
//! reservation chain replicates ([`ReplicationConfig`]) and who votes
//! on the `→ Active` commit ([`ReplicaSet`]). Building them from one
//! source is what guarantees the quorum is LAN-local by construction
//! rather than by hope.

use crate::adapter::net::behavior::fold::NodeId;
use crate::adapter::net::redex::{PlacementStrategy, ReplicationConfig, ReplicationConfigError};

use super::quorum::ReplicaSet;

/// Replication-config metadata key an island's reservation chain sets
/// so [`PlacementStrategy::ColocationStrict`] pins its replicas onto
/// the nodes already holding the island host's own chain — i.e. the
/// GPU host's fault domain.
pub const COLOCATE_WITH_STRICT_KEY: &str = "colocate-with-strict";

/// Build the replication config for an island's reservation chain
/// using [`PlacementStrategy::ColocationStrict`] (plan §5): replicas
/// follow the island host's chain into one fault domain. The runtime
/// resolves the concrete replica nodes from the colocation metadata,
/// so this variant does not itself yield a [`ReplicaSet`] — use
/// [`pinned_island_replicas`] when the concrete fault-domain node set
/// is known up-front (the common case, and what the §6 quorum needs).
pub fn colocated_island_config(factor: u8) -> ReplicationConfig {
    ReplicationConfig::new()
        .with_factor(factor)
        .with_placement(PlacementStrategy::ColocationStrict)
}

/// Build an explicitly-pinned island reservation config **and** the
/// matching [`ReplicaSet`] the §6 quorum votes over, from one
/// fault-domain-local node set (the island host + its rack/DC peers).
///
/// Returning both from a single input is the §5↔§6 contract: the
/// nodes that replicate the reservation chain are exactly the nodes
/// that witness the `→ Active` commit, so the quorum can never span a
/// WAN. The config is `validate()`-checked (non-empty, within the
/// replication-factor bounds, no duplicates); the `ReplicaSet`
/// normalizes the same set for quorum counting.
pub fn pinned_island_replicas(
    replicas: impl IntoIterator<Item = NodeId>,
) -> Result<(ReplicationConfig, ReplicaSet), ReplicationConfigError> {
    // Normalize once so the pinned config and the replica set agree
    // exactly (deduped, ordered).
    let mut nodes: Vec<NodeId> = replicas.into_iter().collect();
    nodes.sort_unstable();
    nodes.dedup();

    let config = ReplicationConfig::new().with_placement(PlacementStrategy::Pinned(nodes.clone()));
    config.validate()?;
    Ok((config, ReplicaSet::new(nodes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colocated_config_uses_colocation_strict() {
        let cfg = colocated_island_config(3);
        assert_eq!(cfg.placement, PlacementStrategy::ColocationStrict);
        assert_eq!(cfg.factor, 3);
        cfg.validate().expect("colocation-strict config is valid");
    }

    #[test]
    fn pinned_replicas_yield_matching_config_and_quorum_set() {
        let (cfg, set) = pinned_island_replicas([5, 1, 3]).expect("valid pinned set");
        // The pinned config carries exactly the (normalized) node set,
        // and its effective factor is the set length.
        assert_eq!(
            cfg.placement,
            PlacementStrategy::Pinned(vec![1, 3, 5]),
            "pinned config carries the normalized fault-domain set",
        );
        assert_eq!(cfg.effective_factor(), 3);
        // The quorum set is the SAME nodes — LAN-local by construction.
        assert_eq!(set.members(), &[1, 3, 5]);
        assert_eq!(set.quorum_threshold(), 2);
    }

    #[test]
    fn duplicate_replicas_are_normalized_not_rejected() {
        // Dedup happens before the pinned config is built, so a caller
        // passing the host twice still gets a valid 2-node set.
        let (cfg, set) = pinned_island_replicas([7, 7, 9]).expect("dedups to a valid set");
        assert_eq!(cfg.effective_factor(), 2);
        assert_eq!(set.members(), &[7, 9]);
    }

    #[test]
    fn empty_replica_set_is_rejected() {
        // An island with no fault-domain replicas can't form a quorum;
        // the pinned-config validator rejects the empty set.
        assert!(pinned_island_replicas([]).is_err());
    }
}
