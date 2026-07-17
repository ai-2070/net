//! ICB-5 — sensed selection through a single claim / fallback (two boundaries).
//!
//! SINGLE CLAIMANT throughout. Rides the SI-6 sensed-selection path on REAL
//! sensed state (`net redex`): `MeshNode::{sensed_candidates, match_islands_sensed,
//! claim_island_sensed}`. Sensed readiness may PRUNE (NotReady) or RERANK
//! (economics) candidates, but selection and reservation authority stay
//! separate, and the fallback still exercises the real production claim loop
//! (`claim_island_sensed` iterates `match_islands_sensed` and `reserve_island`s
//! each — the same reject-walk ICB-4 measured, now over the sensed order).
//!
//! # The opposing seed (Kyra: sensed order verified against an opposing seed)
//!
//! Two providers, one island each, with the LOAD order the REVERSE of the
//! sensed order — so a sensed-led claim order is unambiguously the sensing
//! join, never the selection policy:
//!   - A: self-provider, cheap start (3 ms) → sensed-SELECTED; island_a load 0.5.
//!   - R: leader + provider, expensive start (500 ms) → sensed second; island_r
//!     load 0.1.
//!
//! Baseline `match_islands` (LeastLoaded) puts island_r first; the sensed join
//! (`match_islands_sensed`) puts island_a first.
//!
//! # Two boundaries (M4 split — different failure modes)
//!
//!   - ICB-5a — SENSED RE-SELECTION (the SELECTION changed): an exact readiness
//!     overlay change makes A NotReady, so R becomes the selected provider; an
//!     on-demand `claim_island_sensed` claims R's island directly — a DIFFERENT
//!     provider, with NO reservation rejection (the change is in selection, not
//!     a reservation conflict). On-demand: the bench calls the API after the
//!     overlay change; no automatic scheduler invocation is implied.
//!   - ICB-5b — RESERVATION FALLBACK (the selection is UNCHANGED): A stays
//!     sensed-selected and first, but its island is pre-held by H → the first
//!     reservation apply is `Rejected` → `claim_island_sensed` walks to the next
//!     candidate (R's island) → the fallback commits. The sensed equivalent of
//!     ICB-4: rejected-apply delta EXACTLY ONE, direct observer visibility.
//!
//! Absence of sensed evidence never becomes negative evidence: an UNSENSED host
//! (no readiness branch) is retained in `match_islands_sensed` (W6 — this
//! benchmark proves the UNSENSED case only; `potential`/Unknown retention is a
//! production unit concern, not re-claimed here); a NotReady host is pruned from
//! THIS match only and never suspends the capability entry, proven both
//! behaviorally (plain match) and structurally (byte-identical capability entry)
//! (W5, §4.9).
//!
//! The pre-held first island is PRE-CONVERGED at BOTH A and O, so A's losing
//! reserve of it is rejected at O (O already knows H) — the known-state first
//! island stays `H` at both nodes and only ISLAND_R moves under the fallback.
//! Every fallback sample asserts the full shape at both nodes.
//!
//! Results are localhost MECHANISM/orientation evidence: no arbitration,
//! tie-break, quorum, or fencing is added to make anything pass; ICB-3's
//! distributed-contention result is unchanged; no threshold (ICB-7). Capability
//! propagation latency is NOT restated here (CPB owns it).
//!
//! Run: `cargo bench -p net-mesh --bench island_claim_sensed --features redex`

#[path = "bench_island_claim/mod.rs"]
mod bench_island_claim;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bench_island_claim::{
    apply_reserve, await_reservation_holder, connect, far_deadline, holder_of, node,
    release_and_await_free, runtime, wait_until, LatencyReport, PSK, WORKER_THREADS,
};
use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::behavior::fold::{
    CapabilityFilter, CapabilityFold, CapabilityMembership, CapabilityQuery, EnvelopeMeta,
    FoldKind, IslandRecord, IslandTopologyFold, NodeState, ReservationQuery, SignedAnnouncement,
    UnitSet,
};
use net::adapter::net::behavior::gang::{MatchCriteria, NumericFilter, SelectionPolicy};
use net::adapter::net::behavior::sensing::{
    AudienceScopeCommitment, CanonicalConstraints, CapabilityId, ConsumerLatencyBudget,
    DisclosureClass, EvaluationRequest, Incarnation, InterestSpec, ProjectedReadiness,
    ProviderInterestKey, ProviderSelector, ReadinessEvaluation, ReadinessEvaluator, ResultMode,
    WorkLatencyEnvelope,
};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig};

/// Sensing capability id the interest is registered under.
const CAP: &str = "print.document";
/// Gang matcher tag + class (separate capability space from the sensing one).
const MATCH_TAG: &str = "gpu:h100";
const GANG_CLASS: u64 = 0x67_70_75;
const MIN_UNITS: usize = 8;
const ISLAND_UNITS: u32 = 8;
/// A's island (host A). Higher load — ranks LAST by policy, FIRST by sensing.
const ISLAND_A: u64 = 0x5C00_00A1;
const LOAD_A: f32 = 0.5;
/// R's island (host R). Lower load — ranks FIRST by policy, second by sensing.
const ISLAND_R: u64 = 0x5C00_00B1;
const LOAD_R: f32 = 0.1;
/// An UNSENSED host's island (no readiness branch) — for the absence-of-
/// evidence retention witness (W6). Fictional host id.
const HOST_X: u64 = 0x5C00_0C01;
const ISLAND_X: u64 = 0x5C00_00C1;
/// Explicit long fixture TTL so the fold sweeper never removes a seeded
/// capability / island entry mid-run (the fixture is seeded once for the whole
/// bench — a loaded run must not lose islands halfway through).
const FIXTURE_TTL_SECS: u32 = 3_600;

/// Interest cadence (mirrors the SI-6 bridge witness).
const TTL: Duration = Duration::from_millis(1500);
const REFRESH: Duration = Duration::from_millis(750);
/// Ceiling for a direct remote-visibility read (localhost).
const REMOTE_DEADLINE: Duration = Duration::from_secs(2);
const RESET_TIMEOUT: Duration = Duration::from_secs(2);
/// Ceiling for one sensed-overlay transition to settle.
const OVERLAY_SETTLE: Duration = Duration::from_secs(5);

const SAMPLES: u64 = 20;
const WARMUP: u64 = 3;

fn main() {
    let rt = runtime();
    rt.block_on(async {
        println!("\n=== ICB-5 sensed selection / fallback (single claimant) ===\n");

        let fix = build_fixture().await;
        let budget = ConsumerLatencyBudget::default();

        println!("-- witnesses --");
        w1_opposing_order(&fix, &budget);
        w2_selected_receives_first_claim(&fix, &budget).await;
        w6_absence_of_evidence_retained(&fix, &budget);
        // From here ISLAND_A is the pre-CONVERGED known-state first island
        // (held by H at BOTH A and O), so the fallback moves only ISLAND_R.
        preconverge_first_island(&fix).await;
        w4_sensed_reservation_fallback(&fix, &budget).await;
        w3_w5_reselection_and_no_suspension(&fix, &budget).await;

        println!("\n-- measurement --");
        measure_5b(&fix, &budget).await;
        measure_5a(&fix, &budget).await;
        measure_projection_overhead(&fix, &budget);

        fix.refresh.abort();
    });
}

// ============================================================================
// Sensed fixture (real sensing state — mirrors tests/sensing_scheduler_bridge).
// ============================================================================

/// A's evaluator: flippable readiness + mutable start estimate (self route is
/// zero, so A ranks first while Ready and cheap).
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

/// R's evaluator: always Ready but EXPENSIVE — ranks behind A whenever A is
/// viable.
struct SlowReady;

impl ReadinessEvaluator for SlowReady {
    fn evaluate(&self, _request: &EvaluationRequest<'_>) -> ReadinessEvaluation {
        ReadinessEvaluation::Ready {
            estimated_start: Some(Duration::from_millis(500)),
        }
    }
}

fn sensing_config(owner: AudienceScopeCommitment) -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().expect("bind addr");
    MeshNodeConfig::new(addr, PSK)
        .with_announce_debounce(Duration::ZERO)
        .with_min_announce_interval(Duration::ZERO)
        .with_sensing_coalescing(true)
        .with_sensing_owner_root(owner)
        .with_sensing_incarnation(Incarnation::new(1))
}

fn shared_spec(owner: AudienceScopeCommitment) -> InterestSpec {
    InterestSpec {
        capability_id: CapabilityId::new(CAP),
        constraints: CanonicalConstraints::from_entries([("media", "a4")]).expect("constraints"),
        work_latency: WorkLatencyEnvelope::start_within(Duration::from_secs(5)),
        providers: ProviderSelector::AnyAuthorized,
        result_mode: ResultMode::Any,
        disclosure_class: DisclosureClass::Owner,
        audience: owner,
    }
}

fn criteria() -> MatchCriteria {
    MatchCriteria {
        capability: CapabilityQuery::Composite(CapabilityFilter {
            tags_all: vec![MATCH_TAG.to_string()],
            ..Default::default()
        }),
        numeric: NumericFilter {
            min_units: MIN_UNITS,
            ..Default::default()
        },
        selection: SelectionPolicy::LeastLoaded,
        prefer_capability: None,
    }
}

/// Explicit long-TTL fixture envelope (nothing expires mid-run).
fn fixture_meta() -> EnvelopeMeta {
    EnvelopeMeta {
        announced_at: 0,
        ttl_secs: Some(FIXTURE_TTL_SECS),
        flags: 0,
    }
}

/// Gang capability membership for `host_id` on `node`'s fold.
fn seed_gang_capability(node: &Arc<MeshNode>, signer: &EntityKeypair, host_id: u64) {
    let membership = CapabilityMembership {
        class_hash: GANG_CLASS,
        tags: vec![MATCH_TAG.to_string()],
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
        signer,
        CapabilityFold::KIND_ID,
        GANG_CLASS,
        host_id,
        1,
        fixture_meta(),
        membership,
    )
    .expect("sign gang cap");
    node.capability_fold().apply(ann).expect("apply gang cap");
}

/// One island of [`ISLAND_UNITS`] units hosted by `host_id` at `load`.
fn seed_island(node: &Arc<MeshNode>, signer: &EntityKeypair, host_id: u64, island: u64, load: f32) {
    let record = IslandRecord {
        id: island,
        units: UnitSet::new((0..ISLAND_UNITS).collect()),
        host: host_id,
        capabilities: vec!["model:a1".to_string()],
        load,
        p50_latency_us: 1_500,
    };
    let ann = SignedAnnouncement::sign(
        signer,
        IslandTopologyFold::KIND_ID,
        0,
        host_id,
        1,
        fixture_meta(),
        record,
    )
    .expect("sign island");
    node.island_fold().apply(ann).expect("apply island");
}

struct Fixture {
    a: Arc<MeshNode>, // claimant + consumer + self-provider
    // Held to keep the leader/provider session (and its sensed branch) alive —
    // dropping it would tear down the sensing rendezvous.
    #[allow(dead_code)]
    r: Arc<MeshNode>,
    o: Arc<MeshNode>, // direct observer (reads its OWN exact holder — M7-2)
    h: EntityKeypair, // pre-existing holder identity for ISLAND_A
    spec: InterestSpec,
    a_ready: Arc<AtomicBool>,
    a_id: u64,
    r_id: u64,
    self_branch: ProviderInterestKey,
    cap: CapabilityId,
    refresh: tokio::task::JoinHandle<()>,
}

async fn build_fixture() -> Fixture {
    let owner_kp = EntityKeypair::generate();
    let owner_entity = owner_kp.entity_id().clone();
    let owner = AudienceScopeCommitment::owner_root(&owner_entity);

    let a = Arc::new(
        MeshNode::new(EntityKeypair::generate(), sensing_config(owner))
            .await
            .expect("A"),
    );
    let r = Arc::new(
        MeshNode::new(owner_kp, sensing_config(owner))
            .await
            .expect("R"),
    );
    let o = node().await; // plain observer (same PSK)

    let a_ready = Arc::new(AtomicBool::new(true));
    let a_start_ms = Arc::new(AtomicU64::new(3));
    a.register_readiness_evaluator(
        CapabilityId::new(CAP),
        Arc::new(FlagEvaluator {
            ready: a_ready.clone(),
            start_ms: a_start_ms.clone(),
        }),
    );
    r.register_readiness_evaluator(CapabilityId::new(CAP), Arc::new(SlowReady));

    connect(&a, &r).await;
    connect(&a, &o).await;
    a.start_arc();
    r.start_arc();
    o.start_arc();

    a.announce_capabilities(CapabilitySet::new())
        .await
        .expect("announce A");
    r.announce_capabilities(CapabilitySet::new().add_tag(CAP))
        .await
        .expect("announce R");
    o.announce_capabilities(CapabilitySet::new())
        .await
        .expect("announce O");

    let (a_id, r_id) = (a.node_id(), r.node_id());
    assert!(
        wait_until(Duration::from_secs(5), || {
            a.peer_entity_id(r_id).is_some()
                && r.peer_entity_id(a_id).is_some()
                && o.peer_entity_id(a_id).is_some()
        })
        .await,
        "entity pins (A↔R and O←A) must establish"
    );

    assert!(r.assume_sensing_leader(), "leader role installs at R");
    let cap = CapabilityId::new(CAP);
    assert!(
        wait_until(Duration::from_secs(5), || {
            r.sensing_candidate_snapshot(&cap)
                .iter()
                .any(|c| c.node_id == r_id && c.authorized)
        })
        .await,
        "R's snapshot authorizes itself"
    );

    let spec = shared_spec(owner);
    let self_branch = ProviderInterestKey::new(spec.key(), a_id);
    let leader_branch = ProviderInterestKey::new(spec.key(), r_id);
    a.register_sensing_interest(&spec, a_id, Duration::from_millis(100), TTL)
        .expect("self-register");
    let refresh = {
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
    assert!(
        wait_until(Duration::from_secs(10), || {
            a.sensing_projected(&self_branch) == ProjectedReadiness::Ready
                && a.sensing_projected(&leader_branch) == ProjectedReadiness::Ready
        })
        .await,
        "both sensed branches Ready at A"
    );

    // Gang folds at A: one island per host, LOAD order the REVERSE of sensed.
    let a_kp = a.entity_keypair();
    seed_gang_capability(&a, a_kp, a_id);
    seed_gang_capability(&a, a_kp, r_id);
    seed_island(&a, a_kp, a_id, ISLAND_A, LOAD_A);
    seed_island(&a, a_kp, r_id, ISLAND_R, LOAD_R);

    Fixture {
        a,
        r,
        o,
        h: EntityKeypair::generate(),
        spec,
        a_ready,
        a_id,
        r_id,
        self_branch,
        cap,
        refresh,
    }
}

/// Islands `node_id` holds (Reserved/Active) on `node`'s reservation fold.
fn held_islands(node: &Arc<MeshNode>, node_id: u64) -> Vec<u64> {
    let mut ids: Vec<u64> = node
        .reservation_fold()
        .query(ReservationQuery::HeldBy(node_id))
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    ids.sort_unstable();
    ids
}

/// Wait until A's sensed selected provider equals `expected` (fail-loud).
async fn await_selected(fix: &Fixture, budget: &ConsumerLatencyBudget, expected: u64) {
    assert!(
        wait_until(OVERLAY_SETTLE, || {
            fix.a
                .sensed_candidates(&fix.spec, budget, None)
                .selected_provider()
                == Some(expected)
        })
        .await,
        "sensed selected provider must settle to {expected:#x}"
    );
}

/// The gang capability membership `host_id` publishes in `node`'s fold.
fn gang_membership(node: &Arc<MeshNode>, host_id: u64) -> CapabilityMembership {
    node.capability_fold()
        .query(CapabilityQuery::InClass(GANG_CLASS))
        .into_iter()
        .find(|((_, id), _)| *id == host_id)
        .map(|(_, m)| m)
        .unwrap_or_else(|| panic!("no gang membership for {host_id:#x}"))
}

/// Pre-CONVERGE the known-state first island: apply H's exact reservation of
/// ISLAND_A to BOTH the claimant A and the observer O, and await exact `H` at
/// each. Because O now knows H holds ISLAND_A, A's LOSING reserve of it
/// (broadcast even when Rejected locally) is rejected at O too — so the first
/// island stays `H` at both nodes and only ISLAND_R moves under the fallback.
async fn preconverge_first_island(fix: &Fixture) {
    let h_id = fix.h.node_id();
    apply_reserve(fix.a.reservation_fold(), &fix.h, ISLAND_A, 1);
    apply_reserve(fix.o.reservation_fold(), &fix.h, ISLAND_A, 1);
    assert!(
        wait_until(RESET_TIMEOUT, || {
            holder_of(fix.a.reservation_fold(), ISLAND_A) == Some(h_id)
                && holder_of(fix.o.reservation_fold(), ISLAND_A) == Some(h_id)
        })
        .await,
        "ISLAND_A must be pre-converged to H at BOTH A and O"
    );
}

/// Assert the pre-converged first island is `H` at BOTH A and O.
fn assert_first_island_is_h(fix: &Fixture) {
    let h_id = fix.h.node_id();
    assert_eq!(
        holder_of(fix.a.reservation_fold(), ISLAND_A),
        Some(h_id),
        "A must see ISLAND_A held by H"
    );
    assert_eq!(
        holder_of(fix.o.reservation_fold(), ISLAND_A),
        Some(h_id),
        "O must see ISLAND_A held by H (A's losing reserve is rejected at O)"
    );
}

// ============================================================================
// Witnesses.
// ============================================================================

/// W1 — the opposing seed: the SELECTION policy alone puts R's island first
/// (least-loaded); the sensed join puts A's island first (A is the aggregate's
/// selected provider). A sensed-led order is therefore unambiguously the
/// sensing join, never the policy.
fn w1_opposing_order(fix: &Fixture, budget: &ConsumerLatencyBudget) {
    let crit = criteria();
    assert_eq!(
        fix.a.match_islands(&crit),
        vec![ISLAND_R, ISLAND_A],
        "baseline (LeastLoaded) puts R's island first"
    );
    let sensed = fix.a.sensed_candidates(&fix.spec, budget, None);
    assert_eq!(
        sensed.viable,
        vec![fix.a_id, fix.r_id],
        "aggregate economics"
    );
    assert_eq!(sensed.selected_provider(), Some(fix.a_id));
    assert_eq!(
        fix.a.match_islands_sensed(&crit, &fix.spec, budget, None),
        vec![ISLAND_A, ISLAND_R],
        "sensed rank leads the claim order — OPPOSITE the load order"
    );
    println!("  [PASS] W1 opposing seed: policy=[R,A] but sensed=[A,R] (sensed join, not policy)");
}

/// W2 — `selected_provider()` receives the first claim: with A sensed-selected
/// and its island FREE, `claim_island_sensed` claims A's island directly (Won,
/// no walk), rejected-apply delta zero. Then releases it (leaves ISLAND_A free
/// for the pre-hold step).
async fn w2_selected_receives_first_claim(fix: &Fixture, budget: &ConsumerLatencyBudget) {
    let before = fix.a.reservation_fold().stats().applies_rejected;
    let claimed = fix
        .a
        .claim_island_sensed(&criteria(), &fix.spec, budget, None, far_deadline())
        .await
        .expect("claim")
        .expect("an island is claimable");
    let delta = fix.a.reservation_fold().stats().applies_rejected - before;
    assert_eq!(
        claimed, ISLAND_A,
        "the claim targets the SELECTED provider (A)"
    );
    assert_eq!(
        delta, 0,
        "the selected provider's free island wins on first try"
    );
    release_and_await_free(&fix.a, &[&fix.o], ISLAND_A, RESET_TIMEOUT).await;
    println!("  [PASS] W2 selected_provider receives the first claim (A's island, no rejection)");
}

/// W6 — absence of sensed evidence never prunes: an UNSENSED host (no readiness
/// branch) is RETAINED in `match_islands_sensed` (trailing the sensed band),
/// never treated as negative evidence. Uses fold snapshot/restore so the extra
/// host does not pollute the other cells.
fn w6_absence_of_evidence_retained(fix: &Fixture, budget: &ConsumerLatencyBudget) {
    let cap_snap = fix.a.capability_fold().snapshot();
    let island_snap = fix.a.island_fold().snapshot();
    let a_kp = fix.a.entity_keypair();
    seed_gang_capability(&fix.a, a_kp, HOST_X);
    seed_island(&fix.a, a_kp, HOST_X, ISLAND_X, 0.9); // heaviest → trails by policy too
    let sensed = fix
        .a
        .match_islands_sensed(&criteria(), &fix.spec, budget, None);
    assert!(
        sensed.contains(&ISLAND_X),
        "an unsensed host's island must be RETAINED (absence of evidence never prunes): {sensed:?}"
    );
    fix.a
        .capability_fold()
        .restore(cap_snap, true)
        .expect("restore cap fold");
    fix.a
        .island_fold()
        .restore(island_snap, true)
        .expect("restore island fold");
    // Restored: the unsensed host is gone again.
    assert!(!fix
        .a
        .match_islands_sensed(&criteria(), &fix.spec, budget, None)
        .contains(&ISLAND_X));
    println!(
        "  [PASS] W6 unsensed host retained in sensed match (absence of evidence never prunes)"
    );
}

/// W4 — RESERVATION FALLBACK through the sensed path (ICB-5b core): A stays
/// sensed-selected and first, but ISLAND_A is the pre-CONVERGED known state
/// (`H` at BOTH A and O) → `claim_island_sensed` reserves ISLAND_A (Rejected),
/// walks to R's island, and commits ISLAND_R. Rejected-apply delta EXACTLY ONE;
/// the full known-state shape holds at both nodes (ISLAND_A=H, ISLAND_R=A).
async fn w4_sensed_reservation_fallback(fix: &Fixture, budget: &ConsumerLatencyBudget) {
    assert_eq!(
        fix.a
            .sensed_candidates(&fix.spec, budget, None)
            .selected_provider(),
        Some(fix.a_id),
        "A must still be the selected provider"
    );
    assert_first_island_is_h(fix); // known state at A AND O before the claim
    let mut o_rx = fix.o.reservation_fold().subscribe_changes();
    let deadline = far_deadline(); // outside the observed endpoint
    let before = fix.a.reservation_fold().stats().applies_rejected;
    let claimed = fix
        .a
        .claim_island_sensed(&criteria(), &fix.spec, budget, None, deadline)
        .await
        .expect("claim")
        .expect("the fallback island is claimable");
    // Observe R's island at O BEFORE reading stats.
    tokio::time::timeout(
        REMOTE_DEADLINE,
        await_reservation_holder(&mut o_rx, fix.o.reservation_fold(), ISLAND_R, fix.a_id),
    )
    .await
    .expect("O observes ISLAND_R held by A");
    let delta = fix.a.reservation_fold().stats().applies_rejected - before;
    assert_eq!(
        claimed, ISLAND_R,
        "the fallback walks past held A to R's island"
    );
    assert_eq!(
        delta, 1,
        "exactly one rejected apply (the attempt on held A)"
    );
    // Full known-state shape at BOTH nodes: ISLAND_A=H, ISLAND_R=A.
    assert_first_island_is_h(fix);
    assert_eq!(
        held_islands(&fix.a, fix.a_id),
        vec![ISLAND_R],
        "A holds only R's island"
    );
    assert_eq!(
        holder_of(fix.o.reservation_fold(), ISLAND_R),
        Some(fix.a_id),
        "O sees ISLAND_R held by A"
    );
    release_and_await_free(&fix.a, &[&fix.o], ISLAND_R, RESET_TIMEOUT).await;
    println!(
        "  [PASS] W4 sensed fallback: ISLAND_A=H at A&O → walk to R (rejected-delta=1; R=A at A&O)"
    );
}

/// W3 + W5 — SENSED RE-SELECTION (ICB-5a core) and the §4.9 no-suspension
/// tripwire, in one flip: A goes NotReady → A is pruned FROM THIS interest
/// (non_viable=[A]) and R becomes selected; the sensed match yields only R's
/// island, yet the PLAIN match still offers BOTH (a NotReady interest never
/// suspends the capability entry). The re-selected claim wins R's island with
/// NO rejection. Restores A Ready.
async fn w3_w5_reselection_and_no_suspension(fix: &Fixture, budget: &ConsumerLatencyBudget) {
    // Capture the exact gang capability entries BEFORE the transition (both are
    // Idle) — the §4.9 non-suspension claim is about the capability ENTRY, not
    // just matcher survival.
    let a_mem_before = gang_membership(&fix.a, fix.a_id);
    let r_mem_before = gang_membership(&fix.a, fix.r_id);
    assert_eq!(
        a_mem_before.state,
        NodeState::Idle,
        "A gang entry Idle before"
    );
    assert_eq!(
        r_mem_before.state,
        NodeState::Idle,
        "R gang entry Idle before"
    );

    fix.a_ready.store(false, Ordering::Relaxed);
    fix.a.notify_sensing_state_changed(&fix.cap);
    assert!(
        wait_until(OVERLAY_SETTLE, || {
            fix.a.sensing_projected(&fix.self_branch) == ProjectedReadiness::NotReady
        })
        .await,
        "A's self branch must flip NotReady"
    );
    await_selected(fix, budget, fix.r_id).await;

    let sensed = fix.a.sensed_candidates(&fix.spec, budget, None);
    assert_eq!(
        sensed.non_viable,
        vec![fix.a_id],
        "A pruned FOR THIS interest"
    );
    assert_eq!(sensed.selected_provider(), Some(fix.r_id));
    assert_eq!(
        fix.a
            .match_islands_sensed(&criteria(), &fix.spec, budget, None),
        vec![ISLAND_R],
        "a NotReady host is pruned from THIS match"
    );
    // §4.9 no suspension — BEHAVIORAL: the PLAIN match still offers both islands.
    assert_eq!(
        fix.a.match_islands(&criteria()),
        vec![ISLAND_R, ISLAND_A],
        "NotReady for one interest must NOT suspend the capability entry"
    );
    // §4.9 no suspension — STRUCTURAL: the capability ENTRIES are byte-identical
    // across the readiness transition (still Idle; nothing mutated or suspended).
    assert_eq!(
        gang_membership(&fix.a, fix.a_id),
        a_mem_before,
        "A's gang capability entry must be byte-identical across the NotReady flip"
    );
    assert_eq!(
        gang_membership(&fix.a, fix.r_id),
        r_mem_before,
        "R's gang capability entry must be byte-identical across the NotReady flip"
    );

    let before = fix.a.reservation_fold().stats().applies_rejected;
    let claimed = fix
        .a
        .claim_island_sensed(&criteria(), &fix.spec, budget, None, far_deadline())
        .await
        .expect("claim")
        .expect("the re-selected island is claimable");
    let delta = fix.a.reservation_fold().stats().applies_rejected - before;
    assert_eq!(claimed, ISLAND_R, "the re-selected provider (R) is claimed");
    assert_eq!(
        delta, 0,
        "re-selection is NOT a reservation conflict — no rejection"
    );
    release_and_await_free(&fix.a, &[&fix.o], ISLAND_R, RESET_TIMEOUT).await;

    // Restore A Ready (selected provider back to A).
    fix.a_ready.store(true, Ordering::Relaxed);
    fix.a.notify_sensing_state_changed(&fix.cap);
    await_selected(fix, budget, fix.a_id).await;
    println!("  [PASS] W3/W5 sensed re-selection A→R (no rejection) + NotReady never suspends");
}

// ============================================================================
// Measurement.
// ============================================================================

/// The one-line fixture/group descriptor shared by every measurement row.
fn print_fixture_metadata() {
    println!(
        "   fixture: island_pop=2 eligible_islands=2 units/island={ISLAND_UNITS} claimants=1 providers=2(A,R) logical_sessions=1(A↔R) observer_sessions=1(A↔O) topology=direct-mesh relays=0 routed=no policy=LeastLoaded mode=sensed reservation_deadline=far-future(precomputed; no takeover — ICB-6) workers={WORKER_THREADS}"
    );
}

/// The iterations/warmups line — states plainly that p99 is orientation only.
fn print_group_note(iterations: u64, warmups: u64) {
    println!(
        "   iterations={iterations} warmups_discarded={warmups} measured={} · p99 is orientation only, NOT a baseline/threshold (baselines are ICB-7)",
        iterations - warmups
    );
}

/// ICB-5b — reservation fallback through the sensed path. A stays selected;
/// ISLAND_A is the pre-CONVERGED known state (H at A AND O); each sample walks
/// to R's island. Two boundaries from one claim start: fallback API return and
/// direct observer visibility (observed BEFORE any stats read).
async fn measure_5b(fix: &Fixture, budget: &ConsumerLatencyBudget) {
    await_selected(fix, budget, fix.a_id).await; // A selected, first in sensed order
    let crit = criteria();
    // Population stability before the timed group (long fixture TTL — nothing
    // expired) + pre-converged first island at both nodes.
    assert_eq!(
        fix.a.match_islands(&crit),
        vec![ISLAND_R, ISLAND_A],
        "island population must be intact before the 5b group"
    );
    assert_first_island_is_h(fix);
    let deadline = far_deadline(); // fixture/policy setup — outside timing
    let mut api = LatencyReport::new();
    let mut remote = LatencyReport::new();

    for s in 0..SAMPLES {
        // Known-state precondition at BOTH nodes.
        assert_first_island_is_h(fix);
        assert_eq!(
            holder_of(fix.a.reservation_fold(), ISLAND_R),
            None,
            "A: ISLAND_R free before claim"
        );
        assert_eq!(
            holder_of(fix.o.reservation_fold(), ISLAND_R),
            None,
            "O: ISLAND_R free before claim"
        );
        let mut o_rx = fix.o.reservation_fold().subscribe_changes();
        let before = fix.a.reservation_fold().stats().applies_rejected;

        let t0 = Instant::now();
        let claimed = fix
            .a
            .claim_island_sensed(&crit, &fix.spec, budget, None, deadline)
            .await
            .expect("claim")
            .expect("fallback island");
        let api_dt = t0.elapsed();
        // Observe R's island at O — the endpoint — BEFORE any stats read.
        tokio::time::timeout(
            REMOTE_DEADLINE,
            await_reservation_holder(&mut o_rx, fix.o.reservation_fold(), ISLAND_R, fix.a_id),
        )
        .await
        .expect("O observes ISLAND_R held by A");
        let remote_dt = t0.elapsed();
        let delta = fix.a.reservation_fold().stats().applies_rejected - before;

        assert_eq!(claimed, ISLAND_R, "fallback claims R's island");
        assert_eq!(delta, 1, "exactly one rejected apply per fallback");
        // Full known-state shape at BOTH nodes: ISLAND_A=H, ISLAND_R=A.
        assert_first_island_is_h(fix);
        assert_eq!(
            held_islands(&fix.a, fix.a_id),
            vec![ISLAND_R],
            "A holds only R's island"
        );
        assert_eq!(
            held_islands(&fix.a, fix.h.node_id()),
            vec![ISLAND_A],
            "H holds only ISLAND_A at A"
        );
        assert_eq!(
            holder_of(fix.o.reservation_fold(), ISLAND_R),
            Some(fix.a_id),
            "O sees ISLAND_R held by A"
        );

        if s >= WARMUP {
            api.record(api_dt.as_nanos() as u64);
            remote.record(remote_dt.as_nanos() as u64);
        }
        release_and_await_free(&fix.a, &[&fix.o], ISLAND_R, RESET_TIMEOUT).await;
    }

    api.print_row("ICB-5b · sensed fallback claim API return (walk A→Lost→R) · mechanism");
    remote
        .print_row("ICB-5b · direct remote visibility (O reads R's island held by A) · mechanism");
    println!(
        "   label=\"sensed reservation fallback, single claimant\" · selected_provider=A · first_island=ISLAND_A(pre-converged H at A&O) · final_island=R · rejected_delta=1/sample · distributed-race framing DISCLAIMED"
    );
    print_fixture_metadata();
    print_group_note(SAMPLES, WARMUP);
    println!();
}

/// ICB-5a — sensed re-selection. With A flipped NotReady, R is the selected
/// provider; each on-demand claim wins R's island directly (no rejection). The
/// re-selection (A→R) is the changed SELECTION, not a reservation conflict.
/// Restores A Ready AND awaits settlement before returning (so the overhead
/// group that follows measures the steady sensed state, not a transition).
async fn measure_5a(fix: &Fixture, budget: &ConsumerLatencyBudget) {
    fix.a_ready.store(false, Ordering::Relaxed);
    fix.a.notify_sensing_state_changed(&fix.cap);
    await_selected(fix, budget, fix.r_id).await;
    let crit = criteria();
    // Precondition: sensed match is R only (A pruned); plain still offers both.
    assert_eq!(
        fix.a.match_islands_sensed(&crit, &fix.spec, budget, None),
        vec![ISLAND_R],
        "5a precondition: sensed match is R only"
    );
    assert_eq!(
        fix.a.match_islands(&crit),
        vec![ISLAND_R, ISLAND_A],
        "5a precondition: plain match still offers both"
    );
    let deadline = far_deadline();
    let mut claim = LatencyReport::new();

    for s in 0..SAMPLES {
        assert_eq!(holder_of(fix.a.reservation_fold(), ISLAND_R), None);
        let before = fix.a.reservation_fold().stats().applies_rejected;
        let t0 = Instant::now();
        let claimed = fix
            .a
            .claim_island_sensed(&crit, &fix.spec, budget, None, deadline)
            .await
            .expect("claim")
            .expect("re-selected island");
        let dt = t0.elapsed();
        let delta = fix.a.reservation_fold().stats().applies_rejected - before;
        assert_eq!(claimed, ISLAND_R, "the re-selected provider (R) is claimed");
        assert_eq!(
            delta, 0,
            "re-selection is not a reservation conflict — no rejection"
        );
        if s >= WARMUP {
            claim.record(dt.as_nanos() as u64);
        }
        // Reset — await exact Free at BOTH the claimant and the observer.
        release_and_await_free(&fix.a, &[&fix.o], ISLAND_R, RESET_TIMEOUT).await;
    }

    // Restore A Ready and SETTLE (fail-loud) before returning.
    fix.a_ready.store(true, Ordering::Relaxed);
    fix.a.notify_sensing_state_changed(&fix.cap);
    await_selected(fix, budget, fix.a_id).await;

    claim.print_row("ICB-5a · re-selected-provider claim (sensed, on-demand) · mechanism");
    println!(
        "   label=\"sensed re-selection, single claimant\" · selection_change=A→R (overlay: A NotReady) · claimed=R · rejected_delta=0/sample (no spurious rejection) · on-demand (bench-invoked after the overlay change)"
    );
    print_fixture_metadata();
    print_group_note(SAMPLES, WARMUP);
    println!();
}

/// Sensed-projection overhead: the cost the sensed join ADDS over the plain
/// matcher (read-only; no claim). Precondition is pinned once BEFORE timing
/// (measure_5a settled A back to selected) so the loop measures the STEADY
/// sensed state, never a NotReady/transition mixture. Two mechanism rows, not a
/// threshold. Does NOT restate capability-propagation latency (CPB owns it).
fn measure_projection_overhead(fix: &Fixture, budget: &ConsumerLatencyBudget) {
    let crit = criteria();
    // Steady-state precondition (settled by measure_5a).
    assert_eq!(
        fix.a
            .sensed_candidates(&fix.spec, budget, None)
            .selected_provider(),
        Some(fix.a_id),
        "overhead precondition: A selected"
    );
    assert_eq!(
        fix.a.match_islands(&crit),
        vec![ISLAND_R, ISLAND_A],
        "overhead precondition: plain order == [R, A]"
    );
    assert_eq!(
        fix.a.match_islands_sensed(&crit, &fix.spec, budget, None),
        vec![ISLAND_A, ISLAND_R],
        "overhead precondition: sensed order == [A, R]"
    );

    let mut plain = LatencyReport::new();
    let mut sensed = LatencyReport::new();
    for i in 0..500u64 {
        let t0 = Instant::now();
        let p = fix.a.match_islands(&crit);
        let pd = t0.elapsed();
        std::hint::black_box(&p);
        let t1 = Instant::now();
        let s = fix.a.match_islands_sensed(&crit, &fix.spec, budget, None);
        let sd = t1.elapsed();
        std::hint::black_box(&s);
        if i >= 50 {
            plain.record(pd.as_nanos() as u64);
            sensed.record(sd.as_nanos() as u64);
        }
    }
    plain.print_row("ICB-5 · match_islands (plain matcher) · mechanism");
    sensed
        .print_row("ICB-5 · match_islands_sensed (sensed join = plain + Projection 6) · mechanism");
    println!(
        "   sensed-projection overhead = sensed − plain (the readiness join + re-rank cost) · precondition: selected=A, plain=[R,A], sensed=[A,R]"
    );
    print_fixture_metadata();
    print_group_note(500, 50);
    println!();
}
