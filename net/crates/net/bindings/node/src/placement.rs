//! TSFN-backed `PlacementFilter` bridge for the Node binding.
//!
//! Phase 7 of the SDK plan exposes custom `PlacementFilter`
//! callbacks across the FFI. Slice 2 is the Node side: a
//! [`TsfnPlacementFilter`] that wraps a `ThreadsafeFunction` and
//! implements the substrate's `PlacementFilter` trait. The SDK's
//! `placementFilterFromFn` already produces a
//! `RegisteredPlacementFilter { id, fn }`; the binding's
//! `NetMesh::registerPlacementFilter` (in `lib.rs`) consumes that
//! pair, builds a TSFN, wraps it here, and registers the wrapper
//! under `id` in [`global_placement_filter_registry`].
//!
//! On scoring the wrapper:
//!
//!   1. Looks up the candidate's `CapabilitySet` in the local
//!      `CapabilityIndex` (the daemon factory's `requirements()`
//!      narrows the candidate pool BEFORE scoring; here we
//!      materialize the per-candidate caps for the JS function).
//!   2. Marshals the candidate as a [`PlacementCandidateJs`]
//!      object (`{ nodeId, tags, metadata }`).
//!   3. Calls the TSFN with the candidate; blocks on a bounded
//!      `sync_channel` for the JS predicate's boolean result.
//!   4. Maps `true → Some(1.0)`, `false / error / timeout → None`
//!      (None vetoes the candidate per the trait contract).
//!
//! Bounded wait: the same rationale as
//! `EventDispatchBridge::process` — a re-entrant deadlock or a
//! blocked Node main thread would otherwise hang the placement
//! decision indefinitely. We use a fixed timeout + log on
//! timeout/error to keep operators aware.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use napi::bindgen_prelude::*;
use napi_derive::napi;

use net::adapter::net::behavior::capability::CapabilityIndex;
use net::adapter::net::behavior::placement::{
    Artifact, NodeId as PlacementNodeId, PlacementFilter,
};

/// Default budget for a single TSFN round-trip. Placement
/// scoring runs once per candidate per placement decision, so
/// this is bounded by `O(candidates) × timeout`. 5 s is generous
/// for a synchronous JS predicate; tighter in practice.
const DEFAULT_PLACEMENT_FILTER_TIMEOUT: Duration = Duration::from_secs(5);

/// Candidate handed to the JS placement-filter predicate.
///
/// Mirrors the SDK's `PlacementCandidate` interface — the
/// `node_id` matches the substrate's u64 NodeId, projected to
/// `bigint` on the JS side; `tags` and `metadata` are the live
/// snapshot from the local `CapabilityIndex` at scoring time.
#[napi(object)]
pub struct PlacementCandidateJs {
    pub node_id: BigInt,
    pub tags: Vec<String>,
    pub metadata: HashMap<String, String>,
}

/// TSFN type for the JS placement-filter predicate. Same shape
/// as the other binding TSFNs — `false` callee-handled flag, so
/// JS-thrown errors surface through the `Result<bool>` callback
/// and we treat them as veto.
pub type PlacementFilterTsfn = napi::threadsafe_function::ThreadsafeFunction<
    PlacementCandidateJs,
    bool,
    PlacementCandidateJs,
    napi::Status,
    false,
>;

/// `PlacementFilter` impl that bridges to a JS `(candidate) => bool`
/// predicate via a `ThreadsafeFunction`. Stored in the substrate
/// registry as `Arc<dyn PlacementFilter>`.
pub struct TsfnPlacementFilter {
    /// JS predicate to invoke per candidate.
    tsfn: PlacementFilterTsfn,
    /// Local capability index — looked up to materialize the
    /// candidate's tags/metadata for the JS object.
    capability_index: Arc<CapabilityIndex>,
    /// Bounded wait per scoring call.
    callback_timeout: Duration,
    /// SDK-supplied id, retained for diagnostics in logged
    /// timeout / error messages.
    id: String,
}

impl TsfnPlacementFilter {
    /// Construct a wrapper. The TSFN is built by the caller (via
    /// `Function::build_threadsafe_function().build()?`); we only
    /// take ownership here.
    pub fn new(
        id: String,
        tsfn: PlacementFilterTsfn,
        capability_index: Arc<CapabilityIndex>,
    ) -> Self {
        Self {
            tsfn,
            capability_index,
            callback_timeout: DEFAULT_PLACEMENT_FILTER_TIMEOUT,
            id,
        }
    }

    /// Override the per-call timeout. Useful for tests with
    /// deliberately slow predicates; future work will plumb this
    /// through `registerPlacementFilter` so JS callers can tune
    /// the budget per filter.
    #[allow(dead_code)]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.callback_timeout = timeout;
        self
    }
}

impl PlacementFilter for TsfnPlacementFilter {
    fn placement_score(
        &self,
        target: &PlacementNodeId,
        _artifact: &Artifact<'_>,
    ) -> Option<f32> {
        // Look up the candidate's caps. A candidate not in the
        // index is invisible to the JS predicate (which expects
        // tags + metadata); treat as veto rather than feeding an
        // empty candidate that the JS layer can't distinguish from
        // a real never-tagged node.
        let caps = match self.capability_index.get(*target) {
            Some(c) => c,
            None => return None,
        };

        // Materialize the candidate's tags as strings — the
        // substrate's `Tag` type renders to its on-wire string
        // form via `Display`. The JS layer doesn't need the typed
        // tag taxonomy to evaluate a predicate.
        let tags: Vec<String> = caps.tags.iter().map(|t| t.to_string()).collect();
        // Substrate stores metadata as `BTreeMap` (deterministic
        // iteration order); JS / napi-rs expects `HashMap` for
        // `Record<string, string>`. Cheap copy on the placement
        // hot path — bounded by the per-node metadata cardinality.
        let metadata: HashMap<String, String> = caps
            .metadata
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let candidate = PlacementCandidateJs {
            node_id: BigInt::from(*target),
            tags,
            metadata,
        };

        let (tx, rx) = std::sync::mpsc::sync_channel::<Result<bool>>(1);
        let id_for_log = self.id.clone();

        let status = self.tsfn.call_with_return_value(
            candidate,
            napi::threadsafe_function::ThreadsafeFunctionCallMode::NonBlocking,
            move |ret: Result<bool>, _env| {
                // `send` only fails if the receiver was dropped
                // (placement filter caller gave up). Swallow to
                // avoid napi-rs escalating to a fatal error.
                let _ = tx.send(ret);
                Ok(())
            },
        );
        if status != napi::Status::Ok {
            eprintln!(
                "TsfnPlacementFilter[{id}]: TSFN enqueue failed with status {status:?}; vetoing candidate {target:#x}",
                id = id_for_log,
            );
            return None;
        }

        match rx.recv_timeout(self.callback_timeout) {
            Ok(Ok(true)) => Some(1.0),
            Ok(Ok(false)) => None,
            Ok(Err(e)) => {
                eprintln!(
                    "TsfnPlacementFilter[{id}]: JS predicate threw for candidate {target:#x}: {e}; vetoing",
                    id = self.id,
                );
                None
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                eprintln!(
                    "TsfnPlacementFilter[{id}]: JS predicate did not respond within {ms} ms for candidate {target:#x} (possible re-entrant deadlock or blocked Node main thread); vetoing",
                    id = self.id,
                    ms = self.callback_timeout.as_millis(),
                );
                None
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                eprintln!(
                    "TsfnPlacementFilter[{id}]: JS predicate channel disconnected for candidate {target:#x}; vetoing",
                    id = self.id,
                );
                None
            }
        }
    }
}
