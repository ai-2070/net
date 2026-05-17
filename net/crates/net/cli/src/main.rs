//! `net` — unified command-line interface for the Net mesh SDK.
//!
//! Phase 1 of `NET_CLI_PLAN.md`: scaffolding + read-only surface.
//! Subsequent phases bolt admin writes / nRPC / ICE / daemon-run /
//! blob absorption on top of the same routing skeleton.
//!
//! # Entry point shape
//!
//! `tokio::main` builds the multi-thread runtime once, parses the
//! global `Cli` struct via clap, builds a [`CliContext`] (config +
//! identity + tracing), then dispatches to the matched subcommand.
//! Every subcommand returns an [`ExitCode`] — typed errors flow
//! through [`error::ExitCode`] which maps onto the documented exit
//! table at `NET_CLI_PLAN.md:§"Exit codes (locked)"`.
//!
//! # Module map
//!
//! - [`commands`] — one module per top-level subcommand.
//! - [`config`] — profile-file parsing + env-var fallback.
//! - [`output`] — `--output (json|yaml|ndjson|table|text)` dispatch.
//! - [`error`] — typed exit-code surface; `main` returns its code.
//! - [`prelude`] — re-exports the SDK types every command imports.

mod commands;
mod config;
mod context;
mod error;
mod output;
mod parsers;
mod prelude;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::error::CliError;
use crate::output::OutputFormat;

/// Global argv shape — applies to every subcommand.
#[derive(Parser, Debug)]
#[command(
    name = "net",
    bin_name = "net",
    version,
    about = "Unified command-line interface for the Net mesh.",
    long_about = "Net is the operational counterpart to Deck — \
                  a non-interactive command-line tool that wraps \
                  the Rust SDK for one-shot operator commands, CI \
                  scripting, daemon authoring, and ad-hoc cluster \
                  inspection. See NET_CLI_PLAN.md for the full \
                  surface."
)]
struct Cli {
    /// Profile file path. Defaults to `$XDG_CONFIG_HOME/net/config.toml`.
    #[arg(long, global = true, env = "NET_CONFIG")]
    config: Option<PathBuf>,

    /// Named profile within the config file.
    #[arg(long, global = true, env = "NET_PROFILE", default_value = "default")]
    profile: String,

    /// Output format. Auto-detects `table`/`text` for TTY stdout
    /// and `json`/`ndjson` for non-TTY when omitted.
    #[arg(long, global = true, value_enum)]
    output: Option<OutputFormat>,

    /// Suppress progress diagnostics on stderr.
    #[arg(long, short = 'q', global = true)]
    quiet: bool,

    /// Increase verbosity. `-v` = info, `-vv` = debug, `-vvv` = trace.
    #[arg(long, short = 'v', global = true, action = clap::ArgAction::Count)]
    verbose: u8,

    /// Disable ANSI colour in table / text output. Follows
    /// `$NO_COLOR` when not specified.
    #[arg(long, global = true, env = "NO_COLOR")]
    no_color: bool,

    /// Global per-call timeout. Subcommand-specific timeouts
    /// override this when explicitly set.
    #[arg(long, global = true, value_parser = humantime::parse_duration, default_value = "30s")]
    timeout: std::time::Duration,

    #[command(subcommand)]
    command: Command,
}

/// Top-level subcommand dispatch. Each variant maps onto a module
/// under [`commands`]. New subcommands plug in by adding a variant
/// here + a matching `mod` declaration in `commands/mod.rs`.
#[derive(Subcommand, Debug)]
enum Command {
    /// Print the SDK version + build metadata.
    Version,
    /// Operator identity authoring + inspection.
    #[command(subcommand)]
    Identity(commands::identity::IdentityCommand),
    /// Signed admin-chain commits (9 verbs).
    #[command(subcommand)]
    Admin(commands::admin::AdminCommand),
    /// Break-glass ICE operator surface (simulate → commit).
    #[command(subcommand)]
    Ice(commands::ice::IceCommand),
    /// `MeshOsSnapshot` reads (one-shot).
    #[command(subcommand)]
    Snapshot(commands::snapshot::SnapshotCommand),
    /// Read-only operator-audit queries.
    #[command(subcommand)]
    Audit(commands::audit::AuditCommand),
    /// Substrate log stream.
    #[command(subcommand)]
    Log(LogCommand),
    /// Substrate failure stream.
    #[command(subcommand)]
    Failures(FailuresCommand),
    /// Capability advertisement + discovery.
    #[command(subcommand)]
    Cap(commands::cap::CapCommand),
    /// Peer + NAT-traversal helpers.
    #[command(subcommand)]
    Peer(PeerCommand),
    /// Per-daemon listing.
    #[command(subcommand)]
    Daemon(DaemonCommand),
    /// NetDB local KV adapters (Cortex-backed tasks + memories).
    #[command(subcommand)]
    Netdb(commands::netdb::NetdbCommand),
    // `net db` (MeshDB federated query plane) ships once the SDK
    // exposes a `MeshOsRuntime::chain_reader()` accessor — see
    // `commands/db.rs` for the design stub and
    // `NET_CLI_PLAN.md §8`. `net port (gateway|probe-peer|
    // try-map)` waits on the same mesh-adapter access.
}

#[derive(Subcommand, Debug)]
enum LogCommand {
    /// Tail the log stream.
    Tail(commands::logs::LogTailArgs),
}

#[derive(Subcommand, Debug)]
enum FailuresCommand {
    /// Tail the failure stream.
    Tail(commands::logs::FailuresTailArgs),
}

#[derive(Subcommand, Debug)]
enum PeerCommand {
    /// List peers known to the local snapshot.
    Ls(commands::peer::LsArgs),
    // reflex / nat / reclassify-nat / set-reflex / clear-reflex
    // land once the SDK exposes a mesh adapter accessor on the
    // runtime — followup within Phase 1.
}

#[derive(Subcommand, Debug)]
enum DaemonCommand {
    /// List daemons known to the local snapshot.
    Ls(commands::daemon::LsArgs),
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> ExitCode {
    // Top-level panic / error formatting. Stays minimal — typed
    // errors carry their own kind discriminator through `CliError`.
    if let Err(e) = color_eyre::install() {
        eprintln!("net: failed to install error reporter: {e}");
        return ExitCode::from(1);
    }

    let cli = Cli::parse();
    install_tracing(cli.verbose, cli.quiet);

    match dispatch(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // Print the kind + message to stderr; the exit code
            // carries the discriminator for scripting consumers.
            eprintln!("net: {}", e);
            ExitCode::from(e.code() as u8)
        }
    }
}

async fn dispatch(cli: Cli) -> Result<(), CliError> {
    let output = cli.output;
    let config_path = cli.config.as_deref();
    let profile = cli.profile.as_str();
    match cli.command {
        Command::Version => commands::version::run().await,
        Command::Identity(cmd) => commands::identity::run(cmd, output).await,
        Command::Admin(cmd) => commands::admin::run(cmd, output, config_path, profile).await,
        Command::Ice(cmd) => commands::ice::run(cmd, output, config_path, profile).await,
        Command::Snapshot(cmd) => commands::snapshot::run(cmd, output, config_path, profile).await,
        Command::Audit(cmd) => commands::audit::run(cmd, output, config_path, profile).await,
        Command::Log(LogCommand::Tail(args)) => {
            commands::logs::run_log_tail(args, output, config_path, profile).await
        }
        Command::Failures(FailuresCommand::Tail(args)) => {
            commands::logs::run_failures_tail(args, output, config_path, profile).await
        }
        Command::Cap(cmd) => commands::cap::run(cmd, output, config_path, profile).await,
        Command::Peer(PeerCommand::Ls(args)) => {
            commands::peer::run_ls(args, output, config_path, profile).await
        }
        Command::Daemon(DaemonCommand::Ls(args)) => {
            commands::daemon::run_ls(args, output, config_path, profile).await
        }
        Command::Netdb(cmd) => commands::netdb::run(cmd, output).await,
    }
}

/// Wire tracing-subscriber to the `-v` count. `-q` short-circuits
/// to `error` level so the binary stays silent on the diagnostic
/// channel; explicit `-v` always overrides.
fn install_tracing(verbose: u8, quiet: bool) {
    use tracing_subscriber::{fmt, EnvFilter};

    let level = if quiet && verbose == 0 {
        "error"
    } else {
        match verbose {
            0 => "warn",
            1 => "info",
            2 => "debug",
            _ => "trace",
        }
    };

    let filter = EnvFilter::try_from_env("NET_LOG")
        .unwrap_or_else(|_| EnvFilter::new(format!("net={level},net_sdk={level}")));

    let _ = fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .compact()
        .try_init();
}

// Self-contained duration parser for the `--timeout` /
// `--drain-for` / `--ttl` flags. Mirrors the small subset of the
// `humantime` crate's syntax that operator-facing flags need;
// the real `humantime` crate is intentionally not in the dep
// list (the parser is ~50 lines and avoids an extra build edge
// for one value-parser shim).
mod humantime {
    use std::time::Duration;

    /// Parse a human-readable duration string (`30s`, `2m`,
    /// `500ms`, `1h30m`). Mirrors the small subset the CLI's
    /// `--timeout` / `--drain-for` / `--ttl` flags accept.
    pub(crate) fn parse_duration(s: &str) -> Result<Duration, String> {
        let s = s.trim();
        if s.is_empty() {
            return Err("empty duration".into());
        }
        // Support a comma-separated `1h30m` style by splitting on
        // unit boundaries.
        let mut total = Duration::ZERO;
        let mut digits = String::new();
        let mut units = String::new();
        for c in s.chars() {
            if c.is_ascii_digit() {
                if !units.is_empty() {
                    total += apply_unit(&digits, &units)?;
                    digits.clear();
                    units.clear();
                }
                digits.push(c);
            } else if c.is_alphabetic() {
                units.push(c);
            } else if c.is_whitespace() {
                // tolerate spaces
            } else {
                return Err(format!("invalid character {c:?} in duration"));
            }
        }
        if digits.is_empty() {
            return Err("missing numeric component".into());
        }
        if units.is_empty() {
            // Bare integer → seconds.
            units.push('s');
        }
        total += apply_unit(&digits, &units)?;
        Ok(total)
    }

    fn apply_unit(digits: &str, unit: &str) -> Result<Duration, String> {
        let value: u64 = digits
            .parse()
            .map_err(|_| format!("invalid numeric value {digits:?}"))?;
        let dur = match unit {
            "ns" => Duration::from_nanos(value),
            "us" | "µs" => Duration::from_micros(value),
            "ms" => Duration::from_millis(value),
            "s" | "sec" | "secs" => Duration::from_secs(value),
            "m" | "min" | "mins" => Duration::from_secs(value * 60),
            "h" | "hr" | "hrs" => Duration::from_secs(value * 60 * 60),
            "d" | "day" | "days" => Duration::from_secs(value * 60 * 60 * 24),
            other => return Err(format!("unknown duration unit {other:?}")),
        };
        Ok(dur)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn bare_integer_is_seconds() {
            assert_eq!(parse_duration("30").unwrap(), Duration::from_secs(30));
        }

        #[test]
        fn unit_suffixes() {
            assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
            assert_eq!(parse_duration("2m").unwrap(), Duration::from_secs(120));
            assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
        }

        #[test]
        fn composite_units() {
            assert_eq!(
                parse_duration("1h30m").unwrap(),
                Duration::from_secs(3600 + 30 * 60)
            );
        }

        #[test]
        fn rejects_empty_and_garbage() {
            assert!(parse_duration("").is_err());
            assert!(parse_duration("abc").is_err());
            assert!(parse_duration("10x").is_err());
        }
    }
}
