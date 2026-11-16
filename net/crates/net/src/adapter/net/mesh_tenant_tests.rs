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
                device: None,
                enrolled_origin: None,
            },
        },
    );
}

/// Hold an announcement from `origin` for replay, recording `tenant` as
/// the hold path does (`None` = resolve the origin's tenant live).
fn hold(ctx: &DispatchCtx, origin: u64, tenant: Option<TenantId>) {
    ctx.relay_announcements.insert(
        origin,
        HeldAnnouncement {
            ann: CapabilityAnnouncement::new(
                origin,
                EntityKeypair::generate().entity_id().clone(),
                1,
                crate::adapter::net::behavior::capability::CapabilitySet::new(),
            ),
            tenant,
        },
    );
}

/// Make `peer` a provisional (unenrolled browser) session.
fn make_provisional(ctx: &DispatchCtx, peer: u64) {
    if let Some(mut entry) = ctx.peers.get_mut(&peer) {
        entry.admission = PeerAdmission::provisional(std::time::Instant::now());
    }
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
        hold(&ctx, origin, None);
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

/// **The discovery gap the CLI-anchor browser acceptance hit.** A game-A
/// lobby host announced once; a game-A player connects LATER, sits
/// provisional while it enrolls, and is promoted. The browser install path
/// (`accept_rtc`) replays nothing, and a provisional session is shown no
/// tenant's announcements, so the promotion is the one moment the player
/// can be caught up — and it must be, with the tenant now known.
///
/// Inverse: drop the replay from `promote_admission_with_tenant` and the
/// player's socket receives nothing (the assertion fails).
#[tokio::test]
async fn enrollment_promotion_replays_the_tenants_held_announcements() {
    let node = node().await;
    let ctx = node.dispatch_ctx();
    install(&ctx, 0xA1, "127.0.0.1:9".parse().unwrap(), Some(GAME_A));
    install(&ctx, 0xB1, "127.0.0.1:9".parse().unwrap(), Some(GAME_B));
    hold(&ctx, 0xA1, None);
    hold(&ctx, 0xB1, None);
    let (player, player_addr) = socket().await;
    install(&ctx, 0xA2, player_addr, None);
    make_provisional(&ctx, 0xA2);

    let session = node.peer_session_id(0xA2).expect("session");
    assert!(node.promote_admission_with_tenant(
        0xA2,
        session,
        PeerAddr::Udp(player_addr),
        Some(GAME_A)
    ));
    // The first settle pass (250 ms) lands inside the window; the second
    // (2 s later) does not.
    assert_eq!(
        frames(&player, Duration::from_millis(1500)).await,
        1,
        "the promoted game-A player is replayed game A's lobby announcement, and only it"
    );
}

/// **Security: an unenrolled session is not a native peer.** A
/// provisional session has no tenant, and `tenants_may_meet(Some, None)`
/// is true — so before this rule every game's announcements were flooded
/// and replayed to any browser that completed the handshake and simply
/// never enrolled. It now sees only untenanted announcements.
///
/// Inverse: restore `tenants_may_meet` in the flood (first assertion
/// fails) or in the replay (second fails).
#[tokio::test]
async fn a_provisional_session_sees_no_tenants_announcements() {
    const SENDER: u64 = 0xA1;
    let node = node().await;
    let ctx = node.dispatch_ctx();
    install(&ctx, SENDER, "127.0.0.1:9".parse().unwrap(), Some(GAME_A));
    install(&ctx, 0x0F, "127.0.0.1:9".parse().unwrap(), None);
    let (unenrolled, unenrolled_addr) = socket().await;
    install(&ctx, 0xC1, unenrolled_addr, None);
    make_provisional(&ctx, 0xC1);

    MeshNode::forward_capability_announcement(b"ann".to_vec(), SENDER, SENDER, &ctx);
    assert_eq!(
        frames(&unenrolled, Duration::from_millis(800)).await,
        0,
        "game A's announcement is not flooded to an unenrolled session"
    );

    hold(&ctx, SENDER, None);
    hold(&ctx, 0x0F, None);
    MeshNode::replay_relay_announcements_once(
        0xC1,
        &ctx.relay_announcements,
        &ctx.peers,
        &ctx.sink,
    )
    .await;
    assert_eq!(
        frames(&unenrolled, Duration::from_millis(800)).await,
        1,
        "replay to an unenrolled session carries the native origin's announcement only"
    );
}

/// **Security: a held announcement keeps its origin's tenant after the
/// origin leaves.** The replay used to look the origin's tenant up in the
/// live peer table; once a game-A host's session closed, its still
/// unexpired announcement resolved to "no tenant" and was replayed to
/// every game-B player that attached.
///
/// Inverse: replay on the live lookup alone (ignore the recorded tenant)
/// and the game-B player receives the departed host's announcement.
#[tokio::test]
async fn a_departed_origins_announcement_stays_in_its_tenant() {
    let node = node().await;
    let ctx = node.dispatch_ctx();
    hold(&ctx, 0xA1, Some(GAME_A));
    // 0xA1 is NOT in the peer table: its session has closed.
    let (b_player, b_addr) = socket().await;
    install(&ctx, 0xB2, b_addr, Some(GAME_B));
    MeshNode::replay_relay_announcements_once(
        0xB2,
        &ctx.relay_announcements,
        &ctx.peers,
        &ctx.sink,
    )
    .await;
    assert_eq!(
        frames(&b_player, Duration::from_millis(800)).await,
        0,
        "game B is never replayed a departed game-A origin's announcement"
    );
}

/// The hold path records the origin's tenant while its session is live —
/// the half of the rule above that the replay relies on.
///
/// Inverse: hold without recording the tenant and the held entry's tenant
/// is `None`.
#[tokio::test]
async fn holding_an_announcement_records_its_origins_tenant() {
    let node = node().await;
    let ctx = node.dispatch_ctx();
    let keypair = EntityKeypair::generate();
    let origin = keypair.entity_id().node_id();
    install(&ctx, origin, "127.0.0.1:9".parse().unwrap(), Some(GAME_A));
    let mut ann = CapabilityAnnouncement::new(
        origin,
        keypair.entity_id().clone(),
        1,
        crate::adapter::net::behavior::capability::CapabilitySet::new(),
    );
    ann.sign(&keypair);
    MeshNode::handle_capability_announcement(&ann.to_bytes(), origin, &ctx);
    let held = ctx
        .relay_announcements
        .get(&origin)
        .map(|e| e.value().tenant)
        .expect("a signed direct announcement is held for replay");
    assert_eq!(held, Some(GAME_A));
}

/// A one-link enrollment grant `root → subject`, serialized as the
/// enrollment service answers it.
fn grant_for(subject: &EntityId) -> Vec<u8> {
    TokenChain::single(PermissionToken::issue(
        &EntityKeypair::generate(),
        subject.clone(),
        TokenScope::SUBSCRIBE,
        0x1234,
        3600,
        0,
    ))
    .to_bytes()
}

/// ANCHOR-03: with subject binding on, an Admitted grant binds only
/// the session whose participant it names — the grant's leaf subject
/// must derive the session's node id. A grant for another device, or
/// one that does not parse, binds nothing (promote nothing); with
/// binding off the legacy behaviour is unchanged.
///
/// Inverse: drop the `subject.node_id() != node_id` comparison and the
/// stranger's grant binds.
#[tokio::test]
async fn an_enrollment_grant_binds_only_the_participant_it_names() {
    let node = node().await;
    let device = EntityKeypair::generate();
    let stranger = EntityKeypair::generate();
    let session_of_device = device.entity_id().node_id();
    let session_of_stranger = stranger.entity_id().node_id();
    let grant = grant_for(device.entity_id());

    assert_eq!(
        node.enrollment_grant_device(session_of_stranger, Some(&grant)),
        Ok(None),
        "binding off: unchanged, nothing recorded"
    );
    node.set_enrollment_subject_binding(true);
    assert_eq!(
        node.enrollment_grant_device(session_of_device, Some(&grant)),
        Ok(Some(device.entity_id().clone())),
        "the device's own session is bound to it"
    );
    assert_eq!(
        node.enrollment_grant_device(session_of_stranger, Some(&grant)),
        Err(()),
        "device D's grant must not promote a session that is not D"
    );
    assert_eq!(
        node.enrollment_grant_device(session_of_device, Some(b"not a chain")),
        Err(()),
        "an unreadable grant binds nothing"
    );
    assert_eq!(
        node.enrollment_grant_device(session_of_device, None),
        Err(())
    );
}

/// ANCHOR-03: a session admitted on a grant announces as the granted
/// device and as nobody else. The direct-announcement path already ties
/// `entity.node_id()` to the session's node id; this holds the admitted
/// session to the exact device its grant named.
///
/// Inverse: drop the granted-device check from
/// `handle_capability_announcement` and the other entity is held.
#[tokio::test]
async fn an_admitted_session_announces_only_as_its_granted_device() {
    let node = node().await;
    let ctx = node.dispatch_ctx();
    let speaker = EntityKeypair::generate();
    let origin = speaker.entity_id().node_id();
    let announce = |ctx: &DispatchCtx| {
        let mut ann = CapabilityAnnouncement::new(
            origin,
            speaker.entity_id().clone(),
            1,
            crate::adapter::net::behavior::capability::CapabilitySet::new(),
        );
        ann.sign(&speaker);
        MeshNode::handle_capability_announcement(&ann.to_bytes(), origin, ctx);
    };
    let admit_as = |ctx: &DispatchCtx, device: [u8; 32]| {
        if let Some(mut entry) = ctx.peers.get_mut(&origin) {
            entry.admission = PeerAdmission::Admitted {
                promoted_at: std::time::Instant::now(),
                session_id: origin,
                tenant: None,
                device: Some(device),
                enrolled_origin: None,
            };
        }
    };

    install(&ctx, origin, "127.0.0.1:9".parse().unwrap(), None);
    admit_as(&ctx, *EntityKeypair::generate().entity_id().as_bytes());
    announce(&ctx);
    assert!(
        !ctx.relay_announcements.contains_key(&origin),
        "an entity other than the granted device is not ingested"
    );
    assert!(node.peer_entity_id(origin).is_none(), "nor pinned");

    admit_as(&ctx, *speaker.entity_id().as_bytes());
    announce(&ctx);
    assert!(
        ctx.relay_announcements.contains_key(&origin),
        "the positive control: the granted device's own announcement is held"
    );
}

/// ANCHOR-04, concurrently: the witness below promotes sequentially, so a
/// check-then-commit promotion (count, then promote, under no common lock)
/// would pass it too. Here eight promotions for one tenant capped at 1 are
/// released together on separate threads; exactly one may take the slot.
///
/// Inverse: release the admission lock between the count and the promotion
/// in `promote_admission_with_grant` and more than one promotes.
#[tokio::test]
async fn concurrent_promotions_never_exceed_a_tenant_cap() {
    let node = node().await;
    let ctx = node.dispatch_ctx();
    node.set_enrollment_tenant_cap(Some(Arc::new(|tenant| (tenant == GAME_A).then_some(1))));
    let peers: Vec<(u64, SocketAddr)> = (0..8u64)
        .map(|i| {
            (
                0xE0 + i,
                format!("127.0.0.1:{}", 21 + i).parse().expect("addr"),
            )
        })
        .collect();
    for &(peer, addr) in &peers {
        install(&ctx, peer, addr, None);
        make_provisional(&ctx, peer);
    }
    let barrier = std::sync::Barrier::new(peers.len());
    let admitted = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|s| {
        for &(peer, addr) in &peers {
            let (node, barrier, admitted) = (&node, &barrier, &admitted);
            s.spawn(move || {
                let session = node.peer_session_id(peer).expect("session");
                barrier.wait();
                if node.promote_admission_with_tenant(
                    peer,
                    session,
                    PeerAddr::Udp(addr),
                    Some(GAME_A),
                ) {
                    admitted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            });
        }
    });
    assert_eq!(
        admitted.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "concurrent promotions must not exceed the cap"
    );
    assert_eq!(node.tenant_peer_count(GAME_A), 1);
}

/// ANCHOR-04: a tenant's player cap is enforced AT promotion,
/// atomically with it. The enrollment handler's occupancy check is a
/// per-call preflight: with a cap of 1 and nobody admitted, two
/// visitors answered concurrently both saw room, both responses
/// promoted, and the game held two. Here two provisional sessions are
/// promoted for the same capped tenant — exactly what those two
/// responses do — and only the first is admitted; another tenant is
/// unaffected; and the slot comes back when the admitted session goes.
///
/// The race itself (both handlers observing zero) is not reproduced:
/// the invariant tested is that the count and the promotion are one
/// step, so whichever promotion runs second sees the first.
///
/// Inverse: drop the cap check from `promote_admission_with_grant`
/// and the second promotion succeeds.
#[tokio::test]
async fn a_capped_tenant_is_full_at_promotion_not_only_at_the_handler() {
    let node = node().await;
    let ctx = node.dispatch_ctx();
    node.set_enrollment_tenant_cap(Some(Arc::new(|tenant| (tenant == GAME_A).then_some(1))));
    let addrs: [SocketAddr; 4] = [
        "127.0.0.1:11".parse().unwrap(),
        "127.0.0.1:12".parse().unwrap(),
        "127.0.0.1:13".parse().unwrap(),
        "127.0.0.1:14".parse().unwrap(),
    ];
    for (peer, addr) in [0xD1u64, 0xD2, 0xD3, 0xD4].into_iter().zip(addrs) {
        install(&ctx, peer, addr, None);
        make_provisional(&ctx, peer);
    }
    let promote = |peer: u64, addr: SocketAddr, tenant| {
        let session = node.peer_session_id(peer).expect("session");
        node.promote_admission_with_tenant(peer, session, PeerAddr::Udp(addr), Some(tenant))
    };

    assert!(
        promote(0xD1, addrs[0], GAME_A),
        "the first player takes the slot"
    );
    assert!(
        !promote(0xD2, addrs[1], GAME_A),
        "a second concurrent admission must not exceed the cap"
    );
    assert!(node.peer_is_provisional(0xD2), "it stays provisional");
    assert_eq!(node.tenant_peer_count(GAME_A), 1);
    assert!(
        promote(0xD3, addrs[2], GAME_B),
        "an uncapped tenant is not affected"
    );

    // The admitted player leaves: its slot is free again.
    ctx.peers.remove(&0xD1);
    assert!(
        promote(0xD4, addrs[3], GAME_A),
        "a released slot is reusable"
    );
    assert_eq!(node.tenant_peer_count(GAME_A), 1);
}

/// **The promotion an Admitted enrollment reply follows does not cost
/// the reply its subscription** (CPOL-01 × §12).
///
/// A provisional session subscribes to its own enrollment reply channel
/// under the bootstrap exception: it has no pinned identity, so the
/// origin binding is judged by the origin the session bound. The
/// response path promotes the session and only then publishes the
/// Admitted reply on that channel. The fan-out's policy re-check asked
/// for the subscriber's origin again — the session was no longer
/// provisional and still unpinned, so it got none, the origin binding
/// refused, and the subscription was evicted with the reply that had
/// just enrolled the session: every real enrollment timed out (the SDK
/// `enrollment_over_rtc` witnesses, the browser acceptance run).
///
/// Inverse: drop the `enrolled_reply_origin` fallback from
/// `MeshNode::subscription_origin` (or stop recording the origin at
/// promotion) and the post-promotion assertion fails.
#[tokio::test]
async fn a_promoted_session_keeps_its_enrollment_reply_origin_for_the_fan_out() {
    use crate::adapter::net::rtc::enroll_reply_channel;

    let node = node().await;
    let ctx = node.dispatch_ctx();
    let addr: SocketAddr = "127.0.0.1:9".parse().unwrap();
    let origin = 0x0123_4567_89ab_cdefu64;
    install(&ctx, 0xC2, addr, None);
    make_provisional(&ctx, 0xC2);
    if let Some(mut entry) = ctx.peers.get_mut(&0xC2) {
        assert!(entry.admission.bind_origin(origin), "first claim binds");
    }
    let reply = ChannelName::new(&enroll_reply_channel(origin)).unwrap();
    let foreign = ChannelName::new(&enroll_reply_channel(origin ^ 1)).unwrap();
    assert_eq!(
        node.subscription_origin(0xC2, &reply),
        Some(origin),
        "provisional: the bootstrap exception the subscribe gate admitted it under"
    );

    let session = node.peer_session_id(0xC2).expect("session");
    assert!(node.promote_admission_with_tenant(0xC2, session, PeerAddr::Udp(addr), None));
    assert!(!node.peer_is_provisional(0xC2));
    assert!(
        node.peer_entity_ids.get(&0xC2).is_none(),
        "the premise: promoted, not yet announced, so not pinned"
    );
    assert_eq!(
        node.subscription_origin(0xC2, &reply),
        Some(origin),
        "admitted and unpinned: its reply subscription is judged by the origin it \
         was admitted under, so the Admitted reply is delivered"
    );
    assert_eq!(
        node.subscription_origin(0xC2, &foreign),
        None,
        "and only on its own reply channel — no other origin is vouched for"
    );
}
