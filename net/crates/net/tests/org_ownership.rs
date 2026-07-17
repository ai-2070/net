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
    let authority = NodeAuthority::adopt(&dir, cert, a.entity_id(), 0, None).expect("adopt");

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

    // Phase 2 — install the loaded authority as THE production
    // authority object, then flip emission ON (Migration step 3
    // switch; review-8 §3 — the installed authority is the only
    // certificate source). The next announcement carries exactly
    // the installed cert; B's real ingest verifies + projects.
    a.install_node_authority(Arc::new(authority))
        .expect("install authority");
    a.set_owner_cert_emission(true).expect("enable emission");
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
// Review-8 §2/§3/§4 — production startup + scaffold-sourced emission.
// ---------------------------------------------------------------------------

/// Emission without an installed authority is a loud refusal — an
/// unadopted node cannot claim ownership at runtime.
#[tokio::test]
async fn emission_requires_installed_authority() {
    let node = build_node().await;
    assert!(node.node_authority().is_none());
    assert!(
        node.set_owner_cert_emission(true).is_err(),
        "unadopted node must not enable emission"
    );
    // Disabling is always fine (no-op).
    node.set_owner_cert_emission(false).expect("disable is ok");
}

/// One node, one owner at runtime too: an A-owned node refuses a
/// B-issued authority even when B's cert validly names this node.
#[tokio::test]
async fn install_refuses_foreign_authority() {
    let node = build_node().await;
    let dir_a = scratch_dir("install-a");
    let dir_b = scratch_dir("install-b");

    let cert_a =
        OrgMembershipCert::try_issue(&org(), node.entity_id().clone(), 1, 3600).expect("issue A");
    let authority_a =
        NodeAuthority::adopt(&dir_a, cert_a, node.entity_id(), 0, None).expect("adopt A");
    node.install_node_authority(Arc::new(authority_a))
        .expect("install A");

    let org_b = OrgKeypair::from_bytes([0x99u8; 32]);
    let cert_b =
        OrgMembershipCert::try_issue(&org_b, node.entity_id().clone(), 1, 3600).expect("issue B");
    let authority_b =
        NodeAuthority::adopt(&dir_b, cert_b, node.entity_id(), 0, None).expect("adopt B");
    assert!(
        node.install_node_authority(Arc::new(authority_b)).is_err(),
        "A-owned node must refuse B authority"
    );
    assert_eq!(
        node.node_authority().expect("still owned").owner_org(),
        org().org_id()
    );

    let _ = std::fs::remove_dir_all(&dir_a);
    let _ = std::fs::remove_dir_all(&dir_b);
}

/// Review-8 §4: an installed revocation store never lowers — a
/// stale independently opened store is refused; the same store is
/// idempotent.
#[tokio::test]
async fn store_replacement_never_lowers_the_live_view() {
    let node = build_node().await;
    let publisher = EntityKeypair::generate();
    let dir_hi = scratch_dir("store-hi");
    let dir_lo = scratch_dir("store-lo");

    let hi = Arc::new(OrgRevocationStore::init(dir_hi.join("revocation-state.json")).expect("hi"));
    let mut floors = std::collections::BTreeMap::new();
    floors.insert(publisher.entity_id().clone(), 5u32);
    hi.apply_bundle(&OrgRevocationBundle::try_issue(&org(), &floors).expect("issue"))
        .expect("floor 5");
    node.install_org_revocation_store(hi.clone())
        .expect("install hi");
    // Idempotent same-Arc re-install.
    node.install_org_revocation_store(hi)
        .expect("same store is idempotent");

    let mut low_floors = std::collections::BTreeMap::new();
    low_floors.insert(publisher.entity_id().clone(), 3u32);
    let lo = Arc::new(OrgRevocationStore::init(dir_lo.join("revocation-state.json")).expect("lo"));
    lo.apply_bundle(&OrgRevocationBundle::try_issue(&org(), &low_floors).expect("issue"))
        .expect("floor 3");
    assert!(
        node.install_org_revocation_store(lo).is_err(),
        "stale floor-3 store must not replace floor-5 store"
    );
    assert_eq!(
        node.org_revocation_store()
            .expect("still installed")
            .floor_for(&org().org_id(), publisher.entity_id()),
        5,
        "live floor must remain 5"
    );

    let _ = std::fs::remove_dir_all(&dir_hi);
    let _ = std::fs::remove_dir_all(&dir_lo);
}

/// Review-9 red 1: installing a store whose floors ALREADY rose
/// must reconcile existing projections — no floor-change event
/// fires after installation, so the install itself performs the
/// retraction sweep, and the fold change generation advances.
#[tokio::test]
async fn installing_pre_raised_store_reconciles_existing_projections() {
    let node = build_node().await;
    let dir = scratch_dir("pre-raised");

    // With NO store installed, a generation-4 projection lands.
    let publisher = EntityKeypair::generate();
    let publisher_node_id = publisher.node_id();
    let cert4 = OrgMembershipCert::try_issue(&org(), publisher.entity_id().clone(), 4, 3600)
        .expect("issue");
    let mut ann = CapabilityAnnouncement::new(
        publisher_node_id,
        publisher.entity_id().clone(),
        100,
        CapabilitySet::new().add_tag("nrpc:oa1-echo"),
    )
    .with_owner_cert(Some(cert4));
    ann.sign(&publisher);
    node.test_inject_capability_announcement(ann);
    assert_eq!(
        owner_org_for(node.capability_fold(), publisher_node_id),
        Some(org().org_id())
    );

    // An independent store already carries floor 5.
    let store =
        Arc::new(OrgRevocationStore::init(dir.join("revocation-state.json")).expect("init"));
    let mut floors = std::collections::BTreeMap::new();
    floors.insert(publisher.entity_id().clone(), 5u32);
    store
        .apply_bundle(&OrgRevocationBundle::try_issue(&org(), &floors).expect("issue"))
        .expect("floor 5");

    // Installing it retracts the stale projection immediately and
    // signals fold subscribers.
    let generation_before = node.capability_fold().change_generation();
    node.install_org_revocation_store(store).expect("install");
    assert_eq!(
        owner_org_for(node.capability_fold(), publisher_node_id),
        None,
        "pre-raised install must reconcile existing projections"
    );
    assert!(
        node.capability_fold().change_generation() > generation_before,
        "reconciliation must advance the fold change generation"
    );
    // Capability entry remains.
    assert!(may_execute(
        node.capability_fold(),
        publisher_node_id,
        "nrpc:oa1-echo",
        0xDEAD
    ));

    let _ = std::fs::remove_dir_all(&dir);
}

/// Review-9 red 2: a DETACHED (replaced) store's late raises must
/// not mutate the node — its callback is inert once it is no
/// longer the installed store.
#[tokio::test]
async fn detached_store_cannot_mutate_the_node() {
    let node = build_node().await;
    let dir_old = scratch_dir("detached-old");
    let dir_new = scratch_dir("detached-new");

    let old =
        Arc::new(OrgRevocationStore::init(dir_old.join("revocation-state.json")).expect("old"));
    node.install_org_revocation_store(old.clone())
        .expect("install old");
    // Replace with an (empty, trivially dominating) current store.
    let current =
        Arc::new(OrgRevocationStore::init(dir_new.join("revocation-state.json")).expect("new"));
    node.install_org_revocation_store(current.clone())
        .expect("replace with current");

    // A projection lands under the CURRENT store's floors.
    let publisher = EntityKeypair::generate();
    let publisher_node_id = publisher.node_id();
    let cert4 = OrgMembershipCert::try_issue(&org(), publisher.entity_id().clone(), 4, 3600)
        .expect("issue");
    let mut ann = CapabilityAnnouncement::new(
        publisher_node_id,
        publisher.entity_id().clone(),
        100,
        CapabilitySet::new().add_tag("nrpc:oa1-echo"),
    )
    .with_owner_cert(Some(cert4));
    ann.sign(&publisher);
    node.test_inject_capability_announcement(ann);
    assert_eq!(
        owner_org_for(node.capability_fold(), publisher_node_id),
        Some(org().org_id())
    );

    // Raising floors through the DETACHED store must not touch the
    // node's fold…
    let mut floors = std::collections::BTreeMap::new();
    floors.insert(publisher.entity_id().clone(), 5u32);
    let bundle = OrgRevocationBundle::try_issue(&org(), &floors).expect("issue");
    old.apply_bundle(&bundle)
        .expect("raise through detached store");
    assert_eq!(
        owner_org_for(node.capability_fold(), publisher_node_id),
        Some(org().org_id()),
        "a detached store must be inert"
    );

    // …while the same raise through the INSTALLED store retracts.
    current
        .apply_bundle(&bundle)
        .expect("raise through installed store");
    assert_eq!(
        owner_org_for(node.capability_fold(), publisher_node_id),
        None,
        "the installed store retracts"
    );

    let _ = std::fs::remove_dir_all(&dir_old);
    let _ = std::fs::remove_dir_all(&dir_new);
}

/// Review-9: concurrent dominating replacements are serialized —
/// the final installed floor never regresses (7 wins whether it
/// installs first, refusing 6, or second, dominating 6).
#[tokio::test]
async fn concurrent_replacements_never_lower_the_installed_floor() {
    let node = build_node().await;
    let publisher = EntityKeypair::generate();
    let dir6 = scratch_dir("repl-6");
    let dir7 = scratch_dir("repl-7");

    let make_store = |dir: &PathBuf, floor: u32| {
        let store =
            Arc::new(OrgRevocationStore::init(dir.join("revocation-state.json")).expect("init"));
        let mut floors = std::collections::BTreeMap::new();
        floors.insert(publisher.entity_id().clone(), floor);
        store
            .apply_bundle(&OrgRevocationBundle::try_issue(&org(), &floors).expect("issue"))
            .expect("raise");
        store
    };
    let store6 = make_store(&dir6, 6);
    let store7 = make_store(&dir7, 7);

    let n6 = node.clone();
    let n7 = node.clone();
    let t6 = tokio::task::spawn_blocking(move || n6.install_org_revocation_store(store6));
    let t7 = tokio::task::spawn_blocking(move || n7.install_org_revocation_store(store7));
    let r6 = t6.await.expect("t6");
    let r7 = t7.await.expect("t7");

    assert!(r7.is_ok(), "the floor-7 store always ends up installable");
    // Whether 6 installed first (then 7 replaced it) or was refused
    // after 7, the final floor is 7.
    let final_floor = node
        .org_revocation_store()
        .expect("installed")
        .floor_for(&org().org_id(), publisher.entity_id());
    assert_eq!(
        final_floor, 7,
        "final installed floor never drops (r6: {r6:?})"
    );

    let _ = std::fs::remove_dir_all(&dir6);
    let _ = std::fs::remove_dir_all(&dir7);
}

/// Review-9: `install_node_authority` re-verifies the authority
/// object in full — an authority whose OWN floor rose above its
/// certificate after adoption is refused, and enabled emission
/// goes dark rather than advertising a revoked certificate.
#[tokio::test]
async fn floored_authority_fails_installation_and_emission_goes_dark() {
    let node = build_node().await;
    let dir = scratch_dir("floored-auth");

    let cert1 =
        OrgMembershipCert::try_issue(&org(), node.entity_id().clone(), 1, 3600).expect("issue");
    let authority =
        Arc::new(NodeAuthority::adopt(&dir, cert1, node.entity_id(), 0, None).expect("adopt"));

    // Raise the authority's OWN member floor to 2 after adoption.
    let mut floors = std::collections::BTreeMap::new();
    floors.insert(node.entity_id().clone(), 2u32);
    let bundle = OrgRevocationBundle::try_issue(&org(), &floors).expect("issue");
    authority
        .revocation
        .apply_bundle(&bundle)
        .expect("raise to 2");

    // The reviewer red: installation previously succeeded.
    let err = node
        .install_node_authority(authority.clone())
        .expect_err("floored authority must fail installation");
    assert!(format!("{err}").contains("floor"), "got: {err}");

    // Emission liveness: install a VALID authority, enable
    // emission, then raise the node's own floor — the next
    // announcement goes dark instead of advertising the revoked
    // cert.
    let dir2 = scratch_dir("floored-auth-2");
    let cert1 =
        OrgMembershipCert::try_issue(&org(), node.entity_id().clone(), 1, 3600).expect("issue");
    let authority2 =
        Arc::new(NodeAuthority::adopt(&dir2, cert1, node.entity_id(), 0, None).expect("adopt 2"));
    node.install_node_authority(authority2).expect("install");
    node.set_owner_cert_emission(true).expect("enable");
    node.announce_capabilities(CapabilitySet::new().add_tag("nrpc:oa1-echo"))
        .await
        .expect("announce");
    assert_eq!(
        owner_org_for(node.capability_fold(), node.node_id()),
        Some(org().org_id()),
        "emission live while the cert stands"
    );

    node.org_revocation_store()
        .expect("installed")
        .apply_bundle(&bundle)
        .expect("raise own floor");
    // The raise itself retracted the self-projection…
    assert_eq!(owner_org_for(node.capability_fold(), node.node_id()), None);
    // …and the next announcement carries no cert (emission dark).
    node.announce_capabilities(CapabilitySet::new().add_tag("nrpc:oa1-echo"))
        .await
        .expect("announce dark");
    assert_eq!(
        owner_org_for(node.capability_fold(), node.node_id()),
        None,
        "a self-floored node must stop emitting ownership"
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&dir2);
}

/// Review-8 §9 end-to-end: raising a floor through the INSTALLED
/// store retracts an existing ownership projection immediately —
/// no re-announcement — while the capability entry stays present
/// and `may_execute` is unchanged.
#[tokio::test]
async fn floor_raise_retracts_projection_without_reannouncement() {
    let node = build_node().await;
    let dir = scratch_dir("retract");
    let cert =
        OrgMembershipCert::try_issue(&org(), node.entity_id().clone(), 1, 3600).expect("issue");
    let authority = NodeAuthority::adopt(&dir, cert, node.entity_id(), 0, None).expect("adopt");
    node.install_node_authority(Arc::new(authority))
        .expect("install");

    // A publisher projects ownership from a generation-4 cert.
    let publisher = EntityKeypair::generate();
    let publisher_node_id = publisher.node_id();
    let cert4 = OrgMembershipCert::try_issue(&org(), publisher.entity_id().clone(), 4, 3600)
        .expect("issue");
    let mut ann = CapabilityAnnouncement::new(
        publisher_node_id,
        publisher.entity_id().clone(),
        100,
        CapabilitySet::new().add_tag("nrpc:oa1-echo"),
    )
    .with_owner_cert(Some(cert4));
    ann.sign(&publisher);
    node.test_inject_capability_announcement(ann);
    assert_eq!(
        owner_org_for(node.capability_fold(), publisher_node_id),
        Some(org().org_id())
    );

    // Raise the floor to 5 through the installed store: the stale
    // projection retracts with NO further announcement.
    let mut floors = std::collections::BTreeMap::new();
    floors.insert(publisher.entity_id().clone(), 5u32);
    let bundle = OrgRevocationBundle::try_issue(&org(), &floors).expect("issue");
    node.org_revocation_store()
        .expect("installed")
        .apply_bundle(&bundle)
        .expect("apply floor 5");
    assert_eq!(
        owner_org_for(node.capability_fold(), publisher_node_id),
        None,
        "revoked membership must stop projecting immediately"
    );
    // Capability entry present, verdicts unchanged.
    assert!(may_execute(
        node.capability_fold(),
        publisher_node_id,
        "nrpc:oa1-echo",
        0xDEAD
    ));

    let _ = std::fs::remove_dir_all(&dir);
}

/// Review-9 real-path witness: a certificate acceptable ONLY under
/// the ceremony's skew adopts AND starts — the accepted tolerance
/// is persisted in the membership config, so `MeshNode::new`
/// verifies with exactly what `net node adopt` accepted (no
/// zero-skew startup surprise).
#[tokio::test]
async fn ceremony_skew_carries_into_production_startup() {
    let dir = scratch_dir("skew-startup");
    let keypair = EntityKeypair::from_bytes([0x66u8; 32]);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    // Validly signed, expired 30 s ago: acceptable only with
    // skew ≥ 30.
    let expired = {
        // issue_at is crate-internal; build the same shape through
        // the public issue path is impossible for a past window, so
        // sign a short-lived cert and wait it out — 2 s TTL keeps
        // the test fast while exercising the real expiry.
        let cert = OrgMembershipCert::try_issue(&org(), keypair.entity_id().clone(), 1, 2)
            .expect("issue short-lived");
        let _ = now;
        cert
    };
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Zero-skew ceremony refuses the expired cert…
    assert!(
        NodeAuthority::adopt(&dir, expired.clone(), keypair.entity_id(), 0, None).is_err(),
        "expired cert must refuse a strict ceremony"
    );
    // …a 120 s tolerance accepts it, persisting the tolerance.
    NodeAuthority::adopt(&dir, expired, keypair.entity_id(), 120, None)
        .expect("skewed ceremony accepts");

    // Production startup succeeds with the SAME persisted skew.
    let node = MeshNode::new(keypair.clone(), test_config().with_node_authority_dir(&dir))
        .await
        .expect("startup verifies under the persisted ceremony skew");
    assert!(node.node_authority().is_some());

    let _ = std::fs::remove_dir_all(&dir);
}

/// Review-8 §2 production-startup witnesses: the four configured
/// cases through the real `MeshNode::new`.
#[tokio::test]
async fn production_startup_honors_configured_authority() {
    // (a) No authority configured: legacy startup, no owner
    // projection, no store.
    let legacy = build_node().await;
    assert!(legacy.node_authority().is_none());
    assert!(legacy.org_revocation_store().is_none());

    // (b) Configured and valid: startup succeeds, store installed,
    // emission default OFF (self-announce projects no ownership).
    let dir = scratch_dir("startup");
    let keypair = EntityKeypair::from_bytes([0x77u8; 32]);
    let cert =
        OrgMembershipCert::try_issue(&org(), keypair.entity_id().clone(), 1, 3600).expect("issue");
    NodeAuthority::adopt(&dir, cert, keypair.entity_id(), 0, None).expect("adopt");
    let cfg = test_config().with_node_authority_dir(&dir);
    let node = Arc::new(
        MeshNode::new(keypair.clone(), cfg)
            .await
            .expect("adopted startup succeeds"),
    );
    assert_eq!(
        node.node_authority()
            .expect("authority installed")
            .owner_org(),
        org().org_id()
    );
    assert!(node.org_revocation_store().is_some());
    node.announce_capabilities(CapabilitySet::new().add_tag("nrpc:oa1-echo"))
        .await
        .expect("announce");
    assert_eq!(
        owner_org_for(node.capability_fold(), node.node_id()),
        None,
        "emission defaults OFF"
    );

    // (c) Explicit emission flag: the self-index projects exactly
    // the loaded certificate's org.
    let node2 = Arc::new(
        MeshNode::new(
            keypair.clone(),
            test_config()
                .with_node_authority_dir(&dir)
                .with_owner_cert_emission(true),
        )
        .await
        .expect("emitting startup succeeds"),
    );
    node2
        .announce_capabilities(CapabilitySet::new().add_tag("nrpc:oa1-echo"))
        .await
        .expect("announce");
    assert_eq!(
        owner_org_for(node2.capability_fold(), node2.node_id()),
        Some(org().org_id()),
        "explicit emission emits the loaded authority's cert"
    );

    // (d) Configured but missing/corrupt/floored: startup refuses.
    let missing = scratch_dir("startup-missing");
    assert!(
        MeshNode::new(
            keypair.clone(),
            test_config().with_node_authority_dir(&missing)
        )
        .await
        .is_err(),
        "configured-but-missing authority must refuse startup"
    );
    std::fs::write(dir.join("owner-membership.json"), b"{ nope").expect("corrupt");
    assert!(
        MeshNode::new(keypair.clone(), test_config().with_node_authority_dir(&dir))
            .await
            .is_err(),
        "corrupt authority must refuse startup"
    );

    // (e) Emission flag without a configured authority: refused.
    assert!(
        MeshNode::new(
            EntityKeypair::generate(),
            test_config().with_owner_cert_emission(true)
        )
        .await
        .is_err(),
        "emit_owner_cert without node_authority_dir must refuse"
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&missing);
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
    node.install_org_revocation_store(store).expect("install");

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
    assert!(raised.is_empty(), "lower bundle must not merge");
    assert_eq!(
        restarted.floor_for(&org().org_id(), publisher.entity_id()),
        5,
        "generation 5 remains authoritative across restart"
    );
    node.install_org_revocation_store(restarted)
        .expect("install restarted");
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

// ---------------------------------------------------------------------------
// Review-9 addendum — replacement/installation vs. active raises,
// shared same-path views, and cached-announcement re-validation.
// ---------------------------------------------------------------------------

/// Review-9 addendum P1: store replacement pins the CURRENT
/// store's publish transaction across the dominance comparison and
/// the swap. An active raise on the installed store cannot publish
/// mid-replacement: it lands strictly after the swap, where it is
/// a raise through a DETACHED store — inert for this node.
#[tokio::test]
async fn active_raise_and_replacement_serialize_on_the_publish_guard() {
    let node = build_node().await;
    let publisher = EntityKeypair::generate();
    let publisher_node_id = publisher.node_id();
    let dir_a = scratch_dir("guard-a");
    let dir_b = scratch_dir("guard-b");

    let make_store = |dir: &PathBuf, floor: u32| {
        let store =
            Arc::new(OrgRevocationStore::init(dir.join("revocation-state.json")).expect("init"));
        let mut floors = std::collections::BTreeMap::new();
        floors.insert(publisher.entity_id().clone(), floor);
        store
            .apply_bundle(&OrgRevocationBundle::try_issue(&org(), &floors).expect("issue"))
            .expect("raise");
        store
    };
    let store_a = make_store(&dir_a, 5);
    let store_b = make_store(&dir_b, 5);
    node.install_org_revocation_store(store_a.clone())
        .expect("install A");

    // A generation-7 projection stands under floor 5.
    let cert7 = OrgMembershipCert::try_issue(&org(), publisher.entity_id().clone(), 7, 3600)
        .expect("issue");
    let mut ann = CapabilityAnnouncement::new(
        publisher_node_id,
        publisher.entity_id().clone(),
        100,
        CapabilitySet::new().add_tag("nrpc:oa1-echo"),
    )
    .with_owner_cert(Some(cert7));
    ann.sign(&publisher);
    node.test_inject_capability_announcement(ann);
    assert_eq!(
        owner_org_for(node.capability_fold(), publisher_node_id),
        Some(org().org_id())
    );

    // Replacement pauses under the CURRENT store's publish guard;
    // an active floor-10 raise on A tries to interleave.
    let mut floors10 = std::collections::BTreeMap::new();
    floors10.insert(publisher.entity_id().clone(), 10u32);
    let bundle10 = OrgRevocationBundle::try_issue(&org(), &floors10).expect("issue 10");

    let raise_done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let raiser = {
        let store_a = store_a.clone();
        let raise_done = raise_done.clone();
        let node_for_pause = node.clone();
        let publisher_entity = publisher.entity_id().clone();
        let n = node.clone();
        tokio::task::spawn_blocking(move || {
            n.install_org_revocation_store_paused_for_test(store_b, &move || {
                // Under the guard: start the raise, give it real
                // time, and observe that it CANNOT publish — the
                // enforced view is still 5 and the raise thread is
                // still parked on the publish transaction.
                let store_for_raise = store_a.clone();
                let bundle_for_raise = bundle10.clone();
                let done = raise_done.clone();
                let raise = std::thread::spawn(move || {
                    store_for_raise
                        .apply_bundle(&bundle_for_raise)
                        .expect("raise eventually succeeds");
                    done.store(true, std::sync::atomic::Ordering::Release);
                });
                std::thread::sleep(Duration::from_millis(300));
                assert!(
                    !raise_done.load(std::sync::atomic::Ordering::Acquire),
                    "an active raise must not publish mid-replacement"
                );
                assert_eq!(
                    store_a.floor_for(&org().org_id(), &publisher_entity),
                    5,
                    "the guarded store's live view must not move under the comparison"
                );
                assert_eq!(
                    owner_org_for(node_for_pause.capability_fold(), publisher_node_id),
                    Some(org().org_id()),
                    "no retraction can land mid-replacement"
                );
                // Leak the join handle into the closure's scope; the
                // test joins via the `raise_done` flag below.
                drop(raise);
            })
        })
    };
    raiser
        .await
        .expect("install task")
        .expect("replacement succeeds");

    // The raise unblocks only after the swap: it lands on the now
    // DETACHED store A, whose subscription was removed — the fold
    // projection under the NEW regime (floor 5) survives.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while !raise_done.load(std::sync::atomic::Ordering::Acquire)
        && tokio::time::Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        raise_done.load(std::sync::atomic::Ordering::Acquire),
        "the raise completes once the guard releases"
    );
    assert_eq!(
        store_a.floor_for(&org().org_id(), publisher.entity_id()),
        10,
        "the detached store carries its raise"
    );
    assert_eq!(
        owner_org_for(node.capability_fold(), publisher_node_id),
        Some(org().org_id()),
        "a post-swap raise through the detached store is inert for this node"
    );

    // The complementary ordering: once the installed view HAS
    // risen, a weaker candidate is refused outright (the guarded
    // comparison sees the raise).
    node.org_revocation_store()
        .expect("installed")
        .apply_bundle(&OrgRevocationBundle::try_issue(&org(), &floors10).expect("issue 10 again"))
        .expect("raise installed store to 10");
    let weaker = make_store(&scratch_dir("guard-weak"), 5);
    assert!(
        node.install_org_revocation_store(weaker).is_err(),
        "a raise that publishes first must refuse the weaker candidate"
    );

    let _ = std::fs::remove_dir_all(&dir_a);
    let _ = std::fs::remove_dir_all(&dir_b);
}

/// Review-9 addendum P1: `install_node_authority` pins the
/// CANDIDATE store's publish transaction across verification and
/// publication — a racing self-floor raise cannot land between
/// `self_verify` and the authority store, so the method never
/// returns `Ok` with an authority its own floors had already
/// revoked. The raise lands strictly after publication, where the
/// (already-subscribed) node retracts and emission goes dark.
#[tokio::test]
async fn authority_install_pins_candidate_floors_across_verification() {
    let node = build_node().await;
    let dir = scratch_dir("auth-guard");

    let cert1 =
        OrgMembershipCert::try_issue(&org(), node.entity_id().clone(), 1, 3600).expect("issue");
    let authority =
        Arc::new(NodeAuthority::adopt(&dir, cert1, node.entity_id(), 0, None).expect("adopt"));

    let mut floors = std::collections::BTreeMap::new();
    floors.insert(node.entity_id().clone(), 2u32);
    let bundle2 = OrgRevocationBundle::try_issue(&org(), &floors).expect("issue");

    let raise_done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let install = {
        let n = node.clone();
        let authority = authority.clone();
        let raise_done = raise_done.clone();
        tokio::task::spawn_blocking(move || {
            let auth_for_pause = authority.clone();
            n.install_node_authority_paused_for_test(authority.clone(), &move || {
                let store = auth_for_pause.revocation.clone();
                let bundle = bundle2.clone();
                let done = raise_done.clone();
                let raise = std::thread::spawn(move || {
                    store.apply_bundle(&bundle).expect("raise eventually lands");
                    done.store(true, std::sync::atomic::Ordering::Release);
                });
                std::thread::sleep(Duration::from_millis(300));
                assert!(
                    !raise_done.load(std::sync::atomic::Ordering::Acquire),
                    "a candidate self-floor raise must not publish between \
                     verification and authority publication"
                );
                assert_eq!(
                    auth_for_pause
                        .revocation
                        .floor_for(&org().org_id(), &auth_for_pause.config.owner_cert.member),
                    0,
                    "the verified snapshot is pinned under the guard"
                );
                drop(raise);
            })
        })
    };
    install
        .await
        .expect("install task")
        .expect("installation verified against the pinned snapshot succeeds");

    // The raise lands strictly after publication…
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while !raise_done.load(std::sync::atomic::Ordering::Acquire)
        && tokio::time::Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(raise_done.load(std::sync::atomic::Ordering::Acquire));

    // …where it is an ordinary post-install self-revocation: the
    // node's floor view shows it and emission is dark.
    assert_eq!(
        node.org_revocation_store()
            .expect("installed")
            .floor_for(&org().org_id(), node.entity_id()),
        2
    );
    node.set_owner_cert_emission(true).expect("enable emission");
    node.announce_capabilities(CapabilitySet::new().add_tag("nrpc:oa1-echo"))
        .await
        .expect("announce");
    assert_eq!(
        owner_org_for(node.capability_fold(), node.node_id()),
        None,
        "emission is dark for the post-publication floored certificate"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Review-9 addendum P1: one backing path is ONE security view. A
/// sibling handle's raise — even one that ends durability-uncertain
/// — advances the INSTALLED store's view immediately, retracts the
/// projection, darkens emission, and the recovered state is
/// observed by every handle before the poison clears.
#[cfg(unix)]
#[tokio::test]
async fn poisoned_sibling_write_gates_the_installed_view_until_recovery() {
    use std::os::unix::fs::PermissionsExt;
    let node = build_node().await;
    let dir = scratch_dir("poison-sibling");

    let cert1 =
        OrgMembershipCert::try_issue(&org(), node.entity_id().clone(), 1, 3600).expect("issue");
    let authority =
        Arc::new(NodeAuthority::adopt(&dir, cert1, node.entity_id(), 0, None).expect("adopt"));
    node.install_node_authority(authority).expect("install");
    node.set_owner_cert_emission(true).expect("enable");
    node.announce_capabilities(CapabilitySet::new().add_tag("nrpc:oa1-echo"))
        .await
        .expect("announce");
    assert_eq!(
        owner_org_for(node.capability_fold(), node.node_id()),
        Some(org().org_id())
    );

    // An independent same-path handle — the review-9 addendum's
    // "store A" — shares the installed store's core.
    let state_path = dir.join("revocation-state.json");
    let sibling = OrgRevocationStore::open_existing(&state_path).expect("sibling");
    let installed = node.org_revocation_store().expect("installed");
    assert!(sibling.shares_core_with(&installed));

    // The sibling lands floor 9 but cannot prove the directory
    // entry durable (0o300: no read on the parent, so the
    // post-rename dir fsync fails).
    let mut floors = std::collections::BTreeMap::new();
    floors.insert(node.entity_id().clone(), 9u32);
    let bundle9 = OrgRevocationBundle::try_issue(&org(), &floors).expect("issue");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o300)).expect("chmod");
    assert!(
        sibling.apply_bundle(&bundle9).is_err(),
        "the failed dir fsync must surface"
    );

    // The INSTALLED view advanced with the sibling's publish —
    // never a stale independent floor 1 — and the node reacted:
    // projection retracted, emission dark, poison visible.
    assert_eq!(
        installed.floor_for(&org().org_id(), node.entity_id()),
        9,
        "same-path handles share one live view"
    );
    assert_eq!(
        owner_org_for(node.capability_fold(), node.node_id()),
        None,
        "the sibling's raise retracts through the shared core"
    );
    assert!(installed.is_poisoned(), "poison is path-wide");
    node.announce_capabilities(CapabilitySet::new().add_tag("nrpc:oa1-echo"))
        .await
        .expect("announce while poisoned");
    assert!(
        node.local_announcement_for_test()
            .expect("announced")
            .owner_cert
            .is_none(),
        "emission is dark against the shared floor-9 view"
    );

    // Repair the environment: the stronger view is already what
    // every handle observes BEFORE the poison clears…
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("chmod back");
    assert!(installed.is_poisoned());
    assert_eq!(installed.floor_for(&org().org_id(), node.entity_id()), 9);
    // …and the next apply performs recovery (locked reread
    // republished + successful parent fsync) and clears it.
    sibling.apply_bundle(&bundle9).expect("recovery apply");
    assert!(!installed.is_poisoned(), "recovery clears the shared bit");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Review-9 addendum P1 (cached announcements, send-time seam):
/// the serialization boundary every reusing send path shares
/// re-validates the cached owner certificate — after a self-floor
/// raise the produced bytes carry NO certificate, are re-signed,
/// and supersede the certified version.
#[tokio::test]
async fn cached_announcement_revalidates_at_the_send_boundary() {
    let node = build_node().await;
    let dir = scratch_dir("cache-seam");

    let cert1 =
        OrgMembershipCert::try_issue(&org(), node.entity_id().clone(), 1, 3600).expect("issue");
    let authority =
        Arc::new(NodeAuthority::adopt(&dir, cert1, node.entity_id(), 0, None).expect("adopt"));
    node.install_node_authority(authority).expect("install");
    node.set_owner_cert_emission(true).expect("enable");
    node.announce_capabilities(CapabilitySet::new().add_tag("nrpc:oa1-echo"))
        .await
        .expect("announce");

    // Control: the cached bytes carry the certificate while it
    // stands.
    let bytes = node.announcement_bytes_for_send_for_test().expect("cached");
    let certified = CapabilityAnnouncement::from_bytes(&bytes).expect("decode");
    assert!(certified.owner_cert.is_some());
    assert!(certified.verify().is_ok());

    // Self-floor raise — the cached object is now a lie. The pause
    // between build and send is exactly this seam: every send path
    // (immediate broadcast, deferred flush, session-open push)
    // serializes through it.
    let mut floors = std::collections::BTreeMap::new();
    floors.insert(node.entity_id().clone(), 2u32);
    node.org_revocation_store()
        .expect("installed")
        .apply_bundle(&OrgRevocationBundle::try_issue(&org(), &floors).expect("issue"))
        .expect("raise own floor");

    let bytes = node
        .announcement_bytes_for_send_for_test()
        .expect("rebuilt");
    let rebuilt = CapabilityAnnouncement::from_bytes(&bytes).expect("decode");
    assert!(
        rebuilt.owner_cert.is_none(),
        "a self-floored certificate must not ride any send path"
    );
    assert!(rebuilt.verify().is_ok(), "the rebuild is re-signed");
    assert!(
        rebuilt.version > certified.version,
        "the cert-free replacement supersedes the certified form"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Review-9 addendum P1 (cached announcements, late-join path):
/// a peer that connects AFTER a self-floor raise receives the
/// node's capabilities but never the invalidated certificate — the
/// session-open push rebuilds before sending.
#[tokio::test]
async fn late_join_push_after_self_floor_carries_no_owner_cert() {
    let a = build_node().await;
    let dir = scratch_dir("late-join");

    let cert1 =
        OrgMembershipCert::try_issue(&org(), a.entity_id().clone(), 1, 3600).expect("issue");
    let authority =
        Arc::new(NodeAuthority::adopt(&dir, cert1, a.entity_id(), 0, None).expect("adopt"));
    a.install_node_authority(authority).expect("install");
    a.set_owner_cert_emission(true).expect("enable");
    a.announce_capabilities(CapabilitySet::new().add_tag("nrpc:oa1-echo"))
        .await
        .expect("announce");
    assert_eq!(
        owner_org_for(a.capability_fold(), a.node_id()),
        Some(org().org_id()),
        "certified announcement cached and self-projected"
    );

    // The self-floor rises AFTER the certified announcement was
    // cached; no re-announce happens.
    let mut floors = std::collections::BTreeMap::new();
    floors.insert(a.entity_id().clone(), 2u32);
    a.org_revocation_store()
        .expect("installed")
        .apply_bundle(&OrgRevocationBundle::try_issue(&org(), &floors).expect("issue"))
        .expect("raise own floor");

    // A late joiner receives the push on session open: caps yes,
    // ownership no.
    let b = build_node().await;
    handshake(&a, &b).await;
    let a_id = a.node_id();
    assert!(
        wait_until(&b, |n| {
            may_execute(n.capability_fold(), a_id, "nrpc:oa1-echo", 0xDEAD)
        })
        .await,
        "the late joiner still receives A's capabilities"
    );
    assert_eq!(
        owner_org_for(b.capability_fold(), a_id),
        None,
        "the session-open push must not carry the floored certificate"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Review-9 addendum P1 (cached announcements, deferred-flush
/// path): a self-floor raise during the rate-limit deferral window
/// means the trailing-edge flush broadcasts the REBUILT, cert-free
/// announcement — peers that projected the certified version see
/// it superseded.
#[tokio::test]
async fn deferred_flush_after_self_floor_carries_no_owner_cert() {
    // A long announce window so the second announce reliably
    // defers into a trailing-edge flush.
    let cfg = {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let mut cfg = MeshNodeConfig::new(addr, PSK)
            .with_heartbeat_interval(Duration::from_millis(200))
            .with_session_timeout(Duration::from_secs(5))
            .with_handshake(3, Duration::from_secs(2))
            .with_min_announce_interval(Duration::from_millis(400));
        cfg.socket_buffers = SocketBufferConfig {
            send_buffer_size: TEST_BUFFER_SIZE,
            recv_buffer_size: TEST_BUFFER_SIZE,
        };
        cfg
    };
    let a = Arc::new(
        MeshNode::new(EntityKeypair::generate(), cfg)
            .await
            .expect("MeshNode::new"),
    );
    let b = build_node().await;
    handshake(&a, &b).await;
    // The trailing-edge flush task holds a `Weak<MeshNode>`; it is
    // only schedulable on `start_arc`-started nodes (idempotent on
    // top of `start`, per capability_broadcast.rs).
    a.start_arc();

    let dir = scratch_dir("deferred-flush");
    let cert1 =
        OrgMembershipCert::try_issue(&org(), a.entity_id().clone(), 1, 3600).expect("issue");
    let authority =
        Arc::new(NodeAuthority::adopt(&dir, cert1, a.entity_id(), 0, None).expect("adopt"));
    a.install_node_authority(authority).expect("install");
    a.set_owner_cert_emission(true).expect("enable");

    // First announce broadcasts immediately: B projects ownership.
    a.announce_capabilities(CapabilitySet::new().add_tag("nrpc:oa1-echo"))
        .await
        .expect("announce 1");
    let a_id = a.node_id();
    assert!(
        wait_until(&b, |n| {
            owner_org_for(n.capability_fold(), a_id) == Some(org().org_id())
        })
        .await,
        "B projects the certified announcement"
    );

    // Second announce lands inside the window → deferred flush.
    a.announce_capabilities(CapabilitySet::new().add_tag("nrpc:oa1-echo"))
        .await
        .expect("announce 2 (deferred)");
    // The self-floor rises during the deferral window.
    let mut floors = std::collections::BTreeMap::new();
    floors.insert(a.entity_id().clone(), 2u32);
    a.org_revocation_store()
        .expect("installed")
        .apply_bundle(&OrgRevocationBundle::try_issue(&org(), &floors).expect("issue"))
        .expect("raise own floor");

    // The flush fires at window end with the REBUILT cert-free,
    // version-bumped announcement — B's projection is superseded.
    assert!(
        wait_until(&b, |n| owner_org_for(n.capability_fold(), a_id).is_none()).await,
        "the trailing-edge flush must not ship the floored certificate"
    );
    assert!(
        may_execute(b.capability_fold(), a_id, "nrpc:oa1-echo", 0xDEAD),
        "capabilities survive; only ownership is withdrawn"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Review-9 addendum P2 (observer theft): one store `Arc`
/// installed into TWO nodes keeps BOTH subscribed — a raise
/// retracts on both folds. Previously the second installation
/// silently overwrote the first node's callback.
#[tokio::test]
async fn one_store_installed_into_two_nodes_notifies_both() {
    let node1 = build_node().await;
    let node2 = build_node().await;
    let dir = scratch_dir("two-nodes");

    let store =
        Arc::new(OrgRevocationStore::init(dir.join("revocation-state.json")).expect("init"));
    node1
        .install_org_revocation_store(store.clone())
        .expect("install into node1");
    node2
        .install_org_revocation_store(store.clone())
        .expect("install into node2");

    // Identical generation-4 projections land on both nodes.
    let publisher = EntityKeypair::generate();
    let publisher_node_id = publisher.node_id();
    let cert4 = OrgMembershipCert::try_issue(&org(), publisher.entity_id().clone(), 4, 3600)
        .expect("issue");
    for node in [&node1, &node2] {
        let mut ann = CapabilityAnnouncement::new(
            publisher_node_id,
            publisher.entity_id().clone(),
            100,
            CapabilitySet::new().add_tag("nrpc:oa1-echo"),
        )
        .with_owner_cert(Some(cert4.clone()));
        ann.sign(&publisher);
        node.test_inject_capability_announcement(ann);
        assert_eq!(
            owner_org_for(node.capability_fold(), publisher_node_id),
            Some(org().org_id())
        );
    }

    // One raise retracts on BOTH nodes.
    let mut floors = std::collections::BTreeMap::new();
    floors.insert(publisher.entity_id().clone(), 5u32);
    store
        .apply_bundle(&OrgRevocationBundle::try_issue(&org(), &floors).expect("issue"))
        .expect("raise");
    for (name, node) in [("node1", &node1), ("node2", &node2)] {
        assert_eq!(
            owner_org_for(node.capability_fold(), publisher_node_id),
            None,
            "{name} must observe the retraction (registry, not a stolen slot)"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Review-11 P1 — publication serialization vs openers, and canonical
// dual-core locking (no ABBA under opposite cross-core swaps).
// ---------------------------------------------------------------------------

/// Review-11 P1: a store replacement holds the publish pin across
/// its dominance comparison AND swap, so a same-path opener cannot
/// publish (its `open_existing` blocks on the pinned core) inside
/// that interval — the exact review-10 red where an opener lifted
/// the active floor 10→5 mid-swap. Blocking is strictly stronger
/// than "can't lower": the opener cannot publish anything at all
/// while pinned.
#[tokio::test]
async fn opener_cannot_publish_during_store_replacement() {
    let node = build_node().await;
    let dir_a = scratch_dir("r11-openrepl-a");
    let dir_b = scratch_dir("r11-openrepl-b");
    let a_path = dir_a.join("revocation-state.json");

    let a = Arc::new(OrgRevocationStore::init(&a_path).expect("init A"));
    node.install_org_revocation_store(a.clone())
        .expect("install A");
    // Candidate B (different core, empty → dominates empty A).
    let b =
        Arc::new(OrgRevocationStore::init(dir_b.join("revocation-state.json")).expect("init B"));

    let opener_blocked = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let ob = opener_blocked.clone();
    let a_path_for_opener = a_path.clone();
    let n = node.clone();
    let install = tokio::task::spawn_blocking(move || {
        n.install_org_revocation_store_paused_for_test(b, &move || {
            // Pin held over A (current) + B (candidate). An opener of
            // A's path must NOT complete while the pin is held.
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            let ap = a_path_for_opener.clone();
            std::thread::spawn(move || {
                let s = OrgRevocationStore::open_existing(&ap).expect("open A");
                let _ = done_tx.send(());
                // Keep the handle alive briefly so its core join
                // actually publishes before the thread exits.
                std::thread::sleep(Duration::from_millis(50));
                drop(s);
            });
            let blocked = done_rx.recv_timeout(Duration::from_millis(400)).is_err();
            ob.store(blocked, std::sync::atomic::Ordering::Release);
        })
    });
    install
        .await
        .expect("join install")
        .expect("replacement ok");
    assert!(
        opener_blocked.load(std::sync::atomic::Ordering::Acquire),
        "a same-path opener published inside the replacement's pinned interval"
    );
    // The node ended up on B (the intended candidate).
    assert!(node
        .org_revocation_store()
        .expect("installed")
        .shares_core_with(&b_marker(&dir_b)));

    let _ = std::fs::remove_dir_all(&dir_a);
    let _ = std::fs::remove_dir_all(&dir_b);
}

/// Helper: reopen B's path to compare cores (the installed store is
/// B iff it shares B's core).
fn b_marker(dir_b: &PathBuf) -> OrgRevocationStore {
    OrgRevocationStore::open_existing(dir_b.join("revocation-state.json")).expect("reopen B")
}

/// Review-11 P1: authority installation pins BOTH the candidate and
/// the current store across self-verification and publication, so a
/// same-path opener cannot publish inside that interval either.
#[tokio::test]
async fn opener_cannot_publish_during_authority_installation() {
    let node = build_node().await;
    let dir_a = scratch_dir("r11-openauth-a");
    let dir_b = scratch_dir("r11-openauth-b");

    // Install an initial authority (store A).
    let cert_a =
        OrgMembershipCert::try_issue(&org(), node.entity_id().clone(), 1, 3600).expect("issue A");
    let auth_a =
        Arc::new(NodeAuthority::adopt(&dir_a, cert_a, node.entity_id(), 0, None).expect("adopt A"));
    node.install_node_authority(auth_a).expect("install A");
    let a_path = dir_a.join("revocation-state.json");

    // Candidate authority B (same owner org → same-org renewal).
    let cert_b =
        OrgMembershipCert::try_issue(&org(), node.entity_id().clone(), 2, 3600).expect("issue B");
    let auth_b =
        Arc::new(NodeAuthority::adopt(&dir_b, cert_b, node.entity_id(), 0, None).expect("adopt B"));

    let opener_blocked = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let ob = opener_blocked.clone();
    let n = node.clone();
    let install = tokio::task::spawn_blocking(move || {
        n.install_node_authority_paused_for_test(auth_b, &move || {
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            let ap = a_path.clone();
            std::thread::spawn(move || {
                let s = OrgRevocationStore::open_existing(&ap).expect("open A");
                let _ = done_tx.send(());
                std::thread::sleep(Duration::from_millis(50));
                drop(s);
            });
            let blocked = done_rx.recv_timeout(Duration::from_millis(400)).is_err();
            ob.store(blocked, std::sync::atomic::Ordering::Release);
        })
    });
    install.await.expect("join").expect("authority install ok");
    assert!(
        opener_blocked.load(std::sync::atomic::Ordering::Acquire),
        "a same-path opener published inside the authority install's pinned interval"
    );

    let _ = std::fs::remove_dir_all(&dir_a);
    let _ = std::fs::remove_dir_all(&dir_b);
}

/// Review-11 P1 (canonical dual-core locking): two nodes performing
/// the OPPOSITE cross-core store swap concurrently must both
/// terminate — `publish_guard_pair` locks distinct cores in
/// canonical normalized-path order, so no ABBA cycle forms even
/// though each node's `org_install` mutex is node-local.
#[tokio::test]
async fn two_nodes_opposite_store_swaps_do_not_deadlock() {
    let n1 = build_node().await;
    let n2 = build_node().await;
    let dir_a = scratch_dir("r11-abba-a");
    let dir_b = scratch_dir("r11-abba-b");

    // Empty stores dominate each other, so both swaps are legal.
    let a = Arc::new(OrgRevocationStore::init(dir_a.join("revocation-state.json")).expect("A"));
    let b = Arc::new(OrgRevocationStore::init(dir_b.join("revocation-state.json")).expect("B"));
    n1.install_org_revocation_store(a.clone())
        .expect("n1 installs A");
    n2.install_org_revocation_store(b.clone())
        .expect("n2 installs B");

    // n1: A → B, n2: B → A, concurrently. Loop a few rounds to make
    // the interleaving likely to hit any ordering bug.
    let outcome = tokio::time::timeout(Duration::from_secs(10), async {
        for _ in 0..50 {
            let (n1c, n2c) = (n1.clone(), n2.clone());
            let (bc, ac) = (b.clone(), a.clone());
            let t1 = tokio::task::spawn_blocking(move || n1c.install_org_revocation_store(bc));
            let t2 = tokio::task::spawn_blocking(move || n2c.install_org_revocation_store(ac));
            let _ = t1.await.expect("t1");
            let _ = t2.await.expect("t2");
            // Swap back so the next round repeats the opposite pair.
            let (n1c, n2c) = (n1.clone(), n2.clone());
            let (ac, bc) = (a.clone(), b.clone());
            let t1 = tokio::task::spawn_blocking(move || n1c.install_org_revocation_store(ac));
            let t2 = tokio::task::spawn_blocking(move || n2c.install_org_revocation_store(bc));
            let _ = t1.await.expect("t1b");
            let _ = t2.await.expect("t2b");
        }
    })
    .await;
    assert!(
        outcome.is_ok(),
        "opposite cross-core swaps deadlocked (canonical ordering failed)"
    );

    let _ = std::fs::remove_dir_all(&dir_a);
    let _ = std::fs::remove_dir_all(&dir_b);
}

// ---------------------------------------------------------------------------
// Review-11 P1 — stable send snapshot (seqlock over the SendStamp).
// ---------------------------------------------------------------------------

/// Review-11 P1: a floor that becomes live DURING the send
/// validation window — after the cached bytes are serialized but
/// before they are emitted — must not ship the certified form. The
/// SendStamp's store publish generation moved (bumped inside
/// StoreCore::publish, before any callback), so the stability
/// recheck fails and the seqlock retries, converging to cert-free
/// bytes. This closes the publish-before-notify gap the epoch
/// counter left open.
#[tokio::test]
async fn concurrent_floor_publish_during_send_retries_to_cert_free() {
    let node = build_node().await;
    let dir = scratch_dir("r11-seqlock");

    let cert1 =
        OrgMembershipCert::try_issue(&org(), node.entity_id().clone(), 1, 3600).expect("issue");
    let authority =
        Arc::new(NodeAuthority::adopt(&dir, cert1, node.entity_id(), 0, None).expect("adopt"));
    node.install_node_authority(authority).expect("install");
    node.set_owner_cert_emission(true).expect("enable");
    node.announce_capabilities(CapabilitySet::new().add_tag("nrpc:oa1-echo"))
        .await
        .expect("announce");

    // The probe fires inside the stability window (after the
    // certified bytes are serialized, before they are emitted) and
    // — exactly once — raises this node's own floor above the
    // cert's generation. The seqlock must detect the publish and
    // retry, so the returned bytes carry NO certificate.
    let fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let node_for_probe = node.clone();
    let f = fired.clone();
    let probe = move || {
        if f.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return; // one-shot: let the retry converge
        }
        let mut floors = std::collections::BTreeMap::new();
        floors.insert(node_for_probe.entity_id().clone(), 2u32);
        node_for_probe
            .org_revocation_store()
            .expect("installed")
            .apply_bundle(&OrgRevocationBundle::try_issue(&org(), &floors).expect("issue"))
            .expect("raise own floor");
    };
    let bytes = node
        .announcement_bytes_for_send_probed_for_test(&probe)
        .expect("send bytes");
    assert!(
        fired.load(std::sync::atomic::Ordering::Acquire),
        "probe must have fired inside the stability window"
    );
    let sent = CapabilityAnnouncement::from_bytes(&bytes).expect("decode");
    assert!(
        sent.owner_cert.is_none(),
        "a floor published during the send window must not ship the certified form"
    );
    assert!(
        sent.verify().is_ok(),
        "the rebuilt announcement is re-signed"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Review-11 P1: certificate wall-clock expiry is honored on every
/// reuse boundary even with NO authority/store/emission event —
/// expiry bumps no generation, so the epoch mechanism (review-10)
/// would keep taking the fast path. The seqlock re-derives the
/// desired cert every send (which checks temporal validity), so an
/// announcement cached while its short-lived cert was valid ships
/// cert-free, re-signed, and version-bumped once the window closes.
#[tokio::test]
async fn expired_certificate_cache_reuse_is_cert_free_without_authority_event() {
    let node = build_node().await;
    let dir = scratch_dir("r11-expiry");

    // A 1-second membership cert, adopted with zero skew.
    let cert =
        OrgMembershipCert::try_issue(&org(), node.entity_id().clone(), 1, 1).expect("issue 1s");
    let authority =
        Arc::new(NodeAuthority::adopt(&dir, cert, node.entity_id(), 0, None).expect("adopt"));
    node.install_node_authority(authority).expect("install");
    node.set_owner_cert_emission(true).expect("enable");
    node.announce_capabilities(CapabilitySet::new().add_tag("nrpc:oa1-echo"))
        .await
        .expect("announce");

    // While valid: the cached bytes carry the certificate.
    let certified = CapabilityAnnouncement::from_bytes(
        &node.announcement_bytes_for_send_for_test().expect("cached"),
    )
    .expect("decode");
    assert!(certified.owner_cert.is_some());
    assert!(certified.verify().is_ok());

    // Wait past the validity window with NO further authority
    // event (no floor raise, no reinstall, no emission toggle).
    tokio::time::sleep(Duration::from_millis(2500)).await;

    let bytes = node
        .announcement_bytes_for_send_for_test()
        .expect("post-expiry bytes");
    let sent = CapabilityAnnouncement::from_bytes(&bytes).expect("decode");
    assert!(
        sent.owner_cert.is_none(),
        "an expired certificate must not keep riding the wire"
    );
    assert!(sent.verify().is_ok(), "the cert-free rebuild is re-signed");
    assert!(
        sent.version > certified.version,
        "the cert-free replacement supersedes the certified form"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Review-11 P2 — subscription lifecycle (no core/callback leak on node drop).
// ---------------------------------------------------------------------------

/// Review-11 P2: dropping a node unsubscribes its raise callback
/// from the installed store's shared core. Without the RAII
/// unsubscription the core retains a callback capturing the node's
/// org_revocation slot (core → callback → slot → store → core), a
/// cycle that outlives the node and leaks the core plus its stale
/// fold. Here the test keeps the store handle alive but drops the
/// node, and the core's subscriber count returns to zero.
#[tokio::test]
async fn dropping_a_node_unsubscribes_its_raise_callback() {
    let dir = scratch_dir("r11-leak");
    let store =
        Arc::new(OrgRevocationStore::init(dir.join("revocation-state.json")).expect("init"));
    assert_eq!(store.subscriber_count(), 0);

    let node = build_node().await;
    node.install_org_revocation_store(store.clone())
        .expect("install");
    assert_eq!(
        store.subscriber_count(),
        1,
        "installation registers exactly one node subscriber"
    );

    // Drop the node (the test holds the only strong Arc, and an
    // unstarted node has no background task pinning it). Its Drop
    // must remove the callback from the shared core.
    drop(node);
    assert_eq!(
        store.subscriber_count(),
        0,
        "a dropped node must unsubscribe — else the core/callback leaks"
    );

    // The store still works after the node is gone (a raise simply
    // notifies nobody now).
    let mut floors = std::collections::BTreeMap::new();
    floors.insert(EntityKeypair::generate().entity_id().clone(), 5u32);
    store
        .apply_bundle(&OrgRevocationBundle::try_issue(&org(), &floors).expect("issue"))
        .expect("raise after node drop");

    let _ = std::fs::remove_dir_all(&dir);
}
