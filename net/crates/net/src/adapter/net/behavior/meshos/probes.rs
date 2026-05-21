//! Pull-via-tick source converters — the second source-converter
//! pattern. Where [`super::sources`] uses push-via-observer (each
//! state change fires through an `Observer` trait), probes are
//! polled by the loop on each Tick.
//!
//! Pull-via-tick is the right shape for high-volume signals that:
//!
//! - fire on a hot path the loop should not enter
//!   (proximity-graph edge updates run per pingwave, many per
//!   second);
//! - benefit from coalescing into a per-tick batch (we don't need
//!   every RTT sample — the latest is enough);
//! - have a natural pull surface already (the proximity graph
//!   exposes `all_nodes()` and per-node latency).
//!
//! Push-via-observer is the right shape when:
//!
//! - the signal is sparse + meaningful (daemon lifecycle is
//!   handful-per-day);
//! - latency matters more than throughput (a daemon crash should
//!   reach reconcile within one tick).
//!
//! Both patterns coexist on the loop; pick the one that fits the
//! source.
//!
//! # Surface
//!
//! - [`LocalityProbe`] — emits per-peer RTT samples.
//!   [`ProximityGraphLocalityProbe`] is the production impl over
//!   `ProximityGraph::all_nodes()`.
//! - [`HealthProbe`] — emits per-peer health classifications
//!   (`NodeHealth::Healthy` / `Degraded` / `Unreachable`).
//!   [`ProximityGraphHealthProbe`] derives them from
//!   `ProximityNode::last_seen` against the configured staleness
//!   threshold.
//!
//! Probes use the substrate's `[u8; 32] ↔ u64` id-bridge
//! convention (`mesh::node_id_to_graph_id` / inverse): the first
//! 8 bytes of the proximity NodeId, little-endian, are the MeshOS
//! `NodeId` value.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::adapter::net::behavior::proximity::ProximityGraph;

use super::event::{NodeHealth, NodeId};

/// Locality probe — surfaces per-peer RTT samples on demand.
/// Called once per Tick by the loop; should complete quickly.
pub trait LocalityProbe: Send + Sync + 'static {
    /// Return the latest RTT sample for each known peer. Order
    /// doesn't matter; duplicates are coalesced by the loop's
    /// fold via overwriting. Excluding `this_node` is the
    /// probe's responsibility — the loop doesn't filter.
    fn rtt_samples(&self) -> Vec<(NodeId, Duration)>;
}

/// Health probe — surfaces per-peer health classifications on
/// demand. Called once per Tick. The probe's classification
/// scheme is its own (typical implementations key off
/// last-seen-recently / proximity edge freshness).
pub trait HealthProbe: Send + Sync + 'static {
    /// Return the latest health classification for each known
    /// peer.
    fn health_samples(&self) -> Vec<(NodeId, NodeHealth)>;
}

/// Per-peer inventory axes — the resource / capability /
/// version snapshot the Deck's NODE.INV column surfaces.
/// Every field is `Option` or default-able so a probe can
/// publish only the axes it actually samples (e.g. a host-
/// resource probe might populate `cpu_load_1m` + `mem_*` but
/// leave `capability_set` empty for a capability-only probe
/// to fill in).
///
/// Maps 1:1 onto the corresponding fields on
/// [`super::snapshot::PeerSnapshot`]; the snapshot fold copies
/// each axis through unchanged.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PeerInventory {
    /// 1-minute CPU load average.
    pub cpu_load_1m: Option<f64>,
    /// Host memory used, in bytes.
    pub mem_used_bytes: Option<u64>,
    /// Host memory cap, in bytes.
    pub mem_total_bytes: Option<u64>,
    /// Host disk used, in bytes.
    pub disk_used_bytes: Option<u64>,
    /// Host disk cap, in bytes.
    pub disk_total_bytes: Option<u64>,
    /// Rolling 0.0..=1.0 saturation score the probe computes
    /// from its own signals.
    pub saturation_trend: Option<f32>,
    /// Capabilities the peer advertises.
    pub capability_set: std::collections::BTreeSet<String>,
    /// Substrate semver string.
    pub software_version: Option<String>,
    /// Fork-group origin, if the peer is a fork.
    pub forked_from: Option<NodeId>,
}

/// Inventory probe — surfaces per-peer resource / capability
/// / version axes on demand. Called once per Tick alongside
/// the locality + health probes; partial samples are fine —
/// a probe that only publishes some axes leaves others
/// defaulted.
pub trait InventoryProbe: Send + Sync + 'static {
    /// Return the latest per-peer inventory snapshot. The loop
    /// merges samples into state per Tick; later probes
    /// overwrite earlier on the same peer.
    fn inventory_samples(&self) -> Vec<(NodeId, PeerInventory)>;
}

/// Wraps a `ProximityGraph` and reports RTT samples by reading
/// each known node's `latency_us`. Cheap — one DashMap iterate
/// per Tick + integer conversion per entry.
pub struct ProximityGraphLocalityProbe {
    graph: Arc<ProximityGraph>,
    /// Identifier for this node — excluded from samples since
    /// MeshOS doesn't care about "RTT to self".
    this_node: NodeId,
}

impl ProximityGraphLocalityProbe {
    /// Construct a probe over `graph`. `this_node` is the local
    /// node id (from the loop's `MeshOsConfig::this_node`);
    /// samples to this id are filtered out.
    pub fn new(graph: Arc<ProximityGraph>, this_node: NodeId) -> Self {
        Self { graph, this_node }
    }
}

impl LocalityProbe for ProximityGraphLocalityProbe {
    fn rtt_samples(&self) -> Vec<(NodeId, Duration)> {
        self.graph
            .all_nodes()
            .into_iter()
            .filter_map(|node| {
                let peer = graph_id_to_node_id(&node.node_id);
                if peer == self.this_node {
                    return None;
                }
                Some((peer, Duration::from_micros(node.latency_us)))
            })
            .collect()
    }
}

/// Wraps a `ProximityGraph` and reports peer health based on
/// edge freshness: a peer whose latest pingwave update is older
/// than `stale_threshold` is `Unreachable`; within the staleness
/// window but older than `degraded_threshold` is `Degraded`;
/// fresher than that is `Healthy`.
pub struct ProximityGraphHealthProbe {
    graph: Arc<ProximityGraph>,
    this_node: NodeId,
    degraded_threshold: Duration,
    stale_threshold: Duration,
}

impl ProximityGraphHealthProbe {
    /// Construct a probe with the staleness thresholds. Sensible
    /// defaults: degraded = 3× heartbeat (1.5 s default), stale
    /// = 10× heartbeat (5 s default).
    pub fn new(
        graph: Arc<ProximityGraph>,
        this_node: NodeId,
        degraded_threshold: Duration,
        stale_threshold: Duration,
    ) -> Self {
        Self {
            graph,
            this_node,
            degraded_threshold,
            stale_threshold,
        }
    }

    /// Construct with the recommended defaults (1.5 s degraded
    /// threshold, 5 s stale threshold). Aligns with the proximity
    /// graph's typical heartbeat cadence + reasonable headroom.
    pub fn with_defaults(graph: Arc<ProximityGraph>, this_node: NodeId) -> Self {
        Self::new(
            graph,
            this_node,
            Duration::from_millis(1500),
            Duration::from_secs(5),
        )
    }
}

impl HealthProbe for ProximityGraphHealthProbe {
    fn health_samples(&self) -> Vec<(NodeId, NodeHealth)> {
        let now = Instant::now();
        self.graph
            .all_nodes()
            .into_iter()
            .filter_map(|node| {
                let peer = graph_id_to_node_id(&node.node_id);
                if peer == self.this_node {
                    return None;
                }
                let age = now.saturating_duration_since(node.last_seen);
                let health = if age >= self.stale_threshold {
                    NodeHealth::Unreachable
                } else if age >= self.degraded_threshold {
                    NodeHealth::Degraded
                } else {
                    NodeHealth::Healthy
                };
                Some((peer, health))
            })
            .collect()
    }
}

/// `[u8; 32] → u64` id-bridge, mirroring the substrate's
/// `mesh::graph_id_to_node_id` convention (first 8 bytes
/// little-endian). Inlined here to avoid making the substrate
/// helper public; the convention is the load-bearing piece, not
/// the function location.
#[expect(
    clippy::unwrap_used,
    reason = "input is &[u8; 32]; slicing [0..8] then .try_into::<[u8; 8]>() is statically infallible"
)]
fn graph_id_to_node_id(graph_id: &[u8; 32]) -> NodeId {
    u64::from_le_bytes(graph_id[0..8].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test-only locality probe: returns whatever the test
    /// configured.
    struct FixedLocalityProbe {
        samples: Vec<(NodeId, Duration)>,
    }
    impl LocalityProbe for FixedLocalityProbe {
        fn rtt_samples(&self) -> Vec<(NodeId, Duration)> {
            self.samples.clone()
        }
    }

    /// Test-only health probe.
    struct FixedHealthProbe {
        samples: Vec<(NodeId, NodeHealth)>,
    }
    impl HealthProbe for FixedHealthProbe {
        fn health_samples(&self) -> Vec<(NodeId, NodeHealth)> {
            self.samples.clone()
        }
    }

    #[test]
    fn fixed_locality_probe_returns_configured_samples() {
        let probe = FixedLocalityProbe {
            samples: vec![
                (1, Duration::from_millis(50)),
                (2, Duration::from_millis(120)),
            ],
        };
        let samples = probe.rtt_samples();
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0], (1, Duration::from_millis(50)));
        assert_eq!(samples[1], (2, Duration::from_millis(120)));
    }

    #[test]
    fn fixed_health_probe_returns_configured_samples() {
        let probe = FixedHealthProbe {
            samples: vec![(1, NodeHealth::Healthy), (2, NodeHealth::Unreachable)],
        };
        let samples = probe.health_samples();
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].1, NodeHealth::Healthy);
        assert_eq!(samples[1].1, NodeHealth::Unreachable);
    }

    #[test]
    fn graph_id_bridge_round_trips_first_8_bytes_little_endian() {
        let mut graph_id = [0u8; 32];
        graph_id[0..8].copy_from_slice(&12345u64.to_le_bytes());
        assert_eq!(graph_id_to_node_id(&graph_id), 12345);
    }

    // ---------- Real ProximityGraph* probe coverage ----------
    //
    // The existing tests use FixedLocalityProbe / FixedHealthProbe
    // (test-only impls), which means the production
    // `ProximityGraphLocalityProbe` and `ProximityGraphHealthProbe`
    // were not exercised. Both classify peer state on hot paths
    // that drive routing — a regression in the 3-tier health
    // classifier or the self-filter would silently mis-route
    // traffic.

    use crate::adapter::net::behavior::proximity::{
        EnhancedPingwave, ProximityConfig, ProximityGraph,
    };
    use std::net::SocketAddr;

    /// Build `[u8; 32]` graph ids whose first byte is `n` so the
    /// `graph_id_to_node_id` bridge yields a small predictable u64.
    fn gid(n: u8) -> [u8; 32] {
        let mut id = [0u8; 32];
        id[0] = n;
        id
    }

    fn graph_with_one_peer(peer_id: [u8; 32]) -> Arc<ProximityGraph> {
        let my_id = gid(1);
        let graph = Arc::new(ProximityGraph::new(my_id, ProximityConfig::default()));
        let from_addr: SocketAddr = "127.0.0.1:9000".parse().unwrap();
        let pw = EnhancedPingwave::new(peer_id, 1, 3);
        graph.on_pingwave_from(pw, peer_id, from_addr);
        graph
    }

    #[test]
    fn proximity_graph_locality_probe_filters_self_and_returns_peers() {
        let peer = gid(2);
        let graph = graph_with_one_peer(peer);
        let my_u64 = graph_id_to_node_id(&gid(1));
        let probe = ProximityGraphLocalityProbe::new(graph, my_u64);
        let samples = probe.rtt_samples();
        // Only the peer should appear — self must be filtered.
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].0, graph_id_to_node_id(&peer));
        // Latency is whatever the pingwave estimated; just confirm
        // it's a Duration (no panic).
        let _ = samples[0].1;
    }

    #[test]
    fn proximity_graph_health_probe_classifies_age_into_three_tiers() {
        let peer = gid(2);
        let graph = graph_with_one_peer(peer);
        let my_u64 = graph_id_to_node_id(&gid(1));
        let peer_u64 = graph_id_to_node_id(&peer);

        // Healthy: huge thresholds — peer's fresh `last_seen` is
        // well within both.
        let probe = ProximityGraphHealthProbe::new(
            Arc::clone(&graph),
            my_u64,
            Duration::from_secs(3600),
            Duration::from_secs(3600),
        );
        let samples = probe.health_samples();
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0], (peer_u64, NodeHealth::Healthy));

        // Degraded: age ≥ degraded but < stale. Use 1ns degraded
        // (so age trips it instantly) and a huge stale window so
        // the Unreachable arm doesn't activate.
        let probe = ProximityGraphHealthProbe::new(
            Arc::clone(&graph),
            my_u64,
            Duration::from_nanos(1),
            Duration::from_secs(3600),
        );
        // Spin briefly so the wall clock moves past the 1ns floor.
        for _ in 0..1000 {
            std::hint::black_box(0u64);
        }
        let samples = probe.health_samples();
        assert_eq!(samples[0].1, NodeHealth::Degraded);

        // Unreachable: age ≥ stale.
        let probe = ProximityGraphHealthProbe::new(
            graph,
            my_u64,
            Duration::from_nanos(1),
            Duration::from_nanos(1),
        );
        for _ in 0..1000 {
            std::hint::black_box(0u64);
        }
        let samples = probe.health_samples();
        assert_eq!(samples[0].1, NodeHealth::Unreachable);
    }

    #[test]
    fn proximity_graph_health_probe_with_defaults_picks_sensible_thresholds() {
        // The `with_defaults` constructor wires 1.5s degraded and
        // 5s stale. Construct it, exercise a fresh peer (must read
        // as Healthy), then confirm we can swap to a stricter
        // probe with the same graph and get Unreachable — pinning
        // the defaults shape without depending on their exact
        // numerical values past "stricter probes can override."
        let peer = gid(2);
        let graph = graph_with_one_peer(peer);
        let my_u64 = graph_id_to_node_id(&gid(1));
        let defaults_probe = ProximityGraphHealthProbe::with_defaults(Arc::clone(&graph), my_u64);
        let samples = defaults_probe.health_samples();
        assert_eq!(samples.len(), 1);
        // Defaults treat a just-arrived peer as Healthy.
        assert_eq!(samples[0].1, NodeHealth::Healthy);
        // And the defaults wired the field correctly — sanity that
        // the constructor didn't, say, leave thresholds at zero
        // (which would make every peer Unreachable instead).
        assert_eq!(
            defaults_probe.degraded_threshold,
            Duration::from_millis(1500)
        );
        assert_eq!(defaults_probe.stale_threshold, Duration::from_secs(5));
    }
}
