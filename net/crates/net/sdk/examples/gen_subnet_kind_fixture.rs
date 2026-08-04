//! Regenerate the cross-language subnet stable-kind fixture
//! (`SUBNET_AUTH_SDK_PLAN.md` R4/§7.3).
//!
//! ```text
//! cargo run -p net-mesh-sdk --features net --example gen_subnet_kind_fixture
//! ```
//!
//! Writes `net/crates/net/tests/cross_lang_subnet/stable_kinds.json` —
//! the ONE list of `subnet:<kind>` tokens, control-fact outcome kinds,
//! and access spellings, generated from the canonical Rust matches
//! (`net_sdk::subnet`). Rust, Node, Python, Go, and C consume this same
//! file to prove their classifications stay synchronized. This is a
//! local error-contract drift guard, not an interoperability matrix and
//! not a second source of names: renaming any kind regenerates a
//! different file, and every consumer suite fails until it catches up.
//!
//! Deterministic by construction — no timestamps, no randomness — so
//! CI can regenerate and diff.

use std::path::PathBuf;

fn main() {
    let auth_kinds = net_sdk::subnet::subnet_auth_kinds();
    let local_kinds = net_sdk::subnet::LOCAL_PROVISION_KINDS;
    let fact_kinds = [
        net_sdk::subnet::SubnetFactKind::Descriptor,
        net_sdk::subnet::SubnetFactKind::GatewayAdvertisement,
        net_sdk::subnet::SubnetFactKind::ExportPolicy,
        net_sdk::subnet::SubnetFactKind::RevocationFloor,
    ]
    .map(net_sdk::subnet::fact_kind_wire);

    let fixture = serde_json::json!({
        "version": 1,
        "prefix": "subnet:",
        "auth_kinds": auth_kinds,
        "local_kinds": local_kinds,
        "fact_kinds": fact_kinds,
        "access": ["sameOrg", "granted"],
    });

    // sdk/ → the core crate root, where the cross_lang_* fixtures live.
    let out_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("sdk has a parent")
        .join("tests")
        .join("cross_lang_subnet");
    std::fs::create_dir_all(&out_dir).expect("create cross_lang_subnet");
    let out = out_dir.join("stable_kinds.json");
    let mut body = serde_json::to_string_pretty(&fixture).expect("serialize fixture");
    body.push('\n');
    std::fs::write(&out, body).expect("write fixture");
    println!("wrote {}", out.display());
}
