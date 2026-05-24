//! PyO3 bindings for the `aggregator.registry` + `fold.query`
//! RPC clients. Client surface only — daemon-author types stay
//! Rust-only.
//!
//! Methods take a `Python<'_>` token and `py.detach(|| ...)`
//! around the blocking `runtime.block_on` call so the GIL is
//! released during the RPC (same pattern as `mesh_rpc.rs`;
//! async Python wraps in `asyncio.to_thread`).
//!
//! Errors raise `RegistryClientError` / `FoldQueryClientError`
//! (or a typed subclass) with `.kind` + `.server_detail` set
//! before raise. Discriminators are pinned by
//! `tests/error_kind_mirror.rs`.

use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use pyo3::create_exception;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use tokio::runtime::Runtime;

use net_sdk::aggregator::{
    FoldQueryClient as SdkFoldQueryClient, FoldQueryClientError as SdkFoldQueryClientError,
    FoldQueryError, RegistryClient as SdkRegistryClient,
    RegistryClientError as SdkRegistryClientError, RegistryGroupSummary, RegistryReplicaSummary,
    RegistryRpcError, SummaryAnnouncement,
};

// =========================================================================
// Exception hierarchy — RegistryClientError / FoldQueryClientError +
// concrete typed subclasses matching the Rust SDK's variant set.
//
// Python idiom: `try: ... except UnknownTemplate: ...`. The
// `.kind` / `.server_detail` attributes are also populated for
// generic catch-by-base.
// =========================================================================

create_exception!(
    _net,
    RegistryClientError,
    PyException,
    "Aggregator registry RPC failure. `.kind` is one of `transport` | `codec` | \
     `unknown-template` | `duplicate-group-name` | `spawn-rejected` | \
     `spawn-not-supported`. `.server_detail` carries the long-form text."
);

create_exception!(
    _net,
    UnknownTemplate,
    RegistryClientError,
    "Daemon refused spawn — no template registered under the given name."
);
create_exception!(
    _net,
    DuplicateGroupName,
    RegistryClientError,
    "Daemon refused spawn — a group with the requested name is already registered."
);
create_exception!(
    _net,
    SpawnRejected,
    RegistryClientError,
    "Daemon's spawn callback rejected the request (placement / quota / policy)."
);
create_exception!(
    _net,
    SpawnNotSupported,
    RegistryClientError,
    "Target daemon is read-only — it did not install a spawn callback."
);

create_exception!(
    _net,
    FoldQueryClientError,
    PyException,
    "Fold-query RPC failure. `.kind` is one of `transport` | `codec` | \
     `unknown-kind`. `.server_detail` carries the long-form text."
);
create_exception!(
    _net,
    UnknownFoldKind,
    FoldQueryClientError,
    "Daemon does not register a summarizer for the requested fold kind."
);

fn set_err_attrs<T: Into<PyErr>>(py: Python<'_>, err: T, kind: &str, detail: &str) -> PyErr {
    let py_err: PyErr = err.into();
    let exc = py_err.value(py);
    let _ = exc.setattr("kind", kind);
    let _ = exc.setattr("server_detail", detail);
    py_err
}

fn registry_err(py: Python<'_>, e: SdkRegistryClientError) -> PyErr {
    let (kind, detail): (&str, String) = match &e {
        SdkRegistryClientError::Transport(t) => ("transport", t.to_string()),
        SdkRegistryClientError::Codec(c) => ("codec", c.to_string()),
        SdkRegistryClientError::Server(srv) => match srv {
            RegistryRpcError::DecodeFailed(s) => ("codec", format!("server-decode: {s}")),
            RegistryRpcError::UnknownTemplate(t) => ("unknown-template", t.clone()),
            RegistryRpcError::DuplicateGroupName(n) => ("duplicate-group-name", n.clone()),
            RegistryRpcError::SpawnRejected(d) => ("spawn-rejected", d.clone()),
            RegistryRpcError::SpawnNotSupported => {
                ("spawn-not-supported", "daemon is read-only".to_string())
            }
            RegistryRpcError::UnknownGroup(g) => ("unknown-group", g.clone()),
            RegistryRpcError::ScaleRejected(d) => ("scale-rejected", d.clone()),
            RegistryRpcError::ScaleNotSupported => (
                "scale-not-supported",
                "daemon doesn't accept dynamic scale".to_string(),
            ),
        },
    };
    let message = format!("agg:{kind}: {detail}");
    let py_err = match kind {
        "unknown-template" => UnknownTemplate::new_err(message),
        "duplicate-group-name" => DuplicateGroupName::new_err(message),
        "spawn-rejected" => SpawnRejected::new_err(message),
        "spawn-not-supported" => SpawnNotSupported::new_err(message),
        _ => RegistryClientError::new_err(message),
    };
    set_err_attrs(py, py_err, kind, &detail)
}

fn fold_query_err(py: Python<'_>, e: SdkFoldQueryClientError) -> PyErr {
    let (kind, detail): (&str, String) = match &e {
        SdkFoldQueryClientError::Transport(t) => ("transport", t.to_string()),
        SdkFoldQueryClientError::Codec(c) => ("codec", c.to_string()),
        SdkFoldQueryClientError::Server(srv) => match srv {
            FoldQueryError::UnknownKind { kind } => ("unknown-kind", format!("0x{kind:04x}")),
            FoldQueryError::DecodeFailed(s) => ("codec", format!("server-decode: {s}")),
        },
    };
    let message = format!("agg:{kind}: {detail}");
    let py_err = match kind {
        "unknown-kind" => UnknownFoldKind::new_err(message),
        _ => FoldQueryClientError::new_err(message),
    };
    set_err_attrs(py, py_err, kind, &detail)
}

// =========================================================================
// Conversion — substrate types → Python dicts.
// =========================================================================

fn replica_to_dict<'py>(
    py: Python<'py>,
    r: &RegistryReplicaSummary,
) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("generation", r.generation)?;
    d.set_item("healthy", r.healthy)?;
    d.set_item("diagnostic", r.diagnostic.clone())?;
    d.set_item("placement_node_id", r.placement_node_id)?;
    Ok(d)
}

fn group_to_dict<'py>(py: Python<'py>, g: &RegistryGroupSummary) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("name", &g.name)?;
    let seed_hex = hex::encode(g.group_seed);
    d.set_item("group_seed_hex", seed_hex)?;
    let replicas = PyList::empty(py);
    for r in &g.replicas {
        replicas.append(replica_to_dict(py, r)?)?;
    }
    d.set_item("replicas", replicas)?;
    Ok(d)
}

fn groups_to_list<'py>(
    py: Python<'py>,
    groups: &[RegistryGroupSummary],
) -> PyResult<Bound<'py, PyList>> {
    let out = PyList::empty(py);
    for g in groups {
        out.append(group_to_dict(py, g)?)?;
    }
    Ok(out)
}

fn summary_to_dict<'py>(py: Python<'py>, s: &SummaryAnnouncement) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("fold_kind", u32::from(s.fold_kind))?;
    d.set_item("source_subnet", format!("{}", s.source_subnet))?;
    d.set_item("generation", s.generation)?;
    let buckets = PyList::empty(py);
    for (name, count) in &s.buckets {
        let b = PyDict::new(py);
        b.set_item("name", name)?;
        b.set_item("count", *count)?;
        buckets.append(b)?;
    }
    d.set_item("buckets", buckets)?;
    Ok(d)
}

fn summaries_to_list<'py>(
    py: Python<'py>,
    summaries: &[SummaryAnnouncement],
) -> PyResult<Bound<'py, PyList>> {
    let out = PyList::empty(py);
    for s in summaries {
        out.append(summary_to_dict(py, s)?)?;
    }
    Ok(out)
}

// =========================================================================
// PyRegistryClient
// =========================================================================

/// Client for the `aggregator.registry` RPC service. Construct
/// against a live `NetMesh`; every operation issues a synchronous
/// RPC against the named target node.
#[pyclass(name = "RegistryClient", module = "net._net")]
pub struct PyRegistryClient {
    inner: Arc<RwLock<SdkRegistryClient>>,
    runtime: Arc<Runtime>,
}

#[pymethods]
impl PyRegistryClient {
    #[new]
    fn new(mesh: &crate::mesh_bindings::NetMesh) -> PyResult<Self> {
        let mesh_arc = mesh.node_arc_clone()?;
        Ok(Self {
            inner: Arc::new(RwLock::new(SdkRegistryClient::new(mesh_arc))),
            runtime: mesh.runtime_arc(),
        })
    }

    /// Override the per-call deadline in milliseconds. Mutates
    /// `self` in place and returns it for chaining.
    fn with_deadline(slf: PyRef<'_, Self>, millis: u64) -> PyRef<'_, Self> {
        slf.inner
            .write()
            .set_deadline_mut(Duration::from_millis(millis));
        slf
    }

    /// Enumerate groups on `target_node_id`.
    fn list<'py>(&self, py: Python<'py>, target_node_id: u64) -> PyResult<Bound<'py, PyList>> {
        let inner_snapshot = self.inner.read().clone();
        let runtime = self.runtime.clone();
        let result = py
            .detach(|| runtime.block_on(async move { inner_snapshot.list(target_node_id).await }));
        match result {
            Ok(groups) => groups_to_list(py, &groups),
            Err(e) => Err(registry_err(py, e)),
        }
    }

    /// Spawn a new group by referencing a daemon-side template.
    fn spawn<'py>(
        &self,
        py: Python<'py>,
        target_node_id: u64,
        template_name: String,
        group_name: String,
        replica_count: u8,
    ) -> PyResult<Bound<'py, PyDict>> {
        let inner_snapshot = self.inner.read().clone();
        let runtime = self.runtime.clone();
        let result = py.detach(|| {
            runtime.block_on(async move {
                inner_snapshot
                    .spawn(target_node_id, template_name, group_name, replica_count)
                    .await
            })
        });
        match result {
            Ok(g) => group_to_dict(py, &g),
            Err(e) => Err(registry_err(py, e)),
        }
    }

    /// Tear down a registered group by name. Returns `True` when
    /// the group was found and removed; `False` when no group
    /// matched.
    fn unregister(
        &self,
        py: Python<'_>,
        target_node_id: u64,
        group_name: String,
    ) -> PyResult<bool> {
        let inner_snapshot = self.inner.read().clone();
        let runtime = self.runtime.clone();
        let result = py.detach(|| {
            runtime.block_on(
                async move { inner_snapshot.unregister(target_node_id, group_name).await },
            )
        });
        result.map_err(|e| registry_err(py, e))
    }

    fn __repr__(&self) -> String {
        format!("RegistryClient(inner={:?})", Arc::as_ptr(&self.inner))
    }
}

// =========================================================================
// PyFoldQueryClient
// =========================================================================

/// Client for the `fold.query` RPC service. Caches recent
/// `QueryLatest` responses by `(target, kind)` with a configurable
/// TTL; `SummarizeNow` always goes to the wire.
#[pyclass(name = "FoldQueryClient", module = "net._net")]
pub struct PyFoldQueryClient {
    inner: Arc<RwLock<SdkFoldQueryClient>>,
    runtime: Arc<Runtime>,
}

#[pymethods]
impl PyFoldQueryClient {
    #[new]
    fn new(mesh: &crate::mesh_bindings::NetMesh) -> PyResult<Self> {
        let mesh_arc = mesh.node_arc_clone()?;
        Ok(Self {
            inner: Arc::new(RwLock::new(SdkFoldQueryClient::new(mesh_arc))),
            runtime: mesh.runtime_arc(),
        })
    }

    /// Override the cache TTL in milliseconds. `0` disables the
    /// cache entirely. Warmed cache survives the adjustment.
    fn with_ttl(slf: PyRef<'_, Self>, millis: u64) -> PyRef<'_, Self> {
        slf.inner.write().set_ttl_mut(Duration::from_millis(millis));
        slf
    }

    /// Override the per-call deadline in milliseconds.
    fn with_deadline(slf: PyRef<'_, Self>, millis: u64) -> PyRef<'_, Self> {
        slf.inner
            .write()
            .set_deadline_mut(Duration::from_millis(millis));
        slf
    }

    /// Query the aggregator's latest cached summaries. Cache hit
    /// returns immediately; miss issues a wire RPC, caches the
    /// response, and returns.
    fn query_latest<'py>(
        &self,
        py: Python<'py>,
        target_node_id: u64,
        kind: u16,
    ) -> PyResult<Bound<'py, PyList>> {
        let inner_snapshot = self.inner.read().clone();
        let runtime = self.runtime.clone();
        let result = py.detach(|| {
            runtime.block_on(async move { inner_snapshot.query_latest(target_node_id, kind).await })
        });
        match result {
            Ok(summaries) => summaries_to_list(py, &summaries),
            Err(e) => Err(fold_query_err(py, e)),
        }
    }

    /// Force a fresh `SummarizeNow` query — never cached.
    fn query_summarize_now<'py>(
        &self,
        py: Python<'py>,
        target_node_id: u64,
        kind: u16,
    ) -> PyResult<Bound<'py, PyList>> {
        let inner_snapshot = self.inner.read().clone();
        let runtime = self.runtime.clone();
        let result = py.detach(|| {
            runtime.block_on(async move {
                inner_snapshot
                    .query_summarize_now(target_node_id, kind)
                    .await
            })
        });
        match result {
            Ok(summaries) => summaries_to_list(py, &summaries),
            Err(e) => Err(fold_query_err(py, e)),
        }
    }

    /// Drop every cached entry. Use after a topology change so the
    /// next query re-resolves.
    fn invalidate_cache(&self) {
        self.inner.read().invalidate_cache();
    }

    /// Drop only entries matching `target_node_id`.
    fn invalidate_target(&self, target_node_id: u64) {
        self.inner.read().invalidate_target(target_node_id);
    }

    fn __repr__(&self) -> String {
        format!("FoldQueryClient(inner={:?})", Arc::as_ptr(&self.inner))
    }
}
