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
mod bookmarks;
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

    // Restore the terminal on panic. color_eyre installs a
    // report hook but doesn't undo raw mode / alternate
    // screen; without this hook a panic inside the App's run
    // loop leaves the operator's terminal scrambled.
    let prev_panic_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        ratatui::restore();
        prev_panic_hook(info);
    }));

    let harness = runtime::spawn().await?;
    let deck = harness.deck();
    let blob_adapters = harness.blob_adapters();

    // Phase 4: spawn streaming tails before handing the deck to
    // the App. The handles are kept alive for the App's
    // lifetime; dropping them on shutdown lets the substrate's
    // streams close cleanly.
    let logs_tail = streams::LogsTail::new(streams::LOGS_TAIL_CAP);
    let _logs_stream_task = streams::spawn_logs_stream(deck.clone(), logs_tail.clone());
    let audit_tail = streams::AuditTail::new(streams::AUDIT_TAIL_CAP);
    let _audit_stream_task = streams::spawn_audit_stream(deck.clone(), audit_tail.clone());
    let failures_tail = streams::FailuresTail::new(streams::FAILURES_TAIL_CAP);
    let _failures_stream_task = streams::spawn_failures_stream(deck.clone(), failures_tail.clone());

    // BLOBS inventory poller — unions every wired adapter into
    // a single inventory cache, capped at `BLOBS_TAIL_CAP`.
    // Adapter-level errors flow into the App's toast channel.
    let blobs_tail = streams::BlobsTail::new();

    // NRPC tail — populated by the samples-logs injector today;
    // empty without that flag, ready for a real nRPC observer
    // wire-up.
    let nrpc_tail = streams::NrpcTail::new(streams::NRPC_TAIL_CAP);

    // Bookmark store — loaded from `$XDG_CONFIG_HOME/deck/bookmarks.toml`
    // (or the platform equivalent). A first-run with no config
    // directory yields an empty store; a malformed file is
    // surfaced via stderr so the operator notices. The picker
    // UX that consumes this ships with the multi-cluster slice.
    let bookmarks = bookmarks::BookmarkStore::load().unwrap_or_else(|err| {
        eprintln!("[deck] bookmark store: {err} — using empty store");
        bookmarks::BookmarkStore::empty()
    });

    let this_node = harness.this_node();
    let terminal = ratatui::init();
    let app = App::new(
        deck,
        logs_tail,
        audit_tail,
        failures_tail,
        blob_adapters.clone(),
        blobs_tail.clone(),
        nrpc_tail.clone(),
        bookmarks,
        this_node,
    );
    let _blobs_poll_task = if blob_adapters.is_empty() {
        None
    } else {
        Some(streams::spawn_blobs_poll(
            blob_adapters,
            blobs_tail,
            std::time::Duration::from_millis(500),
            app.toast_tx.clone(),
        ))
    };
    // NRPC traffic injector — only fires under `samples-logs`.
    // Pushes synthetic call records into the NRPC tail at a
    // fixed cadence so the NRPC tab demonstrates the call
    // ring offline.
    #[cfg(feature = "samples-logs")]
    let _nrpc_seeder_task = runtime::spawn_nrpc_seeder(nrpc_tail, this_node);
    #[cfg(not(feature = "samples-logs"))]
    drop(nrpc_tail);
    let pending_admin = app.pending_admin_handle();
    let result = app.run(terminal);
    ratatui::restore();

    // Await any in-flight admin / ICE dispatches the operator
    // confirmed during the session — gives the substrate time
    // to commit (and to push a "failed" toast that the channel
    // will still pick up post-app via the receiver hand-off).
    // 2 s per task is the budget; a stuck RPC doesn't block
    // shutdown forever.
    let handles: Vec<_> = pending_admin
        .lock()
        .map(|mut g| std::mem::take(&mut *g))
        .unwrap_or_default();
    for h in handles {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), h).await;
    }

    // Explicit drop so the harness's tear-down runs before
    // the process exits — drops the SDK + samples daemons +
    // backing tokio tasks deterministically.
    drop(harness);
    result
}
