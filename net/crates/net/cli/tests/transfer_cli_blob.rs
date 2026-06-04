//! End-to-end integration test for `net transfer (recv-blob|send-blob)`.
//!
//! Boots a holder `Mesh` in-process that serves a stored blob over the
//! transfer engine, then drives the `net-mesh` binary as a subprocess
//! (`assert_cmd`) with the holder's bootstrap triple injected as
//! `--node-addr` / `--node-pubkey` / `--node-id` / `--psk-hex` — the same
//! routed-handshake remote-attach path the `net aggregator` tests use.
//!
//! Pins:
//! - `recv-blob` fetches the holder's blob and writes it byte-for-byte to
//!   `--out`, exiting 0 with a JSON summary.
//! - `send-blob` (purely local, no network) computes the SAME
//!   content-addressed reference the holder stored under — a cross-check
//!   that the publish side and the fetch side agree on the address.
//! - a bogus `--blob-ref` and a missing-attach invocation map to the
//!   typed exit codes.

use std::sync::Arc;

use assert_cmd::Command as AssertCommand;
use tempfile::TempDir;

use net_sdk::dataforts::{BlobAdapter, MeshBlobAdapter, Redex};
use net_sdk::transport::{self, BlobRef, Encoding};
use net_sdk::{Mesh, MeshBuilder};

const PSK_HEX: &str = "4242424242424242424242424242424242424242424242424242424242424242";

fn psk() -> [u8; 32] {
    let bytes = hex::decode(PSK_HEX).expect("psk hex");
    bytes.try_into().expect("32-byte psk")
}

/// Deterministic, content-addressable payload (seed varies the bytes).
fn payload(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| ((i.wrapping_mul(31) + seed as usize) % 251) as u8)
        .collect()
}

/// Boot a holder `Mesh` serving a single stored blob; return the mesh
/// (kept alive by the caller) plus the blob's encoded reference.
async fn boot_holder_with_blob(bytes: &[u8]) -> (Mesh, BlobRef) {
    let mesh = MeshBuilder::new("127.0.0.1:0", &psk())
        .expect("mesh builder")
        .build()
        .await
        .expect("mesh build");
    mesh.start();

    let adapter = Arc::new(MeshBlobAdapter::new("holder", Arc::new(Redex::new())));
    transport::serve_blob_transfer(&mesh, adapter.clone());

    let blob_ref = transport::chunk_payload(bytes)
        .expect("chunk payload")
        .into_blob_ref("mesh://holder", Encoding::Replicated)
        .expect("blob ref");
    adapter.store(&blob_ref, bytes).await.expect("store blob");

    (mesh, blob_ref)
}

fn cli_cmd(home_dir: &TempDir) -> AssertCommand {
    let mut cmd = AssertCommand::cargo_bin("net-mesh").expect("cargo_bin");
    cmd.env("HOME", home_dir.path())
        .env("XDG_CONFIG_HOME", home_dir.path())
        .env("USERPROFILE", home_dir.path());
    cmd
}

/// Run `net-mesh transfer <args...>` and return `(code, stdout, stderr)`.
/// `assert_cmd` is blocking, so it runs on a blocking thread to keep the
/// tokio runtime free.
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
async fn recv_blob_fetches_byte_for_byte() {
    let bytes = payload(200 * 1024, 7);
    let (holder, blob_ref) = boot_holder_with_blob(&bytes).await;
    let encoded = hex::encode(blob_ref.encode());

    let home = TempDir::new().expect("home");
    let out_dir = TempDir::new().expect("out");
    let out_path = out_dir.path().join("received.bin");

    let mut args = vec![
        "recv-blob".into(),
        "--blob-ref".into(),
        encoded,
        "--out".into(),
        out_path.display().to_string(),
        "--output".into(),
        "json".into(),
    ];
    args.extend(attach(&holder));

    let (code, stdout, stderr) = run_transfer(&home, args).await;
    assert_eq!(
        code, 0,
        "recv-blob failed: stderr={stderr}\nstdout={stdout}"
    );

    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("non-JSON stdout ({e}): {stdout}"));
    assert_eq!(parsed["bytes"], bytes.len() as u64, "stdout={stdout}");

    let got = std::fs::read(&out_path).expect("read out");
    assert_eq!(got, bytes, "received blob differs from the holder's blob");

    // The atomic write must not leave a `.partial` sibling on success.
    let partial = out_dir.path().join("received.bin.partial");
    assert!(!partial.exists(), "stray .partial left behind: {partial:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn send_blob_computes_the_same_reference_the_holder_stored() {
    // The publish side (`send-blob`) and the storage side
    // (`chunk_payload` → `store`) must derive the identical reference, or
    // `recv-blob` could never name the holder's content. Compute the
    // holder's ref directly, then assert the CLI prints the same one.
    let bytes = payload(200 * 1024, 7);
    let expected = transport::chunk_payload(&bytes)
        .expect("chunk payload")
        .into_blob_ref("mesh://transfer", Encoding::Replicated)
        .expect("blob ref");
    let expected_hex = hex::encode(expected.encode());

    let home = TempDir::new().expect("home");
    let src_dir = TempDir::new().expect("src");
    let src = src_dir.path().join("payload.bin");
    std::fs::write(&src, &bytes).expect("write src");

    let (code, stdout, stderr) = run_transfer(
        &home,
        vec![
            "send-blob".into(),
            src.display().to_string(),
            "--output".into(),
            "json".into(),
        ],
    )
    .await;
    assert_eq!(code, 0, "send-blob failed: stderr={stderr}");
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("parse send-blob");
    assert_eq!(parsed["blob_ref"], expected_hex, "ref mismatch: {stdout}");
    assert_eq!(parsed["size"], bytes.len() as u64);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recv_blob_rejects_bogus_ref() {
    // A non-hex `--blob-ref` is a typed InvalidArgs (exit 2), surfaced
    // before any network work. No holder needed.
    let home = TempDir::new().expect("home");
    let out = TempDir::new().expect("out").path().join("x.bin");
    let (code, _stdout, _stderr) = run_transfer(
        &home,
        vec![
            "recv-blob".into(),
            "--blob-ref".into(),
            "not-hex".into(),
            "--out".into(),
            out.display().to_string(),
            "--node-addr".into(),
            "127.0.0.1:1".into(),
            "--node-pubkey".into(),
            PSK_HEX.into(),
            "--node-id".into(),
            "1".into(),
            "--psk-hex".into(),
            PSK_HEX.into(),
        ],
    )
    .await;
    assert_eq!(code, 2, "expected InvalidArgs exit code");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recv_blob_without_attach_is_invalid_args() {
    // A well-formed `--blob-ref` but no remote-attach target (and no
    // profile defaults under the empty temp HOME) is a typed InvalidArgs
    // (exit 2): `require_remote_attach` rejects before any network work.
    let home = TempDir::new().expect("home");
    let out = TempDir::new().expect("out").path().join("x.bin");
    let (code, _stdout, stderr) = run_transfer(
        &home,
        vec![
            "recv-blob".into(),
            "--blob-ref".into(),
            // 32-byte all-zero hash — parses as a valid Small ref, so the
            // failure can only come from the missing holder target.
            "0".repeat(64),
            "--out".into(),
            out.display().to_string(),
        ],
    )
    .await;
    assert_eq!(code, 2, "expected InvalidArgs exit code; stderr={stderr}");
}
