//! `snapshot` must not present a runtime it just created as a cluster read.
//!
//! With no configuration and no surrounding node, `net-mesh snapshot get`
//! used to exit 0 in 40 ms with entirely plausible JSON:
//!
//! ```json
//! { "daemons": {}, "replicas": {}, "peers": {}, "avoid_list": {},
//!   "local_maintenance": "Active", "recently_emitted": [], ... }
//! ```
//!
//! Every field is empty because the supervisor had existed for
//! milliseconds — but an empty snapshot and a healthy idle cluster are the
//! same document. The only stderr line concerned an ephemeral identity,
//! which points at identity as the missing prerequisite and quietly confirms
//! that everything else worked.
//!
//! The Deck client is built from a runtime this invocation starts, so there
//! is no attach path to fix; the honest behavior is to refuse. These tests
//! pin that: refusing by default, exiting 2, saying why, and still working
//! when the caller opts in with `--local`.

use assert_cmd::prelude::*;
use std::process::Command;

const BIN: &str = "net-mesh";

/// Run a snapshot verb in an isolated config, so a developer's real profile
/// cannot change the outcome.
fn snapshot(args: &[&str], config: &std::path::Path) -> std::process::Output {
    Command::cargo_bin(BIN)
        .unwrap()
        .args(args)
        .arg("--config")
        .arg(config)
        .output()
        .unwrap()
}

/// An empty config file — the "no configuration at all" case from the report.
///
/// Owner-only, because `--config` runs the SEC-05 secret-file gate before
/// parsing and refuses a group- or world-readable config outright. That is
/// the correct behavior; it is just not what these tests are about, and
/// `std::fs::write` leaves the umask default (typically `0o644`).
///
/// Only one of the tests below reaches config loading at all — the rest
/// refuse at argument validation first — so on Unix this defect showed up
/// as a single failure. Windows has no mode bits and the gate only warns
/// there, which is why it passed locally and failed on CI.
fn empty_config() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    dir
}

#[test]
fn get_refuses_without_local_rather_than_printing_an_empty_snapshot() {
    let dir = empty_config();
    let out = snapshot(&["snapshot", "get"], &dir.path().join("config.toml"));

    assert_eq!(
        out.status.code(),
        Some(2),
        "expected exit 2 (invalid args); got {:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("\"peers\""),
        "a snapshot document was printed anyway — the false success is back:\n{stdout}"
    );
}

#[test]
fn status_refuses_without_local_too() {
    let dir = empty_config();
    let out = snapshot(&["snapshot", "status"], &dir.path().join("config.toml"));

    assert_eq!(
        out.status.code(),
        Some(2),
        "expected exit 2 (invalid args); got {:?}",
        out.status.code(),
    );
}

/// The refusal has to teach, or it just becomes a flag people paste blindly.
#[test]
fn the_refusal_explains_what_cannot_be_read_and_what_to_do() {
    let dir = empty_config();
    let out = snapshot(&["snapshot", "get"], &dir.path().join("config.toml"));
    let stderr = String::from_utf8_lossy(&out.stderr);

    for expected in [
        "cannot read a running deployment", // what is actually wrong
        "--local",                          // the opt-in
        "aggregator",                       // where to go instead
    ] {
        assert!(
            stderr.contains(expected),
            "the refusal never mentions {expected:?}:\n{stderr}"
        );
    }
}

#[test]
fn local_still_produces_a_snapshot_for_callers_who_ask_for_one() {
    let dir = empty_config();
    let out = snapshot(
        &["snapshot", "get", "--local", "--output", "json"],
        &dir.path().join("config.toml"),
    );

    assert!(
        out.status.success(),
        "`snapshot get --local` failed: {}",
        String::from_utf8_lossy(&out.stderr),
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let value: serde_json::Value =
        serde_json::from_str(&stdout).expect("--local should emit valid JSON");
    assert!(
        value.get("peers").is_some(),
        "expected a MeshOsSnapshot document, got: {stdout}"
    );
}

/// Opting in must not silence the explanation: a `--local` snapshot pasted
/// into a report should carry its own caveat.
#[test]
fn local_still_says_the_runtime_was_created_by_this_command() {
    let dir = empty_config();
    let out = snapshot(
        &["snapshot", "get", "--local", "-v"],
        &dir.path().join("config.toml"),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        stderr.contains("not a view of a running deployment"),
        "`--local` produced no caveat on stderr:\n{stderr}"
    );
}
