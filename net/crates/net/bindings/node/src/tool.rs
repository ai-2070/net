//! AI tool-calling support for the Node napi binding.
//!
//! Currently exposes:
//!
//! - [`ToolDescriptorJs`] — wire-compatible mirror of the substrate's
//!   `ToolDescriptor` so `NetMesh.listTools()` can hand back full
//!   discovery rows (tool_id, name, version, description, schemas,
//!   requires, latency hint, stateless / streaming flags, tags,
//!   node_count).
//! - [`descriptor_to_js`] — substrate → napi conversion used by
//!   `NetMesh.listTools()` in `lib.rs`.
//!
//! Gated on the binding's `tool` feature (default-on).
//! Plan slice: B-3 of `docs/plans/NRPC_AI_TOOL_CALLING_AND_AGENT_DX.md`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures::stream::BoxStream;
use futures::StreamExt;
use napi::Result;
use napi_derive::napi;
use net::adapter::net::cortex::tool::{ToolDescriptor, ToolListChange, ToolListWatch};
use tokio::sync::{Mutex as TokioMutex, Notify};

/// Wire-compatible mirror of the substrate's
/// [`ToolDescriptor`](net::adapter::net::cortex::tool::ToolDescriptor).
///
/// One row per `(tool_id, version)` slot; `node_count` is filled
/// by the aggregating walk inside `MeshNode::list_tools`. Schemas
/// are stored as JSON-encoded strings — JS code that needs the
/// parsed shape (most provider-format translators do) calls
/// `JSON.parse(descriptor.inputSchema)`.
#[napi(object)]
pub struct ToolDescriptorJs {
    pub tool_id: String,
    pub name: String,
    pub version: String,
    pub description: Option<String>,
    pub input_schema: Option<String>,
    pub output_schema: Option<String>,
    pub requires: Vec<String>,
    pub estimated_time_ms: u32,
    pub stateless: bool,
    pub streaming: bool,
    pub tags: Vec<String>,
    pub node_count: u32,
}

/// Substrate → napi conversion. Pure field copy; no allocation
/// overhead beyond cloning the descriptor's owned String fields.
pub fn descriptor_to_js(d: ToolDescriptor) -> ToolDescriptorJs {
    ToolDescriptorJs {
        tool_id: d.tool_id,
        name: d.name,
        version: d.version,
        description: d.description,
        input_schema: d.input_schema,
        output_schema: d.output_schema,
        requires: d.requires,
        estimated_time_ms: d.estimated_time_ms,
        stateless: d.stateless,
        streaming: d.streaming,
        tags: d.tags,
        node_count: d.node_count,
    }
}

// =========================================================================
// ToolWatchIter — E-5 of POLLING_TO_EVENT_DRIVEN_SDK_PLAN.
//
// Async iterator over the substrate `MeshNode::watch_tools` stream
// (event-driven off the capability fold's change signal). Each `next()`
// yields one JSON-encoded `ToolListChange` (`{"type","descriptor",
// "prev_node_count"?}`) — the TS `watchTools` wrapper `JSON.parse`s it
// into the discriminated union, the same JSON-bridge contract the other
// bindings use. Replaces the prior `setTimeout` + `listTools` re-diff
// poll loop in the TS wrapper.
//
// Same `Arc<Mutex<Option<stream>>>` + shutdown-Notify shape as the
// memory/task watch iters. `close()` trips shutdown; the select's
// shutdown arm wins and the `BoxStream` (the `ToolListWatch`) is dropped
// — dropping its receiver closes the substrate channel, so the diff task
// exits even when parked on the fold change with no ceiling.
// =========================================================================

struct ToolWatchIterInner {
    stream: TokioMutex<Option<BoxStream<'static, ToolListChange>>>,
    shutdown: Notify,
    is_shutdown: AtomicBool,
}

/// Async iterator over a live `watchTools` stream. Each `next()`
/// returns one JSON-encoded `ToolListChange`, or `null` when closed /
/// ended.
#[napi]
pub struct ToolWatchIter {
    inner: Arc<ToolWatchIterInner>,
}

/// Build the napi iterator from a substrate `ToolListWatch`.
pub fn new_tool_watch_iter(watch: ToolListWatch) -> ToolWatchIter {
    let stream: BoxStream<'static, ToolListChange> = watch.boxed();
    ToolWatchIter {
        inner: Arc::new(ToolWatchIterInner {
            stream: TokioMutex::new(Some(stream)),
            shutdown: Notify::new(),
            is_shutdown: AtomicBool::new(false),
        }),
    }
}

// camelCase descriptor JSON matching `ToolDescriptorJs` / `listTools`,
// NOT the substrate's snake_case serde shape — the TS `ToolListChange`
// reads `descriptor.toolId` / `nodeCount`, so the wire JSON must be
// camelCase to round-trip into the TS discriminated union.
fn descriptor_to_camel_json(d: ToolDescriptor) -> serde_json::Value {
    serde_json::json!({
        "toolId": d.tool_id,
        "name": d.name,
        "version": d.version,
        "description": d.description,
        "inputSchema": d.input_schema,
        "outputSchema": d.output_schema,
        "requires": d.requires,
        "estimatedTimeMs": d.estimated_time_ms,
        "stateless": d.stateless,
        "streaming": d.streaming,
        "tags": d.tags,
        "nodeCount": d.node_count,
    })
}

fn change_to_json(change: ToolListChange) -> serde_json::Value {
    match change {
        ToolListChange::Added(d) => {
            serde_json::json!({ "type": "added", "descriptor": descriptor_to_camel_json(d) })
        }
        ToolListChange::Removed(d) => {
            serde_json::json!({ "type": "removed", "descriptor": descriptor_to_camel_json(d) })
        }
        ToolListChange::NodeCountChanged {
            descriptor,
            prev_node_count,
        } => serde_json::json!({
            "type": "node_count_changed",
            "descriptor": descriptor_to_camel_json(descriptor),
            "prevNodeCount": prev_node_count,
        }),
    }
}

#[napi]
impl ToolWatchIter {
    /// Wait for the next change. Returns `null` when the iterator has
    /// been closed or the underlying stream has ended.
    #[napi]
    pub async fn next(&self) -> Result<Option<String>> {
        let inner = &self.inner;
        if inner.is_shutdown.load(Ordering::Acquire) {
            return Ok(None);
        }
        let mut guard = inner.stream.lock().await;
        let stream = match guard.as_mut() {
            Some(s) => s,
            None => return Ok(None),
        };

        let shutdown_fut = inner.shutdown.notified();
        tokio::pin!(shutdown_fut);
        shutdown_fut.as_mut().enable();

        if inner.is_shutdown.load(Ordering::Acquire) {
            *guard = None;
            return Ok(None);
        }

        let next = tokio::select! {
            biased;
            _ = shutdown_fut => {
                *guard = None;
                None
            }
            msg = stream.next() => match msg {
                Some(change) => Some(change),
                None => {
                    *guard = None;
                    None
                }
            }
        };
        match next {
            Some(change) => serde_json::to_string(&change_to_json(change))
                .map(Some)
                .map_err(|e| {
                    napi::Error::from_reason(format!("watch_tools serialize failed: {e}"))
                }),
            None => Ok(None),
        }
    }

    /// Terminate the iterator early. Idempotent.
    #[napi]
    pub fn close(&self) {
        self.inner.is_shutdown.store(true, Ordering::Release);
        self.inner.shutdown.notify_waiters();
        // Best-effort: if no `next()` currently holds the lock, drop the
        // stream now so the substrate `ToolListWatch`'s receiver closes
        // and the diff task exits (releasing its `Arc<MeshNode>` ref)
        // without waiting for the next poll or GC — otherwise an active
        // watch keeps the node un-shutdownable. If a `next()` does hold
        // the lock, its shutdown arm drops the stream instead.
        if let Ok(mut guard) = self.inner.stream.try_lock() {
            *guard = None;
        }
    }
}
