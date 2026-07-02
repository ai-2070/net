//! Reliability modes for Net streams.
//!
//! Net supports two reliability modes:
//! - Fire-and-forget: No acknowledgments, maximum throughput
//! - Reliable: Per-stream reliability with selective NACKs

use bytes::Bytes;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
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

/// Trait for reliability mode implementations.
///
/// Per crypto-session perf #133, the descriptor is exchanged as
/// `Arc<RetransmitDescriptor>` across the trait boundary. The
/// `RetransmitDescriptor` itself carries an inner
/// `Vec<Bytes>` of pre-encryption event payloads — at `max_pending =
/// 32` and ~10 events per packet that's ~320 `Bytes` refcounts
/// dangling off the retransmit window at any given time. Pre-fix
/// `on_send` moved the descriptor in by value (one Vec spine + one
/// refcount bump per inner `Bytes`), and `on_nack` /
/// `get_timed_out` deep-cloned the descriptor per retransmit (one
/// Vec alloc + N `Bytes` refcount bumps per emission). Wrapping in
/// `Arc` makes both paths one atomic refcount bump regardless of
/// the inner Vec's length.
pub trait ReliabilityMode: Send + Sync {
    /// Called when a packet is sent. The descriptor carries pre-
    /// encryption inputs so the retransmit path can rebuild a
    /// fresh-counter packet rather than replaying stale ciphertext.
    fn on_send(&mut self, descriptor: Arc<RetransmitDescriptor>);

    /// Called when a packet is received. Returns true if accepted.
    fn on_receive(&mut self, seq: u64) -> bool;

    /// Check if this mode requires acknowledgments
    fn needs_ack(&self) -> bool;

    /// Build a NACK payload if there are missing sequences
    fn build_nack(&self) -> Option<NackPayload>;

    /// Process a received NACK and return descriptors for the
    /// caller to rebuild + dispatch. The returned `Arc` clones
    /// share the inner `RetransmitDescriptor` allocation; the
    /// caller bumps the refcount instead of deep-cloning the
    /// `Vec<Bytes>` of events.
    fn on_nack(&mut self, nack: &NackPayload) -> Vec<Arc<RetransmitDescriptor>>;

    /// Get descriptors that need retransmission due to timeout. See
    /// [`Self::on_nack`] for the `Arc`-sharing contract.
    fn get_timed_out(&mut self) -> Vec<Arc<RetransmitDescriptor>>;

    /// Take-and-clear the "stream has given up" flag (H-3): `true` once
    /// after a packet exhausts `max_retries` while still unacked — the
    /// reliable layer can no longer recover that gap, so the caller
    /// should signal a stream reset to the peer rather than let it stall
    /// to a higher-level timeout. Default `false` (fire-and-forget never
    /// gives up — it tracks nothing).
    fn take_failed(&mut self) -> bool {
        false
    }

    /// The receiver's cumulative ack — the lowest sequence not yet
    /// contiguously received (`next_expected`). The peer piggybacks this
    /// on its window grants so the sender can prune (H-9). Default 0
    /// (fire-and-forget tracks no sequence).
    fn rx_ack_seq(&self) -> u64 {
        0
    }

    /// Sender-side: a cumulative ack arrived — every sequence below
    /// `ack_seq` has been received, so drop them from the retransmit
    /// window (H-9). Without this, packets linger in `pending` on the
    /// happy path until they spuriously time out and get resent (and,
    /// post-H-3, spuriously give up). Default no-op.
    fn on_ack(&mut self, _ack_seq: u64) {}

    /// Sender-side: a positive SACK-range ack arrived (R-3).
    /// `ack_seq` carries the cumulative ack (applied exactly like
    /// [`Self::on_ack`]); `ranges` are half-open `[start, end)`
    /// received runs strictly above it. SACKed packets are REMOVED
    /// from the retransmit window — the receiver has them, so they
    /// leave in-flight accounting and are never retransmit-eligible
    /// again. This is what stops one lost head packet from RTO-
    /// flooding every tracked packet behind it. Default no-op
    /// (fire-and-forget tracks nothing).
    fn on_ack_ranges(&mut self, _ack_seq: u64, _ranges: &[(u64, u64)]) {}

    /// Receiver-side: the current out-of-order received runs above
    /// [`Self::rx_ack_seq`], newest-first (descending by end), at
    /// most `max_ranges` entries — the payload of an outgoing
    /// `StreamAckRanges` (R-4). Default empty (fire-and-forget
    /// tracks nothing; a gapless reliable stream has none).
    fn build_ack_ranges(&self, _max_ranges: usize) -> Vec<(u64, u64)> {
        Vec::new()
    }

    /// Whether the sender may put another packet in flight under its
    /// congestion window (H-6). Default `true` (fire-and-forget has no
    /// congestion state). A reliable stream returns `false` once
    /// in-flight reaches its cwnd, so the send path back-pressures and
    /// paces to the cwnd under loss.
    fn can_send(&self) -> bool {
        true
    }

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
    fn on_send(&mut self, _descriptor: Arc<RetransmitDescriptor>) {
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
    fn on_nack(&mut self, _nack: &NackPayload) -> Vec<Arc<RetransmitDescriptor>> {
        Vec::new()
    }

    #[inline]
    fn get_timed_out(&mut self) -> Vec<Arc<RetransmitDescriptor>> {
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
    ///
    /// Per crypto-session perf #133, the descriptor is held behind
    /// an `Arc` so that the retransmit emissions (`on_nack` /
    /// `get_timed_out`) clone the refcount instead of deep-cloning
    /// the inner `Vec<Bytes>` events list.
    descriptor: Arc<RetransmitDescriptor>,
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
    /// Received-range index for out-of-order arrivals above
    /// `next_expected` (R-2): half-open `[start, end)` runs, ASCENDING
    /// by start, fully merged (non-overlapping, non-adjacent; every
    /// start > `next_expected`). Replaces the v1 64-bit SACK bitmap,
    /// whose fixed horizon turned one lost head packet into a
    /// 64-packet in-flight ceiling. This is an INDEX, not a payload
    /// buffer — per H-8, payloads are pushed to consumers in arrival
    /// order and reassembled by seq — so memory is
    /// O([`Self::MAX_REORDER_RANGES`]).
    received_ranges: VecDeque<(u64, u64)>,
    /// Out-of-order arrivals accepted into the range index (R-2).
    oo_accepted: u64,
    /// Arrivals rejected past the reorder horizon (R-2).
    oo_dropped_horizon: u64,
    /// Arrivals rejected because the range index was at
    /// [`Self::MAX_REORDER_RANGES`] capacity (R-2).
    oo_dropped_capacity: u64,
    /// Newest SACK view received via
    /// [`ReliabilityMode::on_ack_ranges`], kept (≤ 16 entries, one
    /// message's worth) for the NACK-vs-SACK contradiction check —
    /// best-effort observability, last message wins (R-3).
    last_sacked: Vec<(u64, u64)>,
    /// NACK-vs-SACK contradictions observed: the peer NACKed a seq it
    /// had positively acknowledged. Positive ack wins — the packet is
    /// already out of the window — so this only counts (R-3).
    protocol_anomalies: u64,
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
    /// Set when a packet exhausts `max_retries` while still unacked
    /// (H-3): the reliable layer has given up on that gap. Taken-and-
    /// cleared via [`Self::take_failed`] so the owning node can signal a
    /// stream reset to the peer rather than let it stall to a timeout.
    failed: bool,
    /// Smoothed RTT estimate (RFC 6298, α=1/8); `None` until the first
    /// RTT sample. Drives the adaptive `rto` (H-5).
    srtt: Option<Duration>,
    /// RTT variance estimate (RFC 6298, β=1/4).
    rttvar: Duration,
    /// Congestion window in packets (H-6, Reno-style AIMD): the cap on
    /// in-flight (unacked) packets. Grows on clean acks (slow-start then
    /// congestion-avoidance), halves on a NACK-driven loss, and resets to
    /// the floor on a timeout. On a loss-free path it just grows past the
    /// retransmit window and never gates.
    cwnd: f64,
    /// Slow-start threshold — above it, growth switches from slow-start
    /// (+1/ack) to congestion-avoidance (+1/cwnd per ack).
    ssthresh: f64,
    /// Fast-recovery "recover point" (T-1): the `next_expected` of the
    /// loss episode currently being recovered. `Some(r)` means we've
    /// already reacted to the gap at `r` (resent + one cwnd cut) and are
    /// awaiting its repair; further NACKs at `next_expected <= r` are
    /// duplicates (the receiver re-NACKs every tick, faster than one RTT
    /// on a high-RTT link) and must be ignored so they don't re-resend,
    /// re-halve cwnd, or bump `retries` toward a spurious give-up.
    /// Cleared (`None`) once a cumulative ack advances past `r`.
    recover: Option<u64>,
}

impl ReliableStream {
    /// Default retransmit timeout — the starting RTO before any RTT
    /// sample, and the value used by fixed-RTO callers.
    pub const DEFAULT_RTO: Duration = Duration::from_millis(50);

    /// Lower bound on the adaptive RTO (H-5). Floors the estimate so a
    /// near-zero localhost RTT can't drive the RTO below the grant-drain
    /// + processing latency and cause spurious resends.
    pub const MIN_RTO: Duration = Duration::from_millis(10);

    /// Upper bound on the adaptive RTO (H-5). Caps how long a genuinely
    /// lost packet waits before the timeout backstop resends it.
    pub const MAX_RTO: Duration = Duration::from_secs(2);

    /// Default max pending packets — also the floor when the window is
    /// auto-sized from a stream's tx-window (see
    /// [`Self::max_pending_for_window`]).
    pub const DEFAULT_MAX_PENDING: usize = 32;

    /// Lower bound on the per-packet size assumed when sizing the
    /// retransmit window from a tx-window. The window must track at least
    /// `tx_window / MIN_TRACKED_PACKET_BYTES` packets so nothing in flight
    /// is evicted before it can be retransmitted. 512 B covers bulk (MTU)
    /// and typical control packets; sub-512 B spam on a huge window is the
    /// only residual eviction risk — surfaced loudly via
    /// [`Self::untracked_evictions`] (H-2).
    pub const MIN_TRACKED_PACKET_BYTES: u32 = 512;

    /// Hard cap on the auto-sized retransmit window, bounding the pending
    /// queue's worst-case growth under sustained loss.
    pub const MAX_RETRANSMIT_WINDOW: usize = 16_384;

    /// Default max retries
    pub const DEFAULT_MAX_RETRIES: u8 = 3;

    /// Cap on tracked out-of-order received ranges (R-2). An insert
    /// that would create a fresh range beyond this cap is rejected
    /// (arrival dropped + counted in
    /// [`Self::out_of_order_dropped_capacity`]), bounding both index
    /// memory and the per-packet merge cost. 32 disjoint holes in one
    /// window is pathological sustained reordering; the sender's RTO
    /// recovers whatever a reject drops.
    pub const MAX_REORDER_RANGES: usize = 32;

    /// Cap on the reorder-acceptance horizon in packets (R-2). Equal
    /// to the retransmit-window cap — accepting further ahead than
    /// the sender can even track for retransmit is pointless.
    pub const MAX_REORDER_PACKETS: u64 = Self::MAX_RETRANSMIT_WINDOW as u64;

    /// Initial congestion window in packets (H-6). ~TCP initial window;
    /// big enough that low-volume reliable streams (nRPC) never feel it.
    pub const INIT_CWND: f64 = 32.0;

    /// Congestion-window floor — a stream under sustained loss still
    /// makes forward progress at this many packets in flight.
    pub const MIN_CWND: f64 = 2.0;

    /// Size the retransmit window to a stream's tx-credit window so the
    /// sender can never have more packets in flight than it can
    /// retransmit (the H-1 invariant: tx-window ≤ retransmit-window).
    /// `tx_window == 0` (backpressure disabled) falls back to the default.
    pub fn max_pending_for_window(tx_window: u32) -> usize {
        if tx_window == 0 {
            return Self::DEFAULT_MAX_PENDING;
        }
        // DEFAULT_MAX_PENDING (32) <= MAX_RETRANSMIT_WINDOW (16384), so the
        // clamp bounds are well-ordered.
        ((tx_window / Self::MIN_TRACKED_PACKET_BYTES) as usize)
            .clamp(Self::DEFAULT_MAX_PENDING, Self::MAX_RETRANSMIT_WINDOW)
    }

    /// Create a new reliable stream with default settings.
    ///
    /// `pending` is NOT pre-reserved: it grows on demand to the actual
    /// in-flight count (itself bounded by the tx-window's bytes), so a
    /// generous `max_pending` costs nothing up front — important when
    /// thousands of small-payload streams each carry a reliability state.
    pub fn new() -> Self {
        Self::with_settings(
            Self::DEFAULT_RTO,
            Self::DEFAULT_MAX_PENDING,
            Self::DEFAULT_MAX_RETRIES,
        )
    }

    /// Create with custom settings. `pending` grows on demand (no
    /// pre-reservation) — see [`Self::new`].
    pub fn with_settings(rto: Duration, max_pending: usize, max_retries: u8) -> Self {
        Self {
            next_expected: 0,
            received_ranges: VecDeque::new(),
            oo_accepted: 0,
            oo_dropped_horizon: 0,
            oo_dropped_capacity: 0,
            last_sacked: Vec::new(),
            protocol_anomalies: 0,
            pending: VecDeque::new(),
            rto,
            max_pending,
            max_retries,
            untracked_evictions: 0,
            failed: false,
            srtt: None,
            rttvar: Duration::ZERO,
            cwnd: Self::INIT_CWND,
            ssthresh: f64::MAX,
            recover: None,
        }
    }

    /// Multiplicative decrease on a NACK-driven (fast-retransmit) loss:
    /// ssthresh ← cwnd/2, cwnd ← ssthresh (H-6).
    fn on_loss_fast(&mut self) {
        self.ssthresh = (self.cwnd / 2.0).max(Self::MIN_CWND);
        self.cwnd = self.ssthresh;
    }

    /// Stronger backoff on a timeout (a clearer congestion signal than a
    /// NACK): ssthresh ← cwnd/2, cwnd ← floor — restart slow-start (H-6).
    fn on_loss_timeout(&mut self) {
        self.ssthresh = (self.cwnd / 2.0).max(Self::MIN_CWND);
        self.cwnd = Self::MIN_CWND;
    }

    /// Grow the congestion window for one acked packet: slow-start
    /// (+1) below ssthresh, congestion-avoidance (+1/cwnd) above. Capped
    /// at the retransmit window (can't have more in flight than tracked).
    fn grow_cwnd(&mut self) {
        if self.cwnd < self.ssthresh {
            self.cwnd += 1.0;
        } else {
            self.cwnd += 1.0 / self.cwnd;
        }
        let cap = self.max_pending as f64;
        if self.cwnd > cap {
            self.cwnd = cap;
        }
    }

    /// Fold an RTT sample into the smoothed estimate and recompute the
    /// RTO (RFC 6298). Called from `on_ack` for non-retransmitted
    /// packets only (Karn's algorithm — a retransmitted packet's ack is
    /// ambiguous). The RTO is clamped to [`Self::MIN_RTO`, `Self::MAX_RTO`].
    fn update_rto(&mut self, rtt: Duration) {
        match self.srtt {
            None => {
                self.srtt = Some(rtt);
                self.rttvar = rtt / 2;
            }
            Some(srtt) => {
                let err = srtt.abs_diff(rtt);
                // RTTVAR = 3/4·RTTVAR + 1/4·|SRTT-RTT|
                self.rttvar = (self.rttvar * 3 + err) / 4;
                // SRTT = 7/8·SRTT + 1/8·RTT
                self.srtt = Some((srtt * 7 + rtt) / 8);
            }
        }
        let srtt = self.srtt.unwrap_or(rtt);
        self.rto = (srtt + self.rttvar * 4).clamp(Self::MIN_RTO, Self::MAX_RTO);
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

    /// Check if there are gaps in received sequences.
    ///
    /// A gap exists whenever at least one future sequence has been
    /// received out of order — meaning `next_expected` itself is still
    /// pending (the implicit gap) plus any interior holes between the
    /// received ranges.
    fn has_gaps(&self) -> bool {
        !self.received_ranges.is_empty()
    }

    /// Get bitmap of missing sequences after `next_expected` for the
    /// (unchanged) legacy NACK wire form.
    ///
    /// Bit `i` set means sequence `next_expected + 1 + i` is missing.
    /// Sequence `next_expected` itself is always implicitly missing
    /// whenever `has_gaps()` returns true (that's what makes the NACK
    /// meaningful) — `missing_sequences()` on the resulting NACK emits
    /// `next_expected` first, then the bits of this bitmap.
    ///
    /// Post-R-2 this view is DERIVED from the range index and is
    /// byte-identical to the pre-R-2 bitmap for every state the old
    /// 64-seq horizon could express. When received runs extend past
    /// the 64-seq window the mask widens to all ones — every
    /// unreceived seq inside the window is genuinely missing (there
    /// is provably later data).
    fn missing_bitmap(&self) -> u64 {
        if self.received_ranges.is_empty() {
            return 0;
        }
        // Rebuild the 64-bit received view: bit i = received
        // (next_expected + 1 + i). Ranges are ascending + merged.
        let base = self.next_expected + 1;
        let mut sack: u64 = 0;
        let mut beyond = false;
        for &(start, end) in &self.received_ranges {
            if start >= base + 64 {
                beyond = true;
                break; // ascending — every later range is beyond too
            }
            if end > base + 64 {
                beyond = true;
            }
            // Every range starts > next_expected, i.e. >= base, so
            // the clip below cannot underflow.
            let lo = start.max(base) - base;
            let hi = end.min(base + 64) - base;
            let width = hi - lo;
            if width >= 64 {
                sack = u64::MAX;
            } else {
                sack |= ((1u64 << width) - 1) << lo;
            }
        }
        if sack == 0 {
            // Every received run lives beyond the 64-seq window: the
            // whole window is missing.
            return u64::MAX;
        }
        let mask = if beyond {
            u64::MAX
        } else {
            let highest_bit = 63 - sack.leading_zeros();
            if highest_bit >= 63 {
                u64::MAX
            } else {
                (1u64 << (highest_bit + 1)) - 1
            }
        };
        (!sack) & mask
    }

    /// Budgeted reorder-acceptance horizon in packets (R-2): how far
    /// above `next_expected` an out-of-order arrival is accepted.
    /// Scales with the stream's window-derived budget (`max_pending`
    /// mirrors the rx window bytes / [`Self::MIN_TRACKED_PACKET_BYTES`]
    /// sizing) and floors at the legacy 64, so default-window streams
    /// keep their pre-R-2 behavior exactly.
    pub fn reorder_horizon(&self) -> u64 {
        (self.max_pending as u64).clamp(64, Self::MAX_REORDER_PACKETS)
    }

    /// Out-of-order arrivals accepted into the range index (R-2).
    #[inline]
    pub fn out_of_order_accepted(&self) -> u64 {
        self.oo_accepted
    }

    /// Arrivals rejected past the reorder horizon (R-2).
    #[inline]
    pub fn out_of_order_dropped_horizon(&self) -> u64 {
        self.oo_dropped_horizon
    }

    /// Arrivals rejected at [`Self::MAX_REORDER_RANGES`] capacity.
    #[inline]
    pub fn out_of_order_dropped_capacity(&self) -> u64 {
        self.oo_dropped_capacity
    }

    /// NACK-vs-SACK contradictions observed (R-3). Positive ack wins;
    /// a non-zero value indicates a buggy or hostile peer (the
    /// receiver derives NACKs and ACK ranges from one range index, so
    /// it cannot honestly emit both for the same seq).
    #[inline]
    pub fn protocol_anomalies(&self) -> u64 {
        self.protocol_anomalies
    }

    /// Current out-of-order range count (index occupancy).
    #[inline]
    pub fn reorder_ranges(&self) -> usize {
        self.received_ranges.len()
    }

    /// Insert an out-of-order received `seq` into the range index,
    /// merging with adjacent neighbors (R-2). Returns `false` for
    /// duplicates and for inserts rejected at capacity.
    fn insert_received(&mut self, seq: u64) -> bool {
        // Index of the first range with start > seq.
        let idx = self.received_ranges.partition_point(|&(s, _)| s <= seq);
        // Left neighbor (start <= seq): duplicate or extend-right.
        if idx > 0 {
            let (_, left_end) = self.received_ranges[idx - 1];
            if seq < left_end {
                return false; // duplicate inside the left range
            }
            if seq == left_end {
                // Extends the left range by one; may bridge to the
                // right neighbor.
                self.received_ranges[idx - 1].1 = left_end + 1;
                if idx < self.received_ranges.len() && self.received_ranges[idx].0 == left_end + 1 {
                    let (_, right_end) = self.received_ranges[idx];
                    self.received_ranges[idx - 1].1 = right_end;
                    self.received_ranges.remove(idx);
                }
                self.oo_accepted += 1;
                return true;
            }
        }
        // Right neighbor (start > seq): extend-left.
        if idx < self.received_ranges.len() && self.received_ranges[idx].0 == seq + 1 {
            self.received_ranges[idx].0 = seq;
            self.oo_accepted += 1;
            return true;
        }
        // Fresh disjoint range.
        if self.received_ranges.len() >= Self::MAX_REORDER_RANGES {
            self.oo_dropped_capacity += 1;
            return false;
        }
        self.received_ranges.insert(idx, (seq, seq + 1));
        self.oo_accepted += 1;
        true
    }
}

impl Default for ReliableStream {
    fn default() -> Self {
        Self::new()
    }
}

impl ReliabilityMode for ReliableStream {
    fn on_send(&mut self, descriptor: Arc<RetransmitDescriptor>) {
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
            // Rate-limit the warning: the first eviction is the signal,
            // then every 64th, so a stream stuck in sustained overflow
            // surfaces in logs without drowning them. With H-1 sizing the
            // window to the tx-window this should never fire for a
            // well-configured stream — if it does, the window/packet-size
            // assumption was violated and data was genuinely lost.
            if self.untracked_evictions == 1 || self.untracked_evictions.is_multiple_of(64) {
                tracing::warn!(
                    untracked_evictions = self.untracked_evictions,
                    max_pending = self.max_pending,
                    "ReliableStream: retransmit window full; evicted oldest \
                     unacked packet — NACK for that seq can no longer \
                     recover it. Increase max_pending or apply upstream \
                     backpressure.",
                );
            }
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
            // Head advance + collapse (R-2): absorb the range that
            // just became contiguous, if any. Ranges are merged and
            // non-adjacent, so at most ONE range can start at the new
            // head — absorbing it cannot expose another contiguous
            // range behind it. This is the correctness-critical path
            // that ends a loss episode: e.g. `next_expected = 10`,
            // ranges `[(11, 21)]`, receive 10 ⇒ `next_expected = 21`,
            // ranges empty.
            self.next_expected += 1;
            if let Some(&(start, end)) = self.received_ranges.front() {
                if start == self.next_expected {
                    self.next_expected = end;
                    self.received_ranges.pop_front();
                }
            }
            return true;
        }
        // seq > next_expected: out-of-order future sequence.
        //
        // If the first packet of a stream arrives with seq > 0, this
        // branch records it without advancing next_expected, so
        // sequences `[0, seq)` remain flagged as missing — the
        // receiver requests them via NACK instead of silently
        // skipping them.
        //
        // Budgeted acceptance horizon (R-2): replaces the fixed
        // 64-seq bitmap cap that turned one lost head packet into a
        // 64-packet in-flight ceiling — everything further ahead was
        // dropped on arrival and had to be resent even though it had
        // already crossed the wire.
        let offset = seq - self.next_expected;
        if offset > self.reorder_horizon() {
            self.oo_dropped_horizon += 1;
            return false;
        }
        self.insert_received(seq)
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

    fn on_nack(&mut self, nack: &NackPayload) -> Vec<Arc<RetransmitDescriptor>> {
        // Fast-recovery dedup (T-1): react to a loss episode only once.
        // The receiver re-NACKs a persistent gap every tick (25 ms),
        // which on a link with RTT > tick arrives several times before
        // the first retransmit can return. Without this guard each
        // duplicate would resend the same packet (bandwidth
        // amplification), halve cwnd again (collapsing it far below what
        // one loss warrants), and bump `retries` — tripping a spurious
        // give-up + StreamReset while the retransmit is still in flight.
        // We react when `next_expected` advances past the recover point
        // (a genuinely new gap) and ignore duplicates for the same/older
        // head gap; a lost retransmit is then recovered by the RTO-paced
        // timeout path, not by the NACK flood.
        let new_loss = self.recover.is_none_or(|r| nack.next_expected > r);
        if !new_loss {
            return Vec::new();
        }
        self.recover = Some(nack.next_expected);

        let mut retransmits = Vec::new();

        // Find packets to retransmit based on NACK. Return the
        // pre-encryption descriptors so the caller can rebuild
        // each packet with a fresh cipher counter — replaying the
        // stashed encrypted bytes would trip the receiver's replay
        // window. Per perf #133 the descriptor is `Arc`-shared, so
        // each emission is one atomic refcount bump rather than a
        // deep `Vec<Bytes>` clone.
        for missing_seq in nack.missing_sequences() {
            // R-3 contradiction rule: positive ACK wins. A NACK for a
            // seq the peer already SACKed indicates a buggy or
            // hostile peer — an honest receiver derives NACKs and ACK
            // ranges from one range index and cannot emit both. The
            // packet is already gone from `pending`, so nothing could
            // resend anyway; count the anomaly and move on.
            if self
                .last_sacked
                .iter()
                .any(|&(s, e)| missing_seq >= s && missing_seq < e)
            {
                self.protocol_anomalies = self.protocol_anomalies.saturating_add(1);
                continue;
            }
            for unacked in &mut self.pending {
                if unacked.seq() == missing_seq && unacked.retries < self.max_retries {
                    retransmits.push(Arc::clone(&unacked.descriptor));
                    unacked.retries += 1;
                    unacked.sent_at = Instant::now();
                    break;
                }
            }
        }

        // A NACK-driven retransmit is a loss signal → multiplicative
        // decrease (H-6, fast retransmit).
        if !retransmits.is_empty() {
            self.on_loss_fast();
        }
        retransmits
    }

    fn get_timed_out(&mut self) -> Vec<Arc<RetransmitDescriptor>> {
        let now = Instant::now();
        let rto = self.rto;
        let max_retries = self.max_retries;
        let mut retransmits = Vec::new();
        let mut gave_up = false;

        // Per perf #133 — `Arc::clone` bumps a refcount instead of
        // deep-cloning the `Vec<Bytes>` events list per timed-out packet.
        // A packet that has timed out AND exhausted its retries is
        // dropped from the window (it can't be recovered) and flags the
        // stream as failed (H-3) — previously such packets stayed stuck
        // in `pending` forever, leaking and stalling silently.
        self.pending.retain_mut(|unacked| {
            if now.duration_since(unacked.sent_at) > rto {
                if unacked.retries < max_retries {
                    retransmits.push(Arc::clone(&unacked.descriptor));
                    unacked.retries += 1;
                    unacked.sent_at = now;
                    true // keep — still recoverable
                } else {
                    gave_up = true;
                    false // drop — retransmits exhausted
                }
            } else {
                true // not yet due
            }
        });
        if gave_up {
            self.failed = true;
        }
        // A timeout retransmit is a stronger congestion signal than a
        // NACK → restart slow-start from the floor (H-6).
        if !retransmits.is_empty() {
            self.on_loss_timeout();
        }

        retransmits
    }

    fn take_failed(&mut self) -> bool {
        std::mem::take(&mut self.failed)
    }

    fn rx_ack_seq(&self) -> u64 {
        self.next_expected
    }

    fn on_ack(&mut self, ack_seq: u64) {
        // Pop-front fast path (PERF_AUDIT §3.4): the retransmit
        // window is seq-ordered by insertion (`on_send` pushes to
        // back) so the acked prefix lives at the head. Pre-fix
        // this was a full `pending.retain(|u| u.seq() >= ack_seq)`
        // — O(retransmit_window) per ACK, and on a busy bulk
        // stream the window can grow to MAX_RETRANSMIT_WINDOW
        // (16_384), ACKs arrive every ms via the grant drainer,
        // and the scan dominated CPU.
        //
        // Karn semantics preserved: only non-retransmitted
        // packets (retries == 0) contribute RTT samples, and the
        // freshest such sample wins because we walk front-to-
        // back — same direction `retain` did pre-fix.
        let now = Instant::now();
        let mut sample = None;
        let mut acked = 0usize;
        while let Some(front) = self.pending.front() {
            if front.seq() >= ack_seq {
                break;
            }
            if front.retries == 0 {
                sample = Some(now.duration_since(front.sent_at));
            }
            acked += 1;
            self.pending.pop_front();
        }
        // Straggler tail sweep: a concurrent `send_on_stream` can
        // register a packet whose seq is older than `ack_seq` but
        // ends up behind a newer packet (mesh.rs awaits the
        // socket before registering, so two senders can interleave
        // out-of-order at the tail). These are rare; a bounded
        // look-ahead handles them without falling back to the full
        // O(window) scan. STRAGGLER_LOOKAHEAD = 32 covers the
        // realistic concurrent-sender count comfortably.
        const STRAGGLER_LOOKAHEAD: usize = 32;
        let mut idx = 0;
        while idx < STRAGGLER_LOOKAHEAD && idx < self.pending.len() {
            if self.pending[idx].seq() < ack_seq {
                // `remove(idx)` is O(min(idx, len - idx)) — at most
                // STRAGGLER_LOOKAHEAD = 32 element shifts, bounded
                // and independent of total window size.
                let Some(u) = self.pending.remove(idx) else {
                    // Unreachable: `idx < self.pending.len()` is
                    // verified by the while guard. Bail rather
                    // than panic if it ever ceases to hold.
                    break;
                };
                if u.retries == 0 {
                    sample = Some(now.duration_since(u.sent_at));
                }
                acked += 1;
                // Don't bump `idx` — the slot now holds what was
                // at `idx + 1`.
            } else {
                idx += 1;
            }
        }
        if let Some(rtt) = sample {
            self.update_rto(rtt);
        }
        for _ in 0..acked {
            self.grow_cwnd();
        }
        // Fast recovery (T-1): a cumulative ack past the recover point
        // means the loss episode is repaired — leave recovery so the
        // next genuinely-new gap reacts.
        if self.recover.is_some_and(|r| ack_seq > r) {
            self.recover = None;
        }
    }

    fn on_ack_ranges(&mut self, ack_seq: u64, ranges: &[(u64, u64)]) {
        // Cumulative prune first — pop-front fast path + straggler
        // sweep + RTT sample + cwnd growth + fast-recovery exit.
        self.on_ack(ack_seq);
        if ranges.is_empty() {
            return;
        }
        // Remember the newest SACK view for the NACK contradiction
        // check (best-effort; last message wins, ≤ MAX_ACK_RANGES).
        self.last_sacked = ranges.to_vec();
        // Remove SACKed packets outright — no mark-and-skip. A
        // positive SACK means the receiver HAS them: they leave
        // in-flight accounting and must never be retransmit-eligible
        // again. This is what kills the RTO flood: after one head
        // loss the window holds ONLY the genuinely missing packets,
        // so `get_timed_out` resends O(lost), not O(window).
        let now = Instant::now();
        let mut sample: Option<Duration> = None;
        let mut sacked = 0usize;
        self.pending.retain(|unacked| {
            let seq = unacked.seq();
            let inside = ranges.iter().any(|&(s, e)| seq >= s && seq < e);
            if inside {
                // Karn: only never-retransmitted packets sample RTT.
                // Front-to-back walk ⇒ the newest such packet's
                // sample wins (same convention as `on_ack`).
                if unacked.retries == 0 {
                    sample = Some(now.duration_since(unacked.sent_at));
                }
                sacked += 1;
                false
            } else {
                true
            }
        });
        if let Some(rtt) = sample {
            self.update_rto(rtt);
        }
        // Conservative cwnd growth (R-3): SACKed packets are
        // delivered, so they count as acked — but the growth applied
        // per update is capped at the current cwnd, so one delayed
        // SACK covering thousands of packets can't step-function
        // cwnd into a burst (there is no pacer to absorb it).
        // Slow-start still doubles per update under this cap.
        let cap = (self.cwnd.ceil() as usize).max(1);
        for _ in 0..sacked.min(cap) {
            self.grow_cwnd();
        }
        // `recover` is intentionally NOT cleared by ranges — only a
        // cumulative ack past the recover point ends a loss episode
        // (T-1); the head gap is by definition still missing.
    }

    fn build_ack_ranges(&self, max_ranges: usize) -> Vec<(u64, u64)> {
        // Newest-first (descending by end): truncation under the cap
        // drops the OLDEST ranges — the ones the next cumulative
        // advance covers first (R-1 wire order).
        self.received_ranges
            .iter()
            .rev()
            .take(max_ranges)
            .copied()
            .collect()
    }

    fn can_send(&self) -> bool {
        // H-6: cap in-flight (unacked) packets at the congestion window.
        // On a loss-free path cwnd grows past the retransmit window, so
        // `pending.len()` (also ≤ retransmit window) never reaches it —
        // the gate only bites under sustained loss, which is the point.
        (self.pending.len() as f64) < self.cwnd
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
            .field("received_ranges", &self.received_ranges)
            .field("pending_count", &self.pending.len())
            .field("rto_ms", &self.rto.as_millis())
            .finish()
    }
}

/// Create a boxed reliability mode from configuration. For reliable
/// streams the retransmit window (`max_pending`) is supplied by the
/// caller — sized to the stream's tx-window via
/// [`ReliableStream::max_pending_for_window`] so the sender can never
/// have more in flight than it can retransmit.
pub fn create_reliability_mode(reliable: bool, max_pending: usize) -> Box<dyn ReliabilityMode> {
    if reliable {
        Box::new(ReliableStream::with_settings(
            ReliableStream::DEFAULT_RTO,
            max_pending,
            ReliableStream::DEFAULT_MAX_RETRIES,
        ))
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
    /// shape has something to round-trip through. Returns an
    /// `Arc<...>` per perf #133 — `on_send` consumes the shared
    /// allocation.
    fn descriptor(seq: u64, packet: Bytes) -> Arc<RetransmitDescriptor> {
        Arc::new(RetransmitDescriptor {
            seq,
            stream_id: 0,
            events: vec![packet],
            flags: PacketFlags::RELIABLE,
        })
    }

    #[test]
    fn max_pending_scales_with_window_floored_and_capped() {
        // 0 window (backpressure disabled) → default floor.
        assert_eq!(
            ReliableStream::max_pending_for_window(0),
            ReliableStream::DEFAULT_MAX_PENDING
        );
        // Small window → floored at the default.
        assert_eq!(
            ReliableStream::max_pending_for_window(1024),
            ReliableStream::DEFAULT_MAX_PENDING
        );
        // Mid window → tx_window / MIN_TRACKED_PACKET_BYTES.
        assert_eq!(
            ReliableStream::max_pending_for_window(1024 * 1024),
            (1024 * 1024) / ReliableStream::MIN_TRACKED_PACKET_BYTES as usize
        );
        // Huge window → capped.
        assert_eq!(
            ReliableStream::max_pending_for_window(u32::MAX),
            ReliableStream::MAX_RETRANSMIT_WINDOW
        );
    }

    #[test]
    fn large_window_tracks_all_inflight_without_eviction() {
        // H-1: with a window > the legacy fixed 32, no unacked packet is
        // evicted before it can be retransmitted. Pre-H-1, packet 0 would
        // be evicted once packet 32 was sent and a NACK for it would find
        // nothing to resend.
        let mut s = ReliableStream::with_settings(Duration::from_millis(50), 100, 3);
        for seq in 0..100u64 {
            s.on_send(descriptor(seq, Bytes::from_static(b"payload")));
        }
        assert_eq!(
            s.untracked_evictions(),
            0,
            "a 100-deep window must track all 100 in-flight packets"
        );
        // A NACK for the oldest sequence still recovers it.
        let nack = NackPayload {
            next_expected: 0,
            missing_bitmap: 0,
        };
        let resent = s.on_nack(&nack);
        assert!(
            resent.iter().any(|d| d.seq == 0),
            "oldest in-flight packet must still be retransmittable"
        );
    }

    #[test]
    fn exhausted_retransmits_flag_failure_and_drop() {
        // H-3: a packet that times out past `max_retries` is dropped from
        // the window and flags the stream failed (so the owner can send a
        // reset) — instead of staying stuck forever and stalling silently.
        let rto = Duration::from_millis(5);
        let max_retries = 2u8;
        let mut s = ReliableStream::with_settings(rto, 32, max_retries);
        s.on_send(descriptor(0, Bytes::from_static(b"x")));
        assert!(!s.take_failed());

        // Each RTO elapse → one retransmit, until retries are exhausted.
        for _ in 0..max_retries {
            std::thread::sleep(rto * 2);
            assert_eq!(s.get_timed_out().len(), 1, "still retransmitting");
            assert!(!s.take_failed(), "not failed while retries remain");
        }

        // Next timeout: retries exhausted → give up.
        std::thread::sleep(rto * 2);
        assert!(
            s.get_timed_out().is_empty(),
            "no retransmit emitted once max_retries is hit"
        );
        assert!(s.take_failed(), "stream flagged failed after giving up");
        assert!(!s.take_failed(), "take_failed clears the flag");
        assert!(!s.has_pending(), "given-up packet dropped from the window");
    }

    #[test]
    fn adaptive_rto_rfc6298_and_clamps() {
        // H-5 (deterministic — drives `update_rto` directly to avoid
        // wall-clock flakiness under parallel test load).
        let mut s = ReliableStream::with_settings(Duration::from_millis(50), 32, 3);
        // First sample: srtt=rtt, rttvar=rtt/2 → rto = rtt + 4·(rtt/2) = 3·rtt.
        s.update_rto(Duration::from_millis(20));
        assert_eq!(s.rto, Duration::from_millis(60), "first sample → 3×RTT");
        // A huge RTT estimate caps at MAX_RTO.
        s.update_rto(Duration::from_secs(10));
        assert_eq!(s.rto, ReliableStream::MAX_RTO, "RTO capped at MAX");
        // A fresh stream with a tiny RTT floors at MIN_RTO.
        let mut s2 = ReliableStream::with_settings(Duration::from_millis(50), 32, 3);
        s2.update_rto(Duration::from_micros(1));
        assert_eq!(s2.rto, ReliableStream::MIN_RTO, "tiny RTT floored at MIN");
    }

    #[test]
    fn adaptive_rto_skips_retransmitted_samples_karn() {
        // H-5 / Karn: a retransmitted packet's ack is ambiguous, so it
        // must not update the RTT estimate.
        let mut s = ReliableStream::with_settings(Duration::from_millis(10), 32, 3);
        s.on_send(descriptor(0, Bytes::from_static(b"x")));
        std::thread::sleep(Duration::from_millis(15));
        assert_eq!(s.get_timed_out().len(), 1, "timed-out packet retransmitted");
        s.on_ack(1); // ack — but seq 0 was retransmitted
        assert_eq!(
            s.rto,
            Duration::from_millis(10),
            "Karn: no RTT sample taken from a retransmitted packet"
        );
    }

    #[test]
    fn congestion_window_grows_on_ack_resets_on_timeout() {
        // H-6 AIMD: clean acks grow cwnd (slow-start); a timeout collapses
        // it to the floor.
        let mut s = ReliableStream::with_settings(Duration::from_millis(10), 1000, 3);
        for seq in 0..30u64 {
            s.on_send(descriptor(seq, Bytes::from_static(b"x")));
        }
        s.on_ack(10); // 10 clean acks → slow-start +10
        assert!(
            s.cwnd > ReliableStream::INIT_CWND,
            "cwnd grows on clean acks"
        );
        std::thread::sleep(Duration::from_millis(15));
        assert!(
            !s.get_timed_out().is_empty(),
            "remaining packets time out and retransmit"
        );
        assert_eq!(
            s.cwnd,
            ReliableStream::MIN_CWND,
            "a timeout collapses cwnd to the floor"
        );
    }

    #[test]
    fn congestion_window_halves_on_nack_loss() {
        // H-6: a NACK-driven (fast) retransmit halves cwnd, not to the floor.
        let mut s = ReliableStream::with_settings(Duration::from_millis(50), 1000, 3);
        for seq in 0..20u64 {
            s.on_send(descriptor(seq, Bytes::from_static(b"x")));
        }
        let before = s.cwnd;
        // NACK requesting seq 5 (and a couple more via the bitmap).
        let nack = NackPayload {
            next_expected: 5,
            missing_bitmap: 0,
        };
        assert!(!s.on_nack(&nack).is_empty(), "NACK retransmits seq 5");
        assert!(s.cwnd < before, "fast-retransmit halves cwnd");
        assert!(
            s.cwnd >= ReliableStream::MIN_CWND,
            "but not below the floor"
        );
    }

    #[test]
    fn duplicate_nacks_for_same_gap_are_deduped() {
        // T-1 fast-recovery dedup: a burst of NACKs for the same head gap
        // (the receiver re-NACKs every tick, faster than one RTT) must
        // react only once — no re-resend, no further cwnd cut, no extra
        // `retries` bump → no spurious give-up while the retransmit is in
        // flight.
        let mut s = ReliableStream::with_settings(Duration::from_millis(50), 1000, 3);
        for seq in 0..10u64 {
            s.on_send(descriptor(seq, Bytes::from_static(b"x")));
        }
        let cwnd0 = s.cwnd;
        let nack = NackPayload {
            next_expected: 3,
            missing_bitmap: 0,
        };
        // First NACK for the gap at seq 3 → one retransmit + one cwnd cut.
        assert_eq!(s.on_nack(&nack).len(), 1, "first NACK retransmits seq 3");
        let cwnd1 = s.cwnd;
        assert!(cwnd1 < cwnd0, "first NACK halves cwnd");
        // A flood of identical NACKs → all ignored.
        for _ in 0..5 {
            assert!(
                s.on_nack(&nack).is_empty(),
                "duplicate NACK for the same gap is ignored"
            );
        }
        assert_eq!(s.cwnd, cwnd1, "cwnd is not cut again by duplicate NACKs");
        assert!(!s.take_failed(), "no spurious give-up from the NACK flood");

        // Once the gap is repaired (ack advances past it), a NACK for a
        // NEW, later gap reacts again.
        s.on_ack(4); // seq 3 acked → recovery ends
        let cwnd2 = s.cwnd;
        let nack2 = NackPayload {
            next_expected: 7,
            missing_bitmap: 0,
        };
        assert_eq!(s.on_nack(&nack2).len(), 1, "a new gap reacts");
        assert!(s.cwnd < cwnd2, "a new loss episode cuts cwnd again");
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

    /// PERF_AUDIT §3.4 — the pop-front fast path must drain the
    /// acked prefix from `pending` in one bounded sweep, leaving
    /// only the still-in-flight tail behind. This is the steady-
    /// state shape (no concurrent reorder).
    #[test]
    fn on_ack_pops_acked_prefix_and_keeps_in_flight_tail() {
        let mut mode = ReliableStream::new();
        for i in 0..10u64 {
            mode.on_send(descriptor(i, Bytes::from(format!("p{i}"))));
        }
        assert_eq!(mode.pending.len(), 10);
        // Ack through seq 6 (exclusive); seqs 0..=5 should be gone.
        mode.on_ack(6);
        assert_eq!(mode.pending.len(), 4);
        let remaining_seqs: Vec<u64> = mode.pending.iter().map(|u| u.seq()).collect();
        assert_eq!(remaining_seqs, vec![6, 7, 8, 9]);
    }

    /// PERF_AUDIT §3.4 — the straggler tail sweep must handle an
    /// out-of-order packet whose seq is below `ack_seq` but ended
    /// up behind a newer packet in the deque (the realistic
    /// concurrent-`send_on_stream` race the audit calls out). The
    /// sweep is bounded; this test fits comfortably inside the
    /// 32-element look-ahead.
    #[test]
    fn on_ack_picks_up_straggler_behind_newer_packet() {
        let mut mode = ReliableStream::new();
        // Insert in deliberately-out-of-order shape: seqs 0..=4 in
        // order, then seq 7, then seq 5 (the straggler), then seq 6
        // (newer-ordered).
        for i in 0..=4u64 {
            mode.on_send(descriptor(i, Bytes::from(format!("p{i}"))));
        }
        mode.on_send(descriptor(7, Bytes::from_static(b"p7")));
        mode.on_send(descriptor(5, Bytes::from_static(b"p5"))); // straggler
        mode.on_send(descriptor(6, Bytes::from_static(b"p6")));
        assert_eq!(mode.pending.len(), 8);
        // Ack through seq 6 (exclusive). Front drains 0..=4
        // (5 entries). Then the deque holds [7, 5, 6]; the straggler
        // sweep should pick up seq 5 (and seq 6 if its seq < ack_seq;
        // here 6 < 6 is false, so it stays).
        mode.on_ack(6);
        let remaining_seqs: Vec<u64> = mode.pending.iter().map(|u| u.seq()).collect();
        assert_eq!(remaining_seqs, vec![7, 6]);
    }

    /// PERF_AUDIT §3.4 — a straggler displaced beyond the
    /// 32-element look-ahead is MISSED by the ACK that covers it
    /// (the bounded sweep is the whole point), but must self-heal:
    /// once the newer packets ahead of it are acked and drained by
    /// the pop-front fast path, the straggler migrates into the
    /// look-ahead window and the next ACK sweeps it. The transient
    /// cost is at most one spurious RTO retransmit; what this test
    /// guards against is the *permanent* leak (straggler pinned in
    /// `pending` forever → repeated spurious retransmits and an
    /// eventual false `failed` via max_retries exhaustion).
    #[test]
    fn on_ack_straggler_beyond_lookahead_self_heals_on_later_ack() {
        // Window sized above the 41 tracked packets so the on_send
        // eviction path (max_pending) stays out of the picture.
        let mut mode = ReliableStream::with_settings(Duration::from_millis(50), 100, 3);
        // 40 newer packets first (seqs 1..=40), then the straggler
        // (seq 0) at index 40 — past the 32-element look-ahead.
        for i in 1..=40u64 {
            mode.on_send(descriptor(i, Bytes::from(format!("p{i}"))));
        }
        mode.on_send(descriptor(0, Bytes::from_static(b"straggler")));
        assert_eq!(mode.pending.len(), 41);

        // ACK covering only the straggler (ack_seq = 1 → seq 0 is
        // acked). Front is seq 1 (>= 1, no pop); the sweep stops at
        // index 31; seq 0 sits at index 40 — missed, still pending.
        mode.on_ack(1);
        assert_eq!(
            mode.pending.len(),
            41,
            "straggler beyond the look-ahead is expected to survive this ACK"
        );

        // ACK covering everything: pop-front drains seqs 1..=40,
        // which moves the straggler to index 0 — well inside the
        // look-ahead — and the sweep collects it.
        mode.on_ack(41);
        assert!(
            mode.pending.is_empty(),
            "straggler must be swept once the packets ahead of it drain: {:?}",
            mode.pending.iter().map(|u| u.seq()).collect::<Vec<_>>()
        );
    }

    /// The worst-case cost of the bounded look-ahead, pinned: a
    /// cumulatively-acked straggler stranded beyond the 32-element
    /// sweep CAN be spuriously retransmitted by the RTO while it
    /// waits for the packets ahead of it to drain — but the
    /// spuriousness is bounded (the retransmit is a duplicate the
    /// receiver's replay window absorbs), the stream must never
    /// reach the failed state because of it, and any later
    /// cumulative ack covering it reclaims it through the pop-front
    /// prefix walk (an acked packet cannot outlive a covering
    /// cumulative ack). Regression guard for the "stranded acked
    /// packet → spurious retransmit / failed-state" hazard of the
    /// §3.4 bounded sweep.
    #[test]
    fn straggler_spurious_retransmit_is_bounded_and_never_fails_stream() {
        let rto = Duration::from_millis(5);
        let mut s = ReliableStream::with_settings(rto, 100, 3);
        // 40 newer packets, then the straggler (seq 0) at index 40
        // — past the 32-element look-ahead.
        for i in 1..=40u64 {
            s.on_send(descriptor(i, Bytes::from(format!("p{i}"))));
        }
        s.on_send(descriptor(0, Bytes::from_static(b"straggler")));

        // Cumulative ack covering ONLY the straggler: front is seq 1
        // (>= 1, no pop), the sweep stops at index 31 — seq 0 stays
        // pending even though the peer has it.
        s.on_ack(1);
        assert_eq!(s.pending.len(), 41, "straggler stranded past the sweep");

        // RTO elapses: the stranded-but-acked straggler is among the
        // retransmits (the bounded cost the look-ahead trades for
        // O(window) ack scans). Retries remain (< max), so the
        // stream must NOT flag failed.
        std::thread::sleep(rto * 2);
        let resent = s.get_timed_out();
        assert!(
            resent.iter().any(|d| d.seq == 0),
            "stranded acked straggler is spuriously retransmitted once"
        );
        assert!(
            !s.take_failed(),
            "a spurious straggler retransmit must not fail the stream"
        );

        // Later cumulative ack covering everything: the pop-front
        // prefix walk drains seqs 1..=40 AND then seq 0 (0 < 41) —
        // the acked straggler cannot survive a covering ack.
        s.on_ack(41);
        assert!(
            s.pending.is_empty(),
            "covering cumulative ack reclaims the straggler: {:?}",
            s.pending.iter().map(|u| u.seq()).collect::<Vec<_>>()
        );
        assert!(!s.take_failed(), "stream healthy after recovery");
    }

    #[test]
    fn test_reliable_stream_pending() {
        let mut mode = ReliableStream::new();

        assert!(!mode.has_pending());

        mode.on_send(descriptor(1, Bytes::from_static(b"packet1")));
        mode.on_send(descriptor(2, Bytes::from_static(b"packet2")));

        assert!(mode.has_pending());

        // ACK should clear pending. `on_ack` takes the cumulative ack =
        // next_expected (exclusive), so 3 acks seq 1 and 2.
        mode.on_ack(3);
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
        let mode = create_reliability_mode(false, ReliableStream::DEFAULT_MAX_PENDING);
        assert_eq!(mode.name(), "fire-and-forget");

        let mode = create_reliability_mode(true, ReliableStream::DEFAULT_MAX_PENDING);
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
        // Regression: a duplicate seq 0 must be rejected for exactly-once
        // delivery. `on_receive` rejects any `seq < next_expected` as a
        // duplicate: after seq 0 is accepted, `next_expected` advances to
        // 1, so a re-delivered seq 0 (0 < 1) is rejected. (An earlier
        // impl special-cased `seq == 0 && ack_seq == 0`; since `ack_seq`
        // stayed 0 right after receiving seq 0, that path re-accepted the
        // duplicate — the `next_expected`-based check has no such hole.)
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
        // Regression: seq 0 arriving after the window has advanced (e.g.
        // next_expected = 6) must be rejected as a duplicate. The
        // `seq < next_expected` check covers it and must not move the
        // window backwards. (An earlier seq-0 special case risked
        // resetting the window to 0.) This test ensures the fix holds.
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
        mode.on_send(Arc::new(RetransmitDescriptor {
            seq: 0,
            stream_id: 7,
            events: events_a.clone(),
            flags: PacketFlags::RELIABLE,
        }));
        mode.on_send(Arc::new(RetransmitDescriptor {
            seq: 1,
            stream_id: 7,
            events: events_b.clone(),
            flags: PacketFlags::RELIABLE,
        }));
        mode.on_send(Arc::new(RetransmitDescriptor {
            seq: 2,
            stream_id: 7,
            events: events_c.clone(),
            flags: PacketFlags::RELIABLE,
        }));

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

        // A duplicate NACK for the same head gap is deduped (T-1) — the
        // first retransmit is in flight, so re-NACKing must not re-emit.
        let dup = NackPayload {
            next_expected: 1,
            missing_bitmap: 0,
        };
        assert!(
            mode.on_nack(&dup).is_empty(),
            "duplicate NACK for the same gap is deduped"
        );

        // A NACK for a genuinely newer gap (seq 2) does re-emit, carrying
        // its own pre-encryption descriptor. Each retransmit lets the
        // caller produce a fresh-counter packet — the descriptor inputs
        // (stream_id/events/flags) are what fix the replay-window
        // rejection; cipher-counter freshness is the rebuild caller's job.
        let nack2 = NackPayload {
            next_expected: 2,
            missing_bitmap: 0,
        };
        let retransmits2 = mode.on_nack(&nack2);
        assert_eq!(retransmits2.len(), 1);
        let r2 = &retransmits2[0];
        assert_eq!(r2.seq, 2);
        assert_eq!(r2.events, events_c);
        assert_eq!(r2.flags, PacketFlags::RELIABLE);
        assert_eq!(r2.stream_id, 7);
    }

    /// Pin crypto-session perf #133: `on_nack` and `get_timed_out`
    /// must emit `Arc::clone`s of the descriptor already held in
    /// the retransmit window, not deep copies. Compare backing
    /// pointers via `Arc::as_ptr` — a regression that swaps back to
    /// `descriptor.clone()` on the inner `RetransmitDescriptor`
    /// would silently re-introduce the per-retransmit
    /// `Vec<Bytes>` allocation + N `Bytes` refcount bumps.
    #[test]
    fn retransmits_share_descriptor_via_arc_refcount_not_deep_clone() {
        let mut mode = ReliableStream::with_settings(Duration::from_millis(20), 32, 5);

        let original = Arc::new(RetransmitDescriptor {
            seq: 0,
            stream_id: 7,
            events: vec![Bytes::from_static(b"event-A")],
            flags: PacketFlags::RELIABLE,
        });
        let original_ptr = Arc::as_ptr(&original);
        mode.on_send(Arc::clone(&original));

        // NACK path: emitted Arc points at the same allocation as
        // the original we pushed (refcount bump, not a clone).
        let nack = NackPayload {
            next_expected: 0,
            missing_bitmap: 1,
        };
        let from_nack = mode.on_nack(&nack);
        assert_eq!(from_nack.len(), 1, "nack should produce one retransmit");
        assert_eq!(
            Arc::as_ptr(&from_nack[0]),
            original_ptr,
            "on_nack must clone the Arc, not deep-clone the descriptor"
        );

        // Timeout path: re-arm the timer, sleep, drain. Same
        // pointer-identity assertion as the NACK path.
        std::thread::sleep(Duration::from_millis(35));
        let from_timeout = mode.get_timed_out();
        assert!(
            !from_timeout.is_empty(),
            "expected at least one timed-out retransmit"
        );
        assert_eq!(
            Arc::as_ptr(&from_timeout[0]),
            original_ptr,
            "get_timed_out must clone the Arc, not deep-clone the descriptor"
        );
    }

    // ── STREAM_ACK_BATCHING Phase 2 (R-2/R-3): range index, budgeted
    //    horizon, SACK-driven pruning ────────────────────────────────

    /// R-2 head-collapse — the correctness-critical path that ends a
    /// loss episode: filling the head gap must advance `next_expected`
    /// THROUGH the range that just became contiguous, not by one.
    #[test]
    fn head_collapse_advances_through_absorbed_range() {
        let mut s = ReliableStream::new();
        for i in 0..10u64 {
            assert!(s.on_receive(i));
        }
        for i in 11..21u64 {
            assert!(s.on_receive(i), "seq {i} within horizon must be accepted");
        }
        assert_eq!(s.ack_seq(), 9, "head gap at 10 pins the cumulative ack");
        assert_eq!(s.reorder_ranges(), 1, "11..21 merged into one range");
        assert!(s.build_nack().is_some());

        assert!(s.on_receive(10), "head fill");
        assert_eq!(
            s.next_expected(),
            21,
            "next_expected must collapse through the absorbed range"
        );
        assert_eq!(s.reorder_ranges(), 0);
        assert!(s.build_nack().is_none(), "no gaps left");
    }

    /// R-2 budgeted horizon: default windows keep the legacy 64
    /// exactly; large windows scale it up (the pre-R-2 cliff: one
    /// lost head packet capped usable in-flight at 64 regardless of
    /// window).
    #[test]
    fn reorder_horizon_floors_at_64_and_scales_with_window() {
        // Default (max_pending = 32) → horizon 64: identical to the
        // legacy bitmap behavior at both edges.
        let mut s = ReliableStream::new();
        assert!(s.on_receive(0)); // next_expected = 1
        assert!(s.on_receive(1 + 64), "offset 64 accepted (legacy edge)");
        assert!(!s.on_receive(1 + 65), "offset 65 rejected (legacy edge)");
        assert_eq!(s.out_of_order_dropped_horizon(), 1);

        // Window-sized stream → horizon = max_pending.
        let mut big = ReliableStream::with_settings(Duration::from_millis(50), 1000, 3);
        assert!(big.on_receive(0));
        assert!(
            big.on_receive(1 + 1000),
            "offset 1000 accepted with a 1000-packet window"
        );
        assert!(!big.on_receive(1 + 1001), "offset 1001 rejected");
        assert_eq!(big.reorder_horizon(), 1000);
    }

    /// R-2 capacity bound: a 33rd disjoint hole is rejected (counted),
    /// but an arrival that MERGES into existing ranges is always
    /// accepted — capacity gates fresh ranges, not progress.
    #[test]
    fn range_capacity_rejects_fresh_range_but_accepts_merges() {
        let mut s = ReliableStream::with_settings(Duration::from_millis(50), 16_384, 3);
        assert!(s.on_receive(0)); // next_expected = 1
        for i in 0..ReliableStream::MAX_REORDER_RANGES as u64 {
            assert!(s.on_receive(2 + i * 2), "disjoint seq {}", 2 + i * 2);
        }
        assert_eq!(s.reorder_ranges(), ReliableStream::MAX_REORDER_RANGES);

        let next_disjoint = 2 + ReliableStream::MAX_REORDER_RANGES as u64 * 2;
        assert!(!s.on_receive(next_disjoint), "fresh range at capacity");
        assert_eq!(s.out_of_order_dropped_capacity(), 1);

        // seq 3 bridges (2,3) and (4,5) into (2,5): accepted, and the
        // index SHRINKS.
        assert!(s.on_receive(3));
        assert_eq!(s.reorder_ranges(), ReliableStream::MAX_REORDER_RANGES - 1);
    }

    /// R-2: the legacy NACK bitmap derived from the range index is
    /// identical to the pre-R-2 bitmap for expressible states, and
    /// widens to all-ones when received runs live beyond the 64-seq
    /// window (everything in the window is genuinely missing).
    #[test]
    fn legacy_nack_bitmap_derives_from_ranges() {
        // Mirror of `test_reliable_stream_gap` shape: received {0, 1,
        // 3, 5} → next_expected 2, received view {3, 5} = bits 1 and 3
        // off base 3 → missing = seq 4 (bit 1).
        let mut m = ReliableStream::new();
        for seq in [0u64, 1, 3, 5] {
            assert!(m.on_receive(seq));
        }
        let nack = m.build_nack().unwrap();
        assert_eq!(nack.next_expected, 2);
        let missing: Vec<u64> = nack.missing_sequences().collect();
        assert_eq!(missing, vec![2, 4]);

        // Far-future run only: whole 64-seq window is missing.
        let mut far = ReliableStream::with_settings(Duration::from_millis(50), 1000, 3);
        assert!(far.on_receive(0));
        assert!(far.on_receive(200));
        let nack = far.build_nack().unwrap();
        assert_eq!(nack.next_expected, 1);
        assert_eq!(nack.missing_bitmap, u64::MAX);
    }

    /// R-3 — the killer demo at unit level: one lost head packet
    /// under a large in-flight, SACKed via one range, leaves O(lost)
    /// in the retransmit window; the RTO backstop resends 1 packet,
    /// not 1000 (pre-R-3 it resent everything and cwnd floored).
    #[test]
    fn on_ack_ranges_suppresses_rto_flood_after_head_loss() {
        let rto = Duration::from_millis(5);
        let mut s = ReliableStream::with_settings(rto, 16_384, 3);
        for seq in 0..1000u64 {
            s.on_send(descriptor(seq, Bytes::from_static(b"x")));
        }
        // Receiver has 1..1000, missing only 0.
        s.on_ack_ranges(0, &[(1, 1000)]);
        assert_eq!(
            s.pending.len(),
            1,
            "only the genuinely missing head stays tracked"
        );
        std::thread::sleep(rto * 2);
        let resent = s.get_timed_out();
        assert_eq!(resent.len(), 1, "RTO resends O(lost), not O(window)");
        assert_eq!(resent[0].seq, 0);
        assert!(!s.take_failed(), "retries remain — no spurious give-up");
    }

    /// R-3: cwnd growth per SACK update is capped at the current cwnd
    /// — one delayed SACK covering thousands of packets must not
    /// step-function cwnd into a burst.
    #[test]
    fn on_ack_ranges_cwnd_growth_is_capped_per_update() {
        let mut s = ReliableStream::with_settings(Duration::from_millis(50), 16_384, 3);
        for seq in 0..5000u64 {
            s.on_send(descriptor(seq, Bytes::from_static(b"x")));
        }
        let before = s.cwnd;
        s.on_ack_ranges(0, &[(1, 5000)]);
        assert!(
            s.cwnd <= before * 2.0,
            "one update at most doubles cwnd (slow-start under cap): {} -> {}",
            before,
            s.cwnd
        );
    }

    /// R-3 / Karn: a SACKed packet that was retransmitted must not
    /// contribute an RTT sample.
    #[test]
    fn on_ack_ranges_skips_rtt_sample_for_retransmitted_karn() {
        let mut s = ReliableStream::with_settings(Duration::from_millis(10), 32, 3);
        s.on_send(descriptor(0, Bytes::from_static(b"x")));
        s.on_send(descriptor(1, Bytes::from_static(b"x")));
        std::thread::sleep(Duration::from_millis(15));
        assert_eq!(s.get_timed_out().len(), 2, "both retransmitted");
        s.on_ack_ranges(0, &[(1, 2)]);
        assert_eq!(
            s.rto,
            Duration::from_millis(10),
            "Karn: no RTT sample from a retransmitted SACKed packet"
        );
    }

    /// R-3 contradiction rule: a NACK claiming a seq missing that a
    /// prior SACK acknowledged resends nothing (positive ack wins)
    /// and bumps the anomaly counter.
    #[test]
    fn nack_after_sack_counts_protocol_anomaly_and_resends_nothing() {
        let mut s = ReliableStream::new();
        for seq in 0..5u64 {
            s.on_send(descriptor(seq, Bytes::from_static(b"x")));
        }
        s.on_ack_ranges(0, &[(2, 4)]); // peer HAS 2 and 3
        assert_eq!(s.protocol_anomalies(), 0);
        let nack = NackPayload {
            next_expected: 2, // …and now claims 2 is missing
            missing_bitmap: 0,
        };
        assert!(
            s.on_nack(&nack).is_empty(),
            "positive ack wins — nothing resent"
        );
        assert_eq!(s.protocol_anomalies(), 1);
    }

    /// R-1/R-4 producer↔consumer consistency: ranges built by the
    /// receiver are newest-first, truncate oldest-first, and always
    /// pass the wire codec's strict validation.
    #[test]
    fn build_ack_ranges_newest_first_and_codec_valid() {
        use crate::adapter::net::subprotocol::stream_window::{StreamAckRanges, MAX_ACK_RANGES};

        let mut s = ReliableStream::with_settings(Duration::from_millis(50), 16_384, 3);
        assert!(s.on_receive(0)); // next_expected = 1
        for i in 0..20u64 {
            assert!(s.on_receive(2 + 2 * i));
        }
        let ranges = s.build_ack_ranges(MAX_ACK_RANGES);
        assert_eq!(ranges.len(), MAX_ACK_RANGES, "truncated to the cap");
        assert_eq!(
            ranges[0],
            (2 + 2 * 19, 2 + 2 * 19 + 1),
            "newest (highest) range first"
        );
        assert!(
            ranges.windows(2).all(|w| w[0].0 > w[1].1),
            "strictly descending, non-adjacent"
        );
        assert_eq!(
            ranges.last().copied().unwrap(),
            (2 + 2 * 4, 2 + 2 * 4 + 1),
            "truncation dropped the 4 OLDEST ranges"
        );

        // Whatever the receiver produces must decode cleanly.
        let msg = StreamAckRanges {
            stream_id: 7,
            ack_seq: s.rx_ack_seq(),
            ranges,
        };
        assert_eq!(
            StreamAckRanges::decode(&msg.encode()).expect("receiver output is always codec-valid"),
            msg
        );
    }
}
