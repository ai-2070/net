//! Agent-to-agent (A2A) task handoff — the transport-independent core
//! (`HERMES_INTEGRATION_PLAN_V2.md` Phase 3; frozen plan Phase 5).
//!
//! In-root A2A is for **parallelism**: one enrolled agent hands a long job to
//! another (which does *not* share its memory), keeps working, and can cancel
//! mid-run — and the other side **demonstrably stops**. Same-root *sequential*
//! work uses direct capabilities (Phase 2), not this — "asking the other Hermes
//! is briefing an amnesiac colleague with partial memory."
//!
//! This module is the executor side's task manager + the wire types, transport
//! independent:
//!
//! - [`TaskBrief`] — the job + the context the executor needs as **Datafort
//!   artifact refs** (the other agent doesn't share your memory, so inlining
//!   would pretend otherwise).
//! - [`TaskState`] — the lifecycle `requested → accepted → running →
//!   completed{ref} | failed | cancelled`.
//! - [`TaskExecutor`] — the host agent's runner (the plugin wires it to
//!   Hermes's own agent loop).
//! - [`TaskRegistry`] — spawns executors, tracks their state, and routes
//!   cancellation through a [`CancelToken`] so a cancel stops the work.
//! - [`TaskAck`] / [`TaskRecord`] — the requester-facing shapes.
//!
//! The mesh wiring (serve + client) is `mesh_a2a` (gated `net + cortex`).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

/// The lifecycle state of an A2A task.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TaskState {
    /// Submitted but not yet recorded by an executor.
    Requested,
    /// The executor accepted the brief; work is queued.
    Accepted,
    /// The executor is running the job.
    Running,
    /// Done — the result is an **artifact (Datafort) ref**, promoted home
    /// explicitly rather than inlined.
    Completed {
        /// The Datafort/blob ref the result was written to.
        result_ref: String,
    },
    /// The executor failed.
    Failed {
        /// A human-readable failure reason.
        error: String,
    },
    /// Cancelled by the requester; the executor stopped.
    Cancelled,
}

impl TaskState {
    /// Whether this is an end state (no further transitions).
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            TaskState::Completed { .. } | TaskState::Failed { .. } | TaskState::Cancelled
        )
    }

    /// A short label for logging / display.
    pub fn label(&self) -> &'static str {
        match self {
            TaskState::Requested => "requested",
            TaskState::Accepted => "accepted",
            TaskState::Running => "running",
            TaskState::Completed { .. } => "completed",
            TaskState::Failed { .. } => "failed",
            TaskState::Cancelled => "cancelled",
        }
    }
}

/// A task brief: the job plus the context the executor needs, carried as
/// **Datafort artifact refs** (the other agent doesn't share your memory).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskBrief {
    /// The task id (client-generated, unique).
    pub task_id: String,
    /// The job description.
    pub prompt: String,
    /// Artifact refs the executor should read for context.
    #[serde(default)]
    pub context_refs: Vec<String>,
    /// Free-form routing / classification tags.
    #[serde(default)]
    pub tags: Vec<String>,
}

impl TaskBrief {
    /// A brief for `prompt` with a fresh random task id.
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            task_id: random_id(),
            prompt: prompt.into(),
            context_refs: Vec::new(),
            tags: Vec::new(),
        }
    }

    /// Attach context artifact refs (builder-style).
    #[must_use]
    pub fn with_context_refs(mut self, refs: Vec<String>) -> Self {
        self.context_refs = refs;
        self
    }

    /// Attach routing tags (builder-style).
    #[must_use]
    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }

    /// Canonical JSON bytes for the wire.
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// Decode from JSON bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, A2aError> {
        serde_json::from_slice(bytes).map_err(|e| A2aError::Decode(e.to_string()))
    }
}

/// The requester's acknowledgement of a submitted task.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskAck {
    /// The task id (echoes the brief's).
    pub task_id: String,
    /// Whether the executor accepted the brief.
    pub accepted: bool,
    /// Why it was rejected, if it wasn't accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl TaskAck {
    /// Canonical JSON bytes for the wire.
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }
    /// Decode from JSON bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, A2aError> {
        serde_json::from_slice(bytes).map_err(|e| A2aError::Decode(e.to_string()))
    }
}

/// A recorded task: the brief, its current state, and when it last changed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskRecord {
    /// The submitted brief.
    pub brief: TaskBrief,
    /// The current lifecycle state.
    pub state: TaskState,
    /// Unix seconds of the last state change.
    pub updated_at: u64,
}

impl TaskRecord {
    /// Canonical JSON bytes for the wire.
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }
    /// Decode from JSON bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, A2aError> {
        serde_json::from_slice(bytes).map_err(|e| A2aError::Decode(e.to_string()))
    }
}

/// An A2A protocol error.
#[derive(Debug, thiserror::Error)]
pub enum A2aError {
    /// A wire message could not be decoded.
    #[error("a2a decode error: {0}")]
    Decode(String),
    /// The referenced task is not known to this executor.
    #[error("unknown task: {0}")]
    UnknownTask(String),
}

/// A cancellation signal handed to a running [`TaskExecutor`]. A `cancel()` from
/// the requester trips it; a cooperative executor selects on
/// [`cancelled`](Self::cancelled) (or polls [`is_cancelled`](Self::is_cancelled))
/// and returns promptly — so a cancel demonstrably stops the remote work. A
/// non-cooperative executor's future is dropped by the registry's `select!`,
/// which also stops it.
#[derive(Clone)]
pub struct CancelToken {
    tx: Arc<watch::Sender<bool>>,
}

impl CancelToken {
    fn new() -> Self {
        let (tx, _rx) = watch::channel(false);
        Self { tx: Arc::new(tx) }
    }

    /// Trip the token — request cancellation.
    pub fn cancel(&self) {
        // `send_replace` (not `send`) updates the value + notifies receivers
        // *unconditionally* — `send` fails and leaves the value unchanged when
        // there are no receivers yet (the executor subscribes lazily inside
        // `cancelled()`), which would drop the cancellation on the floor.
        self.tx.send_replace(true);
    }

    /// Whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        *self.tx.borrow()
    }

    /// Resolve once cancellation is requested. Race-free: a `watch` receiver
    /// observes the latest value, so a cancel that races the await is not lost.
    pub async fn cancelled(&self) {
        let mut rx = self.tx.subscribe();
        if *rx.borrow() {
            return;
        }
        while rx.changed().await.is_ok() {
            if *rx.borrow() {
                return;
            }
        }
    }
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

/// The host agent's task runner. [`run`](Self::run) executes `brief` and returns
/// an **artifact ref** (a Datafort/blob ref — results are promoted home
/// explicitly, never inlined past a size threshold). A cooperative executor
/// watches `cancel` and returns promptly when it trips.
#[async_trait::async_trait]
pub trait TaskExecutor: Send + Sync {
    /// Run the task, returning the result's artifact ref, or an error string.
    async fn run(&self, brief: TaskBrief, cancel: CancelToken) -> Result<String, String>;
}

/// How long a terminal record outlives its last state change before
/// [`TaskRegistry::submit`]'s housekeeping evicts it: long enough for a
/// requester to poll the outcome (and retry a few times), short enough that a
/// long-lived executor's table doesn't grow without bound. `forget()` remains
/// the immediate path once the result is retrieved.
pub const TERMINAL_RECORD_TTL_SECS: u64 = 60 * 60;

struct Entry {
    brief: TaskBrief,
    state: TaskState,
    updated_at: u64,
    cancel: CancelToken,
}

/// The executor side's live task table: spawns [`TaskExecutor`]s, tracks their
/// [`TaskState`], and routes cancellation. Cheap to clone (shared inner).
#[derive(Clone, Default)]
pub struct TaskRegistry {
    inner: Arc<Mutex<HashMap<String, Entry>>>,
}

impl TaskRegistry {
    /// A fresh, empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Accept `brief`, spawn `executor` to run it, and return the task id. The
    /// spawned task drives `accepted → running → completed{ref} | failed |
    /// cancelled`, racing the executor against the cancel token so a cancel
    /// stops the work (a cooperative executor also watches the token).
    ///
    /// Idempotent per task id: a re-submit of an id the registry already
    /// knows (an nRPC retransmit of the accept) returns the id without
    /// spawning a second executor — re-inserting would orphan the first
    /// entry's cancel token (making the original run uncancellable) and race
    /// two executors on the final state.
    ///
    /// Also evicts terminal records older than [`TERMINAL_RECORD_TTL_SECS`]
    /// (see [`Self::evict_terminal`]) so a long-lived executor's table doesn't
    /// grow without bound; [`Self::forget`] remains the immediate path.
    ///
    /// Requires a tokio runtime context (the serve handler / a `#[tokio::test]`
    /// provides it).
    pub fn submit(&self, brief: TaskBrief, executor: Arc<dyn TaskExecutor>) -> String {
        let id = brief.task_id.clone();
        let cancel = CancelToken::new();
        {
            let mut map = self.inner.lock();
            let now = now_secs();
            map.retain(|_, e| {
                !e.state.is_terminal()
                    || now.saturating_sub(e.updated_at) <= TERMINAL_RECORD_TTL_SECS
            });
            if map.contains_key(&id) {
                return id;
            }
            map.insert(
                id.clone(),
                Entry {
                    brief: brief.clone(),
                    state: TaskState::Accepted,
                    updated_at: now,
                    cancel: cancel.clone(),
                },
            );
        }

        let inner = Arc::clone(&self.inner);
        let id_run = id.clone();
        tokio::spawn(async move {
            set_state(&inner, &id_run, TaskState::Running);
            // Panic containment: an executor panic unwinds this task and
            // would otherwise skip the final set_state, stranding the entry in
            // `Running` forever (status/wait_terminal poll indefinitely, the
            // entry leaks). The guard's Drop records a terminal state on
            // unwind; the normal path disarms it before recording its own.
            let mut panic_guard = PanicGuard {
                inner: Arc::clone(&inner),
                id: id_run.clone(),
                cancel: cancel.clone(),
                armed: true,
            };
            // Cooperative cancellation: the executor watches the token and
            // returns promptly when it trips (the Hermes agent loop is wired to
            // its interrupt machinery). The select! is the backstop for a
            // NON-cooperative executor — one that ignores the token — whose
            // future is dropped when the token trips, so a cancel stops the
            // work either way ([`CancelToken`]'s documented guarantee).
            // `biased` polls the executor first so a result that's already in
            // wins over a simultaneous cancel. Whatever it returns, a requested
            // cancel makes the outcome `Cancelled` — the requester asked to
            // stop, so a partial result isn't promoted.
            let r = tokio::select! {
                biased;
                r = executor.run(brief, cancel.clone()) => r,
                _ = cancel.cancelled() => Err("cancelled".to_string()),
            };
            panic_guard.armed = false;
            let final_state = if cancel.is_cancelled() {
                TaskState::Cancelled
            } else {
                match r {
                    Ok(result_ref) => TaskState::Completed { result_ref },
                    Err(error) => TaskState::Failed { error },
                }
            };
            set_state(&inner, &id_run, final_state);
        });
        id
    }

    /// The current state of `task_id`, if known.
    pub fn status(&self, task_id: &str) -> Option<TaskState> {
        self.inner.lock().get(task_id).map(|e| e.state.clone())
    }

    /// The full record of `task_id`, if known.
    pub fn record(&self, task_id: &str) -> Option<TaskRecord> {
        self.inner.lock().get(task_id).map(|e| TaskRecord {
            brief: e.brief.clone(),
            state: e.state.clone(),
            updated_at: e.updated_at,
        })
    }

    /// Request cancellation of `task_id`. Returns `true` if it existed and was
    /// still in flight (a terminal or unknown task returns `false`). Trips the
    /// token; the spawned task transitions to `Cancelled` once the executor
    /// stops.
    pub fn cancel(&self, task_id: &str) -> bool {
        let map = self.inner.lock();
        match map.get(task_id) {
            Some(e) if !e.state.is_terminal() => {
                e.cancel.cancel();
                true
            }
            _ => false,
        }
    }

    /// Every recorded task, newest-updated first.
    pub fn list(&self) -> Vec<TaskRecord> {
        let mut recs: Vec<TaskRecord> = self
            .inner
            .lock()
            .values()
            .map(|e| TaskRecord {
                brief: e.brief.clone(),
                state: e.state.clone(),
                updated_at: e.updated_at,
            })
            .collect();
        recs.sort_by_key(|r| std::cmp::Reverse(r.updated_at));
        recs
    }

    /// Drop a task's record (housekeeping once the requester has the result).
    /// Returns whether a record existed.
    pub fn forget(&self, task_id: &str) -> bool {
        self.inner.lock().remove(task_id).is_some()
    }

    /// Evict terminal records whose last state change is more than `ttl_secs`
    /// before `now`, returning how many were dropped. In-flight tasks are
    /// never touched. [`Self::submit`] runs this automatically with
    /// [`TERMINAL_RECORD_TTL_SECS`]; exposed for callers that want a tighter
    /// housekeeping schedule than "on the next submission".
    pub fn evict_terminal(&self, ttl_secs: u64, now: u64) -> usize {
        let mut map = self.inner.lock();
        let before = map.len();
        map.retain(|_, e| {
            !e.state.is_terminal() || now.saturating_sub(e.updated_at) <= ttl_secs
        });
        before - map.len()
    }
}

/// Drop-guard armed while [`TaskRegistry::submit`]'s spawned task awaits the
/// executor: a panicking executor unwinds the task, and without this the
/// final `set_state` never runs — the entry is stranded non-terminal (`cancel`
/// returns `true` but nothing transitions, `status`/`wait_terminal` poll
/// `Running` forever) and leaks. On an armed drop the task is recorded
/// `Failed` — or `Cancelled` when the token was tripped (the requester asked
/// to stop; the panic is incidental). The normal completion path disarms it.
struct PanicGuard {
    inner: Arc<Mutex<HashMap<String, Entry>>>,
    id: String,
    cancel: CancelToken,
    armed: bool,
}

impl Drop for PanicGuard {
    fn drop(&mut self) {
        if self.armed {
            let state = if self.cancel.is_cancelled() {
                TaskState::Cancelled
            } else {
                TaskState::Failed {
                    error: "executor panicked".to_string(),
                }
            };
            set_state(&self.inner, &self.id, state);
        }
    }
}

/// Set a task's state, never overwriting a terminal state (so a late `Running`
/// can't clobber a `Cancelled` recorded by a racing cancel).
fn set_state(inner: &Arc<Mutex<HashMap<String, Entry>>>, id: &str, state: TaskState) {
    let mut map = inner.lock();
    if let Some(e) = map.get_mut(id) {
        if !e.state.is_terminal() {
            e.state = state;
            e.updated_at = now_secs();
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A random 16-byte hex id. A `getrandom` failure aborts the process (mirroring
/// the identity layer): these helpers are reachable from FFI and a predictable
/// task id is worse than a crash.
fn random_id() -> String {
    let mut b = [0u8; 16];
    if let Err(e) = getrandom::fill(&mut b) {
        eprintln!("FATAL: A2A task-id getrandom failure ({e:?}); aborting");
        std::process::abort();
    }
    let mut s = String::with_capacity(32);
    use std::fmt::Write as _;
    for x in b {
        let _ = write!(s, "{x:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    /// An executor that returns a fixed result ref after an optional pause,
    /// recording whether it saw the cancel.
    struct MockExecutor {
        result: String,
        saw_cancel: Arc<AtomicBool>,
        /// If true, wait for cancellation instead of completing.
        wait_for_cancel: bool,
    }

    #[async_trait::async_trait]
    impl TaskExecutor for MockExecutor {
        async fn run(&self, _brief: TaskBrief, cancel: CancelToken) -> Result<String, String> {
            if self.wait_for_cancel {
                cancel.cancelled().await;
                self.saw_cancel.store(true, Ordering::SeqCst);
                return Err("cancelled".to_string());
            }
            Ok(self.result.clone())
        }
    }

    async fn wait_terminal(reg: &TaskRegistry, id: &str) -> TaskState {
        for _ in 0..200 {
            if let Some(s) = reg.status(id) {
                if s.is_terminal() {
                    return s;
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("task {id} did not reach a terminal state");
    }

    #[tokio::test]
    async fn a_task_runs_to_completion_with_a_result_ref() {
        let reg = TaskRegistry::new();
        let brief = TaskBrief::new("summarize the logs");
        let id = reg.submit(
            brief,
            Arc::new(MockExecutor {
                result: "blob://result-123".to_string(),
                saw_cancel: Arc::new(AtomicBool::new(false)),
                wait_for_cancel: false,
            }),
        );
        let state = wait_terminal(&reg, &id).await;
        assert_eq!(
            state,
            TaskState::Completed {
                result_ref: "blob://result-123".to_string()
            }
        );
    }

    #[tokio::test]
    async fn cancel_stops_a_running_task() {
        let reg = TaskRegistry::new();
        let saw = Arc::new(AtomicBool::new(false));
        let id = reg.submit(
            TaskBrief::new("grind forever"),
            Arc::new(MockExecutor {
                result: String::new(),
                saw_cancel: Arc::clone(&saw),
                wait_for_cancel: true,
            }),
        );
        // Let it reach Running, then cancel.
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(reg.cancel(&id), "an in-flight task cancels");
        let state = wait_terminal(&reg, &id).await;
        assert_eq!(state, TaskState::Cancelled);
        assert!(
            saw.load(Ordering::SeqCst),
            "the executor observed the cancel"
        );
        // Cancelling a terminal task is a no-op.
        assert!(!reg.cancel(&id));
    }

    /// An executor that IGNORES the cancel token — it just sleeps. Records
    /// whether its future was dropped (`dropped`, via a guard) and whether it
    /// ever ran to completion (`completed`).
    struct StubbornExecutor {
        dropped: Arc<AtomicBool>,
        completed: Arc<AtomicBool>,
    }

    struct SetOnDrop(Arc<AtomicBool>);
    impl Drop for SetOnDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[async_trait::async_trait]
    impl TaskExecutor for StubbornExecutor {
        async fn run(&self, _brief: TaskBrief, _cancel: CancelToken) -> Result<String, String> {
            let _guard = SetOnDrop(Arc::clone(&self.dropped));
            tokio::time::sleep(Duration::from_secs(3600)).await;
            self.completed.store(true, Ordering::SeqCst);
            Ok("blob://too-late".to_string())
        }
    }

    #[tokio::test]
    async fn cancel_stops_a_non_cooperative_executor() {
        // The registry's select! must drop an executor that ignores the token —
        // the CancelToken doc's "a non-cooperative executor's future is dropped
        // by the registry's select!" guarantee.
        let reg = TaskRegistry::new();
        let dropped = Arc::new(AtomicBool::new(false));
        let completed = Arc::new(AtomicBool::new(false));
        let id = reg.submit(
            TaskBrief::new("ignore the token"),
            Arc::new(StubbornExecutor {
                dropped: Arc::clone(&dropped),
                completed: Arc::clone(&completed),
            }),
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(reg.cancel(&id));
        // Terminal promptly — not after the executor's hour-long sleep.
        let state = wait_terminal(&reg, &id).await;
        assert_eq!(state, TaskState::Cancelled);
        assert!(
            dropped.load(Ordering::SeqCst),
            "the non-cooperative executor's future was dropped"
        );
        assert!(!completed.load(Ordering::SeqCst));
    }

    /// An executor that counts how many times it was started, then waits for
    /// the cancel token.
    struct CountingExecutor {
        runs: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl TaskExecutor for CountingExecutor {
        async fn run(&self, _brief: TaskBrief, cancel: CancelToken) -> Result<String, String> {
            self.runs.fetch_add(1, Ordering::SeqCst);
            cancel.cancelled().await;
            Err("cancelled".to_string())
        }
    }

    #[tokio::test]
    async fn duplicate_submit_is_idempotent() {
        // An nRPC retransmit of an accepted brief must not spawn a second
        // executor or replace the entry (which would orphan the first run's
        // cancel token, leaving it uncancellable).
        let reg = TaskRegistry::new();
        let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let brief = TaskBrief::new("retransmitted job");
        let exec = Arc::new(CountingExecutor {
            runs: Arc::clone(&runs),
        });
        let id = reg.submit(brief.clone(), exec.clone());
        let id2 = reg.submit(brief, exec);
        assert_eq!(id, id2, "the retransmit acks the same id");

        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(runs.load(Ordering::SeqCst), 1, "exactly one executor ran");

        // The (single, original) run is still wired to the entry's token.
        assert!(reg.cancel(&id));
        let state = wait_terminal(&reg, &id).await;
        assert_eq!(state, TaskState::Cancelled);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn terminal_records_evict_after_ttl_running_ones_never() {
        let reg = TaskRegistry::new();
        let done = reg.submit(
            TaskBrief::new("quick"),
            Arc::new(MockExecutor {
                result: "blob://r".to_string(),
                saw_cancel: Arc::new(AtomicBool::new(false)),
                wait_for_cancel: false,
            }),
        );
        wait_terminal(&reg, &done).await;
        // Within the TTL the record survives housekeeping.
        assert_eq!(reg.evict_terminal(TERMINAL_RECORD_TTL_SECS, now_secs()), 0);
        assert!(reg.status(&done).is_some());

        let live = reg.submit(
            TaskBrief::new("still running"),
            Arc::new(MockExecutor {
                result: String::new(),
                saw_cancel: Arc::new(AtomicBool::new(false)),
                wait_for_cancel: true,
            }),
        );
        tokio::time::sleep(Duration::from_millis(20)).await;

        // Past the TTL the terminal record evicts; the in-flight one never.
        let future = now_secs() + TERMINAL_RECORD_TTL_SECS + 10;
        assert_eq!(reg.evict_terminal(TERMINAL_RECORD_TTL_SECS, future), 1);
        assert!(reg.status(&done).is_none());
        assert!(reg.status(&live).is_some());
        reg.cancel(&live);
    }

    /// An executor that panics mid-run.
    struct PanickingExecutor;

    #[async_trait::async_trait]
    impl TaskExecutor for PanickingExecutor {
        async fn run(&self, _brief: TaskBrief, _cancel: CancelToken) -> Result<String, String> {
            panic!("executor blew up");
        }
    }

    #[tokio::test]
    async fn a_panicking_executor_marks_the_task_failed() {
        // An executor panic must not strand the task in `Running` — the guard
        // records `Failed` so pollers terminate and the entry can be forgotten.
        let reg = TaskRegistry::new();
        let id = reg.submit(TaskBrief::new("kaboom"), Arc::new(PanickingExecutor));
        let state = wait_terminal(&reg, &id).await;
        assert_eq!(
            state,
            TaskState::Failed {
                error: "executor panicked".to_string()
            }
        );
        // Terminal: cancel is a no-op, the record can be forgotten.
        assert!(!reg.cancel(&id));
        assert!(reg.forget(&id));
    }

    #[tokio::test]
    async fn cancel_of_unknown_task_is_false() {
        let reg = TaskRegistry::new();
        assert!(!reg.cancel("nope"));
        assert!(reg.status("nope").is_none());
    }

    #[tokio::test]
    async fn cancel_token_is_race_free() {
        let token = CancelToken::new();
        token.cancel(); // trip BEFORE awaiting
        assert!(token.is_cancelled());
        // cancelled() must still resolve promptly (the watch retains the value).
        tokio::time::timeout(Duration::from_millis(100), token.cancelled())
            .await
            .expect("cancelled() resolves after a pre-await cancel");
    }

    #[test]
    fn wire_types_round_trip() {
        let brief = TaskBrief::new("do a thing")
            .with_context_refs(vec!["blob://ctx".to_string()])
            .with_tags(vec!["region:office".to_string()]);
        assert_eq!(TaskBrief::decode(&brief.encode()).unwrap(), brief);

        let ack = TaskAck {
            task_id: brief.task_id.clone(),
            accepted: true,
            reason: None,
        };
        assert_eq!(TaskAck::decode(&ack.encode()).unwrap(), ack);

        let rec = TaskRecord {
            brief,
            state: TaskState::Completed {
                result_ref: "blob://r".to_string(),
            },
            updated_at: 42,
        };
        let back = TaskRecord::decode(&rec.encode()).unwrap();
        assert_eq!(back, rec);
        assert!(back.state.is_terminal());
        assert_eq!(back.state.label(), "completed");
    }

    #[test]
    fn task_ids_are_unique() {
        let a = TaskBrief::new("x");
        let b = TaskBrief::new("x");
        assert_ne!(a.task_id, b.task_id);
        assert_eq!(a.task_id.len(), 32);
    }
}
