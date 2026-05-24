//! `net aggregator (inspect|query|ls|spawn|scale)` — operator
//! surface for the substrate's `AggregatorDaemon` state and the
//! `AggregatorRegistry` that holds live groups.
//!
//! Verb shapes:
//!
//! - `inspect` — local. Reads the in-process aggregator's state
//!   via `DeckClient::aggregator_*` accessors. Empty output when
//!   no aggregator is installed (same convention as
//!   `subnet show` / `gateway stats`).
//! - `ls` — local by default; remote when `--node-addr` is set or
//!   `--remote` is passed. Local reads through
//!   `DeckClient::aggregator_registry_snapshot`; remote routes
//!   through `RegistryClient::list`.
//! - `query` — remote-only. Issues a `fold.query` RPC against
//!   the target via `FoldQueryClient`. `--fresh` switches to the
//!   cache-bypass `SummarizeNow` path.
//! - `spawn` — remote-only. Calls `RegistryClient::spawn` with
//!   the daemon-side template name + operator-chosen group name.
//! - `scale` — remote-only. Calls `RegistryClient::scale` for
//!   in-place grow/shrink; surviving replicas keep their
//!   identity + generation across the resize.
//!
//! All write/RPC verbs (`query`, `spawn`, `scale`, `ls --remote`)
//! require remote-attach flags — `--node-addr`, `--node-pubkey`,
//! `--node-id`, `--psk-hex`, each of which can default from the
//! profile.
//!
//! Phase C of `SCALING_SUBNET_SPEC.md`. Direction B / step 5 of
//! `AGGREGATOR_LIFECYCLE_DEFERRED_2026_05_23.md`. Remote-attach +
//! Scale RPC wiring lands per
//! `AGGREGATOR_CLI_REMOTE_ATTACH_AND_SCALE_RPC.md`.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use serde::Serialize;

use crate::context::{resolve_profile, resolve_remote_attach, CliContext, RemoteAttach};
use crate::error::{generic, invalid_args, sdk, CliError};
use crate::parsers::{parse_u16_flexible, parse_u64_flexible};
use crate::prelude::{emit_value, OutputFormat};

/// Flags every aggregator verb accepts for remote-attach. Each
/// is also resolvable from the profile (`node_addr` / `node_pubkey`
/// / `node_id` / `psk_hex`) — the CLI flag wins when both are
/// set. Resolution is centralised in
/// [`crate::context::resolve_remote_attach`]; when every field is
/// `None` and the profile has no defaults, the subcommand runs
/// in-process.
#[derive(Args, Debug, Default)]
pub struct RemoteAttachArgs {
    /// Remote daemon `IP:port`. Operators copy this from the
    /// daemon's `--print-bootstrap` output.
    #[arg(long, value_parser = crate::parsers::parse_socket_addr_string)]
    pub node_addr: Option<String>,
    /// Remote daemon's Noise public key (64 hex chars, optional
    /// `0x` prefix).
    #[arg(long, value_parser = crate::parsers::parse_hex32_string)]
    pub node_pubkey: Option<String>,
    /// Remote daemon's `node_id` (decimal or `0x`-prefixed hex).
    #[arg(long = "node-id", value_parser = crate::parsers::parse_u64_flexible_string)]
    pub remote_node_id: Option<String>,
    /// 32-byte pre-shared key as hex. Required when handshaking
    /// with a remote daemon; profile-level `psk_hex` covers the
    /// common case.
    #[arg(long, value_parser = crate::parsers::parse_hex32_string)]
    pub psk_hex: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum AggregatorCommand {
    /// Show the local aggregator's state (source subnet, fold
    /// kinds, generation, summary cadence, recent summaries).
    Inspect(InspectArgs),
    /// Issue a `fold.query` RPC against a remote aggregator.
    /// Requires `--node-addr` + `--node-pubkey` + `--node-id` +
    /// `--psk-hex` (or the matching profile fields). Output is
    /// the buffered `SummaryAnnouncement` list from the target
    /// aggregator; pass `--fresh` to force a `SummarizeNow` tick
    /// instead of the latest cached buffer.
    Query(QueryArgs),
    /// List aggregator groups registered on the local node's
    /// `AggregatorRegistry`. Output includes per-replica health
    /// + generation. Empty when no registry is installed.
    Ls(LsArgs),
    /// Spawn a new aggregator group on a remote daemon by
    /// referencing one of its operator-configured `[[template]]`
    /// blocks. The daemon resolves the template, builds the
    /// group with the operator-chosen `--name`, and returns its
    /// initial snapshot. Requires the same remote-attach flags
    /// as `query`.
    Spawn(SpawnArgs),
    /// Resize an existing aggregator group on a remote daemon
    /// to the supplied `--replica-count`. Today implemented as
    /// Unregister + Spawn (interim — `B-5` flips to the
    /// dedicated `Scale` RPC once the substrate helper lands).
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

    /// When set (or implicit via `--node-addr`), route `ls`
    /// through the registry RPC against the remote daemon
    /// rather than reading the local registry snapshot. Wired
    /// in A-5.
    #[arg(long, default_value_t = false)]
    pub remote: bool,

    #[command(flatten)]
    pub attach: RemoteAttachArgs,
}

#[derive(Args, Debug)]
pub struct SpawnArgs {
    /// Operator-chosen group name. Must be unique within the
    /// target node's registry.
    #[arg(long)]
    pub name: String,
    /// Daemon-side template (a `[[template]]` block in the
    /// daemon's config) to instantiate. The template owns
    /// `source_subnet` + `fold_kinds` + cadence; the operator
    /// only picks how many replicas and what to call the group.
    #[arg(long)]
    pub template: String,
    /// Number of replicas to spawn. `1..=255`.
    #[arg(long)]
    pub replica_count: u8,

    #[arg(long)]
    pub identity: Option<PathBuf>,

    #[arg(long, default_value_t = crate::prelude::DEFAULT_SUPERVISOR_NODE)]
    pub node: u64,

    #[command(flatten)]
    pub attach: RemoteAttachArgs,
}

#[derive(Args, Debug)]
pub struct ScaleArgs {
    /// Group name to resize.
    #[arg(long)]
    pub name: String,
    /// Daemon-side template the group was spawned from. The
    /// interim path needs this to re-spawn after unregistering;
    /// B-5 drops the requirement when the dedicated `Scale` RPC
    /// lands and the daemon can look the spec up by group name.
    #[arg(long)]
    pub template: String,
    /// Target replica count. `1..=255`.
    #[arg(long)]
    pub replica_count: u8,

    #[arg(long)]
    pub identity: Option<PathBuf>,

    #[arg(long, default_value_t = crate::prelude::DEFAULT_SUPERVISOR_NODE)]
    pub node: u64,

    #[command(flatten)]
    pub attach: RemoteAttachArgs,
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

    #[command(flatten)]
    pub attach: RemoteAttachArgs,
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
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    let target = parse_u64_flexible(&args.target)
        .map_err(|e| invalid_args(format!("target `{}`: {e}", args.target)))?;
    let kind = parse_u16_flexible(&args.kind)
        .map_err(|e| invalid_args(format!("kind `{}`: {e}", args.kind)))?;

    let profile = resolve_profile(config_path, profile_name).await?;
    let remote = require_remote_attach(&profile, &args.attach, "query")?;
    let ctx =
        CliContext::build_with_remote(&profile, args.identity.as_deref(), args.node, false, remote)
            .await?;
    let mesh = ctx.require_mesh_node()?;

    use net_sdk::aggregator::FoldQueryClient;
    let client = FoldQueryClient::new(mesh);
    let summaries = if args.fresh {
        client
            .query_summarize_now(target, kind)
            .await
            .map_err(|e| sdk(format!("fold.query (summarize-now) failed: {e}")))?
    } else {
        client
            .query_latest(target, kind)
            .await
            .map_err(|e| sdk(format!("fold.query (latest) failed: {e}")))?
    };

    let view = QueryView {
        target_node_id: target,
        fold_kind: format!("{kind:#06x}"),
        fresh: args.fresh,
        summary_count: summaries.len() as u64,
        summaries: summaries.into_iter().map(SummaryRow::from).collect(),
    };
    emit_value(OutputFormat::resolve_oneshot(output), &view)
        .map_err(|e| generic(format!("write aggregator query: {e}")))?;
    Ok(())
}

/// Resolve remote-attach for a verb that requires one. The
/// `verb` arg goes into the error message so the operator sees
/// which subcommand needed the missing flag.
fn require_remote_attach(
    profile: &crate::config::Profile,
    args: &RemoteAttachArgs,
    verb: &str,
) -> Result<RemoteAttach, CliError> {
    let resolved = resolve_remote_attach(
        profile,
        args.node_addr.as_deref(),
        args.node_pubkey.as_deref(),
        args.remote_node_id.as_deref(),
        args.psk_hex.as_deref(),
    )?;
    resolved.ok_or_else(|| {
        invalid_args(format!(
            "net aggregator {verb} needs a remote daemon target: pass \
             --node-addr <IP:PORT> --node-pubkey <HEX> --node-id <N> \
             --psk-hex <HEX> (each can be defaulted in the profile as \
             `node_addr` / `node_pubkey` / `node_id` / `psk_hex`)."
        ))
    })
}

async fn run_ls(
    args: LsArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    let profile = resolve_profile(config_path, profile_name).await?;
    // --remote flips the path; --node-addr implies --remote so
    // an operator who supplied attach flags doesn't accidentally
    // read the local registry.
    let want_remote = args.remote || args.attach.node_addr.is_some();
    if want_remote {
        return run_ls_remote(args, output, &profile).await;
    }
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

async fn run_ls_remote(
    args: LsArgs,
    output: Option<OutputFormat>,
    profile: &crate::config::Profile,
) -> Result<(), CliError> {
    let remote = require_remote_attach(profile, &args.attach, "ls --remote")?;
    let target_node_id = remote.node_id;
    let ctx =
        CliContext::build_with_remote(profile, args.identity.as_deref(), args.node, false, remote)
            .await?;
    let mesh = ctx.require_mesh_node()?;

    use net_sdk::aggregator::RegistryClient;
    let client = RegistryClient::new(mesh);
    let groups = client
        .list(target_node_id)
        .await
        .map_err(|e| sdk(format!("aggregator.registry list failed: {e}")))?;

    let view = RemoteLsView {
        target_node_id,
        group_count: groups.len() as u64,
        groups: groups.iter().map(RemoteLsGroupRow::from).collect(),
    };
    emit_value(OutputFormat::resolve_oneshot(output), &view)
        .map_err(|e| generic(format!("write aggregator ls --remote: {e}")))?;
    Ok(())
}

async fn run_spawn(
    args: SpawnArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    if args.replica_count == 0 {
        return Err(invalid_args("replica_count must be > 0"));
    }
    if args.template.trim().is_empty() {
        return Err(invalid_args("--template must not be empty"));
    }
    if args.name.trim().is_empty() {
        return Err(invalid_args("--name must not be empty"));
    }

    let profile = resolve_profile(config_path, profile_name).await?;
    let remote = require_remote_attach(&profile, &args.attach, "spawn")?;
    let target_node_id = remote.node_id;
    let ctx =
        CliContext::build_with_remote(&profile, args.identity.as_deref(), args.node, false, remote)
            .await?;
    let mesh = ctx.require_mesh_node()?;

    use net_sdk::aggregator::RegistryClient;
    let client = RegistryClient::new(mesh);
    let summary = client
        .spawn(target_node_id, args.template, args.name, args.replica_count)
        .await
        .map_err(|e| sdk(format!("aggregator.registry spawn failed: {e}")))?;

    let view = SpawnView::from(&summary);
    emit_value(OutputFormat::resolve_oneshot(output), &view)
        .map_err(|e| generic(format!("write aggregator spawn: {e}")))?;
    Ok(())
}

async fn run_scale(
    args: ScaleArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    if args.replica_count == 0 {
        return Err(invalid_args("replica_count must be > 0"));
    }
    if args.template.trim().is_empty() {
        return Err(invalid_args("--template must not be empty"));
    }
    if args.name.trim().is_empty() {
        return Err(invalid_args("--name must not be empty"));
    }

    let profile = resolve_profile(config_path, profile_name).await?;
    let remote = require_remote_attach(&profile, &args.attach, "scale")?;
    let target_node_id = remote.node_id;
    let ctx =
        CliContext::build_with_remote(&profile, args.identity.as_deref(), args.node, false, remote)
            .await?;
    let mesh = ctx.require_mesh_node()?;

    // Dedicated Scale RPC: grow/shrink in place. Surviving
    // replicas keep their identity + generation across the
    // resize. The daemon verifies the supplied template
    // matches the group's current spec before resizing.
    use net_sdk::aggregator::RegistryClient;
    let client = RegistryClient::new(mesh);
    let summary = client
        .scale(
            target_node_id,
            args.name.clone(),
            args.template.clone(),
            args.replica_count,
        )
        .await
        .map_err(|e| sdk(format!("aggregator.registry scale failed: {e}")))?;

    let view = SpawnView::from(&summary);
    emit_value(OutputFormat::resolve_oneshot(output), &view)
        .map_err(|e| generic(format!("write aggregator scale: {e}")))?;
    Ok(())
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
struct RemoteLsView {
    /// Echo of the remote target.
    target_node_id: u64,
    group_count: u64,
    groups: Vec<RemoteLsGroupRow>,
}

#[derive(Serialize)]
struct RemoteLsGroupRow {
    name: String,
    group_seed: String,
    /// Subnet the aggregator summarizes, rendered as `"3.7"`
    /// (dotted decimal). Read from the wire reply.
    source_subnet: String,
    /// Fold kinds the aggregator publishes summaries for,
    /// rendered as `0x____` strings. Read from the wire reply.
    fold_kinds: Vec<String>,
    healthy_count: u64,
    replica_count: u64,
    replicas: Vec<RemoteReplicaRow>,
}

impl From<&net_sdk::aggregator::RegistryGroupSummary> for RemoteLsGroupRow {
    fn from(g: &net_sdk::aggregator::RegistryGroupSummary) -> Self {
        let replicas: Vec<RemoteReplicaRow> =
            g.replicas.iter().map(RemoteReplicaRow::from).collect();
        let healthy_count = replicas.iter().filter(|r| r.healthy).count() as u64;
        Self {
            name: g.name.clone(),
            group_seed: hex::encode(g.group_seed),
            source_subnet: g.source_subnet.to_string(),
            fold_kinds: g.fold_kinds.iter().map(|k| format!("{k:#06x}")).collect(),
            healthy_count,
            replica_count: replicas.len() as u64,
            replicas,
        }
    }
}

#[derive(Serialize)]
struct SpawnView {
    /// Echo of the group the daemon registered.
    name: String,
    /// 64-char hex rendering of the daemon-side-derived group
    /// seed (today: `blake3(name)` — the daemon owns the
    /// derivation, the operator just sees the resulting bytes).
    group_seed: String,
    /// Subnet the aggregator summarizes — operator sanity-check
    /// that the resolved template matches expectations.
    source_subnet: String,
    /// Fold kinds the aggregator publishes summaries for,
    /// rendered as `0x____` strings.
    fold_kinds: Vec<String>,
    replica_count: u64,
    /// Per-replica rows — same shape as `ls`'s output so
    /// consumers can reuse parsers.
    replicas: Vec<RemoteReplicaRow>,
}

impl From<&net_sdk::aggregator::RegistryGroupSummary> for SpawnView {
    fn from(g: &net_sdk::aggregator::RegistryGroupSummary) -> Self {
        Self {
            name: g.name.clone(),
            group_seed: hex::encode(g.group_seed),
            source_subnet: g.source_subnet.to_string(),
            fold_kinds: g.fold_kinds.iter().map(|k| format!("{k:#06x}")).collect(),
            replica_count: g.replicas.len() as u64,
            replicas: g.replicas.iter().map(RemoteReplicaRow::from).collect(),
        }
    }
}

/// Wire-shape replica row. Distinct from [`LsReplicaRow`] because
/// the local snapshot ([`net_sdk::deck::AggregatorReplicaRow`])
/// and the wire reply
/// ([`net_sdk::aggregator::RegistryReplicaSummary`]) have
/// different field sets; this row mirrors the wire reply.
#[derive(Serialize)]
struct RemoteReplicaRow {
    generation: u64,
    healthy: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    diagnostic: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    placement_node_id: Option<u64>,
}

impl From<&net_sdk::aggregator::RegistryReplicaSummary> for RemoteReplicaRow {
    fn from(r: &net_sdk::aggregator::RegistryReplicaSummary) -> Self {
        Self {
            generation: r.generation,
            healthy: r.healthy,
            diagnostic: r.diagnostic.clone(),
            placement_node_id: r.placement_node_id,
        }
    }
}

#[derive(Serialize)]
struct QueryView {
    /// Echo of the resolved target node so the operator can
    /// diff their flag input against what the RPC actually hit.
    target_node_id: u64,
    /// `FoldKind::KIND_ID` formatted as `0x____`.
    fold_kind: String,
    /// `true` when the call used `SummarizeNow` (cache-bypass).
    fresh: bool,
    /// Convenience — `summaries.len()` rendered as its own field.
    summary_count: u64,
    /// Aggregator's buffered summaries (latest cadence) — same
    /// row shape as `inspect`'s output so consumers can reuse
    /// downstream parsers.
    summaries: Vec<SummaryRow>,
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
