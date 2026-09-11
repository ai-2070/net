//! Session and stream state management for Net.
//!
//! This module manages session state after Noise handshake completion,
//! including per-stream state for multiplexing.

use bytes::Bytes;
use crossbeam_queue::SegQueue;
use dashmap::DashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::clock::{Clock, Instant, SystemClock};

use crate::event::StoredEvent;

use crate::crypto::{PacketCipher, SessionKeys};
use crate::route_hop::SharedHopReplayWindow;
// `SharedPacketPool` is intentionally absent — `NetSession` uses
// only `SharedLocalPool` as the single TX-side AEAD source.
use crate::pool::SharedLocalPool;
use crate::reliability::{
    create_reliability_mode, ReliabilityMode, ReliableStream, RetransmitDescriptor,
};
use crate::stream::DEFAULT_STREAM_WINDOW_BYTES;
use crate::parsed_packet::ParsedPacket;

/// TIME_WAIT-style quarantine window after `close_stream`. A
/// `StreamWindow` grant that arrives for a stream closed within
/// this window is dropped — protects a reopened stream from
/// being credited by in-flight grants minted against the previous
/// lifetime.
///
/// Sized to comfortably exceed grant RTT on LAN / typical mesh
/// deployments. Callers that rapidly reopen the same `stream_id`
/// will see a brief stall (the reopened stream won't receive
/// grants until the quarantine expires) — an acceptable trade-off
/// for correct credit accounting across lifetimes.
pub const GRANT_QUARANTINE_WINDOW: Duration = Duration::from_secs(2);

/// One gapped stream's proactive-tick report (STREAM_ACK_BATCHING
/// R-4), produced by [`NetSession::collect_gap_reports`] under a
/// single reliability lock per stream so the NACK and the SACK ranges
/// describe the same received-range snapshot.
#[derive(Debug, Clone)]
pub struct GapReport {
    /// Stream the gap is on.
    pub stream_id: u64,
    /// Legacy negative ack for the gap (always present — every gapped
    /// stream emits one, capability-independent).
    pub nack: crate::protocol::NackPayload,
    /// Cumulative ack (`next_expected`) captured in the same snapshot
    /// as `ranges`, so the outgoing `StreamAckRanges` is internally
    /// consistent (every range strictly above `ack_seq`).
    pub ack_seq: u64,
    /// Positive SACK ranges, newest-first. Empty when the peer does
    /// not advertise the ack-ranges capability (`want_ranges = false`).
    pub ranges: Vec<(u64, u64)>,
}

/// Session state after handshake completion.
pub struct NetSession {
    /// Session ID (derived from handshake)
    session_id: u64,
    /// Remote peer address
    peer_addr: SocketAddr,
    /// RX cipher (ChaCha20-Poly1305 with counter-based nonces)
    rx_cipher: PacketCipher,
    // No `tx_key` field: `thread_local_pool` is the only surface
    // that holds the TX key on a live `NetSession`. Storing an
    // extra copy here would re-open a cross-pool nonce-reuse
    // hazard — independent counters under the same ChaCha20-
    // Poly1305 key — and would only be read back through a
    // `tx_key()` accessor whose only consumers are misuses (e.g.
    // a fresh `PacketBuilder::new` that bypasses the
    // thread-local pool's nonce sequencing).
    /// Per-stream state
    streams: DashMap<u64, StreamState>,
    /// Last activity timestamp (for session timeout)
    last_activity: AtomicU64,
    /// Thread-local pool for zero-contention hot path. The single
    /// authoritative source of TX-side AEAD encryptions for this
    /// session — see the `tx_key` comment above for the
    /// cross-pool nonce-reuse rationale.
    thread_local_pool: SharedLocalPool,
    /// Default reliability mode for new streams
    default_reliable: bool,
    /// Session is active
    active: AtomicBool,
    /// Monotonic generator for per-`StreamState` epochs. Each opened
    /// stream captures a unique epoch at construction time so that
    /// stale `Stream` handles or `TxSlotGuard`s from a previous
    /// open/close cycle can't silently operate on a new stream that
    /// reuses the same `stream_id`.
    stream_epoch_counter: AtomicU64,
    /// Stream IDs closed within the last `GRANT_QUARANTINE_WINDOW`.
    /// Used to drop in-flight `StreamWindow` grants minted against a
    /// previous lifetime of a `stream_id` so they can't credit a
    /// subsequent reopen. Entries are inserted on `close_stream` and
    /// lazily garbage-collected by `is_grant_quarantined` on read.
    recently_closed: DashMap<u64, Instant>,
    /// Monotonic sequence counter for subprotocol control packets
    /// (grants, membership acks, etc.) that don't belong to a
    /// user-opened stream. Using a separate counter keeps control
    /// traffic out of the `streams` map, so a caller who opens a
    /// stream with a numerically-equal id (e.g., `0x0B00`, the
    /// `SUBPROTOCOL_STREAM_WINDOW` constant) can't have their
    /// sequence space polluted by control packets.
    control_tx_seq: AtomicU64,
    /// Per-session cache of the resolved peer `NodeId`.
    ///
    /// Pre-fix [discovery-routing perf #108 in
    /// `docs/internal/performance/net-discovery-routing-analysis.md`] the
    /// inbound dispatcher's RPC hook ran the
    /// `addr_to_node → peers.get → session_id-match → fallback
    /// O(N) peer scan` resolution chain on **every** inbound RPC
    /// packet. The session itself is stable — once we've resolved
    /// `session → node_id` for an established session, that
    /// mapping doesn't change.
    ///
    /// The cache uses `0` as the "unresolved" sentinel — real
    /// `NodeId`s are non-zero in production (`0` is the test /
    /// loopback sentinel that already gets rejected by the
    /// dispatcher's `Some(from_node) else { drop }` guard).
    /// `Relaxed` ordering is enough: a tear in the published
    /// value would only manifest as a re-resolution on the next
    /// packet (which then re-publishes the same value), and the
    /// resolver itself is the source of truth.
    cached_node_id: AtomicU64,
    /// Key for MACing route-hop envelopes this node sends on this
    /// edge. See [`Self::seal_route_hop`].
    route_hop_tx_key: [u8; 32],
    /// Key for verifying route-hop envelopes received on this edge.
    route_hop_rx_key: [u8; 32],
    /// This edge's outbound hop sequence — separate from the packet
    /// AEAD counter by design.
    route_hop_tx_seq: AtomicU64,
    /// Sliding replay window over inbound hop sequences.
    ///
    /// Lock-free single-writer state, not a mutex: the production
    /// protected-ingress path is single-consumer (one receive loop,
    /// synchronous dispatch), so admission never contends there, and
    /// the ordinary path pays no locking. A second concurrent caller
    /// — only reachable by breaking that ownership rule — is refused
    /// immediately and its packet dropped
    /// ([`crate::route_hop::RouteHopError::Contended`]).
    route_hop_replay: SharedHopReplayWindow,
}

/// Sentinel `stream_id` used in the header of subprotocol control
/// packets (credit grants, etc.). Chosen at the top of the u64
/// range so it cannot collide with practical user-chosen ids or
/// with the output of `stream_id_from_key`. The receiver dispatches
/// these packets by `subprotocol_id`, not `stream_id`, so the
/// sentinel is purely there to keep sender-side per-stream state
/// clean.
pub const CONTROL_STREAM_ID: u64 = u64::MAX;

impl NetSession {
    /// Create a new session from handshake results
    pub fn new(
        keys: SessionKeys,
        peer_addr: SocketAddr,
        pool_size: usize,
        default_reliable: bool,
    ) -> Self {
        let rx_cipher = PacketCipher::new(&keys.rx_key, keys.session_id);

        // Only `thread_local_pool` is constructed with the TX key.
        // Independently constructing a `tx_cipher` and a
        // `packet_pool` with the same key but independent counters
        // would re-open a cross-pool nonce-reuse hazard — see the
        // `tx_key` comment above. The data path uses
        // `thread_local_pool` exclusively.
        let thread_local_pool =
            crate::pool::shared_local_pool(pool_size, &keys.tx_key, keys.session_id);

        // `tx_key` is consumed only by `shared_local_pool` above.
        // Copying it into a struct field would be dead storage and
        // a cross-pool footgun (see the `tx_key` comment on the
        // struct above).
        Self {
            session_id: keys.session_id,
            peer_addr,
            rx_cipher,
            streams: DashMap::new(),
            last_activity: AtomicU64::new(current_timestamp()),
            thread_local_pool,
            default_reliable,
            active: AtomicBool::new(true),
            stream_epoch_counter: AtomicU64::new(1),
            recently_closed: DashMap::new(),
            control_tx_seq: AtomicU64::new(0),
            cached_node_id: AtomicU64::new(0),
            // Unlike `tx_key`, the route-hop keys ARE retained: a
            // relay MACs every forwarded hop, and a MAC has no
            // nonce-reuse hazard to route around — the sequence is an
            // explicit transcript field, not derived counter state.
            route_hop_tx_key: keys.route_hop_tx_key,
            route_hop_rx_key: keys.route_hop_rx_key,
            route_hop_tx_seq: AtomicU64::new(0),
            route_hop_replay: SharedHopReplayWindow::new(),
        }
    }

    /// Wrap `inner` in an authenticated route-hop envelope for this
    /// edge, writing into a caller-owned buffer
    /// (SUBNET_AUTH_PLAN.md D6).
    ///
    /// The sequence is this edge's own, independent of the packet
    /// AEAD counter, so hop accounting can never disturb the
    /// end-to-end session being carried.
    ///
    /// This is the form the forwarding path uses: the buffer belongs
    /// to the forwarder and is reused across packets, so relaying does
    /// not allocate. Size it with
    /// [`route_hop::sealed_len`](crate::route_hop::sealed_len).
    ///
    /// A too-small buffer is refused *before* a sequence is taken —
    /// burning one on a local sizing mistake would open a gap in this
    /// edge's sequence space for no reason.
    pub fn seal_route_hop_into(
        &self,
        out: &mut [u8],
        header: &crate::route_codec::RoutingHeader,
        inner: &[u8],
    ) -> Result<usize, crate::route_hop::RouteHopError> {
        if out.len() < crate::route_hop::sealed_len(inner.len()) {
            return Err(crate::route_hop::RouteHopError::BufferTooSmall);
        }
        let seq = self.route_hop_tx_seq.fetch_add(1, Ordering::Relaxed);
        crate::route_hop::seal_into(
            out,
            &self.route_hop_tx_key,
            self.session_id,
            seq,
            header,
            inner,
        )
    }

    /// Allocating form of [`Self::seal_route_hop_into`], for callers
    /// off the forwarding path.
    pub fn seal_route_hop(&self, header: &crate::route_codec::RoutingHeader, inner: &[u8]) -> Vec<u8> {
        let seq = self.route_hop_tx_seq.fetch_add(1, Ordering::Relaxed);
        crate::route_hop::seal(&self.route_hop_tx_key, self.session_id, seq, header, inner)
    }

    /// Verify an inbound route-hop envelope and admit its sequence
    /// exactly once.
    ///
    /// Returns the opened hop on success. A bad tag is rejected before
    /// the replay window is touched, so a forged packet cannot burn a
    /// sequence slot the legitimate peer still needs.
    pub fn open_route_hop<'a>(
        &self,
        buf: &'a [u8],
    ) -> Result<crate::route_hop::OpenedHop<'a>, crate::route_hop::RouteHopError>
    {
        let opened = crate::route_hop::open(&self.route_hop_rx_key, buf)?;
        self.route_hop_replay.admit(opened.hop_sequence)?;
        Ok(opened)
    }

    /// Read the cached peer `NodeId` resolution. Returns `None`
    /// until the dispatcher's first resolution call publishes a
    /// value via [`Self::cache_node_id`]. See the field doc for
    /// the perf rationale.
    #[inline]
    pub fn cached_node_id(&self) -> Option<u64> {
        match self.cached_node_id.load(Ordering::Relaxed) {
            0 => None,
            n => Some(n),
        }
    }

    /// Publish the resolved peer `NodeId` for subsequent calls.
    /// Idempotent — concurrent first-resolution callers all write
    /// the same value (the resolver is deterministic for a given
    /// `session_id`), so a `store` over an existing identical value
    /// is correct. Callers should pass non-zero `node_id`; `0` is
    /// the reserved "unresolved" sentinel and is a no-op.
    #[inline]
    pub fn cache_node_id(&self, node_id: u64) {
        if node_id != 0 {
            self.cached_node_id.store(node_id, Ordering::Relaxed);
        }
    }

    /// Allocate the next sequence number for a subprotocol control
    /// packet. Uses a session-level counter separate from any
    /// user stream's sequence space — see `CONTROL_STREAM_ID`.
    #[inline]
    pub fn next_control_tx_seq(&self) -> u64 {
        self.control_tx_seq.fetch_add(1, Ordering::Relaxed)
    }

    /// Allocate a unique epoch for a freshly-opened stream.
    ///
    /// Monotonic per session — a stream closed and reopened gets a
    /// **new** epoch, which is how stale `Stream` handles and
    /// `TxSlotGuard`s are prevented from operating on a different
    /// lifetime of the same `stream_id`.
    #[inline]
    fn next_stream_epoch(&self) -> u64 {
        self.stream_epoch_counter.fetch_add(1, Ordering::Relaxed)
    }

    /// Get the session ID
    #[inline]
    pub fn session_id(&self) -> u64 {
        self.session_id
    }

    /// Get the peer address
    #[inline]
    pub fn peer_addr(&self) -> SocketAddr {
        self.peer_addr
    }

    // No `tx_key()` accessor exists — it would be a public
    // footgun with no legitimate callers. Any caller using
    // `session.tx_key()` to construct a fresh `PacketBuilder`
    // would re-introduce a cross-pool nonce-reuse hazard
    // (independent counters under the same ChaCha20-Poly1305 key).
    // All TX-side AEAD operations flow through `thread_local_pool`
    // via `build_heartbeat` and the normal `send_*` paths.

    /// Get the RX cipher
    #[inline]
    pub fn rx_cipher(&self) -> &PacketCipher {
        &self.rx_cipher
    }

    /// Get or create stream state
    pub fn get_or_create_stream(
        &self,
        stream_id: u64,
    ) -> dashmap::mapref::one::RefMut<'_, u64, StreamState> {
        self.streams
            .entry(stream_id)
            .or_insert_with(|| StreamState::new(self.default_reliable))
    }

    /// Like [`Self::get_or_create_stream`], but the receiver-side stream
    /// is created reliable when the arriving packet is `RELIABLE`-flagged
    /// — the sender's reliability is a property of the traffic, not of
    /// the receiver's `default_reliable`. Without this the auto-created
    /// receive stream is `FireAndForget` and never builds a NACK, so a
    /// reliable sender's lost packets are unrecoverable. Only affects the
    /// reliability mode at first-touch (creation); an existing stream
    /// keeps its mode.
    pub fn get_or_create_stream_for_packet(
        &self,
        stream_id: u64,
        reliable: bool,
    ) -> dashmap::mapref::one::RefMut<'_, u64, StreamState> {
        self.streams
            .entry(stream_id)
            .or_insert_with(|| StreamState::new(reliable))
    }

    /// Collect retransmit descriptors for every reliable stream whose
    /// oldest unacked packet has exceeded its RTO. Drives the timeout
    /// backstop (STREAM_RETRANSMIT D-4) that recovers tail loss — the
    /// last packets dropped, with no later arrival to trigger a
    /// receiver NACK. Each call advances the per-packet retry clock, so
    /// a descriptor isn't re-emitted until another RTO elapses, and a
    /// packet past `max_retries` is dropped from the window.
    pub fn collect_timed_out_retransmits(&self) -> Vec<Arc<RetransmitDescriptor>> {
        let mut out = Vec::new();
        for entry in self.streams.iter() {
            let mut due = entry.value().with_reliability(|r| r.get_timed_out());
            out.append(&mut due);
        }
        out
    }

    /// Collect the per-stream gap report for every stream that
    /// currently has a gap (H-4 + STREAM_ACK_BATCHING R-4), in ONE
    /// walk taking each stream's reliability lock exactly once.
    ///
    /// The proactive retransmit tick needs two things for a gapped
    /// stream — the legacy NACK and (for capable peers) the positive
    /// SACK ranges — and both derive from the same received-range
    /// index (`build_nack` and `build_ack_ranges` are non-empty under
    /// the identical `has_gaps()` condition). Snapshotting them under
    /// one lock keeps a tick's NACK and SACK for a stream mutually
    /// consistent and halves the tick's per-stream lock/DashMap cost
    /// versus the old two-walk shape (`collect_gap_nacks` +
    /// `collect_ack_ranges`).
    ///
    /// `ranges` is only built when `want_ranges` (the peer advertises
    /// the ack-ranges capability); otherwise it is left empty so a
    /// non-advertising peer pays nothing for the SACK build. Streams
    /// without gaps contribute nothing — the grant's piggybacked
    /// `ack_seq` already covers the contiguous case.
    pub fn collect_gap_reports(&self, want_ranges: bool, max_ranges: usize) -> Vec<GapReport> {
        let mut out = Vec::new();
        for entry in self.streams.iter() {
            let report = entry.value().with_reliability(|r| {
                r.build_nack().map(|nack| {
                    let ranges = if want_ranges {
                        r.build_ack_ranges(max_ranges)
                    } else {
                        Vec::new()
                    };
                    (nack, r.rx_ack_seq(), ranges)
                })
            });
            if let Some((nack, ack_seq, ranges)) = report {
                out.push(GapReport {
                    stream_id: *entry.key(),
                    nack,
                    ack_seq,
                    ranges,
                });
            }
        }
        out
    }

    /// Take-and-clear the "given up" flag across all streams, returning
    /// the ids of streams whose reliable layer exhausted retransmits on
    /// some packet (H-3). The caller signals a reset to the peer so the
    /// receiver fails fast instead of stalling to a timeout.
    pub fn take_failed_stream_ids(&self) -> Vec<u64> {
        let mut out = Vec::new();
        for entry in self.streams.iter() {
            if entry.value().with_reliability(|r| r.take_failed()) {
                out.push(*entry.key());
            }
        }
        out
    }

    /// Look up stream state without creating it. Returns `None` if the
    /// stream was never opened or has been closed.
    pub fn try_stream(
        &self,
        stream_id: u64,
    ) -> Option<dashmap::mapref::one::Ref<'_, u64, StreamState>> {
        self.streams.get(&stream_id)
    }

    /// Try to acquire `bytes` of send credit on `stream_id` with RAII
    /// refund semantics.
    ///
    /// Returns:
    ///   * [`TxAdmit::Acquired`] with a [`TxSlotGuard`] that refunds
    ///     `bytes` back to `tx_credit_remaining` when dropped —
    ///     including on async cancellation, panic, and early return —
    ///     unless the caller invokes [`TxSlotGuard::commit`] to
    ///     suppress the refund after a successful socket send. This
    ///     is the cure for the credit-leak that a plain "decrement /
    ///     await / maybe-refund" shape would hit when the sending
    ///     future is dropped mid-`.await` (e.g., `tokio::select!`
    ///     cancel).
    ///   * [`TxAdmit::WindowFull`] if `tx_credit_remaining` is below
    ///     `bytes`. `backpressure_events` has already been bumped.
    ///   * [`TxAdmit::StreamClosed`] if the stream isn't registered
    ///     (never opened, closed, or idle-evicted).
    pub fn try_acquire_tx_credit_guard(self: &Arc<Self>, stream_id: u64, bytes: u32) -> TxAdmit {
        self.try_acquire_tx_credit_inner(stream_id, None, bytes)
    }

    /// Like [`Self::try_acquire_tx_credit_guard`], but additionally
    /// rejects the admission if the live `StreamState`'s epoch
    /// differs from `expected_epoch`.
    ///
    /// Use from the typed-handle `send_on_stream` path so a handle
    /// held across a close+reopen cycle doesn't admit against the new
    /// stream's state.
    pub fn try_acquire_tx_credit_matching_epoch(
        self: &Arc<Self>,
        stream_id: u64,
        expected_epoch: u64,
        bytes: u32,
    ) -> TxAdmit {
        self.try_acquire_tx_credit_inner(stream_id, Some(expected_epoch), bytes)
    }

    #[expect(
        clippy::expect_used,
        reason = "seq is set Some on every code path that reaches the Acquired branch; the if-admitted flow guarantees this"
    )]
    fn try_acquire_tx_credit_inner(
        self: &Arc<Self>,
        stream_id: u64,
        expected_epoch: Option<u64>,
        bytes: u32,
    ) -> TxAdmit {
        // Look up the stream and do admission + sequence allocation
        // under ONE DashMap lookup. Splitting these into two lookups
        // would allow a close+reopen race in between — credit would
        // debit the old state while the sequence came from the new
        // state, cross-contaminating accounting across lifetimes and
        // defeating the epoch guard.
        //
        // Capture the state's epoch so the guard's Drop knows whether
        // the stream has been reopened in the interim (naive refund
        // would credit back bytes on the fresh state, which never
        // saw this acquire).
        //
        // Release the DashMap ref before returning so the guard's
        // Drop doesn't deadlock trying to re-acquire it.
        let (admitted, epoch, seq) = match self.streams.get(&stream_id) {
            None => return TxAdmit::StreamClosed,
            Some(state) => {
                // Cache `epoch` once — pre-fix [perf #42 in
                // `docs/internal/performance/net-perf-analysis.md`] the field
                // was read twice through the `Ref`, once for the
                // epoch-mismatch check and once on the return tuple.
                // Trivial field access today but the cache also makes
                // it obvious that both checks observe the same
                // snapshot (rather than reading mid-mutation between
                // the two reads — a defensive read against a future
                // change that makes `epoch` mutable under `&self`).
                let current_epoch = state.epoch();
                if let Some(expected) = expected_epoch {
                    if current_epoch != expected {
                        // The handle is stale: the stream was closed
                        // and reopened since the handle was issued.
                        // Surface this as StreamClosed so the caller
                        // maps it to `StreamError::NotConnected`.
                        return TxAdmit::StreamClosed;
                    }
                }
                let admitted = state.try_acquire_tx_credit(bytes);
                // Only consume a sequence if admission succeeded —
                // otherwise we'd waste sequence numbers on rejected
                // sends.
                let seq = if admitted {
                    Some(state.next_tx_seq())
                } else {
                    None
                };
                (admitted, current_epoch, seq)
            }
        };
        if !admitted {
            return TxAdmit::WindowFull;
        }
        TxAdmit::Acquired {
            guard: TxSlotGuard {
                session: Arc::clone(self),
                stream_id,
                epoch,
                bytes,
                active: true,
            },
            seq: seq.expect("seq is Some when admitted is true"),
        }
    }

    /// Roll back a TX sequence allocated by
    /// [`Self::try_acquire_tx_credit_matching_epoch`] when the packet it
    /// was minted for never reached the wire (scheduler/socket
    /// backpressure after the seq was consumed). Guarded by `epoch` so a
    /// close+reopen race can't roll back a sequence on a fresh stream
    /// state that never issued it — the exact discipline
    /// [`TxSlotGuard::drop`] uses for the byte-credit refund.
    ///
    /// Returns `true` if the sequence was reclaimed (it was the most-
    /// recently-issued seq and no concurrent send raced ahead), leaving
    /// no receiver-visible gap; `false` otherwise.
    pub fn try_rollback_tx_seq(self: &Arc<Self>, stream_id: u64, epoch: u64, seq: u64) -> bool {
        if let Some(state) = self.try_stream(stream_id) {
            if state.epoch() == epoch {
                return state.try_rollback_tx_seq(seq);
            }
        }
        false
    }
}

/// Outcome of [`NetSession::try_acquire_tx_credit_matching_epoch`].
#[derive(Debug)]
pub enum TxAdmit {
    /// Admission succeeded; the guard holds the credit until dropped
    /// or committed. `seq` was allocated under the same DashMap
    /// lookup as the credit acquire — credit and sequence are
    /// guaranteed to belong to the same `StreamState` lifetime.
    Acquired {
        /// RAII credit holder.
        guard: TxSlotGuard,
        /// Sequence number for this send, allocated atomically with
        /// the admission decision.
        seq: u64,
    },
    /// `tx_credit_remaining` was below the requested bytes. The
    /// `backpressure_events` counter was incremented as a side effect.
    WindowFull,
    /// The stream isn't currently open on this session.
    StreamClosed,
}

/// RAII guard holding a byte credit acquired from a stream's
/// `tx_credit_remaining`.
///
/// On `Drop` without a preceding [`Self::commit`], the guard re-looks
/// up the stream and refunds the credit — the intended slot never
/// made it onto the wire (socket send cancelled, early return,
/// panic). After a successful socket send the caller must invoke
/// `commit()` so the bytes stay consumed; the receiver will replenish
/// them via a `StreamWindow` grant.
///
/// If the stream was closed and reopened before the guard drops, the
/// refund is suppressed — the credit belonged to a state that no
/// longer exists.
pub struct TxSlotGuard {
    session: Arc<NetSession>,
    stream_id: u64,
    /// Epoch of the `StreamState` that admitted this guard.
    epoch: u64,
    /// Byte credit this guard holds. Refunded on `Drop` unless
    /// [`Self::commit`] has cleared `active` first.
    bytes: u32,
    active: bool,
}

impl std::fmt::Debug for TxSlotGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TxSlotGuard")
            .field("stream_id", &format_args!("{:#x}", self.stream_id))
            .field("epoch", &self.epoch)
            .field("bytes", &self.bytes)
            .field("active", &self.active)
            .finish()
    }
}

impl TxSlotGuard {
    /// Which stream this guard is holding credit on.
    #[inline]
    pub fn stream_id(&self) -> u64 {
        self.stream_id
    }

    /// Bytes of credit this guard holds.
    #[inline]
    pub fn bytes(&self) -> u32 {
        self.bytes
    }

    /// Mark the send as committed. The guard's Drop will NOT refund —
    /// the bytes are now the receiver's to credit back via a
    /// `StreamWindow` grant.
    #[inline]
    pub fn commit(mut self) {
        self.active = false;
    }

    /// Consume the guard without refunding. Used by tests that want
    /// to simulate a leaked slot; production code should prefer
    /// `commit`.
    #[doc(hidden)]
    pub fn forget(mut self) {
        self.active = false;
    }
}

impl Drop for TxSlotGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        if let Some(state) = self.session.try_stream(self.stream_id) {
            // Only refund if the live state is the same state that
            // admitted us. After a close+reopen the new state has a
            // different epoch — refunding would spuriously credit
            // bytes on a slot we never acquired.
            if state.epoch() == self.epoch {
                state.refund_tx_credit(self.bytes);
            }
        }
    }
}

impl NetSession {
    /// Open a stream with an explicit reliability mode and fair-scheduler
    /// weight.
    ///
    /// Idempotent: if the stream already exists, this is a no-op and the
    /// caller's config is **ignored with a warning log** — the first open
    /// wins. Callers that want to change a stream's config must close +
    /// re-open it.
    pub fn open_stream_with(&self, stream_id: u64, reliable: bool, fairness_weight: u8) -> u64 {
        // Inherit `DEFAULT_STREAM_WINDOW_BYTES` so callers that go
        // through this convenience wrapper (notably `publish_to_peer`)
        // pick up v2 backpressure by default. Callers that want the
        // v1-style unbounded-queue behavior use `open_stream_full`
        // with `tx_window = 0` explicitly.
        self.open_stream_full(
            stream_id,
            reliable,
            fairness_weight,
            DEFAULT_STREAM_WINDOW_BYTES,
        )
    }

    /// Extended open that also sets the per-stream TX window for
    /// backpressure. `tx_window == 0` keeps the pre-backpressure
    /// behavior (unbounded local queue).
    ///
    /// Returns the epoch of the live `StreamState` for `stream_id` —
    /// either the fresh one created for a new stream, or the existing
    /// one if the stream is already open (first-open-wins). Callers
    /// embed this in their `Stream` handle so later sends can reject
    /// stale handles after close+reopen.
    pub fn open_stream_full(
        &self,
        stream_id: u64,
        reliable: bool,
        fairness_weight: u8,
        tx_window: u32,
    ) -> u64 {
        // First-open-wins: warn when a caller's config disagrees
        // with the live stream's. Shared by the read-probe hit and
        // the lost-creation-race Occupied arm below so both
        // occupied shapes keep the pre-§2.12 warning behavior.
        fn warn_if_config_conflicts(
            existing: &StreamState,
            stream_id: u64,
            reliable: bool,
            fairness_weight: u8,
            tx_window: u32,
        ) {
            if existing.reliable_mode() != reliable
                || existing.fairness_weight() != fairness_weight.max(1)
                || existing.tx_window() != tx_window
            {
                tracing::warn!(
                    stream_id = format!("{:#x}", stream_id),
                    existing_reliable = existing.reliable_mode(),
                    new_reliable = reliable,
                    existing_weight = existing.fairness_weight(),
                    new_weight = fairness_weight,
                    existing_tx_window = existing.tx_window(),
                    new_tx_window = tx_window,
                    "open_stream: ignoring conflicting config; first open wins"
                );
            }
        }

        // PERF_AUDIT §2.12 — read-only fast path. `publish_to_peer`
        // calls `open_stream_with` on every publish, but the stream
        // is almost always already open after the first call —
        // pre-fix this took a DashMap write `entry()` lock per
        // publish just to land in the Occupied arm and return the
        // existing epoch. The `get` probe holds a read lock so
        // concurrent opens on other streams don't contend.
        if let Some(existing_ref) = self.streams.get(&stream_id) {
            let existing = existing_ref.value();
            warn_if_config_conflicts(existing, stream_id, reliable, fairness_weight, tx_window);
            return existing.epoch();
        }
        // Slow path: stream missing. Take the write lock and
        // either create the entry or pick up a concurrent
        // creator's epoch on the race.
        use dashmap::mapref::entry::Entry;
        match self.streams.entry(stream_id) {
            Entry::Occupied(existing) => {
                // Lost the creation race to a concurrent opener —
                // same first-open-wins semantics (and the same
                // conflict warning) as the read-probe hit above.
                let existing = existing.get();
                warn_if_config_conflicts(existing, stream_id, reliable, fairness_weight, tx_window);
                existing.epoch()
            }
            Entry::Vacant(v) => {
                let epoch = self.next_stream_epoch();
                v.insert(StreamState::new_full_with_epoch(
                    reliable,
                    fairness_weight,
                    tx_window,
                    epoch,
                ));
                epoch
            }
        }
    }

    /// Close a stream: mark it inactive and remove its state.
    ///
    /// Idempotent — closing a non-existent stream is a no-op. After
    /// close, a subsequent `open_stream_with` creates a fresh stream.
    ///
    /// Also records `stream_id` in the grant-quarantine set so that
    /// any `StreamWindow` grant still in flight from a peer who was
    /// communicating with the just-closed lifetime is dropped rather
    /// than spuriously crediting a later reopen — see
    /// `GRANT_QUARANTINE_WINDOW` and [`Self::is_grant_quarantined`].
    pub fn close_stream(&self, stream_id: u64) {
        if let Some((_, state)) = self.streams.remove(&stream_id) {
            state.deactivate();
            self.recently_closed.insert(stream_id, SystemClock::now());
        }
    }

    /// Whether a `StreamWindow` grant for `stream_id` should be
    /// dropped because the stream was closed within
    /// `GRANT_QUARANTINE_WINDOW`. Lazily garbage-collects expired
    /// entries on call.
    pub fn is_grant_quarantined(&self, stream_id: u64) -> bool {
        let elapsed = match self.recently_closed.get(&stream_id) {
            Some(entry) => entry.value().elapsed(),
            None => return false,
        };
        if elapsed < GRANT_QUARANTINE_WINDOW {
            return true;
        }
        // Entry is past the window — clean it up so the map doesn't
        // grow with stale ids.
        self.recently_closed.remove(&stream_id);
        false
    }

    /// Remove streams whose `last_activity` is older than `max_idle`,
    /// keeping the active count at or below `max_streams` by LRU-evicting
    /// the oldest if still over cap. Returns the number of streams
    /// evicted. Called from the session owner's heartbeat loop.
    pub fn evict_idle_streams(
        &self,
        max_idle: Duration,
        max_streams: usize,
        reason_tag: &'static str,
    ) -> usize {
        let mut evicted = 0;
        let now = current_timestamp();
        let max_idle_ns = u64::try_from(max_idle.as_nanos()).unwrap_or(u64::MAX);

        // Pass 1: drop idle streams.
        let idle: Vec<u64> = self
            .streams
            .iter()
            .filter(|e| now.saturating_sub(e.value().last_activity_ns()) > max_idle_ns)
            .map(|e| *e.key())
            .collect();
        for sid in idle {
            if let Some((_, state)) = self.streams.remove(&sid) {
                state.deactivate();
                self.recently_closed.insert(sid, SystemClock::now());
                evicted += 1;
                tracing::debug!(
                    stream_id = format!("{:#x}", sid),
                    reason = reason_tag,
                    "stream evicted: idle timeout"
                );
            }
        }

        // Pass 2: if still over the cap, LRU-evict the oldest.
        //
        // The (key, last_activity) pair is captured in the same
        // iteration that selects the victim, then `remove_if`
        // re-checks the activity stamp atomically before
        // removing. If a concurrent `open_stream_full` reused the
        // same `stream_id` slot or `touch`-ed it between selection
        // and removal, the stamp differs and we skip the eviction
        // for this round (it'll be re-evaluated on the next sweep
        // if the cap is still exceeded). Pre-fix the iter then
        // remove pair was non-atomic, so a freshly-opened stream
        // could be torn down in the gap between selection and
        // removal — observed as "stream just opened, immediately
        // closed" in production logs.
        while self.streams.len() > max_streams {
            let oldest = self
                .streams
                .iter()
                .min_by_key(|e| e.value().last_activity_ns())
                .map(|e| (*e.key(), e.value().last_activity_ns()));
            match oldest {
                Some((sid, expected_activity_ns)) => {
                    let removed = self
                        .streams
                        .remove_if(&sid, |_, v| v.last_activity_ns() == expected_activity_ns);
                    match removed {
                        Some((_, state)) => {
                            state.deactivate();
                            self.recently_closed.insert(sid, SystemClock::now());
                            evicted += 1;
                            tracing::warn!(
                                stream_id = format!("{:#x}", sid),
                                reason = "cap_exceeded",
                                total_streams = self.streams.len(),
                                max_streams = max_streams,
                                "stream evicted: max_streams cap"
                            );
                        }
                        None => {
                            // The stream was touched / replaced
                            // between selection and removal. Pick a
                            // new victim on the next loop iteration.
                            // Bail if the cap is no longer exceeded,
                            // otherwise the loop terminates anyway.
                            continue;
                        }
                    }
                }
                None => break,
            }
        }

        // Piggyback on this idle-stream sweep: drop any
        // `recently_closed` entry whose insertion time is past
        // `GRANT_QUARANTINE_WINDOW`. Without this sweep,
        // `recently_closed` would only get GC'd by
        // `is_grant_quarantined`, which is called only when an
        // inbound `StreamWindow` grant arrives for that exact
        // `stream_id`. A long-lived peer that opens/closes many
        // distinct stream IDs (e.g., one short-lived stream per
        // RPC) and never receives a late grant for each closed
        // stream would accumulate one entry per closed stream
        // forever — N streams/sec → ~N×T entries after T seconds,
        // unbounded. The sweep itself is bounded by the existing
        // eviction cadence so there's no extra wakeup cost.
        self.recently_closed
            .retain(|_, inserted_at| inserted_at.elapsed() < GRANT_QUARANTINE_WINDOW);

        evicted
    }

    /// Get stream state (read-only)
    pub fn get_stream(
        &self,
        stream_id: u64,
    ) -> Option<dashmap::mapref::one::Ref<'_, u64, StreamState>> {
        self.streams.get(&stream_id)
    }

    /// Get the thread-local pool for zero-contention packet building
    #[inline]
    pub fn thread_local_pool(&self) -> &SharedLocalPool {
        &self.thread_local_pool
    }

    /// Build an AEAD-authenticated heartbeat packet for this session.
    ///
    /// Routes through `thread_local_pool` so the heartbeat shares
    /// its TX counter with data-path packets — heartbeats and data
    /// interleave cleanly on the wire, and the receiver's replay
    /// window admits them in either order.
    ///
    /// Wrapping heartbeat construction in this method removes the
    /// surface that would otherwise let callers build heartbeats
    /// with a fresh `PacketBuilder::new(&[0u8; 32], session_id)`,
    /// which (a) would use the wrong key so the receiver's AEAD
    /// verify would reject every heartbeat, and (b) would reuse
    /// counter=0 across successive heartbeats so the replay window
    /// would reject every heartbeat after the first.
    #[inline]
    pub fn build_heartbeat(&self) -> Bytes {
        self.thread_local_pool.get().build_heartbeat()
    }

    /// Verify an inbound heartbeat's AEAD tag against this session's
    /// RX cipher, commit the counter into the replay window, and
    /// refresh `last_activity`. Returns `true` if the packet was
    /// accepted; the session is mutated only on success.
    ///
    /// Verify and touch are fused into a single call so callers
    /// cannot get the order wrong (verify-then-touch, never the
    /// reverse) or forget to touch (which would defeat session
    /// idle-timeout for legitimate heartbeats).
    ///
    /// Source-address validation (legacy adapter: 1:1 source per
    /// session) and any post-accept observation (mesh:
    /// `failure_detector.heartbeat`) remain the caller's
    /// responsibility — those policies vary by adapter and don't
    /// belong inside the helper.
    ///
    /// Heartbeats MUST decrypt the AEAD tag rather than be fast-
    /// pathed through to `failure_detector.heartbeat` and
    /// `session.touch()` based on `is_heartbeat()` alone — without
    /// the decrypt step, an off-path attacker who observed the
    /// cleartext `session_id` and source UDP address could spoof
    /// heartbeats indefinitely.
    pub fn verify_and_touch_heartbeat(&self, parsed: &ParsedPacket) -> bool {
        // A heartbeat encrypts an empty payload, so the on-wire
        // ciphertext is exactly the 16-byte AEAD tag (see
        // `PacketBuilder::build_heartbeat`). Reject any other
        // length BEFORE invoking the cipher: the AEAD will
        // catch a length mismatch on its own, but a cheap
        // up-front check shortcuts a cleartext-flood attacker
        // who sends short / empty / oversized packets to drain
        // CPU on the decrypt path. ChaCha20-Poly1305 isn't
        // hugely expensive per packet, but the gate is free
        // and removes the cipher from the per-probe budget.
        if parsed.payload.len() != crate::protocol::TAG_SIZE {
            return false;
        }
        let aad = parsed.header.aad();
        let counter = u64::from_le_bytes(parsed.header.nonce[4..12].try_into().unwrap_or([0u8; 8]));
        // Per crypto-session perf #129, route through the
        // verify-only API: heartbeats encrypt an empty plaintext
        // to a 16-byte Poly1305 tag, and the legacy
        // `decrypt(...).is_err()` materialized that empty
        // plaintext into a fresh `Vec<u8>` per call only to drop
        // it. `verify` runs the AEAD tag check without producing
        // a plaintext buffer.
        if self
            .rx_cipher
            .verify(counter, &aad, &parsed.payload)
            .is_err()
        {
            return false;
        }
        // Per crypto-session perf #132: single-lock admit replaces
        // the legacy `is_valid_rx_counter` (pre-verify) +
        // `update_rx_counter` (post-verify) two-step. Heartbeat
        // replays now pay the AEAD verify before being rejected at
        // admit, but the AEAD verify on a 16-byte heartbeat is the
        // cheapest case of ChaCha20-Poly1305 and the saved Mutex
        // op per non-replay heartbeat (which dominates the rate at
        // healthy steady state) is the actual hot path.
        if !self.rx_cipher.try_admit_rx_counter(counter) {
            return false;
        }
        self.touch();
        true
    }

    /// Update last activity timestamp
    #[inline]
    pub fn touch(&self) {
        self.last_activity
            .store(current_timestamp(), Ordering::Release);
    }

    /// Nanoseconds since epoch of the last activity. Useful for
    /// tests / diagnostics that need to observe whether `touch`
    /// has been called.
    #[inline]
    pub fn last_activity_ns(&self) -> u64 {
        self.last_activity.load(Ordering::Acquire)
    }

    /// Check if session has timed out
    #[inline]
    pub fn is_timed_out(&self, timeout: Duration) -> bool {
        let last = self.last_activity.load(Ordering::Acquire);
        let now = current_timestamp();
        let timeout_ns = u64::try_from(timeout.as_nanos()).unwrap_or(u64::MAX);
        now.saturating_sub(last) > timeout_ns
    }

    /// Check if session is active
    #[inline]
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    /// Deactivate the session
    #[inline]
    pub fn deactivate(&self) {
        self.active.store(false, Ordering::Release);
    }

    /// Get all stream IDs
    pub fn stream_ids(&self) -> Vec<u64> {
        self.streams.iter().map(|r| *r.key()).collect()
    }

    /// Get the number of streams
    pub fn stream_count(&self) -> usize {
        self.streams.len()
    }

    /// `true` if any application stream is currently open on this
    /// session. Control-plane traffic rides a separate sequence space
    /// and is not counted. Used by the NAT-traversal direct-path
    /// upgrade's busy gate (`NAT_TRAVERSAL_V2_PLAN.md` C3): a session
    /// carrying live streams must not be swapped out from under them.
    pub fn has_open_streams(&self) -> bool {
        !self.streams.is_empty()
    }

    /// `true` if any stream on this session has unacked in-flight
    /// reliable data (a non-empty retransmit window). Walks the live
    /// streams and short-circuits on the first with pending packets.
    /// Companion to [`Self::has_open_streams`] for the upgrade busy
    /// gate — swapping the session would drop this in-flight data with
    /// no retransmit on the new session.
    pub fn has_unacked(&self) -> bool {
        self.streams
            .iter()
            .any(|entry| entry.value().with_reliability(|r| r.has_pending()))
    }
}

impl std::fmt::Debug for NetSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetSession")
            .field("session_id", &format!("{:016x}", self.session_id))
            .field("peer_addr", &self.peer_addr)
            .field("stream_count", &self.streams.len())
            .field("active", &self.active.load(Ordering::Relaxed))
            .finish()
    }
}

/// Per-stream state for multiplexing.
pub struct StreamState {
    /// Next sequence number to send
    tx_seq: AtomicU64,
    /// Last received sequence number
    rx_seq: AtomicU64,
    /// Reliability mode for this stream
    reliability: parking_lot::Mutex<Box<dyn ReliabilityMode>>,
    /// Inbound event queue (for poll_shard)
    inbound: SegQueue<StoredEvent>,
    /// Stream is active
    active: AtomicBool,
    /// Nanoseconds since epoch of the last activity (send or receive).
    /// Used by the session's idle-eviction sweep.
    last_activity: AtomicU64,
    /// Reliability mode this stream was created with. Stored so
    /// `open_stream` can warn when a caller re-opens with a different
    /// config (config is immutable for the stream's lifetime).
    reliable_mode: bool,
    /// Fair-scheduler quantum multiplier (1 = equal share).
    fairness_weight: u8,
    /// Configured initial credit window in **bytes** for this stream's
    /// send path. `0` disables backpressure entirely (v1 "unbounded"
    /// escape hatch). Non-zero: `tx_credit_remaining` starts here and
    /// is decremented on each socket send.
    tx_window: u32,
    /// Bytes of send credit the sender may still use on this stream
    /// before `send_on_stream` returns `StreamError::Backpressure`.
    /// Decremented on each socket send (atomic CAS). Recomputed
    /// authoritatively from `tx_bytes_sent - max_consumed_seen` on
    /// every inbound `StreamWindow` grant. When `tx_window == 0`,
    /// admission short-circuits and this counter is not consulted.
    tx_credit_remaining: AtomicU32,
    /// Cumulative bytes this sender has committed to the wire on
    /// this stream, across all lifetime credit acquisitions. Bumped
    /// when `try_acquire_tx_credit` admits; rolled back when a
    /// guard drops without commit (refund). The grant handler
    /// reconciles `tx_credit_remaining` against this and
    /// `max_consumed_seen`, so lost grants self-heal on the next
    /// grant arrival.
    tx_bytes_sent: AtomicU64,
    /// Highest `total_consumed` observed from the receiver on this
    /// stream. Monotonic — out-of-order / duplicate grants are
    /// ignored. Updated under CAS to protect the monotonicity
    /// invariant against concurrent grant-dispatch tasks.
    max_consumed_seen: AtomicU64,
    /// Number of `send_on_stream` calls that returned
    /// `StreamError::Backpressure` since this stream opened.
    backpressure_events: AtomicU64,
    /// Cumulative `StreamWindow` grants received on this stream
    /// (sender side). Does not count bytes — counts grant packets.
    credit_grants_received: AtomicU64,
    /// Cumulative `StreamWindow` grants emitted on this stream
    /// (receiver side). Counts grant packets, not bytes.
    credit_grants_sent: AtomicU64,
    /// Receive-side credit bookkeeping. See [`RxCreditState`].
    rx_credit: RxCreditState,
    /// Monotonic epoch issued by the owning `NetSession` at open time.
    /// Close + reopen of the same `stream_id` produces a fresh
    /// `StreamState` with a new epoch; stale `Stream` handles and
    /// `TxSlotGuard`s must fail an equality check against this value
    /// before acting on the state.
    ///
    /// `0` is the "no epoch recorded" sentinel for legacy paths
    /// (`get_or_create_stream`, `send_to_peer` / `send_routed`) that
    /// don't go through the typed handle API.
    epoch: u64,
}

/// Receive-side credit bookkeeping for the v2 round-trip window.
///
/// Tracks how much credit this receiver has extended to the sender
/// vs how much it has "consumed" (accepted off the wire).
///
/// **Accounting cadence:** this is receive-time accounting, NOT
/// application-drain accounting. Every accepted packet calls
/// [`Self::on_bytes_consumed`] from
/// the dispatch loop (`mesh.rs::process_local_packet`), which
/// bumps both `consumed` and `granted` by the on-wire byte
/// count. The "outstanding" credit (`granted - consumed`)
/// therefore stays pinned at the initial window — every byte
/// received is paired with a matching grant.
///
/// This shape exists to close the v1 io::Error-on-full-kernel-
/// buffer gap (a single serial sender used to run
/// `Transport(io::Error)` into a full kernel buffer). Per-stream
/// kernel-buffer protection comes from the round-trip grant
/// loop; per-application throttling comes from a separate
/// mechanism (per-shard queue-depth limits).
///
/// An earlier version of this docstring described a
/// threshold-emit pattern ("when outstanding dips below half
/// the window, a grant is emitted"). That description didn't
/// match the implementation and contradicted the v2 design
/// goal — it has been superseded by the description above.
///
/// `window_bytes` is the per-grant chunk size — also the size of the
/// sender's implicit initial window at open time. `0` disables
/// receive-side bookkeeping entirely (matches the "unbounded" sender
/// escape hatch).
pub struct RxCreditState {
    /// Total credit granted to the sender since stream open, including
    /// the implicit initial window. Saturating u64 — 2^64 bytes is
    /// ~18 exabytes, no realistic workload wraps.
    granted: AtomicU64,
    /// Total inbound bytes this receiver has accepted. Incremented on
    /// the receive path as packets land on this stream. Invariant:
    /// `consumed <= granted` (unless the sender overshoots the initial
    /// window before the first grant — recoverable transient).
    consumed: AtomicU64,
    /// Per-grant chunk size (bytes). Equal to the sender's initial
    /// window at open time. Used by the caller to size grant emission
    /// — see [`Self::on_bytes_consumed`]. `0` disables emission
    /// (the v1 unbounded escape hatch).
    window_bytes: u32,
}

impl RxCreditState {
    fn new(window_bytes: u32) -> Self {
        Self {
            // Prime `granted` with the implicit initial window —
            // matches the sender's starting `tx_credit_remaining`, so
            // the first `on_bytes_consumed` calls reduce "outstanding"
            // rather than go negative.
            granted: AtomicU64::new(window_bytes as u64),
            consumed: AtomicU64::new(0),
            window_bytes,
        }
    }

    /// Bytes of credit outstanding — what the sender believes it can
    /// still send before hitting backpressure, from this receiver's
    /// local view.
    #[inline]
    pub fn outstanding(&self) -> u64 {
        // Read `consumed` first, then `granted`. Paired with the
        // publication order in `on_bytes_consumed` (granted first,
        // then consumed), this guarantees `granted >= consumed`:
        // if our `consumed` load observes a writer's increment, the
        // writer's earlier `granted` increment is already visible to
        // our subsequent `granted` load. Pre-fix the loads ran in
        // the opposite order and `saturating_sub` masked transient
        // `consumed > granted` to zero, surfacing a false "no
        // outstanding bytes" reading to metrics during contention.
        let c = self.consumed.load(Ordering::Acquire);
        let g = self.granted.load(Ordering::Acquire);
        g.saturating_sub(c)
    }

    /// Total bytes consumed since stream open.
    #[inline]
    pub fn consumed(&self) -> u64 {
        self.consumed.load(Ordering::Acquire)
    }

    /// Total bytes granted (including the implicit initial window).
    #[inline]
    pub fn granted(&self) -> u64 {
        self.granted.load(Ordering::Acquire)
    }

    /// Per-grant chunk size this receiver extends.
    #[inline]
    pub fn window_bytes(&self) -> u32 {
        self.window_bytes
    }

    /// Record `bytes` consumed off the wire and return the receiver's
    /// new cumulative consumed-byte count, which the caller ships as
    /// the `total_consumed` field of an authoritative `StreamWindow`
    /// grant. Returns `None` when receive-side bookkeeping is
    /// disabled (`window_bytes == 0`).
    ///
    /// Authoritative grants are self-healing: each grant carries the
    /// receiver's full picture, so a single lost grant is reconciled
    /// by the next one. That's what keeps the sender's credit from
    /// permanently draining when data packets OR grants are dropped
    /// on the wire. One grant per inbound packet is the simplest
    /// cadence; on lossy links the receiver may emit more frequently,
    /// and a future enhancement can batch grants without changing
    /// the wire format.
    pub fn on_bytes_consumed(&self, bytes: u64) -> Option<u64> {
        if self.window_bytes == 0 {
            return None;
        }
        // The v2 design intentionally accounts at receive time
        // (not application-drain time) — see `mesh.rs:3110-3135`
        // ("Accounting runs at receive time (not drain time); this
        // closes the v1 gap where a single serial sender ran
        // `Transport(io::Error)` into a full kernel buffer"). The
        // credit window is for kernel-buffer protection, not
        // application-side throttling; the latter is provided by
        // per-shard queue-depth limits.
        //
        // Every call mints a matching grant of `bytes`, returning
        // the running cumulative consumed count for the caller to
        // ship as `total_consumed` in an authoritative
        // `StreamWindow` packet.
        //
        // Order matters: bump `granted` BEFORE `consumed` so a
        // concurrent `outstanding()` reader that observes the new
        // `consumed` is guaranteed to see the matching `granted`
        // bump as well. With the opposite order, the reader's
        // computation `granted - consumed` could transiently see
        // `consumed > granted` (saturated to zero), surfacing a
        // false "window drained" snapshot to metrics under
        // contention.
        self.granted.fetch_add(bytes, Ordering::AcqRel);
        let new_consumed = self.consumed.fetch_add(bytes, Ordering::AcqRel) + bytes;
        Some(new_consumed)
    }
}

impl std::fmt::Debug for RxCreditState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RxCreditState")
            .field("granted", &self.granted.load(Ordering::Relaxed))
            .field("consumed", &self.consumed.load(Ordering::Relaxed))
            .field("window_bytes", &self.window_bytes)
            .finish()
    }
}

impl StreamState {
    /// Create a new stream state
    pub fn new(reliable: bool) -> Self {
        Self::new_with_weight(reliable, 1)
    }

    /// Create a new stream state with a fair-scheduler weight.
    ///
    /// Uses [`DEFAULT_STREAM_WINDOW_BYTES`] for the initial credit
    /// window — auto-created receive-side streams (via
    /// `get_or_create_stream`) inherit the default so
    /// `RxCreditState` can mint grants on threshold crossings.
    /// Callers that need a specific window go through
    /// [`Self::new_full`].
    pub fn new_with_weight(reliable: bool, fairness_weight: u8) -> Self {
        Self::new_full(reliable, fairness_weight, DEFAULT_STREAM_WINDOW_BYTES)
    }

    /// Create a new stream state with full config (weight + tx window).
    /// Epoch defaults to `0` (the "no epoch" sentinel used by legacy
    /// auto-create paths); sessions that go through `open_stream_full`
    /// allocate a fresh epoch via [`Self::new_full_with_epoch`].
    pub fn new_full(reliable: bool, fairness_weight: u8, tx_window: u32) -> Self {
        Self::new_full_with_epoch(reliable, fairness_weight, tx_window, 0)
    }

    /// Create a new stream state with a caller-supplied epoch.
    ///
    /// Sessions call this via `open_stream_full` with a monotonic
    /// epoch; stale `Stream` handles / `TxSlotGuard`s from a prior
    /// close/reopen cycle will fail the epoch check against the new
    /// state.
    pub fn new_full_with_epoch(
        reliable: bool,
        fairness_weight: u8,
        tx_window: u32,
        epoch: u64,
    ) -> Self {
        // Size the retransmit window to the tx-credit window so the
        // sender can never have more packets in flight than it can
        // retransmit (H-1). Cheap: `pending` grows on demand, so a large
        // window costs no up-front memory.
        let max_pending = ReliableStream::max_pending_for_window(tx_window);
        Self {
            tx_seq: AtomicU64::new(0),
            rx_seq: AtomicU64::new(0),
            reliability: parking_lot::Mutex::new(create_reliability_mode(reliable, max_pending)),
            inbound: SegQueue::new(),
            active: AtomicBool::new(true),
            last_activity: AtomicU64::new(current_timestamp()),
            reliable_mode: reliable,
            fairness_weight: fairness_weight.max(1),
            tx_window,
            // Implicit initial window: the sender starts with full
            // credit so the first send doesn't eat a handshake round
            // trip.
            tx_credit_remaining: AtomicU32::new(tx_window),
            tx_bytes_sent: AtomicU64::new(0),
            max_consumed_seen: AtomicU64::new(0),
            backpressure_events: AtomicU64::new(0),
            credit_grants_received: AtomicU64::new(0),
            credit_grants_sent: AtomicU64::new(0),
            rx_credit: RxCreditState::new(tx_window),
            epoch,
        }
    }

    /// Refresh last-activity timestamp. Called on every send and on
    /// every receive that lands packets/events into the stream.
    #[inline]
    pub fn touch(&self) {
        self.last_activity
            .store(current_timestamp(), Ordering::Release);
    }

    /// Nanoseconds since epoch of the last activity.
    #[inline]
    pub fn last_activity_ns(&self) -> u64 {
        self.last_activity.load(Ordering::Acquire)
    }

    /// Reliability mode this stream was created with.
    #[inline]
    pub fn reliable_mode(&self) -> bool {
        self.reliable_mode
    }

    /// Fair-scheduler weight for this stream.
    #[inline]
    pub fn fairness_weight(&self) -> u8 {
        self.fairness_weight
    }

    /// Monotonic per-session epoch captured at construction time.
    /// `0` means "no epoch recorded" (legacy auto-create path).
    #[inline]
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Configured initial credit window in bytes. `0` means "no limit"
    /// — backpressure is disabled for this stream (v1 escape hatch).
    #[inline]
    pub fn tx_window(&self) -> u32 {
        self.tx_window
    }

    /// Current remaining send credit in bytes. Approaches `0` as the
    /// sender pushes packets without a corresponding receiver grant;
    /// the next acquire at `0` returns Backpressure.
    #[inline]
    pub fn tx_credit_remaining(&self) -> u32 {
        self.tx_credit_remaining.load(Ordering::Acquire)
    }

    /// Cumulative number of Backpressure rejections since the stream opened.
    #[inline]
    pub fn backpressure_events(&self) -> u64 {
        self.backpressure_events.load(Ordering::Relaxed)
    }

    /// Cumulative `StreamWindow` grants received on this stream.
    #[inline]
    pub fn credit_grants_received(&self) -> u64 {
        self.credit_grants_received.load(Ordering::Relaxed)
    }

    /// Cumulative `StreamWindow` grants emitted on this stream.
    #[inline]
    pub fn credit_grants_sent(&self) -> u64 {
        self.credit_grants_sent.load(Ordering::Relaxed)
    }

    /// Access the receive-side credit bookkeeping.
    #[inline]
    pub fn rx_credit(&self) -> &RxCreditState {
        &self.rx_credit
    }

    /// Try to acquire `bytes` of send credit via a CAS loop.
    ///
    /// Returns `true` on success — `tx_credit_remaining` is
    /// decremented and `tx_bytes_sent` is bumped so the
    /// authoritative-grant reconciliation sees a consistent view.
    /// Returns `false` when remaining credit is below `bytes`;
    /// caller returns `StreamError::Backpressure` and the rejection
    /// counter bumps.
    ///
    /// `tx_window == 0` disables the check; all requests admit and
    /// the counter is not touched.
    pub fn try_acquire_tx_credit(&self, bytes: u32) -> bool {
        if self.tx_window == 0 {
            return true;
        }
        loop {
            let cur = self.tx_credit_remaining.load(Ordering::Acquire);
            if cur < bytes {
                self.backpressure_events.fetch_add(1, Ordering::Relaxed);
                return false;
            }
            if self
                .tx_credit_remaining
                .compare_exchange_weak(cur, cur - bytes, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                // Bump the committed-bytes counter only after the
                // CAS wins. The reverse order (bump then CAS) lets
                // a concurrent grant observe the bumped watermark,
                // mint credit up to the window, and then the
                // pending admission's CAS subtracts that credit —
                // net loss of one unit per grant-vs-admission race.
                // The narrow truncation window the audit highlighted
                // (#97) is self-healing via the next grant; the
                // window-invariant violation in the alternative
                // ordering is not.
                self.tx_bytes_sent
                    .fetch_add(bytes as u64, Ordering::Relaxed);
                return true;
            }
            // CAS lost — retry with the fresh value.
        }
    }

    /// Refund `bytes` of send credit. Called by `TxSlotGuard::drop`
    /// when a previously acquired slot never made it to the wire
    /// (socket send cancelled, early return, etc.). Rolls back both
    /// `tx_credit_remaining` and the `tx_bytes_sent` bump recorded at
    /// admission — the bytes never left the sender, so neither
    /// counter should reflect them. No clamp at `tx_window`: grants
    /// may have pushed the counter past the initial window, and
    /// refunding those bytes back to a `tx_window` ceiling would
    /// strand legitimately-granted credit.
    pub fn refund_tx_credit(&self, bytes: u32) {
        if self.tx_window == 0 {
            return;
        }
        self.tx_credit_remaining
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |v| {
                Some(v.saturating_add(bytes))
            })
            .ok();
        self.tx_bytes_sent
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                Some(v.saturating_sub(bytes as u64))
            })
            .ok();
    }

    /// Attempt to roll back a TX sequence number that was allocated via
    /// [`Self::next_tx_seq`] but whose packet never reached the wire
    /// (e.g. the FairScheduler queue was full and `deliver_stream_packet`
    /// returned `Backpressure` *after* the seq was consumed). Unlike the
    /// byte credit — which `TxSlotGuard::drop` always refunds — the seq
    /// is a monotonic `fetch_add` counter, so a blind decrement is unsafe:
    /// a concurrent sender on the same stream may already have consumed
    /// `seq + 1`, and decrementing would re-issue that sender's sequence.
    ///
    /// We therefore roll back **only** via a CAS `seq + 1 -> seq`, which
    /// succeeds exactly when `seq` was the most-recently-issued sequence
    /// (the common case for the backpressure-on-the-last-flush scenario)
    /// and no other send has advanced the counter in between. Returns
    /// `true` if the rollback won the CAS (no gap left behind), `false`
    /// if another allocation raced ahead — in which case the gap is
    /// genuinely unavoidable and the reliable-stream retransmit/NACK
    /// machinery must recover it instead.
    pub fn try_rollback_tx_seq(&self, seq: u64) -> bool {
        self.tx_seq
            .compare_exchange(
                seq.wrapping_add(1),
                seq,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// Apply a receiver grant reporting the receiver's **absolute**
    /// cumulative consumed-byte count on this stream. Monotonic —
    /// grants arriving with `total_consumed` below the already-observed
    /// maximum are treated as stale duplicates and only bump the
    /// `credit_grants_received` counter. Self-healing: a single lost
    /// grant is reconciled by the next one because each grant carries
    /// the receiver's full accounting.
    ///
    /// Reconciliation adds the **delta** of newly-acknowledged bytes
    /// (`total_consumed - prev_max_consumed`) to `tx_credit_remaining`
    /// via `fetch_update`. The additive form composes atomically with
    /// the CAS in `try_acquire_tx_credit` and the `fetch_update` in
    /// `refund_tx_credit`: every operation preserves the invariant
    /// `remaining + (sent - max_consumed) == window` regardless of
    /// interleaving. An earlier `.store()`-based implementation
    /// recomputed from a racy snapshot of `tx_bytes_sent`, which could
    /// silently overwrite a concurrent acquire's CAS result.
    pub fn apply_authoritative_grant(&self, total_consumed: u64) {
        self.credit_grants_received.fetch_add(1, Ordering::Relaxed);
        if self.tx_window == 0 {
            return;
        }
        // Clamp `total_consumed` to the sender-side `tx_bytes_sent`
        // watermark before the CAS. Without this, a malformed or
        // hostile grant carrying `total_consumed = u64::MAX` advanced
        // `max_consumed_seen` to MAX, and every subsequent honest
        // grant tripped the `total_consumed <= prev` early-return —
        // the stream stalled forever. The clamp is safe under honest
        // operation (a receiver can't have consumed bytes the sender
        // hasn't committed) and acts as a safety bound
        // otherwise.
        let sent_watermark = self.tx_bytes_sent.load(Ordering::Acquire);
        let total_consumed = total_consumed.min(sent_watermark);
        // Monotonic CAS update — the value advanced by the successful
        // CAS is the amount of newly-acknowledged bytes.
        let mut prev = self.max_consumed_seen.load(Ordering::Acquire);
        let delta = loop {
            if total_consumed <= prev {
                return; // stale / duplicate grant — ignore
            }
            match self.max_consumed_seen.compare_exchange_weak(
                prev,
                total_consumed,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break total_consumed - prev,
                Err(current) => prev = current,
            }
        };
        // Under honest receiver accounting
        // (`total_consumed <= tx_bytes_sent`) the delta is bounded by
        // the outstanding window, so `saturating_add` is a no-op
        // against overflow and the final value naturally stays at or
        // below `tx_window`.
        //
        // A malformed or buggy grant can report `total_consumed`
        // above what the sender has actually committed, which would
        // otherwise mint credit past the window and let the sender
        // exceed its configured ceiling. The `min(self.tx_window)`
        // clamp caps credit at the configured window regardless of
        // the reported delta — a safety bound, not a correctness
        // requirement under honest operation.
        let grant_add = delta.min(u32::MAX as u64) as u32;
        let window = self.tx_window;
        self.tx_credit_remaining
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |v| {
                Some(v.saturating_add(grant_add).min(window))
            })
            .ok();
    }

    /// Cumulative bytes committed to the wire on this stream.
    /// Admission bumps it; uncommitted-guard drops roll it back.
    #[inline]
    pub fn tx_bytes_sent(&self) -> u64 {
        self.tx_bytes_sent.load(Ordering::Relaxed)
    }

    /// Highest `total_consumed` this sender has observed from the
    /// receiver on this stream. Monotonic.
    #[inline]
    pub fn max_consumed_seen(&self) -> u64 {
        self.max_consumed_seen.load(Ordering::Acquire)
    }

    /// Record that the receiver side has accepted `bytes` off the
    /// wire on this stream. Returns `Some(total_consumed)` — the
    /// receiver's new cumulative consumed count — so the caller can
    /// emit an authoritative `StreamWindow` grant. Returns `None`
    /// when receive-side bookkeeping is disabled (`window_bytes == 0`).
    pub fn on_bytes_consumed(&self, bytes: u64) -> Option<u64> {
        self.rx_credit.on_bytes_consumed(bytes)
    }

    /// Increment the "grants emitted" counter. Called after a grant
    /// packet has been successfully handed to the socket send path.
    #[inline]
    pub fn note_grant_sent(&self) {
        self.credit_grants_sent.fetch_add(1, Ordering::Relaxed);
    }

    /// Get and increment the TX sequence number. Refreshes `last_activity`.
    #[inline]
    pub fn next_tx_seq(&self) -> u64 {
        self.touch();
        self.tx_seq.fetch_add(1, Ordering::Relaxed)
    }

    /// Get the current TX sequence number
    #[inline]
    pub fn current_tx_seq(&self) -> u64 {
        self.tx_seq.load(Ordering::Relaxed)
    }

    /// Update the RX sequence number. Refreshes `last_activity`.
    #[inline]
    pub fn update_rx_seq(&self, seq: u64) {
        self.touch();
        self.rx_seq.fetch_max(seq, Ordering::Relaxed);
    }

    /// Get the current RX sequence number
    #[inline]
    pub fn current_rx_seq(&self) -> u64 {
        self.rx_seq.load(Ordering::Relaxed)
    }

    /// Access the reliability mode
    #[inline]
    pub fn with_reliability<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut Box<dyn ReliabilityMode>) -> R,
    {
        let mut guard = self.reliability.lock();
        f(&mut guard)
    }

    /// Push an event to the inbound queue
    #[inline]
    pub fn push_event(&self, event: StoredEvent) {
        self.inbound.push(event);
    }

    /// Pop an event from the inbound queue
    #[inline]
    pub fn pop_event(&self) -> Option<StoredEvent> {
        self.inbound.pop()
    }

    /// Get the number of pending inbound events
    #[inline]
    pub fn inbound_len(&self) -> usize {
        self.inbound.len()
    }

    /// Check if stream is active
    #[inline]
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    /// Deactivate the stream
    #[inline]
    pub fn deactivate(&self) {
        self.active.store(false, Ordering::Release);
    }
}

impl std::fmt::Debug for StreamState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamState")
            .field("tx_seq", &self.tx_seq.load(Ordering::Relaxed))
            .field("rx_seq", &self.rx_seq.load(Ordering::Relaxed))
            .field("inbound_len", &self.inbound.len())
            .field("active", &self.active.load(Ordering::Relaxed))
            .finish()
    }
}

/// Session manager for handling multiple sessions.
///
/// Currently supports single-peer operation, but designed for
/// future multi-peer extension.
pub struct SessionManager {
    /// Current session (single-peer mode)
    session: parking_lot::RwLock<Option<Arc<NetSession>>>,
    /// Session timeout
    timeout: Duration,
}

impl SessionManager {
    /// Create a new session manager
    pub fn new(timeout: Duration) -> Self {
        Self {
            session: parking_lot::RwLock::new(None),
            timeout,
        }
    }

    /// Set the current session
    pub fn set_session(&self, session: NetSession) {
        let mut guard = self.session.write();
        *guard = Some(Arc::new(session));
    }

    /// Set the current session from an existing Arc
    pub fn set_session_arc(&self, session: Arc<NetSession>) {
        let mut guard = self.session.write();
        *guard = Some(session);
    }

    /// Get the current session
    pub fn get_session(&self) -> Option<Arc<NetSession>> {
        self.session.read().clone()
    }

    /// Clear the current session
    pub fn clear_session(&self) {
        let mut guard = self.session.write();
        if let Some(session) = guard.take() {
            session.deactivate();
        }
    }

    /// Check if there's an active session
    pub fn has_session(&self) -> bool {
        self.session.read().is_some()
    }

    /// Check session health and clean up if timed out
    pub fn check_session(&self) -> bool {
        let guard = self.session.read();
        if let Some(session) = guard.as_ref() {
            if session.is_timed_out(self.timeout) {
                drop(guard);
                self.clear_session();
                return false;
            }
            session.is_active()
        } else {
            false
        }
    }
}

impl std::fmt::Debug for SessionManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionManager")
            .field("has_session", &self.has_session())
            .field("timeout", &self.timeout)
            .finish()
    }
}

use crate::time::current_timestamp;


