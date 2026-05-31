//! Mirror test for the transport C ABI error-code constants
//! (Transport SDK plan, T-C).
//!
//! Two places hand-maintain the same values:
//!   1. `src/ffi/transport.rs` — Rust `pub const NET_*`.
//!   2. `include/net_transport.h` — C `#define NET_*`.
//!
//! Drift is silent at runtime but breaks cross-language pinning (the
//! Go binding in T-F maps these codes to a Go error type). This test
//! parses the header and asserts every transport `#define` matches its
//! Rust sibling, bidirectionally (header→Rust catches a macro Rust
//! lacks; Rust→header catches a constant the header forgot).
//!
//! Mirrors `tests/error_kind_mirror.rs` (which covers `NET_REGISTRY_*`
//! in `net.h`); the parser keys off the transport code prefixes.

#![cfg(all(
    feature = "net",
    feature = "dataforts",
    feature = "netdb",
    feature = "redex-disk"
))]

use std::fs;
use std::path::PathBuf;

use net::ffi::transport::{
    NET_ERR_DIR_INVALID_MANIFEST, NET_ERR_DIR_IO, NET_ERR_DIR_PATH_INVALID,
    NET_ERR_TRANSFER_ALL_PEERS_FAILED, NET_ERR_TRANSFER_BACKEND, NET_ERR_TRANSFER_CANCELLED,
    NET_ERR_TRANSFER_ENGINE_NOT_INSTALLED, NET_ERR_TRANSFER_HASH_MISMATCH,
    NET_ERR_TRANSFER_INVALID_ARGUMENT, NET_ERR_TRANSFER_NOT_FOUND, NET_ERR_TRANSFER_NULL_POINTER,
    NET_ERR_TRANSFER_PANIC, NET_ERR_TRANSFER_SHUTTING_DOWN, NET_TRANSPORT_OK,
};

fn header_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("include")
        .join("net_transport.h")
}

/// A header `#define NAME VALUE` is a transport code iff its name is
/// `NET_TRANSPORT_OK` or starts with `NET_ERR_TRANSFER_` /
/// `NET_ERR_DIR_`. Ignores the shared-handle `_DEFINED` sentinels and
/// any unrelated macros.
fn is_transport_code(name: &str) -> bool {
    name == "NET_TRANSPORT_OK"
        || name.starts_with("NET_ERR_TRANSFER_")
        || name.starts_with("NET_ERR_DIR_")
}

/// Pull transport `#define NAME VALUE` pairs out of the header, in
/// source order, stripping trailing `/* ... */` comments.
fn parse_header_transport_defines() -> Vec<(String, i32)> {
    let header = fs::read_to_string(header_path())
        .unwrap_or_else(|e| panic!("read {}: {e}", header_path().display()));
    let mut out = Vec::new();
    for line in header.lines() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix("#define") else {
            continue;
        };
        let rest = rest.trim();
        let mut parts = rest.split_whitespace();
        let Some(name) = parts.next() else { continue };
        if !is_transport_code(name) {
            continue;
        }
        let Some(value_str) = parts.next() else {
            panic!("malformed #define for {name}: missing value");
        };
        let value_str = value_str
            .split("/*")
            .next()
            .unwrap_or(value_str)
            .split("//")
            .next()
            .unwrap_or(value_str)
            .trim();
        let value: i32 = value_str
            .parse()
            .unwrap_or_else(|e| panic!("non-integer value for {name}: {value_str:?} ({e})"));
        out.push((name.to_string(), value));
    }
    out
}

#[test]
fn every_transport_error_define_matches_rust_constant() {
    let pairs = parse_header_transport_defines();
    assert!(
        !pairs.is_empty(),
        "header at {} produced no transport defines",
        header_path().display(),
    );

    // The full mapping. Add new variants here alongside the Rust const
    // + header #define; the test then locks all three together.
    let expected: &[(&str, i32)] = &[
        ("NET_TRANSPORT_OK", NET_TRANSPORT_OK),
        ("NET_ERR_TRANSFER_NOT_FOUND", NET_ERR_TRANSFER_NOT_FOUND),
        (
            "NET_ERR_TRANSFER_HASH_MISMATCH",
            NET_ERR_TRANSFER_HASH_MISMATCH,
        ),
        (
            "NET_ERR_TRANSFER_ALL_PEERS_FAILED",
            NET_ERR_TRANSFER_ALL_PEERS_FAILED,
        ),
        ("NET_ERR_TRANSFER_CANCELLED", NET_ERR_TRANSFER_CANCELLED),
        (
            "NET_ERR_TRANSFER_NULL_POINTER",
            NET_ERR_TRANSFER_NULL_POINTER,
        ),
        (
            "NET_ERR_TRANSFER_SHUTTING_DOWN",
            NET_ERR_TRANSFER_SHUTTING_DOWN,
        ),
        (
            "NET_ERR_TRANSFER_ENGINE_NOT_INSTALLED",
            NET_ERR_TRANSFER_ENGINE_NOT_INSTALLED,
        ),
        ("NET_ERR_TRANSFER_BACKEND", NET_ERR_TRANSFER_BACKEND),
        ("NET_ERR_TRANSFER_PANIC", NET_ERR_TRANSFER_PANIC),
        (
            "NET_ERR_TRANSFER_INVALID_ARGUMENT",
            NET_ERR_TRANSFER_INVALID_ARGUMENT,
        ),
        ("NET_ERR_DIR_INVALID_MANIFEST", NET_ERR_DIR_INVALID_MANIFEST),
        ("NET_ERR_DIR_PATH_INVALID", NET_ERR_DIR_PATH_INVALID),
        ("NET_ERR_DIR_IO", NET_ERR_DIR_IO),
    ];

    // Header → Rust.
    for (name, header_value) in &pairs {
        let expected_value = expected
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| *v)
            .unwrap_or_else(|| {
                panic!(
                    "header has unknown #define `{name}` = {header_value} (Rust constant missing or test allow-list not updated)"
                )
            });
        assert_eq!(
            *header_value, expected_value,
            "header `#define {name} {header_value}` disagrees with Rust constant ({expected_value})",
        );
    }

    // Rust → Header.
    let header_names: Vec<&str> = pairs.iter().map(|(n, _)| n.as_str()).collect();
    for (name, _) in expected {
        assert!(
            header_names.contains(name),
            "Rust constant `{name}` is not declared in `include/net_transport.h`. Add the `#define` or remove the constant.",
        );
    }

    // Codes are distinct (a copy-paste collision would silently merge
    // two failure modes).
    let mut codes: Vec<i32> = expected.iter().map(|(_, c)| *c).collect();
    codes.sort_unstable();
    codes.dedup();
    assert_eq!(codes.len(), expected.len(), "duplicate transport code");
}
