# Blackstream L0 Transport Protocol (BLTP)

GPU-to-GPU encrypted streaming over UDP for Blackwell clusters.

```
Blackwell-A ◀━━━━ BLTP/UDP ━━━━▶ Blackwell-B
   │                                    │
   └── Blackstream Instance ────────────┘
```

**Target Performance:**
- 40-60M encrypted events/sec
- <10μs end-to-end latency
- Zero-copy, zero-allocation hot path
- 100GbE line-rate capable

---

## Design Principles

| Principle | Implementation |
|-----------|----------------|
| Zero-copy | `Bytes` passthrough, no intermediate buffers |
| Zero-allocation | Pre-allocated packet pools, no heap on hot path |
| Fully encrypted | XChaCha20-Poly1305 per packet |
| Loss-tolerant | Fire-and-forget default, optional per-stream reliability |
| Ordered-per-stream | 64-bit sequence numbers per stream |
| Stateless per packet | No connection state required for decryption |
| Connectionless | Noise handshake once, then pure datagrams |

---

## 1. Why Not QUIC/TLS/gRPC

| Protocol | Problem |
|----------|---------|
| TCP | Head-of-line blocking, kernel overhead, tail latency |
| TLS | Handshake overhead, connection state, not datagram-native |
| QUIC | HTTP semantics, flow control overhead, too heavy |
| gRPC | Protobuf overhead, HTTP/2, connection-oriented |
| Redis | Centralized, network hop, not P2P |

BLTP is inspired by:
- **Aeron** (LMAX exchange) - zero-copy UDP messaging
- **WireGuard** - Noise protocol + ChaCha20-Poly1305
- **NVIDIA NVLink** - GPU-to-GPU control messaging
- **HFT market data** - stateless UDP multicast

---

## 2. Encryption: XChaCha20-Poly1305

### Why XChaCha20-Poly1305

| Property | Benefit |
|----------|---------|
| 24-byte nonce | Nonce-misuse resistant, safe random generation |
| AEAD | Authenticated encryption, no separate MAC step |
| ~80ns per 1KB | CPU encryption cost is negligible |
| WireGuard proven | Battle-tested in production |
| No AES-NI required | Consistent performance across hardware |

```rust
use chacha20poly1305::{XChaCha20Poly1305, aead::{Aead, KeyInit}};

// Encryption: ~80ns for 1KB payload
let cipher = XChaCha20Poly1305::new(&key);
let ciphertext = cipher.encrypt(&nonce, payload)?;
```

### Key Hierarchy

```
PSK (Pre-Shared Key)
    │
    ├── Noise Handshake (NKpsk0 or XKpsk3)
    │       │
    │       └── Session Keys (derived via HKDF)
    │               │
    │               ├── tx_key (32 bytes)
    │               └── rx_key (32 bytes)
    │
    └── Optional: Key Rotation (every N packets or T seconds)
```

---

## 3. Key Exchange: Noise Protocol Framework

### Why Noise (not TLS)

| Feature | Noise | TLS 1.3 |
|---------|-------|---------|
| Handshake RTT | 1 | 1-2 |
| State after handshake | None needed | Connection state |
| Certificate chain | No | Yes (overhead) |
| Forward secrecy | Yes | Yes |
| Identity hiding | Optional | No |
| Implementation complexity | Low | High |

### Noise Pattern: NKpsk0

```
NKpsk0:
  <- s
  ...
  -> psk, e, es
  <- e, ee
```

- **N**: No static key for initiator (anonymous)
- **K**: Responder's static key is known
- **psk0**: Pre-shared key mixed at start

This gives:
- 1-RTT handshake
- Forward secrecy
- Authentication via PSK
- No certificates needed

### Handshake Implementation

```rust
use snow::{Builder, params::NoiseParams};

static NOISE_PATTERN: &str = "Noise_NKpsk0_25519_ChaChaPoly_BLAKE2s";

pub struct NoiseHandshake {
    builder: snow::HandshakeState,
}

impl NoiseHandshake {
    pub fn initiator(psk: &[u8; 32], responder_static: &[u8; 32]) -> Self {
        let builder = Builder::new(NOISE_PATTERN.parse().unwrap())
            .psk(0, psk)
            .remote_public_key(responder_static)
            .build_initiator()
            .unwrap();
        Self { builder }
    }
    
    pub fn responder(psk: &[u8; 32], static_keypair: &snow::Keypair) -> Self {
        let builder = Builder::new(NOISE_PATTERN.parse().unwrap())
            .psk(0, psk)
            .local_private_key(&static_keypair.private)
            .build_responder()
            .unwrap();
        Self { builder }
    }
    
    /// Returns (tx_cipher, rx_cipher) after handshake completion
    pub fn into_transport(self) -> (CipherState, CipherState) {
        let transport = self.builder.into_transport_mode().unwrap();
        // Extract symmetric keys for stateless operation
        transport.extract_ciphers()
    }
}
```

### Post-Handshake: Stateless Operation

After handshake completes:
- Extract symmetric keys
- Discard Noise state
- All subsequent packets are stateless
- No connection tracking required

---

## 4. Packet Format

### Wire Format (Fixed Header: 64 bytes)

```
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|     MAGIC (0x424C)    |  VER  |     FLAGS     |   Reserved    |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                                                               |
+                         NONCE (24 bytes)                      +
|                                                               |
|                                                               |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                                                               |
+                       SESSION_ID (8 bytes)                    +
|                                                               |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                                                               |
+                       STREAM_ID (8 bytes)                     +
|                                                               |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                                                               |
+                       SEQUENCE (8 bytes)                      +
|                                                               |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|        PAYLOAD_LEN (2 bytes)  |       EVENT_COUNT (2 bytes)   |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                                                               |
~                    ENCRYPTED_PAYLOAD (variable)               ~
|                                                               |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                                                               |
+                    POLY1305 AUTH TAG (16 bytes)               +
|                                                               |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

### Header Fields

```rust
/// BLTP packet header - 64 bytes, cache-line aligned
#[repr(C, align(64))]
pub struct BltpHeader {
    /// Magic: "BL" (0x424C)
    pub magic: u16,
    /// Protocol version (1)
    pub version: u8,
    /// Flags (reliability, priority, etc.)
    pub flags: PacketFlags,
    /// Reserved for future use
    pub reserved: u32,
    /// XChaCha20 nonce (24 bytes) - random per packet
    pub nonce: [u8; 24],
    /// Session identifier (from handshake)
    pub session_id: u64,
    /// Stream identifier (for multiplexing)
    pub stream_id: u64,
    /// Per-stream sequence number (monotonic)
    pub sequence: u64,
    /// Payload length (after encryption, before tag)
    pub payload_len: u16,
    /// Number of events in payload
    pub event_count: u16,
}

bitflags::bitflags! {
    #[repr(transparent)]
    pub struct PacketFlags: u8 {
        /// Packet requires acknowledgment
        const RELIABLE = 0b0000_0001;
        /// This is a NACK/retransmit request
        const NACK = 0b0000_0010;
        /// High priority (bypass normal queuing)
        const PRIORITY = 0b0000_0100;
        /// Final packet in batch
        const FIN = 0b0000_1000;
        /// Handshake packet
        const HANDSHAKE = 0b0001_0000;
        /// Heartbeat/keepalive
        const HEARTBEAT = 0b0010_0000;
    }
}
```

### Authenticated Data (AAD)

The header (excluding nonce) is authenticated but not encrypted:

```rust
impl BltpHeader {
    /// Get AAD for AEAD construction
    /// Authenticates: magic, version, flags, session_id, stream_id, sequence
    pub fn aad(&self) -> [u8; 32] {
        let mut aad = [0u8; 32];
        aad[0..2].copy_from_slice(&self.magic.to_le_bytes());
        aad[2] = self.version;
        aad[3] = self.flags.bits();
        aad[8..16].copy_from_slice(&self.session_id.to_le_bytes());
        aad[16..24].copy_from_slice(&self.stream_id.to_le_bytes());
        aad[24..32].copy_from_slice(&self.sequence.to_le_bytes());
        aad
    }
}
```

---

## 5. Reliability Modes

### Mode 1: Fire-and-Forget (Default)

```rust
pub struct FireAndForget;

impl ReliabilityMode for FireAndForget {
    fn on_send(&mut self, _seq: u64) {
        // Nothing to track
    }
    
    fn on_receive(&mut self, _seq: u64) -> bool {
        true // Always accept
    }
    
    fn needs_ack(&self) -> bool {
        false
    }
}
```

**Use cases:**
- LLM token streams (high frequency, loss tolerant)
- Embeddings
- Intermediate activations
- Metrics/telemetry

**Characteristics:**
- Zero overhead
- Maximum throughput
- Lowest latency
- No retransmission

### Mode 2: Reliable (Per-Stream)

```rust
pub struct ReliableStream {
    /// Highest contiguous sequence received
    ack_seq: u64,
    /// Bitmap of received sequences beyond ack_seq
    sack_bitmap: u64,
    /// Pending retransmits (bounded window)
    pending: ArrayVec<UnackedPacket, 32>,
    /// Retransmit timeout
    rto: Duration,
}

impl ReliabilityMode for ReliableStream {
    fn on_send(&mut self, seq: u64, packet: Bytes) {
        if self.pending.len() < 32 {
            self.pending.push(UnackedPacket {
                seq,
                packet,
                sent_at: Instant::now(),
                retries: 0,
            });
        }
    }
    
    fn on_receive(&mut self, seq: u64) -> bool {
        if seq == self.ack_seq + 1 {
            self.ack_seq = seq;
            // Advance through any buffered sequences
            while self.sack_bitmap & 1 != 0 {
                self.ack_seq += 1;
                self.sack_bitmap >>= 1;
            }
            true
        } else if seq > self.ack_seq && seq <= self.ack_seq + 64 {
            // Mark in SACK bitmap
            let offset = seq - self.ack_seq - 1;
            self.sack_bitmap |= 1 << offset;
            true
        } else {
            false // Duplicate or too far ahead
        }
    }
    
    fn build_nack(&self) -> Option<NackPayload> {
        if self.has_gaps() {
            Some(NackPayload {
                ack_seq: self.ack_seq,
                missing_bitmap: self.missing_bitmap(),
            })
        } else {
            None
        }
    }
}
```

**Use cases:**
- Tool call results
- Guardrail decisions
- Session lifecycle events
- Error propagation

**Characteristics:**
- Bounded retransmit window (32 packets)
- Selective NACKs (not ACKs - less traffic)
- Per-stream, not global
- Configurable RTO

---

## 6. Zero-Copy Architecture

### Event Frame Format (Inside Encrypted Payload)

```
+------------------+------------------+------------------+
| LEN (4B) | DATA  | LEN (4B) | DATA  | LEN (4B) | DATA  |
+------------------+------------------+------------------+
     Event 0            Event 1            Event 2
```

Events are concatenated with 4-byte length prefixes. No JSON parsing, no copying.

### Packet Construction (Zero-Allocation)

```rust
pub struct PacketBuilder {
    /// Pre-allocated header buffer (cache-line aligned)
    header: AlignedBuffer<64>,
    /// Pre-allocated payload buffer
    payload: BytesMut,
    /// Cipher instance (reused)
    cipher: XChaCha20Poly1305,
}

impl PacketBuilder {
    /// Build packet without allocation
    #[inline]
    pub fn build(
        &mut self,
        session_id: u64,
        stream_id: u64,
        sequence: u64,
        events: &[RawEvent],
        flags: PacketFlags,
    ) -> Bytes {
        // Reset payload buffer (no allocation)
        self.payload.clear();
        
        // Write event frames directly (zero-copy from RawEvent::bytes())
        for event in events {
            let bytes = event.as_bytes();
            self.payload.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            self.payload.extend_from_slice(bytes);
        }
        
        // Generate random nonce
        let nonce = rand::random::<[u8; 24]>();
        
        // Build header
        let header = BltpHeader {
            magic: MAGIC,
            version: 1,
            flags,
            reserved: 0,
            nonce,
            session_id,
            stream_id,
            sequence,
            payload_len: self.payload.len() as u16,
            event_count: events.len() as u16,
        };
        
        // Encrypt in-place
        let tag = self.cipher.encrypt_in_place_detached(
            &nonce.into(),
            &header.aad(),
            &mut self.payload,
        ).unwrap();
        
        // Combine: header + encrypted_payload + tag
        let mut packet = BytesMut::with_capacity(64 + self.payload.len() + 16);
        packet.extend_from_slice(header.as_bytes());
        packet.extend_from_slice(&self.payload);
        packet.extend_from_slice(&tag);
        
        packet.freeze()
    }
}
```

### Packet Pool (Amortized Allocation)

```rust
use crossbeam_queue::ArrayQueue;

pub struct PacketPool {
    builders: ArrayQueue<PacketBuilder>,
}

impl PacketPool {
    pub fn new(size: usize, key: &[u8; 32]) -> Self {
        let builders = ArrayQueue::new(size);
        for _ in 0..size {
            let _ = builders.push(PacketBuilder::new(key));
        }
        Self { builders }
    }
    
    #[inline]
    pub fn get(&self) -> PooledBuilder {
        let builder = self.builders.pop()
            .unwrap_or_else(|| PacketBuilder::new(&self.key));
        PooledBuilder { pool: self, builder: Some(builder) }
    }
}

pub struct PooledBuilder<'a> {
    pool: &'a PacketPool,
    builder: Option<PacketBuilder>,
}

impl Drop for PooledBuilder<'_> {
    fn drop(&mut self) {
        if let Some(builder) = self.builder.take() {
            let _ = self.pool.builders.push(builder);
        }
    }
}
```

---

## 7. Transport Layer

### Socket Configuration

```rust
use socket2::{Socket, Domain, Type, Protocol, SockRef};

pub struct BltpSocket {
    socket: std::net::UdpSocket,
}

impl BltpSocket {
    pub fn new(bind_addr: SocketAddr) -> io::Result<Self> {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        
        // Maximize buffer sizes for burst handling
        socket.set_recv_buffer_size(64 * 1024 * 1024)?; // 64MB
        socket.set_send_buffer_size(64 * 1024 * 1024)?; // 64MB
        
        // Disable fragmentation (we fit in MTU)
        #[cfg(target_os = "linux")]
        socket.set_mtu_discover(socket2::MtuDiscover::Do)?;
        
        // Enable timestamps for latency measurement
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::io::AsRawFd;
            let fd = socket.as_raw_fd();
            // SO_TIMESTAMPNS for nanosecond precision
            unsafe {
                libc::setsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_TIMESTAMPNS,
                    &1i32 as *const _ as *const libc::c_void,
                    std::mem::size_of::<i32>() as libc::socklen_t,
                );
            }
        }
        
        socket.set_nonblocking(true)?;
        socket.bind(&bind_addr.into())?;
        
        Ok(Self { socket: socket.into() })
    }
}
```

### Batched I/O (Linux io_uring / sendmmsg)

```rust
#[cfg(target_os = "linux")]
pub mod linux {
    use io_uring::{IoUring, opcode, types};
    
    pub struct BatchedTransport {
        ring: IoUring,
        socket_fd: RawFd,
        /// Pre-allocated iovec array
        iovecs: Vec<libc::iovec>,
        /// Pre-allocated msghdr array
        msgs: Vec<libc::mmsghdr>,
    }
    
    impl BatchedTransport {
        /// Send up to 64 packets in a single syscall
        pub fn send_batch(&mut self, packets: &[Bytes], addr: SocketAddr) -> io::Result<usize> {
            // Use sendmmsg for batch sending
            for (i, packet) in packets.iter().take(64).enumerate() {
                self.iovecs[i] = libc::iovec {
                    iov_base: packet.as_ptr() as *mut _,
                    iov_len: packet.len(),
                };
                // ... setup msghdr
            }
            
            let sent = unsafe {
                libc::sendmmsg(
                    self.socket_fd,
                    self.msgs.as_mut_ptr(),
                    packets.len().min(64) as u32,
                    0,
                )
            };
            
            Ok(sent as usize)
        }
        
        /// Receive up to 64 packets in a single syscall
        pub fn recv_batch(&mut self, buffers: &mut [BytesMut]) -> io::Result<Vec<(usize, SocketAddr)>> {
            // Use recvmmsg for batch receiving
            // ...
        }
    }
}
```

### Kernel Bypass (Optional: DPDK/AF_XDP)

For true 100GbE line-rate, kernel bypass is required:

```rust
#[cfg(feature = "dpdk")]
pub mod dpdk {
    // DPDK-based transport for kernel bypass
    // Achieves 100M+ pps on commodity hardware
}

#[cfg(feature = "af_xdp")]
pub mod af_xdp {
    // AF_XDP for kernel bypass without full DPDK
    // Good for 25-50GbE
}
```

---

## 8. Session Management

### Session State

```rust
pub struct BltpSession {
    /// Session ID (from handshake)
    session_id: u64,
    /// Remote peer address
    peer_addr: SocketAddr,
    /// Encryption key for sending
    tx_key: [u8; 32],
    /// Encryption key for receiving  
    rx_key: [u8; 32],
    /// Per-stream state
    streams: DashMap<u64, StreamState>,
    /// Last activity (for timeout)
    last_activity: AtomicU64,
    /// Packet pool
    packet_pool: PacketPool,
}

pub struct StreamState {
    /// Next sequence to send
    tx_seq: AtomicU64,
    /// Reliability mode for this stream
    reliability: Box<dyn ReliabilityMode>,
    /// Inbound event buffer (for ordered delivery)
    inbound: SegQueue<StoredEvent>,
}
```

### Handshake Flow

```
Initiator                              Responder
    │                                      │
    │──── HandshakeInit ──────────────────▶│
    │     [noise_message_1]                │
    │     [initiator_ephemeral]            │
    │                                      │
    │◀─── HandshakeResp ───────────────────│
    │     [noise_message_2]                │
    │     [responder_ephemeral]            │
    │     [encrypted: session_id]          │
    │                                      │
    │     (both derive tx_key, rx_key)     │
    │     (discard noise state)            │
    │                                      │
    │◀═══════════ BLTP Data ═══════════════│
    │════════════ BLTP Data ══════════════▶│
```

---

## 9. Adapter Implementation

### Configuration

```rust
#[derive(Debug, Clone)]
pub struct BltpAdapterConfig {
    /// Local bind address
    pub bind_addr: SocketAddr,
    /// Remote peer address
    pub peer_addr: SocketAddr,
    /// Pre-shared key (32 bytes)
    pub psk: [u8; 32],
    /// Our static keypair (for Noise)
    pub static_keypair: Option<snow::Keypair>,
    /// Peer's static public key (required for initiator)
    pub peer_static_pubkey: Option<[u8; 32]>,
    /// Default reliability mode for new streams
    pub default_reliability: ReliabilityConfig,
    /// Packet pool size
    pub packet_pool_size: usize,
    /// Heartbeat interval
    pub heartbeat_interval: Duration,
    /// Session timeout
    pub session_timeout: Duration,
    /// Enable io_uring / sendmmsg batching
    pub batched_io: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum ReliabilityConfig {
    /// Fire-and-forget (no reliability)
    None,
    /// Lightweight reliability (32-packet window, selective NACK)
    Light,
    /// Full reliability (unbounded retransmit, ordered delivery)
    Full,
}
```

### Adapter Struct

```rust
pub struct BltpAdapter {
    config: BltpAdapterConfig,
    socket: Arc<BltpSocket>,
    session: RwLock<Option<BltpSession>>,
    /// Inbound events per shard (for poll_shard)
    inbound: DashMap<u16, SegQueue<StoredEvent>>,
    /// Background tasks
    tasks: Mutex<Vec<JoinHandle<()>>>,
    /// Shutdown signal
    shutdown: AtomicBool,
    initialized: AtomicBool,
}
```

### Adapter Trait Implementation

```rust
#[async_trait]
impl Adapter for BltpAdapter {
    async fn init(&mut self) -> Result<(), AdapterError> {
        // 1. Create socket
        let socket = BltpSocket::new(self.config.bind_addr)?;
        self.socket = Arc::new(socket);
        
        // 2. Perform Noise handshake
        let session = self.perform_handshake().await?;
        *self.session.write() = Some(session);
        
        // 3. Spawn receiver task
        self.spawn_receiver();
        
        // 4. Spawn heartbeat task
        self.spawn_heartbeat();
        
        self.initialized.store(true, Ordering::Release);
        Ok(())
    }
    
    async fn on_batch(&self, batch: Batch) -> Result<(), AdapterError> {
        let session = self.session.read();
        let session = session.as_ref()
            .ok_or_else(|| AdapterError::Connection("not connected".into()))?;
        
        // Get or create stream state
        let stream_id = batch.shard_id as u64;
        let stream = session.streams.entry(stream_id)
            .or_insert_with(|| StreamState::new(self.config.default_reliability));
        
        // Get packet builder from pool
        let mut builder = session.packet_pool.get();
        
        // Group events into MTU-sized packets
        let mut events_batch = Vec::with_capacity(batch.events.len());
        let mut current_size = 64 + 16; // header + tag
        
        for event in &batch.events {
            let frame_size = 4 + event.raw.len();
            
            if current_size + frame_size > MAX_PACKET_SIZE && !events_batch.is_empty() {
                // Send current packet
                let seq = stream.tx_seq.fetch_add(1, Ordering::Relaxed);
                let raw_events: Vec<_> = events_batch.iter()
                    .map(|e: &&InternalEvent| RawEvent::from_bytes(e.raw.clone()))
                    .collect();
                
                let flags = match &*stream.reliability {
                    r if r.needs_ack() => PacketFlags::RELIABLE,
                    _ => PacketFlags::empty(),
                };
                
                let packet = builder.build(
                    session.session_id,
                    stream_id,
                    seq,
                    &raw_events.iter().collect::<Vec<_>>(),
                    flags,
                );
                
                self.socket.send_to(&packet, self.config.peer_addr)?;
                
                if stream.reliability.needs_ack() {
                    stream.reliability.on_send(seq, packet);
                }
                
                events_batch.clear();
                current_size = 64 + 16;
            }
            
            events_batch.push(event);
            current_size += frame_size;
        }
        
        // Send remaining
        if !events_batch.is_empty() {
            let seq = stream.tx_seq.fetch_add(1, Ordering::Relaxed);
            let raw_events: Vec<_> = events_batch.iter()
                .map(|e| RawEvent::from_bytes(e.raw.clone()))
                .collect();
            
            let packet = builder.build(
                session.session_id,
                stream_id,
                seq,
                &raw_events.iter().collect::<Vec<_>>(),
                PacketFlags::empty(),
            );
            
            self.socket.send_to(&packet, self.config.peer_addr)?;
        }
        
        Ok(())
    }
    
    async fn poll_shard(
        &self,
        shard_id: u16,
        from_id: Option<&str>,
        limit: usize,
    ) -> Result<ShardPollResult, AdapterError> {
        let from_seq: u64 = from_id.and_then(|s| s.parse().ok()).unwrap_or(0);
        
        let mut events = Vec::with_capacity(limit);
        
        if let Some(queue) = self.inbound.get(&shard_id) {
            while events.len() < limit {
                if let Some(event) = queue.pop() {
                    if event.insertion_ts > from_seq {
                        events.push(event);
                    }
                } else {
                    break;
                }
            }
        }
        
        let has_more = self.inbound.get(&shard_id)
            .map(|q| !q.is_empty())
            .unwrap_or(false);
        let next_id = events.last().map(|e| e.insertion_ts.to_string());
        
        Ok(ShardPollResult { events, next_id, has_more })
    }
    
    async fn flush(&self) -> Result<(), AdapterError> {
        // For reliable streams, wait for all ACKs
        if let Some(session) = self.session.read().as_ref() {
            for stream in session.streams.iter() {
                stream.reliability.wait_for_acks().await?;
            }
        }
        Ok(())
    }
    
    async fn shutdown(&self) -> Result<(), AdapterError> {
        self.shutdown.store(true, Ordering::Release);
        
        // Drain tasks
        let tasks: Vec<_> = std::mem::take(&mut *self.tasks.lock());
        for task in tasks {
            let _ = task.await;
        }
        
        self.initialized.store(false, Ordering::Release);
        Ok(())
    }
    
    fn name(&self) -> &'static str {
        "bltp"
    }
    
    async fn is_healthy(&self) -> bool {
        self.initialized.load(Ordering::Acquire)
            && self.session.read().is_some()
    }
}
```

---

## 10. Performance Targets

| Metric | Target | Notes |
|--------|--------|-------|
| Throughput (25GbE) | 10-15M events/sec | Kernel network stack |
| Throughput (100GbE) | 40-60M events/sec | io_uring batching |
| Throughput (100GbE + DPDK) | 100M+ events/sec | Kernel bypass |
| Latency (p50) | <5μs | Same datacenter |
| Latency (p99) | <10μs | Same datacenter |
| Encryption overhead | <100ns/packet | XChaCha20-Poly1305 |
| Memory allocation | 0 per packet | Pool-based |
| CPU overhead | <5% per core | At 10M events/sec |

---

## 11. File Structure

```
src/adapter/
├── mod.rs
├── noop.rs
├── redis.rs
├── jetstream.rs
└── bltp/
    ├── mod.rs          # BltpAdapter
    ├── config.rs       # BltpAdapterConfig
    ├── protocol.rs     # BltpHeader, PacketFlags
    ├── crypto.rs       # Noise handshake, XChaCha20
    ├── reliability.rs  # FireAndForget, ReliableStream
    ├── transport.rs    # BltpSocket, batched I/O
    ├── session.rs      # BltpSession, StreamState
    ├── pool.rs         # PacketPool, PacketBuilder
    └── linux.rs        # io_uring, sendmmsg, AF_XDP
```

---

## 12. Dependencies

```toml
[features]
bltp = [
    "dep:chacha20poly1305",
    "dep:snow",
    "dep:rand",
    "dep:bitflags",
    "dep:crossbeam-queue",
    "dep:socket2",
]
bltp-uring = ["bltp", "dep:io-uring"]
bltp-dpdk = ["bltp"]  # Requires external DPDK installation

[dependencies]
# Encryption
chacha20poly1305 = { version = "0.10", features = ["std"], optional = true }
snow = { version = "0.9", optional = true }

# Utilities
rand = { version = "0.8", optional = true }
bitflags = { version = "2", optional = true }
crossbeam-queue = { version = "0.3", optional = true }
socket2 = { version = "0.5", optional = true }

# Linux-specific
io-uring = { version = "0.6", optional = true }
```

---

## 13. Implementation Order

1. **`protocol.rs`** - BltpHeader, PacketFlags, wire format
2. **`crypto.rs`** - Noise handshake, key derivation, XChaCha20
3. **`pool.rs`** - PacketPool, PacketBuilder, zero-allocation
4. **`reliability.rs`** - FireAndForget, ReliableStream, NACK
5. **`transport.rs`** - BltpSocket, basic send/recv
6. **`session.rs`** - BltpSession, StreamState
7. **`config.rs`** - BltpAdapterConfig
8. **`mod.rs`** - BltpAdapter implementation
9. **`linux.rs`** - sendmmsg/recvmmsg batching
10. **Integration** - Add to config.rs, bus.rs
11. **Tests** - Unit, integration, benchmarks
12. **Optional: io_uring** - For higher throughput
13. **Optional: DPDK/AF_XDP** - For 100GbE line-rate

---

## 14. Security Model

| Threat | Mitigation |
|--------|------------|
| Eavesdropping | XChaCha20-Poly1305 encryption |
| Tampering | Poly1305 authentication tag |
| Replay | Per-stream sequence + sliding window |
| MITM | Noise handshake + PSK authentication |
| Key compromise | Forward secrecy via ephemeral keys |
| Nonce reuse | 24-byte random nonce (192 bits) |
| Amplification | Session ID validation |

---

## 15. Comparison

| Feature | BLTP | QUIC | TCP+TLS | gRPC |
|---------|------|------|---------|------|
| Latency | <10μs | ~100μs | ~50μs | ~500μs |
| Throughput | 40-60M/s | 1-2M/s | 2-5M/s | 100K/s |
| Zero-copy | Yes | No | No | No |
| Connectionless | Yes | No | No | No |
| Encryption | XChaCha20 | AES-GCM | AES-GCM | AES-GCM |
| Reliability | Optional | Required | Required | Required |
| Complexity | Low | High | Medium | High |

---

This is the transport layer that GPU clusters need.
This is what makes Blackstream a true HPC messaging fabric.
