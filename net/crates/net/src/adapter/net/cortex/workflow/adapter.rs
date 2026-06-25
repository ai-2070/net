//! `WorkflowAdapter` — a typed wrapper around
//! `CortexAdapter<WorkflowState>` with domain-level transition helpers
//! (`submit` / `start` / `advance` / `wait` / `block` / `complete` /
//! `fail` / `retry` / `delete`), hiding the `EventMeta` + postcard
//! plumbing.
//!
//! The adapter is the *single writer* for a task chain — the task-lease
//! holder ([`super::lease`]) owns it. Reopening against the same
//! [`Redex`] replays the full history into state (the plan's exact
//! replay / failover-resume property).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use super::super::super::channel::ChannelName;
use super::super::super::redex::{Redex, RedexError, RedexFileConfig};
use super::super::adapter::CortexAdapter;
use super::super::config::CortexAdapterConfig;
use super::super::error::CortexAdapterError;
use super::super::meta::{compute_checksum_with_meta, EventMeta, EVENT_META_SIZE};
use super::super::watermark::WatermarkingFold;
use super::dispatch::{
    DISPATCH_TASK_ADVANCED, DISPATCH_TASK_CANCEL_REQUESTED, DISPATCH_TASK_DELETED,
    DISPATCH_TASK_LINKED, DISPATCH_TASK_RETRIED, DISPATCH_TASK_SUBMITTED,
    DISPATCH_TASK_TRANSITIONED, WORKFLOW_CHANNEL,
};
use super::fold::WorkflowFold;
use super::state::{StatusCounts, WorkflowState};
use super::types::{
    AdvancedPayload, CancelRequestedPayload, DeletedPayload, LinkedPayload, RetriedPayload,
    SubmittedPayload, TaskId, TaskState, TaskStatus, TransitionedPayload,
};

/// Wire format for [`WorkflowAdapter::snapshot`]: wraps the
/// `WorkflowState` blob alongside the adapter's `app_seq` so a restore
/// keeps per-origin `EventMeta::seq_or_ts` monotonic (mirrors
/// `TasksAdapter`).
#[derive(Serialize, Deserialize)]
struct WorkflowSnapshotPayload {
    app_seq: u64,
    inner: Vec<u8>,
}

/// Typed wrapper around `CortexAdapter<WorkflowState>` exposing the
/// task-lifecycle transitions.
pub struct WorkflowAdapter {
    inner: CortexAdapter<WorkflowState>,
    /// Producer identity stamped on every `EventMeta`.
    origin_hash: u64,
    /// Monotonic per-origin counter for `EventMeta::seq_or_ts`, shared
    /// with the inner `WatermarkingFold` so reopening against existing
    /// same-origin events resumes past their `seq_or_ts`.
    app_seq: Arc<AtomicU64>,
}

impl WorkflowAdapter {
    /// Open the workflow adapter against a `Redex` manager on
    /// [`WORKFLOW_CHANNEL`]. Replays the full history into state on
    /// open; subsequent transitions append to the same channel.
    pub async fn open(redex: &Redex, origin_hash: u64) -> Result<Self, CortexAdapterError> {
        Self::open_with_config(redex, origin_hash, RedexFileConfig::default()).await
    }

    /// Like [`Self::open`] but with a caller-supplied `RedexFileConfig`
    /// (e.g. `persistent: true`).
    pub async fn open_with_config(
        redex: &Redex,
        origin_hash: u64,
        redex_config: RedexFileConfig,
    ) -> Result<Self, CortexAdapterError> {
        let name = ChannelName::new(WORKFLOW_CHANNEL)
            .map_err(|e| CortexAdapterError::Redex(RedexError::Channel(e.to_string())))?;
        let app_seq = Arc::new(AtomicU64::new(0));
        let fold = WatermarkingFold::new(WorkflowFold, app_seq.clone(), origin_hash);
        let inner = CortexAdapter::open(
            redex,
            &name,
            redex_config.clone(),
            CortexAdapterConfig::default(),
            fold,
            WorkflowState::new(),
        )?;

        // Await the fold task's catch-up so the wrapper has observed
        // every pre-existing event before any caller-driven ingest can
        // race it. `open_file` is idempotent (same handle).
        let file = redex.open_file(&name, redex_config)?;
        let next_seq = file.next_seq();
        if next_seq > 0 {
            inner.wait_for_seq(next_seq - 1).await.map_err(|folded| {
                CortexAdapterError::FoldStoppedBeforeSeq {
                    wanted: next_seq - 1,
                    folded_through: folded,
                }
            })?;
        }

        Ok(Self {
            inner,
            origin_hash,
            app_seq,
        })
    }

    /// Submit a new task — it enters the chain at step 0, `Submitted`.
    pub fn submit(&self, id: TaskId) -> Result<u64, CortexAdapterError> {
        self.ingest_typed(DISPATCH_TASK_SUBMITTED, &SubmittedPayload { id })
    }

    /// Set a task's status. No-op at fold time if `id` is unknown.
    pub fn transition(&self, id: TaskId, status: TaskStatus) -> Result<u64, CortexAdapterError> {
        self.ingest_typed(
            DISPATCH_TASK_TRANSITIONED,
            &TransitionedPayload { id, status },
        )
    }

    /// Mark the task `Running` (a step is executing).
    pub fn start(&self, id: TaskId) -> Result<u64, CortexAdapterError> {
        self.transition(id, TaskStatus::Running)
    }

    /// Park the task `Waiting` (on a trigger / Thunderdome claim).
    pub fn wait(&self, id: TaskId) -> Result<u64, CortexAdapterError> {
        self.transition(id, TaskStatus::Waiting)
    }

    /// Park the task `Blocked` (on an unmet dependency).
    pub fn block(&self, id: TaskId) -> Result<u64, CortexAdapterError> {
        self.transition(id, TaskStatus::Blocked)
    }

    /// Mark the task `Done` (terminal success).
    pub fn complete(&self, id: TaskId) -> Result<u64, CortexAdapterError> {
        self.transition(id, TaskStatus::Done)
    }

    /// Mark the task `Failed` (terminal failure).
    pub fn fail(&self, id: TaskId) -> Result<u64, CortexAdapterError> {
        self.transition(id, TaskStatus::Failed)
    }

    /// Advance the step cursor (a step completed) — bumps `step`,
    /// resets `attempts`. No-op at fold time if `id` is unknown.
    pub fn advance(&self, id: TaskId) -> Result<u64, CortexAdapterError> {
        self.ingest_typed(DISPATCH_TASK_ADVANCED, &AdvancedPayload { id })
    }

    /// Retry the current step — bumps `attempts`, status → `Running`.
    pub fn retry(&self, id: TaskId) -> Result<u64, CortexAdapterError> {
        self.ingest_typed(DISPATCH_TASK_RETRIED, &RetriedPayload { id })
    }

    /// Delete a task, reclaiming its **whole subtree** — every linked
    /// descendant (shards, spawned children) is removed too, so an
    /// orphaned shard can't keep running (and keep holding a claim).
    /// To also drop triggers waiting on the subtree, read
    /// [`Self::subtree`] before deleting and call
    /// [`TriggerEngine::on_delete`](super::TriggerEngine::on_delete) for
    /// each id.
    pub fn delete(&self, id: TaskId) -> Result<u64, CortexAdapterError> {
        self.ingest_typed(DISPATCH_TASK_DELETED, &DeletedPayload { id })
    }

    /// Record a parent→child lineage edge (a shard / spawned child), so
    /// [`Self::delete`] of the parent cascades to `child`. Idempotent.
    pub fn link(&self, parent: TaskId, child: TaskId) -> Result<u64, CortexAdapterError> {
        self.ingest_typed(DISPATCH_TASK_LINKED, &LinkedPayload { parent, child })
    }

    /// Request cancellation of `id` — a worker-observed signal (the
    /// plan's `cancel.json`). This only records the request; the
    /// single-writer worker polls [`Self::is_cancel_requested`] and
    /// drives the task to a terminal status. Cleared on delete or a
    /// fresh submit.
    pub fn request_cancel(&self, id: TaskId) -> Result<u64, CortexAdapterError> {
        self.ingest_typed(
            DISPATCH_TASK_CANCEL_REQUESTED,
            &CancelRequestedPayload { id },
        )
    }

    /// Read-only access to the materialized state.
    pub fn state(&self) -> Arc<RwLock<WorkflowState>> {
        self.inner.state()
    }

    /// Convenience: the current [`TaskState`] for `id` (acquires the
    /// state read lock briefly).
    pub fn get(&self, id: TaskId) -> Option<TaskState> {
        self.inner.state().read().get(id)
    }

    /// Has cancellation been requested for `id`? The worker polls this.
    pub fn is_cancel_requested(&self, id: TaskId) -> bool {
        self.inner.state().read().is_cancel_requested(id)
    }

    /// `id` plus all its transitive descendants — the subtree a
    /// [`Self::delete`] removes. Read this before deleting to prune the
    /// trigger engine over the same set.
    pub fn subtree(&self, id: TaskId) -> Vec<TaskId> {
        self.inner.state().read().subtree(id)
    }

    /// Roll-up of task counts per status — the observability summary.
    pub fn status_counts(&self) -> StatusCounts {
        self.inner.state().read().status_counts()
    }

    /// Block until every event up through `seq` has been folded.
    pub async fn wait_for_seq(&self, seq: u64) -> Result<(), Option<u64>> {
        self.inner.wait_for_seq(seq).await
    }

    /// Capture a snapshot for restore: `(state_bytes, last_seq)` —
    /// persist both together. Reopening from it via
    /// [`Self::open_from_snapshot`] skips replay up through `last_seq`,
    /// bounding failover catch-up on a long task history (perf note).
    pub fn snapshot(&self) -> Result<(Vec<u8>, Option<u64>), CortexAdapterError> {
        let (inner, last_seq) = self.inner.snapshot()?;
        let payload = WorkflowSnapshotPayload {
            app_seq: self.app_seq.load(Ordering::Acquire),
            inner,
        };
        let bytes = postcard::to_allocvec(&payload).map_err(|e| {
            CortexAdapterError::Redex(RedexError::Encode(format!("workflow snapshot wrap: {e}")))
        })?;
        Ok((bytes, last_seq))
    }

    /// Open the workflow adapter from a snapshot, skipping replay of
    /// events up through `last_seq`.
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
    /// `RedexFileConfig`.
    pub async fn open_from_snapshot_with_config(
        redex: &Redex,
        origin_hash: u64,
        redex_config: RedexFileConfig,
        state_bytes: &[u8],
        last_seq: Option<u64>,
    ) -> Result<Self, CortexAdapterError> {
        let payload: WorkflowSnapshotPayload = postcard::from_bytes(state_bytes).map_err(|e| {
            CortexAdapterError::Redex(RedexError::Encode(format!("workflow snapshot unwrap: {e}")))
        })?;
        let name = ChannelName::new(WORKFLOW_CHANNEL)
            .map_err(|e| CortexAdapterError::Redex(RedexError::Channel(e.to_string())))?;
        let app_seq = Arc::new(AtomicU64::new(payload.app_seq));
        let fold = WatermarkingFold::new(WorkflowFold, app_seq.clone(), origin_hash);
        let inner = CortexAdapter::open_from_snapshot(
            redex,
            &name,
            redex_config.clone(),
            CortexAdapterConfig::default(),
            fold,
            &payload.inner,
            last_seq,
        )?;
        let file = redex.open_file(&name, redex_config)?;
        let next_seq = file.next_seq();
        if next_seq > 0 {
            inner.wait_for_seq(next_seq - 1).await.map_err(|folded| {
                CortexAdapterError::FoldStoppedBeforeSeq {
                    wanted: next_seq - 1,
                    folded_through: folded,
                }
            })?;
        }
        Ok(Self {
            inner,
            origin_hash,
            app_seq,
        })
    }

    /// Build the `EventMeta` header + postcard payload, stamp the
    /// corruption checksum, and append. Mirrors `TasksAdapter`.
    fn ingest_typed<T: serde::Serialize>(
        &self,
        dispatch: u8,
        payload: &T,
    ) -> Result<u64, CortexAdapterError> {
        let app_seq = self.app_seq.fetch_add(1, Ordering::AcqRel);
        let mut meta = EventMeta::new(dispatch, 0, self.origin_hash, app_seq, 0);
        let mut buf = Vec::with_capacity(EVENT_META_SIZE + 64);
        buf.extend_from_slice(&meta.to_bytes());
        buf = postcard::to_extend(payload, buf)
            .map_err(|e| CortexAdapterError::Redex(RedexError::Encode(e.to_string())))?;
        let tail = &buf[EVENT_META_SIZE..];
        meta.checksum = compute_checksum_with_meta(&meta, tail);
        EventMeta::patch_checksum(&mut buf, meta.checksum);
        self.inner.ingest_prebuilt(&buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGIN: u64 = 0x0F10_0001;

    async fn open() -> (Redex, WorkflowAdapter) {
        let redex = Redex::new();
        let adapter = WorkflowAdapter::open(&redex, ORIGIN).await.unwrap();
        (redex, adapter)
    }

    #[tokio::test]
    async fn submit_then_transitions_fold_into_state() {
        let (_redex, wf) = open().await;
        wf.submit(1).unwrap();
        wf.start(1).unwrap();
        wf.advance(1).unwrap(); // step 0 → 1
        wf.retry(1).unwrap(); // attempts 0 → 1, Running
        let seq = wf.complete(1).unwrap();
        wf.wait_for_seq(seq).await.unwrap();

        let st = wf.get(1).expect("task present");
        assert_eq!(st.step, 1);
        assert_eq!(st.attempts, 1);
        assert_eq!(st.status, TaskStatus::Done);
        assert!(st.status.is_terminal());
    }

    #[tokio::test]
    async fn advance_resets_attempts() {
        let (_redex, wf) = open().await;
        wf.submit(7).unwrap();
        wf.retry(7).unwrap();
        wf.retry(7).unwrap(); // attempts = 2
        assert_eq!(
            {
                wf.wait_for_seq(wf.advance(7).unwrap()).await.unwrap();
                wf.get(7).unwrap().attempts
            },
            0
        );
        assert_eq!(wf.get(7).unwrap().step, 1);
    }

    #[tokio::test]
    async fn transition_on_unknown_id_is_a_noop() {
        let (_redex, wf) = open().await;
        let seq = wf.start(42).unwrap(); // never submitted
        wf.wait_for_seq(seq).await.unwrap();
        assert!(wf.get(42).is_none());
    }

    /// Terminal is terminal: a `Done`/`Failed` task can't be moved by a
    /// plain transition or resurrected by retry — so a duplicate /
    /// replayed / buggy-writer event can't un-settle it (review #2).
    #[tokio::test]
    async fn terminal_tasks_cannot_be_resurrected() {
        let (_redex, wf) = open().await;

        // Done is final success: start/retry after complete are no-ops.
        wf.submit(1).unwrap();
        wf.complete(1).unwrap();
        wf.start(1).unwrap();
        let seq = wf.retry(1).unwrap();
        wf.wait_for_seq(seq).await.unwrap();
        assert_eq!(wf.get(1).unwrap().status, TaskStatus::Done);
        assert_eq!(
            wf.get(1).unwrap().attempts,
            0,
            "retry didn't bump a Done task"
        );

        // Failed can't be moved by a plain transition...
        wf.submit(2).unwrap();
        wf.fail(2).unwrap();
        let seq = wf.start(2).unwrap();
        wf.wait_for_seq(seq).await.unwrap();
        assert_eq!(wf.get(2).unwrap().status, TaskStatus::Failed);

        // ...but retry is the sanctioned Failed → Running exit.
        let seq = wf.retry(2).unwrap();
        wf.wait_for_seq(seq).await.unwrap();
        assert_eq!(wf.get(2).unwrap().status, TaskStatus::Running);
        assert_eq!(wf.get(2).unwrap().attempts, 1);
    }

    #[tokio::test]
    async fn delete_reclaims_the_task() {
        let (_redex, wf) = open().await;
        wf.submit(3).unwrap();
        let seq = wf.delete(3).unwrap();
        wf.wait_for_seq(seq).await.unwrap();
        assert!(wf.get(3).is_none());
    }

    /// Delete cascades over the linked subtree — children and
    /// grandchildren go too (corrections #4), while an unrelated subtree
    /// is untouched.
    #[tokio::test]
    async fn delete_cascades_over_the_linked_subtree() {
        let (_redex, wf) = open().await;
        // Tree: 1 → {2, 3}, 3 → {4}. Plus an unrelated 10 → {11}.
        for id in [1, 2, 3, 4, 10, 11] {
            wf.submit(id).unwrap();
        }
        wf.link(1, 2).unwrap();
        wf.link(1, 3).unwrap();
        wf.link(3, 4).unwrap();
        wf.link(10, 11).unwrap();
        let seq = wf.link(10, 11).unwrap(); // idempotent re-link
        wf.wait_for_seq(seq).await.unwrap();

        assert_eq!(wf.subtree(1), vec![1, 2, 3, 4]);
        assert_eq!(wf.state().read().children_of(10), &[11]); // not double-linked

        // Delete the root: the whole subtree is reclaimed in one event.
        let seq = wf.delete(1).unwrap();
        wf.wait_for_seq(seq).await.unwrap();
        for id in [1, 2, 3, 4] {
            assert!(wf.get(id).is_none(), "subtree member {id} reclaimed");
        }
        // The unrelated subtree survives.
        assert!(wf.get(10).is_some());
        assert!(wf.get(11).is_some());
    }

    /// Deleting a non-root detaches it from its parent's child list, so
    /// a later delete of the parent doesn't dangle.
    #[tokio::test]
    async fn delete_detaches_child_from_parent_lineage() {
        let (_redex, wf) = open().await;
        for id in [1, 2, 3] {
            wf.submit(id).unwrap();
        }
        wf.link(1, 2).unwrap();
        let seq = wf.link(1, 3).unwrap();
        wf.wait_for_seq(seq).await.unwrap();

        // Delete child 2 directly.
        let seq = wf.delete(2).unwrap();
        wf.wait_for_seq(seq).await.unwrap();
        assert!(wf.get(2).is_none());
        // Parent 1 now lists only 3.
        assert_eq!(wf.state().read().children_of(1), &[3]);
        assert_eq!(wf.subtree(1), vec![1, 3]);
    }

    /// Replay / failover-resume: a second adapter opened against the
    /// SAME Redex re-folds the chain and reproduces identical state —
    /// the plan's "same chain replays to identical state" property.
    #[tokio::test]
    async fn reopen_replays_to_identical_state() {
        let redex = Redex::new();
        let wf = WorkflowAdapter::open(&redex, ORIGIN).await.unwrap();
        wf.submit(1).unwrap();
        wf.start(1).unwrap();
        wf.advance(1).unwrap();
        wf.submit(2).unwrap();
        let seq = wf.fail(2).unwrap();
        wf.wait_for_seq(seq).await.unwrap();

        // A fresh worker takes over: reopen against the same chain.
        let resumed = WorkflowAdapter::open(&redex, 0x0F10_0002).await.unwrap();
        assert_eq!(resumed.get(1), wf.get(1));
        assert_eq!(resumed.get(2), wf.get(2));
        assert_eq!(
            resumed.get(1).unwrap(),
            TaskState {
                step: 1,
                status: TaskStatus::Running,
                attempts: 0
            }
        );
        assert_eq!(resumed.get(2).unwrap().status, TaskStatus::Failed);
    }

    // --- Phase E: cancel / checkpoint / observability ---

    #[tokio::test]
    async fn cancel_signal_observed_then_cleared_on_delete() {
        let (_redex, wf) = open().await;
        wf.submit(1).unwrap();
        wf.start(1).unwrap();
        let seq = wf.request_cancel(1).unwrap();
        wf.wait_for_seq(seq).await.unwrap();

        // The worker observes the signal and drives the task terminal
        // (its policy — here, Failed). request_cancel itself never
        // changed the status.
        assert!(wf.is_cancel_requested(1));
        assert_eq!(wf.get(1).unwrap().status, TaskStatus::Running);
        let seq = wf.fail(1).unwrap();
        wf.wait_for_seq(seq).await.unwrap();
        assert_eq!(wf.get(1).unwrap().status, TaskStatus::Failed);

        // Delete reclaims the subtree AND clears the cancel signal.
        let seq = wf.delete(1).unwrap();
        wf.wait_for_seq(seq).await.unwrap();
        assert!(!wf.is_cancel_requested(1));
        assert!(wf.get(1).is_none());
    }

    #[tokio::test]
    async fn fresh_submit_clears_a_stale_cancel() {
        let (_redex, wf) = open().await;
        wf.submit(1).unwrap();
        wf.request_cancel(1).unwrap();
        let seq = wf.submit(1).unwrap(); // re-submit resets the task
        wf.wait_for_seq(seq).await.unwrap();
        assert!(
            !wf.is_cancel_requested(1),
            "re-submit clears the stale cancel"
        );
    }

    #[tokio::test]
    async fn status_counts_roll_up() {
        let (_redex, wf) = open().await;
        wf.submit(1).unwrap(); // Submitted
        wf.submit(2).unwrap();
        wf.start(2).unwrap(); // Running
        wf.submit(3).unwrap();
        let seq = wf.complete(3).unwrap(); // Done
        wf.wait_for_seq(seq).await.unwrap();

        let c = wf.status_counts();
        assert_eq!(c.submitted, 1);
        assert_eq!(c.running, 1);
        assert_eq!(c.done, 1);
        assert_eq!(c.total(), 3);
    }

    /// Checkpoint: snapshot a populated workflow and restore it into a
    /// fresh adapter — the state is reproduced without re-folding the
    /// chain (the perf note's bounded failover replay).
    #[tokio::test]
    async fn snapshot_then_restore_reproduces_state() {
        let redex = Redex::new();
        let wf = WorkflowAdapter::open(&redex, ORIGIN).await.unwrap();
        wf.submit(1).unwrap();
        wf.start(1).unwrap();
        wf.advance(1).unwrap();
        wf.submit(2).unwrap();
        let seq = wf.complete(2).unwrap();
        wf.wait_for_seq(seq).await.unwrap();

        let (bytes, last_seq) = wf.snapshot().unwrap();

        // Restore into a fresh Redex: no chain to replay, state comes
        // straight from the checkpoint.
        let redex2 = Redex::new();
        let restored = WorkflowAdapter::open_from_snapshot(&redex2, ORIGIN, &bytes, last_seq)
            .await
            .unwrap();
        assert_eq!(restored.get(1), wf.get(1));
        assert_eq!(restored.get(2), wf.get(2));
        assert_eq!(restored.status_counts(), wf.status_counts());
    }
}
