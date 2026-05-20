//! Smoke test for `NetSocket::new(addr)` — the production-default
//! constructor. Every other test in the crate goes through
//! `SocketBufferConfig::for_testing()` (256 KB recv/send) so the
//! `Default for SocketBufferConfig` impl + `NetSocket::new`'s
//! `with_config(addr, SocketBufferConfig::default())` chain at
//! `src/adapter/net/transport.rs:30-35,60-62` never actually executes
//! in the test suite. That means a typo in `DEFAULT_RECV_BUFFER_SIZE`
//! / `DEFAULT_SEND_BUFFER_SIZE` (64 MiB each) would ship.
//!
//! The test creates two production-default sockets on loopback,
//! sends a datagram from one to the other, and asserts the bytes
//! round-trip. Round-trip success exercises:
//!   * `SocketBufferConfig::default()` (defaults to 64 MiB buffers)
//!   * `NetSocket::new(addr)` (the production constructor path)
//!   * the inner `with_config` body's `set_recv_buffer_size` /
//!     `set_send_buffer_size` calls with the 64-MiB sizes (kernel
//!     accepting that allocation is the actual smoke check; some
//!     locked-down environments may clamp via `rmem_max`, in which
//!     case the buffer ends up smaller but the socket still works).

#![cfg(feature = "net")]

use std::net::SocketAddr;
use std::time::Duration;

use net::adapter::net::NetSocket;

#[tokio::test]
async fn netsocket_new_round_trips_a_loopback_datagram() {
    let bind: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let a = NetSocket::new(bind).await.expect("bind A");
    let b = NetSocket::new(bind).await.expect("bind B");

    let a_addr = a.local_addr();
    let b_addr = b.local_addr();

    let payload = b"netsocket-default-smoke";

    // Send A -> B. `send_to` takes &self so we don't need to thread a
    // mutable reference through. Use the inner UdpSocket directly via
    // `socket_arc()` — the higher-level `PacketSender::send_to` would
    // also work, but this keeps the test scoped to NetSocket itself.
    let bytes_sent = a
        .socket_arc()
        .send_to(payload, b_addr)
        .await
        .expect("send A -> B");
    assert_eq!(bytes_sent, payload.len());

    // Drain on B with a small timeout so the test fails loudly rather
    // than hanging if the kernel-side production-buffer setup is wrong.
    let mut buf = vec![0u8; 256];
    let (len, peer) = tokio::time::timeout(
        Duration::from_secs(2),
        b.socket_arc().recv_from(&mut buf),
    )
    .await
    .expect("recv timed out — production-default NetSocket is not delivering loopback traffic")
    .expect("recv I/O");
    assert_eq!(len, payload.len());
    assert_eq!(&buf[..len], payload);
    assert_eq!(peer, a_addr);
}

#[tokio::test]
async fn netsocket_new_reports_bound_local_addr() {
    // Independent of round-trip: `NetSocket::new(0)` must bind to
    // *some* ephemeral port and `local_addr()` must return it. A
    // regression where `local_addr` returned the unbound `:0` would
    // surface here.
    let bind: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let sock = NetSocket::new(bind).await.expect("bind");

    let resolved = sock.local_addr();
    assert_eq!(resolved.ip(), bind.ip(), "bound IP must match request");
    assert_ne!(resolved.port(), 0, "ephemeral port must resolve to a real port");
}
