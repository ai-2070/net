//! OA-2 grant CLI flow — `net org grant-dispatcher` (grant-capability
//! lands in OA2-F3). Drives the real `net-mesh` binary against a
//! tempdir: a dispatcher grant is written as a versioned JSON
//! envelope, overwrite is refused, and scope selection is validated.

use assert_cmd::prelude::*;
use std::path::Path;
use std::process::Command;

fn keygen(dir: &Path, name: &str) -> std::path::PathBuf {
    let key = dir.join(name);
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "keygen", "--out"])
        .arg(&key)
        .assert()
        .code(0);
    key
}

/// A stable fake dispatcher entity id (any 32 bytes decode as an
/// EntityId; issuance only binds the public half).
const DISPATCHER_HEX: &str = "0707070707070707070707070707070707070707070707070707070707070707";

#[test]
fn grant_dispatcher_writes_grant_and_refuses_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let key = keygen(dir.path(), "org.toml");
    let grant = dir.path().join("dispatcher.grant.json");

    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "grant-dispatcher", "--org-key"])
        .arg(&key)
        .args(["--dispatcher", DISPATCHER_HEX])
        .args(["--capability", "nrpc:svc"])
        .args(["--out"])
        .arg(&grant)
        .assert()
        .code(0);

    let text = std::fs::read_to_string(&grant).unwrap();
    assert!(
        text.contains("\"version\": 1"),
        "envelope carries version 1: {text}"
    );
    assert!(
        text.contains("\"grant\""),
        "envelope carries the grant field"
    );

    // Refuses to clobber without --force.
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "grant-dispatcher", "--org-key"])
        .arg(&key)
        .args(["--dispatcher", DISPATCHER_HEX])
        .args(["--capability", "nrpc:svc"])
        .args(["--out"])
        .arg(&grant)
        .assert()
        .code(2);
}

#[test]
fn grant_dispatcher_any_capability_and_scope_validation() {
    let dir = tempfile::tempdir().unwrap();
    let key = keygen(dir.path(), "org.toml");

    // `--any-capability` alone → OK.
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "grant-dispatcher", "--org-key"])
        .arg(&key)
        .args(["--dispatcher", DISPATCHER_HEX])
        .arg("--any-capability")
        .args(["--out"])
        .arg(dir.path().join("any.grant.json"))
        .assert()
        .code(0);

    // Both `--capability` and `--any-capability` → refused.
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "grant-dispatcher", "--org-key"])
        .arg(&key)
        .args(["--dispatcher", DISPATCHER_HEX])
        .args(["--capability", "nrpc:svc"])
        .arg("--any-capability")
        .args(["--out"])
        .arg(dir.path().join("both.grant.json"))
        .assert()
        .code(2);

    // Neither scope flag → refused.
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "grant-dispatcher", "--org-key"])
        .arg(&key)
        .args(["--dispatcher", DISPATCHER_HEX])
        .args(["--out"])
        .arg(dir.path().join("neither.grant.json"))
        .assert()
        .code(2);
}

/// Stable fake org / node ids (any 32 bytes are a valid OrgId /
/// EntityId; issuance binds the value, not a live key).
const GRANTEE_ORG_HEX: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const TARGET_ORG_HEX: &str = "2222222222222222222222222222222222222222222222222222222222222222";
const TARGET_NODE_HEX: &str = "3333333333333333333333333333333333333333333333333333333333333333";

#[test]
fn grant_capability_invoke_only_writes_grant_without_secret() {
    let dir = tempfile::tempdir().unwrap();
    let key = keygen(dir.path(), "org.toml");
    let grant = dir.path().join("cap.grant.json");

    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "grant-capability", "--org-key"])
        .arg(&key)
        .args(["--grantee-org", GRANTEE_ORG_HEX])
        .args(["--capability", "nrpc:svc"])
        .arg("--invoke")
        .args(["--target-any-owned-by", TARGET_ORG_HEX])
        .args(["--out"])
        .arg(&grant)
        .assert()
        .code(0);

    let text = std::fs::read_to_string(&grant).unwrap();
    assert!(
        text.contains("\"version\": 1"),
        "envelope carries version 1"
    );
    assert!(
        text.contains("\"grant\""),
        "envelope carries the grant field"
    );
    // An INVOKE-only grant mints no audience secret.
    assert!(!dir.path().join("cap.audience.key").exists());
}

#[test]
fn grant_capability_discover_mints_secret_file_0600() {
    let dir = tempfile::tempdir().unwrap();
    let key = keygen(dir.path(), "org.toml");
    let grant = dir.path().join("cap.grant.json");
    let secret = dir.path().join("cap.audience.key");

    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "grant-capability", "--org-key"])
        .arg(&key)
        .args(["--grantee-org", GRANTEE_ORG_HEX])
        .args(["--capability", "nrpc:svc"])
        .arg("--invoke")
        .arg("--discover")
        .args(["--target-node", TARGET_NODE_HEX])
        .args(["--out"])
        .arg(&grant)
        .args(["--audience-out"])
        .arg(&secret)
        .assert()
        .code(0);

    assert!(grant.exists(), "grant file written");
    let secret_bytes = std::fs::read(&secret).unwrap();
    assert_eq!(
        secret_bytes.len(),
        97,
        "audience secret is the canonical 97-byte encode_config",
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&secret).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o077,
            0,
            "audience secret must be owner-only, got {mode:o}"
        );
    }
}

#[test]
fn grant_capability_flag_validation() {
    let dir = tempfile::tempdir().unwrap();
    let key = keygen(dir.path(), "org.toml");
    let out = dir.path().join("x.grant.json");

    // Each of these fails validation BEFORE any file is written.
    for extra in [
        // no rights
        vec!["--target-any-owned-by", TARGET_ORG_HEX],
        // --discover without --audience-out
        vec![
            "--invoke",
            "--discover",
            "--target-any-owned-by",
            TARGET_ORG_HEX,
        ],
        // both target flags
        vec![
            "--invoke",
            "--target-node",
            TARGET_NODE_HEX,
            "--target-any-owned-by",
            TARGET_ORG_HEX,
        ],
        // neither target flag
        vec!["--invoke"],
    ] {
        let mut c = Command::cargo_bin("net-mesh").unwrap();
        c.args(["org", "grant-capability", "--org-key"])
            .arg(&key)
            .args(["--grantee-org", GRANTEE_ORG_HEX])
            .args(["--capability", "nrpc:svc"])
            .args(["--out"])
            .arg(&out);
        for a in &extra {
            c.arg(a);
        }
        c.assert().code(2);
    }

    // --audience-out without --discover → refused.
    let stray_secret = dir.path().join("stray.key");
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "grant-capability", "--org-key"])
        .arg(&key)
        .args(["--grantee-org", GRANTEE_ORG_HEX])
        .args(["--capability", "nrpc:svc"])
        .arg("--invoke")
        .args(["--target-any-owned-by", TARGET_ORG_HEX])
        .args(["--out"])
        .arg(&out)
        .args(["--audience-out"])
        .arg(&stray_secret)
        .assert()
        .code(2);
    assert!(
        !stray_secret.exists(),
        "no secret written on a rejected run"
    );
}
