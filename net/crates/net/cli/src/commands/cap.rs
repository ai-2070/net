//! `net cap (show|query|nodes)` — capability advertisement +
//! discovery from the local snapshot.
//!
//! Phase 1 scope: read-only. Reads `DeckClient::status()` and
//! filters the snapshot's per-peer `capability_set`. The
//! `cap announce` writer + the live `ProximityGraph::query`
//! filter path land in Phase 2 once the SDK exposes a `mesh()`
//! accessor on the runtime.

use std::collections::BTreeSet;
use std::path::PathBuf;

use clap::{Args, Subcommand};
use serde::Serialize;

use crate::context::{resolve_profile, CliContext};
use crate::error::{generic, CliError};
use crate::prelude::{emit_value, OutputFormat};

#[derive(Subcommand, Debug)]
pub enum CapCommand {
    /// Show capabilities for the local node (default) or a
    /// specific peer via `--node`.
    Show(ShowArgs),
    /// Find nodes whose advertised capability set contains
    /// every supplied tag.
    Query(QueryArgs),
    /// List every (node, capabilities) tuple known to the local
    /// capability index.
    Nodes(NodesArgs),
}

#[derive(Args, Debug)]
pub struct ShowArgs {
    /// Peer node id. Defaults to the local node configured by
    /// `--node`.
    #[arg(long, value_name = "PEER_NODE")]
    pub peer: Option<u64>,

    #[arg(long)]
    pub identity: Option<PathBuf>,

    #[arg(long, default_value_t = 0x0001)]
    pub node: u64,
}

#[derive(Args, Debug)]
pub struct QueryArgs {
    /// One or more required tags. A node matches when its
    /// advertised capability set contains every tag listed.
    #[arg(long = "tag", required = true, num_args = 1.., value_name = "TAG")]
    pub tags: Vec<String>,

    #[arg(long)]
    pub identity: Option<PathBuf>,

    #[arg(long, default_value_t = 0x0001)]
    pub node: u64,
}

#[derive(Args, Debug)]
pub struct NodesArgs {
    #[arg(long)]
    pub identity: Option<PathBuf>,

    #[arg(long, default_value_t = 0x0001)]
    pub node: u64,
}

pub async fn run(
    cmd: CapCommand,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    match cmd {
        CapCommand::Show(args) => run_show(args, output, config_path, profile_name).await,
        CapCommand::Query(args) => run_query(args, output, config_path, profile_name).await,
        CapCommand::Nodes(args) => run_nodes(args, output, config_path, profile_name).await,
    }
}

async fn run_show(
    args: ShowArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    let profile = resolve_profile(config_path, profile_name).await?;
    let ctx = CliContext::build(&profile, args.identity.as_deref(), args.node, false).await?;
    let snapshot = ctx.deck().status();
    let target = args.peer.unwrap_or(args.node);
    let caps = snapshot
        .peers
        .get(&target)
        .map(|p| p.capability_set.iter().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    let info = CapShow {
        node: target,
        capabilities: caps,
    };
    emit_value(OutputFormat::resolve_oneshot(output), &info)
        .map_err(|e| generic(format!("write cap show: {e}")))?;
    Ok(())
}

async fn run_query(
    args: QueryArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    let profile = resolve_profile(config_path, profile_name).await?;
    let ctx = CliContext::build(&profile, args.identity.as_deref(), args.node, false).await?;
    let snapshot = ctx.deck().status();
    let required: BTreeSet<String> = args.tags.into_iter().collect();
    let matches: Vec<u64> = snapshot
        .peers
        .iter()
        .filter(|(_, p)| required.iter().all(|t| p.capability_set.contains(t)))
        .map(|(id, _)| *id)
        .collect();
    let info = CapQuery {
        required: required.into_iter().collect(),
        matched_nodes: matches,
    };
    emit_value(OutputFormat::resolve_oneshot(output), &info)
        .map_err(|e| generic(format!("write cap query: {e}")))?;
    Ok(())
}

async fn run_nodes(
    args: NodesArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    let profile = resolve_profile(config_path, profile_name).await?;
    let ctx = CliContext::build(&profile, args.identity.as_deref(), args.node, false).await?;
    let snapshot = ctx.deck().status();
    let rows: Vec<CapNodesRow> = snapshot
        .peers
        .iter()
        .map(|(id, p)| CapNodesRow {
            node: *id,
            capabilities: p.capability_set.iter().cloned().collect(),
        })
        .collect();
    emit_value(OutputFormat::resolve_oneshot(output), &rows)
        .map_err(|e| generic(format!("write cap nodes: {e}")))?;
    Ok(())
}

#[derive(Serialize)]
struct CapShow {
    node: u64,
    capabilities: Vec<String>,
}

#[derive(Serialize)]
struct CapQuery {
    required: Vec<String>,
    matched_nodes: Vec<u64>,
}

#[derive(Serialize)]
struct CapNodesRow {
    node: u64,
    capabilities: Vec<String>,
}
