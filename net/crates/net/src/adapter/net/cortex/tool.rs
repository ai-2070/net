//! AI tool-calling surface — wire types + helpers shared by every
//! binding that exposes typed nRPC services as LLM tools.
//!
//! Gated by the `tool` Cargo feature. Two public exports:
//!
//! 1. [`ToolDescriptor`] — the SDK-facing discovery shape. Returned
//!    by `MeshNode::list_tools` and the future `tool.metadata.fetch`
//!    RPC. Carries the tool's name, version, JSON-Schema descriptions
//!    of its request/response shape, and a `node_count` filled in
//!    by aggregation across the capability fold.
//! 2. [`ToolEvent`] — the streaming envelope every server-streaming
//!    tool emits. One chunk per `ToolEvent` on the wire; clients
//!    decode and route per-variant.
//!
//! The wire-level pieces this composes against (`ToolCapability` in
//! `behavior::capability` + the capability fold + `call_service`
//! / `call_service_streaming`) compile unconditionally; the bits in
//! this module are only the SDK-facing additions and the streaming
//! envelope.
//!
//! Plan: see `docs/plans/NRPC_AI_TOOL_CALLING_AND_AGENT_DX.md`,
//! slices S-2 (descriptor + fold integration) and S-4 (feature gate
//! + envelope).

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::adapter::net::behavior::fold::capability_aggregation::TagMatcher;
use crate::adapter::net::behavior::ToolCapability;

// ============================================================================
// Metadata-key helpers
// ============================================================================
//
// `ToolCapability` already carries the small wire-cheap fields
// (`tool_id`, `name`, `version`, `requires`, `estimated_time_ms`,
// `stateless`). The fields too large or too JSON-ish to round-trip
// through capability TAGS — schemas (`input_schema`, `output_schema`)
// — already use the `CapabilitySet::metadata` extensibility hook via
// the keys `ToolCapability::input_schema_metadata_key` and
// `output_schema_metadata_key`.
//
// This slice extends that convention with three more keys:
//
// - `tool::<id>::description` — human-readable description the model
//   reads to decide when/how to call the tool. Mandatory for tools
//   advertised via `serve_tool`; legacy `ToolCapability` consumers
//   that constructed by hand may omit it (the field defaults to
//   `None` on the descriptor).
// - `tool::<id>::streaming` — `"1"` if the tool's nRPC handler is
//   server-streaming (`serve_tool_streaming` / `serve_rpc_streaming`
//   underneath); `"0"` or absent for unary tools. Encoded as a
//   single-byte ASCII flag rather than a typed bool so the
//   wire-shape of `CapabilitySet::metadata` (`HashMap<String, String>`)
//   doesn't change.
// - `tool::<id>::tags` — comma-separated free-form tags the host
//   attached at register time. Adapters surface these as provider-
//   specific metadata (e.g. Anthropic `cache_control` hints).

/// Metadata key holding the tool's human-readable description.
///
/// Same convention as [`ToolCapability::input_schema_metadata_key`]
/// — schema/description text lives in `CapabilitySet::metadata`
/// rather than the tag wire format because JSON / free-form text
/// can't round-trip through capability tags.
pub fn description_metadata_key(tool_id: &str) -> String {
    format!("tool::{tool_id}::description")
}

/// Metadata key holding the tool's streaming flag (`"1"` or `"0"`).
pub fn streaming_metadata_key(tool_id: &str) -> String {
    format!("tool::{tool_id}::streaming")
}

/// Metadata key holding the tool's free-form tags, comma-separated.
pub fn tags_metadata_key(tool_id: &str) -> String {
    format!("tool::{tool_id}::tags")
}

/// Metadata key holding the tool's `net.pricing.terms@1` envelope as
/// canonical JSON — pricing visible at discovery time, no 402
/// round-trip on the mesh. The value is discovery/UX metadata and
/// non-binding: billing and settlement bind only to quote-instantiated
/// requirements (the quote is provider-signed; announced terms are
/// covered by the announcement signature like every other key here).
/// The substrate carries the string opaquely — parsing and every
/// payment semantic live in `net-payments`, never in core.
pub fn pricing_terms_metadata_key(tool_id: &str) -> String {
    format!("tool::{tool_id}::pricing_terms")
}

// ============================================================================
// ToolDescriptor — SDK-facing discovery shape
// ============================================================================

/// One row in the result of `MeshNode::list_tools(...)`. Aggregates a
/// single (tool_id, version) across however many nodes currently
/// advertise it via the capability fold; `node_count` is filled by
/// the aggregator and is `0` on a freshly-constructed descriptor.
///
/// Source-of-truth fields are pulled from [`ToolCapability`]
/// (`tool_id` / `name` / `version` / `input_schema` /
/// `output_schema` / `requires` / `estimated_time_ms` / `stateless`)
/// plus `CapabilitySet::metadata` keys [`description_metadata_key`],
/// [`streaming_metadata_key`], and [`tags_metadata_key`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolDescriptor {
    /// nRPC service name. Same string the caller passes to
    /// `TypedMeshRpc::call` / `call_service` / `call_service_streaming`.
    pub tool_id: String,
    /// Human-readable name. Same field as `ToolCapability::name`.
    pub name: String,
    /// Tool version (semver-ish). Aggregation dedupes by
    /// `(tool_id, version)`; two nodes advertising the same tool at
    /// different versions surface as separate descriptors.
    pub version: String,
    /// Human-readable description; the model reads this to decide
    /// when/how to call. `None` for legacy tools that didn't go
    /// through `serve_tool`.
    pub description: Option<String>,
    /// JSON Schema (draft 2020-12) for the request body. `None` when
    /// the schema is too large for the capability fold's per-entry
    /// budget; fetch via the future `tool.metadata.fetch` RPC.
    pub input_schema: Option<String>,
    /// JSON Schema for the response body. `None` for non-strict
    /// tools (many models don't require it).
    pub output_schema: Option<String>,
    /// Required dependencies / sibling capabilities — direct mirror
    /// of `ToolCapability::requires`.
    pub requires: Vec<String>,
    /// Soft latency hint for the model scheduler / UI spinner.
    /// `0` if the host didn't supply an estimate.
    pub estimated_time_ms: u32,
    /// Tool is a pure function (same input → same output, no
    /// session state). Adapters use this to decide caching +
    /// parallel-invocation safety.
    pub stateless: bool,
    /// `true` if the handler is server-streaming
    /// (`serve_tool_streaming`). Adapters lower this into their
    /// provider's streaming protocol.
    pub streaming: bool,
    /// Free-form tags the host attached at register time.
    pub tags: Vec<String>,
    /// `net.pricing.terms@1` envelope as canonical JSON, when the host
    /// published this tool as paid ([`pricing_terms_metadata_key`]).
    /// Opaque to the substrate; interpreted only by `net-payments`.
    /// Additive: absent for free tools and on legacy announcements, and
    /// omitted from serialization when `None` so pre-payments consumers
    /// and pinned fixtures see the exact prior shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing_terms: Option<String>,
    /// How many nodes currently advertise this `(tool_id, version)`
    /// pair across the scope queried. Filled by the aggregator;
    /// stays at `0` on a freshly-constructed descriptor.
    pub node_count: u32,
}

impl ToolDescriptor {
    /// Build a descriptor from one `ToolCapability` + the metadata
    /// map it was announced with. `node_count` is left at `0`; the
    /// `MeshNode::list_tools` aggregator fills it during the merge
    /// pass.
    ///
    /// The metadata map is `CapabilitySet::metadata` — the same
    /// hook `ToolCapability::input_schema_metadata_key` /
    /// `output_schema_metadata_key` use.
    pub fn from_capability(cap: &ToolCapability, metadata: &BTreeMap<String, String>) -> Self {
        let description = metadata
            .get(&description_metadata_key(&cap.tool_id))
            .cloned();
        // Streaming flag is encoded as "1" or "0" / absent — keeps
        // `CapabilitySet::metadata`'s `HashMap<String, String>`
        // shape and avoids a parallel typed map.
        let streaming = metadata
            .get(&streaming_metadata_key(&cap.tool_id))
            .map(|s| s == "1")
            .unwrap_or(false);
        let tags = metadata
            .get(&tags_metadata_key(&cap.tool_id))
            .map(|raw| {
                raw.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let pricing_terms = metadata
            .get(&pricing_terms_metadata_key(&cap.tool_id))
            .cloned();
        Self {
            tool_id: cap.tool_id.clone(),
            name: cap.name.clone(),
            version: cap.version.clone(),
            description,
            input_schema: cap.input_schema.clone(),
            output_schema: cap.output_schema.clone(),
            requires: cap.requires.clone(),
            estimated_time_ms: cap.estimated_time_ms,
            stateless: cap.stateless,
            streaming,
            tags,
            pricing_terms,
            node_count: 0,
        }
    }
}

// ============================================================================
// ToolEvent — streaming envelope
// ============================================================================

/// Wire envelope every server-streaming AI tool emits, one envelope
/// per chunk on the underlying [`crate::adapter::net::mesh_rpc::RpcStream`].
///
/// Unary tools synthesize a single [`ToolEvent::Result`] under the
/// hood; client-side `call_tool` unwraps so unary callers never see
/// envelopes directly. Streaming callers (`call_tool_streaming`) see
/// each event as it arrives.
///
/// JSON-encoded per chunk (not postcard) so dumps stay readable and
/// clients can use whatever JSON parser they already have for the
/// typed request body. The envelope is the ONE convention every
/// provider adapter (OpenAI / Anthropic / Gemini / MCP / Hermes /
/// custom) lowers into the framework's native streaming protocol.
///
/// Plan: locked decision #4 in
/// `docs/plans/NRPC_AI_TOOL_CALLING_AND_AGENT_DX.md`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolEvent {
    /// Fires once on stream open. Carries the substrate's `call_id`
    /// so clients can correlate later events to the outstanding
    /// invocation (useful when an agent has multiple tool calls
    /// in flight at once).
    Start {
        /// nRPC service name the call is targeting.
        tool_id: String,
        /// Substrate call id (matches `RpcContext::call_id` on the
        /// server side). Optional because not every host knows the
        /// call id at envelope-construction time.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_id: Option<u64>,
        /// Optional free-form metadata the host wants the agent
        /// to see at stream open (model name, version, etc.).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<serde_json::Value>,
    },
    /// Coarse progress for spinner UIs. `pct` is `0.0..=100.0`.
    Progress {
        /// Optional fractional progress in `0.0..=100.0`. Adapters
        /// surface this as a UI progress hint (loading bar, spinner
        /// label).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pct: Option<f32>,
        /// Optional human-readable progress message
        /// (e.g. `"indexing"`, `"step 2 of 5"`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    /// Partial output — model tokens, file bytes, log lines. The
    /// adapter decides how to lower these into the provider's
    /// streaming protocol (Anthropic `tool_use_block_delta`, etc.).
    Delta {
        /// Partial output payload. Schema is tool-defined; common
        /// shapes are `{"token": "..."}` for LLM streaming and
        /// `{"chunk": "<base64>"}` for binary file chunks.
        data: serde_json::Value,
    },
    /// Terminal full result. Client sees exactly one
    /// [`Result`](ToolEvent::Result) OR one
    /// [`Error`](ToolEvent::Error) per stream — never both.
    Result {
        /// Final result payload. Conforms to the tool's
        /// `output_schema` when one is published.
        data: serde_json::Value,
    },
    /// Terminal failure with structured detail. Adapter lowers this
    /// to the provider's tool-error block.
    Error {
        /// Machine-parseable error code (e.g. `"invalid_input"`,
        /// `"upstream_timeout"`, `"cancelled"`).
        code: String,
        /// Human-readable message; surfaced to the model.
        message: String,
        /// Optional structured detail for debugging
        /// (e.g. `{"upstream": "anthropic"}`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<serde_json::Value>,
    },
}

impl ToolEvent {
    /// True if `self` is a terminal envelope (Result or Error). Used
    /// by the SDK's streaming wrapper to detect end-of-stream when
    /// the underlying `RpcStream` is still open (e.g. a misbehaving
    /// handler that emitted Result but didn't close).
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Result { .. } | Self::Error { .. })
    }
}

// ============================================================================
// ToolListWatch — Stream wrapper around the watch_tools mpsc receiver
// ============================================================================

/// Stream of [`ToolListChange`] events returned by
/// [`crate::adapter::net::MeshNode::watch_tools`]. Implements
/// `futures::Stream<Item = ToolListChange>`.
///
/// Backed by a bounded mpsc; a slow consumer applies backpressure
/// to the diff task (which blocks on the next `send().await`)
/// rather than queueing events without bound.
///
/// Two ways to end the watch:
/// - Drop the `ToolListWatch` — the diff task observes the closed
///   receiver (`tx.closed()`) on its next wake and exits.
/// - Call [`Self::cancel`] — wakes the diff task immediately even
///   if it's parked on the fold change signal with no consumer
///   reading. This is the path FFI bindings use: a blocking
///   `next` parked on the receiver can't be interrupted by
///   dropping the receiver (the blocked recv owns it), so the
///   cancel fires the task to exit, which drops the *sender* and
///   unblocks the parked recv with `None`.
pub struct ToolListWatch {
    pub(crate) receiver: tokio::sync::mpsc::Receiver<ToolListChange>,
    /// Fires the diff task's `select!` cancel arm. Cloned into the
    /// task at construction; `notify_one` stores a permit so a
    /// cancel racing the task's diff phase is still observed on the
    /// next `select!`.
    pub(crate) cancel: std::sync::Arc<tokio::sync::Notify>,
}

impl futures::Stream for ToolListWatch {
    type Item = ToolListChange;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.receiver.poll_recv(cx)
    }
}

impl ToolListWatch {
    /// Signal the diff task to stop. Idempotent. Wakes the task
    /// even when it's parked on the fold change signal, so a
    /// consumer blocked in a synchronous `next` (e.g. an FFI
    /// binding) unblocks promptly: the task exits, drops its
    /// sender, and the blocked recv returns `None`.
    pub fn cancel(&self) {
        self.cancel.notify_one();
    }

    /// Clone the cancel handle so a holder separate from the
    /// receiver (e.g. an FFI handle that has taken the receiver out
    /// to block on it) can fire cancellation. Firing it has the
    /// same effect as [`Self::cancel`].
    pub fn cancel_handle(&self) -> std::sync::Arc<tokio::sync::Notify> {
        self.cancel.clone()
    }

    /// Receive the next change event. Returns `None` when the
    /// diff task exits (consumer dropped the watch, or [`Self::cancel`]
    /// fired). Most callers should treat the watch handle as a
    /// stream and `.next().await` on it instead.
    pub async fn recv(&mut self) -> Option<ToolListChange> {
        self.receiver.recv().await
    }

    /// Non-blocking peek: returns the next change if one is already
    /// queued, otherwise `None`. Useful for poll-style consumers
    /// that want to drain without waiting.
    pub fn try_recv(&mut self) -> Option<ToolListChange> {
        self.receiver.try_recv().ok()
    }
}

// ============================================================================
// ToolListChange — dynamic-discovery diff event
// ============================================================================

/// One change in the set of tools visible to the local capability
/// fold, surfaced by [`crate::adapter::net::MeshNode::watch_tools`].
/// Adapter packages re-emit these to the agent runtime so the
/// model's tool list stays in sync with the mesh.
///
/// Identity for diffing is `(tool_id, version)`; the same `tool_id`
/// across two versions is two independent slots. `Added` and
/// `Removed` carry the full descriptor; `NodeCountChanged` carries
/// the latest descriptor with the new aggregated `node_count` plus
/// the previous count.
///
/// Plan: see `docs/plans/NRPC_AI_TOOL_CALLING_AND_AGENT_DX.md`,
/// slice A-5.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolListChange {
    /// A `(tool_id, version)` slot just appeared in the local fold.
    /// First-arrival event — the agent should add this tool to its
    /// tool list. `node_count` is the publisher count observed at
    /// arrival (typically `1` unless multiple publishers landed in
    /// the same diff window).
    Added(ToolDescriptor),
    /// A `(tool_id, version)` slot disappeared from the local fold —
    /// every publisher dropped it (registry removal + announce, or
    /// TTL expiry across the board). Carries the last-known
    /// descriptor so the adapter has the full shape on hand to do
    /// cleanup (e.g. remove from Anthropic `tools` array by `name`).
    Removed(ToolDescriptor),
    /// The publisher count for a `(tool_id, version)` slot changed,
    /// but the slot itself stayed present. `descriptor.node_count`
    /// is the new count; `prev_node_count` was the previously
    /// observed value. Useful for load-balancing UI ("3 nodes can
    /// serve this tool, up from 1").
    NodeCountChanged {
        /// Latest descriptor — `node_count` is the new aggregated
        /// publisher count.
        descriptor: ToolDescriptor,
        /// The publisher count observed before this change.
        prev_node_count: u32,
    },
}

// ============================================================================
// tool.metadata.fetch — on-demand schema pull
// ============================================================================
//
// The capability fold has a per-entry payload budget — large JSON
// schemas (multi-KB Pydantic-derived shapes, deep nested Zod output)
// can blow it. The fold drops oversized fields, leaving the
// `ToolDescriptor` with `input_schema: None` / `output_schema: None`
// at discovery time. Agents that actually need the schema (to lower
// into a provider's strict-mode tools array) call `tool.metadata.fetch`
// against the host to pull the full descriptor on demand.
//
// The host's own `ToolMetadataRegistry` is the source of truth — a
// local HashMap holding the full descriptor for every `serve_tool` on
// this node. The registry is populated by `serve_tool` (A-2) and
// drained by Drop on the returned `ServeHandle`.

/// Canonical nRPC service name for the on-demand schema pull. Both
/// halves of the call (the SDK-side client helper landing in A-2's
/// `MeshNode::fetch_tool_schema` and the auto-registered server
/// handler) use this constant so a future rename catches at one site.
pub const TOOL_METADATA_FETCH_SERVICE: &str = "tool.metadata.fetch";

/// Request body for the on-demand fetch. Wire shape: just the tool
/// name; agents already discovered the host via the capability fold
/// before issuing this call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolMetadataRequest {
    /// nRPC service name of the tool whose full descriptor the
    /// caller wants. Matches `ToolDescriptor::tool_id`.
    pub name: String,
}

/// Response body — the full descriptor when the host knows about
/// the named tool, or [`ToolMetadataResponse::NotFound`] when the
/// host has no `serve_tool` registration for it. `NotFound` is a
/// successful RPC response (not an `RpcError`) so callers can
/// distinguish "host doesn't have this tool" from "RPC transport
/// failed."
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
#[expect(
    clippy::large_enum_variant,
    reason = "one transient response per tool.metadata.fetch RPC, decoded and \
              immediately consumed — boxing the descriptor would add an \
              allocation + wire-invisible indirection to every Found for a \
              stack-size win nothing is sensitive to (ToolDescriptor crossed \
              the 200-byte lint threshold when pricing_terms landed)"
)]
pub enum ToolMetadataResponse {
    /// Host has a `serve_tool` registration for `name`; descriptor
    /// has every field the registry holds. `node_count` is left at
    /// `0` — the aggregator on the caller's side fills it from the
    /// fold walk.
    Found {
        /// Full descriptor for the requested tool.
        descriptor: ToolDescriptor,
    },
    /// Host has no `serve_tool` registration for the requested
    /// `name`. Distinct from RPC-level errors so a caller can fall
    /// back to another host without treating this as transient.
    NotFound {
        /// Echo of the request name so logs / Display strings can
        /// quote the missing tool without a separate side channel.
        name: String,
    },
}

// ============================================================================
// tool.watch — server-streamed remote watch (RT-6)
// ============================================================================
//
// `MeshNode::watch_tools` is a *local* surface: it diffs the local
// capability fold. A remote consumer (e.g. the Go RPC binding's
// `WatchTools`) previously had to re-poll `list_tools` over nRPC on
// a ticker. `tool.watch` is the server-streaming subscription that
// replaces that poll: the serving node runs its own (event-driven)
// `watch_tools` and streams one `ToolWatchFrame` per change to the
// subscriber, with a bounded per-subscriber buffer and an explicit
// resync signal on overflow. See
// `docs/plans/REALTIME_ROUTING_AND_DISCOVERY_PLAN.md` §4.4 Track C.

/// Canonical nRPC service name for the server-streamed remote tool
/// watch. Both halves of the call (the auto-installed server handler
/// in the SDK and any streaming client) use this constant so a
/// future rename catches at one site.
pub const TOOL_WATCH_SERVICE: &str = "tool.watch";

/// Request body for `tool.watch` — the two `MeshNode::watch_tools`
/// parameters in wire-friendly form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchToolsRequest {
    /// Optional tag filter with the same semantics as `list_tools`
    /// / `watch_tools`: an entry is included if ANY of its tags
    /// match. `None` watches every tool the serving node's fold
    /// sees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matcher: Option<TagMatcher>,
    /// Optional debounce-ceiling in milliseconds, mirroring the
    /// `interval` parameter of `watch_tools`: `None` is pure
    /// event-driven; `Some(ms)` additionally guarantees a re-diff
    /// at least every `ms` on the serving node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_ms: Option<u64>,
}

/// Wire frame of the `tool.watch` stream. One frame per streaming
/// chunk, JSON-encoded, tagged by `"type"` with the same JSON shape
/// the FFI bindings already use for [`ToolListChange`]:
///
/// ```json
/// { "type": "added",              "descriptor": { ... } }
/// { "type": "removed",            "descriptor": { ... } }
/// { "type": "node_count_changed", "descriptor": { ... }, "prev_node_count": 3 }
/// { "type": "resync" }
/// ```
///
/// # Resync contract
///
/// The server relays deltas through a bounded per-subscriber
/// buffer. When a subscriber falls behind and that buffer
/// overflows, the server drops the subscriber's queued deltas and
/// emits one [`ToolWatchFrame::Resync`] instead — a delta is never
/// lost silently. On receiving `Resync` the client must discard its
/// accumulated view and re-baseline from its **own local fold** via
/// `list_tools` (the capability fold is mesh-replicated; there is
/// no remote list service to call). Frames after the `Resync`
/// resume normal delta semantics; a delta that is already reflected
/// in the fresh baseline is possible and must be tolerated.
#[derive(Debug, Clone, PartialEq)]
#[expect(
    clippy::large_enum_variant,
    reason = "one transient frame per streamed change, encoded and immediately \
              consumed — boxing the change would add an allocation + \
              wire-invisible indirection to every delta for a stack-size win \
              nothing is sensitive to (same call as ToolMetadataResponse above)"
)]
pub enum ToolWatchFrame {
    /// One [`ToolListChange`] observed by the serving node's local
    /// watch, relayed verbatim.
    Change(ToolListChange),
    /// The server overflowed this subscriber's buffer and dropped
    /// queued deltas. Re-baseline via `list_tools` — see the
    /// type-level resync contract.
    Resync,
}

/// Wire tag values for [`ToolWatchFrame`]'s `"type"` field. Shared
/// by the manual `Serialize` / `Deserialize` impls below so the two
/// directions can't drift.
const TOOL_WATCH_FRAME_KINDS: [&str; 4] = ["added", "removed", "node_count_changed", "resync"];

impl Serialize for ToolWatchFrame {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        /// Borrowed wire shape — flat `"type"`-tagged map matching
        /// the FFI JSON for `ToolListChange` plus the `resync` kind.
        #[derive(Serialize)]
        struct Wire<'a> {
            #[serde(rename = "type")]
            kind: &'static str,
            #[serde(skip_serializing_if = "Option::is_none")]
            descriptor: Option<&'a ToolDescriptor>,
            #[serde(skip_serializing_if = "Option::is_none")]
            prev_node_count: Option<u32>,
        }
        let wire = match self {
            Self::Change(ToolListChange::Added(d)) => Wire {
                kind: TOOL_WATCH_FRAME_KINDS[0],
                descriptor: Some(d),
                prev_node_count: None,
            },
            Self::Change(ToolListChange::Removed(d)) => Wire {
                kind: TOOL_WATCH_FRAME_KINDS[1],
                descriptor: Some(d),
                prev_node_count: None,
            },
            Self::Change(ToolListChange::NodeCountChanged {
                descriptor,
                prev_node_count,
            }) => Wire {
                kind: TOOL_WATCH_FRAME_KINDS[2],
                descriptor: Some(descriptor),
                prev_node_count: Some(*prev_node_count),
            },
            Self::Resync => Wire {
                kind: TOOL_WATCH_FRAME_KINDS[3],
                descriptor: None,
                prev_node_count: None,
            },
        };
        wire.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ToolWatchFrame {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        /// Owned wire shape — see the `Serialize` impl.
        #[derive(Deserialize)]
        struct Wire {
            #[serde(rename = "type")]
            kind: String,
            #[serde(default)]
            descriptor: Option<ToolDescriptor>,
            #[serde(default)]
            prev_node_count: Option<u32>,
        }
        use serde::de::Error;
        let wire = Wire::deserialize(deserializer)?;
        let descriptor =
            |d: Option<ToolDescriptor>| d.ok_or_else(|| D::Error::missing_field("descriptor"));
        match wire.kind.as_str() {
            "added" => Ok(Self::Change(ToolListChange::Added(descriptor(
                wire.descriptor,
            )?))),
            "removed" => Ok(Self::Change(ToolListChange::Removed(descriptor(
                wire.descriptor,
            )?))),
            "node_count_changed" => Ok(Self::Change(ToolListChange::NodeCountChanged {
                descriptor: descriptor(wire.descriptor)?,
                prev_node_count: wire
                    .prev_node_count
                    .ok_or_else(|| D::Error::missing_field("prev_node_count"))?,
            })),
            "resync" => Ok(Self::Resync),
            other => Err(D::Error::unknown_variant(other, &TOOL_WATCH_FRAME_KINDS)),
        }
    }
}

/// Local-only registry holding the full `ToolDescriptor` for every
/// tool `serve_tool` registered on this node. The
/// `tool.metadata.fetch` handler reads this registry; `serve_tool`
/// writes to it on registration and removes on Drop.
///
/// `parking_lot::Mutex<HashMap<...>>` shape mirrors the existing
/// `cancel_registry` + `tool_metadata` patterns in the codebase
/// — sync access from the dispatch path, no async lock required.
///
/// Caches the snapshot as an `Arc<[ToolDescriptor]>` so the
/// announce path (which calls `snapshot()` every announce — every
/// capability-version bump) doesn't re-clone every descriptor when
/// nothing has changed. Insert/remove invalidate the cache.
#[derive(Debug, Default)]
pub struct ToolMetadataRegistry {
    inner: parking_lot::Mutex<RegistryState>,
    /// Local-caps change signal (RT-2,
    /// REALTIME_ROUTING_AND_DISCOVERY_PLAN). Bumped on every
    /// announce-relevant mutation — insert/replace, and remove of a
    /// present entry — so a change-driven announcer can wake without
    /// polling. `None` for standalone registries (unit tests /
    /// external construction); `MeshNode::new` injects its shared
    /// sender. Lives inside the registry rather than at call sites
    /// so a future mutation path can't forget to fire it (same
    /// reasoning as `Fold::signal_changed`).
    change_signal: Option<std::sync::Arc<tokio::sync::watch::Sender<u64>>>,
}

#[derive(Debug, Default)]
struct RegistryState {
    map: HashMap<String, ToolDescriptor>,
    snapshot: Option<std::sync::Arc<[ToolDescriptor]>>,
}

impl ToolMetadataRegistry {
    /// Empty registry — what a fresh `MeshNode` ships with before
    /// any `serve_tool` call.
    pub fn new() -> Self {
        Self::default()
    }

    /// Empty registry wired to a shared local-caps change signal
    /// (RT-2) — what `MeshNode::new` constructs. Every
    /// announce-relevant mutation bumps the sender's generation.
    pub fn with_change_signal(signal: std::sync::Arc<tokio::sync::watch::Sender<u64>>) -> Self {
        Self {
            inner: Default::default(),
            change_signal: Some(signal),
        }
    }

    /// Fire the change signal, if wired. Called (after the lock is
    /// released) by every mutation that changes what
    /// `announce_capabilities` would emit.
    fn signal_changed(&self) {
        if let Some(tx) = &self.change_signal {
            tx.send_modify(|g| *g = g.wrapping_add(1));
        }
    }

    /// Insert (or replace) the descriptor for `name`. Returns the
    /// previous entry if one existed. Registration paths that must
    /// reject duplicates use [`Self::try_insert`] instead — an
    /// insert-then-rollback around this method would fire the
    /// change signal (and expose the attempted descriptor to
    /// concurrent readers) for a registration that never commits.
    pub fn insert(&self, descriptor: ToolDescriptor) -> Option<ToolDescriptor> {
        let (prev, changed) = {
            let mut guard = self.inner.lock();
            // Only a real change to the announced surface should fire
            // the signal. A byte-identical re-insert (e.g. an app's
            // periodic ensure-registered loop) is a no-op: signaling
            // it would wake the RT-3 announcer into a full mesh-wide
            // capability broadcast + pingwave flood for zero
            // information change (RT-2 review Finding 9). Matches
            // `LocalServiceRegistry::insert`, which already suppresses
            // idempotent re-serves.
            let changed = guard.map.get(&descriptor.tool_id) != Some(&descriptor);
            if changed {
                guard.snapshot = None;
            }
            let prev = guard.map.insert(descriptor.tool_id.clone(), descriptor);
            (prev, changed)
        };
        if changed {
            self.signal_changed();
        }
        prev
    }

    /// Insert only if no descriptor exists for this `tool_id`;
    /// returns `true` when the insert committed. Atomic under the
    /// registry lock, so a duplicate registration neither mutates
    /// the map (concurrent readers never observe the attempted
    /// descriptor) nor fires the change signal — the RT-2/RT-3
    /// announcer must only ever publish registrations that
    /// actually committed. This is the `serve_tool`-path
    /// registration primitive.
    pub fn try_insert(&self, descriptor: ToolDescriptor) -> bool {
        let inserted = {
            let mut guard = self.inner.lock();
            if guard.map.contains_key(&descriptor.tool_id) {
                false
            } else {
                guard.map.insert(descriptor.tool_id.clone(), descriptor);
                guard.snapshot = None;
                true
            }
        };
        if inserted {
            self.signal_changed();
        }
        inserted
    }

    /// Look up the full descriptor for `name`. `None` when the
    /// host doesn't serve this tool. Cloned because the registry
    /// is mutex-protected; the clone is cheap (small heap fields).
    pub fn get(&self, name: &str) -> Option<ToolDescriptor> {
        self.inner.lock().map.get(name).cloned()
    }

    /// Remove the descriptor for `name`. Returns the removed entry
    /// if one existed. Called by the SDK's `serve_tool` Drop hook.
    /// Removing an absent entry is not a capability change and does
    /// not fire the change signal.
    pub fn remove(&self, name: &str) -> Option<ToolDescriptor> {
        let prev = {
            let mut guard = self.inner.lock();
            let prev = guard.map.remove(name);
            if prev.is_some() {
                guard.snapshot = None;
            }
            prev
        };
        if prev.is_some() {
            self.signal_changed();
        }
        prev
    }

    /// Returns the number of registered tools. Useful for the
    /// auto-install branch — the host installs the
    /// `tool.metadata.fetch` service the first time the registry
    /// goes from empty to non-empty.
    pub fn len(&self) -> usize {
        self.inner.lock().map.len()
    }

    /// True when no tools are registered. Convenience used by the
    /// same auto-install branch (`registry.is_empty()` reads
    /// cleaner than `len() == 0` at the call site).
    pub fn is_empty(&self) -> bool {
        self.inner.lock().map.is_empty()
    }

    /// Snapshot every descriptor as a cached `Arc<[ToolDescriptor]>`.
    /// First call after an insert/remove rebuilds the cache; later
    /// calls share the same `Arc` for free.
    pub fn snapshot(&self) -> std::sync::Arc<[ToolDescriptor]> {
        let mut guard = self.inner.lock();
        if let Some(s) = &guard.snapshot {
            return s.clone();
        }
        let snap: std::sync::Arc<[ToolDescriptor]> =
            guard.map.values().cloned().collect::<Vec<_>>().into();
        guard.snapshot = Some(snap.clone());
        snap
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn cap(tool_id: &str) -> ToolCapability {
        ToolCapability::new(tool_id, format!("Name for {tool_id}"))
            .with_version("1.2.3")
            .with_input_schema(r#"{"type":"object"}"#)
    }

    #[test]
    fn metadata_keys_follow_existing_convention() {
        // Same shape as `ToolCapability::input_schema_metadata_key`
        // — pinning here so a future rename catches at this test
        // before any downstream consumer drifts.
        assert_eq!(
            description_metadata_key("web_search"),
            "tool::web_search::description"
        );
        assert_eq!(
            streaming_metadata_key("web_search"),
            "tool::web_search::streaming"
        );
        assert_eq!(tags_metadata_key("web_search"), "tool::web_search::tags");
    }

    #[test]
    fn descriptor_from_capability_picks_up_metadata_fields() {
        let cap = cap("web_search");
        let mut meta = BTreeMap::new();
        meta.insert(
            description_metadata_key("web_search"),
            "Search the web.".to_string(),
        );
        meta.insert(streaming_metadata_key("web_search"), "1".to_string());
        meta.insert(
            tags_metadata_key("web_search"),
            "web,research,external".to_string(),
        );

        let desc = ToolDescriptor::from_capability(&cap, &meta);
        assert_eq!(desc.tool_id, "web_search");
        assert_eq!(desc.version, "1.2.3");
        assert_eq!(desc.description.as_deref(), Some("Search the web."));
        assert!(desc.streaming);
        assert_eq!(desc.tags, vec!["web", "research", "external"]);
        assert_eq!(desc.input_schema.as_deref(), Some(r#"{"type":"object"}"#));
        assert_eq!(
            desc.node_count, 0,
            "node_count is filled by the aggregator, not here"
        );
    }

    // ---- tool.watch wire frames (RT-6) ----

    fn sample_descriptor() -> ToolDescriptor {
        ToolDescriptor::from_capability(&cap("web_search"), &BTreeMap::new())
    }

    #[test]
    fn tool_watch_frame_json_matches_ffi_change_shape() {
        // The frame's JSON for `Change` must be exactly the FFI
        // watch-tools JSON (`{"type": "...", "descriptor": ...}`),
        // so a binding-side decoder can share one code path.
        let added = ToolWatchFrame::Change(ToolListChange::Added(sample_descriptor()));
        let v: serde_json::Value = serde_json::to_value(&added).unwrap();
        assert_eq!(v["type"], "added");
        assert_eq!(v["descriptor"]["tool_id"], "web_search");
        assert!(v.get("prev_node_count").is_none());

        let ncc = ToolWatchFrame::Change(ToolListChange::NodeCountChanged {
            descriptor: sample_descriptor(),
            prev_node_count: 3,
        });
        let v: serde_json::Value = serde_json::to_value(&ncc).unwrap();
        assert_eq!(v["type"], "node_count_changed");
        assert_eq!(v["prev_node_count"], 3);

        let v: serde_json::Value = serde_json::to_value(&ToolWatchFrame::Resync).unwrap();
        assert_eq!(v, serde_json::json!({ "type": "resync" }));
    }

    #[test]
    fn tool_watch_frame_round_trips_every_variant() {
        let frames = vec![
            ToolWatchFrame::Change(ToolListChange::Added(sample_descriptor())),
            ToolWatchFrame::Change(ToolListChange::Removed(sample_descriptor())),
            ToolWatchFrame::Change(ToolListChange::NodeCountChanged {
                descriptor: sample_descriptor(),
                prev_node_count: 7,
            }),
            ToolWatchFrame::Resync,
        ];
        for frame in frames {
            let json = serde_json::to_vec(&frame).unwrap();
            let back: ToolWatchFrame = serde_json::from_slice(&json).unwrap();
            assert_eq!(back, frame);
        }
    }

    #[test]
    fn tool_watch_frame_rejects_malformed_wire() {
        // Missing descriptor on a change kind.
        let err = serde_json::from_str::<ToolWatchFrame>(r#"{"type":"added"}"#);
        assert!(err.is_err(), "added without descriptor must fail");
        // Missing prev_node_count.
        let d = serde_json::to_value(sample_descriptor()).unwrap();
        let raw = serde_json::json!({ "type": "node_count_changed", "descriptor": d });
        assert!(serde_json::from_value::<ToolWatchFrame>(raw).is_err());
        // Unknown kind.
        let err = serde_json::from_str::<ToolWatchFrame>(r#"{"type":"nonsense"}"#);
        assert!(err.is_err(), "unknown frame kind must fail");
    }

    #[test]
    fn watch_tools_request_omits_absent_options() {
        let req = WatchToolsRequest {
            matcher: None,
            interval_ms: None,
        };
        assert_eq!(serde_json::to_string(&req).unwrap(), "{}");
        let req = WatchToolsRequest {
            matcher: Some(TagMatcher::Prefix {
                value: "ai-tool:".into(),
            }),
            interval_ms: Some(250),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: WatchToolsRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn change_signal_fires_on_real_mutations_only() {
        // RT-2 (REALTIME_ROUTING_AND_DISCOVERY_PLAN): the registry
        // fires the injected local-caps signal on announce-relevant
        // mutations only — never on reads or no-op removes — so a
        // change-driven announcer wakes exactly when the announced
        // surface changed.
        let signal = std::sync::Arc::new(tokio::sync::watch::channel(0u64).0);
        let reg = ToolMetadataRegistry::with_change_signal(signal.clone());
        let generation = || *signal.borrow();

        assert!(reg.remove("ghost").is_none());
        assert_eq!(generation(), 0, "remove of an absent tool must not signal");

        let desc = ToolDescriptor::from_capability(&cap("web_search"), &BTreeMap::new());
        reg.insert(desc.clone());
        assert_eq!(generation(), 1, "insert must signal");

        // A byte-identical re-insert is a no-op — it must NOT signal
        // (RT-2 review Finding 9: idempotent re-register would
        // otherwise trigger a full mesh-wide announce for no change).
        reg.insert(desc.clone());
        assert_eq!(generation(), 1, "idempotent re-insert must not signal");

        // A replace that actually changes the descriptor must signal.
        let mut changed = desc;
        changed.description = Some("now with a description".to_string());
        reg.insert(changed);
        assert_eq!(generation(), 2, "a changed replace must signal");

        assert!(reg.remove("web_search").is_some());
        assert_eq!(generation(), 3, "remove of a present tool must signal");

        let _ = reg.get("web_search");
        let _ = reg.snapshot();
        let _ = reg.is_empty();
        assert_eq!(generation(), 3, "reads must not signal");
    }

    #[test]
    fn try_insert_rejects_duplicates_without_mutation_or_signal() {
        // RT-2 review follow-up (cubic P1): a rejected duplicate
        // registration must not mutate the registry (concurrent
        // readers could observe the attempted descriptor) and must
        // not fire the change signal (the announcer would publish a
        // registration that never committed).
        let signal = std::sync::Arc::new(tokio::sync::watch::channel(0u64).0);
        let reg = ToolMetadataRegistry::with_change_signal(signal.clone());
        let generation = || *signal.borrow();

        let original = ToolDescriptor::from_capability(&cap("web_search"), &BTreeMap::new());
        assert!(reg.try_insert(original.clone()), "first insert commits");
        assert_eq!(generation(), 1, "committed insert must signal");

        let mut imposter = ToolDescriptor::from_capability(&cap("web_search"), &BTreeMap::new());
        imposter.description = Some("imposter".to_string());
        assert!(!reg.try_insert(imposter), "duplicate must be rejected");
        assert_eq!(generation(), 1, "rejected duplicate must not signal");
        assert_eq!(
            reg.get("web_search").expect("entry present").description,
            original.description,
            "rejected duplicate must not replace the committed descriptor",
        );
    }

    #[test]
    fn descriptor_from_capability_handles_missing_metadata() {
        let cap = cap("legacy");
        let meta = BTreeMap::new();
        let desc = ToolDescriptor::from_capability(&cap, &meta);
        assert!(desc.description.is_none());
        assert!(!desc.streaming);
        assert!(desc.tags.is_empty());
    }

    #[test]
    fn descriptor_tags_parsing_strips_whitespace_and_drops_empty() {
        let cap = cap("messy");
        let mut meta = BTreeMap::new();
        meta.insert(tags_metadata_key("messy"), "  a , b ,, c  ,".to_string());
        let desc = ToolDescriptor::from_capability(&cap, &meta);
        assert_eq!(desc.tags, vec!["a", "b", "c"]);
    }

    #[test]
    fn tool_event_serde_roundtrip_each_variant() {
        let cases = vec![
            ToolEvent::Start {
                tool_id: "web_search".into(),
                call_id: Some(42),
                metadata: Some(serde_json::json!({"model": "claude-opus-4-7"})),
            },
            ToolEvent::Progress {
                pct: Some(33.3),
                message: Some("indexing".into()),
            },
            ToolEvent::Delta {
                data: serde_json::json!({"token": "the"}),
            },
            ToolEvent::Result {
                data: serde_json::json!({"results": ["a", "b"]}),
            },
            ToolEvent::Error {
                code: "upstream_timeout".into(),
                message: "took >30s".into(),
                details: Some(serde_json::json!({"upstream": "anthropic"})),
            },
        ];
        for event in cases {
            let encoded = serde_json::to_string(&event).expect("encode");
            let decoded: ToolEvent = serde_json::from_str(&encoded).expect("decode");
            assert_eq!(event, decoded, "round-trip must be byte-stable");
        }
    }

    #[test]
    fn tool_event_is_terminal_only_for_result_and_error() {
        assert!(!ToolEvent::Start {
            tool_id: "x".into(),
            call_id: None,
            metadata: None
        }
        .is_terminal());
        assert!(!ToolEvent::Progress {
            pct: None,
            message: None
        }
        .is_terminal());
        assert!(!ToolEvent::Delta {
            data: serde_json::Value::Null
        }
        .is_terminal());
        assert!(ToolEvent::Result {
            data: serde_json::Value::Null
        }
        .is_terminal());
        assert!(ToolEvent::Error {
            code: "".into(),
            message: "".into(),
            details: None
        }
        .is_terminal());
    }

    #[test]
    fn tool_event_optional_fields_omitted_when_none() {
        // `skip_serializing_if = "Option::is_none"` keeps the wire
        // shape minimal — pin so a future field-addition doesn't
        // accidentally break the no-overhead-for-unused-fields
        // contract.
        let event = ToolEvent::Start {
            tool_id: "x".into(),
            call_id: None,
            metadata: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert_eq!(json, r#"{"type":"start","tool_id":"x"}"#);

        let event = ToolEvent::Progress {
            pct: None,
            message: None,
        };
        assert_eq!(
            serde_json::to_string(&event).unwrap(),
            r#"{"type":"progress"}"#
        );
    }

    // ── pricing terms (payments discovery hook) ─────────────────

    #[test]
    fn from_capability_reads_pricing_terms_and_stays_free_without_the_key() {
        let terms = "{\"object\":\"net.pricing.terms@1\"}";
        let capability = cap("paid_tool");
        let mut metadata = BTreeMap::new();
        metadata.insert(pricing_terms_metadata_key("paid_tool"), terms.to_string());
        let paid = ToolDescriptor::from_capability(&capability, &metadata);
        assert_eq!(paid.pricing_terms.as_deref(), Some(terms));

        let free = ToolDescriptor::from_capability(&capability, &BTreeMap::new());
        assert_eq!(free.pricing_terms, None);

        // Absent pricing serializes to nothing — pre-payments consumers
        // and pinned fixtures see the exact prior descriptor shape.
        let json = serde_json::to_string(&free).unwrap();
        assert!(!json.contains("pricing_terms"), "{json}");
    }

    // ── tool.metadata.fetch ─────────────────────────────────────

    fn descriptor(tool_id: &str) -> ToolDescriptor {
        let cap = cap(tool_id);
        ToolDescriptor::from_capability(&cap, &BTreeMap::new())
    }

    #[test]
    fn tool_metadata_fetch_service_name_is_canonical() {
        // Service name lives in one constant so client + server
        // halves can't drift.
        assert_eq!(TOOL_METADATA_FETCH_SERVICE, "tool.metadata.fetch");
    }

    #[test]
    fn tool_metadata_response_serde_distinguishes_found_and_not_found() {
        let found = ToolMetadataResponse::Found {
            descriptor: descriptor("web_search"),
        };
        let not_found = ToolMetadataResponse::NotFound {
            name: "missing".into(),
        };
        for resp in [&found, &not_found] {
            let encoded = serde_json::to_string(resp).unwrap();
            let decoded: ToolMetadataResponse = serde_json::from_str(&encoded).unwrap();
            assert_eq!(*resp, decoded, "round-trip must be byte-stable");
        }
        // `type` tag confirms wire-level disambiguation — a client
        // matching on this string must keep working across releases.
        let found_json = serde_json::to_value(&found).unwrap();
        assert_eq!(found_json["type"], "found");
        let nf_json = serde_json::to_value(&not_found).unwrap();
        assert_eq!(nf_json["type"], "not_found");
    }

    #[test]
    fn tool_metadata_registry_insert_lookup_remove_roundtrip() {
        let reg = ToolMetadataRegistry::new();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);

        let desc = descriptor("web_search");
        assert!(
            reg.insert(desc.clone()).is_none(),
            "first insert returns None"
        );
        assert_eq!(reg.len(), 1);

        let got = reg.get("web_search").expect("get must find it");
        assert_eq!(got, desc);

        // Re-insert returns the previous entry (lets the SDK detect
        // duplicate registration without a separate sentinel).
        let prior = reg
            .insert(desc.clone())
            .expect("second insert returns prior");
        assert_eq!(prior, desc);

        let removed = reg.remove("web_search").expect("remove must find it");
        assert_eq!(removed, desc);
        assert!(reg.is_empty());
        assert!(reg.get("web_search").is_none());
        assert!(reg.remove("web_search").is_none());
    }

    #[test]
    fn tool_metadata_registry_snapshot_returns_all_entries() {
        let reg = ToolMetadataRegistry::new();
        reg.insert(descriptor("a"));
        reg.insert(descriptor("b"));
        reg.insert(descriptor("c"));
        let mut names: Vec<String> = reg.snapshot().iter().map(|d| d.tool_id.clone()).collect();
        names.sort();
        assert_eq!(names, vec!["a", "b", "c"]);
    }

    /// E-3 regression: consecutive `snapshot()` calls without an
    /// intervening insert/remove must return the SAME `Arc` (cached),
    /// not a freshly cloned Vec — that's the whole point of the cache.
    /// Any mutation invalidates: subsequent snapshot must return a
    /// different `Arc`.
    #[test]
    fn tool_metadata_registry_snapshot_caches_until_mutation() {
        let reg = ToolMetadataRegistry::new();
        reg.insert(descriptor("a"));

        let s1 = reg.snapshot();
        let s2 = reg.snapshot();
        assert!(
            std::sync::Arc::ptr_eq(&s1, &s2),
            "two consecutive snapshots without mutation must share the same Arc"
        );

        // Insert invalidates.
        reg.insert(descriptor("b"));
        let s3 = reg.snapshot();
        assert!(
            !std::sync::Arc::ptr_eq(&s1, &s3),
            "insert must invalidate the cached snapshot"
        );

        // Snapshot after insert is now the new cached one.
        let s4 = reg.snapshot();
        assert!(
            std::sync::Arc::ptr_eq(&s3, &s4),
            "snapshot after insert must cache again"
        );

        // Remove invalidates.
        reg.remove("a");
        let s5 = reg.snapshot();
        assert!(
            !std::sync::Arc::ptr_eq(&s3, &s5),
            "remove must invalidate the cached snapshot"
        );

        // Remove of a non-existent key must NOT invalidate (no change).
        let s6 = reg.snapshot();
        reg.remove("nonexistent");
        let s7 = reg.snapshot();
        assert!(
            std::sync::Arc::ptr_eq(&s6, &s7),
            "no-op remove must not invalidate the cached snapshot"
        );
    }
}
