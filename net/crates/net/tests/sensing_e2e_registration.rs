//! SI-2 close-out (SENSING_INTEREST_COALESCING_PLAN v4.3, §4.3/
//! §4.7/§4.10): the full registration story on one real multi-node
//! chain — consumer intake at the leader, candidate resolution over
//! the live planes, the leader's own Local rows, MULTI-HOP upstream
//! propagation toward `next_hop(provider)`, second-consumer
//! coalescing BEFORE the provider hop, and the ttl-churn drain of
//! the whole chain.
//!
//! Topology — four real nodes plus one injected declarer:
//!
//! ```text
//!   A ─┐
//!      R ── H ──(route only)── P
//!   C ─┘
//! ```
//!
//! A and C are consumers with sessions to R; R holds the
//! sensing-leader role; H is the provider-direction next hop —
//! declarer P exists only in R's fold (injected announcement + TOFU
//! pin) with `r.router().add_route(P, addr(H))`, so `next_hop(P)`
//! resolves to H and P itself never has to exist as a session.
//!
//! One shared `sensing_owner_root`: a dedicated FLEET entity whose
//! keypair no node holds — every node serves it via
//! `with_sensing_owner_root`, and every hop admits its peers under
//! the fleet-membership admission (the SI-2 multi-hop half of the
//! `sensing_owner_root` deviation: no per-node key can prove
//! operator fleet ownership, so an explicitly configured hop admits
//! a TOFU-pinned session claiming exactly the served root).
//!
//! Timer parking (the sensing_dispatch.rs recipe, tightened for
//! causality): session timeouts are 10 s so failure detection can't
//! fire in-window; A/C/R heartbeats tick at 100 ms so R's sweeps run
//! promptly — but H's heartbeat is PARKED at 60 s, so H's own expiry
//! sweep can never run in-window and the ONLY way H's table can
//! empty is the explicit upstream `Deregister` R's sweep sends. An H
//! that empties received the deregistration; nothing else removes
//! rows there.
//!
//! UDP delivery is best-effort, so consumers refresh in a loop —
//! re-registration is a soft-state refresh (ttl/2 discipline is the
//! caller's loop in this slice) and each refresh re-arms the whole
//! chain: leader rows, R's Local row, and (damped) the upstream
//! anti-entropy re-send that repairs a lost frame at H.
//!
//! Run: `cargo test --features redex --test sensing_e2e_registration`

#![cfg(all(feature = "net", feature = "redex"))]

mod common;
use common::*;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::capability::{CapabilityAnnouncement, CapabilitySet};
use net::adapter::net::behavior::sensing::{
    encode_interest_frame, AudienceScopeCommitment, CanonicalConstraints, CapabilityId,
    DisclosureClass, DownstreamId, InterestSpec, ProviderInterestKey, ProviderSelector, ResultMode,
    SensingCounters, SensingInterestFrame, WorkLatencyEnvelope, SUBPROTOCOL_SENSING_INTEREST,
};
use net::adapter::net::{EntityId, EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};
use net::adapter::Adapter;

/// The injected declarer — a fold+pin+route fixture, never a
/// session.
const P_DECLARER: u64 = 0xD100;
/// Consumer soft-state lifetime. Generous against CI hiccups (a
/// refresh every 200 ms gives ~7 attempts per window) yet short
/// enough that the churn phase drains the chain promptly.
const TTL: Duration = Duration::from_millis(1500);
/// Requested sample interval D (identity-irrelevant; min-dominance
/// aggregate).
const D: Duration = Duration::from_millis(100);
/// Consumer refresh cadence (~ttl/7; the ttl/2 loop is the caller's
/// in this slice, and retrying faster is semantically free).
const REFRESH: Duration = Duration::from_millis(200);

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

/// The one shared interest spec: `AnyAuthorized` over
/// `print.document`, fleet-scoped. Both consumers build the
/// IDENTICAL spec, so coalescing on the re-derived digest is what
/// the test witnesses.
fn shared_spec(fleet: AudienceScopeCommitment) -> InterestSpec {
    InterestSpec {
        capability_id: CapabilityId::new("print.document"),
        constraints: CanonicalConstraints::from_entries([("media", "a4")]).unwrap(),
        work_latency: WorkLatencyEnvelope::start_within(Duration::from_secs(5)),
        providers: ProviderSelector::AnyAuthorized,
        result_mode: ResultMode::Any,
        disclosure_class: DisclosureClass::Owner,
        audience: fleet,
    }
}

/// Inject declarer P at `r`: folded capability announcement (version
/// = capability generation), TOFU pin to the FLEET entity (the §4.10
/// authorization input), a proximity edge, and the route that makes
/// `next_hop(P)` resolve to H.
fn inject_declarer(r: &Arc<MeshNode>, fleet_entity: &EntityId, via: SocketAddr) {
    let caps = CapabilitySet::new().add_tag("print.document");
    r.test_inject_capability_announcement(CapabilityAnnouncement::new(
        P_DECLARER,
        fleet_entity.clone(),
        7,
        caps,
    ));
    r.test_pin_peer_entity(P_DECLARER, fleet_entity.clone());
    r.router().add_route(P_DECLARER, via);
    r.proximity_graph()
        .test_insert_edge(graph_id(r.node_id()), graph_id(P_DECLARER), 5_000);
}

/// Spawn a consumer's soft-state refresh loop: re-send the encoded
/// registration every [`REFRESH`] until aborted. Stopping the task
/// IS the churn — nothing else withdraws the consumer.
fn spawn_refresher(
    node: Arc<MeshNode>,
    dest: SocketAddr,
    bytes: Vec<u8>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let _ = node
                .send_subprotocol(dest, SUBPROTOCOL_SENSING_INTEREST, &bytes)
                .await;
            tokio::time::sleep(REFRESH).await;
        }
    })
}

/// The full SI-2 story in one flow:
///
/// (a) A's real `CapabilityRegistration` over 0x0C02 coalesces at
///     R's leader — one interest, branches exactly `[P]`;
/// (b) R's OWN table gains the branch's `Local` row AND propagates a
///     `ProviderRegistration` upstream, so H's table gains a
///     `Peer(R)` row for `(interest, P)` carrying the fleet root R's
///     registration proved — multi-hop propagation witnessed on real
///     sessions;
/// (c) a second consumer C with the identical spec coalesces: still
///     one interest, one branch, and H still holds exactly one
///     `Peer(R)` row — demand merged BEFORE the provider hop;
/// (d) ttl churn: A and C stop refreshing; R's sweep drains the
///     leader and R's table, and the upstream `Deregister` empties H
///     (whose own sweep is parked — the emptying IS the frame).
#[tokio::test]
async fn chain_registration_coalesces_propagates_and_drains() {
    // The dedicated fleet identity: no node holds this keypair — the
    // operator knob is the ONLY thing binding the nodes to it.
    let fleet_kp = EntityKeypair::generate();
    let fleet_entity = fleet_kp.entity_id().clone();
    let fleet = AudienceScopeCommitment::owner_root(&fleet_entity);

    let a = Arc::new(
        MeshNode::new(
            EntityKeypair::generate(),
            base_config()
                .with_sensing_coalescing(true)
                .with_sensing_owner_root(fleet),
        )
        .await
        .expect("MeshNode::new A"),
    );
    let c = Arc::new(
        MeshNode::new(
            EntityKeypair::generate(),
            base_config()
                .with_sensing_coalescing(true)
                .with_sensing_owner_root(fleet),
        )
        .await
        .expect("MeshNode::new C"),
    );
    let r = Arc::new(
        MeshNode::new(
            EntityKeypair::generate(),
            base_config()
                .with_sensing_coalescing(true)
                .with_sensing_owner_root(fleet),
        )
        .await
        .expect("MeshNode::new R"),
    );
    // H: heartbeat PARKED — its expiry sweep never runs in-window,
    // so the drain phase's emptying at H can only be the explicit
    // upstream Deregister (see the module docs).
    let h = Arc::new(
        MeshNode::new(EntityKeypair::generate(), {
            let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
            let mut cfg = MeshNodeConfig::new(addr, CHAOS_PSK)
                .with_heartbeat_interval(Duration::from_secs(60))
                .with_session_timeout(Duration::from_secs(10))
                .with_handshake(3, Duration::from_secs(2));
            cfg.socket_buffers = SocketBufferConfig {
                send_buffer_size: CHAOS_BUFFER_SIZE,
                recv_buffer_size: CHAOS_BUFFER_SIZE,
            };
            cfg.with_sensing_coalescing(true)
                .with_sensing_owner_root(fleet)
        })
        .await
        .expect("MeshNode::new H"),
    );

    // Line links only: consumers to R, R to H. A/C never touch H.
    connect_pair(&a, &r).await;
    connect_pair(&c, &r).await;
    connect_pair(&r, &h).await;
    a.start();
    c.start();
    r.start();
    h.start();
    for node in [&a, &c, &r, &h] {
        node.announce_capabilities(CapabilitySet::new())
            .await
            .expect("announce");
    }
    let a_id = a.node_id();
    let c_id = c.node_id();
    let r_id = r.node_id();
    await_condition(Duration::from_secs(5), "entity pins established", || {
        r.peer_entity_id(a_id).is_some()
            && r.peer_entity_id(c_id).is_some()
            && h.peer_entity_id(r_id).is_some()
            && a.peer_entity_id(r_id).is_some()
            && c.peer_entity_id(r_id).is_some()
    })
    .await;

    // The provider population and the provider-direction route.
    inject_declarer(&r, &fleet_entity, h.local_addr());
    assert!(r.assume_sensing_leader(), "leader role installs");

    let spec = shared_spec(fleet);
    let interest_key = spec.key();
    let branch_key = ProviderInterestKey::new(interest_key.clone(), P_DECLARER);
    let r_addr = r.local_addr();

    // ── (a) A registers; R's leader coalesces and resolves [P] ──
    let a_bytes = encode_interest_frame(&SensingInterestFrame::capability_registration(
        &spec, D, TTL, a_id,
    ))
    .expect("A's frame encodes");
    let refresh_a = spawn_refresher(a.clone(), r_addr, a_bytes);

    await_condition(Duration::from_secs(5), "A's registration coalesces", || {
        r.sensing_leader_interest_count() == Some(1)
    })
    .await;
    assert_eq!(
        r.sensing_leader_branches(&interest_key),
        Some(vec![P_DECLARER]),
        "the leader resolves exactly the injected authorized declarer",
    );

    // ── (b) R's OWN table + multi-hop upstream propagation to H ──
    await_condition(Duration::from_secs(5), "R gains its own Local row", || {
        r.sensing_downstreams(&branch_key) == vec![DownstreamId::Local]
    })
    .await;
    assert_eq!(
        r.sensing_interest_count(),
        1,
        "exactly the one branch key at R",
    );
    await_condition(Duration::from_secs(5), "H gains the Peer(R) row", || {
        !h.sensing_downstreams(&branch_key).is_empty()
    })
    .await;
    assert_eq!(
        h.sensing_downstreams(&branch_key),
        vec![DownstreamId::Peer(r_id)],
        "H's row is attributed to R — the coalescing hop, never A or C",
    );
    let row = h
        .sensing_downstream_entry(&branch_key, DownstreamId::Peer(r_id))
        .expect("H's row is present");
    assert_eq!(
        row.owner_root, fleet,
        "H stores the root R's registration proved (the fleet root)",
    );
    assert_eq!(row.requested_sample_interval, D);
    assert_eq!(h.sensing_interest_count(), 1);

    // ── (c) C coalesces on the identical spec BEFORE the hop ──
    let c_bytes = encode_interest_frame(&SensingInterestFrame::capability_registration(
        &spec, D, TTL, c_id,
    ))
    .expect("C's frame encodes");
    let refresh_c = spawn_refresher(c.clone(), r_addr, c_bytes);

    await_condition(
        Duration::from_secs(5),
        "C joins the leader's branch",
        || {
            r.sensing_leader_branch_downstreams(&branch_key)
                .is_some_and(|downstreams| {
                    downstreams.contains(&DownstreamId::Peer(a_id))
                        && downstreams.contains(&DownstreamId::Peer(c_id))
                })
        },
    )
    .await;
    assert_eq!(
        r.sensing_leader_interest_count(),
        Some(1),
        "the identical spec coalesces — still one interest",
    );
    assert_eq!(
        r.sensing_leader_branches(&interest_key),
        Some(vec![P_DECLARER]),
        "still one branch",
    );
    assert_eq!(
        r.sensing_downstreams(&branch_key),
        vec![DownstreamId::Local],
        "R's table still carries ONE Local row — the merge point",
    );
    assert_eq!(
        h.sensing_downstreams(&branch_key),
        vec![DownstreamId::Peer(r_id)],
        "H still holds exactly one Peer(R) row — demand merged before the hop",
    );
    assert_eq!(h.sensing_interest_count(), 1);

    // ── (d) ttl churn: the whole chain drains ──
    refresh_a.abort();
    refresh_c.abort();

    await_condition(Duration::from_secs(10), "R's leader drains", || {
        r.sensing_leader_interest_count() == Some(0)
    })
    .await;
    await_condition(Duration::from_secs(10), "R's table empties", || {
        r.sensing_table_is_empty()
    })
    .await;
    // H's sweep is parked: only the upstream Deregister R's sweep
    // sent can have removed the Peer(R) row.
    await_condition(
        Duration::from_secs(10),
        "H receives the upstream Deregister and empties",
        || h.sensing_table_is_empty(),
    )
    .await;

    // Nothing on the whole flow was protocol-invalid or out of scope
    // at the receiving hops.
    for node in [&r, &h] {
        let counters = node.sensing_counters();
        assert_eq!(SensingCounters::get(&counters.protocol_invalid), 0);
        assert_eq!(SensingCounters::get(&counters.scope_refusals), 0);
        assert_eq!(node.sensing_over_cap_refusals(), 0);
    }

    a.shutdown().await.expect("shutdown A");
    c.shutdown().await.expect("shutdown C");
    r.shutdown().await.expect("shutdown R");
    h.shutdown().await.expect("shutdown H");
}
