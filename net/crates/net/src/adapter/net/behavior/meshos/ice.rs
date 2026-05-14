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

use super::event::{AdminEvent, AvoidScope, ChainId, DaemonRef, NodeId};
use super::snapshot::MeshOsSnapshot;
use crate::adapter::net::identity::{EntityId, EntityKeypair};

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
    /// Flush avoid-list entries cluster-wide under the given
    /// [`AvoidScope`]. Maps to
    /// [`super::event::AdminEvent::FlushAvoidLists`].
    FlushAvoidLists {
        /// Flush scope — see [`AvoidScope`].
        scope: AvoidScope,
    },
}

impl IceActionProposal {
    /// Translate the proposal to its corresponding
    /// [`AdminEvent`]. The substrate folds the [`AdminEvent`];
    /// the proposal is the SDK-side builder + signing form.
    pub fn to_admin_event(&self) -> AdminEvent {
        match self {
            IceActionProposal::FreezeCluster { ttl } => AdminEvent::FreezeCluster { ttl: *ttl },
            IceActionProposal::ThawCluster => AdminEvent::ThawCluster,
            IceActionProposal::FlushAvoidLists { scope } => {
                AdminEvent::FlushAvoidLists { scope: *scope }
            }
        }
    }
}

/// Operator signature over an [`IceActionProposal`]. Carries
/// the issuing operator's id plus a 64-byte ed25519 signature
/// over [`ice_proposal_signing_payload`]'s deterministic
/// postcard encoding. The substrate verifier
/// ([`OperatorRegistry::verify`]) re-checks the bundle on the
/// loop side of every [`super::event::MeshOsEvent::SignedIceCommit`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OperatorSignature {
    /// Issuing operator's id (Deck SDK's
    /// `OperatorIdentity::operator_id`).
    pub operator_id: u64,
    /// 64-byte ed25519 signature over
    /// [`ice_proposal_signing_payload`].
    pub signature: Vec<u8>,
}

/// Deterministic encoding the signing + verifying paths both
/// use. Pinned via the [`IceActionProposal`] postcard form so
/// every binding signs over the same bytes.
pub fn ice_proposal_signing_payload(proposal: &IceActionProposal) -> Vec<u8> {
    postcard::to_allocvec(proposal).expect("postcard encoding of IceActionProposal is infallible")
}

impl OperatorSignature {
    /// Sign `proposal` with `keypair`'s ed25519 secret. The
    /// 64-byte signature covers
    /// [`ice_proposal_signing_payload`], so two operators
    /// signing the same proposal produce bit-identical inputs
    /// the verifier can re-check.
    ///
    /// Panics on a public-only keypair — callers that may hold
    /// one should guard with `EntityKeypair::is_read_only`.
    pub fn sign(keypair: &EntityKeypair, proposal: &IceActionProposal) -> Self {
        let payload = ice_proposal_signing_payload(proposal);
        let sig = keypair.sign(&payload);
        Self {
            operator_id: keypair.origin_hash(),
            signature: sig.to_bytes().to_vec(),
        }
    }
}

/// Operator-key registry. Maps each operator id to the public
/// key the substrate uses to validate that operator's
/// signatures. Shared between the SDK-side gate (Deck SDK's
/// `IceProposal::commit`) and the substrate-side loop verifier
/// (`MeshOsLoop::with_admin_verifier`) so a malicious SDK can't
/// bypass verification.
#[derive(Clone, Debug, Default)]
pub struct OperatorRegistry {
    keys: std::collections::BTreeMap<u64, EntityId>,
}

impl OperatorRegistry {
    /// Empty registry. Every verify against this registry
    /// returns `not_authorized` until at least one operator is
    /// inserted.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an operator's public key. Subsequent
    /// [`Self::verify`] calls for `operator_id` resolve against
    /// this entry.
    pub fn insert(&mut self, operator_id: u64, public_key: EntityId) {
        self.keys.insert(operator_id, public_key);
    }

    /// Convenience: register `keypair`'s public key under its
    /// derived operator id (the keypair's `origin_hash`).
    pub fn register(&mut self, keypair: &EntityKeypair) {
        self.insert(keypair.origin_hash(), keypair.entity_id().clone());
    }

    /// `true` iff `operator_id` is registered.
    pub fn contains(&self, operator_id: u64) -> bool {
        self.keys.contains_key(&operator_id)
    }

    /// Number of registered operators.
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// `true` iff no operators are registered.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Verify `signature` against the registered public key for
    /// `signature.operator_id` over `payload`. Returns
    /// [`VerifyError::NotAuthorized`] for an unknown operator,
    /// [`VerifyError::InvalidSignature`] for a malformed or
    /// tampered signature.
    pub fn verify(
        &self,
        signature: &OperatorSignature,
        payload: &[u8],
    ) -> Result<(), VerifyError> {
        let entity_id = self
            .keys
            .get(&signature.operator_id)
            .ok_or(VerifyError::NotAuthorized {
                operator_id: signature.operator_id,
            })?;
        let sig_bytes: &[u8; 64] = signature.signature.as_slice().try_into().map_err(|_| {
            VerifyError::InvalidSignature {
                operator_id: signature.operator_id,
                reason: format!("signature is not 64 bytes (got {})", signature.signature.len()),
            }
        })?;
        let ed_sig = ed25519_dalek::Signature::from_bytes(sig_bytes);
        entity_id
            .verify(payload, &ed_sig)
            .map_err(|_| VerifyError::InvalidSignature {
                operator_id: signature.operator_id,
                reason: "signature failed verification against the registered public key".into(),
            })
    }

    /// Verify every signature in `signatures` against `payload`
    /// and confirm there are at least `threshold` valid bundles.
    /// Fails fast on the first verification error.
    pub fn verify_bundle(
        &self,
        signatures: &[OperatorSignature],
        payload: &[u8],
        threshold: usize,
    ) -> Result<(), VerifyError> {
        if signatures.len() < threshold {
            return Err(VerifyError::InsufficientSignatures {
                got: signatures.len(),
                required: threshold,
            });
        }
        for sig in signatures {
            self.verify(sig, payload)?;
        }
        Ok(())
    }
}

/// Substrate-side ICE verification error. The Deck SDK maps each
/// variant to its `<<deck-sdk-kind:KIND>>MSG` envelope.
#[derive(Clone, Debug, thiserror::Error)]
pub enum VerifyError {
    /// The operator id on the signature isn't registered with
    /// the cluster's operator policy.
    #[error("operator {operator_id} is not registered in the cluster's operator policy")]
    NotAuthorized {
        /// Issuing operator id from the rejected signature.
        operator_id: u64,
    },
    /// The signature is malformed, tampered, or signed a
    /// different payload than the one verified.
    #[error("operator {operator_id} signature invalid: {reason}")]
    InvalidSignature {
        /// Issuing operator id from the rejected signature.
        operator_id: u64,
        /// Diagnostic detail.
        reason: String,
    },
    /// The bundle carried fewer signatures than the cluster's
    /// configured threshold.
    #[error("insufficient signatures: got {got}, required {required}")]
    InsufficientSignatures {
        /// Number of signatures supplied.
        got: usize,
        /// Minimum required by the cluster's policy.
        required: usize,
    },
}

impl VerifyError {
    /// Stable lowercase kind discriminator the Deck SDK +
    /// cross-language consumers branch on.
    pub fn kind(&self) -> &'static str {
        match self {
            VerifyError::NotAuthorized { .. } => "not_authorized",
            VerifyError::InvalidSignature { .. } => "signature_invalid",
            VerifyError::InsufficientSignatures { .. } => "insufficient_signatures",
        }
    }
}

/// Substrate-side ICE admin verifier — bundles a shared
/// [`OperatorRegistry`] with the cluster's signature threshold.
/// Installed on [`super::event_loop::MeshOsLoop`] via
/// `with_admin_verifier`; the loop runs every
/// [`super::event::MeshOsEvent::SignedIceCommit`] through
/// [`Self::verify_commit`] before folding the inner
/// [`AdminEvent`].
#[derive(Clone, Debug)]
pub struct AdminVerifier {
    registry: std::sync::Arc<OperatorRegistry>,
    threshold: usize,
}

impl AdminVerifier {
    /// Build a verifier with `threshold` minimum signatures.
    /// `threshold = 0` is clamped to `1` — no admin path should
    /// ever accept an empty signature bundle.
    pub fn new(registry: std::sync::Arc<OperatorRegistry>, threshold: usize) -> Self {
        Self {
            registry,
            threshold: threshold.max(1),
        }
    }

    /// Borrow the underlying registry.
    pub fn registry(&self) -> &OperatorRegistry {
        &self.registry
    }

    /// Configured minimum-signature threshold.
    pub fn threshold(&self) -> usize {
        self.threshold
    }

    /// Verify a `SignedIceCommit`-style bundle against
    /// `proposal`. Computes the signing payload internally so
    /// the loop never needs to recompute it on the hot path.
    pub fn verify_commit(
        &self,
        proposal: &IceActionProposal,
        signatures: &[OperatorSignature],
    ) -> Result<(), VerifyError> {
        let payload = ice_proposal_signing_payload(proposal);
        self.registry
            .verify_bundle(signatures, &payload, self.threshold)
    }
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
    /// `FlushAvoidLists::Global` is the heaviest scope —
    /// reconcile will re-emit `MarkAvoid` for any peer that
    /// still meets the degraded-RTT threshold on the next
    /// tick, so the operator should not expect lasting effect
    /// without addressing the underlying RTT cause first.
    GlobalAvoidFlushMayReEmit,
    /// `FlushAvoidLists::Local` targets the operator's own
    /// node only; other nodes ignore the event. Surfaces so
    /// the operator confirms the scope choice matches intent.
    AvoidFlushLocalToTargetNodeOnly,
    /// `FlushAvoidLists::OnPeer` un-avoids the targeted peer
    /// cluster-wide. Carries the peer id so the operator UI
    /// can render "every node will stop avoiding peer X."
    AvoidFlushRecoversPeer {
        /// The peer the flush is un-avoiding cluster-wide.
        peer: NodeId,
    },
}

/// Pure simulator: snapshot + proposal → blast radius. No I/O;
/// safe to call from any thread. The Deck SDK invokes this on
/// the runtime's latest snapshot when an operator clicks "preview"
/// on an ICE action.
pub fn simulate(snapshot: &MeshOsSnapshot, proposal: &IceActionProposal) -> BlastRadius {
    match proposal {
        IceActionProposal::FreezeCluster { ttl } => simulate_freeze(snapshot, *ttl),
        IceActionProposal::ThawCluster => simulate_thaw(snapshot),
        IceActionProposal::FlushAvoidLists { scope } => simulate_flush_avoid_lists(snapshot, *scope),
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

fn simulate_flush_avoid_lists(snapshot: &MeshOsSnapshot, scope: AvoidScope) -> BlastRadius {
    let mut affected_nodes: Vec<NodeId> = snapshot.peers.keys().copied().collect();
    affected_nodes.sort();
    match scope {
        AvoidScope::Local { node } => BlastRadius {
            // Only the targeted node folds the event; other
            // nodes see the chain entry but skip the fold.
            affected_nodes: vec![node],
            affected_replicas: Vec::new(),
            affected_daemons: Vec::new(),
            estimated_drain_delay: None,
            placement_stability_delta: 0.0,
            warnings: vec![BlastWarning::AvoidFlushLocalToTargetNodeOnly],
        },
        AvoidScope::OnPeer { peer } => BlastRadius {
            // Every peer in the cluster folds the event (each
            // removes `peer` from its own avoid list).
            affected_nodes,
            affected_replicas: Vec::new(),
            affected_daemons: Vec::new(),
            estimated_drain_delay: None,
            // Un-avoiding a peer changes which nodes reconcile
            // will consider for placement. Surface as a small
            // non-zero churn estimate without committing to an
            // exact value.
            placement_stability_delta: 0.05,
            warnings: vec![BlastWarning::AvoidFlushRecoversPeer { peer }],
        },
        AvoidScope::Global => BlastRadius {
            affected_nodes,
            affected_replicas: Vec::new(),
            affected_daemons: Vec::new(),
            estimated_drain_delay: None,
            placement_stability_delta: 0.1,
            warnings: vec![BlastWarning::GlobalAvoidFlushMayReEmit],
        },
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

    #[test]
    fn operator_signature_signs_and_verifies_round_trip() {
        let kp = EntityKeypair::generate();
        let mut registry = OperatorRegistry::new();
        registry.register(&kp);
        let proposal = IceActionProposal::FreezeCluster {
            ttl: Duration::from_secs(30),
        };
        let sig = OperatorSignature::sign(&kp, &proposal);
        let payload = ice_proposal_signing_payload(&proposal);
        registry.verify(&sig, &payload).expect("valid signature");
    }

    #[test]
    fn operator_registry_rejects_unknown_operator_via_substrate_path() {
        let kp = EntityKeypair::generate();
        let registry = OperatorRegistry::new(); // empty
        let proposal = IceActionProposal::ThawCluster;
        let sig = OperatorSignature::sign(&kp, &proposal);
        let payload = ice_proposal_signing_payload(&proposal);
        let err = registry.verify(&sig, &payload).unwrap_err();
        assert!(matches!(err, VerifyError::NotAuthorized { .. }));
        assert_eq!(err.kind(), "not_authorized");
    }

    #[test]
    fn admin_verifier_clamps_zero_threshold_to_one() {
        let registry = std::sync::Arc::new(OperatorRegistry::new());
        let verifier = AdminVerifier::new(registry, 0);
        assert_eq!(verifier.threshold(), 1);
    }

    #[test]
    fn admin_verifier_returns_insufficient_signatures_for_empty_bundle() {
        let kp = EntityKeypair::generate();
        let mut registry = OperatorRegistry::new();
        registry.register(&kp);
        let verifier = AdminVerifier::new(std::sync::Arc::new(registry), 2);
        let proposal = IceActionProposal::ThawCluster;
        let sig = OperatorSignature::sign(&kp, &proposal);
        let err = verifier.verify_commit(&proposal, &[sig]).unwrap_err();
        assert!(matches!(
            err,
            VerifyError::InsufficientSignatures { got: 1, required: 2 }
        ));
    }

    #[test]
    fn ice_proposal_to_admin_event_maps_freeze_cluster() {
        let proposal = IceActionProposal::FreezeCluster {
            ttl: Duration::from_secs(45),
        };
        assert!(matches!(
            proposal.to_admin_event(),
            AdminEvent::FreezeCluster { ttl } if ttl == Duration::from_secs(45)
        ));
    }

    #[test]
    fn ice_proposal_to_admin_event_maps_thaw_cluster() {
        assert!(matches!(
            IceActionProposal::ThawCluster.to_admin_event(),
            AdminEvent::ThawCluster
        ));
    }

    #[test]
    fn simulate_flush_avoid_lists_local_targets_one_node() {
        let snap = snapshot_with_peers(&[10, 20, 30]);
        let blast = simulate(
            &snap,
            &IceActionProposal::FlushAvoidLists {
                scope: AvoidScope::Local { node: 42 },
            },
        );
        assert_eq!(blast.affected_nodes, vec![42]);
        assert!(blast
            .warnings
            .iter()
            .any(|w| matches!(w, BlastWarning::AvoidFlushLocalToTargetNodeOnly)));
    }

    #[test]
    fn simulate_flush_avoid_lists_on_peer_covers_every_peer_with_warning() {
        let snap = snapshot_with_peers(&[10, 20, 30]);
        let blast = simulate(
            &snap,
            &IceActionProposal::FlushAvoidLists {
                scope: AvoidScope::OnPeer { peer: 20 },
            },
        );
        assert_eq!(blast.affected_nodes, vec![10, 20, 30]);
        assert!(blast
            .warnings
            .iter()
            .any(|w| matches!(w, BlastWarning::AvoidFlushRecoversPeer { peer: 20 })));
        // Small but non-zero churn signal.
        assert!(blast.placement_stability_delta > 0.0);
    }

    #[test]
    fn simulate_flush_avoid_lists_global_carries_re_emit_warning() {
        let snap = snapshot_with_peers(&[1, 2, 3]);
        let blast = simulate(
            &snap,
            &IceActionProposal::FlushAvoidLists {
                scope: AvoidScope::Global,
            },
        );
        assert_eq!(blast.affected_nodes, vec![1, 2, 3]);
        assert!(blast
            .warnings
            .iter()
            .any(|w| matches!(w, BlastWarning::GlobalAvoidFlushMayReEmit)));
    }

    #[test]
    fn ice_proposal_to_admin_event_maps_flush_avoid_lists() {
        for scope in [
            AvoidScope::Local { node: 42 },
            AvoidScope::OnPeer { peer: 7 },
            AvoidScope::Global,
        ] {
            let proposal = IceActionProposal::FlushAvoidLists { scope };
            match proposal.to_admin_event() {
                AdminEvent::FlushAvoidLists { scope: out } => assert_eq!(out, scope),
                other => panic!("expected FlushAvoidLists, got {other:?}"),
            }
        }
    }

    #[test]
    fn flush_avoid_lists_proposal_postcard_round_trips_for_every_scope() {
        for scope in [
            AvoidScope::Local { node: 42 },
            AvoidScope::OnPeer { peer: 7 },
            AvoidScope::Global,
        ] {
            let proposal = IceActionProposal::FlushAvoidLists { scope };
            let bytes = postcard::to_allocvec(&proposal).expect("encode");
            let decoded: IceActionProposal = postcard::from_bytes(&bytes).expect("decode");
            assert_eq!(decoded, proposal);
        }
    }
}
