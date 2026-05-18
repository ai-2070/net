//! Bandwidth budget — `docs/plans/REDEX_DISTRIBUTED_PLAN.md` §5 +
//! Locked decision 10.
//!
//! Token-bucket rate limiter the catch-up loop consults before
//! shipping a [`SyncResponse`] chunk. Configured per channel via
//! `ReplicationConfig::replication_budget_fraction` (default 0.5)
//! and the operator's measured NIC peak; refills at the configured
//! per-second rate and caps the burst at one second's worth of
//! tokens (so a long idle period doesn't accumulate an unbounded
//! credit that lets the next round saturate the link).
//!
//! Pure logic — caller passes `Instant::now()` so DST and unit
//! tests can advance time deterministically. The eventual tokio
//! interval-driven catch-up loop calls `try_consume(bytes,
//! Instant::now())` before assembling each chunk; on `false` it
//! defers the chunk to the next tick.
//!
//! Backpressure-aware: the reliable-stream layer already throttles
//! based on the receiver's flow-control window; this budget enforces
//! the *sender's* outbound cap so a single channel's catch-up can't
//! starve foreground publish traffic. Both mechanisms compose:
//! whichever is more restrictive at the moment wins.

use std::time::Instant;

/// Token-bucket rate limiter scaled to a fraction of measured NIC
/// peak. Caller mutates via [`Self::try_consume`]; refill is
/// time-driven (passing the current `Instant` each call).
///
/// Burst capacity = one second of tokens. A long idle period
/// doesn't accumulate unbounded credit — the bucket caps at the
/// per-second rate so the next active second is bounded. The plan
/// §5 prefers steady-state throttling over burst absorption; this
/// matches.
#[derive(Debug, Clone)]
pub struct BandwidthBudget {
    /// Tokens currently available, in bytes.
    available_bytes: f64,
    /// Refill rate in bytes per second. Computed at construction
    /// from `nic_peak_bps × fraction`.
    refill_bps: f64,
    /// Bucket capacity in bytes — equal to `refill_bps` so the
    /// burst is bounded at one second's worth of tokens.
    capacity_bytes: f64,
    /// Last time we refilled the bucket. Caller-supplied `Instant`
    /// drives this; no system-clock reads inside the limiter.
    last_refill: Instant,
}

impl BandwidthBudget {
    /// Construct a budget limiter scaled to `fraction × nic_peak_bps`.
    ///
    /// - `fraction` is clamped to `(0.0, 1.0]` (a fraction of zero
    ///   or negative would make the bucket never refill; > 1.0
    ///   would let the channel exceed the measured NIC peak,
    ///   which is meaningless).
    /// - `nic_peak_bps` is the operator's measured per-link peak
    ///   in bytes per second.
    /// - `now` seeds the `last_refill` timestamp; the bucket
    ///   starts full so the first call to `try_consume` succeeds
    ///   up to the capacity.
    pub fn new(fraction: f32, nic_peak_bps: u64, now: Instant) -> Self {
        // Clamp the fraction; the [`ReplicationConfig`] validator
        // already enforces this, but landing it here too keeps
        // unit tests + DST scenarios from constructing a
        // pathological limiter.
        let clamped = if !fraction.is_finite() || fraction <= 0.0 {
            // Lowest non-zero value — keeps the bucket refilling
            // at a glacial pace rather than producing div-by-zero.
            // The config validator rejects this shape before
            // construction in production.
            f32::EPSILON
        } else if fraction > 1.0 {
            1.0
        } else {
            fraction
        };
        let refill_bps = nic_peak_bps as f64 * clamped as f64;
        // Burst capacity caps at one second of tokens. Plan §5:
        // "Per-request chunk_max bounds memory footprint of any
        // single sync exchange" — burst-bucket size honors that.
        let capacity_bytes = refill_bps;
        Self {
            available_bytes: capacity_bytes,
            refill_bps,
            capacity_bytes,
            last_refill: now,
        }
    }

    /// Refill the bucket given `now`. Called internally by
    /// [`Self::try_consume`]; exposed so the eventual heartbeat
    /// loop can pre-refill before consulting the budget for a
    /// multi-chunk decision.
    pub fn refill(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.last_refill);
        if elapsed.is_zero() {
            return;
        }
        let added = self.refill_bps * elapsed.as_secs_f64();
        self.available_bytes = (self.available_bytes + added).min(self.capacity_bytes);
        self.last_refill = now;
    }

    /// Try to consume `bytes` from the bucket. Returns `true` on
    /// success (tokens deducted); `false` on insufficient credit
    /// (state unchanged — caller defers and retries on the next
    /// tick after [`Self::refill`] runs again).
    ///
    /// `bytes == 0` always succeeds without state mutation.
    ///
    /// **Oversize request behavior.** Requests larger than
    /// [`Self::capacity_bytes`] (= one second's worth of refill)
    /// can never accumulate enough credit on their own — the
    /// bucket caps at capacity even after infinite refill. The catch-up
    /// path is responsible for splitting outbound chunks against
    /// this ceiling; if a single event is itself larger than the
    /// budget capacity (rare — e.g. a tiny channel with a large
    /// payload), the call admits it as a one-off, draining the
    /// bucket fully so subsequent requests defer until refill
    /// catches up. Without this clamp the channel would deadlock
    /// trying to send a single event it can never afford.
    pub fn try_consume(&mut self, bytes: u64, now: Instant) -> bool {
        if bytes == 0 {
            return true;
        }
        self.refill(now);
        let cost = bytes as f64;
        if self.available_bytes >= cost {
            self.available_bytes -= cost;
            return true;
        }
        // Oversize-event escape hatch: if the request itself is
        // larger than capacity AND the bucket is at full credit,
        // admit it once and drain. This prevents a per-channel
        // deadlock when a single event exceeds one-second's
        // refill — coordinator-side chunk splitting is preferred,
        // but the budget should never be the reason an event is
        // permanently un-shippable.
        if bytes > self.capacity_bytes as u64
            && self.available_bytes >= self.capacity_bytes - f64::EPSILON
        {
            self.available_bytes = 0.0;
            return true;
        }
        false
    }

    /// Return previously-consumed `bytes` to the bucket. Called
    /// when a wire send fails after `try_consume` already deducted
    /// the cost — otherwise repeated send failures over a flaky
    /// link would drift the budget toward permanent backpressure
    /// without shipping any traffic. Idempotent saturation: the
    /// returned tokens never exceed `capacity_bytes`.
    ///
    /// `bytes == 0` is a no-op.
    pub fn refund(&mut self, bytes: u64) {
        if bytes == 0 {
            return;
        }
        self.available_bytes = (self.available_bytes + bytes as f64).min(self.capacity_bytes);
    }

    /// Current available token count in bytes. Useful for
    /// observability — operators can graph "how much catch-up
    /// budget is unused?" to spot under-utilized links.
    pub fn available_bytes(&self) -> u64 {
        // Saturating-floor at zero — tokens can technically dip
        // a tick below zero via floating-point rounding; we
        // surface the user-facing accumulator clamped.
        self.available_bytes.max(0.0).floor() as u64
    }

    /// Configured refill rate in bytes/sec.
    pub fn refill_bps(&self) -> u64 {
        self.refill_bps as u64
    }

    /// Configured capacity (= refill_bps; one second's burst).
    pub fn capacity_bytes(&self) -> u64 {
        self.capacity_bytes as u64
    }

    /// Update the budget's NIC peak measurement. The proximity
    /// graph's per-link throughput counters update on a 60 s
    /// rolling window; this method lets the coordinator update
    /// without reconstructing the limiter (which would clear the
    /// current token balance).
    pub fn set_nic_peak(&mut self, nic_peak_bps: u64, fraction: f32, now: Instant) {
        // Refill before re-scaling so the existing balance maps
        // to the new capacity correctly.
        self.refill(now);
        let clamped = if !fraction.is_finite() || fraction <= 0.0 {
            f32::EPSILON
        } else if fraction > 1.0 {
            1.0
        } else {
            fraction
        };
        let new_refill = nic_peak_bps as f64 * clamped as f64;
        // Preserve the proportion of fill — a half-full bucket
        // stays half-full after the re-scale.
        let prev_proportion = if self.capacity_bytes > 0.0 {
            self.available_bytes / self.capacity_bytes
        } else {
            1.0
        };
        self.refill_bps = new_refill;
        self.capacity_bytes = new_refill;
        self.available_bytes = (new_refill * prev_proportion).min(new_refill);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn t0() -> Instant {
        Instant::now()
    }

    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    #[test]
    fn bucket_starts_full_at_capacity() {
        // 1 MB/s budget = 1_048_576 bytes/sec.
        let base = t0();
        let bb = BandwidthBudget::new(0.5, 2 * 1024 * 1024, base);
        // 0.5 × 2 MiB/s = 1 MiB/s.
        assert_eq!(bb.refill_bps(), 1024 * 1024);
        assert_eq!(bb.capacity_bytes(), 1024 * 1024);
        assert_eq!(bb.available_bytes(), 1024 * 1024);
    }

    #[test]
    fn try_consume_succeeds_within_capacity() {
        let base = t0();
        let mut bb = BandwidthBudget::new(1.0, 1_000_000, base);
        assert!(bb.try_consume(500_000, base));
        // ~500_000 bytes left (slack for f64 rounding).
        assert!(bb.available_bytes() >= 499_999);
        assert!(bb.available_bytes() <= 500_001);
    }

    #[test]
    fn try_consume_fails_when_empty() {
        let base = t0();
        let mut bb = BandwidthBudget::new(1.0, 100, base);
        // Drain the entire 100-byte capacity.
        assert!(bb.try_consume(100, base));
        // Subsequent consume within the same instant fails.
        assert!(!bb.try_consume(1, base));
    }

    #[test]
    fn refill_restores_tokens_over_time() {
        let base = t0();
        let mut bb = BandwidthBudget::new(1.0, 1_000, base); // 1 KB/s
        bb.try_consume(1_000, base); // drain
        assert_eq!(bb.available_bytes(), 0);
        // 500 ms elapsed → 500 bytes refilled.
        bb.refill(at(base, 500));
        let avail = bb.available_bytes();
        assert!(
            (499..=500).contains(&avail),
            "expected ~500 bytes refilled, got {avail}",
        );
    }

    #[test]
    fn refill_caps_at_capacity_not_unbounded() {
        let base = t0();
        let mut bb = BandwidthBudget::new(1.0, 1_000, base);
        // 10 seconds idle — would refill 10 KB if unbounded;
        // capped at 1 KB (one second's worth).
        bb.refill(at(base, 10_000));
        assert_eq!(bb.available_bytes(), 1_000);
    }

    #[test]
    fn zero_byte_consume_always_succeeds() {
        let base = t0();
        let mut bb = BandwidthBudget::new(1.0, 1_000, base);
        bb.try_consume(1_000, base); // drain
        assert!(bb.try_consume(0, base));
        assert_eq!(bb.available_bytes(), 0); // no spurious refill
    }

    #[test]
    fn fraction_above_one_clamped() {
        let base = t0();
        let bb = BandwidthBudget::new(2.0, 1_000_000, base);
        // Clamped at 1.0 — refill = full NIC peak, not 2×.
        assert_eq!(bb.refill_bps(), 1_000_000);
    }

    #[test]
    fn fraction_zero_falls_back_to_epsilon() {
        let base = t0();
        let bb = BandwidthBudget::new(0.0, 1_000_000_000, base);
        // Epsilon × 1 GB/s = ~119 B/s. Lock the floor: bucket
        // does refill (slowly) rather than being permanently empty.
        assert!(bb.refill_bps() > 0);
    }

    #[test]
    fn fraction_nan_falls_back_to_epsilon() {
        let base = t0();
        let bb = BandwidthBudget::new(f32::NAN, 1_000_000_000, base);
        assert!(bb.refill_bps() > 0);
    }

    #[test]
    fn fraction_neg_inf_falls_back_to_epsilon() {
        let base = t0();
        let bb = BandwidthBudget::new(f32::NEG_INFINITY, 1_000_000_000, base);
        assert!(bb.refill_bps() > 0);
    }

    #[test]
    fn partial_consume_then_refill_then_consume() {
        let base = t0();
        let mut bb = BandwidthBudget::new(1.0, 1_000, base);
        bb.try_consume(600, base); // 400 left
        bb.refill(at(base, 500)); // +500 → capped at 1000? actually 900
                                  // wait: 400 + (500 ms × 1000/s) = 400 + 500 = 900
        let avail = bb.available_bytes();
        assert!((899..=900).contains(&avail), "got {avail}");
        // Consuming the remainder.
        assert!(bb.try_consume(900, at(base, 500)));
    }

    #[test]
    fn try_consume_oversize_with_full_bucket_admits_as_one_off() {
        let base = t0();
        let mut bb = BandwidthBudget::new(1.0, 1_000, base);
        // Full bucket + a single chunk larger than capacity. The
        // budget can't accumulate enough credit even after
        // infinite refill (capacity caps at one second's tokens),
        // so the only choice is admit-once-and-drain. Otherwise
        // the channel deadlocks trying to ship an event that's
        // too large for the configured budget.
        assert!(bb.try_consume(2_000, base));
        // Bucket fully drained; subsequent normal-sized requests
        // defer until refill.
        assert!(!bb.try_consume(1, base));
    }

    #[test]
    fn try_consume_oversize_with_partial_bucket_fails() {
        let base = t0();
        let mut bb = BandwidthBudget::new(1.0, 1_000, base);
        // Drain half the bucket via a normal-sized request.
        assert!(bb.try_consume(500, base));
        // Oversize chunk now arrives. Bucket isn't at full credit
        // anymore, so the escape hatch doesn't fire; caller defers
        // until refill catches up.
        assert!(!bb.try_consume(2_000, base));
        // State preserved on failure — half the bucket is still
        // there for the caller to consume with a smaller request.
        let remaining = bb.available_bytes();
        assert!((499..=501).contains(&remaining));
    }

    #[test]
    fn set_nic_peak_preserves_fill_proportion() {
        let base = t0();
        let mut bb = BandwidthBudget::new(1.0, 1_000, base);
        bb.try_consume(500, base); // half full
        let before = bb.available_bytes();
        assert!((499..=501).contains(&before));
        // NIC peak doubles; half-full stays half-full of the
        // new capacity.
        bb.set_nic_peak(2_000, 1.0, base);
        assert_eq!(bb.capacity_bytes(), 2_000);
        let after = bb.available_bytes();
        assert!(
            (999..=1_000).contains(&after),
            "expected ~half of 2_000 = 1_000; got {after}",
        );
    }

    #[test]
    fn set_nic_peak_to_smaller_caps_at_new_capacity() {
        let base = t0();
        let mut bb = BandwidthBudget::new(1.0, 10_000, base);
        // Full bucket at 10 KB.
        // Shrink to 1 KB peak — bucket must cap at the new
        // capacity, not retain the old 10 KB.
        bb.set_nic_peak(1_000, 1.0, base);
        assert_eq!(bb.capacity_bytes(), 1_000);
        assert!(bb.available_bytes() <= 1_000);
    }

    #[test]
    fn refill_with_zero_elapsed_is_noop() {
        let base = t0();
        let mut bb = BandwidthBudget::new(1.0, 1_000, base);
        bb.try_consume(500, base);
        let before = bb.available_bytes();
        bb.refill(base); // same instant
        assert_eq!(bb.available_bytes(), before);
    }
}
