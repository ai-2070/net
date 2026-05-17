//! `net admin <verb>` — signed admin-chain commits.
//!
//! Wraps the SDK's `AdminCommands` (`DeckClient::admin()`). Every
//! verb constructs the corresponding `AdminEvent`, publishes it
//! through the substrate's admin-commit path, and emits the
//! resulting `ChainCommit` as JSON on stdout. `--dry-run` prints
//! the envelope that would be committed and exits 0 without
//! committing.
//!
//! Phase 2 of `NET_CLI_PLAN.md`. Same in-process supervisor
//! context as the Phase 1 reads.

use std::path::PathBuf;
use std::time::{Duration, UNIX_EPOCH};

use clap::{Args, Subcommand};
use net_sdk::deck::ChainCommit;
use serde::Serialize;

use crate::context::{resolve_profile, CliContext};
use crate::error::{generic, sdk, CliError};
use crate::parsers::parse_u64_flexible;
use crate::prelude::{emit_value, OutputFormat};

#[derive(Subcommand, Debug)]
pub enum AdminCommand {
    /// Drain `node` for `--drain-for` duration. Replicas
    /// migrate, daemons enter graceful shutdown.
    Drain(DrainArgs),
    /// Begin a maintenance window for `node`.
    EnterMaintenance(EnterMaintenanceArgs),
    /// End an active maintenance window for `node`.
    ExitMaintenance(NodeArgs),
    /// Mark `node` ineligible for new placements.
    Cordon(NodeArgs),
    /// Remove a prior cordon on `node`.
    Uncordon(NodeArgs),
    /// Drop the listed replica chains from `node`.
    DropReplicas(DropReplicasArgs),
    /// Force a placement recompute for `node`.
    InvalidatePlacement(NodeArgs),
    /// Force-restart every daemon on `node`.
    RestartAllDaemons(NodeArgs),
    /// Clear `node`'s local avoid list.
    ClearAvoidList(NodeArgs),
}

// Re-used by every node-only verb.
#[derive(Args, Debug)]
pub struct NodeArgs {
    /// Target node id. Accepts decimal (`12345`) or hex (`0xABCD`).
    #[arg(value_parser = parse_u64_flexible)]
    pub node: u64,

    #[command(flatten)]
    pub common: CommonAdminArgs,
}

#[derive(Args, Debug)]
pub struct DrainArgs {
    #[arg(value_parser = parse_u64_flexible)]
    pub node: u64,

    /// Drain window duration (`30s`, `5m`, `1h`).
    #[arg(long, value_parser = crate::humantime::parse_duration)]
    pub drain_for: Duration,

    #[command(flatten)]
    pub common: CommonAdminArgs,
}

#[derive(Args, Debug)]
pub struct EnterMaintenanceArgs {
    #[arg(value_parser = parse_u64_flexible)]
    pub node: u64,

    /// Optional drain window. `None` defers to the cluster's
    /// configured default.
    #[arg(long, value_parser = crate::humantime::parse_duration)]
    pub drain_for: Option<Duration>,

    #[command(flatten)]
    pub common: CommonAdminArgs,
}

#[derive(Args, Debug)]
pub struct DropReplicasArgs {
    #[arg(value_parser = parse_u64_flexible)]
    pub node: u64,

    /// Chain ids to drop. Pass `--chain` repeatedly or as a
    /// space-separated list. Accepts decimal or `0x`-prefixed
    /// hex.
    #[arg(long = "chain", required = true, num_args = 1.., value_name = "CHAIN_ID", value_parser = parse_u64_flexible)]
    pub chains: Vec<u64>,

    #[command(flatten)]
    pub common: CommonAdminArgs,
}

#[derive(Args, Debug)]
pub struct CommonAdminArgs {
    /// Build the envelope, print it, do NOT commit.
    #[arg(long)]
    pub dry_run: bool,

    /// Operator identity file. Overrides the profile's
    /// `identity` setting.
    #[arg(long)]
    pub identity: Option<PathBuf>,

    /// Substrate node id for the in-process supervisor.
    #[arg(long, default_value_t = crate::prelude::DEFAULT_SUPERVISOR_NODE)]
    pub supervisor_node: u64,
}

pub async fn run(
    cmd: AdminCommand,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    match cmd {
        AdminCommand::Drain(args) => {
            handle(
                args.common,
                output,
                config_path,
                profile_name,
                AdminEnvelope::Drain {
                    node: args.node,
                    drain_for_ms: u64::try_from(args.drain_for.as_millis()).unwrap_or(u64::MAX),
                },
                |deck, supervisor_node| {
                    let drain_for = args.drain_for;
                    async move {
                        let _ = supervisor_node;
                        deck.admin().drain(args.node, drain_for).await
                    }
                },
            )
            .await
        }
        AdminCommand::EnterMaintenance(args) => {
            handle(
                args.common,
                output,
                config_path,
                profile_name,
                AdminEnvelope::EnterMaintenance {
                    node: args.node,
                    drain_for_ms: args.drain_for.map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)),
                },
                |deck, _| {
                    let drain_for = args.drain_for;
                    async move { deck.admin().enter_maintenance(args.node, drain_for).await }
                },
            )
            .await
        }
        AdminCommand::ExitMaintenance(args) => {
            handle(
                args.common,
                output,
                config_path,
                profile_name,
                AdminEnvelope::ExitMaintenance { node: args.node },
                |deck, _| async move { deck.admin().exit_maintenance(args.node).await },
            )
            .await
        }
        AdminCommand::Cordon(args) => {
            handle(
                args.common,
                output,
                config_path,
                profile_name,
                AdminEnvelope::Cordon { node: args.node },
                |deck, _| async move { deck.admin().cordon(args.node).await },
            )
            .await
        }
        AdminCommand::Uncordon(args) => {
            handle(
                args.common,
                output,
                config_path,
                profile_name,
                AdminEnvelope::Uncordon { node: args.node },
                |deck, _| async move { deck.admin().uncordon(args.node).await },
            )
            .await
        }
        AdminCommand::DropReplicas(args) => handle(
            args.common,
            output,
            config_path,
            profile_name,
            AdminEnvelope::DropReplicas {
                node: args.node,
                chains: args.chains.clone(),
            },
            move |deck, _| async move { deck.admin().drop_replicas(args.node, args.chains).await },
        )
        .await,
        AdminCommand::InvalidatePlacement(args) => {
            handle(
                args.common,
                output,
                config_path,
                profile_name,
                AdminEnvelope::InvalidatePlacement { node: args.node },
                |deck, _| async move { deck.admin().invalidate_placement(args.node).await },
            )
            .await
        }
        AdminCommand::RestartAllDaemons(args) => {
            handle(
                args.common,
                output,
                config_path,
                profile_name,
                AdminEnvelope::RestartAllDaemons { node: args.node },
                |deck, _| async move { deck.admin().restart_all_daemons(args.node).await },
            )
            .await
        }
        AdminCommand::ClearAvoidList(args) => {
            handle(
                args.common,
                output,
                config_path,
                profile_name,
                AdminEnvelope::ClearAvoidList { node: args.node },
                |deck, _| async move { deck.admin().clear_avoid_list(args.node).await },
            )
            .await
        }
    }
}

async fn handle<F, Fut>(
    common: CommonAdminArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
    envelope: AdminEnvelope,
    commit: F,
) -> Result<(), CliError>
where
    F: FnOnce(std::sync::Arc<net_sdk::deck::DeckClient>, u64) -> Fut,
    Fut: std::future::Future<Output = std::result::Result<ChainCommit, net_sdk::deck::AdminError>>,
{
    if common.dry_run {
        let preview = DryRunPreview {
            dry_run: true,
            envelope,
        };
        emit_value(OutputFormat::resolve_oneshot(output), &preview)
            .map_err(|e| generic(format!("write dry-run preview: {e}")))?;
        return Ok(());
    }

    let profile = resolve_profile(config_path, profile_name).await?;
    let ctx = CliContext::build(
        &profile,
        common.identity.as_deref(),
        common.supervisor_node,
        true,
    )
    .await?;
    let deck = ctx.deck();
    let commit = commit(deck, common.supervisor_node)
        .await
        .map_err(|e| sdk(format!("admin commit failed: {e}")))?;
    let payload = ChainCommitMirror::from(&commit);
    emit_value(OutputFormat::resolve_oneshot(output), &payload)
        .map_err(|e| generic(format!("write commit: {e}")))?;
    Ok(())
}

// =========================================================================
// Wire-form mirrors
// =========================================================================

/// Serializable mirror of `ChainCommit`. The substrate type
/// doesn't derive serde (it carries an `Instant`-like
/// `SystemTime` field); we project to `committed_at_ms` so the
/// output stays portable.
#[derive(Serialize)]
struct ChainCommitMirror {
    commit_id: u64,
    operator_id: u64,
    event_kind: &'static str,
    committed_at_ms: u64,
}

impl From<&ChainCommit> for ChainCommitMirror {
    fn from(c: &ChainCommit) -> Self {
        let committed_at_ms = c
            .committed_at()
            .duration_since(UNIX_EPOCH)
            .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0);
        Self {
            commit_id: c.commit_id(),
            operator_id: c.operator_id(),
            event_kind: c.event_kind(),
            committed_at_ms,
        }
    }
}

/// Used by `--dry-run`. Same envelope shape as `ChainCommit`
/// would carry, but with a `dry_run: true` flag and the
/// pre-commit `AdminEvent` payload instead of the substrate's
/// post-commit metadata.
#[derive(Serialize)]
struct DryRunPreview {
    dry_run: bool,
    envelope: AdminEnvelope,
}

/// Tagged-union projection of the `AdminEvent` variants the CLI
/// emits. Same shape every binding's audit query reads back.
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum AdminEnvelope {
    Drain {
        node: u64,
        drain_for_ms: u64,
    },
    EnterMaintenance {
        node: u64,
        drain_for_ms: Option<u64>,
    },
    ExitMaintenance {
        node: u64,
    },
    Cordon {
        node: u64,
    },
    Uncordon {
        node: u64,
    },
    DropReplicas {
        node: u64,
        chains: Vec<u64>,
    },
    InvalidatePlacement {
        node: u64,
    },
    RestartAllDaemons {
        node: u64,
    },
    ClearAvoidList {
        node: u64,
    },
}
