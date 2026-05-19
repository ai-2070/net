//! `net-mesh completion <shell>` — emit a shell-completion script
//! for the requested shell on stdout.
//!
//! Operators wire this into their shell init or — via the release
//! tarball — get pre-generated scripts under
//! `share/{bash-completion,zsh,fish,powershell}/`. The runtime
//! subcommand lets users who installed via `cargo install` or
//! `cargo binstall` (which skip the tarball's `share/` payload)
//! pull completions on demand:
//!
//! ```sh
//! net-mesh completion bash > /etc/bash_completion.d/net-mesh
//! net-mesh completion zsh  > "$fpath[1]/_net-mesh"
//! net-mesh completion fish > ~/.config/fish/completions/net-mesh.fish
//! net-mesh completion powershell | Out-String | Invoke-Expression
//! ```
//!
//! The tarball-build workflow shells out to the freshly-compiled
//! binary on the native runner to capture each shell's script into
//! `share/`, so cross-compiled targets ship the same completion
//! files (they're shell scripts, not architecture-dependent).

use clap::CommandFactory;
use clap_complete::{generate, Shell};

use crate::error::CliError;

#[derive(clap::Args, Debug)]
pub struct CompletionArgs {
    /// Which shell to emit completions for.
    #[arg(value_enum)]
    pub shell: Shell,
}

pub fn run<C: CommandFactory>(args: CompletionArgs) -> Result<(), CliError> {
    let mut cmd = C::command();
    let name = cmd.get_name().to_string();
    generate(args.shell, &mut cmd, name, &mut std::io::stdout());
    Ok(())
}
