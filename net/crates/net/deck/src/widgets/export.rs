//! Tab exporters — write the current filtered view of the
//! LOGS / AUDIT / FAILURES tail to a plain-text file in the
//! deck's CWD. Operators use the export during incidents to
//! attach a captured view to a write-up without leaving the
//! TUI; the format mirrors the in-deck rendering so the file
//! reads the same as the screen.
//!
//! Filenames carry a wall-clock timestamp:
//! `deck-<tab>-<unix-ms>.txt`. Same-second exports collide
//! intentionally rare (millisecond granularity); the
//! collision yields an overwrite which is fine — operators
//! who want history version-control the directory.

use net_sdk::dataforts::BlobInventoryEntry;
use net_sdk::deck::{
    AdminAuditRecord, AdminEvent, FailureRecord, LogLevel, LogRecord, VerificationOutcome,
};

/// File extension on every export.
const EXTENSION: &str = ".txt";

/// Outcome reported back to the caller. `path` is the
/// resolved file path; `count` is how many records were
/// written.
pub struct ExportResult {
    pub path: String,
    pub count: usize,
}

/// Errors surfaced to the operator as a toast message. The
/// concrete `std::io::Error` is folded into a human string
/// at the boundary — the TUI doesn't have a place to surface
/// structured error kinds.
pub type ExportError = String;

/// Write a slice of LOG records as plain text. Format:
/// `MM:SS.mmm LEVEL  source  message` per line, matching
/// the in-deck row layout (minus the styling).
pub fn write_logs(records: &[LogRecord]) -> Result<ExportResult, ExportError> {
    let (path, mut f) = open_unique("logs")?;
    let mut count = 0;
    for rec in records {
        let level = match rec.level {
            LogLevel::Debug => "DEBUG",
            LogLevel::Info => "INFO ",
            LogLevel::Warn => "WARN ",
            LogLevel::Error => "ERROR",
            _ => "?    ",
        };
        let source = format_log_source(rec);
        writeln!(
            f,
            "{ts}  {level}  {source}  {msg}",
            ts = format_ts_ms(rec.ts_ms),
            msg = rec.message,
        )
        .map_err(|e| format!("write: {e}"))?;
        count += 1;
    }
    use std::io::Write;
    f.flush().map_err(|e| format!("flush: {e}"))?;
    Ok(ExportResult { path, count })
}

/// Write a slice of AUDIT records (newest-first preserved
/// from the caller's projection) as plain text.
pub fn write_audit(records: &[AdminAuditRecord]) -> Result<ExportResult, ExportError> {
    let (path, mut f) = open_unique("audit")?;
    let mut count = 0;
    for rec in records.iter().rev() {
        let outcome = match rec.outcome {
            VerificationOutcome::Accepted => "Accepted",
            VerificationOutcome::Unverified => "Unverified",
            VerificationOutcome::Rejected { .. } => "Rejected",
            _ => "?",
        };
        let op = if rec.operator_ids.is_empty() {
            "—".to_string()
        } else {
            rec.operator_ids
                .iter()
                .map(|id| format!("0x{id:x}"))
                .collect::<Vec<_>>()
                .join(",")
        };
        let (cmd, target) = format_admin_event(&rec.event);
        writeln!(
            f,
            "seq={seq:>5}  ts_ms={ts}  {outcome}  op={op}  cmd={cmd}  target={target}",
            seq = rec.seq,
            ts = rec.committed_at_ms,
        )
        .map_err(|e| format!("write: {e}"))?;
        count += 1;
    }
    use std::io::Write;
    f.flush().map_err(|e| format!("flush: {e}"))?;
    Ok(ExportResult { path, count })
}

/// Write a slice of BLOBS inventory entries as plain text.
/// Ordering preserved from the caller's projection.
pub fn write_blobs(entries: &[BlobInventoryEntry]) -> Result<ExportResult, ExportError> {
    let (path, mut f) = open_unique("blobs")?;
    let mut count = 0;
    for e in entries {
        writeln!(
            f,
            "hash={hash}  ref={ref_}  pin={pin}  first_seen_ms={first}  last_seen_ms={last}",
            hash = e.hash_hex,
            ref_ = e.refcount,
            pin = if e.pinned { "1" } else { "0" },
            first = e.first_seen_unix_ms,
            last = e.last_seen_unix_ms,
        )
        .map_err(|e| format!("write: {e}"))?;
        count += 1;
    }
    use std::io::Write;
    f.flush().map_err(|e| format!("flush: {e}"))?;
    Ok(ExportResult { path, count })
}

/// Write a slice of FAILURE records as plain text. Newest-
/// first ordering matches the in-deck projection.
pub fn write_failures(records: &[FailureRecord]) -> Result<ExportResult, ExportError> {
    let (path, mut f) = open_unique("failures")?;
    let mut count = 0;
    for rec in records.iter().rev() {
        writeln!(
            f,
            "seq={seq:>5}  ts_ms={ts}  source={src}  reason={reason}",
            seq = rec.seq,
            ts = rec.recorded_at_ms,
            src = rec.source,
            reason = rec.reason,
        )
        .map_err(|e| format!("write: {e}"))?;
        count += 1;
    }
    use std::io::Write;
    f.flush().map_err(|e| format!("flush: {e}"))?;
    Ok(ExportResult { path, count })
}

/// Open a new export file with a timestamp in its name,
/// retrying with a `-N` suffix if (extremely rarely) one
/// already exists at the chosen path. `create_new(true)`
/// means we never silently overwrite — the file the modal
/// reports is always the one we just wrote.
///
/// Filename shape:
/// `deck-<tab>-2026-05-16T18-32-45Z.txt`
///
/// ISO 8601 UTC with `:` substituted by `-` for cross-platform
/// filename safety (Windows / NTFS reject `:`). To parse:
///
/// - Rust: replace the time-portion `-`s with `:` then
///   `chrono::DateTime::parse_from_rfc3339`.
/// - JS: same un-mangle then `Date.parse`.
fn open_unique(tab: &str) -> Result<(String, std::io::BufWriter<std::fs::File>), ExportError> {
    let now = chrono::Utc::now();
    // Filename-safe ISO 8601 — dashes everywhere; the
    // un-mangle to RFC3339 is `s/T(\d{2})-(\d{2})-/T$1:$2:/`.
    let stamp = now.format("%Y-%m-%dT%H-%M-%SZ").to_string();
    // Resolve against the process's startup cwd so the
    // recorded path is deterministic for the operator to find
    // afterwards. `current_dir` failures fall back to the bare
    // relative path (writeln will still land it in whatever
    // the OS thinks is cwd).
    let cwd = std::env::current_dir().ok();
    let base = format!("deck-{tab}-{stamp}");
    // Same-second collision retry: append `-1`, `-2`, …
    // until `create_new` succeeds. Caps at 100 attempts to
    // refuse to busy-loop if the directory is full / write-
    // protected.
    for attempt in 0..100 {
        let filename = if attempt == 0 {
            format!("{base}{EXTENSION}")
        } else {
            format!("{base}-{attempt}{EXTENSION}")
        };
        let full_path = match cwd.as_ref() {
            Some(d) => d.join(&filename),
            None => std::path::PathBuf::from(&filename),
        };
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&full_path)
        {
            // BufWriter so every writeln is amortised against
            // an 8 KiB buffer; long log windows used to issue
            // one syscall per line.
            Ok(f) => return Ok((full_path.display().to_string(), std::io::BufWriter::new(f))),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("create {}: {e}", full_path.display())),
        }
    }
    Err(format!("create deck-{tab}-{stamp}: too many collisions"))
}

fn format_ts_ms(ts_ms: u64) -> String {
    // Mirror the in-deck render format (HH:MM:SS.mmm) so an
    // exported log window's timestamps line up with what the
    // operator saw on the LOGS tab.
    let total_sec = ts_ms / 1_000;
    let hh = total_sec / 3_600;
    let mm = (total_sec / 60) % 60;
    let ss = total_sec % 60;
    let ms = ts_ms % 1_000;
    format!("{hh:02}:{mm:02}:{ss:02}.{ms:03}")
}

fn format_log_source(rec: &LogRecord) -> String {
    match (rec.node_id, rec.daemon_id) {
        (Some(n), Some(d)) => format!("0x{n:x}/0x{d:x}"),
        (Some(n), None) => format!("0x{n:x}"),
        (None, Some(d)) => format!("daemon.0x{d:x}"),
        (None, None) => "—".to_string(),
    }
}

/// Reproduce the deck's command + target labels in text form
/// so the export reads consistently with the on-screen rows.
fn format_admin_event(event: &AdminEvent) -> (&'static str, String) {
    use AdminEvent::*;
    match event {
        EnterMaintenance { node, .. } => ("enter_maintenance", format!("0x{node:x}")),
        ExitMaintenance { node } => ("exit_maintenance", format!("0x{node:x}")),
        Drain { node, .. } => ("drain", format!("0x{node:x}")),
        Cordon { node } => ("cordon", format!("0x{node:x}")),
        Uncordon { node } => ("uncordon", format!("0x{node:x}")),
        RestartAllDaemons { node } => ("restart_all_daemons", format!("0x{node:x}")),
        ClearAvoidList { node } => ("clear_avoid_list", format!("0x{node:x}")),
        InvalidatePlacement { node } => ("invalidate_placement", format!("0x{node:x}")),
        DropReplicas { node, chains } => (
            "drop_replicas",
            format!("0x{node:x} chains={}", chains.len()),
        ),
        FreezeCluster { ttl } => ("freeze_cluster", format!("ttl={}s", ttl.as_secs())),
        ThawCluster => ("thaw_cluster", "cluster".to_string()),
        FlushAvoidLists { .. } => ("flush_avoid_lists", "avoid-lists".to_string()),
        ForceEvictReplica { chain, victim } => (
            "force_evict_replica",
            format!("chain=0x{chain:x} victim=0x{victim:x}"),
        ),
        ForceRestartDaemon { daemon } => {
            ("force_restart_daemon", format!("daemon=0x{:x}", daemon.id))
        }
        ForceCutover { chain, target } => (
            "force_cutover",
            format!("chain=0x{chain:x} target=0x{target:x}"),
        ),
        KillMigration { migration } => ("kill_migration", format!("migration=0x{migration:x}")),
        _ => ("unknown", "—".to_string()),
    }
}
