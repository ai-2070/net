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
//! This binary is a THIN writer: the bytes come from
//! `net_sdk::subnet::render_stable_kind_fixture`, which the fixture test
//! byte-compares the committed file against (review-10 P3-1). Running
//! this is therefore never required to detect drift — it is how you fix
//! it.
//!
//! Deterministic by construction — no timestamps, no randomness.

use std::path::PathBuf;

fn main() {
    // sdk/ → the core crate root, where the cross_lang_* fixtures live.
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("sdk has a parent")
        .to_path_buf();
    let out = root.join(net_sdk::subnet::STABLE_KINDS_FIXTURE_PATH);
    std::fs::create_dir_all(out.parent().expect("fixture has a parent"))
        .expect("create cross_lang_subnet");
    std::fs::write(&out, net_sdk::subnet::render_stable_kind_fixture()).expect("write fixture");
    println!("wrote {}", out.display());
}
