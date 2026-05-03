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

use std::time::Instant;

use crate::event::StoredEvent;

use super::crypto::{PacketCipher, SessionKeys};
// `SharedPacketPool` is intentionally absent — `NetSession` uses
// only `SharedLocalPool` as the single TX-side AEAD source.
use super::pool::SharedLocalPool;
use super::reliability::{create_reliability_mode, ReliabilityMode};
use super::stream::DEFAULT_STREAM_WINDOW_BYTES;
use super::transport::ParsedPacket;

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
            super::pool::shared_local_pool(pool_size, &keys.tx_key, keys.session_id);

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
                if let Some(expected) = expected_epoch {
                    if state.epoch() != expected {
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
                (admitted, state.epoch(), seq)
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
        use dashmap::mapref::entry::Entry;
        match self.streams.entry(stream_id) {
            Entry::Occupied(existing) => {
                let existing = existing.get();
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
            self.recently_closed.insert(stream_id, Instant::now());
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
                self.recently_closed.insert(sid, Instant::now());
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
                            self.recently_closed.insert(sid, Instant::now());
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
        if parsed.payload.len() != super::protocol::TAG_SIZE {
            return false;
        }
        let aad = parsed.header.aad();
        let counter = u64::from_le_bytes(parsed.header.nonce[4..12].try_into().unwrap_or([0u8; 8]));
        if !self.rx_cipher.is_valid_rx_counter(counter) {
            return false;
        }
        if self
            .rx_cipher
            .decrypt(counter, &aad, &parsed.payload)
            .is_err()
        {
            return false;
        }
        if !self.rx_cipher.update_rx_counter(counter) {
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
        Self {
            tx_seq: AtomicU64::new(0),
            rx_seq: AtomicU64::new(0),
            reliability: parking_lot::Mutex::new(create_reliability_mode(reliable)),
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

use super::current_timestamp;

#[cfg(test)]
mod heartbeat_api_drift_check {
    //! Tripwire for the heartbeat-unification invariant: every
    //! production-side caller in `mod.rs` and `mesh.rs` that
    //! constructs a heartbeat must go through
    //! [`NetSession::build_heartbeat`]. See
    //! `docs/HEARTBEAT_UNIFICATION_PLAN.md` Step 4.
    //!
    //! `PacketBuilder::new` is `pub(crate)` so the type system
    //! already forbids external callers. Within the crate,
    //! though, a future contributor could legitimately add a new
    //! production caller that reaches into the pool directly
    //! (`session.thread_local_pool().get().build_heartbeat()`)
    //! and bypass the session helper — that pattern was the bug
    //! shape behind #97/#106. This test counts the approved
    //! production call sites and fails if a new one appears
    //! without an explicit allowlist update, forcing the
    //! contributor to confirm the design choice.
    //!
    //! The test scans only the *production* prefixes of each
    //! file (everything before the first column-0
    //! `#[cfg(test)]`), which excludes the test modules where
    //! `PacketBuilder::new(&keys.tx_key, ...)` is the canonical
    //! way to build a heartbeat for a manually-constructed
    //! peer session.

    fn production_prefix(src: &str) -> &str {
        // Top-level test modules are tagged with a column-0
        // `#[cfg(test)]` immediately followed by `mod`. Find the
        // first such marker and treat everything before it as
        // production code. Nested `#[cfg(test)]` mods (indented
        // inside an `impl` or inline `mod` block) are NOT cut
        // here, so production code in those files that follows
        // a nested test mod is still checked. False positives
        // from nested-test-mod content are unlikely because none
        // of the nested test mods in this codebase reference
        // `build_heartbeat`.
        let needle = "\n#[cfg(test)]\nmod ";
        match src.find(needle) {
            Some(idx) => &src[..idx],
            None => src,
        }
    }

    fn count_build_heartbeat_callers(src: &str) -> Vec<String> {
        src.lines()
            .filter(|line| {
                let trimmed = line.trim_start();
                // Skip comments / doc-comments.
                if trimmed.starts_with("//") {
                    return false;
                }
                line.contains(".build_heartbeat(")
            })
            .map(|line| line.trim().to_string())
            .collect()
    }

    #[test]
    fn mod_rs_production_callers_match_allowlist() {
        let prod = production_prefix(include_str!("mod.rs"));
        let callers = count_build_heartbeat_callers(prod);
        // The only approved production caller in mod.rs:
        //   `let packet = session.build_heartbeat();`
        // inside `spawn_heartbeat`. Pre-fix this read
        //   `let packet = pooled.build_heartbeat();`
        // — that pattern is the regression we want to catch.
        let approved = ["let packet = session.build_heartbeat();"];
        assert_eq!(
            callers,
            approved.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            "mod.rs production callers of `.build_heartbeat()` drifted from the \
             approved allowlist. If you intentionally added a new caller, route it \
             through `Session::build_heartbeat` and update this allowlist. \
             See docs/HEARTBEAT_UNIFICATION_PLAN.md."
        );
    }

    #[test]
    fn mesh_rs_production_callers_match_allowlist() {
        let prod = production_prefix(include_str!("mesh.rs"));
        let callers = count_build_heartbeat_callers(prod);
        let approved = ["let packet = session.build_heartbeat();"];
        assert_eq!(
            callers,
            approved.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            "mesh.rs production callers of `.build_heartbeat()` drifted from the \
             approved allowlist. If you intentionally added a new caller, route it \
             through `Session::build_heartbeat` and update this allowlist. \
             See docs/HEARTBEAT_UNIFICATION_PLAN.md."
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_keys() -> SessionKeys {
        SessionKeys {
            tx_key: [0x42u8; 32],
            rx_key: [0x24u8; 32],
            session_id: 0x1234567890ABCDEF,
            // Zero-filled sentinel — this helper bypasses the Noise
            // handshake, so there is no real X25519 peer key to
            // surface. `MeshNode::peer_static_x25519` treats zeros
            // as "not available" and returns `None`.
            remote_static_pub: [0u8; 32],
        }
    }

    #[test]
    fn test_session_creation() {
        let keys = test_keys();
        let peer_addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();

        let session = NetSession::new(keys.clone(), peer_addr, 4, false);

        assert_eq!(session.session_id(), keys.session_id);
        assert_eq!(session.peer_addr(), peer_addr);
        assert!(session.is_active());
        assert_eq!(session.stream_count(), 0);
    }

    #[test]
    fn test_stream_state() {
        let stream = StreamState::new(false);

        // TX sequence
        assert_eq!(stream.next_tx_seq(), 0);
        assert_eq!(stream.next_tx_seq(), 1);
        assert_eq!(stream.current_tx_seq(), 2);

        // RX sequence
        stream.update_rx_seq(5);
        assert_eq!(stream.current_rx_seq(), 5);
        stream.update_rx_seq(3); // Lower value ignored
        assert_eq!(stream.current_rx_seq(), 5);

        // Inbound queue
        let event = StoredEvent::from_value("1".into(), serde_json::json!({"test": 1}), 100, 0);
        stream.push_event(event);
        assert_eq!(stream.inbound_len(), 1);

        let popped = stream.pop_event().unwrap();
        assert_eq!(popped.id, "1");
        assert_eq!(stream.inbound_len(), 0);
    }

    #[test]
    fn test_session_streams() {
        let keys = test_keys();
        let peer_addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();

        let session = NetSession::new(keys, peer_addr, 4, false);

        // Create streams
        {
            let stream = session.get_or_create_stream(0);
            assert_eq!(stream.next_tx_seq(), 0);
        }

        {
            let stream = session.get_or_create_stream(1);
            assert_eq!(stream.next_tx_seq(), 0);
        }

        assert_eq!(session.stream_count(), 2);

        let ids = session.stream_ids();
        assert!(ids.contains(&0));
        assert!(ids.contains(&1));
    }

    #[test]
    fn test_session_timeout() {
        let keys = test_keys();
        let peer_addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();

        let session = NetSession::new(keys, peer_addr, 4, false);

        // Should not be timed out immediately
        assert!(!session.is_timed_out(Duration::from_secs(1)));

        // Touch and verify
        session.touch();
        assert!(!session.is_timed_out(Duration::from_secs(1)));
    }

    #[test]
    fn test_session_manager() {
        let manager = SessionManager::new(Duration::from_secs(30));

        assert!(!manager.has_session());

        let keys = test_keys();
        let peer_addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        let session = NetSession::new(keys, peer_addr, 4, false);

        manager.set_session(session);
        assert!(manager.has_session());

        let retrieved = manager.get_session().unwrap();
        assert!(retrieved.is_active());

        manager.clear_session();
        assert!(!manager.has_session());
    }

    #[test]
    fn test_open_stream_with_idempotent() {
        let keys = test_keys();
        let peer_addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        let session = NetSession::new(keys, peer_addr, 4, false);

        // First open creates state.
        session.open_stream_with(42, true, 3);
        assert_eq!(session.stream_count(), 1);
        let state = session.get_stream(42).unwrap();
        assert!(state.reliable_mode());
        assert_eq!(state.fairness_weight(), 3);
        drop(state);

        // Second open with matching config is a no-op.
        session.open_stream_with(42, true, 3);
        assert_eq!(session.stream_count(), 1);

        // Second open with DIFFERENT config is also a no-op
        // (first open wins). We log a warning but don't mutate.
        session.open_stream_with(42, false, 7);
        let state = session.get_stream(42).unwrap();
        assert!(
            state.reliable_mode(),
            "first open wins — reliable still true"
        );
        assert_eq!(
            state.fairness_weight(),
            3,
            "first open wins — weight still 3"
        );
    }

    #[test]
    fn test_close_stream_removes_state() {
        let keys = test_keys();
        let peer_addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        let session = NetSession::new(keys, peer_addr, 4, false);

        session.open_stream_with(1, false, 1);
        session.open_stream_with(2, true, 2);
        assert_eq!(session.stream_count(), 2);

        session.close_stream(1);
        assert_eq!(session.stream_count(), 1);
        assert!(session.get_stream(1).is_none());
        assert!(session.get_stream(2).is_some());

        // Closing a non-existent stream is a no-op.
        session.close_stream(99);
        assert_eq!(session.stream_count(), 1);

        // Re-open after close creates fresh state with new config.
        session.close_stream(2);
        session.open_stream_with(2, false, 5);
        let state = session.get_stream(2).unwrap();
        assert!(!state.reliable_mode());
        assert_eq!(state.fairness_weight(), 5);
    }

    #[test]
    fn test_evict_idle_streams_timeout_and_cap() {
        let keys = test_keys();
        let peer_addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        let session = NetSession::new(keys, peer_addr, 4, false);

        // Open three streams; touch only one so the other two look idle.
        session.open_stream_with(1, false, 1);
        session.open_stream_with(2, false, 1);
        session.open_stream_with(3, false, 1);
        std::thread::sleep(Duration::from_millis(10));
        session.get_or_create_stream(2).touch();

        // With a tight idle timeout, streams 1 and 3 should be evicted;
        // stream 2 was just touched so it survives.
        let evicted = session.evict_idle_streams(Duration::from_millis(5), usize::MAX, "test");
        assert_eq!(evicted, 2);
        assert_eq!(session.stream_count(), 1);
        assert!(session.get_stream(2).is_some());

        // Cap eviction: open two more streams so we have 3, then cap at 1.
        session.open_stream_with(4, false, 1);
        session.open_stream_with(5, false, 1);
        assert_eq!(session.stream_count(), 3);
        let evicted = session.evict_idle_streams(Duration::from_nanos(u64::MAX), 1, "test");
        assert_eq!(evicted, 2);
        assert_eq!(session.stream_count(), 1);
    }

    /// Regression for BUG_AUDIT_2026_04_30_CORE.md #105: pre-fix
    /// `recently_closed` only got garbage-collected by
    /// `is_grant_quarantined` on inbound `StreamWindow` grants.
    /// A peer churning short-lived streams without receiving a
    /// late grant for each accumulates one entry per closed
    /// stream forever. Post-fix `evict_idle_streams` also
    /// sweeps `recently_closed`, dropping entries past
    /// `GRANT_QUARANTINE_WINDOW`.
    #[test]
    fn evict_idle_streams_sweeps_recently_closed_past_quarantine_window() {
        let keys = test_keys();
        let peer_addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        let session = NetSession::new(keys, peer_addr, 4, false);

        // Manually pre-populate `recently_closed` with entries
        // whose timestamps are well past the quarantine window.
        let stale_inserted_at = Instant::now() - GRANT_QUARANTINE_WINDOW - Duration::from_secs(1);
        session.recently_closed.insert(0xAAAA, stale_inserted_at);
        session.recently_closed.insert(0xBBBB, stale_inserted_at);

        // Add a fresh entry that should NOT be swept yet.
        session.recently_closed.insert(0xFEEDC0DE, Instant::now());

        assert_eq!(session.recently_closed.len(), 3);

        // Run the sweep. No streams to evict (we didn't open
        // any), but the recently_closed sweep should still fire.
        session.evict_idle_streams(Duration::from_millis(1), usize::MAX, "test");

        // Stale entries dropped; fresh entry kept.
        assert!(
            !session.recently_closed.contains_key(&0xAAAA),
            "stale recently_closed entry past quarantine window must be swept"
        );
        assert!(
            !session.recently_closed.contains_key(&0xBBBB),
            "stale recently_closed entry past quarantine window must be swept"
        );
        assert!(
            session.recently_closed.contains_key(&0xFEEDC0DE),
            "fresh recently_closed entry within quarantine window must survive"
        );
    }

    #[test]
    fn test_session_manager_arc_shares_touch_updates() {
        let manager = SessionManager::new(Duration::from_millis(50));

        let keys = test_keys();
        let peer_addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        let session = Arc::new(NetSession::new(keys, peer_addr, 4, false));

        manager.set_session_arc(session.clone());

        std::thread::sleep(Duration::from_millis(30));
        session.touch();

        assert!(
            manager.check_session(),
            "session should be healthy because touch() updated the shared Arc"
        );

        std::thread::sleep(Duration::from_millis(60));
        assert!(
            !manager.check_session(),
            "session should have timed out after 60ms with no touch"
        );
    }

    #[test]
    fn test_stream_state_tx_credit_trips_backpressure() {
        // 100-byte window: two 40-byte acquires fit; third fails.
        let state = StreamState::new_full(false, 1, 100);
        assert!(state.try_acquire_tx_credit(40), "first acquire fits");
        assert!(state.try_acquire_tx_credit(40), "second acquire fits");
        assert!(
            !state.try_acquire_tx_credit(40),
            "third acquire must be refused — only 20 bytes remain"
        );
        assert_eq!(state.backpressure_events(), 1);
        assert_eq!(state.tx_credit_remaining(), 20);
    }

    #[test]
    fn test_stream_state_refund_restores_credit() {
        let state = StreamState::new_full(false, 1, 100);
        assert!(state.try_acquire_tx_credit(80));
        assert!(
            !state.try_acquire_tx_credit(40),
            "window saturated after 80-byte acquire"
        );

        // Refund simulates a cancelled send — credit flows back.
        state.refund_tx_credit(80);
        assert_eq!(state.tx_credit_remaining(), 100);
        assert!(state.try_acquire_tx_credit(100));
    }

    #[test]
    fn test_stream_state_tx_window_zero_is_unbounded() {
        let state = StreamState::new_full(false, 1, 0);
        // `tx_window == 0` short-circuits — no admission check at all.
        for _ in 0..10_000 {
            assert!(state.try_acquire_tx_credit(1));
        }
        assert_eq!(state.backpressure_events(), 0);
    }

    #[test]
    fn test_stream_state_refund_saturates_at_u32_max() {
        // Refund uses saturating u32 addition with no clamp at
        // `tx_window`: a refunded uncommitted guard must return
        // bytes to `tx_credit_remaining` without stranding any
        // credit, and a pathological caller must not wrap the
        // counter.
        let state = StreamState::new_full(false, 1, 100);
        // Manually push tx_credit_remaining near the top so we can
        // exercise the saturating edge.
        state
            .tx_credit_remaining
            .store(u32::MAX - 50, Ordering::Release);
        state.refund_tx_credit(1000);
        assert_eq!(state.tx_credit_remaining(), u32::MAX);
    }

    #[test]
    fn test_authoritative_grant_recomputes_from_absolute_consumed() {
        // Commit 60 bytes, then apply an authoritative grant
        // reporting `total_consumed = 60`. Outstanding = 0, so the
        // sender's remaining credit returns to the full 100-byte
        // window — even though the grant didn't "add" anything.
        let state = StreamState::new_full(false, 1, 100);
        assert!(state.try_acquire_tx_credit(60));
        assert_eq!(state.tx_credit_remaining(), 40);
        assert_eq!(state.tx_bytes_sent(), 60);

        state.apply_authoritative_grant(60);
        assert_eq!(state.tx_credit_remaining(), 100);
        assert_eq!(state.max_consumed_seen(), 60);
        assert_eq!(state.credit_grants_received(), 1);
    }

    #[test]
    fn test_authoritative_grant_self_heals_lost_grants() {
        // Simulate a lost grant: sender commits 30 bytes, grant A
        // (total_consumed = 30) is "lost" — never applied. Sender
        // commits another 40 bytes (total = 70 on sender's side).
        // Grant B arrives with total_consumed = 70; sender
        // reconciles directly to remaining = 100 - (70 - 70) = 100,
        // fully recovering the credit that Grant A would have
        // refunded. This is the self-healing property.
        let state = StreamState::new_full(false, 1, 100);
        assert!(state.try_acquire_tx_credit(30));
        assert!(state.try_acquire_tx_credit(40));
        assert_eq!(state.tx_credit_remaining(), 30);
        assert_eq!(state.tx_bytes_sent(), 70);

        state.apply_authoritative_grant(70); // Grant B — Grant A was dropped
        assert_eq!(state.tx_credit_remaining(), 100);
    }

    #[test]
    fn test_authoritative_grant_monotonic_ignores_stale() {
        // Out-of-order grants: apply 60, then a stale grant of 40.
        // The stale one must be ignored — `max_consumed_seen` stays
        // at 60 and `tx_credit_remaining` is unchanged.
        let state = StreamState::new_full(false, 1, 100);
        assert!(state.try_acquire_tx_credit(80));
        state.apply_authoritative_grant(60);
        let remaining = state.tx_credit_remaining();

        state.apply_authoritative_grant(40); // stale
        assert_eq!(state.max_consumed_seen(), 60);
        assert_eq!(state.tx_credit_remaining(), remaining);
    }

    #[test]
    fn test_authoritative_grant_clamps_to_window_on_malformed_total_consumed() {
        // Regression: a malformed or buggy grant whose
        // `total_consumed` claims more bytes than the sender has
        // actually committed would otherwise mint credit above
        // `tx_window` — the sender could then exceed its configured
        // ceiling on subsequent admits.
        //
        // BUG_REPORT.md #12 (additional fix): we now clamp
        // `total_consumed.min(tx_bytes_sent)` *before* the CAS so
        // the malformed advancement isn't sticky either. Previously
        // a single malformed grant of `u64::MAX` advanced
        // `max_consumed_seen` to MAX, and every subsequent honest
        // grant tripped the `total_consumed <= prev` early-return
        // — the stream stalled forever. With the clamp, a
        // malformed `total_consumed` is bounded by the actual
        // sender-side watermark, so honest grants below the
        // (genuinely sent) watermark remain admittable.
        let state = StreamState::new_full(false, 1, 100);
        assert!(state.try_acquire_tx_credit(40));
        assert_eq!(state.tx_credit_remaining(), 60);
        assert_eq!(state.tx_bytes_sent(), 40);

        // Malformed grant: reports 500 consumed against only 40 sent.
        state.apply_authoritative_grant(500);

        assert_eq!(
            state.tx_credit_remaining(),
            100,
            "malformed grant must not push credit above tx_window",
        );
        // Per the #12 clamp, max_consumed_seen advances to the
        // sender-side watermark (40), NOT to the malformed value.
        assert_eq!(
            state.max_consumed_seen(),
            40,
            "max_consumed_seen must be clamped to tx_bytes_sent (#12)",
        );
        // A subsequent honest grant of 50 — but only after another
        // 50 bytes are actually sent (so tx_bytes_sent rises to 90).
        // The clamp keeps the watermark accurate.
        assert!(state.try_acquire_tx_credit(50));
        state.apply_authoritative_grant(70);
        assert_eq!(state.max_consumed_seen(), 70);
    }

    /// Regression: BUG_REPORT.md #12 — a single malformed grant
    /// claiming `total_consumed = u64::MAX` used to permanently
    /// lock out future grants. The clamp prevents the stuck-state.
    #[test]
    fn test_authoritative_grant_u64_max_does_not_lock_out_future_grants() {
        let state = StreamState::new_full(false, 1, 100);
        assert!(state.try_acquire_tx_credit(40));

        // Hostile grant: claims the receiver consumed every
        // representable byte. Pre-fix this would set
        // max_consumed_seen to u64::MAX and every subsequent
        // grant would early-return.
        state.apply_authoritative_grant(u64::MAX);
        assert_eq!(
            state.max_consumed_seen(),
            40,
            "max_consumed_seen must be clamped to tx_bytes_sent, \
             not advanced to u64::MAX"
        );

        // Send more, then the receiver issues an honest grant.
        // Pre-fix: rejected as stale because 80 < u64::MAX.
        // Post-fix: accepted because max_consumed_seen is at 40.
        assert!(state.try_acquire_tx_credit(40));
        state.apply_authoritative_grant(80);
        assert_eq!(
            state.max_consumed_seen(),
            80,
            "honest grants must not be locked out by a prior malformed \
             u64::MAX grant (#12)"
        );
    }

    #[test]
    fn test_authoritative_grant_does_not_clobber_concurrent_acquire() {
        // Regression: the earlier `.store()`-based reconciliation
        // computed `remaining = window - (sent - consumed)` from a
        // racy snapshot of `tx_bytes_sent` and then *overwrote*
        // `tx_credit_remaining`. A concurrent `try_acquire_tx_credit`
        // that had already CAS'd its debit but not yet bumped
        // `tx_bytes_sent` would have its debit silently undone — the
        // sender could then exceed its window.
        //
        // Hand-drive the interleaving by performing the first half of
        // an acquire (the CAS on `tx_credit_remaining`) before
        // applying the grant, and the second half (the bump of
        // `tx_bytes_sent`) after. If the invariant
        // `remaining + (sent - max_consumed) == window` still holds
        // at the end, the grant respected the in-flight acquire.
        let state = StreamState::new_full(false, 1, 100);
        // Commit 60 bytes up front so `tx_bytes_sent` is non-zero.
        assert!(state.try_acquire_tx_credit(60));
        assert_eq!(state.tx_credit_remaining(), 40);
        assert_eq!(state.tx_bytes_sent(), 60);

        // Step 1 of a would-be `try_acquire_tx_credit(30)`: CAS the
        // credit debit. Defer the `tx_bytes_sent` bump to simulate a
        // thread that has stalled between the two atomic ops.
        state
            .tx_credit_remaining
            .compare_exchange(40, 10, Ordering::AcqRel, Ordering::Acquire)
            .expect("no contention in test harness");

        // Grant arrives while the acquire is mid-flight: sees
        // `tx_bytes_sent = 60` (pre-bump), advances `max_consumed` to 60.
        state.apply_authoritative_grant(60);

        // Step 2: finish the acquire by bumping `tx_bytes_sent`.
        state.tx_bytes_sent.fetch_add(30, Ordering::Relaxed);

        let remaining = state.tx_credit_remaining() as u64;
        let sent = state.tx_bytes_sent();
        let consumed = state.max_consumed_seen();
        assert_eq!(
            remaining + (sent - consumed),
            100,
            "invariant violated: remaining={} sent={} consumed={} (grant clobbered the in-flight acquire)",
            remaining,
            sent,
            consumed,
        );
    }

    #[test]
    fn test_authoritative_grant_invariant_under_thread_contention() {
        // Stress: many interleaved acquires and grants must preserve
        // the end-state invariant `remaining + (sent - consumed) == window`.
        // Each acquire takes 1 byte and each grant advances consumed
        // by 1; running both loops to completion on separate threads
        // exercises the ordering between the acquire's two-step
        // (CAS remaining, then bump sent) and the grant's credit
        // update.
        //
        // The granter mirrors honest receiver accounting by waiting
        // until the sender has actually committed `target` bytes
        // before reporting `total_consumed = target`. Without this
        // ordering the test would synthesize malformed grants that
        // report consumption ahead of sent bytes — the window clamp
        // would then strand the over-grant and fail the equality
        // check even though both operations are behaving correctly.
        use std::sync::atomic::AtomicBool;
        use std::sync::Arc;
        use std::thread;

        const WINDOW: u32 = 64;
        const ITERATIONS: u64 = 2_000;

        for _trial in 0..8 {
            let state = Arc::new(StreamState::new_full(false, 1, WINDOW));
            let go = Arc::new(AtomicBool::new(false));

            let state_a = state.clone();
            let go_a = go.clone();
            let acquirer = thread::spawn(move || {
                while !go_a.load(Ordering::Acquire) {
                    std::hint::spin_loop();
                }
                for _ in 0..ITERATIONS {
                    while !state_a.try_acquire_tx_credit(1) {
                        std::hint::spin_loop();
                    }
                }
            });

            let state_g = state.clone();
            let go_g = go.clone();
            let granter = thread::spawn(move || {
                while !go_g.load(Ordering::Acquire) {
                    std::hint::spin_loop();
                }
                for target in 1..=ITERATIONS {
                    while state_g.tx_bytes_sent() < target {
                        std::hint::spin_loop();
                    }
                    state_g.apply_authoritative_grant(target);
                }
            });

            go.store(true, Ordering::Release);
            acquirer.join().unwrap();
            granter.join().unwrap();

            let remaining = state.tx_credit_remaining() as u64;
            let sent = state.tx_bytes_sent();
            let consumed = state.max_consumed_seen();
            assert_eq!(sent, ITERATIONS);
            assert_eq!(consumed, ITERATIONS);
            assert_eq!(
                remaining + (sent - consumed),
                WINDOW as u64,
                "invariant violated after contention: remaining={} sent={} consumed={}",
                remaining,
                sent,
                consumed,
            );
        }
    }

    #[test]
    fn test_rx_credit_emits_authoritative_total_consumed() {
        // Every `on_bytes_consumed` returns the receiver's running
        // cumulative consumed count, which the caller ships as the
        // `total_consumed` field of an authoritative grant. The
        // function bumps both `consumed` and `granted` by `bytes`
        // — receive-time accounting, see `RxCreditState` rustdoc
        // and BUG_AUDIT_2026_04_30_CORE.md.
        let state = StreamState::new_full(false, 1, 100);
        assert_eq!(state.on_bytes_consumed(60), Some(60));
        assert_eq!(state.on_bytes_consumed(14), Some(74));
        assert_eq!(state.on_bytes_consumed(1), Some(75));
    }

    /// Regression: the v2 receive-time-accounting design
    /// keeps `outstanding = granted - consumed`
    /// pinned at the initial window size. The credit window is
    /// for kernel-buffer protection — application-side throttling
    /// is provided by per-shard queue-depth limits, not this
    /// counter. This test pins the invariant.
    #[test]
    fn rx_credit_outstanding_stays_at_window_under_receive_time_accounting() {
        let state = StreamState::new_full(false, 1, 100);
        let rx = state.rx_credit();
        // Initial: granted=100, consumed=0, outstanding=100.
        assert_eq!(rx.outstanding(), 100);

        state.on_bytes_consumed(30);
        // Receive-time grant: granted=130, consumed=30. outstanding=100.
        assert_eq!(rx.outstanding(), 100);

        state.on_bytes_consumed(70);
        // granted=200, consumed=100. outstanding=100.
        assert_eq!(rx.outstanding(), 100);

        // The pre-fix audit framing claimed this was a bug; closer
        // inspection showed it's the documented v2 design. See
        // `RxCreditState` rustdoc + `mesh.rs:3110-3135`.
    }

    /// Regression: `outstanding()` must never observe a transient
    /// `consumed > granted` inversion under contention.
    ///
    /// Pre-fix `on_bytes_consumed` bumped `consumed` before
    /// `granted`, while `outstanding()` loaded `granted` then
    /// `consumed`. A reader catching the in-flight window saw
    /// `granted` from before a writer's bump but `consumed` from
    /// after — `consumed > granted`, masked by `saturating_sub` to
    /// zero. With the writer-side priming `granted = window_bytes`,
    /// the post-fix invariant is `outstanding() >= window_bytes` at
    /// every instant: the publication order (granted first, then
    /// consumed) plus the matching reader order (consumed first,
    /// then granted) guarantees any observed `consumed` increment is
    /// paired with its `granted` increment by the time the reader
    /// loads `granted`.
    ///
    /// Setup: `window_bytes = K`, every writer call mints `K`
    /// matched bytes. Pre-fix the reader sees outstanding=0 mid-flight;
    /// post-fix the reader always sees outstanding >= K.
    #[test]
    fn rx_credit_outstanding_never_inverts_under_contention() {
        use std::sync::atomic::{AtomicBool, AtomicU64};
        use std::sync::Arc;
        use std::thread;

        const WINDOW: u32 = 64;
        const WRITERS: usize = 6;
        const ITERATIONS: usize = 50_000;

        let state = Arc::new(StreamState::new_full(false, 1, WINDOW));
        let stop = Arc::new(AtomicBool::new(false));
        let min_seen = Arc::new(AtomicU64::new(u64::MAX));
        let reader_loops = Arc::new(AtomicU64::new(0));

        // Reader: spin on `outstanding()` and record the minimum
        // value observed. Stops as soon as the writers signal done.
        let reader = {
            let state = Arc::clone(&state);
            let stop = Arc::clone(&stop);
            let min_seen = Arc::clone(&min_seen);
            let reader_loops = Arc::clone(&reader_loops);
            thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    let v = state.rx_credit().outstanding();
                    let mut current = min_seen.load(std::sync::atomic::Ordering::Relaxed);
                    while v < current {
                        match min_seen.compare_exchange_weak(
                            current,
                            v,
                            std::sync::atomic::Ordering::Relaxed,
                            std::sync::atomic::Ordering::Relaxed,
                        ) {
                            Ok(_) => break,
                            Err(seen) => current = seen,
                        }
                    }
                    reader_loops.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            })
        };

        // Writers: pound `on_bytes_consumed(WINDOW)` so each call
        // moves both counters by exactly the priming window. With
        // K == WINDOW, any inversion of the publication order
        // surfaces as `consumed > granted` and saturates to zero.
        let writers: Vec<_> = (0..WRITERS)
            .map(|_| {
                let state = Arc::clone(&state);
                thread::spawn(move || {
                    for _ in 0..ITERATIONS {
                        let _ = state.on_bytes_consumed(WINDOW as u64);
                    }
                })
            })
            .collect();

        for w in writers {
            w.join().unwrap();
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        reader.join().unwrap();

        // Sanity: the reader actually got CPU time. Without this,
        // the assertion below would silently pass on a single-core
        // / over-subscribed runner.
        assert!(
            reader_loops.load(std::sync::atomic::Ordering::Relaxed) > 1_000,
            "reader did not get enough CPU time to exercise the race",
        );

        // The strong invariant: outstanding never drops below the
        // priming window. Pre-fix this falls to 0 under contention.
        let observed_min = min_seen.load(std::sync::atomic::Ordering::Relaxed);
        assert!(
            observed_min >= WINDOW as u64,
            "outstanding() inverted under contention: min observed = {} (must be >= {})",
            observed_min,
            WINDOW,
        );
    }

    #[test]
    fn test_rx_credit_window_zero_disables_grants() {
        let state = StreamState::new_full(false, 1, 0);
        // No backpressure → no grants.
        assert_eq!(state.on_bytes_consumed(1_000_000), None);
    }

    #[test]
    fn test_regression_reliable_duplicate_must_not_mint_grant() {
        // Regression: the mesh dispatcher (`process_local_packet`)
        // must gate `on_bytes_consumed` on the reliability layer's
        // `on_receive` return. Otherwise retransmissions / replays
        // of already-acked sequences on Reliable streams refund
        // sender credit through the grant path, inflating
        // `tx_credit_remaining` on the sender and distorting the
        // `backpressure_events` picture.
        //
        // This test exercises the primitives the dispatcher
        // composes. The dispatcher's gate itself is verified
        // implicitly by the three-node integration suite; this
        // primitive-level check is the tight loop that fails
        // fastest if the invariant regresses.
        let state = StreamState::new_full(true, 1, 100); // reliable

        // First packet at seq=0: accepted → credit the bytes.
        assert!(
            state.with_reliability(|r| r.on_receive(0)),
            "new seq must be accepted"
        );
        assert_eq!(state.on_bytes_consumed(40), Some(40));

        // Replay of seq=0: rejected. The dispatcher MUST NOT call
        // `on_bytes_consumed` in this branch. We document that
        // invariant by NOT calling it here — if the dispatcher
        // ever un-gates, the matching integration test would
        // observe inflated grants / distorted credit accounting.
        assert!(
            !state.with_reliability(|r| r.on_receive(0)),
            "duplicate seq must be rejected by the reliability layer"
        );

        // Sanity: the rx-credit state reflects only the one
        // accepted packet — `granted = window_bytes + 40`,
        // `consumed = 40`.
        let rx = state.rx_credit();
        assert_eq!(rx.consumed(), 40);
        assert_eq!(rx.granted(), 100 + 40);
    }

    fn session_with_stream(stream_id: u64, tx_window: u32) -> Arc<NetSession> {
        let session = Arc::new(NetSession::new(
            test_keys(),
            "127.0.0.1:9999".parse().unwrap(),
            4,
            false,
        ));
        session.open_stream_full(stream_id, false, 1, tx_window);
        session
    }

    #[test]
    fn test_regression_tx_credit_guard_refunds_on_drop() {
        // Regression: without the RAII guard, `send_on_stream`'s
        // acquire-await-commit shape leaks credit if the send future
        // is dropped mid-`.await` (tokio::select! racing a shutdown,
        // caller abort, panic). Over many cancellations the window
        // would drift toward permanent exhaustion.
        //
        // Fix: `try_acquire_tx_credit_guard` returns a `TxSlotGuard`
        // that refunds the acquired bytes in its Drop impl — unless
        // the caller calls `commit()` first to signal a successful
        // wire send.
        let stream_id = 0x7u64;
        let session = session_with_stream(stream_id, 100);

        let guard = match session.try_acquire_tx_credit_guard(stream_id, 100) {
            TxAdmit::Acquired { guard, .. } => guard,
            other => panic!("expected Acquired, got {:?}", other),
        };
        assert_eq!(
            session.try_stream(stream_id).unwrap().tx_credit_remaining(),
            0,
            "guard's acquire drained the window"
        );
        assert!(matches!(
            session.try_acquire_tx_credit_guard(stream_id, 1),
            TxAdmit::WindowFull
        ));

        // Drop without commit → bytes flow back.
        drop(guard);
        assert_eq!(
            session.try_stream(stream_id).unwrap().tx_credit_remaining(),
            100,
            "dropping an uncommitted guard refunds the credit"
        );
        assert!(matches!(
            session.try_acquire_tx_credit_guard(stream_id, 50),
            TxAdmit::Acquired { .. }
        ));
    }

    #[test]
    fn test_tx_credit_guard_commit_suppresses_refund() {
        // commit() marks the bytes as "gone on the wire" — Drop must
        // NOT refund them. The receiver is responsible for replenishing
        // via a StreamWindow grant.
        let stream_id = 0x17u64;
        let session = session_with_stream(stream_id, 100);

        let guard = match session.try_acquire_tx_credit_guard(stream_id, 40) {
            TxAdmit::Acquired { guard, .. } => guard,
            other => panic!("expected Acquired, got {:?}", other),
        };
        guard.commit();
        assert_eq!(
            session.try_stream(stream_id).unwrap().tx_credit_remaining(),
            60,
            "committed bytes stay consumed"
        );
    }

    #[test]
    fn test_tx_credit_guard_stream_closed_variant() {
        let session = session_with_stream(0x9, 100);
        session.close_stream(0x9);
        assert!(matches!(
            session.try_acquire_tx_credit_guard(0x9, 10),
            TxAdmit::StreamClosed
        ));
    }

    #[test]
    fn test_tx_credit_guard_close_between_acquire_and_drop_no_panic() {
        // Scenario: caller acquires, another task closes, caller
        // drops. The Drop impl's `try_stream` lookup returns None →
        // no-op. Must not panic / resurrect state.
        let stream_id = 0xAu64;
        let session = session_with_stream(stream_id, 100);
        let guard = match session.try_acquire_tx_credit_guard(stream_id, 40) {
            TxAdmit::Acquired { guard, .. } => guard,
            other => panic!("expected Acquired, got {:?}", other),
        };
        session.close_stream(stream_id);
        assert!(session.try_stream(stream_id).is_none());
        drop(guard); // no-op (state is gone); must not panic
        assert!(session.try_stream(stream_id).is_none());
    }

    #[test]
    fn test_tx_credit_guard_forget_leaves_credit_consumed() {
        // forget() is a test-only escape hatch simulating a leaked
        // slot — same effect as commit() but semantically labelled as
        // "don't refund because the bytes are lost, not sent."
        let session = session_with_stream(0xF, 100);
        let g = match session.try_acquire_tx_credit_guard(0xF, 40) {
            TxAdmit::Acquired { guard, .. } => guard,
            other => panic!("expected Acquired, got {:?}", other),
        };
        g.forget();
        assert_eq!(
            session.try_stream(0xF).unwrap().tx_credit_remaining(),
            60,
            "forget() skips the Drop refund"
        );
    }

    #[test]
    fn test_regression_guard_drop_after_reopen_does_not_corrupt_new_stream() {
        // Regression: `TxSlotGuard::drop` must not refund credit onto
        // a fresh `StreamState` that never issued the guard. Epoch
        // check gates the refund.
        let sid = 0x42u64;
        let session = session_with_stream(sid, 100);

        let g = match session.try_acquire_tx_credit_guard(sid, 60) {
            TxAdmit::Acquired { guard, .. } => guard,
            other => panic!("expected Acquired, got {:?}", other),
        };
        let first_epoch = g.epoch_for_test();
        assert_eq!(session.try_stream(sid).unwrap().tx_credit_remaining(), 40);

        // Close + reopen → fresh state with a new epoch + full credit.
        session.close_stream(sid);
        session.open_stream_full(sid, false, 1, 100);
        let second_epoch = session.try_stream(sid).unwrap().epoch();
        assert_ne!(first_epoch, second_epoch, "reopen allocates a new epoch");
        assert_eq!(
            session.try_stream(sid).unwrap().tx_credit_remaining(),
            100,
            "fresh stream starts at full credit"
        );

        // Drop the stale guard — must NOT inflate the new stream's
        // credit beyond its configured window.
        drop(g);
        assert_eq!(
            session.try_stream(sid).unwrap().tx_credit_remaining(),
            100,
            "stale guard must NOT refund onto the new stream's counter"
        );
    }

    #[test]
    fn test_regression_acquire_with_expected_epoch_rejects_after_reopen() {
        let sid = 0x88u64;
        let session = session_with_stream(sid, 100);
        let original_epoch = session.try_stream(sid).unwrap().epoch();

        session.close_stream(sid);
        session.open_stream_full(sid, false, 1, 100);

        assert!(matches!(
            session.try_acquire_tx_credit_matching_epoch(sid, original_epoch, 10),
            TxAdmit::StreamClosed
        ));
        assert_eq!(
            session.try_stream(sid).unwrap().tx_credit_remaining(),
            100,
            "rejected acquire leaves new stream's credit untouched"
        );

        let cur_epoch = session.try_stream(sid).unwrap().epoch();
        assert!(matches!(
            session.try_acquire_tx_credit_matching_epoch(sid, cur_epoch, 10),
            TxAdmit::Acquired { .. }
        ));
    }

    #[test]
    fn test_regression_no_double_counting_grant_and_refund() {
        // Double-counting trap: if both a grant AND a successful-send
        // refund credit the window for the same bytes, every round
        // trip doubles effective capacity. The v2 invariant: commit()
        // suppresses the refund; only a grant replenishes committed
        // bytes.
        let stream_id = 0x100u64;
        let session = session_with_stream(stream_id, 200);

        // Send: acquire 100 bytes, commit.
        let g = match session.try_acquire_tx_credit_guard(stream_id, 100) {
            TxAdmit::Acquired { guard, .. } => guard,
            other => panic!("expected Acquired, got {:?}", other),
        };
        g.commit();
        assert_eq!(
            session.try_stream(stream_id).unwrap().tx_credit_remaining(),
            100,
            "after commit, 100 bytes consumed against a 200-byte window"
        );

        // Authoritative grant reporting total_consumed=100: the
        // receiver has accepted the 100 bytes we committed, so
        // outstanding = 0 and credit returns to the full window.
        session
            .try_stream(stream_id)
            .unwrap()
            .apply_authoritative_grant(100);
        assert_eq!(
            session.try_stream(stream_id).unwrap().tx_credit_remaining(),
            200,
            "grant restores committed credit exactly once"
        );

        // CRITICAL: replaying the same grant (stale duplicate) is
        // ignored by the monotonic `max_consumed_seen` check. No
        // spurious inflation past the original window — the
        // authoritative-grant design makes double-counting
        // impossible even if the grant arrives multiple times.
        session
            .try_stream(stream_id)
            .unwrap()
            .apply_authoritative_grant(100);
        assert_eq!(
            session.try_stream(stream_id).unwrap().tx_credit_remaining(),
            200,
            "replaying a stale grant must not inflate credit",
        );
    }

    #[test]
    fn test_regression_stale_grant_quarantined_after_close_reopen() {
        // Regression (P1): a `StreamWindow` grant keyed only by
        // stream_id could credit a reopened stream with credit
        // minted against the previous lifetime's `StreamState`.
        // Fix: `close_stream` stamps the stream_id into
        // `recently_closed`; `is_grant_quarantined` tells the
        // dispatcher to drop grants that arrive within
        // `GRANT_QUARANTINE_WINDOW`.
        let sid = 0x2077u64;
        let session = session_with_stream(sid, 100);

        // Mid-flight: close the stream, reopen with the same id.
        session.close_stream(sid);
        session.open_stream_full(sid, false, 1, 100);

        // An arriving grant for `sid` must be quarantined because
        // the original lifetime was closed inside the window.
        assert!(
            session.is_grant_quarantined(sid),
            "grants for recently-closed stream must be dropped"
        );

        // The reopened stream's credit is untouched — we don't call
        // apply_authoritative_grant under quarantine.
        assert_eq!(session.try_stream(sid).unwrap().tx_credit_remaining(), 100);
    }

    #[test]
    fn test_grant_quarantine_does_not_fire_without_close() {
        // Baseline: streams that were never closed aren't in the
        // quarantine set. Grants flow normally.
        let sid = 0x2099u64;
        let session = session_with_stream(sid, 100);
        assert!(!session.is_grant_quarantined(sid));
    }

    #[test]
    fn test_regression_control_seq_isolated_from_user_stream() {
        // Regression: `spawn_stream_window_grant` used to draw the
        // grant packet's sequence from
        // `get_or_create_stream(SUBPROTOCOL_STREAM_WINDOW as u64)`,
        // so a user stream opened with the numerically-equal id
        // (0x0B00) would share sequence state with control traffic.
        //
        // Fix: grants ride on the `CONTROL_STREAM_ID` sentinel
        // (`u64::MAX`) with a dedicated session-level
        // `next_control_tx_seq` counter. This test verifies that
        // opening a user stream at the old-collision id leaves its
        // tx_seq untouched while control-seq advances independently.
        let session = Arc::new(NetSession::new(
            test_keys(),
            "127.0.0.1:9999".parse().unwrap(),
            4,
            false,
        ));
        let user_sid = 0x0B00u64; // the old collision target
        session.open_stream_full(user_sid, false, 1, 100);
        let user_tx_seq_before = session.try_stream(user_sid).unwrap().current_tx_seq();

        // Burn some control-seq as though grants had gone out.
        let ctrl_a = session.next_control_tx_seq();
        let ctrl_b = session.next_control_tx_seq();
        let ctrl_c = session.next_control_tx_seq();
        assert_eq!((ctrl_a, ctrl_b, ctrl_c), (0, 1, 2));

        // User stream's tx_seq must NOT have moved.
        assert_eq!(
            session.try_stream(user_sid).unwrap().current_tx_seq(),
            user_tx_seq_before,
        );

        // Conversely, a user send on the same stream must not
        // advance the control-seq counter.
        session.try_stream(user_sid).unwrap().next_tx_seq();
        assert_eq!(session.next_control_tx_seq(), 3);
    }

    #[test]
    fn test_regression_admit_and_seq_atomic_across_reopen_race() {
        // Regression (P2): `send_on_stream` used to acquire credit
        // and then re-look up the stream to fetch `next_tx_seq`.
        // A concurrent close+reopen between the two lookups would
        // debit credit on the old state while the sequence came
        // from the new state — crossing lifetimes and defeating
        // the epoch guard's safety.
        //
        // Fix: `try_acquire_tx_credit_*` now returns both the guard
        // and the sequence under one DashMap lookup. This test
        // verifies that the admitted sequence belongs to the same
        // `StreamState` as the one that was debited.
        let sid = 0x3141u64;
        let session = session_with_stream(sid, 100);
        let epoch_before = session.try_stream(sid).unwrap().epoch();
        let tx_seq_before = session.try_stream(sid).unwrap().current_tx_seq();

        let (guard, seq) = match session.try_acquire_tx_credit_matching_epoch(sid, epoch_before, 40)
        {
            TxAdmit::Acquired { guard, seq } => (guard, seq),
            other => panic!("expected Acquired, got {:?}", other),
        };
        guard.commit();

        // The sequence must come from the state that was debited —
        // i.e., the next `current_tx_seq` is one greater than the
        // value observed before, not zero (as it would be if the
        // seq had come from a fresh state after an intervening
        // reopen).
        let after = session.try_stream(sid).unwrap();
        assert_eq!(seq, tx_seq_before);
        assert_eq!(after.current_tx_seq(), tx_seq_before + 1);
        assert_eq!(after.epoch(), epoch_before);
        assert_eq!(after.tx_credit_remaining(), 60);
    }

    impl TxSlotGuard {
        /// Test-only accessor for the captured epoch.
        fn epoch_for_test(&self) -> u64 {
            self.epoch
        }
    }

    /// CR-12: pin that no `tx_key` method exists on `NetSession`.
    /// This is a source-string tripwire — if a future maintainer
    /// reintroduces the accessor, the test fires loudly. The hazard
    /// it gates (cross-pool nonce reuse) is dormant
    /// unless someone calls a `tx_key()` method, so a behavioural
    /// test would not catch the regression in time. We assemble
    /// the forbidden token at runtime so the test's OWN source
    /// doesn't contain the literal it scans for.
    #[test]
    fn cr12_tx_key_accessor_must_not_exist_on_net_session() {
        // Build the forbidden token at runtime: `fn` + space + `tx_key` + `(`.
        // The literal `fn tx_key(` shape is what we must NOT see in
        // a non-comment line in the source.
        let needle = format!("{} {}{}", "fn", "tx_key", "(");

        let src = include_str!("session.rs");
        for line in src.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue; // doc-comment / line comment
            }
            assert!(
                !trimmed.contains(&needle),
                "CR-12 regression: tx_key accessor reintroduced into session.rs:\n  {}",
                line
            );
        }
    }

    /// Regression: `verify_and_touch_heartbeat` short-circuits
    /// any `parsed.payload.len() != TAG_SIZE` packet before
    /// invoking the cipher. AEAD decryption would catch the
    /// mismatch on its own, but the pre-check shortcuts a
    /// cleartext-flood attacker spamming undersized / oversized
    /// payloads to drain CPU on the decrypt path. The
    /// session must be unmutated on rejection — a flood that
    /// nudged `last_activity` would still be a side-channel for
    /// liveness inference.
    #[test]
    fn verify_and_touch_heartbeat_rejects_wrong_length_before_decrypt() {
        use super::super::protocol::{NetHeader, PacketFlags, TAG_SIZE};
        use bytes::Bytes;

        let keys = test_keys();
        let peer_addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        let session = NetSession::new(keys.clone(), peer_addr, 4, false);

        // Capture last_activity before the spoof attempts so we
        // can assert no mutation.
        let baseline_activity = session.last_activity.load(Ordering::Acquire);

        // Build a fake heartbeat header with a ciphertext that
        // ISN'T 16 bytes — the AEAD would reject this anyway,
        // but we want to assert the length gate fires first
        // (no cipher work, no last_activity nudge).
        let mut nonce = [0u8; 12];
        nonce[0..4].copy_from_slice(&crate::adapter::net::crypto::session_prefix_from_id(
            keys.session_id,
        ));
        nonce[4..12].copy_from_slice(&0u64.to_le_bytes());

        let header = NetHeader::new(
            keys.session_id,
            0, // stream_id
            0, // sequence
            nonce,
            0, // payload_len
            0, // event_count
            PacketFlags::HEARTBEAT,
        );

        for bad_len in [0usize, 1, TAG_SIZE - 1, TAG_SIZE + 1, 64] {
            let parsed = ParsedPacket {
                header,
                payload: Bytes::from(vec![0u8; bad_len]),
                source: peer_addr,
            };
            assert!(
                !session.verify_and_touch_heartbeat(&parsed),
                "wrong-length payload ({bad_len} bytes, expected {TAG_SIZE}) \
                 must be rejected before AEAD decrypt"
            );
            assert_eq!(
                session.last_activity.load(Ordering::Acquire),
                baseline_activity,
                "rejected heartbeat must not advance last_activity ({bad_len} bytes)"
            );
        }
    }
}
