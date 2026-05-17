//! `net audit (recent|stream)` — read the admin-audit ring.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use futures::StreamExt;

use crate::context::{resolve_profile, CliContext};
use crate::error::{generic, sdk, CliError};
use crate::prelude::{emit_stream_row, emit_value, OutputFormat};

#[derive(Subcommand, Debug)]
pub enum AuditCommand {
    /// One-shot read: newest-first list of audit records.
    Recent(RecentArgs),
    /// Tail mode: emit each new audit record as ndjson.
    Stream(StreamArgs),
}

#[derive(Args, Debug)]
pub struct RecentArgs {
    /// Maximum number of records to return.
    #[arg(short = 'n', long, default_value_t = 100)]
    pub limit: usize,

    /// Filter to records signed by this operator id.
    #[arg(long)]
    pub by_operator: Option<u64>,

    /// Filter to records committed inside [start_ms, end_ms].
    #[arg(long, requires = "end_ms")]
    pub start_ms: Option<u64>,
    #[arg(long, requires = "start_ms")]
    pub end_ms: Option<u64>,

    /// Only ICE force-operations.
    #[arg(long)]
    pub force_only: bool,

    /// Watermark — return only records with seq > this value.
    #[arg(long)]
    pub since: Option<u64>,

    #[arg(long)]
    pub identity: Option<PathBuf>,

    #[arg(long, default_value_t = 0x0001)]
    pub node: u64,
}

#[derive(Args, Debug)]
pub struct StreamArgs {
    #[arg(long)]
    pub by_operator: Option<u64>,

    #[arg(long)]
    pub force_only: bool,

    #[arg(long)]
    pub since: Option<u64>,

    #[arg(long)]
    pub identity: Option<PathBuf>,

    #[arg(long, default_value_t = 0x0001)]
    pub node: u64,
}

pub async fn run(
    cmd: AuditCommand,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    match cmd {
        AuditCommand::Recent(args) => run_recent(args, output, config_path, profile_name).await,
        AuditCommand::Stream(args) => run_stream(args, output, config_path, profile_name).await,
    }
}

async fn run_recent(
    args: RecentArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    let profile = resolve_profile(config_path, profile_name).await?;
    let ctx = CliContext::build(&profile, args.identity.as_deref(), args.node).await?;

    let deck = ctx.deck();
    let mut query = deck.audit().recent(args.limit);
    if let Some(op) = args.by_operator {
        query = query.by_operator(op);
    }
    if let (Some(start), Some(end)) = (args.start_ms, args.end_ms) {
        query = query.between(start, end);
    }
    if args.force_only {
        query = query.force_only();
    }
    if let Some(seq) = args.since {
        query = query.since(seq);
    }
    let records = query.collect();
    emit_value(OutputFormat::resolve_oneshot(output), &records)
        .map_err(|e| generic(format!("write audit: {e}")))?;
    Ok(())
}

async fn run_stream(
    args: StreamArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    let profile = resolve_profile(config_path, profile_name).await?;
    let ctx = CliContext::build(&profile, args.identity.as_deref(), args.node).await?;

    let deck = ctx.deck();
    let mut query = deck.audit();
    if let Some(op) = args.by_operator {
        query = query.by_operator(op);
    }
    if args.force_only {
        query = query.force_only();
    }
    if let Some(seq) = args.since {
        query = query.since(seq);
    }
    let mut stream = query.stream();
    let fmt = OutputFormat::resolve_stream(output);

    // Ctrl-C cancels cleanly without aborting mid-record.
    let mut ctrl_c = std::pin::pin!(tokio::signal::ctrl_c());

    loop {
        tokio::select! {
            _ = ctrl_c.as_mut() => {
                tracing::info!("audit stream cancelled by Ctrl-C");
                return Ok(());
            }
            row = stream.next() => {
                match row {
                    Some(Ok(record)) => {
                        emit_stream_row(fmt, &record)
                            .map_err(|e| generic(format!("write audit row: {e}")))?;
                    }
                    Some(Err(e)) => {
                        return Err(sdk(format!("audit stream error: {e}")));
                    }
                    None => return Ok(()),
                }
            }
        }
    }
}
