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
        if !self.counters.contains_key(&channel) && self.cap > 0 && self.counters.len() >= self.cap
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

/// Cluster-wide blob heat registry. Mirrors [`HeatRegistry`] but
/// keyed by `[u8; 32]` (the chunk's BLAKE3 hash) rather than the
/// chain's `u64` `origin_hash` — operators reading a `BlobRef`
/// have the hash in hand and a `u64` projection would unnecessarily
/// collapse it. Same LRU + cap discipline; same per-counter
/// half-life decay; same tick semantics. PR-5j-a foundation for
/// the gravity migration controller.
#[derive(Debug)]
pub struct BlobHeatRegistry {
    counters: HashMap<[u8; 32], HeatCounter>,
    cap: usize,
}

impl Default for BlobHeatRegistry {
    fn default() -> Self {
        Self {
            counters: HashMap::new(),
            cap: DEFAULT_HEAT_REGISTRY_CAP,
        }
    }
}

impl BlobHeatRegistry {
    /// Empty registry with the default cap.
    pub fn new() -> Self {
        Self::default()
    }

    /// Empty registry with an explicit cap. `cap == 0` disables
    /// the bound — only safe when an external loop guarantees
    /// bounded entries (e.g. an adapter that prunes on chunk
    /// delete).
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

    /// Number of tracked blob hashes.
    pub fn len(&self) -> usize {
        self.counters.len()
    }

    /// True iff zero hashes tracked.
    pub fn is_empty(&self) -> bool {
        self.counters.is_empty()
    }

    /// Get-or-create the counter for `hash`. Returns a mutable
    /// reference so the caller can `bump` / `decay_to` /
    /// `record_emission` in one borrow.
    ///
    /// When `len() == cap` and the inserted key is new, the entry
    /// with the oldest `last_update` is evicted first (LRU-style
    /// replacement). `cap == 0` disables the bound.
    pub fn entry_mut(
        &mut self,
        hash: [u8; 32],
        half_life: Duration,
        now: Instant,
    ) -> &mut HeatCounter {
        if !self.counters.contains_key(&hash) && self.cap > 0 && self.counters.len() >= self.cap {
            self.evict_lru();
        }
        self.counters
            .entry(hash)
            .or_insert_with(|| HeatCounter::new(half_life, now))
    }

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

    /// Read-only access to the counter for `hash`.
    pub fn get(&self, hash: &[u8; 32]) -> Option<&HeatCounter> {
        self.counters.get(hash)
    }

    /// Remove the counter for `hash`. Used on chunk delete /
    /// GC sweep.
    pub fn remove(&mut self, hash: &[u8; 32]) {
        self.counters.remove(hash);
    }

    /// Iterate `(hash, counter)` pairs. Read-only.
    pub fn iter(&self) -> impl Iterator<Item = (&[u8; 32], &HeatCounter)> {
        self.counters.iter()
    }

    /// Walk every tracked hash, applying decay through `now` and
    /// asking `should_emit_heat` whether to emit. Returns the
    /// list of `(hash, decision)` pairs the runtime acts on.
    /// Mirrors [`HeatRegistry::tick`]; records emissions against
    /// the counter so the next tick sees the updated snapshot,
    /// and prunes fully-decayed + already-withdrawn entries.
    pub fn tick(
        &mut self,
        policy: &super::DataGravityPolicy,
        now: Instant,
    ) -> Vec<([u8; 32], HeatEmission)> {
        let mut out = Vec::new();
        for (hash, counter) in self.counters.iter_mut() {
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
                out.push((*hash, emission));
            }
        }
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

    // --- BlobHeatRegistry coverage (PR-5j-a) ---

    fn hash(seed: u8) -> [u8; 32] {
        let mut h = [0u8; 32];
        h[0] = seed;
        h
    }

    #[test]
    fn blob_heat_registry_is_empty_by_default() {
        let r = BlobHeatRegistry::new();
        assert!(r.is_empty());
        assert_eq!(r.cap(), DEFAULT_HEAT_REGISTRY_CAP);
    }

    #[test]
    fn blob_heat_entry_mut_creates_then_bumps() {
        let mut r = BlobHeatRegistry::new();
        let half = Duration::from_secs(60);
        let h = hash(0x01);
        r.entry_mut(h, half, t0()).bump(t0());
        let counter = r.get(&h).expect("entry should exist");
        assert!(counter.rate() > 0.0);
    }

    #[test]
    fn blob_heat_entry_mut_at_cap_evicts_lru() {
        let base = t0();
        let mut r = BlobHeatRegistry::with_cap(2);
        let half = Duration::from_secs(60);
        r.entry_mut(hash(0x01), half, base).bump(base);
        r.entry_mut(hash(0x02), half, base + Duration::from_millis(10))
            .bump(base + Duration::from_millis(10));
        r.entry_mut(hash(0x03), half, base + Duration::from_millis(20))
            .bump(base + Duration::from_millis(20));
        // hash(0x01) was LRU when 0x03 inserted past cap → evicted.
        assert!(r.get(&hash(0x01)).is_none());
        assert!(r.get(&hash(0x02)).is_some());
        assert!(r.get(&hash(0x03)).is_some());
    }

    #[test]
    fn blob_heat_tick_emits_above_threshold() {
        let mut r = BlobHeatRegistry::new();
        let policy = super::super::policy::DataGravityPolicy::default();
        let half = policy.decay_half_life;
        let h = hash(0x42);
        let now = t0();
        // Several bumps in quick succession build rate quickly.
        for _ in 0..8 {
            r.entry_mut(h, half, now).bump(now);
        }
        let emissions = r.tick(&policy, now);
        assert!(
            emissions
                .iter()
                .any(|(k, e)| *k == h && matches!(e, HeatEmission::Emit { rate } if *rate > 0.0)),
            "tick must emit for a heated hash; got {emissions:?}"
        );
    }

    #[test]
    fn blob_heat_remove_drops_entry() {
        let mut r = BlobHeatRegistry::new();
        let half = Duration::from_secs(60);
        let h = hash(0x42);
        r.entry_mut(h, half, t0()).bump(t0());
        r.remove(&h);
        assert!(r.is_empty());
    }

    // ========================================================================
    // Concurrency stress (multi-thread bump / tick races)
    //
    // The registries are `HashMap` inside, designed to live under
    // an outer `Arc<Mutex<...>>` (the production wiring on
    // `MeshBlobAdapter`). These tests wrap the registry that way
    // and assert the higher-level invariants — no panic under
    // concurrent bumps from N threads, tick-during-bump remains
    // safe, LRU eviction stays within the cap envelope.
    // ========================================================================

    /// N threads each bump the same chunk hash on a shared
    /// `Arc<Mutex<BlobHeatRegistry>>`. After the race, the rate
    /// must equal `threads × per_thread` (modulo a negligible
    /// decay over the test's millisecond window). Pins the
    /// outer-mutex serialization correctness for concurrent
    /// fetch-path heat updates.
    #[test]
    fn blob_heat_concurrent_bump_accumulates_under_outer_mutex() {
        use std::sync::{Arc, Barrier, Mutex};
        use std::thread;

        let registry = Arc::new(Mutex::new(BlobHeatRegistry::new()));
        let half = Duration::from_secs(60 * 60); // 1 h — negligible decay over the test
        let target = hash(0xAB);
        let threads = 8usize;
        let per_thread = 1_000usize;
        let start = Arc::new(Barrier::new(threads));
        let mut handles = Vec::with_capacity(threads);

        for _ in 0..threads {
            let registry = registry.clone();
            let start = start.clone();
            handles.push(thread::spawn(move || {
                start.wait();
                for _ in 0..per_thread {
                    let now = Instant::now();
                    let mut guard = registry.lock().unwrap();
                    guard.entry_mut(target, half, now).bump(now);
                }
            }));
        }
        for h in handles {
            h.join().expect("worker panicked");
        }

        let guard = registry.lock().unwrap();
        let counter = guard.get(&target).expect("entry must exist");
        let expected = (threads * per_thread) as f64;
        // Decay is negligible (1 h half-life over ms-window), but
        // we allow a generous 1 % slop for the f64 math without
        // making the test brittle.
        let rate = counter.rate();
        assert!(
            rate > expected * 0.99,
            "expected rate ≈ {} (8 × 1000 bumps); got {} (lower bound failed)",
            expected,
            rate,
        );
        assert!(
            rate <= expected,
            "expected rate ≤ {} (no double-counting); got {}",
            expected,
            rate,
        );
    }

    /// Background `bump` storm on a single hash while a foreground
    /// thread runs `tick(policy, now)` repeatedly. Asserts no panic
    /// and the tick produces at least one emission per cycle
    /// (the rate is well above the threshold ratio after the first
    /// few bumps land). Pins the iter-mut-during-bump safety.
    #[test]
    fn blob_heat_tick_concurrent_with_bumps_is_panic_free() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Barrier, Mutex};
        use std::thread;

        let registry = Arc::new(Mutex::new(BlobHeatRegistry::new()));
        let policy = super::super::policy::DataGravityPolicy::default();
        let half = policy.decay_half_life;
        let target = hash(0xCD);
        let stop = Arc::new(AtomicBool::new(false));
        let start = Arc::new(Barrier::new(2));

        // Bump storm.
        let bumper = {
            let registry = registry.clone();
            let stop = stop.clone();
            let start = start.clone();
            thread::spawn(move || {
                start.wait();
                while !stop.load(Ordering::Relaxed) {
                    let now = Instant::now();
                    let mut guard = registry.lock().unwrap();
                    guard.entry_mut(target, half, now).bump(now);
                }
            })
        };

        // Tick loop.
        let ticker = {
            let registry = registry.clone();
            let stop = stop.clone();
            let start = start.clone();
            thread::spawn(move || {
                start.wait();
                let mut total = 0usize;
                for _ in 0..200 {
                    let now = Instant::now();
                    let emissions = {
                        let mut guard = registry.lock().unwrap();
                        guard.tick(&policy, now)
                    };
                    total += emissions.len();
                }
                stop.store(true, Ordering::Relaxed);
                total
            })
        };

        bumper.join().expect("bumper panicked");
        let total_emissions = ticker.join().expect("ticker panicked");
        // The bumper guarantees rate > 0 for most of the ticker's
        // window, so at least *some* emissions land. The exact
        // count is non-deterministic under the race.
        assert!(
            total_emissions > 0,
            "tick must surface at least one emission while bumps run"
        );
    }

    /// LRU eviction under a tight cap with concurrent inserts from
    /// N threads. Asserts len() stays within the cap × shards
    /// envelope (the `entry_mut` len-check + `evict_lru` aren't
    /// transactional under DashMap-less HashMap+Mutex, but the
    /// outer mutex serializes the whole `entry_mut` call so the
    /// overshoot is zero).
    #[test]
    fn blob_heat_lru_cap_holds_under_concurrent_inserts() {
        use std::sync::{Arc, Barrier, Mutex};
        use std::thread;

        let cap = 16usize;
        let registry = Arc::new(Mutex::new(BlobHeatRegistry::with_cap(cap)));
        let half = Duration::from_secs(60);
        let threads = 4usize;
        // Each thread inserts a distinct range of keys past the
        // cap, so eviction must fire repeatedly.
        let inserts_per_thread = 64u8;
        let start = Arc::new(Barrier::new(threads));
        let mut handles = Vec::with_capacity(threads);

        for tid in 0..threads as u8 {
            let registry = registry.clone();
            let start = start.clone();
            handles.push(thread::spawn(move || {
                start.wait();
                for i in 0..inserts_per_thread {
                    let k = hash(tid * inserts_per_thread + i);
                    let now = Instant::now();
                    let mut guard = registry.lock().unwrap();
                    guard.entry_mut(k, half, now);
                }
            }));
        }
        for h in handles {
            h.join().expect("worker panicked");
        }

        let guard = registry.lock().unwrap();
        // The cap is enforced under the outer mutex on the
        // entry_mut hot path — len() must never exceed it.
        assert!(
            guard.len() <= cap,
            "len() {} exceeded cap {} after concurrent inserts; LRU eviction is broken",
            guard.len(),
            cap,
        );
    }
}
