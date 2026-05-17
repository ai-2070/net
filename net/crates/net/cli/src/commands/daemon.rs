//! `net daemon (ls|run|shutdown|log)` — daemon authoring +
//! observation.
//!
//! Phase 1: `ls` (snapshot-driven, ships today).
//!
//! Phase 4 design stubs (intentionally not wired into the clap
//! router):
//!
//! - **`net daemon run --kind <FACTORY-ID>`** — needs a
//!   `net_daemon_factories::register!` macro inventory that
//!   doesn't exist today. The plan (§4 "Daemon authoring
//!   on-ramp") pins the shape: a downstream crate registers
//!   daemon factories under string ids; the CLI iterates the
//!   inventory at startup and dispatches `run --kind <id>` to
//!   the matching factory's `Box<dyn MeshDaemon>` constructor.
//!   The runtime then drives the lifecycle (register → run
//!   process loop → graceful shutdown on Ctrl-C / control
//!   event).
//!
//! - **`net daemon shutdown <ID>`** — wraps
//!   `MeshOsDaemonHandle::graceful_shutdown(grace)`. Blocked on
//!   the same `MeshOsRuntime::mesh()` accessor work the
//!   peer/port/rpc subcommands wait on, because the supervisor
//!   needs to be addressable across processes (the in-process
//!   supervisor doesn't share state across `net daemon run`
//!   and `net daemon shutdown` invocations).
//!
//! - **`net daemon log [--daemon <ID>]`** — already covered by
//!   `net log tail --daemon <ID>` from Phase 1. Phase 4 adds
//!   the `daemon log` alias for consumer ergonomics.

use std::path::PathBuf;

use clap::Args;
use net_sdk::deck::DaemonSnapshot;
use serde::Serialize;

use crate::context::{resolve_profile, CliContext};
use crate::error::{generic, CliError};
use crate::prelude::{emit_value, OutputFormat};

#[derive(Args, Debug)]
pub struct LsArgs {
    #[arg(long)]
    pub identity: Option<PathBuf>,

    #[arg(long, default_value_t = crate::prelude::DEFAULT_SUPERVISOR_NODE)]
    pub node: u64,
}

pub async fn run_ls(
    args: LsArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    let profile = resolve_profile(config_path, profile_name).await?;
    let ctx = CliContext::build(&profile, args.identity.as_deref(), args.node, false).await?;
    let snapshot = ctx.deck().status();
    let rows: Vec<DaemonRow> = snapshot
        .daemons
        .iter()
        .map(|(id, d)| DaemonRow {
            id: *id,
            snapshot: d.clone(),
        })
        .collect();
    emit_value(OutputFormat::resolve_oneshot(output), &rows)
        .map_err(|e| generic(format!("write daemon ls: {e}")))?;
    Ok(())
}

// `serde(flatten)` relies on `DaemonSnapshot` not exposing its own
// `id` field. If the SDK ever adds one, rename this wrapper's `id`
// to `daemon_id` (and update consumer scripts) — serde silently
// allows duplicate keys with last-write-wins.
#[derive(Serialize)]
struct DaemonRow {
    id: u64,
    #[serde(flatten)]
    snapshot: DaemonSnapshot,
}
