//! Documented exit-code coverage.
//!
//! `NET_CLI_PLAN.md:§"Exit codes (locked)"` defines the
//! discriminator → code table. This test pins each documented
//! code by invoking a fixture that produces it.
//!
//! Codes covered today (Phase 1 + Phase 3 ICE):
//! - 0 Success — `net version`.
//! - 1 Generic — `net identity show /nonexistent`.
//! - 2 InvalidArgs — `net snapshot get --bogus`.
//! - 3 SDK error — covered by other commands (no fixture pinned
//!   today because triggering a substrate error deterministically
//!   requires a larger harness).
//! - 8 ConfirmationRefused — `net ice freeze-cluster` with non-TTY
//!   stdin and no `--yes`.
//!
//! Codes 4–7 + 10–12 will be pinned as the matching command
//! surfaces ship (ICE simulation guard / signature verification /
//! connection failure / timeout / daemon factory / db query
//! parse). The simulation-blocked path (code 4) is reachable only
//! when the SDK returns `SimulationRequired` from `commit`, and
//! the CLI always simulates before committing, so triggering it
//! needs a deliberately crafted runtime fixture.

use assert_cmd::prelude::*;
use std::process::Command;
use std::process::Stdio;

#[test]
fn code_0_on_success() {
    Command::cargo_bin("net-mesh")
        .unwrap()
        .arg("version")
        .assert()
        .code(0);
}

#[test]
fn code_1_on_generic_error_missing_identity_file() {
    // Build a path under a tempdir that we never write to —
    // portable across Unix and Windows, and immune to a future
    // operator who happens to have a `this/path/...` folder.
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("definitely-does-not-exist.toml");
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["identity", "show"])
        .arg(&missing)
        .assert()
        .code(1);
}

#[test]
fn code_2_on_invalid_args_unknown_flag() {
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["snapshot", "get", "--this-flag-does-not-exist"])
        .assert()
        .code(2);
}

#[test]
fn code_2_on_invalid_args_unknown_subcommand() {
    Command::cargo_bin("net-mesh")
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
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["log", "tail", "--min-level", "no-such-level"])
        .assert()
        .code(2);
}

#[test]
fn code_8_on_ice_confirmation_refused_non_tty() {
    // Generate an operator identity first — admin/ICE require one
    // (see CliContext::build's require_identity branch). The bin
    // refuses to sign with an ephemeral keypair.
    let dir = tempfile::tempdir().unwrap();
    let identity = dir.path().join("op.toml");
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["identity", "generate", "--out"])
        .arg(&identity)
        .assert()
        .success();

    // `net ice freeze-cluster` without `--yes` and a piped (non-
    // TTY) stdin must exit 8 (ConfirmationRefused). assert_cmd
    // sets stdin to `Stdio::null()` by default which is already
    // non-TTY, but pinning the configuration explicitly here
    // keeps the test legible.
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args([
            "ice",
            "freeze-cluster",
            "--local",
            "--ttl",
            "5m",
            "--identity",
        ])
        .arg(&identity)
        .stdin(Stdio::null())
        .assert()
        .code(8);
}

#[test]
fn code_2_on_malformed_bind_literal_for_listen_and_attach() {
    // Review finding 7: one malformed bind literal must be ONE exit-code
    // class on every verb. `255.255.255.255:0` (broadcast) used to exit 2
    // (usage error) on `wrap --listen` but slip past the attach-side
    // bind validation and die later as exit 6 (connection failure). Both
    // verbs must now reject it as InvalidArgs during argument resolution,
    // before any connection.
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    std::fs::write(&config, "").unwrap();
    let psk = "42".repeat(32);
    let key = "01".repeat(32);

    let listen_out = Command::cargo_bin("net-mesh")
        .unwrap()
        .env_remove("NET_MESH_CONFIG")
        .env_remove("NET_MESH_PROFILE")
        .arg("--config")
        .arg(&config)
        .args([
            "wrap",
            "fixture",
            "--listen",
            "--psk-hex",
            &psk,
            "--bind",
            "255.255.255.255:0",
            "--inspect-target",
            "--",
            "child-must-not-run",
        ])
        .output()
        .unwrap();

    let attach_out = Command::cargo_bin("net-mesh")
        .unwrap()
        .env_remove("NET_MESH_CONFIG")
        .env_remove("NET_MESH_PROFILE")
        .arg("--config")
        .arg(&config)
        .args([
            "wrap",
            "fixture",
            "--node-addr",
            "127.0.0.1:9",
            "--node-pubkey",
            &key,
            "--node-id",
            "1",
            "--psk-hex",
            &psk,
            "--bind",
            "255.255.255.255:0",
            "--inspect-target",
            "--",
            "child-must-not-run",
        ])
        .output()
        .unwrap();

    assert_eq!(
        listen_out.status.code(),
        Some(2),
        "wrap --listen must reject a malformed bind literal as a usage error"
    );
    assert_eq!(
        attach_out.status.code(),
        Some(2),
        "the attach verb must reject the same literal as the same usage-error class"
    );
    assert_eq!(
        listen_out.status.code(),
        attach_out.status.code(),
        "one malformed bind literal must yield one exit-code class on both verbs"
    );
}
