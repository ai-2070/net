//! `net netdb (tasks ls|memories ls|snapshot)` — Cortex-backed
//! local KV store reads.
//!
//! Phase 1 scope: `tasks ls`, `memories ls`, `snapshot --out`.
//! Mutations (`tasks (create|complete|rename|delete)`,
//! `memories (store|retag|pin|unpin|delete)`, `restore`) land in
//! Phase 2 once the read surface has shaken out.
//!
//! Audience is daemon developers + agents debugging local
//! state, not cluster operators. The plan calls this out
//! explicitly (NET_CLI_PLAN.md §9).

use std::path::PathBuf;
use std::sync::Arc;

use clap::{Args, Subcommand};
use net_sdk::cortex::{Memory, NetDb, NetDbBuilder, Redex, Task};
use serde::Serialize;

use crate::error::{generic, sdk, CliError};
use crate::prelude::{emit_value, OutputFormat};

#[derive(Subcommand, Debug)]
pub enum NetdbCommand {
    /// Task adapter operations.
    #[command(subcommand)]
    Tasks(TasksCommand),
    /// Memory adapter operations.
    #[command(subcommand)]
    Memories(MemoriesCommand),
    /// Export the full NetDB snapshot to a file.
    Snapshot(SnapshotArgs),
    /// Restore from a previously-exported snapshot.
    Restore(RestoreArgs),
}

#[derive(Subcommand, Debug)]
pub enum TasksCommand {
    /// List every task in the store.
    Ls(TasksLsArgs),
    /// Create a new task.
    Create(TasksCreateArgs),
    /// Rename an existing task by id.
    Rename(TasksRenameArgs),
    /// Mark a task completed.
    Complete(TasksIdArgs),
    /// Delete a task.
    Delete(TasksIdArgs),
}

#[derive(Subcommand, Debug)]
pub enum MemoriesCommand {
    /// List every memory in the store.
    Ls(MemoriesLsArgs),
    /// Store a new memory.
    Store(MemoriesStoreArgs),
    /// Replace the tag set on an existing memory.
    Retag(MemoriesRetagArgs),
    /// Pin a memory.
    Pin(MemoriesIdArgs),
    /// Unpin a memory.
    Unpin(MemoriesIdArgs),
    /// Delete a memory.
    Delete(MemoriesIdArgs),
}

#[derive(Args, Debug)]
pub struct TasksLsArgs {
    /// Path to the NetDB persistent directory. Defaults to
    /// `$XDG_DATA_HOME/net/netdb`.
    #[arg(long)]
    pub store: Option<PathBuf>,

    /// Operator origin hash. Defaults to 0 — matches the
    /// substrate's "anonymous local store" convention.
    #[arg(long, default_value_t = 0)]
    pub origin: u64,
}

#[derive(Args, Debug)]
pub struct MemoriesLsArgs {
    #[arg(long)]
    pub store: Option<PathBuf>,

    #[arg(long, default_value_t = 0)]
    pub origin: u64,
}

#[derive(Args, Debug)]
pub struct SnapshotArgs {
    #[arg(long)]
    pub store: Option<PathBuf>,

    #[arg(long, default_value_t = 0)]
    pub origin: u64,

    /// Output file. Postcard-encoded `NetDbSnapshot`. Refuses
    /// to overwrite an existing file unless `--force` is set.
    #[arg(long)]
    pub out: PathBuf,

    #[arg(long)]
    pub force: bool,
}

#[derive(Args, Debug)]
pub struct RestoreArgs {
    #[arg(long)]
    pub store: Option<PathBuf>,

    #[arg(long, default_value_t = 0)]
    pub origin: u64,

    /// Input file (postcard-encoded `NetDbSnapshot`).
    #[arg(long)]
    pub from: PathBuf,

    /// Allow restoring over a non-empty store. The substrate
    /// re-folds the snapshot's chains against the local Redex
    /// — without `--force` we refuse if the destination
    /// directory already contains data.
    #[arg(long)]
    pub force: bool,
}

#[derive(Args, Debug)]
pub struct TasksCreateArgs {
    /// 64-bit task id. Decimal or `0x`-prefixed hex.
    #[arg(value_parser = parse_u64_flexible)]
    pub id: u64,
    /// Task title.
    #[arg(long)]
    pub title: String,
    #[command(flatten)]
    pub common: NetdbCommon,
}

#[derive(Args, Debug)]
pub struct TasksRenameArgs {
    #[arg(value_parser = parse_u64_flexible)]
    pub id: u64,
    #[arg(long)]
    pub title: String,
    #[command(flatten)]
    pub common: NetdbCommon,
}

#[derive(Args, Debug)]
pub struct TasksIdArgs {
    #[arg(value_parser = parse_u64_flexible)]
    pub id: u64,
    #[command(flatten)]
    pub common: NetdbCommon,
}

#[derive(Args, Debug)]
pub struct MemoriesStoreArgs {
    #[arg(value_parser = parse_u64_flexible)]
    pub id: u64,
    #[arg(long)]
    pub content: String,
    #[arg(long = "tag", num_args = 0.., value_name = "TAG")]
    pub tags: Vec<String>,
    /// Free-form provenance string (which daemon / process /
    /// human created the memory).
    #[arg(long, default_value = "cli")]
    pub source: String,
    #[command(flatten)]
    pub common: NetdbCommon,
}

#[derive(Args, Debug)]
pub struct MemoriesRetagArgs {
    #[arg(value_parser = parse_u64_flexible)]
    pub id: u64,
    #[arg(long = "tag", num_args = 0.., value_name = "TAG")]
    pub tags: Vec<String>,
    #[command(flatten)]
    pub common: NetdbCommon,
}

#[derive(Args, Debug)]
pub struct MemoriesIdArgs {
    #[arg(value_parser = parse_u64_flexible)]
    pub id: u64,
    #[command(flatten)]
    pub common: NetdbCommon,
}

#[derive(Args, Debug)]
pub struct NetdbCommon {
    #[arg(long)]
    pub store: Option<PathBuf>,
    #[arg(long, default_value_t = 0)]
    pub origin: u64,
}

fn parse_u64_flexible(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(rest, 16).map_err(|e| format!("invalid hex: {e}"))
    } else {
        s.parse::<u64>()
            .map_err(|e| format!("invalid integer: {e}"))
    }
}

pub async fn run(cmd: NetdbCommand, output: Option<OutputFormat>) -> Result<(), CliError> {
    match cmd {
        NetdbCommand::Tasks(TasksCommand::Ls(args)) => run_tasks_ls(args, output).await,
        NetdbCommand::Tasks(TasksCommand::Create(args)) => run_tasks_create(args, output).await,
        NetdbCommand::Tasks(TasksCommand::Rename(args)) => run_tasks_rename(args, output).await,
        NetdbCommand::Tasks(TasksCommand::Complete(args)) => run_tasks_complete(args, output).await,
        NetdbCommand::Tasks(TasksCommand::Delete(args)) => run_tasks_delete(args, output).await,
        NetdbCommand::Memories(MemoriesCommand::Ls(args)) => run_memories_ls(args, output).await,
        NetdbCommand::Memories(MemoriesCommand::Store(args)) => {
            run_memories_store(args, output).await
        }
        NetdbCommand::Memories(MemoriesCommand::Retag(args)) => {
            run_memories_retag(args, output).await
        }
        NetdbCommand::Memories(MemoriesCommand::Pin(args)) => {
            run_memories_pin(args, output, false).await
        }
        NetdbCommand::Memories(MemoriesCommand::Unpin(args)) => {
            run_memories_pin(args, output, true).await
        }
        NetdbCommand::Memories(MemoriesCommand::Delete(args)) => {
            run_memories_delete(args, output).await
        }
        NetdbCommand::Snapshot(args) => run_snapshot(args, output).await,
        NetdbCommand::Restore(args) => run_restore(args, output).await,
    }
}

// =========================================================================
// Task mutations
// =========================================================================

async fn run_tasks_create(
    args: TasksCreateArgs,
    output: Option<OutputFormat>,
) -> Result<(), CliError> {
    let netdb = open_netdb(
        args.common.store.as_deref(),
        args.common.origin,
        true,
        false,
        true,
    )
    .await?;
    let tasks = netdb
        .try_tasks()
        .ok_or_else(|| sdk("NetDB has no tasks adapter wired"))?;
    let seq = tasks
        .create(args.id, args.title, now_ns())
        .map_err(|e| sdk(format!("tasks create: {e}")))?;
    emit_mutation(output, "task_created", args.id, seq)
}

async fn run_tasks_rename(
    args: TasksRenameArgs,
    output: Option<OutputFormat>,
) -> Result<(), CliError> {
    let netdb = open_netdb(
        args.common.store.as_deref(),
        args.common.origin,
        true,
        false,
        true,
    )
    .await?;
    let tasks = netdb
        .try_tasks()
        .ok_or_else(|| sdk("NetDB has no tasks adapter wired"))?;
    let seq = tasks
        .rename(args.id, args.title, now_ns())
        .map_err(|e| sdk(format!("tasks rename: {e}")))?;
    emit_mutation(output, "task_renamed", args.id, seq)
}

async fn run_tasks_complete(
    args: TasksIdArgs,
    output: Option<OutputFormat>,
) -> Result<(), CliError> {
    let netdb = open_netdb(
        args.common.store.as_deref(),
        args.common.origin,
        true,
        false,
        true,
    )
    .await?;
    let tasks = netdb
        .try_tasks()
        .ok_or_else(|| sdk("NetDB has no tasks adapter wired"))?;
    let seq = tasks
        .complete(args.id, now_ns())
        .map_err(|e| sdk(format!("tasks complete: {e}")))?;
    emit_mutation(output, "task_completed", args.id, seq)
}

async fn run_tasks_delete(args: TasksIdArgs, output: Option<OutputFormat>) -> Result<(), CliError> {
    let netdb = open_netdb(
        args.common.store.as_deref(),
        args.common.origin,
        true,
        false,
        true,
    )
    .await?;
    let tasks = netdb
        .try_tasks()
        .ok_or_else(|| sdk("NetDB has no tasks adapter wired"))?;
    let seq = tasks
        .delete(args.id)
        .map_err(|e| sdk(format!("tasks delete: {e}")))?;
    emit_mutation(output, "task_deleted", args.id, seq)
}

// =========================================================================
// Memory mutations
// =========================================================================

async fn run_memories_store(
    args: MemoriesStoreArgs,
    output: Option<OutputFormat>,
) -> Result<(), CliError> {
    let netdb = open_netdb(
        args.common.store.as_deref(),
        args.common.origin,
        false,
        true,
        true,
    )
    .await?;
    let memories = netdb
        .try_memories()
        .ok_or_else(|| sdk("NetDB has no memories adapter wired"))?;
    let seq = memories
        .store(args.id, args.content, args.tags, args.source, now_ns())
        .map_err(|e| sdk(format!("memories store: {e}")))?;
    emit_mutation(output, "memory_stored", args.id, seq)
}

async fn run_memories_retag(
    args: MemoriesRetagArgs,
    output: Option<OutputFormat>,
) -> Result<(), CliError> {
    let netdb = open_netdb(
        args.common.store.as_deref(),
        args.common.origin,
        false,
        true,
        true,
    )
    .await?;
    let memories = netdb
        .try_memories()
        .ok_or_else(|| sdk("NetDB has no memories adapter wired"))?;
    let seq = memories
        .retag(args.id, args.tags, now_ns())
        .map_err(|e| sdk(format!("memories retag: {e}")))?;
    emit_mutation(output, "memory_retagged", args.id, seq)
}

async fn run_memories_pin(
    args: MemoriesIdArgs,
    output: Option<OutputFormat>,
    unpin: bool,
) -> Result<(), CliError> {
    let netdb = open_netdb(
        args.common.store.as_deref(),
        args.common.origin,
        false,
        true,
        true,
    )
    .await?;
    let memories = netdb
        .try_memories()
        .ok_or_else(|| sdk("NetDB has no memories adapter wired"))?;
    let (seq, kind) = if unpin {
        (
            memories
                .unpin(args.id, now_ns())
                .map_err(|e| sdk(format!("memories unpin: {e}")))?,
            "memory_unpinned",
        )
    } else {
        (
            memories
                .pin(args.id, now_ns())
                .map_err(|e| sdk(format!("memories pin: {e}")))?,
            "memory_pinned",
        )
    };
    emit_mutation(output, kind, args.id, seq)
}

async fn run_memories_delete(
    args: MemoriesIdArgs,
    output: Option<OutputFormat>,
) -> Result<(), CliError> {
    let netdb = open_netdb(
        args.common.store.as_deref(),
        args.common.origin,
        false,
        true,
        true,
    )
    .await?;
    let memories = netdb
        .try_memories()
        .ok_or_else(|| sdk("NetDB has no memories adapter wired"))?;
    let seq = memories
        .delete(args.id)
        .map_err(|e| sdk(format!("memories delete: {e}")))?;
    emit_mutation(output, "memory_deleted", args.id, seq)
}

// =========================================================================
// restore
// =========================================================================

async fn run_restore(args: RestoreArgs, output: Option<OutputFormat>) -> Result<(), CliError> {
    let dest = match args.store.as_deref() {
        Some(p) => p.to_path_buf(),
        None => default_netdb_path().ok_or_else(|| {
            generic("no $XDG_DATA_HOME / data dir available; pass --store <PATH>")
        })?,
    };
    if !args.force && dest.exists() {
        let non_empty = match tokio::fs::read_dir(&dest).await {
            Ok(mut iter) => iter.next_entry().await.unwrap_or(None).is_some(),
            Err(_) => false,
        };
        if non_empty {
            return Err(crate::error::invalid_args(format!(
                "target store {} already contains data; pass --force to overwrite",
                dest.display()
            )));
        }
    }
    let bytes = tokio::fs::read(&args.from).await.map_err(|e| {
        generic(format!(
            "failed to read snapshot file {}: {e}",
            args.from.display()
        ))
    })?;
    let snap = net_sdk::cortex::NetDbSnapshot::decode(&bytes)
        .map_err(|e| sdk(format!("netdb snapshot decode: {e}")))?;
    tokio::fs::create_dir_all(&dest).await.map_err(|e| {
        generic(format!(
            "failed to create netdb directory {}: {e}",
            dest.display()
        ))
    })?;
    let redex = Redex::new().with_persistent_dir(&dest);
    let _ = NetDb::builder(redex)
        .origin(args.origin)
        .persistent(true)
        .with_tasks()
        .with_memories()
        .build_from_snapshot(&snap)
        .await
        .map_err(|e| sdk(format!("netdb restore: {e}")))?;
    let info = RestoreResult {
        path: dest.display().to_string(),
        bytes_restored: bytes.len() as u64,
    };
    emit_value(OutputFormat::resolve_oneshot(output), &info)
        .map_err(|e| generic(format!("write restore result: {e}")))?;
    Ok(())
}

// =========================================================================
// Helpers
// =========================================================================

fn now_ns() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

fn emit_mutation(
    output: Option<OutputFormat>,
    kind: &'static str,
    id: u64,
    seq: u64,
) -> Result<(), CliError> {
    let info = MutationResult { kind, id, seq };
    emit_value(OutputFormat::resolve_oneshot(output), &info)
        .map_err(|e| generic(format!("write mutation result: {e}")))?;
    Ok(())
}

#[derive(Serialize)]
struct MutationResult {
    kind: &'static str,
    id: u64,
    seq: u64,
}

#[derive(Serialize)]
struct RestoreResult {
    path: String,
    bytes_restored: u64,
}

async fn run_tasks_ls(args: TasksLsArgs, output: Option<OutputFormat>) -> Result<(), CliError> {
    let netdb = open_netdb(
        args.store.as_deref(),
        args.origin,
        /*tasks=*/ true,
        false,
        /*create_if_missing=*/ false,
    )
    .await?;
    let adapter = netdb
        .try_tasks()
        .ok_or_else(|| sdk("NetDB has no tasks adapter wired"))?;
    let state_arc = adapter.state();
    let guard = state_arc.read();
    let tasks: Vec<Task> = guard.all().cloned().collect();
    emit_value(OutputFormat::resolve_oneshot(output), &tasks)
        .map_err(|e| generic(format!("write tasks: {e}")))?;
    Ok(())
}

async fn run_memories_ls(
    args: MemoriesLsArgs,
    output: Option<OutputFormat>,
) -> Result<(), CliError> {
    let netdb = open_netdb(
        args.store.as_deref(),
        args.origin,
        false,
        /*memories=*/ true,
        /*create_if_missing=*/ false,
    )
    .await?;
    let adapter = netdb
        .try_memories()
        .ok_or_else(|| sdk("NetDB has no memories adapter wired"))?;
    let state_arc = adapter.state();
    let guard = state_arc.read();
    let memories: Vec<Memory> = guard.all().cloned().collect();
    emit_value(OutputFormat::resolve_oneshot(output), &memories)
        .map_err(|e| generic(format!("write memories: {e}")))?;
    Ok(())
}

async fn run_snapshot(args: SnapshotArgs, output: Option<OutputFormat>) -> Result<(), CliError> {
    if !args.force && args.out.exists() {
        return Err(crate::error::invalid_args(format!(
            "{} already exists; pass --force to overwrite",
            args.out.display()
        )));
    }
    let netdb = open_netdb(args.store.as_deref(), args.origin, true, true, false).await?;
    let snapshot = netdb
        .snapshot()
        .map_err(|e| sdk(format!("netdb snapshot: {e}")))?;
    let bytes = snapshot
        .encode()
        .map_err(|e| sdk(format!("netdb snapshot encode: {e}")))?;
    if let Some(parent) = args.out.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| {
            generic(format!(
                "failed to create parent directory {}: {e}",
                parent.display()
            ))
        })?;
    }
    tokio::fs::write(&args.out, &bytes)
        .await
        .map_err(|e| generic(format!("write snapshot to {}: {e}", args.out.display())))?;
    let info = SnapshotResult {
        path: args.out.display().to_string(),
        bytes: bytes.len() as u64,
    };
    emit_value(OutputFormat::resolve_oneshot(output), &info)
        .map_err(|e| generic(format!("write snapshot result: {e}")))?;
    Ok(())
}

async fn open_netdb(
    store: Option<&std::path::Path>,
    origin: u64,
    enable_tasks: bool,
    enable_memories: bool,
    create_if_missing: bool,
) -> Result<Arc<NetDb>, CliError> {
    let path = match store {
        Some(p) => p.to_path_buf(),
        None => default_netdb_path().ok_or_else(|| {
            generic("no $XDG_DATA_HOME / data dir available; pass --store <PATH>")
        })?,
    };
    if create_if_missing {
        tokio::fs::create_dir_all(&path).await.map_err(|e| {
            generic(format!(
                "failed to create netdb directory {}: {e}",
                path.display()
            ))
        })?;
    } else if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
        // Read paths refuse to silently fabricate an empty store —
        // a typo'd `--store /var/tmp/typo` would otherwise return
        // zero rows with no diagnostic.
        return Err(crate::error::invalid_args(format!(
            "netdb store {} does not exist; pass --store <PATH> to an \
             existing store or run a mutation first to create one",
            path.display()
        )));
    }
    let redex = Redex::new().with_persistent_dir(&path);
    let mut builder: NetDbBuilder = NetDb::builder(redex).origin(origin).persistent(true);
    if enable_tasks {
        builder = builder.with_tasks();
    }
    if enable_memories {
        builder = builder.with_memories();
    }
    let netdb = builder
        .build()
        .await
        .map_err(|e| sdk(format!("netdb open at {}: {e}", path.display())))?;
    Ok(Arc::new(netdb))
}

fn default_netdb_path() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("net").join("netdb"))
}

#[derive(Serialize)]
struct SnapshotResult {
    path: String,
    bytes: u64,
}
