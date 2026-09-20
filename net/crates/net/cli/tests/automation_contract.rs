//! Automation framing and confirmation must hold for the complete subprocess output.
use assert_cmd::Command;
use serde_json::Value;
use std::path::Path;
use std::time::Duration;

fn run(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::cargo_bin("net-mesh")
        .unwrap()
        .timeout(Duration::from_secs(10))
        .env_remove("NET_MESH_CONFIG")
        .env_remove("NET_MESH_PROFILE")
        .args(["--config"])
        .arg(dir.join("config.toml"))
        .args(["--output", "json"])
        .args(args)
        .assert()
        .get_output()
        .clone()
}

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.toml");
    std::fs::write(&cfg, "").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&cfg, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let identity = dir.path().join("operator.toml");
    let out = run(
        dir.path(),
        &["identity", "generate", "--out", identity.to_str().unwrap()],
    );
    assert!(out.status.success());
    dir
}

fn ice_args(identity: &Path) -> Vec<&str> {
    vec![
        "ice",
        "freeze-cluster",
        "--local",
        "--ttl",
        "1m",
        "--identity",
        identity.to_str().unwrap(),
    ]
}

#[test]
fn unsupported_timeouts_refuse_before_local_effects() {
    let dir = fixture();
    let artifact = dir.path().join("must-not-exist");
    let commands = [
        vec!["identity", "generate", "--out", artifact.to_str().unwrap()],
        vec![
            "netdb",
            "tasks",
            "create",
            "1",
            "--title",
            "test",
            "--store",
            artifact.to_str().unwrap(),
        ],
        vec!["admin", "cordon", "1", "--dry-run"],
        vec!["audit", "stream", "--local"],
        vec!["aggregator", "ls", "--local"],
        vec!["aggregator", "ls", "--inspect-target"],
        vec!["mcp", "serve", "--inspect-target"],
        vec!["transfer", "ls", "--inspect-target"],
        vec![
            "transfer",
            "recv-blob",
            "--inspect-target",
            "--blob-ref",
            "00",
            "--out",
            artifact.to_str().unwrap(),
        ],
        vec![
            "transfer",
            "recv-dir",
            "--remote-ref",
            "00",
            "--out",
            artifact.to_str().unwrap(),
        ],
        vec!["transfer", "send-blob", artifact.to_str().unwrap()],
        vec![
            "typegen",
            "generate",
            "--language",
            "ts",
            "--from-snapshot",
            "missing.json",
            "--out",
            artifact.to_str().unwrap(),
        ],
        vec![
            "typegen",
            "snapshot",
            "--inspect-target",
            "--out",
            artifact.to_str().unwrap(),
        ],
    ];
    for mut args in commands {
        args.extend(["--timeout", "1s"]);
        let out = run(dir.path(), &args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(out.stdout.is_empty());
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("--timeout is not supported"),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!artifact.exists());
    }
}

#[test]
fn transfer_and_live_typegen_share_timeout_contract_without_publication() {
    let dir = fixture();
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.set_nonblocking(true).unwrap();
    let addr = socket.local_addr().unwrap().to_string();
    let key = "42".repeat(32);
    let destination = dir.path().join("not-published");
    for command in [
        vec!["transfer", "ls"],
        vec!["transfer", "status", "1"],
        vec!["transfer", "cancel", "1"],
        vec![
            "typegen",
            "generate",
            "--language",
            "ts",
            "--out",
            destination.to_str().unwrap(),
        ],
        vec![
            "typegen",
            "snapshot",
            "--out",
            destination.to_str().unwrap(),
        ],
    ] {
        for budget in ["0s", "200ms"] {
            let mut args = command.clone();
            args.extend([
                "--node-addr",
                &addr,
                "--node-id",
                "9",
                "--node-pubkey",
                &key,
                "--psk-hex",
                &key,
                "--timeout",
                budget,
            ]);
            let out = run(dir.path(), &args);
            assert_eq!(
                out.status.code(),
                Some(7),
                "{args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(out.stdout.is_empty());
            assert!(!destination.exists());
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(stderr.contains("does not prove cancellation"));
            assert!(!stderr.contains(&key));
            let mut packet = [0; 2048];
            if budget == "0s" {
                assert_eq!(
                    socket.recv_from(&mut packet).unwrap_err().kind(),
                    std::io::ErrorKind::WouldBlock
                );
            } else {
                while socket.recv_from(&mut packet).is_ok() {}
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_typegen_discovery_budget_preserves_existing_output_on_expiry() {
    let holder = net_sdk::MeshBuilder::new("127.0.0.1:0", &[0x42; 32])
        .unwrap()
        .build()
        .await
        .unwrap();
    holder.start();
    let dir = fixture();
    let destination = dir.path().join("snapshot.json");
    std::fs::write(&destination, b"previous output").unwrap();
    let addr = holder.local_addr().to_string();
    let node = holder.node_id().to_string();
    let public_key = hex::encode(holder.public_key());
    let out_path = destination.to_str().unwrap().to_owned();
    let root = dir.path().to_path_buf();
    tokio::task::spawn_blocking(move || {
        let key = "42".repeat(32);
        let mut args = vec![
            "typegen",
            "snapshot",
            "--out",
            &out_path,
            "--node-addr",
            &addr,
            "--node-id",
            &node,
            "--node-pubkey",
            &public_key,
            "--psk-hex",
            &key,
            "--timeout",
            "1s",
        ];
        let out = run(&root, &args);
        assert_eq!(
            out.status.code(),
            Some(7),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.stdout.is_empty());
        assert_eq!(std::fs::read(&out_path).unwrap(), b"previous output");
        // Existing empty-discovery semantics are unchanged when the budget allows
        // the five-second observation to finish; CLI-3 owns richer acquisition.
        *args.last_mut().unwrap() = "9s";
        let out = run(&root, &args);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let _: Value = serde_json::from_slice(&out.stdout).unwrap();
        let snapshot: Value = serde_json::from_slice(&std::fs::read(&out_path).unwrap()).unwrap();
        assert_eq!(snapshot["descriptors"], serde_json::json!([]));
    })
    .await
    .unwrap();
}

#[test]
fn aggregator_timeout_is_exit_seven_without_success_or_zero_budget_packets() {
    let dir = fixture();
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.set_nonblocking(true).unwrap();
    let addr = socket.local_addr().unwrap().to_string();
    let key = "42".repeat(32);
    let base = [
        "aggregator",
        "ls",
        "--remote",
        "--node-addr",
        &addr,
        "--node-id",
        "9",
        "--node-pubkey",
        &key,
        "--psk-hex",
        &key,
    ];
    let mut args = base.to_vec();
    args.extend(["--timeout", "0s"]);
    let out = run(dir.path(), &args);
    assert_eq!(out.status.code(), Some(7));
    assert!(out.stdout.is_empty());
    let mut packet = [0; 2048];
    assert_eq!(
        socket.recv_from(&mut packet).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    args.pop();
    args.push("200ms");
    let out = run(dir.path(), &args);
    assert_eq!(
        out.status.code(),
        Some(7),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("does not prove cancellation"));
    assert!(!stderr.contains(&key));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mcp_startup_budget_does_not_limit_protocol_lifetime() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let holder = net_sdk::MeshBuilder::new("127.0.0.1:0", &[0x42; 32])
        .unwrap()
        .build()
        .await
        .unwrap();
    holder.start();
    let dir = fixture();
    let mut child = tokio::process::Command::new(assert_cmd::cargo::cargo_bin("net-mesh"))
        .env_remove("NET_MESH_CONFIG")
        .env_remove("NET_MESH_PROFILE")
        .arg("--config")
        .arg(dir.path().join("config.toml"))
        .args([
            "--output",
            "json",
            "--timeout",
            "2s",
            "mcp",
            "serve",
            "--identity",
        ])
        .arg(dir.path().join("operator.toml"))
        .arg("--pin-store")
        .arg(dir.path().join("pins.json"))
        .args([
            "--bind",
            "127.0.0.1:0",
            "--node-addr",
            &holder.local_addr().to_string(),
            "--node-id",
            &holder.node_id().to_string(),
            "--node-pubkey",
            &hex::encode(holder.public_key()),
            "--psk-hex",
            &"42".repeat(32),
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    stdin.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"clientInfo\":{\"name\":\"deadline-test\",\"version\":\"1\"}}}\n").await.unwrap();
    let first = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let first: Value = serde_json::from_str(&first).unwrap();
    assert_eq!(first["id"], 1);
    assert!(first.get("result").is_some(), "{first}");
    tokio::time::sleep(Duration::from_millis(2200)).await;
    assert!(
        child.try_wait().unwrap().is_none(),
        "startup deadline killed the running service"
    );
    stdin.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n").await.unwrap();
    let second = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let second: Value = serde_json::from_str(&second).unwrap();
    assert_eq!(second["id"], 2);
    assert!(second.get("result").is_some(), "{second}");
    drop(stdin);
    let result = tokio::time::timeout(Duration::from_secs(5), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        lines.next_line().await.unwrap().is_none(),
        "unexpected non-protocol stdout"
    );
    assert!(!String::from_utf8_lossy(&result.stderr).contains(&"42".repeat(32)));
}

#[test]
fn mcp_startup_expiry_emits_no_protocol_payload() {
    let dir = fixture();
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.set_nonblocking(true).unwrap();
    let addr = socket.local_addr().unwrap().to_string();
    let key = "42".repeat(32);
    let identity = dir.path().join("operator.toml");
    for budget in ["0s", "200ms"] {
        let out = run(
            dir.path(),
            &[
                "mcp",
                "serve",
                "--identity",
                identity.to_str().unwrap(),
                "--node-addr",
                &addr,
                "--node-id",
                "9",
                "--node-pubkey",
                &key,
                "--psk-hex",
                &key,
                "--bind",
                "127.0.0.1:0",
                "--timeout",
                budget,
            ],
        );
        assert_eq!(
            out.status.code(),
            Some(7),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&out.stderr).contains(&key));
        if budget == "0s" {
            assert_eq!(
                socket.recv_from(&mut [0; 2048]).unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
        }
    }
}

#[test]
fn wrap_attachment_expiry_precedes_child_spawn() {
    let dir = fixture();
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.set_nonblocking(true).unwrap();
    let addr = socket.local_addr().unwrap().to_string();
    let key = "42".repeat(32);
    let identity = dir.path().join("operator.toml");
    for budget in ["0s", "200ms"] {
        let out = run(
            dir.path(),
            &[
                "wrap",
                "fixture",
                "--identity",
                identity.to_str().unwrap(),
                "--node-addr",
                &addr,
                "--node-id",
                "9",
                "--node-pubkey",
                &key,
                "--psk-hex",
                &key,
                "--bind",
                "127.0.0.1:0",
                "--timeout",
                budget,
                "--",
                "nonexistent-child-must-not-spawn",
            ],
        );
        assert_eq!(
            out.status.code(),
            Some(7),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&out.stderr).contains(&key));
        if budget == "0s" {
            assert_eq!(
                socket.recv_from(&mut [0; 2048]).unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
        }
    }
    let out = run(
        dir.path(),
        &[
            "wrap",
            "fixture",
            "--inspect-target",
            "--timeout",
            "1s",
            "--",
            "nonexistent-child-must-not-spawn",
        ],
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--timeout is not supported"));
}

#[test]
fn ice_commit_is_one_complete_json_result_and_dry_run_remains_preview_only() {
    let dir = fixture();
    let identity = dir.path().join("operator.toml");
    let mut args = ice_args(&identity);
    args.push("--yes");
    let out = run(dir.path(), &args);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: Value =
        serde_json::from_slice(&out.stdout).expect("complete stdout is one JSON value");
    assert!(out.stdout.ends_with(b"\n"));
    assert!(value["preview"]["blast_hash"].is_string());
    assert!(value["commit"]["commit_id"].is_u64());
    assert!(String::from_utf8_lossy(&out.stderr).contains("ICE preview"));
    args.pop();
    args.push("--dry-run");
    let out = run(dir.path(), &args);
    assert!(out.status.success());
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(value["blast_hash"].is_string());
    assert!(value.get("commit").is_none());
    assert!(!String::from_utf8_lossy(&out.stderr).contains("Type YES"));
}

#[test]
fn ice_refusal_and_malformed_signatures_emit_no_success_payload() {
    let dir = fixture();
    let identity = dir.path().join("operator.toml");
    let args = ice_args(&identity);
    let out = run(dir.path(), &args);
    assert_eq!(out.status.code(), Some(8));
    assert!(
        out.stdout.is_empty(),
        "refusal must not emit a success-shaped preview"
    );
    for signature in [
        "not-json".to_string(),
        format!(
            r#"{{"operator_id":0,"signature_hex":"{}"}}"#,
            "00".repeat(63)
        ),
    ] {
        let mut invalid = args.clone();
        invalid.extend(["--yes", "--sig", &signature]);
        let out = run(dir.path(), &invalid);
        assert!(!out.status.success());
        assert!(out.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&out.stderr).contains("Type YES"));
    }
    let missing_identity = dir.path().join("missing.toml");
    let mut args = ice_args(&missing_identity);
    args.push("--yes");
    let out = run(dir.path(), &args);
    assert!(
        !out.status.success(),
        "--yes must not bypass identity loading"
    );
    assert!(out.stdout.is_empty());
}
