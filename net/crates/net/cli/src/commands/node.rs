//! `net node adopt` — provision a node's organization ownership
//! (OA-1, `ORG_CAPABILITY_AUTH_PLAN.md`).
//!
//! Adoption installs THREE separately versioned files in the
//! node's authority directory
//! (`$XDG_CONFIG_HOME/net-mesh/authority` by default):
//!
//! ```text
//! owner-membership.json      // NodeAuthorityConfig + owner_cert
//! owner-audience.key         // owner audience handle + key (0600)
//! revocation-state.json      // persisted floor maxima
//! ```
//!
//! One node, one owner: adoption by a different org while an owner
//! is installed fails loudly. Re-adoption by the SAME org (cert
//! renewal) preserves the audience credential and the persisted
//! revocation floors.
//!
//! The certificate is verified BEFORE anything is written — for
//! THIS node's entity id, inside its window, at or above the
//! persisted revocation floor — the same loud self-verification
//! the node repeats at every startup.

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use net_sdk::identity::EntityId;
use net_sdk::org::NodeAuthority;
use serde::Serialize;

use crate::commands::identity::{parse_entity_hex, read_identity_file};
use crate::commands::org::{OrgCertFile, OrgFloorsFile, ORG_FILE_VERSION};
use crate::error::{generic, invalid_args, sdk, CliError};
use crate::prelude::{emit_value, OutputFormat};

#[derive(Subcommand, Debug)]
pub enum NodeCommand {
    /// Adopt this node into an organization (install ownership).
    Adopt(AdoptArgs),
}

#[derive(Args, Debug)]
pub struct AdoptArgs {
    /// Path to the membership certificate JSON (from
    /// `net org issue-cert`).
    #[arg(long, value_name = "PATH")]
    pub cert: PathBuf,

    /// Path to this node's identity file (TOML, from
    /// `net identity generate`); the certificate must name its
    /// public key. Mutually exclusive with `--entity`.
    #[arg(long, value_name = "PATH", conflicts_with = "entity")]
    pub identity: Option<PathBuf>,

    /// This node's entity id (64 hex chars, optional `0x`) —
    /// alternative to `--identity` when only the public half is at
    /// hand.
    #[arg(long, value_name = "HEX")]
    pub entity: Option<String>,

    /// Authority directory. Defaults to
    /// `$XDG_CONFIG_HOME/net-mesh/authority`.
    #[arg(long = "authority-dir", value_name = "DIR")]
    pub authority_dir: Option<PathBuf>,

    /// Optional operator revocation bundle (from
    /// `net org issue-floors`) to merge during adoption.
    #[arg(long, value_name = "PATH")]
    pub floors: Option<PathBuf>,

    /// Clock-skew tolerance (seconds) for the certificate window
    /// check. Strict by default, mirroring the token module;
    /// hard-capped at the token ceiling (300 s) — larger values
    /// are rejected before anything is written.
    #[arg(long = "skew-secs", default_value_t = 0)]
    pub skew_secs: u64,

    /// Allow a permissive identity-file mode on Unix (only
    /// relevant with `--identity`).
    #[arg(long)]
    pub insecure_permissions: bool,
}

pub async fn run(cmd: NodeCommand, output: Option<OutputFormat>) -> Result<(), CliError> {
    match cmd {
        NodeCommand::Adopt(args) => run_adopt(args, output).await,
    }
}

async fn run_adopt(args: AdoptArgs, output: Option<OutputFormat>) -> Result<(), CliError> {
    // Enforce the token-module clock-skew ceiling BEFORE touching
    // anything (review-8 §11): the library would refuse too, but a
    // misuse this plain is argument validation, not a ceremony
    // failure.
    if args.skew_secs > net_sdk::org::MAX_TOKEN_CLOCK_SKEW_SECS {
        return Err(invalid_args(format!(
            "--skew-secs {} exceeds the ceiling of {} seconds (MAX_TOKEN_CLOCK_SKEW_SECS)",
            args.skew_secs,
            net_sdk::org::MAX_TOKEN_CLOCK_SKEW_SECS
        )));
    }

    // Resolve the node's entity id.
    let entity: EntityId = match (&args.identity, &args.entity) {
        (Some(identity_path), None) => {
            let file = read_identity_file(identity_path, args.insecure_permissions).await?;
            parse_entity_hex(&file.public_key_hex)?
        }
        (None, Some(hex_id)) => parse_entity_hex(hex_id)?,
        (None, None) => {
            return Err(invalid_args(
                "pass --identity <PATH> (node identity file) or --entity <HEX>",
            ));
        }
        (Some(_), Some(_)) => unreachable!("clap conflicts_with enforces exclusivity"),
    };

    // Load the certificate envelope.
    let cert_text = tokio::fs::read_to_string(&args.cert).await.map_err(|e| {
        generic(format!(
            "failed to read certificate file {}: {e}",
            args.cert.display()
        ))
    })?;
    let cert_file: OrgCertFile = serde_json::from_str(&cert_text).map_err(|e| {
        invalid_args(format!(
            "certificate file {} failed to parse: {e}",
            args.cert.display()
        ))
    })?;
    if cert_file.version != ORG_FILE_VERSION {
        return Err(invalid_args(format!(
            "certificate file {} has unsupported version {}",
            args.cert.display(),
            cert_file.version
        )));
    }

    // Parse the optional floors bundle BEFORE the ceremony — it
    // participates in pre-write certificate validation inside
    // `NodeAuthority::adopt` (review-8 §7): a certificate the
    // resulting floors would immediately revoke never adopts, and
    // a bundle signed by any org other than the candidate owner is
    // refused before durable state changes (§6).
    let floors_bundle = match &args.floors {
        Some(bundle_path) => {
            let text = tokio::fs::read_to_string(bundle_path).await.map_err(|e| {
                generic(format!(
                    "failed to read floors bundle {}: {e}",
                    bundle_path.display()
                ))
            })?;
            let floors_file: OrgFloorsFile = serde_json::from_str(&text).map_err(|e| {
                invalid_args(format!(
                    "floors bundle {} failed to parse: {e}",
                    bundle_path.display()
                ))
            })?;
            if floors_file.version != ORG_FILE_VERSION {
                return Err(invalid_args(format!(
                    "floors bundle {} has unsupported version {}",
                    bundle_path.display(),
                    floors_file.version
                )));
            }
            Some(floors_file.bundle)
        }
        None => None,
    };

    let dir = match args.authority_dir.clone() {
        Some(explicit) => explicit,
        None => default_authority_dir().ok_or_else(|| {
            invalid_args(
                "cannot determine the default authority directory on this platform \
                 (no config dir); pass --authority-dir explicitly. Refusing to fall \
                 back to the working directory — the authority dir holds the raw \
                 owner audience key.",
            )
        })?,
    };
    // The authority scaffold (`NodeAuthority::adopt`) owns creating the
    // authority directory as an owner-only (0700, atomic) local security
    // boundary and validating an existing one. We deliberately do NOT
    // pre-create it here with a lax umask — that create-then-chmod pattern is
    // exactly what the scaffold avoids (Gate-1).

    // Gate-1 (Windows): the authority scaffold now creates a missing directory
    // (and any missing parents) with a protected owner-only DACL, and
    // re-validates an EXISTING directory's DACL (fail closed unless every
    // write-capable ACE is a trusted principal). What it does NOT walk for a
    // custom path is the PRE-EXISTING ancestor chain: a writable ancestor's
    // owner could still replace the directory entry, which a child DACL cannot
    // prevent. Warn so the operator keeps a custom dir under a protected parent.
    #[cfg(windows)]
    if args.authority_dir.is_some() {
        eprintln!(
            "warning: custom --authority-dir {}: the authority directory's own DACL is \
             created owner-only / re-validated, but its PRE-EXISTING parent directories \
             are not walked on Windows; keep it under a per-user protected location \
             restricted to your account",
            dir.display()
        );
    }

    // Adopt — the ceremony validates EVERYTHING (including the
    // supplied bundle's resulting floors) before writing, applies
    // floors durably, and publishes membership last. Sync file I/O
    // on a oneshot CLI path; the same pattern as `identity
    // revoke`'s store write.
    let authority = NodeAuthority::adopt(
        &dir,
        cert_file.cert,
        &entity,
        args.skew_secs,
        floors_bundle.as_ref(),
    )
    .map_err(|e| sdk(format!("adopt refused: {e}")))?;

    let summary = AdoptOutput {
        authority_dir: dir.display().to_string(),
        owner_org_hex: hex::encode(authority.owner_org().as_bytes()),
        member_hex: hex::encode(entity.as_bytes()),
        generation: authority.config.owner_cert.generation,
        not_after: authority.config.owner_cert.not_after,
        files: NodeAuthority::file_names(),
        floors_applied: floors_bundle.as_ref().map(|b| b.floors().len()),
    };
    emit_value(OutputFormat::resolve_oneshot(output), &summary)
        .map_err(|e| generic(format!("write summary: {e}")))?;
    Ok(())
}

#[derive(Debug, Serialize)]
struct AdoptOutput {
    authority_dir: String,
    owner_org_hex: String,
    member_hex: String,
    generation: u32,
    not_after: u64,
    files: [&'static str; 3],
    #[serde(skip_serializing_if = "Option::is_none")]
    floors_applied: Option<usize>,
}

/// The default authority directory, or `None` when the platform config
/// directory cannot be resolved (§19).
///
/// Deliberately NOT falling back to `PathBuf::from(".")`. The authority
/// directory holds `owner-audience.key` — the raw owner discovery key — and a
/// CWD fallback silently relocates it to wherever the operator happened to be
/// standing. On Unix the ancestor-trust walk would refuse a hostile CWD, but
/// on Windows nothing would: `validate_existing_dir_dacl` checks the directory
/// it is given, and a world-writable CWD that the operator owns passes both
/// the owner check and the ACE walk.
///
/// A broken environment is rare, but the failure mode is provisioning key
/// material somewhere unintended and unnoticed. Refusing and telling the
/// operator to pass `--authority-dir` is strictly better than guessing.
fn default_authority_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("net-mesh").join("authority"))
}

// Re-exported for the integration tests' path assertions.
#[allow(unused)]
pub(crate) fn authority_dir_for(root: &Path) -> PathBuf {
    root.join("authority")
}
