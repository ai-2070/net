//! Main EventBus facade.
//!
//! The EventBus provides a unified API for:
//! - Event ingestion (non-blocking)
//! - Event consumption (async polling with filtering)
//! - Lifecycle management

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::adapter::{Adapter, NoopAdapter};
use crate::config::{AdapterConfig, BatchConfig, EventBusConfig};
use crate::consumer::{ConsumeRequest, ConsumeResponse, PollMerger};
use crate::error::{AdapterError, ConsumerError, IngestionError, IngestionResult};
use crate::event::{Batch, Event, RawEvent};
use crate::shard::{BatchWorker, ScalingDecision, ShardManager, ShardMetrics};

#[cfg(feature = "jetstream")]
use crate::adapter::JetStreamAdapter;
#[cfg(feature = "net")]
use crate::adapter::NetAdapter;
#[cfg(feature = "redis")]
use crate::adapter::RedisAdapter;

/// The main event bus.
///
/// # Example
///
/// ```rust,ignore
/// use net::{EventBus, EventBusConfig, Event};
///
/// #[tokio::main]
/// async fn main() -> Result<(), Box<dyn std::error::Error>> {
///     let bus = EventBus::new(EventBusConfig::default()).await?;
///
///     // Ingest events
///     bus.ingest(Event::from_str(r#"{"token": "hello"}"#)?)?;
///
///     // Poll events
///     let response = bus.poll(ConsumeRequest::new(100)).await?;
///
///     bus.shutdown().await?;
///     Ok(())
/// }
/// ```
pub struct EventBus {
    /// Shard manager for parallel ingestion.
    shard_manager: Arc<ShardManager>,
    /// Adapter for durable storage.
    adapter: Arc<dyn Adapter>,
    /// Poll merger for cross-shard consumption.
    poll_merger: arc_swap::ArcSwap<PollMerger>,
    /// Serializes the `shard_manager.shard_ids() → poll_merger.store`
    /// block in `add_shard_internal` / `remove_shard_internal`.
    /// Without this lock, two callers (e.g. scaling-monitor
    /// add_shard racing manual_scale_down's remove_shard) read the
    /// shard ids snapshot at slightly different points and then
    /// race on the `arc_swap.store`. The write that lands second
    /// can clobber the more-current view: T1 reads `{0..5}`, T2
    /// reads `{1..4}`, T2 stores `{1..4}`, T1 stores `{0..5}` —
    /// the published merger then routes polls to the just-removed
    /// shard 0 until the next topology change repairs the
    /// snapshot.
    poll_merger_swap_lock: parking_lot::Mutex<()>,
    /// Per-shard worker handles. Stored separately so shutdown can
    /// await drain workers *before* batch workers — the drain
    /// worker's final sweep races the batch worker's exit
    /// otherwise, and any events the drain worker pushes to the
    /// channel after the batch worker has stopped reading are
    /// silently lost.
    batch_workers: parking_lot::Mutex<std::collections::HashMap<u16, ShardWorkers>>,
    /// Channels for sending batches to workers (shard_id -> sender).
    batch_senders: parking_lot::RwLock<
        std::collections::HashMap<u16, mpsc::Sender<Vec<crate::event::InternalEvent>>>,
    >,
    /// Shutdown flag.
    shutdown: Arc<AtomicBool>,
    /// Gate signaling drain workers that the in-flight wait has
    /// completed and they may safely run their final ring-buffer
    /// sweep. Distinct from `shutdown` because the drain worker
    /// observing `shutdown=true` alone is not enough: a producer
    /// that read `shutdown=false` may still be mid-push, and if the
    /// drain worker rushes through its final sweep before that push
    /// is visible the event is stranded. `shutdown()` sets this
    /// after waiting for `in_flight_ingests==0`, at which point the
    /// Acquire load on the drain side synchronizes-with the Release
    /// store here, transitively chaining through the SeqCst
    /// in-flight handshake to make every observed-pre-shutdown push
    /// visible to the drain worker's subsequent `pop_batch_into`.
    drain_finalize_ready: Arc<AtomicBool>,
    /// In-flight ingest counter. Incremented before each ingest's
    /// shutdown check and decremented after the push completes (or
    /// bails). `shutdown()` waits for this to drop to zero *after*
    /// setting `shutdown=true` and *before* setting
    /// `drain_finalize_ready=true` so no producer is mid-push when
    /// the drain workers do their final sweep — closing the race
    /// where a producer that observed `shutdown=false` could push
    /// *after* the drain worker's last `pop_batch_into` returned
    /// zero, leaving the event stranded in the ring buffer.
    /// Pre-fix this was `AtomicU32`. A 4-billion-in-flight
    /// wrap is not realistic in production, but the counter
    /// participates in the shutdown protocol — a wrap to 0 would
    /// trick the wait-for-zero loop into thinking shutdown was
    /// safe to proceed while producers were still mid-push.
    /// Widened to `AtomicU64` so the wrap is astronomical
    /// (1.8e19 in-flight ingests).
    in_flight_ingests: AtomicU64,
    /// Set to `true` after `shutdown()` runs to completion. `Drop`
    /// uses this to detect "dropped without an awaited shutdown" —
    /// in that case events still in the ring buffers / mpsc channels
    /// are silently lost (see `Drop` impl).
    shutdown_completed: AtomicBool,
    /// Configuration.
    config: EventBusConfig,
    /// Statistics.
    stats: Arc<EventBusStats>,
    /// Producer nonce. Loaded from
    /// `config.producer_nonce_path` on startup when the path is
    /// configured; otherwise falls back to the per-process default
    /// from `event::batch_process_nonce`. Stamped on every batch
    /// the bus emits — the worker spawn copies this u64 into
    /// `BatchWorkerParams::producer_nonce`, and
    /// `remove_shard_internal`'s stranded-flush uses it via
    /// `Batch::with_nonce`.
    producer_nonce: u64,
    /// Scaling monitor task handle.
    scaling_monitor: parking_lot::Mutex<Option<JoinHandle<()>>>,
}

/// Worker handles for a single shard. The drain worker pumps
/// events from the ring buffer into an mpsc channel; the batch
/// worker reads from that channel and dispatches to the adapter.
/// Shutdown ordering is load-bearing — see `EventBus::shutdown`.
struct ShardWorkers {
    batch: JoinHandle<()>,
    drain: JoinHandle<()>,
    /// Bus-owned mirror of the BatchWorker's `next_sequence`.
    /// `remove_shard_internal` reads this AFTER awaiting `batch`
    /// to learn the worker's final post-flush sequence, then uses
    /// it as the `sequence_start` for the stranded-ring-buffer
    /// flush so the stranded msg-ids fall strictly past every
    /// msg-id the worker emitted — without this, JetStream dedup
    /// would silently drop the stranded batch when both used 0.
    next_sequence: Arc<AtomicU64>,
}

/// RAII guard for an in-flight ingest. Decrements
/// `in_flight_ingests` on drop so `shutdown()` can wait for the
/// counter to reach zero.
struct IngestGuard<'a> {
    bus: &'a EventBus,
}

impl Drop for IngestGuard<'_> {
    fn drop(&mut self) {
        self.bus
            .in_flight_ingests
            .fetch_sub(1, AtomicOrdering::SeqCst);
    }
}

/// Event bus statistics.
#[derive(Debug, Default)]
pub struct EventBusStats {
    /// Total events ingested.
    pub events_ingested: AtomicU64,
    /// Events dropped due to backpressure.
    pub events_dropped: AtomicU64,
    /// Batches dispatched to adapter.
    ///
    /// Pre-fix this field was declared but never incremented anywhere
    /// — `flush()`'s Phase 2 progress probe (`bus.rs:815`) read it as
    /// "did the BatchWorker make progress this `max_delay` window?",
    /// observed `0 == 0`, and always early-broke after a single
    /// window. On Windows-class timer resolution the race against the
    /// BatchWorker's first `recv_timeout` tipped the wrong way for
    /// `flush_is_a_delivery_barrier` regularly. Now incremented in
    /// the BatchWorker spawn (after a successful `dispatch_batch`)
    /// and in `remove_shard_internal`'s stranded-flush.
    pub batches_dispatched: AtomicU64,
    /// Total events dispatched to the adapter (sum of batch lengths
    /// from successful `on_batch` calls). Companion to
    /// `batches_dispatched` — by the time `flush()` returns,
    /// `events_dispatched + events_dropped == events_ingested`. FFI
    /// consumers can also use this to monitor end-to-end delivery.
    pub events_dispatched: AtomicU64,
    /// Set to `true` if `shutdown()` / `shutdown_via_ref()` had to
    /// proceed past the in-flight-ingest grace deadline (5 s) with
    /// producers still mid-push. The stranded count is counted into
    /// `events_dropped` (already; pre-fix), but `shutdown` returns
    /// `Ok(())` regardless — callers that need to distinguish
    /// "clean shutdown" from "lossy shutdown" check this flag in
    /// `bus.stats()` afterward (only meaningful for the
    /// `shutdown_via_ref` path that doesn't consume the bus).
    ///
    /// Pre-fix the warning + `events_dropped` increment
    /// were the only signal; `Result<(), AdapterError>` returned
    /// `Ok` indistinguishable from a clean shutdown.
    pub shutdown_was_lossy: std::sync::atomic::AtomicBool,
}

impl EventBus {
    /// Create a new event bus with the given configuration.
    pub async fn new(config: EventBusConfig) -> Result<Self, AdapterError> {
        // Create adapter from config
        let adapter: Box<dyn Adapter> = match &config.adapter {
            AdapterConfig::Noop => Box::new(NoopAdapter::new()),
            #[cfg(feature = "redis")]
            AdapterConfig::Redis(redis_config) => {
                Box::new(RedisAdapter::new(redis_config.clone())?)
            }
            #[cfg(feature = "jetstream")]
            AdapterConfig::JetStream(js_config) => {
                Box::new(JetStreamAdapter::new(js_config.clone())?)
            }
            #[cfg(feature = "net")]
            AdapterConfig::Net(net_config) => Box::new(NetAdapter::new((**net_config).clone())?),
        };

        Self::new_with_adapter(config, adapter).await
    }

    /// Create a new event bus with a caller-supplied adapter.
    ///
    /// `config.adapter` is ignored — the supplied `adapter` is used
    /// instead. Useful for tests that need to observe or inject
    /// behavior at the adapter boundary (e.g. a counting adapter
    /// for end-to-end delivery assertions, a flaky adapter for
    /// retry-path coverage).
    pub async fn new_with_adapter(
        config: EventBusConfig,
        mut adapter: Box<dyn Adapter>,
    ) -> Result<Self, AdapterError> {
        config
            .validate()
            .map_err(|e| AdapterError::Fatal(e.to_string()))?;

        // Initialize adapter (with timeout to prevent hanging on unreachable backends)
        tokio::time::timeout(config.adapter_timeout, adapter.init())
            .await
            .map_err(|_| AdapterError::Fatal("adapter init timed out".into()))??;
        let adapter: Arc<dyn Adapter> = Arc::from(adapter);

        // Create shard manager (with or without dynamic scaling)
        let shard_manager = if let Some(ref scaling_policy) = config.scaling {
            Arc::new(
                ShardManager::with_mapper(
                    config.num_shards,
                    config.ring_buffer_capacity,
                    config.backpressure_mode,
                    scaling_policy.clone(),
                )
                .map_err(|e| AdapterError::Fatal(e.to_string()))?,
            )
        } else {
            Arc::new(ShardManager::new(
                config.num_shards,
                config.ring_buffer_capacity,
                config.backpressure_mode,
            ))
        };

        // Create poll merger.
        //
        // Pass the live id set rather than the count. At
        // initial construction the ids are dense (`0..num_shards`),
        // but using `shard_ids()` here keeps a single code path with
        // the post-scaling re-stores below.
        let poll_merger = arc_swap::ArcSwap::from_pointee(PollMerger::new(
            adapter.clone(),
            shard_manager.shard_ids(),
        ));

        // Shutdown flag and drain-finalize gate. See `drain_finalize_ready`
        // doc on `EventBus` for the synchronization contract.
        let shutdown = Arc::new(AtomicBool::new(false));
        let drain_finalize_ready = Arc::new(AtomicBool::new(false));

        // Stats are shared with every BatchWorker spawn so successful
        // dispatches increment `batches_dispatched` / `events_dispatched`
        // — `flush()`'s Phase 2 progress probe gates on those.
        let stats = Arc::new(EventBusStats::default());

        // Producer nonce. Persistent path → load-or-create
        // the durable u64 so cross-process retries dedup against the
        // prior incarnation. No path → per-process default (today's
        // at-most-once-across-restart behavior).
        let producer_nonce = match &config.producer_nonce_path {
            Some(path) => crate::adapter::PersistentProducerNonce::load_or_create(path)
                .map_err(|e| {
                    AdapterError::Fatal(format!(
                        "failed to load/create producer-nonce file at {}: {e}",
                        path.display(),
                    ))
                })?
                .nonce(),
            None => crate::event::batch_process_nonce(),
        };

        // Create batch workers for each shard
        let mut batch_workers: std::collections::HashMap<u16, ShardWorkers> =
            std::collections::HashMap::with_capacity(config.num_shards as usize);
        let mut batch_senders =
            std::collections::HashMap::with_capacity(config.num_shards as usize);

        for shard_id in 0..config.num_shards {
            let (tx, rx) = mpsc::channel::<Vec<crate::event::InternalEvent>>(1024);

            let next_sequence = Arc::new(AtomicU64::new(0));

            let batch = spawn_batch_worker(BatchWorkerParams {
                shard_id,
                rx,
                adapter: adapter.clone(),
                shard_manager: shard_manager.clone(),
                config: config.batch.clone(),
                adapter_timeout: config.adapter_timeout,
                batch_retries: config.adapter_batch_retries,
                next_sequence: next_sequence.clone(),
                stats: stats.clone(),
                producer_nonce,
            });

            let drain = spawn_drain_worker_for_shard(
                shard_id,
                shard_manager.clone(),
                tx.clone(),
                shutdown.clone(),
                drain_finalize_ready.clone(),
            );

            batch_workers.insert(
                shard_id,
                ShardWorkers {
                    batch,
                    drain,
                    next_sequence,
                },
            );
            batch_senders.insert(shard_id, tx);
        }

        let bus = Self {
            shard_manager,
            adapter,
            poll_merger,
            poll_merger_swap_lock: parking_lot::Mutex::new(()),
            batch_workers: parking_lot::Mutex::new(batch_workers),
            batch_senders: parking_lot::RwLock::new(batch_senders),
            shutdown,
            drain_finalize_ready,
            in_flight_ingests: AtomicU64::new(0),
            shutdown_completed: AtomicBool::new(false),
            config,
            stats,
            producer_nonce,
            scaling_monitor: parking_lot::Mutex::new(None),
        };

        Ok(bus)
    }

    /// Start the scaling monitor (if dynamic scaling is enabled).
    /// This spawns a background task that periodically evaluates scaling decisions.
    ///
    /// The spawned task holds a `Weak<Self>` rather than a strong
    /// `Arc<Self>` clone. With a strong clone the task kept the bus
    /// alive forever, and `shutdown(self)` (which consumes by value)
    /// was unreachable: callers with an `Arc<EventBus>` could not
    /// `Arc::try_unwrap` to consume it because the spawned task always
    /// held one of the strong refs.
    ///
    /// With a `Weak`, the monitor task upgrades each tick. Once the
    /// last caller-held `Arc` is dropped, the upgrade fails and the
    /// task exits cleanly. To shut down via `shutdown(self)`, the
    /// caller must hold the only strong reference: `Arc::try_unwrap`
    /// on the resulting bus succeeds because the spawned task only
    /// holds a Weak.
    pub fn start_scaling_monitor(self: &Arc<Self>) {
        if self.config.scaling.is_none() {
            return;
        }

        // Idempotency check: no-op when a monitor is already
        // installed. Otherwise a second `start_scaling_monitor`
        // call would overwrite the slot without aborting the
        // previous `JoinHandle` — the displaced task would keep
        // running, holding a `Weak<EventBus>`, only exiting when it
        // next observed `shutdown` or failed to upgrade. Two
        // concurrent monitors would briefly compete for
        // `evaluate_scaling`'s lock, doubling the metrics-tick
        // wakeup rate.
        let mut slot = self.scaling_monitor.lock();
        if slot.is_some() {
            tracing::debug!("start_scaling_monitor: monitor already running, skipping");
            return;
        }

        let weak = Arc::downgrade(self);
        let handle = tokio::spawn(async move {
            run_scaling_monitor_via_weak(weak).await;
        });

        *slot = Some(handle);
    }

    /// Internal: Add a new shard with its workers.
    ///
    /// The previous implementation called `shard_manager.add_shard()`
    /// first, which atomically marked the shard `Active` and published
    /// it to the routing table. So `select_shard` could route producer
    /// pushes to the new id *before* any drain or batch worker existed,
    /// leaving events queued in a buffer with no consumer (and
    /// triggering the configured backpressure mode if the buffer
    /// filled).
    ///
    /// The fix uses the two-phase API on `ShardManager`:
    ///   1. `add_shard()` allocates the id and metrics collector,
    ///      adds the shard to the routing table in `Provisioning`
    ///      state — so `with_shard` works (which the drain worker
    ///      needs) but `select_shard` skips it.
    ///   2. Spawn batch + drain workers and register the sender.
    ///   3. `activate_shard()` flips state to `Active`. Only now
    ///      does `select_shard` start routing producer pushes.
    async fn add_shard_internal(&self) -> Result<u16, AdapterError> {
        self.add_shard_internal_with_cooldown_policy(false).await
    }

    /// Like [`add_shard_internal`] but bypasses the auto-scaling
    /// cooldown. See [`ShardManager::add_shard_force`].
    async fn add_shard_internal_force(&self) -> Result<u16, AdapterError> {
        self.add_shard_internal_with_cooldown_policy(true).await
    }

    async fn add_shard_internal_with_cooldown_policy(
        &self,
        force: bool,
    ) -> Result<u16, AdapterError> {
        // Step 1: provisioning add — not yet selectable.
        let new_id = if force {
            self.shard_manager.add_shard_force()
        } else {
            self.shard_manager.add_shard()
        }
        .map_err(|e| AdapterError::Fatal(e.to_string()))?;

        // Step 2: spawn workers and register the sender.
        let (tx, rx) = mpsc::channel::<Vec<crate::event::InternalEvent>>(1024);

        let next_sequence = Arc::new(AtomicU64::new(0));

        let batch = spawn_batch_worker(BatchWorkerParams {
            shard_id: new_id,
            rx,
            adapter: self.adapter.clone(),
            shard_manager: self.shard_manager.clone(),
            config: self.config.batch.clone(),
            adapter_timeout: self.config.adapter_timeout,
            batch_retries: self.config.adapter_batch_retries,
            next_sequence: next_sequence.clone(),
            stats: self.stats.clone(),
            producer_nonce: self.producer_nonce,
        });

        self.batch_senders.write().insert(new_id, tx.clone());

        let drain = spawn_drain_worker_for_shard(
            new_id,
            self.shard_manager.clone(),
            tx,
            self.shutdown.clone(),
            self.drain_finalize_ready.clone(),
        );

        self.batch_workers.lock().insert(
            new_id,
            ShardWorkers {
                batch,
                drain,
                next_sequence,
            },
        );

        // Step 3: workers are live — flip the shard to Active so
        // `select_shard` will route to it.
        //
        // On `activate_shard` failure we mirror
        // `remove_shard_internal`'s teardown: drop the sender,
        // unmap the provisioning entry (which atomically pops any
        // residual ring-buffer events), gracefully await both
        // workers (so the drain worker's `scratch` Vec sends its
        // contents on the channel and the batch worker's
        // `current_batch` is flushed via the channel-close path),
        // then dispatch any stranded ring-buffer events through
        // the adapter. Pre-fix this used `.abort()` on both
        // handles which dropped the drain
        // worker's scratch and the batch worker's current_batch
        // without dispatch.
        if let Err(e) = self.shard_manager.activate_shard(new_id) {
            tracing::warn!(
                shard_id = new_id,
                error = %e,
                "activate_shard failed; rolling back provisioning state"
            );

            // 1. Drop the bus-side sender. The drain worker still
            //    holds its own clone, so the channel stays open
            //    until `with_shard` returns None (step 2) and the
            //    drain worker breaks out of its loop, dropping
            //    its sender and finally closing the channel.
            self.batch_senders.write().remove(&new_id);

            // 2. Atomically pop any ring-buffer residue and
            //    unmap the Provisioning entry. After this,
            //    `with_shard(new_id)` returns None and the drain
            //    worker exits at its next poll (after sending
            //    any events it had already popped into `scratch`
            //    on this iteration).
            //
            //    For a brand-new Provisioning shard the buffer
            //    should be empty (`select_shard` skips
            //    non-Active states), so `stranded` is normally
            //    `Vec::new()`. The flush below is a defensive
            //    no-op on the happy path but covers any future
            //    code path that routes to a Provisioning shard
            //    or any race window that tucked an event in
            //    before `activate_shard` returned its error.
            let stranded = self.shard_manager.remove_shard(new_id).unwrap_or_default();

            // 3. Take ownership of the worker handles and await
            //    them gracefully. Order: drain first (it pumps
            //    its scratch + final-sweep contents into the
            //    channel and exits), then batch (which receives
            //    those events plus any prior channel residue,
            //    flushes its own `current_batch`, and exits).
            //
            //    Awaiting in this order is what makes the drain
            //    worker's scratch reach the adapter — the
            //    drain's `Some(N>0)` arm `mem::replace`s scratch
            //    into a batch and `sender.send(batch).await`s
            //    it; that send must complete (or fail) before
            //    the drain worker breaks. The batch worker's
            //    `Ok(None)` arm then runs after both senders
            //    are dropped and flushes any pending batch.
            // Bound each JoinHandle await so a worker that's
            // parked inside a slow adapter call (e.g. drain worker
            // mid-`sender.send().await` against a backpressured
            // channel because the batch worker is itself blocked
            // in the adapter) cannot pin rollback indefinitely.
            // 2x `adapter_timeout` is the natural ceiling: the
            // batch worker uses `adapter_timeout` per dispatch and
            // is expected to wake within that window. A timeout
            // here leaks the JoinHandle — acceptable because
            // step 1 already removed the bus-side sender, so the
            // detached task can't observe new work and will exit
            // on its next loop iteration.
            let workers = self.batch_workers.lock().remove(&new_id);
            // `worker_detached` is set when either join times out
            // — the worker is still running on a leaked
            // JoinHandle. In that case the `next_sequence` atomic
            // is no longer a reliable upper bound: the detached
            // worker may publish a final batch whose msg-ids land
            // in the same `[next_sequence..N]` range we'd use for
            // the stranded-flush, producing duplicate XADDs or
            // JetStream dedup hits. Skip the stranded-flush when
            // detached and surface the loss explicitly so the
            // operator sees it.
            let mut worker_detached = false;
            let final_next_sequence = if let Some(workers) = workers {
                let bound = self.config.adapter_timeout.saturating_mul(2);
                match tokio::time::timeout(bound, workers.drain).await {
                    Ok(Ok(())) => {}
                    Ok(Err(err)) => {
                        tracing::warn!(
                            shard_id = new_id,
                            error = %err,
                            "drain worker JoinHandle errored on activate-failure rollback"
                        );
                    }
                    Err(_) => {
                        tracing::warn!(
                            shard_id = new_id,
                            timeout_ms = bound.as_millis() as u64,
                            "drain worker did not exit within timeout on activate-failure rollback; detaching"
                        );
                        worker_detached = true;
                    }
                }
                match tokio::time::timeout(bound, workers.batch).await {
                    Ok(Ok(())) => {}
                    Ok(Err(err)) => {
                        tracing::warn!(
                            shard_id = new_id,
                            error = %err,
                            "BatchWorker JoinHandle errored on activate-failure rollback"
                        );
                    }
                    Err(_) => {
                        tracing::warn!(
                            shard_id = new_id,
                            timeout_ms = bound.as_millis() as u64,
                            "BatchWorker did not exit within timeout on activate-failure rollback; detaching"
                        );
                        worker_detached = true;
                    }
                }
                workers.next_sequence.load(AtomicOrdering::Acquire)
            } else {
                0
            };

            // 4. Dispatch any stranded events as a single-shot
            //    batch so they reach durable storage with the
            //    correct sequence-id segment. Identical to the
            //    `remove_shard_internal` teardown — but only when
            //    the worker actually exited. If it timed out and
            //    is still running on a leaked handle, dispatching
            //    here would emit msg-ids overlapping the worker's
            //    final flush; surface the events as dropped
            //    instead so the duplicate-on-the-wire hazard is
            //    avoided.
            if !stranded.is_empty() && worker_detached {
                let count = stranded.len();
                self.stats
                    .events_dropped
                    .fetch_add(count as u64, AtomicOrdering::Relaxed);
                self.stats
                    .shutdown_was_lossy
                    .store(true, AtomicOrdering::Release);
                tracing::error!(
                    shard_id = new_id,
                    count,
                    "activate-failure rollback: skipping stranded-flush \
                     because a worker JoinHandle timed out and may still \
                     be running; events would collide with the detached \
                     worker's final batch on the wire. Counted as dropped."
                );
            } else if !stranded.is_empty() {
                let count = stranded.len();
                let batch = crate::event::Batch::with_nonce(
                    new_id,
                    stranded,
                    final_next_sequence,
                    self.producer_nonce,
                );
                let dispatched = dispatch_batch(
                    &*self.adapter,
                    batch,
                    new_id,
                    self.config.adapter_timeout,
                    self.config.adapter_batch_retries,
                )
                .await;
                if dispatched {
                    self.stats
                        .batches_dispatched
                        .fetch_add(1, AtomicOrdering::Relaxed);
                    self.stats
                        .events_dispatched
                        .fetch_add(count as u64, AtomicOrdering::Relaxed);
                    tracing::info!(
                        shard_id = new_id,
                        count,
                        "activate-failure rollback: flushed stranded events to adapter",
                    );
                } else {
                    tracing::error!(
                        shard_id = new_id,
                        count,
                        "activate-failure rollback: adapter rejected stranded events; \
                         events lost"
                    );
                }
            }

            return Err(AdapterError::Fatal(e.to_string()));
        }

        // Update poll merger with the post-add id set. Hold
        // `poll_merger_swap_lock` across the snapshot-and-store so a
        // concurrent remove_shard can't sneak between our `shard_ids()`
        // read and our `arc_swap.store` and clobber the published view
        // with a stale snapshot.
        {
            let _swap_guard = self.poll_merger_swap_lock.lock();
            self.poll_merger.store(Arc::new(PollMerger::new(
                self.adapter.clone(),
                self.shard_manager.shard_ids(),
            )));
        }

        tracing::info!(shard_id = new_id, "Added new shard");
        Ok(new_id)
    }

    /// Internal: Remove a stopped shard.
    ///
    /// Previously this dropped the worker `JoinHandle`s and unmapped
    /// the shard without first draining its ring buffer. Any events
    /// still queued at the moment of removal — even just a few from a
    /// producer that pushed concurrently with the scale-down decision
    /// — were silently stranded once the drain worker exited on
    /// `with_shard → None`.
    ///
    /// The fix:
    ///   1. Wait for the drain worker we're about to retire to flush
    ///      the channel, by closing the bus-side sender first.
    ///   2. Call `remove_shard`, which atomically pops the
    ///      ring-buffer remnants and unmaps the shard. The drained
    ///      events come back to us as a `Vec`.
    ///   3. Hand those events directly to the adapter as a
    ///      single-shot batch — bypassing the per-shard pipeline
    ///      that's already being torn down — so they reach durable
    ///      storage.
    async fn remove_shard_internal(&self, shard_id: u16) -> Result<(), AdapterError> {
        // Step 1: drop the bus-side sender. The drain worker still
        // has its own clone and will keep draining; we want it to
        // exit when its `with_shard` call returns `None` after
        // step 2's unmap.
        self.batch_senders.write().remove(&shard_id);

        // Step 2: atomically drain whatever's in the ring buffer and
        // unmap. After this, `with_shard(shard_id)` returns `None`
        // and the drain worker exits at its next poll.
        let stranded = self
            .shard_manager
            .remove_shard(shard_id)
            .map_err(|e| AdapterError::Fatal(e.to_string()))?;

        // Step 3: take ownership of the worker handles (move them
        // OUT of the mutex map so we can `await` them — `await`
        // consumes a `JoinHandle`). With the bus-side sender already
        // dropped (step 1) and the shard unmapped (step 2), the
        // drain worker exits at its next poll and drops its sender
        // clone, which closes the BatchWorker's `rx`. The
        // BatchWorker then flushes any pending `current_batch` and
        // any events still buffered in the channel, dispatches them
        // via the standard `dispatch_batch` path with their PROPER
        // `next_sequence` values, and exits.
        //
        // Await order: drain first, then batch. The drain worker's
        // `Some(N>0)` arm `mem::replace`s scratch into a batch and
        // `sender.send(batch).await`s it; that send must complete
        // (or fail) before drain breaks. The batch worker's
        // `Ok(None)` arm runs after the sender drops and flushes
        // any pending batch. Awaiting in the reverse order would
        // park here forever: the batch worker's `recv()` only
        // returns `None` once every sender clone (including the
        // drain worker's) has dropped.
        //
        // Both awaits are bounded by `2 × adapter_timeout` so a
        // worker parked inside a slow adapter call cannot pin
        // teardown indefinitely. A timeout leaks the JoinHandle —
        // acceptable because step 1 already removed the bus-side
        // sender and step 2 unmapped the shard, so the detached
        // task can't observe new work and will exit on its next
        // loop iteration.
        let workers = self.batch_workers.lock().remove(&shard_id);
        let final_next_sequence = if let Some(workers) = workers {
            let bound = self.config.adapter_timeout.saturating_mul(2);
            match tokio::time::timeout(bound, workers.drain).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    tracing::warn!(
                        shard_id,
                        error = %e,
                        "drain worker JoinHandle errored on await; \
                         drain worker should have already exited via \
                         `with_shard -> None`",
                    );
                }
                Err(_) => {
                    tracing::warn!(
                        shard_id,
                        timeout_ms = bound.as_millis() as u64,
                        "drain worker did not exit within timeout on remove_shard; detaching",
                    );
                }
            }
            match tokio::time::timeout(bound, workers.batch).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    tracing::warn!(
                        shard_id,
                        error = %e,
                        "BatchWorker JoinHandle errored on await; \
                         proceeding with stranded-flush using last \
                         published next_sequence",
                    );
                }
                Err(_) => {
                    tracing::warn!(
                        shard_id,
                        timeout_ms = bound.as_millis() as u64,
                        "BatchWorker did not exit within timeout on remove_shard; detaching",
                    );
                }
            }
            workers.next_sequence.load(AtomicOrdering::Acquire)
        } else {
            // No worker registered for this shard — shouldn't
            // happen on the normal scale-down path, but defensively
            // fall back to 0. This branch only activates if a
            // caller manages to call `remove_shard_internal` for a
            // shard that was never spawned (or already removed).
            0
        };

        // Step 4: flush the stranded ring-buffer events through the
        // adapter in one shot, using `final_next_sequence` (NOT 0)
        // as the `sequence_start`. The stranded batch's msg-ids
        // are `{nonce}:{shard_id}:{final_next_sequence}:{i}` —
        // strictly past every msg-id the worker emitted. Using 0
        // would collide with the worker's very first batch
        // (`{nonce}:{shard_id}:0:{i}`), and JetStream's 2 min dedup
        // window would silently drop the duplicate.
        if !stranded.is_empty() {
            let count = stranded.len();
            // Use the bus's loaded producer nonce so the stranded
            // batch's msg-ids share the same producer-identity
            // segment as everything else this bus has emitted —
            // critical for cross-process dedup.
            let batch = crate::event::Batch::with_nonce(
                shard_id,
                stranded,
                final_next_sequence,
                self.producer_nonce,
            );
            let dispatched = dispatch_batch(
                &*self.adapter,
                batch,
                shard_id,
                self.config.adapter_timeout,
                self.config.adapter_batch_retries,
            )
            .await;
            if dispatched {
                self.stats
                    .batches_dispatched
                    .fetch_add(1, AtomicOrdering::Relaxed);
                self.stats
                    .events_dispatched
                    .fetch_add(count as u64, AtomicOrdering::Relaxed);
                tracing::info!(
                    shard_id,
                    count,
                    sequence_start = final_next_sequence,
                    "Removed shard: flushed stranded ring-buffer events to adapter"
                );
            } else {
                tracing::error!(
                    shard_id,
                    count,
                    sequence_start = final_next_sequence,
                    "Removed shard: adapter rejected stranded ring-buffer events; \
                     events lost"
                );
            }
        }

        // Update poll merger with the post-remove id set.
        // Without this, a default-shards poll (`request.shards == None`)
        // would still iterate `0..num_shards` and skip the live shard
        // whose id is now the largest, while polling a nonexistent /
        // recreated shard at the bottom of the range.
        //
        // `poll_merger_swap_lock` serializes against
        // `add_shard_internal`'s matching block — see the field doc.
        {
            let _swap_guard = self.poll_merger_swap_lock.lock();
            self.poll_merger.store(Arc::new(PollMerger::new(
                self.adapter.clone(),
                self.shard_manager.shard_ids(),
            )));
        }

        tracing::info!(shard_id = shard_id, "Removed shard");
        Ok(())
    }

    /// Try to enter an ingest critical section. Returns `None` if
    /// shutdown is in progress, in which case the caller must
    /// return `IngestionError::ShuttingDown` without touching the
    /// shard manager.
    ///
    /// The `fetch_add` + load(`shutdown`) sequence pairs with
    /// `shutdown()`'s store(`shutdown=true`) + wait-for-zero on
    /// `in_flight_ingests`. SeqCst on both sides closes the
    /// stranding race: every ingest that is observed as in-flight
    /// during shutdown's wait is guaranteed to complete before the
    /// drain workers do their final ring-buffer sweep, so no event
    /// can land in a ring buffer after the drain worker has stopped
    /// reading from it.
    #[inline(always)]
    fn try_enter_ingest(&self) -> Option<IngestGuard<'_>> {
        self.in_flight_ingests.fetch_add(1, AtomicOrdering::SeqCst);
        if self.shutdown.load(AtomicOrdering::SeqCst) {
            self.in_flight_ingests.fetch_sub(1, AtomicOrdering::SeqCst);
            return None;
        }
        Some(IngestGuard { bus: self })
    }

    /// Ingest an event.
    ///
    /// This is a non-blocking operation. The event is added to the appropriate
    /// shard's ring buffer and will be batched and persisted asynchronously.
    ///
    /// # Returns
    ///
    /// The shard ID and insertion timestamp on success.
    #[inline]
    pub fn ingest(&self, event: Event) -> IngestionResult<(u16, u64)> {
        let _g = self
            .try_enter_ingest()
            .ok_or(IngestionError::ShuttingDown)?;

        match self.shard_manager.ingest(event.into_inner()) {
            Ok((shard_id, ts)) => {
                self.stats
                    .events_ingested
                    .fetch_add(1, AtomicOrdering::Relaxed);
                Ok((shard_id, ts))
            }
            Err(e) => {
                self.stats
                    .events_dropped
                    .fetch_add(1, AtomicOrdering::Relaxed);
                Err(e)
            }
        }
    }

    /// Ingest a raw event (pre-serialized with cached hash).
    ///
    /// This is the fastest ingestion path:
    /// - Uses pre-computed hash for shard selection (no serialization)
    /// - Stores bytes directly (no clone needed, reference-counted)
    ///
    /// # Returns
    ///
    /// The shard ID and insertion timestamp on success.
    #[inline]
    pub fn ingest_raw(&self, event: RawEvent) -> IngestionResult<(u16, u64)> {
        let _g = self
            .try_enter_ingest()
            .ok_or(IngestionError::ShuttingDown)?;

        match self.shard_manager.ingest_raw(event) {
            Ok((shard_id, ts)) => {
                self.stats
                    .events_ingested
                    .fetch_add(1, AtomicOrdering::Relaxed);
                Ok((shard_id, ts))
            }
            Err(e) => {
                self.stats
                    .events_dropped
                    .fetch_add(1, AtomicOrdering::Relaxed);
                Err(e)
            }
        }
    }

    /// Ingest a batch of events.
    ///
    /// This is more efficient than calling `ingest` repeatedly: events
    /// destined for the same shard share a single mutex acquisition.
    ///
    /// # Returns
    ///
    /// The number of successfully ingested events.
    pub fn ingest_batch(&self, events: Vec<Event>) -> usize {
        // The shutdown gate lives in `ingest_raw_batch`, which we
        // forward to. No separate guard here — that would double-
        // count `in_flight_ingests` (once for this call, once for
        // the inner call) and could deadlock shutdown under high
        // contention.
        let raw: Vec<RawEvent> = events.into_iter().map(|e| e.into_raw()).collect();
        self.ingest_raw_batch(raw)
    }

    /// Ingest a batch of raw events (fastest batch ingestion).
    ///
    /// Groups events by their destination shard and pushes each group
    /// under a single mutex acquisition.
    ///
    /// # Returns
    ///
    /// The number of successfully ingested events.
    pub fn ingest_raw_batch(&self, events: Vec<RawEvent>) -> usize {
        let _g = match self.try_enter_ingest() {
            Some(g) => g,
            None => return 0,
        };

        let total = events.len();
        let (success, unrouted) = self.shard_manager.ingest_raw_batch(events);
        if success > 0 {
            self.stats
                .events_ingested
                .fetch_add(success as u64, AtomicOrdering::Relaxed);
        }
        // Subtract `unrouted` from the buffer-fullness drop count
        // so the same event isn't tallied in both `events_unrouted`
        // (bumped inside `ShardManager::ingest_raw_batch`) and
        // `events_dropped` — using a plain `total - success` here
        // would double-count unrouted events. Backpressure drops
        // = events that reached a shard but failed to push.
        let dropped = total.saturating_sub(success).saturating_sub(unrouted);
        if dropped > 0 {
            self.stats
                .events_dropped
                .fetch_add(dropped as u64, AtomicOrdering::Relaxed);
        }
        success
    }

    /// Poll events from the bus.
    ///
    /// This retrieves events from storage according to the request parameters.
    ///
    /// # Topology-change visibility
    ///
    /// `ArcSwap::load()` snapshots the current `PollMerger` for
    /// the duration of this call. A concurrent `add_shard` /
    /// `remove_shard_internal` that `.store()`s a fresh merger
    /// only affects **subsequent** polls — this poll continues
    /// against the loaded snapshot's shard list.
    ///
    /// The implications:
    ///   - **add_shard mid-poll:** events ingested into the new
    ///     shard between the merger swap and our return are
    ///     invisible to this call. They appear on the next poll.
    ///   - **remove_shard_internal mid-poll:** the stale merger
    ///     still has the removed shard in its id list. Adapters
    ///     that lazy-create streams on `poll_shard` (JetStream
    ///     in particular) may recreate the stream and return
    ///     empty/stale data. The drained events are dispatched
    ///     to durable storage by `remove_shard_internal` itself
    ///     before this poll's adapter call lands; the next poll
    ///     loads the new merger and sees the correct shard set.
    ///
    /// In both cases the loss is transient and self-healing:
    /// pagination via `next_id` and the next poll's cursor pick
    /// up where we left off. Callers requiring strict
    /// "topology-stable" semantics should serialize their polls
    /// against scaling operations externally.
    pub async fn poll(&self, request: ConsumeRequest) -> Result<ConsumeResponse, ConsumerError> {
        let merger = self.poll_merger.load();
        merger.poll(request).await
    }

    /// Get the number of shards.
    pub fn num_shards(&self) -> u16 {
        self.shard_manager.num_shards()
    }

    /// Get the adapter name.
    pub fn adapter_name(&self) -> &'static str {
        self.adapter.name()
    }

    /// Check if the adapter is healthy.
    pub async fn is_healthy(&self) -> bool {
        self.adapter.is_healthy().await
    }

    /// Get statistics.
    pub fn stats(&self) -> &EventBusStats {
        &self.stats
    }

    /// Get shard statistics.
    pub fn shard_stats(&self) -> crate::shard::ShardStats {
        self.shard_manager.stats()
    }

    /// Sum of `len()` across every shard's ring buffer.
    ///
    /// Mainly useful in tests and operational diagnostics: a
    /// non-zero value at the time of `Drop` (without an awaited
    /// `shutdown()`) would be silently lost, so `Drop` folds this
    /// into `events_dropped` before the bus disappears.
    pub fn pending_in_rings(&self) -> u64 {
        self.shard_manager.total_pending_in_rings()
    }

    /// Flush all pending batches.
    ///
    /// Waits for all shard ring buffers to drain, then for the
    /// per-shard mpsc channels to drain, then for any pending batch
    /// inside each batch worker to time out and dispatch — and only
    /// then calls `adapter.flush()`.
    ///
    /// # Latency bound
    ///
    /// The total wall-clock budget is the sum of three phases:
    ///   * Phase 1 (ring-buffer drain): up to **5 s**.
    ///   * Phase 2 (channel + pending-batch drain): up to
    ///     `min(2 s, batch.max_delay × n_workers)` — capped at 2 s
    ///     so a misconfigured `max_delay` cannot inflate the budget.
    ///   * Phase 3 (`adapter.flush()` call): up to `adapter_timeout`
    ///     (default **30 s**).
    ///
    /// Worst-case `flush()` runtime is therefore **~37 s under
    /// default config**, NOT 5 s as an earlier doc-comment stated.
    /// Callers wiring `flush()` into request-path latencies (HTTP
    /// handler, RPC) MUST set `adapter_timeout` accordingly or run
    /// the flush under their own outer timeout. The 5-second figure
    /// describes Phase 1 only; the doc was misleading and is fixed
    /// here.
    ///
    /// The previous implementation slept a single `batch.max_delay`
    /// (default 10 ms) after the ring buffers drained and immediately
    /// called `adapter.flush()`. Events still in transit through the
    /// per-shard mpsc channel, the batch worker's pending batch, or
    /// the in-progress `adapter.on_batch` call (bounded only by
    /// `adapter_timeout`, default 30 s) could miss the flush. Callers
    /// using `flush()` as a delivery barrier silently lost events.
    pub async fn flush(&self) -> Result<(), AdapterError> {
        let start = tokio::time::Instant::now();
        let timeout = Duration::from_secs(5);
        let mut backoff = Duration::from_micros(100);

        // Phase 1: wait for ring buffers to drain (drain workers
        // pump them into the per-shard mpsc channels).
        loop {
            if self.shard_manager.all_shards_empty() {
                break;
            }
            if start.elapsed() >= timeout {
                tracing::warn!("flush: ring buffers not fully drained after {:?}", timeout);
                break;
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_millis(10));
        }

        // Phase 2: wait until every event ingested before flush()
        // started has been handed to the adapter via `on_batch`.
        // Snapshot the `events_ingested` counter at flush entry —
        // that's our target. We then poll `events_dispatched` (sum
        // of batch lengths from successful adapter dispatches) plus
        // `events_dropped` (events the adapter rejected after retry
        // exhaustion or that never made it past backpressure). The
        // barrier is met when `events_dispatched + events_dropped >=
        // target`: every pre-flush ingest is accounted for in one
        // bucket or the other.
        //
        // This is a true delivery barrier — it doesn't rely on
        // "no progress this window" heuristics that race a
        // BatchWorker whose `batch_start` was set just before
        // flush() ran. A progress gate that reads
        // `batches_dispatched` only works if that counter is
        // actually incremented on every dispatch, and Windows
        // timer resolution alone has historically made any
        // single-`max_delay`-sleep approach a frequent flake.
        let target_ingested = self.stats.events_ingested.load(AtomicOrdering::Acquire);
        let dropped_at_start = self.stats.events_dropped.load(AtomicOrdering::Acquire);
        let dispatched_at_start = self.stats.events_dispatched.load(AtomicOrdering::Acquire);

        // Outer deadline still bounds Phase 2 in case a wedged
        // adapter never returns. `max_delay * num_workers` is the
        // worst-case shape (one partially-filled batch per worker,
        // each waiting its full `max_delay` to time out), capped at
        // 2 s — same upper bound as before.
        //
        // Read the worker count via the shard manager's atomic-
        // backed `num_shards()` rather than `batch_workers.lock()
        // .len()`. The previous spinlock-backed `.lock()` inside
        // an `async fn` could stall the runtime worker thread
        // under contention with concurrent `add_shard_internal` /
        // `remove_shard_internal` callers; the atomic accessor is
        // both faster and async-safe. Mismatch in the
        // worker-count vs shard-count snapshot only changes the
        // phase2 deadline by at most one `max_delay` step, which
        // is bounded by the outer 2s cap regardless.
        let n_workers = usize::from(self.shard_manager.num_shards());
        let phase2_budget = self
            .config
            .batch
            .max_delay
            .saturating_mul(n_workers.max(1) as u32);
        let phase2_deadline =
            tokio::time::Instant::now() + phase2_budget.min(Duration::from_secs(2));

        // Inner poll cadence: re-check the counters every 1 ms (or
        // `max_delay / 16`, whichever is larger). The fast cadence
        // means we exit promptly once the BatchWorker dispatches,
        // rather than waking exactly once per `max_delay` and
        // potentially racing the dispatch by a few ms.
        let poll_interval = (self.config.batch.max_delay / 16).max(Duration::from_millis(1));
        loop {
            let dispatched = self.stats.events_dispatched.load(AtomicOrdering::Acquire);
            let dropped = self.stats.events_dropped.load(AtomicOrdering::Acquire);
            // The barrier: every event ingested pre-flush has been
            // either dispatched or dropped. The bus's invariant
            // `events_ingested = events_dispatched + events_dropped`
            // holds at quiescence; we wait until
            // `dispatched + dropped >= target_ingested`.
            //
            // Pre-fix this used `dispatched + (dropped -
            // dropped_at_start) >= target_ingested`, which under-
            // counted by `dropped_at_start`: even after every
            // pre-flush event was processed, the inequality
            // required `dropped_at_start` MORE post-flush events
            // before signalling done. Workloads with no post-flush
            // ingest hung at the barrier until the deadline fired.
            //
            // Cross-shard race remains: a fast shard's post-flush
            // dispatches can satisfy the global target while a
            // slow shard's pre-flush events linger in its mpsc
            // channel or pending batch. `all_shards_empty()`
            // checks ring buffers but not those downstream
            // queues. Operators relying on flush as a hard
            // delivery barrier should call it during quiet
            // ingest, or in `shutdown` (which gates ingest via
            // `try_enter_ingest`).
            let _ = dispatched_at_start; // reserved for future per-shard accounting
            if dispatched.saturating_add(dropped) >= target_ingested
                && self.shard_manager.all_shards_empty()
            {
                break;
            }
            if tokio::time::Instant::now() >= phase2_deadline {
                tracing::warn!(
                    target = target_ingested,
                    dispatched,
                    dropped = dropped.saturating_sub(dropped_at_start),
                    "flush: Phase 2 deadline reached before all ingested events were dispatched",
                );
                break;
            }
            tokio::time::sleep(poll_interval).await;
        }

        // Phase 3: tell the adapter to flush whatever it has
        // buffered. Bounded by `adapter_timeout` so a hanging
        // adapter can't pin us forever.
        match tokio::time::timeout(self.config.adapter_timeout, self.adapter.flush()).await {
            Ok(r) => r,
            Err(_) => {
                tracing::warn!(
                    "flush: adapter.flush timed out after {:?}",
                    self.config.adapter_timeout
                );
                Err(AdapterError::Fatal("adapter flush timed out".into()))
            }
        }
    }

    /// Gracefully shut down the event bus.
    ///
    /// The shutdown order is load-bearing:
    ///
    ///   1. Signal `shutdown` so drain workers stop pulling from
    ///      ring buffers after their final sweep.
    ///   2. Await **drain workers** so every event the producer
    ///      has handed to the bus is now in the per-shard mpsc
    ///      channel.
    ///   3. Drop `batch_senders` so each channel's last sender is
    ///      gone — the next `recv().await` in a batch worker will
    ///      return `None`.
    ///   4. Await **batch workers**, which drain everything
    ///      remaining in their channel and exit on `recv() = None`.
    ///
    /// Reversing steps 2 and 4 (the previous design) silently
    /// dropped events: a batch worker that exited on the shutdown
    /// flag could leave events the drain worker pushed *after* its
    /// `try_recv` sweep stranded in the channel.
    pub async fn shutdown(self) -> Result<(), AdapterError> {
        self.shutdown_via_ref().await
    }

    /// Shutdown via shared reference — same semantics as
    /// [`shutdown`](Self::shutdown), but does not consume `self`.
    ///
    /// Useful for callers that hold the bus behind `Arc<EventBus>`
    /// (e.g., the SDK, where `subscribe` perpetuates an Arc clone
    /// into every `EventStream`) and therefore cannot satisfy
    /// `Arc::try_unwrap`. Idempotent: the first caller does the
    /// work; concurrent or subsequent callers wait for the
    /// `shutdown_completed` flag and return `Ok(())`.
    pub async fn shutdown_via_ref(&self) -> Result<(), AdapterError> {
        // 1. CAS the shutdown flag false→true. SeqCst pairs with
        // `try_enter_ingest`'s shutdown check — any producer that
        // observed the previous `false` and is mid-push has its
        // `in_flight_ingests` increment ordered before this store
        // (the CAS-success branch is a release of the new `true`),
        // and so will be visible to the wait below.
        //
        // If the CAS loses (someone else — typically a concurrent
        // call or `Drop` — already flipped the flag), spin until
        // they finish. We can't run the rest of the body because
        // workers/senders may already be partially torn down.
        if self
            .shutdown
            .compare_exchange(false, true, AtomicOrdering::SeqCst, AtomicOrdering::SeqCst)
            .is_err()
        {
            // Bound the wait so a `Drop`-only path (which sets
            // `shutdown=true` but never sets `shutdown_completed`)
            // doesn't spin forever.
            //
            // Distinguish the two outcomes for callers. If
            // `shutdown_completed` flips inside the window, we
            // return `Ok(())` and the caller can be sure the first
            // caller finished. If the deadline fires first, we
            // surface `AdapterError::Transient(_)` — the bus IS
            // being shut down (the flag is set), but completion is
            // not yet observable; the caller can treat this as
            // "another thread is working on it, retry the
            // is_shutdown_completed() poll if you need a hard
            // barrier."
            //
            // Returning `Ok(())` in both branches would let
            // shutdown-done assumptions silently drift under a slow
            // adapter (`adapter_timeout` default 30 s > the 10 s
            // spin deadline), letting subsequent code observe a
            // partially-shut-down bus.
            // 10s in production builds; overridable via the
            // `_TEST_OVERRIDE_SHUTDOWN_VIA_REF_DEADLINE` thread-local
            // in test builds so the slow-first-caller test doesn't
            // need to wall-clock-wait for the full deadline. The
            // override is `#[cfg(test)]`-only; production cargo
            // builds compile out the override entirely.
            let deadline_dur = shutdown_via_ref_spin_deadline();
            // Use `tokio::time::Instant` so tests using
            // `tokio::time::pause()` virtualize this clock too.
            // Pre-fix `std::time::Instant` was wall-clock and
            // ignored `pause()`, breaking timeout-bounded tests
            // that wanted to fast-forward the spin deadline.
            let deadline = tokio::time::Instant::now() + deadline_dur;
            while !self.shutdown_completed.load(AtomicOrdering::Acquire) {
                if tokio::time::Instant::now() >= deadline {
                    return Err(AdapterError::Transient(
                        "shutdown_via_ref: another caller is mid-shutdown; \
                         deadline elapsed before shutdown_completed \
                         flipped. The bus IS shutting down; poll \
                         is_shutdown_completed() if you need a hard \
                         barrier."
                            .into(),
                    ));
                }
                // `yield_now` re-queues immediately and keeps the
                // task hot, starving the workers we're waiting on
                // under contention. A short `sleep` parks the task
                // and lets the runtime schedule the workers.
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
            return Ok(());
        }

        // 1a. Wait for in-flight ingests to drain BEFORE the drain
        // workers do their final ring-buffer sweep. Otherwise a
        // producer that observed `shutdown=false` could push *after*
        // the drain worker's last `pop_batch_into` returned zero,
        // leaving the event stranded in the ring buffer when the bus
        // is dropped.
        //
        // This is bounded: every producer either bails on the
        // SeqCst-synchronized shutdown check (no progress past the
        // increment) or completes its single non-blocking push and
        // decrements. Both paths take constant time; the total
        // wait is O(producer threads).
        //
        // The "every observed in-flight ingest completes before
        // the final sweep" property holds under normal conditions,
        // but the 5-second deadline below forces the gate open
        // even when producers are still in their push window. A
        // producer that has incremented `in_flight_ingests` (and
        // so observed `shutdown=false`) but whose push is delayed
        // past the deadline (heavy contention, debugger hit, etc.)
        // will complete its push AFTER the final sweep — its event
        // lands in the ring buffer and is never read. The deadline
        // exists so a stuck producer can't deadlock shutdown
        // indefinitely; the trade-off is documented data loss past
        // the 5 s window, surfaced via the `events_dropped` stat
        // (so the loss is observable to operators) and the `WARN`
        // log below (so it's diagnosable). The "no stranding"
        // promise on the happy path stands; the deadline path is
        // the documented escape hatch.
        // Use `tokio::time::Instant` so tests using
        // `tokio::time::pause()` virtualize this 5-second
        // deadline too — pre-fix the `std::time::Instant`
        // was wall-clock and ignored the test's paused clock.
        let in_flight_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        // Snapshot of `(stranded, ingested, dispatched)` at the
        // deadline — Some(...) iff we hit the deadline path. The
        // post-drain reconciliation reads this to compute the
        // actual drop count (see comment further down).
        let mut deadline_snapshot: Option<(u64, u64, u64)> = None;
        while self.in_flight_ingests.load(AtomicOrdering::SeqCst) > 0 {
            if tokio::time::Instant::now() >= in_flight_deadline {
                let stranded = self.in_flight_ingests.load(AtomicOrdering::SeqCst);
                let ingested_now = self.stats.events_ingested.load(AtomicOrdering::Acquire);
                let dispatched_now = self.stats.events_dispatched.load(AtomicOrdering::Acquire);
                tracing::warn!(
                    in_flight = stranded,
                    lossy = true,
                    "shutdown timed out waiting for in-flight ingests after 5s; \
                     proceeding — up to {} events may strand in the ring buffer \
                     past final drain (documented data-loss path)",
                    stranded,
                );
                // Set the lossy flag immediately so a fast `is_*`
                // poll observes the outcome before the drain
                // finishes. The actual `events_dropped` bump is
                // deferred until after the final drain runs (see
                // "post-drain reconciliation" below) so we don't
                // double-count events that the drain still
                // successfully delivers — pre-fix this bumped
                // `events_dropped += stranded` here and the same
                // events that the final sweep then drained landed
                // in BOTH `events_ingested` and `events_dropped`,
                // breaking the bus's
                // `ingested == dispatched + dropped` invariant
                // and turning `shutdown_was_lossy` into a false
                // positive on every deadline-triggered shutdown.
                self.stats
                    .shutdown_was_lossy
                    .store(true, AtomicOrdering::Release);
                deadline_snapshot = Some((stranded, ingested_now, dispatched_now));
                break;
            }
            // Park instead of `yield_now`. The producers we're
            // waiting on contend for the same runtime threads;
            // re-queuing immediately starves their progress.
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }

        // 1b. Release the drain-finalize gate.
        //
        // Pre-fix used `Ordering::Release` for this store
        // and relied on the SeqCst spin above (loading
        // `in_flight_ingests`) to provide the happens-before for
        // every observed-pre-shutdown push. That works today
        // because SeqCst loads carry an implicit fence, but a
        // future change to the spin's ordering (Relaxed for perf,
        // say) would silently break the drain worker's final-sweep
        // contract — producer pushes might not be visible to
        // `pop_batch_into`. Promote to SeqCst so the load-bearing
        // happens-before is explicit at this site, not derived
        // from another atomic's ordering choice.
        //
        // Caveat: the SeqCst happens-before above only covers the
        // *non-deadline* exit from the spin (the loop condition
        // observed `in_flight_ingests == 0`). On the deadline-break
        // path the loop exits with `in_flight_ingests > 0` — the
        // outstanding producer pushes have NOT been observed
        // synchronized-with this thread, and the SeqCst store below
        // does not retroactively create a happens-before with
        // pushes that are still mid-flight. Those events are
        // exactly the "stranded" events accounted via
        // `events_dropped` and `shutdown_was_lossy`; the contract
        // is "every observed-pre-shutdown push is visible to the
        // final sweep on the happy path; on the deadline path,
        // up to `stranded` events past the gate are surfaced as
        // dropped". The flag at line 1271 (`shutdown_was_lossy`)
        // is the operator-visible signal that this contract was
        // exercised on the lossy branch.
        self.drain_finalize_ready
            .store(true, AtomicOrdering::SeqCst);

        // Stop the scaling monitor first — it's independent of the
        // ingestion path and just needs to observe the flag.
        let scaling_handle = self.scaling_monitor.lock().take();
        if let Some(handle) = scaling_handle {
            let _ = handle.await;
        }

        // Take workers without holding the lock across await.
        let workers = std::mem::take(&mut *self.batch_workers.lock());

        // 2. Await drain workers. Each one pops a final batch (up
        //    to 10k events) from its ring buffer, sends it on the
        //    channel, and exits. After this loop, every event in
        //    the ring buffers has been pushed to its channel.
        //
        // `join_all` lets the runtime overlap drain handles. A
        // sequential `for ... { drain.await; }` would serialize
        // shutdown wall-clock as N×T instead of max(T), which on
        // the default 1024-shard config × per-shard final-drain
        // time makes shutdown painful.
        let (drains, batch_handles): (Vec<_>, Vec<_>) = workers
            .into_iter()
            .map(|(shard_id, ShardWorkers { batch, drain, .. })| {
                ((shard_id, drain), (shard_id, batch))
            })
            .unzip();

        // Surface drain-worker JoinErrors explicitly. The default
        // Tokio runtime does NOT log spawned-task panics, so a
        // `let _ = join_all(...)` would silently swallow a panic
        // and mask stranded events. tracing::error per failure
        // makes the incident grep-able post-mortem.
        let drain_handles: Vec<_> = drains.into_iter().map(|(_, h)| h).collect();
        let drain_ids: Vec<u16> = batch_handles.iter().map(|(id, _)| *id).collect();
        for (shard_id, result) in drain_ids
            .iter()
            .copied()
            .zip(futures::future::join_all(drain_handles).await)
        {
            if let Err(e) = result {
                tracing::error!(
                    shard_id,
                    error = %e,
                    "drain worker JoinHandle errored on shutdown await"
                );
            }
        }

        // 3. Drop the original senders so the channels close once
        //    drain-worker sender clones (already dropped above)
        //    are gone too. Without this, batch workers would block
        //    on `recv().await` forever.
        drop(std::mem::take(&mut *self.batch_senders.write()));

        // 4. Await batch workers. They drain their channel until
        //    `recv() = None`, flush, and exit.
        //
        // Same parallelization as the drain phase, with the same
        // explicit JoinError surfacing.
        let batch_only: Vec<_> = batch_handles.into_iter().map(|(_, h)| h).collect();
        for (shard_id, result) in drain_ids
            .into_iter()
            .zip(futures::future::join_all(batch_only).await)
        {
            if let Err(e) = result {
                tracing::error!(
                    shard_id,
                    error = %e,
                    "BatchWorker JoinHandle errored on shutdown await"
                );
            }
        }

        // Flush and shutdown adapter (with timeout to prevent hanging)
        let timeout = self.config.adapter_timeout;
        if tokio::time::timeout(timeout, self.adapter.flush())
            .await
            .is_err()
        {
            tracing::error!("Adapter flush timed out during shutdown");
        }
        let result = tokio::time::timeout(timeout, self.adapter.shutdown())
            .await
            .map_err(|_| AdapterError::Fatal("adapter shutdown timed out".into()))?;

        // Post-drain reconciliation for the lossy-shutdown path.
        //
        // If we hit the in-flight deadline above, `deadline_snapshot`
        // holds `(stranded, ingested@deadline, dispatched@deadline)`.
        // Some of those `stranded` producers' events landed in the
        // ring AFTER our deadline check but BEFORE the
        // `drain_finalize_ready` gate flipped — those events are
        // now successfully ingested, drained, and dispatched through
        // the adapter. They appear in `events_dispatched` (the
        // delta since the deadline), so:
        //
        //   actual_drops = stranded
        //                  - (dispatched_after_drain - dispatched@deadline)
        //                  - (ingested_after_drain - ingested@deadline)
        //                       only counting events that landed but
        //                       weren't dispatched (dropped under
        //                       backpressure, etc.)
        //
        // The cleaner reconciliation: events that completed
        // `try_enter_ingest` AFTER the deadline either completed
        // ingest (bumping `events_ingested`) or were dropped on
        // backpressure (bumping `events_dropped` from the existing
        // backpressure paths). The `stranded - delta_ingested`
        // remainder is producers whose `try_enter_ingest` succeeded
        // but never reached `shard_manager.ingest()` — those are
        // the genuinely-lost events we should account for.
        if let Some((stranded, ingested_at_deadline, _dispatched_at_deadline)) = deadline_snapshot {
            let ingested_after = self.stats.events_ingested.load(AtomicOrdering::Acquire);
            let post_deadline_ingests = ingested_after.saturating_sub(ingested_at_deadline);
            let actual_drops = stranded.saturating_sub(post_deadline_ingests);
            if actual_drops > 0 {
                self.stats
                    .events_dropped
                    .fetch_add(actual_drops, AtomicOrdering::Relaxed);
            }
            tracing::warn!(
                stranded_at_deadline = stranded,
                post_deadline_ingests,
                actual_drops,
                "lossy shutdown reconciled: post-drain `events_dropped` bumped \
                 by stranded - post-deadline-ingests (pre-fix this bumped by \
                 the full `stranded` count, double-counting events the drain \
                 still successfully delivered)",
            );
        }

        // Mark shutdown as completed so Drop knows not to warn.
        self.shutdown_completed.store(true, AtomicOrdering::Release);
        result
    }

    /// True once `shutdown` / `shutdown_via_ref` has signaled — does
    /// not imply the shutdown work has finished. Use
    /// [`is_shutdown_completed`](Self::is_shutdown_completed) for
    /// completion.
    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(AtomicOrdering::Acquire)
    }

    /// True once `shutdown` / `shutdown_via_ref` has fully drained
    /// workers and the adapter shutdown returned (success path only).
    pub fn is_shutdown_completed(&self) -> bool {
        self.shutdown_completed.load(AtomicOrdering::Acquire)
    }

    /// Get shard metrics (if dynamic scaling is enabled).
    pub fn shard_metrics(&self) -> Option<Vec<ShardMetrics>> {
        self.shard_manager.collect_metrics()
    }

    /// Check if dynamic scaling is enabled.
    pub fn is_dynamic_scaling_enabled(&self) -> bool {
        self.config.scaling.is_some()
    }

    /// Manually trigger a scale-up (for testing or manual intervention).
    ///
    /// Bypasses the auto-scaling cooldown so a deliberate operator
    /// request isn't rate-limited by the auto-scaling cadence.
    /// Pre-fix this looped `add_shard_internal()` N times, each
    /// of which bumped `last_scaling`, so iteration 1+ failed
    /// with `InCooldown` against any non-zero cooldown — the
    /// first shard was left half-added (workers spawned, routing
    /// entry installed) while the error propagated to the
    /// caller. The `max_shards` budget check still applies.
    pub async fn manual_scale_up(&self, count: u16) -> Result<Vec<u16>, AdapterError> {
        let mut new_ids = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let id = self.add_shard_internal_force().await?;
            new_ids.push(id);
        }
        Ok(new_ids)
    }

    /// Manually trigger a scale-down (for testing or manual intervention).
    ///
    /// Marks `count` shards as `Draining`, waits for them to empty,
    /// finalizes them to `Stopped`, and removes them from the
    /// routing table — mirroring the scaling monitor's per-tick
    /// finalize loop. Returns the IDs of shards that were
    /// successfully drained AND removed (subset of those marked
    /// Draining if any failed to empty within the deadline).
    ///
    /// Drives the full scale-down lifecycle synchronously: a
    /// plain `mapper.scale_down` call marks shards `Draining` but
    /// does NOT finalize them — finalization is the scaling
    /// monitor's responsibility. Bus configs without an active
    /// monitor (or callers that shut down before the monitor's
    /// next tick) would otherwise lose any events queued in those
    /// shards' ring buffers.
    pub async fn manual_scale_down(&self, count: u16) -> Result<Vec<u16>, AdapterError> {
        let mapper = self
            .shard_manager
            .mapper()
            .ok_or_else(|| AdapterError::Fatal("Dynamic scaling not enabled".into()))?;

        let drained_ids = mapper
            .scale_down(count)
            .map_err(|e| AdapterError::Fatal(e.to_string()))?;

        // `finalize_draining` requires the shard to have been
        // Draining for >100ms with an empty ring buffer and no
        // pushes since drain start. Poll until every requested
        // shard finalizes, capped by an outer deadline so a wedged
        // producer can't pin this method forever.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut finalized: std::collections::HashSet<u16> = std::collections::HashSet::new();
        let target: std::collections::HashSet<u16> = drained_ids.iter().copied().collect();

        while finalized.len() < target.len() && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let stopped = mapper.finalize_draining();
            // `finalize_draining` is destructive — every qualifying
            // Draining shard transitions to Stopped in one shot,
            // regardless of who initiated the drain. Pre-fix the
            // `if target.contains(&shard_id)` filter dropped non-
            // target ids on the floor; if the scaling monitor (or
            // a parallel `manual_scale_down` on a different target
            // set) finalized one of THEIR shards in the same tick,
            // that shard ended up Stopped with workers + routing
            // entry intact — leaked. Always tear down via
            // `remove_shard_internal` so the bus-side state
            // (workers, sender, routing) is freed; only count the
            // target subset toward the returned Vec.
            for shard_id in stopped {
                let _ = self.remove_shard_internal(shard_id).await;
                if target.contains(&shard_id) {
                    finalized.insert(shard_id);
                }
            }
        }

        // Surface partial success at WARN level. The return shape
        // is preserved for compat (changing to `(Vec, Vec)` would
        // break the existing callers + test) so a smaller-than-
        // target list could otherwise be silently mistaken for full
        // success; the WARN log gives operations tooling something
        // to alert on.
        if finalized.len() < target.len() {
            let still_draining: Vec<u16> = target.difference(&finalized).copied().collect();
            tracing::warn!(
                requested = target.len(),
                finalized = finalized.len(),
                still_draining = ?still_draining,
                "manual_scale_down deadline elapsed before all targeted \
                 shards finalized — events still in-flight on the listed \
                 shards. They will finalize on the next scaling-monitor \
                 tick or on shutdown."
            );
        }

        Ok(finalized.into_iter().collect())
    }
}

impl Drop for EventBus {
    fn drop(&mut self) {
        // Signal shutdown so background tasks (drain workers, batch
        // workers, scaling monitor) observe the flag and exit. We
        // cannot await futures in Drop, but setting the atomic flag
        // triggers eventual termination.
        //
        // Previously used `Release` here while `try_enter_ingest`
        // and `shutdown()` use `SeqCst` on the same flag. `&mut self`
        // exclusion makes that sound today (no concurrent ingest can
        // observe a half-published shutdown). The mismatch is purely
        // defensive — switching to `SeqCst` matches the rest of the
        // lifecycle and removes a footgun if a future change ever
        // opened a path where `Drop` overlaps an in-flight
        // `try_enter_ingest`.
        self.shutdown.store(true, AtomicOrdering::SeqCst);
        // Also release the drain-finalize gate so any drain worker
        // already parked waiting for it can proceed and exit. Without
        // this, drop-without-shutdown leaves drain workers blocked on
        // `drain_finalize_ready` until their internal deadline fires
        // (which delays task cleanup by `DRAIN_FINALIZE_TIMEOUT`).
        // Best-effort durability: drop never gets the in-flight wait,
        // so any push that lands after this point is still lost.
        self.drain_finalize_ready
            .store(true, AtomicOrdering::SeqCst);

        // Workers do NOT hold `Arc<EventBus>` — they hold
        // independent `Arc<ShardManager>` / `Arc<dyn Adapter>`
        // clones plus the channel halves. When `Drop` returns,
        // those Arcs survive in the still-running tasks and they
        // continue draining / dispatching until they observe the
        // shutdown flag we just set. There's no partial-Drop UB
        // risk: nothing on the worker side dereferences a
        // dropped EventBus field. The flags promote the
        // worker tasks from "blocked on recv / parked on
        // drain_finalize_ready" to "observe shutdown=true and
        // exit" so they don't linger indefinitely.
        //
        // If `shutdown()` was never awaited, any events still in the
        // per-shard ring buffers or mpsc channels are lost — the
        // adapter's `flush()` and `shutdown()` won't run, so durable
        // backends never see them. We can't fix that from `Drop` (no
        // async), but we *can* surface the data-loss risk loudly so
        // it doesn't hide. The check is bounded to "shutdown was
        // never started"; an in-progress shutdown is fine because the
        // call site is awaiting it.
        if !self.shutdown_completed.load(AtomicOrdering::Acquire) {
            // Count events still sitting in shard ring buffers. They
            // are stranded — the drain workers will see `shutdown =
            // true` and exit without flushing, the adapter's
            // `flush()`/`shutdown()` never run, so anything in the
            // rings at this point is permanently lost. Surface that
            // loss via `events_dropped` so post-mortem stats reflect
            // reality (operators alerting on `events_dropped > 0`
            // would otherwise miss the entire incident), and set
            // `shutdown_was_lossy` so the boolean view is consistent
            // with the counter view.
            //
            // Events in the BatchWorker mpsc channels or pending
            // batches are not counted here — those workers may still
            // observe the shutdown flag and exit, but we have no
            // synchronous way from Drop to enumerate them. The ring-
            // buffer count is a lower bound on the stranded total.
            let stranded_in_rings = self.shard_manager.total_pending_in_rings();
            if stranded_in_rings > 0 {
                self.stats
                    .events_dropped
                    .fetch_add(stranded_in_rings, AtomicOrdering::Relaxed);
                self.stats
                    .shutdown_was_lossy
                    .store(true, AtomicOrdering::Release);
            }

            let stats = self.shard_manager.stats();
            tracing::warn!(
                events_ingested = stats.events_ingested,
                events_dropped = stats.events_dropped,
                stranded_in_rings,
                "EventBus dropped without an awaited shutdown(). Any in-flight \
                 events still in the ring buffers or batch channels will be lost \
                 — the adapter's flush()/shutdown() never ran. Call \
                 `bus.shutdown().await` before dropping for durable shutdown."
            );
        }
    }
}

/// Spin deadline for the second-caller path in
/// `shutdown_via_ref`. 10s in production.
#[cfg(not(test))]
fn shutdown_via_ref_spin_deadline() -> std::time::Duration {
    std::time::Duration::from_secs(10)
}

/// Test-only override. Stored as milliseconds; `0` (the default)
/// means "use the production 10s". Set via
/// [`set_shutdown_via_ref_spin_deadline_for_test`] from inside a
/// test that needs to exercise the deadline-elapsed path without
/// wall-clock-waiting the full 10s.
///
/// This is a global atomic shared across all tests in the
/// `cargo test --lib` binary. If two tests touched it concurrently,
/// one's override would leak into the other's expectations. Tests
/// that use the override MUST take the
/// [`SHUTDOWN_DEADLINE_OVERRIDE_GUARD`] mutex for the duration of
/// their override-setter / read window — see
/// [`set_shutdown_via_ref_spin_deadline_for_test`].
#[cfg(test)]
static SHUTDOWN_VIA_REF_DEADLINE_OVERRIDE_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Serializes access to the deadline override so concurrent tests
/// can't observe each other's transient values. Tests that override
/// the deadline take this mutex via
/// [`shutdown_deadline_override_lock`] and hold the guard until
/// they reset the override to 0.
///
/// Uses `tokio::sync::Mutex` rather than `std::sync::Mutex` so
/// the guard can legitimately be held across `.await` points
/// while the guarded test runs (clippy::await_holding_lock would
/// otherwise fire on the std variant — and rightly so for
/// production code).
#[cfg(test)]
static SHUTDOWN_DEADLINE_OVERRIDE_GUARD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Acquire the guard mutex protecting the deadline-override
/// static. Tests touching the override hold the returned guard
/// across both the set and the reset, so concurrent tests
/// observe a consistent default.
#[cfg(test)]
pub(crate) async fn shutdown_deadline_override_lock() -> tokio::sync::MutexGuard<'static, ()> {
    SHUTDOWN_DEADLINE_OVERRIDE_GUARD.lock().await
}

#[cfg(test)]
fn shutdown_via_ref_spin_deadline() -> std::time::Duration {
    let ms = SHUTDOWN_VIA_REF_DEADLINE_OVERRIDE_MS.load(std::sync::atomic::Ordering::Relaxed);
    if ms == 0 {
        std::time::Duration::from_secs(10)
    } else {
        std::time::Duration::from_millis(ms)
    }
}

#[cfg(test)]
pub(crate) fn set_shutdown_via_ref_spin_deadline_for_test(ms: u64) {
    SHUTDOWN_VIA_REF_DEADLINE_OVERRIDE_MS.store(ms, std::sync::atomic::Ordering::Relaxed);
}

async fn run_scaling_monitor_via_weak(weak: std::sync::Weak<EventBus>) {
    // Refresh `interval` from the policy on every tick. The previous
    // version cached it once at task start, so any future runtime
    // policy update would not be adopted by the monitor without a
    // process restart. Today `EventBusConfig` is immutable
    // post-construction so this is a no-op — but reading it each tick
    // is cheap (one atomic / RwLock read) and removes the latent
    // footgun.
    loop {
        let interval = match weak.upgrade() {
            Some(bus) => match &bus.config.scaling {
                Some(p) => p.metrics_window,
                None => return,
            },
            None => return,
        };
        tokio::time::sleep(interval).await;

        let bus = match weak.upgrade() {
            Some(b) => b,
            // Last strong ref dropped — caller is shutting down (or
            // already gone). Exit cleanly.
            None => break,
        };

        // SeqCst to match the writer side (`EventBus::shutdown` /
        // `Drop`). The Acquire/Release handshake on
        // `drain_finalize_ready` already provides the load-bearing
        // happens-before today — but a future code change that
        // piggybacks on `shutdown`'s ordering (e.g. a producer that
        // observes shutdown without going through
        // `try_enter_ingest`) would silently break under Relaxed.
        // Aligning the read-side ordering with the writer-side
        // SeqCst is a one-instruction tax for the safety.
        if bus.shutdown.load(AtomicOrdering::SeqCst) {
            break;
        }

        // Collect metrics for observability.
        if let Some(metrics) = bus.shard_manager.collect_metrics() {
            for m in &metrics {
                if m.fill_ratio > 0.5 {
                    tracing::debug!(
                        shard_id = m.shard_id,
                        fill_ratio = m.fill_ratio,
                        event_rate = m.event_rate,
                        "Shard metrics"
                    );
                }
            }
        }

        // Evaluate scaling.
        match bus.shard_manager.evaluate_scaling() {
            ScalingDecision::ScaleUp(count) => {
                tracing::info!(count = count, "Scaling up shards");
                for _ in 0..count {
                    if let Err(e) = bus.add_shard_internal().await {
                        tracing::error!(error = %e, "Failed to add shard");
                        break;
                    }
                }
            }
            ScalingDecision::ScaleDown(count) => {
                tracing::info!(count = count, "Scaling down shards");
                if let Some(mapper) = bus.shard_manager.mapper() {
                    if let Ok(drained) = mapper.scale_down(count) {
                        for shard_id in drained {
                            let _ = bus.shard_manager.drain_shard(shard_id);
                        }
                    }
                }
            }
            ScalingDecision::None => {}
        }

        if let Some(mapper) = bus.shard_manager.mapper() {
            let stopped = mapper.finalize_draining();
            for shard_id in stopped {
                let _ = bus.remove_shard_internal(shard_id).await;
            }
        }

        // CRITICAL: drop the strong ref BEFORE the next sleep so a
        // concurrent `shutdown(self)` caller can `Arc::try_unwrap`
        // the last strong ref while we're sleeping.
        drop(bus);
    }
}

/// Spawn a batch worker for a shard.
/// Dispatch a batch to the adapter with timeout and optional retries.
/// Returns true if the batch was accepted, false if all attempts failed.
///
/// Non-retryable errors (e.g. `AdapterError::Connection`,
/// `AdapterError::Fatal`, `AdapterError::Serialization`) skip the
/// retry loop and drop the batch immediately. Retrying a fatal error
/// just delays the inevitable while burning CPU and amplifying log
/// noise. Use `AdapterError::is_retryable` as the single source of
/// truth for this decision.
/// Compute the per-attempt backoff for `dispatch_batch` retries.
///
/// Pre-fix the retry loop slept a flat `Duration::from_millis(100)`
/// after every failure. Under a partial backend outage (Redis /
/// JetStream slow but not dead), every shard's BatchWorker retried
/// on the exact same 100 ms cadence, producing a synchronized
/// retry storm that amplified load while the backend was
/// recovering.
///
/// Post-fix: exponential backoff (100, 200, 400, 800, 1600, 3200 ms)
/// with per-(shard, attempt) jitter to decorrelate retries across
/// shards. Capped at attempt=5 (3.2 s base) so retries don't grow
/// unboundedly. Jitter is `[-25%, +25%]` of the base, derived from
/// hashing `(shard_id, attempt)` — deterministic per shard but
/// uncorrelated across shards, which is exactly what the storm
/// mitigation needs.
fn retry_backoff(shard_id: u16, attempt: u32) -> Duration {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    // 100 ms × 2^attempt, capped at attempt=5 → 100/200/400/800/1600/3200.
    let base_ms: u64 = 100u64.saturating_mul(1u64 << attempt.min(5));

    let mut hasher = DefaultHasher::new();
    shard_id.hash(&mut hasher);
    attempt.hash(&mut hasher);
    let h = hasher.finish();
    // Jitter in [0, base_ms/2), centered to give [-25%, +25%].
    let jitter_range = (base_ms / 2).max(1);
    let jitter = (h % jitter_range) as i64 - (jitter_range as i64 / 2);
    let final_ms = (base_ms as i64 + jitter).max(1) as u64;

    Duration::from_millis(final_ms)
}

async fn dispatch_batch(
    adapter: &dyn Adapter,
    batch: Batch,
    shard_id: u16,
    timeout: Duration,
    retries: u32,
) -> bool {
    // Retry attempts clone the batch; the final attempt moves it, saving
    // one clone per dispatch (the common path is retries == 0).
    for attempt in 0..retries {
        match tokio::time::timeout(timeout, adapter.on_batch(batch.clone())).await {
            Ok(Ok(())) => return true,
            Ok(Err(e)) => {
                if !e.is_retryable() {
                    // Tag with a `reason` field so this
                    // distinct drop cause is separately filterable
                    // from retry-exhausted and timeout in
                    // observability tools.
                    tracing::error!(
                        shard_id,
                        error = %e,
                        attempt,
                        reason = "non_retryable",
                        "Non-retryable error from adapter, dropping batch"
                    );
                    return false;
                }
                tracing::warn!(shard_id, error = %e, attempt, "Batch dispatch failed, retrying");
            }
            Err(_) => {
                tracing::warn!(shard_id, attempt, "Adapter on_batch timed out, retrying");
            }
        }
        tokio::time::sleep(retry_backoff(shard_id, attempt)).await;
    }

    // Pre-fix the final attempt collapsed every drop into
    // one log line ("Failed to dispatch batch, dropping"), making
    // it impossible to tell retry-exhausted from fatal-non-
    // retryable from timeout in metrics. The non-retryable case
    // already has its own log inside the retry loop above (early
    // return); here we tag retry-exhausted vs timeout-after-
    // retries with distinct `reason` fields so log-based
    // observability tools can break the drops out by cause.
    match tokio::time::timeout(timeout, adapter.on_batch(batch)).await {
        Ok(Ok(())) => true,
        Ok(Err(e)) => {
            tracing::error!(
                shard_id,
                error = %e,
                reason = "retry_exhausted",
                attempts = retries + 1,
                "Failed to dispatch batch after exhausting retries, dropping"
            );
            false
        }
        Err(_) => {
            tracing::error!(
                shard_id,
                reason = "timeout",
                attempts = retries + 1,
                "Adapter on_batch timed out on final attempt, dropping batch"
            );
            false
        }
    }
}

struct BatchWorkerParams {
    shard_id: u16,
    rx: mpsc::Receiver<Vec<crate::event::InternalEvent>>,
    adapter: Arc<dyn Adapter>,
    shard_manager: Arc<ShardManager>,
    config: BatchConfig,
    adapter_timeout: Duration,
    batch_retries: u32,
    /// Bus-owned mirror of `BatchWorker::next_sequence`. The worker
    /// stores its post-flush sequence here on every dispatch so the
    /// bus can read it after the worker exits — see
    /// `ShardWorkers::next_sequence` for the consumer side.
    next_sequence: Arc<AtomicU64>,
    /// Bus-level stats. The worker increments
    /// `batches_dispatched` and `events_dispatched` after every
    /// successful `dispatch_batch`. Both must actually be
    /// incremented here, otherwise `flush()`'s Phase 2 progress
    /// probe would always observe zero progress and early-break
    /// after a single `max_delay` window — racing the
    /// BatchWorker's first `recv_timeout` and flaking on
    /// Windows-class timer resolution.
    stats: Arc<EventBusStats>,
    /// Producer nonce stamped on every batch the worker emits.
    /// Bus-loaded from the persistent path when
    /// `producer_nonce_path` is configured, otherwise from the
    /// per-process default.
    producer_nonce: u64,
}

fn spawn_batch_worker(params: BatchWorkerParams) -> JoinHandle<()> {
    let BatchWorkerParams {
        shard_id,
        mut rx,
        adapter,
        shard_manager,
        config,
        adapter_timeout,
        batch_retries,
        next_sequence,
        stats,
        producer_nonce,
    } = params;
    tokio::spawn(async move {
        let mut worker = BatchWorker::new(shard_id, config.clone(), next_sequence, producer_nonce);

        loop {
            // Wait for events with timeout. The batch worker exits
            // only when its channel is closed — i.e. after every
            // upstream sender (the drain worker for this shard +
            // `EventBus::batch_senders`) has been dropped.
            // `EventBus::shutdown` enforces that ordering so no
            // event is left stranded in the channel.
            let recv_timeout = worker.time_until_timeout().unwrap_or(config.max_delay);

            match tokio::time::timeout(recv_timeout, rx.recv()).await {
                Ok(Some(events)) => {
                    if let Some(batch) = worker.add_events(events) {
                        let batch_len = batch.len() as u64;
                        if dispatch_batch(
                            &*adapter,
                            batch,
                            shard_id,
                            adapter_timeout,
                            batch_retries,
                        )
                        .await
                        {
                            stats
                                .batches_dispatched
                                .fetch_add(1, AtomicOrdering::Relaxed);
                            stats
                                .events_dispatched
                                .fetch_add(batch_len, AtomicOrdering::Relaxed);
                            if let Some(shard_ref) = shard_manager.shard(shard_id) {
                                shard_ref.lock().record_batch_dispatch();
                            }
                        }
                    }
                }
                Ok(None) => {
                    // Channel closed — drain any pending and exit.
                    if worker.has_pending() {
                        let batch = worker.flush();
                        if !batch.is_empty() {
                            let batch_len = batch.len() as u64;
                            if dispatch_batch(
                                &*adapter,
                                batch,
                                shard_id,
                                adapter_timeout,
                                batch_retries,
                            )
                            .await
                            {
                                stats
                                    .batches_dispatched
                                    .fetch_add(1, AtomicOrdering::Relaxed);
                                stats
                                    .events_dispatched
                                    .fetch_add(batch_len, AtomicOrdering::Relaxed);
                            }
                        }
                    }
                    break;
                }
                Err(_) => {
                    // Timeout - check if we need to flush
                    if let Some(batch) = worker.add_events(vec![]) {
                        let batch_len = batch.len() as u64;
                        if dispatch_batch(
                            &*adapter,
                            batch,
                            shard_id,
                            adapter_timeout,
                            batch_retries,
                        )
                        .await
                        {
                            stats
                                .batches_dispatched
                                .fetch_add(1, AtomicOrdering::Relaxed);
                            stats
                                .events_dispatched
                                .fetch_add(batch_len, AtomicOrdering::Relaxed);
                            if let Some(shard_ref) = shard_manager.shard(shard_id) {
                                shard_ref.lock().record_batch_dispatch();
                            }
                        }
                    }
                }
            }
        }
    })
}

/// Maximum time a drain worker waits for `drain_finalize_ready`
/// after observing `shutdown=true`. Defense in depth: if a caller
/// drops the bus mid-shutdown without setting the gate, we don't
/// want the worker pinned forever. The shutdown path *always* sets
/// the gate (even on its own timeout), so this deadline is normally
/// unreached.
const DRAIN_FINALIZE_TIMEOUT: Duration = Duration::from_secs(10);

/// Spawn a drain worker for a single shard.
///
/// Uses a scratch `Vec` + `pop_batch_into` so the per-cycle
/// allocation happens *outside* the shard mutex critical section.
/// Each cycle: lock → drain into scratch (no alloc, capacity already
/// reserved) → unlock → `mem::replace` swaps the filled scratch out
/// for a fresh empty `Vec` (alloc *outside* the lock) → send the
/// filled batch on the channel.
fn spawn_drain_worker_for_shard(
    shard_id: u16,
    shard_manager: Arc<ShardManager>,
    sender: mpsc::Sender<Vec<crate::event::InternalEvent>>,
    shutdown: Arc<AtomicBool>,
    drain_finalize_ready: Arc<AtomicBool>,
) -> JoinHandle<()> {
    const STEADY_BATCH: usize = 1_000;
    const FINAL_BATCH: usize = 10_000;

    tokio::spawn(async move {
        let mut scratch: Vec<crate::event::InternalEvent> = Vec::with_capacity(STEADY_BATCH);

        loop {
            // SeqCst to match the writer side (`EventBus::shutdown` /
            // `Drop`). `try_enter_ingest` itself uses SeqCst, and
            // the Acquire/Release handshake on
            // `drain_finalize_ready` (below) is what actually makes
            // the producer-push happen-before visible. Aligning to
            // SeqCst here makes the contract robust to future
            // producer-side changes that might piggyback on
            // `shutdown`'s ordering.
            if shutdown.load(AtomicOrdering::SeqCst) {
                // Before doing the final sweep, wait for `shutdown()`
                // to release the finalize gate. The gate is set only
                // after the in-flight ingest counter reaches zero,
                // which means every producer that read `shutdown=false`
                // has completed its push. Without this wait, the drain
                // worker can race ahead of a late push under
                // shard-mutex serialization (drain takes the lock
                // first, sees nothing, exits; producer then takes the
                // lock and pushes — event stranded).
                //
                // Acquire pairs with the Release in `EventBus::shutdown`
                // and `EventBus::drop`, transitively making every
                // producer push that happened-before its `in_flight`
                // decrement visible to the subsequent `pop_batch_into`.
                let finalize_deadline = std::time::Instant::now() + DRAIN_FINALIZE_TIMEOUT;
                while !drain_finalize_ready.load(AtomicOrdering::Acquire) {
                    if std::time::Instant::now() >= finalize_deadline {
                        tracing::warn!(
                            shard_id,
                            "drain worker timed out waiting for finalize gate; \
                             proceeding with potential event loss"
                        );
                        break;
                    }
                    // Park instead of `yield_now` so we don't
                    // starve the workers / producers we're waiting
                    // on under contention.
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                }

                // Final drain: loop until the ring buffer is empty.
                // A single 10k batch is not enough — the ring
                // buffer can hold up to `ring_buffer_capacity`
                // events (default 1M) and any leftover would be
                // silently lost on shutdown.
                //
                // Pre-fix this broke at the first
                // `popped == 0`. The audit posited a narrow race
                // where a producer that fetch_add'd
                // in_flight_ingests but stalled before the
                // shard-lock body could push AFTER shutdown
                // observed in_flight=0 yet BEFORE this final
                // sweep saw the event. The SeqCst guard pattern
                // makes this unlikely (the push happens-before
                // the guard drop), but the defense is cheap:
                // require TWO consecutive zero-event passes
                // before declaring drain. The yield_now between
                // them gives a stalled producer a chance to land
                // the push.
                let mut final_scratch: Vec<crate::event::InternalEvent> =
                    Vec::with_capacity(FINAL_BATCH);
                let mut consecutive_zeros = 0u32;
                loop {
                    let popped = shard_manager
                        .with_shard(shard_id, |shard| {
                            shard.pop_batch_into(&mut final_scratch, FINAL_BATCH)
                        })
                        .unwrap_or(0);
                    if popped == 0 {
                        consecutive_zeros += 1;
                        if consecutive_zeros >= 2 {
                            break;
                        }
                        // Yield to let any racing producer commit
                        // its push, then re-poll.
                        tokio::task::yield_now().await;
                        continue;
                    }
                    consecutive_zeros = 0;
                    let batch =
                        std::mem::replace(&mut final_scratch, Vec::with_capacity(FINAL_BATCH));
                    let batch_len = batch.len();
                    if let Err(_send_err) = sender.send(batch).await {
                        // Batch worker exited before drain. The
                        // `mem::replace` already pulled events out
                        // of the ring buffer, so the dropped batch
                        // is unrecoverable — the SendError carries
                        // it back but the consumer is gone. Surface
                        // the count loudly so the loss is
                        // observable in operator dashboards rather
                        // than a silent miss in shutdown stats.
                        tracing::error!(
                            shard_id,
                            dropped = batch_len,
                            "drain worker (final): batch worker dropped \
                             channel before final drain completed; \
                             events removed from ring buffer cannot be redelivered",
                        );
                        break;
                    }
                }
                break;
            }

            // Drain events from ring buffer.
            let popped = shard_manager.with_shard(shard_id, |shard| {
                shard.pop_batch_into(&mut scratch, STEADY_BATCH)
            });

            match popped {
                Some(0) => {
                    // No events — yield briefly. The 100μs sleep is deliberate:
                    // this is a latency-first system where the drain loop is the
                    // hot path. Longer backoff would add milliseconds of latency
                    // to the first event after a quiet period, violating the
                    // sub-microsecond design target. The CPU cost of 100μs polling
                    // is acceptable for a system that processes 10M+ events/sec.
                    tokio::time::sleep(Duration::from_micros(100)).await;
                }
                Some(_) => {
                    let batch = std::mem::replace(&mut scratch, Vec::with_capacity(STEADY_BATCH));
                    let batch_len = batch.len();
                    if let Err(_send_err) = sender.send(batch).await {
                        // Steady-state: the only way the batch
                        // worker drops the channel is if it
                        // panicked or `remove_shard_internal`
                        // tore it down out of order with the
                        // drain worker (which the documented
                        // shutdown sequence forbids). Either way,
                        // the events are unrecoverable — the
                        // `mem::replace` above already pulled them
                        // out of the ring buffer. Pre-fix this
                        // simply `break`-d, leaving the loss
                        // invisible. Surface a loud error with
                        // the dropped count so an out-of-order
                        // shutdown or batch-worker panic shows up
                        // in dashboards rather than as a silent
                        // metric gap.
                        tracing::error!(
                            shard_id,
                            dropped = batch_len,
                            "drain worker: batch worker dropped channel \
                             during steady-state drain; events removed from \
                             ring buffer cannot be redelivered",
                        );
                        break;
                    }
                }
                None => {
                    // Shard no longer exists (was removed)
                    break;
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shard::ScalingPolicy;
    use serde_json::json;

    #[tokio::test]
    async fn test_event_bus_basic() {
        let config = EventBusConfig::builder()
            .num_shards(2)
            .ring_buffer_capacity(1024)
            .build()
            .unwrap();

        let bus = EventBus::new(config).await.unwrap();

        // Ingest some events
        for i in 0..10 {
            let event = Event::new(json!({"index": i}));
            bus.ingest(event).unwrap();
        }

        // Give workers time to process
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Check stats
        assert_eq!(
            bus.stats().events_ingested.load(AtomicOrdering::Relaxed),
            10
        );

        bus.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_event_bus_batch_ingest() {
        let config = EventBusConfig::default();
        let bus = EventBus::new(config).await.unwrap();

        let events: Vec<Event> = (0..100).map(|i| Event::new(json!({"i": i}))).collect();

        let ingested = bus.ingest_batch(events);
        assert_eq!(ingested, 100);

        bus.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_event_bus_with_dynamic_scaling() {
        let policy = ScalingPolicy {
            min_shards: 2,
            max_shards: 8,
            ..Default::default()
        };

        let config = EventBusConfig::builder()
            .num_shards(2)
            .ring_buffer_capacity(1024)
            .scaling(policy)
            .build()
            .unwrap();

        let bus = EventBus::new(config).await.unwrap();

        // Verify dynamic scaling is enabled
        assert!(bus.is_dynamic_scaling_enabled());
        assert_eq!(bus.num_shards(), 2);

        // Ingest some events
        for i in 0..100 {
            let event = Event::new(json!({"index": i}));
            bus.ingest(event).unwrap();
        }

        // Give workers time to process
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Check stats
        assert_eq!(
            bus.stats().events_ingested.load(AtomicOrdering::Relaxed),
            100
        );

        bus.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_manual_scale_up() {
        let policy = ScalingPolicy {
            min_shards: 2,
            max_shards: 8,
            cooldown: Duration::from_nanos(1), // Effectively disable cooldown for test
            ..Default::default()
        };

        let config = EventBusConfig::builder()
            .num_shards(2)
            .ring_buffer_capacity(1024)
            .scaling(policy)
            .build()
            .unwrap();

        let bus = EventBus::new(config).await.unwrap();

        assert_eq!(bus.num_shards(), 2);

        // Manually scale up
        let new_ids = bus.manual_scale_up(2).await.unwrap();
        assert_eq!(new_ids.len(), 2);
        assert_eq!(bus.num_shards(), 4);

        // Ingest events - they should be distributed across all shards
        for i in 0..100 {
            let event = Event::new(json!({"index": i}));
            bus.ingest(event).unwrap();
        }

        tokio::time::sleep(Duration::from_millis(100)).await;

        assert_eq!(
            bus.stats().events_ingested.load(AtomicOrdering::Relaxed),
            100
        );

        bus.shutdown().await.unwrap();
    }

    /// Regression for BUG_AUDIT_2026_04_30_CORE.md #82: previously
    /// `manual_scale_down` only called `mapper.scale_down(count)`,
    /// which marks shards as `Draining` but does NOT finalize them.
    /// Bus configs without an active scaling monitor (or callers
    /// shutting down before the monitor's next tick) lost any
    /// events queued in the drained shards' ring buffers because
    /// `remove_shard_internal` was never invoked. The fix runs the
    /// full lifecycle synchronously: scale_down → poll for empty →
    /// finalize_draining → remove_shard_internal.
    ///
    /// We pin this by scaling up, manually scaling down, and
    /// asserting that `num_shards` actually decreases — pre-fix
    /// the count would still reflect the Draining shards.
    #[tokio::test]
    async fn manual_scale_down_finalizes_and_removes_drained_shards() {
        let policy = ScalingPolicy {
            min_shards: 2,
            max_shards: 8,
            cooldown: Duration::from_nanos(1),
            ..Default::default()
        };
        let config = EventBusConfig::builder()
            .num_shards(2)
            .ring_buffer_capacity(1024)
            .scaling(policy)
            .build()
            .unwrap();
        let bus = EventBus::new(config).await.unwrap();

        // Scale up to 4, then back down to 2.
        let added = bus.manual_scale_up(2).await.unwrap();
        assert_eq!(added.len(), 2);
        assert_eq!(bus.num_shards(), 4);

        let removed = bus.manual_scale_down(2).await.unwrap();
        assert_eq!(
            removed.len(),
            2,
            "manual_scale_down must complete the lifecycle for both \
             requested shards (mark Draining → wait → finalize → remove)"
        );

        // Pre-fix: `num_shards` would still be 4 because shards
        // were only marked Draining (and the routing-table removal
        // path never ran). Post-fix: it's back to 2.
        assert_eq!(
            bus.num_shards(),
            2,
            "drained shards must be removed from the routing table"
        );

        bus.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_shard_metrics() {
        let policy = ScalingPolicy::default();

        let config = EventBusConfig::builder()
            .num_shards(2)
            .ring_buffer_capacity(1024)
            .scaling(policy)
            .build()
            .unwrap();

        let bus = EventBus::new(config).await.unwrap();

        // Ingest some events
        for i in 0..50 {
            let event = Event::new(json!({"index": i}));
            bus.ingest(event).unwrap();
        }

        // Get metrics
        let metrics = bus.shard_metrics();
        assert!(metrics.is_some());
        let metrics = metrics.unwrap();
        assert_eq!(metrics.len(), 2);

        bus.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_regression_eventbus_drop_signals_shutdown() {
        // Regression: dropping an EventBus without calling shutdown() used to
        // leave background tasks running indefinitely. The Drop impl now sets
        // the shutdown flag so workers eventually exit.
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            let config = EventBusConfig::builder()
                .num_shards(2)
                .ring_buffer_capacity(1024)
                .build()
                .unwrap();

            let bus = EventBus::new(config).await.unwrap();

            // Ingest some events
            for i in 0..10 {
                let event = Event::new(json!({"index": i}));
                bus.ingest(event).unwrap();
            }

            // Drop without calling shutdown()
            drop(bus);

            // If we reach here, the drop didn't hang
        })
        .await;

        assert!(
            result.is_ok(),
            "EventBus drop should not hang — Drop impl must signal shutdown"
        );
    }

    #[tokio::test]
    async fn test_with_dynamic_scaling_builder() {
        let config = EventBusConfig::builder()
            .num_shards(4)
            .ring_buffer_capacity(2048)
            .with_dynamic_scaling()
            .build()
            .unwrap();

        let bus = EventBus::new(config).await.unwrap();

        assert!(bus.is_dynamic_scaling_enabled());
        assert_eq!(bus.num_shards(), 4);

        bus.shutdown().await.unwrap();
    }

    /// Mock adapter that counts `on_batch` invocations and returns a
    /// configurable error variant. Used to assert dispatch retry
    /// semantics without dragging in a real adapter.
    struct CountingErrAdapter {
        calls: Arc<std::sync::atomic::AtomicU32>,
        make_err: Box<dyn Fn() -> AdapterError + Send + Sync>,
    }

    #[async_trait::async_trait]
    impl crate::adapter::Adapter for CountingErrAdapter {
        async fn init(&mut self) -> Result<(), AdapterError> {
            Ok(())
        }
        async fn on_batch(&self, _batch: Batch) -> Result<(), AdapterError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err((self.make_err)())
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
        ) -> Result<crate::adapter::ShardPollResult, AdapterError> {
            Ok(crate::adapter::ShardPollResult::empty())
        }
        fn name(&self) -> &'static str {
            "counting_err"
        }
        async fn is_healthy(&self) -> bool {
            true
        }
    }

    /// Regression: BUG_REPORT.md #21 — `dispatch_batch` previously
    /// retried every error variant, ignoring `AdapterError::is_retryable`.
    /// A non-retryable error (Connection / Fatal / Serialization)
    /// should now drop the batch immediately rather than burn the
    /// retry budget on something that cannot succeed.
    #[tokio::test(start_paused = true)]
    async fn dispatch_batch_skips_retries_on_non_retryable_error() {
        let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let adapter = CountingErrAdapter {
            calls: calls.clone(),
            make_err: Box::new(|| AdapterError::Connection("refused".into())),
        };

        let batch = Batch::new(0, vec![], 0);
        let ok = dispatch_batch(&adapter, batch, 0, Duration::from_secs(1), 5).await;

        assert!(!ok, "non-retryable error must drop batch");
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "Connection error must not be retried; expected exactly 1 on_batch call"
        );
    }

    /// Sanity: a retryable error *does* go through the full retry
    /// budget. Without this companion check, the previous test could
    /// pass for the wrong reason (e.g. if dispatch always returned on
    /// the first error).
    #[tokio::test(start_paused = true)]
    async fn dispatch_batch_retries_transient_errors() {
        let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let adapter = CountingErrAdapter {
            calls: calls.clone(),
            make_err: Box::new(|| AdapterError::Transient("temp".into())),
        };

        let batch = Batch::new(0, vec![], 0);
        let ok = dispatch_batch(&adapter, batch, 0, Duration::from_secs(1), 3).await;

        assert!(!ok);
        // 3 retries + 1 final attempt = 4 total calls.
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 4);
    }

    /// Counting adapter that records the number of events delivered via
    /// `on_batch`. Used by shutdown-durability tests below.
    struct CountingAdapter {
        received: Arc<AtomicU64>,
    }

    #[async_trait::async_trait]
    impl crate::adapter::Adapter for CountingAdapter {
        async fn init(&mut self) -> Result<(), AdapterError> {
            Ok(())
        }
        async fn on_batch(&self, batch: Batch) -> Result<(), AdapterError> {
            self.received
                .fetch_add(batch.events.len() as u64, AtomicOrdering::SeqCst);
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
        ) -> Result<crate::adapter::ShardPollResult, AdapterError> {
            Ok(crate::adapter::ShardPollResult::empty())
        }
        fn name(&self) -> &'static str {
            "counting"
        }
        async fn is_healthy(&self) -> bool {
            true
        }
    }

    /// `retry_backoff` exponentially grows the base delay
    /// per attempt and adds per-(shard, attempt) jitter to
    /// decorrelate retries across shards. Pin both invariants:
    /// monotonic growth on the base, and jitter that produces
    /// different outputs for different shard ids.
    #[test]
    fn retry_backoff_grows_with_attempt_and_jitters_per_shard() {
        // Shard 0 attempt 0..6: base ms 100, 200, 400, 800, 1600,
        // 3200, 3200 (cap). Plus ±25% jitter.
        let s0_a0 = retry_backoff(0, 0).as_millis();
        let s0_a1 = retry_backoff(0, 1).as_millis();
        let s0_a4 = retry_backoff(0, 4).as_millis();
        let s0_a5 = retry_backoff(0, 5).as_millis();
        let s0_a6 = retry_backoff(0, 6).as_millis();

        // Bounds: each attempt's base is in `[base*0.75, base*1.25)`.
        assert!((75..=125).contains(&s0_a0));
        assert!((150..=250).contains(&s0_a1));
        assert!((1200..=2000).contains(&s0_a4));
        assert!((2400..=4000).contains(&s0_a5));
        // Cap at attempt=5: attempt 6 must NOT exceed attempt 5's
        // upper bound.
        assert!(
            s0_a6 <= 4000,
            "attempt > 5 must cap at the attempt-5 base; got {}ms",
            s0_a6
        );

        // Jitter property: different shards at the same
        // attempt land on different backoffs. Sample 16 distinct
        // shard ids and assert at least 4 unique backoff values.
        //
        // The bound is deliberately loose (4 / 16) because
        // `DefaultHasher`'s exact distribution is **not stable**
        // across Rust toolchain versions — a tighter check (e.g.
        // ≥ 8) would empirically pass on every toolchain we test
        // against today, but a future stdlib change to the hasher
        // could shift the distribution and flake CI for a property
        // (decorrelation across shards) that doesn't actually
        // depend on a high collision-resistance bar. Asserting
        // ≥ 4 unique values out of 16 is enough to catch a real
        // regression (e.g. accidentally hashing only `attempt`
        // and not `shard_id` would collapse all 16 to a single
        // value) while staying robust to hasher-distribution
        // drift.
        use std::collections::HashSet;
        let s_attempt2: HashSet<u128> = (0u16..16)
            .map(|s| retry_backoff(s, 2).as_millis())
            .collect();
        assert!(
            s_attempt2.len() >= 4,
            "jitter must decorrelate retries across shards; \
             only {} unique backoffs across 16 shards",
            s_attempt2.len()
        );
    }

    /// CR-23: pin that `EventBus::shutdown` actually invokes the
    /// adapter's `flush()` and `shutdown()` methods. The existing
    /// `sdk/tests/shutdown_regression.rs` covers the
    /// "shutdown runs even with outstanding Arc clones" property
    /// using a memory adapter whose `flush`/`shutdown` are no-ops
    /// — so a regression that elided the adapter calls would still
    /// pass. This test uses a recording adapter that increments
    /// per-method counters; we assert flush AND shutdown both fired
    /// exactly once after a clean `bus.shutdown().await`.
    ///
    /// The fix routes `Net::shutdown` through
    /// `shutdown_via_ref`, which in turn calls
    /// `self.adapter.flush()` and `self.adapter.shutdown()` once
    /// each. CR-23 pins this contract at the bus layer so an
    /// inadvertent regression at the SDK or adapter wrapper layer
    /// can be caught without an integration setup.
    #[tokio::test]
    async fn cr23_shutdown_invokes_adapter_flush_and_shutdown_exactly_once() {
        struct RecordingAdapter {
            on_batch_calls: Arc<AtomicU64>,
            flush_calls: Arc<AtomicU64>,
            shutdown_calls: Arc<AtomicU64>,
        }

        #[async_trait::async_trait]
        impl crate::adapter::Adapter for RecordingAdapter {
            async fn init(&mut self) -> Result<(), AdapterError> {
                Ok(())
            }
            async fn on_batch(&self, _batch: Batch) -> Result<(), AdapterError> {
                self.on_batch_calls.fetch_add(1, AtomicOrdering::SeqCst);
                Ok(())
            }
            async fn flush(&self) -> Result<(), AdapterError> {
                self.flush_calls.fetch_add(1, AtomicOrdering::SeqCst);
                Ok(())
            }
            async fn shutdown(&self) -> Result<(), AdapterError> {
                self.shutdown_calls.fetch_add(1, AtomicOrdering::SeqCst);
                Ok(())
            }
            async fn poll_shard(
                &self,
                _shard_id: u16,
                _from_id: Option<&str>,
                _limit: usize,
            ) -> Result<crate::adapter::ShardPollResult, AdapterError> {
                Ok(crate::adapter::ShardPollResult::empty())
            }
            fn name(&self) -> &'static str {
                "cr23-recording"
            }
            async fn is_healthy(&self) -> bool {
                true
            }
        }

        let on_batch = Arc::new(AtomicU64::new(0));
        let flush = Arc::new(AtomicU64::new(0));
        let shutdown = Arc::new(AtomicU64::new(0));
        let adapter: Box<dyn crate::adapter::Adapter> = Box::new(RecordingAdapter {
            on_batch_calls: on_batch.clone(),
            flush_calls: flush.clone(),
            shutdown_calls: shutdown.clone(),
        });

        let config = EventBusConfig::builder()
            .num_shards(2)
            .ring_buffer_capacity(1024)
            .build()
            .unwrap();
        let bus = EventBus::new_with_adapter(config, adapter).await.unwrap();

        // Drive a small burst so on_batch fires at least once —
        // pins that the adapter is wired up correctly. The
        // load-bearing assertions below are on flush and shutdown.
        for i in 0..16 {
            let _ = bus.ingest(Event::new(json!({"i": i})));
        }

        // Pre-CR-23 a regression that elided one of these would
        // pass `shutdown_regression.rs::shutdown_runs_even_with_outstanding_event_stream`
        // because the memory adapter's flush/shutdown are no-ops.
        // Here the recording adapter makes the contract observable.
        bus.shutdown().await.unwrap();

        assert!(
            on_batch.load(AtomicOrdering::SeqCst) > 0,
            "sanity: on_batch must have fired at least once"
        );
        assert_eq!(
            flush.load(AtomicOrdering::SeqCst),
            1,
            "CR-23 regression: shutdown MUST call adapter.flush() exactly once"
        );
        assert_eq!(
            shutdown.load(AtomicOrdering::SeqCst),
            1,
            "CR-23 regression: shutdown MUST call adapter.shutdown() exactly once"
        );
    }

    /// CR-25: pin that a SECOND caller of `shutdown_via_ref` whose
    /// CAS loses (because a first caller already flipped the
    /// `shutdown` flag) and whose deadline elapses BEFORE the
    /// first caller sets `shutdown_completed=true` returns
    /// `AdapterError::Transient(_)` — NOT a silent `Ok(())`.
    ///
    /// Pre-CR-25 both branches returned `Ok`. A caller that lost
    /// the CAS race had no way to distinguish "first caller
    /// finished shutdown" from "deadline timed out mid-shutdown."
    /// Under a slow adapter (`adapter_timeout` default 30s >
    /// the 10s spin deadline), the second caller silently saw
    /// `Ok` while the bus was still mid-shutdown — letting
    /// subsequent code observe a partially-shut-down bus.
    ///
    /// We use a slow first caller (sleeps inside `flush()`)
    /// and override the spin deadline to a few ms so the test
    /// runs fast.
    #[tokio::test]
    async fn cr25_second_caller_returns_transient_when_deadline_elapses() {
        struct SlowFlushAdapter {
            // Block flush() for this long. The first caller
            // gets stuck here while the second caller's spin
            // deadline elapses.
            flush_delay: std::time::Duration,
        }

        #[async_trait::async_trait]
        impl crate::adapter::Adapter for SlowFlushAdapter {
            async fn init(&mut self) -> Result<(), AdapterError> {
                Ok(())
            }
            async fn on_batch(&self, _batch: Batch) -> Result<(), AdapterError> {
                Ok(())
            }
            async fn flush(&self) -> Result<(), AdapterError> {
                tokio::time::sleep(self.flush_delay).await;
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
            ) -> Result<crate::adapter::ShardPollResult, AdapterError> {
                Ok(crate::adapter::ShardPollResult::empty())
            }
            fn name(&self) -> &'static str {
                "cr25-slow-flush"
            }
            async fn is_healthy(&self) -> bool {
                true
            }
        }

        // Cubic P2: serialize access to the global deadline
        // override so concurrent tests don't interfere. Hold the
        // guard until the override is reset to 0 below.
        let _override_guard = super::shutdown_deadline_override_lock().await;

        // Override the second-caller spin deadline to a short
        // value so the test doesn't wall-clock-wait 10s. Production
        // builds use the 10s default (the cfg(test) override is
        // compiled out).
        super::set_shutdown_via_ref_spin_deadline_for_test(50);

        let config = EventBusConfig::builder()
            .num_shards(2)
            .ring_buffer_capacity(1024)
            .adapter_timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();
        // First caller's flush() sleeps 500ms — far longer than
        // the 50ms spin deadline.
        let adapter: Box<dyn crate::adapter::Adapter> = Box::new(SlowFlushAdapter {
            flush_delay: std::time::Duration::from_millis(500),
        });
        let bus = Arc::new(EventBus::new_with_adapter(config, adapter).await.unwrap());

        // Spawn the FIRST caller — it wins the CAS and proceeds
        // into the slow flush. We don't await it; we want it
        // running in parallel.
        let bus_first = Arc::clone(&bus);
        let first_caller = tokio::spawn(async move { bus_first.shutdown_via_ref().await });

        // Cubic P2: poll `is_shutdown()` until the first caller
        // has set the flag, instead of a fixed sleep. This makes
        // the test scheduler-independent — we proceed as soon as
        // the first caller has won the CAS, regardless of how
        // tokio happens to schedule things. Bounded by a 1s
        // timeout so a regression that prevents `shutdown_via_ref`
        // from setting the flag doesn't hang the test.
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !bus.is_shutdown() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("first caller did not set shutdown flag within 1s");

        // The second caller's CAS will fail; it enters the spin
        // and times out at 50ms.
        let start = std::time::Instant::now();
        let second_result = bus.shutdown_via_ref().await;
        let elapsed = start.elapsed();

        // Reset the override for other tests, then drop the guard.
        super::set_shutdown_via_ref_spin_deadline_for_test(0);

        // CR-25 contract: the second caller MUST get a Transient
        // error, not a silent Ok.
        let err = second_result.expect_err(
            "CR-25 regression: second caller MUST surface AdapterError::Transient \
             when its deadline elapses, NOT a silent Ok",
        );
        match err {
            AdapterError::Transient(msg) => {
                assert!(
                    msg.contains("deadline elapsed") || msg.contains("mid-shutdown"),
                    "error message must reference the deadline path; got: {msg}"
                );
            }
            other => panic!("expected Transient, got {:?}", other),
        }

        // Sanity: the second caller's elapsed time is bounded by
        // the override (50ms) + scheduler slop. If this is
        // anywhere near the production 10s, the cfg(test)
        // override path broke.
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "second caller took {elapsed:?}; the cfg(test) deadline override \
             broke if this is near the 10s production default"
        );

        // Wait for the first caller to finish. Even though it
        // took the slow path, the bus IS shutting down and will
        // eventually complete.
        let _ = first_caller.await.unwrap();
        assert!(bus.is_shutdown_completed());
    }

    /// Regression: BUG_REPORT.md #6 — `shutdown()` must deliver every
    /// successfully-ingested event to the adapter before returning.
    /// Pins the broader durability contract that the
    /// `drain_finalize_ready` gate supports: the drain worker may not
    /// finalize until the in-flight wait completes.
    ///
    /// Tests across many shards with bursts large enough that the
    /// drain workers are mid-loop when shutdown begins.
    #[tokio::test]
    async fn shutdown_delivers_every_successful_ingest_to_adapter() {
        let received = Arc::new(AtomicU64::new(0));
        let adapter: Box<dyn crate::adapter::Adapter> = Box::new(CountingAdapter {
            received: received.clone(),
        });

        let config = EventBusConfig::builder()
            .num_shards(4)
            .ring_buffer_capacity(4096)
            .build()
            .unwrap();
        let bus = EventBus::new_with_adapter(config, adapter).await.unwrap();

        // Drive a sizable burst across all shards. Capacity > burst so
        // we don't trip backpressure; every successful Ok must reach
        // `on_batch` before shutdown returns.
        let total = 10_000usize;
        let mut successes = 0u64;
        for i in 0..total {
            if bus.ingest(Event::new(json!({"i": i}))).is_ok() {
                successes += 1;
            }
        }

        // Shutdown awaits drain workers; with the BUG_REPORT.md #6 fix
        // those workers wait on `drain_finalize_ready` after observing
        // `shutdown=true`, so any push the producer made before the
        // shutdown flag is guaranteed to be in the ring buffer when
        // the final sweep runs.
        bus.shutdown().await.unwrap();

        let delivered = received.load(AtomicOrdering::SeqCst);
        assert_eq!(
            delivered, successes,
            "shutdown stranded events: {successes} ingested successfully, \
             only {delivered} reached the adapter"
        );
    }

    /// Regression: BUG_REPORT.md #16 — `flush()` must be a delivery
    /// barrier: after it returns successfully, every event the
    /// caller successfully ingested before `flush()` was called
    /// must have been handed to the adapter via `on_batch`.
    /// The previous implementation slept a single `batch.max_delay`
    /// after the ring buffers drained, which left a window where
    /// events could still be sitting in the per-shard mpsc channel
    /// or inside a partially-filled batch awaiting timeout — those
    /// events were silently dropped from the flush guarantee.
    #[tokio::test]
    async fn flush_is_a_delivery_barrier() {
        let received = Arc::new(AtomicU64::new(0));
        let adapter: Box<dyn crate::adapter::Adapter> = Box::new(CountingAdapter {
            received: received.clone(),
        });

        // Use a deliberately *long* batch.max_delay (250ms) so that a
        // partially-filled batch sitting in the batch worker's
        // pending state would survive past the old single-`max_delay`
        // sleep. min_size > burst forces the partial-batch path.
        let config = EventBusConfig::builder()
            .num_shards(2)
            .ring_buffer_capacity(1024)
            .batch(crate::config::BatchConfig {
                min_size: 1_000,
                max_size: 10_000,
                max_delay: Duration::from_millis(250),
                adaptive: false,
                velocity_window: Duration::from_millis(100),
            })
            .build()
            .unwrap();
        let bus = EventBus::new_with_adapter(config, adapter).await.unwrap();

        // A small burst — far below `min_size`, so the batch worker
        // will sit on a partial batch waiting for `max_delay`.
        let burst = 50usize;
        let mut successes = 0u64;
        for i in 0..burst {
            if bus.ingest(Event::new(json!({"i": i}))).is_ok() {
                successes += 1;
            }
        }

        // Time the flush call to confirm we waited long enough for
        // the partial batch to time out. The previous code slept
        // ~10ms total in the post-empty phase; the fix waits up to
        // `max_delay * num_workers` (here 500ms cap, capped at 2s).
        let t0 = std::time::Instant::now();
        bus.flush().await.unwrap();
        let elapsed = t0.elapsed();

        // After flush returns, every successful ingest must have
        // been delivered to the adapter. With the old code this
        // assertion would fail: events sit in the partial batch
        // until `max_delay` (250ms) elapses, but flush returned
        // after only ~10ms.
        let delivered = received.load(AtomicOrdering::SeqCst);
        assert_eq!(
            delivered, successes,
            "flush() returned but only {delivered} of {successes} \
             events reached the adapter (#16); flush waited {:?}",
            elapsed
        );

        bus.shutdown().await.unwrap();
    }

    /// Regression (Phase 1): when configured with a
    /// persistent `producer_nonce_path`, two bus instances launched
    /// against the same path stamp the SAME nonce on every emitted
    /// batch. JetStream / Redis adapters key dedup on this nonce, so
    /// a producer that crashed mid-batch and restarted (within the
    /// backend's dedup window) issues retries with the same msg-ids
    /// and the backend correctly recognizes them as duplicates.
    ///
    /// Pre-fix the per-process nonce regenerated on every startup,
    /// so post-crash retries wrote NEW msg-ids and the backend
    /// persisted the partial-batch's accepted half twice.
    #[tokio::test]
    async fn persistent_producer_nonce_survives_bus_restart() {
        // Use a per-test temp file so concurrent runs don't collide.
        let mut nonce_path = std::env::temp_dir();
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        nonce_path.push(format!("net-test-bus-nonce-{pid}-{nanos}"));

        let make_config = |path: &std::path::Path| {
            EventBusConfig::builder()
                .num_shards(1)
                .ring_buffer_capacity(1024)
                .producer_nonce_path(path)
                .build()
                .unwrap()
        };

        // First bus: ingest one event. Read its nonce off the
        // adapter-bound batch via a recording adapter.
        struct NonceRecordingAdapter {
            nonce: Arc<parking_lot::Mutex<Option<u64>>>,
        }
        #[async_trait::async_trait]
        impl crate::adapter::Adapter for NonceRecordingAdapter {
            async fn init(&mut self) -> Result<(), AdapterError> {
                Ok(())
            }
            async fn on_batch(&self, batch: Batch) -> Result<(), AdapterError> {
                *self.nonce.lock() = Some(batch.process_nonce);
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
            ) -> Result<crate::adapter::ShardPollResult, AdapterError> {
                Ok(crate::adapter::ShardPollResult::empty())
            }
            fn name(&self) -> &'static str {
                "nonce-recording"
            }
        }

        let nonce_first_run = Arc::new(parking_lot::Mutex::new(None));
        {
            let bus = EventBus::new_with_adapter(
                make_config(&nonce_path),
                Box::new(NonceRecordingAdapter {
                    nonce: nonce_first_run.clone(),
                }),
            )
            .await
            .unwrap();
            bus.ingest(Event::new(json!({"i": 1}))).unwrap();
            bus.flush().await.unwrap();
            bus.shutdown().await.unwrap();
        }

        let nonce_second_run = Arc::new(parking_lot::Mutex::new(None));
        {
            let bus = EventBus::new_with_adapter(
                make_config(&nonce_path),
                Box::new(NonceRecordingAdapter {
                    nonce: nonce_second_run.clone(),
                }),
            )
            .await
            .unwrap();
            bus.ingest(Event::new(json!({"i": 2}))).unwrap();
            bus.flush().await.unwrap();
            bus.shutdown().await.unwrap();
        }

        let n_a = nonce_first_run
            .lock()
            .expect("first bus must have dispatched a batch");
        let n_b = nonce_second_run
            .lock()
            .expect("second bus must have dispatched a batch");
        assert_eq!(
            n_a, n_b,
            "two bus instances against the same producer_nonce_path \
             must stamp the same nonce — pre-fix this regenerated on \
             every restart and JetStream's dedup window saw new \
             msg-ids as fresh batches",
        );

        // Cleanup.
        let _ = std::fs::remove_file(&nonce_path);
    }

    /// Pin that ALL spawn sites — both the static initial-shard
    /// loop in `new_with_adapter` and the dynamic-add path in
    /// `add_shard_internal` — clone the bus's loaded
    /// `producer_nonce` correctly. Pre-#56 there was no nonce
    /// concept at the bus layer; if any future refactor drops the
    /// `producer_nonce: self.producer_nonce` line from one of the
    /// spawn sites (or stops loading the persistent path), the
    /// post-scale-up shard's batches would carry a different nonce
    /// and JetStream's cross-restart dedup would silently break for
    /// events ingested into the dynamic shard. Pin all observed
    /// batches across the static + dynamic shards share the bus's
    /// nonce.
    #[tokio::test]
    async fn multi_shard_bus_stamps_consistent_nonce_across_static_and_dynamic_shards() {
        let mut nonce_path = std::env::temp_dir();
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        nonce_path.push(format!("net-test-multi-shard-nonce-{pid}-{nanos}"));

        struct CollectingAdapter {
            nonces: Arc<parking_lot::Mutex<Vec<u64>>>,
        }
        #[async_trait::async_trait]
        impl crate::adapter::Adapter for CollectingAdapter {
            async fn init(&mut self) -> Result<(), AdapterError> {
                Ok(())
            }
            async fn on_batch(&self, batch: Batch) -> Result<(), AdapterError> {
                self.nonces.lock().push(batch.process_nonce);
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
            ) -> Result<crate::adapter::ShardPollResult, AdapterError> {
                Ok(crate::adapter::ShardPollResult::empty())
            }
            fn name(&self) -> &'static str {
                "collecting"
            }
        }

        let nonces = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let policy = ScalingPolicy {
            min_shards: 1,
            max_shards: 8,
            cooldown: Duration::from_nanos(1),
            ..Default::default()
        };
        let config = EventBusConfig::builder()
            .num_shards(2)
            .ring_buffer_capacity(1024)
            .scaling(policy)
            .producer_nonce_path(&nonce_path)
            .build()
            .unwrap();

        let bus = EventBus::new_with_adapter(
            config,
            Box::new(CollectingAdapter {
                nonces: nonces.clone(),
            }),
        )
        .await
        .unwrap();

        // Drive the two static shards.
        for i in 0..200u64 {
            let _ = bus.ingest(Event::new(json!({"i": i})));
        }
        bus.flush().await.unwrap();

        // Add a dynamic shard and drive it too.
        let _ = bus.manual_scale_up(1).await.unwrap();
        for i in 200..400u64 {
            let _ = bus.ingest(Event::new(json!({"i": i})));
        }
        bus.flush().await.unwrap();

        bus.shutdown().await.unwrap();

        let observed = nonces.lock().clone();
        assert!(
            !observed.is_empty(),
            "expected the adapter to have observed at least one batch",
        );
        let first = observed[0];
        for (i, &n) in observed.iter().enumerate() {
            assert_eq!(
                n, first,
                "batch {i} stamped a different nonce ({n:#x}) than the first \
                 batch ({first:#x}) — at least one spawn site (initial-shard \
                 loop or `add_shard_internal`) failed to inherit the bus's \
                 producer_nonce",
            );
        }

        let _ = std::fs::remove_file(&nonce_path);
    }

    /// Pin the within-process caching contract for the fallback
    /// (no-`producer_nonce_path`) path: two bus instances created
    /// in the same process see the SAME `batch_process_nonce()`
    /// because the helper is `OnceLock`-cached. The
    /// "different-across-restarts" semantic is a *process-level*
    /// guarantee — restart the process to get a fresh nonce — and
    /// is pinned by `persistent_producer_nonce_survives_bus_restart`
    /// (which uses a path; the without-path branch of #56 has no
    /// cross-restart guarantee by design).
    ///
    /// Cubic-ai P3: this test was previously named
    /// `process_nonce_fallback_differs_across_bus_instances`, which
    /// contradicted its own assertion (`assert_eq!(n_a, n_b)`).
    /// Renamed to match what it actually pins.
    #[tokio::test]
    async fn process_nonce_fallback_is_cached_within_process() {
        struct NonceRecordingAdapter {
            nonce: Arc<parking_lot::Mutex<Option<u64>>>,
        }
        #[async_trait::async_trait]
        impl crate::adapter::Adapter for NonceRecordingAdapter {
            async fn init(&mut self) -> Result<(), AdapterError> {
                Ok(())
            }
            async fn on_batch(&self, batch: Batch) -> Result<(), AdapterError> {
                *self.nonce.lock() = Some(batch.process_nonce);
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
            ) -> Result<crate::adapter::ShardPollResult, AdapterError> {
                Ok(crate::adapter::ShardPollResult::empty())
            }
            fn name(&self) -> &'static str {
                "nonce-recording"
            }
        }

        let cfg = || {
            EventBusConfig::builder()
                .num_shards(1)
                .ring_buffer_capacity(1024)
                .build()
                .unwrap()
        };

        let n_a = Arc::new(parking_lot::Mutex::new(None));
        let n_b = Arc::new(parking_lot::Mutex::new(None));
        {
            let bus = EventBus::new_with_adapter(
                cfg(),
                Box::new(NonceRecordingAdapter { nonce: n_a.clone() }),
            )
            .await
            .unwrap();
            bus.ingest(Event::new(json!({"i": 1}))).unwrap();
            bus.flush().await.unwrap();
            bus.shutdown().await.unwrap();
        }
        {
            let bus = EventBus::new_with_adapter(
                cfg(),
                Box::new(NonceRecordingAdapter { nonce: n_b.clone() }),
            )
            .await
            .unwrap();
            bus.ingest(Event::new(json!({"i": 2}))).unwrap();
            bus.flush().await.unwrap();
            bus.shutdown().await.unwrap();
        }

        // Note: in a single-process test BOTH bus instances see the
        // same `OnceLock`-cached `batch_process_nonce`, so the
        // nonces ARE equal here even though the documented
        // semantic is "fresh per process." This test pins the
        // cached-within-a-process invariant; the across-PROCESSES
        // semantic is exercised by the persistent-nonce test
        // above (which is the actually-load-bearing path for the
        // persistent-nonce fix).
        let n_a = n_a.lock().unwrap();
        let n_b = n_b.lock().unwrap();
        assert_eq!(
            n_a, n_b,
            "within one process, batch_process_nonce is OnceLock-cached \
             so two bus instances see the same nonce — the \
             different-across-restarts contract is process-level, \
             pinned via `persistent_producer_nonce_survives_bus_restart`",
        );
    }

    /// Regression: `EventBusStats::batches_dispatched`
    /// (and the new `events_dispatched`) must actually increment on
    /// every successful adapter dispatch. Pre-fix `batches_dispatched`
    /// was declared but never updated, so flush()'s Phase 2 progress
    /// gate was constant-zero and early-broke after one window —
    /// flake on Windows-class timer resolution. Pin both counters
    /// directly here so a future refactor that drops the increment
    /// fails this test, not the timing-dependent
    /// `flush_is_a_delivery_barrier`.
    #[tokio::test]
    async fn dispatch_increments_bus_level_event_and_batch_counters() {
        let received = Arc::new(AtomicU64::new(0));
        let adapter: Box<dyn crate::adapter::Adapter> = Box::new(CountingAdapter {
            received: received.clone(),
        });

        let config = EventBusConfig::builder()
            .num_shards(2)
            .ring_buffer_capacity(1024)
            .batch(crate::config::BatchConfig {
                min_size: 1,
                max_size: 10,
                max_delay: Duration::from_millis(10),
                adaptive: false,
                velocity_window: Duration::from_millis(100),
            })
            .build()
            .unwrap();
        let bus = EventBus::new_with_adapter(config, adapter).await.unwrap();

        for i in 0..50 {
            bus.ingest(Event::new(json!({"i": i}))).unwrap();
        }
        bus.flush().await.unwrap();

        let batches = bus.stats().batches_dispatched.load(AtomicOrdering::Acquire);
        let events = bus.stats().events_dispatched.load(AtomicOrdering::Acquire);
        assert!(
            batches > 0,
            "batches_dispatched must be > 0 after flush — pre-fix it was \
             never incremented anywhere, breaking flush()'s Phase 2 progress gate",
        );
        assert_eq!(
            events, 50,
            "events_dispatched must equal the number of events handed to \
             the adapter (got {events}, expected 50)",
        );

        bus.shutdown().await.unwrap();
    }

    /// Regression: BUG_REPORT.md #6 — drop-without-shutdown must
    /// still release the drain-finalize gate so detached drain
    /// workers can exit instead of parking on the gate until the
    /// internal `DRAIN_FINALIZE_TIMEOUT` deadline. Pinning this
    /// keeps the `Drop` impl honest if someone refactors the
    /// shutdown gates later.
    #[tokio::test]
    async fn drop_releases_drain_finalize_gate_promptly() {
        let config = EventBusConfig::builder()
            .num_shards(2)
            .ring_buffer_capacity(1024)
            .build()
            .unwrap();
        let bus = EventBus::new(config).await.unwrap();
        let drain_gate = bus.drain_finalize_ready.clone();

        // Drop without an awaited shutdown.
        drop(bus);

        // The Drop impl must have set the gate. `DRAIN_FINALIZE_TIMEOUT`
        // is 10s; if Drop didn't flip the gate, drain workers would
        // park for up to that long before exiting.
        assert!(
            drain_gate.load(AtomicOrdering::Acquire),
            "Drop must release `drain_finalize_ready` so detached drain \
             workers exit promptly"
        );
    }
}
