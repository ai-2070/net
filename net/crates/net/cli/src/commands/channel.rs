//! `net channel (visibility|ls)` — surface the local mesh's
//! `ChannelConfigRegistry` to operators.
//!
//! `visibility <name>` — look up a single channel's
//! [`Visibility`] config (`SubnetLocal` / `ParentVisible` /
//! `Exported` / `Global`).
//!
//! `ls` — enumerate every registered channel as
//! `(name, visibility)` rows for inventory.
//!
//! When no `MeshNode` is attached to the `DeckClient`, both
//! commands report sensible empties. Shape pinned in
//! `SCALING_SUBNET_SPEC.md` Phase A.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use net_sdk::subnets::Visibility;
use serde::Serialize;

use crate::context::{resolve_profile, CliContext};
use crate::error::{generic, CliError};
use crate::prelude::{emit_value, OutputFormat};

#[derive(Subcommand, Debug)]
pub enum ChannelCommand {
    /// Show a single channel's `Visibility` config.
    Visibility(VisibilityArgs),
    /// List every registered channel.
    Ls(LsArgs),
}

#[derive(Args, Debug)]
pub struct VisibilityArgs {
    /// Channel name (canonical, exact match — falls back through
    /// the registry's prefix table via `get_by_name`).
    pub channel: String,

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

pub async fn run(
    cmd: ChannelCommand,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    match cmd {
        ChannelCommand::Visibility(args) => {
            run_visibility(args, output, config_path, profile_name).await
        }
        ChannelCommand::Ls(args) => run_ls(args, output, config_path, profile_name).await,
    }
}

async fn run_visibility(
    args: VisibilityArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    let profile = resolve_profile(config_path, profile_name).await?;
    let ctx = CliContext::build(&profile, args.identity.as_deref(), args.node, false).await?;
    let deck = ctx.deck();
    let view = VisibilityView {
        channel: args.channel.clone(),
        visibility: deck.channel_visibility(&args.channel).map(visibility_str),
        wire_hash: deck
            .channel_wire_hash(&args.channel)
            .map(|h| format!("{h:#06x}")),
        canonical_hash: deck
            .channel_canonical_hash(&args.channel)
            .map(|h| format!("{h:#018x}")),
    };
    emit_value(OutputFormat::resolve_oneshot(output), &view)
        .map_err(|e| generic(format!("write channel visibility: {e}")))?;
    Ok(())
}

async fn run_ls(
    args: LsArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    let profile = resolve_profile(config_path, profile_name).await?;
    let ctx = CliContext::build(&profile, args.identity.as_deref(), args.node, false).await?;
    let deck = ctx.deck();
    let rows: Vec<ChannelRow> = deck
        .channels()
        .into_iter()
        .map(|(name, vis)| ChannelRow {
            channel: name,
            visibility: visibility_str(vis),
        })
        .collect();
    emit_value(OutputFormat::resolve_oneshot(output), &rows)
        .map_err(|e| generic(format!("write channel ls: {e}")))?;
    Ok(())
}

/// Stable lowercase string representation for the four
/// `Visibility` variants. Output is operator-facing and pinned
/// against external scripts; do NOT switch to Display unless its
/// rendering is also pinned.
fn visibility_str(vis: Visibility) -> String {
    match vis {
        Visibility::SubnetLocal => "subnet-local",
        Visibility::ParentVisible => "parent-visible",
        Visibility::Exported => "exported",
        Visibility::Global => "global",
    }
    .to_string()
}

#[derive(Serialize)]
struct VisibilityView {
    channel: String,
    /// `Some("global"|"parent-visible"|"subnet-local"|"exported")`
    /// when the channel is registered, `None` when it isn't (or
    /// no registry is installed).
    visibility: Option<String>,
    /// Wire `u16` hash that rides the packet header — formatted
    /// `0x____` for consistency with `gateway exports` output.
    wire_hash: Option<String>,
    /// Canonical `u64` hash that keys ACL + fold lookups.
    canonical_hash: Option<String>,
}

#[derive(Serialize)]
struct ChannelRow {
    channel: String,
    visibility: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visibility_str_round_trips_all_four_variants() {
        assert_eq!(visibility_str(Visibility::SubnetLocal), "subnet-local");
        assert_eq!(visibility_str(Visibility::ParentVisible), "parent-visible");
        assert_eq!(visibility_str(Visibility::Exported), "exported");
        assert_eq!(visibility_str(Visibility::Global), "global");
    }
}
