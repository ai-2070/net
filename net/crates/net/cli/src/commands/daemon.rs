//! `net daemon ls` — list daemons known to the local snapshot.
//!
//! Phase 1 scope: read-only listing from
//! `MeshOsSnapshot::daemons`. `net daemon run` (factory-id
//! based authoring on-ramp) + `net daemon shutdown` + `net
//! daemon log` ship in Phase 4.

use std::path::PathBuf;

use clap::Args;
use serde::Serialize;

use crate::context::{resolve_profile, CliContext};
use crate::error::{generic, CliError};
use crate::prelude::{emit_value, OutputFormat};

#[derive(Args, Debug)]
pub struct LsArgs {
    #[arg(long)]
    pub identity: Option<PathBuf>,

    #[arg(long, default_value_t = 0x0001)]
    pub node: u64,
}

pub async fn run_ls(
    args: LsArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    let profile = resolve_profile(config_path, profile_name).await?;
    let ctx = CliContext::build(&profile, args.identity.as_deref(), args.node).await?;
    let snapshot = ctx.deck().status();
    let rows: Vec<DaemonRow> = snapshot
        .daemons
        .iter()
        .map(|(id, d)| DaemonRow {
            id: *id,
            name: format!("{:?}", d),
        })
        .collect();
    emit_value(OutputFormat::resolve_oneshot(output), &rows)
        .map_err(|e| generic(format!("write daemon ls: {e}")))?;
    Ok(())
}

#[derive(Serialize)]
struct DaemonRow {
    id: u64,
    /// Debug-formatted projection of the substrate's
    /// `DaemonSnapshot`. Typed fields land when we add a
    /// `DaemonSummary` mirror — Phase 1 keeps the surface
    /// lightweight while the schema is exercised.
    name: String,
}
