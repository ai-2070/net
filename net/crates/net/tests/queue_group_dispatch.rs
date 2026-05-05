//! Real-network test for `SubscriptionMode::QueueGroup` work
//! distribution.
//!
//! `Mesh::publish` consults `roster.dispatch_recipients(channel)` to
//! choose recipients. For `Broadcast` subscribers every published
//! event reaches every subscriber; for `QueueGroup(name)` members,
//! every published event reaches exactly ONE member of the named
//! group, distributed round-robin per the queue group's atomic
//! cursor (see `channel/roster.rs::QueueGroup::select`).
//!
//! Roster-level unit tests pin the data structure; this test pins
//! the END-TO-END behavior through real network publish — the load-
//! bearing claim that two replica servers in the same queue group
//! actually divide a stream of events between them.
//!
//! Three nodes:
//!   - publisher (A): publishes N events on `test/qg`.
//!   - worker B: subscribes to `test/qg` in `QueueGroup("workers")`
//!     from A; counts arrivals via an inbound dispatcher.
//!   - worker C: same.
//!
//! Asserts: total arrivals across B and C equals N, both > 0
//! (round-robin distributes work; no duplicates, no drops).

#![cfg(feature = "net")]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use net::adapter::net::cortex::{RpcInboundDispatcher, RpcInboundEvent};
use net::adapter::net::{
    ChannelName, ChannelPublisher, EntityKeypair, MeshNode, MeshNodeConfig, PublishConfig,
    SocketBufferConfig,
};

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

/// Two queue-group members on different nodes divide a stream of
/// events between them. Total received equals the number published;
/// each member receives a non-zero share.
#[tokio::test]
async fn queue_group_distributes_events_across_two_subscribers() {
    let publisher = build_node().await;
    let worker_b = build_node().await;
    let worker_c = build_node().await;
    handshake_pair(&publisher, &worker_b).await;
    handshake_pair(&publisher, &worker_c).await;

    let channel = ChannelName::new("test/qg/workers").unwrap();
    let channel_hash = channel.hash();
    let group = "workers".to_string();

    // B and C both subscribe to the channel from the publisher
    // under the same queue group.
    worker_b
        .subscribe_channel_in_queue_group(publisher.node_id(), channel.clone(), group.clone())
        .await
        .expect("worker B subscribe");
    worker_c
        .subscribe_channel_in_queue_group(publisher.node_id(), channel.clone(), group.clone())
        .await
        .expect("worker C subscribe");

    // Each worker registers an inbound dispatcher for the channel
    // hash so we can count arrivals without polling the shard
    // inbound queue.
    let count_b = Arc::new(AtomicUsize::new(0));
    let count_c = Arc::new(AtomicUsize::new(0));
    let count_b_for_disp = count_b.clone();
    let dispatcher_b: RpcInboundDispatcher = Arc::new(move |_ev: RpcInboundEvent| {
        count_b_for_disp.fetch_add(1, Ordering::Relaxed);
    });
    let count_c_for_disp = count_c.clone();
    let dispatcher_c: RpcInboundDispatcher = Arc::new(move |_ev: RpcInboundEvent| {
        count_c_for_disp.fetch_add(1, Ordering::Relaxed);
    });
    assert!(worker_b
        .register_rpc_inbound(channel_hash, dispatcher_b)
        .is_none());
    assert!(worker_c
        .register_rpc_inbound(channel_hash, dispatcher_c)
        .is_none());

    // Publish N events.
    let n: usize = 100;
    let pub_handle = ChannelPublisher::new(channel.clone(), PublishConfig::default());
    for i in 0..n {
        publisher
            .publish(&pub_handle, Bytes::from(format!("event-{i}")))
            .await
            .expect("publish");
    }

    // Wait for the dispatchers to drain.
    assert!(
        wait_until(
            || count_b.load(Ordering::Relaxed) + count_c.load(Ordering::Relaxed) == n,
            Duration::from_secs(5),
        )
        .await,
        "expected total {} arrivals across B+C, got {} + {} = {}",
        n,
        count_b.load(Ordering::Relaxed),
        count_c.load(Ordering::Relaxed),
        count_b.load(Ordering::Relaxed) + count_c.load(Ordering::Relaxed),
    );

    let b_arrivals = count_b.load(Ordering::Relaxed);
    let c_arrivals = count_c.load(Ordering::Relaxed);
    assert_eq!(
        b_arrivals + c_arrivals,
        n,
        "total arrivals must equal published count (no duplicates, no drops)",
    );
    assert!(
        b_arrivals > 0,
        "worker B must receive at least one event (round-robin distribution)",
    );
    assert!(
        c_arrivals > 0,
        "worker C must receive at least one event (round-robin distribution)",
    );
}

/// Mixing `Broadcast` and `QueueGroup` on the same channel: a
/// broadcast subscriber receives every event AND queue-group
/// members divide events among themselves. Pin the
/// "audit logger + worker pool" pattern from the design doc.
#[tokio::test]
async fn broadcast_subscriber_coexists_with_queue_group_on_same_channel() {
    let publisher = build_node().await;
    let auditor = build_node().await;
    let worker_a = build_node().await;
    let worker_b = build_node().await;
    handshake_pair(&publisher, &auditor).await;
    handshake_pair(&publisher, &worker_a).await;
    handshake_pair(&publisher, &worker_b).await;

    let channel = ChannelName::new("test/qg/mixed").unwrap();
    let channel_hash = channel.hash();

    // Auditor: Broadcast subscriber.
    auditor
        .subscribe_channel(publisher.node_id(), channel.clone())
        .await
        .expect("auditor subscribe");
    // Workers: queue-group members.
    let group = "pool".to_string();
    worker_a
        .subscribe_channel_in_queue_group(publisher.node_id(), channel.clone(), group.clone())
        .await
        .expect("worker A subscribe");
    worker_b
        .subscribe_channel_in_queue_group(publisher.node_id(), channel.clone(), group.clone())
        .await
        .expect("worker B subscribe");

    // Inbound counters on each.
    let auditor_count = Arc::new(AtomicUsize::new(0));
    let wa_count = Arc::new(AtomicUsize::new(0));
    let wb_count = Arc::new(AtomicUsize::new(0));
    let auditor_for_disp = auditor_count.clone();
    let wa_for_disp = wa_count.clone();
    let wb_for_disp = wb_count.clone();
    auditor.register_rpc_inbound(
        channel_hash,
        Arc::new(move |_| {
            auditor_for_disp.fetch_add(1, Ordering::Relaxed);
        }),
    );
    worker_a.register_rpc_inbound(
        channel_hash,
        Arc::new(move |_| {
            wa_for_disp.fetch_add(1, Ordering::Relaxed);
        }),
    );
    worker_b.register_rpc_inbound(
        channel_hash,
        Arc::new(move |_| {
            wb_for_disp.fetch_add(1, Ordering::Relaxed);
        }),
    );

    let n: usize = 50;
    let pub_handle = ChannelPublisher::new(channel.clone(), PublishConfig::default());
    for i in 0..n {
        publisher
            .publish(&pub_handle, Bytes::from(format!("event-{i}")))
            .await
            .expect("publish");
    }

    // Wait for the auditor to receive every event AND the worker
    // total to equal n.
    assert!(
        wait_until(
            || auditor_count.load(Ordering::Relaxed) == n
                && wa_count.load(Ordering::Relaxed) + wb_count.load(Ordering::Relaxed) == n,
            Duration::from_secs(5),
        )
        .await,
        "auditor: {}, workers: {} + {}",
        auditor_count.load(Ordering::Relaxed),
        wa_count.load(Ordering::Relaxed),
        wb_count.load(Ordering::Relaxed),
    );

    assert_eq!(
        auditor_count.load(Ordering::Relaxed),
        n,
        "broadcast subscriber must receive every event",
    );
    let wa = wa_count.load(Ordering::Relaxed);
    let wb = wb_count.load(Ordering::Relaxed);
    assert_eq!(
        wa + wb,
        n,
        "queue-group total must equal published count",
    );
    assert!(wa > 0 && wb > 0, "both workers must get some events");
}
