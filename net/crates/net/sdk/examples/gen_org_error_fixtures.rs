//! OSDK-L X1 — generate `tests/cross_lang_org/error_vectors.json`.
//!
//! The `org:` error vocabulary is the contract four language bindings parse.
//! The fixture is derived from the ONE Rust source of that vocabulary
//! (`OrgSdkError::to_wire` and the per-domain `wire_kind` mappers), so it can
//! never drift from the code by being hand-edited:
//!
//! ```text
//! cargo run -p net-mesh-sdk --features net,cortex --example gen_org_error_fixtures
//! ```
//!
//! The drift guard in `src/org/tests_fixture.rs` asserts the checked-in file
//! still matches what this emits, so adding or renaming a kind fails CI until
//! the fixture is regenerated — and then fails every binding's suite until each
//! is updated. That chain is the point: a rename cannot be silent in any
//! language.

fn main() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("tests")
        .join("cross_lang_org")
        .join("error_vectors.json");
    std::fs::create_dir_all(path.parent().expect("parent")).expect("create fixture dir");
    std::fs::write(&path, net_sdk::org::render_error_vectors()).expect("write fixture");
    println!("wrote {}", path.display());
}
