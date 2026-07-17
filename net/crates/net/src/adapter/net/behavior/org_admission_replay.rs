//! OA-2 §2.5 of `docs/plans/ORG_CAPABILITY_AUTH_PLAN.md` — the
//! admission replay guard.
//!
//! Authentication is never replay prevention (a pinned invariant):
//! a valid [`OrgCallProof`](super::org_call::OrgCallProof) can be
//! captured off the wire and re-sent byte-for-byte until it
//! expires. The §2.4 admission order therefore ends every accepted
//! proof with an ATOMIC insert-or-deny into this guard, keyed on
//! the nRPC correlation identity `(caller, call_id)` — NOT request
//! content — BEFORE the handler runs.
//!
//! ```text
//! same (caller, call_id), same binding digest      → Replay
//! same (caller, call_id), different binding digest  → CallIdCollision
//! new  (caller, call_id)                            → Admitted (recorded)
//! ```
//!
//! Keying on `(caller, call_id, binding_digest)` would be WRONG:
//! the same caller could reuse a `call_id` with a freshly signed
//! DIFFERENT binding and mint a new map key, side-stepping the
//! guard. Under correlation-identity keying, ANY reuse of
//! `(caller, call_id)` before expiry denies without a second
//! handler invocation — and the two reuse shapes are
//! distinguishable so a caller bug (id collision) reads
//! differently from an attack (replay).
//!
//! # Retention and capacity
//!
//! Entries are retained to the proof's expiry on a MONOTONIC clock
//! (a wall-clock jump must not evict a still-live guard). An
//! UNEXPIRED entry is NEVER evicted — the guard would otherwise
//! forget a proof still inside its replay window. A bounded map
//! ([`AdmissionReplayConfig::max_entries`]) caps memory against a
//! caller flooding novel `call_id`s; once full of unexpired
//! entries, new admissions DENY with [`ReplayOutcome::CapacityExhausted`]
//! and bump a metric rather than evicting a live guard. The guard
//! is VOLATILE by contract — cross-restart idempotency is the
//! application's concern (as it is for nRPC today).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use parking_lot::Mutex;

use crate::adapter::net::identity::EntityId;

/// Provisional ceiling on tracked in-flight+recent admissions
/// (plan §2.5: "constants frozen after measurement"). Sized so a
/// burst of legitimate concurrent callers fits comfortably while a
/// single caller cannot exhaust process memory with novel
/// `call_id`s. Flagged for OA-2 measurement before freeze.
pub const DEFAULT_MAX_REPLAY_ENTRIES: usize = 65_536;

/// Replay-guard ceilings. One field today; a struct so the OA-2
/// review can add per-caller sub-ceilings without a signature
/// break.
#[derive(Debug, Clone, Copy)]
pub struct AdmissionReplayConfig {
    /// Maximum simultaneously-retained `(caller, call_id)`
    /// entries. At capacity, a novel admission denies rather than
    /// evicting an unexpired guard.
    pub max_entries: usize,
}

impl Default for AdmissionReplayConfig {
    fn default() -> Self {
        Self {
            max_entries: DEFAULT_MAX_REPLAY_ENTRIES,
        }
    }
}

/// The outcome of an admission check. Only [`Self::Admitted`] lets
/// the handler run; the §2.4 engine maps the others to typed
/// `AdmissionDenied` reasons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayOutcome {
    /// First sight of this `(caller, call_id)` within its window —
    /// recorded; the handler may run.
    Admitted,
    /// The SAME proof (identical binding digest) re-presented
    /// before expiry — a replay.
    Replay,
    /// The same `(caller, call_id)` with a DIFFERENT binding
    /// digest — a correlation-id collision (caller bug or a
    /// forged reuse of an id).
    CallIdCollision,
    /// The guard is full of still-live entries; admitting would
    /// require evicting an unexpired guard, so this call is denied
    /// fail-closed.
    CapacityExhausted,
}

struct ReplayEntry {
    binding_digest: [u8; 32],
    /// Monotonic instant at/after which this entry is reusable.
    expires_at: Instant,
}

/// The volatile admission replay guard. One per provider node.
pub struct AdmissionReplayGuard {
    entries: Mutex<HashMap<(EntityId, u64), ReplayEntry>>,
    config: AdmissionReplayConfig,
    /// Count of admissions denied for capacity — a metric surface
    /// (§2.5: "deny + metric on exhaustion").
    capacity_denials: AtomicU64,
}

impl AdmissionReplayGuard {
    /// A guard with the given ceilings.
    pub fn new(config: AdmissionReplayConfig) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            config,
            capacity_denials: AtomicU64::new(0),
        }
    }

    /// A guard with the default ceilings.
    pub fn with_defaults() -> Self {
        Self::new(AdmissionReplayConfig::default())
    }

    /// Atomic insert-or-deny (the last step of §2.4). `now` is the
    /// monotonic clock (real callers pass `Instant::now()`;
    /// `expires_at` is `now + proof-remaining-ttl + skew`,
    /// precomputed by the caller so the guard never touches a wall
    /// clock).
    ///
    /// One lock acquisition covers eviction, the collision check,
    /// and the insert, so two concurrent presentations of one
    /// proof can never both see "absent" and both admit.
    pub fn admit(
        &self,
        caller: &EntityId,
        call_id: u64,
        binding_digest: [u8; 32],
        expires_at: Instant,
        now: Instant,
    ) -> ReplayOutcome {
        let key = (caller.clone(), call_id);
        let mut entries = self.entries.lock();

        // An existing entry for this key: replay vs collision,
        // UNLESS it has expired (then it is reusable — the window
        // closed, so this is a legitimate new call reusing the id).
        if let Some(existing) = entries.get(&key) {
            if existing.expires_at > now {
                return if existing.binding_digest == binding_digest {
                    ReplayOutcome::Replay
                } else {
                    ReplayOutcome::CallIdCollision
                };
            }
            // Expired: overwrite it below as a fresh admission.
            entries.insert(
                key,
                ReplayEntry {
                    binding_digest,
                    expires_at,
                },
            );
            return ReplayOutcome::Admitted;
        }

        // New key. If at capacity, reclaim only EXPIRED slots; if
        // none are reclaimable, deny fail-closed rather than evict
        // a live guard.
        if entries.len() >= self.config.max_entries {
            entries.retain(|_, e| e.expires_at > now);
            if entries.len() >= self.config.max_entries {
                self.capacity_denials.fetch_add(1, Ordering::Relaxed);
                return ReplayOutcome::CapacityExhausted;
            }
        }

        entries.insert(
            key,
            ReplayEntry {
                binding_digest,
                expires_at,
            },
        );
        ReplayOutcome::Admitted
    }

    /// Reclaim every entry whose window has closed as of `now`.
    /// Optional maintenance — [`Self::admit`] reclaims lazily at
    /// capacity — but a periodic sweep keeps steady-state memory
    /// low. Returns how many entries were reclaimed.
    pub fn evict_expired(&self, now: Instant) -> usize {
        let mut entries = self.entries.lock();
        let before = entries.len();
        entries.retain(|_, e| e.expires_at > now);
        before - entries.len()
    }

    /// Current tracked-entry count (test/metric surface).
    pub fn len(&self) -> usize {
        self.entries.lock().len()
    }

    /// `true` iff no entries are tracked.
    pub fn is_empty(&self) -> bool {
        self.entries.lock().is_empty()
    }

    /// Total admissions denied for capacity since construction.
    pub fn capacity_denials(&self) -> u64 {
        self.capacity_denials.load(Ordering::Relaxed)
    }
}

impl std::fmt::Debug for AdmissionReplayGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdmissionReplayGuard")
            .field("entries", &self.len())
            .field("max_entries", &self.config.max_entries)
            .field("capacity_denials", &self.capacity_denials())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn caller(byte: u8) -> EntityId {
        EntityId::from_bytes([byte; 32])
    }

    #[test]
    fn first_admit_records_and_replay_is_denied() {
        let guard = AdmissionReplayGuard::with_defaults();
        let now = Instant::now();
        let expires = now + Duration::from_secs(30);
        let digest = [1u8; 32];

        assert_eq!(
            guard.admit(&caller(1), 7, digest, expires, now),
            ReplayOutcome::Admitted
        );
        // Same proof re-presented within the window: replay.
        assert_eq!(
            guard.admit(&caller(1), 7, digest, expires, now),
            ReplayOutcome::Replay
        );
        assert_eq!(guard.len(), 1);
    }

    #[test]
    fn same_call_id_different_binding_is_a_collision() {
        let guard = AdmissionReplayGuard::with_defaults();
        let now = Instant::now();
        let expires = now + Duration::from_secs(30);

        assert_eq!(
            guard.admit(&caller(1), 7, [1u8; 32], expires, now),
            ReplayOutcome::Admitted
        );
        assert_eq!(
            guard.admit(&caller(1), 7, [2u8; 32], expires, now),
            ReplayOutcome::CallIdCollision
        );
    }

    #[test]
    fn distinct_callers_and_call_ids_are_independent() {
        let guard = AdmissionReplayGuard::with_defaults();
        let now = Instant::now();
        let expires = now + Duration::from_secs(30);
        let digest = [1u8; 32];

        assert_eq!(
            guard.admit(&caller(1), 7, digest, expires, now),
            ReplayOutcome::Admitted
        );
        // Different call_id, same caller: independent.
        assert_eq!(
            guard.admit(&caller(1), 8, digest, expires, now),
            ReplayOutcome::Admitted
        );
        // Different caller, same call_id: independent.
        assert_eq!(
            guard.admit(&caller(2), 7, digest, expires, now),
            ReplayOutcome::Admitted
        );
        assert_eq!(guard.len(), 3);
    }

    #[test]
    fn expired_entry_permits_legitimate_call_id_reuse() {
        let guard = AdmissionReplayGuard::with_defaults();
        let t0 = Instant::now();
        let expires = t0 + Duration::from_secs(30);
        let digest = [1u8; 32];

        assert_eq!(
            guard.admit(&caller(1), 7, digest, expires, t0),
            ReplayOutcome::Admitted
        );
        // Same key AFTER the window closes is a fresh, legitimate
        // call — not a replay.
        let later = t0 + Duration::from_secs(31);
        let new_expires = later + Duration::from_secs(30);
        assert_eq!(
            guard.admit(&caller(1), 7, digest, new_expires, later),
            ReplayOutcome::Admitted
        );
        // And the SAME proof within the NEW window is a replay again.
        assert_eq!(
            guard.admit(&caller(1), 7, digest, new_expires, later),
            ReplayOutcome::Replay
        );
    }

    #[test]
    fn capacity_denies_without_evicting_a_live_guard() {
        let guard = AdmissionReplayGuard::new(AdmissionReplayConfig { max_entries: 2 });
        let now = Instant::now();
        let expires = now + Duration::from_secs(30);

        assert_eq!(
            guard.admit(&caller(1), 1, [1u8; 32], expires, now),
            ReplayOutcome::Admitted
        );
        assert_eq!(
            guard.admit(&caller(2), 2, [2u8; 32], expires, now),
            ReplayOutcome::Admitted
        );
        // Full of LIVE entries: a novel admission is denied, and
        // the metric ticks — no live guard is dropped.
        assert_eq!(
            guard.admit(&caller(3), 3, [3u8; 32], expires, now),
            ReplayOutcome::CapacityExhausted
        );
        assert_eq!(guard.capacity_denials(), 1);
        assert_eq!(guard.len(), 2);
        // The still-live originals remain protected.
        assert_eq!(
            guard.admit(&caller(1), 1, [1u8; 32], expires, now),
            ReplayOutcome::Replay
        );
    }

    #[test]
    fn capacity_reclaims_expired_slots_before_denying() {
        let guard = AdmissionReplayGuard::new(AdmissionReplayConfig { max_entries: 2 });
        let t0 = Instant::now();
        let short = t0 + Duration::from_secs(10);
        let long = t0 + Duration::from_secs(60);

        assert_eq!(
            guard.admit(&caller(1), 1, [1u8; 32], short, t0),
            ReplayOutcome::Admitted
        );
        assert_eq!(
            guard.admit(&caller(2), 2, [2u8; 32], long, t0),
            ReplayOutcome::Admitted
        );
        // After caller(1)'s window closes, a novel admission at
        // capacity reclaims the expired slot instead of denying.
        let later = t0 + Duration::from_secs(11);
        assert_eq!(
            guard.admit(
                &caller(3),
                3,
                [3u8; 32],
                later + Duration::from_secs(30),
                later
            ),
            ReplayOutcome::Admitted
        );
        assert_eq!(guard.capacity_denials(), 0);
        // caller(2) (long window) survived; caller(1) was reclaimed.
        assert_eq!(guard.len(), 2);
    }

    #[test]
    fn evict_expired_reclaims_only_closed_windows() {
        let guard = AdmissionReplayGuard::with_defaults();
        let t0 = Instant::now();
        guard.admit(&caller(1), 1, [1u8; 32], t0 + Duration::from_secs(10), t0);
        guard.admit(&caller(2), 2, [2u8; 32], t0 + Duration::from_secs(60), t0);

        let reclaimed = guard.evict_expired(t0 + Duration::from_secs(11));
        assert_eq!(reclaimed, 1);
        assert_eq!(guard.len(), 1);
        // The unexpired one is untouched — still a replay.
        assert_eq!(
            guard.admit(
                &caller(2),
                2,
                [2u8; 32],
                t0 + Duration::from_secs(60),
                t0 + Duration::from_secs(11)
            ),
            ReplayOutcome::Replay
        );
    }

    #[test]
    fn concurrent_admissions_admit_exactly_once() {
        use std::sync::Arc;
        let guard = Arc::new(AdmissionReplayGuard::with_defaults());
        let now = Instant::now();
        let expires = now + Duration::from_secs(30);
        let digest = [7u8; 32];

        let admitted = Arc::new(AtomicU64::new(0));
        let replayed = Arc::new(AtomicU64::new(0));
        let mut handles = Vec::new();
        for _ in 0..16 {
            let guard = guard.clone();
            let admitted = admitted.clone();
            let replayed = replayed.clone();
            handles.push(std::thread::spawn(move || {
                match guard.admit(&caller(1), 42, digest, expires, now) {
                    ReplayOutcome::Admitted => {
                        admitted.fetch_add(1, Ordering::Relaxed);
                    }
                    ReplayOutcome::Replay => {
                        replayed.fetch_add(1, Ordering::Relaxed);
                    }
                    other => panic!("unexpected outcome {other:?}"),
                }
            }));
        }
        for h in handles {
            h.join().expect("join");
        }
        assert_eq!(admitted.load(Ordering::Relaxed), 1, "exactly one admit");
        assert_eq!(replayed.load(Ordering::Relaxed), 15, "the rest replay");
    }
}
