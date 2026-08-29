//! Capability sensing — the **provider** side, plus the exact-provider
//! readiness projection.
//!
//! `docs/internal/plans/CAPABILITY_SENSING_SDK_INTEGRATION_PLAN.md` §4.4
//! (provider lifecycle) and §4.2 (snapshot). Sensing answers one
//! question — "can this provider currently satisfy capability Y under
//! characteristics C and latency envelope L?" — and the answer is
//! **advisory**: a provider signs what it evaluates about itself and
//! each consumer judges viability against its own latency budget.
//!
//! # What readiness is not
//!
//! Readiness is not a reservation, not admission, not execution
//! authority, and not a freshness claim. A `Ready` observation does not
//! hold capacity and does not authorize an invocation — protected calls
//! still construct their own proof and are still admitted or refused by
//! the provider. Nothing here exposes an evidence age, and a later call
//! may legitimately fail after a `Ready` observation.
//!
//! # Provider side
//!
//! ```no_run
//! use std::sync::Arc;
//! use std::sync::atomic::{AtomicBool, Ordering};
//!
//! use net_sdk::sensing::{
//!     EvaluationRequest, ReadinessEvaluation, ReadinessEvaluator,
//! };
//!
//! /// Readiness is read from a cheap published snapshot. Expensive
//! /// state acquisition stays OUTSIDE `evaluate` — it runs on the
//! /// emission path and must be non-blocking.
//! struct QueueDepth {
//!     accepting: Arc<AtomicBool>,
//! }
//!
//! impl ReadinessEvaluator for QueueDepth {
//!     fn evaluate(&self, _request: &EvaluationRequest<'_>) -> ReadinessEvaluation {
//!         if self.accepting.load(Ordering::Relaxed) {
//!             ReadinessEvaluation::Ready { estimated_start: None }
//!         } else {
//!             ReadinessEvaluation::NotReady { reason: 1 }
//!         }
//!     }
//! }
//!
//! # async fn example(mesh: &net_sdk::mesh::Mesh) -> Result<(), Box<dyn std::error::Error>> {
//! let accepting = Arc::new(AtomicBool::new(true));
//! let readiness = mesh
//!     .sensing()?
//!     .provide("gpu.infer", Arc::new(QueueDepth { accepting: accepting.clone() }))?;
//!
//! // Publish the new state FIRST, then announce the edge: the
//! // notification is a wake, never the value.
//! accepting.store(false, Ordering::Relaxed);
//! readiness.changed();
//!
//! readiness.close();
//! # Ok(())
//! # }
//! ```
//!
//! # Ownership
//!
//! [`ReadinessRegistration`] owns exactly the registration that minted
//! it. Two integrations cannot silently fight over one capability:
//! [`SensingClient::provide`] refuses an occupied capability, and
//! supersession is spelled out with
//! [`SensingClient::provide_replacing`]. A superseded handle's `close`
//! or drop removes nothing, so a replacement is never evicted by the
//! registration it replaced.
//!
//! # What this module deliberately does not contain
//!
//! There is no generic query surface, no provider-free watch, no leader
//! resolution, and no rendezvous: those are later slices. Nothing here
//! exposes leader ids, audience commitments, wire digests, frames,
//! private discovery records, or retry/admission policy.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::sensing;
use net::adapter::net::MeshNode;

use crate::mesh::Mesh;

/// The provider-side evaluator contract and its value types. A
/// capability integration implements [`ReadinessEvaluator`] and nothing
/// else.
pub use sensing::{
    CapabilityId, EvaluationRequest, ReadinessEvaluation, ReadinessEvaluator, StatusReason,
};

/// The persisted provider boot epoch and its derivation. An origin
/// signs its attestation sequence under this counter, so it MUST come
/// from durable storage — a per-boot random value cannot be ordered and
/// would let a replayed old epoch masquerade as a fresh restart.
///
/// Derive it with [`next_incarnation`] over a real
/// [`IncarnationPersistence`] *before* building the mesh, then hand it
/// to [`MeshBuilder::sensing_incarnation`](crate::mesh::MeshBuilder::sensing_incarnation).
pub use sensing::{
    next_incarnation, Incarnation, IncarnationError, IncarnationPersistence, PersistenceFault,
};

/// The canonical interest vocabulary an exact-provider projection is
/// asked about. These are the existing frozen semantics — the SDK adds
/// no parallel ontology and no query DSL.
pub use sensing::{
    CanonicalConstraints, ConstraintError, ConsumerLatencyBudget, DisclosureClass, InterestSpec,
    ProjectedReadiness, ProviderSelector, ResultMode, WorkLatencyEnvelope,
};

/// A loud refusal from the sensing surface.
///
/// Every variant is a configuration or ownership fact the caller can
/// act on. Sensing never degrades silently: a provider that cannot sign
/// readiness is told so rather than left dark.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SensingError {
    /// The capability-sensing plane is off on this mesh (it ships
    /// dark).
    #[error(
        "capability sensing is disabled on this mesh — \
         build it with MeshBuilder::enable_sensing()"
    )]
    Disabled,

    /// Provider readiness was requested on a node with no persisted
    /// sensing incarnation. The origin role is fail-closed: without a
    /// durable epoch the node would either sign an unorderable
    /// sequence or stay silently dark.
    #[error(
        "provider readiness needs a persisted sensing incarnation — \
         derive one with net_sdk::sensing::next_incarnation over durable storage \
         and pass it to MeshBuilder::sensing_incarnation()"
    )]
    IncarnationRequired,

    /// Another registration already serves this capability's
    /// readiness. Close it, or state supersession explicitly with
    /// [`SensingClient::provide_replacing`].
    #[error(
        "a readiness evaluator is already registered for capability `{capability}` — \
         close that registration or call provide_replacing to supersede it"
    )]
    AlreadyProviding {
        /// The contested capability id.
        capability: String,
    },

    /// The exact-provider projection was handed a provider-free
    /// selector. Provider-free sensing is leader-routed and is not part
    /// of this surface; an exact projection needs an exactly addressed
    /// interest.
    #[error(
        "the exact-provider projection needs an exactly addressed interest — \
         ProviderSelector::{selector} is provider-free"
    )]
    ProviderFreeSelector {
        /// The offending selector's variant name.
        selector: &'static str,
    },
}

/// The sensing surface bound to one live node.
///
/// Obtained from [`Mesh::sensing`]. Cheap to clone — it holds the same
/// `Arc<MeshNode>` the mesh does, and all registration state lives on
/// the node, so two clients (or two `Mesh` wrappers over one node)
/// cannot disagree about who owns a capability's readiness.
#[derive(Clone)]
pub struct SensingClient {
    node: Arc<MeshNode>,
}

impl std::fmt::Debug for SensingClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SensingClient")
            .field("node_id", &self.node.node_id())
            .finish()
    }
}

impl Mesh {
    /// The capability-sensing surface for this mesh.
    ///
    /// Refuses loudly with [`SensingError::Disabled`] when the plane is
    /// off. Being a provider additionally needs a persisted
    /// incarnation, which [`SensingClient::provide`] checks — a node
    /// may legitimately hold a client without being an origin.
    pub fn sensing(&self) -> Result<SensingClient, SensingError> {
        SensingClient::bind_node(self.node().clone())
    }
}

impl SensingClient {
    /// Bind the sensing surface to a node handle.
    fn bind_node(node: Arc<MeshNode>) -> Result<Self, SensingError> {
        if !node.sensing_enabled() {
            return Err(SensingError::Disabled);
        }
        Ok(Self { node })
    }

    /// Serve readiness for one capability.
    ///
    /// The evaluator is the existing cheap synchronous contract: it
    /// runs on the emission path at the aggregated cadence plus on
    /// state edges, so it must not block. Keep expensive state
    /// acquisition outside it — publish into an atomic or an
    /// `ArcSwap` snapshot the evaluator merely reads.
    ///
    /// Refuses with:
    ///
    /// - [`SensingError::IncarnationRequired`] when this node has no
    ///   persisted sensing incarnation. A registration that "worked"
    ///   on a node that can never sign is worse than a refusal.
    /// - [`SensingError::AlreadyProviding`] when the capability is
    ///   already served. The incumbent is untouched — this call never
    ///   steals a live registration.
    pub fn provide(
        &self,
        capability: impl Into<CapabilityId>,
        evaluator: Arc<dyn ReadinessEvaluator + Send + Sync>,
    ) -> Result<ReadinessRegistration, SensingError> {
        let capability_id = capability.into();
        self.require_origin()?;
        let registration_id = self
            .node
            .register_readiness_evaluator(capability_id.clone(), evaluator)
            .map_err(|_| SensingError::AlreadyProviding {
                capability: capability_id.as_str().to_string(),
            })?;
        Ok(ReadinessRegistration::new(
            self.node.clone(),
            capability_id,
            registration_id,
        ))
    }

    /// Serve readiness for one capability, EXPLICITLY superseding any
    /// existing registration for it.
    ///
    /// Use this only when supersession is the intent (a reloaded
    /// integration re-installing its own evaluator, say). The
    /// superseded registration is inert from this call onward: its
    /// `close` and its drop both remove nothing, so it can never evict
    /// the registration this call installed.
    ///
    /// Same [`SensingError::IncarnationRequired`] refusal as
    /// [`Self::provide`].
    pub fn provide_replacing(
        &self,
        capability: impl Into<CapabilityId>,
        evaluator: Arc<dyn ReadinessEvaluator + Send + Sync>,
    ) -> Result<ReadinessRegistration, SensingError> {
        let capability_id = capability.into();
        self.require_origin()?;
        let registration_id = self
            .node
            .replace_readiness_evaluator(capability_id.clone(), evaluator);
        Ok(ReadinessRegistration::new(
            self.node.clone(),
            capability_id,
            registration_id,
        ))
    }

    /// The exact-provider readiness projection over a
    /// caller-supplied, ALREADY AUTHORIZED provider population.
    ///
    /// The population is a **hard upper bound**: this is a projection,
    /// not a discovery producer. Sensing can only classify providers
    /// the caller already authorized — it never adds one, and a
    /// retained observation for a provider outside the supplied
    /// population is excluded rather than reported. Duplicates in
    /// `authorized_population` collapse to one projection.
    ///
    /// Providers with no usable observation project
    /// [`ProjectedReadiness::Unknown`] — never `Ready`, and never a
    /// global `NotReady`. An exact `NotReady` prunes that interest's
    /// candidate only; it says nothing about the provider's other
    /// interests and never suspends its capability membership.
    ///
    /// Ordering and viability come from the existing core semantics
    /// (the same classification the aggregate projects), so a caller
    /// and the schedulers can never disagree about one branch.
    ///
    /// Refuses with [`SensingError::ProviderFreeSelector`] if `spec`
    /// is not exactly addressed.
    pub fn exact_provider_readiness(
        &self,
        spec: &InterestSpec,
        budget: &ConsumerLatencyBudget,
        authorized_population: &[u64],
    ) -> Result<ExactProviderReadiness, SensingError> {
        if spec.providers.is_provider_free() {
            return Err(SensingError::ProviderFreeSelector {
                selector: provider_free_selector_name(&spec.providers),
            });
        }
        // The clamp is applied ONCE, here, and every downstream read is
        // asked only about members of it — so no core read can widen it.
        let mut population: Vec<u64> = authorized_population.to_vec();
        population.sort_unstable();
        population.dedup();

        let candidates = self.node.sensed_candidates(spec, budget, Some(&population));
        let overlay = self
            .node
            .sensing_readiness_overlay(spec, budget, Some(&population));

        let interest = spec.key();
        let providers = population
            .iter()
            .map(|provider| {
                let observation = overlay
                    .candidates
                    .iter()
                    .find(|((observed, _), _)| observed == provider)
                    .map(|(key, observation)| (key.1, observation));
                SensedProvider {
                    provider: *provider,
                    readiness: self
                        .node
                        .sensing_projected(&sensing::ProviderInterestKey::new(
                            interest.clone(),
                            *provider,
                        )),
                    estimated_start: observation.and_then(|(_, obs)| obs.estimated_start),
                    capability_generation: observation.map(|(generation, _)| generation),
                }
            })
            .collect();

        Ok(ExactProviderReadiness {
            providers,
            candidates,
        })
    }

    /// The fail-closed origin gate: sensing is on, but signing
    /// readiness for yourself additionally needs the persisted epoch.
    fn require_origin(&self) -> Result<(), SensingError> {
        if !self.node.sensing_enabled() {
            return Err(SensingError::Disabled);
        }
        if !self.node.sensing_origin_active() {
            return Err(SensingError::IncarnationRequired);
        }
        Ok(())
    }
}

/// The variant name of a provider-free selector, for the refusal
/// message. Exact selectors never reach this.
fn provider_free_selector_name(selector: &ProviderSelector) -> &'static str {
    match selector {
        ProviderSelector::AnyAuthorized => "AnyAuthorized",
        ProviderSelector::Group(_) => "Group",
        ProviderSelector::Tags(_) => "Tags",
        ProviderSelector::Node(_) | ProviderSelector::Nodes(_) => "Node",
    }
}

/// One provider's exact-interest readiness projection.
///
/// Carries only verified projection facts. There is deliberately no
/// freshness timestamp and no capacity field: readiness is advisory and
/// reserves nothing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SensedProvider {
    /// The provider's node id.
    pub provider: u64,
    /// `Ready | Unknown | NotReady` for this exact interest.
    pub readiness: ProjectedReadiness,
    /// The provider's own time-to-start estimate, when it signed one.
    pub estimated_start: Option<Duration>,
    /// The capability generation the observation was made under.
    /// `None` when there is no observation yet.
    pub capability_generation: Option<u64>,
}

/// The exact-provider readiness projection for one interest over one
/// authorized population.
///
/// Every list is a subset of the population the caller supplied.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ExactProviderReadiness {
    providers: Vec<SensedProvider>,
    candidates: net::adapter::net::behavior::scheduler_bridge::SensedCandidates,
}

impl ExactProviderReadiness {
    /// Every authorized provider's projection, ordered by node id.
    pub fn providers(&self) -> &[SensedProvider] {
        &self.providers
    }

    /// One provider's projection. `Unknown` for a provider outside the
    /// authorized population — absence of authorization is never
    /// evidence of unreadiness.
    pub fn readiness(&self, provider: u64) -> ProjectedReadiness {
        self.providers
            .iter()
            .find(|sensed| sensed.provider == provider)
            .map_or(ProjectedReadiness::Unknown, |sensed| sensed.readiness)
    }

    /// Providers locally viable for this interest — `Ready` within the
    /// supplied budget, ranked best-first by the existing core
    /// economics.
    pub fn viable(&self) -> &[u64] {
        &self.candidates.viable
    }

    /// Providers with no viability verdict: `Unknown`, or `Ready`
    /// outside the budget. Retained, never pruned — a route change can
    /// make one viable.
    pub fn potential(&self) -> &[u64] {
        &self.candidates.potential
    }

    /// Providers sensed explicitly `NotReady` for THIS interest.
    /// Pruned from this interest's selection only.
    pub fn non_viable(&self) -> &[u64] {
        &self.candidates.non_viable
    }

    /// The provider a call for this interest should target: the
    /// best-ranked viable candidate. `None` when nothing is currently
    /// viable — the caller falls back to its own deterministic choice
    /// over [`Self::potential`], never to a failure.
    pub fn best_provider(&self) -> Option<u64> {
        self.candidates.selected_provider()
    }

    /// How many authorized providers this projection covers.
    pub fn len(&self) -> usize {
        self.providers.len()
    }

    /// Whether the authorized population was empty.
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

/// An owning handle on one provider-readiness registration.
///
/// Removes exactly its own registration, and only while that
/// registration is still the installed one. Dropping a handle that was
/// superseded by [`SensingClient::provide_replacing`] is inert. `close`
/// and drop are idempotent and race-safe: at most one of them performs
/// the removal.
///
/// Not `Clone`: two owners of one registration would make "who removes
/// it" ambiguous. Share it behind an `Arc` if several call sites need
/// to signal state edges.
pub struct ReadinessRegistration {
    node: Arc<MeshNode>,
    capability_id: CapabilityId,
    registration_id: sensing::EvaluatorRegistrationId,
    closed: AtomicBool,
}

impl ReadinessRegistration {
    fn new(
        node: Arc<MeshNode>,
        capability_id: CapabilityId,
        registration_id: sensing::EvaluatorRegistrationId,
    ) -> Self {
        Self {
            node,
            capability_id,
            registration_id,
            closed: AtomicBool::new(false),
        }
    }

    /// The capability this registration serves readiness for.
    pub fn capability(&self) -> &CapabilityId {
        &self.capability_id
    }

    /// Announce that local state affecting this capability changed:
    /// pull every live observation path on it forward to now,
    /// min-gapped at the provider's cadence floor.
    ///
    /// **Publish the new state before calling this.** The notification
    /// is a wake, never a value — a woken beat carries whatever the
    /// evaluator reads at beat time, so an edge announced before the
    /// state is visible simply re-signs the old answer.
    ///
    /// Returns whether any live observation path actually moved:
    /// `false` when nothing is watching this capability, or once this
    /// registration is closed. Inert after `close`, so a stale handle
    /// cannot disturb its replacement's schedule.
    pub fn changed(&self) -> bool {
        if self.closed.load(Ordering::Acquire) {
            return false;
        }
        self.node.notify_sensing_state_changed(&self.capability_id)
    }

    /// Stop serving readiness for this capability.
    ///
    /// Returns whether THIS call removed the registration — so `true`
    /// at most once, and `false` for a repeat close, a close after
    /// drop, or a handle that was already superseded. Live watchers
    /// fall back to `Unknown` at their next beat; nothing fails.
    pub fn close(&self) -> bool {
        if self
            .closed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        self.node
            .unregister_readiness_evaluator(&self.capability_id, self.registration_id)
    }
}

impl Drop for ReadinessRegistration {
    fn drop(&mut self) {
        self.close();
    }
}

impl std::fmt::Debug for ReadinessRegistration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadinessRegistration")
            .field("capability", &self.capability_id.as_str())
            .field("closed", &self.closed.load(Ordering::Acquire))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The handle carries an `Arc<MeshNode>`, a `CapabilityId`, an
    /// opaque id, and an `AtomicBool` — all `Send + Sync`, so the auto
    /// traits are earned rather than asserted. If an internal ever
    /// stops supporting it, this stops compiling instead of the
    /// promise quietly becoming a lie.
    #[test]
    fn the_registration_handle_is_send_and_sync() {
        fn require<T: Send + Sync>() {}
        require::<ReadinessRegistration>();
        require::<SensingClient>();
        require::<ExactProviderReadiness>();
        require::<SensedProvider>();
        require::<SensingError>();
    }

    /// Refusals must be readable without a debugger — each carries the
    /// action the caller has to take.
    #[test]
    fn every_refusal_names_the_remedy() {
        assert!(SensingError::Disabled
            .to_string()
            .contains("MeshBuilder::enable_sensing"));
        assert!(SensingError::IncarnationRequired
            .to_string()
            .contains("next_incarnation"));
        assert!(SensingError::AlreadyProviding {
            capability: "gpu.infer".into(),
        }
        .to_string()
        .contains("gpu.infer"));
        assert!(SensingError::ProviderFreeSelector {
            selector: "AnyAuthorized",
        }
        .to_string()
        .contains("AnyAuthorized"));
    }

    #[test]
    fn provider_free_selectors_are_named_exactly() {
        assert_eq!(
            provider_free_selector_name(&ProviderSelector::AnyAuthorized),
            "AnyAuthorized",
        );
        assert_eq!(
            provider_free_selector_name(&ProviderSelector::Tags(Vec::new())),
            "Tags",
        );
    }
}
