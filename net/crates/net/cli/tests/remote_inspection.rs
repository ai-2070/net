//! All remote clients inspect their actual resolution before any effects.
use assert_cmd::Command;
use serde_json::Value;
use std::time::Duration;

const KEY: &str = "0101010101010101010101010101010101010101010101010101010101010101";
const PSK: &str = "4242424242424242424242424242424242424242424242424242424242424242";

#[cfg(feature = "rtc-bootstrap")]
#[test]
fn standalone_anchor_inspects_before_secrets_sockets_or_acme() {
    let dir = tempfile::tempdir().unwrap();
    let path = config(&dir, "192.0.2.7:7700", Some("invalid-unused-profile-bind"));
    let udp = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let tcp = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let bind = udp.local_addr().unwrap().to_string();
    let listen = tcp.local_addr().unwrap().to_string();
    let cache = dir.path().join("not-created");
    let mut args = vec![
        "anchor",
        "serve",
        "--psk-file",
        "missing-psk",
        "--url",
        "https://anchor.example.com",
        "--credential-issuer",
        KEY,
        "--allow-origin",
        "https://app.example.com",
        "--bind",
        &bind,
        "--listen",
        &listen,
        "--acme-directory",
        "https://acme.example.com/directory",
        "--acme-email",
        "ops@example.com",
        "--acme-cache",
        cache.to_str().unwrap(),
        "--inspect-target",
    ];
    let out = run(&path, &args);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let view: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(view["mode"], "hosted_service");
    assert_eq!(view["bind"], bind);
    assert_eq!(view["listen"], listen);
    assert_eq!(view["identity"]["state"], "unavailable");
    assert_eq!(view["ignored_profile_remote_defaults"], true);
    assert_eq!(view["authorization"], "not_checked");
    assert_eq!(view["tls"], "acme");
    assert_eq!(view["acme_cache"], cache.to_str().unwrap());
    assert!(!String::from_utf8_lossy(&out.stdout).contains(KEY));
    assert!(!cache.exists());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    let bind_index = args.iter().position(|arg| *arg == "--bind").unwrap() + 1;
    args[bind_index] = "invalid";
    let out = run(&path, &args);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--bind"));
    assert!(!String::from_utf8_lossy(&out.stderr).contains("missing-psk"));
    args.pop(); // Normal execution rejects the same bad bind before secret IO.
    let out = run(&path, &args);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--bind"));
}

#[cfg(feature = "rtc-bootstrap")]
#[test]
fn standalone_anchor_defaults_and_dispatch_consume_resolution() {
    let dir = tempfile::tempdir().unwrap();
    let path = config(&dir, "192.0.2.7:7700", Some("unused"));
    let psk = dir.path().join("psk.hex");
    let mut args = vec![
        "anchor",
        "serve",
        "--psk-file",
        psk.to_str().unwrap(),
        "--url",
        "https://anchor.example.com",
        "--credential-issuer",
        KEY,
        "--allow-origin",
        "https://app.example.com",
        "--tls-cert",
        "missing-cert",
        "--tls-key",
        "missing-key",
    ];
    let mut inspect = args.clone();
    inspect.push("--inspect-target");
    let out = run(&path, &inspect);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let view: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(view["bind"], "0.0.0.0:0");
    assert_eq!(view["listen"], "0.0.0.0:8443");
    assert_eq!(view["rtc_bind"], "0.0.0.0:0");
    assert_eq!(view["provenance"]["bind"], "default");
    assert_eq!(view["tls"], "operator");
    assert!(view["rtc_stun_bind"].is_null());
    assert!(view["acme_challenge_bind"].is_null());
    assert!(!psk.exists());
    // A real occupied UDP bind distinguishes consuming the inspected address
    // from accidentally keeping a wildcard/ephemeral default in dispatch.
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let bind = socket.local_addr().unwrap().to_string();
    args.extend(["--bind", &bind]);
    let mut inspect = args.clone();
    inspect.push("--inspect-target");
    let out = run(&path, &inspect);
    assert!(out.status.success());
    let view: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(view["bind"], bind);
    assert_eq!(view["provenance"]["bind"], "flag");
    std::fs::write(&psk, PSK).unwrap();
    let out = run(&path, &args);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("starting the anchor"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Missing STUN binding is a resolution error, not a promised endpoint.
    inspect.extend(["--rtc-stun-public-addr", "192.0.2.1:3478"]);
    let out = run(&path, &inspect);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("requires --rtc-stun-bind"));
}

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
    use net::adapter::net::{ChannelConfigRegistry, EntityKeypair, MeshNode, MeshNodeConfig};
    // Avoid an automatic handshake announcement consuming the default 10s
    // coalescing window before this fixture publishes its five-second probe.
    let node_config = MeshNodeConfig::new(bind.parse().unwrap(), [0x42; 32])
        .with_min_announce_interval(Duration::from_millis(50));
    let mut node = MeshNode::new(EntityKeypair::generate(), node_config)
        .await
        .unwrap();
    let channels = Arc::new(ChannelConfigRegistry::new());
    node.set_channel_configs(channels.clone());
    let host = Arc::new(net_sdk::Mesh::from_node_arc(Arc::new(node), channels, None));
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
        // Publish only after attach, using the fixture's short announce cadence.
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
