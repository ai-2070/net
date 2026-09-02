//! Node-global sensing-interest lease registry (OLB-0 / sensing S0 §4.3).
//!
//! A sensing registration mutates state on the NODE, and multiple SDK/binding
//! wrappers can share one node (`Mesh::from_node_arc` is public; every binding
//! holds `Arc<MeshNode>`). So the refcount that decides register/deregister
//! must live on the node, exactly like the consumer-audience lease
//! ([`OrgAudienceLeases`]) — a per-wrapper count lets two wrappers each believe
//! they are the first installer, and the first to drop withdraws a live
//! watcher's interest.
//!
//! [`OrgAudienceLeases`]: crate::adapter::net::behavior::org_grant_registry::OrgAudienceLeases
//!
//! # Two shapes, not one key
//!
//! A provider-free interest coalesces at a rendezvous leader; an exact-provider
//! interest is per-provider node state. A bare `(audience, interest_digest)`
//! key would alias exact-provider registrations for different providers into
//! one refcount, so the key carries the provider for the exact shape.
//!
//! # Cadence is richer than a refcount
//!
//! A plain count cannot relax the wire cadence when the strictest watcher
//! leaves. Each entry retains the requested interval per holder token and
//! installs their minimum ([`strictest_sample_interval`]). A stricter join
//! tightens it; the strictest leaving relaxes it; a non-strictest leaving
//! changes nothing on the wire.
//!
//! # Ticket-owned application identity
//!
//! The entry also stores the canonical [`InterestSpec`] the interest was
//! registered under, so every action carries the exact spec to (re-)register or
//! deregister. Release takes only the ticket: the registry — not a caller
//! re-supplying arguments — is the single source of the wire identity, so a
//! ticket can never be released against a different key or spec. The soft-state
//! ttl is a single node-owned policy (not a per-holder input), so there is no
//! second aggregation to keep consistent.
//!
//! # Bounded in both dimensions
//!
//! Distinct interest keys and holders-per-key both carry an explicit cap with a
//! deterministic fail-closed refusal, matching every other registry this branch
//! adds (`MAX_NODE_SLOTS` / `MAX_HANDLES_PER_FAMILY`, `MAX_ENTRIES` /
//! `MAX_ENTRIES_PER_SCOPE`). Holders here are local SDK/binding callers rather
//! than remote peers, so this is the branch's bounded-state doctrine rather than
//! a remote-DoS boundary — but "bounds correct in isolation that do not compose"
//! is exactly why every sibling has one (review-pass-2 §6).
//!
//! There is no dead-entry reclamation to attempt before refusing: an entry is
//! removed synchronously by the release that empties it
//! ([`SensingInterestLeases::release`]), and
//! this registry has no expiry of its own, so a refusal always reflects live
//! holders and is never spurious.

use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;

use super::identity::{
    strictest_sample_interval, AudienceScopeCommitment, Digest256, InterestSpec,
};

/// Max distinct sensing-interest keys one node will lease at once.
///
/// Sized with `MAX_NODE_SLOTS`: both answer "how many distinct interest
/// identities may this node retain node-global state for" (review-pass-2 §6).
const MAX_LEASED_INTERESTS: usize = 256;

/// Max live holders of ONE interest key.
///
/// Sized with `MAX_HANDLES_PER_FAMILY`: both answer "how many independent local
/// wrappers may share one identity" — and, as there, a DUPLICATE acquisition by
/// an existing holder spends budget rather than bypassing it, because each
/// acquisition mints its own token.
const MAX_HOLDERS_PER_INTEREST: usize = 64;

/// Why a lease acquisition was refused. Deterministic and state-free: a refused
/// acquisition mints no token and mutates nothing, so the caller sees exactly
/// the pre-call registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseRefused {
    /// The node already leases `MAX_LEASED_INTERESTS` distinct interests. A
    /// live interest is never evicted to make room.
    NodeAtCapacity,
    /// This interest already has `MAX_HOLDERS_PER_INTEREST` live holders.
    InterestAtCapacity,
}

/// Transactional counters for [`SensingInterestLeases`].
///
/// Crate-private on purpose: these describe how this registry's own
/// transactions resolved, not a sensing-plane observability contract, so they
/// stay off the public [`SensingCounters`] surface.
///
/// [`SensingCounters`]: super::evaluator::SensingCounters
#[derive(Default)]
struct LeaseMetrics {
    refused_node_at_capacity: AtomicU64,
    refused_interest_at_capacity: AtomicU64,
    /// Wire reconciliations that FAILED after the registry had already committed
    /// its side (2026-07-23 §6 residual).
    ///
    /// The rollback and release legs cannot propagate an error — the registry
    /// mutation is already done and the caller is either returning the original
    /// failure or nothing at all — so the failure used to be discarded with
    /// `let _ =`. It is not harmless: it is precisely the state where the lease
    /// registry and the wire disagree, and the plane then behaves as if a row
    /// exists that does not (or vice versa) until something else reconciles it.
    /// Counted so the divergence is observable instead of silent.
    reconcile_failures: AtomicU64,
    /// Surviving-holder ORGANIZATION releases refused at the final currentness
    /// fence: nothing was released, the pre-transition registry and table state
    /// stand, and no frame was emitted. Distinct from `reconcile_failures`,
    /// which counts a divergence that already happened.
    release_refused: AtomicU64,
    /// Lease installations INVALIDATED: a tightening acquisition's refusal
    /// partition moved the shared row, and current organization authority then
    /// refused to restore the surviving holders' aggregate. The entry is dropped
    /// rather than left claiming an installed row that no longer exists.
    installations_invalidated: AtomicU64,
}

/// Opaque per-holder token. The registry's crate-private acquisition commit
/// returns one; [`SensingInterestLeases::release`] consumes it via the ticket.
/// Node-local; never on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LeaseToken(u64);

/// The two sensing-interest lease shapes (§4.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SensingLeaseKey {
    /// A rendezvous-coalesced interest; providers are resolved by the leader.
    ProviderFree {
        /// The authority audience scope the interest is registered under.
        audience: AudienceScopeCommitment,
        /// The canonical interest identity digest.
        interest_digest: Digest256,
    },
    /// An interest targeted at one exact provider — per-provider node state.
    ExactProvider {
        /// The authority audience scope the interest is registered under.
        audience: AudienceScopeCommitment,
        /// The canonical interest identity digest.
        interest_digest: Digest256,
        /// The exact provider node id this interest targets.
        provider: u64,
    },
}

/// The wire transition a lease mutation calls for, carrying the authoritative
/// spec the node must (re-)register or deregister with. The registry decides
/// WHAT must happen and supplies the exact identity; the node performs the
/// register/deregister.
#[derive(Debug, Clone, PartialEq)]
pub enum LeaseAction {
    /// First holder for this key — register `spec` at `interval`.
    Register {
        /// The canonical interest spec to register.
        spec: Arc<InterestSpec>,
        /// The sample interval to register on the wire.
        interval: Duration,
    },
    /// The aggregate interval changed (tighter on acquire, looser when the
    /// strictest holder releases) — re-register `spec` at `interval`.
    Reregister {
        /// The canonical interest spec to re-register.
        spec: Arc<InterestSpec>,
        /// The new aggregate sample interval to install.
        interval: Duration,
    },
    /// Refcount changed but the installed interval did not — no wire op.
    Unchanged,
    /// Last holder released — deregister `spec`.
    Deregister {
        /// The canonical interest spec to deregister.
        spec: Arc<InterestSpec>,
    },
}

/// Which shape an acquisition takes, decided by
/// [`SensingInterestLeases::preview_acquire`] before anything is mutated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AcquireTransition {
    /// First holder for this key — register the previewed spec.
    Establish,
    /// The aggregate TIGHTENS. `restore_to` is the aggregate installed right
    /// now, i.e. the surviving holders' cadence.
    ///
    /// A tightening is the ONLY acquisition that can leave the interest table
    /// moved when it fails: it overwrites the row shared by every holder of the
    /// key, so a self-provider emitter refusal partitions that row against THIS
    /// holder's interval and can remove it even though the survivors' interval
    /// would have been admitted. `restore_to` is what the transaction then owes
    /// the table back.
    Tighten { restore_to: Duration },
    /// Refcount only — no wire op, and nothing that can fail.
    Unchanged,
}

/// A previewed acquisition: everything
/// [`SensingInterestLeases::commit_acquire`] will do, computed without mutating
/// anything.
pub(crate) struct PreviewedAcquire {
    key: SensingLeaseKey,
    /// This holder's requested interval.
    interval: Duration,
    /// The plane to RECORD if this acquisition establishes the entry.
    establishing_plane: LeasePlane,
    /// The plane IN FORCE: the established one when the key already exists.
    plane: LeasePlane,
    /// The authoritative spec — the STORED one for an existing key, or the
    /// caller's promoted to an `Arc` for a new one.
    spec: Arc<InterestSpec>,
    transition: AcquireTransition,
}

impl PreviewedAcquire {
    /// The authority plane in force for this lease. For an existing key this is
    /// the RECORDED plane, which the caller must honour instead of whatever
    /// authority happens to be installed right now.
    pub(crate) fn plane(&self) -> LeasePlane {
        self.plane
    }

    /// The wire transition to apply BEFORE the registry commits. Clones an
    /// `Arc`, nothing else.
    pub(crate) fn action(&self) -> LeaseAction {
        match self.transition {
            AcquireTransition::Establish => LeaseAction::Register {
                spec: Arc::clone(&self.spec),
                interval: self.interval,
            },
            AcquireTransition::Tighten { .. } => LeaseAction::Reregister {
                spec: Arc::clone(&self.spec),
                interval: self.interval,
            },
            AcquireTransition::Unchanged => LeaseAction::Unchanged,
        }
    }

    /// The transition that puts the interest table back to the aggregate the
    /// registry still holds, for a tightening whose application failed AFTER
    /// moving rows.
    ///
    /// `None` when there is nothing to restore to: an establishing acquisition
    /// had no earlier aggregate (the partition removed only the row it had just
    /// created), and `Unchanged` never applies a transition at all.
    pub(crate) fn restoration(&self) -> Option<LeaseAction> {
        match self.transition {
            AcquireTransition::Tighten { restore_to } => Some(LeaseAction::Reregister {
                spec: Arc::clone(&self.spec),
                interval: restore_to,
            }),
            AcquireTransition::Establish | AcquireTransition::Unchanged => None,
        }
    }
}

/// A held sensing-interest lease reference (OLB-0). Returned by
/// [`MeshNode::acquire_sensing_interest_lease`]; hand it back to
/// [`MeshNode::release_sensing_interest_lease`] exactly once (an SDK RAII
/// guard does that on drop). Opaque outside the crate, and self-describing —
/// release needs nothing else, so the wire identity can never diverge from a
/// caller's re-supplied arguments.
///
/// [`MeshNode::acquire_sensing_interest_lease`]:
///     crate::adapter::net::MeshNode::acquire_sensing_interest_lease
/// [`MeshNode::release_sensing_interest_lease`]:
///     crate::adapter::net::MeshNode::release_sensing_interest_lease
#[derive(Debug, Clone, Copy)]
pub struct SensingLeaseTicket {
    pub(crate) key: SensingLeaseKey,
    pub(crate) token: LeaseToken,
}

/// Which authority plane a lease was FIRST established under.
///
/// Recorded once, at the establishing acquisition, and never recomputed. Every
/// later transition for that key reads it back rather than re-deriving it from
/// whatever authority happens to be installed at the time.
///
/// This is the fix for a real defect: the plane used to be inferred by
/// comparing the audience against the currently-installed authority's owner
/// organization, so removing the authority — or swapping the owner org — made
/// an existing ORGANIZATION lease look like a LEGACY one, and its next
/// transition would be authored and validated as legacy. That is authority
/// laundering in the release direction.
///
/// It is minimal internal metadata: two states, crate-private, derived at
/// establishment from the node's own installed authority. It is NOT a stored
/// membership certificate and NOT a replayable proof — every organization
/// transition still performs its own fresh capture. It only answers "which
/// plane does this lease belong to", which cannot change for the lease's
/// lifetime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LeasePlane {
    /// Established without organization authority — legacy entity-root frames.
    Legacy,
    /// Established under this node's own installed organization authority.
    Organization,
}

/// One key's shared registration state.
struct LeaseEntry {
    /// The canonical spec every holder of this key registered under (they share
    /// one interest identity by construction — the key is derived from it).
    spec: Arc<InterestSpec>,
    /// Requested interval per live holder token.
    registrations: HashMap<LeaseToken, Duration>,
    /// The interval currently installed on the wire — the minimum of
    /// `registrations` at the last wire-changing action.
    installed_interval: Duration,
    /// The authority plane this lease was established under. Immutable for the
    /// entry's lifetime.
    plane: LeasePlane,
}

/// Reference-counted, cadence-aggregating sensing-interest leases for one node.
#[derive(Default)]
pub struct SensingInterestLeases {
    entries: Mutex<HashMap<SensingLeaseKey, LeaseEntry>>,
    next_token: AtomicU64,
    metrics: LeaseMetrics,
}

impl SensingInterestLeases {
    fn mint_token(&self) -> LeaseToken {
        LeaseToken(self.next_token.fetch_add(1, Ordering::Relaxed))
    }

    /// PREVIEW one acquisition of the interest `key` at the requested
    /// `interval`, deciding everything and mutating NOTHING.
    ///
    /// The acquisition transaction commits the REGISTRY LAST: the caller applies
    /// the table/emitter transition first and only a successful application
    /// reaches [`commit_acquire`](Self::commit_acquire). A refused acquisition
    /// therefore never inserts a reference, so there is no registry rollback at
    /// all — which is what removed the old
    /// insert / refuse / roll-back / restore-through-a-second-fence path, whose
    /// two commits could be split by an authority publication and leave a
    /// surviving holder claiming a row that had been removed.
    ///
    /// Sound because `sensing_lease_apply_mu` is held across the preview AND the
    /// commit, and every registry mutation on this node runs under it — the same
    /// argument [`preview_release`](Self::preview_release) rests on.
    ///
    /// Refuses fail-closed at either bound (review-pass-2 §6). A refusal is
    /// TOTAL: no token is minted, no registration is recorded, the installed
    /// cadence does not move even for a would-be-stricter holder, and no live
    /// interest is evicted to make room.
    ///
    /// `plane` is the authority plane the CALLER can establish this lease under.
    /// It is recorded only on the establishing (vacant) acquisition; an existing
    /// lease keeps the plane it was created with, so a later authority change
    /// cannot reclassify it. [`PreviewedAcquire::plane`] is always the plane
    /// actually in force.
    pub(crate) fn preview_acquire(
        &self,
        key: SensingLeaseKey,
        spec: &InterestSpec,
        interval: Duration,
        plane: LeasePlane,
    ) -> Result<PreviewedAcquire, LeaseRefused> {
        let entries = self.entries.lock();
        let Some(entry) = entries.get(&key) else {
            // Only a NEW key spends the node budget.
            if entries.len() >= MAX_LEASED_INTERESTS {
                self.metrics
                    .refused_node_at_capacity
                    .fetch_add(1, Ordering::AcqRel);
                return Err(LeaseRefused::NodeAtCapacity);
            }
            return Ok(PreviewedAcquire {
                key,
                interval,
                establishing_plane: plane,
                plane,
                // Promoted to an `Arc` HERE and stored verbatim by the commit,
                // so the spec is cloned exactly once across both halves.
                spec: Arc::new(spec.clone()),
                transition: AcquireTransition::Establish,
            });
        };
        if entry.registrations.len() >= MAX_HOLDERS_PER_INTEREST {
            self.metrics
                .refused_interest_at_capacity
                .fetch_add(1, Ordering::AcqRel);
            return Err(LeaseRefused::InterestAtCapacity);
        }
        // `installed_interval` is maintained as the exact minimum of all live
        // registrations, so the new minimum is just this holder's interval
        // against it.
        let transition = if interval < entry.installed_interval {
            AcquireTransition::Tighten {
                restore_to: entry.installed_interval,
            }
        } else {
            AcquireTransition::Unchanged
        };
        Ok(PreviewedAcquire {
            key,
            interval,
            establishing_plane: plane,
            // The ESTABLISHED plane, not the caller's current view of it.
            plane: entry.plane,
            // The registry — not the caller — is the source of the wire identity
            // for an existing key. Refcount bump only.
            spec: Arc::clone(&entry.spec),
            transition,
        })
    }

    /// COMMIT a previewed acquisition, recording the holder.
    ///
    /// Infallible by construction: the preview proved both bounds under the same
    /// `sensing_lease_apply_mu` the caller still holds, so nothing here can
    /// refuse and the caller can never be handed a ticket for a reference that
    /// was not recorded.
    pub(crate) fn commit_acquire(&self, previewed: PreviewedAcquire) -> LeaseToken {
        let PreviewedAcquire {
            key,
            interval,
            establishing_plane,
            plane,
            spec,
            transition,
        } = previewed;
        let mut entries = self.entries.lock();
        let token = self.mint_token();
        match entries.entry(key) {
            Entry::Vacant(v) => {
                debug_assert_eq!(
                    transition,
                    AcquireTransition::Establish,
                    "a vacant key must have previewed as establishing — the apply \
                     guard is held across the preview and this commit"
                );
                let mut registrations = HashMap::new();
                registrations.insert(token, interval);
                v.insert(LeaseEntry {
                    spec,
                    registrations,
                    installed_interval: interval,
                    plane: establishing_plane,
                });
            }
            Entry::Occupied(mut o) => {
                let entry = o.get_mut();
                debug_assert_ne!(
                    transition,
                    AcquireTransition::Establish,
                    "an occupied key must not have previewed as establishing"
                );
                debug_assert_eq!(
                    entry.plane, plane,
                    "the established plane cannot change under the apply guard"
                );
                entry.registrations.insert(token, interval);
                entry.installed_interval = interval.min(entry.installed_interval);
            }
        }
        token
    }

    /// One-shot preview + commit.
    ///
    /// TESTS ONLY. Production must apply the table/emitter transition BETWEEN
    /// the two halves — that ordering is the whole point of splitting them, so
    /// collapsing it back is exactly the defect this shape removed.
    #[cfg(test)]
    fn acquire(
        &self,
        key: SensingLeaseKey,
        spec: &InterestSpec,
        interval: Duration,
        plane: LeasePlane,
    ) -> Result<(LeaseToken, LeaseAction, LeasePlane), LeaseRefused> {
        let previewed = self.preview_acquire(key, spec, interval, plane)?;
        let action = previewed.action();
        let plane = previewed.plane();
        Ok((self.commit_acquire(previewed), action, plane))
    }

    /// Record a wire reconciliation that failed after the registry committed —
    /// the registry/wire divergence window (2026-07-23 §6 residual).
    pub fn note_reconcile_failure(&self) {
        self.metrics
            .reconcile_failures
            .fetch_add(1, Ordering::AcqRel);
    }

    /// How many wire reconciliations failed after the registry had committed.
    /// Nonzero means the lease registry and the wire may disagree.
    pub fn reconcile_failures(&self) -> u64 {
        self.metrics.reconcile_failures.load(Ordering::Acquire)
    }

    /// Record an ORGANIZATION release refused at the final currentness fence.
    /// Nothing moved: this is the "refused" counterpart to
    /// [`note_reconcile_failure`](Self::note_reconcile_failure), which means a
    /// divergence already happened.
    pub(crate) fn note_release_refused(&self) {
        self.metrics.release_refused.fetch_add(1, Ordering::AcqRel);
    }

    /// How many surviving-holder organization releases were refused with nothing
    /// released and nothing emitted. Read only by the in-crate witnesses.
    #[cfg(test)]
    pub(crate) fn release_refusals(&self) -> u64 {
        self.metrics.release_refused.load(Ordering::Acquire)
    }

    /// Drop a lease entry ENTIRELY because its installation cannot exist under
    /// current authority, returning the number of holders dropped.
    ///
    /// Exactly one transition reaches here: a TIGHTENING acquisition whose
    /// self-provider refusal partition moved the shared row, and whose
    /// restoration to the surviving holders' aggregate was then refused by
    /// current organization authority. Retaining the entry would leave those
    /// holders claiming an installed row that no longer exists; removing it
    /// states the truth — the lease is no longer installed — and makes each
    /// surviving ticket's release the no-op it already is, since an organization
    /// lease with no current authority cannot be re-authored either.
    pub(crate) fn invalidate_installation(&self, key: &SensingLeaseKey) -> usize {
        let dropped = self
            .entries
            .lock()
            .remove(key)
            .map_or(0, |entry| entry.registrations.len());
        if dropped > 0 {
            self.metrics
                .installations_invalidated
                .fetch_add(1, Ordering::AcqRel);
        }
        dropped
    }

    /// How many lease installations were invalidated by
    /// [`invalidate_installation`](Self::invalidate_installation). Read only by
    /// the in-crate witnesses.
    #[cfg(test)]
    pub(crate) fn installations_invalidated(&self) -> u64 {
        self.metrics
            .installations_invalidated
            .load(Ordering::Acquire)
    }

    /// Refusal counters: `(node at capacity, interest at capacity)`.
    pub fn refusals(&self) -> (u64, u64) {
        (
            self.metrics
                .refused_node_at_capacity
                .load(Ordering::Acquire),
            self.metrics
                .refused_interest_at_capacity
                .load(Ordering::Acquire),
        )
    }

    /// The authority plane a live lease was ESTABLISHED under, if the key is
    /// still held. Reads recorded metadata; derives nothing from current
    /// authority.
    pub(crate) fn plane_for(&self, key: &SensingLeaseKey) -> Option<LeasePlane> {
        self.entries.lock().get(key).map(|entry| entry.plane)
    }

    /// What [`release`](Self::release) WOULD return, without mutating anything.
    ///
    /// This exists because an organization release is transactional: a
    /// surviving-holder `Reregister` must be authored against fresh current
    /// authority, and if it cannot be, the release must not have happened at
    /// all. Committing first and discovering the failure afterwards is exactly
    /// the divergence this preview prevents — the registry would hold a relaxed
    /// aggregate while the table and the wire kept the strict cadence.
    ///
    /// The preview is only sound if no rival transition for the key can
    /// interleave between it and the commit. The caller guarantees that by
    /// holding the organization transition lock across both (see
    /// `MeshNode::org_transition_mu`).
    pub(crate) fn preview_release(&self, ticket: &SensingLeaseTicket) -> LeaseAction {
        let entries = self.entries.lock();
        let Some(entry) = entries.get(&ticket.key) else {
            return LeaseAction::Unchanged;
        };
        if !entry.registrations.contains_key(&ticket.token) {
            return LeaseAction::Unchanged;
        }
        if entry.registrations.len() == 1 {
            return LeaseAction::Deregister {
                spec: Arc::clone(&entry.spec),
            };
        }
        let new_min = strictest_sample_interval(
            entry
                .registrations
                .iter()
                .filter(|(token, _)| **token != ticket.token)
                .map(|(_, interval)| *interval),
        )
        .unwrap_or(entry.installed_interval);
        if new_min > entry.installed_interval {
            LeaseAction::Reregister {
                spec: Arc::clone(&entry.spec),
                interval: new_min,
            }
        } else {
            LeaseAction::Unchanged
        }
    }

    /// Release a reference held under `ticket`.
    ///
    /// Releasing an unknown or already-released ticket is a no-op. The strictest
    /// holder leaving relaxes the cadence; the last holder leaving deregisters.
    /// All application identity comes from the stored entry, never from the
    /// caller.
    pub fn release(&self, ticket: SensingLeaseTicket) -> LeaseAction {
        let mut entries = self.entries.lock();
        let Entry::Occupied(mut o) = entries.entry(ticket.key) else {
            return LeaseAction::Unchanged;
        };
        let entry = o.get_mut();
        if entry.registrations.remove(&ticket.token).is_none() {
            return LeaseAction::Unchanged;
        }
        if entry.registrations.is_empty() {
            let spec = Arc::clone(&entry.spec);
            o.remove();
            return LeaseAction::Deregister { spec };
        }
        // A retained holder remains (the emptiness case returned above), so the
        // aggregate is defined; fall back to the installed value defensively
        // rather than unwrapping.
        let new_min = strictest_sample_interval(entry.registrations.values().copied())
            .unwrap_or(entry.installed_interval);
        if new_min > entry.installed_interval {
            entry.installed_interval = new_min;
            LeaseAction::Reregister {
                spec: Arc::clone(&entry.spec),
                interval: new_min,
            }
        } else {
            LeaseAction::Unchanged
        }
    }

    /// Test seam: how many distinct interest keys are currently referenced.
    /// Gated with the rest of the sensing seam group (review-pass-2 §1) — a
    /// test seam is not release surface even when it only reads.
    #[doc(hidden)]
    #[cfg(any(test, feature = "fixtures"))]
    pub fn len(&self) -> usize {
        self.entries.lock().len()
    }

    /// Whether no interest is referenced.
    #[doc(hidden)]
    #[cfg(any(test, feature = "fixtures"))]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Test seam: the live holder count and installed interval for one key.
    #[doc(hidden)]
    #[cfg(any(test, feature = "fixtures"))]
    pub fn entry_for_test(&self, key: &SensingLeaseKey) -> Option<(usize, Duration)> {
        self.entries
            .lock()
            .get(key)
            .map(|e| (e.registrations.len(), e.installed_interval))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::sensing::identity::{
        CanonicalConstraints, CapabilityId, DisclosureClass, ProviderSelector, ResultMode,
        WorkLatencyEnvelope,
    };

    fn audience(byte: u8) -> AudienceScopeCommitment {
        AudienceScopeCommitment::from_bytes([byte; 32])
    }

    fn spec(cap: &str) -> InterestSpec {
        InterestSpec {
            capability_id: CapabilityId::new(cap),
            constraints: CanonicalConstraints::from_entries([("k", "v")]).unwrap(),
            work_latency: WorkLatencyEnvelope::start_within(Duration::from_secs(2)),
            providers: ProviderSelector::Node(7),
            result_mode: ResultMode::Any,
            disclosure_class: DisclosureClass::Owner,
            audience: audience(1),
        }
    }

    fn key_for(s: &InterestSpec, provider: u64) -> SensingLeaseKey {
        SensingLeaseKey::ExactProvider {
            audience: s.audience,
            interest_digest: s.interest_digest(),
            provider,
        }
    }

    fn ticket(key: SensingLeaseKey, token: LeaseToken) -> SensingLeaseTicket {
        SensingLeaseTicket { key, token }
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    // ---- review-pass-2 §6: cardinality bounds ------------------------

    /// The 257th DISTINCT interest is refused, the first 256 are untouched, and
    /// the refusal is counted. A live interest is never evicted to make room.
    #[test]
    fn the_two_hundred_and_fifty_seventh_interest_is_refused_without_evicting_any() {
        let leases = SensingInterestLeases::default();
        let s = spec("gpu.infer");
        let mut held = Vec::new();
        for provider in 0..MAX_LEASED_INTERESTS as u64 {
            let key = key_for(&s, provider);
            let (token, action, _) = leases
                .acquire(key, &s, ms(100), LeasePlane::Legacy)
                .expect("within capacity");
            assert!(matches!(action, LeaseAction::Register { .. }));
            held.push(ticket(key, token));
        }
        assert_eq!(leases.len(), MAX_LEASED_INTERESTS);

        let overflow = key_for(&s, MAX_LEASED_INTERESTS as u64);
        assert_eq!(
            leases.acquire(overflow, &s, ms(100), LeasePlane::Legacy),
            Err(LeaseRefused::NodeAtCapacity)
        );
        assert_eq!(
            leases.entry_for_test(&overflow),
            None,
            "a refused acquisition records nothing"
        );
        assert_eq!(
            leases.len(),
            MAX_LEASED_INTERESTS,
            "and evicts nothing — the first 256 are intact"
        );
        assert_eq!(leases.refusals(), (1, 0));

        // An EXISTING key still acquires at node capacity: only a new key spends
        // the node budget.
        let existing = held[0].key;
        let (_t, action, _) = leases
            .acquire(existing, &s, ms(500), LeasePlane::Legacy)
            .expect("an existing interest is not node-bounded");
        assert_eq!(action, LeaseAction::Unchanged);

        // Releasing frees exactly one interest of budget.
        assert!(matches!(
            leases.release(held.pop().expect("held")),
            LeaseAction::Deregister { .. }
        ));
        assert!(leases
            .acquire(overflow, &s, ms(100), LeasePlane::Legacy)
            .is_ok());
    }

    /// The 65th HOLDER of one interest is refused, and — the load-bearing half —
    /// a refused holder that would have been STRICTER does not move the
    /// installed cadence.
    #[test]
    fn the_sixty_fifth_holder_is_refused_and_cannot_tighten_the_cadence() {
        let leases = SensingInterestLeases::default();
        let s = spec("gpu.infer");
        let key = key_for(&s, 7);
        for _ in 0..MAX_HOLDERS_PER_INTEREST {
            leases
                .acquire(key, &s, ms(100), LeasePlane::Legacy)
                .expect("within capacity");
        }
        assert_eq!(
            leases.entry_for_test(&key),
            Some((MAX_HOLDERS_PER_INTEREST, ms(100)))
        );

        assert_eq!(
            leases.acquire(key, &s, ms(10), LeasePlane::Legacy),
            Err(LeaseRefused::InterestAtCapacity)
        );
        assert_eq!(
            leases.entry_for_test(&key),
            Some((MAX_HOLDERS_PER_INTEREST, ms(100))),
            "a refused holder neither joins nor tightens the installed cadence"
        );
        assert_eq!(leases.refusals(), (0, 1));
        assert_eq!(
            leases.len(),
            1,
            "and the refusal creates no second interest"
        );
    }

    #[test]
    fn first_acquire_registers_the_spec_at_its_interval() {
        let leases = SensingInterestLeases::default();
        let s = spec("gpu.infer");
        let key = key_for(&s, 7);
        let (_t, action, _) = leases
            .acquire(key, &s, ms(100), LeasePlane::Legacy)
            .expect("within capacity");
        match action {
            LeaseAction::Register { spec, interval } => {
                assert_eq!(*spec, s);
                assert_eq!(interval, ms(100));
            }
            other => panic!("expected Register, got {other:?}"),
        }
        assert_eq!(leases.entry_for_test(&key), Some((1, ms(100))));
    }

    #[test]
    fn looser_second_acquire_is_unchanged() {
        let leases = SensingInterestLeases::default();
        let s = spec("gpu.infer");
        let key = key_for(&s, 7);
        leases
            .acquire(key, &s, ms(100), LeasePlane::Legacy)
            .expect("within capacity");
        let (_t, action, _) = leases
            .acquire(key, &s, ms(500), LeasePlane::Legacy)
            .expect("within capacity");
        assert_eq!(action, LeaseAction::Unchanged);
        assert_eq!(leases.entry_for_test(&key), Some((2, ms(100))));
    }

    #[test]
    fn stricter_second_acquire_reregisters_tighter() {
        let leases = SensingInterestLeases::default();
        let s = spec("gpu.infer");
        let key = key_for(&s, 7);
        leases
            .acquire(key, &s, ms(500), LeasePlane::Legacy)
            .expect("within capacity");
        let (_t, action, _) = leases
            .acquire(key, &s, ms(100), LeasePlane::Legacy)
            .expect("within capacity");
        match action {
            LeaseAction::Reregister { spec, interval } => {
                assert_eq!(*spec, s);
                assert_eq!(interval, ms(100));
            }
            other => panic!("expected Reregister, got {other:?}"),
        }
    }

    #[test]
    fn releasing_a_non_strictest_holder_makes_no_wire_change() {
        let leases = SensingInterestLeases::default();
        let s = spec("gpu.infer");
        let key = key_for(&s, 7);
        let (strict, _, _) = leases
            .acquire(key, &s, ms(100), LeasePlane::Legacy)
            .expect("within capacity");
        let (loose, _, _) = leases
            .acquire(key, &s, ms(500), LeasePlane::Legacy)
            .expect("within capacity");
        let _ = strict;
        let action = leases.release(ticket(key, loose));
        assert_eq!(action, LeaseAction::Unchanged);
        assert_eq!(leases.entry_for_test(&key), Some((1, ms(100))));
    }

    #[test]
    fn releasing_the_strictest_holder_relaxes_the_cadence() {
        let leases = SensingInterestLeases::default();
        let s = spec("gpu.infer");
        let key = key_for(&s, 7);
        let (strict, _, _) = leases
            .acquire(key, &s, ms(100), LeasePlane::Legacy)
            .expect("within capacity");
        leases
            .acquire(key, &s, ms(500), LeasePlane::Legacy)
            .expect("within capacity");
        match leases.release(ticket(key, strict)) {
            LeaseAction::Reregister { spec, interval } => {
                assert_eq!(*spec, s);
                assert_eq!(interval, ms(500));
            }
            other => panic!("expected Reregister, got {other:?}"),
        }
        assert_eq!(leases.entry_for_test(&key), Some((1, ms(500))));
    }

    #[test]
    fn last_release_deregisters_and_drops_the_entry() {
        let leases = SensingInterestLeases::default();
        let s = spec("gpu.infer");
        let key = key_for(&s, 7);
        let (only, _, _) = leases
            .acquire(key, &s, ms(100), LeasePlane::Legacy)
            .expect("within capacity");
        match leases.release(ticket(key, only)) {
            LeaseAction::Deregister { spec } => assert_eq!(*spec, s),
            other => panic!("expected Deregister, got {other:?}"),
        }
        assert!(leases.is_empty());
    }

    #[test]
    fn equal_interval_holders_share_one_registration() {
        let leases = SensingInterestLeases::default();
        let s = spec("gpu.infer");
        let key = key_for(&s, 7);
        let (a, first, _) = leases
            .acquire(key, &s, ms(100), LeasePlane::Legacy)
            .expect("within capacity");
        let (b, second, _) = leases
            .acquire(key, &s, ms(100), LeasePlane::Legacy)
            .expect("within capacity");
        assert!(matches!(first, LeaseAction::Register { .. }));
        assert_eq!(second, LeaseAction::Unchanged);
        assert_eq!(leases.release(ticket(key, a)), LeaseAction::Unchanged);
        assert!(matches!(
            leases.release(ticket(key, b)),
            LeaseAction::Deregister { .. }
        ));
        assert!(leases.is_empty());
    }

    #[test]
    fn releasing_an_unknown_or_repeated_token_is_a_noop() {
        let leases = SensingInterestLeases::default();
        let s = spec("gpu.infer");
        let key = key_for(&s, 7);
        let k2 = SensingLeaseKey::ExactProvider {
            audience: audience(1),
            interest_digest: s.interest_digest(),
            provider: 9,
        };
        let (k1_tok, _, _) = leases
            .acquire(key, &s, ms(100), LeasePlane::Legacy)
            .expect("within capacity");
        // A real token, but issued for a DIFFERENT key — unknown to key's entry.
        let (k2_tok, _, _) = leases
            .acquire(k2, &s, ms(100), LeasePlane::Legacy)
            .expect("within capacity");
        assert_eq!(leases.release(ticket(key, k2_tok)), LeaseAction::Unchanged);
        assert_eq!(leases.entry_for_test(&key), Some((1, ms(100))));
        // Double release of key's token: first deregisters, second is a noop.
        assert!(matches!(
            leases.release(ticket(key, k1_tok)),
            LeaseAction::Deregister { .. }
        ));
        assert_eq!(leases.release(ticket(key, k1_tok)), LeaseAction::Unchanged);
    }

    #[test]
    fn distinct_keys_never_alias() {
        let leases = SensingInterestLeases::default();
        let s = spec("gpu.infer");
        let k1 = key_for(&s, 7);
        let k2 = SensingLeaseKey::ExactProvider {
            audience: s.audience,
            interest_digest: s.interest_digest(),
            provider: 8,
        };
        leases
            .acquire(k1, &s, ms(100), LeasePlane::Legacy)
            .expect("within capacity");
        leases
            .acquire(k2, &s, ms(100), LeasePlane::Legacy)
            .expect("within capacity");
        assert_eq!(leases.len(), 2);
        assert_eq!(leases.entry_for_test(&k1), Some((1, ms(100))));
        assert_eq!(leases.entry_for_test(&k2), Some((1, ms(100))));
    }
}
