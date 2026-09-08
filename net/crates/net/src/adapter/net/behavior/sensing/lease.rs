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
pub(crate) const MAX_LEASED_INTERESTS: usize = 256;

/// Max live holders of ONE interest key.
///
/// Sized with `MAX_HANDLES_PER_FAMILY`: both answer "how many independent local
/// wrappers may share one identity" — and, as there, a DUPLICATE acquisition by
/// an existing holder spends budget rather than bypassing it, because each
/// acquisition mints its own token.
pub(crate) const MAX_HOLDERS_PER_INTEREST: usize = 64;

/// The reserved end of the holder-token identity space.
///
/// [`LeaseToken`] identity is TERMINAL (D4.8): a token names one acquisition
/// for the life of the process and is never reused. The allocator is therefore
/// a checked counter with `u64::MAX` reserved as an exhaustion SENTINEL rather
/// than a value — `next_token == LEASE_TOKEN_SPACE_END` means "no identity
/// left", so the space cannot wrap and a stale ticket can never name a
/// same-key successor.
const LEASE_TOKEN_SPACE_END: u64 = u64::MAX;

/// Why a lease acquisition was refused. Deterministic and state-free: a refused
/// acquisition mutates no registry state, so the caller sees exactly the
/// pre-call registry. A capacity refusal also reserves no identity; an
/// [`IdentityExhausted`](LeaseRefused::IdentityExhausted) refusal is by
/// definition the state where no identity is left to reserve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseRefused {
    /// The node already leases `MAX_LEASED_INTERESTS` distinct interests. A
    /// live interest is never evicted to make room.
    NodeAtCapacity,
    /// This interest already has `MAX_HOLDERS_PER_INTEREST` live holders.
    InterestAtCapacity,
    /// The terminal holder-token identity space is exhausted, so no further
    /// acquisition can be given a NEVER-REUSED identity.
    ///
    /// Terminal and node-global: every later acquisition on this node refuses
    /// too. Incumbent holders keep their registrations, their aggregate cadence
    /// and their rows, and their tickets still perform a terminal
    /// deregistration — this refuses to MINT, it does not retract.
    IdentityExhausted,
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
    /// Acquisitions refused because the terminal token identity space was
    /// exhausted. Nothing was reserved and nothing moved.
    refused_identity_exhausted: AtomicU64,
}

/// Opaque per-holder token, TERMINAL for the life of the process: the
/// allocator hands each acquisition a distinct value and never reuses or wraps
/// one — the top of the space is a reserved exhaustion sentinel rather than a
/// value. Reserved by the acquisition PREVIEW and recorded by its commit;
/// [`SensingInterestLeases::release`] consumes it via the ticket. Node-local;
/// never on the wire.
///
/// `Ord` is MINT ORDER, and it is load-bearing rather than a convenience: the
/// allocator is a single monotone counter, so `a < b` means exactly "`a` was
/// issued before `b`". An installation identity is the first token issued for
/// a key, so comparing two of them answers "which installation is the newer
/// one" without consulting the registry — which is how the node's refresh
/// schedule refuses to let a stale record overwrite its own successor's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
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
/// any registry state.
///
/// It DOES carry a reserved terminal identity ([`LeaseToken`]). Reserving in
/// the preview is what makes identity exhaustion detectable BEFORE any
/// interest-table or emitter mutation: a transaction that cannot be named must
/// refuse without having touched the wire. The reservation is monotone, so a
/// preview whose application then fails burns that one value and no other
/// holder is affected.
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
    /// The terminal identity reserved for this acquisition, recorded verbatim
    /// by the commit. Never re-derived, so commit cannot mint a second value.
    token: LeaseToken,
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
/// [`MeshNode::try_release_sensing_interest_lease`] — or to the
/// unit-returning [`MeshNode::release_sensing_interest_lease`] — exactly once
/// (an SDK RAII guard does that on drop). Opaque outside the crate, and
/// self-describing — release needs nothing else, so the wire identity can never
/// diverge from a caller's re-supplied arguments.
///
/// `Copy`, deliberately: a REFUSED organization release hands the still-live
/// ticket back, and a caller that retained its own copy may retry with either.
///
/// [`MeshNode::acquire_sensing_interest_lease`]:
///     crate::adapter::net::MeshNode::acquire_sensing_interest_lease
/// [`MeshNode::try_release_sensing_interest_lease`]:
///     crate::adapter::net::MeshNode::try_release_sensing_interest_lease
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

/// The immutable read one REFRESH of a live installation renews from.
///
/// Produced by [`SensingInterestLeases::refresh_view`]; mints nothing, inserts
/// nothing, removes nothing. Everything in it is the registry's own recorded
/// state, so a refresh can never re-register under a caller's stale copy of the
/// spec or the cadence.
pub(crate) struct RefreshView {
    spec: Arc<InterestSpec>,
    installed_interval: Duration,
    plane: LeasePlane,
    installation_id: LeaseToken,
    holders: usize,
}

impl RefreshView {
    /// The canonical STORED interest spec.
    pub(crate) fn spec(&self) -> &Arc<InterestSpec> {
        &self.spec
    }

    /// The cadence currently INSTALLED on the wire (the holders' minimum).
    pub(crate) fn installed_interval(&self) -> Duration {
        self.installed_interval
    }

    /// The authority plane the installation was ESTABLISHED under. A refresh
    /// must re-author on this plane or not at all — never downgrade.
    pub(crate) fn plane(&self) -> LeasePlane {
        self.plane
    }

    /// This installation's identity. A refresh armed for a different one is
    /// refreshing something that no longer exists.
    pub(crate) fn installation_id(&self) -> LeaseToken {
        self.installation_id
    }

    /// Live holder count. A refresh never changes it — that is the property.
    pub(crate) fn holders(&self) -> usize {
        self.holders
    }
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
    /// THIS INSTALLATION's identity: the FIRST token issued for the key, stored
    /// once and never rewritten while the entry lives.
    ///
    /// It survives that first holder's release while others remain, and a final
    /// release followed by a same-key re-acquisition yields a FRESH one —
    /// because the entry is removed and re-established, and the allocator is
    /// terminal and non-aliasing ([`LEASE_TOKEN_SPACE_END`]). That is exactly
    /// what lets a refresh prove it is renewing the installation it was armed
    /// for rather than resurrecting a retired one.
    installation_id: LeaseToken,
}

/// Reference-counted, cadence-aggregating sensing-interest leases for one node.
///
/// `entries` is THE registry synchronization: every read, every decision and
/// every mutation — acquisitions and releases alike — passes through one
/// crate-internal `lock_entries` helper. The one-shot public
/// [`acquire`](SensingInterestLeases::acquire) holds it across decision AND
/// application, so it is atomic against a concurrent final release; the node's
/// split preview/commit transaction relies instead on `sensing_lease_apply_mu`
/// being held across both halves, which is what lets the interest-table
/// application sit between them.
#[derive(Default)]
pub struct SensingInterestLeases {
    entries: Mutex<HashMap<SensingLeaseKey, LeaseEntry>>,
    /// The next unreserved holder identity. Advanced by CHECKED terminal
    /// allocation ([`SensingInterestLeases::reserve_token`]) — never
    /// `fetch_add`, which wraps `u64::MAX` back to `0` and would let a stale
    /// ticket release a same-key successor.
    next_token: AtomicU64,
    metrics: LeaseMetrics,
    /// Instrumented-only: how many times the registry mutex has been taken.
    ///
    /// The atomicity seam. "Decides and applies in ONE critical section" has no
    /// outcome signature on an uncontended run — a composed two-lock operation
    /// returns exactly the same tuple — so the witness counts the acquisitions
    /// the operation actually performs.
    #[cfg(any(test, feature = "fixtures"))]
    entry_lock_acquisitions: AtomicU64,
}

impl SensingInterestLeases {
    /// Reserve the next TERMINAL holder identity, or refuse.
    ///
    /// Checked, not `fetch_add`: `LEASE_TOKEN_SPACE_END` is a reserved
    /// sentinel, never a handed-out value, so the counter saturates there
    /// instead of wrapping to `0`. Once saturated the state is terminal —
    /// every later reservation refuses with
    /// [`LeaseRefused::IdentityExhausted`], and no value is ever issued twice.
    ///
    /// `compare_exchange_weak` rather than `fetch_update` so the exhaustion
    /// verdict is re-read from the CAS's own observed value on every retry: two
    /// racing reservations at the last legal slot must produce exactly one
    /// token and one refusal.
    fn reserve_token(&self) -> Result<LeaseToken, LeaseRefused> {
        let mut current = self.next_token.load(Ordering::Acquire);
        loop {
            if current == LEASE_TOKEN_SPACE_END {
                self.metrics
                    .refused_identity_exhausted
                    .fetch_add(1, Ordering::AcqRel);
                return Err(LeaseRefused::IdentityExhausted);
            }
            match self.next_token.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(LeaseToken(current)),
                Err(observed) => current = observed,
            }
        }
    }

    /// Take the registry's ONE mutex, counting the acquisition in instrumented
    /// builds.
    ///
    /// The count is the witness seam for atomicity: "this operation is a single
    /// registry critical section" is otherwise unobservable, and a composed
    /// two-lock operation is indistinguishable from an atomic one by outcome
    /// alone on an uncontended run.
    fn lock_entries(&self) -> parking_lot::MutexGuard<'_, HashMap<SensingLeaseKey, LeaseEntry>> {
        #[cfg(any(test, feature = "fixtures"))]
        self.entry_lock_acquisitions.fetch_add(1, Ordering::AcqRel);
        self.entries.lock()
    }

    /// How many times the registry mutex has been taken since construction.
    ///
    /// Instrumented builds only; the atomicity witnesses read the DELTA across
    /// one operation.
    #[doc(hidden)]
    #[cfg(any(test, feature = "fixtures"))]
    pub fn entry_lock_acquisitions_for_test(&self) -> u64 {
        self.entry_lock_acquisitions.load(Ordering::Acquire)
    }

    /// Whether `token` is a live holder of `key`, read from the registry
    /// itself.
    ///
    /// Instrumented builds only. The contention witnesses need "the registry
    /// and the returned action agree", which a holder COUNT cannot express.
    #[doc(hidden)]
    #[cfg(any(test, feature = "fixtures"))]
    pub fn holds_token_for_test(&self, key: &SensingLeaseKey, token: LeaseToken) -> bool {
        self.lock_entries()
            .get(key)
            .is_some_and(|entry| entry.registrations.contains_key(&token))
    }

    /// DECIDE one acquisition against an ALREADY-HELD registry view, mutating
    /// no entry.
    ///
    /// Split out from [`preview_acquire`](Self::preview_acquire) so the
    /// one-shot public surface can decide AND apply inside a single critical
    /// section while the node's split transaction keeps its two halves (the
    /// interest-table application legitimately runs between them, under
    /// `sensing_lease_apply_mu`).
    ///
    /// Refuses fail-closed at either bound (review-pass-2 §6) and at identity
    /// exhaustion (D4.8). A CAPACITY refusal is TOTAL: no identity is reserved,
    /// no registration is recorded, the installed cadence does not move even
    /// for a would-be-stricter holder, and no live interest is evicted to make
    /// room. Identity is reserved only AFTER both bounds admit, and reserving
    /// it here — rather than at application — is what makes exhaustion refuse
    /// before the caller has touched the interest table or the emitter.
    ///
    /// `plane` is the authority plane the CALLER can establish this lease under.
    /// It is recorded only on the establishing (vacant) acquisition; an existing
    /// lease keeps the plane it was created with, so a later authority change
    /// cannot reclassify it. [`PreviewedAcquire::plane`] is always the plane
    /// actually in force.
    fn decide_acquire(
        &self,
        entries: &HashMap<SensingLeaseKey, LeaseEntry>,
        key: SensingLeaseKey,
        spec: &InterestSpec,
        interval: Duration,
        plane: LeasePlane,
    ) -> Result<PreviewedAcquire, LeaseRefused> {
        let Some(entry) = entries.get(&key) else {
            // Only a NEW key spends the node budget.
            if entries.len() >= MAX_LEASED_INTERESTS {
                self.metrics
                    .refused_node_at_capacity
                    .fetch_add(1, Ordering::AcqRel);
                return Err(LeaseRefused::NodeAtCapacity);
            }
            let token = self.reserve_token()?;
            return Ok(PreviewedAcquire {
                key,
                interval,
                establishing_plane: plane,
                plane,
                // Promoted to an `Arc` HERE and stored verbatim by the
                // application, so the spec is cloned exactly once across both
                // halves.
                spec: Arc::new(spec.clone()),
                transition: AcquireTransition::Establish,
                token,
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
        let token = self.reserve_token()?;
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
            token,
        })
    }

    /// APPLY a decided acquisition to an ALREADY-HELD registry view.
    fn apply_acquire(
        entries: &mut HashMap<SensingLeaseKey, LeaseEntry>,
        previewed: PreviewedAcquire,
    ) -> LeaseToken {
        let PreviewedAcquire {
            key,
            interval,
            establishing_plane,
            plane,
            spec,
            transition,
            token,
        } = previewed;
        match entries.entry(key) {
            Entry::Vacant(v) => {
                debug_assert_eq!(
                    transition,
                    AcquireTransition::Establish,
                    "a vacant key must have decided as establishing — the decision \
                     and this application are either one registry critical section \
                     or bracketed by the node's apply guard"
                );
                let mut registrations = HashMap::new();
                registrations.insert(token, interval);
                v.insert(LeaseEntry {
                    spec,
                    registrations,
                    installed_interval: interval,
                    plane: establishing_plane,
                    // The FIRST token issued for this key IS the installation
                    // identity. Never rewritten while the entry lives.
                    installation_id: token,
                });
            }
            Entry::Occupied(mut o) => {
                let entry = o.get_mut();
                debug_assert_ne!(
                    transition,
                    AcquireTransition::Establish,
                    "an occupied key must not have decided as establishing"
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
    /// Sound ONLY because `sensing_lease_apply_mu` is held across the preview
    /// AND the commit, and every registry mutation the NODE performs runs under
    /// it — the same argument [`preview_release`](Self::preview_release) rests
    /// on. A caller that cannot hold that guard must use the atomic one-shot
    /// [`acquire`](Self::acquire) instead.
    pub(crate) fn preview_acquire(
        &self,
        key: SensingLeaseKey,
        spec: &InterestSpec,
        interval: Duration,
        plane: LeasePlane,
    ) -> Result<PreviewedAcquire, LeaseRefused> {
        let entries = self.lock_entries();
        self.decide_acquire(&entries, key, spec, interval, plane)
    }

    /// COMMIT a previewed acquisition, recording the holder under the identity
    /// the preview reserved.
    ///
    /// Infallible by construction: the preview proved both bounds AND reserved
    /// a terminal identity under the same `sensing_lease_apply_mu` the caller
    /// still holds, so nothing here can refuse and the caller can never be
    /// handed a ticket for a reference that was not recorded.
    pub(crate) fn commit_acquire(&self, previewed: PreviewedAcquire) -> LeaseToken {
        let mut entries = self.lock_entries();
        Self::apply_acquire(&mut entries, previewed)
    }

    /// Acquire one reference to `key` on the LEGACY plane, ATOMICALLY.
    ///
    /// The pre-existing single-call acquisition surface, preserved verbatim in
    /// arguments and return shape, and inheriting both cardinality bounds and
    /// terminal identity refusal — including
    /// `Err(`[`LeaseRefused::IdentityExhausted`]`)`.
    ///
    /// # One critical section, not two
    ///
    /// This decides AND applies under a SINGLE hold of the registry's own
    /// mutex — the same mutex [`release`](Self::release) takes. Composing the
    /// crate-internal preview and commit instead would lock twice, and a
    /// direct public caller holds no outer node guard to bridge the gap. The
    /// gap was reachable: with `K` held by a sole holder `t0`, a looser
    /// acquisition could decide `Unchanged` and unlock, `t0`'s final release
    /// could then remove `K` and return `Deregister`, and the acquisition
    /// would insert into a now-vacant `K` while still reporting `Unchanged` —
    /// a recorded holder with no installed registration (and a
    /// `debug_assert` failure in a debug build). The same gap let two
    /// last-slot decisions both pass a cardinality bound, and two
    /// same-key establishes both decide `Establish`.
    ///
    /// # Why the node still uses the split form
    ///
    /// PRODUCTION organization transitions must NOT use this: the node has to
    /// apply the table/emitter transition BETWEEN the two halves, and the
    /// authority plane in force is read from recorded provenance rather than
    /// assumed. Those callers hold `sensing_lease_apply_mu` across both halves,
    /// which is the equivalent exclusion at a wider scope. This collapses
    /// both, which is exactly why it is legacy-plane only.
    pub fn acquire(
        &self,
        key: SensingLeaseKey,
        spec: &InterestSpec,
        interval: Duration,
    ) -> Result<(LeaseToken, LeaseAction), LeaseRefused> {
        let (token, action, _) = self.acquire_atomic(key, spec, interval, LeasePlane::Legacy)?;
        Ok((token, action))
    }

    /// One-shot ATOMIC acquisition on an explicit plane — the shared body of
    /// [`acquire`](Self::acquire).
    ///
    /// The registry mutex is taken exactly once and held across the decision
    /// AND its application, so no rival acquisition or release can interleave.
    fn acquire_atomic(
        &self,
        key: SensingLeaseKey,
        spec: &InterestSpec,
        interval: Duration,
        plane: LeasePlane,
    ) -> Result<(LeaseToken, LeaseAction, LeasePlane), LeaseRefused> {
        let mut entries = self.lock_entries();
        let previewed = self.decide_acquire(&entries, key, spec, interval, plane)?;
        let action = previewed.action();
        let in_force = previewed.plane();
        Ok((
            Self::apply_acquire(&mut entries, previewed),
            action,
            in_force,
        ))
    }

    /// [`acquire_atomic`](Self::acquire_atomic) with the plane in force also
    /// returned. TESTS ONLY: production must apply the table/emitter transition
    /// BETWEEN the two halves, which is the whole point of splitting them.
    #[cfg(test)]
    fn acquire_on_plane(
        &self,
        key: SensingLeaseKey,
        spec: &InterestSpec,
        interval: Duration,
        plane: LeasePlane,
    ) -> Result<(LeaseToken, LeaseAction, LeasePlane), LeaseRefused> {
        self.acquire_atomic(key, spec, interval, plane)
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

    /// How many acquisitions were refused because the terminal token identity
    /// space was exhausted.
    ///
    /// A separate accessor rather than a third element on
    /// [`refusals`](Self::refusals): that tuple is a pre-existing surface, and
    /// widening it would break every caller that destructures it.
    pub fn identity_refusals(&self) -> u64 {
        self.metrics
            .refused_identity_exhausted
            .load(Ordering::Acquire)
    }

    /// Test-only allocator seam: place the terminal identity allocator at
    /// `next`, so the space boundary is reachable without issuing `2^64`
    /// tokens.
    ///
    /// `next == LEASE_TOKEN_SPACE_END` is the exhausted state; `next ==
    /// LEASE_TOKEN_SPACE_END - 1` leaves exactly one legal token. Writes the
    /// live counter directly, so what the witnesses drive is the PRODUCTION
    /// allocator and not a parallel one.
    #[doc(hidden)]
    #[cfg(any(test, feature = "fixtures"))]
    pub fn seed_token_space_for_test(&self, next: u64) {
        self.next_token.store(next, Ordering::Release);
    }

    /// The reserved sentinel that marks the identity space exhausted, so a
    /// witness can name the boundary instead of hard-coding `u64::MAX`.
    #[doc(hidden)]
    #[cfg(any(test, feature = "fixtures"))]
    pub fn token_space_end() -> u64 {
        LEASE_TOKEN_SPACE_END
    }

    /// What a REFRESH of one live installation must renew — read from the
    /// registry, mutating nothing.
    ///
    /// Refresh deliberately does NOT go through
    /// [`acquire`](SensingInterestLeases::acquire): that always reserves an
    /// identity and records a holder, so refreshing through it would add a
    /// holder every period, reach `MAX_HOLDERS_PER_INTEREST` and then refuse —
    /// and worse, the final release would stop deregistering, because holders
    /// would remain. So this is a distinct, non-mutating, identity-checked read
    /// and the caller performs the wire effect from it.
    pub(crate) fn refresh_view(&self, key: &SensingLeaseKey) -> Option<RefreshView> {
        let entries = self.lock_entries();
        let entry = entries.get(key)?;
        Some(RefreshView {
            // The canonical STORED spec and the INSTALLED cadence — never a
            // caller-supplied copy of either.
            spec: Arc::clone(&entry.spec),
            installed_interval: entry.installed_interval,
            plane: entry.plane,
            installation_id: entry.installation_id,
            holders: entry.registrations.len(),
        })
    }

    /// The authority plane a live lease was ESTABLISHED under, if the key is
    /// still held. Reads recorded metadata; derives nothing from current
    /// authority.
    pub(crate) fn plane_for(&self, key: &SensingLeaseKey) -> Option<LeasePlane> {
        self.lock_entries().get(key).map(|entry| entry.plane)
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
        let entries = self.lock_entries();
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
        let mut entries = self.lock_entries();
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
        self.lock_entries().len()
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
                .acquire_on_plane(key, &s, ms(100), LeasePlane::Legacy)
                .expect("within capacity");
            assert!(matches!(action, LeaseAction::Register { .. }));
            held.push(ticket(key, token));
        }
        assert_eq!(leases.len(), MAX_LEASED_INTERESTS);

        let overflow = key_for(&s, MAX_LEASED_INTERESTS as u64);
        assert_eq!(
            leases.acquire_on_plane(overflow, &s, ms(100), LeasePlane::Legacy),
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
            .acquire_on_plane(existing, &s, ms(500), LeasePlane::Legacy)
            .expect("an existing interest is not node-bounded");
        assert_eq!(action, LeaseAction::Unchanged);

        // Releasing frees exactly one interest of budget.
        assert!(matches!(
            leases.release(held.pop().expect("held")),
            LeaseAction::Deregister { .. }
        ));
        assert!(leases
            .acquire_on_plane(overflow, &s, ms(100), LeasePlane::Legacy)
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
                .acquire_on_plane(key, &s, ms(100), LeasePlane::Legacy)
                .expect("within capacity");
        }
        assert_eq!(
            leases.entry_for_test(&key),
            Some((MAX_HOLDERS_PER_INTEREST, ms(100)))
        );

        assert_eq!(
            leases.acquire_on_plane(key, &s, ms(10), LeasePlane::Legacy),
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
            .acquire_on_plane(key, &s, ms(100), LeasePlane::Legacy)
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
            .acquire_on_plane(key, &s, ms(100), LeasePlane::Legacy)
            .expect("within capacity");
        let (_t, action, _) = leases
            .acquire_on_plane(key, &s, ms(500), LeasePlane::Legacy)
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
            .acquire_on_plane(key, &s, ms(500), LeasePlane::Legacy)
            .expect("within capacity");
        let (_t, action, _) = leases
            .acquire_on_plane(key, &s, ms(100), LeasePlane::Legacy)
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
            .acquire_on_plane(key, &s, ms(100), LeasePlane::Legacy)
            .expect("within capacity");
        let (loose, _, _) = leases
            .acquire_on_plane(key, &s, ms(500), LeasePlane::Legacy)
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
            .acquire_on_plane(key, &s, ms(100), LeasePlane::Legacy)
            .expect("within capacity");
        leases
            .acquire_on_plane(key, &s, ms(500), LeasePlane::Legacy)
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
            .acquire_on_plane(key, &s, ms(100), LeasePlane::Legacy)
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
            .acquire_on_plane(key, &s, ms(100), LeasePlane::Legacy)
            .expect("within capacity");
        let (b, second, _) = leases
            .acquire_on_plane(key, &s, ms(100), LeasePlane::Legacy)
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
            .acquire_on_plane(key, &s, ms(100), LeasePlane::Legacy)
            .expect("within capacity");
        // A real token, but issued for a DIFFERENT key — unknown to key's entry.
        let (k2_tok, _, _) = leases
            .acquire_on_plane(k2, &s, ms(100), LeasePlane::Legacy)
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
            .acquire_on_plane(k1, &s, ms(100), LeasePlane::Legacy)
            .expect("within capacity");
        leases
            .acquire_on_plane(k2, &s, ms(100), LeasePlane::Legacy)
            .expect("within capacity");
        assert_eq!(leases.len(), 2);
        assert_eq!(leases.entry_for_test(&k1), Some((1, ms(100))));
        assert_eq!(leases.entry_for_test(&k2), Some((1, ms(100))));
    }

    // ---- D4.8: TERMINAL, non-aliasing holder identity ------------------
    //
    // The allocator used to be `AtomicU64::fetch_add`, which wraps `u64::MAX`
    // to `0`. Release authority is `(key, token)` equality, so a wrap hands a
    // NEW holder an identity a long-lived stale ticket already names — and that
    // stale ticket then tears down a successor's row. These drive the
    // PRODUCTION allocator through its test seam; none of them loops through
    // `2^64` values.

    /// The LAST legal identity is issued, and every acquisition after it
    /// refuses — terminally, and without disturbing the incumbent.
    #[test]
    fn the_last_legal_token_is_issued_and_every_later_acquisition_refuses() {
        let leases = SensingInterestLeases::default();
        let s = spec("gpu.infer");
        let incumbent_key = key_for(&s, 1);
        let (incumbent, _, _) = leases
            .acquire_on_plane(incumbent_key, &s, ms(100), LeasePlane::Legacy)
            .expect("the incumbent acquires before the space is narrowed");

        // Exactly ONE identity left.
        leases.seed_token_space_for_test(SensingInterestLeases::token_space_end() - 1);
        let last_key = key_for(&s, 2);
        let (last, _, _) = leases
            .acquire_on_plane(last_key, &s, ms(100), LeasePlane::Legacy)
            .expect("the last legal identity must still be issued");

        // And now the space is terminal — for a NEW key and for an EXISTING one
        // alike, because both need an identity.
        for (label, key) in [("a new key", key_for(&s, 3)), ("an existing key", last_key)] {
            assert_eq!(
                leases.acquire_on_plane(key, &s, ms(10), LeasePlane::Legacy),
                Err(LeaseRefused::IdentityExhausted),
                "{label} was admitted past the end of the identity space"
            );
        }
        assert_eq!(
            leases.identity_refusals(),
            2,
            "both exhaustion refusals must be counted"
        );
        assert_eq!(
            leases.refusals(),
            (0, 0),
            "exhaustion is not a capacity refusal"
        );

        // INCUMBENTS SURVIVE: registrations, cadence and holder count are
        // exactly what they were, and the refused acquisitions joined nothing.
        assert_eq!(
            leases.entry_for_test(&incumbent_key),
            Some((1, ms(100))),
            "an exhausted allocator must not disturb a live holder"
        );
        assert_eq!(
            leases.entry_for_test(&last_key),
            Some((1, ms(100))),
            "the refused stricter acquisition neither joined nor tightened"
        );
        assert_eq!(leases.len(), 2, "and created no third interest");

        // TERMINAL DEREGISTRATION still works for both existing tickets: this
        // refuses to MINT, it does not retract.
        assert!(matches!(
            leases.release(ticket(last_key, last)),
            LeaseAction::Deregister { .. }
        ));
        assert!(matches!(
            leases.release(ticket(incumbent_key, incumbent)),
            LeaseAction::Deregister { .. }
        ));
        assert!(leases.is_empty(), "both leases tore down cleanly");

        // Still terminal after the releases — freeing entries frees capacity,
        // never identity.
        assert_eq!(
            leases.acquire_on_plane(key_for(&s, 4), &s, ms(100), LeasePlane::Legacy),
            Err(LeaseRefused::IdentityExhausted)
        );
    }

    /// The wrap this replaces, named directly: with the allocator parked one
    /// short of the end, a stale ticket for a released holder can never be
    /// re-minted for a SUCCESSOR of the same key.
    ///
    /// RED coupling: restore `fetch_add` and the successor below is handed
    /// token `0` — the exact value `stale` already carries — so the stale
    /// release deregisters the successor's row.
    #[test]
    fn a_wrapped_allocator_cannot_hand_a_successor_a_stale_tickets_identity() {
        let leases = SensingInterestLeases::default();
        let s = spec("gpu.infer");
        let key = key_for(&s, 7);

        // A long-lived ticket minted at the BOTTOM of the space, then released.
        let (stale, _, _) = leases
            .acquire_on_plane(key, &s, ms(100), LeasePlane::Legacy)
            .expect("the first holder acquires");
        assert!(matches!(
            leases.release(ticket(key, stale)),
            LeaseAction::Deregister { .. }
        ));

        // Park the allocator at the last legal identity and mint the successor.
        leases.seed_token_space_for_test(SensingInterestLeases::token_space_end() - 1);
        let (successor, _, _) = leases
            .acquire_on_plane(key, &s, ms(100), LeasePlane::Legacy)
            .expect("the successor acquires under the last legal identity");
        assert_ne!(
            successor, stale,
            "the allocator reissued an identity a stale ticket still names"
        );

        // The stale ticket is a pure no-op against the successor's entry.
        assert_eq!(leases.release(ticket(key, stale)), LeaseAction::Unchanged);
        assert_eq!(
            leases.entry_for_test(&key),
            Some((1, ms(100))),
            "the stale release tore down the successor's registration"
        );
        assert!(matches!(
            leases.release(ticket(key, successor)),
            LeaseAction::Deregister { .. }
        ));
    }

    /// Two racing reservations at the LAST legal identity produce exactly one
    /// token and one refusal — the checked allocator's CAS, not a
    /// read-then-write that could hand the same value to both.
    #[test]
    fn racing_reservations_at_the_boundary_issue_exactly_one_identity() {
        let leases = SensingInterestLeases::default();
        let s = spec("gpu.infer");
        leases.seed_token_space_for_test(SensingInterestLeases::token_space_end() - 1);

        let start = std::sync::Barrier::new(4);
        let outcomes: Vec<_> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..4u64)
                .map(|provider| {
                    let leases = &leases;
                    let s = &s;
                    let start = &start;
                    scope.spawn(move || {
                        start.wait();
                        leases
                            .acquire_on_plane(key_for(s, provider), s, ms(100), LeasePlane::Legacy)
                            .map(|(token, _, _)| token)
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("reservation thread"))
                .collect()
        });

        assert_eq!(
            outcomes.iter().filter(|outcome| outcome.is_ok()).count(),
            1,
            "exactly one racer may take the last identity; got {outcomes:?}"
        );
        for outcome in &outcomes {
            if let Err(refusal) = outcome {
                assert_eq!(*refusal, LeaseRefused::IdentityExhausted);
            }
        }
        assert_eq!(leases.len(), 1, "and exactly one lease was established");
    }

    /// The restored compatibility surface: three arguments, the original
    /// two-element return, legacy-plane behaviour, and terminal identity
    /// refusal integrated.
    #[test]
    fn the_compatibility_acquire_keeps_its_shape_and_refuses_at_exhaustion() {
        let leases = SensingInterestLeases::default();
        let s = spec("gpu.infer");
        let key = key_for(&s, 7);

        let (first, action) = leases.acquire(key, &s, ms(100)).expect("first acquires");
        match action {
            LeaseAction::Register { spec, interval } => {
                assert_eq!(*spec, s);
                assert_eq!(interval, ms(100));
            }
            other => panic!("expected Register, got {other:?}"),
        }
        assert_eq!(
            leases.plane_for(&key),
            Some(LeasePlane::Legacy),
            "the compatibility surface establishes on the LEGACY plane"
        );
        let (_second, action) = leases.acquire(key, &s, ms(50)).expect("second acquires");
        assert!(matches!(action, LeaseAction::Reregister { .. }));

        leases.seed_token_space_for_test(SensingInterestLeases::token_space_end());
        assert_eq!(
            leases.acquire(key, &s, ms(100)),
            Err(LeaseRefused::IdentityExhausted),
            "the compatibility surface must inherit terminal identity refusal"
        );
        assert_eq!(
            leases.entry_for_test(&key),
            Some((2, ms(50))),
            "and must not have disturbed the live holders"
        );
        let _ = first;
    }

    // ---- PUBLIC ACQUIRE ATOMICITY --------------------------------------
    //
    // The public one-shot used to COMPOSE `preview_acquire` + `commit_acquire`.
    // Each locks the registry separately, and a direct public caller holds no
    // outer node guard bridging them. With `K` held by a sole holder `t0`: a
    // looser acquisition decides `Unchanged` and unlocks; `t0`'s final release
    // removes `K` and returns `Deregister`; the acquisition then inserts into a
    // vacant `K` while still reporting `Unchanged` — a recorded holder with no
    // installed registration (and a `debug_assert` failure in a debug build).
    // The same gap let two last-slot decisions both pass a cardinality bound
    // and two same-key decisions both establish.

    /// One public acquisition is ONE registry critical section — the direct,
    /// deterministic signature the composed form cannot have.
    ///
    /// RED coupling: restore
    /// `acquire = preview_acquire(..)? + commit_acquire(..)` and the delta
    /// below is 2.
    #[test]
    fn the_public_acquire_is_one_registry_critical_section() {
        let leases = SensingInterestLeases::default();
        let s = spec("gpu.infer");
        let key = key_for(&s, 7);

        for label in ["establishing", "joining"] {
            let before = leases.entry_lock_acquisitions_for_test();
            leases.acquire(key, &s, ms(100)).expect("acquire");
            assert_eq!(
                leases.entry_lock_acquisitions_for_test() - before,
                1,
                "the {label} public acquisition took the registry mutex more than \
                 once, so a rival release can land between its decision and its \
                 application"
            );
        }
        // The release side is the operation it must be atomic AGAINST, and it is
        // one section too — so a single mutex really is the whole ordering.
        let (token, _) = leases.acquire(key, &s, ms(100)).expect("acquire");
        let before = leases.entry_lock_acquisitions_for_test();
        leases.release(ticket(key, token));
        assert_eq!(
            leases.entry_lock_acquisitions_for_test() - before,
            1,
            "release is not a single registry critical section"
        );
    }

    /// A public acquisition and a concurrent FINAL release of the only other
    /// holder cannot interleave: whichever wins, the returned actions and the
    /// registry agree, and the forbidden pair (`Unchanged` + `Deregister`) is
    /// unreachable.
    ///
    /// This thread holds the registry mutex while both rivals run, so NEITHER
    /// can complete until it is released — that much is an exclusion, and the
    /// non-completion assertion below rests on it. The `ready` signals are sent
    /// BEFORE each rival enters its method, so they prove the threads started,
    /// NOT that either has already reached the mutex. The load-bearing claim is
    /// therefore the outcome: whichever order the mutex is granted in, the two
    /// returned actions and the registry must agree, and the forbidden
    /// `Unchanged` + `Deregister` pair must be unreachable. The deterministic
    /// RED coupling for the composed shape is the sibling
    /// single-critical-section witness above, which needs no schedule at all.
    #[test]
    fn a_public_acquire_cannot_interleave_with_a_final_release() {
        use std::sync::mpsc;

        let leases = Arc::new(SensingInterestLeases::default());
        let s = spec("gpu.infer");
        let key = key_for(&s, 7);
        let (t0, _) = leases.acquire(key, &s, ms(100)).expect("the sole holder");

        let (ready_tx, ready_rx) = mpsc::channel::<&'static str>();
        let (done_tx, done_rx) = mpsc::channel::<&'static str>();

        // HELD: neither rival can make progress while this lives.
        let held = leases.entries.lock();

        let acquirer = {
            let leases = Arc::clone(&leases);
            let s = s.clone();
            let ready = ready_tx.clone();
            let done = done_tx.clone();
            std::thread::spawn(move || {
                let _ = ready.send("acquire");
                let out = leases.acquire(key, &s, ms(500));
                let _ = done.send("acquire");
                out
            })
        };
        let releaser = {
            let leases = Arc::clone(&leases);
            std::thread::spawn(move || {
                let _ = ready_tx.send("release");
                let out = leases.release(ticket(key, t0));
                let _ = done_tx.send("release");
                out
            })
        };

        // Both rival threads have STARTED (this is a start signal, not an
        // arrival-at-the-mutex acknowledgement — see the note above).
        for _ in 0..2 {
            ready_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("both rivals must start");
        }
        // And neither can finish: the registry mutex is the whole ordering.
        assert!(
            done_rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "a registry operation completed while this thread held the registry \
             mutex — acquisition or release is not synchronized by it"
        );

        drop(held);
        let acquired = acquirer.join().expect("the acquirer joins");
        let released = releaser.join().expect("the releaser joins");

        let (a_token, a_action) = acquired.expect("the acquisition is within bounds");
        // EXACTLY the two coherent serializations, nothing else.
        let coherent = match (&a_action, &released) {
            // The acquisition won: it joined a live entry, and the final release
            // then relaxed the cadence to the survivor's.
            (LeaseAction::Unchanged, LeaseAction::Reregister { interval, .. }) => {
                *interval == ms(500)
            }
            // The release won: the entry was gone, so the acquisition
            // RE-ESTABLISHED it and said so.
            (LeaseAction::Register { interval, .. }, LeaseAction::Deregister { .. }) => {
                *interval == ms(500)
            }
            _ => false,
        };
        assert!(
            coherent,
            "incoherent serialization: acquire returned {a_action:?} and the final \
             release returned {released:?}. `Unchanged` + `Deregister` is the \
             split-transaction defect — a recorded holder with no installed \
             registration"
        );
        assert!(
            leases.holds_token_for_test(&key, a_token),
            "the registry must hold the token the acquisition handed out"
        );
        assert_eq!(
            leases.entry_for_test(&key),
            Some((1, ms(500))),
            "and exactly one holder at exactly its cadence"
        );
    }

    /// Concurrent establishes of the SAME fresh key produce exactly ONE
    /// `Register`; every other racer joins. Two `Establish` decisions would
    /// trip the occupied-entry `debug_assert` and report two installations for
    /// one row.
    #[test]
    fn concurrent_same_key_acquisitions_establish_exactly_once() {
        const RACERS: usize = 8;

        let leases = Arc::new(SensingInterestLeases::default());
        let s = spec("gpu.infer");
        let key = key_for(&s, 7);
        let start = Arc::new(std::sync::Barrier::new(RACERS));

        let outcomes: Vec<_> = (0..RACERS)
            .map(|_| {
                let leases = Arc::clone(&leases);
                let s = s.clone();
                let start = Arc::clone(&start);
                std::thread::spawn(move || {
                    start.wait();
                    leases.acquire(key, &s, ms(100))
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|handle| handle.join().expect("racer joins"))
            .collect();

        let mut registers = 0;
        for outcome in &outcomes {
            let (token, action) = outcome.as_ref().expect("all racers are within bounds");
            if matches!(action, LeaseAction::Register { .. }) {
                registers += 1;
            }
            assert!(
                leases.holds_token_for_test(&key, *token),
                "a racer was handed a token the registry does not hold"
            );
        }
        assert_eq!(
            registers, 1,
            "exactly one racer may establish the shared registration; got \
             {registers} out of {RACERS}"
        );
        assert_eq!(leases.entry_for_test(&key), Some((RACERS, ms(100))));
        assert_eq!(leases.len(), 1);
    }

    /// Concurrent acquisitions of DISTINCT new keys competing for the LAST node
    /// slot cannot both pass the bound.
    #[test]
    fn concurrent_last_node_slot_acquisitions_cannot_exceed_the_bound() {
        const RACERS: usize = 6;

        let leases = Arc::new(SensingInterestLeases::default());
        let s = spec("gpu.infer");
        for provider in 0..(MAX_LEASED_INTERESTS as u64 - 1) {
            leases
                .acquire(key_for(&s, provider), &s, ms(100))
                .expect("filling below the bound");
        }
        assert_eq!(leases.len(), MAX_LEASED_INTERESTS - 1);

        let start = Arc::new(std::sync::Barrier::new(RACERS));
        let outcomes: Vec<_> = (0..RACERS as u64)
            .map(|offset| {
                let leases = Arc::clone(&leases);
                let s = s.clone();
                let start = Arc::clone(&start);
                std::thread::spawn(move || {
                    start.wait();
                    leases.acquire(
                        key_for(&s, MAX_LEASED_INTERESTS as u64 + offset),
                        &s,
                        ms(100),
                    )
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|handle| handle.join().expect("racer joins"))
            .collect();

        assert_eq!(
            outcomes.iter().filter(|outcome| outcome.is_ok()).count(),
            1,
            "exactly one racer may take the last node slot; got {outcomes:?}"
        );
        for outcome in &outcomes {
            if let Err(refusal) = outcome {
                assert_eq!(*refusal, LeaseRefused::NodeAtCapacity);
            }
        }
        assert_eq!(
            leases.len(),
            MAX_LEASED_INTERESTS,
            "the node bound was exceeded"
        );
    }

    /// The same race at the per-interest HOLDER bound.
    #[test]
    fn concurrent_last_holder_acquisitions_cannot_exceed_the_bound() {
        const RACERS: usize = 6;

        let leases = Arc::new(SensingInterestLeases::default());
        let s = spec("gpu.infer");
        let key = key_for(&s, 7);
        for _ in 0..(MAX_HOLDERS_PER_INTEREST - 1) {
            leases.acquire(key, &s, ms(100)).expect("filling below");
        }

        let start = Arc::new(std::sync::Barrier::new(RACERS));
        let outcomes: Vec<_> = (0..RACERS)
            .map(|_| {
                let leases = Arc::clone(&leases);
                let s = s.clone();
                let start = Arc::clone(&start);
                std::thread::spawn(move || {
                    start.wait();
                    leases.acquire(key, &s, ms(100))
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|handle| handle.join().expect("racer joins"))
            .collect();

        assert_eq!(
            outcomes.iter().filter(|outcome| outcome.is_ok()).count(),
            1,
            "exactly one racer may take the last holder slot; got {outcomes:?}"
        );
        assert_eq!(
            leases.entry_for_test(&key),
            Some((MAX_HOLDERS_PER_INTEREST, ms(100))),
            "the per-interest holder bound was exceeded"
        );
    }
}
