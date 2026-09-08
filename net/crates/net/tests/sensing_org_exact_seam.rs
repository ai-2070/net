//! OA-5: the fixtures-only COMPOSITION witness for organization exact-provider
//! sensing.
//!
//! Every accepted seam, assembled once, on real nodes over real loopback UDP:
//!
//! 1. **authorized discovery** — a same-organization provider seals a real
//!    owner-scoped capability announcement to the consumer's own owner
//!    audience; the consumer verifies it through the ordinary ingest path, and
//!    the authorized population is that verified discovery intersected with the
//!    consumer's TOFU pins. Nothing hands in a candidate list;
//! 2. **exact org sensing transport** — retaining demand takes the production
//!    organization lease, which authors an `OrgProviderRegistration` under the
//!    canonical organization commitment and sends it from the node-owned
//!    ordered egress. The provider's own intake gate installs the row;
//! 3. **signed observations** — the provider's registered evaluator answers the
//!    admitted registration, and origin-signed attestations drive the
//!    consumer's continuity to `Established` and its projection to `Ready`;
//! 4. **request-relative projection and order** — the consumer captures ONE
//!    projection at one instant and turns it into a candidate order over its
//!    COMPLETE authorized list through the accepted core rule;
//! 5. **exact protected invocation with admission** — the consumer invokes the
//!    sensed-preferred provider over the protected nRPC path, and the provider
//!    admits it through real organization admission.
//!
//! # What this is NOT
//!
//! It is a SEAM composition, assembled by this fixture. It is not evidence that
//! any production caller consumes a sensed order: nothing in production calls
//! `project_sensed_order`, and this file names no `OrgClient` surface. The
//! bridge it uses is fixtures-only and guarded by
//! `tests/sensing_org_exact_guards.rs` plus the external fixtures-off probe.
//!
//! Run: `cargo nextest run --features "cortex tool fixtures" --test
//! sensing_org_exact_seam`
#![cfg(all(feature = "net", feature = "cortex", feature = "fixtures"))]

mod common;
use common::*;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;

use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::behavior::org::{OrgKeypair, OrgMembershipCert};
use net::adapter::net::behavior::org_admission::OrgAdmission;
use net::adapter::net::behavior::org_authority::NodeAuthority;
use net::adapter::net::behavior::org_grant::{
    CapabilityAuthorityId, DispatcherScope, OrgDispatcherGrant,
};
use net::adapter::net::behavior::org_scoped_ann::ScopedCapabilityAnnouncement;
use net::adapter::net::behavior::org_sensing_demand::{
    OrgSensingCapabilityDemand, OrgSensingFamily,
};
use net::adapter::net::behavior::sensing::{
    canonical_org_sensing_commitment, CapabilityId, ConsumerLatencyBudget, DownstreamId,
    EvaluationRequest, Incarnation, ProjectedReadiness, ProviderInterestKey, ReadinessEvaluation,
    ReadinessEvaluator, SensingCounters,
};
use net::adapter::net::cortex::{
    RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus,
};
use net::adapter::net::mesh_rpc::{CallOptions, OrgProofIntent, RpcError};
use net::adapter::net::org_exact_sensing_bridge::{authorized_population, sensed_provider_order};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};
use net::adapter::Adapter;

/// One tag for everything: the sensing capability id, the discovery
/// descriptor's tag, and - as `nrpc:<service>` - the invoked capability
/// authority. One string keeps the composition honest: a mismatch anywhere
/// would show up as an empty population, a silent projection or a denial.
const SERVICE: &str = "gpu.infer";
const TAG: &str = "nrpc:gpu.infer";
const SENSING_TTL: Duration = Duration::from_secs(30);
const SETTLE: Duration = Duration::from_secs(10);

fn org() -> OrgKeypair {
    OrgKeypair::from_bytes([0x42u8; 32])
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_secs()
}

/// A scratch directory that is deliberately NEVER deleted.
///
/// `OrgRevocationStore` keys its process-global core registry by the
/// revocation sidecar's `(device, inode)`. Removing the directory frees the
/// inode while the core is still registered, so a later store in the same test
/// binary can land on the recycled inode, derive the same backing id, and
/// inherit a dead test's floors, poison and generation. The name is salted with
/// nanoseconds because pids recycle too.
struct ScratchDir(PathBuf);

impl ScratchDir {
    fn new(tag: &str) -> Self {
        let salt = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "net-oa5-seam-{tag}-{}-{salt:x}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Self(dir)
    }
}

fn base_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, CHAOS_PSK)
        .with_heartbeat_interval(Duration::from_millis(100))
        .with_session_timeout(Duration::from_secs(10))
        .with_handshake(3, Duration::from_secs(2))
        .with_sensing_coalescing(true)
        .with_sensing_interest_ttl(SENSING_TTL);
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: CHAOS_BUFFER_SIZE,
        recv_buffer_size: CHAOS_BUFFER_SIZE,
    };
    cfg
}

async fn node_with(keypair: EntityKeypair, incarnation: Option<Incarnation>) -> Arc<MeshNode> {
    let mut cfg = base_config();
    if let Some(incarnation) = incarnation {
        cfg = cfg.with_sensing_incarnation(incarnation);
    }
    Arc::new(MeshNode::new(keypair, cfg).await.expect("MeshNode::new"))
}

fn adopt_and_install(node: &Arc<MeshNode>, tag: &str) -> ScratchDir {
    let dir = ScratchDir::new(tag);
    let cert = OrgMembershipCert::try_issue(&org(), node.entity_id().clone(), 1, 3600)
        .expect("issue membership cert");
    let authority =
        NodeAuthority::adopt(&dir.0, cert, node.entity_id(), 0, None).expect("adopt authority");
    node.install_node_authority(Arc::new(authority))
        .expect("install authority");
    dir
}

/// Seal a REAL owner-scoped announcement from `provider` to `consumer`'s own
/// owner audience, and ingest it through the ordinary verified path.
///
/// The provider signs with its genuine entity key and carries a genuine
/// membership certificate for the same organization; the envelope is AEAD-open
/// to the consumer's owner discovery key alone. This is the same function the
/// `SUBPROTOCOL_SCOPED_CAPABILITY_ANN` dispatch arm calls.
fn announce_owner_capability(consumer: &Arc<MeshNode>, provider: &Arc<MeshNode>) {
    let authority = consumer
        .node_authority()
        .expect("the consumer must be organization-authoritative");
    let cert = OrgMembershipCert::try_issue(&org(), provider.entity_id().clone(), 1, 3600)
        .expect("provider membership cert");
    let descriptor = CapabilitySet::new().add_tag(TAG).to_bytes_compact();
    let envelope = ScopedCapabilityAnnouncement::build_owner(
        provider.entity_keypair(),
        org().org_id(),
        cert,
        authority.audience.audience_handle,
        authority.audience.discovery_key(),
        1,
        now_secs() + 3600,
        &descriptor,
    )
    .expect("owner-scoped envelope");
    consumer.ingest_scoped_announcement_for_test(&envelope.to_bytes());
}

/// The provider's readiness answer. Real evaluator, real signed beats.
struct Evaluator {
    ready: bool,
    start: Duration,
}

impl ReadinessEvaluator for Evaluator {
    fn evaluate(&self, _request: &EvaluationRequest<'_>) -> ReadinessEvaluation {
        if self.ready {
            ReadinessEvaluation::Ready {
                estimated_start: Some(self.start),
            }
        } else {
            // A provider-defined detail code: diagnostics, never semantics.
            ReadinessEvaluation::NotReady { reason: 7 }
        }
    }
}

/// The protected handler: records that admission attribution arrived and that
/// the raw proof header never reached the handler view.
struct AdmitHandler {
    calls: Arc<AtomicU64>,
    admitted_caller: Arc<parking_lot::Mutex<Option<String>>>,
    header_stripped: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl RpcHandler for AdmitHandler {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(admitted) = ctx.org_admission.as_ref() {
            *self.admitted_caller.lock() = Some(format!("{:?}", admitted.caller));
        }
        self.header_stripped.store(
            !ctx.payload
                .headers
                .iter()
                .any(|(name, _)| name == "net-org-admission"),
            Ordering::SeqCst,
        );
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: Bytes::from_static(b"pong"),
        })
    }
}

/// The consumer's proof intent for ONE exact provider. Pure data: the org
/// issues the membership and the dispatcher grant, and the caller identity is
/// the consumer node's own entity, so the authenticated session peer is the
/// proof subject.
fn intent_for(caller: EntityKeypair, provider: &Arc<MeshNode>) -> OrgProofIntent {
    let caller_entity = caller.entity_id().clone();
    let capability = CapabilityAuthorityId::for_tag(TAG);
    let membership = OrgMembershipCert::try_issue(&org(), caller_entity.clone(), 1, 3600)
        .expect("caller membership");
    let dispatcher = OrgDispatcherGrant::try_issue(
        &org(),
        caller_entity,
        DispatcherScope::Exact(capability),
        3600,
    )
    .expect("dispatcher grant");
    OrgProofIntent {
        caller: Arc::new(caller),
        membership,
        dispatcher,
        capability_grant: None,
        acting_org: org().org_id(),
        provider_owner_org: org().org_id(),
        provider: provider.entity_id().clone(),
        capability,
        proof_ttl_secs: 30,
    }
}

/// The composed fixture: one consumer, one or two providers, real sessions,
/// real authority, real discovery, retained demand on the org plane.
struct Seam {
    consumer: Arc<MeshNode>,
    consumer_seed: [u8; 32],
    providers: Vec<Arc<MeshNode>>,
    demand: Arc<OrgSensingCapabilityDemand>,
    family: OrgSensingFamily,
    branches: Vec<(u64, ProviderInterestKey)>,
    _dirs: Vec<ScratchDir>,
}

/// Stand the whole thing up. `ready` says, per provider, whether its evaluator
/// answers Ready - so a witness can compose over real Ready AND real NotReady
/// evidence.
async fn compose(tag: &str, ready: &[bool]) -> Seam {
    let consumer_seed = [0x51u8; 32];
    let consumer = node_with(EntityKeypair::from_bytes(consumer_seed), None).await;
    let mut dirs = vec![adopt_and_install(&consumer, &format!("{tag}-consumer"))];

    let mut providers = Vec::new();
    for (index, ready) in ready.iter().enumerate() {
        let provider = node_with(EntityKeypair::generate(), Some(Incarnation::new(1))).await;
        dirs.push(adopt_and_install(
            &provider,
            &format!("{tag}-provider-{index}"),
        ));
        provider
            .register_readiness_evaluator(
                CapabilityId::new(TAG),
                Arc::new(Evaluator {
                    ready: *ready,
                    start: Duration::from_millis(3 + index as u64),
                }),
            )
            .expect("evaluator registers");
        connect_pair(&consumer, &provider).await;
        providers.push(provider);
    }

    consumer.start();
    for provider in &providers {
        provider.start();
    }
    consumer
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("consumer announce");
    for provider in &providers {
        provider
            .announce_capabilities(CapabilitySet::new())
            .await
            .expect("provider announce");
    }
    let consumer_id = consumer.node_id();
    let provider_ids: Vec<u64> = providers.iter().map(|p| p.node_id()).collect();
    {
        let consumer = Arc::clone(&consumer);
        let providers = providers.clone();
        let provider_ids = provider_ids.clone();
        await_condition(
            SETTLE,
            "entity pins established in both directions",
            move || {
                provider_ids
                    .iter()
                    .all(|id| consumer.peer_entity_id(*id).is_some())
                    && providers
                        .iter()
                        .all(|p| p.peer_entity_id(consumer_id).is_some())
            },
        )
        .await;
    }

    // AUTHORIZED DISCOVERY: each provider seals its own owner-scoped
    // announcement to the consumer's audience.
    for provider in &providers {
        announce_owner_capability(&consumer, provider);
    }
    let mut expected = provider_ids.clone();
    expected.sort_unstable();
    assert_eq!(
        authorized_population(&consumer, TAG),
        expected,
        "the population must come from VERIFIED owner-private discovery \
         intersected with the consumer's pins - nothing else authorizes it"
    );

    // RETAIN on the organization plane. `retain` derives its population from
    // that same discovery; the witness seam that takes an explicit list is
    // deliberately not used here.
    let family = OrgSensingFamily::mint(&consumer).expect("mint the sensing family");
    let demand = family.retain(TAG).expect("retain discovery-derived demand");
    assert_eq!(
        demand.population().to_vec(),
        expected,
        "the retained demand's population IS the authorized one"
    );
    let mut branches = demand.retained_branches_for_test();
    branches.sort_by_key(|(provider, _)| *provider);
    assert_eq!(
        branches.len(),
        providers.len(),
        "every authorized provider must be retained: {:?}",
        demand.retained_providers()
    );

    Seam {
        consumer,
        consumer_seed,
        providers,
        demand,
        family,
        branches,
        _dirs: dirs,
    }
}

impl Seam {
    /// Wait until the provider's intake installed the org row AND the
    /// consumer's projection reached `want` from real signed beats.
    async fn settle(&self, index: usize, want: ProjectedReadiness) {
        let provider = &self.providers[index];
        let consumer_id = self.consumer.node_id();
        let provider_id = provider.node_id();
        // Look the branch up BY PROVIDER: the retained branches are sorted by
        // provider id, which is a hash of the entity key and has nothing to do
        // with the order the providers were built in.
        let branch = self
            .branches
            .iter()
            .find(|(candidate, _)| *candidate == provider_id)
            .map(|(_, branch)| branch.clone())
            .unwrap_or_else(|| panic!("no retained branch for provider {provider_id:#x}"));
        let commitment = canonical_org_sensing_commitment(&org().org_id());

        {
            let provider = Arc::clone(provider);
            let branch = branch.clone();
            await_condition(
                SETTLE,
                "the provider's intake installed the org row",
                move || {
                    provider
                        .sensing_downstream_entry(&branch, DownstreamId::Peer(consumer_id))
                        .is_some()
                },
            )
            .await;
        }
        let row = provider
            .sensing_downstream_entry(&branch, DownstreamId::Peer(consumer_id))
            .expect("the provider's row for the consumer");
        assert_eq!(
            row.owner_root, commitment,
            "the row must be rooted at the canonical ORGANIZATION commitment - a \
             legacy entity root here would be an authority downgrade"
        );

        {
            let consumer = Arc::clone(&self.consumer);
            let branch = branch.clone();
            await_condition(
                SETTLE,
                "real signed observations drove the consumer's projection",
                move || consumer.sensing_projected(&branch) == want,
            )
            .await;
        }
        assert_eq!(
            self.consumer.sensing_projected(&branch),
            want,
            "the projection for provider {provider_id:#x}"
        );
        // The projection came from a real SIGNED observation over a live
        // stream, not from a default: the attestation is stored and this hop's
        // continuity toward the origin is established.
        assert!(
            self.consumer.sensing_latest_attestation(&branch).is_some(),
            "no signed attestation was stored for provider {provider_id:#x} - the \
             projection would then be an artefact, not evidence"
        );
        assert_eq!(
            self.consumer.sensing_upstream_continuity(&branch),
            Some(net::adapter::net::behavior::sensing::Continuity::Established),
            "continuity toward provider {provider_id:#x} must be established by a \
             live stream"
        );
        assert!(
            provider.sensing_origin_active(),
            "the provider must have opened a real origin emitter"
        );
    }

    fn intent(&self, index: usize) -> OrgProofIntent {
        intent_for(
            EntityKeypair::from_bytes(self.consumer_seed),
            &self.providers[index],
        )
    }

    async fn teardown(self) {
        self.family.retire(TAG);
        drop(self.demand);
        let _ = self.consumer.shutdown().await;
        for provider in &self.providers {
            let _ = provider.shutdown().await;
        }
    }
}

/// THE COMPOSITION: authorized discovery, exact org transport, signed
/// observations, one coherent projection and order, one admitted protected
/// invocation of the sensed-preferred provider.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn discovery_transport_observations_projection_and_admission_compose() {
    let seam = compose("compose", &[true]).await;
    seam.settle(0, ProjectedReadiness::Ready).await;
    let provider = &seam.providers[0];
    let provider_id = provider.node_id();

    // PROJECTION - one capture, at one instant, over the authorized population.
    let budget = ConsumerLatencyBudget::default();
    let projection = seam.demand.project_sensed_order(Instant::now(), &budget);
    assert_eq!(projection.rows().len(), 1, "{projection:?}");
    assert_eq!(projection.viable(), &[provider_id], "{projection:?}");
    assert_eq!(projection.preferred(), Some(provider_id));
    assert!(
        projection.rows()[0].estimated_start.is_some(),
        "the provider's SIGNED start estimate must have arrived with the beat: \
         {projection:?}"
    );

    // ORDER over the COMPLETE candidate list. The cross-organization candidate
    // is never sensed, so it keeps its place behind the sensed-viable one.
    let granted = provider_id.wrapping_add(0x5EED);
    let mut complete = vec![provider_id, granted];
    let mut same_org = vec![true, false];
    if complete[0] > complete[1] {
        complete.swap(0, 1);
        same_org.swap(0, 1);
    }
    let order = sensed_provider_order(&seam.demand, Instant::now(), &budget, &complete, &same_org);
    assert_eq!(
        order,
        vec![provider_id, granted],
        "the sensed-viable provider leads its own authorized list, and the \
         unsensed cross-organization candidate follows without being pruned"
    );

    // INVOCATION of the sensed-preferred provider, over the protected path.
    let calls = Arc::new(AtomicU64::new(0));
    let admitted_caller = Arc::new(parking_lot::Mutex::new(None));
    let header_stripped = Arc::new(AtomicBool::new(false));
    let _serve = provider
        .serve_rpc_protected(
            SERVICE,
            Arc::new(AdmitHandler {
                calls: Arc::clone(&calls),
                admitted_caller: Arc::clone(&admitted_caller),
                header_stripped: Arc::clone(&header_stripped),
            }),
            OrgAdmission::OwnerDelegated,
            Arc::new(|_| true),
        )
        .expect("serve the protected capability");

    let preferred = projection
        .preferred()
        .expect("the sensed order named a provider");
    assert_eq!(
        preferred, provider_id,
        "the call targets what sensing ranked"
    );
    let reply = seam
        .consumer
        .call(
            preferred,
            SERVICE,
            Bytes::from_static(b"ping"),
            CallOptions {
                org_proof_intent: Some(seam.intent(0)),
                deadline: Some(Instant::now() + Duration::from_secs(5)),
                ..Default::default()
            },
        )
        .await
        .expect("the admitted call returns Ok");

    assert_eq!(reply.body.as_ref(), b"pong");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the handler ran exactly once"
    );
    assert!(
        admitted_caller.lock().is_some(),
        "the handler must see ORGANIZATION ADMISSION attribution - the invocation \
         was authorized by admission, not by sensing"
    );
    assert!(
        header_stripped.load(Ordering::SeqCst),
        "the raw admission proof header must be stripped from the handler view"
    );

    // Sensing added nothing to authorization and nothing broke on the way.
    assert_eq!(
        authorized_population(&seam.consumer, TAG),
        seam.demand.population().to_vec(),
        "the population is still exactly what discovery authorized"
    );
    for node in [&seam.consumer, provider] {
        assert_eq!(
            SensingCounters::get(&node.sensing_counters().protocol_invalid),
            0
        );
        assert_eq!(
            SensingCounters::get(&node.sensing_counters().scope_refusals),
            0
        );
    }

    drop(_serve);
    seam.teardown().await;
}

/// An UNPROVEN call to the sensed-preferred provider is denied.
///
/// The control for the whole composition: sensing ranked this provider, the
/// order named it first, and the transport reaches it - and none of that
/// authorizes anything. Without an organization proof the provider's admission
/// gate refuses and the handler never runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unproven_call_to_the_sensed_provider_is_denied() {
    let seam = compose("unproven", &[true]).await;
    seam.settle(0, ProjectedReadiness::Ready).await;
    let provider = &seam.providers[0];
    let preferred = seam
        .demand
        .project_sensed_order(Instant::now(), &ConsumerLatencyBudget::default())
        .preferred()
        .expect("the sensed order named a provider");
    assert_eq!(preferred, provider.node_id());

    let calls = Arc::new(AtomicU64::new(0));
    let _serve = provider
        .serve_rpc_protected(
            SERVICE,
            Arc::new(AdmitHandler {
                calls: Arc::clone(&calls),
                admitted_caller: Arc::new(parking_lot::Mutex::new(None)),
                header_stripped: Arc::new(AtomicBool::new(false)),
            }),
            OrgAdmission::OwnerDelegated,
            Arc::new(|_| true),
        )
        .expect("serve the protected capability");

    let error = seam
        .consumer
        .call(
            preferred,
            SERVICE,
            Bytes::from_static(b"ping"),
            CallOptions {
                // No org_proof_intent: sensing is the ONLY thing recommending
                // this provider, and it recommends nothing about authority.
                deadline: Some(Instant::now() + Duration::from_secs(5)),
                ..Default::default()
            },
        )
        .await
        .expect_err("a sensed order is not an authorization");
    match error {
        RpcError::ServerError {
            status, message, ..
        } => {
            assert_eq!(status, 0x0009, "AdmissionDenied: {message:?}");
            assert_eq!(
                message.as_bytes(),
                &[0u8],
                "and the coarse reason is Denied - there was no proof at all"
            );
        }
        other => panic!("expected an AdmissionDenied server error, got {other:?}"),
    }
    for _ in 0..20 {
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "the handler ran for an unproven call"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    drop(_serve);
    seam.teardown().await;
}

/// Sensing is ADVISORY: neither plane lets an advisory capture authorize
/// anything, and the two refusals are distinguishable.
///
/// * REMOTE: the provider's own authority becomes unusable after the capture.
///   The advisory order still names it - it was viable when observed - and the
///   invocation is denied by the provider's admission gate, with the handler
///   never running;
/// * LOCAL: the consumer's authority goes away. The capture is unchanged plain
///   data, but the local planes refuse: the demand's authority view is no
///   longer current and the cold authority capture refuses outright, so no
///   invocation intent can be minted at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sensing_is_advisory_and_neither_plane_lets_it_authorize() {
    let seam = compose("advisory", &[true]).await;
    seam.settle(0, ProjectedReadiness::Ready).await;
    let provider = &seam.providers[0];
    let provider_id = provider.node_id();

    let budget = ConsumerLatencyBudget::default();
    let captured_at = Instant::now();
    let captured = seam.demand.project_sensed_order(captured_at, &budget);
    assert_eq!(captured.viable(), &[provider_id], "precondition: viable");
    // An INDEPENDENT copy of what the capture said, taken now. Comparing the
    // capture against a later FRESH projection would be wrong: a fresh
    // projection is evaluated at a fresh instant and is required to age.
    let recorded = (
        captured.rows().to_vec(),
        captured.viable().to_vec(),
        captured.potential().to_vec(),
        captured.non_viable().to_vec(),
    );

    let calls = Arc::new(AtomicU64::new(0));
    let _serve = provider
        .serve_rpc_protected(
            SERVICE,
            Arc::new(AdmitHandler {
                calls: Arc::clone(&calls),
                admitted_caller: Arc::new(parking_lot::Mutex::new(None)),
                header_stripped: Arc::new(AtomicBool::new(false)),
            }),
            OrgAdmission::OwnerDelegated,
            Arc::new(|_| true),
        )
        .expect("serve the protected capability");

    // ---- REMOTE refusal, after the capture -----------------------------
    provider
        .org_revocation_store()
        .expect("the provider has a revocation store")
        .mark_poisoned_for_test();

    let error = seam
        .consumer
        .call(
            provider_id,
            SERVICE,
            Bytes::from_static(b"ping"),
            CallOptions {
                org_proof_intent: Some(seam.intent(0)),
                deadline: Some(Instant::now() + Duration::from_secs(5)),
                ..Default::default()
            },
        )
        .await
        .expect_err("an advisory order cannot make an unusable authority admit");
    match error {
        RpcError::ServerError {
            status, message, ..
        } => {
            assert_eq!(
                status, 0x0009,
                "the provider REFUSED admission: {message:?}"
            );
            assert_eq!(
                message.as_bytes(),
                &[2u8],
                "and the coarse reason is Unavailable - the provider's authority, \
                 not the caller's credentials"
            );
        }
        other => panic!("expected an AdmissionDenied server error, got {other:?}"),
    }
    // The handler must stay dark; an instant read cannot tell "never ran" from
    // "has not run yet", so watch a window.
    for _ in 0..20 {
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "the handler ran for a denied call"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // THE CAPTURE ITSELF is unchanged: provider churn cannot reach into a value
    // that was already taken. This is deliberately NOT a comparison against a
    // later fresh projection - a fresh projection is evaluated at a fresh
    // instant and is REQUIRED to age out (proved positively below).
    assert_eq!(
        (
            captured.rows().to_vec(),
            captured.viable().to_vec(),
            captured.potential().to_vec(),
            captured.non_viable().to_vec()
        ),
        recorded,
        "a capture is a value, not a handle on live state"
    );

    // ---- LOCAL fencing -------------------------------------------------
    assert!(
        seam.demand.authority_is_current(),
        "precondition: the consumer's own authority view is still current"
    );
    seam.consumer.clear_node_authority_for_test();
    assert!(
        !seam.demand.authority_is_current(),
        "the retained demand must know its authority view is gone"
    );
    assert!(
        seam.consumer.org_cold_authority().is_err(),
        "and the invocation plane refuses to capture an authority at all, so no \
         intent can be minted from an advisory order"
    );
    // The capture STILL says what it said - losing local authority does not
    // rewrite a value either.
    assert_eq!(
        captured.viable().to_vec(),
        recorded.1,
        "the advisory capture is still just data, with or without authority"
    );

    // ---- AND AGING IS CORRECT, not something this witness papers over ----
    // A FRESH projection is evaluated at the instant it is given, so evidence
    // that has outlived its window must read Unknown. Proved by asking for a
    // far-future instant rather than by waiting: no sleep, no TTL tuning, no
    // retry. A witness that instead demanded a fresh projection keep matching
    // an old capture would fail the moment real expiry arrived - which is
    // exactly the bug this replaces.
    let aged = seam
        .demand
        .project_sensed_order(captured_at + SENSING_TTL * 4, &budget);
    assert_eq!(
        aged.rows().len(),
        captured.rows().len(),
        "the population is an immutable input, so aging changes verdicts, not \
         membership: {aged:?}"
    );
    assert!(
        aged.rows()
            .iter()
            .all(|row| row.readiness == ProjectedReadiness::Unknown),
        "evidence past its window reads Unknown at the instant it is evaluated \
         against: {aged:?}"
    );
    assert!(
        aged.viable().is_empty() && aged.non_viable().is_empty(),
        "and aged-out evidence neither ranks nor prunes: {aged:?}"
    );
    assert_eq!(
        aged.potential(),
        &[provider_id],
        "it is held as potential - absence of fresh evidence is not evidence of \
         absence: {aged:?}"
    );

    drop(_serve);
    seam.teardown().await;
}

/// REAL sensed evidence orders the complete candidate list: a provider that
/// answers Ready leads, an unsensed cross-organization candidate keeps its
/// place, and a provider that answers NOT READY is ordered last rather than
/// removed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_sensed_evidence_orders_the_complete_candidate_list() {
    let seam = compose("order", &[true, false]).await;
    seam.settle(0, ProjectedReadiness::Ready).await;
    seam.settle(1, ProjectedReadiness::NotReady).await;
    let ready_id = seam.providers[0].node_id();
    let not_ready_id = seam.providers[1].node_id();

    let budget = ConsumerLatencyBudget::default();
    let projection = seam.demand.project_sensed_order(Instant::now(), &budget);
    assert_eq!(projection.rows().len(), 2, "{projection:?}");
    assert_eq!(projection.viable(), &[ready_id], "{projection:?}");
    assert_eq!(projection.non_viable(), &[not_ready_id], "{projection:?}");
    assert!(projection.potential().is_empty(), "{projection:?}");

    // The complete authorized list, in the caller's own deterministic order,
    // with one cross-organization candidate that sensing never sees.
    let granted = ready_id ^ not_ready_id ^ 0xA11CE;
    let mut complete = vec![ready_id, not_ready_id, granted];
    let mut same_org = vec![true, true, false];
    // Sort as the caller's own plan would, keeping the parallel views aligned.
    let mut paired: Vec<(u64, bool)> = complete.drain(..).zip(same_org.drain(..)).collect();
    paired.sort_unstable();
    let complete: Vec<u64> = paired.iter().map(|(provider, _)| *provider).collect();
    let same_org: Vec<bool> = paired.iter().map(|(_, owned)| *owned).collect();

    let order = sensed_provider_order(&seam.demand, Instant::now(), &budget, &complete, &same_org);
    assert_eq!(
        order.len(),
        complete.len(),
        "the order is a permutation of the complete list: {order:?}"
    );
    let mut sorted = order.clone();
    sorted.sort_unstable();
    let mut expected = complete.clone();
    expected.sort_unstable();
    assert_eq!(sorted, expected, "nothing invented, nothing dropped");
    assert_eq!(
        order[0], ready_id,
        "the sensed-viable provider leads: {order:?}"
    );
    assert_eq!(
        order[1], granted,
        "the unsensed cross-organization candidate keeps its place and is never \
         pruned: {order:?}"
    );
    assert_eq!(
        order[2], not_ready_id,
        "and a provider sensed NOT READY is ordered last, not removed: {order:?}"
    );

    seam.teardown().await;
}
