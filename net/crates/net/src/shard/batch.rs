//! Batch aggregation with adaptive sizing.
//!
//! The batch worker continuously drains events from a shard's ring buffer,
//! assembles them into batches, and dispatches them to the adapter.
//!
//! # Adaptive Sizing
//!
//! Batch size is dynamically adjusted based on ingestion velocity:
//! - High velocity → larger batches → fewer adapter calls → higher throughput
//! - Low velocity → smaller batches → lower latency → faster flush

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config::BatchConfig;
use crate::event::{Batch, InternalEvent};

/// Cap on `AdaptiveBatcher::velocity_samples` to bound per-shard
/// memory use under high throughput. `calculate_velocity` reads
/// only `front()` and `back()`, so additional samples in between
/// are pure overhead. 1 024 entries × ~24 bytes per tuple
/// ≈ 24 KiB per shard — well below the 240 KiB pre-fix worst
/// case at 100 k events/s × 100 ms `velocity_window`.
const VELOCITY_SAMPLES_CAP: usize = 1024;

/// Adaptive batch size calculator.
///
/// Tracks recent ingestion velocity and adjusts batch size accordingly.
pub struct AdaptiveBatcher {
    /// Configuration.
    config: BatchConfig,
    /// Current target batch size.
    current_batch_size: usize,
    /// Velocity samples: (timestamp, cumulative_count).
    velocity_samples: VecDeque<(Instant, u64)>,
    /// Total events seen (for velocity calculation).
    total_events: u64,
    /// Last recalculation time.
    last_recalc: Instant,
}

impl AdaptiveBatcher {
    /// Create a new adaptive batcher.
    pub fn new(config: BatchConfig) -> Self {
        Self {
            current_batch_size: config.min_size,
            velocity_samples: VecDeque::with_capacity(100),
            total_events: 0,
            last_recalc: Instant::now(),
            config,
        }
    }

    /// Record events and get the current target batch size.
    ///
    /// Call this each time events are drained from the ring buffer.
    #[inline]
    pub fn record_events(&mut self, count: usize) -> usize {
        // Saturating-add: a stream that's ingested ~2^64 events
        // is already in trouble, but a wrap from `u64::MAX` to a
        // small value would interact with the
        // `saturating_sub(oldest_count)` in `calculate_velocity`
        // — the saturating-sub would underflow to 0 across the
        // wraparound boundary and `velocity` would collapse to 0,
        // forcing the batcher to its `min_size` floor right when
        // sustained high throughput is exactly what the adaptive
        // path was meant to handle. Saturating instead clamps at
        // `u64::MAX` and `newest - oldest = 0` is the documented
        // stop state.
        self.total_events = self.total_events.saturating_add(count as u64);

        if !self.config.adaptive {
            return self.config.max_size;
        }

        let now = Instant::now();

        // Add sample
        self.velocity_samples.push_back((now, self.total_events));

        // Remove old samples outside the time window.
        //
        // `Instant - Duration` panics on underflow, and on Windows
        // `Instant` is QPC-relative to boot — a process that
        // starts within `velocity_window` (typically a few
        // seconds) of boot would abort the batch worker task
        // here. `checked_sub` returns `None` on underflow; in
        // that case skip the time-based eviction (every existing
        // sample is "newer than the window floor" by definition,
        // since the floor predates `Instant::now()`'s zero point).
        // The sample-count cap below still bounds memory.
        if let Some(window_start) = now.checked_sub(self.config.velocity_window) {
            while let Some(&(ts, _)) = self.velocity_samples.front() {
                if ts < window_start {
                    self.velocity_samples.pop_front();
                } else {
                    break;
                }
            }
        }

        // Also cap the deque by sample COUNT. Pre-fix the
        // bound was time-only, so at 100k events/s with a 100 ms
        // velocity_window the deque could grow to ~10 000 entries
        // before time-eviction caught up, costing ~240 KiB per
        // shard for samples never used (calculate_velocity reads
        // only `front()` and `back()`). Cap at
        // VELOCITY_SAMPLES_CAP so the memory footprint is bounded
        // regardless of throughput.
        while self.velocity_samples.len() > VELOCITY_SAMPLES_CAP {
            self.velocity_samples.pop_front();
        }

        // Recalculate batch size periodically (not on every call)
        if now.duration_since(self.last_recalc) > Duration::from_millis(10) {
            self.recalculate_batch_size();
            self.last_recalc = now;
        }

        self.current_batch_size
    }

    /// Get the current target batch size.
    #[inline]
    pub fn batch_size(&self) -> usize {
        self.current_batch_size
    }

    /// Calculate events per second based on recent samples.
    fn calculate_velocity(&self) -> f64 {
        if self.velocity_samples.len() < 2 {
            return 0.0;
        }

        let (oldest_ts, oldest_count) = *self.velocity_samples.front().unwrap();
        let (newest_ts, newest_count) = *self.velocity_samples.back().unwrap();

        let elapsed = newest_ts.duration_since(oldest_ts);
        if elapsed.is_zero() {
            return 0.0;
        }

        let events = newest_count.saturating_sub(oldest_count);
        events as f64 / elapsed.as_secs_f64()
    }

    /// Recalculate the optimal batch size based on recent velocity.
    fn recalculate_batch_size(&mut self) {
        let velocity = self.calculate_velocity();

        // Scale batch size with velocity
        // At 1M events/sec → batch size ~5,000
        // At 10M events/sec → batch size ~50,000 (capped at max)
        //
        // Explicit `clamp(0.0, usize::MAX as f64)` before the `as
        // usize` cast: Rust's `as` cast on f64 → usize is
        // saturating in current versions, but the explicit clamp
        // documents intent and survives any future edition that
        // tightens the cast (e.g. requires `try_from` on
        // overflow). The `velocity > 0.0` guard above already
        // rules out NaN and negative; the upper bound here only
        // matters for the unreachable `velocity > usize::MAX *
        // 200.0` case (~3.7e21 events/sec), but the saturation
        // is cheaper than reasoning about future cast semantics.
        let target = if velocity > 0.0 {
            let scaled = (velocity / 200.0).clamp(0.0, usize::MAX as f64);
            (scaled as usize).clamp(self.config.min_size, self.config.max_size)
        } else {
            self.config.min_size
        };

        // Smooth transitions using exponential moving average
        // new = (old * 3 + target) / 4
        //
        // Saturating: `BatchConfig::validate` doesn't bound
        // `max_size` from above, so a hostile config that pushes
        // `current_batch_size` near `usize::MAX / 3` would
        // overflow the multiply (debug: panic; release: wrap to a
        // tiny value, collapsing the batcher to its `min_size`
        // floor on the next clamp). Saturating preserves the
        // intent — clamp at `usize::MAX` and let the bounds
        // clamp below pull it back into the configured window.
        self.current_batch_size = self
            .current_batch_size
            .saturating_mul(3)
            .saturating_add(target)
            / 4;

        // Ensure we stay within bounds
        self.current_batch_size = self
            .current_batch_size
            .clamp(self.config.min_size, self.config.max_size);
    }

    /// Reset the batcher state.
    pub fn reset(&mut self) {
        self.velocity_samples.clear();
        self.total_events = 0;
        self.current_batch_size = self.config.min_size;
        self.last_recalc = Instant::now();
    }
}

/// Batch worker state.
///
/// Manages batch assembly for a single shard.
pub struct BatchWorker {
    /// Shard ID.
    shard_id: u16,
    /// Adaptive batcher.
    batcher: AdaptiveBatcher,
    /// Current batch being assembled.
    current_batch: Vec<InternalEvent>,
    /// Sequence number for the next batch.
    next_sequence: u64,
    /// Mirror of `next_sequence` published to the bus, so
    /// `EventBus::remove_shard_internal` can read the worker's
    /// final post-flush sequence after awaiting the worker's
    /// `JoinHandle`. Used as the `sequence_start` for the
    /// stranded-ring-buffer flush so its msg-ids don't collide
    /// with the worker's own first batch under JetStream's dedup
    /// window.
    ///
    /// Updated on every successful `flush`. The hot path pays one
    /// release-ordered atomic store per dispatched batch — the
    /// per-batch dispatch already crosses an `await` so the
    /// extra store is amortized away.
    next_sequence_published: Arc<AtomicU64>,
    /// Producer nonce stamped on every produced `Batch`.
    ///
    /// When the bus is configured with `producer_nonce_path`, this
    /// is the persisted u64 from
    /// `adapter::PersistentProducerNonce::load_or_create`. When
    /// not configured, it falls back to the per-process nonce
    /// from `event::batch_process_nonce`. Adapters that key dedup
    /// on `(producer_nonce, shard, sequence_start, i)` (today:
    /// JetStream `Nats-Msg-Id`, Redis `dedup_id` field) use this
    /// to recognize cross-process retries.
    producer_nonce: u64,
    /// Time when the current batch started.
    batch_start: Option<Instant>,
    /// Configuration.
    config: BatchConfig,
}

impl BatchWorker {
    /// Create a new batch worker.
    ///
    /// `next_sequence_published` is the bus-owned mirror of
    /// `next_sequence`. Pass `Arc::new(AtomicU64::new(0))` if the
    /// caller doesn't need to observe the post-exit sequence;
    /// production paths share it with `bus::remove_shard_internal`.
    ///
    /// `producer_nonce` is stamped on every produced `Batch` for
    /// cross-process dedup. The bus passes its loaded nonce in;
    /// tests can use any u64 (typically 0 or the per-process
    /// default).
    pub fn new(
        shard_id: u16,
        config: BatchConfig,
        next_sequence_published: Arc<AtomicU64>,
        producer_nonce: u64,
    ) -> Self {
        let capacity = config.max_size;
        Self {
            shard_id,
            batcher: AdaptiveBatcher::new(config.clone()),
            current_batch: Vec::with_capacity(capacity),
            next_sequence: 0,
            next_sequence_published,
            producer_nonce,
            batch_start: None,
            config,
        }
    }

    /// Add events to the current batch.
    ///
    /// Returns a completed batch if thresholds are met, or None if more events are needed.
    ///
    /// # Empty-input side effect
    ///
    /// Passing an empty `events` vec is **not** a no-op. The
    /// BatchWorker's recv-timeout arm calls `add_events(vec![])`
    /// specifically to drive a `check_timeout` round, which may
    /// flush the in-memory `current_batch` if `max_delay` has
    /// elapsed since the last event arrived. Callers who want
    /// "true no-op on empty input" must check `events.is_empty()`
    /// themselves before calling.
    ///
    /// Pre-fix this side effect was not documented and
    /// surprised callers expecting `add_events([])` to be inert.
    /// The fix is documentation only — the BatchWorker's timeout
    /// flush relies on this behavior, so removing the side effect
    /// would break the timeout-flush mechanism in bus.rs.
    pub fn add_events(&mut self, events: Vec<InternalEvent>) -> Option<Batch> {
        if events.is_empty() {
            return self.check_timeout();
        }

        // Start batch timer if this is the first event
        if self.current_batch.is_empty() {
            self.batch_start = Some(Instant::now());
        }

        // Record events and get target batch size
        let target_size = self.batcher.record_events(events.len());

        // Add events to current batch
        self.current_batch.extend(events);

        // Check if we should flush
        if self.current_batch.len() >= target_size {
            return Some(self.flush());
        }

        // Check timeout
        self.check_timeout()
    }

    /// Check if the batch should be flushed due to timeout.
    fn check_timeout(&mut self) -> Option<Batch> {
        if self.current_batch.is_empty() {
            return None;
        }

        if let Some(start) = self.batch_start {
            if start.elapsed() >= self.config.max_delay {
                return Some(self.flush());
            }
        }

        None
    }

    /// Force flush the current batch, even if thresholds aren't met.
    pub fn flush(&mut self) -> Batch {
        let events = std::mem::replace(
            &mut self.current_batch,
            Vec::with_capacity(self.config.max_size),
        );

        let sequence_start = self.next_sequence;
        // `saturating_add` rather than `+=`: at u64 granularity this can
        // only happen after wraparound (~584 years at 1 B events/s), but
        // the wrap would silently corrupt sequence numbering. Saturating
        // pins the counter at u64::MAX so downstream consumers see a
        // monotonic, observable terminal state instead.
        self.next_sequence = self.next_sequence.saturating_add(events.len() as u64);
        // Publish the post-flush counter to the bus-owned mirror.
        // `bus::remove_shard_internal` reads this after awaiting the
        // worker's `JoinHandle` and uses it as the
        // `sequence_start` for the stranded-ring-buffer flush — that
        // guarantees the stranded msg-ids fall strictly past every
        // msg-id this worker emitted, closing the JetStream-dedup
        // collision risk.
        self.next_sequence_published
            .store(self.next_sequence, Ordering::Release);
        self.batch_start = None;

        Batch::with_nonce(self.shard_id, events, sequence_start, self.producer_nonce)
    }

    /// Check if there are pending events.
    pub fn has_pending(&self) -> bool {
        !self.current_batch.is_empty()
    }

    /// Get the number of pending events.
    pub fn pending_count(&self) -> usize {
        self.current_batch.len()
    }

    /// Get the current target batch size.
    pub fn target_batch_size(&self) -> usize {
        self.batcher.batch_size()
    }

    /// Get time until the current batch times out.
    pub fn time_until_timeout(&self) -> Option<Duration> {
        self.batch_start.map(|start| {
            let elapsed = start.elapsed();
            self.config.max_delay.saturating_sub(elapsed)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_events(count: usize, shard_id: u16) -> Vec<InternalEvent> {
        (0..count)
            .map(|i| InternalEvent::from_value(json!({"i": i}), i as u64, shard_id))
            .collect()
    }

    /// Test helper — most tests don't observe the published sequence,
    /// they just need the third arg.
    fn fresh_published() -> Arc<AtomicU64> {
        Arc::new(AtomicU64::new(0))
    }

    #[test]
    fn test_batch_size_threshold() {
        let config = BatchConfig {
            min_size: 10,
            max_size: 100,
            max_delay: Duration::from_secs(10),
            adaptive: false,
            velocity_window: Duration::from_millis(100),
        };

        let mut worker = BatchWorker::new(0, config, fresh_published(), 0);

        // Add 50 events - should not trigger flush (target is 100 when adaptive=false)
        let batch = worker.add_events(make_events(50, 0));
        assert!(batch.is_none());
        assert_eq!(worker.pending_count(), 50);

        // Add 50 more - should trigger flush
        let batch = worker.add_events(make_events(50, 0));
        assert!(batch.is_some());
        let batch = batch.unwrap();
        assert_eq!(batch.events.len(), 100);
        assert_eq!(batch.shard_id, 0);
    }

    /// `add_events(vec![])` is **not** a no-op. The activate-failure
    /// rollback path in `bus.rs` and the BatchWorker's recv-timeout
    /// arm both rely on the empty-input call to drive a
    /// `check_timeout`, which can flush `current_batch` if
    /// `max_delay` has elapsed. A future refactor that makes
    /// `add_events([])` a true no-op would silently lose those
    /// already-batched events on the rollback path. Pin the
    /// load-bearing behavior here so any such "cleanup" trips a
    /// failing test rather than producing a silent regression.
    #[test]
    fn add_events_empty_can_flush_via_timeout() {
        let config = BatchConfig {
            min_size: 10,
            max_size: 1000,
            max_delay: Duration::from_millis(1),
            adaptive: false,
            velocity_window: Duration::from_millis(100),
        };
        let mut worker = BatchWorker::new(0, config, fresh_published(), 0);

        // Stage some events well below `min_size` so neither size
        // threshold can hide the timeout-flush.
        let pre = worker.add_events(make_events(3, 0));
        assert!(pre.is_none(), "below min_size — no flush yet");

        // Empty input *before* max_delay must be a no-op (returns
        // None). This pins the second half of the contract: the
        // side-effect is bounded to "check timeout", not "always
        // flush".
        let early = worker.add_events(vec![]);
        assert!(
            early.is_none(),
            "empty input before max_delay must NOT flush — \
             check_timeout returns None when start.elapsed() < max_delay"
        );

        // Wait past max_delay and call with empty input — must flush.
        std::thread::sleep(Duration::from_millis(5));
        let flushed = worker.add_events(vec![]);
        assert!(
            flushed.is_some(),
            "empty input after max_delay MUST flush via check_timeout — \
             this is the contract bus.rs and BatchWorker's recv-timeout \
             arm rely on; making it a no-op silently loses events on \
             the activate-failure rollback path"
        );
        assert_eq!(flushed.unwrap().events.len(), 3);
    }

    #[test]
    fn test_batch_timeout() {
        let config = BatchConfig {
            min_size: 10,
            max_size: 1000,
            max_delay: Duration::from_millis(1),
            adaptive: false,
            velocity_window: Duration::from_millis(100),
        };

        let mut worker = BatchWorker::new(0, config, fresh_published(), 0);

        // Add some events
        let batch = worker.add_events(make_events(5, 0));
        assert!(batch.is_none());

        // Wait for timeout
        std::thread::sleep(Duration::from_millis(5));

        // Check timeout triggers flush
        let batch = worker.add_events(vec![]);
        assert!(batch.is_some());
        assert_eq!(batch.unwrap().events.len(), 5);
    }

    #[test]
    fn test_adaptive_batch_sizing() {
        let config = BatchConfig {
            min_size: 100,
            max_size: 10_000,
            max_delay: Duration::from_secs(10),
            adaptive: true,
            velocity_window: Duration::from_millis(100),
        };

        let mut batcher = AdaptiveBatcher::new(config);

        // Initially should be at min_size
        assert_eq!(batcher.batch_size(), 100);

        // Simulate high velocity (add lots of events quickly)
        for _ in 0..100 {
            batcher.record_events(10_000);
            std::thread::sleep(Duration::from_micros(100));
        }

        // Batch size should have increased
        assert!(batcher.batch_size() > 100);
    }

    /// Regression: `recalculate_batch_size` previously did
    /// `current_batch_size * 3 + target` with bare arithmetic. A
    /// hostile `BatchConfig` with `max_size` near `usize::MAX / 3`
    /// could push `current_batch_size` near that threshold, where
    /// the multiply overflows — debug build panics, release wraps
    /// to a tiny value. The fix saturates both the multiply and
    /// add. Pin the saturation so a future revert ("simplify" the
    /// arithmetic) is caught by the test rather than discovered
    /// in production via a debug-build crash.
    #[test]
    fn recalculate_batch_size_saturates_on_hostile_max_size() {
        let config = BatchConfig {
            min_size: 1,
            max_size: usize::MAX,
            max_delay: Duration::from_secs(10),
            adaptive: true,
            velocity_window: Duration::from_millis(100),
        };
        let mut batcher = AdaptiveBatcher::new(config);

        // Drive `current_batch_size` to a value where `* 3` would
        // overflow. The field is module-private but we're in the
        // same module, so direct mutation is fine.
        batcher.current_batch_size = usize::MAX - 1;

        // Pre-fix this would either debug-panic (`overflow when
        // multiplying`) or release-wrap to a small value.
        // Post-fix: saturating_mul keeps the result at usize::MAX
        // and the bounds clamp pulls it back into [min_size,
        // max_size]. Either way, no panic and no wrap to tiny.
        batcher.recalculate_batch_size();

        // Sanity: the resulting size is still inside the
        // configured window and didn't wrap to a small value.
        assert!(
            batcher.current_batch_size >= 1,
            "post-recalc batch size must respect min_size, got {}",
            batcher.current_batch_size,
        );
    }

    #[test]
    fn test_force_flush() {
        let config = BatchConfig {
            min_size: 100,
            max_size: 1000,
            max_delay: Duration::from_secs(10),
            adaptive: false,
            velocity_window: Duration::from_millis(100),
        };

        let mut worker = BatchWorker::new(0, config, fresh_published(), 0);

        // Add some events (below threshold)
        worker.add_events(make_events(50, 0));
        assert_eq!(worker.pending_count(), 50);

        // Force flush
        let batch = worker.flush();
        assert_eq!(batch.events.len(), 50);
        assert!(!worker.has_pending());
    }

    #[test]
    fn test_sequence_numbers() {
        let config = BatchConfig::default();
        let mut worker = BatchWorker::new(0, config.clone(), fresh_published(), 0);

        // Create batches and verify sequence numbers
        worker.add_events(make_events(100, 0));
        let batch1 = worker.flush();
        assert_eq!(batch1.sequence_start, 0);

        worker.add_events(make_events(50, 0));
        let batch2 = worker.flush();
        assert_eq!(batch2.sequence_start, 100);

        worker.add_events(make_events(25, 0));
        let batch3 = worker.flush();
        assert_eq!(batch3.sequence_start, 150);
    }

    /// Regression: every `flush` must publish the
    /// post-flush `next_sequence` to the shared atomic so
    /// `bus::remove_shard_internal` can read it after awaiting the
    /// worker and use it as the stranded-flush `sequence_start`.
    /// Pre-fix the stranded batch hardcoded 0, colliding with the
    /// worker's first batch under JetStream's dedup window.
    #[test]
    fn flush_publishes_post_flush_next_sequence_to_shared_atomic() {
        let config = BatchConfig::default();
        let published = Arc::new(AtomicU64::new(0));
        let mut worker = BatchWorker::new(0, config, published.clone(), 0);

        // Pre-flush: atomic is at its initial value.
        assert_eq!(published.load(Ordering::Acquire), 0);

        worker.add_events(make_events(50, 0));
        let _ = worker.flush();

        assert_eq!(
            published.load(Ordering::Acquire),
            50,
            "post-flush atomic must mirror BatchWorker::next_sequence",
        );
    }

    /// Consecutive flushes keep the published atomic in lock-step
    /// with the internal counter — pin the addition (not just the
    /// initial set) so a future refactor that updates only on
    /// alternate flushes (or only when `events.is_empty()`) gets
    /// caught.
    #[test]
    fn flush_publishes_advance_consecutive_flushes() {
        let config = BatchConfig::default();
        let published = Arc::new(AtomicU64::new(0));
        let mut worker = BatchWorker::new(0, config, published.clone(), 0);

        worker.add_events(make_events(10, 0));
        let _ = worker.flush();
        assert_eq!(published.load(Ordering::Acquire), 10);

        worker.add_events(make_events(7, 0));
        let _ = worker.flush();
        assert_eq!(published.load(Ordering::Acquire), 17);

        worker.add_events(make_events(33, 0));
        let _ = worker.flush();
        assert_eq!(published.load(Ordering::Acquire), 50);
    }

    /// Mirror the saturating-add overflow behavior on the published
    /// atomic. `bus::remove_shard_internal` uses this value as a
    /// `sequence_start`; if it ever overflowed back to 0 the
    /// stranded batch's msg-ids would collide with the worker's
    /// first batch — the exact JetStream-dedup hazard the
    /// stranded-flush path is designed to avoid.
    #[test]
    fn flush_publishes_saturating_max_on_overflow() {
        let config = BatchConfig::default();
        let published = Arc::new(AtomicU64::new(0));
        let mut worker = BatchWorker::new(0, config, published.clone(), 0);

        worker.next_sequence = u64::MAX - 3;
        worker.add_events(make_events(10, 0));
        let _ = worker.flush();

        assert_eq!(worker.next_sequence, u64::MAX);
        assert_eq!(
            published.load(Ordering::Acquire),
            u64::MAX,
            "published atomic must saturate at u64::MAX, not wrap to 6",
        );
    }

    /// Regression: BUG_REPORT.md #19 — `next_sequence` previously used
    /// unchecked `+=`, which would silently wrap on overflow. Saturating
    /// pins it at `u64::MAX` so downstream consumers see a stable
    /// terminal state instead of restarting at 0.
    #[test]
    fn test_sequence_saturates_on_overflow() {
        let config = BatchConfig::default();
        let mut worker = BatchWorker::new(0, config, fresh_published(), 0);

        // Force the counter near overflow.
        worker.next_sequence = u64::MAX - 3;

        worker.add_events(make_events(10, 0));
        let batch = worker.flush();

        assert_eq!(batch.sequence_start, u64::MAX - 3);
        // Without saturation this would wrap to 6 and the next batch
        // would restart sequencing from there.
        assert_eq!(worker.next_sequence, u64::MAX);
    }
}
