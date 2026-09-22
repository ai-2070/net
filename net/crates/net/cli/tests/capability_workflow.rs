// SPDX-License-Identifier: MIT OR Apache-2.0
//! Exactly two live CLI mesh nodes, with no bootstrap node or in-process host.
#[path = "fixtures/capability_client.rs"]
mod client;
use net_sdk::identity::Identity;
use serde_json::{json, Value};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
};

fn command(root: &Path) -> Command {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin("net-mesh"));
    cmd.env_remove("NET_MESH_CONFIG")
        .env_remove("NET_MESH_PROFILE")
        .arg("--config")
        .arg(root.join("config.toml"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    cmd
}
async fn identity(root: &Path, name: &str) -> Identity {
    let path = root.join(name);
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        command(root)
            .args(["identity", "generate", "--out"])
            .arg(&path)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(output.status.success());
    let document: toml::Value = toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    Identity::from_bytes(&hex::decode(document["seed_hex"].as_str().unwrap()).unwrap()).unwrap()
}
async fn exercise(allow_provider: bool) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let config = root.join("config.toml");
    std::fs::write(&config, "").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let provider = identity(root, "provider.toml").await;
    let consumer = identity(root, "consumer.toml").await;
    assert_ne!(provider.node_id(), consumer.node_id());
    let control = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let audit = root.join("invocations.jsonl");
    let mut cmd = command(root);
    cmd.args([
        "--output",
        "ndjson",
        "--timeout",
        "10s",
        "wrap",
        "journey",
        "--listen",
        "--identity",
    ])
    .arg(root.join("provider.toml"))
    .args(["--psk-hex", &"42".repeat(32)]);
    if allow_provider {
        cmd.args(["--allow", &consumer.origin_hash().to_string()]);
    }
    cmd.arg("--")
        .arg(if cfg!(windows) { "python" } else { "python3" })
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/capability_server.py"
        ))
        .arg(&audit)
        .arg(control.local_addr().unwrap().to_string());
    let mut publisher = cmd.spawn().unwrap();
    let (mut control, _) = tokio::time::timeout(Duration::from_secs(15), control.accept())
        .await
        .unwrap()
        .unwrap();
    let mut events = BufReader::new(publisher.stdout.take().unwrap()).lines();
    let ready = tokio::time::timeout(Duration::from_secs(10), events.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        !ready.contains(&"42".repeat(32)),
        "PSK must not appear in readiness"
    );
    let wrapped: Value = serde_json::from_str(&ready).unwrap();
    assert_eq!(wrapped["event"], "wrapped");
    assert_eq!(wrapped["tools"], json!(["journey_echo"]));
    let connection = &wrapped["connection"];
    assert_eq!(
        connection["node_id"],
        format!("0x{:016x}", provider.node_id())
    );
    let bind: std::net::SocketAddr = connection["bind"].as_str().unwrap().parse().unwrap();
    assert!(bind.ip().is_loopback());
    assert_ne!(bind.port(), 0);
    let mut cmd = command(root);
    cmd.args(["--timeout", "10s", "mcp", "serve", "--identity"])
        .arg(root.join("consumer.toml"))
        .arg("--pin-store")
        .arg(root.join("pins.json"))
        .args([
            "--bind",
            "127.0.0.1:0",
            "--node-addr",
            connection["bind"].as_str().unwrap(),
            "--node-id",
            connection["node_id"].as_str().unwrap(),
            "--node-pubkey",
            connection["node_pubkey"].as_str().unwrap(),
            "--psk-hex",
            &"42".repeat(32),
        ]);
    let mut caller = cmd.spawn().unwrap();
    let mut client =
        client::Client::new(caller.stdin.take().unwrap(), caller.stdout.take().unwrap());
    client.initialize().await;
    let cap_id = format!("{}/journey_echo", provider.node_id());
    if allow_provider {
        tokio::time::timeout(Duration::from_secs(25), async {
            loop {
                let found = client
                    .tool("net_search_capabilities", json!({"query":"journey_echo"}))
                    .await;
                if let Ok(rows) = serde_json::from_str::<Value>(client::text(&found)) {
                    if rows["capabilities"]
                        .as_array()
                        .is_some_and(|rows| rows.iter().any(|r| r["cap_id"] == cap_id))
                    {
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("provider discovered by exact identity");
    }
    let args = json!({"cap_id":cap_id,"arguments":{"message":"one explicit invocation"}});
    if allow_provider {
        let denied = client.tool("net_invoke_capability", args.clone()).await;
        assert_eq!(denied["isError"], true, "{denied}");
        assert!(client::text(&denied).contains("approval"), "{denied}");
        assert!(
            !audit.exists(),
            "consent refusal must precede handler effect"
        );
    }
    let approved = tokio::time::timeout(
        Duration::from_secs(10),
        command(root)
            .args(["mcp", "pin", "approve", &cap_id, "--pin-store"])
            .arg(root.join("pins.json"))
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(approved.status.success());
    // Exactly one attempt after approval; never retry ambiguous invocation effects.
    let result = client.tool("net_invoke_capability", args).await;
    if allow_provider {
        assert_eq!(result["isError"], false, "{result}");
        let receipt: Value = serde_json::from_str(client::text(&result)).unwrap();
        assert_eq!(receipt, json!({"message":"one explicit invocation"}));
        let records = std::fs::read_to_string(&audit).unwrap();
        assert_eq!(records.lines().count(), 1, "no hidden retries");
        assert_eq!(serde_json::from_str::<Value>(&records).unwrap(), receipt);
    } else {
        assert_eq!(result["isError"], true, "{result}");
        assert!(
            client::text(&result).contains("Denied by remote wrapper"),
            "{result}"
        );
        assert!(
            !audit.exists(),
            "provider refusal must precede handler effect"
        );
    }
    control.write_all(b"stop").await.unwrap();
    let exited: Value = serde_json::from_str(
        &tokio::time::timeout(Duration::from_secs(10), events.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(exited["event"], "server_exited");
    assert!(
        tokio::time::timeout(Duration::from_secs(10), publisher.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    drop(client);
    assert!(tokio::time::timeout(Duration::from_secs(10), caller.wait())
        .await
        .unwrap()
        .unwrap()
        .success());
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_node_mcp_consent_then_authorized_invocation() {
    exercise(true).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_node_mcp_pin_does_not_override_provider_owner_scope() {
    exercise(false).await;
}
