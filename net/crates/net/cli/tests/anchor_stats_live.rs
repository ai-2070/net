//! Stage 6 slice 5 — `net-mesh anchor stats` against a LIVE anchor.
//!
//! An ICE attempt ledger is not fold state and is never announced,
//! so — unlike every other operator listing — there is no local
//! projection of it the CLI could read. The node that owns the ledger
//! is the only node that can answer for it, which is why this drives
//! the real binary against a real anchor over the real nRPC service.
//!
//! What these hold that the unit tests cannot: that the DENOMINATOR
//! and the caveat travel with the numbers all the way to the
//! operator's screen, and that the degenerate zero-attempt case
//! arrives as an absence rather than as `0 %`.
//!
//! Run: `cargo test -p net-cli --features rtc-bootstrap --test anchor_stats_live`
#![cfg(feature = "rtc-bootstrap")]

use std::sync::Arc;
use std::time::Duration;

use assert_cmd::Command as AssertCommand;
use net::adapter::net::rtc::{
    handle_signal, start_dialog, DialogTable, RtcConfig, RtcRejectReason, RtcSignalMsg,
};
use net::adapter::net::MeshNode;

const PSK: [u8; 32] = [0x42u8; 32];
const PSK_HEX: &str = "4242424242424242424242424242424242424242424242424242424242424242";

/// The peer the driven dialogs name. Never contacted: this test is
/// about the ledger the anchor publishes, and each attempt's terminal
/// transition is applied directly.
const PEER: u64 = 0xBEEF_0002;

const DEADLINE: Duration = Duration::from_secs(3600);

fn stats_command(daemon: &Arc<MeshNode>) -> std::process::Output {
    AssertCommand::cargo_bin("net-mesh")
        .expect("binary")
        .args([
            "--output",
            "json",
            "anchor",
            "stats",
            "--node-addr",
            &daemon.local_addr().to_string(),
            "--node-pubkey",
            &hex::encode(daemon.public_key()),
            "--node-id",
            &format!("{:#x}", daemon.node_id()),
            "--psk-hex",
            PSK_HEX,
        ])
        .timeout(Duration::from_secs(60))
        .output()
        .expect("run anchor stats")
}

fn parse(out: &std::process::Output) -> serde_json::Value {
    assert!(
        out.status.success(),
        "anchor stats failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout was not JSON ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

/// The ratio reaches the operator with its denominator, its residual
/// and its caveat — and before any attempt exists it reaches them as
/// an ABSENCE, not as zero per cent.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn anchor_stats_reports_the_ledger_with_its_denominator_and_its_residual() {
    let mesh = net_sdk::Mesh::builder("127.0.0.1:0", &PSK)
        .expect("builder")
        .rtc(RtcConfig {
            ice_deadline: DEADLINE,
            ..RtcConfig::new().with_bind_addr("127.0.0.1:0".parse().expect("addr"))
        })
        .build()
        .await
        .expect("anchor mesh");
    let daemon = Arc::clone(mesh.node());
    daemon.start();
    let _ice_stats =
        net_sdk::rtc_bootstrap::serve_anchor_ice_stats(&mesh).expect("serve the ICE stats service");

    // ── The degenerate case, first, because it is the one a surface
    //    is most likely to get wrong.
    let row = parse(&stats_command(&daemon));
    assert_eq!(row["rtc_configured"], true);
    assert_eq!(row["ice_attempted"], 0);
    assert_eq!(
        row["ice_direct_ratio"],
        serde_json::Value::Null,
        "0/0 has no ratio, and the row must say so with a null rather than a 0.0 \
         that reads as total failure: {row}"
    );
    let display = row["ice_direct_display"]
        .as_str()
        .expect("a display string");
    assert!(
        display.contains("no attempts") && display.contains("not 0%"),
        "the human line must name the absence AND rule out the misreading: {display}"
    );

    // ── Now a real ledger: two attempts, one refused, one left in
    //    flight. Driven through the production engine functions
    //    against the anchor's own driver, so these are the same
    //    counters an operator would be reading in the field.
    let driver = daemon.rtc_driver().expect("rtc driver");
    let mut table = DialogTable::new();
    start_dialog(driver, &mut table, PEER, 1, DEADLINE)
        .await
        .expect("offer 1");
    start_dialog(driver, &mut table, PEER, 2, DEADLINE)
        .await
        .expect("offer 2");
    handle_signal(
        driver,
        &mut table,
        PEER,
        RtcSignalMsg::Reject {
            dialog: 2,
            reason: RtcRejectReason::Busy,
        },
        DEADLINE,
    )
    .await;

    let row = parse(&stats_command(&daemon));
    let n = |key: &str| {
        row[key]
            .as_u64()
            .unwrap_or_else(|| panic!("{key} in {row}"))
    };
    assert_eq!(
        (
            n("ice_attempted"),
            n("ice_direct"),
            n("ice_relayed"),
            n("ice_failed"),
            n("ice_pending")
        ),
        (2, 0, 0, 1, 1),
        "two attempts — one dialog each — one refused and one still running: {row}"
    );
    assert_eq!(
        n("ice_direct") + n("ice_relayed") + n("ice_failed") + n("ice_pending"),
        n("ice_attempted"),
        "the identity closes only with the residual included, which is exactly why \
         the residual is on the row: {row}"
    );
    assert_eq!(
        row["ice_direct_ratio"].as_f64(),
        Some(0.0),
        "one attempt has terminated without going direct, so the ratio IS zero — a \
         different statement from having no ratio: {row}"
    );
    let display = row["ice_direct_display"]
        .as_str()
        .expect("a display string");
    assert!(
        display.starts_with("0/2 attempts (0%)"),
        "the ratio must be printed over its denominator, never as a bare \
         percentage: {display}"
    );
    assert!(
        display.contains("still in flight"),
        "and the in-flight attempt must be visible, because that is exactly when \
         the outcome terms do not sum to the denominator: {display}"
    );

    // ── The denominator's definition and the caveat travel WITH the
    //    numbers. This is the slice's whole point: a ratio quoted
    //    without its denominator is the thing that gets misread.
    let denominator = row["denominator"].as_str().expect("a denominator string");
    assert!(
        denominator.contains("ATTEMPTS") && denominator.contains("signalling dialog"),
        "the row must state what an attempt IS: {denominator}"
    );
    assert!(
        denominator.contains("Not sessions"),
        "…and what it is not: {denominator}"
    );
    let caveat = row["caveat"].as_str().expect("a caveat string");
    assert!(
        caveat.contains("NOT a success rate for sessions")
            && caveat.contains("relayed session is not a failed one"),
        "the row must rule out the reading that a non-direct attempt is a broken \
         session: {caveat}"
    );
    assert!(
        caveat.contains("never gated"),
        "and that nothing decides anything on this number: {caveat}"
    );
}

/// A node with no RTC driver keeps **no ledger**, and the CLI says
/// that instead of publishing a row of zeros that would read as
/// "nothing has gone direct here".
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn anchor_stats_on_a_node_without_rtc_reports_no_ledger_rather_than_zeros() {
    let mesh = net_sdk::Mesh::builder("127.0.0.1:0", &PSK)
        .expect("builder")
        .build()
        .await
        .expect("plain mesh");
    let daemon = Arc::clone(mesh.node());
    daemon.start();
    let _ice_stats =
        net_sdk::rtc_bootstrap::serve_anchor_ice_stats(&mesh).expect("serve the ICE stats service");

    let row = parse(&stats_command(&daemon));
    assert_eq!(
        row["rtc_configured"], false,
        "the absence of a ledger is a fact the row carries: {row}"
    );
    assert_eq!(row["ice_direct_ratio"], serde_json::Value::Null);
    let display = row["ice_direct_display"]
        .as_str()
        .expect("a display string");
    assert!(
        display.contains("no rtc driver"),
        "…and names it, rather than reporting an empty ledger: {display}"
    );
}

/// With nothing to attach to, the command REFUSES and says why. An
/// attempt ledger has no local projection, so an empty row here would
/// be a fabrication.
#[test]
fn anchor_stats_without_a_daemon_refuses_rather_than_inventing_a_ledger() {
    let out = AssertCommand::cargo_bin("net-mesh")
        .expect("binary")
        .args(["--output", "json", "anchor", "stats"])
        .timeout(Duration::from_secs(30))
        .output()
        .expect("run anchor stats");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr).to_lowercase();
    assert!(
        stderr.contains("never announced") || stderr.contains("node-addr"),
        "the refusal must say why there is nothing local to read: {stderr}"
    );
}
