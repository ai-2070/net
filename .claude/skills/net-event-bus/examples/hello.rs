//! Minimal sanity check for net-sdk (Rust).
//!
//! Drop this into your crate's `examples/` directory and run:
//!   cargo run --example hello
//!
//! Cargo.toml:
//!   [dependencies]
//!   net-mesh-sdk = "0.34"
//!   serde = { version = "1", features = ["derive"] }
//!   tokio = { version = "1", features = ["rt", "macros", "time"] }
//!   futures = "0.3"
//!
//! What it proves: the crate builds, a Net node starts under tokio, a typed
//! event is accepted, and shutdown is clean.
//!
//! WHAT IT DELIBERATELY DOES NOT DO: receive the event back.
//!
//! `.memory()` selects the **Noop adapter**. Events flow producer -> ring
//! buffer -> drain worker -> adapter, and the Noop adapter counts batches and
//! discards them (`adapter/noop.rs`: "Just count, don't store"; its
//! `poll_shard` returns `ShardPollResult::empty()`). So on memory transport
//! `subscribe()` never yields and `poll()` always returns zero — a round-trip
//! example here would hang, not fail.
//!
//! To actually receive events you need an adapter that retains them: Redis or
//! JetStream, or the mesh transport between two nodes. See `mesh.md`.

use net_sdk::Net;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
struct Hello {
    msg: String,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> net_sdk::error::Result<()> {
    let node = Net::builder().shards(1).memory().build().await?;

    let receipt = node.emit(&Hello {
        msg: "hello, mesh".into(),
    })?;

    // `events_ingested` counts at the *producer* boundary — it says the bus
    // accepted the event, not that anything received or stored it.
    let stats = node.stats();
    assert_eq!(stats.events_ingested, 1, "the bus did not accept the event");

    println!(
        "accepted: shard={} ingested={}",
        receipt.shard_id, stats.events_ingested
    );

    node.shutdown().await?;
    Ok(())
}
