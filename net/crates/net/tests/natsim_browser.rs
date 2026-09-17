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

use rows::{
    all_scenarios, AppExchange, Disposition, Enumeration, Forwarding, IceCounters, Media, Nat,
    NatFlows, Row, RowVerdict, CONTROL, NO_MEDIA, NO_MEDIA_RELAYED, ROWS,
};

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

/// The permission-free leg is row 1 with ONE variable: the grant.
///
/// Same NAT pair, same engine, same expected disposition. If any of
/// those drifted, a failure here would be about the topology or the
/// engine and the one question the leg exists to answer — does the
/// product work in a browsing context that was never asked for a
/// media permission — would be unanswerable from its result.
#[test]
fn the_permission_free_leg_is_row_one_with_only_the_grant_removed() {
    let row1 = ROWS[0];
    assert_eq!(
        (NO_MEDIA.nat_a, NO_MEDIA.nat_b, NO_MEDIA.expect),
        (row1.nat_a, row1.nat_b, row1.expect),
        "the permission-free leg must be the SAME row 1 topology and expectation"
    );
    assert_eq!(row1.media, Media::Granted, "row 1 grants camera+microphone");
    assert_eq!(
        NO_MEDIA.media,
        Media::None,
        "and the leg grants nothing — that is the whole variable"
    );
    assert_ne!(NO_MEDIA.scenario, row1.scenario);
    // The Firefox control has always been permission-free too, and
    // for a reason that is NOT a choice: Firefox has no media gate on
    // interface enumeration and Playwright cannot grant it camera or
    // microphone. Recorded so the ungranted Chromium leg is not
    // mistaken for the only ungranted row, and so the control's own
    // label keeps describing the environment it ran in.
    assert_eq!(CONTROL.media, Media::None);
}

/// The permission-free ROUTED leg is the relayed row with ONE
/// variable: the grant.
///
/// It exists because the direct leg structurally cannot answer the
/// routed question — a direct row asserts the anchor's per-pair
/// forwarding counter is FLAT, so it is the one shape that cannot
/// witness forwarding. If this row drifted off `Relayed`, or off the
/// symmetric pair ICE genuinely cannot solve, it would stop driving
/// the routed path and the fallback would go unmeasured while
/// appearing to be covered.
#[test]
fn the_permission_free_routed_leg_is_the_relayed_row_with_only_the_grant_removed() {
    let relayed = ROWS[5];
    assert_eq!(
        (
            NO_MEDIA_RELAYED.nat_a,
            NO_MEDIA_RELAYED.nat_b,
            NO_MEDIA_RELAYED.expect
        ),
        (relayed.nat_a, relayed.nat_b, relayed.expect),
        "the permission-free routed leg must be the SAME symmetric pair and expectation"
    );
    assert_eq!(relayed.media, Media::Granted);
    assert_eq!(NO_MEDIA_RELAYED.media, Media::None);
    assert_ne!(NO_MEDIA_RELAYED.scenario, relayed.scenario);
    assert_eq!(
        NO_MEDIA_RELAYED.pair_forwarding(),
        Forwarding::Carried,
        "and it must require the anchor to have CARRIED the application bytes — a routed leg \
         that expected flat forwarding would witness nothing"
    );
    assert_eq!(
        NO_MEDIA.pair_forwarding(),
        Forwarding::Flat,
        "while the direct leg requires them NOT to have been carried, which is exactly why it \
         cannot answer the routed question and this row has to exist"
    );
}

/// Interface enumeration is REQUIRED of a granted Chromium row and
/// merely RECORDED on a permission-free one — and the runner reads
/// exactly this predicate.
///
/// The asymmetry is the finding, so it is pinned rather than left to
/// a comment. Requiring `real > 0` of a granted row is what named
/// §6.12's cause in one line instead of a sixty-second ICE timeout.
/// Requiring it of a permission-free row would refuse the very
/// measurement that row exists to take: §11.8 observed both tabs at
/// `real=0`, on wildcard ports, reaching the STUN endpoint, solving
/// `direct` and delivering nonce-correlated payloads both ways. A
/// flip in either direction is a silent change of subject.
#[test]
fn enumeration_is_required_where_the_grant_was_made_and_recorded_where_it_was_not() {
    for row in all_scenarios() {
        let required = row.enumeration() == Enumeration::Required;
        assert_eq!(
            required,
            row.media == Media::Granted,
            "row {}: media {} must map to {}",
            row.scenario,
            row.media,
            if row.media == Media::Granted {
                "Enumeration::Required"
            } else {
                "Enumeration::Observed"
            }
        );
        // The predicate itself, as the runner calls it: a tab that
        // allocated nothing on a named network.
        assert_eq!(
            row.enumeration().satisfied_by(0),
            !required,
            "row {}: real=0 must be {} here",
            row.scenario,
            if required { "a refusal" } else { "acceptable" }
        );
        // …and a tab that did enumerate is acceptable on EVERY row.
        // The permission-free legs record the observation instead of
        // asserting its inverse, so a Chromium that stopped gating
        // does not become a product regression.
        assert!(
            row.enumeration().satisfied_by(1),
            "row {}: a tab that enumerated must never be refused",
            row.scenario
        );
    }
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
        assert_eq!(
            arm.media,
            row.media.flag(),
            "{}: MEDIA — the arm decides what the drivers grant, so an arm that disagrees with \
             the table runs one permission environment and asserts another, and no counter in \
             the verdict could reveal it",
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
    let control = arms
        .iter()
        .find(|a| a.scenario == CONTROL.scenario)
        .expect("the control arm, found by NAME — it is no longer the last arm");
    assert_eq!(
        (control.engine_a.as_str(), control.engine_b.as_str()),
        ("firefox", "firefox"),
        "the control runs Firefox on BOTH sides — a mixed pair would not say which engine's ICE \
         stack was responsible for a failure"
    );
    // The Chromium legs that withhold the grant, by NAME and in
    // order. TWO, and they are not two readings of one question:
    // `NO_MEDIA` asks whether an ungranted pair goes DIRECT, and a
    // direct row's anchor forwarding counter is flat BY ASSERTION, so
    // it structurally cannot witness the routed path.
    // `NO_MEDIA_RELAYED` asks the other half — whether Net's
    // anchor-routed fallback, which is not TURN and rides each leaf's
    // authenticated anchor session, carries application bytes for a
    // page that was never asked for a media permission. Pinned as an
    // exact set so a third cannot arrive unexamined and neither can
    // disappear.
    let ungranted: Vec<&str> = arms
        .iter()
        .filter(|a| a.media == "none" && a.engine_a == "chromium")
        .map(|a| a.scenario.as_str())
        .collect();
    assert_eq!(
        ungranted,
        vec![NO_MEDIA.scenario, NO_MEDIA_RELAYED.scenario],
        "exactly two Chromium scenarios run permission-free — the direct leg and the routed \
         leg — because a direct row cannot witness forwarding and a routed row is the only \
         thing that measures the fallback"
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

/// A well-formed application-exchange witness for `row`: both nonces
/// delivered, and the anchor's per-pair counter doing whatever that
/// row's disposition requires of it.
///
/// The pre-values are deliberately NON-zero. By the time the exchange
/// runs the anchor has already carried this pair's routed Noise
/// handshake, so a fixture that started at zero would be testing a
/// state the runner never reports — and would hide a checker that
/// compared absolutes instead of the delta.
fn app_json(row: &Row) -> serde_json::Value {
    let (post_ab, post_ba) = match row.pair_forwarding() {
        Forwarding::Flat => (7, 7),
        Forwarding::Carried => (11, 12),
    };
    serde_json::json!({
        "nonce_a": "a1a1a1a1a1a1a1a1",
        "nonce_b": "b2b2b2b2b2b2b2b2",
        "seen_at_a": "b2b2b2b2b2b2b2b2",
        "seen_at_b": "a1a1a1a1a1a1a1a1",
        "sent_a_to_b": "4",
        "sent_b_to_a": "4",
        "received_at_a": "4",
        "received_at_b": "4",
        "forwarded_pre_ab": "7",
        "forwarded_pre_ba": "7",
        "forwarded_post_ab": post_ab.to_string(),
        "forwarded_post_ba": post_ba.to_string(),
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
        "media": row.media.flag(),
        "app": app_json(row),
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
// The application-delivery witness
// =========================================================================
//
// The row's fourth witness, and the one whose SCHEMA is the new place
// a measurement failure could collapse into observed absence. A
// verdict that simply omitted `app` would present to a direct row as
// "the anchor forwarded nothing", which is exactly the property that
// row is trying to prove. Every row below exists because the checker
// has to refuse that, not average it.

#[test]
fn a_verdict_with_no_application_witness_is_refused() {
    let row = &ROWS[0];
    let mut v = verdict_json(row, "direct", both_direct());
    v.as_object_mut().expect("object").remove("app");
    let err = RowVerdict::from_json(&v)
        .expect_err("a measuring verdict with no application witness is not a measurement");
    assert!(
        err.contains("app missing") && err.contains("not optional"),
        "the refusal must name the missing witness rather than defaulting it: {err}"
    );
}

/// `null` is the shape a runner that tried and failed would write —
/// and it is refused for the same reason, unless the runner ALSO said
/// what went wrong.
#[test]
fn a_null_application_witness_is_refused_unless_the_runner_said_why() {
    let row = &ROWS[0];
    let mut v = verdict_json(row, "direct", both_direct());
    v["app"] = serde_json::Value::Null;
    let err = RowVerdict::from_json(&v).expect_err(
        "null with no error is neither a measurement \
                                                    nor a refusal",
    );
    assert!(err.contains("neither measured nor refused"), "{err}");

    // With a stated reason it parses, and `check` reports the reason
    // rather than a complaint about the witness — the same discipline
    // the ledgers already follow.
    v["errors"] = serde_json::json!(["the page server in nsim_b never came up"]);
    let err = RowVerdict::from_json(&v)
        .expect("an errored verdict must still parse, or its reason is lost")
        .check(row)
        .expect_err("a verdict written around an error is not a measurement");
    assert!(
        err.contains("never came up") && !err.contains("app"),
        "the runner's reason must survive: {err}"
    );
}

#[test]
fn a_missing_application_term_is_an_error_and_never_a_zero() {
    let row = &ROWS[0];
    let mut v = verdict_json(row, "direct", both_direct());
    v["app"]
        .as_object_mut()
        .expect("object")
        .remove("forwarded_post_ab");
    let err = RowVerdict::from_json(&v).expect_err("a term of the witness is absent");
    assert!(
        err.contains("forwarded_post_ab") && err.contains("not a zero"),
        "an absent forwarding count must not read as an unmoved counter: {err}"
    );

    let mut v = verdict_json(row, "direct", both_direct());
    v["app"]
        .as_object_mut()
        .expect("object")
        .remove("seen_at_b");
    let err = RowVerdict::from_json(&v).expect_err("the decoded nonce is absent");
    assert!(
        err.contains("seen_at_b") && err.contains("not an empty one"),
        "an absent nonce must not read as a nonce that did not match: {err}"
    );
}

/// The counting witness a nonce exists to beat: frames arrived, and
/// they were not the ones the peer sent.
#[test]
fn frames_that_arrived_with_the_wrong_nonce_fail_the_row() {
    let row = &ROWS[0];
    let mut v = verdict_json(row, "direct", both_direct());
    v["app"]["seen_at_b"] = serde_json::json!("cccccccccccccccc");
    let err = RowVerdict::from_json(&v)
        .expect("parse")
        .check(row)
        .expect_err("B decoded a nonce A never sent");
    assert!(
        err.contains("No application payload of A's was observed at B"),
        "{err}"
    );

    // And the reverse direction fails on its own: a half-duplex path
    // would otherwise pass a matrix whose whole axis is which side's
    // filter admits which check.
    let mut v = verdict_json(row, "direct", both_direct());
    v["app"]["seen_at_a"] = serde_json::json!("");
    let err = RowVerdict::from_json(&v)
        .expect("parse")
        .check(row)
        .expect_err("A never decoded B's nonce");
    assert!(err.contains("reverse direction"), "{err}");
}

#[test]
fn one_nonce_for_both_directions_is_refused() {
    let row = &ROWS[0];
    let mut v = verdict_json(row, "direct", both_direct());
    for key in ["nonce_a", "nonce_b", "seen_at_a", "seen_at_b"] {
        v["app"][key] = serde_json::json!("a1a1a1a1a1a1a1a1");
    }
    let err = RowVerdict::from_json(&v)
        .expect("parse")
        .check(row)
        .expect_err("one nonce in both directions is satisfiable by a loopback");
    assert!(err.contains("looped back to its own sender"), "{err}");
}

/// **The direct row's new assertion.** Both nonces arrived — and the
/// anchor carried them. That is a relayed path wearing a direct
/// outcome type, and no other witness in the row notices: the ICE
/// ledgers, the typed outcomes and even the gateways' conntrack are
/// all satisfied by a session that installed.
#[test]
fn a_direct_row_whose_payload_the_anchor_forwarded_fails() {
    let row = &ROWS[0];
    let mut v = verdict_json(row, "direct", both_direct());
    v["app"]["forwarded_post_ab"] = serde_json::json!("13");
    v["app"]["forwarded_post_ba"] = serde_json::json!("13");
    let err = RowVerdict::from_json(&v)
        .expect("parse")
        .check(row)
        .expect_err("the anchor forwarded application bytes for a pair that claims direct");
    assert!(
        err.contains("Bytes the anchor carried are not a direct path"),
        "{err}"
    );
}

/// **The relayed row's new assertion.** A relayed row where the
/// payload arrived without the anchor forwarding it is not a relayed
/// row: the bytes took a path this row does not model.
#[test]
fn a_relayed_row_whose_payload_the_anchor_never_forwarded_fails() {
    let row = &ROWS[5];
    let mut v = verdict_json(row, "iceTimeout", anchor_direct_peer_relayed());
    v["app"]["forwarded_post_ab"] = serde_json::json!("7");
    let err = RowVerdict::from_json(&v)
        .expect("parse")
        .check(row)
        .expect_err("a relayed direction the anchor did not carry");
    assert!(err.contains("the anchor IS the path"), "{err}");
}

/// A pair counter that went DOWN is an unusable reading, not a flat
/// path. Saturating the subtraction would have read as flat and
/// passed the direct row.
#[test]
fn a_decreasing_pair_counter_is_an_unusable_reading() {
    let row = &ROWS[0];
    let mut v = verdict_json(row, "direct", both_direct());
    v["app"]["forwarded_post_ab"] = serde_json::json!("3");
    let err = RowVerdict::from_json(&v)
        .expect("parse")
        .check(row)
        .expect_err("7 → 3 is not a flat counter");
    assert!(err.contains("unusable reading"), "{err}");
}

/// Delivery and the counter disposition fail INDEPENDENTLY: a row
/// cannot pass one on the strength of the other.
#[test]
fn delivery_and_the_counter_disposition_are_two_assertions() {
    let row = &ROWS[0];
    let mut v = verdict_json(row, "direct", both_direct());
    v["app"]["sent_a_to_b"] = serde_json::json!("0");
    let err = RowVerdict::from_json(&v)
        .expect("parse")
        .check(row)
        .expect_err("a nonce reported with nothing ever sent");
    assert!(err.contains("cannot have crossed the transport"), "{err}");

    let mut v = verdict_json(row, "direct", both_direct());
    v["app"]["received_at_b"] = serde_json::json!("0");
    let err = RowVerdict::from_json(&v)
        .expect("parse")
        .check(row)
        .expect_err("a matching nonce with a receiver that counted nothing");
    assert!(err.contains("contradict each other"), "{err}");
}

/// The row's own expectation, asserted rather than assumed: four
/// direct rows want a flat pair counter, two relayed rows want a
/// moving one, and the permission-free leg is a direct row.
#[test]
fn every_row_names_the_forwarding_disposition_its_expectation_implies() {
    for row in rows::all_scenarios() {
        let want = match row.expect {
            Disposition::Direct => Forwarding::Flat,
            Disposition::Relayed => Forwarding::Carried,
        };
        assert_eq!(
            row.pair_forwarding(),
            want,
            "{}: a direct row's application bytes must not pass through the anchor, and a \
             relayed row's must",
            row.scenario
        );
    }
}

/// A row that quietly acquired the permission it claims not to need
/// is measuring the granted environment under the ungranted name —
/// and every other number in the verdict would agree with it.
#[test]
fn a_permission_free_leg_that_was_granted_media_fails() {
    let mut v = verdict_json(&NO_MEDIA, "direct", both_direct());
    v["media"] = serde_json::json!("granted");
    let err = RowVerdict::from_json(&v)
        .expect("parse")
        .check(&NO_MEDIA)
        .expect_err("the leg ran with a grant it is defined by not having");
    assert!(err.contains("permission it claims not to need"), "{err}");

    // And the converse: a granted row that ran ungranted is equally
    // mislabelled, because its green would then be evidence about a
    // different environment.
    let row = &ROWS[0];
    let mut v = verdict_json(row, "direct", both_direct());
    v["media"] = serde_json::json!("none");
    RowVerdict::from_json(&v)
        .expect("parse")
        .check(row)
        .expect_err("row 1 ran without the grant its table entry records");
}

#[test]
fn the_permission_free_legs_own_verdict_passes() {
    let v = verdict_json(&NO_MEDIA, "direct", both_direct());
    RowVerdict::from_json(&v)
        .expect("parse")
        .check(&NO_MEDIA)
        .expect("an ungranted Chromium leg that went direct and delivered both nonces");
}

/// Read directly, so the witness's own arithmetic is exercised
/// without a verdict around it.
#[test]
fn the_application_witness_reports_its_own_deltas() {
    let app = AppExchange::from_json(&app_json(&ROWS[5])).expect("parse");
    assert_eq!(
        app.forwarded_delta().expect("monotonic"),
        (4, 5),
        "the delta is what the row asserts on, never the absolute — the anchor has already \
         carried this pair's routed handshake by then"
    );
}

// =========================================================================
// The gateway witness
// =========================================================================

#[test]
fn the_gateways_discriminate_direct_from_relayed() {
    let flows = |a_replied: u64, b_replied: u64| {
        NatFlows::from_json(&serde_json::json!({
            "a": { "udp_flows": 3, "udp_replied": a_replied, "source": "conntrack" },
            "b": { "udp_flows": 3, "udp_replied": b_replied, "source": "procfs" },
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

/// **Measurement failure is not observed absence.**
///
/// A relayed row's confirmation IS a pair of zeros, so a gateway
/// whose conntrack table could not be read at all would confirm it
/// while observing nothing. That is not hypothetical: the GitHub
/// runner kernel is built without `CONFIG_NF_CONNTRACK_PROCFS`, and
/// before `conntrack` was installed in the workflow the script's
/// reader chain produced exactly these zeros.
#[test]
fn an_unreadable_gateway_fails_instead_of_confirming_a_relayed_row() {
    let unreadable = |a_source: &str, b_source: &str| {
        NatFlows::from_json(&serde_json::json!({
            "a": { "udp_flows": 0, "udp_replied": 0, "source": a_source },
            "b": { "udp_flows": 0, "udp_replied": 0, "source": b_source },
        }))
        .expect("parse nat_flow.json")
    };
    for row in [&ROWS[5], &ROWS[0]] {
        let err = unreadable("unreadable", "unreadable")
            .check(row)
            .expect_err("neither gateway's table was read");
        assert!(
            err.contains("was not read") && err.contains("not an absence of flow"),
            "row {}: {err}",
            row.scenario
        );
    }
    // One unreadable side is enough: the row's claim is about both
    // gateways, and half a measurement cannot establish it.
    let err = unreadable("conntrack", "unreadable")
        .check(&ROWS[5])
        .expect_err("gateway b's table was not read");
    assert!(err.contains("gateway b"), "{err}");
}

#[test]
fn a_flow_witness_that_cannot_say_where_its_numbers_came_from_is_refused() {
    let err = NatFlows::from_json(&serde_json::json!({
        "a": { "udp_flows": 0, "udp_replied": 0 },
        "b": { "udp_flows": 0, "udp_replied": 0, "source": "conntrack" },
    }))
    .expect_err("side a does not report its reader");
    assert!(
        err.contains("a.source") && err.contains("opposite facts"),
        "an absent source must not default to readable: {err}"
    );
}
