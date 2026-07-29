//! What the drop counters actually tell you (Rust).
//!
//! Drop this into your crate's `examples/` directory and run:
//!   cargo run --example observe
//!
//! Cargo.toml: same dependencies as `hello.rs`.
//!
//! THE POINT OF THIS FILE, AND IT IS NOT WHAT YOU EXPECT.
//!
//! The usual advice — "backpressure is silent, so watch `events_dropped`" — is
//! wrong for the one mode where it matters most, and that mode is the default.
//!
//! Both counters sit at the **producer boundary**: they record what the bus
//! accepted or refused from you, not what survived to an adapter.
//!
//!   - `DropOldest` (the default) evicts to make room, so the producer always
//!     succeeds and `events_dropped` never moves. Events are lost and nothing
//!     in-process says so.
//!   - `DropNewest` and `FailProducer` refuse the producer instead, so the
//!     `emit` call *and* `events_dropped` both see it.
//!
//! The exact counts vary between runs and between bindings: the drain worker
//! races the producer, so how full the buffer gets is a timing question. The
//! shape is what matters, not the numbers.

use net_sdk::{Backpressure, Net};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct Tick {
    seq: u64,
}

/// Push far past capacity with nothing consuming, and report the counters.
async fn measure(label: &str, mode: Backpressure) -> net_sdk::error::Result<()> {
    let node = Net::builder()
        .shards(1)
        .buffer_capacity(1024) // power of two, and >= 1024 — both enforced
        .backpressure(mode)
        .memory()
        .build()
        .await?;

    let mut refused = 0u32;
    for seq in 0..20_000 {
        if node.emit(&Tick { seq }).is_err() {
            refused += 1;
        }
    }

    let s = node.stats();
    println!(
        "{label:<13} ingested={:<6} dropped={:<6} emit_errors={}",
        s.events_ingested, s.events_dropped, refused
    );

    node.shutdown().await
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> net_sdk::error::Result<()> {
    println!("20,000 events into a 1024-slot buffer, nothing consuming:\n");
    measure("DropOldest", Backpressure::DropOldest).await?;
    measure("DropNewest", Backpressure::DropNewest).await?;
    measure("FailProducer", Backpressure::FailProducer).await?;
    println!("\nDropOldest loses events silently — and it is the default.");
    println!("The other two refuse the producer, so emit() and the counter agree.");
    Ok(())
}
