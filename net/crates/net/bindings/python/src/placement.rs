//! `Py<PyAny>`-backed `PlacementFilter` bridge for the Python binding.
//!
//! Phase 7 of the SDK plan exposes custom `PlacementFilter`
//! callbacks across the FFI. Slice 3 is the Python side: a
//! [`PyPlacementFilter`] that wraps a Python callable and
//! implements the substrate's `PlacementFilter` trait. The SDK's
//! `placement_filter_from_fn` already produces a
//! `RegisteredPlacementFilter { id, fn }`; the binding's
//! `NetMesh.register_placement_filter` (in `lib.rs`) consumes that
//! pair and registers a [`PyPlacementFilter`] under `id` in
//! [`global_placement_filter_registry`].
//!
//! Compared to the Node TSFN bridge (slice 2), the Python bridge
//! is mechanically simpler:
//!
//! - No threadsafe-function machinery; PyO3's `Python::attach`
//!   acquires the GIL inline. The trait is sync, the GIL
//!   acquisition is sync, no channel + timeout dance.
//! - Per-call cost is GIL acquisition + dict construction +
//!   `call1`. Bounded by per-node metadata cardinality.
//!
//! On scoring the wrapper:
//!
//!   1. Looks up the candidate's `CapabilitySet` in the local
//!      `CapabilityIndex`. Missing-from-index → vetoed (no
//!      Python visibility into a never-indexed node).
//!   2. Builds a Python dict
//!      `{ "node_id": int, "tags": list[str], "metadata":
//!      dict[str, str] }` matching the SDK's
//!      `PlacementCandidate` typed-dict shape.
//!   3. Calls the Python predicate via `call1`.
//!   4. Maps `True → Some(1.0)`, `False / Exception / non-bool
//!      return → None` (None vetoes per the trait contract);
//!      exceptions are logged via `eprintln!` so operators can
//!      diagnose silent veto storms.

use std::sync::Arc;

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyTuple};

use net::adapter::net::behavior::fold::{capability_bridge, CapabilityFold, Fold};
use net::adapter::net::behavior::placement::{
    Artifact, NodeId as PlacementNodeId, PlacementFilter,
};

/// `PlacementFilter` impl that bridges to a Python `(candidate) -> bool`
/// predicate via `Py<PyAny>` + `Python::attach`. Stored in the
/// substrate registry as `Arc<dyn PlacementFilter>`.
pub struct PyPlacementFilter {
    /// Python callable to invoke per candidate. `Py<PyAny>` is
    /// `Send + Sync`; the GIL is acquired around every invocation.
    predicate: Py<PyAny>,
    /// Local capability fold — synthesized into a `CapabilitySet`
    /// at scoring time to materialize the candidate's tags/metadata
    /// for the Python dict.
    capability_fold: Arc<Fold<CapabilityFold>>,
    /// SDK-supplied id, retained for diagnostics in logged
    /// exception / type-error messages.
    id: String,
}

impl PyPlacementFilter {
    /// Construct a wrapper. `predicate` is a Python callable; the
    /// GIL is acquired internally around every invocation.
    pub fn new(
        id: String,
        predicate: Py<PyAny>,
        capability_fold: Arc<Fold<CapabilityFold>>,
    ) -> Self {
        Self {
            predicate,
            capability_fold,
            id,
        }
    }
}

impl PlacementFilter for PyPlacementFilter {
    fn placement_score(&self, target: &PlacementNodeId, _artifact: &Artifact<'_>) -> Option<f32> {
        // Look up the candidate's caps. Same semantics as the
        // Node TSFN bridge: missing-from-fold → vetoed (no Python
        // visibility into a never-indexed node, so we can't
        // materialize a meaningful candidate dict).
        if !self
            .capability_fold
            .with_state(|state| state.by_node.contains_key(target))
        {
            return None;
        }
        let caps = capability_bridge::synthesize_capability_set(&self.capability_fold, *target);

        Python::attach(|py| -> Option<f32> {
            // Build the candidate dict matching the SDK's
            // `PlacementCandidate` typed-dict shape (see
            // `sdk-py/src/net_sdk/capability.py`).
            let candidate = PyDict::new(py);
            if let Err(e) = candidate.set_item("node_id", *target) {
                eprintln!(
                    "PyPlacementFilter[{id}]: failed to set node_id on candidate dict: {e}; vetoing {target:#x}",
                    id = self.id,
                );
                return None;
            }

            // Tags as a list[str] — `Tag::Display` renders to the
            // on-wire string form. Substrate caps don't export the
            // typed taxonomy through the Python boundary; the
            // string list mirrors the SDK contract.
            let tags: Vec<String> = caps.tags.iter().map(|t| t.to_string()).collect();
            if let Err(e) = candidate.set_item("tags", tags) {
                eprintln!(
                    "PyPlacementFilter[{id}]: failed to set tags on candidate dict: {e}; vetoing {target:#x}",
                    id = self.id,
                );
                return None;
            }

            // Metadata as dict[str, str].
            let metadata = PyDict::new(py);
            for (k, v) in caps.metadata.iter() {
                if let Err(e) = metadata.set_item(k, v) {
                    eprintln!(
                        "PyPlacementFilter[{id}]: failed to set metadata key {k} on candidate dict: {e}; vetoing {target:#x}",
                        id = self.id,
                    );
                    return None;
                }
            }
            if let Err(e) = candidate.set_item("metadata", metadata) {
                eprintln!(
                    "PyPlacementFilter[{id}]: failed to set metadata on candidate dict: {e}; vetoing {target:#x}",
                    id = self.id,
                );
                return None;
            }

            let args = match PyTuple::new(py, [candidate.into_any()]) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!(
                        "PyPlacementFilter[{id}]: failed to build args tuple: {e}; vetoing {target:#x}",
                        id = self.id,
                    );
                    return None;
                }
            };

            // Call the predicate; map exceptions and non-bool
            // returns to `None` (veto). Logging gives operators
            // a way to diagnose silent veto storms.
            let ret = match self.predicate.call1(py, args) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!(
                        "PyPlacementFilter[{id}]: predicate raised for candidate {target:#x}: {e}; vetoing",
                        id = self.id,
                    );
                    return None;
                }
            };

            match ret.extract::<bool>(py) {
                Ok(true) => Some(1.0),
                Ok(false) => None,
                Err(e) => {
                    eprintln!(
                        "PyPlacementFilter[{id}]: predicate returned non-bool for candidate {target:#x}: {e}; vetoing",
                        id = self.id,
                    );
                    None
                }
            }
        })
    }
}
