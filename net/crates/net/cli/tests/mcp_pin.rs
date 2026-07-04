//! `net mcp pin` end-to-end: approve → list → reject → list against an
//! isolated pin-store file. Exercises the CLI wiring, path override, and the
//! JSON output contract without touching the real per-user store or a mesh.

use assert_cmd::Command;
use predicates::prelude::*;

fn net_mesh() -> Command {
    Command::cargo_bin("net-mesh").expect("net-mesh binary")
}

const CAP: &str = "homelab/github.create_issue";

#[test]
fn pin_approve_then_list_then_reject_round_trips() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = dir.path().join("pins.json");

    // Approve — reports the capability as approved.
    net_mesh()
        .args([
            "mcp",
            "pin",
            "approve",
            CAP,
            "--output",
            "json",
            "--pin-store",
        ])
        .arg(&store)
        .assert()
        .success()
        .stdout(predicate::str::contains(CAP))
        .stdout(predicate::str::contains("approved"));

    // List — the capability shows up as approved.
    net_mesh()
        .args(["mcp", "pin", "list", "--output", "json", "--pin-store"])
        .arg(&store)
        .assert()
        .success()
        .stdout(predicate::str::contains(CAP))
        .stdout(predicate::str::contains("approved"));

    // Reject — removes it.
    net_mesh()
        .args([
            "mcp",
            "pin",
            "reject",
            CAP,
            "--output",
            "json",
            "--pin-store",
        ])
        .arg(&store)
        .assert()
        .success()
        .stdout(predicate::str::contains("rejected"));

    // List again — gone.
    net_mesh()
        .args(["mcp", "pin", "list", "--output", "json", "--pin-store"])
        .arg(&store)
        .assert()
        .success()
        .stdout(predicate::str::contains(CAP).not());
}

#[test]
fn pin_approve_rejects_a_malformed_cap_id() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = dir.path().join("pins.json");
    // No `/` — not a `provider/capability` id.
    net_mesh()
        .args(["mcp", "pin", "approve", "bareword", "--pin-store"])
        .arg(&store)
        .assert()
        .failure();
}
