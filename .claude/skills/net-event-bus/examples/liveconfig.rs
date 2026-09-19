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
    ChannelConfig, ChannelId, ChannelName, PublishConfig, PublishReport, Reliability, Visibility,
};
use net_sdk::Identity;

/// 32 bytes exactly — a PSK, not a passphrase. Every node in a mesh shares it.
const PSK: [u8; 32] = [0x42; 32];

/// How long we wait for a published revision to land in a subscriber's shards.
const DELIVER: Duration = Duration::from_secs(5);

/// Both subscribers are in the roster before the first publish, because
/// `subscribe_channel` blocks on the publisher's ack.
const SUBSCRIBERS: usize = 2;

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

/// A revision that did not reach every subscriber is not a config update. The
/// publisher's own report is the evidence, so read it instead of assuming the
/// fan-out worked.
fn check_delivery(version: u64, report: &PublishReport) -> Result<(), String> {
    if report.attempted != SUBSCRIBERS {
        return Err(format!(
            "v{version}: roster held {} subscribers, expected {SUBSCRIBERS}",
            report.attempted
        ));
    }
    if !report.all_delivered() {
        return Err(format!(
            "v{version}: delivered to {} of {} subscribers, errors: {:?}",
            report.delivered, report.attempted, report.errors
        ));
    }
    Ok(())
}

/// Drain every shard the bus could have routed a channel event to. Published
/// events land on the shard derived from the stream id, so a consumer polls all
/// of them.
///
/// A quiet sweep is not the end of the stream: two revisions published
/// back-to-back can land one poll apart, so the only reason to stop early is
/// holding every revision in `expect`.
async fn drain(
    node: &Mesh,
    applied: &mut BTreeMap<u64, String>,
    expect: &[u64],
) -> Result<usize, Box<dyn std::error::Error>> {
    let deadline = Instant::now() + DELIVER;
    let mut seen = 0;
    loop {
        for shard in 0..4u16 {
            // A failed receive is not an empty shard. Swallowing it would make
            // "nothing was delivered" and "we never looked" the same answer.
            for event in node.recv_shard(shard, 64).await? {
                seen += 1;
                if let Some((version, mode)) = parse(&event.raw) {
                    applied.insert(version, mode);
                }
            }
        }
        if expect.iter().all(|version| applied.contains_key(version)) || Instant::now() >= deadline
        {
            return Ok(seen);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
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

    publisher.start();
    s1.start();
    s2.start();

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
    check_delivery(1, &report)?;

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
    check_delivery(2, &report)?;

    // Both revisions are expected on both subscribers, so drain until each has
    // them rather than until a poll comes back quiet.
    let mut one = BTreeMap::new();
    let mut two = BTreeMap::new();
    drain(&s1, &mut one, &[1, 2]).await?;
    drain(&s2, &mut two, &[1, 2]).await?;

    println!("subscriber one applied:    {one:?}");
    println!("subscriber two applied:    {two:?}");

    // The final line claims both subscribers applied both revisions, with the
    // modes the publisher sent. Check that before claiming it: a revision that
    // never arrived is a failure, not a quieter success.
    for (name, applied) in [("one", &one), ("two", &two)] {
        for (version, mode) in [(1u64, "blue"), (2u64, "green")] {
            if applied.get(&version).map(String::as_str) != Some(mode) {
                return Err(format!(
                    "subscriber {name} never applied v{version}={mode}: {applied:?}"
                )
                .into());
            }
        }
    }

    let applied = [&one, &two]
        .iter()
        .filter(|applied| applied.contains_key(&2))
        .count();

    // Worth pinning: the publisher's roster is what fan-out costs. Zero
    // subscribers is a no-op, not a queue that later has to be drained.
    println!("roster at publish time:    {}", report.attempted);

    println!("RESULT ok subscribers={} applied={applied} version=2", report.attempted);

    publisher.shutdown().await?;
    s1.shutdown().await?;
    s2.shutdown().await?;
    Ok(())
}
