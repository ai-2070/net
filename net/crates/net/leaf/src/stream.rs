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
//! The reliable buffer is **bounded**. A peer that withholds one
//! sequence forever must not pin a browser tab's memory, so at
//! [`MAX_REORDER_HELD`] held sequences the head gap is abandoned:
//! the buffer releases from the lowest held sequence onwards and
//! counts [`DropReason::ReorderBufferFull`]. Stalling forever and
//! growing forever are both worse.

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
/// application streams take bit 49 so the two spaces cannot alias,
/// and neither can collide with the low subprotocol ids
/// (`0x0400..0x1000`) that ride as stream ids.
pub const LEAF_STREAM_DISCRIMINATOR: u64 = 0x0002_0000_0000_0000;

/// Derive a stable stream id from a caller-chosen label.
///
/// Same hash the channel layer uses, so two ends of a stream that
/// agree on the label agree on the id without exchanging it.
pub fn stream_id_from_label(label: &str) -> u64 {
    LEAF_STREAM_DISCRIMINATOR
        | (net_wire::channel::name::channel_hash(label) & 0x0000_FFFF_FFFF_FFFF)
}

/// The consumer side of one inbound stream.
#[derive(Debug)]
pub struct RxStream {
    reliability: Reliability,
    next_expected: u64,
    held: BTreeMap<u64, Vec<Bytes>>,
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

    /// How many sequences are held awaiting a gap.
    #[inline]
    pub fn held(&self) -> usize {
        self.held.len()
    }

    /// Offer one packet's events at sequence `seq`.
    ///
    /// Returns what is now deliverable, **in sequence order** — zero
    /// entries when the packet was held or dropped, more than one
    /// when it filled a gap and released everything behind it.
    pub fn accept(
        &mut self,
        seq: u64,
        events: Vec<Bytes>,
        counters: &LeafCounters,
    ) -> Vec<Vec<Bytes>> {
        if seq < self.next_expected || self.held.contains_key(&seq) {
            counters.drop_for(DropReason::DuplicateSequence);
            return Vec::new();
        }

        if seq > self.next_expected && !self.reliability.is_reliable() {
            // Fire-and-forget: the gap is a loss, counted per
            // skipped sequence, and the consumer is not stalled.
            for _ in self.next_expected..seq {
                counters.drop_for(DropReason::FireAndForgetGap);
            }
            self.next_expected = seq;
        }

        self.held.insert(seq, events);
        if self.held.len() > MAX_REORDER_HELD {
            // Abandon the head gap: release from the lowest held
            // sequence rather than hold unboundedly.
            if let Some(lowest) = self.held.keys().next().copied() {
                for _ in self.next_expected..lowest {
                    counters.drop_for(DropReason::ReorderBufferFull);
                }
                self.next_expected = lowest;
            }
        }

        let mut out = Vec::new();
        while let Some(events) = self.held.remove(&self.next_expected) {
            out.push(events);
            self.next_expected += 1;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(tag: u8) -> Vec<Bytes> {
        vec![Bytes::from(vec![tag])]
    }

    fn tags(batches: &[Vec<Bytes>]) -> Vec<u8> {
        batches.iter().map(|b| b[0][0]).collect()
    }

    /// The H5 property from `rtc_repairs.rs`, at the unit level:
    /// every payload arrives, and reordered by seq they are exactly
    /// what was sent.
    #[test]
    fn a_reliable_stream_delivers_in_sequence_order_whatever_the_arrival_order() {
        let c = LeafCounters::new();
        let mut s = RxStream::new(Reliability::Reliable);

        assert!(s.accept(2, ev(2), &c).is_empty(), "2 waits for 0 and 1");
        assert!(s.accept(1, ev(1), &c).is_empty(), "1 waits for 0");
        assert_eq!(s.held(), 2);

        let released = s.accept(0, ev(0), &c);
        assert_eq!(
            tags(&released),
            vec![0, 1, 2],
            "filling the head gap must release everything behind it, in order"
        );
        assert_eq!(s.held(), 0);
        assert_eq!(s.next_expected(), 3);
        assert_eq!(c.total_drops(), 0);
    }

    #[test]
    fn a_reliable_stream_holds_a_gap_rather_than_delivering_past_it() {
        let c = LeafCounters::new();
        let mut s = RxStream::new(Reliability::Reliable);
        assert_eq!(tags(&s.accept(0, ev(0), &c)), vec![0]);
        assert!(s.accept(2, ev(2), &c).is_empty());
        assert!(s.accept(3, ev(3), &c).is_empty());
        assert_eq!(s.next_expected(), 1, "the stream is waiting for 1");
        assert_eq!(tags(&s.accept(1, ev(1), &c)), vec![1, 2, 3]);
    }

    /// Fire-and-forget's whole point: a gap does not stall.
    #[test]
    fn fire_and_forget_skips_a_gap_and_counts_every_lost_sequence() {
        let c = LeafCounters::new();
        let mut s = RxStream::new(Reliability::FireAndForget);
        assert_eq!(tags(&s.accept(0, ev(0), &c)), vec![0]);

        let out = s.accept(4, ev(4), &c);
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
        s.accept(0, ev(0), &c);
        s.accept(4, ev(4), &c);
        assert!(
            s.accept(2, ev(2), &c).is_empty(),
            "a sequence the stream already skipped past cannot be delivered"
        );
        assert_eq!(c.drops(DropReason::DuplicateSequence), 1);
    }

    #[test]
    fn a_retransmitted_duplicate_is_delivered_once() {
        let c = LeafCounters::new();
        let mut s = RxStream::new(Reliability::Reliable);
        assert_eq!(tags(&s.accept(0, ev(0), &c)), vec![0]);
        assert!(
            s.accept(0, ev(0), &c).is_empty(),
            "the same sequence must never be delivered twice"
        );
        assert_eq!(c.drops(DropReason::DuplicateSequence), 1);

        // A duplicate of a HELD (not yet delivered) sequence too.
        assert!(s.accept(2, ev(2), &c).is_empty());
        assert!(s.accept(2, ev(2), &c).is_empty());
        assert_eq!(c.drops(DropReason::DuplicateSequence), 2);
    }

    /// The bound: hold, but not forever.
    #[test]
    fn a_reliable_stream_abandons_its_head_gap_at_the_buffer_bound() {
        let c = LeafCounters::new();
        let mut s = RxStream::new(Reliability::Reliable);
        // Sequence 0 never arrives; 1..=MAX_REORDER_HELD do.
        for seq in 1..=MAX_REORDER_HELD as u64 {
            assert!(s.accept(seq, ev(1), &c).is_empty(), "seq {seq} must hold");
        }
        assert_eq!(s.held(), MAX_REORDER_HELD);
        assert_eq!(s.next_expected(), 0, "still waiting for 0");
        assert_eq!(c.total_drops(), 0);

        // One more, and the head gap is abandoned.
        let released = s.accept(MAX_REORDER_HELD as u64 + 1, ev(2), &c);
        assert_eq!(
            released.len(),
            MAX_REORDER_HELD + 1,
            "abandoning the gap must release the whole buffer"
        );
        assert_eq!(
            c.drops(DropReason::ReorderBufferFull),
            1,
            "exactly the one abandoned sequence (0) is counted"
        );
        assert_eq!(s.held(), 0);
        assert_eq!(s.next_expected(), MAX_REORDER_HELD as u64 + 2);
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
        assert_eq!(
            id & 0x0001_0000_0000_0000,
            0,
            "must not collide with MeshNode::publish_stream_id's bit 48"
        );
        assert!(id > 0x1_0000, "must not land in the subprotocol id range");
        assert_eq!(
            id,
            stream_id_from_label("app/telemetry"),
            "the derivation must be stable — both ends derive it independently"
        );
        assert_ne!(id, stream_id_from_label("app/other"));
    }
}
