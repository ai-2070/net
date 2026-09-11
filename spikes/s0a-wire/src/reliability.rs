//! Reliability modes for Net streams.
//!
//! Net supports two reliability modes:
//! - Fire-and-forget: No acknowledgments, maximum throughput
//! - Reliable: Per-stream reliability with selective NACKs

use bytes::Bytes;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::clock::{Clock, Instant, SystemClock};

use crate::protocol::{NackPayload, PacketFlags};

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
    /// NACK-vs-SACK contradictions observed: a NACK named a seq that
    /// a prior SACK (in `last_sacked`) positively acknowledged.
    /// Positive ack wins — the packet is already out of the window —
    /// so this only counts (R-3). Best-effort signal, NOT proof of a
    /// hostile peer: the receiver's NACK and SACK for one gap travel
    /// as separate datagrams and can reorder on the wire (or a stale
    /// prior-tick NACK can arrive after a fresher SACK), so an honest
    /// receiver on a lossy/jittery path trips this too. Treat a
    /// non-zero value as "investigate," not "ban."
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
    ///
    /// Caveat (review HORIZON): `max_pending` is derived from the
    /// stream's `tx_window`, and this reuses it as the *receive-side*
    /// acceptance budget. That is correct only because a stream is
    /// currently constructed with one window value feeding both the
    /// tx retransmit-window sizing and the rx credit
    /// (`StreamState::new_full_with_epoch`). If tx and rx windows ever
    /// become independently configurable, the horizon must be driven
    /// by the rx budget explicitly (an rx-window arg into the
    /// reliability state) rather than piggybacking `max_pending` —
    /// otherwise the receiver would accept out-of-order seqs against
    /// the *sender's* window, which R-2 explicitly warns against.
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

    /// NACK-vs-SACK contradictions observed (R-3). Positive ack wins.
    /// A best-effort anomaly signal, NOT a hostile-peer verdict: the
    /// receiver derives NACKs and SACK ranges from one range index and
    /// cannot emit both for the same seq *in a single snapshot*, but
    /// the two ride separate datagrams that reorder independently on
    /// the wire, so an honest peer on a lossy/jittery link raises this
    /// too. Alert on sustained growth, not a single count.
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
            sent_at: SystemClock::now(),
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
            // seq a prior SACK already acknowledged is a best-effort
            // anomaly signal — the packet is already gone from
            // `pending`, so nothing could resend anyway; count it and
            // move on. This is NOT proof of a hostile peer: an honest
            // receiver's NACK and SACK for one gap are separate
            // datagrams that reorder independently, so a stale NACK
            // arriving after a fresher SACK trips this on any lossy /
            // jittery link (see `protocol_anomalies`). The same-cycle
            // build is now consistent (one reliability-lock snapshot
            // in `build_session_control_events`), so what remains is
            // wire reordering, which no receiver can prevent.
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
                    unacked.sent_at = SystemClock::now();
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
        let now = SystemClock::now();
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
        let now = SystemClock::now();
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
        // Reuse the existing buffer instead of allocating a fresh Vec
        // per SACK: on a loss episode these arrive at up to the drain
        // cadence (~1 kHz) plus the 25 ms tick, and the capacity is
        // bounded at MAX_ACK_RANGES, so `clear` + `extend_from_slice`
        // is steady-state allocation-free (review SACKSCAN/H7).
        //
        // Note on the full-`retain` cost (review SACKSCAN/H1): a
        // deliberate short-circuit for a repeated-identical SACK is NOT
        // applied. After the first SACK prunes the window down to only
        // the genuinely-missing packets, `pending` is O(lost), so a
        // repeated SACK's `retain` is already cheap; and skipping it
        // would leave a concurrently-registered in-range straggler
        // (see the `on_ack` straggler sweep) in `pending` for a
        // spurious later retransmit — trading the feature's core
        // no-RTO-flood guarantee for a scan that is no longer hot.
        self.last_sacked.clear();
        self.last_sacked.extend_from_slice(ranges);
        // Remove SACKed packets outright — no mark-and-skip. A
        // positive SACK means the receiver HAS them: they leave
        // in-flight accounting and must never be retransmit-eligible
        // again. This is what kills the RTO flood: after one head
        // loss the window holds ONLY the genuinely missing packets,
        // so `get_timed_out` resends O(lost), not O(window).
        let now = SystemClock::now();
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

