//! `net wrap <name> [flags] -- <command...>` — wrap a local stdio MCP server
//! as owner-only mesh capabilities (`MCP_BRIDGE_PLAN.md` Phase 1, supply side).
//!
//! Builds a mesh node under the operator's identity, joins the mesh via a
//! remote-attach peer, then hands the wrapped server to
//! [`net_mcp::wrap::ServerPublisher::publish_server`] which discovers its
//! tools, announces them, and serves an owner-scoped nRPC handler per tool.
//! The process stays up serving until Ctrl-C; on server exit the publication
//! is withdrawn (announcement cleared, services + child stopped) and the mesh
//! shuts down.
//!
//! Owner-only is keyed on the wrap node's own `origin_hash` (doctrine #3);
//! `--allow <origin>` widens it to specific peer origins. Mapping a whole root
//! identity to the origins of its delegated nodes is a later refinement — for
//! now a remote caller is admitted by listing its origin explicitly.

use std::path::Path;
use std::path::PathBuf;

use clap::Args;
use net_mcp::spec::Implementation;
use net_mcp::wrap::{CredentialOverride, ServerPublisher, Substitutability, WrapConfig};
use tokio::sync::broadcast;

use crate::commands::aggregator::RemoteAttachArgs;
use crate::context::{
    build_attached_mesh, load_operator_identity, require_remote_attach, resolve_profile,
};
use crate::error::{generic, invalid_args, sdk, CliError};
use crate::output::{emit_stream_row, OutputFormat};
use crate::parsers::parse_u64_flexible;

/// A `net wrap` output event, emitted through the `--output` pipeline like
/// every other command (`json` / `ndjson` / `yaml` / `table` / `text`). Wrap
/// is long-running, so it emits a stream: one `wrapped` event, then a
/// `tools_changed` / `server_exited` event per lifecycle transition.
#[derive(serde::Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum WrapEvent<'a> {
    /// The initial report: served + skipped tools, the announced
    /// visibility/scope, and any explicitly-widened caller origins.
    Wrapped {
        name: &'a str,
        tools: &'a [String],
        skipped: &'a [String],
        visibility: &'a str,
        /// The announced invocation-scope label — the baseline (the owning
        /// root identity). `--allow` widens the *local* enforcement beyond
        /// this without changing the announced label, so consumers must read
        /// `allowed_origins` too to know who may actually invoke.
        scope: &'a str,
        /// Peer origins explicitly admitted via `--allow`, on top of the owner
        /// scope. Empty unless the operator widened access — so the structured
        /// output states exactly who beyond same-root may invoke, rather than
        /// implying only same-root through the static `scope`.
        allowed_origins: &'a [u64],
    },
    /// The wrapped server changed its tool set; the mesh was reconciled.
    ToolsChanged {
        added: Vec<String>,
        removed: Vec<String>,
    },
    /// The wrapped server exited; capabilities were withdrawn.
    ServerExited,
}

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
    output: Option<OutputFormat>,
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
    let identity = load_operator_identity(identity_path).await?;

    // Build a mesh under that identity and join via the peer. `Arc` because
    // the publisher (and each publication) holds the mesh alongside us.
    let mesh =
        std::sync::Arc::new(build_attached_mesh("0.0.0.0:0", Some(identity), &remote).await?);

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
    // Widen the local enforcement scope by ref so the origins remain available
    // to report in the output event below.
    for &origin in &allow {
        config.scope.allow(origin);
    }

    let publisher = ServerPublisher::new(std::sync::Arc::clone(&mesh));
    let mut publication = publisher
        .publish_server(program, prog_args, &envs, config)
        .await
        .map_err(|e| sdk(format!("wrap failed: {e}")))?;

    // Report what was wrapped through the `--output` pipeline. Wrap streams
    // (report + lifecycle events), so it resolves the stream format.
    let fmt = OutputFormat::resolve_stream(output);
    emit_stream_row(
        fmt,
        &WrapEvent::Wrapped {
            name: &args.name,
            tools: publication.tools(),
            skipped: publication.skipped_tools(),
            visibility: "owner_only",
            scope: "same_root_identity",
            allowed_origins: &allow,
        },
    )
    .map_err(|e| generic(format!("write output: {e}")))?;

    // Serve until Ctrl-C, refreshing whenever the wrapped server changes its
    // tool set (`tools/list_changed`) so bridged descriptors stay current.
    let mut changed = publication.client().subscribe_list_changed();
    // A separate Arc so the `closed()` select branch doesn't borrow
    // `publication` (which `refresh` borrows mutably in another branch's body).
    let client = std::sync::Arc::clone(publication.client());
    let server_exited = loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break false,
            // The wrapped server exited (clean or crash) — withdraw and stop.
            _ = client.closed() => break true,
            recv = changed.recv() => match recv {
                // A change (or a lagged signal) — reconcile the mesh.
                Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) => {
                    match publication.refresh().await {
                        Ok(delta) if !delta.is_empty() => {
                            let _ = emit_stream_row(
                                fmt,
                                &WrapEvent::ToolsChanged {
                                    added: delta.added,
                                    removed: delta.removed,
                                },
                            );
                        }
                        Ok(_) => {}
                        // Diagnostics stay on stderr, off the structured stdout stream.
                        Err(e) => eprintln!("refresh failed: {e}"),
                    }
                }
                // Broadcast channel closed (client dropped) — stop.
                Err(broadcast::error::RecvError::Closed) => break false,
            },
        }
    };

    if server_exited {
        // Withdraw the publication so peers stop advertising tools whose
        // handlers are about to drop. Log a failure (stderr, off the
        // structured stdout stream) rather than swallowing it — otherwise a
        // stale announcement could linger with no live backing handler and no
        // diagnostic. Mirrors the refresh path.
        if let Err(e) = publication.withdraw().await {
            eprintln!("withdrawing capabilities on server exit failed: {e}");
        }
        let _ = emit_stream_row(fmt, &WrapEvent::ServerExited);
    } else {
        // Ctrl-C: the whole node is going down, so the announcement dies with
        // it — dropping the publication stops the services and the child.
        drop(publication);
    }
    drop(publisher);
    if let Ok(mesh) = std::sync::Arc::try_unwrap(mesh) {
        mesh.shutdown().await.ok();
    }
    Ok(())
}

// Identity loading and mesh attachment are shared with `net mcp serve` — see
// `context::load_operator_identity` and `context::build_attached_mesh`.

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

    fn wrapped_event(allow: &[u64]) -> serde_json::Value {
        let tools = vec!["echo".to_string()];
        let skipped: Vec<String> = Vec::new();
        serde_json::to_value(WrapEvent::Wrapped {
            name: "gh",
            tools: &tools,
            skipped: &skipped,
            visibility: "owner_only",
            scope: "same_root_identity",
            allowed_origins: allow,
        })
        .unwrap()
    }

    #[test]
    fn wrapped_event_reports_widened_allow_origins() {
        // With `--allow`, the widened origins appear in the structured output,
        // so a consumer isn't misled by the static `scope` into assuming only
        // same-root callers are permitted.
        let v = wrapped_event(&[7, 42]);
        assert_eq!(v["event"], "wrapped");
        assert_eq!(v["scope"], "same_root_identity");
        assert_eq!(v["allowed_origins"], serde_json::json!([7, 42]));
    }

    #[test]
    fn wrapped_event_default_scope_is_same_root_only() {
        // No `--allow`: an empty list is the honest "same-root only" case.
        let v = wrapped_event(&[]);
        assert_eq!(v["allowed_origins"], serde_json::json!([]));
    }
}
