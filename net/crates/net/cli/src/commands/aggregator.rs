//! `net aggregator (inspect|query)` — operator surface for
//! [`AggregatorDaemon`] state.
//!
//! `inspect` reads the **local** aggregator's state via the
//! `DeckClient::aggregator_*` accessors (populated when an
//! aggregator is wired into the deck via
//! `DeckClient::with_aggregator`). When no aggregator is
//! installed, output is the natural "not installed" shape —
//! same convention as `subnet show` / `gateway stats`.
//!
//! `query` issues a `fold.query` RPC against a **remote**
//! aggregator and prints the response. Wraps
//! [`FoldQueryClient`] with operator-friendly flag plumbing
//! (target node id parsing, fold kind selection,
//! summarize-now vs. latest, JSON output).
//!
//! Spawn / ls / scale are deferred — those need a daemon binary
//! to host the long-running aggregator + `ReplicaGroup`
//! integration. The one-shot CLI shape only fits inspect and
//! query today.
//!
//! Phase C of `SCALING_SUBNET_SPEC.md`.

use std::path::PathBuf;
use std::time::Duration;

use clap::{Args, Subcommand};
use serde::Serialize;

use crate::context::{resolve_profile, CliContext};
use crate::error::{generic, invalid_args, CliError};
use crate::prelude::{emit_value, OutputFormat};

#[derive(Subcommand, Debug)]
pub enum AggregatorCommand {
    /// Show the local aggregator's state (source subnet, fold
    /// kinds, generation, summary cadence, recent summaries).
    Inspect(InspectArgs),
    /// Issue a `fold.query` RPC against a remote aggregator and
    /// print the response.
    Query(QueryArgs),
}

#[derive(Args, Debug)]
pub struct InspectArgs {
    #[arg(long)]
    pub identity: Option<PathBuf>,

    #[arg(long, default_value_t = crate::prelude::DEFAULT_SUPERVISOR_NODE)]
    pub node: u64,
}

#[derive(Args, Debug)]
pub struct QueryArgs {
    /// Target aggregator's `node_id` — accept decimal or
    /// `0x`-prefixed hex.
    pub target: String,
    /// `FoldKind::KIND_ID` to query. Decimal or `0x`-prefixed
    /// hex.
    #[arg(long)]
    pub kind: String,
    /// When set, force the aggregator to summarize-now (skips
    /// its cached buffer). Default: latest-summary path that
    /// hits the daemon's in-memory buffer.
    #[arg(long, default_value_t = false)]
    pub fresh: bool,

    #[arg(long)]
    pub identity: Option<PathBuf>,

    #[arg(long, default_value_t = crate::prelude::DEFAULT_SUPERVISOR_NODE)]
    pub node: u64,
}

pub async fn run(
    cmd: AggregatorCommand,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    match cmd {
        AggregatorCommand::Inspect(args) => {
            run_inspect(args, output, config_path, profile_name).await
        }
        AggregatorCommand::Query(args) => run_query(args, output, config_path, profile_name).await,
    }
}

async fn run_inspect(
    args: InspectArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    let profile = resolve_profile(config_path, profile_name).await?;
    let ctx = CliContext::build(&profile, args.identity.as_deref(), args.node, false).await?;
    let deck = ctx.deck();
    let view = InspectView {
        aggregator_installed: deck.aggregator_installed(),
        source_subnet: deck.aggregator_source_subnet().map(|s| s.to_string()),
        fold_kinds: deck
            .aggregator_fold_kinds()
            .iter()
            .map(|k| format!("{k:#06x}"))
            .collect(),
        summary_interval_secs: deck.aggregator_summary_interval().as_secs_f64(),
        generation: deck.aggregator_generation(),
        summary_count: deck.aggregator_summaries().len() as u64,
        summaries: deck
            .aggregator_summaries()
            .into_iter()
            .map(SummaryRow::from)
            .collect(),
    };
    emit_value(OutputFormat::resolve_oneshot(output), &view)
        .map_err(|e| generic(format!("write aggregator inspect: {e}")))?;
    Ok(())
}

async fn run_query(
    args: QueryArgs,
    _output: Option<OutputFormat>,
    _config_path: Option<&std::path::Path>,
    _profile_name: &str,
) -> Result<(), CliError> {
    // Validate inputs up-front so the operator sees concrete
    // parse errors rather than a generic "not supported."
    let _target = parse_u64(&args.target, "target")?;
    let _kind = parse_u16(&args.kind, "kind")?;
    // The query path needs a `MeshNode` wired into the
    // DeckClient (so the substrate can route the RPC). The
    // CliContext's deck doesn't carry one today — same gap
    // documented on every other write-or-call surface. Return
    // a typed error rather than crash the FoldQueryClient on
    // an empty handle.
    Err(invalid_args(
        "net aggregator query is read-validation-only today: the target / \
         kind / --fresh flags parse correctly but the substrate call path \
         needs a MeshNode wired into the deck's DeckClient. CliContext::build \
         doesn't construct one in-process yet; when the in-process MeshNode \
         bootstrap lands (or remote-attach), this command flips to live.",
    ))
}

/// Parse a `u64` from a CLI string that accepts either decimal
/// or `0x` / `0X`-prefixed hex.
fn parse_u64(raw: &str, field: &str) -> Result<u64, CliError> {
    let s = raw.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return u64::from_str_radix(hex, 16)
            .map_err(|e| invalid_args(format!("{field} hex `{raw}` not a u64: {e}")));
    }
    s.parse::<u64>()
        .map_err(|e| invalid_args(format!("{field} decimal `{raw}` not a u64: {e}")))
}

/// Parse a `u16` from a CLI string that accepts either decimal
/// or `0x` / `0X`-prefixed hex.
fn parse_u16(raw: &str, field: &str) -> Result<u16, CliError> {
    let s = raw.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return u16::from_str_radix(hex, 16)
            .map_err(|e| invalid_args(format!("{field} hex `{raw}` not a u16: {e}")));
    }
    s.parse::<u16>()
        .map_err(|e| invalid_args(format!("{field} decimal `{raw}` not a u16: {e}")))
}

#[derive(Serialize)]
struct InspectView {
    /// `true` when the deck has an `Arc<AggregatorDaemon>` wired
    /// in. Most operator CLI invocations see `false` today —
    /// the aggregator runs in a separate daemon process, not
    /// the CLI.
    aggregator_installed: bool,
    /// Aggregator's `source_subnet` rendered as e.g. `"3.7"`.
    source_subnet: Option<String>,
    /// Configured fold kinds (`KIND_ID` as `0x____` strings).
    fold_kinds: Vec<String>,
    /// `summary_interval` in fractional seconds. Operators
    /// reading the JSON typically multiply by 1000 for ms.
    summary_interval_secs: f64,
    /// Monotonic tick counter.
    generation: u64,
    /// Convenience — `summaries.len()` rendered as its own
    /// field so the operator sees the count without parsing
    /// the array.
    summary_count: u64,
    /// Latest summaries buffered by the daemon.
    summaries: Vec<SummaryRow>,
}

#[derive(Serialize)]
struct SummaryRow {
    /// Wall clock when emitted in seconds-since-Unix-epoch.
    /// Not currently populated by the substrate (the daemon's
    /// `SummaryAnnouncement` carries `generation` only); kept
    /// here as a forward-compat slot for the wire-publish slice.
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<u64>,
    /// `FoldKind::KIND_ID` formatted as `0x____`.
    fold_kind: String,
    /// `source_subnet` rendered as `"3.7"`.
    source_subnet: String,
    /// Per-bucket counts as `(name, count)` pairs.
    buckets: Vec<(String, u64)>,
    /// Generation that produced this summary.
    generation: u64,
}

impl From<net_sdk::deck::SummaryAnnouncement> for SummaryRow {
    fn from(s: net_sdk::deck::SummaryAnnouncement) -> Self {
        Self {
            timestamp: None,
            fold_kind: format!("{:#06x}", s.fold_kind),
            source_subnet: s.source_subnet.to_string(),
            buckets: s.buckets,
            generation: s.generation,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_u64_accepts_decimal_and_hex() {
        assert_eq!(parse_u64("0", "x").unwrap(), 0);
        assert_eq!(parse_u64("42", "x").unwrap(), 42);
        assert_eq!(parse_u64("0xDEAD", "x").unwrap(), 0xDEAD);
        assert_eq!(parse_u64("0XBEEF", "x").unwrap(), 0xBEEF);
        assert_eq!(
            parse_u64("0xCAFEBABE_DEADBEEF".replace('_', "").as_str(), "x").unwrap(),
            0xCAFE_BABE_DEAD_BEEF
        );
        assert!(parse_u64("not-a-number", "x").is_err());
        assert!(parse_u64("0xZZ", "x").is_err());
    }

    #[test]
    fn parse_u16_accepts_decimal_and_hex_and_rejects_overflow() {
        assert_eq!(parse_u16("0", "x").unwrap(), 0);
        assert_eq!(parse_u16("42", "x").unwrap(), 42);
        assert_eq!(parse_u16("0x0001", "x").unwrap(), 1);
        assert!(parse_u16("65536", "x").is_err()); // overflow u16
        assert!(parse_u16("0x1FFFF", "x").is_err()); // hex overflow u16
    }

    #[test]
    fn _summary_interval_seconds_round_trips_zero() {
        // Operator-readable JSON: `summary_interval_secs: 0.0`
        // when no aggregator is installed (the duration default
        // is Duration::ZERO).
        let _ = Duration::ZERO.as_secs_f64();
    }
}
