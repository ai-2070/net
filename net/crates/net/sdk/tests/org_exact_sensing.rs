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
    // `Any` rather than `Exact`: a scheduler-grade dispatcher is a legitimate
    // production credential, and it is what lets a call for an UNKNOWN service
    // reach discovery and the sensed step instead of being refused a step
    // earlier on scope - which is the path the capacity witness needs.
    let dispatcher = OrgDispatcherGrant::try_issue(&org(), entity, DispatcherScope::Any, 3600)
        .expect("dispatcher grant");
    let credentials = OrgCredentials::new(cert, dispatcher, vec![], vec![]).expect("credentials");
    member.mesh.org(credentials).expect("bind")
}

/// [`bind`] for a credential set that also HOLDS `grants` — the granted
/// discovery plane, through the same public verb.
fn bind_with(member: &Member, grants: Vec<net_sdk::org::types::OrgCapabilityGrant>) -> OrgClient {
    let entity = member.identity.entity_id().clone();
    let cert = OrgMembershipCert::try_issue(&org(), entity.clone(), 1, 3600).expect("membership");
    let dispatcher = OrgDispatcherGrant::try_issue(&org(), entity, DispatcherScope::Any, 3600)
        .expect("dispatcher grant");
    // No audience secret: the grant's discovery audience is installed on the
    // NODE (`install_consumer_grant_audience`), which is what makes the
    // granted plane resolvable — the client only needs the grant itself.
    let credentials = OrgCredentials::new(cert, dispatcher, grants, vec![]).expect("credentials");
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
fn demand_state(client: &OrgClient) -> Option<(Vec<u64>, Vec<u64>, u64)> {
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

/// An installed in-section hook that removes itself on drop.
///
/// The hook lives inside the binding it observes and a witness's hook captures
/// a clone of that same binding, so leaving one installed keeps a fixture-only
/// cycle alive — and a failing assertion unwinds past any manual clear. This
/// makes the removal structural instead.
struct SectionHook<'a> {
    client: &'a OrgClient,
}

impl<'a> SectionHook<'a> {
    fn install(client: &'a OrgClient, hook: Arc<dyn Fn() + Send + Sync>) -> Self {
        client.set_sensing_section_hook_for_test(Some(hook));
        Self { client }
    }
}

impl Drop for SectionHook<'_> {
    fn drop(&mut self) {
        self.client.set_sensing_section_hook_for_test(None);
    }
}

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

    /// One protected call whose REPLY is not the point: fixtures that discover
    /// synthetic, unreachable providers can legitimately have a call planned
    /// for one of them and fail at transport. The planning - and the
    /// reconciliation it drives - is what these witnesses assert.
    async fn try_call(&self) -> Result<String, net_sdk::org::OrgSdkError> {
        self.client
            .call::<Ping, Pong>(SERVICE, &Ping { n: 1 })
            .await
            .map(|reply| reply.served_by)
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
            let _ = self.try_call().await;
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
///
/// The deadline is TWO things at once — the planning budget and the call's own
/// wall clock — so the fixture separates them by scale rather than by shaving
/// the margin: the estimates are tens of seconds and the budget is seconds, so
/// "below every estimate" holds by a factor of four while the RPC itself has
/// seconds of headroom. A budget just under the estimates made the loopback
/// round trip race its own deadline, which is a runner-load failure and not
/// the ordering property (exact-head CI, `Rpc(Timeout { elapsed_ms: 147 })`
/// against a 150 ms deadline). Nothing about the rule under test moves: the
/// unbounded calls still rank by estimate, and the bounded one still finds
/// every provider over budget.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_deadline_below_every_estimate_falls_back_to_the_deterministic_order() {
    let cell = Cell::stand_up(
        "budget",
        true,
        &[
            (true, Duration::from_secs(40)),
            (true, Duration::from_secs(30)),
        ],
    )
    .await;
    let _armed = cell.call().await;
    // Unbounded: both viable, the cheaper one leading.
    cell.await_projection(&[cell.id(1), cell.id(0)], &[]).await;

    // The same evidence under a budget below both estimates: nothing viable,
    // nothing pruned.
    let bounded = ConsumerLatencyBudget {
        end_to_end_within: Some(Duration::from_secs(10)),
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
    //    Ten seconds is a quarter of the cheapest estimate and orders of
    //    magnitude above a loopback round trip, so only the ORDER can decide
    //    this.
    let before = (cell.served(0), cell.served(1));
    assert_eq!(
        cell.call_with_deadline(10_000).await,
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
// Bounds: unknown names strand nothing, and the record set is capped
// ---------------------------------------------------------------------------

/// Calls to services that resolve NOTHING must strand no sensing capacity and
/// accumulate no reconciliation state — and a real service must still acquire
/// afterwards, on the SAME binding.
///
/// This is the reviewer's public-call reproduction as a witness: an empty
/// discovery is `Ok(vec![])`, so a capability with no authorized provider used
/// to occupy one of core's per-family capability slots and one record apiece.
/// Sixty-four unknown names later, the real service could no longer acquire on
/// that binding at all, while a fresh binding on the same node could.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unknown_services_strand_no_sensing_capacity() {
    let cell = Cell::stand_up(
        "unknown",
        true,
        &[
            (false, Duration::from_millis(1)),
            (true, Duration::from_millis(1)),
        ],
    )
    .await;

    // A hundred distinct names that nothing serves. Every one is a local
    // refusal - no RPC is admitted - and every one is planned through the
    // production path, sensed step included.
    for index in 0..100u32 {
        let outcome: Result<Pong, _> = cell
            .client
            .call(&format!("absent.service.{index}"), &Ping { n: 1 })
            .await;
        assert!(
            matches!(
                outcome,
                Err(net_sdk::org::OrgSdkError::Discovery(
                    net_sdk::org::OrgDiscoveryError::NoAuthorizedProvider { .. }
                ))
            ),
            "an unknown service is a local refusal: {outcome:?}"
        );
    }
    assert!(
        cell.client.sensing_capabilities().is_empty(),
        "an unknown service acquires nothing, so it retains no demand: {:?}",
        cell.client.sensing_capabilities()
    );
    assert_eq!(
        cell.client.sensing_records(),
        Some(0),
        "and it leaves no reconciliation record behind either"
    );
    assert!(
        cell.consumer.node.sensing_table_is_empty(),
        "nor any interest row on the node"
    );

    // The REAL service, on that same binding, still acquires - the capacity
    // was never spent.
    let _armed = cell.call().await;
    let (population, retained, _) = demand_state(&cell.client).expect("demand");
    let mut two = vec![cell.id(0), cell.id(1)];
    two.sort_unstable();
    assert_eq!(
        population, two,
        "the real capability retains both providers"
    );
    assert_eq!(retained, two, "with a holder each");
    assert_eq!(cell.client.sensing_capabilities(), vec![capability()]);
    assert_eq!(cell.client.sensing_records(), Some(1));

    // And it orders: the sensed provider wins, on the binding that just made a
    // hundred failed calls.
    cell.await_projection(&[cell.id(1)], &[cell.id(0)]).await;
    let before = cell.served(1);
    assert_eq!(cell.call().await, cell.names[1]);
    assert_eq!(cell.served(1) - before, 1);
    cell.cleanup();
}

// ---------------------------------------------------------------------------
// Ownership that dies AFTER a successful convergence
// ---------------------------------------------------------------------------

/// A holder invalidated after a COMPLETE convergence is recovered, with the
/// sensing authority still current and the population unchanged.
///
/// A recorded ticket list is history: an installation can stop being the
/// holder after the fact. Nothing in the provider list or the authority stamp
/// shows that, so the reuse decision asks core the same liveness question its
/// own convergence asks before carrying a ticket. Here the holder is destroyed
/// by RELEASING the ticket out from under the demand — a real release through
/// the shipped node verb, which is exactly the state a refused restoration
/// leaves behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_holder_that_dies_after_convergence_is_reacquired() {
    let cell = Cell::stand_up(
        "ownership",
        true,
        &[
            (true, Duration::from_millis(1)),
            (true, Duration::from_millis(1)),
        ],
    )
    .await;
    let _armed = cell.call().await;
    let mut two = vec![cell.id(0), cell.id(1)];
    two.sort_unstable();
    let (population, retained, first_identity) = demand_state(&cell.client).expect("demand");
    assert_eq!(population, two, "precondition: a COMPLETE convergence");
    assert_eq!(retained, two);

    // Kill one holder's ownership: release the very tickets the demand holds.
    // The demand's recorded provider list is unchanged by this - that is the
    // whole point - and no authority moves.
    let killed = cell
        .client
        .sensing_release_holders_for_test(&capability())
        .expect("the demand's own holders");
    assert!(killed > 0, "a holder was actually released");
    assert!(
        cell.client.sensing_holders_are_live(&capability()) == Some(false),
        "core reports the demand's ownership as no longer live"
    );
    let (_, still_recorded, _) = demand_state(&cell.client).expect("demand");
    assert_eq!(
        still_recorded, two,
        "and the RECORDED list still names both providers: history, not ownership"
    );

    // A later call must reacquire rather than trust the record.
    cell.call_until(
        "a later call never recovered ownership that died after convergence",
        Duration::from_secs(30),
        || {
            cell.client.sensing_holders_are_live(&capability()) == Some(true)
                && demand_state(&cell.client)
                    .map(|(population, retained, identity)| {
                        population == two && retained == two && identity != first_identity
                    })
                    .unwrap_or(false)
        },
    )
    .await;
    cell.cleanup();
}

/// Concurrent clones that ACTUALLY contend for the reconciliation transaction
/// produce exactly ONE convergence — and never occupy that transaction
/// together.
///
/// Three things this witness deliberately does not rest on:
///
/// * **spawning.** Four tasks are not four contenders. Contention is counted
///   where it actually happens: the section's acquisition tries first and
///   counts only the callers whose try FAILED because somebody else held it.
/// * **elapsed time.** The caller inside the section holds it until that
///   contention count says the others are really blocked on it, not for a
///   fixed window — a window only establishes that time passed, and callers
///   spaced further apart than the window would never have overlapped at all.
/// * **the observer.** Two separate things establish this, because occupancy
///   alone cannot. The gauge is entered by the call path immediately after the
///   serializing guard and left when the whole transaction ends, so it
///   brackets the installed-state read, the decision, `retain` and the record
///   — a lock narrowed to the hook leaves all of that overlapping and the peak
///   rises. But overlap is a property of the SCHEDULE: a section released
///   early is still wrong when the awakened contender happens to wait its
///   turn, and no peak would show it. So the transaction also re-checks its
///   OWN hold at the decision and at the record, and the count of steps that
///   ran without it must be zero. That check needs no second caller and no
///   interleaving: it is about the guard, not about who else was running.
///
/// Nor is "one caller parked while the others arrive at the door" enough. That
/// schedule survives an UNSERIALIZED section through a single early winner:
/// the parked caller sleeps, one other completes the sole convergence, the
/// rest reuse its record — four arrivals, one convergence, nothing detected.
/// So the assertions are ordered by what they can discriminate:
///
/// * real contention was acknowledged at acquisition;
/// * no step of the transaction ran outside its own hold — what an early
///   `drop` of the guard fails, under EVERY permitted schedule;
/// * the peak transaction occupancy is 1 — what both the removed lock and the
///   observer-only lock fail;
/// * exactly one caller converges. The PROPERTY serialization exists for, and
///   deliberately not the discriminator: an executed run with the lock removed
///   still reported one convergence through that early-winner schedule.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn contending_clones_produce_exactly_one_convergence() {
    let cell = Cell::stand_up(
        "clones",
        true,
        &[
            (true, Duration::from_millis(1)),
            (true, Duration::from_millis(1)),
        ],
    )
    .await;
    const CALLERS: u64 = 4;

    // The caller inside the section stays there until the others have really
    // blocked on it. The bound is a safety net for a broken build, not the
    // mechanism: a run that reaches it fails the contention assertion below.
    // The guard clears the hook on the way out - including on unwind, so a
    // failing assertion cannot leave the fixture's own clone inside the
    // binding it is installed on.
    let hook = SectionHook::install(&cell.client, {
        let client = cell.client.clone();
        Arc::new(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let (_, contended, _, _) = client
                    .sensing_section_counters()
                    .expect("an active binding");
                if contended >= CALLERS - 1 || Instant::now() >= deadline {
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        })
    });

    let clones: Vec<OrgClient> = (0..CALLERS).map(|_| cell.client.clone()).collect();
    let mut tasks = Vec::new();
    for clone in &clones {
        let clone = clone.clone();
        tasks.push(tokio::spawn(async move {
            clone
                .call::<Ping, Pong>(SERVICE, &Ping { n: 1 })
                .await
                .map(|reply| reply.served_by)
        }));
    }
    for task in tasks {
        task.await.expect("join").expect("admitted");
    }
    drop(hook);

    let (arrivals, contended, convergences, peak) = cell
        .client
        .sensing_section_counters()
        .expect("an active binding");
    assert!(
        arrivals >= CALLERS,
        "precondition: every caller reached the section door, saw {arrivals}"
    );
    assert!(
        contended >= CALLERS - 1,
        "the callers must have found the section HELD - acquisition-time \
         contention, not elapsed time - saw {contended}"
    );
    assert_eq!(
        cell.client.sensing_section_unguarded_steps(),
        Some(0),
        "the decision and the record must run under the hold this caller took \
         - a section released early loses it whether or not anyone overlapped"
    );
    assert_eq!(
        peak, 1,
        "two callers were inside the decide/converge/record transaction at once"
    );
    assert_eq!(
        convergences, 1,
        "one change, one convergence: {arrivals} contending callers must not each converge"
    );

    // ...and the shared state they all read is the same one.
    let mut two = vec![cell.id(0), cell.id(1)];
    two.sort_unstable();
    let (population, retained, identity) = demand_state(&cell.client).expect("demand");
    assert_eq!(population, two);
    assert_eq!(retained, two, "one holder each - no duplicate tickets");
    assert_eq!(cell.client.sensing_records(), Some(1));
    for clone in &clones {
        let (clone_population, clone_retained, clone_identity) =
            demand_state(clone).expect("demand");
        assert_eq!(clone_population, population);
        assert_eq!(clone_retained, retained);
        assert_eq!(
            clone_identity, identity,
            "every clone reads the SAME demand"
        );
    }
    assert_eq!(
        cell.consumer.node.sensing_interest_count(),
        2,
        "no duplicate interest rows"
    );
    cell.cleanup();
}

// ---------------------------------------------------------------------------
// WITHIN-ATTEMPT discovery movement: the two directions, executed
// ---------------------------------------------------------------------------

/// Inject a real owner-scoped announcement for `provider` into `consumer` at
/// `sequence`, expiring at `expires_at`, and pin the provider's entity.
///
/// Both halves go through the shipped verified paths: the envelope is built by
/// the canonical builder and admitted by the real ingest, and the pin is the
/// same TOFU entry a handshake writes. `sequence` is the announcement's own
/// soft-state sequence, so a later one genuinely REPLACES an expired row
/// rather than being dropped as stale.
fn discover_synthetic_at(
    consumer: &Member,
    owner: &OrgKeypair,
    provider: &net::adapter::net::identity::EntityKeypair,
    sequence: u64,
    expires_at: u64,
) {
    ingest_synthetic_at(&consumer.node, owner, provider, sequence, expires_at);
    consumer
        .node
        .test_pin_peer_entity(provider.entity_id().node_id(), provider.entity_id().clone());
}

/// The announcement half of [`discover_synthetic_at`], against a node handle.
///
/// A section hook outlives any borrow of a [`Member`], so a hook that moves
/// discovery from INSIDE an attempt captures the node's `Arc` and announces
/// through this.
fn ingest_synthetic_at(
    node: &Arc<MeshNode>,
    owner: &OrgKeypair,
    provider: &net::adapter::net::identity::EntityKeypair,
    sequence: u64,
    expires_at: u64,
) {
    ingest_synthetic_declaring(
        node,
        owner,
        provider,
        sequence,
        expires_at,
        CapabilitySet::new().add_tag(TAG),
    );
}

/// [`ingest_synthetic_at`] with an EXPLICIT capability descriptor.
///
/// A provider that stops serving the capability publishes a later-sequence
/// announcement that no longer declares its tag; admitting one is how a
/// witness retires a row from owner-private discovery for this capability
/// DETERMINISTICALLY — the soft-state sequence decides it, not a wall clock.
fn ingest_synthetic_declaring(
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

/// [`discover_synthetic_at`] at the first sequence.
fn discover_synthetic(
    consumer: &Member,
    owner: &OrgKeypair,
    provider: &net::adapter::net::identity::EntityKeypair,
    expires_at: u64,
) {
    discover_synthetic_at(consumer, owner, provider, 1, expires_at);
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs()
}

/// A provider that DISAPPEARS between the caller's capture and core's own
/// query — the narrowing direction — must not be certified as agreement, and
/// the next attempt must converge to what discovery now says.
///
/// The disappearance is real AND deterministically placed. The synthetic
/// provider is announced with a long life, so the caller's capture cannot race
/// its expiry, and it is RETIRED from inside the section hook by the supported
/// soft-state route: a LATER-SEQUENCE owner announcement, signed and admitted
/// through the same verified ingest, whose capability descriptor no longer
/// declares this tag. Owner-private discovery for the tag therefore excludes
/// it the moment that announcement is admitted — decided by the sequence, not
/// by a wall clock — and the hook ASSERTS that exclusion before it returns, so
/// the transition is observed rather than assumed.
///
/// The earlier revision instead republished with a one-second life and slept
/// past it. `unix_now()` truncates to whole seconds, so that lifetime was
/// anywhere from ~0 ms to 1 s and the intended transition rode a sub-second
/// race at ingest time; it failed on CI (run 34410272924) with the ephemeral
/// provider still in core's published population. What exactly a ~0 ms
/// lifetime does at ingest was NOT established, so nothing here claims a
/// mechanism — the repair removes the race instead of timing it better.
///
/// The caller's capture is complete before the hook fires and core's query
/// happens after it returns, so the row is gone strictly between the two, by
/// construction.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn within_attempt_narrowing_is_not_certified_as_agreement() {
    let cell = Cell::stand_up(
        "narrow",
        true,
        &[
            (true, Duration::from_millis(1)),
            (true, Duration::from_millis(1)),
        ],
    )
    .await;
    // A third, synthetic provider - announced to OUTLIVE this witness, so the
    // only disappearance is the one the hook performs below.
    let ephemeral = net::adapter::net::identity::EntityKeypair::generate();
    discover_synthetic(&cell.consumer, &org(), &ephemeral, unix_now() + 3600);
    let ephemeral_id = ephemeral.entity_id().node_id();
    until(
        "the ephemeral provider was never discovered",
        SETTLE,
        || population(&cell.consumer).contains(&ephemeral_id),
    )
    .await;

    // Retire it from INSIDE the attempt: the caller's expectation already
    // includes the ephemeral provider, and core's query will not see it.
    let fired = Arc::new(AtomicUsize::new(0));
    let retracted = Arc::new(AtomicUsize::new(0));
    let hook = SectionHook::install(&cell.client, {
        let fired = fired.clone();
        let retracted = retracted.clone();
        let consumer_node = Arc::clone(&cell.consumer.node);
        let owner = org();
        let ephemeral_key = ephemeral.clone();
        Arc::new(move || {
            if fired.fetch_add(1, Ordering::SeqCst) > 0 {
                return;
            }
            // A later-sequence announcement that no longer declares this
            // capability — the supported way a provider stops serving it. No
            // sleep and no clock margin: the row leaves this capability's
            // owner-private discovery as soon as the announcement is admitted.
            ingest_synthetic_declaring(
                &consumer_node,
                &owner,
                &ephemeral_key,
                2,
                unix_now() + 3600,
                CapabilitySet::new(),
            );
            // OBSERVED, not assumed: current discovery for the tag no longer
            // carries the provider. If the retraction were ever refused, this
            // stays zero and the assertion after the call fails loudly instead
            // of the witness silently testing nothing.
            let gone = consumer_node
                .owner_private_capability_providers(&capability())
                .into_iter()
                .all(|row| row.provider.node_id() != ephemeral_key.entity_id().node_id());
            if gone {
                retracted.fetch_add(1, Ordering::SeqCst);
            }
        })
    });
    let _armed = cell.try_call().await;
    drop(hook);
    assert_eq!(
        fired.load(Ordering::SeqCst),
        1,
        "the section hook must have held one attempt open"
    );
    assert_eq!(
        retracted.load(Ordering::SeqCst),
        1,
        "precondition: the retraction must have removed the row from current \
         owner-private discovery INSIDE the attempt"
    );

    // The expectation that attempt ACTUALLY derived, read from the call path
    // rather than sampled beside it: it contains the provider core then failed
    // to publish, which is the whole shape this witness exists for.
    let first_expectation = cell
        .client
        .sensing_last_expectation()
        .expect("an active binding");
    assert!(
        first_expectation.contains(&ephemeral_id),
        "precondition: the first attempt captured the ephemeral provider: \
         {first_expectation:?}"
    );

    let (population_now, _, _) = demand_state(&cell.client).expect("demand");
    assert!(
        !population_now.contains(&ephemeral_id),
        "core published the NARROWER population: {population_now:?}"
    );

    // That mismatch must not be settled. A later call agrees with what
    // discovery now says - the two real providers - and nothing is stuck.
    let mut two = vec![cell.id(0), cell.id(1)];
    two.sort_unstable();
    let expected = two.clone();
    cell.call_until(
        "the narrowed attempt was certified and never reconverged",
        Duration::from_secs(30),
        || {
            demand_state(&cell.client)
                .map(|(population, retained, _)| population == expected && retained == expected)
                .unwrap_or(false)
        },
    )
    .await;
    // And it SETTLES: once the two sides agree, calls stop reconverging.
    let (_, _, identity) = demand_state(&cell.client).expect("demand");
    for _ in 0..3 {
        let _ = cell.try_call().await;
    }
    let (_, _, identity_again) = demand_state(&cell.client).expect("demand");
    assert_eq!(
        identity, identity_again,
        "an agreed population is not re-acquired on every call"
    );
    cell.cleanup();
}

/// A provider that APPEARS between the caller's capture and core's own query —
/// the widening direction — must not be certified as agreement either, because
/// the caller never observed it and would otherwise never retire it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn within_attempt_widening_is_not_certified_as_agreement() {
    let cell = Cell::stand_up(
        "widen",
        true,
        &[
            (true, Duration::from_millis(1)),
            (true, Duration::from_millis(1)),
        ],
    )
    .await;

    // Inside the attempt, a third provider becomes discovered AND pinned - so
    // core's query sees a population the caller's expectation did not.
    let latecomer = net::adapter::net::identity::EntityKeypair::generate();
    let latecomer_id = latecomer.entity_id().node_id();
    let fired = Arc::new(AtomicUsize::new(0));
    let hook = SectionHook::install(&cell.client, {
        let fired = fired.clone();
        let consumer_node = Arc::clone(&cell.consumer.node);
        let owner = org();
        let latecomer = latecomer.clone();
        Arc::new(move || {
            if fired.fetch_add(1, Ordering::SeqCst) > 0 {
                return;
            }
            let authority = consumer_node.node_authority().expect("authority");
            let cert = OrgMembershipCert::try_issue(&owner, latecomer.entity_id().clone(), 1, 3600)
                .expect("membership");
            let descriptor = CapabilitySet::new().add_tag(TAG).to_bytes_compact();
            let envelope = net::adapter::net::behavior::org_scoped_ann::ScopedCapabilityAnnouncement::build_owner(
                    &latecomer,
                    owner.org_id(),
                    cert,
                    authority.audience.audience_handle,
                    authority.audience.discovery_key(),
                    1,
                    unix_now() + 3600,
                    &descriptor,
                )
                .expect("owner envelope");
            consumer_node.ingest_scoped_announcement_for_test(&envelope.to_bytes());
            consumer_node.test_pin_peer_entity(
                latecomer.entity_id().node_id(),
                latecomer.entity_id().clone(),
            );
        })
    });
    let _armed = cell.try_call().await;
    drop(hook);
    assert_eq!(fired.load(Ordering::SeqCst), 1, "the hook must have fired");

    let (population_now, _, _) = demand_state(&cell.client).expect("demand");
    assert!(
        population_now.contains(&latecomer_id),
        "core published the WIDER population: {population_now:?}"
    );

    // A later call, whose expectation now includes the latecomer, must AGREE
    // rather than inherit an unexamined wider demand - and it must settle.
    let mut three = vec![cell.id(0), cell.id(1), latecomer_id];
    three.sort_unstable();
    let expected = three.clone();
    cell.call_until(
        "the widened attempt never reached agreement",
        Duration::from_secs(30),
        || {
            demand_state(&cell.client)
                .map(|(population, _, _)| population == expected)
                .unwrap_or(false)
        },
    )
    .await;
    let (_, _, identity) = demand_state(&cell.client).expect("demand");
    for _ in 0..3 {
        let _ = cell.try_call().await;
    }
    let (_, _, identity_again) = demand_state(&cell.client).expect("demand");
    assert_eq!(
        identity, identity_again,
        "and once the two sides agree, nothing re-acquires per call"
    );
    cell.cleanup();
}

/// A population TRUNCATED to the sensed cap agrees with the wider expectation
/// that asked for it — so a consumer with more authorized providers than the
/// cap settles instead of re-acquiring on every floored retry.
///
/// The cap is the one explicit exception to exact agreement, and it is
/// explicit precisely because "narrower than asked for" is otherwise the
/// signal that discovery moved under the attempt.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_capped_population_agrees_with_a_wider_expectation() {
    use net::adapter::net::behavior::org_sensing_demand::MAX_SENSED_POPULATION;

    let cell = Cell::stand_up(
        "capped",
        true,
        &[
            (true, Duration::from_millis(1)),
            (true, Duration::from_millis(1)),
        ],
    )
    .await;

    // Enough authorized, pinned, discovered providers to exceed the cap.
    let crowd: Vec<net::adapter::net::identity::EntityKeypair> = (0..MAX_SENSED_POPULATION + 8)
        .map(|_| net::adapter::net::identity::EntityKeypair::generate())
        .collect();
    for member in &crowd {
        discover_synthetic(&cell.consumer, &org(), member, unix_now() + 3600);
    }
    until("the crowd was never discovered", SETTLE, || {
        population(&cell.consumer).len() > MAX_SENSED_POPULATION
    })
    .await;

    let _armed = cell.try_call().await;
    let (published, retained, identity) = demand_state(&cell.client).expect("demand");
    assert_eq!(
        published.len(),
        MAX_SENSED_POPULATION,
        "core truncated the population to its own bound"
    );
    assert_eq!(retained, published, "with a holder for every capped member");
    let mut canonical = population(&cell.consumer);
    canonical.truncate(MAX_SENSED_POPULATION);
    assert_eq!(
        published, canonical,
        "and it is core's CANONICAL prefix - the lowest ids of the expectation - \
         not merely some cap-sized subset of it"
    );

    // It SETTLES: repeated calls, and calls past the retry floor, reuse the
    // same demand rather than re-converging against an expectation the cap can
    // never cover.
    for _ in 0..3 {
        let _ = cell.try_call().await;
    }
    tokio::time::sleep(Duration::from_millis(2200)).await;
    let _ = cell.try_call().await;
    let (_, _, identity_again) = demand_state(&cell.client).expect("demand");
    assert_eq!(
        identity, identity_again,
        "a capped population is agreement, not a mismatch to retry forever"
    );
    cell.cleanup();
}

/// A provider that belongs in the CANONICAL capped population, and went
/// missing from one attempt, is recovered under an UNCHANGED expectation.
///
/// This is the reviewer's reproduction as a witness, and it is the exact case
/// "cap-sized and contained" cannot see. The lowest-id provider LEAVES this
/// capability's owner-private discovery inside the attempt, so core publishes
/// a full-sized population that is a subset of the expectation but NOT the
/// prefix core's own rule would have produced. The provider is then
/// republished at a later sequence BEFORE the next call, so the next
/// expectation is byte-for-byte the recorded one: no changed input, no
/// authority movement, no dead holder — nothing but the agreement rule can
/// retry this, and nothing but the canonical rule can tell the two full-sized
/// populations apart.
///
/// The departure is CLOCK-FREE. An earlier revision republished the row with
/// `unix_now() + 1` and slept past it; `unix_now()` truncates to whole
/// seconds, so that envelope's life was 0-1000 ms and, when the second ticked
/// before `verify_scoped_ingest` evaluated it, the ingest was REFUSED and the
/// original `+3600` row stayed authorized — measured at 2 refusals in 300
/// replays, and the shape that failed CI run 34427405228 with the canonical
/// member still published. The hook now publishes a later-sequence signed
/// owner announcement whose descriptor no longer declares this tag — the
/// supported way a provider stops serving one — and ASSERTS the row is gone
/// from current owner-private discovery before it returns. Separate expiry
/// coverage lives in `an_expired_announcement_leaves_the_population_and_a_live_one_stays`
/// and is untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_missing_canonical_member_is_recovered_under_an_unchanged_expectation() {
    use net::adapter::net::behavior::org_sensing_demand::MAX_SENSED_POPULATION;
    use net::adapter::net::identity::EntityKeypair;

    let cell = Cell::stand_up(
        "canon",
        true,
        &[
            (true, Duration::from_millis(1)),
            (true, Duration::from_millis(1)),
        ],
    )
    .await;

    // More authorized, pinned providers than the cap, so truncation is real.
    let crowd: Vec<EntityKeypair> = (0..MAX_SENSED_POPULATION + 8)
        .map(|_| EntityKeypair::generate())
        .collect();
    for member in &crowd {
        discover_synthetic(&cell.consumer, &org(), member, unix_now() + 3600);
    }

    // ...plus ONE more whose node id is below every other member of the
    // expectation, so it is unambiguously inside core's canonical prefix. Node
    // ids come from entity bytes, so this is found by generating rather than
    // chosen.
    let floor_id = crowd
        .iter()
        .map(|member| member.entity_id().node_id())
        .chain(cell.providers.iter().map(|p| p.node.node_id()))
        .min()
        .expect("a non-empty crowd");
    let mut lowest = None;
    for _ in 0..4096 {
        let candidate = EntityKeypair::generate();
        if candidate.entity_id().node_id() < floor_id {
            lowest = Some(candidate);
            break;
        }
    }
    let lowest = lowest.expect("a provider below every other id");
    let lowest_id = lowest.entity_id().node_id();
    // Every row, including this one, outlives the witness: the expiry that
    // matters happens INSIDE the attempt, below, so the first capture cannot
    // race it.
    discover_synthetic_at(&cell.consumer, &org(), &lowest, 1, unix_now() + 3600);
    until("the crowd was never discovered", SETTLE, || {
        let seen = population(&cell.consumer);
        seen.len() > MAX_SENSED_POPULATION && seen.contains(&lowest_id)
    })
    .await;

    // Hold ONE attempt open and RETIRE the lowest provider from inside it,
    // through a later-sequence announcement that no longer declares this
    // capability. The caller's capture is complete before the hook fires and
    // core's query happens after it returns, so the disappearance is strictly
    // between them - decided by the soft-state sequence, not by a clock.
    let fired = Arc::new(AtomicUsize::new(0));
    let retracted = Arc::new(AtomicUsize::new(0));
    let hook = SectionHook::install(&cell.client, {
        let fired = fired.clone();
        let retracted = retracted.clone();
        let consumer_node = Arc::clone(&cell.consumer.node);
        let owner = org();
        let lowest_key = lowest.clone();
        Arc::new(move || {
            if fired.fetch_add(1, Ordering::SeqCst) > 0 {
                return;
            }
            ingest_synthetic_declaring(
                &consumer_node,
                &owner,
                &lowest_key,
                2,
                unix_now() + 3600,
                CapabilitySet::new(),
            );
            // OBSERVED, not attempted: an ingest that was refused, or a
            // retraction that did not take, leaves this at zero and the
            // assertion after the call fails loudly rather than the witness
            // testing a population nothing removed anything from.
            let gone = consumer_node
                .owner_private_capability_providers(&capability())
                .into_iter()
                .all(|row| row.provider.node_id() != lowest_key.entity_id().node_id());
            if gone {
                retracted.fetch_add(1, Ordering::SeqCst);
            }
        })
    });
    let _armed = cell.try_call().await;
    drop(hook);
    assert_eq!(
        fired.load(Ordering::SeqCst),
        1,
        "the section hook must have held one attempt open"
    );
    assert_eq!(
        retracted.load(Ordering::SeqCst),
        1,
        "precondition: the retraction must have removed the row from current \
         owner-private discovery INSIDE the attempt"
    );

    // The expectation that attempt ACTUALLY derived - read from the call path,
    // not sampled beside it - and it contains the provider core then failed to
    // publish. That is the whole shape this witness exists for.
    let first_expectation = cell
        .client
        .sensing_last_expectation()
        .expect("an active binding");
    assert!(
        first_expectation.contains(&lowest_id),
        "precondition: the first attempt captured the ephemeral provider: \
         {first_expectation:?}"
    );
    assert_eq!(
        first_expectation.first(),
        Some(&lowest_id),
        "and it leads the canonical prefix that attempt asked for"
    );
    let (published, retained, identity) = demand_state(&cell.client).expect("demand");
    assert_eq!(
        published.len(),
        MAX_SENSED_POPULATION,
        "core still published a FULL-sized population: {published:?}"
    );
    assert_eq!(retained, published, "with a holder for every member");
    assert!(
        !published.contains(&lowest_id),
        "and it is missing the canonical member: {published:?}"
    );

    // Restore it BEFORE the next call, at a later sequence, so the next
    // attempt derives the SAME expectation the first one did.
    discover_synthetic_at(&cell.consumer, &org(), &lowest, 3, unix_now() + 3600);
    until("the lowest provider was never rediscovered", SETTLE, || {
        population(&cell.consumer).contains(&lowest_id)
    })
    .await;

    // The disagreement must be retried, and the canonical population restored.
    cell.call_until(
        "a full-sized non-canonical population was certified and never retried",
        Duration::from_secs(30),
        || {
            demand_state(&cell.client)
                .map(|(population, retained, _)| {
                    population.contains(&lowest_id) && retained == population
                })
                .unwrap_or(false)
        },
    )
    .await;
    let (recovered, _, recovered_identity) = demand_state(&cell.client).expect("demand");
    let mut canonical = population(&cell.consumer);
    canonical.truncate(MAX_SENSED_POPULATION);
    assert_eq!(
        recovered, canonical,
        "the recovered population is core's canonical prefix"
    );
    assert_ne!(
        identity, recovered_identity,
        "and it really re-converged rather than reporting the old demand"
    );
    assert_eq!(
        cell.client.sensing_last_expectation(),
        Some(first_expectation),
        "and the attempt that recovered it derived the SAME expectation the \
         frozen one did - nothing about the caller's inputs changed"
    );

    // ...and now that the two sides agree, the cap settles again.
    for _ in 0..3 {
        let _ = cell.try_call().await;
    }
    tokio::time::sleep(Duration::from_millis(2200)).await;
    let _ = cell.try_call().await;
    let (_, _, settled) = demand_state(&cell.client).expect("demand");
    assert_eq!(
        recovered_identity, settled,
        "a canonical capped population is agreement, not a mismatch to retry forever"
    );
    cell.cleanup();
}

/// A SAME-ORGANIZATION provider discovered only under a held DISCOVER grant is
/// outside the sensing expectation, so stable inputs reuse the owner demand.
///
/// Same-org grants are legitimate — an organization may issue one to itself —
/// and this witness installs a real one: a real audience, a real granted
/// envelope through the ordinary ingest path, a real pin. What it must NOT do
/// is enter the SENSED expectation: core derives the sensed population from
/// owner-private discovery alone, so an expectation that asked for the
/// grant-plane provider could never be met, and every call past the retry
/// floor reconverged an owner population that had not changed.
///
/// Classifying by owner org alone cannot see this: the grant-plane provider IS
/// same-org. Only its discovery provenance separates it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_same_org_grant_only_provider_is_outside_the_sensing_expectation() {
    use net::adapter::net::behavior::org_scoped_ann::ScopedCapabilityAnnouncement;
    use net::adapter::net::identity::EntityKeypair;
    use net_sdk::org::types::{GrantRights, GrantTargetScope, OrgCapabilityGrant};

    let cell = Cell::stand_up(
        "sameorg-grant",
        true,
        &[
            (true, Duration::from_millis(1)),
            (true, Duration::from_millis(1)),
        ],
    )
    .await;

    // A REAL grant, issued by this organization TO ITSELF, carrying both
    // rights - so the provider under it is genuinely discoverable and
    // genuinely invocable.
    let (grant, secret) = OrgCapabilityGrant::try_issue(
        &org(),
        org().org_id(),
        capability(),
        GrantRights::DISCOVER.union(GrantRights::INVOKE),
        GrantTargetScope::AnyNodeOwnedBy(org().org_id()),
        3600,
    )
    .expect("issue the same-organization grant");
    let secret = secret.expect("a DISCOVER grant carries audience material");
    let grant_id = grant.grant_id;
    let audience_handle = secret.audience_handle;
    let discovery_key = *secret.discovery_key();
    cell.consumer
        .node
        .install_consumer_grant_audience(grant.clone(), secret)
        .expect("install the consumer grant audience");

    // The grant-plane provider: announced under that grant's audience, pinned
    // exactly like an owner-plane one, and never announced on the owner plane.
    let granted = EntityKeypair::generate();
    let granted_id = granted.entity_id().node_id();
    let cert = OrgMembershipCert::try_issue(&org(), granted.entity_id().clone(), 1, 3600)
        .expect("membership");
    let descriptor = CapabilitySet::new().add_tag(TAG).to_bytes_compact();
    let envelope = ScopedCapabilityAnnouncement::build_granted(
        &granted,
        org().org_id(),
        cert,
        grant_id,
        audience_handle,
        &discovery_key,
        1,
        unix_now() + 3600,
        &descriptor,
    )
    .expect("granted envelope");
    cell.consumer
        .node
        .ingest_scoped_announcement_for_test(&envelope.to_bytes());
    cell.consumer
        .node
        .test_pin_peer_entity(granted.entity_id().node_id(), granted.entity_id().clone());

    // It really is on the granted plane, and really is not on the owner one -
    // so the exclusion below is a decision, not an empty set.
    until(
        "the granted-plane provider was never discovered",
        SETTLE,
        || {
            cell.consumer
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
    let owner_population = population(&cell.consumer);
    assert!(
        !owner_population.contains(&granted_id),
        "precondition: owner-private discovery never saw it: {owner_population:?}"
    );

    // A client that HOLDS the grant: its candidate list includes the
    // grant-plane provider, and its sensing expectation must not.
    let client = bind_with(&cell.consumer, vec![grant]);
    let _first = client
        .call::<Ping, Pong>(SERVICE, &Ping { n: 1 })
        .await
        .map(|reply| reply.served_by);
    let (population_now, retained, identity) = demand_state(&client).expect("demand");
    assert_eq!(
        population_now, owner_population,
        "the sensed population is the OWNER plane's"
    );
    assert_eq!(retained, population_now, "with a holder for each");
    let (_, _, converged, _) = client
        .sensing_section_counters()
        .expect("an active binding");
    assert_eq!(converged, 1, "one convergence for the first call");

    // Nothing changes: same discovery, same authority, same holders. Calls
    // past the retry floor must REUSE the demand rather than reconverge it.
    tokio::time::sleep(Duration::from_millis(2200)).await;
    for _ in 0..3 {
        let _ = client.call::<Ping, Pong>(SERVICE, &Ping { n: 1 }).await;
    }
    let (_, _, converged_again, _) = client
        .sensing_section_counters()
        .expect("an active binding");
    let (_, _, identity_again) = demand_state(&client).expect("demand");
    assert_eq!(
        converged, converged_again,
        "stable inputs must not reconverge: an unreachable expectation retried forever"
    );
    assert_eq!(identity, identity_again, "and the demand is the same one");
    drop(client);
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

/// `direct` is an IDENTITY PIN, not a live session — and this establishes what
/// that costs, on the CONSUMER's own local state.
///
/// The predicate predates this slice: `peer_entity_id` reads the pin map, and
/// a session can stop being usable without the pin going away. Rather than
/// inferring that from a stopped remote process, this witness names the local
/// state exactly: the consumer's own session to the sensed leader is
/// DEACTIVATED while the pin remains, the provider process stays up, and the
/// sensed evidence still ranks that provider first. Then it asserts what
/// actually happens.
///
/// The outcome is recorded, not wished for: selection follows the pin, the
/// call is planned for that provider, and the specific result is asserted. No
/// fallback, retry or liveness predicate is introduced — correcting the
/// predicate would be transport policy, which this slice is not authorized to
/// change, and the same predicate governs the unsensed order.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pinned_but_locally_dead_session_is_still_the_sensed_selection() {
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

    // THE LOCAL STATE, named: the consumer's session to the sensed leader is
    // deactivated. The pin survives, and the provider is still running and
    // still serving - so nothing about the REMOTE side explains the result.
    let session = cell
        .consumer
        .node
        .peer_session_for_test(cell.id(1))
        .expect("the consumer has a session to the sensed leader");
    assert!(session.is_active(), "precondition: it starts live");
    session.deactivate();
    assert!(
        !session.is_active(),
        "the consumer's own session to that provider is no longer live"
    );
    assert!(
        cell.consumer.node.peer_entity_id(cell.id(1)).is_some(),
        "while the identity PIN survives - the inherited predicate"
    );
    assert!(
        cell.consumer
            .node
            .peer_session_for_test(cell.id(0))
            .is_some_and(|live| live.is_active()),
        "and the other provider's session is still live, so a fallback WOULD \
         have somewhere to go"
    );

    // The intended reordered precondition still holds at selection time.
    let projection = cell
        .client
        .sensing_projection(
            &capability(),
            Instant::now(),
            &ConsumerLatencyBudget::default(),
        )
        .expect("demand");
    assert_eq!(
        projection.viable(),
        [cell.id(1)],
        "the sensed order still prefers the provider whose session is dead"
    );

    // THE OUTCOME. Bounded by a deadline so the witness cannot hang, and the
    // bound is generous on purpose: the budget it implies must stay far above
    // every estimate here, and the RPC must not race its own deadline on a
    // loaded runner (the deadline-order witness above failed exactly that way
    // in exact-head CI at 150 ms).
    let before = (cell.served(0), cell.served(1));
    let body = serde_json::to_vec(&Ping { n: 1 }).expect("encode");
    let outcome = cell
        .client
        .call_bytes_deadline(SERVICE, bytes::Bytes::from(body), 10_000, 0)
        .await;

    // ATTRIBUTION, first: which provider did planning actually select, and was
    // the local state still what this witness set up when it did?
    let selected = cell
        .client
        .last_selected_provider()
        .expect("the call planned a provider");
    assert_eq!(
        selected,
        *cell.providers[1].node.entity_id(),
        "selection followed the identity PIN to the provider whose local \
         session is dead - not the live alternative"
    );
    assert!(
        !cell
            .consumer
            .node
            .peer_session_for_test(cell.id(1))
            .expect("the session object survives")
            .is_active(),
        "and that session was still inactive across the call"
    );
    assert_eq!(
        cell.served(0) - before.0,
        0,
        "there is no silent fallback: the live provider was never called"
    );

    // ...and then the specific result of having selected it. ESTABLISHED, not
    // wished for: the RPC send path does not consult a session's `active`
    // flag, so a locally deactivated session is not a barrier to the send.
    // The call therefore succeeds AT THE SELECTED PROVIDER, and its handler is
    // the one that runs.
    let reply = outcome.expect(
        "a locally deactivated session does not stop the send: if this ever \
         fails, the established outcome has changed and the attribution below \
         must be re-derived rather than relaxed",
    );
    let pong: Pong = serde_json::from_slice(&reply).expect("decode");
    assert_eq!(
        pong.served_by, cell.names[1],
        "the reply comes from the provider selection chose - never a substitution"
    );
    assert_eq!(
        cell.served(1) - before.1,
        1,
        "the selected provider's handler ran exactly once"
    );

    // WHAT THIS DOES AND DOES NOT ESTABLISH. `direct` is an identity pin, the
    // pin outlives a session's active flag, and that flag is not a send-path
    // predicate - so no wrong selection and no wrong error is demonstrated
    // here: the call reached the provider selection named. The inherited
    // predicate is therefore recorded, and no SDK correction is triggered by
    // this reproduction. A liveness predicate would be transport policy, which
    // this slice does not change, and the unsensed order selects by the same
    // pin.
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

    for _ in 0..3 {
        assert_eq!(
            cell.call().await,
            cell.names[0],
            "with no evidence the order is the deterministic one"
        );
        // NO DEMAND AT ALL - the point of the gate, and stronger than what
        // this witness used to assert.
        //
        // The binding used to be minted whenever a routing-family id could be
        // allocated, never consulting the node's master switch. Convergence
        // then ran, every lease acquisition refused with `Disabled`, and the
        // published demand had an empty `retained` - so `agreed` was false and
        // `needs_convergence` answered true again after every retry floor.
        // Asserting "a demand exists with no holders" accepted exactly that
        // state; the loop only looked stable here because three calls fit
        // inside one floor.
        assert!(
            demand_state(&cell.client).is_none(),
            "a node with sensing off must hold no demand whatsoever"
        );
    }
    assert_eq!(cell.served(1), 0);
    assert_eq!(cell.served(0), 3);
    assert!(
        cell.consumer.node.sensing_table_is_empty(),
        "a dark consumer registers no interest at all"
    );
    assert!(
        cell.client
            .sensing_projection(
                &capability(),
                Instant::now(),
                &ConsumerLatencyBudget::default(),
            )
            .is_none(),
        "and it projects nothing, because there is nothing to project from"
    );
    cell.cleanup();
}

// ---------------------------------------------------------------------------
// W-55 — the SDK adapter adds no ordering structure
// ---------------------------------------------------------------------------

/// The ordering RULE lives once, in core. The SDK's share is a
/// projection of candidates onto that rule's two slices — so this guard reads
/// the adapter's exact body and requires that it introduces no comparison sort
/// and no ordering structure of its own. A second implementation is what would
/// diverge; this is how the file stays unable to grow one.
///
/// The span is walked by braces from the signature over LEXICALLY FILTERED
/// source — comments and string literals blanked, so the scan sees code only
/// and a `// HashMap::` note or a diagnostic message that names a forbidden
/// construct is not a failure — and an unbalanced walk FAILS rather than
/// passing. Filtering narrows what counts as an occurrence; it does not soften
/// the contract: a comparison sort in the body still fails here even when it
/// happens to produce the same order, and a renamed or deleted adapter fails
/// too, because the span cannot be found.
///
/// The filtering is not taken on trust: the same scan is executed against
/// synthetic sources here — one whose only forbidden tokens sit in a comment
/// and a string literal (which must pass), one that really sorts (which must
/// fail), and one whose adapter is renamed away (which must fail).
#[test]
fn the_sdk_sensed_adapter_adds_no_ordering_structure() {
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/org/call.rs"),
    )
    .expect("read the SDK call path");
    scan_for_ordering_structure(&source).expect("the shipped SDK adapter");

    // The filter narrows what counts as an occurrence...
    let mentioned = synthetic_adapter(
        "let _reason = \"a BinaryHeap or HashMap:: would be a second rule\";\n    \
         // ...and neither is .sort_by_key( nor BTreeMap:: in this note",
    );
    scan_for_ordering_structure(&mentioned)
        .expect("a forbidden construct NAMED in a comment or a string is not one");

    // ...and narrows nothing else: written as code, it still fails, even
    // though this body's output is identical to the unsorted one.
    let sorted = synthetic_adapter("providers.sort_unstable();");
    let complaint = scan_for_ordering_structure(&sorted)
        .expect_err("a comparison sort in the body must fail this guard");
    assert!(
        complaint.contains(".sort_unstable("),
        "and it must name the construct it found: {complaint}"
    );

    // A renamed or deleted adapter fails loudly rather than passing vacuously.
    let renamed =
        synthetic_adapter("").replace("org_sensed_candidate_permutation", "sensed_permutation");
    scan_for_ordering_structure(&renamed)
        .expect_err("a renamed adapter must fail this guard, not disappear from it");
}

/// Require that the adapter in `source` adds no ordering structure of its own.
///
/// `Err` on every way this can go wrong: an absent or unbalanced adapter, a
/// vacuous body, a body that restates the rule instead of applying it, and a
/// forbidden construct in CODE. Taking `&str` is what lets the controls above
/// execute the real scan without mutating the shipped file.
fn scan_for_ordering_structure(source: &str) -> Result<(), String> {
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
    let body = brace_body(source, "fn org_sensed_candidate_permutation(")
        .ok_or_else(|| "the adapter is absent, or its body does not balance".to_string())?;
    if !body.contains("org_sensed_bucket_permutation(") {
        return Err(format!(
            "the adapter must APPLY the core rule rather than restate it:\n{body}"
        ));
    }
    if body.lines().filter(|l| !l.trim().is_empty()).count() <= 3 {
        return Err(format!(
            "anti-vacuity: deleting the adapter's body must fail this guard, \
             not satisfy it:\n{body}"
        ));
    }
    for needle in FORBIDDEN {
        if body.contains(needle) {
            return Err(format!(
                "the SDK adapter must add no ordering structure, found `{needle}` in:\n{body}"
            ));
        }
    }
    Ok(())
}

/// The shipped adapter's shape with `extra` spliced into its body — the
/// controls' source, so the guard's discrimination is executed rather than
/// argued.
fn synthetic_adapter(extra: &str) -> String {
    const SHAPE: &str = r#"
pub(crate) fn org_sensed_candidate_permutation(
    cands: &[AuthorizedOrgCandidate],
    ranked: &[u64],
    pruned: &[u64],
) -> Vec<usize> {
    let mut providers: Vec<u64> = Vec::with_capacity(cands.len());
    let mut same_org: Vec<bool> = Vec::with_capacity(cands.len());
    for candidate in cands {
        providers.push(candidate.provider.node_id());
        same_org.push(matches!(candidate.mode, Mode::SameOrg));
    }
    EXTRA
    org_sensed_bucket_permutation(&same_org, &providers, ranked, pruned)
}
"#;
    SHAPE.replace("EXTRA", extra)
}

/// The body of the function whose signature starts with `signature`, from the
/// first `{` at or after it to the matching `}`, over [`code_only`] source.
/// `None` when the function is absent or the walk fails to balance — both are
/// guard failures, never silent passes.
fn brace_body(source: &str, signature: &str) -> Option<String> {
    let code = code_only(source);
    let start = code.find(signature)?;
    let open = start + code[start..].find('{')?;
    let bytes = code.as_bytes();
    let mut depth = 0usize;
    for index in open..bytes.len() {
        match bytes[index] {
            b'{' => depth += 1,
            b'}' => {
                // The walk starts ON the opening brace, so depth is at least
                // one here and this cannot underflow.
                depth -= 1;
                if depth == 0 {
                    return Some(code[open + 1..index].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// `source` with every comment and every string, byte-string, raw-string and
/// character literal blanked to spaces — newlines kept, so byte offsets, line
/// numbers and indentation are all preserved.
///
/// This is what makes the guard above scan CODE. A forbidden construct NAMED
/// in prose or in an error message is not an ordering structure; a forbidden
/// construct written as code is, wherever it appears. Block comments nest, as
/// they do in Rust, and a `'` that is not a character literal is a lifetime
/// and stays code.
fn code_only(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    // Blank `bytes[index..end]`, keeping newlines, and advance past it.
    let blank = |out: &mut Vec<u8>, index: &mut usize, end: usize| {
        for byte in &bytes[*index..end.min(bytes.len())] {
            out.push(if *byte == b'\n' { b'\n' } else { b' ' });
        }
        *index = end.min(bytes.len());
    };
    while index < bytes.len() {
        let byte = bytes[index];
        let next = bytes.get(index + 1).copied();
        match (byte, next) {
            (b'/', Some(b'/')) => {
                let end = source[index..]
                    .find('\n')
                    .map_or(bytes.len(), |offset| index + offset);
                blank(&mut out, &mut index, end);
            }
            (b'/', Some(b'*')) => {
                let mut depth = 0usize;
                let mut scan = index;
                while scan < bytes.len() {
                    match (bytes[scan], bytes.get(scan + 1).copied()) {
                        (b'/', Some(b'*')) => {
                            depth += 1;
                            scan += 2;
                        }
                        (b'*', Some(b'/')) => {
                            depth -= 1;
                            scan += 2;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => scan += 1,
                    }
                }
                blank(&mut out, &mut index, scan);
            }
            _ => {
                if let Some(end) = raw_string_end(bytes, index) {
                    blank(&mut out, &mut index, end);
                } else if byte == b'"' {
                    let mut scan = index + 1;
                    while scan < bytes.len() {
                        match bytes[scan] {
                            b'\\' => scan += 2,
                            b'"' => {
                                scan += 1;
                                break;
                            }
                            _ => scan += 1,
                        }
                    }
                    blank(&mut out, &mut index, scan);
                } else if let Some(end) = char_literal_end(bytes, index) {
                    blank(&mut out, &mut index, end);
                } else {
                    out.push(byte);
                    index += 1;
                }
            }
        }
    }
    String::from_utf8(out).expect("blanking whole literals keeps char boundaries")
}

/// The end offset of the raw or byte string literal starting at `index`, if one
/// starts there: `r"..."`, `r#"..."#`, `br##"..."##`, and so on.
fn raw_string_end(bytes: &[u8], index: usize) -> Option<usize> {
    let prior = index.checked_sub(1).map(|before| bytes[before]);
    if prior.is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_') {
        // Part of an identifier, not a literal prefix.
        return None;
    }
    let mut scan = index;
    if bytes.get(scan) == Some(&b'b') {
        scan += 1;
    }
    if bytes.get(scan) != Some(&b'r') {
        return None;
    }
    scan += 1;
    let hashes = bytes[scan..]
        .iter()
        .take_while(|byte| **byte == b'#')
        .count();
    scan += hashes;
    if bytes.get(scan) != Some(&b'"') {
        return None;
    }
    scan += 1;
    while scan < bytes.len() {
        if bytes[scan] == b'"'
            && bytes[scan + 1..]
                .iter()
                .take(hashes)
                .filter(|byte| **byte == b'#')
                .count()
                == hashes
        {
            return Some(scan + 1 + hashes);
        }
        scan += 1;
    }
    Some(bytes.len())
}

/// The end offset of the character or byte-character literal starting at
/// `index`, if one starts there. A `'` that opens no literal is a LIFETIME and
/// must stay code, so this recognizes only the closed forms.
fn char_literal_end(bytes: &[u8], index: usize) -> Option<usize> {
    let mut scan = index;
    if bytes.get(scan) == Some(&b'b') && bytes.get(scan + 1) == Some(&b'\'') {
        scan += 1;
    }
    if bytes.get(scan) != Some(&b'\'') {
        return None;
    }
    scan += 1;
    if bytes.get(scan) == Some(&b'\\') {
        // An escape: everything up to the closing quote belongs to it.
        scan += 1;
        while scan < bytes.len() && bytes[scan] != b'\'' {
            scan += 1;
        }
        return (bytes.get(scan) == Some(&b'\'')).then_some(scan + 1);
    }
    // One character - which may be multi-byte - then the closing quote.
    let rest = std::str::from_utf8(&bytes[scan..]).ok()?;
    let one = rest.chars().next()?;
    let close = scan + one.len_utf8();
    (bytes.get(close) == Some(&b'\'')).then_some(close + 1)
}
