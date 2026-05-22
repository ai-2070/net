//! `EventBus::shutdown` drain race + retry path coverage.
//!
//! Two related contracts are pinned here, both load-bearing for
//! exactly-once-style guarantees and neither tested before this
//! file:
//!
//! 1. **Drain race.** `spawn_batch_worker`'s post-shutdown drain
//!    loop (`src/bus.rs:646-668`) explicitly handles events sent by
//!    the drain worker AFTER the worker last checked the shutdown
//!    flag. Without that drain, those final events sit in the
//!    channel buffer and are silently lost. Existing bus tests
//!    only assert `events_ingested` (which counts at the producer
//!    boundary, not at the adapter boundary), so this loss would
//!    be invisible. We assert here that *every* event the producer
//!    handed to the bus reaches the adapter, even when shutdown
//!    races ingestion.
//!
//! 2. **Retry path.** `dispatch_batch` (`src/bus.rs:604-635`)
//!    re-submits a batch up to `adapter_batch_retries` times when
//!    the adapter returns Err. The adapter contract documents that
//!    adapters MUST be idempotent under retry
//!    (`src/adapter/mod.rs:11`). We assert that a transiently
//!    failing adapter still receives every batch eventually under
//!    a non-zero retry budget — no silent drops.
//!
//! Both tests use bespoke adapter fixtures defined in this file.
//! Installation goes through `EventBus::new_with_adapter`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use net::adapter::{Adapter, ShardPollResult};
use net::bus::EventBus;
use net::config::EventBusConfig;
use net::error::AdapterError;
use net::event::{Batch, Event};
use serde_json::json;

/// Adapter that records every event it ever sees, plus its batch
/// id so we can detect both drops and re-deliveries in the retry
/// test. Cheap enough to use at 10k+ events per iteration.
struct CountingAdapter {
    events_seen: Arc<AtomicU64>,
    batches_seen: Arc<AtomicU64>,
}

impl CountingAdapter {
    fn new() -> (Self, Arc<AtomicU64>, Arc<AtomicU64>) {
        let events = Arc::new(AtomicU64::new(0));
        let batches = Arc::new(AtomicU64::new(0));
        (
            Self {
                events_seen: events.clone(),
                batches_seen: batches.clone(),
            },
            events,
            batches,
        )
    }
}

#[async_trait]
impl Adapter for CountingAdapter {
    async fn init(&mut self) -> Result<(), AdapterError> {
        Ok(())
    }
    async fn on_batch(&self, batch: std::sync::Arc<Batch>) -> Result<(), AdapterError> {
        self.batches_seen.fetch_add(1, Ordering::Relaxed);
        self.events_seen
            .fetch_add(batch.len() as u64, Ordering::Relaxed);
        Ok(())
    }
    async fn flush(&self) -> Result<(), AdapterError> {
        Ok(())
    }
    async fn shutdown(&self) -> Result<(), AdapterError> {
        Ok(())
    }
    async fn poll_shard(
        &self,
        _shard_id: u16,
        _from_id: Option<&str>,
        _limit: usize,
    ) -> Result<ShardPollResult, AdapterError> {
        Ok(ShardPollResult::empty())
    }
    fn name(&self) -> &'static str {
        "counting"
    }
}

/// Same as `CountingAdapter` but sleeps before recording, so
/// batches sit "in the adapter" while the bus is shutting down.
/// Surfaces the drain race window.
struct SlowCountingAdapter {
    inner: CountingAdapter,
    delay: Duration,
}

#[async_trait]
impl Adapter for SlowCountingAdapter {
    async fn init(&mut self) -> Result<(), AdapterError> {
        self.inner.init().await
    }
    async fn on_batch(&self, batch: std::sync::Arc<Batch>) -> Result<(), AdapterError> {
        tokio::time::sleep(self.delay).await;
        self.inner.on_batch(batch).await
    }
    async fn flush(&self) -> Result<(), AdapterError> {
        self.inner.flush().await
    }
    async fn shutdown(&self) -> Result<(), AdapterError> {
        self.inner.shutdown().await
    }
    async fn poll_shard(
        &self,
        s: u16,
        f: Option<&str>,
        l: usize,
    ) -> Result<ShardPollResult, AdapterError> {
        self.inner.poll_shard(s, f, l).await
    }
    fn name(&self) -> &'static str {
        "slow-counting"
    }
}

/// Adapter that fails the first `fail_n_times` `on_batch` calls
/// per batch, then succeeds. Counts both attempts (failed + ok)
/// and only-successful deliveries. Used to exercise the retry
/// path. Failure budget is global, not per-batch — easier to set
/// up and exercises retries on the first batch which is the most
/// likely place for ordering bugs.
struct FlakyAdapter {
    failures_remaining: Arc<AtomicU64>,
    successful_events: Arc<AtomicU64>,
    successful_batches: Arc<AtomicU64>,
    attempts: Arc<AtomicU64>,
}

impl FlakyAdapter {
    fn new(fail_n_times: u64) -> (Self, FlakyHandles) {
        let failures = Arc::new(AtomicU64::new(fail_n_times));
        let events = Arc::new(AtomicU64::new(0));
        let batches = Arc::new(AtomicU64::new(0));
        let attempts = Arc::new(AtomicU64::new(0));
        (
            Self {
                failures_remaining: failures.clone(),
                successful_events: events.clone(),
                successful_batches: batches.clone(),
                attempts: attempts.clone(),
            },
            FlakyHandles {
                failures_remaining: failures,
                successful_events: events,
                successful_batches: batches,
                attempts,
            },
        )
    }
}

struct FlakyHandles {
    failures_remaining: Arc<AtomicU64>,
    successful_events: Arc<AtomicU64>,
    successful_batches: Arc<AtomicU64>,
    attempts: Arc<AtomicU64>,
}

#[async_trait]
impl Adapter for FlakyAdapter {
    async fn init(&mut self) -> Result<(), AdapterError> {
        Ok(())
    }
    async fn on_batch(&self, batch: std::sync::Arc<Batch>) -> Result<(), AdapterError> {
        self.attempts.fetch_add(1, Ordering::Relaxed);
        // Atomic check-and-decrement via fetch_update — a racy
        // `load > 0` + `fetch_sub(1)` pair lets two concurrent
        // retries both pass the check when only one slot remains
        // and underflow the counter to u64::MAX (cubic-flagged
        // P2). `fetch_update` returns Ok iff the closure returned
        // Some, which is exactly "we won the slot" semantics.
        let injected = self
            .failures_remaining
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |v| {
                if v > 0 {
                    Some(v - 1)
                } else {
                    None
                }
            })
            .is_ok();
        if injected {
            return Err(AdapterError::Transient(
                "flaky-adapter test-injected failure".into(),
            ));
        }
        self.successful_batches.fetch_add(1, Ordering::Relaxed);
        self.successful_events
            .fetch_add(batch.len() as u64, Ordering::Relaxed);
        Ok(())
    }
    async fn flush(&self) -> Result<(), AdapterError> {
        Ok(())
    }
    async fn shutdown(&self) -> Result<(), AdapterError> {
        Ok(())
    }
    async fn poll_shard(
        &self,
        _: u16,
        _: Option<&str>,
        _: usize,
    ) -> Result<ShardPollResult, AdapterError> {
        Ok(ShardPollResult::empty())
    }
    fn name(&self) -> &'static str {
        "flaky"
    }
}

/// Pin the drain semantics: after `bus.shutdown().await` returns,
/// every event the producer handed to the bus has reached the
/// adapter. With the default zero-retry config and a never-failing
/// adapter, this is also exactly-once.
///
/// Run several iterations to surface schedule-dependent races.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_delivers_all_pending_events_to_adapter() {
    const ITERATIONS: usize = 20;
    const EVENTS_PER_ITER: u64 = 10_000;

    for iter in 0..ITERATIONS {
        let (adapter, events_seen, _batches_seen) = CountingAdapter::new();
        let config = EventBusConfig::builder()
            .num_shards(4)
            .ring_buffer_capacity(1 << 14) // 16k per shard, ample headroom
            .without_scaling()
            .build()
            .unwrap();

        let bus = EventBus::new_with_adapter(config, Box::new(adapter))
            .await
            .unwrap();

        for i in 0..EVENTS_PER_ITER {
            bus.ingest(Event::new(json!({"i": i, "iter": iter})))
                .unwrap();
        }

        bus.shutdown().await.unwrap();

        let delivered = events_seen.load(Ordering::Relaxed);
        assert_eq!(
            delivered, EVENTS_PER_ITER,
            "iter {iter}: producer ingested {EVENTS_PER_ITER} events but adapter only saw {delivered} after shutdown"
        );
    }
}

/// Same property, but with the adapter sleeping per batch so that
/// shutdown is much more likely to land while events are still
/// in the channel between the drain worker and the batch worker.
/// This is the specific scenario the inline comment on
/// `src/bus.rs:651-655` calls out as silently dropping events
/// without the post-shutdown drain.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_drains_events_in_flight_when_flag_is_set() {
    const EVENTS: u64 = 5_000;

    let (inner, events_seen, _) = CountingAdapter::new();
    let adapter = SlowCountingAdapter {
        inner,
        // Long enough that several batches are "in the adapter"
        // when shutdown begins, but short enough to keep the test
        // bounded.
        delay: Duration::from_millis(2),
    };

    let config = EventBusConfig::builder()
        .num_shards(4)
        .ring_buffer_capacity(1 << 14)
        .without_scaling()
        .build()
        .unwrap();

    let bus = EventBus::new_with_adapter(config, Box::new(adapter))
        .await
        .unwrap();

    for i in 0..EVENTS {
        bus.ingest(Event::new(json!({"i": i}))).unwrap();
    }

    bus.shutdown().await.unwrap();

    let delivered = events_seen.load(Ordering::Relaxed);
    assert_eq!(
        delivered, EVENTS,
        "adapter saw {delivered}/{EVENTS} events after shutdown — drain leaked events"
    );
}

/// Pin the retry contract: with `adapter_batch_retries(N)` and
/// an adapter that returns Err on its first K calls (K <= N+1),
/// every batch is eventually delivered. No silent drops on
/// transient failures.
///
/// Note on duplication: `dispatch_batch` retries by re-submitting
/// the *same* batch, so the adapter sees `attempts > batches`.
/// That is the documented contract — adapters MUST be idempotent
/// under retry (`src/adapter/mod.rs:11`). We assert the eventual
/// success count equals what the producer ingested, and verify
/// `attempts > successful_batches` to confirm retries actually
/// happened.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dispatch_batch_retries_eventually_deliver_all_events() {
    const EVENTS: u64 = 1_000;
    const FAIL_FIRST_N_BATCHES: u64 = 3;

    let (adapter, handles) = FlakyAdapter::new(FAIL_FIRST_N_BATCHES);
    let config = EventBusConfig::builder()
        .num_shards(2)
        .ring_buffer_capacity(1 << 14)
        // 3 retries means up to 4 total attempts per batch — enough
        // for FAIL_FIRST_N_BATCHES=3 to all eventually succeed even
        // if they happened to all be the *first* batch on a single
        // shard worker.
        .adapter_batch_retries(3)
        .without_scaling()
        .build()
        .unwrap();

    let bus = EventBus::new_with_adapter(config, Box::new(adapter))
        .await
        .unwrap();

    for i in 0..EVENTS {
        bus.ingest(Event::new(json!({"i": i}))).unwrap();
    }
    bus.shutdown().await.unwrap();

    let delivered = handles.successful_events.load(Ordering::Relaxed);
    let attempts = handles.attempts.load(Ordering::Relaxed);
    let batches = handles.successful_batches.load(Ordering::Relaxed);
    let remaining_failures = handles.failures_remaining.load(Ordering::Relaxed);

    assert_eq!(
        delivered, EVENTS,
        "expected all {EVENTS} events delivered after retries; got {delivered} (attempts={attempts}, batches={batches})"
    );
    assert_eq!(
        remaining_failures, 0,
        "test did not exhaust the injected failure budget — flaky adapter not exercised"
    );
    assert!(
        attempts > batches,
        "no retries observed (attempts={attempts}, batches={batches}) — retry path not exercised"
    );
}
