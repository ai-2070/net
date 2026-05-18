//! ICE — break-glass operator surface, substrate side per
//! [`DECK_SDK_PLAN.md`](../../../../../../docs/plans/DECK_SDK_PLAN.md)
//! Phase 2.
//!
//! Locked decision #4 of the plan: blast-radius simulation is
//! mandatory before any ICE commit. This module ships the
//! substrate-side simulator the Deck SDK's `IceProposal::simulate`
//! binds against. Every ICE proposal the SDK exposes routes
//! through [`simulate`] before the operator commits.
//!
//! # Surface
//!
//! - [`IceActionProposal`] — the substrate-stable enum of ICE
//!   actions the simulator understands. Mirrors what the Deck
//!   SDK's `IceCommands` builder will produce.
//! - [`BlastRadius`] — pre-execution preview: which nodes /
//!   replicas / daemons the action would touch + warnings.
//!   Serializable so the SDK can hand it across the FFI
//!   boundary unchanged.
//! - [`BlastWarning`] — operator-readable hints about non-obvious
//!   consequences (cluster-wide pause, in-flight resumption,
//!   placement reshuffle, …).
//! - [`simulate`] — pure function: snapshot + proposal →
//!   blast radius. No I/O, no side effects.
//!
//! # Scope (this slice)
//!
//! Phase 2 lands here in stages. This slice ships:
//!
//! - `FreezeCluster { ttl }` — affects every peer in the snapshot
//!   for the configured TTL.
//! - `ThawCluster` — clears any in-effect freeze.
//!
//! Future slices add `ForceDrain`, `ForceEvictReplica`,
//! `ForceRestartDaemon`, `ForceCutover`, `KillMigration`,
//! `FlushAvoidLists` alongside the [`super::event::AdminEvent`]
//! variants they map to.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::event::{AdminEvent, AvoidScope, ChainId, DaemonRef, MigrationId, NodeId};
use super::snapshot::MeshOsSnapshot;
use crate::adapter::net::identity::{EntityId, EntityKeypair};

/// Substrate-stable enumeration of ICE proposals the simulator
/// understands. The Deck SDK's `IceCommands` builder produces
/// one of these; the substrate verifier accepts the same form
/// at commit time (Phase 3, behind multi-operator-signing).
///
/// `#[non_exhaustive]` so later slices extend the surface
/// without breaking implementors.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum IceActionProposal {
    /// Pause cluster-wide reconcile output for `ttl`. Maps to
    /// [`super::event::AdminEvent::FreezeCluster`].
    FreezeCluster {
        /// Requested freeze duration.
        ttl: Duration,
    },
    /// Cancel an in-effect freeze. Maps to
    /// [`super::event::AdminEvent::ThawCluster`].
    ThawCluster,
    /// Flush avoid-list entries cluster-wide under the given
    /// [`AvoidScope`]. Maps to
    /// [`super::event::AdminEvent::FlushAvoidLists`].
    FlushAvoidLists {
        /// Flush scope — see [`AvoidScope`].
        scope: AvoidScope,
    },
    /// Force-evict `victim` from `chain` bypassing the
    /// scheduler's rebalance cooldown. Maps to
    /// [`super::event::AdminEvent::ForceEvictReplica`].
    ForceEvictReplica {
        /// Chain whose replica to evict.
        chain: ChainId,
        /// Holder to remove.
        victim: NodeId,
    },
    /// Reset `daemon`'s backoff so the supervisor's gate
    /// no longer suppresses restart. Maps to
    /// [`super::event::AdminEvent::ForceRestartDaemon`].
    ForceRestartDaemon {
        /// The daemon whose backoff to clear.
        daemon: DaemonRef,
    },
    /// Pin `chain` to be placed on `target`. Maps to
    /// [`super::event::AdminEvent::ForceCutover`].
    ForceCutover {
        /// Chain to pin.
        chain: ChainId,
        /// Operator-preferred holder.
        target: NodeId,
    },
    /// Abort an in-flight migration. Maps to
    /// [`super::event::AdminEvent::KillMigration`].
    KillMigration {
        /// The migration to abort.
        migration: MigrationId,
    },
}

impl IceActionProposal {
    /// Translate the proposal to its corresponding
    /// [`AdminEvent`]. The substrate folds the [`AdminEvent`];
    /// the proposal is the SDK-side builder + signing form.
    /// Stable lowercase discriminator the Deck SDK stamps on
    /// the returned [`super::super::deck::ChainCommit`]'s
    /// `event_kind` field. Cross-binding consumers branch on
    /// this string. Matches the corresponding
    /// [`AdminEvent`] variant's wire kind one-to-one so the
    /// audit ring's records show the same kind regardless of
    /// whether the commit rode the ICE proposal path or
    /// arrived as an unsigned admin event.
    pub fn kind(&self) -> &'static str {
        match self {
            IceActionProposal::FreezeCluster { .. } => "freeze_cluster",
            IceActionProposal::ThawCluster => "thaw_cluster",
            IceActionProposal::FlushAvoidLists { .. } => "flush_avoid_lists",
            IceActionProposal::ForceEvictReplica { .. } => "force_evict_replica",
            IceActionProposal::ForceRestartDaemon { .. } => "force_restart_daemon",
            IceActionProposal::ForceCutover { .. } => "force_cutover",
            IceActionProposal::KillMigration { .. } => "kill_migration",
        }
    }

    /// Translate the proposal to its corresponding
    /// [`AdminEvent`]. The substrate folds the `AdminEvent`;
    /// the proposal is the SDK-side builder + signing form.
    pub fn to_admin_event(&self) -> AdminEvent {
        match self {
            IceActionProposal::FreezeCluster { ttl } => AdminEvent::FreezeCluster { ttl: *ttl },
            IceActionProposal::ThawCluster => AdminEvent::ThawCluster,
            IceActionProposal::FlushAvoidLists { scope } => {
                AdminEvent::FlushAvoidLists { scope: *scope }
            }
            IceActionProposal::ForceEvictReplica { chain, victim } => {
                AdminEvent::ForceEvictReplica {
                    chain: *chain,
                    victim: *victim,
                }
            }
            IceActionProposal::ForceRestartDaemon { daemon } => AdminEvent::ForceRestartDaemon {
                daemon: daemon.clone(),
            },
            IceActionProposal::ForceCutover { chain, target } => AdminEvent::ForceCutover {
                chain: *chain,
                target: *target,
            },
            IceActionProposal::KillMigration { migration } => AdminEvent::KillMigration {
                migration: *migration,
            },
        }
    }
}

/// Operator signature over an [`IceActionProposal`]. Carries
/// the issuing operator's id plus a 64-byte ed25519 signature
/// over [`ice_proposal_signing_payload`]'s deterministic
/// postcard encoding. The substrate verifier
/// ([`OperatorRegistry::verify`]) re-checks the bundle on the
/// loop side of every [`super::event::MeshOsEvent::SignedIceCommit`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OperatorSignature {
    /// Issuing operator's id (Deck SDK's
    /// `OperatorIdentity::operator_id`).
    pub operator_id: u64,
    /// 64-byte ed25519 signature over
    /// [`ice_proposal_signing_payload`].
    pub signature: Vec<u8>,
}

/// Domain-separation tag the substrate verifier and the SDK
/// signer both prepend to ICE-proposal signing payloads. Isolates
/// the ICE signature domain from every other postcard-encoded
/// surface in the codebase so a signature captured over an ICE
/// proposal can't be replayed against a different surface whose
/// postcard encoding happens to coincide.
pub const ICE_SIGNING_DOMAIN: &[u8] = b"net.meshos.ice.v1\0";

/// Domain-separation tag for single-signature ordinary-admin
/// signing payloads. Distinct from [`ICE_SIGNING_DOMAIN`] so a
/// signature collected for an ICE bundle can't be reused as an
/// ordinary admin commit and vice versa, even when the inner
/// `AdminEvent` payloads happen to encode identically.
pub const ADMIN_SIGNING_DOMAIN: &[u8] = b"net.meshos.admin.v1\0";

/// Maximum acceptable age (issuer-stamped → verifier-observed)
/// of a signed commit envelope. Bundles older than this window
/// are rejected with [`VerifyError::EnvelopeExpired`] regardless
/// of cryptographic validity — closes the replay window the raw
/// signature otherwise leaves open until operator-key rotation.
pub const DEFAULT_SIGNING_FRESHNESS_WINDOW: Duration = Duration::from_secs(300);

/// Tolerance for a verifier whose wall-clock lags the issuer's.
/// Bundles stamped more than this far in the future relative to
/// the verifier's clock fail with [`VerifyError::EnvelopeFromFuture`].
/// Defends against accidentally-issued-tomorrow bundles while
/// staying loose enough to absorb normal NTP drift.
pub const DEFAULT_SIGNING_FUTURE_SKEW: Duration = Duration::from_secs(30);

/// Plan-default per-target ICE cooldown window. After an ICE
/// force-operation commits against a node (or cluster-wide), a
/// subsequent ICE commit against the same target inside this
/// window fails with [`VerifyError::IceCooldownActive`]. Forces
/// operators to wait between break-glass operations on the same
/// node so a typo can't bounce a daemon into a thrash loop.
pub const DEFAULT_ICE_COOLDOWN_WINDOW: Duration = Duration::from_secs(300);

/// Helper — current wall-clock milliseconds since `UNIX_EPOCH`.
/// Used as the issuer-side `issued_at_ms` stamp by the SDK and
/// as the verifier-side `now_ms` reference on the loop. Returns
/// `0` if the system clock predates `UNIX_EPOCH`, which is a
/// pathological case but matches the same fallback the audit
/// ring uses for `committed_at_ms`.
pub fn now_ms_since_unix_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Length of a [`BlastRadiusHash`] — blake3's standard 32-byte
/// digest width. Pinned so cross-language bindings agree on the
/// wire shape without depending on the blake3 crate exporting
/// the constant.
pub const BLAST_RADIUS_HASH_LEN: usize = 32;

/// Fixed-width digest of a [`BlastRadius`] the SDK observed
/// during `simulate()`. The signing envelope binds the bundle's
/// signatures to this hash so a commit constructed without a
/// prior simulation has no valid hash to sign over — the
/// substrate's enforcement of locked decision #4
/// ("simulate before commit") at the cryptographic layer
/// instead of the honor-system gate the SDK alone provides.
pub type BlastRadiusHash = [u8; BLAST_RADIUS_HASH_LEN];

/// Sentinel hash that means "no simulation was performed."
/// `IceProposal::commit` refuses to construct a signing
/// envelope with this hash, and the substrate verifier rejects
/// any `SignedIceCommit` whose `blast_hash` equals this value
/// with [`VerifyError::SimulationRequired`]. Distinguishes
/// "simulation skipped" from "simulation produced a valid but
/// empty blast radius" — even a no-op proposal hashes to
/// something non-zero because the postcard encoding carries
/// type tags.
pub const SIMULATION_REQUIRED_SENTINEL: BlastRadiusHash = [0u8; BLAST_RADIUS_HASH_LEN];

/// Hash a [`BlastRadius`] for inclusion in the ICE signing
/// envelope. Deterministic over the postcard encoding so the
/// SDK signer and the substrate verifier produce identical
/// bytes for any equal `BlastRadius`. Returns
/// [`SIMULATION_REQUIRED_SENTINEL`] only for the unreachable
/// case of a postcard encoding failure (the enums are
/// composed of types whose encoding is infallible).
pub fn blast_radius_hash(blast: &BlastRadius) -> BlastRadiusHash {
    let bytes =
        postcard::to_allocvec(blast).expect("postcard encoding of BlastRadius is infallible");
    blake3::hash(&bytes).into()
}

/// Deterministic signing payload for an ICE proposal envelope.
/// Layout: `ICE_SIGNING_DOMAIN || issued_at_ms (little-endian
/// u64) || blast_hash (32 bytes) || postcard(proposal)`. The
/// domain tag isolates the signature domain; `issued_at_ms`
/// binds the signature to a freshness window the verifier
/// enforces; `blast_hash` binds the signature to the
/// simulator's pre-execution preview so a commit without prior
/// simulation has no valid hash to sign over.
///
/// Every binding (Rust today; Python / Node / Go / C in
/// follow-up slices) reproduces this exact byte sequence — the
/// substrate verifier rebuilds it locally on every
/// [`super::event::MeshOsEvent::SignedIceCommit`].
pub fn ice_proposal_signing_payload(
    proposal: &IceActionProposal,
    issued_at_ms: u64,
    blast_hash: &BlastRadiusHash,
) -> Vec<u8> {
    let inner = postcard::to_allocvec(proposal)
        .expect("postcard encoding of IceActionProposal is infallible");
    let mut buf =
        Vec::with_capacity(ICE_SIGNING_DOMAIN.len() + 8 + BLAST_RADIUS_HASH_LEN + inner.len());
    buf.extend_from_slice(ICE_SIGNING_DOMAIN);
    buf.extend_from_slice(&issued_at_ms.to_le_bytes());
    buf.extend_from_slice(blast_hash);
    buf.extend_from_slice(&inner);
    buf
}

/// Deterministic signing payload for an ordinary-admin
/// signed-commit envelope. Same shape as
/// [`ice_proposal_signing_payload`] but with
/// [`ADMIN_SIGNING_DOMAIN`] and the postcard encoding of the
/// inner [`AdminEvent`]. The distinct domain tag is what stops
/// an ICE signature from cross-validating as an admin commit.
pub fn admin_event_signing_payload(event: &AdminEvent, issued_at_ms: u64) -> Vec<u8> {
    let inner =
        postcard::to_allocvec(event).expect("postcard encoding of AdminEvent is infallible");
    let mut buf = Vec::with_capacity(ADMIN_SIGNING_DOMAIN.len() + 8 + inner.len());
    buf.extend_from_slice(ADMIN_SIGNING_DOMAIN);
    buf.extend_from_slice(&issued_at_ms.to_le_bytes());
    buf.extend_from_slice(&inner);
    buf
}

impl OperatorSignature {
    /// Sign `proposal` with `keypair`'s ed25519 secret, stamped
    /// at `issued_at_ms` (milliseconds since `UNIX_EPOCH`), and
    /// bound to the simulator's pre-execution preview hash
    /// `blast_hash`. The 64-byte signature covers
    /// [`ice_proposal_signing_payload(proposal, issued_at_ms, blast_hash)`],
    /// so every collaborating operator signing the same
    /// `(proposal, issued_at_ms, blast_hash)` triple produces
    /// bit-identical inputs the substrate verifier can re-check.
    ///
    /// The `blast_hash` binding gives the substrate enforcement
    /// of locked decision #4 — a commit without a prior
    /// simulation has no valid hash to sign over, so the
    /// signatures fail verification. Callers obtain the hash
    /// via [`blast_radius_hash`] applied to the
    /// [`BlastRadius`] returned by `IceProposal::simulate`.
    /// Coordinators collecting signatures from multiple
    /// operators MUST share the same `(issued_at_ms, blast_hash)`
    /// pair across the bundle, otherwise the per-operator
    /// payloads diverge.
    ///
    /// Panics on a public-only keypair — callers that may hold
    /// one should guard with `EntityKeypair::is_read_only`.
    pub fn sign(
        keypair: &EntityKeypair,
        proposal: &IceActionProposal,
        issued_at_ms: u64,
        blast_hash: &BlastRadiusHash,
    ) -> Self {
        let payload = ice_proposal_signing_payload(proposal, issued_at_ms, blast_hash);
        let sig = keypair.sign(&payload);
        Self {
            operator_id: keypair.origin_hash(),
            signature: sig.to_bytes().to_vec(),
        }
    }

    /// Sign an ordinary-admin `event` with `keypair`'s ed25519
    /// secret stamped at `issued_at_ms`. Covers
    /// [`admin_event_signing_payload(event, issued_at_ms)`] —
    /// the admin-side counterpart of [`Self::sign`]. The
    /// distinct [`ADMIN_SIGNING_DOMAIN`] tag is what stops an
    /// ICE-signed bundle from cross-validating as an admin
    /// commit.
    pub fn sign_admin(keypair: &EntityKeypair, event: &AdminEvent, issued_at_ms: u64) -> Self {
        let payload = admin_event_signing_payload(event, issued_at_ms);
        let sig = keypair.sign(&payload);
        Self {
            operator_id: keypair.origin_hash(),
            signature: sig.to_bytes().to_vec(),
        }
    }

    /// Fallible companion to [`Self::sign`]. Returns `None`
    /// when `keypair` is read-only (public-only). UIs that
    /// might hold a read-only operator-identity (e.g. a
    /// secondary client viewing the cluster without the
    /// authority to sign) should reach for this instead of
    /// the panicking [`Self::sign`].
    pub fn try_sign(
        keypair: &EntityKeypair,
        proposal: &IceActionProposal,
        issued_at_ms: u64,
        blast_hash: &BlastRadiusHash,
    ) -> Option<Self> {
        if keypair.is_read_only() {
            return None;
        }
        Some(Self::sign(keypair, proposal, issued_at_ms, blast_hash))
    }

    /// Fallible companion to [`Self::sign_admin`]. Returns
    /// `None` on a read-only keypair. Same UX contract as
    /// [`Self::try_sign`].
    pub fn try_sign_admin(
        keypair: &EntityKeypair,
        event: &AdminEvent,
        issued_at_ms: u64,
    ) -> Option<Self> {
        if keypair.is_read_only() {
            return None;
        }
        Some(Self::sign_admin(keypair, event, issued_at_ms))
    }
}

/// Operator-key registry. Maps each operator id to the public
/// key the substrate uses to validate that operator's
/// signatures. Shared between the SDK-side gate (Deck SDK's
/// `IceProposal::commit`) and the substrate-side loop verifier
/// (`MeshOsLoop::with_admin_verifier`) so a malicious SDK can't
/// bypass verification.
#[derive(Clone, Debug, Default)]
pub struct OperatorRegistry {
    keys: std::collections::BTreeMap<u64, EntityId>,
}

impl OperatorRegistry {
    /// Empty registry. Every verify against this registry
    /// returns `not_authorized` until at least one operator is
    /// inserted.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an operator's public key. Subsequent
    /// [`Self::verify`] calls for `operator_id` resolve against
    /// this entry.
    pub fn insert(&mut self, operator_id: u64, public_key: EntityId) {
        self.keys.insert(operator_id, public_key);
    }

    /// Convenience: register `keypair`'s public key under its
    /// derived operator id (the keypair's `origin_hash`).
    pub fn register(&mut self, keypair: &EntityKeypair) {
        self.insert(keypair.origin_hash(), keypair.entity_id().clone());
    }

    /// `true` iff `operator_id` is registered.
    pub fn contains(&self, operator_id: u64) -> bool {
        self.keys.contains_key(&operator_id)
    }

    /// Number of registered operators.
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// `true` iff no operators are registered.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Verify `signature` against the registered public key for
    /// `signature.operator_id` over `payload`. Returns
    /// [`VerifyError::NotAuthorized`] for an unknown operator,
    /// [`VerifyError::InvalidSignature`] for a malformed or
    /// tampered signature.
    pub fn verify(&self, signature: &OperatorSignature, payload: &[u8]) -> Result<(), VerifyError> {
        let entity_id =
            self.keys
                .get(&signature.operator_id)
                .ok_or(VerifyError::NotAuthorized {
                    operator_id: signature.operator_id,
                })?;
        let sig_bytes: &[u8; 64] = signature.signature.as_slice().try_into().map_err(|_| {
            VerifyError::InvalidSignature {
                operator_id: signature.operator_id,
                reason: format!(
                    "signature is not 64 bytes (got {})",
                    signature.signature.len()
                ),
            }
        })?;
        let ed_sig = ed25519_dalek::Signature::from_bytes(sig_bytes);
        entity_id
            .verify(payload, &ed_sig)
            .map_err(|_| VerifyError::InvalidSignature {
                operator_id: signature.operator_id,
                reason: "signature failed verification against the registered public key".into(),
            })
    }

    /// Verify every signature in `signatures` against `payload`
    /// and confirm at least `threshold` *distinct* operator ids
    /// signed it. Fails fast on the first verification error.
    ///
    /// The distinct-operator check is the load-bearing
    /// guarantee of the M-of-N gate: a bundle of `[sig_A, sig_A]`
    /// from a single operator must not satisfy a threshold of 2
    /// even though both signatures verify. `got` on the
    /// `InsufficientSignatures` error reports the number of
    /// unique operators, not the raw signature count, so the
    /// operator UI surfaces the actual shortfall.
    pub fn verify_bundle(
        &self,
        signatures: &[OperatorSignature],
        payload: &[u8],
        threshold: usize,
    ) -> Result<(), VerifyError> {
        // Cap the bundle so a `MeshOsEvent` arriving with a huge
        // `signatures` vec (e.g. across a future process boundary)
        // can't pin the verifier's CPU for arbitrarily long.
        // 64 is well past any realistic operator-quorum and matches
        // the umbrella shape of "tiny-but-finite operational caps."
        // Honor the configured threshold when it exceeds 64 so a
        // legitimately-large operator quorum doesn't fail-closed
        // against the verifier's own cap — the cap is meant to
        // bound CPU against hostile bundles, not against the
        // configured policy.
        const MAX_SIGNATURES_PER_BUNDLE: usize = 64;
        let max_signatures = MAX_SIGNATURES_PER_BUNDLE.max(threshold);
        if signatures.len() > max_signatures {
            return Err(VerifyError::InsufficientSignatures {
                got: signatures.len(),
                required: threshold,
            });
        }
        let mut unique_operators: std::collections::BTreeSet<u64> =
            std::collections::BTreeSet::new();
        for sig in signatures {
            self.verify(sig, payload)?;
            unique_operators.insert(sig.operator_id);
        }
        if unique_operators.len() < threshold {
            return Err(VerifyError::InsufficientSignatures {
                got: unique_operators.len(),
                required: threshold,
            });
        }
        Ok(())
    }
}

/// Substrate-side ICE verification error. The Deck SDK maps each
/// variant to its `<<deck-sdk-kind:KIND>>MSG` envelope.
#[derive(Clone, Debug, thiserror::Error)]
pub enum VerifyError {
    /// The operator id on the signature isn't registered with
    /// the cluster's operator policy.
    #[error("operator {operator_id} is not registered in the cluster's operator policy")]
    NotAuthorized {
        /// Issuing operator id from the rejected signature.
        operator_id: u64,
    },
    /// The signature is malformed, tampered, or signed a
    /// different payload than the one verified.
    #[error("operator {operator_id} signature invalid: {reason}")]
    InvalidSignature {
        /// Issuing operator id from the rejected signature.
        operator_id: u64,
        /// Diagnostic detail.
        reason: String,
    },
    /// The bundle carried fewer *distinct operators* than the
    /// cluster's configured threshold. `got` reports unique
    /// signers, not raw signature count — a bundle of
    /// `[sig_A, sig_A]` against a `threshold = 2` registers
    /// `got = 1`.
    #[error("insufficient signatures: got {got}, required {required}")]
    InsufficientSignatures {
        /// Number of *distinct* operator ids whose signatures
        /// verified.
        got: usize,
        /// Minimum required by the cluster's policy.
        required: usize,
    },
    /// The signed envelope was stamped earlier than
    /// `max_age_ms` before the verifier's reference clock. Closes
    /// the replay window the raw signature alone leaves open:
    /// captured bundles fail this check once the freshness
    /// window has rolled past.
    #[error(
        "signed envelope expired — issued {issued_at_ms} ms, verified at {now_ms} ms, max age {max_age_ms} ms"
    )]
    EnvelopeExpired {
        /// Issuer-stamped milliseconds since `UNIX_EPOCH`.
        issued_at_ms: u64,
        /// Verifier-observed milliseconds since `UNIX_EPOCH`.
        now_ms: u64,
        /// Configured freshness window in milliseconds.
        max_age_ms: u64,
    },
    /// The signed envelope was stamped further than
    /// `future_skew_ms` ahead of the verifier's reference clock.
    /// Defends against accidentally-issued-tomorrow bundles
    /// while tolerating normal NTP drift up to the skew window.
    #[error(
        "signed envelope is from the future — issued {issued_at_ms} ms, verified at {now_ms} ms, future-skew tolerance {future_skew_ms} ms"
    )]
    EnvelopeFromFuture {
        /// Issuer-stamped milliseconds since `UNIX_EPOCH`.
        issued_at_ms: u64,
        /// Verifier-observed milliseconds since `UNIX_EPOCH`.
        now_ms: u64,
        /// Configured future-skew tolerance in milliseconds.
        future_skew_ms: u64,
    },
    /// The signed envelope carried the
    /// [`SIMULATION_REQUIRED_SENTINEL`] blast-radius hash,
    /// indicating no [`simulate`] preview ran before the
    /// commit. Substrate enforcement of locked decision #4 — a
    /// commit constructed without prior simulation has no valid
    /// hash to sign over, so the verifier rejects regardless of
    /// the signatures' cryptographic validity.
    #[error("ICE commit rejected: simulate() must precede commit() (blast-radius hash is the simulation-required sentinel)")]
    SimulationRequired,
    /// An ICE commit landed inside the cooldown window of a
    /// previous ICE commit against the same target. Forces a
    /// pause between break-glass operations on the same node so
    /// an operator can't bounce a daemon into a thrash loop.
    /// `node` is the target whose cooldown is active; `None`
    /// means a cluster-wide cooldown (the most recent ICE was
    /// cluster-scope like `FreezeCluster`).
    #[error(
        "ICE cooldown active: target {node:?} blocked until {expires_at_ms} ms (verifier observed {now_ms} ms)"
    )]
    IceCooldownActive {
        /// Target node whose cooldown is active, or `None` for
        /// the cluster-wide cooldown.
        node: Option<NodeId>,
        /// When the cooldown lifts (milliseconds since
        /// `UNIX_EPOCH`).
        expires_at_ms: u64,
        /// Verifier-observed milliseconds since `UNIX_EPOCH`.
        now_ms: u64,
    },
}

impl VerifyError {
    /// Stable lowercase kind discriminator the Deck SDK +
    /// cross-language consumers branch on.
    pub fn kind(&self) -> &'static str {
        match self {
            VerifyError::NotAuthorized { .. } => "not_authorized",
            VerifyError::InvalidSignature { .. } => "signature_invalid",
            VerifyError::InsufficientSignatures { .. } => "insufficient_signatures",
            VerifyError::EnvelopeExpired { .. } => "envelope_expired",
            VerifyError::EnvelopeFromFuture { .. } => "envelope_from_future",
            VerifyError::SimulationRequired => "simulation_required",
            VerifyError::IceCooldownActive { .. } => "ice_cooldown_active",
        }
    }
}

/// Per-target cooldown state the [`AdminVerifier`] tracks
/// between successful ICE commits. Each verifier instance has
/// its own state (per-node — every node's loop runs its own
/// verifier). The cooldown applies between ICE commits *only*;
/// ordinary signed-admin commits don't trigger it.
#[derive(Debug, Default)]
struct IceCooldownState {
    /// `node_id → expires_at_ms`. Cooldowns expire by
    /// wall-clock; entries past `now_ms` are pruned lazily on
    /// each check.
    per_node: std::collections::BTreeMap<NodeId, u64>,
    /// Active cluster-wide cooldown (e.g. after
    /// `FreezeCluster`). `None` when no cluster-wide cooldown
    /// is in effect.
    cluster_wide_until_ms: Option<u64>,
}

/// Target enumeration the cooldown gate uses to decide which
/// cooldown bucket to check / write. Cluster-wide proposals
/// share a single bucket; per-node proposals use the
/// `per_node` map.
fn cooldown_targets(proposal: &IceActionProposal) -> CooldownTargets {
    match proposal {
        IceActionProposal::FreezeCluster { .. } | IceActionProposal::ThawCluster => {
            CooldownTargets::ClusterWide
        }
        IceActionProposal::FlushAvoidLists { scope } => match scope {
            AvoidScope::Local { node } => CooldownTargets::PerNode(vec![*node]),
            AvoidScope::OnPeer { peer } => CooldownTargets::PerNode(vec![*peer]),
            AvoidScope::Global => CooldownTargets::ClusterWide,
        },
        IceActionProposal::ForceEvictReplica { victim, .. } => {
            CooldownTargets::PerNode(vec![*victim])
        }
        IceActionProposal::ForceCutover { target, .. } => CooldownTargets::PerNode(vec![*target]),
        // Daemon-id and migration-id proposals don't carry a
        // node binding; treat as cluster-wide so a thrash-loop
        // operator still hits the gate.
        IceActionProposal::ForceRestartDaemon { .. } | IceActionProposal::KillMigration { .. } => {
            CooldownTargets::ClusterWide
        }
    }
}

#[derive(Debug)]
enum CooldownTargets {
    ClusterWide,
    PerNode(Vec<NodeId>),
}

/// Default cap on the per-node admin audit ring. Records older
/// than this drop FIFO so the substrate's audit buffer stays
/// fixed-overhead under churn. Sized for "a few minutes of
/// operator activity" rather than "complete history" — the
/// canonical replay path is the eventual admin audit subchain;
/// this ring is the in-memory snapshot-side surface the Deck
/// SDK reads against until the subchain ships.
pub const DEFAULT_MAX_ADMIN_AUDIT_RECORDS: usize = 256;

/// Outcome the substrate recorded for an attempted admin
/// commit. Verified attempts (signed ICE bundles or future
/// signed-ordinary-admin commits) surface as `Accepted` /
/// `Rejected`; unsigned commits surface as `Unverified`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum VerificationOutcome {
    /// The verifier accepted the signature(s); the inner
    /// `AdminEvent` folded normally.
    Accepted,
    /// The verifier rejected the bundle. `kind` carries the
    /// stable [`VerifyError::kind`] discriminator; `message`
    /// is the operator-readable detail.
    Rejected {
        /// Discriminator the cross-language SDKs branch on
        /// (`not_authorized`, `signature_invalid`,
        /// `insufficient_signatures`).
        kind: String,
        /// Operator-readable detail.
        message: String,
    },
    /// The commit rode the unsigned path. Either no verifier
    /// is installed, or the event arrived via the legacy
    /// `MeshOsEvent::AdminEvent(...)` channel that doesn't
    /// carry a signature. Surfaces so security review can
    /// distinguish "verified" from "no verification path
    /// available."
    Unverified,
}

/// One entry on the substrate's admin audit ring. The
/// [`super::event_loop::MeshOsLoop`] records one of these per
/// admin commit it observes — whether the commit rode the
/// signed [`super::event::MeshOsEvent::SignedIceCommit`] path
/// or the unsigned `MeshOsEvent::AdminEvent(...)` path.
///
/// Carries the operator ids (not the full 64-byte signature
/// bytes) because the audit consumer doesn't need the
/// cryptographic material — just "who claimed authorship of
/// this commit." The list is empty for unsigned commits.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AdminAuditRecord {
    /// Monotonic per-runtime sequence number. Strictly
    /// increasing across the runtime's lifetime — the Deck
    /// SDK's audit-tail stream uses this to dedup across
    /// snapshot polls without depending on
    /// `committed_at_ms` (which can collide when two commits
    /// arrive in the same millisecond).
    pub seq: u64,
    /// Wall-clock milliseconds since `UNIX_EPOCH` at the
    /// moment the loop received the commit. Distinct from
    /// `Instant`-based timing the rest of the loop uses so
    /// audit consumers don't need a reference instant.
    pub committed_at_ms: u64,
    /// The admin event the loop folded (or rejected). Carries
    /// the full wire form so audit consumers can render the
    /// specific operator command without a second lookup.
    pub event: super::event::AdminEvent,
    /// Issuing operator ids from the commit's signatures.
    /// Empty for unsigned commits.
    pub operator_ids: Vec<u64>,
    /// The verifier's outcome for this attempt.
    pub outcome: VerificationOutcome,
    /// `true` when this entry is in the in-memory ring but the
    /// durable chain append failed. Chain consumers replaying
    /// the chain after a restart will not see entries with this
    /// flag — they have to consult the ring directly (which only
    /// holds a bounded recent window). Default `false` so older
    /// serialized records that pre-date the field deserialize
    /// cleanly.
    #[serde(default)]
    pub chain_pending: bool,
}

/// Substrate-side ICE admin verifier — bundles a shared
/// [`OperatorRegistry`] with the cluster's signature threshold
/// and the freshness window used to reject stale envelopes.
/// Installed on [`super::event_loop::MeshOsLoop`] via
/// `with_admin_verifier`; the loop runs every
/// [`super::event::MeshOsEvent::SignedIceCommit`] through
/// [`Self::verify_commit`] before folding the inner
/// [`AdminEvent`].
#[derive(Clone, Debug)]
pub struct AdminVerifier {
    registry: std::sync::Arc<OperatorRegistry>,
    threshold: usize,
    freshness_window: Duration,
    future_skew: Duration,
    ice_cooldown: Duration,
    ice_state: std::sync::Arc<std::sync::Mutex<IceCooldownState>>,
}

impl AdminVerifier {
    /// Build a verifier with `threshold` minimum signatures and
    /// the default freshness window
    /// ([`DEFAULT_SIGNING_FRESHNESS_WINDOW`]) /
    /// future-skew tolerance ([`DEFAULT_SIGNING_FUTURE_SKEW`]) /
    /// ICE cooldown ([`DEFAULT_ICE_COOLDOWN_WINDOW`]).
    /// `threshold = 0` is clamped to `1` — no admin path should
    /// ever accept an empty signature bundle.
    pub fn new(registry: std::sync::Arc<OperatorRegistry>, threshold: usize) -> Self {
        Self::with_freshness(
            registry,
            threshold,
            DEFAULT_SIGNING_FRESHNESS_WINDOW,
            DEFAULT_SIGNING_FUTURE_SKEW,
        )
    }

    /// Build a verifier with explicit freshness + future-skew
    /// windows and the default ICE cooldown. Clusters with
    /// longer multi-operator signature-collection workflows
    /// (or known-large clock skew) can widen via this
    /// constructor.
    pub fn with_freshness(
        registry: std::sync::Arc<OperatorRegistry>,
        threshold: usize,
        freshness_window: Duration,
        future_skew: Duration,
    ) -> Self {
        Self::with_full_policy(
            registry,
            threshold,
            freshness_window,
            future_skew,
            DEFAULT_ICE_COOLDOWN_WINDOW,
        )
    }

    /// Build a verifier with every policy knob explicit.
    /// Primarily for tests that need a short cooldown window;
    /// production callers should prefer [`Self::new`].
    pub fn with_full_policy(
        registry: std::sync::Arc<OperatorRegistry>,
        threshold: usize,
        freshness_window: Duration,
        future_skew: Duration,
        ice_cooldown: Duration,
    ) -> Self {
        Self {
            registry,
            threshold: threshold.max(1),
            freshness_window,
            future_skew,
            ice_cooldown,
            ice_state: std::sync::Arc::new(std::sync::Mutex::new(IceCooldownState::default())),
        }
    }

    /// Borrow the underlying registry.
    pub fn registry(&self) -> &OperatorRegistry {
        &self.registry
    }

    /// Configured minimum-signature threshold.
    pub fn threshold(&self) -> usize {
        self.threshold
    }

    /// Configured freshness window. Envelopes stamped earlier
    /// than `now_ms - freshness_window` fail with
    /// [`VerifyError::EnvelopeExpired`].
    pub fn freshness_window(&self) -> Duration {
        self.freshness_window
    }

    /// Configured future-skew tolerance.
    pub fn future_skew(&self) -> Duration {
        self.future_skew
    }

    /// Configured ICE cooldown window.
    pub fn ice_cooldown(&self) -> Duration {
        self.ice_cooldown
    }

    /// Verify a `SignedIceCommit`-style bundle against
    /// `proposal` stamped at `issued_at_ms` and bound to
    /// `blast_hash`. Rejects the
    /// [`SIMULATION_REQUIRED_SENTINEL`] hash before any
    /// signature math (substrate enforcement of locked
    /// decision #4), checks freshness relative to `now_ms`,
    /// checks per-target ICE cooldown, then verifies every
    /// signature in the bundle against the domain-tagged
    /// signing payload, and finally checks the
    /// distinct-operator count against the configured
    /// threshold. On success, advances the cooldown clock for
    /// each affected target so subsequent ICE commits inside
    /// the window fail with
    /// [`VerifyError::IceCooldownActive`]. Computes the
    /// signing payload internally so the loop never needs to
    /// recompute it on the hot path.
    pub fn verify_commit(
        &self,
        proposal: &IceActionProposal,
        signatures: &[OperatorSignature],
        issued_at_ms: u64,
        blast_hash: &BlastRadiusHash,
        now_ms: u64,
    ) -> Result<(), VerifyError> {
        if *blast_hash == SIMULATION_REQUIRED_SENTINEL {
            return Err(VerifyError::SimulationRequired);
        }
        self.check_freshness(issued_at_ms, now_ms)?;
        let targets = cooldown_targets(proposal);
        self.check_ice_cooldown(&targets, now_ms)?;
        let payload = ice_proposal_signing_payload(proposal, issued_at_ms, blast_hash);
        self.registry
            .verify_bundle(signatures, &payload, self.threshold)?;
        self.record_ice_cooldown(&targets, now_ms);
        Ok(())
    }

    fn check_ice_cooldown(
        &self,
        targets: &CooldownTargets,
        now_ms: u64,
    ) -> Result<(), VerifyError> {
        // Lock failures here mean a previous holder panicked
        // mid-update — treat poison as "no active cooldown" so
        // the verifier doesn't wedge after a panic; the
        // recover-from-poison branch overwrites with fresh
        // state on the next successful commit.
        let state = match self.ice_state.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        match targets {
            CooldownTargets::ClusterWide => {
                if let Some(expires_at_ms) = state.cluster_wide_until_ms {
                    if expires_at_ms > now_ms {
                        return Err(VerifyError::IceCooldownActive {
                            node: None,
                            expires_at_ms,
                            now_ms,
                        });
                    }
                }
            }
            CooldownTargets::PerNode(nodes) => {
                for node in nodes {
                    if let Some(expires_at_ms) = state.per_node.get(node) {
                        if *expires_at_ms > now_ms {
                            return Err(VerifyError::IceCooldownActive {
                                node: Some(*node),
                                expires_at_ms: *expires_at_ms,
                                now_ms,
                            });
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn record_ice_cooldown(&self, targets: &CooldownTargets, now_ms: u64) {
        let expires_at_ms = now_ms.saturating_add(self.ice_cooldown.as_millis() as u64);
        let mut state = match self.ice_state.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        match targets {
            CooldownTargets::ClusterWide => {
                state.cluster_wide_until_ms = Some(expires_at_ms);
            }
            CooldownTargets::PerNode(nodes) => {
                for node in nodes {
                    state.per_node.insert(*node, expires_at_ms);
                }
            }
        }
        // Prune entries that have already expired so the map
        // doesn't grow unbounded with stale nodes the operator
        // never targets again.
        state
            .per_node
            .retain(|_, expires_at_ms| *expires_at_ms > now_ms);
        if let Some(cluster_expires) = state.cluster_wide_until_ms {
            if cluster_expires <= now_ms {
                state.cluster_wide_until_ms = None;
            }
        }
    }

    /// Verify a single-signature ordinary-admin commit against
    /// `event` stamped at `issued_at_ms`. Single-operator path —
    /// `signature.operator_id` must be registered, and the
    /// signature must cover
    /// [`admin_event_signing_payload(event, issued_at_ms)`]. The
    /// ICE `threshold` doesn't apply here because ordinary admin
    /// commits are single-operator by design; only the ICE
    /// surface uses the M-of-N threshold.
    pub fn verify_admin_commit(
        &self,
        event: &AdminEvent,
        signature: &OperatorSignature,
        issued_at_ms: u64,
        now_ms: u64,
    ) -> Result<(), VerifyError> {
        self.check_freshness(issued_at_ms, now_ms)?;
        let payload = admin_event_signing_payload(event, issued_at_ms);
        self.registry.verify(signature, &payload)
    }

    fn check_freshness(&self, issued_at_ms: u64, now_ms: u64) -> Result<(), VerifyError> {
        if issued_at_ms > now_ms {
            let drift_ms = issued_at_ms - now_ms;
            if drift_ms > self.future_skew.as_millis() as u64 {
                return Err(VerifyError::EnvelopeFromFuture {
                    issued_at_ms,
                    now_ms,
                    future_skew_ms: self.future_skew.as_millis() as u64,
                });
            }
            return Ok(());
        }
        let age_ms = now_ms - issued_at_ms;
        if age_ms > self.freshness_window.as_millis() as u64 {
            return Err(VerifyError::EnvelopeExpired {
                issued_at_ms,
                now_ms,
                max_age_ms: self.freshness_window.as_millis() as u64,
            });
        }
        Ok(())
    }
}

/// Pre-execution preview of an ICE action's effect. The Deck
/// SDK surfaces this from `IceProposal::simulate()`; Deck-the-
/// binary renders it as a confirmation prompt before commit.
///
/// Every field is `Serialize + Deserialize` so cross-language
/// bindings round-trip the wire form unchanged.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct BlastRadius {
    /// Nodes that would observe the action — every peer for
    /// cluster-wide actions like `FreezeCluster`, the targeted
    /// node for single-target actions.
    pub affected_nodes: Vec<NodeId>,
    /// Replica chains whose holder set would shift. Empty for
    /// actions that don't move replicas.
    pub affected_replicas: Vec<ChainId>,
    /// Daemons whose lifecycle would shift. Empty for actions
    /// that don't restart / stop daemons.
    pub affected_daemons: Vec<DaemonRef>,
    /// How long the operator should expect the action's
    /// downstream effects to take. For `FreezeCluster` this is
    /// the TTL itself; for drain-style actions this estimates
    /// the wait until the drain completes.
    pub estimated_drain_delay: Option<Duration>,
    /// Heuristic placement-churn estimate in `[0.0, 1.0]`.
    /// `0.0` = no placement disturbance; `1.0` = full
    /// re-distribution. Cluster-wide pause actions report
    /// `0.0` (no placement decisions execute during a freeze).
    pub placement_stability_delta: f32,
    /// Non-fatal hints about consequences the simulator can
    /// foresee but doesn't gate on.
    pub warnings: Vec<BlastWarning>,
}

/// Stable lowercase discriminator for [`BlastRadius`] warnings.
/// Cross-language SDKs match on the variant name; Deck-the-
/// binary renders them with operator-facing messages.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum BlastWarning {
    /// A freeze suppresses every reconcile-driven action — drains,
    /// rebalances, daemon restarts — until the TTL elapses or an
    /// explicit thaw fires.
    ClusterFreezeBlocksOperatorActions,
    /// Thawing a frozen cluster resumes whatever reconcile
    /// transitions were paused mid-flight.
    ThawResumesPendingReconciles,
    /// Thaw issued while no freeze is in effect — no-op.
    ThawHasNoFreezeToCancel,
    /// `FlushAvoidLists::Global` is the heaviest scope —
    /// reconcile will re-emit `MarkAvoid` for any peer that
    /// still meets the degraded-RTT threshold on the next
    /// tick, so the operator should not expect lasting effect
    /// without addressing the underlying RTT cause first.
    GlobalAvoidFlushMayReEmit,
    /// `FlushAvoidLists::Local` targets the operator's own
    /// node only; other nodes ignore the event. Surfaces so
    /// the operator confirms the scope choice matches intent.
    AvoidFlushLocalToTargetNodeOnly,
    /// `FlushAvoidLists::OnPeer` un-avoids the targeted peer
    /// cluster-wide. Carries the peer id so the operator UI
    /// can render "every node will stop avoiding peer X."
    AvoidFlushRecoversPeer {
        /// The peer the flush is un-avoiding cluster-wide.
        peer: NodeId,
    },
    /// `ForceEvictReplica` bypasses the scheduler's rebalance
    /// cooldown for the targeted chain. Operator gets one
    /// eviction now; the cooldown still applies to subsequent
    /// scheduler-driven rebalances of the same chain.
    ForcedEvictionBypassesCooldown {
        /// Chain the force-evict targets.
        chain: ChainId,
        /// Holder being evicted.
        victim: NodeId,
    },
    /// `ForceEvictReplica` referenced a chain that doesn't
    /// appear in the snapshot — substrate accepts the admin
    /// commit, but no node will fire a leader-side eviction
    /// because the leader entry is missing. Surfaces here so
    /// the operator UI flags "this proposal will be a no-op."
    ForcedEvictionTargetsUnknownChain {
        /// The chain id the operator targeted.
        chain: ChainId,
    },
    /// `ForceEvictReplica`'s victim isn't currently observed
    /// as a holder of the chain. The commit still folds and
    /// produces a leader-side eviction action, but the holder
    /// set won't change — the action becomes a no-op at the
    /// dispatcher.
    ForcedEvictionTargetsNonHolder {
        /// The chain id the operator targeted.
        chain: ChainId,
        /// The holder the operator targeted.
        victim: NodeId,
    },
    /// `ForceRestartDaemon` bypasses the supervisor's
    /// `BackingOff` / `CrashLooping` gate so the daemon gets
    /// an immediate retry. Surface so the operator confirms
    /// the underlying cause has been addressed before bouncing
    /// the daemon back into the same crash loop.
    ForcedRestartBypassesBackoff {
        /// The targeted daemon's id.
        daemon_id: u64,
    },
    /// `ForceRestartDaemon` referenced a daemon not currently
    /// observed in the snapshot. The fold still removes any
    /// stale `applied_backoffs` entry, but reconcile won't
    /// emit `StartDaemon` because there's no `DaemonStatus`
    /// entry to track. Operator likely typed the wrong id.
    ForcedRestartTargetsUnknownDaemon {
        /// The daemon id the operator targeted.
        daemon_id: u64,
    },
    /// `ForceRestartDaemon` targeted a daemon whose tracker
    /// is already `Idle`. The commit is a no-op — the operator
    /// might be confused about the daemon's actual state.
    ForcedRestartDaemonNotInBackoff {
        /// The targeted daemon's id.
        daemon_id: u64,
    },
    /// `ForceCutover` bypasses the placement scorer for the
    /// targeted chain. The chain ends up pinned to the target;
    /// the count-driven arm may rebalance if the chain is
    /// now over-replicated.
    ForcedCutoverBypassesPlacementScorer {
        /// Chain the cutover pins.
        chain: ChainId,
        /// Operator's preferred holder.
        target: NodeId,
    },
    /// `ForceCutover` targeted a chain that doesn't appear in
    /// the snapshot. The commit folds but no node will fire
    /// the leader-side placement because the leader entry is
    /// missing.
    ForcedCutoverTargetsUnknownChain {
        /// Chain id the operator targeted.
        chain: ChainId,
    },
    /// `ForceCutover`'s target is already a holder of the
    /// chain — the commit folds and the leader emits a
    /// placement action, but the holder set is unchanged. The
    /// action becomes a dispatcher-side no-op.
    ForcedCutoverTargetAlreadyHolder {
        /// Chain id the operator targeted.
        chain: ChainId,
        /// Target that's already a holder.
        target: NodeId,
    },
    /// `KillMigration` simulation is best-effort across the
    /// cluster: every node folds the chain commit but only the
    /// node hosting the migration's
    /// [`super::migration_aborter::MigrationAborter`] actually
    /// aborts. The simulator only sees the local snapshot's
    /// `in_flight_migrations`, so it can't tell whether the
    /// targeted migration exists on another node — the warning
    /// stays so the operator UI flags the cross-node visibility
    /// limit.
    KillMigrationDispatcherIntegrationPending {
        /// The migration id the operator targeted.
        migration: MigrationId,
    },
}

/// Pure simulator: snapshot + proposal → blast radius. No I/O;
/// safe to call from any thread. The Deck SDK invokes this on
/// the runtime's latest snapshot when an operator clicks "preview"
/// on an ICE action.
///
/// # Semantics — `affected_*` vs the reconcile arm
///
/// The simulator reports **cluster-wide effects** of the
/// proposal — which nodes / replicas / daemons would observe
/// state change once the admin event lands. This is distinct
/// from what
/// [`super::reconcile::reconcile`] emits on any single node:
/// reconcile is leader-gated (only the chain leader emits a
/// `RequestEviction` / `RequestPlacement` action), and the
/// blast-radius preview at operator side has no notion of
/// "which node am I." The two views are consistent: the
/// leader's reconcile produces an action that, when
/// dispatched, has the cluster-wide effect the simulator
/// reports. The
/// `simulator_blast_radius_matches_reconcile_emission_*`
/// regression tests pin that consistency for the chain-mutating
/// proposals (ForceEvictReplica, ForceCutover).
pub fn simulate(snapshot: &MeshOsSnapshot, proposal: &IceActionProposal) -> BlastRadius {
    match proposal {
        IceActionProposal::FreezeCluster { ttl } => simulate_freeze(snapshot, *ttl),
        IceActionProposal::ThawCluster => simulate_thaw(snapshot),
        IceActionProposal::FlushAvoidLists { scope } => {
            simulate_flush_avoid_lists(snapshot, *scope)
        }
        IceActionProposal::ForceEvictReplica { chain, victim } => {
            simulate_force_evict_replica(snapshot, *chain, *victim)
        }
        IceActionProposal::ForceRestartDaemon { daemon } => {
            simulate_force_restart_daemon(snapshot, daemon)
        }
        IceActionProposal::ForceCutover { chain, target } => {
            simulate_force_cutover(snapshot, *chain, *target)
        }
        IceActionProposal::KillMigration { migration } => {
            simulate_kill_migration(snapshot, *migration)
        }
    }
}

fn simulate_freeze(snapshot: &MeshOsSnapshot, ttl: Duration) -> BlastRadius {
    // Every peer the snapshot knows about would observe the
    // freeze. The set comes from the snapshot's peer keys; the
    // local node isn't a peer of itself, so for visibility we
    // include peers only — Deck-the-binary renders the local
    // node separately.
    let mut affected_nodes: Vec<NodeId> = snapshot.peers.keys().copied().collect();
    affected_nodes.sort();
    BlastRadius {
        affected_nodes,
        affected_replicas: Vec::new(),
        affected_daemons: Vec::new(),
        // The downstream effect of a freeze is "nothing happens
        // for `ttl`"; surface `ttl` here so the operator sees
        // the pause window in the preview UI.
        estimated_drain_delay: Some(ttl),
        placement_stability_delta: 0.0,
        warnings: vec![BlastWarning::ClusterFreezeBlocksOperatorActions],
    }
}

fn simulate_flush_avoid_lists(snapshot: &MeshOsSnapshot, scope: AvoidScope) -> BlastRadius {
    let mut affected_nodes: Vec<NodeId> = snapshot.peers.keys().copied().collect();
    affected_nodes.sort();
    match scope {
        AvoidScope::Local { node } => BlastRadius {
            // Only the targeted node folds the event; other
            // nodes see the chain entry but skip the fold.
            affected_nodes: vec![node],
            affected_replicas: Vec::new(),
            affected_daemons: Vec::new(),
            estimated_drain_delay: None,
            placement_stability_delta: 0.0,
            warnings: vec![BlastWarning::AvoidFlushLocalToTargetNodeOnly],
        },
        AvoidScope::OnPeer { peer } => BlastRadius {
            // Every peer in the cluster folds the event (each
            // removes `peer` from its own avoid list).
            affected_nodes,
            affected_replicas: Vec::new(),
            affected_daemons: Vec::new(),
            estimated_drain_delay: None,
            // Un-avoiding a peer changes which nodes reconcile
            // will consider for placement. Surface as a small
            // non-zero churn estimate without committing to an
            // exact value.
            placement_stability_delta: 0.05,
            warnings: vec![BlastWarning::AvoidFlushRecoversPeer { peer }],
        },
        AvoidScope::Global => BlastRadius {
            affected_nodes,
            affected_replicas: Vec::new(),
            affected_daemons: Vec::new(),
            estimated_drain_delay: None,
            placement_stability_delta: 0.1,
            warnings: vec![BlastWarning::GlobalAvoidFlushMayReEmit],
        },
    }
}

fn simulate_force_evict_replica(
    snapshot: &MeshOsSnapshot,
    chain: ChainId,
    victim: NodeId,
) -> BlastRadius {
    let mut warnings = vec![BlastWarning::ForcedEvictionBypassesCooldown { chain, victim }];
    let replica = snapshot.replicas.get(&chain);
    if replica.is_none() {
        warnings.push(BlastWarning::ForcedEvictionTargetsUnknownChain { chain });
    } else if let Some(snap) = replica {
        if !snap.holders.contains(&victim) {
            warnings.push(BlastWarning::ForcedEvictionTargetsNonHolder { chain, victim });
        }
    }
    BlastRadius {
        affected_nodes: vec![victim],
        affected_replicas: vec![chain],
        affected_daemons: Vec::new(),
        estimated_drain_delay: None,
        // Eviction always disturbs placement; surface a small
        // but visible signal so the operator UI flags the
        // change.
        placement_stability_delta: 0.15,
        warnings,
    }
}

fn simulate_force_restart_daemon(snapshot: &MeshOsSnapshot, daemon: &DaemonRef) -> BlastRadius {
    let mut warnings = vec![BlastWarning::ForcedRestartBypassesBackoff {
        daemon_id: daemon.id,
    }];
    match snapshot.daemons.get(&daemon.id) {
        None => warnings.push(BlastWarning::ForcedRestartTargetsUnknownDaemon {
            daemon_id: daemon.id,
        }),
        Some(snap) => {
            if matches!(
                snap.restart_state,
                super::snapshot::RestartStateSnapshot::Idle
            ) {
                warnings.push(BlastWarning::ForcedRestartDaemonNotInBackoff {
                    daemon_id: daemon.id,
                });
            }
        }
    }
    BlastRadius {
        affected_nodes: Vec::new(),
        affected_replicas: Vec::new(),
        affected_daemons: vec![daemon.clone()],
        estimated_drain_delay: None,
        placement_stability_delta: 0.0,
        warnings,
    }
}

fn simulate_force_cutover(
    snapshot: &MeshOsSnapshot,
    chain: ChainId,
    target: NodeId,
) -> BlastRadius {
    let mut warnings = vec![BlastWarning::ForcedCutoverBypassesPlacementScorer { chain, target }];
    match snapshot.replicas.get(&chain) {
        None => warnings.push(BlastWarning::ForcedCutoverTargetsUnknownChain { chain }),
        Some(snap) => {
            if snap.holders.contains(&target) {
                warnings.push(BlastWarning::ForcedCutoverTargetAlreadyHolder { chain, target });
            }
        }
    }
    BlastRadius {
        affected_nodes: vec![target],
        affected_replicas: vec![chain],
        affected_daemons: Vec::new(),
        estimated_drain_delay: None,
        // Pinning a holder changes placement; surface a non-
        // zero signal so the operator UI flags the change.
        placement_stability_delta: 0.15,
        warnings,
    }
}

fn simulate_kill_migration(snapshot: &MeshOsSnapshot, migration: MigrationId) -> BlastRadius {
    // The simulator runs against the local snapshot, so it
    // can only enumerate migrations this node hosts. The
    // warning stays in place because every node folds the
    // chain commit but only the migration's host node
    // actually aborts — the simulator can't see other
    // nodes' orchestrators.
    // The orchestrator's list returns the daemon_origin (which
    // is the MigrationId by construction) but doesn't carry the
    // daemon's name, so the simulator emits a DaemonRef with an
    // empty name. Deck-the-binary's preview UI joins against the
    // snapshot's `daemons` map by id to fill the label.
    let affected_daemons = match snapshot
        .in_flight_migrations
        .iter()
        .find(|m| m.daemon_origin == migration)
    {
        Some(_) => vec![super::event::DaemonRef {
            id: migration,
            name: String::new(),
        }],
        None => Vec::new(),
    };
    BlastRadius {
        affected_nodes: Vec::new(),
        affected_replicas: Vec::new(),
        affected_daemons,
        estimated_drain_delay: None,
        placement_stability_delta: 0.0,
        warnings: vec![BlastWarning::KillMigrationDispatcherIntegrationPending { migration }],
    }
}

fn simulate_thaw(snapshot: &MeshOsSnapshot) -> BlastRadius {
    let warning = if snapshot.freeze_remaining_ms.is_some() {
        BlastWarning::ThawResumesPendingReconciles
    } else {
        BlastWarning::ThawHasNoFreezeToCancel
    };
    BlastRadius {
        affected_nodes: Vec::new(),
        affected_replicas: Vec::new(),
        affected_daemons: Vec::new(),
        estimated_drain_delay: None,
        placement_stability_delta: 0.0,
        warnings: vec![warning],
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::meshos::snapshot::PeerSnapshot;

    fn snapshot_with_peers(peers: &[NodeId]) -> MeshOsSnapshot {
        let mut snap = MeshOsSnapshot::default();
        for peer in peers {
            snap.peers.insert(*peer, PeerSnapshot::default());
        }
        snap
    }

    #[test]
    fn freeze_against_empty_snapshot_reports_no_affected_nodes() {
        let snap = MeshOsSnapshot::default();
        let blast = simulate(
            &snap,
            &IceActionProposal::FreezeCluster {
                ttl: Duration::from_secs(30),
            },
        );
        assert!(blast.affected_nodes.is_empty());
        assert_eq!(blast.estimated_drain_delay, Some(Duration::from_secs(30)));
        assert_eq!(
            blast.warnings,
            vec![BlastWarning::ClusterFreezeBlocksOperatorActions]
        );
    }

    #[test]
    fn freeze_against_three_peers_reports_all_three_sorted() {
        let snap = snapshot_with_peers(&[30, 10, 20]);
        let blast = simulate(
            &snap,
            &IceActionProposal::FreezeCluster {
                ttl: Duration::from_secs(60),
            },
        );
        assert_eq!(blast.affected_nodes, vec![10, 20, 30]);
        assert_eq!(blast.estimated_drain_delay, Some(Duration::from_secs(60)));
        // Cluster-wide pause; no placement decisions execute
        // during the window.
        assert_eq!(blast.placement_stability_delta, 0.0);
        // No daemons / replicas are touched directly — the freeze
        // gates the reconcile output, not the underlying state.
        assert!(blast.affected_replicas.is_empty());
        assert!(blast.affected_daemons.is_empty());
    }

    #[test]
    fn thaw_against_frozen_snapshot_warns_pending_reconciles_resume() {
        let mut snap = MeshOsSnapshot::default();
        snap.freeze_remaining_ms = Some(15_000);
        let blast = simulate(&snap, &IceActionProposal::ThawCluster);
        assert_eq!(
            blast.warnings,
            vec![BlastWarning::ThawResumesPendingReconciles]
        );
        assert!(blast.affected_nodes.is_empty());
        assert_eq!(blast.estimated_drain_delay, None);
    }

    #[test]
    fn thaw_against_unfrozen_snapshot_warns_no_op() {
        let snap = MeshOsSnapshot::default();
        let blast = simulate(&snap, &IceActionProposal::ThawCluster);
        assert_eq!(blast.warnings, vec![BlastWarning::ThawHasNoFreezeToCancel]);
    }

    #[test]
    fn blast_radius_postcard_round_trip_preserves_every_field() {
        // Wire-stability pin: the SDK and bindings deserialize
        // this exact shape. Round-trip every field so a future
        // refactor can't silently change the form.
        let blast = BlastRadius {
            affected_nodes: vec![1, 2, 3],
            affected_replicas: vec![100, 200],
            affected_daemons: vec![DaemonRef {
                id: 7,
                name: "telemetry".into(),
            }],
            estimated_drain_delay: Some(Duration::from_secs(45)),
            placement_stability_delta: 0.25,
            warnings: vec![
                BlastWarning::ClusterFreezeBlocksOperatorActions,
                BlastWarning::ThawResumesPendingReconciles,
            ],
        };
        let bytes = postcard::to_allocvec(&blast).expect("encode");
        let decoded: BlastRadius = postcard::from_bytes(&bytes).expect("decode");
        assert_eq!(decoded, blast);
    }

    #[test]
    fn blast_radius_json_round_trip_preserves_every_field() {
        let blast = BlastRadius {
            affected_nodes: vec![42],
            affected_replicas: Vec::new(),
            affected_daemons: Vec::new(),
            estimated_drain_delay: Some(Duration::from_millis(2_500)),
            placement_stability_delta: 0.0,
            warnings: vec![BlastWarning::ClusterFreezeBlocksOperatorActions],
        };
        let json = serde_json::to_string(&blast).expect("encode");
        let decoded: BlastRadius = serde_json::from_str(&json).expect("decode");
        assert_eq!(decoded, blast);
    }

    #[test]
    fn ice_action_proposal_postcard_round_trips_both_variants() {
        for proposal in [
            IceActionProposal::FreezeCluster {
                ttl: Duration::from_secs(90),
            },
            IceActionProposal::ThawCluster,
        ] {
            let bytes = postcard::to_allocvec(&proposal).expect("encode");
            let decoded: IceActionProposal = postcard::from_bytes(&bytes).expect("decode");
            assert_eq!(decoded, proposal);
        }
    }

    #[test]
    fn operator_signature_signs_and_verifies_round_trip() {
        let kp = EntityKeypair::generate();
        let mut registry = OperatorRegistry::new();
        registry.register(&kp);
        let proposal = IceActionProposal::FreezeCluster {
            ttl: Duration::from_secs(30),
        };
        let issued_at_ms = now_ms_since_unix_epoch();
        let blast = simulate(&MeshOsSnapshot::default(), &proposal);
        let blast_hash = blast_radius_hash(&blast);
        let sig = OperatorSignature::sign(&kp, &proposal, issued_at_ms, &blast_hash);
        let payload = ice_proposal_signing_payload(&proposal, issued_at_ms, &blast_hash);
        registry.verify(&sig, &payload).expect("valid signature");
    }

    #[test]
    fn operator_registry_rejects_unknown_operator_via_substrate_path() {
        let kp = EntityKeypair::generate();
        let registry = OperatorRegistry::new(); // empty
        let proposal = IceActionProposal::ThawCluster;
        let issued_at_ms = now_ms_since_unix_epoch();
        let blast_hash: BlastRadiusHash = [1u8; BLAST_RADIUS_HASH_LEN];
        let sig = OperatorSignature::sign(&kp, &proposal, issued_at_ms, &blast_hash);
        let payload = ice_proposal_signing_payload(&proposal, issued_at_ms, &blast_hash);
        let err = registry.verify(&sig, &payload).unwrap_err();
        assert!(matches!(err, VerifyError::NotAuthorized { .. }));
        assert_eq!(err.kind(), "not_authorized");
    }

    #[test]
    fn admin_verifier_clamps_zero_threshold_to_one() {
        let registry = std::sync::Arc::new(OperatorRegistry::new());
        let verifier = AdminVerifier::new(registry, 0);
        assert_eq!(verifier.threshold(), 1);
    }

    #[test]
    fn admin_verifier_returns_insufficient_signatures_for_empty_bundle() {
        let kp = EntityKeypair::generate();
        let mut registry = OperatorRegistry::new();
        registry.register(&kp);
        let verifier = AdminVerifier::new(std::sync::Arc::new(registry), 2);
        let proposal = IceActionProposal::ThawCluster;
        let issued_at_ms = now_ms_since_unix_epoch();
        let blast_hash: BlastRadiusHash = [1u8; BLAST_RADIUS_HASH_LEN];
        let sig = OperatorSignature::sign(&kp, &proposal, issued_at_ms, &blast_hash);
        let err = verifier
            .verify_commit(&proposal, &[sig], issued_at_ms, &blast_hash, issued_at_ms)
            .unwrap_err();
        assert!(matches!(
            err,
            VerifyError::InsufficientSignatures {
                got: 1,
                required: 2
            }
        ));
    }

    #[test]
    fn admin_verifier_rejects_duplicate_signatures_from_same_operator() {
        // A single operator signing the same proposal twice
        // must not satisfy a multi-op threshold even though
        // both signatures verify. Without operator-id dedup
        // this would silently pass M-of-N — the headline
        // guarantee of the entire ICE surface.
        let kp = EntityKeypair::generate();
        let mut registry = OperatorRegistry::new();
        registry.register(&kp);
        let verifier = AdminVerifier::new(std::sync::Arc::new(registry), 2);
        let proposal = IceActionProposal::ThawCluster;
        let issued_at_ms = now_ms_since_unix_epoch();
        let blast_hash: BlastRadiusHash = [1u8; BLAST_RADIUS_HASH_LEN];
        let sig = OperatorSignature::sign(&kp, &proposal, issued_at_ms, &blast_hash);
        let bundle = [sig.clone(), sig];
        let err = verifier
            .verify_commit(&proposal, &bundle, issued_at_ms, &blast_hash, issued_at_ms)
            .unwrap_err();
        assert!(
            matches!(
                err,
                VerifyError::InsufficientSignatures {
                    got: 1,
                    required: 2
                }
            ),
            "expected InsufficientSignatures {{ got: 1, required: 2 }}, got {err:?}"
        );
    }

    #[test]
    fn admin_verifier_accepts_two_distinct_operators_at_threshold_two() {
        // The positive counterpart of the dedup test — two
        // distinct operators clear the threshold.
        let kp_a = EntityKeypair::generate();
        let kp_b = EntityKeypair::generate();
        let mut registry = OperatorRegistry::new();
        registry.register(&kp_a);
        registry.register(&kp_b);
        let verifier = AdminVerifier::new(std::sync::Arc::new(registry), 2);
        let proposal = IceActionProposal::ThawCluster;
        let issued_at_ms = now_ms_since_unix_epoch();
        let blast_hash: BlastRadiusHash = [1u8; BLAST_RADIUS_HASH_LEN];
        let bundle = [
            OperatorSignature::sign(&kp_a, &proposal, issued_at_ms, &blast_hash),
            OperatorSignature::sign(&kp_b, &proposal, issued_at_ms, &blast_hash),
        ];
        verifier
            .verify_commit(&proposal, &bundle, issued_at_ms, &blast_hash, issued_at_ms)
            .expect("two distinct operators with valid signatures should satisfy threshold = 2");
    }

    #[test]
    fn admin_verifier_rejects_simulation_required_sentinel_blast_hash() {
        // Substrate enforcement of locked decision #4: a commit
        // carrying the SIMULATION_REQUIRED_SENTINEL hash means
        // the SDK skipped simulate(). The verifier rejects
        // BEFORE checking signatures so a commit without a real
        // simulation can never succeed regardless of how many
        // operators co-signed the sentinel.
        let kp = EntityKeypair::generate();
        let mut registry = OperatorRegistry::new();
        registry.register(&kp);
        let verifier = AdminVerifier::new(std::sync::Arc::new(registry), 1);
        let proposal = IceActionProposal::ThawCluster;
        let issued_at_ms = now_ms_since_unix_epoch();
        let sentinel = SIMULATION_REQUIRED_SENTINEL;
        let sig = OperatorSignature::sign(&kp, &proposal, issued_at_ms, &sentinel);
        let err = verifier
            .verify_commit(&proposal, &[sig], issued_at_ms, &sentinel, issued_at_ms)
            .unwrap_err();
        assert_eq!(err.kind(), "simulation_required");
    }

    #[test]
    fn blast_radius_hash_is_deterministic_across_equal_inputs() {
        // Multi-operator signing requires every operator to
        // compute the same hash from the same BlastRadius — the
        // signing payload bytes diverge otherwise. Confirms the
        // hash is stable over equal inputs.
        let blast_a = BlastRadius {
            affected_nodes: vec![1, 2, 3],
            affected_replicas: vec![10],
            affected_daemons: Vec::new(),
            estimated_drain_delay: Some(Duration::from_secs(15)),
            placement_stability_delta: 0.5,
            warnings: vec![BlastWarning::ClusterFreezeBlocksOperatorActions],
        };
        let blast_b = blast_a.clone();
        assert_eq!(blast_radius_hash(&blast_a), blast_radius_hash(&blast_b));
        // Different inputs must produce different hashes.
        let blast_c = BlastRadius {
            affected_nodes: vec![4, 5, 6],
            ..blast_a
        };
        assert_ne!(blast_radius_hash(&blast_b), blast_radius_hash(&blast_c));
        // The deterministic encoding never produces the
        // simulation-required sentinel by accident.
        assert_ne!(blast_radius_hash(&blast_b), SIMULATION_REQUIRED_SENTINEL);
    }

    #[test]
    fn ice_proposal_to_admin_event_maps_freeze_cluster() {
        let proposal = IceActionProposal::FreezeCluster {
            ttl: Duration::from_secs(45),
        };
        assert!(matches!(
            proposal.to_admin_event(),
            AdminEvent::FreezeCluster { ttl } if ttl == Duration::from_secs(45)
        ));
    }

    #[test]
    fn ice_proposal_to_admin_event_maps_thaw_cluster() {
        assert!(matches!(
            IceActionProposal::ThawCluster.to_admin_event(),
            AdminEvent::ThawCluster
        ));
    }

    #[test]
    fn simulate_flush_avoid_lists_local_targets_one_node() {
        let snap = snapshot_with_peers(&[10, 20, 30]);
        let blast = simulate(
            &snap,
            &IceActionProposal::FlushAvoidLists {
                scope: AvoidScope::Local { node: 42 },
            },
        );
        assert_eq!(blast.affected_nodes, vec![42]);
        assert!(blast
            .warnings
            .iter()
            .any(|w| matches!(w, BlastWarning::AvoidFlushLocalToTargetNodeOnly)));
    }

    #[test]
    fn simulate_flush_avoid_lists_on_peer_covers_every_peer_with_warning() {
        let snap = snapshot_with_peers(&[10, 20, 30]);
        let blast = simulate(
            &snap,
            &IceActionProposal::FlushAvoidLists {
                scope: AvoidScope::OnPeer { peer: 20 },
            },
        );
        assert_eq!(blast.affected_nodes, vec![10, 20, 30]);
        assert!(blast
            .warnings
            .iter()
            .any(|w| matches!(w, BlastWarning::AvoidFlushRecoversPeer { peer: 20 })));
        // Small but non-zero churn signal.
        assert!(blast.placement_stability_delta > 0.0);
    }

    #[test]
    fn simulate_flush_avoid_lists_global_carries_re_emit_warning() {
        let snap = snapshot_with_peers(&[1, 2, 3]);
        let blast = simulate(
            &snap,
            &IceActionProposal::FlushAvoidLists {
                scope: AvoidScope::Global,
            },
        );
        assert_eq!(blast.affected_nodes, vec![1, 2, 3]);
        assert!(blast
            .warnings
            .iter()
            .any(|w| matches!(w, BlastWarning::GlobalAvoidFlushMayReEmit)));
    }

    #[test]
    fn ice_proposal_to_admin_event_maps_flush_avoid_lists() {
        for scope in [
            AvoidScope::Local { node: 42 },
            AvoidScope::OnPeer { peer: 7 },
            AvoidScope::Global,
        ] {
            let proposal = IceActionProposal::FlushAvoidLists { scope };
            match proposal.to_admin_event() {
                AdminEvent::FlushAvoidLists { scope: out } => assert_eq!(out, scope),
                other => panic!("expected FlushAvoidLists, got {other:?}"),
            }
        }
    }

    #[test]
    fn simulator_blast_radius_matches_reconcile_emission_for_force_evict() {
        // Consistency-pinning test for locked decision #4's
        // intent: the simulator's BlastRadius must agree
        // semantically with what reconcile would emit on the
        // leader for the same admin event.
        //
        // The two views differ in shape — the simulator
        // reports cluster-wide affected_* sets; reconcile
        // emits a RequestEviction action only on the chain's
        // leader — but they must converge on the same
        // (chain, victim) pair. A future change that breaks
        // this alignment surfaces here.
        use crate::adapter::net::behavior::meshos::action::MeshOsAction;
        use crate::adapter::net::behavior::meshos::reconcile::reconcile;
        use crate::adapter::net::behavior::meshos::state::{DesiredState, MeshOsState};

        const CHAIN: ChainId = 100;
        const LEADER: NodeId = 7;
        const VICTIM: NodeId = 9;

        // Build the post-apply state on the leader. The fold
        // path that lands `ForceEvictReplica` pushes onto
        // `forced_evictions`; reconcile's `diff_forced_evictions`
        // drains it leader-side.
        let mut actual = MeshOsState::default();
        actual.last_tick = Some(std::time::Instant::now());
        actual
            .replicas
            .insert(CHAIN, std::collections::BTreeSet::from([LEADER, 8, VICTIM]));
        actual.replica_leader.insert(CHAIN, LEADER);
        actual.forced_evictions.push((CHAIN, VICTIM));

        // Run reconcile() on the leader; expect exactly one
        // RequestEviction.
        let actions = reconcile(
            &actual,
            &DesiredState::default(),
            LEADER,
            &Default::default(),
            &Default::default(),
            &Default::default(),
            None,
        );
        let eviction = actions
            .iter()
            .find_map(|a| match a {
                MeshOsAction::RequestEviction { chain, victim } => Some((*chain, *victim)),
                _ => None,
            })
            .expect("leader's reconcile should emit one RequestEviction");
        assert_eq!(eviction, (CHAIN, VICTIM));

        // Simulator's BlastRadius reports the same affected
        // chain + victim.
        let mut snap = MeshOsSnapshot::default();
        snap.replicas.insert(
            CHAIN,
            super::super::snapshot::ReplicaSnapshot {
                holders: vec![LEADER, 8, VICTIM],
                desired_count: Some(3),
                leader: Some(LEADER),
            },
        );
        let blast = simulate(
            &snap,
            &IceActionProposal::ForceEvictReplica {
                chain: CHAIN,
                victim: VICTIM,
            },
        );
        assert!(
            blast.affected_replicas.contains(&CHAIN),
            "simulator should mark chain {CHAIN} as affected; got {:?}",
            blast.affected_replicas
        );
        assert!(
            blast.affected_nodes.contains(&VICTIM),
            "simulator should mark victim {VICTIM} as affected; got {:?}",
            blast.affected_nodes
        );
    }

    #[test]
    fn simulator_blast_radius_matches_reconcile_emission_for_force_cutover() {
        // Same consistency contract for ForceCutover.
        use crate::adapter::net::behavior::meshos::action::MeshOsAction;
        use crate::adapter::net::behavior::meshos::reconcile::reconcile;
        use crate::adapter::net::behavior::meshos::state::{DesiredState, MeshOsState};

        const CHAIN: ChainId = 200;
        const LEADER: NodeId = 11;
        const TARGET: NodeId = 42;

        let mut actual = MeshOsState::default();
        actual.last_tick = Some(std::time::Instant::now());
        actual
            .replicas
            .insert(CHAIN, std::collections::BTreeSet::from([LEADER, 12]));
        actual.replica_leader.insert(CHAIN, LEADER);
        actual.forced_placements.push((CHAIN, TARGET));
        let actions = reconcile(
            &actual,
            &DesiredState::default(),
            LEADER,
            &Default::default(),
            &Default::default(),
            &Default::default(),
            None,
        );
        let placement = actions
            .iter()
            .find_map(|a| match a {
                MeshOsAction::RequestPlacement { chain, target, .. } => Some((*chain, *target)),
                _ => None,
            })
            .expect("leader's reconcile should emit one RequestPlacement");
        assert_eq!(placement, (CHAIN, Some(TARGET)));

        let mut snap = MeshOsSnapshot::default();
        snap.replicas.insert(
            CHAIN,
            super::super::snapshot::ReplicaSnapshot {
                holders: vec![LEADER, 12],
                desired_count: Some(3),
                leader: Some(LEADER),
            },
        );
        let blast = simulate(
            &snap,
            &IceActionProposal::ForceCutover {
                chain: CHAIN,
                target: TARGET,
            },
        );
        assert!(blast.affected_replicas.contains(&CHAIN));
        assert!(blast.affected_nodes.contains(&TARGET));
    }

    #[test]
    fn simulate_force_evict_replica_reports_chain_and_victim() {
        let mut snap = MeshOsSnapshot::default();
        // Seed the chain so the simulator can verify victim is
        // a current holder.
        snap.replicas.insert(
            100,
            super::super::snapshot::ReplicaSnapshot {
                holders: vec![7, 8, 9],
                desired_count: Some(3),
                leader: Some(8),
            },
        );
        let blast = simulate(
            &snap,
            &IceActionProposal::ForceEvictReplica {
                chain: 100,
                victim: 7,
            },
        );
        assert_eq!(blast.affected_replicas, vec![100]);
        assert_eq!(blast.affected_nodes, vec![7]);
        assert!(blast.warnings.iter().any(|w| matches!(
            w,
            BlastWarning::ForcedEvictionBypassesCooldown {
                chain: 100,
                victim: 7
            }
        )));
        // Non-zero placement disturbance.
        assert!(blast.placement_stability_delta > 0.0);
    }

    #[test]
    fn simulate_force_evict_replica_warns_on_unknown_chain() {
        let snap = MeshOsSnapshot::default();
        let blast = simulate(
            &snap,
            &IceActionProposal::ForceEvictReplica {
                chain: 100,
                victim: 7,
            },
        );
        assert!(blast.warnings.iter().any(|w| matches!(
            w,
            BlastWarning::ForcedEvictionTargetsUnknownChain { chain: 100 }
        )));
    }

    #[test]
    fn simulate_force_evict_replica_warns_on_non_holder_victim() {
        let mut snap = MeshOsSnapshot::default();
        snap.replicas.insert(
            100,
            super::super::snapshot::ReplicaSnapshot {
                holders: vec![1, 2, 3],
                desired_count: Some(3),
                leader: Some(1),
            },
        );
        let blast = simulate(
            &snap,
            &IceActionProposal::ForceEvictReplica {
                chain: 100,
                victim: 999,
            },
        );
        assert!(blast.warnings.iter().any(|w| matches!(
            w,
            BlastWarning::ForcedEvictionTargetsNonHolder {
                chain: 100,
                victim: 999
            }
        )));
    }

    #[test]
    fn simulate_force_restart_daemon_targets_only_the_daemon() {
        use super::super::snapshot::{
            DaemonLifecycleSnapshot, DaemonSnapshot, RestartStateSnapshot,
        };
        let mut snap = MeshOsSnapshot::default();
        snap.daemons.insert(
            7,
            DaemonSnapshot {
                name: "telemetry".into(),
                lifecycle: DaemonLifecycleSnapshot::Stopped,
                restart_state: RestartStateSnapshot::BackingOff { until_ms: 5_000 },
                ..Default::default()
            },
        );
        let daemon = DaemonRef {
            id: 7,
            name: "telemetry".into(),
        };
        let blast = simulate(
            &snap,
            &IceActionProposal::ForceRestartDaemon {
                daemon: daemon.clone(),
            },
        );
        assert_eq!(blast.affected_daemons, vec![daemon]);
        assert!(blast.affected_nodes.is_empty());
        assert_eq!(blast.placement_stability_delta, 0.0);
        assert!(blast.warnings.iter().any(|w| matches!(
            w,
            BlastWarning::ForcedRestartBypassesBackoff { daemon_id: 7 }
        )));
        // No "unknown" or "not in backoff" warnings — the daemon
        // is observed AND in BackingOff.
        assert!(blast.warnings.iter().all(|w| !matches!(
            w,
            BlastWarning::ForcedRestartTargetsUnknownDaemon { .. }
                | BlastWarning::ForcedRestartDaemonNotInBackoff { .. }
        )));
    }

    #[test]
    fn simulate_force_restart_daemon_warns_on_unknown_daemon() {
        let snap = MeshOsSnapshot::default();
        let daemon = DaemonRef {
            id: 99,
            name: "absent".into(),
        };
        let blast = simulate(&snap, &IceActionProposal::ForceRestartDaemon { daemon });
        assert!(blast.warnings.iter().any(|w| matches!(
            w,
            BlastWarning::ForcedRestartTargetsUnknownDaemon { daemon_id: 99 }
        )));
    }

    #[test]
    fn simulate_force_restart_daemon_warns_when_already_idle() {
        use super::super::snapshot::{DaemonLifecycleSnapshot, DaemonSnapshot};
        let mut snap = MeshOsSnapshot::default();
        snap.daemons.insert(
            7,
            DaemonSnapshot {
                name: "telemetry".into(),
                lifecycle: DaemonLifecycleSnapshot::Running,
                ..Default::default()
            },
        );
        let blast = simulate(
            &snap,
            &IceActionProposal::ForceRestartDaemon {
                daemon: DaemonRef {
                    id: 7,
                    name: "telemetry".into(),
                },
            },
        );
        assert!(blast.warnings.iter().any(|w| matches!(
            w,
            BlastWarning::ForcedRestartDaemonNotInBackoff { daemon_id: 7 }
        )));
    }

    #[test]
    fn ice_proposal_to_admin_event_maps_force_restart_daemon() {
        let daemon = DaemonRef {
            id: 7,
            name: "telemetry".into(),
        };
        let proposal = IceActionProposal::ForceRestartDaemon {
            daemon: daemon.clone(),
        };
        match proposal.to_admin_event() {
            AdminEvent::ForceRestartDaemon { daemon: out } => assert_eq!(out, daemon),
            other => panic!("expected ForceRestartDaemon, got {other:?}"),
        }
    }

    #[test]
    fn simulate_force_cutover_reports_chain_and_target() {
        let mut snap = MeshOsSnapshot::default();
        snap.replicas.insert(
            100,
            super::super::snapshot::ReplicaSnapshot {
                holders: vec![1, 2, 3],
                desired_count: Some(3),
                leader: Some(1),
            },
        );
        let blast = simulate(
            &snap,
            &IceActionProposal::ForceCutover {
                chain: 100,
                target: 99,
            },
        );
        assert_eq!(blast.affected_replicas, vec![100]);
        assert_eq!(blast.affected_nodes, vec![99]);
        assert!(blast.warnings.iter().any(|w| matches!(
            w,
            BlastWarning::ForcedCutoverBypassesPlacementScorer {
                chain: 100,
                target: 99
            }
        )));
        assert!(blast.placement_stability_delta > 0.0);
    }

    #[test]
    fn simulate_force_cutover_warns_on_unknown_chain() {
        let snap = MeshOsSnapshot::default();
        let blast = simulate(
            &snap,
            &IceActionProposal::ForceCutover {
                chain: 100,
                target: 7,
            },
        );
        assert!(blast.warnings.iter().any(|w| matches!(
            w,
            BlastWarning::ForcedCutoverTargetsUnknownChain { chain: 100 }
        )));
    }

    #[test]
    fn simulate_force_cutover_warns_when_target_already_holder() {
        let mut snap = MeshOsSnapshot::default();
        snap.replicas.insert(
            100,
            super::super::snapshot::ReplicaSnapshot {
                holders: vec![7, 8, 9],
                desired_count: Some(3),
                leader: Some(7),
            },
        );
        let blast = simulate(
            &snap,
            &IceActionProposal::ForceCutover {
                chain: 100,
                target: 8,
            },
        );
        assert!(blast.warnings.iter().any(|w| matches!(
            w,
            BlastWarning::ForcedCutoverTargetAlreadyHolder {
                chain: 100,
                target: 8
            }
        )));
    }

    #[test]
    fn simulate_kill_migration_with_empty_snapshot_reports_no_daemons() {
        let snap = MeshOsSnapshot::default();
        let blast = simulate(&snap, &IceActionProposal::KillMigration { migration: 7 });
        // Snapshot has no in-flight migrations to enumerate; the
        // simulator emits zero affected daemons and the
        // cross-node-visibility warning.
        assert!(blast.affected_nodes.is_empty());
        assert!(blast.affected_replicas.is_empty());
        assert!(blast.affected_daemons.is_empty());
        assert!(blast.warnings.iter().any(|w| matches!(
            w,
            BlastWarning::KillMigrationDispatcherIntegrationPending { migration: 7 }
        )));
    }

    #[test]
    fn simulate_kill_migration_enumerates_local_in_flight_migration() {
        use super::super::snapshot::{MigrationPhaseSnapshot, MigrationSnapshot};
        let mut snap = MeshOsSnapshot::default();
        snap.in_flight_migrations = vec![
            MigrationSnapshot {
                daemon_origin: 0xCAFE,
                phase: MigrationPhaseSnapshot::Transfer,
                elapsed_ms: 250,
                ..Default::default()
            },
            // A noise migration that should not match the target.
            MigrationSnapshot {
                daemon_origin: 0xBEEF,
                phase: MigrationPhaseSnapshot::Replay,
                elapsed_ms: 50,
                ..Default::default()
            },
        ]
        .into();
        let blast = simulate(
            &snap,
            &IceActionProposal::KillMigration { migration: 0xCAFE },
        );
        assert_eq!(blast.affected_daemons.len(), 1);
        assert_eq!(blast.affected_daemons[0].id, 0xCAFE);
        // The cross-node-visibility warning stays — the local
        // snapshot can't see other nodes' orchestrators.
        assert!(blast.warnings.iter().any(|w| matches!(
            w,
            BlastWarning::KillMigrationDispatcherIntegrationPending { migration: 0xCAFE }
        )));
    }

    #[test]
    fn ice_proposal_to_admin_event_maps_kill_migration() {
        let proposal = IceActionProposal::KillMigration { migration: 42 };
        match proposal.to_admin_event() {
            AdminEvent::KillMigration { migration } => assert_eq!(migration, 42),
            other => panic!("expected KillMigration, got {other:?}"),
        }
    }

    #[test]
    fn ice_proposal_to_admin_event_maps_force_cutover() {
        let proposal = IceActionProposal::ForceCutover {
            chain: 100,
            target: 7,
        };
        match proposal.to_admin_event() {
            AdminEvent::ForceCutover { chain, target } => {
                assert_eq!(chain, 100);
                assert_eq!(target, 7);
            }
            other => panic!("expected ForceCutover, got {other:?}"),
        }
    }

    #[test]
    fn ice_proposal_to_admin_event_maps_force_evict_replica() {
        let proposal = IceActionProposal::ForceEvictReplica {
            chain: 100,
            victim: 7,
        };
        match proposal.to_admin_event() {
            AdminEvent::ForceEvictReplica { chain, victim } => {
                assert_eq!(chain, 100);
                assert_eq!(victim, 7);
            }
            other => panic!("expected ForceEvictReplica, got {other:?}"),
        }
    }

    #[test]
    fn admin_audit_record_postcard_round_trips_each_outcome() {
        for outcome in [
            VerificationOutcome::Accepted,
            VerificationOutcome::Rejected {
                kind: "signature_invalid".into(),
                message: "bad sig".into(),
            },
            VerificationOutcome::Unverified,
        ] {
            let record = AdminAuditRecord {
                seq: 1,
                committed_at_ms: 12_345,
                event: AdminEvent::FreezeCluster {
                    ttl: Duration::from_secs(60),
                },
                operator_ids: vec![1, 2, 3],
                outcome: outcome.clone(),
                chain_pending: false,
            };
            let bytes = postcard::to_allocvec(&record).expect("encode");
            let decoded: AdminAuditRecord = postcard::from_bytes(&bytes).expect("decode");
            assert_eq!(decoded, record);
        }
    }

    #[test]
    fn admin_audit_record_json_round_trips_for_audit_query_path() {
        let record = AdminAuditRecord {
            seq: 42,
            committed_at_ms: 999,
            event: AdminEvent::ThawCluster,
            operator_ids: vec![42],
            outcome: VerificationOutcome::Accepted,
            chain_pending: false,
        };
        let json = serde_json::to_string(&record).expect("encode");
        let decoded: AdminAuditRecord = serde_json::from_str(&json).expect("decode");
        assert_eq!(decoded, record);
    }

    #[test]
    fn admin_event_signing_payload_carries_domain_tag_and_issued_at_prefix() {
        // The payload starts with the ADMIN_SIGNING_DOMAIN tag
        // then the little-endian issued_at_ms stamp, then the
        // postcard-encoded inner event. The substrate verifier
        // rebuilds the same byte sequence and rejects anything
        // mismatched — this test pins the layout.
        let event = AdminEvent::EnterMaintenance {
            node: 42,
            drain_for: Some(Duration::from_secs(120)),
        };
        let issued_at_ms: u64 = 0x0123_4567_89AB_CDEF;
        let payload = admin_event_signing_payload(&event, issued_at_ms);
        assert!(
            payload.starts_with(ADMIN_SIGNING_DOMAIN),
            "payload must begin with ADMIN_SIGNING_DOMAIN, got {:?}",
            &payload[..ADMIN_SIGNING_DOMAIN.len().min(payload.len())]
        );
        let inner_start = ADMIN_SIGNING_DOMAIN.len() + 8;
        assert_eq!(
            &payload[ADMIN_SIGNING_DOMAIN.len()..inner_start],
            &issued_at_ms.to_le_bytes(),
        );
        let decoded: AdminEvent =
            postcard::from_bytes(&payload[inner_start..]).expect("decode inner event");
        assert_eq!(decoded, event);
    }

    #[test]
    fn ice_and_admin_signing_payloads_use_distinct_domains() {
        // Cross-domain replay defense: signatures collected over
        // an ICE proposal must not cross-verify as an admin
        // commit. The domain tags are the only thing keeping the
        // two byte sequences from colliding for variants whose
        // postcard encoding happens to overlap.
        assert_ne!(ICE_SIGNING_DOMAIN, ADMIN_SIGNING_DOMAIN);
        let event = AdminEvent::ThawCluster;
        let proposal = IceActionProposal::ThawCluster;
        let ts = 1u64;
        let blast_hash: BlastRadiusHash = [1u8; BLAST_RADIUS_HASH_LEN];
        let admin_payload = admin_event_signing_payload(&event, ts);
        let ice_payload = ice_proposal_signing_payload(&proposal, ts, &blast_hash);
        assert_ne!(admin_payload, ice_payload);
    }

    #[test]
    fn admin_verifier_accepts_a_valid_single_signature_admin_commit() {
        let kp = EntityKeypair::generate();
        let mut registry = OperatorRegistry::new();
        registry.register(&kp);
        let verifier = AdminVerifier::new(std::sync::Arc::new(registry), 1);

        let event = AdminEvent::Cordon { node: 42 };
        let issued_at_ms = now_ms_since_unix_epoch();
        let signature = OperatorSignature::sign_admin(&kp, &event, issued_at_ms);
        verifier
            .verify_admin_commit(&event, &signature, issued_at_ms, issued_at_ms)
            .expect("valid single-sig commit");
    }

    #[test]
    fn admin_verifier_rejects_tampered_single_signature_admin_commit() {
        let kp = EntityKeypair::generate();
        let mut registry = OperatorRegistry::new();
        registry.register(&kp);
        let verifier = AdminVerifier::new(std::sync::Arc::new(registry), 1);

        let event = AdminEvent::Cordon { node: 42 };
        let issued_at_ms = now_ms_since_unix_epoch();
        let mut signature = OperatorSignature::sign_admin(&kp, &event, issued_at_ms);
        signature.signature[0] ^= 0x01;
        let err = verifier
            .verify_admin_commit(&event, &signature, issued_at_ms, issued_at_ms)
            .unwrap_err();
        assert_eq!(err.kind(), "signature_invalid");
    }

    #[test]
    fn admin_verifier_rejects_admin_commit_from_unknown_operator() {
        let kp = EntityKeypair::generate();
        // Registry is empty — operator not known.
        let verifier = AdminVerifier::new(std::sync::Arc::new(OperatorRegistry::new()), 1);

        let event = AdminEvent::Cordon { node: 42 };
        let issued_at_ms = now_ms_since_unix_epoch();
        let signature = OperatorSignature::sign_admin(&kp, &event, issued_at_ms);
        let err = verifier
            .verify_admin_commit(&event, &signature, issued_at_ms, issued_at_ms)
            .unwrap_err();
        assert_eq!(err.kind(), "not_authorized");
    }

    #[test]
    fn admin_verifier_rejects_expired_ice_envelope() {
        // Envelopes stamped further in the past than the
        // freshness window fail with EnvelopeExpired regardless
        // of cryptographic validity — closes the replay window
        // the raw signature alone leaves open.
        let kp = EntityKeypair::generate();
        let mut registry = OperatorRegistry::new();
        registry.register(&kp);
        let verifier = AdminVerifier::with_freshness(
            std::sync::Arc::new(registry),
            1,
            Duration::from_secs(60),
            Duration::from_secs(5),
        );
        let proposal = IceActionProposal::ThawCluster;
        let issued_at_ms = 1_000_000u64;
        let now_ms = issued_at_ms + 120_000; // 120s old; window is 60s
        let blast_hash: BlastRadiusHash = [1u8; BLAST_RADIUS_HASH_LEN];
        let sig = OperatorSignature::sign(&kp, &proposal, issued_at_ms, &blast_hash);
        let err = verifier
            .verify_commit(&proposal, &[sig], issued_at_ms, &blast_hash, now_ms)
            .unwrap_err();
        assert_eq!(err.kind(), "envelope_expired");
    }

    #[test]
    fn admin_verifier_rejects_envelope_stamped_too_far_in_future() {
        let kp = EntityKeypair::generate();
        let mut registry = OperatorRegistry::new();
        registry.register(&kp);
        let verifier = AdminVerifier::with_freshness(
            std::sync::Arc::new(registry),
            1,
            Duration::from_secs(60),
            Duration::from_secs(5),
        );
        let proposal = IceActionProposal::ThawCluster;
        let now_ms = 1_000_000u64;
        let issued_at_ms = now_ms + 60_000; // 60s ahead of verifier clock; skew tolerance is 5s
        let blast_hash: BlastRadiusHash = [1u8; BLAST_RADIUS_HASH_LEN];
        let sig = OperatorSignature::sign(&kp, &proposal, issued_at_ms, &blast_hash);
        let err = verifier
            .verify_commit(&proposal, &[sig], issued_at_ms, &blast_hash, now_ms)
            .unwrap_err();
        assert_eq!(err.kind(), "envelope_from_future");
    }

    #[test]
    fn admin_verifier_accepts_envelope_inside_freshness_window() {
        let kp = EntityKeypair::generate();
        let mut registry = OperatorRegistry::new();
        registry.register(&kp);
        let verifier = AdminVerifier::with_freshness(
            std::sync::Arc::new(registry),
            1,
            Duration::from_secs(60),
            Duration::from_secs(5),
        );
        let proposal = IceActionProposal::ThawCluster;
        let issued_at_ms = 1_000_000u64;
        let now_ms = issued_at_ms + 30_000; // 30s old; well within 60s window
        let blast_hash: BlastRadiusHash = [1u8; BLAST_RADIUS_HASH_LEN];
        let sig = OperatorSignature::sign(&kp, &proposal, issued_at_ms, &blast_hash);
        verifier
            .verify_commit(&proposal, &[sig], issued_at_ms, &blast_hash, now_ms)
            .expect("envelope inside freshness window should verify");
    }

    #[test]
    fn ice_cooldown_blocks_second_commit_against_same_target_inside_window() {
        // Per-node cooldown: ForceCutover against node 42
        // imposes a cooldown on node 42; a subsequent ICE
        // commit against node 42 inside the window fails.
        let kp = EntityKeypair::generate();
        let mut registry = OperatorRegistry::new();
        registry.register(&kp);
        let verifier = AdminVerifier::with_full_policy(
            std::sync::Arc::new(registry),
            1,
            Duration::from_secs(300),
            Duration::from_secs(30),
            Duration::from_secs(60),
        );
        let proposal = IceActionProposal::ForceCutover {
            chain: 100,
            target: 42,
        };
        let issued_at_ms = 1_000_000u64;
        let now_ms = issued_at_ms;
        let blast = simulate(&MeshOsSnapshot::default(), &proposal);
        let blast_hash = blast_radius_hash(&blast);
        let sig = OperatorSignature::sign(&kp, &proposal, issued_at_ms, &blast_hash);

        // First commit succeeds and arms the cooldown.
        verifier
            .verify_commit(
                &proposal,
                std::slice::from_ref(&sig),
                issued_at_ms,
                &blast_hash,
                now_ms,
            )
            .expect("first commit should succeed");

        // Second commit inside the window — fails.
        let issued_at_ms2 = issued_at_ms + 30_000; // 30s later; cooldown is 60s
        let now_ms2 = issued_at_ms2;
        let sig2 = OperatorSignature::sign(&kp, &proposal, issued_at_ms2, &blast_hash);
        let err = verifier
            .verify_commit(&proposal, &[sig2], issued_at_ms2, &blast_hash, now_ms2)
            .unwrap_err();
        assert_eq!(err.kind(), "ice_cooldown_active");
        if let VerifyError::IceCooldownActive { node, .. } = err {
            assert_eq!(node, Some(42));
        } else {
            panic!("expected IceCooldownActive {{ node: Some(42), .. }}");
        }
    }

    #[test]
    fn ice_cooldown_releases_after_window_expires() {
        let kp = EntityKeypair::generate();
        let mut registry = OperatorRegistry::new();
        registry.register(&kp);
        let verifier = AdminVerifier::with_full_policy(
            std::sync::Arc::new(registry),
            1,
            Duration::from_secs(3600),
            Duration::from_secs(60),
            Duration::from_secs(60),
        );
        let proposal = IceActionProposal::ForceCutover {
            chain: 100,
            target: 42,
        };
        let issued_at_ms = 1_000_000u64;
        let blast = simulate(&MeshOsSnapshot::default(), &proposal);
        let blast_hash = blast_radius_hash(&blast);
        let sig = OperatorSignature::sign(&kp, &proposal, issued_at_ms, &blast_hash);
        verifier
            .verify_commit(&proposal, &[sig], issued_at_ms, &blast_hash, issued_at_ms)
            .expect("first commit should succeed");

        // Past the cooldown window — should succeed again.
        let later_ms = issued_at_ms + 61_000; // 61s later; cooldown is 60s
        let sig2 = OperatorSignature::sign(&kp, &proposal, later_ms, &blast_hash);
        verifier
            .verify_commit(&proposal, &[sig2], later_ms, &blast_hash, later_ms)
            .expect("commit after cooldown window should succeed");
    }

    #[test]
    fn ice_cooldown_isolates_different_target_nodes() {
        // A cooldown on node 42 should not block a commit
        // against node 99 — the per-node bucket isolates
        // targets.
        let kp = EntityKeypair::generate();
        let mut registry = OperatorRegistry::new();
        registry.register(&kp);
        let verifier = AdminVerifier::with_full_policy(
            std::sync::Arc::new(registry),
            1,
            Duration::from_secs(3600),
            Duration::from_secs(60),
            Duration::from_secs(60),
        );
        let issued_at_ms = 1_000_000u64;
        let proposal_a = IceActionProposal::ForceCutover {
            chain: 100,
            target: 42,
        };
        let blast_a = simulate(&MeshOsSnapshot::default(), &proposal_a);
        let hash_a = blast_radius_hash(&blast_a);
        let sig_a = OperatorSignature::sign(&kp, &proposal_a, issued_at_ms, &hash_a);
        verifier
            .verify_commit(&proposal_a, &[sig_a], issued_at_ms, &hash_a, issued_at_ms)
            .expect("commit against node 42");

        // Different node — should not be in cooldown.
        let proposal_b = IceActionProposal::ForceCutover {
            chain: 200,
            target: 99,
        };
        let blast_b = simulate(&MeshOsSnapshot::default(), &proposal_b);
        let hash_b = blast_radius_hash(&blast_b);
        let sig_b = OperatorSignature::sign(&kp, &proposal_b, issued_at_ms, &hash_b);
        verifier
            .verify_commit(&proposal_b, &[sig_b], issued_at_ms, &hash_b, issued_at_ms)
            .expect("different node should not be in cooldown");
    }

    #[test]
    fn ice_cooldown_freeze_cluster_blocks_subsequent_cluster_wide_commits() {
        // Cluster-wide cooldown: FreezeCluster blocks the next
        // ThawCluster inside the window. Subsequent per-node
        // ICE commits are unaffected because they hit a
        // different cooldown bucket.
        let kp = EntityKeypair::generate();
        let mut registry = OperatorRegistry::new();
        registry.register(&kp);
        let verifier = AdminVerifier::with_full_policy(
            std::sync::Arc::new(registry),
            1,
            Duration::from_secs(3600),
            Duration::from_secs(60),
            Duration::from_secs(60),
        );
        let issued_at_ms = 1_000_000u64;
        let freeze = IceActionProposal::FreezeCluster {
            ttl: Duration::from_secs(120),
        };
        let blast = simulate(&MeshOsSnapshot::default(), &freeze);
        let hash = blast_radius_hash(&blast);
        let sig = OperatorSignature::sign(&kp, &freeze, issued_at_ms, &hash);
        verifier
            .verify_commit(&freeze, &[sig], issued_at_ms, &hash, issued_at_ms)
            .expect("first freeze should succeed");

        let thaw = IceActionProposal::ThawCluster;
        let later_ms = issued_at_ms + 5_000; // 5s later
        let thaw_blast = simulate(&MeshOsSnapshot::default(), &thaw);
        let thaw_hash = blast_radius_hash(&thaw_blast);
        let thaw_sig = OperatorSignature::sign(&kp, &thaw, later_ms, &thaw_hash);
        let err = verifier
            .verify_commit(&thaw, &[thaw_sig], later_ms, &thaw_hash, later_ms)
            .unwrap_err();
        assert_eq!(err.kind(), "ice_cooldown_active");
        if let VerifyError::IceCooldownActive { node, .. } = err {
            assert_eq!(node, None, "FreezeCluster cooldown is cluster-wide");
        }
    }

    #[test]
    fn cross_domain_replay_fails_verification() {
        // A signature collected over an ICE proposal must not
        // cross-validate as an admin commit even when the inner
        // postcard encodings overlap. The domain tags are the
        // only thing keeping the two surfaces from sharing a
        // signature.
        let kp = EntityKeypair::generate();
        let mut registry = OperatorRegistry::new();
        registry.register(&kp);
        let issued_at_ms = now_ms_since_unix_epoch();
        let proposal = IceActionProposal::ThawCluster;
        let blast_hash: BlastRadiusHash = [1u8; BLAST_RADIUS_HASH_LEN];
        let ice_sig = OperatorSignature::sign(&kp, &proposal, issued_at_ms, &blast_hash);
        // Attempt to verify the ICE signature as if it were an
        // admin commit over the same logical action.
        let event = AdminEvent::ThawCluster;
        let admin_payload = admin_event_signing_payload(&event, issued_at_ms);
        let err = registry.verify(&ice_sig, &admin_payload).unwrap_err();
        assert_eq!(err.kind(), "signature_invalid");
    }

    #[test]
    fn admin_audit_record_can_carry_ordinary_admin_event() {
        // Verifies the type's expressive scope: ordinary admin
        // events (no `Force*` discriminator) fit on the same
        // ring as ICE events.
        let record = AdminAuditRecord {
            seq: 7,
            committed_at_ms: 1_000,
            event: AdminEvent::EnterMaintenance {
                node: 42,
                drain_for: Some(Duration::from_secs(120)),
            },
            operator_ids: Vec::new(),
            outcome: VerificationOutcome::Unverified,
            chain_pending: false,
        };
        let bytes = postcard::to_allocvec(&record).expect("encode");
        let decoded: AdminAuditRecord = postcard::from_bytes(&bytes).expect("decode");
        assert_eq!(decoded, record);
    }

    #[test]
    fn flush_avoid_lists_proposal_postcard_round_trips_for_every_scope() {
        for scope in [
            AvoidScope::Local { node: 42 },
            AvoidScope::OnPeer { peer: 7 },
            AvoidScope::Global,
        ] {
            let proposal = IceActionProposal::FlushAvoidLists { scope };
            let bytes = postcard::to_allocvec(&proposal).expect("encode");
            let decoded: IceActionProposal = postcard::from_bytes(&bytes).expect("decode");
            assert_eq!(decoded, proposal);
        }
    }
}
