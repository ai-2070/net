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
use net::adapter::net::behavior::org::{OrgId, OrgKeypair, OrgMembershipCert};
use net::adapter::net::behavior::org_admission::OrgAdmission;
use net::adapter::net::behavior::org_authority::NodeAuthority;
use net::adapter::net::behavior::org_grant::{
    CapabilityAuthorityId, DispatcherScope, OrgDispatcherGrant,
};
use net::adapter::net::behavior::CapabilityAnnouncement;
use net::adapter::net::cortex::{
    RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus,
};
use net::adapter::net::identity::EntityId;
use net::adapter::net::mesh_rpc::{CallOptions, OrgProofIntent, RpcError};
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
    let caller_entity = caller_kp.entity_id().clone();
    let cap = CapabilityAuthorityId::for_tag(&format!("nrpc:{service}"));
    let membership =
        OrgMembershipCert::try_issue(org_b, caller_entity.clone(), 1, 3600).expect("membership");
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
                message.as_bytes().len(),
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
                message.as_bytes().len(),
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
