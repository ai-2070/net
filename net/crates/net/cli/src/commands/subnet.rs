//! `net subnet (show|ls|tree)` — operator-facing inspection of the
//! local mesh node's hierarchical subnet view.
//!
//! All three subcommands route through
//! `net_sdk::deck::DeckClient`'s subnet accessors. When the
//! `DeckClient` doesn't have a `MeshNode` wired in (current
//! [`CliContext::build`] path), the commands return their natural
//! "empty" shape — `show` reports `local_subnet = null`, `ls` and
//! `tree` print empty arrays. That keeps the JSON shape stable
//! against the eventual remote-attach path landing in Phase 5.
//!
//! Shape pinned in `SCALING_SUBNET_SPEC.md` Phase A.

use std::collections::BTreeSet;
use std::path::PathBuf;

use clap::{Args, Subcommand};
use net_sdk::subnets::SubnetId;
use serde::Serialize;

use crate::context::{resolve_profile, CliContext};
use crate::error::{generic, CliError};
use crate::prelude::{emit_value, OutputFormat};

#[derive(Subcommand, Debug)]
pub enum SubnetCommand {
    /// Show this node's `SubnetId` and the policy that derived it.
    Show(ShowArgs),
    /// List every subnet known to this node, with the member
    /// `node_id` set per subnet.
    Ls(LsArgs),
    /// Render the subnet hierarchy as an indented tree.
    Tree(TreeArgs),
}

#[derive(Args, Debug)]
pub struct ShowArgs {
    #[arg(long)]
    pub identity: Option<PathBuf>,

    #[arg(long, default_value_t = crate::prelude::DEFAULT_SUPERVISOR_NODE)]
    pub node: u64,
}

#[derive(Args, Debug)]
pub struct LsArgs {
    #[arg(long)]
    pub identity: Option<PathBuf>,

    #[arg(long, default_value_t = crate::prelude::DEFAULT_SUPERVISOR_NODE)]
    pub node: u64,
}

#[derive(Args, Debug)]
pub struct TreeArgs {
    #[arg(long)]
    pub identity: Option<PathBuf>,

    #[arg(long, default_value_t = crate::prelude::DEFAULT_SUPERVISOR_NODE)]
    pub node: u64,
}

pub async fn run(
    cmd: SubnetCommand,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    match cmd {
        SubnetCommand::Show(args) => run_show(args, output, config_path, profile_name).await,
        SubnetCommand::Ls(args) => run_ls(args, output, config_path, profile_name).await,
        SubnetCommand::Tree(args) => run_tree(args, output, config_path, profile_name).await,
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
    let deck = ctx.deck();
    let view = ShowView {
        local_subnet: deck.local_subnet().map(format_subnet),
        depth: deck.local_subnet().map(|s| s.depth()),
        known_peer_count: deck.known_subnets().len() as u64,
    };
    emit_value(OutputFormat::resolve_oneshot(output), &view)
        .map_err(|e| generic(format!("write subnet show: {e}")))?;
    Ok(())
}

async fn run_ls(
    args: LsArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    let profile = resolve_profile(config_path, profile_name).await?;
    let local_node_id = args.node;
    let ctx = CliContext::build(&profile, args.identity.as_deref(), local_node_id, false).await?;
    let deck = ctx.deck();
    // The deck handles the bucket-by-subnet grouping so the deck
    // SUBNETS tab and this CLI surface stay in sync. Pass the
    // local node id so the local subnet's row carries it as a
    // member (the substrate's `cfg.this_node` uses the same value).
    let rows: Vec<SubnetRow> = deck
        .subnets_with_members(Some(local_node_id))
        .into_iter()
        .map(|r| SubnetRow {
            subnet: format_subnet(r.subnet),
            depth: r.subnet.depth(),
            member_count: r.members.len() as u64,
            members: r.members,
        })
        .collect();
    emit_value(OutputFormat::resolve_oneshot(output), &rows)
        .map_err(|e| generic(format!("write subnet ls: {e}")))?;
    Ok(())
}

async fn run_tree(
    args: TreeArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    let profile = resolve_profile(config_path, profile_name).await?;
    let ctx = CliContext::build(&profile, args.identity.as_deref(), args.node, false).await?;
    let deck = ctx.deck();
    let mut all_subnets: BTreeSet<u32> = BTreeSet::new();
    if let Some(local) = deck.local_subnet() {
        all_subnets.insert(local.raw());
    }
    for (_node_id, subnet) in deck.known_subnets() {
        all_subnets.insert(subnet.raw());
    }
    // For every subnet, also include every ancestor — so a tree
    // render shows the full path even when only deep subnets have
    // members.
    let mut closure: BTreeSet<u32> = BTreeSet::new();
    for &raw in &all_subnets {
        let mut cursor = SubnetId::from_raw(raw);
        loop {
            closure.insert(cursor.raw());
            if cursor.is_global() {
                break;
            }
            cursor = cursor.parent();
        }
    }
    // Convert to depth-then-raw-sorted rendering.
    let mut nodes: Vec<SubnetId> = closure.into_iter().map(SubnetId::from_raw).collect();
    nodes.sort_by_key(|s| (s.depth(), s.raw()));
    let rows: Vec<TreeRow> = nodes
        .into_iter()
        .map(|s| TreeRow {
            subnet: format_subnet(s),
            depth: s.depth(),
            parent: if s.is_global() {
                None
            } else {
                Some(format_subnet(s.parent()))
            },
            is_local: deck.local_subnet() == Some(s),
        })
        .collect();
    emit_value(OutputFormat::resolve_oneshot(output), &rows)
        .map_err(|e| generic(format!("write subnet tree: {e}")))?;
    Ok(())
}

/// Render a `SubnetId` for operator-facing output. Stable string
/// that round-trips through human inspection (e.g. `"3.7.2"` for
/// `SubnetId::new(&[3, 7, 2])`, `"global"` for `SubnetId::GLOBAL`).
fn format_subnet(subnet: SubnetId) -> String {
    subnet.to_string()
}

#[derive(Serialize)]
struct ShowView {
    /// `Some("3.7.2")` when a mesh is attached, `None` otherwise.
    local_subnet: Option<String>,
    /// Subnet hierarchy depth (0 for `SubnetId::GLOBAL`).
    depth: Option<u8>,
    /// How many peers this node has cached a subnet for. Reflects
    /// `MeshNode::known_subnets().len()`.
    known_peer_count: u64,
}

#[derive(Serialize)]
struct SubnetRow {
    subnet: String,
    depth: u8,
    member_count: u64,
    members: Vec<u64>,
}

#[derive(Serialize)]
struct TreeRow {
    subnet: String,
    depth: u8,
    /// `None` for `SubnetId::GLOBAL`; otherwise the parent
    /// subnet's rendered form.
    parent: Option<String>,
    /// `true` when this row matches the local node's `SubnetId`.
    is_local: bool,
}
