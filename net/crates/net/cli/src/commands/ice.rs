//! `net ice <verb>` — break-glass ICE surface.
//!
//! Every verb runs the full simulate → preview → confirm →
//! commit workflow per `NET_CLI_PLAN.md §3 ICE preview workflow`:
//!
//! 1. Construct an `IceProposal` via the SDK factory.
//! 2. Call `simulate()` → get a `BlastRadius`.
//! 3. Render the blast radius as a preview (table on TTY, JSON
//!    elsewhere).
//! 4. TTY: prompt for literal `YES` confirmation; non-TTY:
//!    require `--yes`.
//! 5. `--dry-run` short-circuits before the prompt and exits 0
//!    with the envelope on stdout.
//! 6. Commit; emit the resulting `ChainCommit` on stdout.
//!
//! Operator signatures: with the in-process supervisor's
//! default `ice_signature_threshold = 1`, the local operator
//! key signs the proposal in-process and the commit goes
//! through. The Phase 3 surface accepts pre-collected signatures
//! via `--sig <JSON>` for multi-op coordination workflows.
//!
//! Exit-code mapping:
//! - `SimulationRequired` → code 4.
//! - Verifier rejection (every `VerifyError` variant) → code 5.
//! - Operator declined confirmation → code 8.

use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::time::Duration;

use clap::{Args, Subcommand};
use net_sdk::deck::{AvoidScope, BlastRadius, ChainCommit, DaemonRef, OperatorSignature};
use serde::Serialize;

use crate::context::{resolve_profile, CliContext};
use crate::error::{generic, sdk, CliError, CliError as _CE, ExitCodeKind};
use crate::prelude::{emit_value, OutputFormat};

#[derive(Subcommand, Debug)]
pub enum IceCommand {
    /// Freeze every operator action cluster-wide for `--ttl`.
    FreezeCluster(FreezeArgs),
    /// Lift an in-effect cluster freeze.
    ThawCluster(BareArgs),
    /// Flush avoid-list entries under the chosen scope.
    FlushAvoidLists(FlushArgs),
    /// Force-evict `<VICTIM>` from `<CHAIN>` bypassing the
    /// scheduler's rebalance cooldown.
    ForceEvictReplica(ForceEvictArgs),
    /// Force-restart a specific daemon.
    ForceRestartDaemon(ForceRestartArgs),
    /// Force-cutover `<CHAIN>` to `<TARGET>` bypassing the
    /// scheduler.
    ForceCutover(ForceCutoverArgs),
    /// Abort an in-flight migration.
    KillMigration(KillMigrationArgs),
}

#[derive(Args, Debug)]
pub struct FreezeArgs {
    /// Freeze duration (`5m`, `1h`).
    #[arg(long, value_parser = crate::humantime::parse_duration)]
    pub ttl: Duration,
    #[command(flatten)]
    pub common: CommonIceArgs,
}

#[derive(Args, Debug)]
pub struct BareArgs {
    #[command(flatten)]
    pub common: CommonIceArgs,
}

#[derive(Args, Debug)]
pub struct FlushArgs {
    /// Scope: `global`, `local:<NODE>`, or `on-peer:<PEER>`.
    #[arg(long)]
    pub scope: String,
    #[command(flatten)]
    pub common: CommonIceArgs,
}

#[derive(Args, Debug)]
pub struct ForceEvictArgs {
    /// Chain id (decimal or `0x`-prefixed hex).
    #[arg(value_parser = parse_u64_flexible)]
    pub chain: u64,
    /// Victim node id (decimal or `0x`-prefixed hex).
    #[arg(value_parser = parse_u64_flexible)]
    pub victim: u64,
    #[command(flatten)]
    pub common: CommonIceArgs,
}

#[derive(Args, Debug)]
pub struct ForceRestartArgs {
    /// Daemon id (decimal or `0x`-prefixed hex).
    #[arg(value_parser = parse_u64_flexible)]
    pub daemon_id: u64,
    /// Daemon name (`MeshDaemon::name()`).
    #[arg(long)]
    pub name: String,
    #[command(flatten)]
    pub common: CommonIceArgs,
}

#[derive(Args, Debug)]
pub struct ForceCutoverArgs {
    #[arg(value_parser = parse_u64_flexible)]
    pub chain: u64,
    #[arg(value_parser = parse_u64_flexible)]
    pub target: u64,
    #[command(flatten)]
    pub common: CommonIceArgs,
}

#[derive(Args, Debug)]
pub struct KillMigrationArgs {
    /// Migration id (decimal or `0x`-prefixed hex).
    #[arg(value_parser = parse_u64_flexible)]
    pub migration: u64,
    #[command(flatten)]
    pub common: CommonIceArgs,
}

#[derive(Args, Debug)]
pub struct CommonIceArgs {
    /// Build the envelope + simulate, print the blast radius,
    /// do NOT commit. Exits 0 regardless of operator approval.
    #[arg(long)]
    pub dry_run: bool,

    /// Skip the interactive `YES` prompt. Required when stdout
    /// is non-TTY; rejected on TTY unless explicitly passed.
    #[arg(long)]
    pub yes: bool,

    /// Pre-collected operator signatures, one per `--sig`. Each
    /// argument is a JSON object: `{"operator_id": <u64>,
    /// "signature_hex": "<128 hex chars>"}`. The local operator
    /// always signs in addition to the supplied signatures.
    #[arg(long = "sig", num_args = 0..)]
    pub sigs: Vec<String>,

    /// Operator identity file.
    #[arg(long)]
    pub identity: Option<PathBuf>,

    /// Substrate node id for the in-process supervisor.
    #[arg(long, default_value_t = 0x0001)]
    pub supervisor_node: u64,
}

fn parse_u64_flexible(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(rest, 16).map_err(|e| format!("invalid hex: {e}"))
    } else {
        s.parse::<u64>()
            .map_err(|e| format!("invalid integer: {e}"))
    }
}

fn parse_scope(s: &str) -> Result<AvoidScope, CliError> {
    if s == "global" {
        return Ok(AvoidScope::Global);
    }
    if let Some(rest) = s.strip_prefix("local:") {
        let node = parse_u64_flexible(rest).map_err(|e| {
            crate::error::invalid_args(format!("invalid `local:<NODE>` scope: {e}"))
        })?;
        return Ok(AvoidScope::Local { node });
    }
    if let Some(rest) = s.strip_prefix("on-peer:") {
        let peer = parse_u64_flexible(rest).map_err(|e| {
            crate::error::invalid_args(format!("invalid `on-peer:<PEER>` scope: {e}"))
        })?;
        return Ok(AvoidScope::OnPeer { peer });
    }
    Err(crate::error::invalid_args(format!(
        "scope must be `global` | `local:<NODE>` | `on-peer:<PEER>`; got {s:?}"
    )))
}

pub async fn run(
    cmd: IceCommand,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    match cmd {
        IceCommand::FreezeCluster(args) => {
            let common = args.common;
            let ttl = args.ttl;
            run_ice(common, output, config_path, profile_name, move |deck| {
                deck.ice().freeze_cluster(ttl)
            })
            .await
        }
        IceCommand::ThawCluster(args) => {
            let common = args.common;
            run_ice(common, output, config_path, profile_name, move |deck| {
                deck.ice().thaw_cluster()
            })
            .await
        }
        IceCommand::FlushAvoidLists(args) => {
            let scope = parse_scope(&args.scope)?;
            let common = args.common;
            run_ice(common, output, config_path, profile_name, move |deck| {
                deck.ice().flush_avoid_lists(scope.clone())
            })
            .await
        }
        IceCommand::ForceEvictReplica(args) => {
            let common = args.common;
            let chain = args.chain;
            let victim = args.victim;
            run_ice(common, output, config_path, profile_name, move |deck| {
                deck.ice().force_evict_replica(chain, victim)
            })
            .await
        }
        IceCommand::ForceRestartDaemon(args) => {
            let common = args.common;
            let daemon = DaemonRef {
                id: args.daemon_id,
                name: args.name.clone(),
            };
            run_ice(common, output, config_path, profile_name, move |deck| {
                deck.ice().force_restart_daemon(daemon.clone())
            })
            .await
        }
        IceCommand::ForceCutover(args) => {
            let common = args.common;
            let chain = args.chain;
            let target = args.target;
            run_ice(common, output, config_path, profile_name, move |deck| {
                deck.ice().force_cutover(chain, target)
            })
            .await
        }
        IceCommand::KillMigration(args) => {
            let common = args.common;
            let migration = args.migration;
            run_ice(common, output, config_path, profile_name, move |deck| {
                deck.ice().kill_migration(migration)
            })
            .await
        }
    }
}

async fn run_ice<F>(
    common: CommonIceArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
    build_proposal: F,
) -> Result<(), CliError>
where
    F: for<'a> FnOnce(&'a net_sdk::deck::DeckClient) -> net_sdk::deck::IceProposal<'a>,
{
    let profile = resolve_profile(config_path, profile_name).await?;
    let ctx =
        CliContext::build(&profile, common.identity.as_deref(), common.supervisor_node).await?;
    let deck = ctx.deck();
    let proposal = build_proposal(deck.as_ref());

    // Simulate — substrate-side enforces "simulate before
    // commit" at the cryptographic layer via the
    // `SimulationRequired` sentinel hash, but the CLI runs it
    // for the operator-visible blast-radius preview.
    let simulated = proposal
        .simulate()
        .await
        .map_err(|e| map_ice_error(&format!("simulate: {e}"), e_kind(&e)))?;
    let blast = simulated.blast_radius().clone();
    let preview = SimulationPreview {
        issued_at_ms: simulated.issued_at_ms(),
        blast_hash: hex::encode(simulated.blast_hash()),
        blast,
    };

    // Render the preview before the confirm gate. JSON for
    // non-TTY (script-friendly); table for TTY would ship in a
    // follow-up — JSON works for both today.
    emit_value(OutputFormat::resolve_oneshot(output), &preview)
        .map_err(|e| generic(format!("write ICE preview: {e}")))?;

    if common.dry_run {
        return Ok(());
    }

    // Confirmation gate. The break-glass surface keeps a dual-key
    // feel: `--yes` only short-circuits the prompt when stdin is
    // not a TTY (scripts / CI). On an interactive terminal we
    // always demand the typed `YES` even with `--yes` so a stray
    // shell-history recall can't ram an ICE commit through.
    let stdin_is_tty = is_terminal::IsTerminal::is_terminal(&io::stdin());
    if !stdin_is_tty {
        if !common.yes {
            return Err(_CE::new(
                ExitCodeKind::ConfirmationRefused,
                "stdin is not a TTY; pass --yes to skip the interactive confirm prompt",
            ));
        }
    } else if !prompt_for_yes()? {
        return Err(crate::error::confirmation_refused());
    }

    // Sign locally + collect supplied signatures.
    let mut signatures: Vec<OperatorSignature> = Vec::new();
    let local_sig = ctx.identity().sign_proposal(
        simulated.action(),
        simulated.issued_at_ms(),
        &simulated.blast_hash(),
    );
    signatures.push(local_sig);
    for raw in &common.sigs {
        signatures.push(parse_supplied_sig(raw)?);
    }

    let commit: ChainCommit = simulated
        .commit(&signatures)
        .await
        .map_err(|e| map_ice_error(&format!("commit: {e}"), e_kind(&e)))?;
    let payload = ChainCommitMirror {
        commit_id: commit.commit_id(),
        operator_id: commit.operator_id(),
        event_kind: commit.event_kind(),
        committed_at_ms: commit
            .committed_at()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
    };
    emit_value(OutputFormat::resolve_oneshot(output), &payload)
        .map_err(|e| generic(format!("write commit: {e}")))?;
    Ok(())
}

fn e_kind(e: &net_sdk::deck::IceError) -> Option<&'static str> {
    Some(e.kind)
}

fn map_ice_error(msg: &str, kind: Option<&'static str>) -> CliError {
    match kind {
        Some("simulation_required") => _CE::new(ExitCodeKind::IceSimulationBlocked, msg),
        Some("not_authorized")
        | Some("signature_invalid")
        | Some("insufficient_signatures")
        | Some("envelope_expired")
        | Some("envelope_from_future")
        | Some("ice_cooldown_active") => _CE::new(ExitCodeKind::OperatorPolicyRejected, msg),
        _ => sdk(msg),
    }
}

fn prompt_for_yes() -> Result<bool, CliError> {
    let mut stdout = io::stdout();
    write!(stdout, "Type YES to confirm ICE commit: ")
        .map_err(|e| generic(format!("prompt write: {e}")))?;
    stdout
        .flush()
        .map_err(|e| generic(format!("prompt flush: {e}")))?;
    let mut line = String::new();
    io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| generic(format!("prompt read: {e}")))?;
    Ok(line.trim() == "YES")
}

fn parse_supplied_sig(raw: &str) -> Result<OperatorSignature, CliError> {
    #[derive(serde::Deserialize)]
    struct SigJson {
        operator_id: u64,
        signature_hex: String,
    }
    let parsed: SigJson = serde_json::from_str(raw)
        .map_err(|e| crate::error::invalid_args(format!("--sig must be JSON object: {e}")))?;
    let bytes = hex::decode(&parsed.signature_hex).map_err(|e| {
        crate::error::invalid_args(format!("--sig signature_hex is not valid hex: {e}"))
    })?;
    if bytes.len() != 64 {
        return Err(crate::error::invalid_args(format!(
            "--sig signature_hex decoded to {} bytes; expected 64",
            bytes.len()
        )));
    }
    Ok(OperatorSignature {
        operator_id: parsed.operator_id,
        signature: bytes,
    })
}

// =========================================================================
// Wire-form mirrors
// =========================================================================

/// JSON shape emitted before the confirm prompt — operator sees
/// the blast radius + the hash they're about to sign over.
#[derive(Serialize)]
struct SimulationPreview {
    issued_at_ms: u64,
    blast_hash: String,
    blast: BlastRadius,
}

#[derive(Serialize)]
struct ChainCommitMirror {
    commit_id: u64,
    operator_id: u64,
    event_kind: &'static str,
    committed_at_ms: u64,
}
