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
//! What it proves: the crate builds, a Net node starts under tokio, you can
//! emit a typed event, a subscriber receives it, and shutdown is clean.

use futures::StreamExt;
use net_sdk::Net;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
struct Hello {
    msg: String,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> net_sdk::error::Result<()> {
    let node = Net::builder().shards(1).memory().build().await?;

    // Subscribe FIRST. The stream is hot.
    let mut stream = node.subscribe_typed::<Hello>(Default::default());

    // Spawn the subscribe loop so we can emit on the main task.
    let recv_task = tokio::spawn(async move {
        stream.next().await.map(|r| r.expect("stream item"))
    });

    // Small tick so the subscriber's first poll lands.
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    node.emit(&Hello { msg: "hello, mesh".into() })?;

    let received = recv_task.await.unwrap().expect("no event received");
    println!("received: {:?}", received);

    node.shutdown().await?;
    Ok(())
}
