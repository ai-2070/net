//! Leader plan LS-6 — the provider-free ORGANIZATION sensing leader over REAL
//! transport: consumer A → leader R → provider P, all members of one org.
//!
//! The lighting slice's load-bearing claim (plan §8 "Lighting slice"): an
//! `OrgCapabilityRegistration` from A reaches R's lit dispatch arm, R resolves it
//! against its OWN owner-private discovery (no plaintext fold, no legacy gate),
//! re-authors a fresh `OrgProviderRegistration` to P under R's OWN live
//! membership (D-LS8), P admits it at its org gate (D-LS12) and streams signed
//! beats back to R's Leader row, and R's relay fans them to A — which accepts
//! them only because P is a current owner-private provider in A's OWN store
//! (D-LS6). Every frame on the path is an organization frame authenticated at
//! the hop that admits it; nothing falls back to legacy.
//!
//! The negative witness drops only A's own discovery of P: the leader still
//! senses P, the proofs still arrive at A, and A refuses every one of them.
//!
//! Run: `cargo test --features "cortex fixtures" --test sensing_org_leader_three_node`

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
use net::adapter::net::behavior::sensing::{
    canonical_org_sensing_commitment, AttestedStatus, CanonicalConstraints, CapabilityId,
    DisclosureClass, DownstreamId, EvaluationRequest, Incarnation, InterestSpec,
    ProjectedReadiness, ProviderInterestKey, ProviderSelector, ReadinessEvaluation,
    ReadinessEvaluator, ResultMode, SensingCounters, WorkLatencyEnvelope,
};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};
use net::adapter::Adapter;

const CAP: &str = "nrpc:leader-print";
/// Consumer soft-state lifetime — generous against CI hiccups.
const TTL: Duration = Duration::from_millis(1500);
/// Requested sample interval D.
const D: Duration = Duration::from_millis(100);
/// Consumer refresh cadence.
const REFRESH: Duration = Duration::from_millis(200);
/// The node-wide soft-state horizon.
const SENSING_TTL: Duration = Duration::from_secs(5);

fn org() -> OrgKeypair {
    OrgKeypair::from_bytes([0x42u8; 32])
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_secs()
}

/// A scratch directory that is deliberately NEVER deleted: the revocation
/// store's process-global core registry is keyed by its sidecar's inode, and a
/// recycled inode would hand a later store this test's live core (see
/// `sensing_org_exact_seam.rs`). Salted with nanoseconds because pids recycle.
struct ScratchDir(PathBuf);

impl ScratchDir {
    fn new(tag: &str) -> Self {
        let salt = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "net-org-leader-3n-{tag}-{}-{salt:x}",
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

async fn node_with(incarnation: Option<Incarnation>) -> Arc<MeshNode> {
    let mut cfg = base_config();
    if let Some(incarnation) = incarnation {
        cfg = cfg.with_sensing_incarnation(incarnation);
    }
    Arc::new(
        MeshNode::new(EntityKeypair::generate(), cfg)
            .await
            .expect("MeshNode::new"),
    )
}

/// Adopt and install `org()` membership authority for `node`.
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

/// Seal a REAL owner-scoped announcement of CAP from `provider` to `to`'s own
/// owner audience and ingest it through the ordinary verified path — the
/// function the scoped-announcement dispatch arm calls.
fn announce_owner_capability(to: &Arc<MeshNode>, provider: &Arc<MeshNode>) {
    let authority = to
        .node_authority()
        .expect("the receiver must be organization-authoritative");
    let cert = OrgMembershipCert::try_issue(&org(), provider.entity_id().clone(), 1, 3600)
        .expect("provider membership cert");
    let descriptor = CapabilitySet::new().add_tag(CAP).to_bytes_compact();
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

/// The provider-free ORG interest: any authorized member providing CAP.
fn org_spec() -> InterestSpec {
    InterestSpec {
        capability_id: CapabilityId::new(CAP),
        constraints: CanonicalConstraints::from_entries([("model", "llama")]).unwrap(),
        work_latency: WorkLatencyEnvelope::start_within(Duration::from_secs(2)),
        providers: ProviderSelector::AnyAuthorized,
        result_mode: ResultMode::Any,
        disclosure_class: DisclosureClass::Owner,
        audience: canonical_org_sensing_commitment(&org().org_id()),
    }
}

/// The topology: A — R — P (A and P share no session; every frame between them
/// rides R), every node an `org()` member, P announcing CAP to R's owner
/// audience, R holding the leader role. `consumer_sees_provider` additionally
/// announces P to A's OWN owner audience (the D-LS6 precondition).
struct Mesh {
    a: Arc<MeshNode>,
    r: Arc<MeshNode>,
    p: Arc<MeshNode>,
    _dirs: Vec<ScratchDir>,
}

impl Mesh {
    async fn build(tag: &str, consumer_sees_provider: bool) -> Self {
        let a = node_with(None).await;
        let r = node_with(None).await;
        let p = node_with(Some(Incarnation::new(1))).await;
        let dirs = vec![
            adopt_and_install(&a, &format!("{tag}-a")),
            adopt_and_install(&r, &format!("{tag}-r")),
            adopt_and_install(&p, &format!("{tag}-p")),
        ];
        p.register_readiness_evaluator(CapabilityId::new(CAP), Arc::new(AlwaysReady))
            .expect("evaluator registers");
        connect_pair(&a, &r).await;
        connect_pair(&r, &p).await;
        for node in [&a, &r, &p] {
            node.start();
        }
        for node in [&a, &r, &p] {
            node.announce_capabilities(CapabilitySet::new())
                .await
                .expect("announce");
        }
        let (a_id, r_id, p_id) = (a.node_id(), r.node_id(), p.node_id());
        await_condition(Duration::from_secs(5), "entity pins established", || {
            r.peer_entity_id(a_id).is_some()
                && r.peer_entity_id(p_id).is_some()
                && p.peer_entity_id(r_id).is_some()
                && a.peer_entity_id(r_id).is_some()
        })
        .await;
        // The verifying-hop origin pin (the documented SI-3 seam bound): A
        // verifies P's signatures against P's own entity.
        a.test_pin_peer_entity(p_id, p.entity_id().clone());
        announce_owner_capability(&r, &p);
        if consumer_sees_provider {
            announce_owner_capability(&a, &p);
        }
        assert!(r.assume_sensing_leader(), "the leader role installs at R");
        Self {
            a,
            r,
            p,
            _dirs: dirs,
        }
    }

    /// A's soft-state refresh loop: the provider-free ORG registration to R.
    fn refresh(&self) -> tokio::task::JoinHandle<()> {
        let (a, r_id, spec) = (self.a.clone(), self.r.node_id(), org_spec());
        tokio::spawn(async move {
            loop {
                a.register_org_capability_interest(&spec, r_id, D, TTL)
                    .expect("the org registration is authored");
                tokio::time::sleep(REFRESH).await;
            }
        })
    }

    /// Waits until R resolved P from its own private discovery, holds the Leader
    /// row, and P streams for the branch.
    async fn await_leader_branch(&self) -> ProviderInterestKey {
        let (r, p) = (&self.r, &self.p);
        let interest = org_spec().key();
        let branch = ProviderInterestKey::new(interest.clone(), p.node_id());
        await_condition(Duration::from_secs(10), "the org leader coalesces", || {
            r.sensing_leader_interest_count() == Some(1)
        })
        .await;
        assert_eq!(
            r.sensing_leader_branches(&interest),
            Some(vec![p.node_id()]),
            "R resolved P from its OWN owner-private discovery"
        );
        await_condition(Duration::from_secs(10), "the Leader row at R", || {
            r.sensing_downstreams(&branch) == vec![DownstreamId::Leader]
        })
        .await;
        // P admitted R's re-authored org frame: its row is R's, under R's own
        // membership (P's gate enforces sender == cert.member).
        await_condition(Duration::from_secs(10), "P holds R's org row", || {
            p.sensing_downstreams(&branch) == vec![DownstreamId::Peer(r.node_id())]
        })
        .await;
        await_condition(Duration::from_secs(10), "one stream at P", || {
            p.sensing_live_streams() == 1
        })
        .await;
        branch
    }

    async fn teardown(self, refresher: tokio::task::JoinHandle<()>) {
        refresher.abort();
        for node in [&self.a, &self.r, &self.p] {
            let _ = node.shutdown().await;
        }
    }
}

/// The LS-6 transport proof: provider-free org proofs fan back to the consumer
/// through the org leader, over real transport, with no protocol-invalid input
/// and no scope refusal at any hop.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn provider_free_org_proofs_fan_back_through_the_org_leader() {
    let mesh = Mesh::build("proof", true).await;
    let refresher = mesh.refresh();
    let branch = mesh.await_leader_branch().await;
    let (a, r, p) = (&mesh.a, &mesh.r, &mesh.p);

    await_condition(
        Duration::from_secs(10),
        "A receives P's proof via R",
        || {
            a.sensing_latest_attestation(&branch).is_some_and(|proof| {
                proof.origin == p.node_id() && proof.status == AttestedStatus::Ready
            })
        },
    )
    .await;
    await_condition(Duration::from_secs(5), "A projects Ready", || {
        a.sensing_projected(&branch) == ProjectedReadiness::Ready
    })
    .await;

    for node in [a, r, p] {
        let counters = node.sensing_counters();
        assert_eq!(SensingCounters::get(&counters.protocol_invalid), 0);
        assert_eq!(SensingCounters::get(&counters.scope_refusals), 0);
    }
    let metrics = r.org_leader_metrics();
    assert!(
        metrics.candidate_projection_build_intake_total >= 1,
        "R projected its owner-private candidates at intake"
    );
    assert_eq!(metrics.intake_preflight_refused_total, 0);
    mesh.teardown(refresher).await;
}

/// D-LS6, over real transport: the leader still senses P and its proofs still
/// reach A, but A — which cannot itself see P as a current owner-private
/// member — refuses every one of them and never projects.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_consumer_that_cannot_see_the_provider_refuses_its_proofs() {
    let mesh = Mesh::build("blind", false).await;
    let refresher = mesh.refresh();
    let branch = mesh.await_leader_branch().await;
    let a = &mesh.a;
    await_condition(Duration::from_secs(10), "A refuses P's proofs", || {
        SensingCounters::get(&a.sensing_counters().scope_refusals) >= 1
    })
    .await;
    assert!(
        a.sensing_latest_attestation(&branch).is_none(),
        "no refused proof is stored"
    );
    assert_ne!(a.sensing_projected(&branch), ProjectedReadiness::Ready);
    mesh.teardown(refresher).await;
}
