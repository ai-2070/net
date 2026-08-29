//! Rust SDK integration witnesses for the S1 provider slice
//! (`docs/internal/plans/CAPABILITY_SENSING_SDK_INTEGRATION_PLAN.md`
//! §4.4/§4.2, §6 witnesses 14–17).
//!
//! Exercises `sdk/src/sensing.rs` against real `Mesh`es: the loud
//! refusals, ownership-safe `provide` / `close` / drop, state-edge
//! notification through the live observation path, and the
//! exact-provider projection's population clamp.
//!
//! Every critical case here is an INVERSE witness — it fails against a
//! specific wrong implementation, named in the test's doc comment.
//!
//! The state-edge and retained-observation tests use the SELF-PROVIDER
//! path: one node registers an exact-provider interest in itself, so it
//! is both origin and consumer. That keeps the whole attestation +
//! continuity pipeline live (`MeshNode` feeds its own emitter with no
//! wire hop) while staying deterministic — no second node, no delivery
//! scheduling, no fleet-root configuration the SDK deliberately does
//! not expose.

#![cfg(feature = "net")]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::sensing::{
    CanonicalConstraints, DisclosureClass, InterestSpec, ProviderInterestKey, ProviderSelector,
    ResultMode, WorkLatencyEnvelope,
};
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::sensing::{
    CapabilityId, ConsumerLatencyBudget, EvaluationRequest, Incarnation, ProjectedReadiness,
    ReadinessEvaluation, ReadinessEvaluator, SensingError,
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

async fn mesh_with(enable_sensing: bool, incarnation: Option<Incarnation>) -> Mesh {
    let mut builder = MeshBuilder::new("127.0.0.1:0", &PSK).expect("bind addr");
    if enable_sensing {
        builder = builder.enable_sensing();
    }
    if let Some(incarnation) = incarnation {
        builder = builder.sensing_incarnation(incarnation);
    }
    builder.build().await.expect("build mesh")
}

/// A sensing-enabled ORIGIN: the plane is on and a persisted epoch was
/// supplied, so this node can sign readiness for itself.
async fn origin_mesh() -> Mesh {
    mesh_with(true, Some(Incarnation::new(1))).await
}

/// An exactly addressed interest in `provider`, scoped to this node's
/// own audience.
fn exact_spec(mesh: &Mesh, capability: &str, provider: u64) -> InterestSpec {
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

/// A budget that admits everything — the tests here are about
/// readiness, not about route economics.
fn budget() -> ConsumerLatencyBudget {
    ConsumerLatencyBudget::default()
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
    let spec = exact_spec(mesh, capability, own_id);
    mesh.inner().start();
    mesh.inner()
        .register_sensing_interest(&spec, own_id, D, TTL)
        .expect("self interest registers");
    ProviderInterestKey::new(spec.key(), own_id)
}

// ---------------------------------------------------------------------
// Loud refusals (§6 witness 14)
// ---------------------------------------------------------------------

/// **Inverse witness: a missing incarnation silently accepted.**
///
/// The origin role is fail-closed. An implementation that installed the
/// evaluator anyway would return `Ok` here and leave the caller
/// believing it is publishing readiness on a node that can never sign.
#[tokio::test]
async fn provide_without_a_persisted_incarnation_fails_loudly() {
    let mesh = mesh_with(true, None).await;
    assert!(
        !mesh.inner().sensing_origin_active(),
        "no incarnation must leave the origin role dark",
    );

    let (_ready, _evaluations, evaluator) = FlagEvaluator::pair();
    let client = mesh.sensing().expect("sensing enabled");
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
    let mesh = mesh_with(true, None).await;
    let client = mesh.sensing().expect("sensing enabled");
    assert_eq!(
        client
            .provide_replacing(CapabilityId::new(CAPABILITY), Arc::new(AlwaysNotReady))
            .err(),
        Some(SensingError::IncarnationRequired),
    );
}

/// The plane ships dark: no `SensingClient` at all until it is enabled.
#[tokio::test]
async fn the_sensing_surface_is_refused_while_the_plane_is_disabled() {
    let mesh = mesh_with(false, Some(Incarnation::new(1))).await;
    assert_eq!(mesh.sensing().err(), Some(SensingError::Disabled));
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
        "a superseded handle must not report a removal",
    );
    drop(old);

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
// State-edge notification (§6 witness 15)
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
    tokio::time::sleep(Duration::from_millis(400)).await;
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
/// and a closed handle's notification is inert — so a stale handle
/// cannot disturb its replacement's emission schedule.
#[tokio::test]
async fn changed_is_false_when_nothing_is_watching_and_inert_once_closed() {
    let mesh = origin_mesh().await;
    let client = mesh.sensing().expect("sensing enabled");
    let (_ready, _evaluations, evaluator) = FlagEvaluator::pair();

    let registration = client
        .provide(CapabilityId::new(CAPABILITY), evaluator)
        .expect("provide");
    assert!(
        !registration.changed(),
        "no live stream targets this capability yet",
    );

    let _branch = watch_self(&mesh, CAPABILITY).await;
    assert!(
        poll_until(POLL, || registration.changed()).await,
        "a live stream must eventually be movable",
    );

    assert!(registration.close());
    assert!(
        !registration.changed(),
        "a closed registration must not touch the node",
    );
}

// ---------------------------------------------------------------------
// Exact-provider projection (§6 witnesses 6, 13, 17)
// ---------------------------------------------------------------------

/// **Inverse witness: a provider outside the supplied population
/// admitted.**
///
/// The node holds a real retained observation for ITSELF, yet a
/// projection over a population that excludes it must not report it —
/// not as Ready, not as potential, not at all. An implementation that
/// enumerated observations and then filtered (or forgot to) would leak
/// the self observation here.
#[tokio::test]
async fn the_authorized_population_is_a_hard_upper_bound() {
    let mesh = origin_mesh().await;
    let client = mesh.sensing().expect("sensing enabled");
    let (_ready, _evaluations, evaluator) = FlagEvaluator::pair();
    let registration = client
        .provide(CapabilityId::new(CAPABILITY), evaluator)
        .expect("provide");

    let own_id = mesh.inner().node_id();
    let branch = watch_self(&mesh, CAPABILITY).await;
    assert!(
        poll_until(POLL, || mesh.inner().sensing_projected(&branch)
            == ProjectedReadiness::Ready)
        .await,
        "the self observation never established",
    );

    let spec = exact_spec(&mesh, CAPABILITY, own_id);
    let stranger = own_id.wrapping_add(1);

    // Population excludes the observed provider entirely.
    let clamped = client
        .exact_provider_readiness(&spec, &budget(), &[stranger])
        .expect("exact projection");
    assert_eq!(clamped.len(), 1);
    assert_eq!(clamped.providers()[0].provider, stranger);
    assert!(
        !clamped.viable().contains(&own_id)
            && !clamped.potential().contains(&own_id)
            && !clamped.non_viable().contains(&own_id),
        "a retained observation outside the population escaped the clamp",
    );
    assert_eq!(clamped.readiness(own_id), ProjectedReadiness::Unknown);

    // Including it surfaces the same observation — the clamp filters,
    // it does not suppress.
    let admitted = client
        .exact_provider_readiness(&spec, &budget(), &[own_id, stranger])
        .expect("exact projection");
    assert_eq!(admitted.readiness(own_id), ProjectedReadiness::Ready);
    assert_eq!(admitted.viable(), &[own_id]);
    assert_eq!(admitted.best_provider(), Some(own_id));

    assert!(registration.close());
}

/// **Inverse witness: an unsupported / evaluator-free provider marked
/// Ready or NotReady.**
///
/// A provider with no observation — the "targeted but cannot answer"
/// and "not yet observed" cases — projects `Unknown` and stays
/// `potential`. Neither a `Ready` claim nor a global `NotReady` is
/// honest here: absence of evidence never prunes.
#[tokio::test]
async fn providers_without_an_observation_project_unknown_and_stay_potential() {
    let mesh = origin_mesh().await;
    let client = mesh.sensing().expect("sensing enabled");
    let own_id = mesh.inner().node_id();

    // No evaluator installed for this capability at all, and no watch.
    let spec = exact_spec(&mesh, OTHER_CAPABILITY, own_id);
    let population = [own_id, own_id.wrapping_add(1), own_id.wrapping_add(2)];

    let projection = client
        .exact_provider_readiness(&spec, &budget(), &population)
        .expect("exact projection");

    assert_eq!(projection.len(), 3);
    for sensed in projection.providers() {
        assert_eq!(
            sensed.readiness,
            ProjectedReadiness::Unknown,
            "provider {} must be Unknown, not Ready or NotReady",
            sensed.provider,
        );
        assert_eq!(sensed.estimated_start, None);
        assert_eq!(sensed.capability_generation, None);
    }
    assert!(projection.viable().is_empty(), "nothing may be viable");
    assert!(
        projection.non_viable().is_empty(),
        "an unobserved provider must never be pruned as NotReady",
    );
    assert_eq!(projection.potential().len(), 3);
    assert_eq!(
        projection.best_provider(),
        None,
        "no viable provider means no selection, not an arbitrary one",
    );
}

/// **Inverse witness: an exact NotReady pruning more than its own
/// interest.**
///
/// The provider answers NotReady for the watched interest. That
/// interest's candidate is pruned; a DIFFERENT interest on the same
/// provider is untouched and stays Unknown/potential.
#[tokio::test]
async fn an_exact_not_ready_prunes_only_that_interest() {
    let mesh = origin_mesh().await;
    let client = mesh.sensing().expect("sensing enabled");
    let (ready, _evaluations, evaluator) = FlagEvaluator::pair();
    ready.store(false, Ordering::Relaxed);
    let registration = client
        .provide(CapabilityId::new(CAPABILITY), evaluator)
        .expect("provide");

    let own_id = mesh.inner().node_id();
    let branch = watch_self(&mesh, CAPABILITY).await;
    assert!(
        poll_until(POLL, || mesh.inner().sensing_projected(&branch)
            == ProjectedReadiness::NotReady)
        .await,
        "the NotReady observation never arrived",
    );

    let watched = exact_spec(&mesh, CAPABILITY, own_id);
    let pruned = client
        .exact_provider_readiness(&watched, &budget(), &[own_id])
        .expect("exact projection");
    assert_eq!(pruned.readiness(own_id), ProjectedReadiness::NotReady);
    assert_eq!(pruned.non_viable(), &[own_id]);
    assert!(pruned.viable().is_empty());
    assert_eq!(pruned.best_provider(), None);

    // A different capability on the SAME provider is unaffected.
    let unrelated = exact_spec(&mesh, OTHER_CAPABILITY, own_id);
    let untouched = client
        .exact_provider_readiness(&unrelated, &budget(), &[own_id])
        .expect("exact projection");
    assert_eq!(untouched.readiness(own_id), ProjectedReadiness::Unknown);
    assert_eq!(untouched.potential(), &[own_id]);
    assert!(
        untouched.non_viable().is_empty(),
        "one interest's NotReady must not prune another interest",
    );

    assert!(registration.close());
}

/// Duplicates in the supplied population collapse to one projection —
/// a caller's sloppy candidate list cannot inflate the classification
/// lists or double-count a provider.
#[tokio::test]
async fn duplicate_providers_in_the_population_collapse_to_one_projection() {
    let mesh = origin_mesh().await;
    let client = mesh.sensing().expect("sensing enabled");
    let own_id = mesh.inner().node_id();
    let spec = exact_spec(&mesh, CAPABILITY, own_id);

    let projection = client
        .exact_provider_readiness(&spec, &budget(), &[own_id, own_id, own_id])
        .expect("exact projection");

    assert_eq!(projection.len(), 1);
    assert_eq!(projection.potential(), &[own_id]);
    assert!(projection.viable().is_empty());
    assert!(projection.non_viable().is_empty());
}

/// An empty authorized population projects nothing. Sensing cannot
/// invent a candidate.
#[tokio::test]
async fn an_empty_population_projects_no_providers() {
    let mesh = origin_mesh().await;
    let client = mesh.sensing().expect("sensing enabled");
    let spec = exact_spec(&mesh, CAPABILITY, mesh.inner().node_id());

    let projection = client
        .exact_provider_readiness(&spec, &budget(), &[])
        .expect("exact projection");

    assert!(projection.is_empty());
    assert_eq!(projection.len(), 0);
    assert!(projection.viable().is_empty());
    assert!(projection.potential().is_empty());
    assert!(projection.non_viable().is_empty());
    assert_eq!(projection.best_provider(), None);
}

/// The exact seam refuses a provider-free selector rather than
/// quietly becoming the leader-routed path this slice does not
/// implement.
#[tokio::test]
async fn a_provider_free_selector_is_refused_by_the_exact_seam() {
    let mesh = origin_mesh().await;
    let client = mesh.sensing().expect("sensing enabled");
    let own_id = mesh.inner().node_id();

    let mut spec = exact_spec(&mesh, CAPABILITY, own_id);
    spec.providers = ProviderSelector::AnyAuthorized;
    assert_eq!(
        client
            .exact_provider_readiness(&spec, &budget(), &[own_id])
            .err(),
        Some(SensingError::ProviderFreeSelector {
            selector: "AnyAuthorized",
        }),
    );

    spec.providers = ProviderSelector::Tags(Vec::new());
    assert_eq!(
        client
            .exact_provider_readiness(&spec, &budget(), &[own_id])
            .err(),
        Some(SensingError::ProviderFreeSelector { selector: "Tags" }),
    );

    // The exact form is admitted.
    spec.providers = ProviderSelector::Node(own_id);
    assert!(client
        .exact_provider_readiness(&spec, &budget(), &[own_id])
        .is_ok());
}

// ---------------------------------------------------------------------
// Shared-node ownership
// ---------------------------------------------------------------------

/// Registration state lives on the NODE, not on the SDK wrapper: two
/// `Mesh` wrappers over one `MeshNode` see one registry, so the second
/// wrapper's `provide` is refused rather than silently stealing the
/// first wrapper's capability.
#[tokio::test]
async fn two_mesh_wrappers_over_one_node_share_one_registration() {
    let mesh = origin_mesh().await;
    let (_ready, _evaluations, evaluator) = FlagEvaluator::pair();

    let first = mesh
        .sensing()
        .expect("sensing enabled")
        .provide(CapabilityId::new(CAPABILITY), evaluator)
        .expect("first provide");

    // A second client over the same node.
    let second_client = mesh.sensing().expect("sensing enabled");
    assert_eq!(
        second_client
            .provide(CapabilityId::new(CAPABILITY), Arc::new(AlwaysNotReady))
            .err(),
        Some(SensingError::AlreadyProviding {
            capability: CAPABILITY.to_string(),
        }),
    );

    assert!(first.close(), "the owning handle still owns the row");
    assert!(second_client
        .provide(CapabilityId::new(CAPABILITY), Arc::new(AlwaysNotReady))
        .is_ok());
}
