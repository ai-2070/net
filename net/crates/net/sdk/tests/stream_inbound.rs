// SPDX-License-Identifier: MIT OR Apache-2.0
//! Per-stream inbound receive on the SDK `Mesh` (`RUST_SDK_GAPS_PLAN.md` R1):
//! `open_stream_inbox` / `on_stream_data` deliver each event WITH its
//! authenticated sender, instead of the shard queue, whose `StoredEvent`
//! has none.
//!
//! Two witnesses pin the ownership contract the SDK handle must keep
//! from core, not just the happy path:
//!
//! - a stale handle's teardown can never evict a later receiver on the
//!   same stream id (unregister is by stream id AND registration id);
//! - neither handle holds a strong reference to the node.
//!
//! Real UDP sessions between real nodes; nothing mocked.

#![cfg(feature = "net")]

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::{MeshStream as Stream, Reliability, StreamConfig, StreamInboundEvent};
use parking_lot::Mutex;

const PSK: [u8; 32] = [0x42; 32];
const SID: u64 = 0x5EED_0001;

async fn mesh() -> Mesh {
    MeshBuilder::new("127.0.0.1:0", &PSK)
        .unwrap()
        .build()
        .await
        .unwrap()
}

/// `host` and `alice`, connected (alice initiates) and started.
async fn connected_pair() -> (Mesh, Mesh) {
    let host = mesh().await;
    let alice = mesh().await;
    let host_addr = host.local_addr();
    let host_pub = *host.public_key();
    let host_id = host.node_id();
    let alice_id = alice.node_id();
    let (accepted, connected) = tokio::join!(host.inner().accept(alice_id), async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        alice.inner().connect(host_addr, &host_pub, host_id).await
    });
    accepted.expect("accept");
    connected.expect("connect");
    host.start();
    alice.start();
    (host, alice)
}

fn reliable() -> StreamConfig {
    StreamConfig::new().with_reliability(Reliability::Reliable)
}

/// Alice's stream to the host on `SID`.
fn stream_to_host(alice: &Mesh, host: &Mesh) -> Stream {
    alice
        .open_stream(host.node_id(), SID, reliable())
        .expect("open_stream")
}

async fn send(alice: &Mesh, stream: &Stream, payload: &'static [u8]) {
    alice
        .send_on_stream(stream, &[Bytes::from_static(payload)])
        .await
        .expect("send_on_stream");
}

/// Poll `cond` every 10 ms for up to two seconds.
async fn eventually(what: &str, mut cond: impl FnMut() -> bool) {
    for _ in 0..200 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for: {what}");
}

/// Drain the shard queue `SID` maps to until `count` events were seen
/// or two seconds pass. `recv_shard` consumes what it returns.
async fn drain_queue_until(host: &Mesh, count: usize) -> usize {
    let shard = host.shard_for_stream(SID);
    let mut total = 0;
    for _ in 0..200 {
        total += host.recv_shard(shard, 1000).await.unwrap().len();
        if total >= count {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    total
}

type Seen = Arc<Mutex<Vec<StreamInboundEvent>>>;

/// A handler that records what it receives.
fn collecting() -> (impl Fn(StreamInboundEvent) + Send + Sync + 'static, Seen) {
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let handler = {
        let seen = seen.clone();
        move |event| seen.lock().push(event)
    };
    (handler, seen)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inbox_delivers_with_the_authenticated_sender_and_bypasses_the_queue() {
    let (host, alice) = connected_pair().await;
    let inbox = host.open_stream_inbox(SID, 16).expect("vacant stream");
    assert_eq!(inbox.stream_id(), SID);

    let stream = stream_to_host(&alice, &host);
    send(&alice, &stream, b"one").await;
    send(&alice, &stream, b"two").await;

    let mut got = Vec::new();
    eventually("two inbox events", || {
        while let Some(event) = inbox.try_recv() {
            got.push(event);
        }
        got.len() >= 2
    })
    .await;

    assert!(got.iter().all(|e| e.from_node == alice.node_id()));
    assert!(got.iter().all(|e| e.stream_id == SID));
    let payloads: Vec<&[u8]> = got.iter().map(|e| e.payload.as_ref()).collect();
    assert_eq!(payloads, vec![&b"one"[..], &b"two"[..]]);
    // Delivered to the inbox INSTEAD of the queue, not as well as it.
    assert_eq!(drain_queue_until(&host, 1).await, 0);
    assert_eq!(inbox.dropped(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_receiver_per_stream_whichever_kind_holds_it() {
    let host = mesh().await;

    let inbox = host.open_stream_inbox(SID, 4).expect("vacant");
    assert!(host.open_stream_inbox(SID, 4).is_none(), "inbox then inbox");
    assert!(
        host.on_stream_data(SID, |_| {}).is_none(),
        "inbox then callback"
    );
    drop(inbox);

    let sub = host.on_stream_data(SID, |_| {}).expect("vacant again");
    assert_eq!(sub.stream_id(), SID);
    assert!(
        host.on_stream_data(SID, |_| {}).is_none(),
        "callback then callback"
    );
    assert!(
        host.open_stream_inbox(SID, 4).is_none(),
        "callback then inbox"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn callback_delivers_and_dropping_it_returns_events_to_the_queue() {
    let (host, alice) = connected_pair().await;
    let (handler, seen) = collecting();
    let sub = host.on_stream_data(SID, handler).expect("vacant stream");

    let stream = stream_to_host(&alice, &host);
    send(&alice, &stream, b"to-callback").await;
    eventually("one callback event", || seen.lock().len() == 1).await;
    {
        let seen = seen.lock();
        assert_eq!(seen[0].from_node, alice.node_id());
        assert_eq!(seen[0].payload.as_ref(), b"to-callback");
    }

    drop(sub);
    send(&alice, &stream, b"to-queue").await;
    assert_eq!(
        drain_queue_until(&host, 1).await,
        1,
        "back to the shard queue"
    );
    assert_eq!(
        seen.lock().len(),
        1,
        "the dropped handler hears nothing more"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_inbox_past_capacity_drops_and_counts() {
    let (host, alice) = connected_pair().await;
    let inbox = host.open_stream_inbox(SID, 1).expect("vacant stream");

    let stream = stream_to_host(&alice, &host);
    for payload in [&b"a"[..], b"b", b"c", b"d"] {
        alice
            .send_on_stream(&stream, &[Bytes::copy_from_slice(payload)])
            .await
            .expect("send_on_stream");
    }

    // One waits in the queue; the other three were dropped, not queued.
    eventually("three drops", || inbox.dropped() == 3).await;
    let first = inbox.try_recv().expect("the one that fit");
    assert_eq!(first.payload.as_ref(), b"a");
    assert!(inbox.try_recv().is_none());
}

/// The ownership witness: A is closed, B takes the stream (with a new
/// registration id), and A's handle — closed again, then dropped — must
/// not evict B. Fails if teardown ever removes by stream id alone, or by
/// a registration id it does not own.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stale_handle_cannot_evict_its_successor() {
    let (host, alice) = connected_pair().await;
    let stream = stream_to_host(&alice, &host);

    // Callback A, then callback B.
    let (stale_handler, stale_seen) = collecting();
    let stale = host.on_stream_data(SID, stale_handler).expect("vacant");
    assert!(stale.close(), "the first close removes A's registration");
    let (handler, seen) = collecting();
    let successor = host.on_stream_data(SID, handler).expect("vacant after A");
    assert!(!stale.close(), "a repeat close is a no-op");
    assert!(stale.is_closed());
    drop(stale);

    send(&alice, &stream, b"for-b").await;
    eventually("B still receives", || seen.lock().len() == 1).await;
    assert!(stale_seen.lock().is_empty());
    assert_eq!(drain_queue_until(&host, 1).await, 0, "B held the stream");
    drop(successor);

    // Inbox A, then callback B.
    let stale_inbox = host.open_stream_inbox(SID, 4).expect("vacant");
    assert!(stale_inbox.close());
    let (handler, seen) = collecting();
    let _successor = host.on_stream_data(SID, handler).expect("vacant after A");
    assert!(!stale_inbox.close());
    drop(stale_inbox);

    send(&alice, &stream, b"for-b-again").await;
    eventually("B still receives after a stale inbox drop", || {
        seen.lock().len() == 1
    })
    .await;
    assert_eq!(drain_queue_until(&host, 1).await, 0, "B held the stream");
}

/// Neither handle holds the node strongly. Measured on a node that was
/// never started, so no background task is cloning the `Arc` while the
/// count is read.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handles_hold_the_node_weakly() {
    let host = mesh().await;
    let baseline = Arc::strong_count(host.node());

    let inbox = host.open_stream_inbox(SID, 4).expect("vacant");
    assert_eq!(Arc::strong_count(host.node()), baseline, "inbox is weak");
    drop(inbox);
    let sub = host.on_stream_data(SID, |_| {}).expect("vacant");
    assert_eq!(
        Arc::strong_count(host.node()),
        baseline,
        "subscription is weak"
    );
    let inbox = host.open_stream_inbox(SID + 1, 4).expect("vacant");

    // Shutdown with both handles still alive, then drop them: teardown
    // after the node is gone must be safe.
    host.shutdown()
        .await
        .expect("shutdown with handles outstanding");
    drop(sub);
    drop(inbox);
}

/// A panicking handler must not take the receive path down with it. The
/// core calls the sink inline with no unwind guard, so without the SDK's
/// `catch_unwind` the first panic would end the node's receive task and
/// the second event would never arrive. (Tests build with unwinding; under
/// `panic = "abort"` nothing can contain a panic.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_panicking_handler_is_contained_and_the_node_keeps_receiving() {
    let (host, alice) = connected_pair().await;
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let sub = {
        let seen = seen.clone();
        host.on_stream_data(SID, move |event| {
            if event.payload.as_ref() == b"boom" {
                panic!("handler blew up");
            }
            seen.lock().push(event);
        })
        .expect("vacant stream")
    };

    let stream = stream_to_host(&alice, &host);
    send(&alice, &stream, b"boom").await;
    send(&alice, &stream, b"after").await;
    eventually("the event after the panic", || seen.lock().len() == 1).await;
    assert_eq!(seen.lock()[0].payload.as_ref(), b"after");
    assert_eq!(sub.panics(), 1);
}
