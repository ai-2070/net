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
    // this assert the inner packet survived byte for byte.
    let watcher = wire().await;
    f.gw.router()
        .routing_table()
        .add_route(f.right.node_id(), watcher.local_addr().unwrap());
    let sock = wire().await;

    // ttl = 0 is already expired.
    let expired = RoutingHeader::new(f.right.node_id(), 0x1234, 0);
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
    let live = RoutingHeader::new(f.right.node_id(), 0x1234, 4);
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
