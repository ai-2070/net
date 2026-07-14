//! Gate (s) — the multi-hop real-path test
//! (SENSING_INTEREST_COALESCING_PLAN §4.2 + §4.10, v4.3 review 7):
//! authenticated consumer origin, owner-scope enforcement, digest
//! re-derivation, coalescing at the elected leader, and routed proof
//! fan-out — without confusing transport relays for subscribers.
//!
//! Topology — five real `MeshNode`s:
//!
//! ```text
//!   A ── X ── R ── Y ── C
//! ```
//!
//! Sessions: A↔X, X↔R, C↔Y, Y↔R (the line links), plus A↔R and C↔R
//! end-to-end key-exchange sessions (the same "connect directly for
//! the session keys, route the frames through the relay" pattern the
//! three_node relay suite and `tests/sensing_fallback.rs` pin).
//! Frame traffic is FORCED through the relays with metric-1 route
//! overrides (A: R via X; C: R via Y; R: A via X, C via Y) — the
//! pingwave plane installs only metric ≥ 2 routes and
//! `add_route_with_metric` replaces strictly-better-only, so the
//! overrides are stable.
//!
//! ## Origin-authentication mechanism (investigated, documented)
//!
//! nRPC (`MeshNode::call` → `publish_to_peer`, mesh.rs) builds a
//! DIRECT packet — no `ROUTING_MAGIC` header — and a relay's
//! dispatch drops direct packets whose `session_id` matches none of
//! its sessions, so the nRPC request path cannot traverse a relay
//! today. The frames therefore ride `send_routed` (routing header +
//! payload encrypted end-to-end under the A↔R / C↔R session keys;
//! relays forward header-only — the pinned opaque-relay path), and
//! the leader node authenticates the origin with the SAME machinery
//! the nRPC callee-side capability gate uses:
//! `MeshNode::register_rpc_inbound` → `RpcInboundEvent::from_node`,
//! the AEAD-verified NodeId of the session that decrypted the inner
//! packet — resolved from the inner header's `session_id` in
//! `process_local_packet`, i.e. the END-TO-END sender (A or C),
//! never the relay that delivered the final hop. No new signing is
//! invented. Routed event-plane packets carry the builder-default
//! channel hash 0, so the test registers its intake under canonical
//! channel hash 0 — a TEST-ONLY intake ("test.sensing.
//! capability_registration" in spirit); no sensing wire id is
//! consumed (SI-1 owns 0x0C02/0x0C03).
//!
//! Run: `cargo test --features cortex --test sensing_routed_origin`
//! (cortex ⊃ redex ⊃ net; all three ride the default feature set).

#![cfg(all(feature = "net", feature = "redex", feature = "cortex"))]
#![allow(
    clippy::disallowed_methods,
    reason = "test code uses std/parking_lot sync primitives for SUT plumbing"
)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use net::adapter::net::behavior::sensing::{
    sensing_leader, Attestation, AttestedStatus, AudienceScopeCommitment, CandidatePolicy,
    CandidateProvider, CanonicalConstraints, CapabilityId, CapabilityInterestKey, DisclosureClass,
    DownstreamId, FrameRejection, Incarnation, InterestSpec, ProviderInterestKey,
    ProviderObservationKey, ProviderSelector, ResultMode, ScopeError, SensingCounters,
    SensingInterestFrame, SensingLeader, WorkLatencyEnvelope,
};
use net::adapter::net::cortex::RpcInboundDispatcher;
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};
use net::adapter::Adapter;
use net::event::{batch_process_nonce, Batch, InternalEvent};
use parking_lot::Mutex;
use tokio::net::UdpSocket;

const PSK: [u8; 32] = [0x42u8; 32];
const TEST_BUFFER_SIZE: usize = 256 * 1024;
/// The provider the leader resolves for the coalesced interest —
/// a fixture id: the branch is opened and the proof fanned back by
/// the leader role itself, so no sixth node is needed.
const PROVIDER: u64 = 0x7777_7777;
const TTL: Duration = Duration::from_secs(30);

async fn find_ports(n: usize) -> Vec<u16> {
    let mut ports = Vec::with_capacity(n);
    let mut sockets = Vec::with_capacity(n);
    for _ in 0..n {
        let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        ports.push(sock.local_addr().unwrap().port());
        sockets.push(sock);
    }
    drop(sockets);
    ports
}

fn mk_config(addr: SocketAddr) -> MeshNodeConfig {
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_num_shards(2)
        .with_handshake(3, Duration::from_secs(3))
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(10));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: TEST_BUFFER_SIZE,
        recv_buffer_size: TEST_BUFFER_SIZE,
    };
    cfg
}

async fn wait_until<F: FnMut() -> bool>(mut cond: F, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    cond()
}

/// The proximity graph keys nodes by the u64 node id zero-padded to
/// 32 bytes (mesh.rs `node_id_to_graph_id`); the election speaks
/// plain u64.
fn graph_id(node_id: u64) -> [u8; 32] {
    let mut id = [0u8; 32];
    id[0..8].copy_from_slice(&node_id.to_le_bytes());
    id
}

/// The undirected shared-view RTT closure over one node's proximity
/// graph — the centrality input the rendezvous reads (§4.1).
fn graph_rtt(node: &Arc<MeshNode>) -> impl Fn(u64, u64) -> Option<Duration> + '_ {
    move |a, b| {
        node.proximity_graph()
            .edge_latency(graph_id(a), graph_id(b))
            .or_else(|| {
                node.proximity_graph()
                    .edge_latency(graph_id(b), graph_id(a))
            })
    }
}

/// One event per batch: the JSON-serialized sensing frame (the SI-0
/// semantic form; the codec is SI-1's).
fn frame_batch(frame: &SensingInterestFrame) -> Batch {
    let value = serde_json::to_value(frame).expect("frame serializes");
    Batch {
        shard_id: 0,
        events: vec![InternalEvent::from_value(value, 0, 0)],
        sequence_start: 0,
        process_nonce: batch_process_nonce(),
    }
}

fn payload_batch(value: serde_json::Value) -> Batch {
    Batch {
        shard_id: 0,
        events: vec![InternalEvent::from_value(value, 0, 0)],
        sequence_start: 0,
        process_nonce: batch_process_nonce(),
    }
}

/// Drain a node's shard-0 inbound queue into `sink` until an event
/// with the wanted tag shows up (or the deadline passes). Returns
/// the first matching event.
async fn recv_tagged(
    node: &Arc<MeshNode>,
    sink: &mut Vec<serde_json::Value>,
    tag: &str,
    timeout: Duration,
) -> Option<serde_json::Value> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(found) = sink.iter().find(|json| json["tag"] == tag) {
            return Some(found.clone());
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        let result = node.poll_shard(0, None, 100).await.unwrap();
        for event in &result.events {
            if let Ok(json) = event.parse() {
                sink.push(json);
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// What the leader's intake observed for one decoded frame.
#[derive(Debug, Clone)]
enum Outcome {
    Accepted {
        origin: u64,
        interest: CapabilityInterestKey,
        branches: Vec<u64>,
    },
    Rejected {
        origin: u64,
        rejection: FrameRejection,
    },
}

#[tokio::test]
async fn routed_frames_authenticate_origin_coalesce_and_fan_back() {
    let ports = find_ports(5).await;

    let id_a = EntityKeypair::generate();
    let id_x = EntityKeypair::generate();
    let id_r = EntityKeypair::generate();
    let id_c = EntityKeypair::generate();
    let id_y = EntityKeypair::generate();
    let (nid_a, nid_x, nid_r, nid_c, nid_y) = (
        id_a.node_id(),
        id_x.node_id(),
        id_r.node_id(),
        id_c.node_id(),
        id_y.node_id(),
    );

    // The owner-root fixture: the fleet owner's entity, shared by A,
    // C, and R (v1 owner-root-only, §4.10). Sessions from A and C
    // "prove" this root; the foreign root proves nothing here.
    let owner = EntityKeypair::generate();
    let owner_root = AudienceScopeCommitment::owner_root(owner.entity_id());
    let foreign = EntityKeypair::generate();
    let foreign_root = AudienceScopeCommitment::owner_root(foreign.entity_id());

    let addr_a: SocketAddr = format!("127.0.0.1:{}", ports[0]).parse().unwrap();
    let addr_x: SocketAddr = format!("127.0.0.1:{}", ports[1]).parse().unwrap();
    let addr_r: SocketAddr = format!("127.0.0.1:{}", ports[2]).parse().unwrap();
    let addr_c: SocketAddr = format!("127.0.0.1:{}", ports[3]).parse().unwrap();
    let addr_y: SocketAddr = format!("127.0.0.1:{}", ports[4]).parse().unwrap();

    let node_a = Arc::new(MeshNode::new(id_a, mk_config(addr_a)).await.unwrap());
    let node_x = Arc::new(MeshNode::new(id_x, mk_config(addr_x)).await.unwrap());
    let node_r = Arc::new(MeshNode::new(id_r, mk_config(addr_r)).await.unwrap());
    let node_c = Arc::new(MeshNode::new(id_c, mk_config(addr_c)).await.unwrap());
    let node_y = Arc::new(MeshNode::new(id_y, mk_config(addr_y)).await.unwrap());

    // Sessions. Line links first, then the end-to-end key-exchange
    // sessions (direct connect for the keys — the frames themselves
    // are routed through X/Y below, exactly the sensing_fallback
    // pattern).
    let links: [(&Arc<MeshNode>, &Arc<MeshNode>, SocketAddr, u64); 6] = [
        (&node_a, &node_x, addr_x, nid_x), // A ↔ X
        (&node_x, &node_r, addr_r, nid_r), // X ↔ R
        (&node_c, &node_y, addr_y, nid_y), // C ↔ Y
        (&node_y, &node_r, addr_r, nid_r), // Y ↔ R
        (&node_a, &node_r, addr_r, nid_r), // A ↔ R (end-to-end keys)
        (&node_c, &node_r, addr_r, nid_r), // C ↔ R (end-to-end keys)
    ];
    for (initiator, responder, resp_addr, resp_nid) in links {
        let init_nid = initiator.node_id();
        let resp_pub = *responder.public_key();
        let (r1, r2) = tokio::join!(responder.accept(init_nid), async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            initiator.connect(resp_addr, &resp_pub, resp_nid).await
        });
        r1.expect("accept");
        r2.expect("connect");
    }

    // The leader role + REAL frame intake on R, wired BEFORE anything
    // sends. The dispatcher is the mesh's per-channel inbound hook —
    // the same surface nRPC's serve_rpc bridge rides — so
    // `ev.from_node` below is the AEAD-verified end-to-end origin.
    let counters = Arc::new(SensingCounters::default());
    let leader = Arc::new(Mutex::new(SensingLeader::new(
        owner_root,
        CandidatePolicy::default(),
        3,
        512,
    )));
    let outcomes: Arc<Mutex<Vec<Outcome>>> = Arc::new(Mutex::new(Vec::new()));
    let decode_failures = Arc::new(Mutex::new(0u32));
    // Session-root derivation fixture: which owner root each
    // authenticated origin's session proves. A and C are fleet
    // members under the shared owner root.
    let session_roots: HashMap<u64, AudienceScopeCommitment> =
        HashMap::from([(nid_a, owner_root), (nid_c, owner_root)]);
    let snapshot = vec![CandidateProvider {
        node_id: PROVIDER,
        capability_generation: 1,
        authorized: true,
        reachable: true,
        route_estimate: Duration::from_millis(10),
        tags: Vec::new(),
        groups: Vec::new(),
    }];

    let dispatcher: RpcInboundDispatcher = {
        let counters = counters.clone();
        let leader = leader.clone();
        let outcomes = outcomes.clone();
        let decode_failures = decode_failures.clone();
        Arc::new(move |ev| {
            let Ok(frame) = serde_json::from_slice::<SensingInterestFrame>(&ev.payload) else {
                *decode_failures.lock() += 1;
                return;
            };
            // v1 §4.10: the session root comes from the AUTHENTICATED
            // origin's entity — never from any frame field.
            let session_root = session_roots
                .get(&ev.from_node)
                .copied()
                .unwrap_or(foreign_root);
            let result = leader.lock().register_from_frame(
                &frame,
                ev.from_node,
                &session_root,
                &owner_root,
                &counters,
                &snapshot,
                Instant::now(),
            );
            outcomes.lock().push(match result {
                Ok(reg) => Outcome::Accepted {
                    origin: ev.from_node,
                    interest: reg.interest,
                    branches: reg.branches,
                },
                Err(rejection) => Outcome::Rejected {
                    origin: ev.from_node,
                    rejection,
                },
            });
        })
    };
    // Routed event-plane packets carry the builder-default channel
    // hash 0 — the test-only intake registers there (no sensing wire
    // id exists yet; SI-1 commits 0x0C02).
    assert!(node_r.register_rpc_inbound(0, dispatcher).is_none());

    node_a.start();
    node_x.start();
    node_r.start();
    node_c.start();
    node_y.start();

    // ── The elected leader ──────────────────────────────────────
    // Every observer computes the rendezvous from its OWN pingwave-
    // flooded proximity graph. R holds a session (and hence a
    // penalty-free graph edge) with all four other members; every
    // other member reaches at most two, so R's closeness score is
    // the unique minimum at every observer once its graph learns R's
    // edges. (The per-observer graphs are NOT identical — pingwave
    // dedup drops the slower copy of a wave, so an observer with a
    // direct link to a wave's origin never installs the relayed
    // copy's edge — which is exactly why the wait condition is the
    // election OUTCOME stabilizing, not full edge-set equality.)
    let members = [nid_a, nid_x, nid_r, nid_c, nid_y];
    let healthy = |_: u64| true; // failure-plane integration is SI-5
    let converged = wait_until(
        || {
            [&node_a, &node_c, &node_r]
                .iter()
                .all(|node| sensing_leader(&members, graph_rtt(node), healthy) == Some(nid_r))
        },
        Duration::from_secs(15),
    )
    .await;
    assert!(
        converged,
        "the election never stabilized on R (A: {:?}, C: {:?}, R: {:?})",
        sensing_leader(&members, graph_rtt(&node_a), healthy),
        sensing_leader(&members, graph_rtt(&node_c), healthy),
        sensing_leader(&members, graph_rtt(&node_r), healthy),
    );
    // All three observers agree on the center, from their own
    // graphs.
    let leader_at_a = sensing_leader(&members, graph_rtt(&node_a), healthy);
    let leader_at_c = sensing_leader(&members, graph_rtt(&node_c), healthy);
    let leader_at_r = sensing_leader(&members, graph_rtt(&node_r), healthy);
    assert_eq!(leader_at_a, Some(nid_r), "A elects R (the center)");
    assert_eq!(leader_at_a, leader_at_c, "C agrees");
    assert_eq!(leader_at_a, leader_at_r, "R agrees");

    // ── Force the multi-hop paths ───────────────────────────────
    // Metric-1 overrides (unconditional insert; pingwave routes are
    // metric ≥ 2 and replace strictly-better-only, so these stick):
    // A reaches R via X, C via Y; R fans back via the same relays.
    node_a.router().add_route(nid_r, addr_x);
    node_c.router().add_route(nid_r, addr_y);
    node_r.router().add_route(nid_a, addr_x);
    node_r.router().add_route(nid_c, addr_y);

    // ── Two REAL frames: same predicate/selector/mode, different D ─
    let spec = InterestSpec {
        capability_id: CapabilityId::new("print.document"),
        constraints: CanonicalConstraints::from_entries([("color", "true"), ("media", "a4")])
            .unwrap(),
        work_latency: WorkLatencyEnvelope::start_within(Duration::from_secs(5)),
        providers: ProviderSelector::AnyAuthorized,
        result_mode: ResultMode::Any,
        disclosure_class: DisclosureClass::Owner,
        audience: owner_root,
    };
    let frame_a = SensingInterestFrame::capability_registration(
        &spec,
        Duration::from_millis(100),
        TTL,
        nid_a,
    );
    let frame_c = SensingInterestFrame::capability_registration(
        &spec,
        Duration::from_millis(250),
        TTL,
        nid_c,
    );

    // Send-with-retry (UDP; a re-delivered frame is just a soft-state
    // refresh, so retries are semantically free).
    let accepted_from = |origin: u64, outcomes: &Arc<Mutex<Vec<Outcome>>>| {
        outcomes
            .lock()
            .iter()
            .any(|o| matches!(o, Outcome::Accepted { origin: got, .. } if *got == origin))
    };
    for attempt in 0..20 {
        if !accepted_from(nid_a, &outcomes) {
            node_a
                .send_routed(nid_r, &frame_batch(&frame_a))
                .await
                .expect("A routes its frame to the leader");
        }
        if !accepted_from(nid_c, &outcomes) {
            node_c
                .send_routed(nid_r, &frame_batch(&frame_c))
                .await
                .expect("C routes its frame to the leader");
        }
        if wait_until(
            || accepted_from(nid_a, &outcomes) && accepted_from(nid_c, &outcomes),
            Duration::from_millis(500),
        )
        .await
        {
            break;
        }
        assert!(attempt < 19, "leader never accepted both frames");
    }

    // ── Gate (s) assertions: origin, identity, coalescing ───────
    let expected_digest = spec.interest_digest();
    {
        let outcomes = outcomes.lock();
        let accepted: Vec<_> = outcomes
            .iter()
            .filter_map(|o| match o {
                Outcome::Accepted {
                    origin,
                    interest,
                    branches,
                } => Some((*origin, interest.clone(), branches.clone())),
                Outcome::Rejected { .. } => None,
            })
            .collect();
        // R decoded two real frames and attributed the AUTHENTICATED
        // routed origins — A and C, never the relays X/Y that
        // delivered the final hop.
        let mut origins: Vec<u64> = accepted.iter().map(|(origin, _, _)| *origin).collect();
        origins.sort_unstable();
        origins.dedup();
        let mut expected_origins = vec![nid_a, nid_c];
        expected_origins.sort_unstable();
        assert_eq!(origins, expected_origins, "authenticated origins");
        assert!(
            !accepted.iter().any(|(o, _, _)| *o == nid_x || *o == nid_y),
            "a transport relay must never be attributed as a consumer",
        );
        // Identical RE-DERIVED digests — the identity the leader
        // computed itself, matching the local spec.
        for (_, interest, branches) in &accepted {
            assert_eq!(interest.interest_digest, expected_digest);
            assert_eq!(branches, &vec![PROVIDER], "one bounded branch");
        }
    }
    {
        let leader = leader.lock();
        assert_eq!(leader.interest_count(), 1, "ONE coalesced interest row");
        let key = CapabilityInterestKey {
            capability_id: spec.capability_id.clone(),
            interest_digest: expected_digest,
        };
        assert_eq!(leader.branches(&key), vec![PROVIDER], "one bounded branch");
        assert_eq!(leader.relay.table.len(), 1, "one branch table entry");
        // The subscriber rows are exactly the authenticated
        // consumers; neither relay's node id ever became a
        // downstream.
        let branch = ProviderInterestKey::new(key, PROVIDER);
        let mut downstreams = leader.relay.table.downstreams(&branch, Instant::now());
        downstreams.sort_by_key(|d| match d {
            DownstreamId::Local | DownstreamId::Leader => (0, 0),
            DownstreamId::Peer(id) => (1, *id),
        });
        let mut expected_rows = vec![DownstreamId::Peer(nid_a), DownstreamId::Peer(nid_c)];
        expected_rows.sort_by_key(|d| match d {
            DownstreamId::Local | DownstreamId::Leader => (0, 0),
            DownstreamId::Peer(id) => (1, *id),
        });
        assert_eq!(
            downstreams, expected_rows,
            "downstream rows are exactly Peer(A) and Peer(C)",
        );
    }

    // ── The negative case: a wire scope claim the session doesn't
    // back ─────────────────────────────────────────────────────
    // A (authenticated, in-fleet) sends an internally consistent
    // frame whose audience/scope claims the FOREIGN root. The claim
    // passes digest re-derivation (it is honest about its own
    // fields) and must then be rejected by scope validation against
    // the SESSION-proven root — protocol-invalid input.
    let mut foreign_spec = spec.clone();
    foreign_spec.audience = foreign_root;
    let bad_frame = SensingInterestFrame::capability_registration(
        &foreign_spec,
        Duration::from_millis(100),
        TTL,
        nid_a,
    );
    let rejected_from_a = |outcomes: &Arc<Mutex<Vec<Outcome>>>| {
        outcomes.lock().iter().find_map(|o| match o {
            Outcome::Rejected { origin, rejection } if *origin == nid_a => Some(*rejection),
            _ => None,
        })
    };
    for attempt in 0..20 {
        if rejected_from_a(&outcomes).is_none() {
            node_a
                .send_routed(nid_r, &frame_batch(&bad_frame))
                .await
                .expect("A routes the bad frame");
        }
        if wait_until(
            || rejected_from_a(&outcomes).is_some(),
            Duration::from_millis(500),
        )
        .await
        {
            break;
        }
        assert!(attempt < 19, "the forged-scope frame never surfaced");
    }
    let rejection = rejected_from_a(&outcomes).unwrap();
    assert_eq!(
        rejection,
        FrameRejection::Scope(ScopeError::WireClaimMismatch),
        "a wire-claimed scope the session does not back is refused",
    );
    assert!(rejection.is_security_relevant());
    assert!(
        SensingCounters::get(&counters.protocol_invalid) >= 1,
        "security counter moved",
    );
    assert!(
        SensingCounters::get(&counters.scope_refusals) >= 1,
        "scope refusal counter moved",
    );
    assert_eq!(
        leader.lock().interest_count(),
        1,
        "the rejected frame registered nothing",
    );

    // The rejection surfaces as an error response to the caller,
    // over the same routed path (R → X → A).
    node_r
        .send_routed(
            nid_a,
            &payload_batch(serde_json::json!({
                "tag": "sensing.rejection",
                "detail": rejection.to_string(),
            })),
        )
        .await
        .expect("R routes the error response to A");
    let mut sink_a = Vec::new();
    let err_response = recv_tagged(&node_a, &mut sink_a, "sensing.rejection", TTL)
        .await
        .expect("A receives the error response");
    assert!(
        err_response["detail"]
            .as_str()
            .unwrap()
            .contains("wire-claimed root"),
        "the machine response carries the scope refusal: {err_response}",
    );

    // ── Provider proof fans back to BOTH consumers ──────────────
    // The provider signs one attestation; the leader's relay fans
    // the identical proof to every registered downstream — the
    // authenticated consumers, not the ingress relays.
    let key = CapabilityInterestKey {
        capability_id: spec.capability_id.clone(),
        interest_digest: expected_digest,
    };
    let proof = Attestation::new(
        ProviderObservationKey::new(key.clone(), PROVIDER, 1),
        Incarnation::new(1),
        AttestedStatus::Ready,
        Some(Duration::from_millis(300)),
        1,
        Duration::from_millis(100),
    );
    let deliveries = leader.lock().on_attestation(Instant::now(), &proof, true);
    let to_a = deliveries
        .iter()
        .find(|d| d.to == DownstreamId::Peer(nid_a))
        .expect("the leader fans the proof to A");
    let to_c = deliveries
        .iter()
        .find(|d| d.to == DownstreamId::Peer(nid_c))
        .expect("the leader fans the proof to C");
    assert_eq!(
        to_a.attestation.fingerprint, to_c.attestation.fingerprint,
        "identical signed proof to both consumers",
    );
    assert!(
        deliveries
            .iter()
            .all(|d| d.to == DownstreamId::Peer(nid_a) || d.to == DownstreamId::Peer(nid_c)),
        "no delivery ever targets a transport relay",
    );

    // Route the proof payloads back over the real path (R → X → A,
    // R → Y → C) and assert receipt at both consumers.
    let proof_payload = |delivery: &net::adapter::net::behavior::sensing::Delivery| {
        serde_json::json!({
            "tag": "sensing.proof",
            "provider": PROVIDER,
            "status": "ready",
            // Digest256 serializes as its hex string.
            "fingerprint": serde_json::to_value(delivery.attestation.fingerprint).unwrap(),
        })
    };
    node_r
        .send_routed(nid_a, &payload_batch(proof_payload(to_a)))
        .await
        .expect("R routes the proof to A");
    node_r
        .send_routed(nid_c, &payload_batch(proof_payload(to_c)))
        .await
        .expect("R routes the proof to C");
    let got_a = recv_tagged(&node_a, &mut sink_a, "sensing.proof", TTL)
        .await
        .expect("A receives the fanned proof");
    let mut sink_c = Vec::new();
    let got_c = recv_tagged(&node_c, &mut sink_c, "sensing.proof", TTL)
        .await
        .expect("C receives the fanned proof");
    assert_eq!(got_a, got_c, "both consumers hold the identical proof");

    // ── Relay opacity ───────────────────────────────────────────
    // X and Y forwarded every frame and every proof header-only:
    // they never decrypted, never dispatched, and never decoded a
    // sensing frame (the sensing_fallback opacity assertion, on both
    // shards).
    for (name, relay) in [("X", &node_x), ("Y", &node_y)] {
        for shard in 0..2u16 {
            let polled = relay.poll_shard(shard, None, 100).await.unwrap();
            assert_eq!(
                polled.events.len(),
                0,
                "relay {name} must forward opaquely (shard {shard})",
            );
        }
    }
    assert_eq!(
        *decode_failures.lock(),
        0,
        "every event that reached the leader intake was a real frame",
    );

    for node in [&node_a, &node_x, &node_r, &node_c, &node_y] {
        node.shutdown().await.unwrap();
    }
}
