//! SI-3 (SENSING_INTEREST_COALESCING_PLAN v4.3, §4.4/§4.6): the
//! origin emitter on real sessions — signed readiness streams at the
//! coalesced cadence, immediate status edges, zero idle emission,
//! one-shot signed cadence refusals, and the receiving hop's
//! verified intake (signature + strictly-newer observer gate).
//!
//! Topology — two real nodes:
//!
//! ```text
//!   C (consumer hop) ── P (provider / origin)
//! ```
//!
//! C registers a provider-targeted interest in P over 0x0C02
//! (`register_sensing_interest`); P's origin emitter answers with
//! origin-signed 0x0C03 attestations that C decodes, verifies
//! against P's TOFU-pinned entity, orders through the §4.6 observer
//! gate, and stores latest-per-branch.
//!
//! One shared `sensing_owner_root` (the SI-2 fleet pattern): a
//! dedicated fleet entity whose keypair no node holds binds the two
//! nodes into one sensing scope.
//!
//! Run: `cargo test --test sensing_origin_emitter`

#![cfg(feature = "net")]

mod common;
use common::*;

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::behavior::sensing::{
    encode_attestation, sign_attestation, AttestedStatus, AudienceScopeCommitment,
    CanonicalConstraints, CapabilityId, DisclosureClass, EvaluationRequest, Incarnation,
    InterestSpec, ProviderInterestKey, ProviderSelector, ReadinessEvaluation, ReadinessEvaluator,
    ResultMode, SensingCounters, StatusReason, UnsignedAttestation, WorkLatencyEnvelope,
    SUBPROTOCOL_READINESS_ATTESTATION,
};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};
use net::adapter::Adapter;

/// Requested sample interval D: 200 ms → promised cadence
/// `max(D/2, 50 ms floor)` = 100 ms.
const D: Duration = Duration::from_millis(200);
/// Soft-state lifetime for the streaming test — short enough that
/// the drain phase completes promptly.
const TTL: Duration = Duration::from_millis(1500);
/// Long lifetime for the injection tests: rows must outlive the
/// whole scenario so a row's DISAPPEARANCE is unambiguously the
/// refusal partition, never the sweep.
const LONG_TTL: Duration = Duration::from_secs(10);
/// Consumer refresh cadence (ttl/2 discipline is the caller's loop
/// in this slice; retrying faster is semantically free).
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

/// A minimal real integration: readiness rides one shared flag —
/// exactly the notify-then-edge shape a capability integration has.
struct FlagEvaluator {
    ready: Arc<AtomicBool>,
}

impl ReadinessEvaluator for FlagEvaluator {
    fn evaluate(&self, _request: &EvaluationRequest<'_>) -> ReadinessEvaluation {
        if self.ready.load(Ordering::Relaxed) {
            ReadinessEvaluation::Ready {
                estimated_start: Some(Duration::from_millis(3)),
            }
        } else {
            ReadinessEvaluation::NotReady { reason: 7 }
        }
    }
}

/// Two sensing-enabled nodes under one fleet root, connected,
/// started, announced, and mutually TOFU-pinned. Returns
/// `(consumer, provider, fleet)`.
async fn consumer_provider_pair(
    provider_incarnation: Option<Incarnation>,
) -> (Arc<MeshNode>, Arc<MeshNode>, AudienceScopeCommitment) {
    let fleet_kp = EntityKeypair::generate();
    let fleet = AudienceScopeCommitment::owner_root(fleet_kp.entity_id());

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
    let mut p_cfg = base_config()
        .with_sensing_coalescing(true)
        .with_sensing_owner_root(fleet);
    if let Some(incarnation) = provider_incarnation {
        p_cfg = p_cfg.with_sensing_incarnation(incarnation);
    }
    let p = Arc::new(
        MeshNode::new(EntityKeypair::generate(), p_cfg)
            .await
            .expect("MeshNode::new P"),
    );

    connect_pair(&c, &p).await;
    c.start();
    p.start();
    for node in [&c, &p] {
        node.announce_capabilities(CapabilitySet::new())
            .await
            .expect("announce");
    }
    let c_id = c.node_id();
    let p_id = p.node_id();
    await_condition(Duration::from_secs(5), "entity pins established", || {
        c.peer_entity_id(p_id).is_some() && p.peer_entity_id(c_id).is_some()
    })
    .await;
    (c, p, fleet)
}

/// The SI-3 mainline: registration → signed stream at the promised
/// cadence → immediate status edge on notify → ttl drain retires the
/// stream (zero idle emission).
#[tokio::test]
async fn origin_streams_signed_readiness_then_edges_then_drains() {
    let (c, p, fleet) = consumer_provider_pair(Some(Incarnation::new(3))).await;
    let p_id = p.node_id();

    let ready = Arc::new(AtomicBool::new(true));
    p.register_readiness_evaluator(
        CapabilityId::new("print.document"),
        Arc::new(FlagEvaluator {
            ready: ready.clone(),
        }),
    );
    assert!(
        p.sensing_origin_active(),
        "incarnation supplied → origin role active"
    );

    let spec = shared_spec(fleet);
    let branch = ProviderInterestKey::new(spec.key(), p_id);

    // Soft-state refresh loop — the ttl/2 discipline lives with the
    // caller in this slice; aborting it IS the churn below.
    let refresher = {
        let c = c.clone();
        let spec = spec.clone();
        tokio::spawn(async move {
            loop {
                let _ = c.register_sensing_interest(&spec, p_id, D, TTL);
                tokio::time::sleep(REFRESH).await;
            }
        })
    };

    // ── Signed stream arrives, verified + gated, at the promised
    //    cadence ──
    await_condition(Duration::from_secs(5), "first admitted attestation", || {
        c.sensing_latest_attestation(&branch).is_some()
    })
    .await;
    let first = c.sensing_latest_attestation(&branch).expect("present");
    assert_eq!(first.origin, p_id);
    assert_eq!(first.origin_incarnation, Incarnation::new(3));
    assert_eq!(first.status, AttestedStatus::Ready);
    assert_eq!(first.status_reason, StatusReason::None);
    assert_eq!(first.estimated_start, Some(Duration::from_millis(3)));
    assert_eq!(
        first.promised_cadence,
        Duration::from_millis(100),
        "promised cadence = max(D/2, floor)",
    );
    assert_eq!(first.audience_scope, fleet);
    assert_eq!(p.sensing_live_streams(), 1);

    // The stream beats at ~100 ms: strictly newer seqs keep landing,
    // and the count over a 450 ms window is cadence-shaped, not a
    // flood (refreshes must not add beats — no-op reschedules).
    let s0 = c.sensing_latest_attestation(&branch).expect("present").seq;
    tokio::time::sleep(Duration::from_millis(450)).await;
    let s1 = c.sensing_latest_attestation(&branch).expect("present").seq;
    assert!(s1 > s0, "the stream advances ({s0} → {s1})");
    assert!(
        s1 - s0 <= 10,
        "cadence-shaped emission, not a flood ({s0} → {s1})",
    );

    // ── Status edge: flip + notify → NotReady lands promptly ──
    ready.store(false, Ordering::Relaxed);
    p.notify_sensing_state_changed(&CapabilityId::new("print.document"));
    await_condition(Duration::from_secs(2), "edge attestation lands", || {
        c.sensing_latest_attestation(&branch)
            .is_some_and(|a| a.status == AttestedStatus::NotReady)
    })
    .await;
    let edge = c.sensing_latest_attestation(&branch).expect("present");
    assert_eq!(edge.status_reason, StatusReason::Provider(7));

    // ── ttl drain: the stream retires; zero idle emission ──
    refresher.abort();
    await_condition(Duration::from_secs(10), "P retires the stream", || {
        p.sensing_live_streams() == 0
    })
    .await;
    await_condition(Duration::from_secs(10), "P's table empties", || {
        p.sensing_table_is_empty()
    })
    .await;
    let idle_seq = c.sensing_latest_attestation(&branch).expect("present").seq;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        c.sensing_latest_attestation(&branch).expect("present").seq,
        idle_seq,
        "zero idle emission — no beats after the last downstream died",
    );

    // Nothing on the flow was protocol-invalid at either hop.
    for node in [&c, &p] {
        let counters = node.sensing_counters();
        assert_eq!(SensingCounters::get(&counters.protocol_invalid), 0);
        assert_eq!(SensingCounters::get(&counters.scope_refusals), 0);
    }

    c.shutdown().await.expect("shutdown C");
    p.shutdown().await.expect("shutdown P");
}

/// §4.4 cadence refusal: a below-floor D never streams — the origin
/// answers with ONE signed refusal beat carrying the floor M in
/// `promised_cadence`, and the receiving hop's refusal reaction
/// partitions its own sub-floor rows.
#[tokio::test]
async fn below_floor_interest_refused_with_signed_beat() {
    let (c, p, fleet) = consumer_provider_pair(Some(Incarnation::new(1))).await;
    let p_id = p.node_id();
    // Evaluator registered so a wrongly-admitted stream WOULD emit
    // Ready — the refusal must precede evaluation entirely.
    p.register_readiness_evaluator(
        CapabilityId::new("print.document"),
        Arc::new(FlagEvaluator {
            ready: Arc::new(AtomicBool::new(true)),
        }),
    );

    let spec = shared_spec(fleet);
    let branch = ProviderInterestKey::new(spec.key(), p_id);
    let bad_d = Duration::from_millis(10);

    // Register (re-sending on loss) until the refusal beat lands.
    await_condition(Duration::from_secs(10), "refusal beat lands at C", || {
        if c.sensing_latest_attestation(&branch).is_none() {
            let _ = c.register_sensing_interest(&spec, p_id, bad_d, LONG_TTL);
            false
        } else {
            true
        }
    })
    .await;

    let refusal = c.sensing_latest_attestation(&branch).expect("present");
    assert_eq!(refusal.status, AttestedStatus::ProviderUnknown);
    assert_eq!(
        refusal.status_reason,
        StatusReason::SamplingIntervalUnsupported,
    );
    assert_eq!(
        refusal.promised_cadence,
        Duration::from_millis(50),
        "the provider floor M rides promised_cadence",
    );
    assert_eq!(refusal.estimated_start, None);

    // The origin never streamed and holds no row (the partition
    // removed the only downstream).
    assert_eq!(p.sensing_live_streams(), 0);
    let p_counters = p.sensing_counters();
    assert!(SensingCounters::get(&p_counters.cadence_refusals) >= 1);
    await_condition(Duration::from_secs(5), "P's table empties", || {
        p.sensing_table_is_empty()
    })
    .await;

    // The receiving hop's refusal reaction partitioned its own
    // sub-floor Local row — the ttl is LONG, so the sweep cannot
    // have done this.
    await_condition(
        Duration::from_secs(5),
        "C's sub-floor row partitioned",
        || c.sensing_downstreams(&branch).is_empty(),
    )
    .await;

    // No further beats: the refusal is one-shot per attempt, not a
    // stream.
    let seq = refusal.seq;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        c.sensing_latest_attestation(&branch).expect("present").seq,
        seq,
        "no refusal stream — one signed beat per refused attempt",
    );

    c.shutdown().await.expect("shutdown C");
    p.shutdown().await.expect("shutdown P");
}

/// The intake's fail-closed pipeline, witnessed with hand-crafted
/// frames: a fail-closed dark origin (no incarnation), a tampered
/// signature refused before the gate, a valid attestation admitted,
/// and an equivocation poisoning the incarnation without displacing
/// the admitted observation.
#[tokio::test]
async fn tampered_and_equivocating_attestations_refused() {
    // P deliberately has NO incarnation: rows register, nothing
    // emits — every 0x0C03 frame in this test is hand-authored.
    let (c, p, fleet) = consumer_provider_pair(None).await;
    let c_addr = c.local_addr();
    let p_id = p.node_id();

    let spec = shared_spec(fleet);
    let branch = ProviderInterestKey::new(spec.key(), p_id);
    let digest = spec.interest_digest();

    // Fail-closed §4.6: the plane is on, the interest registers at
    // BOTH hops, and the origin role still refuses to exist.
    assert!(!p.sensing_origin_active());
    await_condition(Duration::from_secs(10), "row registers at P", || {
        if p.sensing_interest_count() == 0 {
            let _ = c.register_sensing_interest(&spec, p_id, D, LONG_TTL);
            false
        } else {
            true
        }
    })
    .await;
    assert_eq!(
        p.sensing_live_streams(),
        0,
        "fail-closed: no stream without incarnation"
    );

    let unsigned = UnsignedAttestation {
        interest_digest: digest,
        origin: p_id,
        origin_incarnation: Incarnation::new(9),
        capability_id: CapabilityId::new("print.document"),
        capability_generation: 1,
        status: AttestedStatus::Ready,
        status_reason: StatusReason::None,
        estimated_start: Some(Duration::from_millis(3)),
        seq: 0,
        promised_cadence: Duration::from_millis(100),
        audience_scope: fleet,
    };
    let c_counters = c.sensing_counters();

    // ── Tampered: validly signed, then a signed field flipped —
    //    the signature no longer matches the transcript ──
    let valid = sign_attestation(p.entity_keypair(), unsigned.clone()).expect("sign");
    let mut tampered = valid.clone();
    tampered.seq = 6;
    let tampered_bytes = encode_attestation(&tampered).expect("encode");
    await_condition(Duration::from_secs(10), "tampered frame refused", || {
        if SensingCounters::get(&c_counters.protocol_invalid) == 0 {
            let p = p.clone();
            let bytes = tampered_bytes.clone();
            tokio::spawn(async move {
                let _ = p
                    .send_subprotocol(c_addr, SUBPROTOCOL_READINESS_ATTESTATION, &bytes)
                    .await;
            });
            false
        } else {
            true
        }
    })
    .await;
    assert!(
        c.sensing_latest_attestation(&branch).is_none(),
        "a tampered attestation never reaches the observation store",
    );

    // ── Valid: admitted through signature + gate into the store ──
    let valid_bytes = encode_attestation(&valid).expect("encode");
    await_condition(
        Duration::from_secs(10),
        "valid attestation admitted",
        || {
            if c.sensing_latest_attestation(&branch).is_none() {
                let p = p.clone();
                let bytes = valid_bytes.clone();
                tokio::spawn(async move {
                    let _ = p
                        .send_subprotocol(c_addr, SUBPROTOCOL_READINESS_ATTESTATION, &bytes)
                        .await;
                });
                false
            } else {
                true
            }
        },
    )
    .await;
    let admitted = c.sensing_latest_attestation(&branch).expect("present");
    assert_eq!(admitted.seq, 0);
    assert_eq!(admitted.estimated_start, Some(Duration::from_millis(3)));

    // ── Equivocation: same (incarnation, seq), different signed
    //    payload — the §4.6 gate poisons the incarnation and the
    //    admitted observation stands ──
    let mut twin = unsigned;
    twin.estimated_start = Some(Duration::from_millis(4));
    let twin = sign_attestation(p.entity_keypair(), twin).expect("sign twin");
    let twin_bytes = encode_attestation(&twin).expect("encode twin");
    await_condition(Duration::from_secs(10), "equivocation poisons", || {
        if c.sensing_observer_poisoned(p_id, digest).is_none() {
            let p = p.clone();
            let bytes = twin_bytes.clone();
            tokio::spawn(async move {
                let _ = p
                    .send_subprotocol(c_addr, SUBPROTOCOL_READINESS_ATTESTATION, &bytes)
                    .await;
            });
            false
        } else {
            true
        }
    })
    .await;
    assert_eq!(
        c.sensing_observer_poisoned(p_id, digest),
        Some(Incarnation::new(9)),
    );
    assert_eq!(
        c.sensing_latest_attestation(&branch)
            .expect("present")
            .estimated_start,
        Some(Duration::from_millis(3)),
        "the equivocating twin never displaces the admitted observation",
    );

    c.shutdown().await.expect("shutdown C");
    p.shutdown().await.expect("shutdown P");
}
