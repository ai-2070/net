//! `net org (keygen|issue-cert|issue-floors|grant-dispatcher)` —
//! organization root authority authoring (OA-1 belonging + OA-2 grant
//! issuance, `ORG_CAPABILITY_AUTH_PLAN.md`).
//!
//! The org root key is OFFLINE key material: it lives on an
//! operator machine, signs membership certificates and
//! revocation-floor bundles through these verbs, and never touches
//! a mesh node. Key files are TOML at
//! `$XDG_CONFIG_HOME/net-mesh/orgs/` by default, mode 0600 with
//! the same ssh-style permission gate as operator identities:
//!
//! ```toml
//! org_id_hex = "..."                   # 64 hex chars (ed25519 vk)
//! seed_hex   = "..."                   # 64 hex chars (32-byte seed)
//! created_at = "2026-07-16T12:34:56Z"
//! note       = "Payments-fleet owner org"
//! ```
//!
//! Certificates and bundles are NOT secrets (they're signed public
//! statements) and are written as versioned JSON envelopes that
//! `net node adopt` and config management consume.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use net_sdk::org::{
    CapabilityAuthorityId, DispatcherScope, OrgDispatcherGrant, OrgKeypair, OrgMembershipCert,
    OrgRevocationBundle, ORG_CERT_TTL_SECS_RECOMMENDED,
};
use serde::{Deserialize, Serialize};

use crate::commands::identity::{
    check_strict_permissions, enforce_strict_permissions, now_iso8601, parse_entity_hex,
    write_identity_atomically,
};
use crate::error::{generic, invalid_args, CliError};
use crate::prelude::{emit_value, OutputFormat};

/// Format version of the cert / floors / grant JSON envelopes.
pub(crate) const ORG_FILE_VERSION: u32 = 1;

/// Default dispatcher/capability grant TTL — 7 days. Grant lifetimes are
/// days–weeks (renewal is re-issue + revoke in v1); the primitive hard-caps at
/// 30 days (`MAX_ORG_GRANT_TTL_SECS`), rejected at issue AND at every verifier.
const GRANT_TTL_SECS_DEFAULT: u64 = 7 * 24 * 60 * 60;

#[derive(Subcommand, Debug)]
pub enum OrgCommand {
    /// Generate a fresh org root keypair (offline key material).
    Keygen(KeygenArgs),
    /// Issue a membership certificate: "node <member> belongs to
    /// this org". Certificates prove belonging only — never
    /// invocation authority.
    IssueCert(IssueCertArgs),
    /// Issue a signed revocation-floor bundle: every certificate
    /// issued to a listed member below its floor generation is
    /// revoked. Nodes merge bundles monotonically — a lower floor
    /// never rolls back a higher one, including across restart.
    IssueFloors(IssueFloorsArgs),
    /// Issue a dispatcher grant: "entity <dispatcher> may act FOR
    /// this org" over a capability (or any). A -> S, org-root-signed;
    /// the caller carries it inside the org-admission proof. Holding
    /// one is never invocation authority — the provider still
    /// verifies the full per-call proof.
    GrantDispatcher(GrantDispatcherArgs),
}

#[derive(Args, Debug)]
pub struct KeygenArgs {
    /// Output path. Defaults to
    /// `$XDG_CONFIG_HOME/net-mesh/orgs/org-<id>.toml`.
    #[arg(long)]
    pub out: Option<PathBuf>,

    /// Free-form note saved alongside the key.
    #[arg(long)]
    pub note: Option<String>,

    /// Overwrite an existing file. Refuses by default.
    #[arg(long)]
    pub force: bool,
}

#[derive(Args, Debug)]
pub struct IssueCertArgs {
    /// Path to the org root key file (from `net org keygen`).
    #[arg(long = "org-key", value_name = "PATH")]
    pub org_key: PathBuf,

    /// The member node's entity id (32-byte ed25519 pubkey, 64 hex
    /// chars, optional `0x`).
    #[arg(long)]
    pub member: String,

    /// Revocation generation stamped into the certificate. Issue
    /// at a generation ≥ the org's current floor for this member;
    /// bump floors via `issue-floors` to retire outstanding certs.
    #[arg(long, default_value_t = 0)]
    pub generation: u32,

    /// Certificate TTL in seconds. Defaults to the recommended ~1
    /// year; hard-capped at 2 years (rejected at issue AND at
    /// every verifier).
    #[arg(long = "ttl-secs", default_value_t = ORG_CERT_TTL_SECS_RECOMMENDED)]
    pub ttl_secs: u64,

    /// Output path for the certificate JSON.
    #[arg(long)]
    pub out: PathBuf,

    /// Overwrite an existing file. Refuses by default.
    #[arg(long)]
    pub force: bool,

    /// Allow permissive org-key file modes on Unix. See
    /// `net identity show --insecure-permissions`.
    #[arg(long)]
    pub insecure_permissions: bool,
}

#[derive(Args, Debug)]
pub struct IssueFloorsArgs {
    /// Path to the org root key file (from `net org keygen`).
    #[arg(long = "org-key", value_name = "PATH")]
    pub org_key: PathBuf,

    /// A floor entry `<member-hex>=<generation>`; repeatable.
    /// Certificates for `<member>` with generation below the value
    /// are revoked on every node that merges this bundle.
    #[arg(long = "floor", value_name = "MEMBER=GEN", required = true)]
    pub floors: Vec<String>,

    /// Output path for the bundle JSON.
    #[arg(long)]
    pub out: PathBuf,

    /// Overwrite an existing file. Refuses by default.
    #[arg(long)]
    pub force: bool,

    /// Allow permissive org-key file modes on Unix.
    #[arg(long)]
    pub insecure_permissions: bool,
}

#[derive(Args, Debug)]
pub struct GrantDispatcherArgs {
    /// Path to the org root key file for the org the dispatcher acts
    /// FOR — the grant is signed by THIS org.
    #[arg(long = "org-key", value_name = "PATH")]
    pub org_key: PathBuf,

    /// The dispatcher's entity id (32-byte ed25519 pubkey, 64 hex
    /// chars, optional `0x`) empowered to act for the org.
    #[arg(long)]
    pub dispatcher: String,

    /// The capability tag the dispatcher may act on, e.g.
    /// `nrpc:my-service`. Mutually exclusive with `--any-capability`.
    #[arg(long)]
    pub capability: Option<String>,

    /// Grant the dispatcher EVERY capability (`DispatcherScope::Any`).
    /// Mutually exclusive with `--capability`.
    #[arg(long = "any-capability")]
    pub any_capability: bool,

    /// Grant TTL in seconds. Defaults to 7 days; hard-capped at 30
    /// days (rejected at issue AND at every verifier).
    #[arg(long = "ttl-secs", default_value_t = GRANT_TTL_SECS_DEFAULT)]
    pub ttl_secs: u64,

    /// Output path for the grant JSON.
    #[arg(long)]
    pub out: PathBuf,

    /// Overwrite an existing file. Refuses by default.
    #[arg(long)]
    pub force: bool,

    /// Allow permissive org-key file modes on Unix.
    #[arg(long)]
    pub insecure_permissions: bool,
}

pub async fn run(cmd: OrgCommand, output: Option<OutputFormat>) -> Result<(), CliError> {
    match cmd {
        OrgCommand::Keygen(args) => run_keygen(args, output).await,
        OrgCommand::IssueCert(args) => run_issue_cert(args, output).await,
        OrgCommand::IssueFloors(args) => run_issue_floors(args, output).await,
        OrgCommand::GrantDispatcher(args) => run_grant_dispatcher(args, output).await,
    }
}

// =========================================================================
// keygen
// =========================================================================

async fn run_keygen(args: KeygenArgs, output: Option<OutputFormat>) -> Result<(), CliError> {
    let keypair = OrgKeypair::generate();
    let org_id_hex = hex::encode(keypair.org_id().as_bytes());

    let path = args
        .out
        .unwrap_or_else(|| default_org_key_path(&org_id_hex));
    refuse_existing(&path, args.force).await?;

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| {
            generic(format!(
                "failed to create parent directory {}: {e}",
                parent.display()
            ))
        })?;
    }

    let file = OrgKeyFile {
        org_id_hex: org_id_hex.clone(),
        seed_hex: hex::encode(keypair.secret_bytes()),
        created_at: now_iso8601(),
        note: args.note.clone(),
    };
    let toml_text = toml::to_string_pretty(&file)
        .map_err(|e| generic(format!("failed to serialize org key TOML: {e}")))?;

    // Same atomic, mode-restricted publish as operator identities —
    // the org root seed must never be world-readable, even
    // transiently.
    let pid = std::process::id();
    let tmp = path.with_extension(format!("tmp.{pid}"));
    write_identity_atomically(&tmp, &path, toml_text.as_bytes()).await?;
    enforce_strict_permissions(&path).await?;

    // Public summary only — never the seed.
    let summary = OrgKeySummary {
        path: path.display().to_string(),
        org_id_hex,
        created_at: file.created_at.clone(),
        note: file.note.clone(),
    };
    emit_value(OutputFormat::resolve_oneshot(output), &summary)
        .map_err(|e| generic(format!("write summary: {e}")))?;
    Ok(())
}

// =========================================================================
// issue-cert
// =========================================================================

async fn run_issue_cert(args: IssueCertArgs, output: Option<OutputFormat>) -> Result<(), CliError> {
    let keypair = load_org_key(&args.org_key, args.insecure_permissions).await?;
    let member = parse_entity_hex(&args.member)?;

    let cert =
        OrgMembershipCert::try_issue(&keypair, member.clone(), args.generation, args.ttl_secs)
            .map_err(|e| invalid_args(format!("issue-cert: {e}")))?;

    refuse_existing(&args.out, args.force).await?;
    write_json_envelope(
        &args.out,
        &OrgCertFile {
            version: ORG_FILE_VERSION,
            cert: cert.clone(),
        },
    )
    .await?;

    let summary = IssueCertOutput {
        path: args.out.display().to_string(),
        org_id_hex: hex::encode(cert.org_id.as_bytes()),
        member_hex: hex::encode(member.as_bytes()),
        generation: cert.generation,
        not_before: cert.not_before,
        not_after: cert.not_after,
    };
    emit_value(OutputFormat::resolve_oneshot(output), &summary)
        .map_err(|e| generic(format!("write summary: {e}")))?;
    Ok(())
}

// =========================================================================
// issue-floors
// =========================================================================

async fn run_issue_floors(
    args: IssueFloorsArgs,
    output: Option<OutputFormat>,
) -> Result<(), CliError> {
    let keypair = load_org_key(&args.org_key, args.insecure_permissions).await?;

    let mut floors = BTreeMap::new();
    for raw in &args.floors {
        let (member_raw, gen_raw) = raw.split_once('=').ok_or_else(|| {
            invalid_args(format!("--floor '{raw}' must be <member-hex>=<generation>"))
        })?;
        let member = parse_entity_hex(member_raw)?;
        let generation: u32 = gen_raw
            .parse()
            .map_err(|e| invalid_args(format!("--floor '{raw}': generation must be a u32: {e}")))?;
        // Duplicate members: highest wins, silently merging two
        // entries would hide an operator typo — refuse instead.
        if floors.insert(member, generation).is_some() {
            return Err(invalid_args(format!(
                "--floor lists member {member_raw} more than once"
            )));
        }
    }

    let bundle = OrgRevocationBundle::try_issue(&keypair, &floors)
        .map_err(|e| invalid_args(format!("issue-floors: {e}")))?;

    refuse_existing(&args.out, args.force).await?;
    write_json_envelope(
        &args.out,
        &OrgFloorsFile {
            version: ORG_FILE_VERSION,
            bundle: bundle.clone(),
        },
    )
    .await?;

    let summary = IssueFloorsOutput {
        path: args.out.display().to_string(),
        org_id_hex: hex::encode(bundle.org_id.as_bytes()),
        floors: bundle.floors().len(),
        issued_at: bundle.issued_at,
    };
    emit_value(OutputFormat::resolve_oneshot(output), &summary)
        .map_err(|e| generic(format!("write summary: {e}")))?;
    Ok(())
}

// =========================================================================
// grant-dispatcher
// =========================================================================

async fn run_grant_dispatcher(
    args: GrantDispatcherArgs,
    output: Option<OutputFormat>,
) -> Result<(), CliError> {
    let keypair = load_org_key(&args.org_key, args.insecure_permissions).await?;
    let dispatcher = parse_entity_hex(&args.dispatcher)?;

    // Exactly one of --capability / --any-capability.
    let (scope, capability_label) = match (&args.capability, args.any_capability) {
        (Some(tag), false) => (
            DispatcherScope::Exact(CapabilityAuthorityId::for_tag(tag)),
            tag.clone(),
        ),
        (None, true) => (DispatcherScope::Any, "any".to_string()),
        (Some(_), true) => {
            return Err(invalid_args(
                "--capability and --any-capability are mutually exclusive",
            ))
        }
        (None, false) => {
            return Err(invalid_args(
                "one of --capability <tag> or --any-capability is required",
            ))
        }
    };

    let grant = OrgDispatcherGrant::try_issue(&keypair, dispatcher.clone(), scope, args.ttl_secs)
        .map_err(|e| invalid_args(format!("grant-dispatcher: {e}")))?;

    refuse_existing(&args.out, args.force).await?;
    write_json_envelope(
        &args.out,
        &OrgDispatcherGrantFile {
            version: ORG_FILE_VERSION,
            grant: grant.clone(),
        },
    )
    .await?;

    let summary = GrantDispatcherOutput {
        path: args.out.display().to_string(),
        org_id_hex: hex::encode(grant.org_id.as_bytes()),
        dispatcher_hex: hex::encode(dispatcher.as_bytes()),
        capability: capability_label,
        not_before: grant.not_before,
        not_after: grant.not_after,
    };
    emit_value(OutputFormat::resolve_oneshot(output), &summary)
        .map_err(|e| generic(format!("write summary: {e}")))?;
    Ok(())
}

// =========================================================================
// Disk shapes
// =========================================================================

#[derive(Debug, Serialize, Deserialize)]
struct OrgKeyFile {
    org_id_hex: String,
    seed_hex: String,
    created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

/// Versioned JSON envelope for a membership certificate (the cert
/// itself renders as hex of its canonical 156-byte wire form).
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OrgCertFile {
    pub(crate) version: u32,
    pub(crate) cert: OrgMembershipCert,
}

/// Versioned JSON envelope for a revocation-floor bundle.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OrgFloorsFile {
    pub(crate) version: u32,
    pub(crate) bundle: OrgRevocationBundle,
}

/// Versioned JSON envelope for a dispatcher grant (the grant renders
/// as hex of its canonical 185-byte wire form).
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OrgDispatcherGrantFile {
    pub(crate) version: u32,
    pub(crate) grant: OrgDispatcherGrant,
}

#[derive(Debug, Serialize)]
struct OrgKeySummary {
    path: String,
    org_id_hex: String,
    created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

#[derive(Debug, Serialize)]
struct IssueCertOutput {
    path: String,
    org_id_hex: String,
    member_hex: String,
    generation: u32,
    not_before: u64,
    not_after: u64,
}

#[derive(Debug, Serialize)]
struct IssueFloorsOutput {
    path: String,
    org_id_hex: String,
    floors: usize,
    issued_at: u64,
}

#[derive(Debug, Serialize)]
struct GrantDispatcherOutput {
    path: String,
    org_id_hex: String,
    dispatcher_hex: String,
    capability: String,
    not_before: u64,
    not_after: u64,
}

// =========================================================================
// Helpers
// =========================================================================

/// Load + parse an org key file, honoring the ssh-style permission
/// gate (the seed is root-of-trust material for the whole org).
async fn load_org_key(path: &Path, insecure_permissions: bool) -> Result<OrgKeypair, CliError> {
    if !insecure_permissions {
        check_strict_permissions(path).await?;
    }
    let text = tokio::fs::read_to_string(path).await.map_err(|e| {
        generic(format!(
            "failed to read org key file {}: {e}",
            path.display()
        ))
    })?;
    let parsed: OrgKeyFile = toml::from_str(&text).map_err(|e| {
        invalid_args(format!(
            "org key file {} failed to parse: {e}",
            path.display()
        ))
    })?;
    let seed = hex::decode(&parsed.seed_hex)
        .map_err(|e| invalid_args(format!("org key seed_hex is not valid hex: {e}")))?;
    let seed: [u8; 32] = seed.as_slice().try_into().map_err(|_| {
        invalid_args(format!(
            "org key seed must be 32 bytes (64 hex chars), got {}",
            seed.len()
        ))
    })?;
    let keypair = OrgKeypair::from_bytes(seed);
    // Consistency check: a hand-edited org_id_hex that disagrees
    // with the seed would otherwise sign as one org while claiming
    // another.
    let derived = hex::encode(keypair.org_id().as_bytes());
    if !parsed.org_id_hex.eq_ignore_ascii_case(&derived) {
        return Err(invalid_args(format!(
            "org key file {}: org_id_hex does not match the key derived from seed_hex",
            path.display()
        )));
    }
    Ok(keypair)
}

async fn refuse_existing(path: &Path, force: bool) -> Result<(), CliError> {
    if force {
        return Ok(());
    }
    match tokio::fs::try_exists(path).await {
        Ok(true) => Err(invalid_args(format!(
            "file already exists at {}; pass --force to overwrite",
            path.display()
        ))),
        Ok(false) => Ok(()),
        Err(e) => Err(generic(format!(
            "failed to stat {}: {e}; pass --force to override",
            path.display()
        ))),
    }
}

async fn write_json_envelope<T: Serialize>(path: &Path, value: &T) -> Result<(), CliError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                generic(format!(
                    "failed to create parent directory {}: {e}",
                    parent.display()
                ))
            })?;
        }
    }
    let json = serde_json::to_vec_pretty(value)
        .map_err(|e| generic(format!("failed to serialize {}: {e}", path.display())))?;
    tokio::fs::write(path, json)
        .await
        .map_err(|e| generic(format!("failed to write {}: {e}", path.display())))
}

fn default_org_key_path(org_id_hex: &str) -> PathBuf {
    let short = &org_id_hex[..org_id_hex.len().min(16)];
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("net-mesh")
        .join("orgs")
        .join(format!("org-{short}.toml"))
}
