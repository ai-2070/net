//! UDP transport layer for Net.
//!
//! This module provides the socket abstraction with optimized settings
//! for high-throughput UDP communication.

use bytes::{Bytes, BytesMut};
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;

use super::protocol::{NetHeader, HEADER_SIZE, MAX_PACKET_SIZE};

/// Default receive buffer size (64 MB)
pub const DEFAULT_RECV_BUFFER_SIZE: usize = 64 * 1024 * 1024;

/// Default send buffer size (64 MB)
pub const DEFAULT_SEND_BUFFER_SIZE: usize = 64 * 1024 * 1024;

/// Socket buffer configuration
#[derive(Debug, Clone, Copy)]
pub struct SocketBufferConfig {
    /// Receive buffer size in bytes
    pub recv_buffer_size: usize,
    /// Send buffer size in bytes
    pub send_buffer_size: usize,
}

impl Default for SocketBufferConfig {
    fn default() -> Self {
        Self {
            recv_buffer_size: DEFAULT_RECV_BUFFER_SIZE,
            send_buffer_size: DEFAULT_SEND_BUFFER_SIZE,
        }
    }
}

impl SocketBufferConfig {
    /// Configuration for tests with smaller buffers
    pub fn for_testing() -> Self {
        Self {
            recv_buffer_size: 256 * 1024, // 256 KB
            send_buffer_size: 256 * 1024, // 256 KB
        }
    }
}

/// Net socket wrapper with optimized settings.
pub struct NetSocket {
    /// Underlying UDP socket
    socket: Arc<UdpSocket>,
    /// Local address
    local_addr: SocketAddr,
    /// Receive buffer
    recv_buf: BytesMut,
}

impl NetSocket {
    /// Create a new Net socket bound to the given address with default (production) buffer sizes.
    pub async fn new(bind_addr: SocketAddr) -> io::Result<Self> {
        Self::with_config(bind_addr, SocketBufferConfig::default()).await
    }

    /// Create a new Net socket with custom buffer configuration.
    pub async fn with_config(
        bind_addr: SocketAddr,
        config: SocketBufferConfig,
    ) -> io::Result<Self> {
        // Create socket with socket2 for advanced options
        let socket2 = socket2::Socket::new(
            if bind_addr.is_ipv4() {
                socket2::Domain::IPV4
            } else {
                socket2::Domain::IPV6
            },
            socket2::Type::DGRAM,
            Some(socket2::Protocol::UDP),
        )?;

        // Set buffer sizes
        socket2.set_recv_buffer_size(config.recv_buffer_size)?;
        socket2.set_send_buffer_size(config.send_buffer_size)?;

        // Enable address reuse
        socket2.set_reuse_address(true)?;

        // Set non-blocking for tokio
        socket2.set_nonblocking(true)?;

        // Bind
        socket2.bind(&bind_addr.into())?;

        // Convert to tokio UdpSocket
        let std_socket: std::net::UdpSocket = socket2.into();
        let socket = UdpSocket::from_std(std_socket)?;
        let local_addr = socket.local_addr()?;

        Ok(Self {
            socket: Arc::new(socket),
            local_addr,
            recv_buf: BytesMut::with_capacity(MAX_PACKET_SIZE),
        })
    }

    /// Create from an existing tokio UdpSocket
    pub fn from_socket(socket: UdpSocket) -> io::Result<Self> {
        let local_addr = socket.local_addr()?;
        Ok(Self {
            socket: Arc::new(socket),
            local_addr,
            recv_buf: BytesMut::with_capacity(MAX_PACKET_SIZE),
        })
    }

    /// Get the local address
    #[inline]
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Get a reference to the underlying socket
    #[inline]
    pub fn socket(&self) -> &UdpSocket {
        &self.socket
    }

    /// Get a clone of the Arc socket
    #[inline]
    pub fn socket_arc(&self) -> Arc<UdpSocket> {
        self.socket.clone()
    }

    /// Connect to a remote address (for send/recv without address)
    pub async fn connect(&self, addr: SocketAddr) -> io::Result<()> {
        self.socket.connect(addr).await
    }

    /// Send a packet to a specific address
    #[inline]
    pub async fn send_to(&self, packet: &[u8], target: SocketAddr) -> io::Result<usize> {
        self.socket.send_to(packet, target).await
    }

    /// Send a packet to the connected address
    #[inline]
    pub async fn send(&self, packet: &[u8]) -> io::Result<usize> {
        self.socket.send(packet).await
    }

    /// Receive a packet, returning the data and source address
    pub async fn recv_from(&mut self) -> io::Result<(Bytes, SocketAddr)> {
        self.recv_buf.resize(MAX_PACKET_SIZE, 0);
        let (len, addr) = self.socket.recv_from(&mut self.recv_buf).await?;
        self.recv_buf.truncate(len);

        Ok((self.recv_buf.split().freeze(), addr))
    }

    /// Receive a packet from the connected address
    pub async fn recv(&mut self) -> io::Result<Bytes> {
        self.recv_buf.resize(MAX_PACKET_SIZE, 0);
        let len = self.socket.recv(&mut self.recv_buf).await?;
        self.recv_buf.truncate(len);

        Ok(self.recv_buf.split().freeze())
    }

    /// Try to receive a packet without blocking
    pub fn try_recv_from(&mut self) -> io::Result<Option<(Bytes, SocketAddr)>> {
        self.recv_buf.resize(MAX_PACKET_SIZE, 0);
        match self.socket.try_recv_from(&mut self.recv_buf) {
            Ok((len, addr)) => {
                self.recv_buf.truncate(len);
                Ok(Some((self.recv_buf.split().freeze(), addr)))
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(e),
        }
    }
}

impl std::fmt::Debug for NetSocket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetSocket")
            .field("local_addr", &self.local_addr)
            .finish()
    }
}

/// Parsed packet for processing
#[derive(Debug)]
pub struct ParsedPacket {
    /// Packet header
    pub header: NetHeader,
    /// Encrypted payload (includes auth tag)
    pub payload: Bytes,
    /// Source address
    pub source: SocketAddr,
}

impl ParsedPacket {
    /// Parse a raw packet
    pub fn parse(data: Bytes, source: SocketAddr) -> Option<Self> {
        if data.len() < HEADER_SIZE {
            return None;
        }

        let header = NetHeader::from_bytes(&data)?;
        if !header.validate() {
            return None;
        }

        let payload = data.slice(HEADER_SIZE..);

        Some(Self {
            header,
            payload,
            source,
        })
    }

    /// Get the expected payload length (ciphertext + tag)
    pub fn expected_payload_len(&self) -> usize {
        self.header.payload_len as usize + super::protocol::TAG_SIZE
    }

    /// Validate payload length
    pub fn is_valid_length(&self) -> bool {
        self.payload.len() == self.expected_payload_len()
    }
}

/// Receiver task for handling inbound packets
pub struct PacketReceiver {
    socket: Arc<UdpSocket>,
    recv_buf: BytesMut,
}

impl PacketReceiver {
    /// Create a new receiver from a shared socket
    pub fn new(socket: Arc<UdpSocket>) -> Self {
        Self {
            socket,
            recv_buf: BytesMut::with_capacity(MAX_PACKET_SIZE),
        }
    }

    /// Receive the next packet
    pub async fn recv(&mut self) -> io::Result<(Bytes, SocketAddr)> {
        self.recv_buf.resize(MAX_PACKET_SIZE, 0);
        let (len, addr) = self.socket.recv_from(&mut self.recv_buf).await?;
        self.recv_buf.truncate(len);

        Ok((self.recv_buf.split().freeze(), addr))
    }

    /// Parse the next packet
    pub async fn recv_parsed(&mut self) -> io::Result<Option<ParsedPacket>> {
        let (data, addr) = self.recv().await?;
        Ok(ParsedPacket::parse(data, addr))
    }
}

impl std::fmt::Debug for PacketReceiver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PacketReceiver").finish()
    }
}

/// Batched packet receiver using recvmmsg on Linux.
///
/// The underlying `BatchedTransport` contains `!Send` raw pointers, so it
/// cannot live inside a `tokio::spawn` future. Instead, a dedicated OS thread
/// owns the transport and sends received packets over a bounded channel.
#[cfg(target_os = "linux")]
pub struct BatchedPacketReceiver {
    rx: tokio::sync::mpsc::Receiver<(Bytes, SocketAddr)>,
    shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
    _socket: Arc<UdpSocket>,
    _thread: Option<std::thread::JoinHandle<()>>,
}

#[cfg(target_os = "linux")]
impl BatchedPacketReceiver {
    /// Create a new batched receiver from a shared socket.
    ///
    /// Spawns a dedicated OS thread that owns `BatchedTransport` and sends
    /// received packets over a bounded channel.
    pub fn new(socket: Arc<UdpSocket>) -> Self {
        use std::os::unix::io::AsRawFd;
        use std::sync::atomic::Ordering;

        let fd = socket.as_raw_fd();

        // Apply high-throughput socket tuning
        let _ = super::linux::configure_socket_for_throughput(fd);

        let (tx, rx) = tokio::sync::mpsc::channel(1024);
        let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let thread_shutdown = shutdown.clone();

        let thread = std::thread::Builder::new()
            .name("net-batch-recv".into())
            .spawn(move || {
                // Do NOT set `SO_RCVTIMEO` on the shared
                // `Arc<UdpSocket>`. The previous code installed a
                // 500ms timeout to give the receive thread a chance
                // to check `shutdown`, but `SO_RCVTIMEO` is a
                // *socket-level* option — every other consumer of
                // the same `Arc<UdpSocket>` (the sync handshake recv
                // at `mod.rs:396` and `mesh.rs:6098-6101`) inherited
                // that timeout.
                //
                // Use `recv_batch` instead, which passes
                // `MSG_DONTWAIT` per-call. That flag is per-syscall,
                // not per-fd, so no other consumer is affected. The
                // existing fallback path (non-blocking + 1ms sleep
                // when empty) is the design that keeps the shared
                // socket clean. The `SO_RCVTIMEO` path is gone.
                let mut transport = super::linux::BatchedTransport::new(fd);

                // Backoff state for the soft-error path. Pre-fix
                // the thread spun at 1ms forever on persistent
                // errors (bad fd, permission revoke), wasting
                // CPU and producing a wall of repeated `warn!`
                // entries. We now exponentially back off up to
                // 100ms, reset on every success, and bail on
                // hard errors (EBADF, ENOTSOCK) via
                // `raw_os_error` so an unrecoverable socket
                // doesn't silently consume a thread.
                const HARD_ERR_EBADF: i32 = libc::EBADF;
                const HARD_ERR_ENOTSOCK: i32 = libc::ENOTSOCK;
                let mut backoff_ms: u64 = 1;
                const MAX_BACKOFF_MS: u64 = 100;

                while !thread_shutdown.load(Ordering::Acquire) {
                    let result = transport.recv_batch(super::linux::MAX_BATCH_SIZE);

                    match result {
                        Ok(packets) => {
                            backoff_ms = 1;
                            if packets.is_empty() {
                                // No data available right now — yield
                                // for 1ms and re-check shutdown. The
                                // sleep is short enough that shutdown
                                // is observed promptly.
                                std::thread::sleep(std::time::Duration::from_millis(1));
                                continue;
                            }
                            for packet in packets {
                                if tx.blocking_send(packet).is_err() {
                                    return;
                                }
                            }
                        }
                        Err(e) => {
                            if thread_shutdown.load(Ordering::Acquire) {
                                return;
                            }
                            if e.kind() == io::ErrorKind::WouldBlock
                                || e.kind() == io::ErrorKind::Interrupted
                            {
                                std::thread::sleep(std::time::Duration::from_millis(1));
                                continue;
                            }
                            // Hard errors: socket fd is gone (closed
                            // out from under us, or never was a
                            // socket). Looping won't recover; the
                            // adapter has to be torn down. Surface
                            // a loud `error!` and exit so the
                            // channel receiver sees `None` and
                            // shutdown propagates.
                            if let Some(raw) = e.raw_os_error() {
                                if raw == HARD_ERR_EBADF || raw == HARD_ERR_ENOTSOCK {
                                    tracing::error!(
                                        error = %e,
                                        raw_os_error = raw,
                                        "batched receive: unrecoverable socket error \
                                         (EBADF/ENOTSOCK), exiting receive thread",
                                    );
                                    return;
                                }
                            }
                            // Transient soft error — exponential
                            // backoff up to MAX_BACKOFF_MS. Pre-fix
                            // every loop slept exactly 1ms; under a
                            // sustained soft error that produced a
                            // 1000 Hz `warn!` storm. Backoff caps
                            // the storm at ~10 Hz at steady state.
                            tracing::warn!(error = %e, backoff_ms, "batched receive error");
                            std::thread::sleep(std::time::Duration::from_millis(backoff_ms));
                            backoff_ms = (backoff_ms * 2).min(MAX_BACKOFF_MS);
                        }
                    }
                }
            })
            .expect("failed to spawn batch receiver thread");

        Self {
            rx,
            shutdown,
            _socket: socket,
            _thread: Some(thread),
        }
    }

    /// Receive the next packet.
    pub async fn recv(&mut self) -> io::Result<(Bytes, SocketAddr)> {
        self.rx
            .recv()
            .await
            .ok_or_else(|| io::Error::new(io::ErrorKind::ConnectionReset, "batch receiver closed"))
    }
}

#[cfg(target_os = "linux")]
impl Drop for BatchedPacketReceiver {
    fn drop(&mut self) {
        self.shutdown
            .store(true, std::sync::atomic::Ordering::Release);
        if let Some(thread) = self._thread.take() {
            let _ = thread.join();
        }
    }
}

/// Sender for transmitting packets
#[derive(Clone)]
pub struct PacketSender {
    socket: Arc<UdpSocket>,
}

impl PacketSender {
    /// Create a new sender from a shared socket
    pub fn new(socket: Arc<UdpSocket>) -> Self {
        Self { socket }
    }

    /// Send a packet to the specified address
    #[inline]
    pub async fn send_to(&self, packet: &[u8], target: SocketAddr) -> io::Result<usize> {
        self.socket.send_to(packet, target).await
    }

    /// Send a packet to the connected address
    #[inline]
    pub async fn send(&self, packet: &[u8]) -> io::Result<usize> {
        self.socket.send(packet).await
    }

    /// Try to send without blocking
    #[inline]
    pub fn try_send_to(&self, packet: &[u8], target: SocketAddr) -> io::Result<usize> {
        self.socket.try_send_to(packet, target)
    }

    /// Send multiple packets in a single syscall (Linux only).
    ///
    /// Falls back to sequential send_to on other platforms.
    ///
    /// Uses `BatchedTransport::new_send_only` so we don't allocate the
    /// 64 × 8KB receive-side buffer set on a pure-send path. This is a
    /// fresh `BatchedTransport` per call because the struct's iovec
    /// slots point into the caller's `packets` slice — sharing one
    /// across concurrent calls would require a lock on the hot path.
    #[cfg(target_os = "linux")]
    pub fn send_batch(&self, packets: &[Bytes], target: SocketAddr) -> io::Result<usize> {
        use std::os::unix::io::AsRawFd;
        let fd = self.socket.as_raw_fd();
        let mut batched = super::linux::BatchedTransport::new_send_only(fd);
        batched.send_batch(packets, target)
    }
}

impl std::fmt::Debug for PacketSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PacketSender").finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_socket(addr: SocketAddr) -> io::Result<NetSocket> {
        NetSocket::with_config(addr, SocketBufferConfig::for_testing()).await
    }

    #[tokio::test]
    async fn test_socket_creation() {
        let socket = test_socket("127.0.0.1:0".parse().unwrap()).await.unwrap();

        assert!(socket.local_addr().port() != 0);
    }

    #[tokio::test]
    async fn test_socket_send_recv() {
        let mut socket1 = test_socket("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let socket2 = test_socket("127.0.0.1:0".parse().unwrap()).await.unwrap();

        let addr1 = socket1.local_addr();
        let addr2 = socket2.local_addr();

        // Send from socket2 to socket1
        let data = b"hello, net!";
        socket2.send_to(data, addr1).await.unwrap();

        // Receive on socket1
        let (received, source) = socket1.recv_from().await.unwrap();
        assert_eq!(&received[..], data);
        assert_eq!(source, addr2);
    }

    #[tokio::test]
    async fn test_parsed_packet() {
        use super::super::protocol::{PacketFlags, NONCE_SIZE};

        let nonce = [0u8; NONCE_SIZE];
        let header = NetHeader::new(
            0x1234,
            0x5678,
            42,
            nonce,
            10, // payload_len
            1,  // event_count
            PacketFlags::NONE,
        );

        let mut data = BytesMut::with_capacity(HEADER_SIZE + 26);
        data.extend_from_slice(&header.to_bytes());
        data.extend_from_slice(&[0u8; 26]); // 10 bytes payload + 16 bytes tag

        let parsed = ParsedPacket::parse(data.freeze(), "127.0.0.1:1234".parse().unwrap()).unwrap();

        assert_eq!(parsed.header.session_id, 0x1234);
        assert_eq!(parsed.header.stream_id, 0x5678);
        assert_eq!(parsed.header.sequence, 42);
        assert!(parsed.is_valid_length());
    }

    #[tokio::test]
    async fn test_sender_receiver() {
        let socket1 = test_socket("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let socket2 = test_socket("127.0.0.1:0".parse().unwrap()).await.unwrap();

        let addr1 = socket1.local_addr();

        let sender = PacketSender::new(socket2.socket_arc());
        let mut receiver = PacketReceiver::new(socket1.socket_arc());

        // Send
        sender.send_to(b"test packet", addr1).await.unwrap();

        // Receive
        let (data, _) = receiver.recv().await.unwrap();
        assert_eq!(&data[..], b"test packet");
    }

    /// Source pin: the Linux `BatchedPacketReceiver`'s receive
    /// loop must
    ///
    /// 1. exponentially back off on transient errors (rather
    ///    than a fixed `sleep(1ms)`), so a sustained soft error
    ///    doesn't produce a 1000Hz `warn!` storm; and
    /// 2. exit on hard socket errors (`EBADF` / `ENOTSOCK`)
    ///    rather than spin forever.
    ///
    /// Pre-fix every error path slept exactly 1ms and looped
    /// indefinitely; an unrecoverable socket (closed fd,
    /// permission revoke) silently consumed a thread until
    /// shutdown. The runtime check is hard to fault-inject
    /// portably (the receive thread is Linux-only and the
    /// errors are platform-specific), so the source pin is the
    /// cheaper tripwire against a "simplification" PR that
    /// flips back to the busy-loop shape.
    #[test]
    fn batched_recv_loop_must_back_off_and_exit_on_hard_error() {
        let src = include_str!("transport.rs");

        // Body of the Linux batch-receive thread closure. We
        // can't easily extract just the loop body, so we look
        // for the markers in the whole file (other Linux
        // helpers don't carry these names).
        let src_no_comments: String = src
            .lines()
            .map(|l| match l.find("//") {
                Some(idx) => &l[..idx],
                None => l,
            })
            .collect::<Vec<_>>()
            .join("\n");

        // Backoff doubles up to a cap.
        assert!(
            src_no_comments.contains("backoff_ms") && src_no_comments.contains("MAX_BACKOFF_MS"),
            "regression: batched recv loop must use exponential \
             backoff (`backoff_ms` / `MAX_BACKOFF_MS`). Pre-fix the \
             loop slept exactly 1ms forever, producing a 1000Hz \
             warn! storm under any sustained soft error."
        );

        // Hard-error early return.
        assert!(
            src_no_comments.contains("libc::EBADF") && src_no_comments.contains("libc::ENOTSOCK"),
            "regression: batched recv loop must check for EBADF / \
             ENOTSOCK and exit. Without it an unrecoverable socket \
             silently consumes a thread until shutdown."
        );
    }
}
