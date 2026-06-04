//! Integration test for the `net transfer (ls|status|cancel)` visibility
//! verbs.
//!
//! These verbs are documented stubs today: the substrate
//! `BlobTransferEngine` exposes no transfer-enumeration accessor and a
//! single-shot CLI owns no persistent engine, so they report against an
//! empty local set and emit a `note` documenting the gap
//! (`TRANSFER_CLI_PLAN.md` Gap D). The JSON shapes are advertised as
//! stable so consumers can code against them ahead of the substrate RPC
//! landing (`docs/cli/TRANSFER.md` §6) — these tests pin that contract:
//!
//! - `ls` → `transfer_count: 0`, empty `transfers[]`, a non-empty `note`.
//! - `status <id>` → `found: false`, echoes `transfer_id`, `note`.
//! - `cancel <id>` → `cancelled: false`, `note`.
//! - a non-numeric `<transfer-id>` maps to the typed InvalidArgs exit.
//!
//! None of these touch the network, so no holder is booted.

use assert_cmd::Command as AssertCommand;
use tempfile::TempDir;

fn cli_cmd(home_dir: &TempDir) -> AssertCommand {
    let mut cmd = AssertCommand::cargo_bin("net-mesh").expect("cargo_bin");
    cmd.env("HOME", home_dir.path())
        .env("XDG_CONFIG_HOME", home_dir.path())
        .env("USERPROFILE", home_dir.path());
    cmd
}

/// Run `net-mesh transfer <args...>` and return `(code, stdout, stderr)`.
fn run_transfer(home: &TempDir, args: &[&str]) -> (i32, String, String) {
    let mut cmd = cli_cmd(home);
    cmd.arg("transfer");
    cmd.args(args);
    let output = cmd.output().expect("invoke net-mesh");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn ls_reports_empty_set_with_note() {
    let home = TempDir::new().expect("home");
    let (code, stdout, stderr) = run_transfer(&home, &["ls", "--output", "json"]);
    assert_eq!(code, 0, "ls failed: stderr={stderr}");

    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("non-JSON stdout ({e}): {stdout}"));
    assert_eq!(parsed["transfer_count"], 0, "stdout={stdout}");
    assert_eq!(
        parsed["transfers"].as_array().map(Vec::len),
        Some(0),
        "transfers must be an empty array: {stdout}"
    );
    assert!(
        parsed["note"].as_str().is_some_and(|n| !n.is_empty()),
        "note must be a non-empty string: {stdout}"
    );
}

#[test]
fn status_reports_not_found_with_note() {
    let home = TempDir::new().expect("home");
    let (code, stdout, stderr) = run_transfer(&home, &["status", "12345", "--output", "json"]);
    assert_eq!(code, 0, "status failed: stderr={stderr}");

    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("non-JSON stdout ({e}): {stdout}"));
    assert_eq!(parsed["transfer_id"], "12345", "echoes the id: {stdout}");
    assert_eq!(parsed["found"], false, "stdout={stdout}");
    assert!(
        parsed["note"].as_str().is_some_and(|n| !n.is_empty()),
        "note must be a non-empty string: {stdout}"
    );
}

#[test]
fn cancel_reports_not_cancelled_with_note() {
    let home = TempDir::new().expect("home");
    let (code, stdout, stderr) = run_transfer(&home, &["cancel", "0x2a", "--output", "json"]);
    assert_eq!(code, 0, "cancel failed: stderr={stderr}");

    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("non-JSON stdout ({e}): {stdout}"));
    assert_eq!(parsed["transfer_id"], "0x2a", "echoes the id: {stdout}");
    assert_eq!(parsed["cancelled"], false, "stdout={stdout}");
    assert!(
        parsed["note"].as_str().is_some_and(|n| !n.is_empty()),
        "note must be a non-empty string: {stdout}"
    );
}

#[test]
fn status_rejects_non_numeric_id() {
    // A non-numeric transfer-id is a typed InvalidArgs (exit 2).
    let home = TempDir::new().expect("home");
    let (code, _stdout, _stderr) = run_transfer(&home, &["status", "not-an-id"]);
    assert_eq!(code, 2, "expected InvalidArgs exit code");
}
