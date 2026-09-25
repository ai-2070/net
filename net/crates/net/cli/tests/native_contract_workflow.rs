// SPDX-License-Identifier: MIT OR Apache-2.0
//! Public native typed tools + live CLI capture + real generated consumption.
//! At most two live mesh nodes; capture CLI exits before the SDK caller starts.
use net_sdk::{
    capabilities::CapabilitySet,
    mesh_rpc::{CallOptionsTyped, Codec},
    tool::ToolDescriptor,
    MeshBuilder,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
struct Request {
    message: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
struct Response {
    message: String,
    provider_node_id: String,
}

fn descriptor() -> ToolDescriptor {
    serde_json::from_value(json!({
        "tool_id":"native_echo", "name":"native_echo", "version":"1.0.0",
        "description":"Public deterministic native typed echo",
        "input_schema":json!({"type":"object","properties":{"message":{"type":"string"}},"required":["message"],"additionalProperties":false}).to_string(),
        "output_schema":json!({"type":"object","properties":{"message":{"type":"string"},"provider_node_id":{"type":"string"}},"required":["message","provider_node_id"],"additionalProperties":false}).to_string(),
        "requires":[],"estimated_time_ms":0,"stateless":true,"streaming":false,"tags":["journey"],"node_count":0
    })).unwrap()
}
fn cli(root: &Path) -> Command {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin("net-mesh"));
    cmd.env_remove("NET_MESH_CONFIG")
        .env_remove("NET_MESH_PROFILE")
        .arg("--config")
        .arg(root.join("config.toml"))
        .args(["--output", "json"])
        .kill_on_drop(true);
    cmd
}
async fn output(cmd: &mut Command) -> std::process::Output {
    let result = tokio::time::timeout(Duration::from_secs(20), cmd.output())
        .await
        .expect("bounded child process")
        .unwrap();
    assert!(
        result.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    result
}
async fn generate(root: &Path, destination: &Path, language: &str) {
    output(
        cli(root)
            .args([
                "typegen",
                "generate",
                "--language",
                language,
                "--from-snapshot",
            ])
            .arg(root.join("snapshot.json"))
            .arg("--out")
            .arg(destination),
    )
    .await;
}
fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, path: &Path, found: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(root, &path, found);
            } else {
                found.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    std::fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut found = BTreeMap::new();
    visit(root, root, &mut found);
    found
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_typed_call_and_generated_consumer_reuse_captured_contract_offline() {
    let python = if cfg!(windows) { "python" } else { "python3" };
    // Mandatory: no green skip when the generated-consumer runtime is missing.
    output(
        Command::new(python)
            .args([
                "-c",
                "import pydantic; assert int(pydantic.__version__.split('.')[0]) == 2",
            ])
            .kill_on_drop(true),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let config = root.join("config.toml");
    std::fs::write(&config, "").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let host = MeshBuilder::new("127.0.0.1:0", &[0x42; 32])
        .unwrap()
        .build()
        .await
        .unwrap();
    let provider = host.node_id();
    let records = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let observed = records.clone();
    let full = descriptor();
    host.start();
    // Publish only after attachment is observed. This avoids racing the
    // default announcement coalescer against CLI's five-second discovery
    // window; no production timers are shortened for this fixture.
    let mut capture = cli(root);
    capture
        .args([
            "--timeout",
            "10s",
            "typegen",
            "snapshot",
            "--tool",
            "native_echo",
            "--node-addr",
            &host.local_addr().to_string(),
            "--node-id",
            &provider.to_string(),
            "--node-pubkey",
            &hex::encode(host.public_key()),
            "--psk-hex",
            &"42".repeat(32),
            "--out",
        ])
        .arg(root.join("snapshot.json"));
    let capture_run = output(&mut capture);
    let announce = async {
        tokio::time::timeout(Duration::from_secs(5), async {
            while host.inner().peer_count() == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("capture process attached before publication");
        let handle = host
            .serve_tool::<Request, Response, _, _>(full.clone(), move |request| {
                let observed = observed.clone();
                async move {
                    let response = Response {
                        message: request.message,
                        provider_node_id: provider.to_string(),
                    };
                    observed.lock().await.push(response.clone());
                    Ok(response)
                }
            })
            .unwrap();
        host.announce_capabilities(CapabilitySet::new())
            .await
            .unwrap();
        handle
    };
    let (_, handle) = tokio::join!(capture_run, announce);
    assert!(records.lock().await.is_empty(), "capture is not invocation");
    let snapshot: Value =
        serde_json::from_slice(&std::fs::read(root.join("snapshot.json")).unwrap()).unwrap();
    assert_eq!(snapshot["descriptors"].as_array().unwrap().len(), 1);
    assert_eq!(
        snapshot["descriptors"][0]["input_schema"],
        full.input_schema.unwrap()
    );
    assert_eq!(
        snapshot["descriptors"][0]["output_schema"],
        full.output_schema.unwrap()
    );
    let generated = root.join("generated");
    let ts = root.join("generated_ts");
    generate(root, &generated, "python").await;
    generate(root, &ts, "ts").await;
    let python_before = files(&generated);
    let ts_before = files(&ts);
    let caller = MeshBuilder::new("127.0.0.1:0", &[0x42; 32])
        .unwrap()
        .build()
        .await
        .unwrap();
    assert_ne!(caller.node_id(), provider);
    caller.start();
    tokio::time::timeout(
        Duration::from_secs(10),
        caller.connect_via(&host.local_addr().to_string(), host.public_key(), provider),
    )
    .await
    .unwrap()
    .unwrap();
    let options = || CallOptionsTyped {
        raw: Default::default(),
        codec: Codec::Json,
    };
    let bad: Result<Response, _> = tokio::time::timeout(
        Duration::from_secs(5),
        caller.call_typed(provider, "native_echo", &json!({"message": []}), options()),
    )
    .await
    .unwrap();
    assert!(
        matches!(&bad, Err(net_sdk::mesh_rpc::RpcError::ServerError { status, .. })
        if *status == net_sdk::mesh_rpc::NRPC_TYPED_BAD_REQUEST),
        "{bad:?}"
    );
    assert!(
        records.lock().await.is_empty(),
        "typed decode refusal precedes handler effect"
    );
    let native: Response = tokio::time::timeout(
        Duration::from_secs(5),
        caller.call_typed(
            provider,
            "native_echo",
            &Request {
                message: "native caller".into(),
            },
            options(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        native,
        Response {
            message: "native caller".into(),
            provider_node_id: provider.to_string()
        }
    );

    // One local adapter connection, one SDK call. It does not synthesize a
    // response: the generated helper's output has crossed the real mesh.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut consumer = Command::new(python);
    consumer
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/native_consumer.py"
        ))
        .arg(root)
        .arg(listener.local_addr().unwrap().to_string())
        .arg(provider.to_string())
        .kill_on_drop(true);
    let forward = async {
        tokio::time::timeout(Duration::from_secs(15), async {
            let (socket, _) = listener.accept().await.unwrap();
            let (read, mut write) = socket.into_split();
            let mut lines = BufReader::new(read).lines();
            let request: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(request["tool_id"], "native_echo");
            let input: Request = serde_json::from_value(request["input"].clone()).unwrap();
            let response: Response = caller
                .call_typed(provider, "native_echo", &input, options())
                .await
                .unwrap();
            write
                .write_all(format!("{}\n", json!({"result":response})).as_bytes())
                .await
                .unwrap();
        })
        .await
        .expect("bounded real-mesh adapter");
    };
    let (result, ()) = tokio::join!(output(&mut consumer), forward);
    let generated_result: Response = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(*records.lock().await, vec![native, generated_result]);
    drop(handle);
    caller.shutdown().await.unwrap();
    host.shutdown().await.unwrap();
    // Neither generator receives a target. These are contract artifacts, not
    // an assertion that invocation works after the provider has stopped.
    generate(root, &root.join("offline_python"), "python").await;
    generate(root, &root.join("offline_ts"), "ts").await;
    assert_eq!(python_before, files(&root.join("offline_python")));
    assert_eq!(ts_before, files(&root.join("offline_ts")));
}
