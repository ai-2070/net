//! Ordering across restarts: incarnation-scoped sequences (plan §4.6).
//!
//! Indirect observers never see the origin's handshake, so the
//! purge-on-rehandshake trick that keeps `WithdrawalSeqGate` honest
//! cannot work here. Instead the ordering key is
//! `(origin, origin_incarnation, interest_digest)` → strictly-newer
//! seq, where the incarnation is a **monotonic persisted boot
//! counter** the origin signs into every attestation. A new
//! incarnation supersedes the old sequence space; a random per-boot
//! value cannot be ordered and would let a replayed old incarnation
//! masquerade as a fresh restart.
//!
//! Two halves, two failure surfaces:
//!
//! - [`next_incarnation`] is the origin-side boot protocol:
//!   load → increment → **persist → only then participate**. Its
//!   failure matrix (store crash, unreadable state, exhaustion) is
//!   what keeps one machine from reusing an incarnation.
//! - [`IncarnationSeqGate`] is the observer-side admission gate. It
//!   contains what persistence cannot promise: filesystem rollback,
//!   restored backups, and cloned identity state are locally
//!   indistinguishable from a legitimate boot, so the *mesh side*
//!   must refuse regressions ([`Admission::StaleIncarnation`]) and
//!   poison proven equivocation ([`Admission::Equivocation`]) so a
//!   duplicated identity degrades to Unknown instead of flapping
//!   Ready. Identity cloning is not a sensing-specific failure —
//!   sensing merely makes it visible; the spike shows it is
//!   contained.
//!
//! The gate map is bounded on the `WithdrawalSeqGate` LRU shape
//! (SI-1): every sighting refreshes the stream's access tick, and on
//! overflow only the least-recently-touched idle tail is evicted —
//! never the whole map, so active pairs keep their ordering (plan
//! §7). Poisoned streams are evicted last; see
//! [`IncarnationSeqGate`] for the exact retention bound.

use std::collections::HashMap;
use std::fmt;

use super::identity::Digest256;

/// A provider boot epoch — the persisted monotonic counter under
/// which an origin signs its attestation sequence.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Incarnation(u64);

impl Incarnation {
    /// Wrap a raw counter value.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// The raw counter value.
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for Incarnation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "inc#{}", self.0)
    }
}

/// A persistence-layer failure (unreadable or unwritable counter
/// state). Deliberately opaque: every fault has the same protocol
/// consequence — the node must not participate under an unordered
/// incarnation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PersistenceFault;

impl fmt::Display for PersistenceFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("incarnation counter persistence fault")
    }
}

impl std::error::Error for PersistenceFault {}

/// Storage for the boot counter, kept beside the identity key
/// material (plan §4.6). `load` distinguishes *absence* (fresh
/// install → `Ok(None)`, counting starts at 1) from *failure*
/// (`Err` → the caller must fail closed: defaulting an unreadable
/// counter to zero would manufacture exactly the rollback this
/// machinery exists to contain).
pub trait IncarnationPersistence {
    /// Read the last persisted counter, `None` on fresh install.
    fn load(&mut self) -> Result<Option<u64>, PersistenceFault>;
    /// Durably store `value`. MUST NOT return `Ok` before the value
    /// would survive a crash.
    fn store(&mut self, value: u64) -> Result<(), PersistenceFault>;
}

/// Why a node could not acquire a boot incarnation. Every variant
/// means the same thing operationally: do not emit attestations.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IncarnationError {
    /// The counter would overflow `u64`. Practically unreachable,
    /// but the defined behavior is refusal — wrapping to 0 would
    /// order the node's next boot *before* its entire history.
    Exhausted,
    /// The counter could not be read or durably written.
    Persistence(PersistenceFault),
}

impl fmt::Display for IncarnationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exhausted => f.write_str("incarnation counter exhausted"),
            Self::Persistence(fault) => fault.fmt(f),
        }
    }
}

impl std::error::Error for IncarnationError {}

/// The origin-side boot protocol: load, increment, **persist, and
/// only then** hand the incarnation out for network participation.
///
/// Ordering matters — the counter is durable before the first signed
/// attestation exists, so a crash at any point produces at worst a
/// persisted-but-never-used value (a harmless gap), never two boots
/// emitting under one incarnation. What this CANNOT defend against
/// is persistence itself going backward (filesystem rollback,
/// restored backup, cloned state): those are locally invisible and
/// are contained on the observer side by [`IncarnationSeqGate`].
pub fn next_incarnation<P: IncarnationPersistence>(
    persistence: &mut P,
) -> Result<Incarnation, IncarnationError> {
    let prev = persistence
        .load()
        .map_err(IncarnationError::Persistence)?
        .unwrap_or(0);
    let next = prev.checked_add(1).ok_or(IncarnationError::Exhausted)?;
    persistence
        .store(next)
        .map_err(IncarnationError::Persistence)?;
    Ok(Incarnation::new(next))
}

/// Admission verdict for one attestation at the ordering gate.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Admission {
    /// Strictly newer in the current (or a superseding) incarnation:
    /// becomes the latest observation for the key.
    Admit,
    /// Same incarnation, seq not newer, same payload — a relay
    /// duplicate or reordered stale beat. Dropped silently.
    StaleSeq,
    /// Older incarnation than the admitted stream — a delayed
    /// pre-restart attestation or a rolled-back origin. Never
    /// admitted: the observer's view must not regress.
    StaleIncarnation,
    /// Same `(incarnation, seq)` as the latest admitted beat but a
    /// DIFFERENT payload: two live emitters under one identity
    /// (cloned state or key compromise). The incarnation is poisoned
    /// — nothing further admits from it, so the key degrades to
    /// Unknown when continuity expires instead of flapping between
    /// the two emitters' states.
    Equivocation,
    /// The incarnation was previously poisoned by an equivocation;
    /// all its beats are refused until a strictly higher incarnation
    /// supersedes it.
    PoisonedIncarnation,
}

impl Admission {
    /// Whether the attestation becomes the key's latest observation.
    pub const fn is_admitted(self) -> bool {
        matches!(self, Self::Admit)
    }
}

#[derive(Clone, Copy)]
struct GateEntry {
    incarnation: Incarnation,
    last_seq: u64,
    last_fingerprint: Digest256,
    /// Highest incarnation proven to have two live emitters; beats
    /// at or below it are refused.
    poisoned_at: Option<Incarnation>,
    /// Access tick of the most recent sighting (admitted or not),
    /// for LRU eviction — a stream still receiving beats is active.
    touch: u64,
}

/// Observer-side strictly-newer admission over
/// `(origin, incarnation, interest_digest)` (plan §4.6).
///
/// The `fingerprint` argument to [`Self::admit`] is a digest of the
/// signed attestation bytes: since the origin signs `(incarnation,
/// seq)` into the payload, two different fingerprints for one
/// `(incarnation, seq)` prove the identity produced two histories.
/// Detection is best-effort at the admission frontier (only the
/// latest beat is retained for comparison) — sufficient for the
/// cloned-emitter case, because independent seq counters started
/// from a cloned snapshot collide at the frontier almost
/// immediately, and every collision poisons the incarnation.
///
/// # Bounds (SI-1, `WithdrawalSeqGate` LRU shape)
///
/// The map is hard-bounded: when a sighting pushes it past
/// `MAX_ENTRIES`, the least-recently-touched streams are evicted
/// down to `LOW_WATER` (plan §7: evict the idle tail, never clear
/// active pairs' ordering). Every sighting — admitted, stale, or
/// poisoned — refreshes the stream's tick, and the incoming stream
/// is sighted BEFORE victims are chosen, so an admit never evicts
/// its own ordering.
///
/// **Poisoned streams are evicted last, not exempted.** A full
/// exemption would let a compromised signing key pin unbounded
/// entries by equivocating across arbitrarily many interest digests
/// — the gate itself would become the memory DoS. Evict-last keeps
/// the hard bound while making the clone-re-admit window (a poisoned
/// stream whose record is dropped would admit the clones afresh)
/// open only under mass equivocation: a poison record can be evicted
/// only when MORE than `LOW_WATER` distinct poisoned streams exist —
/// which requires the origin's signing keys across thousands of
/// streams, i.e. a full identity compromise, not a replay — and even
/// then the `LOW_WATER` most-recently-sighted poison records are
/// retained.
#[derive(Default)]
pub struct IncarnationSeqGate {
    entries: HashMap<(u64, Digest256), GateEntry>,
    /// Monotonic access clock; each [`Self::admit`] stamps the
    /// sighted stream with the next tick so overflow eviction can
    /// drop the least-recently-active streams.
    tick: u64,
}

impl IncarnationSeqGate {
    /// Hard bound that triggers eviction. Matches
    /// `WithdrawalSeqGate::MAX_ENTRIES`: both gates hold one small
    /// ordering record per remote stream, so the same mesh-scale
    /// sizing argument (thousands of peers x a few streams each,
    /// well under the bound in honest operation) budgets both alike.
    const MAX_ENTRIES: usize = 8192;
    /// Post-eviction target: overflow drops the least-recently-
    /// touched streams down to this mark rather than clearing the
    /// whole map. Matches `WithdrawalSeqGate::LOW_WATER`; the
    /// 2048-entry hysteresis gap amortizes the O(n) eviction sweep
    /// over at least that many subsequent inserts.
    const LOW_WATER: usize = 6144;

    /// Empty gate.
    pub fn new() -> Self {
        Self::default()
    }

    /// Run one attestation through the gate. `origin` + `digest`
    /// select the stream; `incarnation`/`seq` order it;
    /// `fingerprint` (digest of the signed bytes) arms equivocation
    /// detection.
    pub fn admit(
        &mut self,
        origin: u64,
        digest: Digest256,
        incarnation: Incarnation,
        seq: u64,
        fingerprint: Digest256,
    ) -> Admission {
        // Sight the incoming stream FIRST — apply its ordering check
        // against any existing entry and stamp it with the newest
        // tick — and only THEN bound the map. Evicting first could
        // drop this very stream's history right before the insert,
        // so even a stale beat would be reinserted as new and
        // wrongly admitted. By sighting first, the incoming stream
        // is the most-recently-touched and is never evicted by its
        // own admit (the `WithdrawalSeqGate` ordering).
        let verdict = self.sight(origin, digest, incarnation, seq, fingerprint);
        self.evict_if_over_capacity();
        verdict
    }

    /// The ordering check itself: refresh/insert the stream's entry
    /// and stamp its access tick. Every sighting refreshes the tick
    /// — even stale or poisoned beats mean the stream is active.
    fn sight(
        &mut self,
        origin: u64,
        digest: Digest256,
        incarnation: Incarnation,
        seq: u64,
        fingerprint: Digest256,
    ) -> Admission {
        let touch = self.tick;
        self.tick += 1;
        let entry = match self.entries.get_mut(&(origin, digest)) {
            None => {
                self.entries.insert(
                    (origin, digest),
                    GateEntry {
                        incarnation,
                        last_seq: seq,
                        last_fingerprint: fingerprint,
                        poisoned_at: None,
                        touch,
                    },
                );
                return Admission::Admit;
            }
            Some(entry) => entry,
        };
        entry.touch = touch;

        if let Some(poisoned) = entry.poisoned_at {
            if incarnation <= poisoned {
                return Admission::PoisonedIncarnation;
            }
        }

        match incarnation.cmp(&entry.incarnation) {
            std::cmp::Ordering::Greater => {
                // A (signed) higher incarnation supersedes the old
                // sequence space entirely — including a poisoned one:
                // a genuine restart is the recovery path out of a
                // resolved clone.
                entry.incarnation = incarnation;
                entry.last_seq = seq;
                entry.last_fingerprint = fingerprint;
                Admission::Admit
            }
            std::cmp::Ordering::Less => Admission::StaleIncarnation,
            std::cmp::Ordering::Equal => {
                if seq > entry.last_seq {
                    entry.last_seq = seq;
                    entry.last_fingerprint = fingerprint;
                    Admission::Admit
                } else if seq == entry.last_seq && fingerprint != entry.last_fingerprint {
                    entry.poisoned_at = Some(incarnation);
                    Admission::Equivocation
                } else {
                    Admission::StaleSeq
                }
            }
        }
    }

    /// Drop only the least-recently-touched streams down to
    /// [`Self::LOW_WATER`] when the map exceeds
    /// [`Self::MAX_ENTRIES`] — plan §7: evict the idle tail, never
    /// clear active pairs' ordering. O(n), but only on the rare
    /// overflow, and the hysteresis gap spreads sweeps apart.
    ///
    /// Victims are ranked clean-before-poisoned, oldest touch first
    /// within each class: a poisoned record whose ordering was
    /// evicted would let the clones re-admit afresh, so poison
    /// records outlive every clean record under pressure (see the
    /// type-level docs for why evict-last rather than a full
    /// exemption, and the exact `> LOW_WATER` bound).
    fn evict_if_over_capacity(&mut self) {
        if self.entries.len() <= Self::MAX_ENTRIES {
            return;
        }
        let excess = self.entries.len() - Self::LOW_WATER;
        // Ticks are unique, so `(poisoned, touch)` totally orders
        // the entries; the `excess` smallest are the victims.
        let mut victims: Vec<(bool, u64, (u64, Digest256))> = self
            .entries
            .iter()
            .map(|(key, entry)| (entry.poisoned_at.is_some(), entry.touch, *key))
            .collect();
        victims.select_nth_unstable_by_key(excess - 1, |&(poisoned, touch, _)| (poisoned, touch));
        for &(_, _, key) in &victims[..excess] {
            self.entries.remove(&key);
        }
    }

    /// The incarnation this stream is poisoned at, if any — the
    /// observation layer maps this to an expired-continuity Unknown.
    pub fn poisoned(&self, origin: u64, digest: Digest256) -> Option<Incarnation> {
        self.entries
            .get(&(origin, digest))
            .and_then(|entry| entry.poisoned_at)
    }

    /// Number of tracked (origin, digest) streams.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the gate tracks no streams yet.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test persistence double with scriptable faults — the
    /// "filesystem" side of the §4.6 failure matrix.
    #[derive(Default)]
    struct FakeDisk {
        value: Option<u64>,
        fail_load: bool,
        fail_store: bool,
        stores: Vec<u64>,
    }

    impl IncarnationPersistence for FakeDisk {
        fn load(&mut self) -> Result<Option<u64>, PersistenceFault> {
            if self.fail_load {
                return Err(PersistenceFault);
            }
            Ok(self.value)
        }
        fn store(&mut self, value: u64) -> Result<(), PersistenceFault> {
            if self.fail_store {
                return Err(PersistenceFault);
            }
            self.value = Some(value);
            self.stores.push(value);
            Ok(())
        }
    }

    fn fp(byte: u8) -> Digest256 {
        Digest256::from_bytes([byte; 32])
    }

    const DIGEST: [u8; 32] = [7u8; 32];

    fn digest() -> Digest256 {
        Digest256::from_bytes(DIGEST)
    }

    #[test]
    fn boots_are_monotonic_and_persisted_before_use() {
        let mut disk = FakeDisk::default();
        for expected in 1..=3u64 {
            let inc = next_incarnation(&mut disk).unwrap();
            assert_eq!(inc, Incarnation::new(expected));
            // Persist-then-participate: by the time the incarnation
            // is handed out, the counter on disk already covers it.
            assert_eq!(disk.value, Some(expected));
        }
        assert_eq!(disk.stores, vec![1, 2, 3]);
    }

    #[test]
    fn store_failure_blocks_participation_without_burning_the_value() {
        // Crash window between increment and persist: the store
        // fails, so NO incarnation is handed out — nothing was ever
        // emitted under the unpersisted candidate, so reusing it on
        // the next successful boot is safe.
        let mut disk = FakeDisk {
            value: Some(4),
            fail_store: true,
            ..FakeDisk::default()
        };
        assert_eq!(
            next_incarnation(&mut disk),
            Err(IncarnationError::Persistence(PersistenceFault)),
        );
        assert_eq!(disk.value, Some(4), "failed store must not advance state");
        disk.fail_store = false;
        assert_eq!(next_incarnation(&mut disk), Ok(Incarnation::new(5)));
    }

    #[test]
    fn unreadable_counter_fails_closed() {
        // An unreadable counter must NOT default to zero — that
        // would be a self-inflicted rollback.
        let mut disk = FakeDisk {
            value: Some(9),
            fail_load: true,
            ..FakeDisk::default()
        };
        assert_eq!(
            next_incarnation(&mut disk),
            Err(IncarnationError::Persistence(PersistenceFault)),
        );
    }

    #[test]
    fn fresh_install_starts_at_one() {
        let mut disk = FakeDisk::default();
        assert_eq!(next_incarnation(&mut disk), Ok(Incarnation::new(1)));
    }

    #[test]
    fn counter_exhaustion_refuses_participation() {
        let mut disk = FakeDisk {
            value: Some(u64::MAX),
            ..FakeDisk::default()
        };
        assert_eq!(
            next_incarnation(&mut disk),
            Err(IncarnationError::Exhausted)
        );
        assert_eq!(disk.value, Some(u64::MAX), "exhaustion must not wrap");
    }

    #[test]
    fn strictly_newer_seq_admits_and_stale_drops() {
        let mut gate = IncarnationSeqGate::new();
        let inc = Incarnation::new(3);
        assert!(gate.admit(1, digest(), inc, 5, fp(1)).is_admitted());
        assert!(gate.admit(1, digest(), inc, 6, fp(2)).is_admitted());
        assert_eq!(gate.admit(1, digest(), inc, 6, fp(2)), Admission::StaleSeq);
        assert_eq!(gate.admit(1, digest(), inc, 4, fp(3)), Admission::StaleSeq);
    }

    #[test]
    fn new_incarnation_supersedes_old_sequence_space() {
        // SI-0 test 8 (observer half): restart resets the seq space;
        // the delayed old-incarnation beat — even with a huge seq —
        // is never admitted over the new stream.
        let mut gate = IncarnationSeqGate::new();
        assert!(gate
            .admit(1, digest(), Incarnation::new(7), 100, fp(1))
            .is_admitted());
        assert!(gate
            .admit(1, digest(), Incarnation::new(8), 1, fp(2))
            .is_admitted());
        assert_eq!(
            gate.admit(1, digest(), Incarnation::new(7), 101, fp(3)),
            Admission::StaleIncarnation,
        );
    }

    #[test]
    fn rollback_is_contained_by_the_observer_gate() {
        // Filesystem rollback / restored backup: the origin's disk
        // went back to 2 and it legitimately boots inc 3 — but the
        // observer has already admitted inc 5. Nothing the
        // rolled-back node emits is admitted (no regression, no
        // flap) until its counter climbs past the admitted stream.
        let mut gate = IncarnationSeqGate::new();
        assert!(gate
            .admit(1, digest(), Incarnation::new(5), 9, fp(1))
            .is_admitted());
        for rolled_back in 3..=5u64 {
            assert_eq!(
                gate.admit(1, digest(), Incarnation::new(rolled_back), 1, fp(2)),
                if rolled_back < 5 {
                    Admission::StaleIncarnation
                } else {
                    Admission::StaleSeq
                },
            );
        }
        assert!(gate
            .admit(1, digest(), Incarnation::new(6), 1, fp(2))
            .is_admitted());
    }

    #[test]
    fn cloned_identity_poisons_the_incarnation_instead_of_flapping() {
        // Two live nodes from one cloned snapshot: both boot inc 7,
        // both count seqs from 1, emitting different statuses. The
        // first frontier collision (same inc, same seq, different
        // signed bytes) proves equivocation; from then on NOTHING in
        // inc 7 admits — the key quietly expires to Unknown rather
        // than flapping between the clones' states.
        let mut gate = IncarnationSeqGate::new();
        let inc = Incarnation::new(7);
        assert!(gate.admit(1, digest(), inc, 1, fp(0xA1)).is_admitted());
        assert!(gate.admit(1, digest(), inc, 2, fp(0xA2)).is_admitted());
        // Clone B's seq 2 carries different payload bytes.
        assert_eq!(
            gate.admit(1, digest(), inc, 2, fp(0xB2)),
            Admission::Equivocation,
        );
        assert_eq!(gate.poisoned(1, digest()), Some(inc));
        // Both clones keep emitting under inc 7 — all refused, in
        // BOTH directions (no last-writer-wins flap).
        assert_eq!(
            gate.admit(1, digest(), inc, 3, fp(0xA3)),
            Admission::PoisonedIncarnation,
        );
        assert_eq!(
            gate.admit(1, digest(), inc, 3, fp(0xB3)),
            Admission::PoisonedIncarnation,
        );
        // A genuine restart (higher signed incarnation) is the
        // recovery path once the operator resolves the clone. The
        // poison record stays pinned at inc 7 — it never blocks the
        // superseding stream, only the equivocating epoch.
        assert!(gate
            .admit(1, digest(), Incarnation::new(8), 1, fp(0xC1))
            .is_admitted());
        assert_eq!(gate.poisoned(1, digest()), Some(inc));
    }

    #[test]
    fn identical_duplicate_is_a_relay_dup_not_equivocation() {
        // Same (inc, seq, fingerprint) — a relay re-delivering the
        // same signed bytes. Must stay StaleSeq; poisoning here
        // would let ordinary at-least-once delivery kill streams.
        let mut gate = IncarnationSeqGate::new();
        let inc = Incarnation::new(2);
        assert!(gate.admit(1, digest(), inc, 4, fp(9)).is_admitted());
        assert_eq!(gate.admit(1, digest(), inc, 4, fp(9)), Admission::StaleSeq);
        assert_eq!(gate.poisoned(1, digest()), None);
    }

    #[test]
    fn streams_are_independent_per_origin_and_digest() {
        let mut gate = IncarnationSeqGate::new();
        let other_digest = Digest256::from_bytes([8u8; 32]);
        assert!(gate
            .admit(1, digest(), Incarnation::new(5), 10, fp(1))
            .is_admitted());
        // Different origin, same digest: fresh stream.
        assert!(gate
            .admit(2, digest(), Incarnation::new(1), 1, fp(2))
            .is_admitted());
        // Same origin, different interest digest: fresh stream.
        assert!(gate
            .admit(1, other_digest, Incarnation::new(1), 1, fp(3))
            .is_admitted());
        assert_eq!(gate.len(), 3);
    }

    #[test]
    fn eviction_drops_oldest_touched_idle_streams_to_low_water() {
        let mut gate = IncarnationSeqGate::new();
        let inc = Incarnation::new(1);
        // Fill exactly to the bound; origin 0 is sighted first, so
        // it is the least-recently-touched entry.
        for origin in 0..(IncarnationSeqGate::MAX_ENTRIES as u64) {
            assert!(gate.admit(origin, digest(), inc, 1, fp(1)).is_admitted());
        }
        assert_eq!(gate.len(), IncarnationSeqGate::MAX_ENTRIES);
        // One more stream tips the bound: the oldest-touched idle
        // tail is evicted down to LOW_WATER — never the whole map.
        let tipping = IncarnationSeqGate::MAX_ENTRIES as u64;
        assert!(gate.admit(tipping, digest(), inc, 1, fp(1)).is_admitted());
        assert_eq!(gate.len(), IncarnationSeqGate::LOW_WATER);
        // The tipping stream (newest touch) survived its own
        // overflow: its ordering still gates a duplicate.
        assert_eq!(
            gate.admit(tipping, digest(), inc, 1, fp(1)),
            Admission::StaleSeq,
        );
        // A recently-touched filler survived too.
        assert_eq!(
            gate.admit(tipping - 1, digest(), inc, 1, fp(1)),
            Admission::StaleSeq,
        );
        // The oldest-touched stream was evicted: its ordering is
        // forgotten, so the same beat re-admits as a first sighting.
        assert!(gate.admit(0, digest(), inc, 1, fp(1)).is_admitted());
    }

    #[test]
    fn active_stream_ordering_survives_one_shot_churn() {
        let mut gate = IncarnationSeqGate::new();
        let inc = Incarnation::new(3);
        // Hot stream outside the churn's origin range.
        let hot = u64::MAX;
        assert!(gate.admit(hot, digest(), inc, 100, fp(1)).is_admitted());
        // Heavy churn: 3x MAX_ENTRIES one-shot streams — several
        // full eviction sweeps — while the hot stream keeps beating
        // (every sighting refreshes its tick, so it never becomes
        // part of the idle tail).
        let mut hot_seq = 100;
        for origin in 0..(3 * IncarnationSeqGate::MAX_ENTRIES as u64) {
            assert!(gate.admit(origin, digest(), inc, 1, fp(2)).is_admitted());
            if origin % 1024 == 0 {
                hot_seq += 1;
                assert!(gate.admit(hot, digest(), inc, hot_seq, fp(3)).is_admitted());
            }
        }
        // The hot stream's ordering survived every sweep: a delayed
        // stale beat is still gated — an evicted entry would have
        // wrongly re-admitted it as a first sighting.
        assert_eq!(
            gate.admit(hot, digest(), inc, 50, fp(4)),
            Admission::StaleSeq,
        );
        assert!(gate
            .admit(hot, digest(), inc, hot_seq + 1, fp(5))
            .is_admitted());
    }

    #[test]
    fn poisoned_stream_survives_eviction_pressure_and_still_refuses() {
        let mut gate = IncarnationSeqGate::new();
        let inc = Incarnation::new(7);
        let cloned = u64::MAX;
        // Two clones collide at the frontier: poisoned.
        assert!(gate.admit(cloned, digest(), inc, 1, fp(0xA1)).is_admitted());
        assert_eq!(
            gate.admit(cloned, digest(), inc, 1, fp(0xB1)),
            Admission::Equivocation,
        );
        assert_eq!(gate.poisoned(cloned, digest()), Some(inc));
        // Heavy one-shot churn with the poisoned stream never
        // re-sighted: under plain LRU it would be the very first
        // eviction victim.
        for origin in 0..(3 * IncarnationSeqGate::MAX_ENTRIES as u64) {
            assert!(gate
                .admit(origin, digest(), Incarnation::new(1), 1, fp(2))
                .is_admitted());
        }
        // Evict-last: with fewer than LOW_WATER poisoned streams the
        // poison record cannot be chosen as a victim, so the clones
        // are still refused after every sweep...
        assert_eq!(gate.poisoned(cloned, digest()), Some(inc));
        assert_eq!(
            gate.admit(cloned, digest(), inc, 999, fp(0xA2)),
            Admission::PoisonedIncarnation,
        );
        // ...and a genuine restart is still the recovery path.
        assert!(gate
            .admit(cloned, digest(), Incarnation::new(8), 1, fp(0xC1))
            .is_admitted());
    }
}
