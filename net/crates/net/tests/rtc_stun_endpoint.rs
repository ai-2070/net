//! Stage 6: the anchor's **separately announced STUN endpoint**.
//!
//! `RtcConfig::stun_addr` binds a second UDP socket that answers STUN
//! and carries nothing else, so an anchor can announce a STUN endpoint
//! that is not its own ICE address. libwebrtc's
//! `UDPPort::OnReadPacket` consumes any datagram from a configured
//! STUN server as a STUN-server response before `GetConnection` looks
//! for a candidate pair, in both directions — which is why the two
//! roles need two tuples rather than a document telling integrators
//! not to collide them.
//!
//! These witnesses are on the driver itself: the socket, its address,
//! and the RTC socket's unchanged behaviour beside it. The
//! announcement, the leaf default and the collision check are their
//! own slices.

#![cfg(feature = "webrtc")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use net::adapter::net::rtc::{
    parse_xor_mapped_address, RtcConfig, RtcDriver, RtcDriverHandle, RtcPeerId, RtcStats,
    STUN_MAGIC_COOKIE,
};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

/// The driver's outputs, kept alive for the test's duration: a
/// dropped receiver would turn every driver send into an error and
/// make the witness about channel teardown instead of STUN.
#[derive(Debug)]
struct Driven {
    handle: RtcDriverHandle,
    _ingress: mpsc::Receiver<(Bytes, RtcPeerId)>,
    _closed: mpsc::Receiver<RtcPeerId>,
}

async fn spawn(config: RtcConfig) -> std::io::Result<Driven> {
    let (ingress_tx, ingress_rx) = mpsc::channel(16);
    let (closed_tx, closed_rx) = mpsc::channel(16);
    let handle = RtcDriver::spawn(
        config,
        "127.0.0.1:0".parse().expect("addr"),
        Arc::new(RtcStats::default()),
        ingress_tx,
        closed_tx,
    )
    .await?;
    Ok(Driven {
        handle,
        _ingress: ingress_rx,
        _closed: closed_rx,
    })
}

fn loopback_ephemeral() -> SocketAddr {
    "127.0.0.1:0".parse().expect("addr")
}

/// An unsolicited RFC 5389 binding request: no attributes, so it is a
/// gathering request and not an ICE connectivity check.
fn binding_request(txid: u8) -> Vec<u8> {
    let mut request = Vec::with_capacity(20);
    request.extend_from_slice(&0x0001u16.to_be_bytes()); // Binding Request
    request.extend_from_slice(&0u16.to_be_bytes()); // no attributes
    request.extend_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
    request.extend_from_slice(&[txid; 12]);
    request
}

/// Send one binding request to `target` from a throwaway socket and
/// return the XOR-MAPPED-ADDRESS it is told, plus the socket's own
/// address.
async fn probe(target: SocketAddr, txid: u8) -> Option<(SocketAddr, SocketAddr)> {
    let client = UdpSocket::bind(loopback_ephemeral())
        .await
        .expect("client socket");
    let mine = client.local_addr().expect("client addr");
    client
        .send_to(&binding_request(txid), target)
        .await
        .expect("send the binding request");
    let mut buf = [0u8; 256];
    let (n, from) = tokio::time::timeout(Duration::from_secs(5), client.recv_from(&mut buf))
        .await
        .ok()?
        .expect("recv");
    assert_eq!(from, target, "the answer must come from the target socket");
    parse_xor_mapped_address(&buf[..n]).map(|mapped| (mapped, mine))
}

/// The second socket answers a binding request with the requester's
/// own address, and it does so **without** `serve_stun`: the flag
/// governs the RTC socket, this endpoint is its own thing.
///
/// Inverse: drop the `stun_addr` arm from `RtcDriver::spawn` (bind
/// nothing extra) — `stun_local_addr()` is `None` and the unwrap
/// fails before a packet is sent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_stun_only_socket_answers_a_binding_request() {
    let driven = spawn(RtcConfig {
        serve_stun: false,
        ..RtcConfig::new()
            .with_bind_addr(loopback_ephemeral())
            .with_stun_addr(loopback_ephemeral())
    })
    .await
    .expect("spawn with a STUN endpoint");

    let stun_addr = driven
        .handle
        .stun_local_addr()
        .expect("a configured STUN bind is resolved post-bind");
    assert_ne!(
        stun_addr.port(),
        0,
        "a `:0` configuration must report the port it actually took, \
         not the wildcard it asked for"
    );

    let (mapped, client) = probe(stun_addr, 0xA5)
        .await
        .expect("the STUN endpoint must answer a binding request");
    assert_eq!(
        mapped, client,
        "XOR-MAPPED-ADDRESS must describe the requester's own reflexive address"
    );

    // R3-B for the second binding: a joined shutdown releases the
    // port, so a successor can rebind an explicit STUN address.
    driven.handle.shutdown_and_join().await;
    UdpSocket::bind(stun_addr)
        .await
        .expect("the STUN port must be free once shutdown has joined");
}

/// The RTC socket is untouched: with `serve_stun` it still answers
/// the diagnostic probe that `UdpBlocked` evidence rests on, and that
/// counter stays specific to `rtc_addr` — traffic to the STUN
/// endpoint is a different published endpoint and is not charged to
/// it.
///
/// Inverse: point the STUN-only loop's responder at the RTC counter
/// (`stats.note_stun_binding_request()` in `stun_only_loop`) — the
/// second `assert_eq!` reads 2 and fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_rtc_socket_still_answers_its_own_probe() {
    let driven = spawn(RtcConfig {
        serve_stun: true,
        ..RtcConfig::new()
            .with_bind_addr(loopback_ephemeral())
            .with_stun_addr(loopback_ephemeral())
    })
    .await
    .expect("spawn with both");
    let stats = Arc::clone(driven.handle.stats());

    let rtc_addr = driven.handle.local_addr();
    let (mapped, client) = probe(rtc_addr, 0xB6)
        .await
        .expect("the diagnostic probe target must answer");
    assert_eq!(
        mapped, client,
        "the RTC socket's responder is unchanged: the requester's own address"
    );
    assert_eq!(
        stats.stun_binding_requests(),
        1,
        "a peer aiming at the published `rtc_addr` is what this counter means"
    );

    // The same request to the STUN endpoint is answered too, and is
    // not counted as an `rtc_addr` hit.
    let stun_addr = driven.handle.stun_local_addr().expect("stun addr");
    let (mapped, client) = probe(stun_addr, 0xC7)
        .await
        .expect("the STUN endpoint must answer");
    assert_eq!(mapped, client);
    assert_eq!(
        stats.stun_binding_requests(),
        1,
        "the STUN endpoint is a different published endpoint; charging its \
         traffic to `rtc_addr` would make the `UdpBlocked` observable ambiguous"
    );

    driven.handle.shutdown_and_join().await;
}

/// Kyra's acceptance boundary 1, in-process half: the announced STUN
/// endpoint is a **distinct** tuple from the announced RTC endpoint.
/// Two ports on one host in one process is the intended shape; what
/// is ruled out is one socket in both roles.
///
/// Inverse: return `Some(self.local_addr)` from `stun_local_addr()`
/// (the "reuse the RTC socket" shortcut) — the inequality fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_two_announced_endpoints_are_different_sockets() {
    let driven = spawn(
        RtcConfig::new()
            .with_bind_addr(loopback_ephemeral())
            .with_stun_addr(loopback_ephemeral()),
    )
    .await
    .expect("spawn with a STUN endpoint");

    let rtc_addr = driven.handle.local_addr();
    let stun_addr = driven.handle.stun_local_addr().expect("stun addr");
    assert_ne!(
        rtc_addr, stun_addr,
        "the STUN endpoint must not be the anchor's ICE address — that is the \
         configuration libwebrtc's receive dispatch breaks"
    );
    // And it is not an adjacent-port guess either: the value is the
    // bind's own truth, whatever the OS gave us.
    assert_ne!(stun_addr.port(), 0);

    driven.handle.shutdown_and_join().await;
}

/// Off unless configured, and configured means *bound*: a contested
/// address fails the spawn rather than announcing an endpoint nothing
/// is listening on, and with `stun_addr: None` the driver takes no
/// second port and has nothing to announce.
///
/// Inverse: make the bind non-fatal (`if let Ok(sock) = ...`) — the
/// `AddrInUse` expectation fails, and an anchor would publish a dead
/// endpoint.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unconfigured_stun_endpoint_binds_nothing() {
    // Hold a port, then ask the driver for it.
    let squatter = UdpSocket::bind(loopback_ephemeral())
        .await
        .expect("squatter socket");
    let contested = squatter.local_addr().expect("squatter addr");

    let err = spawn(
        RtcConfig::new()
            .with_bind_addr(loopback_ephemeral())
            .with_stun_addr(contested),
    )
    .await
    .expect_err("a STUN bind that cannot be taken must fail the spawn");
    assert_eq!(
        err.kind(),
        std::io::ErrorKind::AddrInUse,
        "the second socket is really bound, not assumed: {err}"
    );

    // Same squatter still holding the port: nothing is configured, so
    // the driver binds no second socket and announces nothing.
    let driven = spawn(RtcConfig::new().with_bind_addr(loopback_ephemeral()))
        .await
        .expect("spawn without a STUN endpoint");
    assert_eq!(
        driven.handle.stun_local_addr(),
        None,
        "no configuration, no endpoint: emission has nothing to announce"
    );
    assert_eq!(
        squatter.local_addr().expect("squatter addr"),
        contested,
        "the port the test holds is still the test's"
    );

    driven.handle.shutdown_and_join().await;
}

/// §6.12.1's own configuration, refused at the door: announcing one
/// endpoint as both the RTC address and the STUN address. Two
/// perfectly distinct sockets underneath do not save it — a peer
/// acts on what was announced, and its only symptom would be an ICE
/// timeout with no diagnostic.
///
/// Inverse: delete the `stun_public_addr == public_addr` arm of
/// `RtcConfig::stun_endpoint_conflict` — the spawn succeeds and
/// `expect_err` fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn announcing_one_endpoint_for_both_roles_is_refused() {
    let announced: SocketAddr = "203.0.113.7:7101".parse().expect("addr");
    let err = spawn(RtcConfig {
        public_addr: Some(announced),
        stun_public_addr: Some(announced),
        ..RtcConfig::new()
            .with_bind_addr(loopback_ephemeral())
            .with_stun_addr(loopback_ephemeral())
    })
    .await
    .expect_err("one announced endpoint in both roles must be refused");

    assert_eq!(
        err.kind(),
        std::io::ErrorKind::InvalidInput,
        "a configuration error, not an I/O accident: {err}"
    );
    let text = err.to_string();
    assert!(
        text.contains("stun_public_addr") && text.contains("public_addr"),
        "the error must name both fields: {text}"
    );
    assert!(
        text.contains("203.0.113.7:7101"),
        "the error must name the colliding value: {text}"
    );
    assert!(
        text.contains("distinct UDP endpoints") && text.contains("own STUN server"),
        "the error must say what is wrong and why: {text}"
    );
}

/// The same mistake one level down: one bind for both sockets. It is
/// caught as a configuration error rather than surfacing as the
/// `AddrInUse` the second bind would otherwise produce — which names
/// a port, not the mistake.
///
/// Inverse: delete the `stun_addr == bind_addr` arm — the spawn then
/// fails with `AddrInUse` and the `InvalidInput` assertion fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn binding_one_socket_for_both_roles_is_refused() {
    // An explicit port, because that is what makes the collision
    // detectable: two `:0` binds are two different sockets.
    let shared: SocketAddr = "127.0.0.1:34791".parse().expect("addr");
    let err = spawn(
        RtcConfig::new()
            .with_bind_addr(shared)
            .with_stun_addr(shared),
    )
    .await
    .expect_err("one bind for both sockets must be refused");

    assert_eq!(
        err.kind(),
        std::io::ErrorKind::InvalidInput,
        "the configuration is named, not the port: {err}"
    );
    let text = err.to_string();
    assert!(
        text.contains("stun_addr")
            && text.contains("bind_addr")
            && text.contains("127.0.0.1:34791"),
        "the error must name both fields and the colliding value: {text}"
    );
}

/// The negative: distinct values in both pairs are the ordinary
/// case, and the check refuses nothing it should not — the same IP
/// on different announced ports, which is the intended shape, and
/// two `ip:0` binds, which compare equal and are nonetheless two
/// different sockets because port 0 names no endpoint.
///
/// Inverse: drop the `rtc.port() != 0` exemption from
/// `stun_endpoint_conflict` — the two wildcard binds are read as a
/// collision and this spawn is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn distinct_endpoints_are_not_refused() {
    let driven = spawn(RtcConfig {
        public_addr: Some("203.0.113.7:7101".parse().expect("addr")),
        stun_public_addr: Some("203.0.113.7:3478".parse().expect("addr")),
        ..RtcConfig::new()
            .with_bind_addr("127.0.0.1:0".parse().expect("addr"))
            .with_stun_addr("127.0.0.1:0".parse().expect("addr"))
    })
    .await
    .expect("two ports on one host is the intended configuration");
    assert!(
        driven.handle.stun_local_addr().is_some(),
        "the STUN socket must still be bound"
    );
    driven.handle.shutdown_and_join().await;
}
