// `#[napi]` exports to JS leave items "unused" from Rust's POV, so
// clippy's dead-code analysis doesn't apply here. Suppress at file scope.
#![allow(dead_code)]

//! NAPI surface for the MCP bridge pure helpers
//! (`MCP_BRIDGE_SDK_PLAN.md` P2).
//!
//! Exactly two functions, matching the plan's surface table:
//! `classifyMcpServer` (credential-risk scoring, so a native node can
//! *display* risk before publishing an MCP-backed tool through the
//! general SDK) and `lowerMcpTool` (an MCP `tools/list` entry lowered to
//! the Net `ToolDescriptor` + bridge metadata). Both are pure — no mesh,
//! no process, no secret ever crosses: `classify` reasons over env KEYS
//! and returns a fixed classification label; `lower` sees only tool
//! names/schemas. The bridge's forwarding / keychain internals are never
//! bound (bridge doctrine #3).
//!
//! `lowerMcpTool` returns a typed POJO. `descriptor` is a JSON-encoded
//! string (the Node binding's convention for structured payloads — see
//! `ToolDescriptorJs.inputSchema`; JS does `JSON.parse(result.descriptor)`),
//! and `bridgeMetadata` a `Record<string,string>`, so the DTO is
//! byte-comparable against the other bindings' helper-parity vectors.

use napi::bindgen_prelude::*;
use napi_derive::napi;

use net_mcp::wrap::{
    classify, lower_tool, CredentialOverride, CredentialStatus, LoweringContext, Substitutability,
    WrapEnv,
};

const ERR_MCP_PREFIX: &str = "mcp:";

fn mcp_err(msg: impl std::fmt::Display) -> Error {
    Error::from_reason(format!("{ERR_MCP_PREFIX} {msg}"))
}

/// One env addition passed to the wrapped server — only the KEY drives
/// classification; the value is never inspected beyond presence and never
/// appears in a result.
#[napi(object)]
pub struct EnvPairJs {
    pub key: String,
    pub value: String,
}

/// The lowered-tool DTO. `descriptor` is the `net_sdk` ToolDescriptor as a
/// JSON-encoded string (`JSON.parse` it), and `bridgeMetadata` the
/// `tool::<id>::<field>` announcement metadata (classification labels
/// only, never a secret).
#[napi(object)]
pub struct LoweredToolJs {
    /// The channel-safe (possibly sanitized) service id.
    pub tool_id: String,
    /// The original tool name `tools/call` must use.
    pub mcp_name: String,
    /// The Net discovery descriptor, JSON-encoded (`JSON.parse` it).
    pub descriptor: String,
    /// `tool::<id>::<field>` announcement metadata — labels only.
    pub bridge_metadata: std::collections::HashMap<String, String>,
}

/// Classify a wrapped MCP server's credential exposure. Returns the status
/// label: `"credentialed"`, `"external_api"`, `"unknown"`, or `"none"`.
///
/// Conservative by construction (the classifier is the bridge's one Rust
/// implementation): detection can never yield the ungated `"none"` — only
/// `credentialOverride="no-credentials"` with `force=true` can, mirroring
/// `net wrap --no-credentials --force`. Only env KEYS drive detection;
/// values are never inspected beyond presence and never appear in the
/// result.
#[napi]
pub fn classify_mcp_server(
    program: String,
    args: Vec<String>,
    envs: Vec<EnvPairJs>,
    credential_override: Option<String>,
    force: Option<bool>,
) -> Result<String> {
    let over = match credential_override.as_deref() {
        None => CredentialOverride::Detect,
        Some(label) => CredentialOverride::from_wire(label).ok_or_else(|| {
            mcp_err(format!(
                "unknown credentialOverride {label:?} (expected {})",
                CredentialOverride::EXPECTED
            ))
        })?,
    };
    let envs: Vec<(String, String)> = envs.into_iter().map(|e| (e.key, e.value)).collect();
    classify(
        &WrapEnv {
            program: &program,
            args: &args,
            envs: &envs,
        },
        over,
        force.unwrap_or(false),
    )
    .map(|s| s.as_str().to_string())
    .map_err(mcp_err)
}

/// Lower one MCP `tools/list` entry (as its JSON object string) to the Net
/// discovery shape.
///
/// `credentialStatus` takes the exact label the classifier produced
/// (including a forced `"none"` — this is trusted local input, not a wire
/// value); `substitutability` is `"provider_local"` (default) or
/// `"provider_equivalent"`.
#[napi]
pub fn lower_mcp_tool(
    tool_json: String,
    server_version: String,
    credential_status: String,
    substitutability: Option<String>,
) -> Result<LoweredToolJs> {
    let tool: net_mcp::spec::Tool = serde_json::from_str(&tool_json)
        .map_err(|e| mcp_err(format!("invalid tools/list entry: {e}")))?;
    let credential_status = CredentialStatus::from_label(&credential_status).ok_or_else(|| {
        mcp_err(format!(
            "unknown credentialStatus {credential_status:?} (expected credentialed | external_api | unknown | none)"
        ))
    })?;
    let substitutability = match substitutability.as_deref() {
        None => Substitutability::ProviderLocal,
        Some(label) => Substitutability::from_label(label).ok_or_else(|| {
            mcp_err(format!(
                "unknown substitutability {label:?} (expected {})",
                Substitutability::EXPECTED
            ))
        })?,
    };

    let lowered = lower_tool(
        &tool,
        &LoweringContext {
            server_version,
            credential_status,
            substitutability,
        },
    );
    let descriptor = serde_json::to_string(&lowered.descriptor)
        .map_err(|e| mcp_err(format!("encode descriptor: {e}")))?;
    Ok(LoweredToolJs {
        tool_id: lowered.descriptor.tool_id,
        mcp_name: lowered.mcp_name,
        descriptor,
        bridge_metadata: lowered.bridge_metadata.into_iter().collect(),
    })
}
