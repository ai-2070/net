//! `net-mcp-fixture` — the conformance fixture: a hermetic stdio MCP server
//! with a deterministic, controllable tool set that acceptance runs against
//! (`MCP_BRIDGE_PLAN.md` cross-cutting workstreams). No network, no real
//! credentials, no external processes — you own it, so you can make it
//! misbehave on command.
//!
//! Speaks newline-delimited JSON-RPC 2.0 over stdin/stdout, synchronously
//! (a plain blocking read loop — a test double needs no async runtime).
//!
//! Tool set:
//!   - `echo`  — returns its `message` argument (deterministic baseline)
//!   - `add`   — returns `a + b` (typed-args baseline)
//!   - `boom`  — a tool-level failure (`is_error = true`), NOT a protocol error
//!   - `slow`  — sleeps `ms` milliseconds before replying (latency injection)
//!   - `big`   — returns a `size`-byte string (large-payload)
//!   - `_bump` — flips the tool set + emits `tools/list_changed` (live updates)
//!   - `bonus` — present only after a `_bump`
//!
//! The credential/token-leak behavior is exercised by spawning the fixture
//! with a sentinel env var; the fixture never reads its env, so any tool's
//! result proves a credential doesn't transit (see the stdio tests).

use std::io::{BufRead, Write};

use net_mcp::spec::{
    self, CallToolParams, CallToolResult, Implementation, InitializeResult, ListToolsResult, Tool,
    METHOD_NOT_FOUND,
};
use serde_json::{json, Value};

/// JSON-RPC "invalid params" — returned for a `tools/call` naming a tool the
/// fixture does not define.
const INVALID_PARAMS: i64 = -32602;

fn main() -> std::io::Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    // Injectable state: calling `_bump` flips this, which changes `tools/list`
    // (the `bonus` tool appears) and makes the server emit a
    // `tools/list_changed` notification — the live-descriptor-update behavior.
    let mut bumped = false;

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        // Skip anything that isn't a JSON object — a real host never sends
        // it, and a robust server shouldn't crash on stray input.
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let method = msg.get("method").and_then(Value::as_str);
        let id = msg.get("id").cloned();

        match (method, id) {
            // A request (method + id) — dispatch and reply.
            (Some(method), Some(id)) => {
                let (response, emit_changed) =
                    handle_request(method, id, msg.get("params"), &mut bumped);
                if let Ok(text) = serde_json::to_string(&response) {
                    writeln!(stdout, "{text}")?;
                    stdout.flush()?;
                }
                // A `_bump` call changes the tool set — notify the client so it
                // re-reads `tools/list`.
                if emit_changed {
                    let note = json!({
                        "jsonrpc": spec::JSONRPC_VERSION,
                        "method": spec::method::TOOLS_LIST_CHANGED,
                    });
                    if let Ok(text) = serde_json::to_string(&note) {
                        writeln!(stdout, "{text}")?;
                        stdout.flush()?;
                    }
                }
                // Right after the handshake, emit a SERVER-initiated request.
                // The bridge doesn't support these; it must answer
                // method-not-found OFF its read path and keep serving (never
                // block the reader and deadlock). This exercises that path.
                if method == spec::method::INITIALIZE {
                    let server_req = json!({
                        "jsonrpc": spec::JSONRPC_VERSION,
                        "id": 9001,
                        "method": "sampling/createMessage",
                        "params": {},
                    });
                    if let Ok(text) = serde_json::to_string(&server_req) {
                        writeln!(stdout, "{text}")?;
                        stdout.flush()?;
                    }
                }
            }
            // A notification (method, no id) — e.g. notifications/initialized.
            // Nothing to answer.
            (Some(_), None) => {}
            // Anything else is not a call we handle.
            _ => {}
        }
    }
    Ok(())
}

/// Build the JSON-RPC response for one request, plus whether a
/// `tools/list_changed` notification should follow it.
fn handle_request(
    method: &str,
    id: Value,
    params: Option<&Value>,
    bumped: &mut bool,
) -> (Value, bool) {
    match method {
        spec::method::INITIALIZE => (ok(id, initialize_result()), false),
        spec::method::TOOLS_LIST => (ok(id, tools_list_result(*bumped)), false),
        spec::method::TOOLS_CALL => handle_tools_call(id, params, bumped),
        // Unknown method.
        other => (
            err(id, METHOD_NOT_FOUND, format!("method not found: {other}")),
            false,
        ),
    }
}

/// The `initialize` result: pinned protocol version + a tools capability
/// advertising `listChanged`.
fn initialize_result() -> Value {
    let result = InitializeResult {
        protocol_version: spec::PROTOCOL_VERSION.to_string(),
        capabilities: json!({ "tools": { "listChanged": true } }),
        server_info: Implementation {
            name: "net-mcp-fixture".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
        instructions: None,
    };
    serde_json::to_value(result).unwrap_or_else(|_| json!({}))
}

/// The tool set. Deterministic, except that `bonus` appears only after a
/// `_bump` call — the schema-change-on-command behavior.
fn tools_list_result(bumped: bool) -> Value {
    let mut tools = vec![
        Tool {
            name: "echo".to_string(),
            title: Some("Echo".to_string()),
            description: Some("Return the message argument unchanged.".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": { "message": { "type": "string" } },
                "required": ["message"]
            }),
            output_schema: None,
        },
        Tool {
            name: "add".to_string(),
            title: Some("Add".to_string()),
            description: Some("Return the sum of a and b.".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": { "a": { "type": "number" }, "b": { "type": "number" } },
                "required": ["a", "b"]
            }),
            output_schema: None,
        },
        Tool {
            name: "boom".to_string(),
            title: Some("Boom".to_string()),
            description: Some("Always fails at the tool level (is_error).".to_string()),
            input_schema: json!({ "type": "object", "properties": {} }),
            output_schema: None,
        },
        Tool {
            name: "slow".to_string(),
            title: Some("Slow".to_string()),
            description: Some("Sleep `ms` milliseconds, then reply.".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": { "ms": { "type": "integer", "minimum": 0 } }
            }),
            output_schema: None,
        },
        Tool {
            name: "big".to_string(),
            title: Some("Big".to_string()),
            description: Some("Return a string of `size` bytes (large-payload).".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": { "size": { "type": "integer", "minimum": 0 } }
            }),
            output_schema: None,
        },
        Tool {
            name: "_bump".to_string(),
            title: Some("Bump".to_string()),
            description: Some("Change the tool set and emit tools/list_changed.".to_string()),
            input_schema: json!({ "type": "object", "properties": {} }),
            output_schema: None,
        },
        // A camelCase name — NOT a valid channel id. The wrap layer must
        // sanitize it into a safe service id and still invoke `getCaps`.
        Tool {
            name: "getCaps".to_string(),
            title: Some("Get Caps".to_string()),
            description: Some("A non-channel-safe (camelCase) tool name.".to_string()),
            input_schema: json!({ "type": "object", "properties": {} }),
            output_schema: None,
        },
    ];
    if bumped {
        tools.push(Tool {
            name: "bonus".to_string(),
            title: Some("Bonus".to_string()),
            description: Some("Appears only after a `_bump`.".to_string()),
            input_schema: json!({ "type": "object", "properties": {} }),
            output_schema: None,
        });
    }
    let result = ListToolsResult {
        tools,
        next_cursor: None,
    };
    serde_json::to_value(result).unwrap_or_else(|_| json!({ "tools": [] }))
}

/// Dispatch a `tools/call`. Returns the response plus whether a
/// `tools/list_changed` notification should follow (true only for `_bump`).
fn handle_tools_call(id: Value, params: Option<&Value>, bumped: &mut bool) -> (Value, bool) {
    let call: CallToolParams = match params.map(|p| serde_json::from_value(p.clone())) {
        Some(Ok(c)) => c,
        _ => {
            return (
                err(id, INVALID_PARAMS, "invalid tools/call params".to_string()),
                false,
            )
        }
    };
    let args = &call.arguments;

    let mut emit_changed = false;
    let result = match call.name.as_str() {
        "echo" => {
            let message = args
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default();
            CallToolResult::text_ok(message)
        }
        "add" => {
            let a = args.get("a").and_then(Value::as_f64).unwrap_or(0.0);
            let b = args.get("b").and_then(Value::as_f64).unwrap_or(0.0);
            let sum = a + b;
            // Print integer sums without a trailing `.0`.
            if sum.fract() == 0.0 {
                CallToolResult::text_ok((sum as i64).to_string())
            } else {
                CallToolResult::text_ok(sum.to_string())
            }
        }
        "boom" => CallToolResult::text_error("intentional fixture failure"),
        "slow" => {
            let ms = args.get("ms").and_then(Value::as_u64).unwrap_or(0);
            std::thread::sleep(std::time::Duration::from_millis(ms));
            CallToolResult::text_ok(format!("slept {ms}ms"))
        }
        "big" => {
            let size = args.get("size").and_then(Value::as_u64).unwrap_or(0) as usize;
            CallToolResult::text_ok("x".repeat(size))
        }
        "_bump" => {
            *bumped = true;
            emit_changed = true;
            CallToolResult::text_ok("bumped")
        }
        "bonus" => CallToolResult::text_ok("bonus"),
        "getCaps" => CallToolResult::text_ok("caps-ok"),
        // Unknown tool → a protocol-level error, not a tool result.
        other => {
            return (
                err(id, INVALID_PARAMS, format!("unknown tool: {other}")),
                false,
            )
        }
    };

    let response = match serde_json::to_value(result) {
        Ok(value) => ok(id, value),
        Err(_) => err(
            id,
            INVALID_PARAMS,
            "result serialization failed".to_string(),
        ),
    };
    (response, emit_changed)
}

/// A JSON-RPC success envelope.
fn ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": spec::JSONRPC_VERSION, "id": id, "result": result })
}

/// A JSON-RPC error envelope.
fn err(id: Value, code: i64, message: String) -> Value {
    json!({
        "jsonrpc": spec::JSONRPC_VERSION,
        "id": id,
        "error": { "code": code, "message": message }
    })
}
