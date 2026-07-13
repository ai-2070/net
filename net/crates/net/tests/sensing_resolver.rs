//! SI-2b (SENSING_INTEREST_COALESCING_PLAN v4.3, §4.7/§4.10): the
//! Layer-1 candidate resolver over the REAL capability fold +
//! proximity ranking + reachability + tag provenance, wired into
//! the SI-2a leader intake on live dispatch.
//!
//! Topology: two real nodes, A ↔ R. A is the consumer and (v1
//! owner-root terms) the OWNER — its entity commitment is the
//! fleet root; R serves that root and assumes the sensing-leader
//! role. The provider population is INJECTED at R (fold
//! announcements via `test_inject_capability_announcement`, TOFU
//! pins via `test_pin_peer_entity`, routes via
//! `router().add_route`, proximity edges via `test_insert_edge`):
//!
//! - D1 — declares `print.document`, pinned to the OWNER root,
//!   routable, proximity edge 5 ms;
//! - D2 — declares the same capability, pinned to a FOREIGN root,
//!   routable, proximity edge 1 ms (CLOSER — so its exclusion can
//!   only be §4.10 authorization, never ranking).
//!
//! A routes a provider-free `CapabilityRegistration` to R over the
//! committed 0x0C02 wire path; R's dispatch arm builds the real
//! snapshot and `register_from_frame` must resolve EXACTLY D1 as
//! the active branch.
//!
//! Timer parking (the sensing_dispatch.rs recipe): session
//! timeouts are 10 s so failure detection can't fire in-window;
//! heartbeats tick at 100 ms so sweeps run promptly. UDP is
//! best-effort, so the sender retries — re-registration is a
//! soft-state refresh and semantically free.
//!
//! Run: `cargo test --features redex --test sensing_resolver`

#![cfg(all(feature = "net", feature = "redex"))]

mod common;
use common::*;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::capability::{CapabilityAnnouncement, CapabilitySet};
use net::adapter::net::behavior::sensing::{
    encode_interest_frame, AudienceScopeCommitment, CanonicalConstraints, CapabilityId,
    CapabilityInterestKey, DisclosureClass, InterestSpec, ProviderSelector, ResultMode,
    SensingInterestFrame, WorkLatencyEnvelope, SUBPROTOCOL_SENSING_INTEREST,
};
use net::adapter::net::{EntityId, EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};
use net::adapter::Adapter;

const TTL: Duration = Duration::from_secs(30);
const D: Duration = Duration::from_millis(100);

/// Injected declarer node ids — never sessions, pure fold+pin
/// fixtures.
const D1_AUTHORIZED: u64 = 0xD100;
const D2_FOREIGN: u64 = 0xD200;

fn base_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, CHAOS_PSK)
        .with_heartbeat_interval(Duration::from_millis(100))
        .with_session_timeout(Duration::from_secs(10))
        .with_handshake(3, Duration::from_secs(2));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: CHAOS_BUFFER_SIZE,
        recv_buffer_size: CHAOS_BUFFER_SIZE,
    };
    cfg
}

/// The proximity graph keys nodes by the u64 node id zero-padded to
/// 32 bytes (mesh.rs `node_id_to_graph_id`).
fn graph_id(node_id: u64) -> [u8; 32] {
    let mut id = [0u8; 32];
    id[..8].copy_from_slice(&node_id.to_le_bytes());
    id
}

/// Handshake, start, exchange empty capability announcements so
/// each side TOFU-pins the other (the sensing dispatch arm drops
/// frames from unpinned sessions).
async fn bring_up(a: &Arc<MeshNode>, r: &Arc<MeshNode>) {
    connect_pair(a, r).await;
    a.start();
    r.start();
    a.announce_capabilities(CapabilitySet::new())
        .await
        .expect("A announce");
    r.announce_capabilities(CapabilitySet::new())
        .await
        .expect("R announce");
    let a_id = a.node_id();
    let r_id = r.node_id();
    await_condition(Duration::from_secs(5), "entity pins established", || {
        a.peer_entity_id(r_id).is_some() && r.peer_entity_id(a_id).is_some()
    })
    .await;
}

/// A provider-FREE interest (candidate resolution is the SUT):
/// `AnyAuthorized` over `print.document`, owner-scoped.
fn spec_for(owner: AudienceScopeCommitment) -> InterestSpec {
    InterestSpec {
        capability_id: CapabilityId::new("print.document"),
        constraints: CanonicalConstraints::from_entries([("media", "a4")]).unwrap(),
        work_latency: WorkLatencyEnvelope::start_within(Duration::from_secs(5)),
        providers: ProviderSelector::AnyAuthorized,
        result_mode: ResultMode::Any,
        disclosure_class: DisclosureClass::Owner,
        audience: owner,
    }
}

/// Inject one declarer at `r`: a folded capability announcement
/// (version = capability generation), a TOFU entity pin, a routing
/// table entry, and a proximity edge from R.
fn inject_declarer(
    r: &Arc<MeshNode>,
    node_id: u64,
    entity: &EntityId,
    version: u64,
    tags: &[&str],
    edge_latency_us: u64,
) {
    let mut caps = CapabilitySet::new();
    for tag in tags {
        caps = caps.add_tag(*tag);
    }
    r.test_inject_capability_announcement(CapabilityAnnouncement::new(
        node_id,
        entity.clone(),
        version,
        caps,
    ));
    r.test_pin_peer_entity(node_id, entity.clone());
    let dummy_next_hop: SocketAddr = "127.0.0.1:9".parse().unwrap();
    r.router().add_route(node_id, dummy_next_hop);
    r.proximity_graph()
        .test_insert_edge(graph_id(r.node_id()), graph_id(node_id), edge_latency_us);
}

/// A `CapabilityRegistration` from A lands on R's 0x0C02 dispatch
/// arm, the SI-2b snapshot feeds the leader's resolver, and the
/// coalesced interest activates EXACTLY the authorized declarer —
/// never the closer foreign-root one.
#[tokio::test]
async fn leader_resolves_the_authorized_declarer_from_the_real_planes() {
    // A's entity is the fleet owner root (the sensing_dispatch.rs
    // fixture shape); the foreign declarer pins a different entity.
    let a_kp = EntityKeypair::generate();
    let owner_entity = a_kp.entity_id().clone();
    let owner = AudienceScopeCommitment::owner_root(&owner_entity);
    let foreign_entity = EntityKeypair::generate().entity_id().clone();

    let a = Arc::new(
        MeshNode::new(a_kp, base_config().with_sensing_coalescing(true))
            .await
            .expect("MeshNode::new A"),
    );
    let r = Arc::new(
        MeshNode::new(
            EntityKeypair::generate(),
            base_config()
                .with_sensing_coalescing(true)
                .with_sensing_owner_root(owner),
        )
        .await
        .expect("MeshNode::new R"),
    );
    bring_up(&a, &r).await;
    assert!(r.assume_sensing_leader(), "leader role installs");

    // The injected population: D1 authorized + reachable at 5 ms;
    // D2 foreign-rooted + reachable at 1 ms (closer!).
    inject_declarer(
        &r,
        D1_AUTHORIZED,
        &owner_entity,
        7,
        &["print.document", "calibrated=true"],
        5_000,
    );
    inject_declarer(
        &r,
        D2_FOREIGN,
        &foreign_entity,
        3,
        &["print.document"],
        1_000,
    );

    // Pin the adapter-assembled snapshot first: both declarers
    // visible, flags and ranking inputs from the real planes.
    let snapshot = r.sensing_candidate_snapshot(&CapabilityId::new("print.document"));
    assert_eq!(snapshot.len(), 2, "both injected declarers surface");
    let d1 = snapshot
        .iter()
        .find(|c| c.node_id == D1_AUTHORIZED)
        .expect("D1 in snapshot");
    let d2 = snapshot
        .iter()
        .find(|c| c.node_id == D2_FOREIGN)
        .expect("D2 in snapshot");
    assert!(d1.authorized && d1.reachable);
    assert_eq!(d1.capability_generation, 7, "announcement version");
    assert_eq!(d1.route_estimate, Duration::from_micros(5_000));
    assert!(
        d1.tags
            .iter()
            .any(|t| t.key == "calibrated" && t.value == "true" && t.asserted_by == owner),
        "owner-authored provenance on D1's assertion"
    );
    assert!(!d2.authorized, "foreign pin is never authorized");
    assert!(d2.reachable);
    assert_eq!(
        d2.route_estimate,
        Duration::from_micros(1_000),
        "D2 really is closer — exclusion below must be authority"
    );

    // The real frame over the committed wire path, with retries
    // (best-effort UDP; refreshes are idempotent).
    let spec = spec_for(owner);
    let frame = SensingInterestFrame::capability_registration(&spec, D, TTL, a.node_id());
    let bytes = encode_interest_frame(&frame).expect("frame encodes");
    let r_addr = r.local_addr();
    let mut landed = false;
    for _ in 0..40 {
        a.send_subprotocol(r_addr, SUBPROTOCOL_SENSING_INTEREST, &bytes)
            .await
            .expect("A sends the registration");
        if poll_until(Duration::from_millis(250), || {
            r.sensing_leader_interest_count() == Some(1)
        })
        .await
        {
            landed = true;
            break;
        }
    }
    assert!(landed, "R's leader never accepted A's registration");

    // The resolved branch set: exactly the authorized declarer.
    let key = CapabilityInterestKey {
        capability_id: spec.capability_id.clone(),
        interest_digest: spec.interest_digest(),
    };
    assert_eq!(
        r.sensing_leader_branches(&key),
        Some(vec![D1_AUTHORIZED]),
        "the active branch is the authorized candidate — the closer \
         foreign-root declarer must never enter",
    );

    // A re-delivered registration coalesces into the same row and
    // the same resolution (soft-state refresh, no re-expansion).
    a.send_subprotocol(r_addr, SUBPROTOCOL_SENSING_INTEREST, &bytes)
        .await
        .expect("A refreshes");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(r.sensing_leader_interest_count(), Some(1));
    assert_eq!(r.sensing_leader_branches(&key), Some(vec![D1_AUTHORIZED]));

    // Nothing on this path was protocol-invalid at R.
    let counters = r.sensing_counters();
    assert_eq!(
        net::adapter::net::behavior::sensing::SensingCounters::get(&counters.protocol_invalid),
        0
    );

    a.shutdown().await.expect("shutdown A");
    r.shutdown().await.expect("shutdown R");
}

/// With ONLY foreign-root declarers in the fold, the registration
/// still coalesces (authentic + in-scope) but resolves ZERO
/// branches — the §4.10 boundary fails closed instead of activating
/// an unauthorized stream.
#[tokio::test]
async fn foreign_only_population_resolves_no_branches() {
    let a_kp = EntityKeypair::generate();
    let owner = AudienceScopeCommitment::owner_root(a_kp.entity_id());
    let foreign_entity = EntityKeypair::generate().entity_id().clone();

    let a = Arc::new(
        MeshNode::new(a_kp, base_config().with_sensing_coalescing(true))
            .await
            .expect("MeshNode::new A"),
    );
    let r = Arc::new(
        MeshNode::new(
            EntityKeypair::generate(),
            base_config()
                .with_sensing_coalescing(true)
                .with_sensing_owner_root(owner),
        )
        .await
        .expect("MeshNode::new R"),
    );
    bring_up(&a, &r).await;
    assert!(r.assume_sensing_leader(), "leader role installs");

    inject_declarer(
        &r,
        D2_FOREIGN,
        &foreign_entity,
        1,
        &["print.document"],
        1_000,
    );

    let spec = spec_for(owner);
    let frame = SensingInterestFrame::capability_registration(&spec, D, TTL, a.node_id());
    let bytes = encode_interest_frame(&frame).expect("frame encodes");
    let r_addr = r.local_addr();
    let mut landed = false;
    for _ in 0..40 {
        a.send_subprotocol(r_addr, SUBPROTOCOL_SENSING_INTEREST, &bytes)
            .await
            .expect("A sends the registration");
        if poll_until(Duration::from_millis(250), || {
            r.sensing_leader_interest_count() == Some(1)
        })
        .await
        {
            landed = true;
            break;
        }
    }
    assert!(landed, "the registration itself is accepted");

    let key = CapabilityInterestKey {
        capability_id: spec.capability_id.clone(),
        interest_digest: spec.interest_digest(),
    };
    assert_eq!(
        r.sensing_leader_branches(&key),
        Some(Vec::new()),
        "no authorized candidate ⇒ no active branch (fail closed)",
    );

    a.shutdown().await.expect("shutdown A");
    r.shutdown().await.expect("shutdown R");
}
