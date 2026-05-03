//! Zero-allocation packet pool and builder.
//!
//! This module provides pre-allocated buffers for packet construction
//! to avoid heap allocations on the hot path.

use bytes::{Bytes, BytesMut};
use crossbeam_queue::ArrayQueue;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use super::crypto::{session_prefix_from_id, PacketCipher};
use super::protocol::{
    EventFrame, NetHeader, PacketFlags, MAX_PACKET_SIZE, MAX_PAYLOAD_SIZE, NONCE_SIZE,
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
    origin_hash: u32,
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
    pub fn with_origin(key: &[u8; 32], session_id: u64, origin_hash: u32) -> Self {
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
        origin_hash: u32,
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
    pub fn set_origin_hash(&mut self, origin_hash: u32) {
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

        // Reset buffers (no allocation)
        self.payload.clear();
        self.packet.clear();

        // Write event frames to payload buffer
        EventFrame::write_events(events, &mut self.payload);

        // Build and serialize header once (nonce is placeholder, will be patched)
        let header = NetHeader::new(
            self.session_id,
            stream_id,
            sequence,
            [0u8; NONCE_SIZE],
            self.payload.len() as u16,
            events.len() as u16,
            flags,
        )
        .with_origin(self.origin_hash)
        .with_channel_hash(self.channel_hash);
        let aad = header.aad();
        let mut header_bytes = header.to_bytes();

        // Encrypt payload in-place and get the counter used.
        // ChaCha20-Poly1305 encryption cannot fail with valid inputs —
        // an error here indicates memory corruption or a cipher library bug.
        let counter = match self.cipher.encrypt_in_place(&aad, &mut self.payload) {
            Ok(c) => c,
            Err(e) => panic!(
                "BUG: ChaCha20-Poly1305 encryption failed (session={:016x}, payload_len={}): {}",
                self.session_id,
                self.payload.len(),
                e
            ),
        };

        // Patch nonce into serialized header (bytes 12..24). The
        // 4-byte prefix must be derived the same way the cipher does
        // (`session_prefix_from_id`), otherwise the receiver will
        // reconstruct a different nonce and AEAD verification fails.
        header_bytes[12..16].copy_from_slice(&session_prefix_from_id(self.session_id));
        header_bytes[16..24].copy_from_slice(&counter.to_le_bytes());

        // Patch payload_len to exclude the 16-byte auth tag (bytes 60..62).
        // The `as u16` cast is safe under current MAX_PAYLOAD_SIZE
        // (8112 << u16::MAX), but a future config that raised the cap
        // past `u16::MAX + 16` would silently truncate the wire
        // length and the receiver would mis-frame. The
        // debug_assert is a tripwire so the truncation surfaces in
        // CI before it reaches production.
        debug_assert!(
            self.payload.len() - 16 <= u16::MAX as usize,
            "payload length {} would truncate the u16 wire field; \
             revisit MAX_PAYLOAD_SIZE before raising the cap past u16::MAX + 16",
            self.payload.len() - 16,
        );
        let payload_len = (self.payload.len() - 16) as u16;
        header_bytes[60..62].copy_from_slice(&payload_len.to_le_bytes());

        // Assemble packet: header + encrypted_payload + tag
        self.packet.extend_from_slice(&header_bytes);
        self.packet.extend_from_slice(&self.payload);

        // split() transfers ownership without atomic ref-count bump
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

        self.payload.clear();
        self.packet.clear();

        EventFrame::write_events(events, &mut self.payload);

        let header = NetHeader::new(
            self.session_id,
            stream_id,
            sequence,
            [0u8; NONCE_SIZE],
            self.payload.len() as u16,
            events.len() as u16,
            flags,
        )
        .with_origin(self.origin_hash)
        .with_channel_hash(self.channel_hash)
        .with_subprotocol(subprotocol_id);
        let aad = header.aad();
        let mut header_bytes = header.to_bytes();

        let counter = match self.cipher.encrypt_in_place(&aad, &mut self.payload) {
            Ok(c) => c,
            Err(e) => panic!(
                "BUG: ChaCha20-Poly1305 encryption failed (session={:016x}): {}",
                self.session_id, e
            ),
        };

        header_bytes[12..16].copy_from_slice(&session_prefix_from_id(self.session_id));
        header_bytes[16..24].copy_from_slice(&counter.to_le_bytes());
        // See `build()` for why this debug_assert exists. Same
        // safety argument: `as u16` truncates if MAX_PAYLOAD_SIZE
        // is ever raised past u16::MAX + 16.
        debug_assert!(
            self.payload.len() - 16 <= u16::MAX as usize,
            "payload length {} would truncate the u16 wire field; \
             revisit MAX_PAYLOAD_SIZE before raising the cap past u16::MAX + 16",
            self.payload.len() - 16,
        );
        let payload_len = (self.payload.len() - 16) as u16;
        header_bytes[60..62].copy_from_slice(&payload_len.to_le_bytes());

        self.packet.extend_from_slice(&header_bytes);
        self.packet.extend_from_slice(&self.payload);
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
        header_bytes[60..62].copy_from_slice(&payload_len.to_le_bytes());

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
    origin_hash: u32,
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
    pub fn with_origin(size: usize, key: &[u8; 32], session_id: u64, origin_hash: u32) -> Self {
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
    origin_hash: u32,
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
    pub fn with_origin(size: usize, key: &[u8; 32], session_id: u64, origin_hash: u32) -> Self {
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
        origin_hash: u32,
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
            // Reap any TLS entries whose owning pool has been
            // dropped (Weak fails to upgrade). Cheap: typical
            // entry count per thread is 1–2; the `retain` walk
            // is amortized into every TLS access.
            pools.retain(|_, (weak, _)| weak.strong_count() > 0);
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
            // Reap dead entries on the release path too — release
            // may run on threads that haven't called acquire
            // recently and would otherwise hold dead entries
            // until their next acquire.
            pools.retain(|_, (weak, _)| weak.strong_count() > 0);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_thread_local_pool_basic() {
        let key = [0x42u8; 32];
        let session_id = 0x1234567890ABCDEF;
        let pool = ThreadLocalPool::new(8, &key, session_id);

        assert_eq!(pool.capacity(), 8);
        assert_eq!(pool.session_id(), session_id);
        assert_eq!(
            pool.local_capacity(),
            ThreadLocalPool::DEFAULT_LOCAL_CAPACITY
        );
    }

    #[test]
    fn test_thread_local_pool_acquire_release() {
        let key = [0x42u8; 32];
        let session_id = 0xDEADBEEF;
        let pool = ThreadLocalPool::new(4, &key, session_id);

        // Acquire a builder
        let builder = pool.acquire();
        assert_eq!(builder.session_id(), session_id);

        // Release it back
        pool.release(builder);

        // Acquire again - should get from local cache (no atomics)
        let builder2 = pool.acquire();
        assert_eq!(builder2.session_id(), session_id);
        pool.release(builder2);
    }

    #[test]
    fn test_thread_local_pool_raii_guard() {
        let key = [0x42u8; 32];
        let session_id = 0xCAFEBABE;
        let pool = ThreadLocalPool::new(4, &key, session_id);

        {
            let mut builder = pool.get();
            let events = vec![Bytes::from_static(b"test event")];
            let packet = builder.build(1, 42, &events, PacketFlags::NONE);

            // Verify packet was built correctly
            let header = NetHeader::from_bytes(&packet).unwrap();
            assert_eq!(header.stream_id, 1);
            assert_eq!(header.sequence, 42);
            assert_eq!(header.event_count, 1);
        }
        // Builder automatically returned to pool on drop
    }

    #[test]
    fn test_thread_local_pool_batch_refill() {
        let key = [0x42u8; 32];
        let session_id = 0x1111;
        let pool = ThreadLocalPool::with_local_capacity(16, &key, session_id, 0, 4);

        // Acquire multiple builders to trigger batch refill
        let mut builders = Vec::new();
        for _ in 0..8 {
            builders.push(pool.acquire());
        }

        // All should have correct session ID
        for b in &builders {
            assert_eq!(b.session_id(), session_id);
        }

        // Release all back
        for b in builders {
            pool.release(b);
        }
    }

    #[test]
    fn test_thread_local_pool_overflow_to_shared() {
        let key = [0x42u8; 32];
        let session_id = 0x2222;
        // local_capacity = 2, so local cache holds up to 4 (2 * 2)
        let pool = ThreadLocalPool::with_local_capacity(8, &key, session_id, 0, 2);

        // Acquire and release many builders
        for _ in 0..10 {
            let b = pool.acquire();
            pool.release(b);
        }

        // Pool should still function correctly
        let builder = pool.acquire();
        assert_eq!(builder.session_id(), session_id);
    }

    #[test]
    fn test_shared_local_pool() {
        let key = [0x42u8; 32];
        let session_id = 0x3333;
        let pool = shared_local_pool(8, &key, session_id);

        let pool_clone = pool.clone();

        // Both references work
        let _b1 = pool.get();
        let _b2 = pool_clone.get();
    }

    // ---- Regression tests ----

    #[test]
    fn test_regression_pool_builders_share_tx_counter() {
        // Regression: each PacketBuilder in the pool had an independent
        // PacketCipher with its own counter starting at 0. Two builders
        // from the same pool would produce overlapping nonces with the
        // same key, catastrophically breaking ChaCha20-Poly1305 security
        // (nonce reuse enables plaintext recovery via XOR).
        //
        // Fix: all builders in a pool share a single AtomicU64 TX counter.
        let key = [0x42u8; 32];
        let session_id = 0xAAAA;
        let pool = PacketPool::new(4, &key, session_id);

        let events = vec![Bytes::from_static(b"test")];

        // Get two different builders from the pool
        let mut b1 = pool.get();
        let pkt1 = b1.build(0, 0, &events, PacketFlags::NONE);
        drop(b1);

        let mut b2 = pool.get();
        let pkt2 = b2.build(0, 1, &events, PacketFlags::NONE);
        drop(b2);

        // Extract nonces from the two packets (bytes 12..24 of the header)
        let nonce1 = &pkt1[12..24];
        let nonce2 = &pkt2[12..24];

        assert_ne!(
            nonce1, nonce2,
            "two builders from the same pool must produce different nonces"
        );

        // Verify counters are sequential (both share the same AtomicU64)
        let counter1 = u64::from_le_bytes(nonce1[4..12].try_into().unwrap());
        let counter2 = u64::from_le_bytes(nonce2[4..12].try_into().unwrap());
        assert_eq!(counter1, 0, "first builder should use counter 0");
        assert_eq!(counter2, 1, "second builder should use counter 1");
    }

    #[test]
    fn test_regression_thread_local_pool_builders_share_tx_counter() {
        // Same nonce-reuse regression as PacketPool, but for ThreadLocalPool.
        // Different threads get different builders from the shared pool;
        // without a shared counter they'd produce overlapping nonces.
        let key = [0x42u8; 32];
        let session_id = 0xBBBB;
        let pool = Arc::new(ThreadLocalPool::new(4, &key, session_id));

        // Build packets from two different threads to ensure they get
        // different builders (each thread has its own local cache).
        let pool1 = pool.clone();
        let pkt1 = std::thread::spawn(move || {
            let mut b = pool1.get();
            b.build(0, 0, &[Bytes::from_static(b"test")], PacketFlags::NONE)
        })
        .join()
        .unwrap();

        let pool2 = pool.clone();
        let pkt2 = std::thread::spawn(move || {
            let mut b = pool2.get();
            b.build(0, 1, &[Bytes::from_static(b"test")], PacketFlags::NONE)
        })
        .join()
        .unwrap();

        let nonce1 = &pkt1[12..24];
        let nonce2 = &pkt2[12..24];

        assert_ne!(
            nonce1, nonce2,
            "builders from different threads must produce different nonces"
        );

        let counter1 = u64::from_le_bytes(nonce1[4..12].try_into().unwrap());
        let counter2 = u64::from_le_bytes(nonce2[4..12].try_into().unwrap());
        // Counters should be 0 and 1 (order depends on thread scheduling)
        assert_ne!(
            counter1, counter2,
            "shared counter must prevent nonce reuse across threads"
        );
    }

    #[test]
    fn test_concurrent_pool_no_nonce_collision() {
        // Stress test: 8 threads each build 100 packets concurrently
        // from the same pool. No two packets should share a nonce.
        use std::collections::HashSet;
        use std::sync::Mutex;
        use std::thread;

        let key = [0x42u8; 32];
        let session_id = 0xCCCC;
        let pool = Arc::new(ThreadLocalPool::new(16, &key, session_id));
        let nonces = Arc::new(Mutex::new(Vec::new()));

        let num_threads = 8;
        let packets_per_thread = 100;

        let mut handles = Vec::new();
        for _ in 0..num_threads {
            let pool = pool.clone();
            let nonces = nonces.clone();
            handles.push(thread::spawn(move || {
                let mut local_nonces = Vec::with_capacity(packets_per_thread);
                for seq in 0..packets_per_thread {
                    let mut b = pool.get();
                    let pkt = b.build(
                        0,
                        seq as u64,
                        &[Bytes::from_static(b"x")],
                        PacketFlags::NONE,
                    );
                    // Extract nonce (bytes 12..24 of header)
                    let mut nonce = [0u8; 12];
                    nonce.copy_from_slice(&pkt[12..24]);
                    local_nonces.push(nonce);
                }
                nonces.lock().unwrap().extend(local_nonces);
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        let all_nonces = nonces.lock().unwrap();
        assert_eq!(all_nonces.len(), num_threads * packets_per_thread);

        let unique: HashSet<_> = all_nonces.iter().collect();
        assert_eq!(
            unique.len(),
            all_nonces.len(),
            "all {} nonces must be unique — found {} duplicates",
            all_nonces.len(),
            all_nonces.len() - unique.len()
        );
    }

    #[test]
    fn test_regression_thread_local_pool_isolation() {
        // Regression: ThreadLocalPool used a single global Vec in thread-local
        // storage without keying by pool instance. Two pools with DIFFERENT
        // encryption keys would share the same builder cache, so a builder
        // released to pool A could be acquired from pool B — silently
        // encrypting with the wrong key.
        //
        // Fix: the thread-local cache is a HashMap<u64, Vec<PacketBuilder>>
        // keyed by a unique pool_id assigned at construction time.
        let key_a = [0xAAu8; 32];
        let key_b = [0xBBu8; 32];
        let session_a = 0x1111;
        let session_b = 0x2222;

        let pool_a = ThreadLocalPool::new(4, &key_a, session_a);
        let pool_b = ThreadLocalPool::new(4, &key_b, session_b);

        // Acquire from pool A, release back to pool A
        let builder_a = pool_a.acquire();
        assert_eq!(builder_a.session_id(), session_a);
        pool_a.release(builder_a);

        // Acquire from pool B — must get a builder with session_b,
        // NOT the builder we just released to pool A.
        let builder_b = pool_b.acquire();
        assert_eq!(
            builder_b.session_id(),
            session_b,
            "builder acquired from pool B must have pool B's session_id, \
             not pool A's — thread-local cache must be keyed by pool_id"
        );
        pool_b.release(builder_b);

        // Acquire from pool A again — should still get session_a
        let builder_a2 = pool_a.acquire();
        assert_eq!(
            builder_a2.session_id(),
            session_a,
            "builder acquired from pool A after pool B activity must still \
             have pool A's session_id"
        );
    }

    /// Pin: TLS entries for dropped `ThreadLocalPool` instances
    /// are reaped on the next access. Pre-fix `LOCAL_BUILDERS`
    /// kept the entry forever, leaking ~16 KB × `local_capacity`
    /// per dropped pool per thread that ever touched it. The
    /// bug surfaced as production OOM in proportion to
    /// peer-churn count on long-lived daemons.
    #[test]
    fn dropped_thread_local_pool_evicts_tls_entry_on_next_access() {
        // Build a pool, populate its TLS slot, then drop it.
        // Construct another pool and call acquire to trigger
        // the reap pass. Snapshot the TLS map size before and
        // after to confirm the dead entry was removed.
        let key = [0x33u8; 32];

        // Capture the TLS map's pre-state to isolate from
        // unrelated entries left over from prior tests.
        let pre_size = LOCAL_BUILDERS.with(|m| m.borrow().len());

        let pool_a = ThreadLocalPool::new(4, &key, 0xA);
        // Touch acquire to populate the TLS slot.
        let b = pool_a.acquire();
        pool_a.release(b);
        let pool_a_id = pool_a.pool_id;
        let with_a = LOCAL_BUILDERS.with(|m| m.borrow().contains_key(&pool_a_id));
        assert!(
            with_a,
            "pool A's TLS slot must be populated after acquire/release"
        );

        // Drop the pool.
        drop(pool_a);

        // Without doing any TLS access, the entry is still
        // present (Drop on `ThreadLocalPool` doesn't walk every
        // thread's TLS — that's not feasible from within a
        // single thread's drop). The reap happens on the next
        // access to `LOCAL_BUILDERS`, regardless of which pool
        // triggered it.
        //
        // Trigger the reap by acquire-ing from a fresh pool.
        let pool_b = ThreadLocalPool::new(4, &key, 0xB);
        let b = pool_b.acquire();
        pool_b.release(b);

        // After the reap, the dead pool's entry is gone and
        // only pool B's entry remains (modulo any pre-existing
        // entries from earlier tests).
        let post_size = LOCAL_BUILDERS.with(|m| m.borrow().len());
        let still_has_a = LOCAL_BUILDERS.with(|m| m.borrow().contains_key(&pool_a_id));
        assert!(
            !still_has_a,
            "pool A's dead TLS entry must be reaped on next access — \
             pre-fix this leaked forever (production OOM under peer churn)"
        );
        // The TLS map should not have grown: pool A's entry
        // was removed and pool B's added, so size <= pre_size + 1.
        assert!(
            post_size <= pre_size + 1,
            "TLS map size grew unexpectedly: pre={} post={}",
            pre_size,
            post_size
        );
    }

    #[test]
    fn test_regression_set_key_drains_stale_builders() {
        // Regression: PacketPool::set_key created a new tx_counter starting
        // at 0 but did not drain existing builders from the queue. Those
        // stale builders still held the old counter (also starting at 0),
        // so nonces from before and after the key rotation could collide
        // — a catastrophic nonce-reuse vulnerability for ChaCha20-Poly1305.
        //
        // Fix: set_key now drains the queue so stale builders are never
        // reused. New builders are lazily created with the correct key and
        // counter on the next get().
        use std::collections::HashSet;

        let key1 = [0x11u8; 32];
        let key2 = [0x22u8; 32];
        let session1 = 0xAAAA;
        let session2 = 0xBBBB;

        let mut pool = PacketPool::new(4, &key1, session1);
        let events = vec![Bytes::from_static(b"test")];

        // Build packets with the first key
        let mut nonces_before = Vec::new();
        for seq in 0..3u64 {
            let mut b = pool.get();
            let pkt = b.build(0, seq, &events, PacketFlags::NONE);
            let mut nonce = [0u8; 12];
            nonce.copy_from_slice(&pkt[12..24]);
            nonces_before.push(nonce);
        }

        // Rotate key
        pool.set_key(&key2, session2);

        // All pooled builders should have been drained
        assert_eq!(
            pool.available(),
            0,
            "set_key must drain stale builders from the pool"
        );

        // Build packets with the new key — counters restart at 0
        let mut nonces_after = Vec::new();
        for seq in 0..3u64 {
            let mut b = pool.get();
            let pkt = b.build(0, seq, &events, PacketFlags::NONE);
            let mut nonce = [0u8; 12];
            nonce.copy_from_slice(&pkt[12..24]);
            nonces_after.push(nonce);
        }

        // Even though counters restarted at 0, the session_id prefix
        // in the nonce changed, so all nonces must still be unique.
        let all_nonces: Vec<_> = nonces_before.iter().chain(&nonces_after).collect();
        let unique: HashSet<_> = all_nonces.iter().collect();
        assert_eq!(
            unique.len(),
            all_nonces.len(),
            "nonces must not collide across key rotations"
        );
    }

    /// Passing more than `MAX_EVENTS_PER_PACKET` events to
    /// `build` must panic, not silently wrap on the `events.len()
    /// as u16` cast. A wrapped value below the receiver's cap
    /// would otherwise pass `validate()` and corrupt frame parsing.
    #[test]
    #[should_panic(expected = "PacketBuilder::build called with")]
    fn build_panics_on_event_count_exceeding_cap() {
        let key = [0x42u8; 32];
        let session_id = 0xCAFEu64;
        let pool = ThreadLocalPool::new(2, &key, session_id);
        let mut builder = pool.get();
        // Fabricate `MAX_EVENTS_PER_PACKET + 1` empty events. The
        // payload bytes don't matter for the cap check — `build`
        // must reject before any frame writing.
        let too_many: Vec<Bytes> = (0..=NetHeader::MAX_EVENTS_PER_PACKET as usize)
            .map(|_| Bytes::new())
            .collect();
        let _ = builder.build(0, 0, &too_many, PacketFlags::NONE);
    }

    /// `build_subprotocol` shares the same cap.
    #[test]
    #[should_panic(expected = "PacketBuilder::build_subprotocol called with")]
    fn build_subprotocol_panics_on_event_count_exceeding_cap() {
        let key = [0x42u8; 32];
        let session_id = 0xBABEu64;
        let pool = ThreadLocalPool::new(2, &key, session_id);
        let mut builder = pool.get();
        let too_many: Vec<Bytes> = (0..=NetHeader::MAX_EVENTS_PER_PACKET as usize)
            .map(|_| Bytes::new())
            .collect();
        let _ = builder.build_subprotocol(0, 0, &too_many, PacketFlags::NONE, 1);
    }

    /// Boundary: exactly `MAX_EVENTS_PER_PACKET` events
    /// must be accepted without panic (and without silent
    /// truncation since 2027 fits in `u16`).
    #[test]
    fn build_accepts_exactly_max_events_per_packet() {
        let key = [0x42u8; 32];
        let session_id = 0x4242u64;
        let pool = ThreadLocalPool::new(2, &key, session_id);
        let mut builder = pool.get();
        // The actual event payload size matters because
        // `EventFrame::write_events` will run; use empty events
        // to keep the test cheap. The receiver's `validate()`
        // would reject an event count above `MAX_EVENTS_PER_PACKET`,
        // but the boundary value is inclusive in both the cap
        // and the validator.
        //
        // We test with 1 event (well under the cap, sanity) and
        // also confirm the boundary value would not panic by
        // constructing a vector of zero-byte events at the cap.
        let one_event = vec![Bytes::from_static(b"hi")];
        let _ = builder.build(0, 0, &one_event, PacketFlags::NONE);

        let cap_events: Vec<Bytes> = (0..NetHeader::MAX_EVENTS_PER_PACKET as usize)
            .map(|_| Bytes::new())
            .collect();
        // No panic — boundary inclusive.
        let _ = builder.build(0, 1, &cap_events, PacketFlags::NONE);
    }
}
