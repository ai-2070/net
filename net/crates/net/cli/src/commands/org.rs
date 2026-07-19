//! `net org (keygen|issue-cert|issue-floors|grant-dispatcher|
//! grant-capability)` — organization root authority authoring (OA-1
//! belonging + OA-2 grant issuance, `ORG_CAPABILITY_AUTH_PLAN.md`).
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
    CapabilityAuthorityId, DispatcherScope, GrantRights, GrantTargetScope, OrgCapabilityGrant,
    OrgDispatcherGrant, OrgId, OrgKeypair, OrgMembershipCert, OrgRevocationBundle,
    ORG_CERT_TTL_SECS_RECOMMENDED,
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
    /// Issue a capability grant: "org <grantee> holds <rights> on
    /// <capability> over <target>", signed by THIS (provider) org.
    /// B -> A cross-org access. With --discover a fresh audience
    /// secret is minted and written 0600; only its commitment rides
    /// in the signed grant (the raw key never touches the wire).
    GrantCapability(GrantCapabilityArgs),
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

#[derive(Args, Debug)]
pub struct GrantCapabilityArgs {
    /// Path to the org root key file for the ISSUING (provider) org B
    /// — the grant is signed by THIS org.
    #[arg(long = "org-key", value_name = "PATH")]
    pub org_key: PathBuf,

    /// The grantee org id (OrgId of org A, 64 hex chars, optional
    /// `0x`).
    #[arg(long = "grantee-org")]
    pub grantee_org: String,

    /// The capability tag being granted, e.g. `nrpc:my-service`.
    #[arg(long)]
    pub capability: String,

    /// Grant INVOKE rights (call the capability).
    #[arg(long)]
    pub invoke: bool,

    /// Grant DISCOVER rights (privately find B-owned providers of the
    /// capability). Requires `--audience-out`: a fresh audience
    /// secret is minted and written 0600.
    #[arg(long)]
    pub discover: bool,

    /// Target an EXACT provider node by entity id (64 hex chars).
    /// Mutually exclusive with `--target-any-owned-by`.
    #[arg(long = "target-node")]
    pub target_node: Option<String>,

    /// Target ANY node owned by this org id (64 hex chars). Mutually
    /// exclusive with `--target-node`.
    #[arg(long = "target-any-owned-by")]
    pub target_any_owned_by: Option<String>,

    /// Grant TTL in seconds. Defaults to 7 days; hard-capped at 30
    /// days (rejected at issue AND at every verifier).
    #[arg(long = "ttl-secs", default_value_t = GRANT_TTL_SECS_DEFAULT)]
    pub ttl_secs: u64,

    /// Output path for the grant JSON.
    #[arg(long)]
    pub out: PathBuf,

    /// Output path for the minted audience secret (written 0600).
    /// REQUIRED with `--discover`; rejected without it.
    #[arg(long = "audience-out", value_name = "PATH")]
    pub audience_out: Option<PathBuf>,

    /// Overwrite existing output files. Refuses by default.
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
        OrgCommand::GrantCapability(args) => run_grant_capability(args, output).await,
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
    let mut toml_text = toml::to_string_pretty(&file)
        .map_err(|e| generic(format!("failed to serialize org key TOML: {e}")))?;

    // Same atomic, mode-restricted publish as operator identities —
    // the org root seed must never be world-readable, even
    // transiently.
    let pid = std::process::id();
    let tmp = path.with_extension(format!("tmp.{pid}"));
    write_identity_atomically(&tmp, &path, toml_text.as_bytes()).await?;
    enforce_strict_permissions(&path).await?;
    // Scrub the serialized seed buffer once published (Kyra OA2-F P1); `file`
    // scrubs its own `seed_hex` on `Drop`.
    zeroize_string(&mut toml_text);

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
// grant-capability
// =========================================================================

async fn run_grant_capability(
    args: GrantCapabilityArgs,
    output: Option<OutputFormat>,
) -> Result<(), CliError> {
    let issuer = load_org_key(&args.org_key, args.insecure_permissions).await?;
    let grantee_org = parse_org_hex(&args.grantee_org)?;
    let capability = CapabilityAuthorityId::for_tag(&args.capability);

    // Rights: at least one of --invoke / --discover.
    let rights = match (args.invoke, args.discover) {
        (false, false) => {
            return Err(invalid_args(
                "at least one of --invoke or --discover is required",
            ))
        }
        (true, false) => GrantRights::INVOKE,
        (false, true) => GrantRights::DISCOVER,
        (true, true) => GrantRights::INVOKE.union(GrantRights::DISCOVER),
    };
    let mut rights_labels = Vec::new();
    if args.invoke {
        rights_labels.push("invoke");
    }
    if args.discover {
        rights_labels.push("discover");
    }

    // Audience-secret discipline: an audience file is minted iff
    // --discover, so require --audience-out exactly then.
    match (args.discover, &args.audience_out) {
        (true, None) => return Err(invalid_args(
            "--discover requires --audience-out <PATH> (where to write the minted audience secret)",
        )),
        (false, Some(_)) => {
            return Err(invalid_args(
                "--audience-out is only valid with --discover (no secret is minted otherwise)",
            ))
        }
        _ => {}
    }

    // Target: exactly one of --target-node / --target-any-owned-by.
    let (target_scope, target_label) = match (&args.target_node, &args.target_any_owned_by) {
        (Some(entity_hex), None) => {
            let entity = parse_entity_hex(entity_hex)?;
            let label = format!("node:{}", hex::encode(entity.as_bytes()));
            (GrantTargetScope::ExactNode(entity), label)
        }
        (None, Some(org_hex)) => {
            let org = parse_org_hex(org_hex)?;
            let label = format!("any-owned-by:{}", hex::encode(org.as_bytes()));
            (GrantTargetScope::AnyNodeOwnedBy(org), label)
        }
        (Some(_), Some(_)) => {
            return Err(invalid_args(
                "--target-node and --target-any-owned-by are mutually exclusive",
            ))
        }
        (None, None) => {
            return Err(invalid_args(
                "one of --target-node <entity> or --target-any-owned-by <org> is required",
            ))
        }
    };

    let (grant, secret) = OrgCapabilityGrant::try_issue(
        &issuer,
        grantee_org,
        capability,
        rights,
        target_scope,
        args.ttl_secs,
    )
    .map_err(|e| invalid_args(format!("grant-capability: {e}")))?;

    // Refuse existing outputs BEFORE writing either, so a partial run
    // never leaves a grant without its secret (or the reverse).
    refuse_existing(&args.out, args.force).await?;
    if let Some(audience_out) = &args.audience_out {
        refuse_existing(audience_out, args.force).await?;
    }

    // Write the minted audience secret 0600 (present iff --discover;
    // mirrors the OA-1 owner-audience.key publish). The raw discovery
    // key lives ONLY in this file — never in the grant, never on the
    // wire.
    let audience_out_label = match secret {
        Some(secret) => {
            let audience_out = args
                .audience_out
                .as_ref()
                .expect("--discover requires --audience-out (validated above)");
            write_secret_file(audience_out, &secret.encode_config()).await?;
            Some(audience_out.display().to_string())
        }
        None => None,
    };

    write_json_envelope(
        &args.out,
        &OrgCapabilityGrantFile {
            version: ORG_FILE_VERSION,
            grant: grant.clone(),
        },
    )
    .await?;

    let summary = GrantCapabilityOutput {
        path: args.out.display().to_string(),
        audience_out: audience_out_label,
        grant_id_hex: hex::encode(grant.grant_id),
        issuer_org_hex: hex::encode(grant.issuer_org.as_bytes()),
        grantee_org_hex: hex::encode(grant.grantee_org.as_bytes()),
        capability: args.capability.clone(),
        rights: rights_labels.join(","),
        target: target_label,
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

#[derive(Serialize, Deserialize)]
struct OrgKeyFile {
    org_id_hex: String,
    seed_hex: String,
    created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

impl Drop for OrgKeyFile {
    fn drop(&mut self) {
        // The org root seed rides in `seed_hex`; scrub it on drop so a lingering
        // copy isn't left in freed memory (Kyra OA2-F P1). No `Debug` derive:
        // this struct must never render the seed into a log line.
        zeroize_string(&mut self.seed_hex);
    }
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

/// Versioned JSON envelope for a capability grant (the grant renders
/// as hex of its canonical 318-byte wire form). The signed grant
/// carries only the audience-key COMMITMENT — never the raw key.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OrgCapabilityGrantFile {
    pub(crate) version: u32,
    pub(crate) grant: OrgCapabilityGrant,
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

#[derive(Debug, Serialize)]
struct GrantCapabilityOutput {
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    audience_out: Option<String>,
    grant_id_hex: String,
    issuer_org_hex: String,
    grantee_org_hex: String,
    capability: String,
    rights: String,
    target: String,
    not_before: u64,
    not_after: u64,
}

// =========================================================================
// Helpers
// =========================================================================

/// Best-effort in-place scrub (volatile writes prevent optimizer elision —
/// matching the core crate's convention, no `zeroize` dependency).
fn zeroize_slice(buf: &mut [u8]) {
    for byte in buf.iter_mut() {
        // SAFETY: `byte` is a valid mutable reference for this iteration.
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
}

/// Scrub a `String`'s backing bytes in place (writing `0x00` keeps the buffer
/// valid UTF-8).
fn zeroize_string(s: &mut String) {
    // SAFETY: overwriting with 0 bytes preserves the UTF-8 invariant.
    let bytes = unsafe { s.as_mut_vec() };
    zeroize_slice(bytes);
}

/// Load + parse an org key file, honoring the ssh-style permission
/// gate (the seed is root-of-trust material for the whole org).
async fn load_org_key(path: &Path, insecure_permissions: bool) -> Result<OrgKeypair, CliError> {
    if !insecure_permissions {
        check_strict_permissions(path).await?;
    }
    // The file text carries the raw seed. Scrub it on EVERY exit.
    let mut text = tokio::fs::read_to_string(path).await.map_err(|e| {
        generic(format!(
            "failed to read org key file {}: {e}",
            path.display()
        ))
    })?;
    let outcome = load_org_key_from_text(&text, path);
    zeroize_string(&mut text);
    outcome
}

/// Parse + validate the (secret-bearing) org key text. Kept separate so the
/// caller can scrub the source text unconditionally on return.
fn load_org_key_from_text(text: &str, path: &Path) -> Result<OrgKeypair, CliError> {
    // NEVER interpolate the `toml::de::Error`: its `Display` embeds the
    // offending source LINE, which for this file is the secret `seed_hex`
    // (Kyra OA2-F P1 — a malformed seed line was reproduced verbatim in
    // stderr). Report a sanitized category only. `parsed` scrubs `seed_hex`
    // on its own `Drop` at the end of this function.
    let parsed: OrgKeyFile = toml::from_str(text).map_err(|_| {
        invalid_args(format!(
            "org key file {} is not valid TOML (kind: parse_error)",
            path.display()
        ))
    })?;
    // Decode into a scrubbed buffer; on error, report the category, never the
    // offending value.
    let mut seed_bytes = hex::decode(parsed.seed_hex.as_bytes()).map_err(|_| {
        invalid_args(format!(
            "org key file {} seed_hex is not valid hex (kind: bad_seed_encoding)",
            path.display()
        ))
    })?;
    let result = (|| {
        if seed_bytes.len() != 32 {
            return Err(invalid_args(format!(
                "org key file {} seed must be 32 bytes (64 hex chars), got {} (kind: \
                 bad_seed_length)",
                path.display(),
                seed_bytes.len()
            )));
        }
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&seed_bytes);
        let keypair = OrgKeypair::from_bytes(seed);
        zeroize_slice(&mut seed);
        // Consistency check: a hand-edited org_id_hex that disagrees with the
        // seed would otherwise sign as one org while claiming another.
        let derived = hex::encode(keypair.org_id().as_bytes());
        if !parsed.org_id_hex.eq_ignore_ascii_case(&derived) {
            return Err(invalid_args(format!(
                "org key file {}: org_id_hex does not match the key derived from seed_hex",
                path.display()
            )));
        }
        Ok(keypair)
    })();
    zeroize_slice(&mut seed_bytes);
    result
}

/// Parse a 32-byte org id from hex (optional `0x` prefix).
fn parse_org_hex(s: &str) -> Result<OrgId, CliError> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    let bytes =
        hex::decode(s).map_err(|e| invalid_args(format!("org id is not valid hex: {e}")))?;
    let arr: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
        invalid_args(format!(
            "org id must be 32 bytes (64 hex chars), got {}",
            bytes.len()
        ))
    })?;
    Ok(OrgId::from_bytes(arr))
}

/// Atomically publish a SECRET file at mode 0600 (same discipline as
/// the org root key and the OA-1 `owner-audience.key`) — the raw
/// audience key must never be world-readable, even transiently.
async fn write_secret_file(path: &Path, bytes: &[u8]) -> Result<(), CliError> {
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
    let pid = std::process::id();
    let tmp = path.with_extension(format!("tmp.{pid}"));
    write_identity_atomically(&tmp, path, bytes).await?;
    enforce_strict_permissions(path).await?;
    Ok(())
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
