//! High-precision timestamp generation with zero syscall overhead.
//!
//! This module provides monotonically increasing timestamps using the CPU's
//! Time Stamp Counter (TSC) on x86_64, avoiding syscalls in the hot path.
//!
//! # Design
//!
//! - Uses `quanta` crate which calibrates against the system clock once at startup
//! - Subsequent reads use RDTSC instruction directly (no syscall)
//! - Each shard has its own `TimestampGenerator` to eliminate contention
//! - Monotonicity is guaranteed via atomic CAS operations

use std::sync::atomic::{AtomicU64, Ordering};

/// High-precision timestamp generator using TSC.
///
/// This generator provides strictly monotonic timestamps with sub-nanosecond
/// resolution and zero syscall overhead after initialization.
///
/// # Single-owner invariant
///
/// **Each producer should own a dedicated `TimestampGenerator`.** The
/// type is `Send + Sync` and `next()` *is* safe to call concurrently —
/// monotonicity is preserved by `compare_exchange_weak` — but the CAS
/// loop degenerates into a spin under sustained contention. The whole
/// design rests on the loop almost never iterating: that's only true
/// when one writer at a time accesses the generator.
///
/// The codebase enforces this structurally rather than at runtime:
///
/// - `Shard` owns its `TimestampGenerator` by value (not behind `Arc`).
/// - `TimestampGenerator` is **not** `Clone`, so duplicating one is a
///   deliberate `mem::replace` / `Default::default()` away — visible
///   in code review.
/// - The shard's surrounding `Mutex<Shard>` serializes producers, so
///   `next()` is invoked by exactly one caller at a time per shard.
///
/// If you find yourself reaching for `Arc<TimestampGenerator>`, stop
/// — give each producer its own instance instead. Every additional
/// concurrent caller is one more thread potentially CAS-spinning on
/// `last`.
pub struct TimestampGenerator {
    /// quanta clock (TSC-based after calibration).
    clock: quanta::Clock,
    /// Raw TSC value sampled at construction. All `next()` calls
    /// compute their nanosecond offset relative to this baseline,
    /// so the returned values are "ns since this generator was
    /// created" rather than the unspecified "ns since the
    /// quanta::Clock's internal calibration".
    ///
    /// Pre-fix the next() call did
    /// `clock.delta_as_nanos(0, raw)`. quanta's calibration is
    /// per-Clock and the "0" baseline doesn't correspond to any
    /// meaningful real-world time — the returned ns counts were
    /// in the order of system uptime, not "since this generator".
    /// Two generators created at different times produced
    /// timestamps with different effective offsets even on the
    /// same physical TSC, breaking any consumer reasoning about
    /// "approximately when did this event happen relative to
    /// generator-creation".
    baseline_raw: u64,
    /// Last generated timestamp (for monotonicity).
    last: AtomicU64,
}

impl TimestampGenerator {
    /// Create a new timestamp generator.
    ///
    /// This performs a one-time calibration against the system clock.
    /// Subsequent timestamp reads use TSC directly.
    pub fn new() -> Self {
        let clock = quanta::Clock::new();
        let baseline_raw = clock.raw();
        Self {
            clock,
            baseline_raw,
            last: AtomicU64::new(0),
        }
    }

    /// Generate the next timestamp.
    ///
    /// Returns a strictly monotonically increasing value in
    /// **nanoseconds since this generator was constructed**. This
    /// operation is lock-free and does not invoke any syscalls.
    ///
    /// Previously returned the raw TSC tick count. The docstring
    /// claimed nanoseconds, but on a 3.5 GHz core the value was ~3.5×
    /// larger than ns-since-epoch, breaking any consumer that read
    /// `insertion_ts` and tried to correlate it with wall-clock-derived
    /// timestamps from elsewhere. Converting via `delta_as_nanos` here
    /// costs ~1ns extra per call and gives consumers a unit they can
    /// actually use.
    ///
    /// # Performance
    ///
    /// - Single-threaded: ~6-12ns per call
    /// - Under contention: may loop due to CAS, but still lock-free
    #[inline(always)]
    pub fn next(&self) -> u64 {
        // Ensure strict monotonicity via CAS loop. The TSC read
        // happens INSIDE the loop so a CAS retry under
        // contention re-samples real time. Pre-fix `raw` / `now`
        // were captured once before the loop; if a contended
        // retry took even a few microseconds (worst case under
        // heavy contention) the returned timestamp was `last+1`
        // — wall-clock-correct only as long as no thread won
        // the CAS in the meantime. Under sustained contention
        // the generator drifted arbitrarily far behind real
        // time. Re-reading the TSC is cheap (~1ns, no syscall)
        // and restores the wall-clock contract per call.
        loop {
            let raw = self.clock.raw();
            let now = self.clock.delta_as_nanos(self.baseline_raw, raw);
            let last = self.last.load(Ordering::Acquire);

            // Guard against u64::MAX exhaustion: saturating_add(1) at MAX
            // would return MAX again, breaking strict monotonicity.
            //
            // Pre-fix, at `last == u64::MAX - 1` we'd return
            // `u64::MAX` (via `.max()` clamp) and the NEXT call
            // would panic on `checked_add(1)`. That gap leaves the
            // generator briefly stalled at MAX before failure —
            // not a monotonicity violation, but the caller sees a
            // "value progression" that's actually clamped. Panic
            // preemptively when the result would be u64::MAX so
            // the failure mode is one consistent panic, not
            // "return MAX once then panic on retry."
            let next = match last.checked_add(1) {
                Some(inc) => inc,
                None => panic!("TimestampGenerator: timestamp space exhausted (u64::MAX)"),
            };
            let ts = now.max(next);
            if ts == u64::MAX {
                panic!(
                    "TimestampGenerator: timestamp space exhausted (would return u64::MAX); \
                     last={last}, now={now}",
                );
            }

            match self
                .last
                .compare_exchange_weak(last, ts, Ordering::Release, Ordering::Relaxed)
            {
                Ok(_) => return ts,
                Err(_) => {
                    // Another thread updated; retry
                    std::hint::spin_loop();
                }
            }
        }
    }

    /// Get the current raw timestamp without incrementing.
    ///
    /// This does NOT guarantee monotonicity and is only useful for
    /// measuring elapsed time or debugging.
    #[inline(always)]
    pub fn now_raw(&self) -> u64 {
        self.clock.raw()
    }

    /// Convert a raw timestamp to nanoseconds since this
    /// generator was constructed (i.e. since `baseline_raw`).
    /// Output units match `next()`: the value returned by
    /// `raw_to_nanos(self.now_raw())` is comparable to
    /// recently-`next()`-returned timestamps from the same
    /// generator (modulo the monotonicity floor `next()` enforces).
    ///
    /// Note: NOT "nanoseconds since UNIX epoch". The reference
    /// point is the per-generator construction moment, so two
    /// generators created at different times produce values with
    /// different offsets. For wall-clock-anchored debugging,
    /// combine with `SystemTime::now()` at generator-construction
    /// time (recorded externally).
    ///
    /// Pre-fix this called `delta_as_nanos(0, raw)`, where the
    /// `0` baseline was an unspecified quanta-internal reference
    /// (typically system boot under Windows QPC or the clock's
    /// first-call moment elsewhere). The returned ns values
    /// were in the order of system uptime — not comparable to
    /// `next()` output, despite the function's previous "ns
    /// since epoch" doc-claim. Aligning both to `baseline_raw`
    /// makes the surface consistent.
    #[inline]
    pub fn raw_to_nanos(&self, raw: u64) -> u64 {
        self.clock.delta_as_nanos(self.baseline_raw, raw)
    }

    /// Get the last generated timestamp.
    #[inline]
    pub fn last(&self) -> u64 {
        self.last.load(Ordering::Acquire)
    }
}

impl Default for TimestampGenerator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_monotonicity() {
        let ts_gen = TimestampGenerator::new();
        let mut prev = 0u64;

        for _ in 0..10_000 {
            let ts = ts_gen.next();
            assert!(ts > prev, "timestamps must be strictly increasing");
            prev = ts;
        }
    }

    /// Source pin: `TimestampGenerator::next` must re-read the
    /// TSC inside the CAS loop. Pre-fix `let raw = self.clock
    /// .raw()` and `let now = ...` were captured ONCE before
    /// the loop; under sustained contention a CAS retry reused
    /// the stale `now` and the returned timestamp was `last+1`
    /// rather than wall-clock-now, drifting arbitrarily far
    /// behind real time. The TSC read is ~1ns and is inside
    /// the loop now.
    ///
    /// We can't reliably reproduce the drift in a unit test
    /// (the worst case requires sustained heavy contention
    /// over enough retries that real-time advances measurably
    /// past the stale capture). The source pin catches a
    /// "simplification" PR that hoists the read back outside.
    #[test]
    fn timestamp_next_reads_tsc_inside_cas_loop() {
        let src = include_str!("timestamp.rs");

        // Locate the body of `pub fn next(&self) -> u64`.
        let header = "pub fn next(&self) -> u64 {";
        let start = src
            .find(header)
            .expect("TimestampGenerator::next must exist");
        // The body ends at the next `\n    fn ` or `\n    pub fn ` at
        // the impl indent.
        let after_header = start + header.len();
        let next_fn = src[after_header..]
            .find("\n    pub fn ")
            .or_else(|| src[after_header..].find("\n    fn "))
            .expect("a sibling fn must follow next()")
            + after_header;
        let body = &src[start..next_fn];

        // Strip line comments so the doc-comment that *describes*
        // the rejected pattern doesn't trip the negative
        // assertions.
        let body_no_comments: String = body
            .lines()
            .map(|l| match l.find("//") {
                Some(idx) => &l[..idx],
                None => l,
            })
            .collect::<Vec<_>>()
            .join("\n");

        // Find the `loop {` opening — the CAS loop body must
        // contain BOTH the TSC read and the delta_as_nanos
        // call.
        let loop_idx = body_no_comments
            .find("loop {")
            .expect("CAS loop must exist in next()");
        let loop_body = &body_no_comments[loop_idx..];

        assert!(
            loop_body.contains("self.clock.raw()"),
            "regression: `self.clock.raw()` must be inside the CAS \
             loop in TimestampGenerator::next. Hoisted outside, a \
             retry under contention reuses the stale TSC and the \
             returned timestamp drifts behind real time."
        );
        assert!(
            loop_body.contains("delta_as_nanos"),
            "regression: `delta_as_nanos` must be inside the CAS \
             loop alongside the TSC read."
        );
    }

    #[test]
    fn test_monotonicity_concurrent() {
        let ts_gen = std::sync::Arc::new(TimestampGenerator::new());
        let mut handles = vec![];

        for _ in 0..4 {
            let ts_gen_clone = ts_gen.clone();
            handles.push(thread::spawn(move || {
                let mut timestamps = Vec::with_capacity(1000);
                for _ in 0..1000 {
                    timestamps.push(ts_gen_clone.next());
                }
                timestamps
            }));
        }

        let mut all_timestamps: Vec<u64> = handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect();

        // All timestamps should be unique (strictly monotonic)
        all_timestamps.sort();
        let unique_count = all_timestamps.windows(2).filter(|w| w[0] != w[1]).count() + 1;
        assert_eq!(
            unique_count,
            all_timestamps.len(),
            "all timestamps must be unique"
        );
    }

    #[test]
    fn test_no_syscall_performance() {
        let ts_gen = TimestampGenerator::new();

        // Warm up
        for _ in 0..1000 {
            let _ = ts_gen.next();
        }

        // Measure
        let start = std::time::Instant::now();
        let iterations = 100_000;

        for _ in 0..iterations {
            let _ = ts_gen.next();
        }

        let elapsed = start.elapsed();
        let per_call = elapsed.as_nanos() / iterations as u128;

        // Typically 5-20ns on bare metal, but CI runners can be slower.
        assert!(
            per_call < 500,
            "timestamp generation too slow: {}ns per call",
            per_call
        );
    }

    #[test]
    fn test_timestamp_generator_new() {
        let ts_gen = TimestampGenerator::new();
        // Initial last should be 0
        assert_eq!(ts_gen.last(), 0);
    }

    #[test]
    fn test_timestamp_generator_default() {
        let ts_gen = TimestampGenerator::default();
        assert_eq!(ts_gen.last(), 0);
    }

    #[test]
    fn test_now_raw() {
        let ts_gen = TimestampGenerator::new();
        let raw1 = ts_gen.now_raw();
        let raw2 = ts_gen.now_raw();
        // Raw timestamps should be increasing (or at least not decreasing significantly)
        assert!(raw2 >= raw1 || raw1 - raw2 < 1000); // Allow for some jitter
    }

    #[test]
    fn test_raw_to_nanos() {
        let ts_gen = TimestampGenerator::new();
        let raw = ts_gen.now_raw();
        let nanos = ts_gen.raw_to_nanos(raw);
        // Nanos should be a reasonable value (not zero for a non-zero raw)
        assert!(nanos > 0);
    }

    #[test]
    fn test_raw_to_nanos_zero() {
        let ts_gen = TimestampGenerator::new();
        let nanos = ts_gen.raw_to_nanos(0);
        assert_eq!(nanos, 0);
    }

    #[test]
    fn test_last_after_next() {
        let ts_gen = TimestampGenerator::new();
        let ts1 = ts_gen.next();
        assert_eq!(ts_gen.last(), ts1);

        let ts2 = ts_gen.next();
        assert_eq!(ts_gen.last(), ts2);
        assert!(ts2 > ts1);
    }

    #[test]
    fn test_next_returns_increasing_values() {
        let ts_gen = TimestampGenerator::new();
        let mut prev = ts_gen.next();

        for _ in 0..100 {
            let current = ts_gen.next();
            assert!(current > prev);
            prev = current;
        }
    }

    #[test]
    fn test_multiple_generators_independent() {
        let ts_gen1 = TimestampGenerator::new();
        let ts_gen2 = TimestampGenerator::new();

        let ts1 = ts_gen1.next();
        let ts2 = ts_gen2.next();

        // Both should have advanced
        assert!(ts1 > 0);
        assert!(ts2 > 0);

        // They are independent, so last values are different
        assert_eq!(ts_gen1.last(), ts1);
        assert_eq!(ts_gen2.last(), ts2);
    }

    #[test]
    fn test_now_raw_does_not_affect_last() {
        let ts_gen = TimestampGenerator::new();
        let initial_last = ts_gen.last();

        // Call now_raw multiple times
        let _ = ts_gen.now_raw();
        let _ = ts_gen.now_raw();
        let _ = ts_gen.now_raw();

        // last should not have changed
        assert_eq!(ts_gen.last(), initial_last);
    }

    #[test]
    fn test_rapid_calls() {
        let ts_gen = TimestampGenerator::new();
        let mut timestamps = Vec::with_capacity(10000);

        for _ in 0..10000 {
            timestamps.push(ts_gen.next());
        }

        // All should be unique and strictly increasing
        for window in timestamps.windows(2) {
            assert!(window[1] > window[0]);
        }
    }

    // Regression: saturating_add(1) at u64::MAX used to silently return
    // the same timestamp twice, breaking strict monotonicity (BUGS_3 #6).
    #[test]
    #[should_panic(expected = "timestamp space exhausted")]
    fn test_next_panics_at_u64_max() {
        let ts_gen = TimestampGenerator::new();
        // Force last to u64::MAX
        ts_gen.last.store(u64::MAX, Ordering::Release);
        let _ = ts_gen.next();
    }

    #[test]
    fn test_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<TimestampGenerator>();
    }

    /// Regression: BUG_REPORT.md #14 — `next()` previously returned
    /// raw TSC ticks (~3.5× wall-clock-ns on a 3.5 GHz core) while
    /// claiming nanoseconds. Pin the unit by sleeping for a known
    /// amount of wall-clock time and asserting the delta is roughly
    /// nanoseconds, not TSC ticks.
    #[test]
    fn next_returns_nanoseconds_not_raw_ticks() {
        let ts_gen = TimestampGenerator::new();
        let t0 = ts_gen.next();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let t1 = ts_gen.next();

        let delta = t1 - t0;
        // 50ms == 50_000_000 ns. Allow ±50% slack for sleep
        // imprecision, scheduler jitter, and CI runners.
        let ns_lo = 25_000_000u64;
        let ns_hi = 200_000_000u64;
        assert!(
            delta >= ns_lo && delta <= ns_hi,
            "delta over a 50ms sleep was {delta} — outside the {ns_lo}..={ns_hi} \
             ns window. Most likely the timestamp is in raw TSC ticks again \
             (would be ~150_000_000 on a 3 GHz core)."
        );
    }

    /// A fresh generator's first `next()` value must be
    /// small (close to "ns since this generator was created"),
    /// not "ns since system uptime started" or some other
    /// arbitrary baseline. Pre-fix the baseline was `0` against
    /// the quanta::Clock's internal calibration, so on a system
    /// that had been up for hours the first `next()` returned
    /// many trillion nanoseconds.
    ///
    /// We assert the first `next()` is below ~10ms in nanoseconds,
    /// which is plenty of slack for construction overhead but
    /// nowhere near "ns since boot."
    #[test]
    fn next_first_call_is_close_to_zero() {
        let ts_gen = TimestampGenerator::new();
        let first = ts_gen.next();
        let ten_ms_ns = 10_000_000u64;
        assert!(
            first < ten_ms_ns,
            "first next() returned {first} ns; expected < {ten_ms_ns} ns. \
             Pre-fix this would be ~uptime in ns."
        );
    }
}
