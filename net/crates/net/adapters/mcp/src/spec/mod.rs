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

/// This bridge's own implementation version, reported as `serverInfo.version`
/// in the `initialize` handshake (`net mcp serve`). Sourced from the crate
/// version (`Cargo.toml`) so it never drifts from the release — distinct from
/// [`PROTOCOL_VERSION`], which is the MCP spec version, not the adapter's.
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

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

/// Standard JSON-RPC error code for invalid JSON (the server could not parse
/// the line at all). Replied with a `null` id.
pub const PARSE_ERROR: i64 = -32700;
/// Standard JSON-RPC error code for a well-formed message that is not a valid
/// request object (e.g. missing `method`).
pub const INVALID_REQUEST: i64 = -32600;
/// Standard JSON-RPC error code for a method the peer does not implement —
/// what the client replies to any (unsupported) server-initiated request, and
/// what the shim replies to an unknown MCP method.
pub const METHOD_NOT_FOUND: i64 = -32601;
/// Standard JSON-RPC error code for a recognised method called with
/// malformed / missing params.
pub const INVALID_PARAMS: i64 = -32602;
/// Standard JSON-RPC error code for an internal server error.
pub const INTERNAL_ERROR: i64 = -32603;

// ---------------------------------------------------------------------------
// Server-side envelopes (the `net mcp serve` shim direction)
// ---------------------------------------------------------------------------
//
// The wrap client always chooses its own `i64` request ids ([`JsonRpcRequest`]),
// but a *server* must reflect back whatever id the host chose — JSON-RPC allows
// a string **or** a number — so the serve side carries these permissive forms.

/// A JSON-RPC 2.0 request/response id: a number or a string. The shim echoes
/// it back verbatim so the host can correlate the response with its request.
///
/// Number is tried first (the common case); a JSON string falls through to
/// [`RequestId::Str`]. A `null` / absent id is modelled as `Option<RequestId>`
/// at the use site, not as a variant here.
///
/// The numeric variant carries a raw [`serde_json::Number`], not an `i64`, so
/// **any** JSON number the host chose round-trips losslessly — including one
/// outside `i64` range or (against the spec's advice) fractional. Modelling it
/// as `i64` made such an id fail the *whole* `IncomingMessage` parse, so the
/// shim answered `PARSE_ERROR` with a `null` id the host could not correlate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum RequestId {
    /// A numeric id (`{"id": 7}`), preserved verbatim as a JSON number.
    Number(serde_json::Number),
    /// A string id (`{"id": "abc"}`).
    Str(String),
}

/// How an incoming stdio line classifies once parsed. The stdio transport
/// interleaves requests and notifications on one line stream, so the shim
/// parses each line into an [`IncomingMessage`] then branches on this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncomingKind {
    /// Has both `method` and `id` — expects exactly one response.
    Request,
    /// Has `method`, no `id` — fire-and-forget, no response.
    Notification,
    /// Has `id`, no `method` — a response to a server-initiated request. The
    /// compat-tier shim sends no such requests, so these are ignored.
    Response,
    /// Neither `method` nor `id` — not a valid JSON-RPC message.
    Malformed,
}

/// A parsed incoming stdio line, before it is classified into a request /
/// notification. Every field is optional so a partially-formed message parses
/// (and is then rejected with a crisp JSON-RPC error) rather than failing the
/// whole read loop.
#[derive(Debug, Clone, Deserialize)]
pub struct IncomingMessage {
    #[serde(default)]
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Option<RequestId>,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub params: Option<serde_json::Value>,
}

impl IncomingMessage {
    /// Classify the message by the presence of `method` / `id`.
    pub fn kind(&self) -> IncomingKind {
        match (self.method.is_some(), self.id.is_some()) {
            (true, true) => IncomingKind::Request,
            (true, false) => IncomingKind::Notification,
            (false, true) => IncomingKind::Response,
            (false, false) => IncomingKind::Malformed,
        }
    }
}

/// A JSON-RPC 2.0 success response the shim writes back to the host.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JsonRpcSuccess {
    pub jsonrpc: String,
    pub id: RequestId,
    pub result: serde_json::Value,
}

impl JsonRpcSuccess {
    /// Build a success response with the JSON-RPC version tag filled in.
    pub fn new(id: RequestId, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            result,
        }
    }
}

/// A JSON-RPC 2.0 error response. The `id` is serialized as `null` (never
/// omitted — JSON-RPC requires the member present) when it could not be
/// determined from a malformed request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JsonRpcErrorResponse {
    pub jsonrpc: String,
    pub id: Option<RequestId>,
    pub error: JsonRpcError,
}

impl JsonRpcErrorResponse {
    /// Build an error response for a known request id.
    pub fn new(id: Option<RequestId>, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            error: JsonRpcError {
                code,
                message: message.into(),
                data: None,
            },
        }
    }
}

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
            version: SERVER_VERSION.into(),
        });
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(v["clientInfo"]["name"], "net");
        assert_eq!(v["clientInfo"]["version"], SERVER_VERSION);
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

    #[test]
    fn request_id_accepts_number_and_string() {
        // A host may correlate with a number or a string id; both round-trip
        // and reflect back verbatim.
        let n: RequestId = serde_json::from_str("7").unwrap();
        assert_eq!(n, RequestId::Number(serde_json::Number::from(7)));
        assert_eq!(serde_json::to_value(&n).unwrap(), serde_json::json!(7));

        let s: RequestId = serde_json::from_str("\"abc\"").unwrap();
        assert_eq!(s, RequestId::Str("abc".to_string()));
        assert_eq!(serde_json::to_value(&s).unwrap(), serde_json::json!("abc"));
    }

    #[test]
    fn request_id_round_trips_a_number_outside_i64_range() {
        // Regression (F12): a numeric id larger than i64::MAX must not fail the
        // whole IncomingMessage parse — it round-trips as a raw JSON number so
        // the shim can echo it back and the host can correlate the response.
        let big = "12345678901234567890"; // > i64::MAX
        let msg: IncomingMessage =
            serde_json::from_str(&format!(r#"{{"jsonrpc":"2.0","id":{big},"method":"x"}}"#))
                .expect("an out-of-i64-range id must still parse");
        assert_eq!(msg.kind(), IncomingKind::Request);
        let id = msg.id.expect("id present");
        // Echoing it into a response reproduces the original number verbatim.
        let echoed = serde_json::to_value(&JsonRpcSuccess::new(id, serde_json::json!({}))).unwrap();
        assert_eq!(echoed["id"], serde_json::json!(12345678901234567890u64));
    }

    #[test]
    fn incoming_message_classifies_request_notification_response_malformed() {
        let req: IncomingMessage =
            serde_json::from_value(serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "x" }))
                .unwrap();
        assert_eq!(req.kind(), IncomingKind::Request);

        let note: IncomingMessage =
            serde_json::from_value(serde_json::json!({ "jsonrpc": "2.0", "method": "x" })).unwrap();
        assert_eq!(note.kind(), IncomingKind::Notification);

        let resp: IncomingMessage =
            serde_json::from_value(serde_json::json!({ "jsonrpc": "2.0", "id": 1, "result": {} }))
                .unwrap();
        assert_eq!(resp.kind(), IncomingKind::Response);

        let bad: IncomingMessage =
            serde_json::from_value(serde_json::json!({ "jsonrpc": "2.0" })).unwrap();
        assert_eq!(bad.kind(), IncomingKind::Malformed);
    }

    #[test]
    fn success_response_carries_jsonrpc_tag_and_echoes_id() {
        let res = JsonRpcSuccess::new(
            RequestId::Str("q".into()),
            serde_json::json!({ "ok": true }),
        );
        let v = serde_json::to_value(&res).unwrap();
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["id"], "q");
        assert_eq!(v["result"]["ok"], true);
    }

    #[test]
    fn error_response_serializes_null_id_when_unknown() {
        // JSON-RPC requires the `id` member present even when it couldn't be
        // determined — it must be `null`, not omitted.
        let res = JsonRpcErrorResponse::new(None, PARSE_ERROR, "bad json");
        let v = serde_json::to_value(&res).unwrap();
        assert!(v.get("id").is_some(), "id member must be present");
        assert_eq!(v["id"], serde_json::Value::Null);
        assert_eq!(v["error"]["code"], PARSE_ERROR);
        assert_eq!(v["error"]["message"], "bad json");
    }
}
