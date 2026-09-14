//! Fragmentation and reassembly — the S0c decision, implemented.
//!
//! S0c (plan §7, the `MAX_PAYLOAD_SIZE` bullet) found that a payload
//! of 8 192 bytes fails [`NetHeader::validate`] on arrival and is
//! "dropped with **no log and no counter** — the channel looks dead".
//! `MAX_PAYLOAD_SIZE` is 8 108 (`8192 − 68 header − 16 tag`), and an
//! event carries a 4-byte length prefix inside it, so the largest
//! single event that fits one packet is [`MAX_FRAGMENT_PAYLOAD`].
//!
//! The decision: **the leaf fragments there**. It never emits an
//! over-cap packet, and it never silently discards a caller's bytes —
//! a payload it cannot carry comes back as a typed
//! [`LeafError::Wire`] from the send path.
//!
//! ## The wire fields, and what this module defines
//!
//! [`NetHeader`] has carried `fragment_id`, `fragment_offset` and
//! `frag_flags` since before the extraction, and `aad()` authenticates
//! all three — so a fragment's position cannot be tampered with
//! without breaking the AEAD tag. What did **not** exist anywhere in
//! the tree is an interpretation of `frag_flags`: nothing in the core
//! reads or writes it. This module defines that interpretation
//! ([`FRAG_FRAGMENTED`], [`FRAG_LAST`]) and is therefore the first
//! and only reader. See the Stage 5 report's named gaps: a native
//! node does not reassemble today, so an over-cap payload works
//! leaf ↔ leaf and needs a native-side reassembly arm before it
//! works leaf → native.
//!
//! ## Why the size ceiling is what it is
//!
//! `fragment_offset` is a `u16` **byte** offset (its documented
//! meaning), so the last fragment of a group can start no later than
//! byte 65 535. Rather than leave that as an implicit truncation,
//! [`MAX_FRAGMENTED_PAYLOAD`] states it: eight fragments, the most
//! whose start offsets are all representable. Above it the send path
//! refuses. An application that needs more has streams.
//!
//! [`NetHeader`]: net_wire::protocol::NetHeader
//! [`NetHeader::validate`]: net_wire::protocol::NetHeader::validate
//! [`LeafError::Wire`]: crate::error::LeafError::Wire

use std::collections::HashMap;

use bytes::{Bytes, BytesMut};
use net_wire::clock::Instant;
use net_wire::protocol::{EventFrame, MAX_PAYLOAD_SIZE};

use crate::counters::{DropReason, LeafCounters};
use crate::error::{LeafError, Result};

/// `frag_flags` bit 0: this packet carries a piece of a payload that
/// did not fit one packet. Clear on every unfragmented packet, so an
/// implementation that ignores the field sees exactly the traffic it
/// saw before fragmentation existed.
pub const FRAG_FRAGMENTED: u8 = 0b0000_0001;

/// `frag_flags` bit 1: this is the **final** piece of its group, so
/// `fragment_offset + payload.len()` is the reassembled length.
/// Meaningless without [`FRAG_FRAGMENTED`].
pub const FRAG_LAST: u8 = 0b0000_0010;

/// The largest single event that fits in one packet: the payload cap
/// minus the event frame's 4-byte length prefix.
pub const MAX_FRAGMENT_PAYLOAD: usize = MAX_PAYLOAD_SIZE - EventFrame::LEN_SIZE;

/// Fragments per group, capped so every `fragment_offset` is
/// representable in the header's `u16`.
pub const MAX_FRAGMENTS: usize = 8;

/// The largest payload the leaf will fragment. Beyond this the send
/// path returns an error; it does not truncate and it does not drop.
pub const MAX_FRAGMENTED_PAYLOAD: usize = MAX_FRAGMENT_PAYLOAD * MAX_FRAGMENTS;

// The offsets this module emits must fit the wire field. Checked at
// compile time rather than asserted at runtime: it is a property of
// the two constants above, not of any input.
const _: () = assert!(MAX_FRAGMENT_PAYLOAD * (MAX_FRAGMENTS - 1) <= u16::MAX as usize);

/// Concurrent inbound reassemblies a leaf will hold open.
///
/// The bound is the point: a peer that opens groups and never
/// finishes them would otherwise pin unbounded memory in a browser
/// tab. Eight groups × [`MAX_FRAGMENTED_PAYLOAD`] is ~507 KiB worst
/// case, which is a number a tab can afford to lose to a hostile
/// peer. The ninth simultaneous group is **refused and counted**
/// ([`DropReason::ReassemblyRefused`]) — never silently dropped.
pub const MAX_OUTSTANDING_REASSEMBLIES: usize = 8;

/// How long a partial group may sit incomplete before it is reaped.
///
/// Two seconds is the same order as the wire crate's
/// `GRANT_QUARANTINE_WINDOW`: long enough that a fragment set
/// separated by one retransmit RTO still completes, short enough that
/// a lost tail does not hold the slot against live traffic.
pub const REASSEMBLY_TTL_MS: u64 = 2_000;

/// One piece of an outbound payload, ready to become one packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fragment {
    /// Byte offset of this piece within the original payload.
    pub offset: u16,
    /// `frag_flags` for this piece's header.
    pub flags: u8,
    /// The piece itself.
    pub data: Bytes,
}

/// Split `payload` into the pieces one packet each can carry.
///
/// A payload that already fits returns exactly one [`Fragment`] with
/// `flags == 0` and `offset == 0` — byte-identical on the wire to
/// what the pre-fragmentation leaf sent, so turning fragmentation on
/// changed nothing for traffic below the cap.
pub fn split_payload(payload: &[u8]) -> Result<Vec<Fragment>> {
    if payload.len() <= MAX_FRAGMENT_PAYLOAD {
        return Ok(vec![Fragment {
            offset: 0,
            flags: 0,
            data: Bytes::copy_from_slice(payload),
        }]);
    }
    if payload.len() > MAX_FRAGMENTED_PAYLOAD {
        return Err(LeafError::Wire(format!(
            "payload of {} bytes exceeds the {MAX_FRAGMENTED_PAYLOAD}-byte \
             fragmentation ceiling ({MAX_FRAGMENTS} fragments of \
             {MAX_FRAGMENT_PAYLOAD}); use a stream",
            payload.len()
        )));
    }

    let mut out = Vec::with_capacity(payload.len().div_ceil(MAX_FRAGMENT_PAYLOAD));
    let mut offset = 0usize;
    while offset < payload.len() {
        let end = (offset + MAX_FRAGMENT_PAYLOAD).min(payload.len());
        let last = end == payload.len();
        out.push(Fragment {
            // Bounded by MAX_FRAGMENTED_PAYLOAD above, and the
            // compile-time assertion pins the last start offset.
            offset: offset as u16,
            flags: FRAG_FRAGMENTED | if last { FRAG_LAST } else { 0 },
            data: Bytes::copy_from_slice(&payload[offset..end]),
        });
        offset = end;
    }
    Ok(out)
}

/// One partially reassembled group.
#[derive(Debug)]
struct Partial {
    /// Pieces by offset. A `Vec` and not a map: at most
    /// [`MAX_FRAGMENTS`] entries, so a linear scan beats hashing.
    pieces: Vec<(u16, Bytes)>,
    /// Reassembled length, known once the `FRAG_LAST` piece arrives.
    total: Option<usize>,
    /// Bytes held so far.
    held: usize,
    /// When the group opened, for the TTL sweep.
    opened: Instant,
}

/// Inbound reassembly, bounded in both directions.
#[derive(Debug, Default)]
pub struct Reassembler {
    groups: HashMap<(u64, u16), Partial>,
}

impl Reassembler {
    /// An empty reassembler.
    pub fn new() -> Self {
        Self::default()
    }

    /// How many groups are open. The bound is observable so a test
    /// can assert the refusal rather than infer it.
    pub fn outstanding(&self) -> usize {
        self.groups.len()
    }

    /// Offer one inbound piece.
    ///
    /// `Some(payload)` when this piece completed its group;
    /// `None` when the piece was buffered, refused or discarded — in
    /// which case a counter moved.
    ///
    /// An unfragmented packet (`flags & FRAG_FRAGMENTED == 0`) is
    /// returned straight through, so the common path costs one bit
    /// test and no map lookup.
    #[expect(
        clippy::too_many_arguments,
        reason = "the fragment header's four wire fields plus the peer, the \
                  clock reading and the counter sink; a struct would only \
                  move the list and add a construction at every call site"
    )]
    pub fn accept(
        &mut self,
        peer: u64,
        fragment_id: u16,
        offset: u16,
        flags: u8,
        data: Bytes,
        now: Instant,
        counters: &LeafCounters,
    ) -> Option<Bytes> {
        if flags & FRAG_FRAGMENTED == 0 {
            return Some(data);
        }

        let key = (peer, fragment_id);
        let last = flags & FRAG_LAST != 0;
        let end = offset as usize + data.len();
        if end > MAX_FRAGMENTED_PAYLOAD {
            counters.drop_for(DropReason::ReassemblyInconsistent);
            return None;
        }

        if !self.groups.contains_key(&key) {
            if self.groups.len() >= MAX_OUTSTANDING_REASSEMBLIES {
                // Try the cheap remedy first — a full table is often
                // full of dead groups — then refuse.
                self.expire(now, counters);
            }
            if self.groups.len() >= MAX_OUTSTANDING_REASSEMBLIES {
                counters.drop_for(DropReason::ReassemblyRefused);
                return None;
            }
            self.groups.insert(
                key,
                Partial {
                    pieces: Vec::with_capacity(MAX_FRAGMENTS),
                    total: None,
                    held: 0,
                    opened: now,
                },
            );
        }

        // Present by construction: inserted just above when absent.
        let Some(partial) = self.groups.get_mut(&key) else {
            counters.drop_for(DropReason::ReassemblyInconsistent);
            return None;
        };

        let inconsistent = partial.pieces.iter().any(|(o, d)| {
            let (s, e) = (*o as usize, *o as usize + d.len());
            // Any overlap with an already-held piece, including an
            // exact duplicate: a group is written once.
            s < end && (offset as usize) < e
        }) || partial.pieces.len() >= MAX_FRAGMENTS
            || partial.total.is_some_and(|t| end > t || (last && end != t))
            || (!last && end > MAX_FRAGMENTED_PAYLOAD);
        if inconsistent {
            self.groups.remove(&key);
            counters.drop_for(DropReason::ReassemblyInconsistent);
            return None;
        }

        partial.held += data.len();
        partial.pieces.push((offset, data));
        if last {
            partial.total = Some(end);
        }

        let total = partial.total?;
        if partial.held != total {
            return None;
        }
        // Held bytes equal the total and no two pieces overlap, so
        // coverage of `0..total` is complete. Assemble in offset
        // order.
        let mut partial = self.groups.remove(&key)?;
        partial.pieces.sort_unstable_by_key(|(o, _)| *o);
        let mut out = BytesMut::with_capacity(total);
        for (_, piece) in &partial.pieces {
            out.extend_from_slice(piece);
        }
        counters.reassembled();
        Some(out.freeze())
    }

    /// Reap groups older than [`REASSEMBLY_TTL_MS`], counting each.
    pub fn expire(&mut self, now: Instant, counters: &LeafCounters) {
        let ttl = core::time::Duration::from_millis(REASSEMBLY_TTL_MS);
        let before = self.groups.len();
        self.groups
            .retain(|_, p| now.saturating_duration_since(p.opened) < ttl);
        for _ in 0..(before - self.groups.len()) {
            counters.drop_for(DropReason::ReassemblyExpired);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::now;

    fn payload(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    /// The boundary, exactly: the largest payload that fits one
    /// packet must produce ONE unfragmented piece, and one byte more
    /// must produce two fragmented ones. Off by one here is either a
    /// packet that `NetHeader::validate` silently drops on arrival
    /// (the S0c black hole) or needless fragmentation.
    #[test]
    fn the_fragmentation_boundary_is_max_payload_minus_the_length_prefix() {
        let fits = split_payload(&payload(MAX_FRAGMENT_PAYLOAD)).expect("at the cap");
        assert_eq!(fits.len(), 1);
        assert_eq!(fits[0].flags, 0, "an unfragmented packet sets no bits");
        assert_eq!(fits[0].offset, 0);
        assert_eq!(
            EventFrame::calculate_size(std::slice::from_ref(&fits[0].data)),
            MAX_PAYLOAD_SIZE,
            "the single event must exactly fill the payload cap"
        );

        let over = split_payload(&payload(MAX_FRAGMENT_PAYLOAD + 1)).expect("one over the cap");
        assert_eq!(over.len(), 2);
        assert_eq!(over[0].flags, FRAG_FRAGMENTED);
        assert_eq!(over[1].flags, FRAG_FRAGMENTED | FRAG_LAST);
        assert_eq!(over[1].data.len(), 1);
        for f in &over {
            assert!(
                EventFrame::calculate_size(std::slice::from_ref(&f.data)) <= MAX_PAYLOAD_SIZE,
                "a fragment must fit one packet"
            );
        }
    }

    #[test]
    fn a_payload_above_the_ceiling_is_refused_not_truncated() {
        let err = split_payload(&payload(MAX_FRAGMENTED_PAYLOAD + 1))
            .expect_err("above the ceiling must be an error, never a silent drop");
        match err {
            LeafError::Wire(msg) => {
                assert!(msg.contains("exceeds"), "{msg}");
                assert!(msg.contains("stream"), "the error must name the way out");
            }
            other => panic!("expected a wire error, got {other:?}"),
        }
        // And the largest accepted payload is accepted.
        let at = split_payload(&payload(MAX_FRAGMENTED_PAYLOAD)).expect("at the ceiling");
        assert_eq!(at.len(), MAX_FRAGMENTS);
        assert!(at.iter().all(|f| f.data.len() <= MAX_FRAGMENT_PAYLOAD));
    }

    #[test]
    fn split_then_reassemble_is_the_identity_even_out_of_order() {
        let original = payload(MAX_FRAGMENT_PAYLOAD * 3 + 17);
        let mut pieces = split_payload(&original).expect("splits");
        assert_eq!(pieces.len(), 4);
        pieces.reverse(); // last fragment first: total is known up front

        let c = LeafCounters::new();
        let mut r = Reassembler::new();
        let mut done = None;
        for f in &pieces {
            done = r.accept(7, 42, f.offset, f.flags, f.data.clone(), now(), &c);
        }
        assert_eq!(
            done.expect("the group completes").as_ref(),
            &original[..],
            "reassembly must reproduce the payload byte for byte"
        );
        assert_eq!(r.outstanding(), 0, "a completed group frees its slot");
        assert_eq!(c.total_drops(), 0);
    }

    #[test]
    fn an_unfragmented_packet_passes_straight_through() {
        let c = LeafCounters::new();
        let mut r = Reassembler::new();
        let out = r
            .accept(1, 0, 0, 0, Bytes::from_static(b"plain"), now(), &c)
            .expect("passes through");
        assert_eq!(&out[..], b"plain");
        assert_eq!(r.outstanding(), 0, "no group is opened for plain traffic");
    }

    /// The bound, and the counter that makes refusing it visible.
    #[test]
    fn the_reassembler_refuses_past_its_bound_and_counts_the_refusals() {
        let c = LeafCounters::new();
        let mut r = Reassembler::new();
        let head = Bytes::from(payload(MAX_FRAGMENT_PAYLOAD));
        for group in 0..MAX_OUTSTANDING_REASSEMBLIES as u16 {
            assert!(r
                .accept(1, group, 0, FRAG_FRAGMENTED, head.clone(), now(), &c)
                .is_none());
        }
        assert_eq!(r.outstanding(), MAX_OUTSTANDING_REASSEMBLIES);

        let refused = r.accept(
            1,
            MAX_OUTSTANDING_REASSEMBLIES as u16,
            0,
            FRAG_FRAGMENTED,
            head.clone(),
            now(),
            &c,
        );
        assert!(refused.is_none());
        assert_eq!(
            c.drops(DropReason::ReassemblyRefused),
            1,
            "a refusal must be counted, or the channel just looks dead"
        );
        assert_eq!(r.outstanding(), MAX_OUTSTANDING_REASSEMBLIES);

        // A group already open still makes progress under the bound.
        let tail = Bytes::from_static(b"tail");
        let done = r.accept(
            1,
            0,
            MAX_FRAGMENT_PAYLOAD as u16,
            FRAG_FRAGMENTED | FRAG_LAST,
            tail,
            now(),
            &c,
        );
        assert!(done.is_some(), "an open group is not blocked by the bound");
        assert_eq!(c.drops(DropReason::ReassemblyRefused), 1);
    }

    #[test]
    fn expiry_frees_slots_and_counts_what_it_dropped() {
        let c = LeafCounters::new();
        let mut r = Reassembler::new();
        let t0 = now();
        r.accept(1, 1, 0, FRAG_FRAGMENTED, Bytes::from(payload(64)), t0, &c);
        assert_eq!(r.outstanding(), 1);

        r.expire(t0, &c);
        assert_eq!(r.outstanding(), 1, "not yet due");

        let later = t0 + core::time::Duration::from_millis(REASSEMBLY_TTL_MS + 1);
        r.expire(later, &c);
        assert_eq!(r.outstanding(), 0);
        assert_eq!(c.drops(DropReason::ReassemblyExpired), 1);
    }

    #[test]
    fn a_contradictory_fragment_drops_the_whole_group() {
        let c = LeafCounters::new();
        let mut r = Reassembler::new();
        let a = Bytes::from(payload(100));
        assert!(r.accept(1, 5, 0, FRAG_FRAGMENTED, a, now(), &c).is_none());
        // Overlaps 0..100.
        assert!(r
            .accept(
                1,
                5,
                50,
                FRAG_FRAGMENTED | FRAG_LAST,
                Bytes::from(payload(100)),
                now(),
                &c
            )
            .is_none());
        assert_eq!(c.drops(DropReason::ReassemblyInconsistent), 1);
        assert_eq!(
            r.outstanding(),
            0,
            "a contradicted group is abandoned, not left half-written"
        );
    }

    #[test]
    fn a_duplicate_fragment_is_inconsistent_rather_than_double_counted() {
        let c = LeafCounters::new();
        let mut r = Reassembler::new();
        let piece = Bytes::from(payload(32));
        r.accept(1, 9, 0, FRAG_FRAGMENTED, piece.clone(), now(), &c);
        assert!(r
            .accept(1, 9, 0, FRAG_FRAGMENTED, piece, now(), &c)
            .is_none());
        assert_eq!(c.drops(DropReason::ReassemblyInconsistent), 1);
    }
}
