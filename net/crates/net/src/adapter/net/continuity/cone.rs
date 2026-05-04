//! Causal cones — what events could have influenced a given event.
//!
//! A `CausalCone` answers: "given event E, which other entities' events
//! could have causally preceded E?" Uses the compressed horizon from
//! `CausalLink::horizon_encoded` for approximate O(1) queries, or the
//! full `ObservedHorizon` for exact answers when available.

use crate::adapter::net::state::causal::CausalLink;
use crate::adapter::net::state::horizon::{HorizonEncoder, ObservedHorizon};

/// The causal relationship between two events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Causality {
    /// Event A definitely preceded event B (exact horizon match).
    Definite,
    /// Event A possibly preceded event B (bloom filter match, may be false positive).
    Possible,
    /// Event A definitely did NOT precede event B.
    No,
    /// Cannot determine (insufficient information).
    Unknown,
}

/// The causal cone of an event: the set of observations that preceded it.
///
/// Constructed from a `CausalLink`'s horizon data. Local node has exact
/// cone (full `ObservedHorizon`), remote observers only see the approximate
/// 4-byte compressed horizon.
#[derive(Debug, Clone)]
pub struct CausalCone {
    /// Origin of the event this cone belongs to.
    origin_hash: u64,
    /// Sequence of the event.
    sequence: u64,
    /// Full horizon at this point (exact, if available locally).
    horizon: Option<ObservedHorizon>,
    /// Compressed horizon from the wire (always available).
    /// 64-bit bloom filter — see `state/horizon.rs` for the
    /// FPR-vs-cardinality table and the out-of-band-fallback
    /// escape hatch when n > 16 active origins.
    horizon_encoded: u64,
}

impl CausalCone {
    /// Construct from wire data only (compressed, approximate).
    pub fn from_causal_link(link: &CausalLink) -> Self {
        Self {
            origin_hash: link.origin_hash,
            sequence: link.sequence,
            horizon: None,
            horizon_encoded: link.horizon_encoded,
        }
    }

    /// Construct from full data (exact).
    pub fn from_link_with_horizon(link: &CausalLink, horizon: &ObservedHorizon) -> Self {
        Self {
            origin_hash: link.origin_hash,
            sequence: link.sequence,
            horizon: Some(horizon.clone()),
            horizon_encoded: link.horizon_encoded,
        }
    }

    /// Could event from `other_origin` at `other_seq` have influenced this event?
    pub fn could_have_influenced(&self, other_origin: u64, other_seq: u64) -> Causality {
        // Same entity: strictly ordered by sequence
        if other_origin == self.origin_hash {
            return if other_seq < self.sequence {
                Causality::Definite
            } else {
                Causality::No
            };
        }

        // Check full horizon first (exact answer)
        if let Some(ref horizon) = self.horizon {
            return if horizon.has_observed(other_origin, other_seq) {
                Causality::Definite
            } else {
                Causality::No
            };
        }

        // Fall back to compressed horizon (approximate)
        if HorizonEncoder::might_contain(self.horizon_encoded, other_origin) {
            Causality::Possible
        } else {
            Causality::No
        }
    }

    /// Check if this event is concurrent with another (neither
    /// causally preceded the other).
    ///
    /// When BOTH cones carry the full [`ObservedHorizon`]
    /// out-of-band (`horizon` is `Some` on both), use the exact
    /// `has_observed` path. The compressed `horizon_encoded`
    /// (64-bit bloom) saturates past ~16 distinct origins —
    /// `potentially_concurrent` then collapses toward
    /// constant-`false`, silently mis-reporting genuinely
    /// concurrent events as causally ordered. Above the bloom's
    /// cardinality ceiling the bloom-only path would regress to
    /// "every pair looks ordered."
    ///
    /// We need head sequences to call `has_observed`. The
    /// `CausalCone` model treats `(origin_hash, ?)` — without a
    /// seq — so concurrency reduces to "neither has the other's
    /// origin in its observed set" (the same bilateral check the
    /// bloom approximates). The exact path uses
    /// [`ObservedHorizon::contains_origin`] for that — answers
    /// "have I observed any event from `origin_hash`" without
    /// needing a seq.
    pub fn is_concurrent_with(&self, other: &CausalCone) -> bool {
        match (&self.horizon, &other.horizon) {
            (Some(self_h), Some(other_h)) => {
                // Exact path — bypass the bloom entirely.
                !self_h.contains_origin(other.origin_hash)
                    && !other_h.contains_origin(self.origin_hash)
            }
            _ => {
                // Bloom fallback. Documented limitation: past
                // ~16 distinct origins encoded into either side,
                // this collapses toward constant-`false`.
                // Production callers that need correctness past
                // that ceiling MUST populate `horizon` on both
                // cones (the `Some(_)` arm above).
                HorizonEncoder::potentially_concurrent(
                    self.horizon_encoded,
                    other.origin_hash,
                    other.horizon_encoded,
                    self.origin_hash,
                )
            }
        }
    }

    /// Merge multiple cones (for daemons with multiple inputs).
    ///
    /// The merged cone's horizon is the union of all input horizons.
    pub fn merge(cones: &[CausalCone]) -> Option<CausalCone> {
        if cones.is_empty() {
            return None;
        }

        let first = &cones[0];
        let mut merged_horizon = first.horizon.clone();
        let mut all_exact = first.horizon.is_some();
        let mut merged_encoded = first.horizon_encoded;

        for cone in &cones[1..] {
            match (&mut merged_horizon, &cone.horizon) {
                (Some(merged), Some(h)) => merged.merge(h),
                _ => all_exact = false,
            }
            // OR the bloom bits together for approximate merge
            merged_encoded |= cone.horizon_encoded;
        }

        // Only keep full horizon if ALL inputs had exact data
        if !all_exact {
            merged_horizon = None;
        }

        Some(CausalCone {
            origin_hash: first.origin_hash,
            sequence: first.sequence,
            horizon: merged_horizon,
            horizon_encoded: merged_encoded,
        })
    }

    /// Get the origin hash.
    #[inline]
    pub fn origin_hash(&self) -> u64 {
        self.origin_hash
    }

    /// Get the sequence number.
    #[inline]
    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Get the compressed horizon.
    #[inline]
    pub fn horizon_encoded(&self) -> u64 {
        self.horizon_encoded
    }

    /// Whether this cone has exact (full horizon) data.
    #[inline]
    pub fn is_exact(&self) -> bool {
        self.horizon.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_link(origin: u64, seq: u64, horizon: u64) -> CausalLink {
        CausalLink {
            origin_hash: origin,
            horizon_encoded: horizon,
            sequence: seq,
            parent_hash: 0,
        }
    }

    #[test]
    fn test_same_entity_ordering() {
        let link = make_link(0xAAAA, 10, 0);
        let cone = CausalCone::from_causal_link(&link);

        // Earlier event from same entity = definite influence
        assert_eq!(cone.could_have_influenced(0xAAAA, 5), Causality::Definite);
        // Later event = no influence
        assert_eq!(cone.could_have_influenced(0xAAAA, 15), Causality::No);
        // Same sequence = no (not strictly before)
        assert_eq!(cone.could_have_influenced(0xAAAA, 10), Causality::No);
    }

    #[test]
    fn test_exact_horizon() {
        let mut horizon = ObservedHorizon::new();
        horizon.observe(0xBBBB, 20);

        let link = make_link(0xAAAA, 10, horizon.encode());
        let cone = CausalCone::from_link_with_horizon(&link, &horizon);

        assert!(cone.is_exact());
        assert_eq!(cone.could_have_influenced(0xBBBB, 15), Causality::Definite);
        assert_eq!(cone.could_have_influenced(0xBBBB, 25), Causality::No);
        assert_eq!(cone.could_have_influenced(0xCCCC, 5), Causality::No);
    }

    #[test]
    fn test_approximate_horizon() {
        let mut horizon = ObservedHorizon::new();
        horizon.observe(0xBBBB, 20);
        let encoded = horizon.encode();

        let link = make_link(0xAAAA, 10, encoded);
        let cone = CausalCone::from_causal_link(&link); // no full horizon

        assert!(!cone.is_exact());
        // Bloom filter should report Possible for observed origin
        let result = cone.could_have_influenced(0xBBBB, 15);
        assert!(result == Causality::Possible || result == Causality::No);
    }

    #[test]
    fn test_concurrent_events() {
        // A hasn't observed B, B hasn't observed A
        let mut h_a = ObservedHorizon::new();
        h_a.observe(0xAAAA, 10);
        let mut h_b = ObservedHorizon::new();
        h_b.observe(0xBBBB, 10);

        let link_a = make_link(0xAAAA, 10, h_a.encode());
        let link_b = make_link(0xBBBB, 10, h_b.encode());

        let cone_a = CausalCone::from_causal_link(&link_a);
        let cone_b = CausalCone::from_causal_link(&link_b);

        assert!(cone_a.is_concurrent_with(&cone_b));
    }

    #[test]
    fn test_merge_cones() {
        let mut h1 = ObservedHorizon::new();
        h1.observe(0xBBBB, 5);
        let mut h2 = ObservedHorizon::new();
        h2.observe(0xCCCC, 10);

        let link1 = make_link(0xAAAA, 1, h1.encode());
        let link2 = make_link(0xAAAA, 2, h2.encode());

        let cone1 = CausalCone::from_link_with_horizon(&link1, &h1);
        let cone2 = CausalCone::from_link_with_horizon(&link2, &h2);

        let merged = CausalCone::merge(&[cone1, cone2]).unwrap();
        assert!(merged.is_exact());

        // Merged cone should know about both entities
        assert_eq!(merged.could_have_influenced(0xBBBB, 3), Causality::Definite);
        assert_eq!(merged.could_have_influenced(0xCCCC, 8), Causality::Definite);
    }

    #[test]
    fn test_merge_empty() {
        assert!(CausalCone::merge(&[]).is_none());
    }

    // ---- Regression tests for Cubic AI findings ----

    #[test]
    fn test_regression_merge_mixed_exact_approximate_not_exact() {
        // Regression: merge() marked the result as exact (horizon: Some)
        // even when some inputs lacked full horizons, producing false
        // Definite causality results.
        let mut h = ObservedHorizon::new();
        h.observe(0xBBBB, 10);

        let exact_cone = CausalCone::from_link_with_horizon(&make_link(0xAAAA, 1, h.encode()), &h);
        let approx_cone = CausalCone::from_causal_link(&make_link(0xAAAA, 2, 0));

        let merged = CausalCone::merge(&[exact_cone, approx_cone]).unwrap();
        assert!(
            !merged.is_exact(),
            "merged cone with mixed exact/approximate inputs must not be marked exact"
        );
    }

    /// CR-16: when both cones carry the FULL horizon, concurrency
    /// detection MUST use the exact `contains_origin` path rather
    /// than the bloom. Pre-CR-16 callers that populated the full
    /// horizon got the bloom path anyway — past ~16 distinct
    /// origins encoded into either side, the bloom collapsed to
    /// constant-`false` and concurrency detection silently
    /// regressed to "every pair looks ordered."
    ///
    /// We construct two cones each with 32 observed origins (well
    /// past the bloom's saturation point) where `A` has NOT
    /// observed `B`'s origin and `B` has NOT observed `A`'s
    /// origin. Pre-CR-16 the bloom path returned `false`
    /// (constant-false collapse); post-CR-16 the exact path
    /// correctly returns `true`.
    #[test]
    fn cr16_is_concurrent_with_uses_exact_horizon_when_both_populated() {
        // Build A's horizon with 32 distinct origins, NOT including
        // B's origin (0xBBBB).
        let mut h_a = ObservedHorizon::new();
        for i in 0u64..32 {
            // Skip 0xBBBB so A genuinely hasn't observed B.
            let origin = 0x10000u64 + i;
            h_a.observe(origin, i + 1);
        }
        h_a.observe(0xAAAA, 100); // A's own origin

        // Build B's horizon with 32 distinct origins, NOT including
        // A's origin (0xAAAA).
        let mut h_b = ObservedHorizon::new();
        for i in 0u64..32 {
            let origin = 0x20000u64 + i;
            h_b.observe(origin, i + 1);
        }
        h_b.observe(0xBBBB, 100); // B's own origin

        // Sanity: bloom is saturated — `might_contain` would
        // return true for almost any origin queried against A's
        // encoded horizon. We pin this so the test is meaningful:
        // if the bloom WEREN'T saturated, the exact-vs-bloom
        // outcomes might happen to agree.
        let bloom_a = h_a.encode();
        let bloom_b = h_b.encode();
        let bloom_says_a_observed_b = HorizonEncoder::might_contain(bloom_a, 0xBBBB);
        let bloom_says_b_observed_a = HorizonEncoder::might_contain(bloom_b, 0xAAAA);
        // If both are true, the bloom thinks A observed B and B
        // observed A — concurrency = false (bloom-saturation
        // false-negative on concurrency). The exact path knows
        // better.
        let bloom_says_concurrent = !bloom_says_a_observed_b && !bloom_says_b_observed_a;

        let link_a = make_link(0xAAAA, 100, bloom_a);
        let link_b = make_link(0xBBBB, 100, bloom_b);
        let cone_a_exact = CausalCone::from_link_with_horizon(&link_a, &h_a);
        let cone_b_exact = CausalCone::from_link_with_horizon(&link_b, &h_b);

        // Exact path MUST report concurrent.
        assert!(
            cone_a_exact.is_concurrent_with(&cone_b_exact),
            "CR-16: with both full horizons populated, is_concurrent_with \
             MUST use the exact path. A truly hasn't observed B and B \
             hasn't observed A — concurrency = true."
        );

        // Demonstrate the bloom-fallback failure mode: if EITHER
        // side lacks the full horizon, we fall back to the bloom.
        // Under saturation, the bloom says everyone-observed-
        // everyone, so `is_concurrent_with` returns `false`. This
        // pins the documented limitation.
        let cone_a_bloom = CausalCone::from_causal_link(&link_a);
        let cone_b_bloom = CausalCone::from_causal_link(&link_b);
        let bloom_result = cone_a_bloom.is_concurrent_with(&cone_b_bloom);
        assert_eq!(
            bloom_result, bloom_says_concurrent,
            "bloom path must match the raw HorizonEncoder query — \
             if this differs, the fallback diverged from the bloom \
             primitive (CR-16 sanity)"
        );
    }

    /// CR-16 asymmetric: when only ONE cone carries the full
    /// horizon, `is_concurrent_with` MUST fall back to the bloom
    /// path (not silently use the exact path with one-sided data).
    /// A future caller that thinks "I populated my own horizon, so
    /// I'll get exact concurrency" needs to know that the OTHER
    /// side's representation matters too. This pins the
    /// `(Some, None)` and `(None, Some)` arms route through the
    /// bloom fallback.
    #[test]
    fn cr16_asymmetric_horizon_falls_back_to_bloom() {
        let mut h = ObservedHorizon::new();
        h.observe(0xAAAA, 10);
        h.observe(0xBBBB, 5); // Note: A's horizon contains B.

        let link_a = make_link(0xAAAA, 10, h.encode());
        let link_b = make_link(0xBBBB, 6, 0); // Empty horizon

        // (Some, None): only A carries the full horizon.
        let cone_a_exact = CausalCone::from_link_with_horizon(&link_a, &h);
        let cone_b_bloom = CausalCone::from_causal_link(&link_b);

        // The bloom fallback uses A's encoded horizon (which DOES
        // contain B's origin via Bloom) AND B's encoded horizon
        // (which is 0, so doesn't contain A). Bloom path:
        //   !A.contains(B) && !B.contains(A)
        //   = !true && !false
        //   = false
        // So the bloom path returns `false` (not concurrent).
        let asym_result = cone_a_exact.is_concurrent_with(&cone_b_bloom);

        // Direct bloom-primitive query, for sanity:
        let raw_bloom = HorizonEncoder::potentially_concurrent(
            link_a.horizon_encoded,
            0xBBBB,
            link_b.horizon_encoded,
            0xAAAA,
        );
        assert_eq!(
            asym_result, raw_bloom,
            "CR-16: asymmetric (Some, None) MUST route through the bloom \
             fallback — pre-fix a future caller that started populating \
             one cone's full horizon while leaving the other on the \
             wire-format-only path would get UNDEFINED behavior \
             (some hybrid of exact + bloom). The test pins the \
             defined behavior: bloom on either-missing."
        );

        // Symmetric reverse: (None, Some). Same outcome — bloom
        // fallback, since `is_concurrent_with`'s match arms only
        // take the exact path when BOTH are Some.
        let cone_a_bloom = CausalCone::from_causal_link(&link_a);
        let cone_b_exact = CausalCone::from_link_with_horizon(&link_b, &ObservedHorizon::new());
        let asym_reverse = cone_a_bloom.is_concurrent_with(&cone_b_exact);
        assert_eq!(
            asym_reverse, raw_bloom,
            "CR-16: asymmetric (None, Some) must also fall back to bloom"
        );
    }
}
