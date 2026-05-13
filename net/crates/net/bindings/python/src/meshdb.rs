//! Python bindings for MeshDB — federated query layer.
//!
//! # Slice 1 + 2 scope
//!
//! Slice 1 shipped the minimum end-to-end (atomic factories,
//! in-memory `ChainReader`, sync runner, Phase F cache options).
//! Slice 2 expands the factory surface to cover composite
//! operators + adds result-side decoders so callers can read
//! the sentinel-row payloads emitted by aggregate / window /
//! join operators.
//!
//! - [`PyMeshQuery`] — 1:1-with-AST factory surface.
//!   - Slice 1: `at`, `between`, `latest`.
//!   - Slice 2: `count`, `sum`, `avg`, `min`, `max`,
//!     `distinct_count`, `percentile`, `window`, `join`.
//!   - Deferred: `filter` (needs a Predicate Python surface),
//!     `lineage_back` / `lineage_forward` (need a CapabilityIndex
//!     plumbed through the runner).
//! - [`PyInMemoryChainReader`] — Python-facing in-memory
//!   `ChainReader` impl. Lets Python code `.append(origin, seq,
//!   payload)` then run queries against the resulting fixture.
//!   Phase B+ adds a `from_redex(...)` adapter.
//! - [`PyMeshQueryRunner`] — owns a `LocalMeshQueryExecutor` plus
//!   an in-process Tokio runtime. `.execute(query, options)` drains
//!   the row stream synchronously and returns a `list[ResultRow]`
//!   (locked decision: Python is sync-first; async wrapper is a
//!   follow-up).
//! - [`PyResultRow`] — `(origin: int, seq: int, payload: bytes)`
//!   plus `.decode_aggregate()` / `.decode_joined()` /
//!   `.decode_window()` helpers (slice 2) that postcard-decode
//!   the sentinel-row payloads.
//! - [`PyAggregateResult`] / [`PyGroupKey`] / [`PyJoinedRow`] /
//!   [`PyWindowBoundary`] — slice 2 decoder pyclasses.
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
//! is slice 4 per the locked roadmap; slices 1 + 2 stay
//! factory-only so the surface lands tight.
//!
//! # Async
//!
//! Sync only — `runner.execute(...)` drains into a list. Locked
//! decision: Python sync-first; pyo3-asyncio support is a later
//! slice when a consumer needs it.

use std::sync::{Arc, Mutex};

use pyo3::create_exception;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use tokio::runtime::Runtime;

use net::adapter::net::behavior::meshdb::MeshError;
use net::adapter::net::behavior::meshdb::{
    cache::{CachePolicy, LruResultCache},
    executor::{ChainReader, ExecuteOptions, LocalMeshQueryExecutor, MeshQueryExecutor},
    planner::{
        CostEstimate, JoinKeyMode, JoinStrategy, LineageDirection, LineageEntry, OperatorNode,
        OperatorPlan,
    },
    query::{
        AggregateRowPayload, AggregateValue, GroupKey, JoinKind, JoinedRowPayload,
        NumericAggregateKind, NumericReductionKind, ResultRow, WindowBoundary, WindowSpec,
    },
    ExecutionPlan, SeqNum,
};
use net::adapter::net::behavior::predicate::Predicate;
use net::adapter::net::behavior::tag::{TagKey, TaxonomyAxis};

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

    /// Try to decode this row's payload as an aggregate
    /// payload. Returns `None` for rows that aren't aggregate
    /// sentinels (e.g. raw At/Between/Latest rows whose
    /// payload is event data, or join / window sentinels).
    fn decode_aggregate(&self) -> Option<PyAggregateResult> {
        let payload: AggregateRowPayload = postcard::from_bytes(&self.payload).ok()?;
        Some(PyAggregateResult::from(payload))
    }

    /// Try to decode this row's payload as a joined-row
    /// payload. Returns `None` when the bytes don't
    /// deserialize as a JoinedRow.
    fn decode_joined(&self) -> Option<PyJoinedRow> {
        let payload: JoinedRowPayload = postcard::from_bytes(&self.payload).ok()?;
        Some(PyJoinedRow::from(payload))
    }

    /// Try to decode this row's payload as a window bucket.
    /// Returns `None` when the bytes don't deserialize as a
    /// WindowBoundary.
    fn decode_window(&self) -> Option<PyWindowBoundary> {
        let boundary: WindowBoundary = postcard::from_bytes(&self.payload).ok()?;
        Some(PyWindowBoundary::from(boundary))
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

/// Decoded aggregate-row payload. `group` is `None` for
/// single-bucket aggregates; otherwise it identifies the
/// group via origin / seq / both. `kind` names which
/// aggregate function ran (`"count"` / `"sum"` / `"avg"` /
/// `"min"` / `"max"` / `"distinct_count"` / `"percentile"`);
/// `value` is the numeric output (always set for count /
/// distinct_count; `None` for the others when the group held
/// no numeric rows). `count` mirrors `value` as an integer
/// for the count-flavored kinds — convenience accessor so
/// Python callers don't have to coerce floats.
#[pyclass(name = "AggregateResult", module = "net._net", frozen, from_py_object)]
#[derive(Clone)]
pub struct PyAggregateResult {
    #[pyo3(get)]
    group: Option<PyGroupKey>,
    #[pyo3(get)]
    kind: String,
    #[pyo3(get)]
    value: Option<f64>,
    #[pyo3(get)]
    count: Option<u64>,
}

#[pymethods]
impl PyAggregateResult {
    fn __repr__(&self) -> String {
        let group = self
            .group
            .as_ref()
            .map(|g| g.__repr__())
            .unwrap_or_else(|| "None".to_string());
        let value_str = match (self.value, self.count) {
            (None, Some(c)) => c.to_string(),
            (Some(v), None) => format!("{v}"),
            (Some(v), Some(_)) => format!("{v}"),
            (None, None) => "None".to_string(),
        };
        format!(
            "AggregateResult(kind={:?}, group={group}, value={value_str})",
            self.kind
        )
    }
}

impl From<AggregateRowPayload> for PyAggregateResult {
    fn from(p: AggregateRowPayload) -> Self {
        let group = p.group.map(PyGroupKey::from);
        let (kind, value, count) = match p.value {
            AggregateValue::Count(c) => ("count".to_string(), Some(c as f64), Some(c)),
            AggregateValue::Sum(s) => ("sum".to_string(), Some(s), None),
            AggregateValue::Avg(opt) => ("avg".to_string(), opt, None),
            AggregateValue::Min(opt) => ("min".to_string(), opt, None),
            AggregateValue::Max(opt) => ("max".to_string(), opt, None),
            AggregateValue::DistinctCount(c) => {
                ("distinct_count".to_string(), Some(c as f64), Some(c))
            }
            AggregateValue::Percentile(opt) => ("percentile".to_string(), opt, None),
            // AggregateValue is #[non_exhaustive]; any future
            // variant surfaces as an "unknown" kind so the
            // wire round-trip still works.
            _ => ("unknown".to_string(), None, None),
        };
        Self {
            group,
            kind,
            value,
            count,
        }
    }
}

/// Decoded group-key identifier carried inside an
/// [`PyAggregateResult`]. `kind` is `"origin"` / `"seq"` /
/// `"origin_seq"`; the populated field(s) match the kind.
#[pyclass(name = "GroupKey", module = "net._net", frozen, from_py_object)]
#[derive(Clone)]
pub struct PyGroupKey {
    #[pyo3(get)]
    kind: String,
    #[pyo3(get)]
    origin: Option<u64>,
    #[pyo3(get)]
    seq: Option<u64>,
}

#[pymethods]
impl PyGroupKey {
    fn __repr__(&self) -> String {
        match self.kind.as_str() {
            "origin" => format!("GroupKey(origin={:#x})", self.origin.unwrap_or(0)),
            "seq" => format!("GroupKey(seq={})", self.seq.unwrap_or(0)),
            "origin_seq" => format!(
                "GroupKey(origin={:#x}, seq={})",
                self.origin.unwrap_or(0),
                self.seq.unwrap_or(0)
            ),
            other => format!("GroupKey(<{other}>)"),
        }
    }
}

impl From<GroupKey> for PyGroupKey {
    fn from(g: GroupKey) -> Self {
        match g {
            GroupKey::Origin(o) => Self {
                kind: "origin".to_string(),
                origin: Some(o),
                seq: None,
            },
            GroupKey::Seq(s) => Self {
                kind: "seq".to_string(),
                origin: None,
                seq: Some(s.0),
            },
            GroupKey::OriginSeq { origin, seq } => Self {
                kind: "origin_seq".to_string(),
                origin: Some(origin),
                seq: Some(seq.0),
            },
        }
    }
}

/// Decoded join-row payload. `left` / `right` are the source
/// rows from each side of the join; either side is `None` for
/// outer-join unmatched rows. Inner-join rows always have both
/// `Some`.
#[pyclass(name = "JoinedRow", module = "net._net", frozen, from_py_object)]
#[derive(Clone)]
pub struct PyJoinedRow {
    #[pyo3(get)]
    left: Option<PyResultRow>,
    #[pyo3(get)]
    right: Option<PyResultRow>,
}

#[pymethods]
impl PyJoinedRow {
    fn __repr__(&self) -> String {
        let l = self
            .left
            .as_ref()
            .map(|r| r.__repr__())
            .unwrap_or_else(|| "None".to_string());
        let r = self
            .right
            .as_ref()
            .map(|r| r.__repr__())
            .unwrap_or_else(|| "None".to_string());
        format!("JoinedRow(left={l}, right={r})")
    }
}

impl From<JoinedRowPayload> for PyJoinedRow {
    fn from(p: JoinedRowPayload) -> Self {
        Self {
            left: p.left.map(PyResultRow::from),
            right: p.right.map(PyResultRow::from),
        }
    }
}

/// Decoded window-bucket payload. `start` and `end` are the
/// bucket's seq bounds (half-open); `rows` is the list of
/// rows that landed in the bucket, in seq-asc order.
#[pyclass(name = "WindowBoundary", module = "net._net", frozen, from_py_object)]
#[derive(Clone)]
pub struct PyWindowBoundary {
    #[pyo3(get)]
    start: u64,
    #[pyo3(get)]
    end: u64,
    #[pyo3(get)]
    rows: Vec<PyResultRow>,
}

#[pymethods]
impl PyWindowBoundary {
    fn __repr__(&self) -> String {
        format!(
            "WindowBoundary(start={}, end={}, rows=<{} rows>)",
            self.start,
            self.end,
            self.rows.len()
        )
    }
}

impl From<WindowBoundary> for PyWindowBoundary {
    fn from(b: WindowBoundary) -> Self {
        Self {
            start: b.start.0,
            end: b.end.0,
            rows: b.rows.into_iter().map(PyResultRow::from).collect(),
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
                cache_policy: cache_policy.map(|p| p.inner).unwrap_or_default(),
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

/// Predicate constructor for [`PyMeshQuery::filter`]. Wraps
/// the substrate's [`Predicate`] enum; factory methods mirror
/// the variants useful for row filtering against the synthetic
/// per-row tag view (`row::synthetic_row_view`).
///
/// Field paths target the synthetic `Dataforts` axis: a
/// row-intrinsic name like `"origin"` / `"seq"` resolves to
/// the same key used by the join/aggregate keying surface; a
/// JSON path like `"severity"` or `"a.b.c"` resolves to the
/// flattened JSON-object payload field.
///
/// And / Or / Not compose by passing already-built
/// [`PyPredicate`]s in via factory methods (Python doesn't get
/// operator overloading in slice 3; if `&` / `|` / `~`
/// ergonomics matter, slice 4 can layer them on).
#[pyclass(name = "Predicate", module = "net._net", frozen, from_py_object)]
#[derive(Clone)]
pub struct PyPredicate {
    inner: Predicate,
}

#[pymethods]
impl PyPredicate {
    /// `field` is present (any value).
    #[staticmethod]
    fn exists(field: String) -> Self {
        Self {
            inner: Predicate::Exists {
                key: tag_key(&field),
            },
        }
    }

    /// `field == value` (string equality).
    #[staticmethod]
    fn equals(field: String, value: String) -> Self {
        Self {
            inner: Predicate::Equals {
                key: tag_key(&field),
                value,
            },
        }
    }

    /// `field >= threshold` (numeric).
    #[staticmethod]
    fn numeric_at_least(field: String, threshold: f64) -> Self {
        Self {
            inner: Predicate::NumericAtLeast {
                key: tag_key(&field),
                threshold,
            },
        }
    }

    /// `field <= threshold` (numeric).
    #[staticmethod]
    fn numeric_at_most(field: String, threshold: f64) -> Self {
        Self {
            inner: Predicate::NumericAtMost {
                key: tag_key(&field),
                threshold,
            },
        }
    }

    /// `min <= field <= max` (numeric, both bounds inclusive).
    #[staticmethod]
    fn numeric_in_range(field: String, min: f64, max: f64) -> PyResult<Self> {
        if !(min.is_finite() && max.is_finite()) || min > max {
            return Err(MeshDbError::new_err(format!(
                "numeric_in_range: requires finite min <= max (got min={min}, max={max})"
            )));
        }
        Ok(Self {
            inner: Predicate::NumericInRange {
                key: tag_key(&field),
                min,
                max,
            },
        })
    }

    /// `field.startswith(prefix)` (string).
    #[staticmethod]
    fn string_prefix(field: String, prefix: String) -> Self {
        Self {
            inner: Predicate::StringPrefix {
                key: tag_key(&field),
                prefix,
            },
        }
    }

    /// `pattern in field` (substring; semantics match the
    /// substrate's `Predicate::StringMatches` Phase E note —
    /// substring-only today, regex behind a feature flag
    /// later).
    #[staticmethod]
    fn string_matches(field: String, pattern: String) -> Self {
        Self {
            inner: Predicate::StringMatches {
                key: tag_key(&field),
                pattern,
            },
        }
    }

    /// `field >= version` (semver).
    #[staticmethod]
    fn semver_at_least(field: String, version: String) -> Self {
        Self {
            inner: Predicate::SemverAtLeast {
                key: tag_key(&field),
                version,
            },
        }
    }

    /// Conjunction. Empty list evaluates to `True` (vacuous
    /// match — mirrors the substrate semantics).
    #[staticmethod]
    #[pyo3(name = "and_")]
    fn and_(predicates: Vec<PyPredicate>) -> Self {
        Self {
            inner: Predicate::And(predicates.into_iter().map(|p| p.inner).collect()),
        }
    }

    /// Disjunction. Empty list evaluates to `False` (vacuous
    /// miss).
    #[staticmethod]
    #[pyo3(name = "or_")]
    fn or_(predicates: Vec<PyPredicate>) -> Self {
        Self {
            inner: Predicate::Or(predicates.into_iter().map(|p| p.inner).collect()),
        }
    }

    /// Negation.
    #[staticmethod]
    #[pyo3(name = "not_")]
    fn not_(predicate: PyPredicate) -> Self {
        Self {
            inner: Predicate::Not(Box::new(predicate.inner)),
        }
    }

    fn __repr__(&self) -> String {
        format!("Predicate({:?})", self.inner)
    }
}

/// Build a [`TagKey`] keyed on the synthetic `Dataforts` axis.
/// Every Python-facing predicate field path is rooted here
/// because `row::synthetic_row_view` populates every leaf as
/// a `Dataforts`-axis tag — see the MeshDB row module.
fn tag_key(field: &str) -> TagKey {
    TagKey {
        axis: TaxonomyAxis::Dataforts,
        key: field.to_string(),
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
        Self { plan: plan_of(op) }
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
        Ok(Self { plan: plan_of(op) })
    }

    /// Read the tip event from chain `origin`.
    #[staticmethod]
    fn latest(origin: u64) -> Self {
        let op = OperatorPlan::LatestRead { origin };
        Self { plan: plan_of(op) }
    }

    /// Start a fluent builder. Equivalent to constructing a
    /// fresh [`PyQueryBuilder`]; chainable methods (`at`,
    /// `between`, `latest`, `filter`, `count`, `sum`, `avg`,
    /// `min`, `max`, `percentile`, `distinct_count`, `window`,
    /// `join`) compose into a final [`PyMeshQuery`] via
    /// `.build()`.
    #[staticmethod]
    fn builder() -> PyQueryBuilder {
        PyQueryBuilder { state: None }
    }

    /// Filter `inner`'s rows by `predicate`. The executor builds
    /// a synthetic per-row tag view (origin / seq / flat JSON
    /// payload fields) and evaluates the predicate; rows whose
    /// evaluation returns `True` pass through unchanged. Rows
    /// whose payload isn't JSON are still filterable by their
    /// row-intrinsic fields (`origin`, `seq`); payload field
    /// references against a non-JSON payload simply don't
    /// match.
    #[staticmethod]
    fn filter(inner: &PyMeshQuery, predicate: &PyPredicate) -> Self {
        let op = OperatorPlan::Filter {
            input: Box::new(inner.plan.root.clone()),
            predicate: predicate.inner.to_wire(),
        };
        Self { plan: plan_of(op) }
    }

    /// Tumbling window on `seq` with the given bucket `size`.
    /// Emits one sentinel row per non-empty bucket; decode the
    /// payload with `ResultRow.decode_window()`.
    #[staticmethod]
    fn window(inner: &PyMeshQuery, size: u64) -> PyResult<Self> {
        if size == 0 {
            return Err(MeshDbError::new_err(
                "window: size must be >= 1".to_string(),
            ));
        }
        let op = OperatorPlan::Window {
            input: Box::new(inner.plan.root.clone()),
            spec: WindowSpec::TumblingSeq { size },
        };
        Ok(Self { plan: plan_of(op) })
    }

    /// Count rows. `group_by` is an optional list of row-
    /// intrinsic field names: `None` / `[]` = single bucket;
    /// `["origin"]`, `["seq"]`, or `["origin", "seq"]` for
    /// per-group counts.
    #[staticmethod]
    #[pyo3(signature = (inner, group_by=None))]
    fn count(inner: &PyMeshQuery, group_by: Option<Vec<String>>) -> PyResult<Self> {
        let group_by = parse_group_by(group_by)?;
        let op = OperatorPlan::AggregateCount {
            input: Box::new(inner.plan.root.clone()),
            group_by,
        };
        Ok(Self { plan: plan_of(op) })
    }

    /// Sum of a numeric field across rows. `field` is a row-
    /// intrinsic name (`"origin"` / `"seq"`) or a dotted JSON
    /// path; see `MeshDB row::extract_numeric`.
    #[staticmethod]
    #[pyo3(signature = (inner, field, group_by=None))]
    fn sum(inner: &PyMeshQuery, field: String, group_by: Option<Vec<String>>) -> PyResult<Self> {
        Self::numeric_agg(inner, field, NumericAggregateKind::Sum, group_by)
    }

    /// Arithmetic mean across rows whose field resolves to a
    /// number. Rows where the field is missing / non-numeric
    /// are excluded from both numerator and denominator.
    #[staticmethod]
    #[pyo3(signature = (inner, field, group_by=None))]
    fn avg(inner: &PyMeshQuery, field: String, group_by: Option<Vec<String>>) -> PyResult<Self> {
        Self::numeric_agg(inner, field, NumericAggregateKind::Avg, group_by)
    }

    /// Min / Max / nearest-rank exact percentile. See
    /// [`MeshQuery.percentile`] for the percentile-with-`p`
    /// helper.
    #[staticmethod]
    #[pyo3(signature = (inner, field, group_by=None))]
    fn min(inner: &PyMeshQuery, field: String, group_by: Option<Vec<String>>) -> PyResult<Self> {
        Self::reduction(inner, field, NumericReductionKind::Min, group_by)
    }

    #[staticmethod]
    #[pyo3(signature = (inner, field, group_by=None))]
    fn max(inner: &PyMeshQuery, field: String, group_by: Option<Vec<String>>) -> PyResult<Self> {
        Self::reduction(inner, field, NumericReductionKind::Max, group_by)
    }

    /// Nearest-rank exact percentile at `p ∈ [0.0, 1.0]`. Same
    /// field-extraction semantics as the numeric aggregates.
    #[staticmethod]
    #[pyo3(signature = (inner, field, p, group_by=None))]
    fn percentile(
        inner: &PyMeshQuery,
        field: String,
        p: f64,
        group_by: Option<Vec<String>>,
    ) -> PyResult<Self> {
        if !p.is_finite() || !(0.0..=1.0).contains(&p) {
            return Err(MeshDbError::new_err(format!(
                "percentile: p must be in [0.0, 1.0], got {p}"
            )));
        }
        Self::reduction(
            inner,
            field,
            NumericReductionKind::Percentile { p },
            group_by,
        )
    }

    /// Exact distinct count over the canonical string
    /// projection of a row-intrinsic / JSON field. Bounded by
    /// the executor's per-query memory budget.
    #[staticmethod]
    #[pyo3(signature = (inner, field, group_by=None))]
    fn distinct_count(
        inner: &PyMeshQuery,
        field: String,
        group_by: Option<Vec<String>>,
    ) -> PyResult<Self> {
        let group_by = parse_group_by(group_by)?;
        let op = OperatorPlan::AggregateDistinct {
            input: Box::new(inner.plan.root.clone()),
            group_by,
            field_path: field,
        };
        Ok(Self { plan: plan_of(op) })
    }

    /// Inner / outer hash-join over row-intrinsic or JSON
    /// payload keys. `kind` is one of `"inner"`,
    /// `"left_outer"`, `"right_outer"`, `"full_outer"`.
    /// `key` is the field name both sides share — row-
    /// intrinsic names map to the typed enum
    /// (`origin` / `seq` / `origin,seq`); anything else is
    /// treated as a JSON payload path. `strategy` is
    /// `"hash_broadcast"` (default) or `"sort_merge"`.
    /// Emit a pre-walked lineage as one ResultRow per entry.
    /// `entries` is a list of `LineageEntry` describing the
    /// chains reached during the walk (typically: index 0 is
    /// the start origin with `depth = 0`, ancestors / descendants
    /// follow). `direction` is `"back"` or `"forward"`.
    ///
    /// The walk itself is the caller's responsibility (SDK
    /// consumers maintain their own fork-of: graph view, or
    /// call into a future SDK-side walker). The executor just
    /// emits rows; each `entry` produces a `ResultRow` with
    /// `origin = entry.origin`, `seq = entry.tip_seq or 0`,
    /// payload empty. Callers compose with `at` / `between`
    /// to fetch event content for each ancestor.
    #[staticmethod]
    #[pyo3(signature = (origin, entries, direction))]
    fn lineage_emit(origin: u64, entries: Vec<PyLineageEntry>, direction: &str) -> PyResult<Self> {
        let direction = parse_lineage_direction(direction)?;
        let entries: Vec<LineageEntry> = entries
            .into_iter()
            .map(|e| LineageEntry {
                origin: e.origin,
                depth: e.depth,
                tip_seq: e.tip_seq.map(SeqNum),
            })
            .collect();
        let op = OperatorPlan::LineageEmit {
            origin,
            direction,
            entries,
        };
        Ok(Self { plan: plan_of(op) })
    }

    /// `watermark_secs` is informational under snapshot
    /// semantics; kept on the operator for wire round-trip.
    #[staticmethod]
    #[pyo3(signature = (left, right, kind, key, strategy=None, watermark_secs=5.0))]
    fn join(
        left: &PyMeshQuery,
        right: &PyMeshQuery,
        kind: &str,
        key: &str,
        strategy: Option<&str>,
        watermark_secs: f64,
    ) -> PyResult<Self> {
        let kind = parse_join_kind(kind)?;
        let strategy = parse_join_strategy(strategy)?;
        // Canonical join-key keywords across all shims:
        // "origin", "seq", "origin,seq". Anything else is
        // treated as a dotted JSON field path. The variants
        // "origin+seq" / "seq,origin" were tolerated in
        // earlier slices but are now rejected — cross-language
        // conformance tests need one canonical encoding.
        let key_mode = match key {
            "origin" => JoinKeyMode::Origin,
            "seq" => JoinKeyMode::Seq,
            "origin,seq" => JoinKeyMode::OriginSeq,
            other => JoinKeyMode::Field(other.to_string()),
        };
        let watermark = if watermark_secs.is_finite() && watermark_secs >= 0.0 {
            std::time::Duration::from_secs_f64(watermark_secs)
        } else {
            std::time::Duration::from_secs(5)
        };
        let op = OperatorPlan::HashJoin {
            left: Box::new(left.plan.root.clone()),
            right: Box::new(right.plan.root.clone()),
            key_mode,
            kind,
            strategy,
            watermark,
        };
        Ok(Self { plan: plan_of(op) })
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
            OperatorPlan::Window { spec, .. } => match spec {
                WindowSpec::TumblingSeq { size } => format!("MeshQuery.window(size={size})"),
                _ => "MeshQuery.window(<unknown>)".to_string(),
            },
            OperatorPlan::AggregateCount { .. } => "MeshQuery.count(...)".to_string(),
            OperatorPlan::AggregateNumeric { kind, field_path, .. } => match kind {
                NumericAggregateKind::Sum => format!("MeshQuery.sum(field={field_path:?})"),
                NumericAggregateKind::Avg => format!("MeshQuery.avg(field={field_path:?})"),
            },
            OperatorPlan::AggregateReduction {
                kind, field_path, ..
            } => match kind {
                NumericReductionKind::Min => format!("MeshQuery.min(field={field_path:?})"),
                NumericReductionKind::Max => format!("MeshQuery.max(field={field_path:?})"),
                NumericReductionKind::Percentile { p } => {
                    format!("MeshQuery.percentile(field={field_path:?}, p={p})")
                }
            },
            OperatorPlan::AggregateDistinct { field_path, .. } => {
                format!("MeshQuery.distinct_count(field={field_path:?})")
            }
            OperatorPlan::HashJoin { kind, .. } => {
                format!("MeshQuery.join(kind={kind:?})")
            }
            OperatorPlan::LineageEmit {
                origin,
                direction,
                entries,
            } => format!(
                "MeshQuery.lineage_emit(origin={origin:#x}, direction={direction:?}, entries=<{} entries>)",
                entries.len()
            ),
            OperatorPlan::Filter { .. } => "MeshQuery.filter(...)".to_string(),
            // Slice 2 doesn't yet expose factories for these
            // (Filter needs a Predicate surface; LineageBack /
            // LineageForward need a CapabilityIndex). The
            // variants are reachable via wire round-trip / the
            // builder API in slice 3 / 4 but the factory
            // surface above doesn't produce them yet.
            other => format!("MeshQuery(<{other:?}>)"),
        }
    }
}

impl PyMeshQuery {
    fn numeric_agg(
        inner: &PyMeshQuery,
        field: String,
        kind: NumericAggregateKind,
        group_by: Option<Vec<String>>,
    ) -> PyResult<Self> {
        let group_by = parse_group_by(group_by)?;
        let op = OperatorPlan::AggregateNumeric {
            input: Box::new(inner.plan.root.clone()),
            group_by,
            field_path: field,
            kind,
        };
        Ok(Self { plan: plan_of(op) })
    }

    fn reduction(
        inner: &PyMeshQuery,
        field: String,
        kind: NumericReductionKind,
        group_by: Option<Vec<String>>,
    ) -> PyResult<Self> {
        let group_by = parse_group_by(group_by)?;
        let op = OperatorPlan::AggregateReduction {
            input: Box::new(inner.plan.root.clone()),
            group_by,
            field_path: field,
            kind,
        };
        Ok(Self { plan: plan_of(op) })
    }
}

/// Parse a Python `group_by: list[str] | None` into the
/// planner's `Option<JoinKeyMode>`. `None` / `[]` → `None`
/// (single-bucket); `["origin"]` → `Origin`; `["seq"]` →
/// `Seq`; `["origin", "seq"]` (any order) → `OriginSeq`.
/// Other shapes raise a `MeshDbError` with the same Phase E-1
/// message the planner would surface.
fn parse_group_by(group_by: Option<Vec<String>>) -> PyResult<Option<JoinKeyMode>> {
    let Some(group_by) = group_by else {
        return Ok(None);
    };
    if group_by.is_empty() {
        return Ok(None);
    }
    if group_by.len() == 1 {
        return match group_by[0].as_str() {
            "origin" => Ok(Some(JoinKeyMode::Origin)),
            "seq" => Ok(Some(JoinKeyMode::Seq)),
            other => Err(MeshDbError::new_err(format!(
                "group_by field '{other}' is not a row-intrinsic key; only 'origin' / 'seq' supported"
            ))),
        };
    }
    if group_by.len() == 2
        && group_by[0].as_str() == "origin"
        && group_by[1].as_str() == "seq"
    {
        return Ok(Some(JoinKeyMode::OriginSeq));
    }
    Err(MeshDbError::new_err(format!(
        "group_by shape {group_by:?} not supported; use [], ['origin'], ['seq'], or ['origin', 'seq']"
    )))
}

fn parse_join_kind(s: &str) -> PyResult<JoinKind> {
    match s {
        "inner" => Ok(JoinKind::Inner),
        "left_outer" => Ok(JoinKind::LeftOuter),
        "right_outer" => Ok(JoinKind::RightOuter),
        "full_outer" => Ok(JoinKind::FullOuter),
        other => Err(MeshDbError::new_err(format!(
            "join kind '{other}' not recognised; expected one of: inner, left_outer, right_outer, full_outer"
        ))),
    }
}

fn parse_join_strategy(s: Option<&str>) -> PyResult<JoinStrategy> {
    match s {
        None | Some("hash_broadcast") => Ok(JoinStrategy::HashBroadcast),
        Some("sort_merge") => Ok(JoinStrategy::SortMerge),
        Some(other) => Err(MeshDbError::new_err(format!(
            "join strategy '{other}' not recognised; expected one of: hash_broadcast, sort_merge"
        ))),
    }
}

/// Fluent builder for the common-ops query shape. Each
/// chainable method returns a fresh builder so Python users
/// can write `MeshQuery.builder().between(...).filter(...).count()`.
///
/// Source operators (`at` / `between` / `latest`) seed the
/// pipeline; pipeline operators (`filter` / `count` / numeric
/// aggregates / `window` / `join`) require a seeded state and
/// surface a `MeshDbError` when called on an empty builder.
/// `.build()` consumes the builder into a [`PyMeshQuery`].
///
/// Per the locked Phase F builder scope, this surface only
/// covers the common ops — the rarer operators (`Lineage*`,
/// payload-keyed grouping) still go through the
/// [`PyMeshQuery`] factory methods.
#[pyclass(name = "QueryBuilder", module = "net._net", from_py_object)]
#[derive(Clone)]
pub struct PyQueryBuilder {
    state: Option<PyMeshQuery>,
}

#[pymethods]
impl PyQueryBuilder {
    /// Source: read a single event at `seq`. Resets any prior
    /// builder state.
    fn at(&self, origin: u64, seq: u64) -> Self {
        Self {
            state: Some(PyMeshQuery::at(origin, seq)),
        }
    }

    /// Source: read events in the half-open seq range. Resets
    /// any prior builder state.
    fn between(&self, origin: u64, start: u64, end: u64) -> PyResult<Self> {
        Ok(Self {
            state: Some(PyMeshQuery::between(origin, start, end)?),
        })
    }

    /// Source: read the tip event. Resets any prior builder
    /// state.
    fn latest(&self, origin: u64) -> Self {
        Self {
            state: Some(PyMeshQuery::latest(origin)),
        }
    }

    /// Filter the current pipeline's rows by `predicate`.
    fn filter(&self, predicate: &PyPredicate) -> PyResult<Self> {
        let inner = self.require_state("filter")?;
        Ok(Self {
            state: Some(PyMeshQuery::filter(&inner, predicate)),
        })
    }

    /// Count rows in the current pipeline.
    #[pyo3(signature = (group_by=None))]
    fn count(&self, group_by: Option<Vec<String>>) -> PyResult<Self> {
        let inner = self.require_state("count")?;
        Ok(Self {
            state: Some(PyMeshQuery::count(&inner, group_by)?),
        })
    }

    /// Sum / Avg / Min / Max / Percentile / DistinctCount over
    /// the current pipeline. Same signatures as
    /// [`PyMeshQuery`]'s factories.
    #[pyo3(signature = (field, group_by=None))]
    fn sum(&self, field: String, group_by: Option<Vec<String>>) -> PyResult<Self> {
        let inner = self.require_state("sum")?;
        Ok(Self {
            state: Some(PyMeshQuery::sum(&inner, field, group_by)?),
        })
    }

    #[pyo3(signature = (field, group_by=None))]
    fn avg(&self, field: String, group_by: Option<Vec<String>>) -> PyResult<Self> {
        let inner = self.require_state("avg")?;
        Ok(Self {
            state: Some(PyMeshQuery::avg(&inner, field, group_by)?),
        })
    }

    #[pyo3(signature = (field, group_by=None))]
    fn min(&self, field: String, group_by: Option<Vec<String>>) -> PyResult<Self> {
        let inner = self.require_state("min")?;
        Ok(Self {
            state: Some(PyMeshQuery::min(&inner, field, group_by)?),
        })
    }

    #[pyo3(signature = (field, group_by=None))]
    fn max(&self, field: String, group_by: Option<Vec<String>>) -> PyResult<Self> {
        let inner = self.require_state("max")?;
        Ok(Self {
            state: Some(PyMeshQuery::max(&inner, field, group_by)?),
        })
    }

    #[pyo3(signature = (field, p, group_by=None))]
    fn percentile(&self, field: String, p: f64, group_by: Option<Vec<String>>) -> PyResult<Self> {
        let inner = self.require_state("percentile")?;
        Ok(Self {
            state: Some(PyMeshQuery::percentile(&inner, field, p, group_by)?),
        })
    }

    #[pyo3(signature = (field, group_by=None))]
    fn distinct_count(&self, field: String, group_by: Option<Vec<String>>) -> PyResult<Self> {
        let inner = self.require_state("distinct_count")?;
        Ok(Self {
            state: Some(PyMeshQuery::distinct_count(&inner, field, group_by)?),
        })
    }

    /// Tumbling window on `seq` over the current pipeline.
    fn window(&self, size: u64) -> PyResult<Self> {
        let inner = self.require_state("window")?;
        Ok(Self {
            state: Some(PyMeshQuery::window(&inner, size)?),
        })
    }

    /// Join the current pipeline (left) with `right` on
    /// `key`. Equivalent to `MeshQuery.join(self.build(),
    /// right, ...)`.
    #[pyo3(signature = (right, kind, key, strategy=None, watermark_secs=5.0))]
    fn join(
        &self,
        right: &PyMeshQuery,
        kind: &str,
        key: &str,
        strategy: Option<&str>,
        watermark_secs: f64,
    ) -> PyResult<Self> {
        let inner = self.require_state("join")?;
        Ok(Self {
            state: Some(PyMeshQuery::join(
                &inner,
                right,
                kind,
                key,
                strategy,
                watermark_secs,
            )?),
        })
    }

    /// Terminal: consume the builder into a `MeshQuery`.
    /// Surfaces `MeshDbError` if the builder has no source
    /// (`.at` / `.between` / `.latest` never called).
    fn build(&self) -> PyResult<PyMeshQuery> {
        self.require_state("build")
    }

    fn __repr__(&self) -> String {
        match &self.state {
            None => "QueryBuilder(empty)".to_string(),
            Some(q) => format!("QueryBuilder(state={})", q.__repr__()),
        }
    }
}

impl PyQueryBuilder {
    /// Return a clone of the current state or surface a
    /// helpful error naming the operator that needs one.
    fn require_state(&self, op: &str) -> PyResult<PyMeshQuery> {
        self.state.clone().ok_or_else(|| {
            MeshDbError::new_err(format!(
                "{op}: builder has no source — call .at(...), .between(...), or .latest(...) first"
            ))
        })
    }
}

/// One chain reached during a lineage walk. Pre-walked by the
/// caller and handed to `MeshQuery.lineage_emit(...)`. The SDK
/// doesn't itself walk the `fork-of:` graph — that needs a
/// `CapabilityIndex`, which isn't plumbed through the Python
/// runner yet. Callers maintain their own graph view and emit
/// entries in walk order: index 0 is the start origin with
/// `depth = 0`, ancestors / descendants follow.
#[pyclass(name = "LineageEntry", module = "net._net", frozen, from_py_object)]
#[derive(Clone)]
pub struct PyLineageEntry {
    /// Chain origin hash (substrate `u64`).
    #[pyo3(get)]
    pub origin: u64,
    /// Hops from the walk's start. `0` for the start origin.
    #[pyo3(get)]
    pub depth: u32,
    /// Best-known tip seq for this chain, if any. Surfaces in
    /// the emitted row's `seq` field (defaults to `0` when
    /// `None`).
    #[pyo3(get)]
    pub tip_seq: Option<u64>,
}

#[pymethods]
impl PyLineageEntry {
    #[new]
    #[pyo3(signature = (origin, depth, tip_seq=None))]
    fn new(origin: u64, depth: u32, tip_seq: Option<u64>) -> Self {
        Self {
            origin,
            depth,
            tip_seq,
        }
    }

    fn __repr__(&self) -> String {
        match self.tip_seq {
            None => format!(
                "LineageEntry(origin={:#x}, depth={})",
                self.origin, self.depth
            ),
            Some(t) => format!(
                "LineageEntry(origin={:#x}, depth={}, tip_seq={})",
                self.origin, self.depth, t
            ),
        }
    }
}

fn parse_lineage_direction(s: &str) -> PyResult<LineageDirection> {
    match s {
        "back" => Ok(LineageDirection::Back),
        "forward" => Ok(LineageDirection::Forward),
        other => Err(MeshDbError::new_err(format!(
            "lineage direction '{other}' not recognised; expected 'back' or 'forward'"
        ))),
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

/// Shared Tokio runtime — one per Python interpreter process,
/// not one per runner. Spinning up a multi-thread runtime per
/// runner was meaningful overhead for test harnesses that
/// construct many runners; a single shared runtime suffices
/// because the runner blocks the caller's thread anyway
/// (sync-drain design).
fn shared_runtime() -> Result<Arc<Runtime>, std::io::Error> {
    use std::sync::OnceLock;
    static RT: OnceLock<Arc<Runtime>> = OnceLock::new();
    if let Some(rt) = RT.get() {
        return Ok(rt.clone());
    }
    let rt = Arc::new(Runtime::new()?);
    let _ = RT.set(rt.clone());
    Ok(RT.get().cloned().unwrap_or(rt))
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
        let runtime = shared_runtime()
            .map_err(|e| MeshDbError::new_err(format!("failed to construct tokio runtime: {e}")))?;
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
            runtime,
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
    use pyo3::Python;
    let msg = format!("{e}");
    let kind = e.kind();
    let err = MeshDbError::new_err(msg);
    Python::attach(|py| {
        // Best-effort: attach the structured `kind`
        // discriminator on the raised instance so Python
        // callers can branch on `error.kind` without parsing
        // the message. Stored as a static `&'static str`
        // from `MeshError::kind()` so the value is part of the
        // public SDK contract.
        let _ = err.value(py).setattr("kind", kind);
    });
    err
}

// Tests live in `bindings/python/tests/test_meshdb.py` — the
// pyo3 unit-test linker dance on Windows requires libpython on
// PATH (only reliably available under `maturin develop`), and
// the existing Python bindings don't ship Rust-side tests.
// Run via:
//   maturin develop --features meshdb
//   pytest bindings/python/tests/test_meshdb.py
