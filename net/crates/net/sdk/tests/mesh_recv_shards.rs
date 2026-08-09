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

/// A continuously-fed shard must not starve the others.
///
/// The shard-0-only bug was replaced by a sweep that always *starts* at
/// shard 0 and stops once it has `limit` events. That is the same
/// failure needing sustained load to appear: while shard 0 can fill the
/// limit on its own, shards 1..n are never reached, so a quiet stream
/// on a later shard is invisible for as long as the producer keeps up.
///
/// A finite backlog only *delays* the later shards — drain it and the
/// sweep walks on — so the load here is a producer running for the
/// duration of the read. `recv` is called with `limit = 1`, so each
/// call spends its whole budget on the first non-empty shard it
/// visits, and the read is bounded by a call count rather than a
/// timeout: only a rotating start reaches the quiet shard at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_continuously_fed_shard_does_not_starve_the_others() {
    let psk = [0x42u8; 32];
    let (a, b, addr_b) = two_meshes(&psk).await;
    handshake(&a, &b, addr_b).await;

    let nid_a = a.inner().node_id();
    let shards = a.num_shards().max(1);
    if shards < 2 {
        // Nothing to starve.
        return;
    }

    // `stream_id % shards == 0` → shard 0; the last residue class → the
    // shard furthest from the start of an un-rotated sweep.
    let busy_stream = shards as u64;
    let quiet_stream = shards as u64 * 2 - 1;
    assert_eq!(a.shard_for_stream(busy_stream), 0);
    assert_eq!(a.shard_for_stream(quiet_stream), shards - 1);

    let open = |m: &Mesh, stream_id: u64| {
        m.open_stream(
            nid_a,
            stream_id,
            StreamConfig::new().with_reliability(Reliability::Reliable),
        )
        .expect("open_stream")
    };

    // The quiet event lands first, so its absence later cannot be
    // blamed on ordering.
    let quiet = open(&b, quiet_stream);
    b.send_with_retry(&quiet, &[Bytes::from_static(b"quiet")], 16)
        .await
        .expect("send quiet");

    // Keep shard 0 fed for the whole read.
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let producer = {
        let stop = std::sync::Arc::clone(&stop);
        let busy = open(&b, busy_stream);
        tokio::spawn(async move {
            let mut i = 0u32;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                if b.send_with_retry(&busy, &[Bytes::from(format!("busy-{i}"))], 16)
                    .await
                    .is_err()
                {
                    break;
                }
                i += 1;
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
    };

    // Let the producer build a standing backlog before reading, so
    // shard 0 is non-empty on the very first sweep.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Bounded by CALLS, not time: an un-rotated sweep gets as many
    // chances as it likes and still never leaves shard 0.
    let max_calls = shards as usize * 8;
    let mut saw_quiet = false;
    let mut busy_seen = 0usize;
    for _ in 0..max_calls {
        for event in a.recv(1).await.expect("recv") {
            match std::str::from_utf8(&event.raw) {
                Ok("quiet") => saw_quiet = true,
                Ok(_) => busy_seen += 1,
                Err(_) => {}
            }
        }
        if saw_quiet {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }

    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = producer.await;

    assert!(
        busy_seen > 0,
        "test premise: shard 0 must have been busy throughout, but \
         nothing was read from it",
    );
    assert!(
        saw_quiet,
        "recv never reached shard {} in {max_calls} calls while shard 0 \
         stayed fed ({busy_seen} busy events consumed). A sweep that \
         always starts at shard 0 and stops at `limit` starves every \
         later shard.",
        shards - 1,
    );
}
