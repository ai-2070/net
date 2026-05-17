//! `net snapshot (get|status)` — one-shot reads of the live
//! `MeshOsSnapshot` + the typed `StatusSummary`.
//!
//! - `snapshot get` — `client.status()` returns the freshest
//!   `MeshOsSnapshot`. The wire form is large; default output is
//!   JSON, with `--output yaml` for human-friendly inspection.
//! - `snapshot status` — `client.status_summary()` returns the
//!   typed counts struct (peers / daemons / replica chains / …).
//!   Defaults to a table on TTY, JSON on non-TTY.
//!
//! Both are sync substrate reads — no streams, no Ctrl-C
//! cancellation needed.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use net_sdk::deck::{MeshOsSnapshot, StatusSummary};
use serde::Serialize;

use crate::context::{resolve_profile, CliContext};
use crate::error::{generic, CliError};
use crate::prelude::{emit_value, OutputFormat};

#[derive(Subcommand, Debug)]
pub enum SnapshotCommand {
    /// Print the freshest `MeshOsSnapshot` as JSON / YAML.
    Get(GetArgs),
    /// Print the typed `StatusSummary` (peer / daemon counts +
    /// recent failure / audit ring stats).
    Status(StatusArgs),
}

#[derive(Args, Debug)]
pub struct GetArgs {
    /// Operator identity file. Overrides the profile's
    /// `identity` setting.
    #[arg(long)]
    pub identity: Option<PathBuf>,

    /// Substrate node id for the in-process supervisor.
    #[arg(long, default_value_t = 0x0001)]
    pub node: u64,
}

#[derive(Args, Debug)]
pub struct StatusArgs {
    #[arg(long)]
    pub identity: Option<PathBuf>,

    #[arg(long, default_value_t = 0x0001)]
    pub node: u64,
}

pub async fn run(
    cmd: SnapshotCommand,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    match cmd {
        SnapshotCommand::Get(args) => {
            let profile = resolve_profile(config_path, profile_name).await?;
            let ctx = CliContext::build(&profile, args.identity.as_deref(), args.node).await?;
            let snapshot: MeshOsSnapshot = ctx.deck().status();
            emit_value(OutputFormat::resolve_oneshot(output), &snapshot)
                .map_err(|e| generic(format!("write snapshot: {e}")))?;
        }
        SnapshotCommand::Status(args) => {
            let profile = resolve_profile(config_path, profile_name).await?;
            let ctx = CliContext::build(&profile, args.identity.as_deref(), args.node).await?;
            let summary: StatusSummary = ctx.deck().status_summary();
            // `StatusSummary` lives in the substrate without serde
            // derives; copy into the CLI's local serializable mirror
            // (same shape every binding uses — see
            // `bindings/python/src/deck.rs::status_summary_to_dict`).
            let mirror = StatusSummaryMirror::from(&summary);
            emit_value(OutputFormat::resolve_oneshot(output), &mirror)
                .map_err(|e| generic(format!("write status: {e}")))?;
        }
    }
    Ok(())
}

/// Serializable mirror of the substrate's `StatusSummary`.
/// Fields match `bindings/python/src/deck.rs::status_summary_to_dict`
/// — same shape every binding emits, so a script piping
/// `net snapshot status --output json | jq` reads the same
/// envelope as the Python / Node / Go consumers.
#[derive(Serialize)]
struct StatusSummaryMirror {
    peers: PeerCountsMirror,
    daemons: DaemonCountsMirror,
    replica_chains: u64,
    avoid_list_entries: u64,
    recently_emitted_count: u64,
    recent_failure_count: u64,
    admin_audit_ring_depth: u64,
    freeze_remaining_ms: Option<u64>,
    local_maintenance_active: bool,
}

#[derive(Serialize)]
struct PeerCountsMirror {
    healthy: u64,
    degraded: u64,
    unreachable: u64,
    unknown: u64,
}

#[derive(Serialize)]
struct DaemonCountsMirror {
    running: u64,
    starting: u64,
    stopping: u64,
    stopped: u64,
    backing_off: u64,
    crash_looping: u64,
}

impl From<&StatusSummary> for StatusSummaryMirror {
    fn from(s: &StatusSummary) -> Self {
        Self {
            peers: PeerCountsMirror {
                healthy: s.peers.healthy as u64,
                degraded: s.peers.degraded as u64,
                unreachable: s.peers.unreachable as u64,
                unknown: s.peers.unknown as u64,
            },
            daemons: DaemonCountsMirror {
                running: s.daemons.running as u64,
                starting: s.daemons.starting as u64,
                stopping: s.daemons.stopping as u64,
                stopped: s.daemons.stopped as u64,
                backing_off: s.daemons.backing_off as u64,
                crash_looping: s.daemons.crash_looping as u64,
            },
            replica_chains: s.replica_chains as u64,
            avoid_list_entries: s.avoid_list_entries as u64,
            recently_emitted_count: s.recently_emitted_count as u64,
            recent_failure_count: s.recent_failure_count as u64,
            admin_audit_ring_depth: s.admin_audit_ring_depth as u64,
            freeze_remaining_ms: s.freeze_remaining_ms,
            local_maintenance_active: s.local_maintenance_active,
        }
    }
}
