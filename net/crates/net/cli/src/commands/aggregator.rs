//! `net aggregator (inspect|query)` — operator surface for
//! the substrate's `AggregatorDaemon` state.
//!
//! `inspect` reads the **local** aggregator's state via the
//! `DeckClient::aggregator_*` accessors (populated when an
//! aggregator is wired into the deck via
//! `DeckClient::with_aggregator`). When no aggregator is
//! installed, output is the natural "not installed" shape —
//! same convention as `subnet show` / `gateway stats`.
//!
//! `query` issues a `fold.query` RPC against a **remote**
//! aggregator and prints the response. Wraps the substrate's
//! `FoldQueryClient` with operator-friendly flag plumbing
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

use clap::{Args, Subcommand};
use serde::Serialize;

use crate::context::{resolve_profile, CliContext};
use crate::error::{generic, invalid_args, CliError};
use crate::parsers::{parse_u16_flexible, parse_u64_flexible};
use crate::prelude::{emit_value, OutputFormat};

#[derive(Subcommand, Debug)]
pub enum AggregatorCommand {
    /// Show the local aggregator's state (source subnet, fold
    /// kinds, generation, summary cadence, recent summaries).
    Inspect(InspectArgs),
    /// \[preview\] Issue a `fold.query` RPC against a remote
    /// aggregator. Today this validates flags then errors —
    /// the substrate call path needs a MeshNode wired into the
    /// deck, which the read-only CLI doesn't carry yet.
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
    let view = match deck.aggregator_snapshot() {
        Some(snap) => InspectView {
            aggregator_installed: true,
            source_subnet: Some(snap.source_subnet.to_string()),
            fold_kinds: snap
                .fold_kinds
                .iter()
                .map(|k| format!("{k:#06x}"))
                .collect(),
            summary_interval_secs: snap.summary_interval.as_secs_f64(),
            generation: snap.generation,
            summary_count: snap.summaries.len() as u64,
            summaries: snap
                .summaries
                .iter()
                .cloned()
                .map(SummaryRow::from)
                .collect(),
        },
        None => InspectView {
            aggregator_installed: false,
            source_subnet: None,
            fold_kinds: Vec::new(),
            summary_interval_secs: 0.0,
            generation: 0,
            summary_count: 0,
            summaries: Vec::new(),
        },
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
    let _target = parse_u64_flexible(&args.target)
        .map_err(|e| invalid_args(format!("target `{}`: {e}", args.target)))?;
    let _kind = parse_u16_flexible(&args.kind)
        .map_err(|e| invalid_args(format!("kind `{}`: {e}", args.kind)))?;
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
