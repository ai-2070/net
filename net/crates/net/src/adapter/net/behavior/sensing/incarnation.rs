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
//! In-process spike: the gate map is unbounded here; SI-1 rehosts it
//! on the `WithdrawalSeqGate` LRU shape (evict the idle tail, never
//! clear an active pair's ordering — plan §7).

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
#[derive(Default)]
pub struct IncarnationSeqGate {
    entries: HashMap<(u64, Digest256), GateEntry>,
}

impl IncarnationSeqGate {
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
        let entry = match self.entries.get_mut(&(origin, digest)) {
            None => {
                self.entries.insert(
                    (origin, digest),
                    GateEntry {
                        incarnation,
                        last_seq: seq,
                        last_fingerprint: fingerprint,
                        poisoned_at: None,
                    },
                );
                return Admission::Admit;
            }
            Some(entry) => entry,
        };

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
}
