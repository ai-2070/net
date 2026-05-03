//! Reliability modes for Net streams.
//!
//! Net supports two reliability modes:
//! - Fire-and-forget: No acknowledgments, maximum throughput
//! - Reliable: Per-stream reliability with selective NACKs

use bytes::Bytes;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::protocol::{NackPayload, PacketFlags};

/// Pre-encryption inputs needed to rebuild a packet for
/// retransmission.
///
/// The reliable retransmit path used to stash the fully-encrypted
/// packet bytes, but every encrypted packet carries the cipher's
/// outer counter stamped at build time. Replaying those exact bytes
/// produces the same wire counter on the wire, which the receiver's
/// `update_rx_counter` rejects as a replay — making NACK-driven
/// recovery a no-op the first time it fired. Stashing the rebuild
/// inputs instead lets the retransmit driver call
/// `PacketBuilder::build` with a fresh counter on each retransmit,
/// so the receiver accepts the recovered packet.
#[derive(Debug, Clone)]
pub struct RetransmitDescriptor {
    /// Per-stream sequence number stamped on the packet header.
    pub seq: u64,
    /// Stream id for the rebuild call.
    pub stream_id: u64,
    /// Pre-encryption event payloads (the same `&[Bytes]` originally
    /// passed to `PacketBuilder::build`).
    pub events: Vec<Bytes>,
    /// Packet flags as stamped on the original send.
    pub flags: PacketFlags,
}

/// Trait for reliability mode implementations
pub trait ReliabilityMode: Send + Sync {
    /// Called when a packet is sent. The descriptor carries pre-
    /// encryption inputs so the retransmit path can rebuild a
    /// fresh-counter packet rather than replaying stale ciphertext.
    fn on_send(&mut self, descriptor: RetransmitDescriptor);

    /// Called when a packet is received. Returns true if accepted.
    fn on_receive(&mut self, seq: u64) -> bool;

    /// Check if this mode requires acknowledgments
    fn needs_ack(&self) -> bool;

    /// Build a NACK payload if there are missing sequences
    fn build_nack(&self) -> Option<NackPayload>;

    /// Process a received NACK and return descriptors for the
    /// caller to rebuild + dispatch.
    fn on_nack(&mut self, nack: &NackPayload) -> Vec<RetransmitDescriptor>;

    /// Get descriptors that need retransmission due to timeout.
    fn get_timed_out(&mut self) -> Vec<RetransmitDescriptor>;

    /// Check if there are unacknowledged packets
    fn has_pending(&self) -> bool;

    /// Get the name of this reliability mode
    fn name(&self) -> &'static str;
}

/// Fire-and-forget reliability mode.
///
/// No acknowledgments, no retransmission, maximum throughput.
/// Suitable for:
/// - LLM token streams
/// - Embeddings
/// - Intermediate activations
/// - Metrics/telemetry
#[derive(Debug, Default)]
pub struct FireAndForget {
    /// Last sequence received (for ordering check)
    last_seq: AtomicU64,
}

impl FireAndForget {
    /// Create a new fire-and-forget mode
    pub fn new() -> Self {
        Self::default()
    }
}

impl ReliabilityMode for FireAndForget {
    #[inline]
    fn on_send(&mut self, _descriptor: RetransmitDescriptor) {
        // Nothing to track
    }

    #[inline]
    fn on_receive(&mut self, seq: u64) -> bool {
        // Update last sequence (informational only)
        self.last_seq.fetch_max(seq, Ordering::Relaxed);
        true // Always accept
    }

    #[inline]
    fn needs_ack(&self) -> bool {
        false
    }

    #[inline]
    fn build_nack(&self) -> Option<NackPayload> {
        None
    }

    #[inline]
    fn on_nack(&mut self, _nack: &NackPayload) -> Vec<RetransmitDescriptor> {
        Vec::new()
    }

    #[inline]
    fn get_timed_out(&mut self) -> Vec<RetransmitDescriptor> {
        Vec::new()
    }

    #[inline]
    fn has_pending(&self) -> bool {
        false
    }

    #[inline]
    fn name(&self) -> &'static str {
        "fire-and-forget"
    }
}

/// Unacknowledged packet waiting for ACK/NACK
#[derive(Debug, Clone)]
struct UnackedPacket {
    /// Pre-encryption rebuild inputs. Stashing the descriptor (not
    /// the encrypted bytes) is what lets the retransmit path
    /// produce a fresh-counter packet on each NACK / timeout.
    descriptor: RetransmitDescriptor,
    /// Time when packet was sent
    sent_at: Instant,
    /// Number of retransmission attempts
    retries: u8,
}

impl UnackedPacket {
    #[inline]
    fn seq(&self) -> u64 {
        self.descriptor.seq
    }
}

/// Reliable stream mode with selective NACKs.
///
/// Features:
/// - Bounded retransmit window (32 packets)
/// - Selective NACKs (receiver-driven)
/// - Per-stream state
/// - Configurable RTO
///
/// Suitable for:
/// - Tool call results
/// - Guardrail decisions
/// - Session lifecycle events
/// - Error propagation
pub struct ReliableStream {
    /// The next sequence number we haven't yet received. All sequences
    /// `< next_expected` have been received contiguously. Starts at 0,
    /// expecting seq 0 as the first packet of the stream.
    ///
    /// Use `next_expected()` / `ack_seq()` accessors externally.
    next_expected: u64,
    /// SACK bitmap for out-of-order packets. Bit `i` is set iff sequence
    /// `next_expected + 1 + i` has been received. This represents up to
    /// 64 future sequences after the contiguous range. As `next_expected`
    /// advances, the bitmap is right-shifted so bit 0 always represents
    /// `next_expected + 1`.
    sack_bitmap: u64,
    /// Pending unacknowledged packets (bounded)
    pending: VecDeque<UnackedPacket>,
    /// Retransmit timeout
    rto: Duration,
    /// Maximum pending packets
    max_pending: usize,
    /// Maximum retries per packet
    max_retries: u8,
    /// Number of unacknowledged packets evicted from `pending` because
    /// the window was full when `on_send` arrived. The evicted packet
    /// went on the wire (the caller already issued the syscall) but
    /// is no longer tracked for retransmit — a NACK for that seq can
    /// no longer recover it. This counter surfaces the silent loss
    /// to the metrics layer so operators can size `max_pending` for
    /// their actual sustained reliable-stream throughput. Pre-fix
    /// the eviction was unobservable.
    untracked_evictions: u64,
}

impl ReliableStream {
    /// Default retransmit timeout
    pub const DEFAULT_RTO: Duration = Duration::from_millis(50);

    /// Default max pending packets
    pub const DEFAULT_MAX_PENDING: usize = 32;

    /// Default max retries
    pub const DEFAULT_MAX_RETRIES: u8 = 3;

    /// Create a new reliable stream with default settings
    pub fn new() -> Self {
        Self {
            next_expected: 0,
            sack_bitmap: 0,
            pending: VecDeque::with_capacity(Self::DEFAULT_MAX_PENDING),
            rto: Self::DEFAULT_RTO,
            max_pending: Self::DEFAULT_MAX_PENDING,
            max_retries: Self::DEFAULT_MAX_RETRIES,
            untracked_evictions: 0,
        }
    }

    /// Create with custom settings
    pub fn with_settings(rto: Duration, max_pending: usize, max_retries: u8) -> Self {
        Self {
            next_expected: 0,
            sack_bitmap: 0,
            pending: VecDeque::with_capacity(max_pending),
            rto,
            max_pending,
            max_retries,
            untracked_evictions: 0,
        }
    }

    /// Number of unacknowledged packets that the stream evicted from
    /// its retransmit window because the window was full at `on_send`
    /// time. Each eviction means the caller's syscall succeeded
    /// (bytes left this node) but the packet is no longer tracked
    /// for retransmit — a NACK can no longer recover it. A non-zero
    /// value indicates `max_pending` is undersized for the stream's
    /// sustained throughput. Operators should size up or apply
    /// upstream backpressure rather than accepting silent loss.
    #[inline]
    pub fn untracked_evictions(&self) -> u64 {
        self.untracked_evictions
    }

    /// Set the retransmit timeout
    pub fn set_rto(&mut self, rto: Duration) {
        self.rto = rto;
    }

    /// Lowest sequence number we have not yet received. All sequences
    /// strictly below this value are contiguously received.
    #[inline]
    pub fn next_expected(&self) -> u64 {
        self.next_expected
    }

    /// Highest contiguously-received sequence number, or `None` if no
    /// packets have been received yet.
    #[inline]
    pub fn last_received_contiguous(&self) -> Option<u64> {
        if self.next_expected == 0 {
            None
        } else {
            Some(self.next_expected - 1)
        }
    }

    /// Get the current ack sequence (highest contiguously-received seq).
    /// Returns 0 when nothing has been received yet — callers that need
    /// to distinguish "received seq 0" from "received nothing" should use
    /// [`Self::last_received_contiguous`] instead.
    pub fn ack_seq(&self) -> u64 {
        self.next_expected.saturating_sub(1)
    }

    /// Process an acknowledgment. `acked` is the highest sequence the
    /// peer has contiguously received.
    pub fn on_ack(&mut self, acked: u64) {
        // Remove all pending packets up to and including acked.
        while let Some(front) = self.pending.front() {
            if front.seq() <= acked {
                self.pending.pop_front();
            } else {
                break;
            }
        }
    }

    /// Check if there are gaps in received sequences.
    ///
    /// A gap exists whenever at least one future sequence has been
    /// received out of order — meaning `next_expected` itself is still
    /// pending (the implicit gap) and any interior missing seqs show
    /// up as zero bits in the SACK bitmap below the highest received.
    fn has_gaps(&self) -> bool {
        self.sack_bitmap != 0
    }

    /// Get bitmap of missing sequences after `next_expected`.
    ///
    /// Bit `i` set means sequence `next_expected + 1 + i` is missing.
    /// Sequence `next_expected` itself is always implicitly missing
    /// whenever `has_gaps()` returns true (that's what makes the NACK
    /// meaningful) — `missing_sequences()` on the resulting NACK emits
    /// `next_expected` first, then the bits of this bitmap.
    fn missing_bitmap(&self) -> u64 {
        // Invert sack_bitmap to get missing sequences; only consider
        // bits up to the highest received (otherwise we'd claim
        // sequences we've never heard of are "missing").
        if self.sack_bitmap == 0 {
            return 0;
        }
        let highest_bit = 63 - self.sack_bitmap.leading_zeros();
        let mask = if highest_bit >= 63 {
            u64::MAX
        } else {
            (1u64 << (highest_bit + 1)) - 1
        };
        (!self.sack_bitmap) & mask
    }
}

impl Default for ReliableStream {
    fn default() -> Self {
        Self::new()
    }
}

impl ReliabilityMode for ReliableStream {
    fn on_send(&mut self, descriptor: RetransmitDescriptor) {
        // Evict oldest unacked packet if window is full so that the
        // newest packet is always tracked for retransmission.  Without
        // this, packets sent when the window is full are silently lost
        // from the retransmit buffer even though they were sent on the
        // wire — a gap the receiver can never recover via NACK.
        //
        // Bump `untracked_evictions` on every eviction so the silent
        // loss surfaces via the `untracked_evictions()` accessor (and
        // any metrics layer hooked into it). Pre-fix the eviction was
        // unobservable: a `max_pending`-undersized stream looked
        // healthy from the sender side until NACKs started arriving
        // for sequences whose retransmit had already been dropped.
        if self.pending.len() >= self.max_pending {
            self.pending.pop_front();
            self.untracked_evictions = self.untracked_evictions.saturating_add(1);
            tracing::warn!(
                untracked_evictions = self.untracked_evictions,
                max_pending = self.max_pending,
                "ReliableStream: retransmit window full; evicted oldest \
                 unacked packet — NACK for that seq can no longer \
                 recover it. Increase max_pending or apply upstream \
                 backpressure.",
            );
        }
        self.pending.push_back(UnackedPacket {
            descriptor,
            sent_at: Instant::now(),
            retries: 0,
        });
    }

    fn on_receive(&mut self, seq: u64) -> bool {
        // Anything below next_expected has already been received
        // contiguously; reject as a duplicate.
        if seq < self.next_expected {
            return false;
        }
        if seq == self.next_expected {
            // Next expected sequence — advance the contiguous range,
            // then absorb any already-received future seqs that have
            // just become contiguous.
            //
            // Bitmap invariant (before this call): bit i is set iff
            // seq (old next_expected + 1 + i) has been received. After
            // incrementing next_expected by 1 (but BEFORE shifting),
            // bit 0 of the bitmap now refers to seq new_next_expected
            // itself — which, if set, means that seq was also received
            // out-of-order earlier and we can advance past it too.
            self.next_expected += 1;
            while self.sack_bitmap & 1 != 0 {
                self.next_expected += 1;
                self.sack_bitmap >>= 1;
            }
            // Restore the bitmap invariant: after the loop,
            // bit 0 of the bitmap still refers to seq `next_expected`
            // (not yet received; otherwise the loop would have
            // consumed it). The invariant wants bit 0 to refer to
            // seq `next_expected + 1`, so shift once more.
            self.sack_bitmap >>= 1;
            return true;
        }
        // seq > next_expected: future sequence.
        //
        // The bitmap can represent up to 64 future seqs past the
        // contiguous range. `offset` here is (seq - next_expected),
        // which is ≥ 1. Bit 0 of the bitmap represents
        // `next_expected + 1`, so the bit index is `offset - 1`.
        //
        // If the first packet of a stream arrives with seq > 0, this
        // branch records it without advancing next_expected, so
        // sequences `[0, seq)` remain flagged as missing in the
        // SACK bitmap — the receiver will request them via a NACK
        // instead of silently skipping them (which is what the old
        // code's `seq == ack_seq + 1` branch did, treating seq 0 as
        // already-acknowledged when the stream actually started with
        // a lost packet).
        let offset = seq - self.next_expected;
        if offset > 64 {
            return false;
        }
        let bit = offset - 1;
        let mask = 1u64 << bit;
        if self.sack_bitmap & mask != 0 {
            // Duplicate of a previously-recorded future seq.
            return false;
        }
        self.sack_bitmap |= mask;
        true
    }

    #[inline]
    fn needs_ack(&self) -> bool {
        true
    }

    fn build_nack(&self) -> Option<NackPayload> {
        if self.has_gaps() {
            Some(NackPayload {
                next_expected: self.next_expected,
                missing_bitmap: self.missing_bitmap(),
            })
        } else {
            None
        }
    }

    fn on_nack(&mut self, nack: &NackPayload) -> Vec<RetransmitDescriptor> {
        let mut retransmits = Vec::new();

        // Find packets to retransmit based on NACK. Return the
        // pre-encryption descriptors so the caller can rebuild
        // each packet with a fresh cipher counter — replaying the
        // stashed encrypted bytes would trip the receiver's replay
        // window.
        for missing_seq in nack.missing_sequences() {
            for unacked in &mut self.pending {
                if unacked.seq() == missing_seq && unacked.retries < self.max_retries {
                    retransmits.push(unacked.descriptor.clone());
                    unacked.retries += 1;
                    unacked.sent_at = Instant::now();
                    break;
                }
            }
        }

        retransmits
    }

    fn get_timed_out(&mut self) -> Vec<RetransmitDescriptor> {
        let now = Instant::now();
        let mut retransmits = Vec::new();

        for unacked in &mut self.pending {
            if now.duration_since(unacked.sent_at) > self.rto && unacked.retries < self.max_retries
            {
                retransmits.push(unacked.descriptor.clone());
                unacked.retries += 1;
                unacked.sent_at = now;
            }
        }

        retransmits
    }

    #[inline]
    fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    #[inline]
    fn name(&self) -> &'static str {
        "reliable"
    }
}

impl std::fmt::Debug for ReliableStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReliableStream")
            .field("next_expected", &self.next_expected)
            .field("sack_bitmap", &format!("{:064b}", self.sack_bitmap))
            .field("pending_count", &self.pending.len())
            .field("rto_ms", &self.rto.as_millis())
            .finish()
    }
}

/// Create a boxed reliability mode from configuration
pub fn create_reliability_mode(reliable: bool) -> Box<dyn ReliabilityMode> {
    if reliable {
        Box::new(ReliableStream::new())
    } else {
        Box::new(FireAndForget::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test helper: build a `RetransmitDescriptor` from the legacy
    /// `(seq, packet_bytes)` shape these tests were written against.
    /// Wraps the bytes as a single-event payload so the in-memory
    /// shape has something to round-trip through.
    fn descriptor(seq: u64, packet: Bytes) -> RetransmitDescriptor {
        RetransmitDescriptor {
            seq,
            stream_id: 0,
            events: vec![packet],
            flags: PacketFlags::RELIABLE,
        }
    }

    #[test]
    fn test_fire_and_forget() {
        let mut mode = FireAndForget::new();

        // Should always accept
        assert!(mode.on_receive(1));
        assert!(mode.on_receive(3)); // Gap is fine
        assert!(mode.on_receive(2)); // Out of order is fine

        // No acks needed
        assert!(!mode.needs_ack());
        assert!(mode.build_nack().is_none());
        assert!(!mode.has_pending());

        // No retransmits
        mode.on_send(descriptor(1, Bytes::from_static(b"test")));
        assert!(mode.get_timed_out().is_empty());
    }

    #[test]
    fn test_reliable_stream_in_order() {
        let mut mode = ReliableStream::new();

        // Receive in order starting from seq 0 (the sender always
        // begins at 0).
        assert!(mode.on_receive(0));
        assert_eq!(mode.ack_seq(), 0);
        assert_eq!(mode.last_received_contiguous(), Some(0));

        assert!(mode.on_receive(1));
        assert_eq!(mode.ack_seq(), 1);

        assert!(mode.on_receive(2));
        assert_eq!(mode.ack_seq(), 2);

        assert!(mode.on_receive(3));
        assert_eq!(mode.ack_seq(), 3);

        // No NACK needed
        assert!(mode.build_nack().is_none());
    }

    #[test]
    fn test_reliable_stream_gap() {
        let mut mode = ReliableStream::new();

        // Receive with gap (after an initial in-order seq 0 so the
        // gap is a real mid-stream hole, not a missing prefix).
        assert!(mode.on_receive(0));
        assert!(mode.on_receive(1));
        assert!(mode.on_receive(3)); // Gap at 2
        assert!(mode.on_receive(5)); // Gap at 4

        assert_eq!(mode.ack_seq(), 1);

        // Should have NACK
        let nack = mode.build_nack().unwrap();
        assert_eq!(nack.next_expected, 2);

        // Missing: 2 (the next expected — implicit), 4 (bitmap bit 1).
        let missing: Vec<_> = nack.missing_sequences().collect();
        assert!(missing.contains(&2));
        assert!(missing.contains(&4));
    }

    #[test]
    fn test_reliable_stream_fill_gap() {
        let mut mode = ReliableStream::new();

        // Receive out of order (with seq 0 so the gap is interior, not
        // a missing prefix).
        assert!(mode.on_receive(0));
        assert!(mode.on_receive(1));
        assert!(mode.on_receive(3));
        assert!(mode.on_receive(4));
        assert_eq!(mode.ack_seq(), 1);

        // Fill gap
        assert!(mode.on_receive(2));

        // Should advance
        assert_eq!(mode.ack_seq(), 4);

        // No NACK needed
        assert!(mode.build_nack().is_none());
    }

    #[test]
    fn test_reliable_stream_duplicate() {
        let mut mode = ReliableStream::new();

        assert!(mode.on_receive(0));
        assert!(mode.on_receive(1));
        assert!(mode.on_receive(2));

        // Duplicate should be rejected
        assert!(!mode.on_receive(1));
        assert!(!mode.on_receive(2));

        assert_eq!(mode.ack_seq(), 2);
    }

    #[test]
    fn test_reliable_stream_pending() {
        let mut mode = ReliableStream::new();

        assert!(!mode.has_pending());

        mode.on_send(descriptor(1, Bytes::from_static(b"packet1")));
        mode.on_send(descriptor(2, Bytes::from_static(b"packet2")));

        assert!(mode.has_pending());

        // ACK should clear pending
        mode.on_ack(2);
        assert!(!mode.has_pending());
    }

    #[test]
    fn test_reliable_stream_nack_retransmit() {
        let mut mode = ReliableStream::new();

        mode.on_send(descriptor(1, Bytes::from_static(b"packet1")));
        mode.on_send(descriptor(2, Bytes::from_static(b"packet2")));
        mode.on_send(descriptor(3, Bytes::from_static(b"packet3")));

        // NACK saying "received through seq 1, seq 2 is the next
        // expected (and therefore missing)".
        let nack = NackPayload {
            next_expected: 2,
            missing_bitmap: 0,
        };

        let retransmits = mode.on_nack(&nack);
        assert_eq!(retransmits.len(), 1);
        // The descriptor's first event is the (test-helper-built)
        // payload of the original send.
        assert_eq!(&retransmits[0].events[0][..], b"packet2");
        assert_eq!(retransmits[0].seq, 2);
    }

    #[test]
    fn test_reliable_stream_too_far_ahead() {
        let mut mode = ReliableStream::new();

        assert!(mode.on_receive(0));
        assert!(mode.on_receive(1));

        // Sequence 100 is too far ahead (beyond 64-bit window)
        assert!(!mode.on_receive(100));

        assert_eq!(mode.ack_seq(), 1);
    }

    #[test]
    fn test_reliable_stream_nack_bitmap_full_window() {
        // Regression: when the highest received bit was 63 (full 64-bit window),
        // 1u64 << 64 overflowed, panicking in debug or producing wrong results
        // in release.
        let mut mode = ReliableStream::new();

        // Receive packet 0, 1, then packet 65 (exactly 64 past `1`, at
        // the edge of the window).
        assert!(mode.on_receive(0));
        assert!(mode.on_receive(1));
        assert!(mode.on_receive(65));

        // build_nack should not panic and should report missing sequences
        let nack = mode.build_nack();
        assert!(
            nack.is_some(),
            "NACK should be generated for a gap spanning the full window"
        );

        let missing: Vec<_> = nack.unwrap().missing_sequences().collect();
        // Sequences 2..=64 are missing
        assert!(!missing.is_empty());
    }

    /// Regression: when `pending.len() >= max_pending`, `on_send`
    /// evicts the oldest unacked packet to make room for the new
    /// one. The evicted packet went on the wire but is no longer
    /// tracked for retransmit — a NACK can no longer recover it.
    /// Pre-fix the eviction was unobservable. The fix exposes a
    /// `untracked_evictions()` counter so a metrics layer can
    /// surface the silent loss to operators.
    #[test]
    fn reliable_stream_records_untracked_evictions_when_window_full() {
        const MAX_PENDING: usize = 4;
        let mut mode = ReliableStream::with_settings(Duration::from_millis(50), MAX_PENDING, 3);
        assert_eq!(mode.untracked_evictions(), 0);

        // Fill the window — no evictions yet.
        for seq in 0..(MAX_PENDING as u64) {
            mode.on_send(descriptor(seq, Bytes::from(format!("pkt-{seq}"))));
        }
        assert_eq!(mode.untracked_evictions(), 0);

        // The next 3 sends each force an eviction.
        for seq in (MAX_PENDING as u64)..(MAX_PENDING as u64 + 3) {
            mode.on_send(descriptor(seq, Bytes::from(format!("pkt-{seq}"))));
        }
        assert_eq!(
            mode.untracked_evictions(),
            3,
            "every on_send beyond max_pending must bump untracked_evictions",
        );

        // The evicted seqs (0, 1, 2) are no longer recoverable via
        // NACK — pin that behavior so a future change that quietly
        // re-orders eviction is caught. `missing_sequences()` yields
        // `[next_expected, next_expected+1+i for set bits]`, so
        // `next_expected: 0, missing_bitmap: 0b011` requests
        // [0, 1, 2] without spilling into the still-pending seq 3.
        let nack = NackPayload {
            next_expected: 0,
            missing_bitmap: 0b011,
        };
        let retransmits = mode.on_nack(&nack);
        assert!(
            retransmits.is_empty(),
            "evicted seqs must not produce retransmit descriptors, got {} entries",
            retransmits.len(),
        );
    }

    #[test]
    fn test_create_reliability_mode() {
        let mode = create_reliability_mode(false);
        assert_eq!(mode.name(), "fire-and-forget");

        let mode = create_reliability_mode(true);
        assert_eq!(mode.name(), "reliable");
    }

    #[test]
    fn test_reliable_stream_nack_retransmit_full_cycle() {
        // Full cycle: send packets, receive out of order with gaps,
        // build NACK, retransmit missing, fill gaps, verify ack_seq advances.
        let mut sender = ReliableStream::new();
        let mut receiver = ReliableStream::new();

        // Sender sends packets 0..10
        for seq in 0..10u64 {
            sender.on_send(descriptor(seq, Bytes::from(format!("pkt-{}", seq))));
        }
        assert!(sender.has_pending());

        // Receiver gets packets 0, 1, 3, 5, 6, 7, 9 (missing 2, 4, 8)
        assert!(receiver.on_receive(0));
        assert!(receiver.on_receive(1));
        assert!(receiver.on_receive(3)); // gap at 2
        assert!(receiver.on_receive(5)); // gap at 4
        assert!(receiver.on_receive(6));
        assert!(receiver.on_receive(7));
        assert!(receiver.on_receive(9)); // gap at 8

        assert_eq!(receiver.ack_seq(), 1); // contiguous through 1

        // Receiver builds NACK
        let nack = receiver.build_nack().expect("should have gaps");
        assert_eq!(nack.next_expected, 2);
        let missing: Vec<u64> = nack.missing_sequences().collect();
        assert!(missing.contains(&2), "should report seq 2 missing");
        assert!(missing.contains(&4), "should report seq 4 missing");
        assert!(missing.contains(&8), "should report seq 8 missing");

        // Sender processes NACK → retransmits missing packets
        let retransmits = sender.on_nack(&nack);
        assert_eq!(retransmits.len(), 3, "should retransmit 3 packets");

        // Receiver fills gaps
        assert!(receiver.on_receive(2));
        // After receiving 2: ack_seq should advance through 3, 5, 6, 7
        // Wait — 4 is still missing, so ack_seq advances to 3 then stops
        assert_eq!(
            receiver.ack_seq(),
            3,
            "should advance through contiguous 2,3"
        );

        assert!(receiver.on_receive(4));
        // Now 4 fills gap: ack_seq advances through 5, 6, 7
        assert_eq!(receiver.ack_seq(), 7, "should advance through 4,5,6,7");

        assert!(receiver.on_receive(8));
        // 8 fills gap: ack_seq advances through 9
        assert_eq!(receiver.ack_seq(), 9, "should advance through 8,9");

        // No more gaps
        assert!(
            receiver.build_nack().is_none(),
            "no gaps remaining after retransmit"
        );
    }

    #[test]
    fn test_reliable_stream_retransmit_timeout() {
        let mut mode = ReliableStream::with_settings(
            Duration::from_millis(50), // 50ms RTO — large enough to avoid CI jitter
            32,
            3,
        );

        mode.on_send(descriptor(0, Bytes::from_static(b"pkt-0")));
        mode.on_send(descriptor(1, Bytes::from_static(b"pkt-1")));

        // Nothing should time out yet (we just sent)
        let too_early = mode.get_timed_out();
        assert!(
            too_early.is_empty(),
            "packets should not time out before RTO"
        );

        // Wait well past RTO
        std::thread::sleep(Duration::from_millis(80));

        let timed_out = mode.get_timed_out();
        assert_eq!(timed_out.len(), 2, "both packets should time out");
        assert_eq!(&timed_out[0].events[0][..], b"pkt-0");
        assert_eq!(&timed_out[1].events[0][..], b"pkt-1");

        // Immediately after retransmit, sent_at was reset — shouldn't time out
        // again until another RTO elapses
        let again = mode.get_timed_out();
        assert!(
            again.is_empty(),
            "just retransmitted, shouldn't timeout yet"
        );
    }

    #[test]
    fn test_reliable_stream_max_retries_exhausted() {
        let mut mode = ReliableStream::with_settings(
            Duration::from_millis(50),
            32,
            2, // max 2 retries
        );

        mode.on_send(descriptor(0, Bytes::from_static(b"pkt-0")));

        // Exhaust retries (each iteration waits past RTO then triggers retransmit)
        for _ in 0..3 {
            std::thread::sleep(Duration::from_millis(80));
            let _ = mode.get_timed_out();
        }

        // After max_retries, the packet should no longer be retransmitted
        std::thread::sleep(Duration::from_millis(80));
        let timed_out = mode.get_timed_out();
        assert!(
            timed_out.is_empty(),
            "packet should stop being retransmitted after max_retries"
        );
    }

    #[test]
    fn test_regression_has_gaps_misses_interior_holes() {
        // Regression: has_gaps() used `trailing_zeros() > 0` which relied
        // on the subtle invariant that bit 0 of sack_bitmap is always 0
        // after on_receive returns. The old code was accidentally correct
        // but fragile — any refactor of on_receive could silently break
        // gap detection.
        //
        // Fix: has_gaps() now delegates to missing_bitmap() != 0, which
        // is correct by construction regardless of bitmap invariants.
        let mut mode = ReliableStream::new();

        // Receive 0, 1, 2, 4 — gap at 3
        assert!(mode.on_receive(0));
        assert!(mode.on_receive(1));
        assert!(mode.on_receive(2));
        assert!(mode.on_receive(4));

        assert_eq!(mode.ack_seq(), 2);

        let nack = mode.build_nack().unwrap();
        let missing: Vec<u64> = nack.missing_sequences().collect();
        assert!(missing.contains(&3), "should detect gap at seq 3");
    }

    #[test]
    fn test_regression_has_gaps_with_filled_first_slot() {
        // Verify has_gaps detects interior holes even when sequences
        // immediately after ack_seq are present.
        let mut mode = ReliableStream::new();

        // Receive 0, 1, 3, 5, 7 — gaps at 2, 4, 6
        assert!(mode.on_receive(0));
        assert!(mode.on_receive(1));
        assert!(mode.on_receive(3));
        assert!(mode.on_receive(5));
        assert!(mode.on_receive(7));

        assert_eq!(mode.ack_seq(), 1);

        let nack = mode.build_nack().expect("should detect gaps");
        let missing: Vec<u64> = nack.missing_sequences().collect();
        assert!(missing.contains(&2), "should detect gap at seq 2");
        assert!(missing.contains(&4), "should detect gap at seq 4");
        assert!(missing.contains(&6), "should detect gap at seq 6");
        // 4 entries: next_expected=2 (implicit), plus bits for 4 and 6.
        assert_eq!(missing.len(), 3);
    }

    #[test]
    fn test_regression_on_send_evicts_oldest_when_full() {
        // Regression: on_send silently dropped packets when the pending
        // queue was full. The packet was sent on the wire but never
        // recorded for retransmission, so if lost it could never be
        // recovered via NACK — silently degrading reliability.
        //
        // Fix: on_send now evicts the oldest unacked packet to make room,
        // so the most recent packets are always tracked.
        let mut mode = ReliableStream::with_settings(
            Duration::from_millis(50),
            4, // max 4 pending
            3,
        );

        // Send 6 packets (exceeds max_pending of 4)
        for seq in 0..6u64 {
            mode.on_send(descriptor(seq, Bytes::from(format!("pkt-{}", seq))));
        }

        // Should still have exactly max_pending packets tracked
        assert_eq!(
            mode.pending.len(),
            4,
            "pending queue should be at max_pending"
        );

        // The oldest packets (0, 1) should have been evicted;
        // the newest (2, 3, 4, 5) should be retained.
        let seqs: Vec<u64> = mode.pending.iter().map(|p| p.seq()).collect();
        assert_eq!(
            seqs,
            vec![2, 3, 4, 5],
            "should retain the most recent packets"
        );

        // NACK saying "seq 5 is the next expected (and therefore
        // missing)" — receiver is asking for the retransmit.
        let nack = NackPayload {
            next_expected: 5,
            missing_bitmap: 0,
        };
        let retransmits = mode.on_nack(&nack);
        assert_eq!(retransmits.len(), 1);
        assert_eq!(&retransmits[0].events[0][..], b"pkt-5");
        assert_eq!(retransmits[0].seq, 5);
    }

    #[test]
    fn test_regression_duplicate_seq_zero_rejected() {
        // Regression: on_receive had a special case for seq=0 that checked
        // `seq == 0 && self.ack_seq == 0`. After receiving seq 0, ack_seq
        // was still 0, so a duplicate seq 0 hit the same early return and
        // was accepted again — violating exactly-once delivery for reliable
        // streams.
        //
        // Fix: added `received_first` flag to distinguish "never received
        // anything" from "received seq 0".
        let mut mode = ReliableStream::new();

        // First reception of seq 0 should succeed
        assert!(mode.on_receive(0), "first seq 0 should be accepted");
        assert_eq!(mode.ack_seq(), 0);

        // Duplicate seq 0 should be rejected
        assert!(
            !mode.on_receive(0),
            "duplicate seq 0 must be rejected for exactly-once delivery"
        );

        // Normal continuation should still work
        assert!(mode.on_receive(1));
        assert_eq!(mode.ack_seq(), 1);
    }

    #[test]
    fn test_regression_seq_zero_after_higher_seqs_rejected() {
        // Regression: seq 0 arriving after ack_seq had advanced (e.g., to 5)
        // would pass the `seq == 0 && !received_first` check (false, so it
        // fell through) and then hit `seq <= self.ack_seq` → duplicate.
        // That path was correct, but an earlier version without received_first
        // would have reset ack_seq to 0, moving the window backwards.
        // This test ensures the fix holds.
        let mut mode = ReliableStream::new();

        // Receive 0..5 in order
        for seq in 0..=5 {
            assert!(mode.on_receive(seq));
        }
        assert_eq!(mode.ack_seq(), 5);

        // Late/replayed seq 0 must be rejected and must NOT move ack_seq backwards
        assert!(!mode.on_receive(0), "late seq 0 must be rejected");
        assert_eq!(mode.ack_seq(), 5, "ack_seq must not move backwards");
    }

    #[test]
    fn test_regression_first_received_seq_one_nacks_seq_zero() {
        // Regression (HIGH, BUGS.md): when the first received packet
        // on a reliable stream had seq > 0 (the real-world case where
        // seq 0 was lost in transit), the receiver silently advanced
        // `ack_seq` to that seq, claiming seq 0 had been acknowledged.
        // The sender's retransmit of seq 0 was then rejected as a
        // duplicate, and seq 0 was permanently lost to the application
        // — a reliability-contract violation.
        //
        // Fix: the receiver now leaves `next_expected` at 0 whenever
        // the first received seq is > 0, so the prefix gap is visible
        // to `build_nack()` and the retransmit of seq 0 is accepted
        // when it arrives.
        let mut mode = ReliableStream::new();

        // First received packet has seq 1 (seq 0 was lost in transit).
        assert!(mode.on_receive(1));
        // next_expected must stay at 0 — we haven't received seq 0.
        assert_eq!(mode.next_expected(), 0);
        assert_eq!(
            mode.last_received_contiguous(),
            None,
            "no contiguous prefix yet"
        );

        // A NACK must be generated reporting seq 0 as missing.
        let nack = mode.build_nack().expect("prefix gap must produce a NACK");
        assert_eq!(nack.next_expected, 0, "next_expected in NACK is 0");
        let missing: Vec<u64> = nack.missing_sequences().collect();
        assert!(
            missing.contains(&0),
            "NACK must report seq 0 as missing (was the lost first packet)"
        );

        // Retransmit of seq 0 must be accepted and advance the stream.
        assert!(
            mode.on_receive(0),
            "retransmit of seq 0 must be accepted after it was NACK'd"
        );
        // Now we have seq 0 and 1 contiguously; next_expected advances.
        assert_eq!(mode.next_expected(), 2);
        assert_eq!(mode.ack_seq(), 1);

        // No more gaps.
        assert!(
            mode.build_nack().is_none(),
            "no gaps after the retransmit filled the prefix"
        );
    }

    #[test]
    fn test_regression_first_received_large_seq_bounded_by_window() {
        // When the first received packet has a large seq (e.g. the
        // first 10 packets were all lost), the receiver can still
        // NACK up to the 64-bit bitmap window's worth of gaps. The
        // important property is that seq 0 is reported missing and
        // can be accepted on retransmit — not that *every* gap before
        // the first received seq fits in the bitmap.
        let mut mode = ReliableStream::new();

        // First received packet is seq 10 (0..9 all lost).
        assert!(mode.on_receive(10));
        assert_eq!(mode.next_expected(), 0);

        let nack = mode.build_nack().expect("prefix gap must produce a NACK");
        let missing: Vec<u64> = nack.missing_sequences().collect();
        // seq 0 is always reported as missing when any prefix gap exists.
        assert!(missing.contains(&0), "NACK must report seq 0 missing");
        // seq 1..9 also missing (within the 64-bit bitmap window).
        for expected in 1..=9 {
            assert!(
                missing.contains(&expected),
                "NACK must report seq {expected} missing"
            );
        }

        // Sender retransmits seq 0..9 in order.
        for seq in 0..10u64 {
            assert!(mode.on_receive(seq), "retransmit of seq {seq} accepted");
        }
        assert_eq!(mode.next_expected(), 11);
    }

    #[test]
    fn test_regression_first_received_duplicate_rejected() {
        // When seq 1 arrives first and is accepted (with seq 0 still
        // pending NACK), a subsequent duplicate of seq 1 must be
        // rejected — not double-counted in the bitmap.
        let mut mode = ReliableStream::new();

        assert!(mode.on_receive(1), "first seq 1 accepted");
        assert!(
            !mode.on_receive(1),
            "duplicate of seq 1 must be rejected for exactly-once delivery"
        );
        // State unchanged.
        assert_eq!(mode.next_expected(), 0);
    }

    /// Regression: the retransmit path now stashes pre-encryption
    /// rebuild inputs (`RetransmitDescriptor`), not encrypted bytes.
    /// Previously, `on_send` recorded the fully-encrypted packet
    /// `Bytes` and `on_nack` / `get_timed_out` returned those exact
    /// bytes. Replaying them produced the original wire counter on
    /// the wire, which the receiver's `update_rx_counter` rejects
    /// as a replay — making NACK-driven recovery dead-on-arrival.
    ///
    /// We pin the new shape: descriptors carry stream_id, seq,
    /// events, and flags; multiple retransmits of the same packet
    /// must yield the same descriptor (so the caller's
    /// re-`builder.build` produces a fresh-counter packet each
    /// time).
    #[test]
    fn retransmit_descriptors_carry_pre_encryption_inputs() {
        let mut mode = ReliableStream::with_settings(Duration::from_millis(20), 32, 5);

        // Send three packets with realistic descriptors (stream_id,
        // events list, flags).
        let events_a = vec![Bytes::from_static(b"event-A-payload")];
        let events_b = vec![Bytes::from_static(b"event-B-payload")];
        let events_c = vec![Bytes::from_static(b"event-C-payload")];
        mode.on_send(RetransmitDescriptor {
            seq: 0,
            stream_id: 7,
            events: events_a.clone(),
            flags: PacketFlags::RELIABLE,
        });
        mode.on_send(RetransmitDescriptor {
            seq: 1,
            stream_id: 7,
            events: events_b.clone(),
            flags: PacketFlags::RELIABLE,
        });
        mode.on_send(RetransmitDescriptor {
            seq: 2,
            stream_id: 7,
            events: events_c.clone(),
            flags: PacketFlags::RELIABLE,
        });

        // NACK seq=1.
        let nack = NackPayload {
            next_expected: 1,
            missing_bitmap: 0,
        };
        let retransmits = mode.on_nack(&nack);
        assert_eq!(retransmits.len(), 1);
        let r = &retransmits[0];
        assert_eq!(r.seq, 1);
        assert_eq!(r.stream_id, 7);
        assert_eq!(r.events, events_b);
        assert_eq!(r.flags, PacketFlags::RELIABLE);

        // The descriptor has the inputs needed for
        // `PacketBuilder::build(stream_id, seq, &events, flags)`.
        // Each retransmit lets the caller produce a fresh-counter
        // packet — distinct from the original even though the
        // descriptor itself is identical to what was originally
        // pushed. This is what fixes the replay-window rejection.
        let nack2 = NackPayload {
            next_expected: 1,
            missing_bitmap: 0,
        };
        let retransmits2 = mode.on_nack(&nack2);
        assert_eq!(retransmits2.len(), 1);
        let r2 = &retransmits2[0];
        // The descriptor is the same — the *cipher counter* freshness
        // is the responsibility of the rebuild caller, not of the
        // reliability layer.
        assert_eq!(r2.seq, r.seq);
        assert_eq!(r2.events, r.events);
        assert_eq!(r2.flags, r.flags);
        assert_eq!(r2.stream_id, r.stream_id);
    }
}
