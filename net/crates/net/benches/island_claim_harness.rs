//! ICB-0 harness self-test — the witnesses Kyra requires before ICB-1.
//! This is NOT a measurement bench: it exercises the ICB-local harness
//! (`bench_island_claim/mod.rs`) and asserts the load-bearing counting /
//! delivery / exact-read rules. Run:
//!
//! `cargo bench -p net-mesh --bench island_claim_harness --features net`
//!
//! Any failed witness panics → the bench exits non-zero. No headline,
//! no threshold, no production/arbitration change (ICB-0 scope).

#[path = "bench_island_claim/mod.rs"]
mod bench_island_claim;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use bench_island_claim::*;
use net::adapter::net::behavior::fold::{ApplyOutcome, Fold, FoldChannelRouter, ReservationFold};
use net::adapter::net::behavior::gang::ClaimOutcome;
use net::adapter::net::{EntityKeypair, MeshNode};

fn main() {
    let rt = runtime();
    rt.block_on(async {
        println!("\n=== ICB-0 harness witnesses ===\n");

        // Counting-rule unit witnesses (drive try_route directly).
        w_inserted_counts();
        w_rejected_still_counts();
        w_malformed_not_counted();
        w_duplicate_not_incremented();
        w_wrong_island_not_incremented();
        w_exact_barrier_rejects_overshoot().await;

        // Exact-holder discipline (local fold, deterministic).
        w_exact_holder_wake().await;

        // Transport witnesses (real distinct nodes on localhost).
        w_direct_preflight_succeeds().await;
        w_single_claim_local_smoke().await;
        w_direct_observer_smoke().await;
        w_routed_chain_not_reported().await;
        w_claimant_n_minus_1_observer_n().await;

        // Fixture-reset safety (fail-loud).
        w_reset_succeeds().await;
        w_reset_failure_fails_loud().await;

        println!("\nICB-0 harness witnesses: ALL PASS\n");
    });
}

// ============================================================================
// Counting-rule unit witnesses.
// ============================================================================

fn w_inserted_counts() {
    let island = 0x1Au64;
    let (cr, _fold) = unit_router(island);
    let kp = EntityKeypair::generate();
    let out = cr.try_route(kp.entity_id(), &reserve_bytes(&kp, island, 1));
    assert!(
        matches!(out, Ok(ApplyOutcome::Inserted)),
        "want Inserted, got {out:?}"
    );
    assert_eq!(cr.count(), 1, "a valid Inserted delivery must count");
    println!("  [PASS] valid Inserted delivery counts (1)");
}

fn w_rejected_still_counts() {
    let island = 0x2Au64;
    let (cr, _fold) = unit_router(island);
    let a = EntityKeypair::generate();
    let b = EntityKeypair::generate();
    // A claims first (Inserted); B's fresh foreign claim over the held,
    // unexpired island is merge-rejected — but it was still delivered +
    // verified, so it counts.
    assert!(matches!(
        cr.try_route(a.entity_id(), &reserve_bytes(&a, island, 1)),
        Ok(ApplyOutcome::Inserted)
    ));
    let out_b = cr.try_route(b.entity_id(), &reserve_bytes(&b, island, 1));
    assert!(
        matches!(out_b, Ok(ApplyOutcome::Rejected)),
        "want Rejected, got {out_b:?}"
    );
    assert_eq!(
        cr.count(),
        2,
        "a Rejected but delivered+verified claim still counts"
    );
    println!("  [PASS] Rejected (but delivered) claim still counts (2)");
}

fn w_malformed_not_counted() {
    let island = 0x3Au64;
    let (cr, _fold) = unit_router(island);
    // Garbage bytes: no valid envelope → dispatch errors.
    let out = cr.try_route(
        EntityKeypair::generate().entity_id(),
        &[0xFFu8, 0x00, 0x13, 0x37],
    );
    assert!(out.is_err(), "garbage must not dispatch, got {out:?}");
    // Valid envelope, WRONG publisher: signature/node-id verify fails.
    let a = EntityKeypair::generate();
    let b = EntityKeypair::generate();
    let bytes = reserve_bytes(&a, island, 1);
    let out2 = cr.try_route(b.entity_id(), &bytes);
    assert!(
        out2.is_err(),
        "mismatched publisher must not dispatch, got {out2:?}"
    );
    assert_eq!(cr.count(), 0, "failed dispatch must never count");
    println!("  [PASS] malformed / failed dispatch does not count (0)");
}

fn w_duplicate_not_incremented() {
    let island = 0x4Au64;
    let (cr, _fold) = unit_router(island);
    let a = EntityKeypair::generate();
    let bytes = reserve_bytes(&a, island, 1);
    assert!(matches!(
        cr.try_route(a.entity_id(), &bytes),
        Ok(ApplyOutcome::Inserted)
    ));
    // Identical frame again: same (publisher, island, generation) tuple.
    // It still dispatches (Ok(Rejected) — same-publisher, non-advancing
    // generation), but must not re-count.
    let out2 = cr.try_route(a.entity_id(), &bytes);
    assert!(out2.is_ok(), "re-delivery still dispatches, got {out2:?}");
    assert_eq!(
        cr.count(),
        1,
        "duplicate (publisher,island,generation) must not increment"
    );
    println!("  [PASS] duplicate tuple does not increment (1)");
}

fn w_wrong_island_not_incremented() {
    let tracked = 0x5Au64;
    let (cr, _fold) = unit_router(tracked);
    let a = EntityKeypair::generate();
    // A valid, verified delivery — but for a DIFFERENT island than the
    // one this router tracks.
    let out = cr.try_route(a.entity_id(), &reserve_bytes(&a, 0x5Bu64, 1));
    assert!(
        matches!(out, Ok(ApplyOutcome::Inserted)),
        "off-island frame still dispatches, got {out:?}"
    );
    assert_eq!(
        cr.count(),
        0,
        "delivery for a non-tracked island must not count"
    );
    println!("  [PASS] wrong-island delivery does not increment (0)");
}

// ============================================================================
// Exact-holder discipline.
// ============================================================================

async fn w_exact_holder_wake() {
    let fold = Arc::new(Fold::<ReservationFold>::new());
    let unrelated = EntityKeypair::generate();
    let target = EntityKeypair::generate();
    let target_island = 0xE1u64;
    let other_island = 0xE2u64;

    let mut rx = fold.subscribe_changes();
    // An UNRELATED change fires the fold watch...
    apply_reserve(&fold, &unrelated, other_island, 1);
    // ...but awaiting the TARGET holder must consume that wake WITHOUT
    // returning (the exact read still fails), so a bounded await times out.
    let early = tokio::time::timeout(
        Duration::from_millis(200),
        await_reservation_holder(&mut rx, &fold, target_island, target.node_id()),
    )
    .await;
    assert!(
        early.is_err(),
        "an unrelated wake must not stop the exact-holder await"
    );

    // Once the exact holder is actually present, the await returns.
    apply_reserve(&fold, &target, target_island, 1);
    tokio::time::timeout(
        Duration::from_secs(2),
        await_reservation_holder(&mut rx, &fold, target_island, target.node_id()),
    )
    .await
    .expect("await must return once the exact holder is present");
    println!("  [PASS] exact-holder wake stops only after the exact read");
}

// ============================================================================
// Transport witnesses.
// ============================================================================

async fn w_direct_preflight_succeeds() {
    let (a, b) = pair().await;
    let cb = install_counter(&b, 0);
    assert!(
        reservation_delivers(&a, &cb, 0xD1u64).await,
        "direct reservation A->B must be delivered (preflight)"
    );
    println!("  [PASS] direct reservation-delivery preflight succeeds");
}

async fn w_single_claim_local_smoke() {
    let (a, _b) = pair().await;
    let island = 0xA0u64;
    let mut rx = a.reservation_fold().subscribe_changes();
    let out = a
        .reserve_island(island, far_deadline())
        .await
        .expect("reserve");
    assert!(
        matches!(out, ClaimOutcome::Won),
        "uncontended claim must win, got {out:?}"
    );
    tokio::time::timeout(
        Duration::from_secs(2),
        await_reservation_holder(&mut rx, a.reservation_fold(), island, a.node_id()),
    )
    .await
    .expect("local exact-holder must become visible");
    println!("  [PASS] single-claim local exact-holder smoke");
}

async fn w_direct_observer_smoke() {
    let (a, b) = pair().await;
    let island = 0xB0u64;
    let cb = install_counter(&b, island);
    let mut rxb = b.reservation_fold().subscribe_changes();
    a.reserve_island(island, far_deadline())
        .await
        .expect("reserve");
    tokio::time::timeout(
        Duration::from_secs(3),
        await_reservation_holder(&mut rxb, b.reservation_fold(), island, a.node_id()),
    )
    .await
    .expect("observer must see A as holder");
    assert!(
        wait_count(&cb, 1, Duration::from_secs(2)).await,
        "observer must count the delivery"
    );
    println!("  [PASS] direct observer exact-holder smoke (+counted)");
}

async fn w_routed_chain_not_reported() {
    // Raw A↔R↔B chain, NO direct A–B edge.
    let a = node().await;
    let r = node().await;
    let b = node().await;
    connect(&a, &r).await;
    connect(&r, &b).await;
    a.start_arc();
    r.start_arc();
    b.start_arc();
    // Capabilities DO relay through R, so B learns A's caps...
    warm_pair(&a, &b).await;
    let cb = install_counter(&b, 0);
    // ...but reservations do NOT relay (direct-peer broadcast only), so
    // A's reservation never reaches B: the routed row must be refused.
    assert!(
        !reservation_delivers(&a, &cb, 0xF1u64).await,
        "raw A<->R<->B chain must NOT deliver A's reservation to B"
    );
    println!("  [PASS] raw A<->R<->B chain refused as routed reservation delivery");
}

async fn w_claimant_n_minus_1_observer_n() {
    let island = 0xC1A1u64;
    // Full logical mesh of 4: node[0] observes, node[1..4] are the 3
    // claimants (N = 3). Everyone is a direct peer of everyone.
    let nodes = full_mesh(4).await;
    let observer = nodes[0].clone();
    let claimants: Vec<Arc<MeshNode>> = nodes[1..].to_vec();

    // Counters installed AFTER warm.
    let cobs = install_counter(&observer, island);
    let ccs: Vec<Arc<CountingRouter>> = claimants
        .iter()
        .map(|c| install_counter(c, island))
        .collect();

    // Fire concurrent fresh claims on the shared island.
    let deadline = far_deadline();
    let mut handles = Vec::new();
    for c in &claimants {
        let c = c.clone();
        handles.push(tokio::spawn(async move {
            c.reserve_island(island, deadline).await
        }));
    }
    // Every concurrent fresh LOCAL claim must optimistically Win — the
    // premise ICB-3 later reports (item 7).
    for h in handles {
        let outcome = h.await.expect("claim task join").expect("claim call");
        assert_eq!(
            outcome,
            ClaimOutcome::Won,
            "each concurrent fresh local claim must optimistically Win"
        );
    }

    // Each claimant receives the OTHER two claimants' reservations (N-1);
    // its own local apply never traverses its own inbound router. The
    // barrier is EXACT — an unexpected extra unique delivery fails here.
    for (i, cc) in ccs.iter().enumerate() {
        assert!(
            wait_count(cc, 2, Duration::from_secs(5)).await,
            "claimant {i}: expected EXACTLY N-1=2 foreign deliveries, got {}",
            cc.count()
        );
    }
    // The non-claiming observer receives all N = 3.
    assert!(
        wait_count(&cobs, 3, Duration::from_secs(5)).await,
        "observer: expected EXACTLY N=3 deliveries, got {}",
        cobs.count()
    );

    // Prove the EXACT expected publisher SET was delivered, not merely the
    // cardinality (a matching count with a wrong publisher would be a
    // silent hole). Exact-set + exact-count together pin the endpoint.
    let claimant_ids: Vec<u64> = claimants.iter().map(|c| c.node_id()).collect();
    for (i, cc) in ccs.iter().enumerate() {
        let expected: HashSet<u64> = claimant_ids
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .map(|(_, id)| *id)
            .collect();
        assert_eq!(
            cc.seen_publishers(),
            expected,
            "claimant {i} must observe EXACTLY the other claimants' reservations"
        );
    }
    let all_claimants: HashSet<u64> = claimant_ids.iter().copied().collect();
    assert_eq!(
        cobs.seen_publishers(),
        all_claimants,
        "observer must observe EXACTLY all claimants"
    );
    println!(
        "  [PASS] claimant counts N-1 (=2) & observer counts N (=3), exact publisher sets, all claims Won"
    );
}

// ============================================================================
// Exact-count barrier (Kyra ICB-0 Blocker 1).
// ============================================================================

async fn w_exact_barrier_rejects_overshoot() {
    let island = 0x7Au64;
    let (cr, _fold) = unit_router(island);
    // Record TWO unique verified deliveries for the tracked island.
    let a = EntityKeypair::generate();
    let b = EntityKeypair::generate();
    let _ = cr.try_route(a.entity_id(), &reserve_bytes(&a, island, 1));
    let _ = cr.try_route(b.entity_id(), &reserve_bytes(&b, island, 1));
    assert_eq!(cr.count(), 2, "two distinct deliveries recorded");
    // An EXACT barrier with expected=1 must REJECT the overshoot...
    assert!(
        !wait_count(&cr, 1, Duration::from_millis(200)).await,
        "exact-count barrier must reject overshoot (count 2, expected 1)"
    );
    // ...and accept an exact match.
    assert!(
        wait_count(&cr, 2, Duration::from_millis(200)).await,
        "exact-count barrier must accept an exact match (count 2, expected 2)"
    );
    println!("  [PASS] exact-count barrier rejects overshoot (2 > 1 -> false)");
}

// ============================================================================
// Fixture-reset safety (Kyra ICB-0 Blocker 2).
// ============================================================================

async fn w_reset_succeeds() {
    let (h, o) = pair().await;
    let island = 0x9Bu64;
    assert_eq!(
        h.reserve_island(island, far_deadline())
            .await
            .expect("reserve"),
        ClaimOutcome::Won
    );
    // O converges to holder H...
    let mut rxo = o.reservation_fold().subscribe_changes();
    tokio::time::timeout(
        Duration::from_secs(3),
        await_reservation_holder(&mut rxo, o.reservation_fold(), island, h.node_id()),
    )
    .await
    .expect("observer must see H as holder");
    // ...then a fail-loud reset reaches exact Free on both H and O.
    release_and_await_free(&h, &[&o], island, Duration::from_secs(5)).await;
    assert_eq!(
        holder_of(h.reservation_fold(), island),
        None,
        "holder fold must be Free after reset"
    );
    assert_eq!(
        holder_of(o.reservation_fold(), island),
        None,
        "observer fold must be Free after reset"
    );
    println!("  [PASS] fixture reset reaches exact Free on holder + observer");
}

async fn w_reset_failure_fails_loud() {
    let (h, _peer) = pair().await;
    let island = 0x9Au64;
    assert_eq!(
        h.reserve_island(island, far_deadline())
            .await
            .expect("reserve"),
        ClaimOutcome::Won
    );
    // An isolated observer: the island is held locally by a FOREIGN
    // publisher and it is NOT connected to H, so H's release never reaches
    // it → reset must fail loud. A short timeout keeps the harness fast.
    let o = node().await;
    o.start_arc();
    let foreign = EntityKeypair::generate();
    apply_reserve(o.reservation_fold(), &foreign, island, 1);
    let h2 = h.clone();
    let o2 = o.clone();
    // Suppress the expected panic's default output.
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let handle = tokio::spawn(async move {
        release_and_await_free(&h2, &[&o2], island, Duration::from_millis(300)).await
    });
    let res = handle.await;
    std::panic::set_hook(prev);
    assert!(
        matches!(&res, Err(e) if e.is_panic()),
        "fixture reset must fail loud when an observer never reaches Free (got {res:?})"
    );
    println!("  [PASS] fixture reset fails loud when an observer stays held");
}
