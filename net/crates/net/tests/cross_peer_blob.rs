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
        observed = adapter_b
            .stat(&blob_ref)
            .await
            .expect("B stat")
            .replicas_observed;
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

/// Federation phase 2 — directory transfer between peers. A stores a
/// small tree (each file its own blob); B reconstructs it by fetching
/// the manifest and each leaf blob over the mesh, every leaf an
/// independent per-file substrate operation.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cross_peer_directory_transfer() {
    use net::adapter::net::dataforts::{fetch_dir, store_dir};

    let node_a = build_node().await;
    let node_b = build_node().await;
    handshake(&node_b, &node_a).await;

    let redex_a = Arc::new(Redex::new());
    let adapter_a = Arc::new(MeshBlobAdapter::new("a", redex_a));
    let redex_b = Arc::new(Redex::new());
    let adapter_b = Arc::new(MeshBlobAdapter::new("b", redex_b));

    // A serves + advertises; B fetches.
    let _serve_a = node_a
        .serve_blob_fetch_chunk(adapter_a.clone())
        .expect("serve A");
    adapter_b.enable_peer_fetch(&node_b);

    // Build a small source tree on disk.
    let tmp = std::env::temp_dir().join(format!(
        "net-xpeer-dir-{}-{}",
        std::process::id(),
        node_a.node_id()
    ));
    let src = tmp.join("src");
    let dst = tmp.join("dst");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(src.join("pkg/lib")).unwrap();
    std::fs::write(src.join("index.js"), b"module.exports = 1;").unwrap();
    std::fs::write(src.join("pkg/main.js"), vec![3u8; 40_000]).unwrap();
    std::fs::write(src.join("pkg/lib/util.js"), b"util code here").unwrap();
    // Duplicate content across two files — content addressing dedups.
    std::fs::write(src.join("pkg/copy.js"), b"module.exports = 1;").unwrap();

    // A stores the directory (per-file blobs + manifest blob).
    let root_ref = store_dir(adapter_a.as_ref(), &src)
        .await
        .expect("A store_dir");

    // Force a synchronous final announcement carrying every accumulated
    // causal:<hex> tag (store's per-chunk advertises are best-effort
    // spawned), then wait for B to fold it. The manifest blob is small,
    // so its single chunk hash stands in for "A's caps are folded".
    let manifest_hash = *root_ref.small_hash().expect("small manifest");
    node_a
        .announce_blob_chunk(&manifest_hash)
        .await
        .expect("A announce manifest");
    node_b
        .announce_capabilities(net::adapter::net::behavior::capability::CapabilitySet::new())
        .await
        .expect("B announce");
    let a_id = node_a.node_id();
    wait_until(
        || node_b.find_blob_chunk_holders(&manifest_hash).contains(&a_id),
        "B to fold A's directory chunk advertisements",
    )
    .await;

    // B reconstructs the tree, fetching every leaf over the mesh.
    fetch_dir(adapter_b.as_ref(), &root_ref, &dst)
        .await
        .expect("B fetch_dir");

    for rel in [
        "index.js",
        "pkg/main.js",
        "pkg/lib/util.js",
        "pkg/copy.js",
    ] {
        let want = std::fs::read(src.join(rel)).unwrap();
        let got = std::fs::read(dst.join(rel)).unwrap();
        assert_eq!(want, got, "file {rel} must transfer byte-for-byte");
    }
    assert!(dst.join("pkg/lib").is_dir(), "nested dirs reconstructed");

    let _ = std::fs::remove_dir_all(&tmp);
}

/// Federation phase 2 — RedEX replication transfers MORE chunks than
/// the per-chunk advertisement ceiling, reliably and with no per-chunk
/// `causal` tag at all. This is the path that scales the demo.
///
/// Replication has no auto-election yet (the unwired "Phase F"), so the
/// roles are driven manually — exactly as `redex_replication_e2e.rs`
/// does — A's chunk channels to Leader, B's to Replica. The replica
/// then pulls each chunk's single event via the real heartbeat →
/// SyncRequest → SyncResponse → apply cycle. Neither adapter wires the
/// fetch RPC, so a successful LOCAL fetch on B proves replication — not
/// the RPC and not advertisement — delivered every chunk.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn replication_transfers_past_advertisement_ceiling() {
    use net::adapter::net::channel::ChannelName;
    use net::adapter::net::dataforts::blob::BlobAdapter;
    use net::adapter::net::redex::{
        PlacementStrategy, ReplicaRole, ReplicationConfig, TransitionSignal,
    };

    // Well past the ~15-20 chunks/node advertisement ceiling.
    const N: usize = 30;

    let node_a = build_node().await;
    let node_b = build_node().await;
    handshake(&node_a, &node_b).await;
    let a_id = node_a.node_id();
    let b_id = node_b.node_id();

    let redex_a = Arc::new(Redex::new());
    redex_a.enable_replication(node_a.clone());
    let redex_b = Arc::new(Redex::new());
    redex_b.enable_replication(node_b.clone());

    let cfg = ReplicationConfig::new()
        .with_placement(PlacementStrategy::Pinned(vec![a_id, b_id]))
        .with_heartbeat_ms(100);
    cfg.validate().expect("valid cfg");

    // No serve_blob_fetch_chunk / enable_peer_fetch on either adapter:
    // no fetch RPC, no causal advertisement. Pure replication.
    let adapter_a = Arc::new(MeshBlobAdapter::new("a", redex_a.clone()).with_replication(cfg.clone()));
    let adapter_b = Arc::new(MeshBlobAdapter::new("b", redex_b.clone()).with_replication(cfg.clone()));

    // Manually drive a chunk channel's coordinator to Leader / Replica
    // (Idle is the spawn state; auto-elect is unwired Phase F).
    async fn drive_leader(redex: &Redex, name: &ChannelName) {
        let c = redex
            .replication_coordinator_for(name)
            .expect("leader coordinator");
        c.transition_to(ReplicaRole::Replica, TransitionSignal::CapabilitySelected)
            .await
            .unwrap();
        c.transition_to(ReplicaRole::Candidate, TransitionSignal::MissedHeartbeats)
            .await
            .unwrap();
        c.transition_to(ReplicaRole::Leader, TransitionSignal::ElectionWon)
            .await
            .unwrap();
    }
    async fn drive_replica(redex: &Redex, name: &ChannelName) {
        let c = redex
            .replication_coordinator_for(name)
            .expect("replica coordinator");
        c.transition_to(ReplicaRole::Replica, TransitionSignal::CapabilitySelected)
            .await
            .unwrap();
    }

    // A stores N distinct chunks and becomes leader for each channel.
    let mut blobs = Vec::new();
    for i in 0..N {
        let bytes: Vec<u8> = (0..800).map(|j| ((i * 7 + j) % 251) as u8).collect();
        let (hash, blob_ref) = small_ref(&bytes);
        adapter_a.store(&blob_ref, &bytes).await.expect("A store");
        let name = MeshBlobAdapter::chunk_channel_for_hash(&hash);
        drive_leader(&redex_a, &name).await;
        blobs.push((hash, blob_ref, bytes));
    }

    // B opens each channel (prefetch) and becomes replica.
    for (hash, blob_ref, _) in &blobs {
        adapter_b.prefetch(blob_ref).await.expect("B prefetch");
        let name = MeshBlobAdapter::chunk_channel_for_hash(hash);
        drive_replica(&redex_b, &name).await;
    }

    // Poll until replication has delivered every chunk locally to B.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let mut delivered = 0usize;
    while tokio::time::Instant::now() < deadline {
        delivered = 0;
        for (_, blob_ref, _) in &blobs {
            if adapter_b.fetch(blob_ref).await.is_ok() {
                delivered += 1;
            }
        }
        if delivered == N {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        delivered, N,
        "replication must deliver all {N} chunks (got {delivered}) — well past the advertisement ceiling",
    );

    // Verify every delivered chunk byte-for-byte.
    for (_, blob_ref, bytes) in &blobs {
        let got = adapter_b.fetch(blob_ref).await.expect("B local fetch");
        assert_eq!(got.as_ref(), bytes.as_slice(), "chunk bytes must match");
    }
}

/// Federation phase 2 — directory transfer at the per-chunk
/// advertisement ceiling, with throughput.
///
/// This answers the plan's open question empirically: **per-chunk
/// `causal:<hex>` advertisement does NOT scale to large file counts.**
/// A node's capability announcement is a single datagram, and each
/// chunk a node holds adds a ~71-byte `causal:<hex64>` tag to it, so
/// only ~MTU/71 ≈ 15-20 chunks per node fit before tags fall off the
/// wire and a fetcher can no longer discover holders for them. Measured
/// here: 10 files transfer cleanly; 100 files fail with `NotFound` for
/// the chunks whose tags didn't fit.
///
/// The fix for the `node_modules`-scale demo is RedEX replication's
/// per-NODE advertisement (one `causal:<node_origin>` tag covers all of
/// a node's replicated chunks) + chunk-level pull — see the project
/// memory `nrpc-not-for-bulk-transfer`. This test stays under the
/// ceiling so it pins the working small-tree path + a throughput number.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cross_peer_directory_transfer_at_advertisement_ceiling() {
    use net::adapter::net::dataforts::{fetch_dir, store_dir};

    // Kept under the ~15-20-chunks-per-node advertisement ceiling.
    const N_FILES: usize = 10;

    let node_a = build_node().await;
    let node_b = build_node().await;
    handshake(&node_b, &node_a).await;

    let redex_a = Arc::new(Redex::new());
    let adapter_a = Arc::new(MeshBlobAdapter::new("a", redex_a));
    let redex_b = Arc::new(Redex::new());
    let adapter_b = Arc::new(MeshBlobAdapter::new("b", redex_b));

    let _serve_a = node_a
        .serve_blob_fetch_chunk(adapter_a.clone())
        .expect("serve A");
    adapter_b.enable_peer_fetch(&node_b);

    // Build a tree of N small files (distinct content) across a few
    // subdirs — a node_modules-shaped small-file profile.
    let tmp = std::env::temp_dir().join(format!(
        "net-xpeer-dirscale-{}-{}",
        std::process::id(),
        node_a.node_id()
    ));
    let src = tmp.join("src");
    let dst = tmp.join("dst");
    let _ = std::fs::remove_dir_all(&tmp);
    let mut total_bytes = 0usize;
    for i in 0..N_FILES {
        let sub = src.join(format!("d{}", i % 8));
        std::fs::create_dir_all(&sub).unwrap();
        // ~1.5 KiB each, content varied by index so no dedup masks work.
        let content: Vec<u8> = (0..1500).map(|j| ((i * 31 + j) % 251) as u8).collect();
        total_bytes += content.len();
        std::fs::write(sub.join(format!("f{i}.bin")), &content).unwrap();
    }

    let store_start = std::time::Instant::now();
    let root_ref = store_dir(adapter_a.as_ref(), &src)
        .await
        .expect("A store_dir");
    let store_elapsed = store_start.elapsed();

    let manifest_hash = *root_ref.small_hash().expect("small manifest");
    node_a
        .announce_blob_chunk(&manifest_hash)
        .await
        .expect("A announce manifest");
    node_b
        .announce_capabilities(net::adapter::net::behavior::capability::CapabilitySet::new())
        .await
        .expect("B announce");
    let a_id = node_a.node_id();
    wait_until(
        || node_b.find_blob_chunk_holders(&manifest_hash).contains(&a_id),
        "B to fold A's directory chunk advertisements",
    )
    .await;

    let fetch_start = std::time::Instant::now();
    fetch_dir(adapter_b.as_ref(), &root_ref, &dst)
        .await
        .expect("B fetch_dir at scale");
    let fetch_elapsed = fetch_start.elapsed();

    // Verify every file byte-for-byte.
    for i in 0..N_FILES {
        let rel = format!("d{}/f{i}.bin", i % 8);
        let want = std::fs::read(src.join(&rel)).unwrap();
        let got = std::fs::read(dst.join(&rel)).unwrap();
        assert_eq!(want, got, "file {rel} must transfer byte-for-byte");
    }

    let mbps = (total_bytes as f64 / (1024.0 * 1024.0)) / fetch_elapsed.as_secs_f64().max(1e-6);
    eprintln!(
        "[scale] {N_FILES} files / {total_bytes} bytes: store {:?}, cross-peer fetch {:?} ({mbps:.2} MiB/s)",
        store_elapsed, fetch_elapsed,
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
