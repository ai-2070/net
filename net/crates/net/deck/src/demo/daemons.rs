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

/// Per-node "I'm alive" daemon. Stateless, no inbound
/// processing — the periodic log lines are emitted by a
/// side task that holds the `MeshOsDaemonHandle`; see
/// `spawn::install_heartbeat_loggers`.
///
/// The name is deliberately stable across all 5 nodes so the
/// DAEMONS tab's "by kind" grouping (when the deck adds one)
/// can render them in a single row. The per-node disambiguation
/// comes from the daemon's keypair-derived `origin_hash`, not
/// its `name()`.
pub struct HeartbeatDaemon;

impl MeshDaemon for HeartbeatDaemon {
    fn name(&self) -> &str {
        "heartbeat"
    }
    fn requirements(&self) -> CapabilityFilter {
        CapabilityFilter::default()
    }
    fn process(&mut self, _event: &CausalEvent) -> Result<Vec<Bytes>, DaemonError> {
        Ok(Vec::new())
    }
}

/// Replica-group flavor daemon. The `#replica` suffix is what
/// the deck's `lineage::group_daemons` parser looks for to
/// classify the three instances as a `ReplicaGroup`. Stateless.
pub struct MixerDaemon;

impl MeshDaemon for MixerDaemon {
    fn name(&self) -> &str {
        "audio_mixer#replica"
    }
    fn requirements(&self) -> CapabilityFilter {
        CapabilityFilter::default()
    }
    fn process(&mut self, _event: &CausalEvent) -> Result<Vec<Bytes>, DaemonError> {
        Ok(Vec::new())
    }
}

/// Fork-group flavor daemon. The `#fork@<seq>` suffix carries
/// the parent fork sequence so the deck displays the fork
/// lineage's parent-seq badge. Stateless.
pub struct DroneDaemon;

impl MeshDaemon for DroneDaemon {
    fn name(&self) -> &str {
        "drone_swarm#fork@7"
    }
    fn requirements(&self) -> CapabilityFilter {
        CapabilityFilter::default()
    }
    fn process(&mut self, _event: &CausalEvent) -> Result<Vec<Bytes>, DaemonError> {
        Ok(Vec::new())
    }
}

/// Standby-group flavor daemon. The `#standby` suffix groups
/// the three instances into a `StandbyGroup`. The deck
/// assigns "active" to the lowest-`daemon_id` member; the
/// rest render as "warm" standbys.
pub struct PyroSafetyDaemon;

impl MeshDaemon for PyroSafetyDaemon {
    fn name(&self) -> &str {
        "pyro_safety#standby"
    }
    fn requirements(&self) -> CapabilityFilter {
        CapabilityFilter::default()
    }
    fn process(&mut self, _event: &CausalEvent) -> Result<Vec<Bytes>, DaemonError> {
        Ok(Vec::new())
    }
}
