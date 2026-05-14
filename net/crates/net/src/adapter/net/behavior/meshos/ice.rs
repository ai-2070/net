//! ICE — break-glass operator surface, substrate side per
//! [`DECK_SDK_PLAN.md`](../../../../../../docs/plans/DECK_SDK_PLAN.md)
//! Phase 2.
//!
//! Locked decision #4 of the plan: blast-radius simulation is
//! mandatory before any ICE commit. This module ships the
//! substrate-side simulator the Deck SDK's `IceProposal::simulate`
//! binds against. Every ICE proposal the SDK exposes routes
//! through [`simulate`] before the operator commits.
//!
//! # Surface
//!
//! - [`IceActionProposal`] — the substrate-stable enum of ICE
//!   actions the simulator understands. Mirrors what the Deck
//!   SDK's `IceCommands` builder will produce.
//! - [`BlastRadius`] — pre-execution preview: which nodes /
//!   replicas / daemons the action would touch + warnings.
//!   Serializable so the SDK can hand it across the FFI
//!   boundary unchanged.
//! - [`BlastWarning`] — operator-readable hints about non-obvious
//!   consequences (cluster-wide pause, in-flight resumption,
//!   placement reshuffle, …).
//! - [`simulate`] — pure function: snapshot + proposal →
//!   blast radius. No I/O, no side effects.
//!
//! # Scope (this slice)
//!
//! Phase 2 lands here in stages. This slice ships:
//!
//! - `FreezeCluster { ttl }` — affects every peer in the snapshot
//!   for the configured TTL.
//! - `ThawCluster` — clears any in-effect freeze.
//!
//! Future slices add `ForceDrain`, `ForceEvictReplica`,
//! `ForceRestartDaemon`, `ForceCutover`, `KillMigration`,
//! `FlushAvoidLists` alongside the [`super::event::AdminEvent`]
//! variants they map to.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::event::{ChainId, DaemonRef, NodeId};
use super::snapshot::MeshOsSnapshot;

/// Substrate-stable enumeration of ICE proposals the simulator
/// understands. The Deck SDK's `IceCommands` builder produces
/// one of these; the substrate verifier accepts the same form
/// at commit time (Phase 3, behind multi-operator-signing).
///
/// `#[non_exhaustive]` so later slices extend the surface
/// without breaking implementors.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum IceActionProposal {
    /// Pause cluster-wide reconcile output for `ttl`. Maps to
    /// [`super::event::AdminEvent::FreezeCluster`].
    FreezeCluster {
        /// Requested freeze duration.
        ttl: Duration,
    },
    /// Cancel an in-effect freeze. Maps to
    /// [`super::event::AdminEvent::ThawCluster`].
    ThawCluster,
}

/// Pre-execution preview of an ICE action's effect. The Deck
/// SDK surfaces this from `IceProposal::simulate()`; Deck-the-
/// binary renders it as a confirmation prompt before commit.
///
/// Every field is `Serialize + Deserialize` so cross-language
/// bindings round-trip the wire form unchanged.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct BlastRadius {
    /// Nodes that would observe the action — every peer for
    /// cluster-wide actions like `FreezeCluster`, the targeted
    /// node for single-target actions.
    pub affected_nodes: Vec<NodeId>,
    /// Replica chains whose holder set would shift. Empty for
    /// actions that don't move replicas.
    pub affected_replicas: Vec<ChainId>,
    /// Daemons whose lifecycle would shift. Empty for actions
    /// that don't restart / stop daemons.
    pub affected_daemons: Vec<DaemonRef>,
    /// How long the operator should expect the action's
    /// downstream effects to take. For `FreezeCluster` this is
    /// the TTL itself; for drain-style actions this estimates
    /// the wait until the drain completes.
    pub estimated_drain_delay: Option<Duration>,
    /// Heuristic placement-churn estimate in `[0.0, 1.0]`.
    /// `0.0` = no placement disturbance; `1.0` = full
    /// re-distribution. Cluster-wide pause actions report
    /// `0.0` (no placement decisions execute during a freeze).
    pub placement_stability_delta: f32,
    /// Non-fatal hints about consequences the simulator can
    /// foresee but doesn't gate on.
    pub warnings: Vec<BlastWarning>,
}

/// Stable lowercase discriminator for [`BlastRadius`] warnings.
/// Cross-language SDKs match on the variant name; Deck-the-
/// binary renders them with operator-facing messages.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum BlastWarning {
    /// A freeze suppresses every reconcile-driven action — drains,
    /// rebalances, daemon restarts — until the TTL elapses or an
    /// explicit thaw fires.
    ClusterFreezeBlocksOperatorActions,
    /// Thawing a frozen cluster resumes whatever reconcile
    /// transitions were paused mid-flight.
    ThawResumesPendingReconciles,
    /// Thaw issued while no freeze is in effect — no-op.
    ThawHasNoFreezeToCancel,
}

/// Pure simulator: snapshot + proposal → blast radius. No I/O;
/// safe to call from any thread. The Deck SDK invokes this on
/// the runtime's latest snapshot when an operator clicks "preview"
/// on an ICE action.
pub fn simulate(snapshot: &MeshOsSnapshot, proposal: &IceActionProposal) -> BlastRadius {
    match proposal {
        IceActionProposal::FreezeCluster { ttl } => simulate_freeze(snapshot, *ttl),
        IceActionProposal::ThawCluster => simulate_thaw(snapshot),
    }
}

fn simulate_freeze(snapshot: &MeshOsSnapshot, ttl: Duration) -> BlastRadius {
    // Every peer the snapshot knows about would observe the
    // freeze. The set comes from the snapshot's peer keys; the
    // local node isn't a peer of itself, so for visibility we
    // include peers only — Deck-the-binary renders the local
    // node separately.
    let mut affected_nodes: Vec<NodeId> = snapshot.peers.keys().copied().collect();
    affected_nodes.sort();
    BlastRadius {
        affected_nodes,
        affected_replicas: Vec::new(),
        affected_daemons: Vec::new(),
        // The downstream effect of a freeze is "nothing happens
        // for `ttl`"; surface `ttl` here so the operator sees
        // the pause window in the preview UI.
        estimated_drain_delay: Some(ttl),
        placement_stability_delta: 0.0,
        warnings: vec![BlastWarning::ClusterFreezeBlocksOperatorActions],
    }
}

fn simulate_thaw(snapshot: &MeshOsSnapshot) -> BlastRadius {
    let warning = if snapshot.freeze_remaining_ms.is_some() {
        BlastWarning::ThawResumesPendingReconciles
    } else {
        BlastWarning::ThawHasNoFreezeToCancel
    };
    BlastRadius {
        affected_nodes: Vec::new(),
        affected_replicas: Vec::new(),
        affected_daemons: Vec::new(),
        estimated_drain_delay: None,
        placement_stability_delta: 0.0,
        warnings: vec![warning],
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::meshos::snapshot::PeerSnapshot;

    fn snapshot_with_peers(peers: &[NodeId]) -> MeshOsSnapshot {
        let mut snap = MeshOsSnapshot::default();
        for peer in peers {
            snap.peers.insert(*peer, PeerSnapshot::default());
        }
        snap
    }

    #[test]
    fn freeze_against_empty_snapshot_reports_no_affected_nodes() {
        let snap = MeshOsSnapshot::default();
        let blast = simulate(
            &snap,
            &IceActionProposal::FreezeCluster {
                ttl: Duration::from_secs(30),
            },
        );
        assert!(blast.affected_nodes.is_empty());
        assert_eq!(blast.estimated_drain_delay, Some(Duration::from_secs(30)));
        assert_eq!(
            blast.warnings,
            vec![BlastWarning::ClusterFreezeBlocksOperatorActions]
        );
    }

    #[test]
    fn freeze_against_three_peers_reports_all_three_sorted() {
        let snap = snapshot_with_peers(&[30, 10, 20]);
        let blast = simulate(
            &snap,
            &IceActionProposal::FreezeCluster {
                ttl: Duration::from_secs(60),
            },
        );
        assert_eq!(blast.affected_nodes, vec![10, 20, 30]);
        assert_eq!(blast.estimated_drain_delay, Some(Duration::from_secs(60)));
        // Cluster-wide pause; no placement decisions execute
        // during the window.
        assert_eq!(blast.placement_stability_delta, 0.0);
        // No daemons / replicas are touched directly — the freeze
        // gates the reconcile output, not the underlying state.
        assert!(blast.affected_replicas.is_empty());
        assert!(blast.affected_daemons.is_empty());
    }

    #[test]
    fn thaw_against_frozen_snapshot_warns_pending_reconciles_resume() {
        let mut snap = MeshOsSnapshot::default();
        snap.freeze_remaining_ms = Some(15_000);
        let blast = simulate(&snap, &IceActionProposal::ThawCluster);
        assert_eq!(
            blast.warnings,
            vec![BlastWarning::ThawResumesPendingReconciles]
        );
        assert!(blast.affected_nodes.is_empty());
        assert_eq!(blast.estimated_drain_delay, None);
    }

    #[test]
    fn thaw_against_unfrozen_snapshot_warns_no_op() {
        let snap = MeshOsSnapshot::default();
        let blast = simulate(&snap, &IceActionProposal::ThawCluster);
        assert_eq!(blast.warnings, vec![BlastWarning::ThawHasNoFreezeToCancel]);
    }

    #[test]
    fn blast_radius_postcard_round_trip_preserves_every_field() {
        // Wire-stability pin: the SDK and bindings deserialize
        // this exact shape. Round-trip every field so a future
        // refactor can't silently change the form.
        let blast = BlastRadius {
            affected_nodes: vec![1, 2, 3],
            affected_replicas: vec![100, 200],
            affected_daemons: vec![DaemonRef {
                id: 7,
                name: "telemetry".into(),
            }],
            estimated_drain_delay: Some(Duration::from_secs(45)),
            placement_stability_delta: 0.25,
            warnings: vec![
                BlastWarning::ClusterFreezeBlocksOperatorActions,
                BlastWarning::ThawResumesPendingReconciles,
            ],
        };
        let bytes = postcard::to_allocvec(&blast).expect("encode");
        let decoded: BlastRadius = postcard::from_bytes(&bytes).expect("decode");
        assert_eq!(decoded, blast);
    }

    #[test]
    fn blast_radius_json_round_trip_preserves_every_field() {
        let blast = BlastRadius {
            affected_nodes: vec![42],
            affected_replicas: Vec::new(),
            affected_daemons: Vec::new(),
            estimated_drain_delay: Some(Duration::from_millis(2_500)),
            placement_stability_delta: 0.0,
            warnings: vec![BlastWarning::ClusterFreezeBlocksOperatorActions],
        };
        let json = serde_json::to_string(&blast).expect("encode");
        let decoded: BlastRadius = serde_json::from_str(&json).expect("decode");
        assert_eq!(decoded, blast);
    }

    #[test]
    fn ice_action_proposal_postcard_round_trips_both_variants() {
        for proposal in [
            IceActionProposal::FreezeCluster {
                ttl: Duration::from_secs(90),
            },
            IceActionProposal::ThawCluster,
        ] {
            let bytes = postcard::to_allocvec(&proposal).expect("encode");
            let decoded: IceActionProposal = postcard::from_bytes(&bytes).expect("decode");
            assert_eq!(decoded, proposal);
        }
    }
}
