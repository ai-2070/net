//! `net-aggregator-daemon` binary entry point. The
//! implementation lives in the sibling library
//! (`net_aggregator_daemon`) so integration tests can drive
//! `boot` in-process without spawning subprocesses.

use clap::Parser;

// Long-running daemon: replace the system allocator with mimalloc.
// macOS libmalloc's nano-zone (<=256B) boundary penalises the 256-512B
// per-packet/per-event allocations this process makes on its hot paths;
// mimalloc removes that cliff (~-69% on the 256B frame-write path) and
// is 2-3x faster on small allocations generally. The library crate
// stays allocator-neutral; the choice belongs to the binary.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

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
