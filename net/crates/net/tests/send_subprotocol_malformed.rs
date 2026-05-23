//! Integration tests for `MeshNode::send_subprotocol` error /
//! fast-path behavior and receive-loop robustness against
//! malformed subprotocol frames.
//!
//! Pins the invariants from `TEST_COVERAGE_PLAN.md` §P1-1. Happy
//! paths (migration, capability-announcement, rendezvous) are
//! covered by their respective integration suites; this file
//! covers:
//!
//! - `send_subprotocol` to an unknown peer returns a clean
//!   `Connection("unknown peer")` error without touching session
//!   state, and does so consistently across N concurrent tasks
//!   (no lock-contention-induced Ok, no panic, no corruption).
//! - `send_subprotocol` to a `block_peer`'d address returns
//!   `Ok(())` silently (partition-filter short-circuit — already
//!   documented behavior, pinned here so it doesn't regress).
//! - An empty payload still produces a valid wire packet (no
//!   panic in `build_subprotocol`'s event-frame assembly).
//! - A reserved-range (`0x0001..0x03FF`) subprotocol ID is
//!   accepted by the send path — the sender doesn't police IDs;
//!   the receiver drops unknown IDs silently.
//! - A vendor-range (`0x1000..0xEFFF`) subprotocol ID round-trips
//!   without dispatch-branch panics.
//! - An unknown subprotocol ID sent to a running mesh receiver
//!   does not crash the receive loop — it falls through to the
//!   standard event path, which treats the payload as a generic
//!   event frame.
//!
//! Run: `cargo test --features net --test send_subprotocol_malformed`

#![cfg(feature = "net")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};
use net::error::AdapterError;

const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x42u8; 32];

fn test_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(3, Duration::from_secs(2));
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

async fn connect_pair(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    let a_id = a.node_id();
    let b_pub = *b.public_key();
    let b_addr = b.local_addr();
    let b_id = b.node_id();
    let b_clone = b.clone();
    let accept = tokio::spawn(async move { b_clone.accept(a_id).await });
    a.connect(b_addr, &b_pub, b_id)
        .await
        .expect("connect failed");
    accept
        .await
        .expect("accept task panicked")
        .expect("accept failed");
}

// ─────────────────────────────────────────────────────────────
// SEND-SIDE INVARIANTS
// ─────────────────────────────────────────────────────────────

/// A freshly-built mesh with no connected peers returns a clean
/// `Connection("unknown peer")` error for every send — not a
/// panic, not a silent Ok. Pins the addr-to-node lookup's
/// ok_or-else path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_to_unknown_peer_returns_clean_error() {
    let a = build_node().await;
    // No connect_pair call — A has no peers, addr_to_node is empty.

    let bogus: SocketAddr = "127.0.0.1:1".parse().unwrap();
    let err = a
        .send_subprotocol(bogus, 0x0500, b"payload")
        .await
        .expect_err("send to unknown peer must fail");

    match err {
        AdapterError::Connection(msg) => {
            assert!(
                msg.contains("unknown peer"),
                "error message should mention unknown peer; got {msg:?}",
            );
        }
        other => panic!("expected Connection error; got {other:?}"),
    }
}

/// Sixteen concurrent tasks all sending to an unknown peer
/// should all receive the same clean `Connection("unknown peer")`
/// error. Guards against a future refactor that accidentally
/// uses `unwrap` on the addr lookup, or serializes the send
/// path through a mutex whose contention would hide the error.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_send_to_unknown_peer_is_consistently_errored() {
    let a = build_node().await;
    let bogus: SocketAddr = "127.0.0.1:2".parse().unwrap();

    let mut handles = Vec::with_capacity(16);
    for _ in 0..16 {
        let a_clone = a.clone();
        handles.push(tokio::spawn(async move {
            a_clone.send_subprotocol(bogus, 0x0500, b"payload").await
        }));
    }

    let mut ok_count = 0;
    let mut err_count = 0;
    for h in handles {
        match h.await.expect("task panicked") {
            Ok(()) => ok_count += 1,
            Err(AdapterError::Connection(msg)) => {
                assert!(
                    msg.contains("unknown peer"),
                    "every concurrent failure must be the unknown-peer error; got {msg:?}",
                );
                err_count += 1;
            }
            Err(other) => panic!("unexpected error variant: {other:?}"),
        }
    }
    assert_eq!(
        ok_count, 0,
        "no concurrent send should have spuriously succeeded"
    );
    assert_eq!(
        err_count, 16,
        "every concurrent send should have errored cleanly"
    );
}

/// `block_peer(addr)` adds the addr to the partition filter.
/// `send_subprotocol` against a partition-filtered addr returns
/// `Ok(())` silently — the network partition simulation pretends
/// the packet was sent but the peer didn't get it. Pin this
/// behavior so a future "surface partition-filter drops as
/// errors" change is a deliberate, visible choice.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_to_partition_filtered_peer_returns_ok_silently() {
    let a = build_node().await;
    let b = build_node().await;
    connect_pair(&a, &b).await;
    a.start();
    b.start();

    let b_addr = b.local_addr();
    a.block_peer(b_addr);
    assert!(a.is_blocked(&b_addr));

    // Send to the blocked peer — returns Ok, but nothing reaches B.
    let res = a.send_subprotocol(b_addr, 0x0500, b"payload").await;
    assert!(
        res.is_ok(),
        "send to partition-filtered peer must be a silent Ok; got {res:?}",
    );
}

/// An empty payload produces a valid subprotocol packet. Guards
/// the event-frame assembly against a panic on `events = vec![]`
/// or the zero-length Bytes case.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_empty_payload_does_not_panic() {
    let a = build_node().await;
    let b = build_node().await;
    connect_pair(&a, &b).await;
    a.start();
    b.start();

    let b_addr = b.local_addr();
    a.send_subprotocol(b_addr, 0x0500, b"")
        .await
        .expect("empty payload send should succeed");
}

/// A reserved-range (`0x0001..0x03FF`) subprotocol ID reaches the
/// wire — the sender does NOT police IDs. Plan-level contract:
/// the Subprotocol ID Space table in the main README documents
/// `0x0001..0x03FF` as "Reserved for core", but that's an
/// allocation convention, not a dispatch-time check. The receiver
/// is where unknown IDs get routed to the fall-through event
/// path. Pinning "send accepts any u16" here.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_with_reserved_subprotocol_id_succeeds() {
    let a = build_node().await;
    let b = build_node().await;
    connect_pair(&a, &b).await;
    a.start();
    b.start();

    let b_addr = b.local_addr();
    // 0x0002 — reserved for core, no dispatch handler.
    a.send_subprotocol(b_addr, 0x0002, b"payload")
        .await
        .expect("reserved-range ID must not error at send time");
}

/// A vendor-range (`0x1000..0xEFFF`) subprotocol ID is the
/// documented space for third-party extensions. Sender accepts
/// it cleanly; receiver falls through to the generic event path.
/// Pins "third-party subprotocol allocation" as a first-class
/// supported workflow.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_with_vendor_range_subprotocol_id_succeeds() {
    let a = build_node().await;
    let b = build_node().await;
    connect_pair(&a, &b).await;
    a.start();
    b.start();

    let b_addr = b.local_addr();
    // 0x7777 — arbitrary vendor pick.
    a.send_subprotocol(b_addr, 0x7777, b"vendor payload")
        .await
        .expect("vendor-range ID must not error at send time");
}

/// The upper u16 range (`0xF000..=0xFFFF`) is documented as
/// experimental/ephemeral. Also exercises the boundary case
/// `u16::MAX` so the `subprotocol_id as u64` coercion at the
/// send-path stream-id step is pinned against a future refactor
/// that might introduce signed arithmetic.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_with_max_subprotocol_id_succeeds() {
    let a = build_node().await;
    let b = build_node().await;
    connect_pair(&a, &b).await;
    a.start();
    b.start();

    let b_addr = b.local_addr();
    a.send_subprotocol(b_addr, u16::MAX, b"edge")
        .await
        .expect("u16::MAX subprotocol ID must not overflow or panic");
}

// ─────────────────────────────────────────────────────────────
// RECEIVE-SIDE INVARIANTS
// ─────────────────────────────────────────────────────────────

/// Sending to a running receiver with an unknown subprotocol ID
/// must not crash the receive loop. The dispatch chain's
/// fall-through is the generic event-frame path which queues
/// the payload into a shard. The receiver stays alive + healthy.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn receiver_survives_unknown_subprotocol_id() {
    let a = build_node().await;
    let b = build_node().await;
    connect_pair(&a, &b).await;
    a.start();
    b.start();

    let b_addr = b.local_addr();

    // Send several unknown-ID packets in a row. None should
    // crash B's recv loop.
    for id in [0x0002u16, 0x03FF, 0x1234, 0x7FFF, 0xEFFF] {
        a.send_subprotocol(b_addr, id, b"junk")
            .await
            .expect("send with unknown ID must not error");
    }

    // Give B's recv loop a few scheduler ticks to process the
    // five packets. If any of them had panicked the loop would
    // be dead; we verify the peer is still accepting new sends.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Prove B's recv loop is *actually processing* packets, not
    // just that A's UDP send returns Ok. (Cubic-flagged P2:
    // `send_subprotocol(...).await` only confirms A enqueued to
    // its local socket — a dead recv loop on B would pass that
    // check.) Drive a real capability announcement from A and
    // verify B's capability index observes it — that requires
    // B's recv loop + dispatcher + capability handler all to be
    // alive.
    let a_id = a.node_id();
    a.announce_capabilities(net::adapter::net::behavior::capability::CapabilitySet::new())
        .await
        .expect("real announce after junk barrage");
    let mut propagated = false;
    for _ in 0..40 {
        if b.test_capability_fold_has(a_id) {
            propagated = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        propagated,
        "B's receive loop must survive the unknown-ID barrage and still \
         dispatch a follow-up capability announcement — if B's recv loop \
         had died this would time out",
    );
}

/// A malformed CapabilityAnnouncement payload — wrong magic, wrong
/// length, corrupt signature bytes — arrives on `0x0C00`
/// (capability-announcement) and must be silently dropped by
/// the handler. Pinning that the decoder failure path doesn't
/// poison B's capability index or kill the recv loop.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn receiver_drops_malformed_capability_announcement_without_panic() {
    let a = build_node().await;
    let b = build_node().await;
    connect_pair(&a, &b).await;
    a.start();
    b.start();

    let b_addr = b.local_addr();

    // CapabilityAnnouncement on 0x0C00. Payload is deliberately
    // junk — wrong magic, wrong length, not a valid postcard-
    // encoded announcement. The handler's decode step fails and
    // the packet must be dropped silently.
    a.send_subprotocol(b_addr, 0x0C00, b"not-an-announcement")
        .await
        .expect("send should succeed");
    a.send_subprotocol(b_addr, 0x0C00, &[0u8; 3])
        .await
        .expect("send should succeed on tiny payload");
    a.send_subprotocol(b_addr, 0x0C00, &[0xFFu8; 4096])
        .await
        .expect("send should succeed on large junk payload");

    tokio::time::sleep(Duration::from_millis(100)).await;

    // B's capability index should be empty (no valid announcements
    // arrived). The malformed payloads must not have poisoned it
    // with a bogus entry.
    let a_id = a.node_id();
    assert!(
        !b.test_capability_fold_has(a_id),
        "malformed announcement must not populate B's capability index",
    );

    // And B is still responsive — next real announcement from A
    // lands normally.
    a.announce_capabilities(net::adapter::net::behavior::capability::CapabilitySet::new())
        .await
        .expect("A real announce");
    let mut propagated = false;
    for _ in 0..40 {
        if b.test_capability_fold_has(a_id) {
            propagated = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        propagated,
        "after malformed-payload barrage, B should still index a valid announcement",
    );
}
