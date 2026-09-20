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
    command_with_budget(mesh, root, verb, "9s")
}

fn command_with_budget(
    mesh: &Mesh,
    root: &std::path::Path,
    verb: &str,
    budget: &str,
) -> tokio::process::Command {
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
        .args(["--output", "json", "--timeout", budget, "typegen"])
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

fn descriptor() -> net_sdk::tool::ToolDescriptor {
    serde_json::from_value(serde_json::json!({
        "tool_id":"hydrated", "name":"hydrated", "version":"1.0.0",
        "input_schema": serde_json::json!({"type":"object", "description":"large contract ".repeat(1500), "properties":{"value":{"type":"string"}}, "required":["value"]}).to_string(),
        "output_schema":null, "requires":[], "estimated_time_ms":0,
        "stateless":true, "streaming":false, "tags":[], "node_count":0
    })).unwrap()
}

fn generated_files(
    root: &std::path::Path,
) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
    fn visit(
        root: &std::path::Path,
        dir: &std::path::Path,
        files: &mut std::collections::BTreeMap<std::path::PathBuf, Vec<u8>>,
    ) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(root, &path, files);
            } else {
                files.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    std::fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut files = std::collections::BTreeMap::new();
    visit(root, root, &mut files);
    files
}

async fn publish_projection(mesh: &Mesh) {
    // Deliberately schema-free advertisement; the full (oversized) contract
    // lives only in the real metadata RPC. This does not assert automatic
    // SDK announcement truncation, which is a separate substrate concern.
    mesh.announce_capabilities(
        CapabilitySet::new().add_tool(ToolCapability::new("hydrated", "hydrated")),
    )
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hydrated_snapshot_regenerates_offline_after_provider_shutdown() {
    use net_sdk::tool::{ToolMetadataRequest, ToolMetadataResponse, TOOL_METADATA_FETCH_SERVICE};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let mesh = discovery_host().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let full = descriptor();
    let served = full.clone();
    let handle = mesh
        .serve_rpc_typed::<ToolMetadataRequest, ToolMetadataResponse, _, _>(
            TOOL_METADATA_FETCH_SERVICE,
            net_sdk::mesh_rpc::Codec::Json,
            move |request| {
                assert_eq!(request.name, "hydrated");
                observed.fetch_add(1, Ordering::SeqCst);
                let descriptor = served.clone();
                async move { Ok(ToolMetadataResponse::Found { descriptor }) }
            },
        )
        .unwrap();
    mesh.start();
    let dir = tempfile::tempdir().unwrap();
    let snapshot = dir.path().join("snapshot.json");
    let mut cmd = command(&mesh, dir.path(), "snapshot");
    let child = cmd
        .args(["--tool", "hydrated", "--out"])
        .arg(&snapshot)
        .spawn()
        .unwrap();
    for _ in 0..8 {
        publish_projection(&mesh).await;
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
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let captured: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&snapshot).unwrap()).unwrap();
    assert_eq!(captured["format_version"], 1);
    assert_eq!(
        captured["descriptors"][0]["input_schema"],
        full.input_schema.unwrap()
    );
    assert!(captured["descriptors"][0]["output_schema"].is_null());
    drop(handle);
    mesh.shutdown().await.unwrap();
    for (language, module) in [
        ("ts", "tools/hydrated.ts"),
        ("python", "hydrated/models.py"),
    ] {
        let out = dir.path().join(language);
        let mut before = None;
        for _ in 0..2 {
            let result = tokio::process::Command::new(assert_cmd::cargo::cargo_bin("net-mesh"))
                .env_remove("NET_MESH_CONFIG")
                .env_remove("NET_MESH_PROFILE")
                .arg("--config")
                .arg(dir.path().join("config.toml"))
                .args([
                    "--output",
                    "json",
                    "typegen",
                    "generate",
                    "--language",
                    language,
                    "--from-snapshot",
                ])
                .arg(&snapshot)
                .arg("--out")
                .arg(&out)
                .kill_on_drop(true)
                .output()
                .await
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            let bytes = std::fs::read(out.join(module)).unwrap();
            assert!(String::from_utf8_lossy(&bytes).contains("HydratedRequest"));
            let files = generated_files(&out);
            if let Some(previous) = &before {
                assert_eq!(&files, previous);
            }
            before = Some(files);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn metadata_failures_do_not_publish_or_retry() {
    use net_sdk::tool::{ToolMetadataRequest, ToolMetadataResponse, TOOL_METADATA_FETCH_SERVICE};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    for mode in [
        "not_found",
        "wrong_id",
        "wrong_version",
        "missing_input",
        "bad_schema",
        "oversized_response",
        "timeout",
    ] {
        let mesh = discovery_host().await;
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let _handle = mesh
            .serve_rpc_typed::<ToolMetadataRequest, ToolMetadataResponse, _, _>(
                TOOL_METADATA_FETCH_SERVICE,
                net_sdk::mesh_rpc::Codec::Json,
                move |_| {
                    observed.fetch_add(1, Ordering::SeqCst);
                    async move {
                        if mode == "timeout" {
                            tokio::time::sleep(Duration::from_secs(30)).await;
                        }
                        let mut descriptor = descriptor();
                        match mode {
                            "not_found" => {
                                return Ok(ToolMetadataResponse::NotFound {
                                    name: "hydrated".into(),
                                })
                            }
                            "wrong_id" => descriptor.tool_id = "other".into(),
                            "wrong_version" => descriptor.version = "2.0.0".into(),
                            "missing_input" => descriptor.input_schema = None,
                            "bad_schema" => descriptor.input_schema = Some("not json".into()),
                            "oversized_response" => {
                                descriptor.input_schema = Some("x".repeat(1024 * 1024))
                            }
                            _ => {}
                        }
                        Ok(ToolMetadataResponse::Found { descriptor })
                    }
                },
            )
            .unwrap();
        mesh.start();
        let dir = tempfile::tempdir().unwrap();
        for verb in ["snapshot", "generate"] {
            let output = dir.path().join(verb);
            if verb == "snapshot" {
                std::fs::write(&output, b"old snapshot").unwrap();
            }
            let mut cmd = command_with_budget(&mesh, dir.path(), verb, "2s");
            cmd.args(["--tool", "hydrated", "--out"]).arg(&output);
            if verb == "generate" {
                cmd.args(["--language", "ts"]);
            }
            let child = cmd.spawn().unwrap();
            for _ in 0..5 {
                publish_projection(&mesh).await;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            let result = tokio::time::timeout(Duration::from_secs(5), child.wait_with_output())
                .await
                .unwrap()
                .unwrap();
            assert!(!result.status.success(), "{mode}/{verb}");
            assert!(result.stdout.is_empty());
            let expected = match mode {
                "not_found" => "NotFound",
                "wrong_id" | "wrong_version" => "metadata mismatch",
                "missing_input" => "no input schema",
                "bad_schema" => "unusable input schema",
                "oversized_response" => "1048576-byte limit",
                _ => "exceeded --timeout",
            };
            assert!(
                String::from_utf8_lossy(&result.stderr).contains(expected),
                "{mode}: {}",
                String::from_utf8_lossy(&result.stderr)
            );
            if mode == "timeout" {
                assert_eq!(result.status.code(), Some(7));
            }
            if verb == "snapshot" {
                assert_eq!(std::fs::read(output).unwrap(), b"old snapshot");
            } else {
                assert!(!output.exists());
            }
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "one attempt per invocation: {mode}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn metadata_fetches_share_remaining_budget_without_partial_publication() {
    use net_sdk::tool::{ToolMetadataRequest, ToolMetadataResponse, TOOL_METADATA_FETCH_SERVICE};
    use std::sync::{Arc, Mutex};
    let mesh = discovery_host().await;
    let calls = Arc::new(Mutex::new(Vec::new()));
    let observed = calls.clone();
    let _handle = mesh
        .serve_rpc_typed::<ToolMetadataRequest, ToolMetadataResponse, _, _>(
            TOOL_METADATA_FETCH_SERVICE,
            net_sdk::mesh_rpc::Codec::Json,
            move |request| {
                observed.lock().unwrap().push(request.name.clone());
                async move {
                    tokio::time::sleep(Duration::from_millis(1200)).await;
                    let mut descriptor = descriptor();
                    descriptor.tool_id = request.name.clone();
                    descriptor.name = request.name;
                    Ok(ToolMetadataResponse::Found { descriptor })
                }
            },
        )
        .unwrap();
    mesh.start();
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("snapshot.json");
    std::fs::write(&output, b"old snapshot").unwrap();
    let mut cmd = command_with_budget(&mesh, dir.path(), "snapshot", "3s");
    let started = std::time::Instant::now();
    let child = cmd
        .args(["--tool", "a", "b", "c", "--out"])
        .arg(&output)
        .spawn()
        .unwrap();
    for _ in 0..5 {
        let caps = ["a", "b", "c"]
            .into_iter()
            .fold(CapabilitySet::new(), |caps, id| {
                caps.add_tool(ToolCapability::new(id, id))
            });
        mesh.announce_capabilities(caps).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let result = tokio::time::timeout(Duration::from_secs(5), child.wait_with_output())
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
    assert_eq!(std::fs::read(output).unwrap(), b"old snapshot");
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(
        *calls.lock().unwrap(),
        vec!["a", "b", "c"],
        "each fetch started once; the last uses only the remaining budget"
    );
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
