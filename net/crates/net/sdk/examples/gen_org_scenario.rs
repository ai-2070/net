//! OSDK-L X2 — generate a live cross-org call scenario into a directory.
//!
//! Mints a fresh issuance chain (org A's caller node, org B's provider node,
//! and a B→A DISCOVER|INVOKE grant over `nrpc:customer.read`) and writes the
//! adopted authorities, credential bytes, 0600 audience-secret files, and a
//! `manifest.json` a harness in any language loads to run the call:
//!
//! ```text
//! cargo run -p net-mesh-sdk --features net,cortex --example gen_org_scenario -- <outdir>
//! ```
//!
//! The manifest is the contract. A provider harness loads `provider.*` (build
//! the node from `seed_hex`, install `authority_dir`, install the grant audience
//! from `grant_path` + `grant_secret_path`, serve `granted_service` as Granted);
//! a caller harness loads `caller.*` (build from `seed_hex`, install
//! `authority_dir`, `from_parts(membership, dispatcher, [grant], [secret PATH])`,
//! then call). The audience secret crosses only as a path, never bytes.
//!
//! GENERATED fresh per run — the certs expire, so do not commit an instance.
//! The Rust `live_cross_org_call_from_a_generated_scenario` test drives this end
//! to end; each language binding's harness loads the same manifest on CI.

fn main() {
    let outdir = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: gen_org_scenario <outdir>");
        std::process::exit(2);
    });
    let outdir = std::path::PathBuf::from(outdir);
    std::fs::create_dir_all(&outdir).expect("create outdir");
    let manifest =
        net_sdk::org::write_cross_org_scenario(&outdir).expect("write cross-org scenario");
    println!(
        "wrote {} (service {:?}, provider org {}, caller org {})",
        outdir.join("manifest.json").display(),
        manifest.granted_service,
        manifest.provider.org_id_hex,
        manifest.caller.org_id_hex,
    );
}
