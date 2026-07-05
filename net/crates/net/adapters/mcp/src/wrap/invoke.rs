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

use super::delegation::{DelegationGate, HDR_DELEGATION, HDR_DELEGATION_SIG};
use super::stdio::StdioMcpClient;
use crate::spec::CallToolResult;

/// Application error code (nRPC `RpcStatus::Application`) for a caller that
/// is outside the tool's owner scope. In the app-defined `0x8000..=0xFFFF`
/// range required by [`RpcHandlerError::Application`].
pub const ERR_OWNER_SCOPE: u16 = 0x8001;

/// The canonical owner-scope rejection message the gate returns with
/// [`ERR_OWNER_SCOPE`]. Shared so every producer (this invoke handler, the
/// describe handler) and the demand-side shim that canonicalizes it
/// (`serve::shim` → [`MSG_DENIED_BY_WRAPPER`](crate::serve::MSG_DENIED_BY_WRAPPER))
/// reference one literal — reword it here and both sides move together, rather
/// than the shim silently failing to recognise a reworded rejection.
pub const OWNER_SCOPE_REJECTION: &str = "caller root identity does not match owner scope";
/// The request body was not decodable JSON tool arguments.
pub const ERR_BAD_REQUEST: u16 = 0x8002;
/// The wrapped MCP server failed the call at the protocol level.
pub const ERR_UPSTREAM: u16 = 0x8003;
/// The tool ran and reported a tool-level error (`is_error`). Distinct from
/// `ERR_UPSTREAM` so a caller can tell "the tool said no" from "the bridge
/// couldn't reach the tool".
pub const ERR_TOOL: u16 = 0x8004;
/// A delegated invoke (carrying a [`HDR_DELEGATION`] chain) failed
/// verification — bad/missing signature, replay, stale, revoked, or a chain
/// that doesn't root at the provider's owner. Distinct from
/// [`ERR_OWNER_SCOPE`] (the no-chain origin-allowlist path).
pub const ERR_DELEGATION: u16 = 0x8005;

/// Find the first request header named `name` in `ctx.payload.headers`
/// (`Vec<(String, Vec<u8>)>` — the substrate's `RpcHeader` alias). First match
/// wins, mirroring `predicate_from_rpc_headers`.
fn find_header<'a>(headers: &'a [(String, Vec<u8>)], name: &str) -> Option<&'a [u8]> {
    headers
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.as_slice())
}

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
    /// Admit every caller — the mechanism behind the (deferred) `--public`
    /// exposure. Never set from the CLI in v0 (owner-only is the default).
    all: bool,
}

impl OwnerScope {
    /// Owner-only: just the owning node's origin may invoke.
    pub fn owner_only(owner_origin: u64) -> Self {
        let mut allowed = HashSet::new();
        allowed.insert(owner_origin);
        Self {
            allowed,
            all: false,
        }
    }

    /// Admit **any** caller — the eventual backing for `net wrap --public`,
    /// which is deferred (not in v0), so this is not reachable from the CLI.
    /// Provided for explicit public opt-in and to isolate the invoke/translate
    /// path from the gate in tests.
    pub fn any() -> Self {
        Self {
            allowed: HashSet::new(),
            all: true,
        }
    }

    /// Widen the scope to also admit `origin` (`net wrap --allow peer:<id>`).
    pub fn allow(&mut self, origin: u64) {
        self.allowed.insert(origin);
    }

    /// Is `caller_origin` admitted?
    pub fn allows(&self, caller_origin: u64) -> bool {
        self.all || self.allowed.contains(&caller_origin)
    }
}

/// Decode an nRPC request body into MCP tool arguments. An empty body means
/// "no arguments" (a tool taking no args), which lowers to an empty object.
/// MCP `tools/call` arguments must be a JSON **object**; a non-object body
/// (array / null / primitive) is a caller error, reported crisply here rather
/// than becoming a confusing upstream/tool failure.
pub fn parse_arguments(body: &[u8]) -> Result<Value, String> {
    if body.is_empty() {
        return Ok(Value::Object(serde_json::Map::new()));
    }
    let value: Value =
        serde_json::from_slice(body).map_err(|e| format!("invalid JSON tool arguments: {e}"))?;
    if !value.is_object() {
        return Err("tool arguments must be a JSON object".to_string());
    }
    Ok(value)
}

/// Encode a `CallToolResult` as the nRPC response body (JSON).
pub fn encode_result(result: &CallToolResult) -> Result<Vec<u8>, String> {
    serde_json::to_vec(result).map_err(|e| format!("encode tool result: {e}"))
}

/// Serves one wrapped MCP tool over nRPC. Install with
/// `Mesh::serve_rpc(tool_id, Arc::new(handler))`.
pub struct WrapInvokeHandler {
    client: Arc<StdioMcpClient>,
    /// The wrapped tool's MCP name — what `tools/call` is issued against.
    tool: String,
    /// The nRPC service name this handler is served under (the channel-safe
    /// `tool_id`) — what the caller invokes and signs the delegation challenge
    /// over. Equals `tool` unless the name was sanitized.
    service: String,
    scope: OwnerScope,
    /// Optional delegation gate (Phase 3 Slice B). When set, an invoke that
    /// carries a [`HDR_DELEGATION`] chain is admitted iff the chain + its
    /// per-invoke signature verify; a no-chain invoke still uses `scope`.
    delegation: Option<Arc<DelegationGate>>,
}

impl WrapInvokeHandler {
    /// Build a handler that invokes `tool` on `client`, admitting only
    /// callers in `scope`. No delegation gate — use [`Self::with_delegation`]
    /// to add one.
    pub fn new(client: Arc<StdioMcpClient>, tool: impl Into<String>, scope: OwnerScope) -> Self {
        let tool = tool.into();
        Self {
            client,
            service: tool.clone(),
            tool,
            scope,
            delegation: None,
        }
    }

    /// Set the nRPC service name (the served `tool_id`) used for the delegation
    /// challenge. Defaults to `tool`; the serve site sets it to the `tool_id`
    /// so the caller-signed and provider-verified challenge agree even when the
    /// name was sanitized (`tool_id != mcp_name`).
    #[must_use]
    pub fn with_service(mut self, service: impl Into<String>) -> Self {
        self.service = service.into();
        self
    }

    /// Attach a delegation gate (or `None` to leave the owner-scope-only
    /// behavior). Builder-style so `WrapInvokeHandler::new(...).with_delegation(g)`
    /// reads cleanly at the serve site.
    #[must_use]
    pub fn with_delegation(mut self, delegation: Option<Arc<DelegationGate>>) -> Self {
        self.delegation = delegation;
        self
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
        // [1] Admission. A caller that presents a delegation chain
        //     (`net-delegation`) is admitted iff the chain + its per-invoke
        //     leaf signature verify against the provider's owner root — sound
        //     even though `caller_origin` is spoofable within a channel
        //     (`identity/origin.rs`), because the *signature* proves leaf-key
        //     possession. A caller with no chain falls back to the owner-scope
        //     origin allowlist (the AEAD-verified `caller_origin`), unchanged.
        match (
            &self.delegation,
            find_header(&ctx.payload.headers, HDR_DELEGATION),
        ) {
            (Some(gate), Some(chain_bytes)) => {
                let sig =
                    find_header(&ctx.payload.headers, HDR_DELEGATION_SIG).ok_or_else(|| {
                        RpcHandlerError::Application {
                            code: ERR_DELEGATION,
                            message: "delegation chain present but signature header missing"
                                .to_string(),
                        }
                    })?;
                // Fail-closed: any verification error rejects the invoke; the
                // gate audits the admitted leaf on success. Verify over the
                // SERVICE name (tool_id) the caller signed, not the internal
                // mcp_name.
                gate.verify(&self.service, &ctx.payload.body, chain_bytes, sig)
                    .map_err(|e| RpcHandlerError::Application {
                        code: ERR_DELEGATION,
                        message: e.to_string(),
                    })?;
            }
            _ => {
                // No chain (or no gate configured): owner-scope path.
                if !self.scope.allows(ctx.caller_origin) {
                    return Err(RpcHandlerError::Application {
                        code: ERR_OWNER_SCOPE,
                        message: OWNER_SCOPE_REJECTION.to_string(),
                    });
                }
            }
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

        // [4] Encode the result. Do it per-branch: a tool-level error returns
        //     the full structured result as the ERR_TOOL body via
        //     `tool_error_message` (which never fails — it can't mask the tool
        //     error as an internal one); a success encodes the response body,
        //     where an encode failure genuinely is internal.
        if result.is_error {
            return Err(RpcHandlerError::Application {
                code: ERR_TOOL,
                message: tool_error_message(&result),
            });
        }
        let body = encode_result(&result).map_err(RpcHandlerError::Internal)?;
        Ok(Self::ok(body))
    }
}

/// The `ERR_TOOL` error body for a tool-level failure: the full structured
/// `CallToolResult` as JSON so the caller keeps every content block and any
/// `structured_content`, falling back to just the text blocks if it
/// (implausibly) won't serialize — so a tool error is never masked by an
/// encode failure.
fn tool_error_message(result: &CallToolResult) -> String {
    encode_result(result)
        .map(|body| String::from_utf8_lossy(&body).into_owned())
        .unwrap_or_else(|_| result.text())
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
    fn any_scope_admits_everyone() {
        let scope = OwnerScope::any();
        assert!(scope.allows(0));
        assert!(scope.allows(0xDEAD_BEEF));
        assert!(scope.allows(u64::MAX));
    }

    #[test]
    fn parse_arguments_handles_empty_object_and_invalid() {
        assert_eq!(parse_arguments(b"").unwrap(), serde_json::json!({}));
        assert_eq!(
            parse_arguments(br#"{"message":"hi"}"#).unwrap(),
            serde_json::json!({ "message": "hi" }),
        );
        assert!(parse_arguments(b"not json").is_err());
        // Valid JSON that is not an object is a caller error — MCP tool
        // arguments must be an object.
        for non_object in [&b"[]"[..], b"null", b"5", br#""str""#] {
            assert!(
                parse_arguments(non_object).is_err(),
                "non-object body must be rejected: {:?}",
                std::str::from_utf8(non_object),
            );
        }
    }

    #[test]
    fn encode_result_round_trips() {
        let result = CallToolResult::text_ok("hello");
        let bytes = encode_result(&result).unwrap();
        let back: CallToolResult = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back.text(), "hello");
        assert!(!back.is_error);
    }

    #[test]
    fn tool_error_message_preserves_the_full_structured_result() {
        let mut result = CallToolResult::text_error("boom");
        result.structured_content = Some(serde_json::json!({ "code": 42 }));
        // The ERR_TOOL body is the full JSON result, not just its text — so the
        // caller keeps every content block and any structured_content.
        let decoded: CallToolResult = serde_json::from_str(&tool_error_message(&result)).unwrap();
        assert!(decoded.is_error);
        assert_eq!(decoded.text(), "boom");
        assert_eq!(
            decoded.structured_content,
            Some(serde_json::json!({ "code": 42 })),
        );
    }
}
