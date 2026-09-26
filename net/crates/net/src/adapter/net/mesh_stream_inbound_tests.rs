// SPDX-License-Identifier: MIT OR Apache-2.0
//! Stream sinks (`register_stream_inbound`): events on a registered
//! stream arrive WITH the session-authenticated sender, and bypass the
//! shard queue whose `StoredEvent` has none. Real UDP sessions between
//! real nodes; nothing mocked.
use super::*;
use crate::adapter::Adapter;
use parking_lot::Mutex as SyncMutex;

async fn node() -> Arc<MeshNode> {
    Arc::new(
        MeshNode::new(
            EntityKeypair::generate(),
            MeshNodeConfig::new("127.0.0.1:0".parse().unwrap(), [0x42; 32])
                .with_heartbeat_interval(Duration::from_millis(200))
                .with_session_timeout(Duration::from_secs(5)),
        )
        .await
        .unwrap(),
    )
}

/// `client` connects to `server`.
async fn connect(client: &Arc<MeshNode>, server: &Arc<MeshNode>) {
    let accept = {
        let server = server.clone();
        let client_id = client.node_id();
        tokio::spawn(async move { server.accept(client_id).await.unwrap() })
    };
    client
        .connect(server.local_addr(), server.public_key(), server.node_id())
        .await
        .unwrap();
    accept.await.unwrap();
}

const LABEL: &str = "store/stream-inbound-test";

fn reliable() -> StreamConfig {
    let mut config = StreamConfig::new();
    config.reliability = super::super::stream::Reliability::Reliable;
    config
}

type Seen = Arc<SyncMutex<Vec<StreamInboundEvent>>>;

/// A sink that records what it receives.
fn collecting() -> (StreamInboundSink, Seen) {
    let seen: Seen = Arc::new(SyncMutex::new(Vec::new()));
    let sink: StreamInboundSink = {
        let seen = seen.clone();
        Arc::new(move |event| seen.lock().push(event))
    };
    (sink, seen)
}

async fn wait_for(seen: &Seen, count: usize) {
    for _ in 0..200 {
        if seen.lock().len() >= count {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("expected {count} events, saw {}", seen.lock().len());
}

/// Drain the shard queue `stream_id` maps to. `poll_shard` CONSUMES
/// what it returns, so a count is taken once, not re-read.
async fn drain(node: &MeshNode, stream_id: u64) -> usize {
    let shard = (stream_id % node.config.num_shards as u64) as u16;
    node.poll_shard(shard, None, 1000)
        .await
        .unwrap()
        .events
        .len()
}

/// Drain until `count` events have been seen or two seconds pass.
async fn drain_until(node: &MeshNode, stream_id: u64, count: usize) -> usize {
    let mut total = 0;
    for _ in 0..200 {
        total += drain(node, stream_id).await;
        if total >= count {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    total
}

#[tokio::test]
async fn a_registered_stream_is_delivered_with_its_authenticated_sender_and_not_queued() {
    let host = node().await;
    let alice = node().await;
    let bob = node().await;
    connect(&alice, &host).await;
    connect(&bob, &host).await;
    host.start();
    alice.start();
    bob.start();

    let stream_id = net_wire::channel::name::stream_id_from_label(LABEL);
    let (sink, seen) = collecting();
    host.register_stream_inbound(stream_id, sink)
        .expect("vacant stream");

    let from_alice = alice
        .open_stream(host.node_id(), stream_id, reliable())
        .unwrap();
    let from_bob = bob
        .open_stream(host.node_id(), stream_id, reliable())
        .unwrap();
    alice
        .send_on_stream(
            &from_alice,
            &[
                Bytes::from_static(b"alice-1"),
                Bytes::from_static(b"alice-2"),
            ],
        )
        .await
        .unwrap();
    bob.send_on_stream(&from_bob, &[Bytes::from_static(b"bob-1")])
        .await
        .unwrap();
    wait_for(&seen, 3).await;

    let events = seen.lock().clone();
    let by = |peer: u64| -> Vec<Vec<u8>> {
        events
            .iter()
            .filter(|event| event.from_node == peer)
            .map(|event| event.payload.to_vec())
            .collect()
    };
    // Each sender's events carry THAT sender — the identity two
    // players' frames must never share — in order, on the stream.
    assert_eq!(
        by(alice.node_id()),
        vec![b"alice-1".to_vec(), b"alice-2".to_vec()]
    );
    assert_eq!(by(bob.node_id()), vec![b"bob-1".to_vec()]);
    assert!(events.iter().all(|event| event.stream_id == stream_id));
    // Delivered to the sink INSTEAD of the queue, not as well as it.
    assert_eq!(drain(&host, stream_id).await, 0);
}

#[tokio::test]
async fn registration_is_vacant_only_a_stale_id_cannot_evict_and_unregistering_restores_the_queue()
{
    let host = node().await;
    let alice = node().await;
    connect(&alice, &host).await;
    host.start();
    alice.start();
    let stream_id = net_wire::channel::name::stream_id_from_label(LABEL);

    let (first, seen) = collecting();
    let id = host
        .register_stream_inbound(stream_id, first)
        .expect("vacant");
    let (second, _) = collecting();
    assert!(
        host.register_stream_inbound(stream_id, second).is_none(),
        "occupied"
    );
    assert!(
        !host.unregister_stream_inbound(stream_id, id + 1),
        "a stale id is a no-op"
    );

    let stream = alice
        .open_stream(host.node_id(), stream_id, reliable())
        .unwrap();
    alice
        .send_on_stream(&stream, &[Bytes::from_static(b"to-sink")])
        .await
        .unwrap();
    wait_for(&seen, 1).await;

    assert!(host.unregister_stream_inbound(stream_id, id));
    alice
        .send_on_stream(&stream, &[Bytes::from_static(b"to-queue")])
        .await
        .unwrap();
    assert_eq!(
        drain_until(&host, stream_id, 1).await,
        1,
        "back to the shard queue"
    );
    assert_eq!(
        seen.lock().len(),
        1,
        "the unregistered sink hears nothing more"
    );
}

#[tokio::test]
async fn an_unregistered_stream_still_lands_in_the_shard_queue() {
    let host = node().await;
    let alice = node().await;
    connect(&alice, &host).await;
    host.start();
    alice.start();
    let registered = net_wire::channel::name::stream_id_from_label(LABEL);
    let other = net_wire::channel::name::stream_id_from_label("some/other/stream");
    let (sink, seen) = collecting();
    host.register_stream_inbound(registered, sink).unwrap();

    let stream = alice
        .open_stream(host.node_id(), other, reliable())
        .unwrap();
    alice
        .send_on_stream(&stream, &[Bytes::from_static(b"unrelated")])
        .await
        .unwrap();
    assert_eq!(
        drain_until(&host, other, 1).await,
        1,
        "an unregistered stream is queued as before"
    );
    assert!(seen.lock().is_empty());
}
