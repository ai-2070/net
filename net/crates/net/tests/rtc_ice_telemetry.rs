//! Stage 6 slice 5 — the ICE attempt ledger behind plan §10's
//! `ice_direct / ice_attempted` field telemetry.
//!
//! These witnesses exist because a ratio whose denominator is
//! ambiguous is a metric people misread for a year, and because a
//! four-term identity whose terms do not actually cover the space is
//! how an identity ends up asserted and false. So they assert, on the
//! real production functions and the real counters:
//!
//! 1. the denominator — one attempt per signalling DIALOG;
//! 2. the partition — every counted attempt lands in exactly ONE
//!    outcome, and a late duplicate frame for a terminated attempt
//!    adds nothing;
//! 3. the residual — an attempt in flight is counted in the
//!    denominator and in no outcome, so the sum is short by exactly
//!    the number of live attempts, and that number is readable;
//! 4. the degenerate case — zero attempts has NO ratio, which is not
//!    the same number as zero.
//!
//! No sleeps, no waits, no network: every terminal transition is
//! driven directly through the engine, so nothing here can be flaky
//! and nothing here can pass by accident of timing.
//!
//! Run: `cargo test --features "webrtc fixtures" --test rtc_ice_telemetry`
#![cfg(feature = "webrtc")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use net::adapter::net::rtc::{
    expire_dialogs, handle_signal, start_dialog, DialogTable, RtcConfig, RtcRejectReason,
    RtcSignalMsg, SignalOutcome,
};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};
use net::adapter::Adapter;

const PSK: [u8; 32] = [0x5Cu8; 32];

/// The peer this node's dialogs name. Never contacted: these
/// witnesses are about the ledger, and every terminal transition is
/// applied directly.
const PEER: u64 = 0xBEEF_0001;

/// Long enough that no dialog expires on its own — expiry here is
/// always driven by handing `expire_dialogs` a future instant, so
/// the witness never races the clock.
const ICE_DEADLINE: Duration = Duration::from_secs(3600);

async fn node() -> Arc<MeshNode> {
    let mut cfg = MeshNodeConfig::new("127.0.0.1:0".parse().expect("addr"), PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5));
    cfg.socket_buffers = SocketBufferConfig::for_testing();
    cfg.rtc = Some(RtcConfig {
        ice_deadline: ICE_DEADLINE,
        ..RtcConfig::new().with_bind_addr("127.0.0.1:0".parse().expect("addr"))
    });
    Arc::new(
        MeshNode::new(EntityKeypair::generate(), cfg)
            .await
            .expect("MeshNode::new"),
    )
}

/// `(attempted, direct, relayed, failed, pending)`.
fn ledger(node: &Arc<MeshNode>) -> (u64, u64, u64, u64, u64) {
    let s = node.rtc_stats().ice_snapshot();
    (s.attempted, s.direct, s.relayed, s.failed, s.pending())
}

/// WITNESS 1 — **the denominator is dialogs.** Two offers to the
/// same peer are two attempts, and the second one does not merge
/// into the first because it is a different dialog. This is the fact
/// the whole ratio is read against: "attempts", not "peers" and not
/// "sessions".
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_attempt_is_one_dialog_not_one_peer() {
    let a = node().await;
    let driver = a.rtc_driver().expect("rtc driver");
    let mut table = DialogTable::new();

    assert_eq!(
        ledger(&a),
        (0, 0, 0, 0, 0),
        "a fresh node has attempted nothing"
    );

    start_dialog(driver, &mut table, PEER, 1, ICE_DEADLINE)
        .await
        .expect("first offer");
    assert_eq!(ledger(&a).0, 1);
    start_dialog(driver, &mut table, PEER, 2, ICE_DEADLINE)
        .await
        .expect("second offer");
    assert_eq!(
        ledger(&a),
        (2, 0, 0, 0, 2),
        "two dialogs to the SAME peer are two attempts — the denominator \
         counts attempts, and both of them are still in flight"
    );
    assert_eq!(table.open_for(PEER), 2);

    a.shutdown().await.expect("shutdown");
}

/// WITNESS 2 — **the partition, and no double counting.** The peer's
/// `Reject` is terminal and lands in `ice_failed`; a second `Reject`
/// for the same dialog is a late frame for an attempt that already
/// terminated and must add nothing, or the attempt would be charged
/// twice and the identity would be false in the other direction.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_peers_reject_is_counted_failed_once_and_a_late_duplicate_adds_nothing() {
    let a = node().await;
    let driver = a.rtc_driver().expect("rtc driver");
    let mut table = DialogTable::new();

    start_dialog(driver, &mut table, PEER, 7, ICE_DEADLINE)
        .await
        .expect("offer");
    let reject = |dialog| RtcSignalMsg::Reject {
        dialog,
        reason: RtcRejectReason::Busy,
    };
    let outcome = handle_signal(driver, &mut table, PEER, reject(7), ICE_DEADLINE).await;
    assert!(
        matches!(outcome, SignalOutcome::Ended(RtcRejectReason::Busy)),
        "the refusal must actually end the dialog: {outcome:?}"
    );
    assert_eq!(
        ledger(&a),
        (1, 0, 0, 1, 0),
        "a refused attempt is `ice_failed` — NOT `ice_relayed`: it did not run \
         out of time, the peer declined — and the residual is zero because the \
         attempt is over"
    );

    let late = handle_signal(driver, &mut table, PEER, reject(7), ICE_DEADLINE).await;
    assert!(
        matches!(late, SignalOutcome::Ignored),
        "a Reject for a dialog we no longer hold is a late frame: {late:?}"
    );
    assert_eq!(
        ledger(&a),
        (1, 0, 0, 1, 0),
        "and it charges nothing: one attempt, one outcome"
    );

    a.shutdown().await.expect("shutdown");
}

/// WITNESS 3 — **the two non-`direct` dispositions are different
/// operational facts, and both are in the partition.** One attempt
/// reaches its deadline with ICE never connected (`ice_relayed` — the
/// pair stayed on the anchor, which is not a failure); the other is
/// refused (`ice_failed`). The four terms then sum to the denominator
/// exactly, with nothing pending.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_four_outcomes_sum_to_the_denominator_once_nothing_is_in_flight() {
    let a = node().await;
    let driver = a.rtc_driver().expect("rtc driver");
    let mut table = DialogTable::new();

    start_dialog(driver, &mut table, PEER, 11, ICE_DEADLINE)
        .await
        .expect("offer 11");
    start_dialog(driver, &mut table, PEER, 12, ICE_DEADLINE)
        .await
        .expect("offer 12");

    handle_signal(
        driver,
        &mut table,
        PEER,
        RtcSignalMsg::Reject {
            dialog: 12,
            reason: RtcRejectReason::Declined,
        },
        ICE_DEADLINE,
    )
    .await;

    // Dialog 11 reaches its deadline. Driven by handing the sweep an
    // instant past it rather than by waiting: the clock is not what
    // this witness is about.
    let expired = expire_dialogs(driver, &mut table, Instant::now() + ICE_DEADLINE * 2).await;
    assert_eq!(
        expired,
        vec![(PEER, 11)],
        "exactly the attempt whose deadline passed"
    );

    let s = a.rtc_stats().ice_snapshot();
    assert_eq!(
        (s.attempted, s.direct, s.relayed, s.failed),
        (2, 0, 1, 1),
        "the timed-out attempt is `relayed` and the refused one is `failed`: \
         collapsing them would report 'ICE could not connect' about an attempt \
         that was declined"
    );
    assert_eq!(
        s.direct + s.relayed + s.failed + s.pending(),
        s.attempted,
        "plan §10's identity, with `udp_blocked` structurally absent on a native \
         surface and `pending` carrying the residual"
    );
    assert_eq!(s.pending(), 0, "nothing is in flight");
    assert_eq!(
        a.rtc_stats().ice_pending(),
        0,
        "and the accessor the conformance matrix reads agrees"
    );

    a.shutdown().await.expect("shutdown");
}

/// WITNESS 4 — **the residual is real and readable.** This is the
/// qualifier that gets dropped: an attempt in flight has been counted
/// in the denominator and in no outcome, so a naive four-term sum is
/// short by exactly the number of live attempts. Assert the identity
/// together with `pending == 0`, never after a hopeful wait.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_in_flight_attempt_is_the_exact_shortfall_in_the_identity() {
    let a = node().await;
    let driver = a.rtc_driver().expect("rtc driver");
    let mut table = DialogTable::new();

    start_dialog(driver, &mut table, PEER, 21, ICE_DEADLINE)
        .await
        .expect("offer 21");
    start_dialog(driver, &mut table, PEER, 22, ICE_DEADLINE)
        .await
        .expect("offer 22");
    handle_signal(
        driver,
        &mut table,
        PEER,
        RtcSignalMsg::Reject {
            dialog: 22,
            reason: RtcRejectReason::Busy,
        },
        ICE_DEADLINE,
    )
    .await;

    let s = a.rtc_stats().ice_snapshot();
    assert_eq!(
        s.direct + s.relayed + s.failed,
        s.attempted - 1,
        "with one attempt still running the four outcomes CANNOT sum to the \
         denominator — this is the off-by-one that reads as a lost count"
    );
    assert_eq!(
        s.pending(),
        1,
        "and the shortfall is named rather than left to be discovered"
    );
    assert_eq!(
        s.direct + s.relayed + s.failed + s.pending(),
        s.attempted,
        "the identity closes exactly when the residual is included"
    );

    // Retire it, and the strict form holds.
    expire_dialogs(driver, &mut table, Instant::now() + ICE_DEADLINE * 2).await;
    let s = a.rtc_stats().ice_snapshot();
    assert_eq!((s.pending(), s.relayed), (0, 1));
    assert_eq!(s.direct + s.relayed + s.failed, s.attempted);

    a.shutdown().await.expect("shutdown");
}

/// WITNESS 5 — **the degenerate case is honest.** A node that has
/// never attempted a direct path has no direct-path ratio. `0/0` is
/// not `0 %`, and reporting `0 %` would read as total failure where
/// nothing has happened. The ratio is `None`, and it becomes a number
/// only once there is a denominator.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn zero_attempts_has_no_ratio_and_one_refused_attempt_has_a_zero_one() {
    let a = node().await;
    let driver = a.rtc_driver().expect("rtc driver");
    let mut table = DialogTable::new();

    assert_eq!(
        a.rtc_stats().ice_snapshot().direct_ratio(),
        None,
        "no attempts is not zero per cent"
    );

    start_dialog(driver, &mut table, PEER, 31, ICE_DEADLINE)
        .await
        .expect("offer");
    handle_signal(
        driver,
        &mut table,
        PEER,
        RtcSignalMsg::Reject {
            dialog: 31,
            reason: RtcRejectReason::Busy,
        },
        ICE_DEADLINE,
    )
    .await;
    assert_eq!(
        a.rtc_stats().ice_snapshot().direct_ratio(),
        Some(0.0),
        "one attempt that did not go direct IS zero per cent — which is a \
         different statement from having attempted nothing"
    );

    a.shutdown().await.expect("shutdown");
}

/// WITNESS 6 — **no driver is no ledger.** A node configured without
/// RTC keeps no attempt ledger, and the surfaces say so instead of
/// publishing a ledger of zeros that would read as "nothing has gone
/// direct here".
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_node_without_rtc_has_no_ledger_rather_than_a_ledger_of_zeros() {
    let mut cfg = MeshNodeConfig::new("127.0.0.1:0".parse().expect("addr"), PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5));
    cfg.socket_buffers = SocketBufferConfig::for_testing();
    cfg.rtc = None;
    let a = Arc::new(
        MeshNode::new(EntityKeypair::generate(), cfg)
            .await
            .expect("MeshNode::new"),
    );
    assert!(a.rtc_driver().is_none());
    assert!(
        a.rtc_ice_stats().is_none(),
        "the ledger is absent, which the Deck column and `anchor stats` report \
         as an absence — not as a ratio of zero"
    );
    a.shutdown().await.expect("shutdown");
}
