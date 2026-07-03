//! Invoke side — bridge an incoming nRPC call to a wrapped MCP `tools/call`
//! (`MCP_BRIDGE_PLAN.md` Phase 1, `wrap/invoke.rs`).
//!
//! One [`WrapInvokeHandler`] is served per bridged tool (nRPC service name =
//! `tool_id`). Each call:
//!   1. **owner-scope gate** — the AEAD-verified `caller_origin` must be in
//!      the tool's [`OwnerScope`], else a structured rejection (the plan's
//!      "caller root identity does not match owner scope");
//!   2. **translate** — decode the request body as JSON tool arguments;
//!   3. **invoke** — `tools/call` on the wrapped [`StdioMcpClient`];
//!   4. **encode** — the `CallToolResult` back as the response body.
//!
//! Credentials never enter this path: they live in the wrapped server's
//! process on the owning machine (see [`super::stdio`]); only tool arguments
//! and results cross the mesh.

use std::collections::HashSet;
use std::sync::Arc;

use bytes::Bytes;
use net_sdk::mesh_rpc::{RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus};
use serde_json::Value;

use super::stdio::StdioMcpClient;
use crate::spec::CallToolResult;

/// Application error code (nRPC `RpcStatus::Application`) for a caller that
/// is outside the tool's owner scope. In the app-defined `0x8000..=0xFFFF`
/// range required by [`RpcHandlerError::Application`].
pub const ERR_OWNER_SCOPE: u16 = 0x8001;
/// The request body was not decodable JSON tool arguments.
pub const ERR_BAD_REQUEST: u16 = 0x8002;
/// The wrapped MCP server failed the call at the protocol level.
pub const ERR_UPSTREAM: u16 = 0x8003;
/// The tool ran and reported a tool-level error (`is_error`). Distinct from
/// `ERR_UPSTREAM` so a caller can tell "the tool said no" from "the bridge
/// couldn't reach the tool".
pub const ERR_TOOL: u16 = 0x8004;

/// Who may invoke a wrapped tool. Owner-only by default (doctrine #3): only
/// caller origins the operator has admitted. Widening flags
/// (`--allow peer:<node_id>`) add origins.
///
/// v0 gates on the AEAD-verified `caller_origin` (`origin_hash`) directly.
/// Mapping an origin to a *root* identity (so a delegated sub-identity of the
/// owner also passes) is a later refinement once the permission system lands;
/// until then "same root identity" is approximated by an explicit origin set,
/// which is honest — it never admits an origin the operator didn't list.
#[derive(Debug, Clone, Default)]
pub struct OwnerScope {
    allowed: HashSet<u64>,
}

impl OwnerScope {
    /// Owner-only: just the owning node's origin may invoke.
    pub fn owner_only(owner_origin: u64) -> Self {
        let mut allowed = HashSet::new();
        allowed.insert(owner_origin);
        Self { allowed }
    }

    /// Widen the scope to also admit `origin` (`net wrap --allow peer:<id>`).
    pub fn allow(&mut self, origin: u64) {
        self.allowed.insert(origin);
    }

    /// Is `caller_origin` admitted?
    pub fn allows(&self, caller_origin: u64) -> bool {
        self.allowed.contains(&caller_origin)
    }
}

/// Decode an nRPC request body into MCP tool arguments. An empty body means
/// "no arguments" (a tool taking no args), which lowers to an empty object.
pub fn parse_arguments(body: &[u8]) -> Result<Value, String> {
    if body.is_empty() {
        return Ok(Value::Object(serde_json::Map::new()));
    }
    serde_json::from_slice(body).map_err(|e| format!("invalid JSON tool arguments: {e}"))
}

/// Encode a `CallToolResult` as the nRPC response body (JSON).
pub fn encode_result(result: &CallToolResult) -> Result<Vec<u8>, String> {
    serde_json::to_vec(result).map_err(|e| format!("encode tool result: {e}"))
}

/// Serves one wrapped MCP tool over nRPC. Install with
/// `Mesh::serve_rpc(tool_id, Arc::new(handler))`.
pub struct WrapInvokeHandler {
    client: Arc<StdioMcpClient>,
    tool: String,
    scope: OwnerScope,
}

impl WrapInvokeHandler {
    /// Build a handler that invokes `tool` on `client`, admitting only
    /// callers in `scope`.
    pub fn new(client: Arc<StdioMcpClient>, tool: impl Into<String>, scope: OwnerScope) -> Self {
        Self {
            client,
            tool: tool.into(),
            scope,
        }
    }

    /// A successful nRPC response carrying `body`.
    fn ok(body: Vec<u8>) -> RpcResponsePayload {
        RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: Vec::new(),
            body: Bytes::from(body),
        }
    }
}

#[async_trait::async_trait]
impl RpcHandler for WrapInvokeHandler {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        // [1] Owner-scope gate — the wrapper-side identity check. The origin
        //     is AEAD-verified by the bus, not self-claimed, so this cannot
        //     be spoofed from the request body.
        if !self.scope.allows(ctx.caller_origin) {
            return Err(RpcHandlerError::Application {
                code: ERR_OWNER_SCOPE,
                message: "caller root identity does not match owner scope".to_string(),
            });
        }

        // [2] Translate the request body into tool arguments.
        let arguments =
            parse_arguments(&ctx.payload.body).map_err(|message| RpcHandlerError::Application {
                code: ERR_BAD_REQUEST,
                message,
            })?;

        // [3] Invoke the wrapped tool. A protocol failure (server gone,
        //     JSON-RPC error) is ERR_UPSTREAM; a tool-level `is_error`
        //     result is ERR_TOOL — kept distinct so the caller can tell
        //     "unreachable" from "the tool refused".
        let result = self
            .client
            .call_tool(&self.tool, arguments)
            .await
            .map_err(|e| RpcHandlerError::Application {
                code: ERR_UPSTREAM,
                message: e.to_string(),
            })?;

        let body = encode_result(&result).map_err(RpcHandlerError::Internal)?;
        if result.is_error {
            return Err(RpcHandlerError::Application {
                code: ERR_TOOL,
                message: result.text(),
            });
        }
        Ok(Self::ok(body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_scope_admits_only_listed_origins() {
        let mut scope = OwnerScope::owner_only(7);
        assert!(scope.allows(7), "owner passes");
        assert!(!scope.allows(9), "a stranger is rejected");
        scope.allow(9);
        assert!(scope.allows(9), "widening admits the peer");
    }

    #[test]
    fn empty_scope_admits_nobody() {
        let scope = OwnerScope::default();
        assert!(!scope.allows(0));
        assert!(!scope.allows(7));
    }

    #[test]
    fn parse_arguments_handles_empty_object_and_invalid() {
        assert_eq!(parse_arguments(b"").unwrap(), serde_json::json!({}));
        assert_eq!(
            parse_arguments(br#"{"message":"hi"}"#).unwrap(),
            serde_json::json!({ "message": "hi" }),
        );
        assert!(parse_arguments(b"not json").is_err());
    }

    #[test]
    fn encode_result_round_trips() {
        let result = CallToolResult::text_ok("hello");
        let bytes = encode_result(&result).unwrap();
        let back: CallToolResult = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back.text(), "hello");
        assert!(!back.is_error);
    }
}
