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

use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;

use super::identity::{
    strictest_sample_interval, AudienceScopeCommitment, Digest256, InterestSpec,
};

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
    pub fn acquire(
        &self,
        key: SensingLeaseKey,
        spec: &InterestSpec,
        interval: Duration,
    ) -> (LeaseToken, LeaseAction) {
        let token = self.mint_token();
        let mut entries = self.entries.lock();
        match entries.entry(key) {
            Entry::Vacant(v) => {
                let spec = Arc::new(spec.clone());
                let mut registrations = HashMap::new();
                registrations.insert(token, interval);
                v.insert(LeaseEntry {
                    spec: Arc::clone(&spec),
                    registrations,
                    installed_interval: interval,
                });
                (token, LeaseAction::Register { spec, interval })
            }
            Entry::Occupied(mut o) => {
                let entry = o.get_mut();
                entry.registrations.insert(token, interval);
                // `installed_interval` is maintained as the exact minimum of all
                // live registrations, so the new minimum is just this holder's
                // interval against it.
                let new_min = interval.min(entry.installed_interval);
                if new_min < entry.installed_interval {
                    entry.installed_interval = new_min;
                    (
                        token,
                        LeaseAction::Reregister {
                            spec: Arc::clone(&entry.spec),
                            interval: new_min,
                        },
                    )
                } else {
                    (token, LeaseAction::Unchanged)
                }
            }
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
    #[doc(hidden)]
    pub fn len(&self) -> usize {
        self.entries.lock().len()
    }

    /// Whether no interest is referenced.
    #[doc(hidden)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Test seam: the live holder count and installed interval for one key.
    #[doc(hidden)]
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

    #[test]
    fn first_acquire_registers_the_spec_at_its_interval() {
        let leases = SensingInterestLeases::default();
        let s = spec("gpu.infer");
        let key = key_for(&s, 7);
        let (_t, action) = leases.acquire(key, &s, ms(100));
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
        leases.acquire(key, &s, ms(100));
        let (_t, action) = leases.acquire(key, &s, ms(500));
        assert_eq!(action, LeaseAction::Unchanged);
        assert_eq!(leases.entry_for_test(&key), Some((2, ms(100))));
    }

    #[test]
    fn stricter_second_acquire_reregisters_tighter() {
        let leases = SensingInterestLeases::default();
        let s = spec("gpu.infer");
        let key = key_for(&s, 7);
        leases.acquire(key, &s, ms(500));
        let (_t, action) = leases.acquire(key, &s, ms(100));
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
        let (strict, _) = leases.acquire(key, &s, ms(100));
        let (loose, _) = leases.acquire(key, &s, ms(500));
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
        let (strict, _) = leases.acquire(key, &s, ms(100));
        leases.acquire(key, &s, ms(500));
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
        let (only, _) = leases.acquire(key, &s, ms(100));
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
        let (a, first) = leases.acquire(key, &s, ms(100));
        let (b, second) = leases.acquire(key, &s, ms(100));
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
        let (k1_tok, _) = leases.acquire(key, &s, ms(100));
        // A real token, but issued for a DIFFERENT key — unknown to key's entry.
        let (k2_tok, _) = leases.acquire(k2, &s, ms(100));
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
        leases.acquire(k1, &s, ms(100));
        leases.acquire(k2, &s, ms(100));
        assert_eq!(leases.len(), 2);
        assert_eq!(leases.entry_for_test(&k1), Some((1, ms(100))));
        assert_eq!(leases.entry_for_test(&k2), Some((1, ms(100))));
    }
}
