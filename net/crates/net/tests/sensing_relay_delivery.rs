//! SI-4 (SENSING_INTEREST_COALESCING_PLAN v4.3, §3.5/§4.4/§4.9):
//! relay delivery + overlay application on real sessions — the
//! flagship two-watcher flow plus test 16b's real-path half.
//!
//! Topology — four real nodes:
//!
//! ```text
//!   A (D = 100 ms) ─┐
//!                   R ── P (origin, floor 50 ms)
//!   B (D = 500 ms) ─┘
//! ```
//!
//! Phases:
//! 1. **16b — demand merges, one provider stream:** A and B register
//!    the IDENTICAL interest through R; R holds ONE branch entry
//!    with two rows, P holds ONE Peer(R) row and runs ONE live
//!    stream. Both watchers receive origin-signed proofs from that
//!    single stream, verified at their own hops.
//! 2. **Down-sampling at each watcher's own D:** A observes the
//!    ~100 ms cadence, B is delivered at ~500 ms — never the origin
//!    cadence — and neither is ever false-Unknowned.
//! 3. **LOCAL aggregate views (§3.5):** A's Layer-1 surface projects
//!    Ready with P supporting; the §4.9 overlay change signal fires
//!    on the projection edge.
//! 4. **Status edge immediacy:** a NotReady flip reaches BOTH
//!    watchers well inside B's 500 ms schedule — edges are never
//!    held by the down-sampler.
//! 5. **Hop rule on the real path (SI-0 test 13's tripwire):** the
//!    origin dies; R's upstream continuity expires; a late joiner is
//!    warm-started from R's cache on the PROVISIONAL stream and must
//!    project Unknown despite holding a verified cached Ready — a
//!    real-session cache chain cannot launder continuity.
//!
//! Run: `cargo test --test sensing_relay_delivery`

#![cfg(feature = "net")]

mod common;
use common::*;

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::behavior::sensing::{
    AggregateView, AttestedStatus, AudienceScopeCommitment, CanonicalConstraints, CapabilityId,
    ConsumerLatencyBudget, DisclosureClass, EvaluationRequest, Incarnation, InterestSpec,
    ProjectedReadiness, ProviderInterestKey, ProviderSelector, ReadinessEvaluation,
    ReadinessEvaluator, ResultMode, SensingCounters, StatusReason, WorkLatencyEnvelope,
};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};
use net::adapter::Adapter;

const STRICT_D: Duration = Duration::from_millis(100);
const LOOSE_D: Duration = Duration::from_millis(500);
const TTL: Duration = Duration::from_millis(1500);
/// The plan's ttl/2 refresh discipline — deliberately NOT faster:
/// every registration re-sends the cached latest as §4.4
/// anti-entropy, so an over-eager refresh loop would dominate a
/// loose watcher's delivery schedule and blur the down-sampling
/// measurement below.
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

fn spawn_refresher(
    node: Arc<MeshNode>,
    spec: InterestSpec,
    provider: u64,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let _ = node.register_sensing_interest(&spec, provider, interval, TTL);
            tokio::time::sleep(REFRESH).await;
        }
    })
}

#[tokio::test]
async fn flagship_two_watchers_one_stream_and_the_hop_rule() {
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
    let b = mk(None).await;
    let c2 = mk(None).await;
    let r = mk(None).await;
    let p = mk(Some(Incarnation::new(1))).await;
    let ready = Arc::new(AtomicBool::new(true));
    p.register_readiness_evaluator(
        CapabilityId::new("print.document"),
        Arc::new(FlagEvaluator {
            ready: ready.clone(),
        }),
    );

    connect_pair(&a, &r).await;
    connect_pair(&b, &r).await;
    connect_pair(&c2, &r).await;
    connect_pair(&r, &p).await;
    for node in [&a, &b, &c2, &r, &p] {
        node.start();
    }
    for node in [&a, &b, &c2, &r, &p] {
        node.announce_capabilities(CapabilitySet::new())
            .await
            .expect("announce");
    }
    let (a_id, b_id, c2_id, r_id, p_id) = (
        a.node_id(),
        b.node_id(),
        c2.node_id(),
        r.node_id(),
        p.node_id(),
    );
    await_condition(Duration::from_secs(5), "entity pins established", || {
        r.peer_entity_id(a_id).is_some()
            && r.peer_entity_id(b_id).is_some()
            && r.peer_entity_id(c2_id).is_some()
            && r.peer_entity_id(p_id).is_some()
            && p.peer_entity_id(r_id).is_some()
    })
    .await;
    // Consumers reach P through R; the origin pin at each verifying
    // hop is the documented SI-3 seam bound (pin propagation rides
    // a later slice).
    let p_entity = p.entity_keypair().entity_id().clone();
    for watcher in [&a, &b, &c2] {
        watcher.router().add_route(p_id, r.local_addr());
        watcher.test_pin_peer_entity(p_id, p_entity.clone());
    }

    let spec = shared_spec(fleet);
    let branch = ProviderInterestKey::new(spec.key(), p_id);
    use net::adapter::net::behavior::sensing::DownstreamId;

    // ── Phase 1 (16b): demand merges — one branch at R, ONE
    //    provider stream at P ──
    let refresh_a = spawn_refresher(a.clone(), spec.clone(), p_id, STRICT_D);
    let refresh_b = spawn_refresher(b.clone(), spec.clone(), p_id, LOOSE_D);
    await_condition(Duration::from_secs(10), "both rows at R", || {
        let rows = r.sensing_downstreams(&branch);
        rows.contains(&DownstreamId::Peer(a_id)) && rows.contains(&DownstreamId::Peer(b_id))
    })
    .await;
    assert_eq!(r.sensing_interest_count(), 1, "ONE branch entry at R");
    await_condition(Duration::from_secs(10), "one stream at P", || {
        p.sensing_live_streams() == 1
    })
    .await;
    assert_eq!(
        p.sensing_downstreams(&branch),
        vec![DownstreamId::Peer(r_id)],
        "16b: demand merged BEFORE the provider hop — one Peer(R) row",
    );

    // Both watchers receive verified beats from that single stream.
    await_condition(Duration::from_secs(10), "beats at A and B", || {
        a.sensing_latest_attestation(&branch).is_some()
            && b.sensing_latest_attestation(&branch).is_some()
    })
    .await;
    for (who, node) in [("A", &a), ("B", &b)] {
        let latest = node.sensing_latest_attestation(&branch).expect("present");
        assert_eq!(latest.origin, p_id, "{who}'s proof is origin-signed by P");
        assert_eq!(latest.status, AttestedStatus::Ready);
    }

    // ── Phase 2: down-sampling at each watcher's own D. Each ttl/2
    //    refresh ALSO warm-re-sends the cached latest (§4.4
    //    anti-entropy — plan-correct), so B's count gets a small
    //    allowance for those before the 2× separation is asserted. ──
    let mut seqs_a: HashSet<u64> = HashSet::new();
    let mut seqs_b: HashSet<u64> = HashSet::new();
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if let Some(latest) = a.sensing_latest_attestation(&branch) {
            seqs_a.insert(latest.seq);
        }
        if let Some(latest) = b.sensing_latest_attestation(&branch) {
            seqs_b.insert(latest.seq);
        }
    }
    let warm_allowance = 3;
    assert!(
        seqs_a.len() >= 2 * seqs_b.len().saturating_sub(warm_allowance),
        "B must be down-sampled to its own D, not the origin cadence \
         (A saw {} distinct beats, B saw {})",
        seqs_a.len(),
        seqs_b.len(),
    );
    assert!(
        seqs_a.len() > seqs_b.len(),
        "the strict watcher sees strictly more of the stream \
         (A {}, B {})",
        seqs_a.len(),
        seqs_b.len(),
    );
    assert!(!seqs_b.is_empty(), "B still receives its cadence");

    // ── Phase 3: LOCAL aggregate views (§3.5) + the §4.9 overlay
    //    change signal ──
    assert_eq!(a.sensing_projected(&branch), ProjectedReadiness::Ready);
    let view = a.sensing_aggregate_view(&spec, &ConsumerLatencyBudget::default(), false);
    match view {
        AggregateView::Scalar { status, supporting } => {
            assert_eq!(status, ProjectedReadiness::Ready);
            assert_eq!(supporting, vec![p_id], "P supports the aggregate");
        }
        other => panic!("Any-mode aggregates to Scalar, got {other:?}"),
    }
    let mut overlay_rx = a.subscribe_sensing_overlay_changes();
    let overlay_before = *overlay_rx.borrow_and_update();

    // ── Phase 4: a status edge reaches BOTH watchers well inside
    //    B's 500 ms schedule — never held by the down-sampler ──
    ready.store(false, Ordering::Relaxed);
    p.notify_sensing_state_changed(&CapabilityId::new("print.document"));
    await_condition(Duration::from_millis(400), "edge at both watchers", || {
        let edge = |node: &Arc<MeshNode>| {
            node.sensing_latest_attestation(&branch)
                .is_some_and(|latest| {
                    latest.status == AttestedStatus::NotReady
                        && latest.status_reason == StatusReason::Provider(7)
                })
        };
        edge(&a) && edge(&b)
    })
    .await;
    await_condition(Duration::from_secs(2), "overlay signal fired", || {
        *overlay_rx.borrow_and_update() != overlay_before
    })
    .await;
    await_condition(Duration::from_secs(2), "A's projection follows", || {
        a.sensing_projected(&branch) == ProjectedReadiness::NotReady
    })
    .await;

    // ── Phase 5: the hop rule on the real path — a dead origin's
    //    cache cannot launder continuity to a late joiner ──
    ready.store(true, Ordering::Relaxed);
    p.notify_sensing_state_changed(&CapabilityId::new("print.document"));
    await_condition(Duration::from_secs(2), "back to Ready at R's cache", || {
        r.sensing_latest_attestation(&branch)
            .is_some_and(|latest| latest.status == AttestedStatus::Ready)
    })
    .await;
    p.shutdown().await.expect("shutdown P");
    // R's upstream continuity window (3 × max(cadence, aggregate D))
    // expires; A/B keep the rows and R's cache alive.
    await_condition(
        Duration::from_secs(5),
        "R's upstream continuity dies",
        || {
            r.sensing_upstream_continuity(&branch)
                .is_some_and(|continuity| {
                    continuity != net::adapter::net::behavior::sensing::Continuity::Established
                })
        },
    )
    .await;

    // The late joiner: warm-started from R's cache on the
    // PROVISIONAL stream. It holds a VERIFIED cached Ready — and
    // must still project Unknown (SI-0 test 13, real sessions).
    let refresh_c2 = spawn_refresher(c2.clone(), spec.clone(), p_id, STRICT_D);
    await_condition(Duration::from_secs(10), "C2 warm-started", || {
        c2.sensing_latest_attestation(&branch).is_some()
    })
    .await;
    assert_eq!(
        c2.sensing_latest_attestation(&branch)
            .expect("present")
            .status,
        AttestedStatus::Ready,
        "the cached proof itself is a verified Ready",
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        c2.sensing_projected(&branch),
        ProjectedReadiness::Unknown,
        "real-path cache laundering: a provisional warm-start from a dead \
         origin must never project optimism",
    );

    // Nothing on the whole flow was protocol-invalid anywhere.
    for node in [&a, &b, &c2, &r] {
        let counters = node.sensing_counters();
        assert_eq!(SensingCounters::get(&counters.protocol_invalid), 0);
        assert_eq!(SensingCounters::get(&counters.scope_refusals), 0);
    }

    refresh_a.abort();
    refresh_b.abort();
    refresh_c2.abort();
    a.shutdown().await.expect("shutdown A");
    b.shutdown().await.expect("shutdown B");
    c2.shutdown().await.expect("shutdown C2");
    r.shutdown().await.expect("shutdown R");
}
