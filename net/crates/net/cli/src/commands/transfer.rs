//! `net transfer (recv-blob|send-blob|recv-dir|send-dir|ls|status|cancel)`
//! — operator surface over the `net_sdk::transport` blob / directory
//! movement primitives (`TRANSFER_CLI_PLAN.md`).
//!
//! Verb shapes:
//!
//! - `recv-blob` / `recv-dir` — **remote**. Stand up an ephemeral mesh,
//!   handshake with the holder (the same routed-attach path the
//!   `net aggregator` RPC verbs use), install the blob-transfer engine
//!   locally (required even to *fetch* — `fetch_blob` registers a
//!   pending transfer on the caller's engine), then pull the content
//!   and reconstruct it on disk. `recv-blob` writes a single file via a
//!   temp-and-rename; `recv-dir` delegates to `fetch_dir`, whose atomic
//!   sibling-temp-dir reconstruction (commit 636d31e) leaves the target
//!   untouched on failure.
//! - `send-blob` / `send-dir` — **local**. Compute the content-addressed
//!   [`BlobRef`] a peer fetches by, and (with `--store <dir>`) stage the
//!   bytes into an on-disk store a serving node can serve from. There is
//!   no "push" — this is the publish-and-fetch model (`TRANSFER_CLI_PLAN`
//!   "Out of scope: net transfer push"). Without `--store` the verb only
//!   prints the reference (a dry content-address computation).
//! - `ls` / `status` / `cancel` — **remote**. Query a holder's transfer
//!   engine over the `blob.transfers` RPC (remote-attach), reporting that
//!   node's requester-side in-flight transfers (what it's fetching). A
//!   single-shot CLI owns no engine, so there is nothing local to inspect;
//!   the holder exposes the RPC via `transport::serve_blob_transfer_rpc`.
//!   `cancel` drops the holder's pending entry, failing its awaiting fetch.
//!
//! `recv-*` **and** `ls` / `status` / `cancel` require remote-attach flags
//! — `--node-addr`, `--node-pubkey`, `--node-id`, `--psk-hex` — each
//! defaultable from the profile, exactly like the aggregator RPC verbs.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use clap::{Args, Subcommand};
use serde::Serialize;

use crate::commands::aggregator::RemoteAttachArgs;
use crate::context::{resolve_profile, resolve_remote_attach, CliContext, RemoteAttach};
use crate::error::{generic, invalid_args, sdk, CliError};
use crate::parsers::parse_u64_flexible;
use crate::prelude::{emit_value, OutputFormat};

use net_sdk::dataforts::{BlobAdapter, MeshBlobAdapter, Redex};
use net_sdk::transport::{self, BlobRef, ChunkedPayload, Encoding};

#[derive(Subcommand, Debug)]
pub enum TransferCommand {
    /// Receive a single blob from a peer and write it to `--out`.
    RecvBlob(RecvBlobArgs),
    /// Compute a blob's content reference (and optionally stage it to a
    /// local store); peers fetch by the printed reference.
    SendBlob(SendBlobArgs),
    /// Receive a directory atomically. Reconstruction uses the
    /// temp-and-rename pattern (commit 636d31e); on failure the local
    /// target is left unchanged.
    RecvDir(RecvDirArgs),
    /// Publish a directory: emit its manifest + chunk references (and
    /// optionally stage them to a local store).
    SendDir(SendDirArgs),
    /// List a holder's in-flight (incoming) transfers over the mesh.
    Ls(LsArgs),
    /// Show one of a holder's transfers by stream id.
    Status(StatusArgs),
    /// Cancel one of a holder's in-progress transfers by stream id.
    Cancel(CancelArgs),
}

#[derive(Args, Debug)]
pub struct RecvBlobArgs {
    /// Holder peer id to fetch from (decimal or `0x`-hex). Defaults to
    /// the remote-attach `--node-id` (the node you handshook with).
    #[arg(long, value_parser = crate::parsers::parse_u64_flexible)]
    pub from: Option<u64>,
    /// Content reference: a 32-byte hash (single-chunk blob) or the full
    /// encoded `BlobRef` hex that `send-blob` prints for chunked content.
    #[arg(long = "blob-ref")]
    pub blob_ref: String,
    /// Destination file. Written atomically via `<out>.partial` + rename.
    #[arg(long)]
    pub out: PathBuf,

    #[arg(long)]
    pub identity: Option<PathBuf>,
    #[arg(long, default_value_t = crate::prelude::DEFAULT_SUPERVISOR_NODE)]
    pub node: u64,
    #[command(flatten)]
    pub attach: RemoteAttachArgs,
}

#[derive(Args, Debug)]
pub struct SendBlobArgs {
    /// Source file. Pass `-` to read from stdin.
    pub path: PathBuf,
    /// Stage the bytes into an on-disk store at this directory so a node
    /// rooted there can serve them. Without it, only the reference is
    /// computed and printed (no bytes persisted).
    #[arg(long)]
    pub store: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct RecvDirArgs {
    /// Holder peer id to fetch from (decimal or `0x`-hex). Defaults to
    /// the remote-attach `--node-id`.
    #[arg(long, value_parser = crate::parsers::parse_u64_flexible)]
    pub from: Option<u64>,
    /// Directory manifest reference: a 32-byte hash or the full encoded
    /// `BlobRef` hex that `send-dir` prints.
    #[arg(long = "remote-ref")]
    pub remote_ref: String,
    /// Destination directory. Reconstructed atomically (temp + rename).
    #[arg(long)]
    pub out: PathBuf,
    /// Leaf-file fetch concurrency. `0` uses the SDK default.
    #[arg(long, default_value_t = 0)]
    pub concurrency: usize,

    #[arg(long)]
    pub identity: Option<PathBuf>,
    #[arg(long, default_value_t = crate::prelude::DEFAULT_SUPERVISOR_NODE)]
    pub node: u64,
    #[command(flatten)]
    pub attach: RemoteAttachArgs,
}

#[derive(Args, Debug)]
pub struct SendDirArgs {
    /// Source directory.
    pub path: PathBuf,
    /// Stage the manifest + chunks into an on-disk store at this
    /// directory so a node rooted there can serve them. Without it, only
    /// the manifest reference is computed and printed.
    #[arg(long)]
    pub store: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct LsArgs {
    #[arg(long)]
    pub identity: Option<PathBuf>,
    #[arg(long, default_value_t = crate::prelude::DEFAULT_SUPERVISOR_NODE)]
    pub node: u64,
    #[command(flatten)]
    pub attach: RemoteAttachArgs,
}

#[derive(Args, Debug)]
pub struct StatusArgs {
    /// Transfer id (stream id) to inspect (decimal or `0x`-hex).
    pub transfer_id: String,

    #[arg(long)]
    pub identity: Option<PathBuf>,
    #[arg(long, default_value_t = crate::prelude::DEFAULT_SUPERVISOR_NODE)]
    pub node: u64,
    #[command(flatten)]
    pub attach: RemoteAttachArgs,
}

#[derive(Args, Debug)]
pub struct CancelArgs {
    /// Transfer id (stream id) to cancel (decimal or `0x`-hex).
    pub transfer_id: String,

    #[arg(long)]
    pub identity: Option<PathBuf>,
    #[arg(long, default_value_t = crate::prelude::DEFAULT_SUPERVISOR_NODE)]
    pub node: u64,
    #[command(flatten)]
    pub attach: RemoteAttachArgs,
}

pub async fn run(
    cmd: TransferCommand,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
    quiet: bool,
) -> Result<(), CliError> {
    match cmd {
        TransferCommand::RecvBlob(args) => {
            run_recv_blob(args, output, config_path, profile_name, quiet).await
        }
        TransferCommand::SendBlob(args) => run_send_blob(args, output).await,
        TransferCommand::RecvDir(args) => {
            run_recv_dir(args, output, config_path, profile_name, quiet).await
        }
        TransferCommand::SendDir(args) => run_send_dir(args, output).await,
        TransferCommand::Ls(args) => run_ls(args, output, config_path, profile_name).await,
        TransferCommand::Status(args) => run_status(args, output, config_path, profile_name).await,
        TransferCommand::Cancel(args) => run_cancel(args, output, config_path, profile_name).await,
    }
}

// ── recv-blob ───────────────────────────────────────────────────────

async fn run_recv_blob(
    args: RecvBlobArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
    quiet: bool,
) -> Result<(), CliError> {
    let blob_ref = parse_content_ref(&args.blob_ref, "--blob-ref")?;

    let profile = resolve_profile(config_path, profile_name).await?;
    let remote = require_remote_attach(&profile, &args.attach, "recv-blob")?;
    // `--from` overrides the attach target (fetch via a relay you
    // handshook with); otherwise the holder is the node you connected to.
    // (Parsed to `u64` at argv time, so no fallible re-parse here.)
    let source = args.from.unwrap_or(remote.node_id);
    let ctx =
        CliContext::build_with_remote(&profile, args.identity.as_deref(), args.node, false, remote)
            .await?;
    let mesh = ctx.require_mesh()?;

    // Installing the engine is required to fetch, not just to serve:
    // `fetch_blob` registers a pending transfer on the local engine.
    transport::serve_blob_transfer(
        mesh,
        Arc::new(MeshBlobAdapter::new("recv", Arc::new(Redex::new()))),
    );

    let spinner = Progress::start(
        &format!("fetching blob from peer {source}"),
        progress_enabled(output, quiet),
    );
    let started = Instant::now();
    let bytes = transport::fetch_blob(mesh, source, &blob_ref)
        .await
        .map_err(|e| sdk(format!("fetch_blob from peer {source} failed: {e}")))?;
    let elapsed = started.elapsed();
    spinner.finish();

    write_atomic(&args.out, &bytes).await?;

    let view = RecvBlobView {
        peer: source,
        out: args.out.display().to_string(),
        bytes: bytes.len() as u64,
        duration_secs: elapsed.as_secs_f64(),
        throughput_mib_s: throughput_mib_s(bytes.len() as u64, elapsed),
    };
    emit_value(OutputFormat::resolve_oneshot(output), &view)
        .map_err(|e| generic(format!("write recv-blob result: {e}")))?;
    Ok(())
}

// ── send-blob ───────────────────────────────────────────────────────

async fn run_send_blob(args: SendBlobArgs, output: Option<OutputFormat>) -> Result<(), CliError> {
    let bytes = read_source(&args.path).await?;

    // Compute the content-addressed reference: `chunk_payload` decides
    // inline (single-chunk) vs chunked, then `into_blob_ref` finalizes a
    // Small / Manifest `BlobRef` — the exact value `recv-blob` fetches by.
    let chunked =
        transport::chunk_payload(&bytes).map_err(|e| sdk(format!("chunk payload: {e}")))?;
    let (small_hash, chunk_count) = match &chunked {
        ChunkedPayload::Inline { hash, .. } => (Some(hex::encode(hash)), 1usize),
        ChunkedPayload::Chunked { chunks, .. } => (None, chunks.len()),
    };
    let blob_ref = chunked
        .into_blob_ref("mesh://transfer", Encoding::Replicated)
        .map_err(|e| sdk(format!("build blob ref: {e}")))?;

    let staged = match args.store.as_deref() {
        Some(dir) => {
            let adapter = persistent_adapter(dir, "send-blob").await?;
            adapter
                .store(&blob_ref, &bytes)
                .await
                .map_err(|e| sdk(format!("stage blob into {}: {e}", dir.display())))?;
            Some(dir.display().to_string())
        }
        None => None,
    };

    let view = SendBlobView {
        // Full-fidelity reference: works for single- and multi-chunk
        // content. `recv-blob --blob-ref <this>` decodes it back.
        blob_ref: hex::encode(blob_ref.encode()),
        // Convenience: the bare chunk hash, present only for a
        // single-chunk blob (so the short form is unambiguous).
        hash: small_hash,
        size: bytes.len() as u64,
        chunks: chunk_count as u64,
        staged_to: staged,
    };
    emit_value(OutputFormat::resolve_oneshot(output), &view)
        .map_err(|e| generic(format!("write send-blob result: {e}")))?;
    Ok(())
}

// ── recv-dir ────────────────────────────────────────────────────────

async fn run_recv_dir(
    args: RecvDirArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
    quiet: bool,
) -> Result<(), CliError> {
    let manifest_ref = parse_content_ref(&args.remote_ref, "--remote-ref")?;

    let profile = resolve_profile(config_path, profile_name).await?;
    let remote = require_remote_attach(&profile, &args.attach, "recv-dir")?;
    // `--from` is parsed to `u64` at argv time; default to the attach target.
    let source = args.from.unwrap_or(remote.node_id);
    let ctx =
        CliContext::build_with_remote(&profile, args.identity.as_deref(), args.node, false, remote)
            .await?;
    let mesh = ctx.require_mesh()?;
    transport::serve_blob_transfer(
        mesh,
        Arc::new(MeshBlobAdapter::new("recv", Arc::new(Redex::new()))),
    );

    let spinner = Progress::start(
        &format!("reconstructing directory from peer {source}"),
        progress_enabled(output, quiet),
    );
    let started = Instant::now();
    // `fetch_dir` handles the atomic sibling-temp-dir reconstruction and
    // rolls back on failure, so the target is unchanged unless this
    // returns Ok. It also maps `concurrency == 0` to its own
    // `DEFAULT_FETCH_CONCURRENCY`, so we pass the operator's value through
    // verbatim rather than pre-resolving it here.
    let stats = transport::fetch_dir(mesh, source, &manifest_ref, &args.out, args.concurrency)
        .await
        .map_err(|e| sdk(format!("fetch_dir from peer {source} failed: {e}")))?;
    let elapsed = started.elapsed();
    spinner.finish();

    let view = RecvDirView {
        peer: source,
        out: args.out.display().to_string(),
        files: stats.files as u64,
        dirs: stats.dirs as u64,
        symlinks: stats.symlinks as u64,
        bytes: stats.bytes,
        duration_secs: elapsed.as_secs_f64(),
        throughput_mib_s: throughput_mib_s(stats.bytes, elapsed),
        atomic: true,
    };
    emit_value(OutputFormat::resolve_oneshot(output), &view)
        .map_err(|e| generic(format!("write recv-dir result: {e}")))?;
    Ok(())
}

// ── send-dir ────────────────────────────────────────────────────────

async fn run_send_dir(args: SendDirArgs, output: Option<OutputFormat>) -> Result<(), CliError> {
    if !args.path.is_dir() {
        return Err(invalid_args(format!(
            "send-dir source `{}` is not a directory",
            args.path.display()
        )));
    }

    // `store_dir` walks the tree, content-addresses every leaf, builds
    // the manifest, and stores everything into the adapter — returning
    // the manifest `BlobRef` a peer fetches by. We need a real adapter
    // for it to store into; persist it iff `--store` was given, else use
    // an ephemeral in-memory one (manifest computed, bytes not retained).
    let (adapter, staged) = match args.store.as_deref() {
        Some(dir) => (
            persistent_adapter(dir, "send-dir").await?,
            Some(dir.display().to_string()),
        ),
        None => (
            Arc::new(MeshBlobAdapter::new("send-dir", Arc::new(Redex::new()))),
            None,
        ),
    };

    let manifest_ref = transport::store_dir(&adapter, &args.path)
        .await
        .map_err(|e| sdk(format!("store_dir `{}`: {e}", args.path.display())))?;

    let view = SendDirView {
        remote_ref: hex::encode(manifest_ref.encode()),
        manifest_size: manifest_ref.size(),
        staged_to: staged,
    };
    emit_value(OutputFormat::resolve_oneshot(output), &view)
        .map_err(|e| generic(format!("write send-dir result: {e}")))?;
    Ok(())
}

// ── ls / status / cancel ────────────────────────────────────────────

/// `ls` / `status` / `cancel` all query the holder's `blob.transfers`
/// engine over the mesh (remote-attach), reporting that node's in-flight
/// **requester-side** transfers (what it is currently fetching). Build the
/// connected client once per verb.
async fn transfer_client(
    attach: &RemoteAttachArgs,
    identity: Option<&std::path::Path>,
    node: u64,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
    verb: &str,
) -> Result<(CliContext, u64), CliError> {
    let profile = resolve_profile(config_path, profile_name).await?;
    let remote = require_remote_attach(&profile, attach, verb)?;
    let target = remote.node_id;
    let ctx = CliContext::build_with_remote(&profile, identity, node, false, remote).await?;
    Ok((ctx, target))
}

async fn run_ls(
    args: LsArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    let (ctx, target) = transfer_client(
        &args.attach,
        args.identity.as_deref(),
        args.node,
        config_path,
        profile_name,
        "ls",
    )
    .await?;
    let client = transport::BlobTransferClient::new(ctx.require_mesh_node()?);
    let transfers = client
        .list(target)
        .await
        .map_err(|e| sdk(format!("blob.transfers list on peer {target} failed: {e}")))?;
    let view = LsView {
        transfer_count: transfers.len() as u64,
        transfers: transfers.iter().map(TransferRow::from).collect(),
    };
    emit_value(OutputFormat::resolve_oneshot(output), &view)
        .map_err(|e| generic(format!("write transfer ls: {e}")))?;
    Ok(())
}

async fn run_status(
    args: StatusArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    let stream_id = parse_u64_flexible(&args.transfer_id)
        .map_err(|e| invalid_args(format!("transfer-id `{}`: {e}", args.transfer_id)))?;
    let (ctx, target) = transfer_client(
        &args.attach,
        args.identity.as_deref(),
        args.node,
        config_path,
        profile_name,
        "status",
    )
    .await?;
    let client = transport::BlobTransferClient::new(ctx.require_mesh_node()?);
    let found = client.get(target, stream_id).await.map_err(|e| {
        sdk(format!(
            "blob.transfers status on peer {target} failed: {e}"
        ))
    })?;
    let view = StatusView {
        transfer_id: stream_id,
        found: found.is_some(),
        transfer: found.as_ref().map(TransferRow::from),
    };
    emit_value(OutputFormat::resolve_oneshot(output), &view)
        .map_err(|e| generic(format!("write transfer status: {e}")))?;
    Ok(())
}

async fn run_cancel(
    args: CancelArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    let stream_id = parse_u64_flexible(&args.transfer_id)
        .map_err(|e| invalid_args(format!("transfer-id `{}`: {e}", args.transfer_id)))?;
    let (ctx, target) = transfer_client(
        &args.attach,
        args.identity.as_deref(),
        args.node,
        config_path,
        profile_name,
        "cancel",
    )
    .await?;
    let client = transport::BlobTransferClient::new(ctx.require_mesh_node()?);
    let cancelled = client.cancel(target, stream_id).await.map_err(|e| {
        sdk(format!(
            "blob.transfers cancel on peer {target} failed: {e}"
        ))
    })?;
    let view = CancelView {
        transfer_id: stream_id,
        cancelled,
    };
    emit_value(OutputFormat::resolve_oneshot(output), &view)
        .map_err(|e| generic(format!("write transfer cancel: {e}")))?;
    Ok(())
}

// ── helpers ─────────────────────────────────────────────────────────

/// Parse a content reference: a 32-byte hex hash (a single-chunk
/// `Small` blob) or the full encoded `BlobRef` hex that `send-*` prints
/// for chunked content / directory manifests.
fn parse_content_ref(s: &str, flag: &str) -> Result<BlobRef, CliError> {
    let trimmed = s.trim().trim_start_matches("0x").trim_start_matches("0X");
    let bytes = hex::decode(trimmed)
        .map_err(|e| invalid_args(format!("{flag} `{s}` is not valid hex: {e}")))?;
    if bytes.len() == 32 {
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&bytes);
        // The Small fetch path keys on the hash and ignores the size, so
        // 0 is a safe placeholder for the size we don't have on the wire.
        return Ok(BlobRef::small(format!("mesh://{trimmed}"), hash, 0));
    }
    match BlobRef::decode(&bytes) {
        Ok(Some(r)) => Ok(r),
        Ok(None) => Err(invalid_args(format!(
            "{flag} `{s}` decoded to an empty BlobRef"
        ))),
        Err(e) => Err(invalid_args(format!(
            "{flag} `{s}` is neither a 32-byte hash nor a valid encoded BlobRef: {e}"
        ))),
    }
}

/// Resolve remote-attach for a recv verb that requires a holder target.
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
            "net transfer {verb} needs a holder target: pass --node-addr <IP:PORT> \
             --node-pubkey <HEX> --node-id <N> --psk-hex <HEX> (each can be defaulted \
             in the profile as `node_addr` / `node_pubkey` / `node_id` / `psk_hex`)."
        ))
    })
}

/// Build a persistent on-disk blob adapter rooted at `dir`. The bytes
/// staged here are durable so a node configured against the same
/// directory can serve them.
async fn persistent_adapter(
    dir: &std::path::Path,
    id: &str,
) -> Result<Arc<MeshBlobAdapter>, CliError> {
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| generic(format!("create store dir {}: {e}", dir.display())))?;
    let redex = Arc::new(Redex::new().with_persistent_dir(dir));
    Ok(Arc::new(
        MeshBlobAdapter::new(id, redex).with_persistent(true),
    ))
}

/// Read the send source: a file path, or stdin when the path is `-`.
async fn read_source(path: &std::path::Path) -> Result<Vec<u8>, CliError> {
    if path.as_os_str() == "-" {
        use tokio::io::AsyncReadExt as _;
        let mut buf = Vec::new();
        tokio::io::stdin()
            .read_to_end(&mut buf)
            .await
            .map_err(|e| generic(format!("read stdin: {e}")))?;
        Ok(buf)
    } else {
        tokio::fs::read(path)
            .await
            .map_err(|e| generic(format!("read {}: {e}", path.display())))
    }
}

/// Write `bytes` to `out` atomically: write `<out>.partial` then rename
/// over `out`. On failure the partial file is left for inspection (not
/// auto-cleaned) — matching `fetch_dir`'s temp-and-rename semantics.
async fn write_atomic(out: &std::path::Path, bytes: &[u8]) -> Result<(), CliError> {
    let partial = partial_path(out);
    if let Some(parent) = out.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| generic(format!("create out dir {}: {e}", parent.display())))?;
        }
    }
    tokio::fs::write(&partial, bytes)
        .await
        .map_err(|e| generic(format!("write {}: {e}", partial.display())))?;
    tokio::fs::rename(&partial, out).await.map_err(|e| {
        generic(format!(
            "rename {} -> {}: {e} (partial left in place)",
            partial.display(),
            out.display()
        ))
    })?;
    Ok(())
}

/// `<out>.partial` sibling path for the atomic write.
fn partial_path(out: &std::path::Path) -> PathBuf {
    let mut name = out.file_name().unwrap_or_default().to_os_string();
    name.push(".partial");
    out.with_file_name(name)
}

/// MiB/s over the elapsed duration; `0.0` for a zero-length interval.
fn throughput_mib_s(bytes: u64, elapsed: std::time::Duration) -> f64 {
    let secs = elapsed.as_secs_f64();
    if secs <= 0.0 {
        0.0
    } else {
        (bytes as f64 / (1024.0 * 1024.0)) / secs
    }
}

/// Whether the recv-progress spinner should be drawn: only for a *human*
/// effective output format (`table` / `text`) and not under `--quiet`.
/// `--output json` (or any machine format) and `--quiet` both suppress it
/// so an operator asking for machine-readable or silent operation gets no
/// stderr chatter. The stderr-TTY check is applied separately in
/// [`Progress::start`] (pipes/tests get nothing regardless).
fn progress_enabled(output: Option<OutputFormat>, quiet: bool) -> bool {
    !quiet
        && matches!(
            OutputFormat::resolve_oneshot(output),
            OutputFormat::Table | OutputFormat::Text
        )
}

/// Thin wrapper over an indicatif spinner that no-ops unless `enabled`
/// (see [`progress_enabled`]) AND stderr is a TTY (tests, pipes get
/// nothing) — so the diagnostic never lands on stdout or clutters
/// machine-readable / quiet / non-interactive output.
struct Progress(Option<indicatif::ProgressBar>);

impl Progress {
    fn start(msg: &str, enabled: bool) -> Self {
        use std::io::IsTerminal as _;
        if !enabled || !std::io::stderr().is_terminal() {
            return Self(None);
        }
        let pb = indicatif::ProgressBar::new_spinner();
        pb.set_draw_target(indicatif::ProgressDrawTarget::stderr());
        pb.enable_steady_tick(std::time::Duration::from_millis(120));
        pb.set_message(msg.to_string());
        Self(Some(pb))
    }

    fn finish(self) {
        if let Some(pb) = self.0 {
            pb.finish_and_clear();
        }
    }
}

// ── output views ────────────────────────────────────────────────────

#[derive(Serialize)]
struct RecvBlobView {
    peer: u64,
    out: String,
    bytes: u64,
    duration_secs: f64,
    throughput_mib_s: f64,
}

#[derive(Serialize)]
struct SendBlobView {
    blob_ref: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    hash: Option<String>,
    size: u64,
    chunks: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    staged_to: Option<String>,
}

#[derive(Serialize)]
struct RecvDirView {
    peer: u64,
    out: String,
    files: u64,
    dirs: u64,
    symlinks: u64,
    bytes: u64,
    duration_secs: f64,
    throughput_mib_s: f64,
    /// Always true on success — `fetch_dir` only returns Ok after the
    /// atomic rename committed.
    atomic: bool,
}

#[derive(Serialize)]
struct SendDirView {
    remote_ref: String,
    manifest_size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    staged_to: Option<String>,
}

#[derive(Serialize)]
struct LsView {
    transfer_count: u64,
    transfers: Vec<TransferRow>,
}

/// One in-flight (requester-side) transfer, rendered from a
/// [`transport::TransferStatus`]. `kind` is always `recv` — the engine
/// tracks fetches, not serving tasks.
#[derive(Serialize)]
struct TransferRow {
    /// Transfer stream id (the cancel handle).
    transfer_id: u64,
    /// Peer the bytes are being fetched from.
    peer: u64,
    /// Lowercase-hex BLAKE3 content address being fetched.
    hash: String,
    bytes_received: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    total_bytes: Option<u64>,
}

impl From<&transport::TransferStatus> for TransferRow {
    fn from(s: &transport::TransferStatus) -> Self {
        Self {
            transfer_id: s.stream_id,
            peer: s.holder,
            hash: hex::encode(s.expected_hash),
            bytes_received: s.bytes_received,
            total_bytes: s.total_bytes,
        }
    }
}

#[derive(Serialize)]
struct StatusView {
    transfer_id: u64,
    found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    transfer: Option<TransferRow>,
}

#[derive(Serialize)]
struct CancelView {
    transfer_id: u64,
    cancelled: bool,
}
