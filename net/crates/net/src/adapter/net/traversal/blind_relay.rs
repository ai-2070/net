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

use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use tokio::net::UdpSocket;

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
    /// Either side: channel data.
    pub const DATA: u8 = 0x10;
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
/// `DATA` header length: kind + channel.
pub const DATA_HEADER_LEN: usize = 1 + 4;
const ENDPOINT_LEN: usize = 16 + 2;

/// Registration identifier: derived from the registering entity id.
pub type RegistrationId = [u8; 16];

/// Derive the registration id an entity registers under.
pub fn registration_id(entity: &EntityId) -> RegistrationId {
    let full = blake3::derive_key(REGISTRATION_ID_CONTEXT, entity.as_bytes());
    let mut id = [0u8; 16];
    id.copy_from_slice(&full[..16]);
    id
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
}

impl Refusal {
    fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            1 => Self::BadProof,
            2 => Self::UnknownRegistration,
            3 => Self::Capacity,
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
}

struct Registration {
    entity: EntityId,
    endpoint: SocketAddr,
    expires: Instant,
    channels: Vec<u32>,
    window_start: Instant,
    binds_in_window: u32,
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

    /// Expire registrations (and their channels) and idle channels.
    pub fn sweep(&self, now: Instant) {
        let mut s = self.state.lock();
        let State {
            registrations,
            channels,
        } = &mut *s;
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

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A running blind relay on one UDP socket.
pub struct BlindRelay {
    socket: Arc<UdpSocket>,
    core: Arc<RelayCore>,
}

impl BlindRelay {
    /// Bind the relay socket.
    pub async fn bind(addr: SocketAddr, config: RelayConfig) -> std::io::Result<Self> {
        Ok(Self {
            socket: Arc::new(UdpSocket::bind(addr).await?),
            core: Arc::new(RelayCore::new(config)?),
        })
    }

    /// Bound address.
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    /// Shared state machine (stats, sizes).
    pub fn core(&self) -> &Arc<RelayCore> {
        &self.core
    }

    /// Serve until the task is dropped/aborted.
    pub async fn run(&self) {
        let mut buf = vec![0u8; 65_535];
        let mut last_sweep = Instant::now();
        loop {
            let recv =
                tokio::time::timeout(Duration::from_secs(5), self.socket.recv_from(&mut buf)).await;
            let now = Instant::now();
            if now.duration_since(last_sweep) >= Duration::from_secs(5) {
                self.core.sweep(now);
                last_sweep = now;
            }
            let Ok(Ok((n, from))) = recv else {
                continue;
            };
            if let Action::Send { to, bytes } = self.core.handle(from, &buf[..n], unix_now(), now) {
                let _ = self.socket.send_to(&bytes, to).await;
            }
        }
    }
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

/// A mesh node's handle on one relay. Requests go out on the node's own mesh
/// socket (so a device's registration holds the same NAT mapping its relayed
/// traffic uses); the node's receive loop hands relay control replies to
/// [`Self::deliver`]. Requests are serialized per relay.
pub struct RelayClient {
    relay: SocketAddr,
    socket: Arc<crate::adapter::net::transport::NetSocket>,
    replies_tx: tokio::sync::mpsc::Sender<Message>,
    replies: tokio::sync::Mutex<tokio::sync::mpsc::Receiver<Message>>,
}

impl RelayClient {
    /// A client for `relay` sending on `socket`.
    pub fn new(relay: SocketAddr, socket: Arc<crate::adapter::net::transport::NetSocket>) -> Self {
        let (replies_tx, replies) = tokio::sync::mpsc::channel(32);
        Self {
            relay,
            socket,
            replies_tx,
            replies: tokio::sync::Mutex::new(replies),
        }
    }

    /// The relay's UDP tuple.
    pub fn relay(&self) -> SocketAddr {
        self.relay
    }

    /// Hand a control datagram received from the relay to a waiting request.
    /// Data frames are not control and are ignored here.
    pub fn deliver(&self, bytes: &[u8]) {
        if let Some(message) = Message::decode(bytes) {
            if !matches!(message, Message::Data { .. }) {
                let _ = self.replies_tx.try_send(message);
            }
        }
    }

    async fn exchange<R>(
        &self,
        request: &Message,
        accept: impl Fn(&Message) -> Option<R>,
    ) -> Result<R, RelayError> {
        let mut replies = self.replies.lock().await;
        while replies.try_recv().is_ok() {}
        let bytes = request.encode();
        for _ in 0..RELAY_ATTEMPTS {
            self.socket.send_to(&bytes, self.relay).await?;
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

/// A live registration with a relay. Dropping it stops the refresh; the
/// relay forgets the registration after its TTL.
pub struct RelayRegistration {
    relay: SocketAddr,
    id: RegistrationId,
    refresh: tokio::task::JoinHandle<()>,
}

impl RelayRegistration {
    /// Wrap a registration and the task refreshing it.
    pub fn new(
        relay: SocketAddr,
        id: RegistrationId,
        refresh: tokio::task::JoinHandle<()>,
    ) -> Self {
        Self { relay, id, refresh }
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
}
