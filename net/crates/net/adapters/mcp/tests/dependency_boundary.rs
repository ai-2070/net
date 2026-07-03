//! Doctrine guard (`MCP_BRIDGE_PLAN.md` #1 / Open Risks): the MCP adapter
//! rides on `net-mesh-sdk` ONLY — it must never depend on the core `net-mesh`
//! crate *directly* (its only path to core is through the SDK). This test
//! parses the crate's own `Cargo.toml` and fails the build if a forbidden
//! direct dependency appears, so a violation is caught wherever the tests run.

/// The crate's manifest, embedded at compile time.
const MANIFEST: &str = include_str!("../Cargo.toml");

/// Package names that must never be a *direct* dependency (core crate + its
/// lib alias). Reaching core is the SDK's job.
const FORBIDDEN_CORE: &[&str] = &["net-mesh", "net"];

#[test]
fn adapter_depends_on_the_sdk_only_never_core_directly() {
    let manifest: toml::Value = toml::from_str(MANIFEST).expect("parse Cargo.toml");
    let deps = manifest
        .get("dependencies")
        .and_then(toml::Value::as_table)
        .expect("[dependencies] table");

    // The single mesh-facing dependency is the SDK.
    assert!(
        deps.contains_key("net-mesh-sdk"),
        "net-mesh-sdk must be the adapter's mesh-facing dependency",
    );

    for (name, spec) in deps {
        // A dependency named after the core crate/alias is forbidden.
        assert!(
            !FORBIDDEN_CORE.contains(&name.as_str()),
            "forbidden direct dependency on core crate `{name}` — reach core via net-mesh-sdk",
        );
        // An *aliased* dependency whose `package = "net-mesh"` is equally
        // forbidden (e.g. `foo = {{ package = \"net-mesh\", .. }}`).
        if let Some(pkg) = spec.get("package").and_then(toml::Value::as_str) {
            assert!(
                !FORBIDDEN_CORE.contains(&pkg),
                "dependency `{name}` aliases the forbidden core package `{pkg}`",
            );
        }
    }
}
