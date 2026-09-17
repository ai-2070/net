#![allow(dead_code)]
// The candidate production module is compiled unmodified. Its sole
// parent-scope constant is copied from rtc/admission.rs:43.
const MAX_PROVISIONAL_STREAM_BYTES: u64 = 64 * 1024;
#[path = "C:/Users/chief/orca/workspaces/net/kyra-stage5-round4-516a45c33/net/crates/net/src/adapter/net/rtc/fragment.rs"]
mod fragment;
use fragment::{FragmentPiece, FragmentProvenance, FragmentOutcome, RtcReassembly, GROUP_TTL};
use net_wire::protocol::FRAG_FRAGMENTED;
use bytes::Bytes;
use std::collections::HashSet;
use std::time::{Duration, Instant};
fn batch(sessions: u64) {
    let r = RtcReassembly::new();
    let now = Instant::now();
    let mut expected = HashSet::new();
    for session_id in 1..=sessions {
        for fragment_id in 1..=8u16 {
            let stream_id = u64::from(fragment_id);
            expected.insert((session_id, stream_id));
            let part = FragmentPiece { session_id, fragment_id, offset: 0, flags: FRAG_FRAGMENTED, sequence: 0,
                provenance: FragmentProvenance { stream_id, origin_hash: 1, channel_hash: 1, subprotocol_id: 0, reliable: true },
                data: Bytes::from_static(b"head"),
            };
            assert_eq!(r.accept(part, now), Err(FragmentOutcome::Buffered));
        }
    }
    r.expire(now + GROUP_TTL + Duration::from_millis(1));
    let actual: HashSet<_> = r.take_terminals().into_iter().map(|g| (g.session_id, g.provenance.stream_id)).collect();
    println!("sessions={sessions} owed={} actual={} missing={}", expected.len(), actual.len(), expected.difference(&actual).count());
    assert_eq!(actual, expected, "one normal expiry pass must retain every distinct terminal owner");
}
fn main() {
    let sessions = std::env::args().nth(1).unwrap().parse().unwrap();
    batch(sessions);
}
