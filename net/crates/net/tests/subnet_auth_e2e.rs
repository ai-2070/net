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
//! # §6 evidence map
//!
//! Every row is crossed over the real transport/dispatch path; no row
//! is claimed on the strength of an older semantic-only test.
//!
//! | # | Evidence | Test |
//! |---|---|---|
//! | 1 | fleet membership creates no internal ATTACH | `neither_plane_manufactures_the_other` |
//! | 2 | Vehicle A invokes `perception.roi` with dispatcher proof | `fleet_exported_provider_requires_gateway_export_and_org_authority` |
//! | 3 | the call exposes only the bounded provider | `partner_diagnostic_is_exactly_bounded`, `neither_plane_manufactures_the_other` |
//! | 4 | Vehicle A cannot establish a Vehicle B subnet session | `neither_plane_manufactures_the_other` |
//! | 5 | the gateway needs its OWN exact `EXPORT` at the boundary | `fleet_exported_provider_…`, `exported_registration_requires_exact_boundary_and_exact_export` |
//! | 6 | the camera cannot attach upward or sideways | `vehicle_internal_authority_is_hierarchical` |
//! | 7 | a parent grant reaches its descendants | `vehicle_internal_authority_is_hierarchical` |
//! | 8 | equal path bits under two authorities are unrelated | `equal_path_bits_under_two_authorities_are_unrelated` |
//! | 9 | the Partner grant reaches exactly one exported provider | `partner_diagnostic_is_exactly_bounded` |
//! | 10 | a protected channel still needs its token | `channel_authority_remains_independent_of_subnet_authority` |
//! | 11 | a subnet context invokes nothing without org authority | `neither_plane_manufactures_the_other` |
//! | 12 | org revocation leaves subnet state independent | `org_and_subnet_revocation_are_independent_live` |
//! | 13 | a perception floor spares chassis and the vehicle root | `org_and_subnet_revocation_are_independent_live` |
//! | 14 | replayed credentials/presentations prove nothing | `replayed_credentials_and_presentations_prove_nothing` |
//! | 15 | each axis is re-proven and recovers only itself | `replayed_credentials_…`, `each_axis_recovers_only_itself` |
//! | 16 | a topology-epoch change invalidates old contexts | `topology_epoch_invalidates_old_contexts_before_forwarding` |
//! | 17 | a hostile control publisher is inert | `hostile_control_publisher_is_inert_in_the_full_topology` |
//! | 18 | production relay allocation | **NOT in this file** — `subnet_relay_alloc_e2e` |
//! | 19 | forged locator fields select no authority | `forged_locator_fields_select_no_authority` |
//! | 20 | two gateways re-authenticate and re-tag every hop | `a_two_gateway_route_reauthenticates_every_hop`, `removing_the_second_gateways_exact_right_stops_the_hop` |
//!
//! The D7 seam's own failure modes (registration shape, live darkness
//! on every authority movement, epoch pinning, recovery, coherent
//! publication) are pinned by the focused inverses alongside them.

#![cfg(all(feature = "net", feature = "cortex"))]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::behavior::org::{OrgId, OrgKeypair, OrgMembershipCert, OrgRevocationBundle};
use net::adapter::net::behavior::org_admission::OrgAdmission;
use net::adapter::net::behavior::org_authority::NodeAuthority;
use net::adapter::net::behavior::org_grant::{
    CapabilityAuthorityId, DispatcherScope, GrantRights, GrantTargetScope, OrgCapabilityGrant,
    OrgDispatcherGrant,
};
use net::adapter::net::cortex::{
    RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus,
};
use net::adapter::net::identity::EntityId;
use net::adapter::net::mesh_rpc::{CallOptions, OrgProofIntent, RpcError, ServeError};
use net::adapter::net::subnet::route_hop::ROUTE_HOP_MAGIC;
use net::adapter::net::subnet::{
    build_gateway_context_set, compile_gateway_context, GatewayAdvertisement, SubnetAuthError,
    SubnetAuthPresentation, SubnetAuthorityConfig, SubnetBoundarySet, SubnetControlFact,
    SubnetCredentialSet, SubnetDescriptor, SubnetExportBinding, SubnetExportPolicy,
    SubnetFloorRegistry, SubnetGrant, SubnetRef, SubnetRevocationFloor, SubnetRights,
    TopologySubnetId, VerifiedSubnetContext,
};
use net::adapter::net::{
    ChannelConfig, ChannelConfigRegistry, ChannelId, ChannelName, ChannelPublisher, EntityKeypair,
    MeshNode, MeshNodeConfig, OnFailure, PermissionToken, PublishConfig, Reliability,
    RoutingHeader, SocketBufferConfig, TokenCache, TokenScope,
};
use tokio::net::UdpSocket;

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
const PERCEPTION: &[u8] = &[3, 7];
const WORLD_MODEL: &[u8] = &[3, 7, 1];
const CAMERA: &[u8] = &[3, 7, 2];
const RADAR: &[u8] = &[3, 7, 3];
const CHASSIS: &[u8] = &[3, 8];
const BRAKING: &[u8] = &[3, 8, 1];

/// The camera node's deterministic identity (§9).
const CAMERA_SEED: [u8; 32] = [0xA3; 32];

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

/// Establish a session `initiator → responder` WITHOUT starting
/// either dispatch loop, so a multi-node topology can be wired before
/// any node is accepting while already running (§9).
async fn connect_no_start(initiator: &Arc<MeshNode>, responder: &Arc<MeshNode>) {
    let i_id = initiator.node_id();
    let r_id = responder.node_id();
    let r_pub = *responder.public_key();
    let r_addr = responder.local_addr();
    let r = responder.clone();
    let accept = tokio::spawn(async move { r.accept(i_id).await });
    initiator
        .connect(r_addr, &r_pub, r_id)
        .await
        .expect("connect failed");
    accept
        .await
        .expect("accept task panicked")
        .expect("accept failed");
}

/// Signed announcements from every node, then wait until `hub` has
/// pinned each spoke and each spoke has pinned `hub`.
async fn announce_and_pin(hub: &Arc<MeshNode>, spokes: &[&Arc<MeshNode>]) {
    hub.announce_capabilities(CapabilitySet::new())
        .await
        .expect("hub announce");
    for s in spokes {
        s.announce_capabilities(CapabilitySet::new())
            .await
            .expect("spoke announce");
    }
    let hub_id = hub.node_id();
    let spoke_ids: Vec<u64> = spokes.iter().map(|s| s.node_id()).collect();
    assert!(
        wait_until(Duration::from_secs(5), || {
            spoke_ids.iter().all(|id| hub.peer_entity_id(*id).is_some())
                && spokes.iter().all(|s| s.peer_entity_id(hub_id).is_some())
        })
        .await,
        "entity pins established in both directions across the topology",
    );
}

/// The canonical fleet provisioning applied to an ALREADY connected
/// and started Vehicle A / Vehicle B pair.
async fn provision_fleet(
    vehicle_a: Arc<MeshNode>,
    vehicle_b: Arc<MeshNode>,
    tag: &str,
) -> FleetFixture {
    let vb_kp = EntityKeypair::from_bytes(VEHICLE_B_SEED);
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

/// The two-node fleet: Vehicle A ↔ Vehicle B, provisioned.
async fn fleet_fixture(tag: &str) -> FleetFixture {
    let vehicle_b = build_vehicle_b().await;
    let vehicle_a = build_vehicle_a().await;
    bring_up(&vehicle_a, &vehicle_b).await;
    provision_fleet(vehicle_a, vehicle_b, tag).await
}

/// The fleet PLUS extra peers attached to Vehicle B (an internal
/// camera, an external partner client, …), with EVERY edge
/// handshaked before any dispatch loop starts (§9).
async fn fleet_fixture_with_peers(
    tag: &str,
    seeds: &[[u8; 32]],
) -> (FleetFixture, Vec<Arc<MeshNode>>) {
    let vehicle_b = build_vehicle_b().await;
    let vehicle_a = build_vehicle_a().await;
    let mut peers = Vec::with_capacity(seeds.len());
    for seed in seeds {
        peers.push(build_peer(*seed).await);
    }

    connect_no_start(&vehicle_a, &vehicle_b).await;
    for p in &peers {
        connect_no_start(p, &vehicle_b).await;
    }
    vehicle_a.start();
    vehicle_b.start();
    for p in &peers {
        p.start();
    }
    let mut spokes: Vec<&Arc<MeshNode>> = vec![&vehicle_a];
    spokes.extend(peers.iter());
    announce_and_pin(&vehicle_b, &spokes).await;

    let fixture = provision_fleet(vehicle_a, vehicle_b, tag).await;
    (fixture, peers)
}

/// The fleet PLUS the internal camera peer.
async fn fleet_fixture_with_camera(tag: &str) -> (FleetFixture, Arc<MeshNode>) {
    let (f, mut peers) = fleet_fixture_with_peers(tag, &[CAMERA_SEED]).await;
    (f, peers.remove(0))
}

/// The control-facts channel Vehicle B consumes. An ORDINARY
/// configured channel — no reserved namespace — carrying no authority
/// of its own (S5/D8).
fn control_channel() -> ChannelName {
    ChannelName::new("vehicle-b/subnet/control").unwrap()
}

/// The control-channel publisher's deterministic identity. It is an
/// ordinary channel participant and holds NO subnet-authority root.
const CONTROL_PUB_SEED: [u8; 32] = [0xA5; 32];

fn publisher_for(name: ChannelName) -> ChannelPublisher {
    ChannelPublisher::new(
        name,
        PublishConfig {
            reliability: Reliability::FireAndForget,
            on_failure: OnFailure::BestEffort,
            max_inflight: 16,
        },
    )
}

/// Vehicle B, additionally consuming [`control_channel`] as its
/// signed-control-fact transport.
async fn build_vehicle_b_with_control() -> Arc<MeshNode> {
    let mut cfg = base_config()
        .with_subnet_authority(SubnetAuthorityConfig {
            authority: vb_subnet_root().entity_id().clone(),
            roots: vec![vb_subnet_root().entity_id().clone()],
            maximum_grant_lifetime_secs: 7 * DAY,
        })
        .with_subnet_control_channel(control_channel());
    cfg.subnet_attachment = Some(TopologySubnetId::new(VEHICLE));
    Arc::new(
        MeshNode::new(EntityKeypair::from_bytes(VEHICLE_B_SEED), cfg)
            .await
            .expect("MeshNode::new vehicle B with control channel"),
    )
}

/// The full vehicle topology: fleet caller, internal camera, and an
/// ordinary control-channel publisher Vehicle B subscribes to.
async fn fleet_fixture_with_control(tag: &str) -> (FleetFixture, Arc<MeshNode>, Arc<MeshNode>) {
    let vehicle_b = build_vehicle_b_with_control().await;
    let vehicle_a = build_vehicle_a().await;
    let camera = build_peer(CAMERA_SEED).await;
    let publisher = build_peer(CONTROL_PUB_SEED).await;

    connect_no_start(&vehicle_a, &vehicle_b).await;
    connect_no_start(&camera, &vehicle_b).await;
    connect_no_start(&publisher, &vehicle_b).await;
    vehicle_a.start();
    vehicle_b.start();
    camera.start();
    publisher.start();
    announce_and_pin(&vehicle_b, &[&vehicle_a, &camera, &publisher]).await;

    // Vehicle B consumes the publisher's ordinary channel.
    vehicle_b
        .subscribe_channel(publisher.node_id(), control_channel())
        .await
        .expect("subscribe to the control channel");

    let f = provision_fleet(vehicle_a, vehicle_b, tag).await;
    (f, camera, publisher)
}

/// Publish `bytes` on the control channel and wait until Vehicle B's
/// authority epoch reaches `expect_epoch` — a bounded state
/// predicate, never a blind sleep (§9).
async fn publish_control_fact_and_await_epoch(
    publisher: &Arc<MeshNode>,
    vehicle_b: &Arc<MeshNode>,
    bytes: Vec<u8>,
    expect_epoch: u64,
) -> bool {
    publisher
        .publish(&publisher_for(control_channel()), Bytes::from(bytes))
        .await
        .expect("publish control fact");
    wait_until(Duration::from_secs(5), || {
        vehicle_b
            .subnet_floor_registry()
            .auth_epoch(vb_subnet_root().entity_id())
            == expect_epoch
    })
    .await
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

// ===========================================================================
// §6 Scenario A — vehicle-internal authority is hierarchical and
// authority-local. Evidence 1, 4, 6, 7, 8, 11.
// ===========================================================================

/// A plain peer node: it PRESENTS credentials, it does not anchor or
/// verify any authority of its own.
async fn build_peer(seed: [u8; 32]) -> Arc<MeshNode> {
    Arc::new(
        MeshNode::new(EntityKeypair::from_bytes(seed), base_config())
            .await
            .expect("MeshNode::new peer"),
    )
}

/// Drive a LIVE S3 admission of `peer` into `verifier` at
/// `attachment` under a grant scoped at `scope`: real session, real
/// one-use challenge, real signed presentation, production
/// `admit_subnet_session`. Returns the verifier's own verdict.
async fn try_admit_vb(
    verifier: &Arc<MeshNode>,
    peer: &Arc<MeshNode>,
    peer_kp: &EntityKeypair,
    scope: &[u8],
    attachment: &[u8],
    rights: SubnetRights,
) -> Result<VerifiedSubnetContext, SubnetAuthError> {
    let set = vb_grant(peer_kp, scope, rights);
    let node_id = peer.node_id();
    let nonce = verifier
        .issue_subnet_challenge(node_id)
        .expect("verifier issues a challenge");
    let session_id = verifier
        .peer_session_id(node_id)
        .expect("the peer has a live session");
    let presentation = SubnetAuthPresentation::try_issue(
        peer_kp,
        set.credential_set_hash(),
        session_id,
        verifier.entity_id().clone(),
        nonce,
        vb_ref(attachment),
        rights,
    )
    .expect("issue presentation");
    verifier.admit_subnet_session(node_id, &presentation, &set)
}

/// Evidence 6, 7: attachment is exact, and inheritance runs downward
/// only. A camera-scoped grant admits at the camera domain and
/// NOWHERE else — not upward to perception or the vehicle root, not
/// sideways to radar or chassis. A perception-scoped PARENT grant
/// reaches every descendant with no per-child grant issued, and still
/// stops at its own subtree edge.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vehicle_internal_authority_is_hierarchical() {
    let vehicle_b = build_vehicle_b().await;
    let camera = build_peer(CAMERA_SEED).await;
    let camera_kp = EntityKeypair::from_bytes(CAMERA_SEED);
    bring_up(&camera, &vehicle_b).await;

    let ctx = try_admit_vb(
        &vehicle_b,
        &camera,
        &camera_kp,
        CAMERA,
        CAMERA,
        SubnetRights::ATTACH,
    )
    .await
    .expect("evidence 6: the camera attaches at its own domain");
    assert_eq!(ctx.attachment, TopologySubnetId::new(CAMERA));
    assert_eq!(ctx.scope, TopologySubnetId::new(CAMERA));

    for (target, what) in [
        (PERCEPTION, "upward to its parent"),
        (VEHICLE, "upward to the vehicle root"),
        (RADAR, "sideways to radar"),
        (CHASSIS, "sideways to chassis"),
        (BRAKING, "sideways into the chassis subtree"),
    ] {
        assert_eq!(
            try_admit_vb(
                &vehicle_b,
                &camera,
                &camera_kp,
                CAMERA,
                target,
                SubnetRights::ATTACH,
            )
            .await
            .expect_err("evidence 6: a camera-scoped grant must not reach elsewhere"),
            SubnetAuthError::ScopeNotAncestor,
            "camera attaching {what} must be refused as out of scope",
        );
    }

    for target in [WORLD_MODEL, CAMERA, RADAR, PERCEPTION] {
        try_admit_vb(
            &vehicle_b,
            &camera,
            &camera_kp,
            PERCEPTION,
            target,
            SubnetRights::ATTACH,
        )
        .await
        .expect("evidence 7: a perception parent grant covers its whole subtree");
    }
    for target in [CHASSIS, VEHICLE] {
        assert_eq!(
            try_admit_vb(
                &vehicle_b,
                &camera,
                &camera_kp,
                PERCEPTION,
                target,
                SubnetRights::ATTACH,
            )
            .await
            .expect_err("a perception grant must not escape perception"),
            SubnetAuthError::ScopeNotAncestor,
        );
    }
}

/// Evidence 8: equal compact path bits under two different subnet
/// authorities are unrelated. Vehicle A's root signing the SAME path
/// is not merely insufficient at Vehicle B — Vehicle B anchors no
/// such authority, so it fails closed before any path comparison.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn equal_path_bits_under_two_authorities_are_unrelated() {
    let vehicle_b = build_vehicle_b().await;
    let camera = build_peer(CAMERA_SEED).await;
    let camera_kp = EntityKeypair::from_bytes(CAMERA_SEED);
    bring_up(&camera, &vehicle_b).await;

    let va_set = SubnetCredentialSet::Direct(
        SubnetGrant::try_issue(
            &va_subnet_root(),
            va_subnet_root().entity_id().clone(),
            TopologySubnetId::new(WORLD_MODEL),
            0,
            camera_kp.entity_id().clone(),
            SubnetRights::ATTACH,
            1,
            unix_now() - 60,
            DAY,
        )
        .expect("issue Vehicle A grant"),
    );
    let node_id = camera.node_id();
    let nonce = vehicle_b
        .issue_subnet_challenge(node_id)
        .expect("challenge");
    let session_id = vehicle_b.peer_session_id(node_id).expect("session");
    let presentation = SubnetAuthPresentation::try_issue(
        &camera_kp,
        va_set.credential_set_hash(),
        session_id,
        vehicle_b.entity_id().clone(),
        nonce,
        SubnetRef {
            authority: va_subnet_root().entity_id().clone(),
            path: TopologySubnetId::new(WORLD_MODEL),
        },
        SubnetRights::ATTACH,
    )
    .expect("presentation");

    assert_eq!(
        vehicle_b
            .admit_subnet_session(node_id, &presentation, &va_set)
            .expect_err("evidence 8: another vehicle's authority is not this vehicle's"),
        SubnetAuthError::UnknownAuthority,
    );
    assert!(
        vehicle_b.subnet_context_for(node_id).is_none(),
        "no context may be installed from a foreign authority's grant",
    );

    // The same peer, the same path bits, under Vehicle B's OWN root:
    // admitted. The authority qualification is what decided it.
    try_admit_vb(
        &vehicle_b,
        &camera,
        &camera_kp,
        WORLD_MODEL,
        WORLD_MODEL,
        SubnetRights::ATTACH,
    )
    .await
    .expect("the authority-qualified grant is what admits");
}

/// Evidence 1, 4, 11: neither plane manufactures the other. A BMW
/// fleet proof invokes Vehicle B's exported provider and creates NO
/// Vehicle B subnet context; a genuine Vehicle B subnet context
/// invokes NOTHING without an org proof — and the org-plane denial
/// leaves the subnet plane untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn neither_plane_manufactures_the_other() {
    let (f, camera) = fleet_fixture_with_camera("plane-independence").await;
    let camera_kp = EntityKeypair::from_bytes(CAMERA_SEED);

    f.call(true)
        .await
        .expect("the org-authorized fleet call admits");
    f.assert_no_va_subnet_context();

    try_admit_vb(
        &f.vehicle_b,
        &camera,
        &camera_kp,
        CAMERA,
        CAMERA,
        SubnetRights::ATTACH,
    )
    .await
    .expect("the camera is admitted internally");
    assert!(
        f.vehicle_b.subnet_context_for(camera.node_id()).is_some(),
        "precondition: the camera holds a live subnet context",
    );

    let baseline = f.calls.load(Ordering::SeqCst);
    let result = camera
        .call(
            f.vehicle_b.node_id(),
            SERVICE,
            Bytes::from_static(b"roi?"),
            call_opts(None),
        )
        .await;
    assert_explicit_denial(result, "subnet context without org authority");
    assert_handler_stays_at(&f.calls, baseline, "subnet context without org authority").await;
    assert!(
        f.vehicle_b.subnet_context_for(camera.node_id()).is_some(),
        "an org-plane denial must not disturb the subnet plane",
    );

    let _ = std::fs::remove_dir_all(&f.dir);
}

// ===========================================================================
// §6 Scenario B — the Partner diagnostic is exactly bounded.
// Evidence 9; reinforces 3, 4, 11.
// ===========================================================================

/// The Partner Org root and its diagnostic client identity.
const PARTNER_ORG_SEED: [u8; 32] = [0xB7; 32];
const PARTNER_SEED: [u8; 32] = [0xA4; 32];
const DIAGNOSTIC: &str = "diagnostic.snapshot";

fn partner_org() -> OrgKeypair {
    OrgKeypair::from_bytes(PARTNER_ORG_SEED)
}

/// A real cross-org INVOKE intent: the Partner client's membership and
/// dispatcher grant come from PARTNER Org; the capability grant is
/// issued by BMW (the provider-owner org) with an exact target scope.
fn partner_intent(
    provider: EntityId,
    service: &str,
    target_scope: GrantTargetScope,
) -> OrgProofIntent {
    let caller_kp = EntityKeypair::from_bytes(PARTNER_SEED);
    let caller_entity = caller_kp.entity_id().clone();
    let cap = CapabilityAuthorityId::for_tag(&format!("nrpc:{service}"));
    let (grant, secret) = OrgCapabilityGrant::try_issue(
        &bmw(),
        partner_org().org_id(),
        cap,
        GrantRights::INVOKE,
        target_scope,
        3600,
    )
    .expect("BMW issues the cross-org INVOKE grant");
    assert!(
        secret.is_none(),
        "an INVOKE-only grant carries no audience material",
    );
    let membership = OrgMembershipCert::try_issue(&partner_org(), caller_entity.clone(), 1, 3600)
        .expect("partner membership");
    let dispatcher = OrgDispatcherGrant::try_issue(
        &partner_org(),
        caller_entity,
        DispatcherScope::Exact(cap),
        3600,
    )
    .expect("partner dispatcher");
    OrgProofIntent {
        caller: Arc::new(caller_kp),
        membership,
        dispatcher,
        capability_grant: Some(grant),
        acting_org: partner_org().org_id(),
        provider_owner_org: bmw().org_id(),
        provider,
        capability: cap,
        proof_ttl_secs: 30,
    }
}

/// Evidence 9: the Partner Org's capability grant reaches EXACTLY its
/// one exported diagnostic provider and nothing else — not another
/// capability on the same node, not another provider target, and no
/// internal Vehicle B presence at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn partner_diagnostic_is_exactly_bounded() {
    let (f, peers) = fleet_fixture_with_peers("partner", &[PARTNER_SEED]).await;
    let partner = &peers[0];
    let provider = f.provider.clone();

    // Vehicle B additionally exports ONE diagnostic capability through
    // the SAME declared world-model boundary, cross-org admitted.
    let diag_calls = Arc::new(AtomicUsize::new(0));
    let _diag = f
        .vehicle_b
        .serve_rpc_subnet_exported(
            DIAGNOSTIC,
            Arc::new(RoiHandler {
                calls: diag_calls.clone(),
                attribution_ok: Arc::new(AtomicBool::new(false)),
                proof_stripped: Arc::new(AtomicBool::new(false)),
                expected_caller: partner.entity_id().clone(),
                expected_org: partner_org().org_id(),
                expected_provider: provider.clone(),
            }),
            OrgAdmission::CrossOrgGranted,
            world_model_binding(0),
            Arc::new(|_| true),
        )
        .expect("serve the exported diagnostic");

    // The exact grant, for the exact capability, at the exact node.
    let reply = partner
        .call(
            f.vehicle_b.node_id(),
            DIAGNOSTIC,
            Bytes::from_static(b"snapshot?"),
            call_opts(Some(partner_intent(
                provider.clone(),
                DIAGNOSTIC,
                GrantTargetScope::ExactNode(provider.clone()),
            ))),
        )
        .await
        .expect("evidence 9: the exact partner grant reaches its exported provider");
    assert_eq!(reply.body.as_ref(), b"roi-window");
    assert_eq!(
        diag_calls.load(Ordering::SeqCst),
        1,
        "the diagnostic handler ran exactly once",
    );

    // ANOTHER capability on the same provider: the diagnostic grant
    // does not travel to perception.roi.
    let roi_baseline = f.calls.load(Ordering::SeqCst);
    let result = partner
        .call(
            f.vehicle_b.node_id(),
            SERVICE,
            Bytes::from_static(b"roi?"),
            call_opts(Some(partner_intent(
                provider.clone(),
                SERVICE,
                // The grant names the DIAGNOSTIC capability, but the
                // call invokes perception.roi.
                GrantTargetScope::ExactNode(provider.clone()),
            ))),
        )
        .await;
    assert_explicit_denial(result, "partner reaching another capability");
    assert_handler_stays_at(
        &f.calls,
        roi_baseline,
        "partner reaching another capability",
    )
    .await;

    // ANOTHER provider target: a grant scoped to a different exact
    // node does not authorize this one.
    let diag_baseline = diag_calls.load(Ordering::SeqCst);
    let elsewhere = EntityKeypair::from_bytes([0xEE; 32]).entity_id().clone();
    let result = partner
        .call(
            f.vehicle_b.node_id(),
            DIAGNOSTIC,
            Bytes::from_static(b"snapshot?"),
            call_opts(Some(partner_intent(
                provider.clone(),
                DIAGNOSTIC,
                GrantTargetScope::ExactNode(elsewhere),
            ))),
        )
        .await;
    assert_explicit_denial(result, "partner grant scoped to another provider");
    assert_handler_stays_at(&diag_calls, diag_baseline, "partner grant scoped elsewhere").await;

    // Evidence 4/11 for the Partner: an exported capability call
    // creates NO Vehicle B subnet context, so nothing internal —
    // camera, radar, chassis — is addressable by it.
    assert!(
        f.vehicle_b.subnet_context_for(partner.node_id()).is_none(),
        "the Partner client must acquire no Vehicle B subnet context",
    );
    // And it cannot manufacture one. Vehicle B's root never signed a
    // grant for the Partner; the best credential the Partner can
    // actually produce is one under an authority Vehicle B does not
    // anchor, which fails closed at every internal attachment.
    let partner_kp = EntityKeypair::from_bytes(PARTNER_SEED);
    let foreign_root = va_subnet_root();
    for internal in [CAMERA, RADAR, CHASSIS] {
        let set = SubnetCredentialSet::Direct(
            SubnetGrant::try_issue(
                &foreign_root,
                foreign_root.entity_id().clone(),
                TopologySubnetId::new(internal),
                0,
                partner_kp.entity_id().clone(),
                SubnetRights::ATTACH,
                1,
                unix_now() - 60,
                DAY,
            )
            .expect("issue foreign-authority grant"),
        );
        let node_id = partner.node_id();
        let nonce = f
            .vehicle_b
            .issue_subnet_challenge(node_id)
            .expect("challenge");
        let session_id = f.vehicle_b.peer_session_id(node_id).expect("session");
        let presentation = SubnetAuthPresentation::try_issue(
            &partner_kp,
            set.credential_set_hash(),
            session_id,
            f.vehicle_b.entity_id().clone(),
            nonce,
            SubnetRef {
                authority: foreign_root.entity_id().clone(),
                path: TopologySubnetId::new(internal),
            },
            SubnetRights::ATTACH,
        )
        .expect("presentation");
        assert_eq!(
            f.vehicle_b
                .admit_subnet_session(node_id, &presentation, &set)
                .expect_err("the Partner has no Vehicle B subnet authority"),
            SubnetAuthError::UnknownAuthority,
            "no internal attachment is reachable for the Partner",
        );
    }
    assert!(
        f.vehicle_b.subnet_context_for(partner.node_id()).is_none(),
        "and still no context after every attempt",
    );

    let _ = std::fs::remove_dir_all(&f.dir);
}

// ===========================================================================
// §6 Scenario C — channel authority is independent of subnet
// authority, in BOTH directions. Evidence 10.
// ===========================================================================

/// Evidence 10: a protected internal channel still requires its own
/// channel token despite a valid PARENT subnet context — and the
/// channel token, once held, manufactures no subnet attachment and no
/// provider invocation authority.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn channel_authority_remains_independent_of_subnet_authority() {
    // Vehicle B publishes a token-gated internal channel. The registry
    // must be installed before the dispatch loop starts.
    let vehicle_b = {
        let mut cfg = base_config().with_subnet_authority(SubnetAuthorityConfig {
            authority: vb_subnet_root().entity_id().clone(),
            roots: vec![vb_subnet_root().entity_id().clone()],
            maximum_grant_lifetime_secs: 7 * DAY,
        });
        cfg.subnet_attachment = Some(TopologySubnetId::new(VEHICLE));
        let mut node = MeshNode::new(EntityKeypair::from_bytes(VEHICLE_B_SEED), cfg)
            .await
            .expect("MeshNode::new vehicle B");
        let registry = Arc::new(ChannelConfigRegistry::new());
        let channel = ChannelName::new("vehicle-b/perception/internal").unwrap();
        registry.insert(
            ChannelConfig::new(ChannelId::new(channel)).with_token_roots(vec![
                EntityKeypair::from_bytes(VEHICLE_B_SEED)
                    .entity_id()
                    .clone(),
            ]),
        );
        node.set_channel_configs(registry);
        node.set_token_cache(Arc::new(TokenCache::new()));
        Arc::new(node)
    };
    let vb_kp = EntityKeypair::from_bytes(VEHICLE_B_SEED);
    let camera = build_peer(CAMERA_SEED).await;
    let camera_kp = EntityKeypair::from_bytes(CAMERA_SEED);
    bring_up(&camera, &vehicle_b).await;

    let channel = ChannelName::new("vehicle-b/perception/internal").unwrap();

    // The camera holds a genuine PARENT (perception-scoped) subnet
    // context — strictly stronger, internally, than the channel needs.
    try_admit_vb(
        &vehicle_b,
        &camera,
        &camera_kp,
        PERCEPTION,
        CAMERA,
        SubnetRights::ATTACH,
    )
    .await
    .expect("the camera is admitted under a parent perception grant");
    assert!(
        vehicle_b.subnet_context_for(camera.node_id()).is_some(),
        "precondition: a live parent subnet context",
    );

    // Direction 1 — a parent subnet context is NOT a channel
    // credential: the token-gated channel refuses it.
    assert!(
        camera
            .subscribe_channel(vehicle_b.node_id(), channel.clone())
            .await
            .is_err(),
        "evidence 10: a valid parent subnet context must not admit a \
         token-gated internal channel",
    );

    // With the channel's own token, the same peer is admitted.
    let token = PermissionToken::issue(
        &vb_kp,
        camera_kp.entity_id().clone(),
        TokenScope::SUBSCRIBE,
        channel.hash(),
        300,
        0,
    );
    camera
        .subscribe_channel_with_token(vehicle_b.node_id(), channel.clone(), token)
        .await
        .expect("the channel token is what admits the channel");

    // Direction 2 — the channel token manufactures NO subnet
    // authority: the camera still cannot attach outside its grant…
    assert_eq!(
        try_admit_vb(
            &vehicle_b,
            &camera,
            &camera_kp,
            CAMERA,
            CHASSIS,
            SubnetRights::ATTACH,
        )
        .await
        .expect_err("a channel token grants no ATTACH anywhere"),
        SubnetAuthError::ScopeNotAncestor,
    );
    // …and holds no ROUTE/EXPORT: this node published no gateway
    // authority for the channel subscriber, so protected forwarding
    // state is untouched by the subscription.
    assert!(
        vehicle_b.subnet_gateway_contexts().is_none(),
        "a channel subscription must not publish gateway authority",
    );
}

// ===========================================================================
// §6 Scenario D — organization and subnet revocation are independent,
// live and in both directions. Evidence 12, 13.
// ===========================================================================

/// Re-adopt Vehicle B's BMW authority in its EXISTING store with a
/// membership cert at `generation` — the operator's "here are current
/// credentials" step after a revocation.
///
/// Deliberately the same store: installing a fresh one would be a
/// revocation DOWNGRADE, which `install_node_authority` refuses
/// (`NonMonotonicReplacement`). Recovery must clear the floor by
/// presenting a newer cert, never by forgetting the floor.
fn readopt_bmw_authority(server: &Arc<MeshNode>, dir: &std::path::Path, generation: u32) {
    let node_entity = server.entity_id().clone();
    let node_cert = OrgMembershipCert::try_issue(&bmw(), node_entity.clone(), generation, 3600)
        .expect("node cert");
    let authority =
        NodeAuthority::adopt(dir, node_cert, &node_entity, 0, None).expect("re-adopt authority");
    server
        .install_node_authority(Arc::new(authority))
        .expect("install re-adopted authority");
}

/// Evidence 12 and 13: the org plane and the subnet plane revoke
/// INDEPENDENTLY.
///
/// Direction one — raising Vehicle B's BMW membership floor blocks
/// subsequent fleet-authorized calls (dark handler) while its subnet
/// auth epoch and internal contexts stay exactly as they were.
///
/// Direction two — a signed perception floor delivered over the
/// ordinary control channel kills perception-scoped internal
/// credentials while chassis is untouched, a vehicle-root grant
/// remains structurally dominant, and BMW membership keeps working.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn org_and_subnet_revocation_are_independent_live() {
    let (f, camera, publisher) = fleet_fixture_with_control("indep-revocation").await;
    let camera_kp = EntityKeypair::from_bytes(CAMERA_SEED);

    // Baseline: the fleet call works and the camera is internally
    // admitted under a perception-scoped grant.
    f.call(true).await.expect("baseline fleet call admits");
    try_admit_vb(
        &f.vehicle_b,
        &camera,
        &camera_kp,
        PERCEPTION,
        CAMERA,
        SubnetRights::ATTACH,
    )
    .await
    .expect("camera admitted under perception");
    assert!(f.vehicle_b.subnet_context_for(camera.node_id()).is_some());
    let subnet_epoch_before = f
        .vehicle_b
        .subnet_floor_registry()
        .auth_epoch(vb_subnet_root().entity_id());

    // ---- Direction 1: ORG revocation --------------------------------
    // BMW raises a membership floor above Vehicle B's own cert
    // generation. Its provider self-verification now fails.
    let mut floors = std::collections::BTreeMap::new();
    floors.insert(f.vehicle_b.entity_id().clone(), 9u32);
    let bundle = OrgRevocationBundle::try_issue(&bmw(), &floors).expect("issue org bundle");
    f.vehicle_b
        .node_authority()
        .expect("authority installed")
        .revocation
        .apply_bundle(&bundle)
        .expect("apply org floor");

    let baseline = f.calls.load(Ordering::SeqCst);
    assert_explicit_denial(f.call(true).await, "org membership revoked");
    assert_handler_stays_at(&f.calls, baseline, "org membership revoked").await;

    // The SUBNET plane did not move: same auth epoch, same live
    // internal context.
    assert_eq!(
        f.vehicle_b
            .subnet_floor_registry()
            .auth_epoch(vb_subnet_root().entity_id()),
        subnet_epoch_before,
        "evidence 12: an org revocation must not touch the subnet auth epoch",
    );
    assert!(
        f.vehicle_b.subnet_context_for(camera.node_id()).is_some(),
        "evidence 12: the internal subnet context survives an org revocation",
    );

    // ---- Direction 2: SUBNET revocation -----------------------------
    // Re-provision CURRENT BMW credentials (generation above the
    // floor) in the same store, so the org plane is healthy again
    // without forgetting the floor.
    readopt_bmw_authority(&f.vehicle_b, &f.dir, 10);
    f.call(true)
        .await
        .expect("current BMW credentials restore the fleet call");

    // A signed perception floor arrives over the ordinary control
    // channel — the publisher holds no subnet authority; the
    // SIGNATURE is what makes this fact real.
    let floor = SubnetRevocationFloor::try_issue(
        &vb_subnet_root(),
        vb_ref(PERCEPTION),
        0,
        5, // above the generation-1 internal grants
        1,
        unix_now(),
    )
    .expect("issue perception floor");
    assert!(
        publish_control_fact_and_await_epoch(
            &publisher,
            &f.vehicle_b,
            SubnetControlFact::RevocationFloor(floor).to_bytes(),
            subnet_epoch_before + 1,
        )
        .await,
        "the signed perception floor must be accepted over the control channel",
    );

    // Perception-scoped generation-1 credentials are dead…
    assert_eq!(
        try_admit_vb(
            &f.vehicle_b,
            &camera,
            &camera_kp,
            PERCEPTION,
            CAMERA,
            SubnetRights::ATTACH,
        )
        .await
        .expect_err("evidence 13: perception-scoped grants below the floor are revoked"),
        SubnetAuthError::Revoked,
    );
    // …while the unrelated CHASSIS subtree is untouched…
    try_admit_vb(
        &f.vehicle_b,
        &camera,
        &camera_kp,
        CHASSIS,
        BRAKING,
        SubnetRights::ATTACH,
    )
    .await
    .expect("evidence 13: a chassis-scoped grant is unaffected by a perception floor");
    // …and a vehicle-root grant remains structurally dominant: the
    // perception floor does not lie on its ancestor chain.
    try_admit_vb(
        &f.vehicle_b,
        &camera,
        &camera_kp,
        VEHICLE,
        CAMERA,
        SubnetRights::ATTACH,
    )
    .await
    .expect("evidence 13: the vehicle-root grant stays structurally dominant");

    // The fleet call is now denied too — but for a SUBNET reason, not
    // an org one: world-model lies inside perception, so Vehicle B's
    // own generation-1 EXPORT credential is below the new floor and
    // its exported service darkens (the D7 contract).
    let baseline = f.calls.load(Ordering::SeqCst);
    assert_explicit_denial(
        f.call(true).await,
        "export credential below the subnet floor",
    );
    assert_handler_stays_at(
        &f.calls,
        baseline,
        "export credential below the subnet floor",
    )
    .await;

    // Re-issuing ONLY the subnet-side gateway credentials above the
    // floor — with BMW membership untouched throughout — restores the
    // call. That is the independence: the org plane never moved, so
    // repairing the subnet plane alone is sufficient.
    let above_floor = vec![
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
        .install_subnet_gateway_credentials(&above_floor)
        .expect("install above-floor gateway credentials");
    f.call(true).await.expect(
        "evidence 13: repairing only the SUBNET plane restores the call — \
         BMW membership was never disturbed by the subnet floor",
    );

    let _ = std::fs::remove_dir_all(&f.dir);
}

// ===========================================================================
// §6 Scenario E — every authority is re-proven per session, and
// neither axis is manufactured from the other. Evidence 14, 15.
// ===========================================================================

/// Evidence 14: replaying the camera's credential set or its
/// presentation from an outsider proves nothing. The subject bound
/// into the grant is a full `EntityId`; copying the derived routing
/// id or the public topology path buys no authority.
///
/// Evidence 15 (session/challenge half): every admission consumes a
/// one-use verifier challenge bound to the exact live session, so a
/// captured presentation cannot be re-presented — which is what makes
/// a reconnect re-prove the authority rather than inherit it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn replayed_credentials_and_presentations_prove_nothing() {
    const OUTSIDER_SEED: [u8; 32] = [0xEF; 32];
    let (f, peers) = fleet_fixture_with_peers("replay", &[CAMERA_SEED, OUTSIDER_SEED]).await;
    let (camera, outsider) = (&peers[0], &peers[1]);
    let camera_kp = EntityKeypair::from_bytes(CAMERA_SEED);
    let outsider_kp = EntityKeypair::from_bytes(OUTSIDER_SEED);

    // The camera's genuine credential set and a genuine presentation.
    let set = vb_grant(&camera_kp, CAMERA, SubnetRights::ATTACH);
    let camera_node = camera.node_id();
    let nonce = f
        .vehicle_b
        .issue_subnet_challenge(camera_node)
        .expect("challenge");
    let session_id = f.vehicle_b.peer_session_id(camera_node).expect("session");
    let genuine = SubnetAuthPresentation::try_issue(
        &camera_kp,
        set.credential_set_hash(),
        session_id,
        f.vehicle_b.entity_id().clone(),
        nonce,
        vb_ref(CAMERA),
        SubnetRights::ATTACH,
    )
    .expect("presentation");

    // It admits exactly once…
    f.vehicle_b
        .admit_subnet_session(camera_node, &genuine, &set)
        .expect("the genuine presentation admits");
    // …and REPLAY of the very same presentation fails: the challenge
    // was consumed by the attempt itself.
    assert_eq!(
        f.vehicle_b
            .admit_subnet_session(camera_node, &genuine, &set)
            .expect_err("evidence 15: a captured presentation cannot be replayed"),
        SubnetAuthError::WrongChallenge,
    );

    // The OUTSIDER replays the camera's credential set under a fresh
    // challenge of its own, signing the presentation itself: the
    // grant's subject is the camera's full EntityId, so the outsider
    // is refused even though the credential bytes are genuine.
    let outsider_node = outsider.node_id();
    let out_nonce = f
        .vehicle_b
        .issue_subnet_challenge(outsider_node)
        .expect("challenge");
    let out_session = f.vehicle_b.peer_session_id(outsider_node).expect("session");
    let stolen = SubnetAuthPresentation::try_issue(
        &outsider_kp,
        set.credential_set_hash(),
        out_session,
        f.vehicle_b.entity_id().clone(),
        out_nonce,
        vb_ref(CAMERA),
        SubnetRights::ATTACH,
    )
    .expect("presentation");
    assert_eq!(
        f.vehicle_b
            .admit_subnet_session(outsider_node, &stolen, &set)
            .expect_err("evidence 14: a stolen credential set proves nothing"),
        SubnetAuthError::WrongSubject,
    );
    assert!(
        f.vehicle_b.subnet_context_for(outsider_node).is_none(),
        "the outsider acquires no context",
    );

    // A presentation bound to a DIFFERENT (stale) session is refused
    // even with a fresh, valid challenge — the binding is to the exact
    // incarnation, which is what a reconnect changes.
    let fresh_nonce = f
        .vehicle_b
        .issue_subnet_challenge(camera_node)
        .expect("challenge");
    let stale_session = SubnetAuthPresentation::try_issue(
        &camera_kp,
        set.credential_set_hash(),
        session_id.wrapping_add(1),
        f.vehicle_b.entity_id().clone(),
        fresh_nonce,
        vb_ref(CAMERA),
        SubnetRights::ATTACH,
    )
    .expect("presentation");
    assert_eq!(
        f.vehicle_b
            .admit_subnet_session(camera_node, &stale_session, &set)
            .expect_err("evidence 15: the proof is bound to one incarnation"),
        SubnetAuthError::WrongSession,
    );

    let _ = std::fs::remove_dir_all(&f.dir);
}

/// Evidence 15 (per-axis half): after a SUBNET revocation the subnet
/// axis must be re-proven with above-floor credentials, and doing so
/// manufactures no organization authority — the recovered peer still
/// invokes nothing without an org proof.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn each_axis_recovers_only_itself() {
    let (f, camera, publisher) = fleet_fixture_with_control("axis-recovery").await;
    let camera_kp = EntityKeypair::from_bytes(CAMERA_SEED);

    try_admit_vb(
        &f.vehicle_b,
        &camera,
        &camera_kp,
        PERCEPTION,
        CAMERA,
        SubnetRights::ATTACH,
    )
    .await
    .expect("camera admitted");

    // A signed perception floor arrives.
    let floor = SubnetRevocationFloor::try_issue(
        &vb_subnet_root(),
        vb_ref(PERCEPTION),
        0,
        5,
        1,
        unix_now(),
    )
    .expect("floor");
    assert!(
        publish_control_fact_and_await_epoch(
            &publisher,
            &f.vehicle_b,
            SubnetControlFact::RevocationFloor(floor).to_bytes(),
            1,
        )
        .await,
        "the floor applies",
    );
    assert!(
        f.vehicle_b.subnet_context_for(camera.node_id()).is_none(),
        "the auth-epoch move invalidated the stale context",
    );

    // Re-proving with a BELOW-floor credential fails closed…
    assert_eq!(
        try_admit_vb(
            &f.vehicle_b,
            &camera,
            &camera_kp,
            PERCEPTION,
            CAMERA,
            SubnetRights::ATTACH,
        )
        .await
        .expect_err("a below-floor credential cannot re-prove the axis"),
        SubnetAuthError::Revoked,
    );

    // …and an ABOVE-floor credential recovers exactly the subnet axis.
    let above = SubnetCredentialSet::Direct(
        SubnetGrant::try_issue(
            &vb_subnet_root(),
            vb_subnet_root().entity_id().clone(),
            TopologySubnetId::new(PERCEPTION),
            0,
            camera_kp.entity_id().clone(),
            SubnetRights::ATTACH,
            6,
            unix_now() - 60,
            DAY,
        )
        .expect("above-floor grant"),
    );
    let node_id = camera.node_id();
    let nonce = f
        .vehicle_b
        .issue_subnet_challenge(node_id)
        .expect("challenge");
    let session_id = f.vehicle_b.peer_session_id(node_id).expect("session");
    let presentation = SubnetAuthPresentation::try_issue(
        &camera_kp,
        above.credential_set_hash(),
        session_id,
        f.vehicle_b.entity_id().clone(),
        nonce,
        vb_ref(CAMERA),
        SubnetRights::ATTACH,
    )
    .expect("presentation");
    f.vehicle_b
        .admit_subnet_session(node_id, &presentation, &above)
        .expect("evidence 15: above-floor credentials recover the subnet axis");
    assert!(f.vehicle_b.subnet_context_for(node_id).is_some());

    // The ORG axis was never manufactured by that recovery.
    let baseline = f.calls.load(Ordering::SeqCst);
    let result = camera
        .call(
            f.vehicle_b.node_id(),
            SERVICE,
            Bytes::from_static(b"roi?"),
            call_opts(None),
        )
        .await;
    assert_explicit_denial(result, "recovered subnet axis without org proof");
    assert_handler_stays_at(
        &f.calls,
        baseline,
        "recovered subnet axis without org proof",
    )
    .await;

    let _ = std::fs::remove_dir_all(&f.dir);
}

// ===========================================================================
// §6 Scenario F — a topology-epoch change invalidates old contexts
// before anything can forward under them. Evidence 16.
// ===========================================================================

/// Evidence 16: reparenting (an epoch bump) drops every context
/// minted under the old meaning, old-epoch credentials cannot
/// re-admit, old-epoch control facts do not revive anything, and only
/// fresh epoch-N+1 credentials restore internal authority.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn topology_epoch_invalidates_old_contexts_before_forwarding() {
    let (f, camera, publisher) = fleet_fixture_with_control("epoch-invalidation").await;
    let camera_kp = EntityKeypair::from_bytes(CAMERA_SEED);

    try_admit_vb(
        &f.vehicle_b,
        &camera,
        &camera_kp,
        PERCEPTION,
        CAMERA,
        SubnetRights::ATTACH,
    )
    .await
    .expect("camera admitted under epoch 0");
    assert!(f.vehicle_b.subnet_context_for(camera.node_id()).is_some());

    // Reparenting: the hierarchy is reinterpreted.
    let next_epoch = f.vehicle_b.advance_subnet_topology_epoch();
    assert_eq!(next_epoch, 1);
    assert!(
        f.vehicle_b.subnet_context_for(camera.node_id()).is_none(),
        "evidence 16: every context minted under the old meaning is dropped",
    );

    // Old-epoch credentials cannot re-admit: the path may mean
    // something else now.
    assert_eq!(
        try_admit_vb(
            &f.vehicle_b,
            &camera,
            &camera_kp,
            PERCEPTION,
            CAMERA,
            SubnetRights::ATTACH,
        )
        .await
        .expect_err("an old-epoch credential must not re-admit"),
        SubnetAuthError::WrongTopologyEpoch,
    );

    // An old-epoch signed control fact does not revive anything: it is
    // verified and filed under its own (now superseded) epoch.
    let stale_descriptor =
        SubnetDescriptor::try_issue(&vb_subnet_root(), vb_ref(PERCEPTION), 0, 1, unix_now())
            .expect("old-epoch descriptor");
    publisher
        .publish(
            &publisher_for(control_channel()),
            Bytes::from(SubnetControlFact::Descriptor(stale_descriptor).to_bytes()),
        )
        .await
        .expect("publish old-epoch fact");
    // Order a fresh, current-epoch fact behind it and wait for THAT,
    // so the stale one has provably been processed.
    let current_descriptor = SubnetDescriptor::try_issue(
        &vb_subnet_root(),
        vb_ref(CHASSIS),
        next_epoch,
        1,
        unix_now(),
    )
    .expect("current-epoch descriptor");
    publisher
        .publish(
            &publisher_for(control_channel()),
            Bytes::from(SubnetControlFact::Descriptor(current_descriptor).to_bytes()),
        )
        .await
        .expect("publish current-epoch fact");
    assert!(
        wait_until(Duration::from_secs(5), || f
            .vehicle_b
            .subnet_control_store()
            .descriptor_for(
                vb_subnet_root().entity_id(),
                next_epoch,
                TopologySubnetId::new(CHASSIS)
            )
            .is_some())
        .await,
        "the current-epoch marker fact applied",
    );
    assert!(
        f.vehicle_b.subnet_context_for(camera.node_id()).is_none(),
        "evidence 16: an old-epoch control fact must not revive a dropped context",
    );

    // Fresh epoch-1 credentials restore internal authority.
    let fresh = SubnetCredentialSet::Direct(
        SubnetGrant::try_issue(
            &vb_subnet_root(),
            vb_subnet_root().entity_id().clone(),
            TopologySubnetId::new(PERCEPTION),
            next_epoch,
            camera_kp.entity_id().clone(),
            SubnetRights::ATTACH,
            1,
            unix_now() - 60,
            DAY,
        )
        .expect("fresh-epoch grant"),
    );
    let node_id = camera.node_id();
    let nonce = f
        .vehicle_b
        .issue_subnet_challenge(node_id)
        .expect("challenge");
    let session_id = f.vehicle_b.peer_session_id(node_id).expect("session");
    let presentation = SubnetAuthPresentation::try_issue(
        &camera_kp,
        fresh.credential_set_hash(),
        session_id,
        f.vehicle_b.entity_id().clone(),
        nonce,
        vb_ref(CAMERA),
        SubnetRights::ATTACH,
    )
    .expect("presentation");
    f.vehicle_b
        .admit_subnet_session(node_id, &presentation, &fresh)
        .expect("evidence 16: fresh-epoch credentials restore authority");

    let _ = std::fs::remove_dir_all(&f.dir);
}

// ===========================================================================
// §6 Scenario G — a hostile control-channel publisher is inert in the
// FULL vehicle topology. Evidence 17.
// ===========================================================================

/// Evidence 17: an ordinary, fully-connected control-channel
/// participant that holds no subnet-authority root cannot forge an
/// accepted subnet fact. Unsigned bytes, wrong-root descriptors,
/// gateway advertisements and export policies, malformed frames, and
/// a wrong-authority floor are all verified into inertness — no state
/// moves, no right appears, no context is lost, no handler runs, the
/// node stays healthy, and a correctly signed fact still works
/// afterwards.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hostile_control_publisher_is_inert_in_the_full_topology() {
    let (f, camera, publisher) = fleet_fixture_with_control("hostile").await;
    let camera_kp = EntityKeypair::from_bytes(CAMERA_SEED);
    let hostile_root = EntityKeypair::from_bytes([0xDD; 32]);

    // Full topology baseline: fleet call works, camera admitted.
    f.call(true).await.expect("baseline fleet call");
    try_admit_vb(
        &f.vehicle_b,
        &camera,
        &camera_kp,
        PERCEPTION,
        CAMERA,
        SubnetRights::ATTACH,
    )
    .await
    .expect("camera admitted");
    let calls_before = f.calls.load(Ordering::SeqCst);
    let epoch_before = f
        .vehicle_b
        .subnet_floor_registry()
        .auth_epoch(vb_subnet_root().entity_id());

    // The hostile publisher — a legitimate channel member — sends
    // every shape it can.
    let wrong_root_scope = SubnetRef {
        authority: hostile_root.entity_id().clone(),
        path: TopologySubnetId::new(PERCEPTION),
    };
    let mut malformed = SubnetControlFact::Descriptor(
        SubnetDescriptor::try_issue(&vb_subnet_root(), vb_ref(PERCEPTION), 0, 99, unix_now())
            .expect("descriptor"),
    )
    .to_bytes();
    malformed.truncate(malformed.len() / 2);
    let mut trailing = SubnetControlFact::Descriptor(
        SubnetDescriptor::try_issue(&vb_subnet_root(), vb_ref(PERCEPTION), 0, 98, unix_now())
            .expect("descriptor"),
    )
    .to_bytes();
    trailing.push(0);
    // A descriptor whose SIGNATURE is stripped: correct shape, no
    // authority behind it.
    let SubnetControlFact::Descriptor(mut unsigned) = SubnetControlFact::Descriptor(
        SubnetDescriptor::try_issue(&vb_subnet_root(), vb_ref(PERCEPTION), 0, 97, unix_now())
            .expect("descriptor"),
    ) else {
        unreachable!()
    };
    unsigned.signature = [0u8; 64];

    let hostile_payloads: Vec<Bytes> = vec![
        Bytes::from_static(b""),
        Bytes::from_static(b"not a control fact"),
        Bytes::from(vec![0xFFu8; 2048]),
        Bytes::from(malformed),
        Bytes::from(trailing),
        Bytes::from(SubnetControlFact::Descriptor(unsigned).to_bytes()),
        // Wrong-root, structurally perfect facts of every kind.
        Bytes::from(
            SubnetControlFact::Descriptor(
                SubnetDescriptor::try_issue(
                    &hostile_root,
                    wrong_root_scope.clone(),
                    0,
                    1,
                    unix_now(),
                )
                .expect("hostile descriptor"),
            )
            .to_bytes(),
        ),
        Bytes::from(
            SubnetControlFact::GatewayAdvertisement(
                GatewayAdvertisement::try_issue(
                    &hostile_root,
                    wrong_root_scope.clone(),
                    0,
                    publisher.entity_id().clone(),
                    publisher.node_id(),
                    1,
                    unix_now() - 60,
                    unix_now() + 3600,
                )
                .expect("hostile advertisement"),
            )
            .to_bytes(),
        ),
        Bytes::from(
            SubnetControlFact::ExportPolicy(
                SubnetExportPolicy::try_issue(
                    &hostile_root,
                    wrong_root_scope.clone(),
                    0,
                    vec![0xDEAD_BEEF],
                    1,
                    unix_now() - 60,
                    unix_now() + 3600,
                )
                .expect("hostile export policy"),
            )
            .to_bytes(),
        ),
        Bytes::from(
            SubnetControlFact::RevocationFloor(
                SubnetRevocationFloor::try_issue(
                    &hostile_root,
                    wrong_root_scope,
                    0,
                    999,
                    1,
                    unix_now(),
                )
                .expect("hostile floor"),
            )
            .to_bytes(),
        ),
    ];
    for payload in hostile_payloads {
        publisher
            .publish(&publisher_for(control_channel()), payload)
            .await
            .expect("publish hostile payload");
    }

    // A correctly signed fact ordered behind the barrage: waiting for
    // it proves every hostile frame has been processed.
    let good =
        SubnetDescriptor::try_issue(&vb_subnet_root(), vb_ref(WORLD_MODEL), 0, 7, unix_now())
            .expect("legitimate descriptor");
    publisher
        .publish(
            &publisher_for(control_channel()),
            Bytes::from(SubnetControlFact::Descriptor(good).to_bytes()),
        )
        .await
        .expect("publish the legitimate fact");
    assert!(
        wait_until(Duration::from_secs(5), || f
            .vehicle_b
            .subnet_control_store()
            .descriptor_for(
                vb_subnet_root().entity_id(),
                0,
                TopologySubnetId::new(WORLD_MODEL)
            )
            .is_some())
        .await,
        "evidence 17: a correctly signed fact is still accepted after the barrage",
    );

    // Nothing the hostile publisher sent moved any state.
    assert!(
        f.vehicle_b
            .subnet_control_store()
            .descriptor_for(
                hostile_root.entity_id(),
                0,
                TopologySubnetId::new(PERCEPTION)
            )
            .is_none(),
        "no hostile descriptor state",
    );
    assert!(
        f.vehicle_b
            .subnet_control_store()
            .gateway_for(
                hostile_root.entity_id(),
                0,
                TopologySubnetId::new(PERCEPTION),
                unix_now(),
                30
            )
            .is_none(),
        "no hostile gateway advertisement state",
    );
    assert!(
        f.vehicle_b
            .subnet_control_store()
            .export_policy_for(
                hostile_root.entity_id(),
                0,
                TopologySubnetId::new(PERCEPTION),
                unix_now(),
                30
            )
            .is_none(),
        "no hostile export-policy state",
    );
    assert!(
        f.vehicle_b
            .subnet_control_store()
            .descriptor_for(
                vb_subnet_root().entity_id(),
                0,
                TopologySubnetId::new(PERCEPTION)
            )
            .is_none(),
        "the unsigned/malformed descriptors named a real scope and still applied nothing",
    );
    assert_eq!(
        f.vehicle_b
            .subnet_floor_registry()
            .auth_epoch(vb_subnet_root().entity_id()),
        epoch_before,
        "no hostile floor moved the auth epoch",
    );
    assert_eq!(
        f.vehicle_b
            .subnet_floor_registry()
            .auth_epoch(hostile_root.entity_id()),
        0,
        "and the hostile authority has no epoch of its own here",
    );

    // No right appeared, no context was lost, the node is healthy.
    assert!(
        f.vehicle_b.subnet_context_for(camera.node_id()).is_some(),
        "evidence 17: no admitted context disappears",
    );
    assert!(
        f.vehicle_b
            .subnet_context_for(publisher.node_id())
            .is_none(),
        "evidence 17: the publisher gains no subnet presence",
    );
    assert_eq!(
        f.calls.load(Ordering::SeqCst),
        calls_before,
        "evidence 17: no handler ran on hostile input",
    );
    f.call(true)
        .await
        .expect("evidence 17: the node remains healthy and still serves the fleet");

    let _ = std::fs::remove_dir_all(&f.dir);
}

// ===========================================================================
// §6 Scenario H — an internal two-gateway route re-authenticates and
// re-tags at EVERY hop, and no forged locator field selects
// authority. Evidence 19, 20.
// ===========================================================================

const INNER_TAG: &[u8] = b"vehicle-b-inner-payload";

async fn wire() -> UdpSocket {
    UdpSocket::bind("127.0.0.1:0").await.expect("bind watcher")
}

/// Every datagram that is actually a route-hop envelope. A watcher
/// standing in for a peer's address also receives that peer's
/// ordinary traffic, so heartbeats must never be counted as
/// forwarding (§9).
fn route_hops(datagrams: &[Vec<u8>]) -> Vec<&Vec<u8>> {
    datagrams
        .iter()
        .filter(|d| d.len() >= 2 && u16::from_le_bytes([d[0], d[1]]) == ROUTE_HOP_MAGIC)
        .collect()
}

async fn received_within(sock: &UdpSocket, dur: Duration) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let deadline = tokio::time::Instant::now() + dur;
    let mut buf = vec![0u8; 2048];
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline - tokio::time::Instant::now();
        match tokio::time::timeout(remaining, sock.recv_from(&mut buf)).await {
            Ok(Ok((n, _))) => out.push(buf[..n].to_vec()),
            _ => break,
        }
    }
    out
}

/// Vehicle B's internal line topology: camera — gw1 — gw2 — world
/// model. Each node anchors Vehicle B's authority; each edge is
/// handshaked before any dispatch loop starts; both gateways hold
/// their OWN forwarding credentials at `gw2_rights` / ROUTE.
struct TwoGatewayFixture {
    source: Arc<MeshNode>,
    gw1: Arc<MeshNode>,
    gw2: Arc<MeshNode>,
    dest: Arc<MeshNode>,
    /// A peer with a live session to gw1 but deliberately NO admitted
    /// subnet context — the forged-locator control.
    outsider: Arc<MeshNode>,
}

async fn vb_node(seed: [u8; 32], attachment: &[u8]) -> Arc<MeshNode> {
    let mut cfg = base_config().with_subnet_authority(SubnetAuthorityConfig {
        authority: vb_subnet_root().entity_id().clone(),
        roots: vec![vb_subnet_root().entity_id().clone()],
        maximum_grant_lifetime_secs: 7 * DAY,
    });
    cfg.subnet_attachment = Some(TopologySubnetId::new(attachment));
    Arc::new(
        MeshNode::new(EntityKeypair::from_bytes(seed), cfg)
            .await
            .expect("MeshNode::new"),
    )
}

async fn two_gateway_fixture(gw2_rights: SubnetRights) -> TwoGatewayFixture {
    let (s_kp, g1_kp, g2_kp, d_kp) = (
        EntityKeypair::from_bytes([0xD1; 32]),
        EntityKeypair::from_bytes([0xD2; 32]),
        EntityKeypair::from_bytes([0xD3; 32]),
        EntityKeypair::from_bytes([0xD4; 32]),
    );
    let source = vb_node([0xD1; 32], CAMERA).await;
    let gw1 = vb_node([0xD2; 32], VEHICLE).await;
    let gw2 = vb_node([0xD3; 32], VEHICLE).await;
    let dest = vb_node([0xD4; 32], WORLD_MODEL).await;
    let outsider = vb_node([0xD9; 32], CAMERA).await;

    connect_no_start(&source, &gw1).await;
    connect_no_start(&gw2, &gw1).await;
    connect_no_start(&dest, &gw2).await;
    connect_no_start(&outsider, &gw1).await;
    source.start();
    gw1.start();
    gw2.start();
    dest.start();
    outsider.start();

    // EXACT admitted attachments at every adjacent edge (evidence 20).
    for (verifier, peer, kp, attach) in [
        (&gw1, &source, &s_kp, CAMERA),
        (&gw1, &gw2, &g2_kp, VEHICLE),
        (&gw2, &gw1, &g1_kp, VEHICLE),
        (&gw2, &dest, &d_kp, WORLD_MODEL),
    ] {
        try_admit_vb(verifier, peer, kp, VEHICLE, attach, SubnetRights::ATTACH)
            .await
            .expect("adjacent edge admitted at its exact attachment");
    }

    // Each gateway proves its OWN forwarding rights; neither inherits
    // the other's.
    gw1.install_subnet_gateway_credentials(&[vb_grant(
        &g1_kp,
        VEHICLE,
        SubnetRights::ATTACH.union(SubnetRights::ROUTE),
    )])
    .expect("gw1 credentials");
    gw2.install_subnet_gateway_credentials(&[vb_grant(
        &g2_kp,
        VEHICLE,
        SubnetRights::ATTACH.union(gw2_rights),
    )])
    .expect("gw2 credentials");
    for gw in [&gw1, &gw2] {
        gw.declare_subnet_boundaries(SubnetBoundarySet::new(
            vb_subnet_root().entity_id().clone(),
            0,
            [],
        ));
    }

    // Route learning through PRODUCTION propagation: gw1 must resolve
    // an identity-bound next hop toward dest.
    dest.announce_capabilities(CapabilitySet::new().add_tag("two-gateway-witness"))
        .await
        .expect("dest announce");
    gw2.announce_capabilities(CapabilitySet::new())
        .await
        .expect("gw2 announce");
    let dest_id = dest.node_id();
    let gw2_id = gw2.node_id();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(hop) = gw1.authenticated_next_hop(dest_id) {
            assert_eq!(
                hop.node_id, gw2_id,
                "the learned route must bind the ADJACENT authenticated peer",
            );
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "gw1 never learned an identity-bound route to dest through \
             production propagation",
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    TwoGatewayFixture {
        source,
        gw1,
        gw2,
        dest,
        outsider,
    }
}

/// Evidence 20: the inner packet crosses BOTH gateways byte for byte
/// while every hop is independently authenticated and re-tagged — the
/// final envelope verifies only under the gw2↔dest edge key, and the
/// hop budget moved exactly twice.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_two_gateway_route_reauthenticates_every_hop() {
    let f = two_gateway_fixture(SubnetRights::ROUTE).await;
    let dest_id = f.dest.node_id();

    // Observe only the last leg: gw2's egress toward dest.
    let watcher = wire().await;
    assert!(f
        .gw2
        .set_peer_addr_for_test(dest_id, watcher.local_addr().expect("addr")));

    let header = RoutingHeader::new(dest_id, f.source.node_id() as u32, 8);
    let envelope = f
        .source
        .seal_route_hop_to_peer(f.gw1.node_id(), &header, INNER_TAG)
        .expect("the source seals ONLY to its adjacent gateway");
    let sock = wire().await;
    sock.send_to(&envelope, f.gw1.local_addr())
        .await
        .expect("send");

    let got = received_within(&watcher, Duration::from_millis(1500)).await;
    let hops = route_hops(&got);
    assert_eq!(
        hops.len(),
        1,
        "exactly one hop reaches the destination side"
    );

    // The captured envelope is the SECOND relay's output: it verifies
    // under the gw2↔dest edge key, which gw1 does not hold.
    let (out_header, out_inner) = f
        .dest
        .open_route_hop_from_peer(f.gw2.node_id(), hops[0])
        .expect("the final hop verifies under the gw2↔dest edge key");
    assert_eq!(
        out_header.dest_id, dest_id,
        "the destination rides through BOTH relays unchanged",
    );
    assert_eq!(
        out_inner, INNER_TAG,
        "evidence 20: the inner packet is preserved byte for byte",
    );
    assert_eq!(
        out_header.ttl,
        header.ttl - 2,
        "outer TTL decrements exactly once per relay",
    );
    assert_eq!(
        out_header.hop_count,
        header.hop_count + 2,
        "outer hop_count increments exactly once per relay",
    );
    // No protected-to-legacy fallback: the only thing that reached the
    // destination side was a route-hop envelope.
    assert_eq!(
        got.iter()
            .filter(|d| d.len() >= 2 && u16::from_le_bytes([d[0], d[1]]) != ROUTE_HOP_MAGIC)
            .count(),
        got.len() - hops.len(),
        "no protected packet may degrade to the legacy path",
    );
}

/// Evidence 20 (inverse): the SECOND gateway's exact right is
/// load-bearing. Same topology, same learned route, same valid
/// envelope — but gw2 holds no ROUTE, so nothing reaches the
/// destination side even though gw1 forwarded correctly.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn removing_the_second_gateways_exact_right_stops_the_hop() {
    let f = two_gateway_fixture(SubnetRights::ATTACH).await;
    let dest_id = f.dest.node_id();

    let watcher = wire().await;
    assert!(f
        .gw2
        .set_peer_addr_for_test(dest_id, watcher.local_addr().expect("addr")));

    let header = RoutingHeader::new(dest_id, f.source.node_id() as u32, 8);
    let envelope = f
        .source
        .seal_route_hop_to_peer(f.gw1.node_id(), &header, INNER_TAG)
        .expect("seal to gw1");
    let sock = wire().await;
    sock.send_to(&envelope, f.gw1.local_addr())
        .await
        .expect("send");

    let got = received_within(&watcher, Duration::from_millis(800)).await;
    assert!(
        route_hops(&got).is_empty(),
        "without ROUTE at the second gateway no protected hop may reach \
         the destination side — the first relay's authority must not \
         carry the packet through the second",
    );
}

/// Evidence 19: forging the locator fields selects no authority. The
/// UDP source address and `RoutingHeader.src_id` are not identity —
/// ingress is resolved from the hop session alone — so an envelope
/// sealed by a peer with NO admitted context is refused however those
/// fields are dressed up, while the legitimate source's envelope is
/// forwarded from an arbitrary socket.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn forged_locator_fields_select_no_authority() {
    let f = two_gateway_fixture(SubnetRights::ROUTE).await;
    let dest_id = f.dest.node_id();

    // The fixture's outsider: a live session to gw1, NO admitted
    // subnet context.
    let outsider = f.outsider.clone();

    let watcher = wire().await;
    assert!(f
        .gw2
        .set_peer_addr_for_test(dest_id, watcher.local_addr().expect("addr")));

    // The outsider forges `src_id` to impersonate the admitted source.
    let forged = RoutingHeader::new(dest_id, f.source.node_id() as u32, 8);
    let envelope = outsider
        .seal_route_hop_to_peer(f.gw1.node_id(), &forged, INNER_TAG)
        .expect("the outsider has a session to gw1");
    // …and sends it from a THIRD, unrelated socket address.
    let sock = wire().await;
    sock.send_to(&envelope, f.gw1.local_addr())
        .await
        .expect("send");
    assert!(
        route_hops(&received_within(&watcher, Duration::from_millis(800)).await).is_empty(),
        "evidence 19: neither a forged RoutingHeader.src_id nor a forged \
         UDP source may select an ingress context",
    );

    // The legitimate source's envelope, sent from an equally arbitrary
    // socket, IS forwarded — proving the refusal above was about
    // admitted authority, not about the address it arrived from.
    let header = RoutingHeader::new(dest_id, f.source.node_id() as u32, 8);
    let good = f
        .source
        .seal_route_hop_to_peer(f.gw1.node_id(), &header, INNER_TAG)
        .expect("seal to gw1");
    let sock2 = wire().await;
    sock2
        .send_to(&good, f.gw1.local_addr())
        .await
        .expect("send");
    assert_eq!(
        route_hops(&received_within(&watcher, Duration::from_millis(1500)).await).len(),
        1,
        "the admitted source is forwarded regardless of source address",
    );
}
