//! Real-NAT scenario suite (`NAT_TRAVERSAL_V2_PLAN.md` Stage 4) —
//! Linux-only wrappers around `tests/natsim/run_scenario.sh`, which
//! provisions network namespaces with genuine nftables masquerade
//! between the endpoints and runs `examples/natsim_node.rs` helpers
//! inside them.
//!
//! Every test is `#[ignore]`d: the suite needs root (netns + nft)
//! and only runs in the dedicated `natsim` CI job (or manually on a
//! Linux box):
//!
//! ```text
//! cargo build --example natsim_node --features net,nat-traversal
//! cargo test --test natsim --features net,nat-traversal -- --ignored --test-threads=1
//! ```
//!
//! `--test-threads=1` is required — scenarios share the namespace
//! names and the 10.99.0.0/24 test range.
//!
//! Each scenario asserts on both the connection outcome and the
//! `traversal_stats` deltas (the plan's exit criterion), read from
//! the initiator's `outcome.json` verdict.

#![cfg(all(target_os = "linux", feature = "net", feature = "nat-traversal"))]

use std::process::Command;

/// Locate the `natsim_node` example for the ACTIVE build profile.
/// This test binary lives at `<target>/<profile>/deps/<name>-<hash>`,
/// so the example sits two directories up at
/// `<target>/<profile>/examples/natsim_node` — correct under
/// `--release`, custom `--profile` names, and `CARGO_TARGET_DIR`
/// alike. (Cubic round 6: a hardcoded `target/debug/` path made the
/// no-root guard tests silently skip in release builds; note that
/// `PROFILE` is a build-script-only env var and is NOT available at
/// test runtime, hence the current_exe derivation.) The
/// `NATSIM_NODE_BIN` override mirrors `run_scenario.sh`.
fn natsim_node_bin() -> std::path::PathBuf {
    if let Ok(explicit) = std::env::var("NATSIM_NODE_BIN") {
        return explicit.into();
    }
    let mut p = std::env::current_exe().expect("current_exe");
    p.pop(); // strip the test-binary filename → deps/
    p.pop(); // strip deps/ → <profile>/
    p.push("examples");
    p.push("natsim_node");
    p
}

/// Run one scenario script and return the initiator's outcome JSON.
/// The script prints the outcome path on a `NATSIM_OUTCOME_PATH=`
/// marker line (a bare "last line" is unsafe: the JSON body it
/// follows ends without a trailing newline, so the path would glue
/// onto the closing brace).
fn scenario(name: &str) -> serde_json::Value {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/natsim/run_scenario.sh");
    // Thread the profile-resolved helper path through sudo (which
    // strips the environment by default) so the script exercises the
    // SAME profile's binary as this test run instead of its
    // debug-path default.
    let bin = natsim_node_bin();
    let out = Command::new("sudo")
        .arg("env")
        .arg(format!("NATSIM_NODE_BIN={}", bin.display()))
        .arg(script)
        .arg(name)
        .output()
        .expect("spawn run_scenario.sh (is sudo available?)");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "scenario {name} failed (status {:?})\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}",
        out.status,
    );
    let path = stdout
        .lines()
        .rev()
        .find_map(|l| l.strip_prefix("NATSIM_OUTCOME_PATH="))
        .map(str::trim)
        .expect("NATSIM_OUTCOME_PATH= marker on scenario stdout");
    let bytes = std::fs::read(path).expect("read outcome json");
    serde_json::from_slice(&bytes).expect("parse outcome json")
}

fn stat(v: &serde_json::Value, key: &str) -> u64 {
    v["stats"][key].as_u64().unwrap_or_else(|| {
        panic!("stats.{key} missing from outcome: {v:#}");
    })
}

/// Cone × Cone across two real masqueraded namespaces: the punch
/// lands and the session sits on B's *public NAT mapping* — the
/// first validation of the feature against actual NAT behavior
/// (plan exit criterion).
#[test]
#[ignore = "requires root + Linux netns; run via the natsim CI job"]
fn natsim_cone_cone_punch_succeeds() {
    let v = scenario("cone_cone_punch");
    assert_eq!(v["ok"], true, "punch connect must resolve: {v:#}");
    assert_eq!(stat(&v, "punches_attempted"), 1, "{v:#}");
    assert_eq!(stat(&v, "punches_succeeded"), 1, "{v:#}");
    assert_eq!(stat(&v, "relay_fallbacks"), 0, "{v:#}");
    let addr = v["session_addr"].as_str().unwrap_or("");
    assert!(
        addr.starts_with("10.99.0.3:"),
        "session must land on B's public NAT mapping (10.99.0.3:*), got {addr}",
    );
    assert_eq!(v["self_nat_class"], "Cone", "{v:#}");
    assert_eq!(v["peer_nat_class"], "Cone", "{v:#}");
}

/// Symmetric × Cone (parent decision 8, against a real fully-random
/// masquerade): exactly one punch attempt, the per-destination
/// mapping defeats it, and the session falls back to the relay.
#[test]
#[ignore = "requires root + Linux netns; run via the natsim CI job"]
fn natsim_symmetric_cone_attempts_exactly_once() {
    let v = scenario("symmetric_cone_punch");
    assert_eq!(v["ok"], true, "fallback connect must resolve: {v:#}");
    assert_eq!(v["self_nat_class"], "Symmetric", "{v:#}");
    assert_eq!(stat(&v, "punches_attempted"), 1, "exactly once: {v:#}");
    assert_eq!(stat(&v, "punches_succeeded"), 0, "{v:#}");
    assert_eq!(stat(&v, "relay_fallbacks"), 1, "{v:#}");
    assert_eq!(stat(&v, "punch_timeouts"), 1, "{v:#}");
    let addr = v["session_addr"].as_str().unwrap_or("");
    assert!(
        addr.starts_with("10.99.0.10:") || addr.starts_with("10.99.0.11:"),
        "fallback session must ride a coordinator, got {addr}",
    );
}

/// Symmetric × Symmetric: the matrix skips the punch entirely and
/// rides the relay (zero attempts — the pair can never punch).
#[test]
#[ignore = "requires root + Linux netns; run via the natsim CI job"]
fn natsim_symmetric_symmetric_skips() {
    let v = scenario("symmetric_symmetric_skip");
    assert_eq!(v["ok"], true, "relay connect must resolve: {v:#}");
    assert_eq!(v["self_nat_class"], "Symmetric", "{v:#}");
    assert_eq!(v["peer_nat_class"], "Symmetric", "{v:#}");
    assert_eq!(stat(&v, "punches_attempted"), 0, "matrix skip: {v:#}");
    assert_eq!(stat(&v, "relay_fallbacks"), 1, "{v:#}");
}

/// Cone × Cone with the direct UDP path administratively dropped:
/// the punch times out and falls back within the deadline budget —
/// the "dropped keep-alives" row of the plan's matrix.
#[test]
#[ignore = "requires root + Linux netns; run via the natsim CI job"]
fn natsim_dropped_keepalives_fall_back_within_deadline() {
    let v = scenario("dropped_keepalives");
    assert_eq!(v["ok"], true, "fallback connect must resolve: {v:#}");
    assert_eq!(stat(&v, "punches_attempted"), 1, "{v:#}");
    assert_eq!(stat(&v, "punches_succeeded"), 0, "{v:#}");
    assert_eq!(stat(&v, "relay_fallbacks"), 1, "{v:#}");
    assert_eq!(stat(&v, "punch_timeouts"), 1, "{v:#}");
    let elapsed = v["elapsed_ms"].as_u64().unwrap_or(u64::MAX);
    assert!(
        elapsed < 15_000,
        "punch-failed fallback must resolve within deadline + budget, took {elapsed}ms",
    );
}

/// Stage 3's background upgrade across a real masquerade: a
/// relay-routed session from a NAT'd (lower-node-id) joiner to a
/// public peer migrates off the relay onto the direct path.
#[test]
#[ignore = "requires root + Linux netns; run via the natsim CI job"]
fn natsim_relay_session_upgrades_to_direct() {
    let v = scenario("relay_upgrade");
    assert_eq!(v["ok"], true, "relay connect must resolve: {v:#}");
    assert_eq!(v["started_on_relay"], true, "{v:#}");
    assert_eq!(
        v["upgraded"], true,
        "the background upgrade must land: {v:#}"
    );
    assert!(stat(&v, "upgrades_attempted") >= 1, "{v:#}");
    assert!(stat(&v, "upgrades_succeeded") >= 1, "{v:#}");
    let addr = v["session_addr"].as_str().unwrap_or("");
    assert!(
        addr.starts_with("10.99.0.12:"),
        "upgraded session must sit on B's public addr, got {addr}",
    );
}

// =========================================================================
// Configuration-validation guards (no root, no netns — run anywhere
// the suite compiles). These pin the harness's fail-loudly behavior
// from the cubic round-3 review: misconfiguration must exit 2 with a
// clear message BEFORE any namespace is touched, never silently
// build the wrong topology (or crash mid-provision under `set -e`).
// =========================================================================

/// Run setup.sh with the given args as a plain user. Validation
/// happens before any privileged command, so these paths need no
/// sudo.
fn setup_sh(args: &[&str]) -> std::process::Output {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/natsim/setup.sh");
    Command::new("bash")
        .arg(script)
        .args(args)
        .output()
        .expect("spawn setup.sh")
}

#[test]
fn setup_rejects_unknown_nat_mode_before_touching_namespaces() {
    // The typo case the review called out: "symmetricc" used to fall
    // through to cone masquerade silently.
    let out = setup_sh(&["--nat-b", "symmetricc"]);
    assert_eq!(out.status.code(), Some(2), "invalid mode must exit 2");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("invalid NAT mode"),
        "must name the problem; got: {stderr}",
    );
}

#[test]
fn setup_rejects_public_b_with_a_natted_b_side() {
    // --public-b puts B on the bridge; a NAT'd B namespace alongside
    // it would be a contradictory topology.
    let out = setup_sh(&["--public-b"]);
    assert_eq!(out.status.code(), Some(2), "conflicting config must exit 2");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--public-b requires --nat-b none"),
        "must name the conflict; got: {stderr}",
    );
}

/// The helper binary refuses a joiner with no publics (upgrade mode
/// used to panic on `public_infos[0]`). Needs the built example;
/// skips when it isn't present (the natsim CI job always builds it
/// first).
#[test]
fn helper_rejects_joiner_without_publics() {
    // Profile-aware resolution — under `--release` the example lives
    // in target/release/examples, and a hardcoded debug path made
    // this guard silently skip there (cubic round 6).
    let bin = natsim_node_bin();
    if !bin.exists() {
        eprintln!(
            "natsim_node example not built at {}; skipping",
            bin.display()
        );
        return;
    }
    let out = Command::new(&bin)
        .args([
            "joiner",
            "--name",
            "a",
            "--bind",
            "127.0.0.1:0",
            "--state",
            "/tmp",
            "--target",
            "b",
            "--mode",
            "upgrade",
        ])
        .output()
        .expect("spawn natsim_node");
    assert_eq!(
        out.status.code(),
        Some(2),
        "joiner without --publics must exit 2, not panic",
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("requires --publics"),
        "must name the missing flag; got: {stderr}",
    );
}
