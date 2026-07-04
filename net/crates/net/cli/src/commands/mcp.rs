//! `net mcp serve` — expose the mesh's capabilities to a local MCP host as a
//! stdio MCP server (`MCP_BRIDGE_PLAN.md` Phase 2, demand side).
//!
//! The mirror of `net wrap`: where `wrap` publishes a local MCP server's tools
//! onto the mesh, `serve` runs a stdio MCP **server** that a host (Claude
//! Code, Cursor, …) spawns and speaks JSON-RPC to. It answers with the `net_*`
//! meta-tools that search / describe / invoke capabilities discovered across
//! the mesh, backed by [`net_mcp::serve::MeshGateway`] over a joined node.
//!
//! **stdout is the MCP transport.** The shim owns stdout for JSON-RPC, so this
//! command does *not* emit through the CLI `--output` pipeline — that would
//! corrupt the protocol stream. All operator diagnostics go to stderr (the
//! tracing writer); the meta-tool responses carry the plan's product failure
//! strings back to the host in-band.
//!
//! Owner-scoped wrapped tools admit callers by `origin_hash`, which is derived
//! from the operator *identity*, not the node — so running `net mcp serve`
//! under the same identity as `net wrap` makes those tools invocable with no
//! explicit allow. A different identity would need the wrap side to `--allow`
//! this shim's origin.

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use clap::{Args, Subcommand};
use net_mcp::serve::{
    CapabilityId, ConsentPolicy, MeshGateway, PinState, PinStore, Shim, MSG_NO_DAEMON,
};
use net_sdk::identity::Identity;
use net_sdk::{Mesh, MeshBuilder};
use serde::Serialize;
use tokio::io::BufReader;

use crate::commands::aggregator::RemoteAttachArgs;
use crate::context::{require_remote_attach, resolve_profile, RemoteAttach};
use crate::error::{connection_failure, generic, invalid_args, CliError};
use crate::prelude::{emit_value, OutputFormat};

#[derive(Subcommand, Debug)]
pub enum McpCommand {
    /// Run a stdio MCP server exposing the mesh's capabilities as meta-tools.
    Serve(ServeArgs),
    /// Approve / reject / list capability pins (local client consent).
    #[command(subcommand)]
    Pin(PinCommand),
}

#[derive(Subcommand, Debug)]
pub enum PinCommand {
    /// Approve a pin so the shim may invoke the capability (out-of-band
    /// consent — this is the step the model cannot perform for itself).
    Approve(PinIdArgs),
    /// Reject / remove a pin.
    Reject(PinIdArgs),
    /// List pins and their state (pending / approved).
    List(PinListArgs),
}

#[derive(Args, Debug)]
pub struct PinIdArgs {
    /// The capability id, as `provider/capability` (from a search result).
    pub cap_id: String,

    /// Pin-store file. Defaults to the per-user store `net mcp serve` reads.
    #[arg(long = "pin-store", value_name = "PATH")]
    pub pin_store: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct PinListArgs {
    /// Pin-store file. Defaults to the per-user store `net mcp serve` reads.
    #[arg(long = "pin-store", value_name = "PATH")]
    pub pin_store: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct ServeArgs {
    /// Operator identity file (`seed_hex = "..."`). Owner-scoped wrapped tools
    /// admit callers by origin, so run this under the same identity as your
    /// `net wrap` side to invoke them without an explicit allow.
    #[arg(long)]
    pub identity: Option<PathBuf>,

    /// Pre-approve a credentialed / external / unknown capability for
    /// invocation (repeatable), as `provider/capability`. Without this a
    /// spicy capability is search/describe-only until pinned.
    #[arg(long = "allow-capability", value_name = "PROVIDER/CAP")]
    pub allow_capability: Vec<String>,

    /// Pin-store file. Defaults to the per-user store the `net mcp pin` verbs
    /// write; an approved pin there admits its capability.
    #[arg(long = "pin-store", value_name = "PATH")]
    pub pin_store: Option<PathBuf>,

    /// The mesh peer to join.
    #[command(flatten)]
    pub remote: RemoteAttachArgs,
}

pub async fn run(
    cmd: McpCommand,
    output: Option<OutputFormat>,
    config_path: Option<&Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    match cmd {
        // `serve` ignores `output` — stdout is the MCP JSON-RPC transport (see
        // the module docs); emitting through the output pipeline would corrupt
        // it. The `pin` verbs are ordinary one-shot commands and do use it.
        McpCommand::Serve(args) => run_serve(args, config_path, profile_name).await,
        McpCommand::Pin(cmd) => run_pin(cmd, output).await,
    }
}

async fn run_serve(
    args: ServeArgs,
    config_path: Option<&Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    let profile = resolve_profile(config_path, profile_name).await?;

    // A mesh peer to join — the running node this shim reads capabilities from
    // and routes invocations through. Without one there is nothing to serve.
    let remote = require_remote_attach(&profile, &args.remote, || generic(MSG_NO_DAEMON))?;

    // Operator identity — the shim's origin (and thus which owner-scoped tools
    // admit it) derives from it.
    let identity_path = args
        .identity
        .as_deref()
        .or(profile.identity.as_deref())
        .ok_or_else(|| {
            invalid_args(
                "net mcp serve needs an operator identity: pass --identity <PATH> or set \
                 `identity = \"...\"` in your profile. Wrapped tools admit callers by origin, \
                 so use the same identity as your `net wrap` side (or have it `--allow` this \
                 shim's origin).",
            )
        })?;
    let identity = load_identity(identity_path).await?;

    let mesh = build_shim_mesh(identity, &remote).await?;
    let mesh = Arc::new(mesh);

    // Seed the shim consent allowlist from `--allow-capability`.
    let mut consent = ConsentPolicy::new();
    for raw in &args.allow_capability {
        let id = CapabilityId::parse(raw)
            .map_err(|e| invalid_args(format!("--allow-capability {raw:?}: {e}")))?;
        consent.allow(id);
    }

    let gateway = MeshGateway::new(Arc::clone(&mesh));
    let shim = Shim::new(gateway)
        .with_consent(consent)
        .with_pin_store(resolve_pin_store(args.pin_store.as_deref())?);

    // Serve until the host closes stdin (EOF) or the operator hits Ctrl-C.
    let reader = BufReader::new(tokio::io::stdin());
    let writer = tokio::io::stdout();
    tokio::select! {
        r = shim.serve(reader, writer) => {
            r.map_err(|e| generic(format!("mcp serve loop: {e}")))?;
        }
        _ = tokio::signal::ctrl_c() => {}
    }

    // Reclaim the mesh — the shim + gateway held the other `Arc` and are now
    // dropped — and shut it down. If a stray reference lingers, fall back to a
    // best-effort drop.
    if let Ok(mesh) = Arc::try_unwrap(mesh) {
        mesh.shutdown().await.ok();
    }
    Ok(())
}

/// Resolve the pin-store path: the explicit `--pin-store`, else the per-user
/// default (`<local data>/net-mesh/mcp-pins.json`). The shim and the pin verbs
/// resolve the same default so they read and write one shared file.
fn resolve_pin_store(override_: Option<&Path>) -> Result<PathBuf, CliError> {
    if let Some(p) = override_ {
        return Ok(p.to_path_buf());
    }
    dirs::data_local_dir()
        .or_else(dirs::home_dir)
        .map(|d| d.join("net-mesh").join("mcp-pins.json"))
        .ok_or_else(|| {
            generic(
                "could not determine a per-user data directory for the pin store; \
                 pass --pin-store <PATH>",
            )
        })
}

/// One `pin approve` / `pin reject` result row.
#[derive(Serialize)]
struct PinMutation {
    cap_id: String,
    action: &'static str,
    /// Whether this changed the stored state (an already-approved approve, or a
    /// reject of an absent pin, reports `false`).
    changed: bool,
    store: String,
}

/// One `pin list` row.
#[derive(Serialize)]
struct PinRow {
    cap_id: String,
    state: &'static str,
}

async fn run_pin(cmd: PinCommand, output: Option<OutputFormat>) -> Result<(), CliError> {
    match cmd {
        PinCommand::Approve(args) => pin_mutate(args, output, "approved").await,
        PinCommand::Reject(args) => pin_mutate(args, output, "rejected").await,
        PinCommand::List(args) => pin_list(args, output).await,
    }
}

/// Approve or reject a pin. Approval is the out-of-band consent step the model
/// cannot perform — it moves a pending request to approved (or pre-approves a
/// capability that has no request yet).
async fn pin_mutate(
    args: PinIdArgs,
    output: Option<OutputFormat>,
    action: &'static str,
) -> Result<(), CliError> {
    let id = CapabilityId::parse(&args.cap_id)
        .map_err(|e| invalid_args(format!("cap id {:?}: {e}", args.cap_id)))?;
    let path = resolve_pin_store(args.pin_store.as_deref())?;
    let mut store = PinStore::load(&path)
        .await
        .map_err(|e| generic(format!("load pin store: {e}")))?;
    let changed = match action {
        "approved" => store.approve(&id),
        _ => store.remove(&id),
    };
    store
        .save()
        .await
        .map_err(|e| generic(format!("save pin store: {e}")))?;
    let row = PinMutation {
        cap_id: id.display(),
        action,
        changed,
        store: path.display().to_string(),
    };
    emit_value(OutputFormat::resolve_oneshot(output), &row)
        .map_err(|e| generic(format!("write output: {e}")))?;
    Ok(())
}

async fn pin_list(args: PinListArgs, output: Option<OutputFormat>) -> Result<(), CliError> {
    let path = resolve_pin_store(args.pin_store.as_deref())?;
    let store = PinStore::load(&path)
        .await
        .map_err(|e| generic(format!("load pin store: {e}")))?;
    let rows: Vec<PinRow> = store
        .list()
        .into_iter()
        .map(|(id, state)| PinRow {
            cap_id: id.display(),
            state: match state {
                PinState::Approved => "approved",
                PinState::Pending => "pending",
            },
        })
        .collect();
    emit_value(OutputFormat::resolve_oneshot(output), &rows)
        .map_err(|e| generic(format!("write output: {e}")))?;
    Ok(())
}

/// Load an operator [`Identity`] from an identity TOML file (`seed_hex = ...`).
/// Mirrors `net wrap`'s loader so both sides accept the same file.
async fn load_identity(path: &Path) -> Result<Identity, CliError> {
    let text = tokio::fs::read_to_string(path).await.map_err(|e| {
        generic(format!(
            "failed to read identity file {}: {e}",
            path.display()
        ))
    })?;

    #[derive(serde::Deserialize)]
    struct IdentityFile {
        seed_hex: String,
    }
    let parsed: IdentityFile = toml::from_str(&text).map_err(|e| {
        invalid_args(format!(
            "identity file {} failed to parse: {e}",
            path.display()
        ))
    })?;
    let seed = hex::decode(&parsed.seed_hex).map_err(|e| {
        invalid_args(format!(
            "identity file {} `seed_hex` is not valid hex: {e}",
            path.display()
        ))
    })?;
    Identity::from_bytes(&seed).map_err(|e| {
        invalid_args(format!(
            "identity file {} `seed_hex` is not a valid 32-byte seed: {e:?}",
            path.display()
        ))
    })
}

/// Build a mesh under `identity` and join it via the remote peer (routed
/// handshake), the same path `net wrap` uses so the shim carries a stable,
/// owner-scoped origin.
async fn build_shim_mesh(identity: Identity, remote: &RemoteAttach) -> Result<Mesh, CliError> {
    let mesh = MeshBuilder::new("0.0.0.0:0", &remote.psk)
        .map_err(|e| connection_failure(format!("mesh builder rejected bind address: {e}")))?
        .identity(identity)
        .build()
        .await
        .map_err(|e| connection_failure(format!("mesh build failed: {e}")))?;
    mesh.start();
    mesh.connect_via(&remote.addr.to_string(), &remote.public_key, remote.node_id)
        .await
        .map_err(|e| {
            connection_failure(format!(
                "routed handshake with {} (node_id={}) failed: {e}",
                remote.addr, remote.node_id
            ))
        })?;
    Ok(mesh)
}
