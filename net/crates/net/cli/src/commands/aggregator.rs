//! `net aggregator (inspect|query|ls|spawn|scale)` — operator
//! surface for the substrate's `AggregatorDaemon` state and the
//! `AggregatorRegistry` that holds live groups.
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
//! `ls` enumerates every group registered on the local node's
//! `AggregatorRegistry` with per-replica health + generation.
//! Reads through `DeckClient::aggregator_registry_snapshot` —
//! when the CLI runs against a process that doesn't host a
//! registry, output is empty (same convention as `inspect`).
//!
//! `spawn` and `scale` parse + validate args today but return a
//! typed "needs daemon process" error: the one-shot CLI can't
//! host the long-running aggregator loop. The error includes
//! the parsed arguments so operators see what would be passed.
//! These verbs flip to live once an aggregator-daemon binary +
//! registry-RPC surface land.
//!
//! Phase C of `SCALING_SUBNET_SPEC.md`. Direction B / step 5 of
//! `AGGREGATOR_LIFECYCLE_DEFERRED_2026_05_23.md`.

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
    /// List aggregator groups registered on the local node's
    /// `AggregatorRegistry`. Output includes per-replica health
    /// + generation. Empty when no registry is installed.
    Ls(LsArgs),
    /// \[preview\] Spawn a new aggregator group. Validates args
    /// then errors — needs a daemon process to host the live
    /// group + registry.
    Spawn(SpawnArgs),
    /// \[preview\] Resize an existing group. Validates args
    /// then errors — needs registry-RPC plumbing.
    Scale(ScaleArgs),
}

#[derive(Args, Debug)]
pub struct InspectArgs {
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
pub struct SpawnArgs {
    /// Operator-chosen group name. Must be unique within the
    /// target node's registry.
    #[arg(long)]
    pub name: String,
    /// Number of replicas to spawn. `1..=255`.
    #[arg(long)]
    pub replica_count: u8,
    /// `SubnetId` the aggregator summarizes — accepts dotted
    /// notation (e.g. `3.7`) or decimal.
    #[arg(long)]
    pub source_subnet: String,
    /// 32-byte group seed as hex (64 chars). Optional — when
    /// omitted, the CLI would derive one from `name`. (Today
    /// `spawn` errors before reaching the derivation, so the
    /// flag is parse-only.)
    #[arg(long)]
    pub group_seed: Option<String>,

    #[arg(long)]
    pub identity: Option<PathBuf>,

    #[arg(long, default_value_t = crate::prelude::DEFAULT_SUPERVISOR_NODE)]
    pub node: u64,
}

#[derive(Args, Debug)]
pub struct ScaleArgs {
    /// Group name to resize.
    #[arg(long)]
    pub name: String,
    /// Target replica count. `1..=255`.
    #[arg(long)]
    pub replica_count: u8,

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
        AggregatorCommand::Ls(args) => run_ls(args, output, config_path, profile_name).await,
        AggregatorCommand::Spawn(args) => run_spawn(args, output, config_path, profile_name).await,
        AggregatorCommand::Scale(args) => run_scale(args, output, config_path, profile_name).await,
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

async fn run_ls(
    args: LsArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    let profile = resolve_profile(config_path, profile_name).await?;
    let ctx = CliContext::build(&profile, args.identity.as_deref(), args.node, false).await?;
    let deck = ctx.deck();
    let snapshot = deck.aggregator_registry_snapshot().await;
    let view = match snapshot {
        Some(s) => LsView {
            registry_installed: true,
            group_count: s.groups.len() as u64,
            groups: s.groups.iter().map(LsGroupRow::from).collect(),
        },
        None => LsView {
            registry_installed: false,
            group_count: 0,
            groups: Vec::new(),
        },
    };
    emit_value(OutputFormat::resolve_oneshot(output), &view)
        .map_err(|e| generic(format!("write aggregator ls: {e}")))?;
    Ok(())
}

async fn run_spawn(
    args: SpawnArgs,
    _output: Option<OutputFormat>,
    _config_path: Option<&std::path::Path>,
    _profile_name: &str,
) -> Result<(), CliError> {
    // Validate inputs up-front so the operator sees concrete
    // parse errors rather than a generic "not supported."
    if args.replica_count == 0 {
        return Err(invalid_args("replica_count must be > 0"));
    }
    // SubnetId parse — accepts "global" or dotted u8 levels.
    use std::str::FromStr;
    net_sdk::subnets::SubnetId::from_str(&args.source_subnet)
        .map_err(|e| invalid_args(format!("source_subnet `{}`: {e}", args.source_subnet)))?;
    // group_seed parse if provided.
    if let Some(ref seed) = args.group_seed {
        let trimmed = seed.trim_start_matches("0x");
        let bytes = hex_decode_32(trimmed)
            .map_err(|e| invalid_args(format!("group_seed `{seed}`: {e}")))?;
        let _: [u8; 32] = bytes;
    }
    Err(invalid_args(format!(
        "net aggregator spawn is parse-only today: args validated (name={}, \
         replica_count={}, source_subnet={}) but the substrate call path \
         needs a daemon process hosting an AggregatorRegistry + a registry-RPC \
         surface so the CLI can invoke `register` remotely. Tracked in \
         AGGREGATOR_LIFECYCLE_DEFERRED_2026_05_23.md.",
        args.name, args.replica_count, args.source_subnet,
    )))
}

async fn run_scale(
    args: ScaleArgs,
    _output: Option<OutputFormat>,
    _config_path: Option<&std::path::Path>,
    _profile_name: &str,
) -> Result<(), CliError> {
    if args.replica_count == 0 {
        return Err(invalid_args("replica_count must be > 0"));
    }
    Err(invalid_args(format!(
        "net aggregator scale is parse-only today: args validated (name={}, \
         replica_count={}) but the substrate path needs the same daemon + \
         registry-RPC plumbing as spawn. Tracked in \
         AGGREGATOR_LIFECYCLE_DEFERRED_2026_05_23.md.",
        args.name, args.replica_count,
    )))
}

/// Hex-decode 32 bytes from a 64-char string. Used by
/// `spawn --group-seed`.
fn hex_decode_32(s: &str) -> Result<[u8; 32], String> {
    if s.len() != 64 {
        return Err(format!("expected 64 hex chars, got {}", s.len()));
    }
    let mut out = [0u8; 32];
    for i in 0..32 {
        let byte_str = &s[i * 2..i * 2 + 2];
        out[i] = u8::from_str_radix(byte_str, 16)
            .map_err(|e| format!("invalid hex at byte {i}: {e}"))?;
    }
    Ok(out)
}

#[derive(Serialize)]
struct LsView {
    /// `true` when the deck has an `AggregatorRegistry` wired
    /// in. Most operator CLI invocations see `false` today —
    /// aggregator daemons run in separate processes, not the CLI.
    registry_installed: bool,
    /// Convenience — `groups.len()` rendered as its own field.
    group_count: u64,
    groups: Vec<LsGroupRow>,
}

#[derive(Serialize)]
struct LsGroupRow {
    name: String,
    /// 64-char hex rendering of `group_seed`.
    group_seed: String,
    /// Per-replica rows in declaration order.
    replicas: Vec<LsReplicaRow>,
    /// Summary counter — how many replicas are healthy now.
    healthy_count: u64,
    /// Summary counter — total replicas in the group.
    replica_count: u64,
}

impl From<&net_sdk::deck::AggregatorRegistryGroupSnapshot> for LsGroupRow {
    fn from(g: &net_sdk::deck::AggregatorRegistryGroupSnapshot) -> Self {
        let replicas: Vec<LsReplicaRow> = g.replicas.iter().map(LsReplicaRow::from).collect();
        let healthy_count = replicas.iter().filter(|r| r.healthy).count() as u64;
        Self {
            name: g.name.clone(),
            group_seed: g
                .group_seed
                .iter()
                .fold(String::with_capacity(64), |mut acc, b| {
                    use std::fmt::Write as _;
                    let _ = write!(&mut acc, "{b:02x}");
                    acc
                }),
            replica_count: g.replicas.len() as u64,
            healthy_count,
            replicas,
        }
    }
}

#[derive(Serialize)]
struct LsReplicaRow {
    generation: u64,
    healthy: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    diagnostic: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    placement_node_id: Option<u64>,
}

impl From<&net_sdk::deck::AggregatorReplicaRow> for LsReplicaRow {
    fn from(r: &net_sdk::deck::AggregatorReplicaRow) -> Self {
        Self {
            generation: r.generation,
            healthy: r.healthy,
            diagnostic: r.diagnostic.clone(),
            placement_node_id: r.placement_node_id,
        }
    }
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
