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
    encode_attestation, encode_interest_frame, sign_attestation, AttestedStatus,
    AudienceScopeCommitment, CanonicalConstraints, CapabilityId, DisclosureClass,
    EvaluationRequest, Incarnation, InterestSpec, ProviderInterestKey, ProviderSelector,
    ReadinessEvaluation, ReadinessEvaluator, ResultMode, SensingCounters, SensingInterestFrame,
    StatusReason, UnsignedAttestation, WorkLatencyEnvelope, SUBPROTOCOL_READINESS_ATTESTATION,
    SUBPROTOCOL_SENSING_INTEREST,
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

    // ── ttl drain: the stream retires (zero idle emission is
    //    structural — a retired stream cannot be collected; unit-
    //    tested) and C's observation store reclaims with its table
    //    (closure item 6) ──
    refresher.abort();
    await_condition(Duration::from_secs(10), "P retires the stream", || {
        p.sensing_live_streams() == 0
    })
    .await;
    await_condition(Duration::from_secs(10), "P's table empties", || {
        p.sensing_table_is_empty()
    })
    .await;
    await_condition(Duration::from_secs(10), "C reclaims observations", || {
        c.sensing_observation_count() == 0
    })
    .await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(p.sensing_live_streams(), 0, "the stream stays retired");
    assert_eq!(
        c.sensing_observation_count(),
        0,
        "nothing repopulates a drained hop",
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
        if c.sensing_latest_refusal(&branch).is_none() {
            let _ = c.register_sensing_interest(&spec, p_id, bad_d, LONG_TTL);
            false
        } else {
            true
        }
    })
    .await;

    let refusal = c.sensing_latest_refusal(&branch).expect("present");
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
    // Closure item 6: a refusal is a control response, never
    // warm-start status — the observation store stays empty.
    assert!(
        c.sensing_latest_attestation(&branch).is_none(),
        "refusals never enter the warm-start observation store",
    );

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
        c.sensing_latest_refusal(&branch).expect("present").seq,
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

/// Closure item 5: an evaluator is arbitrary user code and may call
/// back into `MeshNode` from `evaluate()` — with the two-phase
/// emitter loop it runs OUTSIDE the emitter lock, so the callback
/// cannot deadlock the emitter task, and the poke-per-beat feedback
/// loop stays min-gapped at the floor instead of hot-looping.
#[tokio::test]
async fn reentrant_evaluator_cannot_deadlock_the_emitter() {
    struct ReentrantEvaluator {
        node: parking_lot::Mutex<Option<Arc<MeshNode>>>,
    }
    impl ReadinessEvaluator for ReentrantEvaluator {
        fn evaluate(&self, _request: &EvaluationRequest<'_>) -> ReadinessEvaluation {
            if let Some(node) = self.node.lock().as_ref() {
                // Both re-enter MeshNode; the notify path takes the
                // emitter mutex this very loop iteration released
                // before calling us.
                node.notify_sensing_state_changed(&CapabilityId::new("print.document"));
                let _ = node.sensing_live_streams();
            }
            ReadinessEvaluation::Ready {
                estimated_start: None,
            }
        }
    }

    let (c, p, fleet) = consumer_provider_pair(Some(Incarnation::new(1))).await;
    let p_id = p.node_id();
    let evaluator = Arc::new(ReentrantEvaluator {
        node: parking_lot::Mutex::new(None),
    });
    *evaluator.node.lock() = Some(p.clone());
    p.register_readiness_evaluator(CapabilityId::new("print.document"), evaluator);

    let spec = shared_spec(fleet);
    let branch = ProviderInterestKey::new(spec.key(), p_id);
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

    await_condition(Duration::from_secs(5), "stream starts", || {
        c.sensing_latest_attestation(&branch).is_some()
    })
    .await;
    let s0 = c.sensing_latest_attestation(&branch).expect("present").seq;
    tokio::time::sleep(Duration::from_millis(450)).await;
    let s1 = c.sensing_latest_attestation(&branch).expect("present").seq;
    assert!(
        s1 > s0,
        "the stream advances despite reentrancy ({s0} → {s1})"
    );
    // Each beat's poke pulls the next to last+floor (50 ms): the
    // feedback loop runs at the floor, never unboundedly.
    assert!(
        s1 - s0 <= 15,
        "poke-per-beat stays min-gapped at the floor ({s0} → {s1})",
    );

    refresher.abort();
    c.shutdown().await.expect("shutdown C");
    p.shutdown().await.expect("shutdown P");
}

/// Closure item 4: wire intervals are bounded at intake —
/// `0 < D ≤ sensing_interest_ttl`. A zero or beyond-lifetime D never
/// reaches the table or the emitter, from either the wire or the
/// local API.
#[tokio::test]
async fn out_of_bounds_intervals_refused_at_intake() {
    let (c, p, fleet) = consumer_provider_pair(Some(Incarnation::new(1))).await;
    let p_id = p.node_id();
    let p_addr = p.local_addr();
    p.register_readiness_evaluator(
        CapabilityId::new("print.document"),
        Arc::new(FlagEvaluator {
            ready: Arc::new(AtomicBool::new(true)),
        }),
    );
    let spec = shared_spec(fleet);

    // Local API: both interval bounds refused synchronously, and
    // (round 2, item 2) so is a zero soft-state ttl.
    for bad in [Duration::ZERO, Duration::from_secs(3600)] {
        let refused = c.register_sensing_interest(&spec, p_id, bad, TTL);
        assert!(
            matches!(
                refused,
                Err(net::adapter::net::SensingRegistrationError::Interval { .. })
            ),
            "local registration with D={bad:?} must refuse, got {refused:?}",
        );
    }
    assert!(
        matches!(
            c.register_sensing_interest(&spec, p_id, D, Duration::ZERO),
            Err(net::adapter::net::SensingRegistrationError::ZeroTtl)
        ),
        "a zero ttl is dead on arrival",
    );

    // Wire: crafted ProviderRegistrations with out-of-bounds D (or
    // a zero ttl) drop at P's intake — repeated sends, then the
    // table is still empty.
    for (bad_d, bad_ttl) in [
        (Duration::ZERO, TTL),
        (Duration::from_secs(3600), TTL),
        (D, Duration::ZERO),
    ] {
        let frame = SensingInterestFrame::provider_registration(&spec, p_id, bad_d, bad_ttl);
        let bytes = encode_interest_frame(&frame).expect("encode");
        for _ in 0..5 {
            let _ = c
                .send_subprotocol(p_addr, SUBPROTOCOL_SENSING_INTEREST, &bytes)
                .await;
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    assert_eq!(p.sensing_interest_count(), 0, "no row from bad intervals");
    assert_eq!(p.sensing_live_streams(), 0, "no stream from bad intervals");

    // Control: a legal D on the SAME spec registers fine.
    await_condition(Duration::from_secs(10), "legal D registers", || {
        if p.sensing_interest_count() == 0 {
            let _ = c.register_sensing_interest(&spec, p_id, D, TTL);
            false
        } else {
            true
        }
    })
    .await;

    c.shutdown().await.expect("shutdown C");
    p.shutdown().await.expect("shutdown P");
}

/// Closure item 3: a cached provider floor is invalidated when an
/// admitted attestation moves the origin's epoch on EITHER axis —
/// incarnation (restart) or capability generation (redefinition) —
/// so a floor learned under the old epoch cannot keep refusing
/// registrations on stale grounds. All 0x0C03 frames are
/// hand-authored (P stays a dark origin).
#[tokio::test]
async fn cached_floor_invalidates_on_origin_epoch_change() {
    let (c, p, fleet) = consumer_provider_pair(None).await;
    let c_addr = c.local_addr();
    let p_id = p.node_id();
    let spec = shared_spec(fleet);
    let branch = ProviderInterestKey::new(spec.key(), p_id);
    let digest = spec.interest_digest();

    let beat = |incarnation: u64, generation: u64, seq: u64, refusal: bool| {
        let unsigned = UnsignedAttestation {
            interest_digest: digest,
            origin: p_id,
            origin_incarnation: Incarnation::new(incarnation),
            capability_id: CapabilityId::new("print.document"),
            capability_generation: generation,
            status: if refusal {
                AttestedStatus::ProviderUnknown
            } else {
                AttestedStatus::Ready
            },
            status_reason: if refusal {
                StatusReason::SamplingIntervalUnsupported
            } else {
                StatusReason::None
            },
            estimated_start: None,
            seq,
            promised_cadence: Duration::from_millis(50),
            audience_scope: fleet,
        };
        encode_attestation(&sign_attestation(p.entity_keypair(), unsigned).expect("sign"))
            .expect("encode")
    };
    let send_until = |bytes: Vec<u8>, done: Box<dyn Fn() -> bool + Send>, what: &'static str| {
        let p = p.clone();
        async move {
            await_condition(Duration::from_secs(10), what, || {
                if done() {
                    true
                } else {
                    let p = p.clone();
                    let bytes = bytes.clone();
                    tokio::spawn(async move {
                        let _ = p
                            .send_subprotocol(c_addr, SUBPROTOCOL_READINESS_ATTESTATION, &bytes)
                            .await;
                    });
                    false
                }
            })
            .await;
        }
    };

    // A 100 ms row that will SURVIVE the refusals — the floor cache
    // lives on the entry, so a survivor must hold it open.
    await_condition(Duration::from_secs(10), "survivor row at C", || {
        if c.sensing_downstreams(&branch).is_empty() {
            let _ = c.register_sensing_interest(&spec, p_id, D, LONG_TTL);
            false
        } else {
            true
        }
    })
    .await;

    // ── Round 1: incarnation axis ──
    {
        let c = c.clone();
        let branch = branch.clone();
        send_until(
            beat(5, 1, 0, true),
            Box::new(move || c.sensing_latest_refusal(&branch).is_some()),
            "refusal (inc 5) lands",
        )
        .await;
    }
    assert!(
        matches!(
            c.register_sensing_interest(&spec, p_id, Duration::from_millis(10), LONG_TTL),
            Ok(net::adapter::net::behavior::sensing::RegisterOutcome::RefusedByCachedFloor { .. })
        ),
        "the cached floor refuses a sub-floor joiner locally",
    );
    {
        let c = c.clone();
        let branch = branch.clone();
        send_until(
            beat(6, 1, 0, false),
            Box::new(move || c.sensing_latest_attestation(&branch).is_some()),
            "Ready beat (inc 6) lands",
        )
        .await;
    }
    assert!(
        matches!(
            c.register_sensing_interest(&spec, p_id, Duration::from_millis(10), LONG_TTL),
            Ok(net::adapter::net::behavior::sensing::RegisterOutcome::Registered(_))
        ),
        "a new incarnation invalidated the cached floor — the sub-floor request goes through again",
    );

    // ── Round 2: generation axis ──
    // Restore a surviving 100 ms row (the sub-floor registration
    // above replaced the Local row), then re-cache a floor under
    // (inc 6, gen 1).
    assert!(c
        .register_sensing_interest(&spec, p_id, D, LONG_TTL)
        .is_ok());
    {
        let c = c.clone();
        let branch = branch.clone();
        send_until(
            beat(6, 1, 1, true),
            Box::new(move || {
                c.sensing_latest_refusal(&branch)
                    .is_some_and(|r| r.origin_incarnation == Incarnation::new(6))
            }),
            "refusal (inc 6, gen 1) lands",
        )
        .await;
    }
    assert!(
        matches!(
            c.register_sensing_interest(&spec, p_id, Duration::from_millis(10), LONG_TTL),
            Ok(net::adapter::net::behavior::sensing::RegisterOutcome::RefusedByCachedFloor { .. })
        ),
        "the re-cached floor refuses locally again",
    );
    {
        let c = c.clone();
        let branch = branch.clone();
        send_until(
            beat(6, 2, 2, false),
            Box::new(move || {
                c.sensing_latest_attestation(&branch)
                    .is_some_and(|a| a.capability_generation == 2)
            }),
            "Ready beat (gen 2) lands",
        )
        .await;
    }
    assert!(
        matches!(
            c.register_sensing_interest(&spec, p_id, Duration::from_millis(10), LONG_TTL),
            Ok(net::adapter::net::behavior::sensing::RegisterOutcome::Registered(_))
        ),
        "a new capability generation invalidated the cached floor too",
    );

    c.shutdown().await.expect("shutdown C");
    p.shutdown().await.expect("shutdown P");
}

/// Closure item 2 end-to-end (the review's required three-hop
/// mixed-cadence test): a sub-floor consumer joining THROUGH a
/// relay must not strand the surviving consumer's demand.
///
/// ```text
///   A (100 ms) ─┐
///               R ── P (floor 50 ms)
///   C (10 ms) ──┘
/// ```
///
/// C's 10 ms tightens R's aggregate below P's floor; P refuses and
/// partitions its Peer(R) row (stream dies). R partitions Peer(C)
/// out, keeps survivor Peer(A) — and A's next refresh finds the
/// PENDING survivor transition (`on_refusal` no longer consumes it)
/// and re-registers 100 ms upstream, so P's stream RESUMES. Before
/// the fix, `last_advertised` was already consumed and the refresh
/// produced no upstream update: A stranded permanently.
#[tokio::test]
async fn mixed_cadence_refusal_recovers_the_survivor_through_the_relay() {
    let fleet_kp = EntityKeypair::generate();
    let fleet = AudienceScopeCommitment::owner_root(fleet_kp.entity_id());
    let mk = |incarnation: Option<Incarnation>| async move {
        let mut cfg = base_config()
            .with_sensing_coalescing(true)
            .with_sensing_owner_root(fleet);
        if let Some(incarnation) = incarnation {
            cfg = cfg.with_sensing_incarnation(incarnation);
        }
        Arc::new(
            MeshNode::new(EntityKeypair::generate(), cfg)
                .await
                .expect("MeshNode::new"),
        )
    };
    let a = mk(None).await;
    let c = mk(None).await;
    let r = mk(None).await;
    let p = mk(Some(Incarnation::new(1))).await;
    p.register_readiness_evaluator(
        CapabilityId::new("print.document"),
        Arc::new(FlagEvaluator {
            ready: Arc::new(AtomicBool::new(true)),
        }),
    );

    connect_pair(&a, &r).await;
    connect_pair(&c, &r).await;
    connect_pair(&r, &p).await;
    a.start();
    c.start();
    r.start();
    p.start();
    for node in [&a, &c, &r, &p] {
        node.announce_capabilities(CapabilitySet::new())
            .await
            .expect("announce");
    }
    let a_id = a.node_id();
    let c_id = c.node_id();
    let r_id = r.node_id();
    let p_id = p.node_id();
    await_condition(Duration::from_secs(5), "entity pins established", || {
        r.peer_entity_id(a_id).is_some()
            && r.peer_entity_id(c_id).is_some()
            && r.peer_entity_id(p_id).is_some()
            && p.peer_entity_id(r_id).is_some()
    })
    .await;
    // Consumers reach P through R.
    a.router().add_route(p_id, r.local_addr());
    c.router().add_route(p_id, r.local_addr());
    // SI-3 seam bound (documented on the 0x0C03 intake): a hop
    // verifies beats against the ORIGIN's TOFU pin, and pin
    // propagation to non-adjacent hops rides SI-4 — pin P at the
    // consumers deterministically, exactly as the SI-2 chain test
    // pinned its injected declarer.
    let p_entity = p.entity_keypair().entity_id().clone();
    a.test_pin_peer_entity(p_id, p_entity.clone());
    c.test_pin_peer_entity(p_id, p_entity);

    let spec = shared_spec(fleet);
    let branch = ProviderInterestKey::new(spec.key(), p_id);
    use net::adapter::net::behavior::sensing::DownstreamId;

    // ── A registers 100 ms through R; P streams; R observes ──
    let refresher_a = {
        let a = a.clone();
        let spec = spec.clone();
        tokio::spawn(async move {
            loop {
                let _ = a.register_sensing_interest(&spec, p_id, Duration::from_millis(100), TTL);
                tokio::time::sleep(REFRESH).await;
            }
        })
    };
    await_condition(Duration::from_secs(10), "P streams to R", || {
        p.sensing_live_streams() == 1 && r.sensing_latest_attestation(&branch).is_some()
    })
    .await;
    assert_eq!(
        r.sensing_downstreams(&branch),
        vec![DownstreamId::Peer(a_id)],
    );

    // ── C's sub-floor 10 ms triggers the refusal episode ──
    await_condition(Duration::from_secs(10), "refusal lands at R", || {
        if r.sensing_latest_refusal(&branch).is_none() {
            let _ = c.register_sensing_interest(&spec, p_id, Duration::from_millis(10), TTL);
            false
        } else {
            true
        }
    })
    .await;
    let refusal = r.sensing_latest_refusal(&branch).expect("present");
    assert_eq!(
        refusal.status_reason,
        StatusReason::SamplingIntervalUnsupported
    );
    // R partitioned C out and kept the survivor.
    await_condition(Duration::from_secs(5), "C partitioned out at R", || {
        r.sensing_downstreams(&branch) == vec![DownstreamId::Peer(a_id)]
    })
    .await;
    // The forwarded refusal reached C (its own Local row partitioned).
    await_condition(Duration::from_secs(5), "forwarded refusal at C", || {
        c.sensing_latest_refusal(&branch).is_some()
    })
    .await;

    // ── THE RECOVERY (closure item 2): A's refresh re-registers the
    //    survivor aggregate upstream and P's stream RESUMES ──
    await_condition(
        Duration::from_secs(10),
        "P re-registers the survivor",
        || {
            p.sensing_live_streams() == 1
                && p.sensing_downstream_entry(&branch, DownstreamId::Peer(r_id))
                    .is_some_and(|row| row.requested_sample_interval == Duration::from_millis(100))
        },
    )
    .await;
    // Fresh beats land at R past the refusal's seq — the stream is
    // truly live again, not a cached leftover.
    await_condition(Duration::from_secs(10), "fresh beats at R", || {
        r.sensing_latest_attestation(&branch)
            .is_some_and(|beat| beat.status == AttestedStatus::Ready && beat.seq > refusal.seq)
    })
    .await;

    // Nothing on the episode was protocol-invalid anywhere.
    for node in [&a, &c, &r, &p] {
        let counters = node.sensing_counters();
        assert_eq!(SensingCounters::get(&counters.protocol_invalid), 0);
    }

    refresher_a.abort();
    a.shutdown().await.expect("shutdown A");
    c.shutdown().await.expect("shutdown C");
    r.shutdown().await.expect("shutdown R");
    p.shutdown().await.expect("shutdown P");
}

/// Second closure round, items 1 + 4: fail-closed intake ordering
/// and monotonic provider epochs.
///
/// A signed-but-MALFORMED refusal (floor M out of bounds) must be
/// dropped before it consumes sequence admission or moves provider
/// state; a delayed OLD-incarnation beat on a fresh digest passes
/// its digest-local observer gate but must neither regress the
/// provider-wide epoch nor flush valid floor caches.
#[tokio::test]
async fn malformed_refusals_and_stale_epochs_touch_nothing() {
    let (c, p, fleet) = consumer_provider_pair(None).await;
    let c_addr = c.local_addr();
    let p_id = p.node_id();
    let spec_a = shared_spec(fleet);
    let branch_a = ProviderInterestKey::new(spec_a.key(), p_id);
    let mut spec_b = shared_spec(fleet);
    spec_b.constraints = CanonicalConstraints::from_entries([("media", "letter")]).unwrap();
    let branch_b = ProviderInterestKey::new(spec_b.key(), p_id);

    let beat = |spec: &InterestSpec,
                incarnation: u64,
                seq: u64,
                refusal: bool,
                floor_ms: u64,
                start_ms: u64| {
        let unsigned = UnsignedAttestation {
            interest_digest: spec.interest_digest(),
            origin: p_id,
            origin_incarnation: Incarnation::new(incarnation),
            capability_id: CapabilityId::new("print.document"),
            capability_generation: 1,
            status: if refusal {
                AttestedStatus::ProviderUnknown
            } else {
                AttestedStatus::Ready
            },
            status_reason: if refusal {
                StatusReason::SamplingIntervalUnsupported
            } else {
                StatusReason::None
            },
            estimated_start: (!refusal).then(|| Duration::from_millis(start_ms)),
            seq,
            promised_cadence: Duration::from_millis(floor_ms),
            audience_scope: fleet,
        };
        encode_attestation(&sign_attestation(p.entity_keypair(), unsigned).expect("sign"))
            .expect("encode")
    };
    macro_rules! send_until {
        ($bytes:expr, $what:literal, $done:expr) => {
            let bytes = $bytes;
            await_condition(Duration::from_secs(10), $what, || {
                if $done {
                    true
                } else {
                    let p = p.clone();
                    let bytes = bytes.clone();
                    tokio::spawn(async move {
                        let _ = p
                            .send_subprotocol(c_addr, SUBPROTOCOL_READINESS_ATTESTATION, &bytes)
                            .await;
                    });
                    false
                }
            })
            .await;
        };
    }
    let floor_cached = || {
        matches!(
            c.register_sensing_interest(&spec_a, p_id, Duration::from_millis(10), LONG_TTL),
            Ok(net::adapter::net::behavior::sensing::RegisterOutcome::RefusedByCachedFloor { .. })
        )
    };

    // Survivor row on branch A, then establish epoch (inc 6) and a
    // cached floor.
    await_condition(Duration::from_secs(10), "survivor row at C", || {
        if c.sensing_downstreams(&branch_a).is_empty() {
            let _ = c.register_sensing_interest(&spec_a, p_id, D, LONG_TTL);
            false
        } else {
            true
        }
    })
    .await;
    send_until!(
        beat(&spec_a, 6, 0, false, 100, 3),
        "Ready (inc 6) admits",
        c.sensing_latest_attestation(&branch_a).is_some()
    );
    send_until!(beat(&spec_a, 6, 1, true, 50, 0), "refusal caches floor", {
        c.sensing_latest_refusal(&branch_a).is_some()
    });
    assert!(floor_cached(), "the floor refuses a sub-floor joiner");

    // ── Item 1: a signed refusal with M = 0 (malformed) at inc 7,
    //    seq 2 — dropped BEFORE the gate and the epoch ──
    let invalid_before = SensingCounters::get(&c.sensing_counters().protocol_invalid);
    send_until!(
        beat(&spec_a, 7, 2, true, 0, 0),
        "malformed refusal counted",
        SensingCounters::get(&c.sensing_counters().protocol_invalid) > invalid_before
    );
    assert!(
        floor_cached(),
        "a malformed inc-7 refusal must not move the epoch or flush the floor",
    );
    // The gate never saw the inc-7 frame: a valid inc-6 beat at the
    // SAME seq it carried still admits.
    send_until!(
        beat(&spec_a, 6, 2, false, 100, 9),
        "inc-6 seq-2 still admits",
        {
            c.sensing_latest_attestation(&branch_a)
                .is_some_and(|a| a.estimated_start == Some(Duration::from_millis(9)))
        }
    );

    // ── Item 4 (updated for SI-5R P0): a globally STALE-epoch beat
    //    on a FRESH digest is DROPPED before it can touch state —
    //    the three-way epoch standing supersedes its digest-local
    //    gate admission. It never becomes an observation, and it
    //    neither regresses the provider epoch nor flushes the floor.
    //    (Under the old SI-3R2 semantics it admitted on its fresh
    //    digest; SI-5R P0 tightened that to a drop so a stale beat on
    //    a sibling digest cannot re-establish a superseded branch.) ──
    await_condition(Duration::from_secs(10), "row on branch B", || {
        if c.sensing_downstreams(&branch_b).is_empty() {
            let _ = c.register_sensing_interest(&spec_b, p_id, D, LONG_TTL);
            false
        } else {
            true
        }
    })
    .await;
    let superseded_before = SensingCounters::get(&c.sensing_counters().attestations_superseded);
    send_until!(
        beat(&spec_b, 5, 0, false, 100, 3),
        "stale-epoch beat dropped at the provider-wide epoch gate",
        SensingCounters::get(&c.sensing_counters().attestations_superseded) > superseded_before
    );
    assert!(
        c.sensing_latest_attestation(&branch_b).is_none(),
        "a globally stale epoch never becomes an observation (SI-5R P0)",
    );
    assert!(
        floor_cached(),
        "a cross-digest stale epoch must not flush valid floors",
    );

    // A genuinely NEWER epoch still invalidates.
    send_until!(beat(&spec_b, 7, 1, false, 100, 3), "inc 7 advances", {
        c.sensing_latest_attestation(&branch_b)
            .is_some_and(|a| a.origin_incarnation == Incarnation::new(7))
    });
    await_condition(Duration::from_secs(5), "floor invalidated by inc 7", || {
        matches!(
            c.register_sensing_interest(&spec_a, p_id, Duration::from_millis(10), LONG_TTL),
            Ok(net::adapter::net::behavior::sensing::RegisterOutcome::Registered(_))
        )
    })
    .await;

    c.shutdown().await.expect("shutdown C");
    p.shutdown().await.expect("shutdown P");
}

/// Second closure round, item 2: a valid short-ttl row refreshed at
/// exactly ttl/2 must stay continuously live upstream — the damper
/// gap scales to min(100 ms, ttl/2), so the mandated refresh always
/// passes instead of being suppressed until the row expires.
#[tokio::test]
async fn short_ttl_rows_survive_the_damper() {
    let (c, p, fleet) = consumer_provider_pair(Some(Incarnation::new(1))).await;
    let c_id = c.node_id();
    let p_id = p.node_id();
    p.register_readiness_evaluator(
        CapabilityId::new("print.document"),
        Arc::new(FlagEvaluator {
            ready: Arc::new(AtomicBool::new(true)),
        }),
    );
    let spec = shared_spec(fleet);
    let branch = ProviderInterestKey::new(spec.key(), p_id);
    let short_ttl = Duration::from_millis(100);

    // Refresh at exactly ttl/2.
    let refresher = {
        let c = c.clone();
        let spec = spec.clone();
        tokio::spawn(async move {
            loop {
                let _ = c.register_sensing_interest(&spec, p_id, short_ttl, short_ttl);
                tokio::time::sleep(short_ttl / 2).await;
            }
        })
    };
    use net::adapter::net::behavior::sensing::DownstreamId;
    await_condition(Duration::from_secs(10), "upstream row established", || {
        p.sensing_downstream_entry(&branch, DownstreamId::Peer(c_id))
            .is_some()
    })
    .await;

    // Continuously live across several ttls: before the fix the
    // fixed 100 ms damper suppressed every 50 ms refresh and the
    // row expired.
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(25)).await;
        assert!(
            p.sensing_downstream_entry(&branch, DownstreamId::Peer(c_id))
                .is_some(),
            "the upstream row must never lapse while refreshes flow at ttl/2",
        );
    }
    assert_eq!(p.sensing_live_streams(), 1, "the stream rode along");

    refresher.abort();
    c.shutdown().await.expect("shutdown C");
    p.shutdown().await.expect("shutdown P");
}

/// Second closure round, item 3: when tombstone GC ages out the last
/// refusal, the provider's epoch record reclaims with it — nothing
/// outlives everything that justified it.
#[tokio::test]
async fn tombstone_ageout_drains_provider_epochs_too() {
    // Short soft-state horizon so the tombstone ages out quickly.
    let fleet_kp = EntityKeypair::generate();
    let fleet = AudienceScopeCommitment::owner_root(fleet_kp.entity_id());
    let c = Arc::new(
        MeshNode::new(
            EntityKeypair::generate(),
            base_config()
                .with_sensing_coalescing(true)
                .with_sensing_owner_root(fleet)
                .with_sensing_interest_ttl(Duration::from_secs(1)),
        )
        .await
        .expect("MeshNode::new C"),
    );
    let p = Arc::new(
        MeshNode::new(
            EntityKeypair::generate(),
            base_config()
                .with_sensing_coalescing(true)
                .with_sensing_owner_root(fleet),
        )
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
    let p_id = p.node_id();
    let c_id = c.node_id();
    let c_addr = c.local_addr();
    await_condition(Duration::from_secs(5), "entity pins established", || {
        c.peer_entity_id(p_id).is_some() && p.peer_entity_id(c_id).is_some()
    })
    .await;

    let spec = shared_spec(fleet);
    let branch = ProviderInterestKey::new(spec.key(), p_id);
    let unsigned = UnsignedAttestation {
        interest_digest: spec.interest_digest(),
        origin: p_id,
        origin_incarnation: Incarnation::new(3),
        capability_id: CapabilityId::new("print.document"),
        capability_generation: 1,
        status: AttestedStatus::ProviderUnknown,
        status_reason: StatusReason::SamplingIntervalUnsupported,
        estimated_start: None,
        seq: 0,
        promised_cadence: Duration::from_millis(50),
        audience_scope: fleet,
    };
    let refusal_bytes =
        encode_attestation(&sign_attestation(p.entity_keypair(), unsigned).expect("sign"))
            .expect("encode");

    // A sub-floor row that the refusal fully partitions: the branch
    // dies, leaving the tombstone + the epoch record behind.
    await_condition(Duration::from_secs(10), "refusal tombstone lands", || {
        if c.sensing_latest_refusal(&branch).is_some() {
            return true;
        }
        let _ = c.register_sensing_interest(
            &spec,
            p_id,
            Duration::from_millis(10),
            Duration::from_secs(1),
        );
        let p = p.clone();
        let bytes = refusal_bytes.clone();
        tokio::spawn(async move {
            let _ = p
                .send_subprotocol(c_addr, SUBPROTOCOL_READINESS_ATTESTATION, &bytes)
                .await;
        });
        false
    })
    .await;
    assert_eq!(c.sensing_provider_epoch_count(), 1, "epoch recorded");

    // The tombstone ages out (> 1 s with no live branch) and takes
    // the orphaned epoch with it — BOTH maps drain.
    await_condition(Duration::from_secs(10), "tombstone and epoch drain", || {
        c.sensing_latest_refusal(&branch).is_none() && c.sensing_provider_epoch_count() == 0
    })
    .await;
    assert_eq!(c.sensing_observation_count(), 0);

    c.shutdown().await.expect("shutdown C");
    p.shutdown().await.expect("shutdown P");
}

/// Second closure round, item 5: at the live-stream cap a NEW
/// registration is surfaced as an explicit error and its row is
/// rolled back — never a dark row reporting success.
#[tokio::test]
async fn at_capacity_registration_surfaced_and_rolled_back() {
    use net::adapter::net::behavior::sensing::MAX_LIVE_SENSING_STREAMS;
    use net::adapter::net::SensingRegistrationError;

    let node = Arc::new(
        MeshNode::new(
            EntityKeypair::generate(),
            base_config()
                .with_sensing_coalescing(true)
                .with_sensing_incarnation(Incarnation::new(1))
                // The TABLE's per-downstream cap sits at 512 by
                // default and would bite first — this test targets
                // the EMITTER's live-stream cap.
                .with_max_interests_per_peer(2 * MAX_LIVE_SENSING_STREAMS),
        )
        .await
        .expect("MeshNode::new"),
    );
    let self_id = node.node_id();
    let own_root = AudienceScopeCommitment::owner_root(node.entity_keypair().entity_id());
    let spec_n = |i: usize| {
        let mut spec = shared_spec(own_root);
        spec.constraints =
            CanonicalConstraints::from_entries([("slot", format!("{i}").as_str())]).unwrap();
        spec
    };

    for i in 0..MAX_LIVE_SENSING_STREAMS {
        node.register_sensing_interest(&spec_n(i), self_id, D, LONG_TTL)
            .expect("registers under the cap");
    }
    assert_eq!(node.sensing_live_streams(), MAX_LIVE_SENSING_STREAMS);

    // The 1025th distinct stream: explicit error, row rolled back.
    let overflow = spec_n(MAX_LIVE_SENSING_STREAMS);
    let refused = node.register_sensing_interest(&overflow, self_id, D, LONG_TTL);
    assert!(
        matches!(refused, Err(SensingRegistrationError::AtCapacity)),
        "expected AtCapacity, got {refused:?}",
    );
    let overflow_branch = ProviderInterestKey::new(overflow.key(), self_id);
    assert!(
        node.sensing_downstreams(&overflow_branch).is_empty(),
        "the refused registration's row is rolled back, never dark",
    );
    // Review L1 narrow-hold: registration eagerly anchors a consumer cell, so the
    // AtCapacity rollback must remove that just-created cell too — no ghost.
    assert!(
        node.sensing_consumer_cell_interval_for_test(&overflow_branch)
            .is_none(),
        "the eagerly-created consumer cell is rolled back with the row",
    );
    assert_eq!(node.sensing_live_streams(), MAX_LIVE_SENSING_STREAMS);

    // Refreshes of existing live streams still succeed at cap.
    node.register_sensing_interest(&spec_n(0), self_id, D, LONG_TTL)
        .expect("live refresh at capacity");

    node.shutdown().await.expect("shutdown");
}

/// SI-4 re-review item 5: table mutations that move a branch's
/// aggregate D must re-anchor the hop's continuity window
/// IMMEDIATELY — production paths, with NO intervening beat (the
/// origin is silenced by a real partition before the D changes).
///
/// Two interests on one provider, both established at D = 400 ms
/// (window 3 × 400 = 1200 ms). After the partition:
/// - one re-registers at D = 100 ms → window 3 × max(promised 200,
///   100) = 600 ms → the deadline moves INWARD now (suspicion within
///   ~700 ms; the stale window would have held ≥ 800 ms);
/// - the other re-registers at D = 1200 ms → window 3600 ms → the
///   deadline moves OUTWARD now (still Established at +1500 ms,
///   where the stale window would already have expired).
#[tokio::test]
async fn interval_changes_re_anchor_windows_with_no_intervening_beat() {
    let (c, p, fleet) = consumer_provider_pair(Some(Incarnation::new(9))).await;
    let p_id = p.node_id();

    let ready = Arc::new(AtomicBool::new(true));
    p.register_readiness_evaluator(
        CapabilityId::new("print.document"),
        Arc::new(FlagEvaluator {
            ready: ready.clone(),
        }),
    );

    let spec_tight = shared_spec(fleet);
    let spec_loose = {
        let mut spec = shared_spec(fleet);
        spec.constraints = CanonicalConstraints::from_entries([("media", "letter")]).unwrap();
        spec
    };
    let branch_tight = ProviderInterestKey::new(spec_tight.key(), p_id);
    let branch_loose = ProviderInterestKey::new(spec_loose.key(), p_id);

    // Establish BOTH branches at D = 400 ms (long ttl: no refresher —
    // every later mutation below is an explicit registration).
    let d0 = Duration::from_millis(400);
    c.register_sensing_interest(&spec_tight, p_id, d0, LONG_TTL)
        .expect("register tight");
    c.register_sensing_interest(&spec_loose, p_id, d0, LONG_TTL)
        .expect("register loose");
    await_condition(Duration::from_secs(5), "both branches established", || {
        use net::adapter::net::behavior::sensing::Continuity;
        c.sensing_upstream_continuity(&branch_tight) == Some(Continuity::Established)
            && c.sensing_upstream_continuity(&branch_loose) == Some(Continuity::Established)
    })
    .await;

    // Silence the origin — from here on, NO beat reaches C.
    chaos_partition(&c, &p);

    // TIGHTEN: D 400 → 100. The window shrinks from 1200 ms to
    // 600 ms; with the fix, suspicion fires within ~700 ms of the
    // last beat — before the STALE deadline (≥ 800 ms out) could.
    c.register_sensing_interest(&spec_tight, p_id, Duration::from_millis(100), LONG_TTL)
        .expect("tighten");
    // LOOSEN: D 400 → 1200. The window grows to 3600 ms.
    c.register_sensing_interest(&spec_loose, p_id, Duration::from_millis(1200), LONG_TTL)
        .expect("loosen");
    let changed_at = std::time::Instant::now();

    await_condition(
        Duration::from_millis(700),
        "tightened window expires early",
        || {
            use net::adapter::net::behavior::sensing::Continuity;
            c.sensing_upstream_continuity(&branch_tight) == Some(Continuity::Expired)
        },
    )
    .await;

    // The loosened branch holds PAST its stale deadline…
    tokio::time::sleep(Duration::from_millis(1500).saturating_sub(changed_at.elapsed())).await;
    {
        use net::adapter::net::behavior::sensing::Continuity;
        assert_eq!(
            c.sensing_upstream_continuity(&branch_loose),
            Some(Continuity::Established),
            "loosened window must move the deadline outward immediately",
        );
    }
    // …and still expires honestly at the loosened horizon.
    await_condition(
        Duration::from_secs(4),
        "loosened window expires eventually",
        || {
            use net::adapter::net::behavior::sensing::Continuity;
            c.sensing_upstream_continuity(&branch_loose) == Some(Continuity::Expired)
        },
    )
    .await;

    c.shutdown().await.expect("shutdown C");
    p.shutdown().await.expect("shutdown P");
}
