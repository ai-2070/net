//! S4 — generate a live subnet-EXPORTED call scenario into a directory.
//!
//! ```text
//! cargo run -p net-mesh-sdk --features net,cortex,fixtures \
//!     --example gen_subnet_scenario -- <outdir>
//! ```
//!
//! Mints the whole chain an exported serve needs before it will ADMIT — a
//! subnet authority root, an `EXPORT` credential at the exact crossing, the
//! boundary declaration, the provider's adopted org authority, a same-org
//! caller's credentials, and a foreign-org caller's — then writes a
//! `manifest.json` every language's S4 live cell loads.
//!
//! The manifest is the contract. A provider harness builds its node from
//! `provider.seed_hex` with `subnet_authorities`, `provider.attachment`, and
//! the named export from `export_name` + `export_binding` + `export_access`;
//! installs `provider.authority_dir`, the gateway credentials at
//! `provider.gateway_credentials_path`, and the boundaries at
//! `provider.boundary_paths`; then serves `exported_service` against
//! `export_name`. A caller harness builds from `caller.seed_hex`, installs
//! `caller.authority_dir`, binds membership + dispatcher, and calls. The
//! `foreign_caller` role is the fail-closed leg: valid credentials, wrong
//! organization.
//!
//! GENERATED fresh per run — the credentials expire, so do not commit an
//! instance. The Rust `live_s4_cell_from_a_generated_scenario` test drives this
//! end to end; each binding's harness loads the same manifest on CI.

fn main() {
    let outdir = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: gen_subnet_scenario <outdir>");
        std::process::exit(2);
    });
    let outdir = std::path::PathBuf::from(outdir);
    std::fs::create_dir_all(&outdir).expect("create outdir");
    let manifest =
        net_sdk::subnet::fixtures::write_subnet_scenario(&outdir).expect("write subnet scenario");
    println!(
        "wrote {} (service {:?}, export {:?}, provider org {}, foreign org {})",
        outdir.join("manifest.json").display(),
        manifest.exported_service,
        manifest.export_name,
        manifest.provider.org_id_hex,
        manifest.foreign_caller.org_id_hex,
    );
}
