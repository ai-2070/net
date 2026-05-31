//! Cross-language transfer wire-format fixture test (Transport SDK plan
//! T-B). Pins the **postcard** byte encoding of [`TransferControl`] and
//! [`TransferHeader`] so every language tier (Rust here; C/Go, Python,
//! TypeScript in T-H once their bindings ship) round-trips the exact
//! same bytes.
//!
//! Unlike `tool_event_vectors.json` (a JSON-wire type), the transfer
//! control/header types ride postcard, so the fixture stores the
//! expected encoding as hex. For each case this test:
//!
//! 1. Builds the canonical value from the fixture's logical fields.
//! 2. Encodes it with postcard and asserts the bytes equal `postcard_hex`.
//! 3. Decodes `postcard_hex` back and asserts it equals the built value.
//!
//! To (re)generate the fixture after a deliberate wire change, run the
//! ignored emitter and paste its stdout into the fixture file:
//! `cargo test -p net-mesh-sdk --features net,dataforts \
//!   --test transfer_golden_vectors emit_fixture -- --ignored --nocapture`
//!
//! Run: `cargo test -p net-mesh-sdk --features net,dataforts \
//!   --test transfer_golden_vectors`

#![cfg(all(feature = "net", feature = "dataforts"))]

use std::path::PathBuf;

use net_sdk::transport::{TransferControl, TransferHeader};
use serde_json::Value;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("tests")
        .join("cross_lang_transfer_formats")
        .join("transfer_vectors.json")
}

fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn from_hex(s: &str) -> Vec<u8> {
    assert!(s.len() % 2 == 0, "odd-length hex: {s}");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex"))
        .collect()
}

fn hash_from_hex(s: &str) -> [u8; 32] {
    let v = from_hex(s);
    assert_eq!(v.len(), 32, "hash must be 32 bytes, got {}", v.len());
    let mut h = [0u8; 32];
    h.copy_from_slice(&v);
    h
}

/// Build the canonical `TransferControl` for a fixture case's fields.
fn control_from_fields(fields: &Value) -> TransferControl {
    let hash = hash_from_hex(fields["hash"].as_str().expect("control.hash hex"));
    TransferControl::Request { hash }
}

/// Build the canonical `TransferHeader` for a fixture case's fields.
fn header_from_fields(fields: &Value) -> TransferHeader {
    match fields["kind"].as_str().expect("header.kind") {
        "found" => TransferHeader::Found {
            total_len: fields["total_len"].as_u64().expect("header.total_len u64"),
        },
        "not_found" => TransferHeader::NotFound,
        other => panic!("unknown header kind `{other}`"),
    }
}

#[test]
fn transfer_control_round_trips_golden_vectors() {
    let fixture: Value =
        serde_json::from_str(&std::fs::read_to_string(fixture_path()).expect("read fixture"))
            .expect("fixture JSON parses");
    let cases = fixture["transfer_control"]
        .as_array()
        .expect("control cases");
    assert!(!cases.is_empty(), "no transfer_control cases");
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let expected_hex = case["postcard_hex"].as_str().unwrap();
        let value = control_from_fields(&case["fields"]);

        let encoded = postcard::to_allocvec(&value)
            .unwrap_or_else(|e| panic!("case `{name}`: encode failed: {e}"));
        assert_eq!(
            to_hex(&encoded),
            expected_hex,
            "case `{name}`: postcard bytes differ from fixture"
        );

        let decoded: TransferControl = postcard::from_bytes(&from_hex(expected_hex))
            .unwrap_or_else(|e| panic!("case `{name}`: decode failed: {e}"));
        assert_eq!(decoded, value, "case `{name}`: decoded value differs");
    }
}

#[test]
fn transfer_header_round_trips_golden_vectors() {
    let fixture: Value =
        serde_json::from_str(&std::fs::read_to_string(fixture_path()).expect("read fixture"))
            .expect("fixture JSON parses");
    let cases = fixture["transfer_header"].as_array().expect("header cases");
    assert!(!cases.is_empty(), "no transfer_header cases");
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let expected_hex = case["postcard_hex"].as_str().unwrap();
        let value = header_from_fields(&case["fields"]);

        let encoded = postcard::to_allocvec(&value)
            .unwrap_or_else(|e| panic!("case `{name}`: encode failed: {e}"));
        assert_eq!(
            to_hex(&encoded),
            expected_hex,
            "case `{name}`: postcard bytes differ from fixture"
        );

        let decoded: TransferHeader = postcard::from_bytes(&from_hex(expected_hex))
            .unwrap_or_else(|e| panic!("case `{name}`: decode failed: {e}"));
        assert_eq!(decoded, value, "case `{name}`: decoded value differs");
    }
}

/// Regenerator for the fixture (see module docs). Prints the full
/// `transfer_vectors.json` to stdout; ignored so it never runs in the
/// normal suite. The canonical case set lives here — edit it, run the
/// emitter, paste the output into the fixture, and the assertion tests
/// above lock it.
#[test]
#[ignore = "fixture regenerator; run with --ignored --nocapture"]
fn emit_fixture() {
    fn control_case(name: &str, hash: [u8; 32]) -> String {
        let bytes = postcard::to_allocvec(&TransferControl::Request { hash }).unwrap();
        format!(
            "    {{ \"name\": \"{name}\", \"fields\": {{ \"hash\": \"{}\" }}, \"postcard_hex\": \"{}\" }}",
            to_hex(&hash),
            to_hex(&bytes)
        )
    }
    fn found_case(name: &str, total_len: u64) -> String {
        let bytes = postcard::to_allocvec(&TransferHeader::Found { total_len }).unwrap();
        format!(
            "    {{ \"name\": \"{name}\", \"fields\": {{ \"kind\": \"found\", \"total_len\": {total_len} }}, \"postcard_hex\": \"{}\" }}",
            to_hex(&bytes)
        )
    }
    fn not_found_case(name: &str) -> String {
        let bytes = postcard::to_allocvec(&TransferHeader::NotFound).unwrap();
        format!(
            "    {{ \"name\": \"{name}\", \"fields\": {{ \"kind\": \"not_found\" }}, \"postcard_hex\": \"{}\" }}",
            to_hex(&bytes)
        )
    }

    let mut seq = [0u8; 32];
    for (i, b) in seq.iter_mut().enumerate() {
        *b = i as u8;
    }

    let controls = [
        control_case("request_hash_zero", [0u8; 32]),
        control_case("request_hash_sequential", seq),
        control_case("request_hash_ff", [0xFFu8; 32]),
    ];
    let headers = [
        found_case("found_zero", 0),
        found_case("found_one", 1),
        found_case("found_300", 300),
        found_case("found_4mib", 4 * 1024 * 1024),
        found_case("found_u64_max", u64::MAX),
        not_found_case("not_found"),
    ];

    println!("{{");
    println!("  \"_comment\": \"Canonical postcard wire vectors for TransferControl / TransferHeader (Transport SDK plan T-B). Regenerate via the emit_fixture test.\",");
    println!("  \"transfer_control\": [");
    println!("{}", controls.join(",\n"));
    println!("  ],");
    println!("  \"transfer_header\": [");
    println!("{}", headers.join(",\n"));
    println!("  ]");
    println!("}}");
}
