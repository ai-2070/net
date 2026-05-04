//! Compressed observed horizon for causal ordering.
//!
//! Replaces full vector clocks with a fixed-size bloom sketch that fits
//! in `CausalLink::horizon_encoded`. Provides O(1) approximate causality
//! detection without O(N) wire-size scaling.
//!
//! # Approximation envelope (LOAD-BEARING — read before relying on this)
//!
//! - **Approximate.** The wire-encoded horizon is a 64-bit bloom filter
//!   with 3 hash positions per origin. False positives are possible
//!   (and the ONLY error direction the encoding can produce);
//!   false negatives are not. `might_contain` returns `true` if the
//!   origin is in the horizon OR if it bloom-collides with origins
//!   that are.
//! - **Tuned for ≲ 16 active origins.** Past ~16 origins observed
//!   per event the false-positive rate climbs above 50 %, and
//!   `potentially_concurrent` (which AND-negates two `might_contain`
//!   calls) starts collapsing toward constant `false` — i.e. the
//!   sketch claims everything is causally ordered, defeating the
//!   concurrency-detection path. See the FPR table on
//!   `HorizonEncoder` for the full curve.
//! - **Above the ceiling, use the out-of-band full-horizon path.**
//!   Callers that need exact answers at higher origin cardinalities
//!   must NOT rely on `might_contain` / `potentially_concurrent`.
//!   The full [`ObservedHorizon`] is exchanged out-of-band (not
//!   per-event) and queried via [`ObservedHorizon::has_observed`] /
//!   [`CausalCone::from_link_with_horizon`](super::super::continuity::cone::CausalCone::from_link_with_horizon),
//!   which gives an exact answer regardless of cardinality.

use std::collections::HashMap;
use xxhash_rust::xxh3::xxh3_64;

/// Full vector clock — exchanged out-of-band, not per-event.
///
/// Tracks the highest sequence number observed from each entity.
/// Locally-held; cardinality is unbounded by wire-format constraints.
/// Use this (via [`Self::has_observed`]) for exact causality answers
/// when the bloom-filter approximation in [`Self::encode`] isn't
/// precise enough — see the module docs' "Approximation envelope".
#[derive(Debug, Clone, Default)]
pub struct ObservedHorizon {
    /// origin_hash -> highest sequence observed from that entity.
    entries: HashMap<u64, u64>,
    /// Logical time (incremented on each observation).
    logical_time: u64,
}

impl ObservedHorizon {
    /// Create an empty horizon.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an observation of an entity at a given sequence.
    ///
    /// `logical_time` advances via `saturating_add(1)`; raw `+= 1`
    /// would debug-panic on overflow (u64::MAX → wrap) and silently
    /// wrap to 0 in release. Adversarial high-cardinality observe
    /// streams could otherwise panic the receive loop in debug
    /// builds. The `merge` path at line 75 uses the same
    /// convention.
    pub fn observe(&mut self, origin_hash: u64, sequence: u64) {
        let entry = self.entries.entry(origin_hash).or_insert(0);
        if sequence > *entry {
            *entry = sequence;
            self.logical_time = self.logical_time.saturating_add(1);
        }
    }

    /// Get the highest observed sequence for an entity.
    pub fn get(&self, origin_hash: u64) -> Option<u64> {
        self.entries.get(&origin_hash).copied()
    }

    /// Exact "have I observed an event at or past this sequence?"
    /// query. Use this (with the locally-held full horizon) when
    /// `HorizonEncoder::might_contain` would saturate — past
    /// ~16 active origins per event the bloom approximation
    /// becomes unreliable.
    pub fn has_observed(&self, origin_hash: u64, sequence: u64) -> bool {
        self.entries
            .get(&origin_hash)
            .is_some_and(|&seq| seq >= sequence)
    }

    /// Exact "have I observed ANY event from this origin?" query.
    ///
    /// The seq-less variant of [`Self::has_observed`]. Used by
    /// `CausalCone::is_concurrent_with` to bypass the 64-bit bloom
    /// when both sides carry full horizons — the bloom collapses
    /// toward constant-`false` past ~16 distinct origins, silently
    /// mis-reporting genuinely concurrent events as causally
    /// ordered.
    #[inline]
    pub fn contains_origin(&self, origin_hash: u64) -> bool {
        self.entries.contains_key(&origin_hash)
    }

    /// Number of entities in the horizon.
    pub fn entity_count(&self) -> usize {
        self.entries.len()
    }

    /// Logical time of this horizon.
    pub fn logical_time(&self) -> u64 {
        self.logical_time
    }

    /// Iterate over all (origin_hash, sequence) pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&u64, &u64)> {
        self.entries.iter()
    }

    /// Merge another horizon into this one (take max of each entry).
    pub fn merge(&mut self, other: &ObservedHorizon) {
        for (&origin, &seq) in &other.entries {
            let entry = self.entries.entry(origin).or_insert(0);
            if seq > *entry {
                *entry = seq;
            }
        }
        // `saturating_add` so a pathological u64::MAX logical clock
        // on either side doesn't panic in debug or wrap in release.
        // Unreachable in practice but consistent with `observe()`'s
        // counter-hygiene convention.
        self.logical_time = self.logical_time.max(other.logical_time).saturating_add(1);
    }

    /// Encode to an 8-byte compressed horizon for `CausalLink`.
    ///
    /// Returns a 64-bit bloom filter — see [`HorizonEncoder`] for
    /// the FPR-vs-cardinality curve. NOT EXACT past ~16 active
    /// origins; for exact answers, query the full horizon via
    /// [`Self::has_observed`].
    pub fn encode(&self) -> u64 {
        HorizonEncoder::encode(self)
    }
}

/// Encoder/decoder for the 8-byte compressed horizon.
///
/// # Format
///
/// `horizon_encoded` is a 64-bit bloom filter. Each observed
/// `origin_hash` sets 3 bits in the filter, derived from one
/// xxh3 output via Kirsch-Mitzenmacher double-hashing
/// (positions `h1`, `h1 + h2`, `h1 + 2*h2` mod 64).
///
/// Pre-#130 this was a 16-bit bloom packed into the high half of a
/// `u32` with a log-scale max-seq in the low half. The 16-bit bloom
/// saturated after ~6-8 distinct origins, collapsing
/// `potentially_concurrent` into constant-false on any non-tiny
/// mesh. The seq half was unused in production (only test code
/// called `decode_seq`). Post-#130 the full 64 bits are bloom and
/// the seq encoding is removed.
///
/// # False-positive rate (LOAD-BEARING)
///
/// With m = 64 bits, k = 3 hash positions:
///
/// | active origins (n) | FPR of `might_contain` |
/// |---|---|
/// | 2 | ~0.4 % |
/// | 4 | ~3 % |
/// | 8 | ~13 % |
/// | 16 | ~44 % |
/// | 24 | ~70 % |
///
/// Past n ≈ 16, `potentially_concurrent` (which AND-negates two
/// `might_contain` calls) starts collapsing toward constant
/// `false`, defeating concurrency detection. **Callers above
/// this ceiling MUST fall back to the out-of-band full-horizon
/// path** ([`ObservedHorizon::has_observed`] /
/// `CausalCone::from_link_with_horizon`) for exact answers.
pub struct HorizonEncoder;

impl HorizonEncoder {
    /// Compute the three bloom positions for an origin hash.
    ///
    /// Kirsch-Mitzenmacher double-hashing: take xxh3 over the
    /// `origin_hash` bytes, split the 64-bit output into two 32-bit
    /// halves (`h1`, `h2`), and produce positions
    /// `h1`, `h1 + h2`, `h1 + 2*h2` (all mod 64).
    ///
    /// `h2` is OR'd with 1 to force it odd — without that, an `h2`
    /// that happens to be ≡ 0 (mod 64) would collapse all three
    /// positions onto `h1 % 64`, giving a single-bit filter for
    /// that origin. Forcing `h2` odd guarantees the three
    /// positions are distinct (because gcd(odd, 64) = 1, so
    /// `h1`, `h1+h2`, `h1+2*h2` are mutually distinct mod 64).
    ///
    /// Returns a `u64` with exactly 3 bits set (the union of the
    /// three positions).
    #[inline]
    fn bloom_bits_for_origin(origin_hash: u64) -> u64 {
        let h = xxh3_64(&origin_hash.to_le_bytes());
        let h1 = h as u32;
        let h2 = ((h >> 32) as u32) | 1; // force odd — see above
        let p1 = (h1 % 64) as u64;
        let p2 = (h1.wrapping_add(h2) % 64) as u64;
        let p3 = (h1.wrapping_add(h2.wrapping_mul(2)) % 64) as u64;
        (1u64 << p1) | (1u64 << p2) | (1u64 << p3)
    }

    /// Encode a full horizon into 8 bytes (a 64-bit bloom filter).
    pub fn encode(horizon: &ObservedHorizon) -> u64 {
        let mut bloom: u64 = 0;
        for &origin_hash in horizon.entries.keys() {
            bloom |= Self::bloom_bits_for_origin(origin_hash);
        }
        bloom
    }

    /// Check if an origin_hash is possibly in a compressed horizon.
    ///
    /// Returns `false` = definitely not observed,
    /// `true` = possibly observed (false-positive rate climbs with
    /// the number of distinct origins encoded; see the type-level
    /// FPR table). Bloom invariant: every inserted origin reports
    /// `true`.
    pub fn might_contain(encoded: u64, origin_hash: u64) -> bool {
        let bits = Self::bloom_bits_for_origin(origin_hash);
        (encoded & bits) == bits
    }

    /// Check if two events are potentially concurrent (neither
    /// observed the other). Built from two `might_contain` calls.
    ///
    /// Note that `might_contain`'s false positives bias this
    /// function toward `false` ("looks causally ordered"). On a
    /// saturated bloom (>16 distinct origins encoded into either
    /// horizon), this method returns false constantly even for
    /// genuinely concurrent events — see the type-level FPR
    /// discussion. Callers gating conflict resolution / merging on
    /// this method MUST also have an out-of-band escape hatch via
    /// the full [`ObservedHorizon`].
    pub fn potentially_concurrent(
        horizon_a: u64,
        origin_b: u64,
        horizon_b: u64,
        origin_a: u64,
    ) -> bool {
        !Self::might_contain(horizon_a, origin_b) && !Self::might_contain(horizon_b, origin_a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_horizon() {
        let h = ObservedHorizon::new();
        assert_eq!(h.entity_count(), 0);
        assert_eq!(h.encode(), 0);
    }

    #[test]
    fn test_observe() {
        let mut h = ObservedHorizon::new();
        h.observe(0xAAAA, 10);
        h.observe(0xBBBB, 20);

        assert_eq!(h.get(0xAAAA), Some(10));
        assert_eq!(h.get(0xBBBB), Some(20));
        assert_eq!(h.get(0xCCCC), None);
        assert!(h.has_observed(0xAAAA, 5));
        assert!(h.has_observed(0xAAAA, 10));
        assert!(!h.has_observed(0xAAAA, 11));
    }

    /// Regression for BUG_AUDIT_2026_04_30_CORE.md #140:
    /// `observe` previously used `+= 1`, which debug-panics on
    /// overflow at `u64::MAX`. Adversarial high-cardinality
    /// observe streams (or a long-running process that just
    /// happens to hit the wraparound) crashed the receive loop
    /// in debug builds while the same overflow saturated in
    /// release — inconsistent and panic-prone. Post-fix
    /// `observe` uses `saturating_add(1)` matching `merge`.
    #[test]
    fn observe_saturates_logical_time_at_u64_max() {
        let mut h = ObservedHorizon::new();
        // Force logical_time to u64::MAX directly — the public
        // path can't reach it in test time, but the saturating
        // semantics must be tested.
        h.logical_time = u64::MAX;

        // Pre-fix: this would `+= 1` and panic in debug builds
        // (the wrap-to-0 in release was equally a bug).
        h.observe(0xAAAA, 1);

        // Post-fix: saturated, no panic.
        assert_eq!(
            h.logical_time(),
            u64::MAX,
            "saturating_add must clamp at u64::MAX, not wrap to 0"
        );
        // Sanity: the observation was still recorded.
        assert_eq!(h.get(0xAAAA), Some(1));
    }

    #[test]
    fn test_observe_max_only() {
        let mut h = ObservedHorizon::new();
        h.observe(0xAAAA, 10);
        h.observe(0xAAAA, 5); // lower — should not change
        assert_eq!(h.get(0xAAAA), Some(10));

        h.observe(0xAAAA, 15); // higher — should update
        assert_eq!(h.get(0xAAAA), Some(15));
    }

    #[test]
    fn test_merge() {
        let mut h1 = ObservedHorizon::new();
        h1.observe(0xAAAA, 10);
        h1.observe(0xBBBB, 5);

        let mut h2 = ObservedHorizon::new();
        h2.observe(0xAAAA, 8);
        h2.observe(0xCCCC, 20);

        h1.merge(&h2);
        assert_eq!(h1.get(0xAAAA), Some(10)); // max(10, 8)
        assert_eq!(h1.get(0xBBBB), Some(5));
        assert_eq!(h1.get(0xCCCC), Some(20));
    }

    #[test]
    fn test_encode_nonzero() {
        let mut h = ObservedHorizon::new();
        h.observe(0xAAAA, 42);
        let encoded = h.encode();
        assert_ne!(encoded, 0);
    }

    #[test]
    fn test_might_contain() {
        let mut h = ObservedHorizon::new();
        h.observe(0xAAAA, 10);
        h.observe(0xBBBB, 20);
        let encoded = h.encode();

        // Should detect observed origins
        assert!(HorizonEncoder::might_contain(encoded, 0xAAAA));
        assert!(HorizonEncoder::might_contain(encoded, 0xBBBB));

        // Empty horizon should not contain anything
        assert!(!HorizonEncoder::might_contain(0, 0xAAAA));
    }

    #[test]
    fn test_potentially_concurrent() {
        let mut h_a = ObservedHorizon::new();
        h_a.observe(0xAAAA, 10); // A has observed itself
        let enc_a = h_a.encode();

        let mut h_b = ObservedHorizon::new();
        h_b.observe(0xBBBB, 10); // B has observed itself
        let enc_b = h_b.encode();

        // A hasn't observed B and B hasn't observed A — concurrent
        assert!(HorizonEncoder::potentially_concurrent(
            enc_a, 0xBBBB, enc_b, 0xAAAA
        ));

        // Now A observes B
        h_a.observe(0xBBBB, 10);
        let enc_a2 = h_a.encode();

        // A has observed B — no longer concurrent from A's perspective
        assert!(!HorizonEncoder::potentially_concurrent(
            enc_a2, 0xBBBB, enc_b, 0xAAAA
        ));
    }

    // ---- regression coverage ----

    /// Pre-fix the bloom was 16 bits with k=2, saturating
    /// after ~6-8 inserted origins. Post-fix it's 64 bits with k=3,
    /// usable up to ~16 origins at <50 % FPR. Pin the BLOOM-INVARIANT
    /// directly — every inserted origin must report `true`. False
    /// positives are allowed; false negatives are not.
    #[test]
    fn might_contain_has_no_false_negatives_for_inserted_origins() {
        let mut h = ObservedHorizon::new();
        let origins: Vec<u64> = (0..50u64)
            .map(|i| 0xDEAD_0000_u64 ^ i.wrapping_mul(0x9E37_79B1))
            .collect();
        for &o in &origins {
            h.observe(o, 1);
        }
        let encoded = h.encode();
        for &o in &origins {
            assert!(
                HorizonEncoder::might_contain(encoded, o),
                "every inserted origin (here 0x{o:016X}) must report `true` — bloom invariant",
            );
        }
    }

    /// Probabilistic FPR pin at small cardinality. With
    /// m=64, k=3, n=8, expected FPR ≈ 13 %. Test draws 1000 random
    /// non-inserted origins and asserts the empirical FPR stays
    /// below a generous 25 % ceiling — well under the pre-fix
    /// 16-bit bloom's ~57 % at the same n, and conservative enough
    /// to not flake on hash-distribution noise. The point is to
    /// pin "the new bloom is meaningfully better than 16-bit", not
    /// to enforce the exact analytical FPR.
    #[test]
    fn might_contain_fpr_at_n_8_is_well_below_pre_fix_saturation() {
        let mut h = ObservedHorizon::new();
        let inserted: Vec<u64> = (0..8u64)
            .map(|i| 0x1000_0000_u64.wrapping_add(i.wrapping_mul(0xC0FF_EEAB)))
            .collect();
        for &o in &inserted {
            h.observe(o, 1);
        }
        let encoded = h.encode();

        let mut false_positives = 0;
        let trials = 1000;
        for i in 0..trials {
            // Probe origin not in the inserted set.
            let probe = 0xBEEF_0000_u64.wrapping_add((i as u64).wrapping_mul(0x9E37_79B9));
            if inserted.contains(&probe) {
                continue;
            }
            if HorizonEncoder::might_contain(encoded, probe) {
                false_positives += 1;
            }
        }
        let fpr = (false_positives as f64) / (trials as f64);
        assert!(
            fpr < 0.25,
            "FPR at n=8 should be well under the pre-fix 16-bit-bloom \
             saturation (~57 % at n=8); observed {:.1} % over {} trials, \
             expected analytically ~13 %",
            fpr * 100.0,
            trials,
        );
    }

    /// Three set bits per insert (modulo collision when h1 / h2
    /// happen to alias). For a "clean" origin (we choose one whose
    /// xxh3 output gives three distinct bloom positions), the
    /// encoded value must have exactly 3 bits set.
    ///
    /// We test the lower bound: at LEAST 1 bit set per insert (a
    /// tighter "exactly 3" assertion would have to special-case
    /// collision-prone inputs, which doesn't add coverage).
    #[test]
    fn bloom_sets_at_least_one_bit_per_insert() {
        let mut h = ObservedHorizon::new();
        h.observe(0xAAAA_BBBB, 1);
        let encoded = h.encode();
        assert_ne!(encoded, 0, "single insert must set at least one bit");
        let bits = encoded.count_ones();
        assert!(
            (1..=3).contains(&bits),
            "single insert sets between 1 and 3 bits (got {bits})",
        );
    }

    /// Pin the documented practical ceiling at n=16. With m=64, k=3
    /// the analytical FPR is ~44 %; the test asserts the empirical
    /// FPR is below 60 % over 1000 random non-inserted probes, with
    /// enough headroom to absorb hash-distribution noise without
    /// flaking. A regression that broke the bloom encoding (e.g.
    /// reduced `k` to 1, or reverted to the pre-fix 16-bit width)
    /// would push FPR well past 60 % and trip this test.
    ///
    /// Past this ceiling the documented escape hatch is the
    /// out-of-band full-`ObservedHorizon` path.
    #[test]
    fn might_contain_fpr_at_documented_ceiling_stays_under_60_percent() {
        let mut h = ObservedHorizon::new();
        let inserted: Vec<u64> = (0..16u64)
            .map(|i| 0x2000_0000_u64.wrapping_add(i.wrapping_mul(0x9E37_79B9)))
            .collect();
        for &o in &inserted {
            h.observe(o, 1);
        }
        let encoded = h.encode();

        let mut false_positives = 0;
        let trials = 1000;
        for i in 0..trials {
            let probe = 0xFEED_0000_u64.wrapping_add((i as u64).wrapping_mul(0xC0FF_EEAB));
            if inserted.contains(&probe) {
                continue;
            }
            if HorizonEncoder::might_contain(encoded, probe) {
                false_positives += 1;
            }
        }
        let fpr = (false_positives as f64) / (trials as f64);
        assert!(
            fpr < 0.60,
            "FPR at the documented n=16 ceiling should stay under 60 % \
             (analytical ~44 %); observed {:.1} % over {} trials",
            fpr * 100.0,
            trials,
        );
    }

    /// User-facing semantic pin: two nodes that have observed
    /// disjoint sets of peers see each other's events as
    /// `potentially_concurrent`. This is the hot path callers gate
    /// conflict-resolution on; if the bloom over-saturates and
    /// `might_contain` starts returning `true` indiscriminately,
    /// `potentially_concurrent` collapses to constant `false` and
    /// merging never runs — the exact #130 failure mode.
    ///
    /// At n=4 origins each (well under the documented ≤16 ceiling),
    /// disjoint horizons must reliably report concurrent. We sweep a
    /// handful of disjoint origin-pair shapes to ensure the property
    /// isn't an artifact of one fortunate hash.
    #[test]
    fn disjoint_small_horizons_recognize_concurrency() {
        // 32 distinct origin candidates, partitioned into 3 pairs of
        // disjoint 4-element sets so any one fortunate hash doesn't
        // mask a regression. Pair k draws left from
        // `[k*8 .. k*8+4)` and right from `[k*8+4 .. k*8+8)`.
        let candidates: Vec<u64> = (0..32u64)
            .map(|i| 0x3000_0000_u64.wrapping_add(i.wrapping_mul(0xDEAD_BEEF)))
            .collect();

        for split in 0..3 {
            let base = split * 8;
            let left_origins = &candidates[base..base + 4];
            let right_origins = &candidates[base + 4..base + 8];

            let mut left = ObservedHorizon::new();
            for &o in left_origins {
                left.observe(o, 1);
            }
            let mut right = ObservedHorizon::new();
            for &o in right_origins {
                right.observe(o, 1);
            }
            let enc_l = left.encode();
            let enc_r = right.encode();

            // For at least 3 of the 4 right-side origins, left's
            // bloom must NOT contain them (false positives are
            // possible at this cardinality but should be rare).
            let mut left_false_positives = 0;
            let mut right_false_positives = 0;
            for (i, &o) in right_origins.iter().enumerate() {
                if HorizonEncoder::might_contain(enc_l, o) {
                    left_false_positives += 1;
                }
                let l = left_origins[i];
                if HorizonEncoder::might_contain(enc_r, l) {
                    right_false_positives += 1;
                }
            }
            assert!(
                left_false_positives <= 1 && right_false_positives <= 1,
                "split {split}: at n=4 each, expected ≤1 false-positive \
                 cross-origin probe in either direction, got \
                 left={left_false_positives} right={right_false_positives}",
            );

            // And at least ONE pair (origin_a, origin_b) drawn from the
            // disjoint sets must report `potentially_concurrent` — the
            // hot-path semantic.
            let any_concurrent =
                left_origins
                    .iter()
                    .zip(right_origins.iter())
                    .any(|(&origin_a, &origin_b)| {
                        HorizonEncoder::potentially_concurrent(enc_l, origin_b, enc_r, origin_a)
                    });
            assert!(
                any_concurrent,
                "split {split}: at least one cross-origin pair must report \
                 potentially_concurrent for disjoint small horizons",
            );
        }
    }

    /// End-to-end pin: a real `ObservedHorizon` encoded into a
    /// `CausalLink`, serialized through the wire format, and parsed
    /// back must preserve the bloom bits exactly. The wire-size
    /// regression test in `causal.rs` pins the byte count; this
    /// test pins that the bloom semantic survives the round-trip
    /// (catches a future refactor that, e.g., truncates
    /// `horizon_encoded` to u32 in one direction of `to_bytes`).
    #[test]
    fn encoded_horizon_round_trips_through_causal_link_wire_format() {
        use crate::adapter::net::state::causal::CausalLink;

        let mut h = ObservedHorizon::new();
        for i in 0..8u64 {
            h.observe(
                0x4000_0000_u64.wrapping_add(i.wrapping_mul(0xCAFE_F00D)),
                i + 1,
            );
        }
        let encoded = h.encode();
        assert_ne!(encoded, 0);
        // Confirm the bloom has a non-trivial bit pattern (not just
        // saturated and not just empty).
        let bits = encoded.count_ones();
        assert!((1..64).contains(&bits));

        let link = CausalLink::genesis(0xDEAD_BEEF, encoded);
        let bytes = link.to_bytes();
        let parsed = CausalLink::from_bytes(&bytes).unwrap();

        assert_eq!(
            parsed.horizon_encoded, encoded,
            "horizon_encoded must round-trip exactly through CausalLink wire format",
        );

        // And the bloom semantic must survive: every inserted origin
        // still tests positive on the round-tripped value.
        for (&origin, _) in h.iter() {
            assert!(
                HorizonEncoder::might_contain(parsed.horizon_encoded, origin),
                "round-tripped horizon must still recognize inserted origin 0x{origin:016X}",
            );
        }
    }

    /// Defense-in-depth on the Kirsch-Mitzenmacher double-hash
    /// odd-h2 trick. If a future refactor drops the `| 1` mask on
    /// `h2`, an origin whose xxh3 output happens to give h2 ≡ 0
    /// (mod 64) would collapse all three bloom positions onto the
    /// h1 bit, producing a single-bit insert for that origin.
    /// We can't directly inspect the inner hash without exposing
    /// it, but we can pin that across a wide range of inputs no
    /// origin produces a single-bit encoding (with very high
    /// probability, given odd h2).
    #[test]
    fn bloom_does_not_collapse_to_one_bit_for_typical_origins() {
        let mut single_bit_count = 0;
        for i in 0..256u64 {
            let mut h = ObservedHorizon::new();
            h.observe(i, 1);
            let encoded = h.encode();
            if encoded.count_ones() == 1 {
                single_bit_count += 1;
            }
        }
        // With odd h2 and proper double-hashing, no origin should
        // collapse to a single-bit encoding. With even h2 (the
        // pre-fix mistake we're guarding against), ~1/64 origins
        // would. Allow up to 1 collision out of 256 to leave
        // headroom for coincidental p1/p2/p3 aliasing in the modular
        // arithmetic.
        assert!(
            single_bit_count <= 1,
            "{single_bit_count} of 256 origins encoded to a single-bit \
             bloom — Kirsch-Mitzenmacher odd-h2 invariant likely broken",
        );
    }
}
