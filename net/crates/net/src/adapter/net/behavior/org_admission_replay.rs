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

/// Provisional per-caller ceiling (E1.5, verdict §10). Policy runs
/// AFTER replay insertion, so even a policy-vetoed VALID proof
/// consumes a slot; without a per-caller sub-ceiling a single
/// credentialed caller could fill the whole global map and starve
/// every other org fail-closed. Sized to admit a healthy concurrent
/// burst from one caller while leaving ample global headroom for
/// everyone else (16× fits under the global default). Flagged for
/// OA-2 measurement before freeze.
pub const DEFAULT_MAX_REPLAY_ENTRIES_PER_CALLER: usize = 4_096;

/// Replay-guard ceilings — a global map cap plus a per-caller
/// sub-ceiling (E1.5) so one caller cannot consume another's
/// allocation.
#[derive(Debug, Clone, Copy)]
pub struct AdmissionReplayConfig {
    /// Maximum simultaneously-retained `(caller, call_id)`
    /// entries across ALL callers. At capacity, a novel admission
    /// denies rather than evicting an unexpired guard.
    pub max_entries: usize,
    /// Maximum simultaneously-retained entries for ONE caller.
    /// Checked before the global cap, so a flooding caller hits its
    /// own ceiling first and never denies other callers.
    pub max_entries_per_caller: usize,
}

impl Default for AdmissionReplayConfig {
    fn default() -> Self {
        Self {
            max_entries: DEFAULT_MAX_REPLAY_ENTRIES,
            max_entries_per_caller: DEFAULT_MAX_REPLAY_ENTRIES_PER_CALLER,
        }
    }
}

impl AdmissionReplayConfig {
    /// Enforce the ceiling invariant (Kyra E1 audit): both bounds are
    /// positive AND the per-caller ceiling is STRICTLY below the
    /// global one. A `max_entries_per_caller >= max_entries` would let
    /// a single caller fill the entire global guard and starve every
    /// other org — the exact starvation the per-caller ceiling exists
    /// to prevent. Validated loudly at construction rather than
    /// silently clamped.
    pub fn validate(&self) -> Result<(), ReplayConfigError> {
        if self.max_entries == 0 {
            return Err(ReplayConfigError::ZeroGlobalCeiling);
        }
        if self.max_entries_per_caller == 0 {
            return Err(ReplayConfigError::ZeroPerCallerCeiling);
        }
        if self.max_entries_per_caller >= self.max_entries {
            return Err(ReplayConfigError::PerCallerNotBelowGlobal {
                per_caller: self.max_entries_per_caller,
                global: self.max_entries,
            });
        }
        Ok(())
    }
}

/// An invalid [`AdmissionReplayConfig`] (Kyra E1 audit).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ReplayConfigError {
    /// `max_entries == 0` — the global guard could never admit.
    #[error("replay max_entries must be > 0")]
    ZeroGlobalCeiling,
    /// `max_entries_per_caller == 0` — no caller could ever admit.
    #[error("replay max_entries_per_caller must be > 0")]
    ZeroPerCallerCeiling,
    /// `max_entries_per_caller >= max_entries` — one caller could
    /// consume the entire global guard.
    #[error("replay max_entries_per_caller ({per_caller}) must be < max_entries ({global})")]
    PerCallerNotBelowGlobal {
        /// The configured per-caller ceiling.
        per_caller: usize,
        /// The configured global ceiling.
        global: usize,
    },
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
    /// The GLOBAL guard is full of still-live entries; admitting
    /// would require evicting an unexpired guard, so this call is
    /// denied fail-closed.
    CapacityExhausted,
    /// THIS caller already holds the maximum simultaneously-retained
    /// entries (E1.5). Denies only this caller — every other
    /// caller's allocation is untouched, so one flooding org cannot
    /// starve the rest.
    PerCallerCapacityExhausted,
}

struct ReplayEntry {
    binding_digest: [u8; 32],
    /// Monotonic instant at/after which this entry is reusable.
    expires_at: Instant,
}

/// The mutex-guarded state. Nested `caller → (call_id → entry)` so
/// the per-caller ceiling and per-caller reclamation touch ONLY one
/// caller's entries (E1.5); `total` mirrors the summed inner lengths
/// so the global cap is a field read, not an O(callers) sum.
#[derive(Default)]
struct ReplayState {
    by_caller: HashMap<EntityId, HashMap<u64, ReplayEntry>>,
    total: usize,
}

impl ReplayState {
    /// Drop `caller`'s expired entries (and the caller bucket if it
    /// empties). Returns nothing; keeps `total` in step.
    fn reclaim_caller(&mut self, caller: &EntityId, now: Instant) {
        if let Some(inner) = self.by_caller.get_mut(caller) {
            let before = inner.len();
            inner.retain(|_, e| e.expires_at > now);
            self.total -= before - inner.len();
            if inner.is_empty() {
                self.by_caller.remove(caller);
            }
        }
    }

    /// Drop every expired entry across all callers. Returns the
    /// number reclaimed.
    fn reclaim_all(&mut self, now: Instant) -> usize {
        let mut removed = 0usize;
        self.by_caller.retain(|_, inner| {
            let before = inner.len();
            inner.retain(|_, e| e.expires_at > now);
            removed += before - inner.len();
            !inner.is_empty()
        });
        self.total -= removed;
        removed
    }
}

/// The volatile admission replay guard. One per provider node.
pub struct AdmissionReplayGuard {
    entries: Mutex<ReplayState>,
    config: AdmissionReplayConfig,
    /// Count of admissions denied for GLOBAL capacity — a metric
    /// surface (§2.5: "deny + metric on exhaustion").
    capacity_denials: AtomicU64,
    /// Count of admissions denied for PER-CALLER capacity (E1.5) —
    /// a separate metric so operators can tell a fleet-wide flood
    /// from a single abusive caller.
    per_caller_denials: AtomicU64,
}

impl AdmissionReplayGuard {
    /// A guard with the given ceilings, VALIDATED (Kyra E1 audit) —
    /// see [`AdmissionReplayConfig::validate`]. Prefer this over
    /// [`Self::new`] on any config not known-good at compile time.
    pub fn try_new(config: AdmissionReplayConfig) -> Result<Self, ReplayConfigError> {
        config.validate()?;
        Ok(Self::from_validated(config))
    }

    fn from_validated(config: AdmissionReplayConfig) -> Self {
        Self {
            entries: Mutex::new(ReplayState::default()),
            config,
            capacity_denials: AtomicU64::new(0),
            per_caller_denials: AtomicU64::new(0),
        }
    }

    /// A guard with the given ceilings. Panics on an invalid config
    /// (loud, not silently clamped) — use [`Self::try_new`] when the
    /// config comes from untrusted/dynamic input.
    pub fn new(config: AdmissionReplayConfig) -> Self {
        match Self::try_new(config) {
            Ok(guard) => guard,
            Err(e) => panic!("invalid AdmissionReplayConfig: {e}"),
        }
    }

    /// A guard with the default ceilings (always valid).
    pub fn with_defaults() -> Self {
        Self::from_validated(AdmissionReplayConfig::default())
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
        let mut st = self.entries.lock();

        // An existing entry for this exact `(caller, call_id)`:
        // replay vs collision, UNLESS it has expired (then it is
        // reusable — the window closed, so this is a legitimate new
        // call reusing the id). Handled under one `get_mut` so the
        // expired overwrite touches neither `total` nor the
        // per-caller count (the key stays occupied).
        if let Some(inner) = st.by_caller.get_mut(caller) {
            if let Some(existing) = inner.get(&call_id) {
                if existing.expires_at > now {
                    return if existing.binding_digest == binding_digest {
                        ReplayOutcome::Replay
                    } else {
                        ReplayOutcome::CallIdCollision
                    };
                }
                inner.insert(
                    call_id,
                    ReplayEntry {
                        binding_digest,
                        expires_at,
                    },
                );
                return ReplayOutcome::Admitted;
            }
        }

        // New key for this caller. Per-caller ceiling FIRST (E1.5) so
        // a flooding caller hits its own limit before it can pressure
        // the global cap. At capacity, reclaim only THIS caller's
        // expired slots; if still full, deny only this caller.
        let caller_live = st.by_caller.get(caller).map_or(0, HashMap::len);
        if caller_live >= self.config.max_entries_per_caller {
            st.reclaim_caller(caller, now);
            let caller_live = st.by_caller.get(caller).map_or(0, HashMap::len);
            if caller_live >= self.config.max_entries_per_caller {
                self.per_caller_denials.fetch_add(1, Ordering::Relaxed);
                return ReplayOutcome::PerCallerCapacityExhausted;
            }
        }

        // Global ceiling. Reclaim EXPIRED slots fleet-wide; if none
        // are reclaimable, deny fail-closed rather than evict a live
        // guard.
        if st.total >= self.config.max_entries {
            st.reclaim_all(now);
            if st.total >= self.config.max_entries {
                self.capacity_denials.fetch_add(1, Ordering::Relaxed);
                return ReplayOutcome::CapacityExhausted;
            }
        }

        st.by_caller.entry(caller.clone()).or_default().insert(
            call_id,
            ReplayEntry {
                binding_digest,
                expires_at,
            },
        );
        st.total += 1;
        ReplayOutcome::Admitted
    }

    /// Reclaim every entry whose window has closed as of `now`.
    /// Optional maintenance — [`Self::admit`] reclaims lazily at
    /// capacity — but a periodic sweep keeps steady-state memory
    /// low. Returns how many entries were reclaimed.
    pub fn evict_expired(&self, now: Instant) -> usize {
        self.entries.lock().reclaim_all(now)
    }

    /// Current tracked-entry count across all callers (test/metric
    /// surface).
    pub fn len(&self) -> usize {
        self.entries.lock().total
    }

    /// `true` iff no entries are tracked.
    pub fn is_empty(&self) -> bool {
        self.entries.lock().total == 0
    }

    /// Number of entries currently tracked for one caller
    /// (test/metric surface).
    pub fn caller_len(&self, caller: &EntityId) -> usize {
        self.entries
            .lock()
            .by_caller
            .get(caller)
            .map_or(0, HashMap::len)
    }

    /// Total admissions denied for GLOBAL capacity since construction.
    pub fn capacity_denials(&self) -> u64 {
        self.capacity_denials.load(Ordering::Relaxed)
    }

    /// Total admissions denied for PER-CALLER capacity (E1.5) since
    /// construction.
    pub fn per_caller_denials(&self) -> u64 {
        self.per_caller_denials.load(Ordering::Relaxed)
    }
}

impl std::fmt::Debug for AdmissionReplayGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdmissionReplayGuard")
            .field("entries", &self.len())
            .field("max_entries", &self.config.max_entries)
            .field(
                "max_entries_per_caller",
                &self.config.max_entries_per_caller,
            )
            .field("capacity_denials", &self.capacity_denials())
            .field("per_caller_denials", &self.per_caller_denials())
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
        let guard = AdmissionReplayGuard::new(AdmissionReplayConfig {
            max_entries: 2,
            max_entries_per_caller: 1,
        });
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
        let guard = AdmissionReplayGuard::new(AdmissionReplayConfig {
            max_entries: 2,
            max_entries_per_caller: 1,
        });
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

    /// E1.5 witness 21 — one caller cannot consume another's replay
    /// allocation. Caller(1) fills its per-caller ceiling; a further
    /// NOVEL call from caller(1) is denied `PerCallerCapacityExhausted`,
    /// yet caller(2) admits freely and the GLOBAL capacity denial
    /// metric never ticks.
    #[test]
    fn per_caller_ceiling_isolates_a_flooding_caller() {
        let guard = AdmissionReplayGuard::new(AdmissionReplayConfig {
            max_entries: 1_000,
            max_entries_per_caller: 3,
        });
        let now = Instant::now();
        let expires = now + Duration::from_secs(30);

        // caller(1) fills its per-caller allocation with 3 novel calls.
        for call_id in 0..3u64 {
            assert_eq!(
                guard.admit(&caller(1), call_id, [call_id as u8; 32], expires, now),
                ReplayOutcome::Admitted
            );
        }
        assert_eq!(guard.caller_len(&caller(1)), 3);

        // The 4th novel call from caller(1) is denied — only caller(1).
        assert_eq!(
            guard.admit(&caller(1), 99, [9u8; 32], expires, now),
            ReplayOutcome::PerCallerCapacityExhausted
        );
        assert_eq!(guard.per_caller_denials(), 1);
        assert_eq!(guard.capacity_denials(), 0, "global cap never fired");

        // caller(2) is entirely unaffected — its allocation is its own.
        for call_id in 0..3u64 {
            assert_eq!(
                guard.admit(&caller(2), call_id, [call_id as u8; 32], expires, now),
                ReplayOutcome::Admitted
            );
        }
        assert_eq!(guard.caller_len(&caller(2)), 3);
        // A still-live replay from caller(1) is unchanged behavior.
        assert_eq!(
            guard.admit(&caller(1), 0, [0u8; 32], expires, now),
            ReplayOutcome::Replay
        );
    }

    /// The per-caller ceiling reclaims that caller's EXPIRED slots
    /// before denying, so a caller whose earlier calls have aged out
    /// can keep making new ones without ever touching other callers.
    #[test]
    fn per_caller_ceiling_reclaims_expired_before_denying() {
        let guard = AdmissionReplayGuard::new(AdmissionReplayConfig {
            max_entries: 1_000,
            max_entries_per_caller: 2,
        });
        let t0 = Instant::now();
        let short = t0 + Duration::from_secs(10);

        guard.admit(&caller(1), 1, [1u8; 32], short, t0);
        guard.admit(&caller(1), 2, [2u8; 32], short, t0);
        assert_eq!(guard.caller_len(&caller(1)), 2);

        // After caller(1)'s window closes, a novel call at the
        // per-caller cap reclaims the expired slots instead of denying.
        let later = t0 + Duration::from_secs(11);
        assert_eq!(
            guard.admit(
                &caller(1),
                3,
                [3u8; 32],
                later + Duration::from_secs(30),
                later
            ),
            ReplayOutcome::Admitted
        );
        assert_eq!(guard.per_caller_denials(), 0);
        assert_eq!(guard.caller_len(&caller(1)), 1, "expired slots reclaimed");
    }

    /// KC8 — config validation boundaries (Kyra E1 audit). The
    /// invariant is `0 < max_entries_per_caller < max_entries`, loud
    /// via `try_new`, so no config can let one caller consume the
    /// whole global guard.
    #[test]
    fn replay_config_validation_boundaries() {
        // Valid: strictly below.
        assert!(AdmissionReplayConfig {
            max_entries: 10,
            max_entries_per_caller: 9,
        }
        .validate()
        .is_ok());
        assert!(AdmissionReplayGuard::try_new(AdmissionReplayConfig {
            max_entries: 10,
            max_entries_per_caller: 9,
        })
        .is_ok());
        // Defaults are valid.
        assert!(AdmissionReplayConfig::default().validate().is_ok());

        // per_caller == max_entries → rejected.
        assert_eq!(
            AdmissionReplayConfig {
                max_entries: 8,
                max_entries_per_caller: 8,
            }
            .validate(),
            Err(ReplayConfigError::PerCallerNotBelowGlobal {
                per_caller: 8,
                global: 8,
            }),
        );
        // per_caller > max_entries → rejected.
        assert!(matches!(
            AdmissionReplayConfig {
                max_entries: 8,
                max_entries_per_caller: 9,
            }
            .validate(),
            Err(ReplayConfigError::PerCallerNotBelowGlobal { .. }),
        ));
        // Zero ceilings → rejected.
        assert_eq!(
            AdmissionReplayConfig {
                max_entries: 0,
                max_entries_per_caller: 0,
            }
            .validate(),
            Err(ReplayConfigError::ZeroGlobalCeiling),
        );
        assert_eq!(
            AdmissionReplayConfig {
                max_entries: 4,
                max_entries_per_caller: 0,
            }
            .validate(),
            Err(ReplayConfigError::ZeroPerCallerCeiling),
        );
        // try_new surfaces the error rather than clamping.
        assert!(AdmissionReplayGuard::try_new(AdmissionReplayConfig {
            max_entries: 8,
            max_entries_per_caller: 8,
        })
        .is_err());
    }

    /// The loud `new` panics on an invalid config (never clamps).
    #[test]
    #[should_panic(expected = "invalid AdmissionReplayConfig")]
    fn replay_new_panics_on_invalid_config() {
        let _ = AdmissionReplayGuard::new(AdmissionReplayConfig {
            max_entries: 4,
            max_entries_per_caller: 4,
        });
    }
}
