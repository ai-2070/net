//! The service registry you no longer run.
//!
//! Four in-process mesh nodes over loopback UDP: two providers announce the
//! same capability, a caller discovers them and ranks them locally, a third
//! provider appears, and the caller's next lookup ranks it first — with no
//! registry process, no health-check poller, no config reload and no
//! announcement to any address.
//!
//! Run (from a crate whose `examples/` holds this file):
//!
//!   cargo run --example registry
//!
//! Expected final line: `RESULT ok providers=3 joined=1 best_moved=1`

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use net_sdk::capabilities::{
    CapabilityFilter, CapabilityRequirement, CapabilitySet, HardwareCapabilities,
};
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::Identity;

/// 32 bytes exactly — a PSK, not a passphrase. Every node in a mesh shares it.
const PSK: [u8; 32] = [0x42; 32];

/// How long we wait for an announcement to reach the caller's local fold.
const CONVERGE: Duration = Duration::from_secs(5);

async fn build(seed_byte: u8) -> Mesh {
    MeshBuilder::new("127.0.0.1:0", &PSK)
        .expect("builder")
        .identity(Identity::from_seed([seed_byte; 32]))
        .build()
        .await
        .expect("build mesh node")
}

/// One side connects, the other accepts. Symmetric double-connect does not work.
/// Both futures resolve only after the Noise handshake completes, so joining
/// them *is* the wait-until-connected primitive.
async fn handshake(responder: &Mesh, initiator: &Mesh, responder_addr: SocketAddr) {
    let responder_pub = *responder.inner().public_key();
    let responder_id = responder.inner().node_id();
    let initiator_id = initiator.inner().node_id();
    let (accepted, connected) = tokio::join!(
        responder.inner().accept(initiator_id),
        async {
            // Absorbs the accept/connect race; both sides retry with backoff.
            tokio::time::sleep(Duration::from_millis(50)).await;
            initiator
                .inner()
                .connect(responder_addr, &responder_pub, responder_id)
                .await
        }
    );
    accepted.expect("accept");
    connected.expect("connect");
}

/// Poll the caller's local capability fold until `pred` holds, or give up.
async fn until<F: FnMut() -> bool>(what: &str, mut pred: F) -> bool {
    let deadline = Instant::now() + CONVERGE;
    while Instant::now() < deadline {
        if pred() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    eprintln!("timed out waiting for {what}");
    false
}

fn api_filter() -> CapabilityFilter {
    CapabilityFilter::new().require_tag("api")
}

/// A provider = the service tag plus the capacity the caller's ranking reads.
fn provider(memory_gb: u32) -> CapabilitySet {
    CapabilitySet::new()
        .with_hardware(HardwareCapabilities::new().with_memory(memory_gb))
        .add_tag("api")
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Announcements reach directly-connected peers, so the caller connects to
    // every provider it wants to see. There is no directory to join.
    let a = build(0xA1).await; // provider, 16 GB
    let b = build(0xB2).await; // provider, 64 GB
    let c = build(0xC3).await; // provider that appears later, 256 GB
    let caller = build(0xD4).await;

    let addr_a = a.inner().local_addr();
    let addr_b = b.inner().local_addr();
    let addr_caller = caller.inner().local_addr();

    // Every accept() completes before any start() — the dispatch loop races the
    // responder handshake otherwise.
    handshake(&b, &a, addr_b).await; // A <-> B
    handshake(&a, &caller, addr_a).await; // A <-> caller
    handshake(&caller, &b, addr_caller).await; // B <-> caller
    handshake(&caller, &c, addr_caller).await; // C <-> caller

    a.inner().start();
    b.inner().start();
    c.inner().start();
    caller.inner().start();

    // The two original providers announce. That is the entire registration.
    a.announce_capabilities(provider(16)).await?;
    b.announce_capabilities(provider(64)).await?;

    // The caller reads its own fold — there is no registry to query, and the
    // query never leaves this process.
    let mut seen = a.find_nodes(&api_filter());
    if !until("both providers to appear", || {
        seen = caller.find_nodes(&api_filter());
        seen.len() == 2
    })
    .await
    {
        return Err("providers never converged".into());
    }
    let providers = seen.len();

    // Ranking is local and free. Prefer more memory, so the bigger machine wins
    // without any scheduler deciding it centrally.
    let mut req = CapabilityRequirement::from_filter(api_filter());
    req.prefer_more_memory = 1.0;
    let first = caller
        .find_best_node(&req)
        .ok_or("no provider matched")?;

    // A new provider appears. It connects, announces once, and is immediately
    // addressable — no registry entry, no service discovery config, no reload.
    c.announce_capabilities(provider(256)).await?;

    if !until("the new provider to appear", || {
        caller.find_nodes(&api_filter()).len() == 3
    })
    .await
    {
        return Err("the new provider never converged".into());
    }
    let after = caller.find_nodes(&api_filter()).len();
    let best = caller
        .find_best_node(&req)
        .ok_or("no provider matched after the join")?;

    let moved = usize::from(first != best);
    println!("providers found at first lookup: {providers}");
    println!("ranked best:                     {first:#x}");
    println!("providers after the join:        {after}");
    println!("ranked best:                     {best:#x}");

    println!("RESULT ok providers={after} joined=1 best_moved={moved}");

    a.shutdown().await?;
    b.shutdown().await?;
    c.shutdown().await?;
    caller.shutdown().await?;
    Ok(())
}
