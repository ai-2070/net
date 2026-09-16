//! The browser NAT conformance matrix's **platform-independent half**
//! (`BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md` Stage 6).
//!
//! The rows themselves need Linux network namespaces, root, nftables
//! and two headless browsers; they live in `tests/natsim.rs` behind
//! that gate and run only in the `natsim` CI job. Everything that does
//! NOT need a network lives here and runs **everywhere**: the row
//! table, the dispositions and their derivation, the counter-identity
//! checker, the verdict parser, the gateway-flow witness, and the
//! consistency of all of it with `run_scenario.sh`'s own case arms.
//!
//! This split is deliberate, and it is the reason the matrix cannot
//! quietly stop existing. A Linux-gated test file compiles to an empty
//! binary on every other platform: a row table inside it is not
//! type-checked, its arithmetic is never executed, and it can drift
//! from the script that runs it for as long as nobody reads both. The
//! only thing this file cannot prove is what actually happens on the
//! wire — and that is the one thing a netns is for.

#[path = "natsim/rows.rs"]
mod rows;

use rows::{Disposition, IceCounters, Nat, NatFlows, Row, RowVerdict, CONTROL, ROWS};

// =========================================================================
// The table
// =========================================================================

#[test]
fn the_matrix_is_the_six_derived_rows() {
    let got: Vec<(Nat, Nat, Disposition)> =
        ROWS.iter().map(|r| (r.nat_a, r.nat_b, r.expect)).collect();
    assert_eq!(
        got,
        vec![
            (Nat::ConeAr, Nat::ConeAr, Disposition::Direct),
            (Nat::ConeAr, Nat::ConePr, Disposition::Direct),
            (Nat::ConePr, Nat::ConePr, Disposition::Direct),
            (Nat::ConeAr, Nat::Symmetric, Disposition::Direct),
            (Nat::ConePr, Nat::Symmetric, Disposition::Relayed),
            (Nat::Symmetric, Nat::Symmetric, Disposition::Relayed),
        ],
        "the six rows and their derived dispositions: four direct, and TWO relayed — \
         port-restricted x symmetric cannot solve either (see rows.rs for the derivation)"
    );
    let mut names: Vec<&str> = ROWS.iter().map(|r| r.scenario).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), ROWS.len(), "row scenario names must be unique");
}

#[test]
fn the_control_row_is_row_one_on_the_other_engine() {
    let row1 = ROWS[0];
    assert_eq!(
        (CONTROL.nat_a, CONTROL.nat_b, CONTROL.expect),
        (row1.nat_a, row1.nat_b, row1.expect),
        "the Firefox control must be the SAME NAT pair as row 1, so the only variable is the \
         engine"
    );
    assert_ne!(CONTROL.scenario, row1.scenario);
}

/// The seam this whole file exists for: `run_scenario.sh` carries its
/// own copy of the matrix (it has to — it provisions the topology),
/// and nothing but this test stops the two from drifting.
#[test]
fn run_scenario_matrix_matches_the_rust_table() {
    let script = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/natsim/run_scenario.sh"
    ))
    .expect("read run_scenario.sh");
    let arms = rows::parse_script_arms(&script).expect("parse browser case arms");
    let expected = rows::all_scenarios();
    assert_eq!(
        arms.len(),
        expected.len(),
        "run_scenario.sh defines {} browser scenarios, the Rust table has {}: {arms:#?}",
        arms.len(),
        expected.len()
    );
    for (arm, row) in arms.iter().zip(expected.iter()) {
        assert_eq!(arm.scenario, row.scenario, "scenario order/name");
        assert_eq!(arm.nat_a, row.nat_a.mode(), "{}: NAT_A", row.scenario);
        assert_eq!(arm.nat_b, row.nat_b.mode(), "{}: NAT_B", row.scenario);
        assert_eq!(
            arm.expect,
            row.expect.as_str(),
            "{}: EXPECT — the script and the table must agree on the disposition, or the row \
             provisions one topology and asserts another",
            row.scenario
        );
    }
    // The matrix is Chromium's; exactly one scenario is the Firefox
    // control. A second Firefox row would silently double the job's
    // runtime for a second reading of the same netfilter behaviour.
    let firefox: Vec<&str> = arms
        .iter()
        .filter(|a| a.engine_a == "firefox" || a.engine_b == "firefox")
        .map(|a| a.scenario.as_str())
        .collect();
    assert_eq!(
        firefox,
        vec![CONTROL.scenario],
        "exactly one scenario runs Firefox, and it is the control"
    );
    let control = arms.last().expect("control arm");
    assert_eq!(
        (control.engine_a.as_str(), control.engine_b.as_str()),
        ("firefox", "firefox"),
        "the control runs Firefox on BOTH sides — a mixed pair would not say which engine's ICE \
         stack was responsible for a failure"
    );
}

// =========================================================================
// The counter identity
// =========================================================================

/// A leaf that took its anchor bootstrap dialog AND its peer dialog
/// direct.
fn both_direct() -> IceCounters {
    IceCounters {
        attempted: 2,
        direct: 2,
        ..IceCounters::default()
    }
}

/// A leaf whose anchor dialog went direct and whose peer dialog stayed
/// on the anchor.
fn anchor_direct_peer_relayed() -> IceCounters {
    IceCounters {
        attempted: 2,
        direct: 1,
        relayed: 1,
        ..IceCounters::default()
    }
}

#[test]
fn the_two_good_shapes_pass() {
    let direct_row = &ROWS[0];
    both_direct()
        .check_exact("a", direct_row.leaf_expectation())
        .expect("anchor dialog direct, peer dialog direct");
    let relayed_row = &ROWS[5];
    anchor_direct_peer_relayed()
        .check_exact("a", relayed_row.leaf_expectation())
        .expect("anchor dialog direct, peer dialog relayed — a relayed row is a PASS");
}

/// The arithmetic everybody gets wrong once: a leaf's dialog with its
/// own anchor is an attempt, so a relayed row reads `direct == 1`, not
/// `direct == 0`.
#[test]
fn the_anchor_bootstrap_dialog_is_counted_in_every_expectation() {
    for row in ROWS {
        let want = row.leaf_expectation();
        assert_eq!(
            want.attempted, 2,
            "{}: one anchor bootstrap dialog plus one peer dialog",
            row.scenario
        );
        assert_eq!(
            row.anchor_expectation(),
            IceCounters {
                attempted: 2,
                direct: 2,
                ..IceCounters::default()
            },
            "{}: the anchor takes one bootstrap dialog per leaf, both direct, on EVERY row — \
             that is what establishes the anchors were reachable, which is what scopes the \
             plan's 100%-established criterion",
            row.scenario
        );
        match row.expect {
            Disposition::Direct => assert_eq!((want.direct, want.relayed), (2, 1 - 1)),
            Disposition::Relayed => assert_eq!(
                (want.direct, want.relayed),
                (1, 1),
                "{}: the anchor dialog is still direct on a relayed row",
                row.scenario
            ),
        }
        assert_eq!(
            want.udp_blocked, 0,
            "{}: no row blocks UDP egress, so udp_blocked is zero by construction",
            row.scenario
        );
    }
}

/// The failure mode the whole slice turns on: four leaf counters with
/// no call site read zero, and the identity holds as `0 == 0`.
#[test]
fn an_all_zero_side_fails_rather_than_passing_vacuously() {
    let err = IceCounters::default()
        .check_exact("a", ROWS[0].leaf_expectation())
        .expect_err("all-zero must FAIL: the identity is trivially true there");
    assert!(
        err.contains("ice_attempted == 0") && err.contains("proves nothing"),
        "the message must name the vacuity, not just the mismatch: {err}"
    );
}

#[test]
fn an_outcome_total_above_attempted_is_a_broken_partition() {
    let broken = IceCounters {
        attempted: 2,
        direct: 2,
        relayed: 1,
        ..IceCounters::default()
    };
    let err = broken
        .check_exact("a", ROWS[0].leaf_expectation())
        .expect_err("three outcomes for two attempts is not a valid partition");
    assert!(
        err.contains("partition is broken"),
        "a partition whose terms exceed the denominator must say so: {err}"
    );
}

#[test]
fn an_attempt_with_no_outcome_yet_fails_in_the_same_assertion() {
    let in_flight = IceCounters {
        attempted: 2,
        direct: 1,
        ..IceCounters::default()
    };
    let err = in_flight
        .check_exact("a", ROWS[0].leaf_expectation())
        .expect_err("the peer dialog is still in flight");
    assert!(
        err.contains("ice_pending != 0"),
        "an in-flight attempt must surface as pending, not as a quiescence race: {err}"
    );
}

#[test]
fn a_row_that_landed_the_other_way_fails_both_directions() {
    let err = anchor_direct_peer_relayed()
        .check_exact("a", ROWS[0].leaf_expectation())
        .expect_err("a direct row whose peer dialog relayed");
    assert!(err.contains("expected ice_direct == 2, got 1"), "{err}");
    let err = both_direct()
        .check_exact("a", ROWS[5].leaf_expectation())
        .expect_err("a relayed row whose peer dialog went direct");
    assert!(err.contains("expected ice_direct == 1, got 2"), "{err}");
}

#[test]
fn a_retry_shows_as_a_third_attempt() {
    let retried = IceCounters {
        attempted: 3,
        direct: 2,
        relayed: 1,
        ..IceCounters::default()
    };
    let err = retried
        .check_exact("a", ROWS[0].leaf_expectation())
        .expect_err("a row drives two dialogs per side; a third attempt is a defect");
    assert!(
        err.contains("expected 2 attempt(s)") && err.contains("one DIALOG"),
        "an attempt is one dialog, and the message must say so: {err}"
    );
}

#[test]
fn a_missing_term_is_an_error_and_never_a_zero() {
    let v = serde_json::json!({
        "ice_attempted": "2",
        "ice_direct": "2",
        "ice_failed": "0",
        "udp_blocked": "0",
    });
    let err = IceCounters::from_json(&v).expect_err("ice_relayed is absent");
    assert!(
        err.contains("ice_relayed") && err.contains("not a zero"),
        "an absent term and a term at zero are opposite diagnoses: {err}"
    );
}

#[test]
fn counter_terms_survive_values_a_js_number_would_round() {
    // The leaf emits decimal strings for exactly this reason.
    let big = (1u64 << 53) + 1;
    let v = serde_json::json!({
        "ice_attempted": big.to_string(),
        "ice_direct": big.to_string(),
        "ice_relayed": "0",
        "ice_failed": "0",
        "udp_blocked": "0",
    });
    let c = IceCounters::from_json(&v).expect("decimal strings above 2^53");
    assert_eq!(c.attempted, big, "a JS number would have rounded this");
    c.check_exact(
        "a",
        IceCounters {
            attempted: big,
            direct: big,
            ..IceCounters::default()
        },
    )
    .expect("the identity holds at full u64 width");
}

// =========================================================================
// The verdict
// =========================================================================

fn counters_json(c: IceCounters) -> serde_json::Value {
    serde_json::json!({
        "ice_attempted": c.attempted.to_string(),
        "ice_direct": c.direct.to_string(),
        "ice_relayed": c.relayed.to_string(),
        "ice_failed": c.failed.to_string(),
        "udp_blocked": c.udp_blocked.to_string(),
    })
}

fn verdict_json(row: &Row, page_type: &str, side: IceCounters) -> serde_json::Value {
    serde_json::json!({
        "scenario": row.scenario,
        "nat_a": row.nat_a.mode(),
        "nat_b": row.nat_b.mode(),
        "engine_a": "chromium",
        "engine_b": "chromium",
        "page_type": page_type,
        "peer_page_type": page_type,
        "page_detail": "",
        "a": { "node_id": "00000000000000aa", "counters": counters_json(side) },
        "b": { "node_id": "00000000000000bb", "counters": counters_json(side) },
        "anchor": { "counters": counters_json(row.anchor_expectation()) },
        "errors": [],
    })
}

#[test]
fn a_well_formed_direct_verdict_passes_its_row() {
    let row = &ROWS[0];
    let v = verdict_json(row, "direct", both_direct());
    RowVerdict::from_json(&v)
        .expect("parse")
        .check(row)
        .expect("a direct row with direct counters on both sides and a healthy anchor");
}

#[test]
fn a_relayed_verdict_is_typed_ice_timeout_at_the_page_surface() {
    let row = &ROWS[5];
    let v = verdict_json(row, "iceTimeout", anchor_direct_peer_relayed());
    RowVerdict::from_json(&v)
        .expect("parse")
        .check(row)
        .expect("symmetric x symmetric: iceTimeout is the expected, TYPED outcome");
    // The stats term and the outcome type are deliberately different
    // words; accepting the stats word at the page surface would mean
    // accepting a shape the SDK never emits.
    for wrong in ["relayed", "routed"] {
        let v = verdict_json(row, wrong, anchor_direct_peer_relayed());
        let err = RowVerdict::from_json(&v)
            .expect("parse")
            .check(row)
            .expect_err("`relayed`/`routed` are not PeerConnectOutcome types");
        assert!(err.contains("reached no disposition"), "{err}");
    }
}

/// `udpBlocked` is a real outcome type and a wrong answer HERE: every
/// row's anchor dialog is a UDP DataChannel that landed direct, so UDP
/// egress demonstrably works and the narrower cause is not supported.
#[test]
fn udp_blocked_is_refused_as_over_claiming_on_a_nat_row() {
    let row = &ROWS[4];
    let v = verdict_json(row, "udpBlocked", anchor_direct_peer_relayed());
    let err = RowVerdict::from_json(&v)
        .expect("parse")
        .check(row)
        .expect_err("udpBlocked claims more than this row's evidence supports");
    assert!(
        err.contains("narrower cause than the evidence supports"),
        "the message must say WHY it is refused, since udpBlocked is otherwise a valid \
         disposition: {err}"
    );
}

/// One pair has one disposition. Two different ones means one side
/// installed a session the other does not have — which a per-side
/// counter check alone would not catch, because each side's ledger is
/// internally consistent.
#[test]
fn the_offerer_and_the_answerer_must_agree() {
    let row = &ROWS[0];
    let mut v = verdict_json(row, "direct", both_direct());
    v["peer_page_type"] = serde_json::json!("iceTimeout");
    let err = RowVerdict::from_json(&v)
        .expect("parse")
        .check(row)
        .expect_err("offerer direct, answerer timed out");
    assert!(err.contains("One pair has one disposition"), "{err}");
}

#[test]
fn a_typed_failure_reaches_no_disposition() {
    let row = &ROWS[0];
    let mut v = verdict_json(row, "handshakeFailed", both_direct());
    v["page_detail"] = serde_json::json!("noise msg2 never arrived");
    let err = RowVerdict::from_json(&v)
        .expect("parse")
        .check(row)
        .expect_err("a typed failure is not a disposition");
    assert!(
        err.contains("handshakeFailed") && err.contains("noise msg2 never arrived"),
        "the typed failure and its detail must both reach the failure message: {err}"
    );
}

#[test]
fn a_runner_error_fails_the_row_before_any_counter_is_read() {
    let row = &ROWS[0];
    let mut v = verdict_json(row, "direct", both_direct());
    v["errors"] = serde_json::json!(["chromium in nsim_b never reported a node id"]);
    let err = RowVerdict::from_json(&v)
        .expect("parse")
        .check(row)
        .expect_err("a verdict written around an error is not a measurement");
    assert!(err.contains("never reported a node id"), "{err}");
}

/// The shape the runner ACTUALLY writes when it dies before a single
/// step ran: no ledgers, no outcome types, one reason. Taken verbatim
/// from an executed refusal (`--expect direct` against a row the
/// table says is relayed).
///
/// Strict parsing of the ledgers here used to replace that reason
/// with "a.counters: counter ice_attempted is absent", which is the
/// least useful sentence available for a row that failed because a
/// page server never came up.
#[test]
fn an_errored_verdict_reports_the_runners_reason_not_a_parse_complaint() {
    let row = &ROWS[5];
    let v = serde_json::json!({
        "scenario": row.scenario,
        "nat_a": row.nat_a.mode(),
        "nat_b": row.nat_b.mode(),
        "engine_a": "chromium",
        "engine_b": "chromium",
        "page_type": "",
        "peer_page_type": "",
        "page_detail": "",
        "a": { "counters": serde_json::Value::Null, "node_id": "" },
        "b": { "counters": serde_json::Value::Null, "node_id": "" },
        "anchor": { "counters": serde_json::Value::Null },
        "errors": ["the page server in nsim_b never wrote /tmp/x/page_b.ready"],
    });
    let err = RowVerdict::from_json(&v)
        .expect("an errored verdict must still parse, or its reason is lost")
        .check(row)
        .expect_err("a verdict written around an error is not a measurement");
    assert!(
        err.contains("never wrote") && !err.contains("ice_attempted is absent"),
        "the runner's reason must survive: {err}"
    );
}

/// A row that provisioned a different topology than it asserts is the
/// one defect a perfect verdict cannot otherwise reveal.
#[test]
fn a_miswired_scenario_fails_even_with_perfect_counters() {
    let row = &ROWS[3]; // cone-ar x symmetric
    let mut v = verdict_json(row, "direct", both_direct());
    v["nat_b"] = serde_json::json!("cone-pr");
    let err = RowVerdict::from_json(&v)
        .expect("parse")
        .check(row)
        .expect_err("the topology the runner saw is not the row's topology");
    assert!(err.contains("mis-wired"), "{err}");
}

/// A relayed row whose ANCHOR dialog also failed is not a correct
/// fallback — it is a row where nothing was reachable, and the plan's
/// established-session criterion is scoped to reachable anchors.
#[test]
fn a_relayed_row_with_a_broken_anchor_dialog_fails() {
    let row = &ROWS[5];
    let mut v = verdict_json(row, "iceTimeout", anchor_direct_peer_relayed());
    v["anchor"]["counters"] = counters_json(IceCounters {
        attempted: 2,
        direct: 1,
        failed: 1,
        ..IceCounters::default()
    });
    let err = RowVerdict::from_json(&v)
        .expect("parse")
        .check(row)
        .expect_err("one leaf never reached the anchor");
    assert!(
        err.contains("anchor (native)") && err.contains("ice_direct == 2"),
        "{err}"
    );
}

// =========================================================================
// The gateway witness
// =========================================================================

#[test]
fn the_gateways_discriminate_direct_from_relayed() {
    let flows = |a_replied: u64, b_replied: u64| {
        NatFlows::from_json(&serde_json::json!({
            "a": { "udp_flows": 3, "udp_replied": a_replied },
            "b": { "udp_flows": 3, "udp_replied": b_replied },
        }))
        .expect("parse nat_flow.json")
    };
    let direct = &ROWS[0];
    let relayed = &ROWS[5];

    flows(1, 1)
        .check(direct)
        .expect("two-way flow on both gateways");
    flows(0, 0)
        .check(relayed)
        .expect("checks left, nothing came back");

    // An outbound-only flow exists on EVERY row — ICE always sends
    // checks — so "a flow to the peer exists" must not be mistaken
    // for a direct path.
    let err = flows(0, 0)
        .check(direct)
        .expect_err("a direct row whose gateways saw no reply");
    assert!(err.contains("not a direct session"), "{err}");
    let err = flows(1, 1)
        .check(relayed)
        .expect_err("a relayed row whose gateways saw a two-way flow");
    assert!(err.contains("crossed directly"), "{err}");
    // One-sided is still not direct: the punch has two halves.
    flows(1, 0)
        .check(direct)
        .expect_err("only one gateway saw a reply");
}
