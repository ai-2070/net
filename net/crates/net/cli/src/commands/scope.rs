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
