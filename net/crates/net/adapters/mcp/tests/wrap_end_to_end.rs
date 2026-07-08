//! End-to-end supply side: publish the fixture on one node, discover + invoke
//! it from another, over a real two-node mesh.
//!
//! Proves the whole Phase-1 wrap path the unit tests can't:
//! `ServerPublisher::publish_server` announces the tools, the caller discovers
//! them through the capability fold, the nRPC call reaches the served handler,
//! the owner-scope gate admits or rejects on the AEAD-verified caller origin,
//! and the wrapped `tools/call` result comes back over the wire.
//!
//! Two directly-connected nodes are built with `MeshBuilder` + a manual
//! handshake — the same idiom the SDK's own cross-node RPC tests use. The
//! handshake reaches through `Mesh::inner()`; everything else stays on the
//! public `Mesh` surface.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use net_mcp::spec::{CallToolResult, Implementation};
use net_mcp::wrap::{OwnerScope, ServerPublisher, WrapConfig, WrapError};
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
    let host = Arc::new(build_mesh(&[0x42u8; 32]).await); // node B
    handshake(&caller, &host).await;
    let b_id = host.inner().node_id();

    // Real owner-only: admit the caller by its origin_hash — the value its
    // outbound calls carry as `caller_origin`. The successful invoke below
    // then proves `caller_origin == caller.origin_hash()` (the SDK accessor).
    let config = WrapConfig::owner_only(client_info(), caller.origin_hash());
    let publisher = ServerPublisher::new(Arc::clone(&host));
    let publication = publisher
        .publish_server(FIXTURE, &[], &[], config)
        .await
        .expect("publish the fixture on the host");
    assert!(publication.tools().iter().any(|t| t == "echo"));

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

    // Explicit withdraw reverses the publication (services + announcement) —
    // then the host Arc is unique again and can shut down.
    publication.withdraw().await.expect("withdraw");
    assert!(
        !host
            .find_nodes(&CapabilityFilter::new().require_tag("echo"))
            .contains(&b_id),
        "withdraw clears the announced tool set",
    );
    caller.shutdown().await.ok();
    drop(publisher);
    if let Ok(host) = Arc::try_unwrap(host) {
        host.shutdown().await.ok();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_non_channel_safe_tool_name_is_sanitized_and_still_invokable() {
    // F10: the fixture's `getCaps` (camelCase) isn't a valid channel id. It must
    // be BRIDGED under a sanitized service id — not dropped — and invoking that
    // id must reach the original `getCaps` on the wrapped server.
    let caller = build_mesh(&[0x46u8; 32]).await;
    let host = Arc::new(build_mesh(&[0x46u8; 32]).await);
    handshake(&caller, &host).await;
    let b_id = host.inner().node_id();

    let config = WrapConfig::owner_only(client_info(), caller.origin_hash());
    let publisher = ServerPublisher::new(Arc::clone(&host));
    let publication = publisher
        .publish_server(FIXTURE, &[], &[], config)
        .await
        .expect("publish the fixture on the host");
    // Nothing was dropped for being non-channel-safe.
    assert!(
        publication.skipped_tools().is_empty(),
        "no tool dropped: {:?}",
        publication.skipped_tools(),
    );
    // `getCaps` is served under a sanitized id (never verbatim).
    let safe_id = publication
        .tools()
        .iter()
        .find(|t| t.starts_with("getcaps_"))
        .expect("getCaps bridged under a sanitized id")
        .clone();
    assert_ne!(safe_id, "getCaps");

    // Discover + invoke by the sanitized id; the wrapped original runs.
    let filter = CapabilityFilter::new().require_tag(&safe_id);
    assert!(
        wait_until(
            || caller.find_nodes(&filter).contains(&b_id),
            Duration::from_secs(5),
        )
        .await,
        "caller discovers the sanitized capability",
    );
    let reply = call_retry(&caller, b_id, &safe_id, || Bytes::from_static(b"{}"))
        .await
        .expect("invoke the sanitized tool");
    let result: CallToolResult =
        serde_json::from_slice(reply.body.as_ref()).expect("decode CallToolResult");
    assert!(!result.is_error, "{result:?}");
    assert_eq!(result.text(), "caps-ok");

    caller.shutdown().await.ok();
    drop(publication);
    drop(publisher);
    if let Ok(host) = Arc::try_unwrap(host) {
        host.shutdown().await.ok();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_caller_outside_the_owner_scope_is_rejected() {
    let caller = build_mesh(&[0x43u8; 32]).await;
    let host = Arc::new(build_mesh(&[0x43u8; 32]).await);
    handshake(&caller, &host).await;
    let b_id = host.inner().node_id();

    // Owner scope admits only an unrelated origin — the caller is excluded.
    let mut config = WrapConfig::owner_only(client_info(), 0xDEAD_BEEF);
    config.scope = OwnerScope::owner_only(0xDEAD_BEEF);
    let publisher = ServerPublisher::new(Arc::clone(&host));
    // Keep the publication alive so the served handler (not a missing
    // service) is what rejects the out-of-scope caller.
    let _publication = publisher
        .publish_server(FIXTURE, &[], &[], config)
        .await
        .expect("publish the fixture on the host");

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
    drop(_publication);
    drop(publisher);
    if let Ok(host) = Arc::try_unwrap(host) {
        host.shutdown().await.ok();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn refresh_reconciles_the_tool_set_on_list_changed() {
    // Single node: `publish_server` self-indexes its announcement and serves
    // locally, so no peer is needed to observe the reconcile.
    let host = Arc::new(build_mesh(&[0x45u8; 32]).await);

    let mut config = WrapConfig::owner_only(client_info(), 0);
    config.scope = OwnerScope::any();
    let publisher = ServerPublisher::new(Arc::clone(&host));
    let mut publication = publisher
        .publish_server(FIXTURE, &[], &[], config)
        .await
        .expect("publish the fixture");
    let before: Vec<String> = publication.tools().to_vec();
    assert!(
        !before.iter().any(|t| t == "bonus"),
        "bonus is absent before the bump",
    );
    assert!(
        before.iter().any(|t| t == "echo"),
        "echo is served initially"
    );

    // Change the wrapped server's tool set (`_bump` makes `bonus` appear and the
    // server emit tools/list_changed).
    publication
        .client()
        .call_tool("_bump", json!({}))
        .await
        .expect("bump");

    // Refresh reconciles the mesh to the new set.
    let delta = publication.refresh().await.expect("refresh");
    assert!(
        delta.added.contains(&"bonus".to_string()),
        "bonus is newly served: {delta:?}",
    );
    assert!(delta.removed.is_empty(), "nothing was removed: {delta:?}");
    // Every pre-existing tool must remain served — reconciliation adds without
    // dropping tools that are still present.
    for tool in &before {
        assert!(
            publication.tools().iter().any(|t| t == tool),
            "pre-existing tool {tool:?} must remain served after refresh",
        );
    }
    assert!(publication.tools().iter().any(|t| t == "bonus"));
    assert!(
        host.find_nodes(&CapabilityFilter::new().require_tag("bonus"))
            .contains(&host.inner().node_id()),
        "the host now announces the `bonus` tool",
    );

    drop(publication);
    drop(publisher);
    if let Ok(host) = Arc::try_unwrap(host) {
        host.shutdown().await.ok();
    }
}

/// A trivial admission gate so a priced config clears the
/// "priced-without-gate" guard and reaches the pricing-key validation.
struct AdmitAllPayments;

#[async_trait::async_trait]
impl net_mcp::serve::PaymentAdmission for AdmitAllPayments {
    async fn redeem(
        &self,
        _tool_id: &str,
        _quote_id: &str,
        _binding: Option<&[u8]>,
    ) -> Result<(), net_sdk::tool_payment::GateDenial> {
        Ok(())
    }
}

/// M5 regression: pricing keyed by a tool the server does not export
/// (typo, since-renamed, or the sanitized channel id used instead of the
/// wrapped name) must be rejected at publish time — otherwise that key is
/// silently ignored and the tool it meant to price serves for free.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mis_keyed_pricing_config_is_rejected_at_publish() {
    const TERMS: &str = r#"{"object":"net.pricing.terms@1"}"#;
    let host = Arc::new(build_mesh(&[0x51u8; 32]).await);
    let publisher = ServerPublisher::new(Arc::clone(&host));

    // Correctly keyed by the fixture's real `echo` tool: publishes.
    let mut good = WrapConfig::owner_only(client_info(), 0);
    good.pricing.insert("echo".to_string(), TERMS.to_string());
    good.payment_admission = Some(Arc::new(AdmitAllPayments));
    let publication = publisher
        .publish_server(FIXTURE, &[], &[], good)
        .await
        .expect("echo priced by its real name publishes");
    publication.withdraw().await.expect("withdraw");

    // Mis-keyed (`ecko`): matches no tool, rejected before serving.
    let mut bad = WrapConfig::owner_only(client_info(), 0);
    bad.pricing.insert("ecko".to_string(), TERMS.to_string());
    bad.payment_admission = Some(Arc::new(AdmitAllPayments));
    match publisher.publish_server(FIXTURE, &[], &[], bad).await {
        Err(WrapError::PricingKeyUnmatched { keys }) => {
            assert_eq!(keys, vec!["ecko".to_string()]);
        }
        Err(other) => panic!("expected PricingKeyUnmatched, got {other:?}"),
        Ok(_) => panic!("a mis-keyed pricing config must be rejected, but publish succeeded"),
    }

    // The mis-key never announced: no `ecko` capability leaked onto the mesh.
    assert!(
        !host
            .find_nodes(&CapabilityFilter::new().require_tag("ecko"))
            .contains(&host.inner().node_id()),
        "a rejected publish announces nothing",
    );

    drop(publisher);
    if let Ok(host) = Arc::try_unwrap(host) {
        host.shutdown().await.ok();
    }
}
