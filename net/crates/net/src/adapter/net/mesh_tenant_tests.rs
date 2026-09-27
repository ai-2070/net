// SPDX-License-Identifier: MIT OR Apache-2.0
//! Tenant isolation at an anchor (browser plan P0 slice 2): sessions
//! enrolled for different tenants (games) are not shown each other's
//! announcements — neither flooded nor replayed on attach. Real UDP
//! sockets stand in for peers; the tenant rule does not depend on the
//! transport. Relayed transit is witnessed end to end by
//! `examples/anchor-acceptance`, with real browsers.
use super::*;
use crate::adapter::net::crypto::SessionKeys;
use crate::adapter::net::rtc::{PeerAdmission, TenantId};
use std::net::SocketAddr;

const GAME_A: TenantId = TenantId(0xA);
const GAME_B: TenantId = TenantId(0xB);

async fn node() -> MeshNode {
    MeshNode::new(
        EntityKeypair::generate(),
        MeshNodeConfig::new("127.0.0.1:0".parse().unwrap(), [0x61u8; 32]),
    )
    .await
    .expect("MeshNode::new")
}

/// A resolvable peer at `addr`, admitted for `tenant`.
fn install(ctx: &DispatchCtx, peer: u64, addr: SocketAddr, tenant: Option<TenantId>) {
    ctx.peers.insert(
        peer,
        PeerInfo {
            node_id: peer,
            transport: PeerTransport::Direct {
                owned: PeerAddr::Udp(addr),
            },
            session: Arc::new(NetSession::new(
                SessionKeys {
                    tx_key: [0x11u8; 32],
                    rx_key: [0x22u8; 32],
                    session_id: peer,
                    remote_static_pub: [0x33u8; 32],
                    route_hop_tx_key: [0x44u8; 32],
                    route_hop_rx_key: [0x55u8; 32],
                },
                PeerAddr::Udp(addr),
                4,
                false,
            )),
            remote_static_pub: [0x33u8; 32],
            last_initiator_ephemeral: None,
            admission: PeerAdmission::Admitted {
                promoted_at: std::time::Instant::now(),
                session_id: peer,
                tenant,
            },
        },
    );
}

async fn socket() -> (tokio::net::UdpSocket, SocketAddr) {
    let socket = tokio::net::UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = socket.local_addr().expect("addr");
    (socket, addr)
}

/// Frames that arrive on `socket` within `window`.
async fn frames(socket: &tokio::net::UdpSocket, window: Duration) -> usize {
    let mut buf = [0u8; 4096];
    let mut count = 0;
    let deadline = tokio::time::Instant::now() + window;
    while let Ok(Ok(_)) = tokio::time::timeout_at(deadline, socket.recv(&mut buf)).await {
        count += 1;
    }
    count
}

/// Inverse: drop the tenant filter from the flood and game B's socket
/// receives the frame (the first assertion fails).
#[tokio::test]
async fn an_announcement_is_not_flooded_to_another_tenant() {
    const SENDER: u64 = 0xA1;
    let node = node().await;
    let ctx = node.dispatch_ctx();
    let (same, same_addr) = socket().await;
    let (other, other_addr) = socket().await;
    let (native, native_addr) = socket().await;
    install(&ctx, SENDER, "127.0.0.1:9".parse().unwrap(), Some(GAME_A));
    install(&ctx, 0xA2, same_addr, Some(GAME_A));
    install(&ctx, 0xB1, other_addr, Some(GAME_B));
    install(&ctx, 0x0E, native_addr, None);

    MeshNode::forward_capability_announcement(b"ann".to_vec(), SENDER, SENDER, &ctx);
    let window = Duration::from_millis(800);
    let (to_other, to_same, to_native) = tokio::join!(
        frames(&other, window),
        frames(&same, window),
        frames(&native, window)
    );
    assert_eq!(to_other, 0, "game B never hears game A's announcement");
    assert_eq!(to_same, 1, "game A does");
    assert_eq!(to_native, 1, "a native peer (no tenant) meets everyone");
}

/// Inverse: drop the tenant filter from the replay and the game-B peer
/// attaching receives both held announcements.
#[tokio::test]
async fn attaching_replays_only_the_tenants_own_announcements() {
    let node = node().await;
    let ctx = node.dispatch_ctx();
    install(&ctx, 0xA1, "127.0.0.1:9".parse().unwrap(), Some(GAME_A));
    install(&ctx, 0xB1, "127.0.0.1:9".parse().unwrap(), Some(GAME_B));
    for origin in [0xA1u64, 0xB1] {
        ctx.relay_announcements.insert(
            origin,
            CapabilityAnnouncement::new(
                origin,
                EntityKeypair::generate().entity_id().clone(),
                1,
                crate::adapter::net::behavior::capability::CapabilitySet::new(),
            ),
        );
    }
    let (b_player, b_addr) = socket().await;
    let (native, native_addr) = socket().await;
    install(&ctx, 0xB2, b_addr, Some(GAME_B));
    install(&ctx, 0x0E, native_addr, None);

    MeshNode::replay_relay_announcements_once(
        0xB2,
        &ctx.relay_announcements,
        &ctx.peers,
        &ctx.sink,
    )
    .await;
    MeshNode::replay_relay_announcements_once(
        0x0E,
        &ctx.relay_announcements,
        &ctx.peers,
        &ctx.sink,
    )
    .await;
    let window = Duration::from_millis(800);
    let (to_b, to_native) = tokio::join!(frames(&b_player, window), frames(&native, window));
    assert_eq!(
        to_b, 1,
        "a game-B player is replayed game B's announcement only"
    );
    assert_eq!(to_native, 2, "a native peer is replayed both");
}

/// Promotion records the tenant the grant named, bound to the promoted
/// incarnation; the resolver the application installs is what names it.
#[tokio::test]
async fn promotion_records_the_resolved_tenant_on_the_session() {
    let node = node().await;
    let ctx = node.dispatch_ctx();
    let addr: SocketAddr = "127.0.0.1:9".parse().unwrap();
    install(&ctx, 0xC1, addr, None);
    if let Some(mut entry) = ctx.peers.get_mut(&0xC1) {
        entry.admission = PeerAdmission::provisional(std::time::Instant::now());
    }
    assert_eq!(
        node.resolve_enrollment_tenant(b"chain"),
        None,
        "no resolver, no tenant"
    );
    node.set_enrollment_tenant_resolver(Some(Arc::new(|chain: &[u8]| {
        (chain == b"chain-for-a").then_some(GAME_A)
    })));
    assert_eq!(node.resolve_enrollment_tenant(b"chain-for-a"), Some(GAME_A));
    assert_eq!(node.resolve_enrollment_tenant(b"anything else"), None);

    let session = node.peer_session_id(0xC1).expect("session");
    assert!(node.promote_admission_with_tenant(0xC1, session, PeerAddr::Udp(addr), Some(GAME_A)));
    assert_eq!(node.peer_tenant(0xC1), Some(GAME_A));
    assert!(
        !node.promote_admission_with_tenant(0xC1, session, PeerAddr::Udp(addr), Some(GAME_B)),
        "an admitted session is not re-promoted into another tenant"
    );
    assert_eq!(node.peer_tenant(0xC1), Some(GAME_A));
}

/// Relayed transit between two sessions of DIFFERENT tenants is refused
/// at the anchor before it is counted or forwarded; between two sessions
/// of the same tenant it proceeds. The source is the authenticated
/// adjacent session (its address), never the header's claimed `src_id`.
///
/// Inverse: drop the transit tenant check and the game-B destination's
/// forwarded count moves (the first assertion fails).
#[tokio::test]
async fn relayed_transit_between_tenants_is_refused() {
    use crate::adapter::net::route::RoutingHeader;
    let node = node().await;
    let ctx = node.dispatch_ctx();
    let source: SocketAddr = "127.0.0.1:40001".parse().unwrap();
    install(&ctx, 0xA1, source, Some(GAME_A));
    ctx.addr_to_node.insert(PeerAddr::Udp(source), 0xA1);
    install(&ctx, 0xA2, "127.0.0.1:40002".parse().unwrap(), Some(GAME_A));
    install(&ctx, 0xB1, "127.0.0.1:40003".parse().unwrap(), Some(GAME_B));

    let routed = |dest: u64| {
        // The header claims a source; the check must not believe it.
        let mut wire = RoutingHeader::new(dest, 0xA1, 4).to_bytes().to_vec();
        wire.extend_from_slice(&[0u8; crate::adapter::net::protocol::HEADER_SIZE]);
        Bytes::from(wire)
    };
    let forwarded = |dest: u64| {
        ctx.forwarded_app_packets
            .get(&(0xA1u32, dest))
            .map(|count| *count)
            .unwrap_or(0)
    };
    MeshNode::dispatch_packet(routed(0xB1), PeerAddr::Udp(source), &ctx);
    MeshNode::dispatch_packet(routed(0xA2), PeerAddr::Udp(source), &ctx);
    assert_eq!(
        forwarded(0xB1),
        0,
        "game A's session may not relay into game B"
    );
    assert_eq!(forwarded(0xA2), 1, "…and may within game A");
}
