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
//! **Provider lifecycle only.** There is deliberately no query surface,
//! no watch, no snapshot, and no readiness projection here.
//!
//! Exact-provider acquisition and projection are deferred to S4, and
//! the reason is concrete rather than a matter of sequencing: the core
//! still refuses every organization-audience exact-provider lease
//! (`SensingRegistrationError::OrgAudienceUnsupported`) because the
//! lease's wire leg emits legacy frames only, which an
//! organization-authoritative provider refuses. Until
//! organization-authenticated registration intake and
//! organization-audience exact leases are authorized, nothing in this
//! SDK could create the observations a projection would read, so
//! shipping a projection would ship a surface that can only ever answer
//! `Unknown` for the population it was built for.
//!
//! Nothing here exposes leader ids, audience commitments, interest
//! specs, provider selectors, wire digests, frames, private discovery
//! records, or retry/admission policy — and no operation here is
//! owner-scoped, so none of them is needed.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use net::adapter::net::behavior::sensing;
use net::adapter::net::MeshNode;

use crate::mesh::Mesh;

/// The provider-side evaluator contract. A capability integration
/// implements [`ReadinessEvaluator`] and needs nothing else: the
/// request's constraint and latency values are read through their own
/// methods, so the interest vocabulary stays out of this surface.
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
    /// When exact-provider acquisition arrives (S4), it is that surface
    /// that must carry the authority refusal.
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

    /// This module's public surface is provider lifecycle ONLY. The
    /// projection vocabulary the earlier candidate exposed — interest
    /// specs, audience-bearing types, provider selectors, result modes,
    /// budgets, projected readiness — must stay out, and no readiness
    /// projection may reappear here without a separate authorization.
    ///
    /// Non-vacuous by construction: it reads this module's own source,
    /// so a re-export or a projection method fails it.
    #[test]
    fn the_public_surface_of_this_module_is_provider_lifecycle_only() {
        let source = include_str!("sensing.rs");
        // Only the re-export statements and item declarations, so prose
        // and doc links that legitimately NAME a deferred concept do not
        // trip the guard.
        let declarations: String = source
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with("pub use ") || line.starts_with("pub fn "))
            .collect::<Vec<_>>()
            .join("\n");

        for forbidden in [
            "InterestSpec",
            "AudienceScopeCommitment",
            "ProviderSelector",
            "ResultMode",
            "DisclosureClass",
            "ConsumerLatencyBudget",
            "ProjectedReadiness",
            "CanonicalConstraints",
            "WorkLatencyEnvelope",
            "SensedProvider",
            "ExactProviderReadiness",
            "exact_provider_readiness",
        ] {
            assert!(
                !declarations.contains(forbidden),
                "`{forbidden}` is back in the SDK sensing surface — exact-provider \
                 acquisition and projection are deferred to S4, after \
                 organization-audience exact leases are authorized",
            );
        }

        // ...and the provider contract IS present, so the guard cannot
        // pass by the module having been emptied.
        for required in [
            "ReadinessEvaluator",
            "EvaluationRequest",
            "ReadinessEvaluation",
            "CapabilityId",
            "Incarnation",
        ] {
            assert!(
                declarations.contains(required),
                "the provider contract item `{required}` is missing from the surface",
            );
        }
    }

    /// The ownership and state-edge witnesses in
    /// `sdk/tests/sensing_provider.rs` are race and timing proofs: a
    /// retry can only turn a real defect green. The nextest profile
    /// grants two retries by default, so that binary MUST be in the
    /// zero-retry override.
    ///
    /// Guards the config rather than trusting it, because the override is
    /// a filter expression that fails silently when it stops matching.
    #[test]
    fn the_provider_witness_binary_is_excluded_from_retries() {
        let config = include_str!("../../.config/nextest.toml");
        let override_block = config
            .split("[[profile.default.overrides]]")
            .find(|block| block.contains("retries = 0"))
            .expect("a zero-retry override block must exist");
        assert!(
            override_block.contains("binary(sensing_provider)"),
            "sdk/tests/sensing_provider.rs must be in the zero-retry override — \
             its ownership and state-edge witnesses must not be retried into green",
        );
    }
}
