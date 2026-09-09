//! S1 consumer — the OWN-ORGANIZATION EXACT-PROVIDER observation, end to end.
//!
//! An out-of-crate integration test, so everything here is the shipped
//! surface: `Mesh::sensing`, `SensingClient::provide`, `SensingClient::watch`,
//! `SensingWatch::{snapshot, changed, close}`. Nothing is injected into the
//! snapshot: every readiness value these witnesses read was produced by a real
//! provider-side evaluator, signed by that provider's origin emitter, carried
//! over real loopback UDP through the ordinary registration/receive path, and
//! projected by the core's own classifier.
//!
//! Readiness values are always signed observations from a real evaluator.
//! `Unknown` rows are the exception by construction: they arise from local
//! ageing, withdrawal or an unretained provider, not from a signed value.
//!
//! What they hold:
//!
//! * **the observation is real** — signed provider readiness reaches the
//!   snapshot, and a provider state edge wakes an ALREADY-PARKED watcher
//!   below the fallback floor;
//! * **no wake is lost** — the change cursor is marked before the state is
//!   read, so a change placed inside a capture is never parked on; a quiet
//!   park does not return inside the same bound;
//! * **`Unknown` is attributed** — a departed provider's `Unknown` is proved
//!   to be AGEING (the last admitted attestation is still `Ready`), and
//!   withdrawal is its own separate case;
//! * **the request states both bounds** — the provider-start predicate really
//!   reaches the live evaluator and its negative answer stands against a huge
//!   local budget, while two consumer budgets over ONE attestation yield two
//!   viability verdicts and an over-budget provider is DEMOTED, never pruned;
//! * **row economics are the verdict's inputs** — a route change placed after
//!   classification cannot appear beside the verdict it did not produce;
//! * **node-global ownership** — two watches over one node share the interest
//!   row, an explicit close never disturbs a survivor or a successor, a closed
//!   handle is inert and loud, a SLOWER raw holder survives the SDK release
//!   with its renewal intact, an IDLE watch is really renewed across the ttl,
//!   and only the last release settles the refresh. The two-budget witness is
//!   NOT a cadence witness: the public cadence is fixed;
//! * **current authorization on every read** — an unavailable authority and a
//!   signed self-revocation both refuse new reads while keeping leases, a
//!   retracted or expired provider leaves the next snapshot (the retraction
//!   case strictly inside the pacing floor), and recovery restores the
//!   observation without reopening;
//! * **refusals are structured** — each impossible query shape, an
//!   unqualified observer and a node with sensing off are refused by their own
//!   variant, and a provider the node's interest capacity could not acquire
//!   reads `Unknown` and later recovers.
//!
//! Note on features: this crate's dev-dependency on `net-mesh` enables
//! `fixtures`, so nothing here may be read as a fixtures-OFF proof; the
//! consumer surface's own independence from that feature is a BUILD fact
//! (`cargo check -p net-mesh-sdk --lib --no-default-features --features net`),
//! not something this file shows. `fixtures` is needed for the node's
//! refresh-schedule and last-attestation observations, the direct authority
//! removal (labelled as a fixture transition beside the production
//! revocation witness), the placed capture/ranked-phase seams, and the
//! synthetic-member discovery ingest. `cortex` is needed because the authorized
//! population is derived from OWNER-SCOPED SERVICE discovery: a provider gets
//! into it by serving a same-organization service (`Mesh::serve_org`), which
//! is what seals its owner-audience announcement envelope.
#![cfg(all(feature = "net", feature = "cortex", feature = "fixtures"))]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::behavior::sensing::{
    CanonicalConstraints, CapabilityId, DisclosureClass, EvaluationRequest, Incarnation,
    InterestSpec, ProjectedReadiness, ProviderSelector, ReadinessEvaluation, ReadinessEvaluator,
    ResultMode, SensingLeaseTicket, WorkLatencyEnvelope,
};
use net::adapter::net::{ChannelConfigRegistry, MeshNode, MeshNodeConfig};
use net_sdk::identity::Identity;
use net_sdk::mesh::Mesh;
use net_sdk::mesh_rpc::ServeHandle;
use net_sdk::org::types::{
    CapabilityAuthorityId, NodeAuthority, OrgKeypair, OrgMembershipCert, OrgRevocationBundle,
    OwnerAudienceCredential,
};
use net_sdk::org::{OrgAccess, OrgCaller};
use net_sdk::sensing::{
    ReadinessRegistration, SensedViability, SensingError, SensingQuery, SensingWatch,
    DEFAULT_PROVIDER_START_WITHIN, POPULATION_RECONCILE_FLOOR,
};

const TAG: &str = "nrpc:internal.reindex";
/// The owner-scoped service whose `nrpc:` tag IS [`TAG`]. Serving it is what
/// puts a provider into the consumer's owner-private discovery.
const SERVICE: &str = "internal.reindex";
const SETTLE: Duration = Duration::from_secs(20);
/// Above the consumer surface's own population re-derivation floor, so a
/// witness that needs a re-derivation waits for one rather than hoping.
const PAST_FLOOR: Duration = POPULATION_RECONCILE_FLOOR.saturating_add(Duration::from_millis(200));
/// Well below the population re-derivation floor, so a wake inside it can only
/// have come from the node's own change signal.
const WAKE_BOUND: Duration = Duration::from_millis(250);
/// A quiet park must outlast this and still stay below the population floor,
/// so the control excludes both an always-immediate wake and a floor wake.
const QUIET_BOUND: Duration = Duration::from_millis(700);

fn org() -> OrgKeypair {
    OrgKeypair::from_bytes([0x71u8; 32])
}

/// The owner-scoped service's payloads. Never called in these witnesses — the
/// service exists because REGISTERING it is what makes the provider
/// discoverable; sensing is a separate plane from invocation.
#[derive(serde::Serialize, serde::Deserialize)]
struct Ping {
    n: u32,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Pong {
    ok: bool,
}

fn capability() -> CapabilityAuthorityId {
    CapabilityAuthorityId::for_tag(TAG)
}

/// What the live evaluator was actually asked, recorded from inside the
/// production emission path.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Asked {
    capability: String,
    /// The canonical constraint bytes the interest carried.
    constraints: Vec<u8>,
    /// The provider-start bound of the interest's latency envelope.
    start_within: Option<Duration>,
}

/// A real provider-side evaluator that HONORS the request it is given: it
/// answers the provider-start bound the consumer actually asked, exactly as
/// the already-frozen evaluator contract allows.
///
/// An evaluator that ignored `EvaluationRequest` could not distinguish a
/// query that asks a bound this provider can meet from one that asks a bound
/// it cannot, which is the whole point of the predicate witnesses.
struct Answer {
    ready: Arc<AtomicBool>,
    /// This provider's own time-to-start.
    start: Duration,
    /// The last request this evaluator saw, for the witnesses that assert the
    /// interest's real inputs rather than assuming them.
    asked: Arc<parking_lot::Mutex<Option<Asked>>>,
}

impl ReadinessEvaluator for Answer {
    fn evaluate(&self, request: &EvaluationRequest<'_>) -> ReadinessEvaluation {
        *self.asked.lock() = Some(Asked {
            capability: request.capability_id.as_str().to_string(),
            constraints: request.constraints.canonical_bytes(),
            start_within: request.work_latency.provider_start_within,
        });
        if !self.ready.load(Ordering::Relaxed) {
            return ReadinessEvaluation::NotReady { reason: 7 };
        }
        match request.work_latency.provider_start_within {
            // The provider cannot start inside the bound it was ASKED about.
            // A larger consumer budget cannot overturn this answer, because it
            // is an answer to a different question.
            Some(bound) if self.start > bound => ReadinessEvaluation::NotReady { reason: 9 },
            _ => ReadinessEvaluation::Ready {
                estimated_start: Some(self.start),
            },
        }
    }
}

/// One organization member: the public facade, the node it wraps, its
/// identity, its channel registry (a second `Mesh` wrapper over one node needs
/// it), and its authority directory.
struct Member {
    mesh: Mesh,
    node: Arc<MeshNode>,
    identity: Identity,
    configs: Arc<ChannelConfigRegistry>,
    dir: std::path::PathBuf,
}

impl Member {
    /// A SEPARATELY CONSTRUCTED SDK wrapper over this member's node — the
    /// shape the node-global ownership rule is about.
    fn second_wrapper(&self) -> Mesh {
        Mesh::from_node_arc(
            Arc::clone(&self.node),
            Arc::clone(&self.configs),
            Some(self.identity.clone()),
        )
    }
}

async fn mesh_in_org(
    tag: &str,
    owner: &OrgKeypair,
    sensing: bool,
    audience: Option<&OwnerAudienceCredential>,
) -> Member {
    mesh_in_org_full(tag, owner, sensing, audience, None).await
}

/// [`mesh_in_org`] with sensing on and an explicit soft-state interest ttl —
/// what makes an idle-renewal witness observable inside a test's lifetime.
async fn mesh_in_org_with_ttl(tag: &str, owner: &OrgKeypair, ttl: Option<Duration>) -> Member {
    mesh_in_org_full(tag, owner, true, None, ttl).await
}

async fn mesh_in_org_full(
    tag: &str,
    owner: &OrgKeypair,
    sensing: bool,
    audience: Option<&OwnerAudienceCredential>,
    interest_ttl: Option<Duration>,
) -> Member {
    let identity = Identity::generate();
    let mut cfg = MeshNodeConfig::new("127.0.0.1:0".parse().expect("addr"), [0x71u8; 32])
        .with_heartbeat_interval(Duration::from_millis(100))
        .with_session_timeout(Duration::from_secs(10));
    cfg.min_announce_interval = Duration::from_millis(50);
    cfg.configured_identity = true;
    if sensing {
        cfg = cfg
            .with_sensing_coalescing(true)
            .with_sensing_incarnation(Incarnation::new(1));
    }
    if let Some(ttl) = interest_ttl {
        cfg = cfg.with_sensing_interest_ttl(ttl);
    }

    let mut node = MeshNode::new((**identity.keypair()).clone(), cfg)
        .await
        .expect("MeshNode::new");
    let configs = Arc::new(ChannelConfigRegistry::new());
    node.set_channel_configs(configs.clone());
    let node = Arc::new(node);

    let entity = identity.entity_id().clone();
    let cert = OrgMembershipCert::try_issue(owner, entity.clone(), 1, 3600).expect("cert");
    let dir = std::env::temp_dir().join(format!(
        "net-s1c-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let adopted = NodeAuthority::adopt(&dir, cert, &entity, 0, None).expect("adopt");
    let authority = match audience {
        None => adopted,
        Some(shared) => NodeAuthority {
            config: adopted.config.clone(),
            audience: OwnerAudienceCredential::decode_config(&shared.encode_config())
                .expect("decode the shared owner audience"),
            revocation: adopted.revocation.clone(),
        },
    };
    node.install_node_authority(Arc::new(authority))
        .expect("install authority");
    node.set_owner_cert_emission(true)
        .expect("enable owner-cert emission");

    let mesh = Mesh::from_node_arc(node.clone(), configs.clone(), Some(identity.clone()));
    Member {
        mesh,
        node,
        identity,
        configs,
        dir,
    }
}

/// The organization's shared owner discovery audience, copied out of a node
/// that already adopted it.
fn shared_audience(member: &Member) -> OwnerAudienceCredential {
    OwnerAudienceCredential::decode_config(
        &member
            .node
            .node_authority()
            .expect("authority")
            .audience
            .encode_config(),
    )
    .expect("copy the owner audience")
}

/// Handshake, start, announce, and wait for entity pins in both directions —
/// the pin is what authorizes a discovered provider at all.
async fn bring_up(consumer: &Member, providers: &[&Member]) {
    for provider in providers {
        let provider_pub = *provider.mesh.public_key();
        let provider_addr = provider.mesh.local_addr().to_string();
        let target = Arc::clone(&provider.node);
        let consumer_id = consumer.mesh.node_id();
        let accept = tokio::spawn(async move { target.accept(consumer_id).await });
        consumer
            .mesh
            .connect(&provider_addr, &provider_pub, provider.mesh.node_id())
            .await
            .expect("connect");
        accept.await.expect("accept task").expect("accept");
    }
    consumer.mesh.start();
    for provider in providers {
        provider.mesh.start();
    }
    // Announcing is what SEALS each provider's owner-audience envelope from
    // the owner-scoped services it registered; the descriptor comes from
    // those registrations, never from this argument.
    for member in providers.iter().copied().chain([consumer]) {
        member
            .node
            .announce_capabilities(CapabilitySet::new())
            .await
            .expect("announce");
    }
    let deadline = Instant::now() + SETTLE;
    loop {
        let pinned = providers.iter().all(|p| {
            consumer.node.peer_entity_id(p.node.node_id()).is_some()
                && p.node.peer_entity_id(consumer.node.node_id()).is_some()
        });
        if pinned {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "entity pins were not established"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Serve REAL readiness through the shipped provider verb.
fn provide(member: &Member, ready: Arc<AtomicBool>, start: Duration) -> ReadinessRegistration {
    provide_recording(member, ready, start).0
}

/// [`provide`] plus the record of what the live evaluator was ASKED, so a
/// witness reads the interest's real inputs instead of assuming them.
fn provide_recording(
    member: &Member,
    ready: Arc<AtomicBool>,
    start: Duration,
) -> (
    ReadinessRegistration,
    Arc<parking_lot::Mutex<Option<Asked>>>,
) {
    let asked = Arc::new(parking_lot::Mutex::new(None));
    let registration = member
        .mesh
        .sensing()
        .expect("the provider sensing surface binds")
        .provide(
            CapabilityId::new(TAG),
            Arc::new(Answer {
                ready,
                start,
                asked: Arc::clone(&asked),
            }),
        )
        .expect("provide readiness");
    (registration, asked)
}

/// Register the OWNER-SCOPED service whose tag is [`TAG`], through the shipped
/// verb. This is what makes the provider discoverable — and therefore
/// authorizable — to same-organization consumers at all.
fn serve(member: &Member) -> ServeHandle {
    member
        .mesh
        .serve_org(
            SERVICE,
            OrgAccess::SameOrg,
            move |_caller: OrgCaller, _req: Ping| async move { Ok(Pong { ok: true }) },
        )
        .expect("serve_org")
}

/// Open a watch through the shipped consumer verb.
fn watch(member: &Member, query: SensingQuery) -> SensingWatch {
    watch_result(member, query).expect("watch")
}

/// [`watch`] without unwrapping — the refusal witnesses need the error.
fn watch_result(member: &Member, query: SensingQuery) -> Result<SensingWatch, SensingError> {
    member
        .mesh
        .sensing()
        .expect("the sensing surface binds")
        .watch(query)
}

/// The capability descriptor a synthetic owner announcement carries.
fn declared() -> CapabilitySet {
    CapabilitySet::new().add_tag(TAG)
}

/// The consumer's authorized SameOrg population: VERIFIED owner-private
/// discovery intersected with this node's entity pins. Exactly what authorizes
/// a candidate — sensing may reorder it, never extend it.
fn authorized(consumer: &Member) -> Vec<u64> {
    let mut ids: Vec<u64> = consumer
        .node
        .owner_private_capability_providers(&capability())
        .into_iter()
        .filter_map(|row| {
            let node_id = row.provider.node_id();
            (consumer.node.peer_entity_id(node_id).as_ref() == Some(&row.provider))
                .then_some(node_id)
        })
        .collect();
    ids.sort_unstable();
    ids
}

/// Re-announce until the consumer authorizes `wanted` providers.
async fn converge_population(consumer: &Member, providers: &[&Member], wanted: usize) {
    let deadline = Instant::now() + SETTLE;
    loop {
        for provider in providers {
            provider
                .node
                .announce_capabilities(CapabilitySet::new())
                .await
                .ok();
        }
        if authorized(consumer).len() >= wanted {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "owner-private discovery did not resolve {wanted} providers"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Admit ONE owner-scoped capability announcement for `provider` into `node`'s
/// verified owner-private discovery, through the shipped paths: the canonical
/// envelope builder, a real membership certificate from this organization, and
/// the node's own ordinary scoped-announcement ingest.
///
/// Deliberately does NOT pin the provider's entity — that is what makes
/// "discovered but unauthorized" a real state rather than an absence.
fn ingest_owner_announcement(
    node: &Arc<MeshNode>,
    owner: &OrgKeypair,
    provider: &net::adapter::net::identity::EntityKeypair,
    sequence: u64,
    expires_at: u64,
) {
    ingest_owner_announcement_of(node, owner, provider, sequence, expires_at, declared());
}

/// [`ingest_owner_announcement`] with an explicit capability descriptor, so a
/// witness can also admit the RETRACTION a provider publishes when it stops
/// serving the capability: a later-sequence announcement that no longer
/// declares the tag.
fn ingest_owner_announcement_of(
    node: &Arc<MeshNode>,
    owner: &OrgKeypair,
    provider: &net::adapter::net::identity::EntityKeypair,
    sequence: u64,
    expires_at: u64,
    capabilities: CapabilitySet,
) {
    let authority = node.node_authority().expect("authority");
    let cert = OrgMembershipCert::try_issue(owner, provider.entity_id().clone(), 1, 3600)
        .expect("membership");
    let descriptor = capabilities.to_bytes_compact();
    let envelope =
        net::adapter::net::behavior::org_scoped_ann::ScopedCapabilityAnnouncement::build_owner(
            provider,
            owner.org_id(),
            cert,
            authority.audience.audience_handle,
            authority.audience.discovery_key(),
            sequence,
            expires_at,
            &descriptor,
        )
        .expect("owner envelope");
    node.ingest_scoped_announcement_for_test(&envelope.to_bytes());
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs()
}

/// Poll `probe` until it holds, or fail with `what`.
async fn until(what: &str, deadline: Duration, mut probe: impl FnMut() -> bool) {
    let end = Instant::now() + deadline;
    loop {
        if probe() {
            return;
        }
        assert!(Instant::now() < end, "{what}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Snapshot until `predicate` holds, DRIVEN BY `changed()` — so every wait in
/// these witnesses is a real park on the shipped notification, and a lost wake
/// fails as a timeout rather than being polled around.
async fn until_snapshot(
    what: &str,
    watch: &mut SensingWatch,
    mut predicate: impl FnMut(&net_sdk::sensing::SensingSnapshot) -> bool,
) {
    let end = Instant::now() + SETTLE;
    loop {
        let snapshot = watch.snapshot().expect("snapshot");
        if predicate(&snapshot) {
            return;
        }
        assert!(Instant::now() < end, "{what}: last was {snapshot:?}");
        tokio::time::timeout(SETTLE, watch.changed())
            .await
            .unwrap_or_else(|_| panic!("{what}: changed() never returned"))
            .expect("changed");
    }
}

/// The row for `provider`, which must be present.
fn row(
    snapshot: &net_sdk::sensing::SensingSnapshot,
    provider: u64,
) -> net_sdk::sensing::SensedProvider {
    *snapshot
        .provider(provider)
        .unwrap_or_else(|| panic!("the population must carry {provider:#x}: {snapshot:?}"))
}

/// Acquire ONE of this node's own local-root sensing interests, for a provider
/// id nothing else uses. A real lease through the shipped node verb, so
/// releasing it is a real movement of this node's observation state.
fn acquire_local_interest(node: &Arc<MeshNode>, provider: u64) -> SensingLeaseTicket {
    try_acquire_local_interest(node, provider).expect("the local interest must install")
}

fn try_acquire_local_interest(node: &Arc<MeshNode>, provider: u64) -> Option<SensingLeaseTicket> {
    let spec = InterestSpec {
        capability_id: CapabilityId::new("capacity.filler"),
        constraints: CanonicalConstraints::default(),
        work_latency: WorkLatencyEnvelope::start_within(Duration::from_secs(2)),
        providers: ProviderSelector::Node(provider),
        result_mode: ResultMode::Any,
        disclosure_class: DisclosureClass::Owner,
        audience: node.sensing_local_root(),
    };
    node.acquire_sensing_interest_lease(&spec, provider, Duration::from_secs(2))
        .ok()
}

/// Fill this node's sensing-interest lease capacity, so every subsequent
/// exact-provider acquisition meets the production `NodeAtCapacity` refusal.
fn fill_sensing_capacity(node: &Arc<MeshNode>) -> Vec<SensingLeaseTicket> {
    let mut tickets = Vec::new();
    for provider in 1..=512u64 {
        match try_acquire_local_interest(node, u64::MAX - provider) {
            Some(ticket) => tickets.push(ticket),
            // Capacity reached: that is the point.
            None => break,
        }
    }
    assert!(
        !tickets.is_empty(),
        "precondition: the filler leases must actually install"
    );
    tickets
}

/// How many refresh records the node currently keeps armed.
fn armed(node: &Arc<MeshNode>) -> u64 {
    node.org_sensing_demand_state_for_test().armed
}

// ---------------------------------------------------------------------------
// The observation is real, and a provider edge wakes a parked watcher
// ---------------------------------------------------------------------------

/// Signed provider readiness traverses the real registration/receive path into
/// the snapshot; a genuine provider state edge then wakes a parked watcher and
/// the reread shows the new answer.
///
/// The negative half matters as much: a provider sensed NOT ready is DEMOTED —
/// it stays in the population, keeps its row, and only loses its rank. Pruning
/// it would show up here as a missing row.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn signed_readiness_reaches_the_snapshot_and_an_edge_wakes_the_watcher() {
    let owner = org();
    let consumer = mesh_in_org("c-edge", &owner, true, None).await;
    let audience = shared_audience(&consumer);
    let provider = mesh_in_org("p-edge", &owner, true, Some(&audience)).await;
    let _service = serve(&provider);
    bring_up(&consumer, &[&provider]).await;
    converge_population(&consumer, &[&provider], 1).await;

    let ready = Arc::new(AtomicBool::new(true));
    let registration = provide(&provider, ready.clone(), Duration::from_millis(120));
    let provider_id = provider.node.node_id();

    let mut observation = watch(&consumer, SensingQuery::new(TAG));
    until_snapshot("the provider never read Ready", &mut observation, |snap| {
        snap.provider(provider_id)
            .is_some_and(|p| p.readiness() == ProjectedReadiness::Ready)
    })
    .await;

    let snapshot = observation.snapshot().expect("snapshot");
    assert_eq!(snapshot.capability(), TAG);
    assert_eq!(
        snapshot
            .providers()
            .iter()
            .map(|p| p.node_id())
            .collect::<Vec<_>>(),
        vec![provider_id],
        "the population is exactly the authorized provider"
    );
    let sensed = row(&snapshot, provider_id);
    assert_eq!(sensed.viability(), SensedViability::Viable);
    assert_eq!(
        sensed.estimated_start(),
        Some(Duration::from_millis(120)),
        "the PROVIDER-signed estimate reaches the consumer verbatim"
    );
    assert!(
        sensed.route_estimate().is_some(),
        "a directly-sessioned peer has a route estimate"
    );
    assert_eq!(snapshot.ranked(), &[provider_id]);
    assert_eq!(snapshot.preferred(), Some(provider_id));

    // A real provider decision, published BEFORE the edge is announced.
    ready.store(false, Ordering::Relaxed);
    assert!(
        registration.changed(),
        "the state edge must move a live observation path"
    );

    until_snapshot(
        "the edge never reached the watcher",
        &mut observation,
        |snap| {
            snap.provider(provider_id)
                .is_some_and(|p| p.readiness() == ProjectedReadiness::NotReady)
        },
    )
    .await;

    let snapshot = observation.snapshot().expect("snapshot");
    let sensed = row(&snapshot, provider_id);
    assert_eq!(sensed.viability(), SensedViability::NotViable);
    assert_eq!(
        snapshot.ranked(),
        &[] as &[u64],
        "nothing is viable, so nothing is ranked"
    );
    assert_eq!(snapshot.preferred(), None);
    assert_eq!(
        snapshot.providers().len(),
        1,
        "a not-ready provider is demoted, never pruned: {snapshot:?}"
    );

    let _ = std::fs::remove_dir_all(&consumer.dir);
    let _ = std::fs::remove_dir_all(&provider.dir);
}

/// A change that lands DURING a capture is not lost: the next park returns at
/// once rather than waiting out the population floor.
///
/// Three things make this a lost-wake witness rather than a liveness one:
///
/// * the interleaving is PLACED, not raced. The capture seam fires at the one
///   instant that discriminates the two possible orderings — after the change
///   cursor is marked, before any state is read;
/// * what fires there is a REAL change: a live local sensing interest is
///   released through the shipped node verb, which moves this node's own
///   change generation synchronously. That the generation really moved is
///   asserted through an INDEPENDENT subscriber, so the witness cannot pass
///   on a change that never happened;
/// * the node is otherwise QUIET — no providers, no readiness traffic, no
///   discovery movement — so nothing but the seam's change can wake the park,
///   and `WAKE_BOUND` is far below the population floor. A swallowed wake can
///   then only be reported by the floor timer, which this bound excludes.
///
/// The control below establishes that the same park does NOT return inside
/// that bound when nothing changes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_change_landing_inside_a_capture_is_never_lost() {
    let owner = org();
    let consumer = mesh_in_org("c-nolost", &owner, true, None).await;
    let mut observation = watch(&consumer, SensingQuery::new(TAG));

    // One live local interest, owned by this witness, so releasing it is a
    // real movement of this node's observation state.
    let doomed = acquire_local_interest(&consumer.node, 0xD0_0Du64);
    let probe = consumer.node.subscribe_sensing_overlay_changes();
    assert!(
        !probe.has_changed().expect("the generation channel is live"),
        "precondition: the probe starts caught up"
    );

    let node = Arc::clone(&consumer.node);
    let fired = Arc::new(AtomicBool::new(false));
    let once = fired.clone();
    observation.set_capture_seam_for_test(Arc::new(move || {
        if once.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = node.try_release_sensing_interest_lease(doomed);
    }));

    let _during = observation.snapshot().expect("snapshot");
    assert!(fired.load(Ordering::SeqCst), "the seam must have fired");
    assert!(
        probe.has_changed().expect("the generation channel is live"),
        "precondition: the seam's release must really move the change generation"
    );

    tokio::time::timeout(WAKE_BOUND, observation.changed())
        .await
        .expect("a change that landed inside the capture was LOST")
        .expect("changed");

    let _ = std::fs::remove_dir_all(&consumer.dir);
}

/// The control for the witness above: with NOTHING changing, the same park
/// does not return inside `WAKE_BOUND` — and the population floor still wakes
/// it afterwards, so parking is bounded rather than indefinite.
///
/// Without this, an always-immediate `changed()` would satisfy the lost-wake
/// witness vacuously.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_quiet_park_does_not_return_inside_the_wake_bound() {
    let owner = org();
    let consumer = mesh_in_org("c-quiet", &owner, true, None).await;
    let mut observation = watch(&consumer, SensingQuery::new(TAG));
    // No providers, no readiness, no discovery movement: nothing can bump the
    // node's change generation.
    let _ = observation.snapshot().expect("snapshot");
    assert!(
        tokio::time::timeout(QUIET_BOUND, observation.changed())
            .await
            .is_err(),
        "a quiet network must not produce a wake inside the bound"
    );
    // And the floor still guarantees liveness.
    tokio::time::timeout(SETTLE, observation.changed())
        .await
        .expect("the population floor must still wake a parked consumer")
        .expect("changed");

    let _ = std::fs::remove_dir_all(&consumer.dir);
}

/// A provider that goes AWAY makes the observation expire, and the expiry
/// reaches a parked watcher in an otherwise quiet network — no traffic, no
/// replacement observation, no polling by the witness.
///
/// This is the pure continuity property: the provider process is shut down, so
/// nothing arrives to overwrite the last `Ready`. The stale estimate must go
/// with it — an expired cell's provider estimate is metadata nothing vouches
/// for.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_quiet_expiry_reaches_a_parked_watcher_and_clears_the_estimate() {
    let owner = org();
    let consumer = mesh_in_org("c-expiry", &owner, true, None).await;
    let audience = shared_audience(&consumer);
    let provider = mesh_in_org("p-expiry", &owner, true, Some(&audience)).await;
    let _service = serve(&provider);
    bring_up(&consumer, &[&provider]).await;
    converge_population(&consumer, &[&provider], 1).await;

    let ready = Arc::new(AtomicBool::new(true));
    let registration = provide(&provider, ready, Duration::from_millis(90));
    let provider_id = provider.node.node_id();

    let mut observation = watch(&consumer, SensingQuery::new(TAG));
    until_snapshot("the provider never read Ready", &mut observation, |snap| {
        snap.provider(provider_id)
            .is_some_and(|p| p.readiness() == ProjectedReadiness::Ready)
    })
    .await;

    // The provider leaves. Its readiness registration and its whole mesh go
    // with it, so the consumer receives nothing further about it.
    drop(registration);
    let Member {
        mesh, dir: p_dir, ..
    } = provider;
    mesh.shutdown().await.expect("provider shutdown");

    until_snapshot(
        "the quiet expiry never reached the watcher",
        &mut observation,
        |snap| {
            snap.provider(provider_id)
                .is_some_and(|p| p.readiness() == ProjectedReadiness::Unknown)
        },
    )
    .await;

    let snapshot = observation.snapshot().expect("snapshot");
    let sensed = row(&snapshot, provider_id);
    assert_eq!(
        sensed.estimated_start(),
        None,
        "an expired observation carries no start estimate"
    );
    assert_eq!(
        sensed.viability(),
        SensedViability::Potential,
        "Unknown retains potential capacity — it is not a not-ready verdict"
    );

    let _ = std::fs::remove_dir_all(&consumer.dir);
    let _ = std::fs::remove_dir_all(&p_dir);
}

/// WITHDRAWING readiness — the provider closes its registration while staying
/// up and beating — reads `Unknown`, not not-ready, and clears the estimate.
///
/// The inverse of the expiry witness: here the beats keep arriving and carry
/// no evaluation, so this is about withdrawal semantics rather than about
/// elapsed continuity.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn withdrawn_readiness_reads_unknown_and_keeps_the_provider() {
    let owner = org();
    let consumer = mesh_in_org("c-withdraw", &owner, true, None).await;
    let audience = shared_audience(&consumer);
    let provider = mesh_in_org("p-withdraw", &owner, true, Some(&audience)).await;
    let _service = serve(&provider);
    bring_up(&consumer, &[&provider]).await;
    converge_population(&consumer, &[&provider], 1).await;

    let ready = Arc::new(AtomicBool::new(true));
    let registration = provide(&provider, ready, Duration::from_millis(60));
    let provider_id = provider.node.node_id();

    let mut observation = watch(&consumer, SensingQuery::new(TAG));
    until_snapshot("the provider never read Ready", &mut observation, |snap| {
        snap.provider(provider_id)
            .is_some_and(|p| p.readiness() == ProjectedReadiness::Ready)
    })
    .await;

    assert!(registration.close(), "the withdrawal removes the evaluator");

    until_snapshot("the withdrawal never surfaced", &mut observation, |snap| {
        snap.provider(provider_id)
            .is_some_and(|p| p.readiness() == ProjectedReadiness::Unknown)
    })
    .await;

    let snapshot = observation.snapshot().expect("snapshot");
    let sensed = row(&snapshot, provider_id);
    assert_eq!(sensed.viability(), SensedViability::Potential);
    assert_eq!(sensed.estimated_start(), None);
    assert_eq!(snapshot.preferred(), None);
    assert_eq!(
        snapshot.providers().len(),
        1,
        "a withdrawn provider stays authorized and stays reported"
    );

    let _ = std::fs::remove_dir_all(&consumer.dir);
    let _ = std::fs::remove_dir_all(&provider.dir);
}

// ---------------------------------------------------------------------------
// Request-relative: one attestation, two budgets, two verdicts
// ---------------------------------------------------------------------------

/// ONE signed attestation, two watches on ONE node with different CONSUMER
/// budgets: the unbounded one ranks the provider, the tight one demotes it to
/// POTENTIAL — never not-viable, because the provider said `Ready` and only
/// this consumer's own budget disagrees.
///
/// Both watches ask the SAME provider-start bound, which is what makes this a
/// budget witness: the signed answer is one fact and the two verdicts differ
/// only in local arithmetic. The bound is asked explicitly and generously,
/// because the provider's own 30-second start would legitimately be
/// `NotReady` against the default two-second predicate — that mismatch is a
/// different witness, below.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_attestation_yields_two_verdicts_under_two_budgets() {
    let owner = org();
    let consumer = mesh_in_org("c-budget", &owner, true, None).await;
    let audience = shared_audience(&consumer);
    let provider = mesh_in_org("p-budget", &owner, true, Some(&audience)).await;
    let _service = serve(&provider);
    bring_up(&consumer, &[&provider]).await;
    converge_population(&consumer, &[&provider], 1).await;

    let ready = Arc::new(AtomicBool::new(true));
    // Far above any plausible loopback route estimate, so "over budget" is a
    // property of the fixture rather than of the runner's load.
    let _registration = provide(&provider, ready, Duration::from_secs(30));
    let provider_id = provider.node.node_id();
    let asks = SensingQuery::new(TAG).start_within(Duration::from_secs(60));

    let mut unbounded = watch(&consumer, asks.clone());
    let mut tight = watch(&consumer, asks.within(Duration::from_secs(1)));

    until_snapshot("the provider never read Ready", &mut unbounded, |snap| {
        snap.provider(provider_id)
            .is_some_and(|p| p.readiness() == ProjectedReadiness::Ready)
    })
    .await;

    let open = unbounded.snapshot().expect("snapshot");
    assert_eq!(row(&open, provider_id).viability(), SensedViability::Viable);
    assert_eq!(open.preferred(), Some(provider_id));

    let bounded = tight.snapshot().expect("snapshot");
    let sensed = row(&bounded, provider_id);
    assert_eq!(
        sensed.readiness(),
        ProjectedReadiness::Ready,
        "the SIGNED readiness is the same fact for both watchers"
    );
    assert_eq!(
        sensed.viability(),
        SensedViability::Potential,
        "over budget is a demotion, not a pruning: {bounded:?}"
    );
    assert_eq!(bounded.preferred(), None);
    assert_eq!(bounded.ranked(), &[] as &[u64]);
    assert_eq!(
        bounded.providers().len(),
        1,
        "the population is the same population"
    );

    let _ = std::fs::remove_dir_all(&consumer.dir);
    let _ = std::fs::remove_dir_all(&provider.dir);
}

// ---------------------------------------------------------------------------
// Node-global ownership across separately constructed wrappers
// ---------------------------------------------------------------------------

/// Two watches built from SEPARATELY CONSTRUCTED `Mesh` wrappers over one
/// node: they share the node's interest row and its single refresh record.
/// Closing one leaves the survivor observing and the refresh armed; a closed
/// handle is inert (a repeat close removes nothing, and its reads are refused);
/// a SUCCESSOR opened after that close is not disturbed by the stale handle;
/// only the last close stops the refresh and deregisters the row.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn independent_wrappers_share_ownership_and_the_last_close_stops_the_refresh() {
    let owner = org();
    let consumer = mesh_in_org("c-own", &owner, true, None).await;
    let audience = shared_audience(&consumer);
    let provider = mesh_in_org("p-own", &owner, true, Some(&audience)).await;
    let _service = serve(&provider);
    bring_up(&consumer, &[&provider]).await;
    converge_population(&consumer, &[&provider], 1).await;

    let ready = Arc::new(AtomicBool::new(true));
    let _registration = provide(&provider, ready, Duration::from_millis(75));
    let provider_id = provider.node.node_id();
    assert_eq!(armed(&consumer.node), 0, "precondition: nothing armed yet");

    let second = consumer.second_wrapper();
    let mut first_watch = watch(&consumer, SensingQuery::new(TAG));
    let mut second_watch = second
        .sensing()
        .expect("the second wrapper binds")
        .watch(SensingQuery::new(TAG))
        .expect("second watch");

    until_snapshot(
        "the first watch never read Ready",
        &mut first_watch,
        |snap| {
            snap.provider(provider_id)
                .is_some_and(|p| p.readiness() == ProjectedReadiness::Ready)
        },
    )
    .await;
    assert_eq!(
        row(&second_watch.snapshot().expect("snapshot"), provider_id).readiness(),
        ProjectedReadiness::Ready,
        "both wrappers observe ONE shared interest row"
    );
    assert_eq!(
        armed(&consumer.node),
        1,
        "two owners of one exact interest share ONE refresh record"
    );

    // The first owner closes. The survivor must keep observing, and the
    // node's cadence must not be taken away with it.
    assert!(first_watch.close(), "the first close retires this owner");
    assert!(!first_watch.close(), "a repeat close retires nothing");
    assert!(first_watch.is_closed());
    assert_eq!(
        first_watch.snapshot().expect_err("a closed watch is inert"),
        SensingError::WatchClosed
    );
    assert_eq!(
        armed(&consumer.node),
        1,
        "a non-final release must not disarm a live survivor's renewal"
    );
    assert_eq!(
        row(&second_watch.snapshot().expect("snapshot"), provider_id).readiness(),
        ProjectedReadiness::Ready,
        "the survivor's observation is untouched"
    );

    // A SUCCESSOR joins, and the stale handle is used again: it must remove
    // nothing at all.
    let mut successor = watch(&consumer, SensingQuery::new(TAG));
    assert!(!first_watch.close(), "the stale handle stays inert");
    drop(first_watch);
    assert_eq!(
        row(&successor.snapshot().expect("snapshot"), provider_id).readiness(),
        ProjectedReadiness::Ready,
        "a stale handle's close and drop cannot evict a successor"
    );
    assert_eq!(armed(&consumer.node), 1);

    // The LAST owners leave: the row and its cadence go with them.
    assert!(second_watch.close());
    assert_eq!(armed(&consumer.node), 1, "the successor still owns it");
    assert!(successor.close());
    until(
        "the last release never settled the refresh record",
        SETTLE,
        || armed(&consumer.node) == 0,
    )
    .await;
    until(
        "the last release never deregistered the interest row",
        SETTLE,
        || consumer.node.sensing_observation_count() == 0,
    )
    .await;

    let _ = std::fs::remove_dir_all(&consumer.dir);
    let _ = std::fs::remove_dir_all(&provider.dir);
}

/// A watch that is DROPPED without an explicit close retires its demand too —
/// so a forgotten close cannot permanently pin the node's cadence.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dropped_watch_stops_pinning_the_cadence() {
    let owner = org();
    let consumer = mesh_in_org("c-drop", &owner, true, None).await;
    let audience = shared_audience(&consumer);
    let provider = mesh_in_org("p-drop", &owner, true, Some(&audience)).await;
    let _service = serve(&provider);
    bring_up(&consumer, &[&provider]).await;
    converge_population(&consumer, &[&provider], 1).await;

    let ready = Arc::new(AtomicBool::new(true));
    let _registration = provide(&provider, ready, Duration::from_millis(75));
    let provider_id = provider.node.node_id();

    {
        let mut observation = watch(&consumer, SensingQuery::new(TAG));
        until_snapshot("the provider never read Ready", &mut observation, |snap| {
            snap.provider(provider_id)
                .is_some_and(|p| p.readiness() == ProjectedReadiness::Ready)
        })
        .await;
        assert_eq!(armed(&consumer.node), 1);
    }

    until("the drop never released the demand", SETTLE, || {
        armed(&consumer.node) == 0
    })
    .await;
    until("the drop never deregistered the row", SETTLE, || {
        consumer.node.sensing_observation_count() == 0
    })
    .await;

    let _ = std::fs::remove_dir_all(&consumer.dir);
    let _ = std::fs::remove_dir_all(&provider.dir);
}

// ---------------------------------------------------------------------------
// Authority loss and recovery
// ---------------------------------------------------------------------------

/// An unavailable organization authority HIDES the population instead of
/// republishing it: the watch keeps its leases and its refresh record, but a
/// NEW read is refused rather than answered with authorization the node no
/// longer holds. Recovery is immediate — inside the population floor, with no
/// reopen.
///
/// The earlier revision of this witness required the opposite (the old
/// provider still reported after the authority went away), which is exactly
/// the stale-visibility disclosure this repair removes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unavailable_authority_hides_the_population_and_recovery_restores_it() {
    let owner = org();
    let consumer = mesh_in_org("c-auth", &owner, true, None).await;
    let audience = shared_audience(&consumer);
    let provider = mesh_in_org("p-auth", &owner, true, Some(&audience)).await;
    let _service = serve(&provider);
    bring_up(&consumer, &[&provider]).await;
    converge_population(&consumer, &[&provider], 1).await;

    let ready = Arc::new(AtomicBool::new(true));
    let _registration = provide(&provider, ready, Duration::from_millis(75));
    let provider_id = provider.node.node_id();

    let mut observation = watch(&consumer, SensingQuery::new(TAG));
    until_snapshot("the provider never read Ready", &mut observation, |snap| {
        snap.provider(provider_id)
            .is_some_and(|p| p.readiness() == ProjectedReadiness::Ready)
    })
    .await;
    let installed = consumer
        .node
        .node_authority()
        .expect("the authority is installed");

    // The authority goes away. This is the FIXTURE transition (a direct
    // removal), deliberately labelled as such: the production revocation
    // schedule is the witness below.
    consumer.node.clear_node_authority_for_test();
    assert_eq!(
        observation
            .snapshot()
            .expect_err("an unqualified observer must not be answered"),
        SensingError::ObserverNotQualified,
        "a new read must not present historical authorization as current"
    );
    assert_eq!(
        watch_result(&consumer, SensingQuery::new(TAG))
            .expect_err("a new watch must not be established either"),
        SensingError::ObserverNotQualified
    );
    assert_eq!(
        armed(&consumer.node),
        1,
        "hiding the population must not disarm the live renewal or release the lease"
    );

    // And it comes back — INSIDE the population floor, so this cannot pass by
    // waiting out a pacing timer.
    consumer
        .node
        .install_node_authority(installed)
        .expect("reinstall authority");
    let recovered_at = Instant::now();
    let recovered = observation
        .snapshot()
        .expect("a requalified observer is answered again");
    assert!(
        recovered_at.elapsed() < POPULATION_RECONCILE_FLOOR,
        "recovery must not wait out the population floor"
    );
    assert_eq!(
        recovered
            .providers()
            .iter()
            .map(|p| p.node_id())
            .collect::<Vec<_>>(),
        vec![provider_id],
        "and it returns the CURRENTLY authorized population"
    );
    assert_eq!(armed(&consumer.node), 1);

    let _ = std::fs::remove_dir_all(&consumer.dir);
    let _ = std::fs::remove_dir_all(&provider.dir);
}

/// The PRODUCTION schedule: a signed revocation bundle raises THIS consumer's
/// own member floor above its installed certificate. Both an existing watch
/// and a newly opened one must refuse, and re-admission under a fresh
/// certificate must restore the observation.
///
/// This is the counterexample the stamp check could not see: the authority
/// object is still installed, the provider's own membership is untouched, its
/// discovery row still qualifies, and the retained holder is still live — only
/// the OBSERVER stopped being a member.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_self_revoked_observer_is_refused_on_existing_and_new_reads() {
    let owner = org();
    let consumer = mesh_in_org("c-revoke", &owner, true, None).await;
    let audience = shared_audience(&consumer);
    let provider = mesh_in_org("p-revoke", &owner, true, Some(&audience)).await;
    let _service = serve(&provider);
    bring_up(&consumer, &[&provider]).await;
    converge_population(&consumer, &[&provider], 1).await;

    let ready = Arc::new(AtomicBool::new(true));
    let _registration = provide(&provider, ready, Duration::from_millis(75));
    let provider_id = provider.node.node_id();

    let mut observation = watch(&consumer, SensingQuery::new(TAG));
    until_snapshot("the provider never read Ready", &mut observation, |snap| {
        snap.provider(provider_id)
            .is_some_and(|p| p.readiness() == ProjectedReadiness::Ready)
    })
    .await;
    // CONTROL: a healthy membership answers both an existing and a new read.
    let mut control = watch(&consumer, SensingQuery::new(TAG));
    assert_eq!(control.snapshot().expect("control").providers().len(), 1);
    control.close();

    // The organization revokes the CONSUMER: floor 2 over its generation-1
    // certificate, through the shipped bundle path.
    let mut floors = std::collections::BTreeMap::new();
    floors.insert(consumer.identity.entity_id().clone(), 2u32);
    let bundle = OrgRevocationBundle::try_issue(&owner, &floors).expect("bundle");
    consumer
        .node
        .node_authority()
        .expect("authority")
        .revocation
        .apply_bundle(&bundle)
        .expect("the organization raises this member's floor");

    assert_eq!(
        observation
            .snapshot()
            .expect_err("a revoked observer must not be answered"),
        SensingError::ObserverNotQualified
    );
    assert_eq!(
        watch_result(&consumer, SensingQuery::new(TAG))
            .expect_err("nor may it open a new observation"),
        SensingError::ObserverNotQualified
    );
    assert!(
        provider
            .node
            .peer_entity_id(consumer.node.node_id())
            .is_some(),
        "the provider is untouched: this is about the observer's own membership"
    );

    // RE-ADMISSION: a fresh certificate at the new floor restores the
    // observation, on the existing watch.
    let entity = consumer.identity.entity_id().clone();
    let renewed = OrgMembershipCert::try_issue(&owner, entity.clone(), 2, 3600).expect("cert");
    let readmitted =
        NodeAuthority::adopt(&consumer.dir, renewed, &entity, 0, None).expect("re-adopt");
    consumer
        .node
        .install_node_authority(Arc::new(NodeAuthority {
            config: readmitted.config.clone(),
            audience: OwnerAudienceCredential::decode_config(&readmitted.audience.encode_config())
                .expect("audience"),
            revocation: readmitted.revocation.clone(),
        }))
        .expect("install the renewed authority");

    until_snapshot(
        "re-admission never restored the observation",
        &mut observation,
        |snap| snap.provider(provider_id).is_some(),
    )
    .await;

    let _ = std::fs::remove_dir_all(&consumer.dir);
    let _ = std::fs::remove_dir_all(&provider.dir);
}

// ---------------------------------------------------------------------------
// Structured refusals, and the capacity path
// ---------------------------------------------------------------------------

/// Every unsupported or impossible request is refused by its OWN variant, and
/// the same node accepts the supported one — so none of these refusals is a
/// blanket "sensing is unavailable".
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn each_unsupported_request_is_refused_by_its_own_variant() {
    let owner = org();
    let consumer = mesh_in_org("c-refuse", &owner, true, None).await;
    let client = consumer.mesh.sensing().expect("bind");

    assert_eq!(
        client
            .watch(SensingQuery::new("   "))
            .expect_err("a blank capability observes nothing"),
        SensingError::EmptyCapability
    );
    assert_eq!(
        client
            .watch(SensingQuery::new(TAG).within(Duration::ZERO))
            .expect_err("a zero budget admits nothing"),
        SensingError::UnsatisfiableBudget
    );
    assert_eq!(
        client
            .watch(SensingQuery::new(TAG).start_within(Duration::ZERO))
            .expect_err("a zero provider-start bound can never hold"),
        SensingError::UnsatisfiableStartBound
    );

    // The supported form, on the same node: this is what makes the refusals
    // above specific rather than a dark plane.
    let mut ok = client.watch(SensingQuery::new(TAG)).expect("watch");
    assert!(
        ok.snapshot().expect("snapshot").providers().is_empty(),
        "no authorized providers yet is an empty population, not an error"
    );
    ok.close();

    // No installed organization authority: this node is not currently
    // entitled to observe at all, which is its OWN refusal rather than a
    // query-shape or capacity one.
    consumer.node.clear_node_authority_for_test();
    assert_eq!(
        client
            .watch(SensingQuery::new(TAG))
            .expect_err("no authority, no entitlement"),
        SensingError::ObserverNotQualified
    );

    // Sensing off is refused a step earlier, at the surface itself.
    let dark = mesh_in_org("c-dark", &owner, false, None).await;
    assert_eq!(
        dark.mesh
            .sensing()
            .map(|_| ())
            .expect_err("the plane ships dark"),
        SensingError::Disabled
    );

    let _ = std::fs::remove_dir_all(&consumer.dir);
    let _ = std::fs::remove_dir_all(&dark.dir);
}

/// A provider the node's own interest capacity could not acquire is reported
/// `Unknown` — present, potential, no error, nothing armed — and a later
/// snapshot past the population floor acquires it once capacity frees.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_capacity_refused_provider_reads_unknown_and_later_recovers() {
    let owner = org();
    let consumer = mesh_in_org("c-cap", &owner, true, None).await;
    let audience = shared_audience(&consumer);
    let provider = mesh_in_org("p-cap", &owner, true, Some(&audience)).await;
    let _service = serve(&provider);
    bring_up(&consumer, &[&provider]).await;
    converge_population(&consumer, &[&provider], 1).await;

    let ready = Arc::new(AtomicBool::new(true));
    let _registration = provide(&provider, ready, Duration::from_millis(75));
    let provider_id = provider.node.node_id();

    let fillers = fill_sensing_capacity(&consumer.node);
    let mut observation = watch(&consumer, SensingQuery::new(TAG));
    let refused = observation
        .snapshot()
        .expect("a refused acquisition is not a watch error");
    let sensed = row(&refused, provider_id);
    assert_eq!(
        sensed.readiness(),
        ProjectedReadiness::Unknown,
        "an unretained provider has no evidence: {refused:?}"
    );
    assert_eq!(sensed.viability(), SensedViability::Potential);
    assert_eq!(
        armed(&consumer.node),
        0,
        "nothing was retained, so nothing is armed"
    );

    for ticket in fillers {
        let _ = consumer.node.try_release_sensing_interest_lease(ticket);
    }
    tokio::time::sleep(PAST_FLOOR).await;
    until_snapshot(
        "the recovered capacity never produced an acquisition",
        &mut observation,
        |snap| {
            snap.provider(provider_id)
                .is_some_and(|p| p.readiness() == ProjectedReadiness::Ready)
        },
    )
    .await;
    assert_eq!(armed(&consumer.node), 1);

    let _ = std::fs::remove_dir_all(&consumer.dir);
    let _ = std::fs::remove_dir_all(&provider.dir);
}

// ---------------------------------------------------------------------------
// Population movement, and the ceiling on visibility
// ---------------------------------------------------------------------------

/// A provider authorized AFTER the watch was opened enters the population,
/// through the watch's own paced re-derivation, with no reopen and no
/// caller-supplied population. DEPARTURE is a separate witness
/// (`a_retracted_provider_leaves_the_next_snapshot_inside_the_floor` and
/// `an_expired_announcement_leaves_the_population_and_a_live_one_stays`); this
/// one is the widening direction only.
///
/// The widening half is also the visibility ceiling: a provider that is
/// DISCOVERED but not pinned is not authorized, and sensing must never report
/// it. Both providers are announced through the same verified owner path; only
/// the pin separates them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_population_follows_authorization_and_never_widens_past_it() {
    let owner = org();
    let consumer = mesh_in_org("c-pop", &owner, true, None).await;
    let audience = shared_audience(&consumer);
    let first = mesh_in_org("p-pop-1", &owner, true, Some(&audience)).await;
    let second = mesh_in_org("p-pop-2", &owner, true, Some(&audience)).await;

    let _first_service = serve(&first);
    let _second_service = serve(&second);
    bring_up(&consumer, &[&first]).await;
    converge_population(&consumer, &[&first], 1).await;
    let ready_one = Arc::new(AtomicBool::new(true));
    let _first_readiness = provide(&first, ready_one, Duration::from_millis(75));
    let first_id = first.node.node_id();
    let second_id = second.node.node_id();

    let mut observation = watch(&consumer, SensingQuery::new(TAG));
    until_snapshot(
        "the first provider never read Ready",
        &mut observation,
        |snap| {
            snap.provider(first_id)
                .is_some_and(|p| p.readiness() == ProjectedReadiness::Ready)
        },
    )
    .await;
    assert_eq!(
        observation.snapshot().expect("snapshot").providers().len(),
        1
    );

    // The second provider's owner-scoped announcement is admitted into the
    // consumer's VERIFIED owner-private discovery through the real ingest
    // path — the canonical builder, this organization's real membership cert
    // and the consumer's own discovery audience — while the two nodes never
    // handshake, so its entity stays unpinned. Discovered, not authorized.
    ingest_owner_announcement(
        &consumer.node,
        &owner,
        &**second.identity.keypair(),
        1,
        unix_now() + 3600,
    );
    let discovered: Vec<u64> = consumer
        .node
        .owner_private_capability_providers(&capability())
        .into_iter()
        .map(|row| row.provider.node_id())
        .collect();
    assert!(
        discovered.contains(&second_id),
        "precondition: the unpinned provider must really be in verified discovery"
    );
    assert!(
        !authorized(&consumer).contains(&second_id),
        "precondition: and it must not be authorized"
    );
    tokio::time::sleep(PAST_FLOOR).await;
    let narrow = observation.snapshot().expect("snapshot");
    assert!(
        narrow.provider(second_id).is_none(),
        "an unpinned provider is not authorized and must never be sensed: {narrow:?}"
    );
    assert_eq!(
        narrow
            .providers()
            .iter()
            .map(|p| p.node_id())
            .collect::<Vec<_>>(),
        authorized(&consumer),
        "the snapshot population IS the authorized population"
    );

    // Now authorize it for real: handshake (which pins both entities) and let
    // discovery resolve. The watch must pick it up without being reopened.
    bring_up(&consumer, &[&second]).await;
    converge_population(&consumer, &[&first, &second], 2).await;
    let ready_two = Arc::new(AtomicBool::new(true));
    let _second_readiness = provide(&second, ready_two, Duration::from_millis(40));
    tokio::time::sleep(PAST_FLOOR).await;
    until_snapshot(
        "the newly authorized provider never entered the population",
        &mut observation,
        |snap| {
            snap.provider(second_id)
                .is_some_and(|p| p.readiness() == ProjectedReadiness::Ready)
        },
    )
    .await;

    let wide = observation.snapshot().expect("snapshot");
    assert_eq!(
        wide.providers()
            .iter()
            .map(|p| p.node_id())
            .collect::<Vec<_>>(),
        authorized(&consumer)
    );
    assert_eq!(
        wide.ranked().len(),
        2,
        "both viable providers are ranked: {wide:?}"
    );
    assert_eq!(
        wide.preferred(),
        wide.ranked().first().copied(),
        "the preference is the head of the sensed rank order"
    );

    let _ = std::fs::remove_dir_all(&consumer.dir);
    let _ = std::fs::remove_dir_all(&first.dir);
    let _ = std::fs::remove_dir_all(&second.dir);
}

// ---------------------------------------------------------------------------
// Current visibility is a READ-side fact, not a paced one
// ---------------------------------------------------------------------------

/// A provider that stops being currently visible leaves the very NEXT
/// snapshot, inside the population floor — while a still-visible peer stays.
///
/// The retraction is a real authorization fact and needs no clock race: the
/// departing member publishes a later-sequence owner announcement that no
/// longer declares this capability, admitted through the real verified
/// ingest. Verified owner-private discovery therefore excludes it
/// immediately, while its retained lease, the authority stamp and the refresh
/// record are all untouched — exactly the case an acquisition-paced
/// implementation cannot see. The read happens strictly inside the population
/// floor (asserted), so no convergence can be the reason the row disappeared.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_retracted_provider_leaves_the_next_snapshot_inside_the_floor() {
    let owner = org();
    let consumer = mesh_in_org("c-retract", &owner, true, None).await;
    let audience = shared_audience(&consumer);
    let staying = mesh_in_org("p-retract", &owner, true, Some(&audience)).await;
    let _staying_service = serve(&staying);
    bring_up(&consumer, &[&staying]).await;
    converge_population(&consumer, &[&staying], 1).await;

    let ready = Arc::new(AtomicBool::new(true));
    let _stayed = provide(&staying, ready, Duration::from_millis(75));
    let staying_id = staying.node.node_id();

    // The departing member: a real organization member whose owner-scoped
    // announcement is admitted through the verified ingest and whose entity
    // is pinned, so it is genuinely authorized to begin with.
    let leaving = net::adapter::net::identity::EntityKeypair::generate();
    let leaving_id = leaving.entity_id().node_id();
    ingest_owner_announcement(&consumer.node, &owner, &leaving, 1, unix_now() + 3600);
    consumer
        .node
        .test_pin_peer_entity(leaving_id, leaving.entity_id().clone());
    assert!(
        authorized(&consumer).contains(&leaving_id),
        "precondition: the departing member starts out authorized"
    );

    let mut observation = watch(&consumer, SensingQuery::new(TAG));
    let before = observation.snapshot().expect("snapshot");
    assert!(
        before.provider(leaving_id).is_some() && before.provider(staying_id).is_some(),
        "precondition: both members are reported: {before:?}"
    );
    let armed_before = armed(&consumer.node);
    assert!(armed_before >= 1, "precondition: the demand is retained");

    // The RETRACTION: a later-sequence announcement that no longer declares
    // the capability.
    ingest_owner_announcement_of(
        &consumer.node,
        &owner,
        &leaving,
        2,
        unix_now() + 3600,
        CapabilitySet::new(),
    );
    assert!(
        authorized(&consumer).iter().all(|id| *id != leaving_id),
        "precondition: current visibility must already exclude it"
    );

    let read_at = Instant::now();
    let after = observation.snapshot().expect("snapshot");
    assert!(
        read_at.elapsed() < POPULATION_RECONCILE_FLOOR,
        "this read must land inside the population floor to be discriminating"
    );
    assert!(
        after.provider(leaving_id).is_none(),
        "a provider outside current visibility must not be reported: {after:?}"
    );
    assert!(
        after.ranked().iter().all(|id| *id != leaving_id),
        "and it must not survive in the rank order either"
    );
    // CONTROL: the still-visible peer is unaffected, so the clamp is not a
    // blanket emptying.
    assert!(
        after.provider(staying_id).is_some(),
        "the surviving provider must still be reported: {after:?}"
    );
    assert_eq!(
        armed(&consumer.node),
        armed_before,
        "hiding a row must not release a lease or disarm a renewal"
    );

    let _ = std::fs::remove_dir_all(&consumer.dir);
    let _ = std::fs::remove_dir_all(&staying.dir);
}

/// An owner announcement that EXPIRES leaves the population, and a
/// long-lived one beside it does not.
///
/// Wall-clock expiry needs no authority publication and no holder change: the
/// discovery query filters it, and the snapshot is clamped to that query.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_expired_announcement_leaves_the_population_and_a_live_one_stays() {
    let owner = org();
    let consumer = mesh_in_org("c-expire", &owner, true, None).await;
    let audience = shared_audience(&consumer);
    let provider = mesh_in_org("p-expire", &owner, true, Some(&audience)).await;
    let _service = serve(&provider);
    bring_up(&consumer, &[&provider]).await;
    converge_population(&consumer, &[&provider], 1).await;
    let provider_id = provider.node.node_id();

    // A SECOND, synthetic member of the same organization: announced through
    // the real verified ingest with a short life, and pinned, so it is
    // genuinely authorized until its announcement expires.
    let ephemeral = net::adapter::net::identity::EntityKeypair::generate();
    let ephemeral_node = ephemeral.entity_id().node_id();
    ingest_owner_announcement(&consumer.node, &owner, &ephemeral, 1, unix_now() + 2);
    consumer
        .node
        .test_pin_peer_entity(ephemeral_node, ephemeral.entity_id().clone());
    assert!(
        authorized(&consumer).contains(&ephemeral_node),
        "precondition: the short-lived member is authorized while its row lives"
    );

    let ready = Arc::new(AtomicBool::new(true));
    let _registration = provide(&provider, ready, Duration::from_millis(75));
    let mut observation = watch(&consumer, SensingQuery::new(TAG));
    let opened = observation.snapshot().expect("snapshot");
    assert!(
        opened.provider(ephemeral_node).is_some(),
        "the short-lived member is reported while it is visible: {opened:?}"
    );

    until(
        "the announcement never expired out of discovery",
        SETTLE,
        || !authorized(&consumer).contains(&ephemeral_node),
    )
    .await;

    let after = observation.snapshot().expect("snapshot");
    assert!(
        after.provider(ephemeral_node).is_none(),
        "an expired announcement must leave the snapshot: {after:?}"
    );
    // CONTROL: the long-lived real provider is untouched.
    assert!(
        after.provider(provider_id).is_some(),
        "the live provider must remain: {after:?}"
    );

    let _ = std::fs::remove_dir_all(&consumer.dir);
    let _ = std::fs::remove_dir_all(&provider.dir);
}

// ---------------------------------------------------------------------------
// The query asks the predicate it says it asks
// ---------------------------------------------------------------------------

/// The provider-start bound is a REAL question, answered by the real
/// evaluator on the production emission path.
///
/// Three cases over one provider whose own time-to-start is three seconds:
///
/// * the DEFAULT bound (two seconds) is honestly answered `NotReady`, and a
///   hundred-second consumer budget cannot overturn it — it is an answer to a
///   different question;
/// * a query that ASKS five seconds gets `Ready`/`Viable` from the same
///   provider, over an independent interest;
/// * the evaluator's recorded inputs show the real capability id, EMPTY
///   canonical constraints, and the bound each watch asked.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_query_asks_the_provider_start_bound_it_names() {
    let owner = org();
    let consumer = mesh_in_org("c-predicate", &owner, true, None).await;
    let audience = shared_audience(&consumer);
    let provider = mesh_in_org("p-predicate", &owner, true, Some(&audience)).await;
    let _service = serve(&provider);
    bring_up(&consumer, &[&provider]).await;
    converge_population(&consumer, &[&provider], 1).await;

    let ready = Arc::new(AtomicBool::new(true));
    let (_registration, asked) = provide_recording(&provider, ready, Duration::from_secs(3));
    let provider_id = provider.node.node_id();

    // 1. The default bound, with a huge local budget.
    let mut default_watch = watch(
        &consumer,
        SensingQuery::new(TAG).within(Duration::from_secs(100)),
    );
    until_snapshot(
        "the default bound never got an answer",
        &mut default_watch,
        |snap| {
            snap.provider(provider_id)
                .is_some_and(|p| p.readiness() != ProjectedReadiness::Unknown)
        },
    )
    .await;
    let refused = default_watch.snapshot().expect("snapshot");
    let sensed = row(&refused, provider_id);
    assert_eq!(
        sensed.readiness(),
        ProjectedReadiness::NotReady,
        "a provider that cannot start inside the asked bound answers NotReady: {refused:?}"
    );
    assert_eq!(sensed.viability(), SensedViability::NotViable);
    assert_eq!(
        asked
            .lock()
            .clone()
            .expect("the evaluator ran")
            .start_within,
        Some(DEFAULT_PROVIDER_START_WITHIN),
        "the default watch really asked the default bound"
    );
    assert_eq!(
        asked.lock().clone().expect("the evaluator ran").capability,
        TAG,
        "and it asked about this capability, verbatim"
    );
    assert_eq!(
        asked.lock().clone().expect("the evaluator ran").constraints,
        CanonicalConstraints::default().canonical_bytes(),
        "canonical constraints are fixed EMPTY, as documented"
    );

    // 2. ASK the bound the provider can meet. Same provider, same signature
    //    path, different question.
    let mut asking = watch(
        &consumer,
        SensingQuery::new(TAG).start_within(Duration::from_secs(5)),
    );
    until_snapshot("the asked bound never got a Ready", &mut asking, |snap| {
        snap.provider(provider_id)
            .is_some_and(|p| p.readiness() == ProjectedReadiness::Ready)
    })
    .await;
    let honored = asking.snapshot().expect("snapshot");
    let sensed = row(&honored, provider_id);
    assert_eq!(sensed.viability(), SensedViability::Viable);
    assert_eq!(sensed.estimated_start(), Some(Duration::from_secs(3)));
    assert_eq!(honored.preferred(), Some(provider_id));
    assert_eq!(
        asked
            .lock()
            .clone()
            .expect("the evaluator ran")
            .start_within,
        Some(Duration::from_secs(5)),
        "the asked bound reached the live evaluator verbatim"
    );

    // 3. The two watches are INDEPENDENT questions: the default one still
    //    reads NotReady after the generous one succeeded.
    let still = default_watch.snapshot().expect("snapshot");
    assert_eq!(
        row(&still, provider_id).readiness(),
        ProjectedReadiness::NotReady,
        "a different bound must not rewrite this watch's answer: {still:?}"
    );

    let _ = std::fs::remove_dir_all(&consumer.dir);
    let _ = std::fs::remove_dir_all(&provider.dir);
}

// ---------------------------------------------------------------------------
// A row's economics are the inputs its verdict used
// ---------------------------------------------------------------------------

/// A route change placed AFTER classification must not appear beside the
/// verdict it did not produce.
///
/// The mutation is placed with the core's own off-lock projection observer at
/// the `ranked` phase — after the proximity pass and the classifier, before
/// the SDK assembles the result — and both edge directions are replaced so the
/// undirected minimum really moves. The row's own economics must then still
/// agree with its viability under this request's budget.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_row_never_reports_economics_its_verdict_did_not_use() {
    let owner = org();
    let consumer = mesh_in_org("c-route", &owner, true, None).await;
    let audience = shared_audience(&consumer);
    let provider = mesh_in_org("p-route", &owner, true, Some(&audience)).await;
    let _service = serve(&provider);
    bring_up(&consumer, &[&provider]).await;
    converge_population(&consumer, &[&provider], 1).await;

    let ready = Arc::new(AtomicBool::new(true));
    let _registration = provide(&provider, ready, Duration::from_millis(75));
    let provider_id = provider.node.node_id();
    let budget = Duration::from_secs(1);

    let mut observation = watch(&consumer, SensingQuery::new(TAG).within(budget));
    until_snapshot("the provider never read Ready", &mut observation, |snap| {
        snap.provider(provider_id)
            .is_some_and(|p| p.readiness() == ProjectedReadiness::Ready)
    })
    .await;

    // CONTROL: with nothing moving, the row is Viable and its economics fit.
    let control = observation.snapshot().expect("snapshot");
    let sensed = row(&control, provider_id);
    assert_eq!(sensed.viability(), SensedViability::Viable);
    assert!(
        fits(&sensed, budget),
        "control: a viable row's own numbers must fit its budget: {control:?}"
    );

    // Now move the route AFTER the classification that produced the verdict.
    let graph = Arc::clone(consumer.node.proximity_graph());
    let local = graph.my_id();
    let target = graph_id(provider_id);
    let fired = Arc::new(AtomicBool::new(false));
    let once = Arc::clone(&fired);
    consumer
        .node
        .set_sensing_projection_offlock_observer_for_test(Arc::new(move |observation| {
            if observation.phase != "ranked" || once.swap(true, Ordering::SeqCst) {
                return;
            }
            graph.remove_edge(local, target);
            graph.remove_edge(target, local);
            graph.test_insert_edge(local, target, 11_000_000);
            graph.test_insert_edge(target, local, 11_000_000);
        }));

    let moved = observation.snapshot().expect("snapshot");
    assert!(
        fired.load(Ordering::SeqCst),
        "the ranked-phase observer must have fired"
    );
    let sensed = row(&moved, provider_id);
    assert!(
        fits(&sensed, budget) || sensed.viability() != SensedViability::Viable,
        "a row's reported economics must be the ones its verdict used: {moved:?}"
    );
    assert!(
        moved.ranked().contains(&provider_id) == (sensed.viability() == SensedViability::Viable),
        "the rank order and the row's class must agree: {moved:?}"
    );

    let _ = std::fs::remove_dir_all(&consumer.dir);
    let _ = std::fs::remove_dir_all(&provider.dir);
}

/// Whether a row's own reported numbers fit `budget`.
fn fits(row: &net_sdk::sensing::SensedProvider, budget: Duration) -> bool {
    row.route_estimate().unwrap_or_default() + row.estimated_start().unwrap_or_default() <= budget
}

/// The proximity graph's key for a node id (the graph keys nodes by the u64
/// zero-padded to 32 bytes).
fn graph_id(node_id: u64) -> [u8; 32] {
    let mut id = [0u8; 32];
    id[..8].copy_from_slice(&node_id.to_le_bytes());
    id
}

// ---------------------------------------------------------------------------
// Wakes, expiry attribution, and autonomous refresh
// ---------------------------------------------------------------------------

/// An ALREADY-PARKED watcher is woken by a real provider edge, far below the
/// population floor — so the wake came from the node's change signal, not from
/// the fallback timer or from a first reread.
///
/// The park happens first and the edge is fired from another task after the
/// watcher is committed to waiting; the wake bound excludes the one-second
/// fallback. `a_quiet_park_does_not_return_inside_the_wake_bound` is the
/// paired control.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_already_parked_watcher_is_woken_by_a_provider_edge() {
    let owner = org();
    let consumer = mesh_in_org("c-parked", &owner, true, None).await;
    let audience = shared_audience(&consumer);
    let provider = mesh_in_org("p-parked", &owner, true, Some(&audience)).await;
    let _service = serve(&provider);
    bring_up(&consumer, &[&provider]).await;
    converge_population(&consumer, &[&provider], 1).await;

    let ready = Arc::new(AtomicBool::new(true));
    let registration = provide(&provider, ready.clone(), Duration::from_millis(75));
    let provider_id = provider.node.node_id();

    let mut observation = watch(&consumer, SensingQuery::new(TAG));
    until_snapshot("the provider never read Ready", &mut observation, |snap| {
        snap.provider(provider_id)
            .is_some_and(|p| p.readiness() == ProjectedReadiness::Ready)
    })
    .await;
    // Fresh capture, so the change cursor is caught up and the fallback timer
    // is a full floor away.
    let _ = observation.snapshot().expect("snapshot");

    // The edge fires only after this task is parked.
    let edge = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(40)).await;
        ready.store(false, Ordering::Relaxed);
        assert!(
            registration.changed(),
            "the state edge must move a live observation path"
        );
        registration
    });

    let parked = Instant::now();
    tokio::time::timeout(WAKE_BOUND, observation.changed())
        .await
        .expect("a parked watcher must be woken by the provider edge")
        .expect("changed");
    let waited = parked.elapsed();
    assert!(
        waited < POPULATION_RECONCILE_FLOOR,
        "the wake must arrive below the fallback floor, waited {waited:?}"
    );
    let _registration = edge.await.expect("edge task");

    until_snapshot("the edge never became visible", &mut observation, |snap| {
        snap.provider(provider_id)
            .is_some_and(|p| p.readiness() == ProjectedReadiness::NotReady)
    })
    .await;

    let _ = std::fs::remove_dir_all(&consumer.dir);
    let _ = std::fs::remove_dir_all(&provider.dir);
}

/// The `Unknown` a departed provider produces is attributed: the node's LAST
/// ADMITTED attestation for this watch's own branch is still `Ready`, so the
/// projection aged out rather than being replaced by a withdrawal or a fresh
/// negative beat.
///
/// The registration is deliberately NOT closed before the provider stops — a
/// withdrawal would produce exactly the replacement this witness must exclude.
/// The claim is bounded-eventual visibility of loss of continuity, observed
/// through the shipped watch; it is not a claim of exact expiry-instant
/// notification.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_departed_provider_ages_out_rather_than_being_replaced() {
    let owner = org();
    let consumer = mesh_in_org("c-age", &owner, true, None).await;
    let audience = shared_audience(&consumer);
    let provider = mesh_in_org("p-age", &owner, true, Some(&audience)).await;
    let _service = serve(&provider);
    bring_up(&consumer, &[&provider]).await;
    converge_population(&consumer, &[&provider], 1).await;

    let ready = Arc::new(AtomicBool::new(true));
    let registration = provide(&provider, ready, Duration::from_millis(90));
    let provider_id = provider.node.node_id();

    let mut observation = watch(&consumer, SensingQuery::new(TAG));
    until_snapshot("the provider never read Ready", &mut observation, |snap| {
        snap.provider(provider_id)
            .is_some_and(|p| p.readiness() == ProjectedReadiness::Ready)
    })
    .await;
    assert_eq!(
        observation.last_attested_status_for_test(provider_id),
        Some(net::adapter::net::behavior::sensing::AttestedStatus::Ready),
        "precondition: the last admitted attestation is Ready"
    );

    // The provider's PROCESS goes away with its readiness registration still
    // installed, so nothing withdraws and nothing replaces the last value.
    let Member {
        mesh,
        dir: p_dir,
        node: p_node,
        ..
    } = provider;
    std::mem::forget(registration);
    drop(p_node);
    mesh.shutdown().await.expect("provider shutdown");

    until_snapshot("the departure never surfaced", &mut observation, |snap| {
        snap.provider(provider_id)
            .is_some_and(|p| p.readiness() == ProjectedReadiness::Unknown)
    })
    .await;

    assert_eq!(
        observation.last_attested_status_for_test(provider_id),
        Some(net::adapter::net::behavior::sensing::AttestedStatus::Ready),
        "the projection must have AGED OUT: no replacement or withdrawal beat \
         was admitted, so Unknown is loss of continuity"
    );
    let snapshot = observation.snapshot().expect("snapshot");
    let sensed = row(&snapshot, provider_id);
    assert_eq!(
        sensed.estimated_start(),
        None,
        "an aged-out observation carries no start estimate"
    );
    assert_eq!(sensed.viability(), SensedViability::Potential);

    let _ = std::fs::remove_dir_all(&consumer.dir);
    let _ = std::fs::remove_dir_all(&p_dir);
}

/// An IDLE application's watch is renewed by the node: with no `snapshot` and
/// no `changed` call for longer than the interest ttl, the node's refresh
/// worker actually renews the installation, the observation stays live, and
/// after the last owner closes, no further renewal happens.
///
/// The node is built with a short ttl so this is an executed renewal rather
/// than an armed record: `armed == 1` alone is satisfied by a worker that
/// never fires.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_idle_watch_is_actually_renewed_and_stops_after_the_last_close() {
    let owner = org();
    let consumer = mesh_in_org_with_ttl("c-renew", &owner, Some(Duration::from_secs(2))).await;
    let audience = shared_audience(&consumer);
    let provider = mesh_in_org("p-renew", &owner, true, Some(&audience)).await;
    let _service = serve(&provider);
    bring_up(&consumer, &[&provider]).await;
    converge_population(&consumer, &[&provider], 1).await;

    let ready = Arc::new(AtomicBool::new(true));
    let _registration = provide(&provider, ready, Duration::from_millis(75));
    let provider_id = provider.node.node_id();

    let mut observation = watch(&consumer, SensingQuery::new(TAG));
    until_snapshot("the provider never read Ready", &mut observation, |snap| {
        snap.provider(provider_id)
            .is_some_and(|p| p.readiness() == ProjectedReadiness::Ready)
    })
    .await;
    let renewed_before = consumer
        .node
        .org_sensing_demand_state_for_test()
        .refresh_renewed;

    // IDLE: no watch call at all for longer than the ttl.
    tokio::time::sleep(Duration::from_secs(5)).await;
    let renewed_after = consumer
        .node
        .org_sensing_demand_state_for_test()
        .refresh_renewed;
    assert!(
        renewed_after > renewed_before,
        "an idle watch must be RENEWED by the node, not merely armed \
         ({renewed_before} -> {renewed_after})"
    );
    assert_eq!(armed(&consumer.node), 1);
    let after_idle = observation.snapshot().expect("snapshot");
    assert!(
        after_idle.provider(provider_id).is_some(),
        "the observation survived the idle window: {after_idle:?}"
    );

    // The last owner leaves: renewal must STOP.
    assert!(observation.close());
    until("the last close never settled the refresh", SETTLE, || {
        armed(&consumer.node) == 0
    })
    .await;
    let settled = consumer
        .node
        .org_sensing_demand_state_for_test()
        .refresh_renewed;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(
        consumer
            .node
            .org_sensing_demand_state_for_test()
            .refresh_renewed,
        settled,
        "nothing may renew after the last owner released"
    );

    let _ = std::fs::remove_dir_all(&consumer.dir);
    let _ = std::fs::remove_dir_all(&provider.dir);
}

/// A watch coexisting with a SLOWER raw holder of the same exact interest:
/// releasing the (stricter) SDK owner leaves the survivor's row and holder
/// intact, and only the survivor's own release tears it down.
///
/// The public SDK cadence is intentionally fixed, so the differently paced
/// holder is established through the existing raw node lease API against the
/// SAME canonical interest — the core's own spec builder, so this is one
/// digest rather than a resemblance of it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slower_raw_holder_survives_the_sdk_watch_release() {
    let owner = org();
    let consumer = mesh_in_org("c-mixed", &owner, true, None).await;
    let audience = shared_audience(&consumer);
    let provider = mesh_in_org("p-mixed", &owner, true, Some(&audience)).await;
    let _service = serve(&provider);
    bring_up(&consumer, &[&provider]).await;
    converge_population(&consumer, &[&provider], 1).await;

    let ready = Arc::new(AtomicBool::new(true));
    let _registration = provide(&provider, ready, Duration::from_millis(75));
    let provider_id = provider.node.node_id();

    let mut observation = watch(&consumer, SensingQuery::new(TAG));
    until_snapshot("the provider never read Ready", &mut observation, |snap| {
        snap.provider(provider_id)
            .is_some_and(|p| p.readiness() == ProjectedReadiness::Ready)
    })
    .await;

    // A SLOWER holder of the same interest, through the raw node verb.
    let org_id = consumer
        .node
        .node_authority()
        .expect("authority")
        .owner_org();
    let spec = net::adapter::net::behavior::org_sensing_demand::exact_provider_spec(
        TAG,
        net::adapter::net::behavior::sensing::canonical_org_sensing_commitment(&org_id),
        net::adapter::net::behavior::org_sensing_demand::fixed_work_latency(),
        provider_id,
    );
    let slower = consumer
        .node
        .acquire_sensing_interest_lease(&spec, provider_id, Duration::from_secs(4))
        .expect("the slower holder joins the SDK watch's own interest");
    assert!(
        consumer.node.sensing_observation_count() >= 1,
        "precondition: the interest row is live with both holders on it"
    );

    // The stricter SDK owner leaves.
    assert!(observation.close());
    // The survivor's own holder is proved still live by its release
    // SUCCEEDING below; what is asserted here is that the row and its
    // observation were not torn down with the other owner's release.
    assert_eq!(
        armed(&consumer.node),
        1,
        "release-then-SETTLE: a non-final release must NOT take the live \
         installation's renewal away from the surviving holder"
    );
    assert!(
        consumer.node.sensing_observation_count() >= 1,
        "the row survives while the slower holder keeps it referenced"
    );

    // And only the survivor's own release tears it down.
    consumer
        .node
        .try_release_sensing_interest_lease(slower)
        .expect("the survivor releases");
    until(
        "the final release never deregistered the row",
        SETTLE,
        || consumer.node.sensing_observation_count() == 0,
    )
    .await;
    until(
        "the final release never settled the refresh record",
        SETTLE,
        || armed(&consumer.node) == 0,
    )
    .await;

    let _ = std::fs::remove_dir_all(&consumer.dir);
    let _ = std::fs::remove_dir_all(&provider.dir);
}
