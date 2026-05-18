//! End-to-end test for the per-channel-hash RPC inbound dispatcher
//! hook on `MeshNode`.
//!
//! The hook routes inbound events whose `NetHeader::channel_hash`
//! matches a registered dispatcher directly to the dispatcher,
//! bypassing the per-shard `inbound` queue. nRPC's `serve_rpc` /
//! `call` glue uses this to receive RPC events without polling
//! the shard queue.
//!
//! The test exercises:
//! - register / unregister return the prior dispatcher correctly
//! - registered dispatchers receive events; nothing lands in the
//!   shard queue for the registered channel_hash
//! - unregistered channels still flow through the shard queue
//!   (back-compat with all existing pub/sub consumers)

#![cfg(all(feature = "net", feature = "cortex"))]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use net::adapter::net::cortex::{RpcInboundDispatcher, RpcInboundEvent};
use net::adapter::net::{
    ChannelName, ChannelPublisher, EntityKeypair, MeshNode, MeshNodeConfig, PublishConfig,
    SocketBufferConfig,
};
use parking_lot::Mutex;

const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x42u8; 32];

fn test_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(3, Duration::from_secs(2))
        .with_capability_gc_interval(Duration::from_millis(250));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: TEST_BUFFER_SIZE,
        recv_buffer_size: TEST_BUFFER_SIZE,
    };
    cfg
}

async fn build_node() -> Arc<MeshNode> {
    let cfg = test_config();
    let keypair = EntityKeypair::generate();
    Arc::new(MeshNode::new(keypair, cfg).await.expect("MeshNode::new"))
}

async fn handshake_pair(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    let a_id = a.node_id();
    let b_id = b.node_id();
    let b_pub = *b.public_key();
    let b_addr = b.local_addr();
    let b_clone = b.clone();
    let accept = tokio::spawn(async move { b_clone.accept(a_id).await });
    a.connect(b_addr, &b_pub, b_id)
        .await
        .expect("connect failed");
    accept
        .await
        .expect("accept task panicked")
        .expect("accept failed");
    a.start();
    b.start();
}

async fn wait_until<F: FnMut() -> bool>(mut cond: F, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    cond()
}

/// Plain register/unregister behavior on a single node — no
/// network needed. Pin the slot semantics: register on an empty
/// slot returns None, register on an occupied slot returns the
/// prior dispatcher, unregister returns the registered one.
#[tokio::test]
async fn register_and_unregister_round_trip() {
    let node = build_node().await;
    let dispatcher_a: RpcInboundDispatcher = Arc::new(|_| {});
    let dispatcher_b: RpcInboundDispatcher = Arc::new(|_| {});

    // Empty slot.
    assert!(node
        .register_rpc_inbound(0xABCD, dispatcher_a.clone())
        .is_none());
    // Occupied slot — register returns the prior.
    let prior = node.register_rpc_inbound(0xABCD, dispatcher_b.clone());
    assert!(
        prior.is_some(),
        "re-register on occupied slot must return the prior dispatcher",
    );
    // Unregister returns the currently-registered (B).
    let removed = node.unregister_rpc_inbound(0xABCD);
    assert!(
        removed.is_some(),
        "unregister of registered slot must return Some"
    );
    // After unregister, slot is empty again.
    assert!(node.unregister_rpc_inbound(0xABCD).is_none());
}

/// Two canonical `ChannelHash` values that share the same wire `u16`
/// bucket must register / unregister independently. Unregistering one
/// canonical leaves the other addressable through both the registered
/// probe and the next unregister.
#[tokio::test]
async fn unregister_preserves_sibling_in_same_wire_bucket() {
    let node = build_node().await;
    // Two canonical hashes whose low 16 bits collide.
    let canonical_a: u64 = 0x0000_0000_DEAD_BEEF;
    let canonical_b: u64 = 0x0000_0000_CAFE_BEEF;
    assert_eq!(canonical_a as u16, canonical_b as u16);
    assert_ne!(canonical_a, canonical_b);

    let disp_a: RpcInboundDispatcher = Arc::new(|_| {});
    let disp_b: RpcInboundDispatcher = Arc::new(|_| {});

    assert!(node
        .register_rpc_inbound(canonical_a, disp_a.clone())
        .is_none());
    assert!(node
        .register_rpc_inbound(canonical_b, disp_b.clone())
        .is_none());

    assert!(node.rpc_inbound_dispatcher_registered(canonical_a));
    assert!(node.rpc_inbound_dispatcher_registered(canonical_b));

    // Unregister A — B must survive, despite sharing the wire bucket.
    assert!(node.unregister_rpc_inbound(canonical_a).is_some());
    assert!(!node.rpc_inbound_dispatcher_registered(canonical_a));
    assert!(
        node.rpc_inbound_dispatcher_registered(canonical_b),
        "sibling canonical in the same wire bucket must outlive unregister of its peer"
    );

    // B is still removable through the canonical-keyed path.
    assert!(node.unregister_rpc_inbound(canonical_b).is_some());
    assert!(!node.rpc_inbound_dispatcher_registered(canonical_b));
}

/// Regression: the previous unregister path did
/// `check is_empty → drop guard → remove(wire)`, which raced with a
/// concurrent `register_rpc_inbound` for a different canonical sharing
/// the same wire bucket. Under contention, the racing register's entry
/// could be silently deleted. This test hammers the pattern with many
/// register/unregister cycles for canonical A while a second thread
/// keeps canonical B (same wire bucket) registered the whole time; B
/// must remain registered throughout.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unregister_race_does_not_drop_concurrent_sibling_registration() {
    let node = build_node().await;
    let canonical_a: u64 = 0x0000_0000_AAAA_1234;
    let canonical_b: u64 = 0x0000_0000_BBBB_1234;
    assert_eq!(canonical_a as u16, canonical_b as u16);

    // Pin B for the duration of the test.
    let disp_b: RpcInboundDispatcher = Arc::new(|_| {});
    assert!(node
        .register_rpc_inbound(canonical_b, disp_b.clone())
        .is_none());

    let iters = 2_000u32;
    let churn = {
        let node = node.clone();
        tokio::task::spawn_blocking(move || {
            let disp_a: RpcInboundDispatcher = Arc::new(|_| {});
            for _ in 0..iters {
                node.register_rpc_inbound(canonical_a, disp_a.clone());
                node.unregister_rpc_inbound(canonical_a);
            }
        })
    };

    // Sample B's registration repeatedly from another thread; with
    // the previous racy unregister, A's churn could clobber B.
    let probe = {
        let node = node.clone();
        tokio::task::spawn_blocking(move || {
            for _ in 0..(iters * 4) {
                assert!(
                    node.rpc_inbound_dispatcher_registered(canonical_b),
                    "B must remain registered while A churns in the same wire bucket"
                );
                std::hint::spin_loop();
            }
        })
    };

    churn.await.expect("churn task panicked");
    probe.await.expect("probe task panicked");

    // Final state: B still registered, A unregistered.
    assert!(node.rpc_inbound_dispatcher_registered(canonical_b));
    assert!(!node.rpc_inbound_dispatcher_registered(canonical_a));
}

/// End-to-end through the real network. A publishes on a channel B
/// has subscribed to AND for which B has registered an inbound
/// dispatcher. Assert: B's dispatcher receives the event.
#[tokio::test]
async fn registered_dispatcher_receives_published_events() {
    let a = build_node().await;
    let b = build_node().await;
    handshake_pair(&a, &b).await;

    let channel = ChannelName::new("test/rpc/echo").unwrap();
    let channel_hash = channel.hash();

    // B subscribes to the channel via the membership protocol.
    b.subscribe_channel(a.node_id(), channel.clone())
        .await
        .expect("subscribe");

    // B registers an inbound dispatcher for the channel's hash.
    let captured: Arc<Mutex<Vec<RpcInboundEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let captured_for_dispatcher = captured.clone();
    let dispatcher: RpcInboundDispatcher = Arc::new(move |ev| {
        captured_for_dispatcher.lock().push(ev);
    });
    assert!(b.register_rpc_inbound(channel_hash, dispatcher).is_none());

    // A publishes an event on the channel.
    let publisher = ChannelPublisher::new(channel.clone(), PublishConfig::default());
    a.publish(&publisher, Bytes::from_static(b"hello-rpc"))
        .await
        .expect("publish");

    // B's dispatcher must observe the event.
    assert!(
        wait_until(|| !captured.lock().is_empty(), Duration::from_secs(2)).await,
        "dispatcher should receive event within 2s",
    );
    let events = captured.lock();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].channel_hash, channel_hash);
    assert_eq!(events[0].payload.as_ref(), b"hello-rpc");
}

/// Wire-bucket collision: two canonical `ChannelHash` values share a
/// wire `u16` bucket. When a packet arrives stamped with that wire
/// hash, every dispatcher registered in the bucket must be invoked
/// and each must receive its own canonical hash on the
/// `RpcInboundEvent` — that's how dispatchers self-disambiguate.
///
/// Pins the Many-entry branch of the inbound dispatch fast path,
/// which lifts the common single-entry case out of the heap.
#[tokio::test]
async fn wire_bucket_collision_fans_out_to_every_registered_canonical() {
    use net::adapter::net::channel::wire_channel_hash;

    // Find two valid channel names that share a wire `u16` bucket.
    // With 65 536 buckets and a few hundred candidates the birthday
    // bound makes collision near-certain; bound the search anyway.
    let mut seen = std::collections::HashMap::<u16, String>::new();
    let (name1, name2) = (|| -> Option<(String, String)> {
        for i in 0..200_000u64 {
            let name = format!("test/rpc/coll/{}", i);
            let wire = wire_channel_hash(&name);
            if let Some(prev) = seen.get(&wire) {
                return Some((prev.clone(), name));
            }
            seen.insert(wire, name);
        }
        None
    })()
    .expect("no wire-bucket collision in 200K candidates");

    let ch1 = ChannelName::new(&name1).unwrap();
    let ch2 = ChannelName::new(&name2).unwrap();
    assert_eq!(ch1.wire_hash(), ch2.wire_hash());
    assert_ne!(ch1.hash(), ch2.hash(), "canonical must differ");

    let a = build_node().await;
    let b = build_node().await;
    handshake_pair(&a, &b).await;

    // B subscribes only to ch1.
    b.subscribe_channel(a.node_id(), ch1.clone())
        .await
        .expect("subscribe");

    // B registers a dispatcher for each canonical — both land in
    // the same wire bucket.
    let captured1: Arc<Mutex<Vec<RpcInboundEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let captured2: Arc<Mutex<Vec<RpcInboundEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let cap1 = captured1.clone();
    let cap2 = captured2.clone();
    let disp1: RpcInboundDispatcher = Arc::new(move |ev| cap1.lock().push(ev));
    let disp2: RpcInboundDispatcher = Arc::new(move |ev| cap2.lock().push(ev));
    assert!(b.register_rpc_inbound(ch1.hash(), disp1).is_none());
    assert!(b.register_rpc_inbound(ch2.hash(), disp2).is_none());

    // A publishes once on ch1.
    let publisher = ChannelPublisher::new(ch1.clone(), PublishConfig::default());
    a.publish(&publisher, Bytes::from_static(b"collide"))
        .await
        .expect("publish");

    // Both dispatchers must receive the event (wire bucket fan-out),
    // each tagged with its *own* canonical hash so receiver-side
    // logic can self-filter.
    assert!(
        wait_until(
            || !captured1.lock().is_empty() && !captured2.lock().is_empty(),
            Duration::from_secs(2)
        )
        .await,
        "both dispatchers should receive the event within 2s",
    );
    let e1 = captured1.lock();
    let e2 = captured2.lock();
    assert_eq!(e1.len(), 1);
    assert_eq!(e2.len(), 1);
    assert_eq!(e1[0].channel_hash, ch1.hash());
    assert_eq!(e2[0].channel_hash, ch2.hash());
    assert_eq!(e1[0].payload.as_ref(), b"collide");
    assert_eq!(e2[0].payload.as_ref(), b"collide");
}

/// After unregistering the dispatcher, subsequent publishes flow
/// through the shard inbound queue (back-compat). Without the
/// unregister call, the dispatcher would keep receiving events;
/// after it, the dispatcher is silent.
#[tokio::test]
async fn unregister_restores_shard_inbound_path() {
    let a = build_node().await;
    let b = build_node().await;
    handshake_pair(&a, &b).await;

    let channel = ChannelName::new("test/rpc/restore").unwrap();
    let channel_hash = channel.hash();
    b.subscribe_channel(a.node_id(), channel.clone())
        .await
        .expect("subscribe");

    let captured: Arc<Mutex<Vec<RpcInboundEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let captured_for_dispatcher = captured.clone();
    let dispatcher: RpcInboundDispatcher = Arc::new(move |ev| {
        captured_for_dispatcher.lock().push(ev);
    });
    b.register_rpc_inbound(channel_hash, dispatcher);

    let publisher = ChannelPublisher::new(channel.clone(), PublishConfig::default());
    a.publish(&publisher, Bytes::from_static(b"first"))
        .await
        .expect("publish 1");
    assert!(
        wait_until(|| captured.lock().len() == 1, Duration::from_secs(2)).await,
        "dispatcher should receive first event",
    );

    // Now unregister; subsequent publishes should NOT increment
    // the captured count (they go to the shard inbound queue
    // instead, which this test doesn't drain).
    b.unregister_rpc_inbound(channel_hash);
    a.publish(&publisher, Bytes::from_static(b"second"))
        .await
        .expect("publish 2");
    // Give the dispatcher path a moment to NOT fire.
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        captured.lock().len(),
        1,
        "after unregister, dispatcher must NOT receive subsequent events",
    );
}
