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
    /// List the RTC anchors this node has heard announce
    /// themselves, with their `rtc_addr` / `rtc_bootstrap`.
    ///
    /// Requires the `webrtc` build: an anchor row a build cannot act
    /// on is a listing with nothing behind it.
    #[cfg(feature = "webrtc")]
    Ls(LsArgs),
    /// Serve the browser bootstrap listener on this node.
    ///
    /// Requires the `rtc-bootstrap` build, which is the one that
    /// carries an HTTP server at all.
    #[cfg(feature = "rtc-bootstrap")]
    Serve(Box<ServeArgs>),
}

/// `net-mesh anchor ls`.
#[cfg(feature = "webrtc")]
#[derive(Args, Debug)]
pub struct LsArgs {
    /// Operator identity file.
    #[arg(long, value_name = "PATH")]
    pub identity: Option<PathBuf>,

    /// Supervisor node to query.
    #[arg(long, default_value_t = crate::prelude::DEFAULT_SUPERVISOR_NODE)]
    pub node: u64,
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

/// One row of `net-mesh anchor ls`.
#[cfg(feature = "webrtc")]
#[derive(serde::Serialize)]
struct AnchorRow {
    node: String,
    rtc_addr: Option<String>,
    rtc_bootstrap: Option<String>,
    noise_pubkey: Option<String>,
}

pub async fn run(
    cmd: AnchorCommand,
    output: Option<OutputFormat>,
    #[cfg_attr(
        not(any(feature = "webrtc", feature = "rtc-bootstrap")),
        allow(unused_variables)
    )]
    config_path: Option<&std::path::Path>,
    #[cfg_attr(
        not(any(feature = "webrtc", feature = "rtc-bootstrap")),
        allow(unused_variables)
    )]
    profile_name: &str,
) -> Result<(), CliError> {
    match cmd {
        AnchorCommand::Credential(CredentialCommand::Mint(args)) => run_mint(args, output).await,
        AnchorCommand::Credential(CredentialCommand::Inspect(args)) => {
            run_inspect(args, output).await
        }
        #[cfg(feature = "webrtc")]
        AnchorCommand::Ls(args) => run_ls(args, output, config_path, profile_name).await,
        #[cfg(feature = "rtc-bootstrap")]
        AnchorCommand::Serve(args) => run_serve(*args, output, config_path, profile_name).await,
    }
}

#[cfg(feature = "webrtc")]
async fn run_ls(
    args: LsArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    use crate::context::{resolve_profile, CliContext};

    let profile = resolve_profile(config_path, profile_name).await?;
    let ctx = CliContext::build(&profile, args.identity.as_deref(), args.node, false).await?;
    let rows: Vec<AnchorRow> = ctx
        .deck()
        .rtc_anchors()
        .into_iter()
        .map(|row| AnchorRow {
            node: format!("{:#x}", row.node_id),
            rtc_addr: row.rtc_addr.map(|a| a.to_string()),
            rtc_bootstrap: row.rtc_bootstrap,
            noise_pubkey: row.noise_pubkey.as_ref().map(|k| hex_string(k)),
        })
        .collect();
    emit_value(OutputFormat::resolve_oneshot(output), &rows)
        .map_err(|e| generic(format!("write anchor ls: {e}")))?;
    Ok(())
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

/// `net-mesh anchor serve` — run a browser-facing anchor: an RTC
/// socket, the §12 admission contract, and the bootstrap listener.
#[cfg(feature = "rtc-bootstrap")]
#[derive(Args, Debug)]
pub struct ServeArgs {
    /// Mesh bind address for the node itself.
    #[arg(long, value_name = "ADDR", default_value = "0.0.0.0:0")]
    pub bind: String,

    /// The transport trust domain's PSK (64 hex chars), read from a
    /// file. The same PSK the credentials were minted against.
    #[arg(long = "psk-file", value_name = "PATH")]
    pub psk_file: PathBuf,

    /// Address for the HTTPS bootstrap listener.
    #[arg(long = "listen", value_name = "ADDR", default_value = "0.0.0.0:8443")]
    pub listen: String,

    /// The externally reachable base URL of that listener. It is
    /// published as `rtc_bootstrap` on the announcement, so it must
    /// be the name on the certificate.
    #[arg(long = "url", value_name = "URL")]
    pub url: String,

    /// Public address of the RTC/STUN socket, published as
    /// `rtc_addr`. Required behind NAT; without it a browser has no
    /// address to aim ICE at.
    #[arg(long = "rtc-public-addr", value_name = "ADDR")]
    pub rtc_public_addr: Option<String>,

    /// Bind address of the RTC socket. Pin it when `--rtc-public-addr`
    /// maps a fixed port.
    #[arg(long = "rtc-bind", value_name = "ADDR")]
    pub rtc_bind: Option<String>,

    /// Operator-supplied certificate chain (PEM). With `--tls-key`.
    #[arg(long = "tls-cert", value_name = "PATH", requires = "tls_key")]
    pub tls_cert: Option<PathBuf>,

    /// Operator-supplied private key (PEM).
    #[arg(long = "tls-key", value_name = "PATH")]
    pub tls_key: Option<PathBuf>,

    /// ACME directory URL (HTTP-01 on this same listener).
    /// Mutually exclusive with `--tls-cert`.
    #[arg(
        long = "acme-directory",
        value_name = "URL",
        conflicts_with = "tls_cert"
    )]
    pub acme_directory: Option<String>,

    /// ACME contact e-mail.
    #[arg(long = "acme-email", value_name = "EMAIL")]
    pub acme_email: Option<String>,

    /// Where issued certificates are cached.
    #[arg(long = "acme-cache", value_name = "DIR")]
    pub acme_cache: Option<PathBuf>,

    /// Browser origins allowed to call the endpoints and open the
    /// trickle socket. Repeatable. **No wildcard** — an endpoint
    /// that takes a credential does not get one.
    #[arg(long = "allow-origin", value_name = "ORIGIN", required = true)]
    pub allow_origin: Vec<String>,

    /// Per-source-IP `POST /rtc/offer` ceiling per minute.
    #[arg(long = "offers-per-minute")]
    pub offers_per_minute: Option<u32>,
}

/// What `serve` reports once it is up.
#[cfg(feature = "rtc-bootstrap")]
#[derive(serde::Serialize)]
struct ServeReport {
    node: String,
    listening_on: String,
    bootstrap_url: String,
    rtc_addr: Option<String>,
    trust_domain: String,
    noise_pubkey: String,
}

#[cfg(feature = "rtc-bootstrap")]
async fn run_serve(
    args: ServeArgs,
    output: Option<OutputFormat>,
    _config_path: Option<&std::path::Path>,
    _profile_name: &str,
) -> Result<(), CliError> {
    use net_sdk::rtc_bootstrap::{
        serve_bootstrap, AcmeConfig, AcmeState, BootstrapConfig, BootstrapTls,
    };
    use net_sdk::Mesh;

    let psk_hex = tokio::fs::read_to_string(&args.psk_file)
        .await
        .map_err(|e| invalid_args(format!("--psk-file {}: {e}", args.psk_file.display())))?;
    let psk = hex_decode_32(psk_hex.trim()).map_err(|e| invalid_args(format!("psk: {e}")))?;

    let tls = match (&args.tls_cert, &args.tls_key, &args.acme_directory) {
        (Some(cert), Some(key), None) => BootstrapTls::Operator {
            cert_pem: cert.clone(),
            key_pem: key.clone(),
        },
        (None, _, Some(directory)) => {
            let domain = args
                .url
                .trim_start_matches("https://")
                .split('/')
                .next()
                .unwrap_or_default()
                .to_string();
            BootstrapTls::Acme(AcmeConfig {
                directory_url: directory.clone(),
                domain,
                contact_email: args.acme_email.clone().ok_or_else(|| {
                    invalid_args("--acme-email is required with --acme-directory")
                })?,
                cache_dir: args
                    .acme_cache
                    .clone()
                    .unwrap_or_else(|| std::env::temp_dir().join("net-mesh-acme")),
            })
        }
        _ => {
            return Err(invalid_args(
                "browser-trusted TLS is required: pass --tls-cert/--tls-key, or \
                 --acme-directory/--acme-email. There is no self-signed mode, because \
                 a browser refuses one",
            ))
        }
    };

    let mut rtc = net::adapter::net::rtc::RtcConfig::new().with_bootstrap_url(args.url.clone());
    rtc.serve_stun = true;
    if let Some(bind) = args.rtc_bind.as_ref() {
        rtc.bind_addr = Some(
            bind.parse()
                .map_err(|e| invalid_args(format!("--rtc-bind: {e}")))?,
        );
    }
    if let Some(public) = args.rtc_public_addr.as_ref() {
        rtc.public_addr = Some(
            public
                .parse()
                .map_err(|e| invalid_args(format!("--rtc-public-addr: {e}")))?,
        );
    }

    let mesh = Mesh::builder(&args.bind, &psk)
        .map_err(|e| generic(format!("mesh builder: {e}")))?
        .rtc(rtc)
        .build()
        .await
        .map_err(|e| generic(format!("starting the anchor: {e}")))?;
    mesh.start();

    let sdk_psk = Psk::new(psk);
    let mut listener_config = BootstrapConfig::new(
        args.listen
            .parse()
            .map_err(|e| invalid_args(format!("--listen: {e}")))?,
        sdk_psk.clone(),
        tls,
        args.allow_origin
            .first()
            .cloned()
            .unwrap_or_else(|| args.url.clone()),
    );
    listener_config.allowed_origins = args.allow_origin.clone();
    listener_config.ws_allowed_origins = args.allow_origin.clone();
    listener_config.acme = AcmeState::new();
    if let Some(limit) = args.offers_per_minute {
        listener_config.offers_per_ip_per_minute = limit;
    }

    let node = std::sync::Arc::clone(mesh.node());
    let handle = serve_bootstrap(node, listener_config)
        .await
        .map_err(|e| generic(format!("starting the bootstrap listener: {e}")))?;

    emit_value(
        OutputFormat::resolve_oneshot(output),
        &ServeReport {
            node: format!("{:#x}", mesh.node().node_id()),
            listening_on: handle.local_addr().to_string(),
            bootstrap_url: args.url.clone(),
            rtc_addr: args.rtc_public_addr.clone(),
            trust_domain: sdk_psk.trust_domain().to_string(),
            noise_pubkey: hex_string(mesh.node().public_key()),
        },
    )
    .map_err(|e| generic(format!("write anchor serve: {e}")))?;

    // Serve until interrupted; the listener owns its own accept loop.
    tokio::signal::ctrl_c()
        .await
        .map_err(|e| generic(format!("waiting for ctrl-c: {e}")))?;
    handle.shutdown().await;
    Ok(())
}
