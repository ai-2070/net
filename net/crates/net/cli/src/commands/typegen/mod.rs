//! `net transfer`-style command family `net-mesh typegen
//! (generate|snapshot|diff)` — generate typed language bindings
//! (TypeScript, Python) from discovered `ToolDescriptor`s
//! (`TYPEGEN_CLI_PLAN.md`).
//!
//! Descriptors come from one of two sources:
//!
//! - **Live discovery** — remote-attach to a mesh node (same flags as the
//!   `net aggregator` / `net transfer` verbs), let the capability fold
//!   populate, then `Mesh::list_tools`. Requires the attach flags.
//! - **Snapshot** — read a [`TypegenSnapshot`] JSON file pinned by an
//!   earlier `typegen snapshot`. Reproducible and offline; the path CI and
//!   golden tests use.
//!
//! Codegen itself is pure: `ToolDescriptor` → source files
//! (`schema.rs` parses the JSON Schema once into a language-neutral IR;
//! `ts.rs` / `python.rs` render it). The CLI plumbs source → codegen →
//! disk.

mod diff;
mod python;
mod schema;
mod ts;

use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::{Args, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};

use crate::commands::aggregator::RemoteAttachArgs;
use crate::context::{resolve_profile, resolve_remote_attach, CliContext};
use crate::error::{generic, invalid_args, CliError};
use crate::prelude::{emit_value, OutputFormat};

use net_sdk::tool::ToolDescriptor;

/// Snapshot format version. Bump on a non-additive shape change.
const SNAPSHOT_FORMAT_VERSION: u32 = 1;

#[derive(Subcommand, Debug)]
pub enum TypegenCommand {
    /// Generate typed bindings for tools matching a query (or from a snapshot).
    Generate(GenerateArgs),
    /// Pin currently discoverable tools into a snapshot file for
    /// reproducible later regeneration.
    Snapshot(SnapshotArgs),
    /// Show the schema-evolution diff between two snapshots.
    Diff(DiffArgs),
}

/// Target language for `generate`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Language {
    /// TypeScript (`.d.ts` interfaces + `.ts` call helpers).
    Ts,
    /// Python (Pydantic v2 models + `.pyi` stubs).
    Python,
}

#[derive(Args, Debug)]
pub struct GenerateArgs {
    /// Language target.
    #[arg(long, value_enum)]
    pub language: Language,

    /// Tag filter — include a tool if ANY of its tags match. Applied to
    /// both live discovery and snapshot sources.
    #[arg(long = "tag", num_args = 1.., value_name = "TAG")]
    pub tags: Vec<String>,

    /// Explicit tool IDs to generate for (exact match). Applied to both
    /// live discovery and snapshot sources.
    #[arg(long = "tool", num_args = 1.., value_name = "TOOL_ID")]
    pub tools: Vec<String>,

    /// Read descriptors from a snapshot file instead of live discovery.
    #[arg(long, value_name = "PATH")]
    pub from_snapshot: Option<PathBuf>,

    /// Output directory. Created if missing.
    #[arg(long, default_value = "./generated")]
    pub out: PathBuf,

    #[arg(long)]
    pub identity: Option<PathBuf>,
    #[arg(long, default_value_t = crate::prelude::DEFAULT_SUPERVISOR_NODE)]
    pub node: u64,
    #[command(flatten)]
    pub attach: RemoteAttachArgs,
}

#[derive(Args, Debug)]
pub struct SnapshotArgs {
    /// Tag filter — include a tool if ANY of its tags match.
    #[arg(long = "tag", num_args = 1.., value_name = "TAG")]
    pub tags: Vec<String>,
    /// Explicit tool IDs to capture (exact match).
    #[arg(long = "tool", num_args = 1.., value_name = "TOOL_ID")]
    pub tools: Vec<String>,
    /// Snapshot file to write.
    #[arg(long, value_name = "PATH")]
    pub out: PathBuf,

    #[arg(long)]
    pub identity: Option<PathBuf>,
    #[arg(long, default_value_t = crate::prelude::DEFAULT_SUPERVISOR_NODE)]
    pub node: u64,
    #[command(flatten)]
    pub attach: RemoteAttachArgs,
}

#[derive(Args, Debug)]
pub struct DiffArgs {
    /// Baseline snapshot.
    #[arg(long, value_name = "PATH")]
    pub from: PathBuf,
    /// Updated snapshot to compare against the baseline.
    #[arg(long, value_name = "PATH")]
    pub to: PathBuf,
}

// ── snapshot format ─────────────────────────────────────────────────

/// Pinned, source-control-friendly capture of a `list_tools` result.
/// Deterministic given the same query against the same mesh state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypegenSnapshot {
    /// Snapshot format version (`SNAPSHOT_FORMAT_VERSION`).
    pub format_version: u32,
    /// RFC 3339 UTC capture time. Audit metadata; ignored at regenerate.
    pub captured_at: String,
    /// The query that produced this snapshot.
    pub source_query: SnapshotQuery,
    /// Descriptors in `list_tools` order.
    pub descriptors: Vec<ToolDescriptor>,
}

/// The filter a snapshot was captured with (for audit / re-execution).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SnapshotQuery {
    /// `--tag` values (empty = no tag filter).
    pub tags: Vec<String>,
    /// `--tool` values (empty = no id filter).
    pub tools: Vec<String>,
}

// ── codegen interface ───────────────────────────────────────────────

/// One file a renderer wants written, relative to the output dir.
pub struct GeneratedFile {
    /// Path relative to `--out` (forward slashes; mapped to the OS sep).
    pub rel_path: String,
    /// File contents.
    pub contents: String,
}

/// Generation-time provenance threaded into file headers + `meta.json`.
pub struct GenMeta {
    /// Human label of the source (`"live discovery"` or the snapshot path).
    pub source_label: String,
    /// RFC 3339 capture time carried from the snapshot, or generation time.
    pub captured_at: String,
    /// Snapshot format version, for the emitted metadata file.
    pub format_version: u32,
}

pub async fn run(
    cmd: TypegenCommand,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    match cmd {
        TypegenCommand::Generate(args) => {
            run_generate(args, output, config_path, profile_name).await
        }
        TypegenCommand::Snapshot(args) => {
            run_snapshot(args, output, config_path, profile_name).await
        }
        TypegenCommand::Diff(args) => run_diff(args, output).await,
    }
}

// ── generate ────────────────────────────────────────────────────────

async fn run_generate(
    args: GenerateArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    let (descriptors, meta) = match &args.from_snapshot {
        Some(path) => load_snapshot_source(path, &args.tags, &args.tools)?,
        None => {
            fetch_live_source(
                &args.tags,
                &args.tools,
                &args.attach,
                args.identity.as_deref(),
                args.node,
                config_path,
                profile_name,
            )
            .await?
        }
    };

    // Tools without an inline input schema can't generate input types
    // (schema exceeded the fold's per-entry budget). Skip with a warning.
    let mut skipped: Vec<String> = Vec::new();
    let usable: Vec<ToolDescriptor> = descriptors
        .into_iter()
        .filter(|d| {
            if d.input_schema.is_none() {
                eprintln!(
                    "warning: tool `{}` has no inline input schema (size > fold budget); \
                     binding skipped. Re-run after `tool.metadata.fetch` ships.",
                    d.tool_id
                );
                skipped.push(d.tool_id.clone());
                false
            } else {
                true
            }
        })
        .collect();

    // Distinct tool ids can sanitize to the same module basename and would
    // silently overwrite each other's files; warn loudly rather than lose one.
    for (base, ids) in basename_collisions(&usable) {
        eprintln!(
            "warning: tools {} all map to module `{base}` and will overwrite each \
             other's output; rename a tool id to disambiguate.",
            ids.iter()
                .map(|id| format!("`{id}`"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let files = match args.language {
        Language::Ts => ts::generate(&usable, &meta, &mut skipped)?,
        Language::Python => python::generate(&usable, &meta, &mut skipped)?,
    };

    let written = write_generated(&args.out, &files).await?;

    let view = GenerateView {
        language: format!("{:?}", args.language).to_lowercase(),
        tool_count: usable.len() as u64,
        files_written: written,
        skipped,
        out: args.out.display().to_string(),
    };
    emit_value(OutputFormat::resolve_oneshot(output), &view)
        .map_err(|e| generic(format!("write typegen generate result: {e}")))?;
    Ok(())
}

// ── snapshot ────────────────────────────────────────────────────────

async fn run_snapshot(
    args: SnapshotArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    let (descriptors, _meta) = fetch_live_source(
        &args.tags,
        &args.tools,
        &args.attach,
        args.identity.as_deref(),
        args.node,
        config_path,
        profile_name,
    )
    .await?;

    let schema_bytes: u64 = descriptors
        .iter()
        .map(|d| {
            d.input_schema.as_ref().map(|s| s.len()).unwrap_or(0) as u64
                + d.output_schema.as_ref().map(|s| s.len()).unwrap_or(0) as u64
        })
        .sum();

    let snapshot = TypegenSnapshot {
        format_version: SNAPSHOT_FORMAT_VERSION,
        captured_at: now_rfc3339(),
        source_query: SnapshotQuery {
            tags: args.tags.clone(),
            tools: args.tools.clone(),
        },
        descriptors,
    };

    // Pretty + stable so snapshots diff cleanly in source control.
    let json = serde_json::to_string_pretty(&snapshot)
        .map_err(|e| generic(format!("serialize snapshot: {e}")))?;
    if let Some(parent) = args.out.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| generic(format!("create snapshot dir {}: {e}", parent.display())))?;
        }
    }
    tokio::fs::write(&args.out, json.as_bytes())
        .await
        .map_err(|e| generic(format!("write snapshot {}: {e}", args.out.display())))?;

    let view = SnapshotView {
        tool_count: snapshot.descriptors.len() as u64,
        schema_bytes,
        out: args.out.display().to_string(),
    };
    emit_value(OutputFormat::resolve_oneshot(output), &view)
        .map_err(|e| generic(format!("write typegen snapshot result: {e}")))?;
    Ok(())
}

// ── diff ────────────────────────────────────────────────────────────

async fn run_diff(args: DiffArgs, output: Option<OutputFormat>) -> Result<(), CliError> {
    let from = read_snapshot(&args.from)?;
    let to = read_snapshot(&args.to)?;
    let report = diff::diff(&from, &to);

    match OutputFormat::resolve_oneshot(output) {
        // Human formats get the operator-facing report; machine formats get
        // the structured tree.
        OutputFormat::Table | OutputFormat::Text => {
            print!("{}", diff::render_text(&report));
        }
        format => {
            emit_value(format, &report).map_err(|e| generic(format!("write typegen diff: {e}")))?;
        }
    }
    Ok(())
}

// ── descriptor sources ──────────────────────────────────────────────

/// Read descriptors from a snapshot file, optionally narrowing by `--tag`
/// / `--tool`. Returns the descriptors + provenance for file headers.
fn load_snapshot_source(
    path: &Path,
    tags: &[String],
    tools: &[String],
) -> Result<(Vec<ToolDescriptor>, GenMeta), CliError> {
    let snapshot = read_snapshot(path)?;
    let meta = GenMeta {
        source_label: format!("snapshot {}", path.display()),
        captured_at: snapshot.captured_at.clone(),
        format_version: snapshot.format_version,
    };
    let descriptors = filter_descriptors(snapshot.descriptors, tags, tools);
    Ok((descriptors, meta))
}

/// Read + validate a snapshot file into a [`TypegenSnapshot`].
fn read_snapshot(path: &Path) -> Result<TypegenSnapshot, CliError> {
    let bytes = std::fs::read(path)
        .map_err(|e| generic(format!("read snapshot {}: {e}", path.display())))?;
    let snapshot: TypegenSnapshot = serde_json::from_slice(&bytes).map_err(|e| {
        invalid_args(format!(
            "snapshot {} is not a valid TypegenSnapshot: {e}",
            path.display()
        ))
    })?;
    if snapshot.format_version != SNAPSHOT_FORMAT_VERSION {
        return Err(invalid_args(format!(
            "snapshot {} has format_version {} (this build understands {})",
            path.display(),
            snapshot.format_version,
            SNAPSHOT_FORMAT_VERSION
        )));
    }
    Ok(snapshot)
}

/// Discover descriptors live: remote-attach, let the fold populate, then
/// `list_tools`, filtered by `--tag` / `--tool`.
async fn fetch_live_source(
    tags: &[String],
    tools: &[String],
    attach: &RemoteAttachArgs,
    identity: Option<&Path>,
    node: u64,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(Vec<ToolDescriptor>, GenMeta), CliError> {
    let profile = resolve_profile(config_path, profile_name).await?;
    let remote = resolve_remote_attach(
        &profile,
        attach.node_addr.as_deref(),
        attach.node_pubkey.as_deref(),
        attach.remote_node_id.as_deref(),
        attach.psk_hex.as_deref(),
    )?
    .ok_or_else(|| {
        invalid_args(
            "net typegen live discovery needs a mesh target: pass --node-addr <IP:PORT> \
             --node-pubkey <HEX> --node-id <N> --psk-hex <HEX> (each defaultable in the \
             profile), or use --from-snapshot for offline generation.",
        )
    })?;
    let ctx = CliContext::build_with_remote(&profile, identity, node, false, remote).await?;
    let mesh = ctx.require_mesh()?;

    // The fold populates asynchronously after attach. Poll until it
    // reports tools or the budget elapses, so a single-shot CLI doesn't
    // race discovery and emit an empty result.
    let descriptors = discover_with_timeout(mesh, Duration::from_secs(5)).await;
    let descriptors = filter_descriptors(descriptors, tags, tools);

    let meta = GenMeta {
        source_label: "live discovery".to_string(),
        captured_at: now_rfc3339(),
        format_version: SNAPSHOT_FORMAT_VERSION,
    };
    Ok((descriptors, meta))
}

/// Poll `list_tools` until it returns at least one descriptor or `budget`
/// elapses (the fold populates asynchronously after attach).
async fn discover_with_timeout(mesh: &net_sdk::Mesh, budget: Duration) -> Vec<ToolDescriptor> {
    let started = std::time::Instant::now();
    loop {
        let tools = mesh.list_tools(None);
        if !tools.is_empty() || started.elapsed() >= budget {
            return tools;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Apply `--tag` (ANY-match over `descriptor.tags`) and `--tool` (exact id)
/// filters. Empty filters are no-ops.
fn filter_descriptors(
    descriptors: Vec<ToolDescriptor>,
    tags: &[String],
    tools: &[String],
) -> Vec<ToolDescriptor> {
    let by_tag: Vec<ToolDescriptor> = if tags.is_empty() {
        descriptors
    } else {
        descriptors
            .into_iter()
            .filter(|d| d.tags.iter().any(|t| tags.contains(t)))
            .collect()
    };
    filter_by_tools(by_tag, tools)
}

/// Groups of tool ids that sanitize to the same [`module_basename`] (only
/// genuine collisions — groups of size ≥ 2 — are returned, sorted by basename
/// for deterministic output).
fn basename_collisions(descriptors: &[ToolDescriptor]) -> Vec<(String, Vec<String>)> {
    let mut groups: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for d in descriptors {
        groups
            .entry(module_basename(&d.tool_id))
            .or_default()
            .push(d.tool_id.clone());
    }
    groups.into_iter().filter(|(_, ids)| ids.len() > 1).collect()
}

fn filter_by_tools(descriptors: Vec<ToolDescriptor>, tools: &[String]) -> Vec<ToolDescriptor> {
    if tools.is_empty() {
        descriptors
    } else {
        descriptors
            .into_iter()
            .filter(|d| tools.contains(&d.tool_id))
            .collect()
    }
}

// ── output writer ───────────────────────────────────────────────────

/// Write every generated file under `out`, creating parent dirs. Returns
/// the file count.
async fn write_generated(out: &Path, files: &[GeneratedFile]) -> Result<u64, CliError> {
    for file in files {
        let dest = out.join(rel_to_os_path(&file.rel_path));
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| generic(format!("create dir {}: {e}", parent.display())))?;
        }
        tokio::fs::write(&dest, file.contents.as_bytes())
            .await
            .map_err(|e| generic(format!("write {}: {e}", dest.display())))?;
    }
    Ok(files.len() as u64)
}

/// Map a forward-slash relative path to an OS path.
fn rel_to_os_path(rel: &str) -> PathBuf {
    rel.split('/').collect()
}

// ── shared naming helpers ───────────────────────────────────────────

/// PascalCase a tool id for a type name: `acme/web_search` → `AcmeWebSearch`.
/// Splits on any non-alphanumeric character; a leading digit is prefixed
/// with `_` so the result is a valid identifier.
pub(crate) fn pascal_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut new_word = true;
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            if new_word {
                out.extend(ch.to_uppercase());
            } else {
                out.push(ch);
            }
            new_word = false;
        } else {
            new_word = true;
        }
    }
    if out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    if out.is_empty() {
        out.push('_');
    }
    out
}

/// Filesystem-safe module base name: every char outside `[A-Za-z0-9_]`
/// becomes `_`. `acme/web_search` → `acme_web_search`.
pub(crate) fn module_basename(tool_id: &str) -> String {
    let mut out: String = tool_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    if out.is_empty() {
        out.push('_');
    }
    out
}

/// RFC 3339 UTC timestamp for "now". Hand-rolled (no time crate in the CLI
/// deps) via the civil-from-days algorithm.
fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// Days since the Unix epoch → `(year, month, day)` (Howard Hinnant's
/// `civil_from_days`).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ── output views ────────────────────────────────────────────────────

#[derive(Serialize)]
struct GenerateView {
    language: String,
    tool_count: u64,
    files_written: u64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    skipped: Vec<String>,
    out: String,
}

#[derive(Serialize)]
struct SnapshotView {
    tool_count: u64,
    schema_bytes: u64,
    out: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pascal_case_handles_separators_and_digits() {
        assert_eq!(pascal_case("acme/web_search"), "AcmeWebSearch");
        assert_eq!(pascal_case("vendor.tool-name"), "VendorToolName");
        assert_eq!(pascal_case("3d_render"), "_3dRender");
        assert_eq!(pascal_case("already"), "Already");
    }

    #[test]
    fn module_basename_sanitizes() {
        assert_eq!(module_basename("acme/web_search"), "acme_web_search");
        assert_eq!(module_basename("a.b/c"), "a_b_c");
        assert_eq!(module_basename("9lives"), "_9lives");
    }

    #[test]
    fn civil_from_days_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(18_993), (2022, 1, 1));
    }

    fn desc(tool_id: &str) -> ToolDescriptor {
        ToolDescriptor {
            tool_id: tool_id.into(),
            name: tool_id.into(),
            version: "1.0.0".into(),
            description: None,
            input_schema: None,
            output_schema: None,
            requires: vec![],
            estimated_time_ms: 0,
            stateless: true,
            streaming: false,
            tags: vec![],
            node_count: 1,
        }
    }

    #[test]
    fn basename_collisions_detects_only_clashing_ids() {
        // `web-search` and `web_search` both sanitize to `web_search`.
        let descriptors = vec![desc("acme/web-search"), desc("acme/web_search"), desc("acme/maps")];
        let collisions = basename_collisions(&descriptors);
        assert_eq!(collisions.len(), 1, "{collisions:?}");
        assert_eq!(collisions[0].0, "acme_web_search");
        assert_eq!(
            collisions[0].1,
            vec!["acme/web-search".to_string(), "acme/web_search".to_string()]
        );
    }

    #[test]
    fn snapshot_round_trips() {
        let snap = TypegenSnapshot {
            format_version: SNAPSHOT_FORMAT_VERSION,
            captured_at: "2026-06-04T10:00:00Z".into(),
            source_query: SnapshotQuery {
                tags: vec!["search".into()],
                tools: vec![],
            },
            descriptors: vec![],
        };
        let json = serde_json::to_string(&snap).expect("ser");
        let back: TypegenSnapshot = serde_json::from_str(&json).expect("de");
        assert_eq!(back.format_version, SNAPSHOT_FORMAT_VERSION);
        assert_eq!(back.source_query.tags, vec!["search".to_string()]);
    }
}
