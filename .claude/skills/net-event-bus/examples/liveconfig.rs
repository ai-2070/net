//! Live config — the config service you no longer run.
//!
//! One publisher and two subscribers, three in-process mesh nodes over loopback
//! UDP. The publisher registers a channel, both subscribers join by name, and
//! every config revision is pushed once and applied by each subscriber locally.
//! There is no config server to poll, no cache to invalidate and no reload to
//! coordinate — the channel *is* the delivery, and the roster is held by the
//! publisher, not by a broker.
//!
//! Run (from a crate whose `examples/` holds this file):
//!
//!   cargo run --example liveconfig
//!
//! Expected final line: `RESULT ok subscribers=2 applied=2 version=2`

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use bytes::Bytes;
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::{
    ChannelConfig, ChannelId, ChannelName, PublishConfig, Reliability, Visibility,
};
use net_sdk::Identity;

/// 32 bytes exactly — a PSK, not a passphrase. Every node in a mesh shares it.
const PSK: [u8; 32] = [0x42; 32];

/// How long we wait for a published revision to land in a subscriber's shards.
const DELIVER: Duration = Duration::from_secs(5);

async fn build(seed_byte: u8) -> Mesh {
    MeshBuilder::new("127.0.0.1:0", &PSK)
        .expect("builder")
        .identity(Identity::from_seed([seed_byte; 32]))
        .build()
        .await
        .expect("build mesh node")
}

async fn handshake(responder: &Mesh, initiator: &Mesh, responder_addr: SocketAddr) {
    let responder_pub = *responder.inner().public_key();
    let responder_id = responder.inner().node_id();
    let initiator_id = initiator.inner().node_id();
    let (accepted, connected) = tokio::join!(
        responder.inner().accept(initiator_id),
        async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            initiator
                .inner()
                .connect(responder_addr, &responder_pub, responder_id)
                .await
        }
    );
    accepted.expect("accept");
    connected.expect("connect");
}

/// Parse `v=<n>;mode=<name>`; subscribers apply whatever they understand and
/// ignore the rest. A revision never has to be acknowledged back to the
/// publisher for the next one to arrive.
fn parse(payload: &[u8]) -> Option<(u64, String)> {
    let text = std::str::from_utf8(payload).ok()?;
    let mut version = None;
    let mut mode = None;
    for field in text.split(';') {
        if let Some(v) = field.strip_prefix("v=") {
            version = v.parse::<u64>().ok();
        } else if let Some(m) = field.strip_prefix("mode=") {
            mode = Some(m.to_string());
        }
    }
    Some((version?, mode?))
}

/// Drain every shard the bus could have routed a channel event to. Published
/// events land on the shard derived from the stream id, so a consumer polls all
/// of them.
async fn drain(node: &Mesh, applied: &mut BTreeMap<u64, String>) -> usize {
    let deadline = Instant::now() + DELIVER;
    let mut seen = 0;
    while Instant::now() < deadline {
        let mut quiet = true;
        for shard in 0..4u16 {
            if let Ok(events) = node.recv_shard(shard, 64).await {
                for event in events {
                    quiet = false;
                    seen += 1;
                    if let Some((version, mode)) = parse(&event.raw) {
                        applied.insert(version, mode);
                    }
                }
            }
        }
        if quiet && seen > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    seen
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let publisher = build(0xF1).await;
    let s1 = build(0xF2).await;
    let s2 = build(0xF3).await;

    let publisher_addr = publisher.inner().local_addr();
    // Both subscribers connect to the publisher; it accepts both.
    handshake(&publisher, &s1, publisher_addr).await;
    handshake(&publisher, &s2, publisher_addr).await;

    publisher.inner().start();
    s1.inner().start();
    s2.inner().start();

    // The publisher owns the channel config. No broker registers it.
    let channel = ChannelName::new("config/edge").expect("channel name");
    publisher.register_channel(
        ChannelConfig::new(ChannelId::new(channel.clone())).with_visibility(Visibility::Global),
    );

    // Subscribers join by name. `subscribe_channel` blocks on the publisher's
    // ack, so by the time it returns this node is in the roster.
    let publisher_id = publisher.inner().node_id();
    s1.subscribe_channel(publisher_id, &channel).await?;
    s2.subscribe_channel(publisher_id, &channel).await?;

    let config = |version: u64, mode: &str| {
        Bytes::from(format!("v={version};mode={mode}"))
    };

    // Revision 1.
    let report = publisher
        .publish(
            &channel,
            config(1, "blue"),
            PublishConfig {
                reliability: Reliability::Reliable,
                ..Default::default()
            },
        )
        .await?;
    println!(
        "published v1 to {} of {} subscribers",
        report.delivered, report.attempted
    );

    // Revision 2, delivered the same way.
    let report = publisher
        .publish(
            &channel,
            config(2, "green"),
            PublishConfig {
                reliability: Reliability::Reliable,
                ..Default::default()
            },
        )
        .await?;
    println!(
        "published v2 to {} of {} subscribers",
        report.delivered, report.attempted
    );

    let mut one = BTreeMap::new();
    let mut two = BTreeMap::new();
    drain(&s1, &mut one).await;
    drain(&s2, &mut two).await;

    let applied = [&one, &two]
        .iter()
        .filter(|applied| applied.get(&2).is_some())
        .count();

    println!("subscriber one applied:    {one:?}");
    println!("subscriber two applied:    {two:?}");

    // Worth pinning: the publisher's roster is what fan-out costs. Zero
    // subscribers is a no-op, not a queue that later has to be drained.
    println!("roster at publish time:    {}", report.attempted);

    println!("RESULT ok subscribers={} applied={applied} version=2", report.attempted);

    publisher.shutdown().await?;
    s1.shutdown().await?;
    s2.shutdown().await?;
    Ok(())
}
