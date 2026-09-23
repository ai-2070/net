//! `net-mesh relay serve` — run a blind UDP relay in the foreground.
//!
//! The relay is the fallback path for devices that cannot be reached directly:
//! a device registers from its mesh socket (proving its identity key), joiners
//! bind channels to that registration, and the relay forwards their encrypted
//! datagrams without being able to read them. It never holds a PSK, an issuer
//! key or any mesh credential, and it is not a mesh member.

use std::sync::atomic::Ordering;

use clap::{Args, Subcommand};
use net::adapter::net::traversal::blind_relay::{BlindRelay, RelayConfig};
use serde_json::json;

use crate::error::{generic, CliError};
use crate::prelude::{emit_stream_row, OutputFormat};

/// `net-mesh relay ...`
#[derive(Subcommand, Debug)]
pub enum RelayCommand {
    /// Run a blind UDP relay in the foreground until Ctrl-C.
    Serve(ServeArgs),
}

/// `net-mesh relay serve` arguments.
#[derive(Args, Debug)]
pub struct ServeArgs {
    /// UDP address to serve on (`IP:port`); must be reachable by devices and
    /// joiners, e.g. `0.0.0.0:<port>` on a host with a public address.
    #[arg(long, value_name = "ADDR")]
    pub bind: String,

    /// Maximum live device registrations.
    #[arg(long, default_value_t = 100_000)]
    pub max_registrations: usize,

    /// Maximum channels (joiners) per registration.
    #[arg(long, default_value_t = 64)]
    pub max_channels_per_registration: usize,
}

pub async fn run(cmd: RelayCommand, output: Option<OutputFormat>) -> Result<(), CliError> {
    match cmd {
        RelayCommand::Serve(args) => serve(args, output).await,
    }
}

async fn serve(args: ServeArgs, output: Option<OutputFormat>) -> Result<(), CliError> {
    let bind = crate::context::parse_bind_literal(&args.bind)?;
    let fmt = OutputFormat::resolve_stream(output);
    let config = RelayConfig {
        max_registrations: args.max_registrations,
        max_channels_per_registration: args.max_channels_per_registration,
        ..RelayConfig::default()
    };
    let relay = BlindRelay::bind(bind, config)
        .await
        .map_err(|e| crate::error::connection_failure(format!("relay bind {bind}: {e}")))?;
    let local = relay
        .local_addr()
        .map_err(|e| generic(format!("relay socket: {e}")))?;
    let core = relay.core().clone();
    emit_stream_row(
        fmt,
        &json!({ "event": "ready", "relay": local.to_string() }),
    )
    .map_err(|e| generic(format!("write readiness: {e}")))?;

    tokio::select! {
        _ = relay.run() => {}
        _ = tokio::signal::ctrl_c() => {}
    }

    let (registrations, channels) = core.sizes();
    let stats = core.stats();
    emit_stream_row(
        fmt,
        &json!({
            "event": "stopped",
            "relay": local.to_string(),
            "registrations": registrations,
            "channels": channels,
            "forwarded_packets": stats.forwarded_packets.load(Ordering::Relaxed),
            "forwarded_bytes": stats.forwarded_bytes.load(Ordering::Relaxed),
            "dropped": stats.dropped.load(Ordering::Relaxed),
            "refused": stats.refused.load(Ordering::Relaxed),
        }),
    )
    .map_err(|e| generic(format!("write stop event: {e}")))
}
