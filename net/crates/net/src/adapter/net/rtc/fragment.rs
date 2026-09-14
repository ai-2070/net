//! Reassembly of leaf fragments at the RTC ingress.
//!
//! # Why the native side has to do this
//!
//! A browser leaf cannot put more than [`MAX_PAYLOAD_SIZE`] bytes in
//! one Net packet — `NetHeader::validate` refuses it on arrival — so
//! it fragments, stamping `fragment_id`, `fragment_offset` and
//! `frag_flags` on each piece. Those three fields have always been
//! on the wire and have always been authenticated by
//! `NetHeader::aad()`, but nothing in the core ever *read*
//! `frag_flags`. A native node therefore saw a fragmented leaf
//! message as several independent events: a subscriber got partial
//! application payloads, and an nRPC or enrollment frame whose first
//! piece declares a body longer than it carries failed to decode and
//! timed out.
//!
//! That is not a bulk-transfer-only gap. The enrollment writer
//! accepts request bodies up to `MAX_ENROLL_BODY_BYTES`, which
//! exceeds one packet before nRPC framing is added, so the very
//! first thing a leaf says to an anchor can need this.
//!
//! # What is bounded, and by what
//!
//! Nothing here trusts the peer. A group is refused unless its
//! pieces are disjoint, all inside the declared total, and together
//! cover `[0, total)` exactly. Concurrency and memory are bounded
//! **per session**: at most [`MAX_GROUPS_PER_SESSION`] open groups,
//! and at most [`MAX_PROVISIONAL_STREAM_BYTES`] of held bytes — the
//! same whole-session byte budget `ProvisionalBudget` already
//! enforces on receive-stream allocation, reused rather than
//! re-invented so one number governs how much unenrolled receive
//! state a session can pin. A group that sits incomplete past
//! [`GROUP_TTL`] is reaped.
//!
//! The state is keyed by wire session id and dropped with the
//! session, so a reconnect never inherits a predecessor's partial
//! group — leaf fragment ids restart at 1 on every new session.

use std::time::{Duration, Instant};

use bytes::{Bytes, BytesMut};
use dashmap::DashMap;

use net_wire::protocol::{FRAG_FRAGMENTED, FRAG_LAST, MAX_PAYLOAD_SIZE};

use super::MAX_PROVISIONAL_STREAM_BYTES;

/// Concurrent incomplete groups one session may hold.
///
/// The leaf's own fragmentation ceiling is eight pieces per group;
/// eight groups is the same bound its receive side applies, so an
/// honest peer never meets this and a hostile one cannot open
/// unbounded state.
pub const MAX_GROUPS_PER_SESSION: usize = 8;

/// Pieces one group may hold. The leaf refuses to emit more, so a
/// group claiming more is malformed.
pub const MAX_PIECES_PER_GROUP: usize = 8;

/// Largest payload a group may reassemble to: eight full packets'
/// worth, which is the leaf's `MAX_FRAGMENTED_PAYLOAD`.
pub const MAX_REASSEMBLED_BYTES: usize = MAX_PAYLOAD_SIZE * MAX_PIECES_PER_GROUP;

/// How long an incomplete group may sit before it is reaped.
pub const GROUP_TTL: Duration = Duration::from_secs(2);

/// Why a piece did not become a payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FragmentOutcome {
    /// Buffered; the group is not complete yet.
    Buffered,
    /// The group contradicted itself and was discarded.
    Malformed,
    /// The session is already holding as much partial state as it
    /// may.
    Refused,
}

/// One partially reassembled group.
#[derive(Debug)]
struct Partial {
    session_id: u64,
    /// `(offset, bytes)`, at most [`MAX_PIECES_PER_GROUP`].
    pieces: Vec<(u16, Bytes)>,
    /// Known once the `FRAG_LAST` piece arrives.
    total: Option<usize>,
    held: usize,
    opened: Instant,
}

/// Per-session leaf-fragment reassembly for the RTC ingress.
#[derive(Debug, Default)]
pub struct RtcReassembly {
    groups: DashMap<(u64, u16), Partial>,
}

impl RtcReassembly {
    /// An empty reassembler.
    pub fn new() -> Self {
        Self::default()
    }

    /// How many groups are open. Observable so a test asserts the
    /// bound rather than inferring it.
    pub fn outstanding(&self) -> usize {
        self.groups.len()
    }

    /// Bytes currently held for `session_id`.
    pub fn held_bytes(&self, session_id: u64) -> u64 {
        self.groups
            .iter()
            .filter(|e| e.value().session_id == session_id)
            .map(|e| e.value().held as u64)
            .sum()
    }

    /// Offer one inbound piece.
    ///
    /// `Ok(Some(payload))` when it completed its group, `Ok(None)`
    /// when the packet was not a fragment at all (the caller keeps
    /// its event as-is), and `Err(outcome)` when the piece was
    /// buffered, refused or discarded.
    pub fn accept(
        &self,
        session_id: u64,
        fragment_id: u16,
        offset: u16,
        flags: u8,
        data: Bytes,
        now: Instant,
    ) -> Result<Option<Bytes>, FragmentOutcome> {
        if flags & FRAG_FRAGMENTED == 0 {
            return Ok(None);
        }
        let key = (session_id, fragment_id);
        let last = flags & FRAG_LAST != 0;
        let end = offset as usize + data.len();
        if end > MAX_REASSEMBLED_BYTES {
            return Err(FragmentOutcome::Malformed);
        }

        if !self.groups.contains_key(&key) {
            self.expire(now);
            let (groups, held) = self.session_usage(session_id);
            if groups >= MAX_GROUPS_PER_SESSION
                || held.saturating_add(data.len() as u64) > MAX_PROVISIONAL_STREAM_BYTES
            {
                return Err(FragmentOutcome::Refused);
            }
            self.groups.insert(
                key,
                Partial {
                    session_id,
                    pieces: Vec::with_capacity(MAX_PIECES_PER_GROUP),
                    total: None,
                    held: 0,
                    opened: now,
                },
            );
        } else if self
            .held_bytes(session_id)
            .saturating_add(data.len() as u64)
            > MAX_PROVISIONAL_STREAM_BYTES
        {
            self.groups.remove(&key);
            return Err(FragmentOutcome::Refused);
        }

        let complete = {
            let Some(mut entry) = self.groups.get_mut(&key) else {
                return Err(FragmentOutcome::Malformed);
            };
            let partial = entry.value_mut();
            let contradicts = partial.pieces.iter().any(|(o, d)| {
                let (s, e) = (*o as usize, *o as usize + d.len());
                // Any overlap, including an exact duplicate: a group
                // is written once.
                s < end && (offset as usize) < e
            }) || partial.pieces.len() >= MAX_PIECES_PER_GROUP
                || partial.total.is_some_and(|t| end > t || (last && end != t))
                // The declared end binds the pieces that arrived
                // before it, not only the ones that arrive after.
                || (last
                    && partial
                        .pieces
                        .iter()
                        .any(|(o, d)| *o as usize + d.len() > end));
            if contradicts {
                drop(entry);
                self.groups.remove(&key);
                return Err(FragmentOutcome::Malformed);
            }
            partial.held += data.len();
            partial.pieces.push((offset, data));
            if last {
                partial.total = Some(end);
            }
            partial.total.is_some_and(|t| partial.held == t)
        };
        if !complete {
            return Err(FragmentOutcome::Buffered);
        }

        let Some((_, mut partial)) = self.groups.remove(&key) else {
            return Err(FragmentOutcome::Malformed);
        };
        let total = partial.total.unwrap_or(0);
        partial.pieces.sort_unstable_by_key(|(o, _)| *o);
        // Prove the coverage rather than infer it from the byte
        // count: every piece must start exactly where the previous
        // one ended, from zero to the declared total.
        let mut covered = 0usize;
        for (offset, piece) in &partial.pieces {
            if *offset as usize != covered {
                return Err(FragmentOutcome::Malformed);
            }
            covered += piece.len();
        }
        if covered != total {
            return Err(FragmentOutcome::Malformed);
        }
        let mut out = BytesMut::with_capacity(total);
        for (_, piece) in &partial.pieces {
            out.extend_from_slice(piece);
        }
        Ok(Some(out.freeze()))
    }

    fn session_usage(&self, session_id: u64) -> (usize, u64) {
        let mut groups = 0usize;
        let mut held = 0u64;
        for entry in self.groups.iter() {
            if entry.value().session_id == session_id {
                groups += 1;
                held += entry.value().held as u64;
            }
        }
        (groups, held)
    }

    /// Reap groups older than [`GROUP_TTL`].
    pub fn expire(&self, now: Instant) {
        self.groups
            .retain(|_, p| now.saturating_duration_since(p.opened) < GROUP_TTL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: u64 = 0xABCD;

    fn piece(n: usize) -> Bytes {
        Bytes::from(vec![0x5A; n])
    }

    #[test]
    fn a_two_piece_group_reassembles_byte_exactly() {
        let r = RtcReassembly::new();
        let now = Instant::now();
        let head = Bytes::from_static(b"native ");
        let tail = Bytes::from_static(b"reassembly");
        assert_eq!(
            r.accept(SESSION, 1, 0, FRAG_FRAGMENTED, head.clone(), now),
            Err(FragmentOutcome::Buffered)
        );
        let whole = r
            .accept(
                SESSION,
                1,
                head.len() as u16,
                FRAG_FRAGMENTED | FRAG_LAST,
                tail,
                now,
            )
            .expect("completes")
            .expect("payload");
        assert_eq!(whole.as_ref(), b"native reassembly");
        assert_eq!(r.outstanding(), 0, "a completed group frees its slot");
    }

    #[test]
    fn an_unfragmented_packet_is_not_touched() {
        let r = RtcReassembly::new();
        assert_eq!(
            r.accept(SESSION, 0, 0, 0, piece(8), Instant::now()),
            Ok(None)
        );
        assert_eq!(r.outstanding(), 0);
    }

    /// The coverage rule, which a held-byte count alone does not
    /// prove: a hole at the start plus bytes past the declared end
    /// can sum to exactly the total.
    #[test]
    fn a_group_with_a_missing_prefix_and_data_past_its_end_is_refused() {
        let r = RtcReassembly::new();
        let now = Instant::now();
        assert_eq!(
            r.accept(
                SESSION,
                1,
                10,
                FRAG_FRAGMENTED,
                Bytes::from_static(b"later"),
                now
            ),
            Err(FragmentOutcome::Buffered)
        );
        assert_eq!(
            r.accept(
                SESSION,
                1,
                5,
                FRAG_FRAGMENTED | FRAG_LAST,
                Bytes::from_static(b"early"),
                now
            ),
            Err(FragmentOutcome::Malformed)
        );
        assert_eq!(r.outstanding(), 0, "a contradicted group is discarded");
    }

    #[test]
    fn overlapping_pieces_are_refused() {
        let r = RtcReassembly::new();
        let now = Instant::now();
        let _ = r.accept(SESSION, 2, 0, FRAG_FRAGMENTED, piece(16), now);
        assert_eq!(
            r.accept(SESSION, 2, 8, FRAG_FRAGMENTED, piece(16), now),
            Err(FragmentOutcome::Malformed)
        );
    }

    #[test]
    fn a_session_cannot_hold_more_than_the_byte_budget() {
        let r = RtcReassembly::new();
        let now = Instant::now();
        // Each group holds one near-full packet and never completes.
        let chunk = MAX_PAYLOAD_SIZE - 1;
        let mut opened = 0u64;
        for id in 1..=MAX_GROUPS_PER_SESSION as u16 {
            match r.accept(SESSION, id, 0, FRAG_FRAGMENTED, piece(chunk), now) {
                Err(FragmentOutcome::Buffered) => opened += 1,
                Err(FragmentOutcome::Refused) => break,
                other => panic!("unexpected {other:?}"),
            }
        }
        assert!(
            r.held_bytes(SESSION) <= MAX_PROVISIONAL_STREAM_BYTES,
            "held {} bytes past the session budget",
            r.held_bytes(SESSION)
        );
        assert!(opened > 0, "the budget must admit real traffic");
        assert_eq!(
            r.accept(SESSION, 99, 0, FRAG_FRAGMENTED, piece(chunk), now),
            Err(FragmentOutcome::Refused),
            "past the budget every new group is refused"
        );
    }

    /// A reconnect cannot inherit a predecessor's partial group:
    /// the key carries the wire session id, which a new handshake
    /// never reproduces, and the leftover reaps on its TTL.
    #[test]
    fn two_sessions_with_the_same_fragment_id_do_not_mix() {
        let r = RtcReassembly::new();
        let now = Instant::now();
        assert_eq!(
            r.accept(
                SESSION,
                1,
                0,
                FRAG_FRAGMENTED,
                Bytes::from_static(b"old-"),
                now
            ),
            Err(FragmentOutcome::Buffered)
        );
        assert_eq!(
            r.accept(
                SESSION + 1,
                1,
                0,
                FRAG_FRAGMENTED,
                Bytes::from_static(b"new-"),
                now
            ),
            Err(FragmentOutcome::Buffered),
            "the successor opens its OWN group, it does not join the old one"
        );
        let whole = r
            .accept(
                SESSION + 1,
                1,
                4,
                FRAG_FRAGMENTED | FRAG_LAST,
                Bytes::from_static(b"tail"),
                now,
            )
            .expect("completes")
            .expect("payload");
        assert_eq!(
            whole.as_ref(),
            b"new-tail",
            "the successor's payload must not contain the predecessor's bytes"
        );
        assert_eq!(r.held_bytes(SESSION), 4, "the old partial is still its own");
    }

    #[test]
    fn an_incomplete_group_is_reaped_after_its_ttl() {
        let r = RtcReassembly::new();
        let now = Instant::now();
        let _ = r.accept(SESSION, 1, 0, FRAG_FRAGMENTED, piece(64), now);
        assert_eq!(r.outstanding(), 1);
        r.expire(now + GROUP_TTL + Duration::from_millis(1));
        assert_eq!(r.outstanding(), 0);
    }
}
