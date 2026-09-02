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
//! # Transition CLOSURE
//!
//! The six tests below close the transition cycle over real UDP, not just its
//! opening move. Each one is a bounded transition observed from the PROVIDER
//! side:
//!
//! 1. [`an_org_lease_reaches_the_provider_through_real_intake`] — first
//!    registration, then a strictest-holder TIGHTENING.
//! 2. [`releasing_the_strictest_org_holder_relaxes_the_provider_row`] — the
//!    strictest holder goes away and the surviving holder's cadence must be
//!    RE-AUTHORED under fresh organization authority. The transport twin of the
//!    in-crate surviving-holder witness.
//! 3. [`releasing_the_last_org_holder_deregisters_the_provider_row`] — the FINAL
//!    release, whose `Deregister` frames now leave from Phase 2 rather than
//!    inline under the sensing guards. This is the first transport proof of the
//!    deregister direction at all.
//! 4. [`a_stale_org_capture_refuses_and_puts_nothing_on_the_wire`] — an
//!    organization acquisition whose captured authority view is invalidated
//!    inside the REAL production window must refuse and emit NOTHING; the
//!    provider row may not move by so much as a cadence.
//! 5. [`the_later_org_cadence_decision_is_what_the_provider_finally_holds`] —
//!    two cadence decisions CONTEND. The first is parked inside Phase 2, after
//!    its commit and BEFORE its transport effect, and the second is taken while
//!    it is parked. The provider's FINAL row must carry the LATER decision.
//! 6. [`a_parked_org_teardown_cannot_be_overtaken_by_its_own_reacquisition`] —
//!    the same contention across the teardown boundary: a FINAL release is
//!    parked in Phase 2 and a re-acquisition is taken while it is parked. The
//!    provider must end with the re-acquisition's FRESH row and never with the
//!    teardown applied last — the stale resurrection the pre-repair egress
//!    permitted — and, with nothing following it, a final teardown must still
//!    leave NO row at all.
//! 7. [`the_production_send_boundary_is_strictly_serial_in_enqueue_order`] — the
//!    TRANSPORT BOUNDARY itself. Tests 5 and 6 prove the datagrams enter the
//!    ordered egress and that the provider's final row is the later decision's;
//!    neither can fail for a mutant that keeps the queue and then races the
//!    dequeued sends. This one observes each production `send_to` and requires
//!    the consumer to complete send *i* before starting send *i+1*.
//! 8. [`the_ordered_egress_is_bounded_and_keeps_the_latest_decision`] — the
//!    egress is BOUNDED under a consumer that provably cannot run, overflow is
//!    counted, and the LATEST committed decision still reaches the provider.
//! 9. [`the_egress_consumer_does_not_survive_node_shutdown`] — the egress is
//!    node-OWNED: `shutdown` closes and joins its single consumer.
//!
//! Topology is two nodes, one organization, no chaos injection, single send per
//! transition with a soft-state refresh loop for UDP best-effort. Every test
//! that observes a RELAXATION, a REMOVAL or an ABSENCE of output aborts the
//! refresher first — otherwise the re-drive would either mask the transition or
//! reinstall the row underneath the assertion.
//!
//! Tests 5 and 6 additionally contend two decisions for ONE interest against
//! each other, and are deliberately SINGLE-threaded (`#[tokio::test]`, one
//! worker): see [`unpark_and_measure_queue`] for why exactly one worker is
//! load-bearing there. Test 8 is single-threaded for the same reason — blocking
//! the only worker is what makes "the consumer cannot run" a fact. Test 7 is
//! the opposite: it needs SEVERAL workers, or a racing consumer's rival sends
//! could not run either and the discrimination would be vacuous.
//!
//! Run:
//!
//! ```text
//! cargo test --test sensing_org_lease_wire --features "net fixtures"
//! ```
//!
//! `--features net` ALONE DOES NOT WORK, and the previously documented command
//! was therefore wrong even for the four tests that predate this note: tests 4-6
//! drive `MeshNode::set_sensing_phase_two_seam_for_test`, and tests 5-9 drive
//! `MeshNode::org_egress_state_for_test` /
//! `MeshNode::set_org_egress_send_observer_for_test`. All are
//! `#[cfg(any(test, feature = "fixtures"))]`, and an integration test is a
//! separate crate, so `test` is not set on `net` — only `fixtures` exposes them.

#![cfg(feature = "net")]

mod common;
use common::*;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::behavior::org::{OrgKeypair, OrgMembershipCert, OrgRevocationBundle};
use net::adapter::net::behavior::org_authority::NodeAuthority;
use net::adapter::net::behavior::sensing::{
    canonical_org_sensing_commitment, AudienceScopeCommitment, CanonicalConstraints, CapabilityId,
    DisclosureClass, DownstreamId, InterestSpec, ProviderInterestKey, ProviderSelector, ResultMode,
    SensingCounters, WorkLatencyEnvelope,
};
use net::adapter::net::identity::EntityId;
use net::adapter::net::{
    EntityKeypair, MeshNode, MeshNodeConfig, OrgEgressSendPhase, SensingRegistrationError,
    SocketBufferConfig,
};
use net::adapter::Adapter;

/// Requested sample interval D.
const D: Duration = Duration::from_millis(200);
/// A stricter cadence, to drive a second (re-authoring) transition.
const STRICT: Duration = Duration::from_millis(50);
/// Comfortably past the upstream damper min-gap (100 ms).
const PAST_MIN_GAP: Duration = Duration::from_millis(180);
const POLL: Duration = Duration::from_secs(5);
/// How long a "nothing may happen" window is left open before the negative
/// assertion is taken. Generous relative to a loopback RTT (sub-millisecond)
/// and to the damper min-gap, so a frame that WAS sent has long since landed.
const SETTLE: Duration = Duration::from_millis(750);
/// A middle cadence: tighter than [`D`], looser than [`STRICT`]. The PARKED
/// decision's cadence in test 5, so the final row names which decision won.
const MID: Duration = Duration::from_millis(100);
/// The re-acquisition's cadence in test 6. Distinct from every other cadence in
/// this file, so the provider row identifies exactly which decision authored
/// it: only the re-acquisition can produce this value.
const FRESH: Duration = Duration::from_millis(120);
/// How long the runtime thread is held BLOCKED while two contending
/// transitions hand their datagrams to the transport. See
/// [`unpark_and_measure_queue`]: both transitions run on blocking threads and
/// their whole commit-and-hand-off path is synchronous, so this only has to
/// cover a 2 ms park poll, a mutex hand-off and two frame authorings.
const HANDOFF: Duration = Duration::from_millis(250);

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

/// Two live, connected, ORGANIZATION-authoritative nodes: A authors, B verifies.
///
/// The scratch dirs ride along in the return value; dropping them early would
/// only free the paths (nothing is deleted), but they are kept so the authority
/// objects' provenance is obvious at each call site.
struct OrgPair {
    a: Arc<MeshNode>,
    b: Arc<MeshNode>,
    _a_dir: ScratchDir,
    _b_dir: ScratchDir,
}

async fn org_pair(tag: &str) -> OrgPair {
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
    let a_dir = adopt_and_install(&a, &format!("{tag}-a"));
    let b_dir = adopt_and_install(&b, &format!("{tag}-b"));
    bring_up(&a, &b).await;
    OrgPair {
        a,
        b,
        _a_dir: a_dir,
        _b_dir: b_dir,
    }
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

/// [`org_spec`] with a DISTINCT interest identity per `index`.
///
/// The upstream min-gap damper is keyed by `(provider, interest_digest)`, so a
/// witness that needs N real emissions in quick succession needs N distinct
/// interests; re-registering one interest would be damped to a single frame.
fn distinct_org_spec(target: u64, index: usize) -> InterestSpec {
    InterestSpec {
        constraints: CanonicalConstraints::from_entries([
            ("model", "llama-70b"),
            ("shard", &index.to_string()),
        ])
        .unwrap(),
        ..org_spec(target)
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

/// Soft-state re-drive: the lease has no ttl/2 refresh owner in this slice, and
/// UDP is best-effort, so re-acquire an extra holder periodically to re-emit.
/// Each acquisition is idempotent at the provider.
///
/// A refresher only ever re-drives cadence `interval`; it can never tighten a
/// row, relax a row below the surviving holders' aggregate, or remove one. It
/// must nonetheless be ABORTED before any assertion about a relaxation, a
/// removal or an absence — a re-drive after the transition would reinstall the
/// row and hide a missing production send.
fn spawn_refresher(
    a: &Arc<MeshNode>,
    spec: &InterestSpec,
    provider: u64,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    let a = a.clone();
    let spec = spec.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(PAST_MIN_GAP).await;
            if let Ok(t) = a.acquire_sensing_interest_lease(&spec, provider, interval) {
                a.release_sensing_interest_lease(t)
                    .expect("the release must not be refused");
            }
        }
    })
}

/// The production lease path emits an authenticated `OrgProviderRegistration`
/// that reaches the peer through its ordinary intake and installs under the
/// organization-derived root.
#[tokio::test]
async fn an_org_lease_reaches_the_provider_through_real_intake() {
    let OrgPair { a, b, .. } = org_pair("intake").await;

    let a_id = a.node_id();
    let b_id = b.node_id();
    let spec = org_spec(b_id);
    let key = ProviderInterestKey::new(spec.key(), b_id);
    let commitment = canonical_org_sensing_commitment(&org().org_id());

    let protocol_invalid_before = SensingCounters::get(&b.sensing_counters().protocol_invalid);

    let refresher = spawn_refresher(&a, &spec, b_id, D);

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
    a.release_sensing_interest_lease(strict)
        .expect("the release must not be refused");
    a.release_sensing_interest_lease(loose)
        .expect("the release must not be refused");
    a.shutdown().await.expect("shutdown A");
    b.shutdown().await.expect("shutdown B");
}

/// TRANSITION 3 of the cycle, over real UDP: the STRICTEST holder goes away
/// while a looser one survives.
///
/// The surviving-holder release is the only lease transition that must AUTHOR a
/// brand-new organization registration under FRESHLY captured authority — it is
/// not a teardown, and it is the one release that
/// `release_sensing_interest_lease` can refuse. The in-crate witness proves the
/// planner produces the relaxed org variant; this proves the bytes leave the
/// socket, survive B's org authority gate, and move B's own row.
///
/// The refresher is aborted BEFORE the strict release. It re-drives cadence D,
/// so leaving it running would relax the row for free and the test would stay
/// green with the release-side re-authoring deleted.
#[tokio::test]
async fn releasing_the_strictest_org_holder_relaxes_the_provider_row() {
    let OrgPair { a, b, .. } = org_pair("relax").await;

    let a_id = a.node_id();
    let b_id = b.node_id();
    let spec = org_spec(b_id);
    let key = ProviderInterestKey::new(spec.key(), b_id);
    let commitment = canonical_org_sensing_commitment(&org().org_id());

    let protocol_invalid_before = SensingCounters::get(&b.sensing_counters().protocol_invalid);
    let refresher = spawn_refresher(&a, &spec, b_id, D);

    let loose = a
        .acquire_sensing_interest_lease(&spec, b_id, D)
        .expect("the loose own-org holder must acquire");
    assert!(
        poll_until(POLL, || peer_row(&b, &key, a_id).map(|(d, _)| d) == Some(D)).await,
        "the provider never reached the loose cadence {D:?} — the initial organization \
         registration did not cross the wire"
    );

    tokio::time::sleep(PAST_MIN_GAP).await;
    let strict = a
        .acquire_sensing_interest_lease(&spec, b_id, STRICT)
        .expect("the strict own-org holder must acquire");
    assert!(
        poll_until(POLL, || peer_row(&b, &key, a_id).map(|(d, _)| d)
            == Some(STRICT))
        .await,
        "the provider never tightened to {STRICT:?} — the strictest-holder re-authoring \
         did not cross the wire"
    );

    // From here on the ONLY thing that may move B's row is the release-path
    // re-authoring. No re-drive, no second holder, no helper frame.
    refresher.abort();
    tokio::time::sleep(PAST_MIN_GAP).await;

    a.release_sensing_interest_lease(strict)
        .expect("the surviving-holder release must not be refused — A's authority is live");

    assert!(
        poll_until(POLL, || peer_row(&b, &key, a_id).map(|(d, _)| d) == Some(D)).await,
        "the provider never relaxed back to {D:?} after the strictest holder was \
         released. WHAT BREAKS THIS: deleting the Phase-2 `emit_pending_org_send` for \
         the release-path `Reregister` (no frame at all); emitting the relaxed \
         registration as LEGACY bytes (B is org-authoritative and refuses the org \
         audience on the legacy arm as authority laundering, so the row stays at \
         {STRICT:?}); or letting the relaxed frame be OVERTAKEN by a stale strict \
         frame outside `org_transition_mu`'s total order, which leaves the row at \
         {STRICT:?} forever"
    );
    let (interval, root) = peer_row(&b, &key, a_id).expect("row present");
    assert_eq!(interval, D, "at exactly the surviving holder's cadence");
    assert_eq!(
        root, commitment,
        "and the RELAXED row is still keyed under the canonical ORGANIZATION \
         commitment. WHAT BREAKS THIS: re-authoring the release as a legacy \
         `ProviderRegistration`, which would carry A's legacy entity root — the \
         org->legacy downgrade the lease plane record exists to prevent"
    );
    assert_eq!(
        SensingCounters::get(&b.sensing_counters().protocol_invalid),
        protocol_invalid_before,
        "and B admitted every frame in this transition without a single \
         protocol-invalid refusal. WHAT BREAKS THIS: legacy bytes carrying the org \
         audience, which B counts here before installing nothing"
    );

    a.release_sensing_interest_lease(loose)
        .expect("the release must not be refused");
    a.shutdown().await.expect("shutdown A");
    b.shutdown().await.expect("shutdown B");
}

/// TRANSITION 4 — the FINAL release, and the first transport proof of the
/// DEREGISTER direction.
///
/// `Deregister` frames used to be encoded and sent inline, under the projection
/// and lease-apply guards. They now leave from Phase 2: Phase 1 returns the
/// branch keys and `commit_transition_phase_two` sends them after every sensing
/// guard is released. Nothing outside this file observes those bytes actually
/// leaving; every other deregister witness reads local state.
///
/// The refresher MUST be aborted first: a re-drive after the final release
/// reinstalls the row and the absence assertion becomes untestable.
#[tokio::test]
async fn releasing_the_last_org_holder_deregisters_the_provider_row() {
    let OrgPair { a, b, .. } = org_pair("dereg").await;

    let a_id = a.node_id();
    let b_id = b.node_id();
    let spec = org_spec(b_id);
    let key = ProviderInterestKey::new(spec.key(), b_id);

    let protocol_invalid_before = SensingCounters::get(&b.sensing_counters().protocol_invalid);
    let refresher = spawn_refresher(&a, &spec, b_id, D);

    let only = a
        .acquire_sensing_interest_lease(&spec, b_id, D)
        .expect("the only own-org holder must acquire");
    assert!(
        poll_until(POLL, || peer_row(&b, &key, a_id).map(|(d, _)| d) == Some(D)).await,
        "the provider never installed the row at {D:?}, so there is nothing whose \
         removal could be observed"
    );

    // The row must now be torn down by the FINAL release and by nothing else.
    // B's soft-state ttl is 30 s and the poll below is 5 s, so expiry cannot
    // stand in for the Deregister frame.
    refresher.abort();
    tokio::time::sleep(PAST_MIN_GAP).await;
    assert!(
        peer_row(&b, &key, a_id).is_some(),
        "the row must still be live at the moment of the final release — otherwise \
         the removal below would prove nothing about the Deregister frame"
    );

    a.release_sensing_interest_lease(only)
        .expect("a FINAL organization release can never be refused");

    assert!(
        poll_until(POLL, || b
            .sensing_downstream_entry(&key, DownstreamId::Peer(a_id))
            .is_none())
        .await,
        "the provider's row for A survived the final release. WHAT BREAKS THIS: \
         deleting the `for branch_key in &pending.deregistrations` send loop in \
         `commit_transition_phase_two` (Phase 1 still computes the branch keys and \
         every local witness stays green, but no bytes leave and B keeps the row for \
         its full 30 s ttl); dropping the branch keys out of `PendingTransition` when \
         the deregister direction was moved into Phase 2; or reordering the final \
         `Deregister` AHEAD of the preceding registration for the same key, which \
         would let the stale registration land last and leave the row alive"
    );
    assert_eq!(
        SensingCounters::get(&b.sensing_counters().protocol_invalid),
        protocol_invalid_before,
        "and the removal happened through ADMITTED bytes, not a refusal: the \
         `Deregister` frame is plane-independent and must pass B's intake cleanly. \
         WHAT BREAKS THIS: an org-shaped or malformed teardown frame that B rejects \
         — the row would also disappear on ttl expiry later, so the counter is what \
         distinguishes 'B accepted a Deregister' from 'B threw the bytes away'"
    );

    a.shutdown().await.expect("shutdown A");
    b.shutdown().await.expect("shutdown B");
}

/// CURRENTNESS, driven through the REAL production window, observed from the
/// PROVIDER.
///
/// The in-crate witnesses drive this window through the private
/// `acquire_sensing_interest_lease_seamed` Phase-0 seam, which is not reachable
/// from an integration test. This test builds the same window out of the two
/// PUBLIC fixtures seams plus real production locking:
///
/// 1. A one-shot Phase-2 seam parks transition T1 inside
///    `commit_transition_phase_two`. At that point T1 holds `org_transition_mu`
///    and NOTHING else — every sensing guard, and `org_install`, are released.
/// 2. T2 then runs the acquisition under test. Its Phase 0 capture succeeds
///    (`org_install` is free) and it blocks on `org_transition_mu` — i.e. it is
///    parked in exactly the capture-to-fence window.
/// 3. A live revocation-floor raise (`OrgRevocationBundle::try_issue` +
///    `apply_bundle`) moves the installed store's publication generation, which
///    is part of T2's pinned stamp. `org_install` is free, so it lands.
/// 4. T1 is unparked. T2 acquires the order lock, reaches the final currentness
///    FENCE, finds its view stale, and must refuse.
///
/// The assertion is that this refusal is SILENT on the wire: B's row does not
/// gain, move or lose anything, and B counts no refusal — because a refused
/// acquisition must never have authored bytes in the first place.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stale_org_capture_refuses_and_puts_nothing_on_the_wire() {
    let OrgPair { a, b, .. } = org_pair("stale").await;

    let a_id = a.node_id();
    let b_id = b.node_id();
    let spec = org_spec(b_id);
    let key = ProviderInterestKey::new(spec.key(), b_id);
    let commitment = canonical_org_sensing_commitment(&org().org_id());

    // Establish the row that must NOT move, then stop all re-drive so the
    // window below is quiet.
    let refresher = spawn_refresher(&a, &spec, b_id, D);
    let loose = a
        .acquire_sensing_interest_lease(&spec, b_id, D)
        .expect("the loose own-org holder must acquire");
    assert!(
        poll_until(POLL, || peer_row(&b, &key, a_id).map(|(d, _)| d) == Some(D)).await,
        "the provider never installed the baseline row at {D:?}"
    );
    refresher.abort();
    tokio::time::sleep(PAST_MIN_GAP).await;

    let protocol_invalid_before = SensingCounters::get(&b.sensing_counters().protocol_invalid);
    let stale_stamp_before = SensingCounters::get(&a.sensing_counters().org_stale_stamp);

    // (1) Park one organization transition inside Phase 2, holding
    //     `org_transition_mu` and nothing else.
    let entered = Arc::new(AtomicBool::new(false));
    let unpark = Arc::new(AtomicBool::new(false));
    let armed = Arc::new(AtomicBool::new(true));
    {
        let entered = entered.clone();
        let unpark = unpark.clone();
        let armed = armed.clone();
        a.set_sensing_phase_two_seam_for_test(Arc::new(move || {
            // One-shot: only the first transition parks. Later transitions —
            // including the unparked one's own follow-ups and the release at
            // the end — must run straight through.
            if !armed.swap(false, Ordering::SeqCst) {
                return;
            }
            entered.store(true, Ordering::SeqCst);
            while !unpark.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(2));
            }
        }));
    }
    // The parked transition must be one that actually EMITS: the Phase-2 seam
    // fires once per emitting transition, not per phase entry (the acquire
    // rollback path reaches Phase 2 with nothing to send, so firing on entry
    // would make "nothing was emitted" unprovable). A second holder at the SAME
    // cadence `D` yields `LeaseAction::Unchanged` and emits nothing, so park a
    // strictly TIGHTER holder, which yields a `Reregister`.
    let park_interval = Duration::from_millis(100);
    assert!(
        park_interval < D && STRICT < park_interval,
        "the parked cadence must tighten against D and still leave room for STRICT"
    );
    let parked = {
        let a = a.clone();
        let spec = spec.clone();
        tokio::task::spawn_blocking(move || {
            a.acquire_sensing_interest_lease(&spec, b_id, park_interval)
        })
    };
    await_condition(POLL, "a transition parked in Phase 2", || {
        entered.load(Ordering::SeqCst)
    })
    .await;

    // (2) The acquisition under test captures its authority view and then blocks
    //     on the transition order lock — inside the production window.
    let stale = {
        let a = a.clone();
        let spec = spec.clone();
        tokio::task::spawn_blocking(move || a.acquire_sensing_interest_lease(&spec, b_id, STRICT))
    };
    // Phase 0 is a sub-millisecond self-verify; this only has to cover blocking-
    // pool scheduling. Mistiming does not silently pass: an acquisition that
    // captured AFTER the raise below would SUCCEED, and the `expect_err` and the
    // `org_stale_stamp` assertion would both fail loudly.
    tokio::time::sleep(SETTLE).await;

    // (3) Invalidate the captured view for real: a signed floor raise applied to
    //     the INSTALLED store moves its publication generation.
    let mut floors = BTreeMap::new();
    floors.insert(EntityId::from_bytes([0x77u8; 32]), 5u32);
    let bundle = OrgRevocationBundle::try_issue(&org(), &floors).expect("issue a floor bundle");
    a.org_revocation_store()
        .expect("A has an installed revocation store")
        .apply_bundle(&bundle)
        .expect("the floor raise must apply");

    // (4) Release the parked transition; the staled one now reaches the fence.
    unpark.store(true, Ordering::SeqCst);
    let parked_ticket = parked
        .await
        .expect("the parked acquisition joins")
        .expect("the parked holder acquires");
    let err = stale
        .await
        .expect("the staled acquisition joins")
        .expect_err("a view staled inside the production window must not register");
    assert!(
        matches!(err, SensingRegistrationError::OrgAudienceUnsupported),
        "expected the fail-closed org disposition from the currentness fence, got {err:?}"
    );
    assert_eq!(
        SensingCounters::get(&a.sensing_counters().org_stale_stamp),
        stale_stamp_before + 1,
        "the fence — not some earlier refusal — is what rejected this acquisition"
    );
    a.clear_sensing_phase_two_seam_for_test();

    // The refusal must be SILENT. Leave a window wide enough that any frame that
    // was in fact authored has long since crossed loopback and been applied.
    tokio::time::sleep(SETTLE).await;

    let (interval, root) = peer_row(&b, &key, a_id).expect(
        "the pre-existing row must still be there — a refused acquisition tears nothing down",
    );
    assert_eq!(
        interval, park_interval,
        "the provider row carries the PARKED transition's cadence and never the \
         refused one's. WHAT BREAKS THIS: authoring/sending the organization \
         registration BEFORE the currentness fence rules on it (the row would read \
         {STRICT:?}); or leaving the stale acquisition's Phase-2 emission outside \
         `org_transition_mu`'s total order, so the refused transition's bytes get \
         REORDERED out behind the live ones"
    );
    assert_ne!(
        interval, STRICT,
        "the refused acquisition's cadence reached the provider — it must contribute \
         nothing at all"
    );
    assert_eq!(
        root, commitment,
        "and it is still under the canonical ORGANIZATION commitment. WHAT BREAKS \
         THIS: the refused acquisition falling back to legacy authoring, whose frame \
         would re-key the provider row under A's legacy entity root"
    );
    assert_eq!(
        SensingCounters::get(&b.sensing_counters().protocol_invalid),
        protocol_invalid_before,
        "and B saw NOTHING to refuse. WHAT BREAKS THIS: the stale acquisition \
         emitting anything at all — an org frame authored under the now-superseded \
         view, or legacy bytes carrying the org audience — either of which B counts \
         here"
    );

    a.release_sensing_interest_lease(parked_ticket)
        .expect("the release must not be refused");
    a.release_sensing_interest_lease(loose)
        .expect("the release must not be refused");
    a.shutdown().await.expect("shutdown A");
    b.shutdown().await.expect("shutdown B");
}

// ---- PEER-OBSERVABLE ORDERING (tests 5 and 6) ------------------------------

/// A one-shot Phase-2 park.
///
/// `entered` flips when the first EMITTING transition reaches Phase 2. At that
/// point the transition holds `org_transition_mu`, every sensing guard is
/// released, and — the property both ordering witnesses are built on — NOTHING
/// has been handed to the transport yet: the seam fires ABOVE
/// `emit_pending_org_send` and the `pending.deregistrations` loop. It stays
/// parked until `unpark` is set, so a rival transition can be started in
/// exactly the committed-but-not-yet-sent window.
///
/// One-shot, because only the FIRST decision must park: the rival's own Phase 2
/// and every release at the end of the test have to run straight through.
struct Phase2Park {
    entered: Arc<AtomicBool>,
    unpark: Arc<AtomicBool>,
}

fn arm_phase_two_park(a: &Arc<MeshNode>) -> Phase2Park {
    let entered = Arc::new(AtomicBool::new(false));
    let unpark = Arc::new(AtomicBool::new(false));
    let armed = Arc::new(AtomicBool::new(true));
    {
        let entered = entered.clone();
        let unpark = unpark.clone();
        a.set_sensing_phase_two_seam_for_test(Arc::new(move || {
            if !armed.swap(false, Ordering::SeqCst) {
                return;
            }
            entered.store(true, Ordering::SeqCst);
            while !unpark.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(2));
            }
        }));
    }
    Phase2Park { entered, unpark }
}

/// Unpark the parked decision and let BOTH contending transitions hand their
/// datagrams to the transport with the runtime thread deliberately BLOCKED,
/// then report the ordered egress' depth.
///
/// Why this is deterministic, and why the tests below are single-threaded
/// (`#[tokio::test]`, i.e. one worker): the two transitions run on
/// `spawn_blocking` threads and their whole commit-author-encode-hand-off path
/// is synchronous, so they both complete inside this window without needing a
/// runtime worker. The ordered egress' consumer, by contrast, IS a runtime
/// task, so it cannot be polled while this function sits in
/// `std::thread::sleep`. The window therefore ends with the pair of datagrams
/// QUEUED, in decision order, in one FIFO, and none of them sent.
///
/// A returned depth of 0 means the datagrams never entered the ordered egress
/// at all — i.e. the racing per-datagram `tokio::spawn` this repair replaced,
/// under which the peer-observable order is whatever the scheduler picks.
fn unpark_and_measure_queue(a: &Arc<MeshNode>, park: &Phase2Park) -> u64 {
    park.unpark.store(true, Ordering::SeqCst);
    std::thread::sleep(HANDOFF);
    a.org_egress_state_for_test().depth
}

/// TEST 5 — CADENCE ORDERING under real contention, observed from the PROVIDER.
///
/// Two cadence decisions for ONE interest are made back to back, and the first
/// one is stopped between its commit and its transport effect:
///
/// 1. a loose baseline row is established at [`D`];
/// 2. decision A tightens `D` -> [`MID`] and is PARKED inside Phase 2, holding
///    `org_transition_mu`, with nothing yet handed to the transport;
/// 3. decision B — the LATER decision — tightens further, `MID` -> [`STRICT`],
///    while A is parked. It blocks on the transition order, which is asserted;
/// 4. A is released, both hand off, and the provider's FINAL row must read
///    [`STRICT`].
///
/// The direction is deliberately "tighten, then tighten further" so the
/// expected value is unambiguous and every failure mode reads differently:
/// [`STRICT`] is the only correct final cadence, [`MID`] means A's datagram
/// landed LAST (an inversion of decision order), and [`D`] means neither
/// decision reached the wire at all.
///
/// This is the case the pre-repair egress got wrong. `spawn_sensing_frame_send`
/// built the packet synchronously — so the stream SEQUENCE was in call order —
/// and then `tokio::spawn`ed the `send_to`. Two independent tasks race, the
/// receiver applies sensing interest frames in ARRIVAL order and never
/// reorders or rejects by sequence, and this slice has no ttl/2 refresh owner
/// to repair a stale final state. So the peer's FINAL row could be the OLDER
/// decision's, permanently.
///
/// WHICH ASSERTION DISCRIMINATES, MEASURED rather than assumed. The two
/// production `self.org_egress().enqueue(packet, addr)` sites were replaced
/// with the pre-repair racing `tokio::spawn(async move { socket.send_to(..) })`
/// in a scratch copy of this crate and this witness was re-run:
///
/// * the ORDERED-QUEUE assertion (`queued >= 2`) goes red DETERMINISTICALLY,
///   with `saw depth 0` — the racing egress never touches the queue at all;
/// * the ARRIVAL-ORDER assertion below did NOT invert: with the queue
///   assertion neutralised, 20 consecutive runs stayed green (10 on this
///   single-worker runtime, 10 on a 4-worker one). Loopback plus tokio's FIFO
///   spawn order simply do not reorder two sends separated by a mutex
///   hand-off, a frame authoring and an encode.
///
/// So the arrival-order assertion states the PROPERTY — and would catch a real
/// inversion, which is what the peer ultimately observes — while the
/// ordered-queue assertion is what actually discriminates against the racing
/// egress on a loopback host. Both are kept, and neither is described as doing
/// the other's job.
#[tokio::test]
async fn the_later_org_cadence_decision_is_what_the_provider_finally_holds() {
    let OrgPair { a, b, .. } = org_pair("order-cadence").await;

    let a_id = a.node_id();
    let b_id = b.node_id();
    let spec = org_spec(b_id);
    let key = ProviderInterestKey::new(spec.key(), b_id);
    let commitment = canonical_org_sensing_commitment(&org().org_id());
    let protocol_invalid_before = SensingCounters::get(&b.sensing_counters().protocol_invalid);
    assert!(
        STRICT < MID && MID < D,
        "the three cadences must be strictly ordered, or the final row cannot \
         name the winning decision"
    );

    // The baseline the two contending decisions both move away from.
    let refresher = spawn_refresher(&a, &spec, b_id, D);
    let loose = a
        .acquire_sensing_interest_lease(&spec, b_id, D)
        .expect("the loose own-org holder must acquire");
    assert!(
        poll_until(POLL, || peer_row(&b, &key, a_id).map(|(d, _)| d) == Some(D)).await,
        "the provider never installed the baseline row at {D:?}"
    );

    // The refresher is aborted AND JOINED before the park is armed, and both
    // are load-bearing. Aborted: a re-drive would author frames this test
    // attributes to the two contending decisions. Joined: the refresher is a
    // runtime TASK that calls the synchronous lease API, so if it were still
    // alive when A parks it would block this single-threaded runtime's only
    // worker on `org_transition_mu` — and the worker is what has to run the
    // unpark. Cancellation is safe: the task's only await is its sleep, so it
    // can never be cancelled between an acquire and its release.
    refresher.abort();
    let _ = refresher.await;
    tokio::time::sleep(PAST_MIN_GAP).await;

    // The SEND-BOUNDARY trace for the contending pair. The queue-depth
    // assertions below prove the datagrams entered the ordered egress; this
    // proves the consumer then sent them one at a time, in enqueue order — the
    // discrimination a depth assertion structurally cannot make.
    let trace = arm_send_trace(&a);

    // DECISION A: tighten to MID, and stop it between commit and transport.
    let park = arm_phase_two_park(&a);
    let first = {
        let a = a.clone();
        let spec = spec.clone();
        tokio::task::spawn_blocking(move || a.acquire_sensing_interest_lease(&spec, b_id, MID))
    };
    await_condition(POLL, "the tightening decision parked in Phase 2", || {
        park.entered.load(Ordering::SeqCst)
    })
    .await;
    assert_eq!(
        a.org_egress_state_for_test().depth,
        0,
        "the park must sit BEFORE the parked decision's transport effect — that is \
         the entire premise of this witness. A non-zero depth here means the \
         datagram was already handed to the transport, so nothing below could be \
         proving anything about ordering"
    );

    // DECISION B: the LATER decision, taken while A is parked.
    let second = {
        let a = a.clone();
        let spec = spec.clone();
        tokio::task::spawn_blocking(move || a.acquire_sensing_interest_lease(&spec, b_id, STRICT))
    };
    tokio::time::sleep(SETTLE).await;
    assert!(
        !second.is_finished(),
        "the later decision COMPLETED while the earlier one was still parked inside \
         Phase 2. Both transitions are on the organization plane, so both must hold \
         `org_transition_mu` across commit, mutation and emission; if B can commit \
         while A holds it, decision order is not a total order for this key and \
         nothing downstream can be ordered by it"
    );
    assert_eq!(
        peer_row(&b, &key, a_id).map(|(d, _)| d),
        Some(D),
        "the provider row MOVED while both decisions were still un-emitted. Neither \
         contender may reach the wire before the parked one is released"
    );

    // Unpark. Both decisions hand off with the runtime thread blocked, so the
    // pair is observable as queued-and-unsent.
    let queued = unpark_and_measure_queue(&a, &park);
    assert!(
        queued >= 2,
        "expected BOTH contending datagrams queued on the ORDERED egress and none \
         of them sent, saw depth {queued}. A depth of 0 means the datagrams were \
         not handed to the ordered egress at all — the racing per-datagram \
         `tokio::spawn` this repair replaced, whose peer-observable order is \
         whatever the scheduler picks rather than the order the decisions committed"
    );

    let mid_ticket = first
        .await
        .expect("the parked acquisition joins")
        .expect("the parked tightening must acquire");
    let strict_ticket = second
        .await
        .expect("the later acquisition joins")
        .expect("the later tightening must acquire");
    a.clear_sensing_phase_two_seam_for_test();
    assert!(
        poll_until(POLL, || a.org_egress_state_for_test().depth == 0).await,
        "the ordered egress never drained — its single sequential consumer is what \
         turns enqueue order into socket order, and it is not running"
    );
    // THE MECHANISM, at the transport boundary: the two contending datagrams
    // were handed to the socket one at a time, in the order their decisions
    // committed. This is what fails for a mutant that keeps the ordered queue
    // and races the dequeued sends.
    assert_strictly_serial(&trace, 2);

    assert!(
        poll_until(POLL, || peer_row(&b, &key, a_id).map(|(d, _)| d)
            == Some(STRICT))
        .await,
        "the provider never reached the LATER decision's cadence {STRICT:?}"
    );
    // The settle-and-reread is the actual ordering assertion. If the two
    // datagrams inverted, {STRICT:?} still arrives — first — and the poll above
    // passes; it is A's later-landing {MID:?} that then becomes the final word.
    tokio::time::sleep(SETTLE).await;
    let (interval, root) = peer_row(&b, &key, a_id).expect("row present");
    assert_eq!(
        interval, STRICT,
        "the provider's FINAL row is not the LATER decision's cadence {STRICT:?}. \
         WHAT BREAKS THIS: any inversion of the two datagrams on the path from \
         decision to peer — the receiver applies interest frames in arrival order, \
         never reorders or rejects by sequence, and this slice has no ttl/2 refresh \
         owner, so whichever frame lands last is the peer's PERMANENT state and the \
         parked decision's {MID:?} wins forever. NOTE, measured: on a loopback host \
         a racing per-datagram `tokio::spawn` does not actually invert this pair — \
         the ordered-queue assertion above is what discriminates against that \
         egress. This assertion is the property itself, not its proxy"
    );
    assert_ne!(
        interval, MID,
        "the PARKED (older) decision's cadence {MID:?} is the provider's last \
         word — decision order inverted on the wire"
    );
    assert_eq!(
        root, commitment,
        "and the winning row is still keyed under the canonical ORGANIZATION \
         commitment, not A's legacy entity root: ordering the egress must not have \
         changed which authority authored the bytes"
    );
    assert_eq!(
        SensingCounters::get(&b.sensing_counters().protocol_invalid),
        protocol_invalid_before,
        "and B admitted every frame in this contention without one protocol-invalid \
         refusal — the ordering was achieved by ordering the sends, not by the peer \
         throwing bytes away"
    );

    a.release_sensing_interest_lease(strict_ticket)
        .expect("the release must not be refused");
    a.release_sensing_interest_lease(mid_ticket)
        .expect("the release must not be refused");
    a.release_sensing_interest_lease(loose)
        .expect("the release must not be refused");
    a.shutdown().await.expect("shutdown A");
    b.shutdown().await.expect("shutdown B");
}

/// TEST 6 — TEARDOWN ordering: no stale resurrection of a removed row.
///
/// The teardown boundary is where an inverted egress is not merely stale but
/// WRONG in kind, and it is the case the pre-repair design got wrong:
///
/// 1. a row is established at [`D`];
/// 2. the FINAL release is PARKED inside Phase 2 — the registry reference is
///    already committed and the local row already gone, but the `Deregister`
///    has not been handed to the transport;
/// 3. a re-acquisition at [`FRESH`] is taken while the teardown is parked, and
///    must block on the transition order, which is asserted;
/// 4. the teardown is released. Decision order is teardown-then-reacquire, so
///    the provider must end holding the re-acquisition's FRESH row.
///
/// If the two datagrams invert, the `Register` lands first and the `Deregister`
/// lands last, so the peer removes the row the LIVE lease is holding: the
/// consumer believes it has a registration, the provider has none, and nothing
/// in this slice ever repairs it. Symmetrically, the pre-repair egress could
/// let a stale `Register` land after a `Deregister` and RESURRECT a row that
/// was deliberately removed, which is why the last section of this test proves
/// that a final teardown with nothing following it leaves no row at all.
///
/// Sensitivity, MEASURED the same way as test 5 (see its doc comment): with
/// both production `enqueue` sites replaced by the pre-repair racing
/// `tokio::spawn`, this witness goes red DETERMINISTICALLY on the
/// ordered-queue assertion with `saw depth 0`; with that assertion neutralised,
/// the arrival-order assertions stayed green for 20 consecutive runs, because
/// loopback plus tokio's FIFO spawn order do not invert a pair of sends
/// separated by a mutex hand-off. The queue assertion is the discriminator on
/// this host; the row assertions are the property.
#[tokio::test]
async fn a_parked_org_teardown_cannot_be_overtaken_by_its_own_reacquisition() {
    let OrgPair { a, b, .. } = org_pair("order-teardown").await;

    let a_id = a.node_id();
    let b_id = b.node_id();
    let spec = org_spec(b_id);
    let key = ProviderInterestKey::new(spec.key(), b_id);
    let commitment = canonical_org_sensing_commitment(&org().org_id());
    let protocol_invalid_before = SensingCounters::get(&b.sensing_counters().protocol_invalid);
    assert_ne!(
        FRESH, D,
        "the re-acquisition's cadence must differ from the torn-down row's, or a \
         resurrected stale row and a fresh one would be indistinguishable"
    );

    let refresher = spawn_refresher(&a, &spec, b_id, D);
    let only = a
        .acquire_sensing_interest_lease(&spec, b_id, D)
        .expect("the only own-org holder must acquire");
    assert!(
        poll_until(POLL, || peer_row(&b, &key, a_id).map(|(d, _)| d) == Some(D)).await,
        "the provider never installed the row at {D:?}, so there is nothing whose \
         teardown could be ordered"
    );

    // Aborted AND joined before the park is armed. Aborted: a re-drive would
    // re-register the row underneath the teardown and the absence assertions
    // would be untestable. Joined: a live refresher task would block this
    // single-threaded runtime's only worker on `org_transition_mu` while the
    // teardown holds it, and that worker is what runs the unpark. Cancellation
    // cannot leak a holder: the task's only await is its sleep, so it is never
    // cancelled between an acquire and its release — which matters here,
    // because a leaked holder would make the release below not FINAL.
    refresher.abort();
    let _ = refresher.await;
    tokio::time::sleep(PAST_MIN_GAP).await;
    assert!(
        peer_row(&b, &key, a_id).is_some(),
        "the row must still be live at the moment of the final release"
    );

    // DECISION A: the FINAL release, stopped before its `Deregister` reaches
    // the transport.
    let park = arm_phase_two_park(&a);
    let teardown = {
        let a = a.clone();
        tokio::task::spawn_blocking(move || a.release_sensing_interest_lease(only))
    };
    await_condition(POLL, "the final teardown parked in Phase 2", || {
        park.entered.load(Ordering::SeqCst)
    })
    .await;
    assert_eq!(
        a.org_egress_state_for_test().depth,
        0,
        "the park must sit BEFORE the teardown's transport effect: the `Deregister` \
         may not have been handed to the transport yet, or the re-acquisition below \
         is not contending with anything"
    );

    // DECISION B: the re-acquisition, taken while the teardown is parked.
    let reacquire = {
        let a = a.clone();
        let spec = spec.clone();
        tokio::task::spawn_blocking(move || a.acquire_sensing_interest_lease(&spec, b_id, FRESH))
    };
    tokio::time::sleep(SETTLE).await;
    assert!(
        !reacquire.is_finished(),
        "the re-acquisition COMPLETED while the final teardown was still parked \
         inside Phase 2. The teardown holds `org_transition_mu` across commit, \
         mutation and emission precisely so a re-acquisition cannot be decided \
         inside it; without that, teardown and re-acquisition are not ordered \
         against each other at all"
    );
    assert_eq!(
        peer_row(&b, &key, a_id).map(|(d, _)| d),
        Some(D),
        "the provider row moved while the teardown was parked and the \
         re-acquisition undecided — the re-acquisition's registration reached the \
         wire AHEAD of the teardown it must follow"
    );

    let queued = unpark_and_measure_queue(&a, &park);
    assert!(
        queued >= 2,
        "expected the `Deregister` and the re-acquisition's `Register` BOTH queued \
         on the ORDERED egress and neither sent, saw depth {queued}. A depth of 0 \
         means they were handed to independent racing spawned sends instead, under \
         which the teardown can land AFTER the registration that superseded it and \
         silently delete a live lease's row"
    );

    teardown
        .await
        .expect("the parked teardown joins")
        .expect("a FINAL organization release can never be refused");
    let fresh_ticket = reacquire
        .await
        .expect("the re-acquisition joins")
        .expect("the re-acquisition must acquire");
    a.clear_sensing_phase_two_seam_for_test();
    assert!(
        poll_until(POLL, || a.org_egress_state_for_test().depth == 0).await,
        "the ordered egress never drained — its single sequential consumer is what \
         turns enqueue order into socket order, and it is not running"
    );

    assert!(
        poll_until(POLL, || peer_row(&b, &key, a_id).map(|(d, _)| d)
            == Some(FRESH))
        .await,
        "the provider never installed the re-acquisition's FRESH row at {FRESH:?}"
    );
    // As in test 5, the settle-and-reread is the ordering assertion: under an
    // inversion the `Register` arrives FIRST and the poll above passes, and it
    // is the late `Deregister` that has the last word and removes the row.
    tokio::time::sleep(SETTLE).await;
    let (interval, root) = peer_row(&b, &key, a_id).expect(
        "the provider has NO row for A after teardown-then-reacquire. The FINAL \
         state must be the re-acquisition's, because that decision committed LAST. \
         WHAT BREAKS THIS: any inversion of the `Deregister` and the `Register` \
         that follows it, which delivers the teardown last and deletes a row a LIVE \
         lease is holding — a state nothing in this slice ever repairs, since there \
         is no ttl/2 refresh owner. NOTE, measured: on a loopback host a racing \
         per-datagram `tokio::spawn` does not actually invert this pair; the \
         ordered-queue assertion above is what discriminates against that egress",
    );
    assert_eq!(
        interval, FRESH,
        "the provider's row is not the re-acquisition's FRESH row. {FRESH:?} is a \
         cadence only the re-acquisition ever requested, so a row reading anything \
         else is a RESURRECTED stale registration rather than the fresh one"
    );
    assert_ne!(
        interval, D,
        "the provider is holding the TORN-DOWN row's cadence {D:?} — a stale \
         registration was resurrected across the teardown"
    );
    assert_eq!(
        root, commitment,
        "and the fresh row is keyed under the canonical ORGANIZATION commitment, \
         not A's legacy entity root"
    );
    assert_eq!(
        SensingCounters::get(&b.sensing_counters().protocol_invalid),
        protocol_invalid_before,
        "and B admitted the teardown and the re-registration without one \
         protocol-invalid refusal: the `Deregister` is plane-independent and must \
         pass intake cleanly, so the counter distinguishes 'B applied both frames' \
         from 'B threw bytes away'"
    );

    // AND THE OTHER HALF: with NOTHING following it, a final teardown really
    // does leave the provider with no row. Without this, the assertions above
    // would be satisfied by a `Deregister` that never removes anything.
    a.release_sensing_interest_lease(fresh_ticket)
        .expect("a FINAL organization release can never be refused");
    assert!(
        poll_until(POLL, || b
            .sensing_downstream_entry(&key, DownstreamId::Peer(a_id))
            .is_none())
        .await,
        "the provider's row survived a final teardown that nothing followed. B's \
         soft-state ttl is 30 s and this poll is 5 s, so expiry cannot stand in for \
         the `Deregister` frame"
    );

    a.shutdown().await.expect("shutdown A");
    b.shutdown().await.expect("shutdown B");
}

// ---- THE TRANSPORT BOUNDARY (tests 7-9) ------------------------------------

/// A recorded trace of the PRODUCTION organization send boundary.
///
/// Each entry is one `(enqueue sequence, phase)` observation, taken from inside
/// the consumer immediately before and immediately after its own `send_to`. The
/// trace is what discriminates a genuinely sequential consumer from a mutant
/// that keeps the ordered queue and then races the dequeued sends: the queue is
/// touched identically by both, the trace is not.
type SendTrace = Arc<parking_lot::Mutex<Vec<(u64, OrgEgressSendPhase)>>>;

fn arm_send_trace(a: &Arc<MeshNode>) -> SendTrace {
    let trace: SendTrace = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let sink = trace.clone();
    a.set_org_egress_send_observer_for_test(Arc::new(move |seq, phase| {
        sink.lock().push((seq, phase));
    }));
    trace
}

/// [`arm_send_trace`] that also PARKS inside the FIRST datagram's send bracket.
///
/// This is what makes the racing-consumer mutant fail DETERMINISTICALLY rather
/// than probabilistically. The observer blocks for `park` between datagram 0's
/// `Started` and its `Completed`, and then records how many observations exist
/// at that moment:
///
/// * a genuinely SEQUENTIAL consumer is the only writer of the trace and it is
///   blocked, so exactly ONE observation can exist — its own `Started`;
/// * a consumer that dequeues and then `tokio::spawn`s the sends has other
///   tasks free to run on the other workers, so their observations land during
///   the park and the count exceeds one.
///
/// Requires more than one runtime worker, or the rival sends could not run even
/// under the mutant and the discrimination would be vacuous.
fn arm_parked_send_trace(
    a: &Arc<MeshNode>,
    park: Duration,
) -> (SendTrace, Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::atomic::AtomicUsize;

    let trace: SendTrace = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let during_park = Arc::new(AtomicUsize::new(0));
    let sink = trace.clone();
    let counted = during_park.clone();
    let armed = Arc::new(AtomicBool::new(true));
    a.set_org_egress_send_observer_for_test(Arc::new(move |seq, phase| {
        sink.lock().push((seq, phase));
        if phase != OrgEgressSendPhase::Started || !armed.swap(false, Ordering::SeqCst) {
            return;
        }
        std::thread::sleep(park);
        let observed = sink.lock().len();
        counted.store(observed, Ordering::SeqCst);
    }));
    (trace, during_park)
}

/// Assert a recorded trace is STRICTLY SERIAL in enqueue order.
///
/// The contract has exactly one legal shape: for every datagram, `Started` is
/// immediately followed by that same datagram's `Completed`, and sequences are
/// non-decreasing. Both semantic inverses fail here and nowhere else:
///
/// * PARALLELISED sends — two `Started` in a row, i.e. a second `send_to` began
///   before the first returned;
/// * REORDERED sends — a lower sequence starting after a higher one.
fn assert_strictly_serial(trace: &SendTrace, at_least: usize) {
    let observed = trace.lock().clone();
    assert!(
        observed.len() >= at_least * 2,
        "expected at least {at_least} bracketed sends at the production send \
         boundary, saw {} observations: {observed:?}. An EMPTY trace means the \
         datagrams never went through the ordered consumer at all — the racing \
         per-datagram `tokio::spawn` this repair replaced",
        observed.len()
    );
    let mut previous_seq: Option<u64> = None;
    let mut index = 0;
    while index < observed.len() {
        let (seq, phase) = observed[index];
        assert_eq!(
            phase,
            OrgEgressSendPhase::Started,
            "observation {index} of the send trace is {phase:?} for datagram \
             {seq} where a `Started` was due. Two `Started`s in a row means the \
             consumer began a second socket send before the first returned — the \
             ordered queue was kept and the SENDS were parallelised, which is \
             exactly the mutant a queue-depth assertion cannot see. Trace: \
             {observed:?}"
        );
        let Some(&(completed_seq, completed_phase)) = observed.get(index + 1) else {
            panic!(
                "datagram {seq} started at the send boundary and never completed; \
                 the trace ends mid-send: {observed:?}"
            );
        };
        assert_eq!(
            (completed_seq, completed_phase),
            (seq, OrgEgressSendPhase::Completed),
            "datagram {seq}'s `Started` is followed by {completed_phase:?} for \
             datagram {completed_seq}, not by its own completion — the consumer \
             interleaved two socket sends. Trace: {observed:?}"
        );
        if let Some(previous) = previous_seq {
            assert!(
                seq > previous,
                "datagram {seq} was sent AFTER datagram {previous}, so the \
                 consumer dequeued out of enqueue order. Enqueue order is \
                 decision order (producers enqueue under `org_transition_mu`), so \
                 this is a peer-observable inversion of two committed decisions. \
                 Trace: {observed:?}"
            );
        }
        previous_seq = Some(seq);
        index += 2;
    }
}

/// TEST 7 — the SEND BOUNDARY is strictly serial and in enqueue order.
///
/// Tests 5 and 6 prove the datagrams enter the ordered egress and that the
/// provider's final row is the later decision's. Neither can fail for a mutant
/// that keeps the queue and then fans the dequeued datagrams into concurrent
/// `tokio::spawn`ed sends: the queue depth is identical, and on loopback the
/// arrival order does not actually invert (measured, 20/20 — see test 5).
///
/// This witness observes the production `send_to` itself. Several distinct
/// interests are registered back to back so the consumer has a real run of
/// datagrams to order, and the recorded trace must be
/// `Started(0) Completed(0) Started(1) Completed(1) ...`. A parallelising or
/// reordering consumer cannot produce that, and an egress bypass produces an
/// empty trace.
///
/// MECHANISM PROOF, not a UDP outcome: it states what this node's socket calls
/// did, in order. What the peer finally holds is asserted by tests 5 and 6, and
/// a network reordering in flight remains outside any send-side mechanism.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_production_send_boundary_is_strictly_serial_in_enqueue_order() {
    let OrgPair { a, b, .. } = org_pair("send-order").await;
    let b_id = b.node_id();
    // PARKED inside the first send's bracket: see `arm_parked_send_trace` for
    // why the park is what makes the racing-consumer mutant deterministic.
    let (trace, during_park) = arm_parked_send_trace(&a, HANDOFF);

    // DISTINCT interests, so the upstream min-gap damper admits every one of
    // them: it is keyed by `(provider, interest_digest)`, so the same interest
    // registered twice inside 100 ms would emit only once.
    const SENDS: usize = 6;
    let mut tickets = Vec::with_capacity(SENDS);
    for index in 0..SENDS {
        let spec = distinct_org_spec(b_id, index);
        tickets.push(
            a.acquire_sensing_interest_lease(&spec, b_id, D)
                .expect("each distinct own-org interest must acquire"),
        );
    }
    assert!(
        poll_until(POLL, || a.org_egress_state_for_test().sent >= SENDS as u64).await,
        "the ordered consumer never handed all {SENDS} datagrams to the socket; \
         saw {:?}",
        a.org_egress_state_for_test()
    );
    assert_eq!(
        during_park.load(Ordering::SeqCst),
        1,
        "while datagram 0 was parked INSIDE its own send bracket, {} send-boundary \
         observations existed. A sequential consumer is the trace's only writer \
         and it was blocked, so exactly one — its own `Started` — is possible. \
         More means other sends were in flight concurrently: the ordered queue \
         was kept and the SOCKET SENDS were parallelised, which is precisely the \
         mutant a queue-depth assertion cannot see. Trace: {:?}",
        during_park.load(Ordering::SeqCst),
        trace.lock().clone()
    );
    assert_strictly_serial(&trace, SENDS);
    assert_eq!(
        a.org_egress_state_for_test().dropped_oldest,
        0,
        "nothing may be evicted well below the bound"
    );

    for ticket in tickets {
        a.release_sensing_interest_lease(ticket)
            .expect("the release must not be refused");
    }
    a.shutdown().await.expect("shutdown A");
    b.shutdown().await.expect("shutdown B");
}

/// TEST 8 — the ordered egress is BOUNDED under a consumer that cannot run, and
/// the LATEST committed decision survives the overflow.
///
/// The pre-repair egress was an UNBOUNDED channel. A bounded lease registry does
/// not bound transition RATE, so one stalled `send_to` let the queue grow
/// without limit. Producers cannot be blocked instead — the lease API is
/// synchronous and enqueues under `org_transition_mu`, so blocking would stall
/// every organization transition on the socket.
///
/// The bound therefore evicts the OLDEST pending datagram, which is the only
/// eviction policy that keeps the property the egress exists for: the newest
/// datagram is never the casualty. This drives a burst strictly larger than the
/// bound with the runtime's only worker deliberately BLOCKED — so the consumer
/// provably cannot retire anything — and then asserts:
///
/// 1. depth never exceeds the bound;
/// 2. the overflow was counted as an eviction, not silently absorbed;
/// 3. the LAST interest of the burst still reaches the provider.
///
/// Single-threaded (`#[tokio::test]`, one worker) is load-bearing: the burst
/// runs on a blocking thread and is fully synchronous, while the consumer is a
/// runtime TASK, so blocking the one worker is what makes "the consumer cannot
/// run" a fact rather than a hope.
#[tokio::test]
async fn the_ordered_egress_is_bounded_and_keeps_the_latest_decision() {
    use std::sync::mpsc;

    let OrgPair { a, b, .. } = org_pair("bounded").await;
    let a_id = a.node_id();
    let b_id = b.node_id();

    // WARM-UP, on this task: the egress is created lazily and its consumer is
    // `tokio::spawn`ed, so it must first come into existence inside the runtime.
    // Drain it, then take the send baseline.
    let warm_spec = org_spec(b_id);
    let warm = a
        .acquire_sensing_interest_lease(&warm_spec, b_id, D)
        .expect("the warm-up own-org holder must acquire");
    assert!(
        poll_until(POLL, || {
            let state = a.org_egress_state_for_test();
            state.started && state.depth == 0 && state.sent >= 1
        })
        .await,
        "the egress never came up and drained: {:?}",
        a.org_egress_state_for_test()
    );
    let sent_before = a.org_egress_state_for_test().sent;

    // Strictly more than the production bound, so eviction MUST happen. The
    // bound is not exported; the burst is sized against it by construction and
    // the accounting assertion below states the relationship it needs.
    const BURST: usize = 140;
    let last_spec = distinct_org_spec(b_id, BURST - 1);
    let last_key = ProviderInterestKey::new(last_spec.key(), b_id);

    let (done_tx, done_rx) = mpsc::sync_channel::<Vec<_>>(1);
    let burst = {
        let a = a.clone();
        // `spawn_blocking`, not a bare `std::thread`: the whole acquisition path
        // is synchronous but it still needs the runtime CONTEXT, exactly as the
        // pre-existing legacy `spawn_sensing_frame_send` does on this same chain.
        tokio::task::spawn_blocking(move || {
            let mut tickets = Vec::with_capacity(BURST);
            for index in 0..BURST {
                let spec = distinct_org_spec(b_id, index);
                if let Ok(ticket) = a.acquire_sensing_interest_lease(&spec, b_id, D) {
                    tickets.push(ticket);
                }
            }
            let _ = done_tx.send(tickets);
        })
    };
    // BLOCK the single runtime worker until the burst is done. `recv` on a std
    // channel blocks this thread exactly like the `std::thread::sleep` the
    // ordering witnesses use, so the consumer TASK cannot be polled while the
    // blocking-pool thread above runs.
    let tickets = done_rx
        .recv_timeout(Duration::from_secs(60))
        .expect("the synchronous burst must complete without the runtime worker");
    let state = a.org_egress_state_for_test();

    assert_eq!(
        state.sent, sent_before,
        "the consumer retired datagrams while the only runtime worker was \
         blocked, so this run does not observe a stalled egress at all: {state:?}"
    );
    assert!(
        state.depth <= 128,
        "the ordered egress grew to {} pending datagrams. It is supposed to be \
         BOUNDED: an unbounded queue behind one stalled `send_to` is unbounded \
         memory growth driven by transition rate, which the bounded lease \
         registry does not limit. State: {state:?}",
        state.depth
    );
    assert!(
        state.dropped_oldest > 0,
        "a burst of {BURST} datagrams past the bound evicted NOTHING ({state:?}). \
         Either the queue is not bounded, or the overflow was absorbed without \
         being counted — and an uncounted drop is exactly the silent loss the \
         eviction counter exists to expose"
    );
    assert_eq!(
        state.depth + state.dropped_oldest,
        BURST as u64,
        "every enqueued datagram must be accounted for as pending or evicted: \
         {state:?}"
    );

    // THE SEMANTIC PROPERTY: the newest decision is never the casualty. The
    // burst's LAST interest was enqueued last, so it must still be in the queue
    // and must still reach the provider.
    assert!(
        poll_until(POLL, || peer_row(&b, &last_key, a_id).is_some()).await,
        "the LAST decision of an overflowing burst never reached the provider. \
         Overflow must evict the OLDEST pending datagram: dropping the newest \
         would make the peer's final row an older decision, which is the entire \
         defect the ordered egress exists to fix"
    );

    burst.await.expect("the burst joins");
    for ticket in tickets {
        let _ = a.release_sensing_interest_lease(ticket);
    }
    let _ = a.release_sensing_interest_lease(warm);
    a.shutdown().await.expect("shutdown A");
    b.shutdown().await.expect("shutdown B");
}

/// TEST 9 — the egress consumer does NOT survive node shutdown.
///
/// The pre-repair egress discarded its `JoinHandle` and `MeshNode::shutdown` did
/// not close, drain or join it: the consumer task simply outlived the node,
/// holding an `Arc<NetSocket>` and its queue.
///
/// The contract asserted here is the one the node actually implements: shutdown
/// CLOSES the queue and JOINS the consumer within a bounded grace window, so
/// after `shutdown().await` the task has exited. It is bounded on purpose — a
/// stalled socket must not stall shutdown — and the timeout path aborts.
#[tokio::test]
async fn the_egress_consumer_does_not_survive_node_shutdown() {
    let OrgPair { a, b, .. } = org_pair("shutdown").await;
    let b_id = b.node_id();
    let spec = org_spec(b_id);

    let ticket = a
        .acquire_sensing_interest_lease(&spec, b_id, D)
        .expect("the own-org holder must acquire");
    let before = a.org_egress_state_for_test();
    assert!(
        before.started,
        "the egress must exist before shutdown, or this witness is vacuous: \
         {before:?}"
    );
    assert!(
        !before.consumer_finished,
        "the consumer must still be alive before shutdown: {before:?}"
    );

    a.release_sensing_interest_lease(ticket)
        .expect("the release must not be refused");
    a.shutdown().await.expect("shutdown A");

    let after = a.org_egress_state_for_test();
    assert!(
        after.consumer_finished,
        "the ordered egress' consumer task OUTLIVED the node. `shutdown` must \
         close the queue and join the consumer; a discarded `JoinHandle` leaves a \
         task holding the socket and its queue after the node is gone. State: \
         {after:?}"
    );
    assert_eq!(
        after.depth, 0,
        "and shutdown must leave nothing queued behind it: {after:?}"
    );

    b.shutdown().await.expect("shutdown B");
}
