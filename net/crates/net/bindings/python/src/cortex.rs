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
use ::net::adapter::net::cortex::WaitForTokenError as InnerWaitForTokenError;
use ::net::adapter::net::redex::{
    FsyncPolicy as InnerFsyncPolicy, PlacementStrategy as InnerPlacementStrategy,
    Redex as InnerRedex, RedexError as InnerRedexError, RedexEvent as InnerRedexEvent,
    RedexFile as InnerRedexFile, RedexFileConfig, ReplicationConfig as InnerReplicationConfig,
    UnderCapacity as InnerUnderCapacity, WriteToken as InnerWriteToken,
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
// WriteToken — typed handle to a specific write, returned by ingest
// paths and consumed by read-your-writes wait primitives.
// =========================================================================

/// Address of a single write on a specific origin's chain. Pair it
/// with a typed adapter's `wait_for_token(token, deadline_ms=...)`
/// to make sure that adapter's fold has caught up to the write
/// before reading state.
#[pyclass(name = "WriteToken", frozen, eq, hash, from_py_object)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PyWriteToken {
    inner: InnerWriteToken,
}

#[pymethods]
impl PyWriteToken {
    #[new]
    fn new(origin_hash: u64, seq: u64) -> Self {
        Self {
            inner: InnerWriteToken::new(origin_hash, seq),
        }
    }

    #[getter]
    fn origin_hash(&self) -> u64 {
        self.inner.origin_hash
    }

    #[getter]
    fn seq(&self) -> u64 {
        self.inner.seq
    }

    /// Parse a token from its `<16-hex-origin>:<seq>` string form.
    #[staticmethod]
    fn from_string(s: &str) -> PyResult<Self> {
        s.parse::<InnerWriteToken>()
            .map(|inner| Self { inner })
            .map_err(|e| PyValueError::new_err(format!("invalid WriteToken: {}", e)))
    }

    fn __str__(&self) -> String {
        self.inner.to_string()
    }

    fn __repr__(&self) -> String {
        format!(
            "WriteToken(origin_hash=0x{:x}, seq={})",
            self.inner.origin_hash, self.inner.seq
        )
    }
}

impl PyWriteToken {
    fn as_inner(&self) -> InnerWriteToken {
        self.inner
    }
}

/// Lift a substrate `WaitForTokenError` to a `CortexError`. The
/// Python surface intentionally collapses all three variants into
/// one exception class — distinguishing them in app code requires
/// inspecting the message — because the failure responses are
/// usually the same (retry with fresh deadline / shed load /
/// fix the token).
fn map_wait_for_token_err(e: InnerWaitForTokenError) -> PyErr {
    CortexError::new_err(format!("wait_for_token: {}", e))
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

impl PyRedex {
    /// Crate-internal accessor for the underlying `Redex` Arc.
    /// Lets sibling binding modules (e.g. `blob::PyMeshBlobAdapter`)
    /// wire a substrate-owned blob adapter against the same handle
    /// without forcing the operator to manage two parallel Redex
    /// instances. Not exposed to Python.
    ///
    /// Gated on `dataforts` because the only caller — the Python
    /// `MeshBlobAdapter` binding — lives behind that feature.
    /// Without the gate, a no-feature build trips
    /// `-D dead-code` (the method has no consumer).
    #[cfg(feature = "dataforts")]
    pub(crate) fn inner_arc(&self) -> Arc<InnerRedex> {
        self.inner.clone()
    }
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
    ///
    /// Cross-node replication: pass `replication=True` to opt the
    /// channel in. The remaining `replication_*` kwargs tune the
    /// replication policy (factor, heartbeat cadence, placement,
    /// disk-pressure policy, bandwidth budget). The owning `Redex`
    /// must have called `enable_replication(mesh)` first; otherwise
    /// the call raises `RedexError`. See `CONFIG_REPLICATION.md`
    /// for the full operator surface.
    #[pyo3(signature = (
        name,
        *,
        persistent = false,
        fsync_every_n = None,
        fsync_interval_ms = None,
        retention_max_events = None,
        retention_max_bytes = None,
        retention_max_age_ms = None,
        replication = false,
        replication_factor = None,
        replication_heartbeat_ms = None,
        replication_placement = None,
        replication_pinned_nodes = None,
        replication_leader_pinned = None,
        replication_on_under_capacity = None,
        replication_budget_fraction = None,
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
        replication: bool,
        replication_factor: Option<u32>,
        replication_heartbeat_ms: Option<u64>,
        replication_placement: Option<String>,
        replication_pinned_nodes: Option<Vec<u64>>,
        replication_leader_pinned: Option<u64>,
        replication_on_under_capacity: Option<String>,
        replication_budget_fraction: Option<f64>,
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
        // R-6: if `replication=False` but any other replication
        // kwarg is set, fail loud rather than silently opening
        // a single-node file. Operators who typo'd the boolean
        // (or forgot it entirely) get a clear error instead of
        // their carefully-tuned settings being silently ignored.
        if !replication {
            let stray = [
                ("replication_factor", replication_factor.is_some()),
                (
                    "replication_heartbeat_ms",
                    replication_heartbeat_ms.is_some(),
                ),
                ("replication_placement", replication_placement.is_some()),
                (
                    "replication_pinned_nodes",
                    replication_pinned_nodes.is_some(),
                ),
                (
                    "replication_leader_pinned",
                    replication_leader_pinned.is_some(),
                ),
                (
                    "replication_on_under_capacity",
                    replication_on_under_capacity.is_some(),
                ),
                (
                    "replication_budget_fraction",
                    replication_budget_fraction.is_some(),
                ),
            ];
            let first_set = stray.iter().find(|(_, set)| *set).map(|(n, _)| *n);
            if let Some(name) = first_set {
                return Err(RedexError::new_err(format!(
                    "replication: {name} specified without replication=True; \
                     set replication=True to opt the channel in, or remove the kwarg"
                )));
            }
        }
        if replication {
            cfg.replication = Some(build_replication_config(
                replication_factor,
                replication_heartbeat_ms,
                replication_placement,
                replication_pinned_nodes,
                replication_leader_pinned,
                replication_on_under_capacity,
                replication_budget_fraction,
            )?);
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

    /// Install cross-node replication wiring rooted at `mesh`. After
    /// this returns, `open_file` calls with `replication=True` spawn
    /// per-channel replication runtimes. Idempotent — repeated calls
    /// leave the existing wiring in place.
    ///
    /// Gated on the `net` feature: replication requires a `NetMesh`,
    /// which only ships when `net` is enabled.
    ///
    /// See `CONFIG_REPLICATION.md` for the full operator surface.
    #[cfg(feature = "net")]
    fn enable_replication(&self, mesh: &crate::mesh_bindings::NetMesh) -> PyResult<()> {
        let arc = mesh.node_arc_clone()?;
        self.inner.enable_replication(arc);
        Ok(())
    }

    /// R-7: when this wheel is built without the `net` feature
    /// (cortex-only build), surface a typed RedexError naming the
    /// missing feature instead of an `AttributeError` from PyO3.
    /// Pyo3 takes the first matching `#[pymethods]` arm; the
    /// `cfg(not(feature = "net"))` variant only exists in the
    /// degraded build and accepts a `PyObject` so the call site
    /// (`r.enable_replication(some_mesh)`) doesn't fail with a
    /// type error before the typed error fires.
    #[cfg(not(feature = "net"))]
    #[pyo3(signature = (_mesh = None))]
    fn enable_replication(&self, _mesh: Option<Py<PyAny>>) -> PyResult<()> {
        Err(RedexError::new_err(
            "redex: enable_replication requires the `net` feature; \
             rebuild with --features net",
        ))
    }

    /// Count of per-channel replication runtimes currently registered
    /// on this manager. `0` when replication isn't enabled. Useful
    /// for tests and operator observability.
    fn replication_runtime_count(&self) -> u32 {
        self.inner.replication_runtime_count() as u32
    }

    /// Render the replication metrics as Prometheus text. Returns
    /// the empty string when replication isn't enabled — convenient
    /// for piping into an HTTP scrape endpoint without branching.
    ///
    /// Covers the seven per-channel shapes from
    /// `CONFIG_REPLICATION.md`: `*_lag_seconds{role}`,
    /// `*_sync_bytes_total`, `*_leader_changes_total`,
    /// `*_under_capacity_total`, `*_skip_ahead_total`,
    /// `*_election_thrash_total`, `*_witness_withdrawals_total`.
    fn replication_prometheus_text(&self) -> String {
        self.inner.replication_prometheus_text()
    }

    /// Install greedy-LRU dataforts wiring rooted at `mesh`. The
    /// runtime opens per-channel cache files against this Redex
    /// and announces chains via the mesh's `ChainTagSink` impl.
    ///
    /// Locked defaults from `DATAFORTS_PLAN.md` § Phase 1: 100 MiB
    /// per channel, 10 GiB total, 0.25 NIC fraction,
    /// `AnyOfLocalCapabilities` intent, `SoftPreference`
    /// colocation, 200 ms proximity. Override via the kwargs.
    ///
    /// Idempotent — a second call with greedy already enabled is
    /// a no-op (use `disable_greedy_dataforts` + re-enable to
    /// reconfigure).
    ///
    /// Raises `RedexError` on invalid config (range violations on
    /// caps, bandwidth fraction, proximity).
    #[cfg(all(feature = "net", feature = "dataforts"))]
    #[pyo3(signature = (
        mesh,
        *,
        scopes = None,
        proximity_max_rtt_ms = None,
        per_channel_cap_bytes = None,
        total_cap_bytes = None,
        bandwidth_budget_fraction = None,
        nic_peak_bytes_per_s = None,
        observer_inflight_cap = None,
        intent_match = None,
        colocation_policy = None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn enable_greedy_dataforts(
        &self,
        mesh: &crate::mesh_bindings::NetMesh,
        scopes: Option<Vec<String>>,
        proximity_max_rtt_ms: Option<u64>,
        per_channel_cap_bytes: Option<u64>,
        total_cap_bytes: Option<u64>,
        bandwidth_budget_fraction: Option<f64>,
        nic_peak_bytes_per_s: Option<u64>,
        observer_inflight_cap: Option<usize>,
        intent_match: Option<String>,
        colocation_policy: Option<String>,
    ) -> PyResult<()> {
        use net::adapter::net::dataforts::{
            ColocationPolicy, GreedyConfig, IntentMatchPolicy, ScopeLabel,
        };
        let mut cfg = GreedyConfig::new();
        if let Some(s) = scopes {
            cfg = cfg.with_scopes(s.into_iter().map(ScopeLabel::new).collect());
        }
        if let Some(ms) = proximity_max_rtt_ms {
            cfg = cfg.with_proximity_max_rtt(std::time::Duration::from_millis(ms));
        }
        if let Some(bytes) = per_channel_cap_bytes {
            cfg = cfg.with_per_channel_cap_bytes(bytes);
        }
        if let Some(bytes) = total_cap_bytes {
            cfg = cfg.with_total_cap_bytes(bytes);
        }
        if let Some(f) = bandwidth_budget_fraction {
            cfg = cfg.with_bandwidth_budget_fraction(f as f32);
        }
        if let Some(peak) = nic_peak_bytes_per_s {
            cfg = cfg.with_nic_peak_bytes_per_s(Some(peak));
        }
        if let Some(cap) = observer_inflight_cap {
            cfg = cfg.with_observer_inflight_cap(cap);
        }
        if let Some(policy) = intent_match {
            let parsed = match policy.as_str() {
                "disabled" => IntentMatchPolicy::Disabled,
                "any_of_local_capabilities" => IntentMatchPolicy::AnyOfLocalCapabilities,
                "strict" => IntentMatchPolicy::Strict,
                other => {
                    return Err(RedexError::new_err(format!(
                        "greedy intent_match {other:?} unknown (expected disabled, any_of_local_capabilities, strict)"
                    )))
                }
            };
            cfg = cfg.with_intent_match(parsed);
        }
        if let Some(policy) = colocation_policy {
            let parsed = match policy.as_str() {
                "ignore" => ColocationPolicy::Ignore,
                "soft_preference" => ColocationPolicy::SoftPreference,
                "strict_required" => ColocationPolicy::StrictRequired,
                other => {
                    return Err(RedexError::new_err(format!(
                        "greedy colocation_policy {other:?} unknown (expected ignore, soft_preference, strict_required)"
                    )))
                }
            };
            cfg = cfg.with_colocation_policy(parsed);
        }
        let arc = mesh.node_arc_clone()?;
        // Local-caps + intent-registry default to empty / substrate
        // defaults respectively. Application code refreshes via
        // `greedy_set_local_caps` and `greedy_register_intent`
        // when fuller surfaces land.
        let local_caps =
            std::sync::Arc::new(net::adapter::net::behavior::capability::CapabilitySet::default());
        let registry = net::adapter::net::behavior::placement::IntentRegistry::defaults();
        self.inner
            .enable_greedy_dataforts(arc, cfg, local_caps, registry)
            .map_err(|e| RedexError::new_err(format!("greedy config invalid: {}", e)))
    }

    /// `cfg(not net)` stub. Mirrors the
    /// `enable_replication` cross-feature surface so the same
    /// call site doesn't TypeError before the feature-required
    /// message surfaces.
    #[cfg(not(all(feature = "net", feature = "dataforts")))]
    #[pyo3(signature = (_mesh = None, **_kwargs))]
    fn enable_greedy_dataforts(
        &self,
        _mesh: Option<Py<PyAny>>,
        _kwargs: Option<Py<PyAny>>,
    ) -> PyResult<()> {
        Err(RedexError::new_err(
            "redex: enable_greedy_dataforts requires the `dataforts` feature; \
             rebuild with --features dataforts",
        ))
    }

    /// Un-install the greedy wiring. Idempotent — no-op when
    /// greedy isn't enabled.
    #[cfg(feature = "dataforts")]
    fn disable_greedy_dataforts(&self) {
        self.inner.disable_greedy_dataforts();
    }

    /// Number of channels currently in the greedy cache. `0` when
    /// greedy isn't enabled.
    #[cfg(feature = "dataforts")]
    fn greedy_cached_channel_count(&self) -> u32 {
        self.inner
            .greedy_runtime()
            .map(|r| r.cached_channel_count() as u32)
            .unwrap_or(0)
    }

    /// Render the greedy metrics as Prometheus text. Returns the
    /// empty string when greedy isn't enabled.
    ///
    /// Covers per-channel `dataforts_greedy_cache_hits_total`,
    /// `_serve_count_total`, `_evictions_total`,
    /// `_bytes_resident`, plus the cluster-wide
    /// `_admit_rejected_total{reason=...}` and
    /// `_io_budget_used_bytes`.
    #[cfg(feature = "dataforts")]
    fn greedy_prometheus_text(&self) -> String {
        self.inner
            .greedy_runtime()
            .map(|r| r.metrics().snapshot().prometheus_text())
            .unwrap_or_default()
    }

    /// Install data-gravity heat-counter emission on the
    /// already-installed greedy runtime. Validates the policy,
    /// installs it on the runtime, and spawns a tokio task that
    /// fires `gravity_tick().await` on `tick_interval_ms` cadence.
    ///
    /// Requires `enable_greedy_dataforts(mesh)` first — raises
    /// `RedexError` if greedy isn't installed.
    ///
    /// Locked Phase-4 defaults: emit_threshold_ratio = 2.0,
    /// decay_half_life = 30 min. Override via kwargs.
    ///
    /// Idempotent — a second call replaces the prior policy and
    /// restarts the tick task. The heat registry resets on each
    /// re-enable.
    #[cfg(all(feature = "net", feature = "dataforts"))]
    #[pyo3(signature = (
        mesh,
        *,
        tick_interval_ms = 500,
        enabled = true,
        emit_threshold_ratio = None,
        decay_half_life_secs = None,
        normalization_reference_rate = None,
    ))]
    fn enable_gravity_for_greedy(
        &self,
        mesh: &crate::mesh_bindings::NetMesh,
        tick_interval_ms: u64,
        enabled: bool,
        emit_threshold_ratio: Option<f64>,
        decay_half_life_secs: Option<u64>,
        normalization_reference_rate: Option<f64>,
    ) -> PyResult<()> {
        use net::adapter::net::dataforts::DataGravityPolicy;
        let mut policy = DataGravityPolicy::new().with_enabled(enabled);
        if let Some(r) = emit_threshold_ratio {
            policy = policy.with_emit_threshold_ratio(r as f32);
        }
        if let Some(secs) = decay_half_life_secs {
            policy = policy.with_decay_half_life(std::time::Duration::from_secs(secs));
        }
        if let Some(reference) = normalization_reference_rate {
            policy = policy.with_normalization_reference_rate(reference as f32);
        }
        let arc = mesh.node_arc_clone()?;
        self.inner
            .enable_gravity_for_greedy(
                arc,
                policy,
                std::time::Duration::from_millis(tick_interval_ms),
            )
            .map_err(|e| RedexError::new_err(format!("gravity invalid: {}", e)))
    }

    /// Stub when `dataforts` isn't compiled in. Returns a
    /// typed RedexError naming the missing feature.
    #[cfg(not(all(feature = "net", feature = "dataforts")))]
    #[pyo3(signature = (_mesh = None, **_kwargs))]
    fn enable_gravity_for_greedy(
        &self,
        _mesh: Option<Py<PyAny>>,
        _kwargs: Option<Py<PyAny>>,
    ) -> PyResult<()> {
        Err(RedexError::new_err(
            "redex: enable_gravity_for_greedy requires the `dataforts` feature; \
             rebuild with --features dataforts",
        ))
    }

    /// Uninstall the gravity layer. Idempotent — no-op when not
    /// enabled. Greedy itself stays running.
    #[cfg(feature = "dataforts")]
    fn disable_gravity_for_greedy(&self) {
        self.inner.disable_gravity_for_greedy();
    }
}

fn build_replication_config(
    factor: Option<u32>,
    heartbeat_ms: Option<u64>,
    placement: Option<String>,
    pinned_nodes: Option<Vec<u64>>,
    leader_pinned: Option<u64>,
    on_under_capacity: Option<String>,
    budget_fraction: Option<f64>,
) -> PyResult<InnerReplicationConfig> {
    let mut out = InnerReplicationConfig::new();
    if let Some(f) = factor {
        if f > u8::MAX as u32 {
            return Err(RedexError::new_err(format!(
                "replication_factor must fit in u8 (got {f})"
            )));
        }
        out = out.with_factor(f as u8);
    }
    if let Some(hb) = heartbeat_ms {
        out = out.with_heartbeat_ms(hb);
    }
    // R-26: accept ONLY the snake-case canonical form (Python
    // convention). The kebab-case form is rejected loudly so
    // error messages stay unambiguous. Operators on the JS side
    // use kebab-case via the Node binding; that's the canonical
    // there.
    out = out.with_placement(match placement.as_deref() {
        None | Some("standard") => InnerPlacementStrategy::Standard,
        Some("colocation_strict") => InnerPlacementStrategy::ColocationStrict,
        Some("pinned") => {
            let nodes = pinned_nodes.ok_or_else(|| {
                RedexError::new_err(
                    "replication_pinned_nodes required when replication_placement = 'pinned'",
                )
            })?;
            // R-27: reject empty pinned_nodes at the binding
            // layer for a clearer error.
            if nodes.is_empty() {
                return Err(RedexError::new_err(
                    "replication_pinned_nodes must be non-empty when replication_placement = 'pinned'",
                ));
            }
            // R-28: cross-check leader_pinned membership.
            if let Some(lp) = leader_pinned {
                if !nodes.contains(&lp) {
                    return Err(RedexError::new_err(format!(
                        "replication_leader_pinned {lp} is not in replication_pinned_nodes"
                    )));
                }
            }
            InnerPlacementStrategy::Pinned(nodes)
        }
        Some(other) => {
            return Err(RedexError::new_err(format!(
                "unknown replication_placement {other:?}; expected 'standard', 'pinned', or 'colocation_strict'"
            )));
        }
    });
    if let Some(leader) = leader_pinned {
        out = out.with_leader_pinned(Some(leader));
    }
    out = out.with_on_under_capacity(match on_under_capacity.as_deref() {
        None | Some("withdraw") => InnerUnderCapacity::Withdraw,
        Some("evict_oldest") => InnerUnderCapacity::EvictOldest,
        Some(other) => {
            return Err(RedexError::new_err(format!(
                "unknown replication_on_under_capacity {other:?}; expected 'withdraw' or 'evict_oldest'"
            )));
        }
    });
    if let Some(fr) = budget_fraction {
        out = out.with_replication_budget_fraction(fr as f32);
    }
    out.validate()
        .map_err(|e| RedexError::new_err(format!("replication config invalid: {e}")))?;
    Ok(out)
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
///
/// Async equivalent: :class:`AsyncRedexFile` — awaitable `tail()`
/// returning an `AsyncRedexTailIter`.
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
    fn tail(&self, py: Python<'_>, from_seq: u64) -> PyRedexTailIter {
        use futures::StreamExt;
        let adapter = self.inner.clone();
        let runtime = self.runtime.clone();
        let stream =
            py.detach(move || runtime.block_on(async move { adapter.tail(from_seq).boxed() }));
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
///
/// Async equivalent: :class:`AsyncRedexTailIter` — PEP 525 async
/// iterator with the same yield shape.
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
// AsyncRedexFile + AsyncRedexTailIter — T2-C2.
//
// PyRedex itself has no block_on'd I/O — every method is local /
// synchronous, so there's no AsyncRedex wrapper. PyRedexFile's only
// blocking surface is `tail()` (constructs a BoxStream inside a
// tokio runtime context); per-chunk pulls drive the BoxStream
// forward and that's where the async iterator shape pays off.
//
// AsyncRedexFile: constructor from sync PyRedexFile (cheap
// Arc::clone). Pass-through append / append_batch / read_range /
// len / sync / close stay sync. `tail` returns an awaitable
// resolving to AsyncRedexTailIter.
// =========================================================================

/// Async sibling of [`PyRedexFile`]. Wraps the same `Arc<RedexFile>`
/// as the sync sibling; appends are visible across both. Pass-
/// through writes / reads stay sync (no awaiting needed on the
/// in-process write path); `tail` returns an awaitable yielding an
/// `AsyncRedexTailIter`.
///
/// Sync equivalent: :class:`RedexFile`.
#[pyclass(name = "AsyncRedexFile", module = "_net")]
pub struct PyAsyncRedexFile {
    inner: Arc<InnerRedexFile>,
}

#[pymethods]
impl PyAsyncRedexFile {
    /// Build against an existing sync `RedexFile`. Cheap
    /// (`Arc::clone`); both shapes see the same retained-event
    /// index and live appends.
    #[new]
    fn new(file: &PyRedexFile) -> Self {
        Self {
            inner: file.inner.clone(),
        }
    }

    fn append(&self, payload: &[u8]) -> PyResult<u64> {
        self.inner
            .append(payload)
            .map_err(|e| RedexError::new_err(format!("append: {}", e)))
    }

    fn append_batch(&self, payloads: Vec<Vec<u8>>) -> PyResult<Option<u64>> {
        let bytes: Vec<Bytes> = payloads.into_iter().map(Bytes::from).collect();
        self.inner
            .append_batch(&bytes)
            .map_err(|e| RedexError::new_err(format!("append_batch: {}", e)))
    }

    fn read_range(&self, start: u64, end: u64) -> Vec<PyRedexEvent> {
        self.inner
            .read_range(start, end)
            .into_iter()
            .map(PyRedexEvent::from)
            .collect()
    }

    fn __len__(&self) -> usize {
        self.inner.len()
    }

    fn sync(&self) -> PyResult<()> {
        self.inner
            .sync()
            .map_err(|e| RedexError::new_err(format!("sync: {}", e)))
    }

    fn close(&self) -> PyResult<()> {
        self.inner
            .close()
            .map_err(|e| RedexError::new_err(format!("close: {}", e)))
    }

    /// Open a live tail. Returns an awaitable resolving to an
    /// `AsyncRedexTailIter` that yields events with
    /// `seq >= from_seq` (default `0`) — backfills the retained
    /// range, then streams live appends. Stop with
    /// `iter.close()` / `await iter.aclose()`.
    #[pyo3(signature = (from_seq = 0))]
    fn tail<'py>(&self, py: Python<'py>, from_seq: u64) -> PyResult<Bound<'py, PyAny>> {
        let adapter = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let stream = adapter.tail(from_seq).boxed();
            Ok::<PyAsyncRedexTailIter, PyErr>(PyAsyncRedexTailIter {
                inner: Arc::new(RedexTailIterInner {
                    stream: TokioMutex::new(Some(stream)),
                    shutdown: Notify::new(),
                    is_shutdown: AtomicBool::new(false),
                }),
            })
        })
    }
}

/// Async sibling of [`PyRedexTailIter`]. PEP 525 async iterator —
/// ``async for ev in file.tail(seq):``. End-of-stream raises
/// `StopAsyncIteration`; transport errors raise `RedexError`.
///
/// Sync equivalent: :class:`RedexTailIter`.
#[pyclass(name = "AsyncRedexTailIter", module = "_net")]
pub struct PyAsyncRedexTailIter {
    inner: Arc<RedexTailIterInner>,
}

#[pymethods]
impl PyAsyncRedexTailIter {
    fn __aiter__(slf: PyRef<Self>) -> PyRef<Self> {
        slf
    }

    fn __anext__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            if inner.is_shutdown.load(Ordering::Acquire) {
                return Err(pyo3::exceptions::PyStopAsyncIteration::new_err(()));
            }
            let mut guard = inner.stream.lock().await;
            let stream = match guard.as_mut() {
                Some(s) => s,
                None => {
                    return Err(pyo3::exceptions::PyStopAsyncIteration::new_err(()));
                }
            };

            let shutdown_fut = inner.shutdown.notified();
            tokio::pin!(shutdown_fut);
            shutdown_fut.as_mut().enable();

            if inner.is_shutdown.load(Ordering::Acquire) {
                *guard = None;
                return Err(pyo3::exceptions::PyStopAsyncIteration::new_err(()));
            }

            let outcome = tokio::select! {
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
            };
            drop(guard);
            match outcome {
                TailNext::Event(ev) => Ok(PyRedexEvent::from(ev)),
                TailNext::Error(msg) => Err(RedexError::new_err(format!("tail: {}", msg))),
                TailNext::End => Err(pyo3::exceptions::PyStopAsyncIteration::new_err(())),
            }
        })
    }

    fn close(&self) {
        self.inner.is_shutdown.store(true, Ordering::Release);
        self.inner.shutdown.notify_waiters();
    }

    fn aclose<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.close();
        pyo3_async_runtimes::tokio::future_into_py(py, async move { Ok::<(), PyErr>(()) })
    }
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
///
/// Async equivalent: :class:`AsyncTasksAdapter` — same inner
/// adapter; awaitable `wait_for_seq` / `wait_for_token` /
/// `watch_tasks` / `snapshot_and_watch_tasks`.
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
    fn open(py: Python<'_>, redex: &PyRedex, origin_hash: u64, persistent: bool) -> PyResult<Self> {
        let runtime = make_runtime()?;
        let redex_inner = redex.inner.clone();
        let cfg = cfg_from_persistent(persistent);
        let runtime_for_block = runtime.clone();
        let inner = py
            .detach(move || {
                runtime_for_block.block_on(async move {
                    InnerTasksAdapter::open_with_config(&redex_inner, origin_hash, cfg).await
                })
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
        py: Python<'_>,
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
        let runtime_for_block = runtime.clone();
        let inner = py
            .detach(move || {
                runtime_for_block.block_on(async move {
                    InnerTasksAdapter::open_from_snapshot_with_config(
                        &redex_inner,
                        origin_hash,
                        cfg,
                        &bytes,
                        last_seq,
                    )
                    .await
                })
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
    /// Releases the GIL for the duration of the wait. Raises
    /// `CortexError` if the fold task stopped before reaching `seq`
    /// (close, Stop-policy halt, retention-evicted tail lag).
    fn wait_for_seq(&self, py: Python<'_>, seq: u64) -> PyResult<()> {
        let inner = self.inner.clone();
        let runtime = self.runtime.clone();
        py.detach(|| {
            runtime
                .block_on(async move { inner.wait_for_seq(seq).await })
                .map_err(|folded| {
                    CortexError::new_err(format!(
                        "wait_for_seq: fold task stopped; folded_through={folded:?}"
                    ))
                })
        })
    }

    /// Read-your-writes wait. Blocks until this adapter's fold has
    /// applied through `token.seq`, or `deadline_ms` elapses.
    /// Raises `CortexError` on timeout, on a wrong-origin token, or
    /// when the per-channel wait queue is saturated. GIL is released
    /// for the wait.
    ///
    /// `deadline_ms == 0` is a non-blocking poll: returns
    /// immediately with success when the watermark already covers
    /// `token.seq`, or a typed `CortexError` (timeout / wrong-origin
    /// / fold-stopped) otherwise. Mirrors the FFI / Node / Go
    /// `timeout_ms == 0` contract so all four bindings agree.
    #[pyo3(signature = (token, deadline_ms = 1000))]
    fn wait_for_token(
        &self,
        py: Python<'_>,
        token: &PyWriteToken,
        deadline_ms: u64,
    ) -> PyResult<()> {
        let inner = self.inner.clone();
        let inner_token = token.as_inner();
        if deadline_ms == 0 {
            return inner
                .poll_for_token(inner_token)
                .map_err(map_wait_for_token_err);
        }
        let runtime = self.runtime.clone();
        py.detach(|| {
            runtime.block_on(async move {
                inner
                    .wait_for_token(inner_token, std::time::Duration::from_millis(deadline_ms))
                    .await
            })
        })
        .map_err(map_wait_for_token_err)
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
        py: Python<'_>,
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
            py.detach(move || runtime.block_on(async move { w.stream().boxed() }));
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
        py: Python<'_>,
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
        let (snapshot, stream) =
            py.detach(move || runtime.block_on(async move { adapter.snapshot_and_watch(w) }));
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
///
/// Async equivalent: :class:`AsyncTaskWatchIter` — PEP 525 async
/// iterator with the same yield shape.
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

impl From<std::sync::Arc<InnerMemory>> for PyMemory {
    fn from(m: std::sync::Arc<InnerMemory>) -> Self {
        // Per perf #96: post-Arc-query refactor, the watcher /
        // list paths hand back `Vec<Arc<InnerMemory>>`. The
        // Python FFI surface needs owned `String` / `Vec` bytes,
        // so try to unwrap the Arc (zero-cost when refcount is
        // 1 — the common case after a query finishes and we own
        // the only handle) and fall back to a deep clone
        // otherwise. Net per-result cost at the FFI boundary is
        // at most one `InnerMemory` clone — same as pre-fix.
        let owned = std::sync::Arc::try_unwrap(m).unwrap_or_else(|arc| (*arc).clone());
        owned.into()
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
///
/// Async equivalent: :class:`AsyncMemoriesAdapter` — same inner
/// adapter; awaitable `wait_for_seq` / `wait_for_token` /
/// `watch_memories` / `snapshot_and_watch_memories`.
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
    fn open(py: Python<'_>, redex: &PyRedex, origin_hash: u64, persistent: bool) -> PyResult<Self> {
        let runtime = make_runtime()?;
        let redex_inner = redex.inner.clone();
        let cfg = cfg_from_persistent(persistent);
        let runtime_for_block = runtime.clone();
        let inner = py
            .detach(move || {
                runtime_for_block.block_on(async move {
                    InnerMemoriesAdapter::open_with_config(&redex_inner, origin_hash, cfg).await
                })
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
        py: Python<'_>,
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
        let runtime_for_block = runtime.clone();
        let inner = py
            .detach(move || {
                runtime_for_block.block_on(async move {
                    InnerMemoriesAdapter::open_from_snapshot_with_config(
                        &redex_inner,
                        origin_hash,
                        cfg,
                        &bytes,
                        last_seq,
                    )
                    .await
                })
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

    fn wait_for_seq(&self, py: Python<'_>, seq: u64) -> PyResult<()> {
        let inner = self.inner.clone();
        let runtime = self.runtime.clone();
        py.detach(|| {
            runtime
                .block_on(async move { inner.wait_for_seq(seq).await })
                .map_err(|folded| {
                    CortexError::new_err(format!(
                        "wait_for_seq: fold task stopped; folded_through={folded:?}"
                    ))
                })
        })
    }

    /// Read-your-writes wait. Mirrors `TasksAdapter.wait_for_token`
    /// — raises `CortexError` on timeout, wrong-origin, or queue-
    /// full saturation. `deadline_ms == 0` is a non-blocking poll
    /// (same contract as the FFI / Node / Go bindings).
    #[pyo3(signature = (token, deadline_ms = 1000))]
    fn wait_for_token(
        &self,
        py: Python<'_>,
        token: &PyWriteToken,
        deadline_ms: u64,
    ) -> PyResult<()> {
        let inner = self.inner.clone();
        let inner_token = token.as_inner();
        if deadline_ms == 0 {
            return inner
                .poll_for_token(inner_token)
                .map_err(map_wait_for_token_err);
        }
        let runtime = self.runtime.clone();
        py.detach(|| {
            runtime.block_on(async move {
                inner
                    .wait_for_token(inner_token, std::time::Duration::from_millis(deadline_ms))
                    .await
            })
        })
        .map_err(map_wait_for_token_err)
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
        py: Python<'_>,
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
        let stream: BoxStream<'static, Vec<std::sync::Arc<InnerMemory>>> =
            py.detach(move || runtime.block_on(async move { w.stream().boxed() }));
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
        py: Python<'_>,
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
        let (snapshot, stream) =
            py.detach(move || runtime.block_on(async move { adapter.snapshot_and_watch(w) }));
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
    stream: BoxStream<'static, Vec<std::sync::Arc<InnerMemory>>>,
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
    stream: TokioMutex<Option<BoxStream<'static, Vec<std::sync::Arc<InnerMemory>>>>>,
    shutdown: Notify,
    is_shutdown: AtomicBool,
}

/// Sync Python iterator over a live memory filter.
///
/// Async equivalent: :class:`AsyncMemoryWatchIter` — PEP 525 async
/// iterator with the same yield shape.
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
        py: Python<'_>,
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
            Some(PyTasksAdapter::open(py, &redex, origin_hash, persistent)?)
        } else {
            None
        };
        let memories = if with_memories {
            Some(PyMemoriesAdapter::open(
                py,
                &redex,
                origin_hash,
                persistent,
            )?)
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
        py: Python<'_>,
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
                    py,
                    &redex,
                    origin_hash,
                    &bytes,
                    last_seq,
                    persistent,
                )?),
                None => Some(PyTasksAdapter::open(py, &redex, origin_hash, persistent)?),
            }
        } else {
            None
        };

        let memories = if with_memories {
            match snapshot.memories {
                Some((bytes, last_seq)) => Some(PyMemoriesAdapter::open_from_snapshot(
                    py,
                    &redex,
                    origin_hash,
                    &bytes,
                    last_seq,
                    persistent,
                )?),
                None => Some(PyMemoriesAdapter::open(
                    py,
                    &redex,
                    origin_hash,
                    persistent,
                )?),
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

// =========================================================================
// AsyncMemoriesAdapter + AsyncMemoryWatchIter — T2-C3.
//
// Async sibling of `PyMemoriesAdapter`. Shares the same
// `Arc<InnerMemoriesAdapter>` as the sync sibling, so writes via
// either are visible to the other.
//
// Awaitable methods (run on the pyo3-async-runtimes bridge runtime):
//   wait_for_seq, wait_for_token, watch_memories,
//   snapshot_and_watch_memories.
//
// The store/retag/pin/unpin/delete writes are synchronous on the
// inner adapter (they enqueue into the fold task without awaiting),
// so they keep their sync shape on the async wrapper too. Same for
// `list_memories` (in-memory query), `snapshot`, `count`, `close`,
// `is_running`.
// =========================================================================

/// Async sibling of [`PyMemoriesAdapter`]. Construct from a sync
/// [`PyMemoriesAdapter`]; the underlying `MemoriesAdapter` is
/// shared.
///
/// Sync equivalent: :class:`MemoriesAdapter`.
#[pyclass(name = "AsyncMemoriesAdapter", module = "_net", from_py_object)]
#[derive(Clone)]
pub struct PyAsyncMemoriesAdapter {
    inner: Arc<InnerMemoriesAdapter>,
}

#[pymethods]
impl PyAsyncMemoriesAdapter {
    /// Build against an existing sync `MemoriesAdapter`. Cheap
    /// (`Arc::clone`). The two wrappers share the same inner
    /// adapter / fold task; writes are visible across both.
    #[new]
    fn new(memories: &PyMemoriesAdapter) -> Self {
        Self {
            inner: memories.inner.clone(),
        }
    }

    /// Capture a state snapshot for restore via
    /// `MemoriesAdapter.open_from_snapshot`. Sync — captures from
    /// the in-memory state, no awaiting.
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

    /// Await the fold task to consume up to `seq`. Returns an
    /// awaitable resolving to ``None`` on success.
    fn wait_for_seq<'py>(&self, py: Python<'py>, seq: u64) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            inner
                .wait_for_seq(seq)
                .await
                .map_err(|folded| {
                    CortexError::new_err(format!(
                        "wait_for_seq: fold task stopped; folded_through={folded:?}"
                    ))
                })?;
            Ok::<(), PyErr>(())
        })
    }

    /// Read-your-writes wait. `deadline_ms == 0` is a non-blocking
    /// poll (raises immediately if the token isn't ready).
    #[pyo3(signature = (token, deadline_ms = 1000))]
    fn wait_for_token<'py>(
        &self,
        py: Python<'py>,
        token: &PyWriteToken,
        deadline_ms: u64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        let inner_token = token.as_inner();
        if deadline_ms == 0 {
            // Non-blocking poll — still return an awaitable for shape
            // symmetry; resolves immediately.
            let result = inner.poll_for_token(inner_token);
            return pyo3_async_runtimes::tokio::future_into_py(py, async move {
                result.map_err(map_wait_for_token_err)
            });
        }
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            inner
                .wait_for_token(inner_token, std::time::Duration::from_millis(deadline_ms))
                .await
                .map_err(map_wait_for_token_err)
        })
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

    /// In-memory query over the fold's current state. Sync — no
    /// awaiting needed.
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

    /// Returns an awaitable resolving to an `AsyncMemoryWatchIter`
    /// over the live fold stream. Use ``async for`` on the
    /// returned iterator.
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
    fn watch_memories<'py>(
        &self,
        py: Python<'py>,
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
    ) -> PyResult<Bound<'py, PyAny>> {
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
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let stream: BoxStream<'static, Vec<std::sync::Arc<InnerMemory>>> = w.stream().boxed();
            Ok::<PyAsyncMemoryWatchIter, PyErr>(new_async_memory_watch_iter(stream))
        })
    }

    /// Atomic snapshot + watch. Returns an awaitable resolving to
    /// ``(snapshot_list, AsyncMemoryWatchIter)``.
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
    fn snapshot_and_watch_memories<'py>(
        &self,
        py: Python<'py>,
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
    ) -> PyResult<Bound<'py, PyAny>> {
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
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let (snapshot, stream) = adapter.snapshot_and_watch(w);
            let snap_py: Vec<PyMemory> = snapshot.into_iter().map(PyMemory::from).collect();
            let iter = new_async_memory_watch_iter(stream);
            Ok::<(Vec<PyMemory>, PyAsyncMemoryWatchIter), PyErr>((snap_py, iter))
        })
    }
}

fn new_async_memory_watch_iter(
    stream: BoxStream<'static, Vec<std::sync::Arc<InnerMemory>>>,
) -> PyAsyncMemoryWatchIter {
    PyAsyncMemoryWatchIter {
        inner: Arc::new(MemoryWatchIterInner {
            stream: TokioMutex::new(Some(stream)),
            shutdown: Notify::new(),
            is_shutdown: AtomicBool::new(false),
        }),
    }
}

/// Async sibling of [`PyMemoryWatchIter`]. PEP 525 async iterator
/// (``async for batch in iter:``). Each yield is a batch of
/// `Memory` objects matching the watch's filters.
///
/// Sync equivalent: :class:`MemoryWatchIter`.
#[pyclass(name = "AsyncMemoryWatchIter", module = "_net")]
pub struct PyAsyncMemoryWatchIter {
    inner: Arc<MemoryWatchIterInner>,
}

#[pymethods]
impl PyAsyncMemoryWatchIter {
    fn __aiter__(slf: PyRef<Self>) -> PyRef<Self> {
        slf
    }

    fn __anext__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            if inner.is_shutdown.load(Ordering::Acquire) {
                return Err(pyo3::exceptions::PyStopAsyncIteration::new_err(()));
            }
            let mut guard = inner.stream.lock().await;
            let stream = match guard.as_mut() {
                Some(s) => s,
                None => {
                    return Err(pyo3::exceptions::PyStopAsyncIteration::new_err(()));
                }
            };
            let shutdown_fut = inner.shutdown.notified();
            tokio::pin!(shutdown_fut);
            shutdown_fut.as_mut().enable();

            if inner.is_shutdown.load(Ordering::Acquire) {
                *guard = None;
                return Err(pyo3::exceptions::PyStopAsyncIteration::new_err(()));
            }
            let next = tokio::select! {
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
            };
            drop(guard);
            match next {
                Some(items) => {
                    let mapped: Vec<PyMemory> = items.into_iter().map(PyMemory::from).collect();
                    Ok::<Vec<PyMemory>, PyErr>(mapped)
                }
                None => Err(pyo3::exceptions::PyStopAsyncIteration::new_err(())),
            }
        })
    }

    /// Stop the iterator. Idempotent. Subsequent `__anext__` calls
    /// raise `StopAsyncIteration`.
    fn close(&self) {
        self.inner.is_shutdown.store(true, Ordering::Release);
        self.inner.shutdown.notify_waiters();
    }

    /// Async alias for :meth:`close` so users can write
    /// ``await iter.aclose()``.
    fn aclose<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.close();
        pyo3_async_runtimes::tokio::future_into_py(py, async move { Ok::<(), PyErr>(()) })
    }
}

// =========================================================================
// AsyncTasksAdapter + AsyncTaskWatchIter — T2-C4.
//
// Mirror of T2-C3 for the tasks adapter. Same pattern: async sibling
// shares `Arc<InnerTasksAdapter>` with the sync sibling; only the
// wait_for_*/watch_* methods need awaiting (the write surface enqueues
// without awaiting and stays sync on both shapes).
// =========================================================================

/// Async sibling of [`PyTasksAdapter`]. Construct from a sync
/// [`PyTasksAdapter`]; the underlying `TasksAdapter` is shared.
///
/// Sync equivalent: :class:`TasksAdapter`.
#[pyclass(name = "AsyncTasksAdapter", module = "_net", from_py_object)]
#[derive(Clone)]
pub struct PyAsyncTasksAdapter {
    inner: Arc<InnerTasksAdapter>,
}

#[pymethods]
impl PyAsyncTasksAdapter {
    /// Build against an existing sync `TasksAdapter`. Cheap
    /// (`Arc::clone`); the inner adapter / fold task is shared.
    #[new]
    fn new(tasks: &PyTasksAdapter) -> Self {
        Self {
            inner: tasks.inner.clone(),
        }
    }

    /// Capture a state snapshot. Sync.
    fn snapshot(&self) -> PyResult<(Vec<u8>, Option<u64>)> {
        self.inner
            .snapshot()
            .map_err(|e| CortexError::new_err(format!("snapshot failed: {}", e)))
    }

    fn create(&self, id: u64, title: String, now_ns: u64) -> PyResult<u64> {
        self.inner
            .create(id, title, now_ns)
            .map_err(|e| CortexError::new_err(format!("create failed: {}", e)))
    }

    fn rename(&self, id: u64, new_title: String, now_ns: u64) -> PyResult<u64> {
        self.inner
            .rename(id, new_title, now_ns)
            .map_err(|e| CortexError::new_err(format!("rename failed: {}", e)))
    }

    fn complete(&self, id: u64, now_ns: u64) -> PyResult<u64> {
        self.inner
            .complete(id, now_ns)
            .map_err(|e| CortexError::new_err(format!("complete failed: {}", e)))
    }

    fn delete(&self, id: u64) -> PyResult<u64> {
        self.inner
            .delete(id)
            .map_err(|e| CortexError::new_err(format!("delete failed: {}", e)))
    }

    /// Await the fold task to consume up to `seq`. Returns an
    /// awaitable resolving to ``None`` on success.
    fn wait_for_seq<'py>(&self, py: Python<'py>, seq: u64) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            inner
                .wait_for_seq(seq)
                .await
                .map_err(|folded| {
                    CortexError::new_err(format!(
                        "wait_for_seq: fold task stopped; folded_through={folded:?}"
                    ))
                })?;
            Ok::<(), PyErr>(())
        })
    }

    /// Read-your-writes wait. `deadline_ms == 0` is non-blocking.
    #[pyo3(signature = (token, deadline_ms = 1000))]
    fn wait_for_token<'py>(
        &self,
        py: Python<'py>,
        token: &PyWriteToken,
        deadline_ms: u64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        let inner_token = token.as_inner();
        if deadline_ms == 0 {
            let result = inner.poll_for_token(inner_token);
            return pyo3_async_runtimes::tokio::future_into_py(py, async move {
                result.map_err(map_wait_for_token_err)
            });
        }
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            inner
                .wait_for_token(inner_token, std::time::Duration::from_millis(deadline_ms))
                .await
                .map_err(map_wait_for_token_err)
        })
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

    /// In-memory query over the fold's current state. Sync.
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

    /// Returns an awaitable resolving to an `AsyncTaskWatchIter`.
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
    fn watch_tasks<'py>(
        &self,
        py: Python<'py>,
        status: Option<&str>,
        title_contains: Option<String>,
        created_after_ns: Option<u64>,
        created_before_ns: Option<u64>,
        updated_after_ns: Option<u64>,
        updated_before_ns: Option<u64>,
        order_by: Option<&str>,
        limit: Option<u32>,
    ) -> PyResult<Bound<'py, PyAny>> {
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
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let stream: BoxStream<'static, Vec<InnerTask>> = w.stream().boxed();
            Ok::<PyAsyncTaskWatchIter, PyErr>(new_async_task_watch_iter(stream))
        })
    }

    /// Atomic snapshot + watch. Returns an awaitable resolving to
    /// ``(snapshot_list, AsyncTaskWatchIter)``.
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
    fn snapshot_and_watch_tasks<'py>(
        &self,
        py: Python<'py>,
        status: Option<&str>,
        title_contains: Option<String>,
        created_after_ns: Option<u64>,
        created_before_ns: Option<u64>,
        updated_after_ns: Option<u64>,
        updated_before_ns: Option<u64>,
        order_by: Option<&str>,
        limit: Option<u32>,
    ) -> PyResult<Bound<'py, PyAny>> {
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
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let (snapshot, stream) = adapter.snapshot_and_watch(w);
            let snap_py: Vec<PyTask> = snapshot.into_iter().map(PyTask::from).collect();
            let iter = new_async_task_watch_iter(stream);
            Ok::<(Vec<PyTask>, PyAsyncTaskWatchIter), PyErr>((snap_py, iter))
        })
    }
}

fn new_async_task_watch_iter(
    stream: BoxStream<'static, Vec<InnerTask>>,
) -> PyAsyncTaskWatchIter {
    PyAsyncTaskWatchIter {
        inner: Arc::new(TaskWatchIterInner {
            stream: TokioMutex::new(Some(stream)),
            shutdown: Notify::new(),
            is_shutdown: AtomicBool::new(false),
        }),
    }
}

/// Async sibling of [`PyTaskWatchIter`]. PEP 525 async iterator
/// (``async for batch in iter:``).
///
/// Sync equivalent: :class:`TaskWatchIter`.
#[pyclass(name = "AsyncTaskWatchIter", module = "_net")]
pub struct PyAsyncTaskWatchIter {
    inner: Arc<TaskWatchIterInner>,
}

#[pymethods]
impl PyAsyncTaskWatchIter {
    fn __aiter__(slf: PyRef<Self>) -> PyRef<Self> {
        slf
    }

    fn __anext__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            if inner.is_shutdown.load(Ordering::Acquire) {
                return Err(pyo3::exceptions::PyStopAsyncIteration::new_err(()));
            }
            let mut guard = inner.stream.lock().await;
            let stream = match guard.as_mut() {
                Some(s) => s,
                None => {
                    return Err(pyo3::exceptions::PyStopAsyncIteration::new_err(()));
                }
            };
            let shutdown_fut = inner.shutdown.notified();
            tokio::pin!(shutdown_fut);
            shutdown_fut.as_mut().enable();

            if inner.is_shutdown.load(Ordering::Acquire) {
                *guard = None;
                return Err(pyo3::exceptions::PyStopAsyncIteration::new_err(()));
            }
            let next = tokio::select! {
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
            };
            drop(guard);
            match next {
                Some(items) => {
                    let mapped: Vec<PyTask> = items.into_iter().map(PyTask::from).collect();
                    Ok::<Vec<PyTask>, PyErr>(mapped)
                }
                None => Err(pyo3::exceptions::PyStopAsyncIteration::new_err(())),
            }
        })
    }

    fn close(&self) {
        self.inner.is_shutdown.store(true, Ordering::Release);
        self.inner.shutdown.notify_waiters();
    }

    fn aclose<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.close();
        pyo3_async_runtimes::tokio::future_into_py(py, async move { Ok::<(), PyErr>(()) })
    }
}
