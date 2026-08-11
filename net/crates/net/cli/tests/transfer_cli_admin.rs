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

/// Boot a holder serving the `blob.transfers` RPC under `policy`.
/// Returns the mesh and the RPC serve handle — both kept alive by the
/// caller (dropping the handle would stop answering the RPC).
async fn boot_holder_with(policy: transport::TransferAdminPolicy) -> (Mesh, transport::ServeHandle) {
    let mesh = MeshBuilder::new("127.0.0.1:0", &psk())
        .expect("mesh builder")
        .build()
        .await
        .expect("mesh build");
    // Register the RPC service BEFORE start() (mirrors the aggregator
    // daemon boot order) so the `blob.transfers.requests` channel
    // subscription is wired into the dispatch loop before it spins up.
    let adapter = Arc::new(MeshBlobAdapter::new("holder", Arc::new(Redex::new())));
    let serve = transport::serve_blob_transfer_rpc_with_policy(&mesh, adapter, policy)
        .expect("serve transfers rpc");
    mesh.start();
    (mesh, serve)
}

/// Holder that answers anyone.
///
/// `attach()` below drives the CLI without `--identity`, so it comes up
/// anonymous with an unpredictable node id — nothing a holder could
/// name in an allowlist. Proving the round-trip plumbing on its own
/// therefore uses the open policy.
///
/// The allowlist path is not untested for it:
/// `a_named_operator_identity_is_admitted_by_a_node_id_allowlist`
/// drives the real operator workflow end to end, and
/// `closed_policy_refuses_remote_administration` covers the refusal.
async fn boot_holder() -> (Mesh, transport::ServeHandle) {
    boot_holder_with(transport::TransferAdminPolicy::AnyAdmittedPeer).await
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
    assert!(parsed["transfers"]
        .as_array()
        .expect("transfers array")
        .is_empty());

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

/// SEC-03 / AUTH-05 at the CLI boundary. A holder under the default
/// policy answers a non-operator with a refusal, not with its transfer
/// list — and the CLI surfaces that as a failure rather than printing
/// an empty list, which would read as "this node is idle".
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn closed_policy_refuses_remote_administration() {
    let (holder, _serve) = boot_holder_with(transport::TransferAdminPolicy::Closed).await;
    let home = TempDir::new().expect("home");

    for verb in [
        vec!["ls".to_string()],
        vec!["status".to_string(), "0x42".to_string()],
        vec!["cancel".to_string(), "0x42".to_string()],
    ] {
        let label = verb.join(" ");
        let mut args = verb;
        args.extend(["--output".to_string(), "json".to_string()]);
        args.extend(attach(&holder));
        let (code, stdout, stderr) = run_transfer(&home, args).await;
        assert_ne!(
            code, 0,
            "`transfer {label}` succeeded against a Closed holder — \
             stdout={stdout} stderr={stderr}"
        );
        let combined = format!("{stdout}{stderr}");
        assert!(
            combined.contains("not authorized"),
            "`transfer {label}` failed without saying why; an operator who \
             forgot to name themselves needs to be able to tell this from a \
             network fault. output={combined}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ls_without_attach_exits_invalid_args() {
    // ls is a remote verb now; with no holder target it's a typed
    // InvalidArgs (exit 2) before any connection.
    let home = TempDir::new().expect("home");
    let (code, _stdout, _stderr) =
        run_transfer(&home, vec!["ls".into(), "--output".into(), "json".into()]).await;
    assert_eq!(
        code, 2,
        "expected InvalidArgs exit code for ls without attach"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn status_rejects_non_numeric_id() {
    // The transfer-id is parsed to u64 before remote-attach is resolved, so
    // a non-numeric id is a typed InvalidArgs (exit 2) with no holder.
    let home = TempDir::new().expect("home");
    let (code, _stdout, _stderr) =
        run_transfer(&home, vec!["status".into(), "not-an-id".into()]).await;
    assert_eq!(
        code, 2,
        "expected InvalidArgs exit code for a bad transfer-id"
    );
}

/// Task #30. The operator workflow the node-id allowlists depend on,
/// end to end: generate an identity, read the `node_id` off
/// `identity show`, name it on the holder, and attach with
/// `--identity`.
///
/// Before this, `--identity` set the *operator* identity used for
/// signing but the remote-attach mesh always came up anonymous, so the
/// allowlists this release introduces were unsatisfiable from the CLI
/// — the secure configuration existed and could not be reached by the
/// tool operators use. This test fails if that regresses, which the
/// `AnyAdmittedPeer` round-trip test above cannot detect.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_named_operator_identity_is_admitted_by_a_node_id_allowlist() {
    let home = TempDir::new().expect("home");
    let id_path = home.path().join("operator.toml");

    // 1. Generate an identity.
    let mut cmd = cli_cmd(&home);
    cmd.args([
        "identity",
        "generate",
        "--out",
        id_path.to_str().expect("utf-8 path"),
    ]);
    let out = cmd.output().expect("identity generate");
    assert!(
        out.status.success(),
        "identity generate failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // 2. Read its mesh node id the way an operator would.
    let mut cmd = cli_cmd(&home);
    cmd.args([
        "identity",
        "show",
        id_path.to_str().expect("utf-8 path"),
        "--output",
        "json",
    ]);
    let out = cmd.output().expect("identity show");
    assert!(
        out.status.success(),
        "identity show failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let shown: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("identity show emits JSON");
    let node_id_hex = shown["node_id_hex"]
        .as_str()
        .expect("identity show must publish node_id_hex — without it an operator \
                 cannot populate an allowlist without reimplementing the derivation")
        .to_string();
    let node_id = u64::from_str_radix(
        node_id_hex.trim_start_matches("0x"),
        16,
    )
    .expect("node_id_hex parses as hex");

    // 3. Holder names exactly that node, and nobody else.
    let (holder, _serve) =
        boot_holder_with(transport::TransferAdminPolicy::operators([node_id])).await;

    // 4. Attaching WITH the identity is admitted.
    let mut args = vec!["ls".to_string(), "--output".to_string(), "json".to_string()];
    args.extend([
        "--identity".to_string(),
        id_path.to_str().expect("utf-8 path").to_string(),
    ]);
    args.extend(attach(&holder));
    let (code, stdout, stderr) = run_transfer(&home, args).await;
    assert_eq!(
        code, 0,
        "the named operator identity was refused by its own allowlist — \
         the derived node_id and the attached mesh's node_id disagree. \
         stdout={stdout} stderr={stderr}"
    );
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("non-JSON stdout ({e}): {stdout}"));
    assert_eq!(parsed["transfer_count"], 0);

    // 5. Negative control: the same holder, no --identity. The CLI
    //    comes up anonymous and must be refused — otherwise step 4
    //    proves nothing about the identity being the reason.
    let mut args = vec!["ls".to_string(), "--output".to_string(), "json".to_string()];
    args.extend(attach(&holder));
    let (code, stdout, stderr) = run_transfer(&home, args).await;
    assert_ne!(
        code, 0,
        "an anonymous attach was admitted by a node-id allowlist. stdout={stdout} stderr={stderr}"
    );
}
