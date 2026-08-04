//! S5 of `docs/internal/plans/SUBNET_AUTH_PLAN.md` — signed control
//! facts and revocation distribution.
//!
//! Four independently signed facts (descriptor, gateway
//! advertisement, export policy, revocation floor) verified by ONE
//! root-anchored rule regardless of arrival path: a configured
//! channel, local provisioning, and a configuration-bundle fixture
//! hand the same bytes to the same verifier. Channel membership and
//! publication never establish fact authority; revisions are
//! monotonic per `(SubnetRef, fact kind)`; replay and reorder never
//! roll state backward; floors keep the S2 bounded-stale contract;
//! and subnet floors are independent from org membership state in
//! both directions.
//!
//! Run: `cargo test --features net --test subnet_control_facts`

#![cfg(feature = "net")]

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::behavior::org::{OrgKeypair, OrgMembershipCert, OrgRevocationBundle};
use net::adapter::net::behavior::org_revocation::OrgRevocationState;
use net::adapter::net::identity::EntityKeypair;
use net::adapter::net::subnet::{
    auth::verify_credential_set, GatewayAdvertisement, SubnetAuthError, SubnetAuthorityConfig,
    SubnetControlFact, SubnetCredentialSet, SubnetDescriptor, SubnetExportPolicy, SubnetGrant,
    SubnetRef, SubnetRevocationFloor, SubnetRights, TopologySubnetId,
};
use net::adapter::net::{
    ChannelName, ChannelPublisher, MeshNode, MeshNodeConfig, OnFailure, PublishConfig, Reliability,
    SocketBufferConfig,
};
use net::adapter::Adapter;

const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x42u8; 32];
const DAY: u64 = 24 * 60 * 60;
const SKEW: u64 = 30;

fn real_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_secs()
}

fn kp(seed: u8) -> EntityKeypair {
    EntityKeypair::from_bytes([seed; 32])
}

fn scope_of(root: &EntityKeypair, levels: &[u8]) -> SubnetRef {
    SubnetRef {
        authority: root.entity_id().clone(),
        path: TopologySubnetId::new(levels),
    }
}

fn authority_config(root: &EntityKeypair) -> SubnetAuthorityConfig {
    SubnetAuthorityConfig {
        authority: root.entity_id().clone(),
        roots: vec![root.entity_id().clone()],
        maximum_grant_lifetime_secs: 7 * DAY,
    }
}

fn descriptor_fact(root: &EntityKeypair, levels: &[u8], revision: u64) -> SubnetControlFact {
    SubnetControlFact::Descriptor(
        SubnetDescriptor::try_issue(root, scope_of(root, levels), 0, revision, real_now())
            .expect("issue descriptor"),
    )
}

fn gateway_fact(
    root: &EntityKeypair,
    levels: &[u8],
    gateway: &EntityKeypair,
    revision: u64,
) -> SubnetControlFact {
    let now = real_now();
    SubnetControlFact::GatewayAdvertisement(
        GatewayAdvertisement::try_issue(
            root,
            scope_of(root, levels),
            0,
            gateway.entity_id().clone(),
            gateway.node_id(),
            revision,
            now - 60,
            now + 3600,
        )
        .expect("issue gateway advertisement"),
    )
}

fn export_fact(
    root: &EntityKeypair,
    levels: &[u8],
    channels: Vec<u64>,
    revision: u64,
) -> SubnetControlFact {
    let now = real_now();
    SubnetControlFact::ExportPolicy(
        SubnetExportPolicy::try_issue(
            root,
            scope_of(root, levels),
            0,
            channels,
            revision,
            now - 60,
            now + 3600,
        )
        .expect("issue export policy"),
    )
}

fn floor_fact(
    root: &EntityKeypair,
    levels: &[u8],
    minimum_generation: u32,
    revision: u64,
) -> SubnetControlFact {
    SubnetControlFact::RevocationFloor(
        SubnetRevocationFloor::try_issue(
            root,
            scope_of(root, levels),
            0,
            minimum_generation,
            revision,
            real_now(),
        )
        .expect("issue floor"),
    )
}

fn grant_set(
    root: &EntityKeypair,
    subject: &EntityKeypair,
    levels: &[u8],
    generation: u32,
    now: u64,
) -> SubnetCredentialSet {
    SubnetCredentialSet::Direct(
        SubnetGrant::try_issue(
            root,
            root.entity_id().clone(),
            TopologySubnetId::new(levels),
            0,
            subject.entity_id().clone(),
            SubnetRights::ATTACH,
            generation,
            now - 60,
            DAY,
        )
        .expect("issue grant"),
    )
}

async fn build_node(configure: impl FnOnce(MeshNodeConfig) -> MeshNodeConfig) -> Arc<MeshNode> {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_num_shards(1)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(3, Duration::from_secs(2));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: TEST_BUFFER_SIZE,
        recv_buffer_size: TEST_BUFFER_SIZE,
    };
    Arc::new(
        MeshNode::new(EntityKeypair::generate(), configure(cfg))
            .await
            .expect("MeshNode::new"),
    )
}

async fn handshake(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    let a_id = a.node_id();
    let b_id = b.node_id();
    let b_pub = *b.public_key();
    let b_addr = b.local_addr();
    let b_clone = b.clone();
    let accept = tokio::spawn(async move { b_clone.accept(a_id).await });
    a.connect(b_addr, &b_pub, b_id).await.expect("connect");
    accept.await.expect("accept task").expect("accept");
}

async fn wait_until<F>(mut cond: F) -> bool
where
    F: FnMut() -> bool,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    cond()
}

fn publisher_for(name: ChannelName) -> ChannelPublisher {
    ChannelPublisher::new(
        name,
        PublishConfig {
            reliability: Reliability::FireAndForget,
            on_failure: OnFailure::BestEffort,
            max_inflight: 16,
        },
    )
}

/// A publisher node + a consumer node whose
/// `subnet_control_channel` is `channel`, connected and subscribed:
/// everything the publisher publishes on it reaches the consumer's
/// dispatch path.
async fn control_channel_pair(
    channel: &ChannelName,
    consumer_root: &EntityKeypair,
) -> (Arc<MeshNode>, Arc<MeshNode>) {
    // No channel registry on either side: the publisher runs the
    // default admit-with-warning policy, which is exactly the open-
    // channel H1 posture the harmlessness witnesses assume — fact
    // authority must come from the signatures alone.
    let publisher = build_node(|cfg| cfg).await;

    let consumer = build_node(|cfg| {
        cfg.with_subnet_authority(authority_config(consumer_root))
            .with_subnet_control_channel(channel.clone())
    })
    .await;

    handshake(&publisher, &consumer).await;
    publisher.start();
    consumer.start();

    publisher
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("publisher announce");
    consumer
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("consumer announce");
    let pub_id = publisher.node_id();
    assert!(
        wait_until(|| consumer.test_capability_fold_has(pub_id)).await,
        "consumer never indexed the publisher's announcement"
    );

    // The consumer subscribes to the publisher's channel, so the
    // publisher's fan-out includes it.
    consumer
        .subscribe_channel(publisher.node_id(), channel.clone())
        .await
        .expect("subscribe");

    (publisher, consumer)
}

// ---------------------------------------------------------------------------
// Verification is arrival-path independent
// ---------------------------------------------------------------------------

/// The same fact bytes verify identically via the configured
/// channel, local provisioning, and a configuration-bundle fixture —
/// and the channel consumer is NON-exclusive: subscribers still
/// receive the event.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fact_verifies_identically_via_channel_local_and_bundle() {
    let root = kp(1);
    let fact = descriptor_fact(&root, &[3, 7], 5);
    let bytes = fact.to_bytes();
    let path = TopologySubnetId::new(&[3, 7]);

    // Path 1: local provisioning.
    let local = build_node(|cfg| cfg.with_subnet_authority(authority_config(&root))).await;
    let outcome = local
        .apply_subnet_control_fact(&bytes)
        .expect("local apply");
    assert!(outcome.applied);

    // Path 2: configuration-bundle fixture — the bytes ride a file.
    let bundle_path =
        std::env::temp_dir().join(format!("subnet_control_bundle_{}.bin", std::process::id()));
    std::fs::write(&bundle_path, &bytes).expect("write bundle");
    let from_bundle = std::fs::read(&bundle_path).expect("read bundle");
    std::fs::remove_file(&bundle_path).ok();
    let bundled = build_node(|cfg| cfg.with_subnet_authority(authority_config(&root))).await;
    let outcome = bundled
        .apply_subnet_control_fact(&from_bundle)
        .expect("bundle apply");
    assert!(outcome.applied);

    // Path 3: the configured channel.
    let channel = ChannelName::new("ops/subnet-control").unwrap();
    let (publisher, consumer) = control_channel_pair(&channel, &root).await;
    publisher
        .publish(&publisher_for(channel), Bytes::from(bytes))
        .await
        .expect("publish fact");
    assert!(
        wait_until(|| {
            consumer
                .subnet_control_store()
                .descriptor_for(root.entity_id(), 0, path)
                .is_some()
        })
        .await,
        "channel-borne fact never applied"
    );

    // All three stores hold the identical fact.
    let expected = match &fact {
        SubnetControlFact::Descriptor(d) => d.clone(),
        _ => unreachable!(),
    };
    for (label, node) in [
        ("local", &local),
        ("bundle", &bundled),
        ("channel", &consumer),
    ] {
        assert_eq!(
            node.subnet_control_store()
                .descriptor_for(root.entity_id(), 0, path),
            Some(expected.clone()),
            "{label} path must hold the identical verified fact"
        );
    }

    // Non-exclusive: the event still reached the consumer's shard
    // queue for ordinary subscribers (num_shards = 1, so shard 0).
    let polled = consumer.poll_shard(0, None, 16).await.expect("poll");
    assert!(
        !polled.events.is_empty(),
        "the control-facts consumer must not steal events from subscribers"
    );

    publisher.shutdown().await.expect("publisher shutdown");
    consumer.shutdown().await.expect("consumer shutdown");
}

// ---------------------------------------------------------------------------
// Authority comes from roots, not from arrival privilege
// ---------------------------------------------------------------------------

/// Channel membership grants no gateway right: a fully legitimate
/// channel member publishing a gateway advertisement signed by its
/// OWN key changes nothing, and even a root-signed advertisement
/// naming a peer confers no forwarding authority on that peer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn channel_membership_grants_no_gateway_right() {
    let root = kp(1);
    let channel = ChannelName::new("ops/subnet-control").unwrap();
    let (publisher, consumer) = control_channel_pair(&channel, &root).await;
    let path = TopologySubnetId::new(&[3]);

    // The publisher signs an advertisement naming ITSELF, with its
    // own (non-root) key. It is a member of the channel in good
    // standing; that buys it nothing.
    let publisher_kp = EntityKeypair::generate();
    let hostile = GatewayAdvertisement::try_issue(
        &publisher_kp,
        SubnetRef {
            authority: root.entity_id().clone(),
            path,
        },
        0,
        publisher_kp.entity_id().clone(),
        publisher.node_id(),
        1,
        real_now() - 60,
        real_now() + 3600,
    )
    .expect("issue hostile ad");
    publisher
        .publish(
            &publisher_for(channel.clone()),
            Bytes::from(SubnetControlFact::GatewayAdvertisement(hostile).to_bytes()),
        )
        .await
        .expect("publish hostile ad");

    // Deliveries are ordered per stream; publish a valid descriptor
    // afterwards and wait for IT, so the hostile ad has provably
    // been processed by then.
    publisher
        .publish(
            &publisher_for(channel.clone()),
            Bytes::from(descriptor_fact(&root, &[9], 1).to_bytes()),
        )
        .await
        .expect("publish marker");
    assert!(
        wait_until(|| {
            consumer
                .subnet_control_store()
                .descriptor_for(root.entity_id(), 0, TopologySubnetId::new(&[9]))
                .is_some()
        })
        .await,
        "marker fact never applied"
    );
    assert!(
        consumer
            .subnet_control_store()
            .gateway_for(root.entity_id(), 0, path, real_now(), SKEW)
            .is_none(),
        "a channel member's self-signed advertisement must be inert"
    );

    // Even a ROOT-signed advertisement is discovery, not authority:
    // the advertised peer still has no admitted subnet context and
    // no gateway credentials on the consumer.
    let root_signed = gateway_fact(&root, &[3], &publisher_kp, 2);
    publisher
        .publish(
            &publisher_for(channel.clone()),
            Bytes::from(root_signed.to_bytes()),
        )
        .await
        .expect("publish root-signed ad");
    assert!(
        wait_until(|| {
            consumer
                .subnet_control_store()
                .gateway_for(root.entity_id(), 0, path, real_now(), SKEW)
                .is_some()
        })
        .await,
        "root-signed advertisement should apply"
    );
    assert!(
        consumer.subnet_context_for(publisher.node_id()).is_none(),
        "an advertisement is not an admission — the advertised peer \
         holds no verified subnet context"
    );

    publisher.shutdown().await.expect("publisher shutdown");
    consumer.shutdown().await.expect("consumer shutdown");
}

/// Hostile or malformed channel bytes are harmless — despite open
/// H1, the event plane's lack of receive-side token checks: garbage,
/// truncations, oversized counts, unknown tags, and wrong-authority
/// facts all leave the store, the floor registry, and the node
/// itself untouched and live.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn invalid_injected_channel_bytes_are_harmless() {
    let root = kp(1);
    let other_authority = kp(2);
    let channel = ChannelName::new("ops/subnet-control").unwrap();
    let (publisher, consumer) = control_channel_pair(&channel, &root).await;

    let mut truncated = descriptor_fact(&root, &[1], 1).to_bytes();
    truncated.truncate(truncated.len() / 2);
    let mut unknown_tag = descriptor_fact(&root, &[1], 1).to_bytes();
    unknown_tag[0] = 0xEE;
    let payloads: Vec<Bytes> = vec![
        Bytes::from_static(b""),
        Bytes::from_static(b"not a fact at all"),
        Bytes::from(vec![0xFFu8; 4096]),
        Bytes::from(truncated),
        Bytes::from(unknown_tag),
        // Structurally valid, correctly signed — by an authority this
        // consumer does not anchor.
        Bytes::from(descriptor_fact(&other_authority, &[1], 1).to_bytes()),
    ];
    for payload in payloads {
        publisher
            .publish(&publisher_for(channel.clone()), payload)
            .await
            .expect("publish hostile payload");
    }

    // A valid fact published AFTER the barrage still applies — the
    // consumer survived and the pipeline is intact.
    publisher
        .publish(
            &publisher_for(channel.clone()),
            Bytes::from(descriptor_fact(&root, &[2], 1).to_bytes()),
        )
        .await
        .expect("publish valid fact");
    assert!(
        wait_until(|| {
            consumer
                .subnet_control_store()
                .descriptor_for(root.entity_id(), 0, TopologySubnetId::new(&[2]))
                .is_some()
        })
        .await,
        "the node must keep verifying facts after hostile input"
    );

    assert!(
        consumer
            .subnet_control_store()
            .descriptor_for(root.entity_id(), 0, TopologySubnetId::new(&[1]))
            .is_none(),
        "no hostile payload may have materialized state"
    );
    assert!(
        consumer
            .subnet_control_store()
            .descriptor_for(other_authority.entity_id(), 0, TopologySubnetId::new(&[1]))
            .is_none(),
        "an unanchored authority's fact must not apply"
    );
    assert_eq!(
        consumer
            .subnet_floor_registry()
            .auth_epoch(root.entity_id()),
        0,
        "hostile bytes must not move the auth epoch"
    );

    publisher.shutdown().await.expect("publisher shutdown");
    consumer.shutdown().await.expect("consumer shutdown");
}

// ---------------------------------------------------------------------------
// Node-surface semantics (local provisioning path)
// ---------------------------------------------------------------------------

/// Unsigned and wrong-authority facts change no state through the
/// node surface, and the errors are the family's stable codes.
#[tokio::test]
async fn unsigned_and_wrong_authority_facts_change_no_state() {
    let root = kp(1);
    let unanchored = kp(2);
    let node = build_node(|cfg| cfg.with_subnet_authority(authority_config(&root))).await;

    // Tampered signature.
    let SubnetControlFact::Descriptor(mut desc) = descriptor_fact(&root, &[3], 1) else {
        unreachable!()
    };
    desc.signature[0] ^= 1;
    assert_eq!(
        node.apply_subnet_control_fact(&SubnetControlFact::Descriptor(desc).to_bytes()),
        Err(SubnetAuthError::InvalidSignature)
    );

    // An authority this node does not anchor.
    assert_eq!(
        node.apply_subnet_control_fact(&descriptor_fact(&unanchored, &[3], 1).to_bytes()),
        Err(SubnetAuthError::UnknownAuthority)
    );

    // Correct authority, non-root issuer.
    let outsider = EntityKeypair::generate();
    let forged = SubnetDescriptor::try_issue(&outsider, scope_of(&root, &[3]), 0, 1, real_now())
        .expect("issue");
    assert_eq!(
        node.apply_subnet_control_fact(&SubnetControlFact::Descriptor(forged).to_bytes()),
        Err(SubnetAuthError::IssuerNotAuthorized)
    );

    assert!(node
        .subnet_control_store()
        .descriptor_for(root.entity_id(), 0, TopologySubnetId::new(&[3]))
        .is_none());
    assert_eq!(node.subnet_floor_registry().auth_epoch(root.entity_id()), 0);
}

/// Revision monotonicity per `(SubnetRef, kind)` at the node
/// surface, kind-independence included: a newer gateway fact does
/// not suppress a legitimate export-policy fact, and replay/reorder
/// converge without ever rolling back.
#[tokio::test]
async fn revisions_are_monotonic_and_kind_independent() {
    let root = kp(1);
    let gw = EntityKeypair::generate();
    let node = build_node(|cfg| cfg.with_subnet_authority(authority_config(&root))).await;
    let path = TopologySubnetId::new(&[3, 7]);
    let now = real_now();

    // Export policy at revision 1, then a gateway fact at a far
    // higher revision for the SAME scope.
    assert!(
        node.apply_subnet_control_fact(&export_fact(&root, &[3, 7], vec![0xAAAA], 1).to_bytes())
            .expect("apply export")
            .applied
    );
    assert!(
        node.apply_subnet_control_fact(&gateway_fact(&root, &[3, 7], &gw, 99).to_bytes())
            .expect("apply gateway")
            .applied
    );

    let store = node.subnet_control_store();
    assert_eq!(
        store
            .export_policy_for(root.entity_id(), 0, path, now, SKEW)
            .expect("export policy survives")
            .exported_channels,
        vec![0xAAAA],
        "a newer gateway fact must not suppress the export policy"
    );
    // The export stream still advances from ITS OWN revision.
    assert!(
        node.apply_subnet_control_fact(&export_fact(&root, &[3, 7], vec![0xBBBB], 2).to_bytes())
            .expect("apply export 2")
            .applied
    );

    // Replay and regression are applied=false no-ops.
    assert!(
        !node
            .apply_subnet_control_fact(&export_fact(&root, &[3, 7], vec![0xBBBB], 2).to_bytes())
            .expect("replay")
            .applied
    );
    assert!(
        !node
            .apply_subnet_control_fact(&export_fact(&root, &[3, 7], vec![0xCCCC], 1).to_bytes())
            .expect("reorder")
            .applied
    );
    assert_eq!(
        store
            .export_policy_for(root.entity_id(), 0, path, now, SKEW)
            .expect("still present")
            .exported_channels,
        vec![0xBBBB],
        "replay/reorder must never roll state backward"
    );
}

/// The bounded-stale floor contract, driven through the fact path: a
/// verifier may honor an old grant until the newer floor ARRIVES;
/// arrival then revokes exactly the floored subtree, moves the auth
/// epoch once, and replays move nothing.
#[tokio::test]
async fn a_delayed_floor_is_bounded_stale_until_it_arrives() {
    let root = kp(1);
    let subject = kp(2);
    let node = build_node(|cfg| cfg.with_subnet_authority(authority_config(&root))).await;
    let cfg = authority_config(&root);
    let now = real_now();
    let old_grant = grant_set(&root, &subject, &[3, 7], 1, now);

    // The floor exists in the world but has not arrived here: the
    // old grant still verifies. That is the documented contract, not
    // a defect.
    assert!(verify_credential_set(
        &old_grant,
        subject.entity_id(),
        &cfg,
        0,
        node.subnet_floor_registry(),
        now,
        SKEW,
    )
    .is_ok());

    // The floor arrives (as distributed bytes, not a typed call).
    let outcome = node
        .apply_subnet_control_fact(&floor_fact(&root, &[3, 7], 5, 1).to_bytes())
        .expect("apply floor");
    assert!(outcome.applied);
    assert_eq!(node.subnet_floor_registry().auth_epoch(root.entity_id()), 1);

    // Now the old grant is dead…
    assert_eq!(
        verify_credential_set(
            &old_grant,
            subject.entity_id(),
            &cfg,
            0,
            node.subnet_floor_registry(),
            now,
            SKEW,
        ),
        Err(SubnetAuthError::Revoked)
    );
    // …while a sibling subtree is untouched.
    let chassis_grant = grant_set(&root, &subject, &[3, 8], 1, now);
    assert!(verify_credential_set(
        &chassis_grant,
        subject.entity_id(),
        &cfg,
        0,
        node.subnet_floor_registry(),
        now,
        SKEW,
    )
    .is_ok());

    // A replayed floor moves nothing — same epoch, applied=false.
    assert!(
        !node
            .apply_subnet_control_fact(&floor_fact(&root, &[3, 7], 5, 1).to_bytes())
            .expect("replay floor")
            .applied
    );
    assert_eq!(node.subnet_floor_registry().auth_epoch(root.entity_id()), 1);
}

/// Subnet floors and org membership are independent state in both
/// directions: a subnet floor revokes subnet grants while org
/// membership certificates and org revocation floors are untouched,
/// and raising an org floor moves no subnet floor state.
#[tokio::test]
async fn subnet_floors_and_org_membership_are_independent() {
    let root = kp(1);
    let subject = kp(2);
    let org = OrgKeypair::generate();
    let node = build_node(|cfg| cfg.with_subnet_authority(authority_config(&root))).await;
    let cfg = authority_config(&root);
    let now = real_now();

    // The subject is an org member (generation 3) and holds a
    // subnet grant (generation 1).
    let cert = OrgMembershipCert::try_issue(&org, subject.entity_id().clone(), 3, DAY)
        .expect("issue org cert");
    let mut org_state = OrgRevocationState::empty();

    // Direction 1: a subnet floor arrives; subnet grant dies, org
    // state does not move.
    assert!(
        node.apply_subnet_control_fact(&floor_fact(&root, &[3, 7], 5, 1).to_bytes())
            .expect("apply subnet floor")
            .applied
    );
    assert_eq!(
        verify_credential_set(
            &grant_set(&root, &subject, &[3, 7], 1, now),
            subject.entity_id(),
            &cfg,
            0,
            node.subnet_floor_registry(),
            now,
            SKEW,
        ),
        Err(SubnetAuthError::Revoked)
    );
    assert!(cert.verify().is_ok(), "org membership must be untouched");
    assert_eq!(
        org_state.floor_for(&org.org_id(), subject.entity_id()),
        0,
        "no org floor may appear from a subnet floor"
    );

    // Direction 2: the org revokes the member (floor above the
    // cert's generation); subnet floor state does not move.
    let epoch_before = node.subnet_floor_registry().auth_epoch(root.entity_id());
    let mut floors = BTreeMap::new();
    floors.insert(subject.entity_id().clone(), 9u32);
    let bundle = OrgRevocationBundle::try_issue(&org, &floors).expect("issue org bundle");
    assert_eq!(org_state.merge_bundle(&bundle), 1);
    assert_eq!(org_state.floor_for(&org.org_id(), subject.entity_id()), 9);

    assert_eq!(
        node.subnet_floor_registry().auth_epoch(root.entity_id()),
        epoch_before,
        "org revocation must not advance the subnet auth epoch"
    );
    // A subnet grant at a generation above the SUBNET floor still
    // verifies — the org floor has no reach into subnet state.
    assert!(verify_credential_set(
        &grant_set(&root, &subject, &[3, 7], 6, now),
        subject.entity_id(),
        &cfg,
        0,
        node.subnet_floor_registry(),
        now,
        SKEW,
    )
    .is_ok());
}
