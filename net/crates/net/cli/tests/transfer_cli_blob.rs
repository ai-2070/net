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

/// Like [`run_transfer`] but pipes `stdin` to the process — for the
/// `send-blob -` (read from stdin) path.
async fn run_transfer_stdin(
    home: &TempDir,
    args: Vec<String>,
    stdin: Vec<u8>,
) -> (i32, String, String) {
    let bin = cli_cmd(home);
    tokio::task::spawn_blocking(move || {
        let mut cmd = bin;
        cmd.arg("transfer");
        cmd.args(&args);
        cmd.write_stdin(stdin);
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
async fn recv_blob_streams_a_multi_chunk_blob_byte_for_byte() {
    // 10 MiB spans multiple 4 MiB chunks, so this exercises the streamed
    // multi-chunk path (`fetch_blob_stream` → `AtomicFileWriter`) end to
    // end rather than the single-chunk fast path above. The receiver never
    // holds the whole blob — chunks land on disk as they arrive — but the
    // reconstructed file must still match the holder's bytes exactly.
    let bytes = payload(10 * 1024 * 1024, 13);
    let (holder, blob_ref) = boot_holder_with_blob(&bytes).await;
    let encoded = hex::encode(blob_ref.encode());

    let home = TempDir::new().expect("home");
    let out_dir = TempDir::new().expect("out");
    let out_path = out_dir.path().join("big.bin");

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
    assert_eq!(
        got, bytes,
        "multi-chunk blob differs from the holder's blob"
    );
    assert!(
        !out_dir.path().join("big.bin.partial").exists(),
        "stray .partial left behind after a successful multi-chunk fetch"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recv_blob_failure_leaves_partial_not_target() {
    // Boot a holder whose engine is installed and serving, but which does
    // NOT hold the content we ask for. The fetch fails (holder NotFound),
    // so the verb must exit non-zero, leave NO committed `--out`, and leave
    // the `<out>.partial` behind for inspection (TRANSFER.md §5).
    let (holder, _present) = boot_holder_with_blob(&payload(1024, 1)).await;
    let missing = transport::chunk_payload(&payload(8 * 1024, 99))
        .expect("chunk payload")
        .into_blob_ref("mesh://missing", Encoding::Replicated)
        .expect("blob ref");
    let encoded = hex::encode(missing.encode());

    let home = TempDir::new().expect("home");
    let out_dir = TempDir::new().expect("out");
    let out_path = out_dir.path().join("never.bin");

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

    let (code, _stdout, _stderr) = run_transfer(&home, args).await;
    assert_ne!(code, 0, "expected a non-zero exit for a failed fetch");
    assert!(
        !out_path.exists(),
        "the destination must not be committed on failure"
    );
    assert!(
        out_dir.path().join("never.bin.partial").exists(),
        "the .partial must be left in place for inspection on failure"
    );
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn send_blob_multi_chunk_ref_matches_buffered() {
    // A >4 MiB payload spans multiple chunks → Manifest. The streamed
    // send-blob must derive the IDENTICAL reference the buffered
    // chunk_payload path produces, or recv-blob could never name it. Also
    // pins the multi-chunk view shape (≥ 3 chunks, no bare `hash` field).
    let bytes = payload(10 * 1024 * 1024, 21);
    let expected = transport::chunk_payload(&bytes)
        .expect("chunk payload")
        .into_blob_ref("mesh://transfer", Encoding::Replicated)
        .expect("blob ref");
    let expected_hex = hex::encode(expected.encode());

    let home = TempDir::new().expect("home");
    let src_dir = TempDir::new().expect("src");
    let src = src_dir.path().join("big.bin");
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
    assert_eq!(
        parsed["blob_ref"], expected_hex,
        "multi-chunk ref mismatch: {stdout}"
    );
    assert_eq!(parsed["size"], bytes.len() as u64);
    assert!(
        parsed["chunks"].as_u64().expect("chunks") >= 3,
        "expected ≥3 chunks for a 10 MiB payload: {stdout}"
    );
    // Multi-chunk → the bare-hash convenience field is omitted.
    assert!(
        parsed["hash"].is_null(),
        "multi-chunk should omit hash: {stdout}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn send_blob_reads_from_stdin() {
    // `send-blob -` streams the payload from stdin and must derive the same
    // reference as the equivalent file input.
    let bytes = payload(300 * 1024, 5);
    let expected = transport::chunk_payload(&bytes)
        .expect("chunk payload")
        .into_blob_ref("mesh://transfer", Encoding::Replicated)
        .expect("blob ref");
    let expected_hex = hex::encode(expected.encode());

    let home = TempDir::new().expect("home");
    let (code, stdout, stderr) = run_transfer_stdin(
        &home,
        vec![
            "send-blob".into(),
            "-".into(),
            "--output".into(),
            "json".into(),
        ],
        bytes.clone(),
    )
    .await;
    assert_eq!(code, 0, "send-blob - failed: stderr={stderr}");
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("parse send-blob");
    assert_eq!(
        parsed["blob_ref"], expected_hex,
        "stdin ref mismatch: {stdout}"
    );
    assert_eq!(parsed["size"], bytes.len() as u64);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn send_blob_store_then_recv_round_trip() {
    // End-to-end tie of Gap B (streamed send) to Gap A (streamed recv):
    // `send-blob --store` stages a multi-chunk blob's chunks to disk; a
    // holder rooted at that same store dir serves them; `recv-blob` then
    // fetches the blob back byte-for-byte.
    let bytes = payload(6 * 1024 * 1024, 33); // multi-chunk
    let home = TempDir::new().expect("home");
    let store_dir = TempDir::new().expect("store");
    let src_dir = TempDir::new().expect("src");
    let src = src_dir.path().join("payload.bin");
    std::fs::write(&src, &bytes).expect("write src");

    // 1. Stage the bytes into an on-disk store.
    let (code, stdout, stderr) = run_transfer(
        &home,
        vec![
            "send-blob".into(),
            src.display().to_string(),
            "--store".into(),
            store_dir.path().display().to_string(),
            "--output".into(),
            "json".into(),
        ],
    )
    .await;
    assert_eq!(code, 0, "send-blob --store failed: stderr={stderr}");
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("parse send-blob");
    assert_eq!(
        parsed["staged_to"],
        store_dir.path().display().to_string(),
        "stdout={stdout}"
    );
    let blob_ref_hex = parsed["blob_ref"].as_str().expect("blob_ref").to_string();

    // 2. Boot a holder whose persistent adapter is rooted at the same store
    //    dir (same id as the CLI's `send-blob` adapter) so it serves the
    //    staged chunks.
    let mesh = MeshBuilder::new("127.0.0.1:0", &psk())
        .expect("mesh builder")
        .build()
        .await
        .expect("mesh build");
    mesh.start();
    let redex = Arc::new(Redex::new().with_persistent_dir(store_dir.path()));
    let adapter = Arc::new(MeshBlobAdapter::new("send-blob", redex).with_persistent(true));
    transport::serve_blob_transfer(&mesh, adapter);

    // 3. Fetch it back and compare byte-for-byte.
    let out_dir = TempDir::new().expect("out");
    let out_path = out_dir.path().join("got.bin");
    let mut args = vec![
        "recv-blob".into(),
        "--blob-ref".into(),
        blob_ref_hex,
        "--out".into(),
        out_path.display().to_string(),
        "--output".into(),
        "json".into(),
    ];
    args.extend(attach(&mesh));
    let (code, stdout, stderr) = run_transfer(&home, args).await;
    assert_eq!(
        code, 0,
        "recv-blob failed: stderr={stderr}\nstdout={stdout}"
    );
    let got = std::fs::read(&out_path).expect("read out");
    assert_eq!(got, bytes, "round-tripped blob differs from source");
}
