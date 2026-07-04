//! The bridge's own inter-node protocol — how a demand-side shim asks a
//! supply-side wrap node to describe its bridged tools.
//!
//! **Why this exists.** The public SDK lets a joined node *discover* which
//! peers advertise a capability tag and *invoke* a service on a peer, but it
//! does not hand a discovering node a peer's `CapabilitySet` metadata — so the
//! per-tool input schema, description, and credential status the wrap side
//! announces as `tool::<id>::<field>` metadata are not readable off the fold.
//! The demand side needs all three (schema for pre-flight validation,
//! credential status for the consent gate, description for search). So the
//! wrap node serves this small **describe** nRPC service; the demand-side
//! gateway calls it to read the full descriptors. Purely additive: it does not
//! touch the announce/serve/owner-scope path the invoke side already ships.
//!
//! The response carries classification *labels* only — never secrets. A
//! wrapped server's credentials live in its child process on the owning
//! machine; the token-leak invariant holds here as it does on the announce
//! path (`wrap::session`).
//!
//! These types are the shared contract: `wrap` (provider) fills them,
//! `serve::mesh_gateway` (consumer) reads them.

use serde::{Deserialize, Serialize};

/// The nRPC service name a wrap node serves so demand-side shims can read the
/// full descriptors of its bridged tools. Channel-safe (lowercase,
/// `[a-z0-9._/-]`, no traversal) so it is a valid nRPC service id.
pub const DESCRIBE_SERVICE: &str = "mcp.bridge.describe";

/// Capability tag every wrap node advertises in its announce baseline so a
/// demand-side gateway can find bridge providers —
/// `find_nodes(require_tag(BRIDGE_PROVIDER_TAG))` — and then fetch each one's
/// catalog directly via [`DESCRIBE_SERVICE`]. Riding in the announced baseline
/// (not just the served-service `nrpc:` tag) makes discovery independent of
/// service-registration tag-propagation timing. Advertising that a node is a
/// bridge provider is not a secret; describe/invoke remain owner-scoped.
pub const BRIDGE_PROVIDER_TAG: &str = "mcp-bridge";

/// A describe request. Empty in v0 — the whole catalog is returned. Modelled
/// as a struct (not unit) so the wire shape can grow (e.g. a `tool_id` filter)
/// without a breaking change; `#[serde(default)]` on the receiver tolerates an
/// empty body.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DescribeRequest {
    /// If set, return only this tool's info; otherwise the whole catalog.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_id: Option<String>,
}

/// One bridged tool's full public descriptor: everything the demand side needs
/// to search, describe, validate arguments against, and consent-gate it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BridgedToolInfo {
    /// The tool id (the nRPC service name a caller invokes).
    pub tool_id: String,
    /// Human-facing name (the descriptor's `name`; falls back to the id).
    pub name: String,
    /// Description, if the wrapped server gave one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The tool's input JSON Schema (an object; `{}` if none was advertised).
    pub input_schema: serde_json::Value,
    /// The tool's output JSON Schema, if advertised.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<serde_json::Value>,
    /// The wrapped server's version (MCP tools carry no per-tool version).
    pub version: String,
    /// Compat tier — always `mcp_bridge` for wrapped tools.
    pub compat_tier: String,
    /// Credential status wire form (`credentialed` / `external_api` /
    /// `unknown` / `none`). A classification label, never a secret.
    pub credential_status: String,
    /// Substitutability (`provider_local` / `provider_equivalent`).
    pub substitutability: String,
    /// Visibility (`owner_only` by default).
    pub visibility: String,
    /// Invocation scope (`same_root_identity` by default).
    pub invocation_scope: String,
}

/// The describe service response: the wrap node's current bridged tools.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DescribeResponse {
    /// Every bridged tool this node currently serves.
    pub tools: Vec<BridgedToolInfo>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> BridgedToolInfo {
        BridgedToolInfo {
            tool_id: "echo".to_string(),
            name: "Echo".to_string(),
            description: Some("echo it back".to_string()),
            input_schema: json!({ "type": "object" }),
            output_schema: None,
            version: "1.0.0".to_string(),
            compat_tier: "mcp_bridge".to_string(),
            credential_status: "none".to_string(),
            substitutability: "provider_local".to_string(),
            visibility: "owner_only".to_string(),
            invocation_scope: "same_root_identity".to_string(),
        }
    }

    #[test]
    fn describe_request_defaults_and_round_trips() {
        // An empty body deserializes to the default request.
        let empty: DescribeRequest = serde_json::from_slice(b"{}").unwrap();
        assert_eq!(empty, DescribeRequest::default());
        // A set filter round-trips.
        let filtered = DescribeRequest {
            tool_id: Some("echo".to_string()),
        };
        let bytes = serde_json::to_vec(&filtered).unwrap();
        assert_eq!(
            serde_json::from_slice::<DescribeRequest>(&bytes).unwrap(),
            filtered
        );
    }

    #[test]
    fn describe_response_round_trips() {
        let resp = DescribeResponse {
            tools: vec![sample()],
        };
        let bytes = serde_json::to_vec(&resp).unwrap();
        let back: DescribeResponse = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back, resp);
        assert_eq!(back.tools[0].input_schema, json!({ "type": "object" }));
    }

    #[test]
    fn empty_response_is_the_default() {
        assert_eq!(DescribeResponse::default().tools.len(), 0);
    }
}
