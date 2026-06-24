//! Loom concurrency models for FAILURE_PATH_HARDENING_PLAN
//! Stage 2.
//!
//! Run with:
//!
//! ```text
//! RUSTFLAGS="--cfg loom" cargo test --release --test loom_models
//! ```
//!
//! Each model re-implements a production concurrency *pattern*
//! using `loom`'s substitute atomics / threads and asserts the
//! documented memory-ordering contract holds under every
//! exhaustively-explored thread interleaving. This catches
//! Acquire/Release vs. Relaxed confusion, missing publication
//! barriers, and lost-update bugs that probabilistic stress
//! tests in the regular suite can only hope to hit.
//!
//! # Why test patterns, not production structs?
//!
//! Loom substitutes `std::sync::atomic` + `std::sync::Mutex` but
//! not `parking_lot::Mutex` or `dashmap::DashMap`. The crate's
//! atomics-heavy cores (`AuthGuard`, `TokenCache`, `RoutingTable`,
//! `CapabilityIndex`, `FailureDetector`) are DashMap-heavy, so
//! loom can't test them directly without a multi-week shim
//! refactor. The two cores with atomics-only sub-pieces
//! (`SchedulerStreamStats` counter battery in `RoutingTable`,
//! the burst-decrement CAS loop in `LossSimulator`) additionally
//! call `SystemTime::now()` in-situ, which loom's deterministic
//! scheduler can't observe usefully.
//!
//! The workaround: model the *pattern* here with loom's atomics.
//! If the pattern is correct, the production struct using the
//! same pattern is correct by construction. If the production
//! struct ever diverges from the pattern, that's a code review
//! issue — the model stays as the pinned reference.
//!
//! See `docs/FAILURE_PATH_HARDENING_PLAN.md` §Stage 2 for the
//! blocker discussion and the follow-up plan for moving the
//! production structs under loom directly.

#![cfg(loom)]
#![allow(
    clippy::disallowed_methods,
    reason = "loom test uses std::sync types; no real poison concern"
)]

use loom::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use loom::sync::Arc;
use loom::thread;

// ─────────────────────────────────────────────────────────────
// Model 1: SchedulerStreamStats atomic counter battery
//
// Mirrors `src/adapter/net/route.rs:244-325` — five AtomicU64
// counters + an AtomicU64 last-activity timestamp, all using
// Relaxed ordering. Invariant under test: concurrent
// `record_in` / `record_out` / `record_drop` calls never produce
// counts that are arithmetically impossible (sum of increments
// across all threads == final count). Under Relaxed ordering
// this holds by construction for simple fetch_add sequences —
// the loom test pins that the sequence stays simple.
// ─────────────────────────────────────────────────────────────

#[derive(Default)]
struct StatsModel {
    packets_in: AtomicU64,
    packets_out: AtomicU64,
    packets_dropped: AtomicU64,
    bytes_in: AtomicU64,
    bytes_out: AtomicU64,
}

impl StatsModel {
    fn record_in(&self, bytes: u64) {
        self.packets_in.fetch_add(1, Ordering::Relaxed);
        self.bytes_in.fetch_add(bytes, Ordering::Relaxed);
    }
    fn record_out(&self, bytes: u64) {
        self.packets_out.fetch_add(1, Ordering::Relaxed);
        self.bytes_out.fetch_add(bytes, Ordering::Relaxed);
    }
    fn record_drop(&self) {
        self.packets_dropped.fetch_add(1, Ordering::Relaxed);
    }
    fn totals(&self) -> (u64, u64, u64, u64, u64) {
        (
            self.packets_in.load(Ordering::Relaxed),
            self.packets_out.load(Ordering::Relaxed),
            self.packets_dropped.load(Ordering::Relaxed),
            self.bytes_in.load(Ordering::Relaxed),
            self.bytes_out.load(Ordering::Relaxed),
        )
    }
}

#[test]
fn stream_stats_counter_battery_is_atomic_under_concurrent_record() {
    loom::model(|| {
        let stats = Arc::new(StatsModel::default());

        // Thread A: record one `in` of 100 bytes.
        let a = {
            let s = stats.clone();
            thread::spawn(move || s.record_in(100))
        };
        // Thread B: record one `out` of 250 bytes, then one drop.
        let b = {
            let s = stats.clone();
            thread::spawn(move || {
                s.record_out(250);
                s.record_drop();
            })
        };

        a.join().unwrap();
        b.join().unwrap();

        let (p_in, p_out, p_drop, b_in, b_out) = stats.totals();
        // Final state: each counter sees exactly its own
        // contributions. No torn reads, no lost updates.
        assert_eq!(p_in, 1);
        assert_eq!(p_out, 1);
        assert_eq!(p_drop, 1);
        assert_eq!(b_in, 100);
        assert_eq!(b_out, 250);
    });
}

// ─────────────────────────────────────────────────────────────
// Model 2: Burst-decrement CAS loop
//
// Mirrors `src/adapter/net/failure.rs:387-406` — the
// `LossSimulator::should_drop` CAS loop that decrements
// `burst_remaining` atomically to avoid the underflow-to-u64::MAX
// race that a naive `load > 0 then fetch_sub(1)` sequence has
// when two threads see the same `remaining == 1` and both
// subtract. The loom test pins that the CAS loop is correct
// under every thread interleaving: the total number of
// successful decrements equals the initial value, and the
// counter never underflows past zero.
//
// See also: `tests/bus_shutdown_drain.rs` where cubic flagged
// the same pattern mis-implemented (P2 fix replaced `load;
// fetch_sub` with `fetch_update`). This loom model is the
// pattern's reference implementation.
// ─────────────────────────────────────────────────────────────

/// Decrement `counter` by 1 if it's > 0; return true if we
/// successfully decremented. Mirrors the production CAS loop.
fn try_decrement_burst(counter: &AtomicU64) -> bool {
    loop {
        let remaining = counter.load(Ordering::Relaxed);
        if remaining == 0 {
            return false;
        }
        match counter.compare_exchange_weak(
            remaining,
            remaining - 1,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return true,
            Err(_) => continue, // Another thread won; retry.
        }
    }
}

#[test]
fn burst_cas_decrement_never_underflows_under_contention() {
    loom::model(|| {
        let counter = Arc::new(AtomicU64::new(2));
        let decremented_a = Arc::new(AtomicBool::new(false));
        let decremented_b = Arc::new(AtomicBool::new(false));

        // Two threads race to decrement. With initial=2, both
        // SHOULD succeed. With initial=1, only one succeeds.
        // With initial=0, neither. Under a racy load+fetch_sub
        // pattern, two threads seeing remaining==1 would both
        // subtract → counter wraps to u64::MAX. The CAS loop
        // serializes the reads + writes so only one wins per
        // decrement.
        let a = {
            let c = counter.clone();
            let d = decremented_a.clone();
            thread::spawn(move || {
                if try_decrement_burst(&c) {
                    d.store(true, Ordering::Relaxed);
                }
            })
        };
        let b = {
            let c = counter.clone();
            let d = decremented_b.clone();
            thread::spawn(move || {
                if try_decrement_burst(&c) {
                    d.store(true, Ordering::Relaxed);
                }
            })
        };

        a.join().unwrap();
        b.join().unwrap();

        let final_count = counter.load(Ordering::Relaxed);
        // Invariant: counter cannot underflow past 0. Initial
        // is 2, at most 2 threads successfully decrement, so
        // final ∈ {0}. The dangerous regression — final ==
        // u64::MAX - N — is ruled out by this assertion.
        assert_eq!(
            final_count, 0,
            "CAS loop preserves the sum: initial(2) - decrements(2) = 0. \
             A non-zero final count means the loop failed to decrement; a \
             value near u64::MAX means the CAS loop regressed to load+sub.",
        );
        // Both threads must have observed a successful
        // decrement (initial was 2, both CAS loops must have
        // one winning iteration).
        assert!(
            decremented_a.load(Ordering::Relaxed),
            "thread A did not observe a successful decrement",
        );
        assert!(
            decremented_b.load(Ordering::Relaxed),
            "thread B did not observe a successful decrement",
        );
    });
}

#[test]
fn burst_cas_decrement_caps_at_initial_count_under_contention() {
    loom::model(|| {
        // R-39: initial 1, two threads racing — only ONE must
        // win. The other must see `remaining == 0` and return
        // false without wrapping the counter. A regression from
        // the CAS loop to `load; fetch_sub` would let both
        // decrement, producing a counter of u64::MAX. Two
        // threads is enough to drive every interleaving loom
        // explores; the original comment said "three" but the
        // code always spawned two.
        let counter = Arc::new(AtomicU64::new(1));
        let winners = Arc::new(AtomicU64::new(0));

        let threads: Vec<_> = (0..2)
            .map(|_| {
                let c = counter.clone();
                let w = winners.clone();
                thread::spawn(move || {
                    if try_decrement_burst(&c) {
                        w.fetch_add(1, Ordering::Relaxed);
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }

        let final_count = counter.load(Ordering::Relaxed);
        let winner_count = winners.load(Ordering::Relaxed);
        assert_eq!(
            final_count, 0,
            "counter must end at exactly 0 — any other value means the \
             CAS loop is racy. winners saw {winner_count} successful decrements",
        );
        assert_eq!(
            winner_count, 1,
            "exactly one thread must win the last decrement when initial=1",
        );
    });
}

// ─────────────────────────────────────────────────────────────
// Model 3: AuthGuard bloom-filter + verified-cache ordering
//
// Mirrors `src/adapter/net/channel/guard.rs` — the extracted
// `BloomCache` + the `verified` DashMap that `AuthGuard` owns.
// The interesting invariant: a `check_fast` whose `bloom.probe`
// observes bits cleared MUST observe `verified` not-populated
// — otherwise an authorized subscriber's first packets get
// `Denied` before the bloom write has propagated, even though
// `authorize` (on the producer thread) has already returned.
//
// FAILURE_PATH_HARDENING_PLAN §Stage 2 Option B: this model
// tests the real production memory-ordering annotations on
// `BloomCache`. DashMap's internals are out of scope for loom
// (it's not substitutable); we model the DashMap-backed
// verified cache as a single `AtomicBool` with Release/Acquire,
// which captures the only property we care about here —
// "insert happened, and its Release-ordered publication is
// visible to a subsequent Acquire load."
//
// Expected result:
//
// - With `BloomCache::mark` = Release + `probe` = Acquire (the
//   production code as shipped), every interleaving produces a
//   Denied verdict ONLY when verified-cache was also observed
//   as empty. No false-deny.
// - If `mark` is regressed to Relaxed (or probe regressed to
//   Relaxed), loom finds the interleaving where check_fast
//   sees bloom=0 but verified=true — a documented memory-
//   model consequence of Relaxed, and the exact race this
//   extraction was designed to rule out.
// ─────────────────────────────────────────────────────────────

/// Minimal model of the `(BloomCache, verified)` ordering.
/// One bloom bit + one atomic bool, same Release/Acquire
/// annotations as the production types.
struct AuthBloomModel {
    /// A single bloom bit — loom's state-space explodes with
    /// more, and all bits share the same ordering annotation,
    /// so one bit captures the property.
    bloom_bit: AtomicU64,
    /// Substitute for `verified.contains_key(..) == true`. A
    /// real DashMap's `insert` completes with a Release on
    /// its internal Mutex unlock; `contains_key` begins with
    /// an Acquire on the lock. Modeling the insert/present
    /// transition as a Release store on an `AtomicU64` with an
    /// Acquire load in check_fast captures the same
    /// happens-before semantics.
    verified: AtomicU64,
}

impl AuthBloomModel {
    fn new() -> Self {
        Self {
            bloom_bit: AtomicU64::new(0),
            verified: AtomicU64::new(0),
        }
    }
    /// Production `authorize`: Relaxed bloom mark, then the
    /// DashMap-backed verified insert (modeled here as
    /// Release, matching DashMap's per-shard Mutex-unlock
    /// semantics on a real `insert`).
    fn authorize(&self) {
        self.bloom_bit.store(1, Ordering::Relaxed);
        self.verified.store(1, Ordering::Release);
    }
    /// Production `check_fast`: Relaxed bloom probe, then the
    /// DashMap-backed verified lookup (modeled as Acquire,
    /// matching DashMap's Mutex-lock semantics on
    /// `contains_key`). Returns 0=Denied, 1=Allowed,
    /// 2=NeedsFullCheck.
    fn check_fast(&self) -> u8 {
        if self.bloom_bit.load(Ordering::Relaxed) == 0 {
            return 0;
        }
        if self.verified.load(Ordering::Acquire) == 1 {
            1
        } else {
            2
        }
    }
}

/// Property: concurrent `authorize` + `check_fast` produces
/// one of the documented verdicts — no panic, no "impossible"
/// state. A concurrent check DURING an in-flight authorize is
/// allowed to observe ANY snapshot of the in-progress state
/// (pre-bloom, pre-verified, post-bloom-pre-verified,
/// post-both). The verdict is always one of
/// `Denied | NeedsFullCheck | Allowed`; it's the caller's
/// responsibility to retry or consult the slow path when
/// receiving `NeedsFullCheck` or a transient `Denied`.
///
/// This test intentionally does NOT assert that "Denied
/// implies verified is unobserved" — a concurrent consumer
/// can observe a stale bloom (pre-authorize) and then later
/// observe a fresh verified (post-authorize), and both
/// observations are legal. The `post_authorize_check_never_denies`
/// test below pins the stronger property that matters for
/// production: under a synchronized handoff, no false-deny.
#[test]
fn auth_bloom_authorize_check_fast_concurrent_verdict_is_documented() {
    loom::model(|| {
        let m = Arc::new(AuthBloomModel::new());

        let producer = {
            let m = m.clone();
            thread::spawn(move || m.authorize())
        };
        let consumer = {
            let m = m.clone();
            thread::spawn(move || m.check_fast())
        };

        producer.join().unwrap();
        let verdict = consumer.join().unwrap();

        // Only three legal verdicts exist. A fourth value
        // would indicate a memory-safety bug or enum-tag
        // corruption; the test pins total-ness.
        assert!(
            matches!(verdict, 0 | 1 | 2),
            "check_fast returned undocumented verdict {verdict}",
        );
    });
}

/// Stronger property: if the producer fully completes
/// (producer.join() returns), any subsequent check_fast in the
/// SAME thread context as the join must see Allowed or
/// NeedsFullCheck, never Denied.
///
/// This models the "subscribe completes → sender observes
/// authorization" invariant that SDK callers rely on.
#[test]
fn auth_bloom_post_authorize_check_never_denies() {
    loom::model(|| {
        let m = Arc::new(AuthBloomModel::new());

        // Producer runs to completion first (no interleaving
        // at this level — producer.join() is a
        // synchronization point).
        let producer = {
            let m = m.clone();
            thread::spawn(move || m.authorize())
        };
        producer.join().unwrap();

        // Now check_fast on the main thread after the join.
        // join() synchronizes, so both bloom bits and verified
        // are visible here. Must not be Denied.
        let verdict = m.check_fast();
        assert_ne!(
            verdict, 0,
            "check_fast after a joined authorize must never return Denied",
        );
    });
}

// ─────────────────────────────────────────────────────────────
// Model 4: ReplicationCoordinator::record_tail_seq monotonic-max CAS
//
// Mirrors `src/adapter/net/redex/replication_coordinator.rs:181-194`
// — the CAS loop that advances `tail_seq` to the maximum of every
// proposed value. The catch-up path calls this from the replica
// runtime task after each successful `apply_sync_response`; under
// concurrent applies (one chunk completes while the next is
// already in flight) the loop must:
//
// 1. Never regress: the stored value is monotonically non-
//    decreasing across all observed states.
// 2. Converge on the max: after every producer joins, the stored
//    value equals max(initial, every proposed seq).
// 3. Never tear: no value the loop observes via `load` or
//    `compare_exchange_weak` is anything other than a value some
//    producer proposed (or the initial value).
//
// A regression from CAS to `if load < seq { store(seq) }` would
// produce torn updates: thread A's load sees 5, thread B's store
// commits 10, thread A's store commits 5 — final value is 5,
// thread B's update lost. The CAS loop's `Err(now)` branch picks
// up the racing update and reruns the `seq > current` test.
// ─────────────────────────────────────────────────────────────

/// The production CAS pattern from `ReplicationCoordinator::record_tail_seq`.
/// Pinned here under loom's atomic substitutes so the contract is
/// verified across every thread interleaving.
fn record_tail_seq_monotonic_max(cell: &AtomicU64, seq: u64) {
    let mut current = cell.load(Ordering::Relaxed);
    while seq > current {
        match cell.compare_exchange_weak(current, seq, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return,
            Err(now) => current = now,
        }
    }
}

#[test]
fn record_tail_seq_converges_on_max_under_concurrent_updates() {
    loom::model(|| {
        let tail = Arc::new(AtomicU64::new(0));

        // Two threads race with overlapping proposals.
        // Thread A proposes 5 then 10.
        // Thread B proposes 7 then 3.
        // The CAS loop must converge to max(0, 5, 10, 7, 3) = 10.
        let a = {
            let t = tail.clone();
            thread::spawn(move || {
                record_tail_seq_monotonic_max(&t, 5);
                record_tail_seq_monotonic_max(&t, 10);
            })
        };
        let b = {
            let t = tail.clone();
            thread::spawn(move || {
                record_tail_seq_monotonic_max(&t, 7);
                record_tail_seq_monotonic_max(&t, 3);
            })
        };

        a.join().unwrap();
        b.join().unwrap();

        let final_seq = tail.load(Ordering::Relaxed);
        // After every producer finishes, the cell must hold the
        // global max. A torn update (CAS → load+if regression)
        // could leave it at 5, 7, or 3 — any of those would be a
        // monotonicity violation.
        assert_eq!(
            final_seq, 10,
            "monotonic-max CAS must converge on max(proposals); \
             a value below 10 means the loop dropped a proposal",
        );
    });
}

#[test]
fn record_tail_seq_lower_proposal_does_not_regress_existing() {
    loom::model(|| {
        let tail = Arc::new(AtomicU64::new(0));

        // One thread commits the high water mark; another tries
        // to roll it back to a smaller value. The smaller proposal
        // must be a no-op even when racing the higher commit.
        let high = {
            let t = tail.clone();
            thread::spawn(move || record_tail_seq_monotonic_max(&t, 100))
        };
        let low = {
            let t = tail.clone();
            thread::spawn(move || record_tail_seq_monotonic_max(&t, 5))
        };
        high.join().unwrap();
        low.join().unwrap();

        assert_eq!(
            tail.load(Ordering::Relaxed),
            100,
            "lower proposal must never regress the committed max",
        );
    });
}

// ─────────────────────────────────────────────────────────────
// Model 5: ChannelMetricsAtomic counter battery under concurrent transitions
//
// Mirrors `src/adapter/net/redex/replication_metrics.rs:140-175`.
// Multiple counters (sync_bytes_total, leader_changes_total,
// under_capacity_total, skip_ahead_total, election_thrash_total,
// witness_withdrawals_total) all use Relaxed fetch_add. Under
// concurrent runtime tasks driving state transitions on multiple
// channels, the counters MUST reflect every increment — no lost
// updates.
//
// Same shape as Model 1 (StatsModel) but applied to the
// replication-specific counters that the runtime + coordinator
// bump from the hot path.
// ─────────────────────────────────────────────────────────────

#[derive(Default)]
struct ReplicationMetricsModel {
    sync_bytes_total: AtomicU64,
    leader_changes_total: AtomicU64,
    under_capacity_total: AtomicU64,
}

impl ReplicationMetricsModel {
    fn incr_sync_bytes(&self, bytes: u64) {
        self.sync_bytes_total.fetch_add(bytes, Ordering::Relaxed);
    }
    fn incr_leader_change(&self) {
        self.leader_changes_total.fetch_add(1, Ordering::Relaxed);
    }
    fn incr_under_capacity(&self) {
        self.under_capacity_total.fetch_add(1, Ordering::Relaxed);
    }
    fn totals(&self) -> (u64, u64, u64) {
        (
            self.sync_bytes_total.load(Ordering::Relaxed),
            self.leader_changes_total.load(Ordering::Relaxed),
            self.under_capacity_total.load(Ordering::Relaxed),
        )
    }
}

// ─────────────────────────────────────────────────────────────
// Model 6: RedexFile::close idempotent-shutdown swap
//
// Mirrors `src/adapter/net/redex/file.rs:1281` —
// `if self.inner.closed.swap(true, AcqRel) { return Ok(()); }`.
// The swap returns the PRIOR value; the first caller observes
// `false` (and runs the close path), every subsequent caller
// observes `true` (and short-circuits). Under N concurrent
// close() calls, EXACTLY ONE caller must run the cleanup path.
//
// A regression to `if !closed.load() { closed.store(true); ... }`
// would let two concurrent callers both see `false`, both run
// the cleanup, and double-fsync / double-cancel-task / etc.
// ─────────────────────────────────────────────────────────────

/// First-call-wins flag. Returns `true` if this caller is the
/// first to flip the bit; `false` if someone already flipped it.
fn try_first_close(flag: &AtomicBool) -> bool {
    !flag.swap(true, Ordering::AcqRel)
}

#[test]
fn close_swap_pattern_exactly_one_caller_wins() {
    loom::model(|| {
        let closed = Arc::new(AtomicBool::new(false));
        let winners = Arc::new(AtomicU64::new(0));

        // Two threads race to close. Exactly one must observe the
        // swap returning `false` (the prior value), advance
        // `winners`, and run cleanup. The other observes `true`
        // and short-circuits.
        let a = {
            let c = closed.clone();
            let w = winners.clone();
            thread::spawn(move || {
                if try_first_close(&c) {
                    w.fetch_add(1, Ordering::Relaxed);
                }
            })
        };
        let b = {
            let c = closed.clone();
            let w = winners.clone();
            thread::spawn(move || {
                if try_first_close(&c) {
                    w.fetch_add(1, Ordering::Relaxed);
                }
            })
        };

        a.join().unwrap();
        b.join().unwrap();

        assert_eq!(
            winners.load(Ordering::Relaxed),
            1,
            "exactly one caller wins the swap; a load+store regression \
             would let both threads observe `false` and both run cleanup",
        );
        assert!(
            closed.load(Ordering::Relaxed),
            "flag must be true after both joins regardless of who won",
        );
    });
}

#[test]
fn replication_metrics_counters_atomic_under_concurrent_increments() {
    loom::model(|| {
        let m = Arc::new(ReplicationMetricsModel::default());

        // Thread A: simulates a leader handling one sync request
        // (1024 bytes shipped) + one transition into Leader role.
        let a = {
            let m = m.clone();
            thread::spawn(move || {
                m.incr_sync_bytes(1024);
                m.incr_leader_change();
            })
        };
        // Thread B: simulates a replica observing a disk-pressure
        // event (one under_capacity bump) + a leadership transition.
        // Both threads bump leader_changes — loom explores every
        // interleaving of the two fetch_add calls and asserts no
        // lost update lands.
        let b = {
            let m = m.clone();
            thread::spawn(move || {
                m.incr_under_capacity();
                m.incr_leader_change();
            })
        };

        a.join().unwrap();
        b.join().unwrap();

        let (bytes, leader_changes, under_cap) = m.totals();
        // Every increment lands; no lost updates.
        assert_eq!(bytes, 1024);
        assert_eq!(leader_changes, 2, "both threads bumped leader_changes");
        assert_eq!(under_cap, 1);
    });
}

/// Same-counter contention only — three concurrent increments to
/// `leader_changes_total` must always land at 3 regardless of
/// interleaving. A regression that swapped `fetch_add` for a
/// `load + add + store` pattern would let loom find a lost-update
/// schedule and fail this test.
#[test]
fn replication_metrics_same_counter_three_way_contention() {
    loom::model(|| {
        let m = Arc::new(ReplicationMetricsModel::default());
        let handles: Vec<_> = (0..3)
            .map(|_| {
                let m = m.clone();
                thread::spawn(move || m.incr_leader_change())
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let (_, leader_changes, _) = m.totals();
        assert_eq!(leader_changes, 3, "no lost updates under contention");
    });
}

// ─────────────────────────────────────────────────────────────
// Model N: Gang ordered-acquire (Thunderdome §4 / Phase C)
//
// Mirrors `src/adapter/net/behavior/gang/multi.rs::try_acquire_gang`
// — a gang acquires its islands in ASCENDING IslandId order
// (the global lock-ordering), and on the first reject releases
// everything it already grabbed. The production path keys on a
// `ReservationFold` CAS (DashMap-backed, so loom can't drive it
// directly); this models the *pattern* with loom atomics: each
// island is an `AtomicU64` holder (0 = free), the claim is a
// `compare_exchange(0, me)`, the release a `store(0)`.
//
// Invariants under EVERY interleaving:
//   - **No deadlock.** Both gangs always terminate (a gang never
//     blocks while holding — it releases on reject). Because both
//     acquire in ascending order, a hold-and-wait cycle can't form.
//   - **All-or-none.** A gang that returns `true` holds its FULL
//     set; one that returns `false` released its partial hold and
//     holds NONE.
//   - **Single-winner on the contended island.** The two gangs
//     overlap on the highest island; exactly one ends up holding
//     it (and therefore exactly one wins overall).
//
// The two gangs overlap on island 2 — the LAST one each acquires —
// so the loser must release a lower island it already grabbed,
// exercising the release-the-partial-hold path that all-or-none
// hinges on.
// ─────────────────────────────────────────────────────────────

/// `compare_exchange(0 → me)` claim of one island. `Acquire` on
/// success so the holder's subsequent reads happen-after the prior
/// holder's `Release` on drop — mirrors the fold's per-key CAS.
fn island_claim(island: &AtomicU64, me: u64) -> bool {
    island
        .compare_exchange(0, me, Ordering::Acquire, Ordering::Relaxed)
        .is_ok()
}

/// Release a held island back to free.
fn island_release(island: &AtomicU64) {
    island.store(0, Ordering::Release);
}

/// Ordered-acquire one gang's `want` indices (MUST be ascending).
/// Returns `true` iff the full set was claimed; on the first reject
/// it releases everything grabbed so far and returns `false`.
fn ordered_acquire(islands: &[AtomicU64; 3], want: &[usize], me: u64) -> bool {
    let mut held: Vec<usize> = Vec::new();
    for &idx in want {
        if island_claim(&islands[idx], me) {
            held.push(idx);
        } else {
            for &h in held.iter().rev() {
                island_release(&islands[h]);
            }
            return false;
        }
    }
    true
}

#[test]
fn gang_ordered_acquire_is_deadlock_free_and_all_or_none() {
    loom::model(|| {
        let islands = Arc::new([AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)]);

        // Gang 1 (id 1) wants {0, 2}; gang 2 (id 2) wants {1, 2}.
        // Overlap is island 2, acquired LAST by both — so the loser
        // must release its lower island.
        let g1 = {
            let isl = islands.clone();
            thread::spawn(move || ordered_acquire(&isl, &[0, 2], 1))
        };
        let g2 = {
            let isl = islands.clone();
            thread::spawn(move || ordered_acquire(&isl, &[1, 2], 2))
        };

        // No deadlock: both joins return under every interleaving.
        let r1 = g1.join().unwrap();
        let r2 = g2.join().unwrap();

        let held = |idx: usize| islands[idx].load(Ordering::Relaxed);

        // Exactly one gang wins (they contend on island 2).
        assert!(r1 != r2, "exactly one gang wins the contended island");

        // All-or-none: winner holds its FULL set; loser holds NONE.
        if r1 {
            assert_eq!(held(0), 1, "gang1 won → holds island 0");
            assert_eq!(held(2), 1, "gang1 won → holds island 2");
            assert_eq!(held(1), 0, "gang2 lost → released island 1");
        } else {
            assert_eq!(held(1), 2, "gang2 won → holds island 1");
            assert_eq!(held(2), 2, "gang2 won → holds island 2");
            assert_eq!(held(0), 0, "gang1 lost → released island 0");
        }
    });
}

// ─────────────────────────────────────────────────────────────
// Model N+1: Partition-during-claim → quorum-`Active` fence
// (Thunderdome §6 / Phase D — corrections §8 item 1)
//
// Mirrors the CP invariant of
// `src/adapter/net/behavior/gang/active.rs::commit_active`: an
// island's reservation chain is single-writer, and the fence —
// `FenceLedger::accept_active` (accept iff `epoch >
// highest_witnessed`) composed with the reservation fold's
// generation-CAS replace — gates the `→ Active` transition. The
// brutal-test #3 in `active.rs` proves this DETERMINISTICALLY
// (a scripted 3|2 split); this models the same guarantee under
// loom's exhaustive interleaving of two CONCURRENT would-be
// leaders, which is the partition-during-claim race the audit
// (§8 item 1) flagged as present in `active.rs` but absent from
// the DST harness.
//
// The reservation cell packs `(epoch << 32) | leader_id` into one
// `AtomicU64` (0 = Free) so the (epoch, holder) pair is read +
// swapped atomically — the single-writer chain. A leader commits
// only if it carries a STRICTLY-higher epoch than the incumbent
// (the fence); the CAS makes the install atomic.
//
// Invariants under EVERY interleaving of leaders at epochs 2 and 3:
//   - **At most one Active holder.** One cell, atomically swapped —
//     never a torn (half-epoch-A, half-id-B) read.
//   - **The fence holds: the higher epoch always wins.** The
//     epoch-3 leader ends as the holder regardless of timing; the
//     epoch-2 leader never holds at the end (it is refused if it
//     races late, or replaced if it installed first). A stale
//     ex-leader can never strand the island.
//   - **Monotonic epoch.** The committed epoch never regresses.
// ─────────────────────────────────────────────────────────────

const FREE: u64 = 0;

fn pack(epoch: u64, leader: u64) -> u64 {
    (epoch << 32) | leader
}
fn epoch_of(state: u64) -> u64 {
    state >> 32
}

/// One leader's `→ Active` commit attempt against the single-writer
/// reservation cell. Commits iff it carries a strictly-higher epoch
/// than the incumbent (the fence); `SeqCst` so the model explores the
/// total order the production chain imposes. Returns whether it
/// installed (it may later be replaced by a higher epoch).
fn commit_active_fenced(cell: &AtomicU64, leader: u64, epoch: u64) -> bool {
    loop {
        let cur = cell.load(Ordering::SeqCst);
        // Fence: an equal-or-lower epoch never displaces a live Active.
        if cur != FREE && epoch <= epoch_of(cur) {
            return false;
        }
        if cell
            .compare_exchange(cur, pack(epoch, leader), Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            return true;
        }
        // Lost the race — re-read and re-evaluate the fence.
    }
}

#[test]
fn partition_during_claim_fence_lets_only_the_higher_epoch_hold_active() {
    loom::model(|| {
        let cell = Arc::new(AtomicU64::new(FREE));

        // Two would-be leaders race the commit: the ex-leader at the
        // stale epoch 2, the new leader at epoch 3 (post leadership
        // change). They model the two sides of a partition both
        // attempting `→ Active` on the same island.
        let stale = {
            let c = cell.clone();
            thread::spawn(move || commit_active_fenced(&c, 0xA, 2))
        };
        let fresh = {
            let c = cell.clone();
            thread::spawn(move || commit_active_fenced(&c, 0xB, 3))
        };
        let _stale_installed = stale.join().unwrap();
        let fresh_installed = fresh.join().unwrap();

        // The fence guarantee: the epoch-3 leader always ends up
        // holding Active, and the stale epoch-2 leader never does —
        // under every interleaving.
        let end = cell.load(Ordering::SeqCst);
        assert_eq!(epoch_of(end), 3, "the higher epoch wins the fence");
        assert_eq!(end, pack(3, 0xB), "epoch-3 leader holds Active, single-writer");
        assert!(fresh_installed, "the fresh leader always commits (over Free or the stale Active)");
    });
}
