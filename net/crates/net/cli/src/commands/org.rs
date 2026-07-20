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
    /// Issue a membership certificate: "node `<member>` belongs to
    /// this org". Certificates prove belonging only — never
    /// invocation authority.
    IssueCert(IssueCertArgs),
    /// Issue a signed revocation-floor bundle: every certificate
    /// issued to a listed member below its floor generation is
    /// revoked. Nodes merge bundles monotonically — a lower floor
    /// never rolls back a higher one, including across restart.
    IssueFloors(IssueFloorsArgs),
    /// Issue a dispatcher grant: "entity `<dispatcher>` may act FOR
    /// this org" over a capability (or any). A -> S, org-root-signed;
    /// the caller carries it inside the org-admission proof. Holding
    /// one is never invocation authority — the provider still
    /// verifies the full per-call proof.
    GrantDispatcher(GrantDispatcherArgs),
    /// Issue a capability grant: "org `<grantee>` holds `<rights>` on
    /// `<capability>` over `<target>`", signed by THIS (provider) org.
    /// B -> A cross-org access. With --discover a fresh audience
    /// secret is minted and written owner-only (0600 on Unix; parent
    /// DACL on Windows); only its commitment rides in the signed
    /// grant (the raw key never touches the wire).
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

    /// Refused: grant artifacts are published no-clobber. A forced replace is
    /// not crash-atomic and, on a case-insensitive filesystem, an aliased
    /// `--out` (e.g. `ORG.TOML` vs `org.toml`) could destroy the org key. Write
    /// to a fresh path, or remove the old file explicitly.
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
    /// secret is minted and written owner-only (0600 on Unix; on
    /// Windows it inherits the parent directory's DACL and a warning
    /// is emitted — see `--insecure-permissions`).
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

    /// Output path for the minted audience secret. REQUIRED with
    /// `--discover`; rejected without it. Written owner-only: mode
    /// 0600 on Unix; on Windows it inherits the parent directory's
    /// NTFS DACL (a loud warning is emitted unless
    /// `--insecure-permissions`), so point it at an owner-only parent.
    #[arg(long = "audience-out", value_name = "PATH")]
    pub audience_out: Option<PathBuf>,

    /// Refused: grant artifacts are published no-clobber (the grant +
    /// audience-secret pair is not crash-atomic, and a forced replace could
    /// destroy a case-variant alias of the org key). Write to fresh output
    /// paths, or remove the old files explicitly.
    #[arg(long)]
    pub force: bool,

    /// Allow permissive org-key file modes on Unix, and silence the
    /// Windows audience-secret DACL warning.
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
    let toml_text = ScrubbedString::new(
        toml::to_string_pretty(&file)
            .map_err(|e| generic(format!("failed to serialize org key TOML: {e}")))?,
    );

    // Same atomic, mode-restricted publish as operator identities — the org root
    // seed must never be world-readable, even transiently. `toml_text` carries
    // the serialized seed and scrubs on EVERY exit via its Drop guard — including
    // a failed atomic write or permission-enforcement step, not only the success
    // tail (Kyra OA2-F). `file` scrubs its own `seed_hex` on Drop.
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

    // The org root key must never be a publication target — with `--force` the
    // old `refuse_existing` + `tokio::fs::write` pair would truncate it in
    // place, destroying the only key that can issue certs or revocation floors
    // for this org, with every outstanding cert left valid until natural expiry
    // (§2). The alias guard runs REGARDLESS of `--force`.
    refuse_aliased_paths(&[("--org-key", &args.org_key), ("--out", &args.out)])?;
    refuse_existing(&args.out, args.force).await?;
    let json = serialize_json(&OrgCertFile {
        version: ORG_FILE_VERSION,
        cert: cert.clone(),
    })?;
    publish_json_artifact(&args.out, &json, args.force).await?;

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

    // Same boundary as issue-cert: `--out` must never alias `--org-key`, with
    // or without `--force` (§2).
    refuse_aliased_paths(&[("--org-key", &args.org_key), ("--out", &args.out)])?;
    refuse_existing(&args.out, args.force).await?;
    let json = serialize_json(&OrgFloorsFile {
        version: ORG_FILE_VERSION,
        bundle: bundle.clone(),
    })?;
    publish_json_artifact(&args.out, &json, args.force).await?;

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
    refuse_force(args.force)?;
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

    // Staged no-clobber publish — never overwrite the org key, never follow /
    // truncate a leaf symlink (Kyra OA2-F). `--force` is refused (see
    // `refuse_force`): a remove-then-link replace is not crash-atomic.
    refuse_aliased_paths(&[("--org-key", &args.org_key), ("--out", &args.out)])?;
    let json = serialize_json(&OrgDispatcherGrantFile {
        version: ORG_FILE_VERSION,
        grant: grant.clone(),
    })?;
    let tmp = stage_beside(&args.out, &json, false).await?;
    publish_staged(&tmp, &args.out).await?;

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
    refuse_force(args.force)?;
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

    // Aliased paths would clobber the org key or collide the two output
    // artifacts (Kyra OA2-F).
    let mut alias_paths: Vec<(&str, &Path)> = vec![
        ("--org-key", args.org_key.as_path()),
        ("--out", args.out.as_path()),
    ];
    if let Some(audience_out) = &args.audience_out {
        alias_paths.push(("--audience-out", audience_out.as_path()));
    }
    refuse_aliased_paths(&alias_paths)?;

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

    // Publish as STAGED NO-CLOBBER with error rollback (Kyra OA2-F): stage both
    // artifacts in their destination dirs, then publish each no-clobber; if the
    // second publish fails, roll back the first so a partial run never leaves a
    // grant without its secret (or the reverse). This is NOT crash-atomic across
    // the two files (a crash between them can leave one), so `--force` is refused
    // (`refuse_force`) and operators write to fresh paths. Temps are cleaned up
    // synchronously on failure. The raw discovery key lives ONLY in the in-memory
    // scrub guard and the audience file — never in the grant, never on the wire.
    let grant_json = serialize_json(&OrgCapabilityGrantFile {
        version: ORG_FILE_VERSION,
        grant: grant.clone(),
    })?;
    let audience_out_label = match secret {
        Some(secret) => {
            let audience_out = args
                .audience_out
                .as_ref()
                .expect("--discover requires --audience-out (validated above)");
            // The guard scrubs this in-memory copy of the raw key on EVERY exit —
            // including the `?` on grant staging below (Kyra OA2-F). Scrub the
            // source array too so the `encode_config()` temporary isn't left in
            // freed stack memory.
            let mut raw = secret.encode_config();
            let secret_bytes = ScrubbedBytes::new(raw.to_vec());
            zeroize_slice(&mut raw);
            let grant_tmp = stage_beside(&args.out, &grant_json, false).await?;
            let secret_tmp = match stage_beside(audience_out, secret_bytes.as_slice(), true).await {
                Ok(t) => t,
                Err(e) => {
                    remove_file_or_warn(&grant_tmp, "staging temp").await;
                    return Err(e);
                }
            };
            // The secret is persisted to its staged file now; drop the in-memory
            // copy (scrubs).
            drop(secret_bytes);
            // Grant first (no-clobber).
            if let Err(e) = publish_staged(&grant_tmp, &args.out).await {
                remove_file_or_warn(
                    &secret_tmp,
                    "staging temp (holds a copy of the audience secret)",
                )
                .await;
                return Err(e);
            }
            // Then the secret; roll back the grant if it fails so we never leave
            // a grant without its matching secret.
            if let Err(e) = publish_staged(&secret_tmp, audience_out).await {
                remove_file_or_warn(
                    &args.out,
                    "grant (rollback: its audience secret failed to publish)",
                )
                .await;
                return Err(e);
            }
            warn_secret_permissions(audience_out, args.insecure_permissions);
            Some(audience_out.display().to_string())
        }
        None => {
            let tmp = stage_beside(&args.out, &grant_json, false).await?;
            publish_staged(&tmp, &args.out).await?;
            None
        }
    };

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

/// RAII volatile-scrub for a byte buffer holding secret material: zeroes on drop
/// so EVERY exit path (early return, `?`, panic/unwind) scrubs — not only a
/// success tail reached after all fallible operations (Kyra OA2-F). Non-secret
/// payloads may use it too; the extra memset is harmless and keeps staging
/// uniform.
struct ScrubbedBytes(Vec<u8>);

impl ScrubbedBytes {
    fn new(bytes: Vec<u8>) -> Self {
        ScrubbedBytes(bytes)
    }
    fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

impl Drop for ScrubbedBytes {
    fn drop(&mut self) {
        zeroize_slice(&mut self.0);
    }
}

/// RAII volatile-scrub for a `String` holding secret material (e.g. the
/// serialized org root seed): scrubs on EVERY exit, including error returns from
/// the atomic write or the permission-enforcement step, not only the success
/// tail (Kyra OA2-F).
struct ScrubbedString(String);

impl ScrubbedString {
    fn new(s: String) -> Self {
        ScrubbedString(s)
    }
    fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

impl Drop for ScrubbedString {
    fn drop(&mut self) {
        zeroize_string(&mut self.0);
    }
}

/// Grant artifacts are published NO-CLOBBER: `--force` is refused for the grant
/// verbs because a remove-then-link replace is not crash-atomic (a crash between
/// the two loses the old artifact) and, on a case-insensitive filesystem, an
/// aliased `--out` (e.g. `ORG.TOML` vs `org.toml`) could destroy the org key.
/// Write to a fresh path, or remove the old artifact explicitly (Kyra OA2-F).
fn refuse_force(force: bool) -> Result<(), CliError> {
    if force {
        return Err(invalid_args(
            "--force is refused for grant commands: publication is no-clobber (a forced replace \
             is not crash-atomic and, on a case-insensitive filesystem, an aliased output could \
             destroy the org key). Write to a fresh path, or remove the old artifact explicitly.",
        ));
    }
    Ok(())
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
    // Decode into an RAII-scrubbed buffer so the seed clears on EVERY return
    // (hex error, length error, mismatch, success, or unwind) — not only a
    // manually placed cleanup statement at the tail (Kyra OA2-F). On error report
    // the category, never the offending value.
    let seed_bytes = ScrubbedBytes::new(hex::decode(parsed.seed_hex.as_bytes()).map_err(|_| {
        invalid_args(format!(
            "org key file {} seed_hex is not valid hex (kind: bad_seed_encoding)",
            path.display()
        ))
    })?);
    if seed_bytes.as_slice().len() != 32 {
        return Err(invalid_args(format!(
            "org key file {} seed must be 32 bytes (64 hex chars), got {} (kind: bad_seed_length)",
            path.display(),
            seed_bytes.as_slice().len()
        )));
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(seed_bytes.as_slice());
    let keypair = OrgKeypair::from_bytes(seed);
    zeroize_slice(&mut seed);
    // Consistency check: a hand-edited org_id_hex that disagrees with the seed
    // would otherwise sign as one org while claiming another.
    let derived = hex::encode(keypair.org_id().as_bytes());
    if !parsed.org_id_hex.eq_ignore_ascii_case(&derived) {
        return Err(invalid_args(format!(
            "org key file {}: org_id_hex does not match the key derived from seed_hex",
            path.display()
        )));
    }
    Ok(keypair)
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

/// Lexically normalize a path (absolute, `.`-collapsed) for alias comparison —
/// enough to catch a path passed as two outputs or aliased onto the input,
/// within the trustworthy-parent boundary (no fs access, so `..` / symlinks are
/// not resolved). This is a best-effort UX guard, case-sensitive; the actual
/// safety comes from no-clobber publication plus the `--force` refusal, not from
/// this comparison (Kyra OA2-F).
fn normalize_for_alias(p: &Path) -> PathBuf {
    std::path::absolute(p)
        .unwrap_or_else(|_| p.to_path_buf())
        .components()
        .collect()
}

/// Refuse aliased input/output paths (Kyra OA2-F): the org key must not be
/// overwritten, and two output artifacts must not collide (which would leave a
/// grant without its secret, or the reverse).
fn refuse_aliased_paths(paths: &[(&str, &Path)]) -> Result<(), CliError> {
    for i in 0..paths.len() {
        for j in (i + 1)..paths.len() {
            if normalize_for_alias(paths[i].1) == normalize_for_alias(paths[j].1) {
                return Err(invalid_args(format!(
                    "{} and {} resolve to the same path; refusing to alias them",
                    paths[i].0, paths[j].0
                )));
            }
        }
    }
    Ok(())
}

fn serialize_json<T: Serialize>(value: &T) -> Result<Vec<u8>, CliError> {
    serde_json::to_vec_pretty(value).map_err(|e| generic(format!("failed to serialize: {e}")))
}

/// A per-run stage nonce (pid + wall-clock nanos) so a stale temp left by a
/// crashed prior run never blocks a fresh `create_new`.
fn stage_nonce() -> String {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{pid}.{nanos}")
}

/// Stage `bytes` as a temp file in the SAME directory as `final_path`
/// (`create_new` + `fsync`; on Unix mode 0600 when `secret`, else 0644; on
/// Windows the file inherits the parent directory's DACL). The temp is
/// hard-linked onto the final path at publish, so a pre-existing leaf (incl. a
/// symlink) is never followed or truncated. Returns the temp path.
async fn stage_beside(final_path: &Path, bytes: &[u8], secret: bool) -> Result<PathBuf, CliError> {
    if let Some(parent) = final_path.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                generic(format!(
                    "failed to create parent directory {}: {e}",
                    parent.display()
                ))
            })?;
        }
    }
    let file_name = final_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("artifact");
    let tmp = final_path.with_file_name(format!(".{file_name}.stage.{}", stage_nonce()));
    let tmp_owned = tmp.clone();
    // Wrap the copied payload so it scrubs on EVERY exit of the blocking task
    // (open/write/sync failure, success, or unwind) — not only a success tail
    // reached after all fallible steps (Kyra OA2-F).
    let payload = ScrubbedBytes::new(bytes.to_vec());
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(if secret { 0o600 } else { 0o644 });
        }
        #[cfg(not(unix))]
        {
            // On Windows the temp inherits the parent directory's NTFS DACL; the
            // 0600 request has no std analog (warned about at publish time).
            let _ = secret;
        }
        // `create_new`: on open failure we created nothing (never remove a file
        // we don't own — the AlreadyExists case is someone else's file). Once
        // open succeeds we own the temp, so remove it SYNCHRONOUSLY on any
        // subsequent write/sync failure — a partial (possibly secret) temp is
        // never left behind (Kyra OA2-F).
        let mut f = opts.open(&tmp_owned)?;
        let written = (|| -> std::io::Result<()> {
            std::io::Write::write_all(&mut f, payload.as_slice())?;
            f.sync_all()
        })();
        if let Err(e) = written {
            drop(f);
            let _ = std::fs::remove_file(&tmp_owned);
            return Err(e);
        }
        Ok(())
        // `payload` drops here (and on every early return above), scrubbing the
        // copied bytes.
    })
    .await
    .map_err(|e| generic(format!("stage-write task panicked: {e}")))?
    .map_err(|e| generic(format!("failed to stage {}: {e}", tmp.display())))?;
    Ok(tmp)
}

/// Publish a staged temp onto `final_path` with NO-CLOBBER semantics: hard-link
/// (fails if `final_path` exists — never follows/truncates a leaf), then unlink
/// the temp and `fsync` the parent dir. There is NO forced-replace path: a
/// remove-then-link is not crash-atomic (a crash between the two loses the old
/// artifact), so `--force` is refused upstream (`refuse_force`). On a hard-link
/// failure the temp is cleaned up SYNCHRONOUSLY, and a failure to remove the
/// temp after a SUCCESSFUL publish is surfaced LOUDLY (a lingering `*.stage.*`
/// may be an extra name for a secret payload) — never silently ignored (Kyra
/// OA2-F).
async fn publish_staged(tmp: &Path, final_path: &Path) -> Result<(), CliError> {
    let tmp_owned = tmp.to_path_buf();
    let final_owned = final_path.to_path_buf();
    let link = tokio::task::spawn_blocking(move || std::fs::hard_link(&tmp_owned, &final_owned))
        .await
        .map_err(|e| generic(format!("publish task panicked: {e}")))?;
    if let Err(e) = link {
        remove_file_or_warn(tmp, "staging temp").await;
        return Err(if e.kind() == std::io::ErrorKind::AlreadyExists {
            invalid_args(format!(
                "file already exists at {}; publication is no-clobber — write to a fresh path \
                 or remove the old artifact explicitly",
                final_path.display()
            ))
        } else {
            generic(format!("failed to publish {}: {e}", final_path.display()))
        });
    }
    // The final path is now an independent name for the inode; drop the temp.
    // A removal failure here is surfaced loudly (never ignored) — a lingering
    // secret stage temp is an extra owner-only name for that secret.
    remove_file_or_warn(tmp, "staging temp").await;
    sync_parent_dir(final_path).await;
    Ok(())
}

/// Remove a transient artifact, warning LOUDLY with the exact path if removal
/// fails (best-effort by nature — the publish already succeeded or is being
/// rolled back — but never silent). A lingering `*.stage.*` for a secret payload
/// is an extra owner-only name for that secret (Kyra OA2-F).
async fn remove_file_or_warn(path: &Path, what: &str) {
    match tokio::fs::remove_file(path).await {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => eprintln!(
            "warning: failed to remove {what} {}: {e}; remove it manually.",
            path.display()
        ),
    }
}

/// On Windows the CLI has no clean 0600 analog from `std::fs`, so a secret file
/// inherits its parent directory's NTFS DACL. Surface the SAME loud warning the
/// org-key read path uses so a permissive ACL is at least observable in operator
/// logs; `--insecure-permissions` suppresses it (matching the Unix gate's escape
/// hatch). No-op on Unix, where the file was already created mode 0600. No new
/// ACL engine — the custom `--audience-out` parent is operator-asserted trusted
/// (Kyra OA2-F).
#[cfg(not(unix))]
fn warn_secret_permissions(path: &Path, insecure_permissions: bool) {
    if !insecure_permissions {
        eprintln!(
            "warning: the 0600 audience-secret mode is not enforced on Windows; {} inherits its \
             parent directory's NTFS DACL. Ensure the parent is owner-only (or pass \
             --insecure-permissions to silence).",
            path.display()
        );
    }
}

#[cfg(unix)]
fn warn_secret_permissions(_path: &Path, _insecure_permissions: bool) {}

/// `fsync` the parent directory so a link/rename is durable (Unix; best-effort
/// no-op where the platform doesn't support directory fsync).
async fn sync_parent_dir(path: &Path) {
    #[cfg(unix)]
    {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let parent = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        };
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

/// Pre-check for a nicer error than the race-free `create_new`/`hard_link`
/// enforcement in [`stage_beside`] / [`publish_staged`] produces. This is UX,
/// NOT the safety boundary — the publish path fails closed on its own.
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
        // Deliberately NO `--force` advice here. `--force` does not "override"
        // a stat failure — it skips the existence check entirely and publishes
        // over whatever is at the path. Steering the operator toward it on the
        // one path where we cannot see what we are about to replace is exactly
        // backwards (§17).
        Err(e) => Err(generic(format!("failed to stat {}: {e}", path.display()))),
    }
}

/// Publish a non-secret JSON artifact (a membership certificate, a revocation
/// bundle) at `final_path`.
///
/// Both branches stage beside the destination first, so the destination is
/// never truncated in place and a leaf symlink is never followed or written
/// through. They differ only in replace policy:
///
/// - **without `--force`**: hard-link, which fails closed if anything already
///   exists at `final_path`;
/// - **with `--force`**: `rename`, which replaces the destination ATOMICALLY.
///   Unlike the remove-then-link that makes a forced replace unsafe for the
///   grant/secret pair, a rename cannot lose the old artifact on a crash — the
///   destination always names either the old inode or the new one, never
///   nothing.
///
/// Certificates and revocation bundles are renewable by design (≈annual cert
/// renewal, floor bumps), so `--force` stays available here rather than being
/// refused outright as it is for grants ([`refuse_force`]). What it must never
/// do is what `tokio::fs::write` did before this: truncate an arbitrary
/// `--out` in place, following a symlink, with no alias check against
/// `--org-key` (§2).
async fn publish_json_artifact(
    final_path: &Path,
    json: &[u8],
    force: bool,
) -> Result<(), CliError> {
    if force {
        // Only the forced path needs this: without `--force` the no-clobber
        // hard-link already refuses anything that exists at the destination,
        // whatever it contains.
        refuse_replacing_org_key(final_path).await?;
    }
    let tmp = stage_beside(final_path, json, false).await?;
    if force {
        publish_staged_replace(&tmp, final_path).await
    } else {
        publish_staged(&tmp, final_path).await
    }
}

/// Refuse to publish over a file whose CONTENT is org root key material.
///
/// [`refuse_aliased_paths`] catches the common spelling, but it is a lexical,
/// case-sensitive comparison that resolves neither `..` nor symlinks — so on a
/// case-insensitive filesystem `--out ORG.TOML` against `--org-key org.toml`
/// slips past it. Without `--force` that is harmless (publication is
/// no-clobber, so the hard-link fails `AlreadyExists`). With `--force` the
/// replace would succeed and the org root would be unrecoverable.
///
/// Rather than reach for a platform file-identity API (`st_dev`/`st_ino` vs
/// `GetFileInformationByHandle`), this refuses on what actually matters: if the
/// destination parses as an org key file, it is not a publication target
/// however the path was spelled. One check covers case variants, symlinks,
/// hard links, and `..` traversal alike.
async fn refuse_replacing_org_key(path: &Path) -> Result<(), CliError> {
    // Absent, binary, or unreadable — nothing we can identify as key material,
    // and any real failure surfaces from the publish itself.
    let Ok(mut text) = tokio::fs::read_to_string(path).await else {
        return Ok(());
    };
    // The text may BE the seed; scrub it before returning on EITHER branch.
    // NEVER interpolate the parse error — its `Display` embeds the offending
    // source line, which for this file is `seed_hex` (the same reasoning as
    // `load_org_key_from_text`).
    let looks_like_org_key = toml::from_str::<toml::Value>(&text)
        .ok()
        .and_then(|v| v.get("seed_hex").cloned())
        .is_some();
    zeroize_string(&mut text);
    if looks_like_org_key {
        return Err(invalid_args(format!(
            "refusing to overwrite {}: it contains org root key material. --out must not name \
             the org key, however the path is spelled (case variant, symlink, or relative path).",
            path.display()
        )));
    }
    Ok(())
}

/// Atomic REPLACE publish for renewable artifacts: `rename` the staged temp
/// onto `final_path`. Crash-atomic (the destination names the old or the new
/// inode, never nothing) and it replaces a leaf symlink rather than writing
/// through it. The rename consumes the temp, so there is no post-publish
/// cleanup to leak.
async fn publish_staged_replace(tmp: &Path, final_path: &Path) -> Result<(), CliError> {
    let tmp_owned = tmp.to_path_buf();
    let final_owned = final_path.to_path_buf();
    let renamed = tokio::task::spawn_blocking(move || std::fs::rename(&tmp_owned, &final_owned))
        .await
        .map_err(|e| generic(format!("publish task panicked: {e}")))?;
    if let Err(e) = renamed {
        remove_file_or_warn(tmp, "staging temp").await;
        return Err(generic(format!(
            "failed to publish {}: {e}",
            final_path.display()
        )));
    }
    sync_parent_dir(final_path).await;
    Ok(())
}

// `write_json_envelope` (a `tokio::fs::write` wrapper) was deliberately
// REMOVED rather than left unused. It truncated its target in place and wrote
// THROUGH a leaf symlink, and it was the mechanism by which
// `issue-cert --force` / `issue-floors --force` could destroy the org root key
// (§2). Every artifact publish now goes through `stage_beside` +
// `publish_staged` (no-clobber) or `publish_staged_replace` (atomic replace),
// both of which stage beside the destination first. Do not reintroduce a
// direct-write helper here.

fn default_org_key_path(org_id_hex: &str) -> PathBuf {
    let short = &org_id_hex[..org_id_hex.len().min(16)];
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("net-mesh")
        .join("orgs")
        .join(format!("org-{short}.toml"))
}
