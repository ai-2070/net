//! S4B of `docs/internal/plans/SUBNET_AUTH_PLAN.md` — the four
//! authority planes do not substitute for one another.
//!
//! ```text
//! Organization = horizontal federation      (who may act for whom)
//! Subnet       = vertical topology/transport (what may cross where)
//! Channel      = propagation and pub/sub     (what may be seen)
//! Provider     = effect admission            (what may actually run)
//! ```
//!
//! An exported cross-organization effect needs a conjunction:
//!
//! ```text
//! exact admitted ingress attachment
//! + exact authenticated egress attachment
//! + gateway self-held subnet EXPORT over the actual crossing
//! + bounded organization dispatcher/invocation authority
//! + provider-local admission
//! → allow
//! ```
//!
//! Removing any one term denies, and no term can be manufactured from
//! another. That is the whole content of this file.
//!
//! # What this is, and is not
//!
//! This is a focused cross-plane composition contract that calls the
//! **production** subnet and org/provider gate functions directly:
//! `VerifiedGatewayContextSet::authorize_transition`,
//! `verify_provider_authority`, and `verify_org_admission` with a real
//! `provider_policy` closure.
//!
//! It is **not** the planned live multi-node E2E witness. Nothing here
//! drives a datagram through a relay or a call through nRPC dispatch;
//! the conjunction below is asserted by composing the same gates the
//! protected path composes, in the same order, not by observing a
//! packet arrive. The E2E suite is where all four planes are driven
//! through an actual multi-node call.
//!
//! The channel plane is deliberately NOT represented here by a boolean.
//! Its own gates are pinned directly by the `Visibility::Exported`
//! truth table and the wire-hash collision inverses in `mesh.rs` and
//! `gateway.rs`. What this file adds for that plane is only the
//! cross-plane non-substitution assertions.
//!
//! # Why the protected path's real gate order matters
//!
//! Production runs `has_local_capability → verify_provider_authority →
//! verify_org_admission → provider_policy (last) → handler`.
//! `may_admit` is the legacy public capability-fold allow-list and is
//! deliberately absent: composing it here would exercise a path
//! production does not use.

#![cfg(feature = "net")]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::admission_clock::ClockSample;
use net::adapter::net::behavior::org::{OrgKeypair, OrgMembershipCert};
use net::adapter::net::behavior::org_admission::{
    verify_org_admission, AdmissionContext, AdmissionDenied, OrgAdmission,
};
use net::adapter::net::behavior::org_admission_replay::{
    AdmissionReplayConfig, AdmissionReplayGuard,
};
use net::adapter::net::behavior::org_authority::NodeAuthority;
use net::adapter::net::behavior::org_call::OrgCallProof;
use net::adapter::net::behavior::org_grant::{
    CapabilityAuthorityId, DispatcherScope, GrantRights, GrantTargetScope, OrgCapabilityGrant,
    OrgDispatcherGrant,
};
use net::adapter::net::identity::{EntityId, EntityKeypair};
use net::adapter::net::org_admission_gate::verify_provider_authority;
use net::adapter::net::subnet::{
    admission::unix_now_secs, auth::compile_gateway_context, build_gateway_context_set,
    ForwardDenial, SubnetAuthorityConfig, SubnetBoundarySet, SubnetCredentialSet,
    SubnetFloorRegistry, SubnetGrant, SubnetRights, TopologySubnetId, VerifiedGatewayContext,
    VerifiedGatewayContextSet, VerifiedSubnetContext,
};
use net::adapter::net::{MeshNode, MeshNodeConfig, SocketBufferConfig};

// A scratch directory holding an authority's revocation `.lock` sidecar is
// deliberately LEFT BEHIND when its test finishes.
//
// `OrgRevocationStore` keys its PROCESS-GLOBAL core registry by that sidecar's
// `(device, inode)`, so two path aliases of one sidecar share one live view
// (AV-9). Deleting the directory frees the inode while this test's core is
// still registered; Linux recycles a freed inode immediately, so the next store
// opened anywhere in this binary can land on it, derive the same `BackingId`,
// and join THIS test's core — inheriting its floors, its poison bit and its
// generation, and writing through a path that no longer exists
// (`state lock: No such file or directory`).
//
// The victims are whichever tests are scheduled next, so it surfaces as
// unrelated failures in varying combinations rather than as one deterministic
// break. Start-of-test resets stay: they run before anything is registered.

const PSK: [u8; 32] = [0x5Au8; 32];

/// Mirrors the protected-path node config used by
/// integration_nrpc_protected.rs.
fn test_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().expect("addr");
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(3, Duration::from_secs(2));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: TEST_BUFFER_SIZE,
        recv_buffer_size: TEST_BUFFER_SIZE,
    };
    cfg
}
const DAY: u64 = 24 * 60 * 60;
const TEST_BUFFER_SIZE: usize = 256 * 1024;

// The vehicle-internal topology under BMW's subnet authority.
//
//   3        vehicle root
//   3.7      perception domain
//   3.7.1    world-model (a declared protected boundary)
//   3.8      the chassis domain, outside world-model
const VEHICLE: &[u8] = &[3];
const WORLD_MODEL: &[u8] = &[3, 7, 1];
const OUTSIDE: &[u8] = &[3, 8];

// ---------------------------------------------------------------------------
// Identities
// ---------------------------------------------------------------------------

/// BMW: the organization that owns Vehicle B, and the acting org for
/// the same-org baseline.
fn bmw() -> OrgKeypair {
    OrgKeypair::from_bytes([0xB1u8; 32])
}

/// An unrelated partner organization, for the cross-org rows.
fn partner() -> OrgKeypair {
    OrgKeypair::from_bytes([0xB2u8; 32])
}

/// A third org that is neither the provider's owner nor the acting org.
fn stranger() -> OrgKeypair {
    OrgKeypair::from_bytes([0xB3u8; 32])
}

/// The Vehicle A caller entity (the acting dispatcher subject).
fn caller() -> EntityKeypair {
    EntityKeypair::from_bytes([0xA1u8; 32])
}

/// The subnet authority root — BMW's topology authority. Distinct key
/// material from the org keys on purpose: they are different planes.
fn subnet_root() -> EntityKeypair {
    EntityKeypair::from_bytes([0x51u8; 32])
}

/// A second subnet authority with identical path bits, for the
/// "equal compact path under wrong authority" row.
fn other_subnet_root() -> EntityKeypair {
    EntityKeypair::from_bytes([0x52u8; 32])
}

fn perception_roi() -> CapabilityAuthorityId {
    CapabilityAuthorityId::for_tag("nrpc:perception.roi")
}

fn other_capability() -> CapabilityAuthorityId {
    CapabilityAuthorityId::for_tag("nrpc:chassis.brake")
}

// ---------------------------------------------------------------------------
// Subnet plane fixture
// ---------------------------------------------------------------------------

fn subnet_config(root: &EntityKeypair) -> SubnetAuthorityConfig {
    SubnetAuthorityConfig {
        authority: root.entity_id().clone(),
        roots: vec![root.entity_id().clone()],
        maximum_grant_lifetime_secs: 7 * DAY,
    }
}

fn subnet_grant(
    root: &EntityKeypair,
    subject: &EntityKeypair,
    scope: &[u8],
    rights: SubnetRights,
) -> SubnetCredentialSet {
    SubnetCredentialSet::Direct(
        SubnetGrant::try_issue(
            root,
            root.entity_id().clone(),
            TopologySubnetId::new(scope),
            0,
            subject.entity_id().clone(),
            rights,
            1,
            unix_now_secs() - 60,
            DAY,
        )
        .expect("issue subnet grant"),
    )
}

/// One compiled gateway entry for the Vehicle B gateway node.
fn gateway_entry(
    root: &EntityKeypair,
    local: &EntityKeypair,
    scope: &[u8],
    rights: SubnetRights,
) -> VerifiedGatewayContext {
    compile_gateway_context(
        &subnet_grant(root, local, scope, rights),
        local.entity_id(),
        TopologySubnetId::new(scope),
        &subnet_config(root),
        0,
        &SubnetFloorRegistry::new(),
        unix_now_secs(),
        60,
    )
    .expect("compile gateway entry")
}

fn gateway_set(
    root: &EntityKeypair,
    entries: Vec<VerifiedGatewayContext>,
) -> VerifiedGatewayContextSet {
    build_gateway_context_set(root.entity_id(), entries).expect("build gateway set")
}

/// An admitted peer context at an exact attachment.
fn peer_at(
    root: &EntityKeypair,
    subject: &EntityKeypair,
    attachment: &[u8],
    scope: &[u8],
) -> VerifiedSubnetContext {
    VerifiedSubnetContext {
        authority: root.entity_id().clone(),
        attachment: TopologySubnetId::new(attachment),
        scope: TopologySubnetId::new(scope),
        topology_epoch: 0,
        subject: subject.entity_id().clone(),
        subject_node: subject.entity_id().node_id(),
        session_id: 1,
        rights: SubnetRights::ATTACH,
        generation: 1,
        subnet_auth_epoch: 0,
        expires_at: unix_now_secs() + DAY,
        credential_set_hash: [0; 32],
    }
}

fn world_model_boundary(root: &EntityKeypair) -> SubnetBoundarySet {
    SubnetBoundarySet::new(
        root.entity_id().clone(),
        0,
        [TopologySubnetId::new(WORLD_MODEL)],
    )
}

/// The subnet plane's verdict for the exported crossing: a peer inside
/// world-model talking to a peer outside it.
fn subnet_verdict(
    set: &VerifiedGatewayContextSet,
    boundaries: &SubnetBoundarySet,
    ingress: &VerifiedSubnetContext,
    egress: &VerifiedSubnetContext,
) -> Result<(), ForwardDenial> {
    set.authorize_transition(ingress, egress, boundaries, 0, 0, unix_now_secs())
}

// ---------------------------------------------------------------------------
// Provider / org plane fixture
// ---------------------------------------------------------------------------

/// Vehicle B: a real `MeshNode` with an installed `NodeAuthority`
/// owned by BMW, so `verify_provider_authority` reads genuine
/// installed state rather than a hand-populated struct.
struct VehicleB {
    mesh: Arc<MeshNode>,
    entity: EntityKeypair,
    _dir: std::path::PathBuf,
}

async fn vehicle_b(tag: &str) -> VehicleB {
    let entity = EntityKeypair::from_bytes([0xB0u8; 32]);
    let mesh = Arc::new(
        MeshNode::new(entity.clone(), test_config())
            .await
            .expect("mesh node"),
    );

    let node_cert = OrgMembershipCert::try_issue(&bmw(), entity.entity_id().clone(), 1, 3600)
        .expect("provider owner cert");
    let dir = std::env::temp_dir().join(format!(
        "net-subnet-org-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id(),
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let authority = NodeAuthority::adopt(&dir, node_cert, entity.entity_id(), 0, None)
        .expect("adopt node authority");
    mesh.install_node_authority(Arc::new(authority))
        .expect("install node authority");

    VehicleB {
        mesh,
        entity,
        _dir: dir,
    }
}

impl Drop for VehicleB {
    fn drop(&mut self) {}
}

/// Everything the caller side needs to mint one admission proof.
struct CallerCredentials {
    membership: OrgMembershipCert,
    dispatcher: OrgDispatcherGrant,
    capability_grant: Option<OrgCapabilityGrant>,
    acting_org: net::adapter::net::behavior::org::OrgId,
}

/// Same-org baseline: BMW membership plus an exact BMW dispatcher
/// grant for `perception.roi`. No capability grant — `OwnerDelegated`
/// confers none, and carrying one is itself a denial.
fn owner_delegated_credentials(capability: CapabilityAuthorityId) -> CallerCredentials {
    let org = bmw();
    CallerCredentials {
        membership: OrgMembershipCert::try_issue(&org, caller().entity_id().clone(), 1, 3600)
            .expect("membership"),
        dispatcher: OrgDispatcherGrant::try_issue(
            &org,
            caller().entity_id().clone(),
            DispatcherScope::Exact(capability),
            3600,
        )
        .expect("dispatcher grant"),
        capability_grant: None,
        acting_org: org.org_id(),
    }
}

/// Cross-org: Partner membership and dispatcher grant, plus a BMW →
/// Partner capability grant naming the exact provider.
fn cross_org_credentials(
    issuer: &OrgKeypair,
    grantee_org: net::adapter::net::behavior::org::OrgId,
    capability: CapabilityAuthorityId,
    grant_capability: CapabilityAuthorityId,
    rights: GrantRights,
    target: GrantTargetScope,
) -> CallerCredentials {
    let acting = partner();
    let (grant, _) =
        OrgCapabilityGrant::try_issue(issuer, grantee_org, grant_capability, rights, target, 3600)
            .expect("capability grant");
    CallerCredentials {
        membership: OrgMembershipCert::try_issue(&acting, caller().entity_id().clone(), 1, 3600)
            .expect("membership"),
        dispatcher: OrgDispatcherGrant::try_issue(
            &acting,
            caller().entity_id().clone(),
            DispatcherScope::Exact(capability),
            3600,
        )
        .expect("dispatcher grant"),
        capability_grant: Some(grant),
        acting_org: acting.org_id(),
    }
}

/// A monotonically increasing call id. The real org gate commits
/// replay state before running provider policy, so every row needs a
/// fresh one or later rows would deny as replays for the wrong reason.
fn next_call_id() -> u64 {
    static NEXT: AtomicUsize = AtomicUsize::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed) as u64
}

/// Outcome of running the real provider + org gates for one row.
struct OrgOutcome {
    result: Result<net::adapter::net::behavior::org_admission::Admitted, AdmissionDenied>,
    policy_calls: usize,
}

/// Run the protected path's gate order for one call:
/// `verify_provider_authority` then `verify_org_admission` with
/// `provider_policy` last.
#[expect(
    clippy::too_many_arguments,
    reason = "each parameter is a distinct plane input the matrix varies independently; \
              bundling them would hide which row changed what"
)]
fn run_org_gates(
    node: &VehicleB,
    creds: &CallerCredentials,
    mode: OrgAdmission,
    invoked_capability: CapabilityAuthorityId,
    proof_capability: CapabilityAuthorityId,
    provider_org_in_proof: net::adapter::net::behavior::org::OrgId,
    provider_in_proof: EntityId,
    policy_accepts: bool,
    omit_header: bool,
) -> OrgOutcome {
    let clock = ClockSample::now();
    let facts = match verify_provider_authority(&node.mesh, &clock) {
        Ok(facts) => facts,
        Err(denied) => {
            return OrgOutcome {
                result: Err(denied),
                policy_calls: 0,
            }
        }
    };

    let caller_entity = caller().entity_id().clone();
    let call_id = next_call_id();
    let request_digest = [0x7Au8; 32];
    // Well inside MAX_ORG_PROOF_TTL_SECS (30 s): a proof valid too far
    // out is refused as a standing replayable credential.
    let expiry_ns = clock.wall_ns + 10_000_000_000;
    let proof = OrgCallProof::sign_for_call(
        &caller(),
        creds.membership.clone(),
        creds.dispatcher.clone(),
        creds.capability_grant.clone(),
        creds.acting_org,
        provider_org_in_proof,
        provider_in_proof,
        call_id,
        proof_capability,
        expiry_ns,
        request_digest,
    );
    let header = proof.encode().expect("encode proof");
    let headers: Vec<&[u8]> = if omit_header { vec![] } else { vec![&header] };

    let ctx = AdmissionContext {
        mode,
        authenticated_caller: &caller_entity,
        provider: &facts.provider,
        provider_owner_org: facts.provider_owner_org,
        invoked_capability,
        call_id,
        request_digest,
        is_unary: true,
        floors: &facts.floors,
        skew_secs: facts.skew_secs,
    };

    let replay =
        AdmissionReplayGuard::try_new(AdmissionReplayConfig::default()).expect("replay guard");
    let policy_calls = std::cell::Cell::new(0usize);
    let result = verify_org_admission(
        &ctx,
        &headers,
        &replay,
        clock,
        || true,
        |_| {
            policy_calls.set(policy_calls.get() + 1);
            policy_accepts
        },
    );

    OrgOutcome {
        result,
        policy_calls: policy_calls.get(),
    }
}

/// The same-org baseline row, parameterized only by what the caller
/// wants to vary.
fn baseline_org(node: &VehicleB) -> OrgOutcome {
    let creds = owner_delegated_credentials(perception_roi());
    run_org_gates(
        node,
        &creds,
        OrgAdmission::OwnerDelegated,
        perception_roi(),
        perception_roi(),
        bmw().org_id(),
        node.entity.entity_id().clone(),
        true,
        false,
    )
}

// ---------------------------------------------------------------------------
// Positive baseline
// ---------------------------------------------------------------------------

/// The full conjunction admits, and each plane's gate is genuinely
/// consulted.
#[tokio::test]
async fn the_full_conjunction_admits_an_exported_cross_boundary_effect() {
    let node = vehicle_b("baseline").await;
    let root = subnet_root();

    // Subnet plane: Vehicle B's gateway holds EXPORT at exactly the
    // crossed world-model boundary.
    let set = gateway_set(
        &root,
        vec![
            gateway_entry(&root, &node.entity, VEHICLE, SubnetRights::ROUTE),
            gateway_entry(&root, &node.entity, WORLD_MODEL, SubnetRights::EXPORT),
        ],
    );
    let boundaries = world_model_boundary(&root);
    let inside = peer_at(&root, &caller(), WORLD_MODEL, VEHICLE);
    let outside = peer_at(&root, &node.entity, OUTSIDE, VEHICLE);

    subnet_verdict(&set, &boundaries, &inside, &outside)
        .expect("EXPORT at the crossed boundary authorizes the transition");

    // Org + provider planes.
    let outcome = baseline_org(&node);
    let admitted = outcome.result.expect("the full org proof admits");
    assert_eq!(&admitted.caller, caller().entity_id());
    assert_eq!(admitted.acting_org, bmw().org_id());
    assert_eq!(admitted.provider_org, bmw().org_id());
    assert_eq!(&admitted.provider, node.entity.entity_id());
    assert_eq!(admitted.capability, perception_roi());
    assert_eq!(
        outcome.policy_calls, 1,
        "the provider-local policy is the final veto and runs exactly once",
    );
}

// ---------------------------------------------------------------------------
// Independent removals (subnet plane)
// ---------------------------------------------------------------------------

/// Removing the gateway's EXPORT denies on the subnet plane while the
/// org proof is untouched — the planes fail independently.
#[tokio::test]
async fn removing_gateway_export_denies_transport_while_org_proof_still_admits() {
    let node = vehicle_b("no-export").await;
    let root = subnet_root();
    let boundaries = world_model_boundary(&root);
    let inside = peer_at(&root, &caller(), WORLD_MODEL, VEHICLE);
    let outside = peer_at(&root, &node.entity, OUTSIDE, VEHICLE);

    // ROUTE over the whole vehicle, but nothing at the boundary.
    let no_export = gateway_set(
        &root,
        vec![gateway_entry(
            &root,
            &node.entity,
            VEHICLE,
            SubnetRights::ROUTE,
        )],
    );
    assert_eq!(
        subnet_verdict(&no_export, &boundaries, &inside, &outside).unwrap_err(),
        ForwardDenial::ExportMissing,
        "a broad ROUTE must not carry traffic out through a declared boundary",
    );

    // The org plane is entirely unaffected.
    let outcome = baseline_org(&node);
    assert!(
        outcome.result.is_ok(),
        "org authority does not depend on subnet authority: {:?}",
        outcome.result.err(),
    );
}

/// ROUTE at exactly the crossed boundary is still not EXPORT.
#[tokio::test]
async fn route_at_the_boundary_scope_is_not_export() {
    let node = vehicle_b("route-not-export").await;
    let root = subnet_root();
    let boundaries = world_model_boundary(&root);
    let inside = peer_at(&root, &caller(), WORLD_MODEL, VEHICLE);
    let outside = peer_at(&root, &node.entity, OUTSIDE, VEHICLE);

    let routed = gateway_set(
        &root,
        vec![gateway_entry(
            &root,
            &node.entity,
            WORLD_MODEL,
            SubnetRights::ROUTE,
        )],
    );
    assert_eq!(
        subnet_verdict(&routed, &boundaries, &inside, &outside).unwrap_err(),
        ForwardDenial::ExportMissing,
    );

    let outcome = baseline_org(&node);
    assert!(outcome.result.is_ok(), "org plane unchanged");
}

/// EXPORT is not ROUTE either: a wholly internal transition needs
/// ROUTE, and holding EXPORT at the attachment does not supply it.
#[tokio::test]
async fn export_does_not_authorize_an_internal_transition() {
    let node = vehicle_b("export-not-route").await;
    let root = subnet_root();
    // No boundary is crossed: both peers sit inside world-model.
    let boundaries = world_model_boundary(&root);
    let a = peer_at(&root, &caller(), WORLD_MODEL, VEHICLE);
    let b = peer_at(&root, &node.entity, WORLD_MODEL, VEHICLE);

    let export_only = gateway_set(
        &root,
        vec![gateway_entry(
            &root,
            &node.entity,
            WORLD_MODEL,
            SubnetRights::EXPORT,
        )],
    );
    assert_eq!(
        subnet_verdict(&export_only, &boundaries, &a, &b).unwrap_err(),
        ForwardDenial::RouteMissing,
        "EXPORT must never substitute for ROUTE on an internal transition",
    );
}

/// Equal compact path bits under a different subnet authority are a
/// different place. The org credentials are untouched and still valid.
#[tokio::test]
async fn equal_path_under_the_wrong_subnet_authority_denies() {
    let node = vehicle_b("wrong-authority").await;
    let root = subnet_root();
    let other = other_subnet_root();

    let set = gateway_set(
        &root,
        vec![
            gateway_entry(&root, &node.entity, VEHICLE, SubnetRights::ROUTE),
            gateway_entry(&root, &node.entity, WORLD_MODEL, SubnetRights::EXPORT),
        ],
    );
    let boundaries = world_model_boundary(&root);

    // Same path bits, different authority.
    let foreign_peer = peer_at(&other, &caller(), WORLD_MODEL, VEHICLE);
    let ours = peer_at(&root, &node.entity, OUTSIDE, VEHICLE);
    assert_eq!(
        subnet_verdict(&set, &boundaries, &foreign_peer, &ours).unwrap_err(),
        ForwardDenial::ContextNotCurrent,
        "identical path bits under another authority are unrelated",
    );

    let outcome = baseline_org(&node);
    assert!(
        outcome.result.is_ok(),
        "the org credentials never mentioned a subnet authority",
    );
}

// ---------------------------------------------------------------------------
// Independent removals (org / provider planes)
// ---------------------------------------------------------------------------

/// A dispatcher grant scoped to another capability denies, and the
/// provider-local policy must never run — a denied call reaches no
/// application code.
#[tokio::test]
async fn dispatcher_scoped_to_another_capability_denies_before_provider_policy() {
    let node = vehicle_b("wrong-cap").await;
    let root = subnet_root();

    // Subnet plane is fully satisfied.
    let set = gateway_set(
        &root,
        vec![gateway_entry(
            &root,
            &node.entity,
            WORLD_MODEL,
            SubnetRights::EXPORT,
        )],
    );
    let boundaries = world_model_boundary(&root);
    let inside = peer_at(&root, &caller(), WORLD_MODEL, VEHICLE);
    let outside = peer_at(&root, &node.entity, OUTSIDE, VEHICLE);
    subnet_verdict(&set, &boundaries, &inside, &outside).expect("subnet plane satisfied");

    // Dispatcher grant empowers a different capability than the one
    // invoked.
    let creds = owner_delegated_credentials(other_capability());
    let outcome = run_org_gates(
        &node,
        &creds,
        OrgAdmission::OwnerDelegated,
        perception_roi(),
        perception_roi(),
        bmw().org_id(),
        node.entity.entity_id().clone(),
        true,
        false,
    );
    assert_eq!(
        outcome.result.unwrap_err(),
        AdmissionDenied::DispatcherGrantScope,
    );
    assert_eq!(
        outcome.policy_calls, 0,
        "provider-local policy must not run for a call the org gate denied",
    );
}

/// Membership alone, with no admission proof on the call, denies —
/// and again never reaches provider policy.
#[tokio::test]
async fn membership_without_a_call_proof_denies_before_provider_policy() {
    let node = vehicle_b("no-header").await;
    let creds = owner_delegated_credentials(perception_roi());
    let outcome = run_org_gates(
        &node,
        &creds,
        OrgAdmission::OwnerDelegated,
        perception_roi(),
        perception_roi(),
        bmw().org_id(),
        node.entity.entity_id().clone(),
        true,
        true,
    );
    assert_eq!(outcome.result.unwrap_err(), AdmissionDenied::MissingHeader);
    assert_eq!(outcome.policy_calls, 0);
}

/// A provider with no installed authority cannot admit anything, and
/// the subnet plane is unchanged by that fact.
#[tokio::test]
async fn provider_without_installed_authority_cannot_admit() {
    // A node deliberately built WITHOUT `install_node_authority`.
    let entity = EntityKeypair::from_bytes([0xB9u8; 32]);
    let mesh = MeshNode::new(entity.clone(), test_config())
        .await
        .expect("mesh node");

    let clock = ClockSample::now();
    assert!(
        matches!(
            verify_provider_authority(&mesh, &clock),
            Err(AdmissionDenied::ProviderAuthorityUnavailable),
        ),
        "registration-time authority is not usable authority",
    );

    // The subnet plane is untouched: a gateway's transport authority
    // does not depend on the provider's org authority.
    let root = subnet_root();
    let set = gateway_set(
        &root,
        vec![gateway_entry(
            &root,
            &entity,
            WORLD_MODEL,
            SubnetRights::EXPORT,
        )],
    );
    let boundaries = world_model_boundary(&root);
    let inside = peer_at(&root, &caller(), WORLD_MODEL, VEHICLE);
    let outside = peer_at(&root, &entity, OUTSIDE, VEHICLE);
    subnet_verdict(&set, &boundaries, &inside, &outside)
        .expect("subnet authority stands on its own");
}

/// Subnet and org both satisfied, provider-local policy refuses: the
/// effect does not happen, and the veto ran exactly once.
#[tokio::test]
async fn provider_local_veto_denies_a_fully_proven_call() {
    let node = vehicle_b("veto").await;
    let root = subnet_root();

    let set = gateway_set(
        &root,
        vec![gateway_entry(
            &root,
            &node.entity,
            WORLD_MODEL,
            SubnetRights::EXPORT,
        )],
    );
    let boundaries = world_model_boundary(&root);
    let inside = peer_at(&root, &caller(), WORLD_MODEL, VEHICLE);
    let outside = peer_at(&root, &node.entity, OUTSIDE, VEHICLE);
    subnet_verdict(&set, &boundaries, &inside, &outside).expect("subnet plane satisfied");

    let creds = owner_delegated_credentials(perception_roi());
    let outcome = run_org_gates(
        &node,
        &creds,
        OrgAdmission::OwnerDelegated,
        perception_roi(),
        perception_roi(),
        bmw().org_id(),
        node.entity.entity_id().clone(),
        false,
        false,
    );
    assert_eq!(
        outcome.result.unwrap_err(),
        AdmissionDenied::ProviderPolicyRejected,
    );
    assert_eq!(
        outcome.policy_calls, 1,
        "the veto is consulted exactly once, and it is the last word",
    );
}

// ---------------------------------------------------------------------------
// Non-substitution across planes
// ---------------------------------------------------------------------------

/// Organization authority creates no subnet attachment.
///
/// A valid BMW membership and dispatcher grant say who may act for
/// BMW. They contain no topology coordinate, are signed by a different
/// authority, and cannot produce a `VerifiedSubnetContext` — which is
/// why the subnet gate below has nothing to evaluate.
#[tokio::test]
async fn organization_authority_creates_no_subnet_attachment() {
    let node = vehicle_b("org-not-subnet").await;
    let root = subnet_root();

    // The caller holds full BMW org authority.
    let outcome = baseline_org(&node);
    assert!(outcome.result.is_ok(), "org authority is genuinely valid");

    // The gateway holds no subnet credential naming this caller, so
    // there is no attachment for it — and a gateway with no entries
    // authorizes nothing regardless of who is asking.
    let empty = gateway_set(&root, vec![]);
    let boundaries = world_model_boundary(&root);
    let inside = peer_at(&root, &caller(), WORLD_MODEL, VEHICLE);
    let outside = peer_at(&root, &node.entity, OUTSIDE, VEHICLE);
    assert_eq!(
        subnet_verdict(&empty, &boundaries, &inside, &outside).unwrap_err(),
        ForwardDenial::ExportMissing,
        "org authority cannot supply the gateway's transport rights",
    );

    // And the subnet authority's roots never include the org key.
    let cfg = subnet_config(&root);
    assert!(
        !cfg.roots.contains(&EntityId::from_bytes(bmw().org_id().0)),
        "the org identity is not a subnet authority root",
    );
}

/// Subnet authority creates no organization authority.
///
/// A valid Vehicle B subnet grant carries `ATTACH`/`ROUTE`/`EXPORT`
/// over a topology path. It names no org, empowers no dispatcher, and
/// cannot be presented as an admission proof.
#[tokio::test]
async fn subnet_authority_creates_no_organization_authority() {
    let node = vehicle_b("subnet-not-org").await;
    let root = subnet_root();

    // A genuinely valid subnet transport authority.
    let set = gateway_set(
        &root,
        vec![gateway_entry(
            &root,
            &node.entity,
            WORLD_MODEL,
            SubnetRights::EXPORT,
        )],
    );
    let boundaries = world_model_boundary(&root);
    let inside = peer_at(&root, &caller(), WORLD_MODEL, VEHICLE);
    let outside = peer_at(&root, &node.entity, OUTSIDE, VEHICLE);
    subnet_verdict(&set, &boundaries, &inside, &outside).expect("transport authority is valid");

    // The org gate refuses the call: transport rights are not an
    // admission proof, and no header can be derived from them.
    let creds = owner_delegated_credentials(perception_roi());
    let outcome = run_org_gates(
        &node,
        &creds,
        OrgAdmission::OwnerDelegated,
        perception_roi(),
        perception_roi(),
        bmw().org_id(),
        node.entity.entity_id().clone(),
        true,
        true,
    );
    assert_eq!(
        outcome.result.unwrap_err(),
        AdmissionDenied::MissingHeader,
        "a subnet grant cannot stand in for an org admission proof",
    );
}

// ---------------------------------------------------------------------------
// Cross-organization extension
// ---------------------------------------------------------------------------

/// The Partner diagnostic client: Partner membership + Partner
/// dispatcher grant + a BMW → Partner capability grant naming the
/// exact provider, over a subnet crossing the gateway may EXPORT.
#[tokio::test]
async fn cross_org_partner_diagnostic_admits_with_every_term_present() {
    let node = vehicle_b("cross-ok").await;
    let root = subnet_root();

    let set = gateway_set(
        &root,
        vec![gateway_entry(
            &root,
            &node.entity,
            WORLD_MODEL,
            SubnetRights::EXPORT,
        )],
    );
    let boundaries = world_model_boundary(&root);
    let inside = peer_at(&root, &caller(), WORLD_MODEL, VEHICLE);
    let outside = peer_at(&root, &node.entity, OUTSIDE, VEHICLE);
    subnet_verdict(&set, &boundaries, &inside, &outside).expect("subnet plane satisfied");

    let creds = cross_org_credentials(
        &bmw(),
        partner().org_id(),
        perception_roi(),
        perception_roi(),
        GrantRights::INVOKE,
        GrantTargetScope::ExactNode(node.entity.entity_id().clone()),
    );
    let outcome = run_org_gates(
        &node,
        &creds,
        OrgAdmission::CrossOrgGranted,
        perception_roi(),
        perception_roi(),
        bmw().org_id(),
        node.entity.entity_id().clone(),
        true,
        false,
    );
    let admitted = outcome.result.expect("cross-org call admits");
    assert_eq!(admitted.acting_org, partner().org_id());
    assert_eq!(admitted.provider_org, bmw().org_id());
    assert_eq!(outcome.policy_calls, 1);
}

/// Every independent removal from the cross-org row, each with its own
/// distinguishable reason. A single "denied" verdict would not prove
/// the gate checked the thing the row removed.
#[tokio::test]
async fn cross_org_inverses_each_deny_for_their_own_reason() {
    let node = vehicle_b("cross-inverse").await;
    let provider_entity = node.entity.entity_id().clone();
    let elsewhere = EntityKeypair::from_bytes([0xEEu8; 32]);

    // (a) No capability grant at all.
    let mut creds = cross_org_credentials(
        &bmw(),
        partner().org_id(),
        perception_roi(),
        perception_roi(),
        GrantRights::INVOKE,
        GrantTargetScope::ExactNode(provider_entity.clone()),
    );
    creds.capability_grant = None;
    assert_eq!(
        run_org_gates(
            &node,
            &creds,
            OrgAdmission::CrossOrgGranted,
            perception_roi(),
            perception_roi(),
            bmw().org_id(),
            provider_entity.clone(),
            true,
            false,
        )
        .result
        .unwrap_err(),
        AdmissionDenied::MissingCapabilityGrant,
    );

    // (b) Grant signed by an org that is not the provider's owner.
    let creds = cross_org_credentials(
        &stranger(),
        partner().org_id(),
        perception_roi(),
        perception_roi(),
        GrantRights::INVOKE,
        GrantTargetScope::ExactNode(provider_entity.clone()),
    );
    assert_eq!(
        run_org_gates(
            &node,
            &creds,
            OrgAdmission::CrossOrgGranted,
            perception_roi(),
            perception_roi(),
            bmw().org_id(),
            provider_entity.clone(),
            true,
            false,
        )
        .result
        .unwrap_err(),
        AdmissionDenied::ForeignIssuer,
    );

    // (c) Grant issued to a different org than the caller acts for.
    let creds = cross_org_credentials(
        &bmw(),
        stranger().org_id(),
        perception_roi(),
        perception_roi(),
        GrantRights::INVOKE,
        GrantTargetScope::ExactNode(provider_entity.clone()),
    );
    assert_eq!(
        run_org_gates(
            &node,
            &creds,
            OrgAdmission::CrossOrgGranted,
            perception_roi(),
            perception_roi(),
            bmw().org_id(),
            provider_entity.clone(),
            true,
            false,
        )
        .result
        .unwrap_err(),
        AdmissionDenied::GranteeMismatch,
    );

    // (d) Grant without INVOKE.
    let creds = cross_org_credentials(
        &bmw(),
        partner().org_id(),
        perception_roi(),
        perception_roi(),
        GrantRights::DISCOVER,
        GrantTargetScope::ExactNode(provider_entity.clone()),
    );
    assert_eq!(
        run_org_gates(
            &node,
            &creds,
            OrgAdmission::CrossOrgGranted,
            perception_roi(),
            perception_roi(),
            bmw().org_id(),
            provider_entity.clone(),
            true,
            false,
        )
        .result
        .unwrap_err(),
        AdmissionDenied::InsufficientRights,
    );

    // (e) Grant for a different capability than the one invoked.
    let creds = cross_org_credentials(
        &bmw(),
        partner().org_id(),
        perception_roi(),
        other_capability(),
        GrantRights::INVOKE,
        GrantTargetScope::ExactNode(provider_entity.clone()),
    );
    assert_eq!(
        run_org_gates(
            &node,
            &creds,
            OrgAdmission::CrossOrgGranted,
            perception_roi(),
            perception_roi(),
            bmw().org_id(),
            provider_entity.clone(),
            true,
            false,
        )
        .result
        .unwrap_err(),
        AdmissionDenied::CapabilityMismatch,
    );

    // (f) Grant whose target scope names a different provider.
    let creds = cross_org_credentials(
        &bmw(),
        partner().org_id(),
        perception_roi(),
        perception_roi(),
        GrantRights::INVOKE,
        GrantTargetScope::ExactNode(elsewhere.entity_id().clone()),
    );
    assert_eq!(
        run_org_gates(
            &node,
            &creds,
            OrgAdmission::CrossOrgGranted,
            perception_roi(),
            perception_roi(),
            bmw().org_id(),
            provider_entity.clone(),
            true,
            false,
        )
        .result
        .unwrap_err(),
        AdmissionDenied::TargetNotCovered,
    );

    // (g) Everything valid, provider vetoes.
    let creds = cross_org_credentials(
        &bmw(),
        partner().org_id(),
        perception_roi(),
        perception_roi(),
        GrantRights::INVOKE,
        GrantTargetScope::ExactNode(provider_entity.clone()),
    );
    let vetoed = run_org_gates(
        &node,
        &creds,
        OrgAdmission::CrossOrgGranted,
        perception_roi(),
        perception_roi(),
        bmw().org_id(),
        provider_entity,
        false,
        false,
    );
    assert_eq!(
        vetoed.result.unwrap_err(),
        AdmissionDenied::ProviderPolicyRejected,
    );
    assert_eq!(vetoed.policy_calls, 1);
}

/// A complete and valid cross-org proof still crosses nothing if the
/// gateway cannot EXPORT it. The org plane's success is not transport.
#[tokio::test]
async fn a_valid_cross_org_proof_does_not_open_a_subnet_boundary() {
    let node = vehicle_b("cross-no-export").await;
    let root = subnet_root();

    let creds = cross_org_credentials(
        &bmw(),
        partner().org_id(),
        perception_roi(),
        perception_roi(),
        GrantRights::INVOKE,
        GrantTargetScope::ExactNode(node.entity.entity_id().clone()),
    );
    let outcome = run_org_gates(
        &node,
        &creds,
        OrgAdmission::CrossOrgGranted,
        perception_roi(),
        perception_roi(),
        bmw().org_id(),
        node.entity.entity_id().clone(),
        true,
        false,
    );
    assert!(outcome.result.is_ok(), "the org proof is complete");

    // The gateway holds ROUTE but no EXPORT at the crossed boundary.
    let no_export = gateway_set(
        &root,
        vec![gateway_entry(
            &root,
            &node.entity,
            VEHICLE,
            SubnetRights::ROUTE,
        )],
    );
    let boundaries = world_model_boundary(&root);
    let inside = peer_at(&root, &caller(), WORLD_MODEL, VEHICLE);
    let outside = peer_at(&root, &node.entity, OUTSIDE, VEHICLE);
    assert_eq!(
        subnet_verdict(&no_export, &boundaries, &inside, &outside).unwrap_err(),
        ForwardDenial::ExportMissing,
        "a cross-org invocation proof is not a transport credential",
    );
}
