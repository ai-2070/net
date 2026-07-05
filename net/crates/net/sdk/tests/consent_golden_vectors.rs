//! Cross-language consent-surface parity fixture test
//! (`MCP_BRIDGE_SDK_PLAN.md` conformance: DTO vectors — CapabilityId,
//! consent decisions).
//!
//! Loads `crates/net/tests/cross_lang_mcp/consent_vectors.json` — the
//! canonical fixture pinning `net_sdk::consent` parity across the Rust core
//! and every binding. The same file drives the Python
//! (`test_consent_golden_vectors.py`), Node
//! (`consent_golden_vectors.test.ts`), and Go
//! (`consent_golden_vectors_test.go`) verifiers.
//!
//! This Rust verifier is the **source of truth**: it calls the
//! `net_sdk::consent` types directly, so a failure here means the fixture is
//! wrong; a failure in a binding verifier means that binding drifted.
//!
//! Run: `cargo test -p net-mesh-sdk --test consent_golden_vectors`

use std::path::PathBuf;

use net_sdk::consent::{CapabilityId, ConsentPolicy, CredentialStatus};
use serde_json::Value;

fn load_fixture() -> Value {
    // sdk/tests/<this file>.rs — fixture lives at
    // crates/net/tests/cross_lang_mcp/.
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("tests")
        .join("cross_lang_mcp")
        .join("consent_vectors.json");
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read fixture {path:?}: {e}"));
    serde_json::from_str(&raw).expect("fixture JSON parses")
}

fn cap(s: &str) -> CapabilityId {
    CapabilityId::parse(s).unwrap_or_else(|e| panic!("parse cap id {s:?}: {e}"))
}

#[test]
fn cap_id_canonicalize_parity() {
    let fixture = load_fixture();
    for case in fixture["cap_id_canonicalize"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let input = case["input"].as_str().unwrap();
        let got = CapabilityId::parse(input)
            .unwrap_or_else(|e| panic!("[{name}] parse {input:?}: {e}"))
            .display();
        assert_eq!(got, case["expected"].as_str().unwrap(), "[{name}]");
    }
}

#[test]
fn cap_id_invalid_parity() {
    let fixture = load_fixture();
    for case in fixture["cap_id_invalid"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let input = case["input"].as_str().unwrap();
        assert!(
            CapabilityId::parse(input).is_err(),
            "[{name}] {input:?} must be rejected",
        );
    }
}

#[test]
fn credential_requires_consent_parity() {
    let fixture = load_fixture();
    for case in fixture["credential_requires_consent"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let status = case["status"].as_str().unwrap();
        let got = CredentialStatus::from_wire(status).requires_consent();
        assert_eq!(got, case["expected"].as_bool().unwrap(), "[{name}]");
    }
}

#[test]
fn consent_decision_parity() {
    let fixture = load_fixture();
    for case in fixture["consent_decision"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let mut policy = ConsentPolicy::new();
        for op in case["ops"].as_array().unwrap() {
            let id = cap(op["cap_id"].as_str().unwrap());
            match op["op"].as_str().unwrap() {
                "allow" => policy.allow(id),
                "pin" => policy.pin(id),
                "unpin" => policy.unpin(&id),
                other => panic!("[{name}] unknown op {other:?} in fixture"),
            }
        }
        let id = cap(case["cap_id"].as_str().unwrap());
        let status = case["credential_status"].as_str().unwrap();
        let got = if policy.requires_approval(&id, status) {
            "requires_approval"
        } else {
            "allowed"
        };
        assert_eq!(got, case["expected"].as_str().unwrap(), "[{name}]");
    }
}
