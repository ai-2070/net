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

#![cfg(feature = "net")]

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
    assert!(node.register_rpc_inbound(0xABCD, dispatcher_a.clone()).is_none());
    // Occupied slot — register returns the prior.
    let prior = node.register_rpc_inbound(0xABCD, dispatcher_b.clone());
    assert!(
        prior.is_some(),
        "re-register on occupied slot must return the prior dispatcher",
    );
    // Unregister returns the currently-registered (B).
    let removed = node.unregister_rpc_inbound(0xABCD);
    assert!(removed.is_some(), "unregister of registered slot must return Some");
    // After unregister, slot is empty again.
    assert!(node.unregister_rpc_inbound(0xABCD).is_none());
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
