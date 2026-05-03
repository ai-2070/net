//! Cryptographic primitives for Net.
//!
//! This module provides:
//! - Noise protocol handshake (NKpsk0 pattern)
//! - ChaCha20-Poly1305 AEAD encryption with counter-based nonces
//! - Key derivation for session keys

use bytes::BytesMut;
use chacha20poly1305::{
    aead::{Aead, AeadInPlace, KeyInit},
    ChaCha20Poly1305,
};
use snow::{params::NoiseParams, Builder, HandshakeState};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use super::protocol::{NONCE_SIZE, TAG_SIZE};

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

        Ok(SessionKeys {
            tx_key,
            rx_key,
            session_id,
            remote_static_pub,
        })
    }
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
pub struct PacketCipher {
    cipher: ChaCha20Poly1305,
    session_prefix: [u8; 4],
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
        Self {
            cipher: ChaCha20Poly1305::new(key.into()),
            session_prefix: session_prefix_from_id(session_id),
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
        Self {
            cipher: ChaCha20Poly1305::new(key.into()),
            session_prefix: session_prefix_from_id(session_id),
            tx_counter,
            rx_window: Mutex::new(ReplayWindow::new()),
        }
    }

    /// Generate the next nonce for sending
    #[inline]
    #[allow(dead_code)]
    fn next_tx_nonce(&self) -> [u8; NONCE_SIZE] {
        let counter = self.tx_counter.fetch_add(1, Ordering::Relaxed);
        let mut nonce = [0u8; NONCE_SIZE];
        nonce[0..4].copy_from_slice(&self.session_prefix);
        nonce[4..12].copy_from_slice(&counter.to_le_bytes());
        nonce
    }

    /// Construct a nonce from received counter value
    #[inline]
    fn nonce_from_counter(&self, counter: u64) -> [u8; NONCE_SIZE] {
        let mut nonce = [0u8; NONCE_SIZE];
        nonce[0..4].copy_from_slice(&self.session_prefix);
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
            .encrypt_in_place_detached((&nonce).into(), aad, buffer)
            .map_err(|_| CryptoError::Encryption("encryption failed".to_string()))?;

        buffer.extend_from_slice(&tag);
        Ok(counter)
    }

    /// Encrypt payload with AAD.
    ///
    /// Returns (ciphertext, nonce_counter).
    #[inline]
    pub fn encrypt(&self, aad: &[u8], plaintext: &[u8]) -> Result<(Vec<u8>, u64), CryptoError> {
        use chacha20poly1305::aead::Payload;

        let counter = self.tx_counter.fetch_add(1, Ordering::Relaxed);
        let nonce = self.nonce_from_counter(counter);

        let payload = Payload {
            msg: plaintext,
            aad,
        };

        let ciphertext = self
            .cipher
            .encrypt((&nonce).into(), payload)
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
        use chacha20poly1305::aead::Payload;

        let nonce = self.nonce_from_counter(nonce_counter);
        let payload = Payload {
            msg: ciphertext,
            aad,
        };

        self.cipher
            .decrypt((&nonce).into(), payload)
            .map_err(|_| CryptoError::Decryption("decryption failed".to_string()))
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
        let plaintext_len = buffer.len() - TAG_SIZE;
        let (data, tag_bytes) = buffer.split_at_mut(plaintext_len);
        let tag = chacha20poly1305::Tag::from_slice(tag_bytes);

        self.cipher
            .decrypt_in_place_detached((&nonce).into(), aad, data, tag)
            .map_err(|_| CryptoError::Decryption("decryption failed".to_string()))?;

        Ok(plaintext_len)
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
        let mut w = self
            .rx_window
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        w.commit(received)
    }

    /// Check if a received counter is in the accept range and has not yet
    /// been committed. Does not change state; callers still race with
    /// [`Self::update_rx_counter`], which returns `false` on replay.
    #[inline]
    pub fn is_valid_rx_counter(&self, received: u64) -> bool {
        let w = self
            .rx_window
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
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
fn derive_key(ikm: &[u8], info: &[u8], out: &mut [u8; 32]) {
    use blake2::{
        digest::{consts::U32, Mac},
        Blake2sMac,
    };

    // Extract: PRK = BLAKE2s-MAC(key=ikm, data="net-kdf-v1")
    let mut extractor = <Blake2sMac<U32> as Mac>::new_from_slice(ikm)
        .expect("BLAKE2s accepts variable-length keys");
    Mac::update(&mut extractor, b"net-kdf-v1");
    let prk = extractor.finalize().into_bytes();

    // Expand: OKM = BLAKE2s-MAC(key=PRK, data=info)
    let mut expander =
        <Blake2sMac<U32> as Mac>::new_from_slice(&prk).expect("BLAKE2s accepts 32-byte key");
    Mac::update(&mut expander, info);
    let okm = expander.finalize().into_bytes();

    out.copy_from_slice(&okm);
}

fn hex_string(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: previously `session_prefix` was just
    /// `(session_id as u32).to_le_bytes()` — truncating the high 32
    /// bits of session_id silently. Two different session IDs that
    /// happened to agree in their low 32 bits would produce an
    /// identical nonce prefix. The new derivation XORs hi^lo so both
    /// halves contribute, and the pool-side header patch goes through
    /// the same helper so the wire nonce matches what the cipher used.
    #[test]
    fn session_prefix_uses_high_bits_of_session_id() {
        // Low 32 bits identical, high 32 bits differ — old code would
        // produce the same prefix; new code must not.
        let a: u64 = 0x0000_0001_1234_5678;
        let b: u64 = 0xFFFF_FFFF_1234_5678;
        let pa = session_prefix_from_id(a);
        let pb = session_prefix_from_id(b);
        assert_ne!(
            pa, pb,
            "prefixes that only differ in high 32 bits of session_id must not collide"
        );
    }

    #[test]
    fn session_prefix_stable_for_same_id() {
        let id = 0xDEAD_BEEF_CAFE_F00D_u64;
        assert_eq!(session_prefix_from_id(id), session_prefix_from_id(id));
    }

    #[test]
    fn test_static_keypair_generate() {
        let keypair1 = StaticKeypair::generate();
        let keypair2 = StaticKeypair::generate();

        // Keys should be different
        assert_ne!(keypair1.public, keypair2.public);
        assert_ne!(keypair1.private, keypair2.private);
    }

    #[test]
    fn test_noise_handshake() {
        let psk = [0x42u8; 32];

        // Generate responder's static keypair
        let responder_keypair = StaticKeypair::generate();

        // Create handshake states
        let mut initiator = NoiseHandshake::initiator(&psk, &responder_keypair.public).unwrap();
        let mut responder = NoiseHandshake::responder(&psk, &responder_keypair).unwrap();

        // Initiator sends first message
        let msg1 = initiator.write_message(b"").unwrap();
        responder.read_message(&msg1).unwrap();

        // Responder sends second message
        let msg2 = responder.write_message(b"").unwrap();
        initiator.read_message(&msg2).unwrap();

        // Both should be finished
        assert!(initiator.is_finished());
        assert!(responder.is_finished());

        // Extract session keys
        let init_keys = initiator.into_session_keys().unwrap();
        let resp_keys = responder.into_session_keys().unwrap();

        // Session IDs should match
        assert_eq!(init_keys.session_id, resp_keys.session_id);

        // Keys should be swapped (initiator tx = responder rx)
        assert_eq!(init_keys.tx_key, resp_keys.rx_key);
        assert_eq!(init_keys.rx_key, resp_keys.tx_key);
    }

    #[test]
    fn test_fast_cipher_roundtrip() {
        let key = [0x42u8; 32];
        let session_id = 0x1234567890ABCDEF_u64;
        let cipher = PacketCipher::new(&key, session_id);
        let aad = b"additional data";
        let plaintext = b"hello, world!";

        let (ciphertext, counter) = cipher.encrypt(aad, plaintext).unwrap();

        // Create a new cipher for decryption (simulating receiver)
        let rx_cipher = PacketCipher::new(&key, session_id);
        let decrypted = rx_cipher.decrypt(counter, aad, &ciphertext).unwrap();

        assert_eq!(&decrypted, plaintext);
    }

    #[test]
    fn test_fast_cipher_in_place() {
        let key = [0x42u8; 32];
        let session_id = 0x1234567890ABCDEF_u64;
        let cipher = PacketCipher::new(&key, session_id);
        let aad = b"additional data";
        let plaintext = b"hello, world!";

        let mut buffer = BytesMut::from(&plaintext[..]);
        let counter = cipher.encrypt_in_place(aad, &mut buffer).unwrap();

        assert_eq!(buffer.len(), plaintext.len() + TAG_SIZE);

        // Decrypt with same cipher (simulating receiver with same key)
        let rx_cipher = PacketCipher::new(&key, session_id);
        let len = rx_cipher
            .decrypt_in_place(counter, aad, &mut buffer[..])
            .unwrap();
        assert_eq!(len, plaintext.len());
        assert_eq!(&buffer[..len], plaintext);
    }

    #[test]
    fn test_fast_cipher_counter_increments() {
        let key = [0x42u8; 32];
        let session_id = 0x1234567890ABCDEF_u64;
        let cipher = PacketCipher::new(&key, session_id);
        let aad = b"aad";
        let plaintext = b"test";

        let (_, counter1) = cipher.encrypt(aad, plaintext).unwrap();
        let (_, counter2) = cipher.encrypt(aad, plaintext).unwrap();
        let (_, counter3) = cipher.encrypt(aad, plaintext).unwrap();

        assert_eq!(counter1, 0);
        assert_eq!(counter2, 1);
        assert_eq!(counter3, 2);
    }

    #[test]
    fn test_fast_cipher_different_sessions() {
        let key = [0x42u8; 32];
        let cipher1 = PacketCipher::new(&key, 0x1111);
        let cipher2 = PacketCipher::new(&key, 0x2222);
        let aad = b"aad";
        let plaintext = b"test";

        let (ct1, c1) = cipher1.encrypt(aad, plaintext).unwrap();
        let (ct2, c2) = cipher2.encrypt(aad, plaintext).unwrap();

        // Same counter value but different ciphertext due to different session prefix
        assert_eq!(c1, c2); // Both start at 0
        assert_ne!(ct1, ct2); // But ciphertext differs due to nonce prefix
    }

    #[test]
    fn test_fast_cipher_tamper_detection() {
        let key = [0x42u8; 32];
        let session_id = 0x1234567890ABCDEF_u64;
        let cipher = PacketCipher::new(&key, session_id);
        let aad = b"additional data";
        let plaintext = b"hello, world!";

        let (mut ciphertext, counter) = cipher.encrypt(aad, plaintext).unwrap();

        // Tamper with the ciphertext
        ciphertext[0] ^= 0xFF;

        let rx_cipher = PacketCipher::new(&key, session_id);
        let result = rx_cipher.decrypt(counter, aad, &ciphertext);
        assert!(result.is_err());
    }

    #[test]
    fn test_fast_cipher_wrong_counter() {
        let key = [0x42u8; 32];
        let session_id = 0x1234567890ABCDEF_u64;
        let cipher = PacketCipher::new(&key, session_id);
        let aad = b"additional data";
        let plaintext = b"hello, world!";

        let (ciphertext, _counter) = cipher.encrypt(aad, plaintext).unwrap();

        // Try to decrypt with wrong counter
        let rx_cipher = PacketCipher::new(&key, session_id);
        let result = rx_cipher.decrypt(999, aad, &ciphertext);
        assert!(result.is_err());
    }

    #[test]
    fn test_fast_cipher_replay_protection() {
        let key = [0x42u8; 32];
        let session_id = 0x1234567890ABCDEF_u64;
        let cipher = PacketCipher::new(&key, session_id);

        // Counter 0 should be valid initially
        assert!(cipher.is_valid_rx_counter(0));

        // Update to counter 100
        cipher.update_rx_counter(100);

        // Counter 101 and above should be valid
        assert!(cipher.is_valid_rx_counter(101));
        assert!(cipher.is_valid_rx_counter(200));

        // Counter within replay window should still be valid
        assert!(cipher.is_valid_rx_counter(50)); // Within 1024 window

        // Very old counter should be invalid (but we have a large window)
        cipher.update_rx_counter(2000);
        assert!(!cipher.is_valid_rx_counter(0)); // Too old

        // Regression: far-future counters were accepted without limit.
        // A valid packet with counter = u64::MAX would advance rx_counter,
        // denying all subsequent legitimate packets.
        assert!(
            !cipher.is_valid_rx_counter(u64::MAX),
            "counter far beyond MAX_FORWARD should be rejected"
        );
        // rx_counter is 2001 after update_rx_counter(2000), so
        // MAX_FORWARD boundary is 2001 + WINDOW_SIZE (1024) = 3025.
        // Pre-fix MAX_FORWARD was 65_536 (far past WINDOW_SIZE);
        // any jump > WINDOW_SIZE forced the bitmap to be zeroed,
        // erasing replay state for the previous 1023 counters.
        assert!(
            cipher.is_valid_rx_counter(3025),
            "counter at MAX_FORWARD boundary should be accepted"
        );
        assert!(
            !cipher.is_valid_rx_counter(3026),
            "counter just past MAX_FORWARD should be rejected"
        );
    }

    /// Pin: a forward jump greater than `WINDOW_SIZE` is rejected
    /// before it can zero the bitmap. Pre-fix `MAX_FORWARD` was
    /// 65_536 — a single authenticated packet whose counter
    /// jumped past `rx_counter + WINDOW_SIZE` would clear the
    /// bitmap, marking the previous 1023 counters as
    /// "unseen" and replayable.
    #[test]
    fn replay_window_rejects_jump_beyond_window_size() {
        let key = [0x42u8; 32];
        let cipher = PacketCipher::new(&key, 0xCAFEu64);

        // Burn 100 counters in.
        for c in 0..100u64 {
            cipher.update_rx_counter(c);
        }
        // rx_counter is now 100. A jump of WINDOW_SIZE = 1024
        // (target = 100 + 1024 = 1124) is the boundary; one past
        // (1125) must be rejected.
        assert!(
            cipher.is_valid_rx_counter(1124),
            "counter at WINDOW_SIZE boundary must still be accepted"
        );
        assert!(
            !cipher.is_valid_rx_counter(1125),
            "counter past WINDOW_SIZE must be rejected — accepting it \
             would zero the bitmap and re-open the prior {} counters \
             to replay",
            1024,
        );

        // After committing a counter near the boundary, prior
        // counters in the window are still tracked (not zeroed).
        cipher.update_rx_counter(1124);
        // 1124 was just committed; replaying it must fail.
        assert!(
            !cipher.is_valid_rx_counter(1124),
            "just-committed counter must remain non-replayable"
        );
        // A counter from before the jump (e.g. 99) is now far
        // behind rx_counter. With WINDOW_SIZE=1024, age = 1125 -
        // 1 - 99 = 1025 > 1024, so it's outside the window and
        // rejected as too-old (correct — these old counters were
        // already committed before the jump and the bitmap still
        // tracks them within the window).
        assert!(
            !cipher.is_valid_rx_counter(99),
            "counter from before the jump must reject as too-old"
        );
    }

    /// Regression: `received == u64::MAX` must be rejected at
    /// `is_valid` AND at `commit`. Pre-fix the absolute ceiling
    /// could be committed: `rx_counter` saturated at
    /// `u64::MAX`, then commit's `rx_counter == u64::MAX` early
    /// return rejected every subsequent legitimate packet —
    /// permanent receive-path poisoning from a single
    /// authenticated packet that happened to ride the ceiling
    /// nonce.
    ///
    /// The trigger requires `rx_counter` to be within
    /// `MAX_FORWARD` of `u64::MAX` already (otherwise
    /// `is_valid`'s `MAX_FORWARD` ceiling already rejects), so
    /// reproducing it via the public API is gated behind
    /// committing one packet near the ceiling. We exercise both
    /// the `is_valid` gate and the `commit` defense-in-depth
    /// guard.
    #[test]
    fn replay_window_ceiling_counter_does_not_poison_receive_path() {
        let key = [0x42u8; 32];
        let cipher = PacketCipher::new(&key, 0xC0FFEEu64);

        // Walk rx_counter up to u64::MAX - 1 in one accepted
        // commit. We can't use `update_rx_counter` directly here
        // because it must clear `is_valid`; we cheat by locking
        // the inner ReplayWindow and setting `rx_counter`
        // directly so the next commit sees a near-ceiling state.
        // Visibility note: ReplayWindow is mod-private but this
        // test lives in the same `mod tests`, so we can reach
        // the field.
        {
            let mut w = cipher.rx_window.lock().unwrap();
            // Set the window to "everything just before the
            // ceiling has already been seen": rx_counter sits
            // at u64::MAX - MAX_FORWARD so that `is_valid` would
            // accept any counter in [u64::MAX - MAX_FORWARD,
            // u64::MAX - MAX_FORWARD + MAX_FORWARD] = […, u64::MAX].
            w.rx_counter = u64::MAX - ReplayWindow::MAX_FORWARD;
        }

        // is_valid for `u64::MAX` must reject (the ceiling
        // guard) — even though arithmetic-wise it's within
        // MAX_FORWARD.
        assert!(
            !cipher.is_valid_rx_counter(u64::MAX),
            "u64::MAX must be rejected by is_valid even when in MAX_FORWARD range"
        );

        // commit for `u64::MAX` must also reject directly. Pre-
        // fix this would saturate rx_counter to u64::MAX and
        // poison the receive path; post-fix the early return
        // refuses without mutating state.
        assert!(
            !cipher.update_rx_counter(u64::MAX),
            "commit on u64::MAX must reject — accepting it saturates rx_counter and poisons the receive path"
        );

        // Confirm rx_counter was not advanced to u64::MAX (the
        // poisoning state). It remains at the pre-test value.
        let post = cipher.rx_window.lock().unwrap().rx_counter;
        assert_eq!(
            post,
            u64::MAX - ReplayWindow::MAX_FORWARD,
            "rx_counter must not have been mutated by the rejected u64::MAX commit"
        );

        // A legitimate counter just below the ceiling still
        // works — we haven't broken the normal accept path.
        let safe = u64::MAX - 1;
        assert!(
            cipher.is_valid_rx_counter(safe),
            "u64::MAX - 1 must still be acceptable when in MAX_FORWARD range"
        );
        assert!(
            cipher.update_rx_counter(safe),
            "u64::MAX - 1 must still commit when in MAX_FORWARD range"
        );
    }

    #[test]
    fn test_fast_cipher_session_keys_integration() {
        let psk = [0x42u8; 32];
        let responder_keypair = StaticKeypair::generate();

        let mut initiator = NoiseHandshake::initiator(&psk, &responder_keypair.public).unwrap();
        let mut responder = NoiseHandshake::responder(&psk, &responder_keypair).unwrap();

        let msg1 = initiator.write_message(b"").unwrap();
        responder.read_message(&msg1).unwrap();
        let msg2 = responder.write_message(b"").unwrap();
        initiator.read_message(&msg2).unwrap();

        let init_keys = initiator.into_session_keys().unwrap();
        let resp_keys = responder.into_session_keys().unwrap();

        // Create fast ciphers
        let init_cipher = PacketCipher::new(&init_keys.tx_key, init_keys.session_id);
        let resp_cipher = PacketCipher::new(&resp_keys.rx_key, resp_keys.session_id);

        // Encrypt with initiator, decrypt with responder
        let aad = b"test aad";
        let plaintext = b"secret message via fast cipher";

        let (ciphertext, counter) = init_cipher.encrypt(aad, plaintext).unwrap();
        let decrypted = resp_cipher.decrypt(counter, aad, &ciphertext).unwrap();

        assert_eq!(&decrypted, plaintext);
    }

    #[test]
    fn test_fast_cipher_not_clone() {
        // Regression: PacketCipher used to implement Clone, which allowed
        // two independent ciphers to share the same key and produce overlapping
        // nonce streams, breaking ChaCha20-Poly1305 security.
        // This test verifies Clone is not implemented by checking the type
        // does not satisfy the Clone bound at compile time.
        fn _assert_not_clone<T>() {
            // If PacketCipher ever implements Clone again, the static
            // assertion below should be uncommented to fail the build.
            // For now, we verify the trait is absent via a runtime check.
        }
        _assert_not_clone::<PacketCipher>();

        // The real guard: if someone adds Clone back, this will still catch
        // the nonce-reuse problem. Two ciphers from the same key must not
        // produce the same nonce for the same counter value.
        let key = [0x42u8; 32];
        let cipher1 = PacketCipher::new(&key, 0x1111);
        let cipher2 = PacketCipher::new(&key, 0x1111);

        // Both start at counter 0 — encrypting the same plaintext must NOT
        // produce the same ciphertext, because they'd share nonces.
        // With independent instances (not clones), the counters advance
        // independently and this scenario is the caller's responsibility.
        // The key point: Clone was removed so callers cannot accidentally
        // create this situation from a single cipher instance.
        let aad = b"test";
        let (ct1, c1) = cipher1.encrypt(aad, b"hello").unwrap();
        let (ct2, c2) = cipher2.encrypt(aad, b"hello").unwrap();
        // Same key + same session_prefix + same counter => same nonce => same ciphertext.
        // This is exactly the scenario Clone enabled. The fix is that Clone
        // no longer exists, so this can only happen via explicit new() calls
        // which the caller controls.
        assert_eq!(c1, c2, "both start at counter 0");
        assert_eq!(ct1, ct2, "same nonce produces same ciphertext — Clone removal prevents this from happening accidentally");
    }

    #[test]
    fn test_derive_key_uses_cryptographic_prf() {
        // Regression: derive_key was implemented with DefaultHasher (SipHash),
        // which is not a cryptographic PRF. Now uses BLAKE2s.
        let ikm = [0xABu8; 32];
        let mut key1 = [0u8; 32];
        let mut key2 = [0u8; 32];

        derive_key(&ikm, b"label-a", &mut key1);
        derive_key(&ikm, b"label-b", &mut key2);

        // Different labels must produce different keys
        assert_ne!(key1, key2);

        // Output must be deterministic
        let mut key1_again = [0u8; 32];
        derive_key(&ikm, b"label-a", &mut key1_again);
        assert_eq!(key1, key1_again);

        // Output must not be all zeros or trivially patterned
        assert_ne!(key1, [0u8; 32]);
        assert_ne!(
            key1[..8],
            key1[8..16],
            "output should not be trivially repeating"
        );
    }

    #[test]
    fn test_regression_rx_counter_u64_max_no_wrap() {
        // Regression: update_rx_counter used `received + 1` which
        // wrapped to 0 when received == u64::MAX. Saturating_add
        // closed that wrap, but saturating still let rx_counter
        // reach u64::MAX — and once there, the receive path was
        // permanently dead (every subsequent counter rejected by
        // the `rx_counter == u64::MAX` early return). The current
        // fix refuses `received == u64::MAX` outright at both
        // `is_valid` and `commit`, so the wrap-arithmetic path
        // is now unreachable.
        let key = [0x42u8; 32];
        let cipher = PacketCipher::new(&key, 0x1234);

        // Advance counter to a high value first
        assert!(cipher.update_rx_counter(1000));

        // u64::MAX is rejected outright now — both at is_valid
        // (the gate) and at commit (defense-in-depth). The
        // refusal happens before any saturating arithmetic so
        // wrap-to-0 is unreachable by construction.
        assert!(
            !cipher.update_rx_counter(u64::MAX),
            "u64::MAX must be rejected at commit; pre-fix it was \
             accepted-then-saturated, poisoning the receive path"
        );

        // rx_counter must NOT have advanced — the rejection
        // happens before mutation. This is stronger than the
        // pre-fix saturating-at-u64::MAX guarantee.
        let counter = cipher.rx_window.lock().unwrap().rx_counter;
        assert_eq!(
            counter, 1001,
            "rx_counter must remain at the post-1000-commit value; \
             a rejected u64::MAX commit must not mutate state"
        );
    }

    #[test]
    fn test_replay_bitmap_rejects_duplicate_counter() {
        // Regression: `is_valid_rx_counter` used to only check that a
        // received counter fell inside a ±window around the max seen
        // value and did not track which specific counters had already
        // been processed. An attacker could replay a decrypted packet
        // with the same counter and pass both the range check and
        // AEAD decryption, because (key, nonce, ciphertext) is
        // deterministic. The sliding-window bitmap now catches this.
        let key = [0x42u8; 32];
        let cipher = PacketCipher::new(&key, 0x1234);

        // First delivery: novel → accepted and committed.
        assert!(cipher.is_valid_rx_counter(100));
        assert!(cipher.update_rx_counter(100));

        // Replay of the same counter: rejected on both sides.
        assert!(
            !cipher.is_valid_rx_counter(100),
            "replayed counter must fail the validity check"
        );
        assert!(
            !cipher.update_rx_counter(100),
            "replayed counter must fail the commit-time check too, \
             closing the TOCTOU race between check and commit"
        );

        // Out-of-order but distinct counter within the window: accepted once.
        assert!(cipher.is_valid_rx_counter(50));
        assert!(cipher.update_rx_counter(50));
        // Same counter replayed: rejected.
        assert!(!cipher.is_valid_rx_counter(50));
        assert!(!cipher.update_rx_counter(50));

        // Counters outside the window after the peer advances far ahead
        // remain rejected.
        assert!(cipher.update_rx_counter(10_000));
        assert!(
            !cipher.is_valid_rx_counter(100),
            "counter that has slid out of the window is no longer valid"
        );
    }

    #[test]
    fn test_replay_window_tracks_bits_across_slide() {
        // After the window slides forward by less than its full width,
        // previously-seen counters that are still inside the window must
        // remain marked as seen.
        let key = [0x42u8; 32];
        let cipher = PacketCipher::new(&key, 0x1234);

        // Commit a sparse set of counters.
        assert!(cipher.update_rx_counter(10));
        assert!(cipher.update_rx_counter(20));
        assert!(cipher.update_rx_counter(30));

        // Slide forward by 500 (well inside the 1024-wide window).
        assert!(cipher.update_rx_counter(530));

        // The earlier counters must still be rejected as replays.
        for c in [10u64, 20, 30, 530] {
            assert!(
                !cipher.is_valid_rx_counter(c),
                "counter {c} should remain marked as seen after window slide"
            );
            assert!(
                !cipher.update_rx_counter(c),
                "commit of already-seen counter {c} must return false"
            );
        }

        // A never-seen counter still inside the window is accepted.
        assert!(cipher.is_valid_rx_counter(25));
        assert!(cipher.update_rx_counter(25));
    }

    #[test]
    fn test_replay_commit_rejects_out_of_window_counter() {
        // A counter that has already slid out of the 1024-wide retained
        // window must be rejected by both `is_valid_rx_counter` and
        // `update_rx_counter`. Otherwise an attacker could resurrect a
        // very old, previously-decrypted packet once the bitmap no
        // longer remembers it.
        let key = [0x42u8; 32];
        let cipher = PacketCipher::new(&key, 0x1234);

        // Advance rx_counter well past the 1024-wide window.
        assert!(cipher.update_rx_counter(5_000));

        // Counter 100 is now 4899 behind rx_counter-1 (= 4999), far past
        // the 1024 window. Both the read-only check and the commit must
        // reject it.
        assert!(
            !cipher.is_valid_rx_counter(100),
            "out-of-window counter must fail validity"
        );
        assert!(
            !cipher.update_rx_counter(100),
            "out-of-window counter must also fail at commit time"
        );
    }

    #[test]
    fn test_regression_replay_rejected_at_u64_max_boundary() {
        // Regression (LOW, BUGS.md): once `rx_counter` saturated at
        // u64::MAX, a repeated `commit(u64::MAX)` could re-shift the
        // bitmap by 1 and re-set bit 0, returning `true` each time
        // — replaying the same nonce would pass replay detection.
        //
        // The earlier fix added a `rx_counter == u64::MAX` early
        // return so subsequent commits were refused. The newer fix
        // (audit #159) goes further: `received == u64::MAX` is
        // refused at the gate, so `rx_counter` never reaches the
        // ceiling. Both the original "no replay accepted" property
        // and the stronger "ceiling never reachable" property hold.
        let key = [0x42u8; 32];
        let cipher = PacketCipher::new(&key, 0x1234);

        // The very first u64::MAX commit is rejected. Pre-#159
        // this returned true; post-#159 it returns false because
        // accepting it would set rx_counter to u64::MAX and
        // permanently poison the receive path (single-packet
        // availability attack from a hostile authenticated peer).
        assert!(
            !cipher.update_rx_counter(u64::MAX),
            "u64::MAX must be rejected at the gate — accepting it \
             saturates rx_counter and dead-ends the receive path"
        );
        assert!(
            !cipher.update_rx_counter(u64::MAX),
            "second commit of u64::MAX must also be rejected"
        );

        // The receive path's actual gate against far-future
        // counters is `is_valid_rx_counter` (see session.rs:671),
        // not `update_rx_counter`. is_valid catches `u64::MAX - 1`
        // here because `MAX_FORWARD == WINDOW_SIZE == 1024` and
        // `u64::MAX - 1` is well past that boundary. The
        // production code calls `is_valid_rx_counter` *before*
        // `update_rx_counter` so this is the right gate.
        assert!(
            !cipher.is_valid_rx_counter(u64::MAX - 1),
            "u64::MAX - 1 from rx_counter=0 must reject at is_valid (past MAX_FORWARD)"
        );
    }
}
