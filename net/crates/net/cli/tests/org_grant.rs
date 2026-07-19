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
