//! Deck — operator cyberdeck.
//!
//! ratatui + crossterm. Five tabs: NET.MAP / LIST / DATAFORTS /
//! DAEMON / LOGS. Matrix palette pulled from the AI 2070 Net
//! site aesthetic (neon-green on pitch black).
//!
//! Build modes:
//! - default: fixture-only mode. Every tab renders hard-coded
//!   placeholder data. Useful for visual / style work.
//! - `--features demo`: spawns an in-process `MeshOsRuntime` +
//!   four demo daemons + a seeder task that publishes log
//!   lines and signed admin events. Every tab that reads from
//!   `DeckClient::status()` renders live snapshot data.

mod app;
#[cfg(feature = "demo")]
mod demo;
mod nodes;
mod tabs;
mod theme;
mod widgets;

use app::App;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;

    // Demo bootstrap. Without the `demo` feature, the harness
    // doesn't compile in; the app runs in fixture mode.
    #[cfg(feature = "demo")]
    let harness = Some(demo::spawn().await?);
    #[cfg(not(feature = "demo"))]
    let harness: Option<()> = None;

    #[cfg(feature = "demo")]
    let deck = harness.as_ref().map(|h| h.deck());
    #[cfg(not(feature = "demo"))]
    let deck: Option<std::sync::Arc<net_sdk::deck::DeckClient>> = None;

    let terminal = ratatui::init();
    let result = App::new(deck).run(terminal);
    ratatui::restore();

    // Explicit drop so the demo harness's Drop impl fires
    // BEFORE we return from main — aborts the seeder task +
    // shuts the runtime down before the process exits.
    let _ = harness;
    result
}
