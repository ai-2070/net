//! Documented exit-code coverage.
//!
//! `NET_CLI_PLAN.md:§"Exit codes (locked)"` defines the
//! discriminator → code table. This test pins each documented
//! code by invoking a fixture that produces it.
//!
//! Codes covered today (Phase 1):
//! - 0   Success            — `net version`.
//! - 1   Generic            — `net identity show /nonexistent`.
//! - 2   InvalidArgs        — `net snapshot get --bogus`.
//! - 3   SDK error          — covered by other commands (no
//!                            fixture pinned in Phase 1 because
//!                            triggering a substrate error
//!                            deterministically requires a
//!                            larger harness).
//!
//! Codes 4–8 + 10–12 will be pinned as the matching command
//! surfaces ship (ICE / signature verification / confirmation /
//! daemon factory / db query parse).

use assert_cmd::prelude::*;
use std::process::Command;

#[test]
fn code_0_on_success() {
    Command::cargo_bin("net")
        .unwrap()
        .arg("version")
        .assert()
        .code(0);
}

#[test]
fn code_1_on_generic_error_missing_identity_file() {
    Command::cargo_bin("net")
        .unwrap()
        .args(["identity", "show", "/this/path/definitely/does/not/exist.toml"])
        .assert()
        .code(1);
}

#[test]
fn code_2_on_invalid_args_unknown_flag() {
    Command::cargo_bin("net")
        .unwrap()
        .args(["snapshot", "get", "--this-flag-does-not-exist"])
        .assert()
        .code(2);
}

#[test]
fn code_2_on_invalid_args_unknown_subcommand() {
    Command::cargo_bin("net")
        .unwrap()
        .arg("this-subcommand-does-not-exist")
        .assert()
        .code(2);
}

#[test]
fn code_2_on_invalid_log_level() {
    // `net log tail --min-level <bogus>` parses the flag in our
    // own handler, not clap, so it surfaces as code 2
    // (InvalidArgs) — confirms our manual parser routes through
    // the typed error surface.
    Command::cargo_bin("net")
        .unwrap()
        .args(["log", "tail", "--min-level", "no-such-level"])
        .assert()
        .code(2);
}
