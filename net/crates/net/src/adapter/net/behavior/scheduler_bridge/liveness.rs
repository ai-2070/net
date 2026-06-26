//! Projection 4 — observed node liveness → fold-update delta.
//!
//! MeshOS already folds per-peer health into `MeshOsState::node_health`
//! (a heartbeat-derived `NodeHealth`, written by the health probe and
//! the `NodeHealthUpdate` fold). This projection reads that snapshot and
//! classifies each node as down or up, so the gang scheduler's candidate
//! set can be pruned to reality: matching must never offer an island on
//! a dead node.
//!
//! The projection is **node-level and pure** — it returns a
//! [`LivenessDelta`] naming the down / up nodes. Translating that into
//! fold mutations is the wiring's job and is deliberately left out here
//! (plan LD 5 keeps this pure; the appliers land with the runtime):
//!   - **down** node → drop its islands from the `IslandTopology`
//!     candidate set, and mark its `CapabilityFold` entries
//!     liveness-suspended (plan RD 5 — a per-entry flag, *not* a delete,
//!     preserving the fold's CRDT-grade AP semantics);
//!   - **up** node → re-admit its islands and lift capability suspension
//!     so the next heartbeat refreshes the entry.
//!
//! The node→island / node→capability translation lives in the appliers
//! (they hold the topology + capability folds); the projection only
//! needs `MeshOsState`, exactly as the plan's `project_liveness`
//! signature specifies.
//!
//! Classification: `Unreachable` is **down**. `Degraded` (slow but
//! responsive) and `Healthy` are **up** — a degraded node is still a
//! valid, if slower, candidate, and its measured latency already
//! reflects the slowness; dropping it would needlessly shrink the
//! candidate set.

use crate::adapter::net::behavior::meshos::snapshot::{MeshOsSnapshot, PeerHealthSnapshot};
use crate::adapter::net::behavior::meshos::state::MeshOsState;
use crate::adapter::net::behavior::meshos::{NodeHealth, NodeId};

/// Node-level liveness classification derived from `MeshOsState`. The
/// wiring applies `down` (drop islands + suspend capability entries) and
/// `up` (re-admit + allow refresh) idempotently each tick. Both lists
/// are sorted for a stable, replay-deterministic order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LivenessDelta {
    /// Nodes currently unreachable — their islands must leave the
    /// candidate set and their capability entries be suspended.
    pub down: Vec<NodeId>,
    /// Nodes currently reachable (healthy or degraded) — (re)admitted.
    pub up: Vec<NodeId>,
}

/// Project observed node liveness into a [`LivenessDelta`] (plan
/// Projection 4). Pure: reads only `MeshOsState::node_health`, returns a
/// value, performs no I/O and mutates no fold.
pub fn project_liveness(meshos: &MeshOsState) -> LivenessDelta {
    let mut delta = LivenessDelta::default();
    for (&node, &health) in &meshos.node_health {
        // Exhaustive on purpose: `NodeHealth` is `#[non_exhaustive]`,
        // but in-crate that is a no-op, so a future variant breaks this
        // match and forces a deliberate liveness-classification choice
        // rather than silently falling into `up` or `down`.
        match health {
            NodeHealth::Healthy | NodeHealth::Degraded => delta.up.push(node),
            NodeHealth::Unreachable => delta.down.push(node),
        }
    }
    delta.down.sort_unstable();
    delta.up.sort_unstable();
    delta
}

/// Like [`project_liveness`] but reading the public `MeshOsSnapshot`
/// (`MeshOsRuntime::snapshot()`) instead of the loop-internal
/// `MeshOsState` — the form an *external* driver consumes (the raw
/// `MeshOsState` is internal to the reconcile loop). Classifies each peer
/// by its snapshot `health`: `Unreachable` → down; `Healthy` / `Degraded`
/// → up; no health sample yet → unclassified (left out of both lists, so
/// an unconfirmed node is never pruned from matching). Same output shape
/// and sorting as [`project_liveness`].
pub fn project_liveness_from_snapshot(snapshot: &MeshOsSnapshot) -> LivenessDelta {
    let mut delta = LivenessDelta::default();
    for (&node, peer) in &snapshot.peers {
        match peer.health {
            Some(PeerHealthSnapshot::Unreachable) => delta.down.push(node),
            Some(PeerHealthSnapshot::Healthy) | Some(PeerHealthSnapshot::Degraded) => {
                delta.up.push(node)
            }
            // No health sample yet → don't classify; an unconfirmed node
            // is never dropped from matching.
            None => {}
        }
    }
    delta.down.sort_unstable();
    delta.up.sort_unstable();
    delta
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unreachable_is_down_healthy_and_degraded_are_up() {
        let mut meshos = MeshOsState::default();
        meshos.node_health.insert(1, NodeHealth::Healthy);
        meshos.node_health.insert(2, NodeHealth::Unreachable);
        meshos.node_health.insert(3, NodeHealth::Degraded);
        meshos.node_health.insert(4, NodeHealth::Unreachable);

        let delta = project_liveness(&meshos);
        assert_eq!(delta.down, vec![2, 4], "unreachable → down (sorted)");
        assert_eq!(delta.up, vec![1, 3], "healthy + degraded → up (sorted)");
    }

    #[test]
    fn degraded_stays_a_candidate_not_dropped() {
        // A slow-but-responsive node must remain a match candidate;
        // only fully unreachable nodes leave the set.
        let mut meshos = MeshOsState::default();
        meshos.node_health.insert(7, NodeHealth::Degraded);
        let delta = project_liveness(&meshos);
        assert_eq!(delta.up, vec![7]);
        assert!(delta.down.is_empty());
    }

    #[test]
    fn empty_health_yields_empty_delta() {
        let delta = project_liveness(&MeshOsState::default());
        assert_eq!(delta, LivenessDelta::default());
    }

    #[test]
    fn snapshot_variant_classifies_peers_by_snapshot_health() {
        use crate::adapter::net::behavior::meshos::snapshot::PeerSnapshot;

        let mut snap = MeshOsSnapshot::default();
        snap.peers.insert(
            1,
            PeerSnapshot {
                health: Some(PeerHealthSnapshot::Healthy),
                ..Default::default()
            },
        );
        snap.peers.insert(
            2,
            PeerSnapshot {
                health: Some(PeerHealthSnapshot::Unreachable),
                ..Default::default()
            },
        );
        snap.peers.insert(
            3,
            PeerSnapshot {
                health: Some(PeerHealthSnapshot::Degraded),
                ..Default::default()
            },
        );
        // No health sample yet → unclassified.
        snap.peers.insert(4, PeerSnapshot::default());

        let delta = project_liveness_from_snapshot(&snap);
        assert_eq!(delta.down, vec![2], "only Unreachable is down");
        assert_eq!(delta.up, vec![1, 3], "Healthy + Degraded are up");
        assert!(
            !delta.down.contains(&4) && !delta.up.contains(&4),
            "a peer with no health sample is never classified (never pruned)",
        );
    }
}
