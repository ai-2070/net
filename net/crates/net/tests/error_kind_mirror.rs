//! Mirror test for the aggregator C ABI error-kind constants.
//!
//! Five places hand-maintain the same discriminant values:
//!   1. `src/ffi/aggregator.rs` — Rust `pub const NET_REGISTRY_*`.
//!   2. `include/net.h`         — C `#define NET_REGISTRY_*`.
//!   3. `bindings/python/src/aggregator.rs` — kind strings on the
//!      typed exception classes.
//!   4. `bindings/node/aggregator.ts` — `RegistryErrorKind` union.
//!   5. `go/aggregator.go` — `RegistryErrKind*` constants.
//!
//! Drift between any of these is silent at runtime but breaks
//! cross-language pinning. This test parses the C header and
//! asserts every `NET_REGISTRY_*` define matches its Rust
//! sibling. The string-form mirrors (Node TS unions, Python
//! kind strings, Go constants) get a separate inline check
//! that every Rust discriminant has a known kebab-case string.
//!
//! Failure mode: if the header drifts (e.g. someone bumps the
//! Rust constant without the header), this test fails with
//! "macro X = 7 (header) but Rust says 8" and the fix is
//! immediate.

#![cfg(feature = "net")]

use std::fs;
use std::path::PathBuf;

use net::ffi::aggregator::{
    NET_REGISTRY_ERR_CODEC, NET_REGISTRY_ERR_DUPLICATE_GROUP_NAME, NET_REGISTRY_ERR_INVALID_ARGS,
    NET_REGISTRY_ERR_SPAWN_NOT_SUPPORTED, NET_REGISTRY_ERR_SPAWN_REJECTED,
    NET_REGISTRY_ERR_TRANSPORT, NET_REGISTRY_ERR_UNKNOWN_KIND, NET_REGISTRY_ERR_UNKNOWN_TEMPLATE,
    NET_REGISTRY_OK,
};

fn header_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("include")
        .join("net.h")
}

/// Pull `#define NAME VALUE` lines out of the header. Returns
/// `(name, value)` pairs in source order. Ignores defines
/// outside the `NET_REGISTRY_*` family — keeps the matcher
/// future-proof against unrelated header additions.
fn parse_header_registry_defines() -> Vec<(String, i32)> {
    let header = fs::read_to_string(header_path())
        .unwrap_or_else(|e| panic!("read {}: {e}", header_path().display()));
    let mut out = Vec::new();
    for line in header.lines() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix("#define") else {
            continue;
        };
        let rest = rest.trim();
        // `NAME      VALUE [/* trailing comment */]`
        let mut parts = rest.split_whitespace();
        let Some(name) = parts.next() else { continue };
        if !name.starts_with("NET_REGISTRY_") {
            continue;
        }
        let Some(value_str) = parts.next() else {
            panic!("malformed #define for {name}: missing value");
        };
        // Strip trailing comments (defensive — current header
        // doesn't have any but a future edit might).
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
fn every_registry_error_define_matches_rust_constant() {
    let pairs = parse_header_registry_defines();
    assert!(
        !pairs.is_empty(),
        "header at {} produced no NET_REGISTRY_* defines",
        header_path().display(),
    );

    // The full mapping. If a new variant is added on the Rust
    // side, also add it here — the test will then fail if the
    // header drifts.
    let expected: &[(&str, i32)] = &[
        ("NET_REGISTRY_OK", NET_REGISTRY_OK),
        ("NET_REGISTRY_ERR_TRANSPORT", NET_REGISTRY_ERR_TRANSPORT),
        ("NET_REGISTRY_ERR_CODEC", NET_REGISTRY_ERR_CODEC),
        (
            "NET_REGISTRY_ERR_UNKNOWN_TEMPLATE",
            NET_REGISTRY_ERR_UNKNOWN_TEMPLATE,
        ),
        (
            "NET_REGISTRY_ERR_DUPLICATE_GROUP_NAME",
            NET_REGISTRY_ERR_DUPLICATE_GROUP_NAME,
        ),
        (
            "NET_REGISTRY_ERR_SPAWN_REJECTED",
            NET_REGISTRY_ERR_SPAWN_REJECTED,
        ),
        (
            "NET_REGISTRY_ERR_SPAWN_NOT_SUPPORTED",
            NET_REGISTRY_ERR_SPAWN_NOT_SUPPORTED,
        ),
        (
            "NET_REGISTRY_ERR_UNKNOWN_KIND",
            NET_REGISTRY_ERR_UNKNOWN_KIND,
        ),
        (
            "NET_REGISTRY_ERR_INVALID_ARGS",
            NET_REGISTRY_ERR_INVALID_ARGS,
        ),
    ];

    // Header → Rust (catches "header has macro Rust doesn't").
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

    // Rust → Header (catches "Rust added a constant the header
    // forgot"). Required since the test allow-list above is
    // hand-maintained; otherwise a stale allow-list would mask
    // the new constant.
    let header_names: Vec<&str> = pairs.iter().map(|(n, _)| n.as_str()).collect();
    for (name, _) in expected {
        assert!(
            header_names.contains(name),
            "Rust constant `{name}` is not declared in `include/net.h`. Either add the `#define` or remove the constant.",
        );
    }
}

/// Every error-kind discriminant must map to a stable kebab-case
/// string that the Node TS union + Python `.kind` attribute +
/// Go `RegistryErrKind*` constants all reproduce verbatim. Lock
/// the mapping in one place; bindings reference it.
///
/// This is the in-tree reference. Bindings have their own
/// per-language tests that build on top of this string set.
#[test]
fn every_registry_error_kind_has_stable_string_discriminant() {
    let pairs: &[(i32, &str)] = &[
        (NET_REGISTRY_ERR_TRANSPORT, "transport"),
        (NET_REGISTRY_ERR_CODEC, "codec"),
        (NET_REGISTRY_ERR_UNKNOWN_TEMPLATE, "unknown-template"),
        (
            NET_REGISTRY_ERR_DUPLICATE_GROUP_NAME,
            "duplicate-group-name",
        ),
        (NET_REGISTRY_ERR_SPAWN_REJECTED, "spawn-rejected"),
        (NET_REGISTRY_ERR_SPAWN_NOT_SUPPORTED, "spawn-not-supported"),
        (NET_REGISTRY_ERR_UNKNOWN_KIND, "unknown-kind"),
        (NET_REGISTRY_ERR_INVALID_ARGS, "invalid-args"),
    ];

    // Sanity: all strings unique, all numeric values unique.
    let mut strings: Vec<&str> = pairs.iter().map(|(_, s)| *s).collect();
    strings.sort_unstable();
    strings.dedup();
    assert_eq!(strings.len(), pairs.len(), "duplicate kind string");

    let mut codes: Vec<i32> = pairs.iter().map(|(c, _)| *c).collect();
    codes.sort_unstable();
    codes.dedup();
    assert_eq!(codes.len(), pairs.len(), "duplicate discriminant code");

    // All-lowercase + kebab-case (no spaces, no underscores).
    for (_, s) in pairs {
        assert!(
            !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
            "kind string {s:?} is not lower-kebab-case",
        );
    }
}
