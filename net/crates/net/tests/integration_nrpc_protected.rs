//! End-to-end nRPC PROTECTED-admission integration test (#47 live tail).
//!
//! Two real `MeshNode`s in one process, connected via a direct handshake and
//! mutually entity-pinned through signed capability announcements. The provider
//! (P), owned by org B, installs a node authority and serves a PROTECTED unary
//! service; the caller issues a real `MeshNode::call(...)` over the actual
//! transport. This is the LIVE path Kyra required — caller publication →
//! provider gate → handler attribution → response — NOT the private
//! `sign_admission_proof` + `deliver_rpc_inbound_for_test` injection the unit
//! witnesses use.
//!
//! Covers:
//!   * owner-delegated admit → handler runs once, four-party attribution exact,
//!     raw proof header stripped, caller receives the reply;
//!   * missing proof (a public call to a protected service) → handler stays
//!     dark, caller receives `RpcStatus::AdmissionDenied` (0x0009) carrying
//!     exactly one coarse reason byte, with NO timeout substitution.

#![cfg(all(feature = "net", feature = "cortex"))]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::behavior::org::{OrgId, OrgKeypair, OrgMembershipCert, OrgRevocationBundle};
use net::adapter::net::behavior::org_admission::OrgAdmission;
use net::adapter::net::behavior::org_authority::NodeAuthority;
use net::adapter::net::behavior::org_grant::OrgAudienceSecret;
use net::adapter::net::behavior::org_grant::{
    CapabilityAuthorityId, DispatcherScope, GrantRights, GrantTargetScope, OrgCapabilityGrant,
    OrgDispatcherGrant,
};
use net::adapter::net::behavior::org_grant_registry::{
    GrantAudienceInstallError, GrantAudienceInstalled,
};
use net::adapter::net::behavior::CapabilityAnnouncement;
use net::adapter::net::cortex::{
    RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus,
};
use net::adapter::net::identity::EntityId;
use net::adapter::net::mesh_rpc::{CallOptions, OrgProofIntent, RpcError, ServeError, ServeHandle};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};

const PSK: [u8; 32] = [0x42u8; 32];
const TEST_BUFFER_SIZE: usize = 256 * 1024;
/// The proof header the provider strips before the handler sees the request.
const ORG_ADMISSION_HEADER: &str = "net-org-admission";

fn test_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(3, Duration::from_secs(2))
        .with_capability_gc_interval(Duration::from_millis(250));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: TEST_BUFFER_SIZE,
        recv_buffer_size: TEST_BUFFER_SIZE,
    };
    cfg
}

async fn build_node_with(keypair: EntityKeypair) -> Arc<MeshNode> {
    Arc::new(
        MeshNode::new(keypair, test_config())
            .await
            .expect("MeshNode::new"),
    )
}

/// Direct handshake: `a` (the connect initiator) → `b`, then start both.
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

/// Like [`build_node_with`] but with a short `min_announce_interval`, so a
/// re-announce inside a tight test loop actually broadcasts instead of being
/// coalesced away under the default 10 s rate limit (the multi-hop relay
/// witness needs P to re-ship promptly after its emission converges).
async fn build_node_fast_announce(keypair: EntityKeypair) -> Arc<MeshNode> {
    let mut cfg = test_config();
    cfg.min_announce_interval = Duration::from_millis(50);
    Arc::new(MeshNode::new(keypair, cfg).await.expect("MeshNode::new"))
}

/// Establish a direct session `initiator → responder` WITHOUT starting either
/// node's dispatch loop — used to wire a multi-hop topology (P—R—C) before
/// bringing all nodes up together, so no node accepts while already running.
async fn connect_no_start(initiator: &Arc<MeshNode>, responder: &Arc<MeshNode>) {
    let r_id = responder.node_id();
    let r_pub = *responder.public_key();
    let r_addr = responder.local_addr();
    let i_id = initiator.node_id();
    let responder_c = responder.clone();
    let accept = tokio::spawn(async move { responder_c.accept(i_id).await });
    initiator
        .connect(r_addr, &r_pub, r_id)
        .await
        .expect("connect failed");
    accept
        .await
        .expect("accept task panicked")
        .expect("accept failed");
}

async fn wait_until<F: Fn() -> bool>(limit: Duration, cond: F) -> bool {
    let start = Instant::now();
    while start.elapsed() < limit {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    cond()
}

/// Handshake the pair and drive both signed announcements so each node pins the
/// other's entity — the caller-side proof binding needs `caller.peer_entity_id(
/// server)` and the provider-side `resolve_direct_caller` needs
/// `server.peer_entity_id(caller)`.
async fn bring_up(caller: &Arc<MeshNode>, server: &Arc<MeshNode>) {
    handshake_pair(caller, server).await;
    server
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("server announce");
    caller
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("caller announce");
    let caller_id = caller.node_id();
    let server_id = server.node_id();
    assert!(
        wait_until(Duration::from_secs(5), || {
            caller.peer_entity_id(server_id).is_some() && server.peer_entity_id(caller_id).is_some()
        })
        .await,
        "entity pins established in both directions",
    );
}

/// Give `server` an org-B node authority so it can serve a PROTECTED service.
/// Returns org B (the caller mints its proof under it) and the scratch dir.
fn install_authority(server: &Arc<MeshNode>, tag: &str) -> (OrgKeypair, std::path::PathBuf) {
    let node_entity = server.entity_id().clone();
    let org_b = OrgKeypair::from_bytes([0x42u8; 32]);
    let node_cert =
        OrgMembershipCert::try_issue(&org_b, node_entity.clone(), 1, 3600).expect("node cert");
    let dir = std::env::temp_dir().join(format!("net-oa2-live-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let authority =
        NodeAuthority::adopt(&dir, node_cert, &node_entity, 0, None).expect("adopt authority");
    server
        .install_node_authority(Arc::new(authority))
        .expect("install authority");
    (org_b, dir)
}

/// Fold a hand-built restrictive `nrpc:<service>` announcement into each node's
/// capability index (sidestepping broadcast), so the caller-side `may_execute`
/// gate observes an allow-list that admits ONLY `allowed_nodes`. `version` must
/// exceed `serve_rpc`'s auto self-index (v=1/2) to supersede it — use e.g. 100.
fn fold_restrictive_announcement(
    nodes: &[&Arc<MeshNode>],
    target: &Arc<MeshNode>,
    version: u64,
    tag: &str,
    allowed_nodes: Vec<u64>,
) {
    let caps = CapabilitySet::new().add_tag(tag);
    let mut ann =
        CapabilityAnnouncement::new(target.node_id(), target.entity_id().clone(), version, caps);
    ann.allowed_nodes = allowed_nodes;
    for n in nodes {
        n.test_inject_capability_announcement(ann.clone());
    }
}

/// An owner-delegated intent for `caller_kp` (a member of org B) targeting
/// `provider` on `nrpc:<service>`.
fn owner_delegated_intent(
    caller_kp: EntityKeypair,
    org_b: &OrgKeypair,
    provider: EntityId,
    service: &str,
) -> OrgProofIntent {
    owner_delegated_intent_gen(caller_kp, org_b, provider, service, 1)
}

/// Like [`owner_delegated_intent`] but with an explicit membership `generation`
/// — used to drive the revocation floor (a below-floor cert is denied).
fn owner_delegated_intent_gen(
    caller_kp: EntityKeypair,
    org_b: &OrgKeypair,
    provider: EntityId,
    service: &str,
    generation: u32,
) -> OrgProofIntent {
    let caller_entity = caller_kp.entity_id().clone();
    let cap = CapabilityAuthorityId::for_tag(&format!("nrpc:{service}"));
    let membership = OrgMembershipCert::try_issue(org_b, caller_entity.clone(), generation, 3600)
        .expect("membership");
    let dispatcher =
        OrgDispatcherGrant::try_issue(org_b, caller_entity, DispatcherScope::Exact(cap), 3600)
            .expect("dispatcher");
    OrgProofIntent {
        caller: Arc::new(caller_kp),
        membership,
        dispatcher,
        capability_grant: None,
        acting_org: org_b.org_id(),
        provider_owner_org: org_b.org_id(),
        provider,
        capability: cap,
        proof_ttl_secs: 30,
    }
}

/// Records the admission attribution the protected handler observes.
struct AdmitHandler {
    calls: Arc<AtomicUsize>,
    saw_admission: Arc<AtomicBool>,
    attribution_ok: Arc<AtomicBool>,
    proof_stripped: Arc<AtomicBool>,
    expected_caller: EntityId,
    expected_acting_org: OrgId,
    expected_provider_org: OrgId,
    expected_provider: EntityId,
    expected_capability: CapabilityAuthorityId,
}

#[async_trait::async_trait]
impl RpcHandler for AdmitHandler {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(admitted) = ctx.org_admission.as_ref() {
            self.saw_admission.store(true, Ordering::SeqCst);
            // ALL FOUR parties plus the exact capability (E1.6) — not just
            // caller + provider (Kyra #47 final).
            if admitted.caller == self.expected_caller
                && admitted.acting_org == self.expected_acting_org
                && admitted.provider_org == self.expected_provider_org
                && admitted.provider == self.expected_provider
                && admitted.capability == self.expected_capability
            {
                self.attribution_ok.store(true, Ordering::SeqCst);
            }
        }
        let stripped = !ctx
            .payload
            .headers
            .iter()
            .any(|(name, _)| name == ORG_ADMISSION_HEADER);
        self.proof_stripped.store(stripped, Ordering::SeqCst);
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: Bytes::from_static(b"pong"),
        })
    }
}

/// A handler that MUST stay dark for a denied call.
struct DarkHandler {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl RpcHandler for DarkHandler {
    async fn call(&self, _ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: Bytes::new(),
        })
    }
}

/// LIVE owner-delegated admit over the real transport: a valid proof is minted
/// by `call`, verified by the provider gate, and the handler runs exactly once
/// with the four-party attribution and the raw proof header stripped; the caller
/// receives the reply.
#[tokio::test]
async fn live_two_node_owner_delegated_admit() {
    const CALLER_SEED: [u8; 32] = [0x07u8; 32];
    let server = build_node_with(EntityKeypair::generate()).await;
    let caller = build_node_with(EntityKeypair::from_bytes(CALLER_SEED)).await;
    bring_up(&caller, &server).await;

    let (org_b, dir) = install_authority(&server, "admit");
    let provider = server.entity_id().clone();
    let caller_entity = caller.entity_id().clone();

    let calls = Arc::new(AtomicUsize::new(0));
    let saw = Arc::new(AtomicBool::new(false));
    let attribution_ok = Arc::new(AtomicBool::new(false));
    let stripped = Arc::new(AtomicBool::new(false));
    let _serve = server
        .serve_rpc_protected(
            "svc",
            Arc::new(AdmitHandler {
                calls: calls.clone(),
                saw_admission: saw.clone(),
                attribution_ok: attribution_ok.clone(),
                proof_stripped: stripped.clone(),
                expected_caller: caller_entity,
                // Owner-delegated: the caller acts for org B, which also owns P.
                expected_acting_org: org_b.org_id(),
                expected_provider_org: org_b.org_id(),
                expected_provider: provider.clone(),
                expected_capability: CapabilityAuthorityId::for_tag("nrpc:svc"),
            }),
            OrgAdmission::OwnerDelegated,
            Arc::new(|_| true),
        )
        .expect("serve protected");

    // The caller node's identity == the intent's caller identity (same seed), so
    // the authenticated session peer matches the proof subject.
    let intent = owner_delegated_intent(
        EntityKeypair::from_bytes(CALLER_SEED),
        &org_b,
        provider,
        "svc",
    );
    let opts = CallOptions {
        org_proof_intent: Some(intent),
        deadline: Some(Instant::now() + Duration::from_secs(5)),
        ..Default::default()
    };
    let reply = caller
        .call(server.node_id(), "svc", Bytes::from_static(b"ping"), opts)
        .await
        .expect("admitted call returns Ok");

    assert_eq!(reply.body.as_ref(), b"pong", "handler reply body");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "handler ran exactly once");
    assert!(
        saw.load(Ordering::SeqCst),
        "handler observed org_admission attribution",
    );
    assert!(
        attribution_ok.load(Ordering::SeqCst),
        "all four attribution parties (caller, acting org, provider org, provider) plus the \
         exact nrpc:svc capability match",
    );
    assert!(
        stripped.load(Ordering::SeqCst),
        "the raw net-org-admission proof header was stripped from the handler view",
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// LIVE deny over the real transport: a public call (no proof) to a PROTECTED
/// service is denied at the gate — the handler stays dark and the caller
/// receives `RpcStatus::AdmissionDenied` (0x0009) carrying exactly one coarse
/// reason byte, NEVER a timeout substitution.
#[tokio::test]
async fn live_two_node_missing_proof_denied() {
    let server = build_node_with(EntityKeypair::generate()).await;
    let caller = build_node_with(EntityKeypair::from_bytes([0x08u8; 32])).await;
    bring_up(&caller, &server).await;
    let (_org_b, dir) = install_authority(&server, "deny");

    let calls = Arc::new(AtomicUsize::new(0));
    let _serve = server
        .serve_rpc_protected(
            "svc",
            Arc::new(DarkHandler {
                calls: calls.clone(),
            }),
            OrgAdmission::OwnerDelegated,
            Arc::new(|_| true),
        )
        .expect("serve protected");

    // No org_proof_intent: an ordinary public call to a protected service. The
    // deadline is a safety net — a correct deny arrives well within it, and if
    // the deny were ever swallowed the resulting Timeout would fail the match
    // arm below (that IS the "no timeout masquerade" assertion).
    let opts = CallOptions {
        deadline: Some(Instant::now() + Duration::from_secs(5)),
        ..Default::default()
    };
    let err = caller
        .call(server.node_id(), "svc", Bytes::from_static(b"ping"), opts)
        .await
        .expect_err("a public call to a protected service must be denied");

    match err {
        RpcError::ServerError {
            status, message, ..
        } => {
            assert_eq!(status, 0x0009, "status is exactly AdmissionDenied (0x0009)");
            assert_eq!(
                message.len(),
                1,
                "the deny body carries exactly one coarse reason byte",
            );
            assert!(
                matches!(message.as_bytes()[0], 0..=2),
                "the single byte is a valid coarse reason (Denied/NotSupported/Unavailable)",
            );
        }
        other => panic!(
            "expected an AdmissionDenied ServerError, got {other:?} \
             (a Timeout here would be a denial masquerading as a timeout)"
        ),
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "the handler stayed dark for the denied call",
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// LIVE provider-state deny: the provider's revocation store is poisoned AFTER
/// registration, so the call-time `verify_provider_authority` self-check fails
/// closed. A VALID owner-delegated proof is denied `Unavailable` (the provider's
/// authority is durability-uncertain), the handler stays dark, and the caller
/// sees 0x0009 — proving the live gate reads current provider state, not
/// registration-time state.
#[tokio::test]
async fn live_two_node_provider_store_poison_denies() {
    const CALLER_SEED: [u8; 32] = [0x09u8; 32];
    let server = build_node_with(EntityKeypair::generate()).await;
    let caller = build_node_with(EntityKeypair::from_bytes(CALLER_SEED)).await;
    bring_up(&caller, &server).await;
    let (org_b, dir) = install_authority(&server, "poison");
    let provider = server.entity_id().clone();

    let calls = Arc::new(AtomicUsize::new(0));
    let _serve = server
        .serve_rpc_protected(
            "svc",
            Arc::new(DarkHandler {
                calls: calls.clone(),
            }),
            OrgAdmission::OwnerDelegated,
            Arc::new(|_| true),
        )
        .expect("serve protected");

    // Poison the provider's store AFTER a healthy registration.
    server
        .org_revocation_store()
        .expect("a revocation store is installed")
        .mark_poisoned_for_test();

    let intent = owner_delegated_intent(
        EntityKeypair::from_bytes(CALLER_SEED),
        &org_b,
        provider,
        "svc",
    );
    let opts = CallOptions {
        org_proof_intent: Some(intent),
        deadline: Some(Instant::now() + Duration::from_secs(5)),
        ..Default::default()
    };
    let err = caller
        .call(server.node_id(), "svc", Bytes::from_static(b"ping"), opts)
        .await
        .expect_err("a poisoned provider store must deny even a valid proof");

    match err {
        RpcError::ServerError {
            status, message, ..
        } => {
            assert_eq!(status, 0x0009, "status is AdmissionDenied (0x0009)");
            assert_eq!(
                message.as_bytes(),
                &[2u8],
                "coarse reason is exactly Unavailable (provider authority unavailable)",
            );
        }
        other => panic!("expected an AdmissionDenied ServerError, got {other:?}"),
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "the handler stayed dark under the poisoned store",
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// LIVE provider-state deny: the captured `provider_policy` is the final live
/// veto (E1.2). A structurally VALID owner-delegated proof whose provider policy
/// returns `false` is denied, the handler stays dark, and the caller sees 0x0009
/// with a single coarse byte — never a timeout.
#[tokio::test]
async fn live_two_node_policy_veto_denies() {
    const CALLER_SEED: [u8; 32] = [0x0au8; 32];
    let server = build_node_with(EntityKeypair::generate()).await;
    let caller = build_node_with(EntityKeypair::from_bytes(CALLER_SEED)).await;
    bring_up(&caller, &server).await;
    let (org_b, dir) = install_authority(&server, "veto");
    let provider = server.entity_id().clone();

    let calls = Arc::new(AtomicUsize::new(0));
    // The provider policy vetoes EVERY proof.
    let _serve = server
        .serve_rpc_protected(
            "svc",
            Arc::new(DarkHandler {
                calls: calls.clone(),
            }),
            OrgAdmission::OwnerDelegated,
            Arc::new(|_| false),
        )
        .expect("serve protected");

    let intent = owner_delegated_intent(
        EntityKeypair::from_bytes(CALLER_SEED),
        &org_b,
        provider,
        "svc",
    );
    let opts = CallOptions {
        org_proof_intent: Some(intent),
        deadline: Some(Instant::now() + Duration::from_secs(5)),
        ..Default::default()
    };
    let err = caller
        .call(server.node_id(), "svc", Bytes::from_static(b"ping"), opts)
        .await
        .expect_err("a vetoing provider policy must deny a valid proof");

    match err {
        RpcError::ServerError {
            status, message, ..
        } => {
            assert_eq!(status, 0x0009, "status is AdmissionDenied (0x0009)");
            assert_eq!(
                message.len(),
                1,
                "the deny body carries exactly one coarse reason byte",
            );
        }
        other => {
            panic!("expected an AdmissionDenied ServerError, got {other:?} (no timeout masquerade)")
        }
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "the handler stayed dark under the policy veto",
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Records whether the handler's request headers carried the org-admission proof.
struct HeaderSpyHandler {
    calls: Arc<AtomicUsize>,
    saw_proof: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl RpcHandler for HeaderSpyHandler {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if ctx
            .payload
            .headers
            .iter()
            .any(|(name, _)| name == ORG_ADMISSION_HEADER)
        {
            self.saw_proof.store(true, Ordering::SeqCst);
        }
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: Bytes::from_static(b"pong"),
        })
    }
}

/// LIVE mixed-version: a PUBLIC service must never deliver org-admission
/// credential material to its handler. The caller attaches a proof (believing
/// the service protected, or a protected→public downgrade), but the #47 public
/// bridge strips the `net-org-admission` header before dispatch — the handler
/// runs, returns its reply, and never sees the proof.
#[tokio::test]
async fn live_two_node_public_handler_never_sees_proof_header() {
    const CALLER_SEED: [u8; 32] = [0x0bu8; 32];
    let server = build_node_with(EntityKeypair::generate()).await;
    let caller = build_node_with(EntityKeypair::from_bytes(CALLER_SEED)).await;
    bring_up(&caller, &server).await;
    // A PUBLIC service needs no authority; org B only mints the stray proof.
    let org_b = OrgKeypair::from_bytes([0x42u8; 32]);
    let provider = server.entity_id().clone();

    let calls = Arc::new(AtomicUsize::new(0));
    let saw_proof = Arc::new(AtomicBool::new(false));
    let _serve = server
        .serve_rpc(
            "pub",
            Arc::new(HeaderSpyHandler {
                calls: calls.clone(),
                saw_proof: saw_proof.clone(),
            }),
        )
        .expect("serve public");

    let intent = owner_delegated_intent(
        EntityKeypair::from_bytes(CALLER_SEED),
        &org_b,
        provider,
        "pub",
    );
    let opts = CallOptions {
        org_proof_intent: Some(intent),
        deadline: Some(Instant::now() + Duration::from_secs(5)),
        ..Default::default()
    };
    let reply = caller
        .call(server.node_id(), "pub", Bytes::from_static(b"ping"), opts)
        .await
        .expect("a public call carrying a stray proof still succeeds");

    assert_eq!(reply.body.as_ref(), b"pong");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the public handler ran once"
    );
    assert!(
        !saw_proof.load(Ordering::SeqCst),
        "the public handler never saw the org-admission proof header (stripped by the bridge)",
    );
}

/// LIVE routing authority split (Kyra #47 final): a PROTECTED `call_service`
/// must route on the ORG PROOF, not the legacy `may_execute` allow-list. The
/// provider advertises `nrpc:svc` with an allow-list that EXCLUDES the caller.
///   * control — WITHOUT a proof intent, `call_service` applies the legacy gate
///     and denies the caller (`CapabilityDenied`); the handler stays dark.
///   * protected — WITH a proof intent, `call_service` bypasses `may_execute`,
///     selects the exact pinned provider, and the live org gate admits — so the
///     handler runs, proving protected routing is consistent with direct
///     protected `call()`.
#[tokio::test]
async fn live_two_node_protected_call_service_bypasses_legacy_gate() {
    const CALLER_SEED: [u8; 32] = [0x0cu8; 32];
    let server = build_node_with(EntityKeypair::generate()).await;
    let caller = build_node_with(EntityKeypair::from_bytes(CALLER_SEED)).await;
    bring_up(&caller, &server).await;
    let (org_b, dir) = install_authority(&server, "callservice");
    let provider = server.entity_id().clone();

    let calls = Arc::new(AtomicUsize::new(0));
    let _serve = server
        .serve_rpc_protected(
            "svc",
            Arc::new(DarkHandler {
                calls: calls.clone(),
            }),
            OrgAdmission::OwnerDelegated,
            Arc::new(|_| true),
        )
        .expect("serve protected");

    // Restrictive announcement folded into the CALLER's index only: the legacy
    // gate admits ONLY the server itself, so the caller is excluded. The server
    // keeps its permissive self-index, so `has_local_capability` (possession)
    // stays true for the protected admit. Version 100 supersedes serve's auto
    // self-index (v=1/2).
    fold_restrictive_announcement(&[&caller], &server, 100, "nrpc:svc", vec![server.node_id()]);

    // Control: no proof intent → the legacy `may_execute` gate denies locally.
    let deny = caller
        .call_service(
            "svc",
            Bytes::from_static(b"ping"),
            CallOptions {
                deadline: Some(Instant::now() + Duration::from_secs(5)),
                ..Default::default()
            },
        )
        .await
        .expect_err("a public call_service must be denied by the legacy allow-list");
    assert!(
        matches!(deny, RpcError::CapabilityDenied { .. }),
        "public call_service is denied by the legacy allow-list, got {deny:?}",
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "the denied public call never reached the handler",
    );

    // Protected: proof intent → call_service bypasses `may_execute`, selects the
    // exact provider, and the live org gate admits.
    let intent = owner_delegated_intent(
        EntityKeypair::from_bytes(CALLER_SEED),
        &org_b,
        provider,
        "svc",
    );
    caller
        .call_service(
            "svc",
            Bytes::from_static(b"ping"),
            CallOptions {
                org_proof_intent: Some(intent),
                deadline: Some(Instant::now() + Duration::from_secs(5)),
                ..Default::default()
            },
        )
        .await
        .expect("protected call_service must bypass the legacy gate and admit");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the protected call bypassed the legacy gate and reached the handler exactly once",
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ============================================================================
// OA3-4b1 confidentiality-exit witnesses (Kyra OA3 closure)
// ============================================================================

/// A trivial handler for the emission witnesses — never actually invoked.
struct TrivialHandler;

#[async_trait::async_trait]
impl RpcHandler for TrivialHandler {
    async fn call(&self, _ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: Bytes::from_static(b"ok"),
        })
    }
}

/// OA3 closure (Kyra #3): registration visibility is AUTHORITATIVE over caller
/// baseline residue. An owner-scoped service's `nrpc:` tag left in the caller
/// baseline must NOT leak into the plaintext announcement, while an unrelated tag
/// survives — proving the subtraction runs on the public builder.
#[tokio::test]
async fn owner_scoped_residue_is_stripped_from_the_plaintext_announcement() {
    let server = build_node_with(EntityKeypair::from_bytes([0x51u8; 32])).await;
    let (_org_b, dir) = install_authority(&server, "residue-strip");

    // Register "secret" OWNER-SCOPED (requires the installed authority). Hold the
    // handle so the registration is not torn down by Drop.
    let _secret = server
        .serve_rpc_owner_scoped("secret", Arc::new(TrivialHandler), Arc::new(|_| true))
        .expect("owner-scoped serve");

    // Announce a baseline that (as a caller might) pre-tags the owner-scoped
    // service AND carries an unrelated tag.
    let baseline = CapabilitySet::new()
        .add_tag("nrpc:secret")
        .add_tag("region:eu-west");
    server
        .announce_capabilities(baseline)
        .await
        .expect("announce");

    // The stable plaintext announcement excludes nrpc:secret but keeps the
    // unrelated tag. (The explicit announce and serve's spawned re-announce both
    // re-derive from user_caps + the registry, converging to the same projection.)
    assert!(
        wait_until(Duration::from_secs(5), || {
            server
                .local_announcement_for_test()
                .map(|a| {
                    !a.capabilities.has_tag("nrpc:secret")
                        && a.capabilities.has_tag("region:eu-west")
                })
                .unwrap_or(false)
        })
        .await,
        "owner-scoped baseline residue stripped from plaintext; unrelated tag kept",
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// OA3-4b1 Commit B2: an owner-scoped service is delivered ONLY inside the
/// encrypted owner-audience envelope on `SUBPROTOCOL_SCOPED_CAPABILITY_ANN`. The
/// plaintext announcement never carries its `nrpc:` tag; the envelope every send
/// path ships decrypts under the node's OWN owner audience to a descriptor that
/// names exactly the owner-scoped service and no public one.
#[tokio::test]
async fn owner_scoped_service_ships_only_inside_the_encrypted_owner_envelope() {
    use net::adapter::net::behavior::org_revocation::OrgRevocationState;
    use net::adapter::net::behavior::org_scoped_ann::ScopedCapabilityAnnouncement;
    use net::adapter::net::behavior::org_scoped_ingest::{
        verify_scoped_ingest, AudienceAuthority, ScopedIngestContext,
    };

    let server = build_node_with(EntityKeypair::from_bytes([0x53u8; 32])).await;
    let (_org_b, dir) = install_authority(&server, "scoped-delivery");
    // The owner envelope embeds the owner cert, so emission must be ENABLED for
    // any scoped envelope to ship (the same switch the public cert rides).
    server
        .set_owner_cert_emission(true)
        .expect("enable owner-cert emission");

    // One owner-scoped (confidential) service and one public service.
    let _secret = server
        .serve_rpc_owner_scoped("secret", Arc::new(TrivialHandler), Arc::new(|_| true))
        .expect("owner-scoped serve");
    let _public = server
        .serve_rpc("open", Arc::new(TrivialHandler))
        .expect("public serve");
    server
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("announce");

    // Converge: the emission every send path reads carries exactly one scoped
    // envelope, and the plaintext keeps the public tag while excluding the
    // owner-scoped one.
    assert!(
        wait_until(Duration::from_secs(5), || {
            server.announcement_scoped_for_send_for_test().len() == 1
                && server
                    .local_announcement_for_test()
                    .map(|a| {
                        a.capabilities.has_tag("nrpc:open")
                            && !a.capabilities.has_tag("nrpc:secret")
                    })
                    .unwrap_or(false)
        })
        .await,
        "one scoped envelope emitted; plaintext keeps nrpc:open, drops nrpc:secret",
    );

    // Decrypt the shipped envelope under the node's OWN owner audience and
    // confirm the sealed descriptor names exactly the owner-scoped service.
    let scoped = server.announcement_scoped_for_send_for_test();
    let envelope =
        ScopedCapabilityAnnouncement::from_bytes(&scoped[0]).expect("decode scoped envelope");
    let authority = server.node_authority().expect("authority installed");
    let audience = AudienceAuthority::owner(authority.owner_org(), &authority.audience);
    let floors = OrgRevocationState::empty();
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    let ctx = ScopedIngestContext {
        local_owner_org: authority.owner_org(),
        floors: &floors,
        now_secs,
        skew_secs: 5,
    };
    let verified = verify_scoped_ingest(&envelope, &audience, &ctx).expect("owner ingest opens");
    let descriptor = CapabilitySet::from_bytes(verified.descriptor()).expect("descriptor caps");
    assert!(
        descriptor.has_tag("nrpc:secret"),
        "the encrypted descriptor names the owner-scoped service",
    );
    assert!(
        !descriptor.has_tag("nrpc:open"),
        "the encrypted descriptor carries only owner-scoped services, never public ones",
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// OA3-4b2 slice 2 — the granted-audience registration seam. `serve_rpc_granted`
/// requires an installed authority; the registered granted service is
/// DISPATCHABLE (present in the self-fold, so `has_local_capability` admits it —
/// the CrossOrgGranted invoke gate would pass this possession check) yet its tag
/// never rides the plaintext broadcast and — with NO provider grant installed —
/// it emits no discovery envelope. That is register-before-grant fail-closed:
/// dispatchable but undiscoverable. `serve_rpc_protected(CrossOrgGranted)` is
/// UNCHANGED — its tag still rides plaintext (public discovery).
#[tokio::test]
async fn serve_rpc_granted_is_dispatchable_but_undiscoverable_without_a_grant() {
    use net::adapter::net::behavior::fold::capability_bridge::has_local_capability;

    // No authority → refused (same gate as serve_rpc_owner_scoped).
    let bare = build_node_with(EntityKeypair::from_bytes([0x62u8; 32])).await;
    assert!(
        matches!(
            bare.serve_rpc_granted("cross", Arc::new(TrivialHandler), Arc::new(|_| true)),
            Err(ServeError::ProtectedAuthorityRequired(_))
        ),
        "a granted registration without authority is refused",
    );

    let server = build_node_with(EntityKeypair::from_bytes([0x63u8; 32])).await;
    let (_org_b, dir) = install_authority(&server, "granted-seam");
    server
        .set_owner_cert_emission(true)
        .expect("enable owner-cert emission");

    // A granted (confidential cross-org) service, a protected-CrossOrgGranted
    // service (public discovery — unchanged behavior), and a public service.
    let _granted = server
        .serve_rpc_granted("cross", Arc::new(TrivialHandler), Arc::new(|_| true))
        .expect("granted serve");
    let _protected = server
        .serve_rpc_protected(
            "prot",
            Arc::new(TrivialHandler),
            OrgAdmission::CrossOrgGranted,
            Arc::new(|_| true),
        )
        .expect("protected serve");
    let _public = server
        .serve_rpc("open", Arc::new(TrivialHandler))
        .expect("public serve");
    server
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("announce");

    // Converge: plaintext keeps the public + protected tags, drops the granted
    // one; with no provider grant installed, no discovery envelope ships.
    assert!(
        wait_until(Duration::from_secs(5), || {
            server
                .local_announcement_for_test()
                .map(|a| {
                    a.capabilities.has_tag("nrpc:open")
                        && a.capabilities.has_tag("nrpc:prot")
                        && !a.capabilities.has_tag("nrpc:cross")
                })
                .unwrap_or(false)
                && server.announcement_scoped_for_send_for_test().is_empty()
        })
        .await,
        "plaintext keeps public+protected, drops granted; no envelope without a grant",
    );

    // Dispatchable: the granted service IS in the self-fold, so
    // has_local_capability admits it (the provider-possession check the
    // CrossOrgGranted callee gate runs before admission).
    assert!(
        has_local_capability(server.capability_fold(), server.node_id(), "nrpc:cross"),
        "the granted service is locally dispatchable despite being undiscoverable",
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// OA3-4b2 slice 3 — granted-audience emission helpers + witnesses.
// ---------------------------------------------------------------------------

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs()
}

/// A byte-identical copy of a secret (install consumes the original; the witness
/// keeps a copy to open the sealed envelope).
fn copy_secret(secret: &OrgAudienceSecret) -> OrgAudienceSecret {
    // Round-trip through the explicit config codec, then SCRUB the temporary
    // key-bearing buffer (volatile write, RAII-equivalent) so the copy leaves no
    // lingering key material on the test stack (OA2-F hygiene bar).
    let mut buf = secret.encode_config();
    let copy = OrgAudienceSecret::decode_config(&buf).expect("copy secret");
    for byte in buf.iter_mut() {
        // SAFETY: `byte` is a valid mutable reference into the owned array.
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
    copy
}

/// An org-B provider node serving one granted-audience service `svc`, authority
/// installed and emission enabled. Returns the node, its serve handle (kept alive
/// by the caller), scratch dir, entity, and org B.
async fn granted_provider(
    seed: u8,
    tag: &str,
    svc: &str,
) -> (
    Arc<MeshNode>,
    ServeHandle,
    std::path::PathBuf,
    EntityId,
    OrgKeypair,
) {
    let p = build_node_with(EntityKeypair::from_bytes([seed; 32])).await;
    let entity = p.entity_id().clone();
    let org_b = OrgKeypair::from_bytes([0x42u8; 32]);
    let cert = OrgMembershipCert::try_issue(&org_b, entity.clone(), 1, 3600).expect("cert");
    let dir = std::env::temp_dir().join(format!("net-oa34b2-emit-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let authority = NodeAuthority::adopt(&dir, cert, &entity, 0, None).expect("adopt");
    p.install_node_authority(Arc::new(authority))
        .expect("install authority");
    p.set_owner_cert_emission(true).expect("enable emission");
    let handle = p
        .serve_rpc_granted(svc, Arc::new(TrivialHandler), Arc::new(|_| true))
        .expect("granted serve");
    (p, handle, dir, entity, org_b)
}

/// Open a granted-audience envelope as grantee `grantee_org` would, returning the
/// sealed descriptor capabilities on success (or `None` if it does not open).
fn open_granted_envelope(
    scoped_bytes: &[u8],
    grant: &OrgCapabilityGrant,
    secret: &OrgAudienceSecret,
    grantee_org: OrgId,
    now_secs: u64,
) -> Option<CapabilitySet> {
    use net::adapter::net::behavior::org_revocation::OrgRevocationState;
    use net::adapter::net::behavior::org_scoped_ann::ScopedCapabilityAnnouncement;
    use net::adapter::net::behavior::org_scoped_ingest::{
        verify_scoped_ingest, AudienceAuthority, ScopedIngestContext,
    };
    let env = ScopedCapabilityAnnouncement::from_bytes(scoped_bytes).ok()?;
    let authority = AudienceAuthority::granted(grant, secret);
    let floors = OrgRevocationState::empty();
    let ctx = ScopedIngestContext {
        local_owner_org: grantee_org,
        floors: &floors,
        now_secs,
        skew_secs: 5,
    };
    let verified = verify_scoped_ingest(&env, &authority, &ctx).ok()?;
    CapabilitySet::from_bytes(verified.descriptor())
}

/// Wait until P's cached emission carries exactly `n` scoped envelopes,
/// re-announcing across the wait so a coalesced send still converges.
async fn converge_scoped_count(p: &Arc<MeshNode>, n: usize) -> bool {
    for _ in 0..40 {
        p.announce_capabilities(CapabilitySet::new()).await.ok();
        if p.announcement_scoped_for_send_for_test().len() == n {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    p.announcement_scoped_for_send_for_test().len() == n
}

/// OA3-4b2 slice 3 — a granted service ships ONLY inside an encrypted grant
/// envelope. With one matching provider grant installed, P emits exactly one
/// granted envelope; its tag never rides the plaintext broadcast; the grantee A
/// opens it under the grant secret and the sealed descriptor names exactly the
/// granted service.
#[tokio::test]
async fn a_granted_service_ships_only_inside_an_encrypted_grant_envelope() {
    let (p, _h, dir, entity, org_b) = granted_provider(0x70, "one", "cross").await;
    let org_a = OrgKeypair::from_bytes([0x7Au8; 32]);

    let (grant, secret) = OrgCapabilityGrant::try_issue(
        &org_b,
        org_a.org_id(),
        CapabilityAuthorityId::for_tag("nrpc:cross"),
        GrantRights::DISCOVER,
        GrantTargetScope::ExactNode(entity.clone()),
        3600,
    )
    .expect("issue grant");
    let secret = secret.expect("secret");
    let opener = copy_secret(&secret);
    assert_eq!(
        p.install_provider_grant_audience(grant.clone(), secret)
            .expect("install"),
        GrantAudienceInstalled::Installed
    );

    // Converge: exactly one scoped envelope (the granted one — no owner service).
    assert!(
        converge_scoped_count(&p, 1).await,
        "P emits exactly one granted envelope",
    );
    // Plaintext never carries the granted tag.
    assert!(
        p.local_announcement_for_test()
            .map(|a| !a.capabilities.has_tag("nrpc:cross"))
            .unwrap_or(false),
        "the granted tag never appears in the plaintext announcement",
    );

    // The grantee A opens it; the descriptor names exactly the granted service.
    let scoped = p.announcement_scoped_for_send_for_test();
    let descriptor = open_granted_envelope(&scoped[0], &grant, &opener, org_a.org_id(), unix_now())
        .expect("grantee opens the granted envelope");
    assert!(descriptor.has_tag("nrpc:cross"));
    assert!(!descriptor.has_tag("nrpc:open"));

    let _ = std::fs::remove_dir_all(&dir);
}

/// OA3-4b2 slice 3 — two overlapping grants (same capability) emit TWO
/// independently-decryptable envelopes, never coalesced. Each grant's key opens
/// ONLY its own envelope: K1 cannot open K2's, and vice versa.
#[tokio::test]
async fn overlapping_grants_emit_two_independently_decryptable_envelopes() {
    let (p, _h, dir, entity, org_b) = granted_provider(0x71, "two", "cross").await;
    let org_a = OrgKeypair::from_bytes([0x7Au8; 32]);
    let cap = CapabilityAuthorityId::for_tag("nrpc:cross");

    let issue = || {
        let (g, s) = OrgCapabilityGrant::try_issue(
            &org_b,
            org_a.org_id(),
            cap,
            GrantRights::DISCOVER,
            GrantTargetScope::ExactNode(entity.clone()),
            3600,
        )
        .expect("issue");
        (g, s.expect("secret"))
    };
    let (g1, s1) = issue();
    let (g2, s2) = issue();
    assert_ne!(g1.grant_id, g2.grant_id, "distinct grant ids");
    let (o1, o2) = (copy_secret(&s1), copy_secret(&s2));
    p.install_provider_grant_audience(g1.clone(), s1)
        .expect("install g1");
    p.install_provider_grant_audience(g2.clone(), s2)
        .expect("install g2");

    // Two overlapping grants → two envelopes, never coalesced.
    assert!(
        converge_scoped_count(&p, 2).await,
        "two overlapping grants emit two envelopes",
    );
    let scoped = p.announcement_scoped_for_send_for_test();
    let now = unix_now();

    use net::adapter::net::behavior::org_scoped_ann::ScopedCapabilityAnnouncement;
    let envs: Vec<ScopedCapabilityAnnouncement> = scoped
        .iter()
        .map(|b| ScopedCapabilityAnnouncement::from_bytes(b).expect("decode"))
        .collect();
    // Locate each grant's envelope by its grant id.
    let e1 = envs
        .iter()
        .find(|e| e.grant_id() == &g1.grant_id)
        .expect("g1 envelope present");
    let e2 = envs
        .iter()
        .find(|e| e.grant_id() == &g2.grant_id)
        .expect("g2 envelope present");

    // Full-path ingest: each grantee opens ONLY its own grant's envelope.
    assert!(open_granted_envelope(&e1.to_bytes(), &g1, &o1, org_a.org_id(), now).is_some());
    assert!(open_granted_envelope(&e2.to_bytes(), &g2, &o2, org_a.org_id(), now).is_some());

    // AEAD key independence: K1 cannot decrypt K2's ciphertext, and vice versa —
    // each grant/key pair is its own boundary, never coalesced under one key.
    assert!(e1.open_with(o1.discovery_key()).is_ok(), "K1 opens E1",);
    assert!(
        e1.open_with(o2.discovery_key()).is_err(),
        "K2 cannot open E1",
    );
    assert!(e2.open_with(o2.discovery_key()).is_ok(), "K2 opens E2",);
    assert!(
        e2.open_with(o1.discovery_key()).is_err(),
        "K1 cannot open E2",
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// OA3-4b2 slice 3 — a provider grant for a capability with NO locally-registered
/// granted service emits no envelope (fanout is per matching capability, not per
/// installed grant).
#[tokio::test]
async fn an_unrelated_capability_grant_emits_no_granted_envelope() {
    let (p, _h, dir, entity, org_b) = granted_provider(0x72, "none", "cross").await;
    let org_a = OrgKeypair::from_bytes([0x7Au8; 32]);

    // A valid grant that covers this provider but names a DIFFERENT capability.
    let (grant, secret) = OrgCapabilityGrant::try_issue(
        &org_b,
        org_a.org_id(),
        CapabilityAuthorityId::for_tag("nrpc:unrelated"),
        GrantRights::DISCOVER,
        GrantTargetScope::ExactNode(entity.clone()),
        3600,
    )
    .expect("issue");
    p.install_provider_grant_audience(grant, secret.expect("secret"))
        .expect("install");

    // Nothing matches the local `nrpc:cross` service → no envelope at all.
    assert!(
        converge_scoped_count(&p, 0).await,
        "an unrelated-capability grant emits no granted envelope",
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// OA3-4b2 slice 3 — a granted envelope's expiry never outlives its grant:
/// `expires_at = min(now + announce_ttl, grant.not_after, cert.not_after)`. A
/// grant with a short TTL clamps the envelope below the (300 s) announce TTL and
/// the (3600 s) cert.
#[tokio::test]
async fn a_granted_envelope_never_outlives_its_grant() {
    let (p, _h, dir, entity, org_b) = granted_provider(0x73, "ttl", "cross").await;
    let org_a = OrgKeypair::from_bytes([0x7Au8; 32]);

    let (grant, secret) = OrgCapabilityGrant::try_issue(
        &org_b,
        org_a.org_id(),
        CapabilityAuthorityId::for_tag("nrpc:cross"),
        GrantRights::DISCOVER,
        GrantTargetScope::ExactNode(entity.clone()),
        120, // far shorter than the 300 s announce TTL and 3600 s cert
    )
    .expect("issue");
    p.install_provider_grant_audience(grant.clone(), secret.expect("secret"))
        .expect("install");
    assert!(converge_scoped_count(&p, 1).await, "one granted envelope");

    let scoped = p.announcement_scoped_for_send_for_test();
    let env =
        net::adapter::net::behavior::org_scoped_ann::ScopedCapabilityAnnouncement::from_bytes(
            &scoped[0],
        )
        .expect("decode");
    assert!(
        env.expires_at() <= grant.not_after,
        "envelope expiry {} must not outlive the grant not_after {}",
        env.expires_at(),
        grant.not_after,
    );
    // The grant TTL (120 s) is the binding constraint, well below now + 300 s.
    assert!(
        env.expires_at() <= unix_now() + 200,
        "the short grant TTL clamped the envelope expiry below the announce TTL",
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// OA3-4b2 slice 3 — removing a provider grant swaps the registry snapshot
/// pointer, so the cached granted envelope can no longer ship: the send seqlock's
/// pointer-eq check refuses it BEFORE any rebuild lands (the mutation woke a
/// rebuild on a started node; here the unstarted node has no `self_weak`, so the
/// refusal is observed deterministically). A fresh announce then republishes with
/// the grant gone — zero envelopes.
#[tokio::test]
async fn removing_a_provider_grant_refuses_the_cached_granted_envelope() {
    let (p, _h, dir, entity, org_b) = granted_provider(0x74, "remove", "cross").await;
    let org_a = OrgKeypair::from_bytes([0x7Au8; 32]);

    let (grant, secret) = OrgCapabilityGrant::try_issue(
        &org_b,
        org_a.org_id(),
        CapabilityAuthorityId::for_tag("nrpc:cross"),
        GrantRights::DISCOVER,
        GrantTargetScope::ExactNode(entity.clone()),
        3600,
    )
    .expect("issue");
    let grant_id = grant.grant_id;
    p.install_provider_grant_audience(grant, secret.expect("secret"))
        .expect("install");
    assert!(converge_scoped_count(&p, 1).await, "one granted envelope");

    // Remove the grant (unstarted node → no auto re-announce). The cached
    // emission still holds the granted envelope sealed under the OLD snapshot,
    // but the send path pointer-eq check now refuses it: the send returns None.
    assert!(p.remove_provider_grant_audience(&grant_id));
    assert!(
        p.announcement_scoped_for_send_for_test().is_empty(),
        "the cached granted envelope is refused after the grant is removed",
    );

    // A fresh announce rebuilds against the empty registry — zero envelopes.
    p.announce_capabilities(CapabilitySet::new())
        .await
        .expect("announce");
    assert!(
        p.announcement_scoped_for_send_for_test().is_empty(),
        "the rebuilt emission carries no granted envelope",
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// OA3-4b2 slice 4 — consumer nonzero-grant ingest selector witnesses.
// ---------------------------------------------------------------------------

/// Adopt `org` on a fresh node, returning the node + scratch dir.
async fn adopted_node(
    seed: u8,
    org: &OrgKeypair,
    tag: &str,
) -> (Arc<MeshNode>, std::path::PathBuf) {
    let n = build_node_with(EntityKeypair::from_bytes([seed; 32])).await;
    let entity = n.entity_id().clone();
    let cert = OrgMembershipCert::try_issue(org, entity.clone(), 1, 3600).expect("cert");
    let dir = std::env::temp_dir().join(format!("net-oa34b2-cons-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let authority = NodeAuthority::adopt(&dir, cert, &entity, 0, None).expect("adopt");
    n.install_node_authority(Arc::new(authority))
        .expect("install authority");
    (n, dir)
}

/// Build a granted-audience envelope from provider P (org B) sealed under
/// `grant`/`secret`, naming `svc`. Expires 600 s from `now`.
fn granted_envelope_bytes(
    provider_kp: &EntityKeypair,
    org_b: &OrgKeypair,
    grant: &OrgCapabilityGrant,
    secret: &OrgAudienceSecret,
    svc: &str,
    now: u64,
) -> Vec<u8> {
    use net::adapter::net::behavior::org_scoped_ann::ScopedCapabilityAnnouncement;
    let cert = OrgMembershipCert::try_issue(org_b, provider_kp.entity_id().clone(), 1, 3600)
        .expect("cert");
    let descriptor = CapabilitySet::new()
        .add_tag(format!("nrpc:{svc}"))
        .to_bytes_compact();
    ScopedCapabilityAnnouncement::build_granted(
        provider_kp,
        org_b.org_id(),
        cert,
        grant.grant_id,
        secret.audience_handle,
        secret.discovery_key(),
        1,
        now + 600,
        &descriptor,
    )
    .expect("build granted envelope")
    .to_bytes()
}

/// A canonical B→A DISCOVER grant over provider `p`, plus its secret.
fn cross_org_grant(
    org_b: &OrgKeypair,
    org_a: &OrgKeypair,
    p: &EntityId,
    svc: &str,
) -> (OrgCapabilityGrant, OrgAudienceSecret) {
    let (g, s) = OrgCapabilityGrant::try_issue(
        org_b,
        org_a.org_id(),
        CapabilityAuthorityId::for_tag(&format!("nrpc:{svc}")),
        GrantRights::DISCOVER,
        GrantTargetScope::ExactNode(p.clone()),
        3600,
    )
    .expect("issue cross-org grant");
    (g, s.expect("secret"))
}

/// OA3-4b2 slice 4 — a consumer A holding the canonical B→A pair opens and
/// resolves provider P from an inbound GRANTED envelope; a node WITHOUT the pair
/// stores nothing.
#[tokio::test]
async fn an_inbound_granted_announcement_is_verified_and_stored() {
    let org_b = OrgKeypair::from_bytes([0x42u8; 32]);
    let org_a = OrgKeypair::from_bytes([0x7Au8; 32]);
    let provider = EntityKeypair::from_bytes([0x90u8; 32]);
    let p_entity = provider.entity_id().clone();
    let now = unix_now();

    let (grant, secret) = cross_org_grant(&org_b, &org_a, &p_entity, "cross");
    let grant_id = grant.grant_id;
    let env = granted_envelope_bytes(&provider, &org_b, &grant, &secret, "cross", now);

    // Consumer C: org A, with the B→A pair installed → opens + resolves P.
    let (c, c_dir) = adopted_node(0x91, &org_a, "resolve").await;
    c.install_consumer_grant_audience(grant.clone(), copy_secret(&secret))
        .expect("install consumer grant");
    c.ingest_scoped_announcement_for_test(&env);
    assert_eq!(
        c.scoped_granted_providers_for_test(&grant_id, now),
        vec![p_entity.clone()],
        "the grantee opens and resolves P under the grant",
    );

    // Node D: org A but NO consumer grant → stores nothing. Prove non-storage by
    // installing the grant AFTER the ingest: the record never landed, so the
    // query stays empty.
    let (d, d_dir) = adopted_node(0x92, &org_a, "nostore").await;
    d.ingest_scoped_announcement_for_test(&env);
    d.install_consumer_grant_audience(grant.clone(), copy_secret(&secret))
        .expect("install after the drop");
    assert!(
        d.scoped_granted_providers_for_test(&grant_id, now)
            .is_empty(),
        "a node without the pair at ingest time stored nothing",
    );

    let _ = std::fs::remove_dir_all(&c_dir);
    let _ = std::fs::remove_dir_all(&d_dir);
}

/// OA3-4b2 slice 4 — the selector is an EXACT lookup by grant id: an envelope for
/// grant G is dropped by a node that holds only a DIFFERENT grant G2 (no scan
/// across secrets), and stores nothing even after G is later installed.
#[tokio::test]
async fn the_ingest_selector_drops_a_grant_id_it_does_not_hold() {
    let org_b = OrgKeypair::from_bytes([0x42u8; 32]);
    let org_a = OrgKeypair::from_bytes([0x7Au8; 32]);
    let provider = EntityKeypair::from_bytes([0x93u8; 32]);
    let p_entity = provider.entity_id().clone();
    let now = unix_now();

    let (g1, s1) = cross_org_grant(&org_b, &org_a, &p_entity, "cross");
    let (g2, s2) = cross_org_grant(&org_b, &org_a, &p_entity, "other");
    let env1 = granted_envelope_bytes(&provider, &org_b, &g1, &s1, "cross", now);

    // C holds only G2, then receives an envelope for G1 → dropped.
    let (c, dir) = adopted_node(0x94, &org_a, "wrongid").await;
    c.install_consumer_grant_audience(g2, s2)
        .expect("install g2");
    c.ingest_scoped_announcement_for_test(&env1);
    // Install G1 afterward: if the earlier ingest had stored the record it would
    // now be queryable — it is not, proving the mismatched id was dropped.
    c.install_consumer_grant_audience(g1.clone(), copy_secret(&s1))
        .expect("install g1");
    assert!(
        c.scoped_granted_providers_for_test(&g1.grant_id, now)
            .is_empty(),
        "an envelope whose grant id the node did not hold was dropped, not stored",
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// OA3-4b2 slice 4 — removing the consumer credential retracts the stored granted
/// record IMMEDIATELY at query time (no re-announce, no sweep); the record stays
/// physically stored, so re-installing the credential makes it queryable again.
#[tokio::test]
async fn removing_the_consumer_credential_hides_the_stored_granted_record() {
    let org_b = OrgKeypair::from_bytes([0x42u8; 32]);
    let org_a = OrgKeypair::from_bytes([0x7Au8; 32]);
    let provider = EntityKeypair::from_bytes([0x95u8; 32]);
    let p_entity = provider.entity_id().clone();
    let now = unix_now();

    let (grant, secret) = cross_org_grant(&org_b, &org_a, &p_entity, "cross");
    let grant_id = grant.grant_id;
    let env = granted_envelope_bytes(&provider, &org_b, &grant, &secret, "cross", now);

    let (c, dir) = adopted_node(0x96, &org_a, "hide").await;
    c.install_consumer_grant_audience(grant.clone(), copy_secret(&secret))
        .expect("install");
    c.ingest_scoped_announcement_for_test(&env);
    assert_eq!(
        c.scoped_granted_providers_for_test(&grant_id, now),
        vec![p_entity.clone()],
        "resolves before removal",
    );

    // Remove the credential → the record is hidden immediately (read-time filter).
    assert!(c.remove_consumer_grant_audience(&grant_id));
    assert!(
        c.scoped_granted_providers_for_test(&grant_id, now)
            .is_empty(),
        "removing the consumer credential retracts the record at query time",
    );

    // Re-installing the SAME credential re-exposes the still-stored record — proof
    // the retraction was a read-time filter, not an eviction.
    c.install_consumer_grant_audience(grant.clone(), copy_secret(&secret))
        .expect("re-install");
    assert_eq!(
        c.scoped_granted_providers_for_test(&grant_id, now),
        vec![p_entity],
        "the record was hidden, not evicted — re-installing re-exposes it",
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// OA3-4b2 slice 4 — a consumer credential replacement racing the verify→insert
/// window refuses the stale result: a probe swaps the consumer snapshot pointer
/// (installs an unrelated grant) between verify and the pre-insert recheck, so the
/// raced insert is refused. A clean re-ingest against the settled snapshot lands.
#[tokio::test]
async fn a_consumer_credential_replacement_racing_the_granted_insert_is_refused() {
    let org_b = OrgKeypair::from_bytes([0x42u8; 32]);
    let org_a = OrgKeypair::from_bytes([0x7Au8; 32]);
    let provider = EntityKeypair::from_bytes([0x97u8; 32]);
    let p_entity = provider.entity_id().clone();
    let now = unix_now();

    let (grant, secret) = cross_org_grant(&org_b, &org_a, &p_entity, "cross");
    let grant_id = grant.grant_id;
    let env = granted_envelope_bytes(&provider, &org_b, &grant, &secret, "cross", now);

    let (c, dir) = adopted_node(0x98, &org_a, "race").await;
    c.install_consumer_grant_audience(grant.clone(), copy_secret(&secret))
        .expect("install target grant");

    // The probe installs an UNRELATED consumer grant, swapping the registry
    // snapshot pointer while the target-grant ingest is mid-flight.
    let (unrelated, unrelated_secret) = OrgCapabilityGrant::try_issue(
        &org_b,
        org_a.org_id(),
        CapabilityAuthorityId::for_tag("nrpc:unrelated"),
        GrantRights::DISCOVER,
        GrantTargetScope::AnyNodeOwnedBy(org_b.org_id()),
        3600,
    )
    .expect("issue unrelated");
    let unrelated_secret = unrelated_secret.expect("secret");
    let pending = std::sync::Mutex::new(Some((unrelated, unrelated_secret)));
    let c_probe = c.clone();
    let probe = move || {
        if let Some((g, s)) = pending.lock().expect("probe lock").take() {
            c_probe
                .install_consumer_grant_audience(g, s)
                .expect("probe install");
        }
    };
    c.ingest_scoped_announcement_probed_for_test(&env, &probe);
    assert!(
        c.scoped_granted_providers_for_test(&grant_id, now)
            .is_empty(),
        "the raced insert is refused when the consumer snapshot moved during verify",
    );

    // The target grant is still installed; a clean re-ingest against the settled
    // snapshot now lands.
    c.ingest_scoped_announcement_for_test(&env);
    assert_eq!(
        c.scoped_granted_providers_for_test(&grant_id, now),
        vec![p_entity],
        "a clean re-ingest against the settled snapshot resolves P",
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// OA3-4b1 B2 audience-rotation safety (Kyra): a same-org authority replacement
/// rotates the owner audience key while keeping the membership cert equal. The
/// public-cert and visibility checks therefore see no change — only the recorded
/// sealing-authority identity does. The cached scoped envelope sealed under the
/// OLD key must NOT ship after the rotation; only a rebuild under the NEW key may.
#[tokio::test]
async fn a_same_org_audience_rotation_refuses_the_stale_scoped_envelope() {
    use net::adapter::net::behavior::org::{OrgKeypair, OrgMembershipCert};
    use net::adapter::net::behavior::org_authority::NodeAuthority;
    use net::adapter::net::behavior::org_revocation::OrgRevocationState;
    use net::adapter::net::behavior::org_scoped_ann::ScopedCapabilityAnnouncement;
    use net::adapter::net::behavior::org_scoped_ingest::{
        verify_scoped_ingest, AudienceAuthority, ScopedIngestContext,
    };

    let server = build_node_with(EntityKeypair::from_bytes([0x54u8; 32])).await;
    let node_entity = server.entity_id().clone();
    let org = OrgKeypair::from_bytes([0x77u8; 32]);
    // ONE membership cert C, shared by both authorities — only the audience key
    // rotates (each `adopt` generates a fresh random OwnerAudienceCredential).
    let cert = OrgMembershipCert::try_issue(&org, node_entity.clone(), 1, 3600).expect("cert C");
    let owner_org = cert.org_id;

    let dir_a = std::env::temp_dir().join(format!("net-b2-rot-a-{}", std::process::id()));
    let dir_b = std::env::temp_dir().join(format!("net-b2-rot-b-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir_a);
    let _ = std::fs::remove_dir_all(&dir_b);
    let authority_a = Arc::new(
        NodeAuthority::adopt(&dir_a, cert.clone(), &node_entity, 0, None).expect("adopt A"),
    );
    let authority_b = Arc::new(
        NodeAuthority::adopt(&dir_b, cert.clone(), &node_entity, 0, None).expect("adopt B"),
    );
    // Capture each audience (handle + key) BEFORE installing, since install
    // consumes the Arc. The rotation must actually change the key.
    let handle_a = authority_a.audience.audience_handle;
    let key_a = *authority_a.audience.discovery_key();
    let handle_b = authority_b.audience.audience_handle;
    let key_b = *authority_b.audience.discovery_key();
    assert_ne!(key_a, key_b, "the rotation must change the audience key");

    server
        .install_node_authority(authority_a)
        .expect("install A");
    server
        .set_owner_cert_emission(true)
        .expect("enable owner-cert emission");
    let _secret = server
        .serve_rpc_owner_scoped("secret", Arc::new(TrivialHandler), Arc::new(|_| true))
        .expect("owner-scoped serve");
    server
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("announce under A");

    // E1 published under authority A.
    assert!(
        wait_until(Duration::from_secs(5), || {
            server.announcement_scoped_for_send_for_test().len() == 1
        })
        .await,
        "E1 published under authority A",
    );
    let e1 = server.announcement_scoped_for_send_for_test();
    let env1 = ScopedCapabilityAnnouncement::from_bytes(&e1[0]).expect("decode E1");

    // Rotate: replace with same-org authority B (same cert C, new audience K2).
    // The bare test node has no `self_weak`, so no auto re-announce fires; the
    // stale E1 stays cached until we rebuild explicitly. Critically, there is NO
    // await between the sync `install` and the sync scoped read below, so on the
    // current-thread test runtime no re-announce task can interpose — the refusal
    // is observed deterministically.
    server
        .install_node_authority(authority_b)
        .expect("install B (same-org rotation)");
    let after_rotation = server.announcement_scoped_for_send_for_test();
    assert!(
        after_rotation.is_empty(),
        "a rotation must refuse the stale scoped envelope until the emission is rebuilt",
    );

    // Rebuild under B → E2, sealed under K2.
    server
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("announce under B");
    assert!(
        wait_until(Duration::from_secs(5), || {
            server.announcement_scoped_for_send_for_test().len() == 1
        })
        .await,
        "E2 published under authority B",
    );
    let e2 = server.announcement_scoped_for_send_for_test();
    let env2 = ScopedCapabilityAnnouncement::from_bytes(&e2[0]).expect("decode E2");

    let floors = OrgRevocationState::empty();
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs();

    // E1 opens under K1; E2 opens under K2 — both to the owner-scoped descriptor.
    let v1 = verify_scoped_ingest(
        &env1,
        &AudienceAuthority::Owner {
            owner_org,
            audience_handle: handle_a,
            discovery_key: &key_a,
        },
        &ScopedIngestContext {
            local_owner_org: owner_org,
            floors: &floors,
            now_secs,
            skew_secs: 5,
        },
    )
    .expect("E1 opens under K1");
    assert!(CapabilitySet::from_bytes(v1.descriptor())
        .expect("E1 descriptor")
        .has_tag("nrpc:secret"));
    let v2 = verify_scoped_ingest(
        &env2,
        &AudienceAuthority::Owner {
            owner_org,
            audience_handle: handle_b,
            discovery_key: &key_b,
        },
        &ScopedIngestContext {
            local_owner_org: owner_org,
            floors: &floors,
            now_secs,
            skew_secs: 5,
        },
    )
    .expect("E2 opens under K2");
    assert!(CapabilitySet::from_bytes(v2.descriptor())
        .expect("E2 descriptor")
        .has_tag("nrpc:secret"));

    // E2 (sealed under K2) must NOT open under the rotated-out K1.
    assert!(
        verify_scoped_ingest(
            &env2,
            &AudienceAuthority::Owner {
                owner_org,
                audience_handle: handle_b,
                discovery_key: &key_a,
            },
            &ScopedIngestContext {
                local_owner_org: owner_org,
                floors: &floors,
                now_secs,
                skew_secs: 5,
            },
        )
        .is_err(),
        "E2 sealed under the new key must not open under the rotated-out K1",
    );

    let _ = std::fs::remove_dir_all(&dir_a);
    let _ = std::fs::remove_dir_all(&dir_b);
}

/// OA3-5a: a live inbound owner-scoped announcement is opened under this node's
/// OWN owner audience, verified (provider membership + floors + freshness), and
/// landed in the private-discovery store — queryable without ever touching the
/// plaintext fold. A wrong-audience or expired envelope is refused, never stored.
#[tokio::test]
async fn an_inbound_owner_scoped_announcement_is_verified_and_stored() {
    use net::adapter::net::behavior::org::{OrgKeypair, OrgMembershipCert};
    use net::adapter::net::behavior::org_authority::NodeAuthority;
    use net::adapter::net::behavior::org_scoped_ann::ScopedCapabilityAnnouncement;

    let node = build_node_with(EntityKeypair::from_bytes([0x60u8; 32])).await;
    let node_entity = node.entity_id().clone();
    let org = OrgKeypair::from_bytes([0x88u8; 32]);
    let node_cert =
        OrgMembershipCert::try_issue(&org, node_entity.clone(), 1, 3600).expect("node cert");
    let dir = std::env::temp_dir().join(format!("net-oa35-ingest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let authority =
        Arc::new(NodeAuthority::adopt(&dir, node_cert, &node_entity, 0, None).expect("adopt"));
    // The node's OWN owner audience — a same-org provider seals to it.
    let handle = authority.audience.audience_handle;
    let key = *authority.audience.discovery_key();
    node.install_node_authority(authority).expect("install");

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    let descriptor = CapabilitySet::new()
        .add_tag("nrpc:peer-secret")
        .to_bytes_compact();

    // A same-org PROVIDER's owner-scoped envelope, sealed to `disc_key`.
    let make_envelope = |seed: u8, disc_key: [u8; 32], expires_at: u64| -> (EntityId, Vec<u8>) {
        let provider_kp = EntityKeypair::from_bytes([seed; 32]);
        let provider_entity = provider_kp.entity_id().clone();
        let cert = OrgMembershipCert::try_issue(&org, provider_entity.clone(), 1, 3600)
            .expect("provider cert");
        let env = ScopedCapabilityAnnouncement::build_owner(
            &provider_kp,
            org.org_id(),
            cert,
            handle,
            &disc_key,
            1,
            expires_at,
            &descriptor,
        )
        .expect("build owner envelope");
        (provider_entity, env.to_bytes())
    };

    // Good: sealed to the node's real audience key, in-window → verified + stored.
    let (good_provider, good_bytes) = make_envelope(0x61, key, now + 3600);
    node.ingest_scoped_announcement_for_test(&good_bytes);
    assert!(
        node.scoped_owner_providers_for_test(now)
            .iter()
            .any(|p| p == &good_provider),
        "the verified owner-scoped provider is exposed in the private-discovery store",
    );

    // Wrong audience: same handle, DIFFERENT discovery key → AEAD open fails.
    let (bad_provider, bad_bytes) = make_envelope(0x62, [0x99u8; 32], now + 3600);
    node.ingest_scoped_announcement_for_test(&bad_bytes);
    assert!(
        !node
            .scoped_owner_providers_for_test(now)
            .iter()
            .any(|p| p == &bad_provider),
        "a wrong-audience envelope is refused and never stored",
    );

    // Expired: the freshness gate refuses it at ingest.
    let (exp_provider, exp_bytes) = make_envelope(0x63, key, now.saturating_sub(10));
    node.ingest_scoped_announcement_for_test(&exp_bytes);
    assert!(
        !node
            .scoped_owner_providers_for_test(now)
            .iter()
            .any(|p| p == &exp_provider),
        "an expired envelope is refused at ingest",
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// OA3-5 (Kyra closure, publication race): an owner-scoped capability verified
/// against a floor/authority/store view that MOVES before the store insert is
/// refused FAIL-CLOSED, never landed stale. A concurrent revocation-floor
/// publish landing in the exact verify→insert window (driven here through the
/// probe seam) bumps the store generation; the pre-insert recheck sees the view
/// moved and drops the insert. The refusal is isolated to the recheck: the raced
/// provider's OWN floor is never touched, so query-time currentness (3b) would
/// have kept it visible had it been stored — its absence proves it never
/// entered. Re-announcing the identical envelope against the settled view lands.
#[tokio::test]
async fn a_floor_publish_racing_the_scoped_insert_is_refused_then_recovers() {
    use net::adapter::net::behavior::org::{OrgKeypair, OrgMembershipCert, OrgRevocationBundle};
    use net::adapter::net::behavior::org_authority::NodeAuthority;
    use net::adapter::net::behavior::org_scoped_ann::ScopedCapabilityAnnouncement;
    use std::collections::BTreeMap;

    let node = build_node_with(EntityKeypair::from_bytes([0x70u8; 32])).await;
    let node_entity = node.entity_id().clone();
    let org = OrgKeypair::from_bytes([0x89u8; 32]);
    let node_cert =
        OrgMembershipCert::try_issue(&org, node_entity.clone(), 1, 3600).expect("node cert");
    let dir = std::env::temp_dir().join(format!("net-oa35-race-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let authority =
        Arc::new(NodeAuthority::adopt(&dir, node_cert, &node_entity, 0, None).expect("adopt"));
    // The authority's own revocation store BECOMES the node's live store on
    // install — a floor published through this handle bumps the exact generation
    // the ingest recheck reads (it is the same `Arc`, never swapped by a raise).
    let store = authority.revocation.clone();
    let handle = authority.audience.audience_handle;
    let key = *authority.audience.discovery_key();
    node.install_node_authority(authority)
        .expect("install authority");

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    let descriptor = CapabilitySet::new()
        .add_tag("nrpc:peer-secret")
        .to_bytes_compact();

    let make_envelope = |seed: u8| -> (EntityId, Vec<u8>) {
        let provider_kp = EntityKeypair::from_bytes([seed; 32]);
        let provider_entity = provider_kp.entity_id().clone();
        let cert = OrgMembershipCert::try_issue(&org, provider_entity.clone(), 1, 3600)
            .expect("provider cert");
        let env = ScopedCapabilityAnnouncement::build_owner(
            &provider_kp,
            org.org_id(),
            cert,
            handle,
            &key,
            1,
            now + 3600,
            &descriptor,
        )
        .expect("build owner envelope");
        (provider_entity, env.to_bytes())
    };

    // Baseline: a valid same-org envelope lands with the store installed.
    let (clean_provider, clean_bytes) = make_envelope(0x71);
    node.ingest_scoped_announcement_for_test(&clean_bytes);
    assert!(
        node.scoped_owner_providers_for_test(now)
            .iter()
            .any(|p| p == &clean_provider),
        "a valid owner-scoped envelope lands under an installed revocation store",
    );

    // The raced envelope: a floor publish for an UNRELATED member fires between
    // verify and the pre-insert recheck — bumping the store generation WITHOUT
    // touching this provider's own floor.
    let (raced_provider, raced_bytes) = make_envelope(0x72);
    let unrelated_member = EntityKeypair::from_bytes([0xAAu8; 32]).entity_id().clone();
    let race_probe = || {
        let mut floors_map = BTreeMap::new();
        floors_map.insert(unrelated_member.clone(), 5u32);
        let bundle = OrgRevocationBundle::try_issue(&org, &floors_map).expect("issue race bundle");
        store.apply_bundle(&bundle).expect("apply race floor");
    };
    node.ingest_scoped_announcement_probed_for_test(&raced_bytes, &race_probe);
    assert!(
        !node
            .scoped_owner_providers_for_test(now)
            .iter()
            .any(|p| p == &raced_provider),
        "an insert racing a floor publish is refused — the raced provider never enters the store",
    );

    // Its OWN floor was never raised, so the absence is purely the recheck:
    // re-announce the IDENTICAL envelope against the now-settled view and it lands.
    node.ingest_scoped_announcement_for_test(&raced_bytes);
    assert!(
        node.scoped_owner_providers_for_test(now)
            .iter()
            .any(|p| p == &raced_provider),
        "the identical envelope re-announced against the settled view lands cleanly",
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// OA3-5 §3.2 (Kyra APPROVED design) — opaque multi-hop propagation, LIVE:
/// provider P emits an owner-scoped service; relay R (no org authority, no shared
/// audience) forwards the OPAQUE envelope but can neither decrypt nor store it;
/// consumer C — which shares P's owner audience and has NO direct session with P
/// — receives the forwarded frame, opens it, and resolves P in its
/// private-discovery store. Because P and C are never directly connected, C's
/// knowledge of P can only have arrived through R's relay.
#[tokio::test]
async fn an_owner_scoped_announcement_floods_opaquely_through_a_relay_to_the_audience() {
    use net::adapter::net::behavior::org::{OrgKeypair, OrgMembershipCert};
    use net::adapter::net::behavior::org_authority::{NodeAuthority, OwnerAudienceCredential};

    let p = build_node_fast_announce(EntityKeypair::from_bytes([0x80u8; 32])).await;
    let r = build_node_fast_announce(EntityKeypair::from_bytes([0x81u8; 32])).await;
    let c = build_node_fast_announce(EntityKeypair::from_bytes([0x82u8; 32])).await;
    let p_entity = p.entity_id().clone();
    let c_entity = c.entity_id().clone();

    let org = OrgKeypair::from_bytes([0x8Au8; 32]);
    let base = std::env::temp_dir().join(format!("net-oa35-relay-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);

    // --- P: provider with an owner-scoped service ---
    let p_cert = OrgMembershipCert::try_issue(&org, p_entity.clone(), 1, 3600).expect("P cert");
    let p_authority = Arc::new(
        NodeAuthority::adopt(&base.join("p"), p_cert, &p_entity, 0, None).expect("adopt P"),
    );
    // Capture P's owner audience BEFORE install (install consumes the Arc); C
    // shares this exact credential, modelling the org distributing ONE owner
    // audience to its member nodes.
    let shared_audience = p_authority.audience.encode_config();
    p.install_node_authority(p_authority)
        .expect("install P authority");
    p.set_owner_cert_emission(true).expect("enable P emission");
    let _svc = p
        .serve_rpc_owner_scoped("secret", Arc::new(TrivialHandler), Arc::new(|_| true))
        .expect("P owner-scoped serve");

    // --- C: consumer in the SAME org, sharing P's owner audience ---
    let c_cert = OrgMembershipCert::try_issue(&org, c_entity.clone(), 1, 3600).expect("C cert");
    let mut c_authority =
        NodeAuthority::adopt(&base.join("c"), c_cert, &c_entity, 0, None).expect("adopt C");
    c_authority.audience =
        OwnerAudienceCredential::decode_config(&shared_audience).expect("decode shared audience");
    c.install_node_authority(Arc::new(c_authority))
        .expect("install C authority");

    // --- R: pure relay, NO authority (cannot open or store scoped anns) ---

    // Topology: P—R and R—C, but NEVER P—C. Establish both sessions before
    // starting any dispatch loop, then bring all three up together.
    connect_no_start(&p, &r).await;
    connect_no_start(&r, &c).await;
    p.start();
    r.start();
    c.start();

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs();

    // P's cached emission carries exactly one scoped envelope before we rely on
    // the flood shipping it.
    assert!(
        wait_until(Duration::from_secs(5), || {
            p.announcement_scoped_for_send_for_test().len() == 1
        })
        .await,
        "P emits exactly one owner-scoped envelope",
    );

    // Drive the flood: P announces (ships the 0x0C04 hop-0 frame to R); R
    // forwards the opaque frame to C; C opens + stores. Re-announce across the
    // wait so a coalesced/rate-limited send still lands within the window.
    let mut c_resolved_p = false;
    for _ in 0..40 {
        p.announce_capabilities(CapabilitySet::new()).await.ok();
        if c.scoped_owner_providers_for_test(now)
            .iter()
            .any(|prov| prov == &p_entity)
        {
            c_resolved_p = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    assert!(
        c_resolved_p,
        "C resolves P through the relay despite having no direct session with P",
    );

    // The relay ADMITTED the envelope through its dedup gate — proof it received
    // and forwarded it — yet, lacking any authority or audience, opened and
    // stored NOTHING.
    assert!(
        r.scoped_relay_gate_len_for_test() >= 1,
        "the relay admitted and forwarded the opaque envelope",
    );
    assert!(
        r.scoped_owner_providers_for_test(now).is_empty(),
        "the authority-less relay forwards but never decrypts or stores the envelope",
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// OA3-4b2 slice 5 — a GRANTED (cross-org B→A) capability floods opaquely through
/// a relay to the grantee, LIVE. Provider P (org B) emits a granted-private
/// service under a B→A grant; relay R (no authority, no grant) forwards the
/// opaque envelope but can neither decrypt nor store it; consumer A (org B's
/// GRANTEE, holding the B→A consumer credential) — with NO direct session to P —
/// receives the forwarded frame, opens it under the grant, and resolves P. Since
/// P and A are different orgs AND never directly connected, A's knowledge of P can
/// only have arrived through R. Plaintext projections stay clean throughout.
#[tokio::test]
async fn a_granted_capability_floods_opaquely_through_a_relay_to_the_grantee() {
    let p = build_node_fast_announce(EntityKeypair::from_bytes([0x83u8; 32])).await;
    let r = build_node_fast_announce(EntityKeypair::from_bytes([0x84u8; 32])).await;
    let a = build_node_fast_announce(EntityKeypair::from_bytes([0x85u8; 32])).await;
    let p_entity = p.entity_id().clone();

    let org_b = OrgKeypair::from_bytes([0x8Bu8; 32]); // provider org
    let org_a = OrgKeypair::from_bytes([0x8Au8; 32]); // grantee org (distinct)
    let base = std::env::temp_dir().join(format!("net-oa34b2-relay-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);

    // The single B→A grant: P holds it as a PROVIDER record (to emit), A holds it
    // as a CONSUMER record (to open) — same grant_id, same audience key.
    let (grant, secret) = cross_org_grant(&org_b, &org_a, &p_entity, "cross");
    let grant_id = grant.grant_id;

    // --- P: org-B provider with a granted-private service + provider grant ---
    let p_cert = OrgMembershipCert::try_issue(&org_b, p_entity.clone(), 1, 3600).expect("P cert");
    let p_authority = Arc::new(
        NodeAuthority::adopt(&base.join("p"), p_cert, &p_entity, 0, None).expect("adopt P"),
    );
    p.install_node_authority(p_authority)
        .expect("install P authority");
    p.set_owner_cert_emission(true).expect("enable P emission");
    let _svc = p
        .serve_rpc_granted("cross", Arc::new(TrivialHandler), Arc::new(|_| true))
        .expect("P granted serve");
    p.install_provider_grant_audience(grant.clone(), copy_secret(&secret))
        .expect("install P provider grant");

    // --- A: org-A grantee holding the B→A consumer credential ---
    let a_entity = a.entity_id().clone();
    let a_cert = OrgMembershipCert::try_issue(&org_a, a_entity.clone(), 1, 3600).expect("A cert");
    let a_authority = Arc::new(
        NodeAuthority::adopt(&base.join("a"), a_cert, &a_entity, 0, None).expect("adopt A"),
    );
    a.install_node_authority(a_authority)
        .expect("install A authority");
    a.install_consumer_grant_audience(grant.clone(), copy_secret(&secret))
        .expect("install A consumer grant");

    // --- R: pure relay, NO authority (cannot open or store scoped anns) ---

    // Topology: P—R and R—A, but NEVER P—A. Establish both sessions before
    // starting any dispatch loop, then bring all three up together.
    connect_no_start(&p, &r).await;
    connect_no_start(&r, &a).await;
    p.start();
    r.start();
    a.start();

    let now = unix_now();

    // P's cached emission carries exactly one granted envelope before the flood.
    assert!(
        wait_until(Duration::from_secs(5), || {
            p.announcement_scoped_for_send_for_test().len() == 1
        })
        .await,
        "P emits exactly one granted envelope",
    );

    // Drive the flood: P announces (ships the 0x0C04 hop-0 frame to R); R forwards
    // the opaque frame to A; A opens + resolves P under the grant.
    let mut a_resolved_p = false;
    for _ in 0..40 {
        p.announce_capabilities(CapabilitySet::new()).await.ok();
        if a.scoped_granted_providers_for_test(&grant_id, now)
            .iter()
            .any(|prov| prov == &p_entity)
        {
            a_resolved_p = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    assert!(
        a_resolved_p,
        "the grantee A resolves P through the relay despite different orgs and no direct session",
    );

    // The relay ADMITTED the envelope through its dedup gate — proof it received
    // and forwarded it — yet, lacking any authority or grant, opened and stored
    // NOTHING. R is the non-grantee observer: it cannot resolve the capability.
    assert!(
        r.scoped_relay_gate_len_for_test() >= 1,
        "the relay admitted and forwarded the opaque envelope",
    );
    assert!(
        r.scoped_owner_providers_for_test(now).is_empty()
            && r.scoped_granted_providers_for_test(&grant_id, now)
                .is_empty(),
        "the authority-less non-grantee relay forwards but never decrypts or stores",
    );

    // Plaintext stays clean: P never advertises the granted tag in the clear.
    assert!(
        p.local_announcement_for_test()
            .map(|ann| !ann.capabilities.has_tag("nrpc:cross"))
            .unwrap_or(false),
        "the granted tag never appears in P's plaintext announcement",
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// OA3 closure (Kyra #2): a send fired AFTER a visibility change must not ship an
/// emission derived from the STALE visibility snapshot. The send-time generation
/// check — shared by every self-emission path via `announcement_bytes_for_send`
/// (immediate / deferred flush / late-join push) — refuses it; the visibility
/// bump already woke a re-announce that will publish a coherent emission.
#[tokio::test]
async fn a_send_after_a_visibility_change_refuses_the_stale_emission() {
    let server = build_node_with(EntityKeypair::from_bytes([0x52u8; 32])).await;
    let (_org_b, dir) = install_authority(&server, "vis-race");

    let _svc = server
        .serve_rpc("svc", Arc::new(TrivialHandler))
        .expect("public serve");
    server
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("announce");
    // An emission is published at the current visibility generation.
    assert!(
        wait_until(Duration::from_secs(5), || {
            server.announcement_bytes_for_send_for_test().is_some()
        })
        .await,
        "an emission is published",
    );

    // Simulate a concurrent visibility change AFTER publication (a Public ->
    // OwnerScoped re-registration advances the registry generation). The cached
    // emission is now stale.
    server.test_advance_visibility_generation();

    // Every plaintext send funnels through `announcement_bytes_for_send`, which
    // now REFUSES the stale emission — so no immediate / deferred / late-join send
    // ships a tag a visibility change may have made private.
    assert!(
        server.announcement_bytes_for_send_for_test().is_none(),
        "a send after a visibility change must not ship the stale emission",
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// OA3 closure item-2 race (Kyra re-review): a visibility change landing INSIDE
/// the send seqlock — AFTER the plaintext bytes are serialized but BEFORE the
/// final stability check — must NOT release the already-serialized stale bytes.
/// The probed-send seam fires the probe in exactly that window.
#[tokio::test]
async fn a_visibility_change_during_serialization_refuses_the_stale_bytes() {
    let server = build_node_with(EntityKeypair::from_bytes([0x53u8; 32])).await;
    let (_org_b, dir) = install_authority(&server, "vis-serialize-race");

    let _svc = server
        .serve_rpc("svc", Arc::new(TrivialHandler))
        .expect("public serve");
    server
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("announce");
    assert!(
        wait_until(Duration::from_secs(5), || {
            server.announcement_bytes_for_send_for_test().is_some()
        })
        .await,
        "an emission is published",
    );

    // The probe fires inside the send seqlock — after serialize, before the final
    // stability recheck — and advances the visibility generation there. The final
    // recheck (beside the security-stamp comparison) must refuse the already
    // serialized stale plaintext bytes.
    let server_probe = server.clone();
    let probe = move || server_probe.test_advance_visibility_generation();
    assert!(
        server
            .announcement_bytes_for_send_probed_for_test(&probe)
            .is_none(),
        "a visibility change during serialization must refuse the stale bytes",
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// OA3-4b2 slice 1 — the LIVE `MeshNode` grant-audience install/remove surface.
/// A node owned by org B installs a provider record (a grant IT issued over one
/// of its own providers) and a consumer record (a grant naming org B as
/// grantee), exercising the authority-gated APIs, idempotency, removal, and the
/// no-authority refusal. Pure store wiring — no emission/ingest yet.
#[tokio::test]
async fn grant_audience_registries_install_and_remove_on_a_live_node() {
    // A node with no authority cannot hold grant audiences.
    let bare = build_node_with(EntityKeypair::from_bytes([0x60u8; 32])).await;
    let org_b = OrgKeypair::from_bytes([0x42u8; 32]); // install_authority's org
    let org_a = OrgKeypair::from_bytes([0x6Au8; 32]); // a foreign org
    let (provider_grant, provider_secret) = OrgCapabilityGrant::try_issue(
        &org_b,
        org_a.org_id(),
        CapabilityAuthorityId::for_tag("nrpc:reconcile"),
        GrantRights::DISCOVER,
        GrantTargetScope::ExactNode(bare.entity_id().clone()),
        3600,
    )
    .expect("issue provider grant");
    let provider_secret = provider_secret.expect("DISCOVER mints a secret");
    assert_eq!(
        bare.install_provider_grant_audience(provider_grant.clone(), provider_secret)
            .unwrap_err(),
        GrantAudienceInstallError::NoAuthority,
        "a node without authority refuses a grant-audience install",
    );

    // Adopt org B on the node (owner org = org B's id).
    let server = build_node_with(EntityKeypair::from_bytes([0x61u8; 32])).await;
    let node_entity = server.entity_id().clone();
    let node_cert =
        OrgMembershipCert::try_issue(&org_b, node_entity.clone(), 1, 3600).expect("node cert");
    let dir = std::env::temp_dir().join(format!("net-oa34b2-store-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let authority =
        NodeAuthority::adopt(&dir, node_cert, &node_entity, 0, None).expect("adopt authority");
    server
        .install_node_authority(Arc::new(authority))
        .expect("install authority");

    // --- Provider record: a grant org B issued over THIS provider node. ---
    let (p_grant, p_secret) = OrgCapabilityGrant::try_issue(
        &org_b,
        org_a.org_id(),
        CapabilityAuthorityId::for_tag("nrpc:reconcile"),
        GrantRights::DISCOVER.union(GrantRights::INVOKE),
        GrantTargetScope::ExactNode(node_entity.clone()),
        3600,
    )
    .expect("issue provider grant");
    let p_secret = p_secret.expect("secret");
    let p_grant_id = p_grant.grant_id;
    // A byte-identical copy of the secret for the idempotent re-install (re-
    // issuing would mint a fresh random id) — scrubs its temporary key buffer.
    let p_secret_copy = copy_secret(&p_secret);

    assert_eq!(
        server
            .install_provider_grant_audience(p_grant.clone(), p_secret)
            .expect("install provider grant"),
        GrantAudienceInstalled::Installed,
    );
    assert_eq!(server.provider_grant_audiences_len_for_test(), 1);
    // A byte-identical re-install is an idempotent no-op.
    assert_eq!(
        server
            .install_provider_grant_audience(p_grant.clone(), p_secret_copy)
            .expect("idempotent re-install"),
        GrantAudienceInstalled::AlreadyPresent,
    );
    assert_eq!(server.provider_grant_audiences_len_for_test(), 1);
    // A grant this node's org did NOT issue is refused (wrong provider issuer).
    let (foreign_grant, foreign_secret) = OrgCapabilityGrant::try_issue(
        &org_a,
        org_a.org_id(),
        CapabilityAuthorityId::for_tag("nrpc:reconcile"),
        GrantRights::DISCOVER,
        GrantTargetScope::AnyNodeOwnedBy(org_a.org_id()),
        3600,
    )
    .expect("issue foreign grant");
    assert_eq!(
        server
            .install_provider_grant_audience(foreign_grant, foreign_secret.expect("secret"))
            .unwrap_err(),
        GrantAudienceInstallError::WrongProviderIssuer,
    );

    // --- Consumer record: a grant naming org B (this node's org) as grantee. ---
    let (c_grant, c_secret) = OrgCapabilityGrant::try_issue(
        &org_a,
        org_b.org_id(),
        CapabilityAuthorityId::for_tag("nrpc:remote-svc"),
        GrantRights::DISCOVER,
        GrantTargetScope::AnyNodeOwnedBy(org_a.org_id()),
        3600,
    )
    .expect("issue consumer grant");
    let c_grant_id = c_grant.grant_id;
    assert_eq!(
        server
            .install_consumer_grant_audience(c_grant, c_secret.expect("secret"))
            .expect("install consumer grant"),
        GrantAudienceInstalled::Installed,
    );
    assert_eq!(server.consumer_grant_audiences_len_for_test(), 1);
    // The consumer install did NOT touch the provider registry (role separation).
    assert_eq!(server.provider_grant_audiences_len_for_test(), 1);

    // --- Removal is by grant id and role-scoped. ---
    assert!(server.remove_provider_grant_audience(&p_grant_id));
    assert_eq!(server.provider_grant_audiences_len_for_test(), 0);
    // Removing again is a no-op.
    assert!(!server.remove_provider_grant_audience(&p_grant_id));
    // The consumer record is untouched by the provider removal.
    assert_eq!(server.consumer_grant_audiences_len_for_test(), 1);
    assert!(server.remove_consumer_grant_audience(&c_grant_id));
    assert_eq!(server.consumer_grant_audiences_len_for_test(), 0);

    let _ = std::fs::remove_dir_all(&dir);
}

// ============================================================================
// OA-4 slice 1 — live cross-org CrossOrgGranted invocation (Tier 1)
//
// The first end-to-end CrossOrgGranted INVOKE over the real transport: caller S
// (org A) holds a B→A capability grant carrying INVOKE for the provider P₂'s
// capability; P₂ (org B) serves the protected service under `CrossOrgGranted`.
// This is the invocation analog of the OwnerDelegated `live_two_node_owner_
// delegated_admit`, and the harness slice 4's DISCOVER|INVOKE row reuses.
// ============================================================================

/// A cross-org INVOKE intent: caller `caller_kp` is a member of grantee org `A`
/// and holds a B→A capability grant (issued by provider-owner org `B`) carrying
/// INVOKE for `nrpc:<service>`, whose `target_scope` covers provider `P₂`.
///
/// A pure-INVOKE grant carries NO audience material by construction — the
/// returned secret is `None` (the structural rule, asserted here), so this
/// invocation path never touches discovery.
fn cross_org_invoke_intent(
    caller_kp: EntityKeypair,
    org_a: &OrgKeypair,
    org_b: &OrgKeypair,
    provider: EntityId,
    service: &str,
    target_scope: GrantTargetScope,
) -> OrgProofIntent {
    let cap = CapabilityAuthorityId::for_tag(&format!("nrpc:{service}"));
    // Membership + dispatcher grant come from the caller's OWN org A (the acting
    // org named by the membership); the capability grant comes from org B.
    let (grant, secret) = OrgCapabilityGrant::try_issue(
        org_b,
        org_a.org_id(),
        cap,
        GrantRights::INVOKE,
        target_scope,
        3600,
    )
    .expect("issue cross-org INVOKE grant");
    assert!(
        secret.is_none(),
        "an INVOKE-only grant carries no audience material by construction",
    );
    cross_org_intent_with_grant(caller_kp, org_a, provider, org_b.org_id(), service, grant)
}

/// Build a cross-org intent that REUSES a given capability `grant`, targeting
/// `provider` whose owner org is `provider_owner_org`. Membership + dispatcher
/// grant come from the caller's OWN org A. Lets one grant drive several
/// providers (e.g. an `AnyNodeOwnedBy` grant reused across B-owned nodes).
fn cross_org_intent_with_grant(
    caller_kp: EntityKeypair,
    org_a: &OrgKeypair,
    provider: EntityId,
    provider_owner_org: OrgId,
    service: &str,
    grant: OrgCapabilityGrant,
) -> OrgProofIntent {
    let caller_entity = caller_kp.entity_id().clone();
    let cap = CapabilityAuthorityId::for_tag(&format!("nrpc:{service}"));
    let membership =
        OrgMembershipCert::try_issue(org_a, caller_entity.clone(), 1, 3600).expect("membership");
    let dispatcher =
        OrgDispatcherGrant::try_issue(org_a, caller_entity, DispatcherScope::Exact(cap), 3600)
            .expect("dispatcher");
    OrgProofIntent {
        caller: Arc::new(caller_kp),
        membership,
        dispatcher,
        capability_grant: Some(grant),
        acting_org: org_a.org_id(),
        provider_owner_org,
        provider,
        capability: cap,
        proof_ttl_secs: 30,
    }
}

/// LIVE cross-org admit over the real transport: caller S ∈ org A invokes a
/// protected `CrossOrgGranted` service on provider P₂ ∈ org B under a B→A INVOKE
/// grant. The provider gate verifies the proof, the handler runs exactly once
/// with the exact FIVE-field attribution (caller S, acting org A, provider org
/// B, provider P₂, capability C), the raw proof header is stripped before the
/// handler view, and the reply returns to S.
#[tokio::test]
async fn live_two_node_cross_org_granted_admit() {
    const CALLER_SEED: [u8; 32] = [0x1au8; 32];
    let server = build_node_with(EntityKeypair::generate()).await;
    let caller = build_node_with(EntityKeypair::from_bytes(CALLER_SEED)).await;
    bring_up(&caller, &server).await;

    // Provider P₂ is owned by org B; the caller acts for a DISTINCT org A.
    let (org_b, dir) = install_authority(&server, "xorg-admit");
    let org_a = OrgKeypair::from_bytes([0x7au8; 32]);
    assert_ne!(org_a.org_id(), org_b.org_id(), "A and B are distinct orgs");
    let provider = server.entity_id().clone();
    let caller_entity = caller.entity_id().clone();

    let calls = Arc::new(AtomicUsize::new(0));
    let saw = Arc::new(AtomicBool::new(false));
    let attribution_ok = Arc::new(AtomicBool::new(false));
    let stripped = Arc::new(AtomicBool::new(false));
    let _serve = server
        .serve_rpc_protected(
            "svc",
            Arc::new(AdmitHandler {
                calls: calls.clone(),
                saw_admission: saw.clone(),
                attribution_ok: attribution_ok.clone(),
                proof_stripped: stripped.clone(),
                expected_caller: caller_entity,
                // Cross-org: the caller acts for org A; P₂ is owned by org B.
                expected_acting_org: org_a.org_id(),
                expected_provider_org: org_b.org_id(),
                expected_provider: provider.clone(),
                expected_capability: CapabilityAuthorityId::for_tag("nrpc:svc"),
            }),
            OrgAdmission::CrossOrgGranted,
            Arc::new(|_| true),
        )
        .expect("serve protected cross-org");

    // The grant covers exactly P₂ (ExactNode). Same caller seed ⇒ the
    // authenticated session peer matches the proof subject.
    let intent = cross_org_invoke_intent(
        EntityKeypair::from_bytes(CALLER_SEED),
        &org_a,
        &org_b,
        provider.clone(),
        "svc",
        GrantTargetScope::ExactNode(provider),
    );
    let opts = CallOptions {
        org_proof_intent: Some(intent),
        deadline: Some(Instant::now() + Duration::from_secs(5)),
        ..Default::default()
    };
    let reply = caller
        .call(server.node_id(), "svc", Bytes::from_static(b"ping"), opts)
        .await
        .expect("admitted cross-org call returns Ok");

    assert_eq!(reply.body.as_ref(), b"pong", "the reply returns to S");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "handler ran exactly once");
    assert!(
        saw.load(Ordering::SeqCst),
        "handler observed org_admission attribution (Some)",
    );
    assert!(
        attribution_ok.load(Ordering::SeqCst),
        "all five attribution fields (caller S, acting org A, provider org B, provider P₂, \
         capability nrpc:svc) match — no caller-claimed field is used as attribution",
    );
    assert!(
        stripped.load(Ordering::SeqCst),
        "the raw net-org-admission proof header was stripped from the handler view",
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// OA-4 slice 2 (Tier 1, corrupted-provider-state) — missing-local-tag. A
/// protected `CrossOrgGranted` service stays registered, but the provider's own
/// capability tag is superseded out of the fold by an empty higher-version
/// self-announcement. An OTHERWISE-VALID cross-org proof is denied at the
/// possession precheck (`has_local_capability`, NOT `may_execute`) BEFORE the
/// admission engine runs: the handler stays dark and S receives 0x0009. `call`
/// blocks on the denial response, so the witness is race-free.
#[tokio::test]
async fn live_two_node_protected_missing_local_capability_denies() {
    const CALLER_SEED: [u8; 32] = [0x1du8; 32];
    let server = build_node_with(EntityKeypair::generate()).await;
    let caller = build_node_with(EntityKeypair::from_bytes(CALLER_SEED)).await;
    bring_up(&caller, &server).await;
    let (org_b, dir) = install_authority(&server, "notag");
    let org_a = OrgKeypair::from_bytes([0x7au8; 32]);
    let provider = server.entity_id().clone();

    let calls = Arc::new(AtomicUsize::new(0));
    let _serve = server
        .serve_rpc_protected(
            "svc",
            Arc::new(DarkHandler {
                calls: calls.clone(),
            }),
            OrgAdmission::CrossOrgGranted,
            Arc::new(|_| true),
        )
        .expect("serve protected");

    // Supersede the provider self-fold with an empty higher-version announcement:
    // the protected registration remains, but the provider no longer carries the
    // local capability tag.
    server.test_inject_capability_announcement(CapabilityAnnouncement::new(
        server.node_id(),
        server.entity_id().clone(),
        100,
        CapabilitySet::new(),
    ));

    let intent = cross_org_invoke_intent(
        EntityKeypair::from_bytes(CALLER_SEED),
        &org_a,
        &org_b,
        provider.clone(),
        "svc",
        GrantTargetScope::ExactNode(provider),
    );
    let err = caller
        .call(
            server.node_id(),
            "svc",
            Bytes::from_static(b"ping"),
            CallOptions {
                org_proof_intent: Some(intent),
                deadline: Some(Instant::now() + Duration::from_secs(5)),
                ..Default::default()
            },
        )
        .await
        .expect_err("a valid proof to a provider missing its local tag must be denied");
    match err {
        RpcError::ServerError { status, .. } => {
            assert_eq!(
                status, 0x0009,
                "AdmissionDenied (0x0009), never a timeout masquerade",
            );
        }
        other => panic!("expected AdmissionDenied, got {other:?}"),
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "the handler stayed dark — the possession precheck denied before admission",
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// OA-4 slice 2 (Tier 1) — `AnyNodeOwnedBy(B)` reuse and boundary. A single B→A
/// INVOKE grant scoped `AnyNodeOwnedBy(B)` admits at TWO distinct B-owned
/// providers (reuse across discovered nodes), and is denied (0x0009) at a non-B
/// provider — a cryptographically valid proof the non-B owner never issued
/// (`ForeignIssuer`), which doubles as the Tier-1 valid-but-unauthorized cross-
/// org denial. The grant is INVOKE-only, so no audience secret is minted.
#[tokio::test]
async fn live_cross_org_any_node_owned_by_reuse_and_deny() {
    const CALLER_SEED: [u8; 32] = [0x1eu8; 32];
    let org_b = OrgKeypair::from_bytes([0x42u8; 32]);
    let org_a = OrgKeypair::from_bytes([0x7au8; 32]);
    let org_c = OrgKeypair::from_bytes([0x33u8; 32]);

    let caller = build_node_with(EntityKeypair::from_bytes(CALLER_SEED)).await;
    let (p2, dir2) = adopted_node(0x51, &org_b, "anyb-p2").await;
    let (p2b, dir2b) = adopted_node(0x52, &org_b, "anyb-p2b").await;
    let (pc, dirc) = adopted_node(0x53, &org_c, "anyb-pc").await;
    bring_up(&caller, &p2).await;
    bring_up(&caller, &p2b).await;
    bring_up(&caller, &pc).await;

    // One reusable INVOKE grant scoped to ANY B-owned node.
    let cap = CapabilityAuthorityId::for_tag("nrpc:svc");
    let (grant, secret) = OrgCapabilityGrant::try_issue(
        &org_b,
        org_a.org_id(),
        cap,
        GrantRights::INVOKE,
        GrantTargetScope::AnyNodeOwnedBy(org_b.org_id()),
        3600,
    )
    .expect("issue AnyNodeOwnedBy grant");
    assert!(secret.is_none(), "INVOKE-only mints no audience material");

    let p2_calls = Arc::new(AtomicUsize::new(0));
    let _s2 = p2
        .serve_rpc_protected(
            "svc",
            Arc::new(DarkHandler {
                calls: p2_calls.clone(),
            }),
            OrgAdmission::CrossOrgGranted,
            Arc::new(|_| true),
        )
        .expect("serve p2");
    let p2b_calls = Arc::new(AtomicUsize::new(0));
    let _s2b = p2b
        .serve_rpc_protected(
            "svc",
            Arc::new(DarkHandler {
                calls: p2b_calls.clone(),
            }),
            OrgAdmission::CrossOrgGranted,
            Arc::new(|_| true),
        )
        .expect("serve p2b");
    let pc_calls = Arc::new(AtomicUsize::new(0));
    let _sc = pc
        .serve_rpc_protected(
            "svc",
            Arc::new(DarkHandler {
                calls: pc_calls.clone(),
            }),
            OrgAdmission::CrossOrgGranted,
            Arc::new(|_| true),
        )
        .expect("serve pc");

    let opts = || CallOptions {
        deadline: Some(Instant::now() + Duration::from_secs(5)),
        ..Default::default()
    };

    // Reuse: the SAME grant admits at both B-owned providers.
    caller
        .call(
            p2.node_id(),
            "svc",
            Bytes::from_static(b"ping"),
            CallOptions {
                org_proof_intent: Some(cross_org_intent_with_grant(
                    EntityKeypair::from_bytes(CALLER_SEED),
                    &org_a,
                    p2.entity_id().clone(),
                    org_b.org_id(),
                    "svc",
                    grant.clone(),
                )),
                ..opts()
            },
        )
        .await
        .expect("admit at the first B-owned node");
    caller
        .call(
            p2b.node_id(),
            "svc",
            Bytes::from_static(b"ping"),
            CallOptions {
                org_proof_intent: Some(cross_org_intent_with_grant(
                    EntityKeypair::from_bytes(CALLER_SEED),
                    &org_a,
                    p2b.entity_id().clone(),
                    org_b.org_id(),
                    "svc",
                    grant.clone(),
                )),
                ..opts()
            },
        )
        .await
        .expect("admit at the second B-owned node (grant reuse)");
    assert_eq!(p2_calls.load(Ordering::SeqCst), 1, "P₂ handler ran once");
    assert_eq!(
        p2b_calls.load(Ordering::SeqCst),
        1,
        "the second B-owned node's handler ran once — one grant, reused",
    );

    // Boundary: the B-issued grant has no authority at a non-B provider.
    let err = caller
        .call(
            pc.node_id(),
            "svc",
            Bytes::from_static(b"ping"),
            CallOptions {
                org_proof_intent: Some(cross_org_intent_with_grant(
                    EntityKeypair::from_bytes(CALLER_SEED),
                    &org_a,
                    pc.entity_id().clone(),
                    org_c.org_id(),
                    "svc",
                    grant.clone(),
                )),
                ..opts()
            },
        )
        .await
        .expect_err("a B-issued grant must be denied at a non-B provider");
    match err {
        RpcError::ServerError { status, .. } => {
            assert_eq!(status, 0x0009, "AdmissionDenied at the non-B provider");
        }
        other => panic!("expected AdmissionDenied, got {other:?}"),
    }
    assert_eq!(
        pc_calls.load(Ordering::SeqCst),
        0,
        "the non-B provider's handler stayed dark",
    );

    for d in [dir2, dir2b, dirc] {
        let _ = std::fs::remove_dir_all(&d);
    }
}

/// OA-4 slice 3 (Tier 1) — the representative live OwnerDelegated denial:
/// membership-only. The caller holds a valid membership AND a dispatcher grant,
/// but the dispatcher grant is scoped to a DIFFERENT capability, so it does not
/// empower this call → `DispatcherGrantScope`. The handler stays dark; S sees
/// 0x0009. (Membership alone never confers invocation authority.)
#[tokio::test]
async fn live_two_node_owner_delegated_membership_only_denied() {
    const CALLER_SEED: [u8; 32] = [0x2du8; 32];
    let server = build_node_with(EntityKeypair::generate()).await;
    let caller = build_node_with(EntityKeypair::from_bytes(CALLER_SEED)).await;
    bring_up(&caller, &server).await;
    let (org_b, dir) = install_authority(&server, "memonly");
    let provider = server.entity_id().clone();
    let caller_entity = caller.entity_id().clone();

    let calls = Arc::new(AtomicUsize::new(0));
    let _serve = server
        .serve_rpc_protected(
            "svc",
            Arc::new(DarkHandler {
                calls: calls.clone(),
            }),
            OrgAdmission::OwnerDelegated,
            Arc::new(|_| true),
        )
        .expect("serve protected");

    // Valid membership + dispatcher grant, but the dispatcher covers a DIFFERENT
    // capability, so it does not empower nrpc:svc.
    let membership =
        OrgMembershipCert::try_issue(&org_b, caller_entity.clone(), 1, 3600).expect("membership");
    let dispatcher = OrgDispatcherGrant::try_issue(
        &org_b,
        caller_entity,
        DispatcherScope::Exact(CapabilityAuthorityId::for_tag("nrpc:elsewhere")),
        3600,
    )
    .expect("dispatcher");
    let intent = OrgProofIntent {
        caller: Arc::new(EntityKeypair::from_bytes(CALLER_SEED)),
        membership,
        dispatcher,
        capability_grant: None,
        acting_org: org_b.org_id(),
        provider_owner_org: org_b.org_id(),
        provider,
        capability: CapabilityAuthorityId::for_tag("nrpc:svc"),
        proof_ttl_secs: 30,
    };
    let err = caller
        .call(
            server.node_id(),
            "svc",
            Bytes::from_static(b"ping"),
            CallOptions {
                org_proof_intent: Some(intent),
                deadline: Some(Instant::now() + Duration::from_secs(5)),
                ..Default::default()
            },
        )
        .await
        .expect_err("a membership-only proof must be denied");
    match err {
        RpcError::ServerError { status, .. } => assert_eq!(status, 0x0009, "AdmissionDenied"),
        other => panic!("expected AdmissionDenied, got {other:?}"),
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0, "the handler stayed dark");

    let _ = std::fs::remove_dir_all(&dir);
}

/// OA-4 slice 3 (Tier 1) — registering a PROTECTED capability leaves PUBLIC
/// capabilities unchanged. The provider serves a public service AND a protected
/// one on the same node; a public call carrying NO proof still succeeds normally
/// (the legacy `may_execute` path, public handler never sees a proof header),
/// while the protected service still denies an unproven call. Registering the
/// protected capability did not alter the public one's behavior.
#[tokio::test]
async fn live_two_node_public_capability_unchanged_beside_protected() {
    const CALLER_SEED: [u8; 32] = [0x2eu8; 32];
    let server = build_node_with(EntityKeypair::generate()).await;
    let caller = build_node_with(EntityKeypair::from_bytes(CALLER_SEED)).await;
    bring_up(&caller, &server).await;
    let (_org_b, dir) = install_authority(&server, "pubunchanged");

    let pub_calls = Arc::new(AtomicUsize::new(0));
    let saw_proof = Arc::new(AtomicBool::new(false));
    let _pub = server
        .serve_rpc(
            "pub",
            Arc::new(HeaderSpyHandler {
                calls: pub_calls.clone(),
                saw_proof: saw_proof.clone(),
            }),
        )
        .expect("serve public");
    let prot_calls = Arc::new(AtomicUsize::new(0));
    let _prot = server
        .serve_rpc_protected(
            "svc",
            Arc::new(DarkHandler {
                calls: prot_calls.clone(),
            }),
            OrgAdmission::OwnerDelegated,
            Arc::new(|_| true),
        )
        .expect("serve protected");

    // The public call, with NO proof, succeeds — unchanged by the protected reg.
    let reply = caller
        .call(
            server.node_id(),
            "pub",
            Bytes::from_static(b"ping"),
            CallOptions {
                deadline: Some(Instant::now() + Duration::from_secs(5)),
                ..Default::default()
            },
        )
        .await
        .expect("the public call succeeds without a proof");
    assert_eq!(reply.body.as_ref(), b"pong");
    assert_eq!(
        pub_calls.load(Ordering::SeqCst),
        1,
        "public handler ran once"
    );
    assert!(
        !saw_proof.load(Ordering::SeqCst),
        "the public handler saw no org-admission proof header",
    );

    // The protected service still requires a proof: an unproven call is denied.
    let err = caller
        .call(
            server.node_id(),
            "svc",
            Bytes::from_static(b"ping"),
            CallOptions {
                deadline: Some(Instant::now() + Duration::from_secs(5)),
                ..Default::default()
            },
        )
        .await
        .expect_err("the protected service still requires a proof");
    match err {
        RpcError::ServerError { status, .. } => assert_eq!(status, 0x0009, "AdmissionDenied"),
        other => panic!("expected AdmissionDenied, got {other:?}"),
    }
    assert_eq!(
        prot_calls.load(Ordering::SeqCst),
        0,
        "the protected handler stayed dark",
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// OA-4 slice 3 (Tier 1) — the OA-1 restart chain through the LIVE admission
/// gate. A revocation floor of 5 is raised for the caller and persisted; the
/// WHOLE node authority is reopened FROM DISK (the restart), and a lower
/// (generation 3) bundle is a no-op. A below-floor (generation 4) membership
/// cert is then denied `MembershipRevoked` at the live gate (0x0009, handler
/// dark), while an at-floor (generation 5) cert admits — proving persisted floor
/// state reaches the real provider admission path, not just the `may_execute`
/// projection.
#[tokio::test]
async fn live_two_node_owner_delegated_floor_survives_restart_denies() {
    const CALLER_SEED: [u8; 32] = [0x2fu8; 32];
    let server = build_node_with(EntityKeypair::generate()).await;
    let caller = build_node_with(EntityKeypair::from_bytes(CALLER_SEED)).await;
    bring_up(&caller, &server).await;
    let node_entity = server.entity_id().clone();
    let caller_entity = caller.entity_id().clone();
    let org_b = OrgKeypair::from_bytes([0x42u8; 32]);

    // Adopt the provider authority; raise a floor of 5 for the caller, persisted.
    let node_cert =
        OrgMembershipCert::try_issue(&org_b, node_entity.clone(), 1, 3600).expect("node cert");
    let dir = std::env::temp_dir().join(format!("net-oa4-restart-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let authority =
        NodeAuthority::adopt(&dir, node_cert, &node_entity, 0, None).expect("adopt authority");
    let mut floors = std::collections::BTreeMap::new();
    floors.insert(caller_entity.clone(), 5u32);
    authority
        .revocation
        .apply_bundle(&OrgRevocationBundle::try_issue(&org_b, &floors).expect("bundle 5"))
        .expect("apply floor 5");
    server
        .install_node_authority(Arc::new(authority))
        .expect("install authority");

    let calls = Arc::new(AtomicUsize::new(0));
    let _serve = server
        .serve_rpc_protected(
            "svc",
            Arc::new(DarkHandler {
                calls: calls.clone(),
            }),
            OrgAdmission::OwnerDelegated,
            Arc::new(|_| true),
        )
        .expect("serve protected");

    // RESTART: reopen the entire authority from disk — the floor must survive.
    // A lower (generation 3) bundle must be a no-op.
    let reopened = NodeAuthority::open(&dir, &node_entity).expect("reopen authority");
    assert_eq!(
        reopened
            .revocation
            .floor_for(&org_b.org_id(), &caller_entity),
        5,
        "floor 5 survives the restart",
    );
    let mut lower = std::collections::BTreeMap::new();
    lower.insert(caller_entity.clone(), 3u32);
    reopened
        .revocation
        .apply_bundle(&OrgRevocationBundle::try_issue(&org_b, &lower).expect("bundle 3"))
        .expect("lower bundle is a no-op");
    assert_eq!(
        reopened
            .revocation
            .floor_for(&org_b.org_id(), &caller_entity),
        5,
        "floor stays 5 after a lower bundle",
    );
    server
        .install_node_authority(Arc::new(reopened))
        .expect("install reopened authority");

    // Below the floor (generation 4): denied at the live gate.
    let intent4 = owner_delegated_intent_gen(
        EntityKeypair::from_bytes(CALLER_SEED),
        &org_b,
        node_entity.clone(),
        "svc",
        4,
    );
    let err = caller
        .call(
            server.node_id(),
            "svc",
            Bytes::from_static(b"ping"),
            CallOptions {
                org_proof_intent: Some(intent4),
                deadline: Some(Instant::now() + Duration::from_secs(5)),
                ..Default::default()
            },
        )
        .await
        .expect_err("a below-floor cert must be denied after the restart");
    match err {
        RpcError::ServerError { status, .. } => {
            assert_eq!(status, 0x0009, "MembershipRevoked at the live gate");
        }
        other => panic!("expected AdmissionDenied, got {other:?}"),
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "the handler stayed dark for the below-floor call",
    );

    // At the floor (generation 5): admitted — proving the floor is exactly 5.
    let intent5 = owner_delegated_intent_gen(
        EntityKeypair::from_bytes(CALLER_SEED),
        &org_b,
        node_entity.clone(),
        "svc",
        5,
    );
    caller
        .call(
            server.node_id(),
            "svc",
            Bytes::from_static(b"ping"),
            CallOptions {
                org_proof_intent: Some(intent5),
                deadline: Some(Instant::now() + Duration::from_secs(5)),
                ..Default::default()
            },
        )
        .await
        .expect("an at-floor cert admits");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the at-floor call reached the handler",
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ============================================================================
// OA-4 slice 4 — GrantedAudience composition matrix (Tier 1)
//
// Private discovery (OA3-4b2) composed with cross-org invocation (OA-4). A
// consumer resolves the provider from an encrypted grant envelope, then the SAME
// grant's INVOKE right admits the call — while DISCOVER-only, wrong-dispatcher,
// and provider-policy-veto all resolve but fail invocation. Rows already
// witnessed by OA3-4b2 (no-credential, copied-credential, wrong provider
// owner/target, wrong handle/capability, stale registration, plaintext absence,
// observer opacity, AD-transplant, post-rotation) are referenced, not repeated.
// ============================================================================

/// An org-B provider serving one granted-audience `svc` with `handler` and an
/// admit-all policy, authority installed. Returns the node, its serve handle
/// (kept alive by the caller), provider entity, and scratch dir.
async fn granted_service_provider<H: RpcHandler>(
    seed: u8,
    svc: &str,
    handler: Arc<H>,
) -> (Arc<MeshNode>, ServeHandle, EntityId, std::path::PathBuf) {
    let p = build_node_with(EntityKeypair::from_bytes([seed; 32])).await;
    let entity = p.entity_id().clone();
    let org_b = OrgKeypair::from_bytes([0x42u8; 32]);
    let cert = OrgMembershipCert::try_issue(&org_b, entity.clone(), 1, 3600).expect("cert");
    let dir = std::env::temp_dir().join(format!(
        "net-oa4-granted-{svc}-{seed}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let authority = NodeAuthority::adopt(&dir, cert, &entity, 0, None).expect("adopt");
    p.install_node_authority(Arc::new(authority))
        .expect("install authority");
    let handle = p
        .serve_rpc_granted(svc, handler, Arc::new(|_| true))
        .expect("granted serve");
    (p, handle, entity, dir)
}

/// OA-4 slice 4 — the centerpiece: a consumer holding a DISCOVER|INVOKE grant
/// privately RESOLVES the exact provider from an encrypted announcement AND
/// INVOKES that exact provider under the SAME grant. (Plaintext absence and
/// observer opacity are referenced to the OA3-4b2 witnesses.)
#[tokio::test]
async fn live_granted_audience_discovers_then_invokes() {
    let org_b = OrgKeypair::from_bytes([0x42u8; 32]);
    let org_a = OrgKeypair::from_bytes([0x7au8; 32]);
    let now = unix_now();
    let cap = CapabilityAuthorityId::for_tag("nrpc:svc");

    let calls = Arc::new(AtomicUsize::new(0));
    let (p2, _serve, p_entity, p_dir) = granted_service_provider(
        0x61,
        "svc",
        Arc::new(DarkHandler {
            calls: calls.clone(),
        }),
    )
    .await;
    let (a, a_dir) = adopted_node(0x62, &org_a, "gdisc").await;
    bring_up(&a, &p2).await;

    // DISCOVER|INVOKE grant B→A for exactly P₂.
    let (grant, secret) = OrgCapabilityGrant::try_issue(
        &org_b,
        org_a.org_id(),
        cap,
        GrantRights::DISCOVER.union(GrantRights::INVOKE),
        GrantTargetScope::ExactNode(p_entity.clone()),
        3600,
    )
    .expect("grant");
    let secret = secret.expect("a DISCOVER grant mints an audience secret");
    let grant_id = grant.grant_id;

    // Private discovery: A ingests the encrypted announcement and resolves P₂.
    a.install_consumer_grant_audience(grant.clone(), copy_secret(&secret))
        .expect("install consumer grant");
    let p_kp = EntityKeypair::from_bytes([0x61u8; 32]);
    let env = granted_envelope_bytes(&p_kp, &org_b, &grant, &secret, "svc", now);
    a.ingest_scoped_announcement_for_test(&env);
    assert_eq!(
        a.scoped_granted_providers_for_test(&grant_id, now),
        vec![p_entity.clone()],
        "A privately resolves exactly P₂ under the DISCOVER right",
    );

    // Invocation: A invokes the exact P₂ under the same grant's INVOKE right.
    let intent = cross_org_intent_with_grant(
        EntityKeypair::from_bytes([0x62u8; 32]),
        &org_a,
        p_entity.clone(),
        org_b.org_id(),
        "svc",
        grant.clone(),
    );
    a.call(
        p2.node_id(),
        "svc",
        Bytes::from_static(b"ping"),
        CallOptions {
            org_proof_intent: Some(intent),
            deadline: Some(Instant::now() + Duration::from_secs(5)),
            ..Default::default()
        },
    )
    .await
    .expect("the granted invocation admits");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the exact P₂ handler ran once under the same grant",
    );

    let _ = std::fs::remove_dir_all(&p_dir);
    let _ = std::fs::remove_dir_all(&a_dir);
}

/// OA-4 slice 4 — DISCOVER-only resolves but cannot invoke (decrypt-without-
/// invoke). The consumer opens the envelope and resolves P₂, but the grant
/// carries no INVOKE right, so the invocation is denied `InsufficientRights`
/// (0x0009, handler dark). Discovery authority never confers invocation.
#[tokio::test]
async fn live_granted_audience_discover_only_resolves_but_cannot_invoke() {
    let org_b = OrgKeypair::from_bytes([0x42u8; 32]);
    let org_a = OrgKeypair::from_bytes([0x7au8; 32]);
    let now = unix_now();

    let calls = Arc::new(AtomicUsize::new(0));
    let (p2, _serve, p_entity, p_dir) = granted_service_provider(
        0x63,
        "svc",
        Arc::new(DarkHandler {
            calls: calls.clone(),
        }),
    )
    .await;
    let (a, a_dir) = adopted_node(0x64, &org_a, "gdonly").await;
    bring_up(&a, &p2).await;

    // A DISCOVER-only grant (no INVOKE) mints an audience secret.
    let (grant, secret) = cross_org_grant(&org_b, &org_a, &p_entity, "svc");
    let grant_id = grant.grant_id;
    a.install_consumer_grant_audience(grant.clone(), copy_secret(&secret))
        .expect("install consumer grant");
    let p_kp = EntityKeypair::from_bytes([0x63u8; 32]);
    let env = granted_envelope_bytes(&p_kp, &org_b, &grant, &secret, "svc", now);
    a.ingest_scoped_announcement_for_test(&env);
    assert_eq!(
        a.scoped_granted_providers_for_test(&grant_id, now),
        vec![p_entity.clone()],
        "A resolves P₂ under the DISCOVER right",
    );

    // The very same grant cannot invoke: it holds no INVOKE right.
    let intent = cross_org_intent_with_grant(
        EntityKeypair::from_bytes([0x64u8; 32]),
        &org_a,
        p_entity.clone(),
        org_b.org_id(),
        "svc",
        grant.clone(),
    );
    let err = a
        .call(
            p2.node_id(),
            "svc",
            Bytes::from_static(b"ping"),
            CallOptions {
                org_proof_intent: Some(intent),
                deadline: Some(Instant::now() + Duration::from_secs(5)),
                ..Default::default()
            },
        )
        .await
        .expect_err("a DISCOVER-only grant cannot invoke");
    match err {
        RpcError::ServerError { status, .. } => assert_eq!(status, 0x0009, "AdmissionDenied"),
        other => panic!("expected AdmissionDenied, got {other:?}"),
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "the handler stayed dark — discovery did not confer invocation",
    );

    let _ = std::fs::remove_dir_all(&p_dir);
    let _ = std::fs::remove_dir_all(&a_dir);
}

/// OA-4 slice 4 — a wrong dispatcher resolves but the invocation is denied. The
/// consumer resolves P₂ under a DISCOVER|INVOKE grant, but presents a dispatcher
/// grant scoped to a DIFFERENT capability → `DispatcherGrantScope` (post-
/// discovery invocation denial; discovery and invocation authority stay
/// separate).
#[tokio::test]
async fn live_granted_audience_wrong_dispatcher_resolves_but_invocation_denied() {
    let org_b = OrgKeypair::from_bytes([0x42u8; 32]);
    let org_a = OrgKeypair::from_bytes([0x7au8; 32]);
    let now = unix_now();
    let cap = CapabilityAuthorityId::for_tag("nrpc:svc");

    let calls = Arc::new(AtomicUsize::new(0));
    let (p2, _serve, p_entity, p_dir) = granted_service_provider(
        0x65,
        "svc",
        Arc::new(DarkHandler {
            calls: calls.clone(),
        }),
    )
    .await;
    let (a, a_dir) = adopted_node(0x66, &org_a, "gwrongdisp").await;
    bring_up(&a, &p2).await;

    let (grant, secret) = OrgCapabilityGrant::try_issue(
        &org_b,
        org_a.org_id(),
        cap,
        GrantRights::DISCOVER.union(GrantRights::INVOKE),
        GrantTargetScope::ExactNode(p_entity.clone()),
        3600,
    )
    .expect("grant");
    let secret = secret.expect("secret");
    let grant_id = grant.grant_id;
    a.install_consumer_grant_audience(grant.clone(), copy_secret(&secret))
        .expect("install consumer grant");
    let p_kp = EntityKeypair::from_bytes([0x65u8; 32]);
    let env = granted_envelope_bytes(&p_kp, &org_b, &grant, &secret, "svc", now);
    a.ingest_scoped_announcement_for_test(&env);
    assert_eq!(
        a.scoped_granted_providers_for_test(&grant_id, now),
        vec![p_entity.clone()],
        "A resolves P₂",
    );

    // Invoke with a dispatcher grant scoped to a DIFFERENT capability.
    let a_entity = a.entity_id().clone();
    let intent = OrgProofIntent {
        caller: Arc::new(EntityKeypair::from_bytes([0x66u8; 32])),
        membership: OrgMembershipCert::try_issue(&org_a, a_entity.clone(), 1, 3600)
            .expect("membership"),
        dispatcher: OrgDispatcherGrant::try_issue(
            &org_a,
            a_entity,
            DispatcherScope::Exact(CapabilityAuthorityId::for_tag("nrpc:elsewhere")),
            3600,
        )
        .expect("dispatcher"),
        capability_grant: Some(grant.clone()),
        acting_org: org_a.org_id(),
        provider_owner_org: org_b.org_id(),
        provider: p_entity.clone(),
        capability: cap,
        proof_ttl_secs: 30,
    };
    let err = a
        .call(
            p2.node_id(),
            "svc",
            Bytes::from_static(b"ping"),
            CallOptions {
                org_proof_intent: Some(intent),
                deadline: Some(Instant::now() + Duration::from_secs(5)),
                ..Default::default()
            },
        )
        .await
        .expect_err("a dispatcher scoped elsewhere cannot invoke");
    match err {
        RpcError::ServerError { status, .. } => assert_eq!(status, 0x0009, "AdmissionDenied"),
        other => panic!("expected AdmissionDenied, got {other:?}"),
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0, "the handler stayed dark");

    let _ = std::fs::remove_dir_all(&p_dir);
    let _ = std::fs::remove_dir_all(&a_dir);
}

/// OA-4 slice 4 — provider policy is final on the granted/cross-org path too. A
/// consumer with a valid DISCOVER|INVOKE grant resolves P₂, but the provider's
/// policy vetoes the call → denied (0x0009, handler dark), even though every
/// credential is valid.
#[tokio::test]
async fn live_granted_audience_provider_policy_final() {
    let org_b = OrgKeypair::from_bytes([0x42u8; 32]);
    let org_a = OrgKeypair::from_bytes([0x7au8; 32]);
    let now = unix_now();
    let cap = CapabilityAuthorityId::for_tag("nrpc:svc");

    // Provider serves the granted service with a VETO policy.
    let p2 = build_node_with(EntityKeypair::from_bytes([0x67u8; 32])).await;
    let p_entity = p2.entity_id().clone();
    let node_cert =
        OrgMembershipCert::try_issue(&org_b, p_entity.clone(), 1, 3600).expect("node cert");
    let p_dir = std::env::temp_dir().join(format!("net-oa4-granted-veto-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p_dir);
    let authority =
        NodeAuthority::adopt(&p_dir, node_cert, &p_entity, 0, None).expect("adopt authority");
    p2.install_node_authority(Arc::new(authority))
        .expect("install authority");
    let calls = Arc::new(AtomicUsize::new(0));
    let _serve = p2
        .serve_rpc_granted(
            "svc",
            Arc::new(DarkHandler {
                calls: calls.clone(),
            }),
            Arc::new(|_| false),
        )
        .expect("granted serve with veto policy");

    let (a, a_dir) = adopted_node(0x68, &org_a, "gveto").await;
    bring_up(&a, &p2).await;

    let (grant, secret) = OrgCapabilityGrant::try_issue(
        &org_b,
        org_a.org_id(),
        cap,
        GrantRights::DISCOVER.union(GrantRights::INVOKE),
        GrantTargetScope::ExactNode(p_entity.clone()),
        3600,
    )
    .expect("grant");
    let secret = secret.expect("secret");
    let grant_id = grant.grant_id;
    a.install_consumer_grant_audience(grant.clone(), copy_secret(&secret))
        .expect("install consumer grant");
    let p_kp = EntityKeypair::from_bytes([0x67u8; 32]);
    let env = granted_envelope_bytes(&p_kp, &org_b, &grant, &secret, "svc", now);
    a.ingest_scoped_announcement_for_test(&env);
    assert_eq!(
        a.scoped_granted_providers_for_test(&grant_id, now),
        vec![p_entity.clone()],
        "A resolves P₂",
    );

    let intent = cross_org_intent_with_grant(
        EntityKeypair::from_bytes([0x68u8; 32]),
        &org_a,
        p_entity.clone(),
        org_b.org_id(),
        "svc",
        grant.clone(),
    );
    let err = a
        .call(
            p2.node_id(),
            "svc",
            Bytes::from_static(b"ping"),
            CallOptions {
                org_proof_intent: Some(intent),
                deadline: Some(Instant::now() + Duration::from_secs(5)),
                ..Default::default()
            },
        )
        .await
        .expect_err("the provider policy vetoes the call");
    match err {
        RpcError::ServerError { status, .. } => assert_eq!(status, 0x0009, "AdmissionDenied"),
        other => panic!("expected AdmissionDenied, got {other:?}"),
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "the handler stayed dark under the policy veto",
    );

    let _ = std::fs::remove_dir_all(&p_dir);
    let _ = std::fs::remove_dir_all(&a_dir);
}

/// OA-4 slice 4 — INVOKE-only grants hold no audience material BY CONSTRUCTION.
/// Issuance yields no discovery binding and no secret, so there is nothing to
/// install as an audience, no envelope to emit, and no private resolution. The
/// registry refusal of a fabricated install is pinned by
/// `org_grant_registry::invoke_only_grant_is_refused` (Tier 3).
#[test]
fn invoke_only_grant_carries_no_discovery_material() {
    let org_b = OrgKeypair::from_bytes([0x42u8; 32]);
    let org_a = OrgKeypair::from_bytes([0x7au8; 32]);
    let provider = EntityKeypair::from_bytes([0x69u8; 32]).entity_id().clone();
    let (grant, secret) = OrgCapabilityGrant::try_issue(
        &org_b,
        org_a.org_id(),
        CapabilityAuthorityId::for_tag("nrpc:svc"),
        GrantRights::INVOKE,
        GrantTargetScope::ExactNode(provider),
        3600,
    )
    .expect("issue INVOKE-only grant");
    assert!(
        secret.is_none(),
        "an INVOKE-only grant mints no audience secret",
    );
    assert!(
        !grant.permits_discover(),
        "an INVOKE-only grant confers no discovery right",
    );
    assert!(grant.permits_invoke(), "it does carry INVOKE");
}
