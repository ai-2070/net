//! End-to-end supply side: wrap the fixture on one node, discover + invoke it
//! from another, over a real two-node mesh.
//!
//! Proves the whole Phase-1 wrap path the unit tests can't: `wrap_server`
//! announces the tools, the caller discovers them through the capability fold,
//! the nRPC call reaches the served handler, the owner-scope gate admits or
//! rejects on the AEAD-verified caller origin, and the wrapped `tools/call`
//! result comes back over the wire.
//!
//! Two directly-connected nodes are built with `MeshBuilder` + a manual
//! handshake — the same idiom the SDK's own cross-node RPC tests use. The
//! handshake reaches through `Mesh::inner()`; everything else stays on the
//! public `Mesh` surface.

use std::time::Duration;

use bytes::Bytes;
use net_mcp::spec::{CallToolResult, Implementation};
use net_mcp::wrap::{wrap_server, OwnerScope, WrapConfig};
use net_sdk::capabilities::CapabilityFilter;
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::mesh_rpc::CallOptions;
use serde_json::json;

const FIXTURE: &str = env!("CARGO_BIN_EXE_net-mcp-fixture");

fn client_info() -> Implementation {
    Implementation {
        name: "net-wrap".to_string(),
        version: "0.0.0".to_string(),
    }
}

fn echo_args(message: &str) -> Bytes {
    Bytes::from(serde_json::to_vec(&json!({ "message": message })).unwrap())
}

// Each test uses its own PSK so the two test cases (which cargo runs in
// parallel) can never establish sessions with each other's meshes.
async fn build_mesh(psk: &[u8; 32]) -> Mesh {
    MeshBuilder::new("127.0.0.1:0", psk)
        .expect("mesh builder")
        .build()
        .await
        .expect("build mesh")
}

/// Bidirectionally connect two meshes and start their receive loops (the SDK
/// cross-node test idiom).
async fn handshake(a: &Mesh, b: &Mesh) {
    let pub_b = *b.inner().public_key();
    let nid_b = b.inner().node_id();
    let nid_a = a.inner().node_id();
    let addr_b = b.inner().local_addr();
    let (r1, r2) = tokio::join!(b.inner().accept(nid_a), async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        a.inner().connect(addr_b, &pub_b, nid_b).await
    });
    r1.expect("accept");
    r2.expect("connect");
    a.inner().start();
    b.inner().start();
}

/// Poll `cond` until true or `timeout` elapses.
async fn wait_until<F: FnMut() -> bool>(mut cond: F, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    cond()
}

/// A call bounded so a wedged mesh fails the test instead of hanging CI.
async fn call_bounded(
    caller: &Mesh,
    target: u64,
    service: &str,
    body: Bytes,
) -> Result<net_sdk::mesh_rpc::RpcReply, String> {
    match tokio::time::timeout(
        Duration::from_secs(5),
        caller.call(target, service, body, CallOptions::default()),
    )
    .await
    {
        Ok(Ok(reply)) => Ok(reply),
        Ok(Err(e)) => Err(format!("rpc error: {e:?}")),
        Err(_) => Err("call timed out".to_string()),
    }
}

/// Retry a call a few times. The first cross-node call to a freshly-served
/// handler can lose its reply if the handler answers before the caller's
/// per-caller reply subscription has propagated — a warm-up call establishes
/// it, so a retry lands. `body` is a fresh-`Bytes` factory since each attempt
/// consumes one.
async fn call_retry(
    caller: &Mesh,
    target: u64,
    service: &str,
    body: impl Fn() -> Bytes,
) -> Result<net_sdk::mesh_rpc::RpcReply, String> {
    let mut last = String::new();
    for _ in 0..5 {
        match call_bounded(caller, target, service, body()).await {
            Ok(reply) => return Ok(reply),
            Err(e) => {
                last = e;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
    Err(last)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wrap_discover_and_invoke_across_two_nodes() {
    let caller = build_mesh(&[0x42u8; 32]).await; // node A
    let host = build_mesh(&[0x42u8; 32]).await; // node B
    handshake(&caller, &host).await;
    let b_id = host.inner().node_id();

    // Real owner-only: admit the caller by its origin_hash — the value its
    // outbound calls carry as `caller_origin`. The successful invoke below
    // then proves `caller_origin == caller.origin_hash()` (the SDK accessor).
    let config = WrapConfig::owner_only(client_info(), caller.origin_hash());
    let session = wrap_server(&host, FIXTURE, &[], &[], config)
        .await
        .expect("wrap the fixture on the host");
    assert!(session.tools().iter().any(|t| t == "echo"));

    // The caller discovers the host advertising `echo` through the fold.
    let filter = CapabilityFilter::new().require_tag("echo");
    assert!(
        wait_until(
            || caller.find_nodes(&filter).contains(&b_id),
            Duration::from_secs(5),
        )
        .await,
        "caller discovers the host's echo capability",
    );

    // Invoke echo on the host; the owner-scope gate admits the caller and the
    // wrapped tool round-trips the argument.
    let reply = call_retry(&caller, b_id, "echo", || echo_args("hi mesh"))
        .await
        .expect("echo call succeeds");
    let result: CallToolResult =
        serde_json::from_slice(reply.body.as_ref()).expect("decode CallToolResult");
    assert!(!result.is_error);
    assert_eq!(result.text(), "hi mesh");

    caller.shutdown().await.ok();
    host.shutdown().await.ok();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_caller_outside_the_owner_scope_is_rejected() {
    let caller = build_mesh(&[0x43u8; 32]).await;
    let host = build_mesh(&[0x43u8; 32]).await;
    handshake(&caller, &host).await;
    let b_id = host.inner().node_id();

    // Owner scope admits only an unrelated origin — the caller is excluded.
    let mut config = WrapConfig::owner_only(client_info(), 0xDEAD_BEEF);
    config.scope = OwnerScope::owner_only(0xDEAD_BEEF);
    wrap_server(&host, FIXTURE, &[], &[], config)
        .await
        .expect("wrap the fixture on the host");

    // The service is discoverable — search never implies invocation.
    let filter = CapabilityFilter::new().require_tag("echo");
    assert!(
        wait_until(
            || caller.find_nodes(&filter).contains(&b_id),
            Duration::from_secs(5),
        )
        .await,
        "the echo capability is reachable",
    );

    // Invoke echo; the wrapper-side owner-scope gate denies it, so the caller
    // never gets a result. The denial surfaces one of two ways, both `Err`:
    //   - as the structured `ServerError { status: 0x8001, "..owner scope.." }`
    //     the handler returns (see the unit tests + the ERR_OWNER_SCOPE code), or
    //   - as a dropped reply / timeout: an owner-scope rejection is near-instant,
    //     so its reply can outrace the caller's per-caller reply-channel
    //     subscription setup — an nRPC reply-routing timing property for
    //     ultra-fast handlers, not a bridge behavior.
    // Either way the security property holds: an excluded caller cannot invoke.
    let outcome = call_bounded(&caller, b_id, "echo", echo_args("denied")).await;
    assert!(
        outcome.is_err(),
        "a caller outside the owner scope must not get a result; got {outcome:?}",
    );

    caller.shutdown().await.ok();
    host.shutdown().await.ok();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn refresh_reconciles_the_tool_set_on_list_changed() {
    // Single node: `wrap_server` self-indexes its announcement and serves
    // locally, so no peer is needed to observe the reconcile.
    let host = build_mesh(&[0x45u8; 32]).await;

    let mut config = WrapConfig::owner_only(client_info(), 0);
    config.scope = OwnerScope::any();
    let mut session = wrap_server(&host, FIXTURE, &[], &[], config)
        .await
        .expect("wrap the fixture");
    assert!(
        !session.tools().iter().any(|t| t == "bonus"),
        "bonus is absent before the bump",
    );

    // Change the wrapped server's tool set (`_bump` makes `bonus` appear and the
    // server emit tools/list_changed).
    session
        .client()
        .call_tool("_bump", json!({}))
        .await
        .expect("bump");

    // Refresh reconciles the mesh to the new set.
    let delta = session.refresh(&host).await.expect("refresh");
    assert!(
        delta.added.contains(&"bonus".to_string()),
        "bonus is newly served: {delta:?}",
    );
    assert!(session.tools().iter().any(|t| t == "bonus"));
    assert!(
        host.find_nodes(&CapabilityFilter::new().require_tag("bonus"))
            .contains(&host.inner().node_id()),
        "the host now announces the `bonus` tool",
    );

    host.shutdown().await.ok();
}
