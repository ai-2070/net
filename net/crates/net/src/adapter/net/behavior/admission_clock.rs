//! OA2-E0.4 of `docs/plans/OA2E_INTEGRATION_DESIGN.md` — a single
//! paired clock sample for provider admission.
//!
//! Admission checks two things against time: proof/credential
//! FRESHNESS (a wall clock — "is this cert/grant/proof still within
//! its validity window?") and replay RETENTION (a monotonic clock —
//! "how long must this `(caller, call_id)` stay in the guard?"). If
//! those read the wall clock at two DIFFERENT moments, a wall-clock
//! jump between them can immediately expire the replay entry for a
//! proof that just passed its freshness check (addendum §3).
//!
//! [`ClockSample`] captures BOTH clocks together, once, so every
//! per-admission decision derives from the same instant. E0 lands
//! this helper; the RULE that admission uses it (freshness reads
//! `wall_ns`, the replay deadline derives from the same sample) is
//! enforced when E1 wires the admission gate — this module has no
//! caller yet.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// A single paired capture of wall-clock and monotonic time.
///
/// Take one per admission with [`Self::now`], then use `wall_ns` for
/// every certificate/grant/proof freshness check and
/// [`Self::monotonic_deadline_for`] to translate the proof's
/// wall-clock expiry into the monotonic deadline the replay guard
/// retains it under — all relative to the SAME sample.
#[derive(Debug, Clone, Copy)]
pub struct ClockSample {
    /// Wall-clock now, unix NANOSECONDS. Every freshness check reads
    /// this — never a freshly-sampled clock.
    pub wall_ns: u64,
    /// Monotonic now, paired with `wall_ns`. Replay-retention
    /// deadlines derive from this so a wall-clock jump cannot shift
    /// retention out from under a freshness check.
    pub monotonic: Instant,
}

impl ClockSample {
    /// Capture both clocks together. A pre-epoch system clock
    /// saturates `wall_ns` to 0 (the same fail-safe the org module's
    /// `current_timestamp` uses); admission then treats every finite
    /// expiry as in the future, which is fine — the monotonic
    /// deadline still bounds retention.
    pub fn now() -> Self {
        let wall_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos().min(u64::MAX as u128) as u64)
            .unwrap_or(0);
        Self {
            wall_ns,
            monotonic: Instant::now(),
        }
    }

    /// The monotonic deadline corresponding to a wall-clock expiry
    /// (`wall_deadline_ns`, unix ns), derived RELATIVE to this one
    /// sample: `monotonic + max(0, wall_deadline_ns - wall_ns)`.
    ///
    /// A deadline already at or behind `wall_ns` yields
    /// `self.monotonic` (a zero-length retention — the proof is
    /// never held beyond its own life), so a proof that just failed
    /// freshness cannot be retained, and one that just passed is
    /// retained by exactly its remaining validity — consistent with
    /// the freshness check that used the same `wall_ns`.
    pub fn monotonic_deadline_for(&self, wall_deadline_ns: u64) -> Instant {
        let remaining_ns = wall_deadline_ns.saturating_sub(self.wall_ns);
        self.monotonic + Duration::from_nanos(remaining_ns)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_captures_both_clocks_coherently() {
        let before = Instant::now();
        let s = ClockSample::now();
        let after = Instant::now();
        assert!(s.monotonic >= before && s.monotonic <= after);
        // The wall sample is a plausible recent unix-ns value (past
        // 2020: 1.5e18 ns) unless the system clock is pre-epoch.
        assert!(s.wall_ns == 0 || s.wall_ns > 1_500_000_000_000_000_000);
    }

    #[test]
    fn future_deadline_is_that_far_ahead_on_the_monotonic_clock() {
        let s = ClockSample {
            wall_ns: 1_000_000_000_000_000_000,
            monotonic: Instant::now(),
        };
        // 30 s in the future (wall) → 30 s ahead of the sample's
        // monotonic instant.
        let deadline = s.monotonic_deadline_for(s.wall_ns + 30_000_000_000);
        assert_eq!(deadline, s.monotonic + Duration::from_secs(30));
    }

    #[test]
    fn past_or_equal_deadline_never_retains_beyond_the_sample() {
        let s = ClockSample {
            wall_ns: 1_000_000_000_000_000_000,
            monotonic: Instant::now(),
        };
        // Exactly now → zero retention.
        assert_eq!(s.monotonic_deadline_for(s.wall_ns), s.monotonic);
        // Already in the past → still clamped to the sample instant
        // (saturating), never before it.
        assert_eq!(
            s.monotonic_deadline_for(s.wall_ns - 5_000_000_000),
            s.monotonic
        );
    }

    #[test]
    fn one_sample_yields_consistent_deadlines() {
        // Two deadlines derived from ONE sample are ordered by their
        // wall-clock expiries and both anchored to the same monotonic
        // instant — no second wall read can perturb them.
        let s = ClockSample::now();
        let near = s.monotonic_deadline_for(s.wall_ns + 10_000_000_000);
        let far = s.monotonic_deadline_for(s.wall_ns + 20_000_000_000);
        assert!(far > near);
        assert_eq!(far - near, Duration::from_secs(10));
    }
}
