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
    let man = clap_mangen::Man::new(cmd);
    let mut buf = Vec::new();
    man.render(&mut buf)
        .map_err(|e| error::generic(format!("render man page: {e}")))?;
    std::io::stdout()
        .write_all(&buf)
        .map_err(|e| error::generic(format!("write stdout: {e}")))?;
    Ok(())
}
