//! The object store you no longer run.
//!
//! Two in-process mesh nodes over loopback UDP and a content-addressed blob
//! store: a producer stores bytes and mints an address, a second node fetches
//! them by that address, and storing the same bytes again produces the same
//! address. There is no bucket to create, no region to pick and no replication
//! factor to configure — the address *is* the data.
//!
//! Run (from a crate whose `examples/` holds this file):
//!
//!   cargo run --example objectstore
//!
//! Expected final line: `RESULT ok dedup=1 readback=1 bytes=64`

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net_sdk::cortex::Redex;
use net_sdk::dataforts::{publish_blob_ref, BlobRef, MeshBlobAdapter};
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::transport::{fetch_blob, serve_blob_transfer};
use net_sdk::Identity;

/// 32 bytes exactly — a PSK, not a passphrase. Every node in a mesh shares it.
const PSK: [u8; 32] = [0x42; 32];

/// A fixed-size payload, so the byte count in the result line is deterministic.
const PAYLOAD: [u8; 64] = [0x5A; 64];

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

/// A blob store backed by a local log. Each node that serves or fetches blobs
/// installs the transfer subprotocol over its own adapter.
fn blob_store(_name: &str) -> Arc<MeshBlobAdapter> {
    let redex = Arc::new(Redex::new());
    Arc::new(MeshBlobAdapter::new("objects", redex))
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let holder = build(0x91).await;
    let reader = build(0x92).await;

    let holder_addr = holder.inner().local_addr();
    handshake(&holder, &reader, holder_addr).await;
    holder.inner().start();
    reader.inner().start();

    // Install the blob-transfer engine on both nodes before any store or fetch.
    // A fetch needs it just as much as a serve does.
    let holder_store = blob_store("holder");
    let reader_store = blob_store("reader");
    serve_blob_transfer(&holder, holder_store.clone());
    serve_blob_transfer(&reader, reader_store.clone());

    // Store the bytes and mint the address. The address is a BLAKE3 hash of the
    // content — the producer never names a bucket, a key or a path.
    let first = publish_blob_ref(&*holder_store, "mesh:orders/2026-09/payload", &PAYLOAD).await?;
    let encoded = first.encode();
    println!("stored {} bytes", PAYLOAD.len());
    let address = *first.small_hash().ok_or("a small blob ref")?;
    println!("minted address: {}", hex(&address));

    // Read your own write. The caller does not have to know which node the
    // bytes settled on — the address resolves through the mesh.
    let readback = fetch_blob(&reader, holder.inner().node_id(), &first).await?;
    let same = readback.as_ref() == PAYLOAD.as_slice();

    // Content addressing means the second store is a no-op that produces the
    // same address: identical bytes cannot occupy two identities.
    let second = publish_blob_ref(&*reader_store, "mesh:another/name/entirely", &PAYLOAD).await?;
    let dedup = usize::from(first.small_hash() == second.small_hash());

    // A BlobRef round-trips through bytes, which is how one rides inside an
    // ordinary event payload without the substrate inspecting it.
    let decoded = BlobRef::decode(&encoded)?.expect("a small blob ref");
    let round_trip = usize::from(decoded.small_hash() == first.small_hash());

    println!("read back from the mesh:  {} bytes", readback.len());
    println!("same bytes:               {same}");
    println!("storing them again:       same address = {}", dedup == 1);
    println!("ref round-trips on wire:  {}", round_trip == 1);

    println!(
        "RESULT ok dedup={dedup} readback={} bytes={}",
        usize::from(same),
        readback.len()
    );

    holder.shutdown().await?;
    reader.shutdown().await?;
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
