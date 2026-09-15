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
//! [`GROUP_TTL`] is reaped — on every piece, not only when some
//! unrelated new group needs a slot.
//!
//! The state is keyed by wire session id, and a session that ends
//! **retires** its groups through [`RtcReassembly::retire_session`]:
//! the map outlives any one session, so leaving them behind would
//! pin a dead peer's bytes for as long as the mesh ran. Retirement
//! also fences the session for [`GROUP_TTL`], because a packet that
//! had already passed its session lookup when the retirement ran
//! would otherwise recreate exactly the state that was released.
//! Leaf fragment ids restart at 1 on every new session, so a
//! reconnect never inherits a predecessor's partial group either
//! way.

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

/// Sessions whose retirement is remembered at once.
///
/// The marker only has to outlive the packets that were already
/// in flight past their session lookup when the retirement ran, so
/// it expires with [`GROUP_TTL`]. Sixty-four is far above the
/// number of sessions that can end inside one such window, and past
/// it the oldest marker gives way: losing one re-opens nothing but
/// that microscopic window, and the group TTL still reaps whatever
/// a straggler recreated.
pub const MAX_RETIRED_SESSIONS: usize = 64;

/// Why a piece did not become a payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FragmentOutcome {
    /// Buffered; the group is not complete yet.
    Buffered,
    /// Byte-identical to a piece the group already holds, at the
    /// same offset: the legitimate retransmission a sender builds
    /// whenever an acknowledgement is lost. The group keeps what it
    /// has — treating this as a contradiction and discarding the
    /// group is how one lost ack destroyed a whole message.
    Duplicate,
    /// The group contradicted itself and was discarded.
    Malformed,
    /// The session is already holding as much partial state as it
    /// may.
    Refused,
    /// The piece's session has been retired. Its state is gone on
    /// purpose and this packet does not get to bring it back.
    Retired,
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
    /// When the group last accepted a piece.
    ///
    /// Last progress rather than opening: a sender whose retransmit
    /// timer is longer than [`GROUP_TTL`] would otherwise have its
    /// group reaped between two pieces it is still legitimately
    /// resending, and the piece that then arrives opens a group that
    /// can never complete. The lifetime stays bounded because a
    /// group accepts at most [`MAX_PIECES_PER_GROUP`] pieces.
    touched: Instant,
}

/// Per-session leaf-fragment reassembly for the RTC ingress.
#[derive(Debug, Default)]
pub struct RtcReassembly {
    groups: DashMap<(u64, u16), Partial>,
    /// Sessions retired within the last [`GROUP_TTL`].
    retired: DashMap<u64, Instant>,
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

    /// Release every group belonging to `session_id`, and fence it.
    ///
    /// The map outlives any one session, so a session that ends has
    /// to say so: otherwise its incomplete groups hold their bytes
    /// for as long as the mesh lives, and only the TTL — which
    /// nothing guarantees will be reached while the process is
    /// quiet — ever releases them.
    ///
    /// The fence is the other half. Ingress clones the session Arc
    /// before the packet reaches reassembly, so a packet that was
    /// already past that lookup when this ran would otherwise insert
    /// a fresh group under the very key just released. For
    /// [`GROUP_TTL`] after retirement, pieces for `session_id` are
    /// refused with [`FragmentOutcome::Retired`] instead.
    pub fn retire_session(&self, session_id: u64, now: Instant) {
        self.groups.retain(|(s, _), _| *s != session_id);
        self.sweep_retired(now);
        while self.retired.len() >= MAX_RETIRED_SESSIONS {
            let Some(oldest) = self
                .retired
                .iter()
                .min_by_key(|e| *e.value())
                .map(|e| *e.key())
            else {
                break;
            };
            self.retired.remove(&oldest);
        }
        self.retired.insert(session_id, now);
    }

    /// Offer one inbound piece.
    ///
    /// `Ok(Some(payload))` when it completed its group, `Ok(None)`
    /// when the packet was not a fragment at all (the caller keeps
    /// its event as-is), and `Err(outcome)` when the piece was
    /// buffered, duplicated, refused, retired or discarded.
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

        // Age groups out on every piece, not only when a new group
        // wants a slot. A deadline applied when unrelated traffic
        // happens to arrive is not a deadline: without this a group
        // could sit for as long as the mesh lived, and a piece
        // arriving arbitrarily late could still complete it.
        self.expire(now);
        if !self.retired.is_empty() && self.retired.contains_key(&session_id) {
            return Err(FragmentOutcome::Retired);
        }

        let fresh = !self.groups.contains_key(&key);
        if fresh {
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
                    touched: now,
                },
            );
        } else {
            // Both of these read the map, so neither may run while
            // the write guard below is held.
            //
            // The duplicate check comes first because a duplicate
            // adds no bytes: charging it against the session budget
            // would let an ordinary retransmission refuse the group
            // it is trying to complete.
            if self.groups.get(&key).is_some_and(|e| {
                e.value()
                    .pieces
                    .iter()
                    .any(|(o, d)| *o == offset && d == &data)
            }) {
                return Err(FragmentOutcome::Duplicate);
            }
            if self
                .held_bytes(session_id)
                .saturating_add(data.len() as u64)
                > MAX_PROVISIONAL_STREAM_BYTES
            {
                self.groups.remove(&key);
                return Err(FragmentOutcome::Refused);
            }
        }

        let complete = {
            let Some(mut entry) = self.groups.get_mut(&key) else {
                return Err(FragmentOutcome::Malformed);
            };
            let partial = entry.value_mut();
            let contradicts = partial.pieces.iter().any(|(o, d)| {
                let (s, e) = (*o as usize, *o as usize + d.len());
                // Any overlap that is not the exact duplicate
                // handled above: a group is written once.
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
            partial.touched = now;
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

    /// Reap groups whose last progress is older than [`GROUP_TTL`],
    /// and retirement markers past the same horizon.
    pub fn expire(&self, now: Instant) {
        self.groups
            .retain(|_, p| now.saturating_duration_since(p.touched) < GROUP_TTL);
        self.sweep_retired(now);
    }

    fn sweep_retired(&self, now: Instant) {
        if self.retired.is_empty() {
            return;
        }
        self.retired
            .retain(|_, at| now.saturating_duration_since(*at) < GROUP_TTL);
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

    /// The retransmission a sender builds whenever an ack is lost:
    /// the same piece arrives twice, and the retained head has to
    /// survive it so the tail can still complete the message.
    #[test]
    fn an_exact_duplicate_piece_leaves_the_group_intact() {
        let r = RtcReassembly::new();
        let now = Instant::now();
        let head = Bytes::from_static(b"native ");
        assert_eq!(
            r.accept(SESSION, 1, 0, FRAG_FRAGMENTED, head.clone(), now),
            Err(FragmentOutcome::Buffered)
        );
        assert_eq!(
            r.accept(SESSION, 1, 0, FRAG_FRAGMENTED, head.clone(), now),
            Err(FragmentOutcome::Duplicate),
            "a repeated piece is recovery, not a contradiction"
        );
        assert_eq!(r.held_bytes(SESSION), head.len() as u64, "nothing doubled");
        let whole = r
            .accept(
                SESSION,
                1,
                head.len() as u16,
                FRAG_FRAGMENTED | FRAG_LAST,
                Bytes::from_static(b"reassembly"),
                now,
            )
            .expect("completes after the duplicate")
            .expect("payload");
        assert_eq!(whole.as_ref(), b"native reassembly");
    }

    /// Same offset, same length, different bytes: not a
    /// retransmission of anything.
    #[test]
    fn a_conflicting_piece_at_a_held_offset_is_malformed() {
        let r = RtcReassembly::new();
        let now = Instant::now();
        let _ = r.accept(SESSION, 1, 0, FRAG_FRAGMENTED, piece(16), now);
        assert_eq!(
            r.accept(
                SESSION,
                1,
                0,
                FRAG_FRAGMENTED,
                Bytes::from(vec![0xFFu8; 16]),
                now
            ),
            Err(FragmentOutcome::Malformed)
        );
        assert_eq!(r.outstanding(), 0);
    }

    /// A session that ends releases its bytes, and a packet that was
    /// already in flight past its session lookup cannot bring them
    /// back. The successor is untouched by either.
    #[test]
    fn retiring_a_session_releases_its_groups_and_fences_late_packets() {
        let r = RtcReassembly::new();
        let now = Instant::now();
        let _ = r.accept(SESSION, 1, 0, FRAG_FRAGMENTED, piece(64), now);
        let _ = r.accept(SESSION + 1, 1, 0, FRAG_FRAGMENTED, piece(32), now);

        r.retire_session(SESSION, now);
        assert_eq!(r.held_bytes(SESSION), 0, "the old session held nothing now");
        assert_eq!(
            r.held_bytes(SESSION + 1),
            32,
            "retirement is exact: the successor keeps its own group"
        );

        assert_eq!(
            r.accept(SESSION, 2, 0, FRAG_FRAGMENTED, piece(64), now),
            Err(FragmentOutcome::Retired),
            "an already-admitted packet must not recreate retired state"
        );
        assert_eq!(r.held_bytes(SESSION), 0);

        // Past the horizon the marker is gone; nothing of the old
        // session survives it either way.
        let later = now + GROUP_TTL + Duration::from_millis(1);
        assert_eq!(
            r.accept(SESSION, 2, 0, FRAG_FRAGMENTED, piece(64), later),
            Err(FragmentOutcome::Buffered)
        );
    }

    /// The deadline is a deadline: a tail arriving long afterwards
    /// must not complete a group whose TTL has passed, and reaping
    /// must not wait for some unrelated new group to arrive.
    #[test]
    fn a_tail_past_the_ttl_cannot_complete_a_group() {
        let r = RtcReassembly::new();
        let now = Instant::now();
        assert_eq!(
            r.accept(
                SESSION,
                1,
                0,
                FRAG_FRAGMENTED,
                Bytes::from_static(b"head"),
                now
            ),
            Err(FragmentOutcome::Buffered)
        );
        let late = now + GROUP_TTL + Duration::from_millis(1);
        assert_eq!(
            r.accept(
                SESSION,
                1,
                4,
                FRAG_FRAGMENTED | FRAG_LAST,
                Bytes::from_static(b"tail"),
                late
            ),
            Err(FragmentOutcome::Buffered),
            "the head was reaped, so the tail is a new group's first piece"
        );
        assert_eq!(
            r.held_bytes(SESSION),
            4,
            "and it holds only its own bytes — no payload was assembled"
        );
    }
}
