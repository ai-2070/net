//! Dual-stack anchor, slice 1: the driver speaks IPv4 and IPv6 at once
//! (`docs/internal/plans/ANCHOR_DUAL_STACK_PLAN.md`).
//!
//! An anchor offers browsers one host candidate per RTC socket, and a
//! browser can only pair with the families its network routes. These
//! witnesses are on the driver: the second socket, its
//! `IPV6_V6ONLY` option, a candidate per family on every session, one
//! session of each family live at once with application data both
//! ways, and an IPv4-only anchor that still offers exactly one
//! candidate.
//!
//! Run: `cargo nextest run --features "webrtc fixtures cortex nat-traversal" --test rtc_dual_stack`
#![cfg(all(feature = "webrtc", feature = "fixtures"))]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use net::adapter::net::rtc::{RtcConfig, RtcDriver, RtcDriverHandle, RtcPeerId, RtcStats};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};
use str0m::Candidate;
use tokio::sync::mpsc;

/// A driver and its outputs, kept alive for the test's duration.
#[derive(Debug)]
struct Driven {
    handle: RtcDriverHandle,
    ingress: mpsc::Receiver<(Bytes, RtcPeerId)>,
    _closed: mpsc::Receiver<RtcPeerId>,
}

async fn spawn(config: RtcConfig, net_bind: &str) -> std::io::Result<Driven> {
    let (ingress_tx, ingress_rx) = mpsc::channel(64);
    let (closed_tx, closed_rx) = mpsc::channel(16);
    let handle = RtcDriver::spawn(
        config,
        net_bind.parse().expect("addr"),
        Arc::new(RtcStats::default()),
        ingress_tx,
        closed_tx,
    )
    .await?;
    Ok(Driven {
        handle,
        ingress: ingress_rx,
        _closed: closed_rx,
    })
}

fn addr(s: &str) -> SocketAddr {
    s.parse().expect("addr")
}

/// A dual-stack anchor on loopback: IPv4 primary, IPv6 second socket.
fn dual_stack_loopback() -> RtcConfig {
    RtcConfig::new()
        .with_bind_addr(addr("127.0.0.1:0"))
        .with_bind_addr_v6(addr("[::1]:0"))
}

/// The host-candidate addresses an SDP blob offers.
fn sdp_candidates(sdp: &str) -> Vec<SocketAddr> {
    sdp.lines()
        .filter_map(|line| line.strip_prefix("a="))
        .filter(|line| line.starts_with("candidate:"))
        .filter_map(|line| Candidate::from_sdp_string(line).ok())
        .map(|c| c.addr())
        .collect()
}

/// Open one session between `client` (the offerer) and `anchor`,
/// signalled in-process, and return `(client's id, anchor's id,
/// the anchor's answer SDP)`.
async fn open(client: &Driven, anchor: &Driven) -> (RtcPeerId, RtcPeerId, String) {
    let (id_client, offer) = client.handle.create_offer().await.expect("offer");
    let (id_anchor, answer) = anchor.handle.accept_offer(offer).await.expect("answer");
    client
        .handle
        .accept_answer(id_client, answer.clone())
        .await
        .expect("accept answer");
    // Trickle both ways, as the loopback harness does: every anchor
    // candidate to the client (the client pairs the family it has),
    // the client's one candidate to the anchor.
    for candidate_addr in sdp_candidates(&answer) {
        let candidate = Candidate::host(candidate_addr, "udp")
            .expect("anchor candidate")
            .to_sdp_string();
        client
            .handle
            .remote_candidate(id_client, candidate)
            .await
            .expect("client applies anchor candidate");
    }
    let client_candidate = Candidate::host(client.handle.local_addr(), "udp")
        .expect("client candidate")
        .to_sdp_string();
    anchor
        .handle
        .remote_candidate(id_anchor, client_candidate)
        .await
        .expect("anchor applies client candidate");
    (id_client, id_anchor, answer)
}

/// Wait for one datagram on `rx` and return it with its session.
async fn next_ingress(rx: &mut mpsc::Receiver<(Bytes, RtcPeerId)>) -> (Bytes, RtcPeerId) {
    tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("ingress within the deadline")
        .expect("ingress channel open")
}

/// A port free on BOTH wildcards right now, so the dual-stack anchor
/// can be asked to bind the same port in each family.
fn a_port_free_on_both_wildcards() -> u16 {
    for _ in 0..32 {
        let v4 = std::net::UdpSocket::bind("0.0.0.0:0").expect("probe v4 bind");
        let port = v4.local_addr().expect("probe addr").port();
        let v6 = socket2::Socket::new(
            socket2::Domain::IPV6,
            socket2::Type::DGRAM,
            Some(socket2::Protocol::UDP),
        )
        .expect("probe v6 socket");
        v6.set_only_v6(true).expect("probe v6only");
        let free_v6 = v6.bind(&addr(&format!("[::]:{port}")).into()).is_ok();
        drop(v6);
        drop(v4);
        if free_v6 {
            return port;
        }
    }
    panic!("no port was free on both wildcards in 32 tries");
}

/// **`IPV6_V6ONLY` is checked, not inferred.**
///
/// Binding `127.0.0.1:P` beside `[::1]:P` never collides, whatever
/// the option says, so it proves nothing. Two things do: the option
/// read back off the socket, and the **wildcards** `0.0.0.0:P` and
/// `[::]:P` bound together — on Linux a `[::]` socket without
/// `IPV6_V6ONLY` is dual-stack and claims the IPv4 port too, so the
/// second bind fails with `AddrInUse`.
///
/// Inverse: drop `set_only_v6(true)` from `bind_v6_only` and this
/// fails on Linux (CI), where an IPv6 socket defaults to dual-stack:
/// on the read-back and on the spawn. Windows defaults to
/// IPv6-only, so there the mutation is invisible — which is exactly
/// why the option is set explicitly rather than left to the host.
#[tokio::test]
async fn the_ipv6_socket_is_ipv6_only_and_shares_a_wildcard_port_with_ipv4() {
    let port = a_port_free_on_both_wildcards();
    let anchor = spawn(
        RtcConfig::new()
            .with_bind_addr(addr(&format!("0.0.0.0:{port}")))
            .with_bind_addr_v6(addr(&format!("[::]:{port}"))),
        "127.0.0.1:0",
    )
    .await
    .expect("both wildcards bind the same port once the IPv6 socket is IPv6-only");

    assert_eq!(
        anchor.handle.v6_only(),
        Some(true),
        "IPV6_V6ONLY must be set on the IPv6 socket, read back off the socket itself"
    );
    assert_eq!(anchor.handle.local_addr().port(), port);
    assert_eq!(anchor.handle.local_addr_v6().map(|a| a.port()), Some(port));
    anchor.handle.shutdown_and_join().await;
}

/// The flagship driver witness: one anchor, an IPv4 client and an
/// IPv6 client, **both sessions live at the same time**, each on its
/// own family, application data observed by the receiver in both
/// directions on both.
///
/// Inverses, run on 2026-09-28: route every transmit through the
/// primary socket (drop `for_destination`'s IPv6 arm) and the IPv6
/// session never opens; offer only the primary candidate
/// (`new_session`) and the answer lacks the IPv6 one. **Not
/// witnessed:** stamping every arrival with the primary's address
/// instead of its own socket's survived — str0m accepted the IPv6
/// checks anyway — so the per-socket stamp is kept because it is the
/// correct local address, not because this test depends on it.
#[tokio::test]
async fn a_dual_stack_anchor_serves_an_ipv4_and_an_ipv6_client_at_once() {
    let mut anchor = spawn(dual_stack_loopback(), "127.0.0.1:0")
        .await
        .expect("dual-stack anchor");
    let anchor_v4 = anchor.handle.local_addr();
    let anchor_v6 = anchor.handle.local_addr_v6().expect("an IPv6 socket");
    assert!(anchor_v4.is_ipv4() && anchor_v6.is_ipv6());

    let mut client4 = spawn(RtcConfig::new(), "127.0.0.1:0")
        .await
        .expect("ipv4 client");
    let mut client6 = spawn(RtcConfig::new(), "[::1]:0")
        .await
        .expect("ipv6 client");

    // Both sessions opened before either carries data: concurrent,
    // not one family after the other.
    let (c4, a4, answer4) = open(&client4, &anchor).await;
    let (c6, a6, answer6) = open(&client6, &anchor).await;
    for answer in [&answer4, &answer6] {
        let offered = sdp_candidates(answer);
        assert!(
            offered.contains(&anchor_v4) && offered.contains(&anchor_v6),
            "every session answer offers a host candidate per family, got {offered:?}"
        );
    }
    for (driver, id) in [
        (&client4.handle, c4),
        (&anchor.handle, a4),
        (&client6.handle, c6),
        (&anchor.handle, a6),
    ] {
        driver.await_open(id).await.expect("DataChannel opens");
    }

    // Each session selected the pair of its client's family, and the
    // anchor's local half is the socket of that family.
    let (local4, remote4, _) = anchor
        .handle
        .selected_pair(a4)
        .await
        .expect("ipv4 session has a pair");
    let (local6, remote6, _) = anchor
        .handle
        .selected_pair(a6)
        .await
        .expect("ipv6 session has a pair");
    assert_eq!(remote4, client4.handle.local_addr());
    assert_eq!(local4, anchor_v4);
    assert_eq!(remote6, client6.handle.local_addr());
    assert_eq!(local6, anchor_v6);

    // Application data, observed by the receiver, both directions,
    // both families.
    for (client, client_id, anchor_id, tag) in
        [(&mut client4, c4, a4, "v4"), (&mut client6, c6, a6, "v6")]
    {
        let up = format!("up-{tag}");
        client
            .handle
            .transport()
            .submit(up.as_bytes(), client_id)
            .expect("client submits");
        let (bytes, from) = next_ingress(&mut anchor.ingress).await;
        assert_eq!(
            from, anchor_id,
            "the {tag} payload arrives on the {tag} session"
        );
        assert_eq!(&bytes[..], up.as_bytes());

        let down = format!("down-{tag}");
        anchor
            .handle
            .transport()
            .submit(down.as_bytes(), anchor_id)
            .expect("anchor submits");
        let (bytes, from) = next_ingress(&mut client.ingress).await;
        assert_eq!(from, client_id);
        assert_eq!(&bytes[..], down.as_bytes());
    }

    for driver in [&client4.handle, &client6.handle, &anchor.handle] {
        driver.shutdown_and_join().await;
    }
}

/// **IPv4-only is unchanged in behaviour.** An anchor configured
/// without an IPv6 socket binds none and offers exactly one host
/// candidate — its own address — in the session answer.
///
/// Not "the answer is byte-identical": every answer carries fresh
/// ICE credentials and a DTLS fingerprint, so no two ever are.
#[tokio::test]
async fn an_ipv4_only_anchor_offers_exactly_one_candidate() {
    let anchor = spawn(
        RtcConfig::new().with_bind_addr(addr("127.0.0.1:0")),
        "127.0.0.1:0",
    )
    .await
    .expect("ipv4-only anchor");
    assert_eq!(anchor.handle.local_addr_v6(), None);
    assert_eq!(anchor.handle.v6_only(), None);

    let client = spawn(RtcConfig::new(), "127.0.0.1:0")
        .await
        .expect("client");
    let (_, _, answer) = open(&client, &anchor).await;
    assert_eq!(
        sdp_candidates(&answer),
        vec![anchor.handle.local_addr()],
        "one socket, one candidate"
    );
    client.handle.shutdown_and_join().await;
    anchor.handle.shutdown_and_join().await;
}

/// The trickle frame and the bootstrap candidate follow the sockets:
/// one per family, primary first, carrying the public override when
/// one is configured — and one, unchanged, on an IPv4-only anchor.
#[tokio::test]
async fn the_trickled_candidates_are_one_per_family_primary_first() {
    let public_v6 = addr("[2001:db8::7]:7101");
    let dual = node(dual_stack_loopback().with_public_addr_v6(public_v6)).await;
    let driver = dual.rtc_driver().expect("rtc driver");
    let primary = driver.local_addr();
    assert_eq!(
        dual.rtc_advertised_addrs(),
        vec![primary, public_v6],
        "primary first, then the IPv6 socket as advertised (the override, not the bind)"
    );
    let candidates: Vec<SocketAddr> = dual
        .bootstrap_host_candidates()
        .iter()
        .map(|c| Candidate::from_sdp_string(c).expect("sdp candidate").addr())
        .collect();
    assert_eq!(candidates, vec![primary, public_v6]);
    assert_eq!(
        dual.bootstrap_host_candidate()
            .map(|c| Candidate::from_sdp_string(&c).expect("sdp").addr()),
        Some(primary),
        "the single-candidate accessor is the primary, as before"
    );

    let plain = node(RtcConfig::new().with_bind_addr(addr("127.0.0.1:0"))).await;
    assert_eq!(plain.bootstrap_host_candidates().len(), 1);
    assert_eq!(
        plain.rtc_advertised_addrs(),
        vec![plain.rtc_driver().expect("rtc driver").local_addr()]
    );
}

/// A misconfigured IPv6 socket refuses the spawn: it would otherwise
/// advertise an endpoint nothing answers on, or split one family
/// across two sockets.
#[tokio::test]
async fn a_misconfigured_ipv6_socket_refuses_the_spawn() {
    let cases = [
        (
            "an IPv4 address as the IPv6 bind",
            RtcConfig::new().with_bind_addr_v6(addr("127.0.0.1:0")),
            "127.0.0.1:0",
        ),
        (
            "an IPv6 override with no IPv6 socket",
            RtcConfig::new().with_public_addr_v6(addr("[2001:db8::1]:7101")),
            "127.0.0.1:0",
        ),
        (
            "a primary that resolves to IPv6 from the Net socket",
            RtcConfig::new().with_bind_addr_v6(addr("[::1]:0")),
            "[::1]:0",
        ),
        (
            "an explicit IPv6 primary",
            RtcConfig::new()
                .with_bind_addr(addr("[::1]:0"))
                .with_bind_addr_v6(addr("[::1]:0")),
            "127.0.0.1:0",
        ),
    ];
    for (what, config, net_bind) in cases {
        let err = spawn(config, net_bind)
            .await
            .expect_err(&format!("{what} must be refused"));
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::InvalidInput,
            "{what}: refused as a configuration error, got {err}"
        );
    }
}

async fn node(rtc: RtcConfig) -> Arc<MeshNode> {
    let mut cfg = MeshNodeConfig::new(addr("127.0.0.1:0"), [0x44u8; 32]);
    cfg.socket_buffers = SocketBufferConfig::for_testing();
    cfg.rtc = Some(rtc);
    Arc::new(
        MeshNode::new(EntityKeypair::generate(), cfg)
            .await
            .expect("MeshNode::new"),
    )
}
