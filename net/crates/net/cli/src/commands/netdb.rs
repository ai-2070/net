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
use net_sdk::cortex::{NetDb, NetDbBuilder, Redex};
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
}

#[derive(Subcommand, Debug)]
pub enum TasksCommand {
    /// List every task in the store.
    Ls(TasksLsArgs),
}

#[derive(Subcommand, Debug)]
pub enum MemoriesCommand {
    /// List every memory in the store.
    Ls(MemoriesLsArgs),
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

pub async fn run(cmd: NetdbCommand, output: Option<OutputFormat>) -> Result<(), CliError> {
    match cmd {
        NetdbCommand::Tasks(TasksCommand::Ls(args)) => run_tasks_ls(args, output).await,
        NetdbCommand::Memories(MemoriesCommand::Ls(args)) => run_memories_ls(args, output).await,
        NetdbCommand::Snapshot(args) => run_snapshot(args, output).await,
    }
}

async fn run_tasks_ls(
    args: TasksLsArgs,
    output: Option<OutputFormat>,
) -> Result<(), CliError> {
    let netdb = open_netdb(args.store.as_deref(), args.origin, /*tasks=*/ true, false).await?;
    let tasks: Vec<TaskRow> = match netdb.try_tasks() {
        Some(adapter) => {
            let state_arc = adapter.state();
            let guard = state_arc.read();
            guard
                .all()
                .map(|t| TaskRow {
                    id: format!("{:?}", t.id),
                    title: format!("{:?}", t),
                })
                .collect()
        }
        None => Vec::new(),
    };
    emit_value(OutputFormat::resolve_oneshot(output), &tasks)
        .map_err(|e| generic(format!("write tasks: {e}")))?;
    Ok(())
}

async fn run_memories_ls(
    args: MemoriesLsArgs,
    output: Option<OutputFormat>,
) -> Result<(), CliError> {
    let netdb = open_netdb(args.store.as_deref(), args.origin, false, /*memories=*/ true).await?;
    let memories: Vec<MemoryRow> = match netdb.try_memories() {
        Some(adapter) => {
            let state_arc = adapter.state();
            let guard = state_arc.read();
            guard
                .all()
                .map(|m| MemoryRow {
                    id: format!("{:?}", m.id),
                    summary: format!("{:?}", m),
                })
                .collect()
        }
        None => Vec::new(),
    };
    emit_value(OutputFormat::resolve_oneshot(output), &memories)
        .map_err(|e| generic(format!("write memories: {e}")))?;
    Ok(())
}

async fn run_snapshot(
    args: SnapshotArgs,
    output: Option<OutputFormat>,
) -> Result<(), CliError> {
    if !args.force && args.out.exists() {
        return Err(crate::error::invalid_args(format!(
            "{} already exists; pass --force to overwrite",
            args.out.display()
        )));
    }
    let netdb = open_netdb(args.store.as_deref(), args.origin, true, true).await?;
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
) -> Result<Arc<NetDb>, CliError> {
    let path = match store {
        Some(p) => p.to_path_buf(),
        None => default_netdb_path()
            .ok_or_else(|| generic("no $XDG_DATA_HOME / data dir available; pass --store <PATH>"))?,
    };
    tokio::fs::create_dir_all(&path).await.map_err(|e| {
        generic(format!(
            "failed to create netdb directory {}: {e}",
            path.display()
        ))
    })?;
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
struct TaskRow {
    id: String,
    title: String,
}

#[derive(Serialize)]
struct MemoryRow {
    id: String,
    summary: String,
}

#[derive(Serialize)]
struct SnapshotResult {
    path: String,
    bytes: u64,
}
