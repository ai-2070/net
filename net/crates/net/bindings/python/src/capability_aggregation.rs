//! PyO3 surface for the Phase 6c capability-aggregation API.
//!
//! The Rust core exposes
//! [`Fold::aggregate`](::net::adapter::net::behavior::fold::Fold::aggregate)
//! and
//! [`Fold::capacity_ranking`](::net::adapter::net::behavior::fold::Fold::capacity_ranking)
//! against `Fold<CapabilityFold>`. This module wires those through
//! to Python as `capability_aggregate(matcher, group_by, agg)` and
//! `capability_capacity_ranking(query, rtt_map)`.
//!
//! Wire shape per the plan: `TagMatcher` / `GroupBy` / `Aggregation`
//! cross the FFI boundary as JSON-encoded tagged unions. PyO3 can
//! also marshal `PyDict` directly, but JSON strings keep parity with
//! the Node / Go / C bindings and let the sdk-py wrappers handle
//! ergonomics on their side.
//!
//! For the RTT lookup we accept a materialized map (Python `dict[int,
//! int]` projected to `HashMap<NodeId, u32>`). The plan flags a
//! Python callable variant as the natural shape for sdk-py — the map
//! variant ships first and the callable wraps as a follow-up.

use std::collections::HashMap;

use pyo3::prelude::*;
use pyo3::types::PyDict;

use net::adapter::net::behavior::fold::{
    Aggregation, CapabilityFold, CapacityQuery, CapacityRow, Fold, GroupBy, TagMatcher,
};

/// Run [`Fold::aggregate`] against the live capability fold and
/// return a list of `(bucket, value)` tuples. `matcher_json = None`
/// walks every entry.
pub(crate) fn aggregate(
    py: Python<'_>,
    fold: &Fold<CapabilityFold>,
    matcher_json: Option<&str>,
    group_by_json: &str,
    aggregation_json: &str,
) -> PyResult<Py<PyAny>> {
    let matcher: Option<TagMatcher> = match matcher_json {
        Some(s) => Some(serde_json::from_str(s).map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("matcher JSON: {e}"))
        })?),
        None => None,
    };
    let group_by: GroupBy = serde_json::from_str(group_by_json).map_err(|e| {
        pyo3::exceptions::PyValueError::new_err(format!("group_by JSON: {e}"))
    })?;
    let agg: Aggregation = serde_json::from_str(aggregation_json).map_err(|e| {
        pyo3::exceptions::PyValueError::new_err(format!("aggregation JSON: {e}"))
    })?;

    let rows = fold.aggregate(matcher, group_by, agg);

    // Project rows into `[{"bucket": str, "value": int}, ...]`.
    let py_rows = pyo3::types::PyList::empty(py);
    for (bucket, value) in rows {
        let row = PyDict::new(py);
        row.set_item("bucket", bucket)?;
        row.set_item("value", value)?;
        py_rows.append(row)?;
    }
    Ok(py_rows.into())
}

/// Run [`Fold::capacity_ranking`] against the live capability fold
/// and return a list of capacity-row dicts. `rtt_map` is a Python
/// `dict[int, int]` mapping `node_id -> rtt_ms`; `None` or empty
/// disables the RTT filter regardless of `query.max_rtt_ms`.
pub(crate) fn capacity_ranking(
    py: Python<'_>,
    fold: &Fold<CapabilityFold>,
    query_json: &str,
    rtt_map: Option<&Bound<'_, PyDict>>,
) -> PyResult<Py<PyAny>> {
    let query: CapacityQuery = serde_json::from_str(query_json).map_err(|e| {
        pyo3::exceptions::PyValueError::new_err(format!("CapacityQuery JSON: {e}"))
    })?;

    let rtt_lookup: HashMap<u64, u32> = match rtt_map {
        Some(d) => {
            let mut m = HashMap::with_capacity(d.len());
            for (k, v) in d.iter() {
                let id: u64 = k.extract()?;
                let rtt: u32 = v.extract()?;
                m.insert(id, rtt);
            }
            m
        }
        None => HashMap::new(),
    };

    let rows: Vec<CapacityRow> =
        fold.capacity_ranking(query, |node_id| rtt_lookup.get(&node_id).copied());

    let py_rows = pyo3::types::PyList::empty(py);
    for r in rows {
        let row = PyDict::new(py);
        row.set_item("bucket", r.bucket)?;
        row.set_item("idle", r.idle)?;
        row.set_item("busy", r.busy)?;
        row.set_item("reserved", r.reserved)?;
        row.set_item("available", r.available)?;
        row.set_item("summed_capacity", r.summed_capacity)?;
        py_rows.append(row)?;
    }
    Ok(py_rows.into())
}
