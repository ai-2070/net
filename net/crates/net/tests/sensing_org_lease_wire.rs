//! PRODUCTION-COUPLED transport witness for the local-origin organization
//! exact-provider lease egress.
//!
//! Every other witness for this slice is in-crate: they prove the planner emits
//! the org variant, that the emitted frame passes the intake gate, and that the
//! local table row lands under the organization-derived root. All of them would
//! stay green if the production `spawn_sensing_frame_send` call were deleted.
//!
//! This one cannot. Node A drives the PUBLIC lease API; the bytes cross real
//! loopback UDP; node B admits them through its ORDINARY dispatch/intake path
//! (`handle_sensing_interest_frame` -> the org authority gate ->
//! `apply_provider_registration`); and the assertions read B's own interest
//! table. There is no helper round trip and no hand-built frame anywhere in this
//! file.
//!
//! It discriminates against all four failure modes the review named:
//!
//! * **send deleted** — B never gains a row, and the poll times out.
//! * **legacy bytes emitted** — B is organization-authoritative, so a LEGACY
//!   `ProviderRegistration` carrying the organization audience is exactly the C1
//!   authority-laundering case: B refuses it, counts `protocol_invalid`, and
//!   installs nothing. The row assertion fails AND the counter assertion fails.
//! * **intake bypassed** — the row is read from B, which only the real intake
//!   path can write.
//! * **wrong root** — the row's `owner_root` is asserted to be the canonical
//!   organization commitment, not A's legacy entity root.
//!
//! Topology is two nodes, one organization, no chaos injection, single send per
//! transition with a soft-state refresh loop for UDP best-effort.
//!
//! Run: `cargo test --features net --test sensing_org_lease_wire`

#![cfg(feature = "net")]

mod common;
use common::*;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::behavior::org::{OrgKeypair, OrgMembershipCert};
use net::adapter::net::behavior::org_authority::NodeAuthority;
use net::adapter::net::behavior::sensing::{
    canonical_org_sensing_commitment, AudienceScopeCommitment, CanonicalConstraints, CapabilityId,
    DisclosureClass, DownstreamId, InterestSpec, ProviderInterestKey, ProviderSelector, ResultMode,
    SensingCounters, WorkLatencyEnvelope,
};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};
use net::adapter::Adapter;

/// Requested sample interval D.
const D: Duration = Duration::from_millis(200);
/// A stricter cadence, to drive a second (re-authoring) transition.
const STRICT: Duration = Duration::from_millis(50);
/// Comfortably past the upstream damper min-gap (100 ms).
const PAST_MIN_GAP: Duration = Duration::from_millis(180);
const POLL: Duration = Duration::from_secs(5);

/// The one shared organization. Both nodes are members, and it defines the
/// canonical sensing audience commitment the rows are keyed under.
fn org() -> OrgKeypair {
    OrgKeypair::from_bytes([0x42u8; 32])
}

/// A scratch authority directory, deliberately NOT removed on drop.
///
/// `OrgRevocationStore` keys its process-global core registry by the revocation
/// `.lock` sidecar's `(device, inode)`. Freeing that inode while this test's
/// core is still registered lets the next store in this binary alias it and
/// inherit this test's floors, poison bit and generation. The name carries a
/// per-process salt as well as the pid, because pids are recycled and nothing
/// here is ever deleted.
struct ScratchDir(PathBuf);

impl ScratchDir {
    fn new(tag: &str) -> Self {
        let salt = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "net-org-lease-wire-{tag}-{}-{salt:x}",
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
        .with_sensing_coalescing(true);
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: CHAOS_BUFFER_SIZE,
        recv_buffer_size: CHAOS_BUFFER_SIZE,
    };
    cfg
}

/// Real ceremony: issue this node a membership cert under `org()`, adopt an
/// authority into a fresh directory, and install it as the production object —
/// so the node can both vouch for its own re-authoring (A) and VERIFY inbound
/// organization registrations (B).
fn adopt_and_install(node: &Arc<MeshNode>, tag: &str) -> ScratchDir {
    let dir = ScratchDir::new(tag);
    let cert = OrgMembershipCert::try_issue(&org(), node.entity_id().clone(), 1, 3600)
        .expect("issue cert");
    let authority = NodeAuthority::adopt(&dir.0, cert, node.entity_id(), 0, None).expect("adopt");
    node.install_node_authority(Arc::new(authority))
        .expect("install authority");
    dir
}

async fn bring_up(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    connect_pair(a, b).await;
    a.start();
    b.start();
    a.announce_capabilities(CapabilitySet::new())
        .await
        .expect("A announce");
    b.announce_capabilities(CapabilitySet::new())
        .await
        .expect("B announce");
    let a_id = a.node_id();
    let b_id = b.node_id();
    await_condition(Duration::from_secs(5), "entity pins established", || {
        a.peer_entity_id(b_id).is_some() && b.peer_entity_id(a_id).is_some()
    })
    .await;
}

/// An exact-provider interest whose audience is the canonical ORGANIZATION
/// commitment — the audience the local egress derives, and the only one B's gate
/// admits.
fn org_spec(target: u64) -> InterestSpec {
    InterestSpec {
        capability_id: CapabilityId::new("gpu.infer"),
        constraints: CanonicalConstraints::from_entries([("model", "llama-70b")]).unwrap(),
        work_latency: WorkLatencyEnvelope::start_within(Duration::from_secs(2)),
        providers: ProviderSelector::Node(target),
        result_mode: ResultMode::Any,
        disclosure_class: DisclosureClass::Owner,
        audience: canonical_org_sensing_commitment(&org().org_id()),
    }
}

/// B's row for A's interest, if the real intake path installed one.
fn peer_row(
    b: &Arc<MeshNode>,
    key: &ProviderInterestKey,
    a_id: u64,
) -> Option<(Duration, AudienceScopeCommitment)> {
    b.sensing_downstream_entry(key, DownstreamId::Peer(a_id))
        .map(|row| (row.requested_sample_interval, row.owner_root))
}

/// The production lease path emits an authenticated `OrgProviderRegistration`
/// that reaches the peer through its ordinary intake and installs under the
/// organization-derived root.
#[tokio::test]
async fn an_org_lease_reaches_the_provider_through_real_intake() {
    let a = Arc::new(
        MeshNode::new(EntityKeypair::generate(), base_config())
            .await
            .expect("MeshNode::new A"),
    );
    let b = Arc::new(
        MeshNode::new(EntityKeypair::generate(), base_config())
            .await
            .expect("MeshNode::new B"),
    );
    // BOTH nodes are members of the same organization: A to author, B to verify.
    let _a_dir = adopt_and_install(&a, "a");
    let _b_dir = adopt_and_install(&b, "b");
    bring_up(&a, &b).await;

    let a_id = a.node_id();
    let b_id = b.node_id();
    let spec = org_spec(b_id);
    let key = ProviderInterestKey::new(spec.key(), b_id);
    let commitment = canonical_org_sensing_commitment(&org().org_id());

    let protocol_invalid_before = SensingCounters::get(&b.sensing_counters().protocol_invalid);

    // Soft-state re-drive: the lease has no ttl/2 refresh owner in this slice,
    // and UDP is best-effort, so re-acquire an extra holder periodically to
    // re-emit. Each acquisition is idempotent at the provider.
    let refresher = {
        let a = a.clone();
        let spec = spec.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(PAST_MIN_GAP).await;
                if let Ok(t) = a.acquire_sensing_interest_lease(&spec, b_id, D) {
                    a.release_sensing_interest_lease(t);
                }
            }
        })
    };

    // THE PRODUCTION CALL. Nothing else in this test builds or sends a frame.
    let loose = a
        .acquire_sensing_interest_lease(&spec, b_id, D)
        .expect("the own-org exact-provider lease must acquire");

    assert!(
        poll_until(POLL, || peer_row(&b, &key, a_id).is_some()).await,
        "the provider never installed a row for the organization lease — either the \
         production send was dropped, or B refused the bytes (a legacy frame carrying \
         the org audience is refused as authority laundering)"
    );
    let (interval, root) = peer_row(&b, &key, a_id).expect("row present");
    assert_eq!(
        root, commitment,
        "the provider-side row must be registered under the canonical ORGANIZATION \
         commitment — a legacy frame would have carried A's entity root instead"
    );
    assert_eq!(interval, D, "and at the cadence the lease requested");
    assert_eq!(
        SensingCounters::get(&b.sensing_counters().protocol_invalid),
        protocol_invalid_before,
        "the frame passed selector/target and audience intake UNCHANGED — no \
         protocol-invalid refusal on the provider"
    );

    // A stricter holder re-authors a fresh organization frame at the tighter
    // cadence, and that also has to cross the wire.
    tokio::time::sleep(PAST_MIN_GAP).await;
    let strict = a
        .acquire_sensing_interest_lease(&spec, b_id, STRICT)
        .expect("a stricter holder re-authors");
    assert!(
        poll_until(POLL, || peer_row(&b, &key, a_id).map(|(d, _)| d)
            == Some(STRICT))
        .await,
        "the provider never tightened to {STRICT:?} — the re-authored organization \
         frame did not arrive"
    );
    assert_eq!(
        peer_row(&b, &key, a_id).expect("row present").1,
        commitment,
        "the tightened row is STILL under the organization-derived root"
    );
    assert_eq!(
        SensingCounters::get(&b.sensing_counters().protocol_invalid),
        protocol_invalid_before,
        "and the re-authored frame was likewise admitted without refusal"
    );

    refresher.abort();
    a.release_sensing_interest_lease(strict);
    a.release_sensing_interest_lease(loose);
    a.shutdown().await.expect("shutdown A");
    b.shutdown().await.expect("shutdown B");
}
