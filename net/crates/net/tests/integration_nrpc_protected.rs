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
use net::adapter::net::behavior::org::{OrgKeypair, OrgMembershipCert};
use net::adapter::net::behavior::org_admission::OrgAdmission;
use net::adapter::net::behavior::org_authority::NodeAuthority;
use net::adapter::net::behavior::org_grant::{
    CapabilityAuthorityId, DispatcherScope, OrgDispatcherGrant,
};
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
    expected_provider: EntityId,
}

#[async_trait::async_trait]
impl RpcHandler for AdmitHandler {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(admitted) = ctx.org_admission.as_ref() {
            self.saw_admission.store(true, Ordering::SeqCst);
            if admitted.caller == self.expected_caller
                && admitted.provider == self.expected_provider
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
                expected_provider: provider.clone(),
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
        "four-party attribution matches the caller and provider identities",
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
