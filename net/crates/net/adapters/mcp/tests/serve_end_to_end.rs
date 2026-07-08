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

use bytes::Bytes;
use net_mcp::bridge::BRIDGE_PROVIDER_TAG;
use net_mcp::serve::backend::{CapabilityGateway, CapabilityId, GatewayError, InvokeSafety};
use net_mcp::serve::{gated_invoke, ConsentPolicy, GatedOutcome, MeshGateway, PinStore};
use net_mcp::spec::{CallToolResult, Implementation};
use net_mcp::wrap::{
    build_challenge, build_envelope, CredentialOverride, DelegationAudit, DelegationGate,
    DelegationSigner, OwnerScope, ServerPublisher, Substitutability, WrapConfig, HDR_DELEGATION,
    HDR_DELEGATION_SIG,
};
use net_sdk::capabilities::CapabilityFilter;
use net_sdk::delegation::{
    derive_child_seed, DelegationChain, RevocationRegistry, DEFAULT_DELEGATION_DEPTH,
};
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::mesh_rpc::CallOptions;
use net_sdk::Identity;
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
    let host = Arc::new(build_mesh(&[0x51u8; 32]).await); // node B (supply)
    handshake(&caller, &host).await;
    let host_id = host.inner().node_id();

    // Publish the fixture on the host, owner-scoped to the caller's origin
    // (the value its outbound calls carry) so both describe and invoke admit it.
    let config = WrapConfig::owner_only(client_info(), caller.origin_hash());
    let publisher = ServerPublisher::new(Arc::clone(&host));
    let publication = publisher
        .publish_server(FIXTURE, &[], &[], config)
        .await
        .expect("publish the fixture on the host");
    assert!(publication.tools().iter().any(|t| t == "echo"));

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

    // invoke — the wrapped tool round-trips the argument. A free tool
    // carries no payment proof.
    let result = gateway
        .invoke(
            &id,
            json!({ "message": "hi gateway" }),
            InvokeSafety::DuplicateSafe,
            None,
        )
        .await
        .expect("invoke echo");
    assert!(!result.is_error, "{result:?}");
    assert_eq!(result.text(), "hi gateway");

    drop(publication);
    drop(publisher);
    if let Ok(host) = Arc::try_unwrap(host) {
        host.shutdown().await.ok();
    }
    // `caller` is an Arc held by the gateway; it drops with the test.
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_gateway_outside_the_owner_scope_sees_nothing_and_cannot_invoke() {
    let caller = Arc::new(build_mesh(&[0x52u8; 32]).await);
    let host = Arc::new(build_mesh(&[0x52u8; 32]).await);
    handshake(&caller, &host).await;
    let host_id = host.inner().node_id();

    // Owner scope admits only an unrelated origin — the caller is excluded from
    // both describe (visibility) and invoke.
    let mut config = WrapConfig::owner_only(client_info(), 0xDEAD_BEEF);
    config.scope = OwnerScope::owner_only(0xDEAD_BEEF);
    let publisher = ServerPublisher::new(Arc::clone(&host));
    let publication = publisher
        .publish_server(FIXTURE, &[], &[], config)
        .await
        .expect("publish the fixture on the host");

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
            None,
        )
        .await;
    assert!(
        matches!(
            outcome,
            Err(GatewayError::Denied { .. }) | Err(GatewayError::Transport(_))
        ),
        "an out-of-scope caller must not get a result; got {outcome:?}",
    );

    drop(publication);
    drop(publisher);
    if let Ok(host) = Arc::try_unwrap(host) {
        host.shutdown().await.ok();
    }
}

/// Phase 3 Slice B, over a live mesh: a caller the owner-scope allowlist
/// EXCLUDES is nonetheless admitted when it presents a valid delegation chain
/// (rooted at the provider's owner) plus a per-invoke signature by the chain
/// leaf — proving the delegation path admits independently of the (spoofable)
/// origin allowlist, and that the provider audits the admitted leaf.
///
/// The chain + signed envelope ride in the `net-delegation` / `net-delegation-sig`
/// request headers; each retry mints a FRESH nonce because the gate's replay
/// guard (correctly) rejects a reused one, and the reply-channel first-reply
/// race can force a retry.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_delegated_invoke_admits_via_the_chain_and_audits_the_leaf() {
    let caller = Arc::new(build_mesh(&[0x55u8; 32]).await); // node A (demand)
    let host = Arc::new(build_mesh(&[0x55u8; 32]).await); // node B (supply)
    handshake(&caller, &host).await;
    let host_id = host.inner().node_id();

    // The user's delegation tree: root -> machine -> gateway(leaf), derived
    // exactly as the plugin does (deterministic children from the root seed).
    let root = Identity::generate();
    let seed = root.to_bytes();
    let machine = Identity::from_seed(derive_child_seed(&seed, "machine:h"));
    let gateway = Identity::from_seed(derive_child_seed(&seed, "gateway:h:hermes"));
    let chain = DelegationChain::derive_gateway(
        &root,
        &machine,
        gateway.entity_id(),
        Duration::from_secs(3600),
        DEFAULT_DELEGATION_DEPTH,
    )
    .expect("derive the gateway chain");
    let chain_bytes = chain.to_bytes();

    // Provider: owner-scope admits only an UNRELATED origin (so it excludes the
    // caller), plus a delegation gate anchored at `root`. Only a verified chain
    // can admit the caller — proving the delegation path, not owner-scope.
    let seen: Arc<std::sync::Mutex<Vec<DelegationAudit>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink_seen = Arc::clone(&seen);
    let gate = Arc::new(
        DelegationGate::new(
            root.entity_id().clone(),
            Arc::new(RevocationRegistry::new()),
        )
        .with_audit(Arc::new(move |a: &DelegationAudit| {
            sink_seen.lock().expect("audit lock").push(a.clone());
        })),
    );
    let mut config = WrapConfig::owner_only(client_info(), 0xDEAD_BEEF);
    config.scope = OwnerScope::owner_only(0xDEAD_BEEF); // excludes the caller
    config.delegation = Some(gate);

    let publisher = ServerPublisher::new(Arc::clone(&host));
    let publication = publisher
        .publish_server(FIXTURE, &[], &[], config)
        .await
        .expect("publish the fixture on the host");
    assert!(publication.tools().iter().any(|t| t == "echo"));

    // Directly invoke the "echo" service (nRPC service name == tool id) with the
    // delegation headers. Fresh nonce per attempt (replay guard + reply race).
    let args = json!({ "message": "delegated hi" });
    let body = serde_json::to_vec(&args).expect("encode args");

    let mut result: Option<CallToolResult> = None;
    for attempt in 0..8u64 {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs();
        let nonce = attempt + 1;
        let sig = gateway.sign(&build_challenge("echo", &body, ts, nonce));
        let env = build_envelope(ts, nonce, &sig);
        let opts = CallOptions {
            request_headers: vec![
                (HDR_DELEGATION.to_string(), chain_bytes.clone()),
                (HDR_DELEGATION_SIG.to_string(), env),
            ],
            ..CallOptions::default()
        };
        match caller
            .call(host_id, "echo", Bytes::from(body.clone()), opts)
            .await
        {
            Ok(reply) => {
                result = Some(serde_json::from_slice(&reply.body).expect("decode tool result"));
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(150)).await,
        }
    }

    let result = result.expect("a valid delegated invoke must reach the provider");
    assert!(!result.is_error, "{result:?}");
    assert_eq!(result.text(), "delegated hi");

    // The provider audited the admitted LEAF (the gateway), under the root.
    let recorded = seen.lock().expect("audit lock");
    assert!(
        recorded.iter().any(|a| &a.leaf == gateway.entity_id()
            && &a.root == root.entity_id()
            && a.tool == "echo"),
        "provider audit must record the delegated gateway leaf; got {recorded:?}",
    );

    drop(publication);
    drop(publisher);
    if let Ok(host) = Arc::try_unwrap(host) {
        host.shutdown().await.ok();
    }
}

/// Phase 3 Slice B2: the demand-side `MeshGateway` auto-attaches the delegation
/// headers itself — a `DelegationSigner` holding the leaf key + chain signs each
/// invoke — so an owner-scope-EXCLUDED caller invokes successfully through the
/// ordinary `gateway.invoke` path (no hand-built headers), and the provider
/// audits the leaf. This is the B1 wire proof driven through the real caller.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_gateway_auto_attaches_delegation_and_invokes() {
    let caller = Arc::new(build_mesh(&[0x56u8; 32]).await); // node A (demand)
    let host = Arc::new(build_mesh(&[0x56u8; 32]).await); // node B (supply)
    handshake(&caller, &host).await;
    let host_id = host.inner().node_id();

    // root -> machine -> gateway(leaf), derived as the plugin does.
    let root = Identity::generate();
    let seed = root.to_bytes();
    let machine = Identity::from_seed(derive_child_seed(&seed, "machine:h"));
    let gateway_id = Identity::from_seed(derive_child_seed(&seed, "gateway:h:hermes"));
    let chain = DelegationChain::derive_gateway(
        &root,
        &machine,
        gateway_id.entity_id(),
        Duration::from_secs(3600),
        DEFAULT_DELEGATION_DEPTH,
    )
    .expect("derive the gateway chain");

    // Provider excludes the caller from owner-scope; only the delegation gate
    // (anchored at `root`) can admit — so a successful invoke proves the
    // gateway auto-attached a valid chain + signature.
    let seen: Arc<std::sync::Mutex<Vec<DelegationAudit>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink_seen = Arc::clone(&seen);
    let gate = Arc::new(
        DelegationGate::new(
            root.entity_id().clone(),
            Arc::new(RevocationRegistry::new()),
        )
        .with_audit(Arc::new(move |a: &DelegationAudit| {
            sink_seen.lock().expect("audit lock").push(a.clone());
        })),
    );
    let mut config = WrapConfig::owner_only(client_info(), 0xDEAD_BEEF);
    config.scope = OwnerScope::owner_only(0xDEAD_BEEF); // excludes the caller
    config.delegation = Some(gate);
    let publisher = ServerPublisher::new(Arc::clone(&host));
    let publication = publisher
        .publish_server(FIXTURE, &[], &[], config)
        .await
        .expect("publish the fixture on the host");
    assert!(publication.tools().iter().any(|t| t == "echo"));

    // The gateway signs + attaches on every invoke via the held leaf key.
    let signer = Arc::new(DelegationSigner::new(gateway_id.clone(), chain.to_bytes()));
    let gateway = MeshGateway::new(Arc::clone(&caller))
        .with_invoke_timeout(Duration::from_secs(3))
        .with_delegation(signer);

    // Invoke directly (owner-scope excludes the caller, so describe would show
    // nothing — the B2 delegation path is invoke-only).
    let id = CapabilityId::new(host_id.to_string(), "echo");
    let result = invoke_retry(&gateway, &id, "delegated via gateway").await;
    assert!(!result.is_error, "{result:?}");
    assert_eq!(result.text(), "delegated via gateway");

    let recorded = seen.lock().expect("audit lock");
    assert!(
        recorded.iter().any(|a| &a.leaf == gateway_id.entity_id()
            && &a.root == root.entity_id()
            && a.tool == "echo"),
        "provider must audit the delegated leaf; got {recorded:?}",
    );

    drop(publication);
    drop(publisher);
    if let Ok(host) = Arc::try_unwrap(host) {
        host.shutdown().await.ok();
    }
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
                None,
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
    let host_a = Arc::new(build_mesh(&psk).await);
    let host_b = Arc::new(build_mesh(&psk).await);

    // Caller connects to both providers (hub), then everyone starts.
    connect(&caller, &host_a).await;
    connect(&caller, &host_b).await;
    caller.inner().start();
    host_a.inner().start();
    host_b.inner().start();

    let id_a = host_a.inner().node_id();
    let id_b = host_b.inner().node_id();

    // Publish the same fixture on both hosts as a substitutable,
    // uncredentialed tool so the demand side collapses them and can fail over.
    let publisher_a = ServerPublisher::new(Arc::clone(&host_a));
    let publication_a = publisher_a
        .publish_server(
            FIXTURE,
            &[],
            &[],
            substitutable_config(caller.origin_hash()),
        )
        .await
        .expect("publish on host a");
    let publisher_b = ServerPublisher::new(Arc::clone(&host_b));
    let publication_b = publisher_b
        .publish_server(
            FIXTURE,
            &[],
            &[],
            substitutable_config(caller.origin_hash()),
        )
        .await
        .expect("publish on host b");

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
        drop(publication_a);
        drop(publisher_a);
        if let Ok(host_a) = Arc::try_unwrap(host_a) {
            host_a.shutdown().await.ok();
        }
    } else {
        drop(publication_b);
        drop(publisher_b);
        if let Ok(host_b) = Arc::try_unwrap(host_b) {
            host_b.shutdown().await.ok();
        }
    }

    // Invoke again — the primary is gone, so it transparently fails over to the
    // surviving provider. The model never sees the failure.
    let after = invoke_retry(&gateway, &id, "after").await;
    assert!(!after.is_error, "failover invoke should succeed: {after:?}");
    assert_eq!(after.text(), "after");
}

/// `gated_invoke`, retried past the reply-channel first-reply race: a transient
/// `Transport` failure on the first describe/invoke RPC is not the gate's
/// verdict, so retry until the outcome is stable.
async fn gated_invoke_stable(
    gateway: &MeshGateway,
    consent: &ConsentPolicy,
    pins: Option<&PinStore>,
    id: &CapabilityId,
    args: &serde_json::Value,
) -> GatedOutcome {
    let mut out = gated_invoke(gateway, consent, pins, None, id, args.clone()).await;
    for _ in 0..6 {
        if !matches!(out, GatedOutcome::Failed(GatewayError::Transport(_))) {
            return out;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
        out = gated_invoke(gateway, consent, pins, None, id, args.clone()).await;
    }
    out
}

/// The consent gate, end to end against the live wrapped fixture — the exact
/// SDK path the Python `CapabilityGateway` wraps. An unapproved invoke is held
/// at `RequiresApproval` and never reaches the provider; once the operator
/// approves the pin in the shared store (as `net mcp pin approve` would), the
/// same call clears consent, reaches the provider, and the wrapped tool runs.
/// Consent (`gated_invoke` + `PinStore`) and provider owner-scope (the wrap
/// config) are two independent gates, both satisfied here.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gated_invoke_holds_until_the_pin_is_approved() {
    let caller = Arc::new(build_mesh(&[0x54u8; 32]).await); // node A (demand)
    let host = Arc::new(build_mesh(&[0x54u8; 32]).await); // node B (supply)
    handshake(&caller, &host).await;
    let host_id = host.inner().node_id();

    // Wrap the fixture, owner-scoped to the caller's origin so the provider
    // admits the invoke once consent clears.
    let config = WrapConfig::owner_only(client_info(), caller.origin_hash());
    let publisher = ServerPublisher::new(Arc::clone(&host));
    let publication = publisher
        .publish_server(FIXTURE, &[], &[], config)
        .await
        .expect("publish the fixture on the host");
    assert!(publication.tools().iter().any(|t| t == "echo"));

    assert!(
        wait_until(
            || caller
                .find_nodes(&CapabilityFilter::new().require_tag(BRIDGE_PROVIDER_TAG))
                .contains(&host_id),
            Duration::from_secs(5),
        )
        .await,
        "caller discovers the wrapped provider",
    );

    let gateway = MeshGateway::new(Arc::clone(&caller)).with_invoke_timeout(Duration::from_secs(3));

    // Consumer-side consent: an empty policy (so everything is gated) plus the
    // real, machine-shared pin store on disk — the same file `net mcp pin`
    // writes and a running shim reads.
    let consent = ConsentPolicy::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let pin_path = dir.path().join("mcp-pins.json");

    let id = CapabilityId::new(host_id.to_string(), "echo");
    let args = json!({ "message": "gated hello" });

    // [1] No approval yet → the gate fires; the wrapped tool is never reached.
    let out = gated_invoke_stable(&gateway, &consent, None, &id, &args).await;
    assert!(
        matches!(out, GatedOutcome::RequiresApproval),
        "an unapproved capability must be held at RequiresApproval; got {out:?}",
    );

    // [2] The operator approves the pin out of band — a full locked
    // load->apply->save on the shared store, exactly as `net mcp pin approve`.
    PinStore::mutate(pin_path.clone(), |s| s.approve(&id))
        .await
        .expect("approve the pin");

    // [3] With the fresh approval visible, consent clears and the same invoke
    // reaches the provider; the wrapped echo round-trips the argument.
    let pins = PinStore::load(pin_path.clone()).await.ok();
    let out = gated_invoke_stable(&gateway, &consent, pins.as_ref(), &id, &args).await;
    match out {
        GatedOutcome::Invoked(result) => {
            assert!(!result.is_error, "{result:?}");
            assert_eq!(result.text(), "gated hello");
        }
        other => panic!("an approved capability must reach the provider; got {other:?}"),
    }

    drop(publication);
    drop(publisher);
    if let Ok(host) = Arc::try_unwrap(host) {
        host.shutdown().await.ok();
    }
}
