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

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
/// The ABSOLUTE observation deadline a wake witness allows for its whole
/// attributed observation — several bounded parks plus their synchronous
/// captures — measured from the first park and still below the population
/// fallback floor. Distinct from [`WAKE_BOUND`], which bounds ONE park.
const OBSERVATION_BOUND: Duration = Duration::from_millis(900);
/// A quiet park must outlast this and still stay below the population floor,
/// so the control excludes both an always-immediate wake and a floor wake.
const QUIET_BOUND: Duration = Duration::from_millis(700);
/// The cadence the SDK's retained demand asks (the fixed internal policy,
/// clamped to the node's soft-state horizon — 2s on these nodes).
const SDK_SAMPLE_INTERVAL: Duration = Duration::from_secs(2);
/// The LOOSER cadence the coexisting raw holder asks.
const SLOWER_SAMPLE_INTERVAL: Duration = Duration::from_secs(4);
/// The pacing floor the recovery witnesses widen to. Long enough that "the
/// previous success floor is still unexpired" survives a signed on-disk
/// ceremony, so those witnesses qualify the SCHEDULE instead of racing a
/// short wall-clock interval.
const RECOVERY_FLOOR: Duration = Duration::from_secs(120);

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
    /// This provider's own time-to-start, in milliseconds, published as
    /// ordinary provider state so a witness can move it while readiness stays
    /// `Ready`.
    start_ms: Arc<AtomicU64>,
    /// EVERY request this evaluator saw. A list, not a last value: two
    /// watches asking different bounds are two concurrent beat streams, and a
    /// single slot would let one overwrite the other's observation.
    asked: Arc<parking_lot::Mutex<Vec<Asked>>>,
}

impl ReadinessEvaluator for Answer {
    fn evaluate(&self, request: &EvaluationRequest<'_>) -> ReadinessEvaluation {
        self.asked.lock().push(Asked {
            capability: request.capability_id.as_str().to_string(),
            constraints: request.constraints.canonical_bytes(),
            start_within: request.work_latency.provider_start_within,
        });
        if !self.ready.load(Ordering::Relaxed) {
            return ReadinessEvaluation::NotReady { reason: 7 };
        }
        let start = Duration::from_millis(self.start_ms.load(Ordering::Relaxed));
        match request.work_latency.provider_start_within {
            // The provider cannot start inside the bound it was ASKED about.
            // A larger consumer budget cannot overturn this answer, because it
            // is an answer to a different question.
            Some(bound) if start > bound => ReadinessEvaluation::NotReady { reason: 9 },
            _ => ReadinessEvaluation::Ready {
                estimated_start: Some(start),
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

/// [`provide`] with the provider's own time-to-start left ADJUSTABLE, so a
/// witness can publish a new estimate while readiness stays `Ready`.
fn provide_adjustable(
    member: &Member,
    ready: Arc<AtomicBool>,
    start: Duration,
) -> (ReadinessRegistration, Arc<AtomicU64>) {
    let start_ms = Arc::new(AtomicU64::new(start.as_millis() as u64));
    let registration = member
        .mesh
        .sensing()
        .expect("the provider sensing surface binds")
        .provide(
            CapabilityId::new(TAG),
            Arc::new(Answer {
                ready,
                start_ms: Arc::clone(&start_ms),
                asked: Arc::new(parking_lot::Mutex::new(Vec::new())),
            }),
        )
        .expect("provide readiness");
    (registration, start_ms)
}

/// [`provide`] plus the record of what the live evaluator was ASKED, so a
/// witness reads the interest's real inputs instead of assuming them.
fn provide_recording(
    member: &Member,
    ready: Arc<AtomicBool>,
    start: Duration,
) -> (ReadinessRegistration, Arc<parking_lot::Mutex<Vec<Asked>>>) {
    let asked = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let registration = member
        .mesh
        .sensing()
        .expect("the provider sensing surface binds")
        .provide(
            CapabilityId::new(TAG),
            Arc::new(Answer {
                ready,
                start_ms: Arc::new(AtomicU64::new(start.as_millis() as u64)),
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

/// PARK ACKNOWLEDGEMENT: converge, then prove the park really is `Pending`
/// with the fallback deadline far outside [`WAKE_BOUND`], and return that
/// deadline.
///
/// Retried across ordinary beat arrivals: a live provider legitimately beats
/// at its cadence, so a wake landing inside the acknowledgement window is not
/// a defect — it just means this attempt was not a quiet window. A wake
/// consumed here is a wake this witness then cannot ride, which is the
/// conservative direction. `changed` is cancel-safe and the seen-cursor lives
/// on the receiver, so the timeout leaves no state behind.
async fn acknowledge_parked(observation: &mut SensingWatch) -> Instant {
    for _ in 0..40 {
        let _ = observation.snapshot().expect("snapshot");
        let fallback = observation.fallback_deadline_for_test();
        if fallback.saturating_duration_since(Instant::now()) <= WAKE_BOUND.saturating_mul(2) {
            // Too close to the fallback to attribute anything; let the floor
            // elapse so the next snapshot re-arms it.
            tokio::time::sleep(Duration::from_millis(200)).await;
            continue;
        }
        if tokio::time::timeout(Duration::from_millis(80), observation.changed())
            .await
            .is_err()
        {
            assert_eq!(
                observation.fallback_deadline_for_test(),
                fallback,
                "the acknowledged park must not have re-armed the fallback"
            );
            return fallback;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("no quiet window to acknowledge a park in");
}

/// Park until a wake arrives whose IMMEDIATE read satisfies `carried`, and
/// return how many earlier wakes were skipped.
///
/// This is the whole attribution, inside the bounded observation. Two bounds,
/// kept distinct:
///
/// * a PER-PARK bound — every individual park must return within
///   [`WAKE_BOUND`];
/// * an ABSOLUTE OBSERVATION DEADLINE — the earlier of the caller's saved
///   fallback deadline and [`OBSERVATION_BOUND`] from the first park. It is
///   checked when the wake returns AND AGAIN once the synchronous snapshot and
///   predicate have COMPLETED, because an async timeout cannot interrupt
///   synchronous work: a lock wait or descheduling inside the capture would
///   otherwise let state observed after the deadline be accepted as if it had
///   arrived inside it.
///
/// So the fallback timer cannot explain the return, and no LATER producer can
/// retroactively give meaning to an EARLIER unrelated wake: unrelated wakes are
/// counted and re-parked on, never accepted, and the producer is joined only
/// after the evidence, for cleanup.
///
/// The quiet acknowledgement that precedes this is a receiver/cursor proof
/// only — a cancelled `changed()` is not a barrier on this wait — which is
/// exactly why the accepted wake must carry the state itself.
async fn wake_carrying(
    what: &str,
    observation: &mut SensingWatch,
    fallback: Instant,
    mut carried: impl FnMut(&net_sdk::sensing::SensingSnapshot) -> bool,
) -> usize {
    let deadline = fallback.min(Instant::now() + OBSERVATION_BOUND);
    let mut skipped = 0usize;
    loop {
        tokio::time::timeout(WAKE_BOUND, observation.changed())
            .await
            .unwrap_or_else(|_| panic!("{what}: no wake arrived inside the wake bound"))
            .expect("changed");
        assert!(
            Instant::now() < deadline,
            "{what}: the wake must arrive before the observation deadline"
        );
        let snapshot = observation.snapshot().expect("snapshot");
        let accepted = carried(&snapshot);
        // AFTER the synchronous capture and predicate: what is accepted is the
        // COMPLETED observation, so it must meet the same deadline.
        assert!(
            Instant::now() < deadline,
            "{what}: the completed capture must also meet the observation \
             deadline — state read after it is not evidence of the wake"
        );
        if accepted {
            return skipped;
        }
        skipped += 1;
        assert!(
            skipped <= 8,
            "{what}: too many wakes carried nothing ({snapshot:?})"
        );
    }
}

/// Publish a REAL, UNRELATED node change: acquire and release one of this
/// node's own local interests. Used to prove a witness cannot accept a wake
/// that preceded its named event.
fn publish_unrelated_change(node: &Arc<MeshNode>, provider: u64) {
    let ticket = acquire_local_interest(node, provider);
    let _ = node.try_release_sensing_interest_lease(ticket);
}

/// Snapshot until `predicate` holds, DRIVEN BY `changed()` — so every wait in
/// these witnesses is a real park on the shipped notification, and a lost wake
/// fails as a timeout rather than being polled around.
///
/// Returns the snapshot that SATISFIED the predicate, because that is the read
/// a witness may assert on. Readiness and viability are continuity-based: a
/// projection decays (`Ready` -> `Unknown`, `Viable`/`NotViable` ->
/// `Potential`, an estimate to `None`) once a branch's continuity window
/// elapses without a fresh qualifying beat. A witness that waits for a state
/// and then takes a SECOND, independent snapshot to assert on is therefore
/// asserting about a read it never qualified — which is how
/// `the_population_follows_authorization_and_never_widens_past_it` failed CI
/// run 34517413243 with `ranked` of length 1 and the newly authorized
/// provider read as `Unknown`. Fold the claim into the predicate and assert on
/// what comes back.
async fn until_snapshot(
    what: &str,
    watch: &mut SensingWatch,
    mut predicate: impl FnMut(&net_sdk::sensing::SensingSnapshot) -> bool,
) -> net_sdk::sensing::SensingSnapshot {
    let end = Instant::now() + SETTLE;
    loop {
        let snapshot = watch.snapshot().expect("snapshot");
        if predicate(&snapshot) {
            return snapshot;
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

    let snapshot = until_snapshot(
        "the edge never reached the watcher",
        &mut observation,
        |snap| {
            snap.provider(provider_id)
                .is_some_and(|p| p.readiness() == ProjectedReadiness::NotReady)
        },
    )
    .await;

    // On the read that qualified: a second one could have aged the signed
    // `NotReady` into `Unknown`, whose viability is `Potential`, not
    // `NotViable` — a decay, not the demotion this asserts.
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
/// Four things make this a lost-wake witness rather than a liveness one:
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
///   discovery movement — so nothing but the seam's change can wake the park;
/// * the FALLBACK is excluded by the deadline it actually has, not by the
///   shortness of a timeout. The park is acknowledged first
///   ([`acknowledge_parked`]), which proves the receiver is caught up and
///   returns the floor deadline in force, checked to be far outside
///   [`WAKE_BOUND`] and unmoved by the acknowledgement; the accepted wake is
///   then required to arrive AND to have its capture COMPLETE before that
///   saved deadline ([`wake_carrying`]). An earlier revision only bounded the
///   park by `WAKE_BOUND`, which does not exclude a floor wake that happens
///   to be due inside it: with the watch's edge consumed, a park scheduled
///   DELIBERATELY to fallback − 150 ms — inside the old 250 ms bound, and
///   modelling permissible descheduling rather than a measured natural delay
///   or a flake rate — let the old assertion accept the floor's wake. (The
///   fallback − 400 ms figure belongs to the quiet control below, whose
///   negative window is 700 ms.)
///
/// The seam's release does not change what a snapshot REPORTS — the interest
/// is a local-root one for another capability — so the accepted wake is
/// qualified against the NODE instead: its immediate read must show the doomed
/// lease gone from the registry. Together with the deadline checks that
/// exclude the fallback, that is the attribution; an unconditional predicate
/// would let the watch's own paced re-derivation satisfy this witness with no
/// placed change at all. The quiet control below establishes that the same
/// park does NOT return inside that bound when nothing changes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_change_landing_inside_a_capture_is_never_lost() {
    let owner = org();
    let consumer = mesh_in_org("c-nolost", &owner, true, None).await;
    let mut observation = watch(&consumer, SensingQuery::new(TAG));

    // One live local interest, owned by this witness, so releasing it is a
    // real movement of this node's observation state. Acquired BEFORE the
    // park is acknowledged, so the acquisition's own generation bump is
    // consumed there rather than being mistaken for the seam's release.
    let doomed = acquire_local_interest(&consumer.node, 0xD0_0Du64);
    let fallback = acknowledge_parked(&mut observation).await;

    // Subscribed after the acknowledgement, so "the generation moved" is a
    // statement about the seam and nothing that preceded it.
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

    // The wake is accepted unconditionally, and NOTHING is asserted about the
    // skip count: with an unconditional predicate the first wake is always
    // accepted, so a `skipped == 0` assertion could not fail and would not be
    // evidence. What this witness rests on is `wake_carrying`'s per-park
    // `WAKE_BOUND` and the SAVED fallback deadline, checked on arrival and
    // again after the capture completes — that is what excludes the floor.
    let _ = wake_carrying(
        "a change that landed inside the capture was LOST",
        &mut observation,
        fallback,
        |_| true,
    )
    .await;

    let _ = std::fs::remove_dir_all(&consumer.dir);
}

/// The control for the witness above: with NOTHING changing, the same park
/// does not return inside a QUALIFIED quiet window.
///
/// Without this, an always-immediate `changed()` would satisfy the lost-wake
/// witness vacuously.
///
/// The window is qualified by the floor deadline actually in force. The
/// pacing floor is WIDENED first ([`SensingWatch::set_population_floor_for_test`],
/// which re-arms relative to the last convergence, not to a fresh `now`) and
/// the remaining margin is then READ and asserted to exceed the negative
/// window. A snapshot on an already-installed, unexpired demand does not
/// re-derive and therefore does not re-arm, so under the production floor the
/// margin left at the park is whatever the setup consumed: the review's
/// discriminator parked DELIBERATELY with ~390 ms left (sleeping to the saved
/// fallback − 400 ms, modelling permissible descheduling — not a measured
/// natural setup delay and not a claim about flake frequency) and the floor
/// legitimately fired inside the old 700 ms negative window. That is an
/// attribution defect in this control, NOT a production wake defect.
///
/// Default-floor liveness is a SEPARATE statement and is kept in its own
/// witness below, so widening the floor here cannot quietly delete it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_quiet_park_does_not_return_inside_the_wake_bound() {
    let owner = org();
    let consumer = mesh_in_org("c-quiet", &owner, true, None).await;
    let mut observation = watch(&consumer, SensingQuery::new(TAG));
    // No providers, no readiness, no discovery movement: nothing can bump the
    // node's change generation.
    let _ = observation.snapshot().expect("snapshot");
    observation.set_population_floor_for_test(Duration::from_secs(120));
    let fallback = observation.fallback_deadline_for_test();
    let margin = fallback.saturating_duration_since(Instant::now());
    assert!(
        margin > QUIET_BOUND.saturating_mul(2),
        "precondition: the widened floor must leave the negative window far \
         inside the fallback, saw {margin:?}"
    );
    assert!(
        tokio::time::timeout(QUIET_BOUND, observation.changed())
            .await
            .is_err(),
        "a quiet network must not produce a wake inside the bound"
    );
    // The floor was the only thing that could have explained a return, and it
    // is still unexpired at the end of the window.
    assert!(
        observation.fallback_deadline_for_test() == fallback && Instant::now() < fallback,
        "the qualified window must have closed before the fallback it was \
         measured against"
    );

    let _ = std::fs::remove_dir_all(&consumer.dir);
}

/// Parking is BOUNDED rather than indefinite: under the production floor, a
/// quiet consumer's park is woken by the population fallback.
///
/// Held separately from the quiet control above, which widens the floor to
/// qualify its negative window. The wake is attributed rather than assumed:
/// on a node with nothing to report, the return must not arrive BEFORE the
/// deadline the watch itself published, so this cannot pass on an immediate
/// or spurious wake.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_population_floor_wakes_a_quiet_parked_consumer() {
    let owner = org();
    let consumer = mesh_in_org("c-floorwake", &owner, true, None).await;
    let mut observation = watch(&consumer, SensingQuery::new(TAG));
    let _ = observation.snapshot().expect("snapshot");
    let fallback = observation.fallback_deadline_for_test();
    tokio::time::timeout(SETTLE, observation.changed())
        .await
        .expect("the population floor must still wake a parked consumer")
        .expect("changed");
    assert!(
        Instant::now() >= fallback,
        "a quiet consumer's wake must be the floor's, not an earlier one"
    );

    let _ = std::fs::remove_dir_all(&consumer.dir);
}

/// TIMED CONTINUITY EXPIRY, isolated from every other cause of `Unknown`, and
/// its NOTIFICATION distinguished from a timer reread.
///
/// The provider stays up, keeps its readiness registration installed, keeps
/// beating and keeps its session — so there is no withdrawal, no replacement
/// beat and no failure-plane disruption. The only thing that moves is the
/// consumer cells' continuity clock, driven at a chosen instant through the
/// SAME pass the maintenance loop runs
/// (`MeshNode::expire_sensing_consumer_cells_for_test`).
///
/// Attribution rests on three things rather than on a wall clock:
///
/// * the park is ACKNOWLEDGED Pending first, and the fallback deadline is read
///   (not assumed) and shown to be far outside the wake bound, so a fallback
///   wake cannot explain the return;
/// * an INDEPENDENT subscriber to the node's change generation shows it
///   advanced across the transition — removing only the expiry publication
///   leaves this unsatisfied even though the projection would still age out on
///   a later read;
/// * the last ADMITTED attestation is still `Ready`, so no replacement or
///   withdrawal produced the `Unknown`, and the session is still live, so the
///   failure plane did not.
///
/// `a_quiet_park_does_not_return_inside_the_wake_bound` remains the no-event
/// control. No exact distributed expiry-instant notification is claimed: the
/// claim is that the expiry publishes a change this consumer is woken by.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn timed_continuity_expiry_publishes_a_wake_and_clears_the_estimate() {
    let owner = org();
    let consumer = mesh_in_org("c-expiry", &owner, true, None).await;
    let audience = shared_audience(&consumer);
    let provider = mesh_in_org("p-expiry", &owner, true, Some(&audience)).await;
    let _service = serve(&provider);
    bring_up(&consumer, &[&provider]).await;
    converge_population(&consumer, &[&provider], 1).await;

    let ready = Arc::new(AtomicBool::new(true));
    // The registration is NEVER closed: a withdrawal is the cause this witness
    // has to exclude.
    let registration = provide(&provider, ready, Duration::from_millis(90));
    let provider_id = provider.node.node_id();

    let mut observation = watch(&consumer, SensingQuery::new(TAG));
    until_snapshot("the provider never read Ready", &mut observation, |snap| {
        snap.provider(provider_id)
            .is_some_and(|p| p.readiness() == ProjectedReadiness::Ready)
    })
    .await;
    // PRODUCTION LINK: the maintenance schedule must itself drive the shared
    // age-and-publish operation. Nothing in this witness has called it yet, so
    // a rising count can only come from the production task — and a build that
    // stopped calling it there (or stopped scheduling the sweep) fails here
    // even though the fixture-driven path below would still work.
    let passes_before = consumer.node.sensing_expiry_passes_for_test();
    until(
        "the maintenance schedule never ran the expiry operation",
        SETTLE,
        || consumer.node.sensing_expiry_passes_for_test() > passes_before,
    )
    .await;

    let fallback = acknowledge_parked(&mut observation).await;

    // An INDEPENDENT view of the node's change generation, marked caught up
    // AFTER the acknowledgement, so only what follows can move it.
    let mut generation = consumer.node.subscribe_sensing_overlay_changes();
    let _ = generation.borrow_and_update();

    // An UNRELATED publisher fires FIRST, deliberately: a real local-interest
    // release moves this node's change generation before the named event. A
    // witness that accepted any first wake would be satisfied by this one.
    publish_unrelated_change(&consumer.node, 0xE1_A1);

    // Only the continuity clock moves after that, from another task, while
    // this one parks.
    let clock = Arc::clone(&consumer.node);
    let expiry = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(
            clock.expire_sensing_consumer_cells_for_test(
                Instant::now() + Duration::from_secs(3_600)
            ),
            "precondition: the expiry pass must actually move a projection"
        );
    });

    // The accepted wake is the one whose OWN immediate read shows the expiry.
    let skipped = wake_carrying("timed expiry", &mut observation, fallback, |snap| {
        snap.provider(provider_id)
            .is_some_and(|p| p.readiness() == ProjectedReadiness::Unknown)
    })
    .await;
    assert!(
        skipped >= 1,
        "the deliberate unrelated wake must have been observed and SKIPPED, \
         not accepted as the expiry"
    );
    expiry.await.expect("expiry task");
    // The PUBLICATION evidence is the accepted wake itself: a parked
    // `changed()` returns only on a published generation change, and the wake
    // accepted above is the one whose own completed capture showed the expiry
    // inside the observation deadline. The independent subscriber below is
    // therefore only a liveness cross-check — the deliberate unrelated
    // publisher alone would already have set it — and is NOT counted as
    // expiry-specific publication evidence.
    assert!(
        generation
            .has_changed()
            .expect("the generation channel is live"),
        "cross-check: this node's change generation moved at all"
    );

    let snapshot = observation.snapshot().expect("snapshot");
    let sensed = row(&snapshot, provider_id);
    assert_eq!(
        sensed.estimated_start(),
        None,
        "an expired observation carries no start estimate"
    );
    assert_eq!(sensed.viability(), SensedViability::Potential);

    // The excluded causes, asserted rather than assumed.
    assert_eq!(
        observation.last_attested_status_for_test(provider_id),
        Some(net::adapter::net::behavior::sensing::AttestedStatus::Ready),
        "no replacement or withdrawal beat was admitted"
    );
    assert!(
        consumer.node.peer_session_for_test(provider_id).is_some(),
        "the session is still live, so the failure plane did not cause this"
    );
    assert!(
        provider.node.sensing_live_streams() > 0,
        "the provider is still emitting: nothing was withdrawn"
    );
    drop(registration);

    let _ = std::fs::remove_dir_all(&consumer.dir);
    let _ = std::fs::remove_dir_all(&provider.dir);
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

    let open = until_snapshot("the provider never read Ready", &mut unbounded, |snap| {
        snap.provider(provider_id)
            .is_some_and(|p| p.readiness() == ProjectedReadiness::Ready)
    })
    .await;
    assert_eq!(row(&open, provider_id).viability(), SensedViability::Viable);
    assert_eq!(open.preferred(), Some(provider_id));

    // The tight watch is asked for ITS OWN qualified read rather than sampled
    // once: it is a separate watch that was never waited on, and readiness
    // decays between reads, so a bare snapshot here could carry `Unknown` and
    // say nothing about the two verdicts this witness is about.
    let bounded = until_snapshot(
        "the tight watch never read the same signed Ready",
        &mut tight,
        |snap| {
            snap.provider(provider_id)
                .is_some_and(|p| p.readiness() == ProjectedReadiness::Ready)
        },
    )
    .await;
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
    // The second wrapper is asked for its own qualified read: it shares the
    // row, but a bare snapshot taken after the first watch's wait can carry a
    // decayed projection and would then fail on ageing rather than on
    // ownership.
    let shared = until_snapshot(
        "the second wrapper never observed the shared row",
        &mut second_watch,
        |snap| {
            snap.provider(provider_id)
                .is_some_and(|p| p.readiness() == ProjectedReadiness::Ready)
        },
    )
    .await;
    assert_eq!(
        row(&shared, provider_id).readiness(),
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
    // A LONG pacing floor: every "the previous success floor is still
    // unexpired" claim below is then a statement about the schedule, not a
    // race against however long the transition takes.
    observation.set_population_floor_for_test(RECOVERY_FLOOR);
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

    // And it comes back. Recovery legitimately needs a FRESH convergence — the
    // installed demand's authority stamp is stale under the reinstalled
    // authority — so what must be proved is that it is not DELAYED by the
    // previous success floor: the convergence count advances on the first read
    // after re-admission, while that floor is demonstrably still unexpired.
    let convergences_before = observation.convergences_for_test();
    let unexpired_floor = observation.fallback_deadline_for_test();
    consumer
        .node
        .install_node_authority(installed)
        .expect("reinstall authority");
    assert!(
        Instant::now() < unexpired_floor,
        "precondition: the previous success floor has NOT elapsed, so a paced \
         implementation would still be waiting"
    );
    let recovered = observation
        .snapshot()
        .expect("a requalified observer is answered again");
    assert_eq!(
        observation.convergences_for_test(),
        convergences_before + 1,
        "a moved authority bypasses the success floor and ATTEMPTS a fresh \
         convergence at once (the rows below are what say it succeeded)"
    );
    assert!(
        Instant::now() < unexpired_floor,
        "and it did so before that floor would have expired"
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
    // Same widened floor as the fixture transition: the signed ceremony writes
    // to disk, so the window must not depend on that finishing inside a short
    // wall-clock interval.
    observation.set_population_floor_for_test(RECOVERY_FLOOR);
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

    // The FIRST read after re-admission requalifies and re-derives, WHILE the
    // last successful convergence's floor is demonstrably unexpired — so a
    // paced-only implementation would still be waiting and could not produce
    // this result.
    let convergences_before = observation.convergences_for_test();
    let unexpired_floor = observation.fallback_deadline_for_test();
    assert!(
        Instant::now() < unexpired_floor,
        "precondition: the previous success floor has NOT elapsed after the \
         signed ceremony"
    );
    let restored = observation
        .snapshot()
        .expect("re-admission restores the observation on the next read");
    assert!(
        restored.provider(provider_id).is_some(),
        "and it returns the currently authorized population: {restored:?}"
    );
    // ATTEMPTS, precisely: the counter records that a convergence was
    // attempted, not that a publication identity changed. The restored rows
    // above are what say it succeeded.
    assert_eq!(
        observation.convergences_for_test(),
        convergences_before + 1,
        "the moved authority attempted a fresh convergence rather than waiting"
    );
    assert!(
        Instant::now() < unexpired_floor,
        "and it did so before that floor would have expired"
    );

    let _ = std::fs::remove_dir_all(&consumer.dir);
    let _ = std::fs::remove_dir_all(&provider.dir);
}

/// Publish a REAL, UNRELATED organization view movement: a revocation floor
/// for an entity that is neither this consumer nor its provider, through the
/// shipped bundle path. It moves the revocation store's publication
/// generation — which is what a capture's currency recheck detects — without
/// touching anyone's qualification.
fn publish_unrelated_view_movement(node: &Arc<MeshNode>, owner: &OrgKeypair, floor: u32) {
    let stranger = net::adapter::net::identity::EntityKeypair::generate();
    let mut floors = std::collections::BTreeMap::new();
    floors.insert(stranger.entity_id().clone(), floor);
    let bundle = OrgRevocationBundle::try_issue(owner, &floors).expect("bundle");
    node.node_authority()
        .expect("authority")
        .revocation
        .apply_bundle(&bundle)
        .expect("the organization publishes an unrelated floor");
}

/// ONE view movement inside the live-membership capture spends the read's
/// second attempt and still answers — it is not an avoidable refusal.
///
/// The movement is PLACED, not raced: the fixtures capture-window seam fires
/// inside each capture the read performs, at the head of the window the
/// capture's own currency recheck closes, and this hook publishes a real
/// unrelated revocation on the FIRST capture only. So attempt 1 observes a
/// genuinely moved view, the view is then stable, and attempt 2 must succeed.
///
/// Distinct from the pre-existing end-of-attempt retry: the movement lands
/// during the membership capture, which returned `ViewChanged` and — before
/// this repair — exited the loop through `.ok()?` without using attempt 2.
/// The seam counter proves the second attempt really ran, and the returned
/// population proves it answered under a currently qualified view.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_view_movement_in_the_capture_spends_a_retry_and_still_answers() {
    let owner = org();
    let consumer = mesh_in_org("c-vmove", &owner, true, None).await;
    let audience = shared_audience(&consumer);
    let provider = mesh_in_org("p-vmove", &owner, true, Some(&audience)).await;
    let _service = serve(&provider);
    bring_up(&consumer, &[&provider]).await;
    converge_population(&consumer, &[&provider], 1).await;
    let provider_id = provider.node.node_id();

    let mut observation = watch(&consumer, SensingQuery::new(TAG));
    // CONTROL: with no movement placed, this read is qualified and reports the
    // provider — so the refusal below can only come from the movement.
    let baseline = observation.snapshot().expect("baseline");
    assert!(
        baseline.provider(provider_id).is_some(),
        "precondition: the population is visible before any movement: {baseline:?}"
    );

    let captures = Arc::new(AtomicU64::new(0));
    let seen = captures.clone();
    let seam_owner = org();
    let seam_node = Arc::clone(&consumer.node);
    consumer
        .node
        .set_sensing_visibility_capture_seam_for_test(Arc::new(move || {
            if seen.fetch_add(1, Ordering::SeqCst) > 0 {
                // Stabilized: the second attempt sees an unmoving view.
                return;
            }
            publish_unrelated_view_movement(&seam_node, &seam_owner, 11);
        }));
    let answered = observation
        .snapshot()
        .expect("one movement inside the capture must not refuse a qualified observer");
    consumer
        .node
        .clear_sensing_visibility_capture_seam_for_test();
    assert_eq!(
        captures.load(Ordering::SeqCst),
        2,
        "the read must have spent its SECOND attempt, not answered on the first"
    );
    assert!(
        answered.provider(provider_id).is_some(),
        "and the answer is the currently authorized population: {answered:?}"
    );

    let _ = std::fs::remove_dir_all(&consumer.dir);
    let _ = std::fs::remove_dir_all(&provider.dir);
}

/// A view that keeps moving stays BOUNDED and refuses: two attempts, then
/// `ObserverNotQualified` — never a spin, and never a population reported
/// under a view that stopped qualifying it.
///
/// The same seam as the witness above, without the once-only gate: every
/// capture observes a fresh real movement. This is the control that keeps the
/// retry honest — the repair must not have turned a bound into a loop.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_continuously_moving_view_refuses_after_the_bound() {
    let owner = org();
    let consumer = mesh_in_org("c-vspin", &owner, true, None).await;
    let audience = shared_audience(&consumer);
    let provider = mesh_in_org("p-vspin", &owner, true, Some(&audience)).await;
    let _service = serve(&provider);
    bring_up(&consumer, &[&provider]).await;
    converge_population(&consumer, &[&provider], 1).await;

    let mut observation = watch(&consumer, SensingQuery::new(TAG));
    let _ = observation.snapshot().expect("baseline");

    let captures = Arc::new(AtomicU64::new(0));
    let seen = captures.clone();
    let seam_owner = org();
    let seam_node = Arc::clone(&consumer.node);
    consumer
        .node
        .set_sensing_visibility_capture_seam_for_test(Arc::new(move || {
            let n = seen.fetch_add(1, Ordering::SeqCst);
            publish_unrelated_view_movement(&seam_node, &seam_owner, 20 + n as u32);
        }));
    let refusal = observation
        .snapshot()
        .expect_err("a view that never stops moving must not be answered");
    consumer
        .node
        .clear_sensing_visibility_capture_seam_for_test();
    assert_eq!(refusal, SensingError::ObserverNotQualified);
    assert_eq!(
        captures.load(Ordering::SeqCst),
        2,
        "and it must refuse AT the bound: exactly two attempts, no spin"
    );

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
    // ONE read carries the whole claim: the newly authorized provider is in
    // the population, Ready, and RANKED beside the incumbent. A second,
    // independent snapshot could legitimately show it decayed to
    // `Unknown`/`Potential` between the two reads - its cadence is 40 ms - and
    // that is exactly how this witness failed CI run 34517413243.
    let wide = until_snapshot(
        "the newly authorized provider never entered the population as a ranked member",
        &mut observation,
        |snap| {
            snap.provider(second_id)
                .is_some_and(|p| p.readiness() == ProjectedReadiness::Ready)
                && snap.ranked().len() == 2
        },
    )
    .await;
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
    // The departing member must itself be RETAINED, so its later absence is a
    // removal rather than an unretained member that was never there.
    assert!(
        observation
            .retained_providers_for_test()
            .contains(&leaving_id),
        "precondition: the departing member holds a lease of its own: {:?}",
        observation.retained_providers_for_test()
    );
    // SUSPEND the paced re-derivation. From here nothing but the per-read
    // visibility clamp can change which rows are reported, and the
    // convergence count proves it: a clamp-free implementation has no other
    // mechanism to remove the row.
    observation.suspend_convergence_for_test(true);
    let convergences_before = observation.convergences_for_test();

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

    let after = observation.snapshot().expect("snapshot");
    assert_eq!(
        observation.convergences_for_test(),
        convergences_before,
        "no re-derivation ran, so only the per-read clamp can explain the result"
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
    // SUSPEND the paced re-derivation, so the expiry cannot be explained by a
    // convergence that happened to come due while the announcement aged out.
    observation.suspend_convergence_for_test(true);
    let convergences_before = observation.convergences_for_test();

    until(
        "the announcement never expired out of discovery",
        SETTLE,
        || authorized(&consumer).iter().all(|id| *id != ephemeral_node),
    )
    .await;

    let after = observation.snapshot().expect("snapshot");
    assert_eq!(
        observation.convergences_for_test(),
        convergences_before,
        "no re-derivation ran: the expiry left the snapshot through the clamp"
    );
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
    let refused = until_snapshot(
        "the default bound never got an answer",
        &mut default_watch,
        |snap| {
            snap.provider(provider_id)
                .is_some_and(|p| p.readiness() != ProjectedReadiness::Unknown)
        },
    )
    .await;
    let sensed = row(&refused, provider_id);
    assert_eq!(
        sensed.readiness(),
        ProjectedReadiness::NotReady,
        "a provider that cannot start inside the asked bound answers NotReady: {refused:?}"
    );
    assert_eq!(sensed.viability(), SensedViability::NotViable);
    // Keyed by the BOUND, not by recency: the recorder keeps every request it
    // saw, so a second watch's concurrent beat stream cannot be mistaken for
    // this one's.
    let default_ask = asked
        .lock()
        .iter()
        .find(|ask| ask.start_within == Some(DEFAULT_PROVIDER_START_WITHIN))
        .cloned()
        .expect("the default watch really asked the default bound");
    assert_eq!(
        default_ask.capability, TAG,
        "and it asked about this capability, verbatim"
    );
    assert_eq!(
        default_ask.constraints,
        CanonicalConstraints::default().canonical_bytes(),
        "canonical constraints are fixed EMPTY, as documented"
    );

    // 2. ASK the bound the provider can meet. Same provider, same signature
    //    path, different question.
    let mut asking = watch(
        &consumer,
        SensingQuery::new(TAG).start_within(Duration::from_secs(5)),
    );
    let honored = until_snapshot("the asked bound never got a Ready", &mut asking, |snap| {
        snap.provider(provider_id)
            .is_some_and(|p| p.readiness() == ProjectedReadiness::Ready)
    })
    .await;
    let sensed = row(&honored, provider_id);
    assert_eq!(sensed.viability(), SensedViability::Viable);
    assert_eq!(sensed.estimated_start(), Some(Duration::from_secs(3)));
    assert_eq!(honored.preferred(), Some(provider_id));
    assert!(
        asked
            .lock()
            .iter()
            .any(|ask| ask.start_within == Some(Duration::from_secs(5))),
        "the asked bound reached the live evaluator verbatim: {:?}",
        asked.lock()
    );

    // 3. The two watches are INDEPENDENT questions: the default one still
    //    reads NotReady after the generous one succeeded. Asked for its own
    //    qualified read — an aged `Unknown` here is decay, not a rewritten
    //    answer, and would say nothing about independence.
    let still = until_snapshot(
        "the default watch never answered again",
        &mut default_watch,
        |snap| {
            snap.provider(provider_id)
                .is_some_and(|p| p.readiness() != ProjectedReadiness::Unknown)
        },
    )
    .await;
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
    // CONTROL: with nothing moving, the row is Viable and its economics fit —
    // asserted on the read that qualified, since a second read can age the
    // projection out from under the control.
    let control = until_snapshot("the provider never read Ready", &mut observation, |snap| {
        snap.provider(provider_id)
            .is_some_and(|p| p.readiness() == ProjectedReadiness::Ready)
    })
    .await;
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
    // Converge, so the change cursor is caught up and the FALLBACK deadline is
    // a known, freshly re-armed distance away. `snapshot` only re-arms it when
    // it actually converges, which is why the deadline is read rather than
    // assumed.
    let fallback = acknowledge_parked(&mut observation).await;

    // An UNRELATED publisher fires FIRST, deliberately: a real local-interest
    // release moves this node's change generation before the provider edge, so
    // a witness that accepted any first wake would be satisfied by it.
    publish_unrelated_change(&consumer.node, 0xE1_A2);

    // Only then does the edge fire, from another task, while this one parks.
    let edge = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(60)).await;
        ready.store(false, Ordering::Relaxed);
        assert!(
            registration.changed(),
            "the state edge must move a live observation path"
        );
        registration
    });

    // The accepted wake is the one whose OWN immediate read carries the edge;
    // earlier wakes are skipped, and the producer is joined only afterwards,
    // for cleanup, never to give an earlier wake meaning.
    let skipped = wake_carrying("provider edge", &mut observation, fallback, |snap| {
        snap.provider(provider_id)
            .is_some_and(|p| p.readiness() == ProjectedReadiness::NotReady)
    })
    .await;
    assert!(
        skipped >= 1,
        "the deliberate unrelated wake must have been observed and SKIPPED, \
         not accepted as the provider edge"
    );
    let _registration = edge.await.expect("edge task");

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
///
/// SCOPE, precisely: this excludes a replacement or withdrawal beat as the
/// cause, and nothing more. A departing process also breaks the path, so
/// failure-plane disruption is an admissible cause HERE; it is
/// `timed_continuity_expiry_publishes_a_wake_and_clears_the_estimate` that
/// isolates timed expiry from the failure plane and attributes the
/// notification. This witness makes no notification claim: the loss becomes
/// visible eventually, through either the change signal or the fallback.
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
    // The interval a renewal re-authors from lives in the LEASE REGISTRY, so
    // the renewal claim below is pinned against that, not only against a local
    // cell. The node's soft-state horizon is 2s here, so the SDK's fixed
    // cadence clamps to it.
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
    let renewal_key = net::adapter::net::behavior::sensing::SensingLeaseKey::ExactProvider {
        audience: spec.audience,
        interest_digest: spec.interest_digest(),
        provider: provider_id,
    };
    let installed_interval = consumer
        .node
        .sensing_lease_entry_for_test(&renewal_key)
        .expect("the registry holds this watch's interest");
    assert_eq!(
        installed_interval,
        (1, SDK_SAMPLE_INTERVAL),
        "precondition: one holder, at the interval future renewals re-author"
    );

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
        .acquire_sensing_interest_lease(&spec, provider_id, SLOWER_SAMPLE_INTERVAL)
        .expect("the slower holder joins the SDK watch's own interest");
    let branch =
        net::adapter::net::behavior::sensing::ProviderInterestKey::new(spec.key(), provider_id);
    assert!(
        consumer
            .node
            .sensing_lease_holder_installation_for_test(&slower)
            .is_some(),
        "precondition: the raw holder really joined the installation"
    );
    // The EFFECTIVE cadence while both hold it: the strictest, which is the
    // SDK's 2s rather than the raw holder's 4s — in the LEASE REGISTRY, which
    // is what future renewals re-author from, and in the local cell.
    assert_eq!(
        consumer
            .node
            .sensing_lease_entry_for_ticket_for_test(&slower),
        Some((2, SDK_SAMPLE_INTERVAL)),
        "precondition: two registry holders at the strictest installed interval"
    );
    assert_eq!(
        consumer
            .node
            .sensing_consumer_cell_interval_for_test(&branch),
        Some(SDK_SAMPLE_INTERVAL),
        "precondition: the shared cell runs at the strictest interval"
    );
    let installation_before = consumer
        .node
        .sensing_lease_holder_installation_for_test(&slower)
        .expect("precondition: the survivor's installation identity");

    // The stricter SDK owner leaves.
    assert!(observation.close());
    assert_eq!(
        armed(&consumer.node),
        1,
        "release-then-SETTLE: a non-final release must NOT take the live \
         installation's renewal away from the surviving holder"
    );
    // AUTHORITATIVE cadence first: the registry's own installed interval is
    // what a future renewal re-authors from, so a regression that relaxed only
    // the local cell — or only the emitted preview — is caught here. Exactly
    // one holder remains, at the survivor's own 4s.
    until(
        "the survivor's registry cadence never relaxed",
        SETTLE,
        || {
            consumer
                .node
                .sensing_lease_entry_for_ticket_for_test(&slower)
                == Some((1, SLOWER_SAMPLE_INTERVAL))
        },
    )
    .await;
    assert_eq!(
        consumer
            .node
            .sensing_consumer_cell_interval_for_test(&branch),
        Some(SLOWER_SAMPLE_INTERVAL),
        "and the local cell agrees with the registry"
    );
    assert_eq!(
        consumer
            .node
            .sensing_lease_holder_installation_for_test(&slower),
        Some(installation_before),
        "the SAME installation is still held — still owned, still renewed"
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
    assert_eq!(
        consumer
            .node
            .sensing_lease_holder_installation_for_test(&slower),
        None,
        "the survivor's own release really removed its holder"
    );
    assert_eq!(
        consumer
            .node
            .sensing_lease_entry_for_ticket_for_test(&slower),
        None,
        "and the registry entry itself is gone"
    );
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

// ---------------------------------------------------------------------------
// Same-Ready economics move the SDK's own result and rank
// ---------------------------------------------------------------------------

/// Two providers, both staying `Ready`, one publishing a NEW signed start
/// estimate: the SDK must expose the new estimate and reverse `ranked()` and
/// `preferred()` accordingly.
///
/// Nothing here changes readiness, population, authority or route: the only
/// moving input is a semantically valid provider-signed estimate. The paired
/// control reads the same watch twice with nothing moving and requires a
/// stable order, so the reversal cannot be read as ordinary churn.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_signed_estimate_reverses_the_sdk_rank_while_both_stay_ready() {
    let owner = org();
    let consumer = mesh_in_org("c-rank", &owner, true, None).await;
    let audience = shared_audience(&consumer);
    let fast = mesh_in_org("p-rank-1", &owner, true, Some(&audience)).await;
    let slow = mesh_in_org("p-rank-2", &owner, true, Some(&audience)).await;
    let _fast_service = serve(&fast);
    let _slow_service = serve(&slow);
    bring_up(&consumer, &[&fast, &slow]).await;
    converge_population(&consumer, &[&fast, &slow], 2).await;

    let fast_id = fast.node.node_id();
    let slow_id = slow.node.node_id();
    let (fast_registration, fast_start) = provide_adjustable(
        &fast,
        Arc::new(AtomicBool::new(true)),
        Duration::from_millis(100),
    );
    let _slow_registration = provide_adjustable(
        &slow,
        Arc::new(AtomicBool::new(true)),
        Duration::from_millis(600),
    );

    let mut observation = watch(&consumer, SensingQuery::new(TAG));
    let first = until_snapshot(
        "the fixture never ranked both providers",
        &mut observation,
        |snap| {
            snap.ranked().len() == 2
                && snap
                    .provider(fast_id)
                    .is_some_and(|p| p.estimated_start() == Some(Duration::from_millis(100)))
                && snap
                    .provider(slow_id)
                    .is_some_and(|p| p.estimated_start() == Some(Duration::from_millis(600)))
        },
    )
    .await;
    assert_eq!(
        first.preferred(),
        Some(fast_id),
        "the lower signed estimate leads: {first:?}"
    );
    assert_eq!(first.ranked(), &[fast_id, slow_id]);

    // CONTROL: nothing moves, the order is stable. A SECOND read, requalified
    // on both rows still being ranked — so a row ageing out is not mistaken
    // for the order changing, while a genuine reorder still fails here.
    let again = until_snapshot(
        "the order never re-read with both providers ranked",
        &mut observation,
        |snap| snap.ranked().len() == 2,
    )
    .await;
    assert_eq!(again.ranked(), first.ranked(), "control: a stable order");

    // ONE input moves: the fast provider publishes a much worse start, still
    // `Ready`. State first, then the edge — the notification is a wake.
    fast_start.store(1_500, Ordering::Relaxed);
    assert!(fast_registration.changed());

    let reversed = until_snapshot(
        "the new estimate never reached the consumer as a re-ranked order",
        &mut observation,
        |snap| {
            snap.provider(fast_id)
                .is_some_and(|p| p.estimated_start() == Some(Duration::from_millis(1_500)))
                && snap.ranked().len() == 2
        },
    )
    .await;
    for provider in reversed.providers() {
        assert_eq!(
            provider.readiness(),
            ProjectedReadiness::Ready,
            "both providers must still be Ready: {reversed:?}"
        );
    }
    assert_eq!(
        reversed.ranked(),
        &[slow_id, fast_id],
        "the SDK's rank must follow the new signed economics: {reversed:?}"
    );
    assert_eq!(reversed.preferred(), Some(slow_id));
    assert_eq!(
        row(&reversed, fast_id).estimated_start(),
        Some(Duration::from_millis(1_500)),
        "and it exposes the estimate it ranked with"
    );

    let _ = std::fs::remove_dir_all(&consumer.dir);
    let _ = std::fs::remove_dir_all(&fast.dir);
    let _ = std::fs::remove_dir_all(&slow.dir);
}

// ---------------------------------------------------------------------------
// Consumer-boundary negatives: the granted plane, and partial acquisition
// ---------------------------------------------------------------------------

/// A provider discovered ONLY under a held same-organization DISCOVER grant —
/// positively announced on the granted plane and positively pinned — is never
/// sensed through the SDK watch, while the owner-plane provider beside it is.
///
/// The grant is real (issued by this organization to itself, DISCOVER+INVOKE,
/// its audience installed on the consumer node) and the announcement is a real
/// `build_granted` envelope through the ordinary verified ingest. Only its
/// discovery PROVENANCE separates it: an owner-org predicate cannot see this,
/// because the grant-plane provider is same-org too.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_granted_only_provider_is_never_sensed_and_the_owner_one_is() {
    use net::adapter::net::behavior::org_scoped_ann::ScopedCapabilityAnnouncement;
    use net::adapter::net::identity::EntityKeypair;
    use net_sdk::org::types::{GrantRights, GrantTargetScope, OrgCapabilityGrant};

    let owner = org();
    let consumer = mesh_in_org("c-grant", &owner, true, None).await;
    let audience = shared_audience(&consumer);
    let provider = mesh_in_org("p-grant", &owner, true, Some(&audience)).await;
    let _service = serve(&provider);
    bring_up(&consumer, &[&provider]).await;
    converge_population(&consumer, &[&provider], 1).await;
    let provider_id = provider.node.node_id();

    let (grant, secret) = OrgCapabilityGrant::try_issue(
        &owner,
        owner.org_id(),
        capability(),
        GrantRights::DISCOVER.union(GrantRights::INVOKE),
        GrantTargetScope::AnyNodeOwnedBy(owner.org_id()),
        3600,
    )
    .expect("issue the same-organization grant");
    let secret = secret.expect("a DISCOVER grant carries audience material");
    let grant_id = grant.grant_id;
    let audience_handle = secret.audience_handle;
    let discovery_key = *secret.discovery_key();
    consumer
        .node
        .install_consumer_grant_audience(grant.clone(), secret)
        .expect("install the consumer grant audience");

    let granted = EntityKeypair::generate();
    let granted_id = granted.entity_id().node_id();
    let cert = OrgMembershipCert::try_issue(&owner, granted.entity_id().clone(), 1, 3600)
        .expect("membership");
    let descriptor = declared().to_bytes_compact();
    let envelope = ScopedCapabilityAnnouncement::build_granted(
        &granted,
        owner.org_id(),
        cert,
        grant_id,
        audience_handle,
        &discovery_key,
        1,
        unix_now() + 3600,
        &descriptor,
    )
    .expect("granted envelope");
    consumer
        .node
        .ingest_scoped_announcement_for_test(&envelope.to_bytes());
    consumer
        .node
        .test_pin_peer_entity(granted_id, granted.entity_id().clone());

    // NONVACUOUS: it really is on the granted plane, and really is not on the
    // owner one.
    until(
        "the granted-plane provider was never discovered",
        SETTLE,
        || {
            consumer
                .node
                .org_cold_discovery(&capability(), &[grant_id])
                .map(|capture| {
                    capture
                        .granted_providers(&grant_id)
                        .iter()
                        .any(|row| row.provider == *granted.entity_id())
                })
                .unwrap_or(false)
        },
    )
    .await;
    assert!(
        authorized(&consumer).iter().all(|id| *id != granted_id),
        "precondition: owner-private discovery never saw it"
    );

    let ready = Arc::new(AtomicBool::new(true));
    let _registration = provide(&provider, ready, Duration::from_millis(75));
    let mut observation = watch(&consumer, SensingQuery::new(TAG));
    until_snapshot(
        "the owner-plane provider never read Ready",
        &mut observation,
        |snap| {
            snap.provider(provider_id)
                .is_some_and(|p| p.readiness() == ProjectedReadiness::Ready)
        },
    )
    .await;

    let snapshot = observation.snapshot().expect("snapshot");
    assert!(
        snapshot.provider(granted_id).is_none(),
        "a grant-plane-only provider must never be sensed: {snapshot:?}"
    );
    assert!(
        snapshot.ranked().iter().all(|id| *id != granted_id),
        "and it must not appear in the rank order either"
    );
    // OWNER-POSITIVE SURVIVOR: the exclusion is a decision, not an empty set.
    assert_eq!(
        snapshot
            .providers()
            .iter()
            .map(|p| p.node_id())
            .collect::<Vec<_>>(),
        vec![provider_id],
        "the owner-plane provider is still sensed: {snapshot:?}"
    );
    assert!(
        observation
            .retained_providers_for_test()
            .iter()
            .all(|id| *id != granted_id),
        "and nothing was even retained for it"
    );

    let _ = std::fs::remove_dir_all(&consumer.dir);
    let _ = std::fs::remove_dir_all(&provider.dir);
}

/// PARTIAL acquisition: with room for exactly one more interest, a watch over
/// two authorized providers retains one and is refused the other. The retained
/// one is a live observation, the refused one reads `Unknown`, and closing the
/// watch cleans up ONLY its own acquired state — a second owner of the same
/// interest keeps its holder.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_partial_acquisition_keeps_its_own_state_and_another_owners() {
    let owner = org();
    let consumer = mesh_in_org("c-partial", &owner, true, None).await;
    let audience = shared_audience(&consumer);
    let first = mesh_in_org("p-partial-1", &owner, true, Some(&audience)).await;
    let second = mesh_in_org("p-partial-2", &owner, true, Some(&audience)).await;
    let _first_service = serve(&first);
    let _second_service = serve(&second);
    bring_up(&consumer, &[&first, &second]).await;
    converge_population(&consumer, &[&first, &second], 2).await;

    let _first_readiness = provide(
        &first,
        Arc::new(AtomicBool::new(true)),
        Duration::from_millis(75),
    );
    let _second_readiness = provide(
        &second,
        Arc::new(AtomicBool::new(true)),
        Duration::from_millis(75),
    );
    let population = authorized(&consumer);
    assert_eq!(
        population.len(),
        2,
        "precondition: two authorized providers"
    );

    // Leave room for EXACTLY ONE more interest, so the watch's second
    // acquisition meets the production capacity refusal while its first
    // succeeds.
    let mut fillers = fill_sensing_capacity(&consumer.node);
    let freed = fillers.pop().expect("one filler to free");
    consumer
        .node
        .try_release_sensing_interest_lease(freed)
        .expect("free exactly one slot");

    let mut observation = watch(&consumer, SensingQuery::new(TAG));
    let retained = observation.retained_providers_for_test();
    assert_eq!(
        retained.len(),
        1,
        "precondition: exactly one member was acquired and one refused: \
         retained {retained:?} of {population:?}"
    );
    let acquired = retained[0];
    let refused = population
        .iter()
        .copied()
        .find(|id| *id != acquired)
        .expect("the refused member");

    let snapshot = observation.snapshot().expect("snapshot");
    assert_eq!(
        snapshot.providers().len(),
        2,
        "both authorized members are still reported: {snapshot:?}"
    );
    assert_eq!(
        row(&snapshot, refused).readiness(),
        ProjectedReadiness::Unknown,
        "the refused member has no evidence: {snapshot:?}"
    );
    assert_eq!(
        row(&snapshot, refused).viability(),
        SensedViability::Potential,
        "and refusal is not a verdict"
    );
    until_snapshot(
        "the acquired member never read Ready",
        &mut observation,
        |snap| {
            snap.provider(acquired)
                .is_some_and(|p| p.readiness() == ProjectedReadiness::Ready)
        },
    )
    .await;
    assert_eq!(
        armed(&consumer.node),
        1,
        "one acquisition, one armed record"
    );

    // A SECOND owner of the acquired member's exact interest.
    let org_id = consumer
        .node
        .node_authority()
        .expect("authority")
        .owner_org();
    let spec = net::adapter::net::behavior::org_sensing_demand::exact_provider_spec(
        TAG,
        net::adapter::net::behavior::sensing::canonical_org_sensing_commitment(&org_id),
        net::adapter::net::behavior::org_sensing_demand::fixed_work_latency(),
        acquired,
    );
    let other_owner = consumer
        .node
        .acquire_sensing_interest_lease(&spec, acquired, SDK_SAMPLE_INTERVAL)
        .expect("a second owner joins the acquired interest");

    // The watch closes: only ITS state goes.
    assert!(observation.close());
    assert!(
        consumer
            .node
            .sensing_lease_holder_installation_for_test(&other_owner)
            .is_some(),
        "the other owner's holder survives the partial watch's close"
    );
    assert!(
        consumer.node.sensing_observation_count() >= 1,
        "and so does the row it keeps referenced"
    );

    consumer
        .node
        .try_release_sensing_interest_lease(other_owner)
        .expect("the other owner releases");
    until(
        "the final release never deregistered the row",
        SETTLE,
        || consumer.node.sensing_observation_count() == 0,
    )
    .await;
    until(
        "the final release never settled the refresh",
        SETTLE,
        || armed(&consumer.node) == 0,
    )
    .await;
    for ticket in fillers {
        let _ = consumer.node.try_release_sensing_interest_lease(ticket);
    }

    let _ = std::fs::remove_dir_all(&consumer.dir);
    let _ = std::fs::remove_dir_all(&first.dir);
    let _ = std::fs::remove_dir_all(&second.dir);
}
