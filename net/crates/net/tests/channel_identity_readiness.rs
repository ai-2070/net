//! Token admission needs an authenticated identity — not a discovery
//! entry.
//!
//! A channel credential's leaf names an [`EntityId`]. To evaluate it the
//! publisher must know, with proof, which entity is on the wire. Before
//! the identity-proof subprotocol the only way a plain consumer could
//! supply that was to **advertise capabilities** and wait for the
//! publisher to index the announcement — a discovery-plane round trip
//! that has nothing to do with the credential, imposed on peers with no
//! services to publish. The examples encoded it as an announce-and-poll
//! warm-up, and a subscriber that skipped it was told `Unauthorized`,
//! which points at the credential rather than at the missing
//! prerequisite.
//!
//! What these tests hold:
//!
//! - **A fresh subscriber needs nothing but its credential.** No
//!   `announce_capabilities`, no discovery poll: subscribe is accepted
//!   and events actually arrive.
//! - **The credential is still not a proof of identity.** A different
//!   peer presenting somebody else's token is refused — and refused as
//!   `Unauthorized`, with its own identity fully established, so the
//!   rejection is a verdict on the credential and not a readiness
//!   artifact.
//! - **The two failures are distinguishable.** A peer that cannot prove
//!   its identity while holding a perfectly valid credential is told
//!   `IdentityNotEstablished`.
//! - **A reconnect does not inherit the binding.** A new session
//!   incarnation starts from "identity not established", and the
//!   runtime re-authenticates before the credential works again.
//!
//! Sessions are established with a single-hop `connect_via` because the
//! routed handshake is the only inbound handshake path the dispatcher
//! serves post-`start()` — which is what lets the same pair re-handshake
//! for the reconnect case.

#![cfg(feature = "net")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use net::adapter::net::identity::TokenChain;
use net::adapter::net::{
    ChannelConfig, ChannelConfigRegistry, ChannelId, ChannelName, ChannelPublisher, EntityKeypair,
    MeshNode, MeshNodeConfig, OnFailure, PermissionToken, PublishConfig, Reliability,
    SocketBufferConfig, TokenCache, TokenScope,
};
use net::adapter::Adapter;

const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x42u8; 32];
const TOKEN_TTL_SECS: u64 = 300;
const PAYLOAD: &[u8] = b"gated-payload";

fn test_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(500))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(3, Duration::from_secs(3));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: TEST_BUFFER_SIZE,
        recv_buffer_size: TEST_BUFFER_SIZE,
    };
    cfg
}

struct Node {
    mesh: Arc<MeshNode>,
    keypair: EntityKeypair,
    registry: Arc<ChannelConfigRegistry>,
}

async fn build_node_with_identity(keypair: EntityKeypair) -> Node {
    let mut node = MeshNode::new(keypair.clone(), test_config())
        .await
        .expect("MeshNode::new");
    let registry = Arc::new(ChannelConfigRegistry::new());
    node.set_channel_configs(registry.clone());
    node.set_token_cache(Arc::new(TokenCache::new()));
    Node {
        mesh: Arc::new(node),
        keypair,
        registry,
    }
}

async fn build_node() -> Node {
    build_node_with_identity(EntityKeypair::generate()).await
}

/// Give `client` a session with `server`, post-`start()`, without either
/// side announcing anything.
async fn routed_session(client: &Arc<MeshNode>, server: &Arc<MeshNode>) {
    let server_pub = *server.public_key();
    client
        .connect_via(server.local_addr(), &server_pub, server.node_id())
        .await
        .expect("routed handshake must complete");
}

/// Drain every shard until `payload` shows up, or the bounded wait
/// expires. The mesh inbound queues are consume-once, so a poll that
/// finds something unrelated must keep the search going rather than
/// re-reading the same cursor.
async fn await_payload(node: &Arc<MeshNode>, payload: &[u8]) -> bool {
    let shards = node.num_shards().max(1);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        for shard in 0..shards {
            let polled = node
                .poll_shard(shard, None, 32)
                .await
                .expect("polling a shard must not fail");
            if polled
                .events
                .iter()
                .any(|event| event.raw.as_ref() == payload)
            {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    false
}

/// A token-gated channel rooted at `publisher`'s own entity, plus the
/// publisher's own PUBLISH grant so it can clear its own gate.
fn gate_channel(publisher: &Node, channel: &ChannelName) {
    publisher.registry.insert(
        ChannelConfig::new(ChannelId::new(channel.clone()))
            .with_token_roots(vec![publisher.keypair.entity_id().clone()]),
    );
    let self_token = PermissionToken::issue(
        &publisher.keypair,
        publisher.keypair.entity_id().clone(),
        TokenScope::PUBLISH,
        channel.hash(),
        TOKEN_TTL_SECS,
        0,
    );
    publisher
        .mesh
        .set_publish_chain(channel, TokenChain::single(self_token));
}

/// The acceptance case: a consumer that has advertised nothing and
/// queried nothing presents the credential it was issued, and receives
/// data.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fresh_subscriber_with_no_advertised_capabilities_subscribes_and_receives_data() {
    let publisher = build_node().await;
    let subscriber = build_node().await;
    publisher.mesh.start();
    subscriber.mesh.start();
    routed_session(&subscriber.mesh, &publisher.mesh).await;

    let channel = ChannelName::new("lab/gated").expect("channel name");
    gate_channel(&publisher, &channel);
    let token = PermissionToken::issue(
        &publisher.keypair,
        subscriber.keypair.entity_id().clone(),
        TokenScope::SUBSCRIBE,
        channel.hash(),
        TOKEN_TTL_SECS,
        0,
    );

    // Positive control on the premise: nothing has established this
    // peer's identity yet. If this ever starts out true the test proves
    // nothing about the prerequisite.
    let subscriber_id = subscriber.mesh.node_id();
    assert!(
        !publisher.mesh.peer_identity_established(subscriber_id),
        "premise: the publisher must not already hold an authenticated \
         identity for a peer that has announced nothing"
    );

    subscriber
        .mesh
        .subscribe_channel_with_token(publisher.mesh.node_id(), channel.clone(), token)
        .await
        .expect(
            "a credential issued to this subscriber must be usable without \
             advertising capabilities or polling a discovery index",
        );

    assert!(
        publisher.mesh.peer_identity_established(subscriber_id),
        "the subscribe must have established the identity it needed, on this \
         session incarnation"
    );

    // Admission is not the deliverable, and neither is a send count:
    // `PublishReport::delivered` counts per-peer sends that succeeded,
    // which on a FireAndForget channel says nothing about receipt. The
    // observable is the payload arriving at the subscriber.
    let publisher_handle = ChannelPublisher::new(
        channel.clone(),
        PublishConfig {
            reliability: Reliability::FireAndForget,
            on_failure: OnFailure::BestEffort,
            max_inflight: 16,
        },
    );
    let report = publisher
        .mesh
        .publish(&publisher_handle, Bytes::from_static(PAYLOAD))
        .await
        .expect("publish must be authorized");
    assert_eq!(
        report.delivered, 1,
        "premise: the publisher had exactly this one subscriber to send to"
    );
    assert!(
        await_payload(&subscriber.mesh, PAYLOAD).await,
        "the admitted subscriber must actually RECEIVE the event, not merely \
         be sent one"
    );
}

/// The credential is not a proof of identity: the issuer's signature
/// says who RECEIVED the grant, never who is holding the bytes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_different_peer_presenting_that_credential_is_refused() {
    let publisher = build_node().await;
    let grantee = build_node().await;
    let impostor = build_node().await;
    publisher.mesh.start();
    grantee.mesh.start();
    impostor.mesh.start();
    routed_session(&impostor.mesh, &publisher.mesh).await;

    let channel = ChannelName::new("lab/gated").expect("channel name");
    gate_channel(&publisher, &channel);

    // Issued to the grantee, stolen by the impostor.
    let stolen = PermissionToken::issue(
        &publisher.keypair,
        grantee.keypair.entity_id().clone(),
        TokenScope::SUBSCRIBE,
        channel.hash(),
        TOKEN_TTL_SECS,
        0,
    );
    let err = impostor
        .mesh
        .subscribe_channel_with_token(publisher.mesh.node_id(), channel, stolen)
        .await
        .expect_err("a credential issued to another entity must not admit this peer");

    let impostor_id = impostor.mesh.node_id();
    assert!(
        publisher.mesh.peer_identity_established(impostor_id),
        "the impostor's OWN identity is established — which is what makes the \
         rejection below a verdict on the credential rather than on readiness"
    );
    let text = err.to_string();
    assert!(
        text.contains("Unauthorized"),
        "presenting another entity's credential must be refused on the \
         credential axis; got: {text}"
    );
    assert!(
        !text.contains("IdentityNotEstablished"),
        "the impostor proved its identity, so readiness must not be blamed; \
         got: {text}"
    );
}

/// The two failures an operator confuses must be distinguishable on
/// the wire: a valid credential that cannot be bound to an
/// authenticated identity is a missing prerequisite, not a bad grant.
///
/// The subscriber here holds a **public-only** keypair — the same shape
/// a daemon reaches after migrating without its private material. It
/// owns an entity, a credential is issued to that entity, and it simply
/// cannot sign the proof. That makes "identity cannot be established"
/// a property of the node rather than a race against a timer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_peer_that_cannot_prove_its_identity_is_told_exactly_that() {
    let publisher = build_node().await;
    let holder = EntityKeypair::generate();
    let subscriber =
        build_node_with_identity(EntityKeypair::public_only(holder.entity_id().clone())).await;
    publisher.mesh.start();
    subscriber.mesh.start();
    routed_session(&subscriber.mesh, &publisher.mesh).await;

    let channel = ChannelName::new("lab/gated").expect("channel name");
    gate_channel(&publisher, &channel);
    // A genuinely valid credential: right issuer, right subject, right
    // channel, right scope, unexpired.
    let token = PermissionToken::issue(
        &publisher.keypair,
        holder.entity_id().clone(),
        TokenScope::SUBSCRIBE,
        channel.hash(),
        TOKEN_TTL_SECS,
        0,
    );

    let err = subscriber
        .mesh
        .subscribe_channel_with_token(publisher.mesh.node_id(), channel, token)
        .await
        .expect_err("an unbindable credential must not be admitted");
    let text = err.to_string();
    assert!(
        text.contains("IdentityNotEstablished"),
        "a valid credential the publisher cannot bind to an authenticated \
         identity must report the missing prerequisite, not a credential \
         rejection; got: {text}"
    );
    assert!(
        !publisher
            .mesh
            .peer_identity_established(subscriber.mesh.node_id()),
        "premise: identity really was not established"
    );
}
