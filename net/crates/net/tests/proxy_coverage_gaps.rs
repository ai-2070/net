//! Invariant tests for `src/adapter/net/proxy.rs` branches the
//! existing in-module tests don't exercise:
//!
//!   * `HopStats::avg_latency_ns` zero-samples early return.
//!   * `NetProxy::local_addr` resolves the ephemeral bind to a
//!     real port (not 0).
//!   * `forward()` drop branches:
//!       - PacketTooSmall when `data.len() < ROUTING_HEADER_SIZE`.
//!       - InvalidHeader when `RoutingHeader::from_bytes` returns
//!         None.
//!       - TtlExpired AFTER `forward()`'s decrement zeros TTL —
//!         the post-decrement tripwire is distinct from the
//!         pre-decrement check covered by the in-module tests.
//!   * `forward_and_send()` happy path + Local / Dropped
//!     pass-through. The Err(SendFailed) rollback path needs an
//!     artificial socket failure and is left as a follow-up.
//!   * `send_to` / `recv_from` round-trip.
//!   * `reset_stats` zeros every counter.

#![cfg(feature = "net")]

use std::net::SocketAddr;
use std::time::Duration;

use bytes::Bytes;

use net::adapter::net::{
    ForwardResult, HopStats, MultiHopPacketBuilder, NetProxy, ProxyConfig, ProxyError,
};

const LOCAL_BIND: &str = "127.0.0.1:0";

fn cfg(local_id: u64) -> ProxyConfig {
    ProxyConfig::new(local_id, LOCAL_BIND.parse().unwrap())
}

#[test]
fn hop_stats_avg_latency_returns_zero_with_no_samples() {
    let stats = HopStats::new();
    assert_eq!(
        stats.avg_latency_ns(),
        0,
        "avg over zero samples must return 0, not divide-by-zero"
    );
}

#[tokio::test]
async fn proxy_local_addr_resolves_bound_port() {
    let proxy = NetProxy::new(cfg(0x1111)).await.unwrap();
    let addr = proxy
        .local_addr()
        .expect("local_addr must succeed on a freshly-bound proxy");
    assert_eq!(
        addr.ip(),
        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
    );
    assert_ne!(addr.port(), 0, "ephemeral bind must resolve to a real port");
}

#[tokio::test]
async fn forward_drops_packet_too_small() {
    let proxy = NetProxy::new(cfg(0x1234)).await.unwrap();
    // ROUTING_HEADER_SIZE = 18 (see route.rs:21). A single byte
    // is comfortably below the threshold.
    let too_small = Bytes::from_static(&[0u8]);
    match proxy.forward(too_small) {
        ForwardResult::Dropped(ProxyError::PacketTooSmall) => {}
        other => panic!("expected Dropped(PacketTooSmall), got {other:?}"),
    }
    assert_eq!(proxy.stats().packets_dropped, 1);
}

#[tokio::test]
async fn forward_drops_invalid_header() {
    let proxy = NetProxy::new(cfg(0x1234)).await.unwrap();
    // Provide exactly ROUTING_HEADER_SIZE bytes of zeros — the
    // length gate passes, but RoutingHeader::from_bytes rejects
    // because the magic / version / etc fields don't match.
    let bogus = Bytes::from(vec![0u8; 18]);
    match proxy.forward(bogus) {
        ForwardResult::Dropped(ProxyError::InvalidHeader) => {}
        other => panic!("expected Dropped(InvalidHeader), got {other:?}"),
    }
    assert_eq!(proxy.stats().packets_dropped, 1);
}

#[tokio::test]
async fn forward_drops_ttl_expired_after_decrement() {
    // Start with TTL=1; the pre-decrement `is_expired()` check
    // passes (1 > 0), then `forward()` decrements to 0 and the
    // post-decrement `new_header.is_expired()` trip fires the
    // second-stage TtlExpired drop at proxy.rs:303-306.
    let proxy = NetProxy::new(cfg(0x1234)).await.unwrap();
    let next_hop: SocketAddr = "127.0.0.1:9001".parse().unwrap();
    proxy.add_route(0x5678, next_hop);

    let builder = MultiHopPacketBuilder::new(0xABCD);
    let packet = builder.build(0x5678, 1, b"post-decrement-zero");

    match proxy.forward(packet) {
        ForwardResult::Dropped(ProxyError::TtlExpired) => {}
        other => panic!("expected Dropped(TtlExpired) after decrement, got {other:?}"),
    }
    assert_eq!(proxy.stats().packets_dropped, 1);
}

#[tokio::test]
async fn forward_and_send_happy_path_round_trips_to_next_hop() {
    // Bind a second socket as the "next hop"; forward_and_send
    // should actually deliver the packet over loopback.
    let listener = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let next_hop = listener.local_addr().unwrap();

    let proxy = NetProxy::new(cfg(0x1234)).await.unwrap();
    proxy.add_route(0x5678, next_hop);

    let builder = MultiHopPacketBuilder::new(0xABCD);
    let packet = builder.build(0x5678, 8, b"forward-and-send");

    let result = proxy
        .forward_and_send(packet)
        .await
        .expect("forward_and_send must succeed against a bound next-hop");
    match result {
        ForwardResult::Forwarded { next_hop: hop, .. } => assert_eq!(hop, next_hop),
        other => panic!("expected Forwarded, got {other:?}"),
    }

    // Drain the listener with a small timeout to prove the bytes
    // actually crossed the wire.
    let mut buf = vec![0u8; 256];
    let (len, _from) = tokio::time::timeout(Duration::from_secs(2), listener.recv_from(&mut buf))
        .await
        .expect("listener recv timed out")
        .expect("listener recv I/O");
    assert!(len > 0, "next hop did not receive any bytes");
}

#[tokio::test]
async fn forward_and_send_passes_local_delivery_through_unchanged() {
    // forward_and_send's `ForwardResult::Local(payload)` arm at
    // proxy.rs:384 should return the same Local variant the
    // synchronous `forward()` produced — no network involvement.
    let proxy = NetProxy::new(cfg(0x1234)).await.unwrap();

    let builder = MultiHopPacketBuilder::new(0xABCD);
    let packet = builder.build(0x1234, 8, b"local-pass-through");

    let result = proxy.forward_and_send(packet).await.expect("local Ok");
    match result {
        ForwardResult::Local(payload) => assert_eq!(&payload[..], b"local-pass-through"),
        other => panic!("expected Local, got {other:?}"),
    }
}

#[tokio::test]
async fn forward_and_send_surfaces_dropped_as_error() {
    // `forward()` returning `Dropped(e)` must surface as `Err(e)`
    // from `forward_and_send` (proxy.rs:385). Easiest trigger:
    // a too-small packet → PacketTooSmall.
    let proxy = NetProxy::new(cfg(0x1234)).await.unwrap();
    let err = proxy
        .forward_and_send(Bytes::from_static(&[0u8]))
        .await
        .expect_err("PacketTooSmall must surface as Err");
    assert!(matches!(err, ProxyError::PacketTooSmall), "got {err:?}");
}

#[tokio::test]
async fn send_to_and_recv_from_round_trip() {
    // Two proxies on loopback; A.send_to(B) → B.recv_from(A).
    let a = NetProxy::new(cfg(0xAAAA)).await.unwrap();
    let b = NetProxy::new(cfg(0xBBBB)).await.unwrap();

    let a_addr = a.local_addr().unwrap();
    let b_addr = b.local_addr().unwrap();

    let payload = b"send_to-recv_from-smoke";
    let sent = a
        .send_to(payload, b_addr)
        .await
        .expect("send_to must succeed on loopback");
    assert_eq!(sent, payload.len());

    let mut buf = vec![0u8; 256];
    let (len, peer) = tokio::time::timeout(Duration::from_secs(2), b.recv_from(&mut buf))
        .await
        .expect("recv_from timed out")
        .expect("recv_from I/O");
    assert_eq!(len, payload.len());
    assert_eq!(&buf[..len], payload);
    assert_eq!(peer, a_addr);
}

#[tokio::test]
async fn reset_stats_zeros_every_counter() {
    let proxy = NetProxy::new(cfg(0x1234)).await.unwrap();

    // Generate a couple of forwards + drops to bump every counter.
    proxy.add_route(0x5678, "127.0.0.1:9001".parse().unwrap());
    let builder = MultiHopPacketBuilder::new(0xABCD);
    let _ = proxy.forward(builder.build(0x5678, 8, b"hi"));
    let _ = proxy.forward(builder.build(0x5678, 0, b"expired")); // drop
    let _ = proxy.forward(Bytes::from_static(&[0u8])); // packet-too-small drop

    let stats_before = proxy.stats();
    assert!(
        stats_before.packets_received > 0,
        "precondition: counters bumped"
    );

    proxy.reset_stats();

    let stats_after = proxy.stats();
    assert_eq!(stats_after.packets_received, 0);
    assert_eq!(stats_after.packets_forwarded, 0);
    assert_eq!(stats_after.packets_dropped, 0);
    assert_eq!(stats_after.bytes_forwarded, 0);
    assert_eq!(
        stats_after.avg_latency_ns, 0,
        "avg_latency_ns must report 0 once the sample counter is reset (HopStats:97 branch)"
    );
}
