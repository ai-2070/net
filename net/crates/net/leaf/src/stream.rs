//! Streams: reliable and fire-and-forget, with the **consumer-side
//! `seq` reorder** the Stage 3 witnesses established.
//!
//! The witnesses this implements against
//! (`net/crates/net/tests/rtc_loopback.rs`,
//! `net/crates/net/tests/rtc_repairs.rs`):
//!
//! - "the sequence numbers a single RTC stream delivers must be
//!   contiguous and ascending"
//!   (`rtc_ingress_preserves_per_source_order_through_one_owner`);
//! - "every payload must arrive, and reordered by seq they must be
//!   exactly what was sent" — the H5 assertion in
//!   `rtc_repairs.rs`, which is explicit that the reorder is the
//!   *consumer's* job. The sender's reliability layer retransmits;
//!   ordering is restored where the events are consumed.
//!
//! Two modes, and the difference is what happens to a gap:
//!
//! | | gap in the sequence | duplicate |
//! |---|---|---|
//! | [`Reliability::Reliable`] | hold later events until it fills | dropped, counted |
//! | [`Reliability::FireAndForget`] | **skip it** — deliver now | dropped, counted |
//!
//! Fire-and-forget that stalled waiting for a retransmit it will
//! never get would be reliable-with-extra-steps; dropping is the
//! contract, and every skipped sequence is counted
//! ([`DropReason::FireAndForgetGap`]) so loss is measurable rather
//! than invisible.
//!
//! The reliable buffer is **bounded**, and the bound is not a
//! licence to lose data. A peer that withholds one sequence forever
//! must not pin a browser tab's memory, so past
//! [`MAX_REORDER_HELD`] held sequences the stream **fails**, typed
//! ([`ReorderOverflow`], surfaced as
//! [`StreamFailure::ReorderOverflow`](crate::node::StreamFailure::ReorderOverflow)):
//! the consumer is told its reliable stream ended and which
//! sequence was never filled. Releasing past the hole and counting
//! it — what this did before — is silent loss on a stream whose
//! whole contract is that nothing is lost, and a counter is not a
//! disposition. Stalling forever, growing forever and lying are all
//! worse than a terminal failure.

use std::collections::BTreeMap;

use bytes::Bytes;

use crate::counters::{DropReason, LeafCounters};

/// Per-stream delivery mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reliability {
    /// Retransmitted by the sender; reordered and gap-filled by the
    /// consumer. Delivers in sequence order or not at all.
    Reliable,
    /// Never retransmitted, never held. A gap is a loss.
    FireAndForget,
}

impl Reliability {
    /// The wire flag: `PacketFlags::RELIABLE` iff reliable.
    #[inline]
    pub const fn is_reliable(self) -> bool {
        matches!(self, Self::Reliable)
    }

    /// Parse the SDK spelling. Accepts the camel-cased JS form and
    /// the snake_cased Rust one so a caller cannot pick the wrong
    /// half of the boundary.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "reliable" => Some(Self::Reliable),
            "fireAndForget" | "fire_and_forget" | "fire-and-forget" => Some(Self::FireAndForget),
            _ => None,
        }
    }
}

/// Held sequences before a reliable stream gives up on its head gap.
///
/// 64 sequences is two orders of magnitude above the reorder depth a
/// single SCTP DataChannel produces in practice (the Stage 3 witness
/// reorders a handful), and it bounds the hold at 64 packets' worth
/// of payload — ~507 KiB worst case, the same budget
/// [`crate::frame::MAX_OUTSTANDING_REASSEMBLIES`] spends.
pub const MAX_REORDER_HELD: usize = 64;

/// The bit-49 discriminator for a leaf-opened stream id.
///
/// `MeshNode::publish_stream_id` packs channel-keyed publisher
/// streams under bit 48 (`0x0001_0000_0000_0000`); leaf-opened
/// application streams take bit 49. Both are far above the low
/// subprotocol ids (`0x0400..0x1000`) that also ride as stream ids,
/// which IS guaranteed: setting a bit at 2^48 or 2^49 puts the
/// value out of that range unconditionally.
///
/// **What it does NOT guarantee**, stated because the obvious
/// reading is wrong: neither bit *reserves* a space. Both formulas
/// `OR` the bit into a full 64-bit `xxh3` hash, and roughly half of
/// all channel hashes already have the bit set — for those the `OR`
/// is a no-op and the id is indistinguishable from the raw hash
/// (`0xd1a8…` becomes `0xd1a9…`, one nibble). So a publisher stream
/// and a leaf stream CAN collide, at hash-collision probability
/// rather than never. Nothing is broken today because both ends
/// derive ids the same way and a collision would only mean two
/// logical streams sharing per-stream state on one session — but
/// the word "cannot" does not belong in this comment, and the
/// Stage 5 report carries it as a core-side latent hazard.
pub const LEAF_STREAM_DISCRIMINATOR: u64 = 0x0002_0000_0000_0000;

/// Derive a stable stream id from a caller-chosen label.
///
/// Same hash the channel layer uses, so two ends of a stream that
/// agree on the label agree on the id without exchanging it.
pub fn stream_id_from_label(label: &str) -> u64 {
    LEAF_STREAM_DISCRIMINATOR
        | (net_wire::channel::name::channel_hash(label) & 0x0000_FFFF_FFFF_FFFF)
}

/// The largest sequence jump a single packet may declare.
///
/// The sequence is a `u64` an authenticated peer chooses, and the
/// gap disposition is per-sequence bookkeeping. Without a ceiling a
/// one-byte packet claiming sequence `u64::MAX` asks the receive
/// path to account for 2^64 losses; with one, a jump past the
/// ceiling is refused and counted and the stream keeps its cursor.
/// 65 536 is three orders of magnitude above the reorder depth a
/// DataChannel produces and above any burst loss a retransmit
/// window would leave behind.
pub const MAX_SEQUENCE_GAP: u64 = 65_536;

/// One delivery and everything it is attributed to.
///
/// The reorder buffer holds these rather than bare bytes: a packet
/// released when a later arrival fills the gap keeps **its own**
/// sequence, origin, channel and subprotocol, instead of borrowing
/// the metadata of whichever packet happened to unblock it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamRecord {
    /// The sequence this delivery owns — the first of its span.
    pub seq: u64,
    /// How many stream sequences it consumes. One for an ordinary
    /// packet; a reassembled group consumes the sequences its
    /// fragments arrived on.
    pub span: u64,
    /// The stream it belongs to.
    pub stream_id: u64,
    /// The subprotocol that put it on this stream.
    ///
    /// Held with the record because dispatch is **deferred**: a
    /// publication held behind a membership frame is released by
    /// the membership frame's arrival, and dispatching the released
    /// record under the arriving packet's subprotocol would decode
    /// an application body as channel membership. One sequence
    /// space, one cursor, and every record keeps its own plane.
    pub subprotocol_id: u16,
    /// The mode the record's packet — or, for a reassembled group,
    /// its FIRST fragment — declared.
    ///
    /// Held with the record for the same reason the plane is: the
    /// mode decides whether a gap ahead of this record is held for
    /// recovery or skipped and counted, and taking it from
    /// whichever arrival completed a fragment group let a reliable
    /// group's head open its stream's cursor in fire-and-forget.
    pub reliable: bool,
    /// The publisher's full 64-bit origin hash.
    pub origin_hash: u64,
    /// The `u16` channel-hash hint from its header.
    pub channel_hash: u16,
    /// The events it carries.
    pub payloads: Vec<Bytes>,
}

/// A reliable stream whose reorder bound was exceeded with its head
/// gap still unfilled — terminal.
///
/// The alternative disposition, kept honest: a reliable stream may
/// **grow** its hold up to [`MAX_REORDER_HELD`] and it may
/// back-pressure its sender by withholding credit, but once the
/// bound is reached the two remaining choices are unbounded memory
/// or an ended stream. Delivering the records behind the hole is not
/// a third choice; it is reliable delivery with the reliability
/// removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReorderOverflow {
    /// The stream that ended.
    pub stream_id: u64,
    /// The sequence that was never filled — the head gap.
    pub missing: u64,
    /// How many records were being held when the bound went.
    pub held: usize,
}

/// The consumer side of one inbound stream.
#[derive(Debug)]
pub struct RxStream {
    reliability: Reliability,
    next_expected: u64,
    held: BTreeMap<u64, StreamRecord>,
}

impl RxStream {
    /// A stream whose first expected sequence is `0` — what
    /// `StreamState::next_tx_seq` produces first on the other side.
    pub fn new(reliability: Reliability) -> Self {
        Self {
            reliability,
            next_expected: 0,
            held: BTreeMap::new(),
        }
    }

    /// This stream's mode.
    #[inline]
    pub fn reliability(&self) -> Reliability {
        self.reliability
    }

    /// The next sequence that would be delivered in order.
    #[inline]
    pub fn next_expected(&self) -> u64 {
        self.next_expected
    }

    /// How many records are held awaiting a gap.
    #[inline]
    pub fn held(&self) -> usize {
        self.held.len()
    }

    /// Whether `seq` is already owned — delivered, or covered by the
    /// span of a record still held.
    fn already_owned(&self, seq: u64) -> bool {
        if seq < self.next_expected {
            return true;
        }
        self.held
            .range(..=seq)
            .next_back()
            .is_some_and(|(start, rec)| start.saturating_add(rec.span) > seq)
    }

    /// Make this stream reliable if it is not already, returning
    /// whether the mode was upgraded.
    ///
    /// **Reliability is decided by the traffic, not by whichever
    /// record touched the id first.** A channel's publish stream id
    /// is derived from the channel, so fire-and-forget and reliable
    /// publications to one channel share one id; creating the
    /// consumer's stream from the first record's mode meant a
    /// reliable record arriving after fire-and-forget traffic was
    /// released in ARRIVAL order, with its gaps counted as
    /// fire-and-forget loss. A reliable stream's contract is
    /// gap-free delivery in sequence order, so the stronger mode
    /// wins and never downgrades.
    ///
    /// The cursor needs no adjustment: the fire-and-forget half
    /// advanced `next_expected` past every sequence it skipped or
    /// delivered, which is exactly where the reliable half must
    /// resume — those sequences are unrecoverable (their sender
    /// retained nothing) and must not be waited for.
    pub fn ensure_reliable(&mut self) -> bool {
        if self.reliability.is_reliable() {
            return false;
        }
        self.reliability = Reliability::Reliable;
        true
    }

    /// Offer one delivery record.
    ///
    /// `Ok` carries what is now deliverable, **in sequence order** —
    /// zero entries when the record was held or dropped, more than
    /// one when it filled a gap and released everything behind it.
    /// Each returned record is the one that was buffered, carrying
    /// the sequence, header and subprotocol it arrived with.
    ///
    /// `Err` is terminal: a reliable stream held
    /// [`MAX_REORDER_HELD`] records and the head gap is still open,
    /// so the stream ends rather than silently release past it. The
    /// buffer is emptied with the error — the caller retires the
    /// stream, and nothing is delivered out of order on the way out.
    pub fn accept(
        &mut self,
        record: StreamRecord,
        counters: &LeafCounters,
    ) -> core::result::Result<Vec<StreamRecord>, ReorderOverflow> {
        let seq = record.seq;
        let stream_id = record.stream_id;
        // A reliable record makes the stream reliable, whatever
        // opened it — see `ensure_reliable`.
        if record.reliable {
            self.ensure_reliable();
        }
        if self.already_owned(seq) {
            counters.drop_for(DropReason::DuplicateSequence);
            return Ok(Vec::new());
        }
        if seq.saturating_sub(self.next_expected) > MAX_SEQUENCE_GAP {
            counters.drop_for(DropReason::SequenceGapTooLarge);
            return Ok(Vec::new());
        }

        if seq > self.next_expected && !self.reliability.is_reliable() {
            // Fire-and-forget: the gap is a loss, counted once per
            // skipped sequence but in one arithmetic step, and the
            // consumer is not stalled.
            counters.drop_n(DropReason::FireAndForgetGap, seq - self.next_expected);
            self.next_expected = seq;
        }

        self.held.insert(seq, record);
        if self.held.len() > MAX_REORDER_HELD {
            // Structurally a reliable-only outcome: a
            // fire-and-forget gap moves the cursor to the arriving
            // sequence above, so its drain always empties the
            // buffer and it holds nothing to overflow. Reaching
            // here means a reliable head gap outlasted the bound.
            let overflow = ReorderOverflow {
                stream_id,
                missing: self.next_expected,
                held: self.held.len(),
            };
            self.held.clear();
            return Err(overflow);
        }

        let mut out = Vec::new();
        while let Some(record) = self.held.remove(&self.next_expected) {
            self.next_expected = self.next_expected.saturating_add(record.span.max(1));
            out.push(record);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(seq: u64, tag: u8) -> StreamRecord {
        StreamRecord {
            seq,
            span: 1,
            stream_id: LEAF_STREAM_DISCRIMINATOR | 1,
            subprotocol_id: 0,
            origin_hash: 0xA1 + seq,
            channel_hash: tag as u16,
            payloads: vec![Bytes::from(vec![tag])],
            reliable: true,
        }
    }

    /// The same record as a FIRE-AND-FORGET arrival. A record's
    /// `reliable` flag is the packet's, and the stream's mode follows
    /// it (`RxStream::ensure_reliable`), so a fire-and-forget stream
    /// can only be fed fire-and-forget records.
    fn faf_rec(seq: u64, tag: u8) -> StreamRecord {
        StreamRecord {
            reliable: false,
            ..rec(seq, tag)
        }
    }

    /// Every accept below is on a stream that must not overflow;
    /// the one that must is asserted on directly.
    fn ok(out: core::result::Result<Vec<StreamRecord>, ReorderOverflow>) -> Vec<StreamRecord> {
        out.expect("this schedule must not overflow the reorder bound")
    }

    fn tags(records: &[StreamRecord]) -> Vec<u8> {
        records.iter().map(|r| r.payloads[0][0]).collect()
    }

    /// The H5 property from `rtc_repairs.rs`, at the unit level:
    /// every payload arrives, and reordered by seq they are exactly
    /// what was sent.
    #[test]
    fn a_reliable_stream_delivers_in_sequence_order_whatever_the_arrival_order() {
        let c = LeafCounters::new();
        let mut s = RxStream::new(Reliability::Reliable);

        assert!(
            ok(s.accept(rec(2, 2), &c)).is_empty(),
            "2 waits for 0 and 1"
        );
        assert!(ok(s.accept(rec(1, 1), &c)).is_empty(), "1 waits for 0");
        assert_eq!(s.held(), 2);

        let released = ok(s.accept(rec(0, 0), &c));
        assert_eq!(
            released
                .iter()
                .map(|r| (r.seq, r.payloads[0][0]))
                .collect::<Vec<_>>(),
            vec![(0, 0), (1, 1), (2, 2)],
            "each released record keeps its own sequence, not the gap-filling arrival's"
        );
        assert_eq!(
            released.iter().map(|r| r.origin_hash).collect::<Vec<_>>(),
            vec![0xA1, 0xA2, 0xA3],
            "and its own header provenance"
        );
        assert_eq!(s.held(), 0);
        assert_eq!(s.next_expected(), 3);
        assert_eq!(c.total_drops(), 0);
    }

    #[test]
    fn a_reliable_stream_holds_a_gap_rather_than_delivering_past_it() {
        let c = LeafCounters::new();
        let mut s = RxStream::new(Reliability::Reliable);
        assert_eq!(tags(&ok(s.accept(rec(0, 0), &c))), vec![0]);
        assert!(ok(s.accept(rec(2, 2), &c)).is_empty());
        assert!(ok(s.accept(rec(3, 3), &c)).is_empty());
        assert_eq!(s.next_expected(), 1, "the stream is waiting for 1");
        assert_eq!(tags(&ok(s.accept(rec(1, 1), &c))), vec![1, 2, 3]);
    }

    /// Fire-and-forget's whole point: a gap does not stall.
    #[test]
    fn fire_and_forget_skips_a_gap_and_counts_every_lost_sequence() {
        let c = LeafCounters::new();
        let mut s = RxStream::new(Reliability::FireAndForget);
        assert_eq!(tags(&ok(s.accept(faf_rec(0, 0), &c))), vec![0]);

        let out = ok(s.accept(faf_rec(4, 4), &c));
        assert_eq!(
            tags(&out),
            vec![4],
            "fire-and-forget must deliver immediately, not wait for 1..3"
        );
        assert_eq!(
            c.drops(DropReason::FireAndForgetGap),
            3,
            "sequences 1, 2 and 3 are losses and must each be counted"
        );
        assert_eq!(s.held(), 0, "fire-and-forget holds nothing");
        assert_eq!(s.next_expected(), 5);
    }

    #[test]
    fn a_late_arrival_after_a_fire_and_forget_skip_is_dropped_as_a_duplicate() {
        let c = LeafCounters::new();
        let mut s = RxStream::new(Reliability::FireAndForget);
        ok(s.accept(faf_rec(0, 0), &c));
        ok(s.accept(faf_rec(4, 4), &c));
        assert!(
            ok(s.accept(faf_rec(2, 2), &c)).is_empty(),
            "a sequence the stream already skipped past cannot be delivered"
        );
        assert_eq!(c.drops(DropReason::DuplicateSequence), 1);
    }

    #[test]
    fn a_retransmitted_duplicate_is_delivered_once() {
        let c = LeafCounters::new();
        let mut s = RxStream::new(Reliability::Reliable);
        assert_eq!(tags(&ok(s.accept(rec(0, 0), &c))), vec![0]);
        assert!(
            ok(s.accept(rec(0, 0), &c)).is_empty(),
            "the same sequence must never be delivered twice"
        );
        assert_eq!(c.drops(DropReason::DuplicateSequence), 1);

        // A duplicate of a HELD (not yet delivered) sequence too.
        assert!(ok(s.accept(rec(2, 2), &c)).is_empty());
        assert!(ok(s.accept(rec(2, 2), &c)).is_empty());
        assert_eq!(c.drops(DropReason::DuplicateSequence), 2);
    }

    /// The bound: hold, but not forever — and not by releasing past
    /// the hole either. A reliable stream that reaches the bound
    /// **ends**, and says which sequence it never got.
    #[test]
    fn a_reliable_stream_fails_typed_at_the_buffer_bound_rather_than_losing_its_head_gap() {
        let c = LeafCounters::new();
        let mut s = RxStream::new(Reliability::Reliable);
        // Sequence 0 never arrives; 1..=MAX_REORDER_HELD do.
        for seq in 1..=MAX_REORDER_HELD as u64 {
            assert!(
                ok(s.accept(rec(seq, 1), &c)).is_empty(),
                "seq {seq} must hold"
            );
        }
        assert_eq!(s.held(), MAX_REORDER_HELD);
        assert_eq!(s.next_expected(), 0, "still waiting for 0");
        assert_eq!(c.total_drops(), 0);

        // One more, and the stream is over. Nothing behind the hole
        // is delivered: that would be silent loss on a reliable
        // stream, which is what a counter used to paper over.
        let overflow = s
            .accept(rec(MAX_REORDER_HELD as u64 + 1, 2), &c)
            .expect_err("exceeding the bound with an open head gap must be terminal");
        assert_eq!(
            overflow,
            ReorderOverflow {
                stream_id: LEAF_STREAM_DISCRIMINATOR | 1,
                missing: 0,
                held: MAX_REORDER_HELD + 1,
            },
            "the failure must name the sequence that was never filled"
        );
        assert_eq!(s.held(), 0, "the buffer is released with the failure");
    }

    #[test]
    fn reliability_parses_both_spellings_and_rejects_anything_else() {
        assert_eq!(Reliability::parse("reliable"), Some(Reliability::Reliable));
        assert_eq!(
            Reliability::parse("fireAndForget"),
            Some(Reliability::FireAndForget)
        );
        assert_eq!(
            Reliability::parse("fire_and_forget"),
            Some(Reliability::FireAndForget)
        );
        assert_eq!(Reliability::parse("best-effort"), None);
    }

    #[test]
    fn leaf_stream_ids_cannot_alias_the_publish_or_subprotocol_spaces() {
        let id = stream_id_from_label("app/telemetry");
        assert_eq!(id & LEAF_STREAM_DISCRIMINATOR, LEAF_STREAM_DISCRIMINATOR);
        // Bit 48 — the native publisher-stream discriminator — is
        // MASKED OFF, unconditionally. That is what lets a receiver
        // tell a channel publication from a leaf stream by the
        // namespace rather than by one bit of a hash that both
        // formulas leave to chance.
        assert_eq!(
            id & crate::channel::PUBLISH_STREAM_DISCRIMINATOR,
            0,
            "a leaf stream id must never carry the publish discriminator"
        );
        assert!(
            id > 0x1_0000,
            "a leaf stream id must never land in the subprotocol id range"
        );
        assert_eq!(
            id,
            stream_id_from_label("app/telemetry"),
            "the derivation must be stable — both ends derive it independently"
        );
        assert_ne!(id, stream_id_from_label("app/other"));
    }
}
