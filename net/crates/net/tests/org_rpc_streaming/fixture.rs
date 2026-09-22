//! Test fixture for the Stage 1 protected-streaming witnesses.
//!
//! The node/authority/intent helpers are COPIED from
//! `tests/integration_nrpc_protected.rs:71-372` (the Stage 1 table's named
//! fixture range), adapted only for module form (`pub(crate)`, imports).
//! Additions for the streaming witnesses are marked "fixture addition".
//!
//! Session-binding rule (Main, S1.1a interim): bindings come from EXPLICIT
//! `NetSession::with_binding` establishments ([`session_bindings`]) — never
//! from a live `peer_session_binding()` accessor, which returns `None` until
//! the handshake-site migration lands.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::behavior::org::{OrgId, OrgKeypair, OrgMembershipCert};
use net::adapter::net::behavior::org_authority::NodeAuthority;
use net::adapter::net::behavior::org_grant::{
    CapabilityAuthorityId, DispatcherScope, GrantRights, GrantTargetScope, OrgCapabilityGrant,
    OrgDispatcherGrant,
};
use net::adapter::net::behavior::CapabilityAnnouncement;
use net::adapter::net::cortex::{
    RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus,
};
use net::adapter::net::identity::EntityId;
use net::adapter::net::mesh_rpc::OrgProofIntent;
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};
use net_wire::crypto::SessionKeys;
use net_wire::peer_addr::PeerAddr;
use net_wire::session::NetSession;

const PSK: [u8; 32] = [0x42u8; 32];
const TEST_BUFFER_SIZE: usize = 256 * 1024;
/// The proof header the provider strips before the handler sees the request.
pub(crate) const ORG_ADMISSION_HEADER: &str = "net-org-admission";

pub(crate) fn test_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(4, Duration::from_secs(4))
        .with_capability_gc_interval(Duration::from_millis(250));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: TEST_BUFFER_SIZE,
        recv_buffer_size: TEST_BUFFER_SIZE,
    };
    cfg
}

pub(crate) async fn build_node_with(keypair: EntityKeypair) -> Arc<MeshNode> {
    Arc::new(
        MeshNode::new(keypair, test_config())
            .await
            .expect("MeshNode::new"),
    )
}

/// Direct handshake: `a` (the connect initiator) → `b`, then start both.
pub(crate) async fn handshake_pair(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
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

/// Like [`build_node_with`] but with a short `min_announce_interval`.
pub(crate) async fn build_node_fast_announce(keypair: EntityKeypair) -> Arc<MeshNode> {
    let mut cfg = test_config();
    cfg.min_announce_interval = Duration::from_millis(50);
    Arc::new(MeshNode::new(keypair, cfg).await.expect("MeshNode::new"))
}

/// Establish a direct session WITHOUT starting either node's dispatch loop.
pub(crate) async fn connect_no_start(initiator: &Arc<MeshNode>, responder: &Arc<MeshNode>) {
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

pub(crate) async fn wait_until<F: Fn() -> bool>(limit: Duration, cond: F) -> bool {
    let start = Instant::now();
    while start.elapsed() < limit {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    cond()
}

/// Handshake the pair and drive both signed announcements so each node pins
/// the other's entity.
pub(crate) async fn bring_up(caller: &Arc<MeshNode>, server: &Arc<MeshNode>) {
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
pub(crate) fn install_authority(
    server: &Arc<MeshNode>,
    tag: &str,
) -> (OrgKeypair, std::path::PathBuf) {
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

/// Fold a hand-built restrictive `nrpc:<service>` announcement into each
/// node's capability index.
pub(crate) fn fold_restrictive_announcement(
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
pub(crate) fn owner_delegated_intent(
    caller_kp: EntityKeypair,
    org_b: &OrgKeypair,
    provider: EntityId,
    service: &str,
) -> OrgProofIntent {
    owner_delegated_intent_gen(caller_kp, org_b, provider, service, 1)
}

/// Like [`owner_delegated_intent`] but with an explicit membership
/// `generation`.
pub(crate) fn owner_delegated_intent_gen(
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

/// FIXTURE ADDITION — a CROSS-ORG intent: `caller_kp` is a member of org A,
/// holding a B→A `INVOKE` capability grant for `nrpc:<service>` on the exact
/// provider.
pub(crate) fn cross_org_intent(
    caller_kp: EntityKeypair,
    org_a: &OrgKeypair,
    org_b: &OrgKeypair,
    provider: EntityId,
    service: &str,
) -> OrgProofIntent {
    let caller_entity = caller_kp.entity_id().clone();
    let cap = CapabilityAuthorityId::for_tag(&format!("nrpc:{service}"));
    let membership =
        OrgMembershipCert::try_issue(org_a, caller_entity.clone(), 1, 3600).expect("membership");
    let dispatcher =
        OrgDispatcherGrant::try_issue(org_a, caller_entity, DispatcherScope::Exact(cap), 3600)
            .expect("dispatcher");
    let (capability_grant, _audience) = OrgCapabilityGrant::try_issue(
        org_b,
        org_a.org_id(),
        cap,
        GrantRights::INVOKE,
        GrantTargetScope::ExactNode(provider.clone()),
        3600,
    )
    .expect("capability grant");
    OrgProofIntent {
        caller: Arc::new(caller_kp),
        membership,
        dispatcher,
        capability_grant: Some(capability_grant),
        acting_org: org_a.org_id(),
        provider_owner_org: org_b.org_id(),
        provider,
        capability: cap,
        proof_ttl_secs: 30,
    }
}

/// FIXTURE ADDITION — two session bindings from EXPLICITLY-bound sessions
/// (Main's F1 fixture rule: `NetSession::with_binding`, never a live
/// accessor). The values are the full32-byte handshake hashes those
/// establishments carry — two distinct establishments stand in for the
/// original session and its replacement.
pub(crate) fn session_bindings() -> ([u8; 32], [u8; 32]) {
    let hash_a = [0xA1u8; 32];
    let hash_b = [0xB2u8; 32];
    let keys = SessionKeys {
        tx_key: [0x11; 32],
        rx_key: [0x22; 32],
        session_id: 1,
        remote_static_pub: [0; 32],
        route_hop_tx_key: [0x33; 32],
        route_hop_rx_key: [0x44; 32],
    };
    let addr: SocketAddr = "127.0.0.1:9".parse().unwrap();
    let first = NetSession::with_binding(keys.clone(), hash_a, PeerAddr::Udp(addr), 4, true);
    let second = NetSession::with_binding(keys, hash_b, PeerAddr::Udp(addr), 4, true);
    let first = first.handshake_binding().expect("explicitly-bound session");
    let second = second
        .handshake_binding()
        .expect("explicitly-bound session");
    (first, second)
}

/// Records the admission attribution the protected handler observes.
pub(crate) struct AdmitHandler {
    pub calls: Arc<AtomicUsize>,
    pub saw_admission: Arc<AtomicBool>,
    pub attribution_ok: Arc<AtomicBool>,
    pub proof_stripped: Arc<AtomicBool>,
    pub expected_caller: EntityId,
    pub expected_acting_org: OrgId,
    pub expected_provider_org: OrgId,
    pub expected_provider: EntityId,
    pub expected_capability: CapabilityAuthorityId,
}

#[async_trait::async_trait]
impl RpcHandler for AdmitHandler {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(admitted) = ctx.org_admission.as_ref() {
            self.saw_admission.store(true, Ordering::SeqCst);
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
pub(crate) struct DarkHandler {
    pub calls: Arc<AtomicUsize>,
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

/// §T2 — assert a handler stayed dark, and KEEPS being dark (bounded
/// polling window; the plan forbids shrinking it).
pub(crate) async fn assert_handler_stays_dark(calls: &Arc<AtomicUsize>, what: &str) {
    const SETTLE: Duration = Duration::from_millis(200);
    const STEP: Duration = Duration::from_millis(10);
    let deadline = Instant::now() + SETTLE;
    loop {
        let observed = calls.load(Ordering::SeqCst);
        assert_eq!(
            observed, 0,
            "{what}: the handler RAN ({observed} call(s)) despite the denial — \
             the request reached the fold even though the caller was denied",
        );
        if Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(STEP).await;
    }
}

/// FIXTURE ADDITION — an opening REQUEST payload for `service` with the given
/// streaming-flag shape and body (no admission header: the mint helper
/// appends exactly one).
pub(crate) fn opening_request(
    service: &str,
    streaming_flags: u16,
    body: &[u8],
) -> net::adapter::net::cortex::RpcRequestPayload {
    net::adapter::net::cortex::RpcRequestPayload {
        service: service.to_string(),
        deadline_ns: 0,
        flags: streaming_flags,
        headers: Vec::new(),
        body: Bytes::copy_from_slice(body),
    }
}
