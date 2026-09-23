//! Stage 3 — the org streaming facade's live witnesses (S3.1).
//!
//! Real Rust callers through the public facade (`mesh.org(..)` +
//! `OrgClient::call_streaming` / `call_client_stream` / `call_duplex`) for the
//! three streaming shapes, same-org and granted, with the pin / refusal /
//! ownership properties the stage's exit names. Providers ride the FROZEN
//! core seams (`serve_rpc_{owner_scoped,granted}_*`) at this row — the facade
//! provider verbs are row 3.2's — and every handler observes the VERIFIED
//! `ctx.org_admission` projection, never caller-claimed identity.
//!
//! Fixture shape from `src/org/tests_live.rs:176-273` (`fast_mesh`), with the
//! grant/audience ceremony of `live_cross_org_call_through_the_facade`.
//!
//! `live_same_org_client_stream_through_the_facade` is THE PIN WITNESS: its
//! scenario resolves a second provider MID-CALL, and the 3.1 pin receipt's
//! mutation (the send re-resolving instead of reusing the pinned plan) must
//! redden its named assertion.

#![cfg(all(feature = "net", feature = "cortex"))]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use net::adapter::net::behavior::org::OrgRevocationBundle;
use net::adapter::net::behavior::org_admission::Admitted;
use net::adapter::net::behavior::CapabilitySet;
use net::adapter::net::cortex::{
    RequestStream, RpcClientStreamingHandler, RpcContext, RpcDuplexHandler, RpcHandlerError,
    RpcResponsePayload, RpcResponseSink, RpcStatus, RpcStreamingContext, RpcStreamingHandler,
};
use net::adapter::net::identity::EntityId;
use net::adapter::net::org_admission_gate::OrgProviderPolicy;
use net::adapter::net::{ChannelConfigRegistry, MeshNode, MeshNodeConfig};
use net_sdk::identity::Identity;
use net_sdk::mesh::Mesh;
use net_sdk::mesh_rpc::{RequestStreamTyped, ResponseSinkTyped};
use net_sdk::org::types::*;
use net_sdk::org::{CoarseAdmissionReason, OrgAccess, OrgCaller, OrgCredentials, OrgSdkError};

// ---------------------------------------------------------------------------
// Message shapes
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct Ping {
    n: u32,
}

/// A server-streaming / duplex item, tagged with the serving node's id so a
/// witness can prove WHICH provider produced every item.
#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct Item {
    n: u32,
    served_by: u64,
}

/// The terminal response of an upload, carrying every chunk the provider saw.
#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct UploadSummary {
    chunks: usize,
    seen: Vec<u32>,
    served_by: u64,
}

// ---------------------------------------------------------------------------
// Fixtures (shape: `src/org/tests_live.rs:176-273`)
// ---------------------------------------------------------------------------

fn org_a() -> OrgKeypair {
    OrgKeypair::from_bytes([0xA1u8; 32])
}

fn org_b() -> OrgKeypair {
    OrgKeypair::from_bytes([0xB2u8; 32])
}

fn cap(tag: &str) -> CapabilityAuthorityId {
    CapabilityAuthorityId::for_tag(tag)
}

/// Membership + dispatcher for `member` acting for `org`, both wide open.
fn belonging(org: &OrgKeypair, member: &EntityId) -> (OrgMembershipCert, OrgDispatcherGrant) {
    let cert = OrgMembershipCert::try_issue(org, member.clone(), 1, 3600).expect("cert");
    let grant =
        OrgDispatcherGrant::try_issue(org, member.clone(), DispatcherScope::Any, 3600).expect("dg");
    (cert, grant)
}

/// A DISCOVER|INVOKE grant from `issuer` to `grantee_org` over `capability`.
fn discover_grant(
    issuer: &OrgKeypair,
    grantee_org: OrgId,
    capability: CapabilityAuthorityId,
    ttl: u64,
) -> (OrgCapabilityGrant, OrgAudienceSecret) {
    let (grant, secret) = OrgCapabilityGrant::try_issue(
        issuer,
        grantee_org,
        capability,
        GrantRights::INVOKE.union(GrantRights::DISCOVER),
        GrantTargetScope::AnyNodeOwnedBy(issuer.org_id()),
        ttl,
    )
    .expect("grant");
    (grant, secret.expect("discover mints a secret"))
}

/// A mesh whose node re-announces promptly, adopted into `owner`'s org.
///
/// `shared_audience` models §3.4's out-of-band pre-staging: owner-scoped
/// discovery is keyed on ONE per-organization audience, so two independently
/// adopted nodes each minting their own could never open each other's
/// envelopes.
async fn fast_mesh(
    tag: &str,
    owner: &OrgKeypair,
    shared_audience: Option<&OwnerAudienceCredential>,
) -> (Mesh, Identity, std::path::PathBuf) {
    let identity = Identity::generate();
    let mut cfg = MeshNodeConfig::new("127.0.0.1:0".parse().expect("addr"), [0x51u8; 32])
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5));
    cfg.min_announce_interval = Duration::from_millis(50);
    cfg.configured_identity = true;

    let mut node = MeshNode::new((**identity.keypair()).clone(), cfg)
        .await
        .expect("MeshNode::new");
    let channel_configs = Arc::new(ChannelConfigRegistry::new());
    node.set_channel_configs(channel_configs.clone());
    let node = Arc::new(node);

    let entity = identity.entity_id().clone();
    let cert = OrgMembershipCert::try_issue(owner, entity.clone(), 1, 3600).expect("cert");
    let dir = std::env::temp_dir().join(format!(
        "net-osdk-org-streaming-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let authority = NodeAuthority::adopt(&dir, cert, &entity, 0, None).expect("adopt");
    let authority = match shared_audience {
        None => authority,
        Some(shared) => NodeAuthority {
            config: authority.config.clone(),
            audience: OwnerAudienceCredential::decode_config(&shared.encode_config())
                .expect("decode shared owner audience"),
            revocation: authority.revocation.clone(),
        },
    };
    node.install_node_authority(Arc::new(authority))
        .expect("install authority");
    node.set_owner_cert_emission(true)
        .expect("enable owner-cert emission");

    let mesh = Mesh::from_node_arc(node, channel_configs, Some(identity.clone()));
    (mesh, identity, dir)
}

/// Handshake two meshes and wait for both entity pins — protected RPC is
/// direct-session-only, so the pins ARE the precondition.
async fn bring_up(caller: &Mesh, provider: &Mesh) {
    let provider_pub = *provider.public_key();
    let provider_addr = provider.local_addr().to_string();
    let p_clone = provider.node().clone();
    let caller_id = caller.node_id();
    let accept = tokio::spawn(async move { p_clone.accept(caller_id).await });
    caller
        .connect(&provider_addr, &provider_pub, provider.node_id())
        .await
        .expect("connect");
    accept.await.expect("accept task").expect("accept");
    caller.start();
    provider.start();

    for m in [caller, provider] {
        m.node()
            .announce_capabilities(CapabilitySet::new())
            .await
            .expect("announce");
    }
    wait_for_pins(caller, provider).await;
}

/// Join a FRESH provider to an already-started caller (the mid-call arrival
/// the pin witness needs): pre-accept on the fresh side, connect from the
/// live side, start only the provider.
async fn attach_provider(caller: &Mesh, provider: &Mesh) {
    let provider_pub = *provider.public_key();
    let provider_addr = provider.local_addr().to_string();
    let p_clone = provider.node().clone();
    let caller_id = caller.node_id();
    let accept = tokio::spawn(async move { p_clone.accept(caller_id).await });
    caller
        .connect(&provider_addr, &provider_pub, provider.node_id())
        .await
        .expect("connect");
    accept.await.expect("accept task").expect("accept");
    provider.start();

    for m in [caller, provider] {
        m.node()
            .announce_capabilities(CapabilitySet::new())
            .await
            .expect("announce");
    }
    wait_for_pins(caller, provider).await;
}

async fn wait_for_pins(a: &Mesh, b: &Mesh) {
    let (a_id, b_id) = (a.node_id(), b.node_id());
    for _ in 0..100 {
        if a.node().peer_entity_id(b_id).is_some() && b.node().peer_entity_id(a_id).is_some() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("entity pins were not established in both directions");
}

/// Re-announce until the caller's cold discovery resolves `wanted` providers
/// for `capability` (owner plane + the named grant planes).
async fn converge(
    provider: &Mesh,
    caller: &Mesh,
    capability: &CapabilityAuthorityId,
    discover_grant_ids: &[[u8; 32]],
    wanted: usize,
) -> bool {
    for _ in 0..100 {
        provider
            .node()
            .announce_capabilities(CapabilitySet::new())
            .await
            .ok();
        if let Ok(capture) = caller
            .node()
            .org_cold_discovery(capability, discover_grant_ids)
        {
            let mut resolved = capture.owner_providers().len();
            for id in discover_grant_ids {
                resolved += capture.granted_providers(id).len();
            }
            if resolved >= wanted {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

/// Order two providers the way deterministic selection does (lowest provider
/// entity id, byte-wise) and return `(low, high)`.
fn by_entity_order<'a>(a: &'a Mesh, b: &'a Mesh) -> (&'a Mesh, &'a Mesh) {
    if a.node().entity_id().as_bytes() < b.node().entity_id().as_bytes() {
        (a, b)
    } else {
        (b, a)
    }
}

/// The shared credential set for a caller: membership + wide-open dispatcher,
/// optionally holding a cross-org grant + its audience secret.
fn caller_credentials(
    org: &OrgKeypair,
    caller_identity: &Identity,
    grant: Option<(OrgCapabilityGrant, OrgAudienceSecret)>,
) -> OrgCredentials {
    let (cert, dg) = belonging(org, caller_identity.entity_id());
    let (grants, secrets) = match grant {
        None => (vec![], vec![]),
        Some((g, s)) => (vec![g], vec![s]),
    };
    OrgCredentials::new(cert, dg, grants, secrets).expect("credentials")
}

// ---------------------------------------------------------------------------
// Provider handlers (the frozen core seams) + their verified attribution
// ---------------------------------------------------------------------------

/// The four-party attribution every handler asserts from `ctx.org_admission`.
#[derive(Clone)]
struct Attribution {
    caller: EntityId,
    acting_org: OrgId,
    provider_org: OrgId,
    provider: EntityId,
    capability: CapabilityAuthorityId,
}

impl Attribution {
    fn matches(&self, a: &Admitted) -> bool {
        a.caller == self.caller
            && a.acting_org == self.acting_org
            && a.provider_org == self.provider_org
            && a.provider == self.provider
            && a.capability == self.capability
    }

    /// The same five verified facts as seen by a FACADE handler — the
    /// `OrgCaller` projection. `entity` is the caller's ed25519 entity id
    /// (verified at admission), never the u64 routing `caller_origin`.
    fn matches_caller(&self, c: &OrgCaller) -> bool {
        c.entity == self.caller
            && c.acting_org == self.acting_org
            && c.provider_org == self.provider_org
            && c.provider == self.provider
            && c.capability == self.capability
    }
}

/// Shared per-run counters a witness polls.
#[derive(Clone, Default)]
struct Counters {
    ran: Arc<AtomicUsize>,
    attribution_ok: Arc<AtomicBool>,
    items: Arc<AtomicUsize>,
}

/// Server-streaming: emits `count` items tagged with the provider's node id.
struct FixedStreamProvider {
    count: u32,
    served_by: u64,
    attribution: Attribution,
    counters: Counters,
}

#[async_trait::async_trait]
impl RpcStreamingHandler for FixedStreamProvider {
    async fn call(&self, ctx: RpcContext, sink: RpcResponseSink) -> Result<(), RpcHandlerError> {
        self.counters.ran.fetch_add(1, Ordering::SeqCst);
        if let Some(a) = ctx.org_admission.as_ref() {
            self.counters
                .attribution_ok
                .store(self.attribution.matches(a), Ordering::SeqCst);
        }
        for n in 0..self.count {
            let body = serde_json::to_vec(&Item {
                n,
                served_by: self.served_by,
            })
            .expect("encode item");
            sink.send(Bytes::from(body));
            self.counters.items.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }
}

/// Client-streaming: drains the upload and answers with everything it saw.
struct UploadProvider {
    served_by: u64,
    attribution: Attribution,
    counters: Counters,
}

#[async_trait::async_trait]
impl RpcClientStreamingHandler for UploadProvider {
    async fn call(
        &self,
        ctx: RpcStreamingContext,
        mut requests: RequestStream,
    ) -> Result<RpcResponsePayload, RpcHandlerError> {
        self.counters.ran.fetch_add(1, Ordering::SeqCst);
        if let Some(a) = ctx.org_admission.as_ref() {
            self.counters
                .attribution_ok
                .store(self.attribution.matches(a), Ordering::SeqCst);
        }
        let mut seen = Vec::new();
        while let Some(chunk) = requests.next().await {
            let ping: Ping =
                serde_json::from_slice(&chunk).map_err(|e| RpcHandlerError::Application {
                    code: 0x8001,
                    message: format!("bad request: {e}"),
                })?;
            seen.push(ping.n);
            self.counters.items.fetch_add(1, Ordering::SeqCst);
        }
        let body = serde_json::to_vec(&UploadSummary {
            chunks: seen.len(),
            seen,
            served_by: self.served_by,
        })
        .expect("encode summary");
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: Bytes::from(body),
        })
    }
}

/// Duplex: echoes every upload item as a tagged response item.
struct EchoDuplexProvider {
    served_by: u64,
    attribution: Attribution,
    counters: Counters,
}

#[async_trait::async_trait]
impl RpcDuplexHandler for EchoDuplexProvider {
    async fn call(
        &self,
        ctx: RpcStreamingContext,
        mut requests: RequestStream,
        responses: RpcResponseSink,
    ) -> Result<(), RpcHandlerError> {
        self.counters.ran.fetch_add(1, Ordering::SeqCst);
        if let Some(a) = ctx.org_admission.as_ref() {
            self.counters
                .attribution_ok
                .store(self.attribution.matches(a), Ordering::SeqCst);
        }
        while let Some(chunk) = requests.next().await {
            let ping: Ping =
                serde_json::from_slice(&chunk).map_err(|e| RpcHandlerError::Application {
                    code: 0x8001,
                    message: format!("bad request: {e}"),
                })?;
            let body = serde_json::to_vec(&Item {
                n: ping.n,
                served_by: self.served_by,
            })
            .expect("encode item");
            responses.send(Bytes::from(body));
            self.counters.items.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }
}

fn policy() -> OrgProviderPolicy {
    // The facade's policy is `|_| true`: the provider veto stays the caller's
    // extension point (row 3.2). Providers at THIS row use the same trivial
    // policy so the handler sees exactly what the facade will show it.
    Arc::new(|_| true)
}

fn attribution_for(
    identity: &Identity,
    provider: &Mesh,
    service: &str,
    acting: OrgId,
    provider_org: OrgId,
) -> Attribution {
    Attribution {
        caller: identity.entity_id().clone(),
        acting_org: acting,
        provider_org,
        provider: provider.node().entity_id().clone(),
        capability: cap(&format!("nrpc:{service}")),
    }
}

// ---------------------------------------------------------------------------
// 1. Same-org, server-streaming, through the facade
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_same_org_streaming_through_the_facade() {
    let a = org_a();
    let (provider, _p_identity, p_dir) = fast_mesh("s3-ss-provider", &a, None).await;
    let shared = OwnerAudienceCredential::decode_config(
        &provider
            .node()
            .node_authority()
            .expect("authority")
            .audience
            .encode_config(),
    )
    .expect("copy owner audience");
    let (caller, c_identity, c_dir) = fast_mesh("s3-ss-caller", &a, Some(&shared)).await;
    bring_up(&caller, &provider).await;

    let service = "s3.echo";
    let counters = Counters::default();
    let _serve = provider
        .node()
        .serve_rpc_owner_scoped_streaming(
            service,
            Arc::new(FixedStreamProvider {
                count: 2,
                served_by: provider.node_id(),
                attribution: attribution_for(
                    &c_identity,
                    &provider,
                    service,
                    a.org_id(),
                    a.org_id(),
                ),
                counters: counters.clone(),
            }),
            policy(),
        )
        .expect("serve owner-scoped streaming");

    let credentials = caller_credentials(&a, &c_identity, None);
    let org = caller.org(credentials).expect("bind");
    assert!(
        converge(&provider, &caller, &cap(&format!("nrpc:{service}")), &[], 1).await,
        "the caller privately resolved the provider"
    );

    let mut stream = org
        .call_streaming::<Ping, Item>(service, &Ping { n: 7 })
        .await
        .expect("the protected streaming call is admitted");
    let mut items = Vec::new();
    while let Some(item) = stream.next().await {
        items.push(item.expect("item"));
    }

    assert_eq!(
        items,
        vec![
            Item {
                n: 0,
                served_by: provider.node_id()
            },
            Item {
                n: 1,
                served_by: provider.node_id()
            },
        ],
        "both items come from the planned provider"
    );
    assert_eq!(counters.ran.load(Ordering::SeqCst), 1, "handler ran once");
    assert!(
        counters.attribution_ok.load(Ordering::SeqCst),
        "the handler saw the full verified attribution — the call traversed canonical admission"
    );
    assert_eq!(
        org.last_selected_provider(),
        Some(provider.node().entity_id().clone()),
        "planning selected the provider and never re-resolved"
    );

    let _ = std::fs::remove_dir_all(&p_dir);
    let _ = std::fs::remove_dir_all(&c_dir);
}

// ---------------------------------------------------------------------------
// 2. Same-org, client-streaming — THE PIN WITNESS (a second provider resolves
//    mid-call and must capture nothing)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_same_org_client_stream_through_the_facade() {
    let a = org_a();
    let (m1, _i1, dir1) = fast_mesh("s3-pin-m1", &a, None).await;
    let shared = OwnerAudienceCredential::decode_config(
        &m1.node()
            .node_authority()
            .expect("authority")
            .audience
            .encode_config(),
    )
    .expect("copy owner audience");
    let (m2, _i2, dir2) = fast_mesh("s3-pin-m2", &a, Some(&shared)).await;
    // `low` is the provider deterministic selection would prefer (lowest
    // entity id) — it arrives MID-CALL, so any re-resolution after its arrival
    // must land there for the receipt's mutation to be observable.
    let (low, high) = by_entity_order(&m1, &m2);
    let (caller, c_identity, c_dir) = fast_mesh("s3-pin-caller", &a, Some(&shared)).await;

    let service = "s3.upload";
    let high_counters = Counters::default();
    let _serve_high = high
        .node()
        .serve_rpc_owner_scoped_client_stream(
            service,
            Arc::new(UploadProvider {
                served_by: high.node_id(),
                attribution: attribution_for(&c_identity, high, service, a.org_id(), a.org_id()),
                counters: high_counters.clone(),
            }),
            policy(),
        )
        .expect("serve owner-scoped client-streaming");

    bring_up(&caller, high).await;

    let credentials = caller_credentials(&a, &c_identity, None);
    let org = caller.org(credentials).expect("bind");
    assert!(
        converge(high, &caller, &cap(&format!("nrpc:{service}")), &[], 1).await,
        "precondition: only the pinned provider is resolvable at the verb"
    );

    // ---- the call opens and the first chunk is delivered to `high` ----
    let mut call = org
        .call_client_stream::<Ping, UploadSummary>(service)
        .await
        .expect("plan resolved the provider");
    call.send(&Ping { n: 10 })
        .await
        .expect("first chunk is sent");

    // ---- the scenario resolves a SECOND provider mid-call ----
    let low_counters = Counters::default();
    let _serve_low = low
        .node()
        .serve_rpc_owner_scoped_client_stream(
            service,
            Arc::new(UploadProvider {
                served_by: low.node_id(),
                attribution: attribution_for(&c_identity, low, service, a.org_id(), a.org_id()),
                counters: low_counters.clone(),
            }),
            policy(),
        )
        .expect("serve owner-scoped client-streaming");
    attach_provider(&caller, low).await;
    assert!(
        converge(low, &caller, &cap(&format!("nrpc:{service}")), &[], 2).await,
        "precondition: the second provider resolved mid-call"
    );

    call.send(&Ping { n: 20 })
        .await
        .expect("second chunk is sent");
    let summary = call.finish().await.expect("the upload is admitted");

    // THE named pin assertion — the 3.1 pin receipt's mutation must redden
    // exactly this one ("resolve a second provider mid-call → the pin witness
    // reddens").
    assert_eq!(
        summary,
        UploadSummary {
            chunks: 2,
            seen: vec![10, 20],
            served_by: high.node_id(),
        },
        "the provider is pinned per call: chunk two and the terminal land on \
         the planned provider even though a second provider resolved mid-call"
    );
    assert_eq!(
        low_counters.ran.load(Ordering::SeqCst),
        0,
        "the second provider captured nothing — no re-resolution, no retry"
    );
    assert_eq!(
        high_counters.ran.load(Ordering::SeqCst),
        1,
        "the pinned handler ran once"
    );
    assert!(
        high_counters.attribution_ok.load(Ordering::SeqCst),
        "the pinned handler saw the verified attribution"
    );
    assert_eq!(
        org.last_selected_provider(),
        Some(high.node().entity_id().clone()),
        "exactly one plan for the call — the pinned provider"
    );

    let _ = std::fs::remove_dir_all(&dir1);
    let _ = std::fs::remove_dir_all(&dir2);
    let _ = std::fs::remove_dir_all(&c_dir);
}

// ---------------------------------------------------------------------------
// 3. Same-org, duplex, through the facade
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_same_org_duplex_through_the_facade() {
    let a = org_a();
    let (provider, _p_identity, p_dir) = fast_mesh("s3-dx-provider", &a, None).await;
    let shared = OwnerAudienceCredential::decode_config(
        &provider
            .node()
            .node_authority()
            .expect("authority")
            .audience
            .encode_config(),
    )
    .expect("copy owner audience");
    let (caller, c_identity, c_dir) = fast_mesh("s3-dx-caller", &a, Some(&shared)).await;
    bring_up(&caller, &provider).await;

    let service = "s3.echo.duplex";
    let counters = Counters::default();
    let _serve = provider
        .node()
        .serve_rpc_owner_scoped_duplex(
            service,
            Arc::new(EchoDuplexProvider {
                served_by: provider.node_id(),
                attribution: attribution_for(
                    &c_identity,
                    &provider,
                    service,
                    a.org_id(),
                    a.org_id(),
                ),
                counters: counters.clone(),
            }),
            policy(),
        )
        .expect("serve owner-scoped duplex");

    let credentials = caller_credentials(&a, &c_identity, None);
    let org = caller.org(credentials).expect("bind");
    assert!(
        converge(&provider, &caller, &cap(&format!("nrpc:{service}")), &[], 1).await,
        "the caller privately resolved the provider"
    );

    let mut call = org
        .call_duplex::<Ping, Item>(service)
        .await
        .expect("the protected duplex call is admitted");
    call.send(&Ping { n: 1 }).await.expect("send 1");
    call.send(&Ping { n: 2 }).await.expect("send 2");
    call.finish_sending().await.expect("half-close");
    let mut items = Vec::new();
    while let Some(item) = call.next().await {
        items.push(item.expect("item"));
    }

    assert_eq!(
        items,
        vec![
            Item {
                n: 1,
                served_by: provider.node_id()
            },
            Item {
                n: 2,
                served_by: provider.node_id()
            },
        ],
        "both echoes come from the planned provider"
    );
    assert_eq!(counters.ran.load(Ordering::SeqCst), 1, "handler ran once");
    assert!(
        counters.attribution_ok.load(Ordering::SeqCst),
        "the handler saw the full verified attribution"
    );
    assert_eq!(
        org.last_selected_provider(),
        Some(provider.node().entity_id().clone()),
        "planning selected the provider and never re-resolved"
    );

    let _ = std::fs::remove_dir_all(&p_dir);
    let _ = std::fs::remove_dir_all(&c_dir);
}

// ---------------------------------------------------------------------------
// 4-6. Granted (cross-org) — the same three shapes over the grant plane
// ---------------------------------------------------------------------------

/// The shared granted fixture: org B's provider, org A's caller holding B's
/// DISCOVER|INVOKE grant, both sides provisioned like
/// `live_cross_org_call_through_the_facade`.
struct GrantedFixture {
    provider: Mesh,
    caller: Mesh,
    c_identity: Identity,
    a: OrgKeypair,
    b: OrgKeypair,
    grant_id: [u8; 32],
    grant: OrgCapabilityGrant,
    secret: OrgAudienceSecret,
    dirs: Vec<std::path::PathBuf>,
}

async fn granted_fixture(tag: &str) -> GrantedFixture {
    let (a, b) = (org_a(), org_b());
    let (provider, _p_identity, p_dir) = fast_mesh(&format!("{tag}-provider"), &b, None).await;
    let (caller, c_identity, c_dir) = fast_mesh(&format!("{tag}-caller"), &a, None).await;
    bring_up(&caller, &provider).await;

    let (grant, secret) = discover_grant(&b, a.org_id(), cap("nrpc:customer.read"), 3600);
    let grant_id = grant.grant_id;
    let provider_secret =
        OrgAudienceSecret::decode_config(&secret.encode_config()).expect("copy secret");
    provider
        .node()
        .install_provider_grant_audience(grant.clone(), provider_secret)
        .expect("provider audience");

    GrantedFixture {
        provider,
        caller,
        c_identity,
        a,
        b,
        grant_id,
        grant,
        secret,
        dirs: vec![p_dir, c_dir],
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_cross_org_streaming_through_the_facade() {
    let fx = granted_fixture("s3-x-ss").await;
    let service = "customer.read";
    let counters = Counters::default();
    let _serve = fx
        .provider
        .node()
        .serve_rpc_granted_streaming(
            service,
            Arc::new(FixedStreamProvider {
                count: 2,
                served_by: fx.provider.node_id(),
                attribution: attribution_for(
                    &fx.c_identity,
                    &fx.provider,
                    service,
                    fx.a.org_id(),
                    fx.b.org_id(),
                ),
                counters: counters.clone(),
            }),
            policy(),
        )
        .expect("serve granted streaming");

    // `OrgAudienceSecret` is deliberately non-`Clone`; the fixture's pair is
    // MOVED into this witness's credentials (each witness mints its own).
    let credentials = caller_credentials(&fx.a, &fx.c_identity, Some((fx.grant, fx.secret)));
    let org = fx.caller.org(credentials).expect("bind");
    assert!(
        converge(
            &fx.provider,
            &fx.caller,
            &cap(&format!("nrpc:{service}")),
            &[fx.grant_id],
            1
        )
        .await,
        "the grantee privately resolved the B-owned provider"
    );

    let mut stream = org
        .call_streaming::<Ping, Item>(service, &Ping { n: 3 })
        .await
        .expect("the cross-org streaming call is admitted");
    let mut items = Vec::new();
    while let Some(item) = stream.next().await {
        items.push(item.expect("item"));
    }

    assert_eq!(
        items,
        vec![
            Item {
                n: 0,
                served_by: fx.provider.node_id()
            },
            Item {
                n: 1,
                served_by: fx.provider.node_id()
            },
        ],
        "both items come from the granted provider"
    );
    assert_eq!(counters.ran.load(Ordering::SeqCst), 1, "handler ran once");
    assert!(
        counters.attribution_ok.load(Ordering::SeqCst),
        "four-party attribution reached the handler (S acted for A under B's grant on exact P)"
    );

    for d in fx.dirs {
        let _ = std::fs::remove_dir_all(d);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_cross_org_client_stream_through_the_facade() {
    let fx = granted_fixture("s3-x-cs").await;
    let service = "customer.read";
    let counters = Counters::default();
    let _serve = fx
        .provider
        .node()
        .serve_rpc_granted_client_stream(
            service,
            Arc::new(UploadProvider {
                served_by: fx.provider.node_id(),
                attribution: attribution_for(
                    &fx.c_identity,
                    &fx.provider,
                    service,
                    fx.a.org_id(),
                    fx.b.org_id(),
                ),
                counters: counters.clone(),
            }),
            policy(),
        )
        .expect("serve granted client-streaming");

    let credentials = caller_credentials(&fx.a, &fx.c_identity, Some((fx.grant, fx.secret)));
    let org = fx.caller.org(credentials).expect("bind");
    assert!(
        converge(
            &fx.provider,
            &fx.caller,
            &cap(&format!("nrpc:{service}")),
            &[fx.grant_id],
            1
        )
        .await,
        "the grantee privately resolved the B-owned provider"
    );

    let mut call = org
        .call_client_stream::<Ping, UploadSummary>(service)
        .await
        .expect("plan resolved the granted provider");
    call.send(&Ping { n: 5 }).await.expect("send");
    call.send(&Ping { n: 6 }).await.expect("send");
    let summary = call
        .finish()
        .await
        .expect("the cross-org upload is admitted");

    assert_eq!(
        summary,
        UploadSummary {
            chunks: 2,
            seen: vec![5, 6],
            served_by: fx.provider.node_id(),
        },
        "the granted provider saw the whole upload and answered"
    );
    assert_eq!(counters.ran.load(Ordering::SeqCst), 1, "handler ran once");
    assert!(
        counters.attribution_ok.load(Ordering::SeqCst),
        "four-party attribution reached the handler"
    );

    for d in fx.dirs {
        let _ = std::fs::remove_dir_all(d);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_cross_org_duplex_through_the_facade() {
    let fx = granted_fixture("s3-x-dx").await;
    let service = "customer.read";
    let counters = Counters::default();
    let _serve = fx
        .provider
        .node()
        .serve_rpc_granted_duplex(
            service,
            Arc::new(EchoDuplexProvider {
                served_by: fx.provider.node_id(),
                attribution: attribution_for(
                    &fx.c_identity,
                    &fx.provider,
                    service,
                    fx.a.org_id(),
                    fx.b.org_id(),
                ),
                counters: counters.clone(),
            }),
            policy(),
        )
        .expect("serve granted duplex");

    let credentials = caller_credentials(&fx.a, &fx.c_identity, Some((fx.grant, fx.secret)));
    let org = fx.caller.org(credentials).expect("bind");
    assert!(
        converge(
            &fx.provider,
            &fx.caller,
            &cap(&format!("nrpc:{service}")),
            &[fx.grant_id],
            1
        )
        .await,
        "the grantee privately resolved the B-owned provider"
    );

    let call = org
        .call_duplex::<Ping, Item>(service)
        .await
        .expect("the cross-org duplex call is admitted");
    // `into_split` — the independent halves (upload sink + response stream).
    let (mut sink, mut stream) = call.into_split();
    sink.send(&Ping { n: 8 }).await.expect("send 1");
    sink.send(&Ping { n: 9 }).await.expect("send 2");
    sink.finish_sending().await.expect("half-close");
    let mut items = Vec::new();
    while let Some(item) = stream.next().await {
        items.push(item.expect("item"));
    }

    assert_eq!(
        items,
        vec![
            Item {
                n: 8,
                served_by: fx.provider.node_id()
            },
            Item {
                n: 9,
                served_by: fx.provider.node_id()
            },
        ],
        "both echoes come from the granted provider, via the split halves"
    );
    assert_eq!(counters.ran.load(Ordering::SeqCst), 1, "handler ran once");
    assert!(
        counters.attribution_ok.load(Ordering::SeqCst),
        "four-party attribution reached the handler"
    );

    for d in fx.dirs {
        let _ = std::fs::remove_dir_all(d);
    }
}

// ---------------------------------------------------------------------------
// 7. A facade streaming call against a unary-only provider is refused with the
//    typed NotSupported coarse denial — no public fallback, no retry
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn facade_stream_against_unary_only_provider_is_not_supported() {
    let a = org_a();
    let (provider, _p_identity, p_dir) = fast_mesh("s3-unary-provider", &a, None).await;
    let shared = OwnerAudienceCredential::decode_config(
        &provider
            .node()
            .node_authority()
            .expect("authority")
            .audience
            .encode_config(),
    )
    .expect("copy owner audience");
    let (caller, c_identity, c_dir) = fast_mesh("s3-unary-caller", &a, Some(&shared)).await;
    bring_up(&caller, &provider).await;

    // The provider serves ONLY the unary verb for this service.
    let ran = Arc::new(AtomicUsize::new(0));
    let ran_h = ran.clone();
    let _serve = provider
        .serve_org(
            "s3.unary.only",
            OrgAccess::SameOrg,
            move |_c: OrgCaller, req: Ping| {
                let ran = ran_h.clone();
                async move {
                    ran.fetch_add(1, Ordering::SeqCst);
                    Ok(Item {
                        n: req.n,
                        served_by: 0,
                    })
                }
            },
        )
        .expect("serve_org");

    let credentials = caller_credentials(&a, &c_identity, None);
    let org = caller.org(credentials).expect("bind");
    assert!(
        converge(&provider, &caller, &cap("nrpc:s3.unary.only"), &[], 1).await,
        "the caller resolved the unary provider"
    );

    let mut stream = org
        .call_streaming::<Ping, Item>("s3.unary.only", &Ping { n: 1 })
        .await
        .expect("the opening itself is well-formed");
    match stream.next().await {
        Some(Err(OrgSdkError::AdmissionDenied(CoarseAdmissionReason::NotSupported))) => {}
        other => panic!(
            "a facade stream against a unary-only provider must end as \
             AdmissionDenied(NotSupported); got {other:?}"
        ),
    }
    assert!(
        stream.next().await.is_none(),
        "the refusal is the stream's terminal item"
    );
    assert_eq!(
        ran.load(Ordering::SeqCst),
        0,
        "the unary handler never ran — no fallback shape, no retry"
    );

    let _ = std::fs::remove_dir_all(&p_dir);
    let _ = std::fs::remove_dir_all(&c_dir);
}

// ---------------------------------------------------------------------------
// 8. Dropping the stream emits exactly one cancel: the dropped call retires
//    (handler cancelled, emission frozen, fold keys drained) and a sibling
//    stream is untouched
// ---------------------------------------------------------------------------

/// Ticks until cancelled; the FIRST call lands on `first`'s counters, later
/// calls on `second`'s — one registration serves both streams.
struct DuoTickProvider {
    first: Counters,
    second: Counters,
    served_by: u64,
    attribution: Attribution,
    runs: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl RpcStreamingHandler for DuoTickProvider {
    async fn call(&self, ctx: RpcContext, sink: RpcResponseSink) -> Result<(), RpcHandlerError> {
        let run = self.runs.fetch_add(1, Ordering::SeqCst);
        let counters = if run == 0 {
            self.first.clone()
        } else {
            self.second.clone()
        };
        counters.ran.fetch_add(1, Ordering::SeqCst);
        if let Some(a) = ctx.org_admission.as_ref() {
            counters
                .attribution_ok
                .store(self.attribution.matches(a), Ordering::SeqCst);
        }
        let mut n: u32 = 0;
        loop {
            tokio::select! {
                // Cooperative stop. The handler-side observation is NOT a
                // witness instrument: on the protected path the handler
                // future runs inside the retire supervisor and is dropped at
                // the retirement without a guaranteed final poll (§2.2
                // `forced` — cooperation is best-effort by contract).
                _ = ctx.cancellation.cancelled() => return Ok(()),
                _ = tokio::time::sleep(Duration::from_millis(50)) => {
                    let body = serde_json::to_vec(&Item { n, served_by: self.served_by })
                        .expect("encode item");
                    sink.send(Bytes::from(body));
                    counters.items.fetch_add(1, Ordering::SeqCst);
                    n += 1;
                }
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dropping_org_stream_emits_one_cancel() {
    let a = org_a();
    let (provider, _p_identity, p_dir) = fast_mesh("s3-drop-provider", &a, None).await;
    let shared = OwnerAudienceCredential::decode_config(
        &provider
            .node()
            .node_authority()
            .expect("authority")
            .audience
            .encode_config(),
    )
    .expect("copy owner audience");
    let (caller, c_identity, c_dir) = fast_mesh("s3-drop-caller", &a, Some(&shared)).await;
    bring_up(&caller, &provider).await;

    let service = "s3.forever";
    let dropped_counters = Counters::default();
    let sibling_counters = Counters::default();
    let runs = Arc::new(AtomicUsize::new(0));
    let serve = provider
        .node()
        .serve_rpc_owner_scoped_streaming(
            service,
            Arc::new(DuoTickProvider {
                first: dropped_counters.clone(),
                second: sibling_counters.clone(),
                served_by: provider.node_id(),
                attribution: attribution_for(
                    &c_identity,
                    &provider,
                    service,
                    a.org_id(),
                    a.org_id(),
                ),
                runs,
            }),
            policy(),
        )
        .expect("serve owner-scoped streaming");
    let fold = serve
        .streaming_fold_for_test()
        .expect("fixtures expose the streaming fold");

    let credentials = caller_credentials(&a, &c_identity, None);
    let org = caller.org(credentials).expect("bind");
    assert!(
        converge(&provider, &caller, &cap(&format!("nrpc:{service}")), &[], 1).await,
        "the caller resolved the provider"
    );

    // Stream A is opened first (it lands on `first`'s counters); the sibling
    // second. Both are live before the drop.
    let mut dropped = org
        .call_streaming::<Ping, Item>(service, &Ping { n: 1 })
        .await
        .expect("stream A is admitted");
    dropped.next().await.expect("A item").expect("A ok");
    dropped.next().await.expect("A item").expect("A ok");
    let mut sibling = org
        .call_streaming::<Ping, Item>(service, &Ping { n: 2 })
        .await
        .expect("stream B is admitted");
    sibling.next().await.expect("B item").expect("B ok");
    sibling.next().await.expect("B item").expect("B ok");
    assert_eq!(
        fold.lock().in_flight_keys().len(),
        2,
        "precondition: both calls hold in-flight records"
    );

    drop(dropped);

    // The drop's ONE cancel reached the provider and retired EXACTLY the one
    // dropped call: its in-flight record leaves the fold and the sibling's
    // stays. Without the cancel the dropped record would linger (red here);
    // a cancel that tore down both calls would leave zero (also red here).
    let drain_deadline = std::time::Instant::now() + Duration::from_secs(3);
    while fold.lock().in_flight_keys().len() != 1 && std::time::Instant::now() < drain_deadline {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(
        fold.lock().in_flight_keys().len(),
        1,
        "dropping_org_stream_emits_one_cancel: the drop's one cancel retires \
         exactly the one dropped call at the provider (its in-flight record \
         leaves; the sibling's stays)"
    );

    // Output emission froze at the retirement (no zombie producer).
    let emitted_at_retire = dropped_counters.items.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(
        dropped_counters.items.load(Ordering::SeqCst),
        emitted_at_retire,
        "the retired handler stopped emitting"
    );

    // The sibling is healthy and keeps producing after the drop — the one
    // cancel targeted the one call. The production sample spans several tick
    // periods (the queue can satisfy one `next()` instantly).
    let sibling_before = sibling_counters.items.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(
        sibling_counters.items.load(Ordering::SeqCst) > sibling_before,
        "and its handler is still producing — no stray cancel reached it"
    );
    let after = sibling
        .next()
        .await
        .expect("sibling item")
        .expect("sibling ok");
    assert_eq!(
        after,
        Item {
            n: 2,
            served_by: provider.node_id()
        },
        "the sibling stream continues unaffected"
    );

    // The retired record stays retired (the count is exact, not a moment).
    assert_eq!(
        fold.lock().in_flight_keys().len(),
        1,
        "exactly one record retired — the sibling's is still live"
    );

    let _ = std::fs::remove_dir_all(&p_dir);
    let _ = std::fs::remove_dir_all(&c_dir);
}

// ---------------------------------------------------------------------------
// 9. Row 3.2 — the facade PROVIDER rows: the handler receives the verified
//    `OrgCaller` projection (all three shapes), never caller-claimed identity
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn handler_receives_verified_org_caller_not_origin() {
    let a = org_a();
    let (provider, _p_identity, p_dir) = fast_mesh("s3-prov-provider", &a, None).await;
    let shared = OwnerAudienceCredential::decode_config(
        &provider
            .node()
            .node_authority()
            .expect("authority")
            .audience
            .encode_config(),
    )
    .expect("copy owner audience");
    let (caller, c_identity, c_dir) = fast_mesh("s3-prov-caller", &a, Some(&shared)).await;
    bring_up(&caller, &provider).await;

    let (ss_service, cs_service, dx_service) =
        ("s3.prov.stream", "s3.prov.upload", "s3.prov.duplex");
    let served_by = provider.node_id();
    let (ss_ok, cs_ok, dx_ok) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    );

    let ss_attr = attribution_for(&c_identity, &provider, ss_service, a.org_id(), a.org_id());
    let ss_check = ss_ok.clone();
    let _ss = provider
        .serve_org_streaming(
            ss_service,
            OrgAccess::SameOrg,
            move |c: OrgCaller, req: Ping, sink: ResponseSinkTyped<Item>| {
                let (ss_attr, ss_check) = (ss_attr.clone(), ss_check.clone());
                async move {
                    // The five verified facts — entity included. The facade
                    // never shows `caller_origin`, so what the handler sees
                    // can only be the admission-verified identity (the ed25519
                    // entity id — never the u64 routing hash).
                    ss_check.store(ss_attr.matches_caller(&c), Ordering::SeqCst);
                    sink.send(&Item {
                        n: req.n,
                        served_by,
                    })
                }
            },
        )
        .expect("serve_org_streaming");

    let cs_attr = attribution_for(&c_identity, &provider, cs_service, a.org_id(), a.org_id());
    let cs_check = cs_ok.clone();
    let _cs = provider
        .serve_org_client_stream(
            cs_service,
            OrgAccess::SameOrg,
            move |c: OrgCaller, mut requests: RequestStreamTyped<Ping>| {
                let (cs_attr, cs_check) = (cs_attr.clone(), cs_check.clone());
                async move {
                    cs_check.store(cs_attr.matches_caller(&c), Ordering::SeqCst);
                    let mut seen = Vec::new();
                    while let Some(item) = requests.next().await {
                        let ping = item.map_err(|e| format!("decode: {e}"))?;
                        seen.push(ping.n);
                    }
                    Ok(UploadSummary {
                        chunks: seen.len(),
                        seen,
                        served_by,
                    })
                }
            },
        )
        .expect("serve_org_client_stream");

    let dx_attr = attribution_for(&c_identity, &provider, dx_service, a.org_id(), a.org_id());
    let dx_check = dx_ok.clone();
    let _dx = provider
        .serve_org_duplex(
            dx_service,
            OrgAccess::SameOrg,
            move |c: OrgCaller,
                  mut requests: RequestStreamTyped<Ping>,
                  sink: ResponseSinkTyped<Item>| {
                let (dx_attr, dx_check) = (dx_attr.clone(), dx_check.clone());
                async move {
                    dx_check.store(dx_attr.matches_caller(&c), Ordering::SeqCst);
                    while let Some(item) = requests.next().await {
                        let ping = item.map_err(|e| format!("decode: {e}"))?;
                        sink.send(&Item {
                            n: ping.n,
                            served_by,
                        })?;
                    }
                    Ok(())
                }
            },
        )
        .expect("serve_org_duplex");

    let credentials = caller_credentials(&a, &c_identity, None);
    let org = caller.org(credentials).expect("bind");
    for service in [ss_service, cs_service, dx_service] {
        assert!(
            converge(&provider, &caller, &cap(&format!("nrpc:{service}")), &[], 1).await,
            "the caller resolved the provider for {service}"
        );
    }

    let mut stream = org
        .call_streaming::<Ping, Item>(ss_service, &Ping { n: 4 })
        .await
        .expect("streaming call");
    assert_eq!(
        stream.next().await.expect("item").expect("ok"),
        Item { n: 4, served_by },
        "streaming round trip"
    );

    let mut call = org
        .call_client_stream::<Ping, UploadSummary>(cs_service)
        .await
        .expect("client-stream call");
    call.send(&Ping { n: 5 }).await.expect("send");
    assert_eq!(
        call.finish().await.expect("terminal"),
        UploadSummary {
            chunks: 1,
            seen: vec![5],
            served_by,
        },
        "client-stream round trip"
    );

    let mut dx = org
        .call_duplex::<Ping, Item>(dx_service)
        .await
        .expect("duplex call");
    dx.send(&Ping { n: 6 }).await.expect("send");
    dx.finish_sending().await.expect("half-close");
    assert_eq!(
        dx.next().await.expect("item").expect("ok"),
        Item { n: 6, served_by },
        "duplex round trip"
    );

    assert!(
        ss_ok.load(Ordering::SeqCst),
        "handler_receives_verified_org_caller_not_origin: the streaming \
         handler's OrgCaller is the admission-verified five-field projection \
         (the ed25519 entity id — never the caller_origin routing hash)"
    );
    assert!(
        cs_ok.load(Ordering::SeqCst),
        "the client-streaming handler's OrgCaller is the verified projection"
    );
    assert!(
        dx_ok.load(Ordering::SeqCst),
        "the duplex handler's OrgCaller is the verified projection"
    );

    let _ = std::fs::remove_dir_all(&p_dir);
    let _ = std::fs::remove_dir_all(&c_dir);
}

// ---------------------------------------------------------------------------
// 10. Row 3.2 — a mid-stream revocation surfaces as the stream's FINAL
//     `AdmissionDenied(Denied)` item (the frozen Revoked → Denied coarse map)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revocation_surfaces_as_final_admission_denied_item() {
    let a = org_a();
    let (provider, _p_identity, p_dir) = fast_mesh("s3-revoke-provider", &a, None).await;
    let shared = OwnerAudienceCredential::decode_config(
        &provider
            .node()
            .node_authority()
            .expect("authority")
            .audience
            .encode_config(),
    )
    .expect("copy owner audience");
    let (caller, c_identity, c_dir) = fast_mesh("s3-revoke-caller", &a, Some(&shared)).await;
    bring_up(&caller, &provider).await;

    let service = "s3.revoke";
    let served_by = provider.node_id();
    let ran = Arc::new(AtomicUsize::new(0));
    let ran_h = ran.clone();
    // The handler ticks forever; the retirement force-drops it (§2.2
    // `forced`) — exactly the contract under test.
    let _serve = provider
        .serve_org_streaming(
            service,
            OrgAccess::SameOrg,
            move |_c: OrgCaller, _req: Ping, sink: ResponseSinkTyped<Item>| {
                let ran = ran_h.clone();
                async move {
                    ran.fetch_add(1, Ordering::SeqCst);
                    let mut n: u32 = 0;
                    loop {
                        sink.send(&Item { n, served_by })?;
                        n += 1;
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
            },
        )
        .expect("serve_org_streaming");

    let credentials = caller_credentials(&a, &c_identity, None);
    let org = caller.org(credentials).expect("bind");
    assert!(
        converge(&provider, &caller, &cap(&format!("nrpc:{service}")), &[], 1).await,
        "the caller resolved the provider"
    );

    let mut stream = org
        .call_streaming::<Ping, Item>(service, &Ping { n: 1 })
        .await
        .expect("the streaming call is admitted");
    let first = stream.next().await.expect("item").expect("live item");
    assert_eq!(
        first,
        Item { n: 0, served_by },
        "precondition: the stream is live and delivering"
    );

    // Revoke the CALLER mid-stream: its membership is generation 1, so a
    // floor of 2 revokes it. The raise publishes synchronously through the
    // provider's REAL store — the active stream retires at the boundary,
    // before `apply_bundle` returns.
    let mut floors = std::collections::BTreeMap::new();
    floors.insert(c_identity.entity_id().clone(), 2u32);
    let bundle = OrgRevocationBundle::try_issue(&a, &floors).expect("revocation bundle");
    provider
        .node()
        .org_revocation_store()
        .expect("installed revocation store")
        .apply_bundle(&bundle)
        .expect("apply the raised floor");

    let mut final_item = None;
    while let Some(item) = stream.next().await {
        final_item = Some(item);
    }
    match final_item {
        Some(Err(OrgSdkError::AdmissionDenied(CoarseAdmissionReason::Denied))) => {}
        other => panic!(
            "revocation_surfaces_as_final_admission_denied_item: the stream \
             must end as AdmissionDenied(Denied) (Revoked's frozen coarse \
             byte); got {other:?}"
        ),
    }
    assert_eq!(
        ran.load(Ordering::SeqCst),
        1,
        "the handler ran once and was retired by the raise"
    );

    let _ = std::fs::remove_dir_all(&p_dir);
    let _ = std::fs::remove_dir_all(&c_dir);
}
