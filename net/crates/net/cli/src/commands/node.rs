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
    /// check. Strict by default, mirroring the token module.
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

    let dir = args
        .authority_dir
        .clone()
        .unwrap_or_else(default_authority_dir);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| generic(format!("failed to create {}: {e}", dir.display())))?;

    // Adopt — verifies before writing; loud on any refusal. Sync
    // file I/O on a oneshot CLI path; the same pattern as
    // `identity revoke`'s store write.
    let authority = NodeAuthority::adopt(&dir, cert_file.cert, &entity, args.skew_secs)
        .map_err(|e| sdk(format!("adopt refused: {e}")))?;

    // Optionally merge an operator floors bundle (monotone; a
    // lower bundle is a no-op, never an error).
    let floors_applied = match &args.floors {
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
            let raised = authority
                .revocation
                .apply_bundle(&floors_file.bundle)
                .map_err(|e| sdk(format!("floors bundle rejected: {e}")))?;
            Some(raised)
        }
        None => None,
    };

    let summary = AdoptOutput {
        authority_dir: dir.display().to_string(),
        owner_org_hex: hex::encode(authority.owner_org().as_bytes()),
        member_hex: hex::encode(entity.as_bytes()),
        generation: authority.config.owner_cert.generation,
        not_after: authority.config.owner_cert.not_after,
        files: NodeAuthority::file_names(),
        floors_raised: floors_applied,
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
    floors_raised: Option<usize>,
}

fn default_authority_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("net-mesh")
        .join("authority")
}

// Re-exported for the integration tests' path assertions.
#[allow(unused)]
pub(crate) fn authority_dir_for(root: &Path) -> PathBuf {
    root.join("authority")
}
