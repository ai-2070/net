//! All remote clients inspect their actual resolution before any effects.
use assert_cmd::Command;
use serde_json::Value;
use std::time::Duration;

const KEY: &str = "0101010101010101010101010101010101010101010101010101010101010101";
const PSK: &str = "4242424242424242424242424242424242424242424242424242424242424242";

fn config(dir: &tempfile::TempDir, addr: &str, bind: Option<&str>) -> std::path::PathBuf {
    let path = dir.path().join("config.toml");
    let mut body = format!("[default]\nnode_addr = '{addr}'\nnode_pubkey = '{KEY}'\nnode_id = '9'\npsk_hex = '{PSK}'\n");
    if let Some(bind) = bind {
        body.push_str(&format!("bind = '{bind}'\n"));
    }
    std::fs::write(&path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    path
}

fn run(path: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::cargo_bin("net-mesh")
        .unwrap()
        .timeout(Duration::from_secs(5))
        .args(["--config"])
        .arg(path)
        .args(["--output", "json"])
        .args(args)
        .assert()
        .get_output()
        .clone()
}

#[test]
fn remote_inspection_matrix_has_no_network_files_or_child_process() {
    let dir = tempfile::tempdir().unwrap();
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.set_nonblocking(true).unwrap();
    let path = config(&dir, &socket.local_addr().unwrap().to_string(), None);
    let destination = dir.path().join("must-not-be-created");
    let dest = destination.to_str().unwrap();
    let commands: Vec<Vec<&str>> = vec![
        vec![
            "aggregator",
            "query",
            "9",
            "--kind",
            "1",
            "--inspect-target",
        ],
        vec![
            "aggregator",
            "spawn",
            "--template",
            "test",
            "--name",
            "sample",
            "--replica-count",
            "1",
            "--inspect-target",
        ],
        vec![
            "aggregator",
            "scale",
            "--template",
            "test",
            "--name",
            "sample",
            "--replica-count",
            "2",
            "--inspect-target",
        ],
        vec![
            "transfer",
            "recv-blob",
            "--blob-ref",
            KEY,
            "--out",
            dest,
            "--from",
            "19",
            "--inspect-target",
        ],
        vec![
            "transfer",
            "recv-dir",
            "--remote-ref",
            KEY,
            "--out",
            dest,
            "--inspect-target",
        ],
        vec!["transfer", "ls", "--inspect-target"],
        vec!["transfer", "status", "1", "--inspect-target"],
        vec!["transfer", "cancel", "1", "--inspect-target"],
        vec![
            "typegen",
            "generate",
            "--language",
            "ts",
            "--out",
            dest,
            "--inspect-target",
        ],
        vec!["typegen", "snapshot", "--out", dest, "--inspect-target"],
        vec![
            "wrap",
            "test",
            "--inspect-target",
            "--",
            "this-program-must-not-be-started",
        ],
        vec!["mcp", "serve", "--inspect-target"],
    ];
    for args in commands {
        let out = run(&path, &args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8(out.stdout).unwrap();
        assert!(!text.contains(KEY) && !text.contains(PSK));
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["target"]["node_id"], 9, "{args:?}");
        assert_eq!(v["authorization"], "not_checked");
        if args[0] == "wrap" || args[0] == "mcp" {
            assert_eq!(v["mode"], "hosted_service");
            assert_eq!(v["bind"], "0.0.0.0:0");
        } else {
            assert_eq!(v["bind"], "127.0.0.1:0");
        }
        if args.contains(&dest) {
            assert_eq!(v["destination"], dest);
        }
        if args.contains(&"19") {
            assert_eq!(v["provider_node_id"], 19);
        }
        assert!(!destination.exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
    assert_eq!(
        socket.recv(&mut [0; 2048]).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn bind_resolution_rejects_loopback_to_offhost_and_preserves_overrides() {
    let dir = tempfile::tempdir().unwrap();
    let path = config(&dir, "192.0.2.7:7700", None);
    let out = run(&path, &["aggregator", "ls", "--inspect-target"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--bind"));
    let out = run(
        &path,
        &[
            "aggregator",
            "ls",
            "--bind",
            "0.0.0.0:0",
            "--inspect-target",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["bind"], "0.0.0.0:0");
    assert_eq!(v["provenance"]["bind"], "flag");
    let out = run(
        &path,
        &[
            "aggregator",
            "ls",
            "--remote",
            "--bind",
            "0.0.0.0:0",
            "--inspect-target",
        ],
    );
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["provenance"]["mode"], "flag");
    config(&dir, "192.0.2.7:7700", Some("0.0.0.0:0"));
    let out = run(&path, &["aggregator", "ls", "--inspect-target"]);
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["provenance"]["bind"], "profile");
    for flags in [
        &["--bind", "127.0.0.1:0"][..],
        &["--bind", "[::]:0"],
        &["--local", "--bind", "0.0.0.0:0"],
    ] {
        let mut args = vec!["aggregator", "ls", "--inspect-target"];
        args.extend_from_slice(flags);
        let out = run(&path, &args);
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
    }
    for target in ["0.0.0.0:7700", "224.0.0.1:7700", "192.0.2.7:0"] {
        config(&dir, target, Some("0.0.0.0:0"));
        let out = run(&path, &["aggregator", "ls", "--inspect-target"]);
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
    }
    config(&dir, "[2001:db8::7]:7700", Some("[::]:0"));
    let out = run(&path, &["aggregator", "ls", "--inspect-target"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["bind"], "[::]:0");
}

#[test]
fn offline_typegen_refuses_remote_only_flags_before_output() {
    let dir = tempfile::tempdir().unwrap();
    let path = config(&dir, "127.0.0.1:9", None);
    let input = dir.path().join("not-read.json");
    let dest = dir.path().join("not-created");
    for flag in [&["--bind", "0.0.0.0:0"][..], &["--node-id", "7"]] {
        let mut args = vec![
            "typegen",
            "generate",
            "--language",
            "ts",
            "--from-snapshot",
            input.to_str().unwrap(),
            "--out",
            dest.to_str().unwrap(),
        ];
        args.extend_from_slice(flag);
        let out = run(&path, &args);
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
        assert!(String::from_utf8_lossy(&out.stderr).contains("for live typegen"));
        assert!(!dest.exists());
    }
}

#[cfg(feature = "rtc-bootstrap")]
#[test]
fn anchor_clients_inspect_without_connecting() {
    let dir = tempfile::tempdir().unwrap();
    let path = config(&dir, "127.0.0.1:9", None);
    for verb in ["ls", "stats"] {
        let out = run(&path, &["anchor", verb, "--inspect-target"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(v["target"]["node_id"], 9);
    }
}

/// Two real participants on this runner's non-loopback interface, not two
/// computers. The provider observes the actual source IP, so a renderer-only
/// bind change cannot pass. UDP connect selects a route without sending data.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_typegen_consumes_the_inspected_non_loopback_bind() {
    use std::sync::Arc;
    let route = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
    route
        .connect("192.0.2.1:9")
        .expect("test runner needs an IPv4 route to select its interface");
    let ip = route.local_addr().unwrap().ip();
    assert!(!ip.is_loopback() && !ip.is_unspecified());
    let bind = format!("{ip}:0");
    let host = Arc::new(
        net_sdk::MeshBuilder::new(&bind, &[0x42; 32])
            .unwrap()
            .build()
            .await
            .unwrap(),
    );
    host.start();
    let descriptor: net_sdk::tool::ToolDescriptor = serde_json::from_value(serde_json::json!({
        "tool_id": "bind_probe", "name": "Bind Probe", "version": "1.0.0",
        "description": "Non-loopback acquisition witness",
        "input_schema": "{\"type\":\"object\",\"properties\":{\"text\":{\"type\":\"string\"}},\"required\":[\"text\"]}",
        "output_schema": null, "requires": [], "estimated_time_ms": 1,
        "stateless": true, "streaming": false, "tags": ["bind-test"], "node_count": 1
    })).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let profile = config(&dir, &host.inner().local_addr().to_string(), Some(&bind));
    let body = std::fs::read_to_string(&profile)
        .unwrap()
        .replace(KEY, &hex::encode(host.inner().public_key()))
        .replace("node_id = '9'", &format!("node_id = '{}'", host.node_id()));
    std::fs::write(&profile, body).unwrap();
    let seed = [7u8; 32];
    let identity = net_sdk::identity::Identity::from_bytes(&seed).unwrap();
    let client_id = identity.node_id();
    let identity_path = dir.path().join("identity.toml");
    std::fs::write(
        &identity_path,
        format!("seed_hex = '{}'\n", hex::encode(seed)),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&identity_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let snapshot = dir.path().join("tools.json");
    let inspection = run(
        &profile,
        &[
            "typegen",
            "snapshot",
            "--tool",
            "bind_probe",
            "--identity",
            identity_path.to_str().unwrap(),
            "--out",
            snapshot.to_str().unwrap(),
            "--inspect-target",
        ],
    );
    assert!(
        inspection.status.success(),
        "{}",
        String::from_utf8_lossy(&inspection.stderr)
    );
    let view: Value = serde_json::from_slice(&inspection.stdout).unwrap();
    assert_eq!(view["bind"], bind);
    assert!(!snapshot.exists());
    assert!(host.inner().peer_addr(client_id).is_none());

    let announcer_host = Arc::clone(&host);
    let announcer = tokio::spawn(async move {
        // Publish only after attach: an earlier announcement would consume
        // the provider's 10s rate-limit window before the CLI's 5s discovery.
        while announcer_host.inner().peer_addr(client_id).is_none() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let _served = announcer_host
            .serve_tool::<Value, Value, _, _>(descriptor, |value| async move { Ok(value) })
            .unwrap();
        loop {
            announcer_host
                .announce_capabilities(net_sdk::capabilities::CapabilitySet::new())
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });
    let snapshot_arg = snapshot.clone();
    let result = tokio::task::spawn_blocking(move || {
        Command::cargo_bin("net-mesh")
            .unwrap()
            .timeout(Duration::from_secs(30))
            .args(["--config"])
            .arg(profile)
            .args(["typegen", "snapshot", "--tool", "bind_probe", "--identity"])
            .arg(identity_path)
            .arg("--out")
            .arg(snapshot_arg)
            .args(["--output", "json"])
            .assert()
            .get_output()
            .clone()
    })
    .await
    .unwrap();
    announcer.abort();
    let _ = announcer.await;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(host.inner().peer_addr(client_id).unwrap().ip(), ip);
    let captured: Value = serde_json::from_slice(&std::fs::read(&snapshot).unwrap()).unwrap();
    assert_eq!(
        captured["descriptors"][0]["tool_id"],
        "bind_probe",
        "{captured}; stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    Arc::try_unwrap(host)
        .ok()
        .expect("host references released")
        .shutdown()
        .await
        .unwrap();
    let generated = dir.path().join("generated");
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["typegen", "generate", "--language", "ts", "--from-snapshot"])
        .arg(snapshot)
        .arg("--out")
        .arg(&generated)
        .assert()
        .success();
    assert!(generated.join("tools/bind_probe.ts").exists());
}
