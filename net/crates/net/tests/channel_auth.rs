//! Integration tests for channel authentication enforcement
//! (Stage E of `SDK_SECURITY_SURFACE_PLAN.md`). Exercises the
//! cap-filter + token paths end-to-end through the publish /
//! subscribe hot paths.
//!
//! Run: `cargo test --features net --test channel_auth`

#![cfg(feature = "net")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use net::adapter::net::behavior::capability::{CapabilityFilter, CapabilitySet};
use net::adapter::net::{
    ChannelConfig, ChannelConfigRegistry, ChannelId, ChannelName, ChannelPublisher, EntityKeypair,
    MeshNode, MeshNodeConfig, OnFailure, PermissionToken, PublishConfig, Reliability,
    SocketBufferConfig, TokenCache, TokenScope,
};

const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x42u8; 32];

fn test_config() -> MeshNodeConfig {
    // Bind via `127.0.0.1:0` so the OS picks a free port — no
    // pre-bind reservation, no TOCTOU race with parallel tests.
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

/// A test node bundle: the mesh + its keypair (so tests can issue
/// tokens signed by this node) + the channel registry. The node's
/// `TokenCache` is installed on the mesh during construction but
/// not surfaced here — tests don't need to poke it directly.
struct Node {
    mesh: Arc<MeshNode>,
    keypair: EntityKeypair,
    registry: Arc<ChannelConfigRegistry>,
}

async fn build_node() -> Node {
    build_node_with_config(test_config()).await
}

async fn build_node_with_config(cfg: MeshNodeConfig) -> Node {
    let keypair = EntityKeypair::generate();
    let mut node = MeshNode::new(keypair.clone(), cfg)
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

/// Handshake A↔B without starting either node.
async fn handshake_no_start(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
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
}

async fn wait_until<F>(mut cond: F) -> bool
where
    F: FnMut() -> bool,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    cond()
}

/// Handshake + start + both nodes announce capabilities so the
/// publisher's peer_entity_ids has the subscriber's EntityId before
/// any subscribe attempt.
async fn setup_pair(a_caps: CapabilitySet, b_caps: CapabilitySet) -> (Node, Node) {
    setup_pair_with_configs(test_config(), test_config(), a_caps, b_caps).await
}

async fn setup_pair_with_configs(
    a_cfg: MeshNodeConfig,
    b_cfg: MeshNodeConfig,
    a_caps: CapabilitySet,
    b_caps: CapabilitySet,
) -> (Node, Node) {
    let a = build_node_with_config(a_cfg).await;
    let b = build_node_with_config(b_cfg).await;
    handshake_no_start(&a.mesh, &b.mesh).await;
    a.mesh.start();
    b.mesh.start();

    a.mesh
        .announce_capabilities(a_caps)
        .await
        .expect("A announce");
    b.mesh
        .announce_capabilities(b_caps)
        .await
        .expect("B announce");

    // Wait until A sees B's entity via the capability index — same
    // dispatch populates peer_entity_ids.
    let b_id = b.mesh.node_id();
    let learned = wait_until(|| a.mesh.test_capability_fold_has(b_id)).await;
    assert!(learned, "A never indexed B's capability announcement");

    (a, b)
}

#[tokio::test]
async fn subscribe_denied_by_cap_filter() {
    let (a, b) = setup_pair(CapabilitySet::new(), CapabilitySet::new()).await;

    let name = ChannelName::new("lab/gpu").unwrap();
    let filter = CapabilityFilter::new().require_tag("gpu");
    a.registry
        .insert(ChannelConfig::new(ChannelId::new(name.clone())).with_subscribe_caps(filter));

    let result = b.mesh.subscribe_channel(a.mesh.node_id(), name).await;
    assert!(
        result.is_err(),
        "subscribe should have been denied for missing subscribe_caps"
    );
}

#[tokio::test]
async fn subscribe_denied_by_missing_token() {
    let (a, b) = setup_pair(CapabilitySet::new(), CapabilitySet::new()).await;

    let name = ChannelName::new("lab/secret").unwrap();
    a.registry.insert(
        ChannelConfig::new(ChannelId::new(name.clone()))
            .with_token_roots(vec![a.keypair.entity_id().clone()]),
    );

    // B subscribes with no token.
    let result = b.mesh.subscribe_channel(a.mesh.node_id(), name).await;
    assert!(
        result.is_err(),
        "subscribe should have been denied for missing token"
    );
}

#[tokio::test]
async fn subscribe_accepted_with_valid_token() {
    let (a, b) = setup_pair(CapabilitySet::new(), CapabilitySet::new()).await;

    let name = ChannelName::new("lab/signed").unwrap();
    a.registry.insert(
        ChannelConfig::new(ChannelId::new(name.clone()))
            .with_token_roots(vec![a.keypair.entity_id().clone()]),
    );

    // Publisher issues a SUBSCRIBE token for B's entity. Duration
    // is generous so the test isn't timing-sensitive.
    let token = PermissionToken::issue(
        &a.keypair,
        b.keypair.entity_id().clone(),
        TokenScope::SUBSCRIBE,
        name.hash(),
        300,
        0,
    );

    b.mesh
        .subscribe_channel_with_token(a.mesh.node_id(), name, token)
        .await
        .expect("subscribe should be accepted with a valid token");
}

#[tokio::test]
async fn subscribe_rejected_with_expired_token() {
    let (a, b) = setup_pair(CapabilitySet::new(), CapabilitySet::new()).await;

    let name = ChannelName::new("lab/short").unwrap();
    a.registry.insert(
        ChannelConfig::new(ChannelId::new(name.clone()))
            .with_token_roots(vec![a.keypair.entity_id().clone()]),
    );

    // 1-second token, then sleep past `not_after`.
    // (duration_secs == 0 is now rejected; mint with the minimum
    // valid TTL and wait it out.)
    let token = PermissionToken::issue(
        &a.keypair,
        b.keypair.entity_id().clone(),
        TokenScope::SUBSCRIBE,
        name.hash(),
        1,
        0,
    );
    // Let now() tick past `not_after`.
    tokio::time::sleep(Duration::from_millis(1_100)).await;

    let result = b
        .mesh
        .subscribe_channel_with_token(a.mesh.node_id(), name, token)
        .await;
    assert!(result.is_err(), "expired token should not authorize");
}

#[tokio::test]
async fn subscribe_rejected_with_wrong_subject_token() {
    let (a, b) = setup_pair(CapabilitySet::new(), CapabilitySet::new()).await;

    let name = ChannelName::new("lab/wrong").unwrap();
    a.registry.insert(
        ChannelConfig::new(ChannelId::new(name.clone()))
            .with_token_roots(vec![a.keypair.entity_id().clone()]),
    );

    // Token issued for a THIRD entity, not B.
    let bystander = EntityKeypair::generate();
    let token = PermissionToken::issue(
        &a.keypair,
        bystander.entity_id().clone(),
        TokenScope::SUBSCRIBE,
        name.hash(),
        300,
        0,
    );

    // B presents it. The chain roots at A (a channel root), but its
    // leaf subject is `bystander`, not B (the presenter), so the
    // leaf-binding check in `TokenChain::verify_authorizes` rejects it.
    let result = b
        .mesh
        .subscribe_channel_with_token(a.mesh.node_id(), name, token)
        .await;
    assert!(
        result.is_err(),
        "token issued for a different subject must not authorize B"
    );
}

#[tokio::test]
async fn rejected_subscribe_retains_no_chain_and_no_cache_entry() {
    // An unauthorized subscribe must leave the publisher holding no
    // state for the rejected peer: not in the shared `TokenCache` (the
    // original DoS vector — self-signed tokens spammed into the cache
    // before the ACL check) and, post root-anchoring, not in
    // `subscriber_chains` either (a retained chain would be re-checked
    // by the sweep / publish path and is a memory-growth vector under
    // rejected-subscribe spam). The chain store is the live guard now:
    // the shared cache is no longer written on the subscribe path at
    // all, so asserting only on it is vacuous.
    let (a, b) = setup_pair(CapabilitySet::new(), CapabilitySet::new()).await;

    let name = ChannelName::new("lab/leak").unwrap();
    a.registry.insert(
        ChannelConfig::new(ChannelId::new(name.clone()))
            .with_token_roots(vec![a.keypair.entity_id().clone()]),
    );

    // Pre-test: publisher retains nothing.
    let shared_cache = a
        .mesh
        .token_cache()
        .cloned()
        .expect("publisher should have a shared token cache");
    assert_eq!(shared_cache.len(), 0, "precondition: empty cache");
    assert_eq!(
        a.mesh.subscriber_chain_count(),
        0,
        "precondition: no retained chains"
    );

    // B signs a token intended for a THIRD bystander entity, not
    // itself. The token is signature-valid but unauthorized for B.
    let bystander = EntityKeypair::generate();
    let token = PermissionToken::issue(
        &a.keypair,
        bystander.entity_id().clone(),
        TokenScope::SUBSCRIBE,
        name.hash(),
        300,
        0,
    );

    let result = b
        .mesh
        .subscribe_channel_with_token(a.mesh.node_id(), name, token)
        .await;
    assert!(result.is_err(), "unauthorized subscribe must be rejected");

    // Post-test: a rejected subscribe must retain no chain (the live
    // regression guard) and still touch nothing in the shared cache.
    assert_eq!(
        a.mesh.subscriber_chain_count(),
        0,
        "rejected subscribe must not retain a token chain"
    );
    assert_eq!(
        shared_cache.len(),
        0,
        "rejected subscribe must not populate the shared token cache"
    );
}

#[tokio::test]
async fn publish_denied_by_own_cap_filter() {
    let (a, b) = setup_pair(
        CapabilitySet::new(), // A has NO `admin` tag
        CapabilitySet::new(),
    )
    .await;

    let name = ChannelName::new("lab/admin-only").unwrap();
    let filter = CapabilityFilter::new().require_tag("admin");
    a.registry
        .insert(ChannelConfig::new(ChannelId::new(name.clone())).with_publish_caps(filter));

    // Suppress unused-variable warning; test just needs `b` alive.
    let _ = b;

    let publisher = ChannelPublisher::new(
        name.clone(),
        PublishConfig {
            reliability: Reliability::FireAndForget,
            on_failure: OnFailure::BestEffort,
            max_inflight: 16,
        },
    );
    let result = a.mesh.publish(&publisher, Bytes::from_static(b"x")).await;
    assert!(
        result.is_err(),
        "publisher without required caps must not publish"
    );
}

#[tokio::test]
async fn unauth_channel_accepts_everyone() {
    // Backwards-compat regression: no subscribe_caps, no
    // publish_caps, no require_token → open channel.
    let (a, b) = setup_pair(CapabilitySet::new(), CapabilitySet::new()).await;

    let name = ChannelName::new("lab/open").unwrap();
    a.registry
        .insert(ChannelConfig::new(ChannelId::new(name.clone())));

    b.mesh
        .subscribe_channel(a.mesh.node_id(), name.clone())
        .await
        .expect("open channel must accept any subscriber");

    let publisher = ChannelPublisher::new(
        name,
        PublishConfig {
            reliability: Reliability::FireAndForget,
            on_failure: OnFailure::BestEffort,
            max_inflight: 16,
        },
    );
    let report = a
        .mesh
        .publish(&publisher, Bytes::from_static(b"hello"))
        .await
        .expect("open-channel publish");
    assert_eq!(report.attempted, 1);
    assert_eq!(report.delivered, 1);
}

/// A subscribe whose first transmission never reaches the publisher
/// must still land, via the retransmits `membership_max_attempts`
/// allows inside the one `membership_ack_timeout` budget.
///
/// The membership subprotocol is fire-and-forget UDP —
/// `send_subprotocol_to_node` hands a frame to `send_datagram` with no
/// NACKs and no retransmit window. So a single dropped datagram in
/// either direction used to cost the caller the entire ack timeout and
/// surface as a hard `Connection` error, on the one control plane whose
/// callers all just retry anyway. That is what made
/// `subscribe_accepted_with_valid_token` flake under `llvm-cov` on CI:
/// 16 concurrent tests on a small runner starved the receive task, the
/// kernel buffer filled, and one loopback packet went missing.
///
/// The partition filter stands in for the loss. It drops in BOTH
/// directions at the node that holds it, so blocking B at A discards
/// the `Subscribe` itself rather than only its `Ack` — a strictly
/// harder case than the CI flake, and the one a single-shot request
/// cannot survive at all.
#[tokio::test]
async fn subscribe_survives_a_dropped_membership_datagram() {
    // Three attempts across a 3 s budget → roughly one per second.
    let mut b_cfg = test_config();
    b_cfg.membership_ack_timeout = Duration::from_secs(3);
    b_cfg.membership_max_attempts = 3;

    let (a, b) = setup_pair_with_configs(
        test_config(),
        b_cfg,
        CapabilitySet::new(),
        CapabilitySet::new(),
    )
    .await;

    let name = ChannelName::new("lab/lossy").unwrap();
    a.registry
        .insert(ChannelConfig::new(ChannelId::new(name.clone())));

    // Black-hole B at A, so B's first attempt is dropped outright.
    let b_addr = b.mesh.local_addr();
    a.mesh.block_peer(b_addr);

    let a_id = a.mesh.node_id();
    let subscribe = tokio::spawn({
        let b_mesh = b.mesh.clone();
        let name = name.clone();
        async move { b_mesh.subscribe_channel(a_id, name).await }
    });

    // Heal after the first attempt's slice has certainly elapsed but
    // well inside `session_timeout`, so the peer is never declared
    // failed — this is packet loss, not a partition.
    tokio::time::sleep(Duration::from_millis(1_200)).await;
    a.mesh.unblock_peer(&b_addr);

    subscribe
        .await
        .expect("subscribe task panicked")
        .expect("a retransmit must carry the subscribe once the loss clears");

    // The subscribe really took effect — a retransmit that merely
    // returned Ok without rostering the peer would be worse than the
    // timeout it replaced.
    let publisher = ChannelPublisher::new(
        name,
        PublishConfig {
            reliability: Reliability::FireAndForget,
            on_failure: OnFailure::BestEffort,
            max_inflight: 16,
        },
    );
    let report = a
        .mesh
        .publish(&publisher, Bytes::from_static(b"after-loss"))
        .await
        .expect("publish after a recovered subscribe");
    assert_eq!(
        report.delivered, 1,
        "the recovered subscribe must leave the peer actually rostered"
    );
}

/// M1 (2026-07-31 audit): a token-gated **prefix** channel must work
/// end to end — subscribe accepted AND the first publish delivered.
///
/// Pre-fix the gate and the chain retention both keyed on
/// `cfg.channel_id.hash()`, which for a prefix config is a sentinel
/// standing for the whole family. Two things went wrong at once:
/// a token minted for the sentinel authorized every channel under the
/// prefix, and the chain was retained under the sentinel key while the
/// publish path looked it up by the real channel — so a legitimate
/// subscriber was accepted with a successful Ack and then revoked
/// before a single event reached it. Accept-then-fail-closed, reported
/// to the peer as success.
#[tokio::test]
async fn token_gated_prefix_channel_subscribes_and_delivers() {
    let (a, b) = setup_pair(CapabilitySet::new(), CapabilitySet::new()).await;

    // Registered as a PREFIX, with a sentinel channel_id — the shape
    // nRPC uses for its per-caller reply channels.
    let sentinel = ChannelName::new("lab/pfx/sentinel").unwrap();
    a.registry.insert_prefix(
        "lab/pfx/",
        ChannelConfig::new(ChannelId::new(sentinel))
            .with_token_roots(vec![a.keypair.entity_id().clone()]),
    );

    // The real, dynamically-named channel the peer asks for.
    let name = ChannelName::new("lab/pfx/instance-one").unwrap();
    let token = PermissionToken::issue(
        &a.keypair,
        b.keypair.entity_id().clone(),
        TokenScope::SUBSCRIBE,
        name.hash(), // scoped to the REAL channel, not the sentinel
        300,
        0,
    );
    b.mesh
        .subscribe_channel_with_token(a.mesh.node_id(), name.clone(), token)
        .await
        .expect("token-gated prefix subscribe must be accepted");

    // The part that used to fail: the publish path must find the
    // retained chain and actually deliver.
    let publisher = ChannelPublisher::new(
        name.clone(),
        PublishConfig {
            reliability: Reliability::FireAndForget,
            on_failure: OnFailure::BestEffort,
            max_inflight: 16,
        },
    );
    // The publisher is the channel's own root, so it needs a PUBLISH
    // grant to itself to clear its own gate.
    let self_token = PermissionToken::issue(
        &a.keypair,
        a.keypair.entity_id().clone(),
        TokenScope::PUBLISH,
        name.hash(),
        300,
        0,
    );
    a.mesh.set_publish_chain(
        &name,
        net::adapter::net::identity::TokenChain::single(self_token),
    );

    let report = a
        .mesh
        .publish(&publisher, Bytes::from_static(b"payload"))
        .await
        .expect("publish must be authorized");
    assert_eq!(
        report.delivered, 1,
        "a legitimately token-gated prefix subscriber must actually receive \
         events, not be revoked before the first delivery"
    );
}

/// M4 (2026-07-31 audit): a token-gated channel on a publisher with no
/// `TokenCache` must reject the subscribe outright.
///
/// Pre-fix the three enforcement points disagreed. Subscribe ACCEPTED
/// (it fell back to a transient empty revocation registry and verified
/// the chain normally), every publish DENIED (the token-gated branch
/// requires `Some(cache)`), and the sweep was a no-op (it returns early
/// without a cache). So the peer received an accepting Ack for a
/// subscription that could never deliver an event — and because the
/// publish-time denial revokes the AuthGuard entry WITHOUT removing the
/// roster entry, and the sweep that would evict it can never run, the
/// peer stayed rostered with no periodic recovery. On a queue-group
/// channel that is a standing denial of service: selection happens
/// before the auth filter, so the stranded peer keeps consuming that
/// group's events and they are dropped rather than delivered to a
/// working member. Installing a cache later did not repair it — the
/// guard entry was already revoked and the sweep never re-grants — so
/// clearing it took the peer going away or sending something that drops
/// its per-peer state (an explicit re-subscribe, or an `Unsubscribe`,
/// which revokes the guard entry, removes the retained chain and drops
/// the roster entry). Every one of those is peer-driven: the publisher
/// had no way to repair itself.
///
/// `set_token_cache` already documented "when unset, `require_token`
/// channels always reject". This makes that true.
#[tokio::test]
async fn token_gated_subscribe_rejected_when_no_token_cache() {
    // Publisher A deliberately has a channel registry but NO token
    // cache — the exact configuration that used to accept-then-strand.
    let a = {
        let keypair = EntityKeypair::generate();
        let mut node = MeshNode::new(keypair.clone(), test_config())
            .await
            .expect("MeshNode::new");
        let registry = Arc::new(ChannelConfigRegistry::new());
        node.set_channel_configs(registry.clone());
        Node {
            mesh: Arc::new(node),
            keypair,
            registry,
        }
    };
    let b = build_node().await;
    handshake_no_start(&a.mesh, &b.mesh).await;
    a.mesh.start();
    b.mesh.start();

    let name = ChannelName::new("lab/nocache").unwrap();
    a.registry.insert(
        ChannelConfig::new(ChannelId::new(name.clone()))
            .with_token_roots(vec![a.keypair.entity_id().clone()]),
    );

    // A perfectly good token — the rejection must come from the missing
    // cache, not from anything wrong with the credential.
    let token = PermissionToken::issue(
        &a.keypair,
        b.keypair.entity_id().clone(),
        TokenScope::SUBSCRIBE,
        name.hash(),
        300,
        0,
    );
    let result = b
        .mesh
        .subscribe_channel_with_token(a.mesh.node_id(), name.clone(), token)
        .await;

    assert!(
        result.is_err(),
        "a token-gated channel with no TokenCache must reject the subscribe \
         rather than accept one that can never deliver"
    );
    assert!(
        !a.mesh
            .roster()
            .is_subscribed(b.mesh.node_id(), &ChannelId::new(name)),
        "the rejected peer must not be left in the roster — that is the \
         stranded-subscriber state this fix exists to prevent"
    );
}

/// L1 (2026-07-31 audit): a registry-less node's permissiveness must be
/// a recorded decision, not a consequence of an unset field.
///
/// `Deny` fails closed; the default still admits (so no existing
/// embedder breaks) but now says so in the log exactly once.
#[tokio::test]
async fn unregistered_channel_policy_governs_registry_less_nodes() {
    use net::adapter::net::UnregisteredChannelPolicy;

    async fn subscribe_under(policy: UnregisteredChannelPolicy) -> bool {
        // A deliberately registry-less publisher.
        let keypair = EntityKeypair::generate();
        let cfg = test_config().with_unregistered_channel_policy(policy);
        let a = Arc::new(MeshNode::new(keypair, cfg).await.expect("MeshNode::new"));
        let b = build_node().await;
        handshake_no_start(&a, &b.mesh).await;
        a.start();
        b.mesh.start();

        let name = ChannelName::new("lab/unregistered").unwrap();
        b.mesh.subscribe_channel(a.node_id(), name).await.is_ok()
    }

    assert!(
        !subscribe_under(UnregisteredChannelPolicy::Deny).await,
        "Deny must fail closed on a node with no channel registry"
    );
    assert!(
        subscribe_under(UnregisteredChannelPolicy::Allow).await,
        "Allow is an intentionally open mesh"
    );
    assert!(
        subscribe_under(UnregisteredChannelPolicy::AllowWithWarning).await,
        "the default must preserve historical behaviour — permissive, but \
         it now warns once so a forgotten registry is observable"
    );
}

/// M2 (2026-07-31 audit): under `QueueGroupPolicy::TokenBound` a peer
/// may join only the group its grant names.
///
/// Queue-group membership is a claim on other members' work: every
/// event goes to exactly ONE member, so an attacker who joins a
/// production group takes a share of its events and, by not processing
/// them, destroys that share. Pre-fix the group name was an
/// unauthenticated string taken straight off the wire, with no config
/// axis restricting who could join what.
#[tokio::test]
async fn queue_group_join_requires_a_grant_for_that_group() {
    use net::adapter::net::{queue_group_hash, QueueGroupPolicy};

    let (a, b) = setup_pair(CapabilitySet::new(), CapabilitySet::new()).await;

    let name = ChannelName::new("work/queue").unwrap();
    a.registry.insert(
        ChannelConfig::new(ChannelId::new(name.clone()))
            .with_token_roots(vec![a.keypair.entity_id().clone()])
            .with_queue_group_policy(QueueGroupPolicy::TokenBound),
    );

    // A grant for the "batch" group specifically.
    let grant = PermissionToken::issue(
        &a.keypair,
        b.keypair.entity_id().clone(),
        TokenScope::SUBSCRIBE,
        queue_group_hash(name.as_str(), "batch"),
        300,
        0,
    );

    // Joining the granted group works.
    b.mesh
        .subscribe_channel_in_queue_group_with_token(
            a.mesh.node_id(),
            name.clone(),
            "batch".to_string(),
            grant.clone(),
        )
        .await
        .expect("the granted queue group must be joinable");

    // Joining a DIFFERENT group with the same grant must not.
    let result = b
        .mesh
        .subscribe_channel_in_queue_group_with_token(
            a.mesh.node_id(),
            name.clone(),
            "realtime".to_string(),
            grant,
        )
        .await;
    assert!(
        result.is_err(),
        "a grant for one queue group must not admit the holder to another — \
         that is the work-stealing this policy exists to stop"
    );

    // And a plain channel-scoped SUBSCRIBE token — what an ordinary
    // read-only subscriber holds — is not a worker grant.
    let reader_token = PermissionToken::issue(
        &a.keypair,
        b.keypair.entity_id().clone(),
        TokenScope::SUBSCRIBE,
        name.hash(),
        300,
        0,
    );
    let result = b
        .mesh
        .subscribe_channel_in_queue_group_with_token(
            a.mesh.node_id(),
            name,
            "batch".to_string(),
            reader_token,
        )
        .await;
    assert!(
        result.is_err(),
        "a channel-scoped subscribe token must not double as a queue-group \
         worker grant, or the policy would be a no-op"
    );
}

/// A `TokenBound` queue-group worker must work end to end — join
/// accepted AND events actually delivered.
///
/// `TokenBound` cannot be configured without `token_roots`, and roots
/// make `token_required()` true, which makes the publish path re-verify
/// every admitted subscriber's retained chain on every packet. That
/// re-check asked the chain about the CHANNEL hash, but a group grant
/// authorizes `queue_group_hash(channel, group)` — a deliberately
/// different hash, so that a reader's channel token cannot double as a
/// worker grant. The question therefore had no answer, the worker was
/// revoked and de-rostered before its first event, and the feature was
/// unusable: accepted with a successful Ack, then silently starved.
///
/// The subscribe-side test above passed throughout — it never published.
#[tokio::test]
async fn token_bound_queue_group_worker_receives_events() {
    use net::adapter::net::{queue_group_hash, QueueGroupPolicy};

    let (a, b) = setup_pair(CapabilitySet::new(), CapabilitySet::new()).await;

    let name = ChannelName::new("work/delivery").unwrap();
    a.registry.insert(
        ChannelConfig::new(ChannelId::new(name.clone()))
            .with_token_roots(vec![a.keypair.entity_id().clone()])
            .with_queue_group_policy(QueueGroupPolicy::TokenBound),
    );

    let grant = PermissionToken::issue(
        &a.keypair,
        b.keypair.entity_id().clone(),
        TokenScope::SUBSCRIBE,
        queue_group_hash(name.as_str(), "batch"),
        300,
        0,
    );
    b.mesh
        .subscribe_channel_in_queue_group_with_token(
            a.mesh.node_id(),
            name.clone(),
            "batch".to_string(),
            grant,
        )
        .await
        .expect("the granted queue group must be joinable");

    // The publisher is the channel's own root, so it needs a PUBLISH
    // grant to itself to clear its own gate.
    let self_token = PermissionToken::issue(
        &a.keypair,
        a.keypair.entity_id().clone(),
        TokenScope::PUBLISH,
        name.hash(),
        300,
        0,
    );
    a.mesh.set_publish_chain(
        &name,
        net::adapter::net::identity::TokenChain::single(self_token),
    );

    let publisher = ChannelPublisher::new(
        name.clone(),
        PublishConfig {
            reliability: Reliability::FireAndForget,
            on_failure: OnFailure::BestEffort,
            max_inflight: 16,
        },
    );
    let report = a
        .mesh
        .publish(&publisher, Bytes::from_static(b"work-item"))
        .await
        .expect("publish must be authorized");
    assert_eq!(
        report.delivered, 1,
        "a worker admitted by a group grant must actually receive the group's \
         events, not be revoked by a publish-time re-check asking its grant \
         about the channel instead of the group"
    );

    // Publish-time denial de-rosters as well as revoking, so a surviving
    // roster entry is the sharpest evidence the re-check agreed.
    assert!(
        a.mesh
            .roster()
            .is_subscribed(b.mesh.node_id(), &ChannelId::new(name.clone())),
        "the worker must still be rostered after a publish — a failed \
         re-check evicts it, which on a queue group means that group's \
         events are dropped rather than delivered"
    );

    // Second publish: the first one flips `signatures_verified`, so this
    // takes the presigned re-check path. Both paths must agree about
    // which hash the grant answers for.
    let report = a
        .mesh
        .publish(&publisher, Bytes::from_static(b"work-item-2"))
        .await
        .expect("publish must be authorized");
    assert_eq!(
        report.delivered, 1,
        "the presigned re-check path must use the same grant hash as the \
         full one — it skips signatures, not scope"
    );
}

/// `QueueGroupPolicy::Deny` must hold on a channel that has NO other
/// gate — which is the shape it is most likely to be reached for.
///
/// `authorize_subscribe` short-circuits open channels before evaluating
/// policy, and that test used to read only the cap filters and
/// `require_token`. `Deny` needs neither: it is meaningful on a plainly
/// open channel, and it is exactly there that the short-circuit returned
/// "accepted" before the policy was consulted, making the setting a
/// silent no-op.
///
/// Driven through the real subscribe path rather than
/// `can_join_queue_group` directly — the unit test on that function
/// passed throughout, because it never reached the short-circuit.
#[tokio::test]
async fn queue_group_deny_holds_on_a_channel_with_no_other_gates() {
    use net::adapter::net::QueueGroupPolicy;

    let (a, b) = setup_pair(CapabilitySet::new(), CapabilitySet::new()).await;

    // No cap filters, no token roots, no `require_token`. The policy is
    // the only thing standing between B and the group.
    let name = ChannelName::new("work/broadcast-only").unwrap();
    a.registry.insert(
        ChannelConfig::new(ChannelId::new(name.clone()))
            .with_queue_group_policy(QueueGroupPolicy::Deny),
    );

    let result = b
        .mesh
        .subscribe_channel_in_queue_group(a.mesh.node_id(), name.clone(), "workers".to_string())
        .await;
    assert!(
        result.is_err(),
        "QueueGroupPolicy::Deny must refuse a queue-group subscribe even when \
         the channel has no cap filter and no token gate — otherwise the \
         open-channel short-circuit answers before the policy is read"
    );
    assert!(
        !a.mesh
            .roster()
            .is_subscribed(b.mesh.node_id(), &ChannelId::new(name.clone())),
        "a refused queue-group join must not be left in the roster, or it \
         keeps consuming that group's selections"
    );

    // The policy restricts group membership, not the channel: a
    // broadcast subscriber is unaffected and keeps the cheap path.
    b.mesh
        .subscribe_channel(a.mesh.node_id(), name)
        .await
        .expect("Deny governs queue groups only — broadcast must still work");
}

/// The same bypass, reached the other way: a peer whose entity the
/// publisher has not pinned.
///
/// Widening the open-channel short-circuit is not enough on its own. On
/// a channel with no `require_token`, an unpinned peer takes a separate
/// early return that runs the capability match against a dummy identity
/// — a path that answers "do your caps fit?" and never asks the
/// queue-group question at all. `Deny` admits nobody and `TokenBound`
/// needs a chain bound to a real AEAD-verified entity, so an unpinned
/// peer must be refused on both.
#[tokio::test]
async fn queue_group_deny_holds_for_an_unpinned_peer() {
    use net::adapter::net::QueueGroupPolicy;

    // Deliberately NOT `setup_pair`: no announcement, so A never pins
    // B's entity.
    let a = build_node().await;
    let b = build_node().await;
    handshake_no_start(&a.mesh, &b.mesh).await;
    a.mesh.start();
    b.mesh.start();

    let name = ChannelName::new("work/unpinned").unwrap();
    a.registry.insert(
        ChannelConfig::new(ChannelId::new(name.clone()))
            .with_queue_group_policy(QueueGroupPolicy::Deny),
    );
    assert!(
        a.mesh.peer_entity_id(b.mesh.node_id()).is_none(),
        "harness precondition: B must be unpinned on A"
    );

    let result = b
        .mesh
        .subscribe_channel_in_queue_group(a.mesh.node_id(), name.clone(), "workers".to_string())
        .await;
    assert!(
        result.is_err(),
        "an unpinned peer must not slip past a restricted queue-group policy \
         via the cap-only early return"
    );
    assert!(
        !a.mesh
            .roster()
            .is_subscribed(b.mesh.node_id(), &ChannelId::new(name)),
        "a refused queue-group join must not be left in the roster"
    );
}

#[tokio::test]
async fn tampered_announcement_signature_rejected() {
    use net::adapter::net::behavior::capability::CapabilityAnnouncement;

    // Direct regression for the CapabilityAnnouncement sign/verify
    // round-trip (E-1). No mesh, pure data-structure test.
    let kp = EntityKeypair::generate();
    let mut ann = CapabilityAnnouncement::new(
        kp.node_id(),
        kp.entity_id().clone(),
        1,
        CapabilitySet::new().add_tag("ok"),
    );
    ann.sign(&kp);
    assert!(ann.verify().is_ok(), "fresh signature must verify");

    // Tamper: flip a byte in the capability set (fields are
    // JSON-serialized inside the signed region, so a tag swap
    // invalidates the signature).
    let mut tampered = ann.clone();
    tampered.capabilities = CapabilitySet::new().add_tag("tampered");
    assert!(
        tampered.verify().is_err(),
        "tampered announcement must fail verification"
    );
}
