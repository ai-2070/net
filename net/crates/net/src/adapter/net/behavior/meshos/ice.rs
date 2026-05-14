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
    /// Force-evict `victim` from `chain` bypassing the
    /// scheduler's rebalance cooldown. Maps to
    /// [`super::event::AdminEvent::ForceEvictReplica`].
    ForceEvictReplica {
        /// Chain whose replica to evict.
        chain: ChainId,
        /// Holder to remove.
        victim: NodeId,
    },
    /// Reset `daemon`'s backoff so the supervisor's gate
    /// no longer suppresses restart. Maps to
    /// [`super::event::AdminEvent::ForceRestartDaemon`].
    ForceRestartDaemon {
        /// The daemon whose backoff to clear.
        daemon: DaemonRef,
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
            IceActionProposal::ForceEvictReplica { chain, victim } => {
                AdminEvent::ForceEvictReplica {
                    chain: *chain,
                    victim: *victim,
                }
            }
            IceActionProposal::ForceRestartDaemon { daemon } => AdminEvent::ForceRestartDaemon {
                daemon: daemon.clone(),
            },
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

/// Default cap on the per-node ICE audit ring. Records older
/// than this drop FIFO so the substrate's audit buffer stays
/// fixed-overhead under churn. Sized for "a few minutes of
/// operator activity" rather than "complete history" — the
/// canonical replay path is the eventual ICE audit subchain;
/// this ring is the in-memory snapshot-side surface the Deck
/// SDK reads against until the subchain ships.
pub const DEFAULT_MAX_ICE_AUDIT_RECORDS: usize = 256;

/// Outcome the substrate verifier recorded for an attempted
/// ICE commit. Both accepted and rejected attempts surface on
/// the audit ring so security review can replay every
/// break-glass attempt — successful or otherwise.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum VerificationOutcome {
    /// The verifier accepted the bundle; the inner `AdminEvent`
    /// folded normally.
    Accepted,
    /// The verifier rejected the bundle. `kind` carries the
    /// stable [`VerifyError::kind`] discriminator; `message`
    /// is the operator-readable detail.
    Rejected {
        /// Discriminator the cross-language SDKs branch on
        /// (`not_authorized`, `signature_invalid`,
        /// `insufficient_signatures`).
        kind: String,
        /// Operator-readable detail.
        message: String,
    },
    /// No verifier was installed when the commit arrived; the
    /// inner event folded without verification. Only seen in
    /// in-process tests and dev configs — production loops
    /// install a verifier.
    Unverified,
}

/// One entry on the substrate's ICE audit ring. The
/// [`super::event_loop::MeshOsLoop`] records one of these per
/// [`super::event::MeshOsEvent::SignedIceCommit`] regardless of
/// whether the verifier accepted or rejected the bundle.
///
/// Carries the operator ids (not the full 64-byte signature
/// bytes) because the audit consumer doesn't need the
/// cryptographic material — just "who claimed authorship of
/// this commit."
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IceAuditRecord {
    /// Wall-clock milliseconds since `UNIX_EPOCH` at the
    /// moment the loop received the commit. Distinct from
    /// `Instant`-based timing the rest of the loop uses so
    /// audit consumers don't need a reference instant.
    pub committed_at_ms: u64,
    /// The proposal payload the signature bundle covered.
    pub proposal: IceActionProposal,
    /// Issuing operator ids from the bundle's signatures. The
    /// signatures themselves aren't retained — the verifier
    /// already accepted or rejected them, and the audit ring
    /// is a footprint surface, not a re-verification surface.
    pub operator_ids: Vec<u64>,
    /// The verifier's outcome for this attempt.
    pub outcome: VerificationOutcome,
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
    /// `ForceEvictReplica` bypasses the scheduler's rebalance
    /// cooldown for the targeted chain. Operator gets one
    /// eviction now; the cooldown still applies to subsequent
    /// scheduler-driven rebalances of the same chain.
    ForcedEvictionBypassesCooldown {
        /// Chain the force-evict targets.
        chain: ChainId,
        /// Holder being evicted.
        victim: NodeId,
    },
    /// `ForceEvictReplica` referenced a chain that doesn't
    /// appear in the snapshot — substrate accepts the admin
    /// commit, but no node will fire a leader-side eviction
    /// because the leader entry is missing. Surfaces here so
    /// the operator UI flags "this proposal will be a no-op."
    ForcedEvictionTargetsUnknownChain {
        /// The chain id the operator targeted.
        chain: ChainId,
    },
    /// `ForceEvictReplica`'s victim isn't currently observed
    /// as a holder of the chain. The commit still folds and
    /// produces a leader-side eviction action, but the holder
    /// set won't change — the action becomes a no-op at the
    /// dispatcher.
    ForcedEvictionTargetsNonHolder {
        /// The chain id the operator targeted.
        chain: ChainId,
        /// The holder the operator targeted.
        victim: NodeId,
    },
    /// `ForceRestartDaemon` bypasses the supervisor's
    /// `BackingOff` / `CrashLooping` gate so the daemon gets
    /// an immediate retry. Surface so the operator confirms
    /// the underlying cause has been addressed before bouncing
    /// the daemon back into the same crash loop.
    ForcedRestartBypassesBackoff {
        /// The targeted daemon's id.
        daemon_id: u64,
    },
    /// `ForceRestartDaemon` referenced a daemon not currently
    /// observed in the snapshot. The fold still removes any
    /// stale `applied_backoffs` entry, but reconcile won't
    /// emit `StartDaemon` because there's no `DaemonStatus`
    /// entry to track. Operator likely typed the wrong id.
    ForcedRestartTargetsUnknownDaemon {
        /// The daemon id the operator targeted.
        daemon_id: u64,
    },
    /// `ForceRestartDaemon` targeted a daemon whose tracker
    /// is already `Idle`. The commit is a no-op — the operator
    /// might be confused about the daemon's actual state.
    ForcedRestartDaemonNotInBackoff {
        /// The targeted daemon's id.
        daemon_id: u64,
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
        IceActionProposal::ForceEvictReplica { chain, victim } => {
            simulate_force_evict_replica(snapshot, *chain, *victim)
        }
        IceActionProposal::ForceRestartDaemon { daemon } => {
            simulate_force_restart_daemon(snapshot, daemon)
        }
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

fn simulate_force_evict_replica(
    snapshot: &MeshOsSnapshot,
    chain: ChainId,
    victim: NodeId,
) -> BlastRadius {
    let mut warnings = vec![BlastWarning::ForcedEvictionBypassesCooldown { chain, victim }];
    let replica = snapshot.replicas.get(&chain);
    if replica.is_none() {
        warnings.push(BlastWarning::ForcedEvictionTargetsUnknownChain { chain });
    } else if let Some(snap) = replica {
        if !snap.holders.contains(&victim) {
            warnings.push(BlastWarning::ForcedEvictionTargetsNonHolder { chain, victim });
        }
    }
    BlastRadius {
        affected_nodes: vec![victim],
        affected_replicas: vec![chain],
        affected_daemons: Vec::new(),
        estimated_drain_delay: None,
        // Eviction always disturbs placement; surface a small
        // but visible signal so the operator UI flags the
        // change.
        placement_stability_delta: 0.15,
        warnings,
    }
}

fn simulate_force_restart_daemon(
    snapshot: &MeshOsSnapshot,
    daemon: &DaemonRef,
) -> BlastRadius {
    let mut warnings = vec![BlastWarning::ForcedRestartBypassesBackoff {
        daemon_id: daemon.id,
    }];
    match snapshot.daemons.get(&daemon.id) {
        None => warnings.push(BlastWarning::ForcedRestartTargetsUnknownDaemon {
            daemon_id: daemon.id,
        }),
        Some(snap) => {
            if matches!(
                snap.restart_state,
                super::snapshot::RestartStateSnapshot::Idle
            ) {
                warnings.push(BlastWarning::ForcedRestartDaemonNotInBackoff {
                    daemon_id: daemon.id,
                });
            }
        }
    }
    BlastRadius {
        affected_nodes: Vec::new(),
        affected_replicas: Vec::new(),
        affected_daemons: vec![daemon.clone()],
        estimated_drain_delay: None,
        placement_stability_delta: 0.0,
        warnings,
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
    fn simulate_force_evict_replica_reports_chain_and_victim() {
        let mut snap = MeshOsSnapshot::default();
        // Seed the chain so the simulator can verify victim is
        // a current holder.
        snap.replicas.insert(
            100,
            super::super::snapshot::ReplicaSnapshot {
                holders: vec![7, 8, 9],
                desired_count: Some(3),
                leader: Some(8),
            },
        );
        let blast = simulate(
            &snap,
            &IceActionProposal::ForceEvictReplica {
                chain: 100,
                victim: 7,
            },
        );
        assert_eq!(blast.affected_replicas, vec![100]);
        assert_eq!(blast.affected_nodes, vec![7]);
        assert!(blast
            .warnings
            .iter()
            .any(|w| matches!(
                w,
                BlastWarning::ForcedEvictionBypassesCooldown {
                    chain: 100,
                    victim: 7
                }
            )));
        // Non-zero placement disturbance.
        assert!(blast.placement_stability_delta > 0.0);
    }

    #[test]
    fn simulate_force_evict_replica_warns_on_unknown_chain() {
        let snap = MeshOsSnapshot::default();
        let blast = simulate(
            &snap,
            &IceActionProposal::ForceEvictReplica {
                chain: 100,
                victim: 7,
            },
        );
        assert!(blast.warnings.iter().any(|w| matches!(
            w,
            BlastWarning::ForcedEvictionTargetsUnknownChain { chain: 100 }
        )));
    }

    #[test]
    fn simulate_force_evict_replica_warns_on_non_holder_victim() {
        let mut snap = MeshOsSnapshot::default();
        snap.replicas.insert(
            100,
            super::super::snapshot::ReplicaSnapshot {
                holders: vec![1, 2, 3],
                desired_count: Some(3),
                leader: Some(1),
            },
        );
        let blast = simulate(
            &snap,
            &IceActionProposal::ForceEvictReplica {
                chain: 100,
                victim: 999,
            },
        );
        assert!(blast.warnings.iter().any(|w| matches!(
            w,
            BlastWarning::ForcedEvictionTargetsNonHolder {
                chain: 100,
                victim: 999
            }
        )));
    }

    #[test]
    fn simulate_force_restart_daemon_targets_only_the_daemon() {
        use super::super::snapshot::{
            DaemonLifecycleSnapshot, DaemonSnapshot, RestartStateSnapshot,
        };
        let mut snap = MeshOsSnapshot::default();
        snap.daemons.insert(
            7,
            DaemonSnapshot {
                name: "telemetry".into(),
                lifecycle: DaemonLifecycleSnapshot::Stopped,
                health: None,
                saturation: 0.0,
                restart_state: RestartStateSnapshot::BackingOff { until_ms: 5_000 },
            },
        );
        let daemon = DaemonRef {
            id: 7,
            name: "telemetry".into(),
        };
        let blast = simulate(
            &snap,
            &IceActionProposal::ForceRestartDaemon {
                daemon: daemon.clone(),
            },
        );
        assert_eq!(blast.affected_daemons, vec![daemon]);
        assert!(blast.affected_nodes.is_empty());
        assert_eq!(blast.placement_stability_delta, 0.0);
        assert!(blast
            .warnings
            .iter()
            .any(|w| matches!(
                w,
                BlastWarning::ForcedRestartBypassesBackoff { daemon_id: 7 }
            )));
        // No "unknown" or "not in backoff" warnings — the daemon
        // is observed AND in BackingOff.
        assert!(blast
            .warnings
            .iter()
            .all(|w| !matches!(
                w,
                BlastWarning::ForcedRestartTargetsUnknownDaemon { .. }
                    | BlastWarning::ForcedRestartDaemonNotInBackoff { .. }
            )));
    }

    #[test]
    fn simulate_force_restart_daemon_warns_on_unknown_daemon() {
        let snap = MeshOsSnapshot::default();
        let daemon = DaemonRef {
            id: 99,
            name: "absent".into(),
        };
        let blast = simulate(
            &snap,
            &IceActionProposal::ForceRestartDaemon { daemon },
        );
        assert!(blast.warnings.iter().any(|w| matches!(
            w,
            BlastWarning::ForcedRestartTargetsUnknownDaemon { daemon_id: 99 }
        )));
    }

    #[test]
    fn simulate_force_restart_daemon_warns_when_already_idle() {
        use super::super::snapshot::{
            DaemonLifecycleSnapshot, DaemonSnapshot, RestartStateSnapshot,
        };
        let mut snap = MeshOsSnapshot::default();
        snap.daemons.insert(
            7,
            DaemonSnapshot {
                name: "telemetry".into(),
                lifecycle: DaemonLifecycleSnapshot::Running,
                health: None,
                saturation: 0.0,
                restart_state: RestartStateSnapshot::Idle,
            },
        );
        let blast = simulate(
            &snap,
            &IceActionProposal::ForceRestartDaemon {
                daemon: DaemonRef {
                    id: 7,
                    name: "telemetry".into(),
                },
            },
        );
        assert!(blast.warnings.iter().any(|w| matches!(
            w,
            BlastWarning::ForcedRestartDaemonNotInBackoff { daemon_id: 7 }
        )));
    }

    #[test]
    fn ice_proposal_to_admin_event_maps_force_restart_daemon() {
        let daemon = DaemonRef {
            id: 7,
            name: "telemetry".into(),
        };
        let proposal = IceActionProposal::ForceRestartDaemon {
            daemon: daemon.clone(),
        };
        match proposal.to_admin_event() {
            AdminEvent::ForceRestartDaemon { daemon: out } => assert_eq!(out, daemon),
            other => panic!("expected ForceRestartDaemon, got {other:?}"),
        }
    }

    #[test]
    fn ice_proposal_to_admin_event_maps_force_evict_replica() {
        let proposal = IceActionProposal::ForceEvictReplica {
            chain: 100,
            victim: 7,
        };
        match proposal.to_admin_event() {
            AdminEvent::ForceEvictReplica { chain, victim } => {
                assert_eq!(chain, 100);
                assert_eq!(victim, 7);
            }
            other => panic!("expected ForceEvictReplica, got {other:?}"),
        }
    }

    #[test]
    fn ice_audit_record_postcard_round_trips_each_outcome() {
        for outcome in [
            VerificationOutcome::Accepted,
            VerificationOutcome::Rejected {
                kind: "signature_invalid".into(),
                message: "bad sig".into(),
            },
            VerificationOutcome::Unverified,
        ] {
            let record = IceAuditRecord {
                committed_at_ms: 12_345,
                proposal: IceActionProposal::FreezeCluster {
                    ttl: Duration::from_secs(60),
                },
                operator_ids: vec![1, 2, 3],
                outcome: outcome.clone(),
            };
            let bytes = postcard::to_allocvec(&record).expect("encode");
            let decoded: IceAuditRecord = postcard::from_bytes(&bytes).expect("decode");
            assert_eq!(decoded, record);
        }
    }

    #[test]
    fn ice_audit_record_json_round_trips_for_audit_query_path() {
        let record = IceAuditRecord {
            committed_at_ms: 999,
            proposal: IceActionProposal::ThawCluster,
            operator_ids: vec![42],
            outcome: VerificationOutcome::Accepted,
        };
        let json = serde_json::to_string(&record).expect("encode");
        let decoded: IceAuditRecord = serde_json::from_str(&json).expect("decode");
        assert_eq!(decoded, record);
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
