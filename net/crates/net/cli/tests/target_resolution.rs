//! Target resolution must be inspectable without starting a supervisor or mesh.

use assert_cmd::Command;
use serde_json::Value;
use std::path::Path;
use std::time::Duration;

const KEY: &str = "0101010101010101010101010101010101010101010101010101010101010101";
const PSK: &str = "4242424242424242424242424242424242424242424242424242424242424242";

fn write_config(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

fn config_text(addr: &str, id: u64) -> String {
    format!("[default]\nnode_addr = '{addr}'\nnode_pubkey = '{KEY}'\nnode_id = '{id}'\npsk_hex = '{PSK}'\n")
}

fn run(path: &Path, flags: &[&str]) -> std::process::Output {
    Command::cargo_bin("net-mesh")
        .unwrap()
        .timeout(Duration::from_secs(30))
        .args(["aggregator", "ls", "--output", "json", "--config"])
        .arg(path)
        .args(flags)
        .assert()
        .get_output()
        .clone()
}

fn inspect(path: &Path, flags: &[&str]) -> Value {
    let mut args = flags.to_vec();
    args.push("--inspect-target");
    let out = run(path, &args);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(!text.contains(PSK));
    assert!(!text.contains(KEY));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("ephemeral keypair"));
    assert!(!stderr.contains("Starts a temporary supervisor"));
    serde_json::from_str(&text).unwrap()
}

#[test]
fn flags_profiles_and_overrides_resolve_without_network_activity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    // Bound socket deliberately has no responder. Inspection must send no UDP.
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.set_nonblocking(true).unwrap();
    let addr = socket.local_addr().unwrap().to_string();
    write_config(&path, "");
    let flags = [
        "--node-addr",
        &addr,
        "--node-pubkey",
        KEY,
        "--node-id",
        "7",
        "--psk-hex",
        PSK,
    ];
    let v = inspect(&path, &flags);
    assert_eq!(v["mode"], "remote");
    assert_eq!(v["target"]["node_id"], 7);
    assert_eq!(v["provenance"]["node_addr"], "flag");
    assert_eq!(v["identity"]["state"], "unavailable");
    write_config(&path, &config_text(&addr, 9));
    let before = std::fs::read(&path).unwrap();
    let v = inspect(&path, &[]);
    assert_eq!(v["target"]["node_id"], 9);
    assert_eq!(v["provenance"]["node_addr"], "profile");
    assert_eq!(v["bind"], "127.0.0.1:0");
    let v = inspect(&path, &["--node-id", "11"]);
    assert_eq!(v["target"]["node_id"], 11);
    assert_eq!(v["provenance"]["node_id"], "flag");
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    let mut packet = [0; 2048];
    assert_eq!(
        socket.recv(&mut packet).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn explicit_local_ignores_profile_remote_defaults_but_not_explicit_targets() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    write_config(&path, &config_text("127.0.0.1:9", 9));
    let v = inspect(&path, &["--local"]);
    assert_eq!(v["mode"], "temporary_supervisor");
    assert!(v["target"].is_null());
    assert_eq!(v["ignored_profile_remote_defaults"], true);
    for flags in [&["--local", "--remote"][..], &["--local", "--node-id", "2"]] {
        let out = run(&path, flags);
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
        assert!(String::from_utf8_lossy(&out.stderr).contains("conflicts"));
    }
}

#[test]
fn partial_profile_does_not_become_a_local_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    write_config(&path, &format!("[default]\npsk_hex = '{PSK}'\n"));
    let out = run(&path, &[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("requires --node-addr"));
}

#[test]
fn explicit_missing_config_and_unknown_profile_are_errors() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing.toml");
    let out = run(&path, &["--local"]);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert!(!path.exists());
    write_config(&path, "");
    let out = run(&path, &["--local", "--profile", "does-not-exist"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown profile"));
}

#[test]
fn inspection_uses_the_configured_public_identity_without_revealing_seed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let identity = dir.path().join("identity.toml");
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["identity", "generate", "--out"])
        .arg(&identity)
        .assert()
        .success();
    write_config(
        &path,
        &format!("[default]\nidentity = '{}'\n", identity.display()),
    );
    let fingerprint = Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["identity", "fingerprint"])
        .arg(&identity)
        .args(["--output", "json"])
        .assert()
        .success()
        .get_output()
        .clone();
    let expected: Value = serde_json::from_slice(&fingerprint.stdout).unwrap();
    let before = std::fs::read(&identity).unwrap();
    let v = inspect(&path, &["--local"]);
    assert_eq!(v["identity"]["state"], "configured");
    assert_eq!(v["identity"]["fingerprint"], expected["fingerprint"]);
    assert_eq!(v["authorization"], "not_checked");
    assert_eq!(std::fs::read(&identity).unwrap(), before);
    let file: toml::Value = toml::from_str(std::str::from_utf8(&before).unwrap()).unwrap();
    assert!(!v.to_string().contains(file["seed_hex"].as_str().unwrap()));
    write_config(&path, "");
    let v = inspect(
        &path,
        &["--local", "--identity", identity.to_str().unwrap()],
    );
    assert_eq!(v["provenance"]["identity"], "flag");
    assert_eq!(v["identity"]["fingerprint"], expected["fingerprint"]);
}

#[test]
fn malformed_config_and_identity_fail_inspection_without_secret_echo() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    write_config(&path, &format!("[default]\npsk_hex = '{PSK}"));
    let out = run(&path, &["--inspect-target", "--local"]);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&out.stderr).contains(PSK));
    let identity = dir.path().join("identity.toml");
    write_config(&identity, &format!("seed_hex = '{PSK}"));
    write_config(&path, "");
    let out = run(
        &path,
        &[
            "--inspect-target",
            "--local",
            "--identity",
            identity.to_str().unwrap(),
        ],
    );
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&out.stderr).contains(PSK));
}

#[test]
fn unavailable_remote_never_falls_back_to_local() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    write_config(
        &path,
        &config_text(&socket.local_addr().unwrap().to_string(), 19),
    );
    let out = run(&path, &[]);
    assert_eq!(
        out.status.code(),
        Some(6),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&out.stderr).contains("Starts a temporary supervisor"));
}

#[cfg(target_os = "linux")]
#[test]
fn missing_implicit_default_config_remains_optional() {
    let dir = tempfile::tempdir().unwrap();
    let out = Command::cargo_bin("net-mesh")
        .unwrap()
        .env("XDG_CONFIG_HOME", dir.path())
        .env_remove("NET_MESH_CONFIG")
        .env_remove("NET_MESH_PROFILE")
        .args([
            "aggregator",
            "ls",
            "--local",
            "--inspect-target",
            "--output",
            "json",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["mode"], "temporary_supervisor");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}
