//! Linux-specific optimizations for Net.
//!
//! This module provides:
//! - sendmmsg/recvmmsg for batched I/O
//! - io_uring support (optional)
//! - Socket configuration for high-throughput

use bytes::{Bytes, BytesMut};
use std::io;
use std::net::SocketAddr;
use std::os::unix::io::RawFd;

use super::protocol::MAX_PACKET_SIZE;

/// Maximum number of messages in a single sendmmsg/recvmmsg call
pub const MAX_BATCH_SIZE: usize = 64;

/// Batched transport using sendmmsg/recvmmsg.
///
/// This provides significantly higher throughput than individual
/// send/recv calls by amortizing syscall overhead.
pub struct BatchedTransport {
    /// Socket file descriptor
    socket_fd: RawFd,
    /// Pre-allocated iovec structures
    iovecs: Vec<libc::iovec>,
    /// Pre-allocated mmsghdr structures
    msgs: Vec<libc::mmsghdr>,
    /// Pre-allocated sockaddr_in structures
    addrs: Vec<libc::sockaddr_in>,
    /// Receive buffers
    recv_buffers: Vec<BytesMut>,
}

impl BatchedTransport {
    /// Create a new batched transport from a socket file descriptor,
    /// allocating both send-side scratch (iovecs/msgs/addrs) and the
    /// full recv-side 8KB-per-slot buffer set. Use this when the
    /// transport will be used for recvmmsg.
    pub fn new(socket_fd: RawFd) -> Self {
        Self::new_inner(socket_fd, true)
    }

    /// Like `new`, but skips the recv_buffers allocation (64 × 8KB =
    /// 512 KiB) for callers that only ever call `send_batch`. The
    /// full struct is returned with an empty `recv_buffers`; any
    /// `recv_*` call that needs them must use `new` instead.
    pub fn new_send_only(socket_fd: RawFd) -> Self {
        Self::new_inner(socket_fd, false)
    }

    fn new_inner(socket_fd: RawFd, with_recv_buffers: bool) -> Self {
        let mut iovecs = Vec::with_capacity(MAX_BATCH_SIZE);
        let mut msgs = Vec::with_capacity(MAX_BATCH_SIZE);
        let mut addrs = Vec::with_capacity(MAX_BATCH_SIZE);
        let mut recv_buffers = if with_recv_buffers {
            Vec::with_capacity(MAX_BATCH_SIZE)
        } else {
            Vec::new()
        };

        for _ in 0..MAX_BATCH_SIZE {
            iovecs.push(libc::iovec {
                iov_base: std::ptr::null_mut(),
                iov_len: 0,
            });

            addrs.push(unsafe { std::mem::zeroed() });

            // `mem::zeroed` rather than struct-literal: musl's
            // `libc::msghdr` carries private `__pad1` / `__pad2`
            // fields that aren't constructible from a literal,
            // and zero-init is the correct initial state for all
            // fields we use here. Same applies to every
            // `self.msgs[i].msg_hdr = ...` assignment below.
            msgs.push(unsafe { std::mem::zeroed() });

            if with_recv_buffers {
                recv_buffers.push(BytesMut::with_capacity(MAX_PACKET_SIZE));
            }
        }

        Self {
            socket_fd,
            iovecs,
            msgs,
            addrs,
            recv_buffers,
        }
    }

    /// Send multiple packets in a single syscall.
    ///
    /// Returns the number of packets successfully sent — equal to
    /// `packets.len().min(MAX_BATCH_SIZE)` on full success.
    ///
    /// Previously returned `Ok(sent as usize)` after a single
    /// `sendmmsg`. Linux can return `0 < sent < count` on partial
    /// sends; the caller in this crate just recorded `sent` without
    /// re-queueing the tail, so packets `[sent..count)` were silently
    /// lost. For reliable streams `on_send` had already stashed each
    /// packet's bytes for retransmit, so they sat in `pending` "in
    /// flight" without ever reaching the wire — eventually NACK'd,
    /// but with extra latency that didn't need to happen.
    ///
    /// The fix is a small inner loop: re-issue `sendmmsg` on the
    /// unsent tail until either all packets ship, or the syscall
    /// returns a hard error, or we make zero progress (which we
    /// return as `Ok(sent_so_far)` rather than spinning forever).
    pub fn send_batch(&mut self, packets: &[Bytes], target: SocketAddr) -> io::Result<usize> {
        if packets.is_empty() {
            return Ok(0);
        }

        // Convert target address once; reused across every chunk.
        let target_addr = match target {
            SocketAddr::V4(addr) => {
                let mut sockaddr: libc::sockaddr_in = unsafe { std::mem::zeroed() };
                sockaddr.sin_family = libc::AF_INET as u16;
                sockaddr.sin_port = addr.port().to_be();
                sockaddr.sin_addr.s_addr = u32::from_ne_bytes(addr.ip().octets());
                sockaddr
            }
            SocketAddr::V6(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "IPv6 not yet supported for batched I/O",
                ));
            }
        };

        // Chunk internally rather than silently truncating to the
        // first `MAX_BATCH_SIZE` packets. Pre-fix `total =
        // packets.len().min(MAX_BATCH_SIZE)` returned `Ok(64)` for
        // any `packets.len() > 64`, and the caller compared the
        // returned count against `packets.len()` to detect partial
        // sends — so the silent truncation looked like a fully
        // successful 64-packet send. Reliable streams already
        // stashed the unsent tail's bytes for retransmit, so they
        // sat "in flight" without ever reaching the wire until
        // NACK'd.
        let mut total_sent: usize = 0;
        for chunk_start in (0..packets.len()).step_by(MAX_BATCH_SIZE) {
            let chunk_end = (chunk_start + MAX_BATCH_SIZE).min(packets.len());
            let chunk_len = chunk_end - chunk_start;
            let chunk_sent =
                self.send_batch_chunk(&packets[chunk_start..chunk_end], &target_addr)?;
            total_sent += chunk_sent;
            // Partial chunk send means the kernel back-pressured;
            // surface the running total rather than re-queueing
            // the tail and risking another partial.
            if chunk_sent < chunk_len {
                return Ok(total_sent);
            }
        }
        Ok(total_sent)
    }

    /// Send up to `MAX_BATCH_SIZE` packets in a single `sendmmsg`,
    /// retrying the tail on benign errors. Caller is responsible
    /// for ensuring `packets.len() <= MAX_BATCH_SIZE`.
    fn send_batch_chunk(
        &mut self,
        packets: &[Bytes],
        target_addr: &libc::sockaddr_in,
    ) -> io::Result<usize> {
        debug_assert!(packets.len() <= MAX_BATCH_SIZE);
        let total = packets.len();
        if total == 0 {
            return Ok(0);
        }

        // Setup messages for the chunk up front. The retry loop
        // below issues sendmmsg against the tail starting at
        // `&self.msgs[sent_so_far]`, so the slot contents remain
        // valid for the entire call.
        // `iov_base: *mut c_void` is the Linux ABI shape; the
        // kernel reads through this pointer for sendmmsg and
        // never writes. The const→mut cast at `packet.as_ptr()
        // as *mut _` below is API-mandated (libc::iovec doesn't
        // expose a read-only variant) and the actual behavior is
        // sound — the `&[Bytes]` argument keeps the storage alive
        // for the syscall's duration, and the kernel's reads
        // through `iov_base` don't violate Rust's aliasing model.
        //
        // Strict-provenance / Miri does flag the const→mut cast
        // as "pointer laundering" because Miri can't know the
        // kernel won't write. Documenting the soundness argument
        // here is the static answer; a dynamic answer would need
        // `pointer::with_addr` or a similar provenance-explicit
        // API once stabilized.
        for (i, packet) in packets.iter().enumerate() {
            self.iovecs[i] = libc::iovec {
                iov_base: packet.as_ptr() as *mut _,
                iov_len: packet.len(),
            };

            self.addrs[i] = *target_addr;

            // See `new_inner` for the rationale: musl's `msghdr`
            // has private padding fields, so we zero the struct
            // and overwrite the public fields rather than using a
            // struct literal.
            self.msgs[i].msg_hdr = unsafe { std::mem::zeroed() };
            self.msgs[i].msg_hdr.msg_name = &mut self.addrs[i] as *mut _ as *mut _;
            self.msgs[i].msg_hdr.msg_namelen = std::mem::size_of::<libc::sockaddr_in>() as u32;
            self.msgs[i].msg_hdr.msg_iov = &mut self.iovecs[i];
            self.msgs[i].msg_hdr.msg_iovlen = 1;
            self.msgs[i].msg_len = 0;
        }

        // Retry tail until either all packets ship, the kernel
        // returns a hard error, or we make zero progress.
        let mut sent_so_far: usize = 0;
        while sent_so_far < total {
            let remaining = total - sent_so_far;
            let sent = unsafe {
                libc::sendmmsg(
                    self.socket_fd,
                    self.msgs.as_mut_ptr().add(sent_so_far),
                    remaining as u32,
                    0,
                )
            };

            if sent < 0 {
                let err = io::Error::last_os_error();
                // EINTR is benign — retry the tail. Same for
                // EAGAIN/EWOULDBLOCK only when *no* progress has
                // been made; otherwise we surface the partial
                // count and let the caller decide.
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                if sent_so_far > 0 {
                    return Ok(sent_so_far);
                }
                return Err(err);
            }
            let sent = sent as usize;
            if sent == 0 {
                // Zero progress — bail with what we got. Should
                // not happen on a healthy socket; treating it as
                // an indefinite spin would be worse than
                // surfacing the partial count.
                break;
            }
            sent_so_far += sent;
        }

        Ok(sent_so_far)
    }

    /// Receive multiple packets in a single syscall.
    ///
    /// Returns a vector of (data, source_address) tuples.
    pub fn recv_batch(&mut self, max_count: usize) -> io::Result<Vec<(Bytes, SocketAddr)>> {
        let count = max_count.min(MAX_BATCH_SIZE);
        if count == 0 {
            return Ok(Vec::new());
        }

        // A `BatchedTransport` constructed via `new_send_only`
        // skips the `recv_buffers` allocation, so indexing into them
        // below would panic with index-out-of-bounds. Surface the
        // misuse as an explicit error instead.
        if self.recv_buffers.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "BatchedTransport constructed via `new_send_only` cannot \
                 receive packets — use `new` if recv is needed",
            ));
        }

        // Setup receive buffers
        for i in 0..count {
            self.recv_buffers[i].resize(MAX_PACKET_SIZE, 0);

            self.iovecs[i] = libc::iovec {
                iov_base: self.recv_buffers[i].as_mut_ptr() as *mut _,
                iov_len: MAX_PACKET_SIZE,
            };

            self.addrs[i] = unsafe { std::mem::zeroed() };

            // See `new_inner` for the zero-then-assign rationale.
            self.msgs[i].msg_hdr = unsafe { std::mem::zeroed() };
            self.msgs[i].msg_hdr.msg_name = &mut self.addrs[i] as *mut _ as *mut _;
            self.msgs[i].msg_hdr.msg_namelen = std::mem::size_of::<libc::sockaddr_in>() as u32;
            self.msgs[i].msg_hdr.msg_iov = &mut self.iovecs[i];
            self.msgs[i].msg_hdr.msg_iovlen = 1;
            self.msgs[i].msg_len = 0;
        }

        // Receive (non-blocking)
        let received = unsafe {
            libc::recvmmsg(
                self.socket_fd,
                self.msgs.as_mut_ptr(),
                count as u32,
                // `as _` so the constant matches `recvmmsg`'s
                // flags-arg type — `c_int` on glibc, `c_uint` on
                // musl.
                libc::MSG_DONTWAIT as _,
                std::ptr::null_mut(),
            )
        };

        if received < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::WouldBlock {
                return Ok(Vec::new());
            }
            return Err(err);
        }

        // Collect results
        let mut results = Vec::with_capacity(received as usize);
        for i in 0..(received as usize) {
            let len = self.msgs[i].msg_len as usize;
            let mut buffer = std::mem::replace(
                &mut self.recv_buffers[i],
                BytesMut::with_capacity(MAX_PACKET_SIZE),
            );
            buffer.truncate(len);

            let addr = sockaddr_to_socket_addr(&self.addrs[i])?;
            results.push((buffer.freeze(), addr));
        }

        Ok(results)
    }

    /// Receive multiple packets, blocking until at least one is available.
    #[allow(dead_code)]
    pub fn recv_batch_blocking(
        &mut self,
        max_count: usize,
    ) -> io::Result<Vec<(Bytes, SocketAddr)>> {
        let count = max_count.min(MAX_BATCH_SIZE);
        if count == 0 {
            return Ok(Vec::new());
        }

        // See `recv_batch` for the rationale on this guard.
        if self.recv_buffers.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "BatchedTransport constructed via `new_send_only` cannot \
                 receive packets — use `new` if recv is needed",
            ));
        }

        // Setup receive buffers
        for i in 0..count {
            self.recv_buffers[i].resize(MAX_PACKET_SIZE, 0);

            self.iovecs[i] = libc::iovec {
                iov_base: self.recv_buffers[i].as_mut_ptr() as *mut _,
                iov_len: MAX_PACKET_SIZE,
            };

            self.addrs[i] = unsafe { std::mem::zeroed() };

            // See `new_inner` for the zero-then-assign rationale.
            self.msgs[i].msg_hdr = unsafe { std::mem::zeroed() };
            self.msgs[i].msg_hdr.msg_name = &mut self.addrs[i] as *mut _ as *mut _;
            self.msgs[i].msg_hdr.msg_namelen = std::mem::size_of::<libc::sockaddr_in>() as u32;
            self.msgs[i].msg_hdr.msg_iov = &mut self.iovecs[i];
            self.msgs[i].msg_hdr.msg_iovlen = 1;
            self.msgs[i].msg_len = 0;
        }

        // Receive (blocking)
        let received = unsafe {
            libc::recvmmsg(
                self.socket_fd,
                self.msgs.as_mut_ptr(),
                count as u32,
                // Blocking. `as _` for the same flags-arg type
                // mismatch between glibc/musl noted above.
                0_i32 as _,
                std::ptr::null_mut(),
            )
        };

        if received < 0 {
            return Err(io::Error::last_os_error());
        }

        // Collect results
        let mut results = Vec::with_capacity(received as usize);
        for i in 0..(received as usize) {
            let len = self.msgs[i].msg_len as usize;
            let mut buffer = std::mem::replace(
                &mut self.recv_buffers[i],
                BytesMut::with_capacity(MAX_PACKET_SIZE),
            );
            buffer.truncate(len);

            let addr = sockaddr_to_socket_addr(&self.addrs[i])?;
            results.push((buffer.freeze(), addr));
        }

        Ok(results)
    }
}

impl std::fmt::Debug for BatchedTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BatchedTransport")
            .field("socket_fd", &self.socket_fd)
            .field("max_batch_size", &MAX_BATCH_SIZE)
            .finish()
    }
}

/// Convert sockaddr_in to SocketAddr
fn sockaddr_to_socket_addr(addr: &libc::sockaddr_in) -> io::Result<SocketAddr> {
    let ip = std::net::Ipv4Addr::from(u32::from_be(addr.sin_addr.s_addr));
    let port = u16::from_be(addr.sin_port);
    Ok(SocketAddr::new(ip.into(), port))
}

/// Configure socket for high-throughput operation.
pub fn configure_socket_for_throughput(fd: RawFd) -> io::Result<()> {
    // Increase buffer sizes
    unsafe {
        let recv_buf: i32 = 64 * 1024 * 1024; // 64 MB
        let send_buf: i32 = 64 * 1024 * 1024; // 64 MB

        if libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            &recv_buf as *const _ as *const libc::c_void,
            std::mem::size_of::<i32>() as u32,
        ) < 0
        {
            return Err(io::Error::last_os_error());
        }

        if libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_SNDBUF,
            &send_buf as *const _ as *const libc::c_void,
            std::mem::size_of::<i32>() as u32,
        ) < 0
        {
            return Err(io::Error::last_os_error());
        }

        // Enable busy polling (reduces latency)
        let busy_poll: i32 = 50; // microseconds
        let _ = libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_BUSY_POLL,
            &busy_poll as *const _ as *const libc::c_void,
            std::mem::size_of::<i32>() as u32,
        );

        // Disable fragmentation
        let pmtu: i32 = libc::IP_PMTUDISC_DO;
        let _ = libc::setsockopt(
            fd,
            libc::IPPROTO_IP,
            libc::IP_MTU_DISCOVER,
            &pmtu as *const _ as *const libc::c_void,
            std::mem::size_of::<i32>() as u32,
        );
    }

    Ok(())
}

/// Enable nanosecond timestamps on the socket.
#[allow(dead_code)]
pub fn enable_timestamps(fd: RawFd) -> io::Result<()> {
    unsafe {
        let enable: i32 = 1;
        if libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_TIMESTAMPNS,
            &enable as *const _ as *const libc::c_void,
            std::mem::size_of::<i32>() as u32,
        ) < 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::UdpSocket;
    use std::os::unix::io::AsRawFd;

    #[test]
    fn test_batched_transport_creation() {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let fd = socket.as_raw_fd();
        let transport = BatchedTransport::new(fd);

        assert!(transport.iovecs.len() == MAX_BATCH_SIZE);
        assert!(transport.msgs.len() == MAX_BATCH_SIZE);
    }

    #[test]
    fn test_send_recv_batch() {
        let socket1 = UdpSocket::bind("127.0.0.1:0").unwrap();
        let socket2 = UdpSocket::bind("127.0.0.1:0").unwrap();

        socket1.set_nonblocking(true).unwrap();
        socket2.set_nonblocking(true).unwrap();

        let addr1 = socket1.local_addr().unwrap();
        let addr2 = socket2.local_addr().unwrap();

        let mut transport1 = BatchedTransport::new(socket1.as_raw_fd());
        let mut transport2 = BatchedTransport::new(socket2.as_raw_fd());

        // Send batch from transport2 to transport1
        let packets = vec![
            Bytes::from_static(b"packet1"),
            Bytes::from_static(b"packet2"),
            Bytes::from_static(b"packet3"),
        ];

        let sent = transport2.send_batch(&packets, addr1).unwrap();
        assert_eq!(sent, 3);

        // Small delay for packets to arrive
        std::thread::sleep(std::time::Duration::from_millis(10));

        // Receive batch on transport1
        let received = transport1.recv_batch(10).unwrap();
        assert_eq!(received.len(), 3);

        assert_eq!(&received[0].0[..], b"packet1");
        assert_eq!(&received[1].0[..], b"packet2");
        assert_eq!(&received[2].0[..], b"packet3");

        for (_, source) in &received {
            assert_eq!(*source, addr2);
        }
    }

    #[test]
    fn test_configure_socket() {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let fd = socket.as_raw_fd();

        // Should not fail
        configure_socket_for_throughput(fd).unwrap();
    }

    /// Regression for BUG_AUDIT_2026_04_30_CORE.md #90:
    /// `BatchedTransport::new_send_only` skips the `recv_buffers`
    /// allocation, leaving the vector empty. Pre-fix, calling
    /// `recv_batch` on a send-only transport panicked with
    /// index-out-of-bounds at the first `self.recv_buffers[i]
    /// .resize(...)` line. The fix surfaces the misuse as an
    /// `io::ErrorKind::Unsupported` instead.
    #[test]
    fn recv_batch_returns_unsupported_for_send_only_transport() {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let fd = socket.as_raw_fd();
        let mut transport = BatchedTransport::new_send_only(fd);

        let err = transport
            .recv_batch(8)
            .expect_err("send-only recv must surface Unsupported, not panic");
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);

        let err_blocking = transport
            .recv_batch_blocking(8)
            .expect_err("send-only recv_batch_blocking must also surface Unsupported");
        assert_eq!(err_blocking.kind(), io::ErrorKind::Unsupported);

        // Sanity: a `new()` (recv-capable) transport doesn't trip
        // the guard. We don't actually assert success of recv (no
        // packets are arriving), just that the guard isn't fired.
        let mut recv_transport = BatchedTransport::new(fd);
        // 0-count is the explicit no-op path before the guard.
        let zero = recv_transport.recv_batch(0).unwrap();
        assert!(zero.is_empty());
    }
}
