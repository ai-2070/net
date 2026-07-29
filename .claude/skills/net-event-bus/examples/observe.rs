//! Observe a drop, and handle a producer failure (Rust).
//!
//! Drop this into your crate's `examples/` directory and run:
//!   cargo run --example observe
//!
//! Cargo.toml: same dependencies as `hello.rs`.
//!
//! What it proves: `stats()` reports drops, and under `FailProducer` the
//! producer sees a structured `SdkError` rather than silence.
//!
//! WHY THIS EXISTS. Under the default `DropOldest` / `DropNewest` modes,
//! backpressure is *silent* — nothing throws, nothing returns false, and the
//! only evidence is `events_dropped`. If you take one thing from this file:
//! alert on that counter, because nothing else will tell you.

use net_sdk::{Backpressure, Net};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
struct Tick {
    seq: u64,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> net_sdk::error::Result<()> {
    // --- Silent drops: the default posture -------------------------------
    let node = Net::builder()
        .shards(1)
        .buffer_capacity(64)
        .backpressure(Backpressure::DropOldest)
        .memory()
        .build()
        .await?;

    // Far more events than the buffer holds, with nothing consuming them.
    for seq in 0..1_000 {
        // Note the `?`: this returns Ok even while events are being dropped.
        node.emit(&Tick { seq })?;
    }

    let s = node.stats();
    println!(
        "DropOldest: ingested={} dropped={} batches={}",
        s.events_ingested, s.events_dropped, s.batches_dispatched
    );
    node.shutdown().await?;

    // --- FailProducer: the one mode that tells you -----------------------
    let strict = Net::builder()
        .shards(1)
        .buffer_capacity(64)
        .backpressure(Backpressure::FailProducer)
        .memory()
        .build()
        .await?;

    let mut refused = 0u32;
    for seq in 0..1_000 {
        if let Err(e) = strict.emit(&Tick { seq }) {
            // Structured, not a string: match the variant. `SdkError` is
            // #[non_exhaustive], so keep a catch-all arm.
            refused += 1;
            if refused == 1 {
                println!("FailProducer: first refusal -> {e:?}");
            }
        }
    }
    println!("FailProducer: refused {refused} of 1000");

    strict.shutdown().await?;
    Ok(())
}
