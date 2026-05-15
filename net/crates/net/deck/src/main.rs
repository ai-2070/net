//! Deck — operator cyberdeck.
//!
//! ratatui + crossterm. Five tabs: NET.MAP / LIST / DATAFORTS /
//! DAEMON / LOGS. Matrix palette pulled from the AI 2070 Net
//! site aesthetic (neon-green on pitch black).
//!
//! Build modes:
//! - default: live in-process `MeshOsRuntime`, no sample
//!   data. Every tab reads from the snapshot — empty until
//!   real cluster sources are wired.
//! - `--features samples`: adds a static fixture of 17 fake
//!   peers + 11 daemons across all four lineage groups so
//!   the deck has something concrete to monitor. No event
//!   seeders — the deck observes whatever steady state the
//!   runtime + supervisor produce on their own.

mod app;
mod lineage;
mod nodes;
mod runtime;
mod streams;
mod tabs;
mod theme;
mod widgets;

use app::App;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;

    let harness = runtime::spawn().await?;
    let deck = harness.deck();
    let blob_metrics = harness.blob_metrics();

    // Phase 4: spawn streaming tails before handing the deck to
    // the App. The handles are kept alive for the App's
    // lifetime; dropping them on shutdown lets the substrate's
    // streams close cleanly.
    let logs_tail = streams::LogsTail::new(streams::LOGS_TAIL_CAP);
    let _logs_stream_task = streams::spawn_logs_stream(deck.clone(), logs_tail.clone());
    let audit_tail = streams::AuditTail::new(streams::AUDIT_TAIL_CAP);
    let _audit_stream_task = streams::spawn_audit_stream(deck.clone(), audit_tail.clone());
    let failures_tail = streams::FailuresTail::new(streams::FAILURES_TAIL_CAP);
    let _failures_stream_task =
        streams::spawn_failures_stream(deck.clone(), failures_tail.clone());

    let terminal = ratatui::init();
    let result =
        App::new(deck, logs_tail, audit_tail, failures_tail, blob_metrics).run(terminal);
    ratatui::restore();

    // Explicit drop so the harness's tear-down runs before
    // the process exits — drops the SDK + samples daemons +
    // backing tokio tasks deterministically.
    drop(harness);
    result
}
