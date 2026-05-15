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
mod tabs;
mod theme;
mod widgets;

use app::App;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;

    let harness = runtime::spawn().await?;
    let deck = harness.deck();

    let terminal = ratatui::init();
    let result = App::new(deck).run(terminal);
    ratatui::restore();

    // Explicit drop so the harness's tear-down runs before
    // the process exits — drops the SDK + samples daemons +
    // backing tokio tasks deterministically.
    drop(harness);
    result
}
