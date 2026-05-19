//! `net-blob` — operator CLI for the Dataforts v0.2
//! substrate-owned blob CAS surface.
//!
//! Library-mode tool: opens a local `Redex` against the supplied
//! persistent directory, wraps it in a [`MeshBlobAdapter`], and
//! drives whichever subcommand was requested. No daemon, no IPC —
//! the operator runs this against a process-shared persistent dir
//! while the daemon is offline (or against a side-by-side dir for
//! inspection / triage workflows). For online ops a downstream
//! daemon-side RPC layer can wrap the same subcommand surface.
//!
//! # State persistence model
//!
//! **Chunk bytes persist on disk** via Redex — `put` in one CLI
//! run, `get` from another, the bytes round-trip cleanly.
//!
//! **Refcount-table + metrics state is per-process** — they live
//! inside [`MeshBlobAdapter`], which the CLI rebuilds on every
//! invocation. So:
//!
//! - `pin` / `unpin` / `gc` work *within* a single CLI invocation
//!   (e.g. `put + pin` chained from a script is meaningful inside
//!   one process tree).
//! - `ls` / `metrics` reflect *this* CLI run only — they will not
//!   show entries from prior `put` / `pin` runs.
//! - The intended home for cross-invocation refcount + metrics is
//!   the long-lived daemon. The CLI is at its best for the
//!   atomic ops: `put`, `get`, `stat`, `exists`, and the
//!   adapter-bound `metrics` of a single run.
//!
//! Persistent refcount + metrics across CLI runs is a separate
//! design step — likely an on-disk index in `<dir>/refcount.bin`
//! that the adapter constructor reads at startup, or a "walk
//! Redex on init" recovery path. Neither is in PR-5l's scope.
//!
//! Subcommands:
//!
//! - `put <file>` — content-address bytes; print the BlobRef hash.
//! - `get <hash> [--out <file>]` — fetch by hash, write to stdout
//!   or `--out`.
//! - `stat <hash> [--size <bytes>]` — print the BlobStat shape
//!   (size / replicas / encoding / last-seen).
//! - `exists <hash>` — exit 0 / 1 based on local presence.
//! - `ls` — list every hash tracked in the local refcount table.
//! - `pin <hash>` — pin against GC (unauth — operator path; the
//!   peer-facing `pin_authorized` is reserved for the chain-fold
//!   integration).
//! - `unpin <hash>` — release a pin.
//! - `gc [--retention <duration>] [--dry-run]` — run a sweep.
//! - `metrics` — print the adapter's Prometheus text body.
//!
//! `--format human|json` picks the output shape. Human is the
//! default; JSON is suitable for piping into `jq` or another tool
//! in operator scripts.

use std::fs;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand, ValueEnum};
use serde::Serialize;

use net::adapter::net::dataforts::blob::blob_tree::TreeNode;
use net::adapter::net::dataforts::blob::{
    BlobAdapter, BlobRef, BlobStat, Encoding, MeshBlobAdapter, RefcountEntry, RepairReport,
};
use net::adapter::net::redex::Redex;

/// `net-blob` — operator CLI for dataforts blob storage.
#[derive(Parser, Debug)]
#[command(
    name = "net-blob",
    version,
    about = "Operator CLI for the Dataforts v0.2 substrate-owned blob CAS",
    long_about = "Opens a local Redex against --dir and runs the requested op against \
                  a MeshBlobAdapter wrapping it. No daemon required."
)]
struct Cli {
    /// Persistent storage directory for the local Redex. Must
    /// match the path the daemon (if any) uses, otherwise the
    /// CLI is reading a different on-disk slice.
    #[arg(short = 'd', long, env = "NET_BLOB_DIR", default_value = "./blob-data")]
    dir: PathBuf,

    /// Output format. Human is the default; JSON suits operator
    /// scripts piping into `jq` etc.
    #[arg(short, long, value_enum, default_value_t = OutputFormat::Human)]
    format: OutputFormat,

    /// Adapter identity tag — surfaces in the Prometheus output
    /// + the Redex channel-namespace.
    #[arg(long, default_value = "cli")]
    adapter_id: String,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Store a file as a content-addressed blob; print the
    /// resolved BlobRef (URI / hash / size).
    Put {
        /// Path to read. Pass `-` to read stdin.
        path: String,
        /// URI prefix to stamp on the BlobRef. Defaults to
        /// `mesh://<hex>`.
        #[arg(long)]
        uri: Option<String>,
    },
    /// Fetch a blob by hex-encoded hash; write bytes to stdout or
    /// `--out`.
    Get {
        /// 64-char lowercase hex of the BLAKE3-256 hash.
        hash: String,
        /// File to write to. Defaults to stdout (binary).
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Optional size hint for the BlobRef lookup. Not used by
        /// the local fetch path; included for round-trip parity
        /// with `stat`.
        #[arg(long, default_value_t = 0)]
        size: u64,
    },
    /// Print the BlobStat shape for `hash` (size, replicas,
    /// encoding, last-seen).
    Stat {
        /// 64-char lowercase hex of the BLAKE3-256 hash.
        hash: String,
        /// Size to stamp on the BlobRef passed to `stat`. The
        /// stat path returns this size when the adapter doesn't
        /// track per-blob metadata; pass the known size for
        /// faithful output.
        #[arg(long, default_value_t = 0)]
        size: u64,
    },
    /// Exit 0 when the chunk is locally present, 1 when absent.
    Exists {
        /// 64-char lowercase hex of the BLAKE3-256 hash.
        hash: String,
    },
    /// List every hash in the local refcount table with its
    /// refcount + pin status + first/last seen.
    Ls,
    /// Pin a hash against GC. Uses the unauth `pin` variant —
    /// operator path. The peer-facing `pin_authorized` lives
    /// inside the substrate for the chain-fold integration.
    Pin {
        /// 64-char lowercase hex of the BLAKE3-256 hash.
        hash: String,
    },
    /// Release a pin. Mirrors `pin` semantics — unauth variant.
    Unpin {
        /// 64-char lowercase hex of the BLAKE3-256 hash.
        hash: String,
    },
    /// Run a GC sweep. Returns the count of chunks reclaimed.
    Gc {
        /// Retention floor — entries newer than this aren't
        /// candidates even with refcount=0. Accepts `30s`, `5m`,
        /// `1h`, `24h`, `7d`.
        #[arg(long, default_value = "24h")]
        retention: String,
        /// Mark the sweep as running under disk pressure; the
        /// retention floor is bypassed and every refcount=0 +
        /// unpinned hash is eligible.
        #[arg(long)]
        disk_pressure: bool,
        /// Skip the actual delete; just list what would be
        /// swept.
        #[arg(long)]
        dry_run: bool,
    },
    /// Print the adapter's Prometheus text body.
    Metrics,
    /// Active-overflow operator commands (v0.3). Status-only
    /// in the current ship; future actions land here.
    Overflow {
        #[command(subcommand)]
        action: OverflowCmd,
    },
    /// Repair a v0.3 `BlobRef::Tree` blob with
    /// `Encoding::ReedSolomon` encoding: walk every stripe,
    /// reconstruct any missing data chunks from parity, re-store
    /// them under their original content-addressed hashes. Prints
    /// the `RepairReport` (stripes walked / repaired /
    /// unrecoverable, chunks restored).
    Repair {
        /// 64-char lowercase hex of the BLAKE3-256 root hash.
        hash: String,
        /// Total blob size in bytes — required to construct the
        /// BlobRef::Tree. Available from `stat` on the original
        /// store.
        #[arg(long)]
        size: u64,
        /// Tree depth (1..=4). Required to construct the
        /// BlobRef::Tree; the depth lives in the wire BlobRef
        /// but isn't recoverable from the root hash alone.
        #[arg(long)]
        depth: u8,
    },
    /// Walk a v0.3 `BlobRef::Tree` blob and print the manifest
    /// node hierarchy: depth + arity + per-stripe shape for
    /// erasure-coded leaves. Useful for diagnosing manifest-tree
    /// shape regressions and verifying tree depth claims match
    /// actual structure.
    Tree {
        /// 64-char lowercase hex of the BLAKE3-256 root hash.
        hash: String,
        #[arg(long)]
        size: u64,
        #[arg(long)]
        depth: u8,
    },
    /// Walk every reachable chunk of a v0.3 `BlobRef::Tree`,
    /// fetch its bytes, and verify the BLAKE3 hash matches the
    /// manifest's recorded hash. Reports the count of healthy /
    /// missing / corrupted chunks. Operators run after a
    /// suspected disk corruption event to identify which blobs
    /// need `repair`.
    Verify {
        /// 64-char lowercase hex of the BLAKE3-256 root hash.
        hash: String,
        #[arg(long)]
        size: u64,
        #[arg(long)]
        depth: u8,
    },
    /// Walk a v0.3 `BlobRef::Tree` to a specific byte offset and
    /// print the chunk + sub-offset that byte lives in. Reports
    /// the manifest-tree path (root → internal nodes → leaf) and
    /// the resolved chunk's hash + offset-within-chunk. For RS-
    /// encoded blobs, the stripe index + per-stripe encoding are
    /// included so the operator can correlate a byte offset with
    /// its parity-protected stripe.
    Path {
        /// 64-char lowercase hex of the BLAKE3-256 root hash.
        hash: String,
        #[arg(long)]
        size: u64,
        #[arg(long)]
        depth: u8,
        /// Byte offset within the logical blob to resolve.
        /// `0..size`.
        #[arg(long)]
        offset: u64,
    },
}

#[derive(Subcommand, Debug)]
enum OverflowCmd {
    /// Print the local overflow state: the configured
    /// `enabled` boolean, the runtime `active` flag (set by
    /// the most recent tick), the configured thresholds, and
    /// the cumulative counter family (admitted / rejected /
    /// errors / hysteresis transitions).
    Status,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum OutputFormat {
    Human,
    Json,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("net-blob: {}", e);
            ExitCode::from(1)
        }
    }
}

async fn run(cli: Cli) -> Result<ExitCode, Box<dyn std::error::Error>> {
    // Build the persistent Redex + wrap in a MeshBlobAdapter.
    let redex = Arc::new(Redex::new().with_persistent_dir(&cli.dir));
    let adapter = MeshBlobAdapter::new(&cli.adapter_id, redex).with_persistent(true);

    match cli.cmd {
        Cmd::Put { path, uri } => cmd_put(&adapter, &path, uri.as_deref(), cli.format).await,
        Cmd::Get { hash, out, size } => {
            cmd_get(&adapter, &hash, out.as_deref(), size, cli.format).await
        }
        Cmd::Stat { hash, size } => cmd_stat(&adapter, &hash, size, cli.format).await,
        Cmd::Exists { hash } => cmd_exists(&adapter, &hash).await,
        Cmd::Ls => cmd_ls(&adapter, cli.format),
        Cmd::Pin { hash } => cmd_pin(&adapter, &hash, cli.format),
        Cmd::Unpin { hash } => cmd_unpin(&adapter, &hash, cli.format),
        Cmd::Gc {
            retention,
            disk_pressure,
            dry_run,
        } => cmd_gc(&adapter, &retention, disk_pressure, dry_run, cli.format).await,
        Cmd::Metrics => cmd_metrics(&adapter, cli.format),
        Cmd::Overflow { action } => match action {
            OverflowCmd::Status => cmd_overflow_status(&adapter, cli.format),
        },
        Cmd::Repair { hash, size, depth } => {
            cmd_repair(&adapter, &hash, size, depth, cli.format).await
        }
        Cmd::Tree { hash, size, depth } => cmd_tree(&adapter, &hash, size, depth, cli.format).await,
        Cmd::Verify { hash, size, depth } => {
            cmd_verify(&adapter, &hash, size, depth, cli.format).await
        }
        Cmd::Path {
            hash,
            size,
            depth,
            offset,
        } => cmd_path(&adapter, &hash, size, depth, offset, cli.format).await,
    }
}

// ============================================================================
// Subcommand implementations
// ============================================================================

async fn cmd_put(
    adapter: &MeshBlobAdapter,
    path: &str,
    uri: Option<&str>,
    fmt: OutputFormat,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let bytes = read_input(path)?;
    let hash: [u8; 32] = blake3::hash(&bytes).into();
    let uri = uri
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("mesh://{}", hex32(&hash)));
    let blob = BlobRef::small(uri.clone(), hash, bytes.len() as u64);
    adapter.store(&blob, &bytes).await?;

    #[derive(Serialize)]
    struct PutOut<'a> {
        uri: &'a str,
        hash: String,
        size: u64,
    }
    let out = PutOut {
        uri: &uri,
        hash: hex32(&hash),
        size: bytes.len() as u64,
    };
    match fmt {
        OutputFormat::Human => {
            println!("stored: {}", out.uri);
            println!("hash:   {}", out.hash);
            println!("size:   {} bytes", out.size);
        }
        OutputFormat::Json => println!("{}", serde_json::to_string(&out)?),
    }
    Ok(ExitCode::SUCCESS)
}

async fn cmd_get(
    adapter: &MeshBlobAdapter,
    hash_hex: &str,
    out_path: Option<&std::path::Path>,
    size: u64,
    fmt: OutputFormat,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    // `get` writes raw blob bytes — JSON formatting isn't
    // meaningful. Reject explicitly so operators piping into a
    // JSON consumer see a clear error rather than corrupted bytes.
    if matches!(fmt, OutputFormat::Json) {
        return Err(
            "get: --format json not supported; output is raw bytes (stdout or --out file)".into(),
        );
    }
    let hash = parse_hash(hash_hex)?;
    let blob = BlobRef::small(format!("mesh://{}", hex32(&hash)), hash, size);
    let bytes = adapter.fetch(&blob).await?;
    match out_path {
        Some(p) => {
            // Refuse to clobber an existing file or follow a
            // symlink. The CLI may run with elevated privileges
            // and a naive `fs::write` would happily overwrite
            // /etc/passwd or follow a symlink an attacker
            // pre-planted at the operator-supplied path.
            // `create_new(true)` errors if the path already
            // exists for any reason — that includes existing
            // symlinks (the symlink path "exists" even if its
            // target doesn't). Operators who legitimately want to
            // overwrite must `rm` the file first; the noisy
            // failure mode is the correct default for an operator
            // CLI.
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(p)
                .map_err(|e| {
                    format!(
                        "net-blob: refused to write to {}: {} (existing path or symlink; \
                         remove it first if overwrite is intended)",
                        p.display(),
                        e
                    )
                })?;
            f.write_all(&bytes)?;
            eprintln!("net-blob: wrote {} bytes to {}", bytes.len(), p.display());
        }
        None => {
            io::stdout().write_all(&bytes)?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

async fn cmd_stat(
    adapter: &MeshBlobAdapter,
    hash_hex: &str,
    size: u64,
    fmt: OutputFormat,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let hash = parse_hash(hash_hex)?;
    let blob = BlobRef::small(format!("mesh://{}", hex32(&hash)), hash, size);
    let stat: BlobStat = adapter.stat(&blob).await?;
    print_stat(&hex32(&hash), &stat, fmt)?;
    Ok(ExitCode::SUCCESS)
}

async fn cmd_exists(
    adapter: &MeshBlobAdapter,
    hash_hex: &str,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let hash = parse_hash(hash_hex)?;
    let blob = BlobRef::small(format!("mesh://{}", hex32(&hash)), hash, 0);
    let exists = adapter.exists(&blob).await?;
    if exists {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::from(1))
    }
}

fn cmd_ls(
    adapter: &MeshBlobAdapter,
    fmt: OutputFormat,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let mut entries: Vec<([u8; 32], RefcountEntry)> = adapter.refcount_table().snapshot();
    entries.sort_by_key(|(h, _)| *h);
    print_ls(&entries, fmt)?;
    Ok(ExitCode::SUCCESS)
}

fn cmd_pin(
    adapter: &MeshBlobAdapter,
    hash_hex: &str,
    fmt: OutputFormat,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let hash = parse_hash(hash_hex)?;
    let now = now_unix_ms();
    adapter.pin(hash, now);
    match fmt {
        OutputFormat::Human => println!("pinned: {}", hex32(&hash)),
        OutputFormat::Json => println!(
            "{}",
            serde_json::json!({"op": "pin", "hash": hex32(&hash), "ts": now})
        ),
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_unpin(
    adapter: &MeshBlobAdapter,
    hash_hex: &str,
    fmt: OutputFormat,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let hash = parse_hash(hash_hex)?;
    let now = now_unix_ms();
    adapter.unpin(hash, now);
    match fmt {
        OutputFormat::Human => println!("unpinned: {}", hex32(&hash)),
        OutputFormat::Json => println!(
            "{}",
            serde_json::json!({"op": "unpin", "hash": hex32(&hash), "ts": now})
        ),
    }
    Ok(ExitCode::SUCCESS)
}

async fn cmd_gc(
    adapter: &MeshBlobAdapter,
    retention: &str,
    disk_pressure: bool,
    dry_run: bool,
    fmt: OutputFormat,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let retention = parse_duration(retention)?;
    let now = now_unix_ms();
    if dry_run {
        let candidates = adapter
            .refcount_table()
            .deletable_hashes(now, retention, disk_pressure);
        match fmt {
            OutputFormat::Human => {
                println!("gc dry-run: {} candidates", candidates.len());
                for h in &candidates {
                    println!("  {}", hex32(h));
                }
            }
            OutputFormat::Json => println!(
                "{}",
                serde_json::json!({
                    "dry_run": true,
                    "retention_secs": retention.as_secs(),
                    "disk_pressure": disk_pressure,
                    "candidates": candidates.iter().map(hex32).collect::<Vec<_>>(),
                })
            ),
        }
        return Ok(ExitCode::SUCCESS);
    }
    // The adapter's sweep_gc uses its configured retention floor.
    // Wrap with the operator-supplied retention by rebuilding the
    // adapter — the floor is a builder field, so this is the
    // cleanest way to honor the CLI flag without leaking
    // mutability into the adapter type.
    let adapter_with_retention = adapter.clone().with_retention_floor(retention);
    let swept = adapter_with_retention.sweep_gc(now, disk_pressure).await?;
    match fmt {
        OutputFormat::Human => println!("gc: swept {} chunks", swept),
        OutputFormat::Json => println!(
            "{}",
            serde_json::json!({
                "dry_run": false,
                "swept": swept,
                "retention_secs": retention.as_secs(),
                "disk_pressure": disk_pressure,
            })
        ),
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_metrics(
    adapter: &MeshBlobAdapter,
    fmt: OutputFormat,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    // Prometheus text body is the only supported metrics shape.
    // Reject --format json explicitly so operators piping into
    // `jq` see a clear error rather than a downstream JSON parse
    // failure.
    if matches!(fmt, OutputFormat::Json) {
        return Err(
            "metrics: --format json not supported; only Prometheus text exposition is emitted"
                .into(),
        );
    }
    let body = adapter.prometheus_text();
    print!("{}", body);
    Ok(ExitCode::SUCCESS)
}

fn cmd_overflow_status(
    adapter: &MeshBlobAdapter,
    fmt: OutputFormat,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    // The CLI runs against a freshly-constructed adapter — the
    // in-process `overflow_active` flag is always `false` here
    // (no tick has fired in this process). What's persistent is
    // the configured boolean + thresholds, which the operator
    // reads to confirm the daemon-side config is what they
    // expect. The cumulative counters are also process-local
    // — `net-blob` is the wrong tool for cross-restart counter
    // history; that's the Prometheus scraper's job. Print what
    // we have, label what we don't.
    let config = adapter.overflow_config();
    let active = adapter.overflow_active();
    let snap = adapter.metrics().snapshot();
    let o = &snap.overflow;
    match fmt {
        OutputFormat::Human => {
            println!("overflow status (adapter={})", adapter.adapter_id());
            println!("  configured enabled:        {}", config.enabled);
            println!("  runtime active (this proc): {}", active);
            println!(
                "  high_water_ratio:          {:.3}",
                config.high_water_ratio
            );
            println!("  low_water_ratio:           {:.3}", config.low_water_ratio);
            println!(
                "  max_pushes_per_tick:       {}",
                config.max_pushes_per_tick
            );
            println!("  scope:                     {:?}", config.scope);
            println!("  tick_interval_ms:          {}", config.tick_interval_ms);
            println!("  --- counters (this process) ---");
            println!("  pushes_admitted_total:     {}", o.pushes_admitted_total);
            println!("  push_errors_total:         {}", o.push_errors_total);
            println!("  pushed_bytes_total:        {}", o.pushed_bytes_total);
            println!(
                "  rejected_no_target_total:  {}",
                o.rejected_no_target_total
            );
            println!(
                "  rejected (no_storage_cap): {}",
                o.rejected_no_storage_cap_total
            );
            println!(
                "  rejected (not_participating): {}",
                o.rejected_not_participating_total
            );
            println!(
                "  rejected (sender_not_overflowing): {}",
                o.rejected_sender_not_overflowing_total
            );
            println!(
                "  rejected (unhealthy):      {}",
                o.rejected_unhealthy_total
            );
            println!(
                "  rejected (scope_mismatch): {}",
                o.rejected_scope_mismatch_total
            );
            println!(
                "  rejected (insufficient_disk): {}",
                o.rejected_insufficient_disk_total
            );
            println!(
                "  high_water_triggered_total: {}",
                o.high_water_triggered_total
            );
            println!("  low_water_cleared_total:   {}", o.low_water_cleared_total);
            println!("  disk_ratio (last tick):    {:.3}", o.disk_ratio);
        }
        OutputFormat::Json => println!(
            "{}",
            serde_json::json!({
                "adapter": adapter.adapter_id(),
                "config": {
                    "enabled": config.enabled,
                    "high_water_ratio": config.high_water_ratio,
                    "low_water_ratio": config.low_water_ratio,
                    "max_pushes_per_tick": config.max_pushes_per_tick,
                    "scope": format!("{:?}", config.scope),
                    "tick_interval_ms": config.tick_interval_ms,
                },
                "active": active,
                "counters": {
                    "pushes_admitted_total": o.pushes_admitted_total,
                    "push_errors_total": o.push_errors_total,
                    "pushed_bytes_total": o.pushed_bytes_total,
                    "rejected_no_target_total": o.rejected_no_target_total,
                    "rejected_no_storage_cap_total": o.rejected_no_storage_cap_total,
                    "rejected_not_participating_total": o.rejected_not_participating_total,
                    "rejected_sender_not_overflowing_total": o.rejected_sender_not_overflowing_total,
                    "rejected_unhealthy_total": o.rejected_unhealthy_total,
                    "rejected_scope_mismatch_total": o.rejected_scope_mismatch_total,
                    "rejected_insufficient_disk_total": o.rejected_insufficient_disk_total,
                    "high_water_triggered_total": o.high_water_triggered_total,
                    "low_water_cleared_total": o.low_water_cleared_total,
                    "disk_ratio": o.disk_ratio,
                },
            })
        ),
    }
    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// v0.3 Phase C/D subcommands: repair, tree, verify
// ============================================================================

/// Boxed pinned future the recursive `walk_tree_print` /
/// `verify_walk` helpers return — async-recursion in stable
/// Rust requires the boxed return type. Aliased here so the
/// signature stays readable.
type RecursiveWalkFuture<'a> = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<(), Box<dyn std::error::Error>>> + Send + 'a>,
>;

/// Construct a `BlobRef::Tree` from operator-supplied parts. The
/// CLI takes (hash, size, depth) because the depth lives in the
/// wire BlobRef envelope and isn't recoverable from the root
/// chunk alone. Stamps `Encoding::Replicated` by default — the
/// repair/tree/verify subcommands re-derive per-leaf encoding
/// from the manifest itself, so the BlobRef-level encoding here
/// only affects the repair report's `replicated_leaves_skipped`
/// counter (and that path is robust to a mismatch).
fn build_tree_ref(
    hash_hex: &str,
    size: u64,
    depth: u8,
) -> Result<BlobRef, Box<dyn std::error::Error>> {
    let hash = parse_hash(hash_hex)?;
    let uri = format!("mesh://{}", hex32(&hash));
    let blob_ref = BlobRef::tree(uri, Encoding::Replicated, hash, size, depth)
        .map_err(|e| format!("invalid BlobRef::Tree parts: {}", e))?;
    Ok(blob_ref)
}

async fn cmd_repair(
    adapter: &MeshBlobAdapter,
    hash_hex: &str,
    size: u64,
    depth: u8,
    fmt: OutputFormat,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let blob_ref = build_tree_ref(hash_hex, size, depth)?;
    let report: RepairReport = adapter.repair_blob(&blob_ref).await?;
    match fmt {
        OutputFormat::Human => {
            println!("repair: {}", hash_hex);
            println!("  stripes_walked:              {}", report.stripes_walked);
            println!(
                "  stripes_already_healthy:     {}",
                report.stripes_already_healthy
            );
            println!("  stripes_repaired:            {}", report.stripes_repaired);
            println!("  chunks_restored:             {}", report.chunks_restored);
            println!(
                "  stripes_unrecoverable:       {}",
                report.stripes_unrecoverable
            );
            println!(
                "  replicated_stripes_skipped:  {}",
                report.replicated_stripes_skipped
            );
            println!(
                "  replicated_leaves_skipped:   {}",
                report.replicated_leaves_skipped
            );
        }
        OutputFormat::Json => println!(
            "{}",
            serde_json::json!({
                "hash": hash_hex,
                "stripes_walked": report.stripes_walked,
                "stripes_already_healthy": report.stripes_already_healthy,
                "stripes_repaired": report.stripes_repaired,
                "chunks_restored": report.chunks_restored,
                "stripes_unrecoverable": report.stripes_unrecoverable,
                "replicated_stripes_skipped": report.replicated_stripes_skipped,
                "replicated_leaves_skipped": report.replicated_leaves_skipped,
            })
        ),
    }
    // Exit 0 when nothing unrecoverable — operators commonly chain
    // `repair --format json | jq` and rely on exit status for
    // "did this need human attention".
    if report.stripes_unrecoverable > 0 {
        Ok(ExitCode::from(2))
    } else {
        Ok(ExitCode::SUCCESS)
    }
}

async fn cmd_tree(
    adapter: &MeshBlobAdapter,
    hash_hex: &str,
    size: u64,
    depth: u8,
    fmt: OutputFormat,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let blob_ref = build_tree_ref(hash_hex, size, depth)?;
    let root_hash = *blob_ref.tree_root_hash().expect("Tree built above");
    let mut json_nodes: Vec<serde_json::Value> = Vec::new();
    walk_tree_print(adapter, root_hash, 0, fmt, &mut json_nodes).await?;
    if matches!(fmt, OutputFormat::Json) {
        println!(
            "{}",
            serde_json::json!({
                "root_hash": hash_hex,
                "size": size,
                "depth": depth,
                "nodes": json_nodes,
            })
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// Recursive helper for `cmd_tree`. Walks down from `node_hash`,
/// printing one line per node in Human mode or appending one
/// JSON entry per node in JSON mode.
fn walk_tree_print<'a>(
    adapter: &'a MeshBlobAdapter,
    node_hash: [u8; 32],
    indent: usize,
    fmt: OutputFormat,
    json_nodes: &'a mut Vec<serde_json::Value>,
) -> RecursiveWalkFuture<'a> {
    Box::pin(async move {
        let bytes = adapter.fetch_chunk(&node_hash).await?;
        let node = TreeNode::decode(&bytes)?;
        let pad = "  ".repeat(indent);
        match &node {
            TreeNode::Internal { children } => {
                match fmt {
                    OutputFormat::Human => println!(
                        "{}internal[{}] {} ({} bytes covered)",
                        pad,
                        children.len(),
                        hex32(&node_hash),
                        node.covered_bytes()
                    ),
                    OutputFormat::Json => json_nodes.push(serde_json::json!({
                        "hash": hex32(&node_hash),
                        "kind": "internal",
                        "depth": indent,
                        "arity": children.len(),
                        "covered_bytes": node.covered_bytes(),
                    })),
                }
                for (child_hash, _) in children {
                    walk_tree_print(adapter, *child_hash, indent + 1, fmt, json_nodes).await?;
                }
            }
            TreeNode::Leaf { chunks } => match fmt {
                OutputFormat::Human => println!(
                    "{}leaf[{}] {} ({} bytes covered)",
                    pad,
                    chunks.len(),
                    hex32(&node_hash),
                    node.covered_bytes()
                ),
                OutputFormat::Json => json_nodes.push(serde_json::json!({
                    "hash": hex32(&node_hash),
                    "kind": "leaf",
                    "depth": indent,
                    "chunks": chunks.len(),
                    "covered_bytes": node.covered_bytes(),
                })),
            },
            TreeNode::ErasureLeaf { stripes } => match fmt {
                OutputFormat::Human => {
                    println!(
                        "{}erasure_leaf[{} stripes] {} ({} bytes covered)",
                        pad,
                        stripes.len(),
                        hex32(&node_hash),
                        node.covered_bytes()
                    );
                    for (i, stripe) in stripes.iter().enumerate() {
                        let pad2 = "  ".repeat(indent + 1);
                        let data_count = stripe.chunks.iter().filter(|c| c.is_data()).count();
                        let parity_count = stripe.chunks.iter().filter(|c| c.is_parity()).count();
                        println!(
                            "{}stripe[{}] {:?}: {} data + {} parity ({} bytes)",
                            pad2,
                            i,
                            stripe.encoding,
                            data_count,
                            parity_count,
                            stripe.covered_bytes()
                        );
                    }
                }
                OutputFormat::Json => json_nodes.push(serde_json::json!({
                    "hash": hex32(&node_hash),
                    "kind": "erasure_leaf",
                    "depth": indent,
                    "stripes": stripes.len(),
                    "covered_bytes": node.covered_bytes(),
                })),
            },
        }
        Ok(())
    })
}

async fn cmd_verify(
    adapter: &MeshBlobAdapter,
    hash_hex: &str,
    size: u64,
    depth: u8,
    fmt: OutputFormat,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let blob_ref = build_tree_ref(hash_hex, size, depth)?;
    let root_hash = *blob_ref.tree_root_hash().expect("Tree built above");
    let mut healthy = 0u64;
    let mut missing = 0u64;
    let mut corrupted = 0u64;
    verify_walk(
        adapter,
        root_hash,
        &mut healthy,
        &mut missing,
        &mut corrupted,
    )
    .await?;
    match fmt {
        OutputFormat::Human => {
            println!("verify: {}", hash_hex);
            println!("  healthy:    {}", healthy);
            println!("  missing:    {}", missing);
            println!("  corrupted:  {}", corrupted);
        }
        OutputFormat::Json => println!(
            "{}",
            serde_json::json!({
                "hash": hash_hex,
                "healthy": healthy,
                "missing": missing,
                "corrupted": corrupted,
            })
        ),
    }
    if missing > 0 || corrupted > 0 {
        Ok(ExitCode::from(2))
    } else {
        Ok(ExitCode::SUCCESS)
    }
}

/// Recursive walker for `cmd_verify`. Counts every reachable
/// chunk — data, parity, manifest nodes — and verifies each
/// fetched byte sequence hashes back to its expected hash.
fn verify_walk<'a>(
    adapter: &'a MeshBlobAdapter,
    node_hash: [u8; 32],
    healthy: &'a mut u64,
    missing: &'a mut u64,
    corrupted: &'a mut u64,
) -> RecursiveWalkFuture<'a> {
    Box::pin(async move {
        // First verify the manifest node itself.
        let bytes = match adapter.fetch_chunk(&node_hash).await {
            Ok(b) => b,
            Err(_) => {
                *missing = missing.saturating_add(1);
                return Ok(());
            }
        };
        let computed: [u8; 32] = blake3::hash(&bytes).into();
        if computed != node_hash {
            *corrupted = corrupted.saturating_add(1);
            return Ok(());
        }
        *healthy = healthy.saturating_add(1);

        let node = TreeNode::decode(&bytes)?;
        match node {
            TreeNode::Internal { children } => {
                for (child_hash, _) in children {
                    verify_walk(adapter, child_hash, healthy, missing, corrupted).await?;
                }
            }
            TreeNode::Leaf { chunks } => {
                for chunk in chunks {
                    verify_chunk(adapter, &chunk.hash, healthy, missing, corrupted).await;
                }
            }
            TreeNode::ErasureLeaf { stripes } => {
                for stripe in stripes {
                    for chunk in stripe.chunks {
                        verify_chunk(adapter, &chunk.hash, healthy, missing, corrupted).await;
                    }
                }
            }
        }
        Ok(())
    })
}

async fn verify_chunk(
    adapter: &MeshBlobAdapter,
    hash: &[u8; 32],
    healthy: &mut u64,
    missing: &mut u64,
    corrupted: &mut u64,
) {
    match adapter.fetch_chunk(hash).await {
        Ok(bytes) => {
            let computed: [u8; 32] = blake3::hash(&bytes).into();
            if computed == *hash {
                *healthy = healthy.saturating_add(1);
            } else {
                *corrupted = corrupted.saturating_add(1);
            }
        }
        Err(_) => *missing = missing.saturating_add(1),
    }
}

async fn cmd_path(
    adapter: &MeshBlobAdapter,
    hash_hex: &str,
    size: u64,
    depth: u8,
    offset: u64,
    fmt: OutputFormat,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    if offset >= size {
        return Err(format!(
            "path: offset {} is at or past the blob's logical size {}",
            offset, size
        )
        .into());
    }
    let blob_ref = build_tree_ref(hash_hex, size, depth)?;
    let root_hash = *blob_ref.tree_root_hash().expect("Tree built above");

    // Descend through internal nodes, tracking the visited path
    // hashes for the operator's report. The descent stops when
    // we reach a leaf (Replicated or ErasureLeaf).
    let mut path_hashes: Vec<String> = vec![hex32(&root_hash)];
    let mut current_hash = root_hash;
    let mut current_base: u64 = 0;
    let mut current_size: u64 = size;
    loop {
        let bytes = adapter.fetch_chunk(&current_hash).await?;
        let node = TreeNode::decode(&bytes)?;
        match node {
            TreeNode::Internal { children } => {
                // Pick the child whose subtree contains the offset.
                let mut child_offset: u64 = current_base;
                let mut picked: Option<([u8; 32], u64, u64)> = None;
                for (child_hash, child_size) in children {
                    let child_end = child_offset.saturating_add(child_size);
                    if offset >= child_offset && offset < child_end {
                        picked = Some((child_hash, child_offset, child_size));
                        break;
                    }
                    child_offset = child_end;
                }
                let (next_hash, next_base, next_size) = picked.ok_or_else(|| {
                    format!(
                        "path: internal node at {} has no child covering offset {} \
                         (subtree spans [{}, {}))",
                        hex32(&current_hash),
                        offset,
                        current_base,
                        current_base.saturating_add(current_size)
                    )
                })?;
                path_hashes.push(hex32(&next_hash));
                current_hash = next_hash;
                current_base = next_base;
                current_size = next_size;
            }
            TreeNode::Leaf { chunks } => {
                let mut chunk_offset = current_base;
                for chunk in chunks {
                    let chunk_size_u64 = chunk.size as u64;
                    let chunk_end = chunk_offset.saturating_add(chunk_size_u64);
                    if offset >= chunk_offset && offset < chunk_end {
                        let sub_offset = offset - chunk_offset;
                        return print_path_result(
                            hash_hex,
                            offset,
                            &path_hashes,
                            &PathResult {
                                leaf_kind: "leaf",
                                stripe_index: None,
                                stripe_encoding: None,
                                chunk_hash: hex32(&chunk.hash),
                                chunk_size: chunk.size,
                                chunk_role: "data".to_owned(),
                                sub_offset,
                            },
                            fmt,
                        );
                    }
                    chunk_offset = chunk_end;
                }
                return Err(format!(
                    "path: leaf at {} had no chunk covering offset {} (offset \
                     past last chunk)",
                    hex32(&current_hash),
                    offset
                )
                .into());
            }
            TreeNode::ErasureLeaf { stripes } => {
                let mut stripe_offset = current_base;
                for (i, stripe) in stripes.iter().enumerate() {
                    let stripe_size = stripe.covered_bytes();
                    let stripe_end = stripe_offset.saturating_add(stripe_size);
                    if offset >= stripe_offset && offset < stripe_end {
                        let mut chunk_offset = stripe_offset;
                        for chunk in stripe.chunks.iter().filter(|c| c.is_data()) {
                            let chunk_size_u64 = chunk.size as u64;
                            let chunk_end = chunk_offset.saturating_add(chunk_size_u64);
                            if offset >= chunk_offset && offset < chunk_end {
                                let sub_offset = offset - chunk_offset;
                                let enc_label = match stripe.encoding {
                                    Encoding::Replicated => "Replicated".to_owned(),
                                    Encoding::ReedSolomon { k, m } => {
                                        format!("ReedSolomon(k={}, m={})", k, m)
                                    }
                                };
                                return print_path_result(
                                    hash_hex,
                                    offset,
                                    &path_hashes,
                                    &PathResult {
                                        leaf_kind: "erasure_leaf",
                                        stripe_index: Some(i),
                                        stripe_encoding: Some(enc_label),
                                        chunk_hash: hex32(&chunk.hash),
                                        chunk_size: chunk.size,
                                        chunk_role: "data".to_owned(),
                                        sub_offset,
                                    },
                                    fmt,
                                );
                            }
                            chunk_offset = chunk_end;
                        }
                    }
                    stripe_offset = stripe_end;
                }
                return Err(format!(
                    "path: erasure leaf at {} had no stripe covering offset {}",
                    hex32(&current_hash),
                    offset
                )
                .into());
            }
        }
    }
}

struct PathResult {
    leaf_kind: &'static str,
    stripe_index: Option<usize>,
    stripe_encoding: Option<String>,
    chunk_hash: String,
    chunk_size: u32,
    chunk_role: String,
    sub_offset: u64,
}

fn print_path_result(
    blob_hash_hex: &str,
    offset: u64,
    path_hashes: &[String],
    result: &PathResult,
    fmt: OutputFormat,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    match fmt {
        OutputFormat::Human => {
            println!("path: blob={} offset={}", blob_hash_hex, offset);
            println!("  manifest path:");
            for (depth, h) in path_hashes.iter().enumerate() {
                println!("    [{}] {}", depth, h);
            }
            println!("  leaf_kind:      {}", result.leaf_kind);
            if let Some(i) = result.stripe_index {
                println!("  stripe_index:   {}", i);
            }
            if let Some(enc) = &result.stripe_encoding {
                println!("  encoding:       {}", enc);
            }
            println!("  chunk_hash:     {}", result.chunk_hash);
            println!("  chunk_size:     {} bytes", result.chunk_size);
            println!("  chunk_role:     {}", result.chunk_role);
            println!(
                "  sub_offset:     {} (byte within the chunk)",
                result.sub_offset
            );
        }
        OutputFormat::Json => println!(
            "{}",
            serde_json::json!({
                "blob_hash": blob_hash_hex,
                "offset": offset,
                "manifest_path": path_hashes,
                "leaf_kind": result.leaf_kind,
                "stripe_index": result.stripe_index,
                "stripe_encoding": result.stripe_encoding,
                "chunk_hash": result.chunk_hash,
                "chunk_size": result.chunk_size,
                "chunk_role": result.chunk_role,
                "sub_offset": result.sub_offset,
            })
        ),
    }
    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// Output helpers
// ============================================================================

fn print_stat(
    hash_hex: &str,
    stat: &BlobStat,
    fmt: OutputFormat,
) -> Result<(), Box<dyn std::error::Error>> {
    #[derive(Serialize)]
    struct StatOut<'a> {
        hash: &'a str,
        size: u64,
        replicas_observed: u32,
        replica_target: Option<u8>,
        last_seen_unix_ms: Option<u64>,
        encoding: Option<String>,
    }
    let out = StatOut {
        hash: hash_hex,
        size: stat.size,
        replicas_observed: stat.replicas_observed,
        replica_target: stat.replica_target,
        last_seen_unix_ms: stat.last_seen_unix_ms,
        encoding: stat.encoding.map(|e| format!("{:?}", e)),
    };
    match fmt {
        OutputFormat::Human => {
            println!("hash:               {}", out.hash);
            println!("size:               {} bytes", out.size);
            println!("replicas_observed:  {}", out.replicas_observed);
            if let Some(t) = out.replica_target {
                println!("replica_target:     {}", t);
            }
            if let Some(ts) = out.last_seen_unix_ms {
                println!("last_seen_unix_ms:  {}", ts);
            }
            if let Some(e) = &out.encoding {
                println!("encoding:           {}", e);
            }
        }
        OutputFormat::Json => println!("{}", serde_json::to_string(&out)?),
    }
    Ok(())
}

fn print_ls(
    entries: &[([u8; 32], RefcountEntry)],
    fmt: OutputFormat,
) -> Result<(), Box<dyn std::error::Error>> {
    #[derive(Serialize)]
    struct LsRow {
        hash: String,
        refcount: u32,
        pinned: bool,
        first_seen_unix_ms: u64,
        last_seen_unix_ms: u64,
    }
    match fmt {
        OutputFormat::Human => {
            println!(
                "{:<64}  {:>6}  {:>6}  {:>14}  {:>14}",
                "hash", "refct", "pinned", "first_seen", "last_seen"
            );
            for (h, e) in entries {
                println!(
                    "{:<64}  {:>6}  {:>6}  {:>14}  {:>14}",
                    hex32(h),
                    e.refcount,
                    e.pinned,
                    e.first_seen_unix_ms,
                    e.last_seen_unix_ms,
                );
            }
            println!("({} entries)", entries.len());
        }
        OutputFormat::Json => {
            let rows: Vec<LsRow> = entries
                .iter()
                .map(|(h, e)| LsRow {
                    hash: hex32(h),
                    refcount: e.refcount,
                    pinned: e.pinned,
                    first_seen_unix_ms: e.first_seen_unix_ms,
                    last_seen_unix_ms: e.last_seen_unix_ms,
                })
                .collect();
            println!("{}", serde_json::to_string(&rows)?);
        }
    }
    Ok(())
}

// ============================================================================
// Input / parse helpers
// ============================================================================

fn read_input(path: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    if path == "-" {
        let mut buf = Vec::new();
        io::stdin().read_to_end(&mut buf)?;
        Ok(buf)
    } else {
        Ok(fs::read(path)?)
    }
}

fn parse_hash(s: &str) -> Result<[u8; 32], Box<dyn std::error::Error>> {
    if s.len() != 64 {
        return Err(format!("expected a 64-char hex hash; got {} chars", s.len()).into());
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        let pair = s.get(i * 2..i * 2 + 2).ok_or("hash slice out of range")?;
        *b = u8::from_str_radix(pair, 16)
            .map_err(|e| format!("non-hex char in hash at index {}: {}", i * 2, e))?;
    }
    Ok(out)
}

fn parse_duration(s: &str) -> Result<Duration, Box<dyn std::error::Error>> {
    // Hand-parse a suffix grammar: `<n><s|m|h|d>`. Keeps the
    // dep surface tight (no `humantime` crate).
    let (num_part, unit_char) = match s.chars().last() {
        Some(c) if c.is_ascii_alphabetic() => (&s[..s.len() - 1], c),
        _ => return Err(format!("retention must end in s/m/h/d; got `{}`", s).into()),
    };
    let n: u64 = num_part
        .parse()
        .map_err(|e| format!("retention prefix must be a non-negative integer: {}", e))?;
    let multiplier: u64 = match unit_char {
        's' | 'S' => 1,
        'm' | 'M' => 60,
        'h' | 'H' => 3600,
        'd' | 'D' => 86_400,
        _ => return Err(format!("unknown retention unit `{}`", unit_char).into()),
    };
    let secs = n
        .checked_mul(multiplier)
        .ok_or_else(|| format!("retention value `{}` overflows u64 seconds", s))?;
    Ok(Duration::from_secs(secs))
}

fn hex32(hash: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in hash {
        use std::fmt::Write;
        let _ = write!(s, "{:02x}", b);
    }
    s
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
