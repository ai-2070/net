//! Helpers for the slice 1.4 (registry + revocation) witnesses.
//!
//! The unit-witness idiom of slice 1.3 continues: the witnesses drive the
//! PRODUCTION seams — the shared §3 helper (`admit_protected_opening`),
//! the registry's own `reserve`/`install` where the interleaving is the
//! property, and `RpcServerStreamingFold::apply_inbound_admitted`'s
//! lease-carrying form — against REAL `OrgRevocationStore` fixtures
//! (filesystem-backed; AV-9: scratch dirs are left behind, never deleted
//! mid-binary). Retirement is observed at the record's `retire_reason()`
//! (the moment the synchronous §2.3 boundary lands) and at the
//! supervisor's `terminal()`.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use parking_lot::Mutex;

use net::adapter::net::behavior::admission_clock::ClockSample;
use net::adapter::net::behavior::org::{OrgKeypair, OrgMembershipCert, OrgRevocationBundle};
use net::adapter::net::behavior::org_admission::{Admitted, OrgAdmission};
use net::adapter::net::behavior::org_admission_replay::AdmissionReplayGuard;
use net::adapter::net::behavior::org_authority::NodeAuthority;
use net::adapter::net::behavior::org_call::RpcCallShape;
use net::adapter::net::cortex::rpc::{
    ProtectedCallKey, ProtectedCallLease, ProtectedCallRegistry, ProtectedStreamCall, RpcContext,
    RpcHandlerError, RpcRequestPayload, RpcResponseSink, RpcServerStreamingFold,
    RpcStreamingHandler, SessionIdentity, StreamCallLifetime, StreamTerminalReason,
    DISPATCH_RPC_REQUEST, FLAG_RPC_STREAMING_RESPONSE, HEADER_NRPC_STREAM_WINDOW_INITIAL,
};
use net::adapter::net::cortex::EventMeta;
use net::adapter::net::mesh_rpc::{
    admit_protected_opening, test_sign_admission_proof, OpeningRefusal, OrgProofIntent,
    ProtectedOpeningOutcome,
};
use net::adapter::net::org_admission_gate::{OrgProviderPolicy, RegisteredRpcService};
use net::adapter::net::MeshNode;

pub(crate) const SERVICE: &str = "svc";

/// A provider-local `nrpc:svc` capability tag, matching the fixture's.
pub(crate) fn tag() -> String {
    format!("nrpc:{SERVICE}")
}

/// A protected registration with an explicit id (the registry records
/// bind to it — `ServeHandle` drop / `retire_registration` semantics).
pub(crate) fn owner_reg(registration_id: u64) -> RegisteredRpcService {
    let policy: OrgProviderPolicy = Arc::new(|_| true);
    RegisteredRpcService::protected(
        registration_id,
        Arc::from(SERVICE),
        OrgAdmission::OwnerDelegated,
        policy,
    )
    .expect("an owner-delegated registration is structurally valid")
}

/// A cross-org (`CrossOrgGranted`) registration — the sibling-of-another-
/// org witness admits its org-A member here.
pub(crate) fn granted_reg(registration_id: u64) -> RegisteredRpcService {
    let policy: OrgProviderPolicy = Arc::new(|_| true);
    RegisteredRpcService::protected(
        registration_id,
        Arc::from(SERVICE),
        OrgAdmission::CrossOrgGranted,
        policy,
    )
    .expect("a cross-org registration is structurally valid")
}

/// Install a node authority (real `OrgRevocationStore` on disk) and
/// return the authority too, so a witness can drive the store handles.
pub(crate) fn install_authority_owned(
    server: &Arc<MeshNode>,
    tag_name: &str,
) -> (OrgKeypair, Arc<NodeAuthority>, std::path::PathBuf) {
    let node_entity = server.entity_id().clone();
    let org_b = OrgKeypair::from_bytes([0x42u8; 32]);
    let node_cert =
        OrgMembershipCert::try_issue(&org_b, node_entity.clone(), 1, 3600).expect("node cert");
    let dir = std::env::temp_dir().join(format!("net-s14-{tag_name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let authority =
        NodeAuthority::adopt(&dir, node_cert, &node_entity, 0, None).expect("adopt authority");
    let authority = Arc::new(authority);
    server
        .install_node_authority(authority.clone())
        .expect("install authority");
    (org_b, authority, dir)
}

/// Raise `(org_b, member) → generation` through the REAL store's
/// `apply_bundle` — the publication runs every subscriber synchronously
/// before returning (the §2.3 boundary under test).
pub(crate) fn raise_floor(
    store: &Arc<net::adapter::net::behavior::org_revocation::OrgRevocationStore>,
    org: &OrgKeypair,
    member: net::adapter::net::identity::EntityId,
    generation: u32,
) {
    let mut floors = BTreeMap::new();
    floors.insert(member, generation);
    let bundle = OrgRevocationBundle::try_issue(org, &floors).expect("issue floor bundle");
    store.apply_bundle(&bundle).expect("apply floor raise");
}

/// Mint a server-streaming opening through the REAL caller-side helper and
/// return the wire frame (proof header included) it was minted over.
pub(crate) fn mint_ss_opening(
    intent: &OrgProofIntent,
    binding: [u8; 32],
    call_id: u64,
    origin: u64,
    window: Option<u32>,
    body: &[u8],
) -> (Bytes, RpcRequestPayload) {
    let mut req = RpcRequestPayload {
        service: SERVICE.to_string(),
        deadline_ns: 0,
        flags: FLAG_RPC_STREAMING_RESPONSE,
        headers: vec![],
        body: Bytes::copy_from_slice(body),
    };
    if let Some(w) = window {
        req.headers.push((
            HEADER_NRPC_STREAM_WINDOW_INITIAL.to_string(),
            w.to_string().into_bytes(),
        ));
    }
    let (name, proof) = test_sign_admission_proof(
        intent,
        call_id,
        &req,
        RpcCallShape::ServerStreaming,
        Some(binding),
    )
    .expect("mint the streaming proof header");
    req.headers.push((name, proof));
    let meta = EventMeta::new(DISPATCH_RPC_REQUEST, 0, origin, call_id, 0);
    let mut buf = Vec::new();
    buf.extend_from_slice(&meta.to_bytes());
    net::adapter::net::cortex::rpc::encode_rpc_route(&mut buf, 0);
    req.encode_into(&mut buf);
    (Bytes::from(buf), req)
}

/// The synthetic session identity most witnesses bind their records to
/// (the fold keys on it; only the session-replacement witness uses a REAL
/// establishment).
pub(crate) fn synthetic_session() -> SessionIdentity {
    SessionIdentity {
        peer: 0xA,
        session_id: 0x51,
        establishment: Some([0xA1u8; 32]),
    }
}

/// One §3 admission transaction through the REAL shared helper: minted
/// proof → `admit_protected_opening` (reserve → verify → install).
/// Returns the frame (for the fold seam) plus the verified facts and the
/// installed lease.
#[allow(clippy::too_many_arguments)]
pub(crate) fn admit_ss(
    mesh: &Arc<MeshNode>,
    intent: &OrgProofIntent,
    reg: &RegisteredRpcService,
    replay: &AdmissionReplayGuard,
    clock: ClockSample,
    call_id: u64,
    origin: u64,
    session: &SessionIdentity,
    binding: [u8; 32],
    window: Option<u32>,
    body: &[u8],
) -> Result<(Bytes, Admitted, ProtectedCallLease), OpeningRefusal> {
    let (frame, _req) = mint_ss_opening(intent, binding, call_id, origin, window, body);
    // The PROVIDER-side sample comes after the caller-side mint — in real
    // time the caller signs before the frame ever reaches the provider,
    // and the proof-TTL ceiling (`check_proof_expiry_at`) compares the
    // claimed expiry against the admission's ONE sample. The caller's
    // `monotonic` base is preserved so a witness can jump replay
    // retention deterministically (the clock pairing `admission_clock.rs`
    // documents: freshness reads `wall_ns`, retention derives from
    // `monotonic`).
    let clock = ClockSample {
        wall_ns: ClockSample::now().wall_ns,
        monotonic: clock.monotonic,
    };
    let inbound = super::s13::inbound(session.session_id, session.peer, origin, frame.clone());
    let outcome = admit_protected_opening(
        mesh,
        &inbound,
        call_id,
        &intent.caller.entity_id().clone(),
        &tag(),
        reg,
        replay,
        clock,
        RpcCallShape::ServerStreaming,
        Some(binding),
        session.clone(),
        Some(1),
    )?;
    match outcome {
        ProtectedOpeningOutcome::Admitted {
            admitted,
            lease,
            credential_ends_ns: _,
        } => Ok((frame, admitted, lease)),
    }
}

/// Feed one admitted opening through the SS fold's lease-carrying seam —
/// the §3 transfer at the fold's effect boundary.
pub(crate) fn open_ss_call(
    fold: &mut RpcServerStreamingFold,
    frame: &Bytes,
    admitted: Admitted,
    lease: ProtectedCallLease,
    session: &SessionIdentity,
    origin: u64,
    lifetime: &StreamCallLifetime<'_>,
) -> Arc<ProtectedStreamCall> {
    fold.apply_inbound_admitted(
        &super::s13::inbound(session.session_id, session.peer, origin, frame.clone()),
        admitted,
        lifetime,
        Some(lease),
    )
    .expect("the installed lease transfers at the fold")
}

/// A streaming handler that hands its `RpcResponseSink` to the witness
/// and parks — every send is the witness's explicit decision ("the
/// sibling sends its NEXT item after publication").
pub(crate) struct SinkHolder {
    pub(crate) sink: Arc<Mutex<Option<RpcResponseSink>>>,
    pub(crate) started: Arc<AtomicUsize>,
    pub(crate) dropped: Arc<AtomicBool>,
}

impl SinkHolder {
    pub(crate) fn new() -> Self {
        Self {
            sink: Arc::new(Mutex::new(None)),
            started: Arc::new(AtomicUsize::new(0)),
            dropped: Arc::new(AtomicBool::new(false)),
        }
    }
}

#[async_trait::async_trait]
impl RpcStreamingHandler for SinkHolder {
    async fn call(&self, _ctx: RpcContext, sink: RpcResponseSink) -> Result<(), RpcHandlerError> {
        struct DropFlag(Arc<AtomicBool>);
        impl Drop for DropFlag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let _flag = DropFlag(Arc::clone(&self.dropped));
        *self.sink.lock() = Some(sink);
        self.started.fetch_add(1, Ordering::SeqCst);
        std::future::pending::<()>().await;
        Ok(())
    }
}

/// A streaming handler that queues `chunks` through the void `send` and
/// returns — the response pump (parked on a zero-credit window) is the
/// blocked stream the revocation witnesses retire.
pub(crate) struct QueueChunksAndReturn {
    pub(crate) chunks: Vec<&'static [u8]>,
    pub(crate) returned: Arc<AtomicUsize>,
    pub(crate) dropped: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl RpcStreamingHandler for QueueChunksAndReturn {
    async fn call(&self, _ctx: RpcContext, sink: RpcResponseSink) -> Result<(), RpcHandlerError> {
        struct DropFlag(Arc<AtomicBool>);
        impl Drop for DropFlag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let _flag = DropFlag(Arc::clone(&self.dropped));
        for chunk in &self.chunks {
            sink.send(Bytes::from_static(chunk));
        }
        self.returned.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// The one-call registry view most assertions read.
pub(crate) fn registry_of(node: &Arc<MeshNode>) -> Arc<ProtectedCallRegistry> {
    net::adapter::net::cortex::rpc::protected_call_registry_for(node.node_id())
}

/// The registry key shape the witnesses name.
pub(crate) fn call_key(
    caller: &net::adapter::net::identity::EntityId,
    call_id: u64,
) -> ProtectedCallKey {
    ProtectedCallKey {
        caller: caller.clone(),
        call_id,
    }
}

/// Install THE registry for this node with the tiny §2.7 byte budgets
/// (before the store install binds it).
pub(crate) fn set_tiny_byte_registry(node: &Arc<MeshNode>) {
    net::adapter::net::cortex::rpc::set_protected_call_registry_for_node(
        node.node_id(),
        net::adapter::net::cortex::rpc::tiny_byte_call_limits(),
        net::adapter::net::cortex::rpc::tiny_byte_byte_limits(),
    )
    .expect("the tiny limit set validates");
}

/// Issue a floor bundle without applying it (the store-replacement
/// witness pre-raises the replacement store so it dominates).
pub(crate) fn floor_bundle(
    org: &OrgKeypair,
    member: net::adapter::net::identity::EntityId,
    generation: u32,
) -> OrgRevocationBundle {
    let mut floors = BTreeMap::new();
    floors.insert(member, generation);
    OrgRevocationBundle::try_issue(org, &floors).expect("issue floor bundle")
}

/// The typed terminal a revoked / authority-lost stream emits (the wire
/// assertion helper): `AdmissionDenied` + coarse `Denied` byte `[0]`.
pub(crate) fn is_denied_byte(
    status_body: &(net::adapter::net::cortex::rpc::RpcStatus, &Bytes),
) -> bool {
    matches!(
        status_body.0,
        net::adapter::net::cortex::rpc::RpcStatus::AdmissionDenied
    ) && status_body.1.as_ref() == [0u8].as_slice()
}

/// Terminal reason equality helper (bounded wait — the supervisor lands
/// the terminal asynchronously after the signal fires — then the exact
/// assertion).
pub(crate) async fn assert_terminal(
    call: &ProtectedStreamCall,
    want: StreamTerminalReason,
    what: &str,
) {
    assert!(
        super::s13::wait_for(Duration::from_secs(30), || call.terminal().is_some()).await,
        "{what}: the terminal never landed",
    );
    let got = call.terminal();
    assert!(
        matches!(&got, Some(got) if *got == want),
        "{what}: expected terminal {want:?}, got {got:?}",
    );
}
