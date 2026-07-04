//! `net forwarding` — manage the caller-side credential/header forwarding
//! **policy** and audit it (`MCP_CREDENTIAL_FORWARDING_PLAN.md` Phase 1).
//!
//! This is the operator surface over [`net_mcp::forward::ForwardingStore`]: the
//! global kill switch, per-secret destination bindings, and the redaction-safe
//! audit that lists every grant. It manages **policy only** — the destination
//! bindings that are safe to audit — never secret *values*. Value entry (`net
//! secret set`) arrives with the value backend (keychain / encrypted store);
//! until then a configured ref has a policy but no value, and forwarding stays
//! off by default regardless.
//!
//! Mirrors `net mcp pin`: the store path resolves to a per-user default (or an
//! explicit `--store`), writes go through the store's cross-process locked
//! `mutate`, and output flows through the `--output` pipeline — except the
//! human `audit` view, which renders the store's own value-free table for
//! `text`/`table` and structured JSON/YAML otherwise.

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use net_mcp::forward::{AllowList, ForwardingStore, ProviderScope, StoreError};
use serde::Serialize;

use crate::error::{generic, invalid_args, CliError};
use crate::prelude::{emit_value, OutputFormat};

#[derive(Subcommand, Debug)]
pub enum ForwardingCommand {
    /// Turn the global forwarding kill switch ON. Forwarding still requires a
    /// matching per-secret grant AND destination acceptance.
    Enable(StoreArgs),
    /// Turn the global forwarding kill switch OFF (the default). Nothing is
    /// forwarded while off, whatever the grants say.
    Disable(StoreArgs),
    /// Configure a secret ref's forwarding policy — the wire header it injects
    /// as and where it may go. Does NOT enter the secret value (that needs the
    /// value backend).
    Allow(AllowArgs),
    /// Remove a secret ref's forwarding policy.
    Rm(RmArgs),
    /// Show every configured grant — value-free by construction.
    Audit(StoreArgs),
    /// Store a secret ref's VALUE in the OS keychain, read from stdin (so it
    /// never lands in argv / shell history). Requires a build with
    /// `--features keychain`; the policy (`allow`) is configured separately.
    SetValue(SetValueArgs),
}

#[derive(Args, Debug)]
pub struct StoreArgs {
    /// Forwarding policy store file. Defaults to the per-user store.
    #[arg(long = "store", value_name = "PATH")]
    pub store: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct AllowArgs {
    /// The secret ref name — a lowercase slug label (`github-token`), never the
    /// value.
    pub ref_name: String,

    /// The wire header the secret is injected as (e.g. `Authorization`).
    #[arg(long)]
    pub header: String,

    /// A provider id (node id or `org:<name>`) allowed to receive it.
    /// Repeatable. Omit both this and `--any-provider` to deny all (the safe
    /// default).
    #[arg(long = "provider", value_name = "ID")]
    pub provider: Vec<String>,

    /// Allow ANY provider. Intended only for a vetted non-secret header; a
    /// credential bound to `any` is almost always a mistake.
    #[arg(long = "any-provider", conflicts_with = "provider")]
    pub any_provider: bool,

    /// A capability-id glob this secret may accompany (e.g. `github.*`).
    /// Repeatable. Empty matches nothing.
    #[arg(long = "capability", value_name = "GLOB")]
    pub capability: Vec<String>,

    /// Optional audit-legibility label (`--purpose github-api`).
    #[arg(long)]
    pub purpose: Option<String>,

    /// Confirm configuring a `Cookie` / `Set-Cookie` header (session cookies
    /// are ambient authority in its worst form).
    #[arg(long)]
    pub force: bool,

    #[command(flatten)]
    pub store: StoreArgs,
}

#[derive(Args, Debug)]
pub struct RmArgs {
    /// The secret ref to remove.
    pub ref_name: String,

    #[command(flatten)]
    pub store: StoreArgs,
}

#[derive(Args, Debug)]
pub struct SetValueArgs {
    /// The secret ref name to store the value under (should match a ref you
    /// configured with `net forwarding allow`).
    pub ref_name: String,
}

pub async fn run(
    cmd: ForwardingCommand,
    output: Option<OutputFormat>,
    _config_path: Option<&Path>,
    _profile_name: &str,
) -> Result<(), CliError> {
    match cmd {
        ForwardingCommand::Enable(args) => set_enabled(args, output, true).await,
        ForwardingCommand::Disable(args) => set_enabled(args, output, false).await,
        ForwardingCommand::Allow(args) => allow(args, output).await,
        ForwardingCommand::Rm(args) => rm(args, output).await,
        ForwardingCommand::Audit(args) => audit(args, output).await,
        ForwardingCommand::SetValue(args) => set_value(args, output).await,
    }
}

/// Store a secret value in the OS keychain (keychain-feature build). The value
/// is read from stdin so it never enters argv or shell history; a single
/// trailing newline is stripped. The value is never echoed back.
#[cfg(feature = "keychain")]
async fn set_value(args: SetValueArgs, output: Option<OutputFormat>) -> Result<(), CliError> {
    use net_mcp::forward::{validate_ref_name, KeychainSecretBackend, DEFAULT_KEYCHAIN_SERVICE};
    use tokio::io::AsyncReadExt;

    // Reject a mistyped ref name before reading the secret: the keychain account
    // is this name verbatim, but the policy side stores refs as lowercase slugs
    // (`net forwarding allow`), so a value under a non-slug name can never be
    // resolved and would fail silently as `ValueMissing` at forward time.
    validate_ref_name(&args.ref_name).map_err(|e| invalid_args(e.to_string()))?;

    let mut buf = Vec::new();
    tokio::io::stdin()
        .read_to_end(&mut buf)
        .await
        .map_err(|e| generic(format!("read secret from stdin: {e}")))?;
    // Strip one trailing newline (`\n` or `\r\n`) from the piped value.
    if buf.last() == Some(&b'\n') {
        buf.pop();
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
    }
    if buf.is_empty() {
        return Err(invalid_args("no secret value on stdin (pipe the value in)"));
    }

    let backend = KeychainSecretBackend::default();
    let result = backend.set(&args.ref_name, &buf).await;
    // Best-effort scrub of the local plaintext copy regardless of outcome.
    buf.iter_mut().for_each(|b| *b = 0);
    result.map_err(|e| generic(format!("store secret in keychain: {e}")))?;

    emit_row(
        output,
        MutationRow {
            action: "value-set",
            ref_name: Some(args.ref_name),
            changed: true,
            store: format!("keychain:{DEFAULT_KEYCHAIN_SERVICE}"),
        },
    )
}

/// Fallback when the binary was built without the `keychain` feature: there is
/// no value store, so say so rather than silently missing the command.
#[cfg(not(feature = "keychain"))]
async fn set_value(_args: SetValueArgs, _output: Option<OutputFormat>) -> Result<(), CliError> {
    Err(generic(
        "this `net` build has no secret value store; rebuild net-cli with \
         `--features keychain` to enter secret values",
    ))
}

/// Resolve the forwarding-store path: the explicit `--store`, else the per-user
/// default (`<local data>/net-mesh/forwarding.json`) — the same location every
/// `net forwarding` verb reads and writes.
fn resolve_store(override_: Option<&Path>) -> Result<PathBuf, CliError> {
    if let Some(p) = override_ {
        return Ok(p.to_path_buf());
    }
    dirs::data_local_dir()
        .or_else(dirs::home_dir)
        .map(|d| d.join("net-mesh").join("forwarding.json"))
        .ok_or_else(|| {
            generic(
                "could not determine a per-user data directory for the forwarding store; \
                 pass --store <PATH>",
            )
        })
}

/// Map a store error onto the CLI exit surface: a policy-validation failure is
/// a usage error; an I/O / corruption failure is generic.
fn store_err(e: StoreError) -> CliError {
    match e {
        StoreError::InvalidRefName { .. }
        | StoreError::HeaderNotForwardable { .. }
        | StoreError::CookieRequiresForce { .. }
        | StoreError::SensitiveHeaderNotPlain { .. }
        | StoreError::SecretProviderAny { .. }
        | StoreError::Header(_) => invalid_args(e.to_string()),
        StoreError::Io { .. } | StoreError::Corrupt { .. } => generic(e.to_string()),
    }
}

/// One `enable` / `disable` / `allow` / `rm` result row.
#[derive(Serialize)]
struct MutationRow {
    action: &'static str,
    /// The ref this acted on (absent for the kill-switch verbs).
    #[serde(skip_serializing_if = "Option::is_none")]
    ref_name: Option<String>,
    /// Whether the store changed (a no-op rm / redundant toggle reports false).
    changed: bool,
    store: String,
}

async fn set_enabled(
    args: StoreArgs,
    output: Option<OutputFormat>,
    enabled: bool,
) -> Result<(), CliError> {
    let path = resolve_store(args.store.as_deref())?;
    let changed = ForwardingStore::mutate(path.clone(), |s| {
        let was = s.is_enabled();
        s.set_enabled(enabled);
        Ok(was != enabled)
    })
    .await
    .map_err(store_err)?;
    emit_row(
        output,
        MutationRow {
            action: if enabled { "enabled" } else { "disabled" },
            ref_name: None,
            changed,
            store: path.display().to_string(),
        },
    )
}

async fn allow(args: AllowArgs, output: Option<OutputFormat>) -> Result<(), CliError> {
    let providers = if args.any_provider {
        ProviderScope::Any
    } else if args.provider.is_empty() {
        ProviderScope::None
    } else {
        ProviderScope::Ids(args.provider.clone())
    };
    let allow = AllowList {
        providers,
        capabilities: args.capability.clone(),
    };
    let path = resolve_store(args.store.store.as_deref())?;
    let ref_name = args.ref_name.clone();
    ForwardingStore::mutate(path.clone(), move |s| {
        s.set_secret(
            &args.ref_name,
            &args.header,
            allow,
            args.purpose,
            args.force,
        )
    })
    .await
    .map_err(store_err)?;
    emit_row(
        output,
        MutationRow {
            action: "allowed",
            ref_name: Some(ref_name),
            changed: true,
            store: path.display().to_string(),
        },
    )
}

async fn rm(args: RmArgs, output: Option<OutputFormat>) -> Result<(), CliError> {
    let path = resolve_store(args.store.store.as_deref())?;
    let ref_name = args.ref_name.clone();
    let changed =
        ForwardingStore::mutate(path.clone(), move |s| Ok(s.remove_secret(&args.ref_name)))
            .await
            .map_err(store_err)?;
    emit_row(
        output,
        MutationRow {
            action: "removed",
            ref_name: Some(ref_name),
            changed,
            store: path.display().to_string(),
        },
    )
}

async fn audit(args: StoreArgs, output: Option<OutputFormat>) -> Result<(), CliError> {
    let path = resolve_store(args.store.as_deref())?;
    let store = ForwardingStore::load(&path).await.map_err(store_err)?;
    let audit = store.audit();
    let fmt = OutputFormat::resolve_oneshot(output);
    match fmt {
        // The human views get the store's own value-free table.
        OutputFormat::Text | OutputFormat::Table => {
            print!("{}", audit.render());
            Ok(())
        }
        // Machine views get the structured object.
        _ => emit_value(fmt, &audit).map_err(|e| generic(format!("write output: {e}"))),
    }
}

fn emit_row(output: Option<OutputFormat>, row: MutationRow) -> Result<(), CliError> {
    emit_value(OutputFormat::resolve_oneshot(output), &row)
        .map_err(|e| generic(format!("write output: {e}")))
}
