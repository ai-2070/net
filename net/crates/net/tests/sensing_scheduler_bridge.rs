//! SI-6 (SENSING_INTEREST_COALESCING_PLAN §6/§4.9): sensed aggregate
//! views join the gang scheduler's candidate pruning through the same
//! projection seam as local liveness — on REAL sensed state.
//!
//! Topology (the proven R == P shape): A is consumer AND
//! self-provider (flippable evaluator, cheap start estimate); R is
//! leader AND provider (owner identity, expensive start estimate).
//! A holds two live Ready cells under one digest, with the
//! consumer-local economics ranking A's own branch first. Gang-side,
//! A's folds carry one island per host, with the LOAD order the
//! REVERSE of the sensed order — so a sensed-led claim order is
//! unambiguously the sensing join, never the selection policy.
//!
//! Run: `cargo test --features redex --test sensing_scheduler_bridge`

#![cfg(all(feature = "net", feature = "redex"))]

mod common;
use common::*;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::behavior::fold::{
    CapabilityFilter, CapabilityFold, CapabilityMembership, CapabilityQuery, EnvelopeMeta,
    FoldKind, IslandRecord, IslandTopologyFold, NodeState, SignedAnnouncement, UnitSet,
};
use net::adapter::net::behavior::gang::{MatchCriteria, NumericFilter, SelectionPolicy};
use net::adapter::net::behavior::sensing::{
    AggregateView, AudienceScopeCommitment, CanonicalConstraints, CapabilityId,
    ConsumerLatencyBudget, DisclosureClass, EvaluationRequest, Incarnation, InterestSpec,
    ProjectedReadiness, ProviderInterestKey, ProviderSelector, ReadinessEvaluation,
    ReadinessEvaluator, ResultMode, WorkLatencyEnvelope,
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

/// A's evaluator: flippable readiness with a MUTABLE start estimate
/// (millis) — self route is zero, so A ranks first while Ready and
/// cheap; raising the estimate reverses the rank with NO status
/// edge (the SI-6 review's economics-only witness).
struct FlagEvaluator {
    ready: Arc<AtomicBool>,
    start_ms: Arc<AtomicU64>,
}

impl ReadinessEvaluator for FlagEvaluator {
    fn evaluate(&self, _request: &EvaluationRequest<'_>) -> ReadinessEvaluation {
        if self.ready.load(Ordering::Relaxed) {
            ReadinessEvaluation::Ready {
                estimated_start: Some(Duration::from_millis(self.start_ms.load(Ordering::Relaxed))),
            }
        } else {
            ReadinessEvaluation::NotReady { reason: 7 }
        }
    }
}

/// R's evaluator: always Ready, but EXPENSIVE to start — ranks
/// behind A on the consumer-local economics whenever A is viable.
struct SlowReady;

impl ReadinessEvaluator for SlowReady {
    fn evaluate(&self, _request: &EvaluationRequest<'_>) -> ReadinessEvaluation {
        ReadinessEvaluation::Ready {
            estimated_start: Some(Duration::from_millis(500)),
        }
    }
}

/// Gang-style capability membership for `node` (signed by its real
/// keypair) — the same shape the gang matcher's step-1 query reads.
fn announce_gang_capability(node: &Arc<MeshNode>, kp: &EntityKeypair, node_id: u64) {
    let membership = CapabilityMembership {
        class_hash: 0x67_70_75,
        tags: vec!["gpu:h100".into()],
        hardware: None,
        state: NodeState::Idle,
        region: None,
        price_quote: None,
        reflex_addr: None,
        allowed_nodes: Vec::new(),
        allowed_subnets: Vec::new(),
        allowed_groups: Vec::new(),
        metadata: BTreeMap::new(),
        owner_org: None,
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

/// One island hosted by `node_id`, at `load`.
fn announce_island(node: &Arc<MeshNode>, kp: &EntityKeypair, node_id: u64, island: u64, load: f32) {
    let record = IslandRecord {
        id: island,
        units: UnitSet::new((0..8u32).collect()),
        host: node_id,
        capabilities: vec!["model:a1".into()],
        load,
        p50_latency_us: 1_500,
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
async fn sensed_readiness_leads_the_claim_order_and_never_suspends() {
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
    let a = mk(EntityKeypair::generate(), Some(Incarnation::new(1))).await;
    let r = mk(owner_kp, Some(Incarnation::new(1))).await;
    let a_ready = Arc::new(AtomicBool::new(true));
    let a_start_ms = Arc::new(AtomicU64::new(3));
    a.register_readiness_evaluator(
        CapabilityId::new("print.document"),
        Arc::new(FlagEvaluator {
            ready: a_ready.clone(),
            start_ms: a_start_ms.clone(),
        }),
    );
    r.register_readiness_evaluator(CapabilityId::new("print.document"), Arc::new(SlowReady));

    connect_pair(&a, &r).await;
    a.start();
    r.start();
    a.announce_capabilities(CapabilitySet::new())
        .await
        .expect("announce A");
    r.announce_capabilities(CapabilitySet::new().add_tag("print.document"))
        .await
        .expect("announce R");

    let (a_id, r_id) = (a.node_id(), r.node_id());
    await_condition(Duration::from_secs(5), "entity pins established", || {
        a.peer_entity_id(r_id).is_some() && r.peer_entity_id(a_id).is_some()
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

    // ── Two REAL sensed branches at A under one digest ──
    let spec = shared_spec(owner);
    let self_branch = ProviderInterestKey::new(spec.key(), a_id);
    let leader_branch = ProviderInterestKey::new(spec.key(), r_id);
    a.register_sensing_interest(&spec, a_id, Duration::from_millis(100), TTL)
        .expect("self-register");
    let refresh_a = {
        let a = a.clone();
        let spec = spec.clone();
        tokio::spawn(async move {
            loop {
                let _ = a.register_sensing_interest(&spec, a_id, Duration::from_millis(100), TTL);
                let _ =
                    a.register_capability_interest(&spec, r_id, Duration::from_millis(100), TTL);
                tokio::time::sleep(REFRESH).await;
            }
        })
    };
    await_condition(Duration::from_secs(10), "both branches Ready at A", || {
        a.sensing_projected(&self_branch) == ProjectedReadiness::Ready
            && a.sensing_projected(&leader_branch) == ProjectedReadiness::Ready
    })
    .await;

    // ── Gang folds at A: one island per host, LOAD order the
    //    REVERSE of the sensed order ──
    let a_kp = a.entity_keypair();
    announce_gang_capability(&a, a_kp, a_id);
    announce_gang_capability(&a, r.entity_keypair(), r_id);
    let (island_a, island_r) = (0xA1u64, 0xB1u64);
    announce_island(&a, a_kp, a_id, island_a, 0.5);
    announce_island(&a, r.entity_keypair(), r_id, island_r, 0.1);

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
    let budget = ConsumerLatencyBudget::default();

    // Baseline: the selection policy alone puts R's island first
    // (least-loaded).
    assert_eq!(
        a.match_islands(&criteria),
        vec![island_r, island_a],
        "baseline claim order is the selection policy's",
    );

    // ── The SI-6 join: A's own branch is the aggregate's SELECTED
    //    provider (self route 0 + 3 ms start vs R's 500 ms), so its
    //    island leads DESPITE the load order ──
    let sensed = a.sensed_candidates(&spec, &budget, None);
    assert_eq!(sensed.viable, vec![a_id, r_id], "aggregate economics");
    assert_eq!(sensed.selected_provider(), Some(a_id));
    assert_eq!(
        a.match_islands_sensed(&criteria, &spec, &budget, None),
        vec![island_a, island_r],
        "sensed rank leads the claim order",
    );
    // The claim targets the selected provider.
    let claimed = a
        .claim_island_sensed(&criteria, &spec, &budget, None, u64::MAX)
        .await
        .expect("claim")
        .expect("an island is claimable");
    assert_eq!(claimed, island_a, "the claim targets the SELECTED provider");

    // ── §4.9 overlay accessor: aggregate + per-(provider, gen)
    //    observations, joined at read time ──
    let overlay = a.sensing_readiness_overlay(&spec, &budget, None);
    assert_eq!(
        overlay.aggregate,
        AggregateView::Scalar {
            status: ProjectedReadiness::Ready,
            supporting: vec![a_id, r_id],
        },
    );
    assert_eq!(overlay.candidates.len(), 2, "both observations joined");
    // SI-6 review P1: the candidates half rides the SAME resolved-
    // population seam as the aggregate — A's cell is live and
    // retained, yet a population of [r_id] must exclude it from
    // BOTH halves.
    let scoped = a.sensing_readiness_overlay(&spec, &budget, Some(&[r_id]));
    assert_eq!(
        scoped.aggregate,
        AggregateView::Scalar {
            status: ProjectedReadiness::Ready,
            supporting: vec![r_id],
        },
    );
    assert_eq!(
        scoped
            .candidates
            .iter()
            .map(|((provider, _), _)| *provider)
            .collect::<Vec<_>>(),
        vec![r_id],
        "resolved population must filter the overlay candidates",
    );

    // ── SI-6 review P1 (reviewer-reproduced): a Ready→Ready
    //    ECONOMICS change must wake the scheduler. A's signed start
    //    estimate rises 3 ms → 1000 ms (readiness unchanged); the
    //    selection reverses to R and the unified scheduler-input
    //    watch fires ──
    let signal = a.subscribe_sensing_scheduler_inputs();
    let generation_before = *signal.borrow();
    a_start_ms.store(1000, Ordering::Relaxed);
    a.notify_sensing_state_changed(&cap);
    await_condition(
        Duration::from_secs(5),
        "economics flip reverses the selection",
        || {
            a.sensed_candidates(&spec, &budget, None)
                .selected_provider()
                == Some(r_id)
        },
    )
    .await;
    assert!(
        *signal.borrow() > generation_before,
        "a Ready→Ready economics change must wake the scheduler",
    );

    // ── SI-6 review P1, fold axis: a capability-fold membership
    //    change fires the SAME unified watch with NO sensing-state
    //    movement at all ──
    let n = mk(EntityKeypair::generate(), None).await;
    connect_pair(&a, &n).await;
    n.start();
    // Consume the session-open topology bump first, so the next
    // assertion isolates the FOLD axis.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let generation_before = *signal.borrow();
    let projections_before = (
        a.sensing_projected(&self_branch),
        a.sensing_projected(&leader_branch),
    );
    n.announce_capabilities(CapabilitySet::new().add_tag("unrelated:tag"))
        .await
        .expect("announce N");
    await_condition(
        Duration::from_secs(5),
        "a fold membership change wakes the scheduler",
        || *signal.borrow() > generation_before,
    )
    .await;
    assert_eq!(
        (
            a.sensing_projected(&self_branch),
            a.sensing_projected(&leader_branch),
        ),
        projections_before,
        "the fold-axis wake carried no sensing-state movement",
    );

    // ── The flip: A's own readiness goes NotReady — sensed prune,
    //    never a suspension ──
    let signal = a.subscribe_sensing_overlay_changes();
    let generation_before = *signal.borrow();
    a_ready.store(false, Ordering::Relaxed);
    a.notify_sensing_state_changed(&cap);
    await_condition(Duration::from_secs(5), "self branch flips NotReady", || {
        a.sensing_projected(&self_branch) == ProjectedReadiness::NotReady
    })
    .await;
    assert!(
        *signal.borrow() > generation_before,
        "the overlay signal is the re-match wake-up",
    );

    // Re-match: A's host is pruned FOR THIS INTEREST; R's island is
    // the claim target now.
    let sensed = a.sensed_candidates(&spec, &budget, None);
    assert_eq!(sensed.non_viable, vec![a_id]);
    assert_eq!(sensed.selected_provider(), Some(r_id));
    assert_eq!(
        a.match_islands_sensed(&criteria, &spec, &budget, None),
        vec![island_r],
        "a NotReady host is pruned from THIS match",
    );
    let claimed = a
        .claim_island_sensed(&criteria, &spec, &budget, None, u64::MAX)
        .await
        .expect("claim")
        .expect("the fallback island is claimable");
    assert_eq!(
        claimed, island_r,
        "the claim follows the surviving provider"
    );

    // ── The §4.9 tripwire: one interest's NotReady never suspends
    //    the capability entry — the PLAIN match still offers both
    //    hosts' islands ──
    assert_eq!(
        a.match_islands(&criteria),
        vec![island_r, island_a],
        "no suspension: unrelated matching is untouched",
    );

    refresh_a.abort();
    a.shutdown().await.expect("shutdown A");
    r.shutdown().await.expect("shutdown R");
    n.shutdown().await.expect("shutdown N");
}
