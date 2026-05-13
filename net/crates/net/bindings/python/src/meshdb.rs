//! Python bindings for MeshDB — federated query layer.
//!
//! # Slice 1 scope
//!
//! The first Python SDK slice ships the absolute minimum to run an
//! end-to-end MeshDB query from Python:
//!
//! - [`PyMeshQuery`] — a 1:1-with-AST factory surface. Variants
//!   construct via static methods (`MeshQuery.at(...)`,
//!   `MeshQuery.between(...)`, `MeshQuery.latest(...)`). Other
//!   variants land in slice 2.
//! - [`PyInMemoryChainReader`] — Python-facing in-memory
//!   `ChainReader` impl. Lets Python code `.append(origin, seq,
//!   payload)` then run queries against the resulting fixture.
//!   Phase B+ adds a `from_redex(...)` adapter.
//! - [`PyMeshQueryRunner`] — owns a `LocalMeshQueryExecutor` plus
//!   an in-process Tokio runtime. `.execute(query, options)` drains
//!   the row stream synchronously and returns a `list[ResultRow]`
//!   (locked decision: Python is sync-first; async wrapper is a
//!   follow-up).
//! - [`PyResultRow`] — `(origin: int, seq: int, payload: bytes)`.
//! - [`PyExecuteOptions`] + [`PyCachePolicy`] — Phase F cache
//!   surface. Default is `TimeBound(5s)`; callers can pass
//!   `CachePolicy.permanent()` or `bypass_cache=True`.
//! - [`MeshDbError`] — Python exception covering every MeshError
//!   variant (mapped via Display for now; structured access
//!   lands when consumers ask for it).
//!
//! # Builder
//!
//! The fluent builder API (`MeshQuery.query().between(...).filter(...)`)
//! is slice 2. Slice 1 stays factory-only so the surface lands tight.
//!
//! # Async
//!
//! Slice 1 is sync only — `runner.execute(...)` drains into a list.
//! Locked decision: Python sync-first; pyo3-asyncio support is a
//! later slice when a consumer needs it.

use std::sync::{Arc, Mutex};

use pyo3::create_exception;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use tokio::runtime::Runtime;

use net::adapter::net::behavior::meshdb::{
    cache::{CachePolicy, LruResultCache},
    executor::{ChainReader, ExecuteOptions, LocalMeshQueryExecutor, MeshQueryExecutor},
    planner::{CostEstimate, OperatorNode, OperatorPlan},
    query::ResultRow,
    ExecutionPlan, SeqNum,
};
use net::adapter::net::behavior::meshdb::MeshError;

create_exception!(
    _net,
    MeshDbError,
    PyException,
    "MeshDB query failure — covers planner / executor / cache errors.\n\nString form mirrors the Rust `MeshError::Display`."
);

/// One row from a query result. `origin` is the chain's 16-hex
/// u64 identifier; `seq` is the sequence number; `payload` is
/// opaque bytes (typically the event body or a postcard-encoded
/// envelope for aggregate / join / window sentinel rows).
#[pyclass(name = "ResultRow", module = "net._net", frozen, from_py_object)]
#[derive(Clone)]
pub struct PyResultRow {
    #[pyo3(get)]
    pub origin: u64,
    #[pyo3(get)]
    pub seq: u64,
    payload: Vec<u8>,
}

#[pymethods]
impl PyResultRow {
    /// The row's opaque payload bytes.
    #[getter]
    fn payload<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, &self.payload)
    }

    fn __repr__(&self) -> String {
        format!(
            "ResultRow(origin={:#018x}, seq={}, payload=<{} bytes>)",
            self.origin,
            self.seq,
            self.payload.len()
        )
    }
}

impl From<ResultRow> for PyResultRow {
    fn from(r: ResultRow) -> Self {
        Self {
            origin: r.origin,
            seq: r.seq.0,
            payload: r.payload,
        }
    }
}

/// Cache policy passed through `ExecuteOptions.cache_policy`.
/// `Permanent` is the explicit opt-in for queries over closed
/// substrate ranges (e.g. `At(chain, seq)` — the answer is
/// immutable). `TimeBound(ttl_secs)` is the default (5 s,
/// mirroring the join watermark).
#[pyclass(name = "CachePolicy", module = "net._net", frozen, from_py_object)]
#[derive(Clone)]
pub struct PyCachePolicy {
    inner: CachePolicy,
}

#[pymethods]
impl PyCachePolicy {
    /// `Permanent` — cache until LRU eviction. Use only for
    /// queries whose result is immutable under substrate
    /// semantics (`At`, closed `Between`).
    #[staticmethod]
    fn permanent() -> Self {
        Self {
            inner: CachePolicy::Permanent,
        }
    }

    /// `TimeBound { ttl: seconds }` — TTL expiry. Defaults to
    /// 5 s when neither this nor `permanent()` is specified;
    /// pass `seconds = 0` for an effectively-no-cache mode
    /// (cache writes succeed but every lookup misses).
    #[staticmethod]
    #[pyo3(signature = (seconds=5.0))]
    fn time_bound(seconds: f64) -> Self {
        let secs = if seconds.is_finite() && seconds >= 0.0 {
            seconds
        } else {
            5.0
        };
        Self {
            inner: CachePolicy::TimeBound {
                ttl: std::time::Duration::from_secs_f64(secs),
            },
        }
    }

    fn __repr__(&self) -> String {
        match self.inner {
            CachePolicy::Permanent => "CachePolicy.permanent()".to_string(),
            CachePolicy::TimeBound { ttl } => {
                format!("CachePolicy.time_bound({:.3})", ttl.as_secs_f64())
            }
        }
    }
}

/// Per-execute options. Phase F locked decisions:
/// `bypass_cache=False` and `cache_policy=TimeBound(5s)` by
/// default; callers override per query.
#[pyclass(name = "ExecuteOptions", module = "net._net", frozen, from_py_object)]
#[derive(Clone)]
pub struct PyExecuteOptions {
    inner: ExecuteOptions,
}

#[pymethods]
impl PyExecuteOptions {
    #[new]
    #[pyo3(signature = (bypass_cache=false, cache_policy=None))]
    fn new(bypass_cache: bool, cache_policy: Option<PyCachePolicy>) -> Self {
        Self {
            inner: ExecuteOptions {
                bypass_cache,
                cache_policy: cache_policy
                    .map(|p| p.inner)
                    .unwrap_or_default(),
            },
        }
    }

    #[getter]
    fn bypass_cache(&self) -> bool {
        self.inner.bypass_cache
    }

    fn __repr__(&self) -> String {
        let policy = PyCachePolicy {
            inner: self.inner.cache_policy,
        };
        format!(
            "ExecuteOptions(bypass_cache={}, cache_policy={})",
            self.inner.bypass_cache,
            policy.__repr__()
        )
    }
}

/// 1:1 AST surface. Construct via static factory methods that
/// mirror the Rust `OperatorPlan` variants. Slice 1 ships the
/// atomic operators (`at`, `between`, `latest`); composite
/// variants and the fluent builder land in slice 2.
///
/// Internally this carries a fully-planned `OperatorNode` so the
/// runner doesn't need to re-plan. Phase B+ may switch to a
/// `MeshQuery::V1` enum carrying the raw AST (so `Discovered`
/// resolution + cardinality estimation happen at execute time).
#[pyclass(name = "MeshQuery", module = "net._net", frozen, from_py_object)]
#[derive(Clone)]
pub struct PyMeshQuery {
    /// Materialized operator-plan tree. For slice 1 we plan
    /// at construction time since the only operators we expose
    /// don't need planner-side resolution.
    plan: ExecutionPlan,
}

#[pymethods]
impl PyMeshQuery {
    /// Read the event at `seq` from chain `origin`.
    #[staticmethod]
    fn at(origin: u64, seq: u64) -> Self {
        let op = OperatorPlan::AtRead {
            origin,
            seq: SeqNum(seq),
        };
        Self {
            plan: plan_of(op),
        }
    }

    /// Read events in the half-open seq range `[start, end)`
    /// from chain `origin`.
    #[staticmethod]
    fn between(origin: u64, start: u64, end: u64) -> PyResult<Self> {
        if start >= end {
            return Err(MeshDbError::new_err(format!(
                "between: start ({start}) must be < end ({end})"
            )));
        }
        let op = OperatorPlan::BetweenRead {
            origin,
            start: SeqNum(start),
            end: SeqNum(end),
        };
        Ok(Self {
            plan: plan_of(op),
        })
    }

    /// Read the tip event from chain `origin`.
    #[staticmethod]
    fn latest(origin: u64) -> Self {
        let op = OperatorPlan::LatestRead { origin };
        Self {
            plan: plan_of(op),
        }
    }

    fn __repr__(&self) -> String {
        match &self.plan.root.operator {
            OperatorPlan::AtRead { origin, seq } => {
                format!("MeshQuery.at(origin={origin:#018x}, seq={})", seq.0)
            }
            OperatorPlan::BetweenRead { origin, start, end } => format!(
                "MeshQuery.between(origin={origin:#018x}, start={}, end={})",
                start.0, end.0
            ),
            OperatorPlan::LatestRead { origin } => {
                format!("MeshQuery.latest(origin={origin:#018x})")
            }
            // Slice 1 only exposes the three atomic operators
            // above; other variants are unreachable via the
            // current factory surface.
            other => format!("MeshQuery(<{other:?}>)"),
        }
    }
}

fn plan_of(op: OperatorPlan) -> ExecutionPlan {
    ExecutionPlan {
        root: OperatorNode {
            operator: op,
            target_nodes: vec![],
            cost: CostEstimate::default(),
        },
        total_cost: CostEstimate::default(),
    }
}

/// In-process `ChainReader` Python wrapper. Slice 1 ships a
/// simple in-memory variant: `.append(origin, seq, payload)` to
/// populate, hand off to `MeshQueryRunner(reader)`. Phase B+
/// adds adapters for the Redex-backed reader.
#[pyclass(name = "InMemoryChainReader", module = "net._net")]
pub struct PyInMemoryChainReader {
    inner: Arc<InMemoryStore>,
}

#[derive(Default)]
struct InMemoryStore {
    chains: Mutex<std::collections::BTreeMap<u64, std::collections::BTreeMap<SeqNum, Vec<u8>>>>,
}

impl ChainReader for InMemoryStore {
    fn read_one(&self, origin: u64, seq: SeqNum) -> Option<Vec<u8>> {
        self.chains.lock().unwrap().get(&origin)?.get(&seq).cloned()
    }

    fn read_range(&self, origin: u64, start: SeqNum, end: SeqNum) -> Vec<(SeqNum, Vec<u8>)> {
        self.chains
            .lock()
            .unwrap()
            .get(&origin)
            .map(|chain| {
                chain
                    .range(start..end)
                    .map(|(s, p)| (*s, p.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn latest_seq(&self, origin: u64) -> Option<SeqNum> {
        self.chains
            .lock()
            .unwrap()
            .get(&origin)?
            .keys()
            .next_back()
            .copied()
    }
}

#[pymethods]
impl PyInMemoryChainReader {
    #[new]
    fn new() -> Self {
        Self {
            inner: Arc::new(InMemoryStore::default()),
        }
    }

    /// Append a single event to the in-memory store. `payload`
    /// accepts any `bytes`-like object.
    fn append(&self, origin: u64, seq: u64, payload: Vec<u8>) {
        self.inner
            .chains
            .lock()
            .unwrap()
            .entry(origin)
            .or_default()
            .insert(SeqNum(seq), payload);
    }

    /// Tip of chain `origin`, or `None` if unknown.
    fn latest_seq(&self, origin: u64) -> Option<u64> {
        self.inner.latest_seq(origin).map(|s| s.0)
    }

    fn __repr__(&self) -> String {
        let chains = self.inner.chains.lock().unwrap().len();
        format!("InMemoryChainReader(chains={chains})")
    }
}

/// Owns a [`LocalMeshQueryExecutor`] + a Tokio runtime; exposes
/// `.execute(query, options) -> list[ResultRow]`. Sync drain by
/// design — locked decision: Python is sync-first, async wrapper
/// is a later slice.
#[pyclass(name = "MeshQueryRunner", module = "net._net")]
pub struct PyMeshQueryRunner {
    runtime: Arc<Runtime>,
    executor: Arc<LocalMeshQueryExecutor<InMemoryStore>>,
}

#[pymethods]
impl PyMeshQueryRunner {
    /// Build a runner over the given `InMemoryChainReader`.
    /// `enable_cache=True` wires the Phase F LRU; the
    /// `capability_version` closure is hard-wired to `0`
    /// because there's no `CapabilityIndex` plumbed yet (slice
    /// 1 is local-executor-only).
    #[new]
    #[pyo3(signature = (reader, enable_cache=false))]
    fn new(reader: &PyInMemoryChainReader, enable_cache: bool) -> PyResult<Self> {
        let runtime = Runtime::new().map_err(|e| {
            MeshDbError::new_err(format!("failed to construct tokio runtime: {e}"))
        })?;
        let store = reader.inner.clone();
        let executor: LocalMeshQueryExecutor<InMemoryStore> = if enable_cache {
            let cache: Arc<dyn net::adapter::net::behavior::meshdb::cache::ResultCache> =
                Arc::new(LruResultCache::default());
            let version_fn: Arc<dyn Fn() -> u64 + Send + Sync> = Arc::new(|| 0);
            LocalMeshQueryExecutor::with_cache(store, cache, version_fn)
        } else {
            LocalMeshQueryExecutor::new(store)
        };
        Ok(Self {
            runtime: Arc::new(runtime),
            executor: Arc::new(executor),
        })
    }

    /// Execute `query` synchronously. Returns the full row list
    /// (sync drain). Phase F cache options ride on `options`;
    /// when `None`, defaults are applied (TimeBound { 5 s },
    /// bypass_cache=False).
    #[pyo3(signature = (query, options=None))]
    fn execute(
        &self,
        py: Python<'_>,
        query: &PyMeshQuery,
        options: Option<PyExecuteOptions>,
    ) -> PyResult<Vec<PyResultRow>> {
        let plan = query.plan.clone();
        let opts = options.map(|o| o.inner).unwrap_or_default();
        let executor = self.executor.clone();
        let runtime = self.runtime.clone();
        // Release the GIL while we drive the executor.
        py.detach(move || {
            runtime.block_on(async move {
                use futures::StreamExt;
                let running = executor
                    .execute_with(plan, opts)
                    .await
                    .map_err(map_mesh_error)?;
                let mut stream = running.rows;
                let mut out: Vec<PyResultRow> = Vec::new();
                while let Some(item) = stream.next().await {
                    let row = item.map_err(map_mesh_error)?;
                    out.push(row.into());
                }
                Ok::<_, PyErr>(out)
            })
        })
    }
}

fn map_mesh_error(e: MeshError) -> PyErr {
    MeshDbError::new_err(format!("{e}"))
}

// Tests live in `bindings/python/tests/test_meshdb.py` — the
// pyo3 unit-test linker dance on Windows requires libpython on
// PATH (only reliably available under `maturin develop`), and
// the existing Python bindings don't ship Rust-side tests.
// Run via:
//   maturin develop --features meshdb
//   pytest bindings/python/tests/test_meshdb.py
