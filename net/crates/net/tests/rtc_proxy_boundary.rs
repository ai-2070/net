//! F7 — `proxy.rs:349` `NetProxy::forward_and_send` — is the one
//! forwarding site in the S0e F1–F7 inventory that carries **no**
//! admission gate. `docs/internal/spikes/S0E_BOOTSTRAP_FRAMES.md`
//! row F7 records the gap ("proxy route table only"), and
//! `S4A_REPORT.md` justifies leaving it ungated by *non-reachability*:
//! "the proxy forwards from its own route table and has no view of
//! the mesh's peer map or of the `ProvisionalEndpoints` projection;
//! **no RTC peer reaches it today**."
//!
//! That justification is a claim about a missing edge, and until now
//! it existed only as prose. This file is the witness for it.
//!
//! **What the boundary is.** `NetProxy` is a standalone component:
//! its own `UdpSocket`, its own next-hop table, no `MeshNode` handle
//! and — the load-bearing half — no `MeshNode` holds *it*. Its only
//! two ingresses are (a) datagrams arriving on its own socket, which
//! a standalone deployment pumps into `forward_and_send`, and (b) a
//! direct call by whoever owns the value. Mesh dispatch owns no
//! `NetProxy`, so an RTC frame — DataChannel → driver → dispatch —
//! has no edge to traverse.
//!
//! **What the witness observes.** A real `NetProxy` stood up as the
//! anchor's sidecar: `local_id` = the anchor's own node id, a route
//! already installed for the exact third-party destination the RTC
//! peer names, a live `recv_from` → `forward_and_send` loop for the
//! whole test, and a real next-hop socket behind it. So if any seam
//! from the anchor's dispatch to F7 existed, the drive below would
//! land a datagram on the sink. Three observables, all of which
//! exist today (nothing was added to `proxy.rs`): the count of
//! datagrams read off the proxy's own socket, `NetProxy::stats()`,
//! and the next-hop sink's receive count.
//!
//! **What it does not claim.**
//!
//! * Nothing about a gate. There is no gate at F7 to test, and the
//!   zero here is *not* gate-derived: phase 2 re-drives the identical
//!   frame after `promote_admission`, when the F1 gate refuses
//!   nothing, and F7 is still not reached. The boundary is
//!   structural, and removing an admission gate would not move these
//!   counters — stated here so this file is never misread as
//!   enforcement coverage.
//! * Not that `NetProxy` is unreachable in general. A value-holder
//!   can call it, and the positive control does exactly that through
//!   the socket ingress — same proxy, same route, same routed-envelope
//!   shape, only the delivery path changed. That is what keeps the
//!   zero from being vacuous.
//! * Only *direct* ingress is covered by the first witness. A
//!   transitive chain — anchor relays a hop to a socket that happens
//!   to be a proxy — is out of reach for a different reason, pinned
//!   separately below: the relay path needs an authenticated
//!   adjacency, and the proxy cannot become one.
//!
//! Run: `cargo nextest run --features "webrtc fixtures cortex nat-traversal" --test rtc_proxy_boundary`
#![cfg(all(feature = "webrtc", feature = "fixtures"))]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
// The workspace disallows `std::sync::Mutex::lock`
// (clippy `disallowed_methods`): every lock here is parking_lot's.
use parking_lot::Mutex;
use net::adapter::net::rtc::{connect_rtc_loopback, RtcConfig};
use net::adapter::net::{
    EntityKeypair, MeshNode, MeshNodeConfig, MultiHopPacketBuilder, NetProxy, PeerAddr,
    ProxyConfig, RoutingHeader, SocketBufferConfig, ROUTING_HEADER_SIZE,
};
use tokio::net::UdpSocket;

const PSK: [u8; 32] = [0x5Cu8; 32];
const LOOPBACK: &str = "127.0.0.1:0";

/// The TTL `send_transit_probe_for_test` stamps on its routing
/// header (`mesh.rs:35012`). The positive control reuses it so the
/// two envelopes differ only in where they were sent.
const TRANSIT_TTL: u8 = 8;

fn config(rtc: Option<RtcConfig>) -> MeshNodeConfig {
    let mut cfg = MeshNodeConfig::new(LOOPBACK.parse().expect("addr"), PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5));
    cfg.socket_buffers = SocketBufferConfig::for_testing();
    cfg.rtc = rtc;
    cfg
}

async fn node(rtc: Option<RtcConfig>) -> Arc<MeshNode> {
    Arc::new(
        MeshNode::new(EntityKeypair::generate(), config(rtc))
            .await
            .expect("MeshNode::new"),
    )
}

fn rtc_config() -> RtcConfig {
    RtcConfig::new().with_bind_addr(LOOPBACK.parse().expect("addr"))
}

fn anchor_config() -> RtcConfig {
    RtcConfig {
        serve_bootstrap: true,
        ..rtc_config()
    }
}

async fn wait_for<F: Fn() -> bool>(predicate: F, within: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + within;
    while tokio::time::Instant::now() < deadline {
        if predicate() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    predicate()
}

/// An anchor (`serve_bootstrap`) and a browser stand-in joined over
/// a DataChannel, installed **provisional** — the 4a fixture shape,
/// unchanged.
async fn anchor_and_provisional_client() -> (
    Arc<MeshNode>,
    Arc<MeshNode>,
    net::adapter::net::rtc::RtcPeerId,
) {
    let anchor = node(Some(anchor_config())).await;
    let client = node(Some(rtc_config())).await;
    anchor.start_arc();
    client.start_arc();
    let (id_anchor, _id_client) = connect_rtc_loopback(&anchor, &client)
        .await
        .expect("DataChannel + Noise");
    assert!(
        anchor.peer_is_provisional(client.node_id()),
        "the premise: a browser-facing anchor installs an RTC session as provisional"
    );
    (anchor, client, id_anchor)
}

/// A live F7 deployment, positioned to catch any edge from the mesh.
///
/// `local_id` is the anchor's own node id and `dest_id` is the
/// destination the RTC peer will name, with a route installed — so
/// the *only* thing missing for a forward to happen is the frame.
struct ProxySidecar {
    proxy: Arc<NetProxy>,
    proxy_addr: SocketAddr,
    /// Datagrams read off the proxy's own socket. The rawest
    /// "anything at all arrived" observable there is: it precedes
    /// every decision `forward()` makes.
    socket_ingress: Arc<AtomicU64>,
    /// Datagrams the proxy actually put on the wire to the next hop.
    sink_hits: Arc<AtomicU64>,
    last_at_sink: Arc<Mutex<Vec<u8>>>,
}

impl ProxySidecar {
    async fn spawn(local_id: u64, dest_id: u64) -> Self {
        let sink = Arc::new(UdpSocket::bind(LOOPBACK).await.expect("bind next hop"));
        let sink_addr = sink.local_addr().expect("next-hop addr");

        let proxy = Arc::new(
            NetProxy::new(ProxyConfig::new(local_id, LOOPBACK.parse().expect("addr")))
                .await
                .expect("NetProxy::new"),
        );
        let proxy_addr = proxy.local_addr().expect("proxy addr");
        proxy.add_route(dest_id, sink_addr);

        let socket_ingress = Arc::new(AtomicU64::new(0));
        let sink_hits = Arc::new(AtomicU64::new(0));
        let last_at_sink = Arc::new(Mutex::new(Vec::new()));

        // F7 exactly as a standalone proxy runs it: socket ingress
        // straight into `forward_and_send`.
        {
            let proxy = Arc::clone(&proxy);
            let ingress = Arc::clone(&socket_ingress);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                while let Ok((n, _from)) = proxy.recv_from(&mut buf).await {
                    ingress.fetch_add(1, Ordering::SeqCst);
                    let _ = proxy
                        .forward_and_send(Bytes::copy_from_slice(&buf[..n]))
                        .await;
                }
            });
        }
        {
            let hits = Arc::clone(&sink_hits);
            let last = Arc::clone(&last_at_sink);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                while let Ok((n, _from)) = sink.recv_from(&mut buf).await {
                    *last.lock() = buf[..n].to_vec();
                    hits.fetch_add(1, Ordering::SeqCst);
                }
            });
        }

        Self {
            proxy,
            proxy_addr,
            socket_ingress,
            sink_hits,
            last_at_sink,
        }
    }

    /// Every F7 observable reads zero: nothing arrived at the proxy's
    /// socket, `forward()` was never entered, and the next hop is
    /// silent — while the route that would have carried a forward is
    /// demonstrably installed.
    fn assert_untouched(&self, phase: &str) {
        let stats = self.proxy.stats();
        assert_eq!(
            self.socket_ingress.load(Ordering::SeqCst),
            0,
            "{phase}: no datagram may reach the proxy's socket from the RTC path"
        );
        assert_eq!(
            stats.packets_received, 0,
            "{phase}: F7's entry counter must be untouched — an RTC frame never \
             reaches `forward()`"
        );
        assert_eq!(
            stats.packets_forwarded, 0,
            "{phase}: nothing may be forwarded on an RTC peer's behalf"
        );
        assert_eq!(
            stats.packets_dropped, 0,
            "{phase}: a drop would still mean the frame got *in* — F7 was entered"
        );
        assert_eq!(
            self.sink_hits.load(Ordering::SeqCst),
            0,
            "{phase}: the next hop must see nothing"
        );
        assert_eq!(
            stats.routes, 1,
            "{phase}: the route is installed, so a forward was possible — the zeros \
             above are the missing edge, not a missing route"
        );
    }
}

/// THE WITNESS: an RTC peer's routed envelope is adjudicated by the
/// mesh at F1 and never reaches F7 — provisional or promoted — while
/// the identical envelope delivered to the proxy's own socket is
/// received, forwarded and counted.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn an_rtc_peers_routed_envelope_never_reaches_the_proxy() {
    let (anchor, client, endpoint) = anchor_and_provisional_client().await;
    let client_id = client.node_id();
    let third_party = node(None).await;
    let dest = third_party.node_id();

    // The sidecar is the anchor's own id with the RTC peer's named
    // destination already routed: maximally reachable, if an edge
    // existed.
    let f7 = ProxySidecar::spawn(anchor.node_id(), dest).await;

    // ---- Phase 1: provisional.
    //
    // The drive is the frame family whose header is *the same codec*
    // F7 parses — a `RoutingHeader`-prefixed routed envelope with a
    // third-party `dest_id`, asked of the anchor over the DataChannel
    // the way a browser must ask.
    let refused_before = anchor.rtc_stats().admission_refused_transit();
    client
        .send_transit_probe_for_test(anchor.node_id(), dest)
        .await
        .expect("the probe leaves the client");
    assert!(
        wait_for(
            || anchor.rtc_stats().admission_refused_transit() > refused_before,
            Duration::from_secs(10)
        )
        .await,
        "the probe must arrive and be adjudicated at F1 — without this, F7's zeros \
         below would only prove the frame never left the client"
    );
    f7.assert_untouched("provisional");

    // ---- Phase 2: promoted.
    //
    // Same peer, same call, admission now open. This is the half that
    // makes the claim structural rather than gate-derived.
    let session = anchor
        .peer_session_id(client_id)
        .expect("the installed session");
    assert!(
        anchor.promote_admission(client_id, session, PeerAddr::Rtc(endpoint)),
        "promotion of the live incarnation"
    );
    assert!(!anchor.peer_is_provisional(client_id));

    let refused_before = anchor.rtc_stats().admission_refused_transit();
    client
        .send_transit_probe_for_test(anchor.node_id(), dest)
        .await
        .expect("the probe leaves the client");
    // Long enough for the DataChannel hop plus dispatch; the phase-1
    // refusal above lands well inside this window.
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(
        anchor.rtc_stats().admission_refused_transit(),
        refused_before,
        "an admitted peer's transit is not refused — so the silence at F7 cannot be \
         credited to the admission gate"
    );
    f7.assert_untouched("promoted");

    // ---- Positive control: the same envelope, delivered to F7's own
    // socket. Only the path changed.
    let envelope =
        MultiHopPacketBuilder::new(client_id as u32).build(dest, TRANSIT_TTL, b"transit");
    let injector = UdpSocket::bind(LOOPBACK).await.expect("bind injector");
    injector
        .send_to(&envelope, f7.proxy_addr)
        .await
        .expect("deliver to the proxy's own socket");

    assert!(
        wait_for(
            || f7.sink_hits.load(Ordering::SeqCst) == 1,
            Duration::from_secs(5)
        )
        .await,
        "positive control: F7 forwards an envelope that reaches its socket — \
         otherwise the zeros above measure a dead observable"
    );
    let stats = f7.proxy.stats();
    assert_eq!(f7.socket_ingress.load(Ordering::SeqCst), 1);
    assert_eq!(stats.packets_received, 1, "F7 was entered exactly once");
    assert_eq!(stats.packets_forwarded, 1);
    assert_eq!(stats.packets_dropped, 0);

    // …and what left F7 is the real forward, header rewritten.
    let forwarded = f7.last_at_sink.lock().clone();
    let header =
        RoutingHeader::from_bytes(&forwarded[..ROUTING_HEADER_SIZE]).expect("forwarded header");
    assert_eq!(header.dest_id, dest);
    assert_eq!(
        header.ttl,
        TRANSIT_TTL - 1,
        "the forward decremented TTL — this is F7's own rewrite, not an echo"
    );
    assert_eq!(header.hop_count, 1);
    assert_eq!(&forwarded[ROUTING_HEADER_SIZE..], b"transit");
}

/// The second half of the reachability argument: F7 cannot be the
/// *terminus* of a relay chain either.
///
/// The one production path that does forward on a peer's behalf
/// (`dispatch_packet`'s non-local arm) sends to an authenticated
/// adjacency. A proxy socket answers raw UDP — it receives the
/// handshake datagram, as asserted below — but it speaks no Noise, so
/// it never becomes one. Therefore no relay hop can be addressed to
/// it, and the direct-ingress witness above is not narrowed by the
/// transitive case.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_proxy_socket_cannot_become_the_authenticated_adjacency_a_relay_needs() {
    let f7 = ProxySidecar::spawn(0xDEAD_BEEF, 0x0BAD_CAFE).await;

    // A tight handshake budget: the point is that it fails, not how
    // patiently it retries.
    let peer = Arc::new(
        MeshNode::new(
            EntityKeypair::generate(),
            config(None).with_handshake(1, Duration::from_millis(300)),
        )
        .await
        .expect("MeshNode::new"),
    );
    peer.start_arc();

    let outcome = tokio::time::timeout(
        Duration::from_secs(10),
        peer.connect(f7.proxy_addr, &[0x11u8; 32], 0x7777),
    )
    .await;
    if let Ok(Ok(node_id)) = outcome {
        panic!(
            "a proxy socket must never become a mesh adjacency — it 'completed' a \
             handshake as node {node_id:#x}"
        );
    }
    assert_eq!(
        peer.peer_count(),
        0,
        "no peer entry may be installed for a socket that cannot authenticate"
    );

    // The failure is the handshake, not unreachability: the proxy's
    // socket did receive the attempt.
    assert!(
        f7.socket_ingress.load(Ordering::SeqCst) > 0,
        "the handshake datagram reached the proxy's socket — so 'cannot become a \
         peer' is about authentication, not about the socket being deaf"
    );
    assert_eq!(
        f7.sink_hits.load(Ordering::SeqCst),
        0,
        "a Noise handshake is not a routing header: nothing is forwarded"
    );
}
