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

use net::adapter::net::behavior::fold::{
    CapabilityFilter, CapabilityFold, CapabilityMembership, CapabilityQuery, EnvelopeMeta, FoldKind,
    GpuSet, IslandRecord, IslandTopologyFold, NodeState, SignedAnnouncement,
};
use net::adapter::net::behavior::gang::{
    match_islands, single_island_claim, ClaimOutcome, MatchCriteria, NumericFilter, SelectionPolicy,
};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig};

const PSK: [u8; 32] = [0x5a; 32];

async fn build_node() -> Arc<MeshNode> {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    Arc::new(
        MeshNode::new(EntityKeypair::generate(), MeshNodeConfig::new(addr, PSK))
            .await
            .expect("MeshNode::new"),
    )
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
        gpus: GpuSet::new(vec![0, 1, 2, 3, 4, 5, 6, 7]),
        host: node_id,
        warm_models: vec![0xA1],
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
            min_gpus: 8,
            ..Default::default()
        },
        selection: SelectionPolicy::LeastLoaded,
        prefer_warm_model: None,
    };

    // The scheduler reads the node's wired folds: both islands match,
    // least-loaded first (B at 0.2 before A at 0.7).
    let order = match_islands(node.capability_fold(), node.island_fold(), &criteria);
    assert_eq!(order, vec![0xB0, 0xA0], "scheduler ranks over the node's island fold");

    // Claim the top island through the node's reservation fold.
    let claimant = EntityKeypair::generate();
    let cn = claimant.entity_id().node_id();
    let now_us = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_micros() as u64;
    let deadline = now_us + 60_000_000;
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
            .query(net::adapter::net::behavior::fold::ReservationQuery::State(0xB0))[0]
            .1
            .holder(),
        Some(cn),
    );
}
