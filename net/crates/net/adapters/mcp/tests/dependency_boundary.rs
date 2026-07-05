//! Doctrine guard (`MCP_BRIDGE_PLAN.md` #1 / Open Risks): the MCP adapter
//! rides on `net-mesh-sdk` ONLY — it must never depend on the core `net-mesh`
//! crate *directly* (its only path to core is through the SDK). This test
//! parses the crate's own `Cargo.toml` and fails the build if a forbidden
//! direct dependency appears, so a violation is caught wherever the tests run.
//!
//! Also guards the P0 carve-out (`MCP_BRIDGE_SDK_PLAN.md`): the consent /
//! pin-store types the bridge re-exports must *be* the SDK's types, never a
//! bridge-local reimplementation — one lock implementation on one file, ever.

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

/// Compile-time identity proofs: each `const` compiles only when the bridge
/// path and the SDK path name the *same* type (`std::convert::identity` is
/// `fn(T) -> T`, so the two sides must unify). If a bridge-local
/// reimplementation of consent, pins, capability identity, or the credential
/// vocabulary ever replaced a re-export, this file would stop compiling —
/// the "one lock implementation" doctrine enforced by the type system rather
/// than review vigilance. (No `#[test]` needed; the assertions run at
/// compile time.)
const _PIN_STORE_IS_THE_SDK_TYPE: fn(net_mcp::serve::PinStore) -> net_sdk::pins::PinStore =
    std::convert::identity;
const _PIN_STATE_IS_THE_SDK_TYPE: fn(net_mcp::serve::PinState) -> net_sdk::pins::PinState =
    std::convert::identity;
const _PIN_ERROR_IS_THE_SDK_TYPE: fn(
    net_mcp::serve::PinStoreError,
) -> net_sdk::pins::PinStoreError = std::convert::identity;
const _CONSENT_POLICY_IS_THE_SDK_TYPE: fn(
    net_mcp::serve::ConsentPolicy,
) -> net_sdk::consent::ConsentPolicy = std::convert::identity;
const _CONSENT_DECISION_IS_THE_SDK_TYPE: fn(
    net_mcp::serve::ConsentDecision,
) -> net_sdk::consent::ConsentDecision = std::convert::identity;
const _CAPABILITY_ID_IS_THE_SDK_TYPE: fn(
    net_mcp::serve::CapabilityId,
) -> net_sdk::consent::CapabilityId = std::convert::identity;
const _CREDENTIAL_STATUS_IS_THE_SDK_TYPE: fn(
    net_mcp::wrap::CredentialStatus,
) -> net_sdk::consent::CredentialStatus = std::convert::identity;
