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
use net_wire::protocol::MAX_EVENT_SIZE;

use crate::counters::{DropReason, LeafCounters};
use crate::error::{LeafError, Result};

// The interpretation of `frag_flags` now lives beside the header
// that carries it, in the wire crate, because the native RTC
// ingress reassembles leaf fragments and the two ends must read one
// definition. Re-exported here so this module stays the leaf's
// single fragmentation vocabulary.
pub use net_wire::protocol::{FRAG_FRAGMENTED, FRAG_LAST};

/// The largest single event that fits in one packet.
///
/// The wire crate's [`MAX_EVENT_SIZE`] — the payload cap minus the
/// event frame's 4-byte length prefix — under the name this module
/// reasons in. One definition, because the native send path refuses
/// above exactly this bound and the leaf fragments at exactly it: two
/// derivations of the same number could drift apart and turn a
/// refusal into a drop.
pub const MAX_FRAGMENT_PAYLOAD: usize = MAX_EVENT_SIZE;

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

/// Where one inbound piece came from.
///
/// The fields a delivered event is attributed to. They are carried
/// per piece rather than read off whichever packet happened to
/// complete a group: a group's payload belongs to its **first**
/// fragment's sequence and header, and synthesising that from the
/// completing arrival is exactly the defect R7 names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PieceMeta {
    /// Per-stream sequence the piece's packet carried.
    pub sequence: u64,
    /// Stream the piece arrived on.
    pub stream_id: u64,
    /// The publisher's full 64-bit origin hash.
    pub origin_hash: u64,
    /// The `u16` channel-hash hint.
    pub channel_hash: u16,
    /// The subprotocol that put the piece on the stream.
    ///
    /// Carried per piece for exactly the reason the stream and the
    /// origin are: a group's plane belongs to its **first**
    /// fragment. Reading it off whichever packet completed the
    /// group let one session-authenticated producer send a prefix
    /// under one plane and the finishing piece under another, and
    /// have the whole assembled payload dispatched under the
    /// second — an event body decoded as channel membership, or
    /// membership decoded as an event, after both planes'
    /// sequences were already acknowledged.
    pub subprotocol_id: u16,
    /// Whether the piece's packet declared the reliable mode.
    ///
    /// The mode is what decides the consumer cursor's gap
    /// disposition — hold and recover, or skip and count — so a
    /// group assembled out of pieces that disagree about it has no
    /// single answer to give the reorder buffer. Bound here, so
    /// there is one.
    pub reliable: bool,
}

/// A complete payload, and the stream sequences it consumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assembled {
    /// Provenance of the group's **first** piece — the sequence the
    /// reorder buffer must key this payload under.
    pub meta: PieceMeta,
    /// How many stream sequences the group occupies. One for an
    /// unfragmented packet; the group's sequence span otherwise.
    pub span: u64,
    /// The reassembled payload.
    pub data: Bytes,
}

/// One piece of a group, as it arrived.
#[derive(Debug)]
struct Piece {
    /// Byte offset the piece starts at.
    offset: u16,
    /// Stream sequence the piece's packet carried. Zero on the
    /// sequence-less control path, where no piece owns a sequence.
    sequence: u64,
    /// The piece's bytes.
    data: Bytes,
}

/// One partially reassembled group.
#[derive(Debug)]
struct Partial {
    /// Pieces as they arrived. A `Vec` and not a map: at most
    /// [`MAX_FRAGMENTS`] entries, so a linear scan beats hashing.
    pieces: Vec<Piece>,
    /// Reassembled length, known once the `FRAG_LAST` piece arrives.
    total: Option<usize>,
    /// Bytes held so far.
    held: usize,
    /// When the group last accepted a piece, for the TTL sweep.
    ///
    /// Last progress rather than opening. A sender whose retransmit
    /// timer is longer than [`REASSEMBLY_TTL_MS`] would otherwise
    /// have its group reaped between two pieces it is still
    /// legitimately resending, and the tail arriving afterwards
    /// opens a group that can never complete — silent loss of bytes
    /// the receive side has already acknowledged. The lifetime
    /// stays bounded: a group accepts at most [`MAX_FRAGMENTS`]
    /// pieces, so it can be extended at most that many times.
    touched: Instant,
    /// The delivery metadata every piece of the group must agree
    /// on, fixed by its first arrival, with `sequence` holding the
    /// lowest sequence seen so far.
    ///
    /// Fixing it is the point. The group key is only
    /// `(scope, fragment_id)`, so without this one payload could be
    /// assembled out of pieces claiming different streams, origins,
    /// channels, planes and modes, and would be delivered under
    /// whichever piece happened to carry the lowest sequence —
    /// while the rest of its provenance came from whichever piece
    /// happened to arrive last.
    first: PieceMeta,
    /// Whether the caller supplies stream sequences for this group.
    sequenced: bool,
}

/// A fragment group whose retained bytes were given up on after its
/// sequences had already been acknowledged.
///
/// The reassembler's bounds are real — a TTL, a group count, a
/// consistency rule — and each of them can end a group whose
/// pieces the receive path already acknowledged. The
/// acknowledgement is what retires the sender's only copy: from
/// that moment the bytes cannot come back, because the peer has
/// nothing left to rebuild from and no wire gap remains to NACK.
/// Removing them and moving a counter is therefore silent loss of
/// data this leaf claimed as progress. So a group names the stream
/// it owned on its way out, and the node turns that into the
/// stream's typed terminal disposition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Abandoned {
    /// Session incarnation the group belonged to.
    pub scope: u64,
    /// The stream whose sequences the group consumed.
    pub stream_id: u64,
    /// The mode the group's FIRST fragment declared.
    ///
    /// **The complete-or-terminal rule is reliability's, not
    /// sequencing's.** The paragraph above is exactly true of a
    /// reliable group: its sequences were acknowledged, the
    /// acknowledgement retired the sender's only copy, and giving
    /// up on the bytes afterwards is loss of data this leaf claimed
    /// as progress — so the stream ends, typed. A fire-and-forget
    /// group's contract is the opposite one. Its sequences were
    /// never retransmittable in the first place, an incomplete
    /// group is the loss its mode already permits, and ending the
    /// stream over it makes an expected loss permanently fatal to
    /// every later message on that id. So the mode rides out with
    /// the group and the node dispositions each kind by its own
    /// contract.
    pub reliable: bool,
}

/// Report a group whose retained bytes are being given up on.
///
/// Only a **sequenced** group is reported. It is the one whose
/// pieces consumed stream sequences the receive path acknowledged;
/// the sequence-less control path owns no stream and no
/// acknowledgement, so nothing there can be stranded by a reap.
fn note_abandoned(abandoned: &mut Vec<Abandoned>, scope: u64, partial: &Partial) {
    if partial.sequenced {
        abandoned.push(Abandoned {
            scope,
            stream_id: partial.first.stream_id,
            reliable: partial.first.reliable,
        });
    }
}

/// Inbound reassembly, bounded in both directions.
#[derive(Debug, Default)]
pub struct Reassembler {
    groups: HashMap<(u64, u16), Partial>,
    abandoned: Vec<Abandoned>,
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

    /// Drop every group belonging to `scope`.
    ///
    /// A replaced session restarts its fragment ids at 1, so a
    /// partial group left behind by the predecessor would be
    /// indistinguishable from the successor's first group and the two
    /// would interleave into one corrupt payload. Retiring by scope
    /// is what keeps a re-handshake from mixing them.
    pub fn retire(&mut self, scope: u64) {
        self.groups.retain(|(s, _), _| *s != scope);
    }

    /// Drop every group of one stream's receive lifetime.
    ///
    /// A RESET ends that lifetime: the peer's send half gave up and
    /// may restart the id from sequence zero, so the cursor and the
    /// reliability ranges are reset. A partial group retained from
    /// before the reset is not part of the lifetime that follows it
    /// — leave it and an old delayed tail completes it afterwards,
    /// and a payload from before the reset is delivered against the
    /// fresh cursor, behind the `PeerReset` the consumer was already
    /// given. Nothing is reported as abandoned: the reset **is** the
    /// stream's terminal disposition, and the caller owns it.
    pub fn retire_stream(&mut self, scope: u64, stream_id: u64, counters: &LeafCounters) {
        let before = self.groups.len();
        self.groups
            .retain(|(s, _), p| *s != scope || !p.sequenced || p.first.stream_id != stream_id);
        for _ in 0..(before - self.groups.len()) {
            counters.drop_for(DropReason::StreamFailed);
        }
    }

    /// Take the groups whose acknowledged ownership was given up on.
    ///
    /// Drained rather than counted: a counter cannot name the stream
    /// that lost its bytes, and naming it is the whole difference
    /// between a bounded reassembler and one that acknowledges data
    /// it then discards.
    pub fn take_abandoned(&mut self) -> Vec<Abandoned> {
        core::mem::take(&mut self.abandoned)
    }

    /// Remove `key`, reporting whatever ownership it was holding.
    fn abandon(&mut self, key: (u64, u16)) {
        if let Some(partial) = self.groups.remove(&key) {
            note_abandoned(&mut self.abandoned, key.0, &partial);
        }
    }

    /// Offer one inbound piece, without sequence context.
    ///
    /// The control subprotocols ride the control stream and are not
    /// reordered, so a fragment of one owns no sequence span; this is
    /// their entry point, and the pure statement of the reassembly
    /// contract. Event-plane traffic goes through
    /// [`Self::accept_piece`], which additionally carries the
    /// provenance the consumer-side reorder keys on.
    #[expect(
        clippy::too_many_arguments,
        reason = "the fragment header's four wire fields plus the scope, the \
                  clock reading and the counter sink; a struct would only \
                  move the list and add a construction at every call site"
    )]
    pub fn accept(
        &mut self,
        scope: u64,
        fragment_id: u16,
        offset: u16,
        flags: u8,
        data: Bytes,
        now: Instant,
        counters: &LeafCounters,
    ) -> Option<Bytes> {
        self.accept_inner(scope, None, fragment_id, offset, flags, data, now, counters)
            .map(|assembled| assembled.data)
    }

    /// Offer one inbound piece carrying its packet's provenance.
    ///
    /// `Some(assembled)` when this piece completed its group — with
    /// the **first** piece's sequence and header, and the number of
    /// sequences the group consumed. `None` when the piece was
    /// buffered, refused or discarded, in which case a counter moved.
    ///
    /// An unfragmented packet (`flags & FRAG_FRAGMENTED == 0`) is
    /// returned straight through owning its own single sequence, so
    /// the common path costs one bit test and no map lookup.
    #[expect(
        clippy::too_many_arguments,
        reason = "the fragment header's four wire fields plus the scope, the \
                  arrival's provenance, the clock reading and the counter \
                  sink; a struct would only move the list"
    )]
    pub fn accept_piece(
        &mut self,
        scope: u64,
        meta: PieceMeta,
        fragment_id: u16,
        offset: u16,
        flags: u8,
        data: Bytes,
        now: Instant,
        counters: &LeafCounters,
    ) -> Option<Assembled> {
        self.accept_inner(
            scope,
            Some(meta),
            fragment_id,
            offset,
            flags,
            data,
            now,
            counters,
        )
    }

    /// Whether a piece of `fragment_id` would be retained right now.
    ///
    /// A caller asks this **before** it acknowledges the packet's
    /// sequence, because the two decisions have to agree. A fragment
    /// refused for capacity is a fragment the peer must stay free to
    /// send again; a cumulative acknowledgement takes that freedom
    /// away, and the group's bytes are then lost for good with
    /// nothing left to recover them from. Refusing before the
    /// acknowledgement leaves the sequence outstanding, so the
    /// ordinary retransmit brings the piece back once a slot frees —
    /// capacity pressure becomes backpressure rather than silent
    /// loss.
    ///
    /// `true` for anything that is not a fragment, and for a group
    /// that is already open: an open group is never blocked by the
    /// bound. A refusal is counted here, so the caller reports it by
    /// returning rather than by inventing a second reason.
    pub fn admits(
        &mut self,
        scope: u64,
        fragment_id: u16,
        flags: u8,
        now: Instant,
        counters: &LeafCounters,
    ) -> bool {
        if flags & FRAG_FRAGMENTED == 0 {
            return true;
        }
        // Before the open-group shortcut, not after it. A group
        // that is past its deadline is not an open group, and
        // answering `true` for one is how a late piece was
        // acknowledged and then found its own group reaped by the
        // very next step.
        self.expire(now, counters);
        if self.groups.contains_key(&(scope, fragment_id))
            || self.groups.len() < MAX_OUTSTANDING_REASSEMBLIES
        {
            return true;
        }
        counters.drop_for(DropReason::ReassemblyRefused);
        false
    }

    /// The one reassembly rule. `meta` is `None` on the
    /// sequence-less control path, where no piece owns a sequence
    /// and the group has no provenance to keep consistent.
    #[expect(
        clippy::too_many_arguments,
        reason = "the fragment header's four wire fields plus the scope, the \
                  arrival's provenance, the clock reading and the counter \
                  sink; a struct would only move the list"
    )]
    fn accept_inner(
        &mut self,
        scope: u64,
        meta: Option<PieceMeta>,
        fragment_id: u16,
        offset: u16,
        flags: u8,
        data: Bytes,
        now: Instant,
        counters: &LeafCounters,
    ) -> Option<Assembled> {
        if flags & FRAG_FRAGMENTED == 0 {
            return Some(Assembled {
                meta: meta.unwrap_or_default(),
                span: 1,
                data,
            });
        }

        let key = (scope, fragment_id);
        let last = flags & FRAG_LAST != 0;
        let end = offset as usize + data.len();
        if end > MAX_FRAGMENTED_PAYLOAD {
            counters.drop_for(DropReason::ReassemblyInconsistent);
            return None;
        }

        // Age groups out on every piece, not only when a new group
        // wants a slot. A deadline that is applied when unrelated
        // traffic happens to arrive is not a deadline: without this,
        // a group could sit for as long as the session lived and a
        // tail arriving arbitrarily late could still complete it.
        self.expire(now, counters);

        if !self.groups.contains_key(&key) {
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
                    touched: now,
                    first: meta.unwrap_or_default(),
                    sequenced: meta.is_some(),
                },
            );
        }

        // Present by construction: inserted just above when absent.
        let Some(partial) = self.groups.get_mut(&key) else {
            counters.drop_for(DropReason::ReassemblyInconsistent);
            return None;
        };

        // Group-wide delivery metadata. Every piece must claim the
        // stream, origin, channel, plane and mode the group's first
        // piece claimed. A piece that does not is not a piece of
        // this group, whatever its fragment id says — and accepting
        // it would deliver one peer's bytes under another's stream,
        // or one plane's bytes decoded as another's.
        if meta.is_some_and(|m| {
            m.stream_id != partial.first.stream_id
                || m.origin_hash != partial.first.origin_hash
                || m.channel_hash != partial.first.channel_hash
                || m.subprotocol_id != partial.first.subprotocol_id
                || m.reliable != partial.first.reliable
        }) {
            self.abandon(key);
            counters.drop_for(DropReason::ReassemblyInconsistent);
            return None;
        }

        let sequence = meta.map_or(0, |m| m.sequence);

        // A legitimate retransmission: the same offset, the same
        // sequence, the same bytes. The sender rebuilds an
        // unacknowledged fragment with a fresh AEAD counter whenever
        // its acknowledgement is lost, so this arrives in ordinary
        // recovery, and the caller has already repeated the
        // acknowledgement the peer is missing. Here it is a no-op.
        // Treating it as an overlap and discarding the group is what
        // destroyed a partial message every time one ack was lost.
        if partial
            .pieces
            .iter()
            .any(|p| p.offset == offset && p.sequence == sequence && p.data == data)
        {
            counters.drop_for(DropReason::ReassemblyDuplicate);
            return None;
        }

        let inconsistent = partial.pieces.iter().any(|p| {
            let (s, e) = (p.offset as usize, p.offset as usize + p.data.len());
            // Conflicting bytes: any overlap that is not the exact
            // duplicate handled above. Or a sequence the group
            // already holds a piece for — one sequence carries one
            // piece, and two pieces on one sequence would let a
            // group claim a span it does not own.
            (s < end && (offset as usize) < e) || (partial.sequenced && p.sequence == sequence)
        }) || partial.pieces.len() >= MAX_FRAGMENTS
            || partial.total.is_some_and(|t| end > t || (last && end != t))
            // The declared end binds pieces that arrived BEFORE it.
            // Checking only pieces that arrive after the LAST fragment
            // is what let a group hold bytes past its own total while
            // its prefix was missing, and still assemble because the
            // held-byte count happened to equal the total.
            || (last
                && partial
                    .pieces
                    .iter()
                    .any(|p| p.offset as usize + p.data.len() > end));
        if inconsistent {
            self.abandon(key);
            counters.drop_for(DropReason::ReassemblyInconsistent);
            return None;
        }

        partial.held += data.len();
        partial.pieces.push(Piece {
            offset,
            sequence,
            data,
        });
        partial.touched = now;
        if sequence < partial.first.sequence {
            partial.first.sequence = sequence;
        }
        if last {
            partial.total = Some(end);
        }

        let total = partial.total?;
        if partial.held != total {
            return None;
        }
        let mut partial = self.groups.remove(&key)?;
        partial.pieces.sort_unstable_by_key(|p| p.offset);

        // Prove the coverage rather than infer it from the byte
        // count: walk the sorted pieces and require each to start
        // exactly where the last one ended, from zero to the declared
        // total. Eight pieces at most, so this is cheaper than the
        // reasoning needed to be sure the byte count implied it.
        let mut covered = 0usize;
        for piece in &partial.pieces {
            if piece.offset as usize != covered {
                note_abandoned(&mut self.abandoned, scope, &partial);
                counters.drop_for(DropReason::ReassemblyInconsistent);
                return None;
            }
            covered += piece.data.len();
        }
        if covered != total {
            note_abandoned(&mut self.abandoned, scope, &partial);
            counters.drop_for(DropReason::ReassemblyInconsistent);
            return None;
        }

        // The group owns exactly the sequences its pieces arrived on,
        // and they must be contiguous. A `max - min + 1` span would
        // let two pieces on sequences 0 and 2 claim sequence 1 — a
        // record that was never part of this group, whose own payload
        // the advancing reorder cursor would then step over. The span
        // needs no separate bound: it is the piece count, which
        // `MAX_FRAGMENTS` already caps.
        let span = if partial.sequenced {
            let mut seqs = [0u64; MAX_FRAGMENTS];
            for (slot, piece) in seqs.iter_mut().zip(&partial.pieces) {
                *slot = piece.sequence;
            }
            let seqs = &mut seqs[..partial.pieces.len()];
            seqs.sort_unstable();
            if seqs.windows(2).any(|w| w[1] != w[0] + 1) {
                note_abandoned(&mut self.abandoned, scope, &partial);
                counters.drop_for(DropReason::ReassemblyInconsistent);
                return None;
            }
            seqs.len() as u64
        } else {
            1
        };

        let mut out = BytesMut::with_capacity(total);
        for piece in &partial.pieces {
            out.extend_from_slice(&piece.data);
        }
        counters.reassembled();
        Some(Assembled {
            meta: partial.first,
            span,
            data: out.freeze(),
        })
    }

    /// Reap groups older than [`REASSEMBLY_TTL_MS`], counting each,
    /// and reporting the acknowledged ownership each one held.
    pub fn expire(&mut self, now: Instant, counters: &LeafCounters) {
        let ttl = core::time::Duration::from_millis(REASSEMBLY_TTL_MS);
        let before = self.groups.len();
        let abandoned = &mut self.abandoned;
        self.groups.retain(|key, p| {
            if now.saturating_duration_since(p.touched) < ttl {
                return true;
            }
            note_abandoned(abandoned, key.0, p);
            false
        });
        for _ in 0..(before - self.groups.len()) {
            counters.drop_for(DropReason::ReassemblyExpired);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::now;
    use net_wire::protocol::{EventFrame, MAX_PAYLOAD_SIZE};

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

    fn provenance(sequence: u64) -> PieceMeta {
        PieceMeta {
            sequence,
            stream_id: 4,
            origin_hash: 5,
            channel_hash: 6,
            subprotocol_id: 0x0A00,
            reliable: true,
        }
    }

    /// The retransmission production itself creates: when an ack is
    /// lost the sender rebuilds the very same fragment, so the same
    /// piece arrives twice. The retained partial has to survive it
    /// and still complete — discarding the group on the duplicate is
    /// how a legitimate recovery lost a whole message.
    #[test]
    fn an_exact_duplicate_fragment_is_a_counted_no_op() {
        let c = LeafCounters::new();
        let mut r = Reassembler::new();
        let head = Bytes::from(payload(32));
        assert!(r
            .accept_piece(
                1,
                provenance(0),
                9,
                0,
                FRAG_FRAGMENTED,
                head.clone(),
                now(),
                &c
            )
            .is_none());
        assert!(r
            .accept_piece(1, provenance(0), 9, 0, FRAG_FRAGMENTED, head, now(), &c)
            .is_none());
        assert_eq!(c.drops(DropReason::ReassemblyDuplicate), 1);
        assert_eq!(c.drops(DropReason::ReassemblyInconsistent), 0);
        assert_eq!(r.outstanding(), 1, "the retained partial must survive");

        let done = r
            .accept_piece(
                1,
                provenance(1),
                9,
                32,
                FRAG_FRAGMENTED | FRAG_LAST,
                Bytes::from_static(b"tail"),
                now(),
                &c,
            )
            .expect("the group completes after the duplicate");
        assert_eq!(done.data.len(), 36);
        assert_eq!(done.span, 2, "the group owns both its sequences");
        assert_eq!(done.meta.sequence, 0, "delivery keys on the first piece");
    }

    /// Same offset, same length, different bytes: not a retransmit
    /// of anything, and the group cannot be written twice.
    #[test]
    fn a_fragment_conflicting_with_a_held_piece_is_refused() {
        let c = LeafCounters::new();
        let mut r = Reassembler::new();
        r.accept(
            1,
            9,
            0,
            FRAG_FRAGMENTED,
            Bytes::from(payload(32)),
            now(),
            &c,
        );
        assert!(r
            .accept(
                1,
                9,
                0,
                FRAG_FRAGMENTED,
                Bytes::from(vec![0xFF; 32]),
                now(),
                &c
            )
            .is_none());
        assert_eq!(c.drops(DropReason::ReassemblyDuplicate), 0);
        assert_eq!(c.drops(DropReason::ReassemblyInconsistent), 1);
        assert_eq!(r.outstanding(), 0);
    }

    /// Two pieces on sequences 0 and 2 cover their bytes exactly,
    /// but the group would claim sequence 1 — a record that was
    /// never a fragment of it, and whose own payload the reorder
    /// cursor would then step over.
    #[test]
    fn a_group_cannot_claim_a_sequence_none_of_its_pieces_arrived_on() {
        let c = LeafCounters::new();
        let mut r = Reassembler::new();
        assert!(r
            .accept_piece(
                1,
                provenance(0),
                3,
                0,
                FRAG_FRAGMENTED,
                Bytes::from_static(b"head"),
                now(),
                &c
            )
            .is_none());
        assert!(
            r.accept_piece(
                1,
                provenance(2),
                3,
                4,
                FRAG_FRAGMENTED | FRAG_LAST,
                Bytes::from_static(b"tail"),
                now(),
                &c
            )
            .is_none(),
            "a gap in the group's sequences is not a group"
        );
        assert_eq!(c.drops(DropReason::ReassemblyInconsistent), 1);
    }

    /// One group, two streams. The group key is only
    /// `(scope, fragment_id)`, so nothing but this check stops a
    /// peer assembling one payload out of pieces that claim
    /// different delivery metadata.
    #[test]
    fn a_group_cannot_change_its_stream_origin_or_channel() {
        let c = LeafCounters::new();
        let mut r = Reassembler::new();
        assert!(r
            .accept_piece(
                1,
                provenance(0),
                3,
                0,
                FRAG_FRAGMENTED,
                Bytes::from_static(b"head"),
                now(),
                &c
            )
            .is_none());
        let elsewhere = PieceMeta {
            stream_id: 40,
            ..provenance(1)
        };
        assert!(
            r.accept_piece(
                1,
                elsewhere,
                3,
                4,
                FRAG_FRAGMENTED | FRAG_LAST,
                Bytes::from_static(b"tail"),
                now(),
                &c
            )
            .is_none(),
            "a piece claiming another stream is not a piece of this group"
        );
        assert_eq!(c.drops(DropReason::ReassemblyInconsistent), 1);
        assert_eq!(r.outstanding(), 0);
    }

    /// The deadline is a deadline: a group must not be completable
    /// long afterwards just because no other group needed its slot.
    #[test]
    fn a_tail_arriving_past_the_ttl_cannot_complete_a_reaped_group() {
        let c = LeafCounters::new();
        let mut r = Reassembler::new();
        let t0 = now();
        assert!(r
            .accept(
                1,
                7,
                0,
                FRAG_FRAGMENTED,
                Bytes::from_static(b"head"),
                t0,
                &c
            )
            .is_none());
        let late = t0 + core::time::Duration::from_millis(REASSEMBLY_TTL_MS + 1);
        assert!(
            r.accept(
                1,
                7,
                4,
                FRAG_FRAGMENTED | FRAG_LAST,
                Bytes::from_static(b"tail"),
                late,
                &c
            )
            .is_none(),
            "the head was reaped, so the tail cannot assemble a payload"
        );
        assert_eq!(c.drops(DropReason::ReassemblyExpired), 1);
    }

    /// The plane and the mode are group-wide facts too, and the
    /// group key does not carry them. A producer that keeps the
    /// stream, origin, channel and sequence span honest and changes
    /// only the completing piece's subprotocol would otherwise have
    /// its whole payload dispatched under that piece's plane —
    /// after both planes' sequences were acknowledged.
    #[test]
    fn a_group_cannot_change_its_plane() {
        let c = LeafCounters::new();
        let mut r = Reassembler::new();
        assert!(r
            .accept_piece(
                1,
                provenance(0),
                3,
                0,
                FRAG_FRAGMENTED,
                Bytes::from_static(b"head"),
                now(),
                &c
            )
            .is_none());
        let other_plane = PieceMeta {
            subprotocol_id: 0x0B00,
            ..provenance(1)
        };
        assert!(
            r.accept_piece(
                1,
                other_plane,
                3,
                4,
                FRAG_FRAGMENTED | FRAG_LAST,
                Bytes::from_static(b"tail"),
                now(),
                &c
            )
            .is_none(),
            "a piece claiming another plane is not a piece of this group"
        );
        assert_eq!(c.drops(DropReason::ReassemblyInconsistent), 1);
        assert_eq!(r.outstanding(), 0);
    }

    /// Same for the reliable bit: it is what the consumer cursor's
    /// gap disposition is chosen from, so a group whose pieces
    /// disagree about it has no single answer to give.
    #[test]
    fn a_group_cannot_change_its_mode() {
        let c = LeafCounters::new();
        let mut r = Reassembler::new();
        assert!(r
            .accept_piece(
                1,
                provenance(0),
                3,
                0,
                FRAG_FRAGMENTED,
                Bytes::from_static(b"head"),
                now(),
                &c
            )
            .is_none());
        let other_mode = PieceMeta {
            reliable: false,
            ..provenance(1)
        };
        assert!(
            r.accept_piece(
                1,
                other_mode,
                3,
                4,
                FRAG_FRAGMENTED | FRAG_LAST,
                Bytes::from_static(b"tail"),
                now(),
                &c
            )
            .is_none(),
            "a piece claiming another mode is not a piece of this group"
        );
        assert_eq!(c.drops(DropReason::ReassemblyInconsistent), 1);
        assert_eq!(r.outstanding(), 0);
    }

    /// A reap of a sequenced group names the stream whose
    /// acknowledged sequences it was holding. Counting the loss is
    /// what the caller cannot act on: the stream id is.
    #[test]
    fn a_reaped_sequenced_group_names_the_stream_it_owned() {
        let c = LeafCounters::new();
        let mut r = Reassembler::new();
        let t0 = now();
        assert!(r
            .accept_piece(
                9,
                provenance(0),
                3,
                0,
                FRAG_FRAGMENTED,
                Bytes::from_static(b"head"),
                t0,
                &c
            )
            .is_none());
        assert!(r.take_abandoned().is_empty(), "nothing is lost yet");
        r.expire(
            t0 + core::time::Duration::from_millis(REASSEMBLY_TTL_MS),
            &c,
        );
        assert_eq!(
            r.take_abandoned(),
            vec![Abandoned {
                scope: 9,
                stream_id: 4,
                reliable: true
            }],
            "the reap must surrender the stream's ownership, not just count it"
        );
        assert!(
            r.take_abandoned().is_empty(),
            "a drained report is not reported twice"
        );
    }

    /// The sequence-less control path owns no stream and no
    /// acknowledgement, so its reap strands nothing and must not
    /// manufacture a terminal disposition for stream zero.
    #[test]
    fn a_reaped_control_group_strands_no_stream() {
        let c = LeafCounters::new();
        let mut r = Reassembler::new();
        let t0 = now();
        assert!(r
            .accept(
                1,
                7,
                0,
                FRAG_FRAGMENTED,
                Bytes::from_static(b"head"),
                t0,
                &c
            )
            .is_none());
        r.expire(
            t0 + core::time::Duration::from_millis(REASSEMBLY_TTL_MS),
            &c,
        );
        assert_eq!(c.drops(DropReason::ReassemblyExpired), 1);
        assert!(r.take_abandoned().is_empty());
    }

    /// A contradicted group has usually already been acknowledged
    /// too, so destroying it surrenders the same ownership a reap
    /// does.
    #[test]
    fn a_contradicted_sequenced_group_names_the_stream_it_owned() {
        let c = LeafCounters::new();
        let mut r = Reassembler::new();
        assert!(r
            .accept_piece(
                2,
                provenance(0),
                3,
                0,
                FRAG_FRAGMENTED,
                Bytes::from_static(b"head"),
                now(),
                &c
            )
            .is_none());
        assert!(r
            .accept_piece(
                2,
                provenance(1),
                3,
                2,
                FRAG_FRAGMENTED,
                Bytes::from_static(b"overlap"),
                now(),
                &c
            )
            .is_none());
        assert_eq!(
            r.take_abandoned(),
            vec![Abandoned {
                scope: 2,
                stream_id: 4,
                reliable: true
            }]
        );
    }

    /// A stream's receive lifetime ending takes its partial groups
    /// with it, and only its own: another stream in the same session
    /// and the sequence-less control groups are untouched.
    #[test]
    fn retiring_one_stream_takes_only_its_own_groups() {
        let c = LeafCounters::new();
        let mut r = Reassembler::new();
        let elsewhere = PieceMeta {
            stream_id: 40,
            ..provenance(0)
        };
        for (fragment_id, meta) in [(3u16, provenance(0)), (4, elsewhere)] {
            assert!(r
                .accept_piece(
                    1,
                    meta,
                    fragment_id,
                    0,
                    FRAG_FRAGMENTED,
                    Bytes::from_static(b"head"),
                    now(),
                    &c
                )
                .is_none());
        }
        assert!(r
            .accept(
                1,
                5,
                0,
                FRAG_FRAGMENTED,
                Bytes::from_static(b"ctl"),
                now(),
                &c
            )
            .is_none());
        assert_eq!(r.outstanding(), 3);
        r.retire_stream(1, 4, &c);
        assert_eq!(r.outstanding(), 2, "only stream 4's group goes");
        assert_eq!(c.drops(DropReason::StreamFailed), 1);
        assert!(
            r.take_abandoned().is_empty(),
            "the reset is the disposition; retirement must not add a second one"
        );
        r.retire_stream(2, 4, &c);
        assert_eq!(r.outstanding(), 2, "another scope's stream 4 is not ours");
    }
}
