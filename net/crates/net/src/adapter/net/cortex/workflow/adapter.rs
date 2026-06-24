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

use super::super::super::channel::ChannelName;
use super::super::super::redex::{Redex, RedexError, RedexFileConfig};
use super::super::adapter::CortexAdapter;
use super::super::config::CortexAdapterConfig;
use super::super::error::CortexAdapterError;
use super::super::meta::{compute_checksum_with_meta, EventMeta, EVENT_META_SIZE};
use super::super::watermark::WatermarkingFold;
use super::dispatch::{
    DISPATCH_TASK_ADVANCED, DISPATCH_TASK_DELETED, DISPATCH_TASK_RETRIED, DISPATCH_TASK_SUBMITTED,
    DISPATCH_TASK_TRANSITIONED, WORKFLOW_CHANNEL,
};
use super::fold::WorkflowFold;
use super::state::WorkflowState;
use super::types::{
    AdvancedPayload, DeletedPayload, RetriedPayload, SubmittedPayload, TaskId, TaskState,
    TaskStatus, TransitionedPayload,
};

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
        self.ingest_typed(DISPATCH_TASK_TRANSITIONED, &TransitionedPayload { id, status })
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

    /// Delete a task, reclaiming its subtree.
    pub fn delete(&self, id: TaskId) -> Result<u64, CortexAdapterError> {
        self.ingest_typed(DISPATCH_TASK_DELETED, &DeletedPayload { id })
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

    /// Block until every event up through `seq` has been folded.
    pub async fn wait_for_seq(&self, seq: u64) -> Result<(), Option<u64>> {
        self.inner.wait_for_seq(seq).await
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
        assert_eq!({ wf.wait_for_seq(wf.advance(7).unwrap()).await.unwrap(); wf.get(7).unwrap().attempts }, 0);
        assert_eq!(wf.get(7).unwrap().step, 1);
    }

    #[tokio::test]
    async fn transition_on_unknown_id_is_a_noop() {
        let (_redex, wf) = open().await;
        let seq = wf.start(42).unwrap(); // never submitted
        wf.wait_for_seq(seq).await.unwrap();
        assert!(wf.get(42).is_none());
    }

    #[tokio::test]
    async fn delete_reclaims_the_task() {
        let (_redex, wf) = open().await;
        wf.submit(3).unwrap();
        let seq = wf.delete(3).unwrap();
        wf.wait_for_seq(seq).await.unwrap();
        assert!(wf.get(3).is_none());
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
}
