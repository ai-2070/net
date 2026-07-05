//! MCP bridge pure-helper bindings (`MCP_BRIDGE_SDK_PLAN.md` P1).
//!
//! Exactly two functions, matching the plan's surface table: `classify`
//! (credential-risk scoring, so a native node can *display* risk before
//! publishing an MCP-backed tool through the general SDK) and `lower_tool`
//! (an MCP `tools/list` entry lowered to the Net `ToolDescriptor` + bridge
//! metadata). Both are pure — no mesh, no process, no secret ever crosses:
//! `classify` reasons over env KEYS and returns a fixed classification
//! label; `lower` sees only tool names/schemas. The bridge's forwarding /
//! keychain internals are never bound (doctrine #3).
//!
//! Results cross the boundary as JSON strings so the cross-binding
//! helper-parity vectors can compare byte-identical DTOs.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

use net_mcp::wrap::{
    classify, lower_tool, CredentialOverride, CredentialStatus, LoweringContext, Substitutability,
    WrapEnv,
};

fn mcp_err(msg: impl std::fmt::Display) -> PyErr {
    PyValueError::new_err(format!("mcp: {msg}"))
}

/// Classify a wrapped MCP server's credential exposure. Returns the status
/// label: `"credentialed"`, `"external_api"`, `"unknown"`, or `"none"`.
///
/// Conservative by construction (the classifier is the bridge's one Rust
/// implementation): detection can never yield the ungated `"none"` — only
/// the explicit `credential_override="no-credentials"` with `force=True`
/// can, mirroring `net wrap --no-credentials --force`. Only env KEYS drive
/// detection; values are never inspected beyond presence and never appear
/// in the result.
#[pyfunction]
#[pyo3(signature = (program, args, envs, credential_override=None, force=false))]
pub fn classify_mcp_server(
    program: &str,
    args: Vec<String>,
    envs: Vec<(String, String)>,
    credential_override: Option<&str>,
    force: bool,
) -> PyResult<&'static str> {
    let over = match credential_override {
        None | Some("detect") => CredentialOverride::Detect,
        Some("credentialed") => CredentialOverride::Credentialed,
        Some("no-credentials") => CredentialOverride::NoCredentials,
        Some(other) => {
            return Err(mcp_err(format!(
                "unknown credential_override {other:?} (expected detect | credentialed | no-credentials)"
            )))
        }
    };
    classify(
        &WrapEnv {
            program,
            args: &args,
            envs: &envs,
        },
        over,
        force,
    )
    .map(CredentialStatus::as_str)
    .map_err(mcp_err)
}

/// Lower one MCP `tools/list` entry (as its JSON object) to the Net
/// discovery shape. Returns a JSON string:
/// `{"tool_id", "mcp_name", "descriptor", "bridge_metadata"}` —
/// `tool_id` is the channel-safe (possibly sanitized) service id,
/// `mcp_name` the original name `tools/call` must use, `descriptor` the
/// `net_sdk` ToolDescriptor, and `bridge_metadata` the
/// `tool::<id>::<field>` announcement metadata (classification labels
/// only, never a secret).
///
/// `credential_status` takes the exact label the classifier produced
/// (including a forced `"none"` — this is trusted local input, not a wire
/// value); `substitutability` is `"provider_local"` or
/// `"provider_equivalent"`.
#[pyfunction]
#[pyo3(signature = (tool_json, server_version, credential_status, substitutability="provider_local"))]
pub fn lower_mcp_tool(
    tool_json: &str,
    server_version: &str,
    credential_status: &str,
    substitutability: &str,
) -> PyResult<String> {
    let tool: net_mcp::spec::Tool = serde_json::from_str(tool_json)
        .map_err(|e| mcp_err(format!("invalid tools/list entry: {e}")))?;
    let credential_status = CredentialStatus::from_label(credential_status).ok_or_else(|| {
        mcp_err(format!(
            "unknown credential_status {credential_status:?} (expected credentialed | external_api | unknown | none)"
        ))
    })?;
    let substitutability = match substitutability {
        "provider_local" => Substitutability::ProviderLocal,
        "provider_equivalent" => Substitutability::ProviderEquivalent,
        other => {
            return Err(mcp_err(format!(
                "unknown substitutability {other:?} (expected provider_local | provider_equivalent)"
            )))
        }
    };

    let lowered = lower_tool(
        &tool,
        &LoweringContext {
            server_version: server_version.to_string(),
            credential_status,
            substitutability,
        },
    );
    serde_json::to_string(&serde_json::json!({
        "tool_id": lowered.descriptor.tool_id,
        "mcp_name": lowered.mcp_name,
        "descriptor": lowered.descriptor,
        "bridge_metadata": lowered.bridge_metadata,
    }))
    .map_err(|e| mcp_err(format!("encode lowered tool: {e}")))
}
