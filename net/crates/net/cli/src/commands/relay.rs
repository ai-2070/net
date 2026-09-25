//! `net-mesh relay serve` — run a blind UDP relay in the foreground.
//!
//! The relay is the fallback path for devices that cannot be reached directly:
//! a device registers from its mesh socket (proving its identity key), joiners
//! bind channels to that registration, and the relay forwards their encrypted
//! datagrams without being able to read them. It never holds a PSK, an issuer
//! key or any mesh credential, and it is not a mesh member.
//!
//! The same port number also serves TCP: enrollment splices, and a tunnel
//! that nodes whose UDP to the relay goes unanswered fall back to (bind port
//! 443 to reach networks that only allow it). The tunnel is plain TCP carrying
//! the same end-to-end ciphertext; it is not claimed to cross proxies.

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
    /// Address to serve on (`IP:port`), UDP and TCP on the same port; must be
    /// reachable by devices and joiners, e.g. `0.0.0.0:443` on a host with a
    /// public address (TCP/443 is the fallback for networks that block UDP).
    #[arg(long, value_name = "ADDR")]
    pub bind: String,

    /// Maximum live device registrations.
    #[arg(long, default_value_t = 100_000)]
    pub max_registrations: usize,

    /// Maximum channels (joiners) per registration.
    #[arg(long, default_value_t = 8_192)]
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
    // Test-only seam (`fixtures` builds): behave as a network that blocks UDP
    // to the relay, so the TCP tunnel fallback runs end to end in a CLI test.
    #[cfg(feature = "fixtures")]
    if std::env::var_os("NET_MESH_FIXTURE_RELAY_BLOCK_UDP").is_some() {
        relay.block_udp_for_test(true);
    }
    emit_stream_row(
        fmt,
        &json!({ "event": "ready", "relay": local.to_string() }),
    )
    .map_err(|e| generic(format!("write readiness: {e}")))?;

    tokio::select! {
        _ = relay.run() => {}
        _ = shutdown_signal() => {}
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
            "registrations_accepted": stats.registrations.load(Ordering::Relaxed),
            "splices": stats.splices_opened.load(Ordering::Relaxed),
            "splice_bytes": stats.splice_bytes.load(Ordering::Relaxed),
            "tunnels": stats.tunnels_opened.load(Ordering::Relaxed),
        }),
    )
    .map_err(|e| generic(format!("write stop event: {e}")))
}

/// Ctrl-C, or SIGTERM on Unix (how service managers stop a relay).
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}
