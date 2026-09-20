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

use crate::commands::identity::{parse_entity_hex, read_identity_file};
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
    /// themselves, with their `rtc_addr` / `rtc_bootstrap`, plus
    /// `rtc_stun_addr` for an anchor that announced a separate
    /// STUN endpoint.
    ///
    /// Requires the `rtc-bootstrap` build: it reads the listing
    /// from a live daemon's anchor directory, and an anchor row a
    /// build cannot act on is a listing with nothing behind it.
    #[cfg(feature = "rtc-bootstrap")]
    Ls(LsArgs),
    /// Read an anchor's ICE attempt ledger: plan §10's
    /// `ice_direct / ice_attempted` deployment telemetry.
    ///
    /// **The denominator is ATTEMPTS, not sessions.** One attempt is
    /// one signalling dialog — the offer the anchor sent, or an
    /// offer it accepted (which on an anchor includes the bootstrap
    /// dialog of every browser that arrived). A caller that retries
    /// after a timeout spends two attempts; an ICE restart inside
    /// one dialog is one.
    ///
    /// The ratio is reported, never gated, and it is NOT a success
    /// rate for sessions: a relayed session is not a failed one —
    /// the routed path through an anchor is a supported disposition.
    /// Nor is it a per-pair fact; ask the pair whether a pair is
    /// direct.
    ///
    /// Requires the `rtc-bootstrap` build: the ledger is not fold
    /// state, so it is read from the live node that owns it.
    #[cfg(feature = "rtc-bootstrap")]
    Stats(StatsArgs),
    /// Serve the browser bootstrap listener on this node.
    ///
    /// Requires the `rtc-bootstrap` build, which is the one that
    /// carries an HTTP server at all.
    #[cfg(feature = "rtc-bootstrap")]
    Serve(Box<ServeArgs>),
}

/// `net-mesh anchor ls`.
#[cfg(feature = "rtc-bootstrap")]
#[derive(Args, Debug)]
pub struct LsArgs {
    /// Operator identity file.
    #[arg(long, value_name = "PATH")]
    pub identity: Option<PathBuf>,

    /// Supervisor node to query.
    #[arg(long, default_value_t = crate::prelude::DEFAULT_SUPERVISOR_NODE)]
    pub node: u64,

    /// How long to wait for the attached node to ingest the
    /// daemon's announcements before answering. An attach is a fresh
    /// session: the rows arrive as the announcements flood to it.
    #[arg(long = "wait-secs", default_value_t = 5)]
    pub wait_secs: u64,

    /// The daemon to attach to. Anchor rows come from
    /// signature-verified announcements a LIVE mesh has ingested, so
    /// this listing needs a mesh — the Deck client the CLI builds
    /// in-process has none (R6).
    #[command(flatten)]
    pub remote: crate::commands::aggregator::RemoteAttachArgs,
}

/// `net-mesh anchor stats`.
#[cfg(feature = "rtc-bootstrap")]
#[derive(Args, Debug)]
pub struct StatsArgs {
    /// Operator identity file.
    #[arg(long, value_name = "PATH")]
    pub identity: Option<PathBuf>,

    /// Supervisor node to query.
    #[arg(long, default_value_t = crate::prelude::DEFAULT_SUPERVISOR_NODE)]
    pub node: u64,

    /// How long to wait for the anchor to answer.
    #[arg(long = "wait-secs", default_value_t = 5)]
    pub wait_secs: u64,

    /// The anchor to ask. An ICE attempt ledger is the answering
    /// node's OWN and is never announced, so — unlike fold state —
    /// there is no local view of it to fall back on.
    #[command(flatten)]
    pub remote: crate::commands::aggregator::RemoteAttachArgs,
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
    /// Inspect signer, PSK source and output without reading the PSK or minting.
    #[arg(long)]
    pub inspect_target: bool,
    /// The mesh root entity id (64 hex chars, optional `0x`) — the
    /// key a joining browser anchor-verifies its grant against.
    #[arg(long, value_name = "HEX")]
    pub root: String,

    /// The **issuer** identity file (TOML, from
    /// `net-mesh identity generate`) that signs this credential.
    ///
    /// The anchor is configured with its public half and verifies
    /// the signature before reading any other field, so the two
    /// lifetimes are the issuer's to set. A recipient holds the PSK
    /// and can re-encode anything — but cannot sign.
    #[arg(long = "issuer-identity", value_name = "PATH")]
    pub issuer_identity: PathBuf,

    /// Allow a permissive identity-file mode on Unix.
    #[arg(long)]
    pub insecure_permissions: bool,

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
    /// Inspect input selection without reading or decoding the credential.
    #[arg(long)]
    pub inspect_target: bool,
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
    issuer: String,
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
    issuer: String,
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
#[cfg(feature = "rtc-bootstrap")]
#[derive(serde::Serialize)]
struct AnchorRow {
    node: String,
    rtc_addr: Option<String>,
    /// The anchor's **separately announced STUN endpoint** (Stage
    /// 6), when it configured one. Omitted from the JSON otherwise:
    /// the overwhelming majority of anchors announce none, and a row
    /// for one of those must read exactly as it did before this
    /// field existed. The siblings keep emitting `null` — they
    /// predate the convention, and changing their rendering is a
    /// consumer-visible decision this slice does not make.
    #[serde(skip_serializing_if = "Option::is_none")]
    rtc_stun_addr: Option<String>,
    rtc_bootstrap: Option<String>,
    noise_pubkey: Option<String>,
}

/// What `net-mesh anchor stats` prints.
///
/// Every field the operator needs to read the ratio correctly is in
/// the row — the denominator, the residual, and the denominator's
/// own definition — because a ratio whose denominator is ambiguous
/// is a metric that gets misread for a year.
#[cfg(feature = "rtc-bootstrap")]
#[derive(serde::Serialize)]
struct IceStatsRow {
    /// The anchor that answered. The ledger is its own.
    node: String,
    /// `false` when that node has no RTC driver: it keeps no attempt
    /// ledger at all, and the counters below are placeholders rather
    /// than observations.
    rtc_configured: bool,
    /// **The denominator**: direct-path attempts, one per signalling
    /// dialog.
    ice_attempted: u64,
    /// Attempts that ended with an installed direct RTC endpoint.
    ice_direct: u64,
    /// Attempts that hit their deadline with ICE never connected —
    /// for a peer dialog, the pair stayed on the anchor. Not a
    /// failure.
    ice_relayed: u64,
    /// Attempts that ended for a reason other than their deadline.
    ice_failed: u64,
    /// Attempts still in flight. The outcome terms sum to
    /// `ice_attempted` only when this is zero.
    ice_pending: u64,
    /// `ice_direct / ice_attempted`, or `null` when nothing has been
    /// attempted. **`null` is not `0.0`** — no attempts is not zero
    /// per cent.
    ice_direct_ratio: Option<f64>,
    /// The ratio as an operator reads it out loud, denominator
    /// included (`"6/9 attempts (67%)"`), or the reason there is no
    /// number. Rendered, never parsed.
    ice_direct_display: String,
    /// What the denominator IS, carried with the numbers so the
    /// ratio cannot be quoted without it.
    denominator: &'static str,
    /// What the ratio does NOT mean.
    caveat: &'static str,
}

/// The one-line prose form of the denominator, printed with every
/// row.
#[cfg(feature = "rtc-bootstrap")]
const ICE_DENOMINATOR: &str = "ice_attempted = direct-path ATTEMPTS, one per signalling dialog \
     (the offer this anchor sent, or an offer it accepted — including each browser's bootstrap \
     dialog, and on a browser leaf its own bootstrap dialog with its anchor, so a page that went \
     direct with one peer reports TWO attempts). Not sessions, not peers: a retry after a timeout \
     spends two attempts, and an ICE restart inside one dialog is one. PER PARTICIPANT, never per \
     system: this is THIS node's own ledger. Two browsers going direct through one anchor is three \
     dialogs in the system, and no counter reports three — each browser reports two and this \
     anchor reports two. Summing ledgers across nodes double-counts every pair dialog.";

/// The one-line prose form of what the ratio is not.
#[cfg(feature = "rtc-bootstrap")]
const ICE_CAVEAT: &str = "Reported, never gated. NOT a success rate for sessions: a relayed \
     session is not a failed one — the routed path through an anchor is a supported disposition, \
     and ICE reaching `connected` is not a health gate. NOT a per-pair fact either: ask the pair \
     (`peer_endpoint`) whether a given pair is direct. `udp_blocked` is absent because a node \
     signalling over UDP cannot have UDP blocked; that term belongs to the browser leaf.";

pub async fn run(
    cmd: AnchorCommand,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    match cmd {
        AnchorCommand::Credential(CredentialCommand::Mint(args)) if args.inspect_target => {
            let psk_source = psk_source(&args)?;
            let issuer = load_credential_issuer(&args).await?;
            let profile = crate::context::resolve_profile(config_path, profile_name).await?;
            let mut target = crate::target::TargetInspection::local(&profile, "offline");
            target.configured_identity(issuer.entity_id().as_bytes());
            target.source = Some(args.issuer_identity);
            target.destination = args.out;
            target.provenance("identity", "flag");
            target.provenance("source", "flag");
            target.provenance(
                "destination",
                if target.destination.is_some() {
                    "flag"
                } else {
                    "stdout"
                },
            );
            target.provenance("psk", psk_source);
            emit_value(
                OutputFormat::resolve_oneshot(output),
                &CredentialTargetInspection {
                    target,
                    psk_source,
                    psk_file: args.psk_file,
                    credential_stdout_on_execution: true,
                },
            )
            .map_err(|e| generic(format!("write inspection: {e}")))
        }
        AnchorCommand::Credential(CredentialCommand::Inspect(args)) if args.inspect_target => {
            let profile = crate::context::resolve_profile(config_path, profile_name).await?;
            let mut target = crate::target::TargetInspection::local(&profile, "offline");
            if let Some(path) = args.credential.strip_prefix('@') {
                target.source = Some(PathBuf::from(path));
                target.provenance("source", "file");
            } else {
                target.provenance("source", "inline");
            }
            let psk_source = if args.psk_hex.is_some() {
                "inline"
            } else {
                "unused"
            };
            target.provenance("psk", psk_source);
            emit_value(
                OutputFormat::resolve_oneshot(output),
                &CredentialTargetInspection {
                    target,
                    psk_source,
                    psk_file: None,
                    credential_stdout_on_execution: false,
                },
            )
            .map_err(|e| generic(format!("write inspection: {e}")))
        }
        AnchorCommand::Credential(CredentialCommand::Mint(args)) => run_mint(args, output).await,
        AnchorCommand::Credential(CredentialCommand::Inspect(args)) => {
            run_inspect(args, output).await
        }
        #[cfg(feature = "rtc-bootstrap")]
        AnchorCommand::Ls(args) => run_ls(args, output, config_path, profile_name).await,
        #[cfg(feature = "rtc-bootstrap")]
        AnchorCommand::Stats(args) => run_stats(args, output, config_path, profile_name).await,
        #[cfg(feature = "rtc-bootstrap")]
        AnchorCommand::Serve(args) => run_serve(*args, output, config_path, profile_name).await,
    }
}

#[derive(Serialize)]
struct CredentialTargetInspection {
    #[serde(flatten)]
    target: crate::target::TargetInspection,
    psk_source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    psk_file: Option<PathBuf>,
    /// Mint's normal report includes the secret credential even with --out.
    credential_stdout_on_execution: bool,
}

fn psk_source(args: &MintArgs) -> Result<&'static str, CliError> {
    match (&args.psk_file, &args.psk_hex) {
        (Some(_), _) => Ok("file"),
        (None, Some(_)) => Ok("inline"),
        (None, None) => Err(invalid_args(
            "one of --psk-hex or --psk-file is required: the credential IS the PSK plus an invite",
        )),
    }
}

async fn load_credential_issuer(args: &MintArgs) -> Result<net_sdk::identity::Identity, CliError> {
    let issuer_file = read_identity_file(&args.issuer_identity, args.insecure_permissions).await?;
    let seed = hex_decode_32(&issuer_file.seed_hex)
        .map_err(|e| invalid_args(format!("--issuer-identity: seed_hex: {e}")))?;
    Ok(net_sdk::identity::Identity::from_seed(seed))
}

#[cfg(feature = "rtc-bootstrap")]
async fn run_ls(
    args: LsArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    use crate::context::{require_remote_attach, resolve_profile, CliContext};

    let profile = resolve_profile(config_path, profile_name).await?;
    // **R6: the rows come from a live mesh.** The in-process Deck
    // client is built with `mesh: None`, so `rtc_anchors()` on it was
    // structurally empty — the listing could never show an anchor no
    // matter how many announced. Attaching to the daemon is the same
    // path every other cross-node listing uses.
    let remote = require_remote_attach(&profile, &args.remote, || {
        invalid_args("anchor ls needs a live mesh target: pass --node-addr/--node-pubkey/--node-id/--psk-hex or set profile defaults")
    })?;
    if args.remote.inspect_target {
        return crate::target::inspect(
            &profile,
            &args.remote,
            args.identity.as_deref(),
            Some(&remote),
            "remote",
        )
        .await?
        .emit(output);
    }
    let target = remote.node_id;
    let ctx =
        CliContext::build_with_remote(&profile, args.identity.as_deref(), args.node, false, remote)
            .await?;
    // **Ask the node that has ingested the announcements.** The two
    // address fields are locally-filled fold projections that
    // deliberately do not travel, so a freshly attached client's own
    // view is empty by construction — which is precisely how this
    // listing used to be structurally empty. The anchor answers for
    // itself over `net.mesh.anchors`.
    let mesh = ctx.require_mesh()?;
    let raw = tokio::time::timeout(
        std::time::Duration::from_secs(args.wait_secs.max(1)),
        mesh.call_raw_bytes(
            target,
            net_sdk::rtc_bootstrap::ANCHOR_DIRECTORY_SERVICE,
            Vec::new(),
        ),
    )
    .await
    .map_err(|_| generic("the anchor directory did not answer in time"))?
    .map_err(|e| {
        generic(format!(
            "the anchor directory did not answer ({e}) — is this node running \
             `net-mesh anchor serve`?"
        ))
    })?;
    let rows: Vec<AnchorRow> =
        serde_json::from_slice::<Vec<net_sdk::rtc_bootstrap::AnchorDirectoryRow>>(&raw)
            .map_err(|e| generic(format!("the anchor directory's reply did not parse: {e}")))?
            .into_iter()
            .map(|row| AnchorRow {
                node: row.node,
                rtc_addr: row.rtc_addr,
                rtc_stun_addr: row.rtc_stun_addr,
                rtc_bootstrap: row.rtc_bootstrap,
                noise_pubkey: row.noise_pubkey,
            })
            .collect();
    emit_value(OutputFormat::resolve_oneshot(output), &rows)
        .map_err(|e| generic(format!("write anchor ls: {e}")))?;
    Ok(())
}

/// `net-mesh anchor stats` — the answering anchor's ICE attempt
/// ledger.
///
/// Remote by construction, and not for the reason `ls` is: an
/// attempt ledger is not fold state and is never announced, so there
/// is no local projection of it that could be read instead. The node
/// that owns the ledger is the only node that can answer for it.
#[cfg(feature = "rtc-bootstrap")]
async fn run_stats(
    args: StatsArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    use crate::context::{require_remote_attach, resolve_profile, CliContext};

    let profile = resolve_profile(config_path, profile_name).await?;
    let remote = require_remote_attach(&profile, &args.remote, || {
        invalid_args("anchor stats needs a live mesh target: pass --node-addr/--node-pubkey/--node-id/--psk-hex or set profile defaults")
    })?;
    if args.remote.inspect_target {
        return crate::target::inspect(
            &profile,
            &args.remote,
            args.identity.as_deref(),
            Some(&remote),
            "remote",
        )
        .await?
        .emit(output);
    }
    let target = remote.node_id;
    let ctx =
        CliContext::build_with_remote(&profile, args.identity.as_deref(), args.node, false, remote)
            .await?;
    let mesh = ctx.require_mesh()?;
    let raw = tokio::time::timeout(
        Duration::from_secs(args.wait_secs.max(1)),
        mesh.call_raw_bytes(
            target,
            net_sdk::rtc_bootstrap::ANCHOR_ICE_STATS_SERVICE,
            Vec::new(),
        ),
    )
    .await
    .map_err(|_| generic("the anchor's ICE stats did not answer in time"))?
    .map_err(|e| {
        generic(format!(
            "the anchor's ICE stats did not answer ({e}) — is this node running \
             `net-mesh anchor serve`?"
        ))
    })?;
    let stats = serde_json::from_slice::<net_sdk::rtc_bootstrap::AnchorIceStats>(&raw)
        .map_err(|e| generic(format!("the anchor's ICE stats reply did not parse: {e}")))?;
    let row = IceStatsRow {
        ice_direct_display: ice_direct_display(&stats),
        node: stats.node,
        rtc_configured: stats.rtc_configured,
        ice_attempted: stats.attempted,
        ice_direct: stats.direct,
        ice_relayed: stats.relayed,
        ice_failed: stats.failed,
        ice_pending: stats.pending,
        ice_direct_ratio: stats.direct_ratio,
        denominator: ICE_DENOMINATOR,
        caveat: ICE_CAVEAT,
    };
    emit_value(OutputFormat::resolve_oneshot(output), &row)
        .map_err(|e| generic(format!("write anchor stats: {e}")))?;
    Ok(())
}

/// The ratio spelled out with its denominator, or the reason there
/// is no ratio to spell.
///
/// Three distinct absences, never collapsed into a number:
/// "no rtc driver" (no ledger exists), "no attempts" (the ledger is
/// empty — **not** `0%`, which would report total failure where
/// nothing has happened), and the ratio itself, which always carries
/// `n/m attempts` so it cannot be quoted as a session success rate.
#[cfg(feature = "rtc-bootstrap")]
fn ice_direct_display(stats: &net_sdk::rtc_bootstrap::AnchorIceStats) -> String {
    if !stats.rtc_configured {
        return "no rtc driver — this node keeps no attempt ledger".to_string();
    }
    let Some(ratio) = stats.direct_ratio else {
        return "no attempts yet — no direct-path ratio exists (this is not 0%)".to_string();
    };
    let head = format!(
        "{}/{} attempts ({}%)",
        stats.direct,
        stats.attempted,
        (ratio * 100.0).round() as u64
    );
    if stats.pending == 0 {
        head
    } else {
        format!(
            "{head}, {} attempt(s) still in flight — the outcome terms sum to \
             ice_attempted only once that is 0",
            stats.pending
        )
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

    let issuer = load_credential_issuer(&args).await?;
    let invite = InviteToken::mint(
        &root,
        args.url.clone(),
        Duration::from_secs(args.invite_ttl_secs),
    );
    let credential = BrowserBootstrapCredential::mint(
        &issuer,
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
            issuer: hex_string(parsed.issuer.as_bytes()),
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
            issuer: hex_string(credential.issuer.as_bytes()),
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
    psk_source(args)?;
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
    /// Resolve listeners/TLS paths without reading PSK/TLS files or starting services.
    #[arg(long)]
    pub inspect_target: bool,
    /// Mesh bind address for the node itself (default: 0.0.0.0:0).
    #[arg(long, value_name = "ADDR")]
    pub bind: Option<String>,

    /// The transport trust domain's PSK (64 hex chars), read from a
    /// file. The same PSK the credentials were minted against.
    #[arg(long = "psk-file", value_name = "PATH")]
    pub psk_file: PathBuf,

    /// Address for the HTTPS bootstrap listener (default: 0.0.0.0:8443).
    #[arg(long = "listen", value_name = "ADDR")]
    pub listen: Option<String>,

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

    /// Bind address of a SECOND UDP socket that answers STUN only,
    /// published as `rtc_stun_addr` on the announcement. Off by
    /// default: an anchor that passes neither this nor
    /// `--rtc-stun-public-addr` opens no second socket and
    /// announces no STUN endpoint.
    ///
    /// It exists because `--rtc-public-addr` is this anchor's ICE
    /// endpoint: a browser pairing WITH this anchor cannot gather
    /// against it, so the endpoint it gathers against has to be a
    /// distinct one. Use `:0` to let the OS pick — the announced
    /// value is the port the socket actually bound, never a guess.
    #[arg(long = "rtc-stun-bind", value_name = "ADDR")]
    pub rtc_stun_bind: Option<String>,

    /// Public address of that STUN socket, published as
    /// `rtc_stun_addr`. Required behind NAT, and it must be a
    /// **distinct externally reachable endpoint** from
    /// `--rtc-public-addr` — two ports on this host is fine, a
    /// gateway mapping that lands both on one public tuple is not.
    #[arg(long = "rtc-stun-public-addr", value_name = "ADDR")]
    pub rtc_stun_public_addr: Option<String>,

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

    /// Where issued certificates are cached. One subdirectory per
    /// domain (R4b).
    #[arg(long = "acme-cache", value_name = "DIR")]
    pub acme_cache: Option<PathBuf>,

    /// Address for the plaintext HTTP-01 challenge ingress, bound
    /// before ordering. Defaults to `0.0.0.0:80`, the port an ACME
    /// directory dials.
    #[arg(long = "acme-challenge-addr", value_name = "ADDR")]
    pub acme_challenge_addr: Option<String>,

    /// The issuer whose signature this anchor accepts on a
    /// credential (64 hex chars) — the public half of the key
    /// `anchor credential mint --issuer-identity` uses (R3).
    #[arg(long = "credential-issuer", value_name = "HEX")]
    pub credential_issuer: String,

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
    /// The endpoint this anchor announces as `rtc_addr`, read off
    /// the running node rather than echoed from the flag.
    rtc_addr: Option<String>,
    /// The announced STUN endpoint (Stage 6), **as resolved** — the
    /// operator's override when a second socket was bound, the
    /// address that socket actually bound when it was not, and
    /// absent when there is no second socket at all.
    ///
    /// Not an echo of `--rtc-stun-public-addr`. An override without
    /// a bind is refused at startup, and if it were merely echoed
    /// here the report would describe an endpoint nothing answers
    /// on; a `:0` bind, conversely, has no value to echo and a real
    /// one to report.
    ///
    /// **Deliberately asymmetric with its sibling**: this key is
    /// absent when no STUN endpoint was configured, while
    /// `rtc_addr` above still renders `null`. `rtc_addr` predates
    /// the omit-when-absent convention and a consumer parsing this
    /// report is entitled to the shape it already has, so changing
    /// it is a separate, consumer-visible decision — not a side
    /// effect of adding a field.
    #[serde(skip_serializing_if = "Option::is_none")]
    rtc_stun_addr: Option<String>,
    trust_domain: String,
    noise_pubkey: String,
}

/// The RTC driver configuration `serve` runs with, from the
/// operator's flags.
///
/// Its own function so the flag → config mapping is a thing a test
/// can read without binding a socket or starting a mesh: every
/// address here is operator input, and the failure mode worth
/// guarding is a flag that parses fine and lands on nothing.
///
/// Two pairs, and they are not interchangeable:
///
/// * `--rtc-bind` / `--rtc-public-addr` → the ICE/RTC socket. It
///   keeps `serve_stun = true`, which is what answers the
///   diagnostic `UdpBlocked` probe aimed at `rtc_addr`.
/// * `--rtc-stun-bind` / `--rtc-stun-public-addr` → the SECOND,
///   STUN-only socket announced as `rtc_stun_addr` (Stage 6),
///   additional to the first and never a replacement for it.
///   Neither flag given: no second socket, nothing announced.
#[cfg(feature = "rtc-bootstrap")]
fn rtc_config_from_args(args: &ServeArgs) -> Result<net::adapter::net::rtc::RtcConfig, CliError> {
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
    if let Some(bind) = args.rtc_stun_bind.as_ref() {
        rtc.stun_addr = Some(
            bind.parse()
                .map_err(|e| invalid_args(format!("--rtc-stun-bind: {e}")))?,
        );
    }
    if let Some(public) = args.rtc_stun_public_addr.as_ref() {
        rtc.stun_public_addr = Some(
            public
                .parse()
                .map_err(|e| invalid_args(format!("--rtc-stun-public-addr: {e}")))?,
        );
    }
    Ok(rtc)
}

#[cfg(feature = "rtc-bootstrap")]
fn resolve_serve(args: &ServeArgs) -> Result<ResolvedServe, CliError> {
    use net_sdk::rtc_bootstrap::{AcmeConfig, BootstrapTls};
    let bind: std::net::SocketAddr = args
        .bind
        .as_deref()
        .unwrap_or("0.0.0.0:0")
        .parse()
        .map_err(|e| invalid_args(format!("--bind: {e}")))?;
    let listen = args
        .listen
        .as_deref()
        .unwrap_or("0.0.0.0:8443")
        .parse()
        .map_err(|e| invalid_args(format!("--listen: {e}")))?;
    let issuer = parse_entity_hex(&args.credential_issuer)?;
    let challenge = args
        .acme_challenge_addr
        .as_deref()
        .unwrap_or("0.0.0.0:80")
        .parse()
        .map_err(|e| invalid_args(format!("--acme-challenge-addr: {e}")))?;

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
            let acme = AcmeConfig::new(
                directory.clone(),
                domain,
                args.acme_email.clone().ok_or_else(|| {
                    invalid_args("--acme-email is required with --acme-directory")
                })?,
                // One subdirectory per domain inside this path (R4b).
                args.acme_cache
                    .clone()
                    .unwrap_or_else(|| std::env::temp_dir().join("net-mesh-acme")),
            );
            BootstrapTls::Acme(acme)
        }
        _ => {
            return Err(invalid_args(
                "browser-trusted TLS is required: pass --tls-cert/--tls-key, or \
                 --acme-directory/--acme-email. There is no self-signed mode, because \
                 a browser refuses one",
            ))
        }
    };

    let mut rtc = rtc_config_from_args(args)?;
    // Resolve the documented RTC default once, rather than having inspection
    // invent an address separately from the config consumed by the builder.
    rtc.bind_addr
        .get_or_insert_with(|| std::net::SocketAddr::new(bind.ip(), 0));
    if rtc.stun_public_addr.is_some() && rtc.stun_addr.is_none() {
        return Err(invalid_args(
            "--rtc-stun-public-addr requires --rtc-stun-bind",
        ));
    }
    Ok(ResolvedServe {
        bind,
        listen,
        issuer,
        challenge,
        tls,
        rtc,
    })
}

#[cfg(feature = "rtc-bootstrap")]
struct ResolvedServe {
    bind: std::net::SocketAddr,
    listen: std::net::SocketAddr,
    issuer: net_sdk::identity::EntityId,
    challenge: std::net::SocketAddr,
    tls: net_sdk::rtc_bootstrap::BootstrapTls,
    rtc: net::adapter::net::rtc::RtcConfig,
}

#[cfg(feature = "rtc-bootstrap")]
#[derive(Serialize)]
struct ServeInspection {
    #[serde(flatten)]
    target: crate::target::TargetInspection,
    listen: std::net::SocketAddr,
    rtc_bind: Option<std::net::SocketAddr>,
    rtc_public_addr: Option<std::net::SocketAddr>,
    rtc_stun_bind: Option<std::net::SocketAddr>,
    rtc_stun_public_addr: Option<std::net::SocketAddr>,
    tls: &'static str,
    tls_cert: Option<PathBuf>,
    tls_key: Option<PathBuf>,
    acme_cache: Option<PathBuf>,
    acme_challenge_bind: Option<std::net::SocketAddr>,
    credential_issuer_fingerprint: String,
}

#[cfg(feature = "rtc-bootstrap")]
async fn run_serve(
    args: ServeArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    use net_sdk::rtc_bootstrap::{serve_bootstrap, AcmeState, BootstrapConfig, BootstrapTls};
    use net_sdk::Mesh;
    let resolved = resolve_serve(&args)?;
    if args.inspect_target {
        let profile = crate::context::resolve_profile(config_path, profile_name).await?;
        let mut target = crate::target::TargetInspection::standalone_service(
            &profile,
            resolved.bind.to_string(),
        );
        for (name, explicit) in [
            ("bind", args.bind.is_some()),
            ("listen", args.listen.is_some()),
            ("rtc_bind", args.rtc_bind.is_some()),
        ] {
            target.provenance(name, if explicit { "flag" } else { "default" });
        }
        target.provenance("psk", "flag");
        target.provenance("credential_issuer", "flag");
        target.provenance("tls", "flag");
        target.provenance(
            "rtc_public_addr",
            if args.rtc_public_addr.is_some() {
                "flag"
            } else {
                "runtime"
            },
        );
        target.provenance(
            "rtc_stun_bind",
            if args.rtc_stun_bind.is_some() {
                "flag"
            } else {
                "unused"
            },
        );
        let (tls, tls_cert, tls_key, acme_cache) = match &resolved.tls {
            BootstrapTls::Operator { cert_pem, key_pem } => (
                "operator",
                Some(cert_pem.clone()),
                Some(key_pem.clone()),
                None,
            ),
            BootstrapTls::Acme(config) => ("acme", None, None, Some(config.cache_dir.clone())),
        };
        for (name, explicit) in [
            ("acme_cache", args.acme_cache.is_some()),
            ("acme_challenge_bind", args.acme_challenge_addr.is_some()),
        ] {
            target.provenance(
                name,
                if tls != "acme" {
                    "unused"
                } else if explicit {
                    "flag"
                } else {
                    "default"
                },
            );
        }
        return emit_value(
            OutputFormat::resolve_oneshot(output),
            &ServeInspection {
                target,
                listen: resolved.listen,
                rtc_bind: resolved.rtc.bind_addr,
                rtc_public_addr: resolved.rtc.public_addr,
                rtc_stun_bind: resolved.rtc.stun_addr,
                rtc_stun_public_addr: resolved.rtc.stun_public_addr,
                tls,
                tls_cert,
                tls_key,
                acme_cache,
                acme_challenge_bind: if tls == "acme" {
                    Some(resolved.challenge)
                } else {
                    None
                },
                credential_issuer_fingerprint: crate::target::public_fingerprint(
                    resolved.issuer.as_bytes(),
                ),
            },
        )
        .map_err(|e| generic(format!("write anchor target inspection: {e}")));
    }
    let psk_hex = tokio::fs::read_to_string(&args.psk_file)
        .await
        .map_err(|e| invalid_args(format!("--psk-file {}: {e}", args.psk_file.display())))?;
    let psk = hex_decode_32(psk_hex.trim()).map_err(|e| invalid_args(format!("psk: {e}")))?;

    let mesh = Mesh::builder(&resolved.bind.to_string(), &psk)
        .map_err(|e| generic(format!("mesh builder: {e}")))?
        .rtc(resolved.rtc)
        .build()
        .await
        .map_err(|e| generic(format!("starting the anchor: {e}")))?;
    mesh.start();

    let sdk_psk = Psk::new(psk);
    let mut listener_config = BootstrapConfig::new(
        resolved.listen,
        sdk_psk.clone(),
        resolved.issuer,
        resolved.tls,
        args.allow_origin
            .first()
            .cloned()
            .unwrap_or_else(|| args.url.clone()),
    );
    listener_config.allowed_origins = args.allow_origin.clone();
    listener_config.ws_allowed_origins = args.allow_origin.clone();
    listener_config.acme = AcmeState::new();
    listener_config.acme_challenge_addr = resolved.challenge;
    if let Some(limit) = args.offers_per_minute {
        listener_config.offers_per_ip_per_minute = limit;
    }

    // R6: operator tooling reads the anchors THIS node has ingested.
    let _directory = net_sdk::rtc_bootstrap::serve_anchor_directory(&mesh)
        .map_err(|e| generic(format!("serving the anchor directory: {e}")))?;
    // Stage 6 slice 5: and the anchor's own ICE attempt ledger, for
    // the same reason the directory is a service — the ledger is not
    // fold state, so only the node that owns it can answer for it.
    let _ice_stats = net_sdk::rtc_bootstrap::serve_anchor_ice_stats(&mesh)
        .map_err(|e| generic(format!("serving the anchor ICE stats: {e}")))?;
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
            // **The resolved endpoints, read off the node.** These
            // used to echo the two flags back, which made the
            // report agree with the operator's intent rather than
            // with what the anchor serves: `--rtc-stun-public-addr`
            // without `--rtc-stun-bind` printed an endpoint no
            // socket answered on. The node's accessors are the
            // same ones the signed announcement is built from, so
            // the report and the announcement cannot disagree.
            rtc_addr: mesh.node().rtc_public_addr().map(|a| a.to_string()),
            rtc_stun_addr: mesh.node().rtc_public_stun_addr().map(|a| a.to_string()),
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

/// Stage 6 — the operator surface of the separately announced STUN
/// endpoint: the two flags that configure it, and the JSON that
/// reports it.
///
/// Behind `rtc-bootstrap` because `ServeArgs` is: the verbs that
/// announce and read the endpoint only exist in that build.
/// Run: `cargo test -p net-cli --features rtc-bootstrap anchor`.
#[cfg(all(test, feature = "rtc-bootstrap"))]
mod tests {
    use super::*;

    /// `ServeArgs` parsed from argv, with only the four flags the
    /// parser requires supplied. `extra` is the thing under test.
    fn serve_args(extra: &[&str]) -> ServeArgs {
        use clap::Parser;

        /// `ServeArgs` is a flattened `Args`, so it needs a
        /// `Parser` root to be parsed standalone.
        #[derive(Parser)]
        struct Root {
            #[command(flatten)]
            serve: ServeArgs,
        }

        let mut argv = vec![
            "net-mesh",
            "--psk-file",
            "psk.hex",
            "--url",
            "https://anchor.example.com",
            "--credential-issuer",
            "00",
            "--allow-origin",
            "https://app.example.com",
        ];
        argv.extend_from_slice(extra);
        Root::parse_from(argv).serve
    }

    /// The two STUN flags land on the two `RtcConfig` fields that
    /// open and announce the second socket — and an anchor that
    /// passes neither configures nothing, which is what keeps the
    /// endpoint off unless an operator asks for it.
    ///
    /// The `serve_stun` assertion is not incidental: the new socket
    /// is ADDITIONAL. The RTC socket keeps answering the diagnostic
    /// `UdpBlocked` probe aimed at `rtc_addr`, so a change that
    /// moved STUN duty to the second socket would break the probe's
    /// classification while every assertion about the new fields
    /// still passed.
    ///
    /// Inverse receipt: drop either `if let` in
    /// [`rtc_config_from_args`] and the matching `Some` assertion
    /// fails — the flag parses, prints in `--help`, and reaches
    /// nothing, which is the failure mode an operator cannot see.
    #[test]
    fn the_stun_flags_reach_the_rtc_config_and_are_absent_by_default() {
        let configured = rtc_config_from_args(&serve_args(&[
            "--rtc-stun-bind",
            "0.0.0.0:3479",
            "--rtc-stun-public-addr",
            "203.0.113.7:3479",
        ]))
        .expect("both flags parse");
        assert_eq!(
            configured.stun_addr,
            Some("0.0.0.0:3479".parse().expect("addr")),
            "--rtc-stun-bind is the SECOND socket's bind"
        );
        assert_eq!(
            configured.stun_public_addr,
            Some("203.0.113.7:3479".parse().expect("addr")),
            "--rtc-stun-public-addr is the endpoint that gets announced"
        );

        let bare = rtc_config_from_args(&serve_args(&[])).expect("no STUN flags parse");
        assert_eq!(
            bare.stun_addr, None,
            "no flag, no second socket: an anchor that configures nothing announces \
             nothing"
        );
        assert_eq!(bare.stun_public_addr, None);
        assert!(
            bare.serve_stun,
            "the RTC socket still answers STUN — it is the diagnostic UdpBlocked \
             probe's target, and the second socket is additional to it, never a \
             replacement"
        );

        // The ICE pair is untouched by the STUN pair, and vice
        // versa: four flags, four fields, no aliasing.
        let both_pairs = rtc_config_from_args(&serve_args(&[
            "--rtc-bind",
            "0.0.0.0:7101",
            "--rtc-public-addr",
            "203.0.113.7:7101",
            "--rtc-stun-bind",
            "0.0.0.0:3479",
        ]))
        .expect("all four parse");
        assert_eq!(
            both_pairs.bind_addr,
            Some("0.0.0.0:7101".parse().expect("addr"))
        );
        assert_eq!(
            both_pairs.public_addr,
            Some("203.0.113.7:7101".parse().expect("addr"))
        );
        assert_eq!(
            both_pairs.stun_addr,
            Some("0.0.0.0:3479".parse().expect("addr"))
        );
        assert_eq!(
            both_pairs.stun_public_addr, None,
            "a bind without a public address announces the socket's own resolved \
             endpoint, decided in the driver — not here"
        );
    }

    /// An unparseable address is refused by the flag that carried
    /// it, named, with the invalid-args exit code — not silently
    /// dropped into an anchor that then announces nothing.
    #[test]
    fn a_malformed_stun_address_names_the_flag_that_carried_it() {
        for (flag, value) in [
            ("--rtc-stun-bind", "not-an-address"),
            ("--rtc-stun-public-addr", "203.0.113.7"),
        ] {
            let err = rtc_config_from_args(&serve_args(&[flag, value]))
                .expect_err("a malformed address must be refused");
            assert_eq!(err.kind(), crate::error::ExitCodeKind::InvalidArgs);
            assert!(
                err.to_string().contains(flag),
                "the refusal must name the flag the operator typed: {err}"
            );
        }
    }

    /// The listing row omits the STUN endpoint entirely when the
    /// anchor announced none: a row for one of the anchors that has
    /// not opted in is byte-identical to the row this command
    /// printed before Stage 6.
    ///
    /// Inverse receipt: remove the `skip_serializing_if` and the
    /// first assertion fails with `"rtc_stun_addr": null` in the
    /// row — a key every consumer of `anchor ls` would now have to
    /// account for to describe an anchor that has nothing to do
    /// with STUN.
    #[test]
    fn the_listing_row_omits_the_stun_endpoint_unless_the_anchor_announced_one() {
        let row = AnchorRow {
            node: "0x1".to_string(),
            rtc_addr: Some("203.0.113.7:7101".to_string()),
            rtc_stun_addr: None,
            rtc_bootstrap: Some("https://anchor.example.com".to_string()),
            noise_pubkey: None,
        };
        assert_eq!(
            serde_json::to_string(&row).expect("serialize"),
            r#"{"node":"0x1","rtc_addr":"203.0.113.7:7101","rtc_bootstrap":"https://anchor.example.com","noise_pubkey":null}"#,
            "the row an anchor without a STUN endpoint produces, key for key"
        );

        let announced = AnchorRow {
            rtc_stun_addr: Some("203.0.113.7:3479".to_string()),
            ..row
        };
        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&announced).expect("serialize"))
                .expect("parse");
        assert_eq!(
            json["rtc_stun_addr"], "203.0.113.7:3479",
            "and the announced endpoint travels under its own key, beside rtc_addr"
        );
        assert_eq!(
            json["rtc_addr"], "203.0.113.7:7101",
            "which is a different endpoint, and stays one"
        );
    }

    /// `serve`'s report echoes the PUBLIC STUN address the operator
    /// configured, and omits the key when there was none.
    ///
    /// **Deliberately asymmetric**, and pinned here so the
    /// asymmetry is a decision rather than a drift: `rtc_addr`
    /// still renders `null` when unset because it did before this
    /// field existed and a consumer parsing this report is
    /// entitled to the shape it has. The new key is absent
    /// instead.
    #[test]
    fn the_serve_report_echoes_the_announced_stun_endpoint_and_omits_the_key_without_one() {
        let base = ServeReport {
            node: "0x1".to_string(),
            listening_on: "0.0.0.0:8443".to_string(),
            bootstrap_url: "https://anchor.example.com".to_string(),
            rtc_addr: None,
            rtc_stun_addr: None,
            trust_domain: "td".to_string(),
            noise_pubkey: "ab".to_string(),
        };
        let json = serde_json::to_string(&base).expect("serialize");
        assert!(
            !json.contains("rtc_stun_addr"),
            "an anchor that configured no STUN endpoint reports no such key: {json}"
        );
        assert!(
            json.contains(r#""rtc_addr":null"#),
            "…while its sibling keeps the shape it already had, null and all: {json}"
        );

        let announced = ServeReport {
            rtc_addr: Some("203.0.113.7:7101".to_string()),
            rtc_stun_addr: Some("203.0.113.7:3479".to_string()),
            ..base
        };
        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&announced).expect("serialize"))
                .expect("parse");
        assert_eq!(json["rtc_stun_addr"], "203.0.113.7:3479");
        assert_eq!(
            json["rtc_addr"], "203.0.113.7:7101",
            "the two endpoints are reported separately because they ARE separate"
        );
    }
}
