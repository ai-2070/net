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
    CapabilityAuthorityId, DispatcherScope, GrantRights, GrantTargetScope, OrgAudienceSecret,
    OrgCapabilityGrant, OrgDispatcherGrant,
};
use net::adapter::net::behavior::org_scoped_ann::ScopedCapabilityAnnouncement;
use net::adapter::net::behavior::org_sensing_demand::{
    OrgSensingCapabilityDemand, OrgSensingFamily,
};
use net::adapter::net::behavior::sensing;
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
    /// How many invocation intents the composed preparation has CONSTRUCTED.
    /// A fenced preparation must leave this untouched: "no intent" is the
    /// property, and a counter is how a witness sees it rather than inferring
    /// it from a refused call.
    intents: Arc<AtomicU64>,
    _dirs: Vec<ScratchDir>,
}

/// Why a composed preparation refused, BEFORE any intent existed.
#[derive(Debug, PartialEq, Eq)]
enum PrepareRefused {
    /// No local organization authority could be captured at all.
    NoAuthority,
    /// The captured authority moved between the capture and the mint
    /// boundary - the local fence, distinct from any remote refusal.
    Superseded,
    /// The order named no same-organization candidate.
    NoCandidate,
    /// The chosen provider has no PINNED ENTITY on this node, so nothing could
    /// bind a proof to it. This is the narrow pin-binding check and nothing
    /// wider: it is not the SDK's direct-session requirement, which lives with
    /// the candidate builder and is not reachable from core.
    Unpinned(u64),
}

/// One prepared invocation: the provider the ORDER chose and the intent minted
/// for exactly that provider.
#[derive(Debug)]
struct Prepared {
    provider: u64,
    intent: OrgProofIntent,
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
        intents: Arc::new(AtomicU64::new(0)),
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

    /// The COMPOSED preparation: capture authority, let the real candidate
    /// order choose the provider, and mint an intent for exactly that provider
    /// - with the final currentness decision immediately before the mint.
    ///
    /// This orchestrates existing seams and invents no authority algorithm of
    /// its own: `org_cold_authority` captures, `sensed_provider_order` orders
    /// through the accepted core rule, `peer_entity_id` answers directness, and
    /// `org_cold_authority_is_current` is the fence. What the fixture supplies
    /// is the ORDER of those steps, which is the thing under test: nothing may
    /// be minted after the fence says the view moved.
    ///
    /// `between` runs after the capture and before the fence, so a witness can
    /// move the authority in exactly that window.
    fn prepare_with(
        &self,
        complete: &[u64],
        same_org: &[bool],
        budget: &sensing::ConsumerLatencyBudget,
        between: impl FnOnce(),
    ) -> Result<Prepared, PrepareRefused> {
        // 1. CAPTURE the authority this preparation will be judged against.
        let authority = self
            .consumer
            .org_cold_authority()
            .map_err(|_| PrepareRefused::NoAuthority)?;

        // 2. The real candidate order chooses. Sensing only reorders what
        //    authority already admitted.
        let order = sensed_provider_order(&self.demand, Instant::now(), budget, complete, same_org);
        let chosen = order.iter().copied().find(|provider| {
            complete
                .iter()
                .position(|candidate| candidate == provider)
                .is_some_and(|index| same_org[index])
        });
        let Some(provider_id) = chosen else {
            return Err(PrepareRefused::NoCandidate);
        };

        // 3. PIN BINDING is annotated, never a sensing verdict. This is the
        //    narrow core check - "is this peer's entity known here" - and not
        //    the SDK's wider direct-session requirement.
        if self.consumer.peer_entity_id(provider_id).is_none() {
            return Err(PrepareRefused::Unpinned(provider_id));
        }

        between();

        // 4. THE FENCE, immediately before the mint. Nothing below this line
        //    may run against a view that has already moved.
        if !self.consumer.org_cold_authority_is_current(&authority) {
            return Err(PrepareRefused::Superseded);
        }

        // 5. Only now does an intent exist.
        let provider = self
            .providers
            .iter()
            .find(|provider| provider.node_id() == provider_id)
            .unwrap_or_else(|| panic!("the order named an unknown provider {provider_id:#x}"));
        self.intents.fetch_add(1, Ordering::SeqCst);
        Ok(Prepared {
            provider: provider_id,
            intent: intent_for(EntityKeypair::from_bytes(self.consumer_seed), provider),
        })
    }

    fn prepare(
        &self,
        complete: &[u64],
        same_org: &[bool],
        budget: &sensing::ConsumerLatencyBudget,
    ) -> Result<Prepared, PrepareRefused> {
        self.prepare_with(complete, same_org, budget, || {})
    }

    /// Invoke exactly what the preparation chose.
    async fn invoke(&self, prepared: Prepared) -> Result<Bytes, RpcError> {
        self.consumer
            .call(
                prepared.provider,
                SERVICE,
                Bytes::from_static(b"ping"),
                CallOptions {
                    org_proof_intent: Some(prepared.intent),
                    deadline: Some(Instant::now() + Duration::from_secs(5)),
                    ..Default::default()
                },
            )
            .await
            .map(|reply| reply.body)
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
/// observations, one coherent projection, the candidate ORDER that follows from
/// it, and one admitted protected invocation of exactly the provider that order
/// chose.
///
/// The order is DISCRIMINATING: two real providers, and the one sensing ranks
/// first is the one the caller's own list ranks LAST. A composition that
/// computed an order and then invoked by its own criterion would call the other
/// provider, and the other provider's handler is watched for exactly that.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn discovery_transport_observations_projection_and_admission_compose() {
    let seam = compose("compose", &[true, true]).await;
    seam.settle(0, ProjectedReadiness::Ready).await;
    seam.settle(1, ProjectedReadiness::Ready).await;

    // The caller's own list is id-sorted; make sensing prefer the LAST of it.
    let mut ids: Vec<u64> = seam.providers.iter().map(|p| p.node_id()).collect();
    ids.sort_unstable();
    let (behind, ahead) = (ids[0], ids[1]);
    for provider in &seam.providers {
        let start = if provider.node_id() == ahead {
            Duration::from_millis(1)
        } else {
            Duration::from_millis(500)
        };
        provider
            .replace_readiness_evaluator(
                CapabilityId::new(TAG),
                Arc::new(Evaluator { ready: true, start }),
            )
            .expect("evaluator replaces");
        provider.notify_sensing_state_changed(&CapabilityId::new(TAG));
    }
    let budget = ConsumerLatencyBudget::default();
    {
        let demand = Arc::clone(&seam.demand);
        await_condition(
            SETTLE,
            "sensed economics rank the higher-id provider first",
            move || {
                demand
                    .project_sensed_order(Instant::now(), &budget)
                    .viable()
                    == [ahead, behind]
            },
        )
        .await;
    }

    // Both providers serve the SAME capability; only the chosen one may run.
    let mut handlers = Vec::new();
    let mut serves = Vec::new();
    for provider in &seam.providers {
        let calls = Arc::new(AtomicU64::new(0));
        let admitted = Arc::new(parking_lot::Mutex::new(None));
        let stripped = Arc::new(AtomicBool::new(false));
        serves.push(
            provider
                .serve_rpc_protected(
                    SERVICE,
                    Arc::new(AdmitHandler {
                        calls: Arc::clone(&calls),
                        admitted_caller: Arc::clone(&admitted),
                        header_stripped: Arc::clone(&stripped),
                    }),
                    OrgAdmission::OwnerDelegated,
                    Arc::new(|_| true),
                )
                .expect("serve the protected capability"),
        );
        handlers.push((provider.node_id(), calls, admitted, stripped));
    }

    // THE COMPOSED PREPARATION: the order chooses, and the intent is minted for
    // exactly what it chose.
    let complete = ids.clone();
    let same_org = vec![true, true];
    let prepared = seam
        .prepare(&complete, &same_org, &budget)
        .expect("a current authority and a sensed candidate");
    assert_eq!(
        prepared.provider, ahead,
        "the SENSED order chose, not the caller's own id order ({complete:?})"
    );
    assert_eq!(
        seam.intents.load(Ordering::SeqCst),
        1,
        "exactly one intent was minted, for the chosen provider"
    );

    let body = seam
        .invoke(prepared)
        .await
        .expect("the admitted call returns");
    assert_eq!(body.as_ref(), b"pong");

    for (provider_id, calls, admitted, stripped) in &handlers {
        if *provider_id == ahead {
            assert_eq!(
                calls.load(Ordering::SeqCst),
                1,
                "the sensed-first provider handled the call exactly once"
            );
            assert!(
                admitted.lock().is_some(),
                "and it saw ORGANIZATION ADMISSION attribution - authorization \
                 came from admission, never from sensing"
            );
            assert!(
                stripped.load(Ordering::SeqCst),
                "the raw admission proof header must be stripped"
            );
        } else {
            assert_eq!(
                calls.load(Ordering::SeqCst),
                0,
                "the provider the caller's own order would have picked stayed \
                 dark - the invocation really followed the sensed order"
            );
        }
    }

    // Sensing added nothing to authorization and nothing broke on the way.
    let mut population = seam.demand.population().to_vec();
    population.sort_unstable();
    assert_eq!(
        authorized_population(&seam.consumer, TAG),
        population,
        "the population is still exactly what discovery authorized"
    );
    for node in std::iter::once(&seam.consumer).chain(seam.providers.iter()) {
        assert_eq!(
            SensingCounters::get(&node.sensing_counters().protocol_invalid),
            0
        );
        assert_eq!(
            SensingCounters::get(&node.sensing_counters().scope_refusals),
            0
        );
    }

    drop(serves);
    seam.teardown().await;
}

/// A STALE local preparation mints no intent and sends nothing.
///
/// The authority moves in the window between the capture and the mint
/// boundary - the one place the fence exists to cover. The preparation must
/// refuse there, with the intent counter untouched, and the provider's handler
/// must never see a call. This is the LOCAL fence; the remote refusal against a
/// provider whose own authority is unusable is a separate, later boundary
/// (`sensing_is_advisory_and_neither_plane_lets_it_authorize`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stale_local_preparation_mints_no_intent_and_sends_nothing() {
    let seam = compose("stale", &[true]).await;
    seam.settle(0, ProjectedReadiness::Ready).await;
    let provider = &seam.providers[0];
    let provider_id = provider.node_id();
    let budget = ConsumerLatencyBudget::default();

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

    // Baseline: the same preparation succeeds while the view is current, so the
    // refusal below is the MOVEMENT, not a broken fixture.
    let prepared = seam
        .prepare(&[provider_id], &[true], &budget)
        .expect("a current view prepares");
    assert_eq!(prepared.provider, provider_id);
    assert_eq!(seam.intents.load(Ordering::SeqCst), 1);
    drop(prepared);

    // Now move the authority INSIDE the preparation, after its capture.
    let consumer = Arc::clone(&seam.consumer);
    let refused = seam
        .prepare_with(&[provider_id], &[true], &budget, move || {
            let dir = ScratchDir::new("stale-rotation");
            let cert = OrgMembershipCert::try_issue(&org(), consumer.entity_id().clone(), 1, 3600)
                .expect("re-issue the membership cert");
            let authority = NodeAuthority::adopt(&dir.0, cert, consumer.entity_id(), 0, None)
                .expect("adopt a replacement authority");
            consumer.clear_node_authority_for_test();
            consumer
                .install_node_authority(Arc::new(authority))
                .expect("install the replacement");
            // Leak the directory deliberately: see `ScratchDir`.
            std::mem::forget(dir);
        })
        .expect_err("a moved authority must fence the preparation");
    assert_eq!(
        refused,
        PrepareRefused::Superseded,
        "the LOCAL fence refused - not a remote admission verdict"
    );
    assert_eq!(
        seam.intents.load(Ordering::SeqCst),
        1,
        "the fenced preparation minted NO intent: the counter is still the one \
         from the baseline"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "and nothing was sent - the handler never saw a call"
    );

    // With no authority at all, the preparation refuses even earlier.
    seam.consumer.clear_node_authority_for_test();
    assert_eq!(
        seam.prepare(&[provider_id], &[true], &budget)
            .expect_err("no authority, no preparation"),
        PrepareRefused::NoAuthority
    );
    assert_eq!(
        seam.intents.load(Ordering::SeqCst),
        1,
        "still no new intent"
    );
    assert!(
        !seam.demand.authority_is_current(),
        "and the retained demand knows its own view is gone"
    );

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

/// An ALL-PRUNED order falls back to the caller's own order, and prunes nobody.
///
/// Both real providers answer NOT READY, so nothing is viable. The order must
/// then be exactly the caller's own list - a sensed verdict deprioritizes, it
/// never removes - and the composed preparation still chooses, so a
/// fully-pessimistic sensing plane cannot strand a request.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_all_pruned_order_falls_back_to_the_callers_own_order() {
    let seam = compose("pruned", &[false, false]).await;
    seam.settle(0, ProjectedReadiness::NotReady).await;
    seam.settle(1, ProjectedReadiness::NotReady).await;

    let mut complete: Vec<u64> = seam.providers.iter().map(|p| p.node_id()).collect();
    complete.sort_unstable();
    let same_org = vec![true, true];
    let budget = ConsumerLatencyBudget::default();

    let projection = seam.demand.project_sensed_order(Instant::now(), &budget);
    assert!(projection.viable().is_empty(), "{projection:?}");
    assert_eq!(projection.non_viable().len(), 2, "{projection:?}");

    let order = sensed_provider_order(&seam.demand, Instant::now(), &budget, &complete, &same_org);
    assert_eq!(
        order, complete,
        "with nothing viable the order is the caller's own, and every candidate \
         is still in it"
    );
    let prepared = seam
        .prepare(&complete, &same_org, &budget)
        .expect("an all-pruned sensing plane must not strand the request");
    assert_eq!(
        prepared.provider, complete[0],
        "the caller's own first candidate is chosen"
    );

    seam.teardown().await;
}

/// A GRANTED-plane provider is never sensed, and a provider visible on BOTH
/// discovery planes is sensed exactly once - through the owner plane.
///
/// Both announcements are real: a cross-organization DISCOVER grant is
/// installed on the consumer, and the providers seal granted envelopes to that
/// grant's audience, verified through the ordinary ingest path.
///
/// SCOPE, stated rather than smuggled: the owner-first CLASSIFICATION of a
/// dual-plane provider (`push_unique` producing one `Mode::SameOrg` candidate)
/// lives in the SDK's candidate builder, which this slice may not edit and
/// cannot reach - the core crate has no candidate type and no dependency on the
/// SDK. What IS core, and is witnessed here, is the sensing side of the same
/// rule: the sensed population is derived from the owner plane alone, so a
/// granted-only provider never enters it and a dual-plane provider enters it
/// once, never twice.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_granted_only_provider_is_never_sensed_and_a_dual_plane_provider_is_sensed_once() {
    let seam = compose("granted", &[true]).await;
    seam.settle(0, ProjectedReadiness::Ready).await;
    let owner_provider = seam.providers[0].node_id();

    // A REAL cross-organization DISCOVER grant, installed on the consumer.
    let other_org = OrgKeypair::from_bytes([0x77u8; 32]);
    let capability = CapabilityAuthorityId::for_tag(TAG);
    let (grant, secret) = OrgCapabilityGrant::try_issue(
        &other_org,
        org().org_id(),
        capability,
        GrantRights::DISCOVER,
        GrantTargetScope::AnyNodeOwnedBy(other_org.org_id()),
        3600,
    )
    .expect("issue the cross-organization DISCOVER grant");
    let secret: OrgAudienceSecret = secret.expect("a DISCOVER grant carries audience material");
    let grant_id = grant.grant_id;
    let audience_handle = secret.audience_handle;
    let discovery_key = *secret.discovery_key();
    seam.consumer
        .install_consumer_grant_audience(grant, secret)
        .expect("install the consumer grant audience");

    // The granted-only provider is a REAL NODE, connected and PINNED, so its
    // absence from the sensed population cannot be explained away by the pin
    // intersection: it is eligible at every boundary except authorization.
    let granted_node = node_with(EntityKeypair::generate(), None).await;
    connect_pair(&seam.consumer, &granted_node).await;
    granted_node.start();
    granted_node
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("granted-plane node announce");
    let granted_id = granted_node.node_id();
    let consumer_id = seam.consumer.node_id();
    {
        let (consumer, peer) = (Arc::clone(&seam.consumer), Arc::clone(&granted_node));
        await_condition(
            SETTLE,
            "the granted-only provider is pinned in both directions",
            move || {
                consumer.peer_entity_id(granted_id).is_some()
                    && peer.peer_entity_id(consumer_id).is_some()
            },
        )
        .await;
    }

    // Two granted envelopes: one for that pinned granted-only node, and one for
    // the provider the OWNER plane already authorized.
    let descriptor = CapabilitySet::new().add_tag(TAG).to_bytes_compact();
    for keypair in [
        granted_node.entity_keypair(),
        seam.providers[0].entity_keypair(),
    ] {
        let cert = OrgMembershipCert::try_issue(&other_org, keypair.entity_id().clone(), 1, 3600)
            .expect("granted-plane membership cert");
        let envelope = ScopedCapabilityAnnouncement::build_granted(
            keypair,
            other_org.org_id(),
            cert,
            grant_id,
            audience_handle,
            &discovery_key,
            1,
            now_secs() + 3600,
            &descriptor,
        )
        .expect("granted envelope");
        seam.consumer
            .ingest_scoped_announcement_for_test(&envelope.to_bytes());
    }

    // PRECONDITIONS, both halves of what would make an exclusion vacuous:
    // the grant discovery really verified and stored both providers, AND the
    // granted-only one is pinned - so if granted discovery were wrongly fed
    // into the sensed population, it WOULD appear there.
    let granted: Vec<_> = seam
        .consumer
        .scoped_granted_providers_for_test(&grant_id, now_secs());
    assert!(
        granted.contains(granted_node.entity_id())
            && granted.contains(seam.providers[0].entity_id()),
        "precondition: both granted announcements were verified and stored: \
         {granted:?}"
    );
    assert!(
        seam.consumer.peer_entity_id(granted_id).is_some(),
        "precondition: the granted-only provider is pinned, so the pin \
         intersection cannot be what excludes it"
    );

    // ...and neither changed what is SENSED. No SENSE authority exists for
    // this provider, and DISCOVER alone never manufactures one.
    assert_eq!(
        authorized_population(&seam.consumer, TAG),
        vec![owner_provider],
        "the sensed population is owner-plane only: a pinned, granted, \
         verified provider is STILL not in it, and the dual-plane provider is \
         in it exactly ONCE"
    );
    let refreshed = seam.family.retain(TAG).expect("re-retain");
    assert_eq!(refreshed.population().to_vec(), vec![owner_provider]);
    assert_eq!(refreshed.retained_providers(), vec![owner_provider]);
    let projection =
        refreshed.project_sensed_order(Instant::now(), &ConsumerLatencyBudget::default());
    assert_eq!(
        projection.rows().len(),
        1,
        "one row, not two: {projection:?}"
    );

    // And in the ORDER, that same real granted candidate keeps its place and is
    // never pruned - sensing has no verdict about it at all.
    let mut paired = [(owner_provider, true), (granted_id, false)];
    paired.sort_unstable();
    let complete: Vec<u64> = paired.iter().map(|(id, _)| *id).collect();
    let same_org: Vec<bool> = paired.iter().map(|(_, owned)| *owned).collect();
    let order = sensed_provider_order(
        &refreshed,
        Instant::now(),
        &ConsumerLatencyBudget::default(),
        &complete,
        &same_org,
    );
    assert_eq!(
        order,
        vec![owner_provider, granted_id],
        "the sensed owner-plane provider leads; the granted candidate follows, \
         unsensed and unpruned"
    );

    // ALL-GRANTED CONTROL: the same two real providers, presented as granted
    // candidates only. One of them is sensed Ready on the owner plane - and it
    // still buys nothing, because no candidate is same-organization: the order
    // is the caller's own, unchanged and complete.
    let mut all_granted = vec![owner_provider, granted_id];
    all_granted.sort_unstable();
    let unchanged = sensed_provider_order(
        &refreshed,
        Instant::now(),
        &ConsumerLatencyBudget::default(),
        &all_granted,
        &[false, false],
    );
    assert_eq!(
        unchanged, all_granted,
        "an all-Granted list is returned exactly as given: no promotion of the \
         provider sensing happens to like, and no pruning"
    );

    drop(refreshed);
    seam.teardown().await;
    net::adapter::Adapter::shutdown(granted_node.as_ref())
        .await
        .expect("stop the granted-plane node");
}

/// An authorized provider with NO pinned entity is neither sensed nor callable.
///
/// Discovery authorizes it, so it is not an authority failure - but the sensed
/// population is discovery INTERSECTED with this node's pins, and a proof
/// cannot be bound to a peer whose entity is unknown. The composed preparation
/// therefore refuses at the PIN-BINDING check and mints nothing, which is the
/// core-side shape of "annotated, never a sensing verdict". A missing pin is
/// NOT the SDK's missing direct session: that requirement lives with the
/// candidate builder, is not reachable from core, and is OA-6's to witness.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unpinned_authorized_provider_is_neither_sensed_nor_callable() {
    let seam = compose("unpinned", &[true]).await;
    seam.settle(0, ProjectedReadiness::Ready).await;
    let pinned = seam.providers[0].node_id();

    // A real owner-scoped announcement for an entity this node has never met.
    let stranger = EntityKeypair::from_bytes([0x94u8; 32]);
    let authority = seam.consumer.node_authority().expect("authority");
    let cert = OrgMembershipCert::try_issue(&org(), stranger.entity_id().clone(), 1, 3600)
        .expect("stranger membership cert");
    let descriptor = CapabilitySet::new().add_tag(TAG).to_bytes_compact();
    let envelope = ScopedCapabilityAnnouncement::build_owner(
        &stranger,
        org().org_id(),
        cert,
        authority.audience.audience_handle,
        authority.audience.discovery_key(),
        1,
        now_secs() + 3600,
        &descriptor,
    )
    .expect("owner envelope");
    seam.consumer
        .ingest_scoped_announcement_for_test(&envelope.to_bytes());
    assert!(
        seam.consumer
            .scoped_owner_providers_for_test(now_secs())
            .contains(stranger.entity_id()),
        "precondition: discovery really authorized the stranger"
    );

    // It is authorized, and still not sensed: there is no pin to sense it AT.
    assert_eq!(
        authorized_population(&seam.consumer, TAG),
        vec![pinned],
        "an unpinned authorized provider is not in the sensed population"
    );

    // And a caller that lists it anyway is refused at the directness boundary,
    // before any intent exists.
    let stranger_node = stranger.entity_id().node_id();
    let before = seam.intents.load(Ordering::SeqCst);
    let refused = seam
        .prepare(&[stranger_node], &[true], &ConsumerLatencyBudget::default())
        .expect_err("no pinned entity, no proof binding");
    assert_eq!(refused, PrepareRefused::Unpinned(stranger_node));
    assert_eq!(
        seam.intents.load(Ordering::SeqCst),
        before,
        "and nothing was minted for it"
    );

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
