// SPDX-License-Identifier: MIT OR Apache-2.0
//! Real subprocess startup cancellation and NDJSON lifetime controls.
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

async fn exercise(stall: bool) {
    let dir = tempfile::tempdir().unwrap();
    let fixture = dir
        .path()
        .join(format!("mcp-fixture{}", std::env::consts::EXE_SUFFIX));
    let compile = tokio::process::Command::new("rustc")
        .arg("--edition=2021")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/wrap_startup.rs"
        ))
        .arg("-o")
        .arg(&fixture)
        .kill_on_drop(true)
        .output()
        .await
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let config = dir.path().join("config.toml");
    std::fs::write(&config, "").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let identity = dir.path().join("operator.toml");
    let generated = tokio::process::Command::new(assert_cmd::cargo::cargo_bin("net-mesh"))
        .env_remove("NET_MESH_CONFIG")
        .env_remove("NET_MESH_PROFILE")
        .arg("--config")
        .arg(&config)
        .args(["identity", "generate", "--out"])
        .arg(&identity)
        .output()
        .await
        .unwrap();
    assert!(generated.status.success());
    let mesh = net_sdk::MeshBuilder::new("127.0.0.1:0", &[0x42; 32])
        .unwrap()
        .build()
        .await
        .unwrap();
    mesh.start();
    let control = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut child = tokio::process::Command::new(assert_cmd::cargo::cargo_bin("net-mesh"))
        .env_remove("NET_MESH_CONFIG")
        .env_remove("NET_MESH_PROFILE")
        .arg("--config")
        .arg(&config)
        .args([
            "--timeout",
            "2s",
            "--output",
            "ndjson",
            "wrap",
            "fixture",
            "--identity",
        ])
        .arg(&identity)
        .args([
            "--bind",
            "127.0.0.1:0",
            "--node-addr",
            &mesh.local_addr().to_string(),
            "--node-id",
            &mesh.node_id().to_string(),
            "--node-pubkey",
            &hex::encode(mesh.public_key()),
            "--psk-hex",
            &"42".repeat(32),
            "--",
        ])
        .arg(&fixture)
        .arg(control.local_addr().unwrap().to_string())
        .arg(if stall { "stall" } else { "serve" })
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let (mut socket, _) = tokio::time::timeout(Duration::from_secs(5), control.accept())
        .await
        .unwrap()
        .unwrap();
    let mut ready = [0; 6];
    tokio::time::timeout(Duration::from_secs(5), socket.read_exact(&mut ready))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&ready, b"ready\n");
    if stall {
        let output = tokio::time::timeout(Duration::from_secs(5), child.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(7),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty());
        let closed = tokio::time::timeout(Duration::from_secs(5), socket.read(&mut [0]))
            .await
            .unwrap();
        assert!(
            matches!(closed, Ok(0))
                || matches!(closed, Err(ref e) if e.kind() == std::io::ErrorKind::ConnectionReset),
            "{closed:?}"
        );
        assert!(!String::from_utf8_lossy(&output.stderr).contains(&"42".repeat(32)));
    } else {
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let row = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let row: serde_json::Value = serde_json::from_str(&row).unwrap();
        assert_eq!(row["event"], "wrapped");
        assert_eq!(row["tools"].as_array().unwrap().len(), 1);
        tokio::time::sleep(Duration::from_millis(2200)).await;
        assert!(child.try_wait().unwrap().is_none());
        socket.write_all(b"x").await.unwrap();
        let row = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let row: serde_json::Value = serde_json::from_str(&row).unwrap();
        assert_eq!(row["event"], "server_exited");
        let output = tokio::time::timeout(Duration::from_secs(5), child.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(lines.next_line().await.unwrap().is_none());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn startup_timeout_kills_unresponsive_wrapped_child() {
    exercise(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wrapped_provider_outlives_startup_budget_and_emits_ndjson() {
    exercise(false).await;
}
