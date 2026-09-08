//! OA-6 — the PRODUCTION organization exact-sensing connection.
//!
//! This is an out-of-crate integration test, so everything it touches is the
//! shipped surface: `Mesh::org`, `OrgClient::call`, `Mesh::serve_org`,
//! `Mesh::sensing().provide(..)`. There is no fixture assembly here, no
//! injected order, and no substituted selector — the sensed order these
//! witnesses observe is the one `plan_attempt` actually computed on the call
//! path, and the invocation is the one `MeshNode::call` actually admitted.
//!
//! What each witness holds:
//!
//! * **the binding owns the acquisition** — one mint per bind, shared by every
//!   clone; an intermediate clone's drop retires nothing, the last one retires
//!   everything, and two binds are independent;
//! * **the real call consumes the sensed order** — with two authorized, pinned,
//!   discovered providers whose evaluators disagree, the protected invocation
//!   lands on the SENSED-preferred one even though deterministic selection
//!   would have taken the other;
//! * **unavailable evidence changes nothing but the order** — a consumer whose
//!   sensing plane is off still calls, deterministically, with zero sensing
//!   state;
//! * **the SDK adapter adds no ordering structure** — a source guard over the
//!   adapter's own body.
//!
//! Note on features: this crate's dev-dependency on `net-mesh` enables
//! `fixtures`, so nothing here may be read as a fixtures-OFF build proof. The
//! production path's own independence from that feature is a BUILD fact, shown
//! by `cargo check -p net-mesh-sdk --lib` (no dev-dependencies, `fixtures`
//! off), not by this file. Every seam used below is `pub` without a fixtures
//! gate.
#![cfg(all(feature = "net", feature = "cortex"))]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::behavior::sensing::{
    CapabilityId, ConsumerLatencyBudget, EvaluationRequest, Incarnation, ReadinessEvaluation,
    ReadinessEvaluator,
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
    let identity = Identity::generate();
    let mut cfg = MeshNodeConfig::new("127.0.0.1:0".parse().expect("addr"), [0x51u8; 32])
        .with_heartbeat_interval(Duration::from_millis(100))
        .with_session_timeout(Duration::from_secs(10));
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
fn provide(member: &Member, ready: bool) -> ReadinessRegistration {
    provide_at(member, ready, Duration::from_millis(1))
}

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

// ---------------------------------------------------------------------------
// W-53 — the real production call consumes the sensed order
// ---------------------------------------------------------------------------

/// Two authorized, pinned, discovered providers of ONE capability, with real
/// evaluators that disagree: the provider deterministic selection takes —
/// lowest provider entity id, first direct — answers NotReady, the other
/// answers Ready.
///
/// The protected invocation must land on the SENSED provider. Nothing is
/// substituted: the order is the one the production call path computed, the
/// proof was minted for the provider that order chose, and the reply names the
/// handler that actually ran.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_production_call_lands_on_the_sensed_provider() {
    let owner = org();
    let p1 = mesh_in_org("sensed-p1", &owner, true, None).await;
    let audience = shared_audience(&p1);
    let p2 = mesh_in_org("sensed-p2", &owner, true, Some(&audience)).await;
    let consumer = mesh_in_org("sensed-c", &owner, true, Some(&audience)).await;
    bring_up(&consumer, &[&p1, &p2]).await;

    let (low, high) = by_entity_order(&p1, &p2);
    let (low_id, high_id) = (low.node.node_id(), high.node.node_id());
    let low_calls = Arc::new(AtomicUsize::new(0));
    let high_calls = Arc::new(AtomicUsize::new(0));
    let _low_serve = serve(low, "unsensed-choice", low_calls.clone());
    let _high_serve = serve(high, "sensed-choice", high_calls.clone());
    let _low_ready = provide(low, false);
    let _high_ready = provide(high, true);

    let client = bind(&consumer);
    assert!(
        client.sensing_binding().is_active(),
        "precondition: the bind minted an acquisition family"
    );
    converge_population(&consumer, &[&p1, &p2], 2).await;

    // The FIRST call is what ARMS the acquisition: it retains demand for this
    // capability, which registers one exact-provider interest per provider.
    // Whatever evidence existed at that instant is whatever it was - the
    // witness is the warmed call further down.
    let _armed: Pong = client
        .call(SERVICE, &Ping { n: 1 })
        .await
        .expect("the first protected call is admitted");

    let family = client.sensing_binding().family().expect("Active").clone();
    let demand = {
        let deadline = Instant::now() + SETTLE;
        loop {
            if let Some(demand) = family.demand(&capability()) {
                if demand.population().len() == 2 {
                    break demand;
                }
            }
            assert!(
                Instant::now() < deadline,
                "the call path never retained demand over both providers"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    };

    // Wait for the REAL signed evidence to separate them: the Ready provider
    // viable, the NotReady one pruned. Observation, never injection.
    {
        let deadline = Instant::now() + SETTLE;
        loop {
            let projection =
                demand.project_sensed_order(Instant::now(), &ConsumerLatencyBudget::default());
            if projection.viable() == [high_id] && projection.non_viable() == [low_id] {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "signed readiness never separated the two providers: {projection:?}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    // THE WITNESS: one warmed call, counted at both handlers.
    let low_before = low_calls.load(Ordering::SeqCst);
    let high_before = high_calls.load(Ordering::SeqCst);
    let reply: Pong = client
        .call(SERVICE, &Ping { n: 2 })
        .await
        .expect("the warmed protected call is admitted");

    assert_eq!(
        reply,
        Pong {
            served_by: "sensed-choice".to_string()
        },
        "the call must reach the SENSED provider, not the deterministic first one"
    );
    assert_eq!(
        high_calls.load(Ordering::SeqCst) - high_before,
        1,
        "the sensed provider's handler ran exactly once"
    );
    assert_eq!(
        low_calls.load(Ordering::SeqCst) - low_before,
        0,
        "and the provider deterministic selection would have taken never ran"
    );

    // Sensing REORDERS an authorized list; it does not shrink one. Both
    // providers are still authorized, discovered and pinned.
    let mut both = vec![low_id, high_id];
    both.sort_unstable();
    assert_eq!(
        population(&consumer),
        both,
        "sensing removed no authorization: a deprioritized candidate stays eligible"
    );

    drop(client);
    drop(demand);
    drop(family);
    for member in [&p1, &p2, &consumer] {
        let _ = std::fs::remove_dir_all(&member.dir);
    }
}

// ---------------------------------------------------------------------------
// W-54 — unavailable evidence preserves deterministic unsensed planning
// ---------------------------------------------------------------------------

/// A consumer whose sensing plane is OFF — the shipped default — calls the same
/// two providers. No evidence can exist, so the call takes the deterministic
/// order it always did: lowest provider entity id with a live direct session.
///
/// This is the production shape of the failure ladder's floor: no order, no
/// error, no sensing state, no new refusal, and no per-call retry of the
/// refused acquisition.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_consumer_without_sensing_plans_the_deterministic_order() {
    let owner = org();
    let p1 = mesh_in_org("dark-p1", &owner, true, None).await;
    let audience = shared_audience(&p1);
    let p2 = mesh_in_org("dark-p2", &owner, true, Some(&audience)).await;
    // SENSING OFF on the consumer.
    let consumer = mesh_in_org("dark-c", &owner, false, Some(&audience)).await;
    bring_up(&consumer, &[&p1, &p2]).await;
    assert!(
        !consumer.node.sensing_enabled(),
        "precondition: this consumer's sensing plane is off"
    );

    let (low, high) = by_entity_order(&p1, &p2);
    let low_calls = Arc::new(AtomicUsize::new(0));
    let high_calls = Arc::new(AtomicUsize::new(0));
    let _low_serve = serve(low, "deterministic-choice", low_calls.clone());
    let _high_serve = serve(high, "other", high_calls.clone());
    // Both providers really answer readiness, and the one a SENSED order would
    // prefer is the high-id one - so evidence leaking into this consumer's
    // planning would be caught below rather than hidden.
    let _low_ready = provide(low, false);
    let _high_ready = provide(high, true);

    let client = bind(&consumer);
    converge_population(&consumer, &[&p1, &p2], 2).await;
    let family = client.sensing_binding().family().expect("Active").clone();

    let mut first_demand = None;
    for _ in 0..3 {
        let reply: Pong = client
            .call(SERVICE, &Ping { n: 7 })
            .await
            .expect("the protected call is admitted without sensing");
        // The acquisition is attempted ONCE and then reused, even though it
        // acquired nothing: a dark plane must not turn every call into a fresh
        // convergence attempt.
        let demand = family.demand(&capability()).expect("the demand exists");
        match &first_demand {
            None => first_demand = Some(demand),
            Some(previous) => assert!(
                Arc::ptr_eq(previous, &demand),
                "no per-call re-acquisition: the same demand is reused"
            ),
        }
        assert_eq!(
            reply,
            Pong {
                served_by: "deterministic-choice".to_string()
            },
            "with no evidence the order is the deterministic one - lowest \
             provider id, first direct"
        );
    }
    assert_eq!(high_calls.load(Ordering::SeqCst), 0);
    assert_eq!(low_calls.load(Ordering::SeqCst), 3);

    assert!(
        consumer.node.sensing_table_is_empty(),
        "a dark consumer registers no interest at all"
    );
    // The family still minted - the mint does not consult the plane - and the
    // convergence it attempted acquired NOTHING, because every per-provider
    // acquisition is refused by the dark plane. So the projection is empty and
    // the order it could contribute is the identity.
    let demand = first_demand.expect("a demand was recorded");
    assert!(
        demand.retained_providers().is_empty(),
        "a dark plane acquires no interest, so there is no evidence to order by"
    );
    let projection = demand.project_sensed_order(Instant::now(), &ConsumerLatencyBudget::default());
    assert!(
        projection.viable().is_empty() && projection.non_viable().is_empty(),
        "and nothing is ranked or pruned: {projection:?}"
    );

    drop(demand);
    drop(family);
    drop(client);
    for member in [&p1, &p2, &consumer] {
        let _ = std::fs::remove_dir_all(&member.dir);
    }
}

// ---------------------------------------------------------------------------
// The sensed RANK order, not merely the sensed/pruned split
// ---------------------------------------------------------------------------

/// Two providers, BOTH viable, differing only in the estimate they themselves
/// report. The one the sensing plane ranks first is not the one deterministic
/// selection would take, so the call must follow the RANK — not just the
/// viable/pruned partition, and not the candidate list's own order.
///
/// This is the discriminating half of "the production call consumes the sensed
/// order": with nothing pruned, an implementation that treated the ranked list
/// as an unordered set would still pass the pruning witness and fail here.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_production_call_follows_the_sensed_rank_between_viable_providers() {
    let owner = org();
    let p1 = mesh_in_org("rank-p1", &owner, true, None).await;
    let audience = shared_audience(&p1);
    let p2 = mesh_in_org("rank-p2", &owner, true, Some(&audience)).await;
    let consumer = mesh_in_org("rank-c", &owner, true, Some(&audience)).await;
    bring_up(&consumer, &[&p1, &p2]).await;

    let (low, high) = by_entity_order(&p1, &p2);
    let (low_id, high_id) = (low.node.node_id(), high.node.node_id());
    let low_calls = Arc::new(AtomicUsize::new(0));
    let high_calls = Arc::new(AtomicUsize::new(0));
    let _low_serve = serve(low, "slow-but-first", low_calls.clone());
    let _high_serve = serve(high, "fast-but-last", high_calls.clone());
    // BOTH Ready, so nothing is pruned; the deterministic-first provider is
    // the SLOW one, so only rank order can move the call.
    let _low_ready = provide_at(low, true, Duration::from_millis(400));
    let _high_ready = provide_at(high, true, Duration::from_millis(1));

    let client = bind(&consumer);
    converge_population(&consumer, &[&p1, &p2], 2).await;
    let _armed: Pong = client
        .call(SERVICE, &Ping { n: 1 })
        .await
        .expect("admitted");
    let family = client.sensing_binding().family().expect("Active").clone();
    let demand = {
        let deadline = Instant::now() + SETTLE;
        loop {
            if let Some(demand) = family.demand(&capability()) {
                let projection =
                    demand.project_sensed_order(Instant::now(), &ConsumerLatencyBudget::default());
                // Both viable, the FAST one ranked first, nothing pruned.
                if projection.viable() == [high_id, low_id] && projection.non_viable().is_empty() {
                    break demand;
                }
            }
            assert!(
                Instant::now() < deadline,
                "the two Ready providers never ranked by their own estimates"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };

    let low_before = low_calls.load(Ordering::SeqCst);
    let high_before = high_calls.load(Ordering::SeqCst);
    let reply: Pong = client
        .call(SERVICE, &Ping { n: 2 })
        .await
        .expect("the warmed protected call is admitted");
    assert_eq!(
        reply,
        Pong {
            served_by: "fast-but-last".to_string()
        },
        "the call must follow the sensed RANK, not the candidate list's order"
    );
    assert_eq!(high_calls.load(Ordering::SeqCst) - high_before, 1);
    assert_eq!(
        low_calls.load(Ordering::SeqCst) - low_before,
        0,
        "the slower viable provider is deprioritized, not called"
    );

    drop(demand);
    drop(family);
    drop(client);
    for member in [&p1, &p2, &consumer] {
        let _ = std::fs::remove_dir_all(&member.dir);
    }
}

// ---------------------------------------------------------------------------
// The fallback contract, at the production call edge
// ---------------------------------------------------------------------------

/// EVERY provider answers a fresh explicit NotReady. The accepted contract is
/// that this DEPRIORITIZES and nothing more: an all-pruned order falls back to
/// the caller's own deterministic order, no candidate is eliminated, and no new
/// error is manufactured. The call is admitted and served.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_all_not_ready_population_still_calls_in_the_deterministic_order() {
    let owner = org();
    let p1 = mesh_in_org("pruned-p1", &owner, true, None).await;
    let audience = shared_audience(&p1);
    let p2 = mesh_in_org("pruned-p2", &owner, true, Some(&audience)).await;
    let consumer = mesh_in_org("pruned-c", &owner, true, Some(&audience)).await;
    bring_up(&consumer, &[&p1, &p2]).await;

    let (low, high) = by_entity_order(&p1, &p2);
    let (low_id, high_id) = (low.node.node_id(), high.node.node_id());
    let low_calls = Arc::new(AtomicUsize::new(0));
    let high_calls = Arc::new(AtomicUsize::new(0));
    let _low_serve = serve(low, "low", low_calls.clone());
    let _high_serve = serve(high, "high", high_calls.clone());
    let _low_ready = provide(low, false);
    let _high_ready = provide(high, false);

    let client = bind(&consumer);
    converge_population(&consumer, &[&p1, &p2], 2).await;
    let _armed: Pong = client
        .call(SERVICE, &Ping { n: 1 })
        .await
        .expect("admitted");
    let family = client.sensing_binding().family().expect("Active").clone();
    let demand = {
        let deadline = Instant::now() + SETTLE;
        loop {
            if let Some(demand) = family.demand(&capability()) {
                let projection =
                    demand.project_sensed_order(Instant::now(), &ConsumerLatencyBudget::default());
                let mut pruned = projection.non_viable().to_vec();
                pruned.sort_unstable();
                let mut both = vec![low_id, high_id];
                both.sort_unstable();
                if projection.viable().is_empty() && pruned == both {
                    break demand;
                }
            }
            assert!(
                Instant::now() < deadline,
                "both providers should have answered a fresh explicit NotReady"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };

    let before = low_calls.load(Ordering::SeqCst);
    let reply: Pong = client
        .call(SERVICE, &Ping { n: 2 })
        .await
        .expect("an all-pruned order is not an error: the call is still admitted");
    assert_eq!(
        reply,
        Pong {
            served_by: "low".to_string()
        },
        "all-pruned falls back to the caller's own deterministic order"
    );
    assert_eq!(low_calls.load(Ordering::SeqCst) - before, 1);
    assert_eq!(
        population(&consumer).len(),
        2,
        "and a pruned provider is deprioritized, never de-authorized"
    );

    drop(demand);
    drop(family);
    drop(client);
    for member in [&p1, &p2, &consumer] {
        let _ = std::fs::remove_dir_all(&member.dir);
    }
}

/// Evidence that has EXPIRED cannot eliminate authorization or invent an error.
///
/// Both providers serve readiness, the consumer senses them, and then both
/// registrations are withdrawn: their beats stop and the observations age out.
/// The order that remains ranks and prunes nobody, so planning falls back to
/// the deterministic order and the call is still admitted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn expired_evidence_falls_back_and_manufactures_no_error() {
    let owner = org();
    let p1 = mesh_in_org("expire-p1", &owner, true, None).await;
    let audience = shared_audience(&p1);
    let p2 = mesh_in_org("expire-p2", &owner, true, Some(&audience)).await;
    let consumer = mesh_in_org("expire-c", &owner, true, Some(&audience)).await;
    bring_up(&consumer, &[&p1, &p2]).await;

    let (low, high) = by_entity_order(&p1, &p2);
    let high_id = high.node.node_id();
    let low_calls = Arc::new(AtomicUsize::new(0));
    let high_calls = Arc::new(AtomicUsize::new(0));
    let _low_serve = serve(low, "low", low_calls.clone());
    let _high_serve = serve(high, "high", high_calls.clone());
    let low_ready = provide(low, false);
    let high_ready = provide(high, true);

    let client = bind(&consumer);
    converge_population(&consumer, &[&p1, &p2], 2).await;
    let _armed: Pong = client
        .call(SERVICE, &Ping { n: 1 })
        .await
        .expect("admitted");
    let family = client.sensing_binding().family().expect("Active").clone();
    let demand = {
        let deadline = Instant::now() + SETTLE;
        loop {
            if let Some(demand) = family.demand(&capability()) {
                if demand
                    .project_sensed_order(Instant::now(), &ConsumerLatencyBudget::default())
                    .viable()
                    == [high_id]
                {
                    break demand;
                }
            }
            assert!(
                Instant::now() < deadline,
                "precondition: live evidence never established"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };

    // WITHDRAW readiness on both providers: no evaluator, so no further beats.
    assert!(low_ready.close(), "the low provider withdraws readiness");
    assert!(high_ready.close(), "the high provider withdraws readiness");

    // Age out. The consumer's cells expire to Unknown, which ranks and prunes
    // nobody - it is not a NotReady verdict.
    {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let projection =
                demand.project_sensed_order(Instant::now(), &ConsumerLatencyBudget::default());
            if projection.viable().is_empty() && projection.non_viable().is_empty() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "withdrawn readiness must age out to Unknown: {projection:?}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    let before = low_calls.load(Ordering::SeqCst);
    let high_before = high_calls.load(Ordering::SeqCst);
    let reply: Pong = client
        .call(SERVICE, &Ping { n: 2 })
        .await
        .expect("expired evidence is not an error: the call is still admitted");
    assert_eq!(
        reply,
        Pong {
            served_by: "low".to_string()
        },
        "with no live evidence the deterministic order returns"
    );
    assert_eq!(low_calls.load(Ordering::SeqCst) - before, 1);
    assert_eq!(
        population(&consumer).len(),
        2,
        "and both providers are still authorized"
    );
    assert_eq!(
        high_calls.load(Ordering::SeqCst) - high_before,
        0,
        "and the provider whose Ready evidence expired is no longer preferred"
    );

    drop(demand);
    drop(family);
    drop(client);
    for member in [&p1, &p2, &consumer] {
        let _ = std::fs::remove_dir_all(&member.dir);
    }
}

// ---------------------------------------------------------------------------
// W-49 / W-50 / W-51 — the binding owns the acquisition
// ---------------------------------------------------------------------------

/// One mint per bind, shared by every clone. An intermediate clone's drop
/// retires nothing; the last owner's drop retires everything, down to the
/// interest rows on the node. Two binds are independent.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_binding_owns_the_acquisition_and_the_last_owner_retires_it() {
    let owner = org();
    let p1 = mesh_in_org("own-p1", &owner, true, None).await;
    let audience = shared_audience(&p1);
    let p2 = mesh_in_org("own-p2", &owner, true, Some(&audience)).await;
    let consumer = mesh_in_org("own-c", &owner, true, Some(&audience)).await;
    bring_up(&consumer, &[&p1, &p2]).await;

    let calls = Arc::new(AtomicUsize::new(0));
    let _s1 = serve(&p1, "p1", calls.clone());
    let _s2 = serve(&p2, "p2", calls.clone());
    let _r1 = provide(&p1, true);
    let _r2 = provide(&p2, true);

    let client = bind(&consumer);
    // W-49: the mint happened at BIND, once, and produced one family.
    let family = client.sensing_binding().family().expect("Active").clone();
    assert_eq!(
        family.owners(),
        2,
        "the client and this witness's handle are the only owners"
    );
    assert!(
        family.capabilities().is_empty(),
        "and nothing is retained before a call needs it"
    );

    converge_population(&consumer, &[&p1, &p2], 2).await;
    let _reply: Pong = client
        .call(SERVICE, &Ping { n: 1 })
        .await
        .expect("admitted");
    assert_eq!(
        family.capabilities(),
        vec![capability()],
        "the call path retained demand for exactly the capability it planned"
    );
    assert_eq!(
        family
            .demand(&capability())
            .expect("demand")
            .retained_providers()
            .len(),
        2,
        "one exact interest per provider"
    );
    assert!(
        !consumer.node.sensing_table_is_empty(),
        "and the interest is real: the node holds the rows"
    );

    // W-49: a clone SHARES the acquisition - it never mints a second one.
    let clone = client.clone();
    assert_eq!(
        family.owners(),
        3,
        "the clone shares the family body rather than minting its own"
    );
    let clone_family = clone.sensing_binding().family().expect("Active").clone();
    assert!(
        Arc::ptr_eq(clone_family.node(), family.node()),
        "and it is bound to the same node"
    );
    assert_eq!(clone_family.capabilities(), vec![capability()]);
    drop(clone_family);

    // W-50: an intermediate clone's drop retires NOTHING.
    drop(clone);
    assert!(
        family.demand(&capability()).is_some(),
        "an intermediate clone's release must not retire a surviving client's demand"
    );
    assert!(
        !consumer.node.sensing_table_is_empty(),
        "and the rows must still be installed"
    );
    let _reply: Pong = client
        .call(SERVICE, &Ping { n: 2 })
        .await
        .expect("the surviving client still calls");

    // Independent binds are independent: a second bind mints its OWN family.
    let other = bind(&consumer);
    let other_family = other.sensing_binding().family().expect("Active").clone();
    assert!(
        other_family.capabilities().is_empty(),
        "a separate bind starts with its own empty acquisition"
    );
    assert_eq!(
        family.capabilities(),
        vec![capability()],
        "and the first bind's demand is untouched by it"
    );
    assert_eq!(
        other_family.owners(),
        2,
        "the two binds do not share one body"
    );
    drop(other_family);
    drop(other);

    // W-51: the LAST owner's drop retires everything.
    drop(client);
    assert_eq!(
        family.owners(),
        1,
        "this witness now holds the only reference"
    );
    assert!(
        family.demand(&capability()).is_some(),
        "which is exactly why nothing has been retired yet"
    );
    drop(family);
    let deadline = Instant::now() + SETTLE;
    loop {
        if consumer.node.sensing_table_is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the last owner's release must retire every retained interest"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    for member in [&p1, &p2, &consumer] {
        let _ = std::fs::remove_dir_all(&member.dir);
    }
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
