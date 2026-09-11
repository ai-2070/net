//! Zero-allocation packet pool and builder.
//!
//! This module provides pre-allocated buffers for packet construction
//! to avoid heap allocations on the hot path.

use bytes::{Bytes, BytesMut};
use crossbeam_queue::ArrayQueue;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::crypto::{session_prefix_from_id, PacketCipher};
use crate::protocol::{
    EventFrame, NetHeader, PacketFlags, HEADER_SIZE, MAX_PACKET_SIZE, MAX_PAYLOAD_SIZE, NONCE_SIZE,
    PAYLOAD_LEN_OFFSET,
};

/// Pre-allocated packet builder using counter-based nonces for zero-allocation
/// packet construction.
pub struct PacketBuilder {
    /// Pre-allocated payload buffer
    payload: BytesMut,
    /// Fast cipher with counter-based nonces
    cipher: PacketCipher,
    /// Scratch buffer for packet assembly
    packet: BytesMut,
    /// Session ID for this builder
    session_id: u64,
    /// Origin hash from entity identity (0 if no identity configured)
    origin_hash: u64,
    /// Channel hash for the current stream (0 if not bound to a channel)
    channel_hash: u16,
}

impl PacketBuilder {
    /// Create a new packet builder.
    ///
    /// `pub(crate)` not `pub`: every legitimate caller is inside
    /// `adapter/net/`. Demoted as part of the heartbeat-unification
    /// pass — see [`HEARTBEAT_UNIFICATION_PLAN.md`] — to prevent a
    /// caller from substituting `&[0u8; 32]` for the session's real
    /// TX key, which would produce AEAD-tagged heartbeats whose tag
    /// the receiver could never verify against the session's actual
    /// key. Heartbeats now go through
    /// [`NetSession::build_heartbeat`]; data-path packets go
    /// through the pool. No external caller should be constructing
    /// raw-key builders.
    pub(crate) fn new(key: &[u8; 32], session_id: u64) -> Self {
        Self {
            payload: BytesMut::with_capacity(MAX_PAYLOAD_SIZE),
            cipher: PacketCipher::new(key, session_id),
            packet: BytesMut::with_capacity(MAX_PACKET_SIZE),
            session_id,
            origin_hash: 0,
            channel_hash: 0,
        }
    }

    /// Create a new packet builder with origin identity
    pub fn with_origin(key: &[u8; 32], session_id: u64, origin_hash: u64) -> Self {
        Self {
            payload: BytesMut::with_capacity(MAX_PAYLOAD_SIZE),
            cipher: PacketCipher::new(key, session_id),
            packet: BytesMut::with_capacity(MAX_PACKET_SIZE),
            session_id,
            origin_hash,
            channel_hash: 0,
        }
    }

    /// Create a packet builder that shares a TX counter with other builders.
    ///
    /// All builders sharing the same counter atomically increment it,
    /// preventing nonce reuse when multiple builders encrypt with the
    /// same key (e.g., in a `PacketPool` or `ThreadLocalPool`).
    pub fn with_shared_counter(
        key: &[u8; 32],
        session_id: u64,
        origin_hash: u64,
        tx_counter: Arc<AtomicU64>,
    ) -> Self {
        Self {
            payload: BytesMut::with_capacity(MAX_PAYLOAD_SIZE),
            cipher: PacketCipher::with_shared_tx_counter(key, session_id, tx_counter),
            packet: BytesMut::with_capacity(MAX_PACKET_SIZE),
            session_id,
            origin_hash,
            channel_hash: 0,
        }
    }

    /// Update the encryption key and session ID
    pub fn set_key(&mut self, key: &[u8; 32], session_id: u64) {
        self.cipher = PacketCipher::new(key, session_id);
        self.session_id = session_id;
    }

    /// Update the encryption key, session ID, and shared counter
    pub fn set_key_shared(&mut self, key: &[u8; 32], session_id: u64, tx_counter: Arc<AtomicU64>) {
        self.cipher = PacketCipher::with_shared_tx_counter(key, session_id, tx_counter);
        self.session_id = session_id;
    }

    /// Set the origin hash
    pub fn set_origin_hash(&mut self, origin_hash: u64) {
        self.origin_hash = origin_hash;
    }

    /// Set the channel hash for outgoing packets
    pub fn set_channel_hash(&mut self, channel_hash: u16) {
        self.channel_hash = channel_hash;
    }

    /// Build a packet from events using counter-based encryption.
    ///
    /// This method:
    /// 1. Writes events to the payload buffer with length prefixes
    /// 2. Serializes the header once, derives AAD from the serialized bytes
    /// 3. Encrypts the payload in-place using counter-based nonce
    /// 4. Patches nonce and payload_len into the serialized header
    /// 5. Assembles the final packet
    ///
    /// Returns the complete packet as `Bytes`.
    ///
    /// # Panics
    ///
    /// Panics if `events.len() > NetHeader::MAX_EVENTS_PER_PACKET`.
    /// Pre-fix this performed `events.len() as u16` and
    /// silently wrapped on overflow. A caller passing `>65 535`
    /// events stored `len % 65 536` in the wire `event_count`
    /// field; the receiver mis-parsed the payload because the
    /// stored count no longer matched the encoded frames. Worse,
    /// a wrapped value below the receiver's `MAX_EVENTS_PER_PACKET`
    /// cap (e.g. caller passed 67 562 → wrapped 32 026, which is
    /// `> 2027` → noisy reject; but caller passed 65 537 → wrapped
    /// 1, which is `<= 2027` → silent corruption). The batching
    /// layer above must already enforce the cap; this is a
    /// defense-in-depth `panic!` so a missed cap surfaces
    /// immediately instead of silently corrupting frames.
    #[inline]
    pub fn build(
        &mut self,
        stream_id: u64,
        sequence: u64,
        events: &[Bytes],
        flags: PacketFlags,
    ) -> Bytes {
        assert!(
            events.len() <= NetHeader::MAX_EVENTS_PER_PACKET as usize,
            "PacketBuilder::build called with {} events; \
             MAX_EVENTS_PER_PACKET is {}. The batching layer must \
             split before calling build().",
            events.len(),
            NetHeader::MAX_EVENTS_PER_PACKET,
        );

        // PERF_AUDIT §2.6 — frame events directly into the packet
        // buffer at `HEADER_SIZE` offset (rather than into a
        // separate `payload` scratch, then memcpy the full
        // encrypted payload across after the header). Reserves
        // HEADER_SIZE placeholder bytes up front so events land at
        // their final wire offset; the header is patched into the
        // placeholder after encryption supplies the nonce counter
        // and final payload length. Eliminates one ~payload-sized
        // memcpy per TX.
        self.packet.clear();
        self.packet.resize(HEADER_SIZE, 0);

        // Append event frames after the header placeholder. The
        // payload-region length tracked below excludes HEADER_SIZE.
        EventFrame::write_events(events, &mut self.packet);
        let plaintext_len = self.packet.len() - HEADER_SIZE;

        // Build and serialize header once (nonce is placeholder,
        // will be patched). The wire payload_len field stores the
        // ciphertext-only length, which equals `plaintext_len` —
        // the 16-byte tag isn't counted (see comment below). For
        // the AAD-providing call, the plaintext length is the
        // value used by both ends of the channel.
        let header = NetHeader::new(
            self.session_id,
            stream_id,
            sequence,
            [0u8; NONCE_SIZE],
            plaintext_len as u16,
            events.len() as u16,
            flags,
        )
        .with_origin(self.origin_hash)
        .with_channel_hash(self.channel_hash);
        let aad = header.aad();
        let mut header_bytes = header.to_bytes();

        // Encrypt the in-place payload region. ChaCha20-Poly1305
        // encryption cannot fail with valid inputs — an error
        // here indicates memory corruption or a cipher library bug.
        let (counter, tag) = match self
            .cipher
            .encrypt_in_place_detached(&aad, &mut self.packet[HEADER_SIZE..])
        {
            Ok(out) => out,
            Err(e) => panic!(
                "BUG: ChaCha20-Poly1305 encryption failed (session={:016x}, payload_len={}): {}",
                self.session_id, plaintext_len, e
            ),
        };
        // Append the detached 16-byte auth tag in place — same
        // wire shape as the prior path, just without the
        // intermediate scratch buffer.
        self.packet.extend_from_slice(&tag);

        // Patch nonce into the in-buffer header (bytes 12..24).
        // The 4-byte prefix must be derived the same way the
        // cipher does (`session_prefix_from_id`); a divergence
        // would make the receiver reconstruct a different nonce
        // and AEAD verification would fail.
        header_bytes[12..16].copy_from_slice(&session_prefix_from_id(self.session_id));
        header_bytes[16..24].copy_from_slice(&counter.to_le_bytes());

        // Wire payload_len reflects ciphertext-without-tag (the
        // tag is appended separately and the receiver computes
        // `total - HEADER_SIZE - TAG_SIZE`). The `as u16` cast is
        // safe under current MAX_PAYLOAD_SIZE (8112 << u16::MAX);
        // a future config raising the cap past u16::MAX would
        // silently truncate this field.
        debug_assert!(
            plaintext_len <= u16::MAX as usize,
            "payload length {} would truncate the u16 wire field; \
             revisit MAX_PAYLOAD_SIZE before raising the cap past u16::MAX",
            plaintext_len,
        );
        let payload_len = plaintext_len as u16;
        header_bytes[PAYLOAD_LEN_OFFSET..PAYLOAD_LEN_OFFSET + 2]
            .copy_from_slice(&payload_len.to_le_bytes());

        // Splice the final header bytes into the placeholder.
        self.packet[..HEADER_SIZE].copy_from_slice(&header_bytes);

        // split() transfers ownership without atomic ref-count bump.
        self.packet.split().freeze()
    }

    /// Build a packet with a subprotocol identifier.
    ///
    /// Same as `build()` but sets `subprotocol_id` in the Net header,
    /// which is included in the AEAD authenticated data.
    ///
    /// # Panics
    ///
    /// Panics on `events.len() > NetHeader::MAX_EVENTS_PER_PACKET` —
    /// see [`Self::build`] for the rationale.
    #[inline]
    pub fn build_subprotocol(
        &mut self,
        stream_id: u64,
        sequence: u64,
        events: &[Bytes],
        flags: PacketFlags,
        subprotocol_id: u16,
    ) -> Bytes {
        assert!(
            events.len() <= NetHeader::MAX_EVENTS_PER_PACKET as usize,
            "PacketBuilder::build_subprotocol called with {} events; \
             MAX_EVENTS_PER_PACKET is {}",
            events.len(),
            NetHeader::MAX_EVENTS_PER_PACKET,
        );

        // PERF_AUDIT §2.6 — same in-place framing pattern as
        // `build()`. See that method for the full rationale.
        self.packet.clear();
        self.packet.resize(HEADER_SIZE, 0);

        EventFrame::write_events(events, &mut self.packet);
        let plaintext_len = self.packet.len() - HEADER_SIZE;

        let header = NetHeader::new(
            self.session_id,
            stream_id,
            sequence,
            [0u8; NONCE_SIZE],
            plaintext_len as u16,
            events.len() as u16,
            flags,
        )
        .with_origin(self.origin_hash)
        .with_channel_hash(self.channel_hash)
        .with_subprotocol(subprotocol_id);
        let aad = header.aad();
        let mut header_bytes = header.to_bytes();

        let (counter, tag) = match self
            .cipher
            .encrypt_in_place_detached(&aad, &mut self.packet[HEADER_SIZE..])
        {
            Ok(out) => out,
            Err(e) => panic!(
                "BUG: ChaCha20-Poly1305 encryption failed (session={:016x}): {}",
                self.session_id, e
            ),
        };
        self.packet.extend_from_slice(&tag);

        header_bytes[12..16].copy_from_slice(&session_prefix_from_id(self.session_id));
        header_bytes[16..24].copy_from_slice(&counter.to_le_bytes());
        debug_assert!(
            plaintext_len <= u16::MAX as usize,
            "payload length {} would truncate the u16 wire field; \
             revisit MAX_PAYLOAD_SIZE before raising the cap past u16::MAX",
            plaintext_len,
        );
        let payload_len = plaintext_len as u16;
        header_bytes[PAYLOAD_LEN_OFFSET..PAYLOAD_LEN_OFFSET + 2]
            .copy_from_slice(&payload_len.to_le_bytes());

        self.packet[..HEADER_SIZE].copy_from_slice(&header_bytes);
        self.packet.split().freeze()
    }

    /// Build a handshake packet (unencrypted)
    #[inline]
    pub fn build_handshake(&mut self, payload: &[u8]) -> Bytes {
        self.packet.clear();

        let header = NetHeader::handshake(payload.len() as u16);

        self.packet.extend_from_slice(&header.to_bytes());
        self.packet.extend_from_slice(payload);

        self.packet.split().freeze()
    }

    /// Build an AEAD-authenticated heartbeat packet.
    ///
    /// Heartbeats used to be cleartext (header only, no auth tag) —
    /// the receiver only checked `source == peer_addr` (UDP source,
    /// spoofable) and `session_id` match (64-bit, observable in
    /// flight). An off-path attacker who guessed or observed the
    /// session_id could call `session.touch()` indefinitely,
    /// defeating both the idle timeout and the failure detector.
    ///
    /// Now: encrypt an empty payload with the session's TX cipher
    /// so the heartbeat carries a 16-byte Poly1305 tag over the
    /// header's AAD. An off-path attacker without the session key
    /// cannot forge a tag that decrypts.
    #[inline]
    pub fn build_heartbeat(&mut self) -> Bytes {
        self.payload.clear();
        self.packet.clear();

        // No payload bytes — encryption produces just the tag.
        let header = NetHeader::heartbeat(self.session_id);
        let aad = header.aad();
        let mut header_bytes = header.to_bytes();

        let counter = match self.cipher.encrypt_in_place(&aad, &mut self.payload) {
            Ok(c) => c,
            Err(e) => panic!(
                "BUG: heartbeat AEAD encryption failed (session={:016x}): {}",
                self.session_id, e
            ),
        };

        // Patch nonce + counter into the header bytes, same as
        // `build` does for data packets.
        header_bytes[12..16].copy_from_slice(&session_prefix_from_id(self.session_id));
        header_bytes[16..24].copy_from_slice(&counter.to_le_bytes());

        // payload_len is 0 (the tag rides outside the declared
        // payload length, like every other AEAD-tagged packet).
        let payload_len = 0u16;
        header_bytes[PAYLOAD_LEN_OFFSET..PAYLOAD_LEN_OFFSET + 2]
            .copy_from_slice(&payload_len.to_le_bytes());

        self.packet.extend_from_slice(&header_bytes);
        self.packet.extend_from_slice(&self.payload); // just the 16-byte tag
        self.packet.split().freeze()
    }

    /// Get the maximum number of events that can fit in a single packet
    #[inline]
    pub fn max_events_for_size(&self, avg_event_size: usize) -> usize {
        let frame_overhead = EventFrame::LEN_SIZE;
        MAX_PAYLOAD_SIZE / (avg_event_size + frame_overhead)
    }

    /// Check if events would fit in a single packet
    #[inline]
    pub fn would_fit(&self, events: &[Bytes]) -> bool {
        EventFrame::calculate_size(events) <= MAX_PAYLOAD_SIZE
    }

    /// Get the session ID
    #[inline]
    pub fn session_id(&self) -> u64 {
        self.session_id
    }
}

impl std::fmt::Debug for PacketBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PacketBuilder")
            .field("session_id", &format!("{:016x}", self.session_id))
            .field("payload_capacity", &self.payload.capacity())
            .field("packet_capacity", &self.packet.capacity())
            .finish()
    }
}

/// Pool of packet builders for amortized allocation.
///
/// Uses counter-based nonces for zero-allocation packet construction.
/// All builders in the pool share a single TX counter to prevent nonce reuse.
pub struct PacketPool {
    /// Queue of available builders
    builders: ArrayQueue<PacketBuilder>,
    /// Encryption key for new builders
    key: [u8; 32],
    /// Session ID for builders
    session_id: u64,
    /// Origin hash for L1 identity
    origin_hash: u64,
    /// Pool capacity
    capacity: usize,
    /// Shared TX counter — all builders atomically increment this to prevent
    /// nonce reuse when multiple builders encrypt with the same key.
    tx_counter: Arc<AtomicU64>,
}

impl PacketPool {
    /// Create a new packet pool
    pub fn new(size: usize, key: &[u8; 32], session_id: u64) -> Self {
        Self::with_origin(size, key, session_id, 0)
    }

    /// Create a new packet pool with origin identity
    pub fn with_origin(size: usize, key: &[u8; 32], session_id: u64, origin_hash: u64) -> Self {
        let tx_counter = Arc::new(AtomicU64::new(0));
        let builders = ArrayQueue::new(size);

        // Pre-populate the pool with builders sharing the same TX counter
        for _ in 0..size {
            let _ = builders.push(PacketBuilder::with_shared_counter(
                key,
                session_id,
                origin_hash,
                tx_counter.clone(),
            ));
        }

        Self {
            builders,
            key: *key,
            session_id,
            origin_hash,
            capacity: size,
            tx_counter,
        }
    }

    /// Update the encryption key and session ID.
    ///
    /// Drains all pooled builders so that no stale builder can encrypt
    /// with the old key while sharing a counter that restarted at zero
    /// (which would cause nonce reuse and break ChaCha20-Poly1305).
    /// Builders are lazily re-created with the new key on the next `get()`.
    pub fn set_key(&mut self, key: &[u8; 32], session_id: u64) {
        self.key = *key;
        self.session_id = session_id;
        // Reset the shared counter for the new session
        self.tx_counter = Arc::new(AtomicU64::new(0));
        // Drain all existing builders — they hold the old key and old
        // counter Arc, so using them risks nonce reuse during the
        // transition window.
        while self.builders.pop().is_some() {}
    }

    /// Get a builder from the pool
    #[inline]
    pub fn get(&self) -> PooledBuilder<'_> {
        let builder = self.builders.pop().unwrap_or_else(|| {
            PacketBuilder::with_shared_counter(
                &self.key,
                self.session_id,
                self.origin_hash,
                self.tx_counter.clone(),
            )
        });

        PooledBuilder {
            pool: self,
            builder: Some(builder),
        }
    }

    /// Get the pool capacity
    #[inline]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Get the number of available builders
    #[inline]
    pub fn available(&self) -> usize {
        self.builders.len()
    }

    /// Get the session ID
    #[inline]
    pub fn session_id(&self) -> u64 {
        self.session_id
    }
}

impl std::fmt::Debug for PacketPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PacketPool")
            .field("capacity", &self.capacity)
            .field("available", &self.builders.len())
            .field("session_id", &format!("{:016x}", self.session_id))
            .finish()
    }
}

/// RAII guard for a fast pooled builder.
pub struct PooledBuilder<'a> {
    pool: &'a PacketPool,
    builder: Option<PacketBuilder>,
}

#[expect(
    clippy::expect_used,
    reason = "self.builder is Some between construction and Drop::drop; calling these methods after drop is a caller-side use-after-free invariant violation, not a recoverable runtime condition"
)]
impl<'a> PooledBuilder<'a> {
    /// Build a packet from events
    #[inline]
    pub fn build(
        &mut self,
        stream_id: u64,
        sequence: u64,
        events: &[Bytes],
        flags: PacketFlags,
    ) -> Bytes {
        self.builder
            .as_mut()
            .expect("BUG: PooledBuilder used after drop")
            .build(stream_id, sequence, events, flags)
    }

    /// Build a handshake packet
    #[inline]
    pub fn build_handshake(&mut self, payload: &[u8]) -> Bytes {
        self.builder
            .as_mut()
            .expect("BUG: PooledBuilder used after drop")
            .build_handshake(payload)
    }

    /// Build a heartbeat packet
    #[inline]
    pub fn build_heartbeat(&mut self) -> Bytes {
        self.builder
            .as_mut()
            .expect("BUG: PooledBuilder used after drop")
            .build_heartbeat()
    }

    /// Check if events would fit in a single packet
    #[inline]
    pub fn would_fit(&self, events: &[Bytes]) -> bool {
        self.builder
            .as_ref()
            .expect("BUG: PooledBuilder used after drop")
            .would_fit(events)
    }
}

impl Drop for PooledBuilder<'_> {
    fn drop(&mut self) {
        if let Some(mut builder) = self.builder.take() {
            // Update key/session if pool values have changed
            if builder.session_id() != self.pool.session_id {
                builder.set_key_shared(
                    &self.pool.key,
                    self.pool.session_id,
                    self.pool.tx_counter.clone(),
                );
            }
            // Sync origin_hash in case it changed
            builder.set_origin_hash(self.pool.origin_hash);
            // Return to pool (ignore if full)
            let _ = self.pool.builders.push(builder);
        }
    }
}

impl std::fmt::Debug for PooledBuilder<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PooledBuilder")
            .field("has_builder", &self.builder.is_some())
            .finish()
    }
}

// `SharedPacketPool` and `shared_pool` are intentionally absent:
// they were the wrappers around `PacketPool`, and dropping the
// unused `NetSession::packet_pool` getter removed their only
// consumer. `PacketPool` itself is still the underlying type used
// by the thread-local pool internally. Keeping those wrappers with
// no caller would re-invite a cross-pool nonce-reuse hazard.

// ============================================================================
// Thread-Local Pool (Zero-Contention Hot Path)
// ============================================================================

use std::cell::RefCell;
use std::sync::Weak;

/// Per-pool TLS entry. The `Weak<()>` is a liveness witness for
/// the owning `ThreadLocalPool` instance: when the pool is
/// dropped, its `Arc<()>` reaches strong-count 0, the weak fails
/// to upgrade, and the TLS slot is reaped on the next access by
/// any pool. Without this, a long-lived daemon that churns
/// `ThreadLocalPool` instances (NAT rebind, peer reconnect,
/// mesh rebuild) would leak `local_capacity × num_threads`
/// `PacketBuilder` slots — ~16 KB each — for every dropped
/// pool, OOMing in proportion to lifetime peer-churn count.
type LocalBuildersEntry = (Weak<()>, Vec<PacketBuilder>);

thread_local! {
    /// Thread-local cache of fast packet builders, keyed by a unique pool ID
    /// to prevent cross-pool contamination when multiple `ThreadLocalPool`
    /// instances exist (which may use different encryption keys).
    static LOCAL_BUILDERS: RefCell<std::collections::HashMap<u64, LocalBuildersEntry>> =
        RefCell::new(std::collections::HashMap::new());

    /// Per-thread counter that gates the dead-entry reap walk on
    /// `acquire`/`release`. Pre-fix [perf #17/#32 in
    /// `docs/internal/performance/net-perf-analysis.md`] every call ran a
    /// `HashMap::retain` that called `Weak::strong_count()` (an
    /// atomic load) on every entry — at packet rates this dominated
    /// the TLS path's cost (82ns vs 38ns for the shared pool in
    /// published benches). The reap is now amortized: we only walk
    /// once every [`REAP_INTERVAL`] calls.
    ///
    /// `Cell<u32>` rather than `AtomicU32` because this is thread-
    /// local — single-owner, no atomicity required.
    static LOCAL_REAP_COUNTER: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Reap dead `LOCAL_BUILDERS` entries every Nth call. 4096 is a
/// trade-off: small enough that a churned-out pool's TLS slot is
/// reclaimed quickly (~ms at packet rates), large enough that the
/// atomic walk vanishes from the hot path. A bursty workload may
/// briefly hold a few dead entries; each is one `Weak<()>` +
/// `Vec<PacketBuilder>` slot capped at `local_capacity * 2`, so the
/// peak hold-over is bounded by `live_pool_count` worth of
/// slots — same order as the steady-state cache.
const REAP_INTERVAL: u32 = 4096;

/// Increment the per-thread reap counter and, on every Nth call,
/// walk the TLS pool map dropping entries whose owning
/// `ThreadLocalPool` has been deallocated (the `Weak<()>` fails
/// to upgrade). The walk is O(live_pools + dead_pools); typical
/// thread holds 1–2 entries so the work is small even when it
/// fires — the win is moving it off the hot per-call path.
#[inline]
fn maybe_reap_dead_pools(pools: &mut std::collections::HashMap<u64, LocalBuildersEntry>) {
    let should_reap = LOCAL_REAP_COUNTER.with(|c| {
        let next = c.get().wrapping_add(1);
        c.set(next);
        next.is_multiple_of(REAP_INTERVAL)
    });
    if should_reap {
        pools.retain(|_, (weak, _)| weak.strong_count() > 0);
    }
}

/// Global counter for assigning unique IDs to each ThreadLocalPool instance.
static NEXT_POOL_ID: AtomicU64 = AtomicU64::new(0);

/// Thread-local fast packet pool for zero-contention packet building.
///
/// This pool uses thread-local storage to cache packet builders, falling back
/// to a shared `ArrayQueue` when the local cache is empty. This design:
///
/// - Eliminates atomic operations on the hot path (when local cache is warm)
/// - Maintains fairness through periodic returns to shared pool
/// - Auto-refills from shared pool in batches to amortize atomic costs
///
/// # Performance
///
/// When the local cache is warm, `acquire()` and `release()` have zero atomic
/// operations, making them ~10-15% faster than the shared pool under contention.
///
/// # Nonce Safety
///
/// All builders in the pool (including those in thread-local caches) share a
/// single TX counter. This prevents nonce reuse when different threads
/// encrypt with the same key.
pub struct ThreadLocalPool {
    /// Unique ID for this pool instance — used as key in thread-local storage
    /// to prevent cross-pool builder contamination.
    pool_id: u64,
    /// Liveness witness for thread-local entries. Cloned on
    /// `acquire`/`release` and stored alongside each TLS entry
    /// as a `Weak<()>`; when this `Arc<()>` is dropped (the pool
    /// itself drops), every thread's stale entry's `Weak` fails
    /// to upgrade and the entry is reaped on the next TLS
    /// access. See `LocalBuildersEntry` for the full rationale.
    alive: Arc<()>,
    /// Shared fallback pool
    shared: ArrayQueue<PacketBuilder>,
    /// Encryption key for new builders
    key: [u8; 32],
    /// Session ID for builders
    session_id: u64,
    /// Origin hash for L1 identity
    origin_hash: u64,
    /// Maximum builders per thread-local cache
    local_capacity: usize,
    /// Total pool capacity
    capacity: usize,
    /// Shared TX counter — all builders atomically increment this to prevent
    /// nonce reuse across threads.
    tx_counter: Arc<AtomicU64>,
}

impl ThreadLocalPool {
    /// Default number of builders to cache per thread
    pub const DEFAULT_LOCAL_CAPACITY: usize = 8;

    /// Create a new thread-local pool
    pub fn new(size: usize, key: &[u8; 32], session_id: u64) -> Self {
        Self::with_local_capacity(size, key, session_id, 0, Self::DEFAULT_LOCAL_CAPACITY)
    }

    /// Create a new thread-local pool with origin identity
    pub fn with_origin(size: usize, key: &[u8; 32], session_id: u64, origin_hash: u64) -> Self {
        Self::with_local_capacity(
            size,
            key,
            session_id,
            origin_hash,
            Self::DEFAULT_LOCAL_CAPACITY,
        )
    }

    /// Create a new thread-local pool with custom local capacity
    pub fn with_local_capacity(
        size: usize,
        key: &[u8; 32],
        session_id: u64,
        origin_hash: u64,
        local_capacity: usize,
    ) -> Self {
        let tx_counter = Arc::new(AtomicU64::new(0));
        let shared = ArrayQueue::new(size);

        // Pre-populate the shared pool with builders sharing the same TX counter
        for _ in 0..size {
            let _ = shared.push(PacketBuilder::with_shared_counter(
                key,
                session_id,
                origin_hash,
                tx_counter.clone(),
            ));
        }

        Self {
            pool_id: NEXT_POOL_ID.fetch_add(1, Ordering::Relaxed),
            alive: Arc::new(()),
            shared,
            key: *key,
            session_id,
            origin_hash,
            local_capacity,
            capacity: size,
            tx_counter,
        }
    }

    /// Acquire a builder from the pool.
    ///
    /// First tries the thread-local cache (zero atomics), then falls back
    /// to the shared pool, refilling the local cache in batches.
    #[inline]
    pub fn acquire(&self) -> PacketBuilder {
        LOCAL_BUILDERS.with(|pools| {
            let mut pools = pools.borrow_mut();
            // Amortized reap: walk every `REAP_INTERVAL`-th call
            // rather than every call. Pre-fix every acquire ran a
            // `HashMap::retain` + `Weak::strong_count` on every
            // entry — at packet rates that was the dominant cost
            // on the TLS path (perf #17/#32). The reap stays in the
            // same module-level invariant: dropped pools' slots get
            // reaped on the next access by any pool, just not
            // necessarily *this* one.
            maybe_reap_dead_pools(&mut pools);
            let entry = pools
                .entry(self.pool_id)
                .or_insert_with(|| (Arc::downgrade(&self.alive), Vec::new()));
            let pool = &mut entry.1;

            // Fast path: pop from local cache (no atomics)
            if let Some(mut builder) = pool.pop() {
                // Update key/session if changed
                if builder.session_id() != self.session_id {
                    builder.set_key_shared(&self.key, self.session_id, self.tx_counter.clone());
                }
                // Sync origin_hash in case it changed
                builder.set_origin_hash(self.origin_hash);
                return builder;
            }

            // Slow path: refill from shared pool
            let refill_count = self.local_capacity.min(self.shared.len());
            for _ in 0..refill_count {
                if let Some(b) = self.shared.pop() {
                    pool.push(b);
                } else {
                    break;
                }
            }

            // Try local again after refill
            pool.pop()
                .map(|mut b| {
                    if b.session_id() != self.session_id {
                        b.set_key_shared(&self.key, self.session_id, self.tx_counter.clone());
                    }
                    b.set_origin_hash(self.origin_hash);
                    b
                })
                .unwrap_or_else(|| {
                    PacketBuilder::with_shared_counter(
                        &self.key,
                        self.session_id,
                        self.origin_hash,
                        self.tx_counter.clone(),
                    )
                })
        })
    }

    /// Release a builder back to the pool.
    ///
    /// Keeps builders in the thread-local cache up to `local_capacity * 2`,
    /// then returns excess to the shared pool.
    #[inline]
    pub fn release(&self, mut builder: PacketBuilder) {
        // Update key/session if changed
        if builder.session_id() != self.session_id {
            builder.set_key_shared(&self.key, self.session_id, self.tx_counter.clone());
        }
        // Sync origin_hash
        builder.set_origin_hash(self.origin_hash);

        LOCAL_BUILDERS.with(|pools| {
            let mut pools = pools.borrow_mut();
            // Amortized reap: same gate as the acquire path. Pre-
            // fix every release ran a `HashMap::retain` —
            // perf #32.
            maybe_reap_dead_pools(&mut pools);
            let entry = pools
                .entry(self.pool_id)
                .or_insert_with(|| (Arc::downgrade(&self.alive), Vec::new()));
            let pool = &mut entry.1;

            if pool.len() < self.local_capacity * 2 {
                // Keep in local cache
                pool.push(builder);
            } else {
                // Return excess to shared pool
                let _ = self.shared.push(builder);
            }
        })
    }

    /// Get the pool capacity
    #[inline]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Get the number of builders in the shared pool
    #[inline]
    pub fn shared_available(&self) -> usize {
        self.shared.len()
    }

    /// Get the session ID
    #[inline]
    pub fn session_id(&self) -> u64 {
        self.session_id
    }

    /// Get the local capacity per thread
    #[inline]
    pub fn local_capacity(&self) -> usize {
        self.local_capacity
    }
}

impl std::fmt::Debug for ThreadLocalPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThreadLocalPool")
            .field("capacity", &self.capacity)
            .field("shared_available", &self.shared.len())
            .field("local_capacity", &self.local_capacity)
            .field("session_id", &format!("{:016x}", self.session_id))
            .finish()
    }
}

/// RAII guard for a thread-local pooled builder.
pub struct ThreadLocalPooledBuilder<'a> {
    pool: &'a ThreadLocalPool,
    builder: Option<PacketBuilder>,
}

#[expect(
    clippy::expect_used,
    reason = "self.builder is Some between construction and Drop::drop; calling these methods after drop is a caller-side use-after-free invariant violation, not a recoverable runtime condition"
)]
impl<'a> ThreadLocalPooledBuilder<'a> {
    /// Build a packet from events
    #[inline]
    pub fn build(
        &mut self,
        stream_id: u64,
        sequence: u64,
        events: &[Bytes],
        flags: PacketFlags,
    ) -> Bytes {
        self.builder
            .as_mut()
            .expect("BUG: PooledBuilder used after drop")
            .build(stream_id, sequence, events, flags)
    }

    /// Build a handshake packet
    #[inline]
    pub fn build_handshake(&mut self, payload: &[u8]) -> Bytes {
        self.builder
            .as_mut()
            .expect("BUG: PooledBuilder used after drop")
            .build_handshake(payload)
    }

    /// Build a heartbeat packet
    #[inline]
    pub fn build_heartbeat(&mut self) -> Bytes {
        self.builder
            .as_mut()
            .expect("BUG: PooledBuilder used after drop")
            .build_heartbeat()
    }

    /// Build a packet with a subprotocol identifier
    #[inline]
    pub fn build_subprotocol(
        &mut self,
        stream_id: u64,
        sequence: u64,
        events: &[Bytes],
        flags: PacketFlags,
        subprotocol_id: u16,
    ) -> Bytes {
        self.builder
            .as_mut()
            .expect("BUG: PooledBuilder used after drop")
            .build_subprotocol(stream_id, sequence, events, flags, subprotocol_id)
    }

    /// Set the channel hash on the underlying builder so the next
    /// `build*` call stamps it on the outgoing packet header.
    /// Used by the publish path to thread the channel identity onto
    /// the wire (so receiver-side per-channel dispatchers like
    /// nRPC's `register_rpc_inbound` can route).
    #[inline]
    pub fn set_channel_hash(&mut self, channel_hash: u16) {
        self.builder
            .as_mut()
            .expect("BUG: PooledBuilder used after drop")
            .set_channel_hash(channel_hash);
    }

    /// Set the origin hash on the underlying builder so the next
    /// `build*` call stamps it on the outgoing packet header.
    /// Used by the publish path to thread the publisher's chain
    /// identity onto the wire — receivers route per-chain logic
    /// (greedy cache, gravity heat counters) against this value
    /// rather than the protocol-default zero.
    #[inline]
    pub fn set_origin_hash(&mut self, origin_hash: u64) {
        self.builder
            .as_mut()
            .expect("BUG: PooledBuilder used after drop")
            .set_origin_hash(origin_hash);
    }

    /// Check if events would fit in a single packet
    #[inline]
    pub fn would_fit(&self, events: &[Bytes]) -> bool {
        self.builder
            .as_ref()
            .expect("BUG: PooledBuilder used after drop")
            .would_fit(events)
    }
}

impl Drop for ThreadLocalPooledBuilder<'_> {
    fn drop(&mut self) {
        if let Some(builder) = self.builder.take() {
            self.pool.release(builder);
        }
    }
}

impl std::fmt::Debug for ThreadLocalPooledBuilder<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThreadLocalPooledBuilder")
            .field("has_builder", &self.builder.is_some())
            .finish()
    }
}

/// Shared thread-local pool (thread-safe)
pub type SharedLocalPool = Arc<ThreadLocalPool>;

/// Create a shared thread-local pool
pub fn shared_local_pool(size: usize, key: &[u8; 32], session_id: u64) -> SharedLocalPool {
    Arc::new(ThreadLocalPool::new(size, key, session_id))
}

impl ThreadLocalPool {
    /// Get a builder with RAII guard
    #[inline]
    pub fn get(&self) -> ThreadLocalPooledBuilder<'_> {
        ThreadLocalPooledBuilder {
            pool: self,
            builder: Some(self.acquire()),
        }
    }
}

