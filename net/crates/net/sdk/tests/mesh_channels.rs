//! Rust SDK smoke test for the channel (distributed pub/sub) surface.
//!
//! Exercises the end-to-end flow: publisher registers a channel,
//! subscriber calls `subscribe_channel` over the wire, publisher's
//! fan-out delivers a payload. Covers the ACL reject path too.

#![cfg(feature = "net")]

use std::time::Duration;

use bytes::Bytes;
use net_sdk::error::SdkError;
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::{
    AckReason, ChannelConfig, ChannelId, ChannelName, PublishConfig, Reliability, Visibility,
};

async fn two_meshes(psk: &[u8; 32]) -> (Mesh, Mesh, std::net::SocketAddr) {
    let a = MeshBuilder::new("127.0.0.1:0", psk)
        .unwrap()
        .build()
        .await
        .unwrap();
    let b = MeshBuilder::new("127.0.0.1:0", psk)
        .unwrap()
        .build()
        .await
        .unwrap();
    let addr_b = b.inner().local_addr();
    (a, b, addr_b)
}

async fn handshake(a: &Mesh, b: &Mesh, addr_b: std::net::SocketAddr) {
    let pub_b = *b.inner().public_key();
    let nid_b = b.inner().node_id();
    let nid_a = a.inner().node_id();
    let (r1, r2) = tokio::join!(b.inner().accept(nid_a), async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        a.inner().connect(addr_b, &pub_b, nid_b).await
    });
    r1.expect("accept");
    r2.expect("connect");
    a.start();
    b.start();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_subscribe_and_publish_end_to_end() {
    let psk = [0x42u8; 32];
    let (a, b, addr_b) = two_meshes(&psk).await;
    handshake(&a, &b, addr_b).await;

    // B is the publisher; register a permissive channel config.
    let channel = ChannelName::new("sensors/temp").unwrap();
    b.register_channel(
        ChannelConfig::new(ChannelId::new(channel.clone())).with_visibility(Visibility::Global),
    );

    // A subscribes to B's channel.
    let nid_b = b.inner().node_id();
    a.subscribe_channel(nid_b, &channel)
        .await
        .expect("subscribe should succeed under permissive config");

    // Give the publisher a tick to register A in its roster.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // B publishes one payload; A should receive it via the normal
    // event-bus drain on B... wait, publisher -> subscribers, so A
    // receives. Poll A's inbound shard.
    let report = b
        .publish(
            &channel,
            Bytes::from_static(b"hello"),
            PublishConfig {
                reliability: Reliability::Reliable,
                ..Default::default()
            },
        )
        .await
        .expect("publish");
    assert_eq!(report.attempted, 1, "exactly one subscriber on roster");
    assert_eq!(report.delivered, 1, "publish must reach the one subscriber");
    assert!(report.errors.is_empty(), "no per-peer errors expected");

    // Drain A's inbound — published events land on the shard derived
    // from their stream_id, not necessarily shard 0, so poll every
    // shard the mesh was built with.
    let mut received = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_millis(2000);
    while std::time::Instant::now() < deadline && received.is_empty() {
        for shard in 0..4u16 {
            let events = a.recv_shard(shard, 16).await.expect("recv_shard");
            for e in events {
                received.push(e.raw.to_vec());
            }
        }
        if received.is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    assert!(
        !received.is_empty(),
        "subscriber must observe the published payload on the event bus"
    );
    assert!(
        received.iter().any(|bytes| bytes.as_slice() == b"hello"),
        "expected payload 'hello' in received events, got {:?}",
        received
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_subscribe_unknown_channel_returns_rejected() {
    // Publisher registers `foo` but subscriber asks for `bar` —
    // publisher's ACL path returns `UnknownChannel` and the SDK
    // surfaces it as `SdkError::ChannelRejected(UnknownChannel)`.
    //
    // A permissive "no configs installed" path accepts everything,
    // so we MUST register at least one channel on the publisher to
    // enable ACL enforcement.
    let psk = [0x43u8; 32];
    let (a, b, addr_b) = two_meshes(&psk).await;
    handshake(&a, &b, addr_b).await;

    let registered = ChannelName::new("foo").unwrap();
    b.register_channel(ChannelConfig::new(ChannelId::new(registered)));

    let nid_b = b.inner().node_id();
    let requested = ChannelName::new("bar").unwrap();
    let err = a
        .subscribe_channel(nid_b, &requested)
        .await
        .expect_err("unknown channel must be rejected");

    match err {
        SdkError::ChannelRejected(Some(AckReason::UnknownChannel)) => {}
        other => panic!("expected ChannelRejected(UnknownChannel), got {:?}", other),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_unsubscribe_is_idempotent() {
    // Unsubscribing a non-member is accepted on the publisher side —
    // the SDK should return Ok(()).
    let psk = [0x44u8; 32];
    let (a, b, addr_b) = two_meshes(&psk).await;
    handshake(&a, &b, addr_b).await;

    let channel = ChannelName::new("chan/x").unwrap();
    b.register_channel(ChannelConfig::new(ChannelId::new(channel.clone())));

    let nid_b = b.inner().node_id();
    a.unsubscribe_channel(nid_b, &channel)
        .await
        .expect("unsubscribe of non-member should succeed");
}
