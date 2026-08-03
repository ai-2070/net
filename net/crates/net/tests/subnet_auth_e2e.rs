//! §6 of `docs/internal/plans/SUBNET_AUTH_PLAN.md` — live end-to-end
//! evidence harness for the BMW two-vehicle authority model.
//!
//! The load-bearing test is the FOUR-PLANE conjunction on one real
//! `perception.roi` call over real transport:
//!
//! ```text
//! organization proof        (BMW membership + exact dispatcher grant)
//! + gateway EXPORT          (Vehicle B's own credential at WORLD_MODEL)
//! + provider admission      (provider-local policy)
//! + exported dispatch       (serve_rpc_subnet_exported → MeshNode::call)
//! ```
//!
//! The production composition point is the D7 repair: a
//! subnet-exported registration binds one service to one exact
//! declared crossing ([`SubnetExportBinding`]), and dispatch
//! revalidates — per call, before organization admission — that the
//! provider CURRENTLY holds exact `EXPORT` at that declared boundary
//! under the current epochs. Removing any one plane denies while the
//! others stay valid.
//!
//! No test here composes the gates by hand: no direct
//! `authorize_transition` calls, no hand-built subnet verdicts inside
//! `provider_policy`, no invented registration APIs. The caller
//! proves organization admission and NEVER acquires a Vehicle B
//! subnet context.
//!
//! Evidence map (§6): the four-plane test covers rows 1–5 and 11 for
//! the fleet-call surface; the focused inverses below pin the D7
//! seam's failure modes (registration shape, live darkness on every
//! authority movement, epoch pinning, recovery). Rows 6–20 continue
//! in the A–H scenarios (separate work, gated on this repair).

#![cfg(all(feature = "net", feature = "cortex"))]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::behavior::org::{OrgId, OrgKeypair, OrgMembershipCert};
use net::adapter::net::behavior::org_admission::OrgAdmission;
use net::adapter::net::behavior::org_authority::NodeAuthority;
use net::adapter::net::behavior::org_grant::{
    CapabilityAuthorityId, DispatcherScope, OrgDispatcherGrant,
};
use net::adapter::net::cortex::{
    RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus,
};
use net::adapter::net::identity::EntityId;
use net::adapter::net::mesh_rpc::{CallOptions, OrgProofIntent, RpcError, ServeError};
use net::adapter::net::subnet::{
    build_gateway_context_set, compile_gateway_context, SubnetAuthError, SubnetAuthorityConfig,
    SubnetBoundarySet, SubnetCredentialSet, SubnetExportBinding, SubnetFloorRegistry, SubnetGrant,
    SubnetRef, SubnetRevocationFloor, SubnetRights, TopologySubnetId,
};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};

const PSK: [u8; 32] = [0x42u8; 32];
const TEST_BUFFER_SIZE: usize = 256 * 1024;
const DAY: u64 = 24 * 60 * 60;
const ORG_ADMISSION_HEADER: &str = "net-org-admission";

// Deterministic identities (§9): one seed per fixture identity.
const VEHICLE_A_SEED: [u8; 32] = [0xA1; 32];
const VEHICLE_B_SEED: [u8; 32] = [0xA2; 32];
/// Vehicle B's subnet authority root — deliberately DISTINCT from
/// both organization roots (§3): subnet authority is vertical and
/// installation-local, org authority is horizontal.
const VB_SUBNET_ROOT_SEED: [u8; 32] = [0xC0; 32];
/// Vehicle A's own subnet authority root, for the equal-path-bits
/// independence witnesses.
const VA_SUBNET_ROOT_SEED: [u8; 32] = [0xC5; 32];
/// BMW org root.
const BMW_ORG_SEED: [u8; 32] = [0xB0; 32];

// Vehicle B's internal hierarchy (§3), compact path levels.
const VEHICLE: &[u8] = &[3];
const WORLD_MODEL: &[u8] = &[3, 7, 1];
const CAMERA: &[u8] = &[3, 7, 2];

const SERVICE: &str = "perception.roi";

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_secs()
}

fn vb_subnet_root() -> EntityKeypair {
    EntityKeypair::from_bytes(VB_SUBNET_ROOT_SEED)
}

fn va_subnet_root() -> EntityKeypair {
    EntityKeypair::from_bytes(VA_SUBNET_ROOT_SEED)
}

fn bmw() -> OrgKeypair {
    OrgKeypair::from_bytes(BMW_ORG_SEED)
}

fn vb_ref(levels: &[u8]) -> SubnetRef {
    SubnetRef {
        authority: vb_subnet_root().entity_id().clone(),
        path: TopologySubnetId::new(levels),
    }
}

fn base_config() -> MeshNodeConfig {
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

/// Vehicle B: anchors its OWN subnet authority and attaches at
/// `VEHICLE`. (Vehicle A deliberately anchors nothing of Vehicle
/// B's — it must never acquire a Vehicle B subnet context.)
async fn build_vehicle_b() -> Arc<MeshNode> {
    let mut cfg = base_config().with_subnet_authority(SubnetAuthorityConfig {
        authority: vb_subnet_root().entity_id().clone(),
        roots: vec![vb_subnet_root().entity_id().clone()],
        maximum_grant_lifetime_secs: 7 * DAY,
    });
    cfg.subnet_attachment = Some(TopologySubnetId::new(VEHICLE));
    Arc::new(
        MeshNode::new(EntityKeypair::from_bytes(VEHICLE_B_SEED), cfg)
            .await
            .expect("MeshNode::new vehicle B"),
    )
}

async fn build_vehicle_a() -> Arc<MeshNode> {
    Arc::new(
        MeshNode::new(EntityKeypair::from_bytes(VEHICLE_A_SEED), base_config())
            .await
            .expect("MeshNode::new vehicle A"),
    )
}

/// Handshake, start, and mutually entity-pin the pair via signed
/// capability announcements (the caller-side proof binding and the
/// provider-side `resolve_direct_caller` both need the pins).
async fn bring_up(caller: &Arc<MeshNode>, server: &Arc<MeshNode>) {
    let a_id = caller.node_id();
    let b_id = server.node_id();
    let b_pub = *server.public_key();
    let b_addr = server.local_addr();
    let b_clone = server.clone();
    let accept = tokio::spawn(async move { b_clone.accept(a_id).await });
    caller
        .connect(b_addr, &b_pub, b_id)
        .await
        .expect("connect failed");
    accept
        .await
        .expect("accept task panicked")
        .expect("accept failed");
    caller.start();
    server.start();

    server
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("server announce");
    caller
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("caller announce");
    assert!(
        wait_until(Duration::from_secs(5), || {
            caller.peer_entity_id(b_id).is_some() && server.peer_entity_id(a_id).is_some()
        })
        .await,
        "entity pins established in both directions",
    );
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

/// Install Vehicle B's BMW node authority (the org plane's provider
/// anchor). Returns the scratch dir for cleanup.
fn install_bmw_authority(server: &Arc<MeshNode>, tag: &str) -> std::path::PathBuf {
    let node_entity = server.entity_id().clone();
    let node_cert =
        OrgMembershipCert::try_issue(&bmw(), node_entity.clone(), 1, 3600).expect("node cert");
    let dir = std::env::temp_dir().join(format!("net-subnet-e2e-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let authority =
        NodeAuthority::adopt(&dir, node_cert, &node_entity, 0, None).expect("adopt authority");
    server
        .install_node_authority(Arc::new(authority))
        .expect("install authority");
    dir
}

/// A Vehicle B subnet grant signed by Vehicle B's OWN subnet root,
/// with explicit epoch / generation / lifetime for the darkness
/// witnesses.
fn vb_grant_at(
    subject: &EntityKeypair,
    scope: &[u8],
    rights: SubnetRights,
    topology_epoch: u32,
    generation: u32,
    lifetime_secs: u64,
) -> SubnetCredentialSet {
    SubnetCredentialSet::Direct(
        SubnetGrant::try_issue(
            &vb_subnet_root(),
            vb_subnet_root().entity_id().clone(),
            TopologySubnetId::new(scope),
            topology_epoch,
            subject.entity_id().clone(),
            rights,
            generation,
            unix_now() - 60,
            lifetime_secs,
        )
        .expect("issue subnet grant"),
    )
}

fn vb_grant(subject: &EntityKeypair, scope: &[u8], rights: SubnetRights) -> SubnetCredentialSet {
    vb_grant_at(subject, scope, rights, 0, 1, DAY)
}

/// Vehicle B's canonical gateway credential set, exactly as the §3
/// provisioning table writes it: ATTACH + ROUTE at VEHICLE, and a
/// delegated EXPORT at the exact WORLD_MODEL boundary. The second
/// credential compiles because ROUTE/EXPORT-only credentials are
/// delegated forwarding authority — they do not claim the gateway is
/// ATTACHED at that scope (the D7 compiler repair).
fn gateway_credentials_with_export(vb_kp: &EntityKeypair) -> Vec<SubnetCredentialSet> {
    vec![
        vb_grant(
            vb_kp,
            VEHICLE,
            SubnetRights::ATTACH.union(SubnetRights::ROUTE),
        ),
        vb_grant(vb_kp, WORLD_MODEL, SubnetRights::EXPORT),
    ]
}

/// The same set minus ONLY the WORLD_MODEL `EXPORT` credential.
fn gateway_credentials_without_export(vb_kp: &EntityKeypair) -> Vec<SubnetCredentialSet> {
    vec![vb_grant(
        vb_kp,
        VEHICLE,
        SubnetRights::ATTACH.union(SubnetRights::ROUTE),
    )]
}

/// Declare Vehicle B's protected crossings: exactly WORLD_MODEL.
fn declare_world_model_boundary(vehicle_b: &Arc<MeshNode>, topology_epoch: u32) {
    vehicle_b.declare_subnet_boundaries(SubnetBoundarySet::new(
        vb_subnet_root().entity_id().clone(),
        topology_epoch,
        [TopologySubnetId::new(WORLD_MODEL)],
    ));
}

fn world_model_binding(topology_epoch: u32) -> SubnetExportBinding {
    SubnetExportBinding::new(vb_ref(WORLD_MODEL), topology_epoch)
}

/// A fresh owner-delegated intent: Vehicle A acts for BMW, which
/// also owns the Vehicle B provider. Freshly minted per call (§9).
fn fleet_intent(provider: EntityId) -> OrgProofIntent {
    let caller_kp = EntityKeypair::from_bytes(VEHICLE_A_SEED);
    let caller_entity = caller_kp.entity_id().clone();
    let cap = CapabilityAuthorityId::for_tag(&format!("nrpc:{SERVICE}"));
    let membership =
        OrgMembershipCert::try_issue(&bmw(), caller_entity.clone(), 1, 3600).expect("membership");
    let dispatcher =
        OrgDispatcherGrant::try_issue(&bmw(), caller_entity, DispatcherScope::Exact(cap), 3600)
            .expect("dispatcher");
    OrgProofIntent {
        caller: Arc::new(caller_kp),
        membership,
        dispatcher,
        capability_grant: None,
        acting_org: bmw().org_id(),
        provider_owner_org: bmw().org_id(),
        provider,
        capability: cap,
        proof_ttl_secs: 30,
    }
}

fn call_opts(intent: Option<OrgProofIntent>) -> CallOptions {
    CallOptions {
        org_proof_intent: intent,
        deadline: Some(Instant::now() + Duration::from_secs(5)),
        ..Default::default()
    }
}

/// Records the admission attribution the protected handler observes,
/// plus a dynamic provider-policy switch for the provider inverse.
struct RoiHandler {
    calls: Arc<AtomicUsize>,
    attribution_ok: Arc<AtomicBool>,
    proof_stripped: Arc<AtomicBool>,
    expected_caller: EntityId,
    expected_org: OrgId,
    expected_provider: EntityId,
}

#[async_trait::async_trait]
impl RpcHandler for RoiHandler {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(admitted) = ctx.org_admission.as_ref() {
            if admitted.caller == self.expected_caller
                && admitted.acting_org == self.expected_org
                && admitted.provider_org == self.expected_org
                && admitted.provider == self.expected_provider
                && admitted.capability == CapabilityAuthorityId::for_tag("nrpc:perception.roi")
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
            body: Bytes::from_static(b"roi-window"),
        })
    }
}

/// §9: darkness is asserted over a bounded settlement window, from a
/// phase-local baseline (the handler legitimately ran in earlier
/// phases of the same test).
async fn assert_handler_stays_at(calls: &Arc<AtomicUsize>, baseline: usize, what: &str) {
    const SETTLE: Duration = Duration::from_millis(200);
    const STEP: Duration = Duration::from_millis(10);
    let deadline = Instant::now() + SETTLE;
    loop {
        let observed = calls.load(Ordering::SeqCst);
        assert_eq!(
            observed, baseline,
            "{what}: the handler RAN ({observed} vs baseline {baseline}) despite the denial",
        );
        if Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(STEP).await;
    }
}

/// An explicit denial: an `AdmissionDenied` ServerError — NEVER a
/// timeout, and never success.
fn assert_explicit_denial(
    result: Result<net::adapter::net::mesh_rpc::RpcReply, RpcError>,
    what: &str,
) {
    match result {
        Err(RpcError::ServerError { status, .. }) => {
            assert_eq!(
                status, 0x0009,
                "{what}: denial must be AdmissionDenied (0x0009), got {status:#06x}",
            );
        }
        Err(other) => panic!(
            "{what}: expected an explicit AdmissionDenied ServerError, got {other:?} \
             (a Timeout here would be a denial masquerading as a timeout)"
        ),
        Ok(_) => panic!("{what}: the call was ADMITTED — the removed plane did not gate it"),
    }
}

/// One fully-provisioned Vehicle A ↔ Vehicle B pair with the service
/// registered subnet-exported. Returns everything the scenario tests
/// mutate.
struct FleetFixture {
    vehicle_a: Arc<MeshNode>,
    vehicle_b: Arc<MeshNode>,
    vb_kp: EntityKeypair,
    provider: EntityId,
    calls: Arc<AtomicUsize>,
    attribution_ok: Arc<AtomicBool>,
    proof_stripped: Arc<AtomicBool>,
    policy_allows: Arc<AtomicBool>,
    /// `Option` so a scenario can retire the registration (drop the
    /// handle) while continuing to drive the fixture.
    serve: Option<net::adapter::net::mesh_rpc::ServeHandle>,
    dir: std::path::PathBuf,
}

async fn fleet_fixture(tag: &str) -> FleetFixture {
    let vehicle_b = build_vehicle_b().await;
    let vehicle_a = build_vehicle_a().await;
    let vb_kp = EntityKeypair::from_bytes(VEHICLE_B_SEED);
    bring_up(&vehicle_a, &vehicle_b).await;

    let dir = install_bmw_authority(&vehicle_b, tag);
    let provider = vehicle_b.entity_id().clone();

    declare_world_model_boundary(&vehicle_b, 0);
    vehicle_b
        .install_subnet_gateway_credentials(&gateway_credentials_with_export(&vb_kp))
        .expect("install gateway credentials with EXPORT");

    let policy_allows = Arc::new(AtomicBool::new(true));
    let policy_probe = policy_allows.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let attribution_ok = Arc::new(AtomicBool::new(false));
    let proof_stripped = Arc::new(AtomicBool::new(false));
    let serve = vehicle_b
        .serve_rpc_subnet_exported(
            SERVICE,
            Arc::new(RoiHandler {
                calls: calls.clone(),
                attribution_ok: attribution_ok.clone(),
                proof_stripped: proof_stripped.clone(),
                expected_caller: vehicle_a.entity_id().clone(),
                expected_org: bmw().org_id(),
                expected_provider: provider.clone(),
            }),
            OrgAdmission::OwnerDelegated,
            world_model_binding(0),
            Arc::new(move |_| policy_probe.load(Ordering::SeqCst)),
        )
        .expect("serve perception.roi subnet-exported");

    FleetFixture {
        vehicle_a,
        vehicle_b,
        vb_kp,
        provider,
        calls,
        attribution_ok,
        proof_stripped,
        policy_allows,
        serve: Some(serve),
        dir,
    }
}

impl FleetFixture {
    async fn call(
        &self,
        with_proof: bool,
    ) -> Result<net::adapter::net::mesh_rpc::RpcReply, RpcError> {
        let intent = with_proof.then(|| fleet_intent(self.provider.clone()));
        self.vehicle_a
            .call(
                self.vehicle_b.node_id(),
                SERVICE,
                Bytes::from_static(b"roi?"),
                call_opts(intent),
            )
            .await
    }

    fn assert_no_va_subnet_context(&self) {
        assert!(
            self.vehicle_b
                .subnet_context_for(self.vehicle_a.node_id())
                .is_none(),
            "Vehicle A must never acquire a Vehicle B subnet context",
        );
    }
}

// ===========================================================================
// §5 — the four-plane composition point. Evidence 1–5, 11.
// ===========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fleet_exported_provider_requires_gateway_export_and_org_authority() {
    let f = fleet_fixture("four-plane").await;

    // ---- Phase 1: positive baseline -------------------------------
    let reply = f
        .call(true)
        .await
        .expect("the fully-credentialed fleet call is admitted");
    assert_eq!(reply.body.as_ref(), b"roi-window", "exact reply body");
    assert_eq!(
        f.calls.load(Ordering::SeqCst),
        1,
        "handler ran exactly once"
    );
    assert!(
        f.attribution_ok.load(Ordering::SeqCst),
        "attribution names Vehicle A, BMW (acting and provider org), Vehicle B, \
         and nrpc:perception.roi exactly",
    );
    assert!(
        f.proof_stripped.load(Ordering::SeqCst),
        "raw proof material was stripped before the handler view",
    );
    f.assert_no_va_subnet_context();

    // ---- Phase 2: gateway-authority inverse -----------------------
    // The ONLY removed thing is Vehicle B's exact WORLD_MODEL EXPORT
    // credential; org proof, provider policy, and the registration
    // are untouched.
    f.vehicle_b
        .install_subnet_gateway_credentials(&gateway_credentials_without_export(&f.vb_kp))
        .expect("reinstall gateway credentials WITHOUT export");
    let baseline = f.calls.load(Ordering::SeqCst);
    assert_explicit_denial(f.call(true).await, "gateway-authority inverse");
    assert_handler_stays_at(&f.calls, baseline, "gateway-authority inverse").await;

    // ---- Phase 3: organization inverse ----------------------------
    f.vehicle_b
        .install_subnet_gateway_credentials(&gateway_credentials_with_export(&f.vb_kp))
        .expect("restore gateway credentials with EXPORT");
    let baseline = f.calls.load(Ordering::SeqCst);
    assert_explicit_denial(f.call(false).await, "organization inverse");
    assert_handler_stays_at(&f.calls, baseline, "organization inverse").await;

    // ---- Phase 4: provider inverse --------------------------------
    f.policy_allows.store(false, Ordering::SeqCst);
    let baseline = f.calls.load(Ordering::SeqCst);
    assert_explicit_denial(f.call(true).await, "provider inverse");
    assert_handler_stays_at(&f.calls, baseline, "provider inverse").await;

    // The conjunction restored end-to-end: all four planes back →
    // admitted again (proves the inverses denied for their own
    // reason, not lingering damage).
    f.policy_allows.store(true, Ordering::SeqCst);
    let reply = f
        .call(true)
        .await
        .expect("restored conjunction admits again");
    assert_eq!(reply.body.as_ref(), b"roi-window");
    f.assert_no_va_subnet_context();

    let _ = std::fs::remove_dir_all(&f.dir);
}

// ===========================================================================
// §8 — focused registration-shape inverses.
// ===========================================================================

/// Registration fails closed for every impossible shape: no boundary
/// set, a binding that is not an exact declared boundary, no exact
/// EXPORT, ancestor EXPORT offered for a descendant binding, and a
/// wrong-authority binding.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn exported_registration_requires_exact_boundary_and_exact_export() {
    let vehicle_b = build_vehicle_b().await;
    let vehicle_a = build_vehicle_a().await;
    let vb_kp = EntityKeypair::from_bytes(VEHICLE_B_SEED);
    bring_up(&vehicle_a, &vehicle_b).await;
    let dir = install_bmw_authority(&vehicle_b, "reg-shape");

    let dark = Arc::new(AtomicUsize::new(0));
    let serve = |vb: &Arc<MeshNode>, binding: SubnetExportBinding| {
        vb.serve_rpc_subnet_exported(
            SERVICE,
            Arc::new(RoiHandler {
                calls: dark.clone(),
                attribution_ok: Arc::new(AtomicBool::new(false)),
                proof_stripped: Arc::new(AtomicBool::new(false)),
                expected_caller: vehicle_a.entity_id().clone(),
                expected_org: bmw().org_id(),
                expected_provider: vb.entity_id().clone(),
            }),
            OrgAdmission::OwnerDelegated,
            binding,
            Arc::new(|_| true),
        )
    };

    // No boundary set declared at all.
    vehicle_b
        .install_subnet_gateway_credentials(&gateway_credentials_with_export(&vb_kp))
        .expect("install credentials");
    assert!(
        matches!(
            serve(&vehicle_b, world_model_binding(0)),
            Err(ServeError::SubnetExportUnauthorized(_))
        ),
        "registration must fail with no declared boundary set",
    );

    // The binding path is not an exact declared boundary (CAMERA is
    // declared, WORLD_MODEL is not).
    vehicle_b.declare_subnet_boundaries(SubnetBoundarySet::new(
        vb_subnet_root().entity_id().clone(),
        0,
        [TopologySubnetId::new(CAMERA)],
    ));
    assert!(
        matches!(
            serve(&vehicle_b, world_model_binding(0)),
            Err(ServeError::SubnetExportUnauthorized(_))
        ),
        "a binding must name an exactly-declared boundary",
    );

    // Boundary right, but no exact EXPORT credential.
    declare_world_model_boundary(&vehicle_b, 0);
    vehicle_b
        .install_subnet_gateway_credentials(&gateway_credentials_without_export(&vb_kp))
        .expect("install without export");
    assert!(
        matches!(
            serve(&vehicle_b, world_model_binding(0)),
            Err(ServeError::SubnetExportUnauthorized(_))
        ),
        "registration must fail without exact EXPORT authority",
    );

    // Ancestor EXPORT (at VEHICLE) does not satisfy the WORLD_MODEL
    // binding — exact means exact, no ancestor inheritance.
    vehicle_b
        .install_subnet_gateway_credentials(&[vb_grant(
            &vb_kp,
            VEHICLE,
            SubnetRights::ATTACH
                .union(SubnetRights::ROUTE)
                .union(SubnetRights::EXPORT),
        )])
        .expect("install ancestor-export set");
    assert!(
        matches!(
            serve(&vehicle_b, world_model_binding(0)),
            Err(ServeError::SubnetExportUnauthorized(_))
        ),
        "EXPORT at VEHICLE must not satisfy a service bound to WORLD_MODEL",
    );

    // Wrong authority: a binding under Vehicle A's subnet root.
    vehicle_b
        .install_subnet_gateway_credentials(&gateway_credentials_with_export(&vb_kp))
        .expect("restore canonical credentials");
    let foreign = SubnetExportBinding::new(
        SubnetRef {
            authority: va_subnet_root().entity_id().clone(),
            path: TopologySubnetId::new(WORLD_MODEL),
        },
        0,
    );
    assert!(
        matches!(
            serve(&vehicle_b, foreign),
            Err(ServeError::SubnetExportUnauthorized(_))
        ),
        "equal path bits under a different authority must not satisfy",
    );

    // Control: the canonical shape registers.
    let handle = serve(&vehicle_b, world_model_binding(0)).expect("canonical shape registers");
    drop(handle);
    assert_eq!(
        dark.load(Ordering::SeqCst),
        0,
        "no handler ran during shape checks"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ===========================================================================
// §8 — live darkness on every authority movement, and recovery.
// ===========================================================================

/// A LIVE registration darkens when any term of its export authority
/// moves — wholesale credential replacement, wholesale boundary
/// replacement, a signed revocation floor, credential expiry — and
/// recovers when exact current authority returns under the same
/// topology epoch.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_exported_service_darkens_on_authority_movement_and_recovers() {
    let f = fleet_fixture("darkness").await;

    // Baseline: admitted.
    f.call(true).await.expect("baseline admits");

    // (a) Wholesale credential replacement without EXPORT → dark.
    f.vehicle_b
        .install_subnet_gateway_credentials(&gateway_credentials_without_export(&f.vb_kp))
        .expect("replace without export");
    let baseline = f.calls.load(Ordering::SeqCst);
    assert_explicit_denial(f.call(true).await, "credential replacement");
    assert_handler_stays_at(&f.calls, baseline, "credential replacement").await;

    // Restoring exact current EXPORT under the same topology epoch
    // lets the EXISTING registration recover — no re-registration.
    f.vehicle_b
        .install_subnet_gateway_credentials(&gateway_credentials_with_export(&f.vb_kp))
        .expect("restore export");
    f.call(true)
        .await
        .expect("recovers after credential restore");

    // (b) Wholesale boundary replacement dropping WORLD_MODEL → dark.
    f.vehicle_b
        .declare_subnet_boundaries(SubnetBoundarySet::new(
            vb_subnet_root().entity_id().clone(),
            0,
            [TopologySubnetId::new(CAMERA)],
        ));
    let baseline = f.calls.load(Ordering::SeqCst);
    assert_explicit_denial(f.call(true).await, "boundary replacement");
    assert_handler_stays_at(&f.calls, baseline, "boundary replacement").await;
    declare_world_model_boundary(&f.vehicle_b, 0);
    f.call(true).await.expect("recovers after boundary restore");

    // (c) A signed revocation floor above the credential generation →
    // auth epoch moves → dark. Fresh above-floor credentials recover.
    let floor = SubnetRevocationFloor::try_issue(
        &vb_subnet_root(),
        vb_ref(VEHICLE),
        0,
        5, // above the generation-1 gateway credentials
        1,
        unix_now(),
    )
    .expect("issue floor");
    assert!(f.vehicle_b.apply_subnet_floor(&floor).expect("apply floor"));
    let baseline = f.calls.load(Ordering::SeqCst);
    assert_explicit_denial(f.call(true).await, "revocation floor");
    assert_handler_stays_at(&f.calls, baseline, "revocation floor").await;

    let fresh = vec![
        vb_grant_at(
            &f.vb_kp,
            VEHICLE,
            SubnetRights::ATTACH.union(SubnetRights::ROUTE),
            0,
            6,
            DAY,
        ),
        vb_grant_at(&f.vb_kp, WORLD_MODEL, SubnetRights::EXPORT, 0, 6, DAY),
    ];
    f.vehicle_b
        .install_subnet_gateway_credentials(&fresh)
        .expect("install above-floor credentials");
    f.call(true)
        .await
        .expect("recovers with above-floor credentials");
    f.assert_no_va_subnet_context();

    // (d) Credential expiry → dark, by state predicate with a bounded
    // deadline (the short-lived set expires ~2 s out).
    let short = vec![
        vb_grant_at(
            &f.vb_kp,
            VEHICLE,
            SubnetRights::ATTACH.union(SubnetRights::ROUTE),
            0,
            6,
            62, // not_before = now-60 → expires ~2 s from now
        ),
        vb_grant_at(&f.vb_kp, WORLD_MODEL, SubnetRights::EXPORT, 0, 6, 62),
    ];
    f.vehicle_b
        .install_subnet_gateway_credentials(&short)
        .expect("install short-lived credentials");
    f.call(true)
        .await
        .expect("short-lived set admits while live");
    // Bounded state-predicate wait: keep calling until the set's
    // expiry makes the live registration deny explicitly. Each probe
    // is a real call; the deadline bounds the whole wait.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    let mut expired_denied = false;
    while tokio::time::Instant::now() < deadline {
        match f.call(true).await {
            Err(RpcError::ServerError { status: 0x0009, .. }) => {
                expired_denied = true;
                break;
            }
            _ => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
    assert!(
        expired_denied,
        "an expired gateway credential set must darken the live registration",
    );

    let _ = std::fs::remove_dir_all(&f.dir);
}

/// Topology-epoch movement darkens the old registration PERMANENTLY —
/// fresh epoch-N+1 credentials and boundaries do not revive a binding
/// declared under epoch N; only explicit re-registration under the
/// new epoch recovers.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn topology_epoch_movement_darkens_until_explicit_reregistration() {
    let f = fleet_fixture("epoch").await;
    f.call(true).await.expect("baseline admits");

    // Reparenting: the epoch advances. The old binding must go dark.
    let next_epoch = f.vehicle_b.advance_subnet_topology_epoch();
    let baseline = f.calls.load(Ordering::SeqCst);
    assert_explicit_denial(f.call(true).await, "topology epoch advance");
    assert_handler_stays_at(&f.calls, baseline, "topology epoch advance").await;

    // Even with FRESH epoch-N+1 boundaries and credentials, the old
    // registration stays dark: its binding names epoch N, and path
    // bits must not silently transfer to a reinterpreted hierarchy.
    declare_world_model_boundary(&f.vehicle_b, next_epoch);
    let fresh = vec![
        vb_grant_at(
            &f.vb_kp,
            VEHICLE,
            SubnetRights::ATTACH.union(SubnetRights::ROUTE),
            next_epoch,
            1,
            DAY,
        ),
        vb_grant_at(
            &f.vb_kp,
            WORLD_MODEL,
            SubnetRights::EXPORT,
            next_epoch,
            1,
            DAY,
        ),
    ];
    f.vehicle_b
        .install_subnet_gateway_credentials(&fresh)
        .expect("install fresh-epoch credentials");
    let baseline = f.calls.load(Ordering::SeqCst);
    assert_explicit_denial(f.call(true).await, "old binding under new epoch");
    assert_handler_stays_at(&f.calls, baseline, "old binding under new epoch").await;

    // Explicit re-registration under the new epoch recovers.
    let mut f = f;
    drop(f.serve.take());
    let policy_probe = f.policy_allows.clone();
    let _serve2 = f
        .vehicle_b
        .serve_rpc_subnet_exported(
            SERVICE,
            Arc::new(RoiHandler {
                calls: f.calls.clone(),
                attribution_ok: f.attribution_ok.clone(),
                proof_stripped: f.proof_stripped.clone(),
                expected_caller: f.vehicle_a.entity_id().clone(),
                expected_org: bmw().org_id(),
                expected_provider: f.provider.clone(),
            }),
            OrgAdmission::OwnerDelegated,
            world_model_binding(next_epoch),
            Arc::new(move |_| policy_probe.load(Ordering::SeqCst)),
        )
        .expect("re-register under the new epoch");
    f.call(true)
        .await
        .expect("explicit re-registration recovers");
    f.assert_no_va_subnet_context();

    let _ = std::fs::remove_dir_all(&f.dir);
}

// ===========================================================================
// §8 — the compiler witnesses for the D7 delegated-authority repair.
// ===========================================================================

/// The canonical Vehicle B set compiles exactly as the plan writes it:
/// attached at VEHICLE, ATTACH/ROUTE at VEHICLE, EXPORT-only at
/// WORLD_MODEL. And the control: an ATTACH-bearing grant at an
/// unrelated descendant still fails ScopeNotAncestor — delegation
/// loosened forwarding scopes, never where the node may claim to BE.
#[test]
fn gateway_compiler_accepts_delegated_descendant_export_but_not_attach() {
    let vb_kp = EntityKeypair::from_bytes(VEHICLE_B_SEED);
    let config = SubnetAuthorityConfig {
        authority: vb_subnet_root().entity_id().clone(),
        roots: vec![vb_subnet_root().entity_id().clone()],
        maximum_grant_lifetime_secs: 7 * DAY,
    };
    let floors = SubnetFloorRegistry::new();
    let attachment = TopologySubnetId::new(VEHICLE);
    let compile = |set: &SubnetCredentialSet| {
        compile_gateway_context(
            set,
            vb_kp.entity_id(),
            attachment,
            &config,
            0,
            &floors,
            unix_now(),
            30,
        )
    };

    // ATTACH/ROUTE at the attachment + delegated EXPORT at the exact
    // descendant boundary — both compile, and publish as one set.
    let attach_route = compile(&vb_grant(
        &vb_kp,
        VEHICLE,
        SubnetRights::ATTACH.union(SubnetRights::ROUTE),
    ))
    .expect("ATTACH/ROUTE at the attachment compiles");
    let export_only = compile(&vb_grant(&vb_kp, WORLD_MODEL, SubnetRights::EXPORT))
        .expect("delegated EXPORT-only at an exact descendant compiles");
    build_gateway_context_set(
        vb_subnet_root().entity_id(),
        vec![attach_route, export_only],
    )
    .expect("the canonical Vehicle B set publishes");

    // Control: ATTACH at an unrelated descendant is still a claim
    // about where the node BELONGS, and still fails containment.
    assert_eq!(
        compile(&vb_grant(&vb_kp, CAMERA, SubnetRights::ATTACH)).err(),
        Some(SubnetAuthError::ScopeNotAncestor),
        "an ATTACH-bearing credential must still contain the attachment",
    );
}

// ===========================================================================
// Coherent-publication witnesses (review HOLD on 4e74216e0): gateway
// credentials and boundaries publish as ONE aggregate, so a captured
// admission stamp can never present a torn "both current" view.
// ===========================================================================

/// Publishing EITHER member — credentials or boundaries — changes the
/// aggregate's snapshot identity and invalidates previously captured
/// export facts, even when the republished content is identical. The
/// stamp fingerprints the one aggregate pointer, so there is no pair
/// of loads for a replacement to land between.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn publication_of_either_member_invalidates_captured_export_facts() {
    use net::adapter::net::behavior::admission_clock::ClockSample;
    use net::adapter::net::org_admission_gate::verify_subnet_export;

    let f = fleet_fixture("coherent-stamp").await;
    let binding = world_model_binding(0);

    // Captured against the current aggregate: current.
    let facts =
        verify_subnet_export(&f.vehicle_b, &binding, &ClockSample::now()).expect("capture facts");
    assert!(
        facts.is_current(&f.vehicle_b),
        "freshly captured facts are current"
    );

    // Republishing the SAME credential content still replaces the
    // aggregate snapshot the stamp fingerprints: identity, not
    // content, is the invalidation trigger — so captured facts die.
    f.vehicle_b
        .install_subnet_gateway_credentials(&gateway_credentials_with_export(&f.vb_kp))
        .expect("republish identical credentials");
    assert!(
        !facts.is_current(&f.vehicle_b),
        "captured facts must be invalidated by a credential publication",
    );

    // Same for the boundaries member.
    let facts =
        verify_subnet_export(&f.vehicle_b, &binding, &ClockSample::now()).expect("recapture");
    assert!(facts.is_current(&f.vehicle_b));
    declare_world_model_boundary(&f.vehicle_b, 0);
    assert!(
        !facts.is_current(&f.vehicle_b),
        "captured facts must be invalidated by a boundary publication",
    );

    // And the calls still work end to end after both republications.
    f.call(true)
        .await
        .expect("still admitted after republication");
    let _ = std::fs::remove_dir_all(&f.dir);
}

/// SUPPLEMENTAL stress evidence: two uncontrolled writer storms over
/// the two members. This does NOT by itself distinguish rcu from a
/// naive load-modify-store (an uncontrolled schedule rarely holds a
/// stale capture across the other writer's publication) — the
/// deterministic proof is
/// `a_held_stale_capture_cannot_lose_the_concurrent_publication`
/// below, which forces exactly that schedule.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_publication_loses_neither_authority_surface() {
    let vehicle_b = build_vehicle_b().await;
    let vb_kp = EntityKeypair::from_bytes(VEHICLE_B_SEED);

    // Two writers storm the two members concurrently. The credential
    // writer alternates one-entry / two-entry sets and ends on the
    // two-entry canonical set; the boundary writer alternates a
    // camera-only set with the canonical world-model set and ends on
    // world-model. Each writer's FINAL publication must survive the
    // other's entire storm.
    const ROUNDS: usize = 200;
    let b1 = vehicle_b.clone();
    let k1 = vb_kp.clone();
    let creds = tokio::task::spawn_blocking(move || {
        for i in 0..ROUNDS {
            let set = if i % 2 == 0 {
                gateway_credentials_without_export(&k1)
            } else {
                gateway_credentials_with_export(&k1)
            };
            b1.install_subnet_gateway_credentials(&set)
                .expect("install");
        }
        // Final: the canonical two-entry set.
        b1.install_subnet_gateway_credentials(&gateway_credentials_with_export(&k1))
            .expect("final install");
    });
    let b2 = vehicle_b.clone();
    let bounds = tokio::task::spawn_blocking(move || {
        for i in 0..ROUNDS {
            let path = if i % 2 == 0 { CAMERA } else { WORLD_MODEL };
            b2.declare_subnet_boundaries(SubnetBoundarySet::new(
                vb_subnet_root().entity_id().clone(),
                0,
                [TopologySubnetId::new(path)],
            ));
        }
        // Final: the canonical world-model boundary.
        declare_world_model_boundary(&b2, 0);
    });
    creds.await.expect("credential writer");
    bounds.await.expect("boundary writer");

    let gateway = vehicle_b
        .subnet_gateway_contexts()
        .expect("gateway member survived the storm");
    assert_eq!(
        gateway.entries().len(),
        2,
        "the credential writer's FINAL two-entry set must survive the boundary storm",
    );
    let boundaries = vehicle_b
        .subnet_boundaries()
        .expect("boundary member survived the storm");
    assert_eq!(
        boundaries.boundaries(),
        &[TopologySubnetId::new(WORLD_MODEL)],
        "the boundary writer's FINAL world-model set must survive the credential storm",
    );
}

/// THE deterministic lost-update witness (D7 evidence closure), in
/// BOTH directions: each writer takes one turn as the held-stale
/// party, so a naive rewrite of EITHER writer — or of the one shared
/// compare-and-retry primitive both route through — REDs here.
///
/// Phase A: the boundary writer captures (G0, B0) and is HELD inside
/// its capture→compare-and-swap window; the gateway writer publishes
/// G1; the boundary writer resumes, loses the CAS, re-captures, and
/// lands (G1, B1). Phase B mirrors it: the GATEWAY writer is held on
/// a (G1, B1) capture while the boundary writer publishes B2; the
/// gateway writer retries and lands (G2, B2). In each phase the
/// pacing hook running a second time IS the observed retry; a naive
/// load-modify-store held writer stores its stale capture verbatim —
/// one hook invocation, the concurrent publication lost, RED.
/// Verified by per-writer and shared-primitive mutation control (see
/// the commit message); the storm test above remains supplemental.
#[cfg(feature = "fixtures")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_held_stale_capture_cannot_lose_the_concurrent_publication() {
    let vehicle_b = build_vehicle_b().await;
    let vb_kp = EntityKeypair::from_bytes(VEHICLE_B_SEED);

    // Initial aggregate (G0, B0): the one-entry gateway set and the
    // camera boundary.
    vehicle_b
        .install_subnet_gateway_credentials(&gateway_credentials_without_export(&vb_kp))
        .expect("install G0");
    vehicle_b.declare_subnet_boundaries(SubnetBoundarySet::new(
        vb_subnet_root().entity_id().clone(),
        0,
        [TopologySubnetId::new(CAMERA)],
    ));

    let captured = Arc::new(std::sync::Barrier::new(2));
    let release = Arc::new(std::sync::Barrier::new(2));
    let hook_calls = Arc::new(AtomicUsize::new(0));

    let b = vehicle_b.clone();
    let (cap, rel, calls) = (captured.clone(), release.clone(), hook_calls.clone());
    let schedule = tokio::task::spawn_blocking(move || {
        let writer_b = b.clone();
        let boundary_writer = std::thread::spawn(move || {
            // B1 = the canonical world-model boundary, through the
            // PRODUCTION rcu path with the pacing hook.
            writer_b.test_declare_subnet_boundaries_paced(
                SubnetBoundarySet::new(
                    vb_subnet_root().entity_id().clone(),
                    0,
                    [TopologySubnetId::new(WORLD_MODEL)],
                ),
                &|| {
                    // First capture: rendezvous, then hold until the
                    // gateway writer has published. A retry passes
                    // straight through — it is a FRESH capture.
                    if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                        cap.wait();
                        rel.wait();
                    }
                },
            );
        });

        // The boundary writer now holds a (G0, B0) capture. Publish
        // G1 — the canonical two-entry set — inside its window.
        captured.wait();
        b.install_subnet_gateway_credentials(&gateway_credentials_with_export(
            &EntityKeypair::from_bytes(VEHICLE_B_SEED),
        ))
        .expect("publish G1 mid-window");
        release.wait();
        boundary_writer.join().expect("boundary writer");
    });
    schedule.await.expect("schedule");

    let observed_hook_calls = hook_calls.load(Ordering::SeqCst);
    assert!(
        observed_hook_calls >= 2,
        "the boundary writer must LOSE its stale compare-and-swap and          re-capture (saw {observed_hook_calls} hook call(s)): a single          capture means its stale view was stored verbatim over the          gateway publication",
    );
    assert_eq!(
        vehicle_b
            .subnet_gateway_contexts()
            .expect("gateway member present")
            .entries()
            .len(),
        2,
        "G1 must survive the boundary writer's held stale capture",
    );
    assert_eq!(
        vehicle_b
            .subnet_boundaries()
            .expect("boundary member present")
            .boundaries(),
        &[TopologySubnetId::new(WORLD_MODEL)],
        "B1 must land beside the surviving G1",
    );

    // ---- Phase B: the GATEWAY writer is the held-stale party -------
    // From (G1, B1): hold the gateway writer's (G2) capture, publish
    // B2 mid-window, release. G2 is the one-entry set; B2 is the
    // camera boundary — fresh values so a lost update is visible.
    let captured_b = Arc::new(std::sync::Barrier::new(2));
    let release_b = Arc::new(std::sync::Barrier::new(2));
    let gw_hook_calls = Arc::new(AtomicUsize::new(0));

    let b = vehicle_b.clone();
    let (cap_b, rel_b, gw_calls) = (captured_b.clone(), release_b.clone(), gw_hook_calls.clone());
    let schedule_b = tokio::task::spawn_blocking(move || {
        let writer_b = b.clone();
        let gateway_writer = std::thread::spawn(move || {
            writer_b
                .test_install_subnet_gateway_credentials_paced(
                    &gateway_credentials_without_export(&EntityKeypair::from_bytes(VEHICLE_B_SEED)),
                    &|| {
                        if gw_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                            cap_b.wait();
                            rel_b.wait();
                        }
                    },
                )
                .expect("paced gateway publish");
        });

        // The gateway writer now holds a (G1, B1) capture. Publish B2
        // inside its window.
        captured_b.wait();
        b.declare_subnet_boundaries(SubnetBoundarySet::new(
            vb_subnet_root().entity_id().clone(),
            0,
            [TopologySubnetId::new(CAMERA)],
        ));
        release_b.wait();
        gateway_writer.join().expect("gateway writer");
    });
    schedule_b.await.expect("schedule B");

    let observed_gw_calls = gw_hook_calls.load(Ordering::SeqCst);
    assert!(
        observed_gw_calls >= 2,
        "the GATEWAY writer must LOSE its stale compare-and-swap and          re-capture (saw {observed_gw_calls} hook call(s)): a single          capture means its stale view was stored verbatim over the          boundary publication",
    );
    assert_eq!(
        vehicle_b
            .subnet_gateway_contexts()
            .expect("gateway member present")
            .entries()
            .len(),
        1,
        "G2 must land beside the surviving B2",
    );
    assert_eq!(
        vehicle_b
            .subnet_boundaries()
            .expect("boundary member present")
            .boundaries(),
        &[TopologySubnetId::new(CAMERA)],
        "B2 must survive the gateway writer's held stale capture",
    );
}

/// Provider-side authority movement is never charged to the caller's
/// failed-admission budget (D7). Vehicle B republishes IDENTICAL
/// canonical credentials in a tight loop — content stays valid, so
/// the only denial a call can hit is the §9.5 stability seam
/// observing the aggregate identity move mid-verification
/// (`AuthorityChanged`). The provider's budget here is deliberately
/// TINY (2 failures, 1/s refill): if those denials were charged, the
/// honest caller's bucket would exhaust within the storm and the
/// post-storm call would be throttled `Unavailable` even under
/// restored, stable authority.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn provider_authority_churn_never_charges_the_caller() {
    use net::adapter::net::behavior::org_admission_replay::AdmissionRateLimitConfig;

    let vehicle_b = {
        let mut cfg = base_config()
            .with_subnet_authority(SubnetAuthorityConfig {
                authority: vb_subnet_root().entity_id().clone(),
                roots: vec![vb_subnet_root().entity_id().clone()],
                maximum_grant_lifetime_secs: 7 * DAY,
            })
            .with_admission_rate_limit(AdmissionRateLimitConfig {
                max_failed_per_peer: 2,
                refill_per_sec: 1,
                max_tracked_peers: 64,
            });
        cfg.subnet_attachment = Some(TopologySubnetId::new(VEHICLE));
        Arc::new(
            MeshNode::new(EntityKeypair::from_bytes(VEHICLE_B_SEED), cfg)
                .await
                .expect("MeshNode::new vehicle B"),
        )
    };
    let vehicle_a = build_vehicle_a().await;
    let vb_kp = EntityKeypair::from_bytes(VEHICLE_B_SEED);
    bring_up(&vehicle_a, &vehicle_b).await;
    let dir = install_bmw_authority(&vehicle_b, "limiter-churn");
    let provider = vehicle_b.entity_id().clone();

    declare_world_model_boundary(&vehicle_b, 0);
    vehicle_b
        .install_subnet_gateway_credentials(&gateway_credentials_with_export(&vb_kp))
        .expect("install credentials");
    let calls = Arc::new(AtomicUsize::new(0));
    let _serve = vehicle_b
        .serve_rpc_subnet_exported(
            SERVICE,
            Arc::new(RoiHandler {
                calls: calls.clone(),
                attribution_ok: Arc::new(AtomicBool::new(false)),
                proof_stripped: Arc::new(AtomicBool::new(false)),
                expected_caller: vehicle_a.entity_id().clone(),
                expected_org: bmw().org_id(),
                expected_provider: provider.clone(),
            }),
            OrgAdmission::OwnerDelegated,
            world_model_binding(0),
            Arc::new(|_| true),
        )
        .expect("serve");

    // Storm: republish the SAME canonical set continuously while the
    // caller issues valid calls. Every deny is provider-side identity
    // movement; the caller's proofs are impeccable throughout.
    let stop = Arc::new(AtomicBool::new(false));
    let churn_stop = stop.clone();
    let churn_b = vehicle_b.clone();
    let churn_kp = vb_kp.clone();
    let churn = tokio::task::spawn_blocking(move || {
        while !churn_stop.load(Ordering::SeqCst) {
            churn_b
                .install_subnet_gateway_credentials(&gateway_credentials_with_export(&churn_kp))
                .expect("churn republish");
        }
    });

    let mut denials = 0usize;
    let mut admits = 0usize;
    for _ in 0..300 {
        if denials >= 8 {
            break;
        }
        match vehicle_a
            .call(
                vehicle_b.node_id(),
                SERVICE,
                Bytes::from_static(b"roi?"),
                call_opts(Some(fleet_intent(provider.clone()))),
            )
            .await
        {
            Ok(_) => admits += 1,
            Err(RpcError::ServerError { status: 0x0009, .. }) => denials += 1,
            Err(other) => panic!("churn denial must be explicit, got {other:?}"),
        }
    }
    stop.store(true, Ordering::SeqCst);
    churn.await.expect("churn writer");
    assert!(
        denials >= 8,
        "the storm produced only {denials} stability denials in 300 calls — \
         the witness needs the mid-verification window to be hit",
    );

    // The budget is 2; the caller just absorbed >= 8 provider-side
    // denials. Under restored, stable authority the very next call
    // must ADMIT — which it cannot if those denials were charged.
    let reply = vehicle_a
        .call(
            vehicle_b.node_id(),
            SERVICE,
            Bytes::from_static(b"roi?"),
            call_opts(Some(fleet_intent(provider.clone()))),
        )
        .await
        .expect(
            "a caller that only ever presented valid proofs must not be \
             throttled by the provider's own authority churn",
        );
    assert_eq!(reply.body.as_ref(), b"roi-window");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        admits + 1,
        "the handler ran exactly once per ADMITTED call — every churn \
         denial left it dark",
    );

    let _ = std::fs::remove_dir_all(&dir);
}
