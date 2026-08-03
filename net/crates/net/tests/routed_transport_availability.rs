//! A peer reached by the ROUTED handshake must be reachable.
//!
//! `addr_to_node` answers "which peer OWNS this address". A relay's
//! address does not belong to the endpoint behind it, so a routed
//! session must not be published there — three security predicates
//! (direct-hop resolution, withdrawal ownership, protected promotion)
//! read that index to decide adjacency, and a shared relay tuple cannot
//! be expressed as ownership in a single-valued reverse map.
//!
//! Removing that write without also moving the SEND path off the index
//! made every routed peer unreachable: `send_subprotocol` /
//! `send_to_peer` resolved node → address → node, and the second half
//! of that round trip missed. Every caller that already held a node id
//! was laundering it through an address that could not name its peer.
//!
//! The repair is [`PeerTransport`] — durable per-peer state recording
//! what the address IS — plus node-keyed send primitives that read it.
//! These tests pin the availability half of that contract, so a future
//! tightening of the ownership index cannot silently take reachability
//! with it again.
//!
//! The degenerate single-hop case (the "relay" is the destination, the
//! CLI remote-attach and SDK enrolment pattern) is the one that broke,
//! because it is exactly where "nobody claims this address" used to be
//! read as "the endpoint owns it".

#![cfg(feature = "net")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::{ChannelName, EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};

const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x42u8; 32];

fn test_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(500))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(3, Duration::from_secs(3));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: TEST_BUFFER_SIZE,
        recv_buffer_size: TEST_BUFFER_SIZE,
    };
    cfg
}

async fn build_node() -> Arc<MeshNode> {
    Arc::new(
        MeshNode::new(EntityKeypair::generate(), test_config())
            .await
            .expect("MeshNode::new"),
    )
}

/// A single-hop `connect_via` session must carry real traffic.
///
/// The observable is a channel-membership round trip: A asks B to add
/// it as a subscriber and waits for B's ack. That exercises the whole
/// path — A resolves the peer, seals the request under the routed
/// session, B decrypts and answers, A matches the ack — rather than
/// merely asserting that a send call returned `Ok` locally.
///
/// Before the send path was moved off the ownership index this failed
/// with `Connection("unknown peer")` at the first send, because nothing
/// claimed B's address on A's side.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_single_hop_routed_session_carries_membership_traffic() {
    let a = build_node().await;
    let b = build_node().await;

    let b_pub = *b.public_key();
    let b_addr = b.local_addr();
    let b_id = b.node_id();

    // B's dispatcher answers the routed msg1; the routed handshake has
    // no pre-`start()` responder path.
    b.start();
    a.start();

    a.connect_via(b_addr, &b_pub, b_id)
        .await
        .expect("routed handshake to a single-hop 'relay' must complete");

    assert!(
        a.peer_count() > 0,
        "the routed session must be registered on A"
    );

    let channel = ChannelName::new("routed.availability").expect("valid channel name");
    tokio::time::timeout(
        Duration::from_secs(5),
        a.subscribe_channel(b_id, channel.clone()),
    )
    .await
    .expect("the membership round trip must not hang")
    .expect(
        "a routed peer must be reachable: the send path resolves the peer's own \
         transport, not an ownership claim on the address it points at",
    );
}

/// The same session must NOT publish an ownership claim on the address
/// it sends to.
///
/// Reachability and ownership are different facts, and the whole point
/// of the repair is that restoring the first does not restore the
/// second. `authenticated_next_hop` resolves only through the protected
/// candidate, which a routed install never writes — so a peer that is
/// reachable here is still not an authenticated adjacency.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_routed_session_is_reachable_without_becoming_an_adjacency() {
    let a = build_node().await;
    let b = build_node().await;

    let b_pub = *b.public_key();
    let b_addr = b.local_addr();
    let b_id = b.node_id();

    b.start();
    a.start();

    a.connect_via(b_addr, &b_pub, b_id)
        .await
        .expect("routed handshake must complete");

    // Reachable...
    let channel = ChannelName::new("routed.no.adjacency").expect("valid channel name");
    tokio::time::timeout(Duration::from_secs(5), a.subscribe_channel(b_id, channel))
        .await
        .expect("no hang")
        .expect("routed peer must be reachable");

    // ...but not adjacent. A routed session authenticates the far
    // endpoint, not whoever owns the address the datagrams go to, so it
    // contributes no protected candidate and protected forwarding fails
    // closed on it.
    assert!(
        a.authenticated_next_hop(b_id).is_none(),
        "a routed session must not resolve as an authenticated adjacency — \
         being able to send to a peer is not evidence about who receives the hop"
    );
}

/// The direct handshake keeps both facts. Same observable as above, but
/// `connect` establishes a real adjacency, so the peer is reachable AND
/// resolves as an authenticated next hop.
///
/// This is the control: it shows the routed case above fails the
/// adjacency assertion because of what the session proves, not because
/// the assertion is unsatisfiable.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_direct_session_is_both_reachable_and_adjacent() {
    let a = build_node().await;
    let b = build_node().await;

    let b_pub = *b.public_key();
    let b_addr = b.local_addr();
    let b_id = b.node_id();
    let a_id = a.node_id();

    let b_clone = b.clone();
    let accept = tokio::spawn(async move { b_clone.accept(a_id).await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    a.start();

    a.connect(b_addr, &b_pub, b_id)
        .await
        .expect("direct handshake must complete");
    accept
        .await
        .expect("accept task panicked")
        .expect("accept must complete");
    b.start();

    let channel = ChannelName::new("direct.availability").expect("valid channel name");
    tokio::time::timeout(Duration::from_secs(5), a.subscribe_channel(b_id, channel))
        .await
        .expect("no hang")
        .expect("direct peer must be reachable");

    let hop = a
        .authenticated_next_hop(b_id)
        .expect("a direct handshake IS an authenticated adjacency");
    assert_eq!(hop.node_id, b_id);
    assert_eq!(hop.addr, b_addr);
}
