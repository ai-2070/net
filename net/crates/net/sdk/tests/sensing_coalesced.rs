//! SDK plan LS-7b — the COALESCED consumer watch, end to end through the
//! shipped surface: `SensingQuery::coalesced`, `SensingClient::watch`,
//! `SensingWatch::{snapshot, close}`, `SensingSnapshot::is_coalesced`.
//!
//! Every member joins the organization the ordinary way: an installed
//! authority, and an owner-scoped service registered with `Mesh::serve_org`,
//! whose announcement is what puts it into the others' private discovery.
//! Nothing assumes a leader role: the consumer ELECTS the leader from its own
//! view (D-LS15), and the leader serves because every coalescing node holds
//! the org role (D-LS14).
//!
//! What these hold:
//!
//! - signed readiness from a real evaluator reaches a coalesced snapshot
//!   through the elected leader, and the snapshot says it is coalesced;
//! - two coalesced watches share ONE hold, and a close releases only its own;
//! - with no member able to lead, the watch is refused with its own kind and
//!   NOTHING is acquired — in particular no exact-provider lease, so there is
//!   no silent fallback;
//! - an out-of-range `Top(k)` is refused before any state is touched.
#![cfg(all(feature = "net", feature = "cortex", feature = "fixtures"))]

use std::sync::Arc;
use std::time::{Duration, Instant};

use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::behavior::sensing::{
    CapabilityId, EvaluationRequest, Incarnation, ReadinessEvaluation, ReadinessEvaluator,
};
use net::adapter::net::{ChannelConfigRegistry, MeshNode, MeshNodeConfig};
use net_sdk::identity::Identity;
use net_sdk::mesh::Mesh;
use net_sdk::mesh_rpc::ServeHandle;
use net_sdk::org::types::{NodeAuthority, OrgKeypair, OrgMembershipCert, OwnerAudienceCredential};
use net_sdk::org::{OrgAccess, OrgCaller};
use net_sdk::sensing::{
    Coalesced, ReadinessRegistration, SensedViability, SensingError, SensingQuery, SensingWatch,
};

/// The capability the consumer watches: P's owner-scoped service tag.
const TAG: &str = "nrpc:coalesced.reindex";
const SERVICE: &str = "coalesced.reindex";
/// The leader candidate's own, unrelated owner-scoped service.
const RELAY_SERVICE: &str = "coalesced.relay";
const SETTLE: Duration = Duration::from_secs(20);

fn org() -> OrgKeypair {
    OrgKeypair::from_bytes([0x73u8; 32])
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Ping {
    n: u32,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Pong {
    ok: bool,
}

struct AlwaysReady;

impl ReadinessEvaluator for AlwaysReady {
    fn evaluate(&self, _request: &EvaluationRequest<'_>) -> ReadinessEvaluation {
        ReadinessEvaluation::Ready {
            estimated_start: Some(Duration::from_millis(7)),
        }
    }
}

struct Member {
    mesh: Mesh,
    node: Arc<MeshNode>,
}

/// One organization member. Every member after the first takes the FIRST one's
/// owner discovery audience: each adoption mints its own, and members must
/// share one to read each other's owner-scoped announcements.
async fn member(tag: &str, audience: Option<&OwnerAudienceCredential>) -> Member {
    let identity = Identity::generate();
    let mut cfg = MeshNodeConfig::new("127.0.0.1:0".parse().expect("addr"), [0x73u8; 32])
        .with_heartbeat_interval(Duration::from_millis(100))
        .with_session_timeout(Duration::from_secs(10))
        .with_sensing_coalescing(true)
        .with_sensing_incarnation(Incarnation::new(1))
        .with_sensing_interest_ttl(Duration::from_secs(5));
    cfg.min_announce_interval = Duration::from_millis(50);
    cfg.configured_identity = true;
    let mut node = MeshNode::new((**identity.keypair()).clone(), cfg)
        .await
        .expect("MeshNode::new");
    let configs = Arc::new(ChannelConfigRegistry::new());
    node.set_channel_configs(configs.clone());
    let node = Arc::new(node);
    let entity = identity.entity_id().clone();
    let cert = OrgMembershipCert::try_issue(&org(), entity.clone(), 1, 3600).expect("cert");
    let dir = std::env::temp_dir().join(format!(
        "net-ls7b-{tag}-{}-{:?}",
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
    let mesh = Mesh::from_node_arc(node.clone(), configs, Some(identity));
    Member { mesh, node }
}

/// One handshake: `from` dials `to`.
async fn link(from: &Member, to: &Member) {
    let to_pub = *to.mesh.public_key();
    let to_addr = to.mesh.local_addr().to_string();
    let target = Arc::clone(&to.node);
    let from_id = from.mesh.node_id();
    let accept = tokio::spawn(async move { target.accept(from_id).await });
    from.mesh
        .connect(&to_addr, &to_pub, to.mesh.node_id())
        .await
        .expect("connect");
    accept.await.expect("accept task").expect("accept");
}

/// The organization's shared owner discovery audience, copied out of a member
/// that already adopted it.
fn audience_of(member: &Member) -> OwnerAudienceCredential {
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

fn serve(member: &Member, service: &str) -> ServeHandle {
    member
        .mesh
        .serve_org(
            service,
            OrgAccess::SameOrg,
            move |_caller: OrgCaller, _req: Ping| async move { Ok(Pong { ok: true }) },
        )
        .expect("serve_org")
}

fn provide(member: &Member) -> ReadinessRegistration {
    member
        .mesh
        .sensing()
        .expect("sensing binds")
        .provide(CapabilityId::new(TAG), Arc::new(AlwaysReady))
        .expect("provide readiness")
}

fn watch(member: &Member, query: SensingQuery) -> Result<SensingWatch, SensingError> {
    member.mesh.sensing().expect("sensing binds").watch(query)
}

async fn until(what: &str, deadline: Duration, mut probe: impl FnMut() -> bool) {
    let end = Instant::now() + deadline;
    while !probe() {
        assert!(Instant::now() < end, "timed out waiting for: {what}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Start everyone, announce (which seals each member's owner-scoped envelope
/// from its served services), and re-announce until `ready` holds.
async fn bring_up(members: &[&Member], what: &str, mut ready: impl FnMut() -> bool) {
    for member in members {
        member.mesh.start().expect("mesh start");
    }
    let end = Instant::now() + SETTLE;
    loop {
        for member in members {
            member
                .node
                .announce_capabilities(CapabilitySet::new())
                .await
                .expect("announce");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        if ready() {
            return;
        }
        assert!(Instant::now() < end, "timed out waiting for: {what}");
    }
}

/// Consumer A, leader candidate R (serving an unrelated service) and provider
/// P (serving TAG and providing its readiness): A — R, A — P, R — P.
struct Org {
    a: Member,
    r: Member,
    p: Member,
    _served: Vec<ServeHandle>,
    _provided: ReadinessRegistration,
}

async fn org_of_three() -> Org {
    let a = member("a", None).await;
    let shared = audience_of(&a);
    let r = member("r", Some(&shared)).await;
    let p = member("p", Some(&shared)).await;
    let served = vec![serve(&r, RELAY_SERVICE), serve(&p, SERVICE)];
    let provided = provide(&p);
    link(&a, &r).await;
    link(&a, &p).await;
    link(&r, &p).await;
    let (r_id, p_id) = (r.node.node_id(), p.node.node_id());
    bring_up(&[&a, &r, &p], "A elects R and R sees P", || {
        a.node.org_sensing_leader_for(TAG) == Some(r_id)
            && r.node.peer_entity_id(p_id).is_some()
            && !r
                .node
                .owner_private_capability_providers(
                    &net_sdk::org::types::CapabilityAuthorityId::for_tag(TAG),
                )
                .is_empty()
    })
    .await;
    Org {
        a,
        r,
        p,
        _served: served,
        _provided: provided,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_coalesced_watch_reports_signed_readiness_through_the_elected_leader() {
    let org = org_of_three().await;
    let p_id = org.p.node.node_id();
    let mut watched = watch(&org.a, SensingQuery::new(TAG).coalesced(Coalesced::Best))
        .expect("the coalesced watch opens");
    until("R coalesces the interest", SETTLE, || {
        org.r.node.sensing_leader_interest_count() == Some(1)
    })
    .await;
    let mut seen = None;
    until("a coalesced snapshot ranks P viable", SETTLE, || {
        let snapshot = watched.snapshot().expect("snapshot");
        assert!(snapshot.is_coalesced(), "a coalesced watch says so");
        let viable = snapshot
            .provider(p_id)
            .is_some_and(|row| row.viability() == SensedViability::Viable);
        if viable {
            seen = Some(snapshot);
        }
        viable
    })
    .await;
    let snapshot = seen.expect("captured");
    assert_eq!(snapshot.preferred(), Some(p_id));
    assert_eq!(
        snapshot
            .provider(p_id)
            .and_then(|row| row.estimated_start()),
        Some(Duration::from_millis(7)),
        "the provider-signed start estimate rides through the leader"
    );
    assert!(watched.close());
    assert_eq!(
        org.a.node.org_coalesced_metrics().held_interests,
        0,
        "close released the hold"
    );
    assert_eq!(watched.snapshot().unwrap_err(), SensingError::WatchClosed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_coalesced_watches_share_one_hold_and_close_releases_only_its_own() {
    let org = org_of_three().await;
    let query = || SensingQuery::new(TAG).coalesced(Coalesced::Top(2));
    let mut first = watch(&org.a, query()).expect("first");
    let mut second = watch(&org.a, query()).expect("second");
    assert_eq!(org.a.node.org_coalesced_metrics().held_interests, 1);
    assert!(first.close());
    assert_eq!(
        org.a.node.org_coalesced_metrics().held_interests,
        1,
        "the survivor's hold stands"
    );
    assert!(second
        .snapshot()
        .expect("the survivor reads")
        .is_coalesced());
    drop(second);
    assert_eq!(
        org.a.node.org_coalesced_metrics().held_interests,
        0,
        "drop releases the last hold"
    );
    assert_eq!(first.snapshot().unwrap_err(), SensingError::WatchClosed);
}

/// No member can lead TAG: A sees only P, and P provides it. The coalesced
/// watch is refused with its own kind, acquires NOTHING — no coalesced hold
/// and no exact-provider lease — and an exact watch the caller opens itself
/// still works, so the refusal is not a disguised exact watch either way.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_electable_leader_refuses_with_its_own_kind_and_never_falls_back() {
    let a = member("solo-a", None).await;
    let p = member("solo-p", Some(&audience_of(&a))).await;
    let _served = serve(&p, SERVICE);
    let _provided = provide(&p);
    link(&a, &p).await;
    let tag_id = net_sdk::org::types::CapabilityAuthorityId::for_tag(TAG);
    bring_up(&[&a, &p], "A discovers P", || {
        !a.node
            .owner_private_capability_providers(&tag_id)
            .is_empty()
    })
    .await;
    let leases_before = a.node.sensing_interest_count();
    let refused = watch(&a, SensingQuery::new(TAG).coalesced(Coalesced::Best)).unwrap_err();
    assert_eq!(refused, SensingError::NoSensingLeader);
    assert_eq!(refused.kind(), "no_sensing_leader");
    assert!(!refused.is_terminal());
    assert_eq!(a.node.org_coalesced_metrics().held_interests, 0);
    assert_eq!(
        a.node.sensing_interest_count(),
        leases_before,
        "no exact-provider row was acquired in its place"
    );
    let mut exact = watch(&a, SensingQuery::new(TAG)).expect("an exact watch still opens");
    assert!(!exact.snapshot().expect("exact snapshot").is_coalesced());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_out_of_range_top_is_refused_before_any_state() {
    let a = member("range", None).await;
    for k in [0u16, 33, u16::MAX] {
        let refused = watch(&a, SensingQuery::new(TAG).coalesced(Coalesced::Top(k))).unwrap_err();
        assert_eq!(refused, SensingError::UnsatisfiableCoalescing, "Top({k})");
        assert_eq!(refused.kind(), "unsatisfiable_coalescing");
    }
    assert_eq!(a.node.org_coalesced_metrics().held_interests, 0);
}
