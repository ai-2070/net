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

/// Max length of a tool id, leaving headroom under the substrate's 255-char
/// channel-name cap for the `.replies.<origin>` suffix the RPC layer appends.
const MAX_TOOL_ID_LEN: usize = 200;

/// Whether `name` can be used verbatim as an nRPC service id.
///
/// A served tool's id becomes the channel names `<id>.requests` /
/// `<id>.replies.*`, which the substrate validates as **lowercase** names over
/// `[a-z0-9._/-]` with no `//` and no `.`/`..` path segments. A name that fails
/// this can't be a channel id as-is; [`channel_safe_tool_id`] sanitizes it so
/// the tool is still bridged rather than dropped.
pub fn is_serviceable_tool_id(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_TOOL_ID_LEN
        && !name.starts_with('/')
        && !name.ends_with('/')
        && !name.contains("//")
        && name
            .chars()
            .all(|c| matches!(c, 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '/'))
        && name.split('/').all(|seg| seg != "." && seg != "..")
}

/// A channel-safe nRPC service id for MCP tool `name`.
///
/// A name that is already [`is_serviceable_tool_id`] is used verbatim, so the
/// common case has no wire change. Otherwise the name is lowercased with every
/// out-of-charset character mapped to `_`, truncated to fit, and suffixed with
/// a short hash of the **original** name. The hash keeps distinct names
/// collision-free and stops a sanitized id from shadowing a serviceable one —
/// so tools with uppercase / spaced / punctuated names (e.g. `createIssue`) are
/// BRIDGED under a stable safe id rather than silently dropped. The wrap side
/// keeps the original name ([`LoweredTool::mcp_name`]) to invoke the tool. The
/// result always satisfies [`is_serviceable_tool_id`].
///
/// `name` must be non-empty — an empty tool name has no usable id and is
/// skipped by the caller before lowering.
pub fn channel_safe_tool_id(name: &str) -> String {
    if is_serviceable_tool_id(name) {
        return name.to_string();
    }
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    name.hash(&mut hasher);
    let suffix = format!("{:06x}", hasher.finish() & 0x00ff_ffff);
    // All-ASCII output (each source char maps to one ASCII byte), so byte
    // truncation below can never split a char.
    let sanitized: String = name
        .chars()
        .map(|c| {
            let lc = c.to_ascii_lowercase();
            if matches!(lc, 'a'..='z' | '0'..='9' | '_' | '-') {
                lc
            } else {
                '_'
            }
        })
        .collect();
    let max_base = MAX_TOOL_ID_LEN.saturating_sub(suffix.len() + 1);
    let mut base = sanitized;
    base.truncate(max_base);
    format!("{base}_{suffix}")
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
    /// The wrapped server's ORIGINAL tool name — the string `tools/call` must
    /// use to invoke it. Equals `descriptor.tool_id` for a channel-safe name;
    /// differs when the name was sanitized into a mesh-safe id.
    pub mcp_name: String,
}

/// Lower one MCP `tools/list` entry.
///
/// The nRPC `tool_id` is the tool's name made channel-safe by
/// [`channel_safe_tool_id`] (verbatim when already safe); the original name is
/// kept in [`LoweredTool::mcp_name`] for invocation. Provider namespacing for
/// duplicate grouping is a Phase 4 concern and does not enter the id here.
/// Compat tier is always `mcp_bridge`, visibility / scope are the owner-only
/// defaults, and the schemas ride verbatim as JSON strings.
pub fn lower_tool(tool: &Tool, ctx: &LoweringContext) -> LoweredTool {
    let tool_id = channel_safe_tool_id(&tool.name);
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
        mcp_name: tool.name.clone(),
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

    #[test]
    fn serviceable_tool_id_accepts_only_channel_safe_names() {
        for ok in ["echo", "get_current_time", "a.b", "svc/sub", "x-1"] {
            assert!(is_serviceable_tool_id(ok), "{ok:?} should be serviceable");
        }
        for bad in [
            "",            // empty
            "createIssue", // uppercase
            "a b",         // space
            "/x",          // leading slash
            "x/",          // trailing slash
            "a//b",        // double slash
            "a/../b",      // traversal segment
            "a/./b",       // dot segment
            "emoji\u{1f600}",
        ] {
            assert!(!is_serviceable_tool_id(bad), "{bad:?} should be rejected");
        }
        assert!(
            !is_serviceable_tool_id(&"a".repeat(MAX_TOOL_ID_LEN + 1)),
            "over-long ids are rejected",
        );
    }

    #[test]
    fn channel_safe_id_passes_serviceable_names_verbatim() {
        for ok in ["echo", "get_current_time", "a.b", "svc/sub", "x-1"] {
            assert_eq!(channel_safe_tool_id(ok), ok, "{ok:?} is already safe");
        }
    }

    #[test]
    fn channel_safe_id_sanitizes_and_bridges_non_safe_names() {
        // F10: a non-channel-safe name (uppercase, spaces, punctuation,
        // non-ASCII) maps to a VALID, stable service id rather than being
        // dropped.
        for bad in ["createIssue", "get Status", "n@me!", "Ünïcode", "a b/c"] {
            let id = channel_safe_tool_id(bad);
            assert!(
                is_serviceable_tool_id(&id),
                "{bad:?} -> {id:?} must be serviceable",
            );
        }
        // Deterministic, and distinct inputs get distinct ids.
        assert_eq!(
            channel_safe_tool_id("createIssue"),
            channel_safe_tool_id("createIssue"),
        );
        // A sanitized id never shadows the serviceable name it lowercases to.
        assert_ne!(
            channel_safe_tool_id("createIssue"),
            channel_safe_tool_id("createissue"),
        );
        assert_eq!(channel_safe_tool_id("createissue"), "createissue");
        assert!(channel_safe_tool_id("createIssue").starts_with("createissue_"));
    }

    #[test]
    fn lower_tool_keeps_the_original_name_for_a_sanitized_id() {
        // F10: the descriptor advertises the safe id, but mcp_name is the
        // original so the invoke handler calls the right wrapped tool, and the
        // bridge metadata is keyed by the safe id the demand side describes.
        let tool = Tool {
            name: "createIssue".to_string(),
            title: None,
            description: None,
            input_schema: json!({ "type": "object" }),
            output_schema: None,
        };
        let lowered = lower_tool(
            &tool,
            &ctx(CredentialStatus::Unknown, Substitutability::ProviderLocal),
        );
        assert_eq!(
            lowered.mcp_name, "createIssue",
            "original preserved for invoke"
        );
        let id = &lowered.descriptor.tool_id;
        assert_ne!(id, "createIssue", "the id was sanitized");
        assert!(is_serviceable_tool_id(id));
        assert!(id.starts_with("createissue_"));
        assert!(lowered.bridge_metadata.contains_key(&compat_tier_key(id)));
    }
}
