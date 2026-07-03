//! `net wrap <name> [flags] -- <command...>` — wrap a local stdio MCP server
//! as owner-only mesh capabilities (`MCP_BRIDGE_PLAN.md` Phase 1, supply side).
//!
//! Builds a mesh node under the operator's identity, joins the mesh via a
//! remote-attach peer, then hands the wrapped server to
//! [`net_mcp::wrap::wrap_server`] which discovers its tools, announces them,
//! and serves an owner-scoped nRPC handler per tool. The process stays up
//! serving until Ctrl-C; on shutdown the wrap session drops (withdrawing the
//! services and stopping the wrapped process) and the mesh shuts down.
//!
//! Owner-only is keyed on the wrap node's own `origin_hash` (doctrine #3);
//! `--allow <origin>` widens it to specific peer origins. Mapping a whole root
//! identity to the origins of its delegated nodes is a later refinement — for
//! now a remote caller is admitted by listing its origin explicitly.

use std::path::Path;
use std::path::PathBuf;

use clap::Args;
use net_mcp::spec::Implementation;
use net_mcp::wrap::{wrap_server, CredentialOverride, Substitutability, WrapConfig};
use net_sdk::identity::Identity;
use net_sdk::{Mesh, MeshBuilder};
use tokio::sync::broadcast;

use crate::commands::aggregator::RemoteAttachArgs;
use crate::context::{require_remote_attach, resolve_profile, RemoteAttach};
use crate::error::{connection_failure, generic, invalid_args, sdk, CliError};
use crate::output::OutputFormat;
use crate::parsers::parse_u64_flexible;

#[derive(Args, Debug)]
pub struct WrapArgs {
    /// A short label for this wrapped server (shown in output; not a tool id).
    pub name: String,

    /// Force credential status to `credentialed` (upward — always allowed).
    #[arg(long, conflicts_with = "no_credentials")]
    pub credentialed: bool,

    /// Force credential status to `none` (downward — requires `--force`).
    #[arg(long)]
    pub no_credentials: bool,

    /// Confirm a downward `--no-credentials` override.
    #[arg(long)]
    pub force: bool,

    /// Declare the wrapped tools substitutable across providers (Phase 4).
    #[arg(long)]
    pub substitutable: bool,

    /// Environment variable for the wrapped server (`KEY=VALUE`, repeatable).
    /// Stays in the child process on this machine; never transits the mesh.
    #[arg(long = "env", value_name = "KEY=VALUE")]
    pub env: Vec<String>,

    /// Also admit this caller `origin_hash` (decimal or `0x`-hex, repeatable).
    /// Owner-only by default; widen for specific peers.
    #[arg(long = "allow", value_name = "ORIGIN")]
    pub allow: Vec<String>,

    /// Operator identity file. Defaults to the profile's `identity`. Owner-only
    /// scoping keys on it, so a stable identity (not an ephemeral key) is
    /// required.
    #[arg(long)]
    pub identity: Option<PathBuf>,

    /// The mesh peer to join.
    #[command(flatten)]
    pub remote: RemoteAttachArgs,

    /// The stdio MCP server command + args, after `--`.
    #[arg(last = true, required = true, value_name = "COMMAND")]
    pub command: Vec<String>,
}

pub async fn run(
    args: WrapArgs,
    _output: Option<OutputFormat>,
    config_path: Option<&Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    let profile = resolve_profile(config_path, profile_name).await?;

    // The mesh peer to join. `net wrap` must join a mesh to be reachable.
    let remote = require_remote_attach(&profile, &args.remote, || {
        invalid_args(
            "net wrap needs a mesh peer to join. Pass \
             --node-addr/--node-pubkey/--node-id/--psk-hex (or set them in your \
             profile) pointing at a running mesh node.",
        )
    })?;

    // Operator identity — owner-only keys on this node's origin.
    let identity_path = args
        .identity
        .as_deref()
        .or(profile.identity.as_deref())
        .ok_or_else(|| {
            invalid_args(
                "net wrap needs an operator identity: pass --identity <PATH> or set \
                 `identity = \"...\"` in your profile. Owner-only scoping keys on it, \
                 so an ephemeral key would admit nobody.",
            )
        })?;
    let identity = load_identity(identity_path).await?;

    // Build a mesh under that identity and join via the peer.
    let mesh = build_wrap_mesh(identity, &remote).await?;

    // Parse the rest of the operator's intent.
    let (program, prog_args) = args
        .command
        .split_first()
        .ok_or_else(|| invalid_args("the wrapped command after `--` is empty"))?;
    let envs = parse_env_pairs(&args.env)?;
    let allow = parse_allow_origins(&args.allow)?;

    let mut config = WrapConfig::owner_only(
        Implementation {
            name: format!("net-wrap/{}", args.name),
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
        mesh.origin_hash(),
    );
    config.credential_override =
        resolve_credential_override(args.credentialed, args.no_credentials);
    config.force = args.force;
    config.substitutability = if args.substitutable {
        Substitutability::ProviderEquivalent
    } else {
        Substitutability::ProviderLocal
    };
    for origin in allow {
        config.scope.allow(origin);
    }

    let mut session = wrap_server(&mesh, program, prog_args, &envs, config)
        .await
        .map_err(|e| sdk(format!("wrap failed: {e}")))?;

    // Report what was wrapped.
    println!(
        "wrapped {} tool(s) from `{}`:",
        session.tools().len(),
        args.name
    );
    for tool in session.tools() {
        println!("  {tool}");
    }
    if !session.skipped_tools().is_empty() {
        eprintln!(
            "skipped {} tool(s) whose names aren't valid service ids: {:?}",
            session.skipped_tools().len(),
            session.skipped_tools(),
        );
    }
    println!("visibility: owner_only");
    println!("scope: same_root_identity");
    println!("serving — press Ctrl-C to stop.");

    // Serve until Ctrl-C, refreshing whenever the wrapped server changes its
    // tool set (`tools/list_changed`) so bridged descriptors stay current. On
    // teardown the session drops (withdrawing services + stopping the child)
    // and the mesh shuts down.
    let mut changed = session.client().subscribe_list_changed();
    // A separate Arc so the `closed()` select branch doesn't borrow `session`
    // (which `refresh` borrows mutably in another branch's body).
    let client = std::sync::Arc::clone(session.client());
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            // The wrapped server exited (clean or crash) — withdraw and stop.
            _ = client.closed() => {
                eprintln!("wrapped server exited; withdrawing capabilities.");
                let _ = mesh
                    .announce_capabilities(net_sdk::capabilities::CapabilitySet::new())
                    .await;
                break;
            }
            recv = changed.recv() => match recv {
                // A change (or a lagged signal) — reconcile the mesh.
                Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) => {
                    match session.refresh(&mesh).await {
                        Ok(delta) if !delta.is_empty() => println!(
                            "tools changed: added {:?}, removed {:?}",
                            delta.added, delta.removed,
                        ),
                        Ok(_) => {}
                        Err(e) => eprintln!("refresh failed: {e}"),
                    }
                }
                // Broadcast channel closed (client dropped) — stop.
                Err(broadcast::error::RecvError::Closed) => break,
            },
        }
    }
    drop(session);
    mesh.shutdown().await.ok();
    Ok(())
}

/// Load an operator [`Identity`] from an identity TOML file (`seed_hex = ...`).
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
/// handshake, mirroring the CLI's remote-attach path but with the operator's
/// identity so the served capabilities carry a stable, owner-scoped origin).
async fn build_wrap_mesh(identity: Identity, remote: &RemoteAttach) -> Result<Mesh, CliError> {
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

/// Parse `KEY=VALUE` env pairs.
fn parse_env_pairs(raw: &[String]) -> Result<Vec<(String, String)>, CliError> {
    raw.iter()
        .map(|kv| {
            kv.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .ok_or_else(|| invalid_args(format!("--env {kv:?} must be KEY=VALUE")))
        })
        .collect()
}

/// Parse `--allow` origin hashes (decimal or `0x`-hex).
fn parse_allow_origins(raw: &[String]) -> Result<Vec<u64>, CliError> {
    raw.iter()
        .map(|s| parse_u64_flexible(s).map_err(|e| invalid_args(format!("--allow {s:?}: {e}"))))
        .collect()
}

/// Resolve the credential override from the two flags (upward beats detect;
/// downward is validated later against `--force`).
fn resolve_credential_override(credentialed: bool, no_credentials: bool) -> CredentialOverride {
    if credentialed {
        CredentialOverride::Credentialed
    } else if no_credentials {
        CredentialOverride::NoCredentials
    } else {
        CredentialOverride::Detect
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_pairs_parse_and_reject_missing_equals() {
        let ok = parse_env_pairs(&["A=1".to_string(), "B=x=y".to_string()]).unwrap();
        assert_eq!(
            ok,
            vec![("A".into(), "1".into()), ("B".into(), "x=y".into())]
        );
        assert!(parse_env_pairs(&["nope".to_string()]).is_err());
    }

    #[test]
    fn allow_origins_parse_decimal_and_hex() {
        let got = parse_allow_origins(&["7".to_string(), "0x2a".to_string()]).unwrap();
        assert_eq!(got, vec![7, 42]);
        assert!(parse_allow_origins(&["nan".to_string()]).is_err());
    }

    #[test]
    fn credential_override_precedence() {
        assert_eq!(
            resolve_credential_override(true, false),
            CredentialOverride::Credentialed
        );
        assert_eq!(
            resolve_credential_override(false, true),
            CredentialOverride::NoCredentials
        );
        assert_eq!(
            resolve_credential_override(false, false),
            CredentialOverride::Detect
        );
    }
}
