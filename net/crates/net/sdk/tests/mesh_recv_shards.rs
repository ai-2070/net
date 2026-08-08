//! `Mesh::recv` must actually reach every shard.
//!
//! Inbound stream traffic is placed on `stream_id % num_shards`
//! (`adapter/net/mesh.rs`). `recv` documented "events from all shards"
//! and polled shard 0, so at the default of four shards three quarters
//! of ordinary stream ids were unreadable through it — and through the
//! Node and Python `poll` methods, which had the same body.
//!
//! The stream ids here are chosen to cover every residue class mod 4,
//! so a shard-0-only implementation fails on three of the four.

#![cfg(feature = "net")]

use std::time::Duration;

use bytes::Bytes;
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::{Reliability, StreamConfig};

async fn two_meshes(psk: &[u8; 32]) -> (Mesh, Mesh, std::net::SocketAddr) {
    let a = MeshBuilder::new("127.0.0.1:0", psk)
        .unwrap()
        .build()
        .await
        .unwrap();
    let b = MeshBuilder::new("127.0.0.1:0", psk)
        .unwrap()
        .build()
        .await
        .unwrap();
    let addr_b = b.inner().local_addr();
    (a, b, addr_b)
}

async fn handshake(a: &Mesh, b: &Mesh, addr_b: std::net::SocketAddr) {
    let pub_b = *b.inner().public_key();
    let nid_b = b.inner().node_id();
    let nid_a = a.inner().node_id();
    let (r1, r2) = tokio::join!(b.inner().accept(nid_a), async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        a.inner().connect(addr_b, &pub_b, nid_b).await
    });
    r1.expect("accept");
    r2.expect("connect");
    a.inner().start();
    b.inner().start();
}

/// `shard_for_stream` must agree with the dispatch-side modulo. If
/// these drift, every targeted read silently polls the wrong queue.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shard_for_stream_matches_the_dispatch_mapping() {
    let psk = [0x42u8; 32];
    let (a, _b, _addr) = two_meshes(&psk).await;

    let shards = a.num_shards();
    assert!(shards > 0, "a mesh must report at least one shard");

    for stream_id in [0u64, 1, 2, 3, 4, 7, 0xCAFE, u64::MAX] {
        assert_eq!(
            a.shard_for_stream(stream_id),
            (stream_id % shards as u64) as u16,
            "shard_for_stream({stream_id}) must equal stream_id % num_shards",
        );
    }
}

/// The documented example ids from `streams.md` land off shard 0, which
/// is what made the guide's own snippets impossible to run.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn documented_example_stream_ids_are_not_on_shard_zero() {
    let psk = [0x42u8; 32];
    let (a, _b, _addr) = two_meshes(&psk).await;

    if a.num_shards() != 4 {
        // The guide's arithmetic assumes the default.
        return;
    }
    assert_eq!(a.shard_for_stream(7), 3, "7 % 4 == 3");
    assert_eq!(a.shard_for_stream(0xCAFE), 2, "0xCAFE % 4 == 2");
}

/// End-to-end: send on a stream whose id maps to a non-zero shard and
/// require `recv` to surface it. This is the case a shard-0 poll drops
/// entirely.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recv_merges_traffic_from_every_shard() {
    let psk = [0x42u8; 32];
    let (a, b, addr_b) = two_meshes(&psk).await;
    handshake(&a, &b, addr_b).await;

    let nid_a = a.inner().node_id();
    let shards = a.num_shards().max(1);

    // One stream per residue class, so at the default of four shards
    // exactly one of these would have been visible before the fix.
    let stream_ids: Vec<u64> = (0..shards as u64).map(|r| r + shards as u64).collect();

    for &stream_id in &stream_ids {
        let stream = b
            .open_stream(
                nid_a,
                stream_id,
                StreamConfig::new().with_reliability(Reliability::Reliable),
            )
            .expect("open_stream");
        let payload = vec![Bytes::from(format!("stream-{stream_id}"))];
        b.send_with_retry(&stream, &payload, 16)
            .await
            .expect("send_with_retry");
    }

    let mut seen: Vec<String> = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline && seen.len() < stream_ids.len() {
        for event in a.recv(64).await.expect("recv") {
            if let Ok(text) = std::str::from_utf8(&event.raw) {
                let text = text.to_string();
                if !seen.contains(&text) {
                    seen.push(text);
                }
            }
        }
        if seen.len() < stream_ids.len() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    for &stream_id in &stream_ids {
        let expected = format!("stream-{stream_id}");
        assert!(
            seen.contains(&expected),
            "recv missed stream {stream_id} (shard {}) — saw {seen:?}. \
             A shard-0-only recv drops every stream whose id is not \
             congruent to 0 mod {shards}.",
            a.shard_for_stream(stream_id),
        );
    }
}
