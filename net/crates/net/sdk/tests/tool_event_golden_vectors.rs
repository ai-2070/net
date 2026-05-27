//! Cross-language ToolEvent envelope round-trip fixture test (plan T-2).
//!
//! Loads `crates/net/tests/cross_lang_tool_formats/tool_event_vectors.json`
//! — the canonical fixture pinning JSON byte-equality across all four
//! language impls of `ToolEvent`. For each case:
//!
//! 1. Deserialize `wire` into Rust's `ToolEvent`.
//! 2. Re-serialize back to `serde_json::Value`.
//! 3. Assert the round-tripped value deep-equals the original `wire`.
//! 4. Assert `is_terminal()` matches the fixture's `is_terminal`.
//!
//! Run: `cargo test --features tool --test tool_event_golden_vectors`

#![cfg(feature = "tool")]

use std::path::PathBuf;

use net_sdk::tool::ToolEvent;
use serde_json::Value;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("tests")
        .join("cross_lang_tool_formats")
        .join("tool_event_vectors.json")
}

fn load_fixture() -> Value {
    let raw = std::fs::read_to_string(fixture_path()).expect("read fixture");
    serde_json::from_str(&raw).expect("fixture JSON parses")
}

#[test]
fn tool_event_round_trip_matches_golden_vectors() {
    let fixture = load_fixture();
    let cases = fixture["cases"].as_array().expect("cases array");
    assert!(!cases.is_empty(), "no cases in fixture");
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let wire = &case["wire"];
        let expected_terminal = case["is_terminal"].as_bool().unwrap();

        // Deserialize wire JSON → ToolEvent.
        let event: ToolEvent = serde_json::from_value(wire.clone())
            .unwrap_or_else(|e| panic!("case `{name}`: deserialize failed: {e}"));

        // Pin is_terminal.
        assert_eq!(
            event.is_terminal(),
            expected_terminal,
            "case `{name}`: is_terminal"
        );

        // Re-serialize and deep-compare.
        let round_tripped = serde_json::to_value(&event)
            .unwrap_or_else(|e| panic!("case `{name}`: serialize failed: {e}"));
        assert_eq!(
            round_tripped, *wire,
            "case `{name}`: round-tripped JSON differs from wire"
        );
    }
}
