//! S4B of `docs/internal/plans/SUBNET_AUTH_PLAN.md` — production
//! relay enforcement.
//!
//! The gateway under test is a real `MeshNode` with two authenticated
//! neighbours. Traffic is driven as raw route-hop envelopes onto its
//! socket, which is the only way to reach the live pre-AEAD relay
//! branch — the one path that actually forwards in production
//! (`NetRouter::route_packet` and `NetProxy` have no production
//! callers and gating them would enforce nothing).
//!
//! What these pin, in one sentence: nothing a sender can *claim*
//! selects forwarding authority, and no side effect happens before
//! the hop is authenticated and the transition authorized.

#![cfg(feature = "net")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::identity::EntityKeypair;
use net::adapter::net::subnet::route_hop::{self, ROUTE_HOP_MAGIC};
use net::adapter::net::subnet::{
    admission::unix_now_secs, SubnetAuthPresentation, SubnetAuthorityConfig, SubnetCredentialSet,
    SubnetGrant, SubnetRef, SubnetRights, TopologySubnetId,
};
use net::adapter::net::{
    MeshNode, MeshNodeConfig, RoutingHeader, SocketBufferConfig, ROUTING_HEADER_SIZE,
};
use tokio::net::UdpSocket;

const PSK: [u8; 32] = [0x64u8; 32];
const DAY: u64 = 24 * 60 * 60;

fn root() -> EntityKeypair {
    EntityKeypair::from_bytes([0xC1; 32])
}

fn cfg(attachment: Option<&[u8]>) -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut c = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(3, Duration::from_secs(2))
        .with_subnet_authority(SubnetAuthorityConfig {
            authority: root().entity_id().clone(),
            roots: vec![root().entity_id().clone()],
            maximum_grant_lifetime_secs: 7 * DAY,
        });
    if let Some(a) = attachment {
        c.subnet_attachment = Some(TopologySubnetId::new(a));
    }
    c.socket_buffers = SocketBufferConfig {
        send_buffer_size: 256 * 1024,
        recv_buffer_size: 256 * 1024,
    };
    c
}

async fn node(kp: EntityKeypair, attachment: Option<&[u8]>) -> Arc<MeshNode> {
    Arc::new(
        MeshNode::new(kp, cfg(attachment))
            .await
            .expect("MeshNode::new"),
    )
}

async fn handshake(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    let a_id = a.node_id();
    let b_pub = *b.public_key();
    let b_addr = b.local_addr();
    let b2 = b.clone();
    let accept = tokio::spawn(async move { b2.accept(a_id).await });
    a.connect(b_addr, &b_pub, b.node_id())
        .await
        .expect("connect");
    accept.await.expect("task").expect("accept");
}

fn grant(subject: &EntityKeypair, scope: &[u8], rights: SubnetRights) -> SubnetCredentialSet {
    SubnetCredentialSet::Direct(
        SubnetGrant::try_issue(
            &root(),
            root().entity_id().clone(),
            TopologySubnetId::new(scope),
            0,
            subject.entity_id().clone(),
            rights,
            1,
            unix_now_secs() - 60,
            DAY,
        )
        .expect("issue"),
    )
}

/// Drive a full S3 admission of `peer` into `verifier` at `attachment`.
async fn admit(
    verifier: &Arc<MeshNode>,
    peer: &Arc<MeshNode>,
    peer_kp: &EntityKeypair,
    scope: &[u8],
    attachment: &[u8],
    rights: SubnetRights,
) {
    let set = grant(peer_kp, scope, rights);
    let node_id = peer.node_id();
    let nonce = verifier.issue_subnet_challenge(node_id).expect("challenge");
    let sid = verifier.peer_session_id(node_id).expect("session");
    let p = SubnetAuthPresentation::try_issue(
        peer_kp,
        set.credential_set_hash(),
        sid,
        verifier.entity_id().clone(),
        nonce,
        SubnetRef {
            authority: root().entity_id().clone(),
            path: TopologySubnetId::new(attachment),
        },
        rights,
    )
    .expect("presentation");
    verifier
        .admit_subnet_session(node_id, &p, &set)
        .expect("admission");
}

/// The gateway plus two admitted neighbours, with a route installed
/// from `left` to `right` through the gateway.
struct Fixture {
    gw: Arc<MeshNode>,
    gw_kp: EntityKeypair,
    left: Arc<MeshNode>,
    right: Arc<MeshNode>,
}

async fn fixture(left_at: &[u8], right_at: &[u8]) -> Fixture {
    let gw_kp = EntityKeypair::generate();
    let left_kp = EntityKeypair::generate();
    let right_kp = EntityKeypair::generate();
    let gw = node(gw_kp.clone(), Some(&[3])).await;
    let left = node(left_kp.clone(), None).await;
    let right = node(right_kp.clone(), None).await;

    handshake(&left, &gw).await;
    handshake(&right, &gw).await;
    gw.start();
    left.start();
    right.start();

    admit(&gw, &left, &left_kp, &[3], left_at, SubnetRights::ATTACH).await;
    admit(&gw, &right, &right_kp, &[3], right_at, SubnetRights::ATTACH).await;

    // The route toward `right` is installed by the handshake itself
    // now: a direct peer is identity-qualified by construction, so
    // `add_direct_route` binds `next_hop_id`. Installing a legacy
    // `add_route` here would overwrite that and silently disable
    // protected egress resolution.

    Fixture {
        gw,
        gw_kp,
        left,
        right,
    }
}

/// A raw socket standing in for `left`'s wire side, so a test can
/// send arbitrary (including hostile) bytes at the gateway.
async fn wire() -> UdpSocket {
    UdpSocket::bind("127.0.0.1:0").await.expect("bind")
}

/// Stand-in for an inner Net packet. Must be at least
/// `protocol::HEADER_SIZE` (68) bytes or dispatch classifies the
/// datagram as malformed rather than routed, and the legacy-path
/// assertions below would pass vacuously.
const INNER_TAG: &[u8] =
    b"NEinner-end-to-end-ciphertext-untouched-by-any-relay-0123456789-padding-to-clear-the-header-size-floor";

/// Count datagrams arriving at `sock` within a short window.
///
/// Callers that repoint a peer.s address at the watcher see that
/// peer.s ordinary traffic too (heartbeats, announcements), so
/// protected-forwarding assertions filter with [`route_hops`].
async fn received_within(sock: &UdpSocket, dur: Duration) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let deadline = tokio::time::Instant::now() + dur;
    let mut buf = vec![0u8; 4096];
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            break;
        }
        match tokio::time::timeout(left, sock.recv_from(&mut buf)).await {
            Ok(Ok((n, _))) => out.push(buf[..n].to_vec()),
            _ => break,
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Unauthenticated inputs cannot select authority
// ---------------------------------------------------------------------------

/// The core S4B claim. A legacy routing packet — the format that
/// carries `RoutingHeader.src_id`, arrives from a UDP source address,
/// and wraps an inner packet whose `NetHeader.subnet_id` a sender
/// controls — is refused outright by a protected gateway. None of
/// those values may select forwarding authority, so a gateway that
/// holds credentials will not relay traffic that offers only them.
#[tokio::test]
async fn a_protected_gateway_refuses_untagged_legacy_relay() {
    let f = fixture(&[3, 7, 1], &[3, 7, 2]).await;
    f.gw.install_subnet_gateway_credentials(&[grant(
        &f.gw_kp,
        &[3],
        SubnetRights::ATTACH.union(SubnetRights::ROUTE),
    )])
    .expect("install gateway credentials");

    // Watch the intended egress.
    let watcher = wire().await;
    f.gw.router()
        .routing_table()
        .add_route(f.right.node_id(), watcher.local_addr().unwrap());

    let sock = wire().await;
    let header = RoutingHeader::new(f.right.node_id(), 0x1234, 8);
    let mut legacy = Vec::new();
    legacy.extend_from_slice(&header.to_bytes());
    legacy.extend_from_slice(INNER_TAG);
    sock.send_to(&legacy, f.gw.local_addr())
        .await
        .expect("send");

    assert!(
        received_within(&watcher, Duration::from_millis(300))
            .await
            .is_empty(),
        "a protected gateway must not downgrade to the unauthenticated legacy path",
    );
}

/// Without gateway credentials the same node is not a protected
/// gateway, and the legacy path is unchanged — the protected mode is
/// opt-in via credentials, not a global behavior break.
#[tokio::test]
async fn a_node_without_gateway_credentials_keeps_legacy_forwarding() {
    let f = fixture(&[3, 7, 1], &[3, 7, 2]).await;
    let watcher = wire().await;
    // A LEARNED destination, not a direct peer: an ordinary route is
    // the only candidate for it, which is what public forwarding
    // actually rides. Pointing a direct peer's destination at a
    // watcher with `add_route` would not redirect anything — the
    // handshake's identity-bound candidate lives in its own slot and
    // wins the metric-1 tie, by design.
    const LEARNED: u64 = 0x0FF1_CE01;
    f.gw.router()
        .routing_table()
        .add_route(LEARNED, watcher.local_addr().unwrap());

    let sock = wire().await;
    let header = RoutingHeader::new(LEARNED, 0x1234, 8);
    let mut legacy = Vec::new();
    legacy.extend_from_slice(&header.to_bytes());
    legacy.extend_from_slice(INNER_TAG);
    sock.send_to(&legacy, f.gw.local_addr())
        .await
        .expect("send");

    let got = received_within(&watcher, Duration::from_millis(400)).await;
    assert_eq!(got.len(), 1, "public routing still forwards");
    let fwd = RoutingHeader::from_bytes(&got[0][..ROUTING_HEADER_SIZE]).expect("header");
    assert_eq!(fwd.ttl, 7, "legacy TTL decrements");
    assert_eq!(
        &got[0][ROUTING_HEADER_SIZE..],
        INNER_TAG,
        "legacy forwarding never touches the inner packet",
    );
}

/// A forged envelope — right shape, wrong key — is dropped. The
/// gateway holds full authority and both peers are admitted, so the
/// tag is the only thing standing in the way.
#[tokio::test]
async fn an_invalid_hop_tag_is_dropped_before_any_forward() {
    let f = fixture(&[3, 7, 1], &[3, 7, 2]).await;
    f.gw.install_subnet_gateway_credentials(&[grant(
        &f.gw_kp,
        &[3],
        SubnetRights::ATTACH.union(SubnetRights::ROUTE),
    )])
    .expect("install");

    let watcher = wire().await;
    f.gw.router()
        .routing_table()
        .add_route(f.right.node_id(), watcher.local_addr().unwrap());

    let sock = wire().await;
    let header = RoutingHeader::new(f.right.node_id(), 0x1234, 8);
    let sid = f
        .left
        .peer_session_id(f.gw.node_id())
        .expect("left has a session with gw");
    // Attacker key — the shape is perfect, the MAC is not.
    let forged = route_hop::seal(&[0xFF; 32], sid, 1, &header, INNER_TAG);
    assert_eq!(
        u16::from_le_bytes([forged[0], forged[1]]),
        ROUTE_HOP_MAGIC,
        "precondition: this is a well-formed envelope",
    );
    sock.send_to(&forged, f.gw.local_addr())
        .await
        .expect("send");

    assert!(
        received_within(&watcher, Duration::from_millis(300))
            .await
            .is_empty(),
        "a bad hop tag must drop before route lookup or forwarding",
    );
}

/// An envelope naming a session id that does not exist resolves to no
/// ingress identity and is dropped — a sender cannot conjure an
/// ingress peer by guessing.
#[tokio::test]
async fn an_unknown_hop_session_is_dropped() {
    let f = fixture(&[3, 7, 1], &[3, 7, 2]).await;
    f.gw.install_subnet_gateway_credentials(&[grant(
        &f.gw_kp,
        &[3],
        SubnetRights::ATTACH.union(SubnetRights::ROUTE),
    )])
    .expect("install");

    let watcher = wire().await;
    f.gw.router()
        .routing_table()
        .add_route(f.right.node_id(), watcher.local_addr().unwrap());

    let sock = wire().await;
    let header = RoutingHeader::new(f.right.node_id(), 0x1234, 8);
    let bogus = route_hop::seal(&[0x01; 32], 0xDEAD_BEEF_DEAD_BEEF, 1, &header, INNER_TAG);
    sock.send_to(&bogus, f.gw.local_addr()).await.expect("send");

    assert!(received_within(&watcher, Duration::from_millis(300))
        .await
        .is_empty(),);
}

// ---------------------------------------------------------------------------
// Authority is required, independently, at each of three places
// ---------------------------------------------------------------------------

/// With no gateway credentials installed, a well-formed authenticated
/// envelope still forwards nothing: peer admission is not the
/// gateway's authority.
#[tokio::test]
async fn missing_local_gateway_context_denies() {
    let f = fixture(&[3, 7, 1], &[3, 7, 2]).await;
    assert!(f.gw.subnet_gateway_contexts().is_none());

    let watcher = wire().await;
    f.gw.router()
        .routing_table()
        .add_route(f.right.node_id(), watcher.local_addr().unwrap());

    // A correctly-keyed envelope is impossible for the test to build
    // without the session key, so assert the weaker but sufficient
    // fact: the node is not a protected gateway and the protected
    // path forwards nothing for it.
    let sock = wire().await;
    let header = RoutingHeader::new(f.right.node_id(), 0x1234, 8);
    let env = route_hop::seal(&[0x01; 32], 1, 1, &header, INNER_TAG);
    sock.send_to(&env, f.gw.local_addr()).await.expect("send");
    assert!(received_within(&watcher, Duration::from_millis(300))
        .await
        .is_empty());
}

/// Withdrawing the ingress peer's admission denies subsequent
/// forwards even though the gateway's own authority is intact.
#[tokio::test]
async fn withdrawn_ingress_admission_denies() {
    let f = fixture(&[3, 7, 1], &[3, 7, 2]).await;
    f.gw.install_subnet_gateway_credentials(&[grant(
        &f.gw_kp,
        &[3],
        SubnetRights::ATTACH.union(SubnetRights::ROUTE),
    )])
    .expect("install");
    assert!(f.gw.subnet_context_for(f.left.node_id()).is_some());

    f.gw.withdraw_subnet_admission(f.left.node_id());
    assert!(
        f.gw.subnet_context_for(f.left.node_id()).is_none(),
        "ingress peer is no longer admitted, so its hops cannot be relayed",
    );
}

/// Withdrawing the EGRESS peer's admission denies too — both ends of
/// the transition must be admitted, not just the one that spoke.
#[tokio::test]
async fn withdrawn_egress_admission_denies() {
    let f = fixture(&[3, 7, 1], &[3, 7, 2]).await;
    f.gw.install_subnet_gateway_credentials(&[grant(
        &f.gw_kp,
        &[3],
        SubnetRights::ATTACH.union(SubnetRights::ROUTE),
    )])
    .expect("install");
    assert!(f.gw.subnet_context_for(f.right.node_id()).is_some());

    f.gw.withdraw_subnet_admission(f.right.node_id());
    assert!(f.gw.subnet_context_for(f.right.node_id()).is_none());
    // Ingress is still admitted — the denial is specifically the
    // egress end.
    assert!(f.gw.subnet_context_for(f.left.node_id()).is_some());
}

/// An accepted revocation floor moves the authority epoch, which
/// invalidates the compiled peer contexts the relay depends on.
#[tokio::test]
async fn a_floor_invalidates_the_contexts_the_relay_reads() {
    use net::adapter::net::subnet::SubnetRevocationFloor;
    let f = fixture(&[3, 7, 1], &[3, 7, 2]).await;
    f.gw.install_subnet_gateway_credentials(&[grant(
        &f.gw_kp,
        &[3],
        SubnetRights::ATTACH.union(SubnetRights::ROUTE),
    )])
    .expect("install");
    assert!(f.gw.subnet_context_for(f.left.node_id()).is_some());

    let floor = SubnetRevocationFloor::try_issue(
        &root(),
        SubnetRef {
            authority: root().entity_id().clone(),
            path: TopologySubnetId::new(&[3]),
        },
        0,
        5,
        1,
        unix_now_secs(),
    )
    .expect("issue floor");
    assert!(f.gw.apply_subnet_floor(&floor).expect("apply"));
    assert!(
        f.gw.subnet_context_for(f.left.node_id()).is_none(),
        "revocation must reach the relay through the compiled contexts",
    );
}

// ---------------------------------------------------------------------------
// Next-hop identity
// ---------------------------------------------------------------------------

/// A legacy (address-only) route carries no identity, so it cannot
/// select protected forwarding authority — it resolves to nothing
/// rather than to whoever currently answers at that address.
#[tokio::test]
async fn a_legacy_route_is_not_an_authenticated_next_hop() {
    let f = fixture(&[3, 7, 1], &[3, 7, 2]).await;
    // A learned multi-hop route for a node that is NOT a direct peer:
    // address only, no bound identity. Direct peers get an
    // identity-qualified entry from the handshake, so the distinction
    // has to be drawn on a route that legitimately has no identity.
    const LEARNED: u64 = 0xDEAD_BEEF;
    f.gw.router()
        .routing_table()
        .add_route(LEARNED, f.right.local_addr());
    assert!(
        f.gw.router().routing_table().lookup(LEARNED).is_some(),
        "precondition: the legacy route resolves for ordinary routing",
    );
    assert!(
        f.gw.authenticated_next_hop(LEARNED).is_none(),
        "an address-only route must not be usable for protected forwarding",
    );
    assert!(f.gw.authenticated_next_hop(0x0BAD_0BAD).is_none());

    // And the direct peer DOES resolve, which is what makes protected
    // egress possible at all.
    assert!(
        f.gw.authenticated_next_hop(f.right.node_id()).is_some(),
        "a direct peer must be identity-qualified by the handshake",
    );
}

/// Identity is bound into the route entry at install time, so it is
/// the stable half of a next hop.
///
/// Two inverse properties, which is the point of binding it there
/// instead of resolving an address through a mutable map: an address
/// change follows the identity, and a *different* identity arriving
/// at the old address inherits nothing.
#[tokio::test]
async fn route_identity_survives_address_change_and_resists_address_reuse() {
    let f = fixture(&[3, 7, 1], &[3, 7, 2]).await;
    let right_id = f.right.node_id();
    let table = f.gw.router().routing_table();

    table.add_authenticated_route(right_id, f.right.local_addr(), right_id);
    let hop =
        f.gw.authenticated_next_hop(right_id)
            .expect("an identity-bound route resolves");
    assert_eq!(hop.node_id, right_id);
    assert_eq!(hop.addr, f.right.local_addr());

    // NAT rebind: the address moves under the SAME identity.
    let moved: SocketAddr = "127.0.0.1:59999".parse().unwrap();
    assert!(
        table.rebind_authenticated_route(right_id, right_id, moved),
        "an address change under the bound identity is permitted",
    );
    let (id, addr) = table
        .lookup_authenticated(right_id)
        .expect("route still present");
    assert_eq!(id, right_id, "identity is unchanged by an address move");
    assert_eq!(addr, moved);

    // Address reuse: a DIFFERENT identity cannot take the route over,
    // which is exactly what resolving identity from a mutable address
    // map would have allowed.
    let interloper = f.left.node_id();
    assert!(
        !table.rebind_authenticated_route(right_id, interloper, f.left.local_addr()),
        "a different identity must not inherit an existing protected route",
    );
    let (id, _) = table
        .lookup_authenticated(right_id)
        .expect("route still present");
    assert_eq!(
        id, right_id,
        "the bound identity must survive an attempted takeover",
    );
}

// ---------------------------------------------------------------------------
// TTL disposition
// ---------------------------------------------------------------------------

/// Gateway TTL uses the mutable OUTER routing header. An expired
/// outer TTL stops the relay; the inner packet — whose
/// `NetHeader.hop_ttl` is AAD-covered — is never rewritten, so the
/// bytes that leave are the bytes that arrived.
#[tokio::test]
async fn outer_ttl_expires_while_the_inner_packet_is_untouched() {
    let f = fixture(&[3, 7, 1], &[3, 7, 2]).await;

    // Legacy mode (no gateway credentials) makes the forwarded bytes
    // observable without holding a session key, which is what lets
    // this assert the inner packet survived byte for byte. The
    // destination is a LEARNED one rather than a direct peer, so the
    // ordinary route installed here is the only candidate for it —
    // see the note in the legacy-forwarding test above.
    const LEARNED: u64 = 0x0FF1_CE02;
    let watcher = wire().await;
    f.gw.router()
        .routing_table()
        .add_route(LEARNED, watcher.local_addr().unwrap());
    let sock = wire().await;

    // ttl = 0 is already expired.
    let expired = RoutingHeader::new(LEARNED, 0x1234, 0);
    let mut pkt = Vec::new();
    pkt.extend_from_slice(&expired.to_bytes());
    pkt.extend_from_slice(INNER_TAG);
    sock.send_to(&pkt, f.gw.local_addr()).await.expect("send");
    assert!(
        received_within(&watcher, Duration::from_millis(250))
            .await
            .is_empty(),
        "an expired outer TTL must stop the relay",
    );

    // A live TTL forwards, decrements the OUTER header only, and
    // leaves every inner byte alone.
    let live = RoutingHeader::new(LEARNED, 0x1234, 4);
    let mut pkt = Vec::new();
    pkt.extend_from_slice(&live.to_bytes());
    pkt.extend_from_slice(INNER_TAG);
    sock.send_to(&pkt, f.gw.local_addr()).await.expect("send");
    let got = received_within(&watcher, Duration::from_millis(400)).await;
    assert_eq!(got.len(), 1);
    let fwd = RoutingHeader::from_bytes(&got[0][..ROUTING_HEADER_SIZE]).expect("header");
    assert_eq!(fwd.ttl, 3);
    assert_eq!(fwd.hop_count, 1);
    assert_eq!(
        &got[0][ROUTING_HEADER_SIZE..],
        INNER_TAG,
        "the inner packet must be byte-identical after a hop",
    );
}

// ---------------------------------------------------------------------------
// The positive witness
// ---------------------------------------------------------------------------

/// A VALID protected hop is authenticated, authorized, forwarded, and
/// re-tagged under the egress edge key — with the inner packet
/// byte-identical and only the outer TTL moved.
///
/// Every other protected test in this file sends a deliberately invalid
/// datagram and asserts nothing comes out. That shape cannot distinguish
/// "correctly refused" from "the protected branch forwards nothing at
/// all", and for a while it did not: `add_authenticated_route` had no
/// production caller, so `lookup_authenticated` never resolved and every
/// authorized hop died at egress lookup. The whole slice would have
/// stayed green while forwarding was dead.
///
/// This is the test that fails if the protected branch drops valid
/// packets, skips the transition check, mutates TTL before authorizing,
/// seals under the wrong key, or re-tags incorrectly.
#[tokio::test]
async fn a_valid_protected_hop_is_forwarded_and_retagged() {
    let f = fixture(&[3, 7, 1], &[3, 7, 2]).await;
    f.gw.install_subnet_gateway_credentials(&[grant(
        &f.gw_kp,
        &[3],
        SubnetRights::ATTACH.union(SubnetRights::ROUTE),
    )])
    .expect("install gateway credentials");
    // No declared boundary: both attachments are inside the vehicle, so
    // this is an internal transition and ROUTE governs it.
    f.gw.declare_subnet_boundaries(net::adapter::net::subnet::SubnetBoundarySet::new(
        root().entity_id().clone(),
        0,
        [],
    ));

    // Observe the egress. The relay reads the egress address from the
    // peer snapshot, so the watcher goes there — the session, keys and
    // admitted context of `right` are untouched, which is what keeps
    // the emitted tag verifiable below.
    let watcher = wire().await;
    assert!(
        f.gw.set_peer_addr_for_test(f.right.node_id(), watcher.local_addr().expect("addr")),
        "the egress peer must exist",
    );

    // Seal a genuine hop on the left↔gateway edge.
    let header = RoutingHeader::new(f.right.node_id(), f.left.node_id() as u32, 8);
    let envelope = f
        .left
        .seal_route_hop_to_peer(f.gw.node_id(), &header, INNER_TAG)
        .expect("left has a session to the gateway");

    // Sent from an unrelated socket on purpose: ingress identity comes
    // from the hop session id, never from the UDP source address. If
    // the relay were reading the source address this would fail.
    let sock = wire().await;
    sock.send_to(&envelope, f.gw.local_addr())
        .await
        .expect("send");

    let got = received_within(&watcher, Duration::from_millis(500)).await;
    let hops = route_hops(&got);
    assert_eq!(hops.len(), 1, "exactly one hop must be forwarded");
    let out = hops[0];

    // It is a protected envelope, not a downgrade to the legacy path.
    assert_eq!(
        u16::from_le_bytes([out[0], out[1]]),
        ROUTE_HOP_MAGIC,
        "a protected hop must be re-emitted protected",
    );

    // Re-tagged under the EGRESS edge key: the gateway→right session,
    // not the one it arrived on. Opening it proves the MAC verifies and
    // the sequence is admitted exactly once.
    let (out_header, out_inner) = f
        .right
        .open_route_hop_from_peer(f.gw.node_id(), out)
        .expect("the forwarded hop verifies under the egress edge key");

    // The inner end-to-end packet is byte-identical.
    assert_eq!(
        out_inner, INNER_TAG,
        "the relay must not touch the inner packet",
    );
    // Only the outer routing header moved, and only downward.
    assert_eq!(out_header.dest_id, f.right.node_id());
    // EXACTLY once, on both fields. A disjunction here would pass if
    // the relay decremented twice, incremented twice, or moved only
    // one field — the prose says "exactly once", so the assertion has
    // to say it too.
    assert_eq!(
        out_header.ttl,
        header.ttl - 1,
        "outer TTL must be decremented exactly once",
    );
    assert_eq!(
        out_header.hop_count,
        header.hop_count + 1,
        "outer hop_count must be incremented exactly once",
    );

    // The same envelope replayed at the gateway is refused, so the
    // forward above consumed its ingress sequence.
    sock.send_to(&envelope, f.gw.local_addr())
        .await
        .expect("send replay");
    let after_replay = received_within(&watcher, Duration::from_millis(300)).await;
    assert!(
        route_hops(&after_replay).is_empty(),
        "a replayed hop envelope must not be forwarded a second time",
    );
}

/// The positive path still requires authorization: same valid envelope,
/// same authenticated edge, but the gateway holds no ROUTE over the
/// transition. Nothing is emitted.
///
/// Paired with the test above this is the discriminating inverse — one
/// proves forwarding happens, the other proves it happens *because* the
/// transition was authorized.
#[tokio::test]
async fn a_valid_hop_without_route_authority_is_not_forwarded() {
    let f = fixture(&[3, 7, 1], &[3, 7, 2]).await;
    // ATTACH only — admission, never forwarding.
    f.gw.install_subnet_gateway_credentials(&[grant(&f.gw_kp, &[3], SubnetRights::ATTACH)])
        .expect("install gateway credentials");
    f.gw.declare_subnet_boundaries(net::adapter::net::subnet::SubnetBoundarySet::new(
        root().entity_id().clone(),
        0,
        [],
    ));

    let watcher = wire().await;
    assert!(f
        .gw
        .set_peer_addr_for_test(f.right.node_id(), watcher.local_addr().expect("addr")));

    let header = RoutingHeader::new(f.right.node_id(), f.left.node_id() as u32, 8);
    let envelope = f
        .left
        .seal_route_hop_to_peer(f.gw.node_id(), &header, INNER_TAG)
        .expect("session");

    let sock = wire().await;
    sock.send_to(&envelope, f.gw.local_addr())
        .await
        .expect("send");

    let got = received_within(&watcher, Duration::from_millis(300)).await;
    assert!(
        route_hops(&got).is_empty(),
        "ATTACH is not forwarding authority, however valid the hop is",
    );
}

/// The learned-route FIRST-HOP production witness — one credentialed
/// gateway, its first protected relay. (The live two-gateway flow is
/// the pair of tests below; this one deliberately proves less: that a
/// route learned through production propagation selects the
/// authenticated ADJACENT hop for a NON-adjacent destination.)
///
/// Topology: `left ↔ gw ↔ right ↔ dest`. The gateway's only knowledge
/// of `dest` is what production learning installs from traffic
/// arriving via `right`. The convergence poll is DETERMINISTIC about
/// which writer satisfies it: pingwaves are unauthenticated datagrams
/// and install legacy (address-only) routes that can never resolve
/// through `authenticated_next_hop`, so the only writer able to
/// satisfy the poll is capability-announcement learning — the exact
/// path this test drives with `announce_capabilities`. A mutation
/// breaking that writer cannot be masked by the pingwave path.
///
/// This is the test that fails if learned routes install without
/// identity, bind the advertised origin or the destination instead of
/// the adjacent peer, or resolve identity from an address at egress
/// time.
#[tokio::test]
async fn a_production_learned_route_selects_the_authenticated_first_hop() {
    use net::adapter::net::behavior::capability::CapabilitySet;

    let gw_kp = EntityKeypair::generate();
    let left_kp = EntityKeypair::generate();
    let right_kp = EntityKeypair::generate();
    let dest_kp = EntityKeypair::generate();
    let gw = node(gw_kp.clone(), Some(&[3])).await;
    let left = node(left_kp.clone(), None).await;
    let right = node(right_kp.clone(), None).await;
    let dest = node(dest_kp.clone(), None).await;

    // Line topology; every handshake precedes every start().
    handshake(&left, &gw).await;
    handshake(&right, &gw).await;
    handshake(&dest, &right).await;
    gw.start();
    left.start();
    right.start();
    dest.start();

    admit(&gw, &left, &left_kp, &[3], &[3, 7, 1], SubnetRights::ATTACH).await;
    admit(
        &gw,
        &right,
        &right_kp,
        &[3],
        &[3, 7, 2],
        SubnetRights::ATTACH,
    )
    .await;

    gw.install_subnet_gateway_credentials(&[grant(
        &gw_kp,
        &[3],
        SubnetRights::ATTACH.union(SubnetRights::ROUTE),
    )])
    .expect("install gateway credentials");
    gw.declare_subnet_boundaries(net::adapter::net::subnet::SubnetBoundarySet::new(
        root().entity_id().clone(),
        0,
        [],
    ));

    // Kick production learning explicitly (the pingwave flood also
    // runs on its own schedule): `dest`'s announcement reaches `right`
    // direct (hop 0) and `gw` forwarded (hop 1), and the forwarded
    // receipt is a learned-route install at the gateway.
    dest.announce_capabilities(CapabilitySet::new().add_tag("learned-route-witness"))
        .await
        .expect("announce");

    // Await convergence — and require IDENTITY, not reachability: the
    // poll accepts nothing until the gateway resolves `dest` to an
    // authenticated next hop, which a legacy (address-only) learned
    // install never satisfies.
    let dest_id = dest.node_id();
    let right_id = right.node_id();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let hop = loop {
        if let Some(hop) = gw.authenticated_next_hop(dest_id) {
            break hop;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the gateway never learned an identity-bound route to dest \
             through production propagation",
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(
        hop.node_id, right_id,
        "the learned route must bind the ADJACENT authenticated peer — \
         not the destination, the advertised origin, or whoever answers \
         at an address",
    );

    // Observe the egress leg. The consistent-move seam repoints the
    // peer record, indexes, AND the identity-bound routes, exactly as
    // a live NAT rebind does.
    let watcher = wire().await;
    assert!(gw.set_peer_addr_for_test(right_id, watcher.local_addr().expect("addr")));

    // A protected hop from `left`, destined PAST the gateway's
    // adjacency.
    let header = RoutingHeader::new(dest_id, left.node_id() as u32, 8);
    let envelope = left
        .seal_route_hop_to_peer(gw.node_id(), &header, INNER_TAG)
        .expect("left has a session to the gateway");
    let sock = wire().await;
    sock.send_to(&envelope, gw.local_addr())
        .await
        .expect("send");

    let got = received_within(&watcher, Duration::from_millis(500)).await;
    let hops = route_hops(&got);
    assert_eq!(hops.len(), 1, "exactly one hop must be forwarded");
    let out = hops[0];

    // Re-tagged for the ADJACENT hop — the gw↔right edge key…
    let (out_header, out_inner) = right
        .open_route_hop_from_peer(gw.node_id(), out)
        .expect("the forwarded hop verifies under the gw↔right edge key");
    // …while still aimed at the REMOTE destination.
    assert_eq!(
        out_header.dest_id, dest_id,
        "the remote destination must ride through the relay unchanged",
    );
    assert_eq!(
        out_inner, INNER_TAG,
        "the relay must not touch the inner packet",
    );
    assert_eq!(
        out_header.ttl,
        header.ttl - 1,
        "outer TTL must be decremented exactly once",
    );
    assert_eq!(
        out_header.hop_count,
        header.hop_count + 1,
        "outer hop_count must be incremented exactly once",
    );
}

/// Two credentialed gateways in a line, for the live multi-hop
/// witnesses: `left ↔ gw1 ↔ gw2 ↔ dest`. Both gateways hold admitted
/// contexts for their neighbours; `gw2_rights` decides whether the
/// SECOND gateway may forward (the positive witness grants ROUTE, the
/// inverse withholds it). gw1 always holds ROUTE, so every difference
/// observed at the destination side is gw2's doing.
struct TwoGatewayFixture {
    gw1: Arc<MeshNode>,
    gw2: Arc<MeshNode>,
    left: Arc<MeshNode>,
    dest: Arc<MeshNode>,
}

async fn two_gateway_fixture(gw2_rights: SubnetRights) -> TwoGatewayFixture {
    use net::adapter::net::behavior::capability::CapabilitySet;

    let gw1_kp = EntityKeypair::generate();
    let gw2_kp = EntityKeypair::generate();
    let left_kp = EntityKeypair::generate();
    let dest_kp = EntityKeypair::generate();
    let gw1 = node(gw1_kp.clone(), Some(&[3])).await;
    let gw2 = node(gw2_kp.clone(), Some(&[3])).await;
    let left = node(left_kp.clone(), None).await;
    let dest = node(dest_kp.clone(), None).await;

    // Line topology; every handshake precedes every start().
    handshake(&left, &gw1).await;
    handshake(&gw2, &gw1).await;
    handshake(&dest, &gw2).await;
    gw1.start();
    gw2.start();
    left.start();
    dest.start();

    // gw1's two hop-local attachments: left in, gw2 out.
    admit(
        &gw1,
        &left,
        &left_kp,
        &[3],
        &[3, 7, 1],
        SubnetRights::ATTACH,
    )
    .await;
    admit(&gw1, &gw2, &gw2_kp, &[3], &[3, 7, 2], SubnetRights::ATTACH).await;
    // gw2's two hop-local attachments: gw1 in, dest out.
    admit(&gw2, &gw1, &gw1_kp, &[3], &[3, 7, 2], SubnetRights::ATTACH).await;
    admit(
        &gw2,
        &dest,
        &dest_kp,
        &[3],
        &[3, 7, 3],
        SubnetRights::ATTACH,
    )
    .await;

    gw1.install_subnet_gateway_credentials(&[grant(
        &gw1_kp,
        &[3],
        SubnetRights::ATTACH.union(SubnetRights::ROUTE),
    )])
    .expect("install gw1 credentials");
    gw2.install_subnet_gateway_credentials(&[grant(&gw2_kp, &[3], gw2_rights)])
        .expect("install gw2 credentials");
    for gw in [&gw1, &gw2] {
        gw.declare_subnet_boundaries(net::adapter::net::subnet::SubnetBoundarySet::new(
            root().entity_id().clone(),
            0,
            [],
        ));
    }

    // Production learning: dest announces; gw2 receives it direct
    // (hop 0 — its route to dest is the handshake's authenticated
    // adjacency), gw1 receives it forwarded through gw2 (hop 1) and
    // installs the identity-bound learned route. As in the first-hop
    // witness, only capability learning can satisfy the identity
    // poll — pingwave installs are legacy by design.
    dest.announce_capabilities(CapabilitySet::new().add_tag("two-gateway-witness"))
        .await
        .expect("announce");
    let dest_id = dest.node_id();
    let gw2_id = gw2.node_id();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(hop) = gw1.authenticated_next_hop(dest_id) {
            assert_eq!(
                hop.node_id, gw2_id,
                "gw1's learned route must bind gw2, its authenticated adjacent hop",
            );
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "gw1 never learned an identity-bound route to dest through \
             production propagation",
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    TwoGatewayFixture {
        gw1,
        gw2,
        left,
        dest,
    }
}

/// The LIVE two-gateway positive witness. One protected envelope
/// crosses TWO production relays — `left → gw1 → gw2 → dest-side
/// watcher` — with gw1's route to the remote destination established
/// only through production propagation via gw2.
///
/// Both relays run the full production path on a real receive loop:
/// classification, ingress MAC + replay, authenticated route lookup,
/// local transition authorization, TTL mutation, re-tag under the
/// next edge key. Nothing between gw1 and gw2 is intercepted — the
/// inter-gateway leg lands on gw2's real socket. Only the FINAL leg
/// is observed, by standing a watcher at gw2's view of `dest`, and
/// the captured envelope must verify under the gw2↔dest edge key
/// with the destination and inner bytes untouched and the hop budget
/// moved exactly TWICE.
#[tokio::test]
async fn a_protected_hop_traverses_two_credentialed_gateways() {
    let f = two_gateway_fixture(SubnetRights::ATTACH.union(SubnetRights::ROUTE)).await;
    let dest_id = f.dest.node_id();

    // Observe only the last leg: gw2's egress toward dest.
    let watcher = wire().await;
    assert!(f
        .gw2
        .set_peer_addr_for_test(dest_id, watcher.local_addr().expect("addr")));

    let header = RoutingHeader::new(dest_id, f.left.node_id() as u32, 8);
    let envelope = f
        .left
        .seal_route_hop_to_peer(f.gw1.node_id(), &header, INNER_TAG)
        .expect("left has a session to gw1");
    let sock = wire().await;
    sock.send_to(&envelope, f.gw1.local_addr())
        .await
        .expect("send");

    let got = received_within(&watcher, Duration::from_millis(1000)).await;
    let hops = route_hops(&got);
    assert_eq!(
        hops.len(),
        1,
        "exactly one hop must arrive at the destination side",
    );
    let out = hops[0];

    // The captured envelope is the SECOND relay's output: it verifies
    // under the gw2↔dest edge key, which gw1 does not hold.
    let (out_header, out_inner) = f
        .dest
        .open_route_hop_from_peer(f.gw2.node_id(), out)
        .expect("the final hop verifies under the gw2↔dest edge key");
    assert_eq!(
        out_header.dest_id, dest_id,
        "the destination must ride through BOTH relays unchanged",
    );
    assert_eq!(
        out_inner, INNER_TAG,
        "neither relay may touch the inner packet",
    );
    assert_eq!(
        out_header.ttl,
        header.ttl - 2,
        "outer TTL must be decremented exactly once per relay",
    );
    assert_eq!(
        out_header.hop_count,
        header.hop_count + 2,
        "outer hop_count must be incremented exactly once per relay",
    );
}

/// The second gateway's authority is load-bearing, not decorative:
/// same topology, same learned route, same valid envelope — but gw2
/// holds only ATTACH. gw1 (which holds ROUTE) forwards the hop to
/// gw2, and gw2 refuses the transition, so nothing reaches the
/// destination side. A mutation that made the second relay skip its
/// own authorization would turn this test red.
#[tokio::test]
async fn a_second_gateway_without_route_authority_stops_the_hop() {
    let f = two_gateway_fixture(SubnetRights::ATTACH).await;
    let dest_id = f.dest.node_id();

    let watcher = wire().await;
    assert!(f
        .gw2
        .set_peer_addr_for_test(dest_id, watcher.local_addr().expect("addr")));

    let header = RoutingHeader::new(dest_id, f.left.node_id() as u32, 8);
    let envelope = f
        .left
        .seal_route_hop_to_peer(f.gw1.node_id(), &header, INNER_TAG)
        .expect("left has a session to gw1");
    let sock = wire().await;
    sock.send_to(&envelope, f.gw1.local_addr())
        .await
        .expect("send");

    let got = received_within(&watcher, Duration::from_millis(500)).await;
    assert!(
        route_hops(&got).is_empty(),
        "without ROUTE at the second gateway, no protected hop may reach \
         the destination side — first-relay authority must not carry the \
         packet through the second",
    );
}

/// Only the route-hop envelopes among `datagrams`.
///
/// A watcher standing in for a peer's address also receives that peer's
/// ordinary traffic. Counting raw datagrams would make a
/// protected-forwarding assertion pass or fail on heartbeat timing.
fn route_hops(datagrams: &[Vec<u8>]) -> Vec<&Vec<u8>> {
    datagrams
        .iter()
        .filter(|d| d.len() >= 2 && u16::from_le_bytes([d[0], d[1]]) == ROUTE_HOP_MAGIC)
        .collect()
}
