//! Live-wiring integration test for the Thunderdome gang-claim
//! scheduler: the `IslandTopologyFold` is mounted on `MeshNode`
//! alongside the capability + reservation folds, and the match→claim
//! pipeline reads the node's wired folds end-to-end.
//!
//! Peer announcements are applied directly to the node's folds (the
//! same effect the inbound `SUBPROTOCOL_FOLD` dispatch produces, since
//! the island fold is now registered in the node's `FoldRegistry`),
//! then the scheduler runs over `node.capability_fold()` +
//! `node.island_fold()` and claims through `node.reservation_fold()`.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::behavior::fold::{
    CapabilityFilter, CapabilityFold, CapabilityMembership, CapabilityQuery, EnvelopeMeta,
    FoldKind, UnitSet, IslandQuery, IslandRecord, IslandTopologyFold, NodeState, ReservationQuery,
    SignedAnnouncement,
};
use net::adapter::net::behavior::gang::{
    match_islands, single_island_claim, ClaimOutcome, MatchCriteria, NumericFilter, SelectionPolicy,
};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig};

const PSK: [u8; 32] = [0x5a; 32];

async fn build_node() -> Arc<MeshNode> {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(3, Duration::from_secs(2));
    Arc::new(
        MeshNode::new(EntityKeypair::generate(), cfg)
            .await
            .expect("MeshNode::new"),
    )
}

/// House pattern: handshake `a` → `b` (and accept on `b`).
async fn connect_pair(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    let a_id = a.node_id();
    let b_pub = *b.public_key();
    let b_addr = b.local_addr();
    let b_id = b.node_id();
    let b_clone = b.clone();
    let accept = tokio::spawn(async move { b_clone.accept(a_id).await });
    a.connect(b_addr, &b_pub, b_id).await.expect("connect");
    accept.await.expect("accept task").expect("accept");
}

fn now_us() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_micros() as u64
}

/// Sign a capability announcement (peer carries `tags`, Idle) and
/// apply it to `node`'s capability fold — the effect inbound dispatch
/// has.
fn prime_capability(node: &MeshNode, kp: &EntityKeypair, node_id: u64, tags: Vec<String>) {
    let membership = CapabilityMembership {
        class_hash: 0x67_70_75,
        tags,
        hardware: None,
        state: NodeState::Idle,
        region: None,
        price_quote: None,
        reflex_addr: None,
        allowed_nodes: Vec::new(),
        allowed_subnets: Vec::new(),
        allowed_groups: Vec::new(),
        metadata: BTreeMap::new(),
    };
    let ann = SignedAnnouncement::sign(
        kp,
        CapabilityFold::KIND_ID,
        membership.class_hash,
        node_id,
        1,
        EnvelopeMeta::default(),
        membership,
    )
    .expect("sign cap");
    node.capability_fold().apply(ann).expect("apply cap");
}

/// Sign an island record (hosted by `node_id`) and apply it to
/// `node`'s island fold.
fn prime_island(node: &MeshNode, kp: &EntityKeypair, node_id: u64, id: u64, load: f32) {
    let record = IslandRecord {
        id,
        units: UnitSet::new(vec![0, 1, 2, 3, 4, 5, 6, 7]),
        host: node_id,
        capabilities: vec!["model:a1".into()],
        load,
        p50_latency_us: 1_200,
    };
    let ann = SignedAnnouncement::sign(
        kp,
        IslandTopologyFold::KIND_ID,
        0,
        node_id,
        1,
        EnvelopeMeta::default(),
        record,
    )
    .expect("sign island");
    node.island_fold().apply(ann).expect("apply island");
}

#[tokio::test]
async fn island_fold_is_wired_and_scheduler_matches_and_claims_over_node_folds() {
    let node = build_node().await;

    // Two GPU peers fold into the node's capability + island folds.
    let peer_a = EntityKeypair::generate();
    let peer_b = EntityKeypair::generate();
    let na = peer_a.entity_id().node_id();
    let nb = peer_b.entity_id().node_id();
    prime_capability(&node, &peer_a, na, vec!["gpu:h100".into()]);
    prime_capability(&node, &peer_b, nb, vec!["gpu:h100".into()]);
    prime_island(&node, &peer_a, na, 0xA0, 0.7);
    prime_island(&node, &peer_b, nb, 0xB0, 0.2);

    let criteria = MatchCriteria {
        capability: CapabilityQuery::Composite(CapabilityFilter {
            tags_all: vec!["gpu:h100".into()],
            ..Default::default()
        }),
        numeric: NumericFilter {
            min_units: 8,
            ..Default::default()
        },
        selection: SelectionPolicy::LeastLoaded,
        prefer_capability: None,
    };

    // The scheduler reads the node's wired folds: both islands match,
    // least-loaded first (B at 0.2 before A at 0.7).
    let order = match_islands(node.capability_fold(), node.island_fold(), &criteria);
    assert_eq!(
        order,
        vec![0xB0, 0xA0],
        "scheduler ranks over the node's island fold"
    );

    // Claim the top island through the node's reservation fold.
    let claimant = EntityKeypair::generate();
    let cn = claimant.entity_id().node_id();
    let deadline = now_us() + 60_000_000;
    let got = single_island_claim(
        node.reservation_fold(),
        &claimant,
        cn,
        1,
        order[0],
        deadline,
    )
    .expect("claim");
    assert_eq!(got, ClaimOutcome::Won);
    assert_eq!(
        node.reservation_fold()
            .query(net::adapter::net::behavior::fold::ReservationQuery::State(
                0xB0
            ))[0]
            .1
            .holder(),
        Some(cn),
    );
}

/// 2-node broadcast: a host publishes its island topology and it
/// converges into a connected peer's island fold over the wire —
/// proving the island fold is registered in the live dispatch path
/// (`publish_island_topology` → `SUBPROTOCOL_FOLD` → peer's fold).
///
/// The host first announces capabilities: the `SUBPROTOCOL_FOLD`
/// dispatch keys on `peer_entity_ids`, which the receiver populates
/// from the publisher's capability announcement (the entity
/// bootstrap). We then re-publish the island each poll so the
/// one-shot broadcast can't race ahead of that bootstrap.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn island_topology_broadcasts_to_a_connected_peer() {
    let host = build_node().await;
    let peer = build_node().await;
    connect_pair(&host, &peer).await;
    host.start();
    peer.start();

    let host_id = host.node_id();
    // Bootstrap: the peer learns host's EntityId from this.
    host.announce_capabilities(CapabilitySet::new())
        .await
        .expect("announce");

    let record = IslandRecord {
        id: 0xC0,
        units: UnitSet::new(vec![0, 1, 2, 3]),
        host: 0, // overwritten with host.node_id() by publish
        capabilities: vec!["model:a1".into()],
        load: 0.42,
        p50_latency_us: 900,
    };
    let peer_view = peer.clone();
    let mut converged = false;
    for _ in 0..50 {
        host.publish_island_topology(record.clone())
            .await
            .expect("publish island");
        tokio::time::sleep(Duration::from_millis(100)).await;
        if !peer_view
            .island_fold()
            .query(IslandQuery::Get(0xC0))
            .is_empty()
        {
            converged = true;
            break;
        }
    }
    assert!(converged, "peer should fold the host's island announcement");

    let row = peer.island_fold().query(IslandQuery::Get(0xC0));
    assert_eq!(row[0].1.host, host_id, "host stamped as the announcer");
    assert_eq!(row[0].1.load, 0.42);
}

/// A node that hosts GPU islands must see them in its OWN island fold
/// after `publish_island_topology` — the broadcast only reaches peers,
/// but the node's own scheduler (`match_islands` / `claim_island`)
/// reads the local fold. Without the self-apply a co-located
/// scheduler+host could never schedule onto its own hardware (review #1).
#[tokio::test]
async fn publish_island_topology_self_indexes_for_the_local_scheduler() {
    let node = build_node().await;
    let host_id = node.node_id();

    let record = IslandRecord {
        id: 0xE0,
        units: UnitSet::new(vec![0, 1, 2, 3, 4, 5, 6, 7]),
        host: 0, // overwritten with node.node_id() by publish
        capabilities: vec!["model:a1".into()],
        load: 0.1,
        p50_latency_us: 800,
    };
    node.publish_island_topology(record).await.expect("publish");

    // Visible locally with no peer and no wire round-trip.
    let row = node.island_fold().query(IslandQuery::Get(0xE0));
    assert_eq!(
        row.len(),
        1,
        "self-published island is visible in the node's own fold"
    );
    assert_eq!(row[0].1.host, host_id, "host stamped as this node");
    assert_eq!(row[0].1.load, 0.1);
}

/// Node-level claim round-trip: a scheduler node folds a GPU peer's
/// capability + island (primed here as already-converged), runs
/// `claim_island` against its OWN folds, and the resulting
/// reservation broadcasts to a connected peer's reservation fold.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn claim_island_reserves_and_broadcasts_to_peer() {
    let scheduler = build_node().await;
    let peer = build_node().await;
    connect_pair(&scheduler, &peer).await;
    scheduler.start();
    peer.start();

    // Bootstrap: the peer learns the scheduler's EntityId so the
    // scheduler's reservation broadcasts will dispatch into the
    // peer's fold.
    scheduler
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("announce");

    // A GPU peer's capability + island already folded into the
    // scheduler's view (fold→fold convergence is exercised by the
    // broadcast test above).
    let gpu = EntityKeypair::generate();
    let gn = gpu.entity_id().node_id();
    prime_capability(&scheduler, &gpu, gn, vec!["gpu:h100".into()]);
    prime_island(&scheduler, &gpu, gn, 0xD0, 0.1);

    let criteria = MatchCriteria {
        capability: CapabilityQuery::Composite(CapabilityFilter {
            tags_all: vec!["gpu:h100".into()],
            ..Default::default()
        }),
        numeric: NumericFilter {
            min_units: 8,
            ..Default::default()
        },
        selection: SelectionPolicy::LeastLoaded,
        prefer_capability: None,
    };

    // First claim establishes the local hold (optimistic AP view).
    let claimed = scheduler
        .claim_island(&criteria, now_us() + 60_000_000)
        .await
        .expect("claim_island");
    assert_eq!(claimed, Some(0xD0));
    let sched_id = scheduler.node_id();
    assert_eq!(
        scheduler
            .reservation_fold()
            .query(ReservationQuery::State(0xD0))[0]
            .1
            .holder(),
        Some(sched_id),
    );

    // Re-broadcast the reservation (a legal self-extend) each poll so
    // it converges on the peer once the entity bootstrap has landed.
    let peer_view = peer.clone();
    let mut converged = false;
    for _ in 0..50 {
        scheduler
            .reserve_island(0xD0, now_us() + 60_000_000)
            .await
            .expect("reserve");
        tokio::time::sleep(Duration::from_millis(100)).await;
        if peer_view
            .reservation_fold()
            .query(ReservationQuery::State(0xD0))
            .first()
            .and_then(|(_, s)| s.holder())
            == Some(sched_id)
        {
            converged = true;
            break;
        }
    }
    assert!(
        converged,
        "peer should see the scheduler's reservation converge"
    );
}
