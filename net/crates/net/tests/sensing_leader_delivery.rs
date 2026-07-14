//! SI-4 review P0 witness: the PROVIDER-FREE production path,
//! end-to-end on real sessions.
//!
//! ```text
//!   A (D = 100 ms) ─┐
//!                   R (leader) ── P (origin, holds the owner identity)
//!   B (D = 500 ms) ─┘
//! ```
//!
//! A and B register provider-free capability interests
//! (`register_capability_interest`, real 0x0C02
//! `CapabilityRegistration` frames) with the installed leader R; R
//! re-derives + coalesces them, resolves P from its LIVE fold (P's
//! real capability announcement), registers its coalesced demand as
//! the LEADER row, and propagates one `ProviderRegistration`
//! upstream. P runs ONE signed stream back to R; R's 0x0C03 intake
//! dispatches the Leader row to `SensingLeader::on_attestation`,
//! which fans the identical signed bytes to the REAL consumer rows —
//! and both A and B receive, verify, and project the proof through
//! their digest-level provider-free expectations.
//!
//! Identity note: P is constructed WITH the owner (fleet) keypair —
//! the v1 single-owner shape where the provider is the owner's own
//! device. Its announcement therefore pins the owner entity at R, so
//! §4.10 snapshot authorization and attestation signature
//! verification agree on the same identity.
//!
//! Run: `cargo test --features redex --test sensing_leader_delivery`

#![cfg(all(feature = "net", feature = "redex"))]

mod common;
use common::*;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::behavior::sensing::{
    AttestedStatus, AudienceScopeCommitment, CanonicalConstraints, CapabilityId, DownstreamId,
    EvaluationRequest, Incarnation, InterestSpec, ProjectedReadiness, ProviderInterestKey,
    ProviderSelector, ReadinessEvaluation, ReadinessEvaluator, ResultMode, SensingCounters,
    StatusReason, WorkLatencyEnvelope,
};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};
use net::adapter::Adapter;

const TTL: Duration = Duration::from_millis(1500);
const REFRESH: Duration = Duration::from_millis(750);

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

fn shared_spec(owner: AudienceScopeCommitment) -> InterestSpec {
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
use net::adapter::net::behavior::sensing::DisclosureClass;

struct AlwaysReady;

impl ReadinessEvaluator for AlwaysReady {
    fn evaluate(&self, _request: &EvaluationRequest<'_>) -> ReadinessEvaluation {
        ReadinessEvaluation::Ready {
            estimated_start: Some(Duration::from_millis(3)),
        }
    }
}

#[tokio::test]
async fn provider_free_proofs_fan_back_through_the_leader() {
    // P holds the OWNER identity (module docs).
    let owner_kp = EntityKeypair::generate();
    let owner_entity = owner_kp.entity_id().clone();
    let owner = AudienceScopeCommitment::owner_root(&owner_entity);

    let mk = |kp: EntityKeypair, incarnation: Option<Incarnation>| async move {
        let mut cfg = base_config()
            .with_sensing_coalescing(true)
            .with_sensing_owner_root(owner);
        if let Some(incarnation) = incarnation {
            cfg = cfg.with_sensing_incarnation(incarnation);
        }
        Arc::new(MeshNode::new(kp, cfg).await.expect("MeshNode::new"))
    };
    let a = mk(EntityKeypair::generate(), None).await;
    let b = mk(EntityKeypair::generate(), None).await;
    let r = mk(EntityKeypair::generate(), None).await;
    let p = mk(owner_kp, Some(Incarnation::new(1))).await;
    p.register_readiness_evaluator(CapabilityId::new("print.document"), Arc::new(AlwaysReady));

    connect_pair(&a, &r).await;
    connect_pair(&b, &r).await;
    connect_pair(&r, &p).await;
    for node in [&a, &b, &r, &p] {
        node.start();
    }
    // P's REAL capability announcement is the fold input the
    // leader's snapshot resolves from.
    for node in [&a, &b, &r] {
        node.announce_capabilities(CapabilitySet::new())
            .await
            .expect("announce");
    }
    p.announce_capabilities(CapabilitySet::new().add_tag("print.document"))
        .await
        .expect("announce P");

    let (a_id, b_id, r_id, p_id) = (a.node_id(), b.node_id(), r.node_id(), p.node_id());
    await_condition(Duration::from_secs(5), "entity pins established", || {
        r.peer_entity_id(a_id).is_some()
            && r.peer_entity_id(b_id).is_some()
            && r.peer_entity_id(p_id).is_some()
            && p.peer_entity_id(r_id).is_some()
    })
    .await;
    // The verifying-hop origin pin (the documented SI-3 seam bound):
    // A and B verify P's signatures against the owner identity.
    a.test_pin_peer_entity(p_id, owner_entity.clone());
    b.test_pin_peer_entity(p_id, owner_entity.clone());

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
    let interest = spec.key();

    // ── Provider-free registrations from BOTH consumers ──
    let refresher = |node: Arc<MeshNode>, interval: Duration| {
        let spec = spec.clone();
        tokio::spawn(async move {
            loop {
                let _ = node.register_capability_interest(&spec, r_id, interval, TTL);
                tokio::time::sleep(REFRESH).await;
            }
        })
    };
    let refresh_a = refresher(a.clone(), Duration::from_millis(100));
    let refresh_b = refresher(b.clone(), Duration::from_millis(500));

    // The leader coalesces both onto ONE interest, resolves P, and
    // holds its coalesced demand as the LEADER row.
    await_condition(Duration::from_secs(10), "leader coalesces", || {
        r.sensing_leader_interest_count() == Some(1)
    })
    .await;
    assert_eq!(
        r.sensing_leader_branches(&interest),
        Some(vec![p_id]),
        "the leader resolved the real announced provider",
    );
    await_condition(Duration::from_secs(10), "Leader row at R", || {
        r.sensing_downstreams(&branch) == vec![DownstreamId::Leader]
    })
    .await;
    await_condition(Duration::from_secs(10), "one stream at P", || {
        p.sensing_live_streams() == 1
    })
    .await;

    // ── THE P0 WITNESS: real signed 0x0C03 proofs return through R
    //    and fan to BOTH provider-free consumers ──
    await_condition(Duration::from_secs(10), "A receives the proof", || {
        a.sensing_latest_attestation(&branch)
            .is_some_and(|proof| proof.origin == p_id && proof.status == AttestedStatus::Ready)
    })
    .await;
    await_condition(Duration::from_secs(10), "B receives the proof", || {
        b.sensing_latest_attestation(&branch)
            .is_some_and(|proof| proof.origin == p_id && proof.status == AttestedStatus::Ready)
    })
    .await;
    // And both PROJECT it: the digest-level provider-free
    // expectation feeds each consumer's own overlay cell.
    await_condition(Duration::from_secs(5), "A projects Ready", || {
        a.sensing_projected(&branch) == ProjectedReadiness::Ready
    })
    .await;
    await_condition(Duration::from_secs(5), "B projects Ready", || {
        b.sensing_projected(&branch) == ProjectedReadiness::Ready
    })
    .await;

    // Nothing on the whole flow was protocol-invalid at any hop.
    for node in [&a, &b, &r, &p] {
        let counters = node.sensing_counters();
        assert_eq!(SensingCounters::get(&counters.protocol_invalid), 0);
        assert_eq!(SensingCounters::get(&counters.scope_refusals), 0);
    }

    // ── Drain: consumers stop; the whole chain unwinds ──
    refresh_a.abort();
    refresh_b.abort();
    await_condition(Duration::from_secs(10), "leader drains", || {
        r.sensing_leader_interest_count() == Some(0)
    })
    .await;
    await_condition(Duration::from_secs(10), "P retires", || {
        p.sensing_live_streams() == 0
    })
    .await;

    a.shutdown().await.expect("shutdown A");
    b.shutdown().await.expect("shutdown B");
    r.shutdown().await.expect("shutdown R");
    p.shutdown().await.expect("shutdown P");
}

#[tokio::test]
async fn provider_free_ttl_half_refreshes_do_not_starve_leader_delivery() {
    // SI-4 re-review P1: `SensingRelay::register_downstream` reset
    // the existing delivery slot and warm-started on EVERY refresh —
    // under D > TTL/2 with ttl/2 refreshes the leader pushed
    // next_due forward forever and cleared pending live work, so a
    // provider-free consumer saw only provisional snapshots and a
    // healthy Ready degraded to permanent Unknown.
    //
    //   A (D = 1200 ms > TTL/2, refreshes every 750 ms) — R — P
    let owner_kp = EntityKeypair::generate();
    let owner_entity = owner_kp.entity_id().clone();
    let owner = AudienceScopeCommitment::owner_root(&owner_entity);

    let mk = |kp: EntityKeypair, incarnation: Option<Incarnation>| async move {
        let mut cfg = base_config()
            .with_sensing_coalescing(true)
            .with_sensing_owner_root(owner);
        if let Some(incarnation) = incarnation {
            cfg = cfg.with_sensing_incarnation(incarnation);
        }
        Arc::new(MeshNode::new(kp, cfg).await.expect("MeshNode::new"))
    };
    let a = mk(EntityKeypair::generate(), None).await;
    let r = mk(EntityKeypair::generate(), None).await;
    let p = mk(owner_kp, Some(Incarnation::new(1))).await;
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
    // D strictly greater than TTL/2 — every refresh lands while the
    // previous delivery window is still open.
    let d = Duration::from_millis(1200);
    let refresh = {
        let a = a.clone();
        let spec = spec.clone();
        tokio::spawn(async move {
            loop {
                let _ = a.register_capability_interest(&spec, r_id, d, TTL);
                tokio::time::sleep(REFRESH).await;
            }
        })
    };

    await_condition(Duration::from_secs(10), "A projects Ready", || {
        a.sensing_projected(&branch) == ProjectedReadiness::Ready
    })
    .await;

    // Hold past the FULL starvation horizon: with the slot reset on
    // every refresh, A's last continuity-bearing delivery is the
    // first edge beat; its own upstream window (3 × 600 ms) carries
    // provisional warm-starts as bearing feeds for at most another
    // 1.8 s, and the consumer window (3 × 1200 ms) lapses at most
    // ~5.1 s after establishment — inside this 6 s hold, a healthy
    // Ready degrades to Unknown. Un-reset schedules keep live
    // deliveries flowing at A's own D and Ready holds throughout.
    let mut seqs = std::collections::HashSet::new();
    for poll in 0..20u32 {
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            a.sensing_projected(&branch),
            ProjectedReadiness::Ready,
            "healthy stream starved at poll {poll}",
        );
        if let Some(proof) = a.sensing_latest_attestation(&branch) {
            seqs.insert(proof.seq);
        }
    }
    assert!(
        seqs.len() >= 2,
        "live deliveries kept flowing at A's own D (distinct seqs: {})",
        seqs.len(),
    );

    refresh.abort();
    a.shutdown().await.expect("shutdown A");
    r.shutdown().await.expect("shutdown R");
    p.shutdown().await.expect("shutdown P");
}

#[tokio::test]
async fn watch_expiry_fully_reclaims_provider_free_observation_state() {
    // SI-4 re-review item 6: a provider-free branch has no
    // provider-keyed table row at the final consumer, so no later
    // table-expiry event cleans its observation state — before the
    // fix, watch expiry removed the expectation and consumer cells
    // but leaked latest/upstream/slots/provider epochs forever,
    // letting provider churn permanently consume
    // MAX_SENSING_OBSERVATIONS.
    //
    //   A (watch, D = 200 ms) — R (leader) — P (owner identity)
    let owner_kp = EntityKeypair::generate();
    let owner_entity = owner_kp.entity_id().clone();
    let owner = AudienceScopeCommitment::owner_root(&owner_entity);

    let mk = |kp: EntityKeypair, incarnation: Option<Incarnation>| async move {
        let mut cfg = base_config()
            .with_sensing_coalescing(true)
            .with_sensing_owner_root(owner);
        if let Some(incarnation) = incarnation {
            cfg = cfg.with_sensing_incarnation(incarnation);
        }
        Arc::new(MeshNode::new(kp, cfg).await.expect("MeshNode::new"))
    };
    let a = mk(EntityKeypair::generate(), None).await;
    let r = mk(EntityKeypair::generate(), None).await;
    let p = mk(owner_kp, Some(Incarnation::new(1))).await;
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
    let refresher = |node: Arc<MeshNode>| {
        let spec = spec.clone();
        tokio::spawn(async move {
            loop {
                let _ =
                    node.register_capability_interest(&spec, r_id, Duration::from_millis(200), TTL);
                tokio::time::sleep(REFRESH).await;
            }
        })
    };

    // ── Materialize the full observation state at A ──
    let refresh_a = refresher(a.clone());
    await_condition(Duration::from_secs(10), "A materializes the branch", || {
        a.sensing_observation_count() == 1
            && a.sensing_provider_epoch_count() == 1
            && a.sensing_projected(&branch) == ProjectedReadiness::Ready
    })
    .await;
    let overlay = a.subscribe_sensing_overlay_changes();
    let generation_before = *overlay.borrow();

    // ── Expiry: the consumer stops refreshing; the watch lapses ──
    refresh_a.abort();
    await_condition(
        Duration::from_secs(10),
        "watch expiry drains ALL observation state",
        || {
            a.sensing_observation_count() == 0
                && a.sensing_provider_epoch_count() == 0
                && a.sensing_projected(&branch) == ProjectedReadiness::Unknown
        },
    )
    .await;
    assert!(
        *overlay.borrow() > generation_before,
        "a disappearing projection fires the overlay signal",
    );

    // ── Reuse: a fresh registration flows end-to-end again ──
    let refresh_a = refresher(a.clone());
    await_condition(Duration::from_secs(10), "re-registration flows again", || {
        a.sensing_observation_count() == 1
            && a.sensing_projected(&branch) == ProjectedReadiness::Ready
    })
    .await;

    refresh_a.abort();
    a.shutdown().await.expect("shutdown A");
    r.shutdown().await.expect("shutdown R");
    p.shutdown().await.expect("shutdown P");
}

#[tokio::test]
async fn provider_refusal_partitions_the_leaders_real_consumer_rows() {
    // SI-4 re-review item 4: the mesh holds ONE aggregate Leader row
    // while the leader relay holds the real per-consumer cadences.
    // Before the fix a provider refusal reached only mesh Peer rows —
    // the leader never learned, so it could neither refuse its
    // sub-floor consumer, retain the compliant one, nor re-register
    // the surviving aggregate.
    //
    //   A (D = 10 ms  < floor 50 ms → refused, exact signed bytes)
    //   B (D = 100 ms ≥ floor        → retained, live proofs)
    //        └── R (leader) ── P (floor M = 50 ms, owner identity)
    let owner_kp = EntityKeypair::generate();
    let owner_entity = owner_kp.entity_id().clone();
    let owner = AudienceScopeCommitment::owner_root(&owner_entity);

    let mk = |kp: EntityKeypair, incarnation: Option<Incarnation>| async move {
        let mut cfg = base_config()
            .with_sensing_coalescing(true)
            .with_sensing_owner_root(owner);
        if let Some(incarnation) = incarnation {
            cfg = cfg.with_sensing_incarnation(incarnation);
        }
        Arc::new(MeshNode::new(kp, cfg).await.expect("MeshNode::new"))
    };
    let a = mk(EntityKeypair::generate(), None).await;
    let b = mk(EntityKeypair::generate(), None).await;
    let r = mk(EntityKeypair::generate(), None).await;
    let p = mk(owner_kp, Some(Incarnation::new(1))).await;
    p.register_readiness_evaluator(CapabilityId::new("print.document"), Arc::new(AlwaysReady));

    connect_pair(&a, &r).await;
    connect_pair(&b, &r).await;
    connect_pair(&r, &p).await;
    for node in [&a, &b, &r, &p] {
        node.start();
    }
    for node in [&a, &b, &r] {
        node.announce_capabilities(CapabilitySet::new())
            .await
            .expect("announce");
    }
    p.announce_capabilities(CapabilitySet::new().add_tag("print.document"))
        .await
        .expect("announce P");

    let (a_id, b_id, r_id, p_id) = (a.node_id(), b.node_id(), r.node_id(), p.node_id());
    await_condition(Duration::from_secs(5), "entity pins established", || {
        r.peer_entity_id(a_id).is_some()
            && r.peer_entity_id(b_id).is_some()
            && r.peer_entity_id(p_id).is_some()
            && p.peer_entity_id(r_id).is_some()
    })
    .await;
    // Both consumers verify P's signatures (the SI-3 seam bound) —
    // A for the forwarded signed REFUSAL, B for the proofs.
    a.test_pin_peer_entity(p_id, owner_entity.clone());
    b.test_pin_peer_entity(p_id, owner_entity.clone());

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
    let refresher = |node: Arc<MeshNode>, interval: Duration| {
        let spec = spec.clone();
        tokio::spawn(async move {
            loop {
                let _ = node.register_capability_interest(&spec, r_id, interval, TTL);
                tokio::time::sleep(REFRESH).await;
            }
        })
    };
    // A demands 10 ms — below P's 50 ms attestation cadence floor.
    let refresh_a = refresher(a.clone(), Duration::from_millis(10));
    let refresh_b = refresher(b.clone(), Duration::from_millis(100));

    // ── A receives P's EXACT signed refusal through the leader ──
    await_condition(Duration::from_secs(10), "A holds the refusal", || {
        a.sensing_latest_refusal(&branch).is_some()
    })
    .await;
    let refusal = a.sensing_latest_refusal(&branch).expect("present");
    assert_eq!(refusal.origin, p_id, "origin-authored, forwarded verbatim");
    assert_eq!(
        refusal.status_reason,
        StatusReason::SamplingIntervalUnsupported,
    );
    assert_eq!(
        refusal.promised_cadence,
        Duration::from_millis(50),
        "the tagged floor M rides promised_cadence",
    );

    // ── B is retained: the surviving 100 ms aggregate re-registers
    //    and live proofs keep flowing ──
    await_condition(Duration::from_secs(10), "B projects Ready", || {
        b.sensing_projected(&branch) == ProjectedReadiness::Ready
    })
    .await;
    await_condition(Duration::from_secs(10), "one surviving stream at P", || {
        p.sensing_live_streams() == 1
    })
    .await;
    // Fresh beats past the refusal keep arriving for B.
    let seq_mark = b
        .sensing_latest_attestation(&branch)
        .expect("B holds proofs")
        .seq;
    await_condition(Duration::from_secs(10), "B's proofs advance", || {
        b.sensing_latest_attestation(&branch)
            .is_some_and(|proof| proof.seq > seq_mark)
    })
    .await;
    // A never earns a live projection: its cadence was refused and
    // the leader's cached floor refuses its refreshes locally.
    assert_ne!(a.sensing_projected(&branch), ProjectedReadiness::Ready);

    // Honest counters: a refusal is an authorization/policy outcome,
    // never protocol-invalid input.
    for node in [&a, &b, &r, &p] {
        let counters = node.sensing_counters();
        assert_eq!(SensingCounters::get(&counters.protocol_invalid), 0);
    }

    refresh_a.abort();
    refresh_b.abort();
    a.shutdown().await.expect("shutdown A");
    b.shutdown().await.expect("shutdown B");
    r.shutdown().await.expect("shutdown R");
    p.shutdown().await.expect("shutdown P");
}

#[tokio::test]
async fn leader_resolved_as_provider_serves_its_own_proofs() {
    // SI-4 re-review P0: the candidate snapshot permits the leader
    // node ITSELF to resolve as provider (R == P) — a Leader row on
    // a local-provider branch. The origin emitter must dispatch its
    // locally signed beats to the Leader destination (feed
    // `SensingLeader::on_attestation`, fan the resulting real
    // frames), exactly like Peer and Local; before the fix it
    // filtered `Leader => None` and, with no peer rows, signed
    // nothing anyone received.
    //
    //   A (D = 100 ms) ─┐
    //                   R (leader == provider, owner identity)
    //   B (D = 500 ms) ─┘
    let owner_kp = EntityKeypair::generate();
    let owner_entity = owner_kp.entity_id().clone();
    let owner = AudienceScopeCommitment::owner_root(&owner_entity);

    let mk = |kp: EntityKeypair, incarnation: Option<Incarnation>| async move {
        let mut cfg = base_config()
            .with_sensing_coalescing(true)
            .with_sensing_owner_root(owner);
        if let Some(incarnation) = incarnation {
            cfg = cfg.with_sensing_incarnation(incarnation);
        }
        Arc::new(MeshNode::new(kp, cfg).await.expect("MeshNode::new"))
    };
    let a = mk(EntityKeypair::generate(), None).await;
    let b = mk(EntityKeypair::generate(), None).await;
    // R holds the owner identity AND the origin role: leader,
    // provider, and owner are one node (the v1 single-owner shape).
    let r = mk(owner_kp, Some(Incarnation::new(1))).await;
    r.register_readiness_evaluator(CapabilityId::new("print.document"), Arc::new(AlwaysReady));

    connect_pair(&a, &r).await;
    connect_pair(&b, &r).await;
    for node in [&a, &b, &r] {
        node.start();
    }
    for node in [&a, &b] {
        node.announce_capabilities(CapabilitySet::new())
            .await
            .expect("announce");
    }
    r.announce_capabilities(CapabilitySet::new().add_tag("print.document"))
        .await
        .expect("announce R");

    let (a_id, b_id, r_id) = (a.node_id(), b.node_id(), r.node_id());
    // The origin R is ADJACENT to both consumers, so its entity pins
    // through the ordinary handshake — no seam-bound test pin.
    await_condition(Duration::from_secs(5), "entity pins established", || {
        r.peer_entity_id(a_id).is_some()
            && r.peer_entity_id(b_id).is_some()
            && a.peer_entity_id(r_id).is_some()
            && b.peer_entity_id(r_id).is_some()
    })
    .await;

    assert!(r.assume_sensing_leader(), "leader role installs at R");
    let cap = CapabilityId::new("print.document");
    await_condition(
        Duration::from_secs(5),
        "R's snapshot authorizes ITSELF",
        || {
            r.sensing_candidate_snapshot(&cap)
                .iter()
                .any(|candidate| candidate.node_id == r_id && candidate.authorized)
        },
    )
    .await;

    let spec = shared_spec(owner);
    let branch = ProviderInterestKey::new(spec.key(), r_id);
    let interest = spec.key();

    let refresher = |node: Arc<MeshNode>, interval: Duration| {
        let spec = spec.clone();
        tokio::spawn(async move {
            loop {
                let _ = node.register_capability_interest(&spec, r_id, interval, TTL);
                tokio::time::sleep(REFRESH).await;
            }
        })
    };
    let refresh_a = refresher(a.clone(), Duration::from_millis(100));
    let refresh_b = refresher(b.clone(), Duration::from_millis(500));

    // The leader coalesces both interests and resolves ITSELF.
    await_condition(Duration::from_secs(10), "leader coalesces", || {
        r.sensing_leader_interest_count() == Some(1)
    })
    .await;
    assert_eq!(
        r.sensing_leader_branches(&interest),
        Some(vec![r_id]),
        "the leader resolved itself as provider",
    );
    await_condition(Duration::from_secs(10), "Leader row at R", || {
        r.sensing_downstreams(&branch) == vec![DownstreamId::Leader]
    })
    .await;
    await_condition(Duration::from_secs(10), "one local stream at R", || {
        r.sensing_live_streams() == 1
    })
    .await;

    // ── THE WITNESS: R's locally signed proofs dispatch through the
    //    Leader destination and fan to BOTH consumers ──
    await_condition(Duration::from_secs(10), "A receives R's proof", || {
        a.sensing_latest_attestation(&branch)
            .is_some_and(|proof| proof.origin == r_id && proof.status == AttestedStatus::Ready)
    })
    .await;
    await_condition(Duration::from_secs(10), "B receives R's proof", || {
        b.sensing_latest_attestation(&branch)
            .is_some_and(|proof| proof.origin == r_id && proof.status == AttestedStatus::Ready)
    })
    .await;
    await_condition(Duration::from_secs(5), "A projects Ready", || {
        a.sensing_projected(&branch) == ProjectedReadiness::Ready
    })
    .await;
    await_condition(Duration::from_secs(5), "B projects Ready", || {
        b.sensing_projected(&branch) == ProjectedReadiness::Ready
    })
    .await;

    for node in [&a, &b, &r] {
        let counters = node.sensing_counters();
        assert_eq!(SensingCounters::get(&counters.protocol_invalid), 0);
        assert_eq!(SensingCounters::get(&counters.scope_refusals), 0);
    }

    // ── Drain: consumers stop; the self-provider chain unwinds ──
    refresh_a.abort();
    refresh_b.abort();
    await_condition(Duration::from_secs(10), "leader drains", || {
        r.sensing_leader_interest_count() == Some(0)
    })
    .await;
    await_condition(Duration::from_secs(10), "R's own stream retires", || {
        r.sensing_live_streams() == 0
    })
    .await;

    a.shutdown().await.expect("shutdown A");
    b.shutdown().await.expect("shutdown B");
    r.shutdown().await.expect("shutdown R");
}
