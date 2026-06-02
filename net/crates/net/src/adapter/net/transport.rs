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

// --- Recv-loop batching instrument (NRPC_RECV_LOOP_BATCHING_PLAN) --------
//
// Symmetric to the send-loop drain instrument in `router.rs`. Measures the
// receive path's *batch size*: how many packets a single `recvmmsg`
// (`BatchedTransport::recv_batch`) returned. `packets / syscalls` is the
// realized ingress syscall-collapse factor — the recv analogue of the send
// side's packets/flush ratio. Off unless `arm_recv_drain_histo()` is called
// BEFORE the receive thread starts (the thread latches the flag once at
// startup), so an unarmed loop does zero recording.
//
// Compiled only under the `batched-ingress` build feature — the instrument
// measures the batched receive path, so when that path isn't built neither is
// this. Measurement scaffolding, not supported public API: every entry point
// is `#[doc(hidden)]` and re-exported only so the in-repo integration tests
// (which compile as external crates) can drive it.
#[cfg(feature = "batched-ingress")]
mod recv_instrument {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    static RECV_DRAIN_HISTO: [AtomicU64; 9] = [
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
    ];
    static RECV_DRAIN_ARMED: AtomicBool = AtomicBool::new(false);
    static RECV_DRAIN_MAX: AtomicU64 = AtomicU64::new(0);
    static RECV_BATCH_SYSCALLS: AtomicU64 = AtomicU64::new(0);
    static RECV_BATCH_PACKETS: AtomicU64 = AtomicU64::new(0);

    /// Arm the recv-batch-size histogram. Must be called BEFORE the receive
    /// thread starts — the thread reads the flag once at startup.
    #[doc(hidden)]
    pub fn arm_recv_drain_histo() {
        RECV_DRAIN_ARMED.store(true, Ordering::Relaxed);
    }

    /// Snapshot the recv-batch-size histogram. Index `i` (for `i < 8`) counts
    /// `recvmmsg` returns whose packet count fell in `[2^i, 2^(i+1))`; index
    /// `8` is `[256, ∞)`.
    #[doc(hidden)]
    pub fn recv_drain_histo_snapshot() -> [u64; 9] {
        let mut out = [0u64; 9];
        for (i, b) in RECV_DRAIN_HISTO.iter().enumerate() {
            out[i] = b.load(Ordering::Relaxed);
        }
        out
    }

    /// Largest single `recvmmsg` batch observed so far (exact, not bucketed).
    #[doc(hidden)]
    pub fn recv_drain_max() -> u64 {
        RECV_DRAIN_MAX.load(Ordering::Relaxed)
    }

    /// `(syscalls, packets)` received via the batched-ingress path.
    /// `packets / syscalls` is the realized recv syscall-collapse factor
    /// (→ `MAX_BATCH_SIZE` as the socket saturates).
    #[doc(hidden)]
    pub fn recv_batch_stats() -> (u64, u64) {
        (
            RECV_BATCH_SYSCALLS.load(Ordering::Relaxed),
            RECV_BATCH_PACKETS.load(Ordering::Relaxed),
        )
    }

    /// Latch the arm flag (called once at receive-thread startup, mirroring
    /// the send loop's `measure_drain`). Only compiled on Linux, the sole
    /// batched recv path; the per-packet fallback elsewhere has no batch to
    /// record.
    #[cfg(target_os = "linux")]
    #[inline]
    pub(super) fn recv_drain_armed() -> bool {
        RECV_DRAIN_ARMED.load(Ordering::Relaxed)
    }

    /// Record one non-empty `recvmmsg` batch of `packets` packets. `armed` is
    /// the value latched at thread startup; when false this is a no-op so the
    /// production path pays nothing.
    #[cfg(target_os = "linux")]
    #[inline]
    pub(super) fn record_recv_batch(packets: u64, armed: bool) {
        if !armed || packets == 0 {
            return;
        }
        RECV_BATCH_SYSCALLS.fetch_add(1, Ordering::Relaxed);
        RECV_BATCH_PACKETS.fetch_add(packets, Ordering::Relaxed);
        // floor(log2(packets)), clamped to the top bucket.
        let bucket = (63 - packets.leading_zeros()).min(8) as usize;
        RECV_DRAIN_HISTO[bucket].fetch_add(1, Ordering::Relaxed);
        RECV_DRAIN_MAX.fetch_max(packets, Ordering::Relaxed);
    }
}

#[cfg(feature = "batched-ingress")]
pub use recv_instrument::{
    arm_recv_drain_histo, recv_batch_stats, recv_drain_histo_snapshot, recv_drain_max,
};

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

    /// Receive a packet, returning the data and source address.
    ///
    /// Routes through tokio's `recv_buf_from(&mut BufMut)` for the same
    /// reason as [`PacketReceiver::recv`] (crypto-session perf #130): the
    /// legacy `resize(MAX_PACKET_SIZE, 0)` + `recv_from(&mut [u8])` shape
    /// memset ~1500 bytes per packet only for the kernel to overwrite them
    /// on the next syscall. `recv_buf_from` writes directly into the
    /// `BytesMut`'s spare capacity, so the kernel's bytes are the first
    /// writers and no pre-init is needed.
    pub async fn recv_from(&mut self) -> io::Result<(Bytes, SocketAddr)> {
        self.recv_buf.clear();
        self.recv_buf.reserve(MAX_PACKET_SIZE);
        let (_len, addr) = self.socket.recv_buf_from(&mut self.recv_buf).await?;
        Ok((self.recv_buf.split().freeze(), addr))
    }

    /// Receive a packet from the connected address.
    ///
    /// Routes through tokio's `recv_buf(&mut BufMut)` for the same reason
    /// as [`Self::recv_from`] — see that method's docs.
    pub async fn recv(&mut self) -> io::Result<Bytes> {
        self.recv_buf.clear();
        self.recv_buf.reserve(MAX_PACKET_SIZE);
        let _len = self.socket.recv_buf(&mut self.recv_buf).await?;
        Ok(self.recv_buf.split().freeze())
    }

    /// Try to receive a packet without blocking.
    ///
    /// Routes through tokio's `try_recv_buf_from(&mut BufMut)` for the
    /// same reason as [`Self::recv_from`] — see that method's docs.
    pub fn try_recv_from(&mut self) -> io::Result<Option<(Bytes, SocketAddr)>> {
        self.recv_buf.clear();
        self.recv_buf.reserve(MAX_PACKET_SIZE);
        match self.socket.try_recv_buf_from(&mut self.recv_buf) {
            Ok((_len, addr)) => Ok(Some((self.recv_buf.split().freeze(), addr))),
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

    /// Receive the next packet.
    ///
    /// Routes through tokio's `recv_buf_from(&mut BufMut)` per
    /// crypto-session perf #130. The legacy
    /// `resize(MAX_PACKET_SIZE, 0)` + `recv_from(&mut [u8])` shape
    /// zero-filled ~1500 bytes per packet just for the kernel to
    /// overwrite them on the next syscall — pure wasted memset
    /// bandwidth at packet rate. `recv_buf_from` writes directly
    /// into the `BytesMut`'s spare capacity using `BufMut::chunk_mut`,
    /// so the kernel's bytes are the first writers and no
    /// pre-init is needed.
    ///
    /// `clear()` + `reserve(MAX_PACKET_SIZE)` returns the buffer
    /// to length 0 while keeping the existing allocation; the
    /// `reserve` is a no-op once steady-state capacity is reached.
    /// The `freeze()` at the end transfers ownership of the
    /// initialized prefix to the returned `Bytes`; `split()`
    /// leaves the underlying allocation behind for the next call
    /// to reuse via `reserve`.
    pub async fn recv(&mut self) -> io::Result<(Bytes, SocketAddr)> {
        self.recv_buf.clear();
        self.recv_buf.reserve(MAX_PACKET_SIZE);
        let (_len, addr) = self.socket.recv_buf_from(&mut self.recv_buf).await?;
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
///
/// The channel carries a *whole recvmmsg batch* per message (not one packet
/// per message): a 64-packet recvmmsg costs one `blocking_send` and one
/// cross-thread handoff instead of 64, so the per-packet overhead the syscall
/// batching removed isn't silently re-imposed by the channel. `recv()` drains
/// the most-recent batch from `pending` front-to-back (preserving arrival
/// order) before pulling the next batch off the channel, so its
/// one-packet-at-a-time signature is unchanged.
#[cfg(target_os = "linux")]
pub struct BatchedPacketReceiver {
    rx: tokio::sync::mpsc::Receiver<Vec<(Bytes, SocketAddr)>>,
    /// Packets from the batch currently being drained, in arrival order.
    pending: std::collections::VecDeque<(Bytes, SocketAddr)>,
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
    #[expect(
        clippy::expect_used,
        reason = "std::thread::Builder::spawn only fails on OS resource exhaustion (OOM, ulimit); aborting at startup is the documented behavior for that condition"
    )]
    pub fn new(socket: Arc<UdpSocket>) -> Self {
        use std::os::unix::io::AsRawFd;
        use std::sync::atomic::Ordering;

        let fd = socket.as_raw_fd();

        // Apply high-throughput socket tuning
        let _ = super::linux::configure_socket_for_throughput(fd);

        // Channel depth is in *batches* now, not packets. Worst case it
        // buffers `RECV_BATCH_CHANNEL_DEPTH * MAX_BATCH_SIZE` packets in front
        // of the kernel socket buffer; once full, the thread's `blocking_send`
        // parks, the kernel buffer fills, and existing flow control takes over
        // (unchanged backpressure). Tune against the c128 measurement — see
        // NRPC_RECV_LOOP_BATCHING_PLAN.
        const RECV_BATCH_CHANNEL_DEPTH: usize = 64;
        let (tx, rx) = tokio::sync::mpsc::channel(RECV_BATCH_CHANNEL_DEPTH);
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

                // Latch the recv instrument once at startup (mirrors the send
                // loop's `measure_drain`): when armed, each non-empty recvmmsg
                // records its batch size; when unarmed, recording is a no-op.
                // Only present under the `batched-ingress` feature (the
                // instrument it feeds is gated there); this receiver is also
                // used by NetAdapter without the feature, where it doesn't
                // record.
                #[cfg(feature = "batched-ingress")]
                let measure = recv_instrument::recv_drain_armed();

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
                            // Capture the batch size before the move so it can
                            // be recorded AFTER a successful handoff: a batch
                            // dropped because the channel closed mid-teardown is
                            // never delivered to the consumer, so counting it
                            // before the send would inflate the syscall/packet
                            // tallies by the in-flight batch at shutdown.
                            #[cfg(feature = "batched-ingress")]
                            let batch_len = packets.len() as u64;
                            // Hand the whole batch over in a single channel op
                            // (one cross-thread handoff per recvmmsg, not per
                            // packet). `recv()` drains it front-to-back.
                            if tx.blocking_send(packets).is_err() {
                                return;
                            }
                            // One non-empty recvmmsg, successfully handed off,
                            // is one ingress syscall the consumer will observe.
                            #[cfg(feature = "batched-ingress")]
                            recv_instrument::record_recv_batch(batch_len, measure);
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
            pending: std::collections::VecDeque::new(),
            shutdown,
            _socket: socket,
            _thread: Some(thread),
        }
    }

    /// Receive the next packet.
    ///
    /// Drains the current batch front-to-back (arrival order) before pulling
    /// the next recvmmsg batch off the channel, so callers still see one
    /// packet at a time in the order the kernel delivered them. Returns
    /// `ConnectionReset` once the receive thread has exited and every buffered
    /// packet has been handed out.
    pub async fn recv(&mut self) -> io::Result<(Bytes, SocketAddr)> {
        loop {
            if let Some(packet) = self.pending.pop_front() {
                return Ok(packet);
            }
            match self.rx.recv().await {
                // The thread only sends non-empty batches; the loop re-checks
                // `pending` and returns the first packet. (An empty batch, were
                // one ever sent, simply loops back to await the next.)
                Some(batch) => self.pending.extend(batch),
                None => {
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionReset,
                        "batch receiver closed",
                    ))
                }
            }
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for BatchedPacketReceiver {
    fn drop(&mut self) {
        self.shutdown
            .store(true, std::sync::atomic::Ordering::Release);
        // Close the channel BEFORE joining. The recv thread only checks
        // `shutdown` at the top of its loop, so a thread currently parked in
        // `blocking_send` on a full channel would never see the flag — and the
        // consumer side (us) is being torn down, so nothing drains the channel
        // to unblock it. Closing the receiver makes that parked `blocking_send`
        // return `Err`, on which the thread returns; otherwise `join()` below
        // deadlocks. (The shutdown store still covers the common case where the
        // thread is in its idle poll.)
        self.rx.close();
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

    /// Source pin: crypto-session perf #130 — `PacketReceiver::recv`
    /// MUST route through tokio's `recv_buf_from` (which appends
    /// to a `BufMut`'s spare capacity) and MUST NOT `resize(N, 0)`
    /// on the receive buffer. Pre-fix every packet paid a
    /// `~1500 byte memset` to zero just for the kernel to
    /// overwrite the same bytes immediately. A regression that
    /// flips back to `resize(MAX_PACKET_SIZE, 0)` would silently
    /// re-introduce gigabytes/sec of memset on high-pps
    /// deployments — observable only as a microbenchmark
    /// regression at runtime, so pin via source inspection.
    #[test]
    fn packet_receiver_recv_must_use_recv_buf_from_not_resize_zero() {
        let src = include_str!("transport.rs");
        let body_idx = src
            .find("pub async fn recv(&mut self) -> io::Result<(Bytes, SocketAddr)>")
            .expect("PacketReceiver::recv must exist");
        // Look at the immediate body of the method (next ~400 chars).
        let body = &src[body_idx..body_idx + 1200];
        assert!(
            body.contains("recv_buf_from"),
            "regression: PacketReceiver::recv must route through `recv_buf_from` \
             per crypto-session perf #130; falling back to `recv_from(&mut [u8])` \
             requires pre-zeroing the buffer. Body: {body}"
        );
        assert!(
            !body.contains("resize(MAX_PACKET_SIZE, 0)"),
            "regression: PacketReceiver::recv must NOT pre-zero the receive buffer; \
             pre-fix this memset ~1500 bytes per packet only for the kernel to \
             overwrite them immediately. Body: {body}"
        );
    }

    /// Source pin: the three `NetSocket` receive entry points
    /// (`recv_from`, `recv`, `try_recv_from`) MUST route through
    /// tokio's `*_buf*` family (which appends to a `BufMut`'s spare
    /// capacity) and MUST NOT `resize(N, 0)` on the receive buffer.
    /// Same rationale as the `PacketReceiver::recv` pin above
    /// (crypto-session perf #130): pre-fix each call paid ~1500 bytes
    /// of memset bandwidth per packet only for the kernel to overwrite
    /// the same bytes immediately. These `NetSocket` methods were
    /// sibling code in the same file and were missed by the original
    /// fix; the pin guards against a "simplification" PR that flips
    /// any of them back to the legacy shape.
    ///
    /// Per the same runtime-built-needle pattern as
    /// `batched_recv_must_use_set_len_not_resize_zero` in `linux.rs`,
    /// the bad needle is assembled at runtime from the template
    /// `"resize({}, 0)"` and the identifier `"MAX_PACKET_SIZE"` so
    /// this test's own assertion strings don't self-match in the
    /// inspected source.
    #[test]
    fn net_socket_recv_methods_must_use_recv_buf_family_not_resize_zero() {
        let src = include_str!("transport.rs");
        let src_no_comments: String = src
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");

        let impl_start = src_no_comments
            .find("impl NetSocket {")
            .expect("impl NetSocket block must exist");
        let impl_rest = &src_no_comments[impl_start..];
        let impl_end = impl_rest
            .find("\nimpl ")
            .map(|i| impl_start + 1 + i)
            .unwrap_or(src_no_comments.len());
        let impl_body = &src_no_comments[impl_start..impl_end];

        let bad_needle = format!("resize({}, 0)", "MAX_PACKET_SIZE");
        assert!(
            !impl_body.contains(&bad_needle),
            "regression: NetSocket receive methods must NOT pre-zero \
             the recv buffer per crypto-session perf #130; pre-fix the \
             memset of ~1500 bytes per packet only existed for the \
             kernel to overwrite the same bytes immediately."
        );

        for needle in ["recv_buf_from", "recv_buf(", "try_recv_buf_from"] {
            assert!(
                impl_body.contains(needle),
                "regression: NetSocket impl must route receive through \
                 tokio's `{needle}` (BufMut spare-capacity API); falling \
                 back to the `&mut [u8]` family requires pre-zeroing \
                 the buffer."
            );
        }
    }

    #[tokio::test]
    async fn net_socket_recv_from_round_trips_loopback_datagram() {
        let mut a = test_socket("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let b = test_socket("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let a_addr = a.local_addr();

        b.send_to(b"hello-recv_from", a_addr).await.unwrap();

        let (data, from) = tokio::time::timeout(std::time::Duration::from_secs(2), a.recv_from())
            .await
            .expect("recv_from did not return within 2 s")
            .expect("recv_from failed");

        assert_eq!(&data[..], b"hello-recv_from");
        assert_eq!(from, b.local_addr());
    }

    #[tokio::test]
    async fn net_socket_recv_round_trips_loopback_datagram() {
        let mut a = test_socket("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let b = test_socket("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let a_addr = a.local_addr();
        let b_addr = b.local_addr();

        a.connect(b_addr).await.unwrap();
        b.send_to(b"hello-recv", a_addr).await.unwrap();

        let data = tokio::time::timeout(std::time::Duration::from_secs(2), a.recv())
            .await
            .expect("recv did not return within 2 s")
            .expect("recv failed");

        assert_eq!(&data[..], b"hello-recv");
    }

    #[tokio::test]
    async fn net_socket_try_recv_from_returns_none_when_empty_and_data_when_ready() {
        let mut a = test_socket("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let b = test_socket("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let a_addr = a.local_addr();

        assert!(
            matches!(a.try_recv_from(), Ok(None)),
            "try_recv_from on an empty socket must return Ok(None) \
             (WouldBlock mapped to None)"
        );

        b.send_to(b"hello-try_recv_from", a_addr).await.unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match a.try_recv_from() {
                Ok(Some((data, from))) => {
                    assert_eq!(&data[..], b"hello-try_recv_from");
                    assert_eq!(from, b.local_addr());
                    break;
                }
                Ok(None) => {
                    if std::time::Instant::now() >= deadline {
                        panic!("try_recv_from did not observe loopback datagram within 2 s");
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                Err(e) => panic!("try_recv_from errored: {e}"),
            }
        }
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

    /// Runtime smoke test (Linux only): construct the
    /// `BatchedPacketReceiver`, drive a few packets through the
    /// recvmmsg loop, and drop it. Exercises the constructor body,
    /// the `Ok(packets)` happy branch, the `Ok([])` empty-batch
    /// sleep branch (between sends, while the channel is empty),
    /// and the `Drop::drop` shutdown handshake. Pairs with the
    /// source-pin tripwire above — the tripwire catches a
    /// "simplification" PR; this test catches an actual runtime
    /// regression where the loop fails to deliver packets or
    /// fails to shut down within the join.
    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn batched_recv_delivers_and_shuts_down_cleanly() {
        let recv_sock = test_socket("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let recv_addr = recv_sock.local_addr();
        let send_sock = test_socket("127.0.0.1:0".parse().unwrap()).await.unwrap();

        let mut batched = BatchedPacketReceiver::new(recv_sock.socket_arc());

        // Send a few packets. Three is enough to exercise the
        // happy-path branch through the loop without depending on
        // recvmmsg ever returning more than one at a time.
        for i in 0..3u8 {
            send_sock
                .socket_arc()
                .send_to(&[0xAA, i, 0xBB], recv_addr)
                .await
                .unwrap();
        }

        // Drain with a bounded deadline. The 1 ms sleep inside the
        // empty-batch / WouldBlock branch means the thread re-checks
        // shutdown promptly; 2 s is generous for loopback delivery.
        let mut got = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while got.len() < 3 && std::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(200), batched.recv()).await
            {
                Ok(Ok((data, _addr))) => got.push(data),
                Ok(Err(_)) | Err(_) => continue,
            }
        }
        assert_eq!(
            got.len(),
            3,
            "BatchedPacketReceiver did not deliver three loopback datagrams within 2 s"
        );

        // Dropping the receiver sets the shutdown AtomicBool and
        // joins the thread. If the loop body doesn't notice the
        // shutdown flag promptly, this hangs forever — the
        // `tokio::time::timeout` around `spawn_blocking` bounds the
        // join so a regression surfaces as a clean test failure
        // rather than an indefinite hang.
        let join = tokio::task::spawn_blocking(move || drop(batched));
        tokio::time::timeout(std::time::Duration::from_secs(2), join)
            .await
            .expect("Drop::drop for BatchedPacketReceiver did not join within 2 s")
            .expect("spawn_blocking join");
    }

    // The earlier `batched_recv_exits_on_hard_socket_error` runtime
    // tripwire was deleted: rustc 1.95's IO-safety enforcement aborts
    // the process the next time anyone closes an already-closed fd
    // (`fatal runtime error: IO Safety violation: owned file
    // descriptor already closed, aborting`), and `libc::close(fd)` on
    // an fd still owned by `Arc<UdpSocket>` is exactly that case. The
    // source-pin tripwire `batched_recv_loop_must_back_off_and_exit_on_hard_error`
    // at the start of this module already catches the
    // "simplification PR that removes the EBADF check" class of
    // regression by textual matching on the source, and that's the
    // cheapest tripwire available — runtime fault injection of EBADF
    // isn't safely possible in modern Rust without leaking the
    // socket.
}
