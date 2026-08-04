//! S1 of `docs/internal/plans/SUBNET_AUTH_PLAN.md` — self-declared
//! `subnet:` / `group:` membership axes are demoted from admission to
//! routing.
//!
//! The pre-S1 callee-side gate admitted an RPC when the caller's
//! *self-declared* subnet/group tag matched the provider's
//! `allowed_subnets` / `allowed_groups` — values the provider itself
//! publishes in a cleartext broadcast (audit: the "secret" is
//! disclosed by the artifact it protects). Post-S1:
//!
//! - callee-side admission is `allowed_nodes` or nothing (permissive
//!   default when every list is empty stays);
//! - caller-side candidate *narrowing* on those axes is kept — it
//!   admits nothing;
//! - multiple membership tags collapse deterministically (0 or >1
//!   distinct subnet tags → no membership), replacing the wire-order
//!   last-wins divergence.
//!
//! Witness 4 of the plan ("protected org/provider admission remains
//! unchanged") is pinned by the existing org-admission suites, which
//! run unmodified in the S1 gate — the protected path resolves
//! admission via `has_local_capability` + `verify_org_admission` and
//! never consulted the demoted axes.

#![cfg(all(feature = "net", feature = "cortex"))]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use net::adapter::net::behavior::fold::capability_bridge;
use net::adapter::net::behavior::{
    group::GroupId, subnet::SubnetId, CapabilityAnnouncement, CapabilitySet,
};
use net::adapter::net::cortex::{
    RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus,
};
use net::adapter::net::mesh_rpc::{CallOptions, RpcError};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};

const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x51u8; 32];

fn test_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(3, Duration::from_secs(2))
        .with_capability_gc_interval(Duration::from_millis(250))
        .with_min_announce_interval(Duration::from_millis(10));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: TEST_BUFFER_SIZE,
        recv_buffer_size: TEST_BUFFER_SIZE,
    };
    cfg
}

async fn build_node() -> Arc<MeshNode> {
    let cfg = test_config();
    let keypair = EntityKeypair::generate();
    Arc::new(MeshNode::new(keypair, cfg).await.expect("MeshNode::new"))
}

async fn handshake_pair(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    let a_id = a.node_id();
    let b_pub = *b.public_key();
    let b_addr = b.local_addr();
    let b_clone = b.clone();
    let accept = tokio::spawn(async move { b_clone.accept(a_id).await });
    a.connect(b_addr, &b_pub, b.node_id())
        .await
        .expect("connect failed");
    accept
        .await
        .expect("accept task panicked")
        .expect("accept failed");
    a.start();
    b.start();
}

struct EchoHandler;

#[async_trait::async_trait]
impl RpcHandler for EchoHandler {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: ctx.payload.body,
        })
    }
}

/// Target announcement with explicit allow-lists. `version >= 100` to
/// supersede `serve_rpc`'s auto-self-index (v=1) and auto-re-announce
/// (v=2) — the fold rejects `version <= current`.
fn target_announcement(
    target: &Arc<MeshNode>,
    version: u64,
    capability_tag: &str,
    allowed_nodes: Vec<u64>,
    allowed_subnets: Vec<SubnetId>,
    allowed_groups: Vec<GroupId>,
) -> CapabilityAnnouncement {
    let caps = CapabilitySet::new().add_tag(capability_tag);
    let mut ann =
        CapabilityAnnouncement::new(target.node_id(), target.entity_id().clone(), version, caps);
    ann.allowed_nodes = allowed_nodes;
    ann.allowed_subnets = allowed_subnets;
    ann.allowed_groups = allowed_groups;
    ann
}

/// Caller announcement self-declaring membership tags, in the exact
/// order given (order must not matter post-S1).
fn caller_announcement_with_tags(
    caller: &Arc<MeshNode>,
    version: u64,
    tags: &[String],
) -> CapabilityAnnouncement {
    let mut caps = CapabilitySet::new();
    for t in tags {
        caps = caps.add_tag(t.clone());
    }
    CapabilityAnnouncement::new(caller.node_id(), caller.entity_id().clone(), version, caps)
}

fn deadline_opts() -> CallOptions {
    CallOptions {
        deadline: Some(Instant::now() + Duration::from_millis(1500)),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Witness 1 — a matching self-declared `subnet:` tag alone does not admit
// ---------------------------------------------------------------------------

/// The caller self-declares exactly the subnet the target's
/// `allowed_subnets` names, and the target's own index holds both
/// announcements. Direct `call` (bypassing caller-side narrowing) so
/// the callee-side gate is what answers. Pre-S1 this round-trips;
/// post-S1 the callee denies.
#[tokio::test]
async fn subnet_axis_alone_does_not_admit() {
    let target = build_node().await;
    let caller = build_node().await;
    handshake_pair(&caller, &target).await;

    let _serve = target
        .serve_rpc("echo", Arc::new(EchoHandler))
        .expect("serve_rpc");

    let subnet = SubnetId::from_bytes([0x42; 16]);
    let target_ann = target_announcement(&target, 100, "nrpc:echo", vec![], vec![subnet], vec![]);
    let caller_ann = caller_announcement_with_tags(&caller, 1, &[subnet.to_tag()]);
    target.test_inject_capability_announcement(target_ann.clone());
    caller.test_inject_capability_announcement(target_ann);
    target.test_inject_capability_announcement(caller_ann.clone());
    caller.test_inject_capability_announcement(caller_ann);

    let err = caller
        .call(
            target.node_id(),
            "echo",
            Bytes::from_static(b"subnet-claimant"),
            deadline_opts(),
        )
        .await
        .expect_err("self-declared subnet membership must not admit (S1 demotion)");
    assert!(
        matches!(err, RpcError::CapabilityDenied { .. }),
        "expected CapabilityDenied from the callee-side gate, got {err:?}",
    );
}

// ---------------------------------------------------------------------------
// Witness 2 — a matching self-declared `group:` tag alone does not admit
// ---------------------------------------------------------------------------

#[tokio::test]
async fn group_axis_alone_does_not_admit() {
    let target = build_node().await;
    let caller = build_node().await;
    handshake_pair(&caller, &target).await;

    let _serve = target
        .serve_rpc("echo", Arc::new(EchoHandler))
        .expect("serve_rpc");

    let group = GroupId::from_bytes([0x77; 32]);
    let target_ann = target_announcement(&target, 100, "nrpc:echo", vec![], vec![], vec![group]);
    let caller_ann = caller_announcement_with_tags(&caller, 1, &[group.to_tag()]);
    target.test_inject_capability_announcement(target_ann.clone());
    caller.test_inject_capability_announcement(target_ann);
    target.test_inject_capability_announcement(caller_ann.clone());
    caller.test_inject_capability_announcement(caller_ann);

    let err = caller
        .call(
            target.node_id(),
            "echo",
            Bytes::from_static(b"group-claimant"),
            deadline_opts(),
        )
        .await
        .expect_err("self-declared group membership must not admit (S1 demotion)");
    assert!(
        matches!(err, RpcError::CapabilityDenied { .. }),
        "expected CapabilityDenied from the callee-side gate, got {err:?}",
    );
}

// ---------------------------------------------------------------------------
// Witness 3 — `allowed_nodes` remains load-bearing
// ---------------------------------------------------------------------------

/// The node axis is blake2s-bound to the announcing entity at
/// dispatch and stays the one admitting allow-list: the listed caller
/// round-trips, an unlisted one is denied.
#[tokio::test]
async fn allowed_nodes_remains_load_bearing() {
    let target = build_node().await;
    let listed = build_node().await;
    handshake_pair(&listed, &target).await;

    let _serve = target
        .serve_rpc("echo", Arc::new(EchoHandler))
        .expect("serve_rpc");

    let target_ann = target_announcement(
        &target,
        100,
        "nrpc:echo",
        vec![listed.node_id()],
        vec![],
        vec![],
    );
    target.test_inject_capability_announcement(target_ann.clone());
    listed.test_inject_capability_announcement(target_ann);

    let reply = listed
        .call(
            target.node_id(),
            "echo",
            Bytes::from_static(b"listed"),
            deadline_opts(),
        )
        .await
        .expect("allowed_nodes member must still round-trip");
    assert_eq!(reply.body.as_ref(), b"listed");
}

// ---------------------------------------------------------------------------
// Witness 3b — permissive default unchanged
// ---------------------------------------------------------------------------

/// All three allow-lists empty still admits any handshaken caller —
/// the S1 demotion narrows nothing for unrestricted services.
#[tokio::test]
async fn permissive_default_unchanged() {
    let target = build_node().await;
    let caller = build_node().await;
    handshake_pair(&caller, &target).await;

    let _serve = target
        .serve_rpc("echo", Arc::new(EchoHandler))
        .expect("serve_rpc");

    let reply = caller
        .call(
            target.node_id(),
            "echo",
            Bytes::from_static(b"open"),
            deadline_opts(),
        )
        .await
        .expect("empty allow-lists must stay permissive");
    assert_eq!(reply.body.as_ref(), b"open");
}

// ---------------------------------------------------------------------------
// Witness 5 — caller-side candidate filtering still narrows
// ---------------------------------------------------------------------------

/// `may_execute_batch` is the caller-side `candidates.retain(...)`
/// predicate. It keeps using the self-declared axes to *narrow* —
/// skip providers whose allow-lists we can't match — which admits
/// nothing. A matching caller keeps the candidate; a non-matching
/// caller drops it.
#[tokio::test]
async fn caller_side_filtering_still_narrows() {
    let matching = build_node().await;
    let non_matching = build_node().await;
    let target = build_node().await;

    let subnet = SubnetId::from_bytes([0x42; 16]);
    let target_ann = target_announcement(&target, 100, "nrpc:echo", vec![], vec![subnet], vec![]);
    let matching_ann = caller_announcement_with_tags(&matching, 1, &[subnet.to_tag()]);

    for n in [&matching, &non_matching] {
        n.test_inject_capability_announcement(target_ann.clone());
    }
    matching.test_inject_capability_announcement(matching_ann);

    let kept = capability_bridge::may_execute_batch(
        matching.capability_fold(),
        &[target.node_id()],
        "nrpc:echo",
        matching.node_id(),
    );
    assert_eq!(
        kept,
        vec![true],
        "a caller matching the provider's subnet axis keeps the candidate (routing)",
    );

    let dropped = capability_bridge::may_execute_batch(
        non_matching.capability_fold(),
        &[target.node_id()],
        "nrpc:echo",
        non_matching.node_id(),
    );
    assert_eq!(
        dropped,
        vec![false],
        "a caller with no matching membership drops the candidate (routing)",
    );
}

// ---------------------------------------------------------------------------
// Witness 6 — multiple membership tags collapse deterministically
// ---------------------------------------------------------------------------

/// Two distinct `subnet:` tags on one caller are out-of-model: the
/// verdict must be "no subnet membership" regardless of wire/insert
/// order. Pre-S1 the live derive was last-wins over unspecified
/// `HashSet` order — with BOTH declared subnets in the provider's
/// allow-list, last-wins admitted either way while the deterministic
/// collapse yields no membership, so this pins the semantic change
/// deterministically (not merely the order-independence).
#[tokio::test]
async fn multiple_membership_tags_collapse_deterministically() {
    let s1 = SubnetId::from_bytes([0xAA; 16]);
    let s2 = SubnetId::from_bytes([0xBB; 16]);

    for order in [[s1, s2], [s2, s1]] {
        let caller = build_node().await;
        let target = build_node().await;
        let target_ann =
            target_announcement(&target, 100, "nrpc:echo", vec![], vec![s1, s2], vec![]);
        let caller_ann =
            caller_announcement_with_tags(&caller, 1, &[order[0].to_tag(), order[1].to_tag()]);
        caller.test_inject_capability_announcement(target_ann);
        caller.test_inject_capability_announcement(caller_ann);

        let verdict = capability_bridge::may_execute_batch(
            caller.capability_fold(),
            &[target.node_id()],
            "nrpc:echo",
            caller.node_id(),
        );
        assert_eq!(
            verdict,
            vec![false],
            "two distinct subnet tags must collapse to no membership \
             (declared order {:?} first)",
            order[0],
        );
    }

    // Single tag still narrows as expected.
    let caller = build_node().await;
    let target = build_node().await;
    let target_ann = target_announcement(&target, 100, "nrpc:echo", vec![], vec![s1], vec![]);
    let caller_ann = caller_announcement_with_tags(&caller, 1, &[s1.to_tag()]);
    caller.test_inject_capability_announcement(target_ann);
    caller.test_inject_capability_announcement(caller_ann);
    let verdict = capability_bridge::may_execute_batch(
        caller.capability_fold(),
        &[target.node_id()],
        "nrpc:echo",
        caller.node_id(),
    );
    assert_eq!(verdict, vec![true], "single subnet tag still narrows");
}
