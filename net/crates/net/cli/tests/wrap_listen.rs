// SPDX-License-Identifier: MIT OR Apache-2.0
use assert_cmd::Command;
use serde_json::Value;
use std::{path::Path, time::Duration};

fn config(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}
fn run(path: &Path, args: &[&str], child: &Path, marker: &Path) -> std::process::Output {
    Command::cargo_bin("net-mesh")
        .unwrap()
        .timeout(Duration::from_secs(5))
        .env_remove("NET_MESH_CONFIG")
        .env_remove("NET_MESH_PROFILE")
        .arg("--config")
        .arg(path)
        .args(["--output", "json", "wrap", "fixture", "--listen"])
        .args(args)
        .args(["--"])
        .arg(child)
        .arg(marker)
        .output()
        .unwrap()
}

/// Compile the marker-child fixture into `dir`; returns the child binary path.
fn marker_child(dir: &Path) -> std::path::PathBuf {
    let child = dir.join(format!("marker-child{}", std::env::consts::EXE_SUFFIX));
    let status = std::process::Command::new("rustc")
        .arg("--edition=2021")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/marker_child.rs"
        ))
        .arg("-o")
        .arg(&child)
        .status()
        .expect("compile marker_child fixture");
    assert!(status.success());
    child
}
#[test]
fn listener_inspection_uses_profile_psk_and_bind_without_binding_or_starting_child() {
    let dir = tempfile::tempdir().unwrap();
    let child = marker_child(dir.path());
    let marker = dir.path().join("child-started.marker");
    let path = dir.path().join("config.toml");
    let held = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let bind = held.local_addr().unwrap().to_string();
    let psk = "42".repeat(32);
    config(
        &path,
        &format!("[default]\npsk_hex = '{psk}'\nbind = '{bind}'\n"),
    );
    let result = run(&path, &["--inspect-target"], &child, &marker);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let output = String::from_utf8(result.stdout).unwrap();
    assert!(!output.contains(&psk));
    let view: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(view["bind"], bind);
    assert_eq!(view["provenance"]["bind"], "profile");
    assert_eq!(view["provenance"]["psk"], "profile");
    assert_eq!(view["ignored_profile_remote_defaults"], false);
    assert_eq!(view["identity"]["state"], "unavailable");
    assert!(view["target"].is_null());
    let override_psk = format!("0x{}", "24".repeat(32));
    let result = run(
        &path,
        &[
            "--inspect-target",
            "--bind",
            "127.0.0.1:0",
            "--psk-hex",
            &override_psk,
        ],
        &child,
        &marker,
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let view: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(view["bind"], "127.0.0.1:0");
    assert_eq!(view["provenance"]["bind"], "flag");
    assert_eq!(view["provenance"]["psk"], "flag");
    assert!(
        !marker.exists(),
        "listener inspection started the wrapped child"
    );
}
#[test]
fn listener_requires_valid_psk_and_refuses_ambiguous_targets_before_execution() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let psk = "42".repeat(32);
    let child = marker_child(dir.path());
    let marker = dir.path().join("child-started.marker");
    for (profile, args, diagnostic) in [
        (String::new(), vec![], "requires --psk-hex"),
        (
            "[default]\npsk_hex = 'secret-not-hex'\n".into(),
            vec![],
            "exactly 32 bytes",
        ),
        (
            String::new(),
            vec!["--psk-hex", &psk, "--node-addr", "127.0.0.1:9"],
            "conflicts with remote",
        ),
        (
            "[default]\nnode_id = '7'\n".into(),
            vec!["--psk-hex", &psk],
            "conflicts with remote",
        ),
        (
            "[default]\nendpoint = 'http://localhost:1234'\n".into(),
            vec!["--psk-hex", &psk],
            "endpoint",
        ),
        (
            String::new(),
            vec!["--psk-hex", &psk, "--bind", "bad"],
            "IP:port",
        ),
        (
            String::new(),
            vec!["--psk-hex", &psk, "--bind", "224.0.0.1:0"],
            "multicast",
        ),
        (
            String::new(),
            vec!["--psk-hex", &psk, "--bind", "255.255.255.255:0"],
            "broadcast",
        ),
    ] {
        config(&path, &profile);
        let result = run(&path, &args, &child, &marker);
        assert!(
            !marker.exists(),
            "{args:?}: the wrapped child started before validation refused"
        );
        assert!(!result.status.success());
        assert!(result.stdout.is_empty());
        let error = String::from_utf8_lossy(&result.stderr);
        assert!(error.contains(diagnostic), "{error}");
        assert!(!error.contains("secret-not-hex"));
        assert!(!error.contains(&psk));
    }
}
