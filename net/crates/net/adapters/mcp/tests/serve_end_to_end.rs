//! End-to-end demand side: wrap the fixture on one node, then drive the real
//! `MeshGateway` from another node to search, describe, and invoke it over a
//! live two-node mesh (`MCP_BRIDGE_PLAN.md` Phase 2).
//!
//! This proves the whole option-B path the shim's in-process tests can't: the
//! wrap node announces the `mcp-bridge` provider tag and serves the describe
//! service; the gateway discovers the provider through the capability fold,
//! fetches its catalog (schema + credential status) over the describe RPC, and
//! invokes a tool over nRPC — with the owner-scope gate admitting or rejecting
//! on the AEAD-verified caller origin.
//!
//! Same two-node idiom as `wrap_end_to_end`: `MeshBuilder` + a manual
//! handshake reaching through `Mesh::inner()`; distinct PSKs per test so the
//! parallel cases can't cross-connect.

use std::sync::Arc;
use std::time::Duration;

use net_mcp::bridge::BRIDGE_PROVIDER_TAG;
use net_mcp::serve::backend::{CapabilityGateway, CapabilityId, GatewayError, InvokeSafety};
use net_mcp::serve::MeshGateway;
use net_mcp::spec::{CallToolResult, Implementation};
use net_mcp::wrap::{wrap_server, CredentialOverride, OwnerScope, Substitutability, WrapConfig};
use net_sdk::capabilities::CapabilityFilter;
use net_sdk::mesh::{Mesh, MeshBuilder};
use serde_json::json;

const FIXTURE: &str = env!("CARGO_BIN_EXE_net-mcp-fixture");

fn client_info() -> Implementation {
    Implementation {
        name: "net-wrap".to_string(),
        version: "0.0.0".to_string(),
    }
}

async fn build_mesh(psk: &[u8; 32]) -> Mesh {
    MeshBuilder::new("127.0.0.1:0", psk)
        .expect("mesh builder")
        .build()
        .await
        .expect("build mesh")
}

/// Bidirectionally connect two meshes and start their receive loops.
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gateway_searches_describes_and_invokes_across_two_nodes() {
    let caller = Arc::new(build_mesh(&[0x51u8; 32]).await); // node A (demand)
    let host = build_mesh(&[0x51u8; 32]).await; // node B (supply)
    handshake(&caller, &host).await;
    let host_id = host.inner().node_id();

    // Wrap the fixture on the host, owner-scoped to the caller's origin (the
    // value its outbound calls carry) so both describe and invoke admit it.
    let config = WrapConfig::owner_only(client_info(), caller.origin_hash());
    let session = wrap_server(&host, FIXTURE, &[], &[], config)
        .await
        .expect("wrap the fixture on the host");
    assert!(session.tools().iter().any(|t| t == "echo"));

    // The caller discovers the host as a bridge provider through the fold.
    assert!(
        wait_until(
            || caller
                .find_nodes(&CapabilityFilter::new().require_tag(BRIDGE_PROVIDER_TAG))
                .contains(&host_id),
            Duration::from_secs(5),
        )
        .await,
        "caller discovers the host as an mcp-bridge provider",
    );

    // Short invoke deadline: these tests drive ultra-fast handlers whose reply
    // can be lost to the first-reply race (surfacing as a Timeout), so bound
    // that path rather than waiting out the generous production default.
    let gateway = MeshGateway::new(Arc::clone(&caller)).with_invoke_timeout(Duration::from_secs(3));

    // search — the gateway fetches the host's catalog over the describe RPC and
    // returns summaries (with the credential status the consent gate needs).
    // Retry: the first describe can lose its reply to the reply-channel race.
    let mut summaries = Vec::new();
    for _ in 0..5 {
        summaries = gateway.search("echo").await.expect("search");
        if !summaries.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let echo = summaries
        .iter()
        .find(|s| s.id.capability == "echo")
        .expect("echo is discoverable via the gateway");
    assert_eq!(echo.id.provider, host_id.to_string());
    assert_eq!(echo.compat_tier, "mcp_bridge");

    // describe — full schema comes back.
    let id = CapabilityId::new(host_id.to_string(), "echo");
    let detail = gateway.describe(&id).await.expect("describe echo");
    assert_eq!(detail.input_schema["type"], "object");
    assert_eq!(detail.id.display(), format!("{host_id}/echo"));

    // invoke — the wrapped tool round-trips the argument.
    let result = gateway
        .invoke(
            &id,
            json!({ "message": "hi gateway" }),
            InvokeSafety::DuplicateSafe,
        )
        .await
        .expect("invoke echo");
    assert!(!result.is_error, "{result:?}");
    assert_eq!(result.text(), "hi gateway");

    drop(session);
    host.shutdown().await.ok();
    // `caller` is an Arc held by the gateway; it drops with the test.
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_gateway_outside_the_owner_scope_sees_nothing_and_cannot_invoke() {
    let caller = Arc::new(build_mesh(&[0x52u8; 32]).await);
    let host = build_mesh(&[0x52u8; 32]).await;
    handshake(&caller, &host).await;
    let host_id = host.inner().node_id();

    // Owner scope admits only an unrelated origin — the caller is excluded from
    // both describe (visibility) and invoke.
    let mut config = WrapConfig::owner_only(client_info(), 0xDEAD_BEEF);
    config.scope = OwnerScope::owner_only(0xDEAD_BEEF);
    let session = wrap_server(&host, FIXTURE, &[], &[], config)
        .await
        .expect("wrap the fixture on the host");

    // The provider tag is still discoverable (v0 does not scope the announcement
    // itself) — search never implies invocation.
    assert!(
        wait_until(
            || caller
                .find_nodes(&CapabilityFilter::new().require_tag(BRIDGE_PROVIDER_TAG))
                .contains(&host_id),
            Duration::from_secs(5),
        )
        .await,
        "the bridge provider is reachable",
    );

    // Short invoke deadline: these tests drive ultra-fast handlers whose reply
    // can be lost to the first-reply race (surfacing as a Timeout), so bound
    // that path rather than waiting out the generous production default.
    let gateway = MeshGateway::new(Arc::clone(&caller)).with_invoke_timeout(Duration::from_secs(3));

    // search — the host denies the describe (owner scope), so the excluded
    // caller sees no capabilities. (An owner-scope denial can also surface as a
    // dropped-reply timeout for this ultra-fast rejection; search skips both, so
    // the result is empty either way.)
    let summaries = gateway.search("echo").await.expect("search runs");
    assert!(
        summaries.is_empty(),
        "an out-of-scope gateway must see no capabilities; got {summaries:?}",
    );

    // invoke — denied at the wrapper. The rejection surfaces as `Denied`, or as
    // a `Transport` timeout when the fast rejection outraces reply-subscription
    // setup; both are errors and neither yields a result.
    let id = CapabilityId::new(host_id.to_string(), "echo");
    let outcome = gateway
        .invoke(
            &id,
            json!({ "message": "nope" }),
            InvokeSafety::DuplicateSafe,
        )
        .await;
    assert!(
        matches!(
            outcome,
            Err(GatewayError::Denied(_)) | Err(GatewayError::Transport(_))
        ),
        "an out-of-scope caller must not get a result; got {outcome:?}",
    );

    drop(session);
    host.shutdown().await.ok();
}

/// One-way connect `caller` → `host` (accept + connect), without starting the
/// receive loops — the caller starts once after connecting to every host.
async fn connect(caller: &Mesh, host: &Mesh) {
    let pub_h = *host.inner().public_key();
    let nid_h = host.inner().node_id();
    let nid_c = caller.inner().node_id();
    let addr_h = host.inner().local_addr();
    let (r1, r2) = tokio::join!(host.inner().accept(nid_c), async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        caller.inner().connect(addr_h, &pub_h, nid_h).await
    });
    r1.expect("accept");
    r2.expect("connect");
}

/// A substitutable, uncredentialed wrap config — the two conditions the demand
/// side requires to collapse a capability across providers and fail over.
fn substitutable_config(owner_origin: u64) -> WrapConfig {
    let mut config = WrapConfig::owner_only(client_info(), owner_origin);
    config.substitutability = Substitutability::ProviderEquivalent;
    config.credential_override = CredentialOverride::NoCredentials;
    config.force = true;
    config
}

/// Invoke with a short retry — the first cross-node call to a provider can lose
/// its reply to the reply-channel race even after the gateway's own retries.
async fn invoke_retry(gateway: &MeshGateway, id: &CapabilityId, message: &str) -> CallToolResult {
    for _ in 0..6 {
        if let Ok(result) = gateway
            .invoke(
                id,
                json!({ "message": message }),
                InvokeSafety::DuplicateSafe,
            )
            .await
        {
            return result;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    panic!("invoke never succeeded for {}", id.display());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn invoke_fails_over_when_the_primary_provider_goes_down() {
    let psk = [0x53u8; 32];
    let caller = Arc::new(build_mesh(&psk).await);
    let host_a = build_mesh(&psk).await;
    let host_b = build_mesh(&psk).await;

    // Caller connects to both providers (hub), then everyone starts.
    connect(&caller, &host_a).await;
    connect(&caller, &host_b).await;
    caller.inner().start();
    host_a.inner().start();
    host_b.inner().start();

    let id_a = host_a.inner().node_id();
    let id_b = host_b.inner().node_id();

    // Wrap the same fixture on both hosts as a substitutable, uncredentialed
    // tool so the demand side collapses them and can fail over.
    let session_a = wrap_server(
        &host_a,
        FIXTURE,
        &[],
        &[],
        substitutable_config(caller.origin_hash()),
    )
    .await
    .expect("wrap on host a");
    let session_b = wrap_server(
        &host_b,
        FIXTURE,
        &[],
        &[],
        substitutable_config(caller.origin_hash()),
    )
    .await
    .expect("wrap on host b");

    // The caller discovers echo on BOTH providers.
    assert!(
        wait_until(
            || {
                let nodes = caller.find_nodes(&CapabilityFilter::new().require_tag("echo"));
                nodes.contains(&id_a) && nodes.contains(&id_b)
            },
            Duration::from_secs(5),
        )
        .await,
        "echo discovered on both providers",
    );

    // Short invoke deadline: these tests drive ultra-fast handlers whose reply
    // can be lost to the first-reply race (surfacing as a Timeout), so bound
    // that path rather than waiting out the generous production default.
    // This is the failover hero path, so opt into cross-provider collapse +
    // failover (off by default — see MeshGateway::trust_equivalent_providers).
    let gateway = MeshGateway::new(Arc::clone(&caller))
        .with_invoke_timeout(Duration::from_secs(3))
        .trust_equivalent_providers(true);

    // Search collapses the two providers into ONE logical capability.
    let mut summaries = Vec::new();
    for _ in 0..5 {
        summaries = gateway.search("echo").await.expect("search");
        if summaries
            .iter()
            .any(|s| s.id.capability == "echo" && s.providers.len() == 2)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let echo = summaries
        .iter()
        .find(|s| s.id.capability == "echo")
        .expect("echo group discovered");
    assert_eq!(
        echo.providers.len(),
        2,
        "collapsed group: {:?}",
        echo.providers
    );
    // Providers are sorted; the id points at the primary (lowest node id).
    let primary = echo.providers[0];
    assert_eq!(echo.id.provider, primary.to_string());
    let id = CapabilityId::new(primary.to_string(), "echo");

    // Invoke succeeds via the primary.
    let before = invoke_retry(&gateway, &id, "before").await;
    assert!(!before.is_error, "{before:?}");
    assert_eq!(before.text(), "before");

    // Kill the primary provider mid-session.
    if primary == id_a {
        drop(session_a);
        host_a.shutdown().await.ok();
    } else {
        drop(session_b);
        host_b.shutdown().await.ok();
    }

    // Invoke again — the primary is gone, so it transparently fails over to the
    // surviving provider. The model never sees the failure.
    let after = invoke_retry(&gateway, &id, "after").await;
    assert!(!after.is_error, "failover invoke should succeed: {after:?}");
    assert_eq!(after.text(), "after");
}
