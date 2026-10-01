//! Replica-set selection for [`PlacementStrategy::Standard`] and
//! [`PlacementStrategy::ColocationStrict`] channels.
//!
//! `REDEX_DISTRIBUTED_PLAN.md` §4: replicas are the top N candidates by
//! `PlacementFilter::placement_score` for `Artifact::Replica`, re-selected
//! on roster change. `REDEX_REPLICA_PLACEMENT_PLAN.md` settles how nodes
//! agree on that set without coordinating: every node computes it from
//! data every node sees the same way.
//!
//! - **Candidates** are the nodes advertising the channel's
//!   `dataforts:replica-candidate:<id>` tag, which a node carries while
//!   it has the channel open.
//! - **Scoring** uses only axes whose inputs are announced data:
//!   colocation, intent (against the default registry, the same on every
//!   node) and storage resources. RTT proximity, leadership anti-affinity
//!   and node-local custom filters differ by viewer, so they don't choose
//!   replicas; election still ranks the chosen replicas by RTT.
//! - **Selection** is the top `factor` by score, ties to the lower NodeId.
//!
//! The same capability view therefore yields the same set on every node.
//!
//! [`PlacementStrategy::Standard`]: super::PlacementStrategy::Standard
//! [`PlacementStrategy::ColocationStrict`]: super::PlacementStrategy::ColocationStrict

use std::sync::Arc;

use crate::adapter::net::behavior::capability::CapabilitySet;
use crate::adapter::net::behavior::placement::{
    Artifact, ColocationPolicy, IntentMatchPolicy, IntentRegistry, NodeId, PlacementFilter,
    ResourceAxis, StandardPlacement,
};
use crate::adapter::net::channel::ChannelName;
use crate::adapter::net::MeshNode;

use super::replication::ChannelId;

/// Pick the replica set: candidates with a positive score, ranked by
/// score (highest first) then NodeId (lowest first), the first `factor`
/// of them, returned in ascending NodeId order so two resolutions compare
/// equal exactly when they name the same nodes.
///
/// `score` returning `None` or a non-positive / non-finite value
/// excludes the candidate (`StandardPlacement` reports a strict
/// colocation miss as `0.0`).
pub fn select_replica_set(
    candidates: impl IntoIterator<Item = NodeId>,
    factor: usize,
    score: impl Fn(NodeId) -> Option<f32>,
) -> Vec<NodeId> {
    let mut scored: Vec<(NodeId, f32)> = candidates
        .into_iter()
        .filter_map(|node| {
            score(node)
                .filter(|s| s.is_finite() && *s > 0.0)
                .map(|s| (node, s))
        })
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    scored.dedup_by_key(|(node, _)| *node);
    let mut set: Vec<NodeId> = scored.into_iter().take(factor).map(|(n, _)| n).collect();
    set.sort_unstable();
    set
}

/// Resolves a channel's replica set while its replication runtime runs.
/// Installed for `Standard` / `ColocationStrict` channels; `Pinned`
/// channels have a fixed set and no resolver.
#[async_trait::async_trait]
pub trait ReplicaSetResolver: Send + Sync {
    /// The replica set now, as [`select_replica_set`] returns it.
    fn resolve(&self) -> Vec<NodeId>;

    /// The replication factor the set is resolved to.
    fn factor(&self) -> usize;

    /// How long a short set must stay unchanged before this node joins
    /// it. A node that resolves fewer than `factor` replicas may simply
    /// not have heard the other candidates yet; joining at once would
    /// make it the leader of a set of one, beside another such leader
    /// elsewhere, and writes on both would diverge. A full set joins at
    /// once.
    fn settle_window(&self) -> std::time::Duration;

    /// Make sure this node advertises itself as a candidate. Called every
    /// tick, so it must be cheap when the advertisement is in place; it
    /// re-announces when a concurrent capability rewrite dropped it.
    async fn ensure_candidate(&self);

    /// Stop advertising this node as a candidate (the runtime is
    /// shutting down).
    async fn withdraw_candidate(&self);
}

/// The production resolver: candidates from the mesh's capability fold,
/// scored by [`StandardPlacement`] with the viewer-independent axes.
pub struct MeshReplicaPlacement {
    mesh: Arc<MeshNode>,
    /// This resolver's claim on the channel's candidate tag (see
    /// `MeshNode::claim_replica_candidate`).
    holder: u64,
    /// Set once the graceful `withdraw_candidate` released the claim, so
    /// `Drop` doesn't release it twice.
    released: std::sync::atomic::AtomicBool,
    channel: ChannelName,
    channel_id: ChannelId,
    factor: usize,
    strict: bool,
    /// The replica artifact's capabilities: `placement_metadata` as its
    /// metadata, which the colocation and intent axes read.
    artifact_caps: CapabilitySet,
}

impl MeshReplicaPlacement {
    /// A resolver for `channel` choosing `factor` replicas. `strict`
    /// selects `ColocationStrict` semantics (the colocation hint is a
    /// requirement, not a preference).
    pub fn new(
        mesh: Arc<MeshNode>,
        channel: ChannelName,
        factor: usize,
        strict: bool,
        placement_metadata: &std::collections::BTreeMap<String, String>,
    ) -> Self {
        let artifact_caps = CapabilitySet {
            metadata: placement_metadata.clone(),
            ..CapabilitySet::default()
        };
        let holder = NEXT_CANDIDATE_HOLDER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let channel_id = ChannelId::from_name(&channel);
        mesh.register_replica_candidate_holder(holder, channel_id.as_bytes());
        Self {
            mesh,
            holder,
            released: std::sync::atomic::AtomicBool::new(false),
            channel_id,
            channel,
            factor,
            strict,
            artifact_caps,
        }
    }
}

#[async_trait::async_trait]
impl ReplicaSetResolver for MeshReplicaPlacement {
    fn resolve(&self) -> Vec<NodeId> {
        let candidates = self
            .mesh
            .find_replica_candidates(self.channel_id.as_bytes());
        let fold = self.mesh.capability_fold();
        let placement = StandardPlacement::new(fold)
            .with_colocation_policy(if self.strict {
                ColocationPolicy::StrictRequired
            } else {
                ColocationPolicy::SoftPreference
            })
            .with_intent_match(IntentMatchPolicy::Strict)
            .with_intent_registry(IntentRegistry::defaults())
            .with_resource_axis(ResourceAxis::Storage);
        let artifact = Artifact::Replica {
            channel: &self.channel,
            capabilities: &self.artifact_caps,
        };
        select_replica_set(candidates, self.factor, |node| {
            placement.placement_score(&node, &artifact)
        })
    }

    fn factor(&self) -> usize {
        self.factor
    }

    /// Two announce windows: a candidate's tag can reach a peer up to one
    /// window late (the origin's trailing flush), plus one of slack.
    fn settle_window(&self) -> std::time::Duration {
        self.mesh.min_announce_interval().saturating_mul(2)
    }

    async fn ensure_candidate(&self) {
        if let Err(e) = self
            .mesh
            .claim_replica_candidate(self.holder, self.channel_id.as_bytes())
            .await
        {
            tracing::warn!(error = ?e, "replication: announcing replica candidacy failed");
        }
    }

    async fn withdraw_candidate(&self) {
        self.released
            .store(true, std::sync::atomic::Ordering::Release);
        if let Err(e) = self
            .mesh
            .release_replica_candidate(self.holder, self.channel_id.as_bytes())
            .await
        {
            tracing::warn!(error = ?e, "replication: withdrawing replica candidacy failed");
        }
    }
}

/// The graceful shutdown withdraws candidacy (`withdraw_candidate`), but a
/// runtime can also be aborted: shut down off a tokio runtime, dropped
/// with its `Redex` without `disable_replication`, or cancelled with a
/// full priority lane. The resolver lives in that task, so it is dropped
/// either way; dropping it takes the tag out of the baseline (every
/// later announce would otherwise re-send it, and peers would keep
/// choosing this node as a replica) and, on a runtime, re-announces.
impl Drop for MeshReplicaPlacement {
    fn drop(&mut self) {
        use crate::adapter::net::mesh::CandidateRelease;
        if self.released.load(std::sync::atomic::Ordering::Acquire) {
            return; // the graceful path released it
        }
        let id = *self.channel_id.as_bytes();
        let outcome = self.mesh.release_replica_candidate_sync(self.holder, &id);
        let rt = tokio::runtime::Handle::try_current();
        match (outcome, rt) {
            (CandidateRelease::StillClaimed | CandidateRelease::NotAdvertised, _) => {}
            // Out of the baseline; publish that.
            (CandidateRelease::Removed, Ok(rt)) => {
                let mesh = self.mesh.clone();
                rt.spawn(async move {
                    if let Err(e) = mesh.reannounce_current_capabilities().await {
                        tracing::warn!(error = ?e, "replication: re-announce after dropping candidacy failed");
                    }
                });
            }
            // Still in the baseline: withdraw it properly, unless a new
            // resolver claimed the channel meanwhile.
            (CandidateRelease::LockUnavailable, Ok(rt)) => {
                let mesh = self.mesh.clone();
                rt.spawn(async move {
                    if mesh.replica_candidate_unclaimed(&id) {
                        if let Err(e) = mesh.withdraw_replica_candidate(&id).await {
                            tracing::warn!(error = ?e, "replication: deferred candidacy withdraw failed");
                        }
                    }
                });
            }
            (CandidateRelease::Removed, Err(_)) => {
                // Removed from the baseline; peers learn at the next announce.
            }
            (CandidateRelease::LockUnavailable, Err(_)) => tracing::warn!(
                "replication: couldn't drop replica candidacy (announce lock busy, no runtime); \
                 it stays advertised until the channel is reopened or the node restarts"
            ),
        }
    }
}

/// Source of [`MeshReplicaPlacement`] holder ids.
static NEXT_CANDIDATE_HOLDER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_the_top_factor_by_score_then_node_id() {
        let scores = |n: NodeId| match n {
            1 => Some(0.5),
            2 => Some(0.9),
            3 => Some(0.9),
            4 => Some(0.7),
            _ => None,
        };
        // 2 and 3 tie at 0.9 (both in); 4 beats 1.
        assert_eq!(select_replica_set([1, 2, 3, 4], 3, scores), vec![2, 3, 4]);
        // A tie at the cut goes to the lower NodeId.
        assert_eq!(select_replica_set([3, 2, 1], 1, scores), vec![2]);
    }

    #[test]
    fn excludes_vetoed_zero_and_non_finite_scores() {
        let scores = |n: NodeId| match n {
            1 => None,
            2 => Some(0.0),
            3 => Some(f32::NAN),
            4 => Some(-1.0),
            5 => Some(0.1),
            _ => None,
        };
        assert_eq!(select_replica_set([1, 2, 3, 4, 5], 3, scores), vec![5]);
    }

    #[test]
    fn fewer_candidates_than_factor_takes_them_all_and_order_is_canonical() {
        let all = |_| Some(1.0);
        assert_eq!(select_replica_set([9, 4, 7], 5, all), vec![4, 7, 9]);
        // Input order and duplicates don't change the result.
        assert_eq!(select_replica_set([7, 9, 4, 9], 5, all), vec![4, 7, 9]);
        assert!(select_replica_set(Vec::<NodeId>::new(), 3, all).is_empty());
    }
}
