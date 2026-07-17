//! OA2-E1.1 / E1.3 foundations — provider self-verification and the
//! admission security stamp, against real `MeshNode` instances.
//!
//! These exercise the UNWIRED building blocks the live gate (E1.2)
//! will call: `verify_provider_authority` (registration-time
//! authority is not usable authority — an authority-dark or poisoned
//! provider cannot admit) and `capture_admission_stamp` (the §9.5
//! security fingerprint). No `serve_rpc` path is touched yet.

#![cfg(all(feature = "net", feature = "cortex"))]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::org::{OrgKeypair, OrgMembershipCert, OrgRevocationBundle};
use net::adapter::net::behavior::org_admission::AdmissionDenied;
use net::adapter::net::behavior::org_authority::NodeAuthority;
use net::adapter::net::org_admission_gate::{capture_admission_stamp, verify_provider_authority};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};

const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x42u8; 32];

fn test_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(3, Duration::from_secs(2));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: TEST_BUFFER_SIZE,
        recv_buffer_size: TEST_BUFFER_SIZE,
    };
    cfg
}

async fn build_node() -> Arc<MeshNode> {
    Arc::new(
        MeshNode::new(EntityKeypair::generate(), test_config())
            .await
            .expect("MeshNode::new"),
    )
}

fn org() -> OrgKeypair {
    OrgKeypair::from_bytes([0x42u8; 32])
}

fn scratch_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "net-oa2-admission-gate-{tag}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// Adopt `node` into `org()` (real ceremony, tempdir authority) and
/// install the authority as the production object.
async fn adopt_and_install(node: &Arc<MeshNode>, tag: &str) -> PathBuf {
    let dir = scratch_dir(tag);
    let cert = OrgMembershipCert::try_issue(&org(), node.entity_id().clone(), 1, 3600)
        .expect("issue cert");
    let authority = NodeAuthority::adopt(&dir, cert, node.entity_id(), 0, None).expect("adopt");
    node.install_node_authority(Arc::new(authority))
        .expect("install authority");
    dir
}

/// A healthy adopted provider yields its four facts: its own entity,
/// its proven owner org, a floor snapshot, and a non-empty stamp.
#[tokio::test]
async fn verify_provider_authority_returns_facts_for_a_healthy_provider() {
    let node = build_node().await;
    let dir = adopt_and_install(&node, "healthy").await;

    let facts = verify_provider_authority(&node).expect("healthy provider admits");
    assert_eq!(facts.provider, *node.entity_id());
    assert_eq!(facts.provider_owner_org, org().org_id());
    assert_ne!(facts.stamp.authority_ptr, 0, "authority installed");
    assert_ne!(facts.stamp.store_ptr, 0, "store installed");
    assert!(!facts.stamp.poisoned);

    // The captured stamp is current against a freshly-recomputed one
    // (no floor moved, same authority/store installed).
    assert!(
        facts.stamp.is_current(&capture_admission_stamp(&node)),
        "stamp stable when nothing changed",
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Witness 25 seed — an authority-dark node (never adopted) cannot
/// admit a protected call: no authority installed → the provider
/// self-verify is `ProviderAuthorityUnavailable`, and its stamp is
/// the empty (all-zero) fingerprint.
#[tokio::test]
async fn authority_dark_node_cannot_admit() {
    let node = build_node().await;
    assert!(node.node_authority().is_none());

    assert_eq!(
        verify_provider_authority(&node).map(|_| ()),
        Err(AdmissionDenied::ProviderAuthorityUnavailable),
    );

    let stamp = capture_admission_stamp(&node);
    assert_eq!(stamp.authority_ptr, 0);
    assert_eq!(stamp.store_ptr, 0);
    assert_eq!(stamp.store_generation, 0);
    assert!(!stamp.poisoned);
}

/// Installing an authority moves the stamp from the empty
/// fingerprint to a live one — so the §9.5 recheck would notice an
/// authority appearing or disappearing mid-flight.
#[tokio::test]
async fn stamp_reflects_authority_installation() {
    let node = build_node().await;
    let before = capture_admission_stamp(&node);
    assert_eq!(before.authority_ptr, 0);
    assert_eq!(before.store_ptr, 0);

    let dir = adopt_and_install(&node, "stamp").await;

    let after = capture_admission_stamp(&node);
    assert_ne!(after.authority_ptr, 0);
    assert_ne!(after.store_ptr, 0);
    // A stamp captured before installation is NOT current against the
    // installed view — the authority changed.
    assert!(!before.is_current(&after));

    let _ = std::fs::remove_dir_all(&dir);
}

/// A floor raised through the installed store bumps the store
/// generation, so a stamp captured before the raise is no longer
/// current — the exact signal the §9.5 recheck uses to catch a
/// mid-admission floor raise.
#[tokio::test]
async fn stamp_notices_a_floor_raise() {
    let node = build_node().await;
    let dir = adopt_and_install(&node, "floor").await;

    let facts = verify_provider_authority(&node).expect("healthy");
    let before = facts.stamp;

    // Raise a floor on the installed store (a real bundle apply).
    let store = node.org_revocation_store().expect("installed store");
    let subject = EntityKeypair::generate();
    let mut floors = std::collections::BTreeMap::new();
    floors.insert(subject.entity_id().clone(), 7u32);
    store
        .apply_bundle(&OrgRevocationBundle::try_issue(&org(), &floors).expect("issue"))
        .expect("apply floor");

    let after = capture_admission_stamp(&node);
    assert!(
        !before.is_current(&after),
        "a floor raise must invalidate a stamp captured before it",
    );
    assert!(
        after.store_generation > before.store_generation,
        "the store generation advanced",
    );

    let _ = std::fs::remove_dir_all(&dir);
}
