//! S4 — generate a live subnet-EXPORTED call scenario into a directory.
//!
//! The subnet analogue of [`crate::org::fixtures::write_cross_org_scenario`],
//! and the contract every language's S4 live cell loads. One generator, one
//! manifest, five consumers — a scenario whose producer and consumers could
//! disagree cannot exist.
//!
//! # What a provider needs before an exported serve will admit
//!
//! An exported service is not "a public service with an org check". Dispatch
//! revalidates the exact crossing against the node's LIVE gateway authority on
//! every call, before organization admission, so the generator has to mint the
//! whole chain:
//!
//! ```text
//! subnet authority root S          signs the provider's transport credential
//!   └─ EXPORT grant, scope [3,9]   the exact crossing, epoch 0
//! boundary declaration [3,9]       the edge this node protects
//! org A membership + adoption      the provider's owner organization
//! named export "factory-export"    the provider-local label, bound to [3,9]@0
//! ```
//!
//! Miss any one and the serve registers but every call is refused — which is
//! precisely why the live cells exist rather than a registration smoke test.
//!
//! # The three roles
//!
//! | Role | Org | Expected outcome |
//! |---|---|---|
//! | provider | A | serves the named export |
//! | caller | A | admitted — same organization as the provider |
//! | foreign caller | B | refused — a different org holding no grant |
//!
//! The foreign caller is what makes the fail-closed leg cross-boundary rather
//! than merely malformed: its credentials are internally valid and correctly
//! signed, by the wrong organization. A suite that only tested a corrupt
//! credential would prove the decoder works, not that the boundary holds.
//!
//! GENERATED fresh per run — the certs expire, so do not commit an instance.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::org::types::{
    DispatcherScope, NodeAuthority, OrgDispatcherGrant, OrgKeypair, OrgMembershipCert,
};
use net::adapter::net::identity::EntityKeypair;
use net::adapter::net::subnet::{SubnetCredentialSet, SubnetGrant, SubnetRights, TopologySubnetId};

// ---------------------------------------------------------------------------
// The frozen scenario inputs. Seeded, so any language's constructor
// reconstructs the exact entity the credentials were issued for.
// ---------------------------------------------------------------------------

/// Mesh PSK for every node in the scenario.
pub const SUBNET_SCENARIO_PSK: [u8; 32] = [0x53u8; 32];
/// The provider node's identity seed.
pub const SUBNET_PROVIDER_SEED: [u8; 32] = [0x31u8; 32];
/// The same-org caller's identity seed.
pub const SUBNET_CALLER_SEED: [u8; 32] = [0x32u8; 32];
/// The foreign-org caller's identity seed.
pub const SUBNET_FOREIGN_CALLER_SEED: [u8; 32] = [0x33u8; 32];
/// The organization that owns the provider AND the admitted caller.
pub const SUBNET_ORG_SEED: [u8; 32] = [0xA3u8; 32];
/// A DIFFERENT organization, owning the caller that must be refused.
pub const SUBNET_FOREIGN_ORG_SEED: [u8; 32] = [0xB3u8; 32];
/// The subnet AUTHORITY root — signs the provider's EXPORT credential.
pub const SUBNET_AUTHORITY_SEED: [u8; 32] = [0xC3u8; 32];

/// The exported service name.
pub const EXPORTED_SERVICE: &str = "fleet.telemetry";
/// The capability tag that service derives.
pub const EXPORTED_CAPABILITY_TAG: &str = "nrpc:fleet.telemetry";
/// The provider-local export label. Never announced, never accepted from a
/// caller.
pub const EXPORT_NAME: &str = "factory-export";
/// A label deliberately NOT configured — the unknown-name inverse every suite
/// asserts BEFORE it serves anything.
pub const UNKNOWN_EXPORT_NAME: &str = "no-such-export";

/// The provider's own security attachment point.
pub const PROVIDER_ATTACHMENT: &[u8] = &[3];
/// The exported crossing: the exact authority-qualified path the provider
/// holds `EXPORT` for, and the export binding names.
pub const EXPORTED_CROSSING: &[u8] = &[3, 9];
/// The topology epoch everything is minted under.
pub const TOPOLOGY_EPOCH: u32 = 0;

/// Credential lifetime. Long enough for a CI run, short enough that a
/// committed instance is obviously stale.
pub const SUBNET_SCENARIO_TTL_SECS: u64 = 3600;
/// The per-authority grant-lifetime ceiling the trust anchor declares.
pub const MAXIMUM_GRANT_LIFETIME_SECS: u64 = 7 * 24 * 60 * 60;

// ---------------------------------------------------------------------------
// The manifest — the contract
// ---------------------------------------------------------------------------

/// This node's subnet AUTHORITY trust anchor, in the shape every constructor
/// takes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioSubnetAuthority {
    /// 32-byte authority entity id, 64 hex chars.
    pub authority_hex: String,
    /// Root entity ids permitted to sign for that authority.
    pub root_hexes: Vec<String>,
    /// Per-authority grant-lifetime ceiling.
    pub maximum_grant_lifetime_secs: u64,
}

/// The exported crossing and the epoch it was declared under.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioExportBinding {
    /// The authority the crossing is qualified by.
    pub authority_hex: String,
    /// Path levels, outermost first.
    pub path: Vec<u8>,
    /// The epoch the binding was declared under.
    pub topology_epoch: u32,
}

/// Provider role inputs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioSubnetProvider {
    /// Identity seed, hex. Reconstructs the exact entity the credentials name.
    pub seed_hex: String,
    /// The provider entity id, hex — what an admitted handler reports as
    /// `provider`.
    pub entity_id_hex: String,
    /// The provider's owner organization id, hex.
    pub org_id_hex: String,
    /// Adopted org authority directory, relative to the manifest.
    pub authority_dir: String,
    /// This node's own security attachment path.
    pub attachment: Vec<u8>,
    /// Framed `SubnetCredentialSet` wire bytes carrying the EXPORT right at
    /// the exported crossing. Install WHOLESALE.
    pub gateway_credentials_path: String,
    /// The boundary inventory to declare: subtree roots whose edge this node
    /// protects.
    pub boundary_paths: Vec<Vec<u8>>,
}

/// Caller role inputs — used for both the admitted and the refused caller.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioSubnetCaller {
    /// Identity seed, hex.
    pub seed_hex: String,
    /// The caller entity id, hex — what an admitted handler reports as
    /// `caller`.
    pub entity_id_hex: String,
    /// The organization this caller acts for, hex.
    pub org_id_hex: String,
    /// Adopted org authority directory, relative to the manifest.
    pub authority_dir: String,
    /// Membership certificate bytes.
    pub membership_path: String,
    /// Dispatcher grant bytes.
    pub dispatcher_path: String,
}

/// The subnet-exported scenario contract. Paths are relative to the
/// manifest's directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubnetScenarioManifest {
    /// Schema version.
    pub version: u32,
    /// Human note (the invariant + how to regenerate).
    pub description: String,
    /// The mesh PSK, hex.
    pub psk_hex: String,
    /// The service the provider exports and both callers invoke.
    pub exported_service: String,
    /// The capability tag that service derives.
    pub exported_capability_tag: String,
    /// The provider-local export label to serve against.
    pub export_name: String,
    /// A label that is NOT configured — assert the local refusal with it.
    pub unknown_export_name: String,
    /// Trust anchors for the provider's constructor.
    pub subnet_authorities: Vec<ScenarioSubnetAuthority>,
    /// The named export's binding.
    pub export_binding: ScenarioExportBinding,
    /// `"sameOrg"` — the admitted caller shares the provider's organization.
    pub export_access: String,
    /// Provider role.
    pub provider: ScenarioSubnetProvider,
    /// The caller that must be ADMITTED (same org as the provider).
    pub caller: ScenarioSubnetCaller,
    /// The caller that must be REFUSED (a different org, holding no grant).
    pub foreign_caller: ScenarioSubnetCaller,
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

fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Adopt an org authority and write the caller-side credentials beside it.
fn write_caller_role(
    dir: &Path,
    rel: &str,
    org: &OrgKeypair,
    seed: [u8; 32],
) -> std::io::Result<ScenarioSubnetCaller> {
    let entity = EntityKeypair::from_bytes(seed).entity_id().clone();
    let auth_dir = dir.join("authority");
    // Adoption refuses to overwrite; start clean so the generator is rerunnable.
    let _ = std::fs::remove_dir_all(&auth_dir);
    std::fs::create_dir_all(dir)?;

    let adopt_cert = OrgMembershipCert::try_issue(org, entity.clone(), 1, SUBNET_SCENARIO_TTL_SECS)
        .map_err(io_err)?;
    NodeAuthority::adopt(&auth_dir, adopt_cert, &entity, 0, None).map_err(io_err)?;

    let membership = OrgMembershipCert::try_issue(org, entity.clone(), 1, SUBNET_SCENARIO_TTL_SECS)
        .map_err(io_err)?;
    let dispatcher = OrgDispatcherGrant::try_issue(
        org,
        entity.clone(),
        DispatcherScope::Any,
        SUBNET_SCENARIO_TTL_SECS,
    )
    .map_err(io_err)?;
    std::fs::write(dir.join("membership.bin"), membership.to_bytes())?;
    std::fs::write(dir.join("dispatcher.bin"), dispatcher.to_bytes())?;

    Ok(ScenarioSubnetCaller {
        seed_hex: to_hex(&seed),
        entity_id_hex: to_hex(entity.as_bytes()),
        org_id_hex: to_hex(org.org_id().as_bytes()),
        authority_dir: format!("{rel}/authority"),
        membership_path: format!("{rel}/membership.bin"),
        dispatcher_path: format!("{rel}/dispatcher.bin"),
    })
}

/// Mint the full subnet-exported issuance chain into `outdir` and return (and
/// write) its manifest.
///
/// The single implementation the `gen_subnet_scenario` example and every
/// language's S4 live cell consume.
pub fn write_subnet_scenario(outdir: &Path) -> std::io::Result<SubnetScenarioManifest> {
    let org = OrgKeypair::from_bytes(SUBNET_ORG_SEED);
    let foreign_org = OrgKeypair::from_bytes(SUBNET_FOREIGN_ORG_SEED);
    let subnet_root = EntityKeypair::from_bytes(SUBNET_AUTHORITY_SEED);
    let authority_id = subnet_root.entity_id().clone();

    let provider_entity = EntityKeypair::from_bytes(SUBNET_PROVIDER_SEED)
        .entity_id()
        .clone();

    let provider_dir = outdir.join("provider");
    let provider_auth = provider_dir.join("authority");
    let _ = std::fs::remove_dir_all(&provider_auth);
    std::fs::create_dir_all(&provider_dir)?;

    // The provider's adopted org authority — the `net node adopt` ceremony.
    let provider_cert =
        OrgMembershipCert::try_issue(&org, provider_entity.clone(), 1, SUBNET_SCENARIO_TTL_SECS)
            .map_err(io_err)?;
    NodeAuthority::adopt(&provider_auth, provider_cert, &provider_entity, 0, None)
        .map_err(io_err)?;

    // The subnet transport credential: EXPORT at exactly the exported
    // crossing, under the authority root the provider will also trust.
    // `not_before` is backdated a minute so the credential is usable the
    // instant a consumer loads it, across small clock skew.
    let grant = SubnetGrant::try_issue(
        &subnet_root,
        authority_id.clone(),
        TopologySubnetId::new(EXPORTED_CROSSING),
        TOPOLOGY_EPOCH,
        provider_entity.clone(),
        SubnetRights::EXPORT,
        1,
        unix_now_secs().saturating_sub(60),
        SUBNET_SCENARIO_TTL_SECS,
    )
    .map_err(io_err)?;
    let credential_set = SubnetCredentialSet::Direct(grant);
    std::fs::write(
        provider_dir.join("gateway_credentials.bin"),
        credential_set.to_bytes(),
    )?;

    let caller = write_caller_role(&outdir.join("caller"), "caller", &org, SUBNET_CALLER_SEED)?;
    let foreign_caller = write_caller_role(
        &outdir.join("foreign_caller"),
        "foreign_caller",
        &foreign_org,
        SUBNET_FOREIGN_CALLER_SEED,
    )?;

    let manifest = SubnetScenarioManifest {
        version: 1,
        description: "SSDK S4 — a live subnet-exported scenario: a provider \
                      inside a protected subnet serves a NAMED export that a \
                      same-org caller invokes with organization authority \
                      only, and a foreign-org caller is refused. GENERATED \
                      fresh per run (credentials expire after \
                      SUBNET_SCENARIO_TTL_SECS) — do not commit. Regenerate \
                      via `cargo run -p net-mesh-sdk --features \
                      net,cortex,fixtures --example gen_subnet_scenario -- \
                      <outdir>`."
            .to_string(),
        psk_hex: to_hex(&SUBNET_SCENARIO_PSK),
        exported_service: EXPORTED_SERVICE.to_string(),
        exported_capability_tag: EXPORTED_CAPABILITY_TAG.to_string(),
        export_name: EXPORT_NAME.to_string(),
        unknown_export_name: UNKNOWN_EXPORT_NAME.to_string(),
        subnet_authorities: vec![ScenarioSubnetAuthority {
            authority_hex: to_hex(authority_id.as_bytes()),
            root_hexes: vec![to_hex(authority_id.as_bytes())],
            maximum_grant_lifetime_secs: MAXIMUM_GRANT_LIFETIME_SECS,
        }],
        export_binding: ScenarioExportBinding {
            authority_hex: to_hex(authority_id.as_bytes()),
            path: EXPORTED_CROSSING.to_vec(),
            topology_epoch: TOPOLOGY_EPOCH,
        },
        export_access: "sameOrg".to_string(),
        provider: ScenarioSubnetProvider {
            seed_hex: to_hex(&SUBNET_PROVIDER_SEED),
            entity_id_hex: to_hex(provider_entity.as_bytes()),
            org_id_hex: to_hex(org.org_id().as_bytes()),
            authority_dir: "provider/authority".to_string(),
            attachment: PROVIDER_ATTACHMENT.to_vec(),
            gateway_credentials_path: "provider/gateway_credentials.bin".to_string(),
            boundary_paths: vec![EXPORTED_CROSSING.to_vec()],
        },
        caller,
        foreign_caller,
    };
    let json = serde_json::to_string_pretty(&manifest).map_err(io_err)?;
    std::fs::write(outdir.join("manifest.json"), json)?;
    Ok(manifest)
}
