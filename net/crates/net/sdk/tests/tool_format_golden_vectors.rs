//! Cross-language tool-format compatibility fixture test (plan T-1).
//!
//! Loads `crates/net/tests/cross_lang_tool_formats/golden_vectors.json`
//! — the canonical fixture pinning byte-equality across all four
//! `net_sdk::tool::formats` translators (OpenAI / Anthropic / MCP /
//! Gemini, both directions). The same file drives the Node TS,
//! Python, and Go binding tests.
//!
//! Failure of any case here signals cross-binding wire-format
//! drift — investigate the diff before silencing.
//!
//! Run: `cargo test --features tool --test tool_format_golden_vectors`

#![cfg(feature = "tool")]

use std::path::PathBuf;

use net_sdk::tool::formats::{anthropic, gemini, mcp, openai, ToolCallParseError, ToolCallSpec};
use net_sdk::tool::ToolDescriptor;
use serde_json::Value;

fn fixture_path() -> PathBuf {
    // sdk/tests/<this file>.rs — fixtures live in the workspace at
    // crates/net/tests/cross_lang_tool_formats/.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("tests")
        .join("cross_lang_tool_formats")
        .join("golden_vectors.json")
}

fn load_fixture() -> Value {
    let path = fixture_path();
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read fixture {path:?}: {e}"));
    serde_json::from_str(&raw).expect("fixture JSON parses")
}

/// Build a `ToolDescriptor` from the fixture's per-descriptor
/// `input` object. The fixture stores schemas as parsed objects
/// under `input_schema_object` / `output_schema_object` so the
/// JSON is human-readable; the descriptor needs them as
/// JSON-encoded strings (matching the wire shape).
fn descriptor_from_fixture(input: &Value) -> ToolDescriptor {
    let input_schema = input
        .get("input_schema_object")
        .filter(|v| !v.is_null())
        .map(|v| serde_json::to_string(v).expect("input_schema serializes"));
    let output_schema = input
        .get("output_schema_object")
        .filter(|v| !v.is_null())
        .map(|v| serde_json::to_string(v).expect("output_schema serializes"));
    ToolDescriptor {
        tool_id: input["tool_id"].as_str().unwrap().to_string(),
        name: input["name"].as_str().unwrap().to_string(),
        version: input["version"].as_str().unwrap().to_string(),
        description: input
            .get("description")
            .filter(|v| !v.is_null())
            .map(|v| v.as_str().unwrap().to_string()),
        input_schema,
        output_schema,
        requires: input["requires"]
            .as_array()
            .map(|a| a.iter().map(|s| s.as_str().unwrap().to_string()).collect())
            .unwrap_or_default(),
        estimated_time_ms: input["estimated_time_ms"].as_u64().unwrap() as u32,
        stateless: input["stateless"].as_bool().unwrap(),
        streaming: input["streaming"].as_bool().unwrap(),
        tags: input["tags"]
            .as_array()
            .map(|a| a.iter().map(|s| s.as_str().unwrap().to_string()).collect())
            .unwrap_or_default(),
        pricing_terms: input
            .get("pricing_terms")
            .filter(|v| !v.is_null())
            .map(|v| v.as_str().unwrap().to_string()),
        node_count: input["node_count"].as_u64().unwrap() as u32,
    }
}

#[test]
fn descriptor_lowerings_match_golden_vectors() {
    let fixture = load_fixture();
    let cases = fixture["descriptors"]
        .as_array()
        .expect("fixture has `descriptors` array");
    assert!(!cases.is_empty(), "no descriptor cases in fixture");
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let desc = descriptor_from_fixture(&case["input"]);

        // OpenAI
        let got = openai::to_openai_tool(&desc);
        assert_eq!(
            got, case["lowered_openai"],
            "openai lowering mismatch on case `{name}`"
        );

        // Anthropic
        let got = anthropic::to_anthropic_tool(&desc);
        assert_eq!(
            got, case["lowered_anthropic"],
            "anthropic lowering mismatch on case `{name}`"
        );

        // MCP
        let got = mcp::to_mcp_tool(&desc);
        assert_eq!(
            got, case["lowered_mcp"],
            "mcp lowering mismatch on case `{name}`"
        );

        // Gemini
        let got = gemini::to_gemini_function_declaration(&desc);
        assert_eq!(
            got, case["lowered_gemini"],
            "gemini lowering mismatch on case `{name}`"
        );
    }
}

fn assert_lower_spec(case_name: &str, got: &ToolCallSpec, expected: &Value) {
    assert_eq!(
        got.name,
        expected["name"].as_str().unwrap(),
        "case `{case_name}`: name"
    );
    // For OpenAI, expected_spec carries `arguments_json` (string)
    // verbatim; for others, `arguments_parsed` (object) — both
    // representations must agree after a `serde_json::from_str` on
    // got.arguments_json.
    if let Some(want_str) = expected.get("arguments_json").and_then(|v| v.as_str()) {
        assert_eq!(
            got.arguments_json, want_str,
            "case `{case_name}`: arguments_json (string-compared)"
        );
    }
    if let Some(want_parsed) = expected.get("arguments_parsed") {
        let parsed: Value = serde_json::from_str(&got.arguments_json)
            .expect("got.arguments_json must parse as JSON");
        assert_eq!(
            parsed, *want_parsed,
            "case `{case_name}`: arguments (deep-equal)"
        );
    }
    let want_id = expected.get("provider_call_id");
    match (want_id, &got.provider_call_id) {
        (Some(v), Some(g)) if v.is_string() => {
            assert_eq!(
                g,
                v.as_str().unwrap(),
                "case `{case_name}`: provider_call_id"
            );
        }
        (Some(v), None) if v.is_null() => {}
        (None, None) => {}
        (a, b) => panic!("case `{case_name}`: provider_call_id mismatch (want={a:?} got={b:?})"),
    }
}

#[test]
fn lower_openai_matches_golden_vectors() {
    let fixture = load_fixture();
    for case in fixture["lower_openai_cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let spec = openai::lower_openai_tool_call(&case["reply_json"])
            .unwrap_or_else(|e| panic!("case `{name}`: parse failed: {e:?}"));
        assert_lower_spec(name, &spec, &case["expected_spec"]);
    }
}

#[test]
fn lower_anthropic_matches_golden_vectors() {
    let fixture = load_fixture();
    for case in fixture["lower_anthropic_cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let spec = anthropic::lower_anthropic_tool_use(&case["reply_json"])
            .unwrap_or_else(|e| panic!("case `{name}`: parse failed: {e:?}"));
        assert_lower_spec(name, &spec, &case["expected_spec"]);
    }
}

#[test]
fn lower_mcp_matches_golden_vectors() {
    let fixture = load_fixture();
    for case in fixture["lower_mcp_cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let spec = mcp::lower_mcp_tools_call(&case["reply_json"])
            .unwrap_or_else(|e| panic!("case `{name}`: parse failed: {e:?}"));
        assert_lower_spec(name, &spec, &case["expected_spec"]);
    }
}

#[test]
fn lower_gemini_matches_golden_vectors() {
    let fixture = load_fixture();
    for case in fixture["lower_gemini_cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let spec = gemini::lower_gemini_function_call(&case["reply_json"])
            .unwrap_or_else(|e| panic!("case `{name}`: parse failed: {e:?}"));
        assert_lower_spec(name, &spec, &case["expected_spec"]);
    }
}

#[test]
fn error_cases_all_reject() {
    let fixture = load_fixture();
    for case in fixture["error_cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let provider = case["provider"].as_str().unwrap();
        let reply = &case["reply_json"];
        let err: Result<ToolCallSpec, ToolCallParseError> = match provider {
            "openai" => openai::lower_openai_tool_call(reply),
            "anthropic" => anthropic::lower_anthropic_tool_use(reply),
            "mcp" => mcp::lower_mcp_tools_call(reply),
            "gemini" => gemini::lower_gemini_function_call(reply),
            other => panic!("unknown provider `{other}` in case `{name}`"),
        };
        assert!(
            err.is_err(),
            "case `{name}`: expected parse error, got {err:?}",
        );
    }
}
