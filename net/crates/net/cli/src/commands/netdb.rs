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
use crate::parsers::parse_u64_flexible;
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

    /// Include the tasks adapter in the snapshot. Defaults to
    /// `true`. Pass `--no-tasks` to capture a memories-only
    /// snapshot. Pre-fix this was unconditional and a tasks-only
    /// store snapshot silently fabricated an empty memories
    /// channel that `restore` then believed was authoritative.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    pub with_tasks: bool,

    /// Include the memories adapter in the snapshot. Defaults to
    /// `true`. Pass `--no-memories` to capture a tasks-only
    /// snapshot.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    pub with_memories: bool,
}

#[derive(Args, Debug)]
pub struct RestoreArgs {
    #[arg(long)]
    pub store: Option<PathBuf>,

    /// Origin to attribute the restored chains to. `NetDbSnapshot`
    /// doesn't carry the producing origin in its envelope today,
    /// so this is operator-supplied — pass the same value the
    /// snapshot was authored under to avoid a silent cross-origin
    /// restore. Required. Pass `--allow-origin-zero` if the
    /// snapshot really was produced at origin 0; this keeps the
    /// "forgot the flag" footgun from silently folding chains
    /// against the wrong origin.
    #[arg(long)]
    pub origin: Option<u64>,

    /// Acknowledge that `--origin 0` is intentional (e.g.
    /// single-node deployments or test snapshots). Without this,
    /// `--origin 0` is rejected to flush out the "forgot to pass
    /// --origin" footgun.
    #[arg(long)]
    pub allow_origin_zero: bool,

    /// Input file (postcard-encoded `NetDbSnapshot`).
    #[arg(long)]
    pub from: PathBuf,

    /// Allow restoring over a non-empty store. **The substrate
    /// re-folds the snapshot's chains against the existing local
    /// Redex** — `--force` does NOT clear `--store` first. The
    /// effective operation is therefore "merge snapshot into the
    /// current store," not "replace store with snapshot." If you
    /// need a clean restore, remove `--store` manually before
    /// running, or pass `--clear` to have the CLI do it for you.
    /// Without `--force` we refuse if `--store` already contains
    /// data.
    #[arg(long)]
    pub force: bool,

    /// Clear `--store` before folding the snapshot, producing a
    /// clean restore rather than a merge. Implies `--force`. Use
    /// when the snapshot is the authoritative state and any
    /// existing chains under `--store` should be discarded.
    #[arg(long)]
    pub clear: bool,
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

pub async fn run(
    cmd: NetdbCommand,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    // Resolve profile.netdb up-front so every subcommand path
    // (including the read-only `tasks ls` path that doesn't carry
    // a `--store`) honours it. Pre-fix `netdb` was the only
    // top-level that ignored --config/--profile, so an operator
    // with `netdb = "/srv/netdb"` in `prod` would land in the
    // default `$XDG_DATA_HOME/net/netdb` and write mutations into
    // the wrong store.
    let profile_netdb = crate::context::resolve_profile(config_path, profile_name)
        .await
        .ok()
        .and_then(|p| p.netdb);
    let profile_netdb = profile_netdb.as_deref();
    match cmd {
        NetdbCommand::Tasks(TasksCommand::Ls(args)) => {
            run_tasks_ls(args, output, profile_netdb).await
        }
        NetdbCommand::Tasks(TasksCommand::Create(args)) => {
            run_tasks_create(args, output, profile_netdb).await
        }
        NetdbCommand::Tasks(TasksCommand::Rename(args)) => {
            run_tasks_rename(args, output, profile_netdb).await
        }
        NetdbCommand::Tasks(TasksCommand::Complete(args)) => {
            run_tasks_complete(args, output, profile_netdb).await
        }
        NetdbCommand::Tasks(TasksCommand::Delete(args)) => {
            run_tasks_delete(args, output, profile_netdb).await
        }
        NetdbCommand::Memories(MemoriesCommand::Ls(args)) => {
            run_memories_ls(args, output, profile_netdb).await
        }
        NetdbCommand::Memories(MemoriesCommand::Store(args)) => {
            run_memories_store(args, output, profile_netdb).await
        }
        NetdbCommand::Memories(MemoriesCommand::Retag(args)) => {
            run_memories_retag(args, output, profile_netdb).await
        }
        NetdbCommand::Memories(MemoriesCommand::Pin(args)) => {
            run_memories_pin(args, output, false, profile_netdb).await
        }
        NetdbCommand::Memories(MemoriesCommand::Unpin(args)) => {
            run_memories_pin(args, output, true, profile_netdb).await
        }
        NetdbCommand::Memories(MemoriesCommand::Delete(args)) => {
            run_memories_delete(args, output, profile_netdb).await
        }
        NetdbCommand::Snapshot(args) => run_snapshot(args, output, profile_netdb).await,
        NetdbCommand::Restore(args) => run_restore(args, output, profile_netdb).await,
    }
}

// =========================================================================
// Task mutations
// =========================================================================

async fn run_tasks_create(
    args: TasksCreateArgs,
    output: Option<OutputFormat>,
    profile_netdb: Option<&std::path::Path>,
) -> Result<(), CliError> {
    let netdb = open_netdb(
        args.common.store.as_deref(),
        profile_netdb,
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
    emit_mutation(output, "task_created", args.id, seq, netdb)
}

async fn run_tasks_rename(
    args: TasksRenameArgs,
    output: Option<OutputFormat>,
    profile_netdb: Option<&std::path::Path>,
) -> Result<(), CliError> {
    let netdb = open_netdb(
        args.common.store.as_deref(),
        profile_netdb,
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
    emit_mutation(output, "task_renamed", args.id, seq, netdb)
}

async fn run_tasks_complete(
    args: TasksIdArgs,
    output: Option<OutputFormat>,
    profile_netdb: Option<&std::path::Path>,
) -> Result<(), CliError> {
    let netdb = open_netdb(
        args.common.store.as_deref(),
        profile_netdb,
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
    emit_mutation(output, "task_completed", args.id, seq, netdb)
}

async fn run_tasks_delete(
    args: TasksIdArgs,
    output: Option<OutputFormat>,
    profile_netdb: Option<&std::path::Path>,
) -> Result<(), CliError> {
    let netdb = open_netdb(
        args.common.store.as_deref(),
        profile_netdb,
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
    emit_mutation(output, "task_deleted", args.id, seq, netdb)
}

// =========================================================================
// Memory mutations
// =========================================================================

async fn run_memories_store(
    args: MemoriesStoreArgs,
    output: Option<OutputFormat>,
    profile_netdb: Option<&std::path::Path>,
) -> Result<(), CliError> {
    let netdb = open_netdb(
        args.common.store.as_deref(),
        profile_netdb,
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
    emit_mutation(output, "memory_stored", args.id, seq, netdb)
}

async fn run_memories_retag(
    args: MemoriesRetagArgs,
    output: Option<OutputFormat>,
    profile_netdb: Option<&std::path::Path>,
) -> Result<(), CliError> {
    let netdb = open_netdb(
        args.common.store.as_deref(),
        profile_netdb,
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
    emit_mutation(output, "memory_retagged", args.id, seq, netdb)
}

async fn run_memories_pin(
    args: MemoriesIdArgs,
    output: Option<OutputFormat>,
    unpin: bool,
    profile_netdb: Option<&std::path::Path>,
) -> Result<(), CliError> {
    let netdb = open_netdb(
        args.common.store.as_deref(),
        profile_netdb,
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
    emit_mutation(output, kind, args.id, seq, netdb)
}

async fn run_memories_delete(
    args: MemoriesIdArgs,
    output: Option<OutputFormat>,
    profile_netdb: Option<&std::path::Path>,
) -> Result<(), CliError> {
    let netdb = open_netdb(
        args.common.store.as_deref(),
        profile_netdb,
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
    emit_mutation(output, "memory_deleted", args.id, seq, netdb)
}

// =========================================================================
// restore
// =========================================================================

async fn run_restore(
    args: RestoreArgs,
    output: Option<OutputFormat>,
    profile_netdb: Option<&std::path::Path>,
) -> Result<(), CliError> {
    // Pre-fix `--origin` was `default_value_t = 0` so clap could
    // not distinguish "operator forgot --origin" from "operator
    // typed --origin 0". A defaulted-to-zero origin silently
    // folded chains against the wrong origin if the snapshot was
    // authored elsewhere. Require explicit --origin, with
    // --allow-origin-zero for the legitimate origin=0 case.
    let origin = match (args.origin, args.allow_origin_zero) {
        (Some(0), false) => {
            return Err(crate::error::invalid_args(
                "--origin 0 must be explicitly acknowledged via --allow-origin-zero \
                 (the snapshot has no embedded origin, so a defaulted zero risks a \
                 silent cross-origin fold)",
            ));
        }
        (Some(v), _) => v,
        (None, true) => 0,
        (None, false) => {
            return Err(crate::error::invalid_args(
                "--origin <u64> is required; pass the value the snapshot was authored \
                 under, or --allow-origin-zero for an intentional origin=0 restore",
            ));
        }
    };
    let dest = match args.store.as_deref() {
        Some(p) => p.to_path_buf(),
        None => match profile_netdb {
            Some(p) => p.to_path_buf(),
            None => default_netdb_path().ok_or_else(|| {
                generic("no $XDG_DATA_HOME / data dir available; pass --store <PATH>")
            })?,
        },
    };
    // `--clear` implies `--force` and produces an actual restore
    // (snapshot replaces existing store). Plain `--force` keeps
    // the pre-fix merge semantic — re-documented honestly above
    // so the verb-vs-behavior gap is visible to operators.
    let force = args.force || args.clear;
    if args.clear && dest.exists() {
        tokio::fs::remove_dir_all(&dest).await.map_err(|e| {
            generic(format!(
                "--clear: failed to remove existing store {}: {e}",
                dest.display()
            ))
        })?;
    } else if args.force && dest.exists() {
        eprintln!(
            "warning: --force on a non-empty store {} merges the snapshot's chains \
             into the existing Redex (this is a fold, not a replace). Pass --clear \
             to remove the store before folding.",
            dest.display()
        );
    }
    if !force && dest.exists() {
        // The non-empty check must distinguish empty-dir from
        // read-error: pre-fix `read_dir`'s `Err(_) => false` and
        // `next_entry`'s `.unwrap_or(None)` both swallowed I/O
        // errors and proceeded as if the directory were empty —
        // letting a populated store get overwritten without
        // `--force` when read_dir hit a permission error, the
        // path was not actually a directory, or next_entry hit
        // mid-enumeration jitter.
        let non_empty = match tokio::fs::read_dir(&dest).await {
            Ok(mut iter) => match iter.next_entry().await {
                Ok(Some(_)) => true,
                Ok(None) => false,
                Err(e) => {
                    return Err(generic(format!(
                        "failed to inspect target store {}: {e}; pass --force to override",
                        dest.display()
                    )));
                }
            },
            Err(e) => {
                return Err(generic(format!(
                    "failed to open target store {} for inspection: {e}; pass --force to override",
                    dest.display()
                )));
            }
        };
        if non_empty {
            return Err(crate::error::invalid_args(format!(
                "target store {} already contains data; pass --force to overwrite",
                dest.display()
            )));
        }
    }
    // Hard cap on snapshot file size before letting postcard
    // touch operator-supplied bytes. Postcard is memory-safe but
    // obeys the decoded `Vec` length prefixes; a crafted `--from`
    // blob can request multi-GB allocations and OOM the daemon.
    // 4 GiB is generous for any legitimate snapshot
    // (`bindings/python/tests/test_netdb.py` exercises this path
    // with byte-sized blobs); raise the bound here when a real
    // workload pushes past it.
    const SNAPSHOT_MAX_BYTES: u64 = 4 * 1024 * 1024 * 1024;
    let meta = tokio::fs::metadata(&args.from).await.map_err(|e| {
        generic(format!(
            "failed to stat snapshot file {}: {e}",
            args.from.display()
        ))
    })?;
    if meta.len() > SNAPSHOT_MAX_BYTES {
        return Err(crate::error::invalid_args(format!(
            "snapshot file {} is {} bytes, exceeds the {} byte ceiling; \
             pass a smaller snapshot or raise SNAPSHOT_MAX_BYTES",
            args.from.display(),
            meta.len(),
            SNAPSHOT_MAX_BYTES
        )));
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
    // Open only the adapters the snapshot actually carries.
    // Pre-fix the builder unconditionally wired `with_tasks` and
    // `with_memories`, so a tasks-only snapshot still materialised
    // an empty memories channel under `dest` (and vice-versa);
    // operators reading the restored store back saw a phantom
    // memories adapter they never asked for, and on a `--force`
    // (merge) restore the pre-existing memories chains on disk got
    // folded against an empty snapshot half.
    let redex = Redex::new().with_persistent_dir(&dest);
    let mut builder = NetDb::builder(redex).origin(origin).persistent(true);
    if snap.tasks.is_some() {
        builder = builder.with_tasks();
    }
    if snap.memories.is_some() {
        builder = builder.with_memories();
    }
    if snap.tasks.is_none() && snap.memories.is_none() {
        return Err(crate::error::invalid_args(
            "snapshot file carries neither tasks nor memories; nothing to restore",
        ));
    }
    let netdb = builder
        .build_from_snapshot(&snap)
        .await
        .map_err(|e| sdk(format!("netdb restore: {e}")))?;
    let info = RestoreResult {
        path: dest.display().to_string(),
        // `bytes_read` reflects the on-disk postcard envelope
        // length, which is what we actually loaded from `--from`.
        // Pre-fix this field was named `bytes_restored`, which
        // implied "bytes folded into state" - misleading
        // telemetry on a destructive command. Renamed to make
        // the semantic accurate; per-adapter `last_seq` for
        // "what landed" is tracked separately via the substrate.
        bytes_read: bytes.len() as u64,
    };
    let r = emit_value(OutputFormat::resolve_oneshot(output), &info)
        .map_err(|e| generic(format!("write restore result: {e}")));
    if let Err(e) = netdb.close() {
        tracing::warn!(error = %e, "netdb close failed at end of restore");
    }
    r?;
    Ok(())
}

// =========================================================================
// Helpers
// =========================================================================

fn now_ns() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => u64::try_from(d.as_nanos()).unwrap_or(u64::MAX),
        // Pre-1970 host clock. Pre-fix this `unwrap_or(0)`-ed,
        // stamping every `created_ns` / `updated_ns` with 0 and
        // collapsing time-ordered filters into insertion order.
        // Surface a tracing warning and fall back to a fixed
        // post-epoch sentinel (10 minutes past epoch) so
        // downstream filters at least see a unique non-zero
        // value, and operators see the misconfig in logs.
        Err(_) => {
            tracing::warn!(
                "system clock is before UNIX epoch; stamping events with the post-epoch \
                 sentinel - check NTP / RTC configuration"
            );
            600_000_000_000
        }
    }
}

fn emit_mutation(
    output: Option<OutputFormat>,
    kind: &'static str,
    id: u64,
    seq: u64,
    netdb: Arc<NetDb>,
) -> Result<(), CliError> {
    let info = MutationResult { kind, id, seq };
    let r = emit_value(OutputFormat::resolve_oneshot(output), &info)
        .map_err(|e| generic(format!("write mutation result: {e}")));
    // Close the NetDb on the success path. Pre-fix the Arc just
    // dropped on function exit and `NetDb` has no Drop impl, so
    // the persistent-dir handle outlived the function on Windows
    // (no fsync, no explicit close) until the inner adapters'
    // Arcs final-dropped. A subsequent `net netdb <verb>` in the
    // same process tree, or a future netdb-watcher holding the
    // same dir, could see stale data, and a crash during
    // shutdown could lose the last appended frame.
    if let Err(e) = netdb.close() {
        tracing::warn!(error = %e, "netdb close failed at end of CLI mutation");
    }
    r?;
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
    bytes_read: u64,
}

async fn run_tasks_ls(
    args: TasksLsArgs,
    output: Option<OutputFormat>,
    profile_netdb: Option<&std::path::Path>,
) -> Result<(), CliError> {
    let netdb = open_netdb(
        args.store.as_deref(),
        profile_netdb,
        args.origin,
        /*tasks=*/ true,
        false,
        /*create_if_missing=*/ false,
    )
    .await?;
    let adapter = netdb
        .try_tasks()
        .ok_or_else(|| sdk("NetDB has no tasks adapter wired"))?;
    // Collect under the read guard and drop it before the
    // synchronous stdout write. Pre-fix the guard lived across
    // `emit_value`, which means a concurrent `net netdb tasks
    // create` in the same process tree (and any future in-process
    // watcher) blocked on the writer lock while we drained
    // stdout; a piped `| jq` on a slow consumer stalled writers
    // indefinitely.
    let tasks: Vec<Task> = {
        let state_arc = adapter.state();
        let guard = state_arc.read();
        guard.all().cloned().collect()
    };
    let r = emit_value(OutputFormat::resolve_oneshot(output), &tasks)
        .map_err(|e| generic(format!("write tasks: {e}")));
    if let Err(e) = netdb.close() {
        tracing::warn!(error = %e, "netdb close failed at end of tasks ls");
    }
    r?;
    Ok(())
}

async fn run_memories_ls(
    args: MemoriesLsArgs,
    output: Option<OutputFormat>,
    profile_netdb: Option<&std::path::Path>,
) -> Result<(), CliError> {
    let netdb = open_netdb(
        args.store.as_deref(),
        profile_netdb,
        args.origin,
        false,
        /*memories=*/ true,
        /*create_if_missing=*/ false,
    )
    .await?;
    let adapter = netdb
        .try_memories()
        .ok_or_else(|| sdk("NetDB has no memories adapter wired"))?;
    // Drop the read guard before the synchronous stdout write -
    // see the run_tasks_ls comment for the rationale.
    let memories: Vec<Memory> = {
        let state_arc = adapter.state();
        let guard = state_arc.read();
        guard.all().cloned().collect()
    };
    let r = emit_value(OutputFormat::resolve_oneshot(output), &memories)
        .map_err(|e| generic(format!("write memories: {e}")));
    if let Err(e) = netdb.close() {
        tracing::warn!(error = %e, "netdb close failed at end of memories ls");
    }
    r?;
    Ok(())
}

async fn run_snapshot(
    args: SnapshotArgs,
    output: Option<OutputFormat>,
    profile_netdb: Option<&std::path::Path>,
) -> Result<(), CliError> {
    // `try_exists` distinguishes Ok(false)=absent from Err=stat
    // failure; pre-fix `.exists()` followed symlinks and returned
    // false on permission errors, letting a symlink at `--out`
    // pointing at a sensitive file or a permission-denied stat
    // skip the safety gate.
    if !args.force {
        match tokio::fs::try_exists(&args.out).await {
            Ok(true) => {
                return Err(crate::error::invalid_args(format!(
                    "{} already exists; pass --force to overwrite",
                    args.out.display()
                )));
            }
            Ok(false) => {}
            Err(e) => {
                return Err(generic(format!(
                    "failed to stat {}: {e}; pass --force to override",
                    args.out.display()
                )));
            }
        }
    }
    if !args.with_tasks && !args.with_memories {
        return Err(crate::error::invalid_args(
            "snapshot must include at least one of tasks or memories; \
             do not pass both --with-tasks=false and --with-memories=false",
        ));
    }
    let netdb = open_netdb(
        args.store.as_deref(),
        profile_netdb,
        args.origin,
        args.with_tasks,
        args.with_memories,
        false,
    )
    .await?;
    let snapshot = netdb
        .snapshot()
        .map_err(|e| sdk(format!("netdb snapshot: {e}")))?;
    let bytes = snapshot
        .encode()
        .map_err(|e| sdk(format!("netdb snapshot encode: {e}")))?;
    // `parent()` returns Some("") for a bare filename like
    // "snap.bin"; `create_dir_all("")` errors on Windows. Filter
    // it so we skip the dir-create step when the user passed a
    // bare path or a relative leaf.
    if let Some(parent) = args.out.parent().filter(|p| !p.as_os_str().is_empty()) {
        tokio::fs::create_dir_all(parent).await.map_err(|e| {
            generic(format!(
                "failed to create parent directory {}: {e}",
                parent.display()
            ))
        })?;
    }
    // Atomic + durable publish: write to a temp file next to
    // the final destination, fsync it, rename onto the target,
    // fsync the parent dir so the rename is durable. Pre-fix the
    // sequence stopped at `write` + `rename`, which is atomic
    // visibility-wise but not durable: a power loss after the
    // rename returned but before the page-cache / dirent flush
    // could still revert to the previous snapshot. The
    // tmp+rename layer alone closed the "truncated blob at the
    // documented path" hazard from #3; this commit closes the
    // crash-window hazard from #20.
    let pid = std::process::id();
    let tmp = args.out.with_extension(format!("tmp.{pid}"));
    let bytes_owned = bytes.clone();
    let tmp_for_write = tmp.clone();
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_for_write)?;
        f.write_all(&bytes_owned)?;
        f.sync_all()
    })
    .await
    .map_err(|e| generic(format!("snapshot tmp-write task panicked: {e}")))?
    .map_err(|e| generic(format!("write snapshot tmp {}: {e}", tmp.display())))?;
    tokio::fs::rename(&tmp, &args.out).await.map_err(|e| {
        // Best-effort cleanup of the temp file if rename failed;
        // ignore secondary errors.
        let tmp_for_cleanup = tmp.clone();
        tokio::spawn(async move {
            let _ = tokio::fs::remove_file(tmp_for_cleanup).await;
        });
        generic(format!(
            "rename snapshot tmp {} -> {}: {e}",
            tmp.display(),
            args.out.display()
        ))
    })?;
    // Fsync the parent directory so the dirent change from the
    // rename is durable. Best-effort on Windows (parent fsync is
    // a noop on NTFS dirents; ReadDirectoryChangesW + commit
    // semantics differ from POSIX), so we ignore the error there.
    if let Some(parent) = args.out.parent().filter(|p| !p.as_os_str().is_empty()) {
        let parent = parent.to_path_buf();
        let _ = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            let f = std::fs::File::open(parent)?;
            f.sync_all()
        })
        .await
        .map_err(|e| generic(format!("parent-dir fsync task panicked: {e}")))?;
    }
    let info = SnapshotResult {
        path: args.out.display().to_string(),
        bytes: bytes.len() as u64,
    };
    let r = emit_value(OutputFormat::resolve_oneshot(output), &info)
        .map_err(|e| generic(format!("write snapshot result: {e}")));
    if let Err(e) = netdb.close() {
        tracing::warn!(error = %e, "netdb close failed at end of snapshot");
    }
    r?;
    Ok(())
}

async fn open_netdb(
    store: Option<&std::path::Path>,
    profile_netdb: Option<&std::path::Path>,
    origin: u64,
    enable_tasks: bool,
    enable_memories: bool,
    create_if_missing: bool,
) -> Result<Arc<NetDb>, CliError> {
    // Resolution precedence: explicit --store > profile.netdb >
    // $XDG_DATA_HOME default. Pre-fix `profile.netdb` was ignored
    // entirely for every netdb subcommand, so an operator with
    // `netdb = "/srv/netdb"` in their `prod` profile and
    // `net --profile prod netdb tasks ls` landed in the default
    // path and writes mutations into the wrong store.
    let path = match store {
        Some(p) => p.to_path_buf(),
        None => match profile_netdb {
            Some(p) => p.to_path_buf(),
            None => default_netdb_path().ok_or_else(|| {
                generic("no $XDG_DATA_HOME / data dir available; pass --store <PATH>")
            })?,
        },
    };
    if create_if_missing {
        tokio::fs::create_dir_all(&path).await.map_err(|e| {
            generic(format!(
                "failed to create netdb directory {}: {e}",
                path.display()
            ))
        })?;
    } else {
        // Read paths refuse to silently fabricate an empty store —
        // a typo'd `--store /var/tmp/typo` would otherwise return
        // zero rows with no diagnostic. Surface a permission error
        // from `try_exists` distinctly so the operator gets the
        // right remediation (fix ACLs vs pass `--store`).
        match tokio::fs::try_exists(&path).await {
            Ok(true) => {}
            Ok(false) => {
                return Err(crate::error::invalid_args(format!(
                    "netdb store {} does not exist; pass --store <PATH> to an \
                     existing store or run a mutation first to create one",
                    path.display()
                )));
            }
            Err(e) => {
                return Err(generic(format!(
                    "failed to stat netdb store {}: {e}",
                    path.display()
                )));
            }
        }
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
    dirs::data_dir().map(|d| d.join("net-mesh").join("netdb"))
}

#[derive(Serialize)]
struct SnapshotResult {
    path: String,
    bytes: u64,
}
