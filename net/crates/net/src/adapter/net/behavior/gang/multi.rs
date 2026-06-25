//! Multi-island gang protocol (plan §4 / Phase C) — the genuinely
//! new distributed-systems work: claiming N islands all-or-none.
//!
//! **Option 4a — ordered acquire + bounded backoff (the default).**
//! Acquire islands in ascending [`IslandId`] order. That single
//! global lock-ordering is what makes the protocol deadlock-free: a
//! gang only ever waits on an island whose id is greater than every
//! island it currently holds, so a hold-and-wait *cycle* across gangs
//! is impossible (the classic A-holds-1-wants-2 / B-holds-2-wants-1
//! deadlock can't form when both acquire 1 before 2). On any reject
//! the attempt releases everything it grabbed and backs off, so a
//! gang never blocks while holding — and a deadline bounds the retry
//! loop, so a gang that can't assemble its set fails cleanly (holding
//! nothing) instead of livelocking.
//!
//! The reserve TTL handles the **node-killed-mid-claim** case for
//! free: a dead claimant's `Reserved` islands lapse at their
//! `until_unix_us` deadline, and the reservation fold's cross-
//! publisher takeover lets a live gang reclaim them — no sweeper, no
//! coordination.
//!
//! Option 4b (two-phase reserve→commit) is parked for gangs whose
//! island count makes ordered-acquire backoff pathological; 4a ships
//! first (plan §4).
//!
//! This module stops at "all islands `Reserved` by one gang". The
//! `→ Active` edge — quorum-witnessed with a fencing epoch — is
//! Phase D.

use crate::adapter::net::behavior::fold::{Fold, IslandId, JobId, NodeId, ReservationFold};
use crate::adapter::net::identity::EntityKeypair;

use super::claim::{release_island, single_island_claim, ClaimError, ClaimOutcome, Claimant};

/// A gang job's claim over multiple islands — all-or-none. The plan
/// types `islands` as a `SmallVec`; the crate has no `smallvec` dep,
/// so a `Vec` stands in (gang island counts are tiny).
#[derive(Debug, Clone)]
pub struct GangClaim {
    /// The job this gang runs once it holds every island.
    pub job: JobId,
    /// Islands the gang needs, all-or-none. Acquire normalizes to
    /// ascending order; input order is irrelevant.
    pub islands: Vec<IslandId>,
    /// Wall-clock-micros deadline after which [`acquire_gang`] gives
    /// up retrying and returns [`GangOutcome::DeadlineExceeded`].
    pub deadline_us: u64,
}

/// Result of one [`try_acquire_gang`] pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquireAttempt {
    /// Every island was reserved — the gang holds the full set.
    Held,
    /// `blocker` was held by someone else. Every island this attempt
    /// had already grabbed was released before returning, so the gang
    /// holds nothing.
    Blocked(IslandId),
}

/// Outcome of the full [`acquire_gang`] retry loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GangOutcome {
    /// All islands held (`Reserved`), in ascending id order. The
    /// caller proceeds to `→ Active` (Phase D adds the quorum gate).
    Held(Vec<IslandId>),
    /// Could not assemble the full set before `deadline_us`; the gang
    /// holds nothing (every partial hold was released).
    DeadlineExceeded,
}

/// One ordered-acquire pass: reserve every island in **ascending id
/// order**. On the first reject, release everything grabbed so far
/// (in reverse) and return [`AcquireAttempt::Blocked`]. Deterministic
/// and side-effect-clean (holds the full set or nothing), so it's the
/// unit-testable core of the protocol.
///
/// `islands_sorted` MUST be sorted ascending + deduped — the global
/// lock-ordering invariant. [`acquire_gang`] guarantees this;
/// call it directly only with an already-normalized slice.
pub fn try_acquire_gang(
    reservations: &Fold<ReservationFold>,
    keypair: &EntityKeypair,
    node_id: NodeId,
    generation: &mut u64,
    islands_sorted: &[IslandId],
    until_unix_us: u64,
) -> Result<AcquireAttempt, ClaimError> {
    let mut held: Vec<IslandId> = Vec::with_capacity(islands_sorted.len());
    for &island in islands_sorted {
        let gen = *generation;
        *generation += 1;
        match single_island_claim(reservations, keypair, node_id, gen, island, until_unix_us)? {
            ClaimOutcome::Won => held.push(island),
            ClaimOutcome::Lost => {
                // Release everything grabbed this attempt so the gang
                // never blocks while holding. Reverse order is
                // cosmetic — releases are independent CAS-es. Best-
                // effort: a `?` here would short-circuit the rollback on
                // a (rare) sign/apply error mid-loop and strand the
                // earlier-grabbed islands, breaking all-or-none. All-or-
                // none wins; every grabbed island gets a release attempt
                // (review #8).
                for &grabbed in held.iter().rev() {
                    let gen = *generation;
                    *generation += 1;
                    let _ = release_island(reservations, keypair, node_id, gen, grabbed);
                }
                return Ok(AcquireAttempt::Blocked(island));
            }
        }
    }
    Ok(AcquireAttempt::Held)
}

/// Acquire a whole gang's islands all-or-none: retry
/// [`try_acquire_gang`] with bounded backoff until the full set is
/// held or `claim.deadline_us` passes.
///
/// `reserve_ttl_us` is how long each `Reserved` lasts before a
/// foreign gang may take it over (sized from the claim-round latency,
/// plan open question 3); a fresh deadline is stamped on every
/// attempt. `now_us` and `backoff` are injected so the loop is
/// deterministically testable: production passes the crate's
/// `current_timestamp_micros` and a jittered sleep; tests pass a fake
/// clock and a no-op. `backoff` receives the zero-based attempt
/// number. `claimant` carries the identity + generation; the loop
/// advances its generation through [`try_acquire_gang`].
pub fn acquire_gang(
    claimant: &mut Claimant,
    claim: &GangClaim,
    reserve_ttl_us: u64,
    now_us: impl Fn() -> u64,
    mut backoff: impl FnMut(u32),
) -> Result<GangOutcome, ClaimError> {
    // Normalize to the global lock-order once.
    let mut islands = claim.islands.clone();
    islands.sort_unstable();
    islands.dedup();

    let mut attempt = 0u32;
    loop {
        let until = now_us().saturating_add(reserve_ttl_us);
        match try_acquire_gang(
            claimant.reservations,
            claimant.keypair,
            claimant.node_id,
            &mut claimant.generation,
            &islands,
            until,
        )? {
            AcquireAttempt::Held => return Ok(GangOutcome::Held(islands)),
            AcquireAttempt::Blocked(_) => {
                // Check the deadline AFTER releasing (try_acquire_gang
                // already released) so a deadline-exceeded gang holds
                // nothing.
                if now_us() >= claim.deadline_us {
                    return Ok(GangOutcome::DeadlineExceeded);
                }
                backoff(attempt);
                attempt = attempt.saturating_add(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Barrier};
    use std::time::Duration;

    use super::*;
    use crate::adapter::net::behavior::fold::ReservationQuery;
    use crate::adapter::net::current_timestamp_micros;

    fn new_reservations() -> Fold<ReservationFold> {
        Fold::with_sweep_interval(Duration::ZERO)
    }

    fn fresh() -> u64 {
        current_timestamp_micros() + 60_000_000
    }

    fn holder_of(fold: &Fold<ReservationFold>, island: IslandId) -> Option<NodeId> {
        fold.query(ReservationQuery::State(island))
            .first()
            .and_then(|(_, s)| s.holder())
    }

    #[test]
    fn try_acquire_holds_full_set_when_free() {
        let fold = new_reservations();
        let kp = EntityKeypair::generate();
        let n = kp.entity_id().node_id();
        let mut g = 1;
        let r = try_acquire_gang(&fold, &kp, n, &mut g, &[1, 2, 3], fresh()).unwrap();
        assert_eq!(r, AcquireAttempt::Held);
        for island in [1, 2, 3] {
            assert_eq!(holder_of(&fold, island), Some(n));
        }
    }

    #[test]
    fn try_acquire_releases_all_on_block_and_reports_blocker() {
        let fold = new_reservations();
        let a = EntityKeypair::generate();
        let b = EntityKeypair::generate();
        let (na, nb) = (a.entity_id().node_id(), b.entity_id().node_id());

        // B pre-holds island 2.
        single_island_claim(&fold, &b, nb, 1, 2, fresh()).unwrap();

        // A tries [1,2,3]: grabs 1, blocks on 2, must release 1.
        let mut g = 1;
        let r = try_acquire_gang(&fold, &a, na, &mut g, &[1, 2, 3], fresh()).unwrap();
        assert_eq!(r, AcquireAttempt::Blocked(2));
        assert_eq!(holder_of(&fold, 1), None, "island 1 must be released");
        assert_eq!(holder_of(&fold, 2), Some(nb), "B still holds 2");
        assert_eq!(holder_of(&fold, 3), None, "never reached 3");
    }

    #[test]
    fn ascending_lock_order_prevents_the_classic_two_gang_deadlock() {
        // A wants {2,1}, B wants {1,2} — opposite input orders, the
        // setup that deadlocks WITHOUT a global lock-ordering. Both
        // normalize to [1,2], so whoever grabs island 1 first wins
        // the round; the other blocks on 1 and holds nothing. No
        // hold-and-wait cycle can form.
        let fold = new_reservations();
        let a = EntityKeypair::generate();
        let b = EntityKeypair::generate();
        let (na, nb) = (a.entity_id().node_id(), b.entity_id().node_id());

        // A acquires first (single-threaded determinism).
        let mut ca = Claimant::new(&fold, &a, na);
        let ra = acquire_gang(
            &mut ca,
            &GangClaim {
                job: 1,
                islands: vec![2, 1], // unsorted on purpose
                deadline_us: fresh(),
            },
            60_000_000,
            current_timestamp_micros,
            |_| {},
        )
        .unwrap();
        assert_eq!(
            ra,
            GangOutcome::Held(vec![1, 2]),
            "A holds the set in id order"
        );

        // B, deadline in the past → exactly one blocked attempt then
        // a clean give-up holding nothing (no deadlock, no spin).
        let mut cb = Claimant::new(&fold, &b, nb);
        let rb = acquire_gang(
            &mut cb,
            &GangClaim {
                job: 2,
                islands: vec![1, 2],
                deadline_us: 0, // already past → give up after one try
            },
            60_000_000,
            current_timestamp_micros,
            |_| {},
        )
        .unwrap();
        assert_eq!(rb, GangOutcome::DeadlineExceeded);
        // B left nothing behind; A still holds both.
        assert_eq!(holder_of(&fold, 1), Some(na));
        assert_eq!(holder_of(&fold, 2), Some(na));
    }

    #[test]
    fn retry_succeeds_after_the_blocker_releases() {
        // A holds {2}. B wants {1,2}: first attempt blocks on 2 and
        // releases 1. A releases 2. B's next attempt holds {1,2}.
        // The injected clock lets exactly two attempts run.
        let fold = new_reservations();
        let a = EntityKeypair::generate();
        let b = EntityKeypair::generate();
        let (na, nb) = (a.entity_id().node_id(), b.entity_id().node_id());

        single_island_claim(&fold, &a, na, 1, 2, fresh()).unwrap();

        // Clock advances 1 unit per read; deadline far in the future.
        let clock = AtomicU64::new(1);
        let now = || clock.fetch_add(1, Ordering::Relaxed);

        // After the first blocked attempt, A releases island 2 from
        // the backoff hook — simulating the holder finishing.
        let mut ga_rel = 100u64;
        let backoff = |_attempt: u32| {
            release_island(&fold, &a, na, ga_rel, 2).unwrap();
            ga_rel += 1;
        };

        let mut cb = Claimant::new(&fold, &b, nb);
        let rb = acquire_gang(
            &mut cb,
            &GangClaim {
                job: 2,
                islands: vec![1, 2],
                deadline_us: u64::MAX,
            },
            60_000_000,
            now,
            backoff,
        )
        .unwrap();
        assert_eq!(rb, GangOutcome::Held(vec![1, 2]));
        assert_eq!(holder_of(&fold, 1), Some(nb));
        assert_eq!(holder_of(&fold, 2), Some(nb));
    }

    #[test]
    fn dead_claimants_expired_reserves_are_taken_over() {
        // Node-killed-mid-claim: A reserves {2,3} with an ALREADY-
        // EXPIRED deadline (simulating a dead node whose reserve
        // lapsed) then never releases. B's ordered acquire of {2,3,4}
        // takes over the expired reserves — no sweeper needed.
        let fold = new_reservations();
        let a = EntityKeypair::generate();
        let b = EntityKeypair::generate();
        let (na, nb) = (a.entity_id().node_id(), b.entity_id().node_id());

        let expired = current_timestamp_micros().saturating_sub(60_000_000);
        let mut ga = 1;
        // A grabs {2,3} with expired TTL (held, but reclaimable).
        let ra = try_acquire_gang(&fold, &a, na, &mut ga, &[2, 3], expired).unwrap();
        assert_eq!(ra, AcquireAttempt::Held);
        assert_eq!(holder_of(&fold, 2), Some(na));

        // B acquires {2,3,4}; the expired reserves on 2 and 3 are
        // taken over.
        let mut cb = Claimant::new(&fold, &b, nb);
        let rb = acquire_gang(
            &mut cb,
            &GangClaim {
                job: 9,
                islands: vec![2, 3, 4],
                deadline_us: fresh(),
            },
            60_000_000,
            current_timestamp_micros,
            |_| {},
        )
        .unwrap();
        assert_eq!(rb, GangOutcome::Held(vec![2, 3, 4]));
        for island in [2, 3, 4] {
            assert_eq!(holder_of(&fold, island), Some(nb));
        }
    }

    /// Brutal test #2 (plan Phase C): two multi-island gangs with
    /// overlapping islands contend, sustained, released together by a
    /// barrier. Each acquires its full set, holds briefly, releases.
    /// Asserts: both threads TERMINATE (zero deadlock), each ends up
    /// holding its full set at least once (all-or-none progress), and
    /// the all-or-none invariant holds — a gang is never observed
    /// holding a strict, non-empty subset of its islands at rest.
    #[test]
    fn two_overlapping_gangs_make_bounded_deadlock_free_progress() {
        let fold = Arc::new(new_reservations());
        let barrier = Arc::new(Barrier::new(2));
        // Overlap on {3,4}.
        let gangs = [vec![1, 2, 3, 4], vec![3, 4, 5, 6]];

        let handles: Vec<_> = gangs
            .into_iter()
            .map(|islands| {
                let fold = fold.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let kp = EntityKeypair::generate();
                    let node = kp.entity_id().node_id();
                    let mut claimant = Claimant::new(&fold, &kp, node);
                    let claim = GangClaim {
                        job: 1,
                        islands: islands.clone(),
                        // Generous deadline so contention resolves by
                        // taking turns, not by timing out.
                        deadline_us: current_timestamp_micros() + 5_000_000,
                    };
                    barrier.wait();
                    let outcome = acquire_gang(
                        &mut claimant,
                        &claim,
                        2_000_000,
                        current_timestamp_micros,
                        |_| std::thread::sleep(Duration::from_micros(200)),
                    )
                    .expect("acquire");
                    // If held, immediately release so the other gang
                    // can take its turn, and report success.
                    if let GangOutcome::Held(ref held) = outcome {
                        let mut sorted = islands.clone();
                        sorted.sort_unstable();
                        assert_eq!(held, &sorted, "Held set is the full gang, in order");
                        for &island in held {
                            release_island(&fold, &kp, node, claimant.next_gen(), island).unwrap();
                        }
                    }
                    matches!(outcome, GangOutcome::Held(_))
                })
            })
            .collect();

        // Both threads must terminate (no deadlock / hang). With the
        // generous deadline + release-after-hold, both should win a
        // turn.
        let results: Vec<bool> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert!(
            results.iter().all(|&held| held),
            "both gangs eventually assembled their full set (bounded, deadlock-free): {results:?}",
        );
        // Everything released at rest.
        for island in [1, 2, 3, 4, 5, 6] {
            assert_eq!(
                holder_of(&fold, island),
                None,
                "island {island} released at rest"
            );
        }
    }
}
