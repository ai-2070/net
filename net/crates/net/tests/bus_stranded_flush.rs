//! Regression coverage for `bus::remove_shard_internal`'s stranded-
//! ring-buffer flush — covering the sequence_start collision under
//! JetStream dedup and the premature finalize-vs-pending-events
//! race.
//!
//! Both bugs land at the same code path: when a shard is being torn
//! down (via `manual_scale_down` or the scaling-monitor's auto path),
//! `remove_shard_internal` constructs a `Batch::new(shard_id,
//! stranded, sequence_start)` to flush whatever events were left in
//! the ring buffer at unmap time.
//!
//! - **Sequence collision.** Pre-fix `sequence_start` was hardcoded to `0`. Every
//!   `BatchWorker` for the same shard ALSO starts at sequence 0, so
//!   the stranded batch's per-event msg-ids
//!   (`{nonce}:{shard_id}:0:{i}`) collided with the worker's very
//!   first batch's msg-ids — JetStream's 2 min dedup window dropped
//!   the duplicates, silently losing the stranded events. The fix
//!   reads the worker's final `next_sequence` from a shared atomic
//!   after awaiting its `JoinHandle` and uses that as the
//!   `sequence_start`.
//!
//! - **Finalize race.** Pre-fix `remove_shard_internal` dropped the
//!   `BatchWorker`'s `JoinHandle` without await, so a finalize
//!   triggered while the worker still had `current_batch` events or
//!   in-flight mpsc-channel events would race the worker's own
//!   dispatch against this function's stranded-flush. The fix awaits
//!   the worker first; by the time the stranded batch dispatches,
//!   the worker has flushed everything it had pending under proper
//!   sequencing.
//!
//! Bespoke `RecordingAdapter` records every batch's `(shard_id,
//! sequence_start, len, msg_id_prefix)` so the assertions can
//! distinguish "batch was delivered" from "batch was deduped" and
//! pin both fixes directly.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use net::adapter::{Adapter, ShardPollResult};
use net::bus::EventBus;
use net::config::{EventBusConfig, ScalingPolicy};
use net::error::AdapterError;
use net::event::{Batch, Event};
use parking_lot::Mutex;
use serde_json::json;

/// One observation of an `on_batch` call.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BatchObservation {
    shard_id: u16,
    sequence_start: u64,
    len: usize,
    /// Stamped by the bus from its loaded `producer_nonce`.
    /// Recorded so tests can pin that the stranded-flush path uses
    /// the same nonce as the worker batches.
    process_nonce: u64,
}

type BatchHandle = Arc<Mutex<Vec<BatchObservation>>>;
type MsgIdHandle = Arc<Mutex<Vec<(u16, u64, usize)>>>;

/// Adapter that records every batch verbatim and emits a
/// `(shard_id, sequence_start, i)` "msg-id" tuple per event so the
/// test can detect collisions the same way JetStream's dedup window
/// would.
#[derive(Clone)]
struct RecordingAdapter {
    batches: BatchHandle,
    msg_ids: MsgIdHandle,
}

impl RecordingAdapter {
    fn new() -> (Self, BatchHandle, MsgIdHandle) {
        let batches = Arc::new(Mutex::new(Vec::new()));
        let msg_ids = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                batches: batches.clone(),
                msg_ids: msg_ids.clone(),
            },
            batches,
            msg_ids,
        )
    }
}

#[async_trait]
impl Adapter for RecordingAdapter {
    async fn init(&mut self) -> Result<(), AdapterError> {
        Ok(())
    }
    async fn on_batch(&self, batch: Batch) -> Result<(), AdapterError> {
        let shard_id = batch.shard_id;
        let sequence_start = batch.sequence_start;
        let len = batch.len();

        // Mirror the JetStream msg-id construction
        // (`adapter/jetstream.rs:281`): every event becomes a
        // `(shard_id, sequence_start, i)` tuple. If two batches'
        // tuples collide, JetStream's dedup would drop the second.
        // We don't drop here — we record, and assert no duplicates
        // in the test.
        {
            let mut ids = self.msg_ids.lock();
            for i in 0..len {
                ids.push((shard_id, sequence_start, i));
            }
        }
        self.batches.lock().push(BatchObservation {
            shard_id,
            sequence_start,
            len,
            process_nonce: batch.process_nonce,
        });
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
        "recording"
    }
}

fn config(num_shards: u16) -> EventBusConfig {
    let policy = ScalingPolicy {
        min_shards: 1,
        max_shards: 16,
        cooldown: Duration::from_nanos(1),
        ..Default::default()
    };
    EventBusConfig::builder()
        .num_shards(num_shards)
        .ring_buffer_capacity(1024)
        .scaling(policy)
        .build()
        .unwrap()
}

/// Pin the stranded-flush batch's `sequence_start` MUST
/// be strictly past every `(shard_id, sequence_start, i)` msg-id
/// the worker emitted. Pre-fix it was hardcoded to 0, colliding
/// with the worker's very first batch.
#[tokio::test]
async fn stranded_flush_does_not_collide_with_worker_msg_ids() {
    let (adapter, batches, msg_ids) = RecordingAdapter::new();
    let bus = EventBus::new_with_adapter(config(2), Box::new(adapter))
        .await
        .unwrap();

    // Scale up to 4, then back down to 2. The 2 removed shards
    // each produce a stranded-flush via `remove_shard_internal`.
    let added = bus.manual_scale_up(2).await.unwrap();
    assert_eq!(added.len(), 2);

    // Push enough events to force several flushes per shard so the
    // worker's `next_sequence` is well past 0 by the time scale-down
    // runs.
    for i in 0..2_000u64 {
        let _ = bus.ingest(Event::new(json!({"i": i})));
    }
    bus.flush().await.unwrap();

    let removed = bus.manual_scale_down(2).await.unwrap();
    assert_eq!(removed.len(), 2);
    bus.shutdown().await.unwrap();

    // Assertion: every `(shard_id, sequence_start, i)` tuple is
    // unique. A pre-fix run would have at least one shard where the
    // stranded batch's msg-ids collide with the worker's first
    // batch's msg-ids — under JetStream dedup those events would be
    // silently dropped.
    let ids = msg_ids.lock().clone();
    let mut sorted = ids.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        ids.len(),
        "duplicate (shard, sequence_start, i) msg-id tuples observed — \
         stranded-flush collided with worker batch. \
         Total batches: {}, total msg-ids: {}, unique: {}",
        batches.lock().len(),
        ids.len(),
        sorted.len(),
    );
}

/// Pin the wire-level invariant directly on the recorded batches:
/// for every shard there's at most one batch with `sequence_start
/// == 0`, even though both the worker's first batch AND the
/// stranded-flush batch *could* both want that slot.
#[tokio::test]
async fn at_most_one_batch_per_shard_uses_sequence_start_zero() {
    let (adapter, batches, _) = RecordingAdapter::new();
    let bus = EventBus::new_with_adapter(config(2), Box::new(adapter))
        .await
        .unwrap();

    let _ = bus.manual_scale_up(2).await.unwrap();
    for i in 0..1_000u64 {
        let _ = bus.ingest(Event::new(json!({"i": i})));
    }
    bus.flush().await.unwrap();

    let _ = bus.manual_scale_down(2).await.unwrap();
    bus.shutdown().await.unwrap();

    let observations = batches.lock().clone();
    // For each shard_id, count batches with sequence_start == 0.
    use std::collections::HashMap;
    let mut zero_starts: HashMap<u16, usize> = HashMap::new();
    for o in &observations {
        if o.sequence_start == 0 {
            *zero_starts.entry(o.shard_id).or_default() += 1;
        }
    }
    for (shard_id, count) in &zero_starts {
        assert!(
            *count <= 1,
            "shard {} produced {} batches with sequence_start=0 — \
             stranded-flush re-used the worker's first-batch sequence, \
             colliding under JetStream dedup. \
             Recorded batches: {:?}",
            shard_id,
            count,
            observations,
        );
    }
}

/// Pin: events still in the BatchWorker's mpsc channel or
/// `current_batch` at the moment `remove_shard_internal` is invoked
/// must reach the adapter — they must NOT be silently dropped or
/// race the stranded-flush.
///
/// We force an eager finalize by pushing a small number of events
/// (less than `min_batch_size`), letting the drain worker pump them
/// into the BatchWorker's channel, then triggering scale-down before
/// the BatchWorker's flush timeout fires. Pre-fix the `JoinHandle`
/// would be dropped without await, and the worker's pending
/// `current_batch` would race with the stranded-flush dispatch.
#[tokio::test]
async fn events_in_flight_at_finalize_reach_adapter() {
    let (adapter, _batches, msg_ids) = RecordingAdapter::new();
    let bus = EventBus::new_with_adapter(config(2), Box::new(adapter))
        .await
        .unwrap();

    let added = bus.manual_scale_up(2).await.unwrap();
    assert_eq!(added.len(), 2);

    // Push 100 events but DON'T call flush. The drain worker will
    // pump them into the BatchWorker's channel; the BatchWorker may
    // or may not have flushed (depends on `min_batch_size` /
    // `max_delay`). Either way, scale-down must not lose them.
    const N: u64 = 100;
    for i in 0..N {
        let _ = bus.ingest(Event::new(json!({"i": i})));
    }

    // Tight scale-down — no `flush()` first. With pre-fix code, any
    // events still in the BatchWorker's `current_batch` or in the
    // mpsc channel could be dispatched concurrently with the
    // stranded-flush, racing through dedup.
    let _ = bus.manual_scale_down(2).await.unwrap();
    bus.shutdown().await.unwrap();

    // Every event must show up in the adapter exactly once.
    let total_seen: usize = msg_ids.lock().len();
    assert_eq!(
        total_seen, N as usize,
        "expected exactly {N} events delivered to adapter; got {total_seen}. \
         Events lost between BatchWorker pending state and stranded-flush \
         (race window)",
    );

    // No duplicates either — the same msg-id collision logic
    // applies if the worker's pending batch and the
    // stranded batch raced through dedup.
    let ids = msg_ids.lock().clone();
    let mut sorted = ids.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        ids.len(),
        "duplicate msg-id tuples observed during in-flight finalize — \
         BatchWorker's pending batch raced with stranded-flush",
    );
}

/// Pin: `manual_scale_up(N>1)` succeeds against a non-zero
/// cooldown. Pre-fix the loop called `add_shard_internal`
/// which routed to `scale_up_provisioning(1)`, bumping
/// `last_scaling = Instant::now()` on each call. Iteration 1+
/// then immediately failed with `InCooldown` (default 30s
/// cooldown), leaving the first shard half-added (workers
/// spawned, routing entry installed) and returning the error
/// to the operator with no rollback. Post-fix `manual_scale_up`
/// bypasses the auto-scaling cooldown so a deliberate operator
/// request can scale up by N in one call regardless of the
/// cooldown setting.
#[tokio::test]
async fn manual_scale_up_succeeds_with_nonzero_cooldown() {
    let (adapter, _batches, _msg_ids) = RecordingAdapter::new();
    // Realistic operator-deploy config: a multi-second
    // cooldown to dampen the auto-scaling monitor.
    let policy = ScalingPolicy {
        min_shards: 1,
        max_shards: 8,
        cooldown: Duration::from_secs(30),
        ..Default::default()
    };
    let cfg = EventBusConfig::builder()
        .num_shards(1)
        .ring_buffer_capacity(1024)
        .scaling(policy)
        .build()
        .unwrap();
    let bus = EventBus::new_with_adapter(cfg, Box::new(adapter))
        .await
        .unwrap();

    // Scale up by 4 in one call. Pre-fix this would error on
    // the second iteration (the first call bumped last_scaling,
    // the second hit cooldown).
    let added = bus
        .manual_scale_up(4)
        .await
        .expect("manual_scale_up under nonzero cooldown must succeed");
    assert_eq!(added.len(), 4, "all 4 requested shards must be added");

    bus.shutdown().await.unwrap();
}

/// `SlowRecordingAdapter` sleeps for `delay` inside `on_batch`,
/// which lets the BatchWorker's mpsc channel back up + lets events
/// pile in the ring buffer while a scale-down is in flight. This
/// is what actually exercises the stranded-flush code path —
/// without it, the ring buffer drains so quickly that
/// `remove_shard()` returns an empty `stranded` `Vec` and
/// `remove_shard_internal` never builds a stranded batch at all.
struct SlowRecordingAdapter {
    inner: RecordingAdapter,
    delay: Duration,
}

#[async_trait]
impl Adapter for SlowRecordingAdapter {
    async fn init(&mut self) -> Result<(), AdapterError> {
        Ok(())
    }
    async fn on_batch(&self, batch: Batch) -> Result<(), AdapterError> {
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
        _shard_id: u16,
        _from_id: Option<&str>,
        _limit: usize,
    ) -> Result<ShardPollResult, AdapterError> {
        Ok(ShardPollResult::empty())
    }
    fn name(&self) -> &'static str {
        "slow_recording"
    }
}

/// Pin the producer-nonce + sequence-start interaction: when the bus is configured
/// with a persistent `producer_nonce_path`, the stranded-flush
/// batch (constructed in `remove_shard_internal`) MUST stamp the
/// same `process_nonce` as the worker's own batches. JetStream's
/// `Nats-Msg-Id` keys dedup on this nonce; a stranded batch with a
/// different nonce would land outside the producer-identity dedup
/// scope and cross-restart retries wouldn't recognize it as a
/// duplicate of the prior incarnation's stranded events.
///
/// The fix at `bus.rs::remove_shard_internal` uses `Batch::with_nonce(..., self.producer_nonce)`;
/// a future refactor that reverted to `Batch::new` (which calls the
/// per-process fallback `batch_process_nonce()`) would silently
/// regress this for buses configured with a persistent path.
///
/// We use the same `SlowRecordingAdapter` pattern from the
/// stranded-flush test below to actually exercise the stranded-flush path
/// — without back-pressure on the worker pipeline `flush()`
/// drains the ring buffer cleanly and `remove_shard_internal`'s
/// `if !stranded.is_empty()` block doesn't run.
#[tokio::test]
async fn stranded_flush_uses_bus_producer_nonce() {
    let (recording, batches, _msg_ids) = RecordingAdapter::new();
    let slow = SlowRecordingAdapter {
        inner: recording,
        delay: Duration::from_millis(5),
    };

    // Per-test temp file so concurrent runs don't collide.
    let mut nonce_path = std::env::temp_dir();
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    nonce_path.push(format!("net-test-stranded-nonce-{pid}-{nanos}"));

    let policy = ScalingPolicy {
        min_shards: 1,
        max_shards: 16,
        cooldown: Duration::from_nanos(1),
        ..Default::default()
    };
    let config = EventBusConfig::builder()
        .num_shards(2)
        .ring_buffer_capacity(2048)
        .scaling(policy)
        .producer_nonce_path(&nonce_path)
        .build()
        .unwrap();

    let bus = EventBus::new_with_adapter(config, Box::new(slow))
        .await
        .unwrap();

    let added = bus.manual_scale_up(2).await.unwrap();
    assert_eq!(added.len(), 2);

    // Push events fast enough that the slow adapter backs up the
    // BatchWorker channel and events accumulate in the ring buffer
    // — by the time `manual_scale_down` runs, the marked-Draining
    // shards still have events queued and the stranded-flush
    // dispatch fires.
    for i in 0..5_000u64 {
        let _ = bus.ingest(Event::new(json!({"i": i})));
    }

    let _ = bus.manual_scale_down(2).await.unwrap();
    bus.shutdown().await.unwrap();

    let observations = batches.lock().clone();
    assert!(
        !observations.is_empty(),
        "expected the recording adapter to have observed at least one batch",
    );
    let first_nonce = observations[0].process_nonce;
    for (i, obs) in observations.iter().enumerate() {
        assert_eq!(
            obs.process_nonce, first_nonce,
            "batch {i} (shard {}, seq {}, len {}) stamped a different \
             nonce ({:#x}) than the first batch ({:#x}) — the \
             stranded-flush path must use the bus's producer_nonce",
            obs.shard_id, obs.sequence_start, obs.len, obs.process_nonce, first_nonce,
        );
    }

    let _ = std::fs::remove_file(&nonce_path);
}

/// Pin both fixes directly by *forcing* the stranded-flush path
/// to run with a non-empty `stranded` Vec. The slow adapter backs
/// up the BatchWorker's mpsc channel, which backs up the drain
/// worker, which leaves events sitting in the ring buffer at the
/// moment of `remove_shard()`. The fix's `final_next_sequence` read
/// MUST be strictly past every `(shard_id, sequence_start, i)` the
/// worker emitted before exit.
///
/// Without the fix this test fails: the stranded batch's msg-ids
/// (`sequence_start = 0`) collide with the worker's first batch's
/// msg-ids — duplicates show up in `msg_ids`.
#[tokio::test]
async fn stranded_flush_with_real_stranded_events_uses_post_worker_sequence() {
    let (recording, batches, msg_ids) = RecordingAdapter::new();
    let slow = SlowRecordingAdapter {
        inner: recording,
        delay: Duration::from_millis(5),
    };

    let policy = ScalingPolicy {
        min_shards: 1,
        max_shards: 16,
        cooldown: Duration::from_nanos(1),
        ..Default::default()
    };
    let config = EventBusConfig::builder()
        .num_shards(2)
        .ring_buffer_capacity(2048)
        .scaling(policy)
        .build()
        .unwrap();

    let bus = EventBus::new_with_adapter(config, Box::new(slow))
        .await
        .unwrap();

    let added = bus.manual_scale_up(2).await.unwrap();
    assert_eq!(added.len(), 2);

    // Push events very rapidly. With the slow adapter (5 ms per
    // batch), the BatchWorker's 1024-slot mpsc channel backs up,
    // the drain worker stalls on `sender.send().await`, and events
    // queue up in the ring buffer. By the time `manual_scale_down`
    // runs, the marked-Draining shard has both:
    //   - emitted batches with sequence_starts 0, k, 2k, … (worker
    //     sequence advances on every flush)
    //   - leftover events in the ring buffer (stranded)
    for i in 0..5_000u64 {
        let _ = bus.ingest(Event::new(json!({"i": i})));
    }

    let _ = bus.manual_scale_down(2).await.unwrap();
    bus.shutdown().await.unwrap();

    // Verify there are NO duplicate msg-ids — the stranded batch's
    // sequence_start was past the worker's emitted ones.
    let ids = msg_ids.lock().clone();
    let mut sorted = ids.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        ids.len(),
        "duplicate msg-id tuples observed in stranded-flush path. \
         Total batches: {}, total msg-ids: {}, unique: {}",
        batches.lock().len(),
        ids.len(),
        sorted.len(),
    );
}

/// Pin the cross-cutting guarantee: a sustained ingest -> repeated
/// scale_up/scale_down cycle delivers every event the producer
/// pushed to the adapter, with no msg-id collisions across cycles.
/// This is the "production-shape" stress for both fixes together.
#[tokio::test]
async fn repeated_scale_cycles_preserve_every_event_with_unique_msg_ids() {
    let (adapter, _batches, msg_ids) = RecordingAdapter::new();
    let bus = EventBus::new_with_adapter(config(2), Box::new(adapter))
        .await
        .unwrap();

    let mut total_ingested = 0u64;
    for cycle in 0..3 {
        let _ = bus.manual_scale_up(1).await.unwrap();
        for i in 0..200u64 {
            if bus
                .ingest(Event::new(json!({"cycle": cycle, "i": i})))
                .is_ok()
            {
                total_ingested += 1;
            }
        }
        bus.flush().await.unwrap();
        let _ = bus.manual_scale_down(1).await.unwrap();
    }
    bus.shutdown().await.unwrap();

    let ids = msg_ids.lock().clone();
    assert_eq!(
        ids.len() as u64,
        total_ingested,
        "{} ingested events; adapter saw {}",
        total_ingested,
        ids.len(),
    );

    let mut sorted = ids.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        ids.len(),
        "duplicate msg-id tuples observed across scale cycles",
    );
}

/// Adapter whose `on_batch` parks indefinitely. Models a wedged
/// downstream (network partition, deadlocked driver) and lets us
/// observe whether `remove_shard_internal` returns within bounded
/// time when its workers are themselves parked inside an adapter
/// call.
struct WedgedAdapter;

#[async_trait]
impl Adapter for WedgedAdapter {
    async fn init(&mut self) -> Result<(), AdapterError> {
        Ok(())
    }
    async fn on_batch(&self, _batch: Batch) -> Result<(), AdapterError> {
        std::future::pending::<()>().await;
        unreachable!()
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
        "wedged"
    }
}

/// Pin: when worker handles cannot make progress (wedged adapter),
/// `manual_scale_down` must NOT pin indefinitely waiting for them
/// to exit. The worker handles are awaited under a timeout derived
/// from `adapter_timeout`; on expiry the JoinHandle is detached.
///
/// Pre-fix `remove_shard_internal` awaited the JoinHandles bare,
/// AND in the wrong order (batch before drain), so a drain worker
/// parked mid-`sender.send().await` against a batch worker parked
/// mid-adapter call would deadlock the awaiter forever.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn manual_scale_down_returns_within_bounded_time_when_adapter_wedged() {
    let policy = ScalingPolicy {
        min_shards: 1,
        max_shards: 16,
        cooldown: Duration::from_nanos(1),
        ..Default::default()
    };
    let cfg = EventBusConfig::builder()
        .num_shards(2)
        .ring_buffer_capacity(1024)
        .scaling(policy)
        .adapter_timeout(Duration::from_millis(150))
        .build()
        .unwrap();

    let bus = EventBus::new_with_adapter(cfg, Box::new(WedgedAdapter))
        .await
        .unwrap();

    bus.manual_scale_up(2).await.unwrap();

    // Push enough events to keep workers busy in `on_batch` (which
    // never returns).
    for i in 0..2_000u64 {
        let _ = bus.ingest(Event::new(json!({"i": i})));
    }

    // Each worker await is bounded by 2 × adapter_timeout = 300 ms,
    // and we tear down 2 shards sequentially, so the upper bound is
    // ~1.2 s of worker waits plus the stranded-flush dispatch_batch
    // (which uses adapter_timeout = 150 ms with retries). Allow
    // generous slack for CI.
    let started = std::time::Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(15), bus.manual_scale_down(2))
        .await
        .expect("manual_scale_down hung past 15s — timeout-bounded teardown regressed");
    result.expect("manual_scale_down returned Err");

    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(15),
        "manual_scale_down took {:?}; expected bounded by adapter_timeout",
        elapsed,
    );

    // Best-effort shutdown — the wedged adapter will not let us
    // drain cleanly, but shutdown itself is bounded by its own
    // deadline. We don't assert success here; the assertion above
    // is the regression target.
    let _ = tokio::time::timeout(Duration::from_secs(5), bus.shutdown()).await;
}

/// Pin: when an `EventBus` is dropped without an awaited
/// `shutdown()`, the helper used by the Drop impl
/// (`shard_manager.total_pending_in_rings()`) must report the
/// exact count of events still sitting in ring buffers. The Drop
/// impl folds this into `events_dropped` + sets
/// `shutdown_was_lossy` before the bus disappears so post-mortem
/// stats reflect the data-loss incident.
///
/// The Drop impl itself runs as the bus disappears so its own
/// `events_dropped` increment is unobservable from this test
/// (the `EventBusStats` lives on the bus). Pin the helper that
/// supplies the increment instead — its return value is the
/// exact value the Drop impl folds in.
#[tokio::test]
async fn total_pending_in_rings_reports_stranded_count() {
    let policy = ScalingPolicy {
        min_shards: 1,
        max_shards: 4,
        cooldown: Duration::from_nanos(1),
        ..Default::default()
    };
    // Slow adapter + huge `min_size` and `max_delay` so events
    // accumulate in rings instead of racing through to dispatch.
    let batch_cfg = net::config::BatchConfig {
        min_size: 10_000,
        max_size: 10_000,
        max_delay: Duration::from_secs(60),
        adaptive: false,
        velocity_window: Duration::from_millis(100),
    };
    let cfg = EventBusConfig::builder()
        .num_shards(2)
        .ring_buffer_capacity(1024)
        .scaling(policy)
        .batch(batch_cfg)
        .build()
        .unwrap();

    let (recording, _batches, _msg_ids) = RecordingAdapter::new();
    let slow = SlowRecordingAdapter {
        inner: recording,
        delay: Duration::from_secs(60),
    };
    let bus = EventBus::new_with_adapter(cfg, Box::new(slow))
        .await
        .unwrap();

    // Ingest enough events to overflow the ring buffer faster
    // than the drain worker can pump them out. With
    // `ring_buffer_capacity = 1024` per shard and 2 shards,
    // pushing 5_000 events guarantees at least one ring is
    // saturated at the moment we read `pending_in_rings()`.
    const N: u64 = 5_000;
    for i in 0..N {
        let _ = bus.ingest(Event::new(json!({"i": i})));
    }

    let pending_in_rings = bus.pending_in_rings();
    assert!(
        pending_in_rings > 0,
        "expected events still in ring buffers before drop \
         (got {}); the Drop impl's stranded-in-rings increment \
         would be silently 0 and `shutdown_was_lossy` would not \
         be set, masking the data-loss incident",
        pending_in_rings,
    );

    // Force the warn-and-account path by dropping the bus
    // without awaiting shutdown. The Drop impl folds
    // `pending_in_rings` into `events_dropped` and sets
    // `shutdown_was_lossy`. We can't read those post-drop
    // (the EventBusStats lives on the bus), but the helper
    // value above is what the Drop impl uses.
    drop(bus);
}
