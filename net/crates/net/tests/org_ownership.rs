//! OA-1 §1.6 exit-gate integration witnesses —
//! `docs/plans/ORG_CAPABILITY_AUTH_PLAN.md` (scaffolded
//! ownership) against real `MeshNode` instances:
//!
//! 1. Wire witness — an owner cert attached under the emission
//!    switch rides the real broadcast and projects `owner_org` at
//!    the RECEIVER's ingest; with emission off, announcement bytes
//!    stay pre-OA-1 and nothing projects.
//! 2. Ingest drops bad certs, not announcements (node level).
//! 3. Floor witness + the restart chain: floors raised through
//!    the persisted store drop below-floor certs at ingest;
//!    replacing the operator bundle with a VALID lower one and
//!    "restarting" (reopening the store from disk) never rolls
//!    the floor back.
//! 4. Authority-dark pin — `may_execute` verdicts are identical
//!    with and without a verified owner cert.

#![cfg(feature = "net")]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::capability::{CapabilityAnnouncement, CapabilitySet};
use net::adapter::net::behavior::fold::capability_bridge::{may_execute, owner_org_for};
use net::adapter::net::behavior::org::{OrgKeypair, OrgMembershipCert, OrgRevocationBundle};
use net::adapter::net::behavior::org_authority::NodeAuthority;
use net::adapter::net::behavior::org_revocation::OrgRevocationStore;
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};

const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x42u8; 32];

fn test_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(3, Duration::from_secs(2))
        .with_capability_gc_interval(Duration::from_millis(250))
        .with_min_announce_interval(Duration::from_millis(10));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: TEST_BUFFER_SIZE,
        recv_buffer_size: TEST_BUFFER_SIZE,
    };
    cfg
}

async fn build_node() -> Arc<MeshNode> {
    let cfg = test_config();
    let keypair = EntityKeypair::generate();
    Arc::new(MeshNode::new(keypair, cfg).await.expect("MeshNode::new"))
}

async fn handshake(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    let a_id = a.node_id();
    let b_id = b.node_id();
    let b_pub = *b.public_key();
    let b_addr = b.local_addr();
    let b_clone = b.clone();
    let accept = tokio::spawn(async move { b_clone.accept(a_id).await });
    a.connect(b_addr, &b_pub, b_id)
        .await
        .expect("connect failed");
    accept
        .await
        .expect("accept task panicked")
        .expect("accept failed");
    a.start();
    b.start();
}

/// Poll `cond` every 25ms for up to 2s (per capability_broadcast.rs
/// — no fixed sleeps on slow CI boxes).
async fn wait_until<F>(node: &Arc<MeshNode>, mut cond: F) -> bool
where
    F: FnMut(&MeshNode) -> bool,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        if cond(node) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    cond(node)
}

fn org() -> OrgKeypair {
    OrgKeypair::from_bytes([0x42u8; 32])
}

fn scratch_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("net-org-ownership-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

// ---------------------------------------------------------------------------
// 1. Wire witness — emission on/off across a real broadcast.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn owner_cert_projects_across_the_wire_only_when_emitted() {
    let a = build_node().await;
    let b = build_node().await;
    handshake(&a, &b).await;

    // Adopt A into the org (real ceremony, tempdir authority).
    let dir = scratch_dir("wire");
    let cert =
        OrgMembershipCert::try_issue(&org(), a.entity_id().clone(), 1, 3600).expect("issue cert");
    let authority = NodeAuthority::adopt(&dir, cert, a.entity_id(), 0).expect("adopt");

    // Phase 1 — emission OFF (default): announce, verify B folds
    // the caps but projects NO ownership (pre-OA-1 byte shape).
    a.announce_capabilities(CapabilitySet::new().add_tag("nrpc:oa1-echo"))
        .await
        .expect("announce");
    let a_id = a.node_id();
    assert!(
        wait_until(&b, |n| {
            may_execute(n.capability_fold(), a_id, "nrpc:oa1-echo", 0xDEAD)
        })
        .await,
        "B must fold A's announcement"
    );
    assert_eq!(
        owner_org_for(b.capability_fold(), a_id),
        None,
        "emission off ⇒ no ownership projected"
    );

    // Phase 2 — emission ON (Migration step 3 switch): the next
    // announcement carries the cert; B's real ingest verifies it
    // and projects owner_org.
    a.set_owner_cert_emission(Some(authority.config.owner_cert.clone()));
    a.announce_capabilities(CapabilitySet::new().add_tag("nrpc:oa1-echo"))
        .await
        .expect("announce with cert");
    assert!(
        wait_until(&b, |n| {
            owner_org_for(n.capability_fold(), a_id) == Some(org().org_id())
        })
        .await,
        "B must project A's verified owner org after emission is enabled"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// 2. Ingest drops bad certs, not announcements (node level).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn node_ingest_drops_bad_cert_but_keeps_announcement() {
    let node = build_node().await;
    let publisher = EntityKeypair::generate();
    let publisher_node_id = publisher.node_id();

    // A tampered cert on an otherwise-valid announcement.
    let mut cert = OrgMembershipCert::try_issue(&org(), publisher.entity_id().clone(), 1, 3600)
        .expect("issue");
    cert.signature[0] ^= 1;
    let mut ann = CapabilityAnnouncement::new(
        publisher_node_id,
        publisher.entity_id().clone(),
        100,
        CapabilitySet::new().add_tag("nrpc:oa1-echo"),
    )
    .with_owner_cert(Some(cert));
    ann.sign(&publisher);
    node.test_inject_capability_announcement(ann);

    // Announcement kept: the publisher is discoverable/capable.
    assert!(
        may_execute(
            node.capability_fold(),
            publisher_node_id,
            "nrpc:oa1-echo",
            0xDEAD
        ),
        "announcement must be kept when only the cert is bad"
    );
    // Cert dropped: authority-dark.
    assert_eq!(
        owner_org_for(node.capability_fold(), publisher_node_id),
        None
    );
}

// ---------------------------------------------------------------------------
// 3. Floor witness + restart chain at the node level.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn floors_gate_ingest_and_survive_restart_with_lower_valid_bundle() {
    let node = build_node().await;
    let publisher = EntityKeypair::generate();
    let publisher_node_id = publisher.node_id();
    let dir = scratch_dir("floors");
    let state_path = dir.join("revocation-state.json");

    // Install a persisted store with floor 5 for the publisher.
    let store = Arc::new(OrgRevocationStore::init(&state_path).expect("init"));
    let mut floors = std::collections::BTreeMap::new();
    floors.insert(publisher.entity_id().clone(), 5u32);
    let bundle5 = OrgRevocationBundle::try_issue(&org(), &floors).expect("issue");
    store.apply_bundle(&bundle5).expect("apply floor 5");
    node.install_org_revocation_store(store);

    let inject = |generation: u32, version: u64| {
        let cert =
            OrgMembershipCert::try_issue(&org(), publisher.entity_id().clone(), generation, 3600)
                .expect("issue");
        let mut ann = CapabilityAnnouncement::new(
            publisher_node_id,
            publisher.entity_id().clone(),
            version,
            CapabilitySet::new().add_tag("nrpc:oa1-echo"),
        )
        .with_owner_cert(Some(cert));
        ann.sign(&publisher);
        node.test_inject_capability_announcement(ann);
    };

    // Below the floor: cert dropped (announcement kept).
    inject(4, 100);
    assert_eq!(
        owner_org_for(node.capability_fold(), publisher_node_id),
        None
    );
    // At the floor: projects.
    inject(5, 200);
    assert_eq!(
        owner_org_for(node.capability_fold(), publisher_node_id),
        Some(org().org_id())
    );

    // RESTART: reopen the persisted maxima from disk and apply a
    // VALID lower bundle (generation 3) — the §1.6 witness. The
    // floor must remain 5 and a generation-4 cert must still be
    // dropped.
    let restarted = Arc::new(OrgRevocationStore::open_existing(&state_path).expect("reopen"));
    let mut lower = std::collections::BTreeMap::new();
    lower.insert(publisher.entity_id().clone(), 3u32);
    let bundle3 = OrgRevocationBundle::try_issue(&org(), &lower).expect("issue");
    let raised = restarted
        .apply_bundle(&bundle3)
        .expect("lower bundle is a no-op");
    assert_eq!(raised, 0, "lower bundle must not merge");
    assert_eq!(
        restarted.floor_for(&org().org_id(), publisher.entity_id()),
        5,
        "generation 5 remains authoritative across restart"
    );
    node.install_org_revocation_store(restarted);
    inject(4, 300);
    assert_eq!(
        owner_org_for(node.capability_fold(), publisher_node_id),
        None,
        "below-floor cert must stay dropped after the restart chain"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// 4. Authority-dark pin — may_execute ignores ownership.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn may_execute_is_identical_with_and_without_verified_cert() {
    let node = build_node().await;
    let publisher = EntityKeypair::generate();
    let publisher_node_id = publisher.node_id();
    let caller = 0xCA11;

    let build = |with_cert: bool, restrict: bool, version: u64| {
        let mut ann = CapabilityAnnouncement::new(
            publisher_node_id,
            publisher.entity_id().clone(),
            version,
            CapabilitySet::new().add_tag("nrpc:oa1-echo"),
        );
        if with_cert {
            let cert = OrgMembershipCert::try_issue(&org(), publisher.entity_id().clone(), 1, 3600)
                .expect("issue");
            ann = ann.with_owner_cert(Some(cert));
        }
        if restrict {
            ann.allowed_nodes = vec![0xFFFF];
        }
        ann.sign(&publisher);
        ann
    };

    // Permissive: admitted regardless of ownership; and the cert
    // really did project (so the pin isn't vacuous).
    node.test_inject_capability_announcement(build(true, false, 100));
    assert_eq!(
        owner_org_for(node.capability_fold(), publisher_node_id),
        Some(org().org_id())
    );
    let with_cert = may_execute(
        node.capability_fold(),
        publisher_node_id,
        "nrpc:oa1-echo",
        caller,
    );
    node.test_inject_capability_announcement(build(false, false, 200));
    let without_cert = may_execute(
        node.capability_fold(),
        publisher_node_id,
        "nrpc:oa1-echo",
        caller,
    );
    assert_eq!(with_cert, without_cert, "permissive verdict must match");
    assert!(with_cert, "permissive baseline admits");

    // Restricted to another node: denied regardless of ownership.
    node.test_inject_capability_announcement(build(true, true, 300));
    assert!(
        !may_execute(
            node.capability_fold(),
            publisher_node_id,
            "nrpc:oa1-echo",
            caller
        ),
        "restriction must deny with a verified cert present"
    );
}
