//! Wrap orchestration — tie the pieces together into a running supply-side
//! session (`MCP_BRIDGE_PLAN.md` Phase 1).
//!
//! [`wrap_server`] spawns a stdio MCP server, discovers its tools, lowers
//! them, announces them as a capability set, and serves one
//! [`WrapInvokeHandler`] per tool over nRPC. The returned [`WrapSession`]
//! owns the serve handles and the client, so dropping it withdraws the
//! services and kills the wrapped process.
//!
//! Everything mesh-facing goes through the `net-mesh-sdk` [`Mesh`] surface
//! (doctrine #1). Owner-only enforcement is the handler's [`OwnerScope`];
//! announcement-level allow-list scoping (so non-owners can't even discover
//! the capability) is a later hardening — for v0 the visibility / scope ride
//! as announcement *metadata*, honest even before the full permission system.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use net_sdk::capabilities::CapabilitySet;
use net_sdk::mesh::Mesh;
use net_sdk::mesh_rpc::ServeHandle;
use net_sdk::tool::description_metadata_key;

use super::catalog::{build_catalog, shared_catalog, DescribeHandler, SharedCatalog};
use super::credentials::{classify, ClassifyError, CredentialOverride, WrapEnv};
use super::descriptor::{
    lower_tool, LoweredTool, LoweringContext, Substitutability,
};
use super::invoke::{OwnerScope, WrapInvokeHandler};
use super::stdio::StdioMcpClient;
use super::McpError;
use crate::bridge::{BRIDGE_PROVIDER_TAG, DESCRIBE_SERVICE};
use crate::spec::Implementation;

/// A failure setting up a wrap session.
#[derive(Debug, thiserror::Error)]
pub enum WrapError {
    /// Talking to the wrapped MCP server failed (spawn / initialize / list).
    #[error("wrapped MCP server error: {0}")]
    Mcp(#[from] McpError),
    /// Credential classification rejected the operator's flags.
    #[error("credential classification: {0}")]
    Classify(#[from] ClassifyError),
    /// Announcing the capability set on the mesh failed.
    #[error("capability announce failed: {0}")]
    Announce(String),
    /// Serving a tool's nRPC handler failed.
    #[error("serving tool {tool:?} failed: {reason}")]
    Serve {
        /// The tool whose serve failed.
        tool: String,
        /// The underlying serve error, stringified (the SDK error type is
        /// not part of this crate's surface).
        reason: String,
    },
    /// A wrapped tool's stored schema could not be parsed while building the
    /// describe catalog.
    #[error("describe catalog build failed: {0}")]
    Catalog(#[from] super::catalog::CatalogError),
}

/// What a [`WrapSession::refresh`] changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RefreshDelta {
    /// Tool ids newly served + announced.
    pub added: Vec<String>,
    /// Tool ids withdrawn (the wrapped server dropped them).
    pub removed: Vec<String>,
}

impl RefreshDelta {
    /// True when nothing changed.
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }
}

/// A running wrap session: the announced + served tools of one wrapped MCP
/// server. Drop to withdraw every service and stop the wrapped process.
pub struct WrapSession {
    /// The connected client. Held so the wrapped process outlives the
    /// session; dropping it kills the child.
    client: Arc<StdioMcpClient>,
    /// `tool_id -> serve handle`. Dropping a handle reverses its `serve_rpc`.
    /// Keyed so [`refresh`](Self::refresh) can reconcile against the new set.
    handles: HashMap<String, ServeHandle>,
    /// Lowering context reused when re-lowering on refresh (the wrapped
    /// server's version + the per-wrap credential status / substitutability).
    ctx: LoweringContext,
    /// Owner scope reused when serving tools that appear on refresh.
    scope: OwnerScope,
    /// The tool ids served, sorted, for diagnostics.
    tools: Vec<String>,
    /// MCP tool names skipped because they have no usable id (an empty name).
    /// Non-charset-safe names are sanitized and bridged, not skipped. Surfaced
    /// so the operator sees what wasn't bridged instead of it failing silently.
    skipped: Vec<String>,
    /// Serve handle for the describe service (`bridge::DESCRIBE_SERVICE`).
    /// Held so dropping the session withdraws it with the tool handlers.
    _describe_handle: ServeHandle,
    /// The describe catalog the describe handler reads; swapped on refresh so
    /// demand-side describes stay current ("always up-to-date types").
    describe_catalog: SharedCatalog,
}

impl WrapSession {
    /// The tool ids this session serves.
    pub fn tools(&self) -> &[String] {
        &self.tools
    }

    /// MCP tool names that were skipped because they have no usable id (an
    /// empty name). A non-charset-safe name is sanitized and bridged instead.
    pub fn skipped_tools(&self) -> &[String] {
        &self.skipped
    }

    /// The wrapped client (e.g. to subscribe to `tools/list_changed`).
    pub fn client(&self) -> &Arc<StdioMcpClient> {
        &self.client
    }

    /// Re-read the wrapped server's tools and reconcile the mesh: announce the
    /// new set, serve tools that appeared, and withdraw tools that vanished.
    /// Call this on a `tools/list_changed` notification (subscribe via
    /// [`client`](Self::client)`().subscribe_list_changed()`) so bridged
    /// descriptors stay current — "always up-to-date types" holds for bridged
    /// tools too.
    pub async fn refresh(&mut self, mesh: &Mesh) -> Result<RefreshDelta, WrapError> {
        let (lowered, skipped) = discover_and_lower(&self.client, &self.ctx).await?;
        let desired: HashSet<String> = lowered
            .iter()
            .map(|lt| lt.descriptor.tool_id.clone())
            .collect();

        // Announce the new set first — tags propagate only from the announced
        // baseline (see `wrap_server`).
        let caps = build_capability_set(&lowered);
        mesh.announce_capabilities(caps)
            .await
            .map_err(|e| WrapError::Announce(e.to_string()))?;

        // Refresh the describe catalog so demand-side describes see the new
        // schemas/status. The describe service keeps serving; only its data
        // swaps ("always up-to-date types" for the describe path too).
        *self.describe_catalog.write().await = std::sync::Arc::new(build_catalog(&lowered)?);

        // Serve tools that appeared.
        let mut added = Vec::new();
        for lt in &lowered {
            let tool_id = lt.descriptor.tool_id.clone();
            if !self.handles.contains_key(&tool_id) {
                // Serve under the channel-safe `tool_id`, but invoke the wrapped
                // tool by its original `mcp_name` (they differ for a sanitized
                // name).
                let handler = WrapInvokeHandler::new(
                    Arc::clone(&self.client),
                    lt.mcp_name.clone(),
                    self.scope.clone(),
                );
                let handle =
                    mesh.serve_rpc(&tool_id, Arc::new(handler))
                        .map_err(|e| WrapError::Serve {
                            tool: tool_id.clone(),
                            reason: e.to_string(),
                        })?;
                self.handles.insert(tool_id.clone(), handle);
                added.push(tool_id);
            }
        }
        // Withdraw tools that vanished (dropping the handle reverses serve_rpc).
        let removed: Vec<String> = self
            .handles
            .keys()
            .filter(|id| !desired.contains(*id))
            .cloned()
            .collect();
        for tool_id in &removed {
            self.handles.remove(tool_id);
        }

        self.tools = self.handles.keys().cloned().collect();
        self.tools.sort();
        self.skipped = skipped;
        added.sort();
        Ok(RefreshDelta {
            added,
            removed: {
                let mut r = removed;
                r.sort();
                r
            },
        })
    }
}

/// Assemble the capability set announced for a wrapped server: one tag per
/// tool (so `cap search <tool>` finds it) plus the bridge metadata and
/// description under the `tool::<id>::<field>` convention, and — when there is
/// at least one tool — the [`BRIDGE_PROVIDER_TAG`] so a demand-side gateway can
/// find this node as a bridge provider and fetch its catalog via the describe
/// service.
///
/// One set for the whole server — capability announcements are
/// whole-node-set replacements, so every tool's tags and metadata must ride
/// together in a single announcement.
pub fn build_capability_set(lowered: &[LoweredTool]) -> CapabilitySet {
    let mut caps = CapabilitySet::new();
    if !lowered.is_empty() {
        caps = caps.add_tag(BRIDGE_PROVIDER_TAG.to_string());
    }
    for lt in lowered {
        let d = &lt.descriptor;
        caps = caps.add_tag(d.tool_id.clone());
        for (key, value) in &lt.bridge_metadata {
            caps = caps.with_metadata(key.clone(), value.clone());
        }
        if let Some(description) = &d.description {
            caps = caps.with_metadata(description_metadata_key(&d.tool_id), description.clone());
        }
    }
    caps
}

/// Configuration for a wrap: the credential-classification overrides and the
/// substitutability the operator declared, plus who may invoke.
#[derive(Debug, Clone)]
pub struct WrapConfig {
    /// How this server identifies itself to the wrapped MCP server.
    pub client_info: Implementation,
    /// Who may invoke the wrapped tools (owner-only by default).
    pub scope: OwnerScope,
    /// Credential-status override (`--credentialed` / `--no-credentials`).
    pub credential_override: CredentialOverride,
    /// Confirms a downward credential override (`--force`).
    pub force: bool,
    /// Whether the tools are declared interchangeable across providers.
    pub substitutability: Substitutability,
}

impl WrapConfig {
    /// A default config: owner-only to `owner_origin`, detect credentials,
    /// provider-local.
    pub fn owner_only(client_info: Implementation, owner_origin: u64) -> Self {
        Self {
            client_info,
            scope: OwnerScope::owner_only(owner_origin),
            credential_override: CredentialOverride::Detect,
            force: false,
            substitutability: Substitutability::ProviderLocal,
        }
    }
}

/// Wrap a stdio MCP server as mesh capabilities: spawn, discover, lower,
/// announce, and serve every tool. Returns the live [`WrapSession`].
pub async fn wrap_server(
    mesh: &Mesh,
    program: &str,
    args: &[String],
    envs: &[(String, String)],
    config: WrapConfig,
) -> Result<WrapSession, WrapError> {
    // Classify BEFORE spawning: an invalid override (e.g. downward without
    // --force) should fail fast without starting a process.
    let credential_status = classify(
        &WrapEnv {
            program,
            args,
            envs,
        },
        config.credential_override,
        config.force,
    )?;

    // Connect and discover.
    let client =
        Arc::new(StdioMcpClient::spawn(program, args, envs, config.client_info.clone()).await?);
    let init = client.initialize().await?;

    let ctx = LoweringContext {
        server_version: init.server_info.version,
        credential_status,
        substitutability: config.substitutability,
    };
    let (lowered, skipped) = discover_and_lower(&client, &ctx).await?;

    // Announce the tool set, then serve a handler for each. Announce must come
    // first: the substrate re-broadcasts the merged capability set as each
    // service registers, so the tool tags only propagate to peers when they are
    // already in the announced (`user_caps`) baseline — serving first would
    // broadcast bare `nrpc:` tags and the later announce would not overtake them
    // at peers. The window between announce and the (local, near-instant) serves
    // is closed by the rollback below.
    let caps = build_capability_set(&lowered);
    mesh.announce_capabilities(caps)
        .await
        .map_err(|e| WrapError::Announce(e.to_string()))?;

    // Serve the describe service so demand-side gateways can read the bridged
    // tools' full descriptors (schema + credential status). Owner-scoped like
    // invoke — describe is visibility. If it fails, roll back the announcement.
    let describe_catalog = shared_catalog(build_catalog(&lowered)?);
    let describe_handle = match mesh.serve_rpc(
        DESCRIBE_SERVICE,
        Arc::new(DescribeHandler::new(
            describe_catalog.clone(),
            config.scope.clone(),
        )),
    ) {
        Ok(handle) => handle,
        Err(e) => {
            let _ = mesh.announce_capabilities(CapabilitySet::new()).await;
            return Err(WrapError::Serve {
                tool: DESCRIBE_SERVICE.to_string(),
                reason: e.to_string(),
            });
        }
    };

    // Serve one caller-aware handler per tool. If any serve fails, roll back so
    // a failed startup never leaves stale, uninvocable capabilities
    // discoverable: drop the handles served so far (each reverses its
    // `serve_rpc`), drop the describe handle, and withdraw the announcement
    // before returning the error.
    let mut handles: HashMap<String, ServeHandle> = HashMap::with_capacity(lowered.len());
    for lt in &lowered {
        let tool_id = lt.descriptor.tool_id.clone();
        // Serve under the channel-safe `tool_id`, invoke by the original
        // `mcp_name` (they differ only for a sanitized name).
        let handler =
            WrapInvokeHandler::new(Arc::clone(&client), lt.mcp_name.clone(), config.scope.clone());
        match mesh.serve_rpc(&tool_id, Arc::new(handler)) {
            Ok(handle) => {
                handles.insert(tool_id, handle);
            }
            Err(e) => {
                drop(handles);
                drop(describe_handle);
                // Best-effort withdraw so discovery matches runtime state.
                let _ = mesh.announce_capabilities(CapabilitySet::new()).await;
                return Err(WrapError::Serve {
                    tool: tool_id,
                    reason: e.to_string(),
                });
            }
        }
    }

    let mut tools: Vec<String> = handles.keys().cloned().collect();
    tools.sort();
    Ok(WrapSession {
        client,
        handles,
        ctx,
        scope: config.scope,
        tools,
        skipped,
        _describe_handle: describe_handle,
        describe_catalog,
    })
}

/// Discover the wrapped server's tools and lower them, returning
/// `(lowered, skipped_names)`. Shared by [`wrap_server`] and
/// [`WrapSession::refresh`].
///
/// A name that isn't already a valid nRPC service id is *sanitized* into one by
/// [`lower_tool`] (via `channel_safe_tool_id`) rather than dropped, so a tool
/// with an uppercase / spaced / punctuated name is still bridged. Only a tool
/// with an empty name — which has no usable id — is skipped.
async fn discover_and_lower(
    client: &StdioMcpClient,
    ctx: &LoweringContext,
) -> Result<(Vec<LoweredTool>, Vec<String>), WrapError> {
    let tools = client.list_tools().await?;
    let (usable, empty_named): (Vec<_>, Vec<_>) =
        tools.into_iter().partition(|t| !t.name.trim().is_empty());
    let skipped: Vec<String> = empty_named.into_iter().map(|t| t.name).collect();
    let lowered: Vec<LoweredTool> = usable.iter().map(|t| lower_tool(t, ctx)).collect();
    Ok((lowered, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::Tool;
    use net_sdk::capabilities::CapabilityFilter;
    use serde_json::json;

    fn tool(name: &str, description: &str) -> Tool {
        Tool {
            name: name.to_string(),
            title: None,
            description: Some(description.to_string()),
            input_schema: json!({ "type": "object" }),
            output_schema: None,
        }
    }

    fn lowered(names: &[(&str, &str)]) -> Vec<LoweredTool> {
        let ctx = LoweringContext {
            server_version: "1.0.0".to_string(),
            credential_status: super::super::CredentialStatus::Unknown,
            substitutability: Substitutability::ProviderLocal,
        };
        names
            .iter()
            .map(|(n, d)| lower_tool(&tool(n, d), &ctx))
            .collect()
    }

    #[test]
    fn capability_set_carries_a_tag_and_metadata_per_tool() {
        let caps = build_capability_set(&lowered(&[("echo", "echo it"), ("add", "sum")]));

        // Each tool's name is a discoverable tag.
        assert!(CapabilityFilter::new().require_tag("echo").matches(&caps));
        assert!(CapabilityFilter::new().require_tag("add").matches(&caps));

        // Bridge metadata + description ride under tool::<id>::<field>.
        assert_eq!(
            caps.metadata
                .get(&super::super::descriptor::compat_tier_key("echo"))
                .map(String::as_str),
            Some("mcp_bridge"),
        );
        assert_eq!(
            caps.metadata
                .get(&super::super::descriptor::credential_status_key("add"))
                .map(String::as_str),
            Some("unknown"),
        );
        assert_eq!(
            caps.metadata
                .get(&description_metadata_key("echo"))
                .map(String::as_str),
            Some("echo it"),
        );
    }

    #[test]
    fn empty_tool_list_yields_an_empty_capability_set() {
        let caps = build_capability_set(&[]);
        assert!(!CapabilityFilter::new().require_tag("echo").matches(&caps));
        // No tools ⇒ not advertised as a bridge provider either.
        assert!(!CapabilityFilter::new()
            .require_tag(BRIDGE_PROVIDER_TAG)
            .matches(&caps));
    }

    #[test]
    fn a_non_empty_set_advertises_the_bridge_provider_tag() {
        // Demand-side gateways find providers via this tag, then fetch each
        // one's catalog through the describe service.
        let caps = build_capability_set(&lowered(&[("echo", "echo it")]));
        assert!(CapabilityFilter::new()
            .require_tag(BRIDGE_PROVIDER_TAG)
            .matches(&caps));
    }

    #[test]
    fn announced_metadata_carries_classification_not_secrets() {
        // Structural token-leak guard (doctrine #100). A wrapped server's
        // credentials go to the child process env (StdioMcpClient::spawn),
        // never into lowering/announce — so the announcement can only ever
        // carry the credential *classification* label, never a token.
        let ctx = LoweringContext {
            server_version: "1.0.0".to_string(),
            credential_status: super::super::CredentialStatus::Credentialed,
            substitutability: Substitutability::ProviderLocal,
        };
        let caps = build_capability_set(&[lower_tool(&tool("secretive", "uses a token"), &ctx)]);
        // The credential_status value is a fixed classification label.
        assert_eq!(
            caps.metadata
                .get(&super::super::descriptor::credential_status_key(
                    "secretive"
                ))
                .map(String::as_str),
            Some("credentialed"),
        );
        // No announced metadata value is anything but the small set of
        // classification labels + the human description — never a secret.
        for value in caps.metadata.values() {
            assert!(
                !value.contains("token") || value == "uses a token",
                "announced metadata must not carry secret-shaped values: {value:?}",
            );
        }
    }
}
