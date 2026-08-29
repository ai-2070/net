//! Rust SDK integration witnesses for the S1 provider slice
//! (`docs/internal/plans/CAPABILITY_SENSING_SDK_INTEGRATION_PLAN.md`
//! §4.4, §4.5, §6 witnesses 14–16).
//!
//! Exercises `sdk/src/sensing.rs` against real `Mesh`es: the loud
//! configuration refusals, ownership-safe `provide` / `close` / drop,
//! state-edge notification through the live observation path, the
//! publication fence that stops a superseded evaluation from becoming
//! the latest observation, and the terminal registration-identity
//! exhaustion state.
//!
//! There are deliberately NO projection witnesses: this slice ships no
//! projection. See the module docs for why exact-provider acquisition
//! and projection are deferred to S4.
//!
//! Every critical case here is an INVERSE witness — it fails against a
//! specific wrong implementation, named in the test's doc comment.
//!
//! Most tests use the SELF-PROVIDER path: one node registers an
//! exact-provider interest in itself, so it is both origin and
//! consumer. That keeps the whole attestation + continuity pipeline live
//! (`MeshNode` feeds its own emitter with no wire hop) while staying
//! deterministic — no second node, no delivery scheduling, and no
//! fleet-root configuration the SDK deliberately does not expose.
//!
//! The fence witnesses need the emitter to sit inside user code while
//! the test moves ownership, so they run on a multi-thread runtime: the
//! blocking evaluator parks a worker thread by design.
//!
//! This binary is in the `retries = 0` override in
//! `net/crates/net/.config/nextest.toml`. These are race and timing
//! proofs; a retry could only turn a real defect green.

#![cfg(feature = "net")]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::sensing::{
    CanonicalConstraints, DisclosureClass, InterestSpec, ProjectedReadiness, ProviderInterestKey,
    ProviderSelector, ResultMode, SensingCounters, WorkLatencyEnvelope,
};
use net_sdk::identity::Identity;
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::sensing::{
    CapabilityId, EvaluationRequest, Incarnation, ReadinessEvaluation, ReadinessEvaluator,
    SensingError,
};

const PSK: [u8; 32] = [0x5cu8; 32];
const CAPABILITY: &str = "gpu.infer";
const OTHER_CAPABILITY: &str = "print.document";
/// Sample interval for the self-provider stream, deliberately far
/// above the poll windows below.
///
/// Two emitter facts shape this. The promised cadence is
/// `max(D / 2, cadence_floor)`, and a state-edge poke can only pull a
/// beat forward to `last + cadence_floor` — so a D at or below twice
/// the 50 ms floor makes every poke a no-op by construction. And a
/// cadence anywhere near [`POLL`] would let ordinary beats deliver a
/// status flip that only the state edge should be able to deliver,
/// which would make the state-edge witness vacuous.
///
/// 20 s promises a 10 s cadence: the FIRST beat is still immediate (so
/// every observation below establishes at once), the second is far
/// outside every poll window, and an edge still pulls a beat to within
/// the 50 ms floor.
const D: Duration = Duration::from_secs(20);
const TTL: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_secs(5);
/// How long a "this must NOT happen" assertion waits before concluding
/// the thing genuinely did not happen. Only ever used after a positive
/// signal proves the code under test already reached the decision point.
const SETTLE: Duration = Duration::from_millis(600);

/// Reports whatever its backing flag currently says, and counts calls
/// so a test can prove the evaluator was re-run rather than a cached
/// verdict replayed.
struct FlagEvaluator {
    ready: Arc<AtomicBool>,
    evaluations: Arc<AtomicU64>,
}

impl FlagEvaluator {
    fn pair() -> (Arc<AtomicBool>, Arc<AtomicU64>, Arc<Self>) {
        let ready = Arc::new(AtomicBool::new(true));
        let evaluations = Arc::new(AtomicU64::new(0));
        let evaluator = Arc::new(Self {
            ready: ready.clone(),
            evaluations: evaluations.clone(),
        });
        (ready, evaluations, evaluator)
    }
}

impl ReadinessEvaluator for FlagEvaluator {
    fn evaluate(&self, _request: &EvaluationRequest<'_>) -> ReadinessEvaluation {
        self.evaluations.fetch_add(1, Ordering::Relaxed);
        if self.ready.load(Ordering::Relaxed) {
            ReadinessEvaluation::Ready {
                estimated_start: Some(Duration::from_millis(3)),
            }
        } else {
            ReadinessEvaluation::NotReady { reason: 7 }
        }
    }
}

/// A distinguishable second integration, so a replacement is observable
/// through the readiness projection alone.
struct AlwaysNotReady;

impl ReadinessEvaluator for AlwaysNotReady {
    fn evaluate(&self, _request: &EvaluationRequest<'_>) -> ReadinessEvaluation {
        ReadinessEvaluation::NotReady { reason: 99 }
    }
}

/// Parks inside `evaluate` until released, so a test can hold a
/// readiness evaluation IN FLIGHT while it moves ownership out from
/// under it.
///
/// Spins rather than blocking on a channel so it works on any runtime
/// flavor; the tests that use it request extra worker threads because
/// this deliberately occupies one.
struct BlockingEvaluator {
    entered: Arc<AtomicBool>,
    release: Arc<AtomicBool>,
    returned: Arc<AtomicBool>,
}

struct BlockingHandles {
    entered: Arc<AtomicBool>,
    release: Arc<AtomicBool>,
    returned: Arc<AtomicBool>,
}

impl BlockingEvaluator {
    fn new() -> (BlockingHandles, Arc<Self>) {
        let entered = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let returned = Arc::new(AtomicBool::new(false));
        (
            BlockingHandles {
                entered: entered.clone(),
                release: release.clone(),
                returned: returned.clone(),
            },
            Arc::new(Self {
                entered,
                release,
                returned,
            }),
        )
    }
}

impl ReadinessEvaluator for BlockingEvaluator {
    fn evaluate(&self, _request: &EvaluationRequest<'_>) -> ReadinessEvaluation {
        self.entered.store(true, Ordering::Release);
        while !self.release.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        self.returned.store(true, Ordering::Release);
        // Ready is the dangerous verdict: if the fence leaks, a stale
        // Ready is what would wrongly become the latest observation.
        ReadinessEvaluation::Ready {
            estimated_start: Some(Duration::from_millis(1)),
        }
    }
}

async fn mesh_with(
    enable_sensing: bool,
    durable_identity: bool,
    incarnation: Option<Incarnation>,
) -> Mesh {
    let mut builder = MeshBuilder::new("127.0.0.1:0", &PSK).expect("bind addr");
    if enable_sensing {
        builder = builder.enable_sensing();
    }
    if durable_identity {
        builder = builder.identity(Identity::generate());
    }
    if let Some(incarnation) = incarnation {
        builder = builder.sensing_incarnation(incarnation);
    }
    builder.build().await.expect("build mesh")
}

/// A sensing-enabled ORIGIN: the plane is on, the identity is durable,
/// and a persisted epoch was supplied, so this node can sign readiness
/// for itself.
async fn origin_mesh() -> Mesh {
    mesh_with(true, true, Some(Incarnation::new(1))).await
}

/// An exactly addressed interest in `provider`, scoped to this node's
/// own audience. Built from core types on purpose: driving the
/// observation path is a TEST concern, and the SDK deliberately exposes
/// no interest vocabulary.
fn self_interest(mesh: &Mesh, capability: &str, provider: u64) -> InterestSpec {
    InterestSpec {
        capability_id: CapabilityId::new(capability),
        constraints: CanonicalConstraints::from_entries([("model", "llama-70b")])
            .expect("canonical constraints"),
        work_latency: WorkLatencyEnvelope::start_within(Duration::from_secs(2)),
        providers: ProviderSelector::Node(provider),
        result_mode: ResultMode::Any,
        disclosure_class: DisclosureClass::Owner,
        audience: mesh.inner().sensing_local_root(),
    }
}

async fn poll_until<F: FnMut() -> bool>(limit: Duration, mut check: F) -> bool {
    let deadline = std::time::Instant::now() + limit;
    loop {
        if check() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Start the self-provider observation path: the node registers an
/// exact interest in ITSELF, which feeds its own origin emitter.
/// Returns the branch key the projection is read under.
async fn watch_self(mesh: &Mesh, capability: &str) -> ProviderInterestKey {
    let own_id = mesh.inner().node_id();
    let spec = self_interest(mesh, capability, own_id);
    mesh.inner().start();
    mesh.inner()
        .register_sensing_interest(&spec, own_id, D, TTL)
        .expect("self interest registers");
    ProviderInterestKey::new(spec.key(), own_id)
}

fn attestations_emitted(mesh: &Mesh) -> u64 {
    SensingCounters::get(&mesh.inner().sensing_counters().attestations_emitted)
}

// ---------------------------------------------------------------------
// Loud configuration refusals (§4.5, §6 witness 14)
// ---------------------------------------------------------------------

/// **Inverse witness: a missing incarnation silently accepted.**
///
/// The origin role is fail-closed. An implementation that installed the
/// evaluator anyway would return `Ok` here and leave the caller
/// believing it is publishing readiness on a node that can never sign.
#[tokio::test]
async fn provide_without_a_persisted_incarnation_fails_loudly() {
    let mesh = mesh_with(true, true, None).await;
    assert!(
        !mesh.inner().sensing_origin_active(),
        "no incarnation must leave the origin role dark",
    );

    let (_ready, _evaluations, evaluator) = FlagEvaluator::pair();
    let client = mesh
        .sensing()
        .expect("sensing enabled with a durable identity");
    assert_eq!(
        client
            .provide(CapabilityId::new(CAPABILITY), evaluator)
            .err(),
        Some(SensingError::IncarnationRequired),
    );

    // Total refusal: nothing was installed, so a later correctly
    // configured provider is not locked out by a phantom incumbent.
    assert_eq!(
        mesh.inner().sensing_evaluator_count(),
        0,
        "the refused provide must have installed nothing",
    );
}

/// The same refusal reaches `provide_replacing`: supersession is not a
/// way around the fail-closed origin gate.
#[tokio::test]
async fn provide_replacing_without_an_incarnation_fails_loudly_too() {
    let mesh = mesh_with(true, true, None).await;
    let client = mesh.sensing().expect("sensing enabled");
    assert_eq!(
        client
            .provide_replacing(CapabilityId::new(CAPABILITY), Arc::new(AlwaysNotReady))
            .err(),
        Some(SensingError::IncarnationRequired),
    );
    assert_eq!(mesh.inner().sensing_evaluator_count(), 0);
}

/// The plane ships dark: no `SensingClient` at all until it is enabled.
#[tokio::test]
async fn the_sensing_surface_is_refused_while_the_plane_is_disabled() {
    let mesh = mesh_with(false, true, Some(Incarnation::new(1))).await;
    assert_eq!(mesh.sensing().err(), Some(SensingError::Disabled));
}

/// **Inverse witness: an ephemeral-identity node accepted as a
/// provider.**
///
/// A provider signs its attestations with the node's entity key. On a
/// generated keypair every restart changes that key, so the consumer's
/// trust-on-first-use pin breaks and the persisted incarnation — whose
/// entire purpose is making restarts orderable — is meaningless. Refused
/// at `sensing()`, before any registration can exist.
#[tokio::test]
async fn the_sensing_surface_is_refused_without_a_durable_node_identity() {
    let mesh = mesh_with(true, false, Some(Incarnation::new(1))).await;
    assert!(
        mesh.inner().sensing_origin_active(),
        "the plane and epoch are configured — identity is the only thing missing",
    );
    assert_eq!(
        mesh.sensing().err(),
        Some(SensingError::DurableIdentityRequired),
    );
}

// ---------------------------------------------------------------------
// Registration ownership (§6 witness 16)
// ---------------------------------------------------------------------

/// A second integration cannot silently take a served capability. The
/// incumbent keeps evaluating.
#[tokio::test]
async fn a_second_provide_for_one_capability_is_refused_and_the_incumbent_survives() {
    let mesh = origin_mesh().await;
    let client = mesh.sensing().expect("sensing enabled");
    let (_ready, evaluations, evaluator) = FlagEvaluator::pair();

    let incumbent = client
        .provide(CapabilityId::new(CAPABILITY), evaluator)
        .expect("first provide");

    assert_eq!(
        client
            .provide(CapabilityId::new(CAPABILITY), Arc::new(AlwaysNotReady))
            .err(),
        Some(SensingError::AlreadyProviding {
            capability: CAPABILITY.to_string(),
        }),
    );
    assert_eq!(mesh.inner().sensing_evaluator_count(), 1);

    // The incumbent — not the rejected rival — is what a beat runs.
    let branch = watch_self(&mesh, CAPABILITY).await;
    assert!(
        poll_until(POLL, || evaluations.load(Ordering::Relaxed) > 0).await,
        "the incumbent evaluator never ran",
    );
    assert_ne!(
        mesh.inner().sensing_projected(&branch),
        ProjectedReadiness::NotReady,
        "the refused rival's NotReady must never reach the projection",
    );

    assert!(incumbent.close());
}

/// **Inverse witness: unconditional unregister on old-handle drop.**
///
/// After an explicit replacement, dropping the superseded handle must
/// remove nothing. An implementation that removed by capability id
/// alone would evict the replacement here and leave the capability
/// unserved.
#[tokio::test]
async fn dropping_a_superseded_handle_cannot_remove_its_replacement() {
    let mesh = origin_mesh().await;
    let client = mesh.sensing().expect("sensing enabled");
    let (_ready, old_evaluations, old_evaluator) = FlagEvaluator::pair();

    let old = client
        .provide(CapabilityId::new(CAPABILITY), old_evaluator)
        .expect("first provide");
    let new = client
        .provide_replacing(CapabilityId::new(CAPABILITY), Arc::new(AlwaysNotReady))
        .expect("explicit replacement");

    // The stale handle's close reports that it removed nothing, and its
    // drop is likewise inert.
    assert!(
        !old.close(),
        "a superseded handle must not report a removal"
    );
    drop(old);
    assert_eq!(
        mesh.inner().sensing_evaluator_count(),
        1,
        "the replacement must still be installed",
    );

    // The replacement is still the one that answers beats: NotReady
    // (marker 99), and the superseded evaluator never runs again.
    let before = old_evaluations.load(Ordering::Relaxed);
    let branch = watch_self(&mesh, CAPABILITY).await;
    assert!(
        poll_until(POLL, || mesh.inner().sensing_projected(&branch)
            == ProjectedReadiness::NotReady)
        .await,
        "the replacement evaluator never reached the projection — \
         the superseded handle evicted it",
    );
    assert_eq!(
        old_evaluations.load(Ordering::Relaxed),
        before,
        "the superseded evaluator must never be consulted again",
    );

    assert!(new.close());
}

/// The current handle's close removes exactly its own registration, and
/// close/drop are idempotent: the removal is reported at most once.
#[tokio::test]
async fn close_removes_exactly_this_registration_and_is_idempotent() {
    let mesh = origin_mesh().await;
    let client = mesh.sensing().expect("sensing enabled");
    let (_ready, _evaluations, evaluator) = FlagEvaluator::pair();

    // A second capability's registration must be untouched by the
    // first's close.
    let other = client
        .provide(
            CapabilityId::new(OTHER_CAPABILITY),
            Arc::new(AlwaysNotReady),
        )
        .expect("other provide");
    let registration = client
        .provide(CapabilityId::new(CAPABILITY), evaluator)
        .expect("provide");
    assert_eq!(registration.capability().as_str(), CAPABILITY);

    assert_eq!(mesh.inner().sensing_evaluator_count(), 2);
    assert!(registration.close(), "the live handle performs the removal");
    assert!(!registration.close(), "a repeat close is inert");
    drop(registration);
    assert_eq!(
        mesh.inner().sensing_evaluator_count(),
        1,
        "close removed exactly one registration",
    );

    // The vacated slot admits a new registration; the sibling
    // capability was never disturbed.
    let successor = client
        .provide(CapabilityId::new(CAPABILITY), Arc::new(AlwaysNotReady))
        .expect("the vacated capability admits a successor");
    assert!(other.close(), "the sibling registration was still live");
    assert!(successor.close());
    assert_eq!(mesh.inner().sensing_evaluator_count(), 0);
}

// ---------------------------------------------------------------------
// The publication fence (H1)
// ---------------------------------------------------------------------

/// **Inverse witness: an in-flight evaluation publishing after its
/// registration was REPLACED.**
///
/// The evaluator is held inside `evaluate` with a `Ready` verdict
/// pending. While it is parked there, the replacement completes. When
/// the old evaluation is released, its `Ready` must not become the
/// latest observation — exactly one attestation may ever be published
/// for this branch, and it must be the successor's `NotReady`.
///
/// Fails against: signing and applying without revalidating ownership;
/// revalidating by capability rather than by registration id; testing
/// currentness and then publishing outside the section.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_evaluation_in_flight_cannot_publish_after_its_replacement_completes() {
    let mesh = origin_mesh().await;
    let client = mesh.sensing().expect("sensing enabled");
    let (blocking, evaluator) = BlockingEvaluator::new();

    let old = client
        .provide(CapabilityId::new(CAPABILITY), evaluator)
        .expect("provide");
    let branch = watch_self(&mesh, CAPABILITY).await;

    // The emitter is now parked inside the user evaluator.
    assert!(
        poll_until(POLL, || blocking.entered.load(Ordering::Acquire)).await,
        "the evaluator never ran, so nothing is in flight to fence",
    );
    assert_eq!(
        attestations_emitted(&mesh),
        0,
        "nothing may be published while the evaluation is still in flight",
    );

    // Ownership moves while that evaluation is parked. This must not
    // block on the evaluator: the registry's commit section is entered
    // only AFTER user code returns.
    let new = client
        .provide_replacing(CapabilityId::new(CAPABILITY), Arc::new(AlwaysNotReady))
        .expect("replacement completes while the old evaluation is in flight");
    assert!(!old.close(), "the superseded handle owns nothing");

    // Release the stale evaluation and let it try to commit.
    blocking.release.store(true, Ordering::Release);
    assert!(
        poll_until(POLL, || blocking.returned.load(Ordering::Acquire)).await,
        "the parked evaluation never returned",
    );
    tokio::time::sleep(SETTLE).await;

    assert_eq!(
        attestations_emitted(&mesh),
        0,
        "the superseded evaluation published anyway — the fence leaked",
    );
    assert!(
        mesh.inner().sensing_latest_attestation(&branch).is_none(),
        "a stale Ready reached the wire cache",
    );
    assert_eq!(
        mesh.inner().sensing_projected(&branch),
        ProjectedReadiness::Unknown,
        "a stale Ready reached the local projection",
    );

    // The successor still works, and its beat is the FIRST publication.
    assert!(new.changed(), "the successor owns the schedule");
    assert!(
        poll_until(POLL, || mesh.inner().sensing_projected(&branch)
            == ProjectedReadiness::NotReady)
        .await,
        "the successor never published",
    );
    assert_eq!(
        attestations_emitted(&mesh),
        1,
        "exactly one attestation — the successor's — may ever have been published",
    );

    assert!(new.close());
}

/// **Inverse witness: an in-flight evaluation publishing after its
/// registration was CLOSED.**
///
/// Same fence, the other ownership edge. After `close` returns, the
/// parked `Ready` must never become the latest observation and no
/// attestation may be published at all.
///
/// Fails against: the same three wrong implementations as the
/// replacement witness.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_evaluation_in_flight_cannot_publish_after_its_close_completes() {
    let mesh = origin_mesh().await;
    let client = mesh.sensing().expect("sensing enabled");
    let (blocking, evaluator) = BlockingEvaluator::new();

    let registration = client
        .provide(CapabilityId::new(CAPABILITY), evaluator)
        .expect("provide");
    let branch = watch_self(&mesh, CAPABILITY).await;

    assert!(
        poll_until(POLL, || blocking.entered.load(Ordering::Acquire)).await,
        "the evaluator never ran, so nothing is in flight to fence",
    );

    // Close while the evaluation is parked. This must complete without
    // waiting on user code.
    assert!(
        registration.close(),
        "close must succeed while an evaluation is in flight",
    );
    assert_eq!(mesh.inner().sensing_evaluator_count(), 0);

    blocking.release.store(true, Ordering::Release);
    assert!(
        poll_until(POLL, || blocking.returned.load(Ordering::Acquire)).await,
        "the parked evaluation never returned",
    );
    tokio::time::sleep(SETTLE).await;

    assert_eq!(
        attestations_emitted(&mesh),
        0,
        "a closed registration's evaluation published anyway — the fence leaked",
    );
    assert!(
        mesh.inner().sensing_latest_attestation(&branch).is_none(),
        "a closed registration's Ready reached the wire cache",
    );
    assert_eq!(
        mesh.inner().sensing_projected(&branch),
        ProjectedReadiness::Unknown,
        "a closed registration's Ready reached the local projection",
    );
}

// ---------------------------------------------------------------------
// State-edge notification (H2, §6 witness 15)
// ---------------------------------------------------------------------

/// **Inverse witness: a `changed` notification that does not reach the
/// exact observation path, or one that publishes a cached verdict
/// instead of waking a fresh evaluation.**
///
/// The cadence is deliberately far outside the poll window (see [`D`]),
/// so ordinary beats cannot deliver the flip. The test first proves
/// that: after publishing `NotReady` it waits and confirms the
/// observation is STILL `Ready` — which is also the honest statement of
/// why the notification must FOLLOW publication, since a beat woken
/// before the state was visible would simply re-sign the old answer.
/// Only then does it announce the edge, and the flip that follows is
/// attributable to nothing else.
///
/// Fails against: `changed()` that pokes nothing; a `provide` that
/// snapshotted the registration-time verdict and a `changed()` that
/// republished it.
#[tokio::test]
async fn a_state_edge_notification_advances_the_exact_observation_path() {
    let mesh = origin_mesh().await;
    let client = mesh.sensing().expect("sensing enabled");
    let (ready, evaluations, evaluator) = FlagEvaluator::pair();

    let registration = client
        .provide(CapabilityId::new(CAPABILITY), evaluator)
        .expect("provide");
    let branch = watch_self(&mesh, CAPABILITY).await;

    assert!(
        poll_until(POLL, || mesh.inner().sensing_projected(&branch)
            == ProjectedReadiness::Ready)
        .await,
        "the self-provider observation path never reached Ready",
    );
    let before = evaluations.load(Ordering::Relaxed);

    // Publish the new state. The next scheduled beat is ~10 s out, so
    // nothing may change until the edge is announced.
    ready.store(false, Ordering::Relaxed);
    tokio::time::sleep(SETTLE).await;
    assert_eq!(
        mesh.inner().sensing_projected(&branch),
        ProjectedReadiness::Ready,
        "the cadence must not be able to deliver this flip — \
         otherwise the state-edge assertion below proves nothing",
    );
    assert_eq!(
        evaluations.load(Ordering::Relaxed),
        before,
        "no beat may have run yet",
    );

    // Announce the edge. This, and only this, delivers the flip.
    assert!(
        registration.changed(),
        "a live observation path on this capability must move",
    );
    assert!(
        poll_until(POLL, || mesh.inner().sensing_projected(&branch)
            == ProjectedReadiness::NotReady)
        .await,
        "the published NotReady never reached the exact observation path",
    );
    assert!(
        evaluations.load(Ordering::Relaxed) > before,
        "the edge must re-run the evaluator, not replay a cached verdict",
    );

    assert!(registration.close());
}

/// A notification for a capability nothing is watching moves nothing,
/// and a closed handle's notification is inert.
///
/// The inertness half is DIFFERENTIAL, because "closed handle returns
/// false" is trivially satisfiable whenever the node happens to have
/// nothing to move. The stale handle is asked FIRST and its successor
/// SECOND, and the witness demands `successor moved && !stale moved` in
/// one iteration. A `changed()` that ignored ownership would poke the
/// node from the stale handle, consume the movement, and leave the
/// successor with nothing — so the conjunction can never hold.
///
/// Fails against: `changed()` that pokes nothing; `changed()` that
/// ignores `close`.
#[tokio::test]
async fn changed_is_false_when_nothing_is_watching_and_inert_once_closed() {
    let mesh = origin_mesh().await;
    let client = mesh.sensing().expect("sensing enabled");
    let (_ready, _evaluations, evaluator) = FlagEvaluator::pair();

    let stale = client
        .provide(CapabilityId::new(CAPABILITY), evaluator)
        .expect("provide");
    assert!(
        !stale.changed(),
        "no live stream targets this capability yet",
    );

    let _branch = watch_self(&mesh, CAPABILITY).await;
    assert!(
        poll_until(POLL, || stale.changed()).await,
        "a live stream must eventually be movable",
    );

    // Retire the handle and hand the capability to a successor. The
    // observation path is untouched by either operation — it belongs to
    // the interest, not to the evaluator registration.
    assert!(stale.close());
    let successor = client
        .provide(CapabilityId::new(CAPABILITY), Arc::new(AlwaysNotReady))
        .expect("successor provide");

    assert!(
        poll_until(POLL, || {
            let stale_moved = stale.changed();
            let successor_moved = successor.changed();
            successor_moved && !stale_moved
        })
        .await,
        "the stale handle must never move a schedule its successor owns",
    );

    assert!(successor.close());
}

/// **Inverse witness: a SUPERSEDED-BUT-OPEN handle poking its
/// successor's schedule.**
///
/// The previous witness closes the stale handle first, so a `changed()`
/// gated only on the local `closed` bit would still pass it. Here the
/// old handle is never closed — it is simply no longer the owner. A
/// `changed()` that consults only its own flag cannot tell, and would
/// poke.
///
/// Fails against: `changed()` gated on a handle-local `closed` bit
/// rather than on node-side ownership.
#[tokio::test]
async fn a_superseded_but_open_handle_cannot_move_its_successors_schedule() {
    let mesh = origin_mesh().await;
    let client = mesh.sensing().expect("sensing enabled");
    let (_ready, _evaluations, evaluator) = FlagEvaluator::pair();

    let superseded = client
        .provide(CapabilityId::new(CAPABILITY), evaluator)
        .expect("provide");
    let _branch = watch_self(&mesh, CAPABILITY).await;
    assert!(
        poll_until(POLL, || superseded.changed()).await,
        "the original owner must be able to move the schedule",
    );

    // Supersede it, WITHOUT closing the old handle.
    let successor = client
        .provide_replacing(CapabilityId::new(CAPABILITY), Arc::new(AlwaysNotReady))
        .expect("explicit replacement");

    assert!(
        poll_until(POLL, || {
            let superseded_moved = superseded.changed();
            let successor_moved = successor.changed();
            successor_moved && !superseded_moved
        })
        .await,
        "a superseded-but-open handle moved a schedule it no longer owns",
    );

    // And it stays inert — this is not a one-shot transition.
    assert!(!superseded.changed());
    assert!(!superseded.changed());

    assert!(successor.close());
    // The superseded handle's own drop is still inert.
    drop(superseded);
    assert_eq!(mesh.inner().sensing_evaluator_count(), 0);
}

// ---------------------------------------------------------------------
// Registration-identity exhaustion (H3)
// ---------------------------------------------------------------------

/// **Inverse witness: id exhaustion wrapping and aliasing stale
/// tokens.**
///
/// The last issuable identity still installs; past it, every install is
/// refused with a typed terminal error, the incumbent keeps serving, and
/// removal still works. A wrapping allocator would keep returning `Ok`
/// here and would eventually reissue an id a closed handle still holds.
#[tokio::test]
async fn provide_is_terminally_refused_once_registration_identities_are_exhausted() {
    let mesh = origin_mesh().await;
    let client = mesh.sensing().expect("sensing enabled");

    // One below the boundary: the last identity is still issuable.
    mesh.inner().set_sensing_evaluator_next_id_for_test(
        net::adapter::net::MeshNode::sensing_max_registration_id_for_test(),
    );
    assert!(!mesh.inner().sensing_evaluator_identities_exhausted());

    let last = client
        .provide(CapabilityId::new(CAPABILITY), Arc::new(AlwaysNotReady))
        .expect("the last issuable identity installs");
    assert!(mesh.inner().sensing_evaluator_identities_exhausted());

    // Terminal for a fresh capability...
    assert_eq!(
        client
            .provide(
                CapabilityId::new(OTHER_CAPABILITY),
                Arc::new(AlwaysNotReady)
            )
            .err(),
        Some(SensingError::RegistrationIdentityExhausted),
    );
    // ...and for supersession, which leaves the incumbent serving.
    assert_eq!(
        client
            .provide_replacing(CapabilityId::new(CAPABILITY), Arc::new(AlwaysNotReady))
            .err(),
        Some(SensingError::RegistrationIdentityExhausted),
    );
    assert_eq!(
        mesh.inner().sensing_evaluator_count(),
        1,
        "no new registration was published, and the incumbent survived",
    );

    // Removal remains available — a node that cannot install must still
    // be able to stop serving.
    assert!(last.close());
    assert_eq!(mesh.inner().sensing_evaluator_count(), 0);
    // But the vacated slot still cannot be refilled: the state is
    // terminal, not merely a capacity hint.
    assert_eq!(
        client
            .provide(CapabilityId::new(CAPABILITY), Arc::new(AlwaysNotReady))
            .err(),
        Some(SensingError::RegistrationIdentityExhausted),
    );
}

// ---------------------------------------------------------------------
// Shared-node ownership
// ---------------------------------------------------------------------

/// Registration state lives on the NODE, not on the SDK wrapper.
///
/// Uses a genuine SECOND `Mesh` over the same `Arc<MeshNode>` via
/// `Mesh::from_node_arc` — the public constructor that made the
/// audience-lease regression possible — so the two wrappers really are
/// distinct SDK objects. The second wrapper's `provide` must be refused
/// rather than silently stealing the first wrapper's capability, and the
/// first wrapper's handle must remain the owner.
#[tokio::test]
async fn two_mesh_wrappers_over_one_node_share_one_registration() {
    let mesh = origin_mesh().await;
    let (_ready, _evaluations, evaluator) = FlagEvaluator::pair();

    let first = mesh
        .sensing()
        .expect("sensing enabled")
        .provide(CapabilityId::new(CAPABILITY), evaluator)
        .expect("first provide");

    // A genuinely separate SDK wrapper over the same node.
    let second_wrapper = Mesh::from_node_arc(
        mesh.node_arc(),
        Arc::new(net::adapter::net::ChannelConfigRegistry::new()),
        None,
    );
    let second_client = second_wrapper.sensing().expect("sensing enabled");
    assert_eq!(
        second_client
            .provide(CapabilityId::new(CAPABILITY), Arc::new(AlwaysNotReady))
            .err(),
        Some(SensingError::AlreadyProviding {
            capability: CAPABILITY.to_string(),
        }),
    );
    assert_eq!(mesh.inner().sensing_evaluator_count(), 1);

    assert!(first.close(), "the owning handle still owns the row");
    let via_second = second_client
        .provide(CapabilityId::new(CAPABILITY), Arc::new(AlwaysNotReady))
        .expect("the vacated capability admits the second wrapper");
    assert!(via_second.close());
}
