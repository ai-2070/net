//! Federation test phase 1 — simple cross-peer blob transfer.
//!
//! Two `MeshNode`s, paired by direct handshake, each running a
//! `MeshBlobAdapter`. Node A stores a blob; Node B fetches it. Pins
//! the federation S-1/S-2/S-3 trio working end-to-end:
//!
//! - B's local fetch initially misses (the chunk isn't in B's Redex).
//! - The substrate finds A advertising `causal:<hex>` for the chunk.
//! - The fetch routes to A over the `blob.fetch_chunk` nRPC and
//!   returns the bytes (paged in ≤1 MiB segments).
//! - B stores the chunk locally as a side effect (auto-store cache).
//! - A second fetch from B is served locally.
//! - `stat::replicas_observed` on B reports 2 (A and B both hold it).
//! - Bytes returned match the input byte-for-byte.
//! - A blob no peer holds surfaces `NotFound` rather than hanging.
//!
//! Run: `cargo test --features dataforts,cortex --test cross_peer_blob`

#![cfg(all(feature = "dataforts", feature = "cortex"))]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::dataforts::blob::{BlobAdapter, BlobRef, MeshBlobAdapter};
use net::adapter::net::redex::Redex;
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};

const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x42u8; 32];

fn test_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(3, Duration::from_secs(2))
        // Default is 10 s; zero it so every announce in the rapid
        // test sequence (serve → store-advertise → explicit announce)
        // broadcasts rather than being origin-rate-limited — otherwise
        // a fast store fires its causal:<hex> announce inside the
        // window and the tag never reaches the peer.
        .with_min_announce_interval(Duration::ZERO)
        .with_capability_gc_interval(Duration::from_millis(250));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: TEST_BUFFER_SIZE,
        recv_buffer_size: TEST_BUFFER_SIZE,
    };
    cfg
}

async fn build_node() -> Arc<MeshNode> {
    let cfg = test_config();
    let keypair = EntityKeypair::generate();
    Arc::new(MeshNode::new(keypair, cfg).await.expect("MeshNode::new"))
}

async fn handshake(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    let a_id = a.node_id();
    let b_id = b.node_id();
    let b_pub = *b.public_key();
    let b_addr = b.local_addr();
    let b_clone = b.clone();
    let accept = tokio::spawn(async move { b_clone.accept(a_id).await });
    a.connect(b_addr, &b_pub, b_id).await.expect("connect");
    accept.await.expect("accept task").expect("accept");
    a.start();
    b.start();
}

/// Deterministic test payload of `len` bytes.
fn payload(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

/// Build a `BlobRef::Small` over `bytes` (content-addressed hash).
fn small_ref(bytes: &[u8]) -> ([u8; 32], BlobRef) {
    let hash: [u8; 32] = blake3::hash(bytes).into();
    let blob_ref = BlobRef::small("mesh://cross-peer", hash, bytes.len() as u64);
    (hash, blob_ref)
}

/// Poll `cond` until true or the deadline elapses; panic with `what`
/// on timeout so the failure is legible rather than a silent race.
async fn wait_until<F: FnMut() -> bool>(mut cond: F, what: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for: {what}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cross_peer_blob_fetch_round_trip() {
    let node_a = build_node().await;
    let node_b = build_node().await;
    // B (the fetcher) connects to A (the holder), mirroring the
    // caller→server direction the nRPC capability-propagation tests
    // use so A's announcement folds into B's index.
    handshake(&node_b, &node_a).await;

    // Each node runs its own content-addressed store.
    let redex_a = Arc::new(Redex::new());
    let adapter_a = Arc::new(MeshBlobAdapter::new("a", redex_a));
    let redex_b = Arc::new(Redex::new());
    let adapter_b = Arc::new(MeshBlobAdapter::new("b", redex_b));

    // A serves the fetch RPC. B only wires peer-fetch (fallback +
    // advertise-on-store + stat) without serving — B is the fetcher.
    let _serve_a = node_a
        .serve_blob_fetch_chunk(adapter_a.clone())
        .expect("serve A");
    adapter_b.enable_peer_fetch(&node_b);

    // A blob spanning many ~1 KiB fetch segments — exercises the
    // multi-segment paging loop in the cross-peer fetch fallback.
    let bytes = payload(50_000);
    let (hash, blob_ref) = small_ref(&bytes);

    // A stores the blob and advertises it (the causal:<hex> tag rides
    // A's capability announcement). B announces too so the
    // bidirectional capability exchange delivers A's caps to B's fold.
    adapter_a.store(&blob_ref, &bytes).await.expect("A store");
    node_a.announce_blob_chunk(&hash).await.expect("A announce");
    node_b
        .announce_capabilities(net::adapter::net::behavior::capability::CapabilitySet::new())
        .await
        .expect("B announce");

    let a_id = node_a.node_id();
    let b_id = node_b.node_id();

    // B must observe A as a holder before the fetch can route.
    wait_until(
        || node_b.find_blob_chunk_holders(&hash).contains(&a_id),
        "B's fold to index A as a causal:<hex> holder",
    )
    .await;

    // First fetch on B: local miss → fallback to A → bytes returned.
    let fetched = adapter_b.fetch(&blob_ref).await.expect("B first fetch");
    assert_eq!(
        fetched.as_ref(),
        bytes.as_slice(),
        "cross-peer fetch must return the bytes byte-for-byte",
    );

    // Auto-store side effect: B now advertises itself as a holder.
    wait_until(
        || node_b.find_blob_chunk_holders(&hash).contains(&b_id),
        "B to become a holder after auto-store",
    )
    .await;

    // Second fetch is served from B's local store (B holds it now).
    let again = adapter_b.fetch(&blob_ref).await.expect("B second fetch");
    assert_eq!(again.as_ref(), bytes.as_slice(), "local re-fetch matches");

    // replicas_observed on B reports 2 — A and B both hold the chunk.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut observed = 0u32;
    while tokio::time::Instant::now() < deadline {
        observed = adapter_b.stat(&blob_ref).await.expect("B stat").replicas_observed;
        if observed >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        observed >= 2,
        "replicas_observed must reflect A and B; got {observed}",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cross_peer_blob_fetch_not_found_when_no_holder() {
    let node_a = build_node().await;
    let node_b = build_node().await;
    handshake(&node_a, &node_b).await;

    let redex_a = Arc::new(Redex::new());
    let adapter_a = Arc::new(MeshBlobAdapter::new("a", redex_a));
    let redex_b = Arc::new(Redex::new());
    let adapter_b = Arc::new(MeshBlobAdapter::new("b", redex_b));

    let _serve_a = node_a
        .serve_blob_fetch_chunk(adapter_a.clone())
        .expect("serve A");
    let _serve_b = node_b
        .serve_blob_fetch_chunk(adapter_b.clone())
        .expect("serve B");

    // Neither node ever stored this blob — no peer advertises it.
    let bytes = payload(64 * 1024);
    let (_hash, blob_ref) = small_ref(&bytes);

    let err = adapter_b
        .fetch(&blob_ref)
        .await
        .expect_err("fetch of an unheld blob must error, not hang");
    use net::adapter::net::dataforts::blob::BlobError;
    assert!(
        matches!(err, BlobError::NotFound(_)),
        "expected NotFound, got {err:?}",
    );
}
