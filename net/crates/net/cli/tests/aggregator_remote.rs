//! End-to-end remote-attach integration test for `net aggregator`.
//!
//! Boots an aggregator-daemon in-process via `boot()`, then drives
//! the CLI binary as a subprocess (`assert_cmd`) with the daemon's
//! `--print-bootstrap` triple injected as `--node-addr` /
//! `--node-pubkey` / `--node-id` / `--psk-hex`. Each verb's exit
//! code + stdout JSON shape is asserted against the daemon's
//! registry state.
//!
//! # Substrate gap (positive-path tests are `#[ignore]`)
//!
//! The substrate's dispatch loop drops direct handshake msg1
//! packets from peers it hasn't pre-`accept()`ed (see
//! `mesh.rs:2409-2417` and the `pending_direct_initiators`
//! comment at `mesh.rs:3247-3250` — the responder-side registry
//! is explicitly deferred). Every CLI subprocess invocation
//! generates a fresh ephemeral identity, so the daemon can't
//! pre-`accept` it. Until that gap closes, only the negative-
//! path tests (which exit before the handshake) run by default.
//!
//! Pin set:
//! - `ls --remote` against a daemon with two static groups   `#[ignore]`
//! - `spawn` against a configured template adds a third      `#[ignore]`
//! - `query` against the template's `source_subnet`          `#[ignore]`
//! - `scale` (interim Unregister + Spawn) resizes a group    `#[ignore]`
//! - bad pubkey / missing flag map to typed exit codes

use std::time::Duration;

use assert_cmd::Command as AssertCommand;
use net_aggregator_daemon::{boot, drain_registry, BootedDaemon, Cli};
use tempfile::{NamedTempFile, TempDir};
use tokio::io::AsyncWriteExt;

const PSK_HEX: &str = "4242424242424242424242424242424242424242424242424242424242424242";

async fn write_temp_config(toml: &str) -> NamedTempFile {
    let mut f = NamedTempFile::new().expect("tempfile");
    let path = f.path().to_path_buf();
    {
        let mut handle = tokio::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&path)
            .await
            .expect("open tempfile");
        handle
            .write_all(toml.as_bytes())
            .await
            .expect("write tempfile");
        handle.flush().await.expect("flush tempfile");
    }
    // Keep the NamedTempFile guard alive so the file outlives this
    // helper. `let _` would drop it.
    let _ = &mut f;
    f
}

/// Build a `net-mesh` subprocess command with the remote-attach
/// flags pointed at the booted daemon. `HOME` / `XDG_CONFIG_HOME`
/// are redirected to a per-test temp dir so the CLI doesn't read
/// the operator's local config.
fn cli_cmd(_booted: &BootedDaemon, home_dir: &TempDir) -> AssertCommand {
    let mut cmd = AssertCommand::cargo_bin("net-mesh").expect("cargo_bin");
    cmd.env("HOME", home_dir.path())
        .env("XDG_CONFIG_HOME", home_dir.path())
        .env("USERPROFILE", home_dir.path());
    cmd.arg("aggregator");
    cmd
}

fn attach_args(booted: &BootedDaemon, vec: &mut Vec<String>) {
    vec.push("--node-addr".into());
    vec.push(booted.bound_addr.to_string());
    vec.push("--node-pubkey".into());
    vec.push(hex::encode(booted.public_key));
    vec.push("--node-id".into());
    vec.push(booted.mesh.node_id().to_string());
    vec.push("--psk-hex".into());
    vec.push(PSK_HEX.into());
}

/// Run `net-mesh aggregator <verb> [args...]` and return
/// `(exit_code, stdout, stderr)`. Wraps `assert_cmd` in
/// `spawn_blocking` so the tokio runtime doesn't deadlock.
async fn run_cli(
    booted: &BootedDaemon,
    home_dir: &TempDir,
    verb: &str,
    extra: &[&str],
) -> (i32, String, String) {
    let mut argv: Vec<String> = vec![verb.into()];
    for s in extra {
        argv.push((*s).into());
    }
    attach_args(booted, &mut argv);
    let bin_cmd = cli_cmd(booted, home_dir);
    let argv_owned = argv.clone();
    tokio::task::spawn_blocking(move || {
        let mut cmd = bin_cmd;
        cmd.args(&argv_owned);
        let output = cmd.output().expect("invoke net-mesh");
        let code = output.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        (code, stdout, stderr)
    })
    .await
    .expect("spawn_blocking")
}

async fn boot_daemon(toml: &str) -> (BootedDaemon, NamedTempFile) {
    let cfg = write_temp_config(toml).await;
    let cli = Cli {
        config: cfg.path().to_path_buf(),
        listen: None,
        verbose: 0,
        print_bootstrap: false,
    };
    let booted = boot(cli).await.expect("daemon boot");
    booted.mesh.start();
    (booted, cfg)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "blocked on substrate direct-handshake responder gap; see task #102"]
async fn ls_remote_lists_configured_groups() {
    let toml = format!(
        r#"
            listen = "127.0.0.1:0"
            psk_hex = "{PSK_HEX}"

            [[group]]
            name = "alpha"
            source_subnet = "3.7"
            fold_kinds = [1]
            replica_count = 2
            summary_interval_ms = 50

            [[group]]
            name = "beta"
            source_subnet = "3.8"
            fold_kinds = [1]
            replica_count = 1
            summary_interval_ms = 50
        "#
    );
    let home = TempDir::new().expect("home tempdir");
    let (booted, _cfg) = boot_daemon(&toml).await;

    let (code, stdout, stderr) = run_cli(&booted, &home, "ls", &["--remote"]).await;
    assert_eq!(code, 0, "ls --remote failed: stderr={stderr}");
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("non-JSON stdout ({e}): {stdout}"));
    assert_eq!(parsed["group_count"], 2, "stdout={stdout}");
    let names: Vec<&str> = parsed["groups"]
        .as_array()
        .expect("groups array")
        .iter()
        .map(|g| g["name"].as_str().expect("name string"))
        .collect();
    assert_eq!(names, vec!["alpha", "beta"]);

    drain_registry(&booted.registry).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "blocked on substrate direct-handshake responder gap; see task #102"]
async fn spawn_against_template_adds_a_group() {
    let toml = format!(
        r#"
            listen = "127.0.0.1:0"
            psk_hex = "{PSK_HEX}"

            [[template]]
            name = "primary"
            source_subnet = "3.7"
            fold_kinds = [1]
            summary_interval_ms = 50
        "#
    );
    let home = TempDir::new().expect("home tempdir");
    let (booted, _cfg) = boot_daemon(&toml).await;

    let (code, stdout, stderr) = run_cli(
        &booted,
        &home,
        "spawn",
        &[
            "--template",
            "primary",
            "--name",
            "dynamic",
            "--replica-count",
            "2",
        ],
    )
    .await;
    assert_eq!(code, 0, "spawn failed: stderr={stderr}");
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("non-JSON stdout ({e}): {stdout}"));
    assert_eq!(parsed["name"], "dynamic");
    assert_eq!(parsed["replica_count"], 2);

    // `ls --remote` shows the new group.
    let (code, stdout, _) = run_cli(&booted, &home, "ls", &["--remote"]).await;
    assert_eq!(code, 0);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("parse ls");
    assert_eq!(parsed["group_count"], 1);
    assert_eq!(parsed["groups"][0]["name"], "dynamic");

    drain_registry(&booted.registry).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "blocked on substrate direct-handshake responder gap; see task #102"]
async fn scale_resizes_existing_group() {
    let toml = format!(
        r#"
            listen = "127.0.0.1:0"
            psk_hex = "{PSK_HEX}"

            [[template]]
            name = "primary"
            source_subnet = "3.7"
            fold_kinds = [1]
            summary_interval_ms = 50

            [[group]]
            name = "alpha"
            source_subnet = "3.7"
            fold_kinds = [1]
            replica_count = 2
            summary_interval_ms = 50
        "#
    );
    let home = TempDir::new().expect("home tempdir");
    let (booted, _cfg) = boot_daemon(&toml).await;

    let (code, stdout, stderr) = run_cli(
        &booted,
        &home,
        "scale",
        &[
            "--name",
            "alpha",
            "--template",
            "primary",
            "--replica-count",
            "4",
        ],
    )
    .await;
    assert_eq!(code, 0, "scale failed: stderr={stderr}");
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("parse scale");
    assert_eq!(parsed["replica_count"], 4);
    assert_eq!(parsed["name"], "alpha");

    drain_registry(&booted.registry).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "blocked on substrate direct-handshake responder gap; see task #102"]
async fn query_returns_summary_for_configured_group() {
    let toml = format!(
        r#"
            listen = "127.0.0.1:0"
            psk_hex = "{PSK_HEX}"

            [[group]]
            name = "alpha"
            source_subnet = "3.7"
            fold_kinds = [1]
            replica_count = 1
            summary_interval_ms = 50
        "#
    );
    let home = TempDir::new().expect("home tempdir");
    let (booted, _cfg) = boot_daemon(&toml).await;
    // Give the aggregator one tick to produce a summary before we
    // query for it. Without this, `query_latest` returns an empty
    // Vec because the in-memory buffer hasn't filled yet.
    tokio::time::sleep(Duration::from_millis(150)).await;

    // Query targets the group's source-subnet's owning aggregator;
    // the daemon's node_id IS the aggregator host because there's
    // one node in this test. Pass the daemon's node_id as `target`.
    let target = booted.mesh.node_id().to_string();
    let (code, stdout, stderr) = run_cli(
        &booted,
        &home,
        "query",
        &[&target, "--kind", "0x0001", "--fresh"],
    )
    .await;
    assert_eq!(code, 0, "query failed: stderr={stderr}");
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("non-JSON stdout ({e}): {stdout}"));
    assert_eq!(parsed["fresh"], true);
    assert_eq!(parsed["fold_kind"], "0x0001");

    drain_registry(&booted.registry).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn missing_pubkey_exits_with_invalid_args() {
    // Don't boot a daemon — the failure is purely flag validation.
    let mut cmd = AssertCommand::cargo_bin("net-mesh").expect("cargo_bin");
    let home = TempDir::new().expect("home tempdir");
    cmd.env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env("USERPROFILE", home.path())
        .args([
            "aggregator",
            "query",
            "0x1",
            "--kind",
            "0x0001",
            "--node-addr",
            "127.0.0.1:1",
            "--node-id",
            "1",
            "--psk-hex",
            PSK_HEX,
        ]);
    let result = tokio::task::spawn_blocking(move || cmd.output())
        .await
        .expect("spawn_blocking")
        .expect("invoke");
    assert_eq!(
        result.status.code(),
        Some(2),
        "expected exit code 2 (InvalidArgs); stderr={}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bad_pubkey_hex_exits_with_invalid_args() {
    let mut cmd = AssertCommand::cargo_bin("net-mesh").expect("cargo_bin");
    let home = TempDir::new().expect("home tempdir");
    cmd.env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env("USERPROFILE", home.path())
        .args([
            "aggregator",
            "query",
            "0x1",
            "--kind",
            "0x0001",
            "--node-addr",
            "127.0.0.1:1",
            "--node-pubkey",
            "0xnotvalidhex",
            "--node-id",
            "1",
            "--psk-hex",
            PSK_HEX,
        ]);
    let result = tokio::task::spawn_blocking(move || cmd.output())
        .await
        .expect("spawn_blocking")
        .expect("invoke");
    assert_eq!(result.status.code(), Some(2));
}
