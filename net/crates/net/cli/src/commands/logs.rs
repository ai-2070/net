//! `net log tail` and `net failures tail` — substrate log/failure
//! streams.

use std::path::PathBuf;

use clap::Args;
use futures::StreamExt;
use net_sdk::deck::LogFilter;
use net_sdk::meshos::LogLevel as CoreLogLevel;

use crate::context::{resolve_profile, CliContext};
use crate::error::{generic, invalid_args, sdk, CliError};
use crate::prelude::{emit_stream_row, OutputFormat};

// =========================================================================
// log tail
// =========================================================================

#[derive(Args, Debug)]
pub struct LogTailArgs {
    /// Follow the stream (default). The flag is accepted for
    /// symmetry with `tail -f`; `net log tail` always tails.
    #[arg(short = 'f', long)]
    pub follow: bool,

    /// Minimum severity. One of `trace|debug|info|warn|error`.
    #[arg(long)]
    pub min_level: Option<String>,

    /// Restrict to records originating from this daemon.
    #[arg(long)]
    pub daemon: Option<u64>,

    /// Restrict to records originating from this node.
    #[arg(long)]
    pub node_filter: Option<u64>,

    /// Watermark — emit only records with seq > this value.
    #[arg(long)]
    pub since: Option<u64>,

    #[arg(long)]
    pub identity: Option<PathBuf>,

    #[arg(long, default_value_t = 0x0001)]
    pub node: u64,
}

pub async fn run_log_tail(
    args: LogTailArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    let _ = args.follow; // accepted but always-on
    // Validate the filter up-front so an invalid `--min-level`
    // exits before we pay the substrate-startup cost. The other
    // filter knobs (daemon / node / since) are typed by clap.
    let min_level = match args.min_level.as_deref() {
        Some(s) => Some(parse_log_level(s)?),
        None => None,
    };

    let profile = resolve_profile(config_path, profile_name).await?;
    let ctx = CliContext::build(&profile, args.identity.as_deref(), args.node, false).await?;

    let mut filter = LogFilter::new();
    if let Some(level) = min_level {
        filter = filter.min_level(level);
    }
    if let Some(d) = args.daemon {
        filter = filter.with_daemon(d);
    }
    if let Some(n) = args.node_filter {
        filter = filter.with_node(n);
    }
    if let Some(seq) = args.since {
        filter = filter.since(seq);
    }

    let mut stream = ctx.deck().subscribe_logs(filter);
    let fmt = OutputFormat::resolve_stream(output);
    let mut ctrl_c = std::pin::pin!(tokio::signal::ctrl_c());

    loop {
        tokio::select! {
            _ = ctrl_c.as_mut() => {
                tracing::info!("log tail cancelled by Ctrl-C");
                return Ok(());
            }
            row = stream.next() => {
                match row {
                    Some(Ok(record)) => emit_stream_row(fmt, &record)
                        .map_err(|e| generic(format!("write log row: {e}")))?,
                    Some(Err(e)) => return Err(sdk(format!("log stream error: {e}"))),
                    None => return Ok(()),
                }
            }
        }
    }
}

fn parse_log_level(s: &str) -> Result<CoreLogLevel, CliError> {
    Ok(match s.to_lowercase().as_str() {
        "trace" => CoreLogLevel::Trace,
        "debug" => CoreLogLevel::Debug,
        "info" => CoreLogLevel::Info,
        "warn" | "warning" => CoreLogLevel::Warn,
        "error" => CoreLogLevel::Error,
        other => {
            return Err(invalid_args(format!(
                "log level must be one of trace|debug|info|warn|error; got {other:?}"
            )));
        }
    })
}

// =========================================================================
// failures tail
// =========================================================================

#[derive(Args, Debug)]
pub struct FailuresTailArgs {
    /// Watermark — emit only records with seq > this value.
    #[arg(long, default_value_t = 0)]
    pub since_seq: u64,

    #[arg(long)]
    pub identity: Option<PathBuf>,

    #[arg(long, default_value_t = 0x0001)]
    pub node: u64,
}

pub async fn run_failures_tail(
    args: FailuresTailArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    let profile = resolve_profile(config_path, profile_name).await?;
    let ctx = CliContext::build(&profile, args.identity.as_deref(), args.node, false).await?;

    let mut stream = ctx.deck().subscribe_failures(args.since_seq);
    let fmt = OutputFormat::resolve_stream(output);
    let mut ctrl_c = std::pin::pin!(tokio::signal::ctrl_c());

    loop {
        tokio::select! {
            _ = ctrl_c.as_mut() => {
                tracing::info!("failures tail cancelled by Ctrl-C");
                return Ok(());
            }
            row = stream.next() => {
                match row {
                    Some(Ok(record)) => emit_stream_row(fmt, &record)
                        .map_err(|e| generic(format!("write failure row: {e}")))?,
                    Some(Err(e)) => return Err(sdk(format!("failure stream error: {e}"))),
                    None => return Ok(()),
                }
            }
        }
    }
}
