//! OSDK-L X1 — the cross-language error-vocabulary fixture.
//!
//! `#[doc(hidden)]`: this is build/test tooling, not part of the facade's five
//! concepts. It is `pub` only so `examples/gen_org_error_fixtures.rs` and the
//! drift guard can both reach it.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::error::{
    OrgCredentialError, OrgDiscoveryError, OrgErrorDomain, OrgSdkError, ERR_ORG_PREFIX,
};
use super::types::{
    CapabilityAuthorityId, CoarseAdmissionReason, DispatcherScope, GrantAudienceInstallError,
    GrantRights, GrantTargetScope, NodeAuthority, OrgCapabilityGrant, OrgDispatcherGrant,
    OrgKeypair, OrgMembershipCert,
};
use net::adapter::net::identity::EntityKeypair;

/// One representative error per kind, so every token in the vocabulary appears
/// in the fixture with its real rendering.
fn samples() -> Vec<OrgSdkError> {
    let bad_ttl = super::types::OrgMembershipCert::try_issue(
        &super::types::OrgKeypair::from_bytes([0x11u8; 32]),
        net::adapter::net::identity::EntityKeypair::from_bytes([0x22u8; 32])
            .entity_id()
            .clone(),
        1,
        u64::MAX,
    )
    .expect_err("ttl ceiling yields a canonical OrgError");

    let entity = |b: u8| {
        net::adapter::net::identity::EntityKeypair::from_bytes([b; 32])
            .entity_id()
            .clone()
    };
    let org = |b: u8| super::types::OrgKeypair::from_bytes([b; 32]).org_id();
    let hex = "ab".repeat(32);

    vec![
        // ---- credentials (local; nothing was sent) ----
        OrgCredentialError::PersistentIdentityRequired.into(),
        OrgCredentialError::NodeAuthorityRequired.into(),
        OrgCredentialError::NodeAuthorityOrgMismatch {
            authority_org: org(0x31),
            membership_org: org(0x32),
        }
        .into(),
        OrgCredentialError::MemberBindingMismatch {
            expected: entity(0x41),
            credential: entity(0x42),
        }
        .into(),
        OrgCredentialError::SignatureInvalid {
            credential: "membership".to_string(),
            source: bad_ttl,
        }
        .into(),
        OrgCredentialError::DispatcherBindingMismatch {
            dispatcher: entity(0x51),
            member: entity(0x52),
        }
        .into(),
        OrgCredentialError::ActingOrgMismatch {
            membership_org: org(0x61),
            dispatcher_org: org(0x62),
        }
        .into(),
        OrgCredentialError::GrantNotForActingOrg {
            grant_id: hex.clone(),
            grantee_org: org(0x71),
        }
        .into(),
        OrgCredentialError::DuplicateGrant {
            grant_id: hex.clone(),
        }
        .into(),
        OrgCredentialError::AudienceSecretMismatch {
            grant_id: hex.clone(),
        }
        .into(),
        OrgCredentialError::AudienceInstallRefused {
            grant_id: hex.clone(),
            source: GrantAudienceInstallError::GrantNotCurrent,
        }
        .into(),
        OrgCredentialError::AudienceSecretFile {
            path: "/etc/net/grants/example.audience".to_string(),
            detail: "audience secret is not a regular file".to_string(),
        }
        .into(),
        OrgCredentialError::NotCurrentlyValid {
            credential: "membership".to_string(),
            source: super::types::OrgError::InvalidFormat,
        }
        .into(),
        OrgCredentialError::DispatcherScopeExcludesCapability {
            capability: hex.clone(),
        }
        .into(),
        OrgCredentialError::MissingCapabilityGrant {
            capability: hex.clone(),
        }
        .into(),
        OrgCredentialError::AmbiguousCapabilityGrant {
            capability: hex.clone(),
            grant_ids: vec![hex.clone(), "cd".repeat(32)],
        }
        .into(),
        // ---- discovery (local; nothing was sent) ----
        OrgDiscoveryError::NoAuthorizedProvider {
            capability: hex.clone(),
            considered: 3,
        }
        .into(),
        OrgDiscoveryError::ProviderNotDirect {
            provider: entity(0x81),
        }
        .into(),
        // ---- admission denied (REMOTE) ----
        OrgSdkError::AdmissionDenied(CoarseAdmissionReason::Denied),
        OrgSdkError::AdmissionDenied(CoarseAdmissionReason::NotSupported),
        OrgSdkError::AdmissionDenied(CoarseAdmissionReason::Unavailable),
        // ---- rpc (transport / non-admission server failure) ----
        OrgSdkError::Rpc(net::adapter::net::mesh_rpc::RpcError::Timeout { elapsed_ms: 5000 }),
        OrgSdkError::Rpc(net::adapter::net::mesh_rpc::RpcError::NoRoute {
            target: 0xDEAD,
            reason: "no path".to_string(),
        }),
        OrgSdkError::Rpc(net::adapter::net::mesh_rpc::RpcError::Cancelled),
    ]
}

/// Render the canonical `error_vectors.json` content.
///
/// Lives in the library so the generator example and the drift guard share ONE
/// implementation — a fixture whose generator and checker could disagree would
/// guard nothing.
pub fn render_error_vectors() -> String {
    let mut out = String::new();
    out.push_str("{\n");
    out.push_str(
        "  \"description\": \"OSDK-L X1 — the canonical `org:` error vocabulary every language \
         binding parses. Each vector's `wire` string is what Rust emits; a binding MUST recover \
         `domain` and `kind` from it, and MUST use `is_local` to decide whether the request left \
         the process. GENERATED — do not hand-edit; run `cargo run -p net-mesh-sdk --features \
         net,cortex --example gen_org_error_fixtures`.\",\n",
    );
    out.push_str(&format!(
        "  \"version\": 1,\n  \"prefix\": {ERR_ORG_PREFIX:?},\n"
    ));

    // The domain vocabulary, with the load-bearing local/remote split.
    out.push_str("  \"domains\": [\n");
    let domains = [
        OrgErrorDomain::Credentials,
        OrgErrorDomain::Discovery,
        OrgErrorDomain::AdmissionDenied,
        OrgErrorDomain::Rpc,
        OrgErrorDomain::Unclassified,
    ];
    for (i, d) in domains.iter().enumerate() {
        out.push_str(&format!(
            "    {{ \"token\": {:?}, \"is_local\": {} }}{}\n",
            d.as_wire(),
            d.is_local(),
            if i + 1 == domains.len() { "" } else { "," }
        ));
    }
    out.push_str("  ],\n");

    out.push_str("  \"vectors\": [\n");
    let samples = samples();
    for (i, e) in samples.iter().enumerate() {
        let domain = e.domain();
        out.push_str(&format!(
            "    {{ \"wire\": {:?}, \"domain\": {:?}, \"kind\": {:?}, \"is_local\": {} }}{}\n",
            e.to_wire(),
            domain.as_wire(),
            e.wire_kind(),
            domain.is_local(),
            if i + 1 == samples.len() { "" } else { "," }
        ));
    }
    out.push_str("  ],\n");

    // The row Kyra's §D5a exists for: a binding meeting a domain this build
    // does not define must classify it as `unknown` — NEVER as one of the four,
    // because claiming `admission_denied` would assert a remote evaluation that
    // may never have happened.
    out.push_str(
        "  \"unclassified_cases\": [\n\
         \x20   { \"wire\": \"org:frobnicate:whatever: detail\", \"expect_domain\": \"unknown\", \
         \"expect_is_local\": false },\n\
         \x20   { \"wire\": \"org:\", \"expect_domain\": \"unknown\", \"expect_is_local\": false },\n\
         \x20   { \"wire\": \"org:credentials\", \"expect_domain\": \"unknown\", \
         \"expect_is_local\": false },\n\
         \x20   { \"wire\": \"not-an-org-error\", \"expect_domain\": \"unknown\", \
         \"expect_is_local\": false }\n\
         \x20 ],\n",
    );
    out.push_str(
        "  \"notes\": [\n\
         \x20   \"A binding MUST NOT report `admission_denied` for a string it could not parse: \
         that asserts a request reached a provider and its admission engine evaluated it.\",\n\
         \x20   \"`admission_denied` vectors carry the coarse bucket and NOTHING else — a precise \
         remote reason would be a credential oracle (OA2-E2).\",\n\
         \x20   \"`org:rpc:` reuses the frozen nRPC kind vocabulary rather than minting second \
         names for the same conditions.\"\n\
         \x20 ]\n",
    );
    out.push_str("}\n");
    out
}

// ===========================================================================
// OSDK-L X2 — the live cross-language call scenario.
//
// A deterministic, on-disk issuance chain a provider in one language serves and
// a caller in another invokes, over a real mesh. The manifest is the contract
// every language harness loads. Generation is fully SYNCHRONOUS — it mints
// crypto artifacts and writes files; it never builds a mesh.
// ===========================================================================

/// The pre-shared key every node in the scenario builds with (32 bytes).
pub const SCENARIO_PSK: [u8; 32] = [0x51u8; 32];
/// The provider node's 32-byte identity seed. Its entity is
/// `EntityKeypair::from_bytes(PROVIDER_SEED)`, which is exactly what a language
/// binding's `NewMeshNode(seed)` reconstructs — so the provider's adopted
/// authority (issued for that entity) installs cleanly in every language.
pub const PROVIDER_SEED: [u8; 32] = [0x11u8; 32];
/// The caller node's 32-byte identity seed (see [`PROVIDER_SEED`]).
pub const CALLER_SEED: [u8; 32] = [0x22u8; 32];
/// The provider's owner org (org B) root key seed.
pub const PROVIDER_ORG_SEED: [u8; 32] = [0xB2u8; 32];
/// The caller's org (org A) root key seed.
pub const CALLER_ORG_SEED: [u8; 32] = [0xA1u8; 32];
/// The Granted (cross-org) service the provider serves and the caller calls.
pub const GRANTED_SERVICE: &str = "customer.read";
/// The capability tag the granted service derives (`nrpc:<service>`).
pub const GRANTED_CAPABILITY_TAG: &str = "nrpc:customer.read";
/// Validity window for every issued cert/grant. The scenario is fresh per run,
/// so a bounded TTL is fine (and matches the substrate's ceilings).
pub const SCENARIO_TTL_SECS: u64 = 3600;

/// The provider role's inputs — what a provider harness loads to serve.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioProvider {
    /// 32-byte identity seed, hex — build the node with this.
    pub seed_hex: String,
    /// The provider's owner org id, hex (for assertions).
    pub org_id_hex: String,
    /// Adopted node-authority directory (relative to the manifest), to install.
    pub authority_dir: String,
    /// The capability-grant wire bytes to install as a provider grant audience.
    pub grant_path: String,
    /// The grant's audience-secret file (0600) — installed by PATH, never bytes.
    pub grant_secret_path: String,
}

/// The caller role's inputs — what a caller harness loads to call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioCaller {
    /// 32-byte identity seed, hex — build the node with this.
    pub seed_hex: String,
    /// The caller's acting org id, hex (for assertions).
    pub org_id_hex: String,
    /// Adopted node-authority directory (relative to the manifest), to install.
    pub authority_dir: String,
    /// The membership-cert wire bytes for the caller's credentials.
    pub membership_path: String,
    /// The dispatcher-grant wire bytes for the caller's credentials.
    pub dispatcher_path: String,
    /// The capability-grant wire bytes for the caller's credentials.
    pub grant_path: String,
    /// The grant's audience-secret file (0600) — supplied by PATH to `from_parts`.
    pub grant_secret_path: String,
}

/// The cross-org scenario contract. Paths are relative to the manifest's
/// directory. Generated fresh per run; do not commit an instance (the certs
/// expire after [`SCENARIO_TTL_SECS`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossOrgScenarioManifest {
    /// Schema version.
    pub version: u32,
    /// Human note (the invariant + how to regenerate).
    pub description: String,
    /// The mesh PSK, hex.
    pub psk_hex: String,
    /// The Granted service name the provider serves and the caller calls.
    pub granted_service: String,
    /// The capability tag that service derives.
    pub granted_capability_tag: String,
    /// Provider role inputs.
    pub provider: ScenarioProvider,
    /// Caller role inputs.
    pub caller: ScenarioCaller,
}

fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn io_err<E: std::fmt::Display>(e: E) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

/// Write a secret file the way a binding must supply one: exact bytes, and
/// owner-only (0600 on Unix; on Windows the loader gates on the file's own
/// protected DACL, which a freshly created file inherits acceptably — the same
/// path the cross-platform from-files test uses).
fn write_secret_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::File::create(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Mint the full cross-org issuance chain into `outdir` and return (and write)
/// its manifest — the single implementation the `gen_org_scenario` example and
/// the Rust `live_cross_org_call_from_a_generated_scenario` test both use, so a
/// scenario whose generator and consumer could disagree cannot exist.
///
/// The shape: org A owns the caller node; org B owns the provider node; B
/// issues A a DISCOVER|INVOKE grant over `nrpc:customer.read` on any B-owned
/// node. The provider installs that grant's audience and serves `Granted`; the
/// caller loads membership + dispatcher + grant + the audience-secret PATH and
/// calls. Identities are seeded so any language's `NewMeshNode(seed)`
/// reconstructs the exact entity the certs were issued for.
pub fn write_cross_org_scenario(outdir: &Path) -> std::io::Result<CrossOrgScenarioManifest> {
    let org_a = OrgKeypair::from_bytes(CALLER_ORG_SEED); // caller's org
    let org_b = OrgKeypair::from_bytes(PROVIDER_ORG_SEED); // provider's org
    let capability = CapabilityAuthorityId::for_tag(GRANTED_CAPABILITY_TAG);

    let provider_entity = EntityKeypair::from_bytes(PROVIDER_SEED).entity_id().clone();
    let caller_entity = EntityKeypair::from_bytes(CALLER_SEED).entity_id().clone();

    let provider_dir = outdir.join("provider");
    let caller_dir = outdir.join("caller");
    let provider_auth = provider_dir.join("authority");
    let caller_auth = caller_dir.join("authority");
    // Adoption refuses to overwrite; start clean so the generator is rerunnable.
    let _ = std::fs::remove_dir_all(&provider_auth);
    let _ = std::fs::remove_dir_all(&caller_auth);
    std::fs::create_dir_all(&provider_dir)?;
    std::fs::create_dir_all(&caller_dir)?;

    // Adopted authorities — the `net node adopt` ceremony, written to disk.
    let provider_cert =
        OrgMembershipCert::try_issue(&org_b, provider_entity.clone(), 1, SCENARIO_TTL_SECS)
            .map_err(io_err)?;
    NodeAuthority::adopt(&provider_auth, provider_cert, &provider_entity, 0, None)
        .map_err(io_err)?;
    let caller_auth_cert =
        OrgMembershipCert::try_issue(&org_a, caller_entity.clone(), 1, SCENARIO_TTL_SECS)
            .map_err(io_err)?;
    NodeAuthority::adopt(&caller_auth, caller_auth_cert, &caller_entity, 0, None)
        .map_err(io_err)?;

    // The caller's credentials — membership + a wide-open dispatcher grant.
    let caller_membership =
        OrgMembershipCert::try_issue(&org_a, caller_entity.clone(), 1, SCENARIO_TTL_SECS)
            .map_err(io_err)?;
    let caller_dispatcher = OrgDispatcherGrant::try_issue(
        &org_a,
        caller_entity.clone(),
        DispatcherScope::Any,
        SCENARIO_TTL_SECS,
    )
    .map_err(io_err)?;

    // B → A: DISCOVER|INVOKE over the capability on any B-owned node, with the
    // discovery audience secret the provider installs and the caller holds.
    let (grant, secret) = OrgCapabilityGrant::try_issue(
        &org_b,
        org_a.org_id(),
        capability,
        GrantRights::INVOKE.union(GrantRights::DISCOVER),
        GrantTargetScope::AnyNodeOwnedBy(org_b.org_id()),
        SCENARIO_TTL_SECS,
    )
    .map_err(io_err)?;
    let secret = secret.ok_or_else(|| io_err("DISCOVER grant must mint an audience secret"))?;
    let grant_bytes = grant.to_bytes();
    let secret_bytes = secret.encode_config();

    // Caller credential files.
    std::fs::write(
        caller_dir.join("membership.bin"),
        caller_membership.to_bytes(),
    )?;
    std::fs::write(
        caller_dir.join("dispatcher.bin"),
        caller_dispatcher.to_bytes(),
    )?;
    std::fs::write(caller_dir.join("grant.bin"), &grant_bytes)?;
    write_secret_file(&caller_dir.join("grant.audience"), &secret_bytes)?;

    // Provider install files (the same grant + secret it announces under).
    std::fs::write(provider_dir.join("grant.bin"), &grant_bytes)?;
    write_secret_file(&provider_dir.join("grant.audience"), &secret_bytes)?;

    let manifest = CrossOrgScenarioManifest {
        version: 1,
        description: "OSDK-L X2 — a live cross-org scenario: org B's provider \
                      serves a Granted capability that org A's caller invokes. \
                      GENERATED fresh per run (certs expire after \
                      SCENARIO_TTL_SECS) — do not commit. Regenerate via `cargo \
                      run -p net-mesh-sdk --features net,cortex --example \
                      gen_org_scenario -- <outdir>`."
            .to_string(),
        psk_hex: to_hex(&SCENARIO_PSK),
        granted_service: GRANTED_SERVICE.to_string(),
        granted_capability_tag: GRANTED_CAPABILITY_TAG.to_string(),
        provider: ScenarioProvider {
            seed_hex: to_hex(&PROVIDER_SEED),
            org_id_hex: to_hex(org_b.org_id().as_bytes()),
            authority_dir: "provider/authority".to_string(),
            grant_path: "provider/grant.bin".to_string(),
            grant_secret_path: "provider/grant.audience".to_string(),
        },
        caller: ScenarioCaller {
            seed_hex: to_hex(&CALLER_SEED),
            org_id_hex: to_hex(org_a.org_id().as_bytes()),
            authority_dir: "caller/authority".to_string(),
            membership_path: "caller/membership.bin".to_string(),
            dispatcher_path: "caller/dispatcher.bin".to_string(),
            grant_path: "caller/grant.bin".to_string(),
            grant_secret_path: "caller/grant.audience".to_string(),
        },
    };
    let json = serde_json::to_string_pretty(&manifest).map_err(io_err)?;
    std::fs::write(outdir.join("manifest.json"), json)?;
    Ok(manifest)
}
