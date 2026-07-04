//! The `net_*` meta-tool surface the host sees (`MCP_BRIDGE_PLAN.md` Phase 2,
//! `serve/meta_tools.rs`).
//!
//! The shim's default `tools/list` is **meta-tools only** — not the raw mesh
//! capabilities. The model searches, describes, then invokes through these
//! five tools; a capability only becomes a first-class typed tool once pinned
//! (Phase 3). This keeps the host's tool list small (host truncation is a real
//! risk) and puts consent + validation on the invoke path.

use crate::spec::Tool;

/// The meta-tool method names, in one place.
pub mod name {
    /// Search the mesh for capabilities matching a query.
    pub const SEARCH: &str = "net_search_capabilities";
    /// Full schema + risk/credential status for one capability.
    pub const DESCRIBE: &str = "net_describe_capability";
    /// Invoke a capability (pre-flight validated + consent-gated).
    pub const INVOKE: &str = "net_invoke_capability";
    /// List the capabilities pinned as first-class tools.
    pub const LIST_PINNED: &str = "net_list_pinned_capabilities";
    /// Request a pin for a capability — creates a *pending* request; does not
    /// grant anything (Phase 3: consent happens outside the model loop).
    pub const REQUEST_PIN: &str = "net_request_pin";
}

/// Build a meta-tool [`Tool`] with an object input schema.
fn tool(name: &str, description: &str, input_schema: serde_json::Value) -> Tool {
    Tool {
        name: name.to_string(),
        title: None,
        description: Some(description.to_string()),
        input_schema,
        output_schema: None,
    }
}

/// A `{ <field>: string }` required-object schema — the common meta-tool shape.
fn one_string_arg(field: &str, description: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": { field: { "type": "string", "description": description } },
        "required": [field],
        "additionalProperties": false,
    })
}

/// The full meta-tool list returned by `tools/list` in the default surface.
pub fn meta_tools() -> Vec<Tool> {
    vec![
        tool(
            name::SEARCH,
            "Search the trusted mesh for capabilities (tools published by other \
             machines via `net wrap`). Returns matching capabilities with their \
             provider, compat tier, and whether they require local approval to \
             invoke. Use this first — the mesh's tools are not listed directly.",
            one_string_arg(
                "query",
                "Free-text query matched against capability id, name, and description.",
            ),
        ),
        tool(
            name::DESCRIBE,
            "Describe one capability: its full input schema, credential/risk \
             status, provider, and compat tier. Call this before invoking so you \
             pass correctly-shaped arguments.",
            one_string_arg(
                "cap_id",
                "The capability id from a search result, e.g. `homelab/github.create_issue`.",
            ),
        ),
        tool(
            name::INVOKE,
            "Invoke a capability with arguments validated against its schema \
             first. Capabilities that carry credentials or reach external APIs \
             require local approval (allowlist or an approved pin) before they \
             will run; search/describe still work without it.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "cap_id": {
                        "type": "string",
                        "description": "The capability id, e.g. `homelab/github.create_issue`.",
                    },
                    "arguments": {
                        "type": "object",
                        "description": "Arguments for the capability, matching its input schema (see net_describe_capability).",
                    },
                },
                "required": ["cap_id"],
                "additionalProperties": false,
            }),
        ),
        tool(
            name::LIST_PINNED,
            "List capabilities that have been pinned as first-class tools for \
             this machine and user profile.",
            serde_json::json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        ),
        tool(
            name::REQUEST_PIN,
            "Request that a capability be pinned as a first-class tool. This only \
             creates a pending request — a human must approve it out-of-band \
             (`net mcp pin approve <id>`); it grants no access by itself.",
            one_string_arg("cap_id", "The capability id to request a pin for."),
        ),
    ]
}

/// Is `name` one of the meta-tools?
pub fn is_meta_tool(name: &str) -> bool {
    matches!(
        name,
        self::name::SEARCH
            | self::name::DESCRIBE
            | self::name::INVOKE
            | self::name::LIST_PINNED
            | self::name::REQUEST_PIN
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_all_five_meta_tools_with_object_schemas() {
        let tools = meta_tools();
        assert_eq!(tools.len(), 5);
        for t in &tools {
            assert!(is_meta_tool(&t.name), "{} should be a meta-tool", t.name);
            assert_eq!(
                t.input_schema.get("type").and_then(|v| v.as_str()),
                Some("object"),
                "{} input schema must be an object",
                t.name,
            );
            assert!(t.description.is_some(), "{} needs a description", t.name);
        }
    }

    #[test]
    fn every_meta_tool_name_is_recognised() {
        for name in [
            name::SEARCH,
            name::DESCRIBE,
            name::INVOKE,
            name::LIST_PINNED,
            name::REQUEST_PIN,
        ] {
            assert!(is_meta_tool(name));
        }
        assert!(!is_meta_tool("tools/call"));
        assert!(!is_meta_tool("net_unknown"));
    }

    #[test]
    fn search_and_describe_require_their_string_arg() {
        let tools = meta_tools();
        let search = tools.iter().find(|t| t.name == name::SEARCH).unwrap();
        assert_eq!(
            search.input_schema["required"],
            serde_json::json!(["query"])
        );
        let describe = tools.iter().find(|t| t.name == name::DESCRIBE).unwrap();
        assert_eq!(
            describe.input_schema["required"],
            serde_json::json!(["cap_id"]),
        );
    }

    #[test]
    fn invoke_requires_cap_id_but_arguments_optional() {
        let tools = meta_tools();
        let invoke = tools.iter().find(|t| t.name == name::INVOKE).unwrap();
        assert_eq!(
            invoke.input_schema["required"],
            serde_json::json!(["cap_id"])
        );
        assert!(invoke.input_schema["properties"].get("arguments").is_some());
    }
}
