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

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

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
    pub fn from_capability(cap: &ToolCapability, metadata: &HashMap<String, String>) -> Self {
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
/// `futures::Stream<Item = ToolListChange>`. Dropping the watch
/// ends the underlying polling task on its next tick.
///
/// Backed by a bounded mpsc; a slow consumer applies backpressure
/// to the polling task (which blocks on the next `send().await`)
/// rather than queueing events without bound.
pub struct ToolListWatch {
    pub(crate) receiver: tokio::sync::mpsc::Receiver<ToolListChange>,
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
    /// Receive the next change event. Returns `None` when the
    /// underlying polling task exits (cannot happen while the
    /// `MeshNode` is alive). Most callers should treat the watch
    /// handle as a stream and `.next().await` on it instead.
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

/// Local-only registry holding the full `ToolDescriptor` for every
/// tool `serve_tool` registered on this node. The
/// `tool.metadata.fetch` handler reads this registry; `serve_tool`
/// writes to it on registration and removes on Drop.
///
/// `parking_lot::Mutex<HashMap<...>>` shape mirrors the existing
/// `cancel_registry` + `tool_metadata` patterns in the codebase
/// — sync access from the dispatch path, no async lock required.
#[derive(Debug, Default)]
pub struct ToolMetadataRegistry {
    inner: parking_lot::Mutex<HashMap<String, ToolDescriptor>>,
}

impl ToolMetadataRegistry {
    /// Empty registry — what a fresh `MeshNode` ships with before
    /// any `serve_tool` call. `Default::default()` works too;
    /// keeping the named constructor so call sites read clearly.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert (or replace) the descriptor for `name`. Returns the
    /// previous entry if one existed — callers can use this for
    /// duplicate-registration diagnostics (the SDK's `serve_tool`
    /// rejects duplicate names rather than silently overwriting).
    pub fn insert(&self, descriptor: ToolDescriptor) -> Option<ToolDescriptor> {
        let mut guard = self.inner.lock();
        guard.insert(descriptor.tool_id.clone(), descriptor)
    }

    /// Look up the full descriptor for `name`. `None` when the
    /// host doesn't serve this tool. Cloned because the registry
    /// is mutex-protected; the clone is cheap (small heap fields).
    pub fn get(&self, name: &str) -> Option<ToolDescriptor> {
        self.inner.lock().get(name).cloned()
    }

    /// Remove the descriptor for `name`. Returns the removed entry
    /// if one existed. Called by the SDK's `serve_tool` Drop hook.
    pub fn remove(&self, name: &str) -> Option<ToolDescriptor> {
        self.inner.lock().remove(name)
    }

    /// Returns the number of registered tools. Useful for the
    /// auto-install branch — the host installs the
    /// `tool.metadata.fetch` service the first time the registry
    /// goes from empty to non-empty.
    pub fn len(&self) -> usize {
        self.inner.lock().len()
    }

    /// True when no tools are registered. Convenience used by the
    /// same auto-install branch (`registry.is_empty()` reads
    /// cleaner than `len() == 0` at the call site).
    pub fn is_empty(&self) -> bool {
        self.inner.lock().is_empty()
    }

    /// Snapshot every descriptor as a `Vec`. Used by the
    /// `tool.metadata.list` variant (a future addition) and by
    /// `Deck` panels that want the full local set. Allocates;
    /// callers that just want the count use `len()`.
    pub fn snapshot(&self) -> Vec<ToolDescriptor> {
        self.inner.lock().values().cloned().collect()
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
        let mut meta = HashMap::new();
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

    #[test]
    fn descriptor_from_capability_handles_missing_metadata() {
        let cap = cap("legacy");
        let meta = HashMap::new();
        let desc = ToolDescriptor::from_capability(&cap, &meta);
        assert!(desc.description.is_none());
        assert!(!desc.streaming);
        assert!(desc.tags.is_empty());
    }

    #[test]
    fn descriptor_tags_parsing_strips_whitespace_and_drops_empty() {
        let cap = cap("messy");
        let mut meta = HashMap::new();
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

    // ── tool.metadata.fetch ─────────────────────────────────────

    fn descriptor(tool_id: &str) -> ToolDescriptor {
        let cap = cap(tool_id);
        ToolDescriptor::from_capability(&cap, &HashMap::new())
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
        let mut names: Vec<String> = reg.snapshot().into_iter().map(|d| d.tool_id).collect();
        names.sort();
        assert_eq!(names, vec!["a", "b", "c"]);
    }
}
