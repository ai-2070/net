//! Provider-side describe service (`MCP_BRIDGE_PLAN.md` Phase 2, option B).
//!
//! A wrap node serves one [`DescribeHandler`] on
//! [`bridge::DESCRIBE_SERVICE`](crate::bridge::DESCRIBE_SERVICE)
//! so a demand-side gateway can read the full descriptors of its bridged tools
//! — input schema, description, and credential status — which the announced
//! capability metadata carries but the public SDK does not surface to a
//! discovering node. This is additive: it does not touch the announce / serve
//! / owner-scope path the invoke side already ships.
//!
//! The service is gated by the **same [`OwnerScope`]** as invoke: describe is
//! visibility, and wrapped tools are visible only to the owner scope by default
//! (doctrine #3). A caller admitted to invoke can describe; one that is not is
//! denied on both, with the same structured rejection. The describe service is
//! one nRPC service per node while a node can carry several publications
//! (`MCP_BRIDGE_SDK_PLAN.md` P0), so the catalog is a list of
//! [`CatalogPart`]s — each publication's tools under its own scope — and the
//! handler filters per caller: a caller admitted by publication A but not B
//! describes exactly A's tools.
//!
//! Only classification *labels* cross this path, never secrets — the same
//! token-leak invariant the announce path holds (`super::session`).

use std::sync::Arc;

use bytes::Bytes;
use net_sdk::mesh_rpc::{RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus};
use tokio::sync::RwLock;

use super::descriptor::{
    compat_tier_key, credential_status_key, invocation_scope_key, schema_hash,
    substitutability_key, visibility_key, LoweredTool,
};
use super::invoke::{OwnerScope, ERR_BAD_REQUEST, ERR_OWNER_SCOPE, OWNER_SCOPE_REJECTION};
use crate::bridge::{BridgedToolInfo, DescribeRequest, DescribeResponse};

/// One publication's slice of the merged describe catalog: its bridged tools
/// plus the [`OwnerScope`] gating their visibility.
pub struct CatalogPart {
    /// Who may describe (and invoke) this publication's tools.
    pub scope: OwnerScope,
    /// The publication's bridged-tool descriptors.
    pub catalog: DescribeResponse,
}

/// The shared, swappable catalog a [`DescribeHandler`] reads. `refresh` swaps
/// the inner `Arc` so a describe in flight keeps its consistent snapshot and
/// readers clone only a pointer.
pub type SharedCatalog = Arc<RwLock<Arc<Vec<CatalogPart>>>>;

/// A tool's stored JSON-schema string failed to parse while building the
/// describe catalog. Surfaced rather than silently downgraded to a permissive
/// `{}` schema (which would let malformed input pass the demand side's
/// pre-flight validation). In practice this never fires — the schema string is
/// produced by serializing a `serde_json::Value` in the lowering — so a failure
/// signals real corruption worth reporting, not masking.
#[derive(Debug, thiserror::Error)]
#[error("bridged tool {tool:?} has a malformed {field} schema: {reason}")]
pub struct CatalogError {
    /// The tool whose schema failed to parse.
    pub tool: String,
    /// Which schema (`input` / `output`).
    pub field: &'static str,
    /// The underlying parse error.
    pub reason: String,
}

/// Build the describe catalog from the lowered tools. Fails if any tool's
/// stored schema string does not parse (see [`CatalogError`]).
pub fn build_catalog(lowered: &[LoweredTool]) -> Result<DescribeResponse, CatalogError> {
    let tools = lowered
        .iter()
        .map(to_bridged_tool_info)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(DescribeResponse { tools })
}

/// Wrap the scoped catalog parts as a fresh [`SharedCatalog`].
pub fn shared_catalog(parts: Vec<CatalogPart>) -> SharedCatalog {
    Arc::new(RwLock::new(Arc::new(parts)))
}

/// Lower one bridged tool to its wire descriptor. Reads the standard fields off
/// the [`ToolDescriptor`](net_sdk::tool::ToolDescriptor) and the bridge fields
/// off the `tool::<id>::<field>` metadata the same lowering produced. A missing
/// classification defaults to empty, which the demand side reads as `unknown`
/// (the spicy default) — never a bypass.
fn to_bridged_tool_info(lt: &LoweredTool) -> Result<BridgedToolInfo, CatalogError> {
    let d = &lt.descriptor;
    let id = &d.tool_id;
    let meta = |key: String| lt.bridge_metadata.get(&key).cloned().unwrap_or_default();

    // The lowering stores schemas as JSON strings; parse them back to objects
    // so the demand side gets structured schemas. A parse failure is
    // propagated, never downgraded to a permissive `{}` — a malformed input
    // schema must not silently become "accepts anything" on the demand side.
    // A missing input schema is a no-argument tool and lowers to `{}`.
    let input_schema = match d.input_schema.as_deref() {
        Some(s) => serde_json::from_str(s).map_err(|e| CatalogError {
            tool: id.clone(),
            field: "input",
            reason: e.to_string(),
        })?,
        None => serde_json::json!({}),
    };
    let output_schema = match d.output_schema.as_deref() {
        Some(s) => Some(serde_json::from_str(s).map_err(|e| CatalogError {
            tool: id.clone(),
            field: "output",
            reason: e.to_string(),
        })?),
        None => None,
    };
    // Computed from the parsed input schema (not read from bridge_metadata,
    // which no longer carries it — `lower_tool` stays golden-vector-stable).
    // Before the struct literal moves `input_schema`.
    let schema_hash_hex = schema_hash(&input_schema);

    Ok(BridgedToolInfo {
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
        schema_hash: schema_hash_hex,
        pricing_terms: d.pricing_terms.clone(),
    })
}

/// Serves [`bridge::DESCRIBE_SERVICE`](crate::bridge::DESCRIBE_SERVICE): returns
/// the node's bridged-tool catalog (optionally filtered to one tool), gated
/// per [`CatalogPart`] by each publication's own owner scope.
pub struct DescribeHandler {
    catalog: SharedCatalog,
}

impl DescribeHandler {
    /// Build a handler reading `catalog`. Each part carries the scope that
    /// admits its callers, so the handler needs no scope of its own.
    pub fn new(catalog: SharedCatalog) -> Self {
        Self { catalog }
    }
}

#[async_trait::async_trait]
impl RpcHandler for DescribeHandler {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        let snapshot = self.catalog.read().await.clone();

        // Owner-scope gate — describe is visibility; the same AEAD-verified
        // origin check the invoke handler applies, per publication: the
        // caller sees exactly the parts whose scope admits it. A caller no
        // part admits gets the same structured rejection as invoke (this also
        // covers the transient zero-publication catalog).
        if !snapshot
            .iter()
            .any(|part| part.scope.allows(ctx.caller_origin))
        {
            return Err(RpcHandlerError::Application {
                code: ERR_OWNER_SCOPE,
                message: OWNER_SCOPE_REJECTION.to_string(),
            });
        }
        let visible: Vec<&BridgedToolInfo> = snapshot
            .iter()
            .filter(|part| part.scope.allows(ctx.caller_origin))
            .flat_map(|part| part.catalog.tools.iter())
            .collect();

        // An empty body is "describe everything"; a body is a DescribeRequest.
        let req: DescribeRequest = if ctx.payload.body.is_empty() {
            DescribeRequest::default()
        } else {
            serde_json::from_slice(&ctx.payload.body).map_err(|e| RpcHandlerError::Application {
                code: ERR_BAD_REQUEST,
                message: format!("invalid describe request: {e}"),
            })?
        };

        let body = render(&req, &visible)
            .map_err(|e| RpcHandlerError::Internal(format!("encode describe response: {e}")))?;

        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: Vec::new(),
            body: Bytes::from(body),
        })
    }
}

/// Render the describe response bytes for a request against the caller's
/// visible tools. Split out of the handler so the filter/encode logic is
/// unit-testable without constructing an `RpcContext` (the scope + end-to-end
/// paths are covered by the live two-node test).
fn render(req: &DescribeRequest, tools: &[&BridgedToolInfo]) -> Result<Vec<u8>, serde_json::Error> {
    let tools: Vec<BridgedToolInfo> = tools
        .iter()
        .filter(|t| match &req.tool_id {
            Some(id) => &t.tool_id == id,
            None => true,
        })
        .map(|t| (*t).clone())
        .collect();
    serde_json::to_vec(&DescribeResponse { tools })
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
                pricing: std::collections::BTreeMap::new(),
            },
        )
    }

    #[test]
    fn builds_catalog_carrying_schema_and_status() {
        let catalog = build_catalog(&[
            lowered("echo", CredentialStatus::None),
            lowered("secret", CredentialStatus::Credentialed),
        ])
        .unwrap();
        assert_eq!(catalog.tools.len(), 2);
        let echo = catalog.tools.iter().find(|t| t.tool_id == "echo").unwrap();
        assert_eq!(echo.credential_status, "none");
        assert_eq!(echo.compat_tier, "mcp_bridge");
        assert_eq!(echo.version, "2.0.0");
        assert_eq!(echo.input_schema["type"], "object");
        assert_eq!(echo.visibility, "owner_only");
        // The content hash rides in the catalog so a consumer can cache by it.
        assert_eq!(
            echo.schema_hash,
            crate::wrap::descriptor::schema_hash(&echo.input_schema),
        );
        assert!(!echo.schema_hash.is_empty());
        let secret = catalog
            .tools
            .iter()
            .find(|t| t.tool_id == "secret")
            .unwrap();
        assert_eq!(secret.credential_status, "credentialed");
    }

    /// Flatten a catalog into the `render` input shape (the handler's
    /// per-caller visible-tools view).
    fn visible(catalog: &DescribeResponse) -> Vec<&BridgedToolInfo> {
        catalog.tools.iter().collect()
    }

    #[test]
    fn render_returns_the_whole_catalog_by_default() {
        let catalog = build_catalog(&[
            lowered("echo", CredentialStatus::None),
            lowered("add", CredentialStatus::None),
        ])
        .unwrap();
        let bytes = render(&DescribeRequest::default(), &visible(&catalog)).unwrap();
        let resp: DescribeResponse = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(resp.tools.len(), 2);
    }

    #[test]
    fn render_filters_to_one_tool() {
        let catalog = build_catalog(&[
            lowered("echo", CredentialStatus::None),
            lowered("add", CredentialStatus::None),
        ])
        .unwrap();
        let req = DescribeRequest {
            tool_id: Some("add".to_string()),
        };
        let bytes = render(&req, &visible(&catalog)).unwrap();
        let resp: DescribeResponse = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(resp.tools.len(), 1);
        assert_eq!(resp.tools[0].tool_id, "add");
    }

    #[test]
    fn render_filter_miss_yields_empty() {
        let catalog = build_catalog(&[lowered("echo", CredentialStatus::None)]).unwrap();
        let req = DescribeRequest {
            tool_id: Some("nope".to_string()),
        };
        let bytes = render(&req, &visible(&catalog)).unwrap();
        let resp: DescribeResponse = serde_json::from_slice(&bytes).unwrap();
        assert!(resp.tools.is_empty());
    }

    #[test]
    fn render_only_sees_the_parts_that_admit_the_caller() {
        // The handler filters parts by scope BEFORE render — model that here:
        // a caller admitted by one publication describes exactly its tools,
        // never a co-published server's.
        let mine = build_catalog(&[lowered("echo", CredentialStatus::None)]).unwrap();
        let theirs = build_catalog(&[lowered("add", CredentialStatus::None)]).unwrap();
        let parts = vec![
            CatalogPart {
                scope: OwnerScope::owner_only(7),
                catalog: mine,
            },
            CatalogPart {
                scope: OwnerScope::owner_only(8),
                catalog: theirs,
            },
        ];
        let caller: u64 = 7;
        let visible: Vec<&BridgedToolInfo> = parts
            .iter()
            .filter(|p| p.scope.allows(caller))
            .flat_map(|p| p.catalog.tools.iter())
            .collect();
        let bytes = render(&DescribeRequest::default(), &visible).unwrap();
        let resp: DescribeResponse = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(resp.tools.len(), 1);
        assert_eq!(resp.tools[0].tool_id, "echo");
    }

    #[test]
    fn a_malformed_schema_string_fails_the_build_instead_of_masking() {
        // A corrupt stored schema must surface as a build error, not silently
        // become a permissive `{}` that would weaken demand-side validation.
        let mut lt = lowered("echo", CredentialStatus::None);
        lt.descriptor.input_schema = Some("{ not valid json".to_string());
        let err = build_catalog(&[lt]).unwrap_err();
        assert_eq!(err.tool, "echo");
        assert_eq!(err.field, "input");
    }
}
