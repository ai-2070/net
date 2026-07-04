//! Provider-side describe service (`MCP_BRIDGE_PLAN.md` Phase 2, option B).
//!
//! A wrap node serves one [`DescribeHandler`] on [`bridge::DESCRIBE_SERVICE`]
//! so a demand-side gateway can read the full descriptors of its bridged tools
//! — input schema, description, and credential status — which the announced
//! capability metadata carries but the public SDK does not surface to a
//! discovering node. This is additive: it does not touch the announce / serve
//! / owner-scope path the invoke side already ships.
//!
//! The service is gated by the **same [`OwnerScope`]** as invoke: describe is
//! visibility, and wrapped tools are visible only to the owner scope by default
//! (doctrine #3). A caller admitted to invoke can describe; one that is not is
//! denied on both, with the same structured rejection.
//!
//! Only classification *labels* cross this path, never secrets — the same
//! token-leak invariant the announce path holds (`super::session`).

use std::sync::Arc;

use bytes::Bytes;
use net_sdk::mesh_rpc::{RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus};
use tokio::sync::RwLock;

use super::descriptor::{
    compat_tier_key, credential_status_key, invocation_scope_key, substitutability_key,
    visibility_key, LoweredTool,
};
use super::invoke::{OwnerScope, ERR_BAD_REQUEST, ERR_OWNER_SCOPE};
use crate::bridge::{BridgedToolInfo, DescribeRequest, DescribeResponse};

/// The shared, swappable catalog a [`DescribeHandler`] reads. `refresh` swaps
/// the inner `Arc` so a describe in flight keeps its consistent snapshot and
/// readers clone only a pointer.
pub type SharedCatalog = Arc<RwLock<Arc<DescribeResponse>>>;

/// Build the describe catalog from the lowered tools.
pub fn build_catalog(lowered: &[LoweredTool]) -> DescribeResponse {
    DescribeResponse {
        tools: lowered.iter().map(to_bridged_tool_info).collect(),
    }
}

/// Wrap a [`DescribeResponse`] as a fresh [`SharedCatalog`].
pub fn shared_catalog(response: DescribeResponse) -> SharedCatalog {
    Arc::new(RwLock::new(Arc::new(response)))
}

/// Lower one bridged tool to its wire descriptor. Reads the standard fields off
/// the [`ToolDescriptor`](net_sdk::tool::ToolDescriptor) and the bridge fields
/// off the `tool::<id>::<field>` metadata the same lowering produced. A missing
/// classification defaults to empty, which the demand side reads as `unknown`
/// (the spicy default) — never a bypass.
fn to_bridged_tool_info(lt: &LoweredTool) -> BridgedToolInfo {
    let d = &lt.descriptor;
    let id = &d.tool_id;
    let meta = |key: String| lt.bridge_metadata.get(&key).cloned().unwrap_or_default();

    // The lowering stores schemas as JSON strings; parse them back to objects
    // so the demand side gets structured schemas without re-parsing strings.
    let input_schema = d
        .input_schema
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let output_schema = d
        .output_schema
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok());

    BridgedToolInfo {
        tool_id: id.clone(),
        name: d.name.clone(),
        description: d.description.clone(),
        input_schema,
        output_schema,
        version: d.version.clone(),
        compat_tier: meta(compat_tier_key(id)),
        credential_status: meta(credential_status_key(id)),
        substitutability: meta(substitutability_key(id)),
        visibility: meta(visibility_key(id)),
        invocation_scope: meta(invocation_scope_key(id)),
    }
}

/// Serves [`bridge::DESCRIBE_SERVICE`](crate::bridge::DESCRIBE_SERVICE): returns
/// the node's bridged-tool catalog (optionally filtered to one tool), gated by
/// the owner scope.
pub struct DescribeHandler {
    catalog: SharedCatalog,
    scope: OwnerScope,
}

impl DescribeHandler {
    /// Build a handler reading `catalog`, admitting only callers in `scope`.
    pub fn new(catalog: SharedCatalog, scope: OwnerScope) -> Self {
        Self { catalog, scope }
    }
}

#[async_trait::async_trait]
impl RpcHandler for DescribeHandler {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        // Owner-scope gate — describe is visibility; the same AEAD-verified
        // origin check the invoke handler applies.
        if !self.scope.allows(ctx.caller_origin) {
            return Err(RpcHandlerError::Application {
                code: ERR_OWNER_SCOPE,
                message: "caller root identity does not match owner scope".to_string(),
            });
        }

        // An empty body is "describe everything"; a body is a DescribeRequest.
        let req: DescribeRequest = if ctx.payload.body.is_empty() {
            DescribeRequest::default()
        } else {
            serde_json::from_slice(&ctx.payload.body).map_err(|e| RpcHandlerError::Application {
                code: ERR_BAD_REQUEST,
                message: format!("invalid describe request: {e}"),
            })?
        };

        let snapshot = self.catalog.read().await.clone();
        let body = render(&req, &snapshot)
            .map_err(|e| RpcHandlerError::Internal(format!("encode describe response: {e}")))?;

        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: Vec::new(),
            body: Bytes::from(body),
        })
    }
}

/// Render the describe response bytes for a request against a catalog snapshot.
/// Split out of the handler so the filter/encode logic is unit-testable
/// without constructing an `RpcContext` (the scope + end-to-end paths are
/// covered by the live two-node test).
fn render(req: &DescribeRequest, catalog: &DescribeResponse) -> Result<Vec<u8>, serde_json::Error> {
    match &req.tool_id {
        Some(id) => {
            let filtered = DescribeResponse {
                tools: catalog
                    .tools
                    .iter()
                    .filter(|t| &t.tool_id == id)
                    .cloned()
                    .collect(),
            };
            serde_json::to_vec(&filtered)
        }
        None => serde_json::to_vec(catalog),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::Tool;
    use crate::wrap::credentials::CredentialStatus;
    use crate::wrap::descriptor::{lower_tool, LoweringContext, Substitutability};
    use serde_json::json;

    fn lowered(name: &str, cred: CredentialStatus) -> LoweredTool {
        let tool = Tool {
            name: name.to_string(),
            title: None,
            description: Some(format!("does {name}")),
            input_schema: json!({ "type": "object", "properties": { "x": { "type": "string" } } }),
            output_schema: None,
        };
        lower_tool(
            &tool,
            &LoweringContext {
                server_version: "2.0.0".to_string(),
                credential_status: cred,
                substitutability: Substitutability::ProviderLocal,
            },
        )
    }

    #[test]
    fn builds_catalog_carrying_schema_and_status() {
        let catalog = build_catalog(&[
            lowered("echo", CredentialStatus::None),
            lowered("secret", CredentialStatus::Credentialed),
        ]);
        assert_eq!(catalog.tools.len(), 2);
        let echo = catalog.tools.iter().find(|t| t.tool_id == "echo").unwrap();
        assert_eq!(echo.credential_status, "none");
        assert_eq!(echo.compat_tier, "mcp_bridge");
        assert_eq!(echo.version, "2.0.0");
        assert_eq!(echo.input_schema["type"], "object");
        assert_eq!(echo.visibility, "owner_only");
        let secret = catalog
            .tools
            .iter()
            .find(|t| t.tool_id == "secret")
            .unwrap();
        assert_eq!(secret.credential_status, "credentialed");
    }

    #[test]
    fn render_returns_the_whole_catalog_by_default() {
        let catalog = build_catalog(&[
            lowered("echo", CredentialStatus::None),
            lowered("add", CredentialStatus::None),
        ]);
        let bytes = render(&DescribeRequest::default(), &catalog).unwrap();
        let resp: DescribeResponse = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(resp.tools.len(), 2);
    }

    #[test]
    fn render_filters_to_one_tool() {
        let catalog = build_catalog(&[
            lowered("echo", CredentialStatus::None),
            lowered("add", CredentialStatus::None),
        ]);
        let req = DescribeRequest {
            tool_id: Some("add".to_string()),
        };
        let bytes = render(&req, &catalog).unwrap();
        let resp: DescribeResponse = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(resp.tools.len(), 1);
        assert_eq!(resp.tools[0].tool_id, "add");
    }

    #[test]
    fn render_filter_miss_yields_empty() {
        let catalog = build_catalog(&[lowered("echo", CredentialStatus::None)]);
        let req = DescribeRequest {
            tool_id: Some("nope".to_string()),
        };
        let bytes = render(&req, &catalog).unwrap();
        let resp: DescribeResponse = serde_json::from_slice(&bytes).unwrap();
        assert!(resp.tools.is_empty());
    }
}
