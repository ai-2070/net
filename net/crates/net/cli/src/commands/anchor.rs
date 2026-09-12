//! `net-mesh anchor` — the browser-facing anchor's operator surface
//! (plan §5 Layer 0, Stage 4b).
//!
//! This module owns the **browser bootstrap credential**: the
//! artefact an application hands a browser so it can reach an anchor
//! at all (an invite, the anchor Noise key the browser pins, the
//! NKpsk0 PSK, the trust domain that PSK defines, and the bootstrap
//! URL). See [`net_sdk::bootstrap_credential`] for the format and its
//! two lifetimes.
//!
//! ```text
//! net-mesh anchor credential mint    --root … --psk-hex … --anchor-noise-pubkey … --url …
//! net-mesh anchor credential inspect --credential <string|@path>
//! ```
//!
//! **The minted string contains the PSK.** `mint` therefore writes it
//! with the same 0600 staging discipline as `net-mesh org` uses for
//! key material, and refuses to overwrite an existing file without
//! `--force`. `inspect` never prints the PSK — only its trust-domain
//! id, which is a one-way function of it.

use std::path::PathBuf;
use std::time::Duration;

use clap::{Args, Subcommand};
use net_sdk::bootstrap_credential::{BrowserBootstrapCredential, Psk};
use net_sdk::enrollment::InviteToken;
use serde::Serialize;

use crate::commands::identity::parse_entity_hex;
use crate::commands::org::{publish_staged, stage_beside};
use crate::error::{generic, invalid_args, CliError};
use crate::parsers::hex_decode_32;
use crate::prelude::{emit_value, OutputFormat};

/// Default standing lifetime of the PSK half: 30 days. The invite
/// nonce's own default is minutes — the two are deliberately far
/// apart, which is the point of stating both in the format.
const DEFAULT_PSK_TTL_SECS: u64 = 30 * 24 * 3600;

/// Default lifetime of the single-use invite half: 15 minutes.
const DEFAULT_INVITE_TTL_SECS: u64 = 15 * 60;

#[derive(Subcommand, Debug)]
pub enum AnchorCommand {
    /// Browser bootstrap credentials (mint / inspect).
    #[command(subcommand)]
    Credential(CredentialCommand),
}

#[derive(Subcommand, Debug)]
pub enum CredentialCommand {
    /// Mint a browser bootstrap credential.
    Mint(MintArgs),
    /// Print a credential's fields. Never prints the PSK.
    Inspect(InspectArgs),
}

#[derive(Args, Debug)]
pub struct MintArgs {
    /// The mesh root entity id (64 hex chars, optional `0x`) — the
    /// key a joining browser anchor-verifies its grant against.
    #[arg(long, value_name = "HEX")]
    pub root: String,

    /// The anchor's Noise static X25519 **public** key (64 hex
    /// chars). This is the key the browser pins; it is never read
    /// out of the anchor's HTTP response.
    #[arg(long = "anchor-noise-pubkey", value_name = "HEX")]
    pub anchor_noise_pubkey: String,

    /// The trust domain's NKpsk0 PSK (64 hex chars). **A secret**:
    /// prefer `--psk-file`, which keeps it out of the shell history
    /// and the process table.
    #[arg(long = "psk-hex", value_name = "HEX", conflicts_with = "psk_file")]
    pub psk_hex: Option<String>,

    /// Read the PSK (64 hex chars) from a file instead of argv.
    #[arg(long = "psk-file", value_name = "PATH")]
    pub psk_file: Option<PathBuf>,

    /// The anchor's bootstrap listener URL, e.g.
    /// `https://anchor.example.com`. Must be `https://` (or
    /// `http://localhost` for development) — a browser cannot fetch
    /// anything else from a secure context.
    #[arg(long, value_name = "URL")]
    pub url: String,

    /// Lifetime of the single-use invite half, in seconds.
    #[arg(long = "invite-ttl-secs", default_value_t = DEFAULT_INVITE_TTL_SECS)]
    pub invite_ttl_secs: u64,

    /// Lifetime of the standing PSK half, in seconds.
    #[arg(long = "psk-ttl-secs", default_value_t = DEFAULT_PSK_TTL_SECS)]
    pub psk_ttl_secs: u64,

    /// Write the credential string here (0600). Without it the
    /// string goes to stdout, which is what a deployment pipeline
    /// usually wants.
    #[arg(long, value_name = "PATH")]
    pub out: Option<PathBuf>,

    /// Overwrite `--out` if it exists.
    #[arg(long)]
    pub force: bool,
}

#[derive(Args, Debug)]
pub struct InspectArgs {
    /// The credential string, or `@PATH` to read it from a file.
    #[arg(long, value_name = "STRING|@PATH")]
    pub credential: String,

    /// Check the credential against this trust domain's PSK (64 hex
    /// chars) — the check an anchor makes before doing anything with
    /// a presented credential.
    #[arg(long = "psk-hex", value_name = "HEX")]
    pub psk_hex: Option<String>,
}

/// What `mint` reports. The credential string is the payload; every
/// other field is there so the operator can see what they minted
/// without parsing it back.
#[derive(Serialize)]
struct MintReport {
    credential: String,
    trust_domain: String,
    bootstrap_url: String,
    /// The single-use half's deadline (unix seconds).
    nonce_expires_at: u64,
    /// The standing half's deadline (unix seconds).
    psk_expires_at: u64,
    /// Present only when `--out` was given.
    #[serde(skip_serializing_if = "Option::is_none")]
    written_to: Option<String>,
}

/// What `inspect` reports. **No PSK field exists on this struct** —
/// the trust-domain id is the only thing derived from it that is
/// safe to print.
#[derive(Serialize)]
struct InspectReport {
    root: String,
    rendezvous: String,
    bootstrap_url: String,
    anchor_noise_pubkey: String,
    trust_domain: String,
    nonce_expires_at: u64,
    psk_expires_at: u64,
    /// `Ok` / the reason the credential is not presentable right now.
    status: String,
    /// Present only with `--psk-hex`: whether this credential belongs
    /// to that PSK's trust domain.
    #[serde(skip_serializing_if = "Option::is_none")]
    trust_domain_matches: Option<bool>,
}

pub async fn run(cmd: AnchorCommand, output: Option<OutputFormat>) -> Result<(), CliError> {
    match cmd {
        AnchorCommand::Credential(CredentialCommand::Mint(args)) => run_mint(args, output).await,
        AnchorCommand::Credential(CredentialCommand::Inspect(args)) => {
            run_inspect(args, output).await
        }
    }
}

async fn run_mint(args: MintArgs, output: Option<OutputFormat>) -> Result<(), CliError> {
    let root = parse_entity_hex(&args.root)?;
    let anchor_noise_pubkey = hex_decode_32(&args.anchor_noise_pubkey)
        .map_err(|e| invalid_args(format!("--anchor-noise-pubkey: {e}")))?;
    let psk = Psk::new(read_psk(&args).await?);
    if args.invite_ttl_secs == 0 || args.psk_ttl_secs == 0 {
        return Err(invalid_args(
            "a zero TTL mints a credential that is already expired",
        ));
    }

    let invite = InviteToken::mint(
        &root,
        args.url.clone(),
        Duration::from_secs(args.invite_ttl_secs),
    );
    let credential = BrowserBootstrapCredential::mint(
        invite,
        anchor_noise_pubkey,
        psk,
        args.url.clone(),
        Duration::from_secs(args.psk_ttl_secs),
    );
    // Mint through the parser: a credential this binary cannot read
    // back is not one a browser could use either (the URL check
    // lives there, so a bad `--url` fails here rather than at the
    // browser).
    let encoded = credential.encode();
    let parsed = BrowserBootstrapCredential::decode(&encoded).map_err(|e| {
        invalid_args(format!(
            "refusing to emit a credential that does not parse back: {e}"
        ))
    })?;

    let written_to = match args.out.as_ref() {
        None => None,
        Some(path) => {
            if path.exists() && !args.force {
                return Err(generic(format!(
                    "{} already exists; pass --force to overwrite",
                    path.display()
                )));
            }
            // `secret: true` — the string carries the PSK, so it gets
            // the same 0600 staging as key material.
            let tmp = stage_beside(path, encoded.as_bytes(), true).await?;
            publish_staged_or_replace(&tmp, path, args.force).await?;
            Some(path.display().to_string())
        }
    };

    emit_value(
        OutputFormat::resolve_oneshot(output),
        &MintReport {
            credential: encoded,
            trust_domain: parsed.trust_domain.to_string(),
            bootstrap_url: parsed.bootstrap_url.clone(),
            nonce_expires_at: parsed.nonce_expires_at(),
            psk_expires_at: parsed.psk_expires_at(),
            written_to,
        },
    )
    .map_err(|e| generic(format!("write anchor credential mint: {e}")))?;
    Ok(())
}

async fn run_inspect(args: InspectArgs, output: Option<OutputFormat>) -> Result<(), CliError> {
    let raw = match args.credential.strip_prefix('@') {
        Some(path) => tokio::fs::read_to_string(path)
            .await
            .map_err(|e| invalid_args(format!("--credential @{path}: {e}")))?,
        None => args.credential.clone(),
    };
    let credential = BrowserBootstrapCredential::decode(&raw)
        .map_err(|e| invalid_args(format!("--credential: {e}")))?;

    let trust_domain_matches = match args.psk_hex.as_deref() {
        None => None,
        Some(hex) => {
            let psk =
                Psk::new(hex_decode_32(hex).map_err(|e| invalid_args(format!("--psk-hex: {e}")))?);
            Some(credential.check_trust_domain(&psk).is_ok())
        }
    };
    let status = match credential.validate() {
        Ok(()) => "ok".to_string(),
        Err(e) => e.to_string(),
    };

    emit_value(
        OutputFormat::resolve_oneshot(output),
        &InspectReport {
            root: hex_string(credential.root().as_bytes()),
            rendezvous: credential.invite.rendezvous.clone(),
            bootstrap_url: credential.bootstrap_url.clone(),
            anchor_noise_pubkey: hex_string(&credential.anchor_noise_pubkey),
            trust_domain: credential.trust_domain.to_string(),
            nonce_expires_at: credential.nonce_expires_at(),
            psk_expires_at: credential.psk_expires_at(),
            status,
            trust_domain_matches,
        },
    )
    .map_err(|e| generic(format!("write anchor credential inspect: {e}")))?;
    Ok(())
}

/// The PSK from `--psk-file` or `--psk-hex`, in that preference
/// order. Exactly one is required: minting without a PSK would
/// produce a credential no browser could handshake with.
async fn read_psk(args: &MintArgs) -> Result<[u8; 32], CliError> {
    let hex = match (args.psk_file.as_ref(), args.psk_hex.as_ref()) {
        (Some(path), _) => tokio::fs::read_to_string(path)
            .await
            .map_err(|e| invalid_args(format!("--psk-file {}: {e}", path.display())))?,
        (None, Some(hex)) => hex.clone(),
        (None, None) => return Err(invalid_args(
            "one of --psk-hex or --psk-file is required: the credential IS the PSK plus an invite",
        )),
    };
    hex_decode_32(hex.trim()).map_err(|e| invalid_args(format!("psk: {e}")))
}

async fn publish_staged_or_replace(
    tmp: &std::path::Path,
    final_path: &std::path::Path,
    force: bool,
) -> Result<(), CliError> {
    if force && final_path.exists() {
        let tmp_owned = tmp.to_path_buf();
        let final_owned = final_path.to_path_buf();
        return tokio::task::spawn_blocking(move || std::fs::rename(&tmp_owned, &final_owned))
            .await
            .map_err(|e| generic(format!("publish task panicked: {e}")))?
            .map_err(|e| generic(format!("publishing the credential failed: {e}")));
    }
    publish_staged(tmp, final_path).await
}

fn hex_string(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
