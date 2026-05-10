//! Bloom-filter primitive — Phase D of `CAPABILITY_SYSTEM_PLAN.md`.
//!
//! Backs `CapabilitySet::chain_bloom` (next slice). For nodes
//! holding many `causal:`-tagged chains, switching from
//! per-tag-string enumeration to a bloom probe keeps capability
//! announcements compact: 10 K chains in ≤ 500 KB at ≤ 1% false-
//! positive rate per the plan's target sizing.
//!
//! Probe pattern: callers that match the bloom run a follow-up
//! precise lookup (existing `causal:<hex>` tag membership) before
//! issuing real reads. False positives become recoverable misses;
//! false negatives are impossible by construction.
//!
//! ## Implementation
//!
//! - **Bit array** — `Vec<u64>` block storage; `len_bits` is the
//!   logical size (multiple of 64; rounded up at construction).
//! - **Hashing** — `xxhash_rust::xxh3::xxh3_128` once per probe
//!   produces a 128-bit value; the upper 64 bits seed `h1`, the
//!   lower 64 seed `h2`. Per-key `k` indices then come from
//!   double hashing: `bit_i(x) = (h1 + i * h2) mod m`. Avoids
//!   running `k` independent hashes, keeps `xxhash_rust` (already
//!   a dep) as the only crypto-adjacent dependency, and matches
//!   the standard Bloom-filter implementation pattern.
//!
//! ## Sizing
//!
//! Given `expected_items = n` and `false_positive_rate = p`:
//!
//! - Optimal bit count `m = ceil(-n * ln(p) / (ln 2)^2)`.
//! - Optimal hash count `k = round((m / n) * ln 2)`, clamped to
//!   `[1, 32]` so a degenerate input (e.g., `n = 0`) doesn't
//!   produce a zero-`k` filter that admits everything OR a
//!   gigantic `k` that wastes CPU on tiny populations.
//!
//! ## Serde
//!
//! Stored on the wire as `{ "len_bits": <usize>, "k": <u32>, "bits": [u64...] }`.
//! `bits` is a u64 array — each element packs 64 bits of the
//! filter. Deserialization rejects out-of-range `k` and `len_bits`
//! that doesn't match `bits.len() * 64`, so a hand-edited or
//! adversarial wire payload can't construct a malformed filter.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};
use xxhash_rust::xxh3::xxh3_128_with_seed;

/// Domain-separation seed for the chain-tag bloom filter. Splits
/// the xxh3-128 state so callers using xxh3 elsewhere in the
/// codebase can't accidentally collide on the bloom's hash space.
const BLOOM_HASH_SEED: u64 = 0xB100_F1AC_DEAD_CAFE;

/// Hard ceiling on `k`. A filter with more than 32 hashes per
/// insert/query wastes CPU on tiny populations; the optimum for
/// 1% false-positive rate is `k ≈ 7`. Cap defensively.
const MAX_K: u32 = 32;

/// Hard floor on `len_bits`. The filter rounds up to a multiple
/// of 64; the smallest meaningful filter is one u64 (64 bits).
const MIN_LEN_BITS: usize = 64;

/// Probabilistic-membership Bloom filter. Stores no actual keys —
/// only their hash projections — so insertion is destructive in
/// the sense that the original keys can't be enumerated; lookups
/// answer "definitely-not" or "probably-yes". False positives
/// happen at the configured rate; false negatives never happen.
///
/// Construct via [`BloomFilter::new`] (sized from
/// `(expected_items, false_positive_rate)`) or via
/// [`BloomFilter::with_params`] (explicit `len_bits` + `k`).
/// Insert with [`BloomFilter::insert`] (any `&[u8]` key); test
/// with [`BloomFilter::contains`].
#[derive(Clone, PartialEq, Eq)]
pub struct BloomFilter {
    /// Logical bit count (multiple of 64).
    len_bits: usize,
    /// Per-key hash count.
    k: u32,
    /// Bit storage, 64 bits per element.
    bits: Vec<u64>,
}

impl fmt::Debug for BloomFilter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Don't dump the entire bit array — for large filters
        // that's noise. Show the parameters + populated-bit count.
        let popcount: u32 = self.bits.iter().map(|w| w.count_ones()).sum();
        f.debug_struct("BloomFilter")
            .field("len_bits", &self.len_bits)
            .field("k", &self.k)
            .field("popcount", &popcount)
            .field(
                "fill_ratio",
                &(popcount as f64 / self.len_bits.max(1) as f64),
            )
            .finish()
    }
}

/// Round `n` up to the nearest multiple of 64, saturating at
/// `usize::MAX`. Used by [`BloomFilter::new`] /
/// [`BloomFilter::with_params`] to align filter sizes to whole
/// 64-bit words. Saturating arithmetic avoids a debug-build panic
/// (or release-build wrap) when the unrounded value is within
/// 63 of `usize::MAX` — a pathological combination of large
/// `expected_items` and tiny `false_positive_rate` can produce
/// such values via the optimum-`m` formula.
fn round_up_to_64(n: usize) -> usize {
    n.saturating_add(63) & !63
}

impl BloomFilter {
    /// Build a filter sized for `expected_items` insertions at the
    /// target `false_positive_rate` (e.g. `0.01` for 1%).
    ///
    /// `expected_items == 0` is clamped to `1` to avoid a divide-
    /// by-zero in the optimum-`m` formula; the resulting filter is
    /// minimum-sized but still functional. `false_positive_rate`
    /// is clamped to `(1e-9, 0.5)` — values outside that band
    /// produce nonsensical filter sizes (huge or empty).
    pub fn new(expected_items: usize, false_positive_rate: f64) -> Self {
        let n = expected_items.max(1) as f64;
        let p = false_positive_rate.clamp(1e-9, 0.5);
        // Optimal bit count: m = -n * ln(p) / (ln 2)^2
        let ln2 = std::f64::consts::LN_2;
        let m_float = (-n * p.ln() / (ln2 * ln2)).ceil();
        let m_raw = if m_float.is_finite() && m_float > 0.0 {
            m_float as usize
        } else {
            MIN_LEN_BITS
        };
        // Round UP to a multiple of 64 + apply the floor.
        // `m_raw + 63` saturates at `usize::MAX` so a pathological
        // `m_float` (e.g. from a `false_positive_rate` clamped to
        // `1e-9` paired with an enormous `expected_items`) can't
        // panic in debug or wrap in release. Ceiling to the
        // nearest multiple of 64 is a no-op when the value is
        // already aligned.
        let len_bits = round_up_to_64(m_raw).max(MIN_LEN_BITS);
        // Optimal k: k = (m / n) * ln 2
        let k_float = ((len_bits as f64 / n) * ln2).round();
        let k = (k_float as u32).clamp(1, MAX_K);
        Self::with_params(len_bits, k)
    }

    /// Build a filter from explicit parameters. `len_bits` is
    /// rounded up to a multiple of 64; `k` is clamped to
    /// `[1, MAX_K]`. Used internally by [`BloomFilter::new`] and
    /// by deserialization; exposed for callers that want
    /// reproducible filter shapes (cross-binding fixtures, etc.).
    pub fn with_params(len_bits: usize, k: u32) -> Self {
        let len_bits = round_up_to_64(len_bits).max(MIN_LEN_BITS);
        let k = k.clamp(1, MAX_K);
        let words = len_bits / 64;
        Self {
            len_bits,
            k,
            bits: vec![0u64; words],
        }
    }

    /// Insert a key. Idempotent: re-inserting the same key has no
    /// effect on the filter contents.
    pub fn insert(&mut self, key: &[u8]) {
        // Precompute h1/h2/m as locals so the mutable bit-array
        // access inside the loop doesn't conflict with the
        // shared `&self` borrow `bit_indices` would imply.
        let h128 = xxh3_128_with_seed(key, BLOOM_HASH_SEED);
        let h1 = (h128 >> 64) as u64;
        // CR-23: force h2 odd. `len_bits` rounds up to a multiple
        // of 64 (= 2^6 × something), so the modulus `m` shares
        // factor 2 with any even h2; the probe cycle length
        // halves, ≈50% of keys hit fewer distinct bits than `k`
        // claims, and the false-positive rate drifts above the
        // configured target. Coprime-via-`|= 1` is the standard
        // double-hashing remedy.
        let h2 = (h128 as u64) | 1;
        let m = self.len_bits as u64;
        for i in 0..self.k {
            let combined = h1.wrapping_add((i as u64).wrapping_mul(h2));
            let idx = (combined % m) as usize;
            let word = idx / 64;
            let bit = idx % 64;
            self.bits[word] |= 1u64 << bit;
        }
    }

    /// Test membership. Returns `true` if `key` is *probably*
    /// present (or definitely-yes if no false positives have
    /// occurred), `false` if `key` is definitely absent.
    pub fn contains(&self, key: &[u8]) -> bool {
        let h128 = xxh3_128_with_seed(key, BLOOM_HASH_SEED);
        let h1 = (h128 >> 64) as u64;
        // CR-23: must mirror `insert`'s h2-odd coercion or
        // round-trip membership breaks.
        let h2 = (h128 as u64) | 1;
        let m = self.len_bits as u64;
        for i in 0..self.k {
            let combined = h1.wrapping_add((i as u64).wrapping_mul(h2));
            let idx = (combined % m) as usize;
            let word = idx / 64;
            let bit = idx % 64;
            if self.bits[word] & (1u64 << bit) == 0 {
                return false;
            }
        }
        true
    }

    /// Logical bit count. Always a multiple of 64.
    pub fn len_bits(&self) -> usize {
        self.len_bits
    }

    /// Per-key hash count.
    pub fn hash_count(&self) -> u32 {
        self.k
    }

    /// Number of bits set to 1. Useful for fill-ratio diagnostics.
    pub fn popcount(&self) -> u32 {
        self.bits.iter().map(|w| w.count_ones()).sum()
    }

    /// Estimated false-positive rate at the current fill ratio.
    /// Bounds on the true rate: `(popcount / len_bits)^k`.
    /// Approximate but useful for telemetry.
    pub fn estimated_false_positive_rate(&self) -> f64 {
        let fill = self.popcount() as f64 / self.len_bits.max(1) as f64;
        fill.powi(self.k as i32)
    }

    /// Serialized byte size. Used by the
    /// `CapabilityAnnouncementPolicy` threshold logic to decide
    /// when bloom mode beats per-tag enumeration.
    pub fn serialized_bytes(&self) -> usize {
        // 8 bytes for len_bits + 4 for k + (bits.len() * 8) for bits
        // (approximate — the actual JSON / postcard encoding adds
        // framing, but the bit-array is the dominant term).
        8 + 4 + self.bits.len() * 8
    }

    // [`Self::insert`] and [`Self::contains`] each inline the
    // bit-index computation rather than going through a shared
    // helper — the borrow checker rejects an `&self`-returning
    // iterator inside `insert`'s mutable bit-array access (the
    // iterator's borrow overlaps the `bits[word] |=` write).
    // Keeping the formula identical in both methods (double-hash
    // + modular reduction) is the contract; a regression in one
    // body trips the round-trip / membership tests.
}

// ─── Serde wire format ──────────────────────────────────────────

/// On-wire shape: explicit `len_bits` + `k` + bit array.
/// Validation happens in [`Deserialize::deserialize`] so a
/// hand-edited or adversarial wire payload can't construct a
/// malformed filter.
#[derive(Serialize, Deserialize)]
struct BloomFilterWire {
    len_bits: usize,
    k: u32,
    bits: Vec<u64>,
}

impl Serialize for BloomFilter {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        BloomFilterWire {
            len_bits: self.len_bits,
            k: self.k,
            bits: self.bits.clone(),
        }
        .serialize(s)
    }
}

impl<'de> Deserialize<'de> for BloomFilter {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let wire = BloomFilterWire::deserialize(d)?;
        // Reject malformed wire: len_bits MUST equal bits.len() * 64.
        if wire.len_bits != wire.bits.len() * 64 {
            return Err(serde::de::Error::custom(format!(
                "bloom filter wire mismatch: len_bits={}, bits.len()*64={}",
                wire.len_bits,
                wire.bits.len() * 64,
            )));
        }
        // Reject zero-bit filters (would make `bit_indices` divide
        // by zero) and out-of-range k.
        if wire.len_bits < MIN_LEN_BITS {
            return Err(serde::de::Error::custom(format!(
                "bloom filter len_bits={} below minimum {}",
                wire.len_bits, MIN_LEN_BITS,
            )));
        }
        if wire.k < 1 || wire.k > MAX_K {
            return Err(serde::de::Error::custom(format!(
                "bloom filter k={} out of range [1, {}]",
                wire.k, MAX_K,
            )));
        }
        Ok(Self {
            len_bits: wire.len_bits,
            k: wire.k,
            bits: wire.bits,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Inserted keys must always test as `contains`. False
    /// negatives violate the bloom contract.
    #[test]
    fn inserted_keys_always_pass_membership() {
        let mut bf = BloomFilter::new(1000, 0.01);
        for i in 0..1000u32 {
            bf.insert(&i.to_le_bytes());
        }
        for i in 0..1000u32 {
            assert!(bf.contains(&i.to_le_bytes()), "false negative on key {i}");
        }
    }

    /// Empty filter: nothing passes. Pin the trivial-case contract.
    #[test]
    fn empty_filter_rejects_all_keys() {
        let bf = BloomFilter::new(100, 0.01);
        assert!(!bf.contains(b"foo"));
        assert!(!bf.contains(b"bar"));
        assert!(!bf.contains(&[0u8; 32]));
        assert_eq!(bf.popcount(), 0);
    }

    /// False-positive rate stays near the configured target. A
    /// filter sized for 10K @ 1% should have an empirical FP rate
    /// in roughly `[0.005, 0.025]` against a 10K out-of-set probe
    /// corpus — the analytic optimum is 1%, but stochastic
    /// variation widens the band.
    #[test]
    fn empirical_false_positive_rate_near_target() {
        let n = 10_000usize;
        let p = 0.01;
        let mut bf = BloomFilter::new(n, p);
        for i in 0..n {
            bf.insert(format!("key-in-{i}").as_bytes());
        }
        let mut fp = 0;
        for i in 0..n {
            if bf.contains(format!("probe-out-{i}").as_bytes()) {
                fp += 1;
            }
        }
        let observed = fp as f64 / n as f64;
        assert!(
            (0.005..=0.025).contains(&observed),
            "observed FP rate {observed:.4} far from target {p}; \
             popcount={}, fill_ratio={:.3}",
            bf.popcount(),
            bf.popcount() as f64 / bf.len_bits() as f64,
        );
    }

    /// Plan target — 10K chains @ 1% must fit in ≤ 500 KB.
    /// Pin the wire-budget sizing so a future `len_bits` formula
    /// regression trips this.
    #[test]
    fn ten_thousand_at_one_percent_under_500kb() {
        let bf = BloomFilter::new(10_000, 0.01);
        assert!(
            bf.serialized_bytes() <= 500 * 1024,
            "10K @ 1% sizing budget breached: {} bytes (target ≤ 500 KB)",
            bf.serialized_bytes(),
        );
    }

    /// Insert idempotence: re-inserting the same key doesn't move
    /// any bits. Hardens the assumption that contains() is
    /// stable across re-insert cycles.
    #[test]
    fn insert_is_idempotent() {
        let mut bf = BloomFilter::new(100, 0.01);
        bf.insert(b"hello");
        let pop_after_first = bf.popcount();
        bf.insert(b"hello");
        assert_eq!(
            bf.popcount(),
            pop_after_first,
            "re-insert moved bits — should be a no-op",
        );
    }

    /// Sizing degeneracies: zero items + extreme false-positive
    /// rates clamp to sensible defaults rather than producing a
    /// filter with zero hashes (which would admit every key) or
    /// zero bits (which would divide-by-zero).
    #[test]
    fn sizing_degeneracies_clamp_to_safe_defaults() {
        // Zero items: clamp to 1; minimum-sized filter survives.
        let bf0 = BloomFilter::new(0, 0.01);
        assert!(bf0.len_bits() >= MIN_LEN_BITS);
        assert!(bf0.hash_count() >= 1);

        // Tiny FPR: still produces a filter (just a big one).
        let bf_tiny = BloomFilter::new(100, 1e-12);
        assert!(bf_tiny.len_bits() >= MIN_LEN_BITS);
        assert!(bf_tiny.hash_count() <= MAX_K);

        // Crazy-loose FPR: clamped to 0.5; small filter.
        let bf_loose = BloomFilter::new(100, 0.99);
        assert!(bf_loose.len_bits() >= MIN_LEN_BITS);
    }

    /// Regression: the bit-size rounding step used to compute
    /// `(n + 63) & !63`. For values within 63 of `usize::MAX`,
    /// that overflowed — a debug-build panic, a release-build
    /// wrap to a tiny aligned value. Saturating the addition
    /// keeps the rounding well-defined at the upper boundary.
    /// (Constructing the filter with a near-MAX size would still
    /// fail the `Vec::with_capacity` allocation; the regression
    /// here is the rounding helper itself.)
    #[test]
    fn round_up_to_64_saturates_at_usize_max() {
        assert_eq!(round_up_to_64(0), 0);
        assert_eq!(round_up_to_64(1), 64);
        assert_eq!(round_up_to_64(64), 64);
        assert_eq!(round_up_to_64(65), 128);
        // Saturation regime — pre-fix this overflowed.
        assert_eq!(round_up_to_64(usize::MAX), usize::MAX & !63);
        assert_eq!(round_up_to_64(usize::MAX - 62), usize::MAX & !63);
        // The largest representable already-aligned value passes
        // through unchanged.
        assert_eq!(round_up_to_64(usize::MAX & !63), usize::MAX & !63,);
    }

    /// Round-trip via serde — write to JSON, read back, verify
    /// every previously-inserted key still passes.
    #[test]
    fn serde_round_trip_preserves_membership() {
        let mut bf = BloomFilter::new(500, 0.01);
        for i in 0..500u32 {
            bf.insert(&i.to_le_bytes());
        }
        let json = serde_json::to_string(&bf).unwrap();
        let restored: BloomFilter = serde_json::from_str(&json).unwrap();
        assert_eq!(bf, restored);
        for i in 0..500u32 {
            assert!(restored.contains(&i.to_le_bytes()));
        }
    }

    /// Malformed wire (mismatched `len_bits` vs `bits.len() * 64`)
    /// rejects on deserialize.
    #[test]
    fn serde_rejects_mismatched_len_bits() {
        // Construct a wire payload by hand where len_bits doesn't
        // match the bits array. Pin the wire-validation contract.
        let bad = serde_json::json!({
            "len_bits": 128,
            "k": 7,
            "bits": [0u64], // only 64 bits, but len_bits says 128
        });
        let result: Result<BloomFilter, _> = serde_json::from_value(bad);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("len_bits") || err.contains("bits"),
            "error message must reference the mismatch: {err}",
        );
    }

    /// Out-of-range `k` rejects on deserialize. A k=0 filter
    /// would admit every key (the for-loop in `bit_indices` runs
    /// zero iterations and `contains` returns true vacuously),
    /// so refusing it at decode time prevents that vulnerability.
    #[test]
    fn serde_rejects_zero_or_excessive_k() {
        // k=0
        let zero_k = serde_json::json!({
            "len_bits": 64,
            "k": 0,
            "bits": [0u64],
        });
        assert!(serde_json::from_value::<BloomFilter>(zero_k).is_err());

        // k=999
        let huge_k = serde_json::json!({
            "len_bits": 64,
            "k": 999,
            "bits": [0u64],
        });
        assert!(serde_json::from_value::<BloomFilter>(huge_k).is_err());
    }

    /// Debug output contains the parameters but doesn't dump the
    /// raw bit array. Pin the diagnostic shape so a future Debug
    /// regression that prints megabytes of bits trips the test.
    #[test]
    fn debug_output_is_compact() {
        let mut bf = BloomFilter::new(10_000, 0.01);
        bf.insert(b"some-key");
        let s = format!("{bf:?}");
        assert!(s.contains("BloomFilter"));
        assert!(s.contains("len_bits"));
        assert!(s.contains("popcount"));
        // Don't include the raw bit-array in the Debug output —
        // for a 10K@1% filter that's ~100 KB of u64s.
        assert!(s.len() < 200, "debug output too long: {} chars", s.len());
    }
}
