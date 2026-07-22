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
//! # Generation guard
//!
//! Every wire-changing action carries a globally monotonic `generation`. The
//! node records the highest generation it has installed for a key and applies
//! an action only when its generation exceeds that — so a deregister that races
//! a new acquire (and loses the lock order) cannot remove the successor
//! registration. The registry decides WHAT must happen; the node performs the
//! register/deregister.
//!
//! This is a landed **primitive**: the `SensingLeaseKey` / `LeaseAction` values
//! are consumed by the `MeshNode` acquire/release wiring in the next OLB-0
//! sub-phase (mirroring OA, which landed admission primitives before live
//! wiring).

use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use parking_lot::Mutex;

use super::identity::{strictest_sample_interval, AudienceScopeCommitment, Digest256};

/// Opaque per-holder token. [`SensingInterestLeases::acquire`] returns one;
/// [`SensingInterestLeases::release`] consumes it. Node-local; never on the
/// wire.
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

/// The wire transition a lease mutation requires. The registry is pure: it
/// decides what the node must do; the node performs the register/deregister and
/// enforces the generation guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseAction {
    /// First holder for this key — register the interest at `interval`.
    Register {
        /// The sample interval to register on the wire.
        interval: Duration,
        /// The monotonic generation stamp for this install.
        generation: u64,
    },
    /// The aggregate interval changed (tighter on acquire, looser when the
    /// strictest holder releases) — re-register at `interval`.
    Reregister {
        /// The new aggregate sample interval to install.
        interval: Duration,
        /// The monotonic generation stamp for this re-install.
        generation: u64,
    },
    /// Refcount changed but the installed interval did not — no wire op.
    Unchanged,
    /// Last holder released — deregister. `generation` stamps the removal above
    /// the last install so a racing re-acquire's register wins.
    Deregister {
        /// The monotonic generation stamp for this removal.
        generation: u64,
    },
}

impl LeaseAction {
    /// The generation of a wire-changing action, if any. `Unchanged` has none.
    pub fn generation(&self) -> Option<u64> {
        match self {
            LeaseAction::Register { generation, .. }
            | LeaseAction::Reregister { generation, .. }
            | LeaseAction::Deregister { generation } => Some(*generation),
            LeaseAction::Unchanged => None,
        }
    }
}

/// One key's shared registration state.
struct LeaseEntry {
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
    next_generation: AtomicU64,
}

impl SensingInterestLeases {
    fn mint_token(&self) -> LeaseToken {
        LeaseToken(self.next_token.fetch_add(1, Ordering::Relaxed))
    }

    fn mint_generation(&self) -> u64 {
        // Start at 1 so 0 can mean "nothing installed" on the node side.
        self.next_generation.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Acquire a reference to the interest `key` at the requested `interval`.
    ///
    /// The returned [`LeaseAction`] tells the node what wire transition to
    /// perform; the returned [`LeaseToken`] must be handed back to
    /// [`release`](Self::release) exactly once.
    pub fn acquire(&self, key: SensingLeaseKey, interval: Duration) -> (LeaseToken, LeaseAction) {
        let token = self.mint_token();
        let mut entries = self.entries.lock();
        match entries.entry(key) {
            Entry::Vacant(v) => {
                let generation = self.mint_generation();
                let mut registrations = HashMap::new();
                registrations.insert(token, interval);
                v.insert(LeaseEntry {
                    registrations,
                    installed_interval: interval,
                });
                (
                    token,
                    LeaseAction::Register {
                        interval,
                        generation,
                    },
                )
            }
            Entry::Occupied(mut o) => {
                let entry = o.get_mut();
                entry.registrations.insert(token, interval);
                // `installed_interval` is maintained as the exact minimum of all
                // live registrations, so the new minimum is just this holder's
                // interval against it — no fallible aggregation needed.
                let new_min = interval.min(entry.installed_interval);
                if new_min < entry.installed_interval {
                    entry.installed_interval = new_min;
                    let generation = self.mint_generation();
                    (
                        token,
                        LeaseAction::Reregister {
                            interval: new_min,
                            generation,
                        },
                    )
                } else {
                    (token, LeaseAction::Unchanged)
                }
            }
        }
    }

    /// Release a reference previously acquired for `key`.
    ///
    /// Releasing an unknown or already-released token is a no-op. The strictest
    /// holder leaving relaxes the cadence; the last holder leaving deregisters.
    pub fn release(&self, key: SensingLeaseKey, token: LeaseToken) -> LeaseAction {
        let mut entries = self.entries.lock();
        let Entry::Occupied(mut o) = entries.entry(key) else {
            return LeaseAction::Unchanged;
        };
        let entry = o.get_mut();
        if entry.registrations.remove(&token).is_none() {
            return LeaseAction::Unchanged;
        }
        if entry.registrations.is_empty() {
            let generation = self.mint_generation();
            o.remove();
            return LeaseAction::Deregister { generation };
        }
        // A retained holder remains (the emptiness case returned above), so the
        // aggregate is defined; fall back to the installed value defensively
        // rather than unwrapping.
        let new_min = strictest_sample_interval(entry.registrations.values().copied())
            .unwrap_or(entry.installed_interval);
        if new_min > entry.installed_interval {
            entry.installed_interval = new_min;
            let generation = self.mint_generation();
            LeaseAction::Reregister {
                interval: new_min,
                generation,
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

    fn audience(byte: u8) -> AudienceScopeCommitment {
        AudienceScopeCommitment::from_bytes([byte; 32])
    }

    fn digest(byte: u8) -> Digest256 {
        Digest256::from_bytes([byte; 32])
    }

    fn provider_free() -> SensingLeaseKey {
        SensingLeaseKey::ProviderFree {
            audience: audience(1),
            interest_digest: digest(1),
        }
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn first_acquire_registers_at_its_interval() {
        let leases = SensingInterestLeases::default();
        let (_t, action) = leases.acquire(provider_free(), ms(100));
        assert_eq!(
            action,
            LeaseAction::Register {
                interval: ms(100),
                generation: 1
            }
        );
        assert_eq!(leases.entry_for_test(&provider_free()), Some((1, ms(100))));
    }

    #[test]
    fn looser_second_acquire_is_unchanged() {
        let leases = SensingInterestLeases::default();
        leases.acquire(provider_free(), ms(100));
        let (_t, action) = leases.acquire(provider_free(), ms(500));
        assert_eq!(action, LeaseAction::Unchanged);
        assert_eq!(leases.entry_for_test(&provider_free()), Some((2, ms(100))));
    }

    #[test]
    fn stricter_second_acquire_reregisters_tighter() {
        let leases = SensingInterestLeases::default();
        let (_a, first) = leases.acquire(provider_free(), ms(500));
        let (_b, second) = leases.acquire(provider_free(), ms(100));
        assert_eq!(
            first,
            LeaseAction::Register {
                interval: ms(500),
                generation: 1
            }
        );
        assert_eq!(
            second,
            LeaseAction::Reregister {
                interval: ms(100),
                generation: 2
            }
        );
        assert!(second.generation() > first.generation());
    }

    #[test]
    fn releasing_a_non_strictest_holder_makes_no_wire_change() {
        let leases = SensingInterestLeases::default();
        let (_strict, _) = leases.acquire(provider_free(), ms(100));
        let (loose, _) = leases.acquire(provider_free(), ms(500));
        let action = leases.release(provider_free(), loose);
        assert_eq!(action, LeaseAction::Unchanged);
        assert_eq!(leases.entry_for_test(&provider_free()), Some((1, ms(100))));
    }

    #[test]
    fn releasing_the_strictest_holder_relaxes_the_cadence() {
        let leases = SensingInterestLeases::default();
        let (strict, _) = leases.acquire(provider_free(), ms(100));
        let (_loose, _) = leases.acquire(provider_free(), ms(500));
        let action = leases.release(provider_free(), strict);
        // Only two wire-changing actions occurred (the initial Register and this
        // relaxing Reregister); the looser acquire in between minted no
        // generation, so this is generation 2.
        assert_eq!(
            action,
            LeaseAction::Reregister {
                interval: ms(500),
                generation: 2
            }
        );
        assert_eq!(leases.entry_for_test(&provider_free()), Some((1, ms(500))));
    }

    #[test]
    fn last_release_deregisters_and_drops_the_entry() {
        let leases = SensingInterestLeases::default();
        let (only, reg) = leases.acquire(provider_free(), ms(100));
        let action = leases.release(provider_free(), only);
        match action {
            LeaseAction::Deregister { generation } => {
                assert!(generation > reg.generation().unwrap());
            }
            other => panic!("expected Deregister, got {other:?}"),
        }
        assert!(leases.is_empty());
    }

    #[test]
    fn equal_interval_holders_share_one_registration() {
        let leases = SensingInterestLeases::default();
        let (a, first) = leases.acquire(provider_free(), ms(100));
        let (b, second) = leases.acquire(provider_free(), ms(100));
        assert!(matches!(first, LeaseAction::Register { .. }));
        assert_eq!(second, LeaseAction::Unchanged);
        assert_eq!(leases.release(provider_free(), a), LeaseAction::Unchanged);
        assert!(matches!(
            leases.release(provider_free(), b),
            LeaseAction::Deregister { .. }
        ));
        assert!(leases.is_empty());
    }

    #[test]
    fn releasing_an_unknown_token_is_a_noop() {
        let leases = SensingInterestLeases::default();
        let k2 = SensingLeaseKey::ExactProvider {
            audience: audience(1),
            interest_digest: digest(1),
            provider: 9,
        };
        let (k1_tok, _) = leases.acquire(provider_free(), ms(100));
        // A real token, but issued for a DIFFERENT key — unknown to k1's entry.
        let (k2_tok, _) = leases.acquire(k2, ms(100));
        assert_eq!(
            leases.release(provider_free(), k2_tok),
            LeaseAction::Unchanged
        );
        assert_eq!(leases.entry_for_test(&provider_free()), Some((1, ms(100))));
        // Double release of k1's token: first deregisters, second is a noop
        // (the entry is gone, so the token is now unknown).
        assert!(matches!(
            leases.release(provider_free(), k1_tok),
            LeaseAction::Deregister { .. }
        ));
        assert_eq!(
            leases.release(provider_free(), k1_tok),
            LeaseAction::Unchanged
        );
    }

    #[test]
    fn generations_are_globally_monotonic_across_keys() {
        let leases = SensingInterestLeases::default();
        let k1 = provider_free();
        let k2 = SensingLeaseKey::ExactProvider {
            audience: audience(1),
            interest_digest: digest(1),
            provider: 7,
        };
        let (_a, ra) = leases.acquire(k1, ms(100));
        let (_b, rb) = leases.acquire(k2, ms(100));
        // Distinct keys never alias — two independent registrations.
        assert!(matches!(ra, LeaseAction::Register { .. }));
        assert!(matches!(rb, LeaseAction::Register { .. }));
        assert!(rb.generation() > ra.generation());
        assert_eq!(leases.len(), 2);
    }

    #[test]
    fn exact_provider_keys_do_not_alias_across_providers() {
        let leases = SensingInterestLeases::default();
        let p7 = SensingLeaseKey::ExactProvider {
            audience: audience(1),
            interest_digest: digest(1),
            provider: 7,
        };
        let p8 = SensingLeaseKey::ExactProvider {
            audience: audience(1),
            interest_digest: digest(1),
            provider: 8,
        };
        leases.acquire(p7, ms(100));
        leases.acquire(p8, ms(100));
        assert_eq!(leases.len(), 2);
        assert_eq!(leases.entry_for_test(&p7), Some((1, ms(100))));
        assert_eq!(leases.entry_for_test(&p8), Some((1, ms(100))));
    }
}
