//! `net version` — print the binary version + the embedded SDK
//! version + git revision the binary was built against.
//!
//! Pure read-only: no SDK runtime is touched, no config is
//! consulted. The first subcommand to ship in Phase 1 so the
//! clap routing skeleton has something concrete to dispatch.

use crate::prelude::*;

pub async fn run() -> Result<(), CliError> {
    let info = VersionInfo {
        // `CARGO_PKG_VERSION` is set by cargo at build time —
        // tracks `cli/Cargo.toml:version`.
        cli_version: env!("CARGO_PKG_VERSION"),
        // Embed the SDK version the binary is linked against.
        // The SDK's `Cargo.toml:version` always tracks the
        // workspace version, so this is also the substrate
        // version.
        sdk_version: "0.17.0",
        // `OPTION_ENV!` returns `None` when the var isn't set;
        // CI populates `NET_BUILD_REVISION` from `git rev-parse
        // --short HEAD` in release builds.
        git_revision: option_env!("NET_BUILD_REVISION"),
    };
    // `version` predates the `--output` global flag's full
    // dispatch wiring; emit JSON to stdout unconditionally —
    // the value is small enough that table-vs-JSON doesn't
    // change ergonomics, and consumers piping the output to
    // `jq` get a stable shape.
    emit_value(OutputFormat::Json, &info)
        .map_err(|e| crate::error::generic(format!("failed to write version output: {e}")))
}

#[derive(serde::Serialize)]
struct VersionInfo {
    cli_version: &'static str,
    sdk_version: &'static str,
    git_revision: Option<&'static str>,
}
