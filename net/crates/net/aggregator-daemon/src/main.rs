//! `net-aggregator-daemon` binary entry point. The
//! implementation lives in the sibling library
//! (`net_aggregator_daemon`) so integration tests can drive
//! `boot` in-process without spawning subprocesses.

use clap::Parser;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> std::process::ExitCode {
    let cli = net_aggregator_daemon::Cli::parse();
    net_aggregator_daemon::init_tracing(cli.verbose);

    match net_aggregator_daemon::run(cli).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "aggregator daemon failed");
            std::process::ExitCode::FAILURE
        }
    }
}
