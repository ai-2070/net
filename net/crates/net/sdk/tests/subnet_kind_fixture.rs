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

fn committed_fixture() -> serde_json::Value {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("sdk has a parent")
        .join("tests")
        .join("cross_lang_subnet")
        .join("stable_kinds.json");
    let body = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {} (run gen_subnet_kind_fixture): {e}", path.display()));
    serde_json::from_str(&body).expect("fixture parses")
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
