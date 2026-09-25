//! Blind UDP relay — the fallback path for a node that cannot be reached
//! directly (no router mapping, carrier-grade NAT).
//!
//! **Blind** means the relay never holds a trust-domain PSK, an issuer key or
//! any mesh credential. It forwards opaque datagrams (the mesh's NKpsk0
//! ciphertext) between two endpoints by a channel number and cannot read,
//! forge or join anything it carries.
//!
//! # Protocol (one UDP socket on the relay)
//!
//! A **device** keeps a registration from its own mesh socket, so the same NAT
//! mapping carries its relayed traffic:
//!
//! 1. `HELLO { entity }` → `CHALLENGE { nonce, observed endpoint }`. The nonce
//!    is stateless: a keyed hash of the source endpoint, entity and time bucket.
//! 2. `REGISTER { entity, nonce, signature }` → `REGISTERED { registration id,
//!    ttl }`. The ed25519 signature covers the nonce **and the endpoint the
//!    relay observed**, so a captured registration cannot be replayed from
//!    another address to divert the device's traffic. The registration id is
//!    derived from the entity id, so only that key can claim it.
//! 3. Re-registering before the TTL keeps both the registration and the NAT
//!    mapping alive; a changed endpoint (NAT rebinding) is simply re-recorded.
//!
//! A **joiner** that knows the registration id (it is carried in a signed join
//! token) sends `BIND { registration id }` → `BOUND { channel }`. After that,
//! `DATA { channel, payload }` from the joiner is forwarded to the device's
//! current endpoint and `DATA` from that device endpoint on the channel is
//! forwarded to the joiner. Anything else is dropped.
//!
//! **No amplification:** every reply is no larger than the request that caused
//! it (`HELLO` and `BIND` are padded), and forwarding is 1:1. Registrations,
//! channels per registration, total channels and per-registration bind rate are
//! bounded; idle state expires. Relay state is never authority.
//!
//! # Byte-stream splice (TCP, same port number)
//!
//! Enrollment is a PSK-free Noise NK session over a byte stream, so the relay
//! also splices TCP: a joiner connects and sends `[JOIN][registration id]`; the
//! relay sends a UDP `OFFER { splice id }` to the device's registered endpoint
//! (resent until answered or [`RelayConfig::splice_accept_wait`] passes); the
//! device dials back with `[ACCEPT][splice id]`, and the relay answers both
//! streams with one status byte and copies bytes blindly between them. The
//! splice id is 128 random bits, sent only to the registered endpoint; the
//! stream's end-to-end Noise handshake still authenticates the device, so a
//! party that claimed an offer could not impersonate it. Offers are only ever
//! sent to a registered device, after a completed TCP handshake, at a bounded
//! per-registration rate. Splices are bounded in number, bytes per direction and
//! lifetime.
//!
//! # TCP tunnel (same port number, last resort)
//!
//! A node whose UDP to the relay gets no answer (a network that blocks UDP)
//! opens `[TUNNEL][16 zero bytes]` on the same TCP listener. After the `OK`
//! status byte the stream carries exactly the datagrams above, each as a
//! big-endian `u16` length and the datagram. The relay gives each tunnel a
//! synthetic endpoint in the RFC 6666 discard prefix (`100::/64`), which no
//! UDP datagram can legitimately come from, and runs the unchanged state
//! machine against it: registration still signs that observed endpoint, and
//! anything the relay addresses to it goes down the tunnel. Nothing about it
//! is claimed to cross proxies or TLS-inspecting middleboxes: it is plain TCP,
//! carrying the same end-to-end ciphertext.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::Semaphore;

use crate::adapter::net::identity::{EntityId, EntityKeypair};

/// Message type bytes (first byte of every relay datagram).
pub mod kind {
    /// Device → relay: start a registration.
    pub const HELLO: u8 = 0x01;
    /// Relay → device: stateless challenge.
    pub const CHALLENGE: u8 = 0x02;
    /// Device → relay: signed registration.
    pub const REGISTER: u8 = 0x03;
    /// Relay → device: registration accepted.
    pub const REGISTERED: u8 = 0x04;
    /// Joiner → relay: open a channel to a registration.
    pub const BIND: u8 = 0x05;
    /// Relay → joiner: channel opened.
    pub const BOUND: u8 = 0x06;
    /// Relay → sender: request refused (see [`super::Refusal`]).
    pub const REFUSED: u8 = 0x07;
    /// Relay → device: a joiner is waiting on a byte-stream splice.
    pub const OFFER: u8 = 0x08;
    /// Either side: channel data.
    pub const DATA: u8 = 0x10;
}

/// The byte-stream splice preamble (TCP, same port number as the relay's UDP
/// socket): one kind byte and 16 bytes of id, answered with one status byte.
pub mod splice {
    /// Joiner → relay: `[JOIN][registration id]`.
    pub const JOIN: u8 = 0x20;
    /// Device → relay: `[ACCEPT][splice id]` (from an `OFFER`).
    pub const ACCEPT: u8 = 0x21;
    /// Node → relay: `[TUNNEL][16 zero bytes]` — carry this node's relay
    /// datagrams over the stream, each `u16`-length-prefixed.
    pub const TUNNEL: u8 = 0x22;
    /// Preamble length.
    pub const PREAMBLE_LEN: usize = 1 + 16;
    /// Status byte: spliced; everything after it is the peer's bytes. Any
    /// other status is a [`super::Refusal`] code and the stream closes.
    pub const OK: u8 = 0;
}

const REGISTER_DOMAIN: &[u8] = b"net-mesh blind relay register v1";
const REGISTRATION_ID_CONTEXT: &str = "net-mesh blind relay registration v1";
const CHALLENGE_CONTEXT: &str = "net-mesh blind relay challenge v1";
/// Challenge validity bucket; a nonce is accepted for its bucket and the next.
const CHALLENGE_BUCKET_SECS: u64 = 30;

/// `HELLO` length: kind + entity + padding (≥ `CHALLENGE` length).
pub const HELLO_LEN: usize = 1 + 32 + 32;
/// `CHALLENGE` length: kind + nonce + observed endpoint.
pub const CHALLENGE_LEN: usize = 1 + 16 + ENDPOINT_LEN;
/// `REGISTER` length: kind + entity + nonce + signature.
pub const REGISTER_LEN: usize = 1 + 32 + 16 + 64;
/// `REGISTERED` length: kind + registration id + ttl seconds.
pub const REGISTERED_LEN: usize = 1 + 16 + 2;
/// `BIND` length: kind + registration id + padding (≥ `BOUND` length).
pub const BIND_LEN: usize = 1 + 16 + 16;
/// `BOUND` length: kind + registration id + channel.
pub const BOUND_LEN: usize = 1 + 16 + 4;
/// `REFUSED` length: kind + code.
pub const REFUSED_LEN: usize = 2;
/// `OFFER` length: kind + splice id.
pub const OFFER_LEN: usize = 1 + 16;
/// `DATA` header length: kind + channel.
pub const DATA_HEADER_LEN: usize = 1 + 4;
const ENDPOINT_LEN: usize = 16 + 2;

/// Registration identifier: derived from the registering entity id.
pub type RegistrationId = [u8; 16];

/// Splice identifier: random, the capability to claim one pending splice.
pub type SpliceId = [u8; 16];

/// Derive the registration id an entity registers under.
pub fn registration_id(entity: &EntityId) -> RegistrationId {
    let full = blake3::derive_key(REGISTRATION_ID_CONTEXT, entity.as_bytes());
    let mut id = [0u8; 16];
    id.copy_from_slice(&full[..16]);
    id
}

/// The synthetic endpoint the relay gives its `n`th TCP tunnel: inside the
/// RFC 6666 discard-only prefix `100::/64`, so it can never collide with a
/// UDP source.
pub fn tunnel_endpoint(n: u64) -> SocketAddr {
    let seg = |shift: u32| ((n >> shift) & 0xffff) as u16;
    SocketAddr::new(
        IpAddr::V6(Ipv6Addr::new(
            0x0100,
            0,
            0,
            0,
            seg(48),
            seg(32),
            seg(16),
            seg(0),
        )),
        0,
    )
}

/// Whether `addr` is a tunnel endpoint (see [`tunnel_endpoint`]).
pub fn is_tunnel_endpoint(addr: &SocketAddr) -> bool {
    match addr.ip() {
        IpAddr::V6(v6) => v6.segments()[..4] == [0x0100, 0, 0, 0],
        IpAddr::V4(_) => false,
    }
}

/// Largest datagram a tunnel frame carries (its `u16` length).
const TUNNEL_FRAME_MAX: usize = u16::MAX as usize;
/// Frames queued toward one tunnel before further ones are dropped (datagram
/// semantics: a slow stream loses datagrams, it never stalls the relay).
const TUNNEL_QUEUE: usize = 1_024;

/// Read one `u16`-length-prefixed frame; `None` at end of stream, on error,
/// or on a zero-length frame.
pub(crate) async fn read_tunnel_frame<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut R,
) -> Option<Vec<u8>> {
    let mut len = [0u8; 2];
    reader.read_exact(&mut len).await.ok()?;
    let len = u16::from_be_bytes(len) as usize;
    if len == 0 {
        return None;
    }
    let mut frame = vec![0u8; len];
    reader.read_exact(&mut frame).await.ok()?;
    Some(frame)
}

/// Write one frame (`u16` length + bytes); frames over the limit are dropped.
pub(crate) async fn write_tunnel_frame<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    frame: &[u8],
) -> std::io::Result<()> {
    if frame.is_empty() || frame.len() > TUNNEL_FRAME_MAX {
        return Ok(());
    }
    let mut out = Vec::with_capacity(2 + frame.len());
    out.extend_from_slice(&(frame.len() as u16).to_be_bytes());
    out.extend_from_slice(frame);
    writer.write_all(&out).await
}

/// Why the relay refused a request. Deliberately coarse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Challenge expired, signature invalid, or entity mismatch.
    BadProof = 1,
    /// No such registration (or it expired).
    UnknownRegistration = 2,
    /// A capacity or rate limit was hit.
    Capacity = 3,
    /// The registered device did not answer a splice offer in time.
    Unreachable = 4,
}

impl Refusal {
    fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            1 => Self::BadProof,
            2 => Self::UnknownRegistration,
            3 => Self::Capacity,
            4 => Self::Unreachable,
            _ => return None,
        })
    }
}

/// A decoded relay datagram.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    /// See [`kind::HELLO`].
    Hello {
        /// The registering device's entity id.
        entity: EntityId,
    },
    /// See [`kind::CHALLENGE`].
    Challenge {
        /// Stateless nonce to sign.
        nonce: [u8; 16],
        /// The endpoint the relay saw the `HELLO` from.
        observed: SocketAddr,
    },
    /// See [`kind::REGISTER`].
    Register {
        /// The registering device's entity id.
        entity: EntityId,
        /// The nonce from `CHALLENGE`.
        nonce: [u8; 16],
        /// ed25519 signature over [`register_message`].
        signature: [u8; 64],
    },
    /// See [`kind::REGISTERED`].
    Registered {
        /// The registration id joiners bind to.
        id: RegistrationId,
        /// Seconds until the registration must be refreshed.
        ttl_secs: u16,
    },
    /// See [`kind::BIND`].
    Bind {
        /// The registration to reach.
        id: RegistrationId,
    },
    /// See [`kind::BOUND`].
    Bound {
        /// The registration reached.
        id: RegistrationId,
        /// The channel allocated for this joiner.
        channel: u32,
    },
    /// See [`kind::REFUSED`].
    Refused(Refusal),
    /// See [`kind::OFFER`].
    Offer {
        /// The pending splice to claim.
        splice: SpliceId,
    },
    /// See [`kind::DATA`].
    Data {
        /// Channel the payload travels on.
        channel: u32,
        /// Opaque payload (mesh ciphertext); never inspected.
        payload: Vec<u8>,
    },
}

fn put_endpoint(out: &mut Vec<u8>, addr: SocketAddr) {
    let ip = match addr.ip() {
        IpAddr::V4(v4) => v4.to_ipv6_mapped(),
        IpAddr::V6(v6) => v6,
    };
    out.extend_from_slice(&ip.octets());
    out.extend_from_slice(&addr.port().to_be_bytes());
}

fn take_endpoint(bytes: &[u8]) -> SocketAddr {
    let mut ip = [0u8; 16];
    ip.copy_from_slice(&bytes[..16]);
    let ip = Ipv6Addr::from(ip);
    let port = u16::from_be_bytes([bytes[16], bytes[17]]);
    let ip = match ip.to_ipv4_mapped() {
        Some(v4) => IpAddr::V4(v4),
        None => IpAddr::V6(ip),
    };
    SocketAddr::new(ip, port)
}

impl Message {
    /// Encode to a datagram.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            Self::Hello { entity } => {
                out.push(kind::HELLO);
                out.extend_from_slice(entity.as_bytes());
                out.resize(HELLO_LEN, 0);
            }
            Self::Challenge { nonce, observed } => {
                out.push(kind::CHALLENGE);
                out.extend_from_slice(nonce);
                put_endpoint(&mut out, *observed);
            }
            Self::Register {
                entity,
                nonce,
                signature,
            } => {
                out.push(kind::REGISTER);
                out.extend_from_slice(entity.as_bytes());
                out.extend_from_slice(nonce);
                out.extend_from_slice(signature);
            }
            Self::Registered { id, ttl_secs } => {
                out.push(kind::REGISTERED);
                out.extend_from_slice(id);
                out.extend_from_slice(&ttl_secs.to_be_bytes());
            }
            Self::Bind { id } => {
                out.push(kind::BIND);
                out.extend_from_slice(id);
                out.resize(BIND_LEN, 0);
            }
            Self::Bound { id, channel } => {
                out.push(kind::BOUND);
                out.extend_from_slice(id);
                out.extend_from_slice(&channel.to_be_bytes());
            }
            Self::Refused(r) => {
                out.push(kind::REFUSED);
                out.push(*r as u8);
            }
            Self::Offer { splice } => {
                out.push(kind::OFFER);
                out.extend_from_slice(splice);
            }
            Self::Data { channel, payload } => {
                out.reserve(DATA_HEADER_LEN + payload.len());
                out.push(kind::DATA);
                out.extend_from_slice(&channel.to_be_bytes());
                out.extend_from_slice(payload);
            }
        }
        out
    }

    /// Strictly decode a datagram; `None` for unknown kinds or wrong lengths.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let (&k, body) = bytes.split_first()?;
        let arr = |range: std::ops::Range<usize>| -> Option<&[u8]> { body.get(range) };
        let fixed = |len: usize| bytes.len() == len;
        Some(match k {
            kind::HELLO if fixed(HELLO_LEN) => Self::Hello {
                entity: EntityId::from_bytes(arr(0..32)?.try_into().ok()?),
            },
            kind::CHALLENGE if fixed(CHALLENGE_LEN) => Self::Challenge {
                nonce: arr(0..16)?.try_into().ok()?,
                observed: take_endpoint(arr(16..16 + ENDPOINT_LEN)?),
            },
            kind::REGISTER if fixed(REGISTER_LEN) => Self::Register {
                entity: EntityId::from_bytes(arr(0..32)?.try_into().ok()?),
                nonce: arr(32..48)?.try_into().ok()?,
                signature: arr(48..112)?.try_into().ok()?,
            },
            kind::REGISTERED if fixed(REGISTERED_LEN) => Self::Registered {
                id: arr(0..16)?.try_into().ok()?,
                ttl_secs: u16::from_be_bytes(arr(16..18)?.try_into().ok()?),
            },
            kind::BIND if fixed(BIND_LEN) => Self::Bind {
                id: arr(0..16)?.try_into().ok()?,
            },
            kind::BOUND if fixed(BOUND_LEN) => Self::Bound {
                id: arr(0..16)?.try_into().ok()?,
                channel: u32::from_be_bytes(arr(16..20)?.try_into().ok()?),
            },
            kind::REFUSED if fixed(REFUSED_LEN) => Self::Refused(Refusal::from_code(body[0])?),
            kind::OFFER if fixed(OFFER_LEN) => Self::Offer {
                splice: arr(0..16)?.try_into().ok()?,
            },
            kind::DATA if bytes.len() >= DATA_HEADER_LEN => Self::Data {
                channel: u32::from_be_bytes(arr(0..4)?.try_into().ok()?),
                payload: body[4..].to_vec(),
            },
            _ => return None,
        })
    }
}

/// The message a device signs to register: binds the relay's nonce, the
/// endpoint the relay observed, and the entity.
pub fn register_message(nonce: &[u8; 16], observed: SocketAddr, entity: &EntityId) -> Vec<u8> {
    let mut m = REGISTER_DOMAIN.to_vec();
    m.extend_from_slice(nonce);
    put_endpoint(&mut m, observed);
    m.extend_from_slice(entity.as_bytes());
    m
}

/// Device side: answer a `CHALLENGE` with a signed `REGISTER`.
pub fn sign_register(keypair: &EntityKeypair, nonce: [u8; 16], observed: SocketAddr) -> Message {
    let entity = keypair.entity_id().clone();
    let signature = keypair
        .sign(&register_message(&nonce, observed, &entity))
        .to_bytes();
    Message::Register {
        entity,
        nonce,
        signature,
    }
}

/// Relay limits and timers.
#[derive(Clone, Debug)]
pub struct RelayConfig {
    /// Maximum live registrations.
    pub max_registrations: usize,
    /// Maximum channels per registration.
    pub max_channels_per_registration: usize,
    /// Maximum live channels in total.
    pub max_channels: usize,
    /// Registration lifetime without a refresh.
    pub registration_ttl: Duration,
    /// Channel lifetime without traffic.
    pub channel_idle: Duration,
    /// Maximum `BIND`s per registration per [`Self::bind_window`].
    pub binds_per_window: u32,
    /// Window for the bind rate limit.
    pub bind_window: Duration,
    /// Maximum splice `JOIN`s per registration per [`Self::bind_window`].
    pub splices_per_window: u32,
    /// Maximum splices waiting for their device, in total.
    pub max_pending_splices: usize,
    /// Maximum splices waiting or running, in total.
    pub max_live_splices: usize,
    /// Maximum open TCP connections (preambles, waiting joiners, splices).
    pub max_tcp_connections: usize,
    /// How long a joiner waits for the device to accept an offer.
    pub splice_accept_wait: Duration,
    /// Bytes forwarded per direction before a splice is cut.
    pub splice_max_bytes: u64,
    /// Lifetime of a running splice.
    pub splice_lifetime: Duration,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            max_registrations: 100_000,
            max_channels_per_registration: 64,
            max_channels: 250_000,
            registration_ttl: Duration::from_secs(90),
            channel_idle: Duration::from_secs(120),
            binds_per_window: 16,
            bind_window: Duration::from_secs(10),
            splices_per_window: 4,
            max_pending_splices: 1_024,
            max_live_splices: 4_096,
            max_tcp_connections: 8_192,
            splice_accept_wait: Duration::from_secs(5),
            // Enrollment is a handshake, one request and one bundle.
            splice_max_bytes: 1024 * 1024,
            splice_lifetime: Duration::from_secs(60),
        }
    }
}

/// Relay counters (monotonic).
#[derive(Debug, Default)]
pub struct RelayStats {
    /// Registrations accepted (including refreshes).
    pub registrations: AtomicU64,
    /// Channels opened.
    pub channels_opened: AtomicU64,
    /// Data datagrams forwarded.
    pub forwarded_packets: AtomicU64,
    /// Data bytes forwarded (payload only).
    pub forwarded_bytes: AtomicU64,
    /// Datagrams dropped (malformed, unknown channel, wrong endpoint).
    pub dropped: AtomicU64,
    /// Requests refused.
    pub refused: AtomicU64,
    /// Byte-stream splices established.
    pub splices_opened: AtomicU64,
    /// Bytes copied by splices (both directions).
    pub splice_bytes: AtomicU64,
    /// TCP tunnels opened.
    pub tunnels_opened: AtomicU64,
}

struct Registration {
    entity: EntityId,
    endpoint: SocketAddr,
    expires: Instant,
    channels: Vec<u32>,
    window_start: Instant,
    binds_in_window: u32,
    splice_window_start: Instant,
    splices_in_window: u32,
}

struct Channel {
    id: RegistrationId,
    joiner: SocketAddr,
    last_seen: Instant,
}

#[derive(Default)]
struct State {
    registrations: HashMap<RegistrationId, Registration>,
    channels: HashMap<u32, Channel>,
    /// Splices waiting for their device: splice id → (registration, opened).
    pending_splices: HashMap<SpliceId, (RegistrationId, Instant)>,
}

/// What the relay should send in response to one datagram.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// Send nothing.
    Drop,
    /// Send `bytes` to `to`.
    Send {
        /// Destination.
        to: SocketAddr,
        /// Datagram.
        bytes: Vec<u8>,
    },
}

/// The relay's state machine, independent of any socket (so it is testable
/// and deterministic). [`BlindRelay`] drives it from a UDP socket.
pub struct RelayCore {
    secret: [u8; 32],
    config: RelayConfig,
    state: Mutex<State>,
    stats: RelayStats,
}

impl RelayCore {
    /// A relay with a fresh challenge secret.
    pub fn new(config: RelayConfig) -> std::io::Result<Self> {
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret).map_err(|e| std::io::Error::other(e.to_string()))?;
        Ok(Self {
            secret,
            config,
            state: Mutex::new(State::default()),
            stats: RelayStats::default(),
        })
    }

    /// Counters.
    pub fn stats(&self) -> &RelayStats {
        &self.stats
    }

    /// Live registrations and channels.
    pub fn sizes(&self) -> (usize, usize) {
        let s = self.state.lock();
        (s.registrations.len(), s.channels.len())
    }

    fn nonce(&self, from: SocketAddr, entity: &EntityId, bucket: u64) -> [u8; 16] {
        let mut h = blake3::Hasher::new_keyed(&self.secret);
        h.update(CHALLENGE_CONTEXT.as_bytes());
        put_endpoint_hash(&mut h, from);
        h.update(entity.as_bytes());
        h.update(&bucket.to_be_bytes());
        let mut out = [0u8; 16];
        out.copy_from_slice(&h.finalize().as_bytes()[..16]);
        out
    }

    fn refuse(&self, to: SocketAddr, why: Refusal) -> Action {
        self.stats.refused.fetch_add(1, Ordering::Relaxed);
        Action::Send {
            to,
            bytes: Message::Refused(why).encode(),
        }
    }

    /// Handle one datagram from `from` at wall-clock `now_secs` / monotonic `now`.
    pub fn handle(&self, from: SocketAddr, bytes: &[u8], now_secs: u64, now: Instant) -> Action {
        let Some(message) = Message::decode(bytes) else {
            self.stats.dropped.fetch_add(1, Ordering::Relaxed);
            return Action::Drop;
        };
        match message {
            Message::Hello { entity } => {
                let nonce = self.nonce(from, &entity, now_secs / CHALLENGE_BUCKET_SECS);
                Action::Send {
                    to: from,
                    bytes: Message::Challenge {
                        nonce,
                        observed: from,
                    }
                    .encode(),
                }
            }
            Message::Register {
                entity,
                nonce,
                signature,
            } => {
                let bucket = now_secs / CHALLENGE_BUCKET_SECS;
                let fresh = [bucket, bucket.saturating_sub(1)]
                    .iter()
                    .any(|b| self.nonce(from, &entity, *b) == nonce);
                let signed = entity
                    .verify_bytes(&register_message(&nonce, from, &entity), &signature)
                    .is_ok();
                if !fresh || !signed {
                    return self.refuse(from, Refusal::BadProof);
                }
                let id = registration_id(&entity);
                let mut s = self.state.lock();
                if !s.registrations.contains_key(&id)
                    && s.registrations.len() >= self.config.max_registrations
                {
                    drop(s);
                    return self.refuse(from, Refusal::Capacity);
                }
                let expires = now + self.config.registration_ttl;
                s.registrations
                    .entry(id)
                    .and_modify(|r| {
                        r.endpoint = from;
                        r.expires = expires;
                    })
                    .or_insert(Registration {
                        entity,
                        endpoint: from,
                        expires,
                        channels: Vec::new(),
                        window_start: now,
                        binds_in_window: 0,
                        splice_window_start: now,
                        splices_in_window: 0,
                    });
                drop(s);
                self.stats.registrations.fetch_add(1, Ordering::Relaxed);
                Action::Send {
                    to: from,
                    bytes: Message::Registered {
                        id,
                        ttl_secs: self.config.registration_ttl.as_secs().min(u16::MAX as u64)
                            as u16,
                    }
                    .encode(),
                }
            }
            Message::Bind { id } => self.bind(from, id, now),
            Message::Data { channel, payload } => self.forward(from, channel, payload, now),
            // Relay-originated kinds arriving at the relay are dropped.
            _ => {
                self.stats.dropped.fetch_add(1, Ordering::Relaxed);
                Action::Drop
            }
        }
    }

    fn bind(&self, from: SocketAddr, id: RegistrationId, now: Instant) -> Action {
        let mut s = self.state.lock();
        let total = s.channels.len();
        let Some(reg) = s.registrations.get_mut(&id) else {
            drop(s);
            return self.refuse(from, Refusal::UnknownRegistration);
        };
        if reg.expires <= now {
            drop(s);
            return self.refuse(from, Refusal::UnknownRegistration);
        }
        if now.duration_since(reg.window_start) >= self.config.bind_window {
            reg.window_start = now;
            reg.binds_in_window = 0;
        }
        if reg.binds_in_window >= self.config.binds_per_window
            || reg.channels.len() >= self.config.max_channels_per_registration
            || total >= self.config.max_channels
        {
            drop(s);
            return self.refuse(from, Refusal::Capacity);
        }
        reg.binds_in_window += 1;
        let mut channel = 0u32;
        while channel == 0 || s.channels.contains_key(&channel) {
            let mut b = [0u8; 4];
            if getrandom::fill(&mut b).is_err() {
                drop(s);
                return self.refuse(from, Refusal::Capacity);
            }
            channel = u32::from_be_bytes(b);
        }
        s.channels.insert(
            channel,
            Channel {
                id,
                joiner: from,
                last_seen: now,
            },
        );
        if let Some(reg) = s.registrations.get_mut(&id) {
            reg.channels.push(channel);
        }
        drop(s);
        self.stats.channels_opened.fetch_add(1, Ordering::Relaxed);
        Action::Send {
            to: from,
            bytes: Message::Bound { id, channel }.encode(),
        }
    }

    fn forward(&self, from: SocketAddr, channel: u32, payload: Vec<u8>, now: Instant) -> Action {
        let mut s = self.state.lock();
        let State {
            registrations,
            channels,
            ..
        } = &mut *s;
        let target = channels.get_mut(&channel).and_then(|c| {
            let device = registrations
                .get(&c.id)
                .filter(|r| r.expires > now)?
                .endpoint;
            let to = if from == c.joiner {
                device
            } else if from == device {
                c.joiner
            } else {
                return None;
            };
            c.last_seen = now;
            Some(to)
        });
        drop(s);
        match target {
            Some(to) => {
                self.stats.forwarded_packets.fetch_add(1, Ordering::Relaxed);
                self.stats
                    .forwarded_bytes
                    .fetch_add(payload.len() as u64, Ordering::Relaxed);
                Action::Send {
                    to,
                    bytes: Message::Data { channel, payload }.encode(),
                }
            }
            None => {
                self.stats.dropped.fetch_add(1, Ordering::Relaxed);
                Action::Drop
            }
        }
    }

    /// A joiner asks to splice a byte stream to registration `id`: allocate a
    /// pending splice and return its id and the device endpoint to offer it
    /// to. Rate-limited per registration and bounded in total.
    pub fn begin_splice(
        &self,
        id: RegistrationId,
        now: Instant,
    ) -> Result<(SpliceId, SocketAddr), Refusal> {
        let mut s = self.state.lock();
        let pending = s.pending_splices.len();
        let reg = s
            .registrations
            .get_mut(&id)
            .filter(|r| r.expires > now)
            .ok_or(Refusal::UnknownRegistration)?;
        if now.duration_since(reg.splice_window_start) >= self.config.bind_window {
            reg.splice_window_start = now;
            reg.splices_in_window = 0;
        }
        if reg.splices_in_window >= self.config.splices_per_window
            || pending >= self.config.max_pending_splices
        {
            return Err(Refusal::Capacity);
        }
        reg.splices_in_window += 1;
        let endpoint = reg.endpoint;
        let mut splice = [0u8; 16];
        getrandom::fill(&mut splice).map_err(|_| Refusal::Capacity)?;
        s.pending_splices.insert(splice, (id, now));
        Ok((splice, endpoint))
    }

    /// The live endpoint of registration `id` (it may move on NAT rebinding).
    pub fn device_endpoint(&self, id: &RegistrationId, now: Instant) -> Option<SocketAddr> {
        self.state
            .lock()
            .registrations
            .get(id)
            .filter(|r| r.expires > now)
            .map(|r| r.endpoint)
    }

    /// The device claims a pending splice. `true` exactly once per splice id,
    /// and only while it is pending.
    pub fn claim_splice(&self, splice: &SpliceId) -> bool {
        self.state.lock().pending_splices.remove(splice).is_some()
    }

    /// Forget a pending splice (the joiner gave up or it was claimed).
    pub fn abandon_splice(&self, splice: &SpliceId) {
        self.state.lock().pending_splices.remove(splice);
    }

    /// Splices waiting for their device.
    pub fn pending_splices(&self) -> usize {
        self.state.lock().pending_splices.len()
    }

    /// Expire registrations (and their channels), idle channels and stale
    /// pending splices.
    pub fn sweep(&self, now: Instant) {
        let mut s = self.state.lock();
        let State {
            registrations,
            channels,
            pending_splices,
        } = &mut *s;
        let stale = self.config.splice_accept_wait * 2;
        pending_splices.retain(|_, (_, opened)| now.duration_since(*opened) < stale);
        registrations.retain(|_, r| r.expires > now);
        channels.retain(|_, c| {
            registrations.contains_key(&c.id)
                && now.duration_since(c.last_seen) < self.config.channel_idle
        });
        for reg in registrations.values_mut() {
            reg.channels.retain(|ch| channels.contains_key(ch));
        }
    }

    /// The entity registered under `id`, if live.
    pub fn registered_entity(&self, id: &RegistrationId) -> Option<EntityId> {
        self.state
            .lock()
            .registrations
            .get(id)
            .map(|r| r.entity.clone())
    }
}

fn put_endpoint_hash(h: &mut blake3::Hasher, addr: SocketAddr) {
    let mut buf = Vec::with_capacity(ENDPOINT_LEN);
    put_endpoint(&mut buf, addr);
    h.update(&buf);
}

/// A random port in the IANA dynamic range (49152–65535).
fn random_dynamic_port() -> std::io::Result<u16> {
    let mut b = [0u8; 2];
    getrandom::fill(&mut b).map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(49_152 + u16::from_be_bytes(b) % 16_384)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// How long a new TCP connection may take to send its preamble.
const PREAMBLE_WAIT: Duration = Duration::from_secs(5);
/// Offer resend interval while a joiner waits.
const OFFER_RESEND: Duration = Duration::from_millis(1_000);

/// A running blind relay: one UDP socket and, on the same port number, the TCP
/// listener for byte-stream splices.
pub struct BlindRelay {
    shared: Arc<Shared>,
    listener: TcpListener,
}

struct Shared {
    socket: Arc<UdpSocket>,
    core: Arc<RelayCore>,
    /// Joiners waiting for their device, by splice id.
    waiting: Mutex<HashMap<SpliceId, tokio::sync::oneshot::Sender<TcpStream>>>,
    live_splices: Arc<Semaphore>,
    connections: Arc<Semaphore>,
    /// Open TCP tunnels, by synthetic endpoint.
    tunnels: Mutex<HashMap<SocketAddr, tokio::sync::mpsc::Sender<Vec<u8>>>>,
    next_tunnel: AtomicU64,
    /// Test seam: drop every UDP datagram (a network that blocks UDP).
    udp_blocked: std::sync::atomic::AtomicBool,
}

impl BlindRelay {
    /// Bind the relay's UDP socket and its TCP listener on the same port.
    ///
    /// With port 0 the OS-chosen TCP port is tried first, then random ports
    /// from the IANA dynamic range until one binds for both protocols. Some
    /// hosts (Windows with Hyper-V/WSL) reserve large TCP-only and UDP-only
    /// port blocks inside the ephemeral range and hand out ephemeral ports
    /// sequentially, so retrying the OS's choice alone can keep landing in
    /// the same reserved block.
    pub async fn bind(addr: SocketAddr, config: RelayConfig) -> std::io::Result<Self> {
        let attempts = if addr.port() == 0 { 64 } else { 1 };
        let mut last = None;
        for attempt in 0..attempts {
            let candidate = if attempt == 0 {
                addr
            } else {
                SocketAddr::new(addr.ip(), random_dynamic_port()?)
            };
            let listener = match TcpListener::bind(candidate).await {
                Ok(listener) => listener,
                Err(e) => {
                    last = Some(e);
                    continue;
                }
            };
            let port = listener.local_addr()?.port();
            match UdpSocket::bind(SocketAddr::new(addr.ip(), port)).await {
                Ok(socket) => {
                    return Ok(Self {
                        shared: Arc::new(Shared {
                            socket: Arc::new(socket),
                            live_splices: Arc::new(Semaphore::new(config.max_live_splices)),
                            connections: Arc::new(Semaphore::new(config.max_tcp_connections)),
                            core: Arc::new(RelayCore::new(config)?),
                            waiting: Mutex::new(HashMap::new()),
                            tunnels: Mutex::new(HashMap::new()),
                            next_tunnel: AtomicU64::new(0),
                            udp_blocked: std::sync::atomic::AtomicBool::new(false),
                        }),
                        listener,
                    });
                }
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| std::io::Error::other("relay bind")))
    }

    /// Bound address (UDP; the TCP listener shares the port number).
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.shared.socket.local_addr()
    }

    /// Shared state machine (stats, sizes).
    pub fn core(&self) -> &Arc<RelayCore> {
        &self.shared.core
    }

    /// Test seam: while set, the relay drops every UDP datagram, as a network
    /// that blocks UDP would; TCP (splices and tunnels) is unaffected.
    #[doc(hidden)]
    pub fn block_udp_for_test(&self, blocked: bool) {
        self.shared.udp_blocked.store(blocked, Ordering::Relaxed);
    }

    /// Serve until the task is dropped/aborted.
    pub async fn run(&self) {
        tokio::join!(self.serve_udp(), self.serve_tcp());
    }

    async fn serve_udp(&self) {
        let shared = &self.shared;
        let mut buf = vec![0u8; 65_535];
        let mut last_sweep = Instant::now();
        loop {
            let recv =
                tokio::time::timeout(Duration::from_secs(5), shared.socket.recv_from(&mut buf))
                    .await;
            let now = Instant::now();
            if now.duration_since(last_sweep) >= Duration::from_secs(5) {
                shared.core.sweep(now);
                last_sweep = now;
            }
            let Ok(Ok((n, from))) = recv else {
                continue;
            };
            // A tunnel endpoint is never a UDP source (discard-only prefix);
            // a datagram claiming one is dropped before the state machine.
            if is_tunnel_endpoint(&from) || shared.udp_blocked.load(Ordering::Relaxed) {
                continue;
            }
            if let Action::Send { to, bytes } = shared.core.handle(from, &buf[..n], unix_now(), now)
            {
                shared.deliver(to, bytes).await;
            }
        }
    }

    async fn serve_tcp(&self) {
        loop {
            let stream = match self.listener.accept().await {
                Ok((stream, _)) => stream,
                Err(_) => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            };
            // Capacity is refused by closing, never by queueing.
            let Ok(permit) = self.shared.connections.clone().try_acquire_owned() else {
                self.shared
                    .core
                    .stats
                    .refused
                    .fetch_add(1, Ordering::Relaxed);
                continue;
            };
            let shared = self.shared.clone();
            tokio::spawn(async move {
                let _permit = permit;
                shared.serve_stream(stream).await;
            });
        }
    }
}

impl Shared {
    /// Send one relay datagram to `to`: down its tunnel when `to` is a tunnel
    /// endpoint (dropped if that tunnel is gone or full), else over UDP.
    async fn deliver(&self, to: SocketAddr, bytes: Vec<u8>) {
        if is_tunnel_endpoint(&to) {
            let tx = self.tunnels.lock().get(&to).cloned();
            if let Some(tx) = tx {
                let _ = tx.try_send(bytes);
            }
            return;
        }
        if self.udp_blocked.load(Ordering::Relaxed) {
            return;
        }
        let _ = self.socket.send_to(&bytes, to).await;
    }

    /// A node's TCP tunnel: a synthetic endpoint the state machine sees like
    /// a UDP source, fed by the stream's frames; everything addressed to it
    /// goes back down the stream. Ends at end of stream, on an error, or after
    /// [`RelayConfig::channel_idle`] without a frame.
    async fn tunnel(&self, mut stream: TcpStream, zero: [u8; 16]) {
        if zero != [0u8; 16] || stream.write_all(&[splice::OK]).await.is_err() {
            self.core.stats.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let endpoint = tunnel_endpoint(self.next_tunnel.fetch_add(1, Ordering::Relaxed));
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(TUNNEL_QUEUE);
        self.tunnels.lock().insert(endpoint, tx);
        self.core
            .stats
            .tunnels_opened
            .fetch_add(1, Ordering::Relaxed);
        let (mut reader, mut writer) = stream.into_split();
        let write = async {
            while let Some(frame) = rx.recv().await {
                if write_tunnel_frame(&mut writer, &frame).await.is_err() {
                    break;
                }
            }
        };
        let idle = self.core.config.channel_idle;
        let read = async {
            loop {
                let frame = match tokio::time::timeout(idle, read_tunnel_frame(&mut reader)).await {
                    Ok(Some(frame)) => frame,
                    _ => break,
                };
                let now = Instant::now();
                if let Action::Send { to, bytes } =
                    self.core.handle(endpoint, &frame, unix_now(), now)
                {
                    self.deliver(to, bytes).await;
                }
            }
        };
        tokio::select! {
            _ = read => {}
            _ = write => {}
        }
        self.tunnels.lock().remove(&endpoint);
    }

    async fn serve_stream(&self, mut stream: TcpStream) {
        let mut preamble = [0u8; splice::PREAMBLE_LEN];
        let read = tokio::time::timeout(PREAMBLE_WAIT, stream.read_exact(&mut preamble)).await;
        if !matches!(read, Ok(Ok(_))) {
            self.core.stats.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let mut id = [0u8; 16];
        id.copy_from_slice(&preamble[1..]);
        match preamble[0] {
            splice::JOIN => self.join(stream, id).await,
            splice::ACCEPT => self.accept(stream, id).await,
            splice::TUNNEL => self.tunnel(stream, id).await,
            _ => {
                self.core.stats.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    async fn refuse(&self, mut stream: TcpStream, why: Refusal) {
        self.core.stats.refused.fetch_add(1, Ordering::Relaxed);
        let _ = stream.write_all(&[why as u8]).await;
        let _ = stream.shutdown().await;
    }

    /// A joiner's splice: offer it to the device until it dials back or the
    /// wait expires, then copy bytes between the two streams.
    async fn join(&self, mut joiner: TcpStream, id: RegistrationId) {
        let Ok(live) = self.live_splices.clone().try_acquire_owned() else {
            return self.refuse(joiner, Refusal::Capacity).await;
        };
        let (splice_id, mut endpoint) = match self.core.begin_splice(id, Instant::now()) {
            Ok(v) => v,
            Err(why) => return self.refuse(joiner, why).await,
        };
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        self.waiting.lock().insert(splice_id, tx);
        let offer = Message::Offer { splice: splice_id }.encode();
        let deadline = tokio::time::Instant::now() + self.core.config.splice_accept_wait;
        let device = loop {
            self.deliver(endpoint, offer.clone()).await;
            let tick = (tokio::time::Instant::now() + OFFER_RESEND).min(deadline);
            match tokio::time::timeout_at(tick, &mut rx).await {
                Ok(Ok(device)) => break Some(device),
                Ok(Err(_)) => break None,
                Err(_) if tokio::time::Instant::now() >= deadline => break None,
                Err(_) => match self.core.device_endpoint(&id, Instant::now()) {
                    Some(current) => endpoint = current,
                    None => break None,
                },
            }
        };
        self.waiting.lock().remove(&splice_id);
        self.core.abandon_splice(&splice_id);
        let Some(mut device) = device else {
            return self.refuse(joiner, Refusal::Unreachable).await;
        };
        if joiner.write_all(&[splice::OK]).await.is_err()
            || device.write_all(&[splice::OK]).await.is_err()
        {
            return;
        }
        self.core
            .stats
            .splices_opened
            .fetch_add(1, Ordering::Relaxed);
        let config = &self.core.config;
        let copied = tokio::time::timeout(
            config.splice_lifetime,
            pipe(joiner, device, config.splice_max_bytes),
        )
        .await
        .unwrap_or(0);
        self.core
            .stats
            .splice_bytes
            .fetch_add(copied, Ordering::Relaxed);
        drop(live);
    }

    /// The device claims a pending splice with the id the relay offered it.
    async fn accept(&self, device: TcpStream, splice_id: SpliceId) {
        if self.core.claim_splice(&splice_id) {
            if let Some(joiner) = self.waiting.lock().remove(&splice_id) {
                let _ = joiner.send(device);
                return;
            }
        }
        self.refuse(device, Refusal::UnknownRegistration).await;
    }
}

/// Copy bytes both ways, each direction capped at `cap`; a finished direction
/// half-closes its destination. Returns the bytes copied.
async fn pipe(a: TcpStream, b: TcpStream, cap: u64) -> u64 {
    let (ar, mut aw) = a.into_split();
    let (br, mut bw) = b.into_split();
    let up = async {
        let n = tokio::io::copy(&mut ar.take(cap), &mut bw)
            .await
            .unwrap_or(0);
        let _ = bw.shutdown().await;
        n
    };
    let down = async {
        let n = tokio::io::copy(&mut br.take(cap), &mut aw)
            .await
            .unwrap_or(0);
        let _ = aw.shutdown().await;
        n
    };
    let (up, down) = tokio::join!(up, down);
    up + down
}

// ---- client side (a mesh node using a relay) ---------------------------------

/// Failure talking to a relay.
#[derive(Debug, thiserror::Error)]
pub enum RelayError {
    /// Socket error sending to the relay.
    #[error("relay transport: {0}")]
    Io(#[from] std::io::Error),
    /// No answer within the retry budget.
    #[error("relay did not answer")]
    Timeout,
    /// The relay closed a splice stream without a status.
    #[error("relay closed the stream")]
    Eof,
    /// The relay refused the request.
    #[error("relay refused: {0:?}")]
    Refused(Refusal),
    /// The node stopped listening for this relay.
    #[error("relay client closed")]
    Closed,
}

/// Per-attempt wait for a relay reply.
const RELAY_REPLY_WAIT: Duration = Duration::from_millis(1_500);
/// Attempts per request (UDP may drop either direction).
const RELAY_ATTEMPTS: u32 = 3;
/// UDP attempts before a request falls back to the TCP tunnel.
const RELAY_UDP_ATTEMPTS: u32 = 2;
/// Bound on connecting a tunnel and reading its status byte.
const TUNNEL_CONNECT_WAIT: Duration = Duration::from_secs(5);

/// Where a node's relay tunnels hand received datagrams: each with the
/// relay's address, for the node's relay ingress (the same path a UDP
/// datagram from the relay takes).
pub type TunnelIngress = tokio::sync::mpsc::Sender<(Vec<u8>, SocketAddr)>;
/// The receiving end of [`TunnelIngress`].
pub type TunnelIngressRx = tokio::sync::mpsc::Receiver<(Vec<u8>, SocketAddr)>;

/// A mesh node's handle on one relay. Requests go out on the node's own mesh
/// socket (so a device's registration holds the same NAT mapping its relayed
/// traffic uses); the node's receive loop hands relay control replies to
/// [`Self::deliver`]. Requests are serialized per relay.
pub struct RelayClient {
    relay: SocketAddr,
    socket: Arc<crate::adapter::net::transport::NetSocket>,
    /// The node's relay tunnels (shared with its send path).
    tunnels: Arc<crate::adapter::net::transport::RelayTunnels>,
    ingress: TunnelIngress,
    /// Serializes tunnel opens to this relay.
    opening: tokio::sync::Mutex<()>,
    replies_tx: tokio::sync::mpsc::Sender<Message>,
    replies: tokio::sync::Mutex<tokio::sync::mpsc::Receiver<Message>>,
    /// Where splice offers go while this node accepts splices.
    offers: Mutex<Option<tokio::sync::mpsc::Sender<SpliceId>>>,
}

impl RelayClient {
    /// A client for `relay` sending on `socket`, falling back to a TCP
    /// tunnel (installed in `tunnels`, received frames handed to `ingress`)
    /// when UDP to the relay gets no answer.
    pub fn new(
        relay: SocketAddr,
        socket: Arc<crate::adapter::net::transport::NetSocket>,
        tunnels: Arc<crate::adapter::net::transport::RelayTunnels>,
        ingress: TunnelIngress,
    ) -> Self {
        let (replies_tx, replies) = tokio::sync::mpsc::channel(32);
        Self {
            relay,
            socket,
            tunnels,
            ingress,
            opening: tokio::sync::Mutex::new(()),
            replies_tx,
            replies: tokio::sync::Mutex::new(replies),
            offers: Mutex::new(None),
        }
    }

    /// The relay's UDP tuple.
    pub fn relay(&self) -> SocketAddr {
        self.relay
    }

    /// Hand a control datagram received from the relay to a waiting request,
    /// or a splice offer to the acceptor (dropped when not accepting). Data
    /// frames are not control and are ignored here.
    pub fn deliver(&self, bytes: &[u8]) {
        match Message::decode(bytes) {
            Some(Message::Data { .. }) | None => {}
            Some(Message::Offer { splice }) => {
                if let Some(offers) = self.offers.lock().as_ref() {
                    let _ = offers.try_send(splice);
                }
            }
            Some(message) => {
                let _ = self.replies_tx.try_send(message);
            }
        }
    }

    /// Whether this node currently reaches the relay through a TCP tunnel.
    pub fn tunneled(&self) -> bool {
        self.tunnels.contains(&self.relay)
    }

    /// One request/reply exchange. While a tunnel to this relay is up, over
    /// it (one endpoint for everything this node holds at the relay). Else
    /// over UDP, and when UDP gets no answer, over a freshly opened tunnel:
    /// TCP is the last resort, never the first try.
    async fn exchange<R>(
        &self,
        request: &Message,
        accept: impl Fn(&Message) -> Option<R>,
    ) -> Result<R, RelayError> {
        let mut replies = self.replies.lock().await;
        while replies.try_recv().is_ok() {}
        let bytes = request.encode();
        if let Some(tx) = self.tunnels.tunnel_sender(self.relay) {
            return self
                .attempts(&mut replies, &bytes, Some(&tx), RELAY_ATTEMPTS, &accept)
                .await;
        }
        match self
            .attempts(&mut replies, &bytes, None, RELAY_UDP_ATTEMPTS, &accept)
            .await
        {
            Err(RelayError::Timeout) => {}
            answered => return answered,
        }
        let tx = self.open_tunnel().await?;
        self.attempts(&mut replies, &bytes, Some(&tx), RELAY_ATTEMPTS, &accept)
            .await
    }

    /// Open (or reuse) the TCP tunnel to this relay: `[TUNNEL][0; 16]`, one
    /// status byte, then `u16`-length-prefixed datagrams both ways. The
    /// tunnel is installed for the node's send path and removed when its
    /// stream ends; received datagrams go to the node's relay ingress.
    async fn open_tunnel(&self) -> Result<tokio::sync::mpsc::Sender<Vec<u8>>, RelayError> {
        let _serial = self.opening.lock().await;
        if let Some(tx) = self.tunnels.tunnel_sender(self.relay) {
            return Ok(tx);
        }
        let relay = self.relay;
        let stream = tokio::time::timeout(TUNNEL_CONNECT_WAIT, async {
            let mut stream = TcpStream::connect(relay).await?;
            let mut preamble = [0u8; splice::PREAMBLE_LEN];
            preamble[0] = splice::TUNNEL;
            stream.write_all(&preamble).await?;
            let mut status = [0u8; 1];
            stream.read_exact(&mut status).await?;
            match status[0] {
                splice::OK => Ok(stream),
                code => Err(RelayError::Refused(
                    Refusal::from_code(code).unwrap_or(Refusal::BadProof),
                )),
            }
        })
        .await
        .map_err(|_| RelayError::Timeout)??;
        let (mut reader, mut writer) = stream.into_split();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(TUNNEL_QUEUE);
        self.tunnels.install(relay, tx.clone());
        let mine = tx.downgrade();
        let tunnels = self.tunnels.clone();
        let ingress = self.ingress.clone();
        tokio::spawn(async move {
            let write = async {
                while let Some(frame) = rx.recv().await {
                    if write_tunnel_frame(&mut writer, &frame).await.is_err() {
                        break;
                    }
                }
            };
            let read = async {
                while let Some(frame) = read_tunnel_frame(&mut reader).await {
                    if ingress.send((frame, relay)).await.is_err() {
                        break;
                    }
                }
            };
            tokio::select! {
                _ = read => {}
                _ = write => {}
            }
            if let Some(tx) = mine.upgrade() {
                tunnels.remove_if(relay, &tx);
            }
            tracing::debug!(%relay, "blind relay tunnel ended");
        });
        tracing::info!(%relay, "blind relay: UDP unanswered, using the TCP tunnel");
        Ok(tx)
    }

    async fn attempts<R>(
        &self,
        replies: &mut tokio::sync::mpsc::Receiver<Message>,
        bytes: &[u8],
        tunnel: Option<&tokio::sync::mpsc::Sender<Vec<u8>>>,
        attempts: u32,
        accept: &impl Fn(&Message) -> Option<R>,
    ) -> Result<R, RelayError> {
        for _ in 0..attempts {
            match tunnel {
                Some(tx) => tx
                    .send(bytes.to_vec())
                    .await
                    .map_err(|_| RelayError::Closed)?,
                None => {
                    self.socket.send_to(bytes, self.relay).await?;
                }
            }
            let deadline = tokio::time::Instant::now() + RELAY_REPLY_WAIT;
            loop {
                match tokio::time::timeout_at(deadline, replies.recv()).await {
                    Ok(Some(Message::Refused(why))) => return Err(RelayError::Refused(why)),
                    Ok(Some(reply)) => {
                        if let Some(value) = accept(&reply) {
                            return Ok(value);
                        }
                    }
                    Ok(None) => return Err(RelayError::Closed),
                    Err(_) => break,
                }
            }
        }
        Err(RelayError::Timeout)
    }

    /// Register `keypair`'s entity: challenge, signed proof, registration id
    /// and the relay's refresh deadline.
    pub async fn register(
        &self,
        keypair: &EntityKeypair,
    ) -> Result<(RegistrationId, Duration), RelayError> {
        let hello = Message::Hello {
            entity: keypair.entity_id().clone(),
        };
        let (nonce, observed) = self
            .exchange(&hello, |m| match m {
                Message::Challenge { nonce, observed } => Some((*nonce, *observed)),
                _ => None,
            })
            .await?;
        let expected = registration_id(keypair.entity_id());
        self.exchange(&sign_register(keypair, nonce, observed), |m| match m {
            Message::Registered { id, ttl_secs } if *id == expected => {
                Some((*id, Duration::from_secs(u64::from(*ttl_secs))))
            }
            _ => None,
        })
        .await
    }

    /// Open a channel to registration `id`; the relayed endpoint to use.
    pub async fn bind(&self, id: RegistrationId) -> Result<u32, RelayError> {
        self.exchange(&Message::Bind { id }, |m| match m {
            Message::Bound { id: bound, channel } if *bound == id => Some(*channel),
            _ => None,
        })
        .await
    }
}

/// Wait for a splice's status byte (the relay may hold a joiner for its whole
/// offer wait before answering).
const SPLICE_STATUS_WAIT: Duration = Duration::from_secs(15);
/// Concurrent splice dial-backs per registration.
const SPLICE_DIALS: usize = 8;
/// Recently seen offers remembered to ignore resends.
const SEEN_OFFERS: usize = 64;

async fn splice_request(
    relay: SocketAddr,
    kind: u8,
    id: [u8; 16],
) -> Result<TcpStream, RelayError> {
    let exchange = async {
        let mut stream = TcpStream::connect(relay).await?;
        let mut preamble = [0u8; splice::PREAMBLE_LEN];
        preamble[0] = kind;
        preamble[1..].copy_from_slice(&id);
        stream.write_all(&preamble).await?;
        let mut status = [0u8; 1];
        match stream.read_exact(&mut status).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Err(RelayError::Eof),
            Err(e) => return Err(e.into()),
        }
        match status[0] {
            splice::OK => Ok(stream),
            code => Err(RelayError::Refused(
                Refusal::from_code(code).unwrap_or(Refusal::BadProof),
            )),
        }
    };
    tokio::time::timeout(SPLICE_STATUS_WAIT, exchange)
        .await
        .map_err(|_| RelayError::Timeout)?
}

/// Joiner side: open a byte stream to the device registered as `id` on
/// `relay`. On `Ok`, the stream carries the device's bytes; it is still
/// unauthenticated — run the end-to-end handshake over it.
pub async fn open_splice(relay: SocketAddr, id: RegistrationId) -> Result<TcpStream, RelayError> {
    splice_request(relay, splice::JOIN, id).await
}

/// Device side: claim the splice the relay offered.
pub async fn accept_splice(relay: SocketAddr, splice: SpliceId) -> Result<TcpStream, RelayError> {
    splice_request(relay, splice::ACCEPT, splice).await
}

/// A live registration with a relay. Dropping it stops the refresh (and any
/// splice acceptor); the relay forgets the registration after its TTL.
pub struct RelayRegistration {
    relay: SocketAddr,
    id: RegistrationId,
    client: Arc<RelayClient>,
    refresh: tokio::task::JoinHandle<()>,
    splicer: Option<tokio::task::JoinHandle<()>>,
}

impl RelayRegistration {
    /// Wrap a registration, its client and the task refreshing it.
    pub fn new(
        client: Arc<RelayClient>,
        id: RegistrationId,
        refresh: tokio::task::JoinHandle<()>,
    ) -> Self {
        Self {
            relay: client.relay(),
            id,
            client,
            refresh,
            splicer: None,
        }
    }

    /// Accept byte-stream splices joiners open through the relay: each offer
    /// is dialled back once and the spliced stream (status already read) is
    /// yielded. Streams are unauthenticated until the caller's handshake.
    /// Offers arriving while the receiver is full are dropped.
    pub fn accept_splices(&mut self, capacity: usize) -> tokio::sync::mpsc::Receiver<TcpStream> {
        let (offers_tx, mut offers) = tokio::sync::mpsc::channel(16);
        let (out, streams) = tokio::sync::mpsc::channel(capacity.max(1));
        *self.client.offers.lock() = Some(offers_tx);
        let relay = self.relay;
        let dials = Arc::new(Semaphore::new(SPLICE_DIALS));
        if let Some(old) = self.splicer.take() {
            old.abort();
        }
        self.splicer = Some(tokio::spawn(async move {
            let mut seen = std::collections::VecDeque::with_capacity(SEEN_OFFERS);
            while let Some(splice) = offers.recv().await {
                if seen.contains(&splice) {
                    continue;
                }
                if seen.len() == SEEN_OFFERS {
                    seen.pop_front();
                }
                seen.push_back(splice);
                let Ok(permit) = dials.clone().try_acquire_owned() else {
                    continue;
                };
                let out = out.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    match accept_splice(relay, splice).await {
                        Ok(stream) => {
                            let _ = out.try_send(stream);
                        }
                        Err(e) => {
                            tracing::debug!(%relay, error = %e, "blind relay splice dial-back failed")
                        }
                    }
                });
            }
        }));
        streams
    }

    /// The relay's UDP tuple.
    pub fn relay(&self) -> SocketAddr {
        self.relay
    }

    /// The id joiners bind to.
    pub fn id(&self) -> RegistrationId {
        self.id
    }
}

impl Drop for RelayRegistration {
    fn drop(&mut self) {
        self.refresh.abort();
        if let Some(splicer) = self.splicer.take() {
            splicer.abort();
            *self.client.offers.lock() = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    fn register(relay: &RelayCore, kp: &EntityKeypair, from: SocketAddr, now: Instant) -> Action {
        let Action::Send { bytes, .. } = relay.handle(
            from,
            &Message::Hello {
                entity: kp.entity_id().clone(),
            }
            .encode(),
            1_000,
            now,
        ) else {
            panic!("no challenge");
        };
        let Some(Message::Challenge { nonce, observed }) = Message::decode(&bytes) else {
            panic!("bad challenge");
        };
        assert_eq!(observed, from);
        relay.handle(
            from,
            &sign_register(kp, nonce, observed).encode(),
            1_000,
            now,
        )
    }

    fn decoded(action: Action) -> (SocketAddr, Message) {
        match action {
            Action::Send { to, bytes } => (to, Message::decode(&bytes).unwrap()),
            Action::Drop => panic!("expected a reply"),
        }
    }

    #[test]
    fn codec_round_trips_and_rejects_wrong_lengths() {
        let kp = EntityKeypair::generate();
        let messages = [
            Message::Hello {
                entity: kp.entity_id().clone(),
            },
            Message::Challenge {
                nonce: [7; 16],
                observed: addr("203.0.113.9:40000"),
            },
            Message::Challenge {
                nonce: [7; 16],
                observed: addr("[2001:db8::1]:9"),
            },
            sign_register(&kp, [1; 16], addr("10.0.0.1:1")),
            Message::Registered {
                id: [2; 16],
                ttl_secs: 90,
            },
            Message::Bind { id: [3; 16] },
            Message::Bound {
                id: [3; 16],
                channel: 42,
            },
            Message::Refused(Refusal::Capacity),
            Message::Refused(Refusal::Unreachable),
            Message::Offer { splice: [4; 16] },
            Message::Data {
                channel: 9,
                payload: b"ciphertext".to_vec(),
            },
        ];
        for m in messages {
            let bytes = m.encode();
            assert_eq!(Message::decode(&bytes), Some(m.clone()));
            if !matches!(m, Message::Data { .. }) {
                let mut longer = bytes.clone();
                longer.push(0);
                assert_eq!(Message::decode(&longer), None, "{m:?}");
                assert_eq!(Message::decode(&bytes[..bytes.len() - 1]), None, "{m:?}");
            }
        }
        assert_eq!(Message::decode(&[0xEE, 1, 2]), None);
        assert_eq!(Message::decode(&[]), None);
    }

    #[test]
    fn replies_are_never_larger_than_their_requests() {
        const {
            assert!(CHALLENGE_LEN <= HELLO_LEN);
            assert!(REGISTERED_LEN <= REGISTER_LEN);
            assert!(BOUND_LEN <= BIND_LEN);
            assert!(REFUSED_LEN <= BIND_LEN && REFUSED_LEN <= REGISTER_LEN);
        }
    }

    #[test]
    fn a_device_registers_under_its_own_derived_id() {
        let relay = RelayCore::new(RelayConfig::default()).unwrap();
        let kp = EntityKeypair::generate();
        let (_, reply) = decoded(register(
            &relay,
            &kp,
            addr("198.51.100.7:5000"),
            Instant::now(),
        ));
        assert_eq!(
            reply,
            Message::Registered {
                id: registration_id(kp.entity_id()),
                ttl_secs: 90
            }
        );
        assert_eq!(
            relay.registered_entity(&registration_id(kp.entity_id())),
            Some(kp.entity_id().clone())
        );
    }

    #[test]
    fn a_registration_replayed_from_another_address_is_refused() {
        let relay = RelayCore::new(RelayConfig::default()).unwrap();
        let kp = EntityKeypair::generate();
        let device = addr("198.51.100.7:5000");
        let now = Instant::now();
        let (_, Message::Challenge { nonce, observed }) = decoded(
            relay.handle(
                device,
                &Message::Hello {
                    entity: kp.entity_id().clone(),
                }
                .encode(),
                1_000,
                now,
            ),
        ) else {
            panic!()
        };
        let proof = sign_register(&kp, nonce, observed).encode();
        // Captured and replayed from the attacker's endpoint: refused.
        let (to, reply) = decoded(relay.handle(addr("192.0.2.66:6666"), &proof, 1_000, now));
        assert_eq!(
            (to, reply),
            (addr("192.0.2.66:6666"), Message::Refused(Refusal::BadProof))
        );
        assert_eq!(relay.sizes().0, 0);
        // A proof signed by another key for this entity id is refused.
        let other = EntityKeypair::generate();
        let Message::Register { signature, .. } = sign_register(&other, nonce, observed) else {
            panic!()
        };
        let forged = Message::Register {
            entity: kp.entity_id().clone(),
            nonce,
            signature,
        };
        assert_eq!(
            decoded(relay.handle(device, &forged.encode(), 1_000, now)).1,
            Message::Refused(Refusal::BadProof)
        );
        // A stale challenge (two buckets later) is refused.
        assert_eq!(
            decoded(relay.handle(device, &proof, 1_000 + 2 * CHALLENGE_BUCKET_SECS, now)).1,
            Message::Refused(Refusal::BadProof)
        );
        // The genuine proof from the observed endpoint registers.
        assert!(matches!(
            decoded(relay.handle(device, &proof, 1_000, now)).1,
            Message::Registered { .. }
        ));
    }

    #[test]
    fn data_is_forwarded_only_between_the_channel_endpoints() {
        let relay = RelayCore::new(RelayConfig::default()).unwrap();
        let kp = EntityKeypair::generate();
        let (device, joiner, stranger) = (
            addr("198.51.100.7:5000"),
            addr("203.0.113.9:40000"),
            addr("192.0.2.66:6666"),
        );
        let now = Instant::now();
        register(&relay, &kp, device, now);
        let id = registration_id(kp.entity_id());
        let (to, Message::Bound { channel, .. }) =
            decoded(relay.handle(joiner, &Message::Bind { id }.encode(), 1_000, now))
        else {
            panic!()
        };
        assert_eq!(to, joiner);
        let data = |c: u32, p: &[u8]| {
            Message::Data {
                channel: c,
                payload: p.to_vec(),
            }
            .encode()
        };
        // Joiner → device and device → joiner, payload untouched.
        assert_eq!(
            decoded(relay.handle(joiner, &data(channel, b"msg1"), 1_000, now)),
            (
                device,
                Message::Data {
                    channel,
                    payload: b"msg1".to_vec()
                }
            )
        );
        assert_eq!(
            decoded(relay.handle(device, &data(channel, b"msg2"), 1_000, now)),
            (
                joiner,
                Message::Data {
                    channel,
                    payload: b"msg2".to_vec()
                }
            )
        );
        // A stranger cannot inject on the channel; unknown channels drop.
        assert_eq!(
            relay.handle(stranger, &data(channel, b"x"), 1_000, now),
            Action::Drop
        );
        assert_eq!(
            relay.handle(joiner, &data(channel ^ 1, b"x"), 1_000, now),
            Action::Drop
        );
        // The device re-registers from a new endpoint (NAT rebinding): data follows it.
        let moved = addr("198.51.100.7:5999");
        register(&relay, &kp, moved, now);
        assert_eq!(
            decoded(relay.handle(joiner, &data(channel, b"m"), 1_000, now)).0,
            moved
        );
        assert_eq!(
            relay.handle(device, &data(channel, b"m"), 1_000, now),
            Action::Drop
        );
    }

    #[test]
    fn binds_are_bounded_and_state_expires() {
        let config = RelayConfig {
            max_channels_per_registration: 2,
            binds_per_window: 10,
            ..RelayConfig::default()
        };
        let relay = RelayCore::new(config.clone()).unwrap();
        let kp = EntityKeypair::generate();
        let now = Instant::now();
        register(&relay, &kp, addr("198.51.100.7:5000"), now);
        let id = registration_id(kp.entity_id());
        let bind = |from: &str| {
            decoded(relay.handle(addr(from), &Message::Bind { id }.encode(), 1_000, now)).1
        };
        assert!(matches!(bind("203.0.113.1:1"), Message::Bound { .. }));
        assert!(matches!(bind("203.0.113.2:1"), Message::Bound { .. }));
        assert_eq!(bind("203.0.113.3:1"), Message::Refused(Refusal::Capacity));
        assert_eq!(
            decoded(relay.handle(
                addr("203.0.113.4:1"),
                &Message::Bind { id: [9; 16] }.encode(),
                1_000,
                now
            ))
            .1,
            Message::Refused(Refusal::UnknownRegistration)
        );
        // Everything expires once the registration is not refreshed.
        relay.sweep(now + config.registration_ttl + Duration::from_secs(1));
        assert_eq!(relay.sizes(), (0, 0));
    }

    #[test]
    fn the_bind_rate_is_limited_per_registration() {
        let config = RelayConfig {
            binds_per_window: 3,
            ..RelayConfig::default()
        };
        let relay = RelayCore::new(config).unwrap();
        let kp = EntityKeypair::generate();
        let now = Instant::now();
        register(&relay, &kp, addr("198.51.100.7:5000"), now);
        let id = registration_id(kp.entity_id());
        let results: Vec<Message> = (0..4)
            .map(|i| {
                decoded(relay.handle(
                    addr(&format!("203.0.113.{}:1", i + 1)),
                    &Message::Bind { id }.encode(),
                    1_000,
                    now,
                ))
                .1
            })
            .collect();
        assert!(results[..3]
            .iter()
            .all(|m| matches!(m, Message::Bound { .. })));
        assert_eq!(results[3], Message::Refused(Refusal::Capacity));
    }

    async fn ask(relay: SocketAddr, sock: &UdpSocket, m: Message) {
        sock.send_to(&m.encode(), relay).await.unwrap();
    }

    #[tokio::test]
    async fn a_live_relay_forwards_between_real_sockets() {
        let relay = BlindRelay::bind("127.0.0.1:0".parse().unwrap(), RelayConfig::default())
            .await
            .unwrap();
        let relay_addr = relay.local_addr().unwrap();
        let core = relay.core().clone();
        let task = tokio::spawn(async move { relay.run().await });
        let device = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let joiner = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let kp = EntityKeypair::generate();
        let mut buf = vec![0u8; 2048];
        ask(
            relay_addr,
            &device,
            Message::Hello {
                entity: kp.entity_id().clone(),
            },
        )
        .await;
        let n = device.recv(&mut buf).await.unwrap();
        let Some(Message::Challenge { nonce, observed }) = Message::decode(&buf[..n]) else {
            panic!()
        };
        ask(relay_addr, &device, sign_register(&kp, nonce, observed)).await;
        let n = device.recv(&mut buf).await.unwrap();
        let Some(Message::Registered { id, .. }) = Message::decode(&buf[..n]) else {
            panic!()
        };
        ask(relay_addr, &joiner, Message::Bind { id }).await;
        let n = joiner.recv(&mut buf).await.unwrap();
        let Some(Message::Bound { channel, .. }) = Message::decode(&buf[..n]) else {
            panic!()
        };
        ask(
            relay_addr,
            &joiner,
            Message::Data {
                channel,
                payload: b"hello device".to_vec(),
            },
        )
        .await;
        let n = device.recv(&mut buf).await.unwrap();
        assert_eq!(
            Message::decode(&buf[..n]),
            Some(Message::Data {
                channel,
                payload: b"hello device".to_vec()
            })
        );
        ask(
            relay_addr,
            &device,
            Message::Data {
                channel,
                payload: b"hello joiner".to_vec(),
            },
        )
        .await;
        let n = joiner.recv(&mut buf).await.unwrap();
        assert_eq!(
            Message::decode(&buf[..n]),
            Some(Message::Data {
                channel,
                payload: b"hello joiner".to_vec()
            })
        );
        assert_eq!(core.stats().forwarded_packets.load(Ordering::Relaxed), 2);
        task.abort();
    }

    fn mesh_config() -> crate::adapter::net::MeshNodeConfig {
        let mut cfg =
            crate::adapter::net::MeshNodeConfig::new("127.0.0.1:0".parse().unwrap(), [0x5Au8; 32])
                .with_heartbeat_interval(Duration::from_millis(500))
                .with_session_timeout(Duration::from_secs(5))
                .with_handshake(3, Duration::from_secs(3));
        cfg.socket_buffers = crate::adapter::net::SocketBufferConfig {
            send_buffer_size: 256 * 1024,
            recv_buffer_size: 256 * 1024,
        };
        cfg
    }

    async fn mesh_node() -> Arc<crate::adapter::net::MeshNode> {
        Arc::new(
            crate::adapter::net::MeshNode::new(EntityKeypair::generate(), mesh_config())
                .await
                .expect("MeshNode::new"),
        )
    }

    /// A real mesh session — routed handshake plus a sealed request/ack
    /// round trip — runs end-to-end through a blind relay: the device only
    /// registered from its mesh socket, the joiner only bound a channel, and
    /// every packet between them was forwarded by the relay as opaque data.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_mesh_session_runs_end_to_end_through_the_blind_relay() {
        use crate::adapter::net::{ChannelName, PeerAddr};

        let relay = BlindRelay::bind("127.0.0.1:0".parse().unwrap(), RelayConfig::default())
            .await
            .unwrap();
        let relay_addr = relay.local_addr().unwrap();
        let core = relay.core().clone();
        let relay_task = tokio::spawn(async move { relay.run().await });

        let device = mesh_node().await;
        let joiner = mesh_node().await;
        device.start();
        joiner.start();

        let registration = device.relay_register(relay_addr).await.expect("register");
        assert_eq!(registration.id(), registration_id(device.entity_id()));
        let via = joiner
            .relay_bind(relay_addr, registration.id())
            .await
            .expect("bind");
        assert!(matches!(via, PeerAddr::Relayed { relay, .. } if relay == relay_addr));

        let before = core.stats().forwarded_packets.load(Ordering::Relaxed);
        joiner
            .connect_via_endpoint(via, device.public_key(), device.node_id())
            .await
            .expect("routed handshake through the relay");
        let channel = ChannelName::new("relay.e2e").unwrap();
        tokio::time::timeout(
            Duration::from_secs(5),
            joiner.subscribe_channel(device.node_id(), channel),
        )
        .await
        .expect("the round trip must not hang")
        .expect("a sealed request and its ack must cross the relay");
        let forwarded = core.stats().forwarded_packets.load(Ordering::Relaxed) - before;
        // msg1 + msg2 + request + ack, at least.
        assert!(forwarded >= 4, "relay forwarded only {forwarded} packets");

        drop(registration);
        relay_task.abort();
    }

    /// Without a registration the relay has nothing to bind to, and a joiner
    /// cannot reach the device through it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn binding_an_unregistered_device_is_refused() {
        let relay = BlindRelay::bind("127.0.0.1:0".parse().unwrap(), RelayConfig::default())
            .await
            .unwrap();
        let relay_addr = relay.local_addr().unwrap();
        let relay_task = tokio::spawn(async move { relay.run().await });
        let device = mesh_node().await;
        let joiner = mesh_node().await;
        joiner.start();
        let err = joiner
            .relay_bind(relay_addr, registration_id(device.entity_id()))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("UnknownRegistration"), "{err}");
        relay_task.abort();
    }

    fn registered(
        core: &RelayCore,
        kp: &EntityKeypair,
        from: SocketAddr,
        now: Instant,
    ) -> RegistrationId {
        let (_, Message::Registered { id, .. }) = decoded(register(core, kp, from, now)) else {
            panic!("not registered");
        };
        id
    }

    /// Splices exist only for live registrations, are rate-limited per
    /// registration and bounded in total, and each id is claimable once.
    #[test]
    fn splices_are_bounded_rate_limited_and_claimed_once() {
        let config = RelayConfig {
            splices_per_window: 2,
            max_pending_splices: 3,
            ..RelayConfig::default()
        };
        let core = RelayCore::new(config.clone()).unwrap();
        let now = Instant::now();
        let device = addr("198.51.100.7:4000");
        let a = registered(&core, &EntityKeypair::generate(), device, now);
        let b = registered(
            &core,
            &EntityKeypair::generate(),
            addr("198.51.100.8:4000"),
            now,
        );

        assert_eq!(
            core.begin_splice([9; 16], now),
            Err(Refusal::UnknownRegistration)
        );
        let (first, endpoint) = core.begin_splice(a, now).unwrap();
        assert_eq!(endpoint, device, "offers go to the registered endpoint");
        let (second, _) = core.begin_splice(a, now).unwrap();
        assert_ne!(first, second);
        assert_eq!(core.begin_splice(a, now), Err(Refusal::Capacity), "rate");
        core.begin_splice(b, now).unwrap();
        assert_eq!(core.begin_splice(b, now), Err(Refusal::Capacity), "total");

        assert!(core.claim_splice(&first));
        assert!(!core.claim_splice(&first), "claimed twice");
        core.abandon_splice(&second);
        assert!(!core.claim_splice(&second), "claimed after abandon");
        assert!(!core.claim_splice(&[0; 16]), "unknown id");

        let later = now + config.bind_window;
        core.begin_splice(a, later).unwrap();
        assert_eq!(core.pending_splices(), 2);
        core.sweep(later + config.splice_accept_wait * 2);
        assert_eq!(core.pending_splices(), 0, "stale pending splices expire");
        assert_eq!(
            core.begin_splice(a, now + config.registration_ttl),
            Err(Refusal::UnknownRegistration),
            "an expired registration takes no splices"
        );
    }

    async fn live_relay(
        config: RelayConfig,
    ) -> (SocketAddr, Arc<RelayCore>, tokio::task::JoinHandle<()>) {
        let relay = BlindRelay::bind("127.0.0.1:0".parse().unwrap(), config)
            .await
            .unwrap();
        let addr = relay.local_addr().unwrap();
        let core = relay.core().clone();
        (addr, core, tokio::spawn(async move { relay.run().await }))
    }

    /// A joiner's byte stream reaches the device that registered from its
    /// mesh socket: the relay offers the splice over UDP, the device dials
    /// back, and bytes flow both ways through the relay.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_byte_stream_is_spliced_to_the_registered_device() {
        let (relay, core, task) = live_relay(RelayConfig::default()).await;
        let device = mesh_node().await;
        device.start();
        let mut registration = device.relay_register(relay).await.unwrap();
        let mut streams = registration.accept_splices(4);

        let mut joiner = open_splice(relay, registration.id())
            .await
            .expect("spliced");
        let mut at_device = tokio::time::timeout(Duration::from_secs(5), streams.recv())
            .await
            .expect("device got the stream")
            .unwrap();
        joiner.write_all(b"to device").await.unwrap();
        let mut buf = [0u8; 9];
        at_device.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"to device");
        at_device.write_all(b"to joiner").await.unwrap();
        joiner.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"to joiner");
        assert_eq!(core.stats().splices_opened.load(Ordering::Relaxed), 1);
        assert_eq!(core.pending_splices(), 0);
        task.abort();
    }

    /// A device that does not accept splices leaves the joiner refused as
    /// unreachable once the offer wait runs out; nothing stays pending.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_splice_the_device_never_accepts_is_refused_as_unreachable() {
        let (relay, core, task) = live_relay(RelayConfig {
            splice_accept_wait: Duration::from_millis(1_500),
            ..RelayConfig::default()
        })
        .await;
        let device = mesh_node().await;
        device.start();
        let registration = device.relay_register(relay).await.unwrap();
        let err = open_splice(relay, registration.id()).await.unwrap_err();
        assert!(
            matches!(err, RelayError::Refused(Refusal::Unreachable)),
            "{err}"
        );
        assert_eq!(core.pending_splices(), 0);
        let err = open_splice(relay, [7; 16]).await.unwrap_err();
        assert!(
            matches!(err, RelayError::Refused(Refusal::UnknownRegistration)),
            "{err}"
        );
        task.abort();
    }

    /// An `ACCEPT` naming no pending splice (guessed, replayed or already
    /// claimed) is refused and is never spliced to a waiting joiner.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_accept_for_no_pending_splice_is_refused() {
        let (relay, core, task) = live_relay(RelayConfig {
            splice_accept_wait: Duration::from_millis(1_500),
            ..RelayConfig::default()
        })
        .await;
        let device = mesh_node().await;
        device.start();
        // Registered but not accepting: the joiner waits on its offer.
        let registration = device.relay_register(relay).await.unwrap();
        let waiting = tokio::spawn(open_splice(relay, registration.id()));
        tokio::time::timeout(Duration::from_secs(2), async {
            while core.pending_splices() == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the joiner is waiting");
        let err = accept_splice(relay, [0xAB; 16]).await.unwrap_err();
        assert!(
            matches!(err, RelayError::Refused(Refusal::UnknownRegistration)),
            "{err}"
        );
        let joiner = waiting.await.unwrap().unwrap_err();
        assert!(
            matches!(joiner, RelayError::Refused(Refusal::Unreachable)),
            "the waiting joiner must not be spliced to a forged accept: {joiner}"
        );
        assert_eq!(core.stats().splices_opened.load(Ordering::Relaxed), 0);
        task.abort();
    }

    /// Each direction of a splice is cut at the byte cap.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_splice_is_cut_at_its_byte_cap() {
        let (relay, _core, task) = live_relay(RelayConfig {
            splice_max_bytes: 1_024,
            ..RelayConfig::default()
        })
        .await;
        let device = mesh_node().await;
        device.start();
        let mut registration = device.relay_register(relay).await.unwrap();
        let mut streams = registration.accept_splices(1);
        let mut joiner = open_splice(relay, registration.id()).await.unwrap();
        let mut at_device = streams.recv().await.unwrap();
        joiner.write_all(&[0x55; 4_096]).await.unwrap();
        let mut got = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), at_device.read_to_end(&mut got))
            .await
            .expect("the capped direction ends")
            .unwrap();
        assert_eq!(got.len(), 1_024);
        task.abort();
    }

    /// A routed handshake cancelled by its caller (a timeout before a
    /// fallback path) must not leave its pending entry behind: the next
    /// attempt to the same peer — here the relay fallback's shape, a fresh
    /// `connect_via` — succeeds instead of failing "already in flight".
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_cancelled_routed_handshake_does_not_block_the_next_attempt() {
        let device = mesh_node().await;
        let joiner = mesh_node().await;
        device.start();
        joiner.start();
        let dead = {
            let s = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
            s.local_addr().unwrap()
        };
        let cancelled = tokio::time::timeout(
            Duration::from_millis(300),
            joiner.connect_via(dead, device.public_key(), device.node_id()),
        )
        .await;
        assert!(cancelled.is_err(), "the dead path must still be pending");
        tokio::time::timeout(
            Duration::from_secs(5),
            joiner.connect_via(device.local_addr(), device.public_key(), device.node_id()),
        )
        .await
        .expect("no hang")
        .expect("the cancelled attempt must not block this one");
    }

    // ---- TCP tunnel: the last resort when UDP is blocked (V3 S8) ----

    /// A relay whose UDP is blocked (as a network that drops UDP would), its
    /// TCP listener serving as usual.
    async fn udp_blocked_relay() -> (SocketAddr, Arc<RelayCore>, tokio::task::JoinHandle<()>) {
        let relay = Arc::new(
            BlindRelay::bind("127.0.0.1:0".parse().unwrap(), RelayConfig::default())
                .await
                .unwrap(),
        );
        relay.block_udp_for_test(true);
        let addr = relay.local_addr().unwrap();
        let core = relay.core().clone();
        (addr, core, tokio::spawn(async move { relay.run().await }))
    }

    #[test]
    fn tunnel_endpoints_are_discard_prefix_and_distinct() {
        let a = tunnel_endpoint(0);
        let b = tunnel_endpoint(u64::MAX);
        assert!(is_tunnel_endpoint(&a) && is_tunnel_endpoint(&b));
        assert_ne!(a, b);
        for real in [
            "127.0.0.1:9000",
            "[::1]:9000",
            "[2001:db8::1]:443",
            "[::ffff:10.0.0.1]:1",
        ] {
            assert!(!is_tunnel_endpoint(&addr(real)), "{real}");
        }
    }

    /// UDP to the relay is blocked: registration and bind fall back to the
    /// TCP tunnel on the same port, and a real mesh session (routed handshake
    /// plus a sealed request/ack) runs end-to-end through it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_mesh_session_runs_through_the_tcp_tunnel_when_udp_is_blocked() {
        use crate::adapter::net::ChannelName;

        let (relay_addr, core, relay_task) = udp_blocked_relay().await;
        let device = mesh_node().await;
        let joiner = mesh_node().await;
        device.start();
        joiner.start();

        let registration = device
            .relay_register(relay_addr)
            .await
            .expect("registered over the tunnel");
        assert!(
            device.relay_tunneled(relay_addr),
            "the device fell back to TCP"
        );
        let via = joiner
            .relay_bind(relay_addr, registration.id())
            .await
            .expect("bound over the tunnel");
        assert!(
            joiner.relay_tunneled(relay_addr),
            "the joiner fell back to TCP"
        );
        joiner
            .connect_via_endpoint(via, device.public_key(), device.node_id())
            .await
            .expect("routed handshake through the tunnels");
        tokio::time::timeout(
            Duration::from_secs(5),
            joiner.subscribe_channel(device.node_id(), ChannelName::new("relay.tcp").unwrap()),
        )
        .await
        .expect("no hang")
        .expect("a sealed request and its ack cross both tunnels");
        assert_eq!(core.stats().tunnels_opened.load(Ordering::Relaxed), 2);
        assert!(core.stats().forwarded_packets.load(Ordering::Relaxed) >= 4);
        drop(registration);
        relay_task.abort();
    }

    /// With UDP answering, no tunnel is opened: TCP is the last resort.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn udp_is_used_when_it_answers() {
        let (relay_addr, core, relay_task) = live_relay(RelayConfig::default()).await;
        let device = mesh_node().await;
        let joiner = mesh_node().await;
        device.start();
        joiner.start();
        let registration = device.relay_register(relay_addr).await.expect("register");
        joiner
            .relay_bind(relay_addr, registration.id())
            .await
            .expect("bind");
        assert!(!device.relay_tunneled(relay_addr) && !joiner.relay_tunneled(relay_addr));
        assert_eq!(core.stats().tunnels_opened.load(Ordering::Relaxed), 0);
        drop(registration);
        relay_task.abort();
    }

    /// Enrollment splices reach a device registered over the tunnel: the
    /// relay's offer goes down the device's tunnel and the device dials back.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_byte_stream_is_spliced_to_a_device_registered_over_the_tunnel() {
        let (relay, core, task) = udp_blocked_relay().await;
        let device = mesh_node().await;
        device.start();
        let mut registration = device.relay_register(relay).await.unwrap();
        assert!(device.relay_tunneled(relay));
        let mut streams = registration.accept_splices(4);
        let mut joiner = open_splice(relay, registration.id())
            .await
            .expect("spliced");
        let mut at_device = tokio::time::timeout(Duration::from_secs(5), streams.recv())
            .await
            .expect("device got the stream")
            .unwrap();
        joiner.write_all(b"to device").await.unwrap();
        let mut buf = [0u8; 9];
        at_device.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"to device");
        assert_eq!(core.stats().splices_opened.load(Ordering::Relaxed), 1);
        task.abort();
    }
}
