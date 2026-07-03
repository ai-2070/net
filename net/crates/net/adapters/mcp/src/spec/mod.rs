//! MCP wire types — targeting the **2026-07-28 stateless** spec shape over
//! the stdio transport (newline-delimited JSON-RPC 2.0, one message per
//! line, UTF-8, no embedded newlines).
//!
//! Everything version-specific about MCP lives in this module (doctrine #6
//! of `MCP_BRIDGE_PLAN.md`): when the spec finalizes or shifts, this is the
//! only file that changes. The rest of the adapter is written against these
//! types, not against raw JSON.
//!
//! Scope is deliberately the **compat tier** (doctrine #2): request/response
//! tool calls only — `initialize`, `tools/list`, `tools/call`, and the
//! `tools/list_changed` notification. No sampling, elicitation, resources,
//! prompts, or streaming; a bridged MCP tool is request/response and nothing
//! more.

use serde::{Deserialize, Serialize};

/// The MCP protocol version this adapter negotiates. Dated string per the
/// MCP versioning scheme; bump here (and only here) when tracking the spec
/// through to final (`MCP_BRIDGE_PLAN.md` "Spec tracking").
pub const PROTOCOL_VERSION: &str = "2026-07-28";

/// The JSON-RPC 2.0 version tag every message carries.
pub const JSONRPC_VERSION: &str = "2.0";

// ---------------------------------------------------------------------------
// JSON-RPC 2.0 envelopes
// ---------------------------------------------------------------------------

/// A JSON-RPC request — carries an `id` and expects exactly one response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: i64,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

impl JsonRpcRequest {
    /// Build a request with the JSON-RPC version tag filled in.
    pub fn new(id: i64, method: impl Into<String>, params: Option<serde_json::Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            method: method.into(),
            params,
        }
    }
}

/// A JSON-RPC notification — a method call with **no** `id`, so it expects
/// no response (e.g. `notifications/initialized`, `tools/list_changed`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

impl JsonRpcNotification {
    /// Build a notification with the JSON-RPC version tag filled in.
    pub fn new(method: impl Into<String>, params: Option<serde_json::Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: method.into(),
            params,
        }
    }
}

/// The error object inside a JSON-RPC error response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// Standard JSON-RPC error code for a method the peer does not implement —
/// what the client replies to any (unsupported) server-initiated request.
pub const METHOD_NOT_FOUND: i64 = -32601;

// ---------------------------------------------------------------------------
// initialize
// ---------------------------------------------------------------------------

/// A peer's name + version, sent in both directions during `initialize`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Implementation {
    pub name: String,
    pub version: String,
}

/// `initialize` request params (client → server).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub protocol_version: String,
    /// Client capabilities. The bridge advertises none of the rich features
    /// (no sampling/roots), so this is an empty object on the wire.
    pub capabilities: serde_json::Value,
    pub client_info: Implementation,
}

impl InitializeParams {
    /// The params this adapter sends: the pinned protocol version, empty
    /// capabilities, and the supplied client identity.
    pub fn new(client_info: Implementation) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION.to_string(),
            capabilities: serde_json::json!({}),
            client_info,
        }
    }
}

/// `initialize` result (server → client). Unknown fields (server
/// capabilities beyond what the bridge uses, `instructions`, …) are ignored.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub protocol_version: String,
    #[serde(default)]
    pub capabilities: serde_json::Value,
    pub server_info: Implementation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

// ---------------------------------------------------------------------------
// tools/list
// ---------------------------------------------------------------------------

/// Default `input_schema` when a non-conforming server omits `inputSchema`:
/// an empty object, so the field is always an object rather than JSON `null`.
fn default_input_schema() -> serde_json::Value {
    serde_json::json!({})
}

/// One tool as reported by `tools/list`. `input_schema` is a JSON Schema
/// object kept as an opaque [`serde_json::Value`] — the descriptor-lowering
/// slice re-serializes it onto a `ToolDescriptor`; here it stays verbatim.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema for the tool's arguments. MCP requires it; default to an
    /// empty object (`{}`, not JSON `null`) if a non-conforming server omits
    /// it, so downstream code can always treat it as an object.
    #[serde(default = "default_input_schema")]
    pub input_schema: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<serde_json::Value>,
}

/// `tools/list` result. `next_cursor` supports pagination; the bridge reads
/// every page (a later slice) — v0 fixtures return a single page.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ListToolsResult {
    #[serde(default)]
    pub tools: Vec<Tool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

// ---------------------------------------------------------------------------
// tools/call
// ---------------------------------------------------------------------------

/// `tools/call` request params.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CallToolParams {
    pub name: String,
    /// Tool arguments — an arbitrary JSON object validated against the
    /// tool's `input_schema` by the shim before it reaches here.
    #[serde(default)]
    pub arguments: serde_json::Value,
}

/// `tools/call` result. `is_error` distinguishes a **tool-level** failure
/// (the tool ran and reported an error, in-band) from a protocol error (a
/// JSON-RPC error response) — the bridge must preserve that distinction.
///
/// `content` is kept as raw JSON blocks so unknown block types (images,
/// resources, …) round-trip losslessly to the nRPC caller; [`text`] pulls
/// out the concatenated `text` blocks for the common case.
///
/// [`text`]: CallToolResult::text
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct CallToolResult {
    #[serde(default)]
    pub content: Vec<serde_json::Value>,
    #[serde(default)]
    pub is_error: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<serde_json::Value>,
}

impl CallToolResult {
    /// Concatenate the `text` field of every `{"type":"text",...}` content
    /// block. The common case for a request/response bridged tool.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("")
    }

    /// A single text-content success result (fixture / helper convenience).
    pub fn text_ok(text: impl Into<String>) -> Self {
        Self {
            content: vec![serde_json::json!({ "type": "text", "text": text.into() })],
            is_error: false,
            structured_content: None,
        }
    }

    /// A single text-content error result (`is_error = true`).
    pub fn text_error(text: impl Into<String>) -> Self {
        Self {
            content: vec![serde_json::json!({ "type": "text", "text": text.into() })],
            is_error: true,
            structured_content: None,
        }
    }
}

// ---------------------------------------------------------------------------
// method names
// ---------------------------------------------------------------------------

/// Canonical MCP method / notification names, in one place.
pub mod method {
    pub const INITIALIZE: &str = "initialize";
    pub const INITIALIZED: &str = "notifications/initialized";
    pub const TOOLS_LIST: &str = "tools/list";
    pub const TOOLS_CALL: &str = "tools/call";
    pub const TOOLS_LIST_CHANGED: &str = "notifications/tools/list_changed";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_serializes_to_canonical_jsonrpc() {
        let req = JsonRpcRequest::new(1, method::TOOLS_LIST, None);
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["id"], 1);
        assert_eq!(v["method"], "tools/list");
        // `params: None` is omitted, not serialized as null.
        assert!(v.get("params").is_none());
    }

    #[test]
    fn initialize_params_carry_the_pinned_protocol_version() {
        let p = InitializeParams::new(Implementation {
            name: "net".into(),
            version: "0.30.0".into(),
        });
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(v["clientInfo"]["name"], "net");
        assert_eq!(v["capabilities"], serde_json::json!({}));
    }

    #[test]
    fn initialize_result_ignores_unknown_server_fields() {
        // A real server sends capabilities + extras the bridge doesn't read.
        let raw = serde_json::json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": { "listChanged": true } },
            "serverInfo": { "name": "fixture", "version": "0.1.0" },
            "instructions": "hi",
            "somethingNew": 42
        });
        let res: InitializeResult = serde_json::from_value(raw).unwrap();
        assert_eq!(res.server_info.name, "fixture");
        assert_eq!(res.instructions.as_deref(), Some("hi"));
    }

    #[test]
    fn tool_uses_camelcase_input_schema() {
        let raw = serde_json::json!({
            "name": "echo",
            "description": "echo it back",
            "inputSchema": { "type": "object", "properties": { "message": { "type": "string" } } }
        });
        let tool: Tool = serde_json::from_value(raw).unwrap();
        assert_eq!(tool.name, "echo");
        assert_eq!(tool.input_schema["type"], "object");
        // Re-serialization round-trips the camelCase key.
        let back = serde_json::to_value(&tool).unwrap();
        assert!(back.get("inputSchema").is_some());
        assert!(back.get("input_schema").is_none());
    }

    #[test]
    fn tool_without_input_schema_defaults_to_empty_object() {
        // A non-conforming server that omits inputSchema must lower to `{}`,
        // never JSON `null`, so downstream code can treat it as an object.
        let tool: Tool = serde_json::from_value(serde_json::json!({ "name": "bare" })).unwrap();
        assert_eq!(tool.input_schema, serde_json::json!({}));
        assert!(tool.input_schema.is_object());
    }

    #[test]
    fn call_result_text_concatenates_only_text_blocks() {
        let res = CallToolResult {
            content: vec![
                serde_json::json!({ "type": "text", "text": "hello " }),
                serde_json::json!({ "type": "image", "data": "…" }),
                serde_json::json!({ "type": "text", "text": "world" }),
            ],
            is_error: false,
            structured_content: None,
        };
        assert_eq!(res.text(), "hello world");
    }

    #[test]
    fn call_result_error_flag_round_trips() {
        let res = CallToolResult::text_error("boom");
        let v = serde_json::to_value(&res).unwrap();
        assert_eq!(v["isError"], true);
        let back: CallToolResult = serde_json::from_value(v).unwrap();
        assert!(back.is_error);
        assert_eq!(back.text(), "boom");
    }
}
