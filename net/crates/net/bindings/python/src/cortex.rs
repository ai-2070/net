//! Python bindings for the CortEX adapter slice — tasks + memories.
//!
//! Sync surface: every method blocks on the underlying tokio runtime
//! and releases the GIL via `py.detach()` around async waits. Watch
//! iterators use Python's native sync iterator protocol (`__iter__` /
//! `__next__` — `StopIteration` on end).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures::stream::BoxStream;
use futures::StreamExt;
use pyo3::exceptions::{PyRuntimeError, PyStopIteration, PyValueError};
use pyo3::prelude::*;
use tokio::runtime::Runtime;
use tokio::sync::{Mutex as TokioMutex, Notify};

use ::net::adapter::net::channel::ChannelName;
use ::net::adapter::net::cortex::memories::{
    MemoriesAdapter as InnerMemoriesAdapter, Memory as InnerMemory, OrderBy as InnerMemoriesOrderBy,
};
use ::net::adapter::net::cortex::tasks::{
    OrderBy as InnerTasksOrderBy, Task as InnerTask, TaskStatus as InnerTaskStatus,
    TasksAdapter as InnerTasksAdapter,
};
use ::net::adapter::net::redex::{
    FsyncPolicy as InnerFsyncPolicy, Redex as InnerRedex, RedexError as InnerRedexError,
    RedexEvent as InnerRedexEvent, RedexFile as InnerRedexFile, RedexFileConfig,
};
use bytes::Bytes;

pyo3::create_exception!(
    _net,
    CortexError,
    pyo3::exceptions::PyException,
    "Raised when a CortEX adapter operation fails. Covers `adapter \
     closed`, `fold stopped at seq N`, and underlying RedEX storage \
     errors. Catch with `except CortexError:`."
);

pyo3::create_exception!(
    _net,
    NetDbError,
    pyo3::exceptions::PyException,
    "Raised when a NetDB operation fails. Covers snapshot encode / \
     decode errors and missing-model accesses (tasks / memories not \
     enabled on this handle). Per-adapter operations raise \
     `CortexError`; this class is reserved for errors that span the \
     NetDB handle itself."
);

pyo3::create_exception!(
    _net,
    RedexError,
    pyo3::exceptions::PyException,
    "Raised when a raw RedEX file operation fails: append / tail / \
     read / sync / close, invalid channel names, mutually-exclusive \
     config options, or `persistent=True` without a `persistent_dir` \
     on the owning `Redex`."
);

// =========================================================================
// Shared helpers
// =========================================================================

/// One shared tokio runtime for every CortEX / RedEX handle. Opening
/// N adapters / files used to spawn N runtimes (one per handle),
/// each with its own worker thread pool — wasteful at memory and CPU.
/// A single multi-threaded runtime drives every handle; construction
/// is lazy so Python tests that never touch cortex pay nothing.
fn make_runtime() -> PyResult<Arc<Runtime>> {
    use std::sync::OnceLock;
    static RT: OnceLock<Arc<Runtime>> = OnceLock::new();
    // Can't use `get_or_init` with a fallible init, so do the check
    // manually. Runtime::new() returns an io::Error that's normally
    // surfaced to the caller on first-touch; if it fails once it'll
    // keep failing, so caching the error (or panicking) would leave
    // subsequent callers without a recovery path. Instead, try fresh
    // each time the slot is empty.
    if let Some(existing) = RT.get() {
        return Ok(existing.clone());
    }
    let rt = Runtime::new()
        .map(Arc::new)
        .map_err(|e| PyRuntimeError::new_err(format!("failed to create tokio runtime: {}", e)))?;
    // If another thread raced and populated the slot, reuse theirs.
    Ok(RT.get_or_init(|| rt).clone())
}

fn parse_task_status(s: &str) -> PyResult<InnerTaskStatus> {
    match s.to_lowercase().as_str() {
        "pending" => Ok(InnerTaskStatus::Pending),
        "completed" => Ok(InnerTaskStatus::Completed),
        other => Err(PyValueError::new_err(format!(
            "invalid status {:?} (expected 'pending' or 'completed')",
            other
        ))),
    }
}

fn task_status_str(s: InnerTaskStatus) -> &'static str {
    match s {
        InnerTaskStatus::Pending => "pending",
        InnerTaskStatus::Completed => "completed",
    }
}

fn parse_tasks_order_by(s: &str) -> PyResult<InnerTasksOrderBy> {
    match s.to_lowercase().as_str() {
        "id_asc" => Ok(InnerTasksOrderBy::IdAsc),
        "id_desc" => Ok(InnerTasksOrderBy::IdDesc),
        "created_asc" => Ok(InnerTasksOrderBy::CreatedAsc),
        "created_desc" => Ok(InnerTasksOrderBy::CreatedDesc),
        "updated_asc" => Ok(InnerTasksOrderBy::UpdatedAsc),
        "updated_desc" => Ok(InnerTasksOrderBy::UpdatedDesc),
        other => Err(PyValueError::new_err(format!(
            "invalid order_by {:?} (expected one of id_asc|id_desc|created_asc|created_desc|updated_asc|updated_desc)",
            other
        ))),
    }
}

fn cfg_from_persistent(persistent: bool) -> RedexFileConfig {
    if persistent {
        RedexFileConfig::default().with_persistent(true)
    } else {
        RedexFileConfig::default()
    }
}

fn parse_memories_order_by(s: &str) -> PyResult<InnerMemoriesOrderBy> {
    match s.to_lowercase().as_str() {
        "id_asc" => Ok(InnerMemoriesOrderBy::IdAsc),
        "id_desc" => Ok(InnerMemoriesOrderBy::IdDesc),
        "created_asc" => Ok(InnerMemoriesOrderBy::CreatedAsc),
        "created_desc" => Ok(InnerMemoriesOrderBy::CreatedDesc),
        "updated_asc" => Ok(InnerMemoriesOrderBy::UpdatedAsc),
        "updated_desc" => Ok(InnerMemoriesOrderBy::UpdatedDesc),
        other => Err(PyValueError::new_err(format!(
            "invalid order_by {:?}",
            other
        ))),
    }
}

// =========================================================================
// Redex manager
// =========================================================================

/// Local RedEX manager. One handle shared across all adapters on
/// this node.
///
/// `persistent_dir`: if provided, files opened through adapters with
/// `persistent=True` write to `<persistent_dir>/<channel_path>/{idx,dat}`
/// and replay from those files on reopen. Heap-only when `None`.
#[pyclass(name = "Redex")]
pub struct PyRedex {
    inner: Arc<InnerRedex>,
    persistent_dir: Option<String>,
}

#[pymethods]
impl PyRedex {
    #[new]
    #[pyo3(signature = (persistent_dir = None))]
    fn new(persistent_dir: Option<String>) -> Self {
        let inner = match &persistent_dir {
            Some(dir) => InnerRedex::new().with_persistent_dir(dir),
            None => InnerRedex::new(),
        };
        Self {
            inner: Arc::new(inner),
            persistent_dir,
        }
    }

    fn __repr__(&self) -> String {
        match &self.persistent_dir {
            Some(dir) => format!("Redex(persistent_dir={:?})", dir),
            None => "Redex(local)".into(),
        }
    }

    /// Open (or get) a raw RedEX file for domain-agnostic persistent
    /// logging. Returns the same handle across repeat calls with the
    /// same `name`; config is honored only on first open.
    ///
    /// Use this when you want an append-only event log without the
    /// CortEX fold / typed-adapter layer. With `persistent=True`, this
    /// `Redex` must have been constructed with a `persistent_dir`.
    ///
    /// `fsync_every_n` and `fsync_interval_ms` are mutually exclusive;
    /// leave both unset for the default "never fsync on append"
    /// policy (`close()` and explicit `sync()` still fsync).
    #[pyo3(signature = (
        name,
        *,
        persistent = false,
        fsync_every_n = None,
        fsync_interval_ms = None,
        retention_max_events = None,
        retention_max_bytes = None,
        retention_max_age_ms = None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn open_file(
        &self,
        name: &str,
        persistent: bool,
        fsync_every_n: Option<u64>,
        fsync_interval_ms: Option<u64>,
        retention_max_events: Option<u64>,
        retention_max_bytes: Option<u64>,
        retention_max_age_ms: Option<u64>,
    ) -> PyResult<PyRedexFile> {
        let channel = ChannelName::new(name).map_err(|e| RedexError::new_err(format!("{}", e)))?;
        let mut cfg = RedexFileConfig {
            persistent,
            ..RedexFileConfig::default()
        };
        match (fsync_every_n, fsync_interval_ms) {
            (Some(_), Some(_)) => {
                return Err(RedexError::new_err(
                    "fsync_every_n and fsync_interval_ms are mutually exclusive",
                ));
            }
            (Some(0), _) => {
                return Err(RedexError::new_err("fsync_every_n must be > 0"));
            }
            (Some(n), None) => {
                cfg.fsync_policy = InnerFsyncPolicy::EveryN(n);
            }
            (None, Some(0)) => {
                return Err(RedexError::new_err("fsync_interval_ms must be > 0"));
            }
            (None, Some(ms)) => {
                cfg.fsync_policy = InnerFsyncPolicy::Interval(std::time::Duration::from_millis(ms));
            }
            (None, None) => {}
        }
        cfg.retention_max_events = retention_max_events;
        cfg.retention_max_bytes = retention_max_bytes;
        if let Some(ms) = retention_max_age_ms {
            cfg.retention_max_age_ns = Some(ms.saturating_mul(1_000_000));
        }
        let file = self
            .inner
            .open_file(&channel, cfg)
            .map_err(|e| RedexError::new_err(format!("open_file: {}", e)))?;
        let runtime = make_runtime()?;
        Ok(PyRedexFile {
            inner: Arc::new(file),
            runtime,
        })
    }
}

// =========================================================================
// Raw RedEX file — domain-agnostic event log
// =========================================================================

/// A materialized RedEX event: `seq` + `payload` + checksum / inline
/// flag. Clone is O(payload size).
#[pyclass(name = "RedexEvent", from_py_object)]
#[derive(Clone)]
pub struct PyRedexEvent {
    #[pyo3(get)]
    pub seq: u64,
    #[pyo3(get)]
    pub payload: Vec<u8>,
    /// Low-28-bit xxh3 truncation of the payload at append time.
    #[pyo3(get)]
    pub checksum: u32,
    /// True if the payload was stored inline in the 20-byte entry.
    #[pyo3(get)]
    pub is_inline: bool,
}

impl From<InnerRedexEvent> for PyRedexEvent {
    fn from(ev: InnerRedexEvent) -> Self {
        PyRedexEvent {
            seq: ev.entry.seq,
            payload: ev.payload.to_vec(),
            checksum: ev.entry.checksum(),
            is_inline: ev.entry.is_inline(),
        }
    }
}

#[pymethods]
impl PyRedexEvent {
    fn __repr__(&self) -> String {
        format!(
            "RedexEvent(seq={}, payload=<{} bytes>, checksum={:#010x}, is_inline={})",
            self.seq,
            self.payload.len(),
            self.checksum,
            self.is_inline
        )
    }
}

/// Raw RedEX file handle. Cheap to share — methods take `&self`.
#[pyclass(name = "RedexFile")]
pub struct PyRedexFile {
    inner: Arc<InnerRedexFile>,
    runtime: Arc<Runtime>,
}

#[pymethods]
impl PyRedexFile {
    /// Append one payload. Returns the assigned sequence number.
    fn append(&self, payload: &[u8]) -> PyResult<u64> {
        self.inner
            .append(payload)
            .map_err(|e| RedexError::new_err(format!("append: {}", e)))
    }

    /// Append a batch atomically. Returns the seq of the FIRST event,
    /// or `None` if `payloads` was empty (no events appended).
    /// Subsequent events are `first + 0, first + 1, ...`.
    ///
    /// The underlying `RedexFile::append_batch`
    /// returns `Result<Option<u64>>` so callers can distinguish
    /// "wrote zero" from "wrote one with seq N". The Python
    /// signature mirrors that — `int | None`.
    fn append_batch(&self, payloads: Vec<Vec<u8>>) -> PyResult<Option<u64>> {
        let bytes: Vec<Bytes> = payloads.into_iter().map(Bytes::from).collect();
        self.inner
            .append_batch(&bytes)
            .map_err(|e| RedexError::new_err(format!("append_batch: {}", e)))
    }

    /// Read the half-open range `[start, end)` from the in-memory
    /// index. Only retained entries are returned; evicted seqs are
    /// silently skipped.
    fn read_range(&self, start: u64, end: u64) -> Vec<PyRedexEvent> {
        self.inner
            .read_range(start, end)
            .into_iter()
            .map(PyRedexEvent::from)
            .collect()
    }

    /// Number of retained events (post-retention eviction).
    fn __len__(&self) -> usize {
        self.inner.len()
    }

    /// Open a live tail. Returns a sync Python iterator that yields
    /// events with `seq >= from_seq` (default `0`) — backfills the
    /// retained range atomically, then streams live appends. Stop
    /// early with `iter.close()` or let the iterator run to
    /// `StopIteration` when the file closes.
    #[pyo3(signature = (from_seq = 0))]
    fn tail(&self, from_seq: u64) -> PyRedexTailIter {
        use futures::StreamExt;
        let adapter = self.inner.clone();
        let runtime = self.runtime.clone();
        let stream = runtime.block_on(async move { adapter.tail(from_seq).boxed() });
        PyRedexTailIter {
            inner: Arc::new(RedexTailIterInner {
                stream: TokioMutex::new(Some(stream)),
                shutdown: Notify::new(),
                is_shutdown: AtomicBool::new(false),
            }),
            runtime: self.runtime.clone(),
        }
    }

    /// Explicit fsync. Always fsyncs regardless of configured policy;
    /// no-op on heap-only files.
    fn sync(&self) -> PyResult<()> {
        self.inner
            .sync()
            .map_err(|e| RedexError::new_err(format!("sync: {}", e)))
    }

    /// Close the file. Outstanding tail iterators terminate on their
    /// next `__next__` call with `StopIteration`.
    fn close(&self) -> PyResult<()> {
        self.inner
            .close()
            .map_err(|e| RedexError::new_err(format!("close: {}", e)))
    }
}

struct RedexTailIterInner {
    stream: TokioMutex<
        Option<BoxStream<'static, std::result::Result<InnerRedexEvent, InnerRedexError>>>,
    >,
    shutdown: Notify,
    is_shutdown: AtomicBool,
}

/// Sync Python iterator over a live `RedexFile.tail()`.
#[pyclass(name = "RedexTailIter")]
pub struct PyRedexTailIter {
    inner: Arc<RedexTailIterInner>,
    runtime: Arc<Runtime>,
}

#[pymethods]
impl PyRedexTailIter {
    fn __iter__(slf: PyRef<Self>) -> PyRef<Self> {
        slf
    }

    fn __next__(&self, py: Python<'_>) -> PyResult<PyRedexEvent> {
        let inner = self.inner.clone();
        let runtime = self.runtime.clone();
        let outcome = py.detach(move || {
            runtime.block_on(async move {
                if inner.is_shutdown.load(Ordering::Acquire) {
                    return TailNext::End;
                }
                let mut guard = inner.stream.lock().await;
                let stream = match guard.as_mut() {
                    Some(s) => s,
                    None => return TailNext::End,
                };

                let shutdown_fut = inner.shutdown.notified();
                tokio::pin!(shutdown_fut);
                shutdown_fut.as_mut().enable();

                if inner.is_shutdown.load(Ordering::Acquire) {
                    *guard = None;
                    return TailNext::End;
                }

                tokio::select! {
                    biased;
                    _ = shutdown_fut => {
                        *guard = None;
                        TailNext::End
                    }
                    msg = stream.next() => match msg {
                        Some(Ok(ev)) => TailNext::Event(ev),
                        Some(Err(InnerRedexError::Closed)) => {
                            *guard = None;
                            TailNext::End
                        }
                        Some(Err(e)) => {
                            *guard = None;
                            TailNext::Error(format!("{}", e))
                        }
                        None => {
                            *guard = None;
                            TailNext::End
                        }
                    }
                }
            })
        });
        match outcome {
            TailNext::Event(ev) => Ok(PyRedexEvent::from(ev)),
            TailNext::Error(msg) => Err(RedexError::new_err(format!("tail: {}", msg))),
            TailNext::End => Err(PyStopIteration::new_err(())),
        }
    }

    /// Terminate the iterator. Idempotent; subsequent `__next__`
    /// raises `StopIteration`.
    fn close(&self) {
        self.inner.is_shutdown.store(true, Ordering::Release);
        self.inner.shutdown.notify_waiters();
    }
}

enum TailNext {
    Event(InnerRedexEvent),
    Error(String),
    End,
}

// =========================================================================
// Tasks
// =========================================================================

/// A materialized task record.
#[pyclass(name = "Task", from_py_object)]
#[derive(Clone)]
pub struct PyTask {
    #[pyo3(get)]
    pub id: u64,
    #[pyo3(get)]
    pub title: String,
    #[pyo3(get)]
    pub status: String,
    #[pyo3(get)]
    pub created_ns: u64,
    #[pyo3(get)]
    pub updated_ns: u64,
}

impl From<InnerTask> for PyTask {
    fn from(t: InnerTask) -> Self {
        PyTask {
            id: t.id,
            title: t.title,
            status: task_status_str(t.status).into(),
            created_ns: t.created_ns,
            updated_ns: t.updated_ns,
        }
    }
}

#[pymethods]
impl PyTask {
    fn __repr__(&self) -> String {
        format!(
            "Task(id={}, title={:?}, status={:?}, created_ns={}, updated_ns={})",
            self.id, self.title, self.status, self.created_ns, self.updated_ns
        )
    }
}

/// Typed tasks adapter handle.
#[pyclass(name = "TasksAdapter", from_py_object)]
#[derive(Clone)]
pub struct PyTasksAdapter {
    inner: Arc<InnerTasksAdapter>,
    runtime: Arc<Runtime>,
}

#[pymethods]
impl PyTasksAdapter {
    /// Open the tasks adapter against a Redex manager.
    ///
    /// `persistent`: if `True`, the file writes to disk under the
    /// Redex's configured `persistent_dir` and replays from disk on
    /// reopen. Requires the Redex to have been constructed with
    /// `persistent_dir`; otherwise raises `RuntimeError`.
    #[staticmethod]
    #[pyo3(signature = (redex, origin_hash, persistent = false))]
    fn open(redex: &PyRedex, origin_hash: u64, persistent: bool) -> PyResult<Self> {
        let runtime = make_runtime()?;
        let redex_inner = redex.inner.clone();
        let cfg = cfg_from_persistent(persistent);
        let inner = runtime
            .block_on(async move {
                InnerTasksAdapter::open_with_config(&redex_inner, origin_hash, cfg).await
            })
            .map_err(|e| CortexError::new_err(format!("TasksAdapter open failed: {}", e)))?;
        Ok(Self {
            inner: Arc::new(inner),
            runtime,
        })
    }

    /// Open from a snapshot captured via `snapshot()`. Skips replay
    /// of events `[0, last_seq]` on the underlying file.
    #[staticmethod]
    #[pyo3(signature = (redex, origin_hash, state_bytes, last_seq = None, persistent = false))]
    fn open_from_snapshot(
        redex: &PyRedex,
        origin_hash: u64,
        state_bytes: &[u8],
        last_seq: Option<u64>,
        persistent: bool,
    ) -> PyResult<Self> {
        let runtime = make_runtime()?;
        let redex_inner = redex.inner.clone();
        let cfg = cfg_from_persistent(persistent);
        let bytes = state_bytes.to_vec();
        let inner = runtime
            .block_on(async move {
                InnerTasksAdapter::open_from_snapshot_with_config(
                    &redex_inner,
                    origin_hash,
                    cfg,
                    &bytes,
                    last_seq,
                )
                .await
            })
            .map_err(|e| {
                CortexError::new_err(format!("TasksAdapter open_from_snapshot failed: {}", e))
            })?;
        Ok(Self {
            inner: Arc::new(inner),
            runtime,
        })
    }

    /// Capture a state snapshot. Returns `(state_bytes, last_seq)`.
    /// Persist both together; restore via `open_from_snapshot`.
    fn snapshot(&self) -> PyResult<(Vec<u8>, Option<u64>)> {
        self.inner
            .snapshot()
            .map_err(|e| CortexError::new_err(format!("snapshot failed: {}", e)))
    }

    /// Create a new task. Returns the RedEX sequence.
    fn create(&self, id: u64, title: String, now_ns: u64) -> PyResult<u64> {
        self.inner
            .create(id, title, now_ns)
            .map_err(|e| CortexError::new_err(format!("create failed: {}", e)))
    }

    /// Rename an existing task. No-op at fold time if `id` is unknown.
    fn rename(&self, id: u64, new_title: String, now_ns: u64) -> PyResult<u64> {
        self.inner
            .rename(id, new_title, now_ns)
            .map_err(|e| CortexError::new_err(format!("rename failed: {}", e)))
    }

    /// Mark a task completed.
    fn complete(&self, id: u64, now_ns: u64) -> PyResult<u64> {
        self.inner
            .complete(id, now_ns)
            .map_err(|e| CortexError::new_err(format!("complete failed: {}", e)))
    }

    /// Delete a task.
    fn delete(&self, id: u64) -> PyResult<u64> {
        self.inner
            .delete(id)
            .map_err(|e| CortexError::new_err(format!("delete failed: {}", e)))
    }

    /// Block until every event up through `seq` has been folded.
    /// Releases the GIL for the duration of the wait.
    fn wait_for_seq(&self, py: Python<'_>, seq: u64) {
        let inner = self.inner.clone();
        let runtime = self.runtime.clone();
        py.detach(|| {
            runtime.block_on(async move { inner.wait_for_seq(seq).await });
        });
    }

    /// Close the adapter. Idempotent.
    fn close(&self) -> PyResult<()> {
        self.inner
            .close()
            .map_err(|e| CortexError::new_err(format!("close failed: {}", e)))
    }

    /// True if the fold task is currently running.
    fn is_running(&self) -> bool {
        self.inner.is_running()
    }

    /// Total task count (ignores filters).
    fn count(&self) -> u32 {
        self.inner.state().read().len() as u32
    }

    /// Snapshot query. All filter args are keyword-only.
    #[pyo3(signature = (
        *,
        status=None,
        title_contains=None,
        created_after_ns=None,
        created_before_ns=None,
        updated_after_ns=None,
        updated_before_ns=None,
        order_by=None,
        limit=None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn list_tasks(
        &self,
        status: Option<&str>,
        title_contains: Option<String>,
        created_after_ns: Option<u64>,
        created_before_ns: Option<u64>,
        updated_after_ns: Option<u64>,
        updated_before_ns: Option<u64>,
        order_by: Option<&str>,
        limit: Option<u32>,
    ) -> PyResult<Vec<PyTask>> {
        let state_handle = self.inner.state();
        let state = state_handle.read();
        let mut q = state.query();
        if let Some(s) = status {
            q = q.where_status(parse_task_status(s)?);
        }
        if let Some(s) = title_contains {
            q = q.title_contains(s);
        }
        if let Some(ns) = created_after_ns {
            q = q.created_after(ns);
        }
        if let Some(ns) = created_before_ns {
            q = q.created_before(ns);
        }
        if let Some(ns) = updated_after_ns {
            q = q.updated_after(ns);
        }
        if let Some(ns) = updated_before_ns {
            q = q.updated_before(ns);
        }
        if let Some(o) = order_by {
            q = q.order_by(parse_tasks_order_by(o)?);
        }
        if let Some(l) = limit {
            q = q.limit(l as usize);
        }
        Ok(q.collect().into_iter().map(PyTask::from).collect())
    }

    /// Open a reactive watcher. Returns a Python iterator — use with
    /// `for tasks in adapter.watch_tasks(status='pending'):`. Cancel
    /// iteration early with `iter.close()`.
    #[pyo3(signature = (
        *,
        status=None,
        title_contains=None,
        created_after_ns=None,
        created_before_ns=None,
        updated_after_ns=None,
        updated_before_ns=None,
        order_by=None,
        limit=None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn watch_tasks(
        &self,
        status: Option<&str>,
        title_contains: Option<String>,
        created_after_ns: Option<u64>,
        created_before_ns: Option<u64>,
        updated_after_ns: Option<u64>,
        updated_before_ns: Option<u64>,
        order_by: Option<&str>,
        limit: Option<u32>,
    ) -> PyResult<PyTaskWatchIter> {
        let w = build_task_watcher(
            &self.inner,
            status,
            title_contains,
            created_after_ns,
            created_before_ns,
            updated_after_ns,
            updated_before_ns,
            order_by,
            limit,
        )?;
        // `stream()` requires an active tokio runtime (it spawns a
        // forwarding task); run via block_on to install the context.
        let runtime = self.runtime.clone();
        let stream: BoxStream<'static, Vec<InnerTask>> =
            runtime.block_on(async move { w.stream().boxed() });
        Ok(new_task_watch_iter(stream, self.runtime.clone()))
    }

    /// Atomic "paint what's here now, then react to changes" primitive.
    /// Returns `(snapshot, iter)` in one call; the iterator drops only
    /// leading emissions equal to `snapshot`, so a mutation racing
    /// construction is forwarded through instead of being silently
    /// dropped. Prefer this to `list_tasks` + `watch_tasks` called
    /// separately — those race each other.
    ///
    /// Python usage:
    ///
    ///     snap, it = adapter.snapshot_and_watch_tasks(status='pending')
    ///     render(snap)
    ///     for batch in it:
    ///         render(batch)
    #[pyo3(signature = (
        *,
        status=None,
        title_contains=None,
        created_after_ns=None,
        created_before_ns=None,
        updated_after_ns=None,
        updated_before_ns=None,
        order_by=None,
        limit=None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn snapshot_and_watch_tasks(
        &self,
        status: Option<&str>,
        title_contains: Option<String>,
        created_after_ns: Option<u64>,
        created_before_ns: Option<u64>,
        updated_after_ns: Option<u64>,
        updated_before_ns: Option<u64>,
        order_by: Option<&str>,
        limit: Option<u32>,
    ) -> PyResult<(Vec<PyTask>, PyTaskWatchIter)> {
        let w = build_task_watcher(
            &self.inner,
            status,
            title_contains,
            created_after_ns,
            created_before_ns,
            updated_after_ns,
            updated_before_ns,
            order_by,
            limit,
        )?;
        let adapter = self.inner.clone();
        let runtime = self.runtime.clone();
        let (snapshot, stream) = runtime.block_on(async move { adapter.snapshot_and_watch(w) });
        Ok((
            snapshot.into_iter().map(PyTask::from).collect(),
            new_task_watch_iter(stream, self.runtime.clone()),
        ))
    }
}

#[allow(clippy::too_many_arguments)]
fn build_task_watcher(
    adapter: &InnerTasksAdapter,
    status: Option<&str>,
    title_contains: Option<String>,
    created_after_ns: Option<u64>,
    created_before_ns: Option<u64>,
    updated_after_ns: Option<u64>,
    updated_before_ns: Option<u64>,
    order_by: Option<&str>,
    limit: Option<u32>,
) -> PyResult<::net::adapter::net::cortex::tasks::TasksWatcher> {
    let mut w = adapter.watch();
    if let Some(s) = status {
        w = w.where_status(parse_task_status(s)?);
    }
    if let Some(s) = title_contains {
        w = w.title_contains(s);
    }
    if let Some(ns) = created_after_ns {
        w = w.created_after(ns);
    }
    if let Some(ns) = created_before_ns {
        w = w.created_before(ns);
    }
    if let Some(ns) = updated_after_ns {
        w = w.updated_after(ns);
    }
    if let Some(ns) = updated_before_ns {
        w = w.updated_before(ns);
    }
    if let Some(o) = order_by {
        w = w.order_by(parse_tasks_order_by(o)?);
    }
    if let Some(l) = limit {
        w = w.limit(l as usize);
    }
    Ok(w)
}

fn new_task_watch_iter(
    stream: BoxStream<'static, Vec<InnerTask>>,
    runtime: Arc<Runtime>,
) -> PyTaskWatchIter {
    PyTaskWatchIter {
        inner: Arc::new(TaskWatchIterInner {
            stream: TokioMutex::new(Some(stream)),
            shutdown: Notify::new(),
            is_shutdown: AtomicBool::new(false),
        }),
        runtime,
    }
}

struct TaskWatchIterInner {
    stream: TokioMutex<Option<BoxStream<'static, Vec<InnerTask>>>>,
    shutdown: Notify,
    is_shutdown: AtomicBool,
}

/// Sync Python iterator over a live task filter. `__next__` blocks
/// on the underlying stream and raises `StopIteration` when the
/// iterator is closed or the stream ends.
#[pyclass(name = "TaskWatchIter")]
pub struct PyTaskWatchIter {
    inner: Arc<TaskWatchIterInner>,
    runtime: Arc<Runtime>,
}

#[pymethods]
impl PyTaskWatchIter {
    fn __iter__(slf: PyRef<Self>) -> PyRef<Self> {
        slf
    }

    fn __next__(&self, py: Python<'_>) -> PyResult<Vec<PyTask>> {
        let inner = self.inner.clone();
        let runtime = self.runtime.clone();
        let result = py.detach(move || {
            runtime.block_on(async move {
                if inner.is_shutdown.load(Ordering::Acquire) {
                    return None;
                }
                let mut guard = inner.stream.lock().await;
                let stream = guard.as_mut()?;

                let shutdown_fut = inner.shutdown.notified();
                tokio::pin!(shutdown_fut);
                shutdown_fut.as_mut().enable();

                if inner.is_shutdown.load(Ordering::Acquire) {
                    *guard = None;
                    return None;
                }

                tokio::select! {
                    biased;
                    _ = shutdown_fut => {
                        *guard = None;
                        None
                    }
                    msg = stream.next() => match msg {
                        Some(items) => Some(items),
                        None => {
                            *guard = None;
                            None
                        }
                    }
                }
            })
        });
        match result {
            Some(items) => Ok(items.into_iter().map(PyTask::from).collect()),
            None => Err(PyStopIteration::new_err(())),
        }
    }

    /// Terminate the iterator. Subsequent `__next__` raises
    /// `StopIteration`. Idempotent.
    fn close(&self) {
        self.inner.is_shutdown.store(true, Ordering::Release);
        self.inner.shutdown.notify_waiters();
    }
}

// =========================================================================
// Memories
// =========================================================================

/// A materialized memory record.
#[pyclass(name = "Memory", from_py_object)]
#[derive(Clone)]
pub struct PyMemory {
    #[pyo3(get)]
    pub id: u64,
    #[pyo3(get)]
    pub content: String,
    #[pyo3(get)]
    pub tags: Vec<String>,
    #[pyo3(get)]
    pub source: String,
    #[pyo3(get)]
    pub created_ns: u64,
    #[pyo3(get)]
    pub updated_ns: u64,
    #[pyo3(get)]
    pub pinned: bool,
}

impl From<InnerMemory> for PyMemory {
    fn from(m: InnerMemory) -> Self {
        PyMemory {
            id: m.id,
            content: m.content,
            tags: m.tags,
            source: m.source,
            created_ns: m.created_ns,
            updated_ns: m.updated_ns,
            pinned: m.pinned,
        }
    }
}

#[pymethods]
impl PyMemory {
    fn __repr__(&self) -> String {
        format!(
            "Memory(id={}, content={:?}, tags={:?}, source={:?}, pinned={}, created_ns={}, updated_ns={})",
            self.id,
            self.content,
            self.tags,
            self.source,
            self.pinned,
            self.created_ns,
            self.updated_ns
        )
    }
}

/// Typed memories adapter handle.
#[pyclass(name = "MemoriesAdapter", from_py_object)]
#[derive(Clone)]
pub struct PyMemoriesAdapter {
    inner: Arc<InnerMemoriesAdapter>,
    runtime: Arc<Runtime>,
}

#[pymethods]
impl PyMemoriesAdapter {
    /// Open the memories adapter against a Redex manager. See
    /// `TasksAdapter.open` for `persistent` semantics.
    #[staticmethod]
    #[pyo3(signature = (redex, origin_hash, persistent = false))]
    fn open(redex: &PyRedex, origin_hash: u64, persistent: bool) -> PyResult<Self> {
        let runtime = make_runtime()?;
        let redex_inner = redex.inner.clone();
        let cfg = cfg_from_persistent(persistent);
        let inner = runtime
            .block_on(async move {
                InnerMemoriesAdapter::open_with_config(&redex_inner, origin_hash, cfg).await
            })
            .map_err(|e| CortexError::new_err(format!("MemoriesAdapter open failed: {}", e)))?;
        Ok(Self {
            inner: Arc::new(inner),
            runtime,
        })
    }

    /// Open from a snapshot captured via `snapshot()`.
    #[staticmethod]
    #[pyo3(signature = (redex, origin_hash, state_bytes, last_seq = None, persistent = false))]
    fn open_from_snapshot(
        redex: &PyRedex,
        origin_hash: u64,
        state_bytes: &[u8],
        last_seq: Option<u64>,
        persistent: bool,
    ) -> PyResult<Self> {
        let runtime = make_runtime()?;
        let redex_inner = redex.inner.clone();
        let cfg = cfg_from_persistent(persistent);
        let bytes = state_bytes.to_vec();
        let inner = runtime
            .block_on(async move {
                InnerMemoriesAdapter::open_from_snapshot_with_config(
                    &redex_inner,
                    origin_hash,
                    cfg,
                    &bytes,
                    last_seq,
                )
                .await
            })
            .map_err(|e| {
                CortexError::new_err(format!("MemoriesAdapter open_from_snapshot failed: {}", e))
            })?;
        Ok(Self {
            inner: Arc::new(inner),
            runtime,
        })
    }

    /// Capture a state snapshot for restore via `open_from_snapshot`.
    fn snapshot(&self) -> PyResult<(Vec<u8>, Option<u64>)> {
        self.inner
            .snapshot()
            .map_err(|e| CortexError::new_err(format!("snapshot failed: {}", e)))
    }

    #[pyo3(signature = (id, content, tags, source, now_ns))]
    fn store(
        &self,
        id: u64,
        content: String,
        tags: Vec<String>,
        source: String,
        now_ns: u64,
    ) -> PyResult<u64> {
        self.inner
            .store(id, content, tags, source, now_ns)
            .map_err(|e| CortexError::new_err(format!("store failed: {}", e)))
    }

    fn retag(&self, id: u64, tags: Vec<String>, now_ns: u64) -> PyResult<u64> {
        self.inner
            .retag(id, tags, now_ns)
            .map_err(|e| CortexError::new_err(format!("retag failed: {}", e)))
    }

    fn pin(&self, id: u64, now_ns: u64) -> PyResult<u64> {
        self.inner
            .pin(id, now_ns)
            .map_err(|e| CortexError::new_err(format!("pin failed: {}", e)))
    }

    fn unpin(&self, id: u64, now_ns: u64) -> PyResult<u64> {
        self.inner
            .unpin(id, now_ns)
            .map_err(|e| CortexError::new_err(format!("unpin failed: {}", e)))
    }

    fn delete(&self, id: u64) -> PyResult<u64> {
        self.inner
            .delete(id)
            .map_err(|e| CortexError::new_err(format!("delete failed: {}", e)))
    }

    fn wait_for_seq(&self, py: Python<'_>, seq: u64) {
        let inner = self.inner.clone();
        let runtime = self.runtime.clone();
        py.detach(|| {
            runtime.block_on(async move { inner.wait_for_seq(seq).await });
        });
    }

    fn close(&self) -> PyResult<()> {
        self.inner
            .close()
            .map_err(|e| CortexError::new_err(format!("close failed: {}", e)))
    }

    fn is_running(&self) -> bool {
        self.inner.is_running()
    }

    fn count(&self) -> u32 {
        self.inner.state().read().len() as u32
    }

    #[pyo3(signature = (
        *,
        source=None,
        content_contains=None,
        tag=None,
        any_tag=None,
        all_tags=None,
        pinned=None,
        created_after_ns=None,
        created_before_ns=None,
        updated_after_ns=None,
        updated_before_ns=None,
        order_by=None,
        limit=None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn list_memories(
        &self,
        source: Option<String>,
        content_contains: Option<String>,
        tag: Option<String>,
        any_tag: Option<Vec<String>>,
        all_tags: Option<Vec<String>>,
        pinned: Option<bool>,
        created_after_ns: Option<u64>,
        created_before_ns: Option<u64>,
        updated_after_ns: Option<u64>,
        updated_before_ns: Option<u64>,
        order_by: Option<&str>,
        limit: Option<u32>,
    ) -> PyResult<Vec<PyMemory>> {
        let state_handle = self.inner.state();
        let state = state_handle.read();
        let mut q = state.query();
        if let Some(s) = source {
            q = q.where_source(s);
        }
        if let Some(s) = content_contains {
            q = q.content_contains(s);
        }
        if let Some(t) = tag {
            q = q.where_tag(t);
        }
        if let Some(tags) = any_tag {
            q = q.where_any_tag(tags);
        }
        if let Some(tags) = all_tags {
            q = q.where_all_tags(tags);
        }
        if let Some(p) = pinned {
            q = q.where_pinned(p);
        }
        if let Some(ns) = created_after_ns {
            q = q.created_after(ns);
        }
        if let Some(ns) = created_before_ns {
            q = q.created_before(ns);
        }
        if let Some(ns) = updated_after_ns {
            q = q.updated_after(ns);
        }
        if let Some(ns) = updated_before_ns {
            q = q.updated_before(ns);
        }
        if let Some(o) = order_by {
            q = q.order_by(parse_memories_order_by(o)?);
        }
        if let Some(l) = limit {
            q = q.limit(l as usize);
        }
        Ok(q.collect().into_iter().map(PyMemory::from).collect())
    }

    #[pyo3(signature = (
        *,
        source=None,
        content_contains=None,
        tag=None,
        any_tag=None,
        all_tags=None,
        pinned=None,
        created_after_ns=None,
        created_before_ns=None,
        updated_after_ns=None,
        updated_before_ns=None,
        order_by=None,
        limit=None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn watch_memories(
        &self,
        source: Option<String>,
        content_contains: Option<String>,
        tag: Option<String>,
        any_tag: Option<Vec<String>>,
        all_tags: Option<Vec<String>>,
        pinned: Option<bool>,
        created_after_ns: Option<u64>,
        created_before_ns: Option<u64>,
        updated_after_ns: Option<u64>,
        updated_before_ns: Option<u64>,
        order_by: Option<&str>,
        limit: Option<u32>,
    ) -> PyResult<PyMemoryWatchIter> {
        let w = build_memory_watcher(
            &self.inner,
            source,
            content_contains,
            tag,
            any_tag,
            all_tags,
            pinned,
            created_after_ns,
            created_before_ns,
            updated_after_ns,
            updated_before_ns,
            order_by,
            limit,
        )?;
        let runtime = self.runtime.clone();
        let stream: BoxStream<'static, Vec<InnerMemory>> =
            runtime.block_on(async move { w.stream().boxed() });
        Ok(new_memory_watch_iter(stream, self.runtime.clone()))
    }

    /// Atomic snapshot + watch. Mirrors
    /// `TasksAdapter.snapshot_and_watch_tasks`.
    #[pyo3(signature = (
        *,
        source=None,
        content_contains=None,
        tag=None,
        any_tag=None,
        all_tags=None,
        pinned=None,
        created_after_ns=None,
        created_before_ns=None,
        updated_after_ns=None,
        updated_before_ns=None,
        order_by=None,
        limit=None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn snapshot_and_watch_memories(
        &self,
        source: Option<String>,
        content_contains: Option<String>,
        tag: Option<String>,
        any_tag: Option<Vec<String>>,
        all_tags: Option<Vec<String>>,
        pinned: Option<bool>,
        created_after_ns: Option<u64>,
        created_before_ns: Option<u64>,
        updated_after_ns: Option<u64>,
        updated_before_ns: Option<u64>,
        order_by: Option<&str>,
        limit: Option<u32>,
    ) -> PyResult<(Vec<PyMemory>, PyMemoryWatchIter)> {
        let w = build_memory_watcher(
            &self.inner,
            source,
            content_contains,
            tag,
            any_tag,
            all_tags,
            pinned,
            created_after_ns,
            created_before_ns,
            updated_after_ns,
            updated_before_ns,
            order_by,
            limit,
        )?;
        let adapter = self.inner.clone();
        let runtime = self.runtime.clone();
        let (snapshot, stream) = runtime.block_on(async move { adapter.snapshot_and_watch(w) });
        Ok((
            snapshot.into_iter().map(PyMemory::from).collect(),
            new_memory_watch_iter(stream, self.runtime.clone()),
        ))
    }
}

#[allow(clippy::too_many_arguments)]
fn build_memory_watcher(
    adapter: &InnerMemoriesAdapter,
    source: Option<String>,
    content_contains: Option<String>,
    tag: Option<String>,
    any_tag: Option<Vec<String>>,
    all_tags: Option<Vec<String>>,
    pinned: Option<bool>,
    created_after_ns: Option<u64>,
    created_before_ns: Option<u64>,
    updated_after_ns: Option<u64>,
    updated_before_ns: Option<u64>,
    order_by: Option<&str>,
    limit: Option<u32>,
) -> PyResult<::net::adapter::net::cortex::memories::MemoriesWatcher> {
    let mut w = adapter.watch();
    if let Some(s) = source {
        w = w.where_source(s);
    }
    if let Some(s) = content_contains {
        w = w.content_contains(s);
    }
    if let Some(t) = tag {
        w = w.where_tag(t);
    }
    if let Some(tags) = any_tag {
        w = w.where_any_tag(tags);
    }
    if let Some(tags) = all_tags {
        w = w.where_all_tags(tags);
    }
    if let Some(p) = pinned {
        w = w.where_pinned(p);
    }
    if let Some(ns) = created_after_ns {
        w = w.created_after(ns);
    }
    if let Some(ns) = created_before_ns {
        w = w.created_before(ns);
    }
    if let Some(ns) = updated_after_ns {
        w = w.updated_after(ns);
    }
    if let Some(ns) = updated_before_ns {
        w = w.updated_before(ns);
    }
    if let Some(o) = order_by {
        w = w.order_by(parse_memories_order_by(o)?);
    }
    if let Some(l) = limit {
        w = w.limit(l as usize);
    }
    Ok(w)
}

fn new_memory_watch_iter(
    stream: BoxStream<'static, Vec<InnerMemory>>,
    runtime: Arc<Runtime>,
) -> PyMemoryWatchIter {
    PyMemoryWatchIter {
        inner: Arc::new(MemoryWatchIterInner {
            stream: TokioMutex::new(Some(stream)),
            shutdown: Notify::new(),
            is_shutdown: AtomicBool::new(false),
        }),
        runtime,
    }
}

struct MemoryWatchIterInner {
    stream: TokioMutex<Option<BoxStream<'static, Vec<InnerMemory>>>>,
    shutdown: Notify,
    is_shutdown: AtomicBool,
}

/// Sync Python iterator over a live memory filter.
#[pyclass(name = "MemoryWatchIter")]
pub struct PyMemoryWatchIter {
    inner: Arc<MemoryWatchIterInner>,
    runtime: Arc<Runtime>,
}

#[pymethods]
impl PyMemoryWatchIter {
    fn __iter__(slf: PyRef<Self>) -> PyRef<Self> {
        slf
    }

    fn __next__(&self, py: Python<'_>) -> PyResult<Vec<PyMemory>> {
        let inner = self.inner.clone();
        let runtime = self.runtime.clone();
        let result = py.detach(move || {
            runtime.block_on(async move {
                if inner.is_shutdown.load(Ordering::Acquire) {
                    return None;
                }
                let mut guard = inner.stream.lock().await;
                let stream = guard.as_mut()?;

                let shutdown_fut = inner.shutdown.notified();
                tokio::pin!(shutdown_fut);
                shutdown_fut.as_mut().enable();

                if inner.is_shutdown.load(Ordering::Acquire) {
                    *guard = None;
                    return None;
                }

                tokio::select! {
                    biased;
                    _ = shutdown_fut => {
                        *guard = None;
                        None
                    }
                    msg = stream.next() => match msg {
                        Some(items) => Some(items),
                        None => {
                            *guard = None;
                            None
                        }
                    }
                }
            })
        });
        match result {
            Some(items) => Ok(items.into_iter().map(PyMemory::from).collect()),
            None => Err(PyStopIteration::new_err(())),
        }
    }

    fn close(&self) {
        self.inner.is_shutdown.store(true, Ordering::Release);
        self.inner.shutdown.notify_waiters();
    }
}

// =========================================================================
// NetDB — unified query façade over tasks + memories
// =========================================================================

use ::net::adapter::net::netdb::NetDbSnapshot as InnerNetDbSnapshot;

/// Unified NetDB handle bundling `TasksAdapter` + `MemoriesAdapter`.
///
/// Construct via [`PyNetDb::open`] / [`PyNetDb::open_from_snapshot`].
/// Access per-model adapters via the `tasks` / `memories` properties.
///
/// For raw event / stream access, drop down to the underlying
/// adapters (or RedEX directly).
#[pyclass(name = "NetDb")]
pub struct PyNetDb {
    tasks: Option<PyTasksAdapter>,
    memories: Option<PyMemoriesAdapter>,
}

#[pymethods]
impl PyNetDb {
    /// Open a NetDB with the requested models. Each enabled model
    /// spawns its own CortEX fold task on its own tokio runtime.
    #[staticmethod]
    #[pyo3(signature = (
        *,
        origin_hash,
        persistent_dir = None,
        persistent = false,
        with_tasks = false,
        with_memories = false,
    ))]
    fn open(
        origin_hash: u64,
        persistent_dir: Option<String>,
        persistent: bool,
        with_tasks: bool,
        with_memories: bool,
    ) -> PyResult<Self> {
        let redex = match &persistent_dir {
            Some(dir) => PyRedex {
                inner: Arc::new(InnerRedex::new().with_persistent_dir(dir)),
                persistent_dir: Some(dir.clone()),
            },
            None => PyRedex {
                inner: Arc::new(InnerRedex::new()),
                persistent_dir: None,
            },
        };

        let tasks = if with_tasks {
            Some(PyTasksAdapter::open(&redex, origin_hash, persistent)?)
        } else {
            None
        };
        let memories = if with_memories {
            Some(PyMemoriesAdapter::open(&redex, origin_hash, persistent)?)
        } else {
            None
        };

        Ok(Self { tasks, memories })
    }

    /// Open a NetDB and restore each enabled model's state from the
    /// bundle. Models whose bundle entry is `None` are opened from
    /// scratch (equivalent to `open` for that model).
    #[staticmethod]
    #[pyo3(signature = (
        bundle,
        *,
        origin_hash,
        persistent_dir = None,
        persistent = false,
        with_tasks = false,
        with_memories = false,
    ))]
    fn open_from_snapshot(
        bundle: &[u8],
        origin_hash: u64,
        persistent_dir: Option<String>,
        persistent: bool,
        with_tasks: bool,
        with_memories: bool,
    ) -> PyResult<Self> {
        let snapshot = InnerNetDbSnapshot::decode(bundle)
            .map_err(|e| NetDbError::new_err(format!("decode bundle: {}", e)))?;

        let redex = match &persistent_dir {
            Some(dir) => PyRedex {
                inner: Arc::new(InnerRedex::new().with_persistent_dir(dir)),
                persistent_dir: Some(dir.clone()),
            },
            None => PyRedex {
                inner: Arc::new(InnerRedex::new()),
                persistent_dir: None,
            },
        };

        let tasks = if with_tasks {
            match snapshot.tasks {
                Some((bytes, last_seq)) => Some(PyTasksAdapter::open_from_snapshot(
                    &redex,
                    origin_hash,
                    &bytes,
                    last_seq,
                    persistent,
                )?),
                None => Some(PyTasksAdapter::open(&redex, origin_hash, persistent)?),
            }
        } else {
            None
        };

        let memories = if with_memories {
            match snapshot.memories {
                Some((bytes, last_seq)) => Some(PyMemoriesAdapter::open_from_snapshot(
                    &redex,
                    origin_hash,
                    &bytes,
                    last_seq,
                    persistent,
                )?),
                None => Some(PyMemoriesAdapter::open(&redex, origin_hash, persistent)?),
            }
        } else {
            None
        };

        Ok(Self { tasks, memories })
    }

    /// The tasks adapter, or `None` if tasks weren't enabled.
    #[getter]
    fn tasks(&self) -> Option<PyTasksAdapter> {
        self.tasks.clone()
    }

    /// The memories adapter, or `None` if memories weren't enabled.
    #[getter]
    fn memories(&self) -> Option<PyMemoriesAdapter> {
        self.memories.clone()
    }

    /// Snapshot every enabled model into one opaque bincode blob.
    /// Persist the returned bytes; restore via `open_from_snapshot`.
    fn snapshot(&self) -> PyResult<Vec<u8>> {
        let tasks = match &self.tasks {
            Some(t) => Some(
                t.inner
                    .snapshot()
                    .map_err(|e| CortexError::new_err(format!("snapshot tasks: {}", e)))?,
            ),
            None => None,
        };
        let memories = match &self.memories {
            Some(m) => Some(
                m.inner
                    .snapshot()
                    .map_err(|e| CortexError::new_err(format!("snapshot memories: {}", e)))?,
            ),
            None => None,
        };
        let snap = InnerNetDbSnapshot { tasks, memories };
        snap.encode()
            .map_err(|e| NetDbError::new_err(format!("encode bundle: {}", e)))
    }

    /// Close every enabled adapter. Idempotent.
    fn close(&self) -> PyResult<()> {
        if let Some(t) = &self.tasks {
            t.close()?;
        }
        if let Some(m) = &self.memories {
            m.close()?;
        }
        Ok(())
    }

    fn __repr__(&self) -> String {
        format!(
            "NetDb(tasks={}, memories={})",
            self.tasks.is_some(),
            self.memories.is_some()
        )
    }
}
