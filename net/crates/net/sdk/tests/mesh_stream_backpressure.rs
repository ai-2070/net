//! Rust SDK smoke test: stream open → concurrent sends hit the
//! window → `SdkError::Backpressure` surfaces through the SDK layer,
//! `send_with_retry` absorbs the pressure and eventually succeeds.
//!
//! This exercises the full chain `MeshNode` → core `StreamError` →
//! `SdkError::Backpressure` conversion in `sdk/src/error.rs`. If the
//! `From<StreamError>` impl regresses, this test surfaces it
//! immediately rather than at daemon runtime.

#![cfg(feature = "net")]

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use net_sdk::error::SdkError;
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::{Reliability, StreamConfig};

/// Build two `Mesh` nodes bound on OS-assigned ephemeral ports. Reads
/// the kernel-picked port back via `MeshNode::local_addr()` after the
/// bind has already succeeded — avoids the classic "bind+drop to probe,
/// then re-bind" race that can make tests flaky under parallel runs.
async fn two_bound_meshes(psk: &[u8; 32]) -> (Mesh, Mesh, std::net::SocketAddr) {
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

/// Open a stream with window=1, spawn concurrent sends, assert that at
/// least one observes `SdkError::Backpressure` (i.e. the core variant
/// crosses the SDK boundary cleanly).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_sdk_surfaces_backpressure_variant() {
    let psk = [0x42u8; 32];
    let (a, b, addr_b) = two_bound_meshes(&psk).await;

    // Handshake (A connects to B as initiator, B accepts).
    let pub_b = *b.inner().public_key();
    let nid_b = b.inner().node_id();
    let nid_a = a.inner().node_id();

    let (r1, r2) = tokio::join!(b.inner().accept(nid_a), async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        a.inner().connect(addr_b, &pub_b, nid_b).await
    });
    r1.expect("accept");
    r2.expect("connect");
    a.start();
    b.start();

    let a = Arc::new(a);
    // v2 wire-bytes accounting: each packet on the wire costs
    // 64 B Net header + 16 B AEAD tag + 6 B payload (2 B `{}` + 4 B
    // EventFrame length) = 86 B. A 96-byte window fits exactly one
    // packet; concurrent callers race for the same window and
    // surface Backpressure just like v1.
    let stream = a
        .open_stream(
            nid_b,
            0x1337,
            StreamConfig::new()
                .with_reliability(Reliability::FireAndForget)
                .with_window_bytes(96),
        )
        .expect("open_stream");

    // 16 concurrent sends on a window-1 stream. At least one must
    // cross the SDK boundary as `SdkError::Backpressure`.
    let mut handles = Vec::new();
    for _ in 0..16 {
        let mesh = Arc::clone(&a);
        let stream = stream.clone();
        handles.push(tokio::spawn(async move {
            mesh.send_on_stream(&stream, &[Bytes::from_static(b"{}")])
                .await
        }));
    }
    let mut bp = 0usize;
    let mut ok = 0usize;
    for h in handles {
        match h.await.unwrap() {
            Ok(()) => ok += 1,
            Err(SdkError::Backpressure) => bp += 1,
            Err(e) => panic!("unexpected error kind: {:?}", e),
        }
    }
    assert!(
        bp > 0,
        "expected SdkError::Backpressure from concurrent sends; ok={}, bp={}",
        ok,
        bp
    );
    assert!(ok > 0);

    let stats = a.stream_stats(nid_b, 0x1337).expect("stats");
    assert_eq!(stats.tx_window, 96);
    assert!(stats.backpressure_events >= bp as u64);

    // Shutdown — consume the Arc.
    Arc::try_unwrap(a).ok().unwrap().shutdown().await.unwrap();
    b.shutdown().await.unwrap();
}

/// `send_with_retry` should absorb Backpressure and eventually succeed
/// on a 4-slot window under 32 concurrent senders.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_sdk_send_with_retry_succeeds_through_backpressure() {
    let psk = [0x42u8; 32];
    let (a, b, addr_b) = two_bound_meshes(&psk).await;

    let pub_b = *b.inner().public_key();
    let nid_b = b.inner().node_id();
    let nid_a = a.inner().node_id();

    let (r1, r2) = tokio::join!(b.inner().accept(nid_a), async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        a.inner().connect(addr_b, &pub_b, nid_b).await
    });
    r1.unwrap();
    r2.unwrap();
    a.start();
    b.start();

    let a = Arc::new(a);
    // v2 wire-bytes window: 512 bytes ≈ 5 × (80 B overhead + small
    // payload) — enough for a handful of packets concurrently, but
    // small enough that retries have to wait for receiver-driven
    // StreamWindow grants to free credit.
    let stream = a
        .open_stream(nid_b, 0x2468, StreamConfig::new().with_window_bytes(512))
        .unwrap();

    let mut handles = Vec::new();
    for i in 0..32 {
        let mesh = Arc::clone(&a);
        let stream = stream.clone();
        let payload = Bytes::from(format!(r#"{{"i":{}}}"#, i));
        handles.push(tokio::spawn(async move {
            mesh.send_with_retry(&stream, &[payload], 64).await
        }));
    }
    for h in handles {
        h.await.unwrap().expect("send_with_retry must succeed");
    }

    let stats = a.stream_stats(nid_b, 0x2468).unwrap();
    // What matters is that the retry path actually engaged — i.e.
    // some concurrent callers hit Backpressure and `send_with_retry`
    // absorbed it. `tx_credit_remaining` at end-of-run is unstable
    // (can legitimately land at 0 if the last commit happened between
    // the last grant and the snapshot), so we assert on the bumped
    // rejection counter instead.
    assert!(stats.backpressure_events > 0);

    Arc::try_unwrap(a).ok().unwrap().shutdown().await.unwrap();
    b.shutdown().await.unwrap();
}
