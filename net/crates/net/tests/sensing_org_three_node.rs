//! OLB org-auth Piece 5 — three-node organization sensing re-authoring over
//! REAL transport (consumer A → relay B → provider C).
//!
//! The exact-provider go-live's load-bearing transport claim: a relay B that
//! receives an authenticated `OrgProviderRegistration` re-authors a FRESH
//! `OrgProviderRegistration` upstream under B's OWN live membership certificate
//! — never the downstream consumer's certificate, and never a legacy downgrade.
//!
//! There is no mock socket or byte capture: the witness lands B's emitted frame
//! on a REAL provider node C and inspects C's sensing-table row. That row is
//! cryptographically dispositive — C's own organization-authority gate
//! (`verify_org_sensing_registration`) enforces `sender_entity == cert.member`,
//! so a `Peer(B)` row carrying `owner_root == canonical_org_sensing_commitment`
//! can exist ONLY if B sent a fresh org frame vouched by B's own certificate:
//!
//!   * B forwards A's cert → C: SenderMemberMismatch → no row
//!   * B downgrades to a legacy frame → C: an entity/fleet root, never the org
//!     commitment (domain-separated) → assert fails
//!   * B's membership is unprovable → B emits nothing (no fallback) → no row
//!
//! The B-side row (`Peer(A)`, same org root) additionally shows A's leg was
//! admitted under A's cert and B's upstream leg is a distinct re-authoring, not
//! a passthrough.
//!
//! Run: `cargo test --features net --test sensing_org_three_node`

#![cfg(feature = "net")]

mod common;
use common::*;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::org::{OrgKeypair, OrgMembershipCert};
use net::adapter::net::behavior::org_authority::NodeAuthority;
use net::adapter::net::behavior::sensing::{
    canonical_org_sensing_commitment, encode_interest_frame, AudienceScopeCommitment,
    CanonicalConstraints, CapabilityId, DisclosureClass, DownstreamId, InterestSpec,
    ProviderInterestKey, ProviderSelector, ResultMode, SensingCounters, SensingInterestFrame,
    WorkLatencyEnvelope, SUBPROTOCOL_SENSING_INTEREST,
};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};

/// Provider soft-state lifetime — generous against CI hiccups (a refresh every
/// 200 ms gives ~7 attempts per window).
const TTL: Duration = Duration::from_millis(1500);
/// Requested sample interval D.
const D: Duration = Duration::from_millis(100);
/// Refresh cadence for the soft-state re-send loop.
const REFRESH: Duration = Duration::from_millis(200);

fn base_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, CHAOS_PSK)
        .with_heartbeat_interval(Duration::from_millis(100))
        .with_session_timeout(Duration::from_secs(10))
        .with_handshake(3, Duration::from_secs(2));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: CHAOS_BUFFER_SIZE,
        recv_buffer_size: CHAOS_BUFFER_SIZE,
    };
    cfg
}

/// The one shared organization: A/B/C are members of it, and it defines the
/// canonical sensing audience commitment every hop's row is keyed under.
fn org() -> OrgKeypair {
    OrgKeypair::from_bytes([0x42u8; 32])
}

/// An RAII scratch authority directory: created on construction, removed on
/// drop. The live `OrgRevocationStore` is backed by this dir, so the guard is
/// held for the whole test and cleaned up afterward (no PID-named residue).
struct ScratchDir(PathBuf);

impl ScratchDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("net-olb-piece5-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Self(dir)
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Adopt `node` into `org()` (real ceremony, tempdir authority) and install the
/// authority as the production object — so this node can VERIFY inbound org
/// registrations AND vouch for its own re-authoring. Returns the RAII directory
/// guard; the caller holds it for the test's lifetime.
async fn adopt_and_install(node: &Arc<MeshNode>, tag: &str) -> ScratchDir {
    let dir = ScratchDir::new(tag);
    let cert = OrgMembershipCert::try_issue(&org(), node.entity_id().clone(), 1, 3600)
        .expect("issue cert");
    let authority = NodeAuthority::adopt(&dir.0, cert, node.entity_id(), 0, None).expect("adopt");
    node.install_node_authority(Arc::new(authority))
        .expect("install authority");
    dir
}

/// A node-targeted org interest whose audience is the canonical org commitment
/// (C's gate refuses any other audience).
fn org_spec(target: u64, audience: AudienceScopeCommitment) -> InterestSpec {
    InterestSpec {
        capability_id: CapabilityId::new("gpu.infer"),
        constraints: CanonicalConstraints::from_entries([("model", "llama")]).unwrap(),
        work_latency: WorkLatencyEnvelope::start_within(Duration::from_secs(2)),
        providers: ProviderSelector::Node(target),
        result_mode: ResultMode::Any,
        disclosure_class: DisclosureClass::Owner,
        audience,
    }
}

/// Soft-state refresh loop: re-send the encoded frame every [`REFRESH`] until
/// aborted (UDP is best-effort; registration is idempotent).
fn spawn_refresher(
    node: Arc<MeshNode>,
    dest: SocketAddr,
    bytes: Vec<u8>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let _ = node
                .send_subprotocol(dest, SUBPROTOCOL_SENSING_INTEREST, &bytes)
                .await;
            tokio::time::sleep(REFRESH).await;
        }
    })
}

#[tokio::test]
async fn relay_reauthors_org_provider_under_its_own_membership() {
    let commitment = canonical_org_sensing_commitment(&org().org_id());

    // Three real nodes. B and C hold their OWN org membership (they verify and,
    // for B, re-author). A only needs a cert for its OWN entity to attach to the
    // frame it sends — it drives a raw subprotocol send, not an installed
    // authority.
    let a = Arc::new(
        MeshNode::new(
            EntityKeypair::generate(),
            base_config().with_sensing_coalescing(true),
        )
        .await
        .expect("MeshNode::new A"),
    );
    let b = Arc::new(
        MeshNode::new(
            EntityKeypair::generate(),
            base_config().with_sensing_coalescing(true),
        )
        .await
        .expect("MeshNode::new B"),
    );
    let c = Arc::new(
        MeshNode::new(
            EntityKeypair::generate(),
            base_config().with_sensing_coalescing(true),
        )
        .await
        .expect("MeshNode::new C"),
    );
    // Held for the whole test: the live revocation stores are backed by these
    // dirs, and dropping the guards at the end removes them.
    let _b_dir = adopt_and_install(&b, "relay").await;
    let _c_dir = adopt_and_install(&c, "provider").await;

    // Line links only: A—B and B—C. A never touches C.
    connect_pair(&a, &b).await;
    connect_pair(&b, &c).await;
    a.start();
    b.start();
    c.start();
    for node in [&a, &b, &c] {
        node.announce_capabilities(net::adapter::net::behavior::capability::CapabilitySet::new())
            .await
            .expect("announce");
    }
    let a_id = a.node_id();
    let b_id = b.node_id();
    let c_id = c.node_id();
    await_condition(Duration::from_secs(5), "entity pins established", || {
        b.peer_entity_id(a_id).is_some()
            && b.peer_entity_id(c_id).is_some()
            && c.peer_entity_id(b_id).is_some()
            && a.peer_entity_id(b_id).is_some()
    })
    .await;

    // A mints a cert for its OWN entity and sends an OrgProviderRegistration
    // naming C as the provider, addressed to B. B re-authors toward C.
    let a_cert = OrgMembershipCert::try_issue(&org(), a.entity_id().clone(), 1, 3600)
        .expect("A's own membership cert");
    let spec = org_spec(c_id, commitment);
    let key = ProviderInterestKey::new(spec.key(), c_id);
    let a_bytes = encode_interest_frame(&SensingInterestFrame::org_provider_registration(
        &spec, c_id, D, TTL, a_cert,
    ))
    .expect("A's org provider frame encodes");
    let refresh_a = spawn_refresher(a.clone(), b.local_addr(), a_bytes);

    // B admits A's leg under A's certificate: a row attributed to Peer(A),
    // proven under the canonical ORG commitment (never A's entity root).
    await_condition(
        Duration::from_secs(5),
        "B admits A's org provider leg",
        || b.sensing_downstreams(&key) == vec![DownstreamId::Peer(a_id)],
    )
    .await;
    let b_row = b
        .sensing_downstream_entry(&key, DownstreamId::Peer(a_id))
        .expect("B's downstream row for A is present");
    assert_eq!(
        b_row.owner_root, commitment,
        "B stores the canonical org commitment A's cert proved, not a legacy/entity root",
    );

    // THE load-bearing proof: B re-authored a FRESH OrgProviderRegistration to C
    // under B's OWN live membership. C's row is attributed to Peer(B) and carries
    // the org commitment — which C's gate admits only for a valid org frame
    // vouched by B's own certificate. A legacy downgrade or a forwarded A-cert
    // would land no such row.
    await_condition(
        Duration::from_secs(5),
        "C receives B's re-authored org frame",
        || c.sensing_downstreams(&key) == vec![DownstreamId::Peer(b_id)],
    )
    .await;
    let c_row = c
        .sensing_downstream_entry(&key, DownstreamId::Peer(b_id))
        .expect("C's downstream row for B is present");
    assert_eq!(
        c_row.owner_root, commitment,
        "C's row proves B re-authored under the ORG commitment (own cert), not a downgrade",
    );
    assert_eq!(
        c_row.requested_sample_interval, D,
        "the re-authored provider leg preserves the demand interval",
    );

    // No downgrade, no laundering: C admitted the org frame cleanly — no
    // protocol-invalid or scope refusals were counted on the provider hop.
    assert_eq!(
        SensingCounters::get(&c.sensing_counters().protocol_invalid),
        0,
        "C counts no protocol-invalid frames — B's re-authoring is well-formed org input",
    );
    assert_eq!(
        SensingCounters::get(&c.sensing_counters().scope_refusals),
        0,
        "C counts no scope refusals — the org frame never took the legacy scope path",
    );

    // Stop the refresh loop and await the cancellation so the task is fully
    // torn down before the nodes (and the RAII authority dirs) drop.
    refresh_a.abort();
    let _ = refresh_a.await;
}
