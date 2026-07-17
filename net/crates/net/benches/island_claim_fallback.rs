//! ICB-4 — known-state fallback (SINGLE CLAIMANT).
//!
//! Measures the ordinary claim loop walking PAST one pre-converged, already-
//! held island to the next free candidate. This is `MeshNode::claim_island`
//! (`mesh.rs`): iterate `match_islands` in ranked order, `reserve_island` each,
//! return the first that Wins. When the top-ranked island is held by someone
//! else in this node's local view, its optimistic CAS is `Rejected` (`Lost`)
//! and the loop walks to the next.
//!
//! # SINGLE CLAIMANT — the distributed-race framing is DISCLAIMED
//!
//! There is exactly ONE claimant `C`. A *multi*-scheduler walk to the free
//! island `B` would just recreate the ICB-3 divergence **on B** (every
//! scheduler correctly rejects held `A`, then concurrently inserts its own
//! `Reserved{self}` on the empty `B` — no tie-break, no convergence). So there
//! is NO fleet allocation spread, NO "every scheduler ends non-conflicting",
//! and NO multi-scheduler fallback throughput here (Kyra v0.2→v0.3 blocker B1).
//! The pre-existing holder `H` is a fixed known state, not a competitor.
//!
//! # Fixture (from one claim start)
//!
//!   - `C` — the single claimant (a real started node).
//!   - `H` — a pre-existing holder IDENTITY; `A` is pre-converged
//!     `Reserved{H}` in `C`'s local reservation view (a directly-applied
//!     signed envelope — the same bytes that would arrive on the wire).
//!   - `O` — a direct observer node (`C ↔ O`), reading its OWN local exact
//!     holder (M7-2: an observer, never host authority).
//!   - `A` (held by `H`) is ranked immediately before FREE `B`
//!     (`SelectionPolicy::LeastLoaded`; `load(A) < load(B)`).
//!
//! Timed: `C.claim_island(...)` → `C`'s reserve of `A` returns `Lost` → the
//! loop walks to `B` → `B` commits locally and the fan-out is attempted (E1:
//! `reserve_island` awaits the fan-out before returning) → `O` reads `B` held
//! by `C`. Two boundaries from one start: fallback claim API return, and
//! direct remote visibility.
//!
//! Nine per-sample assertions (all fail-loud): A ranks before B · A held by H
//! in C's exact local view before timing · B free before timing · the
//! reservation-fold rejected-apply delta increases by EXACTLY ONE for C's
//! attempt on A · the returned island is B · C's local exact holder of B is C ·
//! O's exact holder of B is C · A remains held by H · no other island changed
//! on C (H holds exactly {A}, C holds exactly {B}).
//!
//! Honesty note (W4): a LOSING reserve still gossips — `C`'s rejected
//! `Reserved{C}` on `A` is broadcast to `O` regardless of the local CAS
//! outcome, so `O` ends up observing `A` held by `C` while `C` keeps `A` held
//! by `H`. That is an orthogonal ICB-3-style divergence, NOT part of the
//! fallback outcome (which concerns `B`), and it is exactly why the framing is
//! single-claimant. It is witnessed, not hidden.
//!
//! No arbitration, tie-break, merge change, quorum, or sensed readiness. No
//! threshold or public claim (ICB-7).
//!
//! Run: `cargo bench -p net-mesh --bench island_claim_fallback --features net`

#[path = "bench_island_claim/mod.rs"]
mod bench_island_claim;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bench_island_claim::{
    apply_reserve, await_reservation_holder, far_deadline, holder_of, node, pair,
    release_and_await_free, runtime, LatencyReport, WORKER_THREADS,
};
use net::adapter::net::behavior::fold::{
    CapabilityFilter as FoldCapFilter, CapabilityFold, CapabilityMembership, CapabilityQuery,
    EnvelopeMeta, FoldKind, IslandRecord, IslandTopologyFold, NodeState, ReservationQuery,
    SignedAnnouncement, UnitSet,
};
use net::adapter::net::behavior::gang::{
    ClaimOutcome, MatchCriteria, NumericFilter, SelectionPolicy,
};
use net::adapter::net::{EntityKeypair, MeshNode};

/// Capability tag that makes the fixture host a matcher candidate.
const MATCH_TAG: &str = "gpu:h100";
/// Fixed capability class for the seeded host.
const CLASS: u64 = 0;
/// Eligibility floor (eligibility only — a successful claim reserves the
/// whole island; the matcher reserves nothing).
const MIN_UNITS: usize = 4;
/// Units seeded per island (≥ MIN_UNITS so both A and B are eligible).
const ISLAND_UNITS: u32 = 8;
/// Fictional host of A and B in C's folds (matcher fixture; a pure fold read —
/// the host need not be a live node). Far from any real random node id.
const HOST: u64 = 0x4C00_0001;
/// The pre-converged, already-held island.
const ISLAND_A: u64 = 0x4C00_00A0;
/// The free fallback island.
const ISLAND_B: u64 = 0x4C00_00B0;
/// Loads: A ranks immediately before B under LeastLoaded (load(A) < load(B)).
const LOAD_A: f32 = 0.1;
const LOAD_B: f32 = 0.6;
/// Long fixture TTL so the fold sweeper never removes a seeded topology entry.
const FIXTURE_TTL_SECS: u32 = 3_600;
/// Ceiling for the direct remote-visibility endpoint (localhost is fast).
const REMOTE_DEADLINE: Duration = Duration::from_secs(2);
/// Fail-loud reset ceiling (release B, await exact Free on C and O).
const RESET_TIMEOUT: Duration = Duration::from_secs(2);

const SAMPLES: u64 = 40;
const WARMUP: u64 = 5;

fn main() {
    let rt = runtime();
    rt.block_on(async {
        println!("\n=== ICB-4 known-state fallback (single claimant) ===\n");

        println!("-- witnesses --");
        w1_held_island_still_matches().await;
        w2_reserve_decisions_and_rejected_counter().await;
        w3_claim_island_walks_to_b().await;
        w4_losing_reserve_gossips_observer_diverges().await;

        println!("\n-- measurement --");
        measure().await;
    });
}

// ============================================================================
// Fixture seeding (in-process fold applies; excluded from timing).
// ============================================================================

fn fixture_meta() -> EnvelopeMeta {
    EnvelopeMeta {
        announced_at: 0,
        ttl_secs: Some(FIXTURE_TTL_SECS),
        flags: 0,
    }
}

/// Seed the matcher host (`node_id = HOST`) carrying [`MATCH_TAG`], Idle.
fn seed_host(node: &Arc<MeshNode>, signer: &EntityKeypair) {
    let ann = SignedAnnouncement::sign(
        signer,
        CapabilityFold::KIND_ID,
        CLASS,
        HOST,
        1,
        fixture_meta(),
        CapabilityMembership {
            class_hash: CLASS,
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
        },
    )
    .expect("sign host capability");
    node.capability_fold()
        .apply(ann)
        .expect("apply host capability");
}

/// Seed one island of [`ISLAND_UNITS`] units hosted by [`HOST`] with `load`.
fn seed_island(node: &Arc<MeshNode>, signer: &EntityKeypair, island: u64, load: f32) {
    let record = IslandRecord {
        id: island,
        units: UnitSet::new((0..ISLAND_UNITS).collect()),
        host: HOST,
        capabilities: Vec::new(),
        load,
        p50_latency_us: 1_500,
    };
    let ann = SignedAnnouncement::sign(
        signer,
        IslandTopologyFold::KIND_ID,
        CLASS,
        HOST,
        1,
        fixture_meta(),
        record,
    )
    .expect("sign island");
    node.island_fold().apply(ann).expect("apply island");
}

/// Seed C's matcher fixture: host + A(load 0.1) + B(load 0.6).
fn seed_topology(node: &Arc<MeshNode>, signer: &EntityKeypair) {
    seed_host(node, signer);
    seed_island(node, signer, ISLAND_A, LOAD_A);
    seed_island(node, signer, ISLAND_B, LOAD_B);
}

fn criteria() -> MatchCriteria {
    MatchCriteria {
        capability: CapabilityQuery::Composite(FoldCapFilter {
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

/// The islands `node_id` holds in `fold` (Reserved or Active), ascending.
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

// ============================================================================
// Measurement — one fixed claimant/observer, fixed A/B, clean reset per sample.
// ============================================================================

struct Fixture {
    c: Arc<MeshNode>,
    o: Arc<MeshNode>,
    h: EntityKeypair,
}

/// Build the single-claimant fallback fixture: C↔O started + warmed; C's
/// matcher folds seeded with host + A + B; A pre-converged `Reserved{H}` in
/// C's local view.
async fn build_fixture() -> Fixture {
    let (c, o) = pair().await;
    let signer = EntityKeypair::generate(); // fixture announcement signer
    let h = EntityKeypair::generate(); // pre-existing holder identity
    seed_topology(&c, &signer);
    // Pre-converge A held by H in C's local reservation view.
    apply_reserve(c.reservation_fold(), &h, ISLAND_A, 1);
    Fixture { c, o, h }
}

async fn measure() {
    let fix = build_fixture().await;
    let crit = criteria();
    let c_id = fix.c.node_id();
    let h_id = fix.h.node_id();
    // Precompute the far-future takeover deadline ONCE, outside timing (no
    // takeover fires inside a fallback sample — ICB-6 owns deadlines).
    let deadline = far_deadline();

    let mut api = LatencyReport::new();
    let mut remote = LatencyReport::new();

    for s in 0..SAMPLES {
        // --- Pre-timing assertions (fail-loud). ---
        let order = fix.c.match_islands(&crit);
        let a_pos = order.iter().position(|&i| i == ISLAND_A);
        let b_pos = order.iter().position(|&i| i == ISLAND_B);
        assert!(
            a_pos.is_some() && b_pos.is_some() && a_pos < b_pos,
            "[1] A must rank before B in the match order (got {order:?})"
        );
        assert_eq!(
            holder_of(fix.c.reservation_fold(), ISLAND_A),
            Some(h_id),
            "[2] A must be held by H in C's exact local view before timing"
        );
        assert_eq!(
            holder_of(fix.c.reservation_fold(), ISLAND_B),
            None,
            "[3] B must be free in C's view before timing"
        );

        // Subscribe O's fold BEFORE t0 (missed-wakeup-safe remote endpoint).
        let mut o_rx = fix.o.reservation_fold().subscribe_changes();
        let rejected_before = fix.c.reservation_fold().stats().applies_rejected;

        // --- TIMED: fallback claim (walk A→Lost→B), then remote visibility. ---
        let t0 = Instant::now();
        let claimed = fix
            .c
            .claim_island(&crit, deadline)
            .await
            .expect("claim_island");
        let api_dt = t0.elapsed();
        // Tight rejected-apply boundary: the reject happened entirely inside
        // claim_island (reserve A rejected, reserve B inserted).
        let rejected_delta = fix.c.reservation_fold().stats().applies_rejected - rejected_before;
        // Remote visibility endpoint (inside the timed region): O reads B held
        // by C. The predicate is checked first, so an already-visible holder
        // returns immediately.
        tokio::time::timeout(
            REMOTE_DEADLINE,
            await_reservation_holder(&mut o_rx, fix.o.reservation_fold(), ISLAND_B, c_id),
        )
        .await
        .expect("O must observe B held by C within the remote deadline");
        let remote_dt = t0.elapsed();

        // --- Post assertions (fail-loud). ---
        assert_eq!(claimed, Some(ISLAND_B), "[5] the returned island must be B");
        assert_eq!(
            rejected_delta, 1,
            "[4] the reservation-fold rejected-apply delta must increase by exactly one (C's attempt on A)"
        );
        assert_eq!(
            holder_of(fix.c.reservation_fold(), ISLAND_B),
            Some(c_id),
            "[6] C's local exact holder of B must be C"
        );
        assert_eq!(
            holder_of(fix.o.reservation_fold(), ISLAND_B),
            Some(c_id),
            "[7] O's exact holder of B must be C"
        );
        assert_eq!(
            holder_of(fix.c.reservation_fold(), ISLAND_A),
            Some(h_id),
            "[8] A must remain held by H"
        );
        // [9] No other island changed on C: H holds exactly {A}, C holds
        // exactly {B}.
        assert_eq!(
            held_islands(&fix.c, h_id),
            vec![ISLAND_A],
            "[9] H must hold exactly {{A}} on C"
        );
        assert_eq!(
            held_islands(&fix.c, c_id),
            vec![ISLAND_B],
            "[9] C must hold exactly {{B}} on C"
        );

        if s >= WARMUP {
            api.record(api_dt.as_nanos() as u64);
            remote.record(remote_dt.as_nanos() as u64);
        }

        // --- Clean reset (fail-loud): release B, await exact Free on C and O.
        // A stays Reserved{H} (never released). B cycles free→held→free. ---
        release_and_await_free(&fix.c, &[&fix.o], ISLAND_B, RESET_TIMEOUT).await;
    }

    api.print_row("ICB-4 · fallback claim API return (walk A→Lost→B) · mechanism");
    remote.print_row("ICB-4 · direct remote visibility (O reads B held by C) · mechanism");
    println!(
        "   label=\"fallback from one pre-converged reservation, single claimant\" · claimant=1 (no coordinator; distributed-race framing DISCLAIMED — a multi-scheduler walk to B recreates ICB-3 on B)"
    );
    println!(
        "   fixture=fixed A(held-by-H)/B(free) · reset=release-B + await-exact-Free (A stays Reserved{{H}}) · deadline=far-future (precomputed; no takeover — ICB-6) · workers={WORKER_THREADS}"
    );
    println!();
}

// ============================================================================
// Witnesses.
// ============================================================================

/// W1 — a held island is STILL offered by the matcher: `match_islands` reads
/// only the capability + island-topology folds, never the reservation fold, so
/// `A` (held by `H`) still ranks before free `B`. This is what makes the
/// reject-walk possible at all. Exercises the real `MeshNode::match_islands`.
async fn w1_held_island_still_matches() {
    let n = node().await; // unstarted: match_islands is a pure fold read
    let signer = EntityKeypair::generate();
    let h = EntityKeypair::generate();
    seed_topology(&n, &signer);
    apply_reserve(n.reservation_fold(), &h, ISLAND_A, 1);
    let order = n.match_islands(&criteria());
    assert_eq!(
        order.iter().position(|&i| i == ISLAND_A),
        Some(0),
        "held A must still be offered first (matcher ignores reservation state): {order:?}"
    );
    assert!(
        order.iter().position(|&i| i == ISLAND_B) > Some(0),
        "free B must rank after held A: {order:?}"
    );
    println!("  [PASS] W1 held island still matches (matcher ignores reservation state)");
}

/// W2 — the CAS decisions that drive the walk, on the real `reserve_island`:
/// reserving held `A` returns `Lost` AND bumps the reservation-fold
/// rejected-apply counter by exactly one; reserving free `B` returns `Won` and
/// bumps it by zero. Pins the `Lost`/`Won` verdicts and the counter the
/// measurement's assertion [4] reads.
async fn w2_reserve_decisions_and_rejected_counter() {
    let (c, _o) = pair().await;
    let h = EntityKeypair::generate();
    apply_reserve(c.reservation_fold(), &h, ISLAND_A, 1);
    let deadline = far_deadline();

    let before_a = c.reservation_fold().stats().applies_rejected;
    let out_a = c
        .reserve_island(ISLAND_A, deadline)
        .await
        .expect("reserve A");
    let after_a = c.reservation_fold().stats().applies_rejected;
    assert_eq!(out_a, ClaimOutcome::Lost, "reserve of held A must be Lost");
    assert_eq!(
        after_a - before_a,
        1,
        "a rejected reserve must bump applies_rejected by exactly one"
    );

    let before_b = c.reservation_fold().stats().applies_rejected;
    let out_b = c
        .reserve_island(ISLAND_B, deadline)
        .await
        .expect("reserve B");
    let after_b = c.reservation_fold().stats().applies_rejected;
    assert_eq!(out_b, ClaimOutcome::Won, "reserve of free B must be Won");
    assert_eq!(
        after_b - before_b,
        0,
        "a winning reserve must not bump applies_rejected"
    );
    println!("  [PASS] W2 reserve(held A)=Lost (+1 rejected), reserve(free B)=Won (+0 rejected)");
}

/// W3 — end-to-end walk on the real `claim_island`: with A held by H ranked
/// before free B, the loop rejects A and returns B, with the rejected-apply
/// delta exactly one. Pins the whole fallback composition on the production API.
async fn w3_claim_island_walks_to_b() {
    let fix = build_fixture().await;
    let before = fix.c.reservation_fold().stats().applies_rejected;
    let claimed = fix
        .c
        .claim_island(&criteria(), far_deadline())
        .await
        .expect("claim_island");
    let delta = fix.c.reservation_fold().stats().applies_rejected - before;
    assert_eq!(
        claimed,
        Some(ISLAND_B),
        "claim_island must walk past A to B"
    );
    assert_eq!(
        delta, 1,
        "exactly one rejected apply (the attempt on held A)"
    );
    assert_eq!(
        holder_of(fix.c.reservation_fold(), ISLAND_A),
        Some(fix.h.node_id()),
        "A must remain held by H after the walk"
    );
    println!("  [PASS] W3 claim_island walks A→Lost→B (returns B, rejected-delta=1, A stays H)");
}

/// W4 — HONESTY: a losing reserve still gossips. C's rejected `Reserved{C}` on
/// A is broadcast to O regardless of the local CAS, so O ends up observing A
/// held by C while C keeps A held by H — an orthogonal ICB-3-style divergence,
/// NOT part of the fallback outcome (B), and precisely why this is
/// single-claimant. Each node reads its OWN local view (M7-2), not authority.
async fn w4_losing_reserve_gossips_observer_diverges() {
    let fix = build_fixture().await;
    let c_id = fix.c.node_id();
    let h_id = fix.h.node_id();
    let mut o_rx = fix.o.reservation_fold().subscribe_changes();
    fix.c
        .claim_island(&criteria(), far_deadline())
        .await
        .expect("claim_island");
    // O eventually observes A held by C (the losing broadcast landed).
    tokio::time::timeout(
        REMOTE_DEADLINE,
        await_reservation_holder(&mut o_rx, fix.o.reservation_fold(), ISLAND_A, c_id),
    )
    .await
    .expect("O must observe the losing A broadcast (A held by C)");
    assert_eq!(
        holder_of(fix.o.reservation_fold(), ISLAND_A),
        Some(c_id),
        "O observes A held by C (losing reserve gossiped)"
    );
    assert_eq!(
        holder_of(fix.c.reservation_fold(), ISLAND_A),
        Some(h_id),
        "C keeps A held by H — the fallback outcome (B) is unaffected; the A views diverge"
    );
    println!(
        "  [PASS] W4 losing reserve gossips → O diverges on A (C:A=H, O:A=C); single-claimant disclaimer"
    );
}
