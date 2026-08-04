//! The PRODUCTION protected-relay branch does not allocate per packet
//! (`docs/internal/plans/SUBNET_AUTH_PLAN.md` D6/D9; the owed witness
//! named by `subnet_auth_e2e.rs` §6.18).
//!
//! `tests/subnet_route_hop_alloc.rs` proves the sealing *primitive*
//! is allocation-free, and `mesh.rs`'s
//! `protected_forward_allocation_pins` structurally pin that
//! `relay_protected_hop` still calls it — but a structural guard is
//! weaker than a measurement, and nothing before this file measured
//! the branch that actually runs per packet per hop in production:
//! authority snapshot, hop authentication, replay admission, context
//! and route lookups, the transition decision, header mutation,
//! re-tagging into the fixed worker buffer, and the socket send.
//!
//! A bare process-wide counter cannot measure that branch: it runs on
//! a tokio worker thread on which *reception* allocates per packet.
//! Attribution comes from `subnet::alloc_probe` (fixtures-only): the
//! branch marks its own extent, and this file's global allocator
//! charges only allocations made inside that extent, on any thread.
//! `sections_entered` proves the marker actually ran, so a zero count
//! can never be a vacuous pass over a branch that never executed.
//!
//! Like the primitive witness, this binary holds exactly ONE `#[test]`:
//! a `#[global_allocator]` is process-wide, and a second concurrent
//! test would charge its relay sections to this one's counter.
//!
//! Cheap is only interesting if it is also right, so the measured
//! traffic is verified at the far edge: the last forwarded envelope
//! must open under the egress edge key with the inner packet
//! byte-identical.

#![cfg(all(feature = "net", feature = "fixtures"))]

use std::alloc::{GlobalAlloc, Layout, System};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::identity::EntityKeypair;
use net::adapter::net::subnet::route_hop::ROUTE_HOP_MAGIC;
use net::adapter::net::subnet::{
    admission::unix_now_secs, alloc_probe, SubnetAuthPresentation, SubnetAuthorityConfig,
    SubnetBoundarySet, SubnetCredentialSet, SubnetGrant, SubnetRef, SubnetRights, TopologySubnetId,
};
use net::adapter::net::{MeshNode, MeshNodeConfig, RoutingHeader, SocketBufferConfig};
use tokio::net::UdpSocket;

/// Allocations observed while some thread was inside the production
/// relay branch. Everything outside the branch — reception, timers,
/// the test harness itself — is deliberately invisible to it.
static RELAY_ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

struct RelaySectionCountingAllocator;

// SAFETY: every method forwards directly to `System`, which upholds
// the `GlobalAlloc` contract. The only additions are a read of a
// const-initialized thread-local flag and a relaxed counter increment,
// neither of which allocates, touches allocator state, or affects the
// returned pointers.
unsafe impl GlobalAlloc for RelaySectionCountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if alloc_probe::in_relay_section() {
            RELAY_ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: `layout` is forwarded unchanged from our caller,
        // which is required to have supplied a valid one.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr`/`layout` come from a matching `alloc` in this
        // same allocator, which forwarded to `System`.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if alloc_probe::in_relay_section() {
            RELAY_ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: as `dealloc`, plus `new_size` is forwarded unchanged
        // from a caller required to have checked it.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOC: RelaySectionCountingAllocator = RelaySectionCountingAllocator;

const PSK: [u8; 32] = [0x64u8; 32];
const DAY: u64 = 24 * 60 * 60;

/// How many valid hops are relayed before the count starts, charging
/// every lazy one-time initialization on the relay path — thread-local
/// forward buffer, map shard growth, socket path — to the warm-up.
const WARMUP_PACKETS: u64 = 64;
/// Steady-state hops measured for the allocation claim.
const MEASURED_PACKETS: u64 = 256;

fn root() -> EntityKeypair {
    EntityKeypair::from_bytes([0xC1; 32])
}

fn cfg(attachment: Option<&[u8]>) -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    // Session timeout far above the test's total runtime: the watcher
    // interposes on the gateway→right address for the whole measured
    // window, so with a short timeout the starved peering would be
    // evicted before the final open-under-the-egress-key check.
    let mut c = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(60))
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

/// Wait until the gateway's forwarded counter reaches `target`, or
/// panic with a diagnostic if it stalls: a stall here means valid
/// hops stopped being relayed, which is its own regression.
async fn wait_forwarded(gw: &Arc<MeshNode>, target: u64, why: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let forwarded = gw.protected_relay_stats().forwarded();
        if forwarded >= target {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{why}: forwarded stalled at {forwarded}/{target}",
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// A realistic spread of inner end-to-end packets. Every inner must be
/// at least `protocol::HEADER_SIZE` (68) bytes so dispatch on the far
/// side would classify it as a packet rather than noise, and the top
/// size sits near a 1500-byte MTU minus envelope overhead.
fn inners() -> Vec<Vec<u8>> {
    [96usize, 512, 1200]
        .iter()
        .map(|&n| (0..n).map(|i| (i % 251) as u8).collect())
        .collect()
}

/// One valid protected hop per call, sealed on the genuine left↔gateway
/// edge with a fresh sequence, sent from an unrelated socket (ingress
/// identity is the hop session, never the UDP source).
async fn send_hops(
    left: &Arc<MeshNode>,
    gw: &Arc<MeshNode>,
    right_id: u64,
    sock: &UdpSocket,
    count: u64,
    inner_set: &[Vec<u8>],
) {
    for i in 0..count {
        let header = RoutingHeader::new(right_id, left.node_id() as u32, 8);
        let inner = &inner_set[(i as usize) % inner_set.len()];
        let envelope = left
            .seal_route_hop_to_peer(gw.node_id(), &header, inner)
            .expect("left has a session to the gateway");
        sock.send_to(&envelope, gw.local_addr())
            .await
            .expect("send");
        // Pace lightly so loopback receive buffers never drop: a lost
        // envelope would stall the forwarded counter, not corrupt the
        // measurement, but the witness should not flake on burst loss.
        if i % 32 == 31 {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }
}

/// Steady-state protected forwarding through the production relay
/// branch allocates nothing — measured, not inferred from structure.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_relay_steady_state_allocates_nothing() {
    // --- Topology: left ⇄ gateway ⇄ right, all real nodes. ---------
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
    // Both attachments are inside the vehicle subtree: an internal
    // transition governed by ROUTE, with an explicitly empty declared
    // boundary inventory.
    gw.declare_subnet_boundaries(SubnetBoundarySet::new(root().entity_id().clone(), 0, []));

    // Observe the egress without disturbing `right`'s session state,
    // so the re-tagged envelope stays verifiable under its edge key.
    let watcher = UdpSocket::bind("127.0.0.1:0").await.expect("bind");
    assert!(
        gw.set_peer_addr_for_test(right.node_id(), watcher.local_addr().expect("addr")),
        "the egress peer must exist",
    );

    let sender = UdpSocket::bind("127.0.0.1:0").await.expect("bind");
    let inner_set = inners();

    // --- Warm-up: charge one-time initialization before counting. --
    send_hops(
        &left,
        &gw,
        right.node_id(),
        &sender,
        WARMUP_PACKETS,
        &inner_set,
    )
    .await;
    wait_forwarded(&gw, WARMUP_PACKETS, "warm-up").await;

    // --- The measured steady state. --------------------------------
    let allocs_before = RELAY_ALLOCATIONS.load(Ordering::Relaxed);
    let entered_before = alloc_probe::sections_entered();
    let forwarded_before = gw.protected_relay_stats().forwarded();

    send_hops(
        &left,
        &gw,
        right.node_id(),
        &sender,
        MEASURED_PACKETS,
        &inner_set,
    )
    .await;
    wait_forwarded(&gw, forwarded_before + MEASURED_PACKETS, "measured window").await;

    let allocations = RELAY_ALLOCATIONS.load(Ordering::Relaxed) - allocs_before;
    let entered = alloc_probe::sections_entered() - entered_before;
    let forwarded = gw.protected_relay_stats().forwarded() - forwarded_before;

    // The branch demonstrably ran, once per measured hop.
    assert_eq!(
        forwarded, MEASURED_PACKETS,
        "every measured hop must be forwarded by the production branch",
    );
    assert!(
        entered >= MEASURED_PACKETS,
        "the measured section must have covered the relay branch \
         (entered {entered}, expected at least {MEASURED_PACKETS}) — \
         a zero allocation count over an unexecuted branch would be vacuous",
    );
    // The claim itself.
    assert_eq!(
        allocations, 0,
        "relaying {MEASURED_PACKETS} hops through the production branch \
         must not allocate; saw {allocations} allocator calls inside the \
         relay section",
    );

    // --- Cheap is only interesting if it is also right. ------------
    // Drain the watcher and verify a forwarded envelope under the
    // egress edge key: still a protected envelope, inner intact. The
    // drain is bounded by a hard overall deadline — the gateway keeps
    // sending its ordinary peer traffic (heartbeats) at the watcher,
    // so "loop until quiet" alone would spin for as long as the
    // peering lives. Buffered envelopes arrive instantly; a 50 ms
    // quiet gap or the deadline ends the drain, whichever is first.
    let mut last_hop: Option<Vec<u8>> = None;
    let mut buf = vec![0u8; 4096];
    let drain_deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < drain_deadline {
        match tokio::time::timeout(Duration::from_millis(50), watcher.recv_from(&mut buf)).await {
            Ok(Ok((n, _))) => {
                let datagram = &buf[..n];
                if datagram.len() >= 2
                    && u16::from_le_bytes([datagram[0], datagram[1]]) == ROUTE_HOP_MAGIC
                {
                    last_hop = Some(datagram.to_vec());
                }
            }
            _ => break,
        }
    }
    let last_hop = last_hop.expect("at least one forwarded envelope must reach the watcher");
    let (out_header, out_inner) = right
        .open_route_hop_from_peer(gw.node_id(), &last_hop)
        .expect("the forwarded hop verifies under the egress edge key");
    assert_eq!(out_header.dest_id, right.node_id());
    assert!(
        inner_set.contains(&out_inner),
        "the relay must not touch the inner packet",
    );
}
