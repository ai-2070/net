//! **One rule, two probes** (`docs/internal/plans/ANCHOR_DUAL_STACK_PLAN.md`,
//! defect 3).
//!
//! The Rust (WASM) probe and the TypeScript probe used to disagree on
//! what "answered" means: TypeScript counted a STUN error response as
//! proof the endpoint was reachable, Rust counted only a
//! server-reflexive candidate. The same network could be typed
//! `udp-blocked` by one and `ice-timeout` by the other.
//!
//! Both now judge a probe's events by the same vector file,
//! `browser-ts/test/fixtures/stun-probe-verdicts.json`; this is the
//! Rust half, `browser-ts/test/classification.test.ts` the other. A
//! case either side answers differently fails that side.

use std::path::PathBuf;

use net_leaf::bootstrap::{probe_verdict, ProbeEvent, ProbeOutcome};

fn fixture() -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../browser-ts/test/fixtures/stun-probe-verdicts.json");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text).expect("the vector file is JSON")
}

fn event(value: &serde_json::Value) -> ProbeEvent {
    if let Some(line) = value.get("candidate").and_then(serde_json::Value::as_str) {
        return ProbeEvent::Candidate(line.to_string());
    }
    if value.get("ice").and_then(serde_json::Value::as_str) == Some("failed") {
        return ProbeEvent::ConnectionFailed;
    }
    let code = value
        .get("error")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or_else(|| panic!("an event is a candidate or an error: {value}"));
    ProbeEvent::CandidateError(u16::try_from(code).expect("a STUN code fits u16"))
}

#[test]
fn the_rust_probe_rule_agrees_with_every_shared_vector() {
    let fixture = fixture();
    let cases = fixture["cases"].as_array().expect("cases");
    assert!(cases.len() >= 10, "the vector file lost its cases");
    for case in cases {
        let name = case["name"].as_str().expect("name");
        let events: Vec<ProbeEvent> = case["events"]
            .as_array()
            .expect("events")
            .iter()
            .map(event)
            .collect();
        let want = match case["verdict"].as_str().expect("verdict") {
            "answered" => ProbeOutcome::Answered,
            "unanswered" => ProbeOutcome::Unanswered,
            "notRun" => ProbeOutcome::NotRun,
            other => panic!("{name}: unknown verdict {other:?}"),
        };
        assert_eq!(probe_verdict(&events), want, "{name}");
    }
}
