//! `net-mesh snapshot (get|status)` — one-shot reads of the live
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
//!
//! # Why these require `--local`
//!
//! The Deck client is built from a `MeshOsDaemonSdk` that
//! [`CliContext::build`] starts *during this invocation*. There is no
//! attach path: `build_with_remote` gives the context an `Arc<MeshNode>`
//! for typed RPC clients, but `DeckClient::from_runtime` still reads the
//! local runtime. So a snapshot read can only ever describe a supervisor
//! this process just created, which is empty by construction.
//!
//! Without a flag, that produced the worst possible outcome: plausible
//! snapshot JSON, exit 0, and the only warning on stderr about an
//! ephemeral identity — so the reader concludes the cluster is healthy
//! and idle rather than that nothing was inspected. `--local` makes the
//! choice explicit; the default now says what is missing.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use net_sdk::deck::{MeshOsSnapshot, StatusSummary};
use serde::Serialize;

use crate::context::{resolve_profile, CliContext};
use crate::error::{generic, invalid_args, CliError};
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
    #[arg(long, default_value_t = crate::prelude::DEFAULT_SUPERVISOR_NODE)]
    pub node: u64,

    /// Report the in-process supervisor this command starts, rather than a
    /// running deployment.
    ///
    /// Required, because that is the only thing this command can read. The
    /// result is a fresh, empty snapshot — useful for checking the output
    /// shape or in tests, and not a picture of any cluster.
    #[arg(long)]
    pub local: bool,
}

#[derive(Args, Debug)]
pub struct StatusArgs {
    #[arg(long)]
    pub identity: Option<PathBuf>,

    #[arg(long, default_value_t = crate::prelude::DEFAULT_SUPERVISOR_NODE)]
    pub node: u64,

    /// Report the in-process supervisor this command starts, rather than a
    /// running deployment. See `net-mesh snapshot get --help`.
    #[arg(long)]
    pub local: bool,
}

/// Refuse to present a freshly created runtime as a deployment read.
///
/// Returns exit code 2 (invalid args), because this is a usage problem: the
/// operator asked a question this command cannot answer, and the honest
/// answer is to say so rather than to serialize an empty struct.
fn require_explicit_local(local: bool, verb: &str) -> Result<(), CliError> {
    if local {
        // Still say it, so a `--local` result piped into a report is not
        // mistaken later for a cluster observation.
        tracing::warn!(
            "--local: reporting the in-process supervisor started by this \
             command. It has no peers, daemons or replicas because it was \
             created a moment ago; this is not a view of a running deployment."
        );
        return Ok(());
    }

    Err(invalid_args(format!(
        "`snapshot {verb}` cannot read a running deployment. The Deck client \
         is built from an in-process supervisor started by this command, so \
         the only snapshot it can produce is of a runtime created moments \
         ago — empty by construction, and indistinguishable from a healthy \
         idle cluster. Pass --local to inspect that fresh runtime on purpose \
         (output shape, smoke tests). To observe a real node, use a surface \
         that attaches to one: `net-mesh aggregator`, `net-mesh peer`, or \
         net-deck."
    )))
}

pub async fn run(
    cmd: SnapshotCommand,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    match cmd {
        SnapshotCommand::Get(args) => {
            require_explicit_local(args.local, "get")?;
            let profile = resolve_profile(config_path, profile_name).await?;
            let ctx =
                CliContext::build(&profile, args.identity.as_deref(), args.node, false).await?;
            let snapshot: MeshOsSnapshot = ctx.deck().status();
            emit_value(OutputFormat::resolve_oneshot(output), &snapshot)
                .map_err(|e| generic(format!("write snapshot: {e}")))?;
        }
        SnapshotCommand::Status(args) => {
            require_explicit_local(args.local, "status")?;
            let profile = resolve_profile(config_path, profile_name).await?;
            let ctx =
                CliContext::build(&profile, args.identity.as_deref(), args.node, false).await?;
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
/// `net-mesh snapshot status --output json | jq` reads the same
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
