//! End-to-end integration test for `net transfer (ls|status|cancel)`.
//!
//! These verbs query a holder's `blob.transfers` engine over the mesh
//! (remote-attach). The holder boots a `Mesh` and installs the engine +
//! introspection RPC via `serve_blob_transfer_rpc`; the CLI then drives
//! `ls` / `status` / `cancel` as subprocesses and asserts the JSON shapes.
//!
//! The holder has no in-flight fetches (a loopback transfer completes too
//! fast to observe deterministically), so this pins the round-trip
//! plumbing against an empty registry: `ls` → empty, `status <id>` →
//! not-found, `cancel <id>` → not-cancelled. The *populated* engine
//! accessors are unit-tested in the substrate (`transfer.rs`); the answer
//! logic + wire codec in `transfer_rpc.rs`.

use std::sync::Arc;

use assert_cmd::Command as AssertCommand;
use tempfile::TempDir;

use net_sdk::dataforts::{MeshBlobAdapter, Redex};
use net_sdk::transport;
use net_sdk::{Mesh, MeshBuilder};

const PSK_HEX: &str = "4242424242424242424242424242424242424242424242424242424242424242";

fn psk() -> [u8; 32] {
    hex::decode(PSK_HEX)
        .expect("psk hex")
        .try_into()
        .expect("32-byte psk")
}

/// Boot a holder serving the `blob.transfers` RPC. Returns the mesh and
/// the RPC serve handle — both kept alive by the caller (dropping the
/// handle would stop answering the RPC).
async fn boot_holder() -> (Mesh, transport::ServeHandle) {
    let mesh = MeshBuilder::new("127.0.0.1:0", &psk())
        .expect("mesh builder")
        .build()
        .await
        .expect("mesh build");
    mesh.start();
    let adapter = Arc::new(MeshBlobAdapter::new("holder", Arc::new(Redex::new())));
    let serve = transport::serve_blob_transfer_rpc(&mesh, adapter).expect("serve transfers rpc");
    (mesh, serve)
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
async fn ls_status_cancel_round_trip_over_rpc() {
    let (holder, _serve) = boot_holder().await;
    let home = TempDir::new().expect("home");

    // ls → empty registry, but a real RPC round-trip (exit 0, valid JSON).
    let mut args = vec!["ls".into(), "--output".into(), "json".into()];
    args.extend(attach(&holder));
    let (code, stdout, stderr) = run_transfer(&home, args).await;
    assert_eq!(code, 0, "ls failed: stderr={stderr}\nstdout={stdout}");
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("non-JSON stdout ({e}): {stdout}"));
    assert_eq!(parsed["transfer_count"], 0, "stdout={stdout}");
    assert!(parsed["transfers"].as_array().expect("transfers array").is_empty());

    // status <id> → not found (no such pending transfer), exit 0.
    let mut args = vec![
        "status".into(),
        "0x42".into(),
        "--output".into(),
        "json".into(),
    ];
    args.extend(attach(&holder));
    let (code, stdout, stderr) = run_transfer(&home, args).await;
    assert_eq!(code, 0, "status failed: stderr={stderr}");
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("parse status");
    assert_eq!(parsed["transfer_id"], 0x42);
    assert_eq!(parsed["found"], false, "stdout={stdout}");

    // cancel <id> → nothing to cancel, exit 0.
    let mut args = vec![
        "cancel".into(),
        "0x42".into(),
        "--output".into(),
        "json".into(),
    ];
    args.extend(attach(&holder));
    let (code, stdout, stderr) = run_transfer(&home, args).await;
    assert_eq!(code, 0, "cancel failed: stderr={stderr}");
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("parse cancel");
    assert_eq!(parsed["cancelled"], false, "stdout={stdout}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ls_without_attach_exits_invalid_args() {
    // ls is a remote verb now; with no holder target it's a typed
    // InvalidArgs (exit 2) before any connection.
    let home = TempDir::new().expect("home");
    let (code, _stdout, _stderr) =
        run_transfer(&home, vec!["ls".into(), "--output".into(), "json".into()]).await;
    assert_eq!(code, 2, "expected InvalidArgs exit code for ls without attach");
}
