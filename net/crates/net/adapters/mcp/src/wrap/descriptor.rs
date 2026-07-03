//! Lower an MCP `tools/list` entry to the SDK's [`ToolDescriptor`] plus the
//! MCP-bridge metadata (`MCP_BRIDGE_PLAN.md` Phase 1, `wrap/descriptor.rs`).
//!
//! **Metadata carriage (plan option b).** The standard discovery fields land
//! on `net_sdk::tool::ToolDescriptor`. The bridge-specific fields —
//! `compat_tier`, `credential_status`, `substitutability`, `visibility`,
//! `invocation_scope` — do **not** get new core struct fields; they ride as
//! `CapabilitySet::metadata` keys under the existing `tool::<id>::<field>`
//! convention (the same hook `description` / `input_schema` already use), so
//! the core stays MCP-unaware (doctrine #1). The announce slice folds
//! [`LoweredTool::bridge_metadata`] into the announcement's metadata map.

use std::collections::BTreeMap;

use net_sdk::tool::ToolDescriptor;

use super::credentials::CredentialStatus;
use crate::spec::Tool;

/// `compat_tier` value for every bridged tool (doctrine #2): request/response
/// only, no streams / artifacts / migration.
pub const COMPAT_TIER_MCP_BRIDGE: &str = "mcp_bridge";
/// Default visibility (doctrine #3): owner-only until explicitly widened.
pub const VISIBILITY_OWNER_ONLY: &str = "owner_only";
/// Default invocation scope: only the wrapping root identity may invoke.
pub const INVOCATION_SCOPE_SAME_ROOT: &str = "same_root_identity";

/// Whether a bridged capability may be transparently swapped for another
/// provider's equivalent (Phase 4 failover routing). Default is *not*
/// substitutable — a filesystem-class tool stays provider-local forever.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Substitutability {
    /// NOT substitutable — bound to this provider. The default.
    #[default]
    ProviderLocal,
    /// Interchangeable with another provider's equivalent — only when the
    /// operator explicitly flags it (`net wrap --substitutable`).
    ProviderEquivalent,
}

impl Substitutability {
    /// The wire/tag form.
    pub fn as_str(self) -> &'static str {
        match self {
            Substitutability::ProviderLocal => "provider_local",
            Substitutability::ProviderEquivalent => "provider_equivalent",
        }
    }
}

// --- bridge metadata keys (the `tool::<id>::<field>` convention) ----------

/// Metadata key holding a tool's compat tier.
pub fn compat_tier_key(tool_id: &str) -> String {
    format!("tool::{tool_id}::compat_tier")
}
/// Metadata key holding a tool's credential status.
pub fn credential_status_key(tool_id: &str) -> String {
    format!("tool::{tool_id}::credential_status")
}
/// Metadata key holding a tool's substitutability.
pub fn substitutability_key(tool_id: &str) -> String {
    format!("tool::{tool_id}::substitutability")
}
/// Metadata key holding a tool's visibility.
pub fn visibility_key(tool_id: &str) -> String {
    format!("tool::{tool_id}::visibility")
}
/// Metadata key holding a tool's invocation scope.
pub fn invocation_scope_key(tool_id: &str) -> String {
    format!("tool::{tool_id}::invocation_scope")
}

/// The non-per-tool inputs the lowering needs: who the provider is and the
/// classification the operator/detector produced for this wrap.
#[derive(Debug, Clone)]
pub struct LoweringContext {
    /// The wrapped server's version (from `initialize` `serverInfo.version`).
    /// MCP tools carry no per-tool version, so the server's stands in.
    pub server_version: String,
    /// Credential status for every tool from this server (per-wrap, not
    /// per-tool in v0).
    pub credential_status: CredentialStatus,
    /// Substitutability for every tool from this server.
    pub substitutability: Substitutability,
}

/// The result of lowering one MCP tool: the SDK discovery descriptor plus
/// the bridge metadata to fold into the announcement's `CapabilitySet`.
#[derive(Debug, Clone)]
pub struct LoweredTool {
    /// The standard discovery shape (`net_sdk::tool::ToolDescriptor`).
    pub descriptor: ToolDescriptor,
    /// Bridge-specific metadata, keyed by `tool::<id>::<field>`.
    pub bridge_metadata: BTreeMap<String, String>,
}

/// Lower one MCP `tools/list` entry.
///
/// The tool's `name` becomes the nRPC `tool_id` (the string a caller passes
/// to invoke it); provider namespacing for duplicate grouping is a Phase 4
/// concern and does not enter the id here. Compat tier is always
/// `mcp_bridge`, visibility / scope are the owner-only defaults, and the
/// schemas ride verbatim as JSON strings.
pub fn lower_tool(tool: &Tool, ctx: &LoweringContext) -> LoweredTool {
    let tool_id = tool.name.clone();
    let version = if ctx.server_version.is_empty() {
        "0".to_string()
    } else {
        ctx.server_version.clone()
    };

    let descriptor = ToolDescriptor {
        tool_id: tool_id.clone(),
        // Prefer the human title; fall back to the machine name.
        name: tool.title.clone().unwrap_or_else(|| tool.name.clone()),
        version,
        description: tool.description.clone(),
        input_schema: Some(tool.input_schema.to_string()),
        output_schema: tool.output_schema.as_ref().map(|s| s.to_string()),
        requires: Vec::new(),
        estimated_time_ms: 0,
        // MCP tools carry no purity guarantee, and the compat tier is
        // strictly unary request/response.
        stateless: false,
        streaming: false,
        tags: Vec::new(),
        node_count: 0,
    };

    let mut bridge_metadata = BTreeMap::new();
    bridge_metadata.insert(
        compat_tier_key(&tool_id),
        COMPAT_TIER_MCP_BRIDGE.to_string(),
    );
    bridge_metadata.insert(
        credential_status_key(&tool_id),
        ctx.credential_status.as_str().to_string(),
    );
    bridge_metadata.insert(
        substitutability_key(&tool_id),
        ctx.substitutability.as_str().to_string(),
    );
    bridge_metadata.insert(visibility_key(&tool_id), VISIBILITY_OWNER_ONLY.to_string());
    bridge_metadata.insert(
        invocation_scope_key(&tool_id),
        INVOCATION_SCOPE_SAME_ROOT.to_string(),
    );

    LoweredTool {
        descriptor,
        bridge_metadata,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn echo_tool() -> Tool {
        Tool {
            name: "echo".to_string(),
            title: Some("Echo".to_string()),
            description: Some("Return the message.".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": { "message": { "type": "string" } }
            }),
            output_schema: None,
        }
    }

    fn ctx(status: CredentialStatus, sub: Substitutability) -> LoweringContext {
        LoweringContext {
            server_version: "1.2.3".to_string(),
            credential_status: status,
            substitutability: sub,
        }
    }

    #[test]
    fn lowers_standard_fields_onto_the_descriptor() {
        let lowered = lower_tool(
            &echo_tool(),
            &ctx(CredentialStatus::Unknown, Substitutability::ProviderLocal),
        );
        let d = &lowered.descriptor;
        assert_eq!(d.tool_id, "echo");
        assert_eq!(d.name, "Echo", "human title preferred over machine name");
        assert_eq!(d.version, "1.2.3");
        assert_eq!(d.description.as_deref(), Some("Return the message."));
        assert!(!d.streaming, "compat tier is request/response only");
        // The input schema round-trips as a JSON string.
        let schema: serde_json::Value =
            serde_json::from_str(d.input_schema.as_deref().unwrap()).unwrap();
        assert_eq!(schema["type"], "object");
    }

    #[test]
    fn carries_bridge_metadata_under_the_tool_id_convention() {
        let lowered = lower_tool(
            &echo_tool(),
            &ctx(
                CredentialStatus::Credentialed,
                Substitutability::ProviderLocal,
            ),
        );
        let m = &lowered.bridge_metadata;
        assert_eq!(m.get(&compat_tier_key("echo")).unwrap(), "mcp_bridge");
        assert_eq!(
            m.get(&credential_status_key("echo")).unwrap(),
            "credentialed"
        );
        assert_eq!(
            m.get(&substitutability_key("echo")).unwrap(),
            "provider_local"
        );
        assert_eq!(m.get(&visibility_key("echo")).unwrap(), "owner_only");
        assert_eq!(
            m.get(&invocation_scope_key("echo")).unwrap(),
            "same_root_identity"
        );
    }

    #[test]
    fn substitutability_default_is_provider_local() {
        assert_eq!(Substitutability::default(), Substitutability::ProviderLocal);
        let lowered = lower_tool(
            &echo_tool(),
            &ctx(
                CredentialStatus::Unknown,
                Substitutability::ProviderEquivalent,
            ),
        );
        assert_eq!(
            lowered
                .bridge_metadata
                .get(&substitutability_key("echo"))
                .unwrap(),
            "provider_equivalent",
            "explicit flag overrides the default",
        );
    }

    #[test]
    fn missing_title_falls_back_to_name_and_empty_version_to_zero() {
        let tool = Tool {
            name: "raw".to_string(),
            title: None,
            description: None,
            input_schema: json!({ "type": "object" }),
            output_schema: None,
        };
        let lowered = lower_tool(
            &tool,
            &LoweringContext {
                server_version: String::new(),
                credential_status: CredentialStatus::Unknown,
                substitutability: Substitutability::ProviderLocal,
            },
        );
        assert_eq!(lowered.descriptor.name, "raw");
        assert_eq!(lowered.descriptor.version, "0");
    }
}
