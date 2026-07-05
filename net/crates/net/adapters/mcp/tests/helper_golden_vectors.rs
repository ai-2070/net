//! Cross-language MCP bridge helper-parity fixture test
//! (`MCP_BRIDGE_SDK_PLAN.md` P1-P3 conformance).
//!
//! Loads `crates/net/tests/cross_lang_mcp/helper_vectors.json` — the
//! canonical fixture pinning `classify` / `lower_tool` parity across the
//! Rust core and every binding. The same file drives the Python
//! (`test_mcp_helper_golden_vectors.py`), Node
//! (`mcp_helper_golden_vectors.test.ts`), and Go
//! (`mcp_helper_golden_vectors_test.go`) verifiers.
//!
//! This Rust verifier is the **source of truth**: it calls
//! `net_mcp::wrap::{classify, lower_tool}` directly, so a failure here means
//! the fixture is wrong; a failure in a binding verifier means that binding
//! drifted from the core.
//!
//! Run: `cargo test -p net-mesh-mcp --test helper_golden_vectors`

use std::path::PathBuf;

use net_mcp::wrap::{
    classify, lower_tool, CredentialOverride, CredentialStatus, LoweringContext, Substitutability,
    WrapEnv,
};
use serde_json::{json, Value};

fn fixture_path() -> PathBuf {
    // adapters/mcp/tests/<this file>.rs — the fixture lives in the workspace
    // at crates/net/tests/cross_lang_mcp/.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("cross_lang_mcp")
        .join("helper_vectors.json")
}

fn load_fixture() -> Value {
    let path = fixture_path();
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read fixture {path:?}: {e}"));
    serde_json::from_str(&raw).expect("fixture JSON parses")
}

fn str_vec(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| a.iter().map(|s| s.as_str().unwrap().to_string()).collect())
        .unwrap_or_default()
}

fn env_pairs(v: &Value) -> Vec<(String, String)> {
    v.as_object()
        .map(|m| {
            m.iter()
                .map(|(k, val)| (k.clone(), val.as_str().unwrap().to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn override_of(v: &Value) -> CredentialOverride {
    match v.as_str() {
        None | Some("detect") => CredentialOverride::Detect,
        Some("credentialed") => CredentialOverride::Credentialed,
        Some("no-credentials") => CredentialOverride::NoCredentials,
        Some(other) => panic!("unknown credential_override {other:?} in fixture"),
    }
}

#[test]
fn classify_parity() {
    let fixture = load_fixture();
    for case in fixture["classify"].as_array().expect("classify array") {
        let name = case["name"].as_str().unwrap();
        let program = case["program"].as_str().unwrap();
        let args = str_vec(&case["args"]);
        let envs = env_pairs(&case["envs"]);
        let over = override_of(&case["credential_override"]);
        let force = case["force"].as_bool().unwrap();

        let status = classify(
            &WrapEnv {
                program,
                args: &args,
                envs: &envs,
            },
            over,
            force,
        )
        .unwrap_or_else(|e| panic!("[{name}] classify errored: {e}"));

        assert_eq!(
            status.as_str(),
            case["expected_status"].as_str().unwrap(),
            "[{name}] classify status mismatch",
        );
    }
}

/// Reshape a produced lowered tool into the fixture's canonical comparison
/// shape: the descriptor's `input_schema` / `output_schema` JSON *strings*
/// become parsed `input_schema_object` / `output_schema_object`, so the
/// comparison is by value (whitespace / key order agnostic).
fn normalize_lowered(lowered: &net_mcp::wrap::LoweredTool) -> Value {
    let d = &lowered.descriptor;
    let parse_schema = |s: &Option<String>| -> Value {
        match s {
            Some(s) => serde_json::from_str(s).expect("descriptor schema string parses"),
            None => Value::Null,
        }
    };
    json!({
        "tool_id": d.tool_id,
        "mcp_name": lowered.mcp_name,
        "bridge_metadata": lowered.bridge_metadata,
        "descriptor": {
            "tool_id": d.tool_id,
            "name": d.name,
            "version": d.version,
            "description": d.description,
            "input_schema_object": parse_schema(&d.input_schema),
            "output_schema_object": parse_schema(&d.output_schema),
            "requires": d.requires,
            "estimated_time_ms": d.estimated_time_ms,
            "stateless": d.stateless,
            "streaming": d.streaming,
            "tags": d.tags,
            "node_count": d.node_count,
        },
    })
}

#[test]
fn lower_parity() {
    let fixture = load_fixture();
    for case in fixture["lower"].as_array().expect("lower array") {
        let name = case["name"].as_str().unwrap();
        let tool: net_mcp::spec::Tool =
            serde_json::from_value(case["tool"].clone()).expect("fixture tool deserializes");
        let credential_status = CredentialStatus::from_label(case["credential_status"].as_str().unwrap())
            .expect("fixture credential_status is a valid label");
        let substitutability = match case["substitutability"].as_str().unwrap() {
            "provider_local" => Substitutability::ProviderLocal,
            "provider_equivalent" => Substitutability::ProviderEquivalent,
            other => panic!("unknown substitutability {other:?} in fixture"),
        };

        let lowered = lower_tool(
            &tool,
            &LoweringContext {
                server_version: case["server_version"].as_str().unwrap().to_string(),
                credential_status,
                substitutability,
            },
        );

        let got = normalize_lowered(&lowered);
        let want = &case["expected"];
        assert_eq!(
            &got, want,
            "[{name}] lower DTO mismatch\n got: {}\nwant: {}",
            serde_json::to_string_pretty(&got).unwrap(),
            serde_json::to_string_pretty(want).unwrap(),
        );
    }
}
