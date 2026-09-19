//! The sidecar you no longer run: call a service, not a host.
//!
//! Two providers serve the same service name. A caller addresses the **service**
//! — never a node id — and when the provider that answered first goes away, the
//! next call lands on the survivor, with the retry helper absorbing the
//! transient while the dead provider is still in the roster.
//!
//! This is the service-mesh shape collapsed into the bus: no sidecar to
//! configure, no separate load balancer, no certificate rotation, and no
//! endpoint list to keep in sync. The roster *is* the capability fold.
//!
//! Run (from a crate whose `examples/` holds this file):
//!
//!   cargo run --example failover
//!
//! Expected final line: `RESULT ok providers=2 moved=1 served=2`

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::mesh_rpc::{CallOptionsTyped, Codec};
use net_sdk::mesh_rpc_resilience::RetryPolicy;
use net_sdk::Identity;
use serde::{Deserialize, Serialize};

/// 32 bytes exactly — a PSK, not a passphrase. Every node in a mesh shares it.
const PSK: [u8; 32] = [0x42; 32];

#[derive(Debug, Serialize, Deserialize)]
struct Work {
    units: u32,
}

#[derive(Debug, Serialize, Deserialize)]
struct Answer {
    /// Which provider ran it. The caller never asked for one by id.
    served_by: u64,
    units: u32,
}

async fn build(seed_byte: u8) -> Mesh {
    MeshBuilder::new("127.0.0.1:0", &PSK)
        .expect("builder")
        .identity(Identity::from_seed([seed_byte; 32]))
        .build()
        .await
        .expect("build mesh node")
}

async fn handshake(responder: &Mesh, initiator: &Mesh, responder_addr: SocketAddr) {
    let responder_pub = *responder.inner().public_key();
    let responder_id = responder.inner().node_id();
    let initiator_id = initiator.inner().node_id();
    let (accepted, connected) = tokio::join!(
        responder.inner().accept(initiator_id),
        async {
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

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let caller = build(0xC1).await;
    let provider_one = build(0xC2).await;
    let provider_two = build(0xC3).await;

    let caller_addr = caller.inner().local_addr();
    handshake(&caller, &provider_one, caller_addr).await;
    handshake(&caller, &provider_two, caller_addr).await;
    caller.start();
    provider_one.start();
    provider_two.start();

    // Two servers, one service name. `serve_rpc_typed` advertises `nrpc:work`
    // for each, which is the whole registration.
    let one_id = provider_one.inner().node_id();
    let two_id = provider_two.inner().node_id();
    let _serve_one = provider_one
        .serve_rpc_typed("work", Codec::Json, move |req: Work| async move {
            Ok(Answer {
                served_by: one_id,
                units: req.units,
            })
        })?;
    let _serve_two = provider_two
        .serve_rpc_typed("work", Codec::Json, move |req: Work| async move {
            Ok(Answer {
                served_by: two_id,
                units: req.units,
            })
        })?;

    // The caller discovers the service by name. It learns *who can serve it*,
    // never whom to prefer — that is what makes the next part work.
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if caller.find_service_nodes("work").len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let providers = caller.find_service_nodes("work").len();
    println!("providers advertising `work`: {providers}");

    // Call the service, not a host.
    let opts = CallOptionsTyped::default();
    let first: Answer = caller.call_service_typed("work", &Work { units: 2 }, opts.clone()).await?;
    println!("first call served by:  {:#x}", first.served_by);

    // Take that provider out of the mesh entirely — the hard version of a
    // deploy: not a drain, a death.
    if first.served_by == one_id {
        provider_one.shutdown().await?;
    } else {
        provider_two.shutdown().await?;
    }
    println!("took {:#x} out", first.served_by);

    // The roster still lists the dead provider until the capability fold
    // converges, so the first attempt may be spent on it. The retry helper is
    // what makes that a transient rather than a caller-visible failure.
    let policy = RetryPolicy {
        max_attempts: 6,
        initial_backoff: Duration::from_millis(100),
        max_backoff: Duration::from_millis(500),
        ..RetryPolicy::default()
    };
    let second: Answer = caller
        .call_service_typed_with_retry("work", &Work { units: 5 }, opts, &policy)
        .await?;
    println!("after the death served by: {:#x}", second.served_by);

    let moved = usize::from(second.served_by != first.served_by);
    let served = usize::from(first.units == 2) + usize::from(second.units == 5);
    println!(
        "RESULT ok providers={providers} moved={moved} served={served}"
    );

    caller.shutdown().await?;
    Ok(())
}