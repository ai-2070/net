//! S4 (Python) — generate a live SAME-ORGANIZATION call scenario into a
//! directory.
//!
//! The `gen_org_scenario` example (`net-mesh-sdk`) mints the CROSS-org chain;
//! this mints its same-org counterpart for the Python wheel's live witnesses
//! (§4.4: every shape, both call and serve, **same-org AND granted**): one
//! organization, a provider node and a separate caller node, membership +
//! dispatcher for the caller, and — the §3.4 out-of-band pre-staging step —
//! ONE per-organization owner audience written into BOTH adopted authority
//! directories (owner-scoped discovery is keyed on one audience per
//! organization; two independently adopted nodes each minting their own could
//! never open each other's envelopes).
//!
//! ```text
//! cargo run -p net-python --features org --example gen_org_same_scenario -- <outdir>
//! ```
//!
//! The manifest is the contract. A provider harness loads `provider.*` (build
//! the node from `seed_hex`, install `authority_dir`, serve with
//! `access="same_org"`); a caller harness loads `caller.*` (build from
//! `seed_hex`, install `authority_dir`, then
//! `OrgCredentials(membership, dispatcher, [], [])` — NO audience-secret
//! paths: the shared owner audience travels through the adopted authority,
//! exactly as the Rust `live_same_org_call_through_the_facade` fixture models
//! it).
//!
//! GENERATED fresh per run — the certs expire, so do not commit an instance.

use std::path::Path;

use net_sdk::org::{
    DispatcherScope, NodeAuthority, OrgDispatcherGrant, OrgKeypair, OrgMembershipCert,
};

/// The pre-shared key every node in the scenario builds with (32 bytes).
const SCENARIO_PSK: [u8; 32] = [0x52u8; 32];
/// The provider node's 32-byte identity seed (reconstructs the exact entity
/// the credentials name — the binding's `identity_seed`).
const PROVIDER_SEED: [u8; 32] = [0x33u8; 32];
/// The caller node's 32-byte identity seed.
const CALLER_SEED: [u8; 32] = [0x34u8; 32];
/// The shared organization root key seed.
const ORG_SEED: [u8; 32] = [0xA5u8; 32];
/// Validity window for every issued cert/grant.
const SCENARIO_TTL_SECS: u64 = 3600;

fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Write a secret file the way a binding must load one (the same discipline
/// `net_sdk::org::fixtures` uses): created owner-only (0600 on Unix), and on
/// Windows the loader gates on the file's own protected DACL, which a freshly
/// created file in a standard directory inherits acceptably. When the file
/// already exists (the authority dir's `owner-audience.key`, adopted earlier)
/// the write truncates in place, preserving the ceremony's mode/ACL.
fn write_secret_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}

/// The manifest, built as a `serde_json::Value` — this package depends on
/// `serde_json` only (no derive), and the manifest is one small literal.
fn manifest_json(
    psk_hex: &str,
    org_id_hex: &str,
    provider_seed_hex: &str,
    caller_seed_hex: &str,
) -> serde_json::Value {
    serde_json::json!({
        "version": 1,
        "description": "S4 (Python) — a live same-org scenario: one organization, a \
                        provider node and a caller node sharing the organization's ONE \
                        owner audience (pre-staged into both adopted authority dirs, \
                        §3.4). GENERATED fresh per run (certs expire after \
                        SCENARIO_TTL_SECS) — do not commit. Regenerate via `cargo run \
                        -p net-python --features org --example gen_org_same_scenario -- \
                        <outdir>`.",
        "psk_hex": psk_hex,
        "provider": {
            "seed_hex": provider_seed_hex,
            "org_id_hex": org_id_hex,
            "authority_dir": "provider/authority",
        },
        "caller": {
            "seed_hex": caller_seed_hex,
            "org_id_hex": org_id_hex,
            "authority_dir": "caller/authority",
            "membership_path": "caller/membership.bin",
            "dispatcher_path": "caller/dispatcher.bin",
        },
    })
}

fn main() {
    let outdir = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: gen_org_same_scenario <outdir>");
        std::process::exit(2);
    });
    let outdir = Path::new(&outdir);
    std::fs::create_dir_all(outdir).expect("create outdir");

    let org = OrgKeypair::from_bytes(ORG_SEED);
    let provider_entity = net::adapter::net::identity::EntityKeypair::from_bytes(PROVIDER_SEED)
        .entity_id()
        .clone();
    let caller_entity = net::adapter::net::identity::EntityKeypair::from_bytes(CALLER_SEED)
        .entity_id()
        .clone();

    let provider_dir = outdir.join("provider");
    let caller_dir = outdir.join("caller");
    let provider_auth = provider_dir.join("authority");
    let caller_auth = caller_dir.join("authority");
    // Adoption refuses to overwrite; start clean so the generator is rerunnable.
    let _ = std::fs::remove_dir_all(&provider_auth);
    let _ = std::fs::remove_dir_all(&caller_auth);
    std::fs::create_dir_all(&provider_dir).expect("provider dir");
    std::fs::create_dir_all(&caller_dir).expect("caller dir");

    // Adopted authorities — the `net node adopt` ceremony, written to disk and
    // loaded by `install_org_authority` (the production binding path).
    let provider_cert =
        OrgMembershipCert::try_issue(&org, provider_entity.clone(), 1, SCENARIO_TTL_SECS)
            .expect("provider membership");
    let provider_authority =
        NodeAuthority::adopt(&provider_auth, provider_cert, &provider_entity, 0, None)
            .expect("adopt provider");
    let caller_adopt_cert =
        OrgMembershipCert::try_issue(&org, caller_entity.clone(), 1, SCENARIO_TTL_SECS)
            .expect("caller adopt membership");
    NodeAuthority::adopt(&caller_auth, caller_adopt_cert, &caller_entity, 0, None)
        .expect("adopt caller");

    // §3.4's out-of-band pre-staging: ONE per-organization owner audience for
    // both nodes. The provider's adopted key is the organization's for this
    // scenario; write it over the caller's `owner-audience.key` (same org —
    // the loader's owner check passes) before any node installs the directory.
    // This is the on-disk form of what the Rust `fast_mesh(shared_audience)`
    // fixture does in memory.
    let shared_audience = provider_authority.audience.encode_config();
    write_secret_file(
        &caller_auth.join(net_sdk::org::OWNER_AUDIENCE_FILE),
        &shared_audience,
    )
    .expect("pre-stage the shared owner audience");
    // (The `encode_config` array above is the accepted by-value key residual —
    // same disclosure `write_cross_org_scenario` makes — it is scrubbed when
    // this short-lived generator process exits.)

    // The caller's call-time credentials — membership + a wide-open dispatcher
    // grant. No capability grants, no audience-secret files: same-org private
    // discovery rides the shared owner audience in the adopted authorities.
    let caller_membership =
        OrgMembershipCert::try_issue(&org, caller_entity.clone(), 1, SCENARIO_TTL_SECS)
            .expect("caller membership");
    let caller_dispatcher = OrgDispatcherGrant::try_issue(
        &org,
        caller_entity.clone(),
        DispatcherScope::Any,
        SCENARIO_TTL_SECS,
    )
    .expect("caller dispatcher");
    std::fs::write(
        caller_dir.join("membership.bin"),
        caller_membership.to_bytes(),
    )
    .expect("write membership");
    std::fs::write(
        caller_dir.join("dispatcher.bin"),
        caller_dispatcher.to_bytes(),
    )
    .expect("write dispatcher");

    let manifest = manifest_json(
        &to_hex(&SCENARIO_PSK),
        &to_hex(org.org_id().as_bytes()),
        &to_hex(&PROVIDER_SEED),
        &to_hex(&CALLER_SEED),
    );
    let json = serde_json::to_string_pretty(&manifest).expect("serialize manifest");
    std::fs::write(outdir.join("manifest.json"), json).expect("write manifest");
    println!(
        "wrote {} (provider org {}, shared owner audience pre-staged)",
        outdir.join("manifest.json").display(),
        to_hex(org.org_id().as_bytes()),
    );
}
