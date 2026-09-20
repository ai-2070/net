//! Local resolution must not open stores, read payloads or generate identities.
use assert_cmd::Command;
use serde_json::Value;
use std::path::Path;
use std::time::Duration;

fn config(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

fn run(args: &[&str]) -> std::process::Output {
    Command::cargo_bin("net-mesh")
        .unwrap()
        .env_remove("NET_MESH_CONFIG")
        .env_remove("NET_MESH_PROFILE")
        .timeout(Duration::from_secs(5))
        .args(args)
        .args(["--output", "json"])
        .assert()
        .get_output()
        .clone()
}

#[test]
fn explicit_selections_fail_before_offline_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.toml");
    let artifact = dir.path().join("identity.toml");
    for body in [
        None,
        Some("psk_hex = 'PRIVATE_SENTINEL\n"),
        Some("[default]\n"),
    ] {
        if let Some(body) = body {
            config(&cfg, body);
        }
        for command in [
            vec!["version"],
            vec!["identity", "generate", "--out", artifact.to_str().unwrap()],
            vec!["admin", "cordon", "7", "--dry-run"],
        ] {
            let mut args = vec!["--config", cfg.to_str().unwrap()];
            if body == Some("[default]\n") {
                args.extend(["--profile", "typo"]);
            }
            args.extend(command);
            let out = run(&args);
            assert!(!out.status.success(), "{args:?}");
            assert!(out.stdout.is_empty());
            assert!(!String::from_utf8_lossy(&out.stderr).contains("PRIVATE_SENTINEL"));
            assert!(!artifact.exists());
        }
    }
}

#[test]
fn netdb_and_saved_typegen_inspect_without_opening_paths() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.toml");
    let store = dir.path().join("missing-store");
    let source = dir.path().join("missing-input");
    let dest = dir.path().join("missing-output");
    config(&cfg, &format!("[default]\nnetdb = '{}'\nnode_addr = 'unused-invalid-target'\nbind = 'unused-invalid-bind'\nidentity = 'missing-identity'\n", store.display().to_string().replace('\\', "/")));
    for command in [
        vec!["netdb", "tasks", "ls"],
        vec!["netdb", "tasks", "create", "7", "--title", "test"],
        vec!["netdb", "tasks", "rename", "7", "--title", "test"],
        vec!["netdb", "tasks", "complete", "7"],
        vec!["netdb", "tasks", "delete", "7"],
        vec!["netdb", "memories", "ls"],
        vec!["netdb", "memories", "store", "7", "--content", "test"],
        vec!["netdb", "memories", "retag", "7", "--tag", "test"],
        vec!["netdb", "memories", "pin", "7"],
        vec!["netdb", "memories", "unpin", "7"],
        vec!["netdb", "memories", "delete", "7"],
        vec!["netdb", "snapshot", "--out", dest.to_str().unwrap()],
        vec![
            "netdb",
            "restore",
            "--from",
            source.to_str().unwrap(),
            "--origin",
            "7",
            "--clear",
        ],
        vec![
            "typegen",
            "generate",
            "--language",
            "ts",
            "--from-snapshot",
            source.to_str().unwrap(),
            "--out",
            dest.to_str().unwrap(),
        ],
    ] {
        let mut args = vec!["--config", cfg.to_str().unwrap()];
        args.extend(command.clone());
        args.push("--inspect-target");
        let out = run(&args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let view: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(view["identity"]["state"], "unused");
        assert!(view["target"].is_null() && view["bind"].is_null());
        assert_eq!(view["ignored_profile_remote_defaults"], true);
        assert_eq!(view["authorization"], "not_checked");
        if command.contains(&source.to_str().unwrap()) {
            assert_eq!(view["source"], source.to_str().unwrap());
            assert_eq!(view["provenance"]["source"], "flag");
        }
        if command.contains(&dest.to_str().unwrap()) {
            assert_eq!(view["destination"], dest.to_str().unwrap());
            assert_eq!(view["provenance"]["destination"], "flag");
        }
        if command[0] == "netdb" {
            assert_eq!(view["mode"], "persistent_store");
            assert_eq!(Path::new(view["store"].as_str().unwrap()), store);
            assert_eq!(view["provenance"]["store"], "profile");
        } else {
            assert_eq!(view["mode"], "offline");
        }
        assert!(!store.exists() && !source.exists() && !dest.exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}

#[test]
fn store_override_inspection_and_execution_agree() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.toml");
    let unused = dir.path().join("unused-profile-store");
    let selected = dir.path().join("selected-store");
    config(
        &cfg,
        &format!(
            "[default]\nnetdb = '{}'\n",
            unused.display().to_string().replace('\\', "/")
        ),
    );
    let args = [
        "--config",
        cfg.to_str().unwrap(),
        "netdb",
        "tasks",
        "create",
        "7",
        "--title",
        "resolution witness",
        "--store",
        selected.to_str().unwrap(),
    ];
    let mut inspect_args = args.to_vec();
    inspect_args.push("--inspect-target");
    let out = run(&inspect_args);
    assert!(out.status.success());
    let view: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(view["store"], selected.to_str().unwrap());
    assert_eq!(view["provenance"]["store"], "flag");
    assert!(!selected.exists() && !unused.exists());
    let out = run(&args);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = run(&[
        "--config",
        cfg.to_str().unwrap(),
        "netdb",
        "tasks",
        "ls",
        "--store",
        selected.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    let tasks: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(tasks[0]["title"], "resolution witness");
    assert!(!unused.exists());

    let marker = selected.join("do-not-delete");
    std::fs::write(&marker, b"keep").unwrap();
    let out = run(&[
        "--config",
        cfg.to_str().unwrap(),
        "netdb",
        "restore",
        "--store",
        selected.to_str().unwrap(),
        "--from",
        "missing-snapshot",
        "--clear",
        "--inspect-target",
    ]);
    assert!(out.status.success());
    assert_eq!(std::fs::read(marker).unwrap(), b"keep");

    config(&cfg, "[default]\n");
    let out = run(&[
        "--config",
        cfg.to_str().unwrap(),
        "netdb",
        "tasks",
        "ls",
        "--inspect-target",
    ]);
    assert!(out.status.success());
    let view: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(view["provenance"]["store"], "default");
    assert!(view["store"].as_str().is_some());

    for command in [
        vec!["netdb", "tasks", "ls", "--node-id", "7"],
        vec![
            "typegen",
            "generate",
            "--language",
            "ts",
            "--from-snapshot",
            "missing",
            "--out",
            "unused",
            "--bind",
            "0.0.0.0:0",
        ],
    ] {
        let mut args = vec!["--config", cfg.to_str().unwrap()];
        args.extend(command);
        args.push("--inspect-target");
        let out = run(&args);
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
    }
}

#[test]
fn environment_selections_are_validated_and_flags_override_them() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.toml");
    config(&cfg, "[default]\n");
    let out = Command::cargo_bin("net-mesh")
        .unwrap()
        .env("NET_MESH_CONFIG", &cfg)
        .env("NET_MESH_PROFILE", "typo")
        .args(["version", "--output", "json"])
        .assert()
        .failure()
        .get_output()
        .clone();
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown profile"));
    Command::cargo_bin("net-mesh")
        .unwrap()
        .env("NET_MESH_CONFIG", dir.path().join("missing.toml"))
        .env("NET_MESH_PROFILE", "typo")
        .arg("--config")
        .arg(&cfg)
        .args(["version", "--profile", "default"])
        .assert()
        .success();
}

#[cfg(unix)]
#[test]
fn implicit_config_is_not_a_new_offline_dependency_but_explicit_default_is_checked() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path().join("net-mesh");
    std::fs::create_dir(&config_dir).unwrap();
    config(&config_dir.join("config.toml"), "[broken");
    let base = || {
        let mut cmd = Command::cargo_bin("net-mesh").unwrap();
        cmd.env_remove("NET_MESH_CONFIG")
            .env_remove("NET_MESH_PROFILE")
            .env("XDG_CONFIG_HOME", dir.path())
            .arg("version");
        cmd
    };
    base().assert().success();
    base().args(["--profile", "default"]).assert().failure();
}
