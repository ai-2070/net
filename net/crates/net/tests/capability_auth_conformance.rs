//! v0.4 capability-auth conformance test — Phase 3 of
//! `docs/plans/CAPABILITY_AUTH_PLAN.md`. The plan's §7 lists six
//! scenarios the gate must satisfy; each test below pins one of
//! them against real `MeshNode` instances so a regression that
//! shifts the spec gets caught at the integration boundary.
//!
//! Scenarios:
//! 1. Permissive baseline      — empty allow-lists admit any caller
//! 2. Allow-by-node            — `[B]` admits B, denies C
//! 3. Allow-by-subnet          — `[S]` admits subnet members, denies non-members
//! 4. Allow-by-group           — `[G]` admits group claimants, denies non-claimants
//! 5. Revocation               — new announcement supersedes the old
//! 6. Receiver-side defense    — callee independently rejects with `CapabilityDenied`

#![cfg(all(feature = "net", feature = "cortex"))]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use net::adapter::net::behavior::{
    group::GroupId, subnet::SubnetId, CapabilityAnnouncement, CapabilitySet,
};
use net::adapter::net::cortex::{
    RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus,
};
use net::adapter::net::mesh_rpc::{CallOptions, RpcError};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};

const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x42u8; 32];

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
    handshake_only(a, b).await;
    a.start();
    b.start();
}

/// Handshake without starting receive loops — multi-peer
/// scenarios run every handshake first then call `start()` once
/// on each node, mirroring the `three_node_star` pattern in
/// `tests/nat_classify.rs`. A node that's already started
/// races inbound accepts on subsequent handshakes and the
/// second one times out.
async fn handshake_only(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
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

/// Star topology: every peer connects to `center`. All handshakes
/// happen before any node starts, so the second handshake's
/// accept doesn't race a running receive loop on either party.
async fn star(center: &Arc<MeshNode>, peers: &[&Arc<MeshNode>]) {
    for p in peers {
        handshake_only(p, center).await;
    }
    center.start();
    for p in peers {
        p.start();
    }
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

/// Fold a hand-built announcement into both nodes' capability
/// indexes so the gate (which reads from the local index)
/// observes the intended policy without waiting for broadcast.
/// Tests build their own announcement to set allow-lists +
/// subnet/group tags that the routine
/// `MeshNode::announce_capabilities` path doesn't expose
/// directly.
fn fold_announcement_everywhere(nodes: &[&Arc<MeshNode>], ann: &CapabilityAnnouncement) {
    for n in nodes {
        n.capability_index_arc().index(ann.clone());
    }
}

/// Build an unsigned target announcement that lists the
/// requested capability tag plus the supplied allow-lists.
/// Unsigned is intentional — the in-process `index()` path
/// doesn't verify signatures (verification is the wire-side
/// `handle_capability_announcement` job); tests that exercise
/// the gate sidestep broadcast and fold directly.
fn target_announcement(
    target: &Arc<MeshNode>,
    version: u64,
    capability_tag: &str,
    allowed_nodes: Vec<u64>,
    allowed_subnets: Vec<SubnetId>,
    allowed_groups: Vec<GroupId>,
) -> CapabilityAnnouncement {
    let caps = CapabilitySet::new().add_tag(capability_tag);
    let mut ann = CapabilityAnnouncement::new(
        target.node_id(),
        target.entity_id().clone(),
        version,
        caps,
    );
    ann.allowed_nodes = allowed_nodes;
    ann.allowed_subnets = allowed_subnets;
    ann.allowed_groups = allowed_groups;
    ann
}

/// Build a caller announcement that self-declares a subnet
/// and/or group membership via tags. Used by scenarios 3 / 4
/// where the target's allow-list keys on caller membership.
fn caller_announcement(
    caller: &Arc<MeshNode>,
    version: u64,
    membership_subnet: Option<SubnetId>,
    membership_groups: &[GroupId],
) -> CapabilityAnnouncement {
    let mut caps = CapabilitySet::new();
    if let Some(s) = membership_subnet {
        caps = caps.add_tag(&s.to_tag());
    }
    for g in membership_groups {
        caps = caps.add_tag(&g.to_tag());
    }
    CapabilityAnnouncement::new(
        caller.node_id(),
        caller.entity_id().clone(),
        version,
        caps,
    )
}

// ---------------------------------------------------------------------------
// Scenario 1 — Permissive baseline
// ---------------------------------------------------------------------------

/// A publishes an announcement with all three allow-lists empty;
/// B can execute. Pins that the gate's permissive default
/// (`CAPABILITY_AUTH_PLAN.md` §3 step 3) is observable from end
/// to end on the call_service path.
#[tokio::test]
async fn scenario_1_permissive_baseline_admits_any_caller() {
    let target = build_node().await;
    let caller = build_node().await;
    handshake_pair(&caller, &target).await;

    let _serve = target
        .serve_rpc("echo", Arc::new(EchoHandler))
        .expect("serve_rpc");
    // Announce after serve_rpc so the nrpc tag is merged.
    target
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("target announce");

    // Wait for the caller's index to fold the target's nrpc tag.
    use net::adapter::net::behavior::CapabilityFilter;
    let filter = CapabilityFilter::default().require_tag("nrpc:echo".to_string());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        if caller
            .capability_index_arc()
            .query(&filter)
            .contains(&target.node_id())
        {
            break;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("propagation timeout");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let reply = caller
        .call_service(
            "echo",
            Bytes::from_static(b"permissive"),
            CallOptions::default(),
        )
        .await
        .expect("permissive default must admit any caller");
    assert_eq!(reply.body.as_ref(), b"permissive");
}

// ---------------------------------------------------------------------------
// Scenario 2 — Allow-by-node
// ---------------------------------------------------------------------------

/// A allows `[B]`; B can execute, C cannot. Pins the
/// `allowed_nodes` axis as observed by the caller-side gate
/// inside `call_service`.
#[tokio::test]
async fn scenario_2_allow_by_node_admits_listed_only() {
    let target = build_node().await;
    let allowed_caller = build_node().await;
    let denied_caller = build_node().await;
    star(&target, &[&allowed_caller, &denied_caller]).await;

    // Build a target announcement restricting to `allowed_caller`.
    let ann = target_announcement(
        &target,
        100,
        "nrpc:echo",
        vec![allowed_caller.node_id()],
        vec![],
        vec![],
    );
    fold_announcement_everywhere(&[&target, &allowed_caller, &denied_caller], &ann);

    // Allowed caller succeeds. The target hasn't called
    // serve_rpc, so the call surfaces a ServerError — but the
    // gate said yes, which is what this scenario pins.
    // Distinguish the gate verdict from the missing-handler
    // verdict by asserting the error variant.
    let err = allowed_caller
        .call_service(
            "echo",
            Bytes::from_static(b"hi"),
            CallOptions {
                deadline: Some(Instant::now() + Duration::from_millis(500)),
                ..Default::default()
            },
        )
        .await
        .expect_err("no handler is registered; expect non-Ok");
    assert!(
        !matches!(err, RpcError::CapabilityDenied { .. }),
        "allowed caller must NOT see CapabilityDenied; got {err:?}",
    );

    // Denied caller hits the gate first.
    let err = denied_caller
        .call_service(
            "echo",
            Bytes::from_static(b"hi"),
            CallOptions::default(),
        )
        .await
        .expect_err("denied caller must hit the gate");
    match err {
        RpcError::CapabilityDenied { target: t, capability } => {
            assert_eq!(t, target.node_id());
            assert_eq!(capability, "echo");
        }
        other => panic!("expected CapabilityDenied, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Scenario 3 — Allow-by-subnet
// ---------------------------------------------------------------------------

/// A allows `[subnet S]`; nodes in S can execute, nodes outside
/// cannot. Membership is self-declared via a `subnet:<hex32>`
/// tag on the caller's own announcement (signed + TOFU-bound in
/// production; folded directly here to sidestep broadcast).
#[tokio::test]
async fn scenario_3_allow_by_subnet_admits_subnet_members() {
    let target = build_node().await;
    let in_subnet = build_node().await;
    let out_of_subnet = build_node().await;
    star(&target, &[&in_subnet, &out_of_subnet]).await;

    let subnet = SubnetId([0x42; 16]);
    let target_ann = target_announcement(
        &target,
        200,
        "nrpc:echo",
        vec![],
        vec![subnet],
        vec![],
    );
    let in_subnet_ann = caller_announcement(&in_subnet, 1, Some(subnet), &[]);
    let out_of_subnet_ann = caller_announcement(&out_of_subnet, 1, None, &[]);
    fold_announcement_everywhere(
        &[&target, &in_subnet, &out_of_subnet],
        &target_ann,
    );
    // Each caller's own subnet membership announcement also needs
    // to land in the target's index (the gate reads caller subnet
    // there) AND in the caller's own index (consistency).
    fold_announcement_everywhere(&[&target, &in_subnet], &in_subnet_ann);
    fold_announcement_everywhere(&[&target, &out_of_subnet], &out_of_subnet_ann);

    let err = in_subnet
        .call_service(
            "echo",
            Bytes::from_static(b"in-subnet"),
            CallOptions {
                deadline: Some(Instant::now() + Duration::from_millis(500)),
                ..Default::default()
            },
        )
        .await
        .expect_err("no handler registered; expect non-Ok");
    assert!(
        !matches!(err, RpcError::CapabilityDenied { .. }),
        "subnet member must NOT see CapabilityDenied; got {err:?}",
    );

    let err = out_of_subnet
        .call_service(
            "echo",
            Bytes::from_static(b"out-of-subnet"),
            CallOptions::default(),
        )
        .await
        .expect_err("non-member must hit the gate");
    assert!(matches!(err, RpcError::CapabilityDenied { .. }));
}

// ---------------------------------------------------------------------------
// Scenario 4 — Allow-by-group
// ---------------------------------------------------------------------------

/// A allows `[group G]`; nodes claiming `G` via tag can
/// execute, others cannot.
#[tokio::test]
async fn scenario_4_allow_by_group_admits_group_claimants() {
    let target = build_node().await;
    let claimant = build_node().await;
    let non_claimant = build_node().await;
    star(&target, &[&claimant, &non_claimant]).await;

    let group = GroupId([0x77; 32]);
    let target_ann =
        target_announcement(&target, 300, "nrpc:echo", vec![], vec![], vec![group]);
    let claimant_ann = caller_announcement(&claimant, 1, None, &[group]);
    let non_claimant_ann = caller_announcement(&non_claimant, 1, None, &[]);
    fold_announcement_everywhere(&[&target, &claimant, &non_claimant], &target_ann);
    fold_announcement_everywhere(&[&target, &claimant], &claimant_ann);
    fold_announcement_everywhere(&[&target, &non_claimant], &non_claimant_ann);

    let err = claimant
        .call_service(
            "echo",
            Bytes::from_static(b"group-member"),
            CallOptions {
                deadline: Some(Instant::now() + Duration::from_millis(500)),
                ..Default::default()
            },
        )
        .await
        .expect_err("no handler registered; expect non-Ok");
    assert!(
        !matches!(err, RpcError::CapabilityDenied { .. }),
        "group claimant must NOT see CapabilityDenied; got {err:?}",
    );

    let err = non_claimant
        .call_service(
            "echo",
            Bytes::from_static(b"non-claimant"),
            CallOptions::default(),
        )
        .await
        .expect_err("non-claimant must hit the gate");
    assert!(matches!(err, RpcError::CapabilityDenied { .. }));
}

// ---------------------------------------------------------------------------
// Scenario 5 — Revocation
// ---------------------------------------------------------------------------

/// A publishes v1 permissive, then v2 with
/// `allowed_nodes = [self]`; B's execute fails after v2 is
/// folded. Revocation IS a new announcement — there is no
/// separate `revoke` verb (`CAPABILITY_AUTH_PLAN.md` §"Locked
/// design points" #3).
#[tokio::test]
async fn scenario_5_revocation_via_new_announcement_supersedes() {
    let target = build_node().await;
    let caller = build_node().await;
    handshake_pair(&caller, &target).await;

    // v1: permissive.
    let v1 = target_announcement(&target, 1, "nrpc:echo", vec![], vec![], vec![]);
    fold_announcement_everywhere(&[&target, &caller], &v1);

    let err = caller
        .call_service(
            "echo",
            Bytes::from_static(b"v1"),
            CallOptions {
                deadline: Some(Instant::now() + Duration::from_millis(500)),
                ..Default::default()
            },
        )
        .await
        .expect_err("no handler registered");
    assert!(
        !matches!(err, RpcError::CapabilityDenied { .. }),
        "v1 must admit; got {err:?}",
    );

    // v2: locked to self only — caller is excluded.
    let v2 = target_announcement(
        &target,
        2,
        "nrpc:echo",
        vec![target.node_id()],
        vec![],
        vec![],
    );
    fold_announcement_everywhere(&[&target, &caller], &v2);

    let err = caller
        .call_service(
            "echo",
            Bytes::from_static(b"v2"),
            CallOptions::default(),
        )
        .await
        .expect_err("v2 must deny");
    assert!(matches!(err, RpcError::CapabilityDenied { .. }));
}

// ---------------------------------------------------------------------------
// Scenario 6 — Receiver-side defense
// ---------------------------------------------------------------------------

/// Caller bypasses the local gate (uses direct `call`, which
/// the caller-side gate inside `call_service` does NOT cover);
/// callee independently rejects with `CapabilityDenied`. Pins
/// the defense-in-depth wiring inside `serve_rpc`'s bridge.
///
/// The caller's local index has the target announcement marked
/// permissive (so a hypothetical caller-side gate would admit);
/// the target's OWN index has the restrictive announcement, so
/// the bridge gate denies on receipt.
#[tokio::test]
async fn scenario_6_callee_side_defense_in_depth_rejects() {
    let target = build_node().await;
    let caller = build_node().await;
    handshake_pair(&caller, &target).await;

    let _serve = target
        .serve_rpc("echo", Arc::new(EchoHandler))
        .expect("serve_rpc");

    // Caller's index: permissive view of the target (forged for
    // the purposes of the test — in production both sides would
    // hold the same signed announcement, but the test simulates a
    // caller that skipped or never received the restrictive
    // version).
    let permissive =
        target_announcement(&target, 1, "nrpc:echo", vec![], vec![], vec![]);
    caller.capability_index_arc().index(permissive);

    // Target's own index: restrictive — only a synthetic node id
    // distinct from the caller is admitted.
    let restrictive = target_announcement(
        &target,
        2,
        "nrpc:echo",
        vec![0xDEAD_BEEF_BAAD_F00D],
        vec![],
        vec![],
    );
    target.capability_index_arc().index(restrictive);

    // Use direct `call` so the caller-side `call_service` gate
    // does not fire. The callee-side bridge gate must catch the
    // bypass.
    let err = caller
        .call(
            target.node_id(),
            "echo",
            Bytes::from_static(b"bypass"),
            CallOptions::default(),
        )
        .await
        .expect_err("callee-side gate must deny");
    match err {
        RpcError::CapabilityDenied { target: t, capability } => {
            assert_eq!(t, target.node_id());
            assert_eq!(capability, "echo");
        }
        other => panic!(
            "expected CapabilityDenied surfaced from callee, got {other:?}"
        ),
    }
}

// ---------------------------------------------------------------------------
// Sanity: the helper functions used above behave as documented.
// ---------------------------------------------------------------------------

/// Pin that `fold_announcement_everywhere` actually folds the
/// announcement into each node's index — a misnamed accessor
/// could silently turn every conformance test into a tautology.
#[tokio::test]
async fn helper_fold_announcement_lands_in_every_index() {
    let a = build_node().await;
    let b = build_node().await;
    let ann = target_announcement(&a, 1, "nrpc:probe", vec![], vec![], vec![]);
    fold_announcement_everywhere(&[&a, &b], &ann);
    assert!(a.capability_index_arc().get(a.node_id()).is_some());
    assert!(b.capability_index_arc().get(a.node_id()).is_some());
}
