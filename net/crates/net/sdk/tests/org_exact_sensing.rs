//! OA-6 — the PRODUCTION organization exact-sensing connection.
//!
//! This is an out-of-crate integration test, so everything it touches is the
//! shipped surface: `Mesh::org`, `OrgClient::call`, `Mesh::serve_org`,
//! `Mesh::sensing().provide(..)`. There is no fixture assembly here, no
//! injected order, and no substituted selector — the sensed order these
//! witnesses observe is the one `plan_attempt` actually computed on the call
//! path, and the invocation is the one `MeshNode::call` actually admitted.
//!
//! What these witnesses hold:
//!
//! * **the real call consumes the sensed order** — both the viable/pruned
//!   split and the RANK inside the viable set decide which provider the
//!   protected invocation reaches;
//! * **the order is request-relative** — the same fixture reverses its choice
//!   under a deadline that puts the sensed provider over budget;
//! * **churn reconciles** — a provider discovered, or departed, after the
//!   first call enters or leaves the warmed population, and a holder a
//!   transient capacity refusal could not take is recovered later;
//! * **evidence ages** — a projection asked at a later instant stops vouching,
//!   proved by ASKING rather than sleeping, and withdrawal reads `Unknown`
//!   rather than not-ready;
//! * **the binding owns the acquisition** — one mint per bind shared by every
//!   clone, an intermediate drop retires nothing, the last retires everything,
//!   a refused mint binds INERT and never re-mints;
//! * **every degradation is the deterministic order** — never an error;
//! * **the SDK adapter adds no ordering structure** — a source guard.
//!
//! Note on features: this crate's dev-dependency on `net-mesh` enables
//! `fixtures`, so nothing here may be read as a fixtures-OFF build proof. The
//! production path's own independence from that feature is a BUILD fact, shown
//! by `cargo check -p net-mesh-sdk --lib` (no dev-dependencies, `fixtures`
//! off), not by this file.
//!
//! The binding's ownership and reconciliation are observed through
//! `OrgClient`'s `#[doc(hidden)]`, test/fixtures-only DATA accessors — hence
//! this file's `fixtures` gate. They hand out no family handle, so this file
//! can never hold an owner of the acquisition it is asserting about.
#![cfg(all(feature = "net", feature = "cortex", feature = "fixtures"))]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::behavior::sensing::{
    CanonicalConstraints, CapabilityId, ConsumerLatencyBudget, DisclosureClass, EvaluationRequest,
    Incarnation, InterestSpec, ProviderSelector, ReadinessEvaluation, ReadinessEvaluator,
    ResultMode, SensingLeaseTicket, WorkLatencyEnvelope,
};
use net::adapter::net::{ChannelConfigRegistry, MeshNode, MeshNodeConfig};
use net_sdk::identity::Identity;
use net_sdk::mesh::Mesh;
use net_sdk::mesh_rpc::ServeHandle;
use net_sdk::org::types::{
    CapabilityAuthorityId, DispatcherScope, NodeAuthority, OrgDispatcherGrant, OrgKeypair,
    OrgMembershipCert, OwnerAudienceCredential,
};
use net_sdk::org::{OrgAccess, OrgCaller, OrgClient, OrgCredentials};
use net_sdk::sensing::ReadinessRegistration;

const SERVICE: &str = "internal.reindex";
const TAG: &str = "nrpc:internal.reindex";
const SETTLE: Duration = Duration::from_secs(10);

fn org() -> OrgKeypair {
    OrgKeypair::from_bytes([0x51u8; 32])
}

fn capability() -> CapabilityAuthorityId {
    CapabilityAuthorityId::for_tag(TAG)
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Ping {
    n: u32,
}

#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct Pong {
    served_by: String,
}

/// A real readiness evaluator: one provider answers Ready with an estimate,
/// the other answers NotReady. Both are genuine provider-side decisions the
/// sensing plane signs and ships; neither is a test hook.
struct Answer {
    ready: bool,
    /// The provider's own estimate of time-to-start. It is what makes one
    /// viable provider rank ahead of another.
    start: Duration,
}

impl ReadinessEvaluator for Answer {
    fn evaluate(&self, _request: &EvaluationRequest<'_>) -> ReadinessEvaluation {
        if self.ready {
            ReadinessEvaluation::Ready {
                estimated_start: Some(self.start),
            }
        } else {
            ReadinessEvaluation::NotReady { reason: 7 }
        }
    }
}

/// A mesh in `owner`'s organization, wrapped through the public
/// `Mesh::from_node_arc` seam.
///
/// `sensing` turns the node's sensing plane on; `audience` shares the
/// organization's owner discovery audience so independently adopted nodes can
/// open each other's owner-scoped announcements.
/// One organization member: the public `Mesh` facade, the `Arc<MeshNode>` this
/// witness observes through (`Mesh::node` is crate-private, and an
/// out-of-crate witness is the point), its identity, and its authority dir.
struct Member {
    mesh: Mesh,
    node: Arc<MeshNode>,
    identity: Identity,
    dir: std::path::PathBuf,
}

async fn mesh_in_org(
    tag: &str,
    owner: &OrgKeypair,
    sensing: bool,
    audience: Option<&OwnerAudienceCredential>,
) -> Member {
    mesh_in_org_with(tag, owner, sensing, audience, Duration::from_secs(10)).await
}

/// [`mesh_in_org`] with an explicit session timeout — the departure witness
/// needs a peer that really leaves when its process stops, and the timeout is
/// the supported mechanism for that.
async fn mesh_in_org_with(
    tag: &str,
    owner: &OrgKeypair,
    sensing: bool,
    audience: Option<&OwnerAudienceCredential>,
    session_timeout: Duration,
) -> Member {
    let identity = Identity::generate();
    let mut cfg = MeshNodeConfig::new("127.0.0.1:0".parse().expect("addr"), [0x51u8; 32])
        .with_heartbeat_interval(Duration::from_millis(100))
        .with_session_timeout(session_timeout);
    cfg.min_announce_interval = Duration::from_millis(50);
    cfg.configured_identity = true;
    if sensing {
        cfg = cfg
            .with_sensing_coalescing(true)
            .with_sensing_incarnation(Incarnation::new(1));
    }

    let mut node = MeshNode::new((**identity.keypair()).clone(), cfg)
        .await
        .expect("MeshNode::new");
    let channel_configs = Arc::new(ChannelConfigRegistry::new());
    node.set_channel_configs(channel_configs.clone());
    let node = Arc::new(node);

    let entity = identity.entity_id().clone();
    let cert = OrgMembershipCert::try_issue(owner, entity.clone(), 1, 3600).expect("cert");
    let dir = std::env::temp_dir().join(format!(
        "net-oa6-{tag}-{}-{:?}",
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

    let mesh = Mesh::from_node_arc(node.clone(), channel_configs, Some(identity.clone()));
    Member {
        mesh,
        node,
        identity,
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

/// Handshake the consumer to every provider, start everyone, and wait for the
/// entity pins in both directions. Protected RPC is direct-session-only, so
/// the pins are a precondition, not a nicety.
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

/// Serve the protected capability, naming the server in every reply.
fn serve(member: &Member, name: &'static str, calls: Arc<AtomicUsize>) -> ServeHandle {
    member
        .mesh
        .serve_org(
            SERVICE,
            OrgAccess::SameOrg,
            move |_caller: OrgCaller, _req: Ping| {
                let calls = calls.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(Pong {
                        served_by: name.to_string(),
                    })
                }
            },
        )
        .expect("serve_org")
}

/// Serve REAL readiness for the capability, through the shipped provider verb.
/// [`provide`] with an explicit time-to-start estimate.
fn provide_at(member: &Member, ready: bool, start: Duration) -> ReadinessRegistration {
    member
        .mesh
        .sensing()
        .expect("the provider sensing surface binds")
        .provide(CapabilityId::new(TAG), Arc::new(Answer { ready, start }))
        .expect("provide readiness")
}

/// Bind a same-organization credential set through the public verb.
fn bind(member: &Member) -> OrgClient {
    let entity = member.identity.entity_id().clone();
    let cert = OrgMembershipCert::try_issue(&org(), entity.clone(), 1, 3600).expect("membership");
    let dispatcher =
        OrgDispatcherGrant::try_issue(&org(), entity, DispatcherScope::Exact(capability()), 3600)
            .expect("dispatcher grant");
    let credentials = OrgCredentials::new(cert, dispatcher, vec![], vec![]).expect("credentials");
    member.mesh.org(credentials).expect("bind")
}

/// The consumer's authorized SameOrg population: VERIFIED owner-private
/// discovery intersected with this node's entity pins. Exactly what authorizes
/// a candidate, and what sensing may reorder but never extend.
fn population(consumer: &Member) -> Vec<u64> {
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

/// Re-announce until the consumer resolves `wanted` authorized providers.
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
        if population(consumer).len() >= wanted {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "owner-private discovery did not resolve {wanted} providers"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Order the two providers the way DETERMINISTIC selection does — lowest
/// provider entity id, byte-wise — and return `(low, high)`.
fn by_entity_order<'a>(a: &'a Member, b: &'a Member) -> (&'a Member, &'a Member) {
    if a.node.entity_id().as_bytes() < b.node.entity_id().as_bytes() {
        (a, b)
    } else {
        (b, a)
    }
}

/// Fill this node's sensing-interest lease capacity, so every subsequent
/// exact-provider acquisition is refused at the real capacity boundary.
///
/// Uses only the shipped node verbs, and the interests are this node's OWN
/// owner-rooted ones for provider ids nothing else uses — so the refusal the
/// acquisition path meets is the production `NodeAtCapacity` refusal, not a
/// substituted error.
fn fill_sensing_capacity(node: &Arc<MeshNode>) -> Vec<SensingLeaseTicket> {
    let audience = node.sensing_local_root();
    let mut tickets = Vec::new();
    for provider in 1..=512u64 {
        let target = u64::MAX - provider;
        let spec = InterestSpec {
            capability_id: CapabilityId::new("capacity.filler"),
            constraints: CanonicalConstraints::default(),
            work_latency: WorkLatencyEnvelope::start_within(Duration::from_secs(2)),
            providers: ProviderSelector::Node(target),
            result_mode: ResultMode::Any,
            disclosure_class: DisclosureClass::Owner,
            audience,
        };
        match node.acquire_sensing_interest_lease(&spec, target, Duration::from_secs(2)) {
            Ok(ticket) => tickets.push(ticket),
            // Capacity reached: that is the point.
            Err(_) => break,
        }
    }
    assert!(
        !tickets.is_empty(),
        "precondition: the filler leases must actually install"
    );
    tickets
}

/// Release the capacity fillers.
fn release_sensing_capacity(node: &Arc<MeshNode>, tickets: Vec<SensingLeaseTicket>) {
    for ticket in tickets {
        let _ = node.try_release_sensing_interest_lease(ticket);
    }
}

/// This capability's retained population, holders and demand identity.
fn demand_state(client: &OrgClient) -> Option<(Vec<u64>, Vec<u64>, usize)> {
    client
        .sensing_demand_state(&capability())
        .map(|(mut population, mut retained, identity)| {
            population.sort_unstable();
            retained.sort_unstable();
            (population, retained, identity)
        })
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

// ---------------------------------------------------------------------------
// One shared cell: N same-organization providers of one capability
// ---------------------------------------------------------------------------

/// A stood-up cell: the consumer's bound client, and providers ORDERED the way
/// deterministic selection orders them (ascending provider entity id), so
/// `providers[0]` is always the one the unsensed path would take.
struct Cell {
    consumer: Member,
    providers: Vec<Member>,
    client: OrgClient,
    calls: Vec<Arc<AtomicUsize>>,
    names: Vec<&'static str>,
    _serves: Vec<ServeHandle>,
    ready: Vec<ReadinessRegistration>,
}

const NAMES: [&str; 3] = ["first", "second", "third"];

impl Cell {
    /// Stand up `readiness.len()` providers, all serving the protected
    /// capability. `readiness[i]` applies to the provider at DETERMINISTIC
    /// index `i` — index 0 being the one selection takes without sensing.
    ///
    /// `sensing` is the consumer's plane; `bind_later` leaves the client
    /// unbound until the caller binds it (used by the inert witness).
    async fn stand_up(tag: &str, sensing: bool, readiness: &[(bool, Duration)]) -> Self {
        Self::stand_up_with(tag, sensing, readiness, Duration::from_secs(10)).await
    }

    /// [`Self::stand_up`] with an explicit consumer session timeout.
    async fn stand_up_with(
        tag: &str,
        sensing: bool,
        readiness: &[(bool, Duration)],
        session_timeout: Duration,
    ) -> Self {
        let owner = org();
        let mut members: Vec<Member> = Vec::new();
        let mut audience: Option<OwnerAudienceCredential> = None;
        for index in 0..readiness.len() {
            let member =
                mesh_in_org(&format!("{tag}-p{index}"), &owner, true, audience.as_ref()).await;
            if audience.is_none() {
                audience = Some(shared_audience(&member));
            }
            members.push(member);
        }
        let consumer = mesh_in_org_with(
            &format!("{tag}-c"),
            &owner,
            sensing,
            audience.as_ref(),
            session_timeout,
        )
        .await;
        let refs: Vec<&Member> = members.iter().collect();
        bring_up(&consumer, &refs).await;

        // Deterministic order: ascending provider entity id.
        members.sort_by(|a, b| {
            a.node
                .entity_id()
                .as_bytes()
                .cmp(b.node.entity_id().as_bytes())
        });

        let mut calls = Vec::new();
        let mut serves = Vec::new();
        let mut ready = Vec::new();
        let mut names = Vec::new();
        for (index, member) in members.iter().enumerate() {
            let counter = Arc::new(AtomicUsize::new(0));
            let name = NAMES[index];
            serves.push(serve(member, name, counter.clone()));
            let (is_ready, start) = readiness[index];
            ready.push(provide_at(member, is_ready, start));
            calls.push(counter);
            names.push(name);
        }

        let client = bind(&consumer);
        let refs: Vec<&Member> = members.iter().collect();
        converge_population(&consumer, &refs, members.len()).await;
        Self {
            consumer,
            providers: members,
            client,
            calls,
            names,
            _serves: serves,
            ready,
        }
    }

    fn id(&self, index: usize) -> u64 {
        self.providers[index].node.node_id()
    }

    fn served(&self, index: usize) -> usize {
        self.calls[index].load(Ordering::SeqCst)
    }

    /// One protected call, returning the name of the handler that ran.
    async fn call(&self) -> String {
        let reply: Pong = self
            .client
            .call(SERVICE, &Ping { n: 1 })
            .await
            .expect("the protected call is admitted");
        reply.served_by
    }

    /// One protected call under an explicit deadline, through the shipped
    /// execution-control seam.
    async fn call_with_deadline(&self, deadline_ms: u64) -> String {
        let body = serde_json::to_vec(&Ping { n: 1 }).expect("encode");
        let reply = self
            .client
            .call_bytes_deadline(SERVICE, bytes::Bytes::from(body), deadline_ms, 0)
            .await
            .expect("the deadline-bounded protected call is admitted");
        let pong: Pong = serde_json::from_slice(&reply).expect("decode");
        pong.served_by
    }

    /// Wait until the projection ranks exactly `viable` and prunes exactly
    /// `pruned`, both as provider node ids.
    async fn await_projection(&self, viable: &[u64], pruned: &[u64]) {
        let client = &self.client;
        let (viable, pruned) = (viable.to_vec(), pruned.to_vec());
        until(
            "the real signed evidence never reached that shape",
            SETTLE,
            || {
                let Some(projection) = client.sensing_projection(
                    &capability(),
                    Instant::now(),
                    &ConsumerLatencyBudget::default(),
                ) else {
                    return false;
                };
                projection.viable() == viable.as_slice()
                    && projection.non_viable() == pruned.as_slice()
            },
        )
        .await;
    }

    /// Drive real production calls until `probe` holds. Only a call may
    /// reconcile, so the loop's body IS the mechanism under test.
    async fn call_until(&self, what: &str, deadline: Duration, mut probe: impl FnMut() -> bool) {
        let end = Instant::now() + deadline;
        loop {
            let _ = self.call().await;
            if probe() {
                return;
            }
            assert!(Instant::now() < end, "{what}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    fn cleanup(&self) {
        let _ = std::fs::remove_dir_all(&self.consumer.dir);
        for provider in &self.providers {
            let _ = std::fs::remove_dir_all(&provider.dir);
        }
    }
}

// ---------------------------------------------------------------------------
// The real production call consumes the sensed order
// ---------------------------------------------------------------------------

/// Two authorized, pinned, discovered providers whose real evaluators
/// disagree: the one deterministic selection takes answers NotReady, the other
/// answers Ready. The protected invocation must reach the SENSED one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_production_call_lands_on_the_sensed_provider() {
    let cell = Cell::stand_up(
        "sensed",
        true,
        &[
            (false, Duration::from_millis(1)),
            (true, Duration::from_millis(1)),
        ],
    )
    .await;

    // The first call arms the acquisition; its own order is whatever evidence
    // existed at that instant.
    let _armed = cell.call().await;
    cell.await_projection(&[cell.id(1)], &[cell.id(0)]).await;

    let before = (cell.served(0), cell.served(1));
    assert_eq!(
        cell.call().await,
        cell.names[1],
        "the call must reach the SENSED provider, not the deterministic first one"
    );
    assert_eq!(cell.served(1) - before.1, 1);
    assert_eq!(
        cell.served(0) - before.0,
        0,
        "the provider deterministic selection would have taken never ran"
    );

    // Sensing REORDERS an authorized list; it does not shrink one.
    let mut both = vec![cell.id(0), cell.id(1)];
    both.sort_unstable();
    assert_eq!(
        population(&cell.consumer),
        both,
        "a deprioritized provider stays authorized"
    );
    cell.cleanup();
}

/// Both providers viable, differing only in the estimate they report. The call
/// follows the RANK — so treating the ranked list as an unordered set fails
/// here even though the pruning witness above would still pass.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_production_call_follows_the_sensed_rank_between_viable_providers() {
    let cell = Cell::stand_up(
        "rank",
        true,
        &[
            (true, Duration::from_millis(400)),
            (true, Duration::from_millis(1)),
        ],
    )
    .await;
    let _armed = cell.call().await;
    cell.await_projection(&[cell.id(1), cell.id(0)], &[]).await;

    let before = (cell.served(0), cell.served(1));
    assert_eq!(
        cell.call().await,
        cell.names[1],
        "the call must follow the sensed RANK, not the candidate list's order"
    );
    assert_eq!(cell.served(1) - before.1, 1);
    assert_eq!(cell.served(0) - before.0, 0);
    cell.cleanup();
}

// ---------------------------------------------------------------------------
// The order is REQUEST-RELATIVE: one deadline, one different answer
// ---------------------------------------------------------------------------

/// The same fixture, three calls, one difference: a deadline.
///
/// With no deadline the budget is unbounded and the sensed rank decides. With
/// a deadline below what EVERY provider reports as its own time-to-start,
/// nothing is viable — each is DEMOTED to potential, never pruned and never an
/// error — so the deterministic order returns. A third call without a deadline
/// gets the sensed rank back, so the budget is per REQUEST rather than sticky.
///
/// Ignoring `deadline_ms` in the planning input would leave all three calls
/// identical, which is exactly what this discriminates.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_deadline_below_every_estimate_falls_back_to_the_deterministic_order() {
    let cell = Cell::stand_up(
        "budget",
        true,
        &[
            (true, Duration::from_millis(400)),
            (true, Duration::from_millis(300)),
        ],
    )
    .await;
    let _armed = cell.call().await;
    // Unbounded: both viable, the cheaper one leading.
    cell.await_projection(&[cell.id(1), cell.id(0)], &[]).await;

    // The same evidence under a 150 ms budget: nothing viable, nothing pruned.
    let bounded = ConsumerLatencyBudget {
        end_to_end_within: Some(Duration::from_millis(150)),
    };
    let projection = cell
        .client
        .sensing_projection(&capability(), Instant::now(), &bounded)
        .expect("demand");
    assert!(
        projection.viable().is_empty(),
        "every provider is over this budget: {projection:?}"
    );
    assert!(
        projection.non_viable().is_empty(),
        "and each is DEMOTED, never pruned: {projection:?}"
    );

    // 1. No deadline: the sensed rank wins.
    let before = (cell.served(0), cell.served(1));
    assert_eq!(cell.call().await, cell.names[1]);
    assert_eq!(cell.served(1) - before.1, 1);

    // 2. A deadline below every estimate: the deterministic order returns,
    //    through the shipped execution-control seam, with no invented error.
    let before = (cell.served(0), cell.served(1));
    assert_eq!(
        cell.call_with_deadline(150).await,
        cell.names[0],
        "the call's own deadline is the budget: an over-budget order falls back"
    );
    assert_eq!(cell.served(0) - before.0, 1);
    assert_eq!(cell.served(1) - before.1, 0);

    // 3. No deadline again, same client: the sensed rank returns.
    let before = (cell.served(0), cell.served(1));
    assert_eq!(cell.call().await, cell.names[1]);
    assert_eq!(cell.served(1) - before.1, 1);
    assert_eq!(
        population(&cell.consumer).len(),
        2,
        "and a budget de-authorizes nobody"
    );
    cell.cleanup();
}

// ---------------------------------------------------------------------------
// Churn: additions, departures, and a refused holder recovering
// ---------------------------------------------------------------------------

/// A provider discovered AFTER the first call must enter the warmed
/// population, under an unchanged security authority.
///
/// This is the reviewer's reproduction, as a witness: the authority stamp does
/// not cover discovery rows or pins, so reuse keyed on it alone froze the
/// population at whatever the first call saw.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_provider_discovered_after_the_first_call_enters_the_population() {
    let cell = Cell::stand_up(
        "grow",
        true,
        &[
            (false, Duration::from_millis(1)),
            (false, Duration::from_millis(1)),
        ],
    )
    .await;
    let _armed = cell.call().await;
    let (population, retained, first_identity) = demand_state(&cell.client).expect("demand");
    let mut two = vec![cell.id(0), cell.id(1)];
    two.sort_unstable();
    assert_eq!(population, two, "precondition: two providers retained");
    assert_eq!(retained, two);

    // A THIRD provider joins the same organization, connects, is discovered,
    // and answers readiness. No authority moves.
    let audience = shared_audience(&cell.providers[0]);
    let third = mesh_in_org("grow-p2", &org(), true, Some(&audience)).await;
    bring_up(&cell.consumer, &[&third]).await;
    let third_calls = Arc::new(AtomicUsize::new(0));
    let _third_serve = serve(&third, "third", third_calls.clone());
    let _third_ready = provide_at(&third, true, Duration::from_micros(1));
    let refs: Vec<&Member> = cell
        .providers
        .iter()
        .chain(std::iter::once(&third))
        .collect();
    converge_population(&cell.consumer, &refs, 3).await;

    // Warmed calls must incorporate it. Nothing else changed.
    let mut three = two.clone();
    three.push(third.node.node_id());
    three.sort_unstable();
    let expected = three.clone();
    cell.call_until(
        "warmed production calls never incorporated the newly discovered provider",
        SETTLE,
        || {
            demand_state(&cell.client)
                .map(|(population, retained, _)| population == expected && retained == expected)
                .unwrap_or(false)
        },
    )
    .await;
    let (_, _, identity) = demand_state(&cell.client).expect("demand");
    assert_ne!(
        identity, first_identity,
        "the reconciled demand is a new retained snapshot, not the frozen one"
    );

    // And the third provider is now ORDERABLE: it is the only one answering
    // Ready, so it is the only viable candidate and the call follows it.
    let third_id = third.node.node_id();
    let mut pruned = vec![cell.id(0), cell.id(1)];
    pruned.sort_unstable();
    cell.await_projection(&[third_id], &pruned).await;
    let before = third_calls.load(Ordering::SeqCst);
    assert_eq!(cell.call().await, "third");
    assert_eq!(third_calls.load(Ordering::SeqCst) - before, 1);

    let _ = std::fs::remove_dir_all(&third.dir);
    cell.cleanup();
}

/// A provider that DEPARTS must leave the warmed population, including when
/// its departure leaves too few candidates for an order to matter.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_departed_provider_leaves_the_warmed_population() {
    let cell = Cell::stand_up_with(
        "shrink",
        true,
        &[
            (true, Duration::from_millis(1)),
            (true, Duration::from_millis(1)),
        ],
        // A short session timeout, so a stopped provider really leaves this
        // consumer's pin set rather than lingering for the default window.
        Duration::from_secs(2),
    )
    .await;
    let _armed = cell.call().await;
    let (retained_population, _, _) = demand_state(&cell.client).expect("demand");
    assert_eq!(
        retained_population.len(),
        2,
        "precondition: two providers retained"
    );

    // The second provider goes away: its session and its pin go with it, so
    // the consumer's own authorized population drops to one.
    net::adapter::Adapter::shutdown(cell.providers[1].node.as_ref())
        .await
        .expect("stop the departing provider");
    let survivor = cell.id(0);
    until(
        "the departed provider never left the authorized population",
        Duration::from_secs(30),
        || population(&cell.consumer) == vec![survivor],
    )
    .await;

    // Warmed calls must narrow the retained demand to the survivor - the
    // one-candidate case must NOT skip reconciliation.
    cell.call_until(
        "warmed production calls never released the departed provider's demand",
        SETTLE,
        || {
            demand_state(&cell.client)
                .map(|(population, retained, _)| {
                    population == vec![survivor] && retained == vec![survivor]
                })
                .unwrap_or(false)
        },
    )
    .await;
    cell.cleanup();
}

/// A holder the acquisition could not take - the node's interest capacity was
/// full - is RECOVERED by a later call once capacity frees, without any
/// authority movement and without a per-call re-acquisition in between.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_capacity_refused_holder_is_recovered_by_a_later_call() {
    let cell = Cell::stand_up(
        "recover",
        true,
        &[
            (true, Duration::from_millis(1)),
            (true, Duration::from_millis(1)),
        ],
    )
    .await;

    // Fill the consumer's interest capacity BEFORE the first call, so every
    // per-provider acquisition meets the real capacity refusal.
    let fillers = fill_sensing_capacity(&cell.consumer.node);
    let _armed = cell.call().await;
    let (population, retained, _) = demand_state(&cell.client).expect("demand");
    assert_eq!(population.len(), 2, "the population is still derived");
    assert!(
        retained.is_empty(),
        "but no holder could be acquired at capacity: {retained:?}"
    );

    // While capacity is still full, repeated calls must not hammer the
    // exhausted allocator: the retry is floored.
    let (_, _, identity) = demand_state(&cell.client).expect("demand");
    for _ in 0..3 {
        let _ = cell.call().await;
    }
    let (_, retained_again, identity_again) = demand_state(&cell.client).expect("demand");
    assert!(retained_again.is_empty());
    assert_eq!(
        identity, identity_again,
        "an incomplete convergence must not be retried on every call"
    );

    // Capacity frees. A later call - past the retry floor - recovers the
    // holders, with no authority movement anywhere.
    release_sensing_capacity(&cell.consumer.node, fillers);
    let mut two = vec![cell.id(0), cell.id(1)];
    two.sort_unstable();
    let expected = two.clone();
    cell.call_until(
        "a later call never recovered the refused holders",
        Duration::from_secs(30),
        || {
            demand_state(&cell.client)
                .map(|(_, retained, _)| retained == expected)
                .unwrap_or(false)
        },
    )
    .await;
    cell.cleanup();
}

// ---------------------------------------------------------------------------
// Aging, and withdrawal
// ---------------------------------------------------------------------------

/// LIVE evidence stops vouching at a later instant.
///
/// Freshness is request-relative, so this is proved POSITIVELY by asking the
/// same retained demand for a projection at an instant past the continuity
/// window - no sleeping, no withdrawal, and no replacement `Unknown`
/// observation that could mask a broken window. The provider is still Ready
/// and still beating throughout.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_evidence_stops_vouching_at_a_later_instant() {
    let cell = Cell::stand_up(
        "aging",
        true,
        &[
            (false, Duration::from_millis(1)),
            (true, Duration::from_millis(1)),
        ],
    )
    .await;
    let _armed = cell.call().await;
    cell.await_projection(&[cell.id(1)], &[cell.id(0)]).await;

    // The SAME demand, the SAME live evidence, one later instant.
    let aged = Instant::now() + Duration::from_secs(600);
    let projection = cell
        .client
        .sensing_projection(&capability(), aged, &ConsumerLatencyBudget::default())
        .expect("demand");
    assert!(
        projection.viable().is_empty(),
        "a Ready attestation must stop vouching once its window has elapsed: {projection:?}"
    );
    assert!(
        projection.non_viable().is_empty(),
        "and an aged-out row is Unknown, not not-ready: {projection:?}"
    );

    // Asking at NOW still ranks it, so the aged answer is about the instant
    // rather than about the evidence having gone away.
    let now = cell
        .client
        .sensing_projection(
            &capability(),
            Instant::now(),
            &ConsumerLatencyBudget::default(),
        )
        .expect("demand");
    assert_eq!(
        now.viable(),
        [cell.id(1)],
        "the live projection is unchanged: {now:?}"
    );
    cell.cleanup();
}

/// WITHDRAWING readiness reads `Unknown`, not not-ready, and planning falls
/// back to the deterministic order.
///
/// This is what withdrawal actually produces: the provider keeps beating and
/// the beats carry no evaluation, which the consumer projects as `Unknown`. It
/// is therefore evidence about withdrawal semantics and about the fallback -
/// NOT about elapsed continuity, which the aging witness above proves.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn withdrawn_readiness_reads_unknown_and_plans_deterministically() {
    let mut cell = Cell::stand_up(
        "withdraw",
        true,
        &[
            (false, Duration::from_millis(1)),
            (true, Duration::from_millis(1)),
        ],
    )
    .await;
    let _armed = cell.call().await;
    cell.await_projection(&[cell.id(1)], &[cell.id(0)]).await;

    // Withdraw on both providers.
    for registration in cell.ready.drain(..) {
        assert!(registration.close(), "readiness is withdrawn");
    }
    cell.await_projection(&[], &[]).await;

    let before = (cell.served(0), cell.served(1));
    assert_eq!(
        cell.call().await,
        cell.names[0],
        "with no verdict for anyone the deterministic order returns"
    );
    assert_eq!(cell.served(0) - before.0, 1);
    assert_eq!(cell.served(1) - before.1, 0);
    assert_eq!(
        population(&cell.consumer).len(),
        2,
        "and withdrawal de-authorizes nobody"
    );
    cell.cleanup();
}

/// Every provider answers a fresh explicit NotReady: an all-pruned order falls
/// back to the caller's own deterministic order, eliminates no candidate, and
/// invents no error.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_all_not_ready_population_still_calls_in_the_deterministic_order() {
    let cell = Cell::stand_up(
        "pruned",
        true,
        &[
            (false, Duration::from_millis(1)),
            (false, Duration::from_millis(1)),
        ],
    )
    .await;
    let _armed = cell.call().await;
    let mut both = vec![cell.id(0), cell.id(1)];
    both.sort_unstable();
    cell.await_projection(&[], &both).await;

    let before = cell.served(0);
    assert_eq!(
        cell.call().await,
        cell.names[0],
        "all-pruned falls back to the deterministic order"
    );
    assert_eq!(cell.served(0) - before, 1);
    assert_eq!(population(&cell.consumer).len(), 2);
    cell.cleanup();
}

// ---------------------------------------------------------------------------
// The binding owns the acquisition
// ---------------------------------------------------------------------------

/// One mint per bind, shared by every clone: an intermediate clone's drop
/// retires nothing, the last owner's drop retires everything down to the
/// node's interest rows, and two binds are independent.
///
/// This witness deliberately holds NO family handle — the observation
/// accessors return data — so the client clones really are the only owners.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_binding_owns_the_acquisition_and_the_last_owner_retires_it() {
    let cell = Cell::stand_up(
        "own",
        true,
        &[
            (true, Duration::from_millis(1)),
            (true, Duration::from_millis(1)),
        ],
    )
    .await;
    assert!(cell.client.sensing_is_active(), "the bind minted a family");
    assert_eq!(
        cell.client.sensing_owners(),
        Some(1),
        "the client is the only owner of its acquisition"
    );
    assert!(
        cell.client.sensing_capabilities().is_empty(),
        "and nothing is retained before a call needs it"
    );

    let _armed = cell.call().await;
    assert_eq!(cell.client.sensing_capabilities(), vec![capability()]);
    let (population, retained, _) = demand_state(&cell.client).expect("demand");
    assert_eq!(population.len(), 2, "one exact interest per provider");
    assert_eq!(retained.len(), 2);
    assert!(
        !cell.consumer.node.sensing_table_is_empty(),
        "the interest is real: the node holds the rows"
    );

    // A clone SHARES the acquisition; it never mints a second one.
    let clone = cell.client.clone();
    assert_eq!(cell.client.sensing_owners(), Some(2));
    assert_eq!(clone.sensing_capabilities(), vec![capability()]);
    let (_, _, identity) = demand_state(&cell.client).expect("demand");
    let (_, _, clone_identity) = demand_state(&clone).expect("demand");
    assert_eq!(
        identity, clone_identity,
        "both clones read the SAME retained demand"
    );

    // An intermediate clone's drop retires nothing.
    drop(clone);
    assert_eq!(cell.client.sensing_owners(), Some(1));
    assert!(
        demand_state(&cell.client).is_some(),
        "a clone's release must not retire a surviving client's demand"
    );
    assert!(!cell.consumer.node.sensing_table_is_empty());
    let _still = cell.call().await;

    // A separate bind mints its OWN acquisition.
    let other = bind(&cell.consumer);
    assert!(other.sensing_capabilities().is_empty());
    assert_eq!(other.sensing_owners(), Some(1));
    assert_eq!(cell.client.sensing_capabilities(), vec![capability()]);
    drop(other);

    // The LAST owner's drop retires everything.
    let Cell {
        consumer,
        providers,
        client,
        ..
    } = cell;
    drop(client);
    let node = Arc::clone(&consumer.node);
    until(
        "the last owner's release must retire every retained interest",
        SETTLE,
        move || node.sensing_table_is_empty(),
    )
    .await;

    let _ = std::fs::remove_dir_all(&consumer.dir);
    for provider in &providers {
        let _ = std::fs::remove_dir_all(&provider.dir);
    }
}

/// A bind whose family mint is REFUSED at the real boundary binds anyway,
/// INERT: it calls deterministically, its clones share the inert state, and it
/// never re-mints on the call path.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_mint_binds_inert_and_never_remints() {
    let owner = org();
    let p0 = mesh_in_org("inert-p0", &owner, true, None).await;
    let audience = shared_audience(&p0);
    let p1 = mesh_in_org("inert-p1", &owner, true, Some(&audience)).await;
    let consumer = mesh_in_org("inert-c", &owner, true, Some(&audience)).await;
    bring_up(&consumer, &[&p0, &p1]).await;

    let (low, high) = by_entity_order(&p0, &p1);
    let low_calls = Arc::new(AtomicUsize::new(0));
    let high_calls = Arc::new(AtomicUsize::new(0));
    let _low_serve = serve(low, "first", low_calls.clone());
    let _high_serve = serve(high, "second", high_calls.clone());
    // The high-id provider is the one a SENSED order would prefer, so an
    // accidentally-active binding would be caught by the assertions below.
    let _low_ready = provide_at(low, false, Duration::from_millis(1));
    let _high_ready = provide_at(high, true, Duration::from_millis(1));

    // Drive the family identity space to its terminal state, so the mint is
    // refused through the production allocator rather than a substitute.
    consumer.node.exhaust_org_routing_families_for_test();
    let refusals_before = consumer.node.org_routing_family_refusals_for_test();

    let client = bind(&consumer);
    assert!(
        !client.sensing_is_active(),
        "a refused mint must bind INERT rather than failing the bind"
    );
    assert_eq!(client.sensing_owners(), None);
    assert!(client.sensing_capabilities().is_empty());
    let clone = client.clone();
    assert!(
        !clone.sensing_is_active(),
        "a clone shares the inert state; it cannot become active on its own"
    );
    assert_eq!(
        consumer.node.org_routing_family_refusals_for_test() - refusals_before,
        1,
        "exactly ONE mint was attempted: at bind"
    );

    converge_population(&consumer, &[&p0, &p1], 2).await;
    for _ in 0..3 {
        let reply: Pong = client
            .call(SERVICE, &Ping { n: 1 })
            .await
            .expect("an inert binding still calls");
        assert_eq!(
            reply.served_by, "first",
            "and it plans the deterministic order"
        );
    }
    let reply: Pong = clone
        .call(SERVICE, &Ping { n: 1 })
        .await
        .expect("the clone calls too");
    assert_eq!(reply.served_by, "first");
    assert_eq!(high_calls.load(Ordering::SeqCst), 0);
    assert_eq!(low_calls.load(Ordering::SeqCst), 4);
    assert_eq!(
        consumer.node.org_routing_family_refusals_for_test() - refusals_before,
        1,
        "no call-path re-mint: four admitted calls, still one attempt"
    );
    assert!(
        consumer.node.sensing_table_is_empty(),
        "an inert binding installs no interest"
    );
    assert!(client.sensing_capabilities().is_empty());

    for member in [&p0, &p1, &consumer] {
        let _ = std::fs::remove_dir_all(&member.dir);
    }
}

// ---------------------------------------------------------------------------
// Direct-session attribution: what `direct` actually means today
// ---------------------------------------------------------------------------

/// `direct` is an IDENTITY PIN, not a live session — a predicate that predates
/// this slice (`peer_entity_id` reads the pin map, and peer eviction removes
/// session state without removing the pin). This witness ESTABLISHES the
/// resulting behaviour rather than asserting a wish: a sensed order that
/// prefers a provider whose process is gone, while its evidence is still
/// fresh, selects that provider, and the call fails at TRANSPORT with no
/// silent fallback to the live one.
///
/// Nothing is manufactured: the pin was established by a real handshake and
/// the session died because the provider really stopped. The behaviour is
/// inherited, it is the same for the unsensed order (which also selects the
/// first PINNED candidate), and correcting it would require a live-session
/// predicate in transport, which this slice is not authorized to add. Recorded
/// here so the attribution is explicit instead of implied.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pinned_but_dead_sensed_provider_is_selected_and_fails_at_transport() {
    let cell = Cell::stand_up(
        "dead",
        true,
        &[
            (false, Duration::from_millis(1)),
            (true, Duration::from_millis(1)),
        ],
    )
    .await;
    let _armed = cell.call().await;
    cell.await_projection(&[cell.id(1)], &[cell.id(0)]).await;

    // The sensed leader's process goes away. Its pin remains, and its last
    // Ready attestation is still inside its window.
    net::adapter::Adapter::shutdown(cell.providers[1].node.as_ref())
        .await
        .expect("stop the sensed provider");
    assert!(
        cell.consumer.node.peer_entity_id(cell.id(1)).is_some(),
        "the identity pin outlives the session - this is the inherited predicate"
    );

    let before = (cell.served(0), cell.served(1));
    // Bounded observation: a send to a dead peer has nothing to answer it, so
    // the deadline is how this witness stays a witness instead of a hang. The
    // budget it implies (1.5 s) is far above every estimate here, so it does
    // not change the order under test.
    let body = serde_json::to_vec(&Ping { n: 1 }).expect("encode");
    let outcome = cell
        .client
        .call_bytes_deadline(SERVICE, bytes::Bytes::from(body), 1500, 0)
        .await
        .and_then(|reply| {
            serde_json::from_slice::<Pong>(&reply).map_err(|e| {
                net_sdk::org::OrgSdkError::Rpc(net_sdk::mesh_rpc::RpcError::Codec {
                    direction: net_sdk::mesh_rpc::CodecDirection::Decode,
                    message: format!("{e}"),
                })
            })
        });
    match outcome {
        Err(net_sdk::org::OrgSdkError::Rpc(_)) => {
            // Established behaviour: selection followed the pin, the send had
            // nowhere to go, and the SDK does not retry or re-select.
            assert_eq!(
                cell.served(0) - before.0,
                0,
                "and no silent fallback to the live provider happened"
            );
        }
        Ok(reply) => {
            // If the transport did complete, it must have been the SELECTED
            // provider - never a silent substitution.
            assert_eq!(
                reply.served_by, cell.names[1],
                "a reply may only come from the selected provider"
            );
        }
        Err(other) => panic!("unexpected local refusal: {other:?}"),
    }
    assert_eq!(
        cell.served(1) - before.1,
        0,
        "the stopped provider served nothing"
    );
    cell.cleanup();
}

// ---------------------------------------------------------------------------
// Unavailable evidence: the deterministic floor
// ---------------------------------------------------------------------------

/// A consumer whose sensing plane is OFF - the shipped default - calls
/// deterministically, holds no interest rows, and does not re-converge per
/// call.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_consumer_without_sensing_plans_the_deterministic_order() {
    let cell = Cell::stand_up(
        "dark",
        false,
        &[
            (false, Duration::from_millis(1)),
            (true, Duration::from_millis(1)),
        ],
    )
    .await;
    assert!(
        !cell.consumer.node.sensing_enabled(),
        "precondition: this consumer's sensing plane is off"
    );

    let mut identity = None;
    for _ in 0..3 {
        assert_eq!(
            cell.call().await,
            cell.names[0],
            "with no evidence the order is the deterministic one"
        );
        let (_, retained, current) = demand_state(&cell.client).expect("demand");
        assert!(
            retained.is_empty(),
            "a dark plane acquires no holder, so there is nothing to order by"
        );
        match identity {
            None => identity = Some(current),
            Some(previous) => assert_eq!(
                previous, current,
                "no per-call re-acquisition: the same demand is reused"
            ),
        }
    }
    assert_eq!(cell.served(1), 0);
    assert_eq!(cell.served(0), 3);
    assert!(
        cell.consumer.node.sensing_table_is_empty(),
        "a dark consumer registers no interest at all"
    );
    let projection = cell
        .client
        .sensing_projection(
            &capability(),
            Instant::now(),
            &ConsumerLatencyBudget::default(),
        )
        .expect("demand");
    assert!(projection.viable().is_empty() && projection.non_viable().is_empty());
    cell.cleanup();
}

// ---------------------------------------------------------------------------
// W-55 — the SDK adapter adds no ordering structure
// ---------------------------------------------------------------------------

/// The ordering RULE lives once, in core, over plain data. The SDK's share is a
/// projection of candidates onto that rule's two slices — so this guard reads
/// the adapter's exact body and requires that it introduces no comparison sort
/// and no ordering structure of its own. A second implementation is what would
/// diverge; this is how the file stays unable to grow one.
///
/// The span is walked by braces from the signature, skipping string and
/// comment spans, and an unbalanced walk FAILS rather than passing.
#[test]
fn the_sdk_sensed_adapter_adds_no_ordering_structure() {
    const FORBIDDEN: &[&str] = &[
        ".sort(",
        ".sort_by(",
        ".sort_by_key(",
        ".sort_by_cached_key(",
        ".sort_unstable(",
        ".sort_unstable_by(",
        ".sort_unstable_by_key(",
        ".binary_search",
        "BinaryHeap",
        "BTreeMap::",
        "BTreeSet::",
        "HashMap::",
        "HashSet::",
    ];
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/org/call.rs"),
    )
    .expect("read the SDK call path");
    let body = brace_body(&source, "fn org_sensed_candidate_permutation(")
        .expect("the adapter exists and its body balances");
    assert!(
        body.contains("org_sensed_bucket_permutation("),
        "the adapter must APPLY the core rule rather than restate it"
    );
    assert!(
        body.lines().filter(|l| !l.trim().is_empty()).count() > 3,
        "anti-vacuity: deleting the adapter's body must fail this guard, not satisfy it"
    );
    for needle in FORBIDDEN {
        assert!(
            !body.contains(needle),
            "the SDK adapter must add no ordering structure, found `{needle}` in:\n{body}"
        );
    }
}

/// The body of the function whose signature starts with `signature`, from the
/// first `{` at or after it to the matching `}`, ignoring braces inside string
/// literals and comments. `None` when the function is absent or the walk fails
/// to balance — both are guard failures, never silent passes.
fn brace_body(source: &str, signature: &str) -> Option<String> {
    let start = source.find(signature)?;
    let bytes = source.as_bytes();
    let open = start + source[start..].find('{')?;
    let mut depth = 0usize;
    let mut index = open;
    let mut in_string = false;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    while index < bytes.len() {
        let byte = bytes[index];
        let next = bytes.get(index + 1).copied();
        if in_line_comment {
            if byte == b'\n' {
                in_line_comment = false;
            }
        } else if in_block_comment {
            if byte == b'*' && next == Some(b'/') {
                in_block_comment = false;
                index += 1;
            }
        } else if in_string {
            if byte == b'\\' {
                index += 1;
            } else if byte == b'"' {
                in_string = false;
            }
        } else {
            match (byte, next) {
                (b'/', Some(b'/')) => {
                    in_line_comment = true;
                    index += 1;
                }
                (b'/', Some(b'*')) => {
                    in_block_comment = true;
                    index += 1;
                }
                (b'"', _) => in_string = true,
                (b'{', _) => depth += 1,
                (b'}', _) => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(source[open + 1..index].to_string());
                    }
                }
                _ => {}
            }
        }
        index += 1;
    }
    None
}
