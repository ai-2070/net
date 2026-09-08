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
    canonical_org_sensing_commitment, decode_interest_frame, encode_interest_frame,
    AudienceScopeCommitment, CanonicalConstraints, CapabilityId, DisclosureClass, DownstreamId,
    InterestSpec, ProviderInterestKey, ProviderSelector, ResultMode, SensingCounters,
    SensingInterestFrame, WorkLatencyEnvelope, SUBPROTOCOL_SENSING_INTEREST,
};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};

// A scratch directory holding an authority's revocation `.lock` sidecar is
// deliberately LEFT BEHIND when its test finishes.
//
// `OrgRevocationStore` keys its PROCESS-GLOBAL core registry by that sidecar's
// `(device, inode)`, so two path aliases of one sidecar share one live view
// (AV-9). Deleting the directory frees the inode while this test's core is
// still registered; Linux recycles a freed inode immediately, so the next store
// opened anywhere in this binary can land on it, derive the same `BackingId`,
// and join THIS test's core — inheriting its floors, its poison bit and its
// generation, and writing through a path that no longer exists
// (`state lock: No such file or directory`).
//
// The victims are whichever tests are scheduled next, so it surfaces as
// unrelated failures in varying combinations rather than as one deterministic
// break. Start-of-test resets stay: they run before anything is registered.

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

/// A scratch authority directory: created on construction, and deliberately
/// NOT removed on drop — see the note at the top of this file. The live
/// `OrgRevocationStore` is backed by this dir, and freeing its `.lock` inode
/// while the core keyed on it is still registered is what lets the next store
/// in this binary alias it.
struct ScratchDir(PathBuf);

impl ScratchDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("net-olb-piece5-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Self(dir)
    }
}

// NO cleanup `Drop`, deliberately — see the note at `ScratchDir`.
//
// Freeing this directory's revocation `.lock` inode while the core keyed
// on it is still live lets the NEXT store in this binary alias it. The
// residue a reused PID would trip over is handled by `fresh`'s
// start-of-test reset, which runs before anything is registered.

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

/// A compatible-floor peer with SENSING OFF drops the org frame and stays
/// Unknown — with no legacy downgrade anywhere, and with the intended wire leg
/// ACKNOWLEDGED rather than assumed.
///
/// This drives the LOCAL-ORIGIN lease path, not a hand-built frame: A holds a
/// real organization authority, so `acquire_sensing_interest_lease` takes the
/// organization plane, authors the registration under the canonical commitment
/// and sends it from the node-owned ordered egress. D is an ordinary
/// same-organization peer that simply does not run the sensing plane.
///
/// What must hold, and what must NOT:
///
/// * D installs nothing and moves no sensing counter — the dark receiver drops
///   both sensing subprotocols before decode;
/// * A's own row is rooted at the ORGANIZATION commitment. A peer that cannot
///   answer must never cause the local leg to be re-authored under a legacy
///   entity root, which would be an authority downgrade bought with silence;
/// * A's projection for that branch stays `Unknown`. Absence of evidence is
///   never NotReady, so nothing about D is prunable.
#[tokio::test]
async fn a_floored_peer_with_sensing_off_drops_the_org_frame_and_stays_unknown() {
    let commitment = canonical_org_sensing_commitment(&org().org_id());

    // A: organization-authoritative consumer with sensing ON.
    let a = Arc::new(
        MeshNode::new(
            EntityKeypair::generate(),
            base_config().with_sensing_coalescing(true),
        )
        .await
        .expect("MeshNode::new A"),
    );
    // D: same organization, compatible in every other way, sensing OFF (the
    // default - asserted below rather than assumed).
    let dark_config = base_config();
    assert!(
        !dark_config.enable_sensing_coalescing,
        "sensing must be off by default, or this witness is testing nothing"
    );
    let d = Arc::new(
        MeshNode::new(EntityKeypair::generate(), dark_config)
            .await
            .expect("MeshNode::new D"),
    );
    let _a_dir = adopt_and_install(&a, "dark-consumer").await;
    let _d_dir = adopt_and_install(&d, "dark-peer").await;

    connect_pair(&a, &d).await;
    a.start();
    d.start();
    for node in [&a, &d] {
        node.announce_capabilities(net::adapter::net::behavior::capability::CapabilitySet::new())
            .await
            .expect("announce");
    }
    let (a_id, d_id) = (a.node_id(), d.node_id());
    {
        let (a, d) = (a.clone(), d.clone());
        await_condition(
            Duration::from_secs(5),
            "entity pins established",
            move || a.peer_entity_id(d_id).is_some() && d.peer_entity_id(a_id).is_some(),
        )
        .await;
    }
    assert!(
        a.sensing_enabled(),
        "precondition: A runs the sensing plane"
    );
    assert!(!d.sensing_enabled(), "precondition: D does not");

    // E: the POSITIVE CONTROL peer - same organization, same everything, except
    // that it runs the sensing plane.
    let e = Arc::new(
        MeshNode::new(
            EntityKeypair::generate(),
            base_config().with_sensing_coalescing(true),
        )
        .await
        .expect("MeshNode::new E"),
    );
    let _e_dir = adopt_and_install(&e, "dark-control").await;
    connect_pair(&a, &e).await;
    e.start();
    e.announce_capabilities(net::adapter::net::behavior::capability::CapabilitySet::new())
        .await
        .expect("announce");
    let e_id = e.node_id();
    {
        let (a2, e2) = (a.clone(), e.clone());
        await_condition(Duration::from_secs(5), "E's pins established", move || {
            a2.peer_entity_id(e_id).is_some() && e2.peer_entity_id(a_id).is_some()
        })
        .await;
    }

    // D'S OWN ARRIVAL EVIDENCE. An empty receiver proves nothing by itself:
    // a datagram that was lost, or one aimed at a node that had already
    // stopped, leaves exactly the same empty table. Only the receiver's own
    // event distinguishes "the registration arrived and the dark plane
    // dropped it" from "nothing arrived", so D acknowledges its own dark
    // 0x0C02 drop and the witness identifies the registration it dropped:
    // the authenticated sender, the target branch, the organization scope
    // and the certificate naming the sending hop. Nothing is acknowledged on
    // the wire - no reply, no retry, no reliability - and D's sensing plane
    // stays off throughout.
    let a_entity = a.entity_keypair().entity_id().clone();
    let dropped = Arc::new(parking_lot::Mutex::new(Vec::<(
        u64,
        Option<SensingInterestFrame>,
    )>::new()));
    {
        let dropped = Arc::clone(&dropped);
        d.set_sensing_dark_drop_observer_for_test(Arc::new(move |from, payload| {
            let frame = decode_interest_frame(payload).ok();
            dropped.lock().push((from, frame));
        }));
    }

    // THE PRODUCTION LOCAL-ORIGIN PATH: an organization lease toward D.
    let spec = org_spec(d_id, commitment);
    let key = ProviderInterestKey::new(spec.key(), d_id);
    let ticket = a
        .acquire_sensing_interest_lease(&spec, d_id, D)
        .expect("the organization lease is authored locally regardless of the peer");

    // A's OWN row is organization-rooted. No downgrade, no legacy fallback.
    let local = a
        .sensing_downstream_entry(&key, DownstreamId::LeasedLocal)
        .expect("A's own leased row exists");
    assert_eq!(
        local.owner_root, commitment,
        "the local leg stays rooted at the organization commitment - a silent \
         peer must not buy a legacy re-authoring"
    );

    // The registration REACHED D and D's disabled plane is what dropped it.
    {
        let dropped = Arc::clone(&dropped);
        let a_entity = a_entity.clone();
        await_condition(
            Duration::from_secs(5),
            "D's own dark plane acknowledged THIS registration arriving",
            move || {
                dropped.lock().iter().any(|(from, frame)| {
                    *from == a_id
                        && matches!(
                            frame,
                            Some(SensingInterestFrame::OrgProviderRegistration {
                                target,
                                audience_scope,
                                subscriber_membership,
                                ..
                            }) if *target == d_id
                                && *audience_scope == commitment
                                && subscriber_membership.member == a_entity
                                && subscriber_membership.org_id == org().org_id()
                        )
                })
            },
        )
        .await;
    }

    // ...and it arrived exactly as an ORGANIZATION registration: no legacy
    // shape was ever put on the wire for a peer that answers nothing.
    for (from, frame) in dropped.lock().iter() {
        assert_eq!(*from, a_id, "only A's session delivered anything here");
        assert!(
            matches!(
                frame,
                Some(SensingInterestFrame::OrgProviderRegistration { .. })
            ),
            "a silent peer must never buy a legacy downgrade: {frame:?}"
        );
    }

    // E: an OPTIONAL positive control - same organization, same lease path,
    // same instant, differing only in running the sensing plane. It cannot
    // substitute for D's own event above; it shows the identical leg is one a
    // sensing-enabled peer installs.
    let e_spec = org_spec(e_id, commitment);
    let e_key = ProviderInterestKey::new(e_spec.key(), e_id);
    let e_ticket = a
        .acquire_sensing_interest_lease(&e_spec, e_id, D)
        .expect("the same lease path toward the sensing-enabled peer");
    {
        let e = e.clone();
        let e_key = e_key.clone();
        await_condition(
            Duration::from_secs(5),
            "the identical leg reaches a peer that RUNS the sensing plane",
            move || {
                e.sensing_downstream_entry(&e_key, DownstreamId::Peer(a_id))
                    .is_some()
            },
        )
        .await;
    }
    let e_row = e
        .sensing_downstream_entry(&e_key, DownstreamId::Peer(a_id))
        .expect("E's row for A");
    assert_eq!(
        e_row.owner_root, commitment,
        "and it is organization-rooted, so the leg D dropped was a well-formed \
         organization registration"
    );

    assert!(
        d.sensing_table_is_empty(),
        "a dark peer must gain no sensing rows"
    );
    assert!(
        d.sensing_downstreams(&key).is_empty(),
        "and specifically none for this branch"
    );
    for counter in [
        SensingCounters::get(&d.sensing_counters().protocol_invalid),
        SensingCounters::get(&d.sensing_counters().scope_refusals),
    ] {
        assert_eq!(counter, 0, "a dark peer must move zero sensing counters");
    }
    assert_eq!(
        a.sensing_projected(&key),
        net::adapter::net::behavior::sensing::ProjectedReadiness::Unknown,
        "silence is Unknown - never NotReady, so nothing about a dark peer is \
         prunable"
    );
    assert!(
        a.sensing_latest_attestation(&key).is_none(),
        "and no observation exists to have derived a verdict from"
    );

    let _ = a.try_release_sensing_interest_lease(ticket);
    let _ = a.try_release_sensing_interest_lease(e_ticket);
}
