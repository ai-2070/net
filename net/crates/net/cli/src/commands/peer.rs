//! `net peer ls` — list peers known to the local snapshot.
//!
//! Phase 1 scope: read-only ls only. `peer reflex` / `peer nat` /
//! `peer reclassify-nat` / `peer set-reflex` / `peer clear-reflex`
//! all need direct `MeshAdapter` access (`peer_reflex_addr`,
//! `nat_type`, `reflex_addr`, `reclassify_nat`,
//! `set_reflex_override`, `clear_reflex_override`) which the
//! SDK doesn't expose on `MeshOsRuntime` today. Those land in a
//! follow-up Phase-1 commit that adds a `mesh()` accessor to
//! the SDK runtime surface.

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
    let rows: Vec<PeerRow> = snapshot
        .peers
        .iter()
        .map(|(id, p)| PeerRow {
            node: *id,
            rtt_ms: p.rtt_ms,
            health: p.health.map(|h| format!("{h:?}")),
            maintenance: p.maintenance.map(|m| format!("{m:?}")),
            cpu_load_1m: p.cpu_load_1m,
            mem_used_bytes: p.mem_used_bytes,
            mem_total_bytes: p.mem_total_bytes,
            software_version: p.software_version.clone(),
            capability_count: p.capability_set.len() as u64,
        })
        .collect();
    emit_value(OutputFormat::resolve_oneshot(output), &rows)
        .map_err(|e| generic(format!("write peer ls: {e}")))?;
    Ok(())
}

#[derive(Serialize)]
struct PeerRow {
    node: u64,
    rtt_ms: Option<u64>,
    health: Option<String>,
    maintenance: Option<String>,
    cpu_load_1m: Option<f64>,
    mem_used_bytes: Option<u64>,
    mem_total_bytes: Option<u64>,
    software_version: Option<String>,
    capability_count: u64,
}
