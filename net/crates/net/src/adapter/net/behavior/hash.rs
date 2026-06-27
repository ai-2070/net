//! Shared FNV-1a building block.
//!
//! Several `behavior` modules fold values into a `u64` with the same FNV-1a
//! constants — load-balancer ring keys ([`loadbalance`](super::loadbalance)),
//! proximity capability hashes ([`proximity`](super::proximity)), and the mesh
//! scheduler's dirty fingerprint ([`meshos::scheduler`](super::meshos)). They
//! used to each open-code the constants (spelled inconsistently:
//! `0x100000001b3` vs `0x0000_0100_0000_01b3`), so a change had to be found in
//! three places. This is the single definition.
//!
//! The output is intentionally identical to the prior open-coded loops: these
//! hashes are persisted / cached state (e.g. proximity neighbor metadata), so
//! the byte shape must not drift.

/// FNV-1a 64-bit offset basis.
pub(crate) const FNV1A_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64-bit prime.
pub(crate) const FNV1A_PRIME: u64 = 0x0000_0100_0000_01b3;

/// One FNV-1a step: mix `value` into the running `acc` (`acc ^ value`, then
/// multiply by the prime). Fold a sequence by threading `acc` through repeated
/// calls starting from [`FNV1A_OFFSET`]; the result depends on the order the
/// values are fed, so callers that need determinism must feed them in a stable
/// order (e.g. iterating a `BTreeSet`).
#[inline]
pub(crate) fn fnv1a_step(acc: u64, value: u64) -> u64 {
    (acc ^ value).wrapping_mul(FNV1A_PRIME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_matches_open_coded_fnv1a() {
        // The reference loop the helper replaces.
        let reference = |bytes: &[u64]| {
            let mut h: u64 = 0xcbf2_9ce4_8422_2325;
            for &b in bytes {
                h ^= b;
                h = h.wrapping_mul(0x100000001b3);
            }
            h
        };
        for seq in [&[1u64, 2, 3][..], &[][..], &[0, 255, 4096], &[u64::MAX, 0]] {
            let folded = seq.iter().fold(FNV1A_OFFSET, |acc, &v| fnv1a_step(acc, v));
            assert_eq!(
                folded,
                reference(seq),
                "helper must match the open-coded fold"
            );
        }
    }

    #[test]
    fn step_is_order_sensitive() {
        let a = fnv1a_step(fnv1a_step(FNV1A_OFFSET, 1), 2);
        let b = fnv1a_step(fnv1a_step(FNV1A_OFFSET, 2), 1);
        assert_ne!(a, b, "FNV-1a fold depends on input order");
    }
}
