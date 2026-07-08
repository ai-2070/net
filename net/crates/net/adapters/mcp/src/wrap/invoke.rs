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
use super::policy::{InvokePolicy, PolicyContext, PolicyDecision};
use super::stdio::StdioMcpClient;
use super::McpError;
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
/// A paid tool's payment admission failed — the invocation carried no
/// [`HDR_PAYMENT_QUOTE`](crate::serve::payment::HDR_PAYMENT_QUOTE), the
/// quote was unpaid / frozen / already redeemed / bound to another tool,
/// or the tool is priced but no payment gate is configured (fail-closed).
/// An authorization verdict like its owner-scope and delegation siblings.
pub const ERR_PAYMENT: u16 = net_sdk::tool_payment::ERR_PAYMENT;
/// The invoke was admitted but the provider's [`InvokePolicy`] refused it
/// (V2 Phase 2's in-root toll booth — an allowlist deny, or a dangerous-tool
/// approval that was declined / could not reach the operator). An
/// authorization verdict, so the demand side maps it to `denied` like
/// [`ERR_OWNER_SCOPE`] / [`ERR_DELEGATION`], never a tool-level `is_error`.
/// (0x8007: 0x8006 was taken by [`ERR_PAYMENT`] when the branches merged.)
pub const ERR_POLICY: u16 = 0x8007;

/// Find the first request header named `name` in `ctx.payload.headers`
/// (`Vec<(String, Vec<u8>)>` — the substrate's `RpcHeader` alias). First match
/// wins, mirroring `predicate_from_rpc_headers`.
fn find_header<'a>(headers: &'a [(String, Vec<u8>)], name: &str) -> Option<&'a [u8]> {
    headers
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.as_slice())
}

/// A payment refusal on the full-fidelity reply channel: the human
/// message stays the body (byte-identical to the pre-schematic wire)
/// and the schematic rides the reply header — exactly one, dropped
/// (never truncated) if it exceeds the wire budget. Returned as
/// `Ok(payload)` because the fold passes a handler-authored payload
/// through verbatim; the `RpcHandlerError` convenience channel
/// flattens headers away. Twin of the SDK-native `PaidToolHandler`'s
/// helper — wire-identical refusals on both serving paths.
fn payment_refusal(
    message: String,
    schematic: &net_sdk::tool_payment::FailureSchematic,
) -> RpcResponsePayload {
    RpcResponsePayload {
        status: RpcStatus::Application(ERR_PAYMENT),
        headers: schematic.header_entry().into_iter().collect(),
        body: Bytes::from(message),
    }
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

/// The invoke seam a served tool actually calls when a mesh request arrives.
///
/// [`WrapInvokeHandler`] is generic over this so the *same*
/// announce/describe/owner-scope/delegation path backs two producers that
/// differ in exactly one step:
///
/// - `net wrap` — a wrapped stdio MCP server ([`StdioMcpClient`], the blanket
///   impl below): the mesh call becomes a `tools/call` on the child process.
/// - a native node publishing its **own** in-process toolset
///   ([`ServerPublisher::publish_tools`](super::session::ServerPublisher::publish_tools)):
///   the mesh call is dispatched to a local (e.g. callback-backed) invoker.
///
/// `name` is the tool's original name ([`super::descriptor::LoweredTool::mcp_name`])
/// and `arguments` the caller's decoded JSON object. The result contract
/// mirrors [`StdioMcpClient::call_tool`]: an `Ok` [`CallToolResult`] with
/// `is_error == true` is a **tool-level** failure the tool reported in-band
/// (surfaced as `ERR_TOOL`), while an `Err(McpError)` is a transport/protocol
/// failure (`ERR_UPSTREAM`) — the two are kept apart so a caller can tell
/// "the tool refused" from "the tool was unreachable".
#[async_trait::async_trait]
pub trait ToolInvoker: Send + Sync {
    /// Invoke `name` with `arguments`, returning the tool's structured result.
    async fn call_tool(&self, name: &str, arguments: Value) -> Result<CallToolResult, McpError>;
}

/// `net wrap`'s invoker: dispatch to the wrapped stdio MCP server's
/// `tools/call`. The behavior `publish_server` has always used, now expressed
/// through the shared seam so `publish_tools` can substitute a different one.
#[async_trait::async_trait]
impl ToolInvoker for StdioMcpClient {
    async fn call_tool(&self, name: &str, arguments: Value) -> Result<CallToolResult, McpError> {
        // Fully-qualified to call the inherent method (no trait recursion).
        StdioMcpClient::call_tool(self, name, arguments).await
    }
}

/// Serves one bridged tool over nRPC. Install with
/// `Mesh::serve_rpc(tool_id, Arc::new(handler))`. Backed by any
/// [`ToolInvoker`] — a wrapped MCP server or a node's own local tools.
pub struct WrapInvokeHandler {
    invoker: Arc<dyn ToolInvoker>,
    /// The tool's original name — what [`ToolInvoker::call_tool`] is issued
    /// against.
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
    /// Optional invoke-path policy (V2 Phase 2). When set, an *admitted* invoke
    /// is additionally run past it before the tool executes — the in-root toll
    /// booth. `None` is the allow-all preset (the check is skipped): the mesh
    /// adds reach, not authority.
    policy: Option<Arc<dyn InvokePolicy>>,
    /// Whether this tool was published with pricing terms. A paid tool's
    /// invoke must redeem its quote through `payment` before the wrapped
    /// tool runs — a paid tool with no gate configured rejects everything
    /// (fail-closed; the publish site also validates this up front).
    paid: bool,
    /// The provider's payment admission (the net-payments engine, behind
    /// the [`crate::serve::payment::PaymentAdmission`] seam).
    payment: Option<Arc<dyn crate::serve::payment::PaymentAdmission>>,
}

impl WrapInvokeHandler {
    /// Build a handler that invokes `tool` on `invoker`, admitting only
    /// callers in `scope`. No delegation gate — use [`Self::with_delegation`]
    /// to add one. `invoker` is any [`ToolInvoker`]; an `Arc<StdioMcpClient>`
    /// coerces here directly (the `net wrap` path).
    pub fn new(invoker: Arc<dyn ToolInvoker>, tool: impl Into<String>, scope: OwnerScope) -> Self {
        let tool = tool.into();
        Self {
            invoker,
            service: tool.clone(),
            tool,
            scope,
            delegation: None,
            policy: None,
            paid: false,
            payment: None,
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

    /// Attach an invoke-path policy (V2 Phase 2), or `None` for the allow-all
    /// preset (the check is skipped). Builder-style so
    /// `WrapInvokeHandler::new(...).with_policy(p)` reads cleanly at the serve
    /// site, alongside `with_delegation`.
    #[must_use]
    pub fn with_policy(mut self, policy: Option<Arc<dyn InvokePolicy>>) -> Self {
        self.policy = policy;
        self
    }

    /// Mark this tool as paid and attach the provider's payment admission
    /// gate. `paid = true` with `payment = None` is a valid (fail-closed)
    /// configuration only as a defense in depth — the publish site rejects
    /// it before serving.
    #[must_use]
    pub fn with_payment(
        mut self,
        paid: bool,
        payment: Option<Arc<dyn crate::serve::payment::PaymentAdmission>>,
    ) -> Self {
        self.paid = paid;
        self.payment = payment;
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
        let delegated = match (
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
                true
            }
            _ => {
                // No chain (or no gate configured): owner-scope path.
                if !self.scope.allows(ctx.caller_origin) {
                    return Err(RpcHandlerError::Application {
                        code: ERR_OWNER_SCOPE,
                        message: OWNER_SCOPE_REJECTION.to_string(),
                    });
                }
                false
            }
        };

        // [2] Translate the request body into tool arguments — BEFORE the
        //     payment and policy gates, so a structurally invalid call (one
        //     that can never execute) is rejected as ERR_BAD_REQUEST without
        //     burning the caller's payment quote or consulting the policy.
        //     A real approval policy may prompt a human operator; asking them
        //     to approve garbage would be noise at best and an
        //     approval-fatigue vector at worst.
        let arguments =
            parse_arguments(&ctx.payload.body).map_err(|message| RpcHandlerError::Application {
                code: ERR_BAD_REQUEST,
                message,
            })?;

        // [3] Payment admission (paid tools only). The quote id the caller
        //     attached is redeemed against the provider's payment engine —
        //     settled, billed, unfrozen, bound to this tool, and never
        //     redeemed before. Runs AFTER identity admission (never leak
        //     payment state to a caller the scope would reject) and BEFORE
        //     the wrapped tool: the handler never sees an unpaid call, no
        //     matter what the demand side did or skipped. It also runs before
        //     the policy hook — a caller commits its quote before it can put
        //     a (possibly human) approval prompt in front of the operator.
        if self.paid {
            // Refusals ride the full-fidelity channel (`Ok(payload)` —
            // the fold passes it through verbatim): the human message
            // stays the body, byte-identical to the pre-schematic wire,
            // and the failure schematic rides the reply header.
            let Some(gate) = &self.payment else {
                let schematic =
                    net_sdk::tool_payment::FailureSchematic::gate_missing(&self.service);
                let message = schematic.message.clone();
                return Ok(payment_refusal(message, &schematic));
            };
            let quote_id = match find_header(
                &ctx.payload.headers,
                crate::serve::payment::HDR_PAYMENT_QUOTE,
            )
            .and_then(|raw| std::str::from_utf8(raw).ok())
            {
                Some(quote_id) => quote_id,
                None => {
                    let schematic =
                        net_sdk::tool_payment::FailureSchematic::missing_quote(&self.service);
                    let message = schematic.message.clone();
                    return Ok(payment_refusal(message, &schematic));
                }
            };
            let binding = find_header(
                &ctx.payload.headers,
                crate::serve::payment::HDR_PAYMENT_BINDING,
            );
            if let Err(denial) = gate.redeem(&self.service, quote_id, binding).await {
                return Ok(payment_refusal(denial.message, &denial.schematic));
            }
        }

        // [4] Policy hook (V2 Phase 2, the in-root toll booth). The call is
        //     admitted, well-formed, and paid for; now the provider's policy —
        //     an allowlist, or a dangerous-tool approval that routes to the
        //     operator — gets the final say before the tool runs. `None` is
        //     the allow-all preset (skipped): the mesh adds reach, not
        //     authority. A deny becomes an ERR_POLICY the demand side reports
        //     as `denied`.
        if let Some(policy) = &self.policy {
            if let PolicyDecision::Deny { reason } = policy
                .check(&PolicyContext {
                    tool_id: self.service.clone(),
                    caller_origin: ctx.caller_origin,
                    delegated,
                })
                .await
            {
                return Err(RpcHandlerError::Application {
                    code: ERR_POLICY,
                    message: reason,
                });
            }
        }

        // [5] Invoke the wrapped tool. A protocol failure (server gone,
        //     JSON-RPC error) is ERR_UPSTREAM; a tool-level `is_error`
        //     result is ERR_TOOL — kept distinct so the caller can tell
        //     "unreachable" from "the tool refused".
        let result = self
            .invoker
            .call_tool(&self.tool, arguments)
            .await
            .map_err(|e| RpcHandlerError::Application {
                code: ERR_UPSTREAM,
                message: e.to_string(),
            })?;

        // [6] Encode the result. Do it per-branch: a tool-level error returns
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
