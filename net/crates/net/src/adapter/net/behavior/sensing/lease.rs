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
//! removed synchronously by the release that empties it ([`Self::release`]), and
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
    /// The node already leases [`MAX_LEASED_INTERESTS`] distinct interests. A
    /// live interest is never evicted to make room.
    NodeAtCapacity,
    /// This interest already has [`MAX_HOLDERS_PER_INTEREST`] live holders.
    InterestAtCapacity,
}

/// Refusal counters for [`SensingInterestLeases`].
#[derive(Default)]
struct LeaseMetrics {
    refused_node_at_capacity: AtomicU64,
    refused_interest_at_capacity: AtomicU64,
}

/// Opaque per-holder token. [`SensingInterestLeases::acquire`] returns one;
/// [`SensingInterestLeases::release`] consumes it via the ticket. Node-local;
/// never on the wire.
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

    /// Acquire a reference to the interest `key` (registered under `spec`) at
    /// the requested `interval`.
    ///
    /// The returned [`LeaseAction`] tells the node what wire transition to
    /// perform and carries the authoritative spec; the returned [`LeaseToken`]
    /// is packaged into a [`SensingLeaseTicket`] and handed back to
    /// [`release`](Self::release) exactly once. `spec` is stored on the first
    /// acquisition and reused for every later action for this key.
    ///
    /// Refuses fail-closed at either bound (review-pass-2 §6). A refusal is
    /// TOTAL: no token is minted, no registration is recorded, the installed
    /// cadence does not move even for a would-be-stricter holder, and no live
    /// interest is evicted to make room. The caller therefore has nothing to
    /// roll back.
    pub fn acquire(
        &self,
        key: SensingLeaseKey,
        spec: &InterestSpec,
        interval: Duration,
    ) -> Result<(LeaseToken, LeaseAction), LeaseRefused> {
        let mut entries = self.entries.lock();
        // Checked BEFORE `entry()` and before the token mint, so a refused
        // acquisition is indistinguishable from never having been attempted —
        // and only a NEW key spends node budget.
        if !entries.contains_key(&key) && entries.len() >= MAX_LEASED_INTERESTS {
            self.metrics
                .refused_node_at_capacity
                .fetch_add(1, Ordering::AcqRel);
            return Err(LeaseRefused::NodeAtCapacity);
        }
        match entries.entry(key) {
            Entry::Vacant(v) => {
                let token = self.mint_token();
                let spec = Arc::new(spec.clone());
                let mut registrations = HashMap::new();
                registrations.insert(token, interval);
                v.insert(LeaseEntry {
                    spec: Arc::clone(&spec),
                    registrations,
                    installed_interval: interval,
                });
                Ok((token, LeaseAction::Register { spec, interval }))
            }
            Entry::Occupied(mut o) => {
                let entry = o.get_mut();
                if entry.registrations.len() >= MAX_HOLDERS_PER_INTEREST {
                    self.metrics
                        .refused_interest_at_capacity
                        .fetch_add(1, Ordering::AcqRel);
                    return Err(LeaseRefused::InterestAtCapacity);
                }
                let token = self.mint_token();
                entry.registrations.insert(token, interval);
                // `installed_interval` is maintained as the exact minimum of all
                // live registrations, so the new minimum is just this holder's
                // interval against it.
                let new_min = interval.min(entry.installed_interval);
                if new_min < entry.installed_interval {
                    entry.installed_interval = new_min;
                    Ok((
                        token,
                        LeaseAction::Reregister {
                            spec: Arc::clone(&entry.spec),
                            interval: new_min,
                        },
                    ))
                } else {
                    Ok((token, LeaseAction::Unchanged))
                }
            }
        }
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
            let (token, action) = leases.acquire(key, &s, ms(100)).expect("within capacity");
            assert!(matches!(action, LeaseAction::Register { .. }));
            held.push(ticket(key, token));
        }
        assert_eq!(leases.len(), MAX_LEASED_INTERESTS);

        let overflow = key_for(&s, MAX_LEASED_INTERESTS as u64);
        assert_eq!(
            leases.acquire(overflow, &s, ms(100)),
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
        let (_t, action) = leases
            .acquire(existing, &s, ms(500))
            .expect("an existing interest is not node-bounded");
        assert_eq!(action, LeaseAction::Unchanged);

        // Releasing frees exactly one interest of budget.
        assert!(matches!(
            leases.release(held.pop().expect("held")),
            LeaseAction::Deregister { .. }
        ));
        assert!(leases.acquire(overflow, &s, ms(100)).is_ok());
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
            leases.acquire(key, &s, ms(100)).expect("within capacity");
        }
        assert_eq!(
            leases.entry_for_test(&key),
            Some((MAX_HOLDERS_PER_INTEREST, ms(100)))
        );

        assert_eq!(
            leases.acquire(key, &s, ms(10)),
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
        let (_t, action) = leases.acquire(key, &s, ms(100)).expect("within capacity");
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
        leases.acquire(key, &s, ms(100)).expect("within capacity");
        let (_t, action) = leases.acquire(key, &s, ms(500)).expect("within capacity");
        assert_eq!(action, LeaseAction::Unchanged);
        assert_eq!(leases.entry_for_test(&key), Some((2, ms(100))));
    }

    #[test]
    fn stricter_second_acquire_reregisters_tighter() {
        let leases = SensingInterestLeases::default();
        let s = spec("gpu.infer");
        let key = key_for(&s, 7);
        leases.acquire(key, &s, ms(500)).expect("within capacity");
        let (_t, action) = leases.acquire(key, &s, ms(100)).expect("within capacity");
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
        let (strict, _) = leases.acquire(key, &s, ms(100)).expect("within capacity");
        let (loose, _) = leases.acquire(key, &s, ms(500)).expect("within capacity");
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
        let (strict, _) = leases.acquire(key, &s, ms(100)).expect("within capacity");
        leases.acquire(key, &s, ms(500)).expect("within capacity");
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
        let (only, _) = leases.acquire(key, &s, ms(100)).expect("within capacity");
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
        let (a, first) = leases.acquire(key, &s, ms(100)).expect("within capacity");
        let (b, second) = leases.acquire(key, &s, ms(100)).expect("within capacity");
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
        let (k1_tok, _) = leases.acquire(key, &s, ms(100)).expect("within capacity");
        // A real token, but issued for a DIFFERENT key — unknown to key's entry.
        let (k2_tok, _) = leases.acquire(k2, &s, ms(100)).expect("within capacity");
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
        leases.acquire(k1, &s, ms(100)).expect("within capacity");
        leases.acquire(k2, &s, ms(100)).expect("within capacity");
        assert_eq!(leases.len(), 2);
        assert_eq!(leases.entry_for_test(&k1), Some((1, ms(100))));
        assert_eq!(leases.entry_for_test(&k2), Some((1, ms(100))));
    }
}
