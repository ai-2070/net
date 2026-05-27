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

use napi_derive::napi;
use net::adapter::net::cortex::tool::ToolDescriptor;

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
