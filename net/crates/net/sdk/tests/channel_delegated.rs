// SPDX-License-Identifier: MIT OR Apache-2.0
//! V3-2A C1: delegated channel credentials. An offline root grants an
//! issuing node DELEGATE on one channel; the node mints a device a full chain
//! root → node → device. The publisher (trusting only the root) admits that
//! full chain and refuses its leaf alone or someone else's chain; the device
//! publishes locally only while it holds its managed chain, and exact
//! removal cannot take out a successor.
#![cfg(all(feature = "net", feature = "cortex"))]

use std::time::Duration;

use bytes::Bytes;
use net::adapter::net::channel::{ChannelId, ChannelName};
use net::adapter::net::identity::{EntityKeypair, PermissionToken, TokenChain, TokenScope};
use net_sdk::channel_issuer::{ChannelIssueError, ChannelLeafIssuer};
use net_sdk::identity::Identity;
use net_sdk::mesh::{Mesh, MeshBuilder, SubscribeOptions};
use net_sdk::ChannelConfig;

const PSK: [u8; 32] = [0x6Bu8; 32];
const CHANNEL: &str = "fleet.telemetry";
const DAY: u64 = 24 * 60 * 60;

fn channel() -> ChannelName {
    ChannelName::new(CHANNEL).unwrap()
}

fn gated(root: &EntityKeypair) -> ChannelConfig {
    ChannelConfig::new(ChannelId::new(channel())).with_token_roots(vec![root.entity_id().clone()])
}

fn keypair(identity: &Identity) -> EntityKeypair {
    (**identity.keypair()).clone()
}

/// The root's DELEGATE grant to `issuer` on the channel.
fn grant(root: &EntityKeypair, issuer: &Identity, scope: TokenScope, depth: u8) -> PermissionToken {
    PermissionToken::try_issue(
        root,
        issuer.entity_id().clone(),
        scope,
        channel().hash(),
        30 * DAY,
        depth,
    )
    .unwrap()
}

fn rights(bits: &[TokenScope]) -> TokenScope {
    bits.iter().fold(TokenScope::NONE, |acc, s| acc.union(*s))
}

async fn mesh(identity: &Identity) -> Mesh {
    MeshBuilder::new("127.0.0.1:0", &PSK)
        .unwrap()
        .identity(identity.clone())
        .build()
        .await
        .unwrap()
}

async fn link(a: &Mesh, b: &Mesh) {
    let a_id = a.node_id();
    let node = b.node().clone();
    let accept = tokio::spawn(async move { node.accept(a_id).await });
    a.connect(&b.local_addr().to_string(), b.public_key(), b.node_id())
        .await
        .unwrap();
    accept.await.unwrap().unwrap();
    a.start();
    b.start();
}

#[test]
fn the_issuer_mints_only_publish_or_subscribe_within_its_grant() {
    let root = EntityKeypair::generate();
    let node = Identity::generate();
    let device = Identity::generate();
    let full = rights(&[
        TokenScope::PUBLISH,
        TokenScope::SUBSCRIBE,
        TokenScope::DELEGATE,
    ]);
    let issuer = ChannelLeafIssuer::new(grant(&root, &node, full, 1), keypair(&node)).unwrap();
    assert_eq!(issuer.root(), root.entity_id());
    assert_eq!(issuer.channel_hash(), channel().hash());

    let chain = issuer
        .issue(device.entity_id(), TokenScope::SUBSCRIBE)
        .unwrap();
    assert_eq!(chain.tokens.len(), 2, "root → node → device");
    chain
        .verify_authorizes(
            TokenScope::SUBSCRIBE,
            channel().hash(),
            device.entity_id(),
            &[root.entity_id().clone()],
            &net::adapter::net::identity::RevocationRegistry::new(),
            0,
        )
        .unwrap();
    // Never ADMIN, WILDCARD or DELEGATE, never empty, never beyond the grant.
    for refused in [
        TokenScope::ADMIN,
        TokenScope::WILDCARD,
        TokenScope::DELEGATE,
    ] {
        assert_eq!(
            issuer.issue(device.entity_id(), refused).err(),
            Some(ChannelIssueError::Forbidden),
            "{refused:?}"
        );
    }
    assert_eq!(
        issuer.issue(device.entity_id(), TokenScope::NONE).err(),
        Some(ChannelIssueError::Rights)
    );
    let subscribe_only = ChannelLeafIssuer::new(
        grant(
            &root,
            &node,
            rights(&[TokenScope::SUBSCRIBE, TokenScope::DELEGATE]),
            1,
        ),
        keypair(&node),
    )
    .unwrap();
    assert!(matches!(
        subscribe_only.issue(device.entity_id(), TokenScope::PUBLISH),
        Err(ChannelIssueError::Rights)
    ));
    // A grant for someone else, one that cannot delegate, or one carrying
    // ADMIN is refused at construction.
    assert!(matches!(
        ChannelLeafIssuer::new(grant(&root, &device, full, 1), keypair(&node)),
        Err(ChannelIssueError::NotTheGrantee)
    ));
    assert!(matches!(
        ChannelLeafIssuer::new(grant(&root, &node, full, 0), keypair(&node)),
        Err(ChannelIssueError::CannotDelegate)
    ));
    assert!(matches!(
        ChannelLeafIssuer::new(
            grant(&root, &node, full.union(TokenScope::ADMIN), 1),
            keypair(&node)
        ),
        Err(ChannelIssueError::Forbidden)
    ));
}

/// The publisher trusts only the root. The device's full delegated chain
/// subscribes; the leaf alone does not (it does not anchor at the root), and
/// neither does a chain minted for another device.
#[tokio::test]
async fn a_delegated_chain_subscribes_where_its_leaf_alone_does_not() {
    let root = EntityKeypair::generate();
    let node = Identity::generate();
    let publisher = mesh(&node).await;
    publisher.register_channel(gated(&root));
    let issuer = ChannelLeafIssuer::new(
        grant(
            &root,
            &node,
            rights(&[TokenScope::SUBSCRIBE, TokenScope::DELEGATE]),
            1,
        ),
        keypair(&node),
    )
    .unwrap();

    let device = Identity::generate();
    let subscriber = mesh(&device).await;
    link(&subscriber, &publisher).await;
    let chain = issuer
        .issue(device.entity_id(), TokenScope::SUBSCRIBE)
        .unwrap();

    let attempt = |opts: SubscribeOptions| {
        let subscriber = &subscriber;
        let target = publisher.node_id();
        async move {
            subscriber
                .subscribe_channel_with(target, &channel(), opts)
                .await
        }
    };
    let leaf_only = TokenChain::single(chain.tokens[1].clone());
    assert!(attempt(SubscribeOptions {
        chain: Some(leaf_only),
        ..Default::default()
    })
    .await
    .is_err());
    let someone_else = issuer
        .issue(Identity::generate().entity_id(), TokenScope::SUBSCRIBE)
        .unwrap();
    assert!(attempt(SubscribeOptions {
        chain: Some(someone_else),
        ..Default::default()
    })
    .await
    .is_err());
    attempt(SubscribeOptions {
        chain: Some(chain.clone()),
        ..Default::default()
    })
    .await
    .expect("the full delegated chain subscribes");
    // A token and a chain together is a caller error.
    assert!(attempt(SubscribeOptions {
        chain: Some(chain.clone()),
        token: Some(chain.tokens[1].clone()),
    })
    .await
    .is_err());
}

/// Publish is local: it passes only while the device's own config trusts
/// the root AND it holds its managed chain. One managed chain per channel;
/// a stale removal never removes a successor; removal also evicts the
/// chain's tokens from the token cache.
#[tokio::test]
async fn publish_needs_the_managed_chain_and_exact_removal_spares_a_successor() {
    let root = EntityKeypair::generate();
    let node = Identity::generate();
    let issuer = ChannelLeafIssuer::new(
        grant(
            &root,
            &node,
            rights(&[TokenScope::PUBLISH, TokenScope::DELEGATE]),
            1,
        ),
        keypair(&node),
    )
    .unwrap();
    let device = Identity::generate();
    let local = mesh(&device).await;
    local.register_channel(gated(&root));
    local.start();
    let ch = channel();
    let publish = || local.publish(&ch, Bytes::from_static(b"reading"), Default::default());

    assert!(publish().await.is_err(), "no credential: the gate refuses");
    let first = issuer
        .issue(device.entity_id(), TokenScope::PUBLISH)
        .unwrap();
    // The leaf sits in the token cache too (as a joined device's might).
    device.install_token(first.tokens[1].clone()).unwrap();
    let fp = local
        .install_publish_chain(&channel(), first.clone())
        .unwrap();
    assert_eq!(local.publish_chain_fingerprint(&channel()), Some(fp));
    publish()
        .await
        .expect("the managed chain passes the local gate");

    // One managed chain per channel: a different one is refused.
    std::thread::sleep(Duration::from_millis(1100));
    let second = issuer
        .issue(device.entity_id(), TokenScope::PUBLISH)
        .unwrap();
    assert_ne!(second.fingerprint(), fp);
    assert!(local
        .install_publish_chain(&channel(), second.clone())
        .is_err());
    // Installing the same chain again is a no-op.
    assert_eq!(
        local.install_publish_chain(&channel(), first.clone()),
        Ok(fp)
    );

    // Exact removal: the wrong fingerprint removes nothing.
    assert!(!local.remove_publish_chain_if(&channel(), &second.fingerprint()));
    publish().await.expect("still held");
    assert!(local.remove_publish_chain_if(&channel(), &fp));
    assert!(publish().await.is_err(), "removed: the gate refuses again");
    assert!(
        device
            .lookup_token(device.entity_id(), &channel())
            .is_none(),
        "the chain's leaf was evicted from the token cache"
    );

    // A successor installed after the removal is not touched by a stale
    // removal of the first incarnation.
    let fp2 = local.install_publish_chain(&channel(), second).unwrap();
    assert!(!local.remove_publish_chain_if(&channel(), &fp));
    assert_eq!(local.publish_chain_fingerprint(&channel()), Some(fp2));
    publish().await.expect("the successor still publishes");
}
