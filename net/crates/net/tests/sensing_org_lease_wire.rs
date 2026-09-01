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
//! The four tests below close the transition cycle over real UDP, not just its
//! opening move. Each one is a bounded, single-send transition observed from the
//! PROVIDER side:
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
//!
//! Topology is two nodes, one organization, no chaos injection, single send per
//! transition with a soft-state refresh loop for UDP best-effort. Every test
//! that observes a RELAXATION, a REMOVAL or an ABSENCE of output aborts the
//! refresher first — otherwise the re-drive would either mask the transition or
//! reinstall the row underneath the assertion.
//!
//! Run: `cargo test --features net --test sensing_org_lease_wire`

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
    EntityKeypair, MeshNode, MeshNodeConfig, SensingRegistrationError, SocketBufferConfig,
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
