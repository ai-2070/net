//! Help-text + version sanity tests.
//!
//! These don't pin the *content* of the help output (clap's
//! formatting changes across versions; brittle goldens would
//! generate churn without value). Instead they assert:
//!
//! 1. `--help` succeeds + emits something non-empty.
//! 2. Every documented subcommand has its own working `--help`.
//! 3. `--version` succeeds.
//! 4. `version` subcommand emits valid JSON.

use assert_cmd::prelude::*;
use predicates::prelude::*;
use std::process::Command;

const SUBCOMMANDS: &[&str] = &[
    "version",
    "identity",
    "snapshot",
    "audit",
    "log",
    "failures",
    "cap",
    "peer",
    "daemon",
    "netdb",
];

#[test]
fn help_succeeds() {
    Command::cargo_bin("net")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("net"));
}

#[test]
fn version_flag_succeeds() {
    Command::cargo_bin("net")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("0.17.0"));
}

#[test]
fn every_top_level_subcommand_has_help() {
    for sub in SUBCOMMANDS {
        let assert = Command::cargo_bin("net")
            .unwrap()
            .args([sub, "--help"])
            .assert();
        assert
            .success()
            .stdout(predicate::str::is_empty().not());
    }
}

#[test]
fn version_subcommand_emits_json() {
    let output = Command::cargo_bin("net")
        .unwrap()
        .arg("version")
        .output()
        .unwrap();
    assert!(output.status.success(), "version failed: {output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value: serde_json::Value =
        serde_json::from_str(&stdout).expect("version output should be valid JSON");
    assert!(value.get("cli_version").is_some(), "missing cli_version");
    assert!(value.get("sdk_version").is_some(), "missing sdk_version");
}
