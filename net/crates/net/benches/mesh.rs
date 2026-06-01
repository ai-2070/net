//! Benchmarks for MeshNode components.
//!
//! Run with: cargo bench --features net --bench mesh
//!
//! These benchmarks measure the overhead added by MeshNode composition:
//! - Reroute policy decision latency
//! - Proximity graph operations (pingwave processing, path finding)
//! - Routing table lookups in reroute context
//! - Packet discrimination (direct vs routed)
//!
//! Note: Full send/receive benchmarks require real sockets and handshakes.
//! Those are measured in integration tests with timing assertions, not here.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use dashmap::DashMap;
use net::adapter::net::{
    behavior::loadbalance::HealthStatus,
    behavior::proximity::{EnhancedPingwave, ProximityConfig, ProximityGraph},
    NetHeader, PacketFlags, ReroutePolicy, RoutingTable, NONCE_SIZE, ROUTING_HEADER_SIZE,
};

// ============================================================================
// Reroute policy benchmarks
// ============================================================================

fn make_reroute_setup(num_peers: usize, routes_per_peer: usize) -> (Arc<ReroutePolicy>, Vec<u64>) {
    let rt = Arc::new(RoutingTable::new(0x1111));
    let peers = Arc::new(DashMap::new());

    let mut peer_ids = Vec::with_capacity(num_peers);
    for i in 0..num_peers {
        let node_id = 0x2000 + i as u64;
        let addr: SocketAddr = format!("127.0.0.1:{}", 3000 + i).parse().unwrap();
        peers.insert(node_id, addr);
        peer_ids.push(node_id);

        // Add routes through this peer
        for r in 0..routes_per_peer {
            let dest = 0x10000 + (i * routes_per_peer + r) as u64;
            rt.add_route(dest, addr);
        }
    }

    let graph = Arc::new(ProximityGraph::new([0u8; 32], ProximityConfig::default()));
    let policy = Arc::new(ReroutePolicy::new(rt, peers).with_proximity_graph(graph));
    (policy, peer_ids)
}

fn bench_reroute_policy(c: &mut Criterion) {
    let mut group = c.benchmark_group("mesh_reroute");
    group.throughput(Throughput::Elements(1));

    // Reroute with 3 peers, 1 route each (triangle)
    group.bench_function("triangle_failure", |b| {
        let (policy, peers) = make_reroute_setup(3, 1);
        b.iter(|| {
            policy.on_failure(peers[0]);
            policy.on_recovery(peers[0]);
        });
    });

    // Reroute with 10 peers, 10 routes each
    group.bench_function("10_peers_10_routes", |b| {
        let (policy, peers) = make_reroute_setup(10, 10);
        b.iter(|| {
            policy.on_failure(peers[0]);
            policy.on_recovery(peers[0]);
        });
    });

    // Reroute with 50 peers, 100 routes through the failed one
    group.bench_function("50_peers_100_routes", |b| {
        let (policy, peers) = make_reroute_setup(50, 100);
        b.iter(|| {
            policy.on_failure(peers[0]);
            policy.on_recovery(peers[0]);
        });
    });

    group.finish();
}

// ============================================================================
// Proximity graph benchmarks
// ============================================================================

fn bench_proximity_graph(c: &mut Criterion) {
    let mut group = c.benchmark_group("mesh_proximity");
    group.throughput(Throughput::Elements(1));

    let my_id = [0x01u8; 32];
    let graph = ProximityGraph::new(my_id, ProximityConfig::default());

    // Populate with some nodes
    for i in 0..100u64 {
        let mut node_id = [0u8; 32];
        node_id[0..8].copy_from_slice(&i.to_le_bytes());
        let addr: SocketAddr = format!("127.0.0.1:{}", 4000 + i).parse().unwrap();
        let pw = EnhancedPingwave::new(node_id, i, 3).with_load(0, HealthStatus::Healthy);
        graph.on_pingwave(pw, addr);
    }

    // Process a new pingwave.
    //
    // Uses a dedicated graph: this closure inserts a brand-new node on
    // every Criterion iteration (millions of them over a measurement
    // window). Running it against the shared `graph` would balloon that
    // graph to millions of entries before the read-path benchmarks
    // below execute, making `all_nodes_100` clone millions of nodes
    // instead of 100 — a ~quarter-second artifact that has nothing to
    // do with the cost of listing 100 nodes.
    let growth_graph = ProximityGraph::new(my_id, ProximityConfig::default());
    group.bench_function("on_pingwave_new", |b| {
        let mut seq = 10000u64;
        b.iter(|| {
            seq += 1;
            let mut node_id = [0u8; 32];
            node_id[0..8].copy_from_slice(&seq.to_le_bytes());
            let addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();
            let pw = EnhancedPingwave::new(node_id, seq, 3);
            growth_graph.on_pingwave(pw, addr);
        });
    });

    // Process a duplicate pingwave (dedup hit)
    let known_id = {
        let mut id = [0u8; 32];
        id[0..8].copy_from_slice(&50u64.to_le_bytes());
        id
    };
    group.bench_function("on_pingwave_dedup", |b| {
        let addr: SocketAddr = "127.0.0.1:5050".parse().unwrap();
        b.iter(|| {
            let pw = EnhancedPingwave::new(known_id, 50, 3);
            graph.on_pingwave(pw, addr);
        });
    });

    // Pingwave serialization
    group.bench_function("pingwave_serialize", |b| {
        let pw = EnhancedPingwave::new([0x42u8; 32], 1, 3).with_load(0, HealthStatus::Healthy);
        b.iter(|| pw.to_bytes());
    });

    // Pingwave deserialization
    let pw_bytes = EnhancedPingwave::new([0x42u8; 32], 1, 3).to_bytes();
    group.bench_function("pingwave_deserialize", |b| {
        b.iter(|| EnhancedPingwave::from_bytes(&pw_bytes));
    });

    // Node count (used by graph accessor)
    group.bench_function("node_count", |b| {
        b.iter(|| graph.node_count());
    });

    // all_nodes (used by reroute fallback)
    group.bench_function("all_nodes_100", |b| {
        b.iter(|| graph.all_nodes());
    });

    group.finish();
}

// ============================================================================
// Packet discrimination benchmark
// ============================================================================

fn bench_packet_discrimination(c: &mut Criterion) {
    let mut group = c.benchmark_group("mesh_dispatch");
    group.throughput(Throughput::Elements(1));

    // Build a realistic Net packet header (starts with magic 0x4E45)
    let nonce = [0x42u8; NONCE_SIZE];
    let header = NetHeader::new(0x1234, 0x5678, 42, nonce, 100, 5, PacketFlags::NONE);
    let header_bytes = header.to_bytes();

    // Direct packet (starts with magic)
    let mut direct_packet = Vec::with_capacity(128);
    direct_packet.extend_from_slice(&header_bytes);
    direct_packet.extend_from_slice(&[0u8; 64]); // fake payload
    let direct = Bytes::from(direct_packet);

    // Routed packet (starts with routing header, NOT magic)
    let mut routed_packet = Vec::with_capacity(128);
    let routing_header = [0x11u8; ROUTING_HEADER_SIZE]; // non-magic first 2 bytes
    routed_packet.extend_from_slice(&routing_header);
    routed_packet.extend_from_slice(&header_bytes);
    routed_packet.extend_from_slice(&[0u8; 64]);
    let routed = Bytes::from(routed_packet);

    // Pingwave packet (exactly 72 bytes)
    let pw = EnhancedPingwave::new([0x42u8; 32], 1, 3);
    let pw_packet = Bytes::copy_from_slice(&pw.to_bytes());

    // Discrimination: check magic bytes to classify packet type
    group.bench_function("classify_direct", |b| {
        b.iter(|| {
            let magic = u16::from_le_bytes([direct[0], direct[1]]);
            magic == 0x4E45 // true → direct
        });
    });

    group.bench_function("classify_routed", |b| {
        b.iter(|| {
            routed.len() >= ROUTING_HEADER_SIZE + 64
                && u16::from_le_bytes([routed[0], routed[1]]) != 0x4E45 // true → routed
        });
    });

    group.bench_function("classify_pingwave", |b| {
        b.iter(|| {
            pw_packet.len() == EnhancedPingwave::SIZE // true → pingwave
        });
    });

    group.finish();
}

// ============================================================================
// Routing table benchmark (reroute-relevant operations)
// ============================================================================

fn bench_routing_table(c: &mut Criterion) {
    let mut group = c.benchmark_group("mesh_routing");
    group.throughput(Throughput::Elements(1));

    let rt = RoutingTable::new(0x1111);

    // Populate
    for i in 0..1000u64 {
        let addr: SocketAddr = format!("127.0.0.1:{}", 5000 + (i % 100)).parse().unwrap();
        rt.add_route(i + 0x10000, addr);
    }

    // Lookup hit
    group.bench_function("lookup_hit", |b| {
        b.iter(|| rt.lookup(0x10500));
    });

    // Lookup miss
    group.bench_function("lookup_miss", |b| {
        b.iter(|| rt.lookup(0x99999));
    });

    // is_local check
    group.bench_function("is_local", |b| {
        b.iter(|| rt.is_local(0x1111));
    });

    // all_routes (used by reroute policy to find affected routes)
    for count in [10, 100, 1000] {
        let rt = RoutingTable::new(0x1111);
        for i in 0..count as u64 {
            let addr: SocketAddr = format!("127.0.0.1:{}", 6000 + (i % 50)).parse().unwrap();
            rt.add_route(i + 0x20000, addr);
        }
        group.bench_with_input(BenchmarkId::new("all_routes", count), &count, |b, _| {
            b.iter(|| rt.all_routes());
        });
    }

    // add_route (atomic overwrite)
    group.bench_function("add_route", |b| {
        let addr: SocketAddr = "127.0.0.1:7777".parse().unwrap();
        b.iter(|| rt.add_route(0x10500, addr));
    });

    group.finish();
}

criterion_group!(reroute_benches, bench_reroute_policy,);

criterion_group!(proximity_benches, bench_proximity_graph,);

criterion_group!(dispatch_benches, bench_packet_discrimination,);

criterion_group!(routing_benches, bench_routing_table,);

criterion_main!(
    reroute_benches,
    proximity_benches,
    dispatch_benches,
    routing_benches
);
