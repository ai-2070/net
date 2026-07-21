//! OSDK-L X1 witnesses — the cross-language error-vocabulary fixture.
//!
//! Rust is the fixture's producer AND its first consumer, so these tests do two
//! jobs: guard the checked-in file against drift, and demonstrate the parse
//! every other language must reproduce. A binding that disagrees with these
//! assertions disagrees with the contract.

use super::error::{parse_org_wire, OrgErrorDomain};
use super::fixtures::render_error_vectors;

/// The checked-in fixture, as text.
fn fixture_text() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("tests")
        .join("cross_lang_org")
        .join("error_vectors.json");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn fixture_json() -> serde_json::Value {
    serde_json::from_str(&fixture_text()).expect("fixture is valid JSON")
}

/// The drift guard. Adding or renaming a kind changes `render_error_vectors`,
/// which fails this until the fixture is regenerated — and the regenerated
/// fixture then fails every binding's suite until each is updated. That chain
/// is what makes a rename impossible to land silently in four languages.
#[test]
fn the_checked_in_fixture_matches_the_generator() {
    assert_eq!(
        fixture_text(),
        render_error_vectors(),
        "error_vectors.json is stale — regenerate with:\n  \
         cargo run -p net-mesh-sdk --features net,cortex --example gen_org_error_fixtures"
    );
}

/// Every vector's `wire` string parses back to the domain and kind it claims,
/// using the reference parser bindings mirror.
#[test]
fn every_vector_parses_back_to_its_declared_domain_and_kind() {
    let doc = fixture_json();
    let vectors = doc["vectors"].as_array().expect("vectors array");
    assert!(!vectors.is_empty(), "fixture has vectors");

    for v in vectors {
        let wire = v["wire"].as_str().expect("wire");
        let want_domain = v["domain"].as_str().expect("domain");
        let want_kind = v["kind"].as_str().expect("kind");
        let want_local = v["is_local"].as_bool().expect("is_local");

        let (domain, kind) = parse_org_wire(wire);
        assert_eq!(domain.as_wire(), want_domain, "domain for {wire}");
        assert_eq!(kind, Some(want_kind), "kind for {wire}");
        assert_eq!(domain.is_local(), want_local, "is_local for {wire}");
    }
}

/// The load-bearing fact: `is_local` says whether the request left this
/// process. Credentials and discovery are local; admission and rpc are not.
#[test]
fn the_fixture_agrees_with_rust_on_which_domains_are_local() {
    let doc = fixture_json();
    for d in doc["domains"].as_array().expect("domains") {
        let token = d["token"].as_str().expect("token");
        let is_local = d["is_local"].as_bool().expect("is_local");
        let domain = OrgErrorDomain::from_wire(token).expect("known domain token");
        assert_eq!(domain.is_local(), is_local, "is_local for {token}");
    }
    // Spot-pin the split so a future edit cannot quietly move a domain across
    // it — this is the fact a misclassification would destroy.
    assert!(OrgErrorDomain::Credentials.is_local());
    assert!(OrgErrorDomain::Discovery.is_local());
    assert!(!OrgErrorDomain::AdmissionDenied.is_local());
    assert!(!OrgErrorDomain::Rpc.is_local());
}

/// §D5a: an unparseable or unknown-vocabulary string classifies as `unknown`
/// and NEVER as one of the four canonical domains.
#[test]
fn unclassified_cases_never_impersonate_a_canonical_domain() {
    let doc = fixture_json();
    let cases = doc["unclassified_cases"]
        .as_array()
        .expect("unclassified_cases array");
    assert!(!cases.is_empty(), "the unknown row is the point of §D5a");

    for c in cases {
        let wire = c["wire"].as_str().expect("wire");
        let (domain, kind) = parse_org_wire(wire);
        assert_eq!(
            domain,
            OrgErrorDomain::Unclassified,
            "must not classify {wire:?} as {}",
            domain.as_wire()
        );
        assert_eq!(kind, None, "no kind is recovered from {wire:?}");
        assert_eq!(
            domain.as_wire(),
            c["expect_domain"].as_str().expect("expect")
        );
        assert!(
            !domain.is_local(),
            "unknown claims nothing about where the refusal happened"
        );
    }
}

/// A remote denial carries the coarse bucket and no detail — asserted on the
/// fixture itself, so a binding reading it cannot infer a richer reason that
/// would be a credential oracle.
#[test]
fn admission_denial_vectors_carry_no_detail() {
    let doc = fixture_json();
    let mut seen = 0;
    for v in doc["vectors"].as_array().expect("vectors") {
        if v["domain"] != "admission_denied" {
            continue;
        }
        seen += 1;
        let wire = v["wire"].as_str().expect("wire");
        assert_eq!(
            wire.matches(':').count(),
            2,
            "an admission denial must be exactly org:<domain>:<bucket> — got {wire}"
        );
    }
    assert_eq!(seen, 3, "all three coarse buckets appear in the fixture");
}

/// Every kind token is snake_case ASCII, so every target language can match it
/// without escaping or locale surprises.
#[test]
fn kind_tokens_are_portable_across_languages() {
    let doc = fixture_json();
    for v in doc["vectors"].as_array().expect("vectors") {
        let kind = v["kind"].as_str().expect("kind");
        assert!(
            !kind.is_empty()
                && kind
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
            "non-portable kind token: {kind}"
        );
    }
}
