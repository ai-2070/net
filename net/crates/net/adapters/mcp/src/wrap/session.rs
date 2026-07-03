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

use std::sync::Arc;

use net_sdk::capabilities::CapabilitySet;
use net_sdk::mesh::Mesh;
use net_sdk::mesh_rpc::ServeHandle;
use net_sdk::tool::description_metadata_key;

use super::credentials::{classify, ClassifyError, CredentialOverride, WrapEnv};
use super::descriptor::{lower_tool, LoweredTool, LoweringContext, Substitutability};
use super::invoke::{OwnerScope, WrapInvokeHandler};
use super::stdio::StdioMcpClient;
use super::McpError;
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
}

/// A running wrap session: the announced + served tools of one wrapped MCP
/// server. Drop to withdraw every service and stop the wrapped process.
pub struct WrapSession {
    /// The connected client. Held so the wrapped process outlives the
    /// session; dropping it kills the child.
    client: Arc<StdioMcpClient>,
    /// One serve handle per tool. Each reverses its `serve_rpc` on Drop.
    _handles: Vec<ServeHandle>,
    /// The tool ids served, for diagnostics.
    tools: Vec<String>,
}

impl WrapSession {
    /// The tool ids this session serves.
    pub fn tools(&self) -> &[String] {
        &self.tools
    }

    /// The wrapped client (e.g. to subscribe to `tools/list_changed`).
    pub fn client(&self) -> &Arc<StdioMcpClient> {
        &self.client
    }
}

/// Assemble the capability set announced for a wrapped server: one tag per
/// tool (so `cap search <tool>` finds it) plus the bridge metadata and
/// description under the `tool::<id>::<field>` convention.
///
/// One set for the whole server — capability announcements are
/// whole-node-set replacements, so every tool's tags and metadata must ride
/// together in a single announcement.
pub fn build_capability_set(lowered: &[LoweredTool]) -> CapabilitySet {
    let mut caps = CapabilitySet::new();
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
    let tools = client.list_tools().await?;

    // Lower every tool.
    let ctx = LoweringContext {
        server_version: init.server_info.version,
        credential_status,
        substitutability: config.substitutability,
    };
    let lowered: Vec<LoweredTool> = tools.iter().map(|t| lower_tool(t, &ctx)).collect();

    // Announce the whole set once.
    let caps = build_capability_set(&lowered);
    mesh.announce_capabilities(caps)
        .await
        .map_err(|e| WrapError::Announce(e.to_string()))?;

    // Serve one caller-aware handler per tool.
    let mut handles = Vec::with_capacity(lowered.len());
    let mut served = Vec::with_capacity(lowered.len());
    for lt in &lowered {
        let tool_id = lt.descriptor.tool_id.clone();
        let handler =
            WrapInvokeHandler::new(Arc::clone(&client), tool_id.clone(), config.scope.clone());
        let handle = mesh
            .serve_rpc(&tool_id, Arc::new(handler))
            .map_err(|e| WrapError::Serve {
                tool: tool_id.clone(),
                reason: e.to_string(),
            })?;
        handles.push(handle);
        served.push(tool_id);
    }

    Ok(WrapSession {
        client,
        _handles: handles,
        tools: served,
    })
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
