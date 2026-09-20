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
