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
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use net::adapter::net::dataforts::blob::{
    chunk_payload, BlobAdapter, BlobRef, Encoding, MeshBlobAdapter,
};
use net::adapter::net::dataforts::dir::{
    fetch_dir, store_dir, DirEntry, DirManifest, EntryKind, DIR_MANIFEST_VERSION,
};
use net::adapter::net::redex::Redex;
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};

const PSK: [u8; 32] = [0x42u8; 32];
// 16 MiB so a few concurrent large-file transfers (5 MiB tx window each)
// don't overflow the kernel recv buffer — the flow-control window is
// per-stream, so aggregate in-flight scales with concurrency. Small-file
// workloads never approach this.
const SOCKET_BUF: usize = 16 * 1024 * 1024;

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
    write(
        src.path(),
        "a/one.bin",
        &(0..5000u32).map(|i| i as u8).collect::<Vec<_>>(),
    );
    write(
        src.path(),
        "a/b/two.bin",
        &(0..50_000u32).map(|i| (i % 251) as u8).collect::<Vec<_>>(),
    );
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
    assert_eq!(
        got, want,
        "reconstructed tree must match source byte-for-byte"
    );
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
        write(
            src.path(),
            &format!("pkg{}/mod{}.js", i % 12, i),
            body.as_bytes(),
        );
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
async fn directory_transfer_many_large_files() {
    // Several 4 MiB files in one tree, pulled at the default fan-out.
    // Without the in-flight byte budget, the default concurrency (16)
    // would put ~16 × 4 MiB on the wire at once, overflow the recv
    // buffer, and time out (see tests/transfer_concurrency.rs). The
    // budget self-limits large files to a couple concurrent so the tree
    // transfers cleanly.
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

    let src = TempDir::new("biglots-src");
    let n = 6usize;
    for i in 0..n {
        let body: Vec<u8> = (0..4 * 1024 * 1024usize)
            .map(|j| ((j + i) % 251) as u8)
            .collect();
        write(src.path(), &format!("artifact{i}.bin"), &body);
    }

    let manifest_ref = store_dir(&adapter_a, src.path()).await.expect("store_dir");
    let dest = TempDir::new("biglots-dest");
    // Pass the default fan-out (16) — the byte budget, not this count, is
    // what keeps concurrent large files from overflowing the buffer.
    let stats = fetch_dir(&node_b, a_id, &manifest_ref, dest.path(), 16)
        .await
        .expect("fetch_dir (many large files)");

    assert_eq!(stats.files, n, "all {n} large files transferred");
    assert_eq!(read_tree(dest.path()), read_tree(src.path()), "trees match");
}

/// Generate a `node_modules`-shaped tree under `root`: many packages,
/// each with a few small metadata/source files at depth 2-4, some
/// packages carrying a larger bundled file, and (on unix) a `.bin`
/// symlink. Deterministic content. Returns `(file_count, total_bytes)`.
fn gen_node_modules(root: &Path, packages: usize) -> (usize, u64) {
    let mut files = 0usize;
    let mut bytes = 0u64;
    let mut emit = |rel: String, len: usize, seed: u8| {
        let body: Vec<u8> = (0..len)
            .map(|i| ((i + seed as usize) % 251) as u8)
            .collect();
        write(root, &rel, &body);
        files += 1;
        bytes += len as u64;
    };
    for p in 0..packages {
        let pkg = format!("node_modules/pkg{p:04}");
        // package.json + entry + readme — the small-file bulk.
        emit(format!("{pkg}/package.json"), 200 + (p % 400), 1);
        emit(format!("{pkg}/index.js"), 800 + (p * 7 % 4000), 2);
        emit(format!("{pkg}/README.md"), 500 + (p % 1500), 3);
        // a lib/ dir with several modules (depth 3)
        for f in 0..(3 + p % 5) {
            emit(
                format!("{pkg}/lib/mod{f}.js"),
                300 + (p * f % 6000),
                (f + 4) as u8,
            );
        }
        // every 7th package has a bigger bundled artifact + deeper nest
        if p % 7 == 0 {
            emit(
                format!("{pkg}/dist/bundle.min.js"),
                40_000 + (p % 20_000),
                9,
            );
            emit(
                format!("{pkg}/node_modules/dep{p}/index.js"),
                600 + (p % 2000),
                11,
            );
        }
    }
    // A symlink (best-effort; only exercised where the OS + privilege
    // allow — fetch_dir tolerates failure and read_tree skips links).
    #[cfg(unix)]
    {
        let link = root.join("node_modules/.bin/cli");
        let _ = std::fs::create_dir_all(link.parent().unwrap());
        let _ = std::os::unix::fs::symlink("../pkg0000/index.js", &link);
    }
    (files, bytes)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "scale benchmark — run with --ignored --nocapture"]
async fn bench_nodemodules_scale() {
    // Phase 2 of FAIRSCHEDULER_TRANSPORT_PLAN: a node_modules-shaped
    // tree transferred between paired nodes. NOTE: the plan's absolute
    // target (200 MB / 30k files < 30 s) is a LINUX-localhost figure; on
    // a Windows host the per-datagram loopback latency caps throughput
    // (see transfer_fairness bench), so we report the actual numbers and
    // the structural correctness rather than asserting the wall-clock.
    const PACKAGES: usize = 1500; // ~13k files — toward node_modules scale
    const CONCURRENCY: usize = 16;
    // No per-chunk memory cap: the store now defaults chunk files to a
    // 0 initial reservation and the grow-only segment sizes to content
    // (the on-demand-sizing fix). So ~13k chunks cost ≈ Σ(content), not
    // 13k × 64 MiB. This is the regime that OOM'd before the fix.

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

    let src = TempDir::new("nm-src");
    let (file_count, total_bytes) = gen_node_modules(src.path(), PACKAGES);
    let mib = total_bytes as f64 / (1024.0 * 1024.0);

    let store_start = Instant::now();
    let manifest_ref = store_dir(&adapter_a, src.path()).await.expect("store_dir");
    let store_elapsed = store_start.elapsed();

    let dest = TempDir::new("nm-dest");
    let xfer_start = Instant::now();
    let stats = fetch_dir(&node_b, a_id, &manifest_ref, dest.path(), CONCURRENCY)
        .await
        .expect("fetch_dir");
    let xfer_elapsed = xfer_start.elapsed();

    println!("── node_modules-scale transfer ──");
    println!("  tree:     {file_count} files, {mib:.1} MiB (deep-nested, mixed sizes)");
    println!("  store:    {store_elapsed:?}");
    println!(
        "  transfer: {xfer_elapsed:?} = {:.1} MiB/s, {:.0} files/s",
        mib / xfer_elapsed.as_secs_f64(),
        stats.files as f64 / xfer_elapsed.as_secs_f64()
    );
    println!(
        "  stats:    {} files, {} dirs, {} bytes",
        stats.files, stats.dirs, stats.bytes
    );

    // Correctness is the hard pass criterion regardless of platform speed.
    assert_eq!(stats.files, file_count, "every file transferred");
    assert_eq!(stats.bytes, total_bytes, "byte total matches");
    assert_eq!(
        read_tree(dest.path()),
        read_tree(src.path()),
        "reconstructed node_modules matches source byte-for-byte",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "scale benchmark — run with --ignored --nocapture"]
async fn bench_throughput_invariance() {
    // The architectural claim (plan Phase 2): for EQUAL byte volume,
    // throughput at high file count should be within 80% of throughput
    // at low file count — i.e. the fair scheduler amortizes per-stream
    // overhead so volume, not file count, sets the rate. Measure both
    // extremes and report the ratio. Honest finding either way.
    const VOLUME: usize = 8 * 1024 * 1024; // 8 MiB each arm
    const CONCURRENCY: usize = 8;

    // `cap` is the per-chunk RedEX segment reservation — it must cover
    // the arm's largest chunk (the file, or the directory manifest)
    // while keeping `chunk_count × cap` bounded (see the scale bench).
    async fn run_arm(label: &str, files: usize, file_len: usize, cap: usize) -> f64 {
        let node_a = build_node().await;
        let node_b = build_node().await;
        handshake(&node_b, &node_a).await;
        let a_id = node_a.node_id();
        let adapter_a = Arc::new(
            MeshBlobAdapter::new("a", Arc::new(Redex::new())).with_chunk_file_max_memory_bytes(cap),
        );
        let adapter_b = Arc::new(MeshBlobAdapter::new("b", Arc::new(Redex::new())));
        node_a.serve_blob_transfer(adapter_a.clone());
        node_b.serve_blob_transfer(adapter_b);

        let src = TempDir::new("inv-src");
        for f in 0..files {
            let body: Vec<u8> = (0..file_len).map(|i| ((i + f) % 251) as u8).collect();
            write(src.path(), &format!("d{}/f{f}.bin", f % 32), &body);
        }
        let manifest_ref = store_dir(&adapter_a, src.path()).await.expect("store_dir");
        let dest = TempDir::new("inv-dest");
        let start = Instant::now();
        let stats = fetch_dir(&node_b, a_id, &manifest_ref, dest.path(), CONCURRENCY)
            .await
            .expect("fetch_dir");
        let elapsed = start.elapsed();
        let mib = stats.bytes as f64 / (1024.0 * 1024.0);
        let rate = mib / elapsed.as_secs_f64();
        println!("  {label}: {files} files × {file_len} B = {mib:.1} MiB in {elapsed:?} = {rate:.2} MiB/s");
        rate
    }

    println!(
        "── throughput invariance (equal {} MiB volume) ──",
        VOLUME / (1024 * 1024)
    );
    // Low file count: few 4 MiB files (cap covers one 4 MiB chunk).
    let low = run_arm(
        "few-large ",
        VOLUME / (4 * 1024 * 1024),
        4 * 1024 * 1024,
        5 * 1024 * 1024,
    )
    .await;
    // High file count: many 8 KiB files (cap covers the ~80 KiB manifest).
    let high = run_arm("many-small", VOLUME / (8 * 1024), 8 * 1024, 256 * 1024).await;
    let ratio = high / low;
    println!("  invariance ratio (many-small / few-large): {ratio:.2}  (plan target ≥ 0.80)");
    if ratio >= 0.80 {
        println!("  ✓ throughput scales with volume, not file count");
    } else {
        println!(
            "  ✗ per-file overhead is significant: many-small is {:.0}% of few-large throughput",
            ratio * 100.0
        );
    }
    // Report-only — this bench documents the property; it does not gate CI.
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
    assert!(
        big_len as u64 > BLOB_CHUNK_SIZE_BYTES,
        "must exceed one chunk"
    );
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

// ── Atomic reconstruction (FETCH_DIR_ATOMIC_PLAN) ───────────────────

/// Lowercase-hex of a 32-byte hash.
fn hex32(h: &[u8; 32]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(64);
    for b in h {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Store a hand-crafted manifest as a blob on `adapter` and return its
/// ref — lets a test drive `fetch_dir` with a manifest that `store_dir`
/// would never emit (e.g. one whose entry path escapes the destination,
/// to deterministically fail reconstruction after the temp dir exists).
async fn store_manifest(adapter: &MeshBlobAdapter, manifest: &DirManifest) -> BlobRef {
    let bytes = postcard::to_allocvec(manifest).unwrap();
    let hash: [u8; 32] = blake3::hash(&bytes).into();
    let chunked = chunk_payload(&bytes).unwrap();
    let blob_ref = chunked
        .into_blob_ref(format!("mesh://{}", hex32(&hash)), Encoding::Replicated)
        .unwrap();
    adapter.store(&blob_ref, &bytes).await.unwrap();
    blob_ref
}

/// Names under `parent` that look like `fetch_dir` temp/backup orphans
/// for destination `base` (`.<base>.fetch_*` / `.<base>.replaced_*`).
fn temp_orphans(parent: &Path, base: &str) -> Vec<String> {
    let fetch_pfx = format!(".{base}.fetch_");
    let repl_pfx = format!(".{base}.replaced_");
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(parent) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with(&fetch_pfx) || name.starts_with(&repl_pfx) {
                out.push(name);
            }
        }
    }
    out
}

/// Successful replacement installs the new tree AND drops files from a
/// previous version that aren't in the new manifest (no stale-file
/// accumulation), leaving no temp/backup orphans.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fetch_dir_replaces_and_removes_stale_files() {
    let node_a = build_node().await;
    let node_b = build_node().await;
    handshake(&node_b, &node_a).await;
    let a_id = node_a.node_id();

    let adapter_a = Arc::new(MeshBlobAdapter::new("a", Arc::new(Redex::new())));
    node_a.serve_blob_transfer(adapter_a.clone());
    node_b.serve_blob_transfer(Arc::new(MeshBlobAdapter::new("b", Arc::new(Redex::new()))));

    // v1 = {a.txt: "A1", b.txt: "B1"}.
    let src1 = TempDir::new("src1");
    write(src1.path(), "a.txt", b"A1");
    write(src1.path(), "b.txt", b"B1");
    let m1 = store_dir(&adapter_a, src1.path()).await.expect("store v1");

    // v2 = {a.txt: "A2"} only.
    let src2 = TempDir::new("src2");
    write(src2.path(), "a.txt", b"A2");
    let m2 = store_dir(&adapter_a, src2.path()).await.expect("store v2");

    let parent = TempDir::new("parent");
    let dest = parent.path().join("dest");

    fetch_dir(&node_b, a_id, &m1, &dest, 0)
        .await
        .expect("fetch v1");
    assert_eq!(
        read_tree(&dest).keys().cloned().collect::<Vec<_>>(),
        vec!["a.txt".to_string(), "b.txt".to_string()],
        "v1 has both files"
    );

    fetch_dir(&node_b, a_id, &m2, &dest, 0)
        .await
        .expect("fetch v2");
    let got = read_tree(&dest);
    assert_eq!(
        got.keys().cloned().collect::<Vec<_>>(),
        vec!["a.txt".to_string()],
        "stale b.txt removed by the atomic replace"
    );
    assert_eq!(
        got.get("a.txt").map(Vec::as_slice),
        Some(&b"A2"[..]),
        "new content"
    );
    assert!(
        temp_orphans(parent.path(), "dest").is_empty(),
        "no temp/backup orphans after a successful replace"
    );
}

/// A failure mid-reconstruction (here: an unsafe manifest path, which
/// fails after the temp dir is created) leaves an existing `dest`
/// byte-for-byte untouched and removes the temp tree.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fetch_dir_failure_preserves_existing_dest() {
    let node_a = build_node().await;
    let node_b = build_node().await;
    handshake(&node_b, &node_a).await;
    let a_id = node_a.node_id();

    let adapter_a = Arc::new(MeshBlobAdapter::new("a", Arc::new(Redex::new())));
    node_a.serve_blob_transfer(adapter_a.clone());
    node_b.serve_blob_transfer(Arc::new(MeshBlobAdapter::new("b", Arc::new(Redex::new()))));

    // Seed `dest` with a known good tree.
    let src = TempDir::new("src");
    write(src.path(), "keep.txt", b"original");
    let m_good = store_dir(&adapter_a, src.path()).await.expect("store good");
    let parent = TempDir::new("parent");
    let dest = parent.path().join("dest");
    fetch_dir(&node_b, a_id, &m_good, &dest, 0)
        .await
        .expect("seed dest");
    let before = read_tree(&dest);

    // A manifest whose entry escapes the destination root → reconstruction
    // fails (UnsafePath) after the temp dir is created.
    let bad = DirManifest {
        version: DIR_MANIFEST_VERSION,
        entries: vec![DirEntry {
            path: "../escape.txt".into(),
            kind: EntryKind::File {
                mode: 0o644,
                blob: BlobRef::small("mesh://x", [0xAB; 32], 4).encode(),
            },
        }],
    };
    let m_bad = store_manifest(&adapter_a, &bad).await;

    let err = fetch_dir(&node_b, a_id, &m_bad, &dest, 0).await;
    assert!(err.is_err(), "unsafe manifest path must fail the fetch");

    assert_eq!(read_tree(&dest), before, "dest unchanged after the failure");
    assert!(
        temp_orphans(parent.path(), "dest").is_empty(),
        "temp tree cleaned up on failure"
    );
}

/// A failure when `dest` did not exist must not leave `dest` (nor any
/// temp orphan) behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fetch_dir_failure_does_not_create_dest() {
    let node_a = build_node().await;
    let node_b = build_node().await;
    handshake(&node_b, &node_a).await;
    let a_id = node_a.node_id();

    let adapter_a = Arc::new(MeshBlobAdapter::new("a", Arc::new(Redex::new())));
    node_a.serve_blob_transfer(adapter_a.clone());
    node_b.serve_blob_transfer(Arc::new(MeshBlobAdapter::new("b", Arc::new(Redex::new()))));

    let bad = DirManifest {
        version: DIR_MANIFEST_VERSION,
        entries: vec![DirEntry {
            path: "../escape.txt".into(),
            kind: EntryKind::File {
                mode: 0o644,
                blob: BlobRef::small("mesh://x", [0xCD; 32], 4).encode(),
            },
        }],
    };
    let m_bad = store_manifest(&adapter_a, &bad).await;

    let parent = TempDir::new("parent");
    let dest = parent.path().join("never");
    assert!(fetch_dir(&node_b, a_id, &m_bad, &dest, 0).await.is_err());
    assert!(!dest.exists(), "dest must not exist after a failed fetch");
    assert!(
        temp_orphans(parent.path(), "never").is_empty(),
        "no temp orphan after a failed fetch into a fresh dest"
    );
}

/// Two concurrent fetches into sibling destinations don't collide on
/// their temp paths and each reconstructs its own tree.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_fetch_into_adjacent_dests() {
    let node_a = build_node().await;
    let node_b = build_node().await;
    handshake(&node_b, &node_a).await;
    let a_id = node_a.node_id();

    let adapter_a = Arc::new(MeshBlobAdapter::new("a", Arc::new(Redex::new())));
    node_a.serve_blob_transfer(adapter_a.clone());
    node_b.serve_blob_transfer(Arc::new(MeshBlobAdapter::new("b", Arc::new(Redex::new()))));

    let src1 = TempDir::new("src1");
    write(src1.path(), "x.txt", b"tree-one");
    let m1 = store_dir(&adapter_a, src1.path()).await.expect("store 1");
    let src2 = TempDir::new("src2");
    write(src2.path(), "y.txt", b"tree-two");
    let m2 = store_dir(&adapter_a, src2.path()).await.expect("store 2");

    let parent = TempDir::new("parent");
    let dest_a = parent.path().join("a");
    let dest_b = parent.path().join("b");

    let (r1, r2) = tokio::join!(
        fetch_dir(&node_b, a_id, &m1, &dest_a, 0),
        fetch_dir(&node_b, a_id, &m2, &dest_b, 0),
    );
    r1.expect("fetch a");
    r2.expect("fetch b");

    assert_eq!(read_tree(&dest_a), read_tree(src1.path()), "dest_a == src1");
    assert_eq!(read_tree(&dest_b), read_tree(src2.path()), "dest_b == src2");
    assert!(temp_orphans(parent.path(), "a").is_empty());
    assert!(temp_orphans(parent.path(), "b").is_empty());
}
