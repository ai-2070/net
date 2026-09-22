//! `net-mesh man` — emit the binary's troff(1) man page on stdout.
//!
//! Release tarballs capture the output into
//! `share/man/man1/net-mesh.1` so distro packagers (deb / rpm / AUR
//! / Homebrew) get a ready-to-install man page without re-running
//! the binary at packaging time. Users who installed via
//! `cargo install` can roll their own:
//!
//! ```sh
//! net-mesh man | gzip > /usr/local/share/man/man1/net-mesh.1.gz
//! ```
//!
//! `clap_mangen` walks the parsed clap `Command` tree and renders
//! every subcommand's flags + descriptions; the result is the full
//! per-subcommand reference shown in the `--help` output, formatted
//! for `man(1)`.

use std::io::Write;

use clap::CommandFactory;

use crate::error::{self, CliError};

pub fn run<C: CommandFactory>() -> Result<(), CliError> {
    let cmd = C::command();
    let mut buf = Vec::new();
    render_pages(&cmd, &mut buf)?;
    std::io::stdout()
        .write_all(&buf)
        .map_err(|e| error::generic(format!("write stdout: {e}")))?;
    Ok(())
}

/// Render the root page followed by every subcommand's own page, depth
/// first, into the one output stream. `clap_mangen` renders one `Command`
/// per page, so the tree walk above `Man::new` is what carries each
/// subcommand's flags + descriptions into the reference.
fn render_pages(cmd: &clap::Command, buf: &mut Vec<u8>) -> Result<(), CliError> {
    clap_mangen::Man::new(cmd.clone())
        .render(buf)
        .map_err(|e| error::generic(format!("render man page: {e}")))?;
    for sub in cmd.get_subcommands() {
        render_pages(sub, buf)?;
    }
    Ok(())
}
