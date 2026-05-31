//! Federation phase 2 — directory transfer over router streams.
//!
//! A stores a directory tree (`store_dir`) into its blob adapter and
//! serves blob transfer; B pulls the whole tree from A by manifest ref
//! (`fetch_dir`) over the reliable scheduled stream transport — the
//! FairScheduler transport, NOT RedEX replication and NOT nRPC. Every
//! file is reconstructed on disk byte-for-byte, nested dirs and an
//! empty dir included, with no per-chunk discovery (B pulls from the
//! single known source A).
//!
//! Run: `cargo test --features dataforts --test dir_transfer`

#![cfg(feature = "dataforts")]

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use net::adapter::net::dataforts::blob::MeshBlobAdapter;
use net::adapter::net::dataforts::dir::{fetch_dir, store_dir};
use net::adapter::net::redex::Redex;
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};

const PSK: [u8; 32] = [0x42u8; 32];
const SOCKET_BUF: usize = 8 * 1024 * 1024;

fn test_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(15))
        .with_handshake(3, Duration::from_secs(2));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: SOCKET_BUF,
        recv_buffer_size: SOCKET_BUF,
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

/// Best-effort scratch dir under the OS temp root, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let mut base = std::env::temp_dir();
        base.push(format!(
            "net-dir-xfer-{tag}-{}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed),
            nanos
        ));
        std::fs::create_dir_all(&base).expect("create temp dir");
        Self(base)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Read every regular file under `root` into a `relative-path -> bytes`
/// map (using `/` separators) so two trees can be compared regardless
/// of walk order.
fn read_tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let abs = entry.path();
            let meta = std::fs::symlink_metadata(&abs).unwrap();
            if meta.is_dir() {
                walk(root, &abs, out);
            } else if meta.is_file() {
                let rel = abs
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, std::fs::read(&abs).unwrap());
            }
        }
    }
    walk(root, root, &mut out);
    out
}

fn write(root: &Path, rel: &str, bytes: &[u8]) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, bytes).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn directory_transfer_reconstructs_tree() {
    let node_a = build_node().await;
    let node_b = build_node().await;
    // B (requester) connects to A (holder).
    handshake(&node_b, &node_a).await;
    let a_id = node_a.node_id();

    let redex_a = Arc::new(Redex::new());
    let adapter_a = Arc::new(MeshBlobAdapter::new("a", redex_a));
    let redex_b = Arc::new(Redex::new());
    let adapter_b = Arc::new(MeshBlobAdapter::new("b", redex_b));
    node_a.serve_blob_transfer(adapter_a.clone());
    node_b.serve_blob_transfer(adapter_b);

    // Build a source tree: nested dirs, a tiny file, a mid file (multi-
    // frame, exercises the data plane), a zero-byte file, and an empty
    // directory.
    let src = TempDir::new("src");
    write(src.path(), "readme.txt", b"hello directory transfer");
    write(src.path(), "a/one.bin", &(0..5000u32).map(|i| i as u8).collect::<Vec<_>>());
    write(src.path(), "a/b/two.bin", &(0..50_000u32).map(|i| (i % 251) as u8).collect::<Vec<_>>());
    write(src.path(), "a/b/empty.dat", b"");
    std::fs::create_dir_all(src.path().join("c/empty_dir")).unwrap();

    // A stores the tree.
    let manifest_ref = store_dir(&adapter_a, src.path()).await.expect("store_dir");

    // B pulls it from A.
    let dest = TempDir::new("dest");
    let stats = fetch_dir(&node_b, a_id, &manifest_ref, dest.path(), 0)
        .await
        .expect("fetch_dir");

    // Files reconstructed byte-for-byte.
    let want = read_tree(src.path());
    let got = read_tree(dest.path());
    assert_eq!(got, want, "reconstructed tree must match source byte-for-byte");
    assert_eq!(stats.files, want.len(), "stats.files matches file count");

    // The empty directory survived.
    assert!(
        dest.path().join("c/empty_dir").is_dir(),
        "empty directory must be recreated"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn directory_transfer_many_small_files() {
    // The node_modules-shaped case: many small files, one known source,
    // no per-chunk advertisement. This is what the advertisement-ceiling
    // path (~15-20 chunks/node) could not deliver.
    let node_a = build_node().await;
    let node_b = build_node().await;
    handshake(&node_b, &node_a).await;
    let a_id = node_a.node_id();

    let redex_a = Arc::new(Redex::new());
    let adapter_a = Arc::new(MeshBlobAdapter::new("a", redex_a));
    let redex_b = Arc::new(Redex::new());
    let adapter_b = Arc::new(MeshBlobAdapter::new("b", redex_b));
    node_a.serve_blob_transfer(adapter_a.clone());
    node_b.serve_blob_transfer(adapter_b);

    let src = TempDir::new("manysrc");
    let n = 200usize; // well past the ~15-20 advertisement ceiling
    for i in 0..n {
        let body = format!("file {i} contents — {}", "x".repeat(i % 64));
        write(src.path(), &format!("pkg{}/mod{}.js", i % 12, i), body.as_bytes());
    }

    let manifest_ref = store_dir(&adapter_a, src.path()).await.expect("store_dir");
    let dest = TempDir::new("manydest");
    let stats = fetch_dir(&node_b, a_id, &manifest_ref, dest.path(), 0)
        .await
        .expect("fetch_dir");

    assert_eq!(stats.files, n, "all {n} files transferred");
    assert_eq!(read_tree(dest.path()), read_tree(src.path()), "trees match");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn directory_transfer_large_multichunk_file() {
    // A file larger than the 4 MiB chunk threshold becomes a
    // BlobRef::Manifest (multiple content-addressed chunks). This
    // exercises the dir wrapper's transfer_fetch_blob Manifest branch —
    // fetch each chunk by hash via the transfer transport and
    // concatenate in manifest order — which the small-file tests never
    // hit (they're all single-chunk Small blobs).
    use net::adapter::net::dataforts::blob::BLOB_CHUNK_SIZE_BYTES;

    let node_a = build_node().await;
    let node_b = build_node().await;
    handshake(&node_b, &node_a).await;
    let a_id = node_a.node_id();

    let redex_a = Arc::new(Redex::new());
    let adapter_a = Arc::new(MeshBlobAdapter::new("a", redex_a));
    let redex_b = Arc::new(Redex::new());
    let adapter_b = Arc::new(MeshBlobAdapter::new("b", redex_b));
    node_a.serve_blob_transfer(adapter_a.clone());
    node_b.serve_blob_transfer(adapter_b);

    // ~9 MiB ⇒ 3 chunks (4 MiB + 4 MiB + ~1 MiB). A small sibling file
    // checks the mixed Manifest+Small case in one tree.
    let big_len = 9 * 1024 * 1024usize;
    assert!(big_len as u64 > BLOB_CHUNK_SIZE_BYTES, "must exceed one chunk");
    let big: Vec<u8> = (0..big_len).map(|i| (i % 251) as u8).collect();

    let src = TempDir::new("bigsrc");
    write(src.path(), "small.txt", b"a small sibling");
    write(src.path(), "data/big.bin", &big);

    let manifest_ref = store_dir(&adapter_a, src.path()).await.expect("store_dir");
    let dest = TempDir::new("bigdest");
    let stats = fetch_dir(&node_b, a_id, &manifest_ref, dest.path(), 0)
        .await
        .expect("fetch_dir");

    assert_eq!(stats.files, 2, "both files transferred");
    assert_eq!(stats.bytes, (big_len + 15) as u64, "byte total");
    let got = read_tree(dest.path());
    assert_eq!(got.get("data/big.bin").map(|v| v.len()), Some(big_len));
    assert_eq!(
        read_tree(dest.path()),
        read_tree(src.path()),
        "multi-chunk file + sibling reconstruct byte-for-byte"
    );
}
