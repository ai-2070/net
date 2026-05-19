//! Integration tests for the v0.4 capability-auth execute-gate
//! along the `call_service` path. Phase 2 of
//! `docs/plans/CAPABILITY_AUTH_PLAN.md` — the unit tests in
//! `behavior::capability::tests` exercise [`CapabilityIndex::may_execute`]
//! in isolation; these tests exercise it across the wire on real
//! `MeshNode` handshakes so the caller-side gate (inside
//! [`MeshNode::call_service`]) and the callee-side defense-in-depth
//! gate (inside `serve_rpc`'s bridge) both fire end-to-end.
//!
//! Phase 3 will land the full 6-scenario conformance file at
//! `tests/capability_auth_conformance.rs`. The two tests here
//! prove the Phase 2 wiring is sound.

#![cfg(all(feature = "net", feature = "cortex"))]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use net::adapter::net::behavior::CapabilitySet;
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

/// Wait until A's index has folded B's latest announcement carrying
/// `nrpc:<service>`. Returns early on success; panics on timeout so
/// the test surfaces the propagation failure rather than racing into
/// a misleading deny.
async fn wait_for_service_visibility(node: &Arc<MeshNode>, target: u64, service: &str) {
    use net::adapter::net::behavior::CapabilityFilter;
    let tag = format!("nrpc:{service}");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let filter = CapabilityFilter::default().require_tag(tag.clone());
    while tokio::time::Instant::now() < deadline {
        let nodes = node.capability_index_arc().query(&filter);
        if nodes.contains(&target) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "timed out waiting for target {target:#x} to advertise `{tag}` in caller's capability index",
    );
}

/// Baseline: a server that announces a service with empty allow-lists
/// (the permissive default) admits any caller. This pins that the new
/// gate doesn't break the happy path.
#[tokio::test]
async fn call_service_permissive_announcement_admits_any_caller() {
    let server = build_node().await;
    let caller = build_node().await;
    handshake_pair(&caller, &server).await;

    let _serve = server
        .serve_rpc("echo", Arc::new(EchoHandler))
        .expect("serve_rpc");
    // Announce after serve_rpc so the nrpc tag is merged into the
    // outgoing CapabilitySet — required by the gate semantics
    // (CAPABILITY_AUTH_PLAN.md §3 step 2).
    server
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("server announce");
    caller
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("caller announce");

    wait_for_service_visibility(&caller, server.node_id(), "echo").await;

    let reply = caller
        .call_service(
            "echo",
            Bytes::from_static(b"permissive hello"),
            CallOptions::default(),
        )
        .await
        .expect("permissive default must admit any caller");
    assert_eq!(reply.body.as_ref(), b"permissive hello");
}

/// M3 regression — when multiple peers advertise the same
/// service and only some authorize the caller, `call_service`
/// must filter out the denying peers BEFORE the routing policy
/// picks one. Pre-fix the policy could pick a denying peer even
/// when authorizing peers existed, masking the fact that the
/// call would have succeeded against B.
#[tokio::test]
async fn call_service_filters_unauthorized_candidates_before_target_selection() {
    use net::adapter::net::behavior::CapabilityAnnouncement;

    let caller = build_node().await;
    let denying_server = build_node().await;
    let allowing_server = build_node().await;

    // Build two server-side announcements, both advertising
    // nrpc:echo. denying_server restricts to a synthetic third-
    // party id; allowing_server is permissive. Fold both into
    // the caller's local index — `find_service_nodes` then
    // returns both as candidates.
    for (server, allow) in [
        (&denying_server, vec![0xDEAD_BEEF_BAAD_F00D]),
        (&allowing_server, vec![]),
    ] {
        let caps = CapabilitySet::new().add_tag("nrpc:echo");
        let mut ann = CapabilityAnnouncement::new(
            server.node_id(),
            server.entity_id().clone(),
            100,
            caps,
        );
        ann.allowed_nodes = allow;
        caller.capability_index_arc().index(ann);
    }

    // No RPC handlers are registered on either server; the gate
    // verdict (filter result) is what we're pinning, not end-to-
    // end delivery. With the filter in place, call_service must
    // pick `allowing_server` and attempt the call — the failure
    // mode is then ANYTHING other than `CapabilityDenied` (the
    // call will time out or fail at a later step because no
    // handler is registered + no wire session is open).
    let err = caller
        .call_service(
            "echo",
            Bytes::from_static(b"x"),
            CallOptions {
                deadline: Some(
                    std::time::Instant::now() + Duration::from_millis(500),
                ),
                ..Default::default()
            },
        )
        .await
        .expect_err("no handler registered → call must error somehow");
    assert!(
        !matches!(err, RpcError::CapabilityDenied { .. }),
        "filter must steer call_service to allowing_server; \
         instead got CapabilityDenied which means the denying \
         candidate was picked. err={err:?}",
    );
}

/// Companion to the filter test: when EVERY candidate denies
/// the caller, `call_service` must surface `CapabilityDenied`
/// (rather than NoRoute, which would conflate "no peer
/// advertised the service" with "every peer that did advertise
/// it refused the caller").
#[tokio::test]
async fn call_service_denies_when_every_candidate_rejects_caller() {
    use net::adapter::net::behavior::CapabilityAnnouncement;

    let caller = build_node().await;
    let server_a = build_node().await;
    let server_b = build_node().await;

    for server in [&server_a, &server_b] {
        let caps = CapabilitySet::new().add_tag("nrpc:echo");
        let mut ann = CapabilityAnnouncement::new(
            server.node_id(),
            server.entity_id().clone(),
            100,
            caps,
        );
        ann.allowed_nodes = vec![0xDEAD_BEEF_BAAD_F00D];
        caller.capability_index_arc().index(ann);
    }

    let err = caller
        .call_service(
            "echo",
            Bytes::from_static(b"x"),
            CallOptions::default(),
        )
        .await
        .expect_err("every candidate denies → CapabilityDenied");
    match err {
        RpcError::CapabilityDenied { capability, .. } => {
            assert_eq!(capability, "echo");
        }
        other => panic!("expected CapabilityDenied, got {other:?}"),
    }
}

/// H1 regression — `serve_rpc` must auto-self-index a fresh
/// announcement carrying the new `nrpc:<service>` tag so the
/// callee-side gate has a real policy to consult from the very
/// first inbound event. Pre-fix the bridge skipped permissively
/// when no self-announcement existed, leaving servers that
/// `serve_rpc` without ever calling `announce_capabilities` open
/// to any caller.
#[tokio::test]
async fn serve_rpc_self_indexes_announcement_with_nrpc_tag() {
    let node = build_node().await;
    assert!(
        node.capability_index_arc().get(node.node_id()).is_none(),
        "no self-announcement before serve_rpc",
    );
    let _serve = node
        .serve_rpc("echo", Arc::new(EchoHandler))
        .expect("serve_rpc");
    let self_caps = node
        .capability_index_arc()
        .get(node.node_id())
        .expect("serve_rpc must auto-self-index");
    assert!(
        self_caps.has_tag("nrpc:echo"),
        "auto-self-indexed announcement must carry nrpc:<service>",
    );
}

/// H2 regression — `announce_capabilities` BEFORE `serve_rpc`
/// used to leave the self-announcement without the `nrpc:<service>`
/// tag, causing the callee-side gate to deny every inbound call
/// to the service. Post-fix, `serve_rpc` emits a fresh
/// announcement that merges every currently-registered service,
/// so order doesn't matter to the caller.
#[tokio::test]
async fn serve_rpc_self_index_works_regardless_of_announce_order() {
    let node = build_node().await;
    node.announce_capabilities(CapabilitySet::new())
        .await
        .expect("pre-announce");
    let pre = node
        .capability_index_arc()
        .get(node.node_id())
        .expect("pre-serve_rpc self-ann present");
    assert!(
        !pre.has_tag("nrpc:echo"),
        "pre-serve_rpc self-ann must not carry nrpc:echo",
    );

    let _serve = node
        .serve_rpc("echo", Arc::new(EchoHandler))
        .expect("serve_rpc");
    let post = node
        .capability_index_arc()
        .get(node.node_id())
        .expect("post-serve_rpc self-ann present");
    assert!(
        post.has_tag("nrpc:echo"),
        "post-serve_rpc self-ann must carry the merged tag regardless of order",
    );
}

/// A server that announces with `allowed_nodes = [some_other_node]`
/// (caller not in the list, no subnet or group match) denies the
/// caller. The caller-side gate inside `call_service` fires before
/// any wire round-trip — the test asserts the explicit
/// `RpcError::CapabilityDenied` variant.
#[tokio::test]
async fn call_service_caller_side_gate_denies_when_not_in_allow_list() {
    use net::adapter::net::behavior::CapabilityAnnouncement;

    let server = build_node().await;
    let caller = build_node().await;
    handshake_pair(&caller, &server).await;

    let _serve = server
        .serve_rpc("echo", Arc::new(EchoHandler))
        .expect("serve_rpc");
    // Server announces a restrictive allow-list — `allowed_nodes`
    // names a third-party id that is NOT the caller. The server's
    // own `announce_capabilities` would lay down a permissive
    // announcement (empty lists) merged with the nrpc tag, so we
    // bypass that and directly build a signed announcement with
    // the desired allow-list, then fold it into both sides'
    // indexes so the gate sees the restriction.
    let caps = CapabilitySet::new().add_tag("nrpc:echo");
    let mut ann = CapabilityAnnouncement::new(
        server.node_id(),
        server.entity_id().clone(),
        100,
        caps,
    );
    // Allow-list a synthetic node id distinct from the caller.
    ann.allowed_nodes = vec![0xDEAD_BEEF_BAAD_F00D];
    // Unsigned is fine here — the index's `index()` path doesn't
    // check signatures; signature verification is the
    // `handle_capability_announcement` receiver-side gate, which
    // this test bypasses by folding directly into the local
    // capability index. Caller's index drives the gate decision.
    caller.capability_index_arc().index(ann.clone());
    server.capability_index_arc().index(ann);

    let err = caller
        .call_service(
            "echo",
            Bytes::from_static(b"should-be-denied"),
            CallOptions::default(),
        )
        .await
        .expect_err("restrictive allow-list must deny the caller");
    match err {
        RpcError::CapabilityDenied { target, capability } => {
            assert_eq!(target, server.node_id());
            assert_eq!(capability, "echo");
        }
        other => panic!("expected CapabilityDenied, got {other:?}"),
    }
}

/// `serve_rpc` spawns an async re-announce so peers learn about
/// the new service without the operator calling
/// `announce_capabilities` manually. This pins that the spawn
/// actually fires and the `nrpc:<service>` tag lands in a peer's
/// capability index; a regression that dropped the spawn would
/// keep `serve_rpc_self_indexes_announcement_with_nrpc_tag`
/// passing (that one only checks the local index) but break this.
#[tokio::test]
async fn serve_rpc_spawned_reannounce_propagates_nrpc_tag_to_peers() {
    use net::adapter::net::behavior::CapabilityFilter;

    let server = build_node().await;
    let peer = build_node().await;
    handshake_pair(&peer, &server).await;

    // Intentionally do NOT call `announce_capabilities` on either
    // side. The only path that publishes the nrpc tag to `peer` is
    // the spawned re-announce inside `serve_rpc`.
    let _serve = server
        .serve_rpc("echo", Arc::new(EchoHandler))
        .expect("serve_rpc");

    let filter = CapabilityFilter::default().require_tag("nrpc:echo".to_string());
    let server_id = server.node_id();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        if peer
            .capability_index_arc()
            .query(&filter)
            .contains(&server_id)
        {
            return;
        }
        if tokio::time::Instant::now() > deadline {
            panic!(
                "spawned re-announce did not propagate `nrpc:echo` from server \
                 {server_id:#x} to peer's capability index within 3s; either \
                 the spawn was dropped or broadcast regressed",
            );
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The callee-side bridge gate bumps the per-service
/// `capability_denied_total` counter on rejection, even though the
/// handler is never invoked. Without this bump a noisy
/// unauthorized caller is invisible to operators watching
/// `nrpc_handler_invocations_total`; the dashboard sees "0
/// requests" while the caller sees `CapabilityDenied`. Pin the
/// counter movement end-to-end via the metrics snapshot.
#[tokio::test]
async fn callee_bridge_denial_bumps_capability_denied_metric() {
    use net::adapter::net::behavior::CapabilityAnnouncement;

    let server = build_node().await;
    let caller = build_node().await;
    handshake_pair(&caller, &server).await;

    let _serve = server
        .serve_rpc("echo", Arc::new(EchoHandler))
        .expect("serve_rpc");

    // Caller's local index sees a permissive view (so the
    // caller-side gate admits) — but the server's own index gets
    // a restrictive policy folded directly, so the bridge rejects.
    let caps_permissive = CapabilitySet::new().add_tag("nrpc:echo");
    let permissive = CapabilityAnnouncement::new(
        server.node_id(),
        server.entity_id().clone(),
        50,
        caps_permissive,
    );
    caller.capability_index_arc().index(permissive);

    let caps_restrictive = CapabilitySet::new().add_tag("nrpc:echo");
    let mut restrictive = CapabilityAnnouncement::new(
        server.node_id(),
        server.entity_id().clone(),
        100,
        caps_restrictive,
    );
    restrictive.allowed_nodes = vec![0xDEAD_BEEF_BAAD_F00D];
    server.capability_index_arc().index(restrictive);

    // Use `call` (not `call_service`) so the caller-side gate
    // doesn't fire — the rejection must come from the callee's
    // bridge, which is where the counter bump lives.
    let err = caller
        .call(
            server.node_id(),
            "echo",
            Bytes::from_static(b"bypass"),
            CallOptions {
                deadline: Some(
                    std::time::Instant::now() + Duration::from_secs(2),
                ),
                ..Default::default()
            },
        )
        .await
        .expect_err("callee-side gate must deny");
    assert!(
        matches!(err, RpcError::CapabilityDenied { .. }),
        "expected CapabilityDenied, got {err:?}",
    );

    // The counter is bumped inside the bridge task; the response
    // is also emitted asynchronously. By the time the caller's
    // future resolves with `CapabilityDenied`, the bump has
    // happened (the emit closure runs after the bump on the same
    // path). Snapshot and assert.
    let snap = server.rpc_metrics_snapshot();
    let echo = snap
        .services
        .iter()
        .find(|s| s.service == "echo")
        .expect("echo service tracked in registry");
    assert!(
        echo.capability_denied_total >= 1,
        "bridge denial must bump capability_denied_total; got snapshot {echo:?}",
    );
    assert_eq!(
        echo.handler_invocations_total, 0,
        "handler must not run on denied calls; got {} invocations",
        echo.handler_invocations_total,
    );
}
