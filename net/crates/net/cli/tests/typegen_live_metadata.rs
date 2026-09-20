// SPDX-License-Identifier: MIT OR Apache-2.0
//! Live acquisition through an SDK host and a separate CLI process.
use std::time::Duration;

use net_sdk::capabilities::{CapabilitySet, ToolCapability};
use net_sdk::{Mesh, MeshBuilder};

async fn discovery_host() -> Mesh {
    use net::adapter::net::{ChannelConfigRegistry, EntityKeypair, MeshNode, MeshNodeConfig};
    use std::sync::Arc;
    // Test-only cadence: distinct announcements must arrive inside the
    // production CLI's five-second observation window.
    let config = MeshNodeConfig::new("127.0.0.1:0".parse().unwrap(), [0x42; 32])
        .with_min_announce_interval(Duration::from_millis(50));
    let mut node = MeshNode::new(EntityKeypair::generate(), config)
        .await
        .unwrap();
    let channels = Arc::new(ChannelConfigRegistry::new());
    node.set_channel_configs(channels.clone());
    Mesh::from_node_arc(Arc::new(node), channels, None)
}

fn command(mesh: &Mesh, root: &std::path::Path, verb: &str) -> tokio::process::Command {
    let config = root.join("config.toml");
    std::fs::write(&config, "").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let mut cmd = tokio::process::Command::new(assert_cmd::cargo::cargo_bin("net-mesh"));
    cmd.env_remove("NET_MESH_CONFIG")
        .env_remove("NET_MESH_PROFILE")
        .arg("--config")
        .arg(config)
        .args(["--output", "json", "--timeout", "9s", "typegen"])
        .arg(verb)
        .args([
            "--node-addr",
            &mesh.local_addr().to_string(),
            "--node-id",
            &mesh.node_id().to_string(),
            "--node-pubkey",
            &hex::encode(mesh.public_key()),
            "--psk-hex",
            &"42".repeat(32),
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    cmd
}

fn caps(ids: &[&str]) -> CapabilitySet {
    ids.iter().fold(CapabilitySet::new(), |caps, id| {
        caps.add_tool(
            ToolCapability::new(*id, *id)
                .with_input_schema(r#"{"type":"object","properties":{"value":{"type":"string"}}}"#),
        )
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn requested_tool_arriving_after_unrelated_is_captured() {
    let mesh = discovery_host().await;
    mesh.start();
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("snapshot.json");
    let mut cmd = command(&mesh, dir.path(), "snapshot");
    cmd.args(["--tool", "wanted", "--out"]).arg(&output);
    let mut child = cmd.spawn().unwrap();
    for _ in 0..10 {
        mesh.announce_capabilities(caps(&["unrelated"]))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        child.try_wait().unwrap().is_none(),
        "unrelated tools ended discovery"
    );
    for _ in 0..5 {
        mesh.announce_capabilities(caps(&["unrelated", "wanted"]))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let result = tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let snapshot: serde_json::Value =
        serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap();
    assert_eq!(snapshot["descriptors"].as_array().unwrap().len(), 1);
    assert_eq!(snapshot["descriptors"][0]["tool_id"], "wanted");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn missing_requested_tool_preserves_snapshot_and_generated_destination() {
    let mesh = MeshBuilder::new("127.0.0.1:0", &[0x42; 32])
        .unwrap()
        .build()
        .await
        .unwrap();
    mesh.start();
    let dir = tempfile::tempdir().unwrap();
    for verb in ["snapshot", "generate"] {
        let output = dir.path().join(verb);
        if verb == "snapshot" {
            std::fs::write(&output, b"previous snapshot").unwrap();
        }
        let mut cmd = command(&mesh, dir.path(), verb);
        cmd.args(["--tool", "missing", "--out"]).arg(&output);
        if verb == "generate" {
            cmd.args(["--language", "ts"]);
        }
        let child = cmd.spawn().unwrap();
        for _ in 0..5 {
            mesh.announce_capabilities(caps(&["unrelated"]))
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let result = tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            result.status.code(),
            Some(7),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(result.stdout.is_empty());
        assert!(String::from_utf8_lossy(&result.stderr)
            .contains("requested tools not observed after filters: missing"));
        if verb == "snapshot" {
            assert_eq!(std::fs::read(output).unwrap(), b"previous snapshot");
        } else {
            assert!(!output.exists());
        }
    }
}
