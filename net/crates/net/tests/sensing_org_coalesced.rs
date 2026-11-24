//! SDK plan LS-7a — coalesced (leader-backed) organization sensing over REAL
//! transport, with the leader ELECTED rather than assumed.
//!
//! What the leader track's LS-6 proof could not show, because it installed the
//! role by hand (`assume_sensing_leader`) and refreshed the registration from
//! the test:
//!
//! - every coalescing node holds the org leader role from `start()` (D-LS14),
//!   and LEGACY provider-free intake stays dark until a node explicitly assumes
//!   the legacy role;
//! - the consumer ELECTS its leader per capability from its own view: the
//!   owner-private announcers minus the capability's providers (D-LS15);
//! - the node owns the registration and its refresh, and fails over to the
//!   next member when the leader goes (D-LS16);
//! - a coalesced read is clamped to the consumer's current visibility, so a
//!   provider whose announcement expired leaves the rows at once (D-LS17);
//! - the last release drops the consumer's expectation and its cells.
//!
//! Run: `cargo test --features "cortex fixtures" --test sensing_org_coalesced`

#![cfg(all(feature = "net", feature = "redex", feature = "fixtures"))]

mod common;
use common::*;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::behavior::org::{OrgKeypair, OrgMembershipCert};
use net::adapter::net::behavior::org_authority::NodeAuthority;
use net::adapter::net::behavior::org_scoped_ann::ScopedCapabilityAnnouncement;
use net::adapter::net::behavior::sensing::org_coalesced::{
    OrgCoalescedRefused, OrgCoalescedTicket,
};
use net::adapter::net::behavior::sensing::{
    AudienceScopeCommitment, CanonicalConstraints, CapabilityId, DisclosureClass,
    EvaluationRequest, Incarnation, InterestSpec, ProjectedReadiness, ProviderSelector,
    ReadinessEvaluation, ReadinessEvaluator, ResultMode, WorkLatencyEnvelope,
};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};
use net::adapter::Adapter;

const CAP: &str = "nrpc:coalesced-print";
/// The service the leader candidates announce, so they are organization
/// members in the consumer's view without providing CAP.
const RELAY_SVC: &str = "nrpc:coalesced-relay";
/// The node-wide soft-state horizon: refresh every 1.25 s, register at ttl/2.
const SENSING_TTL: Duration = Duration::from_secs(5);

fn org() -> OrgKeypair {
    OrgKeypair::from_bytes([0x57u8; 32])
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_secs()
}

/// Never deleted: the revocation store's process-global core registry is keyed
/// by its sidecar's inode (see `sensing_org_exact_seam.rs`).
struct ScratchDir(PathBuf);

impl ScratchDir {
    fn new(tag: &str) -> Self {
        let salt = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "net-org-coalesced-{tag}-{}-{salt:x}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Self(dir)
    }
}

fn base_config(session_timeout: Duration) -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, CHAOS_PSK)
        .with_heartbeat_interval(Duration::from_millis(100))
        .with_session_timeout(session_timeout)
        .with_handshake(3, Duration::from_secs(2))
        .with_sensing_coalescing(true)
        .with_sensing_interest_ttl(SENSING_TTL);
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: CHAOS_BUFFER_SIZE,
        recv_buffer_size: CHAOS_BUFFER_SIZE,
    };
    cfg
}

async fn org_node(provider: bool, session_timeout: Duration) -> Arc<MeshNode> {
    let mut cfg = base_config(session_timeout);
    if provider {
        cfg = cfg.with_sensing_incarnation(Incarnation::new(1));
    }
    Arc::new(
        MeshNode::new(EntityKeypair::generate(), cfg)
            .await
            .expect("MeshNode::new"),
    )
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

/// Seal a REAL owner-scoped announcement of `tags` from `member` to `to`'s own
/// owner audience, valid until `expires_at`, and ingest it through the ordinary
/// verified path.
fn announce(to: &Arc<MeshNode>, member: &Arc<MeshNode>, tags: &[&str], expires_at: u64) {
    let authority = to
        .node_authority()
        .expect("the receiver must be organization-authoritative");
    let cert = OrgMembershipCert::try_issue(&org(), member.entity_id().clone(), 1, 3600)
        .expect("member cert");
    let mut descriptor = CapabilitySet::new();
    for tag in tags {
        descriptor = descriptor.add_tag(*tag);
    }
    let envelope = ScopedCapabilityAnnouncement::build_owner(
        member.entity_keypair(),
        org().org_id(),
        cert,
        authority.audience.audience_handle,
        authority.audience.discovery_key(),
        1,
        expires_at,
        &descriptor.to_bytes_compact(),
    )
    .expect("owner-scoped envelope");
    to.ingest_scoped_announcement_for_test(&envelope.to_bytes());
}

struct AlwaysReady;

impl ReadinessEvaluator for AlwaysReady {
    fn evaluate(&self, _request: &EvaluationRequest<'_>) -> ReadinessEvaluation {
        ReadinessEvaluation::Ready {
            estimated_start: Some(Duration::from_millis(5)),
        }
    }
}

fn start_within() -> WorkLatencyEnvelope {
    WorkLatencyEnvelope::start_within(Duration::from_secs(2))
}

/// A started, org-authoritative mesh: `leaders` relay nodes between consumer A
/// and provider P (A — Rᵢ — P), each Rᵢ announcing RELAY_SVC to A, and P
/// announcing CAP to every Rᵢ and to A until `provider_expiry`. Nothing assumes
/// a leader role.
struct Mesh {
    a: Arc<MeshNode>,
    leaders: Vec<Arc<MeshNode>>,
    p: Arc<MeshNode>,
    _dirs: Vec<ScratchDir>,
}

impl Mesh {
    async fn build(
        tag: &str,
        leaders: usize,
        provider_expiry: u64,
        session_timeout: Duration,
        direct_consumer_provider: bool,
    ) -> Self {
        let a = org_node(false, session_timeout).await;
        let p = org_node(true, session_timeout).await;
        let mut relays = Vec::new();
        for _ in 0..leaders {
            relays.push(org_node(false, session_timeout).await);
        }
        let mut dirs = vec![
            adopt_and_install(&a, &format!("{tag}-a")),
            adopt_and_install(&p, &format!("{tag}-p")),
        ];
        for (index, relay) in relays.iter().enumerate() {
            dirs.push(adopt_and_install(relay, &format!("{tag}-r{index}")));
        }
        p.register_readiness_evaluator(CapabilityId::new(CAP), Arc::new(AlwaysReady))
            .expect("evaluator registers");
        for relay in &relays {
            connect_pair(&a, relay).await;
            connect_pair(relay, &p).await;
        }
        if direct_consumer_provider {
            connect_pair(&a, &p).await;
        }
        let all: Vec<&Arc<MeshNode>> = [&a, &p].into_iter().chain(relays.iter()).collect();
        for node in &all {
            node.start();
        }
        for node in &all {
            node.announce_capabilities(CapabilitySet::new())
                .await
                .expect("announce");
        }
        await_condition(Duration::from_secs(5), "entity pins established", || {
            relays.iter().all(|relay| {
                a.peer_entity_id(relay.node_id()).is_some()
                    && relay.peer_entity_id(p.node_id()).is_some()
                    && p.peer_entity_id(relay.node_id()).is_some()
            }) && (!direct_consumer_provider || a.peer_entity_id(p.node_id()).is_some())
        })
        .await;
        // The verifying-hop origin pin: A verifies P's signatures against P.
        a.test_pin_peer_entity(p.node_id(), p.entity_id().clone());
        let far = now_secs() + 3600;
        for relay in &relays {
            announce(relay, &p, &[CAP], far);
            announce(&a, relay, &[RELAY_SVC], far);
        }
        announce(&a, &p, &[CAP], provider_expiry);
        Self {
            a,
            leaders: relays,
            p,
            _dirs: dirs,
        }
    }

    fn leader_by_id(&self, id: u64) -> &Arc<MeshNode> {
        self.leaders
            .iter()
            .find(|relay| relay.node_id() == id)
            .expect("the elected leader is one of the relays")
    }

    /// A's coalesced row for P, if any.
    fn p_row(&self, ticket: &OrgCoalescedTicket) -> Option<ProjectedReadiness> {
        self.a
            .org_coalesced_rows(ticket)
            .and_then(|rows| {
                rows.into_iter()
                    .find(|(provider, _, _)| *provider == self.p.node_id())
            })
            .map(|(_, readiness, _)| readiness)
    }

    async fn teardown(self) {
        for node in [&self.a, &self.p].into_iter().chain(self.leaders.iter()) {
            let _ = node.shutdown().await;
        }
    }
}

/// D-LS14 + D-LS15 + D-LS16, end to end: A elects R from its own view, holds a
/// coalesced interest, and P's proofs come back through R — with no role
/// assumed anywhere. Then two holders share one interest, and the LAST release
/// drops A's expectation and its consumer cells.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_coalesced_interest_reaches_the_elected_leader_without_assuming_the_role() {
    let mesh = Mesh::build("e2e", 1, now_secs() + 3600, Duration::from_secs(10), false).await;
    let (a, r, p) = (&mesh.a, &mesh.leaders[0], &mesh.p);
    assert!(
        !r.serves_legacy_sensing_leader_intake(),
        "the org role at R does not open legacy intake"
    );
    assert_eq!(
        a.org_sensing_leader_for(CAP),
        Some(r.node_id()),
        "A elects R: the only member that does not provide CAP"
    );
    let ticket = a
        .acquire_org_coalesced_interest(CAP, start_within(), ResultMode::Any)
        .expect("the coalesced interest is held");
    assert_eq!(a.org_coalesced_leader(&ticket), Some(r.node_id()));

    await_condition(Duration::from_secs(10), "R coalesces the interest", || {
        r.sensing_leader_interest_count() == Some(1)
    })
    .await;
    assert_eq!(
        r.sensing_leader_branches(ticket.interest()),
        Some(vec![p.node_id()]),
        "R resolved P from its own owner-private discovery"
    );
    await_condition(Duration::from_secs(15), "A projects P Ready", || {
        mesh.p_row(&ticket) == Some(ProjectedReadiness::Ready)
    })
    .await;

    // A second holder joins the SAME interest; releasing the first leaves it.
    let second = a
        .acquire_org_coalesced_interest(CAP, start_within(), ResultMode::Any)
        .expect("a second holder joins");
    assert_eq!(second.interest(), ticket.interest());
    assert_eq!(a.org_coalesced_metrics().held_interests, 1);
    assert!(a.release_org_coalesced_interest(&ticket));
    assert!(!a.release_org_coalesced_interest(&ticket), "idempotent");
    assert!(a.org_coalesced_holds(&second));
    assert!(a.org_coalesced_rows(&second).is_some());

    // The LAST release drops the expectation and reconciles the cells away.
    assert!(a.release_org_coalesced_interest(&second));
    assert_eq!(a.org_coalesced_metrics().held_interests, 0);
    assert!(a.org_coalesced_rows(&second).is_none());
    assert!(
        a.sensing_branch_projections(second.interest()).is_empty(),
        "no consumer cell survives the last release"
    );
    mesh.teardown().await;
}

/// D-LS15: the electorate excludes the providers of the watched capability —
/// per capability, so the same members elect differently for different
/// capabilities.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_electorate_excludes_the_providers_of_the_capability() {
    let mesh = Mesh::build(
        "electorate",
        1,
        now_secs() + 3600,
        Duration::from_secs(10),
        false,
    )
    .await;
    let (a, r, p) = (&mesh.a, &mesh.leaders[0], &mesh.p);
    assert_eq!(a.org_sensing_leader_for(CAP), Some(r.node_id()));
    assert_eq!(
        a.org_sensing_leader_for(RELAY_SVC),
        Some(p.node_id()),
        "R provides RELAY_SVC, so P (a CAP-only announcer) leads it"
    );
    mesh.teardown().await;
}

/// D-LS15 / owner decision 4: no electable leader is a typed refusal that holds
/// nothing. A sees only P, P provides the capability, and P is DIRECTLY
/// connected — reachable and healthy — so only the provider exclusion keeps it
/// out of the electorate.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_electable_leader_is_a_typed_refusal_that_holds_nothing() {
    let mesh = Mesh::build(
        "noleader",
        0,
        now_secs() + 3600,
        Duration::from_secs(10),
        true,
    )
    .await;
    let a = &mesh.a;
    assert_eq!(a.org_sensing_leader_for(CAP), None);
    assert_eq!(
        a.acquire_org_coalesced_interest(CAP, start_within(), ResultMode::Any)
            .unwrap_err(),
        OrgCoalescedRefused::NoLeader
    );
    assert_eq!(a.org_coalesced_metrics().held_interests, 0);
    // An unsupported mode is refused before anything else.
    for mode in [ResultMode::TopK(0), ResultMode::TopK(33), ResultMode::Each] {
        assert_eq!(
            a.acquire_org_coalesced_interest(CAP, start_within(), mode)
                .unwrap_err(),
            OrgCoalescedRefused::UnsupportedMode,
            "{mode:?}"
        );
    }
    mesh.teardown().await;
}

/// D-LS16: when the elected leader goes, the node's own refresh re-elects and
/// re-registers at the next member, and P's proofs resume through it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lost_leader_fails_over_to_the_next_member() {
    let mesh = Mesh::build(
        "failover",
        2,
        now_secs() + 3600,
        Duration::from_secs(2),
        false,
    )
    .await;
    let (a, p) = (&mesh.a, &mesh.p);
    let ticket = a
        .acquire_org_coalesced_interest(CAP, start_within(), ResultMode::Any)
        .expect("held");
    let first = a.org_coalesced_leader(&ticket).expect("a leader");
    let other = mesh
        .leaders
        .iter()
        .map(|relay| relay.node_id())
        .find(|id| *id != first)
        .expect("a second member");
    await_condition(
        Duration::from_secs(15),
        "A projects P Ready via the first leader",
        || mesh.p_row(&ticket) == Some(ProjectedReadiness::Ready),
    )
    .await;

    let _ = mesh.leader_by_id(first).shutdown().await;
    await_condition(
        Duration::from_secs(20),
        "A fails over to the other member",
        || a.org_coalesced_leader(&ticket) == Some(other),
    )
    .await;
    assert!(a.org_coalesced_metrics().failovers_total >= 1);
    let successor = mesh.leader_by_id(other);
    await_condition(
        Duration::from_secs(10),
        "the successor holds the interest",
        || successor.sensing_leader_interest_count() == Some(1),
    )
    .await;
    assert_eq!(
        successor.sensing_leader_branches(ticket.interest()),
        Some(vec![p.node_id()])
    );
    assert!(a.release_org_coalesced_interest(&ticket));
    mesh.teardown().await;
}

/// D-LS17: a coalesced read is CLAMPED to current visibility. When P's
/// announcement expires in A's view, P leaves A's rows at once, although A's
/// consumer cell for it still exists.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_expired_provider_leaves_the_coalesced_rows_on_the_next_read() {
    let expiry = now_secs() + 14;
    let mesh = Mesh::build("clamp", 1, expiry, Duration::from_secs(10), false).await;
    let (a, p) = (&mesh.a, &mesh.p);
    let ticket = a
        .acquire_org_coalesced_interest(CAP, start_within(), ResultMode::Any)
        .expect("held");
    await_condition(Duration::from_secs(15), "A projects P Ready", || {
        mesh.p_row(&ticket) == Some(ProjectedReadiness::Ready)
    })
    .await;
    assert!(
        now_secs() < expiry,
        "the Ready row was observed before the expiry (raise the window if this fires)"
    );
    await_condition(
        Duration::from_secs(25),
        "P's announcement expired at A",
        || now_secs() > expiry,
    )
    .await;
    assert_eq!(
        mesh.p_row(&ticket),
        None,
        "the expired provider is clamped out"
    );
    assert!(
        a.sensing_branch_projections(ticket.interest())
            .iter()
            .any(|(provider, _, _)| *provider == p.node_id()),
        "the consumer cell is still there: only the clamp removed the row"
    );
    assert!(a.release_org_coalesced_interest(&ticket));
    mesh.teardown().await;
}

/// D-LS14: legacy provider-free intake stays DARK on a node that only holds the
/// automatically installed org role, and opens only on an explicit
/// `assume_sensing_leader` (the positive control proves the frames arrive).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn legacy_intake_stays_dark_until_the_legacy_role_is_assumed() {
    // The legacy shape of `sensing_leader_delivery.rs`: P holds the owner
    // identity and announces the capability in the plaintext fold.
    let owner_kp = EntityKeypair::generate();
    let owner_entity = owner_kp.entity_id().clone();
    let owner = AudienceScopeCommitment::owner_root(&owner_entity);
    let mk = |kp: EntityKeypair, incarnation: Option<Incarnation>| async move {
        let mut cfg = base_config(Duration::from_secs(10)).with_sensing_owner_root(owner);
        if let Some(incarnation) = incarnation {
            cfg = cfg.with_sensing_incarnation(incarnation);
        }
        Arc::new(MeshNode::new(kp, cfg).await.expect("MeshNode::new"))
    };
    let a = mk(EntityKeypair::generate(), None).await;
    let r = mk(EntityKeypair::generate(), None).await;
    let p = mk(owner_kp, Some(Incarnation::new(1))).await;
    p.register_readiness_evaluator(CapabilityId::new("print.legacy"), Arc::new(AlwaysReady))
        .expect("evaluator registers");
    connect_pair(&a, &r).await;
    connect_pair(&r, &p).await;
    for node in [&a, &r, &p] {
        node.start();
    }
    for node in [&a, &r] {
        node.announce_capabilities(CapabilitySet::new())
            .await
            .expect("announce");
    }
    p.announce_capabilities(CapabilitySet::new().add_tag("print.legacy"))
        .await
        .expect("announce P");
    let p_id = p.node_id();
    await_condition(Duration::from_secs(5), "R sees P", || {
        r.peer_entity_id(p_id).is_some()
            && r.sensing_candidate_snapshot(&CapabilityId::new("print.legacy"))
                .iter()
                .any(|candidate| candidate.node_id == p_id && candidate.authorized)
    })
    .await;
    let spec = InterestSpec {
        capability_id: CapabilityId::new("print.legacy"),
        constraints: CanonicalConstraints::default(),
        work_latency: WorkLatencyEnvelope::start_within(Duration::from_secs(5)),
        providers: ProviderSelector::AnyAuthorized,
        result_mode: ResultMode::Any,
        disclosure_class: DisclosureClass::Owner,
        audience: owner,
    };
    let send = || {
        a.register_capability_interest(&spec, r.node_id(), Duration::from_millis(100), SENSING_TTL)
            .expect("the legacy registration is authored")
    };
    assert_eq!(
        r.sensing_leader_interest_count(),
        Some(0),
        "R holds the org role from start()"
    );
    assert!(!r.serves_legacy_sensing_leader_intake());
    for _ in 0..10 {
        send();
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(
        r.sensing_leader_interest_count(),
        Some(0),
        "no legacy registration was served"
    );
    // Positive control: the SAME frames are served once the legacy role is
    // assumed explicitly.
    assert!(r.assume_sensing_leader());
    assert!(r.serves_legacy_sensing_leader_intake());
    let mut served = false;
    for _ in 0..25 {
        send();
        tokio::time::sleep(Duration::from_millis(200)).await;
        if r.sensing_leader_interest_count() == Some(1) {
            served = true;
            break;
        }
    }
    assert!(served, "the legacy registration is served after assumption");
    for node in [&a, &r, &p] {
        let _ = node.shutdown().await;
    }
}
