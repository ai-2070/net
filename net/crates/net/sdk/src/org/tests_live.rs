//! OSDK S3 — the composed exit witness.
//!
//! Two live nodes, real transport, both access modes, through the two verbs and
//! nothing else:
//!
//! ```ignore
//! let org = mesh.org(credentials)?;
//! let result: Pong = org.call("customer.read", &request).await?;
//! ```
//!
//! Each test asserts the call actually traversed canonical admission — the
//! handler receives the five-field `Admitted` projection and the raw
//! `net-org-admission` header is gone — rather than assuming a successful
//! response implies it.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::{ChannelConfigRegistry, MeshNode, MeshNodeConfig};

use super::credentials::OrgCredentials;
use super::serve::{OrgAccess, OrgCaller};
use super::tests::{belonging, cap, discover_grant, org_a, org_b};
use super::types::*;
use super::OrgSdkError;
use crate::identity::Identity;
use crate::mesh::Mesh;

#[derive(serde::Serialize, serde::Deserialize)]
struct Ping {
    n: u32,
}

#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct Pong {
    n: u32,
    served_by: String,
}

/// Handshake two meshes and wait for both entity pins — protected RPC is
/// direct-session-only, so the pins ARE the precondition.
async fn bring_up(caller: &Mesh, provider: &Mesh) {
    let provider_pub = *provider.public_key();
    let provider_addr = provider.local_addr();
    let caller_id = caller.node_id();
    let p = provider.node();
    let p_clone = p.clone();
    let accept = tokio::spawn(async move { p_clone.accept(caller_id).await });
    caller
        .connect(
            &provider_addr.to_string(),
            &provider_pub,
            provider.node_id(),
        )
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
    let provider_id = provider.node_id();
    for _ in 0..100 {
        if caller.node().peer_entity_id(provider_id).is_some()
            && provider.node().peer_entity_id(caller_id).is_some()
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("entity pins were not established in both directions");
}

/// Drive propagation to convergence.
///
/// Emission is asynchronous and the scoped envelope ships on its own
/// subprotocol, so re-announce and poll the caller's own authority decision
/// until the provider actually resolves — the same shape the core's live
/// granted-discovery witness uses.
async fn converge_discovery(
    provider: &Mesh,
    client: &super::OrgClient,
    capability: &CapabilityAuthorityId,
) -> bool {
    for _ in 0..100 {
        provider
            .node()
            .announce_capabilities(CapabilitySet::new())
            .await
            .ok();
        if client
            .authorized_targets(capability)
            .map(|(targets, _)| !targets.is_empty())
            .unwrap_or(false)
        {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

/// A mesh whose node re-announces promptly, adopted into `owner`'s org.
///
/// The scoped envelope ships on the announce path, and the default
/// `min_announce_interval` is 10 s — far longer than a test should wait — so
/// this builds the node directly with a short interval and wraps it via the
/// public `Mesh::from_node_arc` seam rather than adding an SDK builder knob
/// that exists only for tests.
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
    // The caller IS supplying an identity here (below), so record that
    // provenance the way MeshBuilder::identity would.
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
        "net-osdk-s3-{tag}-{}-{:?}",
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

/// A configured mesh whose authority is installed through the PRODUCTION path a
/// binding takes — `NodeAuthority::adopt` writes the dir (the `net node adopt`
/// ceremony), then `Mesh::install_org_authority(dir)` opens and installs it.
///
/// Distinct from `fast_mesh`, which installs the in-memory authority directly.
/// This proves the dir-loading path (§7) actually composes into a working call.
async fn provisioned_mesh(tag: &str, owner: &OrgKeypair) -> (Mesh, Identity, std::path::PathBuf) {
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

    // The ADOPTION ceremony writes the authority files (operator/CLI step).
    let cert = OrgMembershipCert::try_issue(owner, entity.clone(), 1, 3600).expect("cert");
    let dir = std::env::temp_dir().join(format!(
        "net-osdk-provision-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = NodeAuthority::adopt(&dir, cert, &entity, 0, None).expect("adopt writes the dir");

    let mesh = Mesh::from_node_arc(node, channel_configs, Some(identity.clone()));
    // The STARTUP step a binding performs: load the adopted files and install.
    mesh.install_org_authority(&dir)
        .expect("install_org_authority loads and installs the adopted dir");
    // Owner-cert emission needs the authority installed first.
    mesh.node()
        .set_owner_cert_emission(true)
        .expect("enable owner-cert emission");
    (mesh, identity, dir)
}

/// Assertions a protected handler makes about its own admission.
struct Facts {
    ran: Arc<AtomicUsize>,
    attribution_ok: Arc<AtomicBool>,
}

impl Facts {
    fn new() -> Self {
        Self {
            ran: Arc::new(AtomicUsize::new(0)),
            attribution_ok: Arc::new(AtomicBool::new(false)),
        }
    }
}

// ---------------------------------------------------------------------------
// The composed example — SameOrg
// ---------------------------------------------------------------------------

/// Same-organization: one org, a provider and a dispatcher, over live
/// transport. The provider serves privately; the caller discovers privately and
/// invokes — through `mesh.org(..)` and `serve_org(..)` alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_same_org_call_through_the_facade() {
    let a = org_a();
    let (provider, _p_identity, p_dir) = fast_mesh("live-same-provider", &a, None).await;
    // The caller shares the organization's owner audience — §3.4's out-of-band
    // pre-staging step, without which it could not open the envelope at all.
    let shared = OwnerAudienceCredential::decode_config(
        &provider
            .node()
            .node_authority()
            .expect("authority")
            .audience
            .encode_config(),
    )
    .expect("copy owner audience");
    let (caller, c_identity, c_dir) = fast_mesh("live-same-caller", &a, Some(&shared)).await;
    bring_up(&caller, &provider).await;

    let facts = Facts::new();
    let expected_caller = c_identity.entity_id().clone();
    let expected_provider = provider.node().entity_id().clone();
    let ran = facts.ran.clone();
    let ok = facts.attribution_ok.clone();
    let org_id = a.org_id();
    let _serve = provider
        .serve_org(
            "internal.reindex",
            OrgAccess::SameOrg,
            move |c: OrgCaller, req: Ping| {
                let (ran, ok) = (ran.clone(), ok.clone());
                let (expected_caller, expected_provider) =
                    (expected_caller.clone(), expected_provider.clone());
                async move {
                    ran.fetch_add(1, Ordering::SeqCst);
                    // The five verified facts, none caller-claimed.
                    ok.store(
                        c.entity == expected_caller
                            && c.acting_org == org_id
                            && c.provider_org == org_id
                            && c.provider == expected_provider
                            && c.capability == cap("nrpc:internal.reindex")
                            && c.is_same_org(),
                        Ordering::SeqCst,
                    );
                    Ok(Pong {
                        n: req.n + 1,
                        served_by: "provider".to_string(),
                    })
                }
            },
        )
        .expect("serve_org");

    // ---- the composed example ----
    let (cert, dg) = belonging(&a, c_identity.entity_id());
    let credentials = OrgCredentials::new(cert, dg, vec![], vec![]).expect("credentials");
    let org = caller.org(credentials).expect("bind");
    assert!(
        converge_discovery(&provider, &org, &cap("nrpc:internal.reindex")).await,
        "the caller privately resolved the provider over the live scoped send"
    );
    let pong: Pong = org
        .call("internal.reindex", &Ping { n: 41 })
        .await
        .expect("the protected call is admitted");
    // ------------------------------

    assert_eq!(
        pong,
        Pong {
            n: 42,
            served_by: "provider".to_string()
        }
    );
    assert_eq!(
        facts.ran.load(Ordering::SeqCst),
        1,
        "handler ran exactly once"
    );
    assert!(
        facts.attribution_ok.load(Ordering::SeqCst),
        "the handler saw the full verified attribution — the call traversed canonical admission"
    );

    let _ = std::fs::remove_dir_all(&p_dir);
    let _ = std::fs::remove_dir_all(&c_dir);
}

// ---------------------------------------------------------------------------
// The composed example — Granted (cross-org)
// ---------------------------------------------------------------------------

/// Cross-organization: org B's provider serves a capability B granted to org A,
/// announced only inside the grant audience. A's dispatcher discovers it
/// privately and invokes it. Four-party attribution is asserted in the handler.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_cross_org_call_through_the_facade() {
    let (a, b) = (org_a(), org_b());
    let (provider, _p_identity, p_dir) = fast_mesh("live-x-provider", &b, None).await;
    let (caller, c_identity, c_dir) = fast_mesh("live-x-caller", &a, None).await;
    bring_up(&caller, &provider).await;

    let facts = Facts::new();
    let expected_caller = c_identity.entity_id().clone();
    let expected_provider = provider.node().entity_id().clone();
    let ran = facts.ran.clone();
    let ok = facts.attribution_ok.clone();
    let (a_id, b_id) = (a.org_id(), b.org_id());
    let _serve = provider
        .serve_org(
            "customer.read",
            OrgAccess::Granted,
            move |c: OrgCaller, req: Ping| {
                let (ran, ok) = (ran.clone(), ok.clone());
                let (expected_caller, expected_provider) =
                    (expected_caller.clone(), expected_provider.clone());
                async move {
                    ran.fetch_add(1, Ordering::SeqCst);
                    // Four-party attribution: S acted for A, under B's grant,
                    // on exact provider P — never "A invoked B".
                    ok.store(
                        c.entity == expected_caller
                            && c.acting_org == a_id
                            && c.provider_org == b_id
                            && c.provider == expected_provider
                            && c.capability == cap("nrpc:customer.read")
                            && !c.is_same_org(),
                        Ordering::SeqCst,
                    );
                    Ok(Pong {
                        n: req.n + 1,
                        served_by: "b-provider".to_string(),
                    })
                }
            },
        )
        .expect("serve_org");

    // B grants A discover+invoke over the capability on any B-owned node, and
    // provisions the provider side (the contract: registration first, audience
    // after).
    let (grant, secret) = discover_grant(&b, a.org_id(), cap("nrpc:customer.read"), 3600);
    let provider_secret =
        OrgAudienceSecret::decode_config(&secret.encode_config()).expect("copy secret");
    provider
        .node()
        .install_provider_grant_audience(grant.clone(), provider_secret)
        .expect("provider audience");

    // ---- the composed example ----
    let (cert, dg) = belonging(&a, c_identity.entity_id());
    let credentials =
        OrgCredentials::new(cert, dg, vec![grant], vec![secret]).expect("credentials");
    let org = caller.org(credentials).expect("bind");
    assert!(
        converge_discovery(&provider, &org, &cap("nrpc:customer.read")).await,
        "the grantee privately resolved the B-owned provider over the live scoped send"
    );
    let pong: Pong = org
        .call("customer.read", &Ping { n: 7 })
        .await
        .expect("the cross-org protected call is admitted");
    // ------------------------------

    assert_eq!(
        pong,
        Pong {
            n: 8,
            served_by: "b-provider".to_string()
        }
    );
    assert_eq!(
        facts.ran.load(Ordering::SeqCst),
        1,
        "handler ran exactly once"
    );
    assert!(
        facts.attribution_ok.load(Ordering::SeqCst),
        "four-party attribution reached the handler — canonical admission ran"
    );

    let _ = std::fs::remove_dir_all(&p_dir);
    let _ = std::fs::remove_dir_all(&c_dir);
}

/// A caller whose org holds NO grant for the capability cannot even discover
/// the cross-org provider: the encrypted announcement is opaque without the
/// audience, so the facade refuses locally and sends nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_cross_org_without_a_grant_discovers_nothing() {
    let (a, b) = (org_a(), org_b());
    let (provider, _p_identity, p_dir) = fast_mesh("live-nogrant-provider", &b, None).await;
    let (caller, c_identity, c_dir) = fast_mesh("live-nogrant-caller", &a, None).await;
    bring_up(&caller, &provider).await;

    let ran = Arc::new(AtomicUsize::new(0));
    let ran_h = ran.clone();
    let _serve = provider
        .serve_org(
            "customer.read",
            OrgAccess::Granted,
            move |_c: OrgCaller, req: Ping| {
                let ran = ran_h.clone();
                async move {
                    ran.fetch_add(1, Ordering::SeqCst);
                    Ok(Pong {
                        n: req.n,
                        served_by: "b".to_string(),
                    })
                }
            },
        )
        .expect("serve_org");

    // Provision the PROVIDER side only — the caller holds no grant or secret.
    let (grant, secret) = discover_grant(&b, a.org_id(), cap("nrpc:customer.read"), 3600);
    provider
        .node()
        .install_provider_grant_audience(grant, secret)
        .expect("provider audience");
    for _ in 0..10 {
        provider
            .node()
            .announce_capabilities(CapabilitySet::new())
            .await
            .ok();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let (cert, dg) = belonging(&a, c_identity.entity_id());
    let org = caller
        .org(OrgCredentials::new(cert, dg, vec![], vec![]).expect("credentials"))
        .expect("bind");

    let err = org
        .call::<_, Pong>("customer.read", &Ping { n: 1 })
        .await
        .expect_err("nothing discoverable without the audience");
    assert!(
        matches!(err, OrgSdkError::Discovery(_)),
        "refused locally, before anything was sent — got {err:?}"
    );
    assert_eq!(
        ran.load(Ordering::SeqCst),
        0,
        "the provider's handler stayed dark"
    );

    let _ = std::fs::remove_dir_all(&p_dir);
    let _ = std::fs::remove_dir_all(&c_dir);
}

/// A LIVE remote denial: local checks pass, the provider refuses.
///
/// This is the distinction the whole error hierarchy exists for. The caller's
/// membership is revoked at the PROVIDER (a floor the caller has no way to
/// know about), so the facade's local pre-flight passes, the request is
/// actually sent, and admission refuses it — surfacing as
/// `OrgSdkError::AdmissionDenied` with the coarse wire reason rather than a
/// credential error or a transport error.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_a_provider_side_revocation_surfaces_as_admission_denied() {
    use net::adapter::net::behavior::org::OrgRevocationBundle;

    let a = org_a();
    let (provider, _p_identity, p_dir) = fast_mesh("live-revoked-provider", &a, None).await;
    let shared = OwnerAudienceCredential::decode_config(
        &provider
            .node()
            .node_authority()
            .expect("authority")
            .audience
            .encode_config(),
    )
    .expect("copy owner audience");
    let (caller, c_identity, c_dir) = fast_mesh("live-revoked-caller", &a, Some(&shared)).await;
    bring_up(&caller, &provider).await;

    let ran = Arc::new(AtomicUsize::new(0));
    let ran_h = ran.clone();
    let _serve = provider
        .serve_org(
            "internal.reindex",
            OrgAccess::SameOrg,
            move |_c: OrgCaller, req: Ping| {
                let ran = ran_h.clone();
                async move {
                    ran.fetch_add(1, Ordering::SeqCst);
                    Ok(Pong {
                        n: req.n,
                        served_by: "p".to_string(),
                    })
                }
            },
        )
        .expect("serve_org");

    // The caller's credentials are minted at generation 1 and stay locally
    // valid for the whole test.
    let (cert, dg) = belonging(&a, c_identity.entity_id());
    let org = caller
        .org(OrgCredentials::new(cert, dg, vec![], vec![]).expect("credentials"))
        .expect("bind");
    assert!(
        converge_discovery(&provider, &org, &cap("nrpc:internal.reindex")).await,
        "the caller resolved the provider before revocation"
    );

    // The org raises a floor to generation 2 for the caller, and ONLY the
    // provider imports the bundle — exactly the split that makes this a remote
    // decision the caller cannot anticipate.
    let mut floors = std::collections::BTreeMap::new();
    floors.insert(c_identity.entity_id().clone(), 2u32);
    let bundle = OrgRevocationBundle::try_issue(&a, &floors).expect("floors");
    provider
        .node()
        .node_authority()
        .expect("authority")
        .revocation
        .apply_bundle(&bundle)
        .expect("provider imports the floor");

    let err = org
        .call::<_, Pong>("internal.reindex", &Ping { n: 1 })
        .await
        .expect_err("the provider refuses the revoked membership");
    match err {
        OrgSdkError::AdmissionDenied(reason) => {
            // Coarse by design — a precise reason would be a credential oracle.
            assert_eq!(
                reason,
                CoarseAdmissionReason::Denied,
                "coarse denial bucket"
            );
        }
        other => panic!("expected a remote admission denial, got {other:?}"),
    }
    assert_eq!(
        ran.load(Ordering::SeqCst),
        0,
        "the handler stayed dark — admission refused before dispatch"
    );

    let _ = std::fs::remove_dir_all(&p_dir);
    let _ = std::fs::remove_dir_all(&c_dir);
}

/// R1: the typed verb IS the raw verb plus JSON.
///
/// Proved by crossing the seams: a TYPED `serve_org` handler answers a
/// hand-written-JSON `call_bytes`, and a RAW `serve_org_bytes` handler answers
/// a typed `call`. If the codec layer did anything beyond marshaling — a
/// different framing, an extra header, a second authority step — one direction
/// would fail.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_raw_and_typed_seams_interoperate() {
    let a = org_a();
    let (provider, _p_identity, p_dir) = fast_mesh("live-r1-provider", &a, None).await;
    let shared = OwnerAudienceCredential::decode_config(
        &provider
            .node()
            .node_authority()
            .expect("authority")
            .audience
            .encode_config(),
    )
    .expect("copy owner audience");
    let (caller, c_identity, c_dir) = fast_mesh("live-r1-caller", &a, Some(&shared)).await;
    bring_up(&caller, &provider).await;

    // A TYPED handler…
    let _typed = provider
        .serve_org(
            "typed.svc",
            OrgAccess::SameOrg,
            |_c: OrgCaller, req: Ping| async move {
                Ok(Pong {
                    n: req.n + 1,
                    served_by: "typed".to_string(),
                })
            },
        )
        .expect("typed serve");

    // …and a RAW handler that does its own JSON, asserting it sees the same
    // verified facts the typed one does.
    let saw_caller = Arc::new(AtomicBool::new(false));
    let saw = saw_caller.clone();
    let org_id = a.org_id();
    let _raw = provider
        .serve_org_bytes(
            "raw.svc",
            OrgAccess::SameOrg,
            move |c: OrgCaller, body: bytes::Bytes| {
                let saw = saw.clone();
                async move {
                    saw.store(c.acting_org == org_id && c.is_same_org(), Ordering::SeqCst);
                    let req: Ping = serde_json::from_slice(&body).map_err(|e| {
                        crate::org::OrgHandlerError::Application {
                            code: 0x8000,
                            message: format!("bad body: {e}"),
                        }
                    })?;
                    let out = serde_json::to_vec(&Pong {
                        n: req.n + 1,
                        served_by: "raw".to_string(),
                    })
                    .expect("encode");
                    Ok(bytes::Bytes::from(out))
                }
            },
        )
        .expect("raw serve");

    let (cert, dg) = belonging(&a, c_identity.entity_id());
    let org = caller
        .org(OrgCredentials::new(cert, dg, vec![], vec![]).expect("credentials"))
        .expect("bind");
    assert!(converge_discovery(&provider, &org, &cap("nrpc:typed.svc")).await);
    assert!(converge_discovery(&provider, &org, &cap("nrpc:raw.svc")).await);

    // Raw caller → typed handler. The bytes are exactly what the codec emits.
    let raw_reply = org
        .call_bytes(
            "typed.svc",
            bytes::Bytes::from(serde_json::to_vec(&Ping { n: 1 }).expect("encode")),
        )
        .await
        .expect("call_bytes reaches the typed handler");
    let decoded: Pong = serde_json::from_slice(&raw_reply).expect("decode");
    assert_eq!(decoded.n, 2);
    assert_eq!(decoded.served_by, "typed");

    // Typed caller → raw handler.
    let typed_reply: Pong = org
        .call("raw.svc", &Ping { n: 10 })
        .await
        .expect("typed call reaches the raw handler");
    assert_eq!(typed_reply.n, 11);
    assert_eq!(typed_reply.served_by, "raw");
    assert!(
        saw_caller.load(Ordering::SeqCst),
        "the raw handler receives the same verified OrgCaller as the typed one"
    );

    let _ = std::fs::remove_dir_all(&p_dir);
    let _ = std::fs::remove_dir_all(&c_dir);
}

/// **Workstream R's acceptance witness — the binding rehearsal.**
///
/// Drives the entire facade the way a language binding will, and only that way:
///
/// - credentials arrive as canonical wire BYTES plus an audience-secret file
///   PATH (`from_parts`), so no in-memory secret and no Rust-typed credential
///   is constructed by the "application";
/// - the provider registers through `serve_org_bytes` and the caller invokes
///   through `call_bytes`, so no generic crosses the seam;
/// - the handler still receives the five verified `OrgCaller` facts;
/// - and it runs cross-org over real two-node transport, so the audience-secret
///   file is genuinely load-bearing — without it the caller discovers nothing.
///
/// If this passes, the path every binding will take is proven to work before
/// any binding exists. If it fails, no amount of napi/PyO3/cgo marshaling would
/// have saved it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_binding_rehearsal_from_files_through_the_raw_seams() {
    use std::io::Write;

    let (a, b) = (org_a(), org_b());
    let (provider, _p_identity, p_dir) = fast_mesh("live-rehearsal-provider", &b, None).await;
    let (caller, c_identity, c_dir) = fast_mesh("live-rehearsal-caller", &a, None).await;
    bring_up(&caller, &provider).await;

    // ---- provider: the RAW seam, doing its own JSON like a binding does ----
    let facts_ok = Arc::new(AtomicBool::new(false));
    let seen = facts_ok.clone();
    let expected_caller = c_identity.entity_id().clone();
    let (a_id, b_id) = (a.org_id(), b.org_id());
    let _serve = provider
        .serve_org_bytes(
            "customer.read",
            OrgAccess::Granted,
            move |c: OrgCaller, body: bytes::Bytes| {
                let seen = seen.clone();
                let expected_caller = expected_caller.clone();
                async move {
                    seen.store(
                        c.entity == expected_caller
                            && c.acting_org == a_id
                            && c.provider_org == b_id
                            && c.capability == cap("nrpc:customer.read")
                            && !c.is_same_org(),
                        Ordering::SeqCst,
                    );
                    let req: Ping = serde_json::from_slice(&body).map_err(|e| {
                        crate::org::OrgHandlerError::Application {
                            code: 0x8000,
                            message: format!("bad body: {e}"),
                        }
                    })?;
                    let out = serde_json::to_vec(&Pong {
                        n: req.n * 2,
                        served_by: "rehearsal".to_string(),
                    })
                    .expect("encode");
                    Ok(bytes::Bytes::from(out))
                }
            },
        )
        .expect("raw serve");

    // ---- operator: issue the grant, provision both sides out of band ----
    let (grant, secret) = discover_grant(&b, a.org_id(), cap("nrpc:customer.read"), 3600);
    let provider_secret =
        OrgAudienceSecret::decode_config(&secret.encode_config()).expect("copy for provider");
    provider
        .node()
        .install_provider_grant_audience(grant.clone(), provider_secret)
        .expect("provider audience");

    // The grantee's copy lands on DISK, 0600 — the only way a binding can
    // supply it, because the key never crosses a language boundary.
    let secret_dir = std::env::temp_dir().join(format!(
        "net-osdk-rehearsal-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&secret_dir);
    std::fs::create_dir_all(&secret_dir).expect("secret dir");
    let secret_path = secret_dir.join("customer-read.audience");
    {
        let mut f = std::fs::File::create(&secret_path).expect("create");
        f.write_all(&secret.encode_config()).expect("write");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&secret_path, std::fs::Permissions::from_mode(0o600))
            .expect("chmod 0600");
    }
    // The application's own copy of the secret is dropped here; from now on the
    // key exists only inside Rust, loaded from the file.
    drop(secret);

    // ---- caller: bytes + a path, exactly the binding constructor ----
    let (cert, dg) = belonging(&a, c_identity.entity_id());
    let credentials = OrgCredentials::from_parts(
        &cert.to_bytes(),
        &dg.to_bytes(),
        &[grant.to_bytes()],
        &[secret_path],
    )
    .expect("credentials load from wire bytes + a secret file");

    let org = caller.org(credentials).expect("bind");
    assert!(
        converge_discovery(&provider, &org, &cap("nrpc:customer.read")).await,
        "the file-loaded audience secret really did enable private discovery"
    );

    // ---- the call: raw bytes in, raw bytes out ----
    let reply = org
        .call_bytes(
            "customer.read",
            bytes::Bytes::from(serde_json::to_vec(&Ping { n: 21 }).expect("encode")),
        )
        .await
        .expect("the cross-org protected call is admitted");
    let pong: Pong = serde_json::from_slice(&reply).expect("decode");

    assert_eq!(pong.n, 42);
    assert_eq!(pong.served_by, "rehearsal");
    assert!(
        facts_ok.load(Ordering::SeqCst),
        "four-party attribution reached the RAW handler — canonical admission ran"
    );

    let _ = std::fs::remove_dir_all(&secret_dir);
    let _ = std::fs::remove_dir_all(&p_dir);
    let _ = std::fs::remove_dir_all(&c_dir);
}

// ---------------------------------------------------------------------------
// §7 provisioning — the methods that make the org surface usable from a binding
// ---------------------------------------------------------------------------

/// `install_org_authority(dir)` loads an adopted directory and satisfies the
/// bind precondition. Before it, `mesh.org` fails `NodeAuthorityRequired`;
/// after it, the same credentials get past that check.
///
/// This is THE gap that made the bindings non-functional: without an authority
/// install path, a Node/Python `mesh.org(..)` could never succeed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn install_org_authority_unblocks_binding() {
    let a = org_a();
    // A configured mesh with NO authority yet.
    let identity = Identity::generate();
    let mut cfg = MeshNodeConfig::new("127.0.0.1:0".parse().expect("addr"), [0x51u8; 32]);
    cfg.configured_identity = true;
    let mut node = MeshNode::new((**identity.keypair()).clone(), cfg)
        .await
        .expect("node");
    let ccfg = Arc::new(ChannelConfigRegistry::new());
    node.set_channel_configs(ccfg.clone());
    let node = Arc::new(node);
    let entity = identity.entity_id().clone();
    let mesh = Mesh::from_node_arc(node, ccfg, Some(identity.clone()));

    let creds = || {
        let (cert, dg) = belonging(&a, &entity);
        OrgCredentials::new(cert, dg, vec![], vec![]).expect("assembles")
    };

    // No authority yet → refused for exactly that reason.
    let err = mesh.org(creds()).expect_err("no authority installed");
    assert!(
        matches!(
            err,
            OrgSdkError::Credentials(crate::org::OrgCredentialError::NodeAuthorityRequired)
        ),
        "got {err:?}"
    );

    // Adopt a dir for THIS entity, then install through the production path.
    let cert = OrgMembershipCert::try_issue(&a, entity.clone(), 1, 3600).expect("cert");
    let dir = std::env::temp_dir().join(format!(
        "net-osdk-install-auth-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = NodeAuthority::adopt(&dir, cert, &entity, 0, None).expect("adopt");

    mesh.install_org_authority(&dir).expect("install succeeds");
    // Now the SAME credentials get past the authority check (to member binding,
    // which passes because the membership names this entity).
    mesh.org(creds())
        .expect("binds after the authority is installed");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `install_org_authority` refuses a directory adopted for a DIFFERENT entity —
/// `NodeAuthority::open` self-verifies against this node's identity, so a
/// binding cannot install someone else's authority.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn install_org_authority_refuses_a_foreign_directory() {
    let a = org_a();
    let identity = Identity::generate();
    let mut cfg = MeshNodeConfig::new("127.0.0.1:0".parse().expect("addr"), [0x51u8; 32]);
    cfg.configured_identity = true;
    let mut node = MeshNode::new((**identity.keypair()).clone(), cfg)
        .await
        .expect("node");
    let ccfg = Arc::new(ChannelConfigRegistry::new());
    node.set_channel_configs(ccfg.clone());
    let node = Arc::new(node);
    let mesh = Mesh::from_node_arc(node, ccfg, Some(identity.clone()));

    // Adopt a dir for a STRANGER entity, not this node.
    let stranger = net::adapter::net::identity::EntityKeypair::generate()
        .entity_id()
        .clone();
    let cert = OrgMembershipCert::try_issue(&a, stranger.clone(), 1, 3600).expect("cert");
    let dir = std::env::temp_dir().join(format!(
        "net-osdk-foreign-auth-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = NodeAuthority::adopt(&dir, cert, &stranger, 0, None).expect("adopt");

    mesh.install_org_authority(&dir)
        .expect_err("an authority adopted for another entity must be refused");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `install_provider_grant_audience(bytes, path)` — the provider-side method a
/// granted `serve_org` needs — installs from wire bytes plus a secret file, and
/// refuses a mismatched secret.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn install_provider_grant_audience_round_trips_from_bytes_and_a_path() {
    use std::io::Write;

    let (a, b) = (org_a(), org_b());
    let (mesh, _identity, dir) = provisioned_mesh("provider-audience", &b).await;
    let provider_entity = mesh.node().entity_id().clone();

    // B grants A discover+invoke on this provider.
    let (grant, secret) = OrgCapabilityGrant::try_issue(
        &b,
        a.org_id(),
        cap("nrpc:customer.read"),
        GrantRights::INVOKE.union(GrantRights::DISCOVER),
        GrantTargetScope::ExactNode(provider_entity),
        3600,
    )
    .expect("grant");
    let secret = secret.expect("discover mints a secret");

    let sdir = std::env::temp_dir().join(format!(
        "net-osdk-prov-secret-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&sdir);
    std::fs::create_dir_all(&sdir).expect("mkdir");
    let secret_path = sdir.join("g.audience");
    {
        let mut f = std::fs::File::create(&secret_path).expect("create");
        f.write_all(&secret.encode_config()).expect("write");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&secret_path, std::fs::Permissions::from_mode(0o600))
            .expect("chmod");
    }

    mesh.install_provider_grant_audience(&grant.to_bytes(), &secret_path)
        .expect("installs from bytes + a path");
    assert_eq!(mesh.node().provider_grant_audiences_len_for_test(), 1);

    // A secret for a DIFFERENT grant is refused.
    let (_other, other_secret) = discover_grant(&b, a.org_id(), cap("nrpc:other"), 3600);
    let other_path = sdir.join("other.audience");
    {
        let mut f = std::fs::File::create(&other_path).expect("create");
        f.write_all(&other_secret.encode_config()).expect("write");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&other_path, std::fs::Permissions::from_mode(0o600))
            .expect("chmod");
    }
    mesh.install_provider_grant_audience(&grant.to_bytes(), &other_path)
        .expect_err("a secret that is not this grant's key must be refused");

    let _ = std::fs::remove_dir_all(&sdir);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The whole binding-shaped path, live: a provider provisioned entirely through
/// the §7 methods (`install_org_authority` + `install_provider_grant_audience`)
/// serves a granted capability that a caller — also provisioned via
/// `install_org_authority`, binding a grant + secret FILE — invokes.
///
/// Every provisioning step here is exactly what a Node or Python app does; no
/// direct `install_node_authority` / in-memory secret is used.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_cross_org_call_through_the_provisioning_methods() {
    use std::io::Write;

    let (a, b) = (org_a(), org_b());
    let (provider, _p_id, p_dir) = provisioned_mesh("prov-provider", &b).await;
    let (caller, c_identity, c_dir) = provisioned_mesh("prov-caller", &a).await;
    bring_up(&caller, &provider).await;

    let ran = Arc::new(AtomicUsize::new(0));
    let ran_h = ran.clone();
    let _serve = provider
        .serve_org(
            "customer.read",
            OrgAccess::Granted,
            move |_c: OrgCaller, req: Ping| {
                let ran = ran_h.clone();
                async move {
                    ran.fetch_add(1, Ordering::SeqCst);
                    Ok(Pong {
                        n: req.n + 100,
                        served_by: "prov".to_string(),
                    })
                }
            },
        )
        .expect("serve_org");

    // Issue the grant; write BOTH sides' secrets to files (the binding shape).
    let (grant, secret) = discover_grant(&b, a.org_id(), cap("nrpc:customer.read"), 3600);
    let sdir = std::env::temp_dir().join(format!(
        "net-osdk-prov-live-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&sdir);
    std::fs::create_dir_all(&sdir).expect("mkdir");
    let write_secret = |name: &str, s: &OrgAudienceSecret| {
        let path = sdir.join(name);
        let mut f = std::fs::File::create(&path).expect("create");
        f.write_all(&s.encode_config()).expect("write");
        drop(f);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        }
        path
    };
    let provider_secret_path = write_secret("provider.audience", &secret);
    let caller_secret_path = write_secret("caller.audience", &secret);

    // Provider provisions its grant audience from bytes + a path.
    provider
        .install_provider_grant_audience(&grant.to_bytes(), &provider_secret_path)
        .expect("provider audience from the §7 method");

    // Caller binds a grant + secret FILE — the whole point of the asymmetry.
    let (cert, dg) = belonging(&a, c_identity.entity_id());
    let credentials = OrgCredentials::from_parts(
        &cert.to_bytes(),
        &dg.to_bytes(),
        &[grant.to_bytes()],
        &[caller_secret_path],
    )
    .expect("credentials from files");
    let org = caller.org(credentials).expect("bind");
    assert!(
        converge_discovery(&provider, &org, &cap("nrpc:customer.read")).await,
        "resolved through a provider provisioned entirely via the §7 methods"
    );

    let pong: Pong = org
        .call("customer.read", &Ping { n: 5 })
        .await
        .expect("the fully binding-shaped cross-org call is admitted");
    assert_eq!(pong.n, 105);
    assert_eq!(pong.served_by, "prov");
    assert_eq!(ran.load(Ordering::SeqCst), 1);

    let _ = std::fs::remove_dir_all(&sdir);
    let _ = std::fs::remove_dir_all(&p_dir);
    let _ = std::fs::remove_dir_all(&c_dir);
}

// ---------------------------------------------------------------------------
// The design test
// ---------------------------------------------------------------------------

/// OSDK's acceptance criterion: the common path must work without the user
/// knowing the substrate's vocabulary. This function is the whole caller-side
/// API surface an application touches, and it names NONE of:
///
/// `OrgProofIntent`, `OwnerDelegated`, `CrossOrgGranted`, the
/// `OrgAudienceSecret` commitment, `ScopedCapabilityAnnouncement`,
/// `VerifiedScopedCapability`, `CoarseAdmissionReason`, `GrantTargetScope`.
///
/// It compiles, so the claim is checked rather than asserted. (Credentials
/// arrive from the operator's issuance tooling; assembling them is not part of
/// the calling surface.)
#[allow(dead_code)]
async fn design_test_the_secure_path_is_short(
    mesh: &Mesh,
    credentials: OrgCredentials,
) -> Result<Pong, OrgSdkError> {
    let org = mesh.org(credentials)?;
    org.call("customer.read", &Ping { n: 1 }).await
}

/// The provider half of the same claim.
#[allow(dead_code)]
fn design_test_the_serve_path_is_short(mesh: &Mesh) -> Result<crate::mesh_rpc::ServeHandle, ()> {
    mesh.serve_org(
        "customer.read",
        OrgAccess::Granted,
        |caller: OrgCaller, req: Ping| async move {
            let _ = caller.acting_org;
            Ok(Pong {
                n: req.n,
                served_by: "p".to_string(),
            })
        },
    )
    .map_err(|_| ())
}
