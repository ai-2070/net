//! Scoped credentials, not an open channel.
//!
//! Two nodes. The publisher owns a channel whose subscriber ACL is rooted at
//! its own entity id; a subscriber holding a token minted for *its* entity id,
//! for *this* channel, with the subscribe scope is admitted. The same
//! subscriber asking without the token is refused.
//!
//! This is the shape a broker makes you build out of ACL files and a separate
//! auth service: there is one identity, one token, one place the decision is
//! made, and the credential is presented per subscribe rather than cached by a
//! connection.
//!
//! Run (from a crate whose `examples/` holds this file):
//!
//!   cargo run --example tokenchannel
//!
//! Expected final line: `RESULT ok granted=1 refused=1`

use std::net::SocketAddr;
use std::time::Duration;

use net_sdk::capabilities::{CapabilityFilter, CapabilitySet};
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::{
    ChannelConfig, ChannelId, ChannelName, Identity, SubscribeOptions, TokenScope, Visibility,
};

/// 32 bytes exactly — a PSK, not a passphrase. Every node in a mesh shares it.
const PSK: [u8; 32] = [0x42; 32];

/// The token's lifetime. Long enough for the example, short enough that a
/// minted credential is never a standing secret.
const TOKEN_TTL: Duration = Duration::from_secs(300);

async fn build(seed_byte: u8) -> (Mesh, Identity) {
    let identity = Identity::from_seed([seed_byte; 32]);
    let node = MeshBuilder::new("127.0.0.1:0", &PSK)
        .expect("builder")
        .identity(identity.clone())
        .build()
        .await
        .expect("build mesh node");
    (node, identity)
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

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let channel = ChannelName::new("config/gated").expect("channel name");

    let (publisher, publisher_identity) = build(0xE1).await;
    let (subscriber, subscriber_identity) = build(0xE2).await;

    let publisher_addr = publisher.inner().local_addr();
    handshake(&publisher, &subscriber, publisher_addr).await;
    publisher.inner().start();
    subscriber.inner().start();

    // Both nodes announce before anything is gated. A token's leaf binds to
    // the subscribing peer's EntityId, and the publisher only learns that
    // EntityId from a signature-verified announcement — so a subscriber that
    // has announced nothing is unauthorized no matter what it presents.
    publisher
        .announce_capabilities(CapabilitySet::new())
        .await?;
    subscriber
        .announce_capabilities(CapabilitySet::new())
        .await?;

    // Wait for the publisher to index it; the announcement is what populates
    // the publisher's peer-entity map.
    let subscriber_node_id = subscriber.inner().node_id();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        if publisher
            .find_nodes(&CapabilityFilter::new())
            .contains(&subscriber_node_id)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    // The channel's subscriber ACL is rooted at the publisher's own entity id.
    // `with_token_roots` is what turns `require_token` on, so this is one
    // declaration rather than a flag plus a trust anchor that can disagree.
    publisher.register_channel(
        ChannelConfig::new(ChannelId::new(channel.clone()))
            .with_visibility(Visibility::Global)
            .with_token_roots(vec![publisher_identity.entity_id().clone()]),
    );
    println!(
        "channel gated on a token rooted at 0x{:x}",
        publisher_identity.entity_id().origin_hash()
    );

    // A credential scoped three ways: to this subscriber's entity id, to this
    // channel, and to the subscribe action alone. It cannot publish, and it is
    // useless to any other node.
    let token = publisher_identity.issue_token(
        subscriber_identity.entity_id().clone(),
        TokenScope::SUBSCRIBE,
        &channel,
        TOKEN_TTL,
        0,
    );
    println!("issued a subscribe-only token to the subscriber");

    // Without it: refused. The publisher answers and says no, which is a
    // different outcome from "the publisher never answered".
    let refused = subscriber
        .subscribe_channel(publisher.inner().node_id(), &channel)
        .await
        .is_err();
    println!("bare subscribe refused:            {refused}");

    // With it: admitted. The credential is presented on the subscribe request
    // itself, not negotiated once per connection.
    let granted = subscriber
        .subscribe_channel_with(
            publisher.inner().node_id(),
            &channel,
            SubscribeOptions {
                token: Some(token),
            },
        )
        .await
        .is_ok();
    println!("token-carrying subscribe admitted: {granted}");

    println!(
        "RESULT ok granted={} refused={}",
        usize::from(granted),
        usize::from(refused)
    );

    publisher.shutdown().await?;
    subscriber.shutdown().await?;
    Ok(())
}
