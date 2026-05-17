//! `TasksAdapter` — a typed wrapper around `CortexAdapter<TasksState>`
//! with domain-level ingest helpers.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use super::super::super::channel::ChannelName;
use super::super::super::redex::{Redex, RedexError, RedexFileConfig, WriteToken};
use super::super::adapter::{CortexAdapter, WaitForTokenError};
use super::super::config::CortexAdapterConfig;
use super::super::envelope::EventEnvelope;
use super::super::error::CortexAdapterError;
use super::super::meta::{compute_checksum_with_meta, EventMeta};
use super::super::watermark::WatermarkingFold;
use super::dispatch::{
    DISPATCH_TASK_COMPLETED, DISPATCH_TASK_CREATED, DISPATCH_TASK_DELETED, DISPATCH_TASK_RENAMED,
    TASKS_CHANNEL,
};
use super::fold::TasksFold;
use super::state::TasksState;
use super::types::{
    TaskCompletedPayload, TaskCreatedPayload, TaskDeletedPayload, TaskId, TaskRenamedPayload,
};
use super::watch::TasksWatcher;

/// Return shape of [`TasksAdapter::snapshot_and_watch`]: the
/// initial filter result plus a boxed stream that emits every
/// subsequent change (dedup'd, with the initial skipped so the
/// caller doesn't double-render).
pub type TasksSnapshotAndWatch = (
    Vec<super::types::Task>,
    std::pin::Pin<Box<dyn futures::Stream<Item = Vec<super::types::Task>> + Send + 'static>>,
);

use futures::StreamExt;

/// Wire format for [`TasksAdapter::snapshot`]: wraps the `TasksState`
/// postcard blob produced by the underlying [`CortexAdapter`] alongside
/// the typed adapter's own `app_seq` counter so restore preserves
/// per-origin monotonicity of `EventMeta::seq_or_ts`.
#[derive(Serialize, Deserialize)]
struct TasksSnapshotPayload {
    /// Next-to-assign `app_seq` value at snapshot time — the adapter
    /// restores its counter to this so post-restore `EventMeta`
    /// records continue with monotonic per-origin sequencing.
    app_seq: u64,
    /// The `CortexAdapter::snapshot` blob (postcard of `TasksState`).
    inner: Vec<u8>,
}

/// Typed wrapper around `CortexAdapter<TasksState>` that exposes
/// domain-level operations (`create`, `rename`, `complete`, `delete`)
/// and hides the `EventMeta` + postcard plumbing.
pub struct TasksAdapter {
    inner: CortexAdapter<TasksState>,
    /// Producer identity stamped on every `EventMeta`.
    origin_hash: u64,
    /// Monotonic per-origin counter for `EventMeta::seq_or_ts`.
    /// Shared with the inner `WatermarkingFold` wrapper around
    /// [`TasksFold`]: the fold task advances this counter via
    /// `fetch_max(seq_or_ts + 1)` for every replayed event whose
    /// `origin_hash` matches ours, so reopening against a Redex
    /// with pre-existing same-origin events produces a counter
    /// that's already past every assigned `seq_or_ts` by the time
    /// the constructor returns. `ingest_typed` then
    /// load-and-CAS-commits against the same atomic.
    app_seq: Arc<AtomicU64>,
}

impl TasksAdapter {
    /// Open the tasks adapter against a `Redex` manager.
    ///
    /// Uses [`TASKS_CHANNEL`] (`"cortex/tasks"`). Replays the full
    /// history into state on open; subsequent events are appended to
    /// the same channel.
    ///
    /// `async` because the constructor awaits the fold task's
    /// catch-up before returning: the inner `WatermarkingFold`
    /// observes every replayed event's `EventMeta` and advances
    /// `app_seq` past any pre-existing same-origin `seq_or_ts`,
    /// so the first `ingest_typed` after `open` cannot collide
    /// with an already-stored event.
    pub async fn open(redex: &Redex, origin_hash: u64) -> Result<Self, CortexAdapterError> {
        Self::open_with_config(redex, origin_hash, RedexFileConfig::default()).await
    }

    /// Like [`Self::open`] but with a caller-supplied `RedexFileConfig`
    /// (useful for `persistent: true` or custom retention).
    pub async fn open_with_config(
        redex: &Redex,
        origin_hash: u64,
        redex_config: RedexFileConfig,
    ) -> Result<Self, CortexAdapterError> {
        let name = ChannelName::new(TASKS_CHANNEL).map_err(|e| {
            CortexAdapterError::Redex(super::super::super::redex::RedexError::Channel(
                e.to_string(),
            ))
        })?;
        let app_seq = Arc::new(AtomicU64::new(0));
        let fold = WatermarkingFold::new(TasksFold, app_seq.clone(), origin_hash);
        let inner = CortexAdapter::open(
            redex,
            &name,
            redex_config.clone(),
            CortexAdapterConfig::default(),
            fold,
            TasksState::new(),
        )?;

        // Wait for the fold task to catch up so the wrapper has
        // observed every pre-existing event before any caller-driven
        // ingest can race against it. `redex.open_file` is idempotent
        // (returns the same handle the inner adapter already holds),
        // so re-opening here is cheap.
        let file = redex.open_file(&name, redex_config)?;
        let next_seq = file.next_seq();
        if next_seq > 0 {
            inner
                .wait_for_seq(next_seq - 1)
                .await
                .map_err(|folded_through| CortexAdapterError::FoldStoppedBeforeSeq {
                    wanted: next_seq - 1,
                    folded_through,
                })?;
        }

        Ok(Self {
            inner,
            origin_hash,
            app_seq,
        })
    }

    /// Create a new task. Returns the RedEX seq of the append.
    pub fn create(
        &self,
        id: TaskId,
        title: impl Into<String>,
        now_ns: u64,
    ) -> Result<u64, CortexAdapterError> {
        let payload = TaskCreatedPayload {
            id,
            title: title.into(),
            now_ns,
        };
        self.ingest_typed(DISPATCH_TASK_CREATED, &payload)
    }

    /// Rename an existing task. No-op at fold time if `id` is unknown.
    pub fn rename(
        &self,
        id: TaskId,
        new_title: impl Into<String>,
        now_ns: u64,
    ) -> Result<u64, CortexAdapterError> {
        let payload = TaskRenamedPayload {
            id,
            new_title: new_title.into(),
            now_ns,
        };
        self.ingest_typed(DISPATCH_TASK_RENAMED, &payload)
    }

    /// Mark a task completed. No-op at fold time if `id` is unknown.
    pub fn complete(&self, id: TaskId, now_ns: u64) -> Result<u64, CortexAdapterError> {
        let payload = TaskCompletedPayload { id, now_ns };
        self.ingest_typed(DISPATCH_TASK_COMPLETED, &payload)
    }

    /// Delete a task. No-op at fold time if `id` is unknown.
    pub fn delete(&self, id: TaskId) -> Result<u64, CortexAdapterError> {
        let payload = TaskDeletedPayload { id };
        self.ingest_typed(DISPATCH_TASK_DELETED, &payload)
    }

    /// Read-only access to the materialized state.
    pub fn state(&self) -> Arc<RwLock<TasksState>> {
        self.inner.state()
    }

    /// Total task count in the current state. Cheap; acquires the
    /// state read lock briefly. Matches the Node/Python SDK surface.
    pub fn count(&self) -> usize {
        self.inner.state().read().len()
    }

    /// Block until every event up through `seq` has been folded.
    /// Returns `Err(folded)` if the fold task stopped before
    /// reaching `seq`; see [`CortexAdapter::wait_for_seq`] for the
    /// stop-vs-success rationale.
    pub async fn wait_for_seq(&self, seq: u64) -> Result<(), Option<u64>> {
        self.inner.wait_for_seq(seq).await
    }

    /// Block until the fold task has processed every event up
    /// through `token.seq`, or `deadline` elapses. Read-your-writes
    /// wait: a writer who got `token` from this origin's ingest
    /// path can call this to make sure the local fold has caught
    /// up before reading state.
    ///
    /// Rejects tokens issued for a different origin with
    /// [`WaitForTokenError::WrongOrigin`] — protects against the
    /// `causal_tokens.get(other_origin).wait(my_token)` aliasing
    /// failure where a wait on this adapter would never resolve
    /// because the targeted seq belongs to someone else's chain.
    pub async fn wait_for_token(
        &self,
        token: WriteToken,
        deadline: Duration,
    ) -> Result<(), WaitForTokenError> {
        if token.origin_hash != self.origin_hash {
            self.inner.note_wrong_origin();
            return Err(WaitForTokenError::WrongOrigin {
                token_origin: token.origin_hash,
                adapter_origin: self.origin_hash,
            });
        }
        self.inner.wait_for_token(token, deadline).await
    }

    /// Non-blocking RYW poll. Synchronously checks origin binding +
    /// the applied watermark and returns without scheduling any
    /// wait. Use for "is my write visible yet?" queries where the
    /// caller doesn't want to block:
    ///
    /// - `Ok(())` — the write is observable; subsequent reads see it.
    /// - `Err(WaitForTokenError::WrongOrigin {..})` — the token's
    ///   `origin_hash` doesn't match this adapter's bound origin.
    /// - `Err(WaitForTokenError::FoldStopped {..})` — the fold task
    ///   has stopped before reaching the target seq; the write will
    ///   never become observable.
    /// - `Err(WaitForTokenError::Timeout)` — not yet (try again later).
    ///
    /// Mirrors the FFI's `timeout_ms == 0` shape so every binding
    /// can expose a "poll, don't wait" entry point with consistent
    /// semantics. No semaphore permit is taken; `QueueFull` is not
    /// reachable on this path.
    pub fn poll_for_token(&self, token: WriteToken) -> Result<(), WaitForTokenError> {
        if token.origin_hash != self.origin_hash {
            self.inner.note_wrong_origin();
            return Err(WaitForTokenError::WrongOrigin {
                token_origin: token.origin_hash,
                adapter_origin: self.origin_hash,
            });
        }
        match self.inner.applied_through_seq() {
            Some(applied) if applied >= token.seq => Ok(()),
            _ if !self.inner.is_running() => Err(WaitForTokenError::FoldStopped {
                applied_through_seq: self.inner.applied_through_seq(),
            }),
            _ => Err(WaitForTokenError::Timeout),
        }
    }

    /// Close the adapter. See [`CortexAdapter::close`].
    pub fn close(&self) -> Result<(), CortexAdapterError> {
        self.inner.close()
    }

    /// True if the fold task is currently running.
    pub fn is_running(&self) -> bool {
        self.inner.is_running()
    }

    /// Access the wrapped [`CortexAdapter`] for cases that need the
    /// lower-level surface.
    pub fn as_cortex(&self) -> &CortexAdapter<TasksState> {
        &self.inner
    }

    /// Origin hash this adapter is bound to. Stamped on every
    /// outgoing `EventMeta`; tokens with a different origin reject
    /// at `wait_for_token`.
    pub fn origin_hash(&self) -> u64 {
        self.origin_hash
    }

    /// Start building a reactive watcher. See
    /// [`TasksWatcher::stream`] for emission semantics (initial +
    /// deduplicated on filter-result change).
    pub fn watch(&self) -> TasksWatcher {
        TasksWatcher::new(self.inner.state(), self.inner.changes().boxed())
    }

    /// One-shot combo: a snapshot of the current filter result PLUS
    /// a stream that emits every **subsequent** change to that
    /// filter. The stream skips the initial emission so the caller
    /// doesn't see the snapshot twice — the snapshot is the initial
    /// state; the stream carries deltas from there forward.
    ///
    /// Useful for UI-style consumers: "paint what's there now, then
    /// react to changes" without a manual dedup against the first
    /// emission.
    pub fn snapshot_and_watch(&self, watcher: TasksWatcher) -> TasksSnapshotAndWatch {
        use futures::StreamExt;
        // Compute the snapshot from the adapter's current state,
        // reusing the watcher's configured filter. Holding the read
        // lock only for the execute call keeps it brief.
        let initial = {
            let state = self.inner.state();
            let guard = state.read();
            watcher.spec_for_snapshot().execute(&guard)
        };
        // Skip ONLY the first emission, and only if it equals the
        // snapshot. Subsequent emissions always forward. A sticky
        // `skip_while(|c| c == &initial)` would handle the
        // snapshot-vs-watcher race (state changes between
        // snapshot read and `watcher.stream()` start, so the
        // watcher's first emission ≠ snapshot — we want to forward
        // it) but introduces a starvation hazard: under an
        // (A → B → A) state oscillation that the single-slot
        // `tokio::sync::watch` collapses into final A, the
        // surviving A equals `initial` so it would be skipped —
        // the consumer would be silent until state diverged from
        // A. The first-only filter handles both cases:
        //   - leading match (no state change since snapshot): skip
        //     the first emission → consumer sees no duplicate
        //   - leading divergence (state changed during the race):
        //     first emission ≠ snapshot → forwarded
        //   - oscillation back to initial (A → B → A): the watch's
        //     surviving A is forwarded as the first item if state
        //     hadn't changed since snapshot — caller can dedup
        //     against their snapshot if they care, or treat it as
        //     "fold tick observed" signal.
        // Implemented via `enumerate().filter(...)` rather than a
        // separate state-carrying skip primitive, since
        // `futures::StreamExt::filter` doesn't accept a `FnMut`.
        let initial_for_stream = initial.clone();
        let stream = watcher
            .stream()
            .enumerate()
            .filter(move |(i, current)| {
                let drop_first = *i == 0 && current == &initial_for_stream;
                futures::future::ready(!drop_first)
            })
            .map(|(_, current)| current)
            .boxed();
        (initial, stream)
    }

    /// Capture a snapshot suitable for restore. Returns
    /// `(state_bytes, last_seq)` — persist both together.
    pub fn snapshot(&self) -> Result<(Vec<u8>, Option<u64>), CortexAdapterError> {
        let (inner, last_seq) = self.inner.snapshot()?;
        let payload = TasksSnapshotPayload {
            app_seq: self.app_seq.load(Ordering::Acquire),
            inner,
        };
        let bytes = postcard::to_allocvec(&payload).map_err(|e| {
            CortexAdapterError::Redex(RedexError::Encode(format!("tasks snapshot wrap: {}", e)))
        })?;
        Ok((bytes, last_seq))
    }

    /// Open the tasks adapter from a snapshot, skipping replay of
    /// events up through `last_seq`.
    ///
    /// See [`Self::open`] for why this is `async`.
    pub async fn open_from_snapshot(
        redex: &Redex,
        origin_hash: u64,
        state_bytes: &[u8],
        last_seq: Option<u64>,
    ) -> Result<Self, CortexAdapterError> {
        Self::open_from_snapshot_with_config(
            redex,
            origin_hash,
            RedexFileConfig::default(),
            state_bytes,
            last_seq,
        )
        .await
    }

    /// Like [`Self::open_from_snapshot`] but with a caller-supplied
    /// `RedexFileConfig` (e.g. for `persistent: true`).
    pub async fn open_from_snapshot_with_config(
        redex: &Redex,
        origin_hash: u64,
        redex_config: RedexFileConfig,
        state_bytes: &[u8],
        last_seq: Option<u64>,
    ) -> Result<Self, CortexAdapterError> {
        let payload: TasksSnapshotPayload = postcard::from_bytes(state_bytes).map_err(|e| {
            CortexAdapterError::Redex(RedexError::Encode(format!("tasks snapshot unwrap: {}", e)))
        })?;
        let name = ChannelName::new(TASKS_CHANNEL)
            .map_err(|e| CortexAdapterError::Redex(RedexError::Channel(e.to_string())))?;

        // Pre-load the snapshot's persisted counter into the
        // shared atomic. The wrapper fold then advances the
        // counter past any events written between snapshot capture
        // and close as part of its replay pass. A separate
        // synchronous post-`last_seq` tail walk would double IO/CPU
        // on large logs.
        let app_seq = Arc::new(AtomicU64::new(payload.app_seq));
        let fold = WatermarkingFold::new(TasksFold, app_seq.clone(), origin_hash);
        let inner = CortexAdapter::open_from_snapshot(
            redex,
            &name,
            redex_config.clone(),
            CortexAdapterConfig::default(),
            fold,
            &payload.inner,
            last_seq,
        )?;

        // Wait for the wrapper fold to observe every replay-tail
        // event before returning. `next_seq` may be `last_seq + 1`
        // (no post-snapshot writes) in which case the wait is a
        // no-op fast path inside `wait_for_seq`.
        let file = redex.open_file(&name, redex_config)?;
        let next_seq = file.next_seq();
        if next_seq > 0 {
            inner
                .wait_for_seq(next_seq - 1)
                .await
                .map_err(|folded_through| CortexAdapterError::FoldStoppedBeforeSeq {
                    wanted: next_seq - 1,
                    folded_through,
                })?;
        }

        Ok(Self {
            inner,
            origin_hash,
            app_seq,
        })
    }

    /// Build the `EventEnvelope` + ingest. Keeps postcard serialization
    /// and `EventMeta` assembly in one place.
    ///
    /// `app_seq` is reserved with a single atomic `fetch_add`
    /// before constructing the `EventEnvelope`. `inner.ingest`
    /// then commits the envelope to the Redex log. If the ingest
    /// fails, the reserved seq is "lost" — i.e. the per-origin
    /// `seq_or_ts` space has a one-unit gap — which is harmless:
    ///
    ///   * `WatermarkingFold` advances via `fetch_max` against
    ///     events that actually landed in the log. The gap from
    ///     a failed ingest is invisible to the watermark.
    ///   * The next successful ingest gets a strictly-larger seq,
    ///     so no duplicate is ever stamped.
    ///   * `seq_or_ts` is not required to be contiguous — it's a
    ///     monotonic per-origin tag, nothing more.
    ///
    /// **Why not load + ingest + CAS-commit?** That shape races
    /// against the `WatermarkingFold` task: when the fold
    /// processes the just-ingested event before the foreground
    /// thread's CAS runs, the watermark advances to the expected
    /// post-CAS value, the CAS observes the now-stale `app_seq`
    /// mismatch, and surfaces a phantom "concurrent ingest_typed
    /// produced duplicate app_seq" error even though no actual
    /// duplicate happened. Single-adapter timing usually has the
    /// foreground CAS running first; dual-adapter timing
    /// (memories + tasks under one NetDb) gives the fold task
    /// enough head-room to land first and the bug surfaces
    /// deterministically. The race is in the protocol:
    /// `fetch_add` sidesteps it.
    ///
    /// **Why no `fetch_sub` rollback on ingest failure?** This is
    /// the chosen design — see the harmlessness rationale above:
    /// the gap is invisible to `WatermarkingFold` (it advances via
    /// `fetch_max` against landed events), the next successful
    /// ingest gets a strictly-larger seq, and `seq_or_ts` is a
    /// monotonic tag, not a contiguous counter. A `fetch_sub` on
    /// `Err` would re-introduce the CAS-style race described
    /// above: two foreground threads each `fetch_add` then `ingest`;
    /// if A's ingest fails and A `fetch_sub`s after B already
    /// `fetch_add`-ed, B's reserved seq jumps backwards and the
    /// next thread can collide. The pre-fix audit doc warned that
    /// a higher counter could survive a snapshot/restore; in
    /// practice the second-adapter-on-same-origin recovery via
    /// on-disk scan is gated by the substrate's already-required
    /// uniqueness contract (one in-memory adapter per
    /// `(channel, origin_hash)`), so the cross-adapter collision
    /// described there is unreachable today.
    fn ingest_typed<T: serde::Serialize>(
        &self,
        dispatch: u8,
        payload: &T,
    ) -> Result<u64, CortexAdapterError> {
        let tail = postcard::to_allocvec(payload).map_err(|e| {
            CortexAdapterError::Redex(super::super::super::redex::RedexError::Encode(
                e.to_string(),
            ))
        })?;
        let app_seq = self.app_seq.fetch_add(1, Ordering::AcqRel);
        // Build the meta with checksum=0 first; `compute_checksum_with_meta`
        // hashes the header (with the checksum slot zeroed) plus
        // the tail, closing the audit-#8 dispatch-flip undercoverage
        // hole that the legacy tail-only `compute_checksum` left
        // open.
        let mut meta = EventMeta::new(dispatch, 0, self.origin_hash, app_seq, 0);
        meta.checksum = compute_checksum_with_meta(&meta, &tail);
        let payload_bytes = Bytes::from(tail);
        let env = EventEnvelope::new(meta, payload_bytes);
        self.inner.ingest(env)
    }
}

impl std::fmt::Debug for TasksAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TasksAdapter")
            .field("origin_hash", &self.origin_hash)
            .field("app_seq", &self.app_seq.load(Ordering::Acquire))
            .field("inner", &self.inner)
            .finish()
    }
}
