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
fn identity_targets_do_not_generate_read_or_revoke_artifacts() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.toml");
    config(
        &cfg,
        "[default]\nnode_addr = 'unused'\nidentity = 'unused-missing'\n",
    );
    let artifact = dir.path().join("identity.toml");
    let store = dir.path().join("revocations.json");
    let issuer = "0101010101010101010101010101010101010101010101010101010101010101";
    for command in [
        vec!["identity", "generate", "--out", artifact.to_str().unwrap()],
        vec!["identity", "show", artifact.to_str().unwrap()],
        vec!["identity", "fingerprint", artifact.to_str().unwrap()],
        vec![
            "identity",
            "revoke",
            issuer,
            "--revocation-store",
            store.to_str().unwrap(),
        ],
    ] {
        let verb = command[1];
        let mut args = vec!["--config", cfg.to_str().unwrap()];
        args.extend(command);
        args.push("--inspect-target");
        let out = run(&args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let view: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(view["ignored_profile_remote_defaults"], true);
        assert_eq!(view["authorization"], "not_checked");
        match verb {
            "generate" => {
                assert_eq!(view["destination"], artifact.to_str().unwrap());
                assert_eq!(view["identity"]["state"], "unavailable");
            }
            "show" | "fingerprint" => {
                assert_eq!(view["source"], artifact.to_str().unwrap());
                assert_eq!(view["identity"]["state"], "unused");
            }
            "revoke" => {
                assert_eq!(view["store"], store.to_str().unwrap());
                assert!(view["subject_fingerprint"].as_str().is_some());
                assert!(!String::from_utf8_lossy(&out.stdout).contains(issuer));
            }
            _ => unreachable!(),
        }
        assert!(!artifact.exists() && !store.exists());
    }
    let out = run(&[
        "--config",
        cfg.to_str().unwrap(),
        "identity",
        "generate",
        "--inspect-target",
    ]);
    assert!(out.status.success());
    let view: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(view.get("destination").is_none());
    assert!(view["destination_pattern"]
        .as_str()
        .unwrap()
        .contains("<generated-operator-id>"));
    assert_eq!(view["identity"]["state"], "unavailable");
    assert!(view["identity"]["fingerprint"].is_null());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    config(&artifact, "PRIVATE_SENTINEL corrupt identity");
    for verb in ["show", "fingerprint"] {
        let out = run(&[
            "--config",
            cfg.to_str().unwrap(),
            "identity",
            verb,
            artifact.to_str().unwrap(),
            "--inspect-target",
        ]);
        assert!(out.status.success());
        assert!(!String::from_utf8_lossy(&out.stdout).contains("PRIVATE_SENTINEL"));
    }
    let out = run(&[
        "--config",
        cfg.to_str().unwrap(),
        "identity",
        "generate",
        "--out",
        artifact.to_str().unwrap(),
        "--force",
        "--inspect-target",
    ]);
    assert!(out.status.success());
    assert_eq!(
        std::fs::read(&artifact).unwrap(),
        b"PRIVATE_SENTINEL corrupt identity"
    );
}

#[test]
fn policy_pins_and_staging_inspect_without_accessing_contents() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.toml");
    config(
        &cfg,
        "[default]\nnode_addr = 'unused'\nidentity = 'missing'\n",
    );
    let store = dir.path().join("store");
    let path = store.to_str().unwrap();
    let commands = [
        vec!["forwarding", "enable", "--store", path],
        vec!["forwarding", "disable", "--store", path],
        vec![
            "forwarding",
            "allow",
            "test",
            "--header",
            "Authorization",
            "--store",
            path,
        ],
        vec!["forwarding", "rm", "test", "--store", path],
        vec!["forwarding", "audit", "--store", path],
        vec![
            "mcp",
            "pin",
            "approve",
            "provider/tool",
            "--pin-store",
            path,
        ],
        vec!["mcp", "pin", "reject", "provider/tool", "--pin-store", path],
        vec!["mcp", "pin", "list", "--pin-store", path],
        vec!["transfer", "send-blob", "missing", "--store", path],
        vec!["transfer", "send-dir", "missing", "--store", path],
    ];
    // First absent, then corrupt: neither inspection may create a lock, load
    // the policy/pin payload, enumerate content, or alter existing bytes.
    for existing in [false, true] {
        if existing {
            std::fs::write(&store, b"PRIVATE_SENTINEL invalid store").unwrap();
        }
        for command in &commands {
            let mut args = vec!["--config", cfg.to_str().unwrap()];
            args.extend(command.iter().copied());
            args.push("--inspect-target");
            let out = run(&args);
            assert!(
                out.status.success(),
                "{args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let view: Value = serde_json::from_slice(&out.stdout).unwrap();
            assert_eq!(view["store"], path);
            assert_eq!(view["provenance"]["store"], "flag");
            assert_eq!(view["mode"], "persistent_store");
            assert_eq!(view["identity"]["state"], "unused");
            assert_eq!(view["ignored_profile_remote_defaults"], true);
            assert!(!String::from_utf8_lossy(&out.stdout).contains("PRIVATE_SENTINEL"));
            assert_eq!(
                std::fs::read_dir(dir.path()).unwrap().count(),
                if existing { 2 } else { 1 }
            );
        }
    }
    assert_eq!(
        std::fs::read(store).unwrap(),
        b"PRIVATE_SENTINEL invalid store"
    );
    for verb in ["send-blob", "send-dir"] {
        let out = run(&[
            "--config",
            cfg.to_str().unwrap(),
            "transfer",
            verb,
            "-",
            "--inspect-target",
        ]);
        assert!(out.status.success());
        let view: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(view["mode"], "offline");
        assert_eq!(view["source"], "-");
        assert!(view.get("store").is_none());
    }
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

#[test]
fn announcement_inspection_reports_the_real_signer_without_signing_or_output() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.toml");
    config(
        &cfg,
        "[default]\nnode_addr = 'unused'\nidentity = 'unused-missing'\n",
    );
    let key = dir.path().join("identity.toml");
    let out = run(&[
        "--config",
        cfg.to_str().unwrap(),
        "identity",
        "generate",
        "--out",
        key.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    let before = std::fs::read(&key).unwrap();
    let fingerprint = run(&["identity", "fingerprint", key.to_str().unwrap()]);
    assert!(fingerprint.status.success());
    let fingerprint: Value = serde_json::from_slice(&fingerprint.stdout).unwrap();
    let dest = dir.path().join("announcement.json");
    let args = [
        "--config",
        cfg.to_str().unwrap(),
        "cap",
        "announce",
        "--key",
        key.to_str().unwrap(),
        "--tag",
        "example.tool",
        "--out",
        dest.to_str().unwrap(),
    ];
    let mut inspect = args.to_vec();
    inspect.push("--inspect-target");
    let out = run(&inspect);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let view: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(view["mode"], "offline");
    assert_eq!(view["identity"]["state"], "configured");
    assert_eq!(view["identity"]["fingerprint"], fingerprint["fingerprint"]);
    assert_eq!(view["destination"], dest.to_str().unwrap());
    assert_eq!(view["provenance"]["identity"], "flag");
    let key_file: toml::Value = toml::from_str(std::str::from_utf8(&before).unwrap()).unwrap();
    assert!(!String::from_utf8_lossy(&out.stdout).contains(key_file["seed_hex"].as_str().unwrap()));
    assert_eq!(std::fs::read(&key).unwrap(), before);
    assert!(!dest.exists());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    let mut stdout_inspect = args[..args.len() - 2].to_vec();
    stdout_inspect.push("--inspect-target");
    let out = run(&stdout_inspect);
    assert!(out.status.success());
    let view: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(view["provenance"]["destination"], "stdout");
    assert!(view.get("destination").is_none());
    let mut wrong_node = inspect.clone();
    wrong_node.extend(["--node-id", "0"]);
    let out = run(&wrong_node);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(!dest.exists());
    let out = run(&args);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(dest.is_file());
    let announcement =
        net_sdk::capabilities::CapabilityAnnouncement::from_bytes(&std::fs::read(&dest).unwrap())
            .unwrap();
    announcement.verify().unwrap();
    assert_eq!(
        hex::encode(announcement.entity_id.as_bytes()),
        key_file["public_key_hex"].as_str().unwrap()
    );
    config(&key, "seed_hex = 'PRIVATE_SENTINEL\n");
    let out = run(&inspect);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&out.stderr).contains("PRIVATE_SENTINEL"));
}

#[test]
fn policy_and_pin_execution_use_inspected_paths_and_defaults_stay_distinct() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.toml");
    config(&cfg, "[default]\nnetdb = 'unrelated-netdb'\n");
    for (command, flag, name) in [
        (vec!["forwarding", "enable"], "--store", "forwarding.json"),
        (
            vec!["mcp", "pin", "approve", "provider/tool"],
            "--pin-store",
            "pins.json",
        ),
    ] {
        let store = dir.path().join(name);
        let mut args = vec!["--config", cfg.to_str().unwrap()];
        args.extend(command);
        args.extend([flag, store.to_str().unwrap()]);
        let mut inspect = args.clone();
        inspect.push("--inspect-target");
        let out = run(&inspect);
        assert!(out.status.success());
        let view: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert!(!store.exists());
        let out = run(&args);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let result: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(view["store"], result["store"]);
        assert!(store.is_file());
    }
    let mut defaults = Vec::new();
    for command in [vec!["forwarding", "audit"], vec!["mcp", "pin", "list"]] {
        let mut args = vec!["--config", cfg.to_str().unwrap()];
        args.extend(command);
        args.push("--inspect-target");
        let out = run(&args);
        assert!(out.status.success());
        let view: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(view["provenance"]["store"], "default");
        defaults.push(view["store"].clone());
    }
    assert_ne!(defaults[0], defaults[1]);
    assert_ne!(defaults[0], "unrelated-netdb");
    assert_ne!(defaults[1], "unrelated-netdb");
    let out = run(&[
        "--config",
        cfg.to_str().unwrap(),
        "forwarding",
        "set-value",
        "test",
        "--inspect-target",
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
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
