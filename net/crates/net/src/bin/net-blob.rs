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

use net::adapter::net::dataforts::blob::{
    BlobAdapter, BlobRef, BlobStat, MeshBlobAdapter, RefcountEntry,
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
