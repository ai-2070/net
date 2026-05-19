//! Stripe-membership index for the v0.3 Phase C6 GC pin.
//!
//! Reed-Solomon-encoded stripes have an inter-chunk lifecycle
//! dependency the v0.2 chunk-refcount model doesn't capture: a
//! parity chunk's presence becomes load-bearing the moment any
//! data chunk of its stripe goes missing. Without coordination,
//! the GC sweep — which only consults the per-chunk refcount +
//! retention floor — can correctly determine that a parity
//! chunk has refcount=0 and unrelated retention has elapsed,
//! and then sweep it… right when that parity chunk was the
//! only thing keeping a degraded stripe recoverable.
//!
//! v0.3 Phase C6 closes the gap with a per-adapter in-memory
//! index: for every RS stripe written, record `(stripe_chunks,
//! k)`. The GC sweep consults the index before deleting any
//! chunk:
//!
//! - If the chunk isn't a member of any registered stripe →
//!   sweep proceeds as in v0.2.
//! - If the chunk is in a stripe and the stripe currently has
//!   `>= k` chunks locally present → sweep proceeds (the stripe
//!   isn't degraded; redundancy is intact).
//! - If the chunk is in a stripe and the stripe has `< k`
//!   chunks present → pin the chunk against GC. The pin lifts
//!   automatically the next sweep tick after a `repair_blob`
//!   restores the stripe to `>= k` chunks.
//!
//! # Scope
//!
//! The index is **per-adapter and in-memory**. It tracks stripes
//! whose store path traversed this adapter; cross-process or
//! cross-restart stripe state is NOT preserved. For v0.3 ship
//! this is the pragmatic minimum — the operator-driven
//! [`MeshBlobAdapter::repair_blob`](super::mesh::MeshBlobAdapter::repair_blob)
//! sweep is the durable recovery mechanism; the GC pin protects
//! against the in-process race window where a degraded-stripe
//! parity chunk's refcount briefly drops to zero between blob
//! dereference and repair.
//!
//! A future commit could persist the index to disk (e.g. an
//! adapter-level `<dir>/stripe_index.bin`) and rebuild on
//! startup by walking known blob roots. That's substantial and
//! out of scope for the initial C6 ship.

use std::collections::HashMap;

/// A single registered stripe — `(all_member_hashes, k)`.
/// `k` is the data-shard count; the stripe is degraded iff fewer
/// than `k` members are locally present.
///
/// `members` includes both data and parity chunks in the order
/// the producer stored them; the index doesn't distinguish at
/// pin-decision time (any missing member counts toward the
/// degradation threshold).
#[derive(Clone, Debug)]
pub struct StripeRecord {
    /// Every member-chunk hash (data + parity), in store order.
    /// Used by the pin check to count locally-present members.
    pub members: Vec<[u8; 32]>,
    /// Minimum data shards needed to reconstruct the stripe.
    /// `< k` present chunks → degraded → pin every member.
    pub k: u8,
}

/// In-memory stripe-membership index. Operators wrap in a
/// `parking_lot::Mutex` for shared access; per-call critical
/// sections are short (one HashMap probe + one Vec append on
/// register, one HashMap probe + iteration on pin check).
#[derive(Default, Debug)]
pub struct StripeMembershipIndex {
    /// Maps a chunk hash to every stripe it's a member of. Most
    /// chunks belong to exactly one stripe; cross-blob dedup of
    /// identical data chunks may produce multi-stripe membership.
    by_chunk: HashMap<[u8; 32], Vec<StripeRecord>>,
    /// Running count of registered stripes — operator metric +
    /// test assertion handle.
    registered_count: u64,
}

impl StripeMembershipIndex {
    /// Construct an empty index. Stripes register via
    /// [`Self::register_stripe`]; the [`StripeMembershipIndex`]
    /// is internally `HashMap` + counter, no I/O on construction.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a stripe. Every chunk hash in `members` gets a
    /// reference back to the supplied record. Idempotent at the
    /// (members, k) tuple level: registering the same stripe
    /// twice doesn't double-count the pin protection (the
    /// deduplication runs at chunk-lookup time — the pin check
    /// only needs to know "is the chunk in any degraded stripe"
    /// regardless of how many copies of the same StripeRecord
    /// land in the by_chunk vec).
    pub fn register_stripe(&mut self, members: Vec<[u8; 32]>, k: u8) {
        if members.is_empty() || k == 0 {
            return; // degenerate; nothing to pin.
        }
        let record = StripeRecord { members: members.clone(), k };
        for hash in &members {
            self.by_chunk.entry(*hash).or_default().push(record.clone());
        }
        self.registered_count = self.registered_count.saturating_add(1);
    }

    /// Return `true` iff `hash` belongs to any registered stripe
    /// whose locally-present member count is `< k`. `is_present`
    /// is the caller's "do I have this chunk locally" predicate —
    /// typically wraps a check against the chunk-file existence
    /// (`MeshBlobAdapter::chunk_exists`) or a refcount-table
    /// lookup.
    ///
    /// If `hash` is not registered (not a member of any stripe
    /// the adapter knows about), returns `false` — the GC sweep
    /// falls back to its v0.2 refcount-only logic.
    pub fn should_pin_against_gc<F>(&self, hash: &[u8; 32], mut is_present: F) -> bool
    where
        F: FnMut(&[u8; 32]) -> bool,
    {
        let Some(stripes) = self.by_chunk.get(hash) else {
            return false;
        };
        for stripe in stripes {
            let present_count = stripe.members.iter().filter(|h| is_present(h)).count();
            if present_count < stripe.k as usize {
                // Degraded stripe — pin every member, including
                // `hash`. Don't keep scanning the other stripes;
                // one degraded stripe is sufficient grounds.
                return true;
            }
        }
        false
    }

    /// Count of stripes registered over the lifetime of this
    /// index. Operator metric / test assertion handle.
    pub fn registered_count(&self) -> u64 {
        self.registered_count
    }

    /// Count of distinct chunk hashes tracked across all
    /// registered stripes. Test assertion handle.
    pub fn tracked_chunk_count(&self) -> usize {
        self.by_chunk.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(byte: u8) -> [u8; 32] {
        let mut a = [0u8; 32];
        a[0] = byte;
        a
    }

    /// Unregistered chunk → never pinned.
    #[test]
    fn no_pin_for_unregistered_chunk() {
        let idx = StripeMembershipIndex::new();
        assert!(!idx.should_pin_against_gc(&h(1), |_| true));
    }

    /// Healthy stripe (all members present) → no pin.
    #[test]
    fn no_pin_for_healthy_stripe() {
        let mut idx = StripeMembershipIndex::new();
        let members = vec![h(1), h(2), h(3), h(4)]; // k=2 → 4 members = 2 data + 2 parity
        idx.register_stripe(members.clone(), 2);
        // Every member is present → not degraded → no pin.
        for hash in &members {
            assert!(!idx.should_pin_against_gc(hash, |_| true));
        }
    }

    /// Degraded stripe (< k members present) → pin every member,
    /// including the missing ones (irrelevant since they're
    /// missing) and the present ones (the load-bearing case).
    #[test]
    fn pin_every_member_of_degraded_stripe() {
        let mut idx = StripeMembershipIndex::new();
        let members = vec![h(1), h(2), h(3), h(4)]; // k=2
        idx.register_stripe(members.clone(), 2);
        // Only one member present: 1 < k=2 → degraded.
        let presence = |hash: &[u8; 32]| *hash == h(1);
        for hash in &members {
            assert!(
                idx.should_pin_against_gc(hash, presence),
                "every member of a degraded stripe must pin"
            );
        }
    }

    /// Exactly `k` members present → at the recovery threshold,
    /// not pinned (any further loss would degrade but the
    /// invariant is "< k" not "<= k").
    #[test]
    fn no_pin_when_exactly_k_present() {
        let mut idx = StripeMembershipIndex::new();
        let members = vec![h(1), h(2), h(3), h(4)]; // k=2
        idx.register_stripe(members.clone(), 2);
        // 2 present, 2 missing → at threshold, no pin.
        let presence = |hash: &[u8; 32]| matches!(hash[0], 1 | 2);
        for hash in &members {
            assert!(!idx.should_pin_against_gc(hash, presence));
        }
    }

    /// Chunk in multiple stripes → pinned if ANY stripe is
    /// degraded.
    #[test]
    fn pin_if_any_stripe_member_is_degraded() {
        let mut idx = StripeMembershipIndex::new();
        // Chunk 0xAA is a member of two stripes.
        idx.register_stripe(vec![h(0xAA), h(2), h(3), h(4)], 2);
        idx.register_stripe(vec![h(0xAA), h(5), h(6), h(7)], 2);
        // Stripe 1 is healthy (all 4 present), stripe 2 is
        // degraded (only 0xAA present).
        let presence = |hash: &[u8; 32]| {
            matches!(hash[0], 0xAA | 2 | 3 | 4)
        };
        assert!(
            idx.should_pin_against_gc(&h(0xAA), presence),
            "chunk in any degraded stripe must pin"
        );
    }

    /// `registered_count` increments per call; degenerate inputs
    /// (empty members or k=0) are no-ops and don't bump counter.
    #[test]
    fn register_count_and_degenerate_no_ops() {
        let mut idx = StripeMembershipIndex::new();
        idx.register_stripe(vec![h(1), h(2)], 1);
        assert_eq!(idx.registered_count(), 1);
        idx.register_stripe(vec![], 1);
        idx.register_stripe(vec![h(1)], 0);
        assert_eq!(idx.registered_count(), 1, "degenerate registers are no-ops");
    }
}
