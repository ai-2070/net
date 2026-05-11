//! `HeatCounter` — per-chain read-rate with exponential decay.
//!
//! Pure data structure. The runtime layer calls
//! [`HeatCounter::bump`] on every authorized read and consults
//! [`HeatCounter::rate`] before the throttled emission check
//! (`should_emit_heat`).
//!
//! Decay function: `rate := rate × 0.5^((now - last_update) / half_life)`.
//! Equivalent to: each call's contribution decays geometrically
//! with the elapsed time since the previous bump.
//!
//! The "rate" units are deliberately abstract — the counter
//! measures bumps-per-half-life, not bumps-per-hour or bumps-
//! per-second. Operators interpret heat values comparatively
//! within a deployment, not absolutely; the wire-form tag
//! (`heat:<hex>=<rate>`) carries the same raw value the counter
//! holds.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Per-chain heat counter.
#[derive(Debug, Clone)]
pub struct HeatCounter {
    /// Current decayed rate. Bumps add `1.0`; decay scales by
    /// `0.5^((now - last_update) / half_life)`.
    rate: f64,
    /// Last `Instant` the rate was updated (either via `bump` or
    /// `decay_to`). Drives the decay multiplier on the next
    /// observation.
    last_update: Instant,
    /// The rate at the last `record_emission` call, or `None` if
    /// no emission has happened yet. Drives the `should_emit_heat`
    /// threshold check.
    last_emitted: Option<f64>,
    /// Exponential-decay half-life. Tied to the channel's
    /// `DataGravityPolicy::decay_half_life`.
    half_life: Duration,
}

impl HeatCounter {
    /// Fresh counter — zero rate, no prior emission.
    pub fn new(half_life: Duration, now: Instant) -> Self {
        Self {
            rate: 0.0,
            last_update: now,
            last_emitted: None,
            half_life,
        }
    }

    /// Apply decay through `now` without bumping. Use when
    /// inspecting the current rate without observing a new
    /// event.
    pub fn decay_to(&mut self, now: Instant) {
        if self.half_life.is_zero() || self.rate == 0.0 {
            self.last_update = now;
            return;
        }
        let elapsed = now.saturating_duration_since(self.last_update);
        let half_lives = elapsed.as_secs_f64() / self.half_life.as_secs_f64();
        // Defensive clamp — very long elapses can saturate the
        // exponent at f64::MIN_POSITIVE. Treat anything past
        // 64 half-lives (≈ ratio 1.8e-20) as zero.
        if half_lives > 64.0 {
            self.rate = 0.0;
        } else {
            self.rate *= 0.5_f64.powf(half_lives);
            if self.rate < f64::EPSILON {
                self.rate = 0.0;
            }
        }
        self.last_update = now;
    }

    /// Observe a read at `now`. Decays the prior rate, then
    /// adds `1.0`.
    pub fn bump(&mut self, now: Instant) {
        self.decay_to(now);
        self.rate += 1.0;
    }

    /// Read the current (decayed) rate without mutating state.
    /// Useful in tests; production callers should `decay_to(now)`
    /// first to reflect time elapsed since the last bump.
    pub fn rate(&self) -> f64 {
        self.rate
    }

    /// The value carried in the last emitted heat tag, if any.
    pub fn last_emitted(&self) -> Option<f64> {
        self.last_emitted
    }

    /// Record that an emission with `rate` just landed. Future
    /// `should_emit_heat` calls compare against this snapshot.
    pub fn record_emission(&mut self, rate: f64) {
        self.last_emitted = Some(rate);
    }

    /// Record a withdrawal (heat=0 emitted). Equivalent to
    /// `record_emission(0.0)`.
    pub fn record_withdrawal(&mut self) {
        self.last_emitted = Some(0.0);
    }

    /// Half-life this counter was constructed with. Read-only —
    /// reconfigure by replacing the counter.
    pub fn half_life(&self) -> Duration {
        self.half_life
    }

    /// Last `Instant` the rate was updated (via `bump` or
    /// `decay_to`). Used by the registry's LRU eviction to pick
    /// the least-recently-touched counter under cap pressure.
    pub fn last_update(&self) -> Instant {
        self.last_update
    }
}

/// Outcome of one runtime tick over a channel's heat counter.
/// Returned by [`HeatRegistry::tick`] so the runtime can route
/// each path to the right wire action without re-deciding the
/// case.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HeatEmission {
    /// No emission — caller does nothing.
    Suppress,
    /// Emit a `heat:<origin_hash_hex>=<rate>` tag.
    Emit {
        /// Decayed read-rate to carry on the wire.
        rate: f64,
    },
    /// Emit a withdrawal — `heat:<origin_hash_hex>=0`.
    Withdraw,
}

/// Default cap on tracked heat counters. Sized to comfortably
/// hold a busy node's working set without growing unboundedly
/// under churn (each counter is ~48 bytes + hashmap overhead,
/// so 8K entries fit in <1 MiB). Operators with workloads beyond
/// this raise via [`HeatRegistry::with_cap`].
pub const DEFAULT_HEAT_REGISTRY_CAP: usize = 8 * 1024;

/// Cluster-wide heat registry. Keyed by `u64` origin_hash — the
/// same chain identifier the substrate's `causal:<hex>` and
/// `heat:<hex>=<rate>` reserved tags carry on the wire.
///
/// Per-channel state is mutated under the registry's outer mutex.
/// The hot path (`bump` per read) takes the lock briefly; total
/// cost is dominated by the decay arithmetic, which is two
/// `as_secs_f64` + one `powf`. Acceptable for read paths today;
/// if telemetry shows contention we can shard by channel-hash.
///
/// The registry has an independent **cap + LRU-style replacement**
/// so the entry count stays bounded even in deployments where
/// the greedy cache (the usual eviction driver) isn't running, or
/// is sized so generously that it never evicts. Past the cap, an
/// `entry_mut` insert evicts the entry with the oldest
/// `last_update` first. The cap defaults to
/// [`DEFAULT_HEAT_REGISTRY_CAP`]; operators tune via
/// [`HeatRegistry::with_cap`].
#[derive(Debug)]
pub struct HeatRegistry {
    counters: HashMap<u64, HeatCounter>,
    cap: usize,
}

impl Default for HeatRegistry {
    fn default() -> Self {
        Self {
            counters: HashMap::new(),
            cap: DEFAULT_HEAT_REGISTRY_CAP,
        }
    }
}

impl HeatRegistry {
    /// Empty registry with the default cap.
    pub fn new() -> Self {
        Self::default()
    }

    /// Empty registry with an explicit cap. `cap == 0` disables
    /// the bound (use only when an external loop guarantees
    /// bounded entries — typically the greedy cache wiring).
    pub fn with_cap(cap: usize) -> Self {
        Self {
            counters: HashMap::new(),
            cap,
        }
    }

    /// Configured cap. `0` means unbounded.
    pub fn cap(&self) -> usize {
        self.cap
    }

    /// Number of tracked channels.
    pub fn len(&self) -> usize {
        self.counters.len()
    }

    /// True iff zero channels tracked.
    pub fn is_empty(&self) -> bool {
        self.counters.is_empty()
    }

    /// Get-or-create the counter for `channel`. Returns a
    /// mutable reference so the caller can `bump` / `decay_to`
    /// / `record_emission` in one borrow.
    ///
    /// When `len() == cap` and the inserted key is new, the entry
    /// with the oldest `last_update` is evicted first (LRU-style
    /// replacement). `cap == 0` disables the bound.
    pub fn entry_mut(
        &mut self,
        channel: u64,
        half_life: Duration,
        now: Instant,
    ) -> &mut HeatCounter {
        if !self.counters.contains_key(&channel)
            && self.cap > 0
            && self.counters.len() >= self.cap
        {
            self.evict_lru();
        }
        self.counters
            .entry(channel)
            .or_insert_with(|| HeatCounter::new(half_life, now))
    }

    /// Evict the counter whose `last_update` is oldest. O(n) over
    /// tracked counters; runs at most once per `entry_mut` past
    /// the cap, so amortized cost stays bounded under steady-state
    /// churn. No-op when empty.
    fn evict_lru(&mut self) {
        let victim = self
            .counters
            .iter()
            .min_by_key(|(_, c)| c.last_update())
            .map(|(k, _)| *k);
        if let Some(key) = victim {
            self.counters.remove(&key);
        }
    }

    /// Read-only access to the counter for `channel`.
    pub fn get(&self, channel: &u64) -> Option<&HeatCounter> {
        self.counters.get(channel)
    }

    /// Remove the counter for `channel`. Used on channel close /
    /// cache eviction.
    pub fn remove(&mut self, channel: &u64) {
        self.counters.remove(channel);
    }

    /// Iterate `(channel, counter)` pairs. Read-only.
    pub fn iter(&self) -> impl Iterator<Item = (&u64, &HeatCounter)> {
        self.counters.iter()
    }

    /// Walk every tracked channel, applying decay through `now`
    /// and asking [`super::should_emit_heat`] whether to emit.
    /// Returns the list of `(channel, decision)` pairs the
    /// runtime acts on.
    ///
    /// Records the emission against the counter for `Emit` /
    /// `Withdraw` decisions before returning, so the next tick
    /// sees the updated `last_emitted` snapshot.
    ///
    /// After the per-counter pass, prunes entries whose rate has
    /// fully decayed to zero AND have already emitted a
    /// withdrawal — there's no future state transition possible
    /// (any new bump would re-enter via `entry_mut`), so keeping
    /// them around just bloats the map and slows subsequent
    /// ticks.
    pub fn tick(
        &mut self,
        policy: &super::DataGravityPolicy,
        now: Instant,
    ) -> Vec<(u64, HeatEmission)> {
        let mut out = Vec::new();
        for (channel, counter) in self.counters.iter_mut() {
            counter.decay_to(now);
            let decision = super::should_emit_heat(counter.rate, counter.last_emitted, policy);
            let emission = match decision {
                super::EmissionDecision::Suppress => HeatEmission::Suppress,
                super::EmissionDecision::Emit { rate } => {
                    counter.record_emission(rate);
                    HeatEmission::Emit { rate }
                }
                super::EmissionDecision::Withdraw => {
                    counter.record_withdrawal();
                    HeatEmission::Withdraw
                }
            };
            if !matches!(emission, HeatEmission::Suppress) {
                out.push((*channel, emission));
            }
        }
        // Prune fully-decayed + already-withdrawn entries. A future
        // bump for the same origin re-enters the registry via
        // `entry_mut`; the LRU cap protects against unbounded
        // re-entries.
        self.counters
            .retain(|_, c| !(c.rate == 0.0 && c.last_emitted == Some(0.0)));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(seed: u64) -> u64 {
        // Tests just need a stable distinct identifier per
        // "channel"; the production wire uses the chain's
        // origin_hash here.
        0xCAFE_BABE_0000_0000 | seed
    }

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn fresh_counter_is_zero() {
        let c = HeatCounter::new(Duration::from_secs(60), t0());
        assert_eq!(c.rate(), 0.0);
        assert_eq!(c.last_emitted(), None);
    }

    #[test]
    fn bump_adds_one_when_no_decay() {
        let base = t0();
        let mut c = HeatCounter::new(Duration::from_secs(60), base);
        c.bump(base);
        assert!((c.rate() - 1.0).abs() < 1e-9);
        c.bump(base);
        assert!((c.rate() - 2.0).abs() < 1e-9);
    }

    #[test]
    fn one_half_life_decays_rate_by_half() {
        let base = t0();
        let half = Duration::from_secs(60);
        let mut c = HeatCounter::new(half, base);
        c.bump(base);
        c.bump(base);
        c.bump(base);
        c.bump(base);
        // rate ≈ 4.0 at base.
        c.decay_to(base + half);
        assert!(
            (c.rate() - 2.0).abs() < 1e-6,
            "rate after half-life ≈ 2.0; got {}",
            c.rate()
        );
        c.decay_to(base + half * 2);
        assert!((c.rate() - 1.0).abs() < 1e-6);
        c.decay_to(base + half * 3);
        assert!((c.rate() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn long_elapse_clamps_to_zero() {
        let base = t0();
        let half = Duration::from_secs(60);
        let mut c = HeatCounter::new(half, base);
        c.bump(base);
        // 100 half-lives — past the clamp threshold (64).
        c.decay_to(base + half * 100);
        assert_eq!(c.rate(), 0.0);
    }

    #[test]
    fn bump_decays_then_adds() {
        let base = t0();
        let half = Duration::from_secs(60);
        let mut c = HeatCounter::new(half, base);
        c.bump(base);
        c.bump(base);
        // rate = 2.0 at base.
        c.bump(base + half);
        // decay 2.0 → 1.0, then +1.0 → 2.0.
        assert!((c.rate() - 2.0).abs() < 1e-6);
    }

    #[test]
    fn record_emission_tracks_last() {
        let base = t0();
        let mut c = HeatCounter::new(Duration::from_secs(60), base);
        c.bump(base);
        c.record_emission(1.5);
        assert_eq!(c.last_emitted(), Some(1.5));
        c.record_withdrawal();
        assert_eq!(c.last_emitted(), Some(0.0));
    }

    // ---- HeatRegistry ----

    #[test]
    fn new_registry_is_empty() {
        let r = HeatRegistry::new();
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
    }

    #[test]
    fn entry_mut_creates_on_first_access() {
        let mut r = HeatRegistry::new();
        let half = Duration::from_secs(60);
        let counter = r.entry_mut(channel(0xA), half, t0());
        counter.bump(t0());
        assert!((counter.rate() - 1.0).abs() < 1e-9);
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn remove_drops_entry() {
        let mut r = HeatRegistry::new();
        let half = Duration::from_secs(60);
        let _ = r.entry_mut(channel(0xA), half, t0());
        r.remove(&channel(0xA));
        assert!(r.is_empty());
    }

    #[test]
    fn entry_mut_at_cap_evicts_lru_on_new_insert() {
        // Cap of 2; insert three distinct chains, each touched at
        // a strictly-later Instant. The first chain is LRU at the
        // moment of the third insert and must evict.
        let base = t0();
        let mut r = HeatRegistry::with_cap(2);
        let half = Duration::from_secs(60);

        let _ = r.entry_mut(channel(0xA), half, base);
        let _ = r.entry_mut(channel(0xB), half, base + Duration::from_secs(1));
        assert_eq!(r.len(), 2);
        // Bumping B updates its last_update past A — making A the
        // LRU. The third insert (C) evicts A.
        let bumped = r.entry_mut(channel(0xB), half, base + Duration::from_secs(2));
        bumped.bump(base + Duration::from_secs(2));
        let _ = r.entry_mut(channel(0xC), half, base + Duration::from_secs(3));
        assert_eq!(r.len(), 2);
        assert!(r.get(&channel(0xA)).is_none(), "LRU entry A evicted");
        assert!(r.get(&channel(0xB)).is_some());
        assert!(r.get(&channel(0xC)).is_some());
    }

    #[test]
    fn entry_mut_cap_zero_is_unbounded() {
        let base = t0();
        let mut r = HeatRegistry::with_cap(0);
        let half = Duration::from_secs(60);
        for i in 0..100u64 {
            let _ = r.entry_mut(channel(i), half, base);
        }
        assert_eq!(r.len(), 100);
    }

    #[test]
    fn tick_prunes_fully_decayed_withdrawn_entries() {
        // After withdrawal + full decay, the entry is bookkeeping
        // noise. tick prunes it so subsequent ticks stay O(active
        // chains), not O(historical chains).
        let base = t0();
        let mut r = HeatRegistry::new();
        let policy = super::super::DataGravityPolicy::default();
        let half = policy.decay_half_life;

        // Bump once, emit, then let the rate decay to zero and
        // tick again to emit the withdrawal.
        let counter = r.entry_mut(channel(0xA), half, base);
        counter.bump(base);
        let _ = r.tick(&policy, base);
        assert_eq!(r.len(), 1);

        // 100 half-lives → rate clamps to zero; next tick emits
        // a withdrawal AND prunes the now-quiescent entry.
        let later = base + half * 100;
        let emissions = r.tick(&policy, later);
        assert!(emissions
            .iter()
            .any(|(_, e)| matches!(e, HeatEmission::Withdraw)));
        let after = r.tick(&policy, later + Duration::from_secs(1));
        assert!(after.is_empty(), "no further emissions");
        assert_eq!(r.len(), 0, "fully-decayed withdrawn entry pruned");
    }

    #[test]
    fn tick_emits_first_observation() {
        let base = t0();
        let mut r = HeatRegistry::new();
        let policy = super::super::DataGravityPolicy::default();
        let counter = r.entry_mut(channel(0xA), policy.decay_half_life, base);
        counter.bump(base);
        let emissions = r.tick(&policy, base);
        assert_eq!(emissions.len(), 1);
        match emissions[0].1 {
            HeatEmission::Emit { rate } => assert!(rate > 0.0),
            other => panic!("expected Emit, got {other:?}"),
        }
        // Subsequent tick suppresses (rate hasn't moved).
        let emissions2 = r.tick(&policy, base);
        assert!(emissions2.is_empty());
    }

    #[test]
    fn tick_emits_withdrawal_after_decay() {
        let base = t0();
        let mut r = HeatRegistry::new();
        let policy = super::super::DataGravityPolicy::default();
        let counter = r.entry_mut(channel(0xA), policy.decay_half_life, base);
        counter.bump(base);
        // First tick — emit.
        let _ = r.tick(&policy, base);
        // 100 half-lives later — rate decays to zero; withdraw.
        let later = base + policy.decay_half_life * 100;
        let emissions = r.tick(&policy, later);
        assert_eq!(emissions.len(), 1);
        assert_eq!(emissions[0].1, HeatEmission::Withdraw);
    }

    #[test]
    fn tick_doubled_rate_re_emits() {
        let base = t0();
        let mut r = HeatRegistry::new();
        let policy = super::super::DataGravityPolicy::default();
        let counter = r.entry_mut(channel(0xA), policy.decay_half_life, base);
        counter.bump(base);
        // First tick — emit at rate ≈ 1.0.
        let first = r.tick(&policy, base);
        assert_eq!(first.len(), 1);
        // More bumps — rate climbs.
        for _ in 0..3 {
            r.entry_mut(channel(0xA), policy.decay_half_life, base)
                .bump(base);
        }
        // Tick — rate is now ≈ 4.0 > 2× last emitted 1.0; emit.
        let second = r.tick(&policy, base);
        assert_eq!(second.len(), 1);
        match second[0].1 {
            HeatEmission::Emit { rate } => assert!(rate >= 4.0 * 0.99),
            other => panic!("expected Emit, got {other:?}"),
        }
    }
}
