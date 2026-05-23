//! `net gateway (stats|exports|export)` — surface the local
//! mesh node's `SubnetGateway` state.
//!
//! `stats` rolls up `local_subnet`, forwarded/dropped counters,
//! peer-subnet list, and export-rule count into a single
//! [`GatewayStats`] row.
//!
//! `exports` enumerates the gateway's export table as
//! `(channel_hash, channel_name?, target_subnets[])` rows.
//!
//! `export <channel> <target-subnet>...` adds (or replaces) an
//! export rule. The channel argument can be either the canonical
//! name (resolved via `DeckClient::channel_wire_hash`) or a
//! `0x` / decimal `u16` wire-hash literal.
//!
//! Shape pinned in `SCALING_SUBNET_SPEC.md` Phase A.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use net_sdk::deck::GatewayStats;
use net_sdk::subnets::SubnetId;
use serde::Serialize;

use crate::context::{resolve_profile, CliContext};
use crate::error::{generic, invalid_args, CliError};
use crate::prelude::{emit_value, OutputFormat};

#[derive(Subcommand, Debug)]
pub enum GatewayCommand {
    /// Aggregate gateway counters + local subnet + peer-subnet list.
    Stats(StatsArgs),
    /// Enumerate the gateway's export table.
    Exports(ExportsArgs),
    /// Add an explicit export rule for a channel. Currently
    /// reports `not_supported` — gateway export mutation requires
    /// a write-capable mesh handle that the read-only CLI doesn't
    /// own today.
    Export(ExportArgs),
}

#[derive(Args, Debug)]
pub struct StatsArgs {
    #[arg(long)]
    pub identity: Option<PathBuf>,

    #[arg(long, default_value_t = crate::prelude::DEFAULT_SUPERVISOR_NODE)]
    pub node: u64,
}

#[derive(Args, Debug)]
pub struct ExportsArgs {
    #[arg(long)]
    pub identity: Option<PathBuf>,

    #[arg(long, default_value_t = crate::prelude::DEFAULT_SUPERVISOR_NODE)]
    pub node: u64,
}

#[derive(Args, Debug)]
pub struct ExportArgs {
    /// Channel name OR `0x`/decimal `u16` wire hash.
    pub channel: String,
    /// Target subnets to export to. At least one required.
    /// Format: `region.fleet.unit[.subsystem]` (e.g. `3.7.2`) or
    /// `global`.
    #[arg(required = true)]
    pub targets: Vec<String>,

    #[arg(long)]
    pub identity: Option<PathBuf>,

    #[arg(long, default_value_t = crate::prelude::DEFAULT_SUPERVISOR_NODE)]
    pub node: u64,
}

pub async fn run(
    cmd: GatewayCommand,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    match cmd {
        GatewayCommand::Stats(args) => run_stats(args, output, config_path, profile_name).await,
        GatewayCommand::Exports(args) => run_exports(args, output, config_path, profile_name).await,
        GatewayCommand::Export(args) => run_export(args, output, config_path, profile_name).await,
    }
}

async fn run_stats(
    args: StatsArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    let profile = resolve_profile(config_path, profile_name).await?;
    let ctx = CliContext::build(&profile, args.identity.as_deref(), args.node, false).await?;
    let view = match ctx.deck().gateway_stats() {
        Some(stats) => StatsView::installed(&stats),
        None => StatsView::not_installed(),
    };
    emit_value(OutputFormat::resolve_oneshot(output), &view)
        .map_err(|e| generic(format!("write gateway stats: {e}")))?;
    Ok(())
}

async fn run_exports(
    args: ExportsArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    let profile = resolve_profile(config_path, profile_name).await?;
    let ctx = CliContext::build(&profile, args.identity.as_deref(), args.node, false).await?;
    let deck = ctx.deck();
    let rows: Vec<ExportRow> = deck
        .gateway_exports()
        .into_iter()
        .map(|(channel_hash, targets)| ExportRow {
            channel_hash: format!("{channel_hash:#06x}"),
            target_count: targets.len() as u64,
            targets: targets.iter().map(|s| s.to_string()).collect(),
        })
        .collect();
    emit_value(OutputFormat::resolve_oneshot(output), &rows)
        .map_err(|e| generic(format!("write gateway exports: {e}")))?;
    Ok(())
}

async fn run_export(
    args: ExportArgs,
    _output: Option<OutputFormat>,
    _config_path: Option<&std::path::Path>,
    _profile_name: &str,
) -> Result<(), CliError> {
    // Validate inputs up-front so the operator sees concrete
    // parse errors rather than a generic "not supported." The
    // mutate path itself is gated on a write-capable mesh handle
    // that the read-only `DeckClient` doesn't carry today.
    let _channel_hash = parse_channel_hash(&args.channel)?;
    for target in &args.targets {
        let _ = parse_subnet(target)?;
    }
    Err(invalid_args(
        "net gateway export is read-validation-only today: arguments parse but \
         the substrate mutate path requires a write-capable mesh handle that the \
         CLI's read-only DeckClient doesn't own. Set the export rule via the \
         operator daemon's config or the substrate's `SubnetGateway::export_channel` \
         API directly until the write-attach surface lands.",
    ))
}

/// Parse a channel arg as either a `0x` / decimal wire-hash
/// literal, or as a name that the caller must look up against the
/// deck's channel registry. The read-validation-only `export`
/// command only confirms the input shape parses.
fn parse_channel_hash(raw: &str) -> Result<u16, CliError> {
    let s = raw.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return u16::from_str_radix(hex, 16)
            .map_err(|e| invalid_args(format!("channel hex `{raw}` not a u16: {e}")));
    }
    if s.chars().all(|c| c.is_ascii_digit()) {
        return s
            .parse::<u16>()
            .map_err(|e| invalid_args(format!("channel decimal `{raw}` not a u16: {e}")));
    }
    // Treat as a channel name — accepted as long as it isn't empty.
    if s.is_empty() {
        return Err(invalid_args("channel name cannot be empty"));
    }
    // We can't resolve a name → hash without a mesh-attached deck;
    // return 0 as a sentinel for the write path (currently
    // disabled), and let the operator know via the not-supported
    // surface.
    Ok(0)
}

/// Parse a subnet arg into a `SubnetId`. Accepts `global` or a
/// dotted form like `3.7.2`. Levels are u8; each must parse.
fn parse_subnet(raw: &str) -> Result<SubnetId, CliError> {
    let s = raw.trim().to_ascii_lowercase();
    if s == "global" {
        return Ok(SubnetId::GLOBAL);
    }
    let parts: Vec<&str> = s.split('.').collect();
    if parts.is_empty() || parts.len() > SubnetId::MAX_DEPTH as usize {
        return Err(invalid_args(format!(
            "subnet `{raw}` must be 1..={max} dotted u8 levels (got {n}); \
             use `global` for SubnetId::GLOBAL",
            max = SubnetId::MAX_DEPTH,
            n = parts.len()
        )));
    }
    let mut levels: Vec<u8> = Vec::with_capacity(parts.len());
    for p in parts {
        levels.push(
            p.parse::<u8>()
                .map_err(|e| invalid_args(format!("subnet level `{p}` in `{raw}` not a u8: {e}")))?,
        );
    }
    SubnetId::try_new(&levels)
        .map_err(|e| invalid_args(format!("subnet `{raw}` rejected: {e:?}")))
}

#[derive(Serialize)]
struct StatsView {
    /// `false` when no `SubnetGateway` is installed on the local
    /// mesh — happens when `set_channel_configs` hasn't been
    /// called (or the deck has no mesh attached).
    gateway_installed: bool,
    local_subnet: Option<String>,
    forwarded: u64,
    dropped: u64,
    peer_subnet_count: u64,
    peer_subnets: Vec<String>,
    export_rules: u64,
}

impl StatsView {
    fn installed(stats: &GatewayStats) -> Self {
        Self {
            gateway_installed: true,
            local_subnet: Some(stats.local_subnet.to_string()),
            forwarded: stats.forwarded,
            dropped: stats.dropped,
            peer_subnet_count: stats.peer_subnets.len() as u64,
            peer_subnets: stats.peer_subnets.iter().map(|s| s.to_string()).collect(),
            export_rules: stats.export_rules,
        }
    }
    fn not_installed() -> Self {
        Self {
            gateway_installed: false,
            local_subnet: None,
            forwarded: 0,
            dropped: 0,
            peer_subnet_count: 0,
            peer_subnets: Vec::new(),
            export_rules: 0,
        }
    }
}

#[derive(Serialize)]
struct ExportRow {
    channel_hash: String,
    target_count: u64,
    targets: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_subnet_accepts_global_and_dotted_levels() {
        assert_eq!(parse_subnet("global").unwrap(), SubnetId::GLOBAL);
        assert_eq!(parse_subnet("GLOBAL").unwrap(), SubnetId::GLOBAL);
        assert_eq!(parse_subnet("3").unwrap(), SubnetId::new(&[3]));
        assert_eq!(parse_subnet("3.7").unwrap(), SubnetId::new(&[3, 7]));
        assert_eq!(parse_subnet("3.7.2").unwrap(), SubnetId::new(&[3, 7, 2]));
        assert_eq!(parse_subnet("3.7.2.1").unwrap(), SubnetId::new(&[3, 7, 2, 1]));
    }

    #[test]
    fn parse_subnet_rejects_overflow_and_garbage() {
        assert!(parse_subnet("256").is_err()); // u8 overflow
        assert!(parse_subnet("3.7.2.1.0").is_err()); // > MAX_DEPTH
        assert!(parse_subnet("not-a-number").is_err());
        assert!(parse_subnet("").is_err());
    }

    #[test]
    fn parse_channel_hash_accepts_hex_decimal_and_names() {
        assert_eq!(parse_channel_hash("0x42").unwrap(), 0x42);
        assert_eq!(parse_channel_hash("0X42").unwrap(), 0x42);
        assert_eq!(parse_channel_hash("66").unwrap(), 66);
        // Name → sentinel 0 (write path is disabled today).
        assert_eq!(parse_channel_hash("internal/metrics").unwrap(), 0);
        assert!(parse_channel_hash("").is_err());
    }
}
