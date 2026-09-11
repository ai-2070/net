//! Cryptographic primitives for Net.
//!
//! This module provides:
//! - Noise protocol handshake (NKpsk0 pattern)
//! - ChaCha20-Poly1305 AEAD encryption with counter-based nonces
//! - Key derivation for session keys

use bytes::{Bytes, BytesMut};
use parking_lot::Mutex;
use crate::aead::AeadKey;
use snow::{params::NoiseParams, Builder, HandshakeState};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::protocol::{NONCE_SIZE, TAG_SIZE};

/// Noise protocol pattern: NKpsk0
///
/// - N: No static key for initiator (anonymous)
/// - K: Responder's static key is known to initiator
/// - psk0: Pre-shared key mixed at start
const NOISE_PATTERN: &str = "Noise_NKpsk0_25519_ChaChaPoly_BLAKE2s";

/// Domain-separated Noise prologue binding `(src_node_id, dest_node_id)`
/// into the handshake transcript.
///
/// Both direct and relayed handshakes use this construction. A relay
/// that rewrites either node id in the outer addressing (routing header
/// for routed handshakes, or the caller's own `peer_node_id` argument
/// for direct) produces a prologue mismatch on the responder, which
/// fails the Noise MAC check on msg1 — the handshake is rejected
/// end-to-end before any session keys are bound to an attacker-chosen
/// identity.
pub fn handshake_prologue(src_node_id: u64, dest_node_id: u64) -> [u8; 32] {
    let mut buf = [0u8; 32];
    buf[0..16].copy_from_slice(b"net-handshake-v1");
    buf[16..24].copy_from_slice(&src_node_id.to_le_bytes());
    buf[24..32].copy_from_slice(&dest_node_id.to_le_bytes());
    buf
}

/// Error type for cryptographic operations
#[derive(Debug, Clone)]
pub enum CryptoError {
    /// Handshake failed
    Handshake(String),
    /// Encryption failed
    Encryption(String),
    /// Decryption failed
    Decryption(String),
    /// Invalid key
    InvalidKey(String),
    /// Invalid nonce
    InvalidNonce,
}

impl std::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Handshake(msg) => write!(f, "handshake error: {}", msg),
            Self::Encryption(msg) => write!(f, "encryption error: {}", msg),
            Self::Decryption(msg) => write!(f, "decryption error: {}", msg),
            Self::InvalidKey(msg) => write!(f, "invalid key: {}", msg),
            Self::InvalidNonce => write!(f, "invalid nonce"),
        }
    }
}

impl std::error::Error for CryptoError {}

/// Session keys derived from Noise handshake
#[derive(Clone)]
pub struct SessionKeys {
    /// Key for encrypting outbound packets
    pub tx_key: [u8; 32],
    /// Key for decrypting inbound packets
    pub rx_key: [u8; 32],
    /// Session ID derived from handshake
    pub session_id: u64,
    /// The remote peer's Noise static public key (X25519). 32 bytes
    /// of public material extracted from the handshake before
    /// transitioning into transport mode. Load-bearing for the
    /// identity-envelope path in daemon migration: the source seals
    /// the daemon's ed25519 seed to this key, knowing the only
    /// party that can unseal it is the peer whose static private
    /// key completed this handshake. `[0; 32]` is a sentinel for
    /// "not available" — some test paths construct `SessionKeys`
    /// directly and don't go through a real handshake.
    pub remote_static_pub: [u8; 32],
    /// Key authenticating route-hop envelopes this node SENDS on this
    /// edge (SUBNET_AUTH_PLAN.md D6).
    ///
    /// Derived from the same handshake hash as the packet keys but
    /// under distinct labels, so route-hop authentication and packet
    /// AEAD never share key material or nonce/sequence space. A relay
    /// authenticates its adjacent hop with these while the inner
    /// end-to-end packet stays sealed under the packet keys it cannot
    /// read.
    pub route_hop_tx_key: [u8; 32],
    /// Key verifying route-hop envelopes this node RECEIVES on this
    /// edge. Mirrors the peer's `route_hop_tx_key`.
    pub route_hop_rx_key: [u8; 32],
}

impl std::fmt::Debug for SessionKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionKeys")
            .field("session_id", &self.session_id)
            .field("tx_key", &"[REDACTED]")
            .field("rx_key", &"[REDACTED]")
            .field(
                "remote_static_pub",
                &format_args!(
                    "{:02x}{:02x}{:02x}{:02x}…",
                    self.remote_static_pub[0],
                    self.remote_static_pub[1],
                    self.remote_static_pub[2],
                    self.remote_static_pub[3],
                ),
            )
            .finish()
    }
}

/// Static keypair for Noise protocol
#[derive(Clone)]
pub struct StaticKeypair {
    /// Private key (32 bytes)
    pub private: [u8; 32],
    /// Public key (32 bytes)
    pub public: [u8; 32],
}

impl StaticKeypair {
    /// Generate a new random keypair
    #[expect(
        clippy::expect_used,
        reason = "NOISE_PATTERN is a compile-time-constant string, parses infallibly; the Noise builder generates keypairs deterministically from valid patterns"
    )]
    pub fn generate() -> Self {
        let builder = Builder::new(
            NOISE_PATTERN
                .parse()
                .expect("static noise pattern is valid"),
        );
        let keypair = builder
            .generate_keypair()
            .expect("keypair generation from valid pattern");
        let mut private = [0u8; 32];
        let mut public = [0u8; 32];
        private.copy_from_slice(&keypair.private);
        public.copy_from_slice(&keypair.public);
        Self { private, public }
    }

    /// Create from existing keys
    pub fn from_keys(private: [u8; 32], public: [u8; 32]) -> Self {
        Self { private, public }
    }

    /// Get the public key
    #[inline]
    pub fn public_key(&self) -> &[u8; 32] {
        &self.public
    }

    /// Get the secret/private key
    #[inline]
    pub fn secret_key(&self) -> &[u8; 32] {
        &self.private
    }
}

impl std::fmt::Debug for StaticKeypair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StaticKeypair")
            .field("public", &hex_string(&self.public))
            .field("private", &"[REDACTED]")
            .finish()
    }
}

/// Noise handshake state machine
pub struct NoiseHandshake {
    state: HandshakeState,
    is_initiator: bool,
}

impl NoiseHandshake {
    /// Create initiator handshake state with an empty prologue.
    ///
    /// The initiator knows the responder's static public key.
    pub fn initiator(psk: &[u8; 32], responder_static: &[u8; 32]) -> Result<Self, CryptoError> {
        Self::initiator_with_prologue(psk, responder_static, &[])
    }

    /// Create initiator handshake state with a caller-supplied prologue.
    ///
    /// The prologue is mixed into the Noise handshake hash but never sent
    /// on the wire. Both peers must use byte-identical prologues or `msg1`
    /// will fail to authenticate. Used by the relayed-handshake path to
    /// bind the `(dest_node_id, src_node_id)` in the plaintext envelope
    /// into the Noise transcript — a relay that rewrites either field
    /// produces a prologue mismatch on the responder, and the attack is
    /// detected as a Noise `read_message` failure.
    pub fn initiator_with_prologue(
        psk: &[u8; 32],
        responder_static: &[u8; 32],
        prologue: &[u8],
    ) -> Result<Self, CryptoError> {
        let params: NoiseParams = NOISE_PATTERN
            .parse()
            .map_err(|e| CryptoError::Handshake(format!("invalid noise params: {}", e)))?;

        let state = Builder::new(params)
            .psk(0, psk)
            .map_err(|e| CryptoError::Handshake(format!("failed to set psk: {}", e)))?
            .prologue(prologue)
            .map_err(|e| CryptoError::Handshake(format!("failed to set prologue: {}", e)))?
            .remote_public_key(responder_static)
            .map_err(|e| CryptoError::Handshake(format!("failed to set remote key: {}", e)))?
            .build_initiator()
            .map_err(|e| CryptoError::Handshake(format!("failed to build initiator: {}", e)))?;

        Ok(Self {
            state,
            is_initiator: true,
        })
    }

    /// Create responder handshake state with an empty prologue.
    ///
    /// The responder uses its static keypair for authentication.
    pub fn responder(psk: &[u8; 32], static_keypair: &StaticKeypair) -> Result<Self, CryptoError> {
        Self::responder_with_prologue(psk, static_keypair, &[])
    }

    /// Create responder handshake state with a caller-supplied prologue.
    ///
    /// See [`Self::initiator_with_prologue`] for the authentication story.
    pub fn responder_with_prologue(
        psk: &[u8; 32],
        static_keypair: &StaticKeypair,
        prologue: &[u8],
    ) -> Result<Self, CryptoError> {
        let params: NoiseParams = NOISE_PATTERN
            .parse()
            .map_err(|e| CryptoError::Handshake(format!("invalid noise params: {}", e)))?;

        let state = Builder::new(params)
            .psk(0, psk)
            .map_err(|e| CryptoError::Handshake(format!("failed to set psk: {}", e)))?
            .prologue(prologue)
            .map_err(|e| CryptoError::Handshake(format!("failed to set prologue: {}", e)))?
            .local_private_key(&static_keypair.private)
            .map_err(|e| CryptoError::Handshake(format!("failed to set local key: {}", e)))?
            .build_responder()
            .map_err(|e| CryptoError::Handshake(format!("failed to build responder: {}", e)))?;

        Ok(Self {
            state,
            is_initiator: false,
        })
    }

    /// Check if handshake is complete
    #[inline]
    pub fn is_finished(&self) -> bool {
        self.state.is_handshake_finished()
    }

    /// Check if we're the initiator
    #[inline]
    #[allow(dead_code)]
    pub fn is_initiator(&self) -> bool {
        self.is_initiator
    }

    /// Write a handshake message
    ///
    /// Returns the message to send to the peer.
    pub fn write_message(&mut self, payload: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let mut buf = vec![0u8; 65535];
        let len = self
            .state
            .write_message(payload, &mut buf)
            .map_err(|e| CryptoError::Handshake(format!("write_message failed: {}", e)))?;
        buf.truncate(len);
        Ok(buf)
    }

    /// Read a handshake message
    ///
    /// Returns the decrypted payload from the peer.
    pub fn read_message(&mut self, message: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let mut buf = vec![0u8; 65535];
        let len = self
            .state
            .read_message(message, &mut buf)
            .map_err(|e| CryptoError::Handshake(format!("read_message failed: {}", e)))?;
        buf.truncate(len);
        Ok(buf)
    }

    /// Complete the handshake and extract session keys.
    ///
    /// This consumes the handshake state and returns the symmetric keys
    /// for stateless packet encryption.
    pub fn into_session_keys(self) -> Result<SessionKeys, CryptoError> {
        if !self.is_finished() {
            return Err(CryptoError::Handshake("handshake not finished".to_string()));
        }

        let is_initiator = self.is_initiator;

        // Get the handshake hash before transitioning (HandshakeState has this method)
        let handshake_hash: [u8; 32] = {
            let hash_slice = self.state.get_handshake_hash();
            let mut arr = [0u8; 32];
            let len = hash_slice.len().min(32);
            arr[..len].copy_from_slice(&hash_slice[..len]);
            arr
        };

        // Capture the remote static pubkey BEFORE `into_transport_mode`
        // consumes the handshake state. Populated on both sides of
        // NKpsk0: initiator learned it out-of-band and handed it to
        // snow via `remote_public_key`; responder learned it from
        // `-> s` in the Noise pattern. Zero-filled if snow returns
        // `None` (shouldn't happen post-handshake but `get_remote_static`
        // is nominally fallible).
        let mut remote_static_pub = [0u8; 32];
        if let Some(rs) = self.state.get_remote_static() {
            let len = rs.len().min(32);
            remote_static_pub[..len].copy_from_slice(&rs[..len]);
        }

        // Transition to transport mode (we don't need the transport state since we're using stateless encryption)
        let _transport = self
            .state
            .into_transport_mode()
            .map_err(|e| CryptoError::Handshake(format!("transport mode failed: {}", e)))?;

        // Derive session ID from handshake hash
        #[expect(
            clippy::unwrap_used,
            reason = "handshake_hash typed as [u8; 32] above; [0..8].try_into::<[u8; 8]>() is infallible"
        )]
        let session_id = u64::from_le_bytes(handshake_hash[0..8].try_into().unwrap());

        // Use HKDF to derive tx and rx keys from handshake hash
        // For NKpsk0, initiator sends first, so:
        // - Initiator: tx_key from first half, rx_key from second half
        // - Responder: rx_key from first half, tx_key from second half
        let mut tx_key = [0u8; 32];
        let mut rx_key = [0u8; 32];

        // Simple key derivation from handshake hash
        // In production, use proper HKDF
        if is_initiator {
            derive_key(&handshake_hash, b"initiator-tx", &mut tx_key);
            derive_key(&handshake_hash, b"initiator-rx", &mut rx_key);
        } else {
            derive_key(&handshake_hash, b"initiator-rx", &mut tx_key);
            derive_key(&handshake_hash, b"initiator-tx", &mut rx_key);
        }

        // Route-hop authentication keys, derived here because this is
        // the last point the handshake hash exists — `into_transport_mode`
        // above already consumed the state. Distinct labels from the
        // packet keys: a route-hop MAC must never be forgeable from
        // packet key material, and the two have independent sequence
        // spaces. Directional, so a captured envelope cannot be
        // reflected back along the edge it came from.
        let mut route_hop_tx_key = [0u8; 32];
        let mut route_hop_rx_key = [0u8; 32];
        if is_initiator {
            derive_key(&handshake_hash, b"route-hop-tx-v1", &mut route_hop_tx_key);
            derive_key(&handshake_hash, b"route-hop-rx-v1", &mut route_hop_rx_key);
        } else {
            derive_key(&handshake_hash, b"route-hop-rx-v1", &mut route_hop_tx_key);
            derive_key(&handshake_hash, b"route-hop-tx-v1", &mut route_hop_rx_key);
        }

        Ok(SessionKeys {
            tx_key,
            rx_key,
            session_id,
            remote_static_pub,
            route_hop_tx_key,
            route_hop_rx_key,
        })
    }
}

/// Build the `ring` AEAD key for the packet path.
///
/// Boxed because `ring::aead::LessSafeKey` is 544 bytes — its inner
/// `UnboundKey` is sized to ring's largest AEAD variant (AES-256-GCM's
/// expanded key schedule + GHASH table), even though this path only ever
/// holds a 32-byte ChaCha20-Poly1305 key. `PacketCipher` is embedded by
/// value in every `PacketBuilder`, and `PacketBuilder`s are moved through
/// the packet pool's `ArrayQueue` on every `get()`/`release()`. Inlining
/// the 544-byte key tripled `PacketBuilder`'s size (~304 → ~816 B) and
/// regressed the pure pool get/return path ~70% (`net_packet_pool`,
/// `pool_comparison`, `pool_contention` — the latter worst, from the
/// fatter slots' cache traffic). Boxing moves an 8-byte pointer instead;
/// the heap allocation is paid only on cipher construction (pool
/// pre-fill / refill / rekey — all cold), never on the steady-state
/// reuse path, and the extra indirection inside seal/open is negligible
/// against the AEAD itself.
/// The `expect` moved into `crate::aead`'s ring backend along with
/// `UnboundKey::new`.
fn packet_key(key: &[u8; 32]) -> Box<AeadKey> {
    Box::new(AeadKey::new(key))
}

/// Packet cipher using ChaCha20-Poly1305 with counter-based nonces.
///
/// Nonce format: `[session_prefix: 4 bytes][counter: 8 bytes]`
/// - session_prefix: derived by folding the 64-bit session_id into 4
///   bytes (hi ^ lo). Every session also derives a fresh key in
///   `derive_session_keys`, so nonce uniqueness per key is guaranteed
///   by the counter alone — the prefix is defense-in-depth only, and
///   XORing both halves retains more entropy than a plain 32-bit
///   truncation of session_id.
/// - counter: monotonically increasing, ensures uniqueness within session
///
/// Safety: Counter-based nonces are safe because:
/// - Counter never repeats within a session (AtomicU64)
/// - Each session has a unique per-direction key, so (key, nonce) pairs
///   never collide across sessions regardless of prefix entropy
/// - 2^64 packets before rollover (unreachable in practice)
///
/// When used inside a `PacketPool`, the TX counter should be shared across
/// all ciphers in the pool via `with_shared_tx_counter()` to prevent nonce
/// reuse across concurrent builders.
///
/// Backend: `ring`'s RFC 8439 ChaCha20-Poly1305 (CRYPTO SPIKE
/// 2026-06-11). Swapped from the RustCrypto `chacha20poly1305`
/// stack, whose poly1305 0.8 AVX2 backend re-derives the Poly1305
/// key powers per MESSAGE — ~700 ns of fixed cost on every packet,
/// paid on both seal and open (975 ns total at 64 B vs ~0.47 ns/B
/// marginal; see `net_encryption/raw_aead`). Identical wire bytes
/// — both implement RFC 8439 — pinned by the cross-impl round-trip
/// tests below.
pub struct PacketCipher {
    /// Boxed to keep this 544-byte key off the by-value pool-moved
    /// `PacketBuilder` — see [`packet_key`] for the size rationale.
    cipher: Box<AeadKey>,
    /// Pre-built nonce with the session prefix already filled into
    /// the first 4 bytes (and the counter bytes left as zeros for
    /// each per-packet overwrite). Per crypto-session perf #138,
    /// `nonce_from_counter` starts from this template instead of
    /// zero-initializing then copy-from-slicing the prefix on
    /// every TX / RX packet. Saves the per-packet prefix memcpy
    /// (4 bytes) and the zero-init (12 bytes) — small in absolute
    /// terms but fires twice per packet (once on encrypt, once on
    /// decrypt) at the highest frequency code path in the system.
    /// The session prefix is the first 4 bytes; callers needing
    /// it independently can read `nonce_template[0..4]`.
    nonce_template: [u8; NONCE_SIZE],
    /// TX counter — owned or shared with other ciphers in a pool.
    tx_counter: Arc<AtomicU64>,
    /// Sliding-window replay state for received counters. A single counter
    /// range check cannot prevent replay: an attacker resending a previously
    /// decrypted packet produces identical AEAD output, so we must track
    /// which counters have already been committed inside the window.
    rx_window: Mutex<ReplayWindow>,
}

/// Sliding-window replay protection.
///
/// Bit `i` of `bitmap` is set iff counter `rx_counter - 1 - i` has been
/// committed (decrypted and accepted). `rx_counter` is `1 + highest_seen`,
/// starting at 0 meaning "nothing received yet". The bitmap is only
/// meaningful once `rx_counter > 0`.
#[derive(Debug)]
struct ReplayWindow {
    rx_counter: u64,
    bitmap: [u64; Self::BITMAP_WORDS],
}

impl ReplayWindow {
    const WINDOW_SIZE: u64 = 1024;
    /// Maximum forward jump in counter values that this window
    /// will accept on a single packet. Pre-fix this was 65_536,
    /// far past `WINDOW_SIZE`. Any jump greater than `WINDOW_SIZE`
    /// forced the bitmap to be zeroed (since the bitmap has
    /// `BITMAP_WORDS × 64 = WINDOW_SIZE` bits), erasing the
    /// "seen" markers for the previous `WINDOW_SIZE - 1` counters
    /// — those sequence numbers became replayable until the new
    /// window had been populated. Cap the forward jump at
    /// `WINDOW_SIZE` so any gap that would discard replay state
    /// is rejected; the peer must re-handshake to legitimately
    /// resume past such a gap.
    const MAX_FORWARD: u64 = Self::WINDOW_SIZE;
    const BITMAP_WORDS: usize = 16;

    const fn new() -> Self {
        Self {
            rx_counter: 0,
            bitmap: [0; Self::BITMAP_WORDS],
        }
    }

    /// Read-only check: is `received` in range and not yet committed?
    fn is_valid(&self, received: u64) -> bool {
        // Reject the ceiling counter unconditionally. If `commit`
        // accepted `received == u64::MAX`, `rx_counter` would
        // saturate at `u64::MAX` (since `rx_counter = received
        // .saturating_add(1)` clamps), and the early-return guard
        // at the top of `commit` would then refuse every
        // subsequent packet — permanent receive-path poisoning
        // from a single authenticated packet. The session is
        // already designed to re-handshake long before counter
        // exhaustion (2^64 packets is unreachable in practice),
        // so excising the ceiling value costs nothing and closes
        // the poisoning vector at the gate. `commit` retains its
        // own `rx_counter == u64::MAX` early return as a
        // defense-in-depth backstop in case a future caller skips
        // `is_valid`.
        if received == u64::MAX {
            return false;
        }
        if received >= self.rx_counter {
            received.saturating_sub(self.rx_counter) <= Self::MAX_FORWARD
        } else {
            let age = self.rx_counter - 1 - received;
            if age >= Self::WINDOW_SIZE {
                return false;
            }
            let word = (age / 64) as usize;
            let bit = age % 64;
            self.bitmap[word] & (1u64 << bit) == 0
        }
    }

    /// Commit `received` as seen. Returns `true` iff this call was the one
    /// that marked it — a `false` return means the counter was already
    /// committed by a concurrent caller (replay detected at commit time) or
    /// is outside the retained window.
    ///
    /// Once `rx_counter` has saturated at `u64::MAX` the session has
    /// exhausted its 64-bit nonce space; further commits are refused
    /// so a crafted `received == u64::MAX` cannot be "re-committed"
    /// repeatedly and bypass replay detection. In practice this
    /// boundary is unreachable (2^64 packets per session), but we
    /// prefer an explicit refusal to a subtle ambiguity at the
    /// ceiling.
    fn commit(&mut self, received: u64) -> bool {
        // Refuse the ceiling counter directly. `is_valid` is the
        // primary gate (see `ReplayWindow::is_valid`) but if a
        // future caller skips it and invokes `commit` directly,
        // accepting `received == u64::MAX` here would saturate
        // `rx_counter` (line below: `received.saturating_add(1)`)
        // and the subsequent-commit guard would then reject every
        // legitimate packet. Refusing at the top prevents the
        // saturation in the first place.
        if received == u64::MAX {
            return false;
        }
        if self.rx_counter == u64::MAX {
            return false;
        }
        if received >= self.rx_counter {
            // saturating_add guards `received == u64::MAX` (with
            // rx_counter == 0 the `+ 1` would panic in debug and wrap
            // in release). shift_bitmap_up already clamps at
            // BITMAP_WORDS * 64, so a saturated value is still safe.
            let shift = (received - self.rx_counter).saturating_add(1);
            self.shift_bitmap_up(shift);
            self.rx_counter = received.saturating_add(1);
            self.bitmap[0] |= 1u64;
            true
        } else {
            let age = self.rx_counter - 1 - received;
            if age >= Self::WINDOW_SIZE {
                return false;
            }
            let word = (age / 64) as usize;
            let bit = age % 64;
            let mask = 1u64 << bit;
            let was_set = self.bitmap[word] & mask != 0;
            self.bitmap[word] |= mask;
            !was_set
        }
    }

    fn shift_bitmap_up(&mut self, shift: u64) {
        if shift == 0 {
            return;
        }
        // Pre-fix this branch silently zeroed the bitmap
        // when a legitimate jump exceeded `WINDOW_SIZE` (1024
        // packets). `MAX_FORWARD` is 65_536, so a single packet
        // accepted with `received - rx_counter > 1024` clears the
        // last 64-ish thousand counters' replay tracking — those
        // sequence numbers can be replayed undetected. Operators
        // should know when this happens so they can investigate
        // (a misbehaving peer, a debug-only fast-forward, or
        // adversarial packet injection mid-stream). Log at warn
        // before zeroing.
        if shift >= (Self::BITMAP_WORDS as u64) * 64 {
            tracing::warn!(
                shift,
                window_size = Self::WINDOW_SIZE,
                max_forward = Self::MAX_FORWARD,
                "anti-replay bitmap reset on large forward jump; \
                 prior {} counters lost replay tracking",
                Self::WINDOW_SIZE,
            );
            self.bitmap = [0; Self::BITMAP_WORDS];
            return;
        }
        let word_shift = (shift / 64) as usize;
        let bit_shift = (shift % 64) as u32;
        if bit_shift == 0 {
            for i in (0..Self::BITMAP_WORDS).rev() {
                self.bitmap[i] = if i >= word_shift {
                    self.bitmap[i - word_shift]
                } else {
                    0
                };
            }
        } else {
            for i in (0..Self::BITMAP_WORDS).rev() {
                let hi = if i >= word_shift {
                    self.bitmap[i - word_shift] << bit_shift
                } else {
                    0
                };
                let lo = if i > word_shift {
                    self.bitmap[i - word_shift - 1] >> (64 - bit_shift)
                } else {
                    0
                };
                self.bitmap[i] = hi | lo;
            }
        }
    }
}

/// Derive the 4-byte nonce prefix from a 64-bit session id. Folds the
/// high and low halves together so every bit of session_id contributes,
/// rather than silently truncating the high 32 bits. Both sender and
/// receiver — and the wire header patching in `pool.rs` — must call
/// this so the on-the-wire nonce matches what the cipher used.
#[inline]
pub(crate) fn session_prefix_from_id(session_id: u64) -> [u8; 4] {
    let lo = session_id as u32;
    let hi = (session_id >> 32) as u32;
    (lo ^ hi).to_le_bytes()
}

impl PacketCipher {
    /// Create a new fast cipher from a 32-byte key and session ID
    pub fn new(key: &[u8; 32], session_id: u64) -> Self {
        let mut nonce_template = [0u8; NONCE_SIZE];
        nonce_template[0..4].copy_from_slice(&session_prefix_from_id(session_id));
        Self {
            cipher: packet_key(key),
            nonce_template,
            tx_counter: Arc::new(AtomicU64::new(0)),
            rx_window: Mutex::new(ReplayWindow::new()),
        }
    }

    /// Create a new cipher that shares a TX counter with other ciphers.
    ///
    /// All ciphers sharing the same counter atomically increment it,
    /// preventing nonce reuse when multiple builders encrypt with the
    /// same key (e.g., in a `PacketPool`).
    pub fn with_shared_tx_counter(
        key: &[u8; 32],
        session_id: u64,
        tx_counter: Arc<AtomicU64>,
    ) -> Self {
        let mut nonce_template = [0u8; NONCE_SIZE];
        nonce_template[0..4].copy_from_slice(&session_prefix_from_id(session_id));
        Self {
            cipher: packet_key(key),
            nonce_template,
            tx_counter,
            rx_window: Mutex::new(ReplayWindow::new()),
        }
    }

    /// Generate the next nonce for sending. Per crypto-session
    /// perf #138, starts from the pre-built `nonce_template` (which
    /// already has the session prefix in bytes 0..4) and only
    /// overwrites the counter bytes 4..12 — eliminates the
    /// per-call zero-init + prefix memcpy of the legacy form.
    #[inline]
    #[allow(dead_code)]
    fn next_tx_nonce(&self) -> [u8; NONCE_SIZE] {
        let counter = self.tx_counter.fetch_add(1, Ordering::Relaxed);
        let mut nonce = self.nonce_template;
        nonce[4..12].copy_from_slice(&counter.to_le_bytes());
        nonce
    }

    /// Construct a nonce from received counter value. Mirrors the
    /// TX path (#138): start from the prefix-filled template,
    /// overwrite only the counter bytes.
    #[inline]
    fn nonce_from_counter(&self, counter: u64) -> [u8; NONCE_SIZE] {
        let mut nonce = self.nonce_template;
        nonce[4..12].copy_from_slice(&counter.to_le_bytes());
        nonce
    }

    /// Get the current TX counter value (for including in packet header)
    #[inline]
    pub fn current_tx_counter(&self) -> u64 {
        self.tx_counter.load(Ordering::Relaxed)
    }

    /// Encrypt payload in-place with AAD.
    ///
    /// Returns the nonce counter used (to include in packet header).
    /// Appends authentication tag to the buffer.
    #[inline]
    pub fn encrypt_in_place(&self, aad: &[u8], buffer: &mut BytesMut) -> Result<u64, CryptoError> {
        let counter = self.tx_counter.fetch_add(1, Ordering::Relaxed);
        let nonce = self.nonce_from_counter(counter);

        let tag = self
            .cipher
            .seal_detached(nonce, aad, buffer.as_mut())
            .map_err(|_| CryptoError::Encryption("encryption failed".to_string()))?;

        buffer.extend_from_slice(&tag);
        Ok(counter)
    }

    /// Encrypt a `&mut [u8]` sub-slice in place with AAD, returning
    /// the nonce counter and the detached 16-byte tag for the caller
    /// to splice in (e.g., append at a chosen offset within the same
    /// outer packet buffer).
    ///
    /// Lets `PacketBuilder` (PERF_AUDIT §2.6) frame events directly
    /// into the packet buffer at `HEADER_SIZE` offset, encrypt that
    /// region in place, and append the tag — eliminating the
    /// previous full-payload memcpy from the scratch `payload`
    /// buffer into `packet`.
    #[inline]
    pub fn encrypt_in_place_detached(
        &self,
        aad: &[u8],
        buffer: &mut [u8],
    ) -> Result<(u64, [u8; 16]), CryptoError> {
        let counter = self.tx_counter.fetch_add(1, Ordering::Relaxed);
        let nonce = self.nonce_from_counter(counter);

        let tag = self
            .cipher
            .seal_detached(nonce, aad, buffer)
            .map_err(|_| CryptoError::Encryption("encryption failed".to_string()))?;
        Ok((counter, tag))
    }

    /// Encrypt payload with AAD.
    ///
    /// Returns (ciphertext, nonce_counter).
    #[inline]
    pub fn encrypt(&self, aad: &[u8], plaintext: &[u8]) -> Result<(Vec<u8>, u64), CryptoError> {
        let counter = self.tx_counter.fetch_add(1, Ordering::Relaxed);
        let nonce = self.nonce_from_counter(counter);

        let mut ciphertext = Vec::with_capacity(plaintext.len() + TAG_SIZE);
        ciphertext.extend_from_slice(plaintext);
        self.cipher
            .seal_append_tag(nonce, aad, &mut ciphertext)
            .map_err(|_| CryptoError::Encryption("encryption failed".to_string()))?;

        Ok((ciphertext, counter))
    }

    /// Decrypt payload with AAD using the provided nonce counter.
    #[inline]
    pub fn decrypt(
        &self,
        nonce_counter: u64,
        aad: &[u8],
        ciphertext: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        let nonce = self.nonce_from_counter(nonce_counter);

        let mut buf = ciphertext.to_vec();
        let plaintext_len = self
            .cipher
            .open_in_place(nonce, aad, &mut buf)
            .map_err(|_| CryptoError::Decryption("decryption failed".to_string()))?;
        buf.truncate(plaintext_len);
        Ok(buf)
    }

    /// Decrypt payload in-place with AAD using the provided nonce counter.
    ///
    /// The buffer should contain ciphertext + tag. Returns plaintext length.
    #[inline]
    pub fn decrypt_in_place(
        &self,
        nonce_counter: u64,
        aad: &[u8],
        buffer: &mut [u8],
    ) -> Result<usize, CryptoError> {
        if buffer.len() < TAG_SIZE {
            return Err(CryptoError::Decryption("buffer too small".to_string()));
        }

        let nonce = self.nonce_from_counter(nonce_counter);

        // ring's `open_in_place` consumes the wire layout directly:
        // ciphertext followed by the 16-byte tag in one contiguous
        // buffer, decrypted in place. Returns the plaintext slice
        // (buffer.len() - TAG_SIZE).
        let plaintext_len = self
            .cipher
            .open_in_place(nonce, aad, buffer)
            .map_err(|_| CryptoError::Decryption("decryption failed".to_string()))?;

        Ok(plaintext_len)
    }

    /// Decrypt a [`Bytes`] payload, preferring the zero-copy
    /// in-place path when the inbound buffer's refcount is `1`
    /// (the common case for freshly-received packets — see
    /// crypto-session perf #128). Falls back to the allocating
    /// [`Self::decrypt`] when the buffer is shared.
    ///
    /// Returns plaintext `Bytes`. On the in-place fast path the
    /// returned `Bytes` is the same allocation as the inbound
    /// buffer, truncated to plaintext length. On the fallback path
    /// it's a fresh allocation wrapping the decrypted `Vec`.
    ///
    /// The contract for callers: pre-replay-check (`is_valid_rx_counter`),
    /// then call this method, then commit (`update_rx_counter`)
    /// only on success. The fast path doesn't change that contract
    /// — it's a pure swap for the inner `decrypt` call.
    #[inline]
    pub fn decrypt_to_bytes(
        &self,
        nonce_counter: u64,
        aad: &[u8],
        ciphertext: Bytes,
    ) -> Result<Bytes, CryptoError> {
        match ciphertext.try_into_mut() {
            Ok(mut buf) => {
                // Fast path: refcount == 1, decrypt in place. No
                // allocation — the inbound buffer becomes the
                // plaintext buffer (shrunk by TAG_SIZE).
                let plaintext_len = self.decrypt_in_place(nonce_counter, aad, &mut buf)?;
                buf.truncate(plaintext_len);
                Ok(buf.freeze())
            }
            Err(shared) => {
                // Slow path: another reader still holds a clone
                // (rare in steady-state RX). Allocate.
                self.decrypt(nonce_counter, aad, &shared).map(Bytes::from)
            }
        }
    }

    /// AEAD-verify a ciphertext + tag without producing plaintext
    /// (crypto-session perf #129). Wraps `decrypt_in_place` over
    /// a small stack-allocated scratch buffer so the AEAD verify
    /// runs without a `Vec` allocation per call.
    ///
    /// Used by [`crate::session::NetSession::verify_and_touch_heartbeat`]
    /// where the inbound packet is a 16-byte tag-only payload —
    /// pre-fix this routed through `decrypt(...)` and immediately
    /// dropped the freshly-allocated `Vec<u8>` plaintext. The
    /// scratch path here is correct for ANY payload size but
    /// optimal for the heartbeat shape (tag-only, plaintext_len ==
    /// 0); larger ciphertexts still avoid the heap because the
    /// scratch sits in a `BytesMut` whose backing is a single
    /// reserve.
    ///
    /// Returns `Ok(())` on tag-valid, `Err` on tag-invalid or
    /// length-too-short.
    #[inline]
    pub fn verify(
        &self,
        nonce_counter: u64,
        aad: &[u8],
        ciphertext: &[u8],
    ) -> Result<(), CryptoError> {
        if ciphertext.len() < TAG_SIZE {
            return Err(CryptoError::Decryption("buffer too small".to_string()));
        }
        // Use BytesMut to materialize a mutable copy for the
        // in-place decrypt. The plaintext is discarded — we only
        // need the tag verify side effect. A heartbeat packet
        // (TAG_SIZE bytes total) gives `plaintext_len == 0`, so
        // the only real cost is the AEAD compute itself.
        let mut buf = BytesMut::with_capacity(ciphertext.len());
        buf.extend_from_slice(ciphertext);
        self.decrypt_in_place(nonce_counter, aad, &mut buf)?;
        Ok(())
    }

    /// Commit a received counter as seen. Must be called only after the
    /// packet has been successfully decrypted and authenticated.
    ///
    /// Returns `true` if the counter was genuinely novel; `false` if it was
    /// already committed by a concurrent caller or has slid out of the
    /// replay window. On `false`, the caller MUST drop the packet — this
    /// closes the TOCTOU race between [`Self::is_valid_rx_counter`] and
    /// this call when two threads decrypt the same replayed packet
    /// concurrently.
    #[inline]
    pub fn update_rx_counter(&self, received: u64) -> bool {
        let mut w = self.rx_window.lock();
        w.commit(received)
    }

    /// Validate-and-commit a received counter in a single Mutex
    /// acquisition.
    ///
    /// Per crypto-session perf #132 — the legacy RX hot path called
    /// `is_valid_rx_counter` (lock+unlock) before decrypt and
    /// `update_rx_counter` (lock+unlock) after decrypt: two
    /// parking_lot Mutex ops per packet, on every inbound packet.
    /// `try_admit_rx_counter` does the equivalent post-decrypt
    /// validate-and-commit under a single lock — `commit` already
    /// rejects out-of-window / already-seen / u64::MAX counters
    /// internally, so the pre-decrypt `is_valid_rx_counter` probe is
    /// redundant for safety. Replays are caught at commit time
    /// either way; the only behavioral change is that replayed
    /// packets pay AEAD verify before being rejected (cheaper than
    /// burning a Mutex lock op on every non-replay).
    ///
    /// Returns `true` exactly when [`Self::update_rx_counter`] would
    /// return `true` on the same input — same novelty semantics,
    /// same window contract, half the lock ops at 1 M pps. The
    /// production RX paths (`mesh.rs`, `mod.rs`,
    /// `session.rs::verify_and_touch_heartbeat`) call this instead
    /// of the legacy two-step.
    ///
    /// The legacy `is_valid_rx_counter` + `update_rx_counter` pair
    /// stays exposed for fuzz / regression tests that exercise the
    /// validate-then-commit boundary as two observable steps.
    #[inline]
    pub fn try_admit_rx_counter(&self, received: u64) -> bool {
        let mut w = self.rx_window.lock();
        w.commit(received)
    }

    /// Check if a received counter is in the accept range and has not yet
    /// been committed. Does not change state; callers still race with
    /// [`Self::update_rx_counter`], which returns `false` on replay.
    #[inline]
    pub fn is_valid_rx_counter(&self, received: u64) -> bool {
        let w = self.rx_window.lock();
        w.is_valid(received)
    }
}

impl std::fmt::Debug for PacketCipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let rx_counter = self.rx_window.try_lock().map(|w| w.rx_counter).unwrap_or(0);
        f.debug_struct("PacketCipher")
            .field("algorithm", &"ChaCha20-Poly1305")
            .field("tx_counter", &self.tx_counter.load(Ordering::Relaxed))
            .field("rx_counter", &rx_counter)
            .finish()
    }
}

// PacketCipher intentionally does not implement Clone.
// Cloning would create an independent cipher with the same key and overlapping
// counter-based nonce streams, breaking ChaCha20-Poly1305 security.

/// Key derivation using BLAKE2s as a PRF in an extract-then-expand construction.
///
/// Derives a 32-byte key from input keying material and an info label.
/// Uses keyed BLAKE2s (256-bit): PRK = BLAKE2s(key=ikm, data=b"net-kdf-v1"),
/// then OKM = BLAKE2s(key=PRK, data=info).
#[expect(
    clippy::expect_used,
    reason = "Blake2sMac::new_from_slice rejects only keys longer than 32 bytes; BLAKE2s output (32 bytes) and arbitrary IKM slices are both within the allowed length"
)]
fn derive_key(ikm: &[u8], info: &[u8], out: &mut [u8; 32]) {
    use blake2::{
        digest::{consts::U32, KeyInit, Mac},
        Blake2sMac,
    };

    // Extract: PRK = BLAKE2s-MAC(key=ikm, data="net-kdf-v1")
    let mut extractor = <Blake2sMac<U32> as KeyInit>::new_from_slice(ikm)
        .expect("BLAKE2s accepts variable-length keys");
    Mac::update(&mut extractor, b"net-kdf-v1");
    let prk = extractor.finalize().into_bytes();

    // Expand: OKM = BLAKE2s-MAC(key=PRK, data=info)
    let mut expander =
        <Blake2sMac<U32> as KeyInit>::new_from_slice(&prk).expect("BLAKE2s accepts 32-byte key");
    Mac::update(&mut expander, info);
    let okm = expander.finalize().into_bytes();

    out.copy_from_slice(&okm);
}

fn hex_string(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

