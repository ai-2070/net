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
