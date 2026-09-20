//! Temporary supervisors must never masquerade as deployed nodes.

use assert_cmd::Command;
use std::time::Duration;

const SCOPE: &str =
    "Starts a temporary supervisor for this command; does not inspect a running node.";

const COMMANDS: &[&[&str]] = &[
    &["admin", "drain", "1", "--drain-for", "1s"],
    &["admin", "enter-maintenance", "1"],
    &["admin", "exit-maintenance", "1"],
    &["admin", "cordon", "1"],
    &["admin", "uncordon", "1"],
    &["admin", "drop-replicas", "1", "--chain", "2"],
    &["admin", "invalidate-placement", "1"],
    &["admin", "restart-all-daemons", "1"],
    &["admin", "clear-avoid-list", "1"],
    &["ice", "freeze-cluster", "--ttl", "1s"],
    &["ice", "thaw-cluster"],
    &["ice", "flush-avoid-lists", "--scope", "global"],
    &["ice", "force-evict-replica", "1", "2"],
    &["ice", "force-restart-daemon", "1", "--name", "test"],
    &["ice", "force-cutover", "1", "2"],
    &["ice", "kill-migration", "1"],
    &["snapshot", "get"],
    &["snapshot", "status"],
    &["audit", "recent"],
    &["audit", "stream"],
    &["log", "tail"],
    &["failures", "tail"],
    &["cap", "show"],
    &["cap", "query", "--tag", "example"],
    &["cap", "nodes"],
    &["peer", "ls"],
    &["daemon", "ls"],
    &["subnet", "show"],
    &["subnet", "ls"],
    &["subnet", "tree"],
    &["gateway", "stats"],
    &["gateway", "exports"],
    &["channel", "visibility", "example"],
    &["channel", "ls"],
    &["aggregator", "inspect"],
    &["aggregator", "ls"],
];

fn config() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    dir
}

fn run(args: &[&str], config: &std::path::Path) -> std::process::Output {
    Command::cargo_bin("net-mesh")
        .unwrap()
        .timeout(Duration::from_secs(10))
        .args(args)
        .arg("--config")
        .arg(config.join("config.toml"))
        .args(["--output", "json", "--no-color"])
        .assert()
        .get_output()
        .clone()
}

#[test]
fn every_temporary_operation_refuses_without_opt_in() {
    let dir = config();
    for args in COMMANDS {
        let out = run(args, dir.path());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {stderr}");
        assert!(out.stdout.is_empty(), "false success from {args:?}");
        assert!(stderr.contains(SCOPE), "{args:?}: {stderr}");
        assert!(stderr.contains("--local"), "{args:?}: {stderr}");
    }
}

#[test]
fn every_temporary_leaf_help_explains_scope() {
    let dir = config();
    for args in COMMANDS {
        let mut argv = args.to_vec();
        argv.push("--help");
        let out = run(&argv, dir.path());
        assert!(out.status.success(), "{argv:?}");
        let help = String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(help.contains(SCOPE), "{argv:?}: {help}");
    }
}

#[test]
fn explicit_local_reads_keep_json_shapes_and_disclose_scope() {
    let dir = config();
    for args in &COMMANDS[16..] {
        if matches!(
            *args,
            ["audit", "stream"] | ["log", "tail"] | ["failures", "tail"]
        ) {
            continue;
        }
        let mut argv = args.to_vec();
        argv.push("--local");
        let out = run(&argv, dir.path());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{argv:?}: {stderr}");
        let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert!(value.is_object() || value.is_array(), "{argv:?}: {value}");
        assert!(value.get("execution_scope").is_none());
        assert!(stderr.contains(SCOPE), "{argv:?}: {stderr}");
    }
}

#[test]
fn admin_offline_preview_needs_neither_local_nor_configuration() {
    let dir = config();
    std::fs::write(dir.path().join("config.toml"), "invalid=[").unwrap();
    let out = run(&["admin", "cordon", "1", "--dry-run"], dir.path());
    assert!(out.status.success());
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["dry_run"], true);
    assert!(!String::from_utf8_lossy(&out.stderr).contains(SCOPE));
}

#[test]
fn local_signed_admin_and_ice_simulation_work_with_identity() {
    let dir = config();
    let identity = dir.path().join("identity.toml");
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["identity", "generate", "--out"])
        .arg(&identity)
        .assert()
        .success();
    for args in &COMMANDS[..16] {
        let mut argv = args.to_vec();
        argv.extend(["--local", "--identity", identity.to_str().unwrap()]);
        if args[0] == "ice" {
            argv.push("--dry-run");
        }
        let out = run(&argv, dir.path());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{args:?}: {stderr}");
        let _: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert!(stderr.contains(SCOPE), "{args:?}: {stderr}");
    }
}

#[test]
fn local_streams_disclose_scope_and_stay_running() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    use std::sync::mpsc;

    let dir = config();
    for args in [["audit", "stream"], ["log", "tail"], ["failures", "tail"]] {
        let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("net-mesh"))
            .args(args)
            .args(["--local", "--quiet", "--output", "ndjson", "--config"])
            .arg(dir.path().join("config.toml"))
            .env("NET_MESH_LOG", "off")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stderr = child.stderr.take().unwrap();
        let (tx, rx) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if line.contains(SCOPE) {
                    let _ = tx.send(());
                }
            }
        });
        let notice = rx.recv_timeout(Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(100));
        // Always reap even if the assertion will fail.
        let early_exit = child.try_wait().unwrap();
        let _ = child.kill();
        let out = child.wait_with_output().unwrap();
        reader.join().unwrap();
        assert!(notice.is_ok(), "{args:?} missing scope notice");
        assert!(early_exit.is_none(), "{args:?} exited before streaming");
        assert!(out.stdout.is_empty(), "scope leaked onto stream stdout");
    }
}

#[test]
fn local_remote_conflicts_refuse_and_gateway_export_stays_unsupported() {
    let dir = config();
    for flags in [
        &["--remote"][..],
        &["--node-id", "7"],
        &["--node-addr", "127.0.0.1:9"],
    ] {
        let mut args = vec!["aggregator", "ls", "--local"];
        args.extend_from_slice(flags);
        let out = run(&args, dir.path());
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
        assert!(String::from_utf8_lossy(&out.stderr).contains("conflicts"));
    }
    let out = run(&["gateway", "export", "example", "global"], dir.path());
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("read-validation-only"));
}

#[test]
fn generated_man_and_completion_include_local_scope() {
    let dir = config();
    let man = run(&["man"], dir.path());
    assert!(man.status.success());
    let text = String::from_utf8_lossy(&man.stdout);
    assert!(text.contains("Temporary") || text.contains("temporary"));
    let completion = run(&["completion", "bash"], dir.path());
    assert!(completion.status.success());
    let text = String::from_utf8_lossy(&completion.stdout);
    assert!(text.contains("--local"));
}
