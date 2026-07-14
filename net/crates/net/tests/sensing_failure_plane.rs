//! SI-5 (SENSING_INTEREST_COALESCING_PLAN §4.8): failure-plane
//! integration, end-to-end on real sessions — per-provider expiry,
//! local aggregate recompute, and re-registration.
//!
//! Every scenario uses LONG-ttl rows and continuity windows far
//! wider than the failure-detection latency, so a disruption
//! observed inside the discrimination deadline is unambiguously the
//! EVENT-DRIVEN failure hook — never the ttl sweep, never the
//! natural window expiry.
//!
//! Run: `cargo test --test sensing_failure_plane`

#![cfg(feature = "net")]

mod common;
use common::*;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::behavior::sensing::{
    encode_attestation, sign_attestation, AttestedStatus, AudienceScopeCommitment,
    CanonicalConstraints, CapabilityId, Continuity, DisclosureClass, EvaluationRequest,
    Incarnation, InterestSpec, ProjectedReadiness, ProviderInterestKey, ProviderSelector,
    ReadinessEvaluation, ReadinessEvaluator, ResultMode, StatusReason, UnsignedAttestation,
    WorkLatencyEnvelope, SUBPROTOCOL_READINESS_ATTESTATION,
};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};
use net::adapter::Adapter;

/// Rows must outlive every scenario, so a row's disappearance is
/// unambiguously the failure hook, never the sweep.
const LONG_TTL: Duration = Duration::from_secs(10);
/// Consumer D: window = 3 × 2 s = 6 s — natural expiry sits far
/// beyond the ~1–2 s failure-detection latency.
const D: Duration = Duration::from_secs(2);
/// The failure-detector edge under test rides this timeout.
const FD_TIMEOUT: Duration = Duration::from_millis(300);
/// The epoch test wants NO failure-detector interference — crafted
/// beats are its only events.
const NO_FD: Duration = Duration::from_secs(10);

fn base_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, CHAOS_PSK)
        .with_heartbeat_interval(Duration::from_millis(100))
        .with_handshake(3, Duration::from_secs(2));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: CHAOS_BUFFER_SIZE,
        recv_buffer_size: CHAOS_BUFFER_SIZE,
    };
    cfg
}

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

struct AlwaysReady;

impl ReadinessEvaluator for AlwaysReady {
    fn evaluate(&self, _request: &EvaluationRequest<'_>) -> ReadinessEvaluation {
        ReadinessEvaluation::Ready {
            estimated_start: Some(Duration::from_millis(3)),
        }
    }
}

async fn sensing_node(
    kp: EntityKeypair,
    fleet: AudienceScopeCommitment,
    incarnation: Option<Incarnation>,
    session_timeout: Duration,
) -> Arc<MeshNode> {
    let mut cfg = base_config()
        .with_session_timeout(session_timeout)
        .with_sensing_coalescing(true)
        .with_sensing_owner_root(fleet);
    if let Some(incarnation) = incarnation {
        cfg = cfg.with_sensing_incarnation(incarnation);
    }
    Arc::new(MeshNode::new(kp, cfg).await.expect("MeshNode::new"))
}

/// §4.8 item 1 (direct) + item 2, and the recovery half: a failed
/// provider's observations expire EVENT-DRIVEN — inside the failure-
/// detection latency, far ahead of the 6 s continuity window — the
/// overlay signal fires, the origin retires the dead consumer's
/// stream far ahead of the 10 s row ttl, and after the heal the
/// ordinary soft-state machinery (re-announce pins, re-registration)
/// re-establishes Ready end-to-end.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_failure_expires_observations_and_recovery_re_establishes() {
    let fleet_kp = EntityKeypair::generate();
    let fleet = AudienceScopeCommitment::owner_root(fleet_kp.entity_id());
    let c = sensing_node(EntityKeypair::generate(), fleet, None, FD_TIMEOUT).await;
    let p = sensing_node(
        EntityKeypair::generate(),
        fleet,
        Some(Incarnation::new(1)),
        FD_TIMEOUT,
    )
    .await;
    p.register_readiness_evaluator(CapabilityId::new("print.document"), Arc::new(AlwaysReady));

    connect_pair(&c, &p).await;
    c.start();
    p.start();
    for node in [&c, &p] {
        node.announce_capabilities(CapabilitySet::new())
            .await
            .expect("announce");
    }
    let (c_id, p_id) = (c.node_id(), p.node_id());
    await_condition(Duration::from_secs(5), "entity pins established", || {
        c.peer_entity_id(p_id).is_some() && p.peer_entity_id(c_id).is_some()
    })
    .await;

    let spec = shared_spec(fleet);
    let branch = ProviderInterestKey::new(spec.key(), p_id);
    c.register_sensing_interest(&spec, p_id, D, LONG_TTL)
        .expect("register");
    await_condition(Duration::from_secs(10), "established + Ready", || {
        c.sensing_upstream_continuity(&branch) == Some(Continuity::Established)
            && c.sensing_projected(&branch) == ProjectedReadiness::Ready
    })
    .await;
    let overlay = c.subscribe_sensing_overlay_changes();
    let generation_before = *overlay.borrow();

    // ── The failure: P vanishes ──
    let partitioned_at = std::time::Instant::now();
    chaos_partition(&c, &p);
    // Event-driven expiry at C: the continuity window (6 s) could
    // not have lapsed inside this deadline — only the failure hook
    // expires here.
    await_condition(
        Duration::from_secs(4),
        "observations expire on the failure edge",
        || {
            c.sensing_upstream_continuity(&branch) == Some(Continuity::Expired)
                && c.sensing_projected(&branch) == ProjectedReadiness::Unknown
        },
    )
    .await;
    assert!(
        partitioned_at.elapsed() < Duration::from_secs(5),
        "disruption must be event-driven",
    );
    assert!(
        *overlay.borrow() > generation_before,
        "a disappearing projection fires the overlay signal",
    );
    // Event-driven downstream loss at P: C's row drops with the
    // failure edge and the stream retires — far ahead of the 10 s
    // row ttl.
    await_condition(
        Duration::from_secs(4),
        "origin retires the dead consumer's stream",
        || p.sensing_live_streams() == 0,
    )
    .await;

    // ── The heal: ordinary soft-state machinery recovers ──
    chaos_heal(&c, &p);
    // Heartbeats resume on the next tick; wait for the failure
    // detector's recovery edge before driving re-announcement.
    await_peer_recovered(&c, p_id, Duration::from_secs(10)).await;
    await_peer_recovered(&p, c_id, Duration::from_secs(10)).await;
    // Failure dropped both entity pins; re-announcing re-pins (the
    // same TOFU path any recovery rides). Re-announce until both
    // pins are back — a frame racing the heal can be lost.
    await_condition(Duration::from_secs(10), "pins re-established", || {
        if c.peer_entity_id(p_id).is_some() && p.peer_entity_id(c_id).is_some() {
            return true;
        }
        let (c, p) = (c.clone(), p.clone());
        tokio::spawn(async move {
            let _ = c.announce_capabilities(CapabilitySet::new()).await;
            let _ = p.announce_capabilities(CapabilitySet::new()).await;
        });
        false
    })
    .await;
    // Re-registration along the recovered path.
    await_condition(Duration::from_secs(15), "recovery re-establishes", || {
        let _ = c.register_sensing_interest(&spec, p_id, D, LONG_TTL);
        c.sensing_projected(&branch) == ProjectedReadiness::Ready
    })
    .await;

    c.shutdown().await.expect("shutdown C");
    p.shutdown().await.expect("shutdown P");
}

/// §4.8 item 1 (multi-hop): `next_hop(P)` Failed — the failed peer
/// is NOT the provider, merely the first hop the stream rides —
/// must expire the provider's observations exactly like a direct
/// failure. Before SI-5 only the failed peer itself was a sensing
/// event.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn next_hop_failure_disrupts_multi_hop_provider_branches() {
    let fleet_kp = EntityKeypair::generate();
    let fleet = AudienceScopeCommitment::owner_root(fleet_kp.entity_id());
    let c = sensing_node(EntityKeypair::generate(), fleet, None, FD_TIMEOUT).await;
    let r = sensing_node(EntityKeypair::generate(), fleet, None, FD_TIMEOUT).await;
    let p = sensing_node(
        EntityKeypair::generate(),
        fleet,
        Some(Incarnation::new(1)),
        FD_TIMEOUT,
    )
    .await;
    p.register_readiness_evaluator(CapabilityId::new("print.document"), Arc::new(AlwaysReady));

    connect_pair(&c, &r).await;
    connect_pair(&r, &p).await;
    for node in [&c, &r, &p] {
        node.start();
    }
    for node in [&c, &r, &p] {
        node.announce_capabilities(CapabilitySet::new())
            .await
            .expect("announce");
    }
    let (c_id, r_id, p_id) = (c.node_id(), r.node_id(), p.node_id());
    await_condition(Duration::from_secs(5), "entity pins established", || {
        c.peer_entity_id(r_id).is_some()
            && r.peer_entity_id(c_id).is_some()
            && r.peer_entity_id(p_id).is_some()
            && p.peer_entity_id(r_id).is_some()
    })
    .await;
    // The verifying-hop origin pin (the SI-3 seam bound): C verifies
    // P's signatures across the R hop.
    let p_entity = p.entity_keypair().entity_id().clone();
    c.test_pin_peer_entity(p_id, p_entity);

    let spec = shared_spec(fleet);
    let branch = ProviderInterestKey::new(spec.key(), p_id);
    // C's route to P resolves through R.
    await_condition(Duration::from_secs(5), "C routes to P via R", || {
        c.register_sensing_interest(&spec, p_id, D, LONG_TTL)
            .is_ok()
            && c.sensing_upstream_continuity(&branch) == Some(Continuity::Established)
            && c.sensing_projected(&branch) == ProjectedReadiness::Ready
    })
    .await;

    // ── The failure: R (the next hop toward P) vanishes from C ──
    let partitioned_at = std::time::Instant::now();
    chaos_partition(&c, &r);
    await_condition(
        Duration::from_secs(4),
        "next_hop failure expires the provider's observations",
        || {
            c.sensing_upstream_continuity(&branch) == Some(Continuity::Expired)
                && c.sensing_projected(&branch) == ProjectedReadiness::Unknown
        },
    )
    .await;
    assert!(
        partitioned_at.elapsed() < Duration::from_secs(5),
        "disruption must be event-driven, not the 6 s window",
    );

    c.shutdown().await.expect("shutdown C");
    r.shutdown().await.expect("shutdown R");
    p.shutdown().await.expect("shutdown P");
}

/// §4.8 item 1 (RT-5 withdrawal): the provider dies TWO hops away —
/// the consumer's own failure detector never fires (its direct peer
/// X stays healthy; P was never direct), so only the received
/// route-withdrawal can expire the provider's observations. X's
/// detector fails P, X floods "P unreachable via me", C drops its
/// (P, via X) route — and the sensing branch toward P expires with
/// it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn route_withdrawal_disrupts_provider_branches_at_remote_consumers() {
    let fleet_kp = EntityKeypair::generate();
    let fleet = AudienceScopeCommitment::owner_root(fleet_kp.entity_id());
    let c = sensing_node(EntityKeypair::generate(), fleet, None, FD_TIMEOUT).await;
    // X detects SLOWER than C's route age-out (3 × 300 ms = 900 ms),
    // so the withdrawal deterministically arrives AFTER C's route to
    // P is gone — pinning the ROUTELESS fallback (the SI-5 review P1
    // path), not the route-drop path.
    let x = sensing_node(
        EntityKeypair::generate(),
        fleet,
        None,
        Duration::from_millis(700),
    )
    .await;
    let p = sensing_node(
        EntityKeypair::generate(),
        fleet,
        Some(Incarnation::new(1)),
        FD_TIMEOUT,
    )
    .await;
    p.register_readiness_evaluator(CapabilityId::new("print.document"), Arc::new(AlwaysReady));

    connect_pair(&c, &x).await;
    connect_pair(&x, &p).await;
    for node in [&c, &x, &p] {
        node.start();
    }
    for node in [&c, &x, &p] {
        node.announce_capabilities(CapabilitySet::new())
            .await
            .expect("announce");
    }
    let (c_id, x_id, p_id) = (c.node_id(), x.node_id(), p.node_id());
    await_condition(Duration::from_secs(5), "entity pins established", || {
        c.peer_entity_id(x_id).is_some()
            && x.peer_entity_id(c_id).is_some()
            && x.peer_entity_id(p_id).is_some()
            && p.peer_entity_id(x_id).is_some()
    })
    .await;
    let p_entity = p.entity_keypair().entity_id().clone();
    c.test_pin_peer_entity(p_id, p_entity);

    // SI-5 review P1: C ALSO holds a RELAY-ROUTED session to P — its
    // PeerInfo carries X's address, so `peers.contains_key(P)` is
    // true throughout while no direct session to P exists. The old
    // routeless fallback misread that as "live direct session" and
    // skipped disruption whenever the route had aged out before the
    // withdrawal arrived.
    let x_bind = x.local_addr();
    let p_pub = *p.public_key();
    c.connect_via(x_bind, &p_pub, p_id)
        .await
        .expect("relay-routed connect_via");
    assert_eq!(
        c.peer_addr(p_id),
        Some(x_bind),
        "precondition: C's session to P rides the relay",
    );

    let spec = shared_spec(fleet);
    let branch = ProviderInterestKey::new(spec.key(), p_id);
    await_condition(Duration::from_secs(5), "C senses P via X", || {
        c.register_sensing_interest(&spec, p_id, D, LONG_TTL)
            .is_ok()
            && c.sensing_upstream_continuity(&branch) == Some(Continuity::Established)
            && c.sensing_projected(&branch) == ProjectedReadiness::Ready
    })
    .await;

    // ── The failure: P dies where only X can see it ──
    let partitioned_at = std::time::Instant::now();
    chaos_partition(&x, &p);
    await_condition(
        Duration::from_secs(5),
        "the received withdrawal expires the provider's observations",
        || {
            c.sensing_upstream_continuity(&branch) == Some(Continuity::Expired)
                && c.sensing_projected(&branch) == ProjectedReadiness::Unknown
        },
    )
    .await;
    assert!(
        partitioned_at.elapsed() < Duration::from_secs(5),
        "disruption must ride the withdrawal, not the 6 s window",
    );

    c.shutdown().await.expect("shutdown C");
    x.shutdown().await.expect("shutdown X");
    p.shutdown().await.expect("shutdown P");
}

/// §4.8 items 3+4: an epoch move on ONE branch supersedes the
/// origin's SIBLING branches immediately — a cross-digest cell must
/// not keep vouching under the old boot (incarnation) or the old
/// definition (generation) until its own next beat happens along.
/// The arriving beat's own branch re-establishes through the
/// ordinary cell semantics; the interests and branches survive.
#[tokio::test]
async fn epoch_supersession_disrupts_sibling_branches() {
    let fleet_kp = EntityKeypair::generate();
    let fleet = AudienceScopeCommitment::owner_root(fleet_kp.entity_id());
    let c = sensing_node(EntityKeypair::generate(), fleet, None, NO_FD).await;
    // P's origin role stays DARK (no incarnation): every beat below
    // is hand-crafted, so the epoch axes are exact.
    let p = sensing_node(EntityKeypair::generate(), fleet, None, NO_FD).await;

    connect_pair(&c, &p).await;
    c.start();
    p.start();
    for node in [&c, &p] {
        node.announce_capabilities(CapabilitySet::new())
            .await
            .expect("announce");
    }
    let (c_id, p_id) = (c.node_id(), p.node_id());
    await_condition(Duration::from_secs(5), "entity pins established", || {
        c.peer_entity_id(p_id).is_some() && p.peer_entity_id(c_id).is_some()
    })
    .await;

    let spec_a = shared_spec(fleet);
    let spec_b = {
        let mut spec = shared_spec(fleet);
        spec.constraints = CanonicalConstraints::from_entries([("media", "letter")]).unwrap();
        spec
    };
    let branch_a = ProviderInterestKey::new(spec_a.key(), p_id);
    let branch_b = ProviderInterestKey::new(spec_b.key(), p_id);
    c.register_sensing_interest(&spec_a, p_id, D, LONG_TTL)
        .expect("register A");
    c.register_sensing_interest(&spec_b, p_id, D, LONG_TTL)
        .expect("register B");

    let c_addr = c.local_addr();
    let craft = |digest: &InterestSpec, incarnation: u64, generation: u64, seq: u64| {
        let unsigned = UnsignedAttestation {
            interest_digest: digest.interest_digest(),
            origin: p_id,
            origin_incarnation: Incarnation::new(incarnation),
            capability_id: CapabilityId::new("print.document"),
            capability_generation: generation,
            status: AttestedStatus::Ready,
            status_reason: StatusReason::None,
            estimated_start: None,
            seq,
            promised_cadence: Duration::from_millis(1000),
            audience_scope: fleet,
        };
        let signed = sign_attestation(p.entity_keypair(), unsigned).expect("sign");
        encode_attestation(&signed).expect("encode")
    };
    let send = |bytes: Vec<u8>| {
        let p = p.clone();
        async move {
            p.send_subprotocol(c_addr, SUBPROTOCOL_READINESS_ATTESTATION, &bytes)
                .await
                .expect("send");
        }
    };

    // ── Establish BOTH branches under (incarnation 5, gen 1) ──
    for seq in 1..=2u64 {
        send(craft(&spec_a, 5, 1, seq)).await;
        send(craft(&spec_b, 5, 1, seq)).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    await_condition(Duration::from_secs(5), "both branches established", || {
        c.sensing_upstream_continuity(&branch_a) == Some(Continuity::Established)
            && c.sensing_upstream_continuity(&branch_b) == Some(Continuity::Established)
            && c.sensing_projected(&branch_b) == ProjectedReadiness::Ready
    })
    .await;

    // ── Incarnation axis: ONE beat on A under incarnation 6 ──
    // B receives nothing, yet must expire immediately (its own
    // window runs 6 s — only the epoch hook can expire it here).
    send(craft(&spec_a, 6, 1, 1)).await;
    await_condition(
        Duration::from_secs(2),
        "sibling branch expires on the incarnation move",
        || {
            c.sensing_upstream_continuity(&branch_b) == Some(Continuity::Expired)
                && c.sensing_projected(&branch_b) == ProjectedReadiness::Unknown
        },
    )
    .await;
    // The arriving branch itself re-established from its live
    // new-incarnation beat.
    assert_eq!(
        c.sensing_upstream_continuity(&branch_a),
        Some(Continuity::Established),
        "the epoch-advancing beat's own branch re-establishes",
    );

    // ── SI-5 review P0 (reviewer-reproduced): a NEWER-SEQ B beat
    //    from the SUPERSEDED incarnation 5 — validly signed and
    //    admissible at its own (origin, digest, incarnation) gate —
    //    must be dropped at the provider-wide epoch check, never
    //    resurrect the sibling the supersession expired ──
    send(craft(&spec_b, 5, 1, 10)).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        c.sensing_upstream_continuity(&branch_b),
        Some(Continuity::Expired),
        "a globally stale incarnation must not revive a sibling branch",
    );
    assert_eq!(c.sensing_projected(&branch_b), ProjectedReadiness::Unknown);

    // ── Re-establish B under the new incarnation ──
    for seq in 2..=3u64 {
        send(craft(&spec_b, 6, 1, seq)).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    await_condition(Duration::from_secs(5), "B re-establishes", || {
        c.sensing_upstream_continuity(&branch_b) == Some(Continuity::Established)
    })
    .await;

    // ── Generation axis: ONE beat on A under (inc 6, gen 2) ──
    send(craft(&spec_a, 6, 2, 2)).await;
    await_condition(
        Duration::from_secs(2),
        "sibling branch expires on the generation move",
        || c.sensing_upstream_continuity(&branch_b) == Some(Continuity::Expired),
    )
    .await;
    // ── SI-5 review P0, generation axis: a newer-seq B beat from
    //    the superseded (6, 1) definition is equally dead ──
    send(craft(&spec_b, 6, 1, 4)).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        c.sensing_upstream_continuity(&branch_b),
        Some(Continuity::Expired),
        "a globally stale generation must not revive a sibling branch",
    );

    // §4.8: the interests and branches themselves SURVIVE the epoch
    // move — fresh new-epoch beats resume them without any
    // re-registration.
    send(craft(&spec_b, 6, 2, 5)).await;
    await_condition(
        Duration::from_secs(5),
        "B resumes under the new epoch",
        || c.sensing_upstream_continuity(&branch_b) == Some(Continuity::Established),
    )
    .await;

    c.shutdown().await.expect("shutdown C");
    p.shutdown().await.expect("shutdown P");
}

/// §4.8 item 2 at the LEADER (the second-relay lesson applied ahead
/// of review): a failed provider-free consumer's leader-relay rows
/// drop event-driven — the coalesced interest drains, the mesh
/// Leader row deregisters, and the provider retires its stream far
/// ahead of the 10 s row ttl.
#[cfg(feature = "redex")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn consumer_failure_drains_leader_demand() {
    //   A (provider-free) — R (leader) — P (owner identity)
    let owner_kp = EntityKeypair::generate();
    let owner_entity = owner_kp.entity_id().clone();
    let owner = AudienceScopeCommitment::owner_root(&owner_entity);
    let a = sensing_node(EntityKeypair::generate(), owner, None, FD_TIMEOUT).await;
    let r = sensing_node(EntityKeypair::generate(), owner, None, FD_TIMEOUT).await;
    let p = sensing_node(owner_kp, owner, Some(Incarnation::new(1)), FD_TIMEOUT).await;
    p.register_readiness_evaluator(CapabilityId::new("print.document"), Arc::new(AlwaysReady));

    connect_pair(&a, &r).await;
    connect_pair(&r, &p).await;
    for node in [&a, &r, &p] {
        node.start();
    }
    for node in [&a, &r] {
        node.announce_capabilities(CapabilitySet::new())
            .await
            .expect("announce");
    }
    p.announce_capabilities(CapabilitySet::new().add_tag("print.document"))
        .await
        .expect("announce P");

    let (a_id, r_id, p_id) = (a.node_id(), r.node_id(), p.node_id());
    await_condition(Duration::from_secs(5), "entity pins established", || {
        r.peer_entity_id(a_id).is_some()
            && r.peer_entity_id(p_id).is_some()
            && p.peer_entity_id(r_id).is_some()
    })
    .await;
    a.test_pin_peer_entity(p_id, owner_entity.clone());

    assert!(r.assume_sensing_leader(), "leader role installs at R");
    let cap = CapabilityId::new("print.document");
    await_condition(Duration::from_secs(5), "R's snapshot authorizes P", || {
        r.sensing_candidate_snapshot(&cap)
            .iter()
            .any(|candidate| candidate.node_id == p_id && candidate.authorized)
    })
    .await;

    let spec = shared_spec(owner);
    let branch = ProviderInterestKey::new(spec.key(), p_id);
    // ONE long-ttl registration — no refresher, so nothing below can
    // be soft-state expiry.
    await_condition(Duration::from_secs(10), "provider-free path live", || {
        let _ = a.register_capability_interest(&spec, r_id, D, LONG_TTL);
        r.sensing_leader_interest_count() == Some(1)
            && p.sensing_live_streams() == 1
            && a.sensing_projected(&branch) == ProjectedReadiness::Ready
    })
    .await;

    // ── The failure: the consumer vanishes from the leader ──
    let partitioned_at = std::time::Instant::now();
    chaos_partition(&a, &r);
    await_condition(
        Duration::from_secs(5),
        "leader demand drains on the failure edge",
        || r.sensing_leader_interest_count() == Some(0),
    )
    .await;
    await_condition(
        Duration::from_secs(5),
        "provider retires the drained stream",
        || p.sensing_live_streams() == 0,
    )
    .await;
    assert!(
        partitioned_at.elapsed() < Duration::from_secs(7),
        "the drain must be event-driven, never the 10 s row ttl",
    );

    a.shutdown().await.expect("shutdown A");
    r.shutdown().await.expect("shutdown R");
    p.shutdown().await.expect("shutdown P");
}
