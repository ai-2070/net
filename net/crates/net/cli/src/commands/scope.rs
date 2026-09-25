//! Explicit opt-in for commands backed only by a fresh Deck supervisor.

use clap::Args;

use crate::error::{invalid_args, CliError};

pub(super) const NOTICE: &str =
    "Starts a temporary supervisor for this command; does not inspect a running node.";

#[derive(Args, Debug)]
pub struct LocalScope {
    #[arg(long, help = NOTICE)]
    pub local: bool,
}

/// Read-only views that can resolve their temporary context without starting it.
#[derive(Args, Debug)]
pub struct InspectableLocalScope {
    #[arg(long, help = NOTICE)]
    pub local: bool,
    /// Inspect context selection without starting a temporary supervisor.
    #[arg(long)]
    pub inspect_target: bool,
}

pub(super) async fn inspect_temporary(
    profile: &crate::config::Profile,
    identity: Option<&std::path::Path>,
    node: u64,
    output: Option<crate::output::OutputFormat>,
) -> Result<(), CliError> {
    inspect_context(profile, identity, node, output, false).await
}

pub(super) async fn inspect_temporary_write(
    profile: &crate::config::Profile,
    identity: Option<&std::path::Path>,
    node: u64,
    output: Option<crate::output::OutputFormat>,
) -> Result<(), CliError> {
    inspect_context(profile, identity, node, output, true).await
}

async fn inspect_context(
    profile: &crate::config::Profile,
    identity: Option<&std::path::Path>,
    node: u64,
    output: Option<crate::output::OutputFormat>,
    identity_required: bool,
) -> Result<(), CliError> {
    crate::context::validate_endpoint(profile)?;
    let mut target = crate::target::inspect(
        profile,
        &super::aggregator::RemoteAttachArgs::default(),
        identity,
        None,
        "temporary_supervisor",
    )
    .await?;
    if identity_required && identity.or(profile.identity.as_deref()).is_none() {
        target.unavailable_identity("execution requires a configured identity");
    }
    #[derive(serde::Serialize)]
    struct View {
        #[serde(flatten)]
        target: crate::target::TargetInspection,
        supervisor_node_id: u64,
        identity_required: bool,
    }
    crate::output::emit_value(
        crate::output::OutputFormat::resolve_oneshot(output),
        &View {
            target,
            supervisor_node_id: node,
            identity_required,
        },
    )
    .map_err(|e| crate::error::generic(format!("write inspection: {e}")))
}

pub(super) fn validate_local(local: bool, command: &str) -> Result<(), CliError> {
    if !local {
        return Err(invalid_args(format!(
            "`{command}` cannot read a running deployment or administer it. \
             Pass --local only for development use. {NOTICE} \
             Remote Deck administration is not implemented; remote aggregator \
             RPC is a separate, explicitly targeted operation."
        )));
    }
    Ok(())
}

pub(super) fn require_local(local: bool, command: &str) -> Result<(), CliError> {
    validate_local(local, command)?;
    // Scope is a safety disclosure, not progress: do not hide it under --quiet
    // or let an environment logging filter suppress it.
    eprintln!("net-mesh: --local: {NOTICE}");
    Ok(())
}
