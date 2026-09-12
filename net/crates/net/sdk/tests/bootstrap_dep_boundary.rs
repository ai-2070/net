//! Stage 4b: **a default build has no HTTP server.**
//!
//! The `rtc-bootstrap` feature pulls axum, hyper, rustls and an ACME
//! client. None of that may reach a consumer who did not ask for a
//! bootstrap listener — the same boundary discipline
//! `adapters/mcp/tests/dependency_boundary.rs` applies to the MCP
//! adapter, and the same one CI's `net-mesh-wire` step applies to
//! the wire crate.
//!
//! This reads the manifests rather than the resolved graph on
//! purpose: a `cargo tree` assertion is a CI step (it needs a
//! resolver run per feature set), while the invariant *this* test
//! holds is the one a reviewer can break by hand in a manifest —
//! declaring an HTTP dependency non-optional, or forgetting to gate
//! it behind the feature.

use std::collections::BTreeSet;

/// Crates that must never be reachable from a default build of the
/// SDK or of the core.
const HTTP_STACK: &[&str] = &[
    "axum",
    "hyper",
    "hyper-util",
    "tower",
    "tower-http",
    "rustls",
    "tokio-rustls",
    "rustls-pemfile",
    "instant-acme",
    "rcgen",
];

fn manifest(text: &str) -> toml::Value {
    toml::from_str(text).expect("the manifest parses")
}

/// Every HTTP-stack dependency the SDK declares is `optional`, and
/// every one is named by the `rtc-bootstrap` feature — so the ONLY
/// way to get axum is to ask for the listener.
#[test]
fn the_http_stack_is_optional_and_reachable_only_from_rtc_bootstrap() {
    let sdk = manifest(include_str!("../Cargo.toml"));
    let deps = sdk["dependencies"]
        .as_table()
        .expect("[dependencies] is a table");
    let feature_list: BTreeSet<String> = sdk["features"]["rtc-bootstrap"]
        .as_array()
        .expect("the rtc-bootstrap feature exists")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();

    for name in HTTP_STACK {
        let Some(dep) = deps.get(*name) else {
            continue;
        };
        assert_eq!(
            dep.get("optional").and_then(|v| v.as_bool()),
            Some(true),
            "{name} must be optional: a non-optional HTTP dependency is in every build"
        );
        assert!(
            feature_list.contains(&format!("dep:{name}")),
            "{name} is optional but no feature turns it on — dead weight, or worse, \
             enabled by something other than rtc-bootstrap"
        );
    }
    assert!(
        feature_list.contains("webrtc"),
        "the listener implies webrtc: an offer with no ICE stack to accept it is nothing"
    );
}

/// No default feature of the SDK enables the listener, directly or
/// through a meta-feature.
#[test]
fn no_default_feature_path_reaches_the_listener() {
    let sdk = manifest(include_str!("../Cargo.toml"));
    let features = sdk["features"].as_table().expect("[features]");
    let mut reachable: BTreeSet<String> = features["default"]
        .as_array()
        .expect("default")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    // Transitive closure over the feature graph.
    loop {
        let mut added = false;
        for name in reachable.clone() {
            if let Some(list) = features.get(&name).and_then(|v| v.as_array()) {
                for entry in list {
                    let entry = entry.as_str().unwrap().to_string();
                    if reachable.insert(entry) {
                        added = true;
                    }
                }
            }
        }
        if !added {
            break;
        }
    }
    assert!(
        !reachable.contains("rtc-bootstrap"),
        "a default SDK build must not enable the bootstrap listener"
    );
    for name in HTTP_STACK {
        assert!(
            !reachable.contains(&format!("dep:{name}")),
            "a default SDK build must not enable {name}"
        );
    }
}

/// The CORE keeps no HTTP dependency at all — the listener lives in
/// the SDK precisely so this stays true.
#[test]
fn the_core_declares_no_http_server_dependency() {
    let core = manifest(include_str!("../../Cargo.toml"));
    let deps = core["dependencies"]
        .as_table()
        .expect("[dependencies] is a table");
    for name in HTTP_STACK {
        assert!(
            !deps.contains_key(*name),
            "net-mesh must not depend on {name}: the bootstrap listener is the SDK's"
        );
    }
}
