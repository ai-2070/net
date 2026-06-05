//! End-to-end integration test for `net transfer (recv-dir|send-dir)`.
//!
//! Boots a holder `Mesh` serving a directory (manifest + chunks) and
//! drives `net-mesh transfer recv-dir` as a subprocess over the
//! routed-attach path, asserting the reconstructed tree matches the
//! source byte-for-byte and that the reconstruction was atomic (no
//! sibling temp dir left behind). Also cross-checks that `send-dir`
//! computes the same manifest reference the holder published.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use assert_cmd::Command as AssertCommand;
use tempfile::TempDir;

use net_sdk::dataforts::{MeshBlobAdapter, Redex};
use net_sdk::transport::{self, BlobRef};
use net_sdk::{Mesh, MeshBuilder};

const PSK_HEX: &str = "4242424242424242424242424242424242424242424242424242424242424242";

fn psk() -> [u8; 32] {
    hex::decode(PSK_HEX)
        .expect("psk hex")
        .try_into()
        .expect("32-byte psk")
}

/// Build a small but non-trivial source tree: multiple files across
/// nested subdirectories with distinct content. Enough to exercise the
/// manifest build + multi-leaf fetch + atomic rename without the
/// loopback-congestion flakiness a 1000-file flood would court (same
/// lesson as the batched-ingress integrity test — assert correctness,
/// keep the offered load light and deterministic).
fn build_source_tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut expected = BTreeMap::new();
    let files = [
        ("README.md", "top-level readme\n".repeat(8)),
        ("src/main.rs", "fn main() {}\n".repeat(64)),
        ("src/lib.rs", "pub fn lib() {}\n".repeat(64)),
        ("src/util/mod.rs", "pub mod util;\n".repeat(32)),
        (
            "data/blob.bin",
            "binary-ish payload \u{1f9ea}\n".repeat(256),
        ),
        ("data/nested/deep.txt", "deep file\n".repeat(128)),
    ];
    for (rel, content) in files {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir");
        std::fs::write(&path, content.as_bytes()).expect("write file");
        expected.insert(rel.replace('\\', "/"), content.into_bytes());
    }
    expected
}

/// Read every file under `root` into a `rel-path -> bytes` map for
/// content comparison (paths normalized to `/`).
fn read_tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).expect("read_dir") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.is_dir() {
                walk(base, &path, out);
            } else {
                let rel = path
                    .strip_prefix(base)
                    .expect("strip prefix")
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, std::fs::read(&path).expect("read file"));
            }
        }
    }
    walk(root, root, &mut out);
    out
}

async fn boot_holder_with_dir(src: &Path) -> (Mesh, BlobRef) {
    let mesh = MeshBuilder::new("127.0.0.1:0", &psk())
        .expect("mesh builder")
        .build()
        .await
        .expect("mesh build");
    mesh.start();
    let adapter = Arc::new(MeshBlobAdapter::new("holder", Arc::new(Redex::new())));
    transport::serve_blob_transfer(&mesh, adapter.clone());
    let manifest_ref = transport::store_dir(&adapter, src)
        .await
        .expect("store_dir");
    (mesh, manifest_ref)
}

fn cli_cmd(home_dir: &TempDir) -> AssertCommand {
    let mut cmd = AssertCommand::cargo_bin("net-mesh").expect("cargo_bin");
    cmd.env("HOME", home_dir.path())
        .env("XDG_CONFIG_HOME", home_dir.path())
        .env("USERPROFILE", home_dir.path());
    cmd
}

async fn run_transfer(home: &TempDir, args: Vec<String>) -> (i32, String, String) {
    let bin = cli_cmd(home);
    tokio::task::spawn_blocking(move || {
        let mut cmd = bin;
        cmd.arg("transfer");
        cmd.args(&args);
        let output = cmd.output().expect("invoke net-mesh");
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    })
    .await
    .expect("spawn_blocking")
}

fn attach(holder: &Mesh) -> Vec<String> {
    vec![
        "--node-addr".into(),
        holder.local_addr().to_string(),
        "--node-pubkey".into(),
        hex::encode(holder.public_key()),
        "--node-id".into(),
        holder.node_id().to_string(),
        "--psk-hex".into(),
        PSK_HEX.into(),
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recv_dir_reconstructs_tree_atomically() {
    let src_dir = TempDir::new().expect("src");
    let expected = build_source_tree(src_dir.path());

    let (holder, manifest_ref) = boot_holder_with_dir(src_dir.path()).await;
    let encoded = hex::encode(manifest_ref.encode());

    let home = TempDir::new().expect("home");
    let out_parent = TempDir::new().expect("out parent");
    let out_path = out_parent.path().join("received_dir");

    let mut args = vec![
        "recv-dir".into(),
        "--remote-ref".into(),
        encoded,
        "--out".into(),
        out_path.display().to_string(),
        "--output".into(),
        "json".into(),
    ];
    args.extend(attach(&holder));

    let (code, stdout, stderr) = run_transfer(&home, args).await;
    assert_eq!(code, 0, "recv-dir failed: stderr={stderr}\nstdout={stdout}");

    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("non-JSON stdout ({e}): {stdout}"));
    assert_eq!(parsed["files"], expected.len() as u64, "stdout={stdout}");
    assert_eq!(parsed["atomic"], true);

    // Reconstructed tree matches the source byte-for-byte.
    let got = read_tree(&out_path);
    assert_eq!(got, expected, "reconstructed tree differs from source");

    // Atomicity: `fetch_dir` reconstructs in a sibling temp dir then
    // renames. On success nothing temp must remain beside the target.
    let leftovers: Vec<_> = std::fs::read_dir(out_parent.path())
        .expect("read out parent")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n != "received_dir")
        .collect();
    assert!(
        leftovers.is_empty(),
        "stray temp entries left beside target: {leftovers:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recv_dir_streams_a_large_multi_chunk_leaf() {
    // A directory with one >4 MiB leaf exercises the streamed per-leaf
    // write path (`fetch_blob_to_file`): the large file is reconstructed by
    // writing chunks to disk as they arrive, never buffered whole. A small
    // sibling covers the unchanged buffered (Small) fast path in the same
    // run. Both must reconstruct byte-for-byte.
    let src_dir = TempDir::new().expect("src");
    let big: Vec<u8> = (0..(6 * 1024 * 1024usize))
        .map(|i| (i.wrapping_mul(31) % 251) as u8)
        .collect();
    std::fs::write(src_dir.path().join("big.bin"), &big).expect("write big");
    std::fs::write(src_dir.path().join("small.txt"), b"hello\n").expect("write small");

    let (holder, manifest_ref) = boot_holder_with_dir(src_dir.path()).await;
    let encoded = hex::encode(manifest_ref.encode());

    let home = TempDir::new().expect("home");
    let out_parent = TempDir::new().expect("out parent");
    let out_path = out_parent.path().join("received_dir");

    let mut args = vec![
        "recv-dir".into(),
        "--remote-ref".into(),
        encoded,
        "--out".into(),
        out_path.display().to_string(),
        "--output".into(),
        "json".into(),
    ];
    args.extend(attach(&holder));

    let (code, stdout, stderr) = run_transfer(&home, args).await;
    assert_eq!(code, 0, "recv-dir failed: stderr={stderr}\nstdout={stdout}");

    let got_big = std::fs::read(out_path.join("big.bin")).expect("read big");
    assert_eq!(got_big, big, "large streamed leaf differs from source");
    let got_small = std::fs::read(out_path.join("small.txt")).expect("read small");
    assert_eq!(got_small, b"hello\n", "small leaf differs from source");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn send_dir_computes_matching_manifest_ref() {
    // `send-dir` and the holder's `store_dir` must derive the identical
    // content-addressed manifest reference (store_dir is deterministic
    // for a fixed tree), or `recv-dir` could never name the published
    // directory.
    let src_dir = TempDir::new().expect("src");
    let _expected_tree = build_source_tree(src_dir.path());

    let adapter = Arc::new(MeshBlobAdapter::new("ref", Arc::new(Redex::new())));
    let expected_ref = transport::store_dir(&adapter, src_dir.path())
        .await
        .expect("store_dir");
    let expected_hex = hex::encode(expected_ref.encode());

    let home = TempDir::new().expect("home");
    let (code, stdout, stderr) = run_transfer(
        &home,
        vec![
            "send-dir".into(),
            src_dir.path().display().to_string(),
            "--output".into(),
            "json".into(),
        ],
    )
    .await;
    assert_eq!(code, 0, "send-dir failed: stderr={stderr}");
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("parse send-dir");
    assert_eq!(
        parsed["remote_ref"], expected_hex,
        "manifest ref mismatch: {stdout}"
    );
}
