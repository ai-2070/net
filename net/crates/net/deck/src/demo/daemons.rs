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
