//! The committed cross-language stable-kind fixture matches the
//! canonical Rust matches it was generated from
//! (`SUBNET_AUTH_SDK_PLAN.md` R4/§7.3).
//!
//! Every binding consumes `tests/cross_lang_subnet/stable_kinds.json`
//! verbatim; this suite is the Rust consumer AND the regeneration
//! guard. Renaming a kind, adding a variant, or editing the JSON by
//! hand fails here until `gen_subnet_kind_fixture` is re-run and the
//! result committed — one source, every language.

#![cfg(feature = "net")]

use net_sdk::subnet::{fact_kind_wire, subnet_auth_kinds, SubnetFactKind, LOCAL_PROVISION_KINDS};

fn fixture_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("sdk has a parent")
        .join(net_sdk::subnet::STABLE_KINDS_FIXTURE_PATH)
}

fn committed_bytes() -> String {
    let path = fixture_path();
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {} (run gen_subnet_kind_fixture): {e}", path.display()))
}

fn committed_fixture() -> serde_json::Value {
    serde_json::from_str(&committed_bytes()).expect("fixture parses")
}

fn strings(v: &serde_json::Value, key: &str) -> Vec<String> {
    v[key]
        .as_array()
        .unwrap_or_else(|| panic!("fixture field {key} is an array"))
        .iter()
        .map(|s| s.as_str().expect("string entry").to_string())
        .collect()
}

/// The committed file is exactly what the canonical matches generate.
#[test]
fn committed_fixture_matches_the_canonical_matches() {
    let fixture = committed_fixture();
    assert_eq!(fixture["version"], 1);
    assert_eq!(fixture["prefix"], "subnet:");

    assert_eq!(
        strings(&fixture, "auth_kinds"),
        subnet_auth_kinds(),
        "core reason codes drifted — regenerate the fixture and update every consumer",
    );
    assert_eq!(
        strings(&fixture, "local_kinds"),
        LOCAL_PROVISION_KINDS,
        "local provision kinds drifted — regenerate the fixture and update every consumer",
    );
    assert_eq!(
        strings(&fixture, "fact_kinds"),
        [
            SubnetFactKind::Descriptor,
            SubnetFactKind::GatewayAdvertisement,
            SubnetFactKind::ExportPolicy,
            SubnetFactKind::RevocationFloor,
        ]
        .map(fact_kind_wire),
    );
    assert_eq!(strings(&fixture, "access"), ["sameOrg", "granted"]);
}

/// The committed file is BYTE-IDENTICAL to what the generator emits
/// (review-10 P3-1).
///
/// The field-by-field assertions above compare selected keys, so an
/// unexpected extra field, a reordering, or formatting drift could pass
/// them while the committed file no longer matched the generator's
/// output — which is exactly what the "exact regeneration gate" claim
/// promised was impossible. This makes it a byte equality, with no need
/// for CI to shell out to the generator: both sides call the same
/// renderer.
#[test]
fn committed_fixture_is_byte_identical_to_the_generator_output() {
    let rendered = net_sdk::subnet::render_stable_kind_fixture();
    let committed = committed_bytes();
    assert_eq!(
        committed,
        rendered,
        "{} has drifted from the generator — re-run \
         `cargo run -p net-mesh-sdk --features net --example gen_subnet_kind_fixture` \
         and commit the result",
        fixture_path().display(),
    );
}

/// Rendering twice produces the same bytes — the determinism the
/// generator's doc claims, asserted rather than assumed. A map with
/// nondeterministic iteration order slipping into the fixture would
/// make the byte gate above flap instead of failing honestly.
#[test]
fn rendering_is_deterministic() {
    assert_eq!(
        net_sdk::subnet::render_stable_kind_fixture(),
        net_sdk::subnet::render_stable_kind_fixture(),
    );
}

/// No token appears twice across the auth and local lists — a
/// duplicated kind would make a binding's classification ambiguous.
#[test]
fn kinds_are_globally_unique() {
    let fixture = committed_fixture();
    let mut seen = std::collections::BTreeSet::new();
    for kind in strings(&fixture, "auth_kinds")
        .into_iter()
        .chain(strings(&fixture, "local_kinds"))
    {
        assert!(seen.insert(kind.clone()), "duplicate stable kind: {kind}");
    }
}
