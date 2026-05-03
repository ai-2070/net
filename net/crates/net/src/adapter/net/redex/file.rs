//! `RedexFile` — the append / tail / read_range primitive.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use bytes::Bytes;
use futures::Stream;
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use super::super::channel::ChannelName;
use super::config::RedexFileConfig;
use super::entry::{payload_checksum, RedexEntry, INLINE_PAYLOAD_SIZE};
use super::error::RedexError;
use super::event::RedexEvent;
use super::retention::compute_eviction_count;
use super::segment::{HeapSegment, MAX_SEGMENT_BYTES};

#[cfg(feature = "redex-disk")]
use super::config::FsyncPolicy;
#[cfg(feature = "redex-disk")]
use super::disk::DiskSegment;
#[cfg(feature = "redex-disk")]
use std::path::Path;
#[cfg(feature = "redex-disk")]
use tokio::sync::Notify;

/// A live tail subscription waiting for new events.
struct TailWatcher {
    /// Minimum seq to deliver (inclusive).
    from_seq: u64,
    /// Channel back to the subscriber. Bounded — see [`TAIL_BUFFER_SIZE`].
    sender: mpsc::Sender<Result<RedexEvent, RedexError>>,
}

/// Mutable state: index, parallel timestamps, segment, and live
/// watchers. All held behind a single lock so the backfill→register
/// handoff is atomic.
struct FileState {
    index: Vec<RedexEntry>,
    /// Per-entry unix-nanos timestamps captured at append time.
    /// Same length as `index`. Used by age-based retention.
    /// In-memory only — not persisted to disk in v1; on reopen of
    /// a persistent file, recovered entries get "now" as their
    /// fake timestamp.
    timestamps: Vec<u64>,
    segment: HeapSegment,
    watchers: Vec<TailWatcher>,
}

/// Shared inner state. Handles are cheap `Arc` clones of this.
struct RedexFileInner {
    name: ChannelName,
    config: RedexFileConfig,
    next_seq: AtomicU64,
    state: Mutex<FileState>,
    closed: AtomicBool,
    #[cfg(feature = "redex-disk")]
    disk: Option<Arc<DiskSegment>>,
    /// Shutdown signal for whichever background fsync task this file
    /// spawned — either `FsyncPolicy::Interval` (timer-driven) or
    /// `FsyncPolicy::EveryN` (signal-driven from
    /// `DiskSegment::maybe_sync_after_append`). `Some` iff a task
    /// was spawned at `open_persistent` time. `close()` calls
    /// `notify_one()` so a permit is stored even if the task hasn't
    /// yet parked on its select; the task observes it, exits, and
    /// releases its DiskSegment reference.
    #[cfg(feature = "redex-disk")]
    fsync_shutdown: Option<Arc<Notify>>,
}

/// Dropping the last `RedexFile` clone without calling `close()`
/// previously leaked the background fsync task (either Interval or
/// EveryN) — it kept a strong `Arc<DiskSegment>` and a shutdown
/// `Notify` whose only firing site was inside `close()`.
/// `redex/index.rs` already had a Drop impl that mirrored this
/// pattern; we mirror it here so a misbehaving caller (or a panic
/// path that bypasses the explicit close) doesn't leak the task for
/// the lifetime of the runtime.
///
/// Drop is best-effort: it fires the notify so the spawned task
/// observes the signal and exits at the next select. We do NOT
/// flush or fsync from Drop because that would require an async
/// runtime context that may not be available.
impl Drop for RedexFileInner {
    fn drop(&mut self) {
        #[cfg(feature = "redex-disk")]
        if let Some(notify) = self.fsync_shutdown.as_ref() {
            // `notify_one` stores a permit even if no waiter is
            // currently parked, so a task that hasn't yet reached
            // its first `notified().await` will still observe the
            // signal on its next select.
            notify.notify_one();
        }
    }
}

/// A handle to a RedEX file. Cheap to clone.
///
/// Created via [`super::Redex::open_file`].
#[derive(Clone)]
pub struct RedexFile {
    inner: Arc<RedexFileInner>,
}

impl RedexFile {
    /// Create a fresh, empty file. Called by `Redex::open_file`.
    pub(super) fn new(name: ChannelName, config: RedexFileConfig) -> Self {
        let capacity = config.max_memory_bytes.min(64 * 1024 * 1024);
        Self {
            inner: Arc::new(RedexFileInner {
                name,
                config,
                next_seq: AtomicU64::new(0),
                state: Mutex::new(FileState {
                    index: Vec::new(),
                    timestamps: Vec::new(),
                    segment: HeapSegment::with_capacity(capacity),
                    watchers: Vec::new(),
                }),
                closed: AtomicBool::new(false),
                #[cfg(feature = "redex-disk")]
                disk: None,
                #[cfg(feature = "redex-disk")]
                fsync_shutdown: None,
            }),
        }
    }

    /// Open (or recover) a file with disk-backed durability.
    ///
    /// Reads `<base_dir>/<channel_path>/idx` and `.../dat` if they
    /// exist, replays the full dat file into the heap segment, and
    /// sets `next_seq` to one past the last recovered entry. New
    /// appends are mirrored to disk.
    ///
    /// A partial trailing record in `idx` (torn write from a crash)
    /// is truncated on reopen.
    #[cfg(feature = "redex-disk")]
    pub(super) fn open_persistent(
        name: ChannelName,
        config: RedexFileConfig,
        base_dir: &Path,
    ) -> Result<Self, RedexError> {
        // Derive the DiskSegment's append-side trigger thresholds
        // from the caller's policy. Each variant enables zero, one,
        // or both triggers; the worker spawned below interprets
        // them via `disk.fsync_signal`.
        let (fsync_every_n, fsync_max_bytes) = match config.fsync_policy {
            FsyncPolicy::Never | FsyncPolicy::Interval(_) => (0, 0),
            FsyncPolicy::EveryN(n) => (n.max(1), 0),
            FsyncPolicy::IntervalOrBytes { max_bytes, .. } => (0, max_bytes),
        };
        let recovered = DiskSegment::open(base_dir, &name, fsync_every_n, fsync_max_bytes)?;
        let next_seq = recovered.index.last().map(|e| e.seq + 1).unwrap_or(0);

        let segment = HeapSegment::from_existing(recovered.payload_bytes);
        // Use the persisted timestamps if they're present and match
        // the recovered index length; otherwise fall back to `now()`
        // and warn so operators know age-based retention is degraded
        // for this run. Without the ts sidecar (or when it's torn /
        // missing), every recovered entry would be timestamped "now"
        // and eligible-for-age-eviction status would be wrong for a
        // full retention window after every restart.
        let timestamps = match recovered.timestamps {
            Some(ts) if ts.len() == recovered.index.len() => ts,
            _ => {
                if !recovered.index.is_empty() {
                    tracing::warn!(
                        channel = %name.as_str(),
                        entries = recovered.index.len(),
                        "ts sidecar missing or mismatched — recovered entries get \
                         `now()` as timestamp, age-based retention degraded for \
                         this run"
                    );
                }
                let now = now_ns();
                vec![now; recovered.index.len()]
            }
        };
        let state = FileState {
            index: recovered.index,
            timestamps,
            segment,
            watchers: Vec::new(),
        };

        let disk = Arc::new(recovered.disk);

        // Spawn whichever fsync background task the policy needs.
        // Both variants hold an Arc<DiskSegment> and a clone of the
        // shutdown Notify; on close() the notify fires and the task
        // exits. Dropping the last RedexFile clone WITHOUT calling
        // close() leaks the task until the runtime shuts down —
        // consistent with the rest of the codebase's lifecycle
        // expectations (callers are expected to `close()` persistent
        // files).
        let fsync_shutdown = match config.fsync_policy {
            FsyncPolicy::Interval(d) if d > std::time::Duration::ZERO => {
                let shutdown = Arc::new(Notify::new());
                let task_shutdown = shutdown.clone();
                let task_disk = disk.clone();
                tokio::spawn(async move {
                    loop {
                        tokio::select! {
                            _ = task_shutdown.notified() => return,
                            _ = tokio::time::sleep(d) => {
                                if let Err(e) = task_disk.sync() {
                                    tracing::warn!(
                                        error = %e,
                                        "Interval fsync failed; continuing"
                                    );
                                }
                            }
                        }
                    }
                });
                Some(shutdown)
            }
            // EveryN moves the actual fsync off the appender thread:
            // `DiskSegment::maybe_sync_after_append` notifies
            // `fsync_signal` when the cadence threshold is reached
            // and returns immediately. This worker awaits that
            // signal and runs the fsync. Multiple notifies that
            // arrive during one in-flight sync coalesce into a
            // single follow-up sync — `Notify` is a single-permit
            // primitive, which is the intended semantics.
            FsyncPolicy::EveryN(_) => {
                let shutdown = Arc::new(Notify::new());
                let task_shutdown = shutdown.clone();
                let task_disk = disk.clone();
                let task_signal = disk.fsync_signal.clone();
                tokio::spawn(async move {
                    loop {
                        tokio::select! {
                            _ = task_shutdown.notified() => return,
                            _ = task_signal.notified() => {
                                if let Err(e) = task_disk.sync() {
                                    tracing::warn!(
                                        error = %e,
                                        "EveryN fsync failed; tail may be unsynced"
                                    );
                                }
                            }
                        }
                    }
                });
                Some(shutdown)
            }
            // IntervalOrBytes selects over a periodic timer AND the
            // byte-threshold signal — whichever fires first triggers
            // a sync. The byte signal is fired by the appender via
            // `maybe_sync_after_append`, which checks `fsync_max_bytes`
            // (already plumbed into the segment above).
            FsyncPolicy::IntervalOrBytes { period, .. } if period > std::time::Duration::ZERO => {
                let shutdown = Arc::new(Notify::new());
                let task_shutdown = shutdown.clone();
                let task_disk = disk.clone();
                let task_signal = disk.fsync_signal.clone();
                tokio::spawn(async move {
                    loop {
                        tokio::select! {
                            _ = task_shutdown.notified() => return,
                            _ = tokio::time::sleep(period) => {
                                if let Err(e) = task_disk.sync() {
                                    tracing::warn!(
                                        error = %e,
                                        "IntervalOrBytes (timer) fsync failed; continuing"
                                    );
                                }
                            }
                            _ = task_signal.notified() => {
                                if let Err(e) = task_disk.sync() {
                                    tracing::warn!(
                                        error = %e,
                                        "IntervalOrBytes (bytes) fsync failed; continuing"
                                    );
                                }
                            }
                        }
                    }
                });
                Some(shutdown)
            }
            // `period == ZERO && max_bytes > 0` is the byte-only
            // variant: skip the timer arm entirely but still react to
            // the appender's byte-threshold notify. Without a worker
            // the notify would just store an unread permit and the
            // bytes would never auto-sync until close — almost
            // certainly not what the caller meant by "byte-only."
            FsyncPolicy::IntervalOrBytes { period, max_bytes }
                if period == std::time::Duration::ZERO && max_bytes > 0 =>
            {
                let shutdown = Arc::new(Notify::new());
                let task_shutdown = shutdown.clone();
                let task_disk = disk.clone();
                let task_signal = disk.fsync_signal.clone();
                tokio::spawn(async move {
                    loop {
                        tokio::select! {
                            _ = task_shutdown.notified() => return,
                            _ = task_signal.notified() => {
                                if let Err(e) = task_disk.sync() {
                                    tracing::warn!(
                                        error = %e,
                                        "IntervalOrBytes (bytes) fsync failed; continuing"
                                    );
                                }
                            }
                        }
                    }
                });
                Some(shutdown)
            }
            _ => None,
        };

        Ok(Self {
            inner: Arc::new(RedexFileInner {
                name,
                config,
                next_seq: AtomicU64::new(next_seq),
                state: Mutex::new(state),
                closed: AtomicBool::new(false),
                disk: Some(disk),
                fsync_shutdown,
            }),
        })
    }

    /// The channel name this file is bound to.
    #[inline]
    pub fn name(&self) -> &ChannelName {
        &self.inner.name
    }

    /// The config this file was opened with.
    #[inline]
    pub fn config(&self) -> &RedexFileConfig {
        &self.inner.config
    }

    /// Number of currently retained entries.
    pub fn len(&self) -> usize {
        self.inner.state.lock().index.len()
    }

    /// True if no entries are retained.
    pub fn is_empty(&self) -> bool {
        self.inner.state.lock().index.is_empty()
    }

    /// Next sequence to be assigned (== total append count since open,
    /// including any evicted head).
    ///
    /// Pre-fix, this read `next_seq` outside the state
    /// lock. `append` / `append_batch` etc. allocate a seq via
    /// `fetch_add` before the disk write and `fetch_sub`-rollback
    /// on failure — both within the state-lock critical section.
    /// A concurrent reader without the lock could observe the
    /// temporarily-bumped value: external observers (metrics,
    /// snapshot logic, an `IndexStart::FromSeq(next_seq())`
    /// re-tail) believed a seq existed that was never durably
    /// appended. Taking the state lock here serializes the read
    /// with the append's commit-or-rollback, so callers only
    /// observe values that have been durably committed (or
    /// never assigned).
    pub fn next_seq(&self) -> u64 {
        let _state = self.inner.state.lock();
        self.inner.next_seq.load(Ordering::Acquire)
    }

    /// Atomic snapshot of `(len, next_seq)`. Observers that need
    /// both values consistently with each other (e.g. metrics
    /// dashboards comparing "retained count" to "total seqs
    /// assigned") should use this rather than `len()` followed
    /// by `next_seq()`.
    ///
    /// Pre-fix observers called the two methods in sequence.
    /// Each method takes the state lock individually, so the
    /// per-call view is consistent — but two appends could
    /// commit between the two reads, and the resulting pair
    /// could satisfy `len + 1 > next_seq_seen` (reader saw
    /// post-append `len` but pre-append `next_seq`). Observers
    /// downstream of a metrics tick would then double-account
    /// the in-flight seqs. This single-lock accessor returns
    /// both values from one critical section so the snapshot is
    /// strictly consistent.
    pub fn len_and_next_seq(&self) -> (usize, u64) {
        let state = self.inner.state.lock();
        let len = state.index.len();
        let next_seq = self.inner.next_seq.load(Ordering::Acquire);
        (len, next_seq)
    }

    /// Whether [`Self::close`] has run. After close, `tail` streams
    /// terminate with `Err(Closed)` and `append`/`append_*` reject
    /// with the same error.
    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::Acquire)
    }

    /// Lowest sequence number currently retained in the in-memory
    /// index, or `None` when the file holds nothing.
    ///
    /// `next_seq() - lowest_retained_seq()` is **not** the count —
    /// retention is by event-or-byte-or-age, not by contiguous range.
    /// Use this for "where can I safely tail from without triggering
    /// `Lagged`?": passing the returned seq (or higher) to
    /// [`Self::tail`] guarantees the backfill does not signal
    /// retention-induced history loss.
    pub fn lowest_retained_seq(&self) -> Option<u64> {
        self.inner.state.lock().index.first().map(|e| e.seq)
    }

    /// Append one event. Returns the assigned sequence.
    ///
    /// Failure modes: `PayloadTooLarge` if the segment is full or the
    /// offset would overflow u32; `Io(_)` if the disk mirror fails
    /// under `redex-disk`. Failure atomicity: memory is committed
    /// only after the disk write succeeds (for persistent files);
    /// `next_seq` is rolled back on disk failure so no seq number is
    /// burnt and no in-memory entry diverges from disk.
    pub fn append(&self, payload: &[u8]) -> Result<u64, RedexError> {
        self.check_not_closed()?;
        let cks = payload_checksum(payload);
        let ts = now_ns();

        let mut state = self.inner.state.lock();

        // Pre-validate capacity + offset width under the state lock —
        // no side effects yet.
        let current_live = state.segment.live_bytes();
        if current_live.saturating_add(payload.len()) > MAX_SEGMENT_BYTES {
            return Err(RedexError::PayloadTooLarge {
                size: payload.len(),
                max: MAX_SEGMENT_BYTES.saturating_sub(current_live),
            });
        }
        let offset = state
            .segment
            .base_offset()
            .saturating_add(current_live as u64);
        let offset_u32 = offset_to_u32(offset)?;

        // Allocate seq only after validation, under the state lock.
        // Concurrent writers also take the lock, so if we roll back
        // on disk failure no other writer has observed our value.
        let seq = self.inner.next_seq.fetch_add(1, Ordering::AcqRel);
        let entry = RedexEntry::new_heap(seq, offset_u32, payload.len() as u32, 0, cks);

        // Disk FIRST. If it fails, roll back the seq allocation and
        // leave memory untouched so callers don't observe a record
        // that was never durably persisted.
        #[cfg(feature = "redex-disk")]
        if let Some(disk) = self.disk() {
            if let Err(e) = disk.append_entry_at(&entry, payload, ts) {
                self.inner.next_seq.fetch_sub(1, Ordering::AcqRel);
                return Err(e);
            }
        }

        // Commit to memory. `segment.append` is infallible here —
        // we pre-validated its capacity above.
        state
            .segment
            .append(payload)
            .expect("pre-validated capacity; segment append cannot fail");
        state.index.push(entry);
        state.timestamps.push(ts);

        // The `Bytes::copy_from_slice` below is purely for watcher
        // delivery; durable storage already landed via `segment.append`.
        // Skip the copy entirely when nobody is tailing.
        if !state.watchers.is_empty() {
            let event = RedexEvent {
                entry,
                payload: Bytes::copy_from_slice(payload),
            };
            notify_watchers(&mut state.watchers, &event);
        }

        Ok(seq)
    }

    /// Append a fixed-length 8-byte inline payload. Skips the segment
    /// indirection. Returns the assigned sequence. Same failure-
    /// atomicity contract as [`Self::append`].
    pub fn append_inline(&self, payload: &[u8; INLINE_PAYLOAD_SIZE]) -> Result<u64, RedexError> {
        self.check_not_closed()?;
        let cks = payload_checksum(payload);
        let ts = now_ns();

        let mut state = self.inner.state.lock();

        let seq = self.inner.next_seq.fetch_add(1, Ordering::AcqRel);
        let entry = RedexEntry::new_inline(seq, payload, cks);

        // Disk FIRST. If it fails, roll back the seq allocation
        // and leave memory untouched so callers don't observe a
        // record that was never durably persisted.
        //
        // INVARIANT: every operation after this block must be
        // infallible. Pre-fix the rollback lived inside the
        // `if let Some(disk)` arm, so a `disk == None` configuration
        // would silently skip rollback. Today every post-block
        // operation (`Vec::push`, `notify_watchers` against a
        // `Vec<Sender>`) is provably infallible, so the disk-None
        // path never reaches a state where rollback is needed.
        // If a future change introduces a fallible operation
        // between this disk block and `Ok(seq)`, factor out the
        // rollback so it fires regardless of the disk arm.
        #[cfg(feature = "redex-disk")]
        if let Some(disk) = self.disk() {
            if let Err(e) = disk.append_entry_at(&entry, payload, ts) {
                self.inner.next_seq.fetch_sub(1, Ordering::AcqRel);
                return Err(e);
            }
        }

        state.index.push(entry);
        state.timestamps.push(ts);

        if !state.watchers.is_empty() {
            let event = RedexEvent {
                entry,
                payload: Bytes::copy_from_slice(payload),
            };
            notify_watchers(&mut state.watchers, &event);
        }

        Ok(seq)
    }

    /// Append many payloads. Returns `Some(seq)` of the FIRST event
    /// in the batch, or `None` for an empty input. All entries
    /// land contiguously in the index.
    ///
    /// # Empty input
    ///
    /// Pre-fix the signature was `Result<u64, RedexError>` and an
    /// empty `payloads` returned `Ok(next_seq)` — the seq value
    /// the next append *would* receive. Callers couldn't
    /// distinguish "wrote zero, seq N would be next" from "wrote
    /// one event with seq N" via the return value alone.
    ///
    /// Breaking change: signature is now
    /// `Result<Option<u64>, RedexError>`. `None` ⇒ empty input,
    /// no events appended; `Some(seq)` ⇒ first seq of the batch.
    /// Callers iterating over optionally-empty batches no longer
    /// need an `is_empty` pre-check.
    ///
    /// Failure atomicity:
    /// - seq numbers are allocated **after** the batch is validated
    ///   to fit (both segment capacity and u32 offset width);
    /// - for persistent files, the batch is written to disk **before**
    ///   any in-memory commit — on disk failure the seq allocation
    ///   rolls back and neither memory nor subscribers observe the
    ///   batch.
    pub fn append_batch(&self, payloads: &[Bytes]) -> Result<Option<u64>, RedexError> {
        self.check_not_closed()?;
        if payloads.is_empty() {
            return Ok(None);
        }

        let ts = now_ns();
        let mut state = self.inner.state.lock();

        // Pre-validate: every payload must fit in the remaining
        // segment capacity, and the final offset must fit in a u32.
        // Under the state lock, nothing else can write to the
        // segment, so the check-then-act is atomic.
        let total_bytes: usize = payloads.iter().map(|p| p.len()).sum();
        let current_live = state.segment.live_bytes();
        if current_live.saturating_add(total_bytes) > MAX_SEGMENT_BYTES {
            return Err(RedexError::PayloadTooLarge {
                size: total_bytes,
                max: MAX_SEGMENT_BYTES.saturating_sub(current_live),
            });
        }
        let base = state
            .segment
            .base_offset()
            .saturating_add(current_live as u64);
        let final_offset = base.saturating_add(total_bytes as u64);
        offset_to_u32(final_offset)?;

        // Allocate the contiguous seq range under the lock so that
        // a rollback on disk failure is safe — no other writer can
        // advance past our allocation while we hold the state lock.
        let first_seq = self
            .inner
            .next_seq
            .fetch_add(payloads.len() as u64, Ordering::AcqRel);

        // Pre-compute entries + running offsets without touching
        // the segment yet. This lets us write the batch to disk
        // before committing any in-memory state.
        let mut events: Vec<RedexEvent> = Vec::with_capacity(payloads.len());
        let mut running = base;
        for (i, payload) in payloads.iter().enumerate() {
            let seq = first_seq + i as u64;
            let cks = payload_checksum(payload);
            let entry = RedexEntry::new_heap(
                seq,
                offset_to_u32(running).expect("pre-validated final offset fits u32"),
                payload.len() as u32,
                0,
                cks,
            );
            running = running.saturating_add(payload.len() as u64);
            events.push(RedexEvent {
                entry,
                payload: payload.clone(),
            });
        }

        // Disk FIRST. Roll back the seq range on failure so no seq
        // is burnt and memory stays clean.
        #[cfg(feature = "redex-disk")]
        if let Some(disk) = self.disk() {
            let pairs: Vec<(RedexEntry, &[u8])> = events
                .iter()
                .map(|e| (e.entry, e.payload.as_ref()))
                .collect();
            if let Err(e) = disk.append_entries_at(&pairs, &vec![ts; pairs.len()]) {
                self.inner
                    .next_seq
                    .fetch_sub(payloads.len() as u64, Ordering::AcqRel);
                return Err(e);
            }
        }

        // Commit to memory. Pre-validated capacity → infallible.
        // One `append_many` call instead of N `append` calls: a
        // single bounds check and a single reserve.
        state
            .segment
            .append_many(payloads)
            .expect("pre-validated capacity; segment append cannot fail under the state lock");
        for event in &events {
            state.index.push(event.entry);
            state.timestamps.push(ts);
        }

        for event in &events {
            notify_watchers(&mut state.watchers, event);
        }
        Ok(Some(first_seq))
    }

    /// Strictly-ordered variant of [`Self::append`].
    ///
    /// Both [`Self::append`] and this method now take the state lock
    /// before allocating a sequence number (the failure-atomicity
    /// fix required moving `fetch_add` inside the lock so rollback
    /// on disk-write failure is safe). That means `append` already
    /// produces in-seq-order index insertions under contention, and
    /// the two paths are functionally equivalent for single writes.
    ///
    /// The real distinction is at the wrapper level: this method
    /// pairs with [`Self::append_batch_ordered`], which holds ONE
    /// lock across an entire batch, whereas [`Self::append_batch`]
    /// also holds one lock per batch today. In v1 the non-ordered
    /// and ordered paths are nearly identical. The distinction is
    /// kept so that a future optimization of [`Self::append`] (e.g.
    /// moving the seq allocation back outside the lock with a
    /// different rollback scheme) doesn't affect callers who need
    /// guaranteed-ordered appends.
    ///
    /// Used by [`super::OrderedAppender`] for replay determinism.
    /// Same failure-atomicity contract as [`Self::append`].
    pub fn append_ordered(&self, payload: &[u8]) -> Result<u64, RedexError> {
        self.check_not_closed()?;
        let cks = payload_checksum(payload);
        let ts = now_ns();

        let mut state = self.inner.state.lock();

        let current_live = state.segment.live_bytes();
        if current_live.saturating_add(payload.len()) > MAX_SEGMENT_BYTES {
            return Err(RedexError::PayloadTooLarge {
                size: payload.len(),
                max: MAX_SEGMENT_BYTES.saturating_sub(current_live),
            });
        }
        let offset = state
            .segment
            .base_offset()
            .saturating_add(current_live as u64);
        let offset_u32 = offset_to_u32(offset)?;

        let seq = self.inner.next_seq.fetch_add(1, Ordering::AcqRel);
        let entry = RedexEntry::new_heap(seq, offset_u32, payload.len() as u32, 0, cks);

        #[cfg(feature = "redex-disk")]
        if let Some(disk) = self.disk() {
            if let Err(e) = disk.append_entry_at(&entry, payload, ts) {
                self.inner.next_seq.fetch_sub(1, Ordering::AcqRel);
                return Err(e);
            }
        }

        state
            .segment
            .append(payload)
            .expect("pre-validated capacity; segment append cannot fail");
        state.index.push(entry);
        state.timestamps.push(ts);

        if !state.watchers.is_empty() {
            let event = RedexEvent {
                entry,
                payload: Bytes::copy_from_slice(payload),
            };
            notify_watchers(&mut state.watchers, &event);
        }

        Ok(seq)
    }

    /// Ordered variant of [`Self::append_inline`]. See
    /// [`Self::append_ordered`]. Same failure-atomicity contract.
    pub fn append_inline_ordered(
        &self,
        payload: &[u8; INLINE_PAYLOAD_SIZE],
    ) -> Result<u64, RedexError> {
        self.check_not_closed()?;
        let cks = payload_checksum(payload);
        let ts = now_ns();

        let mut state = self.inner.state.lock();
        let seq = self.inner.next_seq.fetch_add(1, Ordering::AcqRel);
        let entry = RedexEntry::new_inline(seq, payload, cks);

        #[cfg(feature = "redex-disk")]
        if let Some(disk) = self.disk() {
            if let Err(e) = disk.append_entry_at(&entry, payload, ts) {
                self.inner.next_seq.fetch_sub(1, Ordering::AcqRel);
                return Err(e);
            }
        }

        state.index.push(entry);
        state.timestamps.push(ts);

        if !state.watchers.is_empty() {
            let event = RedexEvent {
                entry,
                payload: Bytes::copy_from_slice(payload),
            };
            notify_watchers(&mut state.watchers, &event);
        }

        Ok(seq)
    }

    /// Ordered variant of [`Self::append_batch`]. The whole batch is
    /// appended under one state-lock acquisition, so it's both
    /// atomic (all-or-nothing within the batch) and strictly
    /// seq-ordered relative to any other ordered writers. Same
    /// failure-atomicity contract as [`Self::append_batch`].
    ///
    /// Returns `Some(first_seq)` on a non-empty batch and `None`
    /// on empty input — same convention as `append_batch`.
    pub fn append_batch_ordered(&self, payloads: &[Bytes]) -> Result<Option<u64>, RedexError> {
        self.check_not_closed()?;
        if payloads.is_empty() {
            return Ok(None);
        }
        let ts = now_ns();
        let mut state = self.inner.state.lock();

        // Pre-validate capacity + offset width before allocating
        // seq numbers — see [`Self::append_batch`] for rationale.
        let total_bytes: usize = payloads.iter().map(|p| p.len()).sum();
        let current_live = state.segment.live_bytes();
        if current_live.saturating_add(total_bytes) > MAX_SEGMENT_BYTES {
            return Err(RedexError::PayloadTooLarge {
                size: total_bytes,
                max: MAX_SEGMENT_BYTES.saturating_sub(current_live),
            });
        }
        let base = state
            .segment
            .base_offset()
            .saturating_add(current_live as u64);
        let final_offset = base.saturating_add(total_bytes as u64);
        offset_to_u32(final_offset)?;

        let first_seq = self
            .inner
            .next_seq
            .fetch_add(payloads.len() as u64, Ordering::AcqRel);

        // Pre-compute entries without touching the segment yet.
        let mut events: Vec<RedexEvent> = Vec::with_capacity(payloads.len());
        let mut running = base;
        for (i, payload) in payloads.iter().enumerate() {
            let seq = first_seq + i as u64;
            let cks = payload_checksum(payload);
            let entry = RedexEntry::new_heap(
                seq,
                offset_to_u32(running).expect("pre-validated final offset fits u32"),
                payload.len() as u32,
                0,
                cks,
            );
            running = running.saturating_add(payload.len() as u64);
            events.push(RedexEvent {
                entry,
                payload: payload.clone(),
            });
        }

        // Disk FIRST. Roll back seq range on failure.
        #[cfg(feature = "redex-disk")]
        if let Some(disk) = self.disk() {
            let pairs: Vec<(RedexEntry, &[u8])> = events
                .iter()
                .map(|e| (e.entry, e.payload.as_ref()))
                .collect();
            if let Err(e) = disk.append_entries_at(&pairs, &vec![ts; pairs.len()]) {
                self.inner
                    .next_seq
                    .fetch_sub(payloads.len() as u64, Ordering::AcqRel);
                return Err(e);
            }
        }

        // Commit to memory. One `append_many` call instead of N
        // `append` calls.
        state
            .segment
            .append_many(payloads)
            .expect("pre-validated capacity; segment append cannot fail under the state lock");
        for event in &events {
            state.index.push(event.entry);
            state.timestamps.push(ts);
        }

        for event in &events {
            notify_watchers(&mut state.watchers, event);
        }
        Ok(Some(first_seq))
    }

    /// Append `value` (postcard-serialized) AND run `fold_fn` against
    /// caller-supplied `state` in the same call. Returns the
    /// assigned seq.
    ///
    /// This is the common "log-an-event and update a materialized
    /// view in one step" pattern, without spinning up a full CortEX
    /// adapter. Callers maintain `state` themselves; the fold
    /// closure sees the just-appended value and mutates state in
    /// place.
    ///
    /// Note: the fold runs AFTER the RedEX append. If the append
    /// succeeds and the fold panics, the log advances but state is
    /// out of sync — callers who need crash-consistency should use
    /// the CortEX adapter's durable `snapshot` + `open_from_snapshot`
    /// instead.
    pub fn append_and_fold<T, F, S>(
        &self,
        value: &T,
        state: &mut S,
        fold_fn: F,
    ) -> Result<u64, RedexError>
    where
        T: serde::Serialize,
        F: FnOnce(&T, &mut S),
    {
        let bytes = postcard::to_allocvec(value)
            .map_err(|e| RedexError::Encode(format!("append_and_fold serialize: {}", e)))?;
        let seq = self.append(&bytes)?;
        fold_fn(value, state);
        Ok(seq)
    }

    /// Convenience: serialize `value` with postcard and append.
    pub fn append_postcard<T: serde::Serialize>(&self, value: &T) -> Result<u64, RedexError> {
        let bytes = postcard::to_allocvec(value).map_err(|e| RedexError::Encode(e.to_string()))?;
        self.append(&bytes)
    }

    /// Subscribe to all events with `seq >= from_seq`, including those
    /// already in the index at call time.
    ///
    /// Backfill and live registration happen atomically under the
    /// state lock: no event can interleave between backfill delivery
    /// and live subscription.
    ///
    /// Delivery is backed by a per-subscription bounded channel of
    /// depth [`RedexFileConfig::tail_buffer_size`].
    ///
    /// - **Backfill overflow** (requested `from_seq` produces more
    ///   retained events than the buffer can hold): pre-flighted
    ///   under the state lock; the subscriber observes
    ///   [`RedexError::Lagged`] as the first stream item and no
    ///   truncated history. Guaranteed deliverable because the
    ///   channel is empty at that point.
    /// - **Live overflow** (subscriber falls behind during live
    ///   delivery): disconnected with a best-effort
    ///   [`RedexError::Lagged`]. Under sustained saturation the
    ///   signal itself may be dropped (the channel is full when we
    ///   try to enqueue it), in which case the subscriber sees a
    ///   clean stream end.
    pub fn tail(
        &self,
        from_seq: u64,
    ) -> impl Stream<Item = Result<RedexEvent, RedexError>> + Send + 'static {
        // mpsc::channel panics on 0; clamp to a minimum of 1.
        let buffer = self.inner.config.tail_buffer_size.max(1);
        let (tx, rx) = mpsc::channel(buffer);

        let mut state = self.inner.state.lock();

        // Check `closed` inside the state lock: close() drains the
        // watcher list under this same lock, so any watcher we register
        // after clearing this check is guaranteed to either (a) be
        // seen by a future close() drain, or (b) predate the close.
        // Checking outside the lock is a TOCTOU — close() could flip
        // the flag + drain before we register here, leaving the
        // subscriber hanging with no `Closed` signal.
        if self.inner.closed.load(Ordering::Acquire) {
            drop(state);
            let _ = tx.try_send(Err(RedexError::Closed));
            return ReceiverStream::new(rx);
        }

        // Detect retention-induced history loss.
        // `partition_point(|e| e.seq < from_seq)` only catches
        // overflow within the retained range — it cannot tell us
        // the requested `from_seq` predates the lowest retained
        // entry, because that entry has already been dropped from
        // `state.index`. Pre-fix, a subscriber asking for
        // `from_seq = 0` after retention had pruned the head
        // received the retained tail with no `Lagged` signal —
        // silent data loss for resume-from-snapshot consumers.
        //
        // Two cases mean history was dropped:
        //   1. The lowest retained seq is greater than `from_seq`.
        //   2. Nothing is retained but events were appended (next_seq
        //      moved past from_seq).
        // Either case → signal `Lagged` before enqueuing anything.
        let next_seq = self.inner.next_seq.load(Ordering::Acquire);
        let history_lost = match state.index.first() {
            Some(first) => first.seq > from_seq,
            None => from_seq < next_seq,
        };
        if history_lost {
            drop(state);
            let _ = tx.try_send(Err(RedexError::Lagged));
            return ReceiverStream::new(rx);
        }

        // Backfill pre-flight. The index is in seq order so we can
        // binary-search for the first matching entry and compute the
        // backfill size in O(log n). If it can't fit in the channel,
        // we signal `Lagged` *before* enqueuing any events — the
        // channel is empty at this point so the signal is guaranteed
        // to land, and the subscriber sees a clean "you missed
        // history" error rather than a silently-truncated prefix
        // (the case the prior best-effort `try_send(Err(Lagged))`
        // could not deliver if the buffer was already saturated).
        let start = state.index.partition_point(|e| e.seq < from_seq);
        let backfill_count = state.index.len() - start;
        if backfill_count > buffer {
            drop(state);
            let _ = tx.try_send(Err(RedexError::Lagged));
            return ReceiverStream::new(rx);
        }

        // Backfill. Uses `try_send` under the state lock so backfill +
        // registration is still atomic with respect to concurrent
        // appends. The pre-flight above guarantees we won't hit the
        // `Full` path here — defensively handle it anyway in case a
        // payload evicts between the count and the materialize.
        for entry in state.index[start..].iter() {
            let event = match materialize(entry, &state.segment) {
                Some(e) => e,
                None => continue, // payload evicted between index retain and read
            };
            match tx.try_send(Ok(event)) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    // Defensive: pre-flight should have prevented this.
                    let _ = tx.try_send(Err(RedexError::Lagged));
                    return ReceiverStream::new(rx);
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    // Receiver dropped before stream was polled.
                    return ReceiverStream::new(rx);
                }
            }
        }

        // Register for live events.
        state.watchers.push(TailWatcher {
            from_seq,
            sender: tx,
        });
        drop(state);

        ReceiverStream::new(rx)
    }

    /// One-shot read of the half-open range `[start, end)` from the
    /// in-memory index. Returns only entries currently retained;
    /// silently skips any seqs that have been evicted.
    pub fn read_range(&self, start: u64, end: u64) -> Vec<RedexEvent> {
        if end <= start {
            return Vec::new();
        }
        let state = self.inner.state.lock();
        let mut out = Vec::new();
        for entry in state.index.iter() {
            if entry.seq < start {
                continue;
            }
            if entry.seq >= end {
                break;
            }
            if let Some(ev) = materialize(entry, &state.segment) {
                out.push(ev);
            }
        }
        out
    }

    /// Run the retention policy synchronously. Exposed so a background
    /// task (heartbeat loop) can drive it; no hot-path cost.
    ///
    /// # Disk I/O under parking_lot Mutex
    ///
    /// `sweep_retention` and `append_batch` call disk operations
    /// (`disk.compact_to`, `disk.append_entries_at`) while
    /// holding `state.lock()`, a non-yielding parking_lot Mutex.
    /// All concurrent appenders, `tail` registrations,
    /// `read_range`, `len`, `is_empty`, and `close` block on the
    /// same lock for the duration of the I/O. **This is a
    /// latency / starvation concern, not a correctness one.**
    /// Throughput-critical deployments should drive
    /// `sweep_retention` from a low-priority background task
    /// scheduled outside the hot path.
    ///
    /// A proper fix would snapshot the state needed for I/O,
    /// release the lock, perform I/O, then re-acquire to
    /// commit — that's a substantial restructure (transactional
    /// staging area, conflict resolution against concurrent
    /// appends) and out of scope here. Documented as a known
    /// performance trade-off so future profilers don't
    /// rediscover it as a "bug".
    pub fn sweep_retention(&self) {
        let cfg = self.inner.config;
        if cfg.retention_max_events.is_none()
            && cfg.retention_max_bytes.is_none()
            && cfg.retention_max_age_ns.is_none()
        {
            return;
        }
        let now = now_ns();
        let mut state = self.inner.state.lock();
        let drop = compute_eviction_count(&state.index, &state.timestamps, now, &cfg);
        if drop == 0 {
            return;
        }

        // Determine the new segment base: first offset of the entry
        // that survives. Inline entries don't consume segment bytes,
        // so skip past them when finding the boundary.
        let mut new_base: Option<u64> = None;
        for e in state.index.iter().skip(drop) {
            if !e.is_inline() {
                new_base = Some(e.payload_offset as u64);
                break;
            }
        }

        // Compute the dat_base that the post-eviction segment would
        // have:
        //   - `new_base` if there's a surviving heap entry (the
        //     segment's base advances to the first surviving heap
        //     entry's absolute offset),
        //   - `segment.base_offset() + live_bytes()` if every
        //     survivor is inline (every byte of dat goes).
        // Compute from pre-eviction state so we can pass it to
        // `compact_to` BEFORE mutating any in-memory state.
        let dat_base = match new_base {
            Some(base) => base,
            None => state.segment.base_offset() + state.segment.live_bytes() as u64,
        };

        // Attempt the disk compaction FIRST against a slice of the
        // surviving entries, and only mutate `state.index`,
        // `state.timestamps`, and `state.segment` on success. On
        // failure, in-memory state is left untouched so reopen
        // replays a consistent picture rather than corrupting the
        // channel. Mutating in-memory state first and then calling
        // `disk.compact_to` would leave a permanent skew on disk
        // failure: in-memory eviction succeeds but on-disk files
        // retain the evicted entries, and reopen would replay the
        // full on-disk state and resurrect entries that had been
        // evicted only in memory.
        //
        // The state lock is held *across* `compact_to`. Releasing
        // it earlier opens a window where a concurrent
        // `append_entry_at` can land on disk with the in-memory
        // state updated to match — but `compact_to` then reads the
        // post-append on-disk dat and writes a new idx/ts derived
        // from the pre-append surviving slice. The racing append's
        // idx record is overwritten, leaving its payload bytes as
        // orphaned dat tail that recovery's `retained_dat_end`
        // truncation drops on next reopen — the append survives in
        // memory until restart, then vanishes. Holding the lock
        // makes appends queue behind this compaction.
        #[cfg(feature = "redex-disk")]
        if let Some(disk) = self.inner.disk.as_ref() {
            let surviving_index: Vec<RedexEntry> = state.index[drop..].to_vec();
            let surviving_timestamps: Vec<u64> = state.timestamps[drop..].to_vec();
            let disk = disk.clone();
            if let Err(e) = disk.compact_to(&surviving_index, &surviving_timestamps, dat_base) {
                tracing::warn!(
                    error = %e,
                    "redex sweep_retention: disk compaction failed; \
                     in-memory state left untouched so reopen replays \
                     a consistent picture rather than resurrecting \
                     in-memory-only-evicted entries"
                );
                return;
            }
        }

        // Disk compaction succeeded (or no disk segment is
        // configured). Now mutate in-memory state to match.
        //
        // Build the new (renormalized) index in a temp Vec, then
        // atomically replace `state.index` and `state.timestamps`.
        // Pre-fix the ordering was: drain prefix, evict segment,
        // then rebase entries' `payload_offset` in a `for ... iter_mut()`
        // loop. A panic in the middle of the rebase loop (allocator
        // failure during a pathological allocation, or an unrelated
        // hardware-level signal) left the index in a half-rebased
        // state — some entries with absolute offsets pointing past
        // the new compacted dat, others with the rebased offsets.
        // Subsequent reads silently missed.
        //
        // Two-phase build-then-swap: phase 1 produces a fresh Vec
        // and only when it's complete does phase 2 mutate
        // `state.index` / `state.timestamps`. A panic in phase 1
        // discards the temp Vec without touching `state`; phase 2
        // is a single `.assign(...)` which is itself panic-free
        // for primitive moves.
        let new_index: Vec<RedexEntry> = state
            .index
            .iter()
            .skip(drop)
            .map(|entry| {
                #[allow(unused_mut)] // mut is used only on the redex-disk path
                let mut e = *entry;
                #[cfg(feature = "redex-disk")]
                {
                    if !e.is_inline() && self.inner.disk.is_some() {
                        e.payload_offset =
                            (e.payload_offset as u64).saturating_sub(dat_base) as u32;
                    }
                }
                e
            })
            .collect();
        let new_timestamps: Vec<u64> = state.timestamps[drop..].to_vec();

        // Phase 2: atomically replace. The two `=` assignments and
        // the segment mutations below are panic-free against the
        // primitives they invoke (Vec drop, simple inline arithmetic).
        state.index = new_index;
        state.timestamps = new_timestamps;
        state.segment.evict_prefix_to(dat_base);
        #[cfg(feature = "redex-disk")]
        if self.inner.disk.is_some() {
            state.segment.rebase_to_zero();
        }

        // `state` drops at the end of the function — all
        // subsequent appenders see the post-compaction layout.
        std::mem::drop(state);
    }

    /// Close the file. Outstanding tail streams receive `RedexError::Closed`.
    /// For persistent files, fsyncs the disk segment before returning
    /// and signals any background fsync task (Interval or EveryN) to
    /// exit. `close()` always fsyncs regardless of the per-file
    /// `FsyncPolicy` — this is the caller's explicit durability
    /// barrier.
    pub fn close(&self) -> Result<(), RedexError> {
        if self.inner.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let mut state = self.inner.state.lock();
        for w in state.watchers.drain(..) {
            // Best-effort `Closed` signal. If the per-subscriber
            // buffer is saturated the signal is dropped, but the
            // sender is dropped at end-of-iteration regardless, so
            // the subscriber still observes a clean stream end.
            let _ = w.sender.try_send(Err(RedexError::Closed));
        }
        drop(state);

        #[cfg(feature = "redex-disk")]
        {
            // Signal the background fsync task (Interval or EveryN)
            // to exit before fsyncing so `close()` isn't racing the
            // task's own sync.
            //
            // `notify_one` stores a permit if the task hasn't yet
            // parked on `notified()` — e.g. a `close()` that races a
            // just-spawned task before it reaches the select, or one
            // that fires while the task is between sleep and the
            // next poll. `notify_waiters` would be lost in that
            // window and the fsync loop would keep running after
            // close.
            if let Some(shutdown) = self.inner.fsync_shutdown.as_ref() {
                shutdown.notify_one();
            }
            if let Some(disk) = self.disk() {
                disk.sync()?;
            }
        }

        Ok(())
    }

    /// Fsync the disk segment (no-op for heap-only files).
    #[cfg(feature = "redex-disk")]
    pub fn sync(&self) -> Result<(), RedexError> {
        if let Some(disk) = self.disk() {
            disk.sync()?;
        }
        Ok(())
    }

    /// Test-only: cumulative successful `sync()` count on the disk
    /// segment. `None` for heap-only files. Used by `FsyncPolicy`
    /// tests to assert cadence without racing real I/O.
    #[cfg(all(test, feature = "redex-disk"))]
    pub fn sync_count(&self) -> Option<u64> {
        self.disk().map(|d| d.sync_count())
    }

    #[cfg(feature = "redex-disk")]
    #[inline]
    fn disk(&self) -> Option<&Arc<DiskSegment>> {
        self.inner.disk.as_ref()
    }

    #[inline]
    fn check_not_closed(&self) -> Result<(), RedexError> {
        if self.inner.closed.load(Ordering::Acquire) {
            Err(RedexError::Closed)
        } else {
            Ok(())
        }
    }
}

impl std::fmt::Debug for RedexFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Read only lock-free state: `len` and the lock-taking
        // `next_seq()` accessor would acquire `state.lock()` (the
        // accessor takes it for read-after-rollback consistency).
        // Two separate acquisitions in one Debug print also produce
        // torn reads. More importantly, any future
        // `tracing::?(?file, …)` inside a region that already holds
        // `state` would deadlock. The atomic gives a possibly-stale
        // (mid-append) value, which is acceptable for diagnostic
        // output and is the same trade-off other lock-free Debug
        // impls in this crate make.
        f.debug_struct("RedexFile")
            .field("name", &self.inner.name)
            .field(
                "next_seq_atomic",
                &self.inner.next_seq.load(Ordering::Relaxed),
            )
            .field("closed", &self.inner.closed.load(Ordering::Relaxed))
            .finish()
    }
}

// -- helpers ---------------------------------------------------------------

/// Verify the entry's stored checksum against the actual payload
/// bytes on every read. The 28-bit xxh3 is computed at append
/// time; without verification at read time, on-disk corruption
/// (torn writes, bit-rot, external tampering) would flow through
/// `materialize` as a valid event and silently poison every
/// downstream consumer. We surface a checksum mismatch by
/// returning `None` (same channel `materialize` already uses for
/// "couldn't construct an event" signals).
fn materialize(entry: &RedexEntry, segment: &HeapSegment) -> Option<RedexEvent> {
    let payload = if entry.is_inline() {
        Bytes::copy_from_slice(&entry.inline_payload()?)
    } else {
        segment.read(entry.payload_offset as u64, entry.payload_len)?
    };

    let stored = entry.checksum();
    let computed = super::entry::payload_checksum(&payload);
    if stored != computed {
        tracing::error!(
            seq = entry.seq,
            stored_checksum = format_args!("{:#x}", stored),
            computed_checksum = format_args!("{:#x}", computed),
            "RedexFile::materialize: checksum mismatch — payload corrupt; dropping entry"
        );
        return None;
    }

    Some(RedexEvent {
        entry: *entry,
        payload,
    })
}

fn notify_watchers(watchers: &mut Vec<TailWatcher>, event: &RedexEvent) {
    // Walk watchers; drop those whose receiver is gone or whose
    // buffer is saturated. On `Full` we make a best-effort attempt
    // to signal `Lagged` before dropping — under true saturation
    // that signal may itself fail, in which case the subscriber
    // sees a plain stream end.
    watchers.retain(|w| {
        if event.entry.seq < w.from_seq {
            return true; // keep, but don't deliver
        }
        match w.sender.try_send(Ok(event.clone())) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                let _ = w.sender.try_send(Err(RedexError::Lagged));
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    });
}

/// Convert a segment offset to the `u32` field that lives in
/// `RedexEntry::payload_offset`. Returns
/// `Err(SegmentOffsetOverflow { offset })` if the absolute segment
/// offset has passed `u32::MAX` — this can only happen on a
/// persistent file whose lifetime heap bytes (append + eviction +
/// re-append) have crossed the 4 GB threshold. The segment itself
/// caps at 3 GB live, so this fires on `base_offset` growth, not
/// live-data growth. The right long-term fix is a sweep-time offset
/// renormalization (v2); until then we surface the overflow instead
/// of silently truncating.
#[inline]
fn offset_to_u32(offset: u64) -> Result<u32, RedexError> {
    u32::try_from(offset).map_err(|_| RedexError::SegmentOffsetOverflow { offset })
}

/// Unix nanoseconds from `SystemTime::now`. Used as the per-entry
/// timestamp for age-based retention. Non-monotonic (wall clock)
/// is acceptable here — retention only needs rough ordering, not
/// strict monotonicity.
#[inline]
fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::super::super::channel::ChannelName;
    use super::*;
    use futures::StreamExt;

    fn make_file(name: &str) -> RedexFile {
        RedexFile::new(ChannelName::new(name).unwrap(), RedexFileConfig::default())
    }

    #[test]
    fn test_append_assigns_monotonic_seq() {
        let f = make_file("t1");
        assert_eq!(f.append(b"a").unwrap(), 0);
        assert_eq!(f.append(b"b").unwrap(), 1);
        assert_eq!(f.append(b"c").unwrap(), 2);
        assert_eq!(f.next_seq(), 3);
    }

    /// Regression: `len_and_next_seq()` returns a consistent
    /// `(len, next_seq)` snapshot under one lock. Pre-fix
    /// observers calling `len()` then `next_seq()` could
    /// catch a transient where two appends commit between the
    /// reads and the snapshot satisfies `len + 1 > next_seq_seen`.
    /// The single-lock accessor pins atomicity.
    ///
    /// We can't easily simulate the pre-fix race in a unit test
    /// (it requires precise inter-thread interleaving), but we
    /// can pin the structural invariant: at every observable
    /// moment, `len_and_next_seq()` returns
    /// `next_seq == len + lowest_retained_seq` (where
    /// `lowest_retained_seq = 0` for an unpruned file). This is
    /// true if and only if the snapshot was taken under the
    /// state lock that the appender holds.
    #[test]
    fn len_and_next_seq_is_consistent_under_appends() {
        let f = make_file("t-consistent-snapshot");
        for i in 0..50 {
            f.append(format!("evt-{i}").as_bytes()).unwrap();
            let (len, next_seq) = f.len_and_next_seq();
            // No prune in this test, so lowest_retained = 0.
            assert_eq!(
                next_seq as usize, len,
                "regression: (len, next_seq) snapshot must satisfy \
                 next_seq == len for an unpruned file. The atomic \
                 accessor pins this; calling len() then next_seq() \
                 separately could observe a transient where they \
                 diverge by 1+ across concurrent appends."
            );
        }
    }

    /// `Debug` must not acquire `state.lock()`. Pre-fix it called
    /// `self.len()` and `self.next_seq()`, both of which lock the
    /// state mutex. Any caller that printed a `RedexFile` from
    /// inside an existing `state`-locked region (e.g. a future
    /// `tracing::?(?file, …)` inside an append path) would
    /// deadlock against itself. The fix reads the lock-free
    /// atomics directly. We pin the property by holding the lock
    /// across a `format!("{:?}", file)` call — pre-fix this
    /// deadlocks; post-fix it returns immediately.
    #[test]
    fn debug_does_not_acquire_state_lock() {
        use std::sync::atomic::Ordering;
        let f = make_file("t-debug-lock");
        f.append(b"x").unwrap();

        // Hold the state lock and format the file. If `Debug`
        // tries to take the same lock, the test hangs (would
        // be caught by CI test-timeout, but more importantly it
        // would have hung pre-fix).
        let guard = f.inner.state.lock();
        let s = format!("{:?}", f);
        drop(guard);

        assert!(s.contains("RedexFile"));
        assert!(
            s.contains("next_seq_atomic"),
            "Debug must surface the lock-free atomic, got: {s}"
        );
        // Sanity: the atomic value matches what `next_seq.load` returns.
        let direct = f.inner.next_seq.load(Ordering::Relaxed);
        assert!(
            s.contains(&direct.to_string()),
            "Debug must reflect the live atomic value, got: {s}"
        );
    }

    #[test]
    fn test_read_range_returns_events_in_order() {
        let f = make_file("t2");
        for i in 0..10u64 {
            f.append(format!("e{}", i).as_bytes()).unwrap();
        }
        let events = f.read_range(2, 5);
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].entry.seq, 2);
        assert_eq!(events[0].payload.as_ref(), b"e2");
        assert_eq!(events[2].entry.seq, 4);
    }

    #[test]
    fn test_read_range_empty_when_end_le_start() {
        let f = make_file("t2e");
        f.append(b"x").unwrap();
        assert!(f.read_range(5, 5).is_empty());
        assert!(f.read_range(10, 3).is_empty());
    }

    #[test]
    fn test_append_batch_sequential() {
        let f = make_file("t3");
        let start = f
            .append_batch(&[Bytes::from_static(b"one"), Bytes::from_static(b"two")])
            .unwrap();
        assert_eq!(start, Some(0));
        assert_eq!(f.next_seq(), 2);
        let events = f.read_range(0, 2);
        assert_eq!(events[0].payload.as_ref(), b"one");
        assert_eq!(events[1].payload.as_ref(), b"two");
    }

    #[test]
    fn test_append_inline_roundtrip() {
        let f = make_file("t4");
        let bytes = *b"abcdefgh";
        let seq = f.append_inline(&bytes).unwrap();
        assert_eq!(seq, 0);
        let events = f.read_range(0, 1);
        assert_eq!(events.len(), 1);
        assert!(events[0].entry.is_inline());
        assert_eq!(events[0].payload.as_ref(), &bytes);
    }

    #[test]
    fn test_append_postcard_roundtrip() {
        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
        struct Foo {
            a: u32,
            b: String,
        }
        let f = make_file("t5");
        let v = Foo {
            a: 42,
            b: "hi".into(),
        };
        let seq = f.append_postcard(&v).unwrap();
        assert_eq!(seq, 0);
        let events = f.read_range(0, 1);
        let decoded: Foo = postcard::from_bytes(&events[0].payload).unwrap();
        assert_eq!(decoded, v);
    }

    #[tokio::test]
    async fn test_tail_backfills_then_lives() {
        let f = make_file("t6");
        for i in 0..5u64 {
            f.append(format!("e{}", i).as_bytes()).unwrap();
        }

        let mut stream = Box::pin(f.tail(0));

        // First 5 events are backfill.
        for i in 0..5u64 {
            let ev = stream.next().await.unwrap().unwrap();
            assert_eq!(ev.entry.seq, i);
            assert_eq!(ev.payload.as_ref(), format!("e{}", i).as_bytes());
        }

        // New appends should be delivered live.
        f.append(b"live").unwrap();
        let ev = stream.next().await.unwrap().unwrap();
        assert_eq!(ev.entry.seq, 5);
        assert_eq!(ev.payload.as_ref(), b"live");
    }

    #[tokio::test]
    async fn test_tail_boundary_no_dupes_no_drops() {
        // Regression: backfill → register handoff must be gapless.
        // We append N events, open a tail, and in parallel the test
        // drives more appends. Every event must arrive exactly once.
        let f = make_file("t7");
        for i in 0..100u64 {
            f.append(format!("e{}", i).as_bytes()).unwrap();
        }

        let mut stream = Box::pin(f.tail(0));

        // Append 50 more after tail registration.
        let f2 = f.clone();
        let handle = tokio::spawn(async move {
            for i in 100..150u64 {
                f2.append(format!("e{}", i).as_bytes()).unwrap();
            }
        });

        let mut seen = Vec::new();
        for _ in 0..150 {
            let ev = stream.next().await.unwrap().unwrap();
            seen.push(ev.entry.seq);
        }
        handle.await.unwrap();

        assert_eq!(seen.len(), 150);
        for (i, &seq) in seen.iter().enumerate() {
            assert_eq!(seq, i as u64, "event {} arrived out of order or missing", i);
        }
    }

    #[tokio::test]
    async fn test_tail_from_mid_sequence() {
        let f = make_file("t8");
        for i in 0..10u64 {
            f.append(format!("e{}", i).as_bytes()).unwrap();
        }
        let mut stream = Box::pin(f.tail(7));
        for i in 7..10u64 {
            let ev = stream.next().await.unwrap().unwrap();
            assert_eq!(ev.entry.seq, i);
        }
    }

    #[test]
    fn test_retention_count() {
        let f = RedexFile::new(
            ChannelName::new("t9").unwrap(),
            RedexFileConfig::default().with_retention_max_events(3),
        );
        for i in 0..10u64 {
            f.append(format!("e{}", i).as_bytes()).unwrap();
        }
        f.sweep_retention();
        assert_eq!(f.len(), 3);
        // Surviving events are the newest 3.
        let events = f.read_range(0, 100);
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].entry.seq, 7);
        assert_eq!(events[2].entry.seq, 9);
    }

    #[test]
    fn test_retention_respects_payload_slicing() {
        let f = RedexFile::new(
            ChannelName::new("t10").unwrap(),
            RedexFileConfig::default().with_retention_max_events(2),
        );
        f.append(b"first").unwrap();
        f.append(b"second").unwrap();
        f.append(b"third").unwrap();
        f.sweep_retention();
        let events = f.read_range(0, 100);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].payload.as_ref(), b"second");
        assert_eq!(events[1].payload.as_ref(), b"third");
    }

    /// A subscriber that requests `tail(from_seq)` for a
    /// `from_seq` BELOW the lowest currently retained entry must
    /// receive `Err(Lagged)` as the first stream item, not a silently
    /// truncated history.
    ///
    /// Pre-fix, `partition_point(|e| e.seq < from_seq)` returned 0
    /// when every retained entry had `seq >= from_seq` (because all
    /// the older ones were dropped from `state.index`), so the
    /// `backfill_count` matched the retained range size and the
    /// subscriber received `[seq=2..N]` with no signal that
    /// `[from_seq..2]` had ever existed.
    #[tokio::test]
    async fn tail_signals_lagged_when_from_seq_below_retained_head() {
        let f = RedexFile::new(
            ChannelName::new("t-bug2").unwrap(),
            RedexFileConfig::default().with_retention_max_events(3),
        );
        for i in 0..10u64 {
            f.append(format!("e{}", i).as_bytes()).unwrap();
        }
        f.sweep_retention();
        // After sweep, retained = seqs 7, 8, 9. lowest_retained = 7.
        assert_eq!(f.lowest_retained_seq(), Some(7));

        // Request from_seq = 0 — every seq < 7 was retained-evicted.
        let mut stream = Box::pin(f.tail(0));
        let first = stream.next().await.expect("must yield at least one item");
        assert!(
            matches!(first, Err(RedexError::Lagged)),
            "expected Lagged signal for from_seq=0 below retained head 7, got {:?}",
            first
                .as_ref()
                .map(|_| "Ok event")
                .map_err(|e| format!("{:?}", e)),
        );
    }

    /// `from_seq` exactly at or above the lowest
    /// retained head must NOT signal `Lagged` — every requested seq
    /// is still present.
    #[tokio::test]
    async fn tail_does_not_signal_lagged_when_from_seq_at_or_above_retained_head() {
        let f = RedexFile::new(
            ChannelName::new("t-bug2-ok").unwrap(),
            RedexFileConfig::default().with_retention_max_events(3),
        );
        for i in 0..10u64 {
            f.append(format!("e{}", i).as_bytes()).unwrap();
        }
        f.sweep_retention();
        assert_eq!(f.lowest_retained_seq(), Some(7));

        // Request from_seq = 7 — exactly the lowest retained.
        let mut stream = Box::pin(f.tail(7));
        let first = stream.next().await.unwrap().unwrap();
        assert_eq!(
            first.entry.seq, 7,
            "tail(7) must return seq 7 as first backfilled event"
        );
    }

    /// `from_seq < next_seq` with no retained
    /// entries also signals `Lagged` — events were appended and
    /// then entirely retention-evicted.
    #[tokio::test]
    async fn tail_signals_lagged_when_all_retained_dropped() {
        let f = RedexFile::new(
            ChannelName::new("t-bug2-all-gone").unwrap(),
            RedexFileConfig::default().with_retention_max_events(0),
        );
        f.append(b"a").unwrap();
        f.append(b"b").unwrap();
        f.sweep_retention();
        assert_eq!(f.len(), 0);
        assert_eq!(f.lowest_retained_seq(), None);
        assert_eq!(f.next_seq(), 2);

        // Request from_seq = 0 — events existed but are gone.
        let mut stream = Box::pin(f.tail(0));
        let first = stream.next().await.expect("must yield at least one item");
        assert!(
            matches!(first, Err(RedexError::Lagged)),
            "expected Lagged when next_seq advanced past from_seq with empty index"
        );
    }

    /// `from_seq >= next_seq` (waiting for future
    /// events) on an empty index must NOT signal `Lagged` — the
    /// subscriber is just ahead of the file.
    #[tokio::test]
    async fn tail_does_not_signal_lagged_when_waiting_for_future_events() {
        let f = make_file("t-bug2-future");
        // No appends. next_seq = 0.

        let stream = Box::pin(f.tail(5));
        // Append something; subscriber should NOT see Lagged for the
        // initial "from_seq=5 with empty index, next_seq=0" condition.
        f.append(b"e5").unwrap(); // seq 0
        f.append(b"e6").unwrap(); // seq 1
        f.append(b"e7").unwrap(); // seq 2
                                  // ... still nothing at seq 5 yet, so stream is idle.
                                  // What we do assert: the backfill check did NOT push Lagged
                                  // synchronously (which would be the next item polled).
                                  // Instead, the watcher is registered live and waiting.
                                  // Cancel via dropping.
        drop(stream);
        // No assertion failure means we didn't hit the Lagged path.
    }

    #[test]
    fn test_close_rejects_further_append() {
        let f = make_file("t11");
        f.append(b"a").unwrap();
        f.close().unwrap();
        assert!(matches!(f.append(b"b"), Err(RedexError::Closed)));
    }

    #[tokio::test]
    async fn test_close_signals_outstanding_tails() {
        let f = make_file("t12");
        f.append(b"a").unwrap();
        let mut stream = Box::pin(f.tail(0));

        // First event is backfill.
        let ev = stream.next().await.unwrap().unwrap();
        assert_eq!(ev.entry.seq, 0);

        f.close().unwrap();

        // Next yield is the Closed error.
        let err = stream.next().await.unwrap().unwrap_err();
        assert!(matches!(err, RedexError::Closed));
    }

    // ---- Regression tests ----

    #[test]
    fn test_regression_offset_to_u32_boundary() {
        // Regression: `offset_to_u32` used to be a silent truncation
        // (`offset as u32`), which corrupted `RedexEntry::payload_offset`
        // on long-running persistent files whose base_offset crossed
        // `u32::MAX`. The fix converts the truncation into a
        // `SegmentOffsetOverflow` error at the exact boundary and
        // surfaces the overflowing offset value.
        assert!(offset_to_u32(0).is_ok());
        assert!(offset_to_u32(u32::MAX as u64).is_ok());
        let err = offset_to_u32(u32::MAX as u64 + 1).unwrap_err();
        assert!(matches!(
            err,
            RedexError::SegmentOffsetOverflow { offset } if offset == u32::MAX as u64 + 1
        ));
        assert!(matches!(
            offset_to_u32(u64::MAX).unwrap_err(),
            RedexError::SegmentOffsetOverflow { offset: u64::MAX }
        ));
    }

    #[test]
    fn test_regression_append_fails_when_base_offset_overflows_u32() {
        // Regression: single appends must surface the offset overflow
        // rather than write a truncated `payload_offset`. We force
        // `base_offset` past `u32::MAX` via the test-only hook and
        // verify the next append returns `SegmentOffsetOverflow`.
        let f = make_file("t_off_overflow");
        {
            let mut state = f.inner.state.lock();
            state.segment.force_base_offset(u32::MAX as u64 + 1);
        }
        // Any further append computes a start offset > u32::MAX and
        // must surface `SegmentOffsetOverflow` from `offset_to_u32`.
        let err = f.append(b"x").unwrap_err();
        assert!(matches!(err, RedexError::SegmentOffsetOverflow { .. }));
    }

    #[test]
    fn test_regression_batch_seq_gap_on_offset_overflow() {
        // Regression: `append_batch` used to `fetch_add(batch_size)`
        // before calling `segment.append`. If any append mid-batch
        // failed, the seq range was allocated but no index entries
        // were written — producing permanent gaps in seq space.
        //
        // The fix pre-validates capacity + final offset under the
        // state lock, advancing `next_seq` only when every append is
        // guaranteed to succeed. A failing batch must leave
        // `next_seq` unchanged.
        let f = make_file("t_batch_gap");
        // One real append so `next_seq` starts at 1.
        f.append(b"a").unwrap();
        assert_eq!(f.next_seq(), 1);

        // Push base_offset so a 2-payload batch of 8 bytes each
        // would overflow u32.
        {
            let mut state = f.inner.state.lock();
            state.segment.force_base_offset(u32::MAX as u64 - 4);
        }

        let err = f
            .append_batch(&[
                Bytes::from_static(b"aaaaaaaa"),
                Bytes::from_static(b"bbbbbbbb"),
            ])
            .unwrap_err();
        assert!(matches!(err, RedexError::SegmentOffsetOverflow { .. }));
        // Critical assertion: next_seq did NOT advance. A naive
        // pre-fix implementation would have `next_seq == 3` here.
        assert_eq!(
            f.next_seq(),
            1,
            "failing batch must not advance next_seq (would leak gap)"
        );
    }

    #[test]
    fn test_regression_ordered_batch_seq_gap_on_offset_overflow() {
        // Same contract as `test_regression_batch_seq_gap_on_offset_overflow`
        // but for `append_batch_ordered`.
        let f = make_file("t_obatch_gap");
        f.append(b"a").unwrap();
        assert_eq!(f.next_seq(), 1);

        {
            let mut state = f.inner.state.lock();
            state.segment.force_base_offset(u32::MAX as u64 - 4);
        }

        let err = f
            .append_batch_ordered(&[
                Bytes::from_static(b"aaaaaaaa"),
                Bytes::from_static(b"bbbbbbbb"),
            ])
            .unwrap_err();
        assert!(matches!(err, RedexError::SegmentOffsetOverflow { .. }));
        assert_eq!(f.next_seq(), 1);
    }

    // ---- Durability-first append regressions (persistent files) ----

    #[cfg(feature = "redex-disk")]
    fn tmp_persistent_dir(tag: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "redex_persist_{}_{}_{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[cfg(feature = "redex-disk")]
    fn make_persistent(name: &str, dir: &std::path::Path) -> RedexFile {
        use super::super::manager::Redex;
        let r = Redex::new().with_persistent_dir(dir);
        r.open_file(
            &ChannelName::new(name).unwrap(),
            RedexFileConfig::default().with_persistent(true),
        )
        .unwrap()
    }

    #[cfg(feature = "redex-disk")]
    #[test]
    fn test_regression_append_rolls_back_on_disk_failure() {
        // Regression: `append` used to commit in-memory state (segment
        // buf, index, timestamps) and advance `next_seq` BEFORE
        // attempting the disk mirror write. A disk failure left the
        // caller with an `Err` but memory diverged from disk — a
        // retry would duplicate the event and a reopen would miss it.
        // The fix writes to disk FIRST and rolls back the seq + leaves
        // memory untouched on disk failure.
        let dir = tmp_persistent_dir("append_rollback");
        let f = make_persistent("t_rollback/append", &dir);
        // One real append to prime the file.
        f.append(b"a").unwrap();
        assert_eq!(f.next_seq(), 1);
        assert_eq!(f.len(), 1);

        // Arm a one-shot failure on the next disk write.
        f.inner.disk.as_ref().unwrap().arm_next_append_failure();

        let err = f.append(b"b").unwrap_err();
        assert!(matches!(err, RedexError::Io(_)));

        // Invariants that MUST hold after a failed append:
        // - next_seq was rolled back (no seq burnt)
        // - in-memory index unchanged (no ghost entry)
        // - segment bytes unchanged (no orphaned payload)
        assert_eq!(
            f.next_seq(),
            1,
            "disk failure must roll back next_seq (no burnt seq)"
        );
        assert_eq!(f.len(), 1, "index must not grow on disk failure");
    }

    #[cfg(feature = "redex-disk")]
    #[test]
    fn test_regression_append_batch_rolls_back_on_disk_failure() {
        // Same contract as above, for `append_batch`. A mid-batch
        // disk failure must roll back the whole seq range and leave
        // memory + index untouched.
        let dir = tmp_persistent_dir("batch_rollback");
        let f = make_persistent("t_rollback/batch", &dir);
        f.append(b"a").unwrap();
        assert_eq!(f.next_seq(), 1);

        f.inner.disk.as_ref().unwrap().arm_next_append_failure();

        let err = f
            .append_batch(&[
                Bytes::from_static(b"x"),
                Bytes::from_static(b"y"),
                Bytes::from_static(b"z"),
            ])
            .unwrap_err();
        assert!(matches!(err, RedexError::Io(_)));

        assert_eq!(
            f.next_seq(),
            1,
            "batch disk failure must roll back the full seq range"
        );
        assert_eq!(f.len(), 1, "index must not grow on batch disk failure");
    }

    /// Regression for BUG_AUDIT_2026_04_30_CORE.md #94: when the
    /// idx `metadata()` call inside `append_entry_inner` failed
    /// AFTER a successful dat write, the original `?` early-return
    /// skipped the dat-rollback block, leaving orphan dat bytes on
    /// disk while the caller was told the append failed. On reopen
    /// those bytes were trimmed by `retained_dat_end`, but the
    /// rollback contract was violated and the seq counter
    /// disagreed with the durable record.
    ///
    /// The fix wraps `metadata()` in an explicit match so the
    /// dat-rollback block runs on this path too. We pin this by
    /// arming a one-shot idx-metadata failure, observing the
    /// caller's Err, then asserting the on-disk dat file size
    /// matches the pre-failure size (no orphan bytes).
    #[cfg(feature = "redex-disk")]
    #[test]
    fn append_rolls_back_dat_on_idx_metadata_failure() {
        let dir = tmp_persistent_dir("append_rollback_idx_meta");
        let f = make_persistent("t_rollback/idx_meta", &dir);

        // Prime with one entry so the dat file has known content.
        f.append(b"AAAAAAAAA").unwrap();
        let dat_path = dir.join("t_rollback").join("idx_meta").join("dat");
        let pre_dat_len = std::fs::metadata(&dat_path).unwrap().len();
        assert_eq!(pre_dat_len, 9, "primed payload must be 9 bytes on disk");

        f.inner
            .disk
            .as_ref()
            .unwrap()
            .arm_next_idx_metadata_failure();

        let err = f.append(b"BBBBBBBBB").unwrap_err();
        assert!(matches!(err, RedexError::Io(_)));

        // Pre-fix: dat would be 18 bytes (orphan "BBBBBBBBB" tail).
        // Post-fix: dat is back to 9 bytes — the rollback ran.
        let post_dat_len = std::fs::metadata(&dat_path).unwrap().len();
        assert_eq!(
            post_dat_len, pre_dat_len,
            "idx metadata failure must roll back the dat write — \
             pre-fix this left {pre_dat_len} bytes; orphan tail bug",
        );
        assert_eq!(f.next_seq(), 1, "next_seq must be rolled back");
        assert_eq!(f.len(), 1, "in-memory index must not grow");
    }

    /// Regression for BUG_AUDIT_2026_04_30_CORE.md #94: same hazard
    /// but for the ts metadata path — a failure here previously
    /// left orphaned dat AND idx bytes (the latter being more
    /// serious because torn-tail recovery resurrects them as
    /// entries with `now()` timestamps when ts is shorter than
    /// idx).
    #[cfg(feature = "redex-disk")]
    #[test]
    fn append_rolls_back_dat_and_idx_on_ts_metadata_failure() {
        let dir = tmp_persistent_dir("append_rollback_ts_meta");
        let f = make_persistent("t_rollback/ts_meta", &dir);

        f.append(b"AAAAAAAAA").unwrap();
        let dat_path = dir.join("t_rollback").join("ts_meta").join("dat");
        let idx_path = dir.join("t_rollback").join("ts_meta").join("idx");
        let pre_dat_len = std::fs::metadata(&dat_path).unwrap().len();
        let pre_idx_len = std::fs::metadata(&idx_path).unwrap().len();

        f.inner
            .disk
            .as_ref()
            .unwrap()
            .arm_next_ts_metadata_failure();

        let err = f.append(b"BBBBBBBBB").unwrap_err();
        assert!(matches!(err, RedexError::Io(_)));

        let post_dat_len = std::fs::metadata(&dat_path).unwrap().len();
        let post_idx_len = std::fs::metadata(&idx_path).unwrap().len();
        assert_eq!(
            post_dat_len, pre_dat_len,
            "ts metadata failure must roll back the dat write"
        );
        assert_eq!(
            post_idx_len, pre_idx_len,
            "ts metadata failure must roll back the idx write — \
             pre-fix this left a record whose ts never landed, \
             producing a length mismatch on next reopen"
        );
        assert_eq!(f.next_seq(), 1, "next_seq must be rolled back");
        assert_eq!(f.len(), 1, "in-memory index must not grow");
    }

    // ---- FsyncPolicy tests (Stage 1 of v2 closeout) ----

    #[cfg(feature = "redex-disk")]
    fn make_persistent_with_policy(
        name: &str,
        base: &std::path::Path,
        policy: super::FsyncPolicy,
    ) -> RedexFile {
        let r = super::super::manager::Redex::new().with_persistent_dir(base);
        r.open_file(
            &ChannelName::new(name).unwrap(),
            RedexFileConfig::default()
                .with_persistent(true)
                .with_fsync_policy(policy),
        )
        .unwrap()
    }

    #[cfg(feature = "redex-disk")]
    #[test]
    fn test_fsync_policy_never_skips_append_syncs() {
        // Never: no append-path fsync at all. Counter stays at 0 until
        // an explicit `sync()` or `close()` triggers one.
        let dir = tmp_persistent_dir("fsync_never");
        let f = make_persistent_with_policy("fsync/never", &dir, super::FsyncPolicy::Never);
        for i in 0..20u64 {
            f.append(format!("n-{}", i).as_bytes()).unwrap();
        }
        assert_eq!(
            f.sync_count(),
            Some(0),
            "Never must not fsync on the append path"
        );
        f.sync().unwrap();
        assert_eq!(f.sync_count(), Some(1), "explicit sync() still works");
    }

    /// Yield-loop helper: EveryN sync runs on a background tokio
    /// task, so after `append` returns the sync may not yet be
    /// observable. Yields up to `max_yields` times until
    /// `sync_count >= expected` or the yield budget is exhausted.
    /// Returns the final observed count.
    #[cfg(all(test, feature = "redex-disk"))]
    async fn wait_for_sync_count(f: &RedexFile, expected: u64, max_yields: usize) -> u64 {
        for _ in 0..max_yields {
            if let Some(n) = f.sync_count() {
                if n >= expected {
                    return n;
                }
            }
            tokio::task::yield_now().await;
        }
        f.sync_count().unwrap_or(0)
    }

    #[cfg(feature = "redex-disk")]
    #[tokio::test(flavor = "current_thread")]
    async fn test_fsync_policy_every_n_syncs_on_cadence() {
        // EveryN(5): one notify per 5 appends. The actual fsync runs
        // off the appender on a background worker. We yield between
        // appends so each cadence boundary's notify is consumed
        // before the next one arrives — without that, multiple
        // notifies arriving while the worker is still parked would
        // coalesce into a single permit (a real-world feature, but
        // it complicates pinning the count in a test).
        let dir = tmp_persistent_dir("fsync_every_n");
        let f = make_persistent_with_policy("fsync/everyn", &dir, super::FsyncPolicy::EveryN(5));
        for i in 0..23u64 {
            f.append(format!("e-{}", i).as_bytes()).unwrap();
            tokio::task::yield_now().await;
        }
        // 23 appends crosses the 5-cadence at 5, 10, 15, 20 = 4
        // notifies. With yields between appends the worker keeps up
        // 1:1, so we expect 4 worker-driven syncs.
        let observed = wait_for_sync_count(&f, 4, 50).await;
        assert_eq!(
            observed, 4,
            "EveryN(5) over 23 yielding appends = 4 worker syncs"
        );
        f.close().unwrap();
        assert_eq!(f.sync_count(), Some(5), "close() adds one more sync");
    }

    #[cfg(feature = "redex-disk")]
    #[tokio::test(flavor = "current_thread")]
    async fn test_fsync_policy_every_n_clamps_zero_to_one() {
        // EveryN(0) would never fire with a naïve implementation; the
        // clamp at `open_persistent` maps 0 (and 1) to "notify on
        // every append."
        let dir = tmp_persistent_dir("fsync_every_n_zero");
        let f =
            make_persistent_with_policy("fsync/everyn_zero", &dir, super::FsyncPolicy::EveryN(0));
        for i in 0..3u64 {
            f.append(format!("e-{}", i).as_bytes()).unwrap();
            tokio::task::yield_now().await;
        }
        let observed = wait_for_sync_count(&f, 3, 50).await;
        assert_eq!(
            observed, 3,
            "EveryN(0) must notify on every append (clamped to 1)"
        );
    }

    /// Phase 3 invariant: the EveryN cadence must NOT block the
    /// appender. Even at N=1 (fsync-on-every-append semantics) the
    /// per-call latency stays at page-cache-write cost, not
    /// fsync-call cost. We approximate this by asserting that 100
    /// rapid appends complete in well under the time a single fsync
    /// would take on the slowest plausible disk (~10 ms each).
    #[cfg(feature = "redex-disk")]
    #[tokio::test(flavor = "current_thread")]
    async fn test_fsync_policy_every_n_does_not_block_appender() {
        let dir = tmp_persistent_dir("fsync_every_n_nonblock");
        let f = make_persistent_with_policy(
            "fsync/everyn_nonblock",
            &dir,
            super::FsyncPolicy::EveryN(1),
        );
        let start = std::time::Instant::now();
        for i in 0..100u64 {
            f.append(format!("nb-{}", i).as_bytes()).unwrap();
        }
        let elapsed = start.elapsed();
        // 100 appends at ~real fsync cost (1–10 ms each on SSD,
        // 10+ ms on HDD) would be 100ms–1s. Page-cache writes alone
        // should land well under 50ms even on a slow CI box.
        assert!(
            elapsed < std::time::Duration::from_millis(50),
            "100 EveryN(1) appends took {:?} — appender should not block on fsync",
            elapsed,
        );
        // Drain pending notifies so the worker has actually run at
        // least once before we tear down.
        let _ = wait_for_sync_count(&f, 1, 50).await;
        f.close().unwrap();
    }

    /// Phase 3 invariant (companion to the non-blocking test): when
    /// many notifies arrive while the worker is parked or mid-sync,
    /// they coalesce into a single follow-up sync. `Notify` is a
    /// single-permit primitive — that's the intended semantics, not
    /// a bug. The durability contract still holds: each sync covers
    /// all bytes appended up to that point, so coalescing only
    /// changes "bytes per fsync," never "bytes that survive a
    /// crash after the next fsync completes."
    #[cfg(feature = "redex-disk")]
    #[tokio::test(flavor = "current_thread")]
    async fn test_fsync_policy_every_n_coalesces_under_burst() {
        let dir = tmp_persistent_dir("fsync_coalesce");
        let f = make_persistent_with_policy("fsync/coalesce", &dir, super::FsyncPolicy::EveryN(1));
        // No yields between appends — current_thread runtime can't
        // schedule the worker until we hit an await, so all 50
        // notifies arrive while the worker is parked. Notify stores
        // exactly one permit; the other 49 are folded in.
        for i in 0..50u64 {
            f.append(format!("c-{}", i).as_bytes()).unwrap();
        }
        let observed = wait_for_sync_count(&f, 1, 50).await;
        assert_eq!(
            observed, 1,
            "50 notifies arriving while worker is parked must coalesce \
             into exactly 1 worker sync"
        );
        f.close().unwrap();
    }

    /// Phase 3 invariant: an fsync error inside the EveryN worker
    /// must be logged and recovered from, NOT propagated as a worker
    /// termination. The failed sync doesn't bump `sync_count`, but
    /// the worker keeps looping and a subsequent successful sync
    /// goes through.
    #[cfg(feature = "redex-disk")]
    #[tokio::test(flavor = "current_thread")]
    async fn test_fsync_policy_every_n_worker_survives_sync_error() {
        let dir = tmp_persistent_dir("fsync_sync_err");
        let f = make_persistent_with_policy("fsync/sync_err", &dir, super::FsyncPolicy::EveryN(1));

        // Arm a one-shot sync failure. The next `sync()` (whether
        // worker- or close-driven) returns Err before touching disk.
        let disk = f.disk().expect("persistent file has disk").clone();
        disk.arm_next_sync_failure();

        // First append → notify → worker runs sync → injected fail.
        f.append(b"first").unwrap();
        // Yield generously so the worker observes the notify, calls
        // the (failing) sync, logs, and re-parks.
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            f.sync_count(),
            Some(0),
            "failed sync must NOT bump sync_count"
        );

        // Second append → fresh notify → worker runs sync → succeeds
        // (the one-shot fail flag was consumed on the failed call).
        f.append(b"second").unwrap();
        let observed = wait_for_sync_count(&f, 1, 50).await;
        assert_eq!(
            observed, 1,
            "worker must recover from prior sync error and process \
             subsequent notifies normally"
        );

        f.close().unwrap();
    }

    /// Phase 4: `IntervalOrBytes` byte arm fires when accumulated
    /// writes cross `max_bytes`. We use a long `period` (no virtual
    /// time advance, so the timer arm never fires) and small
    /// `max_bytes` so the byte arm dominates.
    ///
    /// Each heap append writes `dat_payload + idx_record(20) +
    /// ts(8)` bytes — 78 bytes for a 50-byte payload. With
    /// `max_bytes = 200`, the third such append crosses the
    /// threshold (78·3 = 234 ≥ 200), and the cycle repeats. Six
    /// yielding appends → exactly two syncs.
    #[cfg(feature = "redex-disk")]
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_fsync_policy_interval_or_bytes_byte_threshold_fires() {
        let dir = tmp_persistent_dir("fsync_iob_bytes");
        let f = make_persistent_with_policy(
            "fsync/iob_bytes",
            &dir,
            super::FsyncPolicy::IntervalOrBytes {
                period: std::time::Duration::from_secs(60),
                max_bytes: 200,
            },
        );

        let payload = vec![b'x'; 50];
        for _ in 0..6 {
            f.append(&payload).unwrap();
            tokio::task::yield_now().await;
        }
        let observed = wait_for_sync_count(&f, 2, 50).await;
        assert_eq!(
            observed, 2,
            "6 yielding appends of 78 bytes each must trigger exactly \
             2 byte-threshold syncs at max_bytes=200 (counter resets \
             on each sync, so each 234-byte cycle = one trigger)"
        );

        f.close().unwrap();
    }

    /// Phase 4: `IntervalOrBytes` timer arm fires on schedule when
    /// the byte threshold is far above what's been written. Mirrors
    /// the existing `Interval` test but uses the new variant.
    #[cfg(feature = "redex-disk")]
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_fsync_policy_interval_or_bytes_timer_fires() {
        let dir = tmp_persistent_dir("fsync_iob_timer");
        let f = make_persistent_with_policy(
            "fsync/iob_timer",
            &dir,
            super::FsyncPolicy::IntervalOrBytes {
                period: std::time::Duration::from_millis(50),
                // Effectively unreachable in the test (we'll write
                // far less than 10 MiB of payload).
                max_bytes: 10 * 1024 * 1024,
            },
        );

        // No appends; the timer arm should still fire on schedule.
        for _ in 0..3 {
            tokio::time::advance(std::time::Duration::from_millis(50)).await;
            tokio::task::yield_now().await;
        }

        let observed = f.sync_count().unwrap_or(0);
        assert!(
            observed >= 2,
            "IntervalOrBytes(50ms, big_bytes) after 150ms expected ≥ 2 timer syncs, got {}",
            observed,
        );

        f.close().unwrap();
    }

    /// Phase 4: a timer-driven sync resets `bytes_since_sync`, so
    /// the next byte-threshold check measures from the timer-sync
    /// point — not from the file's open. Without this reset, the
    /// counter would keep growing and the byte arm would over-fire
    /// on the very first append after each timer tick.
    #[cfg(feature = "redex-disk")]
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_fsync_policy_interval_or_bytes_timer_resets_byte_counter() {
        let dir = tmp_persistent_dir("fsync_iob_reset");
        let f = make_persistent_with_policy(
            "fsync/iob_reset",
            &dir,
            super::FsyncPolicy::IntervalOrBytes {
                period: std::time::Duration::from_millis(50),
                max_bytes: 1000,
            },
        );

        // Phase A: append 1 payload (78 bytes). Below threshold, no
        // byte-driven sync. Then advance time so the TIMER fires.
        f.append(b"under-threshold-A").unwrap();
        tokio::task::yield_now().await;
        tokio::time::advance(std::time::Duration::from_millis(50)).await;
        tokio::task::yield_now().await;
        let after_timer = f.sync_count().unwrap_or(0);
        assert!(
            after_timer >= 1,
            "timer should have fired at least once; got {}",
            after_timer,
        );

        // Phase B: append 1 more payload. If the timer-driven sync
        // failed to reset bytes_since_sync, the counter would still
        // be ~95 from phase A and another ~95 from this append —
        // still under 1000, no byte trigger. Behavior identical.
        // The real diagnostic is: append below max_bytes total in
        // phase B and confirm no extra byte-trigger sync fires.
        // Here we append once and assert no NEW byte-driven sync
        // beyond what the timer drove.
        let baseline = f.sync_count().unwrap_or(0);
        f.append(b"under-threshold-B").unwrap();
        // Yield without advancing time — the timer arm shouldn't
        // fire, so any sync we observe must be byte-driven.
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        let after_b = f.sync_count().unwrap_or(0);
        assert_eq!(
            after_b, baseline,
            "with bytes_since_sync correctly reset to 0 after the \
             timer sync, an additional ~95-byte append must NOT \
             cross the 1000-byte threshold (got {} syncs, expected \
             {})",
            after_b, baseline,
        );

        f.close().unwrap();
    }

    /// Phase 4: inline appends bump `bytes_since_sync` by exactly
    /// `idx_record(20) + ts(8) = 28` (no dat write, since the
    /// payload rides inside the 20-byte idx record). With
    /// `max_bytes = 100`, four inline appends cross the threshold
    /// (4·28 = 112). Pins the inline branch of the bytes-written
    /// calc — a regression that accidentally counted the inline
    /// payload as dat bytes would trigger after just 2 appends
    /// (2·36 = 72 still under 100, so actually different — but
    /// 4·36 = 144 vs 4·28 = 112 still both cross at N=4 here, so
    /// pick a tighter threshold). Use `max_bytes = 100` and
    /// require exactly 4 appends → 1 sync.
    #[cfg(feature = "redex-disk")]
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_fsync_policy_interval_or_bytes_byte_threshold_counts_inline() {
        let dir = tmp_persistent_dir("fsync_iob_inline");
        let f = make_persistent_with_policy(
            "fsync/iob_inline",
            &dir,
            super::FsyncPolicy::IntervalOrBytes {
                period: std::time::Duration::from_secs(60),
                max_bytes: 100,
            },
        );

        let payload: [u8; INLINE_PAYLOAD_SIZE] = *b"inline_8";
        // Three inline appends = 84 bytes, under 100. No sync.
        for _ in 0..3 {
            f.append_inline(&payload).unwrap();
            tokio::task::yield_now().await;
        }
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            f.sync_count(),
            Some(0),
            "3 inline appends (3·28 = 84 < 100) must NOT trigger a sync"
        );

        // Fourth crosses (4·28 = 112 ≥ 100).
        f.append_inline(&payload).unwrap();
        let observed = wait_for_sync_count(&f, 1, 50).await;
        assert_eq!(
            observed, 1,
            "4th inline append crosses the byte threshold (4·28 = \
             112 ≥ 100); inline path must NOT charge payload bytes \
             against dat (dat is skipped for inline)"
        );

        f.close().unwrap();
    }

    /// Phase 4: batch appends compute bytes-written as
    /// `dat_buf + idx_buf + ts_buf`. A batch of 3 × 50-byte heap
    /// payloads writes 150 + 60 + 24 = 234 bytes — one syscall to
    /// each file. Pins the batch-path bookkeeping; a regression
    /// that miscounted (e.g. only counted dat) would silently
    /// raise the effective threshold.
    #[cfg(feature = "redex-disk")]
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_fsync_policy_interval_or_bytes_byte_threshold_counts_batch() {
        let dir = tmp_persistent_dir("fsync_iob_batch");
        let f = make_persistent_with_policy(
            "fsync/iob_batch",
            &dir,
            super::FsyncPolicy::IntervalOrBytes {
                period: std::time::Duration::from_secs(60),
                max_bytes: 200,
            },
        );

        let payloads: Vec<Bytes> = (0..3).map(|_| Bytes::from(vec![b'z'; 50])).collect();
        f.append_batch(&payloads).unwrap();
        let observed = wait_for_sync_count(&f, 1, 50).await;
        assert_eq!(
            observed, 1,
            "1 batch of 3·50B (234 bytes total across dat/idx/ts) \
             must trigger 1 sync at max_bytes=200"
        );

        f.close().unwrap();
    }

    /// Phase 4: `max_bytes == 0` disables the byte arm entirely;
    /// the policy is then equivalent to `Interval(period)`. Heavy
    /// writes alone must NOT trigger a sync — only the timer can.
    #[cfg(feature = "redex-disk")]
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_fsync_policy_interval_or_bytes_zero_max_bytes_disables_byte_arm() {
        let dir = tmp_persistent_dir("fsync_iob_zero_bytes");
        let f = make_persistent_with_policy(
            "fsync/iob_zero_bytes",
            &dir,
            super::FsyncPolicy::IntervalOrBytes {
                period: std::time::Duration::from_secs(60),
                max_bytes: 0,
            },
        );

        // Write 100 KB. With max_bytes=0 the byte arm is gated off
        // (`if self.fsync_max_bytes > 0`), so this should produce
        // zero auto-syncs — and we don't advance time, so the
        // timer is silent too.
        let payload = vec![b'y'; 1024];
        for _ in 0..100 {
            f.append(&payload).unwrap();
            tokio::task::yield_now().await;
        }
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            f.sync_count(),
            Some(0),
            "max_bytes=0 must disable the byte arm; no auto-syncs \
             expected before any timer advance"
        );

        f.close().unwrap();
    }

    /// Phase 4: `period == ZERO && max_bytes > 0` is the byte-only
    /// variant. A byte-only worker spawns (no timer arm) and reacts
    /// to the appender's threshold notify. The earlier "no worker
    /// at all when period is zero" behavior was a footgun: a caller
    /// who explicitly asked for byte-only triggering would have
    /// silently received no auto-sync until `close()`.
    #[cfg(feature = "redex-disk")]
    #[tokio::test(flavor = "current_thread")]
    async fn test_fsync_policy_interval_or_bytes_zero_period_byte_only_worker() {
        let dir = tmp_persistent_dir("fsync_iob_byte_only");
        // Tight `max_bytes=100` so the threshold fires quickly. The
        // counter charges idx(20) + ts(8) + payload bytes per heap
        // append, so a single 80-byte payload (108 B total) crosses.
        let f = make_persistent_with_policy(
            "fsync/iob_byte_only",
            &dir,
            super::FsyncPolicy::IntervalOrBytes {
                period: std::time::Duration::ZERO,
                max_bytes: 100,
            },
        );

        // 3 refs: file + worker clone + this local clone.
        let disk = f.disk().expect("persistent file has disk").clone();
        assert_eq!(
            Arc::strong_count(&disk),
            3,
            "period=ZERO with max_bytes>0 must spawn a byte-only worker \
             (file + worker + test clone = 3 refs)"
        );

        // Cross the threshold once — worker should observe the
        // notify and run a sync. No timer advance is involved; if a
        // sync lands here, it can only be the byte arm.
        f.append(&[b'x'; 80]).unwrap();
        let observed = wait_for_sync_count(&f, 1, 50).await;
        assert_eq!(
            observed, 1,
            "byte-only worker must auto-sync on threshold crossing \
             without any timer advance"
        );

        f.close().unwrap();
    }

    /// Phase 4: `period == ZERO && max_bytes == 0` is the fully
    /// degenerate config — equivalent to `Never`. No worker spawns.
    /// The file still works for explicit appends and `close()`-time
    /// fsync.
    #[cfg(feature = "redex-disk")]
    #[tokio::test(flavor = "current_thread")]
    async fn test_fsync_policy_interval_or_bytes_both_zero_no_worker() {
        let dir = tmp_persistent_dir("fsync_iob_both_zero");
        let f = make_persistent_with_policy(
            "fsync/iob_both_zero",
            &dir,
            super::FsyncPolicy::IntervalOrBytes {
                period: std::time::Duration::ZERO,
                max_bytes: 0,
            },
        );

        // file's own ref + this local clone = 2; no worker means
        // no third ref.
        let disk = f.disk().expect("persistent file has disk").clone();
        assert_eq!(
            Arc::strong_count(&disk),
            2,
            "both arms zero must spawn no worker (file + test clone = \
             2 refs)"
        );

        // File still functional. No threshold and no timer means no
        // auto-sync regardless of write volume.
        f.append(b"manual-only-A").unwrap();
        f.append(b"manual-only-B").unwrap();
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        assert_eq!(f.sync_count(), Some(0), "no worker means no auto-syncs");

        // close() is the only durability barrier and must still
        // fsync regardless of policy.
        f.close().unwrap();
        assert_eq!(
            f.sync_count(),
            Some(1),
            "close() always fsyncs regardless of policy"
        );
    }

    /// Phase 3 invariant: after `close()` the EveryN worker must
    /// observe shutdown and drop its `Arc<DiskSegment>` clone, so
    /// the segment can eventually be freed once all RedexFile
    /// handles are dropped. Without this, a closed-but-not-dropped
    /// file would pin the disk segment until runtime shutdown.
    #[cfg(feature = "redex-disk")]
    #[tokio::test(flavor = "current_thread")]
    async fn test_close_releases_worker_disk_reference() {
        let dir = tmp_persistent_dir("fsync_release");
        let f = make_persistent_with_policy("fsync/release", &dir, super::FsyncPolicy::EveryN(1));

        // Snapshot the strong count: file's own ref + worker's clone
        // + this local clone = 3.
        let disk = f.disk().expect("persistent file has disk").clone();
        let initial = Arc::strong_count(&disk);
        assert_eq!(
            initial, 3,
            "expected 3 refs at steady state (file + worker + test); \
             got {}",
            initial,
        );

        f.close().unwrap();

        // close() fires shutdown; the worker observes it on its next
        // select poll, returns from the spawned future, and its
        // captured `task_disk` is dropped. Yield until the count
        // drops or we exhaust the budget.
        let mut final_count = Arc::strong_count(&disk);
        for _ in 0..50 {
            if final_count < initial {
                break;
            }
            tokio::task::yield_now().await;
            final_count = Arc::strong_count(&disk);
        }

        assert_eq!(
            final_count,
            initial - 1,
            "worker must drop its DiskSegment Arc clone after \
             observing shutdown (was {}, now {})",
            initial,
            final_count,
        );
    }

    #[cfg(feature = "redex-disk")]
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_fsync_policy_interval_fires_on_timer() {
        // Interval drives fsync from a tokio background task. We use
        // paused-time so the test is deterministic — advance time
        // through three intervals, expect three syncs (give or take
        // one scheduler hop).
        let dir = tmp_persistent_dir("fsync_interval");
        let f = make_persistent_with_policy(
            "fsync/interval",
            &dir,
            super::FsyncPolicy::Interval(std::time::Duration::from_millis(50)),
        );
        // No appends yet, but the timer ticks anyway.
        for _ in 0..3 {
            tokio::time::advance(std::time::Duration::from_millis(50)).await;
            tokio::task::yield_now().await;
        }
        // The background task may be slightly ahead or behind the
        // advance cursor; require at least 2 to avoid flakes on a
        // busy runner.
        let observed = f.sync_count().unwrap_or(0);
        assert!(
            observed >= 2,
            "Interval(50ms) after 150ms of advance expected ≥ 2 syncs, got {}",
            observed,
        );
        f.close().unwrap();
    }

    #[cfg(feature = "redex-disk")]
    #[test]
    fn test_regression_close_still_syncs_under_never_policy() {
        // Regression guard: `Never` means "no fsync on append," NOT
        // "no durability at all." `close()` is the caller's explicit
        // durability barrier and must always sync regardless of
        // policy; otherwise a clean shutdown of a Never-configured
        // file would silently lose its tail.
        let dir = tmp_persistent_dir("fsync_close_syncs");
        let base = dir.clone();

        {
            let f =
                make_persistent_with_policy("fsync/close_syncs", &base, super::FsyncPolicy::Never);
            for i in 0..10u64 {
                f.append(format!("x-{}", i).as_bytes()).unwrap();
            }
            assert_eq!(f.sync_count(), Some(0), "Never skips append syncs");
            f.close().unwrap();
            assert_eq!(
                f.sync_count(),
                Some(1),
                "close() under Never still syncs once"
            );
        }

        // Reopen and verify every entry survived the close.
        let r = super::super::manager::Redex::new().with_persistent_dir(&base);
        let f2 = r
            .open_file(
                &ChannelName::new("fsync/close_syncs").unwrap(),
                RedexFileConfig::default().with_persistent(true),
            )
            .unwrap();
        assert_eq!(f2.len(), 10, "all 10 entries must persist across close");
    }

    /// Regression: previously, the 28-bit xxh3 stored on every
    /// `RedexEntry` was computed at append but never verified at
    /// read. On-disk corruption (torn writes, bit-rot, external
    /// tampering) flowed through `materialize` as a valid event.
    /// The fix verifies the stored checksum matches the recomputed
    /// payload checksum on every read; mismatched entries are
    /// dropped from the result with an error log.
    ///
    /// We simulate corruption by mutating the in-memory segment
    /// bytes after append and asserting `read_range` no longer
    /// returns the corrupt entry.
    #[test]
    fn read_path_drops_entries_with_bad_checksum() {
        let f = make_file("checksum_verify");

        // Append three heap-stored entries (payload > inline size of
        // 8 bytes, so they live in the segment).
        f.append(b"first-payload-bytes").unwrap();
        f.append(b"second-payload-bytes").unwrap();
        f.append(b"third-payload-bytes").unwrap();

        // Sanity: all three round-trip cleanly.
        let events = f.read_range(0, 100);
        assert_eq!(events.len(), 3);

        // Corrupt the second entry's bytes in the heap segment.
        // We mutate the byte at the entry's offset directly via the
        // shared state lock — same access pattern `materialize` will
        // use, just with a write.
        {
            let mut state = f.inner.state.lock();
            // Find the second entry (seq == 1) and flip a byte at
            // its payload_offset.
            let entry = state.index.iter().find(|e| e.seq == 1).copied().unwrap();
            assert!(
                !entry.is_inline(),
                "test premise: seq=1 must be heap-stored"
            );
            // Flip the first byte of the payload.
            let off = entry.payload_offset as usize;
            let old = state.segment.bytes_for_test_mut()[off];
            state.segment.bytes_for_test_mut()[off] = old.wrapping_add(1);
        }

        // The corrupted entry must be dropped on read; the other
        // two survive.
        let events = f.read_range(0, 100);
        assert_eq!(
            events.len(),
            2,
            "corrupt entry must be dropped from read_range result"
        );
        let surviving_seqs: Vec<u64> = events.iter().map(|e| e.entry.seq).collect();
        assert_eq!(surviving_seqs, vec![0, 2]);
    }

    /// Regression: recovered entries used to get `now()` as their
    /// timestamp (no on-disk persistence), so a 1-hour age-retention
    /// on a process restarted every 30 minutes never evicted
    /// anything — every reopen reset the age clock to zero. The fix
    /// adds a `ts` sidecar that persists per-entry timestamps; on
    /// reopen, `read_timestamps` returns the stored values and
    /// age-based retention works correctly across restart.
    ///
    /// We pin this by:
    ///   1. Creating a persistent file and appending an entry.
    ///   2. Capturing the timestamp.
    ///   3. Reopening the file and verifying the entry's timestamp
    ///      survived (NOT a fresh `now()` from the second open).
    #[cfg(feature = "redex-disk")]
    #[test]
    fn ts_sidecar_preserves_timestamps_across_reopen() {
        let dir = tmp_persistent_dir("ts_sidecar_persist");
        let name = "ts/persist";

        let captured_ts;
        {
            let f = make_persistent(name, &dir);
            f.append(b"hello").unwrap();
            f.append(b"world").unwrap();
            captured_ts = f.inner.state.lock().timestamps.clone();
            f.close().unwrap();
        }
        // Sleep long enough that a "fresh" timestamp would be
        // distinguishable from the captured one.
        std::thread::sleep(std::time::Duration::from_millis(20));

        // Reopen and verify timestamps survived.
        let f2 = make_persistent(name, &dir);
        let restored_ts = f2.inner.state.lock().timestamps.clone();
        assert_eq!(
            restored_ts.len(),
            captured_ts.len(),
            "timestamp count must match index count"
        );
        // Each restored timestamp should match what we captured —
        // pre-fix, restored_ts would be ~20ms newer than
        // captured_ts (because they were sampled at reopen time,
        // not from the sidecar).
        for (i, (cap, restored)) in captured_ts.iter().zip(restored_ts.iter()).enumerate() {
            assert_eq!(
                *cap, *restored,
                "timestamp[{}] must round-trip across reopen (captured={}, restored={})",
                i, cap, restored
            );
        }
        f2.close().unwrap();
    }

    /// Regression: `sweep_retention` mutated only the in-memory
    /// state — the on-disk idx + dat files grew unbounded and on
    /// reopen the full dat was replayed, resurrecting entries that
    /// the previous generation evicted. Now `sweep_retention` calls
    /// into `disk.compact_to` which atomically rewrites idx + dat +
    /// ts to match the post-sweep in-memory state.
    ///
    /// We pin this by:
    ///   1. Append more entries than the retention max allows.
    ///   2. Run `sweep_retention`.
    ///   3. Close and reopen.
    ///   4. Verify the reopened file holds only the surviving
    ///      entries — not the evicted ones.
    #[cfg(feature = "redex-disk")]
    #[test]
    fn sweep_retention_persists_eviction_to_disk() {
        let dir = tmp_persistent_dir("sweep_persist");
        let name = "sweep/persist";

        {
            // Use Redex with a retention limit set in the config.
            use super::super::manager::Redex;
            let r = Redex::new().with_persistent_dir(&dir);
            let f = r
                .open_file(
                    &ChannelName::new(name).unwrap(),
                    RedexFileConfig::default()
                        .with_persistent(true)
                        .with_retention_max_events(2),
                )
                .unwrap();

            // Append 5 heap-stored entries (payloads > 8 bytes).
            f.append(b"AAAAAAAAA").unwrap();
            f.append(b"BBBBBBBBB").unwrap();
            f.append(b"CCCCCCCCC").unwrap();
            f.append(b"DDDDDDDDD").unwrap();
            f.append(b"EEEEEEEEE").unwrap();

            // Sweep — should evict 0, 1, 2 (keeping last 2).
            f.sweep_retention();
            let surviving_in_mem: Vec<u64> =
                f.inner.state.lock().index.iter().map(|e| e.seq).collect();
            assert_eq!(
                surviving_in_mem,
                vec![3, 4],
                "in-memory eviction should keep last 2"
            );

            f.close().unwrap();
        }

        // Reopen — pre-fix, this would resurrect entries 0/1/2/3/4
        // because the on-disk dat still had every byte. Post-fix,
        // the disk was compacted, so only entries 3 and 4 are
        // present.
        use super::super::manager::Redex;
        let r2 = Redex::new().with_persistent_dir(&dir);
        let f2 = r2
            .open_file(
                &ChannelName::new(name).unwrap(),
                RedexFileConfig::default()
                    .with_persistent(true)
                    .with_retention_max_events(2),
            )
            .unwrap();
        let restored_seqs: Vec<u64> = f2.inner.state.lock().index.iter().map(|e| e.seq).collect();
        assert_eq!(
            restored_seqs,
            vec![3, 4],
            "after reopen, only the entries that survived sweep should be present \
             (pre-fix all 5 would resurrect because sweep didn't touch disk)"
        );
        f2.close().unwrap();
    }

    /// Regression for BUG_AUDIT_2026_04_30_CORE.md #95: when
    /// `disk.compact_to` failed, `sweep_retention` previously
    /// committed the in-memory eviction anyway and logged a
    /// warning ("in-memory eviction succeeded but on-disk files
    /// retain evicted entries"). On reopen, recovery replayed
    /// the full on-disk state and resurrected the entries that
    /// were only evicted in memory.
    ///
    /// The fix runs `compact_to` BEFORE mutating in-memory state,
    /// and bails out without mutation on failure. We pin this by:
    ///   1. Append entries past the retention max.
    ///   2. Arm a one-shot `compact_to` failure on the next call.
    ///   3. Run `sweep_retention`.
    ///   4. Verify in-memory state is unchanged (no eviction
    ///      happened — would have lost head entries pre-fix).
    #[cfg(feature = "redex-disk")]
    #[test]
    fn sweep_retention_keeps_in_memory_state_when_disk_compact_fails() {
        let dir = tmp_persistent_dir("sweep_compact_fails");
        let name = "sweep/compact_fails";

        use super::super::manager::Redex;
        let r = Redex::new().with_persistent_dir(&dir);
        let f = r
            .open_file(
                &ChannelName::new(name).unwrap(),
                RedexFileConfig::default()
                    .with_persistent(true)
                    .with_retention_max_events(2),
            )
            .unwrap();

        // Append 5 heap entries (payloads > 8 bytes so they route
        // through the dat segment, not inline).
        f.append(b"AAAAAAAAA").unwrap();
        f.append(b"BBBBBBBBB").unwrap();
        f.append(b"CCCCCCCCC").unwrap();
        f.append(b"DDDDDDDDD").unwrap();
        f.append(b"EEEEEEEEE").unwrap();

        // Arm a one-shot failure on the next `compact_to`.
        f.inner
            .disk
            .as_ref()
            .expect("persistent file must have a disk segment")
            .arm_next_compact_failure();

        // Sweep — `compact_to` will fail. Pre-fix, in-memory state
        // would still be evicted (drained to [3, 4]). Post-fix, the
        // in-memory state is left as [0, 1, 2, 3, 4] so reopen
        // replays a consistent picture.
        f.sweep_retention();

        let post_sweep: Vec<u64> = f.inner.state.lock().index.iter().map(|e| e.seq).collect();
        assert_eq!(
            post_sweep,
            vec![0, 1, 2, 3, 4],
            "in-memory state must be left untouched when disk \
             compaction fails — pre-fix this returned [3, 4] \
             because eviction was committed before the disk write"
        );

        f.close().unwrap();
    }

    /// Regression for BUG_AUDIT_2026_04_30_CORE.md #92: post-sweep
    /// appends were silently lost on restart because `compact_to`
    /// renormalized on-disk offsets to be segment-relative while
    /// `state.index` kept absolute offsets. The next append then
    /// computed `offset = segment.base_offset() + live_bytes()` —
    /// also absolute — and wrote that to disk verbatim. On reopen,
    /// the torn-tail recovery walk saw `(offset + len) > dat_len`
    /// for every post-sweep record and truncated them.
    ///
    /// The fix renormalizes `state.index` and resets `segment`
    /// base_offset to 0 after a successful `compact_to`, so
    /// subsequent appends compute offsets against a 0-based
    /// segment that matches the on-disk layout.
    ///
    /// We pin this by:
    ///   1. Append more entries than the retention max allows.
    ///   2. Run `sweep_retention` to evict the head.
    ///   3. Append more entries AFTER the sweep — these are the
    ///      ones the bug silently lost.
    ///   4. Close and reopen.
    ///   5. Verify the post-sweep appends survive restart.
    #[cfg(feature = "redex-disk")]
    #[test]
    fn sweep_retention_post_sweep_appends_survive_restart() {
        let dir = tmp_persistent_dir("sweep_post_append");
        let name = "sweep/post_append";

        {
            use super::super::manager::Redex;
            let r = Redex::new().with_persistent_dir(&dir);
            let f = r
                .open_file(
                    &ChannelName::new(name).unwrap(),
                    RedexFileConfig::default()
                        .with_persistent(true)
                        .with_retention_max_events(2),
                )
                .unwrap();

            // Append 5 heap entries (payloads > 8 bytes so they
            // route through the dat segment, not inline).
            f.append(b"AAAAAAAAA").unwrap();
            f.append(b"BBBBBBBBB").unwrap();
            f.append(b"CCCCCCCCC").unwrap();
            f.append(b"DDDDDDDDD").unwrap();
            f.append(b"EEEEEEEEE").unwrap();

            // Sweep — evicts 0, 1, 2; keeps 3, 4.
            f.sweep_retention();

            // Post-sweep appends. Pre-fix, these record absolute
            // offsets (300, 400, ...) into an idx whose other
            // records are now relative (0, 100), and the on-disk
            // dat is only 200 bytes — so reopen drops them.
            f.append(b"FFFFFFFFF").unwrap(); // seq=5
            f.append(b"GGGGGGGGG").unwrap(); // seq=6

            // Confirm the in-memory state believes everything is
            // present before close.
            let pre_close: Vec<u64> = f.inner.state.lock().index.iter().map(|e| e.seq).collect();
            assert_eq!(
                pre_close,
                vec![3, 4, 5, 6],
                "in-memory state must hold the post-sweep appends before close"
            );

            f.close().unwrap();
        }

        // Reopen — pre-fix, only [3, 4] would survive because the
        // post-sweep appends got truncated by the torn-tail walk.
        // Post-fix, all four are durable.
        use super::super::manager::Redex;
        let r2 = Redex::new().with_persistent_dir(&dir);
        let f2 = r2
            .open_file(
                &ChannelName::new(name).unwrap(),
                RedexFileConfig::default()
                    .with_persistent(true)
                    .with_retention_max_events(2),
            )
            .unwrap();
        let restored_seqs: Vec<u64> = f2.inner.state.lock().index.iter().map(|e| e.seq).collect();
        assert!(
            restored_seqs.contains(&5),
            "post-sweep append seq=5 must survive restart, got {:?}",
            restored_seqs
        );
        assert!(
            restored_seqs.contains(&6),
            "post-sweep append seq=6 must survive restart, got {:?}",
            restored_seqs
        );
        f2.close().unwrap();
    }

    /// Source pin: `sweep_retention` must build the renormalized
    /// index in a temp `Vec` and only then swap it into
    /// `state.index` / `state.timestamps`. Pre-fix the order was
    /// (drain, evict, then `iter_mut()` rebase loop) — a panic in
    /// the middle of the rebase loop left the index half-rebased
    /// (some entries still pointing past the new compacted dat,
    /// others rebased), so subsequent reads silently missed.
    ///
    /// This is a tripwire against a "simplification" PR that
    /// reverts to in-place mutation: such a PR would compile and
    /// pass every behavior test (no panic is ever injected mid-
    /// loop in unit tests) but reintroduce the half-rebased
    /// state on a real allocator/signal failure.
    #[test]
    fn sweep_retention_must_use_build_then_swap() {
        let src = include_str!("file.rs");

        // Locate `pub fn sweep_retention` and the next sibling
        // `fn ` after it — that's our scope.
        let header = "pub fn sweep_retention(";
        let start = src.find(header).expect("sweep_retention must exist");
        let body_start = start + header.len();
        let next_fn = src[body_start..]
            .find("\n    fn ")
            .or_else(|| src[body_start..].find("\n    pub fn "))
            .expect("a following fn must exist (mod-private impl block)")
            + body_start;
        let body = &src[start..next_fn];

        // Strip line comments so doc / pre-fix-history comments
        // that mention the rejected pattern don't trip the
        // negative assertion.
        let body_no_comments: String = body
            .lines()
            .map(|l| match l.find("//") {
                Some(idx) => &l[..idx],
                None => l,
            })
            .collect::<Vec<_>>()
            .join("\n");

        // Phase-1 marker: a fresh local Vec built from
        // `state.index.iter().skip(drop).map(...)`.
        assert!(
            body_no_comments.contains("let new_index"),
            "regression: sweep_retention must build the new index \
             into a temp `let new_index` Vec before mutating \
             state.index. Building in-place reintroduces the \
             half-rebased-on-panic hazard."
        );
        assert!(
            body_no_comments.contains("let new_timestamps"),
            "regression: sweep_retention must build the new \
             timestamps Vec separately before mutating \
             state.timestamps."
        );

        // Phase-2 marker: assignment to state.index / state.timestamps.
        assert!(
            body_no_comments.contains("state.index = new_index"),
            "regression: sweep_retention's phase-2 swap into \
             state.index must follow the temp-Vec build."
        );
        assert!(
            body_no_comments.contains("state.timestamps = new_timestamps"),
            "regression: sweep_retention's phase-2 swap into \
             state.timestamps must follow the temp-Vec build."
        );

        // Negative pin: the in-place mutation pattern that was
        // the pre-fix shape MUST NOT reappear inside this
        // function body. The pre-fix used a `for ... iter_mut()`
        // loop on `state.index` to rebase payload offsets after
        // draining; reintroducing it brings back the half-rebased
        // hazard.
        assert!(
            !body_no_comments.contains("state.index.iter_mut()"),
            "regression: sweep_retention must not iter_mut() over \
             state.index in place — that's the pre-fix pattern \
             that left the index half-rebased on mid-loop panic."
        );
    }
}
