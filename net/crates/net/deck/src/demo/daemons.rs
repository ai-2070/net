//! `MeshDaemon` implementations the demo cluster registers
//! across its nodes. Each one is intentionally tiny — the demo
//! is not exercising daemon logic, it's exercising the
//! substrate's observation surfaces (snapshot fold, log fan-
//! out, chain machinery, migration orchestrator). The minimum
//! daemon shape is enough.

use bytes::Bytes;
use net_sdk::capabilities::CapabilityFilter;
use net_sdk::compute::CausalEvent;
use net_sdk::meshos::{DaemonError, MeshDaemon};

// The demo's narrative arc is an AI inference fleet:
// 1 NodeAgent per node, 3 InferenceWorkers as a replica
// trio, 3 RolloutForge daemons as a fork group (A/B model
// variants), 3 TrainerCanary daemons as a standby triad
// (1 active + 2 warm).
//
// The name suffixes (`#replica` / `#fork@<seq>` / `#standby`)
// are what the deck's `lineage::group_daemons` parser keys
// on, so changing the base names doesn't break the GROUPS /
// CHAINS rendering — it just relabels the rows with vocab a
// non-engineer viewer recognizes.

/// Per-node monitoring agent. Stateless; the periodic log
/// lines are emitted by a side task that holds the
/// `MeshOsDaemonHandle`. Renamed from `heartbeat` to land in
/// the AI-inference narrative; the name still has no group
/// suffix so the daemon registers as `Solo`.
pub struct NodeAgentDaemon;

impl MeshDaemon for NodeAgentDaemon {
    fn name(&self) -> &str {
        "node_agent"
    }
    fn requirements(&self) -> CapabilityFilter {
        CapabilityFilter::default()
    }
    fn process(&mut self, _event: &CausalEvent) -> Result<Vec<Bytes>, DaemonError> {
        Ok(Vec::new())
    }
}

/// Replica-group flavor daemon — three interchangeable
/// inference workers serving the same model. The deck's
/// `lineage::group_daemons` parser keys on the `#replica`
/// suffix to classify them as a `ReplicaGroup`.
pub struct InferenceWorkerDaemon;

impl MeshDaemon for InferenceWorkerDaemon {
    fn name(&self) -> &str {
        "inference_worker#replica"
    }
    fn requirements(&self) -> CapabilityFilter {
        CapabilityFilter::default()
    }
    fn process(&mut self, _event: &CausalEvent) -> Result<Vec<Bytes>, DaemonError> {
        Ok(Vec::new())
    }
}

/// Fork-group flavor daemon — A/B model rollout variants
/// forked from a shared base at `fork_seq=7`. The deck
/// displays the fork lineage's parent-seq badge alongside
/// the group name.
pub struct RolloutForgeDaemon;

impl MeshDaemon for RolloutForgeDaemon {
    fn name(&self) -> &str {
        "rollout_forge#fork@7"
    }
    fn requirements(&self) -> CapabilityFilter {
        CapabilityFilter::default()
    }
    fn process(&mut self, _event: &CausalEvent) -> Result<Vec<Bytes>, DaemonError> {
        Ok(Vec::new())
    }
}

/// Standby-group flavor daemon — one active trainer canary
/// plus two warm standbys. The deck assigns "active" to the
/// lowest-`daemon_id` member; the rest render as warm.
pub struct TrainerCanaryDaemon;

impl MeshDaemon for TrainerCanaryDaemon {
    fn name(&self) -> &str {
        "trainer_canary#standby"
    }
    fn requirements(&self) -> CapabilityFilter {
        CapabilityFilter::default()
    }
    fn process(&mut self, _event: &CausalEvent) -> Result<Vec<Bytes>, DaemonError> {
        Ok(Vec::new())
    }
}
