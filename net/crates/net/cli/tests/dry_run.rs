//! `--dry-run` smoke tests for the admin + ICE dispatch surface.
//!
//! The admin `--dry-run` path short-circuits before
//! `CliContext::build`, so these tests exercise the clap routing
//! and the JSON envelope shape without paying the substrate-boot
//! cost. The ICE dry-run still simulates (and therefore boots the
//! supervisor), so this file pins only the admin surface; the
//! pre-existing `code_8_on_ice_confirmation_refused_non_tty` test
//! in `tests/exit_codes.rs` already exercises the ICE simulate
//! path on `freeze-cluster`.

use assert_cmd::prelude::*;
use serde_json::Value;
use std::process::{Command, Stdio};

fn json_stdout(args: &[&str]) -> Value {
    let out = Command::cargo_bin("net")
        .unwrap()
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "command {args:?} failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout was not valid JSON: {e}; got: {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

#[test]
fn admin_drain_dry_run() {
    let v = json_stdout(&["admin", "drain", "0x1", "--drain-for", "5m", "--dry-run"]);
    assert_eq!(v["dry_run"], true);
    assert_eq!(v["envelope"]["kind"], "drain");
    assert_eq!(v["envelope"]["node"], 1);
    assert_eq!(v["envelope"]["drain_for_ms"], 5 * 60 * 1000);
}

#[test]
fn admin_enter_maintenance_dry_run() {
    let v = json_stdout(&["admin", "enter-maintenance", "0x1", "--dry-run"]);
    assert_eq!(v["envelope"]["kind"], "enter_maintenance");
}

#[test]
fn admin_exit_maintenance_dry_run() {
    let v = json_stdout(&["admin", "exit-maintenance", "0x1", "--dry-run"]);
    assert_eq!(v["envelope"]["kind"], "exit_maintenance");
}

#[test]
fn admin_cordon_dry_run() {
    let v = json_stdout(&["admin", "cordon", "0x1", "--dry-run"]);
    assert_eq!(v["envelope"]["kind"], "cordon");
}

#[test]
fn admin_uncordon_dry_run() {
    let v = json_stdout(&["admin", "uncordon", "0x1", "--dry-run"]);
    assert_eq!(v["envelope"]["kind"], "uncordon");
}

#[test]
fn admin_drop_replicas_dry_run() {
    let v = json_stdout(&[
        "admin",
        "drop-replicas",
        "0x1",
        "--chain",
        "0xAB",
        "--chain",
        "0xCD",
        "--dry-run",
    ]);
    assert_eq!(v["envelope"]["kind"], "drop_replicas");
    assert_eq!(v["envelope"]["chains"], serde_json::json!([0xAB, 0xCD]));
}

#[test]
fn admin_invalidate_placement_dry_run() {
    let v = json_stdout(&["admin", "invalidate-placement", "0x1", "--dry-run"]);
    assert_eq!(v["envelope"]["kind"], "invalidate_placement");
}

#[test]
fn admin_restart_all_daemons_dry_run() {
    let v = json_stdout(&["admin", "restart-all-daemons", "0x1", "--dry-run"]);
    assert_eq!(v["envelope"]["kind"], "restart_all_daemons");
}

#[test]
fn admin_clear_avoid_list_dry_run() {
    let v = json_stdout(&["admin", "clear-avoid-list", "0x1", "--dry-run"]);
    assert_eq!(v["envelope"]["kind"], "clear_avoid_list");
}
