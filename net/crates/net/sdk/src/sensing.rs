//! Capability sensing — the provider lifecycle and the own-organization
//! exact-provider consumer observation.
//!
//! `docs/internal/plans/CAPABILITY_SENSING_SDK_INTEGRATION_PLAN.md` §4.4
//! (provider lifecycle) and §4.5 (configuration). Sensing answers one
//! question — "can this provider currently satisfy capability Y under
//! characteristics C and latency envelope L?" — and the answer is
//! **advisory**: a provider signs what it evaluates about itself and
//! each consumer judges viability against its own latency budget.
//! Provider admission remains final regardless of what readiness said.
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
//! [`ReadinessRegistration`] owns exactly the registration that issued
//! it, and that ownership is enforced on all three edges:
//!
//! - two integrations cannot silently fight over one capability —
//!   [`SensingClient::provide`] refuses an occupied capability and
//!   supersession must be spelled out with
//!   [`SensingClient::provide_replacing`];
//! - a superseded or closed handle's `close`, drop, and
//!   [`ReadinessRegistration::changed`] are all inert, so it can never
//!   evict or disturb its successor;
//! - a readiness result already being computed when a close or
//!   replacement lands cannot become the latest observation.
//!
//! The last two are not best-effort checks: the node tests ownership
//! and performs the effect inside one critical section shared with
//! registration, replacement, and removal.
//!
//! # Scope of this slice
//!
//! Two surfaces live here, and neither is the whole plan:
//!
//! - the PROVIDER lifecycle — [`SensingClient::provide`],
//!   [`SensingClient::provide_replacing`] and
//!   [`ReadinessRegistration`], unchanged;
//! - the CONSUMER observation for OWN-ORGANIZATION EXACT PROVIDERS —
//!   [`SensingClient::watch`], [`SensingQuery`], [`SensingWatch`],
//!   [`SensingSnapshot`]. See [`consumer`] for what it owns, what it
//!   reuses from the core's retained demand substrate, and what a
//!   snapshot deliberately does not promise.
//!
//! The consumer half rests on a core path that is fully implemented, not
//! on a dead end: an OWN-ORGANIZATION exact-provider lease plans and
//! emits `SensingInterestFrame::OrgProviderRegistration` from installed
//! authority, registers its local row under the organization-derived
//! proven root, and reaches an organization-authoritative peer through
//! that peer's ordinary registration intake. Each acquisition arms a
//! `ttl/2` renewal on the node's single refresh worker, so a retained
//! observation keeps its rows alive between application calls.
//! `SensingRegistrationError::OrgAudienceUnsupported` survives with a
//! NARROWED meaning — the audience is an organization commitment but
//! this node has no live membership to speak with right now, or the
//! captured authority view went stale before the mutation. A FOREIGN
//! organization's commitment is still undetectable from the sending
//! side (a commitment is a one-way derivation), so it takes the legacy
//! path unchanged.
//!
//! What is genuinely absent, here and everywhere else in the SDK:
//! provider-free / leader-backed sensing (no `AnyAuthorized`, tag or
//! group selector), `Granted` and cross-organization sensing, a generic
//! sensed CALL verb, compute/gang sensed adapters, warmed pools, and
//! language bindings. [`SensingQuery`] can express none of them, and
//! [`SensingClient::watch`] refuses what it cannot mean rather than
//! accepting an argument it would ignore.
//!
//! The other consumer of the same substrate is
//! [`crate::org::OrgClient`], which binds one acquisition family per
//! bind and applies the resulting order inside its own call planning.
//! That path is unchanged by this module: a watch is an independent
//! owner of node-global demand, so observing a capability neither
//! disturbs nor depends on any client's call-path acquisition.
//!
//! # What this surface does and does not name
//!
//! The consumer surface names exactly one capability string, one
//! optional end-to-end budget, and the projection's own results:
//! [`ProjectedReadiness`], [`SensedViability`], [`SensedProvider`].
//! It re-exports NO interest, audience or wire vocabulary: no
//! `InterestSpec`, no audience commitment, no provider selector, no
//! result mode, no disclosure class, no lease ticket, no leader id, no
//! digest, no private discovery record, and no retry/admission policy.
//!
//! It does NOT follow that those core types are unreachable. This is a
//! thin SDK over `net`, and [`EvaluationRequest`] necessarily carries
//! the evaluator's inputs — a `CapabilityId`, the canonical constraints,
//! and the work-latency envelope — as part of the already-frozen
//! evaluator contract. Their types are therefore nameable transitively
//! through `net`. What this slice declines to do is *re-export* them as
//! SDK surface or grow a public interest/ranking vocabulary on top of
//! them.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use net::adapter::net::behavior::sensing;
use net::adapter::net::MeshNode;

use crate::mesh::Mesh;

pub mod consumer;

/// The own-organization exact-provider consumer observation: one bounded
/// query, one request-relative snapshot, missed-wake-safe change
/// notification, and explicit close. See [`consumer`] for the ownership
/// and reuse contract.
pub use consumer::{
    ProjectedReadiness, SensedProvider, SensedViability, SensingQuery, SensingSnapshot,
    SensingWatch, DEFAULT_PROVIDER_START_WITHIN, MAX_SENSED_POPULATION, POPULATION_RECONCILE_FLOOR,
};

/// The provider-side evaluator contract. A capability integration
/// implements [`ReadinessEvaluator`] and needs nothing else from this
/// module.
///
/// [`EvaluationRequest`] carries the evaluator's inputs as PUBLIC
/// FIELDS — `capability_id`, `constraints`, and `work_latency` — so an
/// implementation reads them directly. Their types belong to `net`'s
/// frozen evaluator contract and are deliberately not re-exported here;
/// an evaluator that only reads values (`request.constraints.get(..)`)
/// never needs to name them, and one that does can reach them through
/// `net` without this module growing an interest vocabulary.
pub use sensing::{CapabilityId, EvaluationRequest, ReadinessEvaluation, ReadinessEvaluator};

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

    /// This node has no caller-supplied durable identity, so it cannot
    /// be a provider: an origin signs its attestations with the node's
    /// entity key, and a generated ephemeral key makes both the
    /// consumer's trust-on-first-use pin and the persisted incarnation
    /// meaningless across a restart.
    #[error(
        "provider readiness needs a durable node identity — \
         build the mesh with MeshBuilder::identity(..) instead of the \
         generated ephemeral keypair"
    )]
    DurableIdentityRequired,

    /// This node can no longer issue a non-aliasing registration
    /// identity, so no further registration of any kind can be
    /// installed. Terminal and fail-closed: existing registrations keep
    /// serving and can still be closed, but nothing new installs,
    /// because reusing an identity would let a long-closed handle
    /// remove a live registration.
    #[error(
        "this node's readiness-registration identity space is exhausted — \
         no further provider registration can be installed on it"
    )]
    RegistrationIdentityExhausted,

    /// A [`SensingQuery`] with no capability name. The name is the
    /// interest's identity, so a blank one observes nothing.
    #[error(
        "a sensing query needs a capability name — \
         the same id the provider passes to SensingClient::provide"
    )]
    EmptyCapability,

    /// A zero end-to-end budget. No provider can satisfy
    /// `route_estimate + estimated_start <= 0`, so every observation
    /// would be reported as merely potential forever.
    #[error(
        "a zero end-to-end budget admits no provider — \
         pass the request's real deadline to SensingQuery::within, or \
         leave the budget unbounded"
    )]
    UnsatisfiableBudget,

    /// Observing readiness needs this node's OWN installed organization
    /// authority: the observation audience is derived from it, and
    /// there is no caller-supplied audience and no legacy fallback.
    /// Also reported when the captured authority view kept moving
    /// underneath the attempt — a retention publishes the view its
    /// population was derived under, or nothing.
    #[error(
        "observing capability readiness needs a live installed organization \
         authority on this node — adopt one (net_sdk::org::provision) and \
         install it before watching, and retry if it is being rotated"
    )]
    NoOrganizationAuthority,

    /// THIS node is not currently entitled to observe: no organization
    /// authority is installed, its revocation store is poisoned or
    /// generation-exhausted, or this node's OWN membership certificate
    /// has expired or been revoked below the current floor.
    ///
    /// A watch keeps its leases and its recovery state across this
    /// refusal — what it will not do is answer a new read with
    /// authorization it no longer holds.
    #[error(
        "this node is not currently entitled to observe capability readiness — \
         its own organization membership is absent, expired, revoked below the \
         current floor, or its authority view is unreadable; snapshots resume \
         once membership is valid again"
    )]
    ObserverNotQualified,

    /// A zero provider-start bound. No provider can attest that it will
    /// start within no time at all, so every observation would be
    /// `NotReady`.
    #[error(
        "a zero provider-start bound can never be satisfied — \
         pass the real bound to SensingQuery::start_within"
    )]
    UnsatisfiableStartBound,

    /// One observation root already retains as many capabilities as the
    /// core keeps per owner.
    ///
    /// Not a public watch-count bound: every watch mints its own root
    /// and retains exactly one capability, so a well-formed caller does
    /// not reach this. It is mapped rather than swallowed because the
    /// core bound is real and a silent refusal would be worse.
    #[error(
        "this observation root already retains the maximum number of \
         capabilities — the node's observation state is exhausted"
    )]
    WatchesAtCapacity,

    /// The node can no longer mint a demand-ownership identity, so no
    /// further observation can be established on it. Terminal:
    /// existing watches keep observing and can still be closed.
    #[error(
        "this node can no longer mint an observation identity — \
         no further capability watch can be established on it"
    )]
    ObservationIdentityUnavailable,

    /// The watch was closed (or its node is gone). A closed watch is
    /// inert by design: it reports no state and removes nothing.
    #[error("this capability watch is closed — open a new one with SensingClient::watch")]
    WatchClosed,
}

/// The provider-side sensing surface bound to one live node.
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
    /// The provider-side capability-sensing surface for this mesh.
    ///
    /// Every prerequisite this slice's surface actually has is checked
    /// here or at [`SensingClient::provide`], and every failure is a
    /// typed refusal rather than a silently dark plane:
    ///
    /// - [`SensingError::Disabled`] — the sensing plane is off (it ships
    ///   dark; turn it on with
    ///   [`MeshBuilder::enable_sensing`](crate::mesh::MeshBuilder::enable_sensing));
    /// - [`SensingError::DurableIdentityRequired`] — the node runs on a
    ///   generated ephemeral keypair, which no provider can sign
    ///   orderable readiness under;
    /// - [`SensingError::IncarnationRequired`] — checked at `provide`,
    ///   because it is specifically the origin role that needs the
    ///   persisted epoch.
    ///
    /// Absence of the sensing plane at BUILD time is not a runtime
    /// refusal at all: this whole module rides `feature = "net"`, so a
    /// build without it fails to compile at the call site rather than
    /// no-opping.
    ///
    /// # Why no node-authority refusal
    ///
    /// The plan's §4.5 authority refusal guards *owner-scoped* sensing.
    /// This slice exposes none: registering an evaluator names only a
    /// local capability id, carries no audience, and confers no
    /// authority. Whether a given consumer's interest may reach this
    /// provider at all is decided on the registration path, by
    /// `validate_subscriber_scope` and — for organization audiences —
    /// `verify_org_sensing_registration`, both of which run before any
    /// table row exists and neither of which consults this registry.
    /// The evaluator is read only after an admitted row produces a beat.
    /// Exact-provider acquisition is no longer hypothetical: the core
    /// authors an own-organization exact-provider lease from installed
    /// authority and refuses it with `OrgAudienceUnsupported` when no
    /// live membership can be captured. That refusal lives on the
    /// acquisition path, not in this provider registry, and none of it
    /// is exposed through this SDK surface.
    pub fn sensing(&self) -> Result<SensingClient, SensingError> {
        SensingClient::bind_node(self.node().clone())
    }
}

impl SensingClient {
    /// Bind the provider surface to a node handle, checking the
    /// prerequisites that hold for every operation on it.
    fn bind_node(node: Arc<MeshNode>) -> Result<Self, SensingError> {
        if !node.sensing_enabled() {
            return Err(SensingError::Disabled);
        }
        if !node.sensing_identity_is_durable() {
            return Err(SensingError::DurableIdentityRequired);
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
    /// - [`SensingError::RegistrationIdentityExhausted`] when the node
    ///   can no longer issue a non-aliasing registration identity.
    ///
    /// Every refusal is total: nothing is installed.
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
            .map_err(|refusal| install_refusal(refusal, &capability_id))?;
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
    /// [`Self::provide`], and the same
    /// [`SensingError::RegistrationIdentityExhausted`] terminal refusal
    /// — on which the incumbent is left serving, because superseding a
    /// live registration with an un-ownable one would be strictly worse
    /// than refusing.
    pub fn provide_replacing(
        &self,
        capability: impl Into<CapabilityId>,
        evaluator: Arc<dyn ReadinessEvaluator + Send + Sync>,
    ) -> Result<ReadinessRegistration, SensingError> {
        let capability_id = capability.into();
        self.require_origin()?;
        let registration_id = self
            .node
            .replace_readiness_evaluator(capability_id.clone(), evaluator)
            .map_err(|refusal| install_refusal(refusal, &capability_id))?;
        Ok(ReadinessRegistration::new(
            self.node.clone(),
            capability_id,
            registration_id,
        ))
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

/// Map a core install refusal onto the SDK's typed refusal.
///
/// The capability name comes from the caller's own argument, never from
/// the incumbent's registration — the core refusal deliberately carries
/// no id, so a loser learns nothing it could use to evict a winner.
fn install_refusal(
    refusal: sensing::EvaluatorInstallRefusal,
    capability_id: &CapabilityId,
) -> SensingError {
    match refusal {
        sensing::EvaluatorInstallRefusal::Occupied => SensingError::AlreadyProviding {
            capability: capability_id.as_str().to_string(),
        },
        sensing::EvaluatorInstallRefusal::IdentityExhausted => {
            SensingError::RegistrationIdentityExhausted
        }
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
    /// Returns whether any live observation path actually moved.
    /// `false` when nothing is watching this capability, and `false`
    /// whenever this handle no longer owns the capability's readiness —
    /// after its own `close`, and after a
    /// [`SensingClient::provide_replacing`] superseded it even if this
    /// handle is still open.
    ///
    /// Ownership is decided on the NODE, inside the same critical
    /// section as registration, replacement, and removal — not by the
    /// local `closed` flag, which cannot see a supersession it was never
    /// told about. So there is no check-then-poke window: once a
    /// replacement or close has returned, this handle can never move the
    /// successor's schedule.
    pub fn changed(&self) -> bool {
        if self.closed.load(Ordering::Acquire) {
            return false;
        }
        self.node
            .notify_sensing_state_changed_owned(&self.capability_id, self.registration_id)
    }

    /// Stop serving readiness for this capability.
    ///
    /// Returns whether THIS call removed the registration — so `true`
    /// at most once, and `false` for a repeat close, a close after
    /// drop, or a handle that was already superseded. Live watchers
    /// fall back to `Unknown` at their next beat; nothing fails.
    ///
    /// Once this returns, a readiness result the removed evaluator was
    /// already computing can no longer become the latest observation:
    /// the node's removal and the emitter's publication share one
    /// critical section.
    ///
    /// Idempotence is *structural*, not enforced by the local flag: the
    /// core removal is conditional on this handle's registration id, and
    /// an id is never issued twice, so a second attempt could only ever
    /// find someone else's row and refuse. The flag earns its place by
    /// keeping the repeat path (drop always follows an explicit close)
    /// off the node entirely.
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
        require::<SensingError>();
    }

    /// Refusals must be readable without a debugger — each carries the
    /// action the caller has to take, or says plainly that the state is
    /// terminal.
    #[test]
    fn every_refusal_names_the_remedy() {
        assert!(SensingError::Disabled
            .to_string()
            .contains("MeshBuilder::enable_sensing"));
        assert!(SensingError::IncarnationRequired
            .to_string()
            .contains("next_incarnation"));
        assert!(SensingError::DurableIdentityRequired
            .to_string()
            .contains("MeshBuilder::identity"));
        assert!(SensingError::AlreadyProviding {
            capability: "gpu.infer".into(),
        }
        .to_string()
        .contains("gpu.infer"));
        assert!(SensingError::RegistrationIdentityExhausted
            .to_string()
            .contains("exhausted"));
    }

    /// The core refusal maps onto exactly one SDK refusal each, and the
    /// capability name in the message comes from the CALLER's argument —
    /// never from the incumbent, which would leak ownership information
    /// to the loser.
    #[test]
    fn core_install_refusals_map_onto_distinct_sdk_refusals() {
        let capability = CapabilityId::new("gpu.infer");
        assert_eq!(
            install_refusal(sensing::EvaluatorInstallRefusal::Occupied, &capability),
            SensingError::AlreadyProviding {
                capability: "gpu.infer".to_string(),
            },
        );
        assert_eq!(
            install_refusal(
                sensing::EvaluatorInstallRefusal::IdentityExhausted,
                &capability
            ),
            SensingError::RegistrationIdentityExhausted,
        );
    }

    /// `provide` takes `impl Into<CapabilityId>` so a caller may write
    /// the name inline. That conversion must be the IDENTITY on the
    /// name — a `From` that trimmed, lowercased, or otherwise
    /// normalized would silently address a different capability than
    /// the one the caller wrote.
    #[test]
    fn a_capability_name_converts_verbatim() {
        let written: CapabilityId = "gpu.infer".into();
        assert_eq!(written, CapabilityId::new("gpu.infer"));
        assert_eq!(written.as_str(), "gpu.infer");

        let odd: CapabilityId = " Mixed.Case ".to_string().into();
        assert_eq!(odd.as_str(), " Mixed.Case ");
    }

    /// The SDK sensing surface is the PROVIDER lifecycle plus the
    /// own-organization EXACT-PROVIDER consumer observation — and
    /// nothing else. Interest, audience and wire vocabulary must stay
    /// out, and no provider-free / leader-backed / cross-organization
    /// selector may appear, because none of them is implemented.
    ///
    /// Non-vacuous by construction: it reads this module's OWN source
    /// and the consumer submodule's, so a re-export or a public method
    /// that leaks the forbidden vocabulary fails it, and it requires
    /// both halves of the shipped contract to be present, so the guard
    /// cannot pass by the surface having been emptied.
    #[test]
    fn the_public_surface_is_provider_lifecycle_plus_exact_consumer_observation() {
        let sources = [
            include_str!("sensing.rs"),
            include_str!("sensing/consumer.rs"),
        ];
        // WHOLE declarations, not first lines: every `pub use` statement
        // up to its `;`, and every public function signature — `pub fn`,
        // `pub async fn`, `pub const fn` — from its keyword to the `{`
        // or `;` that ends the signature. Prose and doc links that
        // legitimately NAME a deferred concept do not trip the guard,
        // while a type hidden on a continuation line, on an `async`
        // signature, or on a public constant cannot slip past it.
        //
        // The earlier revision scanned only the FIRST line starting
        // `pub fn`, so `pub async fn changed()` — a shipped method — and
        // any wrapped parameter or return type were invisible to it.
        let mut declarations = String::new();
        for source in sources {
            for after in source.split("pub use ").skip(1) {
                let statement = after.split(';').next().unwrap_or("");
                declarations.push_str(statement);
                declarations.push('\n');
            }
            for keyword in ["pub fn ", "pub async fn ", "pub const fn ", "pub const "] {
                for after in source.split(keyword).skip(1) {
                    let end = after
                        .find('{')
                        .into_iter()
                        .chain(after.find(';'))
                        .min()
                        .unwrap_or(after.len());
                    declarations.push_str(&after[..end]);
                    declarations.push('\n');
                }
            }
        }

        for forbidden in [
            // Interest, audience and wire vocabulary: the SDK owns the
            // lifecycle so applications never name any of it.
            "InterestSpec",
            "InterestRegistration",
            "AudienceScopeCommitment",
            "DisclosureClass",
            "ResultMode",
            "CanonicalConstraints",
            "WorkLatencyEnvelope",
            "ConsumerLatencyBudget",
            "CapabilityInterestKey",
            "ProviderInterestKey",
            "SensingLeaseKey",
            "SensingLeaseTicket",
            "Digest256",
            // Selectors and planes that do not exist above this slice.
            "ProviderSelector",
            "AnyAuthorized",
            "TagMatch",
            "GroupRef",
            "SensingLeader",
            // Core-internal ownership and projection containers.
            "OrgSensingFamily",
            "OrgSensedProjection",
            "OrgSensedRow",
        ] {
            assert!(
                !declarations.contains(forbidden),
                "`{forbidden}` is back in the SDK sensing surface — this surface \
                 is the provider lifecycle plus the own-organization \
                 exact-provider observation, and neither interest/wire \
                 vocabulary nor an unimplemented selector plane may appear on \
                 it without a separate authorization",
            );
        }

        // ...and BOTH halves of the shipped contract are present.
        for required in [
            // Provider lifecycle.
            "ReadinessEvaluator",
            "EvaluationRequest",
            "ReadinessEvaluation",
            "CapabilityId",
            "Incarnation",
            // Consumer observation.
            "SensingQuery",
            "SensingWatch",
            "SensingSnapshot",
            "SensedProvider",
            "SensedViability",
            "ProjectedReadiness",
        ] {
            assert!(
                declarations.contains(required),
                "the contract item `{required}` is missing from the surface",
            );
        }

        // ...and the CONSUMER docs must disclose the two facts a caller
        // cannot otherwise see: which bound the provider actually
        // evaluates, and that every read requalifies and clamps.
        let consumer_doc: String = sources[1]
            .lines()
            .take_while(|line| line.starts_with("//!") || line.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        for disclosed in [
            "PROVIDER-EVALUATED predicate",
            "SensingQuery::start_within",
            "ObserverNotQualified",
            "CLAMPED",
            "subset of\n//!   current visibility",
        ] {
            assert!(
                consumer_doc.contains(disclosed),
                "the consumer docs must disclose `{disclosed}` — the request's \
                 provider-evaluated bound and the per-read requalification are \
                 not inferable from the signatures",
            );
        }

        // ...and the DOCS must keep naming what is genuinely absent, so a
        // later slice cannot quietly imply it shipped. The declaration
        // scan above cannot see a stale or overreaching heading.
        let module_doc: String = sources[0]
            .lines()
            .take_while(|line| line.starts_with("//!") || line.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            module_doc.contains("OWN-ORGANIZATION EXACT PROVIDERS"),
            "the module docs must state the consumer surface's exact scope",
        );
        for absent in [
            "AnyAuthorized",
            "cross-organization sensing",
            "warmed pools",
        ] {
            assert!(
                module_doc.contains(absent),
                "the module docs must keep naming `{absent}` as absent",
            );
        }
        assert!(
            !module_doc.contains("deferred to S4"),
            "the module docs still say exact-provider acquisition is deferred: \
             the core implements it and this surface observes it",
        );
        assert!(
            module_doc.contains("OrgProviderRegistration"),
            "the module docs must record the implemented own-organization \
             exact-provider path instead of denying it",
        );

        // The crate root must describe the same boundary.
        let root = include_str!("lib.rs");
        let sensing_comment = root
            .split("pub use crate::sensing::{")
            .next()
            .and_then(|before| before.rsplit("// Capability-sensing").next())
            .expect("the sensing re-export comment must exist");
        assert!(
            sensing_comment.contains("own-organization exact-provider consumer observation"),
            "lib.rs must describe what the sensing re-exports actually are",
        );
        assert!(
            sensing_comment.contains("cross-organization selector"),
            "lib.rs must keep recording that no cross-organization or \
             provider-free selector is exposed",
        );
    }

    /// The witnesses in `sdk/tests/sensing_provider.rs` and
    /// `sdk/tests/sensing_consumer.rs` are race and timing proofs: a
    /// retry can only turn a real defect green — a lost wake, a
    /// superseded handle disturbing its successor, an edge that must
    /// arrive inside a bound. The nextest profile grants two retries by
    /// default, so both binaries MUST be in the zero-retry override.
    ///
    /// Guards the config rather than trusting it, because the override is
    /// a filter expression that fails silently when it stops matching.
    #[test]
    fn the_sensing_witness_binaries_are_excluded_from_retries() {
        let config = include_str!("../../.config/nextest.toml");
        let override_block = config
            .split("[[profile.default.overrides]]")
            .find(|block| block.contains("retries = 0"))
            .expect("a zero-retry override block must exist");
        for binary in ["sensing_provider", "sensing_consumer"] {
            assert!(
                override_block.contains(&format!("binary({binary})")),
                "sdk/tests/{binary}.rs must be in the zero-retry override — its \
                 ownership, wake and state-edge witnesses must not be retried \
                 into green",
            );
        }
    }
}
