//! Bridge probes — read peer state off a real `Mesh` and surface
//! it through the `LocalityProbe` / `HealthProbe` /
//! `InventoryProbe` traits the `MeshOsRuntime`'s event loop polls
//! each tick. The "closes the two-layer split" piece per
//! `DECK_DEMO_HARNESS_PLAN.md` Phase 0.5.
//!
//! v1 scope: each probe iterates a harness-provided expected peer
//! set and reports per-peer state derived from the `Mesh`. RTT is
//! the loopback floor (sub-ms) so the snapshot fold has *some*
//! non-zero RTT to render; a follow-up wires real RTT through the
//! proximity graph once a public accessor lands. Capabilities are
//! empty in v1 — the deck demo's Phase 1 daemons re-publish their
//! own capability sets via the daemon SDK, so the inventory
//! probe's job here is mostly to surface peer presence + reachable
//! / unreachable state to the fold.

use std::sync::Arc;
use std::time::Duration;

use crate::mesh::Mesh;
use crate::meshos::{
    HealthProbe, InventoryProbe, LocalityProbe, MeshOsRuntime, NodeHealth, NodeId, PeerInventory,
};

/// Reads peer reachability off a `Mesh` and reports a loopback-
/// floor RTT for every still-reachable peer in the expected set.
/// Cheap each tick — one `peer_addr` lookup per expected peer.
pub struct MeshLocalityProbe {
    mesh: Arc<Mesh>,
    expected_peers: Arc<Vec<NodeId>>,
    /// Reported RTT for every reachable peer. Loopback in v1; a
    /// real proximity-graph read replaces this once the substrate
    /// exposes one.
    loopback_rtt: Duration,
}

impl MeshLocalityProbe {
    pub fn new(mesh: Arc<Mesh>, expected_peers: Arc<Vec<NodeId>>) -> Self {
        Self {
            mesh,
            expected_peers,
            loopback_rtt: Duration::from_micros(500),
        }
    }
}

impl LocalityProbe for MeshLocalityProbe {
    fn rtt_samples(&self) -> Vec<(NodeId, Duration)> {
        let local = self.mesh.node_id();
        self.expected_peers
            .iter()
            .copied()
            .filter(|id| *id != local)
            .filter(|id| peer_reachable(&self.mesh, *id))
            .map(|id| (id, self.loopback_rtt))
            .collect()
    }
}

/// Reads peer reachability off a `Mesh` and reports `Healthy` for
/// every peer with a live session, `Unreachable` for every
/// expected peer without one. The runtime's fold merges this with
/// other signals; for the v1 harness it's the only health source.
pub struct MeshHealthProbe {
    mesh: Arc<Mesh>,
    expected_peers: Arc<Vec<NodeId>>,
}

impl MeshHealthProbe {
    pub fn new(mesh: Arc<Mesh>, expected_peers: Arc<Vec<NodeId>>) -> Self {
        Self {
            mesh,
            expected_peers,
        }
    }
}

impl HealthProbe for MeshHealthProbe {
    fn health_samples(&self) -> Vec<(NodeId, NodeHealth)> {
        let local = self.mesh.node_id();
        self.expected_peers
            .iter()
            .copied()
            .filter(|id| *id != local)
            .map(|id| {
                let h = if peer_reachable(&self.mesh, id) {
                    NodeHealth::Healthy
                } else {
                    NodeHealth::Unreachable
                };
                (id, h)
            })
            .collect()
    }
}

/// Reports a default `PeerInventory` for every expected peer with
/// a live session. Capability sets stay empty in v1 — daemons
/// publish their own via the daemon SDK, and a per-peer
/// capability mirror in the bridge probe would duplicate that
/// path. CPU / mem / disk / saturation are `None` for the same
/// reason (the substrate has no Mesh-side telemetry surface for
/// those yet).
pub struct MeshInventoryProbe {
    mesh: Arc<Mesh>,
    expected_peers: Arc<Vec<NodeId>>,
}

impl MeshInventoryProbe {
    pub fn new(mesh: Arc<Mesh>, expected_peers: Arc<Vec<NodeId>>) -> Self {
        Self {
            mesh,
            expected_peers,
        }
    }
}

impl InventoryProbe for MeshInventoryProbe {
    fn inventory_samples(&self) -> Vec<(NodeId, PeerInventory)> {
        let local = self.mesh.node_id();
        self.expected_peers
            .iter()
            .copied()
            .filter(|id| *id != local)
            .filter(|id| peer_reachable(&self.mesh, *id))
            .map(|id| (id, PeerInventory::default()))
            .collect()
    }
}

/// Install all three bridge probes on a runtime in one call. The
/// harness calls this once per node after the Mesh pairs
/// handshake; the runtime's tick loop picks the probes up on its
/// next scan.
pub fn install_mesh_probes(
    runtime: &MeshOsRuntime,
    mesh: Arc<Mesh>,
    expected_peers: Arc<Vec<NodeId>>,
) {
    runtime.add_locality_probe(Arc::new(MeshLocalityProbe::new(
        Arc::clone(&mesh),
        Arc::clone(&expected_peers),
    )));
    runtime.add_health_probe(Arc::new(MeshHealthProbe::new(
        Arc::clone(&mesh),
        Arc::clone(&expected_peers),
    )));
    runtime.add_inventory_probe(Arc::new(MeshInventoryProbe::new(mesh, expected_peers)));
}

/// True iff the `Mesh` currently holds a session for `peer`. The
/// SDK's public surface doesn't expose a "is this peer connected"
/// boolean directly; we lean on `MeshNode::peer_addr` (returns
/// `Some(addr)` only when a live session exists for `peer`).
fn peer_reachable(mesh: &Mesh, peer: NodeId) -> bool {
    mesh.inner().peer_addr(peer).is_some()
}
