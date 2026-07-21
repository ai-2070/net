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

use net::adapter::net::behavior::admission_clock::ClockSample;
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

    let facts =
        verify_provider_authority(&node, &ClockSample::now()).expect("healthy provider admits");
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

/// KC4 (ABA) — ProviderFacts RETAINS the authority and store Arcs
/// for the admission's lifetime, so `stamp.authority_ptr` /
/// `store_ptr` cannot be reused by a different object under a §9.5
/// recheck. Holding `facts` therefore raises the installed Arcs'
/// strong counts; dropping it lowers them again.
#[tokio::test]
async fn provider_facts_pins_the_authority_and_store_arcs() {
    let node = build_node().await;
    let dir = adopt_and_install(&node, "aba-pin").await;

    let authority = node.node_authority().expect("authority");
    let store = node.org_revocation_store().expect("store");
    let auth_before = Arc::strong_count(&authority);
    let store_before = Arc::strong_count(&store);

    let facts = verify_provider_authority(&node, &ClockSample::now()).expect("healthy");
    assert!(
        Arc::strong_count(&authority) > auth_before,
        "facts must pin the authority Arc",
    );
    assert!(
        Arc::strong_count(&store) > store_before,
        "facts must pin the store Arc",
    );

    drop(facts);
    assert_eq!(
        Arc::strong_count(&authority),
        auth_before,
        "pin released on drop"
    );
    assert_eq!(
        Arc::strong_count(&store),
        store_before,
        "pin released on drop"
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
        verify_provider_authority(&node, &ClockSample::now()).map(|_| ()),
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

/// KC10 negative — a provider whose OWN owner cert has fallen BELOW a
/// revocation floor on its installed store can no longer admit: the
/// call-time self-verify fails → ProviderAuthorityUnavailable, even
/// though the authority verified at installation.
#[tokio::test]
async fn provider_below_its_own_floor_cannot_admit() {
    let node = build_node().await;
    let dir = adopt_and_install(&node, "below-floor").await;
    // Healthy first.
    assert!(verify_provider_authority(&node, &ClockSample::now()).is_ok());

    // Raise the floor for THIS node's membership (cert generation 1)
    // to 5 via a real bundle apply on the installed store.
    let store = node.org_revocation_store().expect("store");
    let mut floors = std::collections::BTreeMap::new();
    floors.insert(node.entity_id().clone(), 5u32);
    store
        .apply_bundle(&OrgRevocationBundle::try_issue(&org(), &floors).expect("issue"))
        .expect("apply floor");

    assert_eq!(
        verify_provider_authority(&node, &ClockSample::now()).map(|_| ()),
        Err(AdmissionDenied::ProviderAuthorityUnavailable),
        "a below-floor owner cert cannot admit",
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// KC10 negative — an EXPIRED provider owner cert cannot admit. The
/// cert is valid at adopt/install and expires shortly after; the
/// call-time self-verify then rejects it.
#[tokio::test]
async fn provider_with_expired_cert_cannot_admit() {
    let node = build_node().await;
    let dir = scratch_dir("expired");
    // Valid for 2 seconds, zero skew. (A 1s window is too tight
    // against the second-granularity clock — a boundary crossing
    // during adopt could expire it before the first assertion.)
    let cert =
        OrgMembershipCert::try_issue(&org(), node.entity_id().clone(), 1, 2).expect("issue cert");
    let authority = NodeAuthority::adopt(&dir, cert, node.entity_id(), 0, None).expect("adopt");
    node.install_node_authority(Arc::new(authority))
        .expect("install");
    // Valid immediately after install.
    assert!(verify_provider_authority(&node, &ClockSample::now()).is_ok());

    // §T9 — advance the CLOCK SAMPLE rather than sleeping.
    //
    // This slept 2500 ms against a 2 s certificate: a 500 ms margin on a
    // second-granularity `not_after`, under a suite that runs tests in
    // parallel on an oversubscribed CI CPU. That is a flake waiting for a
    // loaded runner, and a flake in a security suite is worse than a gap —
    // `.config/nextest.toml` gives these binaries `retries = 0` precisely so
    // an intermittent signal is not retried into green, so this would fail the
    // job outright.
    //
    // `verify_provider_authority` reads the SUPPLIED sample (pinned by
    // `provider_self_verify_reads_the_supplied_clock_sample` below), so a
    // sample past `not_after` is exactly equivalent and deterministic. It also
    // removes 2.5 s of wall time from every run.
    let expired_sample = ClockSample {
        wall_ns: ClockSample::now()
            .wall_ns
            .saturating_add(10 * 1_000_000_000),
        monotonic: std::time::Instant::now(),
    };
    assert_eq!(
        verify_provider_authority(&node, &expired_sample).map(|_| ()),
        Err(AdmissionDenied::ProviderAuthorityUnavailable),
        "an expired owner cert cannot admit",
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// KC10 negative — a POISONED installed store cannot admit: the
/// durability-uncertain branch denies before any self-verify.
#[tokio::test]
async fn provider_with_poisoned_store_cannot_admit() {
    let node = build_node().await;
    let dir = adopt_and_install(&node, "poisoned").await;
    assert!(verify_provider_authority(&node, &ClockSample::now()).is_ok());

    node.org_revocation_store()
        .expect("store")
        .mark_poisoned_for_test();

    assert_eq!(
        verify_provider_authority(&node, &ClockSample::now()).map(|_| ()),
        Err(AdmissionDenied::ProviderAuthorityUnavailable),
        "a poisoned store cannot admit",
    );
    // The live stamp also reflects the poison.
    assert!(capture_admission_stamp(&node).poisoned);
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

    let facts = verify_provider_authority(&node, &ClockSample::now()).expect("healthy");
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

/// AV-6 item 6: `verify_provider_authority` checks the provider's OWN
/// owner cert against the SUPPLIED `ClockSample`, never a fresh
/// `current_timestamp()` read. A sample far outside the cert's validity
/// window is refused even though `ClockSample::now()` admits — proving
/// the single admission clock is threaded into provider verification,
/// so it can't diverge from the caller-credential checks that read the
/// same sample. (Neuter: revert `self_verify_at(clock.wall_secs())` to
/// `self_verify` and the far-future sample is ignored → admits → red.)
#[tokio::test]
async fn provider_self_verify_reads_the_supplied_clock_sample() {
    let node = build_node().await;
    let dir = adopt_and_install(&node, "clock-thread").await;

    // The real clock admits (cert is fresh, valid ~3600s).
    assert!(verify_provider_authority(&node, &ClockSample::now()).is_ok());

    let now_ns = ClockSample::now().wall_ns;
    const HUGE_NS: u64 = 100_000 * 1_000_000_000; // ~100_000 s

    // A sample far in the FUTURE (past not_after, skew 0) → refused.
    let far_future = ClockSample {
        wall_ns: now_ns.saturating_add(HUGE_NS),
        monotonic: std::time::Instant::now(),
    };
    assert_eq!(
        verify_provider_authority(&node, &far_future).map(|_| ()),
        Err(AdmissionDenied::ProviderAuthorityUnavailable),
        "provider verification must read the supplied clock, not a fresh wall read",
    );

    // A sample far in the PAST (before not_before) → likewise refused.
    let far_past = ClockSample {
        wall_ns: now_ns.saturating_sub(HUGE_NS),
        monotonic: std::time::Instant::now(),
    };
    assert_eq!(
        verify_provider_authority(&node, &far_past).map(|_| ()),
        Err(AdmissionDenied::ProviderAuthorityUnavailable),
    );

    let _ = std::fs::remove_dir_all(&dir);
}
